//! Pure reminder interval state machine.
//!
//! The machine owns no clock, filesystem, environment, or globals. Callers feed
//! it [`Event`] values carrying observed timestamps and persist the returned
//! [`SessionState`] between processes.
//!
//! Each configured rule owns an independent interval. A tool pre-hook resets an
//! interval when the tool matches that rule's reset patterns, or counts the
//! invocation once when it matches a `toolCalls` condition. Counting happens
//! before the tool runs, so failed calls count too. The post-tool event
//! evaluates every interval and emits matched messages in configuration order;
//! an emitted rule resets its own interval.
//!
//! `preCompact` is latched per interval and never emits directly. The next
//! post-tool event evaluates the latch, including inside `all`/`any`
//! compositions. Reset tools and emitted reminders both clear the latch.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{Condition, ObservedEvent, Policy};
use crate::time::TimestampMillis;

/// Version of the serialized session-state schema.
const SCHEMA_VERSION: u32 = 1;

/// Persisted state for one session and one selected agent policy.
///
/// Fields are private and the schema marker is explicit. State loaded with a
/// different schema version, policy fingerprint, agent, or interval count is
/// rejected by [`BoundSession::restore`] and replaced with a fresh state at the
/// current event timestamp by the load boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SessionState {
    schema_version: u32,
    agent: String,
    policy_fingerprint: String,
    intervals: Vec<IntervalState>,
}

impl SessionState {
    /// Creates a fresh state with one interval per policy rule at `now`.
    fn new(policy: &Policy, now: TimestampMillis) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            agent: policy.agent().to_owned(),
            policy_fingerprint: policy.fingerprint().to_owned(),
            intervals: policy
                .entries()
                .iter()
                .map(|_| IntervalState::fresh(now))
                .collect(),
        }
    }
}

/// A persisted session state that has been checked against a policy.
///
/// The private fields prove that the state's schema version, agent, policy
/// fingerprint, interval count, and invocation counters are valid for the
/// borrowed policy. Rule iteration therefore pairs policy entries with
/// intervals without revalidating on every event, and no caller can evolve a
/// state against a different policy.
pub(crate) struct BoundSession<'policy> {
    policy: &'policy Policy,
    state: SessionState,
}

impl<'policy> BoundSession<'policy> {
    /// Creates a fresh state whose intervals all start at `now`.
    ///
    /// The initial state is valid by construction, so it is not revalidated.
    pub(crate) fn fresh(policy: &'policy Policy, now: TimestampMillis) -> Self {
        Self {
            policy,
            state: SessionState::new(policy, now),
        }
    }

    /// Binds persisted state to `policy`, validating it exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`StateError`] for an unsupported schema version, an impossible
    /// zero tool count, a different agent or policy fingerprint, or a
    /// different number of rule intervals.
    pub(crate) fn restore(
        policy: &'policy Policy,
        state: SessionState,
    ) -> Result<Self, StateError> {
        validate(&state, policy)?;
        Ok(Self { policy, state })
    }

    /// Returns the persisted DTO without applying an event.
    pub(crate) fn into_state(self) -> SessionState {
        self.state
    }

    fn apply_tool_before(&mut self, tool: &str, now: TimestampMillis) {
        for (entry, interval) in self
            .policy
            .entries()
            .iter()
            .zip(self.state.intervals.iter_mut())
        {
            if entry
                .reset_tools()
                .iter()
                .any(|pattern| pattern.matches(tool))
            {
                interval.reset(now);
            } else if condition_counts_tool(entry.condition(), tool) {
                let count = interval.counts.entry(tool.to_owned()).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
    }

    fn evaluate(&mut self, now: TimestampMillis) -> Vec<String> {
        let mut messages = Vec::new();

        for (entry, interval) in self
            .policy
            .entries()
            .iter()
            .zip(self.state.intervals.iter_mut())
        {
            if condition_matches(entry.condition(), interval, now) {
                messages.push(entry.message().to_owned());
                interval.reset(now);
            }
        }

        messages
    }

    fn observe(&mut self, event: ObservedEvent) {
        for interval in &mut self.state.intervals {
            interval.observed.insert(event);
        }
    }
}

/// Validates a persisted DTO against the policy it will be bound to.
fn validate(state: &SessionState, policy: &Policy) -> Result<(), StateError> {
    if state.schema_version != SCHEMA_VERSION {
        return Err(StateError::UnsupportedVersion {
            found: state.schema_version,
            expected: SCHEMA_VERSION,
        });
    }

    for interval in &state.intervals {
        if let Some((tool, _)) = interval.counts.iter().find(|(_, count)| **count == 0) {
            return Err(StateError::ZeroToolCount { tool: tool.clone() });
        }
    }

    if state.intervals.len() != policy.entries().len() {
        return Err(StateError::IntervalCountMismatch {
            actual: state.intervals.len(),
            expected: policy.entries().len(),
        });
    }
    if state.agent != policy.agent() {
        return Err(StateError::AgentMismatch);
    }
    if state.policy_fingerprint != policy.fingerprint() {
        return Err(StateError::PolicyMismatch);
    }

    Ok(())
}

/// A state validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum StateError {
    /// The persisted state was written by an unsupported schema version.
    #[error("unsupported session state version {found}; expected version {expected}")]
    UnsupportedVersion {
        /// Version found in the state.
        found: u32,
        /// Version understood by this binary.
        expected: u32,
    },

    /// A persisted invocation counter is zero, which the machine never writes.
    #[error("session state contains a zero invocation count for tool \"{tool}\"")]
    ZeroToolCount {
        /// Tool with the impossible count.
        tool: String,
    },

    /// The state has a different number of intervals than the policy has rules.
    #[error(
        "session state has {actual} reminder intervals, but the selected policy has {expected}"
    )]
    IntervalCountMismatch {
        /// Number of intervals found in the state.
        actual: usize,
        /// Number of rules in the selected policy.
        expected: usize,
    },

    /// The state was created for a different agent.
    #[error("session state belongs to a different agent")]
    AgentMismatch,

    /// The state was created from other rules for the same agent.
    #[error("session state belongs to a different policy fingerprint")]
    PolicyMismatch,
}

/// One independent rule interval inside [`SessionState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IntervalState {
    reset_at_ms: TimestampMillis,
    counts: BTreeMap<String, u64>,
    observed: BTreeSet<ObservedEvent>,
}

impl IntervalState {
    fn fresh(reset_at_ms: TimestampMillis) -> Self {
        Self {
            reset_at_ms,
            counts: BTreeMap::new(),
            observed: BTreeSet::new(),
        }
    }

    fn reset(&mut self, now: TimestampMillis) {
        self.reset_at_ms = now;
        self.counts.clear();
        self.observed.clear();
    }
}

/// A command-hook observation fed to [`evolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// A tool is about to run. Failed invocations also emit this event.
    ToolBefore {
        /// Observed tool name.
        tool: String,
        /// Event timestamp.
        now_ms: TimestampMillis,
    },

    /// A tool finished successfully or failed. Both outcomes use this event.
    ToolAfter {
        /// Event timestamp.
        now_ms: TimestampMillis,
    },

    /// A session event was observed without evaluating conditions.
    Observed {
        /// Event to latch in every interval.
        event: ObservedEvent,
        /// Event timestamp.
        now_ms: TimestampMillis,
    },
}

impl Event {
    fn now_ms(&self) -> TimestampMillis {
        match self {
            Self::ToolBefore { now_ms, .. }
            | Self::ToolAfter { now_ms }
            | Self::Observed { now_ms, .. } => *now_ms,
        }
    }
}

/// A non-empty sequence of reminder messages.
///
/// The first message is stored separately so the type itself guarantees that
/// every value carries at least one message. Remaining messages preserve
/// configuration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReminderMessages {
    first: String,
    rest: Vec<String>,
}

impl ReminderMessages {
    /// Builds a non-empty message sequence, or `None` when `messages` is empty.
    pub(crate) fn from_vec(messages: Vec<String>) -> Option<Self> {
        let mut messages = messages.into_iter();
        let first = messages.next()?;

        Some(Self {
            first,
            rest: messages.collect(),
        })
    }

    /// Iterates over the messages in emission order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.first.as_str()).chain(self.rest.iter().map(String::as_str))
    }

    /// Joins the messages in emission order with `separator`.
    pub(crate) fn join(&self, separator: &str) -> String {
        let mut joined = self.first.clone();

        for message in self.iter().skip(1) {
            joined.push_str(separator);
            joined.push_str(message);
        }

        joined
    }
}

/// The result of applying one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Output {
    /// No reminder is due; continue without publishing anything.
    Continue,

    /// Publish these reminder messages in order.
    Emit {
        /// Non-empty messages from matched rules, in configuration order.
        messages: ReminderMessages,
    },
}

/// Applies one event to a bound session and returns the successor state and output.
///
/// The input already carries its policy proof, so this function does not
/// revalidate the state or reconcile it against another policy. It is
/// deterministic and never reads a clock or performs I/O.
pub(crate) fn evolve(mut bound: BoundSession<'_>, event: Event) -> (SessionState, Output) {
    let now = event.now_ms();
    let output = match event {
        Event::ToolBefore { tool, .. } => {
            bound.apply_tool_before(&tool, now);
            Output::Continue
        }
        Event::ToolAfter { .. } => match ReminderMessages::from_vec(bound.evaluate(now)) {
            Some(messages) => Output::Emit { messages },
            None => Output::Continue,
        },
        Event::Observed { event, .. } => {
            bound.observe(event);
            Output::Continue
        }
    };

    (bound.into_state(), output)
}

fn condition_counts_tool(condition: &Condition, tool: &str) -> bool {
    match condition {
        Condition::ToolCalls(tool_calls) => tool_calls
            .tools()
            .iter()
            .any(|pattern| pattern.matches(tool)),
        Condition::All(composite) | Condition::Any(composite) => composite
            .conditions()
            .iter()
            .any(|child| condition_counts_tool(child, tool)),
        Condition::ElapsedTime(_) | Condition::Event(_) => false,
    }
}

fn condition_matches(
    condition: &Condition,
    interval: &IntervalState,
    now: TimestampMillis,
) -> bool {
    match condition {
        Condition::ToolCalls(tool_calls) => {
            let mut matching_calls = 0_u64;
            for (tool, count) in &interval.counts {
                if tool_calls
                    .tools()
                    .iter()
                    .any(|pattern| pattern.matches(tool))
                {
                    matching_calls = matching_calls.saturating_add(*count);
                }
            }
            matching_calls >= tool_calls.count().get()
        }
        Condition::ElapsedTime(elapsed) => {
            now.elapsed_seconds_since(interval.reset_at_ms) >= elapsed.seconds()
        }
        Condition::Event(event) => interval.observed.contains(&event.event()),
        Condition::All(composite) => composite
            .conditions()
            .iter()
            .all(|child| condition_matches(child, interval, now)),
        Condition::Any(composite) => composite
            .conditions()
            .iter()
            .any(|child| condition_matches(child, interval, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Configuration;

    const FIFTH_TOOL_CALL: &str = r#"{
        agents: {
            Orchestrator: [{
                message: "Remember your prompt obligations.",
                condition: { type: "toolCalls", count: 5 }
            }]
        }
    }"#;

    fn make_policy(document: &str) -> Policy {
        Configuration::parse(document)
            .expect("test configuration should be valid")
            .policy("Orchestrator")
    }

    fn timestamp(milliseconds: u64) -> TimestampMillis {
        TimestampMillis::from(milliseconds)
    }

    fn fresh_state(policy: &Policy, now: TimestampMillis) -> SessionState {
        BoundSession::fresh(policy, now).into_state()
    }

    fn bind(policy: &Policy, state: SessionState) -> BoundSession<'_> {
        BoundSession::restore(policy, state).expect("test state should match its policy")
    }

    fn tool_before(
        policy: &Policy,
        state: SessionState,
        tool: &str,
        now: TimestampMillis,
    ) -> (SessionState, Output) {
        evolve(
            bind(policy, state),
            Event::ToolBefore {
                tool: tool.to_owned(),
                now_ms: now,
            },
        )
    }

    fn tool_after(
        policy: &Policy,
        state: SessionState,
        now: TimestampMillis,
    ) -> (SessionState, Output) {
        evolve(bind(policy, state), Event::ToolAfter { now_ms: now })
    }

    fn observe(
        policy: &Policy,
        state: SessionState,
        event: ObservedEvent,
        now: TimestampMillis,
    ) -> (SessionState, Output) {
        evolve(bind(policy, state), Event::Observed { event, now_ms: now })
    }

    fn assert_continue(output: &Output) {
        assert_eq!(output, &Output::Continue);
    }

    fn assert_emits(output: &Output, message: &str) {
        match output {
            Output::Emit { messages } => {
                assert_eq!(messages.iter().collect::<Vec<_>>(), [message]);
                assert_eq!(messages.join("\n"), message);
            }
            Output::Continue => panic!("expected reminder {message:?}, got Continue"),
        }
    }

    #[test]
    fn reminder_messages_require_at_least_one_message() {
        assert_eq!(ReminderMessages::from_vec(Vec::new()), None);

        let single = ReminderMessages::from_vec(vec!["First reminder.".to_owned()])
            .expect("one message is nonempty");
        assert_eq!(single.iter().collect::<Vec<_>>(), ["First reminder."]);
        assert_eq!(single.join("\n\n"), "First reminder.");

        let multiple = ReminderMessages::from_vec(vec![
            "First reminder.".to_owned(),
            "Second reminder.".to_owned(),
        ])
        .expect("two messages are nonempty");
        assert_eq!(
            multiple.iter().collect::<Vec<_>>(),
            ["First reminder.", "Second reminder."]
        );
        assert_eq!(multiple.join("\n\n"), "First reminder.\n\nSecond reminder.");
    }

    #[test]
    fn every_matching_rule_emits_its_message_in_configuration_order() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [
                        {
                            message: "First reminder.",
                            condition: { type: "elapsedTime", seconds: 0.001 }
                        },
                        {
                            message: "Second reminder.",
                            condition: { type: "elapsedTime", seconds: 0.001 }
                        }
                    ]
                }
            }"#,
        );
        let state = fresh_state(&policy, timestamp(0));

        let (_, output) = tool_after(&policy, state, timestamp(10));
        match output {
            Output::Emit { messages } => {
                assert_eq!(
                    messages.iter().collect::<Vec<_>>(),
                    ["First reminder.", "Second reminder."]
                );
                assert_eq!(messages.join(" "), "First reminder. Second reminder.");
            }
            Output::Continue => panic!("expected two reminders, got Continue"),
        }
    }

    #[test]
    fn fifth_matching_call_emits_and_the_interval_repeats() {
        let policy = make_policy(FIFTH_TOOL_CALL);
        let mut state = fresh_state(&policy, timestamp(0));

        for call in 0..4 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;

            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let (next, output) = tool_before(&policy, state, "bash", timestamp(10));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(10));
        assert_emits(&output, "Remember your prompt obligations.");
        state = next;

        for call in 11..15 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;

            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let (next, output) = tool_before(&policy, state, "bash", timestamp(15));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(15));
        assert_emits(&output, "Remember your prompt obligations.");
    }

    #[test]
    fn failed_calls_count_at_before_and_are_evaluated_at_after() {
        let policy = make_policy(FIFTH_TOOL_CALL);
        let mut state = fresh_state(&policy, timestamp(0));

        for call in 0..4 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let (next, output) = tool_before(&policy, state, "bash", timestamp(10));
        assert_continue(&output);
        state = next;

        // A failed fifth call is still represented by ToolAfter; the machine
        // has no success flag and emits at this point.
        let (_, output) = tool_after(&policy, state, timestamp(10));
        assert_emits(&output, "Remember your prompt obligations.");
    }

    #[test]
    fn reset_tool_clears_counts_before_the_tool_completes() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Remember your prompt obligations.",
                        resetTools: ["task"],
                        condition: { type: "toolCalls", count: 2, tools: ["bash"] }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = tool_before(&policy, state, "bash", timestamp(1));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(1));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "task", timestamp(2));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(3));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(3));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(4));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(4));
        assert_emits(&output, "Remember your prompt obligations.");
    }

    #[test]
    fn rules_keep_independent_intervals() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [
                        {
                            message: "First reminder.",
                            condition: { type: "toolCalls", count: 2, tools: ["bash"] }
                        },
                        {
                            message: "Second reminder.",
                            condition: { type: "toolCalls", count: 2, tools: ["powershell"] }
                        }
                    ]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = tool_before(&policy, state, "bash", timestamp(1));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(1));
        assert_continue(&output);
        state = next;

        for call in 2..4 {
            let (next, output) = tool_before(&policy, state, "powershell", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&policy, state, timestamp(call));
            if call == 2 {
                assert_continue(&output);
            } else {
                assert_emits(&output, "Second reminder.");
            }
            state = next;
        }

        let (next, output) = tool_before(&policy, state, "bash", timestamp(5));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(5));
        assert_emits(&output, "First reminder.");
    }

    #[test]
    fn recursive_all_and_any_conditions_evaluate_together() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Composite reminder.",
                        condition: {
                            type: "all",
                            conditions: [
                                { type: "toolCalls", count: 2, tools: ["bash"] },
                                {
                                    type: "any",
                                    conditions: [
                                        { type: "elapsedTime", seconds: 0.2 },
                                        { type: "event", event: "preCompact" }
                                    ]
                                }
                            ]
                        }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = tool_before(&policy, state, "bash", timestamp(10));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_before(&policy, state, "bash", timestamp(20));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(20));
        assert_continue(&output);
        state = next;

        let (next, output) = observe(&policy, state, ObservedEvent::PreCompact, timestamp(30));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(40));
        assert_emits(&output, "Composite reminder.");
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(50));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_before(&policy, state, "bash", timestamp(60));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(60));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(240));
        assert_emits(&output, "Composite reminder.");
    }

    #[test]
    fn elapsed_time_accepts_fractional_seconds_and_resets_on_emit() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Time reminder.",
                        resetTools: [],
                        condition: { type: "elapsedTime", seconds: 0.5 }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(1000));

        let (next, output) = tool_after(&policy, state, timestamp(1499));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_after(&policy, state, timestamp(1500));
        assert_emits(&output, "Time reminder.");
        state = next;

        let (next, output) = tool_after(&policy, state, timestamp(1999));
        assert_continue(&output);
        state = next;

        let (_, output) = tool_after(&policy, state, timestamp(2000));
        assert_emits(&output, "Time reminder.");
    }

    #[test]
    fn overlapping_tool_patterns_count_one_invocation_once() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Wildcard reminder.",
                        condition: {
                            type: "toolCalls",
                            count: 2,
                            tools: ["bash*", "bash"]
                        }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = tool_before(&policy, state, "bash", timestamp(1));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(1));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(2));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(2));
        assert_emits(&output, "Wildcard reminder.");
    }

    #[test]
    fn pre_compact_is_latched_and_evaluated_by_later_post_tool_events() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Compaction reminder.",
                        condition: {
                            type: "all",
                            conditions: [
                                { type: "event", event: "preCompact" },
                                { type: "toolCalls", count: 1, tools: ["bash"] }
                            ]
                        }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = observe(&policy, state, ObservedEvent::PreCompact, timestamp(10));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_after(&policy, state, timestamp(20));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(30));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(30));
        assert_emits(&output, "Compaction reminder.");
        state = next;

        let (_, output) = tool_after(&policy, state, timestamp(40));
        assert_continue(&output);
    }

    #[test]
    fn reset_tool_clears_a_pending_pre_compact_latch() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Compaction reminder.",
                        resetTools: ["task"],
                        condition: {
                            type: "all",
                            conditions: [
                                { type: "event", event: "preCompact" },
                                { type: "toolCalls", count: 1, tools: ["bash"] }
                            ]
                        }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        let (next, output) = observe(&policy, state, ObservedEvent::PreCompact, timestamp(10));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "task", timestamp(20));
        assert_continue(&output);
        state = next;

        let (next, output) = tool_before(&policy, state, "bash", timestamp(30));
        assert_continue(&output);
        state = next;
        let (next, output) = tool_after(&policy, state, timestamp(30));
        assert_continue(&output);
        state = next;

        // The earlier bash count is still present, so a fresh latch now
        // satisfies the composite condition.
        let (next, output) = observe(&policy, state, ObservedEvent::PreCompact, timestamp(40));
        assert_continue(&output);
        state = next;
        let (_, output) = tool_after(&policy, state, timestamp(50));
        assert_emits(&output, "Compaction reminder.");
    }

    #[test]
    fn persisted_state_keeps_numeric_milliseconds_and_camel_case_names() {
        let policy = make_policy(FIFTH_TOOL_CALL);
        let state = BoundSession::fresh(&policy, timestamp(1_700_000_000_000)).into_state();
        let value =
            serde_json::to_value(&state).expect("session state should serialize to a JSON value");

        assert_eq!(value["schemaVersion"], serde_json::json!(1));
        assert_eq!(value["agent"], serde_json::json!("Orchestrator"));
        assert_eq!(
            value["policyFingerprint"],
            serde_json::json!(policy.fingerprint())
        );
        assert_eq!(
            value["intervals"][0]["resetAtMs"],
            serde_json::json!(1_700_000_000_000_u64)
        );
        assert!(value["intervals"][0].get("reset_at_ms").is_none());
        assert_eq!(value["intervals"][0]["counts"], serde_json::json!({}));
        assert_eq!(value["intervals"][0]["observed"], serde_json::json!([]));
    }

    #[test]
    fn legacy_numeric_fixture_restores_and_evolves() {
        let policy = make_policy(FIFTH_TOOL_CALL);
        let fixture = format!(
            r#"{{"schemaVersion":1,"agent":"Orchestrator","policyFingerprint":"{}","intervals":[{{"resetAtMs":1000,"counts":{{"bash":2}},"observed":[]}}]}}"#,
            policy.fingerprint()
        );
        let state: SessionState =
            serde_json::from_str(&fixture).expect("legacy persisted state should deserialize");
        let bound =
            BoundSession::restore(&policy, state).expect("legacy state should match its policy");

        let (state, output) = evolve(
            bound,
            Event::ToolBefore {
                tool: "bash".to_owned(),
                now_ms: timestamp(1_500),
            },
        );
        assert_continue(&output);

        let value =
            serde_json::to_value(&state).expect("session state should serialize to a JSON value");
        assert_eq!(value["intervals"][0]["resetAtMs"], serde_json::json!(1000));
        assert_eq!(
            value["intervals"][0]["counts"]["bash"],
            serde_json::json!(3)
        );
    }

    #[test]
    fn serialized_state_round_trips_and_resumes_counts_and_events() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Resumed reminder.",
                        condition: {
                            type: "all",
                            conditions: [
                                { type: "event", event: "preCompact" },
                                { type: "toolCalls", count: 3, tools: ["bash"] }
                            ]
                        }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));

        for call in 0..2 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let (next, output) = observe(&policy, state, ObservedEvent::PreCompact, timestamp(10));
        assert_continue(&output);
        state = next;

        let bytes = serde_json::to_vec(&state).expect("session state should serialize");
        let restored: SessionState =
            serde_json::from_slice(&bytes).expect("session state should deserialize");
        let bound = BoundSession::restore(&policy, restored)
            .expect("restored state should match the policy");

        let (next, output) = evolve(
            bound,
            Event::ToolBefore {
                tool: "bash".to_owned(),
                now_ms: timestamp(20),
            },
        );
        assert_continue(&output);
        let (_, output) = tool_after(&policy, next, timestamp(20));
        assert_emits(&output, "Resumed reminder.");
    }

    #[test]
    fn changed_policy_resets_existing_intervals() {
        let old_policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Old reminder.",
                        condition: { type: "toolCalls", count: 3, tools: ["bash"] }
                    }]
                }
            }"#,
        );
        let new_policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "New reminder.",
                        condition: { type: "toolCalls", count: 3, tools: ["bash"] }
                    }]
                }
            }"#,
        );
        assert_ne!(old_policy.fingerprint(), new_policy.fingerprint());

        let mut state = fresh_state(&old_policy, timestamp(0));
        for call in 0..2 {
            let (next, output) = tool_before(&old_policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&old_policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        assert!(matches!(
            BoundSession::restore(&new_policy, state),
            Err(StateError::PolicyMismatch)
        ));
        let mut state = fresh_state(&new_policy, timestamp(1000));
        let (next, output) = tool_after(&new_policy, state, timestamp(1000));
        assert_continue(&output);
        state = next;

        for call in 1001..1003 {
            let (next, output) = tool_before(&new_policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&new_policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let (next, output) = tool_before(&new_policy, state, "bash", timestamp(1003));
        assert_continue(&output);
        let (_, output) = tool_after(&new_policy, next, timestamp(1003));
        assert_emits(&output, "New reminder.");
    }

    #[test]
    fn zero_or_missing_interval_data_is_rejected_at_the_boundary_and_recovers_fresh() {
        let policy = make_policy(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Reset reminder.",
                        condition: { type: "toolCalls", count: 3, tools: ["bash"] }
                    }]
                }
            }"#,
        );
        let mut state = fresh_state(&policy, timestamp(0));
        for call in 0..2 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }

        let mut zeroed =
            serde_json::to_value(&state).expect("session state should serialize to a JSON value");
        zeroed["intervals"][0]["counts"]["bash"] = serde_json::json!(0);
        let tampered: SessionState =
            serde_json::from_value(zeroed).expect("tampered state should still deserialize");
        assert!(matches!(
            BoundSession::restore(&policy, tampered),
            Err(StateError::ZeroToolCount { .. })
        ));

        let (mut state, output) = tool_after(
            &policy,
            fresh_state(&policy, timestamp(500)),
            timestamp(500),
        );
        assert_continue(&output);
        for call in 501..503 {
            let (next, output) = tool_before(&policy, state, "bash", timestamp(call));
            assert_continue(&output);
            state = next;
            let (next, output) = tool_after(&policy, state, timestamp(call));
            assert_continue(&output);
            state = next;
        }
        let (state, output) = tool_before(&policy, state, "bash", timestamp(503));
        assert_continue(&output);
        let (state, output) = tool_after(&policy, state, timestamp(503));
        assert_emits(&output, "Reset reminder.");

        let mut missing =
            serde_json::to_value(&state).expect("session state should serialize to a JSON value");
        missing["intervals"] = serde_json::json!([]);
        let tampered: SessionState =
            serde_json::from_value(missing).expect("tampered state should still deserialize");
        assert!(matches!(
            BoundSession::restore(&policy, tampered),
            Err(StateError::IntervalCountMismatch { .. })
        ));
        let (_, output) = tool_after(
            &policy,
            fresh_state(&policy, timestamp(600)),
            timestamp(600),
        );
        assert_continue(&output);
    }

    #[test]
    fn unsupported_schema_version_resets_safely() {
        let policy = make_policy(FIFTH_TOOL_CALL);
        let state = fresh_state(&policy, timestamp(0));
        let mut value =
            serde_json::to_value(&state).expect("session state should serialize to a JSON value");
        value["schemaVersion"] = serde_json::json!(999);

        let tampered: SessionState =
            serde_json::from_value(value).expect("tampered state should still deserialize");
        assert!(matches!(
            BoundSession::restore(&policy, tampered),
            Err(StateError::UnsupportedVersion {
                found: 999,
                expected: SCHEMA_VERSION
            })
        ));

        let (_, output) = tool_after(
            &policy,
            fresh_state(&policy, timestamp(100)),
            timestamp(100),
        );
        assert_continue(&output);
    }
}
