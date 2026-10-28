//! Parsing and validation of the agent reminders configuration.
//!
//! The configuration is a JSON5 document (JSON with comments and trailing
//! commas). It maps agent names to independent reminder rules. Every field
//! shown below is required; there are no defaulted values:
//!
//! ```json5
//! {
//!   agents: {
//!     Orchestrator: [{
//!       message: "Remember your prompt obligations.",
//!       resetTools: ["task"],
//!       signalOnTools: ["*"],
//!       condition: {
//!         type: "toolCalls",
//!         count: 5,
//!         tools: ["bash", "powershell"],
//!       },
//!     }],
//!   },
//! }
//! ```
//!
//! [`Configuration::parse`] validates the whole document. [`Configuration::policy`]
//! then produces an immutable, owned [`Policy`] for one agent. An unknown agent
//! yields a policy with no rules rather than falling back to another agent's
//! rules.
//!
//! Every rule requires a `signalOnTools`: tool patterns that may carry that
//! rule's signal once its condition is satisfied. There is no implicit
//! fallback to `toolCalls.tools` or to a wildcard; omitting it is a parse
//! error. A rule is rejected at parse time if any `signalOnTools` pattern is
//! fully subsumed by one of that rule's
//! `resetTools` patterns (every tool it matches is also matched by that
//! reset pattern), since reset always wins and such a pattern could never
//! signal. Subsumption is checked against each reset pattern individually,
//! not against their union, so a signal pattern that is only partly covered
//! (e.g. `signalOnTools: ["*"]` alongside `resetTools: ["task"]`) is
//! accepted.
//!
//! Copilot command hooks do not expose context size or utilization. The
//! `contextGrowth` and `contextUtilizationGrowth` condition types are therefore
//! recognized but rejected with an explicit unsupported-feature error; they are
//! never silently treated as another condition.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::time::Seconds;

/// Domain separator for the policy fingerprint.
const FINGERPRINT_DOMAIN: &[u8] = b"agent-reminders.policy.v1\0";

/// Maximum number of nested object and array containers accepted in a document.
const MAX_NESTING_DEPTH: usize = 128;

/// A validated tool reference: an exact tool name or a trailing-`*` prefix.
///
/// The raw string is private so a `ToolPattern` can only be produced by the
/// parser in this module. Matching is case sensitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct ToolPattern(String);

impl ToolPattern {
    /// Returns whether `tool` is selected by this exact or prefix pattern.
    pub(crate) fn matches(&self, tool: &str) -> bool {
        match self.0.strip_suffix('*') {
            Some(prefix) => tool.starts_with(prefix),
            None => tool == self.0,
        }
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether the sets of tool names matched by two patterns intersect.
    fn overlaps(&self, other: &Self) -> bool {
        let (self_prefix, self_wildcard) = self.parts();
        let (other_prefix, other_wildcard) = other.parts();

        match (self_wildcard, other_wildcard) {
            (true, true) => {
                self_prefix.starts_with(other_prefix) || other_prefix.starts_with(self_prefix)
            }
            (true, false) => other_prefix.starts_with(self_prefix),
            (false, true) => self_prefix.starts_with(other_prefix),
            (false, false) => self_prefix == other_prefix,
        }
    }

    fn parts(&self) -> (&str, bool) {
        match self.0.strip_suffix('*') {
            Some(prefix) => (prefix, true),
            None => (&self.0, false),
        }
    }

    /// Returns whether every tool matched by `self` is also matched by `other`,
    /// i.e. `self` can never match a tool that `other` does not also match.
    fn subsumed_by(&self, other: &Self) -> bool {
        let (self_prefix, self_wildcard) = self.parts();
        let (other_prefix, other_wildcard) = other.parts();

        match (self_wildcard, other_wildcard) {
            (true, true) => self_prefix.starts_with(other_prefix),
            (true, false) => false,
            (false, true) => self_prefix.starts_with(other_prefix),
            (false, false) => self_prefix == other_prefix,
        }
    }
}

impl fmt::Display for ToolPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An observed session event that can latch inside a reminder interval.
///
/// Only `preCompact` is exposed by Copilot command hooks. The variant is
/// serializable because pending observations are part of [`crate::machine`]'s
/// persisted session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ObservedEvent {
    /// Context compaction is about to begin.
    PreCompact,
}

/// A validated `toolCalls` condition.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolCallsCondition {
    count: NonZeroU64,
    tools: Vec<ToolPattern>,
}

impl ToolCallsCondition {
    /// Number of matching invocations required to fire.
    pub(crate) fn count(&self) -> NonZeroU64 {
        self.count
    }

    /// Nonempty tool patterns whose invocations are counted.
    pub(crate) fn tools(&self) -> &[ToolPattern] {
        &self.tools
    }
}

/// A validated `elapsedTime` condition.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ElapsedTimeCondition {
    seconds: Seconds,
}

impl ElapsedTimeCondition {
    /// Positive, finite threshold in seconds.
    pub(crate) fn seconds(&self) -> Seconds {
        self.seconds
    }
}

/// A validated `event` condition, which waits for an observed event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventCondition {
    event: ObservedEvent,
}

impl EventCondition {
    /// Event that must be observed within the interval.
    pub(crate) fn event(&self) -> ObservedEvent {
        self.event
    }
}

/// A validated nonempty list of child conditions for `all` or `any`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CompositeCondition {
    conditions: Vec<Condition>,
}

impl CompositeCondition {
    /// Nonempty child conditions, in configuration order.
    pub(crate) fn conditions(&self) -> &[Condition] {
        &self.conditions
    }
}

/// A validated delivery condition.
///
/// Variant payload types have private fields and can only be constructed by the
/// parser, so a `Condition` held by a [`Policy`] satisfies its documented
/// invariants.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum Condition {
    /// Fires after a number of matching tool invocations.
    ToolCalls(ToolCallsCondition),
    /// Fires after a wall-clock duration.
    ElapsedTime(ElapsedTimeCondition),
    /// Fires after a configured event has been observed.
    Event(EventCondition),
    /// Fires when every child condition fires.
    All(CompositeCondition),
    /// Fires when at least one child condition fires.
    Any(CompositeCondition),
}

/// One validated reminder rule.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentReminderEntry {
    message: String,
    reset_tools: Vec<ToolPattern>,
    signal_on_tools: Vec<ToolPattern>,
    condition: Condition,
}

impl AgentReminderEntry {
    /// Reminder text emitted when the condition fires.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Tool patterns whose invocation resets this rule's interval.
    pub(crate) fn reset_tools(&self) -> &[ToolPattern] {
        &self.reset_tools
    }

    /// Tool patterns eligible to carry this rule's signal.
    pub(crate) fn signal_on_tools(&self) -> &[ToolPattern] {
        &self.signal_on_tools
    }

    /// Delivery condition for this rule.
    pub(crate) fn condition(&self) -> &Condition {
        &self.condition
    }
}

/// A fully validated configuration document.
///
/// Agent and rule ordering is deterministic: agents are stored in a sorted map
/// and rules keep their configured array order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Configuration {
    agents: BTreeMap<String, Vec<AgentReminderEntry>>,
}

impl Configuration {
    /// Parses and validates a JSON5 configuration document.
    ///
    /// The document may contain at most 128 levels of nested objects or arrays.
    /// This bound is checked lexically before JSON5 deserialization, so an
    /// excessively nested document is rejected without recursing over its
    /// structure; brackets inside strings and comments do not count toward the
    /// bound. The check is a conservative structural safety limit, not a JSON5
    /// validation pass.
    ///
    /// Unknown fields, malformed tool patterns, nonpositive thresholds, empty
    /// agent names or rule arrays, empty composite conditions, overlapping
    /// counted/reset patterns, overlapping explicit `denyTools`/`resetTools`
    /// patterns, and unsupported context conditions are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::NestingLimitExceeded`] when the document nests
    /// objects or arrays more than 128 levels deep. Returns [`ConfigError`] when
    /// the document is not valid JSON5 or violates the configuration
    /// invariants.
    pub(crate) fn parse(input: &str) -> Result<Self, ConfigError> {
        ensure_nesting_within_limit(input)?;
        let raw: RawConfiguration = json5::from_str(input)?;
        let mut agents = BTreeMap::new();

        for (agent, raw_rules) in raw.agents {
            validate_nonempty_name(&agent)?;
            if raw_rules.is_empty() {
                return Err(ConfigError::EmptyAgentRules { agent });
            }

            let mut rules = Vec::with_capacity(raw_rules.len());
            for (rule_index, raw_rule) in raw_rules.into_iter().enumerate() {
                rules.push(parse_reminder(&agent, rule_index, raw_rule)?);
            }
            agents.insert(agent, rules);
        }

        Ok(Self { agents })
    }

    /// Returns `true` when the document configures no agents.
    ///
    /// A document that is syntactically valid but has a missing or empty
    /// `agents` map disables every reminder; the hook uses this to warn without
    /// treating an unknown agent in a nonempty configuration as an error.
    pub(crate) fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Returns the number of configured agents.
    pub(crate) fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Returns the total number of rules across all configured agents.
    pub(crate) fn rule_count(&self) -> usize {
        self.agents.values().map(Vec::len).sum()
    }

    /// Returns the immutable, owned policy for an agent.
    ///
    /// Unknown agents deliberately receive an empty policy instead of another
    /// agent's rules.
    pub(crate) fn policy(&self, agent: &str) -> Policy {
        let rules = self.agents.get(agent).cloned().unwrap_or_default();
        Policy::new(agent.to_owned(), rules)
    }
}

/// Lexical state used by [`ensure_nesting_within_limit`].
#[derive(Clone, Copy)]
enum ScanState {
    Normal,
    SingleQuotedString,
    DoubleQuotedString,
    LineComment,
    BlockComment,
}

/// Rejects documents that nest objects or arrays deeper than [`MAX_NESTING_DEPTH`].
///
/// The scan is iterative and stops at the first container that exceeds the
/// bound, so the guard itself never recurses. It recognizes strings, escapes,
/// line comments, and block comments only far enough to skip brackets that are
/// not structural. Invalid syntax is left for the JSON5 parser to report.
fn ensure_nesting_within_limit(input: &str) -> Result<(), ConfigError> {
    let mut bytes = input.bytes().peekable();
    let mut state = ScanState::Normal;
    let mut depth = 0_usize;

    while let Some(byte) = bytes.next() {
        match state {
            ScanState::Normal => match byte {
                b'\'' => state = ScanState::SingleQuotedString,
                b'"' => state = ScanState::DoubleQuotedString,
                b'/' => match bytes.peek() {
                    Some(b'/') => {
                        bytes.next();
                        state = ScanState::LineComment;
                    }
                    Some(b'*') => {
                        bytes.next();
                        state = ScanState::BlockComment;
                    }
                    _ => {}
                },
                b'{' | b'[' => {
                    if depth == MAX_NESTING_DEPTH {
                        return Err(nesting_limit_error());
                    }
                    depth += 1;
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            },
            ScanState::SingleQuotedString => {
                if byte == b'\\' {
                    bytes.next();
                } else if byte == b'\'' {
                    state = ScanState::Normal;
                }
            }
            ScanState::DoubleQuotedString => {
                if byte == b'\\' {
                    bytes.next();
                } else if byte == b'"' {
                    state = ScanState::Normal;
                }
            }
            ScanState::LineComment => {
                if matches!(byte, b'\n' | b'\r') {
                    state = ScanState::Normal;
                } else if byte == 0xE2 && bytes.peek() == Some(&0x80) {
                    // U+2028 and U+2029 are UTF-8 `E2 80 A8` and `E2 80 A9`.
                    bytes.next();
                    if matches!(bytes.peek().copied(), Some(0xA8 | 0xA9)) {
                        bytes.next();
                        state = ScanState::Normal;
                    }
                }
            }
            ScanState::BlockComment => {
                if byte == b'*' && bytes.peek() == Some(&b'/') {
                    bytes.next();
                    state = ScanState::Normal;
                }
            }
        }
    }

    Ok(())
}

fn nesting_limit_error() -> ConfigError {
    ConfigError::NestingLimitExceeded {
        limit: MAX_NESTING_DEPTH,
    }
}

/// An immutable rule set for one selected agent.
///
/// A fingerprint of the selected agent plus its rules identifies the policy.
/// Persisted session state stores that fingerprint, so changing either the
/// agent or any rule resets the session's intervals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Policy {
    agent: String,
    rules: Vec<AgentReminderEntry>,
    #[serde(skip)]
    fingerprint: String,
}

impl Policy {
    fn new(agent: String, rules: Vec<AgentReminderEntry>) -> Self {
        let fingerprint = fingerprint_for(&agent, &rules);
        Self {
            agent,
            rules,
            fingerprint,
        }
    }

    /// Agent name selected for this policy.
    pub(crate) fn agent(&self) -> &str {
        &self.agent
    }

    /// Stable lowercase SHA-256 hex digest of the agent and its rules.
    ///
    /// The digest excludes itself and uses a canonical length-prefixed
    /// encoding, so equivalent configurations always produce the same value.
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Rules in configuration order.
    pub(crate) fn entries(&self) -> &[AgentReminderEntry] {
        &self.rules
    }
}

/// A configuration validation failure.
#[derive(Debug, Error)]
pub(crate) enum ConfigError {
    /// The input is not valid JSON5 or has an unknown document/rule field.
    #[error("invalid JSON5 configuration: {0}")]
    Syntax(#[from] json5::Error),

    /// The document nests objects or arrays more deeply than is supported.
    #[error(
        "configuration nesting exceeds the supported maximum of {limit} nested `{{` or `[` \
         containers"
    )]
    NestingLimitExceeded {
        /// Maximum supported number of nested object and array containers.
        limit: usize,
    },

    /// An agent name was present but empty.
    #[error("agent names must not be empty")]
    EmptyAgentName,

    /// An agent was configured without any reminder rules.
    #[error("agent \"{agent}\" must configure at least one reminder")]
    EmptyAgentRules {
        /// Agent whose rule array was empty.
        agent: String,
    },

    /// A rule-level invariant failed.
    #[error("agent \"{agent}\" reminder {rule}: {problem}")]
    InvalidRule {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Description of the failed invariant.
        problem: String,
    },

    /// A condition-level invariant failed.
    #[error("agent \"{agent}\" reminder {rule}: condition {path}: {problem}")]
    InvalidCondition {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Path from the rule's root condition to the failing field.
        path: String,
        /// Description of the failed invariant.
        problem: String,
    },

    /// A tool pattern is not an exact name or a trailing-`*` prefix.
    #[error(
        "agent \"{agent}\" reminder {rule}: tool pattern \"{pattern}\" in {field} is invalid: \
         expected ASCII letters, digits, underscores, or hyphens, optionally followed by one `*`"
    )]
    InvalidToolPattern {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Field or condition path containing the pattern.
        field: String,
        /// Rejected pattern text.
        pattern: String,
    },

    /// A recognized condition requires capabilities Copilot command hooks lack.
    #[error(
        "agent \"{agent}\" reminder {rule}: condition {path}: `{condition}` is not supported by \
         Copilot command hooks because context measurements are unavailable"
    )]
    UnsupportedCondition {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Path from the rule's root condition to the unsupported node.
        path: String,
        /// Unsupported condition type as written in the configuration.
        condition: &'static str,
    },

    /// A tool pattern is both counted and resetting within one rule.
    #[error(
        "agent \"{agent}\" reminder {rule}: counted tool pattern \"{counted}\" overlaps reset tool \
         pattern \"{reset}\""
    )]
    OverlappingToolPatterns {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Counted pattern from a `toolCalls` condition.
        counted: String,
        /// Overlapping reset pattern.
        reset: String,
    },

    /// A `signalOnTools` pattern matches only tools already covered by a single
    /// `resetTools` pattern, so it could never carry a signal.
    #[error(
        "agent \"{agent}\" reminder {rule}: signalOnTools pattern \"{pattern}\" at {path} is fully \
         covered by resetTools pattern \"{reset}\"; a reset tool never signals, so this pattern \
         could never carry a signal"
    )]
    SignalOnToolsSubsumedByReset {
        /// Agent containing the rule.
        agent: String,
        /// Zero-based rule index within the agent's array.
        rule: usize,
        /// Path from the rule's root condition to the subsumed pattern.
        path: String,
        /// Subsumed `signalOnTools` pattern.
        pattern: String,
        /// `resetTools` pattern that covers it.
        reset: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfiguration {
    #[serde(default)]
    agents: BTreeMap<String, Vec<RawReminder>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReminder {
    message: String,
    #[serde(rename = "resetTools")]
    reset_tools: Vec<String>,
    #[serde(rename = "signalOnTools")]
    signal_on_tools: Vec<String>,
    condition: Value,
}

/// Parse-time location of a rule or condition within the configuration.
///
/// The parser descends through composite conditions by deriving child
/// locations, so validation errors identify the exact failing node.
#[derive(Debug, Clone)]
struct RuleLocation<'a> {
    agent: &'a str,
    rule: usize,
    path: String,
}

impl<'a> RuleLocation<'a> {
    fn new(agent: &'a str, rule: usize, path: impl Into<String>) -> Self {
        Self {
            agent,
            rule,
            path: path.into(),
        }
    }

    /// Location of the child condition at `index` within a composite condition.
    fn child_condition(&self, index: usize) -> Self {
        Self::new(
            self.agent,
            self.rule,
            format!("{}.conditions[{index}]", self.path),
        )
    }

    /// Location of the tool pattern at `index` within this condition's `tools`.
    fn tool(&self, index: usize) -> Self {
        Self::new(
            self.agent,
            self.rule,
            format!("{}.tools[{index}]", self.path),
        )
    }

    /// Location of the tool pattern at `index` within this rule's
    /// `signalOnTools`.
    fn signal_tool(&self, index: usize) -> Self {
        Self::new(self.agent, self.rule, format!("signalOnTools[{index}]"))
    }
}

fn parse_reminder(
    agent: &str,
    rule: usize,
    raw: RawReminder,
) -> Result<AgentReminderEntry, ConfigError> {
    if raw.message.is_empty() {
        return Err(invalid_rule(agent, rule, "message must not be empty"));
    }

    let mut reset_tools = Vec::with_capacity(raw.reset_tools.len());
    for (pattern_index, pattern) in raw.reset_tools.iter().enumerate() {
        let location = RuleLocation::new(agent, rule, format!("resetTools[{pattern_index}]"));
        reset_tools.push(parse_tool_pattern(pattern, &location)?);
    }

    let mut signal_on_tools = Vec::with_capacity(raw.signal_on_tools.len());
    let signal_location = RuleLocation::new(agent, rule, "");
    for (pattern_index, pattern) in raw.signal_on_tools.iter().enumerate() {
        let pattern_location = signal_location.signal_tool(pattern_index);
        let pattern = parse_tool_pattern(pattern, &pattern_location)?;
        for reset in &reset_tools {
            if pattern.subsumed_by(reset) {
                return Err(ConfigError::SignalOnToolsSubsumedByReset {
                    agent: agent.to_owned(),
                    rule,
                    path: pattern_location.path.clone(),
                    pattern: pattern.to_string(),
                    reset: reset.to_string(),
                });
            }
        }
        signal_on_tools.push(pattern);
    }

    let location = RuleLocation::new(agent, rule, "condition");
    let condition = parse_condition(&raw.condition, &location)?;

    let entry = AgentReminderEntry {
        message: raw.message,
        reset_tools,
        signal_on_tools,
        condition,
    };
    validate_no_count_reset_overlap(agent, rule, &entry)?;
    Ok(entry)
}

fn validate_nonempty_name(agent: &str) -> Result<(), ConfigError> {
    if agent.is_empty() {
        Err(ConfigError::EmptyAgentName)
    } else {
        Ok(())
    }
}

/// Validates a tool pattern: an exact name, a trailing-`*` prefix, or the
/// bare wildcard `*` matching every tool.
fn parse_tool_pattern(
    value: &str,
    location: &RuleLocation<'_>,
) -> Result<ToolPattern, ConfigError> {
    if value == "*" {
        return Ok(ToolPattern(value.to_owned()));
    }

    let prefix = value.strip_suffix('*').unwrap_or(value);
    let is_valid = !prefix.is_empty()
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');

    if is_valid {
        Ok(ToolPattern(value.to_owned()))
    } else {
        Err(ConfigError::InvalidToolPattern {
            agent: location.agent.to_owned(),
            rule: location.rule,
            field: location.path.clone(),
            pattern: value.to_owned(),
        })
    }
}

fn parse_condition(value: &Value, location: &RuleLocation<'_>) -> Result<Condition, ConfigError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_condition(location, "must be an object"))?;
    let condition_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_condition(location, "must contain a string `type`"))?;

    match condition_type {
        "toolCalls" => {
            ensure_only_fields(object, &["type", "count", "tools"], location)?;
            let count_value = object
                .get("count")
                .ok_or_else(|| missing_field(location, "count"))?;
            let count = positive_count(count_value)
                .ok_or_else(|| invalid_condition(location, "`count` must be a positive integer"))?;
            let tools_value = object
                .get("tools")
                .ok_or_else(|| missing_field(location, "tools"))?;
            let tools = parse_tool_array(tools_value, location)?;
            Ok(Condition::ToolCalls(ToolCallsCondition { count, tools }))
        }
        "elapsedTime" => {
            ensure_only_fields(object, &["type", "seconds"], location)?;
            let seconds = object
                .get("seconds")
                .ok_or_else(|| missing_field(location, "seconds"))
                .and_then(|value| {
                    positive_finite(value).ok_or_else(|| {
                        invalid_condition(location, "`seconds` must be a positive finite number")
                    })
                })?;
            Ok(Condition::ElapsedTime(ElapsedTimeCondition { seconds }))
        }
        "event" => {
            ensure_only_fields(object, &["type", "event"], location)?;
            let event_name = object
                .get("event")
                .ok_or_else(|| missing_field(location, "event"))?
                .as_str()
                .ok_or_else(|| invalid_condition(location, "`event` must be a string"))?;
            let event = match event_name {
                "preCompact" => ObservedEvent::PreCompact,
                other => {
                    return Err(invalid_condition(
                        location,
                        format!("unknown event `{other}`; expected `preCompact`"),
                    ));
                }
            };
            Ok(Condition::Event(EventCondition { event }))
        }
        "contextGrowth" => {
            ensure_only_fields(object, &["type", "tokens"], location)?;
            require_positive_number(object, "tokens", location)?;
            Err(ConfigError::UnsupportedCondition {
                agent: location.agent.to_owned(),
                rule: location.rule,
                path: location.path.clone(),
                condition: "contextGrowth",
            })
        }
        "contextUtilizationGrowth" => {
            ensure_only_fields(object, &["type", "percentagePoints"], location)?;
            require_positive_number(object, "percentagePoints", location)?;
            Err(ConfigError::UnsupportedCondition {
                agent: location.agent.to_owned(),
                rule: location.rule,
                path: location.path.clone(),
                condition: "contextUtilizationGrowth",
            })
        }
        "all" | "any" => {
            ensure_only_fields(object, &["type", "conditions"], location)?;
            let raw_conditions = object
                .get("conditions")
                .ok_or_else(|| missing_field(location, "conditions"))?
                .as_array()
                .ok_or_else(|| {
                    invalid_condition(
                        location,
                        "`conditions` must be a nonempty array of conditions",
                    )
                })?;
            if raw_conditions.is_empty() {
                return Err(invalid_condition(
                    location,
                    "`conditions` must be a nonempty array of conditions",
                ));
            }

            let mut conditions = Vec::with_capacity(raw_conditions.len());
            for (child_index, child) in raw_conditions.iter().enumerate() {
                let child_location = location.child_condition(child_index);
                conditions.push(parse_condition(child, &child_location)?);
            }

            let composite = CompositeCondition { conditions };
            if condition_type == "all" {
                Ok(Condition::All(composite))
            } else {
                Ok(Condition::Any(composite))
            }
        }
        other => Err(invalid_condition(
            location,
            format!("unknown condition type `{other}`"),
        )),
    }
}

fn parse_tool_array(
    value: &Value,
    location: &RuleLocation<'_>,
) -> Result<Vec<ToolPattern>, ConfigError> {
    let array = value.as_array().ok_or_else(|| {
        invalid_condition(
            location,
            "`tools` must be a nonempty array of tool patterns",
        )
    })?;
    if array.is_empty() {
        return Err(invalid_condition(
            location,
            "`tools` must be a nonempty array of tool patterns",
        ));
    }

    let mut tools = Vec::with_capacity(array.len());
    for (pattern_index, item) in array.iter().enumerate() {
        let pattern = item.as_str().ok_or_else(|| {
            invalid_condition(location, format!("tools[{pattern_index}] must be a string"))
        })?;
        tools.push(parse_tool_pattern(pattern, &location.tool(pattern_index))?);
    }
    Ok(tools)
}

fn require_positive_number(
    object: &Map<String, Value>,
    field: &str,
    location: &RuleLocation<'_>,
) -> Result<f64, ConfigError> {
    let value = object
        .get(field)
        .ok_or_else(|| missing_field(location, field))?;
    positive_finite(value).ok_or_else(|| {
        invalid_condition(
            location,
            format!("`{field}` must be a positive finite number"),
        )
    })
}

fn positive_finite(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|number| number.is_finite() && *number > 0.0)
}

/// Resolves a positive integer count from either integer or float notation.
///
/// JSON5 can represent the same positive integer as `5`, `5.0`, or `5e0`.
/// Integer notation is resolved through `Value::as_u64` first, preserving exact
/// integer values before any float handling. Float notation is accepted only
/// when it is finite, positive, has no fractional part, and its exact decimal
/// representation parses as a `u64`; this rejects 2^64 and above without an
/// unchecked float-to-integer cast.
fn positive_count(value: &Value) -> Option<NonZeroU64> {
    if let Some(count) = value.as_u64().and_then(NonZeroU64::new) {
        return Some(count);
    }

    let number = value.as_f64()?;
    if !number.is_finite() || number <= 0.0 || number.fract() != 0.0 {
        return None;
    }

    let rendered = format!("{number:.0}");
    rendered.parse::<u64>().ok().and_then(NonZeroU64::new)
}

fn ensure_only_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    location: &RuleLocation<'_>,
) -> Result<(), ConfigError> {
    for field in object.keys() {
        if !allowed.contains(&field.as_str()) {
            return Err(invalid_condition(
                location,
                format!("unknown field `{field}`"),
            ));
        }
    }
    Ok(())
}

fn missing_field(location: &RuleLocation<'_>, field: &str) -> ConfigError {
    invalid_condition(location, format!("missing required field `{field}`"))
}

fn invalid_rule(agent: &str, rule: usize, problem: impl Into<String>) -> ConfigError {
    ConfigError::InvalidRule {
        agent: agent.to_owned(),
        rule,
        problem: problem.into(),
    }
}

fn invalid_condition(location: &RuleLocation<'_>, problem: impl Into<String>) -> ConfigError {
    ConfigError::InvalidCondition {
        agent: location.agent.to_owned(),
        rule: location.rule,
        path: location.path.clone(),
        problem: problem.into(),
    }
}

fn validate_no_count_reset_overlap(
    agent: &str,
    rule_index: usize,
    entry: &AgentReminderEntry,
) -> Result<(), ConfigError> {
    let mut counted_patterns = Vec::new();
    collect_counted_patterns(&entry.condition, &mut counted_patterns);

    for counted in counted_patterns {
        for reset in &entry.reset_tools {
            if counted.overlaps(reset) {
                return Err(ConfigError::OverlappingToolPatterns {
                    agent: agent.to_owned(),
                    rule: rule_index,
                    counted: counted.to_string(),
                    reset: reset.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn collect_counted_patterns<'a>(condition: &'a Condition, output: &mut Vec<&'a ToolPattern>) {
    match condition {
        Condition::ToolCalls(tool_calls) => output.extend(tool_calls.tools()),
        Condition::All(composite) | Condition::Any(composite) => {
            for child in composite.conditions() {
                collect_counted_patterns(child, output);
            }
        }
        Condition::ElapsedTime(_) | Condition::Event(_) => {}
    }
}

fn fingerprint_for(agent: &str, rules: &[AgentReminderEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    update_len_prefixed(&mut hasher, agent.as_bytes());
    hasher.update(count_as_u64(rules.len()).to_le_bytes());

    for rule in rules {
        update_len_prefixed(&mut hasher, rule.message.as_bytes());
        hash_tool_patterns(&mut hasher, &rule.reset_tools);
        hash_tool_patterns(&mut hasher, &rule.signal_on_tools);
        hash_condition(&mut hasher, &rule.condition);
    }

    hex_digest(hasher.finalize().as_slice())
}

fn hash_condition(hasher: &mut Sha256, condition: &Condition) {
    match condition {
        Condition::ToolCalls(tool_calls) => {
            hasher.update([0_u8]);
            hasher.update(tool_calls.count.get().to_le_bytes());
            hasher.update(count_as_u64(tool_calls.tools.len()).to_le_bytes());
            for pattern in &tool_calls.tools {
                update_len_prefixed(hasher, pattern.as_str().as_bytes());
            }
        }
        Condition::ElapsedTime(elapsed) => {
            hasher.update([1_u8]);
            hasher.update(elapsed.seconds.to_bits().to_le_bytes());
        }
        Condition::Event(event) => {
            hasher.update([2_u8]);
            let ordinal = match event.event {
                ObservedEvent::PreCompact => 0_u8,
            };
            hasher.update([ordinal]);
        }
        Condition::All(composite) => {
            hasher.update([3_u8]);
            hash_children(hasher, composite);
        }
        Condition::Any(composite) => {
            hasher.update([4_u8]);
            hash_children(hasher, composite);
        }
    }
}

fn hash_tool_patterns(hasher: &mut Sha256, patterns: &[ToolPattern]) {
    hasher.update(count_as_u64(patterns.len()).to_le_bytes());
    for pattern in patterns {
        update_len_prefixed(hasher, pattern.as_str().as_bytes());
    }
}

fn hash_children(hasher: &mut Sha256, composite: &CompositeCondition) {
    hasher.update(count_as_u64(composite.conditions.len()).to_le_bytes());
    for child in &composite.conditions {
        hash_condition(hasher, child);
    }
}

fn update_len_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(count_as_u64(bytes.len()).to_le_bytes());
    hasher.update(bytes);
}

fn count_as_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

fn hex_digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MINIMAL: &str = r#"
        {
            agents: {
                Orchestrator: [{
                    message: "Remember your prompt obligations.",
                    resetTools: ["task"],
                    signalOnTools: ["bash", "powershell"],
                    condition: {
                        type: "toolCalls",
                        count: 5,
                        tools: ["bash", "powershell"],
                    }
                }]
            }
        }
    "#;

    fn parse(input: &str) -> Configuration {
        Configuration::parse(input).expect("configuration should be valid")
    }

    #[test]
    fn subsumed_by_covers_all_pattern_kind_combinations() {
        let exact = |s: &str| ToolPattern(s.to_owned());
        let prefix = |s: &str| ToolPattern(format!("{s}*"));

        // exact/exact: equal iff the strings are equal.
        assert!(exact("task").subsumed_by(&exact("task")));
        assert!(!exact("task").subsumed_by(&exact("bash")));

        // exact/wildcard: subsumed iff the exact name has the prefix.
        assert!(exact("task-runner").subsumed_by(&prefix("task")));
        assert!(!exact("taskless").subsumed_by(&prefix("task-")));

        // wildcard/exact: a prefix pattern always matches more than one exact name.
        assert!(!prefix("task").subsumed_by(&exact("task")));

        // wildcard/wildcard: subsumed iff the narrower prefix has the wider prefix.
        assert!(prefix("task-runner").subsumed_by(&prefix("task")));
        assert!(!prefix("task").subsumed_by(&prefix("task-runner")));
    }

    fn policy_value(configuration: &Configuration, agent: &str) -> Value {
        serde_json::to_value(configuration.policy(agent)).expect("policy should serialize")
    }

    #[test]
    fn explicit_fields_round_trip_through_the_policy() {
        let configuration = parse(MINIMAL);
        let policy = configuration.policy("Orchestrator");

        assert_eq!(policy.agent(), "Orchestrator");
        assert_eq!(policy.entries().len(), 1);
        assert_eq!(
            policy_value(&configuration, "Orchestrator"),
            json!({
                "agent": "Orchestrator",
                "rules": [{
                    "message": "Remember your prompt obligations.",
                    "resetTools": ["task"],
                    "signalOnTools": ["bash", "powershell"],
                    "condition": {
                        "type": "toolCalls",
                        "count": 5,
                        "tools": ["bash", "powershell"]
                    }
                }]
            })
        );
    }

    #[test]
    fn an_explicit_empty_reset_tool_list_is_preserved() {
        let configuration = parse(
            r#"{
                agents: {
                    Reviewer: [{
                        message: "Review.",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "elapsedTime",
                            seconds: 0.5,
                        }
                    }]
                }
            }"#,
        );

        assert_eq!(
            policy_value(&configuration, "Reviewer")["rules"][0]["resetTools"],
            json!([])
        );
    }

    #[test]
    fn unknown_agent_has_an_empty_policy() {
        let configuration = parse(MINIMAL);
        let policy = configuration.policy("Reviewer");

        assert_eq!(policy.agent(), "Reviewer");
        assert!(policy.entries().is_empty());
        assert_ne!(
            policy.fingerprint(),
            configuration.policy("Orchestrator").fingerprint()
        );
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        let documents = [
            r#"{ agents: {}, extra: true }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Remember.",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "elapsedTime",
                            seconds: 1,
                        },
                        extra: true
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Remember.",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "elapsedTime",
                            seconds: 1,
                            extra: true
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Remember.",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 2,
                            tools: ["bash"],
                            seconds: 1
                        }
                    }]
                }
            }"#,
        ];

        for document in documents {
            let error = Configuration::parse(document).expect_err("unknown field must be rejected");
            assert!(
                error.to_string().contains("unknown field")
                    || error.to_string().contains("unknown field `seconds`"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn missing_required_fields_are_rejected_with_a_path_accurate_message() {
        let documents = [
            (
                r#"{
                    agents: {
                        Orchestrator: [{
                            condition: { type: "elapsedTime", seconds: 1 },
                            resetTools: []
                        }]
                    }
                }"#,
                "message",
            ),
            (
                r#"{
                    agents: {
                        Orchestrator: [{
                            message: "x",
                            condition: { type: "elapsedTime", seconds: 1 }
                        }]
                    }
                }"#,
                "resetTools",
            ),
            (
                r#"{
                    agents: {
                        Orchestrator: [{
                            message: "x",
                            resetTools: [],
                            signalOnTools: ["*"]
                        }]
                    }
                }"#,
                "condition",
            ),
        ];

        for (document, missing_field_name) in documents {
            let error = Configuration::parse(document).expect_err("missing field must be rejected");
            assert!(
                error.to_string().contains(missing_field_name),
                "error should mention {missing_field_name:?}: {error}"
            );
        }

        let missing_count = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                        signalOnTools: ["*"],
                    condition: { type: "toolCalls", tools: ["bash"] }
                }]
            }
        }"#;
        let error = Configuration::parse(missing_count).expect_err("count is required");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 0: condition condition: missing required field \
             `count`"
        );

        let missing_tools = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                        signalOnTools: ["*"],
                    condition: { type: "toolCalls", count: 1 }
                }]
            }
        }"#;
        let error = Configuration::parse(missing_tools).expect_err("tools is required");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 0: condition condition: missing required field \
             `tools`"
        );

        let missing_seconds = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                        signalOnTools: ["*"],
                    condition: { type: "elapsedTime" }
                }]
            }
        }"#;
        let error = Configuration::parse(missing_seconds).expect_err("seconds is required");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 0: condition condition: missing required field \
             `seconds`"
        );

        let missing_event = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                        signalOnTools: ["*"],
                    condition: { type: "event" }
                }]
            }
        }"#;
        let error = Configuration::parse(missing_event).expect_err("event is required");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 0: condition condition: missing required field \
             `event`"
        );

        let missing_signal_on_tools = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                    condition: {
                        type: "any",
                        conditions: [
                            { type: "event", event: "preCompact" }
                        ]
                    }
                }]
            }
        }"#;
        let error =
            Configuration::parse(missing_signal_on_tools).expect_err("signalOnTools is required");
        assert!(
            error.to_string().contains("missing field `signalOnTools`"),
            "error should mention missing signalOnTools: {error}"
        );
    }

    #[test]
    fn nested_validation_errors_report_the_full_condition_path() {
        let invalid_seconds = r#"{
            agents: {
                Orchestrator: [
                    {
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "elapsedTime",
                            seconds: 1,
                        }
                    },
                    {
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "any",
                            conditions: [
                                { type: "all", conditions: [
                                    {
                                        type: "elapsedTime",
                                        seconds: 0,
                                    }
                                ] }
                            ]
                        }
                    }
                ]
            }
        }"#;

        let error = Configuration::parse(invalid_seconds)
            .expect_err("nonpositive nested seconds must be rejected");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 1: condition \
             condition.conditions[0].conditions[0]: `seconds` must be a positive finite number"
        );

        let invalid_tool = r#"{
            agents: {
                Orchestrator: [{
                    message: "x",
                    resetTools: [],
                        signalOnTools: ["*"],
                    condition: {
                        type: "any",
                        conditions: [
                            {
                                type: "toolCalls",
                                count: 1,
                                tools: ["bash", "bad*pattern"],
                            }
                        ]
                    }
                }]
            }
        }"#;

        let error = Configuration::parse(invalid_tool)
            .expect_err("malformed nested tool pattern must be rejected");
        assert_eq!(
            error.to_string(),
            "agent \"Orchestrator\" reminder 0: tool pattern \"bad*pattern\" in \
             condition.conditions[0].tools[1] is invalid: expected ASCII letters, digits, \
             underscores, or hyphens, optionally followed by one `*`"
        );
    }

    #[test]
    fn empty_and_nonpositive_values_are_rejected() {
        let invalid_documents = [
            r#"{ agents: { "": [{
                message: "x",
                resetTools: [],
                        signalOnTools: ["*"],
                condition: { type: "elapsedTime", seconds: 1 }
            }] } }"#,
            r#"{ agents: { Orchestrator: [] } }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: { type: "elapsedTime", seconds: 1 }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 0,
                            tools: ["bash"],
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: -1,
                            tools: ["bash"],
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 2,
                            tools: [],
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: { type: "elapsedTime", seconds: 0 }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: { type: "elapsedTime", seconds: -0.5 }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: { type: "all", conditions: [] }
                    }]
                }
            }"#,
        ];

        for document in invalid_documents {
            assert!(
                Configuration::parse(document).is_err(),
                "document should be rejected: {document}"
            );
        }
    }

    #[test]
    fn integral_numeric_count_notations_are_equivalent() {
        let count_notations = ["5", "5.0", "5e0"];
        let mut policies = Vec::new();

        for count in count_notations {
            let document = format!(
                r#"{{
                    agents: {{
                        Orchestrator: [{{
                            message: "x",
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {{
                                type: "toolCalls",
                                count: {count},
                                tools: ["bash"],
                            }}
                        }}]
                    }}
                }}"#
            );
            let configuration = Configuration::parse(&document)
                .unwrap_or_else(|error| panic!("count {count:?} should parse: {error}"));
            assert_eq!(
                policy_value(&configuration, "Orchestrator")["rules"][0]["condition"]["count"],
                json!(5),
                "count {count:?} should normalize to an integer"
            );
            policies.push(configuration.policy("Orchestrator"));
        }

        let mut fingerprints = policies.iter().map(Policy::fingerprint);
        let first = fingerprints.next().expect("at least one policy");
        assert!(
            fingerprints.all(|fingerprint| fingerprint == first),
            "equivalent counts must produce the same policy fingerprint"
        );
    }

    #[test]
    fn non_integral_negative_zero_and_overflow_counts_are_rejected() {
        let invalid_counts = [
            "-1",
            "0",
            "5.5",
            "-0.0",
            "NaN",
            "Infinity",
            "-Infinity",
            "18446744073709551616",
            "18446744073709551616.0",
            "1.8446744073709552e19",
            "1e20",
        ];

        for count in invalid_counts {
            let document = format!(
                r#"{{
                    agents: {{
                        Orchestrator: [{{
                            message: "x",
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {{
                                type: "toolCalls",
                                count: {count},
                                tools: ["bash"],
                            }}
                        }}]
                    }}
                }}"#
            );
            assert!(
                Configuration::parse(&document).is_err(),
                "count {count:?} should be rejected"
            );
        }
    }

    #[test]
    fn exact_integer_count_literals_keep_their_precision() {
        let configuration = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 9223372036854775807,
                            tools: ["bash"],
                        }
                    }]
                }
            }"#,
        );

        assert_eq!(
            policy_value(&configuration, "Orchestrator")["rules"][0]["condition"]["count"],
            json!(9223372036854775807_u64)
        );
    }

    #[test]
    fn malformed_tool_patterns_are_rejected() {
        for pattern in ["", "**", "a*b", "a b", "mcp.tool", "é"] {
            let document = format!(
                r#"{{
                    agents: {{
                        Orchestrator: [{{
                            message: "x",
                            resetTools: ["{pattern}"],
                        signalOnTools: ["*"],
                            condition: {{
                                type: "elapsedTime",
                                seconds: 1,
                            }}
                        }}]
                    }}
                }}"#
            );
            let error = Configuration::parse(&document)
                .expect_err("malformed tool pattern must be rejected");
            assert!(
                matches!(error, ConfigError::InvalidToolPattern { .. }),
                "unexpected error for {pattern:?}: {error}"
            );
        }

        let valid = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: ["task", "mcp-metals_*", "A-z0_9-"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 1,
                            tools: ["bash*"],
                        }
                    }]
                }
            }"#,
        );
        assert_eq!(valid.policy("Orchestrator").entries().len(), 1);
    }

    #[test]
    fn bare_wildcard_is_a_valid_tool_pattern() {
        let configuration = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "elapsedTime",
                            seconds: 1,
                        }
                    }]
                }
            }"#,
        );

        assert_eq!(
            policy_value(&configuration, "Orchestrator")["rules"][0]["signalOnTools"],
            json!(["*"])
        );
    }

    #[test]
    fn counted_patterns_may_not_overlap_resetting_patterns() {
        let documents = [
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: ["bash"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 1,
                            tools: ["bash*"],
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: ["mcp-one*"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "any",
                            conditions: [
                                {
                                    type: "toolCalls",
                                    count: 1,
                                    tools: ["mcp-*"],
                                }
                            ]
                        }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: ["task"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 1,
                            tools: ["bash", "task"],
                        }
                    }]
                }
            }"#,
        ];

        for document in documents {
            let error = Configuration::parse(document)
                .expect_err("overlapping counted and reset patterns must be rejected");
            assert!(
                matches!(error, ConfigError::OverlappingToolPatterns { .. }),
                "unexpected error: {error}"
            );
        }

        let disjoint = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: ["bash-*"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 1,
                            tools: ["bash"],
                        }
                    }]
                }
            }"#,
        );
        assert_eq!(disjoint.policy("Orchestrator").entries().len(), 1);
    }

    #[test]
    fn signal_on_tools_rejected_only_when_fully_subsumed_by_reset_tools() {
        fn rejected(reset_tools: &str, signal_on_tools: &str) -> ConfigError {
            let document = format!(
                r#"{{
                    agents: {{
                        Orchestrator: [{{
                            message: "x",
                            resetTools: [{reset_tools}],
                            signalOnTools: [{signal_on_tools}],
                            condition: {{
                                type: "event",
                                event: "preCompact",
                            }}
                        }}]
                    }}
                }}"#
            );
            Configuration::parse(&document)
                .expect_err("subsumed signalOnTools pattern must be rejected")
        }

        fn accepted(reset_tools: &str, signal_on_tools: &str) {
            let document = format!(
                r#"{{
                    agents: {{
                        Orchestrator: [{{
                            message: "x",
                            resetTools: [{reset_tools}],
                            signalOnTools: [{signal_on_tools}],
                            condition: {{
                                type: "event",
                                event: "preCompact",
                            }}
                        }}]
                    }}
                }}"#
            );
            let configuration = parse(&document);
            assert_eq!(configuration.policy("Orchestrator").entries().len(), 1);
        }

        // Exact/exact, dead: rejected.
        let error = rejected(r#""task""#, r#""task""#);
        assert!(
            matches!(error, ConfigError::SignalOnToolsSubsumedByReset { .. }),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("signalOnTools[0]"),
            "error should report the exact signalOnTools path: {error}"
        );

        // Exact pattern inside reset prefix: rejected.
        let error = rejected(r#""task*""#, r#""task""#);
        assert!(
            matches!(error, ConfigError::SignalOnToolsSubsumedByReset { .. }),
            "unexpected error: {error}"
        );

        // Prefix pattern inside reset prefix: rejected.
        let error = rejected(r#""task*""#, r#""task-runner*""#);
        assert!(
            matches!(error, ConfigError::SignalOnToolsSubsumedByReset { .. }),
            "unexpected error: {error}"
        );

        // Wildcard signal pattern matches tools beyond a narrower reset pattern: accepted.
        accepted(r#""task""#, r#""*""#);

        // Reset pattern is exact; the signal prefix also matches other tools: accepted.
        accepted(r#""task""#, r#""task*""#);

        // Disjoint patterns: accepted.
        accepted(r#""task""#, r#""bash""#);
    }

    #[test]
    fn empty_signal_on_tools_is_legal_and_never_signals() {
        let configuration = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: [],
                        condition: {
                            type: "elapsedTime",
                            seconds: 0.001,
                        }
                    }]
                }
            }"#,
        );

        assert_eq!(
            policy_value(&configuration, "Orchestrator")["rules"][0]["signalOnTools"],
            json!([])
        );
    }

    #[test]
    fn composite_conditions_are_parsed_recursively() {
        let configuration = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "all",
                            conditions: [
                                {
                                    type: "toolCalls",
                                    count: 2,
                                    tools: ["bash"],
                                },
                                {
                                    type: "any",
                                    conditions: [
                                        {
                                            type: "elapsedTime",
                                            seconds: 0.25,
                                        },
                                        {
                                            type: "event",
                                            event: "preCompact",
                                        }
                                    ]
                                }
                            ]
                        }
                    }]
                }
            }"#,
        );

        assert_eq!(
            policy_value(&configuration, "Orchestrator")["rules"][0]["condition"],
            json!({
                "type": "all",
                "conditions": [
                    {
                        "type": "toolCalls",
                        "count": 2,
                        "tools": ["bash"]
                    },
                    {
                        "type": "any",
                        "conditions": [
                            {
                                "type": "elapsedTime",
                                "seconds": 0.25
                            },
                            {
                                "type": "event",
                                "event": "preCompact"
                            }
                        ]
                    }
                ]
            })
        );
    }

    #[test]
    fn unknown_condition_types_and_events_are_rejected() {
        let documents = [
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: { type: "unknown" }
                    }]
                }
            }"#,
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "x",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {
                            type: "event",
                            event: "postCompact",
                        }
                    }]
                }
            }"#,
        ];

        for document in documents {
            assert!(
                Configuration::parse(document).is_err(),
                "document should be rejected: {document}"
            );
        }
    }

    #[test]
    fn context_conditions_report_their_unsupported_capability() {
        let documents = [
            (
                r#"{
                    agents: {
                        Orchestrator: [{
                            message: "x",
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {
                                type: "any",
                                conditions: [{ type: "contextGrowth", tokens: 20000 }]
                            }
                        }]
                    }
                }"#,
                "contextGrowth",
            ),
            (
                r#"{
                    agents: {
                        Orchestrator: [{
                            message: "x",
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {
                                type: "contextUtilizationGrowth",
                                percentagePoints: 5
                            }
                        }]
                    }
                }"#,
                "contextUtilizationGrowth",
            ),
        ];

        for (document, condition) in documents {
            let error = Configuration::parse(document)
                .expect_err("unsupported context condition must be rejected");
            let message = error.to_string();
            assert!(
                matches!(error, ConfigError::UnsupportedCondition { .. }),
                "unexpected error kind: {error}"
            );
            assert!(
                message.contains(condition) && message.contains("not supported"),
                "error must identify the unsupported capability: {message}"
            );
        }
    }

    fn nested_composite_document(depth: usize) -> String {
        const PREFIX: &str = r#"{ type: "all", conditions: ["#;
        const SUFFIX: &str = "] }";

        let mut condition = String::with_capacity((PREFIX.len() + SUFFIX.len()) * depth + 64);
        for _ in 0..depth {
            condition.push_str(PREFIX);
        }
        condition.push_str(r#"{ type: "event", event: "preCompact" }"#);
        for _ in 0..depth {
            condition.push_str(SUFFIX);
        }

        format!(
            r#"{{ agents: {{ Orchestrator: [{{ message: "x", resetTools: [], signalOnTools: ["*"], condition: {condition} }}] }} }}"#
        )
    }

    #[test]
    fn deeply_nested_valid_composites_are_rejected_without_deserializing() {
        let document = nested_composite_document(6_000);

        let error = Configuration::parse(&document)
            .expect_err("nested composites beyond the limit must be rejected");

        assert!(matches!(
            error,
            ConfigError::NestingLimitExceeded { limit } if limit == MAX_NESTING_DEPTH
        ));
        let message = error.to_string();
        assert!(
            message.contains("nesting") && message.contains(&MAX_NESTING_DEPTH.to_string()),
            "error must identify the nesting limit: {message}"
        );
    }

    #[test]
    fn nesting_guard_accepts_the_limit_and_rejects_one_more_container() {
        let at_limit = format!(
            "{}{}",
            "[".repeat(MAX_NESTING_DEPTH),
            "]".repeat(MAX_NESTING_DEPTH)
        );
        ensure_nesting_within_limit(&at_limit)
            .expect("nesting at the supported maximum must be accepted");

        let over_limit = format!(
            "{}{}",
            "[".repeat(MAX_NESTING_DEPTH + 1),
            "]".repeat(MAX_NESTING_DEPTH + 1)
        );
        let error = ensure_nesting_within_limit(&over_limit)
            .expect_err("nesting one level beyond the maximum must be rejected");
        assert!(matches!(
            error,
            ConfigError::NestingLimitExceeded { limit } if limit == MAX_NESTING_DEPTH
        ));
    }

    #[test]
    fn valid_configurations_near_the_nesting_limit_are_parsed_or_rejected_cleanly() {
        // A document contributes five container levels before the condition,
        // and each composite adds an object and an array. Sixty-one wrappers
        // therefore place the innermost condition at depth 127.
        let configuration = parse(&nested_composite_document(61));
        assert_eq!(configuration.policy("Orchestrator").entries().len(), 1);

        let error = Configuration::parse(&nested_composite_document(62))
            .expect_err("a document one level beyond the maximum must be rejected");
        assert!(matches!(
            error,
            ConfigError::NestingLimitExceeded { limit } if limit == MAX_NESTING_DEPTH
        ));
    }

    #[test]
    fn brackets_inside_strings_and_comments_do_not_count_toward_nesting() {
        let brackets = "{[".repeat(3_000);
        let document = format!(
            r#"{{
                // line comment with brackets: {brackets}
                /* block comment with brackets: {brackets}
                   and a line comment marker that must not matter: //
                */
                agents: {{
                    Orchestrator: [{{
                        message: "{brackets}",
                        resetTools: [],
                        signalOnTools: ["*"],
                        condition: {{
                            type: "elapsedTime",
                            seconds: 1,
                        }},
                    }}],
                }},
            }}"#
        );

        let configuration = parse(&document);
        assert_eq!(
            configuration.policy("Orchestrator").entries()[0].message(),
            brackets
        );
    }

    #[test]
    fn escaped_quotes_and_comment_markers_inside_strings_are_not_structure() {
        let brackets = "{[".repeat(3_000);
        let document = format!(
            r#"{{
                // line comment with quotes: "double" 'single' \" \' // and {brackets}
                /* block comment with quotes: "double" 'single' \" \'
                   and markers: // /* {brackets}
                */
                agents: {{
                    Orchestrator: [
                        {{
                            message: "escaped quote: \" and backslash: \\ and brackets: {brackets}",
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {{
                                type: "elapsedTime",
                                seconds: 1,
                            }}
                        }},
                        {{
                            message: 'single quote: \' and markers // /* and brackets: {brackets}',
                            resetTools: [],
                        signalOnTools: ["*"],
                            condition: {{
                                type: "elapsedTime",
                                seconds: 1,
                            }}
                        }}
                    ]
                }}
            }}"#
        );

        let configuration = parse(&document);
        let policy = configuration.policy("Orchestrator");
        let entries = policy.entries();
        assert_eq!(
            entries[0].message(),
            format!("escaped quote: \" and backslash: \\ and brackets: {brackets}")
        );
        assert_eq!(
            entries[1].message(),
            format!("single quote: ' and markers // /* and brackets: {brackets}")
        );
    }

    #[test]
    fn unicode_line_separators_end_line_comments() {
        let document = format!(
            "// brackets before U+2028: {}\u{2028}{}",
            "[".repeat(MAX_NESTING_DEPTH + 1),
            nested_composite_document(62)
        );

        let error = Configuration::parse(&document)
            .expect_err("containers after a Unicode line separator must count toward nesting");
        assert!(matches!(error, ConfigError::NestingLimitExceeded { .. }));
    }

    #[test]
    fn fingerprint_is_stable_and_changes_with_the_policy() {
        let first = parse(MINIMAL).policy("Orchestrator");
        let second = parse(MINIMAL).policy("Orchestrator");
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(first.fingerprint().len(), 64);
        assert!(
            first
                .fingerprint()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );

        let changed_message = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "A different reminder.",
                        resetTools: ["task"],
                        signalOnTools: ["*"],
                        condition: {
                            type: "toolCalls",
                            count: 5,
                            tools: ["bash", "powershell"],
                        }
                    }]
                }
            }"#,
        )
        .policy("Orchestrator");
        assert_ne!(first.fingerprint(), changed_message.fingerprint());
        assert_ne!(
            first.fingerprint(),
            parse(MINIMAL).policy("Reviewer").fingerprint()
        );

        let changed_signal_on_tools = parse(
            r#"{
                agents: {
                    Orchestrator: [{
                        message: "Remember your prompt obligations.",
                        resetTools: ["task"],
                        signalOnTools: ["bash"],
                        condition: {
                            type: "toolCalls",
                            count: 5,
                            tools: ["bash", "powershell"],
                        }
                    }]
                }
            }"#,
        )
        .policy("Orchestrator");
        assert_ne!(
            first.fingerprint(),
            changed_signal_on_tools.fingerprint(),
            "a different signalOnTools set must change the fingerprint"
        );
    }
}
