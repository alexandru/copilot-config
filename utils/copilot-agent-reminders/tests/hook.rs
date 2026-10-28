//! Black-box integration tests for the `copilot-agent-reminders` command hook.
//!
//! Every test drives the built executable through its real stdin/stdout
//! protocol and temporary configuration, state, and home directories. No test
//! reads or writes the developer's actual `$HOME`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

const AGENT: &str = "test-agent";
const MESSAGE: &str = "Remember your prompt obligations.";
const SHIPPED_MESSAGE: &str =
    "Soft block: Remember your prompt, you MUST delegate grunt work for efficiency!";
const SESSION: &str = "session-1";
const STATE_FILE_NAME: &str = "agent-reminders-state.json";
const LOCK_FILE_NAME: &str = "agent-reminders-state.lock";
/// A numeric Unix-epoch millisecond timestamp in the hook wire format.
///
/// These black-box tests build JSON payloads for the built executable, so they
/// cannot import the crate-private `TimestampMillis` newtype and must keep the
/// wire representation numeric. This alias marks timestamp-bearing fixtures as
/// semantic times without duplicating or vendoring the production type.
type WireTimestampMillis = u64;

const BASE_TIME: WireTimestampMillis = 1_700_000_000_000;

/// Temporary directory layout shared by one test.
struct TestEnvironment {
    directory: TempDir,
    config: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl TestEnvironment {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create temporary test directory");
        let home = directory.path().join("home");
        fs::create_dir_all(&home).expect("create temporary home directory");

        Self {
            config: directory.path().join("agent-reminders.jsonc"),
            state: directory.path().join("state"),
            home,
            directory,
        }
    }

    /// Writes a one-rule configuration for [`AGENT`].
    fn write_rule(&self, message: &str, reset_tools: &[&str], condition: Value) {
        self.write_rule_with_signal(message, reset_tools, &["*"], condition);
    }

    fn write_rule_with_signal(
        &self,
        message: &str,
        reset_tools: &[&str],
        signal_on_tools: &[&str],
        condition: Value,
    ) {
        self.write_rule_at(
            &self.config,
            message,
            reset_tools,
            signal_on_tools,
            condition,
        );
    }

    fn write_rule_at(
        &self,
        path: &Path,
        message: &str,
        reset_tools: &[&str],
        signal_on_tools: &[&str],
        condition: Value,
    ) {
        self.write_config_at(
            path,
            json!({ (AGENT): [{
            "message": message,
            "resetTools": reset_tools,
            "signalOnTools": signal_on_tools,
            "condition": condition,
        }] }),
        );
    }

    fn write_config_at(&self, path: &Path, agents: Value) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create configuration directory");
        }
        let configuration = json!({ "agents": agents });
        fs::write(
            path,
            serde_json::to_vec_pretty(&configuration).expect("serialize test configuration"),
        )
        .expect("write test configuration");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_copilot-agent-reminders"));
        command.env("HOME", &self.home).env_remove("COPILOT_HOME");
        command
    }

    fn event_args(&self, event: &str) -> Vec<OsString> {
        self.event_args_for_agent(AGENT, event)
    }

    fn event_args_for_agent(&self, agent: &str, event: &str) -> Vec<OsString> {
        vec![
            OsString::from("--agent"),
            OsString::from(agent),
            OsString::from("--event"),
            OsString::from(event),
            OsString::from("--config"),
            self.config.clone().into_os_string(),
            OsString::from("--state-dir"),
            self.state.clone().into_os_string(),
        ]
    }

    fn event_args_with_config(&self, agent: &str, event: &str, config: &Path) -> Vec<OsString> {
        vec![
            OsString::from("--agent"),
            OsString::from(agent),
            OsString::from("--event"),
            OsString::from(event),
            OsString::from("--config"),
            config.as_os_str().to_owned(),
            OsString::from("--state-dir"),
            self.state.clone().into_os_string(),
        ]
    }

    fn default_event_args(&self, event: &str) -> Vec<OsString> {
        vec![
            OsString::from("--agent"),
            OsString::from(AGENT),
            OsString::from("--event"),
            OsString::from(event),
        ]
    }

    fn config_only_args(&self, event: &str) -> Vec<OsString> {
        vec![
            OsString::from("--agent"),
            OsString::from(AGENT),
            OsString::from("--event"),
            OsString::from(event),
            OsString::from("--config"),
            self.config.clone().into_os_string(),
        ]
    }

    fn run_event(&self, event: &str, payload: &[u8]) -> Output {
        self.run_args(&self.event_args(event), payload, &[])
    }

    fn run_args(
        &self,
        arguments: &[OsString],
        stdin: &[u8],
        environment: &[(&str, &OsStr)],
    ) -> Output {
        self.run_args_in(self.directory.path(), arguments, stdin, environment, &[])
    }

    fn run_args_in(
        &self,
        working_directory: &Path,
        arguments: &[OsString],
        stdin: &[u8],
        environment: &[(&str, &OsStr)],
        removed_environment: &[&str],
    ) -> Output {
        let mut command = self.command();
        command
            .current_dir(working_directory)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }
        for name in removed_environment {
            command.env_remove(name);
        }

        let mut child = command.spawn().expect("spawn copilot-agent-reminders");
        if let Some(mut pipe) = child.stdin.take() {
            // The adapter reads stdin before validation, so this write normally
            // succeeds even when arguments or configuration are invalid. It can
            // still fail when the child stops at the bounded-read limit.
            let _ = pipe.write_all(stdin);
        }
        child
            .wait_with_output()
            .expect("wait for copilot-agent-reminders")
    }

    /// Runs the executable and requires the child to consume the whole payload.
    ///
    /// Unlike [`Self::run_args`], a broken pipe fails the test. This is the
    /// regression seam for early argument and configuration failures.
    fn run_args_expecting_stdin_read(&self, arguments: &[OsString], stdin: &[u8]) -> Output {
        let mut command = self.command();
        command
            .current_dir(self.directory.path())
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().expect("spawn copilot-agent-reminders");
        let mut pipe = child.stdin.take().expect("child stdin");
        pipe.write_all(stdin)
            .expect("copilot-agent-reminders must consume stdin before exiting");
        drop(pipe);
        child
            .wait_with_output()
            .expect("wait for copilot-agent-reminders")
    }

    fn spawn_event(&self, event: &str, payload: &[u8]) -> (Child, Vec<u8>) {
        let arguments = self.event_args(event);
        let mut command = self.command();
        command
            .args(&arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = command.spawn().expect("spawn copilot-agent-reminders");
        (child, payload.to_vec())
    }

    fn pre(&self, session: &str, tool: &str, timestamp: WireTimestampMillis) -> Output {
        self.run_event("preToolUse", &payload(session, timestamp, Some(tool)))
    }

    fn compact(&self, session: &str, timestamp: WireTimestampMillis) -> Output {
        self.run_event("preCompact", &payload(session, timestamp, None))
    }

    fn session_start(&self, session: &str, timestamp: WireTimestampMillis) -> Output {
        self.run_event("sessionStart", &payload(session, timestamp, None))
    }

    fn state_file(&self, session: &str) -> PathBuf {
        self.state.join(session).join(STATE_FILE_NAME)
    }

    fn home_state_file(&self, session: &str) -> PathBuf {
        self.home
            .join(".agents/sessions/copilot")
            .join(session)
            .join(STATE_FILE_NAME)
    }

    fn lock_file(&self, session: &str) -> PathBuf {
        self.state.join(session).join(LOCK_FILE_NAME)
    }
}

fn payload(session: &str, timestamp: WireTimestampMillis, tool: Option<&str>) -> Vec<u8> {
    let mut value = json!({
        "sessionId": session,
        "timestamp": timestamp,
        "cwd": "/tmp/project",
        "toolArgs": {"command": "true"},
        "futureField": {"nested": [1, 2, 3]},
    });
    if let Some(tool) = tool {
        value["toolName"] = Value::String(tool.to_owned());
    }
    serde_json::to_vec(&value).expect("serialize test payload")
}

/// A normal-size payload whose ignored tool arguments exceed pipe capacity.
fn large_payload(session: &str, timestamp: WireTimestampMillis) -> Vec<u8> {
    let value = json!({
        "sessionId": session,
        "timestamp": timestamp,
        "cwd": "/tmp/project",
        "toolName": "bash",
        "toolArgs": {
            "command": "true",
            "ignoredPadding": "x".repeat(512 * 1024),
        },
    });
    serde_json::to_vec(&value).expect("serialize large test payload")
}

/// Payload shape delivered by Copilot's PascalCase registration aliases.
fn pascal_case_payload(session: &str, tool: &str) -> Vec<u8> {
    let value = json!({
        "session_id": session,
        "timestamp": "2024-01-01T00:00:00.000Z",
        "tool_name": tool,
        "cwd": "/tmp/project",
    });
    serde_json::to_vec(&value).expect("serialize PascalCase test payload")
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_continue(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected success; stderr: {}",
        stderr_text(output)
    );
    assert_eq!(stdout_text(output), "{}\n");
}

fn assert_single_delivery_log(output: &Output, message_count: usize) {
    let stderr = stderr_text(output);
    assert_eq!(
        stderr.lines().count(),
        1,
        "a denial must add exactly one stderr record: {stderr}"
    );
    assert!(
        stderr.starts_with("copilot-agent-reminders: info: denied "),
        "delivery must be logged as a denial: {stderr}"
    );
    for field in ["sessionId=", "agent=", "timestamp="] {
        assert!(
            stderr.contains(field),
            "delivery record must include {field}: {stderr}"
        );
    }
    assert!(
        stderr.contains("event=preToolUse"),
        "delivery record must include event=preToolUse: {stderr}"
    );
    assert!(
        stderr.contains(&format!("messages={message_count}")),
        "delivery record must include the message count: {stderr}"
    );
}

/// Asserts that `output` is a soft deny carrying exactly `message` as its
/// reason, with exit code 0 and a transport-only delivery log line.
fn assert_denies(output: &Output, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "a preToolUse denial must still exit 0; stderr: {}",
        stderr_text(output)
    );
    let text = stdout_text(output);
    let value: Value = serde_json::from_str(text.trim()).expect("stdout is hook JSON");
    let object = value.as_object().expect("hook output is a JSON object");
    assert_eq!(object.len(), 2, "deny output has exactly two fields");
    assert_eq!(
        object.get("permissionDecision").and_then(Value::as_str),
        Some("deny")
    );
    assert_eq!(
        object
            .get("permissionDecisionReason")
            .and_then(Value::as_str),
        Some(message)
    );
    assert_single_delivery_log(output, 1);
    assert!(
        !stderr_text(output).contains(message),
        "delivery record must not include reminder text: {}",
        stderr_text(output)
    );
}

fn tool_calls(count: u64, tools: &[&str]) -> Value {
    json!({
        "type": "toolCalls",
        "count": count,
        "tools": tools,
    })
}

fn elapsed_time(seconds: f64) -> Value {
    json!({"type": "elapsedTime", "seconds": seconds})
}

fn pre_compact_event() -> Value {
    json!({"type": "event", "event": "preCompact"})
}

#[test]
fn the_fifth_call_denies_and_retrying_the_same_tool_immediately_succeeds() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }

    let fifth = BASE_TIME + 4000;
    assert_denies(&environment.pre(SESSION, "bash", fifth), MESSAGE);

    // The denial fully reset the interval, so an immediate retry succeeds.
    assert_continue(&environment.pre(SESSION, "bash", fifth + 1));

    for index in 1..4 {
        let timestamp = fifth + 1 + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }
    let tenth = fifth + 1 + 4000;
    assert_denies(&environment.pre(SESSION, "bash", tenth), MESSAGE);
}

#[test]
fn reset_tool_resets_an_interval_without_being_denied() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &["task"], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }

    assert_continue(&environment.pre(SESSION, "task", BASE_TIME + 10_000));

    for index in 0..4 {
        let timestamp = BASE_TIME + 20_000 + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }
    assert_denies(
        &environment.pre(SESSION, "bash", BASE_TIME + 24_000),
        MESSAGE,
    );
}

#[test]
fn emitted_reminder_logs_transport_metadata_without_payload_text() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let tool_args_marker = "tool-args-marker-64b2";
    let prompt_marker = "prompt-marker-64b2";
    let input = json!({
        "sessionId": SESSION,
        "timestamp": BASE_TIME,
        "toolName": "bash",
        "toolArgs": {"command": tool_args_marker},
        "prompt": prompt_marker,
    });
    let output = environment.run_event(
        "preToolUse",
        &serde_json::to_vec(&input).expect("serialize payload"),
    );

    assert_denies(&output, MESSAGE);
    let stderr = stderr_text(&output);
    assert_eq!(
        stderr,
        format!(
            "copilot-agent-reminders: info: denied sessionId=\"{SESSION}\" agent=\"{AGENT}\" \
             timestamp={BASE_TIME} event=preToolUse messages=1\n"
        )
    );
    for marker in [tool_args_marker, prompt_marker] {
        assert!(
            !stderr.contains(marker),
            "delivery log must omit payload data: {stderr}"
        );
    }
}

#[test]
fn pre_compact_latches_and_denies_the_next_tool_call() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], pre_compact_event());

    assert_continue(&environment.compact(SESSION, BASE_TIME));
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 1000), MESSAGE);

    // The denial reset the latch, so the next call is unaffected.
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 2000));
}

#[test]
fn sessions_have_independent_intervals() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(2, &["bash"]));

    assert_continue(&environment.pre("session-a", "bash", BASE_TIME));
    assert_continue(&environment.pre("session-b", "bash", BASE_TIME + 1000));

    assert_denies(
        &environment.pre("session-a", "bash", BASE_TIME + 2000),
        MESSAGE,
    );
    assert_denies(
        &environment.pre("session-b", "bash", BASE_TIME + 3000),
        MESSAGE,
    );
}

#[test]
fn session_start_preserves_persisted_state_on_resume() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }

    assert_continue(&environment.session_start(SESSION, BASE_TIME + 10_000));
    assert_continue(&environment.session_start("fresh-session", BASE_TIME + 10_000));
    assert!(environment.state_file("fresh-session").is_file());

    assert_denies(
        &environment.pre(SESSION, "bash", BASE_TIME + 11_000),
        MESSAGE,
    );
}

#[test]
fn all_composites_require_every_child_condition() {
    let environment = TestEnvironment::new();
    environment.write_rule(
        MESSAGE,
        &[],
        json!({
            "type": "all",
            "conditions": [
                tool_calls(2, &["bash"]),
                elapsed_time(1.0),
            ],
        }),
    );

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 100));

    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 2000), MESSAGE);
}

#[test]
fn configured_elapsed_time_rule_denies_at_its_threshold_and_repeats() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], elapsed_time(1.0));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 999));
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 1000), MESSAGE);

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 1999));
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 2000), MESSAGE);
}

#[test]
fn session_start_with_changed_policy_resets_the_elapsed_baseline() {
    let environment = TestEnvironment::new();
    environment.write_rule("Old policy.", &[], elapsed_time(60.0));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));

    let changed_message = "Changed policy.";
    environment.write_rule(changed_message, &[], elapsed_time(1.0));

    assert_continue(&environment.session_start(SESSION, BASE_TIME + 10_000));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 10_999));
    assert_denies(
        &environment.pre(SESSION, "bash", BASE_TIME + 11_000),
        changed_message,
    );
}

#[test]
fn session_start_with_unchanged_policy_preserves_the_elapsed_baseline() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], elapsed_time(2.0));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));

    let resumed = environment.session_start(SESSION, BASE_TIME + 1500);
    assert_continue(&resumed);
    assert!(
        !stderr_text(&resumed).contains("warning"),
        "resuming with the same policy must not reset state: {}",
        stderr_text(&resumed)
    );

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 1999));
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 2000), MESSAGE);
}

#[test]
fn corrupt_state_is_reported_once_and_replaced() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    let session_directory = environment.state.join(SESSION);
    fs::create_dir_all(&session_directory).expect("create corrupt state directory");
    let state_file = environment.state_file(SESSION);
    fs::write(&state_file, b"this is not serialized state").expect("write corrupt state");

    let first_pre = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&first_pre);
    assert_eq!(
        stderr_text(&first_pre).lines().count(),
        1,
        "exactly one state warning is expected"
    );
    assert!(stderr_text(&first_pre).contains("state"));

    let recovered = fs::read_to_string(&state_file).expect("read recovered state");
    serde_json::from_str::<Value>(&recovered).expect("recovered state is valid JSON");

    for index in 1..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 4000), MESSAGE);
}

#[test]
fn session_start_repairs_unsupported_schema_state_with_one_warning() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], elapsed_time(2.0));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));

    let state_file = environment.state_file(SESSION);
    let mut state: Value =
        serde_json::from_slice(&fs::read(&state_file).expect("read persisted state"))
            .expect("persisted state is JSON");
    state["schemaVersion"] = json!(999);
    fs::write(
        &state_file,
        serde_json::to_vec(&state).expect("serialize tampered state"),
    )
    .expect("write tampered state");

    let repaired = environment.session_start(SESSION, BASE_TIME + 5000);
    assert_continue(&repaired);
    assert_eq!(
        stderr_text(&repaired).lines().count(),
        1,
        "exactly one state warning is expected"
    );
    assert!(stderr_text(&repaired).contains("state"));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 6999));
    assert_denies(&environment.pre(SESSION, "bash", BASE_TIME + 7000), MESSAGE);
}

#[test]
fn configuration_changes_start_fresh_intervals() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }

    let changed_message = "The policy changed.";
    environment.write_rule(changed_message, &[], tool_calls(2, &["bash"]));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 5000));
    assert_denies(
        &environment.pre(SESSION, "bash", BASE_TIME + 6000),
        changed_message,
    );
}

#[test]
fn concurrent_processes_share_state_without_losing_updates() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(4, &["bash"]));

    let mut children = Vec::new();
    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        children.push(
            environment.spawn_event("preToolUse", &payload(SESSION, timestamp, Some("bash"))),
        );
    }
    for (child, stdin) in &mut children {
        let mut pipe = child.stdin.take().expect("child stdin");
        pipe.write_all(stdin)
            .expect("write concurrent preToolUse payload");
    }

    let mut denials = 0;
    for (child, _) in children {
        let output = child.wait_with_output().expect("wait for preToolUse");
        assert_eq!(
            output.status.code(),
            Some(0),
            "concurrent hook failed: {}",
            stderr_text(&output)
        );
        let text = stdout_text(&output);
        if text == "{}\n" {
            continue;
        }
        let value: Value = serde_json::from_str(text.trim()).expect("stdout is hook JSON");
        assert_eq!(
            value.get("permissionDecision").and_then(Value::as_str),
            Some("deny")
        );
        assert_eq!(
            value
                .get("permissionDecisionReason")
                .and_then(Value::as_str),
            Some(MESSAGE)
        );
        denials += 1;
    }
    assert_eq!(denials, 1, "exactly one concurrent transition denies");
}

#[test]
fn missing_and_invalid_configuration_fail_open() {
    let environment = TestEnvironment::new();

    let missing = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&missing);
    assert!(!stderr_text(&missing).is_empty());

    fs::write(&environment.config, b"{ agents: [ }").expect("write invalid configuration");
    let invalid = environment.pre(SESSION, "bash", BASE_TIME + 1000);
    assert_continue(&invalid);
    assert!(stderr_text(&invalid).contains("configuration"));
}

#[test]
fn multiline_configuration_failures_report_one_line() {
    let environment = TestEnvironment::new();
    let configuration = json!({
        "agents": {
            "multi\nline": [{
                "message": "x",
                "resetTools": [],
                "signalOnTools": ["*"],
                "condition": {
                    "type": "elapsedTime",
                    "seconds": 0,
                },
            }],
        },
    });
    fs::write(
        &environment.config,
        serde_json::to_vec(&configuration).expect("serialize invalid configuration"),
    )
    .expect("write invalid configuration");

    let output = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&output);

    let stderr = stderr_text(&output);
    assert_eq!(
        stderr.lines().count(),
        1,
        "configuration diagnostics must stay on one line: {stderr}"
    );
    assert!(
        stderr.starts_with("copilot-agent-reminders: invalid configuration "),
        "configuration diagnostic must keep its prefix: {stderr}"
    );
    assert!(
        stderr.contains("positive finite number"),
        "configuration diagnostic must retain the validation cause: {stderr}"
    );
}

#[test]
fn whole_configuration_without_agents_warns_once_and_invalidates_state() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    assert!(environment.state_file(SESSION).is_file());

    environment.write_config_at(&environment.config, json!({}));
    let output = environment.pre(SESSION, "bash", BASE_TIME + 1000);
    assert_continue(&output);

    let stderr = stderr_text(&output);
    assert_eq!(
        stderr.lines().count(),
        1,
        "an empty whole configuration must warn exactly once: {stderr}"
    );
    assert!(
        stderr.starts_with("copilot-agent-reminders: warning: "),
        "empty whole configuration must warn: {stderr}"
    );
    assert!(
        stderr.contains("no configured agents") && stderr.contains("disabled"),
        "empty whole configuration must explain that reminders are disabled: {stderr}"
    );
    assert!(
        !environment.state_file(SESSION).exists(),
        "empty whole configuration must clear prior session state"
    );
    assert!(
        environment.lock_file(SESSION).is_file(),
        "clearing must retain the stable lock file"
    );

    let fresh = environment.pre("fresh-session", "bash", BASE_TIME + 2000);
    assert_continue(&fresh);
    assert_eq!(
        stderr_text(&fresh).lines().count(),
        1,
        "every invocation with an empty whole configuration warns: {}",
        stderr_text(&fresh)
    );
    assert!(
        !environment.state.join("fresh-session").exists(),
        "an empty whole configuration must not create state"
    );
}

#[test]
fn invalid_input_fails_open_for_every_supported_event() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let invalid_json = environment.run_event("preToolUse", b"{ not json");
    assert_continue(&invalid_json);
    assert!(!stderr_text(&invalid_json).is_empty());

    let missing_session = json!({"timestamp": BASE_TIME}).to_string();
    assert_continue(&environment.run_event("preToolUse", missing_session.as_bytes()));

    let wrong_timestamp = json!({"sessionId": SESSION, "timestamp": "later"}).to_string();
    assert_continue(&environment.run_event("preToolUse", wrong_timestamp.as_bytes()));

    let missing_tool = payload(SESSION, BASE_TIME, None);
    let pre_without_tool = environment.run_event("preToolUse", &missing_tool);
    assert_continue(&pre_without_tool);
    assert!(!stderr_text(&pre_without_tool).is_empty());
}

#[test]
fn invalid_session_ids_fail_open_without_creating_directories() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    for session in ["../escape", "bad/id", "with space"] {
        let output =
            environment.run_event("preToolUse", &payload(session, BASE_TIME, Some("bash")));
        assert_continue(&output);
        assert!(!stderr_text(&output).is_empty());
    }

    assert!(!environment.state.exists());
    assert!(!environment.directory.path().join("escape").exists());
}

#[test]
fn unconfigured_agent_on_a_new_session_creates_no_state() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let output = environment.run_args(
        &environment.event_args_for_agent("unconfigured-agent", "preToolUse"),
        &payload(SESSION, BASE_TIME, Some("bash")),
        &[],
    );
    assert_continue(&output);
    assert!(
        stderr_text(&output).is_empty(),
        "an unconfigured agent is not an error: {}",
        stderr_text(&output)
    );
    assert!(!environment.state.exists());
}

#[test]
fn switching_to_an_unconfigured_agent_clears_stale_state_and_keeps_the_lock() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }
    assert!(environment.state_file(SESSION).is_file());
    assert!(environment.lock_file(SESSION).is_file());

    let unknown = environment.run_args(
        &environment.event_args_for_agent("unconfigured-agent", "preToolUse"),
        &payload(SESSION, BASE_TIME + 10_000, Some("bash")),
        &[],
    );
    assert_continue(&unknown);
    assert!(
        stderr_text(&unknown).is_empty(),
        "switching to an unconfigured agent must not warn: {}",
        stderr_text(&unknown)
    );
    assert!(
        !environment.state_file(SESSION).exists(),
        "stale state JSON must be removed"
    );
    assert!(
        environment.lock_file(SESSION).is_file(),
        "the stable lock file must be retained"
    );

    let restart = BASE_TIME + 20_000;
    assert_continue(&environment.pre(SESSION, "bash", restart));
    let restarted: Value = serde_json::from_slice(
        &fs::read(environment.state_file(SESSION)).expect("read restarted state"),
    )
    .expect("restarted state is JSON");
    assert_eq!(
        restarted["intervals"][0]["counts"]["bash"],
        json!(1),
        "the configured agent must restart at the first invocation"
    );

    for index in 1..4 {
        let timestamp = restart + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
    }

    assert_denies(&environment.pre(SESSION, "bash", restart + 4000), MESSAGE);
}

#[test]
fn invalid_arguments_fail_open() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));
    let standard = environment.event_args("preToolUse");
    let input = payload(SESSION, BASE_TIME, Some("bash"));

    let mut duplicate = standard.clone();
    duplicate.extend([OsString::from("--agent"), OsString::from("other")]);
    let duplicate_output = environment.run_args(&duplicate, &input, &[]);
    assert_continue(&duplicate_output);
    assert!(!stderr_text(&duplicate_output).is_empty());

    let mut unknown = standard.clone();
    unknown.push(OsString::from("--bogus"));
    let unknown_output = environment.run_args(&unknown, &input, &[]);
    assert_continue(&unknown_output);
    assert!(!stderr_text(&unknown_output).is_empty());

    let unknown_event = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("notAnEvent"),
        OsString::from("--config"),
        environment.config.clone().into_os_string(),
        OsString::from("--state-dir"),
        environment.state.clone().into_os_string(),
    ];
    let event_output = environment.run_args(&unknown_event, &input, &[]);
    assert_continue(&event_output);
    assert!(stderr_text(&event_output).contains("event"));

    let mut missing_agent = standard;
    missing_agent.drain(0..2);
    let missing_output = environment.run_args(&missing_agent, &input, &[]);
    assert_continue(&missing_output);
    assert!(stderr_text(&missing_output).contains("--agent"));
}

#[test]
fn pascal_case_registration_payloads_fail_open_without_writing_state() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));
    let input = pascal_case_payload(SESSION, "bash");

    for event in ["preToolUse", "preCompact", "sessionStart"] {
        let output = environment.run_event(event, &input);
        assert_continue(&output);

        let stderr = stderr_text(&output);
        assert_eq!(
            stderr.lines().count(),
            1,
            "{event} must report one concise diagnostic; got: {stderr}"
        );
        assert!(
            stderr.starts_with("copilot-agent-reminders: invalid hook input JSON:"),
            "{event} must report a concise invalid-input diagnostic: {stderr}"
        );
        assert!(
            !environment.state.exists(),
            "{event} must not create state for a rejected payload"
        );
    }
}

#[test]
fn oversized_input_fails_open() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let oversized = vec![b' '; 16 * 1024 * 1024 + 1];
    let output = environment.run_event("preToolUse", &oversized);
    assert_continue(&output);
    assert!(stderr_text(&output).contains("16 MiB"));
}

#[test]
fn large_payload_is_consumed_before_configuration_or_argument_failures() {
    let environment = TestEnvironment::new();
    let input = large_payload(SESSION, BASE_TIME);

    let missing_config = environment.directory.path().join("missing.jsonc");
    let missing_output = environment.run_args_expecting_stdin_read(
        &environment.event_args_with_config(AGENT, "preToolUse", &missing_config),
        &input,
    );
    assert_continue(&missing_output);
    assert!(
        stderr_text(&missing_output).contains("configuration"),
        "missing configuration must still be diagnosed: {}",
        stderr_text(&missing_output)
    );

    let invalid_arguments = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("preToolUse"),
        OsString::from("--bogus"),
    ];
    let argument_output = environment.run_args_expecting_stdin_read(&invalid_arguments, &input);
    assert_continue(&argument_output);
    assert!(
        stderr_text(&argument_output).contains("unknown argument"),
        "invalid arguments must still take diagnostic precedence: {}",
        stderr_text(&argument_output)
    );
}

#[test]
fn state_read_errors_fail_open() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let session_directory = environment.state.join(SESSION);
    fs::create_dir_all(&session_directory).expect("create session directory");
    fs::create_dir(environment.state_file(SESSION)).expect("block the state file path");

    let output = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&output);
    assert!(!stderr_text(&output).is_empty());
}

#[test]
fn help_is_successful_and_does_not_touch_stdin_or_state() {
    let environment = TestEnvironment::new();

    let output = environment.run_args(&[OsString::from("--help")], b"", &[]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert!(text.contains("Usage:"));
    assert!(text.contains("--agent"));
    assert!(text.contains("preCompact"));
    assert!(text.contains("128 object or array levels"));
    assert!(stderr_text(&output).is_empty());
    assert!(!environment.state.exists());
    assert!(!environment.config.exists());
}

#[test]
fn help_exits_without_waiting_for_stdin() {
    let environment = TestEnvironment::new();
    let mut command = environment.command();
    command
        .arg("--help")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().expect("spawn copilot-agent-reminders");
    let _stdin = child.stdin.take().expect("child stdin");

    let mut exited = false;
    for _ in 0..100 {
        if child
            .try_wait()
            .expect("poll copilot-agent-reminders")
            .is_some()
        {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        child.kill().expect("kill blocked help process");
    }

    let output = child
        .wait_with_output()
        .expect("wait for copilot-agent-reminders");
    assert!(exited, "help must not read stdin before exiting");
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_text(&output).contains("Usage:"));
    assert!(stderr_text(&output).is_empty());
}

#[test]
fn help_documents_camel_case_registration_requirements() {
    let environment = TestEnvironment::new();

    let output = environment.run_args(&[OsString::from("--help")], b"", &[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr_text(&output).is_empty());

    let help = stdout_text(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        help.contains("MUST use camelCase event keys: preToolUse, preCompact, sessionStart"),
        "help must document the camelCase registration keys: {help}"
    );
    assert!(
        help.contains("camelCase payload fields and a numeric Unix-millisecond timestamp"),
        "help must state the payload shape delivered by camelCase registrations: {help}"
    );
    assert!(
        help.contains(
            "PascalCase aliases deliver snake_case fields and ISO-8601 timestamps \
             and are not supported"
        ),
        "help must state that PascalCase aliases are unsupported: {help}"
    );
    assert!(
        help.contains("registration key, not the --event spelling, determines the payload format"),
        "help must separate registration keys from the --event spelling: {help}"
    );
    assert!(
        help.contains("permissionDecision"),
        "help must describe the deny response shape: {help}"
    );
    assert!(
        !help.contains("additionalContext"),
        "help must not mention the retired additionalContext output: {help}"
    );
}

#[test]
fn default_paths_use_home_and_copilot_home() {
    let environment = TestEnvironment::new();

    let home_config = environment
        .home
        .join(".copilot")
        .join("agent-reminders.jsonc");
    environment.write_rule_at(
        &home_config,
        "Message from HOME.",
        &[],
        &["*"],
        tool_calls(1, &["bash"]),
    );

    let home_args = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("preToolUse"),
    ];
    assert_denies(
        &environment.run_args(&home_args, &payload(SESSION, BASE_TIME, Some("bash")), &[]),
        "Message from HOME.",
    );
    assert!(
        environment
            .home
            .join(".agents/sessions/copilot")
            .join(SESSION)
            .join(STATE_FILE_NAME)
            .is_file()
    );

    let copilot_home = environment.directory.path().join("copilot-home");
    let copilot_config = copilot_home.join("agent-reminders.jsonc");
    environment.write_rule_at(
        &copilot_config,
        "Message from COPILOT_HOME.",
        &[],
        &["*"],
        tool_calls(1, &["bash"]),
    );
    let copilot_home_args = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("preToolUse"),
    ];
    let copilot_home_env = [("COPILOT_HOME", copilot_home.as_os_str())];
    assert_denies(
        &environment.run_args(
            &copilot_home_args,
            &payload("session-2", BASE_TIME + 2, Some("bash")),
            &copilot_home_env,
        ),
        "Message from COPILOT_HOME.",
    );
}

#[test]
fn empty_copilot_home_falls_back_to_the_home_configuration() {
    let environment = TestEnvironment::new();
    let home_config = environment
        .home
        .join(".copilot")
        .join("agent-reminders.jsonc");
    environment.write_rule_at(
        &home_config,
        "Message from HOME.",
        &[],
        &["*"],
        tool_calls(1, &["bash"]),
    );

    let empty_copilot_home = [("COPILOT_HOME", OsStr::new(""))];
    let pre = environment.run_args(
        &environment.default_event_args("preToolUse"),
        &payload(SESSION, BASE_TIME, Some("bash")),
        &empty_copilot_home,
    );
    assert_denies(&pre, "Message from HOME.");
    assert!(environment.home_state_file(SESSION).is_file());
}

#[test]
fn empty_or_missing_home_fails_open_without_writing_to_the_working_directory() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let working = environment.directory.path().join("working");
    fs::create_dir(&working).expect("create working directory");
    let input = payload(SESSION, BASE_TIME, Some("bash"));

    // The explicit configuration loads, so the default state path is reached.
    // Resolving it relative to the working directory would create `.agents`.
    let empty_home = environment.run_args_in(
        &working,
        &environment.config_only_args("preToolUse"),
        &input,
        &[("HOME", OsStr::new(""))],
        &[],
    );
    assert_continue(&empty_home);
    assert!(
        stderr_text(&empty_home).contains("HOME"),
        "empty HOME must be diagnosed: {}",
        stderr_text(&empty_home)
    );

    let missing_home = environment.run_args_in(
        &working,
        &environment.config_only_args("preToolUse"),
        &input,
        &[],
        &["HOME"],
    );
    assert_continue(&missing_home);
    assert!(
        stderr_text(&missing_home).contains("HOME"),
        "missing HOME must be diagnosed: {}",
        stderr_text(&missing_home)
    );

    let default_config = environment.run_args_in(
        &working,
        &environment.default_event_args("preToolUse"),
        &input,
        &[("HOME", OsStr::new(""))],
        &[],
    );
    assert_continue(&default_config);
    assert!(stderr_text(&default_config).contains("HOME"));

    assert_eq!(
        fs::read_dir(&working)
            .expect("read working directory")
            .count(),
        0,
        "default paths must not be resolved relative to the working directory"
    );
    assert!(!environment.state.exists());
}

#[test]
fn explicit_config_and_state_paths_work_without_home() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let pre = environment.run_args_in(
        environment.directory.path(),
        &environment.event_args("preToolUse"),
        &payload(SESSION, BASE_TIME, Some("bash")),
        &[],
        &["HOME"],
    );
    assert_denies(&pre, MESSAGE);
    assert!(environment.state_file(SESSION).is_file());
}

#[cfg(unix)]
#[test]
fn non_utf8_copilot_home_paths_are_supported() {
    use std::os::unix::ffi::OsStringExt;

    let environment = TestEnvironment::new();
    let copilot_home = environment
        .directory
        .path()
        .join(OsString::from_vec(b"copilot-\xff".to_vec()));
    let copilot_config = copilot_home.join("agent-reminders.jsonc");
    environment.write_rule_at(
        &copilot_config,
        "Message from a non-UTF-8 COPILOT_HOME.",
        &[],
        &["*"],
        tool_calls(1, &["bash"]),
    );

    let non_utf8_home = [("COPILOT_HOME", copilot_home.as_os_str())];
    assert_denies(
        &environment.run_args(
            &environment.default_event_args("preToolUse"),
            &payload(SESSION, BASE_TIME, Some("bash")),
            &non_utf8_home,
        ),
        "Message from a non-UTF-8 COPILOT_HOME.",
    );
}

#[test]
fn shipped_configuration_drives_the_expected_hook_behavior() {
    let environment = TestEnvironment::new();
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent-reminders.jsonc");
    assert!(
        config.is_file(),
        "shipped harness configuration is missing at {}",
        config.display()
    );
    fs::read_to_string(&config).expect("read shipped harness configuration");

    let run = |event: &str, timestamp: WireTimestampMillis, tool: Option<&str>| {
        environment.run_args(
            &environment.event_args_with_config("Orchestrator", event, &config),
            &payload(SESSION, timestamp, tool),
            &[],
        )
    };

    let start = run("sessionStart", BASE_TIME, None);
    assert_continue(&start);
    assert!(
        stderr_text(&start).is_empty(),
        "the shipped configuration must parse cleanly: {}",
        stderr_text(&start)
    );

    for index in 0..4 {
        let timestamp = BASE_TIME + 1000 + index * 2000;
        let pre = run("preToolUse", timestamp, Some("bash"));
        assert_continue(&pre);
        assert!(
            stderr_text(&pre).is_empty(),
            "shipped configuration pre hook must not warn: {}",
            stderr_text(&pre)
        );
    }

    let fifth = BASE_TIME + 1000 + 4 * 2000;
    assert_denies(&run("preToolUse", fifth, Some("bash")), SHIPPED_MESSAGE);

    // The denial reset the interval, so an immediate retry succeeds.
    assert_continue(&run("preToolUse", fifth + 1, Some("bash")));

    let compact = run("preCompact", BASE_TIME + 20_000, None);
    assert_continue(&compact);
    assert_denies(
        &run("preToolUse", BASE_TIME + 20_001, Some("bash")),
        SHIPPED_MESSAGE,
    );

    let compact = run("preCompact", BASE_TIME + 30_000, None);
    assert_continue(&compact);
    // "task" is the configured reset tool; it clears the latch and is never
    // itself denied, even though the preCompact latch was pending.
    let reset = run("preToolUse", BASE_TIME + 30_001, Some("task"));
    assert_continue(&reset);
    assert_continue(&run("preToolUse", BASE_TIME + 30_002, Some("bash")));

    // Rule-level signalOnTools narrows preCompact delivery to shell calls.
    let compact = run("preCompact", BASE_TIME + 40_000, None);
    assert_continue(&compact);
    assert_continue(&run("preToolUse", BASE_TIME + 40_001, Some("view")));
    assert_denies(
        &run("preToolUse", BASE_TIME + 40_002, Some("bash")),
        SHIPPED_MESSAGE,
    );
}

#[test]
fn deeply_nested_configuration_is_rejected_without_state_or_stack_overflow() {
    const DEPTH: usize = 6000;

    let environment = TestEnvironment::new();
    let mut configuration = String::from("{\"agents\":{\"");
    configuration.push_str(AGENT);
    configuration.push_str(
        "\":[{\"message\":\"x\",\"resetTools\":[],\"signalOnTools\":[\"*\"],\"condition\":",
    );
    for _ in 0..DEPTH {
        configuration.push_str("{\"type\":\"any\",\"conditions\":[");
    }
    configuration.push_str("{\"type\":\"elapsedTime\",\"seconds\":1}");
    for _ in 0..DEPTH {
        configuration.push_str("]}");
    }
    configuration.push_str("}]}}");
    fs::write(&environment.config, configuration).expect("write deeply nested configuration");

    let output = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&output);

    let stderr = stderr_text(&output);
    let diagnostic = stderr.to_lowercase();
    assert!(
        diagnostic.contains("nesting") && diagnostic.contains("128"),
        "expected a configuration nesting diagnostic, got: {stderr}"
    );
    assert!(
        !environment.state.exists(),
        "rejected configuration must not create state"
    );
}

#[test]
fn validate_accepts_a_valid_configuration_and_touches_no_state() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let output = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
        ],
        b"",
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("is valid"),
        "expected a validity confirmation, got: {stdout}"
    );
    assert!(
        stdout.contains(&environment.config.display().to_string()),
        "expected the resolved path in the confirmation, got: {stdout}"
    );
    assert!(stderr_text(&output).is_empty());
    assert!(
        !environment.state.exists(),
        "--validate must not create a state directory"
    );
}

#[test]
fn validate_rejects_a_configuration_missing_a_required_field() {
    let environment = TestEnvironment::new();
    environment.write_config_at(
        &environment.config,
        json!({ (AGENT): [{
            "message": MESSAGE,
            "resetTools": [],
            "condition": tool_calls(1, &["bash"]),
        }] }),
    );

    let output = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
        ],
        b"",
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    assert!(
        stderr_text(&output).contains("missing field `signalOnTools`"),
        "expected the missing-field diagnostic, got: {}",
        stderr_text(&output)
    );
    assert!(!stdout_text(&output).contains("is valid"));
}

#[test]
fn validate_rejects_a_nonexistent_configuration_path() {
    let environment = TestEnvironment::new();
    let missing = environment.directory.path().join("missing.jsonc");

    let output = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            missing.clone().into_os_string(),
        ],
        b"",
        &[],
    );

    assert_ne!(output.status.code(), Some(0));
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains(&missing.display().to_string()),
        "expected the missing path in the diagnostic, got: {stderr}"
    );
}

#[test]
fn validate_warns_but_succeeds_on_an_empty_configuration() {
    let environment = TestEnvironment::new();
    environment.write_config_at(&environment.config, json!({}));

    let output = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
        ],
        b"",
        &[],
    );

    assert_eq!(output.status.code(), Some(0));
    let stderr = stderr_text(&output);
    assert!(
        stderr.contains("all reminders are disabled"),
        "expected the empty-configuration warning, got: {stderr}"
    );
    assert!(!stdout_text(&output).contains("is valid"));
}

#[test]
fn validate_rejects_combination_with_agent_event_or_state_dir() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let with_agent = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
            OsString::from("--agent"),
            OsString::from(AGENT),
        ],
        b"",
        &[],
    );
    assert_ne!(with_agent.status.code(), Some(0));
    assert!(!stdout_text(&with_agent).contains("is valid"));

    let with_event = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
            OsString::from("--event"),
            OsString::from("preToolUse"),
        ],
        b"",
        &[],
    );
    assert_ne!(with_event.status.code(), Some(0));

    let with_state_dir = environment.run_args(
        &[
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
            OsString::from("--state-dir"),
            environment.state.clone().into_os_string(),
        ],
        b"",
        &[],
    );
    assert_ne!(with_state_dir.status.code(), Some(0));
}

#[test]
fn validate_succeeds_with_stdin_closed() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    let mut command = environment.command();
    command
        .current_dir(environment.directory.path())
        .args([
            OsString::from("--validate"),
            OsString::from("--config"),
            environment.config.clone().into_os_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = command.spawn().expect("spawn copilot-agent-reminders");
    let output = child
        .wait_with_output()
        .expect("wait for copilot-agent-reminders");

    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_text(&output).contains("is valid"));
}

#[test]
fn invalid_configuration_on_pre_tool_use_still_fails_open() {
    let environment = TestEnvironment::new();
    fs::write(&environment.config, b"{ agents: [ }").expect("write invalid configuration");

    let output = environment.pre(SESSION, "bash", BASE_TIME);
    assert_continue(&output);
    assert!(stderr_text(&output).contains("configuration"));
}
