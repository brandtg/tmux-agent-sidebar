---
title: Hermes
description: What the sidebar shows for Hermes panes, and how the shell hooks map to sidebar events.
---

Hermes works with the sidebar through its shell hooks (a `hooks:` block in
`~/.hermes/config.yaml`), so the visible surface is close to Claude Code's,
with a few Hermes-specific gaps.

## What you get

### Status and prompts

- Live status: `pre_llm_call` marks the pane running, `post_llm_call` marks it
  idle (Hermes' `post_llm_call` is the turn-end hook)
- Prompt text from `pre_llm_call`'s `user_message`
- Response preview (`▷ ...`) from `post_llm_call`'s `assistant_response`
- Elapsed time for the current turn

### Attention cues

- Waiting status + wait reason from `on_human_input_request`: dangerous-command
  approvals and sudo prompts map to `permission`, `clarify` questions map to
  `question`
- The wait clears on `on_human_input_resolved` — the moment you answer, the
  pane returns to running
- `notification` desktop alerts for blocking prompts

### Activity log

- Tool calls recorded from `post_tool_call`, which fires for every tool.
  Hermes names are normalized onto the shared vocabulary (`terminal` → Bash,
  `read_file` → Read, `write_file` → Write, `patch` → Edit, `delegate_task` →
  Agent, …); unknown tools pass through.

### Sub-agents

- `subagent_start` / `subagent_stop` track `delegate_task` children, keyed by
  the child role.

### Git

- Branch display from the pane's `cwd`

## What is not available

| Feature                     | Why |
| --------------------------- | --- |
| Permission badge            | Hermes payloads carry no permission-mode field |
| Background shell state      | Hermes background processes have no sidebar-visible flag |
| API failure reason          | No `StopFailure` equivalent is wired (`api_request_error` is per-provider-attempt, too noisy) |
| Task progress counter       | Hermes has no todo-style tool feeding the progress display |
| Worktree lifecycle tracking | Hermes does not emit worktree create/remove hooks |

Note that `on_session_end` fires after every turn and is deliberately not
wired; `on_session_finalize` drives cleanup instead. Crashes and killed panes
are handled by the sidebar's stale-state sweep, same as Codex and OpenCode.

## Setup

Generate and merge the hook block from [Hermes setup](/tmux-agent-sidebar/getting-started/hermes/).
