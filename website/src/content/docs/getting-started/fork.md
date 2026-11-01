---
title: About this fork
description: Why brandtg/tmux-agent-sidebar exists, what it changes relative to hiroppy/tmux-agent-sidebar, and how the two relate.
---

This site documents [brandtg/tmux-agent-sidebar](https://github.com/brandtg/tmux-agent-sidebar), a fork of [hiroppy/tmux-agent-sidebar](https://github.com/hiroppy/tmux-agent-sidebar) created in September 2026.

## Why a fork

The fork exists so maintenance and feature work can happen on one maintainer's timeline. The features that motivated it — Hermes and Antigravity support, shell-pane tracking, a shared TODO panel — landed in the first weeks, and smaller UX changes have kept coming since. Nothing here is a judgment on upstream; the two repos' feature lists make the difference plain, and upstream remains where this project came from.

## What's different

### Agents

- **Hermes** — tracked via its built-in shell hooks: a generated `hooks:` block merged into `~/.hermes/config.yaml`
- **Antigravity** (`agy`) — tracked via its native `hooks.json`, generated and linked automatically on every plugin load

Claude Code, Codex, and OpenCode work as upstream built them — OpenCode still uses the single-file plugin bridge.

### Features

- **TODO panel** (`t`) — a manual checklist pinned to the sidebar, shared across every open sidebar and persisted across restarts
- **Shell-pane tracking** — long-running commands pulse as tasks on non-agent rows, and an optional `shell-exit.sh` snippet captures real exit codes (failures land in the error filter)
- **Small viewports** — the sidebar opens as a tmux popup below a width threshold and auto-hides split panes on resize, so it stays usable over SSH from a phone
- **Worktree detail** — worktree name and branch surfaced on agent rows, with worktree-aware cwd resolution
- **Attention flags** (`f`) — mark agents and processes with a gutter marker
- **Toggle-all semantics** — `prefix + E` records a system-wide choice that new windows and sessions follow
- Git ahead/behind movement shown directly on agent rows (the upstream Feed/Repos bottom panel and pet are gone)
- A long tail of navigation and rendering refinements: session grouping (`s`), compact view (`c`), `?` keybinding reference, continuous `j`/`k` across panels, session titles for OpenCode and Hermes, permission-denied and cwd-changed handling

### Configuration

New tmux options include `@sidebar_width_max`, `@sidebar_preview_lines`, `@sidebar_popup_max_width`, `@sidebar_default_view`, `@sidebar_group_by`, `@sidebar_icon_set`, the `@sidebar_todo_*` options, and the Hermes/Antigravity colors. Upstream options keep working, except a handful tied to the removed bottom panel; the full list is in [tmux options](/tmux-agent-sidebar/reference/tmux-options/).

## Relationship to upstream

- The core — pane discovery, agent adapters, worktree lifecycle, notifications — is upstream's work, lightly adapted.
- The [LICENSE](https://github.com/brandtg/tmux-agent-sidebar/blob/main/LICENSE) keeps hiroppy's MIT copyright line and adds one for the fork's modifications.
- Coming from upstream: install the same way (TPM + install wizard), wire hooks per agent as before, and your existing `~/.tmux.conf` options carry over.
- Issues and pull requests for this fork go to [brandtg/tmux-agent-sidebar](https://github.com/brandtg/tmux-agent-sidebar).
