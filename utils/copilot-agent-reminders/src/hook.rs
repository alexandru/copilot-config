//! Command-hook adapter for Copilot CLI events.
//!
//! The adapter owns process concerns: argument parsing, bounded stdin reads,
//! configuration and state discovery, persistence, and Copilot's stdout and
//! exit-code protocol. State transitions stay in [`crate::machine`].
//!
//! Every failure is fail-open. The adapter writes `{}` and exits successfully
//! even for a failed `preToolUse` invocation; the only non-zero exit is the
//! documented `postToolUseFailure` recovery path after a reminder has been
//! committed to persistent state.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Deserialize;
use thiserror::Error;

use crate::config::{ConfigError, Configuration, ObservedEvent, Policy};
use crate::machine::{BoundSession, Event, Output, SessionState, evolve};
use crate::storage::LockedState;
use crate::time::TimestampMillis;

/// Configuration file name inside the Copilot configuration directory.
const CONFIG_FILE_NAME: &str = "agent-reminders.jsonc";
/// Default configuration directory below `$HOME`.
const DEFAULT_CONFIG_DIRECTORY: &str = ".copilot";
/// Default state directory below `$HOME`.
const DEFAULT_STATE_DIRECTORY: &str = ".agents/sessions/copilot";
/// Upper bound for a hook payload read from stdin.
const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;
/// Separator used when one transition emits several reminders.
const MESSAGE_SEPARATOR: &str = "\n\n";
/// Exact no-action output expected by Copilot command hooks.
const CONTINUE_OUTPUT: &str = "{}\n";
/// `postToolUseFailure` exit code that makes Copilot append stdout to the
/// failed tool result as recovery guidance.
const FAILURE_RECOVERY_EXIT_CODE: u8 = 2;

const HELP: &str = "\
copilot-agent-reminders: Copilot command-hook reminders

Usage:
  copilot-agent-reminders --agent <agent> --event <event> [--config <path>] [--state-dir <path>]

Events:
  preToolUse, postToolUse, postToolUseFailure, preCompact, sessionStart

Hook registration:
  Hook registrations MUST use camelCase event keys: preToolUse, postToolUse,
  postToolUseFailure, preCompact, sessionStart. These keys deliver camelCase
  payload fields and a numeric Unix-millisecond timestamp. PascalCase aliases
  deliver snake_case fields and ISO-8601 timestamps and are not supported.
  The registration key, not the --event spelling, determines the payload
  format.

Configuration:
  JSON5 read from $COPILOT_HOME/agent-reminders.jsonc, or from
  ~/.copilot/agent-reminders.jsonc when COPILOT_HOME is unset or empty.
  State defaults to ~/.agents/sessions/copilot; --config and --state-dir
  override both paths.
  Configuration nesting is limited to 128 object or array levels.

  Supported conditions: toolCalls, elapsedTime, preCompact events, all, any.
  Context growth, context utilization, and model changes are unsupported.
  Configuration and state failures never deny agent work: the hook prints {}
  and exits 0.

Output:
  postToolUse reminders are written as additionalContext JSON. A
  postToolUseFailure reminder prints the reminder text and exits 2 so Copilot
  appends it to the failed tool result.
";

/// Runs the command hook with arguments after the executable name.
///
/// `--help` is handled before stdin or state is touched. All other errors are
/// reported on stderr and converted into a successful no-action result so a
/// hook failure cannot deny work.
pub(crate) fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();

    if args.iter().any(|argument| argument == OsStr::new("--help")) {
        let _ = write_stdout(HELP);
        return ExitCode::SUCCESS;
    }

    match dispatch(&args) {
        Ok(result) => result.write(),
        Err(error) => {
            report(&error);
            let _ = write_stdout(CONTINUE_OUTPUT);
            ExitCode::SUCCESS
        }
    }
}

/// Parsed command-line options.
#[derive(Debug)]
struct Options {
    agent: String,
    event: HookEvent,
    config: Option<PathBuf>,
    state_directory: Option<PathBuf>,
}

/// Hook events supported by this executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PreCompact,
    SessionStart,
}

impl HookEvent {
    fn parse(value: &str) -> Result<Self, HookError> {
        match value {
            "preToolUse" => Ok(Self::PreToolUse),
            "postToolUse" => Ok(Self::PostToolUse),
            "postToolUseFailure" => Ok(Self::PostToolUseFailure),
            "preCompact" => Ok(Self::PreCompact),
            "sessionStart" => Ok(Self::SessionStart),
            other => Err(HookError::Arguments(format!(
                "unknown event {other:?}; expected one of preToolUse, postToolUse, \
                 postToolUseFailure, preCompact, sessionStart"
            ))),
        }
    }

    /// Canonical camelCase event name used in diagnostics and delivery logs.
    const fn name(self) -> &'static str {
        match self {
            Self::PreToolUse => "preToolUse",
            Self::PostToolUse => "postToolUse",
            Self::PostToolUseFailure => "postToolUseFailure",
            Self::PreCompact => "preCompact",
            Self::SessionStart => "sessionStart",
        }
    }
}

/// The subset of Copilot's camelCase hook payload this adapter uses.
///
/// PascalCase registration aliases send snake_case payload fields and ISO-8601
/// timestamps. Those payloads are intentionally rejected instead of translated;
/// only camelCase registrations are supported.
///
/// Unknown fields are intentionally ignored so new payload fields do not break
/// existing hooks. Tool arguments and result blobs are not read.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookInput {
    session_id: String,
    timestamp: TimestampMillis,
    tool_name: Option<String>,
}

/// Delivery metadata recorded after reminder text reaches stdout.
///
/// Only transport-level identifiers are retained. Reminder text, prompts, tool
/// arguments, and tool results never enter the delivery log.
#[derive(Debug)]
struct Delivery {
    session_id: String,
    agent: String,
    observed_at_ms: TimestampMillis,
    event: HookEvent,
    message_count: usize,
}

/// A state transition's output plus delivery metadata.
#[derive(Debug)]
struct HookResult {
    delivery: Delivery,
    output: Output,
}

impl HookResult {
    fn write(self) -> ExitCode {
        match self.output {
            Output::Continue => {
                let _ = write_stdout(CONTINUE_OUTPUT);
                ExitCode::SUCCESS
            }
            Output::Emit { messages } => {
                let joined = messages.join(MESSAGE_SEPARATOR);
                write_reminders(&self.delivery, &joined)
            }
        }
    }
}

/// Writes reminder output and logs the delivery only after stdout succeeds.
fn write_reminders(delivery: &Delivery, joined: &str) -> ExitCode {
    if delivery.event == HookEvent::PostToolUseFailure {
        return match write_stdout(&format!("{joined}\n")) {
            Ok(()) => {
                log_delivery(delivery);
                ExitCode::from(FAILURE_RECOVERY_EXIT_CODE)
            }
            Err(_) => ExitCode::SUCCESS,
        };
    }

    let payload = serde_json::json!({ "additionalContext": joined });
    match serde_json::to_string(&payload) {
        Ok(json) => {
            if write_stdout(&format!("{json}\n")).is_ok() {
                log_delivery(delivery);
            }
        }
        Err(_) => {
            let _ = write_stdout(CONTINUE_OUTPUT);
        }
    }
    ExitCode::SUCCESS
}

fn log_delivery(delivery: &Delivery) {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = writeln!(
        stderr,
        "copilot-agent-reminders: info: emitted sessionId={} agent={} timestamp={} event={} messages={}",
        log_identifier(&delivery.session_id),
        log_identifier(&delivery.agent),
        delivery.observed_at_ms,
        delivery.event.name(),
        delivery.message_count,
    );
}

/// Escapes an identifier as a JSON string so one record stays on one line.
fn log_identifier(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Errors are domain-scoped and always end in a fail-open hook result.
#[derive(Debug, Error)]
enum HookError {
    #[error("invalid arguments: {0}")]
    Arguments(String),

    #[error("could not read hook input from stdin: {0}")]
    ReadInput(#[source] io::Error),

    #[error("hook input exceeds the 16 MiB limit")]
    InputTooLarge,

    #[error("invalid hook input JSON: {0}")]
    InvalidInput(#[source] serde_json::Error),

    #[error("preToolUse hook input is missing a nonempty toolName")]
    MissingToolName,

    #[error(
        "neither COPILOT_HOME nor HOME is set to a nonempty path; cannot locate the configuration"
    )]
    MissingConfigHome,

    #[error("HOME is not set to a nonempty path; cannot locate the state directory")]
    MissingStateHome,

    #[error("could not read configuration {path}: {source}")]
    ConfigRead {
        path: String,
        #[source]
        source: io::Error,
    },

    #[error("invalid configuration {path}: {source}")]
    ConfigInvalid {
        path: String,
        #[source]
        source: ConfigError,
    },

    #[error("could not open session state: {0}")]
    OpenState(#[source] io::Error),

    #[error("could not clear session state: {0}")]
    ClearState(#[source] io::Error),

    #[error("could not read session state: {0}")]
    ReadState(#[source] io::Error),

    #[error("could not serialize session state: {0}")]
    SerializeState(#[source] serde_json::Error),

    #[error("could not write session state: {0}")]
    WriteState(#[source] io::Error),
}

fn dispatch(args: &[OsString]) -> Result<HookResult, HookError> {
    // Read the payload before any fallible validation so Copilot's stdin write
    // cannot fail just because an argument or the configuration is invalid.
    // Argument diagnostics still take precedence over input and config errors.
    let input = read_input();
    let options = parse_options(args)?;
    let input = input?;
    let config_path = config_path(options.config.as_deref())?;
    let configuration = load_configuration(&config_path)?;

    if configuration.is_empty() {
        warn_empty_configuration(&config_path);
    }

    let policy = configuration.policy(&options.agent);
    let event = event_for(options.event, &input)?;
    let state_directory = state_directory(options.state_directory.as_deref())?;
    let output = transition(&policy, &state_directory, &input, event)?;
    let message_count = match &output {
        Output::Continue => 0,
        Output::Emit { messages } => messages.iter().count(),
    };

    Ok(HookResult {
        delivery: Delivery {
            session_id: input.session_id,
            agent: options.agent,
            observed_at_ms: input.timestamp,
            event: options.event,
            message_count,
        },
        output,
    })
}

fn parse_options(args: &[OsString]) -> Result<Options, HookError> {
    let mut agent = None;
    let mut event = None;
    let mut config = None;
    let mut state_directory = None;
    let mut index = 0;

    while index < args.len() {
        let name = args[index]
            .to_str()
            .ok_or_else(|| HookError::Arguments("argument names must be valid UTF-8".to_owned()))?;

        match name {
            "--agent" => {
                let value = next_value(args, index, name)?;
                set_once(&mut agent, utf8_value(name, value)?, name)?;
            }
            "--event" => {
                let value = next_value(args, index, name)?;
                let value = utf8_value(name, value)?;
                set_once(&mut event, HookEvent::parse(&value)?, name)?;
            }
            "--config" => {
                let value = next_value(args, index, name)?;
                set_once(&mut config, PathBuf::from(value), name)?;
            }
            "--state-dir" => {
                let value = next_value(args, index, name)?;
                set_once(&mut state_directory, PathBuf::from(value), name)?;
            }
            other => {
                return Err(HookError::Arguments(format!(
                    "unknown argument {other:?}; try --help"
                )));
            }
        }

        index += 2;
    }

    let agent = agent.ok_or_else(|| HookError::Arguments("missing required --agent".to_owned()))?;
    let event = event.ok_or_else(|| HookError::Arguments("missing required --event".to_owned()))?;

    Ok(Options {
        agent,
        event,
        config,
        state_directory,
    })
}

fn next_value<'a>(args: &'a [OsString], index: usize, name: &str) -> Result<&'a OsStr, HookError> {
    args.get(index + 1)
        .map(OsString::as_os_str)
        .ok_or_else(|| HookError::Arguments(format!("{name} requires a value")))
}

fn utf8_value(name: &str, value: &OsStr) -> Result<String, HookError> {
    let value = value
        .to_str()
        .ok_or_else(|| HookError::Arguments(format!("{name} must be valid UTF-8")))?;
    if value.is_empty() {
        return Err(HookError::Arguments(format!("{name} must not be empty")));
    }
    Ok(value.to_owned())
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), HookError> {
    if slot.is_some() {
        return Err(HookError::Arguments(format!(
            "{name} was provided more than once"
        )));
    }
    *slot = Some(value);
    Ok(())
}

fn config_path(override_path: Option<&Path>) -> Result<PathBuf, HookError> {
    if let Some(path) = override_path {
        return Ok(path.to_owned());
    }

    if let Some(copilot_home) = nonempty_env_os("COPILOT_HOME") {
        return Ok(PathBuf::from(copilot_home).join(CONFIG_FILE_NAME));
    }

    let home = nonempty_env_os("HOME").ok_or(HookError::MissingConfigHome)?;
    Ok(PathBuf::from(home)
        .join(DEFAULT_CONFIG_DIRECTORY)
        .join(CONFIG_FILE_NAME))
}

fn state_directory(override_path: Option<&Path>) -> Result<PathBuf, HookError> {
    if let Some(path) = override_path {
        return Ok(path.to_owned());
    }

    let home = nonempty_env_os("HOME").ok_or(HookError::MissingStateHome)?;
    Ok(PathBuf::from(home).join(DEFAULT_STATE_DIRECTORY))
}

/// Reads an environment variable, treating an empty value as unset.
///
/// The value stays an [`OsString`] so non-UTF-8 paths remain usable.
fn nonempty_env_os(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

fn load_configuration(path: &Path) -> Result<Configuration, HookError> {
    let source = fs::read_to_string(path).map_err(|source| HookError::ConfigRead {
        path: path.display().to_string(),
        source,
    })?;

    Configuration::parse(&source).map_err(|source| HookError::ConfigInvalid {
        path: path.display().to_string(),
        source,
    })
}

fn read_input() -> Result<HookInput, HookError> {
    let stdin = io::stdin();
    let mut reader = stdin.lock().take((MAX_STDIN_BYTES as u64) + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(HookError::ReadInput)?;

    if bytes.len() > MAX_STDIN_BYTES {
        return Err(HookError::InputTooLarge);
    }

    serde_json::from_slice(&bytes).map_err(HookError::InvalidInput)
}

fn event_for(event: HookEvent, input: &HookInput) -> Result<Option<Event>, HookError> {
    let event = match event {
        HookEvent::SessionStart => None,
        HookEvent::PreToolUse => {
            let tool = input
                .tool_name
                .as_deref()
                .filter(|tool| !tool.is_empty())
                .ok_or(HookError::MissingToolName)?;
            Some(Event::ToolBefore {
                tool: tool.to_owned(),
                now_ms: input.timestamp,
            })
        }
        HookEvent::PostToolUse | HookEvent::PostToolUseFailure => Some(Event::ToolAfter {
            now_ms: input.timestamp,
        }),
        HookEvent::PreCompact => Some(Event::Observed {
            event: ObservedEvent::PreCompact,
            now_ms: input.timestamp,
        }),
    };

    Ok(event)
}

fn transition(
    policy: &Policy,
    directory: &Path,
    input: &HookInput,
    event: Option<Event>,
) -> Result<Output, HookError> {
    if policy.entries().is_empty() {
        clear_state(directory, &input.session_id)?;
        return Ok(Output::Continue);
    }

    let locked = LockedState::open(directory, &input.session_id).map_err(HookError::OpenState)?;
    let bound = load_state(&locked, policy, input.timestamp)?;

    let (successor, output) = match event {
        Some(event) => evolve(bound, event),
        None => (bound.into_state(), Output::Continue),
    };

    persist(&locked, &successor)?;
    Ok(output)
}

/// Drops persisted state for an unconfigured agent without touching the lock.
///
/// `open_existing` validates the session identifier even when no state exists
/// and avoids creating the session directory for a session that never had one.
fn clear_state(directory: &Path, session_id: &str) -> Result<(), HookError> {
    let Some(locked) =
        LockedState::open_existing(directory, session_id).map_err(HookError::OpenState)?
    else {
        return Ok(());
    };

    locked.clear().map_err(HookError::ClearState)
}

fn load_state<'policy>(
    locked: &LockedState,
    policy: &'policy Policy,
    now: TimestampMillis,
) -> Result<BoundSession<'policy>, HookError> {
    let Some(bytes) = locked.read().map_err(HookError::ReadState)? else {
        return Ok(BoundSession::fresh(policy, now));
    };

    let state = match serde_json::from_slice::<SessionState>(&bytes) {
        Ok(state) => state,
        Err(error) => {
            warn_reset_state(&error);
            return Ok(BoundSession::fresh(policy, now));
        }
    };

    match BoundSession::restore(policy, state) {
        Ok(bound) => Ok(bound),
        Err(error) => {
            warn_reset_state(&error);
            Ok(BoundSession::fresh(policy, now))
        }
    }
}

fn persist(locked: &LockedState, state: &SessionState) -> Result<(), HookError> {
    let bytes = serde_json::to_vec(state).map_err(HookError::SerializeState)?;
    locked.write(&bytes).map_err(HookError::WriteState)
}

fn warn_empty_configuration(path: &Path) {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = writeln!(
        stderr,
        "copilot-agent-reminders: warning: configuration {} has no configured agents; all reminders are \
         disabled",
        single_line(&path.display())
    );
}

fn warn_reset_state(reason: &impl fmt::Display) {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = writeln!(
        stderr,
        "copilot-agent-reminders: warning: resetting persisted session state: {}",
        single_line(reason)
    );
}

fn write_stdout(text: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

fn report(error: &HookError) {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let _ = writeln!(stderr, "copilot-agent-reminders: {}", single_line(error));
}

fn single_line(value: &impl fmt::Display) -> String {
    value
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_invalid_preserves_the_configuration_error_source() {
        let source =
            Configuration::parse("{ agents: [ }").expect_err("configuration must be invalid");
        let error = HookError::ConfigInvalid {
            path: "agent-reminders.jsonc".to_owned(),
            source,
        };

        let preserved = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<ConfigError>());

        assert!(matches!(preserved, Some(ConfigError::Syntax(_))));
    }
}
