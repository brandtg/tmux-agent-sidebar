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
/// Last user prompt the agent received. Shown in the bottom tab
/// as activity context.
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
/// precedence over the `/rename` label map, which only covers
/// Claude Code.
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

// ─── Sidebar global option keys ─────────────────────────────────────

pub const SIDEBAR_PID: &str = "@sidebar_pid";
pub const SIDEBAR_WIDTH: &str = "@sidebar_width";
pub const SIDEBAR_POSITION: &str = "@sidebar_position";
pub const SIDEBAR_FILTER: &str = "@sidebar_filter";
/// Status filter view a newly opened sidebar lands on (`all` / `running` /
/// `background` / `waiting` / `idle` / `error`). Read once at startup;
/// unset or unrecognized values fall back to `all`.
pub const SIDEBAR_DEFAULT_VIEW: &str = "@sidebar_default_view";
pub const SIDEBAR_CURSOR: &str = "@sidebar_cursor";
pub const SIDEBAR_REPO_FILTER: &str = "@sidebar_repo_filter";
/// Live compact-view flag (`1`/`0`). Written whenever a sidebar toggles
/// compact mode so every open sidebar instance renders the same density.
/// Synced across sidebars like [`SIDEBAR_FILTER`] but deliberately not
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
/// "Show other windows" mode a newly opened sidebar lands on (`on` /
/// `off`, default `off`). Read once at startup; the live
/// [`SIDEBAR_SHOW_WINDOWS`] value is never restored on reopen.
pub const SIDEBAR_DEFAULT_SHOW_WINDOWS: &str = "@sidebar_default_show_windows";
pub const SIDEBAR_BOTTOM_HEIGHT: &str = "@sidebar_bottom_height";
/// Windows narrower than this many columns open the sidebar as a tmux
/// popup instead of a split pane (mobile viewports). `0` disables the
/// popup path entirely. Read by the `toggle` subcommand.
pub const SIDEBAR_POPUP_MAX_WIDTH: &str = "@sidebar_popup_max_width";
/// Window-scoped `#{window_layout}` snapshot taken immediately before the
/// sidebar split is created. The close paths replay it with
/// `select-layout` so every pane returns to its pre-sidebar geometry —
/// without it, tmux hands a killed pane's space to its layout-tree
/// neighbors and repeated toggles progressively shrink them. Cleared
/// again once the restore succeeds.
pub const SIDEBAR_SAVED_LAYOUT: &str = "@sidebar_saved_layout";
pub const SIDEBAR_PET: &str = "@sidebar_pet";
pub const SIDEBAR_NOTIFICATIONS: &str = "@sidebar_notifications";
pub const SIDEBAR_NOTIFICATIONS_EVENTS: &str = "@sidebar_notifications_events";
/// Comma-separated allow-list of agents (`claude`, `codex`, `opencode`)
/// the ⓘ notices evaluates for hook-setup problems. Unset/empty means
/// "no restriction". Parsed by `state::notices::parse_enabled_agents`.
pub const SIDEBAR_AGENTS: &str = "@sidebar_agents";

pub const SIDEBAR_COLOR_ACCENT: &str = "@sidebar_color_accent";
/// System-level sidebar mode written by `toggle-all` (`open` / `closed`).
/// New windows consult it at creation time (via the auto-create hook and
/// the `toggle --create-only` decision), so prefix+E means "sidebars
/// always on/off everywhere", including windows created afterwards.
/// Unset means no mode recorded: creation is governed by
/// [`SIDEBAR_AUTO_CREATE`] alone. Per-window `toggle` presses ignore the
/// mode — they remain explicit user actions.
pub const SIDEBAR_MODE: &str = "@sidebar_mode";
/// Whether a sidebar is created automatically for new windows (`on` /
/// `off`, default `on` via agent-sidebar.conf seeding). Consulted at
/// window-create time; only consulted while [`SIDEBAR_MODE`] is unset.
pub const SIDEBAR_AUTO_CREATE: &str = "@sidebar_auto_create";
pub const SIDEBAR_COLOR_BORDER: &str = "@sidebar_color_border";
pub const SIDEBAR_COLOR_ALL: &str = "@sidebar_color_all";
pub const SIDEBAR_COLOR_RUNNING: &str = "@sidebar_color_running";
pub const SIDEBAR_COLOR_WAITING: &str = "@sidebar_color_waiting";
pub const SIDEBAR_COLOR_IDLE: &str = "@sidebar_color_idle";
pub const SIDEBAR_COLOR_ERROR: &str = "@sidebar_color_error";
pub const SIDEBAR_COLOR_FILTER_INACTIVE: &str = "@sidebar_color_filter_inactive";
pub const SIDEBAR_COLOR_AGENT_CLAUDE: &str = "@sidebar_color_agent_claude";
pub const SIDEBAR_COLOR_AGENT_CODEX: &str = "@sidebar_color_agent_codex";
pub const SIDEBAR_COLOR_AGENT_OPENCODE: &str = "@sidebar_color_agent_opencode";
pub const SIDEBAR_COLOR_PET_BODY: &str = "@sidebar_color_pet_body";
pub const SIDEBAR_COLOR_PET_EYE: &str = "@sidebar_color_pet_eye";
pub const SIDEBAR_COLOR_TEXT_ACTIVE: &str = "@sidebar_color_text_active";
pub const SIDEBAR_COLOR_TEXT_MUTED: &str = "@sidebar_color_text_muted";
pub const SIDEBAR_COLOR_TEXT_INACTIVE: &str = "@sidebar_color_text_inactive";
pub const SIDEBAR_COLOR_SESSION: &str = "@sidebar_color_session";
pub const SIDEBAR_COLOR_PORT: &str = "@sidebar_color_port";
pub const SIDEBAR_COLOR_WAIT_REASON: &str = "@sidebar_color_wait_reason";
pub const SIDEBAR_COLOR_SELECTION: &str = "@sidebar_color_selection";
pub const SIDEBAR_COLOR_BRANCH: &str = "@sidebar_color_branch";
pub const SIDEBAR_COLOR_TASK_PROGRESS: &str = "@sidebar_color_task_progress";
pub const SIDEBAR_COLOR_SUBAGENT: &str = "@sidebar_color_subagent";
pub const SIDEBAR_COLOR_COMMIT_HASH: &str = "@sidebar_color_commit_hash";
pub const SIDEBAR_COLOR_DIFF_ADDED: &str = "@sidebar_color_diff_added";
pub const SIDEBAR_COLOR_DIFF_DELETED: &str = "@sidebar_color_diff_deleted";
pub const SIDEBAR_COLOR_FILE_CHANGE: &str = "@sidebar_color_file_change";
pub const SIDEBAR_COLOR_PR_LINK: &str = "@sidebar_color_pr_link";
pub const SIDEBAR_COLOR_SECTION_TITLE: &str = "@sidebar_color_section_title";
pub const SIDEBAR_COLOR_ACTIVITY_TIMESTAMP: &str = "@sidebar_color_activity_timestamp";
pub const SIDEBAR_COLOR_RESPONSE_ARROW: &str = "@sidebar_color_response_arrow";

pub const SIDEBAR_ICON_SET: &str = "@sidebar_icon_set";
pub const SIDEBAR_ICON_ALL: &str = "@sidebar_icon_all";
pub const SIDEBAR_ICON_RUNNING: &str = "@sidebar_icon_running";
pub const SIDEBAR_ICON_BACKGROUND: &str = "@sidebar_icon_background";
pub const SIDEBAR_ICON_WAITING: &str = "@sidebar_icon_waiting";
pub const SIDEBAR_ICON_IDLE: &str = "@sidebar_icon_idle";
pub const SIDEBAR_ICON_ERROR: &str = "@sidebar_icon_error";
pub const SIDEBAR_ICON_UNKNOWN: &str = "@sidebar_icon_unknown";

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
    s.replace(['\n', '|'], " ")
}

/// Fetch all global tmux options in a single subprocess call.
/// Returns a map of option name → value.
pub fn get_all_global_options() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(output) = run_tmux(&["show", "-g"]) {
        for line in output.lines() {
            // Format: "option-name value" or "@user_option value"
            if let Some((key, value)) = line.split_once(' ') {
                map.insert(key.to_string(), value.trim_matches('"').to_string());
            }
        }
    }
    map
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
    fn sanitize_option_value_replaces_newlines_and_pipes() {
        assert_eq!(sanitize_option_value("a\nb|c"), "a b c");
        assert_eq!(sanitize_option_value("clean"), "clean");
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
