#!/usr/bin/env -S pipx run

# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "json5==0.17.3",
#   "typer==0.27.2",
# ]
# ///

"""Unit tests for copilot-switch. Run: make test-copilot-switch

The pins mirror copilot-switch.py's PEP 723 metadata, because importing the
tool module imports those packages.
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import ModuleType
from typing import Any

TOOL_PATH = Path(__file__).resolve().parent / "copilot-switch.py"


def load_tool() -> ModuleType:
    """Imports copilot-switch.py; its dashed file name needs an explicit file load."""
    spec = importlib.util.spec_from_file_location("copilot_switch", TOOL_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot import {TOOL_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


tool: Any = load_tool()


class MergeTest(unittest.TestCase):
    def test_merges_objects_and_replaces_arrays_and_primitives(self) -> None:
        source = {
            "nested": {"kept": True, "overridden": "old"},
            "items": [1, 2],
            "primitive": "old",
            "retained": {"value": 1},
        }
        target = {
            "nested": {"overridden": "new", "added": True},
            "items": [3],
            "primitive": 42,
            "retained": None,
        }

        merged = tool.merge_objects(source, target)

        self.assertEqual(
            merged,
            {
                "nested": {"kept": True, "overridden": "new", "added": True},
                "items": [3],
                "primitive": 42,
                "retained": {"value": 1},
            },
        )

    def test_null_targets_keep_source_objects_and_replace_scalars(self) -> None:
        source = {"nested": {"kept": True}, "replaced": "source"}
        target = {"nested": None, "replaced": None}

        merged = tool.merge_objects(source, target)

        self.assertEqual(merged, {"nested": {"kept": True}, "replaced": None})


class ResolvePresetTest(unittest.TestCase):
    def test_applies_parents_in_order_and_removes_metadata(self) -> None:
        presets = {
            "base": {"common": True, "value": "base", "base_only": True},
            "second": {"common": True, "value": "second"},
            "selected": {"extends": ["base", "second"], "value": "selected"},
        }
        self.assertEqual(
            tool.resolve_preset(presets, "selected", ()),
            {"value": "selected", "base_only": True},
        )

    def test_rejects_inheritance_cycles(self) -> None:
        presets = {"first": {"extends": ["second"]}, "second": {"extends": ["first"]}}
        with self.assertRaises(tool.ConfigError) as raised:
            tool.resolve_preset(presets, "first", ())
        self.assertEqual(
            str(raised.exception),
            "Circular preset inheritance detected: first -> second -> first",
        )


class GenerateOutputsTest(unittest.TestCase):
    def test_rejects_presets_without_an_output_object(self) -> None:
        preset = {"settings.json": {}, "mcp-config.json": "not-an-object"}
        with tempfile.TemporaryDirectory() as tmp:
            for name in ("settings.common.jsonc", "mcp-config.common.jsonc"):
                (Path(tmp) / name).write_text("{}", encoding="utf-8")
            original = tool.CONFIG_DIR
            tool.CONFIG_DIR = Path(tmp)
            try:
                with self.assertRaises(tool.ConfigError) as raised:
                    tool.generate_outputs(preset, "broken")
            finally:
                tool.CONFIG_DIR = original
        self.assertEqual(
            str(raised.exception),
            "Preset 'broken' must define 'mcp-config.json' as an object",
        )


class ActiveAgentEntriesTest(unittest.TestCase):
    def test_includes_primary_agents_and_filters_disabled_subagents(self) -> None:
        settings = {
            "model": "gpt-5.6-sol",
            "effortLevel": "medium",
            "subagents": {
                "disabledSubagents": ["task", "code-review"],
                "agents": {
                    "Orchestrator": {"model": "ignored"},
                    "Solo": {"model": "ignored"},
                    "Analyst": {
                        "model": "gpt-5.6-luna",
                        "effortLevel": "high",
                        "contextTier": "long_context",
                    },
                    "Junior": {"model": "gpt-5.6-luna", "effortLevel": "high"},
                    "Explorer": {"model": "gpt-5.6-luna"},
                    "task": {"model": "gpt-5.6-luna"},
                    "code-review": {"model": "gpt-5.6-luna"},
                    "rubber-duck": {"model": "gpt-5.6-luna"},
                },
            },
            "builtInAgents": {"rubberDuck": False},
        }

        entries = tool.active_agent_entries(settings)

        self.assertEqual(
            [
                (entry.name, entry.model, entry.effort, entry.context_tier)
                for entry in entries
            ],
            [
                ("Orchestrator", "gpt-5.6-sol", "medium", "-"),
                ("Solo", "gpt-5.6-sol", "medium", "-"),
                ("Analyst", "gpt-5.6-luna", "high", "long_context"),
                ("Junior", "gpt-5.6-luna", "high", "-"),
                ("Explorer", "gpt-5.6-luna", "-", "-"),
            ],
        )


class ReadJsoncTest(unittest.TestCase):
    def test_reads_comments_and_trailing_commas(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.jsonc"
            path.write_text('{\n  // fragment\n  "enabled": true,\n}', encoding="utf-8")
            self.assertEqual(tool.read_jsonc_object(path), {"enabled": True})

    def test_rejects_non_object_roots(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.jsonc"
            path.write_text("[1]", encoding="utf-8")
            with self.assertRaises(tool.ConfigError):
                tool.read_jsonc_object(path)


if __name__ == "__main__":
    unittest.main(verbosity=2)
