use super::AppState;

/// Focus target inside the spawn input popup. Tab / Shift+Tab / arrow
/// keys cycle through these in order; only `Task` accepts text input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnField {
    #[default]
    Task,
    Agent,
    Mode,
}

impl SpawnField {
    pub fn next(self) -> Self {
        match self {
            Self::Task => Self::Agent,
            Self::Agent => Self::Mode,
            Self::Mode => Self::Task,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Task => Self::Mode,
            Self::Agent => Self::Task,
            Self::Mode => Self::Agent,
        }
    }
}

/// At-most-one popup state for the sidebar. The enum variant encodes
/// both which popup is open and its per-popup data, so the "only one
/// popup open at a time" invariant is checked by the type system.
#[derive(Debug, Clone, Default)]
pub enum PopupState {
    #[default]
    None,
    Repo {
        selected: usize,
        area: Option<ratatui::layout::Rect>,
    },
    Notices {
        area: Option<ratatui::layout::Rect>,
    },
    /// Modal text input shown when the user presses `n` (or clicks `+`)
    /// to spawn a new worktree. `target_repo` / `target_repo_root` pin
    /// the spawn target; `agent_idx` / `mode_idx` index into
    /// [`crate::worktree::AGENTS`] / [`crate::worktree::modes_for`] so
    /// arrow keys can cycle the user's agent and permission-mode picks.
    SpawnInput {
        input: String,
        target_repo: String,
        target_repo_root: String,
        agent_idx: usize,
        mode_idx: usize,
        field: SpawnField,
        /// Screen Y of the repo header row that owns the `+` button
        /// this modal was opened from. Renderer anchors the popup just
        /// below it; `None` falls back to a centered layout.
        anchor_y: Option<u16>,
        /// Inline error message rendered at the bottom of the popup
        /// so spawn failures stay visually attached to the input the
        /// user was editing. Cleared on the next edit / field change.
        error: Option<String>,
        /// Set while the spawn runs on a worker thread: the popup stays
        /// visible with a one-line status and swallows all input until
        /// the result arrives on the popup-op channel. `Some` carries
        /// the status text; the error row is not shown while in flight.
        working: Option<&'static str>,
        area: Option<ratatui::layout::Rect>,
    },
    /// Keyboard reference shown when the user presses `?`. Static
    /// content — no per-popup data beyond the rendered rect.
    Help {
        area: Option<ratatui::layout::Rect>,
    },
    /// Confirmation prompt shown when the user presses `x` on a
    /// spawn-created pane. `pane_id` feeds `worktree::remove`; `branch`
    /// is shown in the modal title; `warning` lines spell out what the
    /// force-remove destroys (computed by a pre-flight git check when
    /// the dialog opens).
    RemoveConfirm {
        pane_id: String,
        branch: String,
        warning: Vec<String>,
        error: Option<String>,
        /// Set while the removal runs on a worker thread — same
        /// contract as [`PopupState::SpawnInput::working`].
        working: Option<&'static str>,
        area: Option<ratatui::layout::Rect>,
    },
}

/// Result of a popup-driven worktree operation, produced on a
/// per-operation worker thread and drained by the TUI event loop
/// (`app::run`), which hands it to [`AppState::apply_popup_op_result`].
#[derive(Debug)]
pub enum PopupOpResult {
    /// `worktree::spawn` finished: the new branch name or the error.
    Spawn(Result<String, String>),
    /// `worktree::remove` finished.
    Remove(Result<(), String>),
}

impl PopupState {
    pub fn set_repo_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::Repo { area, .. } = self {
            *area = rect;
        }
    }

    pub fn set_notices_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::Notices { area } = self {
            *area = rect;
        }
    }

    pub fn set_help_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::Help { area } = self {
            *area = rect;
        }
    }

    pub fn set_spawn_input_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::SpawnInput { area, .. } = self {
            *area = rect;
        }
    }

    pub fn set_remove_confirm_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::RemoveConfirm { area, .. } = self {
            *area = rect;
        }
    }
}

impl AppState {
    // ─── Repo popup ──────────────────────────────────────────────────────

    pub fn is_repo_popup_open(&self) -> bool {
        matches!(self.popup, PopupState::Repo { .. })
    }

    pub fn repo_popup_selected(&self) -> usize {
        match &self.popup {
            PopupState::Repo { selected, .. } => *selected,
            _ => 0,
        }
    }

    pub fn set_repo_popup_selected(&mut self, n: usize) {
        if let PopupState::Repo { selected, .. } = &mut self.popup {
            *selected = n;
        }
    }

    pub fn repo_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::Repo { area, .. } => *area,
            _ => None,
        }
    }

    pub fn toggle_repo_popup(&mut self) {
        if self.is_repo_popup_open() {
            self.close_repo_popup();
            return;
        }
        // Set selected to current filter position
        let names = self.repo_names();
        let selected = match &self.global.repo_filter {
            super::RepoFilter::All => 0,
            super::RepoFilter::Repo(name) => names.iter().position(|n| n == name).unwrap_or(0),
        };
        self.popup = PopupState::Repo {
            selected,
            area: None,
        };
    }

    pub fn confirm_repo_popup(&mut self) {
        let selected = self.repo_popup_selected();
        let names = self.repo_names();
        if let Some(name) = names.get(selected) {
            self.global.repo_filter = if selected == 0 {
                super::RepoFilter::All
            } else {
                super::RepoFilter::Repo(name.clone())
            };
        }
        self.popup = PopupState::None;
        self.global.save_repo_filter();
        self.rebuild_row_targets();
    }

    pub fn close_repo_popup(&mut self) {
        self.popup = PopupState::None;
    }

    // ─── Notices popup ───────────────────────────────────────────────────

    pub fn is_notices_popup_open(&self) -> bool {
        matches!(self.popup, PopupState::Notices { .. })
    }

    pub fn notices_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::Notices { area } => *area,
            _ => None,
        }
    }

    pub fn toggle_notices_popup(&mut self) {
        if self.is_notices_popup_open() {
            self.close_notices_popup();
        } else {
            self.popup = PopupState::Notices { area: None };
        }
    }

    pub fn close_notices_popup(&mut self) {
        self.popup = PopupState::None;
        self.notices.copy_targets.clear();
        self.notices.copied_at = None;
    }

    // ─── Help popup (? key) ────────────────────────────────────────────

    pub fn is_help_open(&self) -> bool {
        matches!(self.popup, PopupState::Help { .. })
    }

    pub fn help_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::Help { area } => *area,
            _ => None,
        }
    }

    pub fn toggle_help_popup(&mut self) {
        if self.is_help_open() {
            self.close_help_popup();
        } else {
            self.popup = PopupState::Help { area: None };
        }
    }

    pub fn close_help_popup(&mut self) {
        if self.is_help_open() {
            self.popup = PopupState::None;
        }
    }

    // ─── Spawn input popup (n key / + click) ─────────────────────────────

    pub fn is_spawn_input_open(&self) -> bool {
        matches!(self.popup, PopupState::SpawnInput { .. })
    }

    pub fn spawn_input_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::SpawnInput { area, .. } => *area,
            _ => None,
        }
    }

    pub fn open_spawn_input_for_repo(
        &mut self,
        repo_name: String,
        repo_root: String,
        anchor_y: Option<u16>,
    ) {
        self.popup = PopupState::SpawnInput {
            input: String::new(),
            target_repo: repo_name,
            target_repo_root: repo_root,
            agent_idx: 0,
            mode_idx: 0,
            field: SpawnField::Task,
            anchor_y,
            error: None,
            working: None,
            area: None,
        };
    }

    pub fn open_spawn_input_from_selection(&mut self) {
        let Some(pane) = self.selected_pane() else {
            self.set_flash("spawn: no pane selected");
            return;
        };
        let pane_id = pane.pane_id.clone();
        let Some(group) = self
            .repo_groups
            .iter()
            .find(|g| g.panes.iter().any(|(p, _)| p.pane_id == pane_id))
        else {
            self.set_flash("spawn: could not find repo group for selection");
            return;
        };
        let Some(root) = group
            .panes
            .iter()
            .find_map(|(_, git)| git.repo_root.clone())
        else {
            self.set_flash("spawn: selected pane is not in a git repo");
            return;
        };
        let name = group.name.clone();
        // Anchor the popup directly below the repo header row so it
        // matches what the mouse `+` click flow does.
        let anchor = self
            .layout
            .repo_spawn_targets
            .iter()
            .find(|t| t.repo_name == name)
            .map(|t| t.rect.y);
        self.open_spawn_input_for_repo(name, root, anchor);
    }

    pub fn close_spawn_input(&mut self) {
        if matches!(self.popup, PopupState::SpawnInput { .. }) {
            self.popup = PopupState::None;
        }
    }

    pub fn spawn_input_next_field(&mut self) {
        if let PopupState::SpawnInput { field, error, .. } = &mut self.popup {
            *field = field.next();
            *error = None;
        }
    }

    pub fn spawn_input_prev_field(&mut self) {
        if let PopupState::SpawnInput { field, error, .. } = &mut self.popup {
            *field = field.prev();
            *error = None;
        }
    }

    /// Cycle the value under the focused agent or mode field. No-op on
    /// the task input field so typing isn't interfered with.
    pub fn spawn_input_cycle(&mut self, delta: isize) {
        let PopupState::SpawnInput {
            field,
            agent_idx,
            mode_idx,
            error,
            ..
        } = &mut self.popup
        else {
            return;
        };
        match *field {
            SpawnField::Agent => {
                let len = crate::worktree::AGENTS.len() as isize;
                *agent_idx = ((*agent_idx as isize + delta).rem_euclid(len)) as usize;
                // Mode list is agent-specific.
                *mode_idx = 0;
                *error = None;
            }
            SpawnField::Mode => {
                let agent = crate::worktree::AGENTS
                    .get(*agent_idx)
                    .copied()
                    .unwrap_or("");
                let len = crate::worktree::modes_for(agent).len() as isize;
                if len > 0 {
                    *mode_idx = ((*mode_idx as isize + delta).rem_euclid(len)) as usize;
                    *error = None;
                }
            }
            SpawnField::Task => {}
        }
    }

    pub fn spawn_input_push_char(&mut self, c: char) {
        if let PopupState::SpawnInput {
            input,
            field,
            error,
            ..
        } = &mut self.popup
            && *field == SpawnField::Task
        {
            input.push(c);
            *error = None;
        }
    }

    pub fn spawn_input_pop_char(&mut self) {
        if let PopupState::SpawnInput {
            input,
            field,
            error,
            ..
        } = &mut self.popup
            && *field == SpawnField::Task
        {
            input.pop();
            *error = None;
        }
    }

    fn set_spawn_error(&mut self, msg: impl Into<String>) {
        if let PopupState::SpawnInput { error, .. } = &mut self.popup {
            *error = Some(msg.into());
        }
    }

    /// Whether the spawn popup is waiting for its worker thread.
    pub fn is_spawn_input_working(&self) -> bool {
        matches!(
            self.popup,
            PopupState::SpawnInput {
                working: Some(_),
                ..
            }
        )
    }

    /// Whether the remove-confirm popup is waiting for its worker thread.
    pub fn is_remove_confirm_working(&self) -> bool {
        matches!(
            self.popup,
            PopupState::RemoveConfirm {
                working: Some(_),
                ..
            }
        )
    }

    /// Hand a worktree operation to a fresh worker thread and park the
    /// popup in its working state. The event loop drains the result and
    /// calls [`AppState::apply_popup_op_result`]; repeated confirm
    /// presses are ignored while `working` is set, so an op can never
    /// be submitted twice. Without a channel (unit-test builds) the
    /// call is a no-op so no real git/tmux side effects can fire.
    fn start_popup_op(
        &mut self,
        working: &'static str,
        op: impl FnOnce() -> PopupOpResult + Send + 'static,
    ) {
        let Some(tx) = self.popup_op_tx.clone() else {
            return;
        };
        match &mut self.popup {
            PopupState::SpawnInput {
                working: state,
                error,
                ..
            }
            | PopupState::RemoveConfirm {
                working: state,
                error,
                ..
            } => {
                *state = Some(working);
                *error = None;
            }
            _ => return,
        }
        std::thread::spawn(move || {
            let result = op();
            let _ = tx.send(result);
        });
    }

    /// Apply a finished popup operation delivered by its worker thread.
    /// Success closes the popup exactly as the synchronous path did;
    /// failure returns the popup to its editable state with the error
    /// surfaced inline. No-ops if the popup moved on meanwhile.
    pub fn apply_popup_op_result(&mut self, result: PopupOpResult) {
        match result {
            PopupOpResult::Spawn(Ok(_)) => {
                if matches!(self.popup, PopupState::SpawnInput { .. }) {
                    self.popup = PopupState::None;
                }
            }
            PopupOpResult::Remove(Ok(())) => {
                if matches!(self.popup, PopupState::RemoveConfirm { .. }) {
                    self.popup = PopupState::None;
                }
            }
            PopupOpResult::Spawn(Err(e)) => {
                if let PopupState::SpawnInput { working, error, .. } = &mut self.popup {
                    *working = None;
                    *error = Some(e);
                }
            }
            PopupOpResult::Remove(Err(e)) => {
                if let PopupState::RemoveConfirm { working, error, .. } = &mut self.popup {
                    *working = None;
                    *error = Some(e);
                }
            }
        }
    }

    /// Validate the spawn input and hand the spawn flow to a worker
    /// thread (it can spend minutes in git under a hung/stale repo —
    /// see `pick_unique_slug` — which used to freeze the render loop).
    /// On success the popup closes when the result arrives; on failure
    /// the error is surfaced inside the popup and the modal stays open
    /// so the user can retry.
    pub fn confirm_spawn_input(&mut self) {
        let PopupState::SpawnInput {
            input,
            target_repo_root,
            agent_idx,
            mode_idx,
            working,
            ..
        } = &self.popup
        else {
            return;
        };
        // A second Enter while an op is in flight must not spawn twice.
        if working.is_some() {
            return;
        }
        let task_name = input.trim().to_string();
        if task_name.is_empty() {
            self.set_spawn_error("name is empty");
            return;
        }
        let agent = crate::worktree::AGENTS
            .get(*agent_idx)
            .copied()
            .unwrap_or(crate::worktree::DEFAULT_AGENT)
            .to_string();
        let mode = crate::worktree::modes_for(&agent)
            .get(*mode_idx)
            .copied()
            .unwrap_or(crate::worktree::DEFAULT_MODE)
            .to_string();
        let repo_root = std::path::PathBuf::from(target_repo_root.clone());

        let Some(session) = crate::tmux::pane_session_name(&self.tmux_pane) else {
            self.set_spawn_error("could not resolve tmux session");
            return;
        };

        let req = crate::worktree::SpawnRequest {
            repo_root,
            task_name,
            session,
            agent,
            mode,
        };
        self.start_popup_op("creating worktree…", move || {
            PopupOpResult::Spawn(crate::worktree::spawn(&req))
        });
    }

    // ─── Remove confirm popup (x key) ────────────────────────────────────

    pub fn is_remove_confirm_open(&self) -> bool {
        matches!(self.popup, PopupState::RemoveConfirm { .. })
    }

    pub fn remove_confirm_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::RemoveConfirm { area, .. } => *area,
            _ => None,
        }
    }

    pub fn close_remove_confirm(&mut self) {
        if matches!(self.popup, PopupState::RemoveConfirm { .. }) {
            self.popup = PopupState::None;
        }
    }

    /// Open the remove confirmation popup for the currently selected pane,
    /// but only if it was created by the sidebar's spawn flow. Otherwise
    /// flashes an error so the user knows nothing happened.
    pub fn open_remove_confirm(&mut self) {
        let Some(pane) = self.selected_pane() else {
            self.set_flash("remove: no pane selected");
            return;
        };
        self.open_remove_confirm_for_pane(pane.pane_id.clone());
    }

    pub fn open_remove_confirm_for_pane(&mut self, pane_id: String) {
        let markers = crate::worktree::read_spawn_markers(&pane_id);
        if !markers.is_spawned() {
            self.set_flash("remove: selected pane was not spawned by sidebar");
            return;
        }
        let branch = std::path::Path::new(&markers.worktree_path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.popup = PopupState::RemoveConfirm {
            pane_id,
            branch,
            warning: crate::worktree::removal_warnings(&markers.worktree_path),
            error: None,
            working: None,
            area: None,
        };
    }

    /// Run the remove flow for the pane stored in the confirmation
    /// popup, on a worker thread (same reasoning as the spawn path —
    /// the git steps can hang the render loop). Success closes the
    /// popup when the result arrives; failures are surfaced inside the
    /// popup so the user can retry.
    pub fn confirm_remove(&mut self, mode: crate::worktree::RemoveMode) {
        let pane_id = match &self.popup {
            PopupState::RemoveConfirm {
                pane_id, working, ..
            } => {
                // A second confirm while an op is in flight must not
                // remove twice.
                if working.is_some() {
                    return;
                }
                pane_id.clone()
            }
            _ => return,
        };
        let status = match mode {
            crate::worktree::RemoveMode::WindowOnly => "closing window…",
            crate::worktree::RemoveMode::WindowAndWorktree => "removing worktree…",
        };
        self.start_popup_op(status, move || {
            PopupOpResult::Remove(crate::worktree::remove(&pane_id, mode))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NoticesCopyTarget, RepoFilter};
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::tmux::{
        AgentType, PaneAttention, PaneInfo, PaneStatus, PermissionMode, WorktreeMetadata,
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
            tmux_session: String::new(),
            window_index: 0,
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    // ─── SpawnField cycle ────────────────────────────────────────────

    #[test]
    fn spawn_field_next_and_prev_cycle() {
        assert_eq!(SpawnField::Task.next(), SpawnField::Agent);
        assert_eq!(SpawnField::Agent.next(), SpawnField::Mode);
        assert_eq!(SpawnField::Mode.next(), SpawnField::Task);
        assert_eq!(SpawnField::Task.prev(), SpawnField::Mode);
        assert_eq!(SpawnField::Agent.prev(), SpawnField::Task);
        assert_eq!(SpawnField::Mode.prev(), SpawnField::Agent);
    }

    // ─── PopupState::set_*_area ──────────────────────────────────────

    #[test]
    fn set_area_updates_only_matching_variant() {
        let mut popup = PopupState::Repo {
            selected: 0,
            area: None,
        };
        let rect = ratatui::layout::Rect::new(1, 2, 3, 4);
        popup.set_repo_area(Some(rect));
        popup.set_notices_area(Some(rect));
        popup.set_spawn_input_area(Some(rect));
        popup.set_remove_confirm_area(Some(rect));
        match popup {
            PopupState::Repo { area, .. } => assert_eq!(area, Some(rect)),
            _ => panic!("variant must remain Repo"),
        }
    }

    // ─── Repo popup ──────────────────────────────────────────────────

    #[test]
    fn toggle_repo_popup_sets_selected_to_current() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![],
            },
        ];

        state.toggle_repo_popup();
        assert!(state.is_repo_popup_open());
        assert_eq!(state.repo_popup_selected(), 0);

        state.close_repo_popup();
        state.global.repo_filter = RepoFilter::Repo("beta".into());
        state.toggle_repo_popup();
        assert_eq!(state.repo_popup_selected(), 2);
    }

    #[test]
    fn confirm_repo_popup_sets_filter() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![(test_pane("%1"), PaneGitInfo::default())],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![(test_pane("%2"), PaneGitInfo::default())],
            },
        ];
        state.popup = PopupState::Repo {
            selected: 2,
            area: None,
        };
        state.confirm_repo_popup();

        assert_eq!(state.global.repo_filter, RepoFilter::Repo("beta".into()));
        assert!(!state.is_repo_popup_open());
        assert_eq!(state.layout.pane_row_targets.len(), 1);
        assert_eq!(state.layout.pane_row_targets[0].pane_id, "%2");
    }

    #[test]
    fn confirm_repo_popup_all_resets_filter() {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: true,
            panes: vec![(test_pane("%1"), PaneGitInfo::default())],
        }];
        state.global.repo_filter = RepoFilter::Repo("app".into());
        state.popup = PopupState::Repo {
            selected: 0,
            area: None,
        };
        state.confirm_repo_popup();

        assert_eq!(state.global.repo_filter, RepoFilter::All);
    }

    #[test]
    fn close_repo_popup_resets_popup_state() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Repo {
            selected: 5,
            area: Some(ratatui::layout::Rect::new(0, 0, 10, 5)),
        };
        assert!(state.is_repo_popup_open());
        state.close_repo_popup();
        assert!(matches!(state.popup, PopupState::None));
        assert!(state.repo_popup_area().is_none());
    }

    #[test]
    fn toggle_repo_popup_twice_closes() {
        let mut state = AppState::new("%99".into());
        state.toggle_repo_popup();
        assert!(state.is_repo_popup_open());
        state.toggle_repo_popup();
        assert!(!state.is_repo_popup_open());
    }

    #[test]
    fn set_repo_popup_selected_noop_when_other_variant() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.set_repo_popup_selected(9);
        // Repo accessor returns default when not in Repo variant.
        assert_eq!(state.repo_popup_selected(), 0);
    }

    // ─── Notices popup ───────────────────────────────────────────────

    #[test]
    fn toggle_notices_popup_opens_then_closes() {
        let mut state = AppState::new("%99".into());
        state.toggle_notices_popup();
        assert!(state.is_notices_popup_open());
        state.toggle_notices_popup();
        assert!(!state.is_notices_popup_open());
    }

    #[test]
    fn close_notices_popup_clears_copy_state() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.notices.copy_targets = vec![NoticesCopyTarget {
            area: ratatui::layout::Rect::new(0, 0, 5, 1),
            agent: "claude".into(),
        }];
        state.notices.copied_at = Some(("claude".into(), std::time::Instant::now()));

        state.close_notices_popup();

        assert!(!state.is_notices_popup_open());
        assert!(state.notices.copy_targets.is_empty());
        assert!(state.notices.copied_at.is_none());
    }

    #[test]
    fn notices_popup_area_none_when_closed() {
        let state = AppState::new("%99".into());
        assert!(state.notices_popup_area().is_none());
    }

    // ─── Help popup ────────────────────────────────────────────────

    #[test]
    fn toggle_help_popup_opens_then_closes() {
        let mut state = AppState::new("%99".into());
        state.toggle_help_popup();
        assert!(state.is_help_open());
        state.toggle_help_popup();
        assert!(!state.is_help_open());
    }

    #[test]
    fn close_help_popup_only_closes_the_help_popup() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.close_help_popup();
        assert!(
            matches!(state.popup, PopupState::Notices { .. }),
            "a different popup must not be touched"
        );

        state.popup = PopupState::Help { area: None };
        state.close_help_popup();
        assert!(matches!(state.popup, PopupState::None));
    }

    // ─── Spawn input popup ───────────────────────────────────────────

    #[test]
    fn open_spawn_input_for_repo_initializes_fields() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), Some(7));
        assert!(state.is_spawn_input_open());
        if let PopupState::SpawnInput {
            input,
            target_repo,
            target_repo_root,
            agent_idx,
            mode_idx,
            field,
            anchor_y,
            error,
            working,
            area,
        } = &state.popup
        {
            assert!(input.is_empty());
            assert_eq!(target_repo, "alpha");
            assert_eq!(target_repo_root, "/tmp/alpha");
            assert_eq!(*agent_idx, 0);
            assert_eq!(*mode_idx, 0);
            assert_eq!(*field, SpawnField::Task);
            assert_eq!(*anchor_y, Some(7));
            assert!(error.is_none());
            assert!(working.is_none());
            assert!(area.is_none());
        } else {
            panic!("expected SpawnInput, got {:?}", state.popup);
        }
    }

    #[test]
    fn close_spawn_input_noop_when_not_open() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.close_spawn_input();
        // The other popup must not be touched.
        assert!(matches!(state.popup, PopupState::Notices { .. }));
    }

    #[test]
    fn spawn_input_next_prev_cycle_field() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        state.spawn_input_next_field();
        assert!(matches!(
            state.popup,
            PopupState::SpawnInput {
                field: SpawnField::Agent,
                ..
            }
        ));
        state.spawn_input_next_field();
        assert!(matches!(
            state.popup,
            PopupState::SpawnInput {
                field: SpawnField::Mode,
                ..
            }
        ));
        state.spawn_input_prev_field();
        assert!(matches!(
            state.popup,
            PopupState::SpawnInput {
                field: SpawnField::Agent,
                ..
            }
        ));
    }

    #[test]
    fn spawn_input_field_change_clears_error() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        if let PopupState::SpawnInput { error, .. } = &mut state.popup {
            *error = Some("boom".into());
        }
        state.spawn_input_next_field();
        if let PopupState::SpawnInput { error, .. } = &state.popup {
            assert!(error.is_none());
        }
    }

    #[test]
    fn spawn_input_cycle_agent_wraps_and_resets_mode() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Switch field to Agent, bump mode_idx artificially, then cycle to
        // verify mode_idx resets to 0 on agent change.
        if let PopupState::SpawnInput {
            field, mode_idx, ..
        } = &mut state.popup
        {
            *field = SpawnField::Agent;
            *mode_idx = 1;
        }
        state.spawn_input_cycle(1);
        if let PopupState::SpawnInput {
            agent_idx,
            mode_idx,
            ..
        } = &state.popup
        {
            assert_eq!(*agent_idx, 1 % crate::worktree::AGENTS.len());
            assert_eq!(*mode_idx, 0);
        }
    }

    #[test]
    fn spawn_input_cycle_task_field_is_noop() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        state.spawn_input_cycle(1);
        if let PopupState::SpawnInput {
            agent_idx,
            mode_idx,
            ..
        } = &state.popup
        {
            assert_eq!(*agent_idx, 0);
            assert_eq!(*mode_idx, 0);
        }
    }

    #[test]
    fn spawn_input_push_pop_char_only_on_task_field() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);

        state.spawn_input_push_char('h');
        state.spawn_input_push_char('i');
        if let PopupState::SpawnInput { input, .. } = &state.popup {
            assert_eq!(input, "hi");
        }

        // Switch field — push/pop must have no effect on input text.
        state.spawn_input_next_field();
        state.spawn_input_push_char('x');
        state.spawn_input_pop_char();
        if let PopupState::SpawnInput { input, .. } = &state.popup {
            assert_eq!(input, "hi");
        }
    }

    /// Read the inline popup error for assertions.
    fn popup_error(state: &AppState) -> Option<String> {
        match &state.popup {
            PopupState::SpawnInput { error, .. } | PopupState::RemoveConfirm { error, .. } => {
                error.clone()
            }
            _ => None,
        }
    }

    #[test]
    fn confirm_spawn_input_empty_sets_error() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        state.confirm_spawn_input();
        if let PopupState::SpawnInput { error, .. } = &state.popup {
            assert_eq!(error.as_deref(), Some("name is empty"));
        } else {
            panic!("popup must stay open on error");
        }
    }

    #[test]
    fn confirm_spawn_input_ignores_confirm_while_working() {
        // The no-double-spawn guard must run before any dispatch, so a
        // second Enter while an op is in flight is a synchronous no-op.
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        if let PopupState::SpawnInput { working, .. } = &mut state.popup {
            *working = Some("creating worktree…");
        }
        state.confirm_spawn_input();
        assert!(
            state.is_spawn_input_working(),
            "guard must keep the popup in flight"
        );
    }

    #[test]
    fn start_popup_op_without_channel_is_a_noop() {
        // Test builds have no channel wired, so dispatch must leave the
        // popup untouched rather than flip it into an unreachable
        // working state (and definitely not run the operation).
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "agent/x".into(),
            warning: Vec::new(),
            error: None,
            working: None,
            area: None,
        };
        state.start_popup_op("removing worktree…", || PopupOpResult::Remove(Ok(())));
        assert!(!state.is_remove_confirm_working());
        assert!(state.is_remove_confirm_open());
    }

    #[test]
    fn confirm_remove_dispatches_once_and_applies_result() {
        // Real channel: the worker runs `worktree::remove` against a
        // pane with no spawn markers, which fails fast without touching
        // git or tmux — enough to exercise the async round trip.
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = AppState::new("%99".into());
        state.popup_op_tx = Some(tx);
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "agent/x".into(),
            warning: Vec::new(),
            error: None,
            working: None,
            area: None,
        };

        state.confirm_remove(crate::worktree::RemoveMode::WindowOnly);
        assert!(
            state.is_remove_confirm_working(),
            "popup must park in working state"
        );
        assert!(
            popup_error(&state).is_none(),
            "starting an op must clear a stale inline error"
        );

        // A second confirm while in flight must not queue a second op.
        state.confirm_remove(crate::worktree::RemoveMode::WindowAndWorktree);

        let mut results = Vec::new();
        while let Ok(result) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            results.push(result);
        }
        assert_eq!(results.len(), 1, "exactly one operation must be dispatched");

        match results.remove(0) {
            PopupOpResult::Remove(Err(_)) => {}
            other => panic!("expected a failed remove for an unknown pane, got {other:?}"),
        }

        // ... but the failing result was never applied to the second
        // confirm's state; apply it and check the popup returns to an
        // editable state with the error inline.
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "agent/x".into(),
            warning: Vec::new(),
            error: None,
            working: Some("closing window…"),
            area: None,
        };
        state.apply_popup_op_result(PopupOpResult::Remove(Err(
            "pane was not created by sidebar spawn".into(),
        )));
        assert!(!state.is_remove_confirm_working());
        assert_eq!(
            popup_error(&state).as_deref(),
            Some("pane was not created by sidebar spawn")
        );
    }

    #[test]
    fn apply_popup_op_result_success_closes_popup() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::SpawnInput {
            input: "task".into(),
            target_repo: "alpha".into(),
            target_repo_root: "/tmp/alpha".into(),
            agent_idx: 0,
            mode_idx: 0,
            field: SpawnField::Task,
            anchor_y: None,
            error: None,
            working: Some("creating worktree…"),
            area: None,
        };
        state.apply_popup_op_result(PopupOpResult::Spawn(Ok("agent/task".into())));
        assert!(!state.is_spawn_input_open(), "success must close the popup");

        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "agent/x".into(),
            warning: Vec::new(),
            error: None,
            working: Some("removing worktree…"),
            area: None,
        };
        state.apply_popup_op_result(PopupOpResult::Remove(Ok(())));
        assert!(
            !state.is_remove_confirm_open(),
            "success must close the popup"
        );
    }

    #[test]
    fn apply_popup_op_result_ignores_moved_on_popup() {
        // If the result lands after the popup moved on (not reachable
        // through input today — keys are swallowed while working — the
        // application must not resurrect or mutate the wrong popup).
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.apply_popup_op_result(PopupOpResult::Spawn(Err("boom".into())));
        assert!(matches!(state.popup, PopupState::Notices { .. }));
        state.apply_popup_op_result(PopupOpResult::Spawn(Ok("agent/task".into())));
        assert!(matches!(state.popup, PopupState::Notices { .. }));
    }

    // ─── Remove confirm popup ────────────────────────────────────────

    #[test]
    fn remove_confirm_accessors_and_close() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            branch: "feature/x".into(),
            warning: Vec::new(),
            error: None,
            working: None,
            area: Some(ratatui::layout::Rect::new(0, 0, 20, 5)),
        };
        assert!(state.is_remove_confirm_open());
        assert_eq!(
            state.remove_confirm_popup_area(),
            Some(ratatui::layout::Rect::new(0, 0, 20, 5))
        );
        state.close_remove_confirm();
        assert!(!state.is_remove_confirm_open());
        assert!(state.remove_confirm_popup_area().is_none());
    }

    #[test]
    fn confirm_remove_noop_when_popup_absent() {
        // When no RemoveConfirm popup is open, `confirm_remove` must early-return
        // without touching state. We verify nothing blew up and the popup
        // variant is preserved.
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::Notices { area: None };
        state.confirm_remove(crate::worktree::RemoveMode::WindowOnly);
        assert!(matches!(state.popup, PopupState::Notices { .. }));
    }

    #[test]
    fn open_remove_confirm_without_selection_flashes() {
        let mut state = AppState::new("%99".into());
        state.open_remove_confirm();
        assert!(state.take_flash().is_some());
        assert!(!state.is_remove_confirm_open());
    }
}
