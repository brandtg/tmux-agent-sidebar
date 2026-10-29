---
title: Scripting
description: Read agent status from your own shell scripts or status bar.
---

The sidebar writes agent state into tmux pane options on every hook event, so you can pick them up from any script with `tmux show -t <pane> -pv <key>`.

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
| `@pane_name`               | Friendly agent/session name (from `/rename` on Claude)              |
| `@pane_role`               | `sidebar` for the sidebar pane itself; empty for agent panes        |
| `@pane_prompt`             | Latest user prompt text or response preview                         |
| `@pane_prompt_source`      | `user` when the prompt field holds the user's prompt, `response` when it holds the agent's last reply |
| `@pane_started_at`         | Epoch seconds of the last `UserPromptSubmit`                        |
| `@pane_wait_reason`        | Wait-reason text (populated only when waiting)                      |
| `@pane_bg_cmd`             | Latest sanitized background Bash command (Claude `run_in_background`, Hermes `terminal(background=true)`); empty when no bg shell is tracked. Cleared automatically by a `ps` liveness sweep when the process exits. |
| `@pane_bg_pid`             | PID of the tracked background process when the spawn result reported one (Hermes). The sweep prefers it over the command-string match. |
| `@pane_subagents`          | Comma-separated subagent labels (Claude `Task`, Hermes `delegate_task`). For Hermes a non-empty list survives the parent's Stop — async children are still running — and the final `subagent_stop` drops the pane back to idle. |
| `@pane_cwd`                | Working directory reported by the agent (preferred over `pane_current_path`) |
| `@pane_permission_mode`    | Permission-mode string for the pane (`plan` / `acceptEdits` / `auto` / `bypassPermissions` / …); only `plan` and `!` render as badges |
| `@pane_worktree_name`      | Worktree label when the pane was spawned from the sidebar           |
| `@pane_worktree_branch`    | Branch that was auto-created for the worktree                       |
| `@pane_session_id`         | Agent session ID (opaque; useful for correlating logs)              |

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
