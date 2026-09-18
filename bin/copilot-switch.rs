#!/usr/bin/env -S cargo +nightly -q -Zscript
---cargo
[package]
edition = "2024"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
jsonc-parser = { version = "0.33", features = ["serde"] }
serde_json = { version = "1", features = ["preserve_order"] }
---

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use jsonc_parser::{ParseOptions, parse_to_serde_value};
use serde_json::{Map, Value};

const PRESETS_FILE: &str = "config.presets.jsonc";
const OUTPUT_CONFIGS: [(&str, &str); 2] = [
    ("settings.json", "settings.common.jsonc"),
    ("mcp-config.json", "mcp-config.common.jsonc"),
];
const PRIMARY_AGENTS: [&str; 2] = ["Orchestrator", "Solo"];

#[derive(Debug, Parser)]
#[command(
    name = "copilot-switch",
    about = "Generate a Copilot CLI configuration from a preset",
    disable_help_flag = true
)]
struct Args {
    /// Preset to apply
    #[arg(value_name = "PRESET")]
    preset: Option<String>,

    /// Print help
    #[arg(short, long)]
    help: bool,
}

/// Loads the selected preset, merges it into each common configuration, and writes the outputs.
fn main() -> Result<()> {
    let args = Args::parse();
    let config_dir = config_dir()?;
    let presets = read_jsonc_object(&config_dir.join(PRESETS_FILE))?;

    let Some(preset_name) = args.preset.filter(|_| !args.help) else {
        print_help(&presets);
        return Ok(());
    };

    if !presets.contains_key(&preset_name) {
        bail!(
            "Preset '{preset_name}' not found. Available presets:\n{}",
            format_presets(&presets)
        );
    }

    let preset = resolve_preset(&presets, &preset_name, &mut Vec::new())?;
    let outputs = generate_outputs(&config_dir, &preset, &preset_name)?;

    for (output_name, config) in &outputs {
        write_json(&config_dir.join(*output_name), config)?;
    }

    println!("Successfully switched to preset: {preset_name}");
    println!(
        "Generated {}",
        outputs
            .iter()
            .map(|(output_name, _)| *output_name)
            .collect::<Vec<_>>()
            .join(" and ")
    );
    println!();

    let settings = generated_output(&outputs, "settings.json")?;
    if let Some(model) = settings
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
    {
        let effort = settings
            .get("effortLevel")
            .and_then(Value::as_str)
            .unwrap_or("default");
        println!("Default model: {model} ({effort})");
        println!();
    }

    let entries = active_agent_entries(settings);
    if !entries.is_empty() {
        println!("Subagents:");
        println!();
        print_agent_table(&entries);
    }
    println!();

    Ok(())
}

/// Returns the configuration directory containing this Cargo script's `bin` directory.
///
/// Cargo sets `CARGO_MANIFEST_DIR` to the directory containing the script, even when the
/// compiled script runs from Cargo's cache.
fn config_dir() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("failed to locate the configuration directory")
}

/// Reads a JSONC file and requires its root value to be an object.
fn read_jsonc_object(path: &Path) -> Result<Map<String, Value>> {
    let content = fs::read_to_string(path).with_context(|| {
        if path.exists() {
            format!("failed to read {}", path.display())
        } else {
            format!("configuration file missing: {}", path.display())
        }
    })?;

    let value: Value = parse_to_serde_value(&content, &ParseOptions::default())
        .with_context(|| format!("failed to parse {}", path.display()))?;

    value
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", path.display()))
}

/// Serializes a value as pretty-printed JSON with a trailing newline.
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut content = serde_json::to_string_pretty(value).context("failed to serialize config")?;
    content.push('\n');
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

/// Merges the resolved preset into every common configuration.
///
/// All outputs are validated and computed before the caller writes anything, so an invalid
/// preset cannot leave a partially generated configuration behind.
fn generate_outputs(
    config_dir: &Path,
    preset: &Value,
    preset_name: &str,
) -> Result<Vec<(&'static str, Value)>> {
    OUTPUT_CONFIGS
        .into_iter()
        .map(|(output_name, common_file)| {
            let preset_config = preset
                .get(output_name)
                .filter(|value| value.is_object())
                .ok_or_else(|| {
                    anyhow!("Preset '{preset_name}' must define '{output_name}' as an object")
                })?;

            let common = Value::Object(read_jsonc_object(&config_dir.join(common_file))?);
            Ok((output_name, deep_merge(common, preset_config.clone())))
        })
        .collect()
}

/// Returns a computed output by file name.
fn generated_output<'a>(outputs: &'a [(&'static str, Value)], name: &str) -> Result<&'a Value> {
    outputs
        .iter()
        .find_map(|(output_name, value)| (*output_name == name).then_some(value))
        .with_context(|| format!("generated output missing: {name}"))
}

/// Resolves a preset and its ordered parent chain into one configuration object.
///
/// Later parents override earlier parents, and the selected preset overrides every parent.
/// `ancestors` tracks the active resolution path so inheritance cycles can be reported.
fn resolve_preset(
    presets: &Map<String, Value>,
    preset_name: &str,
    ancestors: &mut Vec<String>,
) -> Result<Value> {
    let preset = presets
        .get(preset_name)
        .ok_or_else(|| anyhow!("Preset '{preset_name}' not found in {PRESETS_FILE}"))?
        .as_object()
        .ok_or_else(|| anyhow!("Preset '{preset_name}' must be an object"))?;

    if let Some(cycle_start) = ancestors.iter().position(|name| name == preset_name) {
        let mut cycle = ancestors[cycle_start..].to_vec();
        cycle.push(preset_name.to_owned());
        bail!(
            "Circular preset inheritance detected: {}",
            cycle.join(" -> ")
        );
    }

    let parent_names = match preset.get("extends") {
        None => return Ok(Value::Object(strip_preset_metadata(preset))),
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| {
                name.as_str().ok_or_else(|| {
                    anyhow!(
                        "Preset '{preset_name}' has invalid 'extends'; expected an array of preset names"
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        Some(_) => bail!(
            "Preset '{preset_name}' has invalid 'extends'; expected an array of preset names"
        ),
    };

    ancestors.push(preset_name.to_owned());
    let inherited =
        parent_names
            .into_iter()
            .try_fold(Value::Object(Map::new()), |merged, parent_name| {
                resolve_preset(presets, parent_name, ancestors)
                    .map(|parent| deep_merge(merged, parent))
            });
    ancestors.pop();

    Ok(deep_merge(
        inherited?,
        Value::Object(strip_preset_metadata(preset)),
    ))
}

/// Removes fields used by the preset system rather than by Copilot CLI itself.
fn strip_preset_metadata(preset: &Map<String, Value>) -> Map<String, Value> {
    let mut config = preset.clone();
    config.remove("common");
    config.remove("extends");
    config
}

/// Recursively merges two JSON values using the switcher's compatibility rules.
///
/// Objects merge recursively, arrays and primitive target values replace their source values,
/// and a target `null` retains a source object.
fn deep_merge(source: Value, target: Value) -> Value {
    match (source, target) {
        (source, Value::Null) => source,
        (Value::Null, target) => target,
        (Value::Object(mut source), Value::Object(target)) => {
            for (key, target_value) in target {
                match source.get_mut(&key) {
                    Some(source_value)
                        if is_object_like(source_value) && is_object_like(&target_value) =>
                    {
                        let original = std::mem::take(source_value);
                        *source_value = deep_merge(original, target_value);
                    }
                    _ => {
                        source.insert(key, target_value);
                    }
                }
            }
            Value::Object(source)
        }
        (_, target) => target,
    }
}

/// Reports whether a value participates in recursive object merging.
///
/// JSON `null` counts as object-like so a target `null` can retain a source object.
fn is_object_like(value: &Value) -> bool {
    matches!(value, Value::Object(_) | Value::Null)
}

/// Iterates over presets that are available for direct selection.
///
/// Presets marked with `"common": true` remain available for inheritance but are hidden here.
fn visible_preset_names(presets: &Map<String, Value>) -> impl Iterator<Item = &str> {
    presets.iter().filter_map(|(name, preset)| {
        (preset.get("common") != Some(&Value::Bool(true))).then_some(name.as_str())
    })
}

/// Formats visible preset names for help and error output.
fn format_presets(presets: &Map<String, Value>) -> String {
    visible_preset_names(presets)
        .map(|name| format!("  - {name}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prints usage information and the presets available for direct selection.
fn print_help(presets: &Map<String, Value>) {
    println!("Copilot CLI configuration preset switcher\n");
    println!("Usage: copilot-switch <preset-name>\n");
    println!("Available presets:\n{}", format_presets(presets));
}

/// Returns the agents shown in the post-switch summary.
///
/// Primary agents come first with the top-level model, subagents follow in configuration
/// order, and disabled subagents are omitted.
fn active_agent_entries(settings: &Value) -> Vec<AgentEntry<'_>> {
    let disabled = disabled_subagents(settings);
    let default_model = settings.get("model").and_then(Value::as_str).unwrap_or("-");
    let default_effort = settings
        .get("effortLevel")
        .and_then(Value::as_str)
        .unwrap_or("-");

    let mut entries = PRIMARY_AGENTS
        .iter()
        .map(|&name| AgentEntry {
            name,
            model: default_model,
            effort: default_effort,
        })
        .collect::<Vec<_>>();

    if let Some(agents) = settings
        .get("subagents")
        .and_then(|value| value.get("agents"))
        .and_then(Value::as_object)
    {
        entries.extend(
            agents
                .iter()
                .filter(|&(name, _)| {
                    !disabled.contains(name.as_str()) && !PRIMARY_AGENTS.contains(&name.as_str())
                })
                .map(|(name, config)| AgentEntry {
                    name,
                    model: string_field(config, "model").unwrap_or("-"),
                    effort: string_field(config, "effortLevel").unwrap_or("-"),
                }),
        );
    }

    entries
}

/// Collects subagent names disabled by explicit config or the built-in rubber duck toggle.
fn disabled_subagents(settings: &Value) -> BTreeSet<&str> {
    let mut disabled = settings
        .get("subagents")
        .and_then(|value| value.get("disabledSubagents"))
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    if settings
        .get("builtInAgents")
        .and_then(|value| value.get("rubberDuck"))
        == Some(&Value::Bool(false))
    {
        disabled.insert("rubber-duck");
    }

    disabled
}

/// Prints the agent, model, and effort mappings for the post-switch summary.
fn print_agent_table(entries: &[AgentEntry<'_>]) {
    if entries.is_empty() {
        return;
    }

    let agent_width = column_width("Agent", entries.iter().map(|entry| entry.name));
    let model_width = column_width("Model", entries.iter().map(|entry| entry.model));
    let effort_width = column_width("Effort", entries.iter().map(|entry| entry.effort));

    println!(
        "  {0:<agent_width$} │ {1:<model_width$} │ Effort",
        "Agent", "Model"
    );
    println!(
        "  {}─┼─{}─┼─{}",
        "─".repeat(agent_width),
        "─".repeat(model_width),
        "─".repeat(effort_width)
    );
    for entry in entries {
        println!(
            "  {name:<agent_width$} │ {model:<model_width$} │ {effort}",
            name = entry.name,
            model = entry.model,
            effort = entry.effort
        );
    }
}

struct AgentEntry<'a> {
    name: &'a str,
    model: &'a str,
    effort: &'a str,
}

/// Returns the display width needed for a table column and its header.
fn column_width<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(|value| value.chars().count())
        .max()
        .unwrap_or(0)
        .max(header.chars().count())
}

/// Returns a string field from a JSON object, or `None` for any other shape or value type.
fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.as_object()?.get(key)?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deep_merge_merges_objects_and_replaces_arrays_and_primitives() {
        let source = json!({
            "nested": { "kept": true, "overridden": "old" },
            "items": [1, 2],
            "primitive": "old",
            "retained": { "value": 1 }
        });
        let target = json!({
            "nested": { "overridden": "new", "added": true },
            "items": [3],
            "primitive": 42,
            "retained": null
        });

        let merged = deep_merge(source, target);

        assert_eq!(
            merged,
            json!({
                "nested": { "kept": true, "overridden": "new", "added": true },
                "items": [3],
                "primitive": 42,
                "retained": { "value": 1 }
            })
        );
    }

    #[test]
    fn resolve_preset_applies_parents_in_order_and_removes_metadata() -> Result<()> {
        let presets = json!({
            "base": { "common": true, "value": "base", "base_only": true },
            "second": { "common": true, "value": "second" },
            "selected": {
                "extends": ["base", "second"],
                "value": "selected"
            }
        });
        let presets = presets
            .as_object()
            .cloned()
            .context("test presets must be an object")?;

        let resolved = resolve_preset(&presets, "selected", &mut Vec::new())?;

        assert_eq!(resolved, json!({ "value": "selected", "base_only": true }));
        Ok(())
    }

    #[test]
    fn resolve_preset_rejects_inheritance_cycles() -> Result<()> {
        let presets = json!({
            "first": { "extends": ["second"] },
            "second": { "extends": ["first"] }
        });
        let presets = presets
            .as_object()
            .cloned()
            .context("test presets must be an object")?;

        let error = resolve_preset(&presets, "first", &mut Vec::new())
            .expect_err("inheritance cycle must fail");

        assert_eq!(
            error.to_string(),
            "Circular preset inheritance detected: first -> second -> first"
        );
        Ok(())
    }

    #[test]
    fn active_agent_entries_include_primary_agents_and_filter_disabled_subagents() {
        let settings = json!({
            "model": "gpt-5.6-sol",
            "effortLevel": "medium",
            "subagents": {
                "disabledSubagents": ["task", "code-review"],
                "agents": {
                    "Orchestrator": { "model": "ignored" },
                    "Solo": { "model": "ignored" },
                    "Junior": { "model": "gpt-5.6-luna", "effortLevel": "high" },
                    "Explorer": { "model": "gpt-5.6-luna" },
                    "task": { "model": "gpt-5.6-luna" },
                    "code-review": { "model": "gpt-5.6-luna" },
                    "rubber-duck": { "model": "gpt-5.6-luna" }
                }
            },
            "builtInAgents": { "rubberDuck": false }
        });

        let entries = active_agent_entries(&settings);
        let rows = entries
            .iter()
            .map(|entry| (entry.name, entry.model, entry.effort))
            .collect::<Vec<_>>();

        assert_eq!(
            rows,
            [
                ("Orchestrator", "gpt-5.6-sol", "medium"),
                ("Solo", "gpt-5.6-sol", "medium"),
                ("Junior", "gpt-5.6-luna", "high"),
                ("Explorer", "gpt-5.6-luna", "-"),
            ]
        );
    }
}
