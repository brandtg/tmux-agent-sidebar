# Other Windows Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in (`w`) section that lists the non-agent tmux windows
sharing a repo with the listed agents, with idle/busy/task status, elapsed
time for busy tasks, keyboard navigation, and Enter/click-to-jump.

**Architecture:** Extend the single `list-panes -a` parse to also retain
non-agent, non-sidebar panes (`OtherPane`) instead of discarding them.
Group those panes into `AppState.other_windows`, keyed by the same repo key
`group_panes_by_repo` already uses, and render one dim row per window beneath
each repo's agent rows. A global `show_windows` flag (synced via
`@sidebar_show_windows`) gates the section. Window rows join the existing
selection/target list so `j`/`k` and Enter work, with Enter jumping via the
existing `select_pane`.

**Tech Stack:** Rust (edition 2024), Ratatui + Crossterm, tmux, `insta`
snapshot tests.

**Spec:** [`docs/superpowers/specs/2026-10-07-other-windows-design.md`](../specs/2026-10-07-other-windows-design.md)

---

## File Structure

### Modify

- `src/tmux/types.rs` — add `OtherPane`, `WindowStatus`; re-export nothing new.
- `src/tmux/query.rs` — collect `other_panes`, expose sidebar window id, add
  `classify_window_status` + interactive allowlist, make `is_shell_command`
  `pub(crate)`.
- `src/tmux.rs` — re-export `OtherPane`, `WindowStatus`, `classify_window_status`.
- `src/tmux/options.rs` — add `SIDEBAR_SHOW_WINDOWS`,
  `SIDEBAR_DEFAULT_SHOW_WINDOWS`.
- `src/group.rs` — add `OtherWindow`, `group_other_windows_by_repo`,
  `repo_group_key`.
- `src/state/pane_runtime.rs` — add `busy_since`; include window panes in prune.
- `src/state/refresh.rs` — thread `other_panes`; rebuild `other_windows`; update
  `busy_since`.
- `src/state/global.rs` — `show_windows` field, toggle/save/load/default.
- `src/state.rs` — `AppState.other_windows` field; reset on new.
- `src/state/layout.rs` — `RowTarget.is_window`; window targets in
  `rebuild_row_targets`.
- `src/state/focus.rs` — `activate_selected_pane` skip summon for window rows.
- `src/app/input.rs` — `w` keybinding.
- `src/app.rs` — feed window paths to the git-info worker.
- `src/ui/panes/row_collector.rs` — render window rows + sub-header.
- `src/ui/panes/row.rs` — `window_row` renderer.
- `agent-sidebar.conf` — seed `@sidebar_default_show_windows`.
- `website/src/content/docs/reference/tmux-options.md` — document both options.

### Create

- `tests/other_windows_tests.rs` — snapshot + contract tests for the section.

---

## Chunk 1: Data layer

Land the parse and grouping first so later chunks can be tested against real
`other_windows` data.

### Task 1: Add `OtherPane` and `WindowStatus` types

**Files:**
- Modify: `src/tmux/types.rs`
- Modify: `src/tmux.rs`

- [ ] **Step 1: Add the types to `src/tmux/types.rs`**

Insert after `WindowInfo` (around line 153):

```rust
/// A non-agent, non-sidebar pane. Retained by the tmux parse so the
/// sidebar can show the "other windows" sharing a repo with the agents.
#[derive(Debug, Clone)]
pub struct OtherPane {
    pub session_name: String,
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub window_active: bool,
    pub pane_id: String,
    pub pane_active: bool,
    pub path: String,
    pub command: String,
    pub pane_pid: Option<u32>,
}

/// Coarse status for a non-agent window row. Distinct from
/// [`PaneStatus`] because windows have no hooks and must never be
/// counted by the agent status filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowStatus {
    Idle,
    Busy,
    Task,
}
```

- [ ] **Step 2: Re-export from `src/tmux.rs`**

Extend the `pub use types::{...}` list:

```rust
pub use types::{
    AgentType, CLAUDE_AGENT, CODEX_AGENT, OPENCODE_AGENT, OtherPane, PaneAttention, PaneInfo,
    PaneStatus, PermissionMode, SessionInfo, WindowInfo, WindowStatus, WorktreeMetadata,
};
```

- [ ] **Step 3: Compile**

```bash
cargo build
```
Expected: builds. The new types are unused for now (`dead_code` warnings are
fine until later tasks wire them in; do not add `#[allow]`).

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add src/tmux/types.rs src/tmux.rs
git commit -m "feat(tmux): add OtherPane and WindowStatus types"
```

---

### Task 2: Collect `other_panes` in the tmux query

**Files:**
- Modify: `src/tmux/query.rs`

- [ ] **Step 1: Add the field to `TmuxSnapshot`**

In the `TmuxSnapshot` struct (`src/tmux/query.rs:130`), add:

```rust
    /// Non-agent, non-sidebar panes across every window. Consumed by
    /// `group_other_windows_by_repo` to build the opt-in windows section.
    pub other_panes: Vec<OtherPane>,
    /// `window_id` of the sidebar's own window, so its panes are never
    /// offered as "other windows".
    pub sidebar_window_id: Option<String>,
```

- [ ] **Step 2: Return the sidebar window id from `extract_sidebar_window_info`**

Change the signature and return value (`src/tmux/query.rs:195`):

```rust
fn extract_sidebar_window_info(
    all_panes_output: &str,
    sidebar_pane: &str,
) -> (bool, bool, Vec<(String, bool, String)>, Option<String>) {
```

Return `(sidebar_pane_active, sidebar_window_active, out, sidebar_window_id)`
at the end of the function.

- [ ] **Step 3: Collect other panes in `build_session_snapshot`**

Update the destructuring and add a collection pass (`src/tmux/query.rs:166`):

```rust
    let (sidebar_pane_active, sidebar_window_active, sidebar_window_panes, sidebar_window_id) =
        extract_sidebar_window_info(all_panes_output, sidebar_pane);
    let process_snapshot = process_snapshot_for_panes(all_panes_output);
    let other_panes = collect_other_panes(all_panes_output, sidebar_window_id.as_deref());
    let (mut sessions_map, codex_pids) =
        build_session_hierarchy(all_panes_output, process_snapshot.as_ref());
```

and include `other_panes` + `sidebar_window_id` in the returned `TmuxSnapshot`.

Add the helper:

```rust
/// Extract every pane that is neither the sidebar nor an agent. A pane is
/// an agent when `@pane_agent` resolves, or (fallback) when its foreground
/// command is an agent binary. Sidebar-window panes are skipped.
fn collect_other_panes(all_panes_output: &str, sidebar_window_id: Option<&str>) -> Vec<OtherPane> {
    let mut out = Vec::new();
    for line in all_panes_output.lines() {
        let parts = split_tmux_fields(line, '|');
        if parts.len() < session_line_field::MIN_FIELDS {
            continue;
        }
        let window_id = &parts[session_line_field::WINDOW_ID];
        if Some(window_id.as_str()) == sidebar_window_id {
            continue;
        }
        let pane_fields = &parts[session_line_field::PANE_LINE_OFFSET..];
        if pane_fields[pane_line_field::PANE_ROLE] == "sidebar" {
            continue;
        }
        let agent_field = &pane_fields[pane_line_field::AGENT];
        let command = &pane_fields[pane_line_field::PANE_CURRENT_COMMAND];
        if AgentType::from_label(agent_field).is_some()
            || AgentType::from_label(command).is_some()
        {
            continue;
        }
        out.push(OtherPane {
            session_name: parts[session_line_field::SESSION_NAME].clone(),
            window_id: window_id.clone(),
            window_index: parts[session_line_field::WINDOW_INDEX].parse().unwrap_or(0),
            window_name: parts[session_line_field::WINDOW_NAME].clone(),
            window_active: parts[session_line_field::WINDOW_ACTIVE] == "1",
            pane_id: pane_fields[pane_line_field::PANE_ID].clone(),
            pane_active: pane_fields[pane_line_field::PANE_ACTIVE] == "1",
            path: pane_fields[pane_line_field::PANE_CURRENT_PATH].clone(),
            command: command.clone(),
            pane_pid: pane_fields[pane_line_field::PANE_PID].parse().ok(),
        });
    }
    out
}
```

Add a `WINDOW_INDEX` constant to `session_line_field` (`src/tmux/query.rs:23`):

```rust
    pub const WINDOW_INDEX: usize = 2;
```

- [ ] **Step 4: Unit tests**

Add to `mod tests` in `src/tmux/query.rs`:

```rust
#[test]
fn collect_other_panes_keeps_shell_pane_and_skips_agent() {
    let sidebar = "s|@0|0|win|1|0|1|running||claude|n|/p|fish||%1|...";
    // Build real 30-field lines with the helper `full_fields()` + window
    // prefix; see `build_session_snapshot` for the field layout.
    // Assert: shell pane -> Some(OtherPane); claude pane -> absent.
}

#[test]
fn collect_other_panes_skips_sidebar_window() {
    // A shell pane in the sidebar's own window must be excluded.
}
```

(Use the existing `full_fields()` fixture helpers; prefix each with six
window-level fields `session|window_id|window_index|window_name|window_active|automatic-rename`.)

- [ ] **Step 5: Verify**

```bash
cargo test --lib tmux::query
cargo build
```
Expected: new tests pass, build clean.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/tmux/query.rs
git commit -m "feat(tmux): collect non-agent panes in the session snapshot"
```

---

### Task 3: Group other windows by repo

**Files:**
- Modify: `src/group.rs`
- Modify: `src/state.rs`
- Modify: `src/state/refresh.rs`

- [ ] **Step 1: Add `OtherWindow`, `group_other_windows_by_repo`, and `repo_group_key`**

In `src/group.rs`, add:

```rust
use std::collections::HashSet;

use crate::tmux::{OtherPane, SessionInfo, WindowStatus};

/// A non-agent window attached to a repo group.
#[derive(Debug, Clone)]
pub struct OtherWindow {
    pub window_id: String,
    pub window_index: i64,
    pub window_name: String,
    pub session_name: String,
    pub pane_id: String,
    pub pane_active: bool,
    pub command: String,
    /// Live cwd of the active pane. The git-info cache is keyed by this
    /// exact path, so it must ride along for the worker feed in `app.rs`.
    pub path: String,
    pub git_info: PaneGitInfo,
    pub status: WindowStatus,
}

/// Repo key for a group, mirroring the key `group_panes_by_repo` assigned:
/// the first pane's resolved repo root, else its grouping anchor.
pub fn repo_group_key(group: &RepoGroup) -> String {
    group
        .panes
        .iter()
        .find_map(|(_, git)| git.repo_root.clone())
        .or_else(|| group.panes.first().map(|(p, _)| p.grouping_anchor().to_string()))
        .unwrap_or_default()
}

/// Bucket non-agent panes by repo key, one `OtherWindow` per window.
/// Excludes windows that already have an agent pane and the sidebar's own
/// window. Windows with no agent repo are still returned; the caller only
/// renders keys that match an existing group.
pub fn group_other_windows_by_repo(
    other_panes: &[OtherPane],
    sessions: &[SessionInfo],
    sidebar_window_id: Option<&str>,
    git_info_cache: &std::collections::HashMap<String, PaneGitInfo>,
) -> IndexMap<String, Vec<OtherWindow>> {
    let agent_windows: HashSet<&str> = sessions
        .iter()
        .flat_map(|s| s.windows.iter())
        .map(|w| w.window_id.as_str())
        .collect();

    let mut by_window: IndexMap<(String, String), OtherWindow> = IndexMap::new();
    for pane in other_panes {
        if agent_windows.contains(pane.window_id.as_str())
            || sidebar_window_id == Some(pane.window_id.as_str())
        {
            continue;
        }
        let git_info = git_info_cache
            .get(&pane.path)
            .cloned()
            .unwrap_or_default();
        let key = git_info
            .repo_root
            .clone()
            .unwrap_or_else(|| pane.path.clone());
        let entry_key = (key.clone(), pane.window_id.clone());
        let candidate = OtherWindow {
            window_id: pane.window_id.clone(),
            window_index: pane.window_index,
            window_name: pane.window_name.clone(),
            session_name: pane.session_name.clone(),
            pane_id: pane.pane_id.clone(),
            pane_active: pane.pane_active,
            command: pane.command.clone(),
            path: pane.path.clone(),
            git_info,
            status: crate::tmux::classify_window_status(&pane.command),
        };
        match by_window.get(&entry_key) {
            // Prefer the active pane's command for the window.
            Some(existing) if existing.pane_active && !candidate.pane_active => {}
            _ => {
                by_window.insert(entry_key, candidate);
            }
        }
    }

    let mut out: IndexMap<String, Vec<OtherWindow>> = IndexMap::new();
    for ((key, _), window) in by_window {
        out.entry(key).or_default().push(window);
    }
    for windows in out.values_mut() {
        windows.sort_by_key(|w| (status_rank(w.status), w.window_index));
    }
    out
}

fn status_rank(status: WindowStatus) -> u8 {
    match status {
        WindowStatus::Task => 0,
        WindowStatus::Busy => 1,
        WindowStatus::Idle => 2,
    }
}
```

`classify_window_status` is fully implemented in Task 9; to keep this chunk
independently testable, add a stub in `src/tmux/query.rs` now and re-export
it from `src/tmux.rs`:

```rust
pub fn classify_window_status(_command: &str) -> WindowStatus {
    WindowStatus::Idle
}
```

- [ ] **Step 2: Add the field to `AppState`**

In `src/state.rs`, near `pub repo_groups` (line 46):

```rust
    /// Non-agent windows sharing a repo with the listed agents, keyed by
    /// the same repo key as `repo_groups`. Rebuilt every refresh.
    pub other_windows: indexmap::IndexMap<String, Vec<crate::group::OtherWindow>>,
```

Initialize it in `AppState::new` with `indexmap::IndexMap::new()`.

- [ ] **Step 3: Rebuild `other_windows` in `apply_session_snapshot`**

Change the signature (`src/state/refresh.rs:54`):

```rust
    pub(crate) fn apply_session_snapshot(
        &mut self,
        sessions: Vec<SessionInfo>,
        other_panes: Vec<crate::tmux::OtherPane>,
        sidebar_window_id: Option<String>,
        sidebar_window_panes: Vec<(String, bool, String)>,
    ) {
```

Prime window paths in the first-tick loop (alongside agent anchors), then
after `self.repo_groups = ...` add:

```rust
        self.other_windows = crate::group::group_other_windows_by_repo(
            &other_panes,
            &sessions,
            sidebar_window_id.as_deref(),
            &self.git_info_cache,
        );
```

- [ ] **Step 4: Update the `refresh` call site**

In `refresh` (`src/state/refresh.rs:188`), extend the `TmuxSnapshot`
destructure and pass the new fields:

```rust
        let TmuxSnapshot {
            mut sessions,
            other_panes,
            sidebar_window_id,
            mut process_snapshot,
            sidebar_window_panes,
            ..
        } = snapshot;
        // ...
        self.apply_session_snapshot(sessions, other_panes, sidebar_window_id, sidebar_window_panes);
```

- [ ] **Step 5: Include window panes in pruning**

In `prune_pane_states_to_current_panes` (`src/state/pane_runtime.rs:130`):

```rust
        for windows in self.other_windows.values() {
            for window in windows {
                active_ids.insert(window.pane_id.clone());
            }
        }
```

- [ ] **Step 6: Feed window paths to the git-info worker**

In `src/app.rs` (around line 135), add window paths to the `paths` set:

```rust
            let mut paths: Vec<String> = state
                .repo_groups
                .iter()
                .flat_map(|group| group.panes.iter())
                .map(|(pane, _)| pane.grouping_anchor().to_string())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            paths.extend(
                state
                    .other_windows
                    .values()
                    .flatten()
                    .map(|w| w.path.clone())
                    .filter(|p| !p.is_empty()),
            );
```

Note: windows key off their live `path`, which is stored on `OtherWindow`
(`path` field) and pushed verbatim — the git-info cache is keyed by the
exact path passed to `resolve_pane_git_info`.

- [ ] **Step 7: Fix every `apply_session_snapshot` caller in tests**

```bash
cargo test --no-run
```

Update test call sites (e.g. `src/state/refresh.rs` tests) to pass
`Vec::new(), None, Vec::new()` for the new arguments.

- [ ] **Step 8: Verify + commit**

```bash
cargo test --lib
cargo fmt
git add src/group.rs src/state.rs src/state/refresh.rs src/state/pane_runtime.rs src/app.rs
git commit -m "feat(state): group non-agent windows by repo"
```

---

### Task 4: Track busy timing per window pane

**Files:**
- Modify: `src/state/pane_runtime.rs`
- Modify: `src/state/refresh.rs`

- [ ] **Step 1: Add `busy_since` to `PaneRuntimeState`**

```rust
    /// Epoch seconds when this pane's foreground command first became a
    /// non-shell program. `None` while idle. Drives the window row's
    /// elapsed counter; observation time, not process start time.
    pub busy_since: Option<u64>,
```

- [ ] **Step 2: Update it each refresh**

Add a method to `AppState` (in `src/state/refresh.rs`, called from
`refresh` after `apply_session_snapshot`):

```rust
    fn refresh_window_busy(&mut self) {
        let now = self.now;
        let mut updates: Vec<(String, Option<u64>)> = Vec::new();
        for windows in self.other_windows.values() {
            for window in windows {
                // `status` already encodes shell-vs-program, so no second
                // command parse is needed here.
                let busy = !matches!(window.status, crate::tmux::WindowStatus::Idle);
                let prior = self
                    .pane_state(&window.pane_id)
                    .and_then(|s| s.busy_since);
                let next = if busy { Some(prior.unwrap_or(now)) } else { None };
                updates.push((window.pane_id.clone(), next));
            }
        }
        for (pane_id, since) in updates {
            self.pane_state_mut(&pane_id).busy_since = since;
        }
    }
```

Call `self.refresh_window_busy();` in `refresh` right after
`self.apply_session_snapshot(...)`.

- [ ] **Step 3: Verify + commit**

```bash
cargo test --lib
cargo fmt
git add src/state/pane_runtime.rs src/state/refresh.rs
git commit -m "feat(state): track busy-since timing for non-agent panes"
```

---

## Chunk 2: Toggle and rendering

### Task 5: `show_windows` global state

**Files:**
- Modify: `src/tmux/options.rs`
- Modify: `src/state/global.rs`
- Modify: `agent-sidebar.conf`

- [ ] **Step 1: Add option constants** (`src/tmux/options.rs`, near `SIDEBAR_COMPACT`)

```rust
/// Live "show other windows" flag (`1`/`0`). Written whenever a sidebar
/// toggles with `w` so every open sidebar renders the section alike.
pub const SIDEBAR_SHOW_WINDOWS: &str = "@sidebar_show_windows";
/// Landing default for a newly opened sidebar (`on`/`off`, default `off`).
pub const SIDEBAR_DEFAULT_SHOW_WINDOWS: &str = "@sidebar_default_show_windows";
```

Re-export both from `src/tmux.rs` in the `options::{...}` list.

- [ ] **Step 2: Add the field and sync methods to `GlobalState`**

Mirror `compact` exactly (`src/state/global.rs`):

- field `pub show_windows: bool` (default `false`),
- `toggle_show_windows()` flips and calls `save_show_windows()` then
  `broadcast_change()`,
- `save_show_windows()` writes `@sidebar_show_windows` as `1`/`0`,
- `apply_all` adopts the live value unconditionally,
- `apply_default_view` seeds from `@sidebar_default_show_windows` only when
  the live option is absent,
- `default_show_windows_from_options(opts)` parsing `on|true|1`.

- [ ] **Step 3: Seed the default in `agent-sidebar.conf`**

Next to the `@sidebar_default_compact_view` seed (line 60):

```tmux
if -F '#{==:#{@sidebar_default_show_windows},}' 'set -g @sidebar_default_show_windows off'
```

- [ ] **Step 4: Tests**

Add a `GlobalState` test mirroring `toggle_compact` coverage: default off,
toggle flips + writes, `apply_all` adopts `1`.

- [ ] **Step 5: Verify + commit**

```bash
cargo test --lib state::global
cargo fmt
git add src/tmux/options.rs src/tmux.rs src/state/global.rs agent-sidebar.conf
git commit -m "feat(state): add show_windows global toggle"
```

---

### Task 6: `w` keybinding

**Files:**
- Modify: `src/app/input.rs`

- [ ] **Step 1: Add the arm**

In `handle_key_event` (near the `c` arm, `src/app/input.rs:166`):

```rust
        KeyCode::Char('w') => {
            state.global.toggle_show_windows();
            state.rebuild_row_targets();
            let count: usize = state.other_windows.values().map(Vec::len).sum();
            if state.global.show_windows {
                state.set_flash(format!("Windows: on ({count})"));
            } else {
                state.set_flash("Windows: off");
            }
        }
```

`set_flash` takes `impl Into<String>`, so the `format!` and the `&str`
literal both work.

- [ ] **Step 2: Test**

Add a test to `src/app/input.rs` tests: `w` flips `state.global.show_windows`
and sets a flash; `w` inside a popup is swallowed (covered by the existing
popup arms running first — assert with an open `SpawnInput` popup).

- [ ] **Step 3: Verify + commit**

```bash
cargo test --lib app::input
cargo fmt
git add src/app/input.rs
git commit -m "feat(input): bind w to toggle the windows section"
```

---

### Task 7: Render window rows

**Files:**
- Modify: `src/ui/panes/row.rs`
- Modify: `src/ui/panes/row_collector.rs`

- [ ] **Step 1: Add `window_row` to `src/ui/panes/row.rs`**

```rust
/// One line for a non-agent window row. `elapsed` is `Some` only for
/// busy/task windows. Window rows are always a single line in both
/// densities; only the surrounding sub-header differs.
pub(super) fn window_row(
    window: &crate::group::OtherWindow,
    elapsed: Option<String>,
    selected: bool,
    width: usize,
    icons: &StatusIcons,
    theme: &ColorTheme,
    spinner_frame: usize,
) -> Line<'static> {
    use crate::tmux::WindowStatus;
    let bg = selected.then_some(theme.selection_bg);
    let apply_bg = |style: Style| match bg {
        Some(c) => style.bg(c),
        None => style,
    };
    let (icon, color) = match window.status {
        WindowStatus::Idle => (icons.status_icon(&PaneStatus::Idle), theme.status_idle),
        WindowStatus::Busy => (icons.status_icon(&PaneStatus::Running), theme.status_running),
        WindowStatus::Task => {
            let (icon, pulse) = running_icon_for(&PaneStatus::Running, spinner_frame, icons);
            (icon, pulse.unwrap_or(theme.status_running))
        }
    };
    // Layout: "▸ <window-name>  <command>  <elapsed-right>"
    // Truncate window name first, command second; elapsed is right-pinned.
    // ...build spans...
}
```

Use `truncate_to_width` and `display_width` from `crate::ui::text`, and
`theme.text_muted` for the command text so window rows read dimmer than
agent rows. Return a single `Line`.

- [ ] **Step 2: Emit rows in `collect`** (`src/ui/panes/row_collector.rs:19`)

After the `for (pane, git_info) in filtered_panes.iter()` loop, still inside
the group loop:

```rust
        let show_windows = state.global.show_windows
            && matches!(state.global.status_filter, crate::state::StatusFilter::All);
        let key = crate::group::repo_group_key(group);
        if show_windows
            && !key.is_empty()
            && let Some(windows) = state.other_windows.get(&key)
        {
            collected.lines.push(Line::from(Span::styled(
                "  ┄ windows".to_string(),
                Style::default().fg(theme.text_muted),
            )));
            collected.line_to_row.push(None);
            for window in windows {
                let is_selected = state.focus_state.sidebar_focused
                    && state.focus_state.focus == Focus::Panes
                    && row_index == state.global.selected_pane_row;
                let elapsed = state
                    .pane_state(&window.pane_id)
                    .and_then(|s| s.busy_since)
                    .filter(|_| matches!(window.status, crate::tmux::WindowStatus::Task | crate::tmux::WindowStatus::Busy))
                    .map(|since| crate::ui::text::elapsed_label(Some(since), state.now));
                let line = row::window_row(
                    window,
                    elapsed,
                    is_selected,
                    width,
                    &state.icons,
                    theme,
                    state.spinner_frame,
                );
                collected.lines.push(line);
                collected.line_to_row.push(Some(row_index));
                row_index += 1;
            }
        }
```

`repo_group_key` returns a `String` (empty for a group with no panes, which
cannot occur here since groups are built from panes).

- [ ] **Step 3: Guard the empty-group skip**

`collect` currently `continue`s when `filtered_panes.is_empty()`. Since
windows only render under `All` (where every group has ≥1 pane), no change
is needed; add a comment noting the invariant.

- [ ] **Step 4: Snapshot test**

Add to `tests/other_windows_tests.rs`:

```rust
// Build an AppState with one repo group (1 agent) and one OtherWindow,
// set show_windows = true, render via draw_agents into TestBackend, and
// insta::assert_snapshot! the buffer. Add a second snapshot with
// show_windows = false to prove the frame is unchanged when off.
```

Per the repo UI test rule, use `insta::assert_snapshot!` only — no
`contains` checks on the rendered frame.

- [ ] **Step 5: Verify + commit**

```bash
cargo test
cargo fmt
git add src/ui/panes/row.rs src/ui/panes/row_collector.rs tests/other_windows_tests.rs
git commit -m "feat(ui): render other windows under each repo group"
```

---

## Chunk 3: Navigation and jump

### Task 8: Make window rows selectable and jumpable

**Files:**
- Modify: `src/state/layout.rs`
- Modify: `src/state/focus.rs`
- Modify: existing `RowTarget` literals in tests and `src/state.rs`

- [ ] **Step 1: Add `is_window` to `RowTarget`**

```rust
#[derive(Debug, Clone)]
pub struct RowTarget {
    pub pane_id: String,
    /// `true` when this row is a non-agent window rather than an agent pane.
    pub is_window: bool,
}
```

- [ ] **Step 2: Add window targets in `rebuild_row_targets`**

After the per-group agent loop (`src/state/layout.rs:85`), mirror the render
order exactly:

```rust
        let show_windows = self.global.show_windows
            && matches!(self.global.status_filter, StatusFilter::All);
        let key = crate::group::repo_group_key(group);
        if show_windows
            && !key.is_empty()
            && let Some(windows) = self.other_windows.get(&key)
        {
            for window in windows {
                self.layout.pane_row_targets.push(RowTarget {
                    pane_id: window.pane_id.clone(),
                    is_window: true,
                });
            }
        }
```

Update the existing agent push to `is_window: false`.

- [ ] **Step 3: Skip the sidebar summon on window jumps**

In `activate_selected_pane` (`src/state/focus.rs:93`), read the target's
`is_window`; when true, `tmux::select_pane(&pane_id)` and return before the
`toggle --create-only` block. Do not change `focused_pane_id` handling for
window rows beyond what `select_pane` already implies.

- [ ] **Step 4: Update all `RowTarget {` literals**

```bash
cargo test --no-run
```

Add `is_window: false` to each construction site (about 20, across
`src/state.rs`, `src/state/focus.rs`, `src/state/layout.rs`,
`src/app/input.rs`, `tests/state_tests.rs`).

- [ ] **Step 5: Contract test (render order == target order)**

In `tests/other_windows_tests.rs`, build a state with agents + windows,
`rebuild_row_targets()`, render a frame, and assert that
`line_to_row` row indices and `pane_row_targets` line up: every
`Some(row)` maps to the expected `pane_id` in order. This is a state-field
assertion (not a visual substring), so it may use equality rather than a
snapshot.

- [ ] **Step 6: Verify + commit**

```bash
cargo test
cargo fmt
git add src/state/layout.rs src/state/focus.rs src/state.rs src/app/input.rs tests/state_tests.rs tests/other_windows_tests.rs
git commit -m "feat(state): make other windows navigable and jumpable"
```

---

## Chunk 4: Status classification

### Task 9: Classify window status and pulse tasks

**Files:**
- Modify: `src/tmux/query.rs`
- Modify: `src/tmux.rs`

- [ ] **Step 1: Re-export the classifier**

`classify_window_status` lives in `query.rs` next to `is_shell_command`, so
it calls the (private) shell check directly. Only the classifier needs
exporting. In `src/tmux.rs`:

```rust
pub(crate) use query::{TmuxSnapshot, classify_window_status, query_session_snapshot};
```

- [ ] **Step 2: Replace the Task 3 stub**

```rust
/// Classify a non-agent pane's foreground command. Shells are idle;
/// known interactive programs are busy-but-steady; everything else is a
/// "task" and pulses. The allowlist is a tuning knob — keep it small.
pub fn classify_window_status(command: &str) -> WindowStatus {
    if is_shell_command(command) {
        return WindowStatus::Idle;
    }
    let base = crate::process::command_basename(command);
    if INTERACTIVE_COMMANDS.contains(&base.as_str()) {
        return WindowStatus::Busy;
    }
    WindowStatus::Task
}

const INTERACTIVE_COMMANDS: &[&str] = &[
    "vim", "nvim", "vi", "emacs", "nano", "less", "more", "man", "htop", "top", "btop",
    "ssh", "mosh", "fzf", "lazygit", "python", "python3", "node", "irb", "psql", "sqlite3",
];
```

`WindowStatus` must be imported in `query.rs` (`use super::types::{..., WindowStatus}`).

- [ ] **Step 3: Tests**

Unit-test `classify_window_status` for: `zsh` → Idle, `vim` → Busy,
`cargo` → Task, `/usr/local/bin/npm` → Task.

- [ ] **Step 4: Verify + commit**

```bash
cargo test --lib tmux::query
cargo fmt
git add src/tmux/query.rs src/tmux.rs
git commit -m "feat(tmux): classify non-agent window status"
```

---

## Chunk 5: Ports (optional, can ship after 1-4)

### Task 10: Show dev-server ports on window rows

**Files:**
- Modify: `src/port.rs`
- Modify: `src/state/refresh.rs`
- Modify: `src/ui/panes/row.rs`

- [ ] **Step 1: Tag scan targets with a kind**

Add to `src/port.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKind {
    Agent,
    Window,
}

pub struct PaneScanTarget {
    pub pane_id: String,
    pub pane_pid: Option<u32>,
    pub agent: crate::tmux::AgentType,
    pub kind: PaneKind,
}
```

Add `pub scanned_agent_panes: HashSet<String>` to `PaneProcessSnapshot`,
populated in `scan_pane_processes` only for `PaneKind::Agent` targets.
`apply_process_snapshot` keeps refreshing ports/commands from
`scanned_panes` but advances dead-scan streaks only over
`scanned_agent_panes`, so window panes are never torn down.

- [ ] **Step 2: Queue window panes for scanning**

Extend `queue_port_scan_if_due` to append one target per
`other_windows[*][*]` with `kind: PaneKind::Window` and `agent` set to a
placeholder (it is only consulted for `tree_has_agent` on agent targets).
Pass `other_panes`/`other_windows` into the function from `refresh`.

- [ ] **Step 3: Render the port**

In `window_row`, accept `ports: Option<&[u16]>` and render `:3000` in
`theme.port` before the elapsed counter. Source it in `collect` from
`state.pane_state(&window.pane_id).map(|s| s.ports.as_slice())`.

- [ ] **Step 4: Verify + commit**

```bash
cargo test
cargo fmt
git add src/port.rs src/state/refresh.rs src/ui/panes/row.rs
git commit -m "feat(ui): show listening ports on other-window rows"
```

---

## Final verification

- [ ] **Full gates**

```bash
cargo test
cargo clippy
cargo fmt --check
```
Expected: all pass.

- [ ] **Live check** (per AGENTS.md debugging notes)

```bash
cargo build --release
cp target/release/tmux-agent-sidebar ~/.tmux/plugins/tmux-agent-sidebar/bin/tmux-agent-sidebar
```

Toggle the sidebar off → on, press `w`, and confirm: a repo with an agent and
a sibling test/editor window shows the window rows; a running `cargo test`
pulses with an elapsed counter; `j`/`k` reach window rows; Enter jumps to the
window without spawning a sidebar there; a non-`All` filter hides the section.

- [ ] **Commit remaining docs**

```bash
git add docs/superpowers/specs/2026-10-07-other-windows-design.md docs/superpowers/plans/2026-10-07-other-windows.md
git commit -m "docs: add other-windows design and implementation plan"
```

---

## Out of scope reminders

- No window rows for repos with no agent (scope is "same repos").
- No new tmux wrappers — `select_pane` already handles cross-session jumps.
- No changes to agent row rendering or the status-filter tallies.
- Ports (Chunk 5) are optional and can land as a follow-up.