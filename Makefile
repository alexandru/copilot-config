# Bootstrap and manage the local Copilot CLI configuration.
# The default target installs the tooling it manages (Node.js, Coursier and cellar,
# a Rust toolchain through rustup, and shared skills), builds the Rust utilities,
# and generates the default configuration when missing. Rust uses the official
# rustup installer when unavailable. Never run
# automatically (no CI, no hooks); review upstream skill content before installation.

.NOTPARALLEL:

ALEXANDRU_SKILLS_TAG := v13.3.2
MATTPOCOCK_SKILLS_TAG := v1.3.1
SKILLS_AGENT := github-copilot
SKILLS_INSTALL_FLAGS := -g -a $(SKILLS_AGENT) -y
# Node.js required by the skills CLI's engines field.
SKILLS_NODE_MINIMUM := 22.20.0
# Third-party packages come only from Homebrew on macOS and the distro's apt
# repositories on Linux; Rust itself comes from the official rustup installer.
APT_GET := $(if $(filter 0,$(shell id -u)),,sudo )apt-get
COURSIER_LINUX_X86_64_URL := https://github.com/coursier/launchers/raw/master/cs-x86_64-pc-linux.gz
COURSIER_LINUX_AARCH64_URL := https://github.com/VirtusLab/coursier-m1/releases/latest/download/cs-aarch64-pc-linux.gz
# Coursier installs launchers outside the default PATH of a non-login shell.
COURSIER_BIN_PATHS := $(HOME)/.local/bin:$(HOME)/.local/share/coursier/bin:$(HOME)/Library/Application Support/Coursier/bin
UTILS_MANIFEST := $(CURDIR)/utils/Cargo.toml
export PATH := $(or $(CARGO_HOME),$(HOME)/.cargo)/bin:$(PATH)
# Honor CARGO_TARGET_DIR; relative values keep Cargo's meaning of relative to this directory.
CARGO_TARGET_DIR ?= $(CURDIR)/utils/target
UTILS_CARGO_FLAGS := --manifest-path "$(UTILS_MANIFEST)" --target-dir "$(CARGO_TARGET_DIR)"

.PHONY: all generate-config install-node check-mattpocock-skills-tag install-skills update-skills install-coursier install-cellar install-rustup build-utils test-utils check-utils test test-copilot-switch

all: build-utils generate-config install-node install-skills install-cellar

generate-config: build-utils
	@if [ -f settings.json ] && [ -f mcp-config.json ]; then \
		echo "settings.json and mcp-config.json already exist; leaving them untouched"; \
	else \
		./bin/copilot-switch default; \
	fi

# The skills CLI runs through npx.
install-node:
	@set -e; \
	node_sufficient='const [have, need] = [process.versions.node, process.argv[1]].map((v) => v.split(".").map(Number)); const i = have.findIndex((part, index) => part !== need[index]); process.exit(i < 0 || have[i] > need[i] ? 0 : 1)'; \
	sufficient() { \
		npx --version >/dev/null 2>&1 && node -e "$$node_sufficient" "$(SKILLS_NODE_MINIMUM)" >/dev/null 2>&1; \
	}; \
	if sufficient; then \
		echo "Node already installed: $$(node --version)"; exit 0; \
	fi; \
	case "$$(uname -s)" in \
		Darwin) if brew list node >/dev/null 2>&1; then brew upgrade node; else brew install node; fi ;; \
		Linux) $(APT_GET) update; $(APT_GET) install -y --no-install-recommends nodejs npm ;; \
		*) echo "Unsupported platform: $$(uname -s)" >&2; exit 1 ;; \
	esac; \
	if ! sufficient; then \
		echo "Node.js $(SKILLS_NODE_MINIMUM) or later with npx is required by the skills CLI; installed: $$(node --version 2>/dev/null || echo none)" >&2; exit 1; \
	fi

check-mattpocock-skills-tag:
	@latest=$$(git ls-remote --tags --refs --sort=-version:refname https://github.com/mattpocock/skills.git 'v*' 2>/dev/null | sed -n '1s#.*refs/tags/##p'); \
	if [ -z "$$latest" ]; then \
		echo "WARN: unable to check the latest mattpocock/skills tag"; \
	elif [ "$$latest" != "$(MATTPOCOCK_SKILLS_TAG)" ]; then \
		echo "WARN: mattpocock/skills is pinned to $(MATTPOCOCK_SKILLS_TAG); latest tag is $$latest"; \
	fi

install-skills:
	npx skills add https://github.com/alexandru/skills/tree/$(ALEXANDRU_SKILLS_TAG) $(SKILLS_INSTALL_FLAGS) --skill \
		code-review \
		code-reviewing \
		mcp-access \
		code-refine \
		code-refinements \
		external-research
	npx skills add https://github.com/mattpocock/skills/tree/$(MATTPOCOCK_SKILLS_TAG) $(SKILLS_INSTALL_FLAGS) --skill \
		codebase-design \
		diagnosing-bugs \
		domain-modeling \
		grill-me \
		grill-with-docs \
		grilling \
		handoff \
		implement \
		improve-codebase-architecture \
		setup-matt-pocock-skills \
		tdd \
		teach \
		to-spec \
		to-tickets
	npx skills add https://github.com/VirtusLab/cellar/ $(SKILLS_INSTALL_FLAGS)
	npx skills add https://github.com/JuliusBrussee/caveman $(SKILLS_INSTALL_FLAGS) --skill caveman
	@echo "Shared skills installed in ~/.agents/skills."

update-skills: check-mattpocock-skills-tag
	$(MAKE) install-skills

# The cellar skill shells out to the cellar CLI, installed through Coursier.
install-coursier:
	@set -e; \
	export PATH="$(COURSIER_BIN_PATHS):$$PATH"; \
	if command -v cs >/dev/null 2>&1; then \
		echo "coursier already installed: $$(cs version)"; exit 0; \
	elif command -v coursier >/dev/null 2>&1; then \
		echo "coursier already installed: $$(coursier version)"; exit 0; \
	fi; \
	case "$$(uname -s)" in \
		Darwin) \
			brew install coursier; \
			coursier setup --yes ;; \
		Linux) \
			case "$$(uname -m)" in \
				x86_64|amd64) url="$(COURSIER_LINUX_X86_64_URL)" ;; \
				aarch64|arm64) url="$(COURSIER_LINUX_AARCH64_URL)" ;; \
				*) echo "Unsupported architecture: $$(uname -m)" >&2; exit 1 ;; \
			esac; \
			missing=; \
			for tool in curl gzip; do command -v "$$tool" >/dev/null 2>&1 || missing="$$missing $$tool"; done; \
			if [ -n "$$missing" ]; then \
				$(APT_GET) update; $(APT_GET) install -y --no-install-recommends $$missing; \
			fi; \
			mkdir -p "$$HOME/.local/bin"; \
			archive=$$(mktemp); \
			trap 'rm -f "$$archive"' 0; \
			curl -fL "$$url" -o "$$archive"; \
			gzip -dc "$$archive" > "$$HOME/.local/bin/cs"; \
			chmod +x "$$HOME/.local/bin/cs"; \
			"$$HOME/.local/bin/cs" setup --yes ;; \
		*) echo "Unsupported platform: $$(uname -s)" >&2; exit 1 ;; \
	esac

install-cellar: install-coursier
	@set -e; \
	export PATH="$(COURSIER_BIN_PATHS):$$PATH"; \
	if command -v cellar >/dev/null 2>&1; then \
		echo "cellar already installed: $$(cellar --version)"; \
	else \
		if command -v cs >/dev/null 2>&1; then cs=cs; else cs=coursier; fi; \
		"$$cs" install --contrib cellar; \
		cellar --version; \
		cellar telemetry disable --global; \
	fi; \
	./bin/utils/setup-path; \
	echo "Cellar PATH updated; new shells will pick it up."

install-rustup:
	@command -v rustup >/dev/null 2>&1 || { \
		installer=$$(curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs) || exit $$?; \
		sh -c "$$installer" -- -y --no-modify-path; }

test: test-utils

build-utils: install-rustup
	cargo build $(UTILS_CARGO_FLAGS) --release --locked --workspace --bins
	mkdir -p bin
	install -m 0755 "$(CARGO_TARGET_DIR)/release/copilot-agent-reminders" bin/copilot-agent-reminders
	install -m 0755 "$(CARGO_TARGET_DIR)/release/copilot-switch" bin/copilot-switch

test-utils: install-rustup
	cargo test $(UTILS_CARGO_FLAGS) --locked --workspace

check-utils: install-rustup
	cargo fmt --manifest-path "$(UTILS_MANIFEST)" --all -- --check
	cargo clippy $(UTILS_CARGO_FLAGS) --locked --workspace --all-targets -- -D warnings

test-copilot-switch: install-rustup
	cargo test $(UTILS_CARGO_FLAGS) --locked -p copilot-switch
