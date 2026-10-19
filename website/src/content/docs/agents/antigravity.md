---
title: Antigravity
description: What the sidebar shows for Antigravity (agy) panes, and how the CLI's hooks map to sidebar events.
---

Antigravity works with the sidebar through its native lifecycle hooks
(`~/.gemini/config/hooks.json`). Its hook payloads carry no prompt or
response text, so the sidebar recovers both from the conversation transcript
the payloads point at (`transcriptPath`) and the conversation title from
Antigravity's summaries store.

## What you get

### Status and prompts

- Live status: `PreInvocation` marks the pane running (it fires before every
  model call in the turn), `Stop` marks it idle
- Prompt text from the newest `USER_INPUT` step of the conversation
  transcript, recovered on `PreInvocation` (the hook payload itself carries
  no prompt)
- Response preview (`▷ ...`) from the final `PLANNER_RESPONSE` transcript
  step, recovered on `Stop`
- Elapsed time for the current turn

### Session titles

- Auto-generated conversation titles ("Create And List File") read from
  `~/.gemini/antigravity-cli/conversation_summaries.db` by the same
  background poll that covers Claude `/rename` labels and Hermes titles —
  a `conversation_id → title` match against the pane's session id. A
  user-set `/title` label wins over the auto-generated one.

### Attention cues

- The finished-turn flag on `Stop` (breathing green until you focus the pane)
- A `Stop` with termination reason `error` (or a populated `error` field)
  marks the pane failed and surfaces the error in the feed
- Desktop notifications on turn completion and on failed turns

### Activity log

- Tool calls recorded from `PostToolUse`, which fires for every tool.
  Antigravity names are normalized onto the shared vocabulary (`run_command`
  → Bash, `view_file` → Read, `write_to_file` → Write,
  `replace_file_content` → Edit, `grep_search` → Grep, `invoke_subagent` →
  Agent, …); unknown tools pass through.

### Git

- Branch display from the workspace path (`workspacePaths[0]`)

## What is not available

| Feature                     | Why |
| --------------------------- | --- |
| Waiting status + wait reason | `PreToolUse` is not wired — its payload cannot distinguish an approval prompt from an auto-approved call |
| Permission badge            | Hook payloads carry no permission-mode field |
| Background shell state      | `run_command`'s `RunPersistent` flag is not mapped |
| Task progress counter       | No todo-style tool feeding the progress display |
| Sub-agent display           | No subagent lifecycle hooks (`invoke_subagent` appears in the activity log only) |
| Worktree lifecycle tracking | Antigravity does not emit worktree create/remove hooks |

## Setup

Wired automatically: `make install` and every plugin reload generate an agy
plugin bundle and link it into `~/.gemini/config/plugins/` — new `agy`
sessions load the hooks from it. See [Antigravity setup](/tmux-agent-sidebar/getting-started/antigravity/)
for the manual `~/.gemini/config/hooks.json` path and the restart note
(running sessions keep the hooks they started with).
Titles need the `sqlite3` CLI on `PATH` (present by default on macOS and
most Linux distributions); without it, panes keep the plain `antigravity`
label and everything else keeps working.
