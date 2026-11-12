BIN := target/release/tmux-agent-sidebar
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)

# e.g. make test ARGS=run_with_timeout
ARGS ?=
part ?=

.DEFAULT_GOAL := build
.PHONY: build test clippy fmt fmt-check ci check insta-accept bump tag install clean

build: ## Build the release binary tmux loads (strip + lto)
	cargo build --release

test: ## Run the full test suite (pass ARGS="" to filter, e.g. make test ARGS=run_with_timeout)
	cargo test $(ARGS)

clippy: ## Lint
	cargo clippy

fmt: ## Auto-format all source (run first, before fmt-check)
	cargo fmt

fmt-check: ## Verify formatting only (what CI runs)
	cargo fmt --check

ci: fmt-check clippy test ## Run everything CI runs (formatting checked, not auto-fixed)

check: ci ## Alias for ci — run all CI checks locally

insta-accept: ## Accept UI snapshot changes after intentional UI edits
	cargo insta accept

bump: ## Bump the version: make bump part=patch|minor|major (Cargo.toml + plugin.json + Cargo.lock)
	@test -n "$(part)" || { echo "usage: make bump part=patch|minor|major"; exit 1; }
	@case "$(part)" in patch|minor|major) ;; *) echo "invalid part '$(part)' (want patch, minor, or major)"; exit 1;; esac
	@old=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml); \
	new=$$(awk -v v="$$old" -v part="$(part)" 'BEGIN { \
		split(v, a, "."); \
		if (part == "major")      { a[1]++; a[2] = 0; a[3] = 0 } \
		else if (part == "minor") { a[2]++; a[3] = 0 } \
		else                      { a[3]++ } \
		print a[1] "." a[2] "." a[3] }'); \
	sed -i.bak 's/^version = ".*"/version = "'"$$new"'"/' Cargo.toml && rm -f Cargo.toml.bak; \
	sed -i.bak 's/"version": ".*"/"version": "'"$$new"'"/' .claude-plugin/plugin.json && rm -f .claude-plugin/plugin.json.bak; \
	cargo check --quiet; \
	toml_v=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml); \
	json_v=$$(sed -n 's/.*"version": "\(.*\)".*/\1/p' .claude-plugin/plugin.json); \
	if [ "$$toml_v" != "$$json_v" ] || [ "$$toml_v" != "$$new" ]; then \
		echo "version drift: Cargo.toml=$$toml_v plugin.json=$$json_v expected=$$new"; exit 1; \
	fi; \
	echo "$$old -> $$new (run make ci, then commit)"

tag: ## Create the vX.Y.Z git tag from the Cargo.toml version (commit the bump first)
	git tag v$(VERSION)
	@echo "tagged v$(VERSION) — push with: git push && git push origin v$(VERSION)"

install: ## Build + copy into bin/, fix up (macOS re-sign), kill running instances, reload tmux.conf
	./install-wizard.sh build-from-source

clean: ## Remove build artifacts
	cargo clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "%-14s %s\n", $$1, $$2}'
