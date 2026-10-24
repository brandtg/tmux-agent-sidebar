# State Management Architecture

## State Scope & Update Frequency

Every piece of state belongs to one of three scopes: **Global** (shared across all sidebar instances via tmux variables), **Per-pane** (keyed by tmux pane ID), or **Local** (single sidebar process only). The table below shows where each field lives, how often it updates, and what triggers the update.

### Global State (synced via tmux global variables)

Stored in `GlobalState`. Written to tmux on change, with the cursor save
debounced briefly so selection changes do not block redraw/input handling;
reloaded on SIGUSR1. At startup — and only at startup — `status_filter` is
overridden by `@sidebar_default_view` (default `all`) so a newly opened
sidebar lands on a deterministic view instead of the last value some other
window persisted. `compact` follows the live `@sidebar_compact` value so
every sidebar renders the same density; `@sidebar_default_compact_view`
(default `off`) only seeds the mode while no live value exists yet, i.e.
before the first `c` toggle on a fresh tmux server.

A shared-option change propagates to every open sidebar two ways: the
changing sidebar broadcasts SIGUSR1 to all other live sidebar processes
(pid-identity-checked, popups included via their anchor pane), and any
SIGUSR1-driven refresh reloads the global options regardless of whether
the receiving sidebar's window is currently active. The reload cannot be
gated on window activation: `#{window_active}` is a per-session flag, so
a sidebar whose window leads an unfocused session never registers a
hidden→visible transition, and multi-client setups keep several windows
"active" at once. Cursor updates are deliberately not broadcast — they
fire on every navigation keystroke. On the SIGUSR1 focus-change reload the
shared cursor is loaded, and the agent list is then re-anchored at the top
(`reset_pane_scroll`), so switching windows or panes always shows the list
from its first row instead of auto-scrolling down to a cursor left by
another window. The one exception is the sidebar itself gaining focus: the
transition tick instead selects the row of the pane that held focus in
this window just before the jump (`select_focused_context_row`), so the
list opens on the agent matching the current tmux context — with the
auto-scroll bringing that row into view. Popups get the same treatment at
startup, pinned to the pane they were opened from. A focus event is not a
user choice, so the selection is not persisted to `@sidebar_cursor`; when
the context pane is unknown or filtered out of the list, the reset to the
first row applies as usual.

| Field | Tmux Variable | Update Trigger | Description |
|-------|--------------|----------------|-------------|
| `status_filter` | `@sidebar_filter` | User input (left/right key) | Active status filter (All/Running/Background/Waiting/Idle/Error); startup landing view comes from `@sidebar_default_view` |
| `selected_pane_row` | `@sidebar_cursor` | User input (j/k key); tmux write flushed after a short debounce | Cursor position in agent list; re-anchored to the first row on a window/pane (focus-change) reload, or to the row of the pre-focus context pane when the sidebar itself gains focus |
| `repo_filter` | `@sidebar_repo_filter` | User input (repo popup) | Repository filter (All or specific repo) |
| `compact` | `@sidebar_compact` | User input (`c` key) | Compact one-line-per-pane rendering; the live value is shared by all open sidebars, and `@sidebar_default_compact_view` seeds it only before the first toggle |
| sidebar mode | `@sidebar_mode` | `toggle-all` sweep and the `maintain-width` mobile takeover | System-level `open`/`closed` intent; consulted by the new-window and new-session hooks (falling back to `@sidebar_auto_create` while unset) so toggle-all covers windows and sessions created afterwards |

`status_filter`, `selected_pane_row`, and `repo_filter` each have a corresponding `last_saved_*` marker to prevent sync conflicts — tmux only overwrites the local value when it differs from what this instance last wrote. `compact` is exempt: the live tmux value is adopted unconditionally so sidebars can never diverge on density.

### Per-pane State (keyed by pane ID)

Written by `cli/hook.rs` on agent events, read by `query_sessions()` every **1 second**.

Each pane's runtime data is split into two buckets:

| Source | Update Trigger | Description |
|--------|----------------|-------------|
| tmux pane options | Event-driven + cleanup on agent exit | Agent type, status, cwd, permission mode, prompt, subagents, worktree, etc. |
| `PaneRuntimeState` in `AppState` | Refresh cycle + cleanup on agent exit | `ports`, `command`, `task_progress`, `task_dismissed_total`, `inactive_since` |

Pane options written to tmux:

| Tmux Option | Update Trigger | Description |
|-------------|----------------|-------------|
| `@pane_agent` | SessionStart | Agent type ("claude" / "codex" / "opencode") |
| `@pane_status` | Every event | Status ("running" / "background" / "waiting" / "idle" / "error") |
| `@pane_cwd` | SessionStart, UserPromptSubmit, Notification, Stop, StopFailure, PermissionDenied, CwdChanged | Hook-reported working directory (skipped when subagents are active). Preferred over tmux's `pane_current_path` when resolving `PaneInfo.path`. |
| `@pane_launch_cwd` | First hook event reporting a cwd (seed); fresh `startup` SessionStart (overwrite) | Launch-time working directory used as the repo-grouping anchor. Once seeded it survives mid-session `cd`s — an agent that moves into another checkout stays grouped under the repo it was launched in. Worktree payloads resolve to `original_repo_dir` so worktree spawns group under the main repo. Cleared on SessionEnd / teardown; a stale anchor from a hard-killed agent is replaced by the next `startup` SessionStart. |
| `@pane_permission_mode` | SessionStart, hook event | Permission mode |
| `@pane_prompt` | UserPromptSubmit, Stop | Latest prompt or response text |
| `@pane_prompt_source` | UserPromptSubmit, Stop | "user" or "response" |
| `@pane_started_at` | UserPromptSubmit | Unix epoch when agent started |
| `@pane_attention` | SessionStart, StopFailure, UserPromptSubmit, ActivityLog (clear); Notification, PermissionDenied, TeammateIdle (`notification`); Stop (`done`) | Attention flag. `notification` = a hook wants the user's eye (waiting / permission / question / teammate idle); `done` = the turn finished and the output is unseen. The sidebar clears the flag when the user focuses the pane — reading the sidebar list does not count — and a new prompt / activity clears it on the hook side. |
| `@pane_wait_reason` | Notification (set); PermissionDenied, StopFailure, TeammateIdle (set); UserPromptSubmit, ToolStart, ActivityLog (clear) | Reason for waiting/error. Notification writes the agent's reason (`permission` / `permission_prompt` / `question` / ...); PermissionDenied writes `permission_denied`; TeammateIdle writes `teammate_idle:<name>`; StopFailure writes the error text. Cleared by a new prompt, a new tool attempt (Claude `PreToolUse`), or a tool result, except for OpenCode's `permission` / `question`, which stay sticky until the agent's explicit reply event (a synthetic empty `UserPromptSubmit`). |
| `@pane_bg_cmd` | ActivityLog (bg Bash), Refresh sweep (clear), SessionEnd (clear) | Latest sanitized command of a Bash tool started with `run_in_background`. Its presence is the single source of truth for "live bg shell" — Stop routes to `background` while it is set, and the row body renders the command. Persists across UserPromptSubmit so shells spanning turns stay visible; overwritten by the next bg Bash. The refresh loop runs a `ps`-based liveness sweep each tick and clears the marker (plus downgrades `background → idle`) when the tracked process is gone. Only the most recent bg Bash is tracked; older ones are not retained. |
| `@pane_bg_pid` | ActivityLog (bg Bash whose result carries a `pid`), Refresh sweep (clear), SessionEnd (clear) | PID of the process behind `@pane_bg_cmd`. Hermes' `terminal(background=true)` result reports it; Claude's `run_in_background` does not, so those spawns keep the string-match sweep. When present, the sweep's liveness check is a plain presence test for that PID in the full-`ps` snapshot — immune to quote stripping, `$(…)` substitution, arg reordering, and login-shell wrappers, none of which the raw-command-vs-ps-args match can survive. |
| `@pane_subagents` | SubagentStart/Stop | Comma-separated active subagent list |
| `@pane_worktree_name` | SessionStart | Worktree name (if applicable) |
| `@pane_worktree_branch` | SessionStart | Worktree branch (if applicable) |
| `@pane_session_id` | SessionStart, UserPromptSubmit, Notification, Stop, StopFailure, PermissionDenied, CwdChanged | Agent-reported session id (skipped when subagents are active) |
| `@pane_session_title` | opencode `session.created` / `session.updated` (via the plugin shim) | Agent session title. Parsed straight into `PaneInfo.session_name` and rendered as the row title; takes precedence over the polled name map (Claude `/rename` labels plus Hermes session titles). Cleared when the pane switches sessions or its metadata is torn down. |

In-memory per-pane runtime state. Every field lives inside
`PaneRuntimeState` so the whole record is dropped together when its
pane disappears (`prune_pane_states_to_current_panes`).

| Field | Update Frequency | Description |
|-------|-----------------|-------------|
| `pane_states.map[...].ports` | Every 10s (port scan) | Listening localhost ports detected from the pane process tree |
| `pane_states.map[...].command` | Every 10s (port scan) | Best-effort commandline for the pane process tree, with tmux command fallback in the UI |
| `pane_states.map[...].task_progress` | Every 1s (refresh cycle) | Parsed from activity log — task list per pane |
| `pane_states.map[...].task_dismissed_total` | On task completion | Tracks dismissed completed-task counts |
| `pane_states.map[...].inactive_since` | On status change | Debounce timestamp (3s grace before hiding tasks) |
| `pane_states.map[...]` | On every refresh | Per-pane runtime: task progress, ports, dead-scan streak, activity-log mtime |
| `pane_states.map[...].task_progress_log_mtime` | Every 1s (refresh cycle) | mtime of the task-progress log last parsed; skips re-parsing when unchanged |

Per-pane file-based state:

| File | Update Trigger | Read Frequency | Description |
|------|---------------|----------------|-------------|
| `$XDG_RUNTIME_DIR/tmux-agent-activity_{pane_id}.log` (fallback `/tmp`) | Each ActivityLog event | Every 1s | Tool usage log (`epoch-seconds\|tool\|label`; legacy `HH:MM\|tool\|label` lines still parse, with their date treated as unknown and sorted oldest), max 200 lines |

### Local State (single sidebar process only)

| Field | Update Frequency | Description |
|-------|-----------------|-------------|
| `repo_groups` | Every 1s | Panes grouped by git repo root (built directly from `tmux::query_sessions()` output, not stored separately as a session list). The path fed into repo resolution is the launch anchor `@pane_launch_cwd` when set, falling back to the live cwd — so mid-session `cd`s into other checkouts don't regroup the pane |
| `focus_state.focused_pane_id` | Every 1s, plus immediately on user-initiated pane jumps | Currently focused agent pane |
| `focus_state.sidebar_focused` | Every 1s | Whether sidebar pane itself has focus |
| `focus_state.focus` | On user input | UI focus: `Filter` / `Panes`; input also triggers an immediate redraw so focus changes appear without waiting for the next poll tick |
| `focus_state.prev_focused_pane_id` | Every 1s | Previous focused pane ID (for detecting focus changes) |
| `now` | Every 1s | Current Unix epoch |
| `scrolls.panes` | On user input / render | Agent list scroll position |
| `theme` | Once at startup | Color theme from tmux `@sidebar_color_*` variables |
| `popup` | On user input / render | `PopupState` enum: `None` / `Repo { selected, area }` / `Notices { area }`. Enforces "at most one popup open" via the type system |
| `layout` | Every frame (render) | `FrameLayout` sub-struct bundling the ephemeral fields the UI rewrites every frame for click hit-testing: `pane_row_targets`, `line_to_row`, `repo_button_col`, `repo_spawn_targets`, `spawn_remove_targets`, `hyperlink_overlays` |
| `notices` | Once at startup / on copy | `NoticesState` sub-struct: `button_col`, `missing_hook_groups`, `claude_plugin_status`, `claude_settings_has_residual_hooks`, `claude_plugin_notice`, `enabled_agents`, `copy_targets`, `copied_at` |
| `timers` | Refresh cycle / on user input | `RefreshTimers` sub-struct gating periodic work: `last_filter_click` (debounce), `last_port_refresh`, `port_scan_initialized` |
| `spinner_frame` | Every 200ms (animation) | Spinner animation frame counter |
| `icons` | Once at startup | `StatusIcons` theme (overridable via tmux options) |
| `tmux_pane` | Once at startup | This sidebar's own tmux pane ID |
| `version_notice` | Once at startup (bg fetch) | GitHub release update notice, `None` when up-to-date |
| `sessions.names` | Every 10s (background thread) | `session_id → session name` map; scanned by `session_poll_loop` in `app/workers.rs` so the TUI thread never blocks on filesystem/subprocess I/O. Covers Claude `/rename` labels (`~/.claude/sessions/*.json`) and Hermes session titles (`~/.hermes/state.db` via a read-only `sqlite3` probe). Re-applied to every pane on every tick because the snapshot rebuild wipes labels |

---

## Update Cycle Summary

```
┌─────────────────────────────────────────────────────────────┐
│  Every frame (~200ms)                                       │
│  layout.* (rebuilt by ui::draw), spinner animation          │
├─────────────────────────────────────────────────────────────┤
│  Every 1s (refresh cycle)                                   │
│  repo_groups, focus_state.focused_pane_id,                  │
│  layout.pane_row_targets,                                   │
│  pane_states.map[..].task_progress                          │
├─────────────────────────────────────────────────────────────┤
│  Every 10s (port scan, background)                          │
│  pane_states.map[..].ports, agent liveness cleanup          │
├─────────────────────────────────────────────────────────────┤
│  Every 10s (session_names background thread)                │
│  sessions.names map populated by session_poll_loop          │
├─────────────────────────────────────────────────────────────┤
│  Once at startup                                             │
│  theme, notices.claude_plugin_*,                            │
│  notices.claude_settings_has_residual_hooks,                │
│  notices.claude_plugin_notice, notices.missing_hook_groups  │
├─────────────────────────────────────────────────────────────┤
│  Every 10s TTL per path (git-info resolver worker)          │
│  PaneGitInfo (repo root, branch, dirty, ahead/behind)       │
├─────────────────────────────────────────────────────────────┤
│  On SIGUSR1 (tmux focus change)                             │
│  GlobalState reloaded from tmux variables                   │
├─────────────────────────────────────────────────────────────┤
│  Event-driven (agent hooks)                                 │
│  @pane_* tmux options, activity log files                   │
├─────────────────────────────────────────────────────────────┤
│  On user input                                              │
│  focus_state.focus, scrolls.panes,                          │
│  GlobalState fields, popup (PopupState enum),               │
│  timers.last_filter_click,                                  │
│  immediate selection / active-pane redraw                   │
├─────────────────────────────────────────────────────────────┤
│  Every frame (render)                                       │
│  layout.line_to_row, popup.area (Repo/Notices variants),    │
│  notices.button_col, notices.copy_targets,                  │
│  layout.hyperlink_overlays                                  │
└─────────────────────────────────────────────────────────────┘
```

---

## Data Flow

```
Agent hooks (hook.sh)
  → CLI `hook` subcommand (cli/hook.rs)
    → resolve_adapter() (event.rs) → adapter.parse() → AgentEvent
    → handle_event() writes @pane_* tmux options + /tmp activity log files
                        ↓
TUI main loop (app::run in app.rs; submodules app/{setup,workers,input,render})
  → startup plugin-state reads (cli/plugin_state.rs)
    → installed_plugins.json / ~/.claude/settings.json
    → initializes Claude notices state once
                        ↓
  → refresh() every 1s
    → query_sessions() (tmux.rs)     ← reads @pane_* via `tmux list-panes -a`
    → group_panes_by_repo() (group.rs)
    → rebuild_row_targets()          ← applies GlobalState filters
    → refresh_task_progress()        ← updates PaneRuntimeState.task_progress
    → refresh_port_data()            ← updates PaneRuntimeState.ports
    → scan_session_process_snapshot() ← detects dead panes and clears stale tmux metadata
                                          (teardown requires two consecutive dead scans;
                                           `dead_scan_streak` tracks the miss count)
                        ↓
  → git_info_rx.try_recv()           ← receives path→PaneGitInfo from resolver worker
  → notices popup render/copy state  ← derived from AppState plugin fields
                        ↓
  → ui::draw() renders frame         ← reads all AppState fields
```

---

## Key Types

```rust
enum Focus { Filter, Panes }
enum StatusFilter { All, Running, Background, Waiting, Idle, Error }
enum RepoFilter { All, Repo(String) }
enum PaneStatus { Running, Background, Waiting, Idle, Error, Unknown }
enum AgentType { Claude, Codex, OpenCode, Unknown }
enum PermissionMode { Default, Plan, AcceptEdits, Auto, DontAsk, BypassPermissions, Defer }

/// At-most-one popup state. The enum encodes both which popup is open
/// and its per-popup data so the invariant is checked by the type system.
enum PopupState {
    None,
    Repo { selected: usize, area: Option<Rect> },
    Notices { area: Option<Rect> },
    /// Modal text input shown when the user spawns a new worktree.
    SpawnInput {
        input: String,
        target_repo: String,
        target_repo_root: String,
        agent_idx: usize,
        mode_idx: usize,
        field: SpawnField,
        anchor_y: Option<u16>,
        error: Option<String>,
        area: Option<Rect>,
    },
    /// Confirmation prompt shown when the user removes a sidebar-spawned pane.
    RemoveConfirm {
        pane_id: String,
        branch: String,
        error: Option<String>,
        area: Option<Rect>,
    },
}

struct ScrollState {
    offset: usize,
    total_lines: usize,
    visible_height: usize,
}

struct HyperlinkOverlay {
    x: u16,
    y: u16,
    text: String,
    url: String,
}

struct PaneRuntimeState {
    ports: Vec<u16>,
    command: Option<String>,
    task_progress: Option<TaskProgress>,
    task_dismissed_total: Option<usize>,
    inactive_since: Option<u64>,
    task_progress_log_mtime: Option<SystemTime>,
    dead_scan_streak: u32,
}

/// Wraps `PaneRuntimeState` per pane. Methods delegate to the
/// underlying `HashMap`.
struct PaneRuntimeMap {
    map: HashMap<String, PaneRuntimeState>,
}

/// Focus-related fields grouped so UI code can pass them as a single
/// sub-struct rather than juggling four flat fields.
struct FocusState {
    sidebar_focused: bool,
    focus: Focus,
    focused_pane_id: Option<String>,
    prev_focused_pane_id: Option<String>,
}

struct ScrollStates {
    panes: ScrollState,
}

/// Session-name map scanned by a background thread so the TUI thread
/// never blocks on `~/.claude/sessions/*.json` reads or the Hermes
/// session-store probe.
struct SessionNamesState {
    names: HashMap<String, String>,
}

/// Frame-scoped render output cached for click hit-testing. Rewritten
/// every frame by the UI layer; consumed by mouse/keyboard handlers
/// before the next render.
struct FrameLayout {
    pane_row_targets: Vec<RowTarget>,
    line_to_row: Vec<Option<usize>>,
    repo_button_col: Option<u16>,
    repo_spawn_targets: Vec<RepoSpawnTarget>,
    spawn_remove_targets: Vec<SpawnRemoveTarget>,
    hyperlink_overlays: Vec<HyperlinkOverlay>,
}

/// Periodic-refresh bookkeeping. session_names refresh is intentionally
/// NOT here — it lives in a dedicated background thread so the TUI
/// thread never performs blocking filesystem I/O.
struct RefreshTimers {
    last_filter_click: Instant,
    last_port_refresh: Instant,
    port_scan_initialized: bool,
}

/// All fields for the ⓘ notices button and its popup.
struct NoticesState {
    button_col: Option<u16>,
    missing_hook_groups: Vec<NoticesMissingHookGroup>,
    claude_plugin_status: ClaudePluginStatus,
    claude_settings_has_residual_hooks: bool,
    claude_plugin_notice: Option<ClaudePluginNotice>,
    copy_targets: Vec<NoticesCopyTarget>,
    copied_at: Option<(String, Instant)>,
}
```

---

## State Invariants

1. `selected_pane_row` is clamped in `rebuild_row_targets()`: reset to 0 when `layout.pane_row_targets` is empty, otherwise clamped to the last valid index
2. Task progress has a 3-second debounce — prevents flicker when agent briefly pauses
3. Global state syncs via tmux variables — enables coordination across sidebar instances
4. Scroll positions are independent — the agent list owns the single `ScrollState`
5. `layout.line_to_row` is rebuilt every frame — ensures accurate click routing
6. Pane runtime state is pruned when the pane disappears — prevents stale per-pane ports and task progress from surviving after the agent is gone
7. At most one popup is open at a time — enforced structurally by the `PopupState` enum, not by parallel boolean flags
8. Hook-based cleanup wins when available; pid-based cleanup is a slower fallback that removes panes when the agent process is gone but the hook did not fire
