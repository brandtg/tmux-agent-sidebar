---
title: Keybindings
description: Every shortcut in the sidebar, the worktree spawn modal, and the close-pane modal.
---

## Sidebar

| Key            | Action                                                        |
| -------------- | ------------------------------------------------------------- |
| `prefix + e`   | Toggle sidebar                                                |
| `prefix + E`   | Toggle sidebar in all windows                                 |
| `prefix + M-1` … `prefix + M-5` | Sidebar-aware builtin layouts (even-horizontal, even-vertical, main-horizontal, main-vertical, tiled): the sidebar keeps its edge and width while the real panes get the builtin's geometry. Replaces tmux's stock `select-layout` bindings only where they are still at their defaults; requires tmux 3.8+. Applying one records the window's `@sidebar_layout_base`, after which layout changes re-pin the sidebar width and closing the sidebar restores that builtin. Set `@sidebar_layout_keys off` before loading to keep the stock bindings |
| `j` / `Down` / `Ctrl-n` | Move selection down                                   |
| `k` / `Up` / `Ctrl-p`   | Move selection up                                     |
| `/`            | Search agents by session title, tmux session, agent label, or branch |
| `a`            | Jump to the next row that needs attention                     |
| `g` / `G`      | Jump to the top / bottom of the list                          |
| `[` / `]`      | Jump to the previous / next group header                      |
| `z`            | Collapse / expand the selected group (clicking a group header folds it too) |
| `Z`            | Fold or expand every group in one key: from a fully expanded list it collapses all; with anything folded it expands all |
| `s`            | Toggle repo / tmux-session grouping                           |
| `PageUp` / `PageDown` | Page the agent list by a viewport                      |
| `r`            | Open repo filter popup                                        |
| `Enter` / `l` / `Right` | Jump to the selected pane                            |
| `o`            | Open the fullscreen agent overview grid popup                 |
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
agent label, and git branch. Typing a repo or tmux session name (the group's
own name) keeps the whole group visible; otherwise only rows whose own fields
match are shown. Status counts in a collapsed group's badge follow the active
query.

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

## Agent overview grid

Opened with `o` (or the header `☰`) inside the sidebar. From anywhere, set
`@sidebar_overview_key` in `tmux.conf` (before loading the plugin) to bind
it globally, e.g. `set -g @sidebar_overview_key O` gives `prefix + O`.
The underlying command is bindable by hand too:
`tmux-agent-sidebar overview --popup --popup-client "#{client_name}"`.

| Key            | Action                                                        |
| -------------- | ------------------------------------------------------------- |
| `Tab` / `→` / `j` | Select the next cell                                      |
| `BackTab` / `←` / `k` | Select the previous cell                              |
| Mouse click    | Select a cell; click the selected cell again to zoom it       |
| `Enter`        | Zoom the selected cell; while zoomed, step into the real pane |
| `p`            | Prompt bar: type a prompt, `Enter` sends it to that pane, `Esc` cancels (supports the TODO editor's keys — `Ctrl-w`, `Ctrl-arrows`, `Ctrl-a/e/u`) |
| `f`            | Close the popup and focus that agent's real pane              |
| `s`            | Toggle cell order: sidebar order (default) / status-first     |
| `q` / `Esc`    | Quit (Esc collapses zoom first)                               |

## Repo filter popup

Opened with `r` or by clicking the menu button at the right edge of the
sidebar header (`ⓘ` when something needs attention, `—` otherwise).

| Key           | Action                                 |
| ------------- | -------------------------------------- |
| `j` / `Down` / `Ctrl-n` | Move selection down          |
| `k` / `Up` / `Ctrl-p`   | Move selection up            |
| `Enter`       | Confirm — filter the list to that repo |
| `Esc`         | Cancel                                 |

## Notices

There is no standalone notices popup: notices render as a section at the top
of the repo filter popup, above the repo list. The popup opens with `r` or by
clicking the `ⓘ` menu button in the sidebar header (shown when hooks or
plugin setup need attention). Notice lines are click-to-copy.

| Key   | Action          |
| ----- | --------------- |
| `Esc` | Close the popup |

## Help popup

Opened with `?`; a keyboard reference for the shortcuts above.

| Key       | Action          |
| --------- | --------------- |
| `?` / `Esc` | Close the popup |
| `j` / `Down` / `k` / `Up` | Scroll one line |
| `PageDown` / `PageUp` | Page the content |

## TODO panel

Opened with `t`; hidden by default. Items are plain text, word-wrapped, and
shared by every sidebar through `~/.local/state/tmux-agent-sidebar/todos.txt`
(or `$XDG_STATE_HOME` when set). Completed items sink to the bottom of the
list, struck through; `H` collapses the section (shared across open sidebars).

The panel's open/closed state is shared across every sidebar by default: open
it in one tmux session and it opens in the others, so the same list stays in
view as you jump between sessions. Set `@sidebar_todo_scope local` to keep the
panel per sidebar instead; the item list stays shared either way.

A todo whose text is a resume command — `claude --resume <id>`,
`codex resume <id>`, or `opencode -s <id>` — is marked with a dim `↻`, and `r`
relaunches it in a new window in the sidebar's tmux session. The working
directory resolves from a trailing `@ <path>` suffix on the item
(e.g. `[ ] claude --resume f4ac0121-… @ ~/Projects/wisy-cloud`), then from
parked-session records, then from the agent's own session store; without a
cwd the launch refuses rather than guessing. The launched command is rebuilt
from the recognized id — the item's text is never executed.

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
| `y`             | Copy the selected item's text to the clipboard                |
| `r`             | Resume a resume-shaped item (`claude --resume <id>`, `codex resume <id>`, `opencode -s <id>`) in a new window |
| `x` / `d`       | Delete the selected item                                      |
| `D`             | Delete all completed items                                    |
| `u` / `Ctrl-z`  | Undo the last delete / clear-completed (4 s window; works from any focus while no text input or search is open) |
| `a`             | Add a new item (insert mode); Esc ends entry                  |
| `H`             | Collapse / expand the completed section (shared across open sidebars) |
| `Esc`           | Stop editing without saving, end entry, or return focus to the agent list |

Text entry uses the usual line-editor keys: `Home` / `End` (or `Ctrl-a` /
`Ctrl-e`) to jump to the start / end of the line, `Ctrl-←` / `Ctrl-→` to move
by word, `Delete` to drop the character after the cursor, `Ctrl-u` to clear
the line, and `Ctrl-w` to delete the word before the cursor. While the panel
has focus, `c` (compact), `w` (other windows), `?` (help), and `/` (search)
stay live, and `q` quits.

Clicking an item's checkbox toggles it; clicking its text selects it, and
clicking the selected item again opens the editor; clicking the open
new-task line re-opens insert mode. The panel advertises nothing else —
`?` (the shortcut overlay) documents the full flow.

## Worktree

| Key | Action                                 |
| --- | -------------------------------------- |
| `n` | Spawn an agent (folder or worktree)    |
| `x` | Remove the selected spawn-created pane |

## Spawn modal

Opened with `n` on a repo.

| Key                                | Action                                                                                           |
| ---------------------------------- | ------------------------------------------------------------------------------------------------ |
| Text keys                          | Type the name (worktree mode only: used as the branch slug and tmux window name)                 |
| `↑` / `↓` / `Tab` / `Shift+Tab`    | Move focus between `MODE` / `AGENT` / `NAME` fields                                              |
| `←` / `→`                          | Cycle the value when the mode or agent field has focus                                           |
| `Enter`                            | Create the window and launch the agent                                                           |
| `Esc`                              | Cancel                                                                                           |

`MODE` picks the spawn kind: `default` launches the agent directly in
the repo folder (no git side effects, no `NAME` field); `worktree`
creates a git worktree + branch first.

## Close pane modal

Opened with `x` on a spawn-created pane.

Worktree spawn:

| Key             | Action                                                                                                    |
| --------------- | --------------------------------------------------------------------------------------------------------- |
| `y` / `Enter`   | Close the tmux window, remove the git worktree (`--force`), and delete the branch (`git branch -D`)       |
| `c`             | Close the tmux window only, keep the worktree and branch on disk                                          |
| `n` / `Esc`     | Cancel                                                                                                    |

Folder spawn (no worktree or branch exists):

| Key             | Action                                   |
| --------------- | ---------------------------------------- |
| `y` / `Enter`   | Close the tmux window                    |
| `n` / `Esc`     | Cancel                                   |

## Mouse

The sidebar is mouse-driven throughout:

- Agent rows — click to select and jump to the pane.
- Group headers — click to fold / unfold. Window headers — click to jump to the window.
- `+` beside a repo opens the spawn modal; `×` on a spawn-created pane opens the close-pane modal.
- Header row — `☰` (left edge) opens the overview grid, the chyron banner selects the pane it reports on, and `ⓘ` / `—` (right edge) toggles the repo popup.
- Repo popup — click a repo to filter to it, click a notice line to copy it; clicking outside closes the popup.
- Help, spawn, and close-pane popups — clicking outside closes them.
- TODO panel — the checkbox toggles the item, its text selects it, clicking the selected item opens the editor, and clicking the input line re-opens entry mode.
- Mouse wheel scrolls the list (3 lines per notch, 1 in compact mode).
