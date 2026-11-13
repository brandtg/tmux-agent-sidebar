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
(or `$XDG_STATE_HOME` when set). Completed items sink to the bottom of the
list, struck through; `H` collapses the section (shared across open sidebars).

The panel's open/closed state is shared across every sidebar by default: open
it in one tmux session and it opens in the others, so the same list stays in
view as you jump between sessions. Set `@sidebar_todo_scope local` to keep the
panel per sidebar instead; the item list stays shared either way.

The keyboard flow follows Google Tasks: Enter opens the selected task for
editing and a second Enter (or Enter in the new-task line) saves and lands on
a fresh task, so a run of items is entered without leaving the keyboard;
Space completes; Esc backs out without creating anything.

| Key             | Action                                                        |
| --------------- | ------------------------------------------------------------- |
| `t`             | Open the panel; focus it when open but unfocused, close it when focused |
| `j` / `Down`    | Move to the next visible item                                 |
| `k` / `Up`      | Move to the previous visible item                             |
| `Ctrl-↓` / `Ctrl-j` | Move the selected item down within the open items        |
| `Ctrl-↑` / `Ctrl-k` | Move the selected item up within the open items          |
| `PageUp` / `PageDown` | Page the item list                                      |
| `Enter` / `e`   | Edit the selected item's text; with no item selected, start a new one |
| `Enter` (while editing or adding) | Save and start the next item; on an empty line, finish entry |
| `Space`         | Toggle the selected item's checkbox                           |
| `x` / `d`       | Delete the selected item                                      |
| `D`             | Delete all completed items                                    |
| `u` / `Ctrl-z`  | Undo the last delete / clear-completed (available for 4 s)    |
| `a`             | Add a new item (insert mode); Esc ends entry                  |
| `H`             | Collapse / expand the completed section (shared across open sidebars) |
| `Esc`           | Stop editing without saving, end entry, or return focus to the agent list |

Clicking an item's checkbox toggles it; clicking its text selects it, and
clicking the selected item again opens the editor; clicking the open
new-task line re-opens insert mode. The panel advertises nothing else —
`?` (the shortcut overlay) documents the full flow.

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
