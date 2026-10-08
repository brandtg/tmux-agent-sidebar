use std::collections::HashSet;
use std::time::Duration;

use crate::activity::{self, TaskProgress};
use crate::cli::sanitize_tmux_value;
use crate::process::{ProcessSnapshot, command_basename};
use crate::state::pane_runtime::WindowFinished;
use crate::tmux::{self, PaneAttention, PaneStatus, SessionInfo, TmuxSnapshot, WindowStatus};

use super::{AppState, PaneRuntimeMap};

/// Minimum flash duration for finished non-agent task indicators: a
/// tracked task that finished keeps its hollow-diamond flash for this
/// long even while its pane is focused, so the transition is actually
/// visible. Pane-focus ticks consume the flag after this grace —
/// without it, a finish that lands while the user is looking at the
/// pane is erased one second later and the row reads idle with no
/// indication anything happened.
pub(crate) const ATTENTION_MIN_FLASH_SECS: u64 = 5;

/// Minimum observed runtime before a finished non-agent task earns the
/// completion flash. Roughly the time after which the user would have
/// looked away to work on something else — commands quicker than this
/// (`ls`, `git status`, a failed build retry) complete while the user is
/// still looking at the pane, so a flash would be noise for every
/// little command. Observation time only: a task already running when
/// the sidebar starts is timed from the first observation and may
/// undercount, in which case its finish is treated like a quick
/// command and stays silent.
pub(crate) const TASK_MIN_RUN_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskProgressDecision {
    Clear,
    Show,
    Dismiss { total: usize },
    Skip,
}

/// A per-pane task-progress update computed in the first pass of
/// `refresh_task_progress`, applied back to `pane_states` in the second pass.
struct PaneTaskUpdate {
    pane_id: String,
    progress: Option<TaskProgress>,
    dismissed_total: Option<usize>,
    inactive_since: Option<u64>,
    log_mtime: Option<std::time::SystemTime>,
}

pub(crate) fn classify_task_progress(
    progress: &TaskProgress,
    dismissed_total: Option<usize>,
) -> TaskProgressDecision {
    if progress.is_empty() {
        return TaskProgressDecision::Clear;
    }
    if progress.all_completed() {
        if dismissed_total == Some(progress.total()) {
            TaskProgressDecision::Skip
        } else {
            TaskProgressDecision::Dismiss {
                total: progress.total(),
            }
        }
    } else {
        TaskProgressDecision::Show
    }
}

impl AppState {
    pub(crate) fn refresh_now(&mut self) {
        self.now = crate::time::now_epoch_secs();
    }

    pub fn apply_session_snapshot(
        &mut self,
        sessions: Vec<SessionInfo>,
        other_panes: Vec<crate::tmux::OtherPane>,
        sidebar_window_panes: Vec<(String, bool, String)>,
    ) {
        // First snapshot primes the anchor→git-info cache synchronously so
        // the very first frame groups by repo root. Later ticks consume
        // only the worker-maintained cache (see `git_info_poll_loop`) and
        // never spawn git on the render thread.
        if !self.timers.git_info_primed {
            let mut primed_paths: HashSet<String> = HashSet::new();
            for session in &sessions {
                for window in &session.windows {
                    for pane in &window.panes {
                        let anchor = pane.grouping_anchor();
                        if anchor.is_empty() || self.git_info_cache.contains_key(anchor) {
                            continue;
                        }
                        if primed_paths.insert(anchor.to_string()) {
                            let info = crate::group::resolve_pane_git_info(anchor);
                            self.git_info_cache.insert(anchor.to_string(), info);
                        }
                    }
                }
            }
            for pane in &other_panes {
                if pane.path.is_empty() || self.git_info_cache.contains_key(&pane.path) {
                    continue;
                }
                if primed_paths.insert(pane.path.clone()) {
                    let info = crate::group::resolve_pane_git_info(&pane.path);
                    self.git_info_cache.insert(pane.path.clone(), info);
                }
            }
            self.timers.git_info_primed = true;
        }
        self.repo_groups = crate::group::group_panes_by_repo(&sessions, &self.git_info_cache);
        self.other_windows = crate::group::group_other_windows_by_repo(
            &other_panes,
            &sessions,
            &self.git_info_cache,
        );
        self.update_attention_stamps();
        self.refresh_window_tasks(&other_panes);
        self.prune_pane_states_to_current_panes();
        self.rebuild_row_targets();
        self.find_focused_pane_from(&sidebar_window_panes);
    }

    /// Same "seen" contract as [`Self::mark_focused_pane_seen`], extended
    /// to non-agent window panes: a tracked finished task waits for the
    /// user's eye and is dropped once its pane has focus — but not before
    /// the minimum-flash grace ([`ATTENTION_MIN_FLASH_SECS`]), so the
    /// flash is actually rendered when the task ends under the user's
    /// gaze. Goes through `focus_state.focused_pane_id` like the agent
    /// path — that id is plain `pane_active` from tmux, so a window pane
    /// the user jumped to clears here too.
    fn mark_window_finished_seen(&mut self) {
        let Some(pane_id) = self.focus_state.focused_pane_id.as_ref() else {
            return;
        };
        let is_window_pane = self
            .other_windows
            .values()
            .any(|windows| windows.iter().any(|w| &w.pane_id == pane_id));
        if !is_window_pane {
            return;
        }
        let now = self.now;
        if let Some(runtime) = self.pane_states.get_mut(pane_id) {
            let fresh = runtime.window_finished.as_ref().is_some_and(|finished| {
                now.saturating_sub(finished.finished_at) < ATTENTION_MIN_FLASH_SECS
            });
            if !fresh {
                runtime.window_finished = None;
            }
        }
    }

    /// Per-tick tracking of non-agent foreground tasks. While a
    /// Task-classified command runs, its basename is remembered; when the
    /// pane drops back to a shell while the task is armed, the task is
    /// confirmed finished, so the row can flash for attention — green for
    /// exit 0 (or an unknown exit), red for a non-zero code. Two guards
    /// keep the flash high-signal:
    ///
    /// - [`TASK_MIN_RUN_SECS`]: quick commands complete while the user
    ///   is still looking at the pane and end silently;
    /// - `@pane_last_cmd`/`@pane_last_exit` (optional shell-integration
    ///   snippet): whenever integration data is present, the
    ///   confirmation requires the recorded command to match the tracked
    ///   one, skipping the one-frame shell gaps inside `a && b` chains.
    ///   Panes without the snippet confirm on the transition itself, and
    ///   their exit code is unknown (treated as success).
    fn refresh_window_tasks(&mut self, other_panes: &[crate::tmux::OtherPane]) {
        for pane in other_panes {
            let pane_id = pane.pane_id.clone();
            let prior_task = self
                .pane_state(&pane_id)
                .and_then(|s| s.window_task_command.clone());
            let prior_since = self.pane_state(&pane_id).and_then(|s| s.window_task_since);
            let prior_finished = self
                .pane_state(&pane_id)
                .and_then(|s| s.window_finished.clone());

            let (task_command, task_since, finished) =
                match crate::tmux::classify_window_status(&pane.command) {
                    WindowStatus::Task => {
                        let base = command_basename(&pane.command).to_string();
                        let prior = prior_task.as_deref();
                        if prior_finished.is_some() || prior != Some(base.as_str()) {
                            (Some(base), Some(self.now), None)
                        } else {
                            (prior_task, prior_since, None)
                        }
                    }
                    WindowStatus::Idle | WindowStatus::Busy => match prior_task {
                        None => (None, None, prior_finished),
                        Some(task) => {
                            let integration_active =
                                pane.last_cmd.is_some() || pane.last_exit.is_some();
                            let confirmed = if integration_active {
                                pane.last_cmd.as_deref() == Some(task.as_str())
                            } else {
                                true
                            };
                            let ran_long_enough = prior_since.is_some_and(|since| {
                                self.now.saturating_sub(since) >= TASK_MIN_RUN_SECS
                            });
                            if confirmed && ran_long_enough {
                                (
                                    None,
                                    None,
                                    Some(WindowFinished {
                                        command: task,
                                        exit_code: pane.last_exit,
                                        finished_at: self.now,
                                    }),
                                )
                            } else if confirmed {
                                // A quick command finished while the user was
                                // still looking at the pane — no flash is
                                // warranted, so the task ends silently and
                                // the row drops back to its plain status.
                                (None, None, None)
                            } else {
                                // The last recorded command is not the tracked
                                // task: a chained command's preexec fired between
                                // the two polls. Drop the stale tracking; a later
                                // poll catches the real completion (or the user's
                                // next task re-arms).
                                (None, None, None)
                            }
                        }
                    },
                };

            if task_command
                == self
                    .pane_state(&pane_id)
                    .and_then(|s| s.window_task_command.clone())
                && task_since == self.pane_state(&pane_id).and_then(|s| s.window_task_since)
                && finished
                    == self
                        .pane_state(&pane_id)
                        .and_then(|s| s.window_finished.clone())
            {
                continue;
            }
            let runtime = self.pane_state_mut(&pane_id);
            runtime.window_task_command = task_command;
            runtime.window_task_since = task_since;
            runtime.window_finished = finished;
        }
    }

    /// Stamp when each agent pane's current attention flag was first
    /// observed, so [`Self::mark_focused_pane_seen`] can tell a fresh
    /// "done and unseen" pulse from one that has been on screen long
    /// enough to have been seen. Runs every snapshot, ahead of the
    /// seen-tick: the stamp must exist even on the first tick after the
    /// hook lands, and observation time (like
    /// [`Self::window_task_since`]) undercounts when the flag predates
    /// the sidebar — a stale flag is treated as always-eligible.
    /// Only `Done` is stamped: `Notification` (permission / question
    /// prompts) must surface immediately and clears on focus as before.
    fn update_attention_stamps(&mut self) {
        let now = self.now;
        let transitions: Vec<(String, PaneAttention)> = self
            .repo_groups
            .iter()
            .flat_map(|g| g.panes.iter())
            .map(|(pane, _)| (pane.pane_id.clone(), pane.attention))
            .collect();
        for (pane_id, attention) in transitions {
            let runtime = self.pane_state_mut(&pane_id);
            match attention {
                PaneAttention::Done if runtime.attention_since.is_none() => {
                    runtime.attention_since = Some(now);
                }
                PaneAttention::None => runtime.attention_since = None,
                _ => {}
            }
        }
    }

    /// Focus counts as "seen": when the user is looking at an agent
    /// pane, drop its `@pane_attention` flag (a pending notification or
    /// an unseen finished turn) so the indicator only survives while the
    /// output is genuinely unread. Skipped while the sidebar itself
    /// holds focus — reading the list is not reading the output — and
    /// the previously focused pane id is then stale by design. Also
    /// skipped while the sidebar's window is not the session's active
    /// window: `find_active_pane` resolves within the sidebar's own
    /// window, whose `pane_active` marker survives after the user moves
    /// elsewhere, so consuming the flag then would swallow notifications
    /// for a pane the user never looked at.
    ///
    /// `Done` additionally honors [`ATTENTION_MIN_FLASH_SECS`]: a turn
    /// that finishes while its pane holds the (stale-surviving)
    /// `pane_active` marker was previously consumed within one tick and
    /// its row snapped to idle with never a visible pulse. The stamp
    /// comes from [`Self::update_attention_stamps`]; once the grace has
    /// elapsed a focused pane still consumes the flag, and flags observed
    /// before the sidebar started are aged (stamp set on first sight) so
    /// they clear immediately.
    pub fn mark_focused_pane_seen(&mut self, sidebar_window_active: bool) {
        if self.focus_state.sidebar_focused || !sidebar_window_active {
            return;
        }
        self.mark_window_finished_seen();
        let Some(pane_id) = self.focus_state.focused_pane_id.clone() else {
            return;
        };
        let Some(pane) = self.pane_by_id(&pane_id) else {
            return;
        };
        if pane.attention == PaneAttention::None {
            return;
        }
        if pane.attention == PaneAttention::Done {
            let now = self.now;
            let stamped = self
                .pane_states
                .get(&pane_id)
                .and_then(|state| state.attention_since);
            let fresh =
                stamped.is_some_and(|since| now.saturating_sub(since) < ATTENTION_MIN_FLASH_SECS);
            if fresh {
                return;
            }
        }
        tmux::unset_pane_option(&pane_id, tmux::PANE_ATTENTION);
        if let Some(pane) = self
            .repo_groups
            .iter_mut()
            .flat_map(|g| g.panes.iter_mut())
            .map(|(pane, _)| pane)
            .find(|pane| pane.pane_id == pane_id)
        {
            pane.attention = PaneAttention::None;
        }
    }

    fn clear_dead_agent_metadata(pane_id: &str) {
        for key in &[
            tmux::PANE_AGENT,
            tmux::PANE_STATUS,
            tmux::PANE_ATTENTION,
            tmux::PANE_PROMPT,
            tmux::PANE_PROMPT_SOURCE,
            tmux::PANE_SUBAGENTS,
            tmux::PANE_CWD,
            tmux::PANE_LAUNCH_CWD,
            tmux::PANE_PERMISSION_MODE,
            tmux::PANE_WORKTREE_NAME,
            tmux::PANE_WORKTREE_BRANCH,
            tmux::PANE_STARTED_AT,
            tmux::PANE_WAIT_REASON,
            tmux::PANE_SESSION_ID,
            tmux::PANE_BG_CMD,
        ] {
            tmux::unset_pane_option(pane_id, key);
        }

        let _ = std::fs::remove_file(activity::log_file_path(pane_id));
    }

    fn refresh_activity_data(&mut self) {
        self.refresh_activity_log();
        self.refresh_task_progress();
        self.auto_switch_tab();
    }

    /// Fast refresh: tmux state + activity log (called every 1s).
    /// Returns whether the sidebar's window is the active tmux window.
    ///
    /// Costs exactly one tmux spawn (`list-panes -a`): the sidebar's own
    /// focus flags and its window's pane list — historically two extra
    /// spawns — are extracted from the same output. Git resolution and the
    /// periodic ps/lsof scan run on worker threads; see `app/workers.rs`.
    pub fn refresh(&mut self) -> bool {
        self.refresh_now();
        let Some(snapshot) = tmux::query_session_snapshot(&self.tmux_pane) else {
            // A failed `list-panes` call is not evidence that no panes
            // exist. Applying an empty snapshot here would prune every
            // pane's runtime state, and `rebuild_row_targets` would reset
            // the repo filter to All and persist that reset in a tmux
            // global option — surviving restarts and propagating to every
            // sidebar instance. Hold the last known-good snapshot instead
            // and retry on the next tick. The window-active flag rides
            // the same way: reporting a fabricated `false` would trip the
            // per-tick global-option reload in the event loop.
            return self.focus_state.window_active;
        };
        // Inside a popup `TMUX_PANE` is the window's active pane at
        // popup-open time, not the sidebar itself, and the popup always
        // holds keyboard focus while open — so the snapshot's focus
        // report is not meaningful here. Reporting inactive would trip
        // the per-tick global-option reload in the event loop after two
        // ticks, so both are forced true in popup mode.
        if self.popup_mode {
            self.focus_state.sidebar_focused = true;
            self.focus_state.window_active = true;
        } else {
            self.focus_state.sidebar_focused = snapshot.sidebar_pane_active;
            self.focus_state.window_active = snapshot.sidebar_window_active;
        }
        let window_active = self.focus_state.window_active;
        let TmuxSnapshot {
            mut sessions,
            other_panes,
            mut process_snapshot,
            sidebar_window_panes,
            ..
        } = snapshot;
        self.sweep_dead_bg_shells_if_due(&mut sessions, &mut process_snapshot);
        self.queue_port_scan_if_due(&sessions);
        self.apply_session_snapshot(sessions, other_panes, sidebar_window_panes);
        self.mark_focused_pane_seen(window_active);
        // `apply_session_snapshot` rebuilds `repo_groups` from a fresh tmux
        // query, and every freshly parsed `PaneInfo` carries an empty
        // `session_name`. Guarding the re-application on a change flag
        // therefore labelled each pane for exactly one frame and lost the
        // label on the next tick -- the optimisation was protecting state
        // the rebuild had already destroyed. The lookup is a pure in-memory
        // `HashMap` hit (the filesystem scan lives in `session_poll_loop`),
        // so re-apply unconditionally.
        self.refresh_session_names();
        self.refresh_activity_data();
        window_active
    }

    /// Apply the current `session_id → name` map to each pane so the
    /// sidebar can render `/rename`-assigned labels. The map itself is
    /// refreshed off-thread by `session_poll_loop` in `main.rs`; this
    /// function only consumes the cached snapshot.
    ///
    /// A pane whose `session_name` is already non-empty carries a
    /// hook-provided title (`@pane_session_title`, e.g. opencode) that
    /// the tmux parse filled in; it outranks the map, which only covers
    /// Claude Code, so it is left untouched.
    fn refresh_session_names(&mut self) {
        for group in &mut self.repo_groups {
            for (pane, _) in &mut group.panes {
                if !pane.session_name.is_empty() {
                    continue;
                }
                if let Some(sid) = &pane.session_id
                    && let Some(name) = self.sessions.names.get(sid)
                {
                    pane.session_name.clone_from(name);
                } else {
                    pane.session_name.clear();
                }
            }
        }
    }

    /// Due-check for the periodic ps/lsof scan. The scan itself no longer
    /// runs here — it used to stall the render thread for up to the
    /// subprocess timeout every 10 seconds. Instead the scan targets are
    /// stashed in `pending_port_scan`; the event loop hands them to the
    /// port-scan worker and results come back through
    /// [`AppState::apply_process_snapshot`].
    pub(crate) fn queue_port_scan_if_due(&mut self, sessions: &[SessionInfo]) {
        const PORT_REFRESH_INTERVAL: Duration = Duration::from_secs(10);

        if self.timers.port_scan_initialized
            && self.timers.last_port_refresh.elapsed() < PORT_REFRESH_INTERVAL
        {
            return;
        }
        self.timers.port_scan_initialized = true;
        self.timers.last_port_refresh = std::time::Instant::now();
        // Pane pid is None only in degenerate cases (pid parse failure).
        // Such panes are still queued so the scan reports them as missed,
        // matching the old inline scanner where they could never appear in
        // the live set and decayed through the dead-scan streak.
        let mut targets: Vec<crate::port::PaneScanTarget> = sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .flat_map(|window| window.panes.iter())
            .map(|pane| crate::port::PaneScanTarget {
                pane_id: pane.pane_id.clone(),
                pane_pid: pane.pane_pid,
                agent: pane.agent.clone(),
                kind: crate::port::PaneKind::Agent,
            })
            .collect();
        // Non-agent window panes are scanned too so their listening ports
        // and foreground command show up. They are marked `Window` so the
        // dead-scan sweep never tears them down.
        for windows in self.other_windows.values() {
            for window in windows {
                targets.push(crate::port::PaneScanTarget {
                    pane_id: window.pane_id.clone(),
                    pane_pid: window.pane_pid,
                    agent: crate::tmux::AgentType::Unknown,
                    kind: crate::port::PaneKind::Window,
                });
            }
        }
        self.pending_port_scan = Some(targets);
    }

    pub(crate) fn take_pending_port_scan(&mut self) -> Option<Vec<crate::port::PaneScanTarget>> {
        self.pending_port_scan.take()
    }

    /// Apply a completed worker scan: refresh per-pane ports/commands for
    /// every pane the scan actually examined, advance dead-scan streaks,
    /// and tear down panes confirmed dead. Runs on the event loop when the
    /// result arrives, shortly after `queue_port_scan_if_due` fired.
    pub(crate) fn apply_process_snapshot(&mut self, scanned: crate::port::PaneProcessSnapshot) {
        for pane_id in &scanned.scanned_panes {
            let pane_state = self.pane_state_mut(pane_id);
            pane_state.ports = scanned
                .ports_by_pane
                .get(pane_id)
                .cloned()
                .unwrap_or_default();
            pane_state.command = scanned.command_by_pane.get(pane_id).cloned();
        }
        // Wipe only after two consecutive scans missed the agent. A
        // single scan can fail to see a live agent (ps timing,
        // wrapper shim), and wiping on the first miss used to delete
        // 14 `@pane_*` options plus the activity log every 10s while
        // the agent was still running. Mirrors the guard
        // `parse_pane_fields_with_processes` applies to Codex and
        // OpenCode panes before their stale-state teardown.
        let dead_panes = advance_dead_scan_streaks(
            &mut self.pane_states,
            &scanned.scanned_agent_panes,
            &scanned.live_agent_panes,
        );
        for pane_id in dead_panes {
            Self::clear_dead_agent_metadata(&pane_id);
            self.clear_pane_state(&pane_id);
        }
    }

    pub(crate) fn refresh_task_progress(&mut self) {
        let mut updates: Vec<PaneTaskUpdate> = Vec::new();
        for group in &self.repo_groups {
            for (pane, _) in &group.panes {
                let prior_state = self.pane_state(&pane.pane_id).cloned().unwrap_or_default();
                let current_mtime = activity::log_mtime(&pane.pane_id);
                // Skip the (full-file) re-parse when the activity log
                // hasn't been touched since the last tick AND the pane
                // is still active. We must still re-evaluate the
                // inactive-grace path while the agent is idle so that a
                // long-stalled progress bar gets dismissed even if the
                // log file itself stops changing.
                let agent_active = pane.status.is_active();
                let log_unchanged =
                    current_mtime.is_some() && current_mtime == prior_state.task_progress_log_mtime;
                if log_unchanged && agent_active {
                    // Just refresh the mtime bookkeeping so we don't
                    // accidentally drop the cache on a future iteration
                    // where current_mtime suddenly becomes None (e.g.
                    // /tmp clean-up). All other prior_state fields
                    // remain authoritative.
                    updates.push(PaneTaskUpdate {
                        pane_id: pane.pane_id.clone(),
                        progress: prior_state.task_progress.clone(),
                        dismissed_total: prior_state.task_dismissed_total,
                        inactive_since: None,
                        log_mtime: current_mtime,
                    });
                    continue;
                }
                // Read all entries for task progress (not limited to display max)
                // so that TaskCreate entries aren't lost when subagents flood the log
                let entries = activity::read_activity_log(&pane.pane_id, 0);
                let progress = activity::parse_task_progress(&entries);
                // Debounce inactive→dismiss transition to avoid flicker.
                //
                // The agent status can briefly drop to idle during normal operation
                // (e.g. when Claude Code processes a system prompt or between tool
                // calls). Without a grace period, the 1-second refresh cycle can
                // catch that transient idle state and immediately hide the task
                // progress bar, causing a visible flicker.
                //
                // We track when each pane first appeared inactive and only dismiss
                // after INACTIVE_GRACE_SECS have elapsed. If the agent returns to
                // Running/Waiting within that window, the timer is reset.
                const INACTIVE_GRACE_SECS: u64 = 3;

                let next_inactive_since = if !agent_active {
                    Some(prior_state.inactive_since.unwrap_or(self.now))
                } else {
                    None
                };
                let grace_expired = next_inactive_since
                    .is_some_and(|since| self.now.saturating_sub(since) >= INACTIVE_GRACE_SECS);

                let decision = if grace_expired && !progress.is_empty() && !progress.all_completed()
                {
                    TaskProgressDecision::Dismiss {
                        total: progress.total(),
                    }
                } else {
                    classify_task_progress(&progress, prior_state.task_dismissed_total)
                };
                let next_progress = match decision {
                    TaskProgressDecision::Clear => None,
                    TaskProgressDecision::Show => Some(progress),
                    TaskProgressDecision::Dismiss { .. } => None,
                    TaskProgressDecision::Skip => prior_state.task_progress.clone(),
                };
                let next_dismissed_total = match decision {
                    TaskProgressDecision::Clear | TaskProgressDecision::Show => None,
                    TaskProgressDecision::Dismiss { total } => Some(total),
                    TaskProgressDecision::Skip => prior_state.task_dismissed_total,
                };
                updates.push(PaneTaskUpdate {
                    pane_id: pane.pane_id.clone(),
                    progress: next_progress,
                    dismissed_total: next_dismissed_total,
                    inactive_since: next_inactive_since,
                    log_mtime: current_mtime,
                });
            }
        }
        for update in updates {
            let pane_state = self.pane_state_mut(&update.pane_id);
            pane_state.inactive_since = update.inactive_since;
            pane_state.task_dismissed_total = update.dismissed_total;
            pane_state.task_progress = update.progress;
            pane_state.task_progress_log_mtime = update.log_mtime;
        }
    }

    /// Run the background-shell liveness sweep at most once per
    /// `BG_SHELL_SWEEP_INTERVAL`. The first call always runs so the
    /// initial pane state is accurate.
    fn sweep_dead_bg_shells_if_due(
        &mut self,
        sessions: &mut [SessionInfo],
        process_snapshot: &mut Option<ProcessSnapshot>,
    ) {
        const BG_SHELL_SWEEP_INTERVAL: Duration = Duration::from_secs(5);
        let should_run = self
            .timers
            .last_bg_shell_sweep
            .is_none_or(|last| last.elapsed() >= BG_SHELL_SWEEP_INTERVAL);
        if !should_run {
            return;
        }
        sweep_dead_bg_shells(sessions, process_snapshot);
        self.timers.last_bg_shell_sweep = Some(std::time::Instant::now());
    }

    pub(crate) fn refresh_activity_log(&mut self) {
        let Some(ref pane_id) = self.focus_state.focused_pane_id else {
            self.activity.entries.clear();
            self.activity.log_cache = None;
            return;
        };
        let current_mtime = activity::log_mtime(pane_id);
        if let (Some(mtime), Some((cached_id, cached_mtime))) =
            (current_mtime, self.activity.log_cache.as_ref())
            && cached_id == pane_id
            && *cached_mtime == mtime
        {
            return;
        }
        // Task-reset markers are internal bookkeeping for parse_task_progress;
        // they should never appear in the user-facing Activity tab.
        let mut entries = activity::read_activity_log(pane_id, self.activity.max_entries);
        entries.retain(|e| e.tool != activity::TASK_RESET_MARKER);
        self.activity.entries = entries;
        self.activity.log_cache = current_mtime.map(|m| (pane_id.clone(), m));
    }
}

/// Consecutive dead scans required before the port-scan sweep tears down
/// a pane's tmux metadata and activity log. Scans run every
/// `PORT_REFRESH_INTERVAL`, so this is ~20s of confirmed absence; one
/// missed scan only starts the streak.
const REQUIRED_DEAD_SCANS: u32 = 2;

/// Advance every scanned pane's dead-scan streak against this scan's live
/// set and return the pane ids whose streak reached [`REQUIRED_DEAD_SCANS`].
/// A pane found alive has its streak reset to zero, so only back-to-back
/// misses confirm the death. Panes the scan did not examine (queued after
/// the scan request) are left untouched so a fresh pane is never punished
/// for a scan that never saw it.
fn advance_dead_scan_streaks(
    pane_states: &mut PaneRuntimeMap,
    scanned_panes: &HashSet<String>,
    live_agent_panes: &HashSet<String>,
) -> Vec<String> {
    let mut confirmed_dead = Vec::new();
    for pane_id in scanned_panes {
        let state = pane_states.entry_mut(pane_id);
        if live_agent_panes.contains(pane_id) {
            state.dead_scan_streak = 0;
        } else {
            state.dead_scan_streak = state.dead_scan_streak.saturating_add(1);
            if state.dead_scan_streak >= REQUIRED_DEAD_SCANS {
                confirmed_dead.push(pane_id.clone());
            }
        }
    }
    confirmed_dead
}

pub(crate) fn sweep_dead_bg_shells(
    sessions: &mut [SessionInfo],
    process_snapshot: &mut Option<ProcessSnapshot>,
) {
    let has_any = sessions
        .iter()
        .flat_map(|s| s.windows.iter())
        .flat_map(|w| w.panes.iter())
        .any(|p| p.bg_shell_cmd.is_some());
    if !has_any {
        return;
    }
    if process_snapshot.is_none() {
        *process_snapshot = ProcessSnapshot::scan();
    }
    if let Some(snapshot) = process_snapshot.as_ref() {
        clear_dead_bg_shells(sessions, snapshot);
    }
}

pub(crate) fn clear_dead_bg_shells(
    sessions: &mut [SessionInfo],
    process_snapshot: &ProcessSnapshot,
) {
    for session in sessions.iter_mut() {
        for window in &mut session.windows {
            for pane in &mut window.panes {
                let Some(cmd) = pane.bg_shell_cmd.as_deref() else {
                    continue;
                };
                if cmd == tmux::BG_CMD_PLACEHOLDER {
                    continue;
                }
                let Some(pane_pid) = pane.pane_pid else {
                    continue;
                };
                if process_snapshot
                    .command_lines_for_tree(&[pane_pid])
                    .iter()
                    .map(|line| sanitize_tmux_value(line))
                    .any(|line| ps_line_matches_cmd(&line, cmd))
                {
                    continue;
                }
                tmux::unset_pane_option(&pane.pane_id, tmux::PANE_BG_CMD);
                if pane.status == PaneStatus::Background {
                    tmux::set_pane_option(&pane.pane_id, tmux::PANE_STATUS, "idle");
                    pane.status = PaneStatus::Idle;
                }
                pane.bg_shell_cmd = None;
            }
        }
    }
}

/// Token-boundary substring match against a ps `command=` line that has
/// already been run through [`sanitize_tmux_value`]. Callers must pre-
/// normalize so the match sees the same `|`/`\n` → space canonicalization
/// applied when `@pane_bg_cmd` was stored; otherwise a piped bg command
/// (`tail -f log | grep X`) would miss on its first sweep.
///
/// Boundary rule treats `-`, `_`, `.` as part of the token (alongside
/// alphanumerics) so that a stored `"cargo-watch"` does not falsely
/// match `"cargo-watch-bin"` in ps. `/` is a boundary so a bare cmd
/// still matches when ps emits the full path (`/usr/local/bin/cargo-watch`).
fn ps_line_matches_cmd(normalized_line: &str, cmd: &str) -> bool {
    if cmd.is_empty() {
        return false;
    }
    let bytes = normalized_line.as_bytes();
    normalized_line.match_indices(cmd).any(|(idx, _)| {
        let end = idx + cmd.len();
        let before_ok = idx == 0 || !is_cmd_token_byte(bytes[idx - 1]);
        let after_ok = end == bytes.len() || !is_cmd_token_byte(bytes[end]);
        before_ok && after_ok
    })
}

fn is_cmd_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, SessionInfo, WindowInfo,
        WorktreeMetadata,
    };

    fn test_pane(id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status: PaneStatus::Running,
            attention: PaneAttention::None,
            agent: AgentType::Claude,
            path: "/tmp".into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
        }
    }

    fn test_session(panes: Vec<PaneInfo>) -> Vec<SessionInfo> {
        vec![SessionInfo {
            session_name: "main".into(),
            windows: vec![WindowInfo {
                window_id: "@0".into(),
                window_name: "test".into(),
                window_active: true,
                auto_rename: false,
                panes,
            }],
        }]
    }

    // ─── clear_dead_bg_shells ───────────────────────────────────────

    fn pane_with_bg(id: &str, cmd: &str, status: PaneStatus) -> PaneInfo {
        let mut p = test_pane(id);
        p.bg_shell_cmd = Some(cmd.into());
        p.status = status;
        p.pane_pid = Some(100);
        p
    }

    fn process_snapshot(ps_out: &str) -> ProcessSnapshot {
        ProcessSnapshot::from_ps_output(ps_out)
    }

    #[test]
    fn clear_dead_bg_shells_retains_shell_present_in_ps_output() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_ALIVE";
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "sleep 300");

        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            "sleep 300",
            PaneStatus::Background,
        )]);
        let snapshot = process_snapshot("100 1 zsh /bin/zsh -c eval 'sleep 300' < /dev/null\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        assert_eq!(
            sessions[0].windows[0].panes[0].bg_shell_cmd.as_deref(),
            Some("sleep 300"),
            "a matching ps line must leave the marker intact",
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_BG_CMD));
    }

    #[test]
    fn clear_dead_bg_shells_ignores_matching_command_in_other_pane_tree() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_OTHER_PANE";
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "sleep 300");
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "background");

        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            "sleep 300",
            PaneStatus::Background,
        )]);
        let snapshot = process_snapshot("100 1 zsh /bin/zsh\n200 1 zsh /bin/zsh -c 'sleep 300'\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        let pane = &sessions[0].windows[0].panes[0];
        assert!(
            pane.bg_shell_cmd.is_none(),
            "a matching command outside the pane process tree must not keep the marker alive",
        );
        assert_eq!(pane.status, PaneStatus::Idle);
    }

    #[test]
    fn clear_dead_bg_shells_clears_when_shell_missing_and_downgrades_background() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_DEAD";
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "sleep 300");
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "background");

        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            "sleep 300",
            PaneStatus::Background,
        )]);
        // ps output contains nothing matching "sleep 300".
        let snapshot = process_snapshot("100 1 zsh /bin/zsh\n200 1 ssh /usr/bin/ssh host\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        let pane = &sessions[0].windows[0].panes[0];
        assert!(
            pane.bg_shell_cmd.is_none(),
            "local pane copy must be cleared so this tick's render reflects it",
        );
        assert_eq!(
            pane.status,
            PaneStatus::Idle,
            "background with a dead shell must downgrade to idle",
        );
        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_BG_CMD));
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
        );
    }

    #[test]
    fn clear_dead_bg_shells_does_not_touch_non_background_status() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_STALE_RUNNING";
        tmux::test_mock::set(pane_id, tmux::PANE_STATUS, "running");
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "npm run dev");

        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            "npm run dev",
            PaneStatus::Running,
        )]);
        let snapshot = process_snapshot("100 1 zsh /bin/zsh\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        let pane = &sessions[0].windows[0].panes[0];
        assert!(pane.bg_shell_cmd.is_none());
        assert_eq!(
            pane.status,
            PaneStatus::Running,
            "non-background status must be left alone",
        );
        assert_eq!(
            tmux::test_mock::get(pane_id, tmux::PANE_STATUS).as_deref(),
            Some("running"),
        );
    }

    #[test]
    fn clear_dead_bg_shells_preserves_placeholder_cmd() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_PLACEHOLDER";
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, tmux::BG_CMD_PLACEHOLDER);

        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            tmux::BG_CMD_PLACEHOLDER,
            PaneStatus::Background,
        )]);
        let snapshot = process_snapshot("");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        assert_eq!(
            sessions[0].windows[0].panes[0].bg_shell_cmd.as_deref(),
            Some(tmux::BG_CMD_PLACEHOLDER),
            "placeholder must survive — we cannot prove the shell is dead",
        );
        assert!(tmux::test_mock::contains(pane_id, tmux::PANE_BG_CMD));
    }

    #[test]
    fn clear_dead_bg_shells_treats_prefix_collision_as_dead() {
        // Regression: a naive `str::contains` match kept the marker
        // alive forever when a shorter stored cmd was a prefix of a
        // live longer cmd.
        let _guard = tmux::test_mock::install();
        let pane_id = "%BG_PREFIX_COLLIDE";
        tmux::test_mock::set(pane_id, tmux::PANE_BG_CMD, "sleep 3");
        let mut sessions = test_session(vec![pane_with_bg(
            pane_id,
            "sleep 3",
            PaneStatus::Background,
        )]);
        let snapshot = process_snapshot("100 1 zsh /bin/zsh -c 'sleep 30' < /dev/null\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        assert!(
            sessions[0].windows[0].panes[0].bg_shell_cmd.is_none(),
            "the marker for `sleep 3` must clear when only `sleep 30` is running",
        );
    }

    #[test]
    fn clear_dead_bg_shells_no_op_when_no_pane_has_bg_marker() {
        let _guard = tmux::test_mock::install();
        let mut sessions = test_session(vec![test_pane("%1")]);
        let snapshot = process_snapshot("100 1 zsh /bin/zsh\n");

        clear_dead_bg_shells(&mut sessions, &snapshot);

        assert!(sessions[0].windows[0].panes[0].bg_shell_cmd.is_none());
    }

    // ─── ps_line_matches_cmd ────────────────────────────────────────

    #[test]
    fn ps_line_matches_cmd_empty_cmd_never_matches() {
        assert!(!ps_line_matches_cmd("anything", ""));
        assert!(!ps_line_matches_cmd("", ""));
    }

    #[test]
    fn ps_line_matches_cmd_no_occurrence_is_false() {
        assert!(!ps_line_matches_cmd("/bin/zsh", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_full_line_match() {
        assert!(ps_line_matches_cmd("sleep 300", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_match_at_start() {
        assert!(ps_line_matches_cmd("sleep 300 --flag", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_match_at_end() {
        assert!(ps_line_matches_cmd("/bin/zsh -c sleep 300", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_rejects_trailing_alnum() {
        // Stored "sleep 3" must not match a live "sleep 30" process.
        assert!(!ps_line_matches_cmd("sleep 30", "sleep 3"));
        assert!(!ps_line_matches_cmd("/bin/zsh sleep 300 end", "sleep 3"));
    }

    #[test]
    fn ps_line_matches_cmd_rejects_leading_alnum() {
        // `mysleep 300` must not match `sleep 300`.
        assert!(!ps_line_matches_cmd("mysleep 300", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_accepts_non_alnum_boundary_chars() {
        // Quotes, parens, semicolons — all count as word boundaries.
        assert!(ps_line_matches_cmd(
            "/bin/zsh -c 'sleep 300' end",
            "sleep 300"
        ));
        assert!(ps_line_matches_cmd("(sleep 300);", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_multibyte_adjacent_treated_as_boundary() {
        // A non-ASCII char adjacent to the match must not panic and
        // must count as a boundary (the byte is not ASCII-alnum).
        assert!(ps_line_matches_cmd("🚀sleep 300", "sleep 300"));
        assert!(ps_line_matches_cmd("sleep 300🚀", "sleep 300"));
    }

    #[test]
    fn ps_line_matches_cmd_piped_cmd_matches_ps_line_with_pipe() {
        // Regression for Bug A: `sanitize_tmux_value` replaces `|` with
        // a space before writing `@pane_bg_cmd`, so the stored value
        // never contains a pipe. ps, however, emits the raw command line
        // with `|` intact — so callers must pre-normalize ps lines through
        // the same filter before this match can see the two sides as equal.
        let raw_line = "/bin/zsh -c 'tail -f log.txt | grep ERROR'";
        let normalized = sanitize_tmux_value(raw_line);
        let stored = "tail -f log.txt   grep ERROR"; // post-sanitize
        assert!(ps_line_matches_cmd(&normalized, stored));
    }

    #[test]
    fn ps_line_matches_cmd_newline_cmd_matches_ps_line() {
        // Same story for `\n` in the original command.
        let raw_line = "/bin/zsh -c 'echo one\necho two'";
        let normalized = sanitize_tmux_value(raw_line);
        let stored = "echo one echo two";
        assert!(ps_line_matches_cmd(&normalized, stored));
    }

    #[test]
    fn ps_line_matches_cmd_rejects_hyphenated_continuation() {
        // Regression for Bug B: `cargo-watch` must not match
        // `cargo-watch-bin` — `-` is part of the token, not a boundary.
        assert!(!ps_line_matches_cmd(
            "/usr/local/bin/cargo-watch-bin",
            "cargo-watch"
        ));
        assert!(!ps_line_matches_cmd("npm-run-all-ng foo", "npm-run-all"));
    }

    #[test]
    fn ps_line_matches_cmd_rejects_dot_continuation() {
        assert!(!ps_line_matches_cmd("node app.js.bak watch", "node app.js"));
    }

    #[test]
    fn ps_line_matches_cmd_accepts_path_prefixed_cmd() {
        // `/` stays a boundary so a bare `cargo-watch` still matches
        // the full-path form ps typically emits for installed binaries.
        assert!(ps_line_matches_cmd(
            "/usr/local/bin/cargo-watch",
            "cargo-watch"
        ));
        assert!(ps_line_matches_cmd(
            "./cargo-watch --watch src",
            "cargo-watch"
        ));
    }

    #[test]
    fn ps_line_matches_cmd_multiple_occurrences_one_valid_matches() {
        // If any occurrence satisfies the boundary check, return true.
        // First occurrence is glued to `mysleep 3`, second is standalone.
        assert!(ps_line_matches_cmd(
            "mysleep 30 /bin/sh sleep 30",
            "sleep 30"
        ));
    }

    // ─── dead-scan streak gate ──────────────────────────────────────

    fn scanned(pane_ids: &[&str]) -> HashSet<String> {
        pane_ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn advance_dead_scan_streaks_first_miss_does_not_confirm() {
        let mut pane_states = PaneRuntimeMap::new();

        let dead = advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());

        assert!(dead.is_empty(), "one missed scan must not confirm death");
        assert_eq!(pane_states.get("%1").unwrap().dead_scan_streak, 1);
    }

    #[test]
    fn advance_dead_scan_streaks_second_consecutive_miss_confirms() {
        let mut pane_states = PaneRuntimeMap::new();

        advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());
        let dead = advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());

        assert_eq!(dead, vec!["%1".to_string()]);
    }

    #[test]
    fn advance_dead_scan_streaks_live_scan_resets_streak() {
        let mut pane_states = PaneRuntimeMap::new();
        let live: HashSet<String> = HashSet::from(["%1".to_string()]);

        advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());
        advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &live);
        let dead = advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());

        assert!(
            dead.is_empty(),
            "a live scan in between must reset the streak"
        );
        assert_eq!(pane_states.get("%1").unwrap().dead_scan_streak, 1);
    }

    #[test]
    fn advance_dead_scan_streaks_leaves_unscanned_panes_untouched() {
        // A pane queued after the scan request was built was never
        // examined by that scan; punishing it with a miss would let a
        // brand-new pane decay toward teardown.
        let mut pane_states = PaneRuntimeMap::new();

        advance_dead_scan_streaks(&mut pane_states, &scanned(&["%1"]), &HashSet::new());

        assert!(pane_states.get("%2").is_none());
    }

    // ─── refresh_session_names ──────────────────────────────────────
    //
    // refresh_session_names no longer scans the filesystem itself; it
    // only consumes the cached `session_names` map populated by the
    // dedicated polling thread in `main.rs`. These tests pin that
    // contract: the function must apply the cached snapshot to every
    // pane and clear stale labels for panes whose session_id is no
    // longer in the map.

    fn pane_with_session(id: &str, session_id: &str) -> PaneInfo {
        let mut p = test_pane(id);
        p.session_id = Some(session_id.to_string());
        p
    }

    fn state_with_panes(panes: Vec<PaneInfo>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![crate::group::RepoGroup {
            name: "test".into(),
            has_focus: true,
            panes: panes
                .into_iter()
                .map(|p| (p, crate::group::PaneGitInfo::default()))
                .collect(),
        }];
        state
    }

    #[test]
    fn refresh_session_names_applies_cached_map_to_panes() {
        let mut state = state_with_panes(vec![
            pane_with_session("%1", "sess-a"),
            pane_with_session("%2", "sess-b"),
        ]);
        state.sessions.names.insert("sess-a".into(), "alpha".into());
        state.sessions.names.insert("sess-b".into(), "beta".into());

        state.refresh_session_names();

        let names: Vec<&str> = state.repo_groups[0]
            .panes
            .iter()
            .map(|(p, _)| p.session_name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn refresh_session_names_leaves_label_empty_when_session_id_missing() {
        // Pane's session_id does not appear in the cached map (e.g. the
        // session JSON file was deleted). Starting from the empty slate
        // a fresh tmux parse produces, the label must stay empty so the
        // UI does not show a name for a session that is gone.
        let mut state = state_with_panes(vec![pane_with_session("%1", "sess-gone")]);
        // session_names is empty — no entry for sess-gone.

        state.refresh_session_names();

        assert!(
            state.repo_groups[0].panes[0].0.session_name.is_empty(),
            "session_name must stay empty when the cache has no entry"
        );
    }

    #[test]
    fn refresh_session_names_preserves_hook_provided_title() {
        // A non-empty session_name after the tmux parse is a
        // hook-provided title (`@pane_session_title`, e.g. opencode).
        // The `/rename` map must not overwrite or clear it, even when
        // the map has no entry for the pane's session.
        let mut state = state_with_panes(vec![pane_with_session("%1", "ses-opencode")]);
        state.repo_groups[0].panes[0].0.session_name = "Fix the login flow".into();
        // session_names is empty — the map only covers Claude.

        state.refresh_session_names();

        assert_eq!(
            state.repo_groups[0].panes[0].0.session_name, "Fix the login flow",
            "hook-provided titles must survive the map application"
        );
    }

    #[test]
    fn snapshot_rebuild_drops_labels_so_refresh_must_reapply_unconditionally() {
        // Regression for the `/rename` label reverting to the default
        // agent name one tick after it appeared.
        //
        // `apply_session_snapshot` rebuilds `repo_groups` from a fresh
        // tmux query, and a freshly parsed `PaneInfo` always carries an
        // empty `session_name` — tmux does not know about Claude's
        // session names. So `refresh` cannot gate `refresh_session_names`
        // on any change signal: the rebuild drops the label every tick,
        // and only an unconditional re-apply restores it. This pins the
        // precondition. If a future change makes the rebuild carry labels
        // over, a change-flag gate becomes safe again and this test is
        // the place that says so.
        let mut state = state_with_panes(vec![pane_with_session("%1", "sess-a")]);
        state.sessions.names.insert("sess-a".into(), "alpha".into());
        state.refresh_session_names();
        assert_eq!(state.repo_groups[0].panes[0].0.session_name, "alpha");

        let next_sessions = test_session(vec![pane_with_session("%1", "sess-a")]);
        state.apply_session_snapshot(next_sessions, Vec::new(), Vec::new());

        assert!(
            state.repo_groups[0].panes[0].0.session_name.is_empty(),
            "the rebuild drops the label, so a gated re-apply would lose it",
        );

        state.refresh_session_names();

        assert_eq!(
            state.repo_groups[0].panes[0].0.session_name, "alpha",
            "an unconditional re-apply restores the label the rebuild dropped",
        );
    }

    #[test]
    fn refresh_session_names_preserves_hook_title_for_pane_with_no_session_id() {
        // Hook-provided titles are self-contained: the pane's tmux
        // option was written by the same hook flow that owns the
        // session id, so a missing session_id must not strip the title.
        let mut state = state_with_panes(vec![test_pane("%1")]);
        state.repo_groups[0].panes[0].0.session_name = "stray".into();
        state.sessions.names.insert("sess-a".into(), "alpha".into());

        state.refresh_session_names();

        assert_eq!(
            state.repo_groups[0].panes[0].0.session_name, "stray",
            "hook-provided titles must not depend on the session_id map"
        );
    }

    // ─── refresh on tmux query failure ──────────────────────────────

    #[test]
    fn refresh_preserves_state_when_tmux_query_fails() {
        // Regression: a failed `list-panes` call used to be applied as an
        // empty snapshot, pruning all per-pane runtime state and letting
        // `rebuild_row_targets` reset the repo filter to All — a reset
        // that was then persisted in a tmux global option and survived
        // restart. A transient failure must hold the previous snapshot.
        let _tmux_down = crate::tmux::test_fail_tmux::install();
        let mut state = state_with_panes(vec![test_pane("%1")]);
        state.global.repo_filter = crate::state::RepoFilter::Repo("myrepo".into());
        state.pane_state_mut("%1").ports = vec![3000];

        let _ = state.refresh();

        assert_eq!(
            state.repo_groups.len(),
            1,
            "a failed query must not prune the previous snapshot's groups"
        );
        assert_eq!(state.repo_groups[0].panes[0].0.pane_id, "%1");
        assert_eq!(
            state.global.repo_filter,
            crate::state::RepoFilter::Repo("myrepo".into()),
            "a failed query must not reset the repo filter"
        );
        assert_eq!(
            state.pane_state("%1").map(|s| s.ports.clone()),
            Some(vec![3000]),
            "a failed query must not wipe per-pane runtime state"
        );
    }

    // ─── mark_focused_pane_seen ─────────────────────────────────────

    fn pane_with_attention(id: &str, attention: PaneAttention) -> PaneInfo {
        let mut p = test_pane(id);
        p.attention = attention;
        p
    }

    #[test]
    fn mark_focused_pane_seen_clears_done_flag_on_focus() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%FOCUSED_DONE";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "done");
        let mut state = state_with_panes(vec![pane_with_attention(pane_id, PaneAttention::Done)]);
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());

        state.mark_focused_pane_seen(true);

        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "focusing the pane must drop the done-unseen flag in tmux"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::None,
            "in-memory flag must clear so the same tick stops pulsing"
        );
    }

    /// A finish observed mid-snapshot must survive the min-flash grace
    /// when its pane already holds focus: the stale-surviving
    /// `pane_active` marker in the sidebar's window is not an
    /// observation of the user having read the result. Without this,
    /// hook's `@pane_attention=done` landed and was consumed within one
    /// tick — the row read idle with never a visible pulse (the agent
    /// analogue of the window-pane flash grace).
    #[test]
    fn done_flag_survives_focus_within_min_flash_grace() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%GRACE_DONE";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "done");
        let mut state = state_with_panes(vec![pane_with_attention(pane_id, PaneAttention::Done)]);
        state.now = 10_000;
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());

        // Simulate the snapshot stamp: the sidebar observed the flag now.
        state.pane_state_mut(pane_id).attention_since = Some(state.now);

        state.mark_focused_pane_seen(true);

        assert!(
            tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "a fresh done flag must survive focus for the flash grace"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::Done,
            "in-memory flag must clear so the same tick keeps pulsing"
        );
    }

    #[test]
    fn done_flag_consumed_after_grace_expires() {
        // Same setup, aged past ATTENTION_MIN_FLASH_SECS: focus consumes
        // the flag exactly like the pre-grace behavior.
        let _guard = tmux::test_mock::install();
        let pane_id = "%AGED_DONE";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "done");
        let mut state = state_with_panes(vec![pane_with_attention(pane_id, PaneAttention::Done)]);
        state.now = 10_000;
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());
        state.pane_state_mut(pane_id).attention_since =
            Some(state.now.saturating_sub(ATTENTION_MIN_FLASH_SECS + 1));

        state.mark_focused_pane_seen(true);

        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "after the grace, focusing the pane consumes the done flag"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::None
        );
    }

    #[test]
    fn update_attention_stamps_observes_new_done_flags_once() {
        // The stamp is taken on first observation and unset when the flag
        // clears, so a later turn's completion re-arms its own grace.
        let _guard = tmux::test_mock::install();
        let pane_id = "%STAMPED_DONE";
        let mut state = state_with_panes(vec![pane_with_attention(pane_id, PaneAttention::Done)]);
        state.now = 5_000;

        state.update_attention_stamps();
        assert_eq!(
            state.pane_state(pane_id).and_then(|s| s.attention_since),
            Some(5_000),
            "first observation stamps the flag"
        );

        state.now = 8_000;
        state.update_attention_stamps();
        assert_eq!(
            state.pane_state(pane_id).and_then(|s| s.attention_since),
            Some(5_000),
            "a still-set flag keeps its original stamp"
        );

        if let Some(pane) = state
            .repo_groups
            .iter_mut()
            .flat_map(|g| g.panes.iter_mut())
            .map(|(pane, _)| pane)
            .find(|pane| pane.pane_id == pane_id)
        {
            pane.attention = PaneAttention::None;
        }
        state.update_attention_stamps();
        assert_eq!(
            state.pane_state(pane_id).and_then(|s| s.attention_since),
            None,
            "a cleared flag resets the stamp so the next completion re-arms"
        );
    }

    #[test]
    fn update_attention_stamps_ignores_notification_flags() {
        // Notification (permission / question prompts) must clear on
        // focus immediately — the grace is scoped to the Done pulse.
        let _guard = tmux::test_mock::install();
        let pane_id = "%WAITING_UNSTAMPED";
        let mut state = state_with_panes(vec![pane_with_attention(
            pane_id,
            PaneAttention::Notification,
        )]);
        state.now = 5_000;

        state.update_attention_stamps();
        assert_eq!(
            state.pane_state(pane_id).and_then(|s| s.attention_since),
            None
        );
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());
        state.mark_focused_pane_seen(true);
        assert!(
            !tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "notification flag clears on focus with no grace"
        );
    }

    #[test]
    fn mark_focused_pane_seen_clears_notification_flag_on_focus() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%FOCUSED_WAITING";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        let mut state = state_with_panes(vec![pane_with_attention(
            pane_id,
            PaneAttention::Notification,
        )]);
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());

        state.mark_focused_pane_seen(true);

        assert!(!tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION));
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::None
        );
    }

    #[test]
    fn mark_focused_pane_seen_skips_when_sidebar_holds_focus() {
        let _guard = tmux::test_mock::install();
        let pane_id = "%SIDEBAR_FOCUSED";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "done");
        let mut state = state_with_panes(vec![pane_with_attention(pane_id, PaneAttention::Done)]);
        state.focus_state.sidebar_focused = true;
        state.focus_state.focused_pane_id = Some(pane_id.into());

        state.mark_focused_pane_seen(true);

        assert!(
            tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "reading the sidebar list is not reading the output — flag must survive"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::Done
        );
    }

    #[test]
    fn mark_focused_pane_seen_leaves_other_panes_flagged() {
        let _guard = tmux::test_mock::install();
        let flagged = "%UNSEEN_DONE";
        tmux::test_mock::set(flagged, tmux::PANE_ATTENTION, "done");
        let mut state = state_with_panes(vec![
            pane_with_attention(flagged, PaneAttention::Done),
            test_pane("%OTHER"),
        ]);
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some("%OTHER".into());

        state.mark_focused_pane_seen(true);

        assert!(
            tmux::test_mock::contains(flagged, tmux::PANE_ATTENTION),
            "focusing a different pane must not consume this pane's done flag"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::Done
        );
    }

    #[test]
    fn mark_focused_pane_seen_ignores_unflagged_focused_pane() {
        let _guard = tmux::test_mock::install();
        let mut state = state_with_panes(vec![test_pane("%CLEAN")]);
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some("%CLEAN".into());

        state.mark_focused_pane_seen(true);

        assert!(!tmux::test_mock::contains("%CLEAN", tmux::PANE_ATTENTION));
    }

    #[test]
    fn mark_focused_pane_seen_skips_when_sidebar_window_inactive() {
        // Regression: `find_active_pane` resolves the active pane of the
        // sidebar's own window, and that `pane_active` marker survives
        // after the user switches to another window. Without the
        // `window_active` gate, a permission-prompt flag on that leftover
        // pane was consumed within a second while the user was elsewhere.
        let _guard = tmux::test_mock::install();
        let pane_id = "%AWAY_WINDOW";
        tmux::test_mock::set(pane_id, tmux::PANE_ATTENTION, "notification");
        let mut state = state_with_panes(vec![pane_with_attention(
            pane_id,
            PaneAttention::Notification,
        )]);
        state.focus_state.sidebar_focused = false;
        state.focus_state.focused_pane_id = Some(pane_id.into());

        state.mark_focused_pane_seen(false);

        assert!(
            tmux::test_mock::contains(pane_id, tmux::PANE_ATTENTION),
            "the sidebar's window is not on screen — flag must survive"
        );
        assert_eq!(
            state.repo_groups[0].panes[0].0.attention,
            PaneAttention::Notification
        );
    }
}
