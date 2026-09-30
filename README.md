<h1 align="center">tmux-agent-sidebar</h1>

<p align="center">One tmux sidebar that tracks every Claude Code, Codex, and OpenCode pane across every session and window. See status, background shells, prompts, Git state, activity, and worktrees without switching windows.</p>

<p align="center"><img src="website/src/assets/captures/hero.png" alt="tmux-agent-sidebar hero" /></p>

<p align="center">
  <a href="https://github.com/brandtg/tmux-agent-sidebar/tree/main/website/src/content/docs">Documentation</a> ·
  <a href="https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/getting-started/installation.md">Getting Started</a> ·
  <a href="https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/agent-pane.mdx">Features</a>
</p>

## Features

- **Every pane, one view** 
  — tracks Claude Code, Codex, and OpenCode panes across all tmux sessions and windows
- **Live metadata** 
  — prompts, tool calls, response previews, background shell state, wait reasons, task progress, and subagent trees refresh as the agents work
- **Worktrees, included** 
  — spawn a fresh worktree + agent from the sidebar and tear it down — window, worktree, and branch — in one keystroke
- **Desktop notifications** 
  — native alerts when an agent finishes, needs permission, or errors out

OpenCode uses a small local plugin bridge instead of per-event hook config. The plugin lives at `.opencode/plugins/tmux-agent-sidebar.js` and can be symlinked as a single file into `~/.config/opencode/plugins/` so it coexists with any existing plugins.

## Requirements

- tmux 3.0+
- [TPM](https://github.com/tmux-plugins/tpm) (or the manual install in [Installation](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/getting-started/installation.md))
- [GitHub CLI](https://cli.github.com/) (optional — required only for PR numbers in the Git tab)

## Quick Start

### 1. Install the plugin

Using [TPM](https://github.com/tmux-plugins/tpm):

```tmux
set -g @plugin 'brandtg/tmux-agent-sidebar'
```

Reload tmux (`tmux source ~/.tmux.conf`), then press `prefix + I`. The install wizard downloads a pre-built binary or builds from source.

### 2. Wire up the agent hooks

- **Claude Code** — register the plugin inside Claude Code:

  ```sh
  /plugin marketplace add ~/.tmux/plugins/tmux-agent-sidebar
  /plugin install tmux-agent-sidebar@brandtg
  ```

- **Codex** — open a Codex pane, press `prefix + e`, click the yellow `ⓘ` badge, copy the setup snippet, paste it into the Codex pane.
- **OpenCode** — symlink just the plugin file so your existing `~/.config/opencode/plugins/` contents stay untouched:

  ```sh
  mkdir -p ~/.config/opencode/plugins
  ln -sf ~/.tmux/plugins/tmux-agent-sidebar/.opencode/plugins/tmux-agent-sidebar.js \
    ~/.config/opencode/plugins/tmux-agent-sidebar.js
  ```

Full walkthroughs: [Claude Code setup](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/getting-started/claude-code.md) · [Codex setup](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/getting-started/codex.md) · [OpenCode setup](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/getting-started/opencode.md)

### 3. Toggle the sidebar

`prefix + e` toggles the sidebar in the current window, `prefix + E` toggles it everywhere.

## Documentation

The [documentation](https://github.com/brandtg/tmux-agent-sidebar/tree/main/website/src/content/docs) covers every feature and option:

- [Agent pane breakdown](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/agent-pane.mdx)
- [Worktree lifecycle](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/worktree.mdx)
- [Activity log](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/activity-log.mdx) · [Git tab](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/git-status.mdx) · [Notifications](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/features/notifications.mdx)
- [Agent support matrix](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/agents/index.md)
- [Keybindings](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/reference/keybindings.md) · [tmux options](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/reference/tmux-options.md) · [Scripting](https://github.com/brandtg/tmux-agent-sidebar/blob/main/website/src/content/docs/reference/scripting.md)

## Development

Symlink the plugin directory to your working copy so builds are picked up without copying:

```sh
rm -rf ~/.tmux/plugins/tmux-agent-sidebar
ln -s <path-to-this-repo> ~/.tmux/plugins/tmux-agent-sidebar
cargo build --release
```

Toggle the sidebar off → on to pick up the new binary.

### Picking up local builds for the Claude Code plugin

If you also installed this as a Claude Code plugin (`/plugin`), its install path
holds a copy of the released binary that hooks resolve before falling back to
`target/release/`. To make local builds flow through Claude Code hooks too,
replace that copy with a symlink to your working copy:

```sh
# Replace the cached plugin install with a symlink to your repo
PLUGIN_CACHE=~/.claude/plugins/cache/<owner>/tmux-agent-sidebar/<version>
rm -rf "$PLUGIN_CACHE"
ln -s <path-to-this-repo> "$PLUGIN_CACHE"
```

Also remove the stale release binary at `bin/tmux-agent-sidebar` in your repo
if present — both the tmux launcher and `hook.sh` prefer `bin/` over
`target/release/`, so a leftover binary there will mask `cargo build --release`
output:

```sh
rm -f bin/tmux-agent-sidebar
```

Note: Claude Code's plugin updater may overwrite the symlink on a future
update; re-run the symlink step if that happens.

## License

[MIT](./LICENSE)
