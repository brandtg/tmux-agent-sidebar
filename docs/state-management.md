# State Management Architecture

## State Scope & Update Frequency

Every piece of state belongs to one of three scopes: **Global** (shared across all sidebar instances via tmux variables), **Per-pane** (keyed by tmux pane ID), or **Local** (single sidebar process only). The table below shows where each field lives, how often it updates, and what triggers the update.

### Global State (synced via tmux global variables)

Stored in `GlobalState`. Written to tmux on change, with the cursor save
debounced briefly so selection changes do not block redraw/input handling;
reloaded on SIGUSR1. `compact` follows the live `@sidebar_compact` value so
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
"active" at once. Because signals are edge-triggered and can be lost, the
snapshot worker also re-fetches the global options on a 5s floor, so every
instance converges without depending on catching a particular signal.
Cursor updates are deliberately not broadcast — they
fire on every navigation keystroke — and the runtime fold deliberately does
not adopt the shared cursor either: an index is only meaningful in the list
it was written against. On the SIGUSR1 focus-change reload both the cursor
row and the scroll offset are deliberately left alone: the user's last
selection and viewport are what they expect to come back to, and parking
the offset at the top used to make the viewport jump back down to the
preserved cursor on refocus when the context pane was unknown. The one
exception is the sidebar itself gaining focus: the transition tick instead
selects the row of the pane that held focus in this window just before the
jump (`select_focused_context_row`), so the list opens on the agent matching
the current tmux context — with the auto-scroll bringing that row into view.
Popups get the same treatment at startup, pinned to the pane they were
opened from, and so does a pane-mode sidebar: it opens unfocused, so the
shared cursor row it adopted is only a placeholder indexing another
window's list — the startup pass lands it on the window's context pane
instead (when that pane is unknown or filtered out, the adopted row rides
along, which is the intended landing behavior). A focus event is not a
user choice, so the selection is not
persisted to `@sidebar_cursor`. Window headers (the `index:name` rows under
a group title) are client-navigation, not selection: they map to `None` in
`line_to_row`, `jump_to_window` deliberately does not touch the cursor, and
their click persists nothing on purpose — it switches the tmux client and
stamps `focus_written_at`; the destination window's own sidebar owns its
cursor and reconciles it on the resulting focus event, in its own process.

| Field | Tmux Variable | Update Trigger | Description |
|-------|--------------|----------------|-------------|
| `selected_pane_row` | `@sidebar_cursor` | User input (j/k key); tmux write flushed after a short debounce | Cursor position in agent list; the cursor and the scroll offset are both preserved across unfocused reloads, and the cursor moves to the row of the pre-focus context pane when the sidebar itself gains focus |
| `repo_filter` | `@sidebar_repo_filter` | User input (repo popup) | Repository filter (All or specific repo) |
| `compact` | `@sidebar_compact` | User input (`c` key) | Compact one-line-per-pane rendering; the live value is shared by all open sidebars, and `@sidebar_default_compact_view` seeds it only before the first toggle |
| `group_by` | `@sidebar_group_by` | User input (`s` key) | Group panes by repo root or tmux session; the runtime toggle writes the shared value and broadcasts, matching `c` / `w` |
| `collapsed_groups` | `@sidebar_collapsed_groups` | User input (`z` / `Z` toggle, or a header click) | Group names folded to a header-only row, stored as a JSON array so names round-trip; persists across restarts and syncs to every open sidebar. `Z` folds everything from a fully expanded list and unfolds all once anything is folded. Cleared — locally and in the shared option, by every instance that observes it — when the grouping mode changes, since names are mode-specific |
| `show_windows` | `@sidebar_show_windows` | User input (`w` key) | Whether non-agent panes list under their window headers; the toggle writes the shared value and is restored on reopen. Landing default `@sidebar_default_show_windows` (default `off`) seeds it only while no live value exists |
| `todo_visible` | `@sidebar_todo_visible` | User input (`t` key, Global scope) | Whether the TODO panel is open; shared across sidebars in Global scope. Landing default `@sidebar_default_todo_visible` (default `off`) seeds it only while no live value exists |
| `todo_hide_completed` | `@sidebar_todo_hide_completed` | User input (`H` key) | Whether completed TODO items are collapsed; shared across sidebars. Landing default `@sidebar_default_todo_hide_completed` — the bundled conf seeds it `on`, so a fresh install starts with the section collapsed |
| `todo_scope` | `@sidebar_todo_scope` | tmux.conf (seed) | Whether the panel's open/closed state is shared (`global`) or per sidebar (`local`); the item list is always shared. Config-time only |
| `session_windows` | `@sidebar_session_windows` | tmux.conf (read on every sync) | Session-mode window level: when grouped by session, each window's rows render under an `index:name` header (the `Session > Window > agent|process` hierarchy) that takes a lightened tint of the accent while one of the window's panes is focused. Config-time only — there is no runtime toggle. Default off |
| `worktree_windows` | `@sidebar_worktree_windows` | tmux.conf (read on every sync) | Repo-mode window level: when grouped by repo, each window's rows render under an `index:name` header (the `Repo > Window > agent|process` hierarchy) that takes a lightened tint of the accent while one of the window's panes is focused. Config-time only — there is no runtime toggle. Default off |
| `worktree_titles` | `@sidebar_worktree_titles` | tmux.conf (read on every sync) | Worktree row titles: an agent row whose session has no user-given name is titled by the linked worktree it works in (`@pane_worktree_name`) instead of the agent's auto-derived name, and the branch line drops the repeated worktree name. Config-time only — there is no runtime toggle. Default off |
| `preview_lines` | `@sidebar_preview_lines` | tmux.conf (read on every sync) | Line cap for the agent message preview (the prompt / response body wrapped under the status row); clamped to 1–10, default 2. Config-time only — there is no runtime toggle |
| sidebar mode | `@sidebar_mode` | `toggle-all` sweep | Server-global `open`/`closed` intent; consulted by the new-window and new-session hooks (falling back to `@sidebar_auto_create` while unset) so toggle-all covers windows and sessions created afterwards |

`selected_pane_row`, `repo_filter`, and `collapsed_groups` each have a corresponding `last_saved_*` marker to prevent sync conflicts — tmux only overwrites the local value when it differs from what this instance last wrote. `compact` is exempt: the live tmux value is adopted unconditionally so sidebars can never diverge on density.

### Per-pane State (keyed by pane ID)

Written by `cli/hook.rs` on agent events, read back every **1 second** by the
snapshot worker's single `tmux list-panes -a` query (`fetch_pane_listing`)
and folded into state on the render thread (`apply_session_refresh` →
`apply_snapshot`). The synchronous `refresh()` runs only at startup; the `s`
regroup requests a worker snapshot via `snapshot_now` and folds it on the
next tick.

Each pane's runtime data is split into two buckets:

| Source | Update Trigger | Description |
|--------|----------------|-------------|
| tmux pane options | Event-driven + cleanup on agent exit | Agent type, status, cwd, permission mode, prompt, subagents, worktree, etc. |
| `PaneRuntimeState` in `AppState` | Refresh cycle + cleanup on agent exit | `ports`, `command`, `task_progress`, `task_dismissed_total`, `inactive_since`, window-task/finished bookkeeping, `attention_since`, dead-scan / port-miss streaks |

Pane options written to tmux:

| Tmux Option | Update Trigger | Description |
|-------------|----------------|-------------|
| `@pane_agent` | Every identity-bearing event | Agent type ("claude" / "codex" / "opencode" / "hermes" / "antigravity") |
| `@pane_status` | Every event | Status ("running" / "background" / "waiting" / "idle" / "error") |
| `@pane_cwd` | SessionStart, UserPromptSubmit, Notification, Stop, StopFailure, PermissionDenied, CwdChanged | Hook-reported working directory (skipped when subagents are active). Preferred over tmux's `pane_current_path` when resolving `PaneInfo.path`. |
| `@pane_launch_cwd` | First hook event reporting a cwd (seed); fresh `startup` SessionStart (overwrite) | Launch-time working directory used as the repo-grouping anchor. Once seeded it survives mid-session `cd`s — an agent that moves into another checkout stays grouped under the repo it was launched in. Worktree payloads resolve to `original_repo_dir` so worktree spawns group under the main repo. Cleared on SessionEnd / teardown; a stale anchor from a hard-killed agent is replaced by the next `startup` SessionStart. |
| `@pane_permission_mode` | SessionStart, hook event | Permission mode |
| `@pane_prompt` | UserPromptSubmit, Stop | Latest prompt or response text |
| `@pane_prompt_source` | UserPromptSubmit, Stop | "user" or "response" |
| `@pane_started_at` | UserPromptSubmit (real prompts; also seeded by the first tool event when missing) | Unix epoch when the current run started; synthetic empty-prompt submits do not reset an existing value |
| `@pane_attention` | SessionStart, StopFailure, UserPromptSubmit, ActivityLog (clear); Notification, PermissionDenied, TeammateIdle (`notification`); Stop (`done`) | Attention flag. `notification` = a hook wants the user's eye (waiting / permission / question / teammate idle); `done` = the turn finished and the output is unseen. The sidebar clears the flag when the user focuses the pane — reading the sidebar list does not count — and a new prompt / activity clears it on the hook side. |
| `@pane_wait_reason` | Notification (set); PermissionDenied, StopFailure, TeammateIdle (set); UserPromptSubmit, ToolStart, ActivityLog (clear) | Reason for waiting/error. Notification writes the agent's reason (`permission` / `permission_prompt` / `question` / ...); PermissionDenied writes `permission_denied`; TeammateIdle writes `teammate_idle:<name>`; StopFailure writes the error text. Cleared by a new prompt, a new tool attempt (Claude `PreToolUse`), or a tool result, except for OpenCode's `permission` / `question`, which stay sticky until the agent's explicit reply event (a synthetic empty `UserPromptSubmit`). |
| `@pane_bg_cmd` | ActivityLog (bg Bash), Refresh sweep (discover + clear), SessionEnd (clear) | Latest sanitized command of a Bash tool started with `run_in_background`. Its presence is the single source of truth for "live bg shell" — Stop routes to `background` while it is set, and the row body renders the command. Persists across UserPromptSubmit so shells spanning turns stay visible; overwritten by the next bg Bash. The refresh loop runs a `ps`-based liveness sweep each tick and clears the marker (plus downgrades `background → idle`) when the tracked process is gone. The same sweep also *discovers* untracked shells: a Claude shell-tool process (`zsh -c …/.claude/shell-snapshots/… && eval '<cmd>'`) alive under a Claude pane with no tracked command is adopted — healing spawn events the hook stream missed (routing gaps, crashed hooks, upgrades mid-spawn). Discovery skips running panes (a foreground tool call's shell has the same shape mid-execution), never overwrites a tracked command, and promotes status only `idle → background` (a `waiting` pane's prompt outranks any shell). Only the most recent bg Bash is tracked; older ones are not retained. |
| `@pane_bg_pid` | ActivityLog (bg Bash whose result carries a `pid`), Refresh sweep (clear), SessionEnd (clear) | PID of the process behind `@pane_bg_cmd`. Hermes' `terminal(background=true)` result reports it; Claude's `run_in_background` does not, so those spawns keep the string-match sweep. When present, the sweep's liveness check is a plain presence test for that PID in the full-`ps` snapshot — immune to quote stripping, `$(…)` substitution, arg reordering, and login-shell wrappers, none of which the raw-command-vs-ps-args match can survive. |
| `@pane_subagents` | SubagentStart/Stop | Comma-separated active subagent list. Liveness guard: non-empty ⇒ parent-owned identity writes are filtered. Cleared by the parent's Stop (Claude children are synchronous within a turn) and drained per SubagentStop. |
| `@pane_subagent_routes` | SubagentStart (append), SessionEnd teardown (clear) | Comma-separated `Type:id` routing copy of every subagent session id the pane has spawned. Consulted by the pane-less hook fallback (`cli/hook/resolve`) after the liveness guard closes: async subagent hosts and bg-pty-host continuations keep firing env-scrubbed events after the parent's Stop and the child's SubagentStop. Cleared only on real session teardown. |
| `@pane_worktree_name` | SessionStart | Worktree name (if applicable); when the payload carries none, the basename of the linked worktree the pane's live directory is in |
| `@pane_worktree_branch` | SessionStart | Worktree branch (if applicable); when the payload carries none, the branch of the linked worktree the pane's live directory is in |
| `@pane_session_id` | SessionStart, UserPromptSubmit, Notification, Stop, StopFailure, PermissionDenied, CwdChanged | Agent-reported session id (skipped when subagents are active) |
| `@pane_session_title` | opencode `session.created` / `session.updated` (via the plugin shim) | Agent session title. Parsed straight into `PaneInfo.session_name` and rendered as the row title; takes precedence over the polled name map (Claude `/rename` labels plus Hermes session titles). Cleared when the pane switches sessions or its metadata is torn down. |
| `@pane_role` | Sidebar split creation | `sidebar` marker written on the sidebar pane itself; the listing excludes that pane from agent rows |
| `@pane_notification_run_id` | SessionStart, UserPromptSubmit | Epoch-ms run scope used to dedup desktop notifications |
| `@pane_os_notify_permission_required` / `@pane_os_notify_task_completed` / `@pane_os_notify_task_failed` | Desktop-notification stamp writes | Dedup stamps — the same event kind on a pane fires at most once every 120 s |
| `@pane_pending_session_end` / `@pane_pending_worktree_remove` | (legacy — no event sets them any more) | Swept on SessionStart and teardown |
| `@pane_last_cmd` / `@pane_last_exit` | `shell-exit.sh` shell integration (not the hooks) | Last shell command and its exit code for non-agent rows |
| `@sidebar_flagged` | Sidebar `f` toggle | `1` / `0` inbox-pin on the pane; dies with the pane |
| `@pane_name` | (no writer) | Read-side placeholder only — the hook-written title option is `@pane_session_title` |

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
| `$XDG_RUNTIME_DIR/tmux-agent-activity_{pane_id}.log` (fallback `/tmp`) | Each ActivityLog event | Every 1s | Tool usage log (`epoch-seconds\|tool\|label`; legacy `HH:MM\|tool\|label` lines still parse, with their date treated as unknown and sorted oldest), capped at 200 lines (rewritten in place once it passes 210) |

### Local State (single sidebar process only)

| Field | Update Frequency | Description |
|-------|-----------------|-------------|
| `repo_groups` | Every 1s | Panes grouped by git repo root (built from the worker-fed snapshot via `apply_session_snapshot`, not stored separately as a session list). The path fed into repo resolution is the launch anchor `@pane_launch_cwd` when set, falling back to the live cwd — so mid-session `cd`s into other checkouts don't regroup the pane |
| `focus_state.focused_pane_id` | Every 1s, plus immediately on user-initiated pane jumps | Currently focused agent pane |
| `focus_state.proxy_focused_pane_id` | Every 1s, cleared immediately on user-initiated pane jumps | Agent pane standing in for the cursor while it sits on a non-agent pane whose row is hidden: the first agent sharing the cursor's window. Applied by the render layer only while the `w` toggle keeps window rows hidden |
| `focus_state.sidebar_focused` | Every 1s | Whether sidebar pane itself has focus |
| `focus_state.focus` | On user input | UI focus: `Panes` / `Todos`; input also triggers an immediate redraw so focus changes appear without waiting for the next poll tick. `Todos` survives a sidebar blur/refocus — coming back restores the keyboard context the user left, the same philosophy as the preserved cursor; the release path is the panel closing (locally or via a shared-visibility adoption), which hands focus back to `Panes` |
| `focus_state.prev_focused_pane_id` | Every 1s | Previous focused pane ID (for detecting focus changes) |
| `now` | Every 1s | Current Unix epoch |
| `scrolls.panes` | On user input / render | Agent list scroll position |
| `search` | On user input | Per-sidebar `/` query and whether the input line is open. Not broadcast: the query is transient. The cursor the user had before `/` is captured on a fresh open and restored by pane id on Esc; a jump or a plain navigation move supersedes the capture |
| `collapsed_cursor_group` | On fold/unfold via `rebuild_row_targets` | Group that swallowed the cursor when every row is hidden behind folds; lets `z` keep resolving the right fold with nothing visible. Across an empty list it survives only while the emptiness is fold-made — a repo-filter- or search-emptied list clears it, so a park hidden by a filter cannot silently die on the next refill. Transient, like the cursor |
| `theme` | Once at startup | Color theme from tmux `@sidebar_color_*` variables |
| `popup` | On user input / render | `PopupState` enum: `None` / `Repo` / `SpawnInput` / `Help` / `RemoveConfirm`. Enforces "at most one popup open" via the type system |
| `layout` | Every frame (render) | `FrameLayout` sub-struct bundling the ephemeral fields the UI rewrites every frame for click hit-testing: `pane_row_targets`, `group_starts` (visible group headers and their first row, used by `[` / `]`), `line_to_row`, `toolbar_row`, `header_width`, `menu_button_col` (the merged `ⓘ` menu button in the header row), `repo_spawn_targets`, `spawn_remove_targets`, `group_header_targets` (fold toggles), `window_header_targets` (`index:name` rows jump the tmux client to that window), `hyperlink_overlays`, `todo_row_targets`, `todo_input_rect`, `todo_area`, `flash_rect` |
| `notices` | Once at startup / on copy | `NoticesState` sub-struct: `missing_hook_groups`, `claude_plugin_status`, `claude_settings_has_residual_hooks`, `claude_plugin_notice`, `enabled_agents`, `copy_targets`, `copied_at`. The menu button's column lives in `layout.menu_button_col` |
| `timers` | Refresh cycle | `RefreshTimers` sub-struct gating periodic work: `last_port_refresh`, `port_scan_initialized`, `git_info_primed`, `last_bg_shell_sweep`, `last_approval_scan`, `approval_scan_initialized` |
| `spinner_frame` | Every 200ms (animation) | Spinner animation frame counter |
| `icons` | Once at startup | `StatusIcons` theme (overridable via tmux options) |
| `tmux_pane` | Once at startup | This sidebar's own tmux pane ID |
| `version_notice` | Once at startup (bg fetch) | GitHub release update notice, `None` when up-to-date |
| `sessions.names` | Every 10s (background thread) | `session_id → session name` map; scanned by `session_poll_loop` in `app/workers.rs` so the TUI thread never blocks on filesystem/subprocess I/O. Covers Claude `/rename` labels (`~/.claude/sessions/*.json`), Hermes session titles (`~/.hermes/state.db`), Antigravity conversation titles (`~/.gemini/antigravity-cli/conversation_summaries.db`), and OpenCode session titles (`~/.local/share/opencode/opencode.db`) — the dbs via read-only `sqlite3` probes; Codex has no session-title store. Re-applied to every pane on every tick because the snapshot rebuild wipes labels |

---

## Update Cycle Summary

```
┌─────────────────────────────────────────────────────────────┐
│  Every frame (~200ms)                                       │
│  layout.* (rebuilt by ui::draw), spinner animation          │
├─────────────────────────────────────────────────────────────┤
│  Every 1s (snapshot tick, worker-fed)                       │
│  SnapshotRequest → snapshot_poll_loop → fetch_pane_listing, │
│  apply_session_refresh / apply_snapshot: repo_groups,       │
│  focus_state.focused_pane_id, proxy_focused_pane_id,        │
│  layout.pane_row_targets, task_progress, chyron diff        │
│  buckets, TODO file reload (one stat per tick),             │
│  queue_port_scan_if_due, queue_approval_scan_if_due,        │
│  sweep_dead_bg_shells_if_due                                │
├─────────────────────────────────────────────────────────────┤
│  Every 2s (approval scan worker)                            │
│  Antigravity approval-prompt capture scan → waiting state   │
├─────────────────────────────────────────────────────────────┤
│  Every 5s (bg-shell sweep)                                  │
│  ps liveness for tracked background shells; dead shells     │
│  clear @pane_bg_cmd / @pane_bg_pid and downgrade to idle    │
├─────────────────────────────────────────────────────────────┤
│  Every 10s (port scan, background)                          │
│  pane_states.map[..].ports, agent liveness cleanup          │
├─────────────────────────────────────────────────────────────┤
│  Every 10s (session_names background thread)                │
│  sessions.names map populated by session_poll_loop          │
├─────────────────────────────────────────────────────────────┤
│  Once at startup                                            │
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
│  immediate selection / active-pane redraw                   │
├─────────────────────────────────────────────────────────────┤
│  Every frame (render)                                       │
│  layout.line_to_row, layout.menu_button_col,                │
│  popup area (Repo/SpawnInput/Help/RemoveConfirm variants),  │
│  notices.copy_targets, layout.hyperlink_overlays            │
└─────────────────────────────────────────────────────────────┘
```

---

## Data Flow

```
Agent hooks (hook.sh)
  → CLI `hook` subcommand (cli/hook.rs)
    → resolve_adapter() (event.rs) → adapter.parse() → AgentEvent
    → resolve_hook_pane() (cli/hook/resolve.rs) ← $TMUX_PANE, or a
      session-id fallback, then a process-ancestry walk when the session
      id matches no pane (Claude bg-pty-host continuation forks / async
      subagent hosts; the walk requires the ancestor's TMUX socket to
      match the hook's own)
    → handle_event() writes @pane_* tmux options + /tmp activity log files
                        ↓
TUI main loop (app::run in app.rs; submodules app/{setup,workers,input,render})
  → startup plugin-state reads (cli/plugin_state.rs)
    → installed_plugins.json / ~/.claude/settings.json
    → initializes Claude notices state once
                        ↓
  → every 1s: SnapshotRequest → snapshot_poll_loop (app/workers.rs)
    → fetch_pane_listing() (tmux.rs) ← reads @pane_* via `tmux list-panes -a`
    → apply_session_refresh() / apply_snapshot() (state/refresh.rs)
      (the synchronous refresh() runs only at startup; the `s` regroup
       requests a worker snapshot via `snapshot_now` and folds it on the
       next tick)
    → group_panes_by_repo() (group.rs)
    → rebuild_row_targets()          ← applies GlobalState filters
    → refresh_task_progress()        ← updates PaneRuntimeState.task_progress
    → queue_port_scan_if_due()       ← hands a scan to the port worker every 10s;
      apply_process_snapshot() applies the result — updates PaneRuntimeState
      ports/command, and advance_dead_scan_streaks() detects dead panes and
      clears stale tmux metadata (teardown requires two consecutive dead
      scans; `dead_scan_streak` tracks the miss count)
                        ↓
  → git_info_rx.try_recv()           ← receives path→PaneGitInfo from resolver worker
  → notices popup render/copy state  ← derived from AppState plugin fields
                        ↓
  → ui::draw() renders frame         ← reads all AppState fields
```

---

## Key Types

```rust
enum Focus { Panes, Todos }
enum RepoFilter { All, Repo(String) }

/// Top-row event channel (see docs/superpowers/specs/
/// 2026-11-15-agent-chyron-design.md). Event-driven, per tick.
struct ChyronState { current: Option<ChyronEvent> }
struct ChyronEvent { pane_id: String, kind: ChyronKind, label: String, shown_at: Instant }
enum ChyronKind { Done, Waiting, Failed }
enum PaneStatus { Running, Background, Waiting, Idle, Error, Unknown }
enum AgentType { Claude, Codex, OpenCode, Hermes, Antigravity, Unknown }
enum PermissionMode { Default, Plan, AcceptEdits, Auto, DontAsk, BypassPermissions, Defer }

/// At-most-one popup state. The enum encodes both which popup is open
/// and its per-popup data so the invariant is checked by the type system.
enum PopupState {
    None,
    Repo { selected: usize, area: Option<Rect>, repo_start: u16 },
    /// Modal text input shown when the user spawns a new agent (`n` / `+`).
    SpawnInput {
        input: String,
        target_repo: String,
        target_repo_root: String,
        target_session: String,
        kind_idx: usize,
        agent_idx: usize,
        field: SpawnField,
        anchor_y: Option<u16>,
        error: Option<String>,
        working: Option<&'static str>,
        area: Option<Rect>,
    },
    /// Scrollable keyboard reference (`?`); the renderer writes back
    /// content / viewport each frame for scroll clamping.
    Help { area: Option<Rect>, scroll: u16, content: u16, viewport: u16 },
    /// Confirmation prompt shown when the user removes a spawn-created pane.
    RemoveConfirm {
        pane_id: String,
        label: String,
        has_worktree: bool,
        warning: Vec<String>,
        error: Option<String>,
        working: Option<&'static str>,
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
    window_task_command: Option<String>,
    window_task_since: Option<u64>,
    window_finished: Option<WindowFinished>,
    attention_since: Option<u64>,
    ports: Vec<u16>,
    command: Option<String>,
    task_progress: Option<TaskProgress>,
    task_dismissed_total: Option<usize>,
    inactive_since: Option<u64>,
    task_progress_log_mtime: Option<SystemTime>,
    dead_scan_streak: u32,
    port_miss_streak: u32,
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
    window_active: bool,
    focus: Focus,
    focused_pane_id: Option<String>,
    focus_written_at: Option<Instant>,
    proxy_focused_pane_id: Option<String>,
    prev_focused_pane_id: Option<String>,
}

struct ScrollStates {
    panes: ScrollState,
    todos: ScrollState,
}

/// Session-name map scanned by a background thread so the TUI thread
/// never blocks on the agents' own session stores (Claude's JSON files,
/// Hermes / Antigravity / OpenCode sqlite3 probes).
struct SessionNamesState {
    names: HashMap<String, String>,
    derived: HashSet<String>,
}

/// Frame-scoped render output cached for click hit-testing. Rewritten
/// every frame by the UI layer; consumed by mouse/keyboard handlers
/// before the next render.
struct FrameLayout {
    pane_row_targets: Vec<RowTarget>,
    group_starts: Vec<GroupStart>,
    line_to_row: Vec<Option<usize>>,
    menu_button_col: Option<u16>,
    toolbar_row: u16,
    header_width: Option<u16>,
    repo_spawn_targets: Vec<RepoSpawnTarget>,
    spawn_remove_targets: Vec<SpawnRemoveTarget>,
    group_header_targets: Vec<GroupHeaderTarget>,
    window_header_targets: Vec<WindowHeaderTarget>,
    hyperlink_overlays: Vec<HyperlinkOverlay>,
    todo_row_targets: Vec<TodoRowTarget>,
    todo_input_rect: Option<Rect>,
    todo_area: Rect,
    flash_rect: Option<Rect>,
}

/// Periodic-refresh bookkeeping. session_names refresh is intentionally
/// NOT here — it lives in a dedicated background thread so the TUI
/// thread never performs blocking filesystem I/O.
struct RefreshTimers {
    last_port_refresh: Instant,
    port_scan_initialized: bool,
    git_info_primed: bool,
    last_bg_shell_sweep: Option<Instant>,
    last_approval_scan: Instant,
    approval_scan_initialized: bool,
}

/// All fields for the notices section of the repo popup. The menu
/// button's column lives in `FrameLayout.menu_button_col`.
struct NoticesState {
    missing_hook_groups: Vec<NoticesMissingHookGroup>,
    claude_plugin_status: ClaudePluginStatus,
    claude_settings_has_residual_hooks: bool,
    claude_plugin_notice: Option<ClaudePluginNotice>,
    enabled_agents: Vec<String>,
    copy_targets: Vec<NoticesCopyTarget>,
    copied_at: Option<(String, Instant)>,
}
```

---

## State Invariants

1. `selected_pane_row` is clamped in `rebuild_row_targets()`: reset to 0 when `layout.pane_row_targets` is empty, otherwise clamped to the last valid index
2. Task progress has a 3-second debounce — prevents flicker when agent briefly pauses
3. Global state syncs via tmux variables — enables coordination across sidebar instances
4. Scroll positions are independent — the pane list and the TODO panel each own a `ScrollState`
5. `layout.line_to_row` is rebuilt every frame — ensures accurate click routing
6. Pane runtime state is pruned when the pane disappears — prevents stale per-pane ports and task progress from surviving after the agent is gone
7. At most one popup is open at a time — enforced structurally by the `PopupState` enum, not by parallel boolean flags
8. Hook-based cleanup wins when available; pid-based cleanup is a slower fallback that removes panes when the agent process is gone but the hook did not fire
9. `proxy_focused_pane_id` is resolved per snapshot and cleared by deliberate jumps; the render layer applies it only while the `w` toggle hides window rows, so a stale cache can never double-light an agent next to its window row
