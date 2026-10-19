#!/usr/bin/env bash

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ -x "$PLUGIN_DIR/bin/tmux-agent-sidebar" ]]; then
    SIDEBAR_BINARY="$PLUGIN_DIR/bin/tmux-agent-sidebar"
elif [[ -x "$PLUGIN_DIR/target/release/tmux-agent-sidebar" ]]; then
    SIDEBAR_BINARY="$PLUGIN_DIR/target/release/tmux-agent-sidebar"
elif command -v "tmux-agent-sidebar" &>/dev/null; then
    SIDEBAR_BINARY="tmux-agent-sidebar"
fi

if [[ -z "$SIDEBAR_BINARY" ]]; then
    tmux run-shell -b "bash '$PLUGIN_DIR/install-wizard.sh'"
    exit 0
fi

INSTALLED_VERSION="$("$SIDEBAR_BINARY" version 2>/dev/null)"
EXPECTED_VERSION="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$PLUGIN_DIR/Cargo.toml")"

if [[ -n "$EXPECTED_VERSION" && "$INSTALLED_VERSION" != "$EXPECTED_VERSION" ]]; then
    tmux run-shell -b "SIDEBAR_UPDATE=1 bash '$PLUGIN_DIR/install-wizard.sh'"
    exit 0
fi

tmux set -g @agent_sidebar_bin "$SIDEBAR_BINARY"

# Ensure the OpenCode global bridge symlink exists, so opencode TUIs in any
# project load it (a project-local .opencode/plugins copy only covers that
# project). Idempotent: re-pointed on every plugin load, which also heals a
# stale target after the clone moves or is reinstalled. Honors
# XDG_CONFIG_HOME the same way opencode resolves its config dir.
OPENCODE_PLUGIN_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/opencode/plugins"
OPENCODE_BRIDGE="$OPENCODE_PLUGIN_DIR/tmux-agent-sidebar.js"
if [[ -f "$PLUGIN_DIR/.opencode/plugins/tmux-agent-sidebar.js" ]]; then
    mkdir -p "$OPENCODE_PLUGIN_DIR"
    ln -sfn "$PLUGIN_DIR/.opencode/plugins/tmux-agent-sidebar.js" "$OPENCODE_BRIDGE"
fi

# Antigravity (agy) hooks: agy loads bundled hooks from plugin directories in
# ~/.gemini/config/plugins/, so generate the plugin bundle with the resolved
# hook.sh path (via `setup antigravity`, whose output IS the hooks.json
# content) and symlink it there — the same auto-wiring shape as the Claude
# plugin and the OpenCode bridge. Regenerated on every load so a rebuilt or
# moved install stays correct; agy picks up the bundle on its next session
# start. The bundle lives in the cache dir (generated content, not a repo
# artifact), and the symlink is what agy discovers.
AGY_PLUGINS_DIR="$HOME/.gemini/config/plugins"
AGY_RUNTIME_PLUGIN="${XDG_CACHE_HOME:-$HOME/.cache}/tmux-agent-sidebar/agy-plugin"
AGY_PLUGIN_LINK="$AGY_PLUGINS_DIR/tmux-agent-sidebar"
if AGY_HOOKS_JSON=$("$SIDEBAR_BINARY" setup antigravity 2>/dev/null) && [[ -n "$AGY_HOOKS_JSON" ]]; then
    mkdir -p "$AGY_RUNTIME_PLUGIN"
    printf '%s\n' "$AGY_HOOKS_JSON" > "$AGY_RUNTIME_PLUGIN/hooks.json"
    cat > "$AGY_RUNTIME_PLUGIN/plugin.json" <<'AGY_MANIFEST'
{ "name": "tmux-agent-sidebar", "description": "tmux-agent-sidebar lifecycle hooks for the Antigravity CLI" }
AGY_MANIFEST
    mkdir -p "$AGY_PLUGINS_DIR"
    ln -sfn "$AGY_RUNTIME_PLUGIN" "$AGY_PLUGIN_LINK"
fi

tmux source-file "$PLUGIN_DIR/agent-sidebar.conf"
