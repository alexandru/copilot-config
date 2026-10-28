//! Black-box tests for the `copilot-switch` executable.
//!
//! Every test links the built binary into a temporary fixture `bin` directory,
//! so the program resolves its configuration from the fixture root. No test
//! reads or writes the developer's real configuration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const PRESETS: &str = "config.presets.jsonc";
const SETTINGS_COMMON: &str = "settings.common.jsonc";
const MCP_COMMON: &str = "mcp-config.common.jsonc";
const SETTINGS: &str = "settings.json";
const MCP: &str = "mcp-config.json";

const SUCCESS_PRESETS: &str = r#"// Presets for the integration test.
{
  "shared": {
    "common": true,
    "settings.json": {
      "model": "shared-model",
      "effortLevel": "low",
    },
    "mcp-config.json": {
      "mcpServers": {
        "shared": { "command": "shared" },
      },
    },
  },
  "selected": {
    "extends": ["shared"],
    "settings.json": {
      "model": "auto",
      "autoTier": "balance",
      "subagents": {
        "agents": {
          "Junior": { "model": "auto", "autoTier": "efficiency" },
          "disabled": { "model": "ignored" },
        },
      },
    },
    "mcp-config.json": {
      "mcpServers": {
        "selected": { "command": "selected" },
      },
    },
  },
  "--dash": {
    "settings.json": { "model": "dash-model" },
    "mcp-config.json": {},
  },
}"#;

const SUCCESS_SETTINGS_COMMON: &str = r#"// Shared settings.
{
  "disabledMcpServers": ["github-mcp-server"],
  "subagents": {
    "disabledSubagents": ["disabled"],
  },
  "builtInAgents": {
    "rubberDuck": false,
  },
  "greeting": "héllo ✓",
}"#;

const SUCCESS_MCP_COMMON: &str = r#"{
  "mcpServers": {},
}"#;

const EXPECTED_SETTINGS: &str = r#"{
  "disabledMcpServers": [
    "github-mcp-server"
  ],
  "subagents": {
    "disabledSubagents": [
      "disabled"
    ],
    "agents": {
      "Junior": {
        "model": "auto",
        "autoTier": "efficiency"
      },
      "disabled": {
        "model": "ignored"
      }
    }
  },
  "builtInAgents": {
    "rubberDuck": false
  },
  "greeting": "héllo ✓",
  "model": "auto",
  "effortLevel": "low",
  "autoTier": "balance"
}
"#;

const EXPECTED_MCP: &str = r#"{
  "mcpServers": {
    "shared": {
      "command": "shared"
    },
    "selected": {
      "command": "selected"
    }
  }
}
"#;

const EXPECTED_SELECTED_STDOUT: &str = concat!(
    "Successfully switched to preset: selected\n",
    "Generated settings.json and mcp-config.json\n",
    "\n",
    "Default model: auto (balance)\n",
    "\n",
    "Agents:\n",
    "\n",
    "  Agent        │ Model │ Effort     │ Context Tier\n",
    "  ─────────────┼───────┼────────────┼─────────────\n",
    "  Orchestrator │ auto  │ balance    │ -\n",
    "  Solo         │ auto  │ balance    │ -\n",
    "  Junior       │ auto  │ efficiency │ -\n",
    "\n",
);

const EXPECTED_HELP: &str = concat!(
    "Copilot CLI configuration preset switcher\n",
    "\n",
    "Usage: copilot-switch <preset-name>\n",
    "\n",
    "Available presets:\n",
    "  - selected\n",
    "  - --dash\n",
);

/// Temporary repository layout with the binary installed under `bin`.
struct Fixture {
    directory: TempDir,
    root: PathBuf,
    binary: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target_tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
        fs::create_dir_all(target_tmp).expect("create target temporary directory");
        let directory = tempfile::Builder::new()
            .prefix("copilot-switch-")
            .tempdir_in(target_tmp)
            .expect("create temporary test directory");
        let root = directory.path().join("root");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("create fixture bin directory");

        let binary = bin.join(format!("copilot-switch{}", std::env::consts::EXE_SUFFIX));
        // A hard link avoids opening the built binary for writing in every test.
        // Concurrent `fs::copy` calls can make another thread's `exec` fail with
        // ETXTBSY because a freshly written executable is still write-held.
        fs::hard_link(env!("CARGO_BIN_EXE_copilot-switch"), &binary)
            .expect("link copilot-switch binary");

        Self {
            directory,
            root,
            binary,
        }
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture subdirectory");
        }
        fs::write(path, content).expect("write fixture file");
    }

    fn write_success_fixture(&self) {
        self.write(PRESETS, SUCCESS_PRESETS);
        self.write(SETTINGS_COMMON, SUCCESS_SETTINGS_COMMON);
        self.write(MCP_COMMON, SUCCESS_MCP_COMMON);
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.run_in(arguments, &self.root)
    }

    fn run_in(&self, arguments: &[&str], working_directory: &Path) -> Output {
        Command::new(&self.binary)
            .args(arguments)
            .current_dir(working_directory)
            .output()
            .expect("run copilot-switch")
    }

    fn output_path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn read_output(&self, name: &str) -> String {
        fs::read_to_string(self.output_path(name)).expect("read generated output")
    }

    fn has_outputs(&self) -> bool {
        self.output_path(SETTINGS).exists() || self.output_path(MCP).exists()
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "unexpected exit status; stderr: {}",
        stderr_text(output)
    );
}

fn path(fixture: &Fixture, name: &str) -> String {
    fixture.output_path(name).display().to_string()
}

#[test]
fn help_lists_only_visible_presets_without_writing_outputs() {
    let fixture = Fixture::new();
    fixture.write(PRESETS, SUCCESS_PRESETS);

    for arguments in [&[][..], &["-h"], &["--help"]] {
        let output = fixture.run(arguments);
        assert_exit(&output, 0);
        assert_eq!(stdout_text(&output), EXPECTED_HELP);
        assert!(stderr_text(&output).is_empty());
    }

    assert!(!fixture.has_outputs(), "help must not generate outputs");
}

#[test]
fn help_reports_an_invalid_preset_document() {
    let fixture = Fixture::new();
    fixture.write(PRESETS, "{ selected: ");

    let output = fixture.run(&["--help"]);

    assert_exit(&output, 1);
    assert!(stdout_text(&output).is_empty());
    assert!(
        stderr_text(&output).starts_with(&format!(
            "Error: failed to parse {}",
            path(&fixture, PRESETS)
        )),
        "unexpected stderr: {}",
        stderr_text(&output)
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn missing_presets_file_reports_one_error_without_writing() {
    let fixture = Fixture::new();

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert!(stdout_text(&output).is_empty());
    assert_eq!(
        stderr_text(&output),
        format!(
            "Error: configuration file missing: {}\n",
            path(&fixture, PRESETS)
        )
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn malformed_presets_file_reports_one_error_without_a_panic() {
    let fixture = Fixture::new();
    fixture.write(PRESETS, "{ selected: ");

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert!(stdout_text(&output).is_empty());
    let stderr = stderr_text(&output);
    assert!(
        stderr.starts_with(&format!(
            "Error: failed to parse {}",
            path(&fixture, PRESETS)
        )),
        "unexpected stderr: {stderr}"
    );
    assert!(!stderr.contains("panicked"), "unexpected panic: {stderr}");
    assert!(!fixture.has_outputs());
}

#[test]
fn non_object_presets_root_is_rejected() {
    let fixture = Fixture::new();
    fixture.write(PRESETS, "[1]");

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        format!(
            "Error: {} must contain a JSON object\n",
            path(&fixture, PRESETS)
        )
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn missing_common_fragment_reports_the_expected_path() {
    let fixture = Fixture::new();
    fixture.write(
        PRESETS,
        r#"{
            "selected": {
                "settings.json": {},
                "mcp-config.json": {},
            },
        }"#,
    );

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        format!(
            "Error: configuration file missing: {}\n",
            path(&fixture, SETTINGS_COMMON)
        )
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn an_invalid_second_output_prevents_both_writes() {
    let fixture = Fixture::new();
    fixture.write(
        PRESETS,
        r#"{
            "selected": {
                "settings.json": { "model": "first" },
                "mcp-config.json": "not-an-object",
            },
        }"#,
    );
    fixture.write(SETTINGS_COMMON, "{}");
    fixture.write(MCP_COMMON, "{}");

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        "Error: Preset 'selected' must define 'mcp-config.json' as an object\n"
    );
    assert!(
        !fixture.has_outputs(),
        "no output may be written when the second output is invalid"
    );
}

#[test]
fn an_invalid_second_common_fragment_prevents_both_writes() {
    let fixture = Fixture::new();
    fixture.write(
        PRESETS,
        r#"{
            "selected": {
                "settings.json": { "model": "first" },
                "mcp-config.json": {},
            },
        }"#,
    );
    fixture.write(SETTINGS_COMMON, "{}");
    fixture.write(MCP_COMMON, "[1]");

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        format!(
            "Error: {} must contain a JSON object\n",
            path(&fixture, MCP_COMMON)
        )
    );
    assert!(
        !fixture.has_outputs(),
        "no output may be written when the second common fragment is invalid"
    );
}

#[test]
fn circular_inheritance_reports_the_resolution_path() {
    let fixture = Fixture::new();
    fixture.write(
        PRESETS,
        r#"{
            "first": { "extends": ["second"] },
            "second": { "extends": ["first"] },
        }"#,
    );

    let output = fixture.run(&["first"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        "Error: Circular preset inheritance detected: first -> second -> first\n"
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn unknown_preset_lists_only_visible_presets() {
    let fixture = Fixture::new();
    fixture.write(PRESETS, SUCCESS_PRESETS);

    let output = fixture.run(&["missing"]);

    assert_exit(&output, 1);
    assert_eq!(
        stderr_text(&output),
        concat!(
            "Error: Preset 'missing' not found. Available presets:\n",
            "  - selected\n",
            "  - --dash\n",
        )
    );
    assert!(!fixture.has_outputs());
}

#[test]
fn success_writes_both_outputs_in_merge_order_and_prints_the_summary() {
    let fixture = Fixture::new();
    fixture.write_success_fixture();

    let output = fixture.run(&["selected"]);

    assert_exit(&output, 0);
    assert!(stderr_text(&output).is_empty());
    assert_eq!(stdout_text(&output), EXPECTED_SELECTED_STDOUT);
    assert_eq!(fixture.read_output(SETTINGS), EXPECTED_SETTINGS);
    assert_eq!(fixture.read_output(MCP), EXPECTED_MCP);
}

#[test]
fn a_common_fragment_remains_selectable_when_named_explicitly() {
    let fixture = Fixture::new();
    fixture.write_success_fixture();

    let output = fixture.run(&["shared"]);

    assert_exit(&output, 0);
    assert!(
        stdout_text(&output).starts_with("Successfully switched to preset: shared\n"),
        "unexpected stdout: {}",
        stdout_text(&output)
    );
    let settings: Value =
        serde_json::from_str(&fixture.read_output(SETTINGS)).expect("settings output is JSON");
    assert_eq!(settings["model"], Value::String("shared-model".to_owned()));
    assert_eq!(settings["effortLevel"], Value::String("low".to_owned()));
}

#[test]
fn arguments_reject_unknown_options_and_extras_and_accept_dash_names_after_double_dash() {
    let fixture = Fixture::new();
    fixture.write_success_fixture();

    let unknown = fixture.run(&["--bogus"]);
    assert_exit(&unknown, 2);
    assert!(stdout_text(&unknown).is_empty());
    assert!(
        stderr_text(&unknown).contains("No such option: --bogus"),
        "unexpected stderr: {}",
        stderr_text(&unknown)
    );
    assert!(!fixture.has_outputs());

    let extra = fixture.run(&["selected", "extra"]);
    assert_exit(&extra, 2);
    assert!(
        stderr_text(&extra).contains("Got unexpected extra argument(s) (extra)"),
        "unexpected stderr: {}",
        stderr_text(&extra)
    );
    assert!(!fixture.has_outputs());

    let dash = fixture.run(&["--", "--dash"]);
    assert_exit(&dash, 0);
    assert!(
        stdout_text(&dash).starts_with("Successfully switched to preset: --dash\n"),
        "unexpected stdout: {}",
        stdout_text(&dash)
    );
    let settings: Value =
        serde_json::from_str(&fixture.read_output(SETTINGS)).expect("settings output is JSON");
    assert_eq!(settings["model"], Value::String("dash-model".to_owned()));
}

#[test]
fn configuration_is_resolved_from_the_binary_not_the_working_directory() {
    let fixture = Fixture::new();
    fixture.write_success_fixture();
    let working_directory = fixture.directory.path().join("working");
    fs::create_dir_all(&working_directory).expect("create working directory");

    let output = fixture.run_in(&["selected"], &working_directory);

    assert_exit(&output, 0);
    assert!(fixture.output_path(SETTINGS).is_file());
    assert!(fixture.output_path(MCP).is_file());
    assert!(!working_directory.join(SETTINGS).exists());
    assert!(!working_directory.join(MCP).exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_binary_resolves_its_target_repository() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    fixture.write_success_fixture();
    let link_directory = fixture.directory.path().join("links");
    let working_directory = fixture.directory.path().join("working");
    fs::create_dir_all(&link_directory).expect("create link directory");
    fs::create_dir_all(&working_directory).expect("create working directory");
    let link = link_directory.join("switch");
    symlink(&fixture.binary, &link).expect("create binary symlink");

    let output = Command::new(&link)
        .arg("selected")
        .current_dir(&working_directory)
        .output()
        .expect("run symlinked copilot-switch");

    assert_exit(&output, 0);
    assert_eq!(stdout_text(&output), EXPECTED_SELECTED_STDOUT);
    assert_eq!(fixture.read_output(SETTINGS), EXPECTED_SETTINGS);
    assert_eq!(fixture.read_output(MCP), EXPECTED_MCP);
    assert!(!working_directory.join(SETTINGS).exists());
    assert!(!working_directory.join(MCP).exists());
}
