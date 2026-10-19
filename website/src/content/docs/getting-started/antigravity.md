---
title: Antigravity setup
description: How the Antigravity (agy) CLI hooks get wired — automatically on plugin load, or by hand.
---

Antigravity works with the sidebar through its native lifecycle hooks (see the
[upstream hook docs](https://antigravity.google/docs/hooks/)). The sidebar
ships a generator that prints the exact block to paste.

## Automatic wiring (default)

`make install` and every plugin reload (the `tmux-agent-sidebar.tmux` entry
script) generate an agy **plugin bundle** — the same shape as the Claude Code
plugin — and symlink it into agy's global plugin directory:

```
~/.cache/tmux-agent-sidebar/agy-plugin/   # generated (hooks.json + plugin.json)
~/.gemini/config/plugins/tmux-agent-sidebar -> ~/.cache/...   # symlink agy loads
```

New `agy` sessions load the hooks from the bundle automatically; no manual
step. The bundle is regenerated on every plugin load, so a rebuilt or moved
install stays correct. Verify with `agy plugin validate
~/.gemini/config/plugins/tmux-agent-sidebar` — it should report `hooks 1`.

**Running sessions keep the hooks they started with.** Restart `agy`
(exit and relaunch) after the first install to pick the bundle up.

## Manual wiring (alternative)

Run the setup subcommand and copy the printed JSON:

```sh
tmux-agent-sidebar setup antigravity
```

Paste it into the global `~/.gemini/config/hooks.json` (create the file if it
does not exist). Because Antigravity's `hooks.json` maps **hook names** to
their event configurations, the sidebar's block appears as one top-level
`"tmux-agent-sidebar"` key — merge it alongside any hook names you already
have instead of replacing the file.

For a workspace-local install, paste the same block into
`<workspace>/.agents/hooks.json` instead. Workspace hooks are scoped to
sessions started in that workspace.

## Restart Antigravity

Hooks load at session start. Restart any running `agy` session after editing
the global config (the CLI reloads workspace hooks when the workspace changes).

## How it works

Each hook fires a subprocess with a JSON payload on stdin; `hook.sh` forwards
it to the sidebar binary, which writes the pane's tmux options. The mapping:

| Antigravity event | Sidebar event       | Used for |
| ----------------- | ------------------- | -------- |
| `SessionStart`    | `session-start`     | Pane discovery, workspace path, conversation id |
| `PreInvocation`   | `user-prompt-submit`| Prompt text (from the conversation transcript), running status, elapsed clock |
| `PostToolUse`     | `activity-log`      | Activity log (all tools) |
| `Stop`            | `stop` / `stop-failure` | Response preview (from the transcript), idle status + attention; reason `error` marks the pane failed |

`PreInvocation` fires before every model call in a turn and its payload
carries no prompt text, so the sidebar recovers the prompt from the newest
`USER_INPUT` step of the transcript the payload points at
(`transcriptPath`) — the same file Antigravity's own transcript viewer
reads. A missing or unreadable transcript degrades to a status-only update.

Conversation titles come from
`~/.gemini/antigravity-cli/conversation_summaries.db`, read by a background
poll every 10 seconds and matched to panes by conversation id — the same
mechanism that covers Claude `/rename` labels and Hermes session titles.

`PreToolUse` and `PostInvocation` are supported upstream but not wired: the
PreToolUse payload cannot distinguish an approval prompt from an
auto-approved call, and PostInvocation adds nothing the Stop hook does not
already cover.

Antigravity parses hook stdout as the hook response, so the sidebar's hook
prints nothing — an empty response is the documented no-op.

## Pane discovery without hooks

The sidebar also detects Antigravity panes before any hook fires by the
`agy` executable name in the pane's process tree, so the row appears
immediately (with idle state) and refines once hooks fire.
