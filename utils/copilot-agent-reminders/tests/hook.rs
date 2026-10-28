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
const SHIPPED_MESSAGE: &str = "Remember your prompt, you should delegate grunt work for efficiency";
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
        self.write_rule_at(&self.config, message, reset_tools, condition);
    }

    fn write_rule_at(&self, path: &Path, message: &str, reset_tools: &[&str], condition: Value) {
        self.write_config_at(
            path,
            json!({ (AGENT): [{
            "message": message,
            "resetTools": reset_tools,
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
        child.wait_with_output().expect("wait for copilot-agent-reminders")
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
        child.wait_with_output().expect("wait for copilot-agent-reminders")
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

    fn post(&self, session: &str, tool: &str, timestamp: WireTimestampMillis) -> Output {
        self.run_event("postToolUse", &payload(session, timestamp, Some(tool)))
    }

    fn failure(&self, session: &str, tool: &str, timestamp: WireTimestampMillis) -> Output {
        self.run_event(
            "postToolUseFailure",
            &payload(session, timestamp, Some(tool)),
        )
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

fn assert_single_delivery_log(output: &Output, event: &str, message: &str) {
    let stderr = stderr_text(output);
    assert_eq!(
        stderr.lines().count(),
        1,
        "emitted output must add exactly one stderr record: {stderr}"
    );
    assert!(
        stderr.starts_with("copilot-agent-reminders: info: emitted "),
        "delivery must be logged as transport emission: {stderr}"
    );
    for field in ["sessionId=", "agent=", "timestamp=", "messages="] {
        assert!(
            stderr.contains(field),
            "delivery record must include {field}: {stderr}"
        );
    }
    assert!(
        stderr.contains(&format!("event={event} ")),
        "delivery record must include event={event}: {stderr}"
    );
    assert!(
        !stderr.contains(message),
        "delivery record must not include reminder text: {stderr}"
    );
}

fn assert_success_reminder(output: &Output, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected success; stderr: {}",
        stderr_text(output)
    );
    let text = stdout_text(output);
    let value: Value = serde_json::from_str(text.trim()).expect("stdout is hook JSON");
    let object = value.as_object().expect("hook output is a JSON object");
    assert_eq!(object.len(), 1, "hook output has exactly one field");
    assert_eq!(
        object.get("additionalContext").and_then(Value::as_str),
        Some(message)
    );
    assert_single_delivery_log(output, "postToolUse", message);
}

fn assert_failure_reminder(output: &Output, message: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected recovery exit code; stderr: {}",
        stderr_text(output)
    );
    assert_eq!(stdout_text(output), format!("{message}\n"));
    assert_single_delivery_log(output, "postToolUseFailure", message);
}

fn tool_calls(count: u64, tools: &[&str]) -> Value {
    json!({"type": "toolCalls", "count": count, "tools": tools})
}

#[test]
fn shell_pairs_emit_at_each_five_invocation_boundary() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..10 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));

        let result = environment.post(SESSION, "bash", timestamp + 1);
        if index == 4 || index == 9 {
            assert_success_reminder(&result, MESSAGE);
        } else {
            assert_continue(&result);
        }
    }
}

#[test]
fn reset_tool_resets_an_interval_without_waiting_for_completion() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &["task"], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
        assert_continue(&environment.post(SESSION, "bash", timestamp + 1));
    }

    assert_continue(&environment.pre(SESSION, "task", BASE_TIME + 10_000));

    for index in 0..5 {
        let timestamp = BASE_TIME + 20_000 + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
        let result = environment.post(SESSION, "bash", timestamp + 1);
        if index == 4 {
            assert_success_reminder(&result, MESSAGE);
        } else {
            assert_continue(&result);
        }
    }
}

#[test]
fn failed_tools_count_and_failure_reminders_use_exit_two() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(2, &["bash"]));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    assert_continue(&environment.failure(SESSION, "bash", BASE_TIME + 1));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 1000));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 1001),
        MESSAGE,
    );

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 2000));
    assert_continue(&environment.failure(SESSION, "bash", BASE_TIME + 2001));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 3000));
    assert_failure_reminder(
        &environment.failure(SESSION, "bash", BASE_TIME + 3001),
        MESSAGE,
    );
}

#[test]
fn emitted_reminder_logs_transport_metadata_without_payload_text() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));

    let tool_args_marker = "tool-args-marker-64b2";
    let tool_result_marker = "tool-result-marker-64b2";
    let prompt_marker = "prompt-marker-64b2";
    let input = json!({
        "sessionId": SESSION,
        "timestamp": BASE_TIME + 1,
        "toolName": "bash",
        "toolArgs": {"command": tool_args_marker},
        "toolResult": tool_result_marker,
        "prompt": prompt_marker,
    });
    let output = environment.run_event(
        "postToolUse",
        &serde_json::to_vec(&input).expect("serialize payload"),
    );

    assert_success_reminder(&output, MESSAGE);
    let stderr = stderr_text(&output);
    assert_eq!(
        stderr,
        format!(
            "copilot-agent-reminders: info: emitted sessionId=\"{SESSION}\" agent=\"{AGENT}\" \
             timestamp={} event=postToolUse messages=1\n",
            BASE_TIME + 1
        )
    );
    for marker in [tool_args_marker, tool_result_marker, prompt_marker] {
        assert!(
            !stderr.contains(marker),
            "delivery log must omit payload data: {stderr}"
        );
    }
}

#[test]
fn failed_tool_recovery_logs_delivery_metadata() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    let output = environment.failure(SESSION, "bash", BASE_TIME + 1);
    assert_failure_reminder(&output, MESSAGE);

    let stderr = stderr_text(&output);
    assert_eq!(
        stderr,
        format!(
            "copilot-agent-reminders: info: emitted sessionId=\"{SESSION}\" agent=\"{AGENT}\" \
             timestamp={} event=postToolUseFailure messages=1\n",
            BASE_TIME + 1
        )
    );
}

#[test]
fn pre_compact_latches_without_emitting_until_the_next_tool_result() {
    let environment = TestEnvironment::new();
    environment.write_rule(
        MESSAGE,
        &[],
        json!({"type": "event", "event": "preCompact"}),
    );

    assert_continue(&environment.compact(SESSION, BASE_TIME));
    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 1000));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 1001),
        MESSAGE,
    );

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 2000));
}

#[test]
fn sessions_have_independent_intervals() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(2, &["bash"]));

    assert_continue(&environment.pre("session-a", "bash", BASE_TIME));
    assert_continue(&environment.post("session-a", "bash", BASE_TIME + 1));

    assert_continue(&environment.pre("session-b", "bash", BASE_TIME + 1000));
    assert_continue(&environment.post("session-b", "bash", BASE_TIME + 1001));

    assert_continue(&environment.pre("session-a", "bash", BASE_TIME + 2000));
    assert_success_reminder(
        &environment.post("session-a", "bash", BASE_TIME + 2001),
        MESSAGE,
    );

    assert_continue(&environment.pre("session-b", "bash", BASE_TIME + 3000));
    assert_success_reminder(
        &environment.post("session-b", "bash", BASE_TIME + 3001),
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
        assert_continue(&environment.post(SESSION, "bash", timestamp + 1));
    }

    assert_continue(&environment.session_start(SESSION, BASE_TIME + 10_000));
    assert_continue(&environment.session_start("fresh-session", BASE_TIME + 10_000));
    assert!(environment.state_file("fresh-session").is_file());

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 11_000));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 11_001),
        MESSAGE,
    );
}

#[test]
fn elapsed_time_composites_require_every_child_condition() {
    let environment = TestEnvironment::new();
    environment.write_rule(
        MESSAGE,
        &[],
        json!({
            "type": "all",
            "conditions": [
                tool_calls(2, &["bash"]),
                {"type": "elapsedTime", "seconds": 1},
            ],
        }),
    );

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME));
    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 100));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 2000));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 2001),
        MESSAGE,
    );
}

#[test]
fn configured_elapsed_time_rule_emits_at_its_threshold_and_repeats() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], json!({"type": "elapsedTime", "seconds": 1}));

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME));
    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 999));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 1000),
        MESSAGE,
    );

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 1999));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 2000),
        MESSAGE,
    );
}

#[test]
fn session_start_with_changed_policy_resets_the_elapsed_baseline() {
    let environment = TestEnvironment::new();
    environment.write_rule(
        "Old policy.",
        &[],
        json!({"type": "elapsedTime", "seconds": 60}),
    );
    assert_continue(&environment.post(SESSION, "bash", BASE_TIME));

    let changed_message = "Changed policy.";
    environment.write_rule(
        changed_message,
        &[],
        json!({"type": "elapsedTime", "seconds": 1}),
    );

    assert_continue(&environment.session_start(SESSION, BASE_TIME + 10_000));

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 10_999));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 11_000),
        changed_message,
    );
}

#[test]
fn session_start_with_unchanged_policy_preserves_the_elapsed_baseline() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], json!({"type": "elapsedTime", "seconds": 2}));
    assert_continue(&environment.post(SESSION, "bash", BASE_TIME));

    let resumed = environment.session_start(SESSION, BASE_TIME + 1500);
    assert_continue(&resumed);
    assert!(
        !stderr_text(&resumed).contains("warning"),
        "resuming with the same policy must not reset state: {}",
        stderr_text(&resumed)
    );

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 1999));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 2000),
        MESSAGE,
    );
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

    let first_post = environment.post(SESSION, "bash", BASE_TIME + 1);
    assert_continue(&first_post);
    assert!(!stderr_text(&first_post).contains("warning"));

    let recovered = fs::read_to_string(&state_file).expect("read recovered state");
    serde_json::from_str::<Value>(&recovered).expect("recovered state is valid JSON");

    for index in 1..5 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
        let result = environment.post(SESSION, "bash", timestamp + 1);
        if index == 4 {
            assert_success_reminder(&result, MESSAGE);
        } else {
            assert_continue(&result);
        }
    }
}

#[test]
fn session_start_repairs_unsupported_schema_state_with_one_warning() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], json!({"type": "elapsedTime", "seconds": 2}));

    assert_continue(&environment.post(SESSION, "bash", BASE_TIME));

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

    let first_post = environment.post(SESSION, "bash", BASE_TIME + 6999);
    assert_continue(&first_post);
    assert!(!stderr_text(&first_post).contains("warning"));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 7000),
        MESSAGE,
    );
}

#[test]
fn configuration_changes_start_fresh_intervals() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(5, &["bash"]));

    for index in 0..4 {
        let timestamp = BASE_TIME + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
        assert_continue(&environment.post(SESSION, "bash", timestamp + 1));
    }

    let changed_message = "The policy changed.";
    environment.write_rule(changed_message, &[], tool_calls(2, &["bash"]));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 5000));
    assert_continue(&environment.post(SESSION, "bash", BASE_TIME + 5001));

    assert_continue(&environment.pre(SESSION, "bash", BASE_TIME + 6000));
    assert_success_reminder(
        &environment.post(SESSION, "bash", BASE_TIME + 6001),
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

    for (child, _) in children {
        let output = child.wait_with_output().expect("wait for preToolUse");
        assert_continue(&output);
    }

    let mut children = Vec::new();
    for index in 0..4 {
        let timestamp = BASE_TIME + 5000 + index * 1000;
        children.push(
            environment.spawn_event("postToolUse", &payload(SESSION, timestamp, Some("bash"))),
        );
    }
    for (child, stdin) in &mut children {
        let mut pipe = child.stdin.take().expect("child stdin");
        pipe.write_all(stdin)
            .expect("write concurrent postToolUse payload");
    }

    let mut reminders = 0;
    for (child, _) in children {
        let output = child.wait_with_output().expect("wait for postToolUse");
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
            value.get("additionalContext").and_then(Value::as_str),
            Some(MESSAGE)
        );
        reminders += 1;
    }
    assert_eq!(reminders, 1, "exactly one concurrent transition emits");
}

#[test]
fn missing_and_invalid_configuration_fail_open() {
    let environment = TestEnvironment::new();

    let missing = environment.post(SESSION, "bash", BASE_TIME);
    assert_continue(&missing);
    assert!(!stderr_text(&missing).is_empty());

    fs::write(&environment.config, b"{ agents: [ }").expect("write invalid configuration");
    let invalid = environment.post(SESSION, "bash", BASE_TIME + 1000);
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
                "condition": {"type": "elapsedTime", "seconds": 0},
            }],
        },
    });
    fs::write(
        &environment.config,
        serde_json::to_vec(&configuration).expect("serialize invalid configuration"),
    )
    .expect("write invalid configuration");

    let output = environment.post(SESSION, "bash", BASE_TIME);
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
    let output = environment.post(SESSION, "bash", BASE_TIME + 1000);
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

    let fresh = environment.post("fresh-session", "bash", BASE_TIME + 2000);
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

    let invalid_json = environment.run_event("postToolUse", b"{ not json");
    assert_continue(&invalid_json);
    assert!(!stderr_text(&invalid_json).is_empty());

    let missing_session = json!({"timestamp": BASE_TIME}).to_string();
    assert_continue(&environment.run_event("preToolUse", missing_session.as_bytes()));

    let wrong_timestamp = json!({"sessionId": SESSION, "timestamp": "later"}).to_string();
    assert_continue(&environment.run_event("postToolUse", wrong_timestamp.as_bytes()));

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
        &environment.event_args_for_agent("unconfigured-agent", "postToolUse"),
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
        assert_continue(&environment.post(SESSION, "bash", timestamp + 1));
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
    assert_continue(&environment.post(SESSION, "bash", restart + 1));

    for index in 1..4 {
        let timestamp = restart + index * 1000;
        assert_continue(&environment.pre(SESSION, "bash", timestamp));
        assert_continue(&environment.post(SESSION, "bash", timestamp + 1));
    }

    let timestamp = restart + 4000;
    assert_continue(&environment.pre(SESSION, "bash", timestamp));
    assert_success_reminder(&environment.post(SESSION, "bash", timestamp + 1), MESSAGE);
}

#[test]
fn invalid_arguments_fail_open() {
    let environment = TestEnvironment::new();
    environment.write_rule(MESSAGE, &[], tool_calls(1, &["bash"]));
    let standard = environment.event_args("postToolUse");
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

    for event in [
        "preToolUse",
        "postToolUse",
        "postToolUseFailure",
        "preCompact",
        "sessionStart",
    ] {
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
    let output = environment.run_event("postToolUse", &oversized);
    assert_continue(&output);
    assert!(stderr_text(&output).contains("16 MiB"));
}

#[test]
fn large_payload_is_consumed_before_configuration_or_argument_failures() {
    let environment = TestEnvironment::new();
    let input = large_payload(SESSION, BASE_TIME);

    let missing_config = environment.directory.path().join("missing.jsonc");
    let missing_output = environment.run_args_expecting_stdin_read(
        &environment.event_args_with_config(AGENT, "postToolUse", &missing_config),
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
        OsString::from("postToolUse"),
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
        if child.try_wait().expect("poll copilot-agent-reminders").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        child.kill().expect("kill blocked help process");
    }

    let output = child.wait_with_output().expect("wait for copilot-agent-reminders");
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
        help.contains(
            "MUST use camelCase event keys: preToolUse, postToolUse, \
             postToolUseFailure, preCompact, sessionStart"
        ),
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
        tool_calls(1, &["bash"]),
    );

    let home_args = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("preToolUse"),
    ];
    assert_continue(&environment.run_args(
        &home_args,
        &payload(SESSION, BASE_TIME, Some("bash")),
        &[],
    ));

    let home_post = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("postToolUse"),
    ];
    assert_success_reminder(
        &environment.run_args(
            &home_post,
            &payload(SESSION, BASE_TIME + 1, Some("bash")),
            &[],
        ),
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
        tool_calls(1, &["bash"]),
    );
    let copilot_home_args = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("preToolUse"),
    ];
    let copilot_home_env = [("COPILOT_HOME", copilot_home.as_os_str())];
    assert_continue(&environment.run_args(
        &copilot_home_args,
        &payload("session-2", BASE_TIME + 2, Some("bash")),
        &copilot_home_env,
    ));

    let copilot_home_args = vec![
        OsString::from("--agent"),
        OsString::from(AGENT),
        OsString::from("--event"),
        OsString::from("postToolUse"),
    ];
    assert_success_reminder(
        &environment.run_args(
            &copilot_home_args,
            &payload("session-2", BASE_TIME + 3, Some("bash")),
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
        tool_calls(1, &["bash"]),
    );

    let empty_copilot_home = [("COPILOT_HOME", OsStr::new(""))];
    let pre = environment.run_args(
        &environment.default_event_args("preToolUse"),
        &payload(SESSION, BASE_TIME, Some("bash")),
        &empty_copilot_home,
    );
    assert_continue(&pre);
    assert!(
        stderr_text(&pre).is_empty(),
        "empty COPILOT_HOME must fall back silently: {}",
        stderr_text(&pre)
    );

    let post = environment.run_args(
        &environment.default_event_args("postToolUse"),
        &payload(SESSION, BASE_TIME + 1, Some("bash")),
        &empty_copilot_home,
    );
    assert_success_reminder(&post, "Message from HOME.");
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
        &environment.config_only_args("postToolUse"),
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
        &environment.config_only_args("postToolUse"),
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
        &environment.default_event_args("postToolUse"),
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
    assert_continue(&pre);
    assert!(
        stderr_text(&pre).is_empty(),
        "explicit paths must not require HOME: {}",
        stderr_text(&pre)
    );

    let post = environment.run_args_in(
        environment.directory.path(),
        &environment.event_args("postToolUse"),
        &payload(SESSION, BASE_TIME + 1, Some("bash")),
        &[],
        &["HOME"],
    );
    assert_success_reminder(&post, MESSAGE);
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
        tool_calls(1, &["bash"]),
    );

    let non_utf8_home = [("COPILOT_HOME", copilot_home.as_os_str())];
    assert_continue(&environment.run_args(
        &environment.default_event_args("preToolUse"),
        &payload(SESSION, BASE_TIME, Some("bash")),
        &non_utf8_home,
    ));
    assert_success_reminder(
        &environment.run_args(
            &environment.default_event_args("postToolUse"),
            &payload(SESSION, BASE_TIME + 1, Some("bash")),
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

    for index in 0..5 {
        let timestamp = BASE_TIME + 1000 + index * 2000;
        let pre = run("preToolUse", timestamp, Some("bash"));
        assert_continue(&pre);
        assert!(
            stderr_text(&pre).is_empty(),
            "shipped configuration pre hook must not warn: {}",
            stderr_text(&pre)
        );

        let post = run("postToolUse", timestamp + 1, Some("bash"));
        if index == 4 {
            assert_success_reminder(&post, SHIPPED_MESSAGE);
        } else {
            assert_continue(&post);
        }
    }

    let compact = run("preCompact", BASE_TIME + 20_000, None);
    assert_continue(&compact);
    let after_compact = run("postToolUse", BASE_TIME + 20_001, Some("bash"));
    assert_success_reminder(&after_compact, SHIPPED_MESSAGE);

    let compact = run("preCompact", BASE_TIME + 30_000, None);
    assert_continue(&compact);
    let reset = run("preToolUse", BASE_TIME + 30_001, Some("task"));
    assert_continue(&reset);
    let after_reset = run("postToolUse", BASE_TIME + 30_002, Some("bash"));
    assert_continue(&after_reset);
}

#[test]
fn deeply_nested_configuration_is_rejected_without_state_or_stack_overflow() {
    const DEPTH: usize = 6000;

    let environment = TestEnvironment::new();
    let mut configuration = String::from("{\"agents\":{\"");
    configuration.push_str(AGENT);
    configuration.push_str("\":[{\"message\":\"x\",\"condition\":");
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
