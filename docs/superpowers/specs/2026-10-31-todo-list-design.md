# Manual TODO list panel

> **Update (2026-10-31):** implemented as designed, with `t` acting as a
> three-state toggle (open + focus when hidden, focus when open but
> unfocused, close when focused) so an open panel can be returned to
> without closing it. Enter in the add line commits the item and keeps
> the input open for rapid entry (Esc ends it), matching a checklist
> app. `x` and `d` delete the selected item (`x` matches the sidebar's
> destructive-`x` convention), and `D` clears completed items. `e`
> edits the selected item's text inline (Enter saves, Esc cancels). One
> small addition: when hide-completed leaves the visible list empty but
> items still exist, the panel says "no open todos — H shows completed"
> instead of the generic add hint, so the user knows completed items are
> still there.
>
> **Update (follow-up):** the panel's open/closed state is now shared
> across every sidebar by default, not per instance. It rides the
> `@sidebar_todo_visible` tmux global (seeded by
> `@sidebar_default_todo_visible`, default `off`), so opening the panel
> in one tmux session shows it in every other one and a sidebar opened
> later lands with it already visible. `@sidebar_todo_scope local` keeps
> the old per-instance behavior; the item list stays shared through the
> file in both modes. Closing the panel from another sidebar also
> releases a local focus/edit that pointed into it.

## Goal

Give the sidebar a place to keep the user's own short list of things to
do across sessions — a manual checklist that has nothing to do with the
agents it monitors. The list lives at the bottom of the sidebar, is
hidden by default, and is opened with `t`. Items are plain text, wrap to
the sidebar width, and carry a checkbox the sidebar owns: Enter toggles
the selected item, `a` adds one, `d` deletes one. Completed items are
hidden by default so the remaining open work stays readable; `H` flips
that.

The list is global, not per window or per session: every sidebar
instance reads and writes the same file, so a note jotted in one session
is visible from every other one.

## Decisions (settled)

- **Placement:** a panel pinned to the bottom of the sidebar, below the
  agent list. Hidden by default; `t` toggles it.
- **Scope:** manual items only. No agent events, no auto-derived tasks,
  no per-pane association.
- **Input:** single-line plain text per item, word-wrapped for display.
  No markdown, no priorities, no due dates.
- **Checkbox:** managed by the sidebar (`[ ]` / `[✓]`), toggled with
  Enter on the selected item.
- **Hide completed:** on by default; a shared, persisted toggle (`H`).
- **Storage:** one plain-text file shared by every sidebar instance,
  under `~/.local/state/tmux-agent-sidebar/` (honoring
  `$XDG_STATE_HOME`). Same path on Linux and macOS.
- **Visibility:** shared across sidebars by default via
  `@sidebar_todo_visible`, so opening the panel in one session shows it
  in the others. `@sidebar_todo_scope local` opts back into per-instance
  visibility. A newly opened sidebar starts hidden unless the shared
  value is on (or `@sidebar_default_todo_visible` is `on`).

## Out of scope

- Agent-driven TODO items, linking a todo to a pane, or notifications.
- Editing an item's text is supported with `e` (Enter saves, Esc
  cancels), but reordering, priorities, due dates, tags, and nesting
  are still out of scope.
- Undo, trash, or confirmation dialogs for deletion.
- Sync across machines; the file is local to the user's account.
- Per-session or per-repo lists.
- Persisting the panel's open/closed state across sidebar restarts.

## Why a file and not tmux options

Everything the sidebar shares across instances today rides on tmux
global options (`@sidebar_compact`, `@sidebar_show_windows`, cursor,
filters). Those are a good fit for a handful of scalars but a poor fit
for an ordered, free-text list: the value would need escaping, would be
visible in `tmux show-options`, and would be capped by tmux's option
handling. A plain file is easier to inspect and edit by hand, survives a
tmux server restart without a config entry, and keeps the list out of
the sidebar's IPC path. The hide-completed flag stays a tmux option
because it is exactly the kind of shared scalar the existing options
already model.

## Storage

### Location

New helper in `src/paths.rs`:

```rust
/// Directory for durable user state (not runtime scratch files).
pub(crate) fn state_dir() -> Option<PathBuf>
```

Resolution:

1. `$XDG_STATE_HOME/tmux-agent-sidebar/` when the variable is set and
   non-empty.
2. Otherwise `$HOME/.local/state/tmux-agent-sidebar/`.
3. If neither is available, persistence is disabled and the list lives
   in memory for the life of the process.

The directory is created on demand with mode `0700`; the file itself is
created `0600`. `paths.rs` already exposes hardened opens
(`open_read`, `open_read_write_private`, `open_append_private`) with
`O_NOFOLLOW`; the TODO store reuses those where they fit and adds a lock
open if needed.

**macOS:** `$XDG_STATE_HOME` is not a native macOS convention, but
`~/.local/state` is the de facto location for tmux/CLI tooling and is
what the rest of this project already assumes. Using the same path on
both platforms keeps one file, one backup target, and one documented
location. The `~/Library/Application Support/...` alternative was
rejected because it splits the list from the project's existing
XDG-oriented runtime paths for no practical gain on a terminal tool.

### File format

`~/.local/state/tmux-agent-sidebar/todos.txt`, one item per line, in
display order:

```
[ ] Buy milk
[✓] Send the invoice
[ ] Read the tmux changelog
```

- `[ ]` = open, `[✓]` = done. Parsing also accepts `[x]` and `[X]` for
  done and tolerates extra spaces, so a hand-edited file keeps working.
- Everything after the checkbox is the item text, trimmed. Text is
  stored on one line; embedded newlines are not possible through the
  input path.
- Lines that do not start with a checkbox are ignored on load (they do
  not become todos). This keeps a stray comment from silently turning
  into an item.
- Serialization always writes `[ ]` / `[✓]`, so the canonical form is
  stable after the first save.

An empty or missing file means an empty list; a missing file is not an
error.

### Concurrency

Multiple sidebars may be open at once and all share one file. Every
mutation is a read-modify-write under an exclusive `flock` on a sibling
`todos.lock` (the same `libc::flock` pattern as
`cli/hook/activity.rs`):

1. Lock `todos.lock` (`O_NOFOLLOW`, `0600`).
2. Re-read `todos.txt` so the mutation applies to the latest content.
3. Apply the change (append / toggle / delete).
4. Write the whole list to `todos.txt.tmp.<pid>` in the same directory
   (`0600`), then `rename` over `todos.txt` — readers never observe a
   partial write.
5. Unlock.

The TUI reloads on change: each refresh tick compares the file's
`(mtime, len)` stamp against the cached one and re-parses when it
differs. The 1s refresh cadence (`src/app.rs:58`) is more than enough for
a list the user edits by hand. A failed lock open degrades to an
unlocked write rather than blocking the UI, matching the subagent lock
behavior.

Selection is by item index. If another instance inserts or deletes
before the current selection, the index can point at a different item
until the next navigation; this is acceptable for a small manual list
and avoids stable-ID bookkeeping in the file format.

### API

New leaf module `src/todo.rs` (registered in `src/lib.rs`):

```rust
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

pub fn todos_path() -> Option<PathBuf>;
pub fn parse(contents: &str) -> Vec<TodoItem>;
pub fn serialize(items: &[TodoItem]) -> String;
pub fn load() -> io::Result<Vec<TodoItem>>;
pub fn stamp() -> Option<(SystemTime, u64)>;
pub fn update<F>(f: F) -> io::Result<Vec<TodoItem>>
where
    F: FnOnce(&mut Vec<TodoItem>);
```

`update` performs the locked read-modify-write and returns the new list,
so the caller can adopt it without a second read.

## State

New module `src/state/todo.rs`, re-exported from `src/state.rs`:

```rust
pub struct TodoState {
    pub items: Vec<TodoItem>,
    /// Selected item index into `items` (not into the visible subset).
    pub selected: usize,
    /// Panel visibility. Mirrors the shared `@sidebar_todo_visible`
    /// value when the scope is global (the default); per instance with
    /// `@sidebar_todo_scope local`.
    pub visible: bool,
    /// `Some(input)` while the inline "add" line is open.
    pub adding: Option<String>,
    /// Last filesystem stamp, used to detect edits from other instances.
    pub file_stamp: Option<(SystemTime, u64)>,
    /// Non-fatal persistence error surfaced in the panel.
    pub error: Option<String>,
}
```

- `AppState` gains `pub todos: TodoState`.
- `ScrollStates` (`src/state/scroll.rs:17`) gains `pub todos: ScrollState`
  for line-based scrolling inside the panel.
- `Focus` (`src/state/focus.rs:5`) gains a `Todos` variant. Focus stays
  `Todos` while the panel is open and focused; `Esc` returns it to
  `Panes` without hiding the panel.
- `GlobalState` (`src/state/global.rs`) gains
  `pub todo_hide_completed: bool`, default `true`, persisted to
  `@sidebar_todo_hide_completed` and seeded by
  `@sidebar_default_todo_hide_completed` (`on`), mirroring
  `compact` / `show_windows`.

`setup::init_state` loads the file once before the first frame (after
`AppState::new`), and `AppState::refresh` calls a cheap
`refresh_todos_if_changed()` so a list edited by another sidebar
appears within a tick. If the tmux query fails, the early return in
`refresh` skips the reload; the next successful tick catches up.

### Derived views

```rust
impl TodoState {
    /// Indices into `items` that should render, honoring hide-completed.
    pub fn visible_indices(&self, hide_completed: bool) -> Vec<usize>;
    pub fn open_count(&self) -> usize;
    pub fn total_count(&self) -> usize;
}
```

`selected` is always clamped to a visible index whenever the visible set
changes (toggle, delete, hide-completed flip, external reload). If the
selected item is completed while hide-completed is on, the selection
moves to the next visible item at the same position, or the previous
one at the end.

## Layout

`PaneLayout` (`src/ui/panes.rs:52`) currently splits the pane into the
filter bar (1 row), secondary header (1 row), and list (rest). It gains a
bottom `todo_area` and the list shrinks:

```rust
struct PaneLayout {
    filter_area: Rect,
    secondary_area: Rect,
    list_area: Rect,
    todo_area: Rect, // zero height when hidden
}
```

Height rules:

- `MAX_TODO_PANEL_HEIGHT = 10` rows (tunable const), lowered to 6 in
  popup mode on narrow viewports.
- Content height = 1 header row + wrapped item lines + 1 input row while
  adding.
- `todo_area.height = content.min(MAX_TODO_PANEL_HEIGHT)` and never more
  than `area.height - 3` (filter + secondary + at least one list row).
- When the panel is hidden, `todo_area.height = 0` and `list_area` is
  exactly what it is today, so rendering stays byte-identical.

Panel shape (no border; a single muted separator line for the header):

```
─────────────────────────────
▸ [ ] Buy milk
  [✓] Send the invoice
  [ ] Read the tmux changelog
+ type a new todo…
```

- Header: `─ todos · 2/5` (open/total) in `theme.text_muted`, with a
  right-aligned `hide done` hint when the setting is on if width allows.
  Truncate with `truncate_to_width`. While the panel owns the keyboard
  the header switches to `theme.accent` + bold so it is obvious whether
  the agent list or the TODO list is active.
- Each item renders `[ ]` or `[✓]` followed by one space, then the text
  word-wrapped with `wrap_text` (`src/ui/text.rs:105`). Continuation
  lines are indented four columns so they align under the text, not the
  checkbox.
- Done items render in `theme.text_muted` with
  `Modifier::CROSSED_OUT` when visible.
- The selected item, only while `Focus::Todos`, gets
  `theme.selection_bg` across all of its wrapped lines.
- Empty list: a muted `(no todos — press a to add)` row under the
  header.
- While adding, the last row shows `+ ` followed by the input and a
  block cursor, reusing the spawn modal's `tail_fit` approach so the
  cursor stays visible in a narrow sidebar.

Rendering touch points:

- `ui/panes.rs`: compute `PaneLayout` from `state.todos.visible` and the
  content height, render the panel into `todo_area` after the agent
  rows, and register click targets.
- The flash banner currently draws on the last row of `area`
  (`render_flash_banner_into`, `src/ui/panes.rs:513`). When the TODO
  panel is visible it temporarily covers that panel row; the banner
  expires after four seconds, so no layout change is needed.
- Popups already render last over the whole `area`, so they cover the
  panel while open. No popup changes.
- Scrolling: render all visible item lines into a vector while recording
  each item's first/last line, then apply `scrolls.todos` the same way
  `compute_scroll_offset` keeps the selected agent row in view. Auto-
  scroll only while `Focus::Todos`.

## Interaction

### Keys

Global, non-modal:

- `t` — three-state toggle: opens the panel and focuses it when hidden;
  focuses it (without closing) when it is open but the agent list or
  filter bar has focus; closes it and returns focus to the agent list
  when the panel already has focus.

While `Focus::Todos`:

| Key | Action |
| --- | --- |
| `j` / `Down`, `k` / `Up` | Move selection among visible items |
| `Enter` | Toggle done on the selected item (persists immediately) |
| `a` | Open the inline add line at the bottom of the panel |
| `e` | Edit the selected item's text (Enter saves, Esc cancels) |
| `x` / `d` | Delete the selected item (immediate, no confirmation) |
| `D` | Delete every completed item |
| `H` | Toggle "hide completed" (shared, persisted) |
| `t` / `Esc` | `t` opens/focuses/closes the panel (see above); `Esc` returns focus to `Panes` and leaves the panel visible |
| `q` | Still quits the sidebar |
| `c` / `w` / `?` | Still global toggles |

While the add line is open, it owns printable keys (like the `/` search
line): `Enter` commits the trimmed text (empty/whitespace adds nothing)
and immediately starts a fresh empty line so several items can be
entered in a row; `Esc` ends the streak and closes the input;
`Backspace` edits, `Ctrl-u` clears, `Ctrl-w` deletes a word, and
`q`/`t` type into the input instead of acting globally. Each new item
is appended at the end of the list and becomes the selection.

`e` on the selected item opens a second inline buffer (`✎`) prefilled
with its text and the same editing keys. Enter saves the trimmed text
and returns to normal panel keys; Esc cancels; an empty edit is ignored
so the original text is kept.

Implementation note: add an early `Focus::Todos` branch in
`handle_key_event` (`src/app/input.rs:37`) after the search branch, so
the panel owns its keys before the general match; global keys it does
not claim fall through or are handled explicitly.

### Mouse

- `FrameLayout` (`src/state/layout.rs:50`) gains
  `todo_row_targets: Vec<TodoRowTarget { rect, index }>`, one rect per
  rendered item spanning all of its wrapped lines, plus
  `todo_input_rect` for the add line.
- Clicking anywhere inside an item's rect selects it and toggles its
  checkbox (a checklist click). Clicking the input line starts adding.
- `handle_mouse_click` (`src/state/layout.rs:288`) checks the TODO
  targets after the spawn/remove targets and before the `line_to_row`
  fallback, so a click in the panel never activates an agent row.
- Mouse wheel over the panel scrolls `scrolls.todos`; `handle_mouse_scroll`
  branches on whether the pointer row falls inside `todo_area`.

### Persistence triggers

- Toggle, add, and delete each call `todo::update`, then adopt the
  returned list and stamp. On failure, set `TodoState.error` and keep
  the in-memory change so the UI stays responsive; the next successful
  write repairs the file.
- The hide-completed toggle writes `@sidebar_todo_hide_completed` and
  broadcasts via `GlobalState::broadcast_change`, so every open sidebar
  updates at once.

## Options

New constants in `src/tmux/options.rs`:

```rust
/// Live "hide completed todos" flag (1/0), shared across sidebars.
pub const SIDEBAR_TODO_HIDE_COMPLETED: &str = "@sidebar_todo_hide_completed";
/// Landing value for a freshly started tmux server (on/off, default on).
pub const SIDEBAR_DEFAULT_TODO_HIDE_COMPLETED: &str = "@sidebar_default_todo_hide_completed";
```

- `GlobalState::apply_all` adopts the live value when present.
- `apply_default_view` seeds it from the default only while the live
  option is unset, exactly like `compact` and `show_windows`.
- `agent-sidebar.conf` seeds
  `@sidebar_default_todo_hide_completed on` when unset and documents the
  option in its header comment.
- Docs to update: README Configuration table, website
  `reference/keybindings.md` (`t`, `a`, `d`, `H`) and
  `reference/tmux-options.md`, and `HELP_ROWS` in `src/ui/help.rs`
  (one row for `t`; the panel's own keys can be mentioned in the panel
  header or left to the docs to keep the help popup short).

## Phases

1. **Storage.** `src/todo.rs` with parse/serialize/load/stamp/update,
   `paths::state_dir`, atomic write + flock, unit tests for round-trip,
   malformed lines, missing file, symlink refusal, and concurrent
   updates.
2. **State and keys.** `TodoState`, `Focus::Todos`, `ScrollStates.todos`,
   add/toggle/delete/selection-clamping logic, `t`/`a`/`d`/`H`/Enter/Esc
   handling, tick reload, `GlobalState.todo_hide_completed` and its tmux
   option. Unit tests drive the keyboard path without a terminal, as in
   `src/app/input.rs` tests.
3. **Rendering.** `PaneLayout.todo_area`, panel renderer with wrapping,
   checkbox glyphs, selection highlight, scrolling, empty/add states,
   click targets, mouse scroll. Snapshot tests for hidden, empty, mixed
   open/done, hide-completed on/off, narrow width, and adding.
4. **Polish and docs.** Help popup row, README and website reference
   updates, `agent-sidebar.conf` seeding, manual smoke test on Linux and
   macOS (file path, permissions, concurrent instances).

## Risks and open questions

- **Concurrent edits.** flock plus atomic rename makes lost updates
  unlikely, but index-based selection can drift after an external edit.
  Acceptable for a hand-maintained list; stable IDs would be the fix if
  it ever bites.
- **Vertical budget.** On a short sidebar the panel competes with the
  agent list. The 10-row cap and the `area.height - 3` floor bound it,
  and the panel is opt-in. If it still feels cramped, lower the cap or
  collapse long items to one line with an ellipsis.
- **`t` discoverability.** Nothing hints that a TODO panel exists. The
  help popup row is the minimum; a dim `t` hint in the secondary header
  is a possible follow-up if it proves invisible.
- **Flash overlap.** A spawn/remove flash briefly covers the panel's
  last row. Harmless, but worth checking during the smoke test.
- **macOS state path.** `~/.local/state` must be created on first use;
  the directory may not exist on a fresh account. The `0700` create path
  handles that, but the smoke test should start from a clean home
  directory.
- **File edited while adding.** The add line keeps its own buffer; a
  reload from another instance does not disturb it, and the commit
  re-reads under the lock. No special handling planned.
- **Unicode width.** Item text uses the same `display_width` /
  `wrap_text` helpers as the rest of the UI, so CJK and emoji wrap
  correctly; the checkbox column is ASCII width.

## Success criteria

- `cargo test`, `cargo clippy`, and `cargo fmt --check` pass.
- With the panel hidden (the default), every existing UI snapshot is
  byte-identical.
- `t` shows the panel at the bottom; `t` again hides it. `Esc` leaves it
  visible and returns focus to the agent list.
- `a` adds a wrapped plain-text item; `Enter` toggles its checkbox; `d`
  deletes it. Changes are on disk immediately and survive a sidebar
  restart.
- With hide-completed on, toggling an item makes it disappear and the
  remaining open items reflow upward. `H` reveals completed items,
  dimmed and struck through, and the setting is shared across open
  sidebars.
- Two sidebars editing the list concurrently do not lose each other's
  changes.
- The file is `~/.local/state/tmux-agent-sidebar/todos.txt` (or under
  `$XDG_STATE_HOME`), directory `0700`, file `0600`, and is
  hand-editable in the documented format.
- New UI tests use `insta::assert_snapshot!` per the repo's UI test
  rule.
