---
title: Scripting
description: Pane options, CLI subcommands, and the activity log for reading agent state from your own scripts.
---

The sidebar writes agent state into tmux pane options on every hook event, so
you can pick it up from any script with `tmux show -t <pane> -pv <key>`. The
same binary also ships subcommands for scripted control, and every tool event
lands in a per-pane activity log you can tail.

## Reading pane options

```bash
# Get a specific pane's agent status
tmux show -t "$pane_id" -pv @pane_status
# → running / background / waiting / idle / error / (empty)

# Get agent type
tmux show -t "$pane_id" -pv @pane_agent
# → claude / codex / opencode / hermes / antigravity / (empty)
```

## Available pane options

| Key                        | Value                                                              |
| -------------------------- | ------------------------------------------------------------------ |
| `@pane_status`             | `running` / `background` / `waiting` / `idle` / `error` / empty    |
| `@pane_attention`          | `notification` while a hook wants attention (waiting / permission), `done` while a finished turn is unseen; empty otherwise. The sidebar clears it when you focus the pane. |
| `@pane_agent`              | `claude` / `codex` / `opencode` / `hermes` / `antigravity` / empty  |
| `@pane_session_title`      | Session title reported by the OpenCode bridge (`session.created` / `session.renamed`); empty for other agents. Claude `/rename` names are not pane options — the sidebar reads them from Claude's session store. |
| `@pane_role`               | `sidebar` for the sidebar pane itself; empty for agent panes        |
| `@pane_prompt`             | Latest user prompt text or response preview                         |
| `@pane_prompt_source`      | `user` when the prompt field holds the user's prompt, `response` when it holds the agent's last reply |
| `@pane_started_at`         | Epoch seconds of the last real user turn (`UserPromptSubmit`); seeded by the first tool event when missing. Synthetic empty-prompt submits do not reset an existing value. |
| `@pane_wait_reason`        | Wait-reason text (populated only when waiting)                      |
| `@pane_bg_cmd`             | Latest sanitized background Bash command (Claude `run_in_background`, Hermes `terminal(background=true)`); empty when no bg shell is tracked. Cleared automatically by a `ps` liveness sweep when the process exits. |
| `@pane_bg_pid`             | PID of the tracked background process when the spawn result reported one (Hermes). The sweep prefers it over the command-string match. |
| `@pane_subagents`          | Comma-separated `agent_type:agent_id` entries (e.g. `Explore:sub-1`), appended as sub-agents start and pruned when they finish. For Hermes a non-empty list survives the parent's Stop — async children are still running — and the final `subagent_stop` drops the pane back to idle. |
| `@pane_cwd`                | Working directory reported by the agent (preferred over `pane_current_path`) |
| `@pane_launch_cwd`         | Directory the pane was launched in — the grouping anchor; set at the first `SessionStart` and left alone afterwards |
| `@pane_permission_mode`    | Permission-mode string for the pane (`plan` / `acceptEdits` / `auto` / `bypassPermissions` / …); only `plan` and `!` render as badges |
| `@pane_worktree_name`      | Worktree label — written from the hook payload's worktree info, including manual worktrees (resolved from the pane's live directory when the payload omits it) |
| `@pane_worktree_branch`    | The worktree's branch, same sources as `@pane_worktree_name`        |
| `@pane_session_id`         | Agent session ID (opaque; useful for correlating logs)              |
| `@pane_last_cmd`           | Last shell command, written by the optional `shell-exit.sh` integration (bash `DEBUG` trap) when sourced in the pane's shell |
| `@pane_last_exit`          | Exit code of the command in `@pane_last_cmd`                        |
| `@sidebar_flagged`         | `1` on panes flagged with `f` inside the sidebar; dies with the pane |

## CLI subcommands

The binary the plugin installs
(`~/.tmux/plugins/tmux-agent-sidebar/bin/tmux-agent-sidebar`) also exposes
subcommands for scripted control:

| Command | Purpose |
| --- | --- |
| `toggle [--create-only] [--force-split] <window-id> [pane-path] [client-width]` | Toggle the sidebar for a window |
| `toggle-all` | Toggle the sidebar in all windows (records `@sidebar_mode` so new windows follow) |
| `set-status <status>` | Write `@pane_status` for the current pane (`running` / `background` / `waiting` / `idle` / `error`, or `clear` to unset) |
| `todo add <text>` / `todo list` / `todo done <n>` / `todo open <n>` / `todo rm <n>` | Manage the shared TODO list |
| `spawn <name>` | Spawn a worktree window for the current pane's repo |
| `capture --session <name> [--window <name>] (--frame-out <file> \| --frames-out <dir>) [--duration-ms <ms>] [--fps <n>] [--crop-rows START:END] [--crop-cols START:END]` | Capture a pane as ANSI frames — the engine behind the overview grid |
| `overview [--popup] [--popup-client <client>] [--interval-ms <ms>]` | Render the live overview grid, optionally inside a tmux popup |
| `switch [--popup] [--popup-client <client>]` | Open the quick switcher — a fuzzy finder over live agent panes with alt-tab recency ordering — optionally inside a tmux popup |
| `layouts [list\|emit]` | Sidebar-aware builtin layouts (`prefix` + `M-1`…`M-5`); `emit` prints the tmux layout string the binding applies (tmux 3.8+) |
| `setup [claude\|codex\|hermes\|antigravity]` | Print the hook-registration snippet for an agent |
| `hook <agent> <event>` | Dispatch an agent hook payload read from stdin — what the registered hooks invoke |
| `auto-close <window-id>` / `maintain-width <window-id> [client-width]` / `notify-focus <window-id>` | Internal helpers wired to tmux hooks |
| `version` | Print the version |

Subcommands exit `0` on success, `1` on a runtime failure (with a line on
stderr), and `2` on a usage error.

## Activity log

Tool events are appended to one file per pane —
`$XDG_RUNTIME_DIR/tmux-agent-activity_<pane-id>.log` (the pane id's leading
`%` becomes `_`; `/tmp` when `XDG_RUNTIME_DIR` is unset) — one line per event:

```text
1763628399|Bash|cargo test
```

The format is `epoch-seconds|tool|label`. The file is capped at 200 lines:
once it passes 210 it is rewritten in place with the last 200. The sidebar
parses these same files to render per-row activity, and scripts can tail them
for their own pipelines.

## Use cases

- **Status bar integration** — surface `@pane_status` in your tmux status line to light up when an agent needs attention.
- **Custom notifications** — if you don't like the built-in desktop notifications, build your own pipeline off the same pane options.
- **Shell aliases** — gate side-effectful commands on agent state.

## Example status line snippet

```bash
# only show the indicator when a status is set
set -g status-right '#(tmux show -t #{pane_id} -pv @pane_status) | %H:%M'
```

If you pair this with a custom notifier, mirror the filter set supported by `@sidebar_notifications_events` — see [Notifications](/tmux-agent-sidebar/features/notifications/).
