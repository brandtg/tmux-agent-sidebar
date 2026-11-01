<h1 align="center">tmux-agent-sidebar</h1>

<p align="center">One tmux sidebar that tracks every Claude Code, Codex, OpenCode, Hermes, and Antigravity pane across every session and window — status, background shells, prompts, Git state, and worktrees, without switching windows.</p>

<p align="center"><img src="website/src/assets/captures/hero.png" alt="tmux-agent-sidebar" /></p>

## Features

- Tracks Claude Code, Codex, OpenCode, Hermes, and Antigravity panes across all tmux sessions and windows
- Live prompts, response previews, background shells, wait reasons, task progress, and subagent trees
- Spawn a worktree + agent from the sidebar; tear down window, worktree, and branch in one keystroke
- Plain shell panes are tracked too — long-running commands pulse as tasks, and an optional shell-integration snippet captures real exit codes
- Desktop notifications when an agent finishes, needs permission, or errors out
- A manual TODO list (`t`), shared across every open sidebar and saved between restarts

## Quick start

Requires tmux 3.0+ and [TPM](https://github.com/tmux-plugins/tpm) (or the [manual install](website/src/content/docs/getting-started/installation.md)).

### 1. Install the plugin

```tmux
set -g @plugin 'brandtg/tmux-agent-sidebar'
```

Reload tmux (`tmux source ~/.tmux.conf`), then press `prefix + I`. The install wizard downloads a pre-built binary or builds from source.

### 2. Wire up agent hooks

- **Claude Code** — installs as a plugin:

  ```sh
  /plugin marketplace add ~/.tmux/plugins/tmux-agent-sidebar
  /plugin install tmux-agent-sidebar@brandtg
  ```

- **Codex** — one-paste setup inside its own pane ([guide](website/src/content/docs/getting-started/codex.md))
- **OpenCode** — symlink the bundled [plugin bridge](website/src/content/docs/getting-started/opencode.md) into `~/.config/opencode/plugins/`
- **Hermes** — merge a generated `hooks:` block into `~/.hermes/config.yaml` ([guide](website/src/content/docs/getting-started/hermes.md))
- **Antigravity** — wired automatically on plugin load ([guide](website/src/content/docs/getting-started/antigravity.md))

Optional: to detect shell-task failures, source the exit-code snippet in `~/.zshrc` / `~/.bashrc`:

```sh
[ -f "$HOME/.tmux/plugins/tmux-agent-sidebar/shell-exit.sh" ] && \
  . "$HOME/.tmux/plugins/tmux-agent-sidebar/shell-exit.sh"
```

### 3. Toggle the sidebar

`prefix + e` toggles it in the current window, `prefix + E` everywhere.

## Documentation

The full docs live in [website/src/content/docs](website/src/content/docs) — every feature and option, including the [agent pane breakdown](website/src/content/docs/features/agent-pane.mdx), [worktree lifecycle](website/src/content/docs/features/worktree.mdx), [keybindings](website/src/content/docs/reference/keybindings.md), and the [tmux options reference](website/src/content/docs/reference/tmux-options.md). All configuration is tmux options set in `~/.tmux.conf`.

## Development

```sh
cargo build --release    # build the binary tmux loads
cargo test               # unit + UI snapshot tests
cargo clippy && cargo fmt --check
```

To pick up local builds, symlink the checkout over the installed plugin and toggle the sidebar off → on:

```sh
ln -sfn <path-to-this-repo> ~/.tmux/plugins/tmux-agent-sidebar
```

## History

Forked from [hiroppy/tmux-agent-sidebar](https://github.com/hiroppy/tmux-agent-sidebar) in September 2026. This fork adds Hermes and Antigravity support, a shared TODO panel, shell-pane tracking with exit codes, and a stream of layout and navigation refinements — [what's different](website/src/content/docs/getting-started/fork.md) has the details. It's where day-to-day maintenance happens now. Thanks to hiroppy and every upstream contributor; the [LICENSE](./LICENSE) carries both copyright lines.

## License

[MIT](./LICENSE)
