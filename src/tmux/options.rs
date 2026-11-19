use super::commands::run_tmux;

// ─── Pane-scoped option keys ─────────────────────────────────────────
//
// Single source of truth for every `@pane_*` tmux option the sidebar
// writes or reads. Hooks, the TUI refresh path, and the query layer
// all go through these constants so a typo can't silently corrupt
// pane state. Keep `clear_agent_pane_state` in `tmux/query.rs` and
// `clear_all_meta` in `cli/hook/context/meta.rs` in sync with this
// list — both sweep the full set on teardown.

/// Agent name the hooks identified for the pane (`claude` / `codex`
/// / `opencode`). Drives the sidebar's per-row icon.
pub const PANE_AGENT: &str = "@pane_agent";
/// Optional human-readable pane label. Currently queried to preserve
/// the `list-panes` field layout, but not rendered.
pub const PANE_NAME: &str = "@pane_name";
/// Visual attention flag that lights up the row when a hook wants the
/// user's eye: `notification` (waiting / permission / teammate idle)
/// or `done` (finished turn the user hasn't looked at yet). The
/// sidebar clears it when the user focuses the pane.
pub const PANE_ATTENTION: &str = "@pane_attention";
/// Hook-reported working directory, preferred over tmux's
/// `pane_current_path` for repo grouping.
pub const PANE_CWD: &str = "@pane_cwd";
/// Launch-time working directory, seeded on the first hook event that
/// reports a cwd and overwritten only by a fresh `startup` SessionStart.
/// Repo grouping keys off this anchor so an agent cd-ing into another
/// checkout mid-session doesn't regroup the pane under the new repo.
pub const PANE_LAUNCH_CWD: &str = "@pane_launch_cwd";
/// Latest backgrounded Bash command (sanitized). Presence is the
/// authoritative "live shell" signal: Stop routes to `background`
/// while this is set, and the sidebar surfaces the command text.
pub const PANE_BG_CMD: &str = "@pane_bg_cmd";
/// Value written to [`PANE_BG_CMD`] when the hook payload omits the real
/// command. The ps liveness sweep matches on this to skip its own entries
/// (placeholder has no process to verify against).
pub const BG_CMD_PLACEHOLDER: &str = "(background shell)";
/// PID of the process behind [`PANE_BG_CMD`], when the spawn result
/// reports one (Hermes `terminal(background=true)`). The ps liveness
/// sweep prefers this over the raw command string: shell-expanded ps
/// args can never match a stored command that used quotes or `$(…)`
/// substitution. Unset for spawns without a PID in the result (Claude's
/// `run_in_background`), which keep the string-match fallback.
pub const PANE_BG_PID: &str = "@pane_bg_pid";
/// Epoch-ms identifier regenerated on every SessionStart so
/// notification fingerprints stay scoped to the current run and
/// don't dedupe across restarts.
pub const PANE_NOTIFICATION_RUN_ID: &str = "@pane_notification_run_id";
/// Last fingerprint we fired a `PermissionRequired` desktop
/// notification for — used to suppress duplicates.
pub const PANE_OS_NOTIFY_PERMISSION_REQUIRED: &str = "@pane_os_notify_permission_required";
/// Same dedup stamp for `TaskCompleted` notifications.
pub const PANE_OS_NOTIFY_TASK_COMPLETED: &str = "@pane_os_notify_task_completed";
/// Same dedup stamp for `TaskFailed` notifications.
pub const PANE_OS_NOTIFY_TASK_FAILED: &str = "@pane_os_notify_task_failed";
/// Legacy marker — see
/// `cli/hook/context/pending.rs::PENDING_SESSION_END` for the
/// rationale for keeping it defined but never set.
pub const PANE_PENDING_SESSION_END: &str = "@pane_pending_session_end";
/// Legacy marker — no current event can set it (the WorktreeRemove
/// event was unreachable and has been removed), but a pre-upgrade
/// install may have left one behind; startup and stale-state sweeps
/// still clear it.
pub const PANE_PENDING_WORKTREE_REMOVE: &str = "@pane_pending_worktree_remove";
/// Permission mode in use by the agent (e.g. `plan`,
/// `acceptEdits`, `bypassPermissions`).
pub const PANE_PERMISSION_MODE: &str = "@pane_permission_mode";
/// Last user prompt the agent received. Shown in the agent row body.
pub const PANE_PROMPT: &str = "@pane_prompt";
/// Where the prompt came from (e.g. `UserPromptSubmit` vs
/// resumed session) — drives rendering nuance.
pub const PANE_PROMPT_SOURCE: &str = "@pane_prompt_source";
/// Pane role marker set by the setup flow (`sidebar` for the
/// sidebar pane itself) so the TUI can exclude itself from the
/// agent list.
pub const PANE_ROLE: &str = "@pane_role";
/// Agent-provided session id, surfaced in the status line for
/// quick reference.
pub const PANE_SESSION_ID: &str = "@pane_session_id";
/// Agent-provided session title (opencode's `session.created` /
/// `session.updated` carry it). Rendered as the row title; takes
/// precedence over the polled label map, which covers Claude
/// `/rename` and Hermes session titles.
pub const PANE_SESSION_TITLE: &str = "@pane_session_title";
/// Epoch-seconds timestamp of the current run's start — drives
/// the "running for Xs" label.
pub const PANE_STARTED_AT: &str = "@pane_started_at";
/// High-level status (`idle` / `running` / `waiting` / `clear`).
pub const PANE_STATUS: &str = "@pane_status";
/// Comma-separated `Type:id` list of currently-active subagents.
/// Non-empty ⇒ the pane is hosting subagent events and writes
/// from their hooks must be filtered out of parent metadata.
pub const PANE_SUBAGENTS: &str = "@pane_subagents";
/// Reason the pane is in `waiting` status (`permission`,
/// `session_resumed`, etc.).
pub const PANE_WAIT_REASON: &str = "@pane_wait_reason";
/// Branch name when the pane is attached to a git worktree.
pub const PANE_WORKTREE_BRANCH: &str = "@pane_worktree_branch";
/// Worktree slug (directory basename) for the attached worktree.
pub const PANE_WORKTREE_NAME: &str = "@pane_worktree_name";
/// User "inbox pin" toggled with the sidebar's `f` key: `1` = flagged,
/// `0` (or unset) = unflagged. Written by the TUI on toggle and read
/// back through the single `list-panes` query, so the pin survives
/// sidebar restarts and is visible from every sidebar instance. It
/// dies with the pane — that is acceptable for a come-back-later hint.
pub const PANE_FLAGGED: &str = "@sidebar_flagged";
/// Basename of the last foreground command the pane's interactive shell
/// started, written by the optional shell-integration snippet (`preexec`
/// on zsh, a DEBUG-trap preexec on bash). Paired with
/// [`PANE_LAST_EXIT`] to confirm that a tracked non-agent task has
/// actually finished (as opposed to a transient inter-command gap).
pub const PANE_LAST_CMD: &str = "@pane_last_cmd";
/// Exit code of the last completed foreground command, written by the
/// shell-integration snippet right after each command returns
/// (`precmd` / `PROMPT_COMMAND`). Read by `collect_other_panes` so a
/// finished non-agent task can be marked failed on a non-zero code.
pub const PANE_LAST_EXIT: &str = "@pane_last_exit";

// ─── Sidebar global option keys ─────────────────────────────────────

pub const SIDEBAR_PID: &str = "@sidebar_pid";
pub const SIDEBAR_WIDTH: &str = "@sidebar_width";
/// Upper bound on the resolved [`SIDEBAR_WIDTH`], in columns. Percentage
/// widths keep growing with the window (tmux also grows an existing
/// sidebar pane when the window gains columns), so on a very wide screen
/// a 20% sidebar would eat more columns than it needs. When set to a
/// positive integer, the resolved width is clamped to it — at split
/// creation and again by the `maintain-width` hook whenever the window
/// is resized while a sidebar is open. Unset, `0`, or invalid means no
/// cap. Parsed by `cli::toggle::sidebar_width_cells`.
pub const SIDEBAR_WIDTH_MAX: &str = "@sidebar_width_max";
pub const SIDEBAR_POSITION: &str = "@sidebar_position";
pub const SIDEBAR_CURSOR: &str = "@sidebar_cursor";
pub const SIDEBAR_REPO_FILTER: &str = "@sidebar_repo_filter";
/// Group names currently folded in the agents panel, stored as a JSON
/// array of strings (group names may contain almost anything, so a
/// plain separator would round-trip badly). Written whenever a sidebar
/// folds or unfolds a group (`z`, a header click, `M`, `Z`) and synced
/// across sidebars like [`SIDEBAR_REPO_FILTER`], so the folds survive
/// restarts and follow the user to every other sidebar. Cleared when
/// the grouping mode changes: names are mode-specific.
pub const SIDEBAR_COLLAPSED_GROUPS: &str = "@sidebar_collapsed_groups";
/// Live compact-view flag (`1`/`0`). Written whenever a sidebar toggles
/// compact mode so every open sidebar instance renders the same density.
/// Synced across sidebars like [`SIDEBAR_REPO_FILTER`] but deliberately not
/// restored on reopen — a new sidebar lands on
/// [`SIDEBAR_DEFAULT_COMPACT_VIEW`] instead.
pub const SIDEBAR_COMPACT: &str = "@sidebar_compact";
/// Compact-view mode a newly opened sidebar lands on (`on` / `off`,
/// default `off`). Read once at startup; the live [`SIDEBAR_COMPACT`]
/// value is never restored on reopen.
pub const SIDEBAR_DEFAULT_COMPACT_VIEW: &str = "@sidebar_default_compact_view";
/// Live "show other windows" flag (`1`/`0`). Written whenever a sidebar
/// toggles with `w` so every open sidebar renders the section alike.
pub const SIDEBAR_SHOW_WINDOWS: &str = "@sidebar_show_windows";
/// How the agents panel buckets panes into groups (`repo` / `session`).
/// `repo` (default) groups by resolved git repo root; `session` groups by
/// tmux session name — combined with [`SIDEBAR_SHOW_WINDOWS`] that turns
/// the panel into an agent-forward session manager. Read on every global
/// sync; unlike the toggle modes it has no runtime keybinding.
pub const SIDEBAR_GROUP_BY: &str = "@sidebar_group_by";
/// Session-mode window level (`on` / `off`, default `off`). When on and
/// the panel is grouped by session, each window's rows render under a
/// muted `index:name` header line — the `Session > Window >
/// agent|process` hierarchy (agents first, then the window's non-agent
/// panes; see `row_collector::collect`). Set in tmux.conf; read on every
/// global sync like [`SIDEBAR_GROUP_BY`], with no runtime toggle.
pub const SIDEBAR_SESSION_WINDOWS: &str = "@sidebar_session_windows";
/// Repo-mode window level (`on` / `off`, default `off`). The repo/worktree
/// counterpart of [`SIDEBAR_SESSION_WINDOWS`]: when on and the panel is
/// grouped by repo, each window's rows render under a muted `index:name`
/// header line — the `Repo > Window > agent|process` hierarchy (see
/// `row_collector::collect`). Set in tmux.conf; read on every global sync
/// like [`SIDEBAR_GROUP_BY`], with no runtime toggle.
pub const SIDEBAR_WORKTREE_WINDOWS: &str = "@sidebar_worktree_windows";
/// Title agent rows by their worktree (`on` / `off`, default `off`): a
/// session without a user-given name shows the linked worktree it is
/// working in instead of the agent's auto-derived name. Read on every
/// global sync like [`SIDEBAR_SESSION_WINDOWS`].
pub const SIDEBAR_WORKTREE_TITLES: &str = "@sidebar_worktree_titles";
/// "Show other windows" mode a newly opened sidebar lands on (`on` /
/// `off`, default `off`). Read once at startup; the live
/// [`SIDEBAR_SHOW_WINDOWS`] value is never restored on reopen.
pub const SIDEBAR_DEFAULT_SHOW_WINDOWS: &str = "@sidebar_default_show_windows";
/// Live "hide completed TODOs" flag (`1`/`0`, default `1`). Written
/// whenever a sidebar toggles the setting with `H` so every open sidebar
/// hides completed items alike. Seeded by
/// [`SIDEBAR_DEFAULT_TODO_HIDE_COMPLETED`] on a fresh tmux server.
pub const SIDEBAR_TODO_HIDE_COMPLETED: &str = "@sidebar_todo_hide_completed";
/// Landing value for [`SIDEBAR_TODO_HIDE_COMPLETED`] (`on` / `off`,
/// default `on`). Read once at startup; the live value wins once any
/// sidebar has toggled.
pub const SIDEBAR_DEFAULT_TODO_HIDE_COMPLETED: &str = "@sidebar_default_todo_hide_completed";
/// Live "TODO panel open" flag (`1`/`0`, default `0`). Written whenever a
/// sidebar opens or closes the manual TODO panel so every open sidebar
/// shows or hides it alike — the panel is one shared list, so its
/// visibility follows the user across sessions. Seeded by
/// [`SIDEBAR_DEFAULT_TODO_VISIBLE`] on a fresh tmux server. Only
/// consulted while [`SIDEBAR_TODO_SCOPE`] is `global`.
pub const SIDEBAR_TODO_VISIBLE: &str = "@sidebar_todo_visible";
/// Landing value for [`SIDEBAR_TODO_VISIBLE`] (`on` / `off`, default
/// `off`). Read once at startup; the live value wins once any sidebar
/// has toggled the panel.
pub const SIDEBAR_DEFAULT_TODO_VISIBLE: &str = "@sidebar_default_todo_visible";
/// Scope of the manual TODO panel (`global` / `local`, default `global`).
/// `global` shares the panel's open/closed state across every sidebar
/// instance via [`SIDEBAR_TODO_VISIBLE`]; `local` keeps it per instance
/// (the item list itself is always shared through the backing file).
pub const SIDEBAR_TODO_SCOPE: &str = "@sidebar_todo_scope";
/// Windows narrower than this many columns open the sidebar as a tmux
/// popup instead of a split pane (mobile viewports). `0` disables the
/// popup path entirely. Read by the `toggle` subcommand.
pub const SIDEBAR_POPUP_MAX_WIDTH: &str = "@sidebar_popup_max_width";
/// Explicit opt-in for hosting the narrow-viewport sidebar in a tmux 3.8
/// floating pane instead of a popup (`on` / `off`, default `off`). Off
/// because tmux 3.8 crashes the whole server intermittently under the
/// exact traffic a sidebar generates (alternate-screen entry in the
/// float, window resize under a float) — see docs/tmux-3.8.md. Read by
/// the `toggle` subcommand; anything but `on` keeps the popup host.
pub const SIDEBAR_FLOAT_HOST: &str = "@sidebar_float_host";
/// Whether the `maintain-width` hook may auto-hide a split-pane sidebar
/// when its window shrinks below [`SIDEBAR_POPUP_MAX_WIDTH`] (`on` /
/// `off`, default `on` via agent-sidebar.conf seeding). The hide is
/// per-window: the crossing window's own sidebar closes (its saved
/// layout replays) and the close is recorded on the window via
/// [`SIDEBAR_AUTO_HIDDEN`], so the same hook can restore the sidebar
/// when a wide viewport drives the window again. Read by the
/// `maintain-width` subcommand; empty is treated as `on`.
pub const SIDEBAR_AUTO_HIDE: &str = "@sidebar_auto_hide";
/// Window-scoped marker written by the `maintain-width` auto-hide after
/// it closes a window's sidebar (`1`), and cleared (unset) by every
/// other sidebar close path — the manual toggle, the `toggle-all` sweep,
/// the popup view's session sweep, and the duplicate collapse. The
/// restore direction of `maintain-width` reopens the window's split
/// sidebar only while this marker is set, so an auto-hide is undone
/// exactly once, by a wide viewport driving the window again, and a
/// manual close or a `toggle-all` sweep is never second-guessed.
pub const SIDEBAR_AUTO_HIDDEN: &str = "@sidebar_auto_hidden";
/// Window-scoped `#{window_layout}` snapshot taken immediately before the
/// sidebar split is created. The close paths replay it with
/// `select-layout` so every pane returns to its pre-sidebar geometry —
/// without it, tmux hands a killed pane's space to its layout-tree
/// neighbors and repeated toggles progressively shrink them. Cleared
/// again once the restore succeeds.
pub const SIDEBAR_SAVED_LAYOUT: &str = "@sidebar_saved_layout";
/// Window-scoped last-seen `#{window_width}`, written by the
/// `maintain-width` subcommand (and by the toggle paths whenever a split
/// sidebar or popup is opened) so the auto-hide decision compares against
/// the latest observation instead of a stale one. Cleared never — it is
/// simply overwritten on every observation.
pub const SIDEBAR_LAST_WINDOW_WIDTH: &str = "@sidebar_last_window_width";
pub const SIDEBAR_NOTIFICATIONS: &str = "@sidebar_notifications";
pub const SIDEBAR_NOTIFICATIONS_EVENTS: &str = "@sidebar_notifications_events";
/// Comma-separated allow-list of agents (`claude`, `codex`, `opencode`,
/// `hermes`) the ⓘ notices evaluates for hook-setup problems. Unset/empty
/// means "no restriction". Parsed by `state::notices::parse_enabled_agents`.
pub const SIDEBAR_AGENTS: &str = "@sidebar_agents";

/// Maximum number of lines the agent message preview (prompt / response
/// body) wraps to. Configurable in tmux.conf; parsed by
/// `state::GlobalState::apply_all` on every sync, clamped to 1..=10.
pub const SIDEBAR_PREVIEW_LINES: &str = "@sidebar_preview_lines";

pub const SIDEBAR_COLOR_ACCENT: &str = "@sidebar_color_accent";
/// Sidebar mode written by `toggle-all` (globally, `open` / `closed`).
/// New windows consult it at creation time (via the auto-create hook and
/// the `toggle --create-only` decision), so prefix+E means "sidebars
/// always on/off everywhere", including windows created afterwards.
/// Unset means no mode recorded: creation is governed by
/// [`SIDEBAR_AUTO_CREATE`] alone. Per-window `toggle` presses ignore
/// the mode — they remain explicit user actions.
pub const SIDEBAR_MODE: &str = "@sidebar_mode";
/// Whether a sidebar is created automatically for new windows (`on` /
/// `off`, default `on` via agent-sidebar.conf seeding). Consulted at
/// window-create time; only consulted while [`SIDEBAR_MODE`] is unset.
pub const SIDEBAR_AUTO_CREATE: &str = "@sidebar_auto_create";
pub const SIDEBAR_COLOR_BORDER: &str = "@sidebar_color_border";
pub const SIDEBAR_COLOR_RUNNING: &str = "@sidebar_color_running";
pub const SIDEBAR_COLOR_WAITING: &str = "@sidebar_color_waiting";
pub const SIDEBAR_COLOR_IDLE: &str = "@sidebar_color_idle";
pub const SIDEBAR_COLOR_ERROR: &str = "@sidebar_color_error";
pub const SIDEBAR_COLOR_FILTER_INACTIVE: &str = "@sidebar_color_filter_inactive";
pub const SIDEBAR_COLOR_AGENT_CLAUDE: &str = "@sidebar_color_agent_claude";
pub const SIDEBAR_COLOR_AGENT_CODEX: &str = "@sidebar_color_agent_codex";
pub const SIDEBAR_COLOR_AGENT_OPENCODE: &str = "@sidebar_color_agent_opencode";
pub const SIDEBAR_COLOR_AGENT_HERMES: &str = "@sidebar_color_agent_hermes";
pub const SIDEBAR_COLOR_AGENT_ANTIGRAVITY: &str = "@sidebar_color_agent_antigravity";
pub const SIDEBAR_COLOR_TEXT_ACTIVE: &str = "@sidebar_color_text_active";
/// Left-gutter marker color for flagged rows (see [`PANE_FLAGGED`]).
pub const SIDEBAR_COLOR_FLAG: &str = "@sidebar_color_flag";
pub const SIDEBAR_COLOR_TEXT_MUTED: &str = "@sidebar_color_text_muted";
pub const SIDEBAR_COLOR_TEXT_INACTIVE: &str = "@sidebar_color_text_inactive";
/// Window-level header (`@sidebar_session_windows` /
/// `@sidebar_worktree_windows`) — the `index:name` line rendered under
/// each group title in the mode it belongs to.
pub const SIDEBAR_COLOR_WINDOW_HEADER: &str = "@sidebar_color_window_header";
pub const SIDEBAR_COLOR_SESSION: &str = "@sidebar_color_session";
pub const SIDEBAR_COLOR_PORT: &str = "@sidebar_color_port";
pub const SIDEBAR_COLOR_WAIT_REASON: &str = "@sidebar_color_wait_reason";
pub const SIDEBAR_COLOR_SELECTION: &str = "@sidebar_color_selection";
pub const SIDEBAR_COLOR_BRANCH: &str = "@sidebar_color_branch";
pub const SIDEBAR_COLOR_TASK_PROGRESS: &str = "@sidebar_color_task_progress";
/// Neon magenta family for non-agent (process) window rows — the triangle
/// glyph's base color and the peak of its [`crate::PROCESS_PULSE`] breathe.
pub const SIDEBAR_COLOR_PROCESS: &str = "@sidebar_color_process";
pub const SIDEBAR_COLOR_SUBAGENT: &str = "@sidebar_color_subagent";
pub const SIDEBAR_COLOR_COMMIT_HASH: &str = "@sidebar_color_commit_hash";
pub const SIDEBAR_COLOR_DIFF_ADDED: &str = "@sidebar_color_diff_added";
pub const SIDEBAR_COLOR_DIFF_DELETED: &str = "@sidebar_color_diff_deleted";
pub const SIDEBAR_COLOR_FILE_CHANGE: &str = "@sidebar_color_file_change";
pub const SIDEBAR_COLOR_PR_LINK: &str = "@sidebar_color_pr_link";
pub const SIDEBAR_COLOR_SECTION_TITLE: &str = "@sidebar_color_section_title";
pub const SIDEBAR_COLOR_ACTIVITY_TIMESTAMP: &str = "@sidebar_color_activity_timestamp";

pub const SIDEBAR_ICON_SET: &str = "@sidebar_icon_set";
pub const SIDEBAR_ICON_RUNNING: &str = "@sidebar_icon_running";
pub const SIDEBAR_ICON_BACKGROUND: &str = "@sidebar_icon_background";
pub const SIDEBAR_ICON_WAITING: &str = "@sidebar_icon_waiting";
pub const SIDEBAR_ICON_IDLE: &str = "@sidebar_icon_idle";
pub const SIDEBAR_ICON_ERROR: &str = "@sidebar_icon_error";
pub const SIDEBAR_ICON_UNKNOWN: &str = "@sidebar_icon_unknown";
/// Glyph the chyron row uses for a finished-turn event (the status
/// icons have no "done" of their own: a done-but-unseen pane is still
/// idle/running by status). Same override pattern as the status icons.
pub const SIDEBAR_ICON_DONE: &str = "@sidebar_icon_done";

pub fn get_option(name: &str) -> Option<String> {
    run_tmux(&["show", "-gv", name])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Normalize a value before storing it in a tmux option. The sidebar's
/// `list-panes` query is line-based and uses `|` as its field separator,
/// so a newline splits the pane's record in two (the pane silently
/// vanishes from the sidebar) and a pipe shifts every downstream field.
/// Both are replaced with spaces. Callers that compare stored values
/// against raw process data (e.g. the bg-shell ps sweep) must apply the
/// same normalization so round-tripping through storage doesn't
/// silently break equality.
pub fn sanitize_option_value(s: &str) -> String {
    // `\r` rides along with `\n` on CRLF payloads; leaving it in place
    // makes `str::lines()` strip it from a rewritten log, so the value no
    // longer round-trips.
    s.replace(['\n', '\r', '|'], " ")
}

/// Fetch all global tmux options in a single subprocess call.
/// Returns a map of option name → value.
///
/// A failed call yields an empty map, indistinguishable from a server
/// with no options set. Callers that must not lose a shared-state reload
/// on a transient tmux failure (the snapshot worker) use
/// [`try_get_all_global_options`] instead.
pub fn get_all_global_options() -> std::collections::HashMap<String, String> {
    try_get_all_global_options().unwrap_or_default()
}

/// [`get_all_global_options`] with an honest failure signal: `None`
/// means the `show -g` spawn itself failed, not that no options exist.
/// The event loop retries a failed signal-driven reload instead of
/// silently applying an empty option set (which every guard would
/// accept as "nothing changed").
pub fn try_get_all_global_options() -> Option<std::collections::HashMap<String, String>> {
    let output = run_tmux(&["show", "-g"])?;
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        if let Some((key, value)) = parse_global_option_line(line) {
            map.insert(key, value);
        }
    }
    Some(map)
}

/// Parse one `tmux show -g` line into `(option, value)`.
///
/// tmux renders string option values shell-safely, and the wrapper it
/// picks depends on the value and the tmux version:
///
/// - plain values come back bare (`@sidebar_group_by session`);
/// - whitespace-carrying values are double-quoted with embedded `"`
///   backslash-escaped (`"has \"double\" quotes"`);
/// - values that need quoting but carry no whitespace may come back
///   single-quoted (tmux ≥ 3.8: `@sidebar_collapsed_groups '["s1","s2"]'`).
///
/// Backslashes are doubled in every style (`a\b` → `a\\b`), and `$`,
/// `` ` ``, `'`, `"`, and tab can all appear backslash-escaped. The old
/// parser only stripped double quotes, so on tmux ≥ 3.8 a JSON fold set
/// arrived as the literal string `'["s1","s2"]'`, `serde_json` rejected
/// it, and — since every sidebar reads the shared options through this
/// parser — `@sidebar_collapsed_groups` never synced: each sidebar kept
/// its own fold set while the shared option silently moved on.
fn parse_global_option_line(line: &str) -> Option<(String, String)> {
    let (key, raw_value) = line.split_once(' ')?;
    Some((key.to_string(), unquote_tmux_value(raw_value.trim_start())))
}

/// Decode a `show -g` value token: strip whichever quote style tmux
/// chose and resolve its backslash escapes. The escape set is decoded
/// identically inside and outside quotes — tmux doubles backslashes
/// unconditionally and escapes `"`, `'`, `$`, `` ` ``, and tab — and an
/// unknown `\x` is kept verbatim so nothing is silently eaten.
fn unquote_tmux_value(raw: &str) -> String {
    // A wrapping quote pair is unambiguous: tmux escapes a literal
    // leading quote (`"` arrives as `\"`, `'` as `\'`), so a token that
    // both starts and ends with the same quote character is always a
    // wrapper — and `''` is the empty value.
    let inner = if raw.len() >= 2 {
        let open = raw.as_bytes()[0];
        let close = *raw.as_bytes().last().unwrap();
        if open == close && (open == b'"' || open == b'\'') {
            &raw[1..raw.len() - 1]
        } else {
            raw
        }
    } else {
        raw
    };
    decode_tmux_escapes(inner)
}

fn decode_tmux_escapes(inner: &str) -> String {
    let mut value = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => value.push('"'),
            Some('\'') => value.push('\''),
            Some('\\') => value.push('\\'),
            Some('$') => value.push('$'),
            Some('`') => value.push('`'),
            Some('t') => value.push('\t'),
            Some(other) => {
                value.push('\\');
                value.push(other);
            }
            None => value.push('\\'),
        }
    }
    value
}

pub fn set_pane_option(pane: &str, key: &str, value: &str) {
    let value = sanitize_option_value(value);
    #[cfg(test)]
    if test_mock::intercept_set(pane, key, &value) {
        return;
    }
    // `--` so a value that begins with `-` (a prompt like "- fix the login
    // bug", an error string) is never parsed as a tmux flag — without it
    // tmux rejects the write and the error is discarded, silently dropping
    // the update.
    let _ = run_tmux(&["set", "-t", pane, "-p", "--", key, &value]);
}

/// Append `value` to a pane option atomically, inside the tmux server.
/// Hook events are independent processes; a get→append→set round trip
/// loses entries when two hooks run concurrently. The appended chunk is
/// passed verbatim by the caller — including any joining comma — because
/// tmux concatenates it onto the current value as-is (and stores it
/// verbatim when the option is unset).
pub fn append_pane_option(pane: &str, key: &str, chunk: &str) {
    let chunk = sanitize_option_value(chunk);
    #[cfg(test)]
    if test_mock::intercept_append(pane, key, &chunk) {
        return;
    }
    let _ = run_tmux(&["set", "-t", pane, "-p", "-a", "--", key, &chunk]);
}

pub fn unset_pane_option(pane: &str, key: &str) {
    #[cfg(test)]
    if test_mock::intercept_unset(pane, key) {
        return;
    }
    let _ = run_tmux(&["set", "-t", pane, "-p", "-u", "--", key]);
}

/// One pane-option write in a batched application: [`PaneOptionWrite::Set`]
/// stores a value, [`PaneOptionWrite::Unset`] clears the option.
pub enum PaneOptionWrite<'a> {
    Set { key: &'a str, value: &'a str },
    Unset { key: &'a str },
}

/// Apply multiple pane-option writes in a single tmux client process,
/// chaining the individual `set` commands with tmux's `;` argument
/// separator. Every identity-bearing hook event writes 5-10 options, each
/// of which is otherwise its own tmux spawn on the agent-side hook budget
/// (audit 2026-11-16 #5); one spawn per event instead of one per option is
/// the difference between a hook that keeps up with a busy turn and one
/// that lags it. An empty batch is a no-op.
pub fn apply_pane_options(pane: &str, writes: &[PaneOptionWrite<'_>]) {
    if writes.is_empty() {
        return;
    }
    #[cfg(test)]
    if test_mock::installed() {
        for write in writes {
            match write {
                PaneOptionWrite::Set { key, value } => {
                    test_mock::intercept_set(pane, key, &sanitize_option_value(value));
                }
                PaneOptionWrite::Unset { key } => {
                    test_mock::intercept_unset(pane, key);
                }
            }
        }
        return;
    }
    let sanitized: Vec<String> = writes
        .iter()
        .map(|write| match write {
            PaneOptionWrite::Set { value, .. } => sanitize_option_value(value),
            PaneOptionWrite::Unset { .. } => String::new(),
        })
        .collect();
    let _ = run_tmux(&build_batch_argv(pane, writes, &sanitized));
}

/// The tmux argv for a batch: each write is the exact argv the
/// single-write path above uses, and consecutive commands are separated
/// by a lone `;` argument, which tmux splits into one round trip.
fn build_batch_argv<'a>(
    pane: &'a str,
    writes: &'a [PaneOptionWrite<'a>],
    sanitized: &'a [String],
) -> Vec<&'a str> {
    let mut argv: Vec<&str> = Vec::new();
    for (index, write) in writes.iter().enumerate() {
        if index > 0 {
            argv.push(";");
        }
        match write {
            // `--` so a value that begins with `-` (a prompt like "- fix
            // the login bug") is never parsed as a tmux flag — same guard
            // as the single-write path above.
            PaneOptionWrite::Set { key, .. } => {
                argv.extend(["set", "-t", pane, "-p", "--", key, &sanitized[index]]);
            }
            PaneOptionWrite::Unset { key } => {
                argv.extend(["set", "-t", pane, "-p", "-u", "--", key]);
            }
        }
    }
    argv
}

pub fn get_pane_option_value(pane: &str, key: &str) -> String {
    #[cfg(test)]
    if let Some(value) = test_mock::intercept_get(pane, key) {
        return value;
    }
    run_tmux(&["show", "-t", pane, "-pv", key])
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Per-thread in-memory tmux pane store used by tests. Activated by
/// installing a mock with [`test_mock::install`]; until then, all
/// `set/unset/get_pane_option*` calls fall through to the real `tmux`
/// command. The whole module is `cfg(test)` so it has zero cost in
/// release builds.
#[cfg(test)]
pub mod test_mock {
    use std::cell::RefCell;
    use std::collections::HashMap;

    type Store = HashMap<(String, String), String>;

    thread_local! {
        static MOCK: RefCell<Option<Store>> = const { RefCell::new(None) };
    }

    /// Install a fresh mock store for the current thread. Returns a guard
    /// that uninstalls the mock on drop so concurrent tests don't leak
    /// state across each other.
    pub fn install() -> MockGuard {
        MOCK.with(|m| *m.borrow_mut() = Some(Store::new()));
        MockGuard
    }

    pub struct MockGuard;

    impl Drop for MockGuard {
        fn drop(&mut self) {
            MOCK.with(|m| *m.borrow_mut() = None);
        }
    }

    /// Pre-populate a pane option in the mock store. Call after `install`.
    pub fn set(pane: &str, key: &str, value: &str) {
        MOCK.with(|m| {
            if let Some(store) = m.borrow_mut().as_mut() {
                store.insert((pane.to_string(), key.to_string()), value.to_string());
            }
        });
    }

    /// Read a pane option from the mock store. Returns `None` if no mock
    /// is installed (so production code paths still hit real tmux).
    pub fn get(pane: &str, key: &str) -> Option<String> {
        MOCK.with(|m| {
            m.borrow().as_ref().map(|store| {
                store
                    .get(&(pane.to_string(), key.to_string()))
                    .cloned()
                    .unwrap_or_default()
            })
        })
    }

    /// Returns true if a key exists in the mock store. Useful for
    /// asserting that a teardown DID NOT remove a key.
    pub fn contains(pane: &str, key: &str) -> bool {
        MOCK.with(|m| {
            m.borrow()
                .as_ref()
                .is_some_and(|store| store.contains_key(&(pane.to_string(), key.to_string())))
        })
    }

    /// Whether a mock store is installed for the current thread. Batched
    /// writes route through the interceptors when it is.
    pub(super) fn installed() -> bool {
        MOCK.with(|m| m.borrow().is_some())
    }

    pub(super) fn intercept_set(pane: &str, key: &str, value: &str) -> bool {
        MOCK.with(|m| {
            if let Some(store) = m.borrow_mut().as_mut() {
                store.insert((pane.to_string(), key.to_string()), value.to_string());
                true
            } else {
                false
            }
        })
    }

    /// Mirrors tmux `set -a` semantics: the chunk is concatenated onto the
    /// current value as-is (an unset option behaves like an empty current
    /// value, so a leading comma in the chunk is preserved verbatim).
    pub(super) fn intercept_append(pane: &str, key: &str, chunk: &str) -> bool {
        MOCK.with(|m| {
            if let Some(store) = m.borrow_mut().as_mut() {
                let current = store
                    .get(&(pane.to_string(), key.to_string()))
                    .cloned()
                    .unwrap_or_default();
                store.insert(
                    (pane.to_string(), key.to_string()),
                    format!("{current}{chunk}"),
                );
                true
            } else {
                false
            }
        })
    }

    pub(super) fn intercept_unset(pane: &str, key: &str) -> bool {
        MOCK.with(|m| {
            if let Some(store) = m.borrow_mut().as_mut() {
                store.remove(&(pane.to_string(), key.to_string()));
                true
            } else {
                false
            }
        })
    }

    pub(super) fn intercept_get(pane: &str, key: &str) -> Option<String> {
        MOCK.with(|m| {
            m.borrow().as_ref().map(|store| {
                store
                    .get(&(pane.to_string(), key.to_string()))
                    .cloned()
                    .unwrap_or_default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_pane_options_matches_individual_writes() {
        let _guard = test_mock::install();
        let pane = "%BATCH";
        apply_pane_options(
            pane,
            &[
                PaneOptionWrite::Set {
                    key: PANE_AGENT,
                    value: "claude",
                },
                PaneOptionWrite::Unset {
                    key: PANE_WAIT_REASON,
                },
                // A leading `-` must survive like on the single-write path.
                PaneOptionWrite::Set {
                    key: PANE_PROMPT,
                    value: "- fix the login bug",
                },
            ],
        );
        assert_eq!(test_mock::get(pane, PANE_AGENT).as_deref(), Some("claude"));
        assert!(
            !test_mock::contains(pane, PANE_WAIT_REASON),
            "batched unsets must clear like unset_pane_option"
        );
        assert_eq!(
            test_mock::get(pane, PANE_PROMPT).as_deref(),
            Some("- fix the login bug")
        );
    }

    #[test]
    fn apply_pane_options_is_a_noop_when_empty() {
        let _guard = test_mock::install();
        let pane = "%BATCH_EMPTY";
        apply_pane_options(pane, &[]);
        assert!(test_mock::installed(), "mock still installed; no panic");
    }

    #[test]
    fn batch_argv_chains_single_write_argv_with_semicolons() {
        let writes = [
            PaneOptionWrite::Set {
                key: "@k1",
                value: "v 1",
            },
            PaneOptionWrite::Unset { key: "@k2" },
            PaneOptionWrite::Set {
                key: "@k3",
                value: "-p not a flag",
            },
        ];
        let sanitized: Vec<String> = writes
            .iter()
            .map(|w| match w {
                PaneOptionWrite::Set { value, .. } => sanitize_option_value(value),
                PaneOptionWrite::Unset { .. } => String::new(),
            })
            .collect();
        assert_eq!(
            build_batch_argv("%9", &writes, &sanitized),
            vec![
                "set",
                "-t",
                "%9",
                "-p",
                "--",
                "@k1",
                "v 1",
                ";",
                "set",
                "-t",
                "%9",
                "-p",
                "-u",
                "--",
                "@k2",
                ";",
                "set",
                "-t",
                "%9",
                "-p",
                "--",
                "@k3",
                "-p not a flag",
            ]
        );
    }

    #[test]
    fn mock_install_round_trips_pane_option() {
        let _guard = test_mock::install();
        set_pane_option("%1", PANE_STATUS, "running");
        assert_eq!(get_pane_option_value("%1", PANE_STATUS), "running");
        assert!(test_mock::contains("%1", PANE_STATUS));
        unset_pane_option("%1", PANE_STATUS);
        assert!(!test_mock::contains("%1", PANE_STATUS));
        // `get` on missing key returns empty string (mock semantics).
        assert!(get_pane_option_value("%1", PANE_STATUS).is_empty());
    }

    #[test]
    fn mock_helpers_get_and_contains_when_installed() {
        let _guard = test_mock::install();
        test_mock::set("%9", "@foo", "bar");
        assert_eq!(test_mock::get("%9", "@foo").as_deref(), Some("bar"));
        assert_eq!(test_mock::get("%9", "@missing").as_deref(), Some(""));
    }

    #[test]
    fn mock_guard_uninstalls_on_drop() {
        {
            let _guard = test_mock::install();
            test_mock::set("%7", "@x", "y");
            assert!(test_mock::contains("%7", "@x"));
        }
        // No mock installed now — `contains` returns false.
        assert!(!test_mock::contains("%7", "@x"));
    }

    #[test]
    fn sanitize_option_value_replaces_newlines_carriage_returns_and_pipes() {
        assert_eq!(sanitize_option_value("a\nb|c"), "a b c");
        assert_eq!(sanitize_option_value("a\r\nb|c"), "a  b c");
        assert_eq!(sanitize_option_value("clean"), "clean");
    }

    #[test]
    fn parse_global_option_line_unquotes_escaped_values() {
        // Legacy double-quoted shape (older tmux): inner `"` escaped.
        assert_eq!(
            parse_global_option_line("@repo \"\\\"quoted\\\" value\""),
            Some(("@repo".into(), "\"quoted\" value".into()))
        );
        assert_eq!(
            parse_global_option_line("@path \"a\\\\b\""),
            Some(("@path".into(), "a\\b".into()))
        );
        assert_eq!(
            parse_global_option_line("@sidebar_width 15%"),
            Some(("@sidebar_width".into(), "15%".into()))
        );
        assert_eq!(parse_global_option_line("no-value"), None);
    }

    #[test]
    fn parse_global_option_line_decodes_tmux38_quoting() {
        // Every line below is captured verbatim from `tmux show -g` on
        // tmux 3.8, which shell-escapes values and may single-quote
        // them. The fold set rides the single-quoted shape, which the
        // old double-quote-only parser left intact — so JSON parsing
        // failed on every reader and collapsed groups never synced.
        assert_eq!(
            parse_global_option_line("@sidebar_collapsed_groups '[\"s1\",\"s2\"]'"),
            Some(("@sidebar_collapsed_groups".into(), "[\"s1\",\"s2\"]".into()))
        );
        // `word`, `two words`, `has "double" quotes`, `it's`, `a\b`
        assert_eq!(
            parse_global_option_line("@t_plain word"),
            Some(("@t_plain".into(), "word".into()))
        );
        assert_eq!(
            parse_global_option_line("@t_space \"two words\""),
            Some(("@t_space".into(), "two words".into()))
        );
        assert_eq!(
            parse_global_option_line("@t_dquote \"has \\\"double\\\" quotes\""),
            Some(("@t_dquote".into(), "has \"double\" quotes".into()))
        );
        assert_eq!(
            parse_global_option_line("@t_squote \"it's single\""),
            Some(("@t_squote".into(), "it's single".into()))
        );
        assert_eq!(
            parse_global_option_line("@t_backslash a\\\\b"),
            Some(("@t_backslash".into(), "a\\b".into()))
        );
        // Empty values come back as `''`.
        assert_eq!(
            parse_global_option_line("@t_empty ''"),
            Some(("@t_empty".into(), "".into()))
        );
        // `$` and backtick are backslash-escaped inside double quotes;
        // tab is escaped as `\t`.
        assert_eq!(
            parse_global_option_line("@t5 \"dollar \\$HOME \\`tick\\`\""),
            Some(("@t5".into(), "dollar $HOME `tick`".into()))
        );
        assert_eq!(
            parse_global_option_line("@t4 tab\\tinside"),
            Some(("@t4".into(), "tab\tinside".into()))
        );
        // A lone quote character is emitted backslash-escaped, unwrapped.
        assert_eq!(
            parse_global_option_line("@t13 \\'"),
            Some(("@t13".into(), "'".into()))
        );
        assert_eq!(
            parse_global_option_line("@t12 \\\""),
            Some(("@t12".into(), "\"".into()))
        );
        // A single-quoted group name survives a double-quoted fold set.
        assert_eq!(
            parse_global_option_line("@t11 \"[\\\"it's\\\"]\""),
            Some(("@t11".into(), "[\"it's\"]".into()))
        );
    }

    #[test]
    fn quoted_global_option_round_trips_through_parser() {
        // The exact shape tmux emits for a value containing a double quote.
        let parsed =
            parse_global_option_line("@sidebar_repo_filter \"\\\"weird\\\" repo\"").unwrap();
        assert_eq!(parsed.1, "\"weird\" repo");
    }

    #[test]
    fn fold_set_round_trips_through_the_show_g_parser() {
        // The exact value the sidebar writes for a folded group, as tmux
        // 3.8 renders it in `show -g` output — the shape that regressed
        // the fold sync.
        let parsed =
            parse_global_option_line("@sidebar_collapsed_groups '[\"⎇ fix (repo)\",\"backend\"]'")
                .unwrap();
        let names: std::collections::HashSet<String> =
            serde_json::from_str(&parsed.1).expect("the decoded value must be valid JSON");
        assert_eq!(
            names,
            ["⎇ fix (repo)", "backend"]
                .map(str::to_string)
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn set_pane_option_sanitizes_value() {
        let _guard = test_mock::install();
        set_pane_option("%1", PANE_PROMPT, "- fix\nthe|bug");
        assert_eq!(
            get_pane_option_value("%1", PANE_PROMPT),
            "- fix the bug",
            "newlines and pipes must never reach tmux option storage"
        );
    }

    #[test]
    fn append_pane_option_concatenates_chunk_verbatim() {
        let _guard = test_mock::install();
        append_pane_option("%1", PANE_SUBAGENTS, ",Plan:sub-2");
        // Unset option: tmux stores the chunk as-is (leading comma and all);
        // readers filter empty entries.
        assert_eq!(get_pane_option_value("%1", PANE_SUBAGENTS), ",Plan:sub-2");
        append_pane_option("%1", PANE_SUBAGENTS, ",Explore:sub-1");
        assert_eq!(
            get_pane_option_value("%1", PANE_SUBAGENTS),
            ",Plan:sub-2,Explore:sub-1"
        );
    }

    #[test]
    fn append_pane_option_sanitizes_chunk() {
        let _guard = test_mock::install();
        append_pane_option("%1", PANE_SUBAGENTS, ",Explore:sub\n1");
        assert_eq!(
            get_pane_option_value("%1", PANE_SUBAGENTS),
            ",Explore:sub 1"
        );
    }
}
