//! Output generation, writing, and the successful command summary.

use std::path::Path;

use serde_json::{Map, Value};

use crate::agents::{active_agent_entries, effective_effort, format_agent_table, string_field};
use crate::config::{self, ConfigError, OUTPUT_CONFIGS, PRESETS_FILE};
use crate::merge::merge_objects;
use crate::presets::{format_presets, help_text, resolve_preset};

/// One generated output file and its merged JSON object.
pub(crate) type GeneratedOutput = (String, Map<String, Value>);

/// Runs the switcher for one configuration directory.
///
/// With no preset, returns the dynamic help text without writing anything.
/// Otherwise the full output set is computed before any file is written, so an
/// invalid preset cannot leave a partially generated configuration behind.
///
/// # Errors
///
/// Returns [`ConfigError`] for unreadable or invalid configuration, invalid
/// inheritance, a missing output object, or a failed write.
pub(crate) fn run(config_dir: &Path, preset_name: Option<&str>) -> Result<String, ConfigError> {
    let presets = config::read_jsonc_object(&config_dir.join(PRESETS_FILE))?;
    let Some(preset_name) = preset_name else {
        return Ok(help_text(&presets));
    };

    if !presets.contains_key(preset_name) {
        return Err(ConfigError::UnknownPreset {
            name: preset_name.to_owned(),
            available: format_presets(&presets),
        });
    }

    let preset = resolve_preset(&presets, preset_name, &[])?;
    let outputs = generate_outputs(&preset, preset_name, config_dir)?;

    for (output_name, output) in &outputs {
        config::write_json_object(&config_dir.join(output_name), output)?;
    }

    success_output(&outputs, preset_name)
}

/// Computes every output without writing it.
///
/// The selected preset must define each output name as an object. Each output
/// is the corresponding shared fragment merged with the preset's object.
///
/// # Errors
///
/// Returns [`ConfigError`] when an output object is missing or a shared
/// fragment cannot be read.
pub(crate) fn generate_outputs(
    preset: &Map<String, Value>,
    preset_name: &str,
    config_dir: &Path,
) -> Result<Vec<GeneratedOutput>, ConfigError> {
    let mut outputs = Vec::with_capacity(OUTPUT_CONFIGS.len());

    for (output_name, common_file) in OUTPUT_CONFIGS {
        let preset_config = preset
            .get(output_name)
            .and_then(Value::as_object)
            .ok_or_else(|| ConfigError::MissingPresetOutput {
                preset: preset_name.to_owned(),
                output: output_name.to_owned(),
            })?;
        let common = config::read_jsonc_object(&config_dir.join(common_file))?;
        outputs.push((
            output_name.to_owned(),
            merge_objects(&common, preset_config),
        ));
    }

    Ok(outputs)
}

fn generated_output<'a>(
    outputs: &'a [GeneratedOutput],
    name: &str,
) -> Result<&'a Map<String, Value>, ConfigError> {
    outputs
        .iter()
        .find_map(|(output_name, config)| (output_name == name).then_some(config))
        .ok_or_else(|| ConfigError::MissingGeneratedOutput {
            name: name.to_owned(),
        })
}

/// Builds the exact successful command summary printed after the writes.
fn success_output(outputs: &[GeneratedOutput], preset_name: &str) -> Result<String, ConfigError> {
    let settings = generated_output(outputs, "settings.json")?;
    let mut output = String::new();

    output.push_str(&format!("Successfully switched to preset: {preset_name}\n"));
    let generated = outputs
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    output.push_str(&format!("Generated {generated}\n\n"));

    let model = settings
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty());
    if let Some(model) = model {
        let effort = string_field(settings, "effortLevel", "default");
        let auto_tier = string_field(settings, "autoTier", "default");
        output.push_str(&format!(
            "Default model: {model} ({})\n\n",
            effective_effort(model, &effort, &auto_tier)
        ));
    }

    let entries = active_agent_entries(settings);
    if !entries.is_empty() {
        output.push_str("Agents:\n\n");
        output.push_str(&format_agent_table(&entries));
        output.push('\n');
    }
    output.push('\n');

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("value is an object").clone()
    }

    #[test]
    fn rejects_presets_without_an_output_object() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        for name in ["settings.common.jsonc", "mcp-config.common.jsonc"] {
            std::fs::write(directory.path().join(name), "{}").expect("write common fragment");
        }
        let preset = object(json!({
            "settings.json": {},
            "mcp-config.json": "not-an-object",
        }));

        let error = generate_outputs(&preset, "broken", directory.path())
            .expect_err("invalid output must be rejected");

        assert_eq!(
            error.to_string(),
            "Preset 'broken' must define 'mcp-config.json' as an object"
        );
    }

    #[test]
    fn computes_outputs_in_configured_order_before_writing() {
        let directory = tempfile::tempdir().expect("create temporary directory");
        std::fs::write(
            directory.path().join("settings.common.jsonc"),
            r#"{ "kept": true, "nested": { "value": 1 } }"#,
        )
        .expect("write settings fragment");
        std::fs::write(
            directory.path().join("mcp-config.common.jsonc"),
            r#"{ "mcpServers": {} }"#,
        )
        .expect("write mcp fragment");
        let preset = object(json!({
            "mcp-config.json": {"mcpServers": {"server": {}}},
            "settings.json": {"nested": {"value": 2}},
        }));

        let outputs = generate_outputs(&preset, "selected", directory.path())
            .expect("outputs should resolve");

        let names: Vec<&str> = outputs.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["settings.json", "mcp-config.json"]);
        assert_eq!(
            Value::Object(outputs[0].1.clone()),
            json!({"kept": true, "nested": {"value": 2}})
        );
        assert!(!directory.path().join("settings.json").exists());
        assert!(!directory.path().join("mcp-config.json").exists());
    }

    #[test]
    fn missing_generated_output_is_reported() {
        let outputs = vec![("settings.json".to_owned(), Map::new())];

        let error = generated_output(&outputs, "mcp-config.json")
            .expect_err("missing output must be rejected");

        assert_eq!(
            error.to_string(),
            "generated output missing: mcp-config.json"
        );
    }
}
