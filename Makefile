# Bootstrap and manage the local Copilot CLI configuration.
# The default target installs every missing prerequisite, generates the default
# configuration when missing, and installs the shared skills globally. Never run
# automatically (no CI, no hooks); review upstream skill content before installation.

.NOTPARALLEL:

ALEXANDRU_SKILLS_TAG := v11.1.0
MATTPOCOCK_SKILLS_TAG := v1.3.1
SKILLS_AGENT := github-copilot
SKILLS_INSTALL_FLAGS := -g -a $(SKILLS_AGENT) -y

.PHONY: all install-pipx generate-config check-mattpocock-skills-tag install-skills update-skills test-copilot-switch

all: install-pipx generate-config install-skills

# copilot-switch runs through pipx.
install-pipx:
	@set -e; \
	if command -v pipx >/dev/null 2>&1; then \
		echo "pipx already installed: $$(pipx --version)"; exit 0; \
	fi; \
	case "$$(uname -s)" in \
		Darwin) brew install pipx ;; \
		Linux) sudo apt-get install -y pipx ;; \
		*) echo "Unsupported platform: $$(uname -s)" >&2; exit 1 ;; \
	esac

generate-config:
	@if [ -f settings.json ] && [ -f mcp-config.json ]; then \
		echo "settings.json and mcp-config.json already exist; leaving them untouched"; \
	else \
		./bin/copilot-switch default; \
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
		refine \
		refinements
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
	npx skills add https://github.com/cursor/plugins/tree/main/pstack/skills/unslop $(SKILLS_INSTALL_FLAGS) --skill unslop
	npx skills add https://github.com/brave/brave-search-skills $(SKILLS_INSTALL_FLAGS) --skill web-search
	@echo "Shared skills installed in ~/.agents/skills."

update-skills: check-mattpocock-skills-tag
	$(MAKE) install-skills

test-copilot-switch:
	./bin/copilot-switch-test.py
