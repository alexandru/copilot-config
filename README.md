# My Copilot CLI configuration

Part of [alexandru/agents-config](https://github.com/alexandru/agents-config).

## Installation

<details>
<summary>STEP 1 — Clone the repository</summary>

### Clone the repository

**WARN** — This is for a fresh Copilot installation (no history):

```sh
git clone https://github.com/alexandru/copilot-config.git ~/.copilot
```

**WARN:** This is your Copilot's working directory, so you may already have a `~/.copilot` that you may need to delete, in which case you could lose all your sessions. An alternative would be...

```sh
if [[ -d ~/.copilot ]]; then
  # Clones in temporary directory
  git clone https://github.com/alexandru/copilot-config.git /tmp/copilot-config
  echo
  # Sync all the files from clone to your working dir
  rsync -rcv /tmp/copilot-config/ ~/.copilot/
  # Doing some index cleanup
  cd ~/.copilot
  git pull
  # Cleanup
  rm -rf /tmp/copilot-config
else
  git clone https://github.com/alexandru/copilot-config.git ~/.copilot
fi
```

If the path isn't standard, you may need to set this in your `~/.zshrc`, `~/.bashrc` or `~/.profile`:
```sh
export COPILOT_HOME=/absolute/path/to/copilot-config
```
</details>

<details>
<summary>STEP 2 — Install the shared skills globally</summary>

### Install the shared skills globally

```sh
cd ~/.copilot
make install-skills
```

The skills are installed under `~/.agents/skills`, where Copilot CLI,
OpenCode, Codex, and Pi can share them.
</details>

<details>
<summary>STEP 3 — Configure Bash/Zsh</summary>

### Configure Bash/Zsh

Some configurations can't be set in Copilot's files, so 
define this useful alias in `~/.zshrc`, `~/.bashrc` or `~/.profile`...
```bash
alias copilot='command copilot --agent Orchestrator --yolo'
```
</details>

<details>
<summary>STEP 4 — Choose a configuration preset</summary>

### Choose a configuration preset

The [copilot-switch](./bin/copilot-switch.rs) utility is for quickly switching between multiple setting presets (e.g., multiple sets of models assigned to your agents). It's built with Rust 😎, so you need [rustup](https://rustup.rs/) installed. 

```sh
# Example:
./bin/copilot-switch work
```

The switcher generates `settings.json` and `mcp-config.json` (Copilot's main configuration files) from:

- [settings.common.jsonc](./settings.common.jsonc)
- [mcp-config.common.jsonc](./mcp-config.common.jsonc)
- [config.presets.jsonc](./config.presets.jsonc)

**WARNING:** Must run `copilot-switch` at least once, otherwise Copilot's configuration is missing!

</details>

<details>
<summary>STEP 5 — Install Cellar (optional)</summary>

### Install Cellar (optional)

[Cellar](https://github.com/VirtusLab/cellar) is useful for JVM dependency API lookup, and this repo's [Makefile](./Makefile) also installs its associated skill.

Install [Coursier](https://get-coursier.io/docs/cli-installation) first:

```sh
## MacOS
brew install coursier/formulas/coursier
cs setup

## Linux x86-64 (aka AMD64)
curl -fL "https://github.com/coursier/launchers/raw/master/cs-x86_64-pc-linux.gz" | gzip -d > cs

## Linux ARM64
curl -fL "https://github.com/VirtusLab/coursier-m1/releases/latest/download/cs-aarch64-pc-linux.gz" | gzip -d > cs
```

Then install Cellar via Coursier:

```sh
cs install --contrib cellar
cellar --version

# Disable telemetry
cellar telemetry disable
```
</details>

## Defined agents

Main agents:

- [Orchestrator](./agents/Orchestrator.agent.md) (default agent): designs and implements changes; delegates evidence, research, and checks.
- [Solo](./agents/Solo.agent.md): designs and implements changes with direct tool use and no delegation.

Sub-agents:

- [Junior](./agents/Junior.agent.md): bounded execution, mechanical work.
- [Explorer](./agents/Explorer.agent.md): read-only codebase evidence gathering.
- [Librarian](./agents/Librarian.agent.md): read-only external documentation and dependency-source research.

## Defined commands

- `/code-review`: review uncommitted changes, a commit, a branch, or a pull request.
- `/plan-implementation`: prepare a detailed implementation plan and save it as a Markdown specification file.
- `/grill-me`: stress-test a plan or decision.
- `/handoff`: prepare context for another agent or session.

## Shared skills

- [alexandru/skills](https://github.com/alexandru/skills/)
  - `code-review`: user-invoked adapter for `code-reviewing`.
  - `code-reviewing`: review changed code for bugs, structural problems, performance issues, and unintended behavior.
  - `simplify`: behavior-preserving code cleanup.
- [mattpocock/skills](https://github.com/mattpocock/skills/tree/v1.2.3)
  - `codebase-design`: deep-module design vocabulary and principles.
  - `diagnosing-bugs`: disciplined diagnosis for hard bugs and regressions.
  - `domain-modeling`: domain language and architectural decisions.
  - `grill-with-docs`: sharpen a plan or design while creating domain documentation.
  - `grilling`: structured decision-tree interviews.
  - `handoff`: prepare context for another agent or session.
  - `implement`: implement work from a specification or set of tickets.
  - `improve-codebase-architecture`: find and work through codebase architecture improvements.
  - `resolving-merge-conflicts`: merge and rebase conflict resolution.
  - `setup-matt-pocock-skills`: configure a repository for the engineering skills.
  - `tdd`: test-first development guidance.
  - `to-spec`: turn the current conversation into a published specification.
  - `to-tickets`: break a plan or specification into tracer-bullet tickets.
- [VirtusLab/cellar](https://github.com/VirtusLab/cellar/)
  - `cellar`: query the APIs of JVM dependencies (Scala, Java).
- [JuliusBrussee/caveman](https://github.com/JuliusBrussee/caveman)
  - `caveman`: token-efficient response modes with preserved technical accuracy.
- [cursor/plugins](https://github.com/cursor/plugins/tree/main/pstack/skills/unslop)
  - `unslop`: remove AI writing patterns and add a human voice.
- [brave/brave-search-skills](https://github.com/brave/brave-search-skills)
  - `web-search`: ranked web search with snippets and rich metadata.

## Updating shared skills

```sh
make update-skills
```

This reinstalls the configured global skill roster from its upstream sources.
