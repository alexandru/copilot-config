#!/usr/bin/env -S pipx run

# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "json5==0.17.3",
#   "typer==0.27.2",
# ]
# ///

"""Generate Copilot CLI's configuration from a preset.

Merges the selected preset from config.presets.jsonc into settings.common.jsonc
and mcp-config.common.jsonc, and writes settings.json and mcp-config.json.
Presets inherit from other presets via "extends"; fragments marked
"common": true stay hidden from direct selection.
"""

import json
import sys
from collections.abc import Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Annotated, TypeAlias

import json5
import typer

PRESETS_FILE = "config.presets.jsonc"
OUTPUT_CONFIGS: tuple[tuple[str, str], ...] = (
    ("settings.json", "settings.common.jsonc"),
    ("mcp-config.json", "mcp-config.common.jsonc"),
)
PRIMARY_AGENTS: tuple[str, ...] = ("Orchestrator", "Solo")

CONFIG_DIR = Path(__file__).resolve().parent.parent

Json: TypeAlias = dict[str, "Json"] | list["Json"] | str | int | float | bool | None
JsonObject: TypeAlias = dict[str, Json]


class ConfigError(Exception):
    """A configuration problem, reported as one clear message, not a traceback."""


def merge_objects(source: JsonObject, target: JsonObject) -> JsonObject:
    """Target values win, shared objects merge recursively, a target null keeps
    the source object, and the result keeps the source's key order, then keys
    new in the target.
    """
    merged = dict(source)
    for key, target_value in target.items():
        source_value = merged.get(key)
        if isinstance(source_value, dict) and target_value is None:
            merged[key] = source_value
        elif isinstance(source_value, dict) and isinstance(target_value, dict):
            merged[key] = merge_objects(source_value, target_value)
        else:
            merged[key] = target_value
    return merged


def extends_names(preset: JsonObject, name: str) -> list[str]:
    extends = preset.get("extends")
    if extends is None:
        return []
    invalid = (
        f"Preset '{name}' has invalid 'extends'; expected an array of preset names"
    )
    if not isinstance(extends, list):
        raise ConfigError(invalid)
    names: list[str] = []
    for item in extends:
        if not isinstance(item, str):
            raise ConfigError(invalid)
        names.append(item)
    return names


def strip_preset_metadata(preset: JsonObject) -> JsonObject:
    """The preset as switcher input, without the switcher's own keys."""
    return {
        key: value for key, value in preset.items() if key not in ("common", "extends")
    }


def resolve_preset(
    presets: JsonObject, name: str, ancestors: tuple[str, ...]
) -> JsonObject:
    """The preset with every "extends" parent merged in.

    Later parents override earlier ones, and the preset overrides its parents.
    `ancestors` is the resolution path, so inheritance cycles can be reported.
    """
    preset = presets.get(name)
    if preset is None:
        raise ConfigError(f"Preset '{name}' not found in {PRESETS_FILE}")
    if not isinstance(preset, dict):
        raise ConfigError(f"Preset '{name}' must be an object")
    if name in ancestors:
        cycle = ancestors[ancestors.index(name) :] + (name,)
        raise ConfigError("Circular preset inheritance detected: " + " -> ".join(cycle))

    inherited: JsonObject = {}
    for parent in extends_names(preset, name):
        inherited = merge_objects(
            inherited, resolve_preset(presets, parent, (*ancestors, name))
        )
    return merge_objects(inherited, strip_preset_metadata(preset))


def read_jsonc_object(path: Path) -> JsonObject:
    """The root object of a JSONC file; configuration files must be objects."""
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise ConfigError(f"configuration file missing: {path}") from None
    except OSError as error:
        raise ConfigError(f"failed to read {path}: {error}") from None
    try:
        value = json5.loads(content)
    except ValueError as error:
        raise ConfigError(f"failed to parse {path}: {error}") from None
    if not isinstance(value, dict):
        raise ConfigError(f"{path} must contain a JSON object")
    return value


def write_json(path: Path, value: Json) -> None:
    """Pretty-printed JSON with a trailing newline."""
    content = json.dumps(value, indent=2, ensure_ascii=False) + "\n"
    try:
        path.write_text(content, encoding="utf-8")
    except OSError as error:
        raise ConfigError(f"failed to write {path}: {error}") from None


def generate_outputs(
    preset: JsonObject, preset_name: str
) -> list[tuple[str, JsonObject]]:
    """Every output fully computed before the caller writes anything, so an
    invalid preset cannot leave a partially generated configuration behind.
    """
    outputs: list[tuple[str, JsonObject]] = []
    for output_name, common_file in OUTPUT_CONFIGS:
        preset_config = preset.get(output_name)
        if not isinstance(preset_config, dict):
            raise ConfigError(
                f"Preset '{preset_name}' must define '{output_name}' as an object"
            )
        common = read_jsonc_object(CONFIG_DIR / common_file)
        outputs.append((output_name, merge_objects(common, preset_config)))
    return outputs


def generated_output(outputs: list[tuple[str, JsonObject]], name: str) -> JsonObject:
    for output_name, config in outputs:
        if output_name == name:
            return config
    raise ConfigError(f"generated output missing: {name}")


def visible_preset_names(presets: JsonObject) -> Iterator[str]:
    """Presets selectable on the command line; "common": true fragments only get inherited."""
    for name, preset in presets.items():
        if not (isinstance(preset, dict) and preset.get("common") is True):
            yield name


def format_presets(presets: JsonObject) -> str:
    return "\n".join(f"  - {name}" for name in visible_preset_names(presets))


def print_help(presets: JsonObject) -> None:
    print("Copilot CLI configuration preset switcher\n")
    print("Usage: copilot-switch <preset-name>\n")
    print(f"Available presets:\n{format_presets(presets)}")


def string_field(config: Json, key: str, default: str) -> str:
    value = config.get(key) if isinstance(config, dict) else None
    return value if isinstance(value, str) else default


@dataclass(frozen=True)
class AgentEntry:
    name: str
    model: str
    effort: str
    context_tier: str


def disabled_subagents(settings: JsonObject) -> set[str]:
    """Subagent names disabled by explicit config or the built-in rubber duck toggle."""
    disabled: set[str] = set()
    subagents = settings.get("subagents")
    if isinstance(subagents, dict):
        names = subagents.get("disabledSubagents")
        if isinstance(names, list):
            disabled = {name for name in names if isinstance(name, str)}
    built_in = settings.get("builtInAgents")
    if isinstance(built_in, dict) and built_in.get("rubberDuck") is False:
        disabled.add("rubber-duck")
    return disabled


def active_agent_entries(settings: JsonObject) -> list[AgentEntry]:
    """Primary agents first with the top-level model, then enabled subagents in
    configuration order.
    """
    default_model = string_field(settings, "model", "-")
    default_effort = string_field(settings, "effortLevel", "-")
    default_context_tier = string_field(settings, "contextTier", "-")
    entries = [
        AgentEntry(
            name=name,
            model=default_model,
            effort=default_effort,
            context_tier=default_context_tier,
        )
        for name in PRIMARY_AGENTS
    ]

    subagents = settings.get("subagents")
    agents = subagents.get("agents") if isinstance(subagents, dict) else None
    disabled = disabled_subagents(settings)
    if isinstance(agents, dict):
        for name, config in agents.items():
            if name not in disabled and name not in PRIMARY_AGENTS:
                entries.append(
                    AgentEntry(
                        name=name,
                        model=string_field(config, "model", "-"),
                        effort=string_field(config, "effortLevel", "-"),
                        context_tier=string_field(config, "contextTier", "-"),
                    )
                )
    return entries


def column_width(header: str, values: Iterable[str]) -> int:
    return max([len(header), *(len(value) for value in values)])


def print_agent_table(entries: list[AgentEntry]) -> None:
    name_width = column_width("Agent", (entry.name for entry in entries))
    model_width = column_width("Model", (entry.model for entry in entries))
    effort_width = column_width("Effort", (entry.effort for entry in entries))
    context_tier_width = column_width(
        "Context Tier", (entry.context_tier for entry in entries)
    )

    print(
        f"  {'Agent':<{name_width}} │ {'Model':<{model_width}} │ "
        f"{'Effort':<{effort_width}} │ Context Tier"
    )
    print(
        f"  {'─' * name_width}─┼─{'─' * model_width}─┼─"
        f"{'─' * effort_width}─┼─{'─' * context_tier_width}"
    )
    for entry in entries:
        print(
            f"  {entry.name:<{name_width}} │ {entry.model:<{model_width}} │ "
            f"{entry.effort:<{effort_width}} │ {entry.context_tier}"
        )


def switch(preset_name: str | None) -> None:
    """Generates Copilot CLI's configuration from the selected preset, or prints help."""
    presets = read_jsonc_object(CONFIG_DIR / PRESETS_FILE)
    if preset_name is None:
        print_help(presets)
        return
    if preset_name not in presets:
        raise ConfigError(
            f"Preset '{preset_name}' not found. Available presets:\n"
            f"{format_presets(presets)}"
        )
    preset = resolve_preset(presets, preset_name, ())
    outputs = generate_outputs(preset, preset_name)

    for output_name, config in outputs:
        write_json(CONFIG_DIR / output_name, config)

    print(f"Successfully switched to preset: {preset_name}")
    print("Generated " + " and ".join(name for name, _ in outputs))
    print()

    settings = generated_output(outputs, "settings.json")
    model = settings.get("model")
    if isinstance(model, str) and model:
        effort = string_field(settings, "effortLevel", "default")
        print(f"Default model: {model} ({effort})")
        print()

    entries = active_agent_entries(settings)
    if entries:
        print("Agents:")
        print()
        print_agent_table(entries)
    print()


app = typer.Typer(add_completion=False, context_settings={"help_option_names": []})


@app.command()
def main(
    preset: Annotated[
        str | None, typer.Argument(metavar="PRESET", help="Preset to apply")
    ] = None,
    show_help: Annotated[bool, typer.Option("--help", "-h", help="Print help")] = False,
) -> None:
    """Generate a Copilot CLI configuration from a preset."""
    try:
        switch(None if show_help else preset)
    except ConfigError as error:
        print(f"Error: {error}", file=sys.stderr)
        raise typer.Exit(code=1) from None


if __name__ == "__main__":
    app()
