//! Best-effort summary of the agents in a generated settings object.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// Primary agent names shown before configured subagents.
pub(crate) const PRIMARY_AGENTS: [&str; 2] = ["Orchestrator", "Solo"];

/// One row of the agent summary table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentEntry {
    /// Agent name.
    pub(crate) name: String,
    /// Configured model, or `-` when absent.
    pub(crate) model: String,
    /// Displayed reasoning setting: effort level or Auto routing tier.
    pub(crate) effort: String,
    /// Configured context tier, or `-` when absent.
    pub(crate) context_tier: String,
}

/// Returns a string setting or `default` when it is absent or not a string.
pub(crate) fn string_field(object: &Map<String, Value>, key: &str, default: &str) -> String {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn value_string_field(value: &Value, key: &str, default: &str) -> String {
    value
        .as_object()
        .map(|object| string_field(object, key, default))
        .unwrap_or_else(|| default.to_owned())
}

/// The reasoning-depth value worth displaying for an agent.
///
/// An auto-routed model picks its own reasoning depth per request, so a
/// configured `effortLevel` has no visible effect; the Auto routing tier is
/// what actually governs cost/quality tradeoffs in that case.
pub(crate) fn effective_effort<'a>(
    model: &str,
    effort_level: &'a str,
    auto_tier: &'a str,
) -> &'a str {
    if model == "auto" {
        auto_tier
    } else {
        effort_level
    }
}

/// Subagent names disabled by explicit configuration or the built-in rubber
/// duck toggle.
pub(crate) fn disabled_subagents(settings: &Map<String, Value>) -> HashSet<String> {
    let mut disabled = HashSet::new();

    if let Some(subagents) = settings.get("subagents").and_then(Value::as_object)
        && let Some(names) = subagents.get("disabledSubagents").and_then(Value::as_array)
    {
        disabled.extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
    }

    let rubber_duck_disabled = settings
        .get("builtInAgents")
        .and_then(Value::as_object)
        .and_then(|built_in| built_in.get("rubberDuck"))
        .and_then(Value::as_bool)
        == Some(false);
    if rubber_duck_disabled {
        disabled.insert("rubber-duck".to_owned());
    }

    disabled
}

/// Primary agents first with the top-level model, then enabled subagents in
/// configuration order.
///
/// The effort column shows the Auto routing tier instead of `effortLevel` for
/// any agent whose effective model is `auto`. A per-agent `autoTier` overrides
/// the top-level one.
pub(crate) fn active_agent_entries(settings: &Map<String, Value>) -> Vec<AgentEntry> {
    let default_model = string_field(settings, "model", "-");
    let default_effort = string_field(settings, "effortLevel", "-");
    let default_context_tier = string_field(settings, "contextTier", "-");
    let auto_tier = string_field(settings, "autoTier", "-");

    let mut entries = Vec::with_capacity(PRIMARY_AGENTS.len());
    for name in PRIMARY_AGENTS {
        entries.push(AgentEntry {
            name: name.to_owned(),
            model: default_model.clone(),
            effort: effective_effort(&default_model, &default_effort, &auto_tier).to_owned(),
            context_tier: default_context_tier.clone(),
        });
    }

    let disabled = disabled_subagents(settings);
    let agents = settings
        .get("subagents")
        .and_then(Value::as_object)
        .and_then(|subagents| subagents.get("agents"))
        .and_then(Value::as_object);

    if let Some(agents) = agents {
        for (name, config) in agents {
            if disabled.contains(name.as_str()) || PRIMARY_AGENTS.contains(&name.as_str()) {
                continue;
            }

            let model = value_string_field(config, "model", "-");
            let effort = value_string_field(config, "effortLevel", "-");
            let agent_auto_tier = value_string_field(config, "autoTier", &auto_tier);
            let displayed_effort = effective_effort(&model, &effort, &agent_auto_tier).to_owned();
            entries.push(AgentEntry {
                name: name.clone(),
                model,
                effort: displayed_effort,
                context_tier: value_string_field(config, "contextTier", "-"),
            });
        }
    }

    entries
}

/// Formats the agent summary table with aligned columns.
pub(crate) fn format_agent_table(entries: &[AgentEntry]) -> String {
    let name_width = column_width("Agent", entries.iter().map(|entry| entry.name.as_str()));
    let model_width = column_width("Model", entries.iter().map(|entry| entry.model.as_str()));
    let effort_width = column_width("Effort", entries.iter().map(|entry| entry.effort.as_str()));
    let context_tier_width = column_width(
        "Context Tier",
        entries.iter().map(|entry| entry.context_tier.as_str()),
    );

    let mut lines = Vec::with_capacity(entries.len() + 2);
    lines.push(format!(
        "  {} │ {} │ {} │ Context Tier",
        pad("Agent", name_width),
        pad("Model", model_width),
        pad("Effort", effort_width)
    ));
    lines.push(format!(
        "  {}─┼─{}─┼─{}─┼─{}",
        "─".repeat(name_width),
        "─".repeat(model_width),
        "─".repeat(effort_width),
        "─".repeat(context_tier_width)
    ));
    for entry in entries {
        lines.push(format!(
            "  {} │ {} │ {} │ {}",
            pad(&entry.name, name_width),
            pad(&entry.model, model_width),
            pad(&entry.effort, effort_width),
            entry.context_tier
        ));
    }

    lines.join("\n")
}

fn pad(value: &str, width: usize) -> String {
    format!("{value:<width$}")
}

fn column_width<'a>(header: &str, values: impl Iterator<Item = &'a str>) -> usize {
    values
        .map(|value| value.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("value is an object").clone()
    }

    fn entry_tuples(entries: &[AgentEntry]) -> Vec<(&str, &str, &str, &str)> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.name.as_str(),
                    entry.model.as_str(),
                    entry.effort.as_str(),
                    entry.context_tier.as_str(),
                )
            })
            .collect()
    }

    #[test]
    fn includes_primary_agents_and_filters_disabled_subagents() {
        let settings = object(json!({
            "model": "gpt-5.6-sol",
            "effortLevel": "medium",
            "subagents": {
                "disabledSubagents": ["task", "code-review"],
                "agents": {
                    "Orchestrator": {"model": "ignored"},
                    "Solo": {"model": "ignored"},
                    "Junior": {
                        "model": "gpt-5.6-luna",
                        "effortLevel": "high",
                        "contextTier": "long_context",
                    },
                    "explore": {"model": "gpt-5.6-luna"},
                    "task": {"model": "gpt-5.6-luna"},
                    "code-review": {"model": "gpt-5.6-luna"},
                    "rubber-duck": {"model": "gpt-5.6-luna"},
                },
            },
            "builtInAgents": {"rubberDuck": false},
        }));

        let entries = active_agent_entries(&settings);

        assert_eq!(
            entry_tuples(&entries),
            [
                ("Orchestrator", "gpt-5.6-sol", "medium", "-"),
                ("Solo", "gpt-5.6-sol", "medium", "-"),
                ("Junior", "gpt-5.6-luna", "high", "long_context"),
                ("explore", "gpt-5.6-luna", "-", "-"),
            ]
        );
    }

    #[test]
    fn shows_auto_tier_instead_of_effort_level_for_auto_model() {
        let settings = object(json!({
            "model": "auto",
            "effortLevel": "medium",
            "autoTier": "balance",
            "subagents": {
                "agents": {
                    "Orchestrator": {"model": "ignored"},
                    "Solo": {"model": "ignored"},
                    "Junior": {"model": "gpt-5.6-luna", "effortLevel": "high"},
                },
            },
        }));

        let entries = active_agent_entries(&settings);

        assert_eq!(
            entry_tuples(&entries),
            [
                ("Orchestrator", "auto", "balance", "-"),
                ("Solo", "auto", "balance", "-"),
                ("Junior", "gpt-5.6-luna", "high", "-"),
            ]
        );
    }

    #[test]
    fn prefers_a_per_agent_auto_tier_over_the_top_level_one() {
        let settings = object(json!({
            "model": "auto",
            "effortLevel": "medium",
            "autoTier": "balance",
            "subagents": {
                "agents": {
                    "Junior": {
                        "model": "auto",
                        "effortLevel": "low",
                        "autoTier": "efficiency",
                    },
                },
            },
        }));

        let entries = active_agent_entries(&settings);

        assert_eq!(
            entry_tuples(&entries),
            [
                ("Orchestrator", "auto", "balance", "-"),
                ("Solo", "auto", "balance", "-"),
                ("Junior", "auto", "efficiency", "-"),
            ]
        );
    }

    #[test]
    fn ignores_non_string_disabled_names_and_respects_enabled_rubber_duck() {
        let settings = object(json!({
            "subagents": {
                "disabledSubagents": ["disabled", 1, null],
                "agents": {
                    "disabled": {"model": "x"},
                    "enabled": {"model": "y"},
                    "rubber-duck": {"model": "z"},
                },
            },
            "builtInAgents": {"rubberDuck": true},
        }));

        let entries = active_agent_entries(&settings);

        assert_eq!(
            entry_tuples(&entries),
            [
                ("Orchestrator", "-", "-", "-"),
                ("Solo", "-", "-", "-"),
                ("enabled", "y", "-", "-"),
                ("rubber-duck", "z", "-", "-"),
            ]
        );
    }

    #[test]
    fn uses_effort_level_for_a_pinned_model() {
        assert_eq!(effective_effort("gpt-5.6-sol", "high", "balance"), "high");
    }

    #[test]
    fn uses_auto_tier_for_the_auto_model() {
        assert_eq!(effective_effort("auto", "high", "balance"), "balance");
    }

    #[test]
    fn table_aligns_columns_and_leaves_the_last_column_unpadded() {
        let entries = vec![
            AgentEntry {
                name: "Orchestrator".to_owned(),
                model: "auto".to_owned(),
                effort: "balance".to_owned(),
                context_tier: "-".to_owned(),
            },
            AgentEntry {
                name: "Junior".to_owned(),
                model: "auto".to_owned(),
                effort: "efficiency".to_owned(),
                context_tier: "-".to_owned(),
            },
        ];

        assert_eq!(
            format_agent_table(&entries),
            concat!(
                "  Agent        │ Model │ Effort     │ Context Tier\n",
                "  ─────────────┼───────┼────────────┼─────────────\n",
                "  Orchestrator │ auto  │ balance    │ -\n",
                "  Junior       │ auto  │ efficiency │ -",
            )
        );
    }
}
