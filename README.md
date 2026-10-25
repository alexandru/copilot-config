# My Copilot CLI configuration

Part of [alexandru/agents-config](https://github.com/alexandru/agents-config).

## Installation

<details>
<summary>STEP 1 — Clone the repository</summary>

### Clone the repository

**WARN** — This is for a fresh Copilot installation (no history):

```sh
git clone https://github.com/alexandru/copilot-config.git ~/.copilot &&
  git -C ~/.copilot config --local status.showUntrackedFiles no
```

**WARN:** This is your Copilot's working directory, so you may already have a `~/.copilot` that you may need to delete, in which case you could lose all your sessions. An alternative would be...

```bash
(
  set -e
  if [[ -d ~/.copilot ]]; then
    # Clones in temporary directory
    git clone https://github.com/alexandru/copilot-config.git /tmp/copilot-config
    echo
    # Sync all the files from clone to your working dir
    rsync -rcv /tmp/copilot-config/ ~/.copilot/
    # Doing some index cleanup
    cd ~/.copilot
    git pull
    git config --local status.showUntrackedFiles no
    # Cleanup
    rm -rf /tmp/copilot-config
  else
    git clone https://github.com/alexandru/copilot-config.git ~/.copilot
    git -C ~/.copilot config --local status.showUntrackedFiles no
  fi
)
```

If the path isn't standard, you may need to set this in your `~/.zshrc`, `~/.bashrc` or `~/.profile`:
```sh
export COPILOT_HOME=/absolute/path/to/copilot-config
```
</details>

<details>
<summary>STEP 2 — Install dependencies</summary>

### Install dependencies

```sh
cd ~/.copilot && make
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
<summary>STEP 4 — Choose a configuration preset (optional)</summary>

### Choose a configuration preset (optional)

The [copilot-switch](./bin/copilot-switch.py) utility is for quickly switching between multiple setting presets (e.g., multiple sets of models assigned to your agents). It's built with Python 🐍, so you need [pipx](https://pipx.pypa.io/) installed (`make install-pipx`).

```sh
# Example:
./bin/copilot-switch work
```

The switcher generates `settings.json` and `mcp-config.json` (Copilot's main configuration files) from:

- [settings.common.jsonc](./settings.common.jsonc)
- [mcp-config.common.jsonc](./mcp-config.common.jsonc)
- [config.presets.jsonc](./config.presets.jsonc)
</details>

<details>
<summary>STEP 5 — Install Cellar (optional)</summary>

### Install Cellar (optional)

[Cellar](https://github.com/VirtusLab/cellar) is useful for JVM dependency API lookup. The default `make` target installs it and its associated skill.

Install [Coursier](https://get-coursier.io/docs/cli-installation) first:

```bash
## MacOS
brew install coursier/formulas/coursier && cs setup
```

```bash
(
  set -eo pipefail
  ## Linux x86-64 (aka AMD64)
  curl -fL "https://github.com/coursier/launchers/raw/master/cs-x86_64-pc-linux.gz" | gzip -d > cs
)
```

```bash
(
  set -eo pipefail
  ## Linux ARM64
  curl -fL "https://github.com/VirtusLab/coursier-m1/releases/latest/download/cs-aarch64-pc-linux.gz" | gzip -d > cs
)
```

Then install Cellar via Coursier:

```bash
(
  set -e
  cs install --contrib cellar
  cellar --version

  # Disable telemetry
  cellar telemetry disable
)
```
</details>

## Defined agents

Main agents:

- [Orchestrator](./agents/Orchestrator.agent.md) (default agent): designs and implements changes; delegates evidence, research, and execution to Junior.
- [Solo](./agents/Solo.agent.md): designs and implements changes; works directly.

Sub-agents:

- [Junior](./agents/Junior.agent.md): codebase and external evidence gathering, bounded execution, mechanical work.

## Shared skills

- [alexandru/skills](https://github.com/alexandru/skills/tree/v13.1.1)
  - `code-review`: user-invoked adapter for `code-reviewing`.
  - `code-reviewing`: review changed code for bugs, structural problems, performance issues, and unintended behavior.
  - `mcp-access`: access MCP servers when native MCP tools are unavailable or unsupported.
  - `code-refine`: behavior-preserving refinement using the `code-refinements` lenses.
  - `code-refinements`: refinement lenses of constraints, simplicity, and parametricity.
- [mattpocock/skills](https://github.com/mattpocock/skills/tree/v1.3.1)
  - `codebase-design`: deep-module design vocabulary and principles.
  - `diagnosing-bugs`: disciplined diagnosis for hard bugs and regressions.
  - `domain-modeling`: domain language and architectural decisions.
  - `grill-me`: user-invoked adapter for `grilling`.
  - `grill-with-docs`: sharpen a plan or design while creating domain documentation.
  - `grilling`: structured decision-tree interviews.
  - `handoff`: prepare context for another agent or session.
  - `implement`: implement work from a specification or set of tickets.
  - `improve-codebase-architecture`: find and work through codebase architecture improvements.
  - `setup-matt-pocock-skills`: configure a repository for the engineering skills.
  - `tdd`: test-first development guidance.
  - `teach`: teach the user a topic through a stateful learning workspace with lessons and reference documents.
  - `to-spec`: turn the current conversation into a published specification.
  - `to-tickets`: break a plan or specification into tracer-bullet tickets.
- [VirtusLab/cellar](https://github.com/VirtusLab/cellar/)
  - `cellar`: query the APIs of JVM dependencies (Scala, Java).
- [JuliusBrussee/caveman](https://github.com/JuliusBrussee/caveman)
  - `caveman`: token-efficient response modes with preserved technical accuracy.

## Updating shared skills

```sh
make update-skills
```

This reinstalls the configured global skill roster from its upstream sources.
