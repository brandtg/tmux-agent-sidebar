---
title: Agent support overview
description: What the sidebar shows for Claude Code, Codex, OpenCode, and Hermes, side by side.
---

Claude Code, Codex, OpenCode, and Hermes work with the sidebar, but they expose different sets of hooks — so the sidebar's surface area is narrower for Codex, OpenCode, and Hermes than it is for Claude Code.

## Feature support by agent

| Feature                                  | Claude Code | Codex        | OpenCode     | Hermes       | Notes                                                                                                                           |
| ---------------------------------------- | ----------- | ------------ | ------------ | ------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| Base status tracking                     | ✓           | ✓            | ✓            | ✓            | Covers `running`, `idle`, and `error`; `waiting` and `background` depend on agent-specific hooks                                |
| Prompt text display                      | ✓           | ✓            | ✓            | ✓            | Saved from `UserPromptSubmit` (Hermes: `pre_llm_call`)                                                                           |
| Response text display (`▷ ...`)          | ✓           | ✓            | ✓            | ✓            | Populated from the `Stop` payload (Hermes: `post_llm_call`)                                                                      |
| Background shell state                   | ✓           | —            | —            | —            | Claude Bash tools can report `run_in_background`; the others do not currently document a background Bash flag                    |
| Waiting status + wait reason             | ✓           | —            | ✓            | ✓            | OpenCode maps permission prompts to waiting notifications; Hermes maps `on_human_input_request` (`approval`/`sudo` → permission, `clarify` → question); Claude also has `Notification`, `PermissionDenied`, and `TeammateIdle` |
| API failure reason display               | ✓           | —            | ✓            | —            | `StopFailure` is wired only for Claude and OpenCode                                                                             |
| Permission badge                         | ✓ (`plan` / `edit` / `auto` / `!`) | ✓ (`auto` / `!` only) | — | — | Codex badges are inferred from process arguments; OpenCode and Hermes do not expose permission modes                             |
| Git branch display                       | ✓           | ✓            | ✓            | ✓            | Uses the pane `cwd`; Claude updates dynamically via `CwdChanged`                                                                |
| Elapsed time                             | ✓           | ✓            | ✓            | ✓            | Since the last prompt                                                                                                            |
| Task progress                            | ✓           | —            | —            | —            | Requires `PostToolUse`; Codex fires `PostToolUse` only for `Bash`, and OpenCode and Hermes do not surface task progress          |
| Task lifecycle notifications             | ✓           | ✓ (`Stop` only) | ✓         | ✓ (`Stop` only) | `Stop` desktop notifications fire for all four. `Notification`, `TaskCompleted`, `StopFailure`, and `PermissionDenied` vary.   |
| Sub-agent display                        | ✓           | —            | —            | ✓            | Requires `SubagentStart` / `SubagentStop`; Hermes wires `subagent_start` / `subagent_stop` (`delegate_task` children)            |
| Activity log                             | ✓           | ✓ (Bash only) | ✓           | ✓            | Codex's `PostToolUse` fires only for `Bash`; OpenCode records the tool events the plugin bridge receives; Hermes records every `post_tool_call` |
| Worktree lifecycle tracking              | ✓           | —            | —            | —            | Requires `WorktreeCreate` / `WorktreeRemove`                                                                                     |
