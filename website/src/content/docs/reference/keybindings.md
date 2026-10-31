---
title: Keybindings
description: Every shortcut in the sidebar, the worktree spawn modal, and the close-pane modal.
---

## Sidebar

| Key            | Action                                                        |
| -------------- | ------------------------------------------------------------- |
| `prefix + e`   | Toggle sidebar                                                |
| `prefix + E`   | Toggle sidebar in all windows                                 |
| `j` / `Down`   | Move selection down                                           |
| `k` / `Up`     | Move selection up                                             |
| `/`            | Search agents by session title, tmux session, or branch       |
| `a`            | Jump to the next row that needs attention                     |
| `g` / `G`      | Jump to the top / bottom of the list                          |
| `[` / `]`      | Jump to the previous / next group header                      |
| `z`            | Collapse / expand the selected group                          |
| `Z`            | Expand every collapsed group                                  |
| `s`            | Toggle repo / tmux-session grouping                           |
| `PageUp` / `PageDown` | Page the agent list by a viewport                      |
| `h` / `Left`   | Previous status filter                                        |
| `l` / `Right`  | Next status filter; jump to the selected pane |
| `r`            | Open repo filter popup                                        |
| `Enter` / `→`  | Jump to the selected pane                                     |
| `Tab`          | Cycle status filter                                           |
| `c`            | Toggle compact view (one line per agent)                      |
| `w`            | Toggle listing non-agent panes (long-running tasks)           |
| `t`            | Open the TODO panel; focus it when open, close it when focused |
| `f`            | Flag / unflag the selected agent (orange inbox-pin marker)    |
| `?`            | Show keyboard reference popup                                 |
| `Esc`          | Return focus or close the popup                               |
| `q`            | Quit the sidebar                                              |

## Search

Press `/` to filter the agent list without collapsing its groups. Matching is
case-insensitive and checks each row's agent session title, tmux session name,
and git branch. Typing a repo or tmux session name keeps the whole group
visible; otherwise only rows whose own fields match are shown. Status counts
in the filter bar follow the active query.

| Key          | Action                                          |
| ------------ | ----------------------------------------------- |
| `/`          | Open search (reopens with the current query)    |
| Text keys    | Edit the query                                  |
| `↑` / `↓`    | Move the selection through the results          |
| `Ctrl-n` / `Ctrl-p` | Move down / up through the results       |
| `Enter`      | Close the input, keeping the query applied      |
| `Ctrl-u`     | Clear the query                                 |
| `Ctrl-w`     | Delete the word before the cursor               |
| `Esc`        | Clear the query and close                       |

Jumping to a pane clears the query, so leaving the sidebar always returns the
list to its unfiltered state.

## Repo filter popup

Opened with `r` or by clicking the repo filter button in the sidebar header.

| Key           | Action                                 |
| ------------- | -------------------------------------- |
| `j` / `Down`  | Move selection down                    |
| `k` / `Up`    | Move selection up                      |
| `Enter`       | Confirm — filter the list to that repo |
| `Esc`         | Cancel                                 |

## Notices popup

Opened by clicking the `ⓘ` badge shown when hooks or plugin setup are missing.

| Key   | Action          |
| ----- | --------------- |
| `Esc` | Close the popup |

## Help popup

Opened with `?`; a keyboard reference for the shortcuts above.

| Key       | Action          |
| --------- | --------------- |
| `?` / `Esc` | Close the popup |

## TODO panel

Opened with `t`; hidden by default. Items are plain text, word-wrapped, and
shared by every sidebar through `~/.local/state/tmux-agent-sidebar/todos.txt`
(or `$XDG_STATE_HOME` when set). Completed items are hidden by default.

| Key             | Action                                                        |
| --------------- | ------------------------------------------------------------- |
| `t`             | Open the panel; focus it when open but unfocused, close it when focused |
| `j` / `Down`    | Move to the next visible item                                 |
| `k` / `Up`      | Move to the previous visible item                             |
| `PageUp` / `PageDown` | Page the item list                                      |
| `Enter`         | Toggle the selected item's checkbox                           |
| `e`             | Edit the selected item's text (Enter saves, Esc cancels)      |
| `x` / `d`       | Delete the selected item                                      |
| `D`             | Delete all completed items                                    |
| `a`             | Add a new item; while the add line is open, Enter commits and starts a fresh line, Esc ends entry |
| `d`             | Delete the selected item                                      |
| `H`             | Toggle "hide completed" (shared across open sidebars)         |
| `Esc`           | Return focus to the agent list; the panel stays visible       |

Clicking an item toggles its checkbox; clicking the add line opens the input.

## Worktree

| Key | Action                                 |
| --- | -------------------------------------- |
| `n` | Spawn a new worktree + agent           |
| `x` | Remove the selected spawn-created pane |

## Spawn worktree modal

Opened with `n` on a repo.

| Key                                | Action                                                                                           |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| Text keys                          | Type the name (used as the branch slug and tmux window name)                                     |
| `↑` / `↓` / `Tab` / `Shift+Tab`    | Move focus between `NAME` / `AGENT` / `MODE` fields                                              |
| `←` / `→`                          | Cycle the value when the agent or mode field has focus                                           |
| `Enter`                            | Create the worktree + window and launch the agent                                                |
| `Esc`                              | Cancel                                                                                           |

## Close pane modal

Opened with `x` on a spawn-created pane.

| Key             | Action                                                                                                    |
| --------------- | --------------------------------------------------------------------------------------------------------- |
| `y` / `Enter`   | Close the tmux window, remove the git worktree (`--force`), and delete the branch (`git branch -D`)       |
| `c`             | Close the tmux window only, keep the worktree and branch on disk                                          |
| `n` / `Esc`     | Cancel                                                                                                    |
