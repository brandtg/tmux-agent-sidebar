---
title: OpenCode
description: What the sidebar shows for OpenCode panes, and how the plugin bridge maps its events.
---

OpenCode works with the sidebar through a local plugin bridge, so the visible
surface is similar to Codex but with a different event source.

## What you get

### Status and prompts

- Live status from `session.execution.started` / `succeeded` / `interrupted` /
  `failed` — the v1-era `session.idle` / `session.status` events are still
  understood as a legacy fallback, but a current OpenCode server never emits
  them
- Prompt text from the `prompt` session hook
- Response preview from `stop`
- Elapsed time for the current turn: starts at the last user prompt and keeps counting across responses, retries, and permission waits until OpenCode goes idle. Subagent runs (the `task` tool spawns them in child sessions) are part of the same turn — their lifecycle events are filtered out so they cannot stop the clock mid-turn or restart it after the turn ends

### Session titles

- Titles arrive through the bridge: `session.created` sends the initial title
  and `session.renamed` / `session.updated` updates carry renames; both
  forward as a `session-title` hook event that writes `@pane_session_title`
- As a self-healing path, the sidebar also polls OpenCode's `opencode.db`
  session store every ~10 s, so renames the bridge missed still show up

### Attention cues

- Waiting status + wait reason from `permission.asked` (`permission`) and
  blocking form asks — the `question` tool's `form.created` (`question`,
  with the request nested under `data.form`) and other forms such as MCP
  elicitations (`elicitation_dialog`). On 2.0.x daemons the dedicated
  `question.asked` (`question`) events are used instead
- The wait clears when OpenCode emits `permission.replied` /
  `form.replied` / `form.cancelled` (or the legacy `question.replied` /
  `question.rejected`) — the bridge re-submits an empty prompt so the pane
  returns to running. While a prompt is open the pane stays `waiting` even if
  a parallel tool call finishes, so the cue cannot be missed
- API failure reason from `session.execution.failed`
- `notification` desktop alerts for permission and question prompts

### Activity log

- Tool calls recorded from the `execute.after` tool hook

### Git

- Branch display from the pane's `cwd`

## What is not available

| Feature                    | Why |
| -------------------------- | --- |
| Permission badge           | OpenCode does not expose the Claude-style permission modes |
| Background shell state     | OpenCode does not currently document a background Bash flag |
| Task progress counter      | The bridge does not map a task-progress event |
| Sub-agent tree             | OpenCode does not emit Claude-style sub-agent hooks |
| Worktree lifecycle tracking | The `WorktreeCreate` / `WorktreeRemove` hooks no longer exist in the plugin (see [Worktrees](/tmux-agent-sidebar/features/worktree/)) |

## Setup

Wire the plugin bridge from [OpenCode setup](/tmux-agent-sidebar/getting-started/opencode/).
