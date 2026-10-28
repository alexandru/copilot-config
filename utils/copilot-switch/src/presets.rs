//! Preset inheritance, metadata filtering, and help text.

use serde_json::{Map, Value};

use crate::config::ConfigError;
use crate::merge::merge_objects;

/// Returns the parent names configured by a preset's `extends` field.
///
/// An absent or null field means no parents. A present field must otherwise be
/// an array of preset-name strings.
///
/// # Errors
///
/// Returns [`ConfigError::InvalidExtends`] when the field has the wrong shape.
pub(crate) fn extends_names(
    preset: &Map<String, Value>,
    name: &str,
) -> Result<Vec<String>, ConfigError> {
    let Some(extends) = preset.get("extends") else {
        return Ok(Vec::new());
    };
    if extends.is_null() {
        return Ok(Vec::new());
    }

    let invalid = || ConfigError::InvalidExtends {
        name: name.to_owned(),
    };
    let items = extends.as_array().ok_or_else(invalid)?;
    let mut names = Vec::with_capacity(items.len());
    for item in items {
        let item = item.as_str().ok_or_else(invalid)?;
        names.push(item.to_owned());
    }
    Ok(names)
}

/// Removes the switcher's own keys from a preset before merging it.
pub(crate) fn strip_preset_metadata(preset: &Map<String, Value>) -> Map<String, Value> {
    let mut stripped = Map::new();
    for (key, value) in preset {
        if key == "common" || key == "extends" {
            continue;
        }
        stripped.insert(key.clone(), value.clone());
    }
    stripped
}

/// Resolves a preset with every `extends` parent merged in.
///
/// Later parents override earlier ones, and the preset overrides all parents.
/// `ancestors` is the current resolution path, used to report cycles.
///
/// # Errors
///
/// Returns [`ConfigError`] when the preset or a parent is missing or not an
/// object, when `extends` is invalid, or when inheritance is circular.
pub(crate) fn resolve_preset(
    presets: &Map<String, Value>,
    name: &str,
    ancestors: &[String],
) -> Result<Map<String, Value>, ConfigError> {
    let preset = match presets.get(name) {
        None | Some(Value::Null) => {
            return Err(ConfigError::MissingPreset {
                name: name.to_owned(),
            });
        }
        Some(Value::Object(preset)) => preset,
        Some(_) => {
            return Err(ConfigError::NonObjectPreset {
                name: name.to_owned(),
            });
        }
    };

    if let Some(index) = ancestors.iter().position(|ancestor| ancestor == name) {
        let mut chain = ancestors[index..].to_vec();
        chain.push(name.to_owned());
        return Err(ConfigError::CircularInheritance {
            chain: chain.join(" -> "),
        });
    }

    let parents = extends_names(preset, name)?;
    let mut next_ancestors = Vec::with_capacity(ancestors.len() + 1);
    next_ancestors.extend_from_slice(ancestors);
    next_ancestors.push(name.to_owned());

    let mut inherited = Map::new();
    for parent in parents {
        let resolved = resolve_preset(presets, &parent, &next_ancestors)?;
        inherited = merge_objects(&inherited, &resolved);
    }

    Ok(merge_objects(&inherited, &strip_preset_metadata(preset)))
}

/// Preset names selectable from the command line, in document order.
///
/// Fragments marked `"common": true` are hidden from help but remain selectable
/// when named explicitly.
pub(crate) fn visible_preset_names(presets: &Map<String, Value>) -> Vec<&str> {
    presets
        .iter()
        .filter(|(_, preset)| !is_common_fragment(preset))
        .map(|(name, _)| name.as_str())
        .collect()
}

fn is_common_fragment(preset: &Value) -> bool {
    preset
        .as_object()
        .and_then(|object| object.get("common"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Formats the visible preset names as help-list lines.
pub(crate) fn format_presets(presets: &Map<String, Value>) -> String {
    visible_preset_names(presets)
        .into_iter()
        .map(|name| format!("  - {name}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The dynamic help text, including the visible preset names.
pub(crate) fn help_text(presets: &Map<String, Value>) -> String {
    format!(
        "Copilot CLI configuration preset switcher\n\n\
         Usage: copilot-switch <preset-name>\n\n\
         Available presets:\n{}\n",
        format_presets(presets)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("value is an object").clone()
    }

    #[test]
    fn applies_parents_in_order_and_removes_metadata() {
        let presets = object(json!({
            "base": {"common": true, "value": "base", "base_only": true},
            "second": {"common": true, "value": "second"},
            "selected": {"extends": ["base", "second"], "value": "selected"},
        }));

        let resolved = resolve_preset(&presets, "selected", &[]).expect("preset should resolve");

        assert_eq!(
            Value::Object(resolved),
            json!({"value": "selected", "base_only": true})
        );
    }

    #[test]
    fn later_parents_override_earlier_parents() {
        let presets = object(json!({
            "base": {"value": "base", "base_only": true},
            "second": {"value": "second", "second_only": true},
            "selected": {"extends": ["base", "second"], "value": "selected"},
        }));

        let resolved = resolve_preset(&presets, "selected", &[]).expect("preset should resolve");

        assert_eq!(
            Value::Object(resolved),
            json!({
                "value": "selected",
                "base_only": true,
                "second_only": true,
            })
        );
    }

    #[test]
    fn rejects_inheritance_cycles() {
        let presets = object(json!({
            "first": {"extends": ["second"]},
            "second": {"extends": ["first"]},
        }));

        let error = resolve_preset(&presets, "first", &[]).expect_err("cycle must be rejected");

        assert_eq!(
            error.to_string(),
            "Circular preset inheritance detected: first -> second -> first"
        );
    }

    #[test]
    fn reports_the_whole_cycle_chain() {
        let presets = object(json!({
            "a": {"extends": ["b"]},
            "b": {"extends": ["c"]},
            "c": {"extends": ["a"]},
        }));

        let error = resolve_preset(&presets, "a", &[]).expect_err("cycle must be rejected");

        assert_eq!(
            error.to_string(),
            "Circular preset inheritance detected: a -> b -> c -> a"
        );
    }

    #[test]
    fn rejects_invalid_extends_values() {
        let invalid_documents = [
            json!({"selected": {"extends": "base"}}),
            json!({"selected": {"extends": ["base", 1]}}),
            json!({"selected": {"extends": [null]}}),
        ];

        for document in invalid_documents {
            let presets = object(document);
            let error = resolve_preset(&presets, "selected", &[])
                .expect_err("invalid extends must be rejected");
            assert_eq!(
                error.to_string(),
                "Preset 'selected' has invalid 'extends'; expected an array of preset names"
            );
        }
    }

    #[test]
    fn treats_a_null_extends_as_no_parents() {
        let presets = object(json!({"selected": {"extends": null, "value": true}}));

        let resolved = resolve_preset(&presets, "selected", &[]).expect("null extends is empty");

        assert_eq!(Value::Object(resolved), json!({"value": true}));
    }

    #[test]
    fn rejects_missing_non_object_and_null_presets() {
        let presets = object(json!({
            "scalar": 1,
            "null": null,
            "selected": {"extends": ["missing"]},
        }));

        let missing =
            resolve_preset(&presets, "missing", &[]).expect_err("missing preset must fail");
        assert_eq!(
            missing.to_string(),
            "Preset 'missing' not found in config.presets.jsonc"
        );

        let scalar = resolve_preset(&presets, "scalar", &[]).expect_err("scalar preset must fail");
        assert_eq!(scalar.to_string(), "Preset 'scalar' must be an object");

        let null = resolve_preset(&presets, "null", &[]).expect_err("null preset must fail");
        assert_eq!(
            null.to_string(),
            "Preset 'null' not found in config.presets.jsonc"
        );

        let parent =
            resolve_preset(&presets, "selected", &[]).expect_err("missing parent must fail");
        assert_eq!(
            parent.to_string(),
            "Preset 'missing' not found in config.presets.jsonc"
        );
    }

    #[test]
    fn stripping_metadata_only_removes_top_level_switcher_keys() {
        let preset = object(json!({
            "common": true,
            "extends": ["base"],
            "nested": {"common": true, "value": 1},
        }));

        let stripped = strip_preset_metadata(&preset);

        assert_eq!(
            Value::Object(stripped),
            json!({"nested": {"common": true, "value": 1}})
        );
    }

    #[test]
    fn common_fragments_are_hidden_but_other_presets_remain_visible() {
        let presets = object(json!({
            "fragment": {"common": true},
            "visible": {"common": false},
            "plain": {},
            "scalar": "preset",
        }));

        assert_eq!(
            visible_preset_names(&presets),
            ["visible", "plain", "scalar"]
        );
        assert_eq!(
            format_presets(&presets),
            "  - visible\n  - plain\n  - scalar"
        );
    }

    #[test]
    fn help_lists_visible_presets() {
        let presets = object(json!({
            "hidden": {"common": true},
            "work": {},
        }));

        assert_eq!(
            help_text(&presets),
            "Copilot CLI configuration preset switcher\n\n\
             Usage: copilot-switch <preset-name>\n\n\
             Available presets:\n  - work\n"
        );
    }
}
