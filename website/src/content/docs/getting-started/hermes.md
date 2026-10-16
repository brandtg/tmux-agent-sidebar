---
title: Hermes setup
description: Wire up the Hermes Agent shell hooks from the generated config snippet.
---

Hermes works with the sidebar through its built-in shell hooks (`hooks:` block
in `~/.hermes/config.yaml`). No plugin authoring required — the sidebar ships a
generator that prints the exact block to paste.

## Generate the hook config

Run the setup subcommand and copy the printed YAML:

```sh
tmux-agent-sidebar setup hermes
```

This prints a `hooks:` block with one shell-hook entry per supported Hermes
event, each calling the plugin's `hook.sh` with the matching sidebar event.
Merge it into `~/.hermes/config.yaml` under the top-level `hooks:` key,
preserving all existing config (and any hooks you already have). If you use
named profiles, merge it into that profile's config instead — hooks are
per-profile.

## Consent

Hermes prompts once per `(event, command)` pair the first time a hook fires.
For non-interactive or hands-off setup, either:

- set `hooks_auto_accept: true` in `~/.hermes/config.yaml`, or
- mirror each emitted `command` string verbatim into the `approvals` array of
  `~/.hermes/shell-hooks-allowlist.json`:

```json
{
  "approvals": [
    { "event": "post_tool_call", "command": "bash ~/.tmux/plugins/tmux-agent-sidebar/hook.sh hermes activity-log" }
  ]
}
```

## Restart Hermes

Hooks register at startup. Restart the Hermes session (or gateway) after
editing the config so the block takes effect, and answer the consent prompt
if you did not pre-approve the commands.

## How it works

Each hook fires a subprocess with a JSON payload on stdin; `hook.sh` forwards
it to the sidebar binary, which writes the pane's tmux options. The mapping:

| Hermes event          | Sidebar event       | Used for |
| --------------------- | ------------------- | -------- |
| `on_session_start`    | `session-start`     | Pane discovery + metadata |
| `pre_llm_call`        | `user-prompt-submit`| Prompt text, running status, elapsed clock |
| `post_llm_call`       | `stop`              | Response preview, idle status |
| `post_tool_call`      | `activity-log`      | Activity log (all tools) |
| `on_human_input_request` | `notification`   | Waiting status + wait reason (`approval`/`sudo` → permission, `clarify` → question) |
| `on_human_input_resolved` | `user-prompt-submit` | Clears the wait when you answer |
| `on_session_finalize` | `session-end`       | Cleanup on CLI/TUI teardown |
| `subagent_start` / `subagent_stop` | `subagent-*` | Sub-agent display for `delegate_task` children |

`on_session_end` is deliberately not wired — despite its name it fires after
every turn; `on_session_finalize` is the real teardown hook.

## Pane discovery without hooks

The sidebar can also detect a Hermes pane before any hook fires. Hermes'
launcher is a Python shim whose process name is `hermes`, so the sidebar walks
the pane's process tree to classify it. The row appears with idle state and
refines once hooks fire.
