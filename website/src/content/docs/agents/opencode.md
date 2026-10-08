---
title: OpenCode
description: What the sidebar shows for OpenCode panes, and how the plugin bridge maps its events.
---

OpenCode works with the sidebar through a local plugin bridge, so the visible
surface is similar to Codex but with a different event source.

## What you get

### Status and prompts

- Live status from `session.created` / `session.status` / `session.idle`
- Prompt text from `chat.message`
- Response preview (`▷ ...`) from `stop`
- Elapsed time for the current turn: starts at the last user prompt and keeps counting across responses, retries, and permission waits until OpenCode goes idle. Subagent runs (the `task` tool spawns them in child sessions) are part of the same turn — their lifecycle events are filtered out so they cannot stop the clock mid-turn or restart it after the turn ends

### Attention cues

- Waiting status + wait reason from `permission.asked` and `question.asked`
- The wait clears when OpenCode emits `permission.replied` / `question.replied` / `question.rejected`. While a prompt is open the pane stays `waiting` even if a parallel tool call finishes, so the cue cannot be missed
- API failure reason from `session.error` / `session.status=error`
- `notification` desktop alerts for permission and question prompts

### Activity log

- Tool calls recorded from `tool.execute.after`

### Git

- Branch display from the pane's `cwd`

## What is not available

| Feature                    | Why |
| -------------------------- | --- |
| Permission badge           | OpenCode does not expose the Claude-style permission modes |
| Background shell state     | OpenCode does not currently document a background Bash flag |
| Task progress counter      | The bridge does not map a task-progress event |
| Sub-agent tree             | OpenCode does not emit Claude-style sub-agent hooks |
| Worktree lifecycle tracking | OpenCode does not emit `WorktreeCreate` / `WorktreeRemove` |

## Setup

Wire the plugin bridge from [OpenCode setup](/tmux-agent-sidebar/getting-started/opencode/).
