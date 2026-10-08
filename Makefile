BIN := target/release/tmux-agent-sidebar

# e.g. make test ARGS=run_with_timeout
ARGS ?=

.DEFAULT_GOAL := build
.PHONY: build test clippy fmt fmt-check ci insta-accept install clean

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

insta-accept: ## Accept UI snapshot changes after intentional UI edits
	cargo insta accept

install: ## Build + copy into bin/, fix up (macOS re-sign), kill running instances, reload tmux.conf
	./install-wizard.sh build-from-source

clean: ## Remove build artifacts
	cargo clean

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "%-14s %s\n", $$1, $$2}'
