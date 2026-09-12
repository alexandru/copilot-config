#!/usr/bin/env node

const fs = require("node:fs");
const path = require("node:path");
const { parse } = require("comment-json");

const root = path.resolve(__dirname, "..");
const presetsFile = path.join(root, "config.presets.jsonc");
const outputConfigs = {
  "settings.json": path.join(root, "settings.common.jsonc"),
  "mcp-config.json": path.join(root, "mcp-config.common.jsonc"),
};
const presetMetadataKeys = new Set(["common", "extends"]);
const primaryAgents = ["Orchestrator", "Solo"];

function fail(message) {
  console.error(`Error: ${message}`);
  process.exit(1);
}

function loadJson(file) {
  try {
    return parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    fail(`Failed to read ${path.relative(root, file)}: ${error.message}`);
  }
}

function deepMerge(source, target) {
  if (target === null || target === undefined) return source;
  if (source === null || source === undefined) return target;
  if (Array.isArray(target)) return target;
  if (typeof target !== "object") return target;
  if (typeof source !== "object" || Array.isArray(source)) return target;

  const result = { ...source };
  for (const [key, value] of Object.entries(target)) {
    if (
      Object.prototype.hasOwnProperty.call(result, key) &&
      typeof result[key] === "object" &&
      !Array.isArray(result[key]) &&
      typeof value === "object" &&
      value !== null &&
      !Array.isArray(value)
    ) {
      result[key] = deepMerge(result[key], value);
    } else {
      result[key] = value;
    }
  }
  return result;
}

function stripPresetMetadata(config) {
  const result = { ...config };
  for (const key of presetMetadataKeys) delete result[key];
  return result;
}

function resolvePreset(presets, presetName, seen = []) {
  if (!Object.prototype.hasOwnProperty.call(presets, presetName)) {
    fail(`Preset '${presetName}' not found in config.presets.jsonc`);
  }
  if (seen.includes(presetName)) {
    fail(`Circular preset inheritance detected: ${seen.concat(presetName).join(" -> ")}`);
  }

  const preset = presets[presetName];
  if (!preset || typeof preset !== "object" || Array.isArray(preset)) {
    fail(`Preset '${presetName}' must be an object`);
  }

  const parentNames = preset.extends;
  if (parentNames === undefined) return stripPresetMetadata(preset);
  if (!Array.isArray(parentNames) || parentNames.some((name) => typeof name !== "string")) {
    fail(`Preset '${presetName}' has invalid 'extends'; expected an array of preset names`);
  }

  const nextSeen = seen.concat(presetName);
  const inherited = parentNames.reduce(
    (merged, parentName) => deepMerge(merged, resolvePreset(presets, parentName, nextSeen)),
    {},
  );
  return stripPresetMetadata(deepMerge(inherited, preset));
}

function listPresets(presets) {
  return Object.keys(presets)
    .filter((name) => presets[name]?.common !== true)
    .map((name) => `  - ${name}`)
    .join("\n");
}

function activeAgentEntries(settings) {
  const disabled = new Set(settings.subagents?.disabledSubagents ?? []);
  if (settings.builtInAgents?.rubberDuck === false) disabled.add("rubber-duck");

  const primaryEntries = primaryAgents.map((name) => [
    name,
    { model: settings.model, effortLevel: settings.effortLevel },
  ]);
  const subagentEntries = Object.entries(settings.subagents?.agents ?? {}).filter(
    ([name]) => !disabled.has(name) && !primaryAgents.includes(name),
  );
  return primaryEntries.concat(subagentEntries);
}

function printAgentTable(entries) {
  if (entries.length === 0) return;

  const agentWidth = Math.max("Agent".length, ...entries.map(([name]) => name.length));
  const modelWidth = Math.max(
    "Model".length,
    ...entries.map(([, config]) => (config.model ?? "-").length),
  );
  const effortWidth = Math.max(
    "Effort".length,
    ...entries.map(([, config]) => (config.effortLevel ?? "-").length),
  );
  const pad = (value, width) => value.padEnd(width);

  console.log(`  ${pad("Agent", agentWidth)} │ ${pad("Model", modelWidth)} │ Effort`);
  console.log(
    `  ${"─".repeat(agentWidth)}─┼─${"─".repeat(modelWidth)}─┼─${"─".repeat(effortWidth)}`,
  );
  for (const [name, config] of entries) {
    console.log(
      `  ${pad(name, agentWidth)} │ ${pad(config.model ?? "-", modelWidth)} │ ${config.effortLevel ?? "-"}`,
    );
  }
}

function main() {
  const presets = loadJson(presetsFile);
  const presetName = process.argv[2];

  if (!presetName) {
    console.log("Copilot CLI configuration preset switcher\n");
    console.log("Usage: copilot-switch <preset-name>\n");
    console.log(`Available presets:\n${listPresets(presets)}`);
    return;
  }

  if (!Object.prototype.hasOwnProperty.call(presets, presetName)) {
    fail(`Preset '${presetName}' not found. Available presets:\n${listPresets(presets)}`);
  }

  const preset = resolvePreset(presets, presetName);
  const configs = Object.fromEntries(
    Object.entries(outputConfigs).map(([outputName, commonFile]) => {
      const config = preset[outputName];
      if (!config || typeof config !== "object" || Array.isArray(config)) {
        fail(`Preset '${presetName}' must define '${outputName}' as an object`);
      }
      return [outputName, deepMerge(loadJson(commonFile), config)];
    }),
  );

  for (const [outputName, config] of Object.entries(configs)) {
    fs.writeFileSync(path.join(root, outputName), `${JSON.stringify(config, null, 2)}\n`);
  }

  console.log(`Successfully switched to preset: ${presetName}`);
  console.log(`Generated ${Object.keys(configs).join(" and ")}`);
  console.log("");
  const settings = configs["settings.json"];
  if (settings.model) {
    console.log(`Default model: ${settings.model} (${settings.effortLevel ?? "default"})`);
    console.log("");
  }
  const agentEntries = activeAgentEntries(settings);
  if (agentEntries.length > 0) {
    console.log("Subagents:");
    console.log("");
    printAgentTable(agentEntries);
  }
  console.log("");
}

main();
