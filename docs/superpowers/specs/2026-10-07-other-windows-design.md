# Other windows in the agent list

> **Update (2026-10-08):** the shipped feature deviates from the original
> design in two ways. Grouping is one row **per pane**, not one
> representative per window (`group_other_windows_by_repo` keys by pane
> id), so two programs in two splits each get a row. Window rows also
> participate in the status filters instead of rendering only under `All`,
> and pane runtime state tracks task completion: when a Task-classified
> foreground command drops back to a shell, the row flashes a green
> hollow diamond until the pane gains focus; a non-zero exit code
> (reported by the optional `shell-exit.sh` shell integration into
> `@pane_last_cmd` / `@pane_last_exit`) marks the row with a red diamond
> plus the exit code and surfaces it in the error filter.

## Goal

Let the sidebar surface the non-agent tmux windows that live in the same
repositories as the agents it already lists — the editors, test runs, dev
servers, and shells a user juggles across a repo. Today those windows are
completely invisible: the user has to remember which window is running the
test suite and hunt for it.

A hotkey (`w`) toggles a section that renders one dim row per non-agent
window beneath each repo's agent rows, showing the window, its foreground
command, how long it has been running, and (later) any listening port.
Busy windows get a status glyph and, for task-like commands, a pulse, so a
long test suite is visible at a glance and can be jumped to with Enter.

## Decisions (settled)

- **Placement:** inline per repo, after the agent rows, with a dim `windows`
  sub-header in expanded mode.
- **Status model:** Idle / Busy / pulsing Task. Shell foreground = idle;
  non-shell = busy; task-like commands pulse and show elapsed; editors,
  pagers, ssh, and REPLs stay steady green.
- **Interaction:** window rows participate in `j`/`k` navigation and
  Enter-to-jump, and are clickable.
- **Scope:** only windows whose repo already has an agent, excluding the
  sidebar's own window and any window already represented by an agent row.
  Windows are hidden while a non-`All` status filter is active.

## Out of scope

- Showing windows in repos that have no agent running.
- Reordering tmux windows, creating windows, or any write beyond selecting.
- A full tmux window list / navigator; this is scoped to "the rest of the
  repos already on screen".
- Changing how agent rows render. Window identity is not added to agent
  rows in this change.

## Why windows are invisible today

`parse_pane_fields_with_processes` returns `None` for any pane without a
recognized agent (`src/tmux/query.rs:364`), and `finalize_sessions` drops
any window left with zero agent panes (`src/tmux/query.rs:334`). So the
session hierarchy the sidebar builds contains only agent panes. Everything
needed to identify a window is already in the raw `list-panes -a` output
(`pane_format`, `src/tmux/query.rs:71`): `window_id`, `window_index`,
`window_name`, `window_active`, `pane_id`, `pane_active`, `pane_pid`,
`pane_current_path`, and `pane_current_command`.

## Data model

### New type: `OtherPane`

In `src/tmux/types.rs`, a struct capturing one non-agent, non-sidebar pane:

```rust
pub struct OtherPane {
    pub session_name: String,
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub window_active: bool,
    pub pane_id: String,
    pub pane_active: bool,
    pub path: String,        // pane_current_path
    pub command: String,     // pane_current_command (basename)
    pub pane_pid: Option<u32>,
}
```

### `TmuxSnapshot` gains `other_panes: Vec<OtherPane>`

Collected in `build_session_snapshot` (`src/tmux/query.rs:166`) in the same
pass over the already-fetched output. A pane line that is not the sidebar
and whose `@pane_agent` / `pane_current_command` does not resolve to an
agent becomes an `OtherPane` instead of being discarded. The existing agent
hierarchy parse is untouched.

### `AppState.other_windows: IndexMap<String, Vec<OtherWindow>>`

`OtherWindow` is the per-window, deduped view the UI renders:

```rust
pub struct OtherWindow {
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub session_name: String,
    pub pane_id: String,       // active pane of the window
    pub pane_active: bool,
    pub command: String,       // foreground command of the active pane
    pub git_info: PaneGitInfo,
    pub status: WindowStatus,  // Idle | Busy | Task (see below)
}
```

Windows are stored in a side map keyed by the same repo key
`group_panes_by_repo` uses (`repo_root`, falling back to the raw path),
rather than a new field on `RepoGroup`. Adding a field to `RepoGroup`
would force edits to ~103 struct literals across the repo (tests and
fixtures), so the map keeps the diff focused on the feature. A sibling
`group_other_windows_by_repo` (`src/group.rs`) resolves each pane's `path`
through the same `git_info_cache` and buckets by key. A `repo_group_key`
helper derives the lookup key from an existing `RepoGroup` (first pane's
`repo_root`, else its `grouping_anchor`) so the UI and grouping agree.

Dedup and exclusion rules:

- One `OtherWindow` per `window_id` per repo group. When a window has
  multiple panes in the same repo, the window's active pane wins for
  command/pid; the rest are dropped.
- Exclude the sidebar's own window.
- Exclude any window that already contributed an agent pane to the group
  (it is not "other").
- A window with panes in two different repos may appear under both; this
  is rare and acceptable.

## State

### `GlobalState.show_windows`

New boolean on `GlobalState` (`src/state/global.rs:11`), persisted to
`@sidebar_show_windows` and seeded by `@sidebar_default_show_windows`,
mirroring `compact` exactly:

- `toggle_show_windows()` flips the field and calls `save_show_windows()`
  (`set -g @sidebar_show_windows 1|0`), then broadcasts via
  `broadcast_change()`.
- `apply_all` adopts the live tmux value unconditionally.
- `apply_default_view` seeds it from `@sidebar_default_show_windows` only
  while the live option is unset.
- New constants in `src/tmux/options.rs`: `SIDEBAR_SHOW_WINDOWS`,
  `SIDEBAR_DEFAULT_SHOW_WINDOWS`.

### Busy timing: `PaneRuntimeState.busy_since`

`PaneRuntimeState` (`src/state/pane_runtime.rs:8`) gains
`busy_since: Option<u64>`. Each refresh, for every window's active pane:

- command is a shell (`is_shell_command`, `src/tmux/query.rs:525`) →
  `busy_since = None`.
- command is non-shell and `busy_since` was `None` → set it to `now`.
- already busy → leave it, so elapsed grows.

Elapsed is `now - busy_since`. Caveat: this is observation time, not
process start time; a sidebar launched mid-run undercounts (see Risks).

### Pruning and the git-info cache must include window panes

Two existing prune paths would silently reset window runtime state every
tick if left alone:

- `prune_pane_states_to_current_panes` (`src/state/pane_runtime.rs:130`)
  builds its live set from `group.panes` only. It must also include every
  `other_windows[*][*].pane_id`.
- `git_info_poll_loop` (`src/app/workers.rs:116`) prunes its cache to the
  paths the event loop sends. The event loop must send window pane paths
  alongside agent anchors, or window entries churn and the resolver
  re-runs `git rev-parse` every tick.

`apply_session_snapshot` (`src/state/refresh.rs:54`) threads
`other_panes` through to grouping, and its first-tick priming loop must
prime window paths too.

## Status model

New enum and classifier:

```rust
pub enum WindowStatus { Idle, Busy, Task }

fn classify_window_status(command: &str) -> WindowStatus
```

- `command` is a shell → `Idle`.
- `command` is in the interactive allowlist (below) → `Busy` (steady green).
- otherwise → `Task` (pulsing green, elapsed shown).

Rendering reuses the agent machinery: `running_icon_for`
(`src/ui/panes/row.rs`, from `status`) and `spinner_frame` for the pulse;
`theme.status_color` for idle/busy colors.

Interactive allowlist (a tunable `const`): `vim`, `nvim`, `vi`, `emacs`,
`nano`, `less`, `more`, `man`, `htop`, `top`, `btop`, `ssh`, `mosh`,
`fzf`, `lazygit`, and common REPLs (`python`, `python3`, `node`, `irb`,
`psql`, `sqlite3`). The list exists only to keep the pulse meaningful;
`pane_current_command` is the sole input.

## Layout

Expanded mode, per repo, after the agent rows:

```
myrepo                                   [+]
┃● fix-auth-token                  3m12s
   feat/auth  :5173
  ┄ windows
  ▸ nvim            editor
  ▸ cargo test      tests         2m14s
  ▸ node            web            :3000
```

Compact mode, one line per window:

```
● fix-auth-token                 3m12s
▸ nvim         editor
▸ cargo test   tests   2m14s
```

Row fields, in priority order: status glyph, window name (truncated),
foreground command, elapsed (busy only), port (server only, phase 3). The
`▸` glyph plus `theme.text_muted` distinguishes window rows from agent
rows. The `windows` sub-header renders only in expanded mode and only when
the group has at least one window row.

Rendering touch points:

- `collect` (`src/ui/panes/row_collector.rs:19`): after the per-group
  agent loop, when `show_windows && status_filter == All`, emit the
  sub-header and window rows, extending `lines` and `line_to_row`.
- `render_filter_bar` (`src/ui/panes/filter_bar.rs:12`) and
  `status_counts()` count agents only; window rows never contribute.
- New `window_row` / `window_compact_row` in `src/ui/panes/row.rs`.

## Interaction

### Selection and navigation

`RowTarget` (`src/state/layout.rs:6`) gains a boolean kind flag so
activation can tell the two apart without changing how rows are read:

```rust
pub struct RowTarget {
    pub pane_id: String,
    /// `true` when this row is a non-agent window rather than an agent
    /// pane. `pane_id` is the window's active pane either way.
    pub is_window: bool,
}
```

- `rebuild_row_targets` (`src/state/layout.rs:73`) pushes agent targets
  first, then window targets per group, in render order. `selected_pane_row`
  indexes this unified list, so `move_pane_selection`
  (`src/state/focus.rs:79`) works unchanged.
- `line_to_row` and the click path in `handle_mouse_click`
  (`src/state/layout.rs:219`) already map screen rows through
  `line_to_row`, so clicking a window row selects and activates it.

### Activation / jump

`activate_selected_pane` (`src/state/focus.rs:93`) branches on
`is_window`:

- agent row → current behavior (select pane, summon a sidebar in the
  target window via `toggle --create-only`).
- window row → call `tmux::select_pane(&pane_id)` and return, skipping
  the sidebar summon. `select_pane` (`src/tmux/commands.rs:187`) already
  resolves the pane's session, switches the client, selects the window,
  and selects the pane, so cross-session and cross-window jumps work with
  no new wrappers.

`selected_pane()` (`src/state/focus.rs:56`) returns a `PaneInfo` and has
no window analogue; for a window row it returns `None`, which callers
must tolerate (e.g. the spawn popup simply does not open from a window
row).

## Hotkey and options

- `w` in `handle_key_event` (`src/app/input.rs:124`), global across
  non-modal focus like `c` and `m`. Flips `show_windows` and flashes
  `Windows: on (N)` / `Windows: off`, where `N` is the count of window
  rows now available.
- `@sidebar_show_windows` (live, shared) and
  `@sidebar_default_show_windows` (seed) documented in the website's
  `reference/tmux-options.md`.
- Optional discoverability: a small `▸N` affordance near the repo filter
  in the secondary header when windows are hidden and exist. Deferred
  unless the toggle proves hard to find.

## Phases

1. **Core.** `OtherPane` capture, `RepoGroup.windows` grouping, prune/cache
   fixes, `show_windows` state + `w` toggle, inline render (idle/busy via
   `pane_current_command`), nav + jump, elapsed via `busy_since`.
2. **Status polish.** `classify_window_status` and the interactive
   allowlist so only task-like commands pulse.
3. **Ports.** Feed window panes into the port scan so dev servers show
   `:port`. Requires a `kind` on `PaneScanTarget` so
   `advance_dead_scan_streaks` (`src/state/refresh.rs:454`) only tears
   down agent targets; without it, window panes (never in
   `live_agent_panes`) would be wiped every scan.

## Risks & open questions

- **Elapsed accuracy.** `busy_since` is observation time. A sidebar
  started while a test is already running undercounts. Fix later with
  `ps -o etimes` via the existing process snapshot if it matters.
- **Interactive false positives.** The allowlist is heuristic; `git log`
  (pager), a long `ssh` session, or an interactive REPL may pulse. The
  list is a single `const` and easy to tune. A steady-green-only fallback
  (drop `Task`, keep `Busy`) is a one-line change if the pulse is noisy.
- **Window churn.** `pane_current_command` and window names change as
  programs start/stop, so the section can flicker. The 1s refresh is the
  same cadence agent rows already tolerate; no new debounce planned.
- **Cross-session jumps.** `switch-client` moves the attached client,
  which may surprise users in a multi-client setup. Verify behavior
  during implementation; fall back to `select-window` only if
  `switch-client` proves disruptive.
- **Vertical budget.** On short terminals the windows section competes
  with the agent list. It is opt-in (`w`) and hidden under filters, which
  bounds the cost, but the sub-header may need to be dropped first.
- **`finalize_sessions` interaction.** `other_panes` must be collected
  before/independently of the agent-hierarchy finalize step so adding it
  cannot resurrect windows into the agent list.

## Success criteria

- `cargo test`, `cargo clippy`, `cargo fmt --check` pass.
- With `w` off, rendering is byte-identical to today (no window rows, no
  layout drift).
- With `w` on, a repo with a running agent and a non-agent window in the
  same checkout shows that window beneath the agent rows, in both
  expanded and compact modes, and the status-filter counts are unchanged.
- A busy window shows the pulse and an elapsed counter; an idle shell
  window does not.
- `j`/`k` reach window rows; Enter jumps to the window (same session and
  cross-session); clicking a window row jumps to it. Jumping to a window
  does not spawn a sidebar there.
- A non-`All` status filter hides the windows section.
- New UI tests use `insta::assert_snapshot!` per the repo's UI test rule.