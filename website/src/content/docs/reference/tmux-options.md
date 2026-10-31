---
title: tmux options
description: Every @sidebar_* / @agent-sidebar-* option the plugin reads.
---

Most options must be set **before** loading the plugin in your `tmux.conf`. Color values can be xterm 256-color numbers (`0`-`255`) or six-digit hex colors (`#RRGGBB` or `RRGGBB`); icons can be any Unicode glyph.

## Sidebar behavior

| Option                           | Default | Description                                                                             |
| -------------------------------- | ------- | --------------------------------------------------------------------------------------- |
| `@sidebar_key`                   | `e`     | Prefix-triggered keybinding to toggle the sidebar in the current window                 |
| `@sidebar_key_all`               | `E`     | Prefix-triggered keybinding to toggle the sidebar in all windows                        |
| `@sidebar_width`                 | `15%`   | Width in columns or as a percentage                                                     |
| `@sidebar_position`              | `left`  | Sidebar placement (`left` or `right`)                                                   |
| `@sidebar_default_view`          | `all`   | Status filter view the sidebar lands on when it opens (`all`, `running`, `background`, `waiting`, `idle`, `error`). Filter changes still sync across open sidebars but are not restored on reopen |
| `@sidebar_group_by`              | `repo`  | How the agents panel groups panes: `repo` buckets them by git repo root, `session` by tmux session name. With `w` (other windows) enabled, `session` turns the panel into an agent-forward session manager — sessions without agents list their plain windows too. Set the default here, or toggle it at runtime with `s` (shared across open sidebars) |
| `@sidebar_default_compact_view`  | `off`   | Whether a newly opened sidebar starts in compact mode — one line per agent. Toggle live with `c` inside the sidebar; the toggle syncs across open sidebars but is not restored on reopen |
| `@sidebar_auto_create`           | `on`    | Auto-create the sidebar on new windows and sessions (set `off` to disable). Consulted at window/session-create time, and only while no `@sidebar_mode` is recorded — a toggle-all sweep overrides it until the next toggle-all |
| `@sidebar_mode`                  | unset   | Sidebar mode written by `prefix+E` (globally) (`open` / `closed`). New windows and sessions follow it, so toggle-all means "always on/off everywhere" rather than a one-shot sweep. Per-window toggles ignore it. Unset falls back to `@sidebar_auto_create` |
| `@sidebar_auto_create_delay`     | `0`     | Seconds to defer auto-create after a window or session opens, so a declaratively-built window (e.g. tmuxinator's `select-layout`) finishes before the sidebar pane is injected; accepts fractional seconds. `0` keeps the create synchronous |
| `@sidebar_popup_max_width`       | `100`   | Windows narrower than this many columns open the sidebar as a tmux popup instead of a split pane — for phones and small SSH viewports. Entering the popup view stops any split-pane sidebars still running in the session, and a toggle press on a narrow window swaps a desktop split pane for the popup directly. Auto-create skips narrow windows; the toggle-all sweep ignores the threshold so "open everywhere" stays uniform. Set `0` to always use a split pane |
| `@sidebar_auto_hide`             | `on`    | Auto-hide a window's split-pane sidebar when the window shrinks below `@sidebar_popup_max_width` (a phone or other small client attached, or resized the window): the sidebar pane closes and the window's saved layout replays, so the real panes keep the whole viewport. Stateless and per-window — nothing records which windows were hidden and nothing reopens them automatically; bring the sidebar back with the toggle/summon keys when a wide viewport is driving the window again, or via auto-create on newly created windows. Only an actual downward crossing of the threshold counts, so sidebars the toggle-all sweep deliberately created on narrow windows are left alone, and the hook's regular firings never act. Set `off` to keep split panes at every width |
| `@sidebar_notifications`         | `on`    | Master switch for desktop notifications                                                 |
| `@sidebar_notifications_events`  | unset   | Restrict events — see [Notifications](/tmux-agent-sidebar/features/notifications/)       |
| `@sidebar_agents`                | unset   | Comma-separated allow-list of agents (`claude`, `codex`, `opencode`, `hermes`, `antigravity`) the ⓘ notices evaluates for missing hooks — e.g. `set -g @sidebar_agents "claude,opencode"` silences the Codex warning for non-Codex users. Unset checks every agent; pane rows and status parsing are unaffected |

## Worktree spawn defaults

| Option                            | Default     | Description                                               |
| --------------------------------- | ----------- | --------------------------------------------------------- |
| `@agent-sidebar-default-agent`    | `claude`    | Agent launched by `n`&nbsp;(also accepts `codex`, `opencode`, `antigravity`) |
| `@agent-sidebar-branch-prefix`    | `agent/`    | Branch prefix for new worktrees                           |
| `@agent-sidebar-worktree-dir`     | `.worktrees` | Repo-relative directory for sidebar-spawned worktrees; absolute paths and `..` are rejected |

## Status and filter colors

| Option                            | Default         | What it paints                                                    |
| --------------------------------- | --------------- | ----------------------------------------------------------------- |
| `@sidebar_color_all`              | `111`&nbsp;(sky blue)| Selected "all" filter icon                                        |
| `@sidebar_color_running`          | `114`&nbsp;(green)   | Selected running/background filter icon and running/background pane status |
| `@sidebar_color_waiting`          | `221`&nbsp;(yellow)  | Selected waiting filter icon, waiting pane status, version banner |
| `@sidebar_color_idle`             | `110`&nbsp;(soft blue) | Selected idle filter icon and idle pane status                  |
| `@sidebar_color_error`            | `167`&nbsp;(soft red) | Selected error filter icon and error pane status                 |
| `@sidebar_color_filter_inactive`  | `245`&nbsp;(mid gray) | Unselected status filter icons and zero counts                   |

## Structural colors

| Option                     | Default              | What it paints                                                                                          |
| -------------------------- | -------------------- | ------------------------------------------------------------------------------------------------------- |
| `@sidebar_color_border`    | `240`&nbsp;(dark gray)    | Unfocused panel borders                                                              |
| `@sidebar_color_accent`    | `153`&nbsp;(pale sky blue) | Active pane marker, focused repo header, repo popup border — the brand color |
| `@sidebar_color_session`   | `39`&nbsp;(blue)          | Session name                                                                                            |
| `@sidebar_color_selection` | `239`&nbsp;(dark gray)    | Selected row background                                                                                 |

## Agent colors

| Option                          | Default            | What it paints       |
| ------------------------------- | ------------------ | -------------------- |
| `@sidebar_color_agent_claude`   | `173`&nbsp;(claude orange) | Claude brand color   |
| `@sidebar_color_agent_codex`    | `141`&nbsp;(purple)     | Codex brand color    |
| `@sidebar_color_agent_opencode` | `117`&nbsp;(light blue) | OpenCode brand color |
| `@sidebar_color_agent_hermes`   | `214`&nbsp;(orange)     | Hermes brand color   |
| `@sidebar_color_agent_antigravity` | `78`&nbsp;(aquamarine) | Antigravity brand color |

## Text colors

| Option                         | Default          | What it paints                                                                                   |
| ------------------------------ | ---------------- | ------------------------------------------------------------------------------------------------ |
| `@sidebar_color_text_active`   | `255`&nbsp;(white)    | Primary text — active rows, counts, filtered repo label                                          |
| `@sidebar_color_text_muted`    | `252`&nbsp;(light gray) | Secondary text — tree branches, empty-state messages, dirty counts |
| `@sidebar_color_text_inactive` | `244`&nbsp;(mid gray) | Body text of unfocused pane rows — prompt / response, idle hint                                  |
| `@sidebar_color_port`          | `246`&nbsp;(light gray) | Port numbers                                                                                   |
| `@sidebar_color_wait_reason`   | `221`&nbsp;(yellow)   | Wait reason text                                                                                 |
| `@sidebar_color_response_arrow`| `81`&nbsp;(bright cyan) | Response arrow                                                                                 |

## Task and sub-agent colors

| Option                          | Default           | What it paints        |
| ------------------------------- | ----------------- | --------------------- |
| `@sidebar_color_task_progress`  | `223`&nbsp;(pale yellow) | Task progress summary |
| `@sidebar_color_subagent`       | `73`&nbsp;(soft teal)  | Sub-agent tree        |

## Git colors

| Option                          | Default            | What it paints      |
| ------------------------------- | ------------------ | ------------------- |
| `@sidebar_color_branch`         | `109`&nbsp;(teal)       | Git branch name     |
| `@sidebar_color_diff_added`     | `114`&nbsp;(green)      | Ahead arrows        |
| `@sidebar_color_diff_deleted`   | `174`&nbsp;(terracotta) | Behind arrows       |

## Section titles

| Option                                | Default      | What it paints      |
| ------------------------------------- | ------------ | ------------------- |
| `@sidebar_color_section_title`        | `109`&nbsp;(teal) | Section titles      |

## Status icons

Any Unicode glyph works. Make sure the glyphs render in your terminal font.

Some fonts draw the default geometric shapes (`●` `◎` `◐`) wider than one
cell, so they bleed into the neighboring column (Berkeley Mono + Nerd Fonts
in Alacritty is a common case). Set `@sidebar_icon_set` to swap the whole
palette at once:

| Value   | Icons                                                                                          |
| ------- | ---------------------------------------------------------------------------------------------- |
| `nerd`  | Nerd Fonts glyphs (`nf-fa-bars`, `nf-fa-circle`, `nf-fa-circle-o-notch`, `nf-fa-adjust`, `nf-fa-circle-o`, `nf-fa-times`, `nf-fa-circle-thin`) — fitted to the cell by the patched font, so they don't overflow |
| `ascii` | Plain ASCII (`=` `*` `+` `~` `-` `x` `.`) — renders identically in every font                   |

Per-icon options below always win over the preset.

| Option                  | Default | Meaning                      |
| ----------------------- | ------- | ---------------------------- |
| `@sidebar_icon_all`     | `≡`     | Status filter bar "all" icon |
| `@sidebar_icon_running`    | `●`     | Running status icon          |
| `@sidebar_icon_background` | `◎`     | Background shell status icon |
| `@sidebar_icon_waiting`    | `◐`     | Waiting status icon          |
| `@sidebar_icon_idle`    | `○`     | Idle status icon             |
| `@sidebar_icon_error`   | `✕`     | Error status icon            |
| `@sidebar_icon_unknown` | `·`     | Unknown status icon          |

## Internal options

Written and maintained by the plugin itself; no need to touch them.

| Option                          | Scope   | Purpose                                                                                                                                    |
| ------------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `@sidebar_last_window_width`    | window  | Last `window_width` observed by the `maintain-width` hook, so the auto-hide decision only fires on an actual downward crossing of the popup threshold |

## Example config

```bash
# Behavior
set -g @sidebar_key T
set -g @sidebar_width 32
set -g @sidebar_position right
set -g @sidebar_notifications_events "stop,notification"
set -g @agent-sidebar-default-agent codex

# Colors
set -g @sidebar_color_accent 117
set -g @sidebar_color_agent_claude "#d97757"
set -g @sidebar_color_agent_opencode 39

# Icons
set -g @sidebar_icon_set nerd
set -g @sidebar_icon_running '▶'
set -g @sidebar_icon_error   '⚠'

run-shell ~/.tmux/plugins/tmux-agent-sidebar/tmux-agent-sidebar.tmux
```
