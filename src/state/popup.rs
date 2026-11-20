use super::AppState;

/// Placeholder warning lines shown in the remove-confirm dialog for
/// worktree spawns while its pre-flight git probes run on a worker
/// thread (see [`AppState::open_remove_confirm_for_pane`]).
const REMOVE_CHECKING_WARNING: [&str; 2] = ["Checking worktree…", ""];

/// Static warning lines for folder-spawned panes: there is no
/// worktree or branch, so removal only closes the tmux window.
const REMOVE_FOLDER_WARNING: [&str; 2] = ["Closes the window;", "no worktree or branch"];

/// Focus target inside the spawn input popup. Tab / Shift+Tab / arrow
/// keys cycle through these in order; only `Name` accepts text input.
/// The order matches the field order the user reads top to bottom:
/// MODE → AGENT → NAME. `Name` is skipped entirely while the spawn
/// kind is `default` (a folder spawn needs no name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpawnField {
    #[default]
    Mode,
    Agent,
    Name,
}

impl SpawnField {
    /// Next field in Tab order. `has_name` is false for folder spawns,
    /// where the NAME row is not rendered and must not be focusable.
    pub fn next(self, has_name: bool) -> Self {
        match self {
            Self::Mode => Self::Agent,
            Self::Agent if has_name => Self::Name,
            Self::Agent => Self::Mode,
            Self::Name => Self::Mode,
        }
    }

    /// Previous field in Shift+Tab order (inverse of [`Self::next`]).
    pub fn prev(self, has_name: bool) -> Self {
        match self {
            Self::Mode if has_name => Self::Name,
            Self::Mode => Self::Agent,
            Self::Agent => Self::Mode,
            Self::Name => Self::Agent,
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
        /// First inner row of the selectable repo list. Rows above it
        /// are the notices section (click-to-copy, not selectable).
        /// Written back by the renderer each frame; `0` keeps the old
        /// list-only mapping when the renderer has not run yet.
        repo_start: u16,
    },
    /// Modal text input shown when the user presses `n` (or clicks `+`)
    /// to spawn a new agent. `target_repo` / `target_repo_root` pin
    /// the spawn target; `kind_idx` indexes into
    /// [`crate::worktree::SPAWN_MODES`] (default = spawn in the repo
    /// folder, worktree = create a worktree first) and `agent_idx`
    /// indexes into [`crate::worktree::AGENTS`] so arrow keys can cycle
    /// the user's picks. The NAME field only exists for worktree spawns.
    SpawnInput {
        input: String,
        target_repo: String,
        target_repo_root: String,
        kind_idx: usize,
        agent_idx: usize,
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
    /// Keyboard reference shown when the user presses `?`. The renderer
    /// writes back `content` / `viewport` each frame (both depend on the
    /// sidebar's size), which `scroll_help` clamps against.
    Help {
        area: Option<ratatui::layout::Rect>,
        /// First content line shown at the top of the overlay.
        scroll: u16,
        /// Total wrapped content lines, as of the last render.
        content: u16,
        /// Visible inner height, as of the last render.
        viewport: u16,
    },
    /// Confirmation prompt shown when the user presses `x` on a
    /// spawn-created pane. `pane_id` feeds `worktree::remove`; `label`
    /// is shown in the modal title (the worktree directory name for
    /// worktree spawns, the repo name for folder spawns); `warning`
    /// lines spell out what the force-remove destroys (computed by a
    /// pre-flight git check on a worker thread after the dialog opens —
    /// the dialog shows a checking placeholder until it lands). Folder
    /// spawns (`has_worktree: false`) have no git state, so the dialog
    /// offers only a window close and the warning lines are static.
    RemoveConfirm {
        pane_id: String,
        label: String,
        has_worktree: bool,
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
    /// A resume launch (`r` on a resume-shaped todo) finished: the
    /// flash banner text on success, the failure text on error.
    Resume(Result<String, String>),
    /// The remove-confirm dialog's pre-flight git warnings finished:
    /// replaces the checking placeholder on the matching dialog. The
    /// two `git status --porcelain` / `rev-list` probes used to run
    /// inline when the dialog opened, blocking the input thread for
    /// their full duration (audit 2026-11-16 J5).
    RemoveWarnings {
        pane_id: String,
        warnings: Vec<String>,
    },
}

impl PopupState {
    pub fn set_repo_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::Repo { area, .. } = self {
            *area = rect;
        }
    }

    pub fn set_repo_start(&mut self, start: u16) {
        if let Self::Repo { repo_start, .. } = self {
            *repo_start = start;
        }
    }

    pub fn set_help_area(&mut self, rect: Option<ratatui::layout::Rect>) {
        if let Self::Help { area, .. } = self {
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

    pub fn repo_popup_repo_start(&self) -> u16 {
        match &self.popup {
            PopupState::Repo { repo_start, .. } => *repo_start,
            _ => 0,
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
            repo_start: 0,
        };
        // Opening the menu claims the header row: a chyron mid-fade is
        // dismissed rather than left to fight the dropdown for row 0.
        self.chyron.current = None;
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

    // ─── Help popup (? key) ────────────────────────────────────────────

    pub fn is_help_open(&self) -> bool {
        matches!(self.popup, PopupState::Help { .. })
    }

    pub fn help_popup_area(&self) -> Option<ratatui::layout::Rect> {
        match &self.popup {
            PopupState::Help { area, .. } => *area,
            _ => None,
        }
    }

    /// The help overlay's current scroll offset (0 while closed).
    pub fn help_scroll(&self) -> u16 {
        match &self.popup {
            PopupState::Help { scroll, .. } => *scroll,
            _ => 0,
        }
    }

    pub fn toggle_help_popup(&mut self) {
        if self.is_help_open() {
            self.close_help_popup();
        } else {
            self.popup = PopupState::Help {
                area: None,
                scroll: 0,
                content: 0,
                viewport: 0,
            };
        }
    }

    /// Record the content/viewport heights the renderer just laid out,
    /// re-clamping scroll against the fresh layout (a resize can shrink
    /// the content).
    pub fn set_help_metrics(&mut self, content: u16, viewport: u16) {
        if let PopupState::Help {
            scroll,
            content: c,
            viewport: v,
            ..
        } = &mut self.popup
        {
            *c = content;
            *v = viewport;
            *scroll = (*scroll).min(content.saturating_sub(viewport));
        }
    }

    /// Scroll the help overlay by `delta` lines (negative up), clamped to
    /// the content the renderer last reported. A no-op while closed.
    pub fn scroll_help(&mut self, delta: isize) {
        if let PopupState::Help {
            scroll,
            content,
            viewport,
            ..
        } = &mut self.popup
        {
            let max = content.saturating_sub(*viewport) as isize;
            *scroll = ((*scroll as isize + delta).clamp(0, max)) as u16;
        }
    }

    /// Page the help overlay by `direction` (+1 / -1) viewport-sized
    /// jumps, leaving one line of overlap so position is never lost.
    pub fn scroll_help_page(&mut self, direction: isize) {
        let page = match &self.popup {
            PopupState::Help { viewport, .. } => viewport.saturating_sub(2).max(1) as isize,
            _ => return,
        };
        self.scroll_help(direction * page);
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
            kind_idx: 0,
            agent_idx: 0,
            field: SpawnField::Mode,
            anchor_y,
            error: None,
            working: None,
            area: None,
        };
    }

    pub fn open_spawn_input_from_selection(&mut self) {
        let Some(target) = self
            .layout
            .pane_row_targets
            .get(self.global.selected_pane_row)
        else {
            self.set_flash("spawn: no pane selected");
            return;
        };
        if target.is_window {
            let pane_id = target.pane_id.clone();
            self.open_spawn_input_from_window_row(&pane_id);
            return;
        }
        let pane_id = target.pane_id.clone();
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

    /// Spawn anchored on a window row: a non-agent pane (shell, nvim,
    /// dev server) that renders via `other_windows` rather than
    /// `repo_groups`. The row's git info pins the repo, and the display
    /// name comes from the repo group the row renders under — so `n`
    /// works wherever the cursor can sit, matching how Enter jumps to
    /// both row kinds.
    fn open_spawn_input_from_window_row(&mut self, pane_id: &str) {
        let Some((repo_key, git)) = self.other_windows.iter().find_map(|(repo_key, windows)| {
            windows
                .iter()
                .find(|w| w.pane_id == pane_id)
                .map(|w| (repo_key.clone(), w.git_info.clone()))
        }) else {
            self.set_flash("spawn: could not find repo group for selection");
            return;
        };
        let Some(root) = git.repo_root.clone() else {
            self.set_flash("spawn: selected pane is not in a git repo");
            return;
        };
        let name = self
            .repo_groups
            .iter()
            .find(|g| crate::group::repo_group_key(g) == repo_key)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| crate::group::path_basename(&repo_key).to_string());
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

    /// Whether the currently selected spawn kind renders the NAME
    /// field: only worktree spawns need a name (folder spawns launch
    /// the agent straight in the repo checkout).
    fn spawn_input_has_name(&self) -> bool {
        matches!(self.popup, PopupState::SpawnInput { kind_idx: 1, .. })
    }

    pub fn spawn_input_next_field(&mut self) {
        let has_name = self.spawn_input_has_name();
        if let PopupState::SpawnInput { field, error, .. } = &mut self.popup {
            *field = field.next(has_name);
            *error = None;
        }
    }

    pub fn spawn_input_prev_field(&mut self) {
        let has_name = self.spawn_input_has_name();
        if let PopupState::SpawnInput { field, error, .. } = &mut self.popup {
            *field = field.prev(has_name);
            *error = None;
        }
    }

    /// Cycle the value under the focused field. No-op on the name
    /// input field so typing isn't interfered with.
    pub fn spawn_input_cycle(&mut self, delta: isize) {
        let PopupState::SpawnInput {
            field,
            kind_idx,
            agent_idx,
            error,
            ..
        } = &mut self.popup
        else {
            return;
        };
        match *field {
            SpawnField::Mode => {
                let len = crate::worktree::SPAWN_MODES.len() as isize;
                *kind_idx = ((*kind_idx as isize + delta).rem_euclid(len)) as usize;
                *error = None;
            }
            SpawnField::Agent => {
                let len = crate::worktree::AGENTS.len() as isize;
                *agent_idx = ((*agent_idx as isize + delta).rem_euclid(len)) as usize;
                *error = None;
            }
            SpawnField::Name => {}
        }
    }

    pub fn spawn_input_push_char(&mut self, c: char) {
        if let PopupState::SpawnInput {
            input,
            field,
            error,
            ..
        } = &mut self.popup
            && *field == SpawnField::Name
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
            && *field == SpawnField::Name
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

    /// Hand a one-shot operation to a fresh worker thread with no popup
    /// to park — the resume launch, which reports through the flash
    /// banner instead of a modal. Same no-channel (unit-test build)
    /// no-op policy as [`Self::start_popup_op`]: without the channel
    /// nothing spawns, so tests never touch real tmux or agent stores.
    pub(super) fn start_background_op(
        &mut self,
        op: impl FnOnce() -> PopupOpResult + Send + 'static,
    ) {
        let Some(tx) = self.popup_op_tx.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let _ = tx.send(op());
        });
    }

    /// Apply a finished popup operation delivered by its worker thread.
    /// Success closes the popup exactly as the synchronous path did;
    /// failure returns the popup to its editable state with the error
    /// surfaced inline. No-ops if the popup moved on meanwhile. Resume
    /// results carry no popup: both outcomes land in the flash banner.
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
            PopupOpResult::RemoveWarnings { pane_id, warnings } => {
                // Only the dialog this pre-flight was opened for: a
                // late result must not overwrite a different pane's
                // dialog, and a closed popup drops it. A confirmed
                // removal before the warnings landed is safe — the
                // lines are informational, the removal is `--force`
                // either way.
                if let PopupState::RemoveConfirm {
                    pane_id: current,
                    warning,
                    ..
                } = &mut self.popup
                    && *current == pane_id
                {
                    *warning = warnings;
                }
            }
            PopupOpResult::Resume(result) => self.flash_resume_result(result),
        }
    }

    /// Validate the spawn input and hand the spawn flow to a worker
    /// thread (it can spend minutes in git under a hung/stale repo —
    /// see `pick_unique_slug` — which used to freeze the render loop).
    /// On success the popup closes when the result arrives; on failure
    /// the error is surfaced inside the popup and the modal stays open
    /// so the user can retry. Folder spawns skip the name check —
    /// the agent launches straight in the repo checkout.
    pub fn confirm_spawn_input(&mut self) {
        let PopupState::SpawnInput {
            input,
            target_repo_root,
            kind_idx,
            agent_idx,
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
        let kind = crate::worktree::SpawnKind::from_mode_index(*kind_idx);
        let agent = crate::worktree::AGENTS
            .get(*agent_idx)
            .copied()
            .unwrap_or(crate::worktree::DEFAULT_AGENT)
            .to_string();

        // Worktree spawns slugify the name into the branch and worktree
        // directory; folder spawns have neither, so an empty input is
        // fine (the window is named after the agent instead).
        let task_name = input.trim().to_string();
        if kind == crate::worktree::SpawnKind::Worktree && task_name.is_empty() {
            self.set_spawn_error("name is empty");
            return;
        }
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
            kind,
        };
        let working_label = match kind {
            crate::worktree::SpawnKind::Folder => "spawning agent…",
            crate::worktree::SpawnKind::Worktree => "creating worktree…",
        };
        self.start_popup_op(working_label, move || {
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
        // Folder spawns carry no worktree path: the dialog becomes a
        // plain window-close confirm (no git probes, no branch in the
        // title — the repo name identifies the pane instead).
        let has_worktree = !markers.worktree_path.is_empty();
        let label = if has_worktree {
            std::path::Path::new(&markers.worktree_path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            std::path::Path::new(&markers.from_repo)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let warning: Vec<String> = if has_worktree {
            REMOVE_CHECKING_WARNING
                .iter()
                .map(|l| l.to_string())
                .collect()
        } else {
            REMOVE_FOLDER_WARNING
                .iter()
                .map(|l| l.to_string())
                .collect()
        };
        self.popup = PopupState::RemoveConfirm {
            pane_id: pane_id.clone(),
            label,
            has_worktree,
            warning,
            error: None,
            working: None,
            area: None,
        };
        if !has_worktree {
            return;
        }
        // The dirty/unpushed probes are two git spawns under a possibly
        // hung repo; they used to run inline while the dialog opened,
        // blocking the input thread (audit 2026-11-16 J5). The dialog
        // opens on the checking placeholder and the worker replaces the
        // lines when they land (`apply_popup_op_result` matches the
        // pane_id, and a confirm before then is safe — the removal is
        // force regardless).
        let worktree_path = markers.worktree_path;
        self.start_background_op(move || PopupOpResult::RemoveWarnings {
            pane_id,
            warnings: crate::worktree::removal_warnings(&worktree_path),
        });
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
    use super::super::RepoFilter;
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
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    // ─── SpawnField cycle ────────────────────────────────────────────

    #[test]
    fn spawn_field_next_and_prev_cycle_with_name() {
        // Worktree spawns render the NAME field: full three-field cycle.
        assert_eq!(SpawnField::Mode.next(true), SpawnField::Agent);
        assert_eq!(SpawnField::Agent.next(true), SpawnField::Name);
        assert_eq!(SpawnField::Name.next(true), SpawnField::Mode);
        assert_eq!(SpawnField::Mode.prev(true), SpawnField::Name);
        assert_eq!(SpawnField::Agent.prev(true), SpawnField::Mode);
        assert_eq!(SpawnField::Name.prev(true), SpawnField::Agent);
    }

    #[test]
    fn spawn_field_cycle_skips_name_for_folder_spawns() {
        // Folder spawns have no NAME field: Agent wraps straight back
        // to Mode in both directions.
        assert_eq!(SpawnField::Mode.next(false), SpawnField::Agent);
        assert_eq!(SpawnField::Agent.next(false), SpawnField::Mode);
        assert_eq!(SpawnField::Mode.prev(false), SpawnField::Agent);
        assert_eq!(SpawnField::Agent.prev(false), SpawnField::Mode);
    }

    // ─── PopupState::set_*_area ──────────────────────────────────────

    #[test]
    fn set_area_updates_only_matching_variant() {
        let mut popup = PopupState::Repo {
            selected: 0,
            area: None,
            repo_start: 0,
        };
        let rect = ratatui::layout::Rect::new(1, 2, 3, 4);
        popup.set_repo_area(Some(rect));
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
            repo_start: 0,
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
            repo_start: 0,
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
            repo_start: 0,
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
        state.popup = PopupState::Help {
            area: None,
            scroll: 0,
            content: 0,
            viewport: 0,
        };
        state.set_repo_popup_selected(9);
        // Repo accessor returns default when not in the Repo variant.
        assert_eq!(state.repo_popup_selected(), 0);
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
        state.popup = PopupState::Repo {
            selected: 0,
            area: None,
            repo_start: 0,
        };
        state.close_help_popup();
        assert!(
            matches!(state.popup, PopupState::Repo { .. }),
            "a different popup must not be touched"
        );

        state.popup = PopupState::Help {
            area: None,
            scroll: 0,
            content: 0,
            viewport: 0,
        };
        state.close_help_popup();
        assert!(matches!(state.popup, PopupState::None));
    }

    #[test]
    fn help_scroll_clamps_to_the_last_reported_content() {
        let mut state = AppState::new("%99".into());
        state.toggle_help_popup();
        // The renderer has not reported anything yet: no room to scroll.
        state.scroll_help(5);
        assert_eq!(
            state.help_scroll(),
            0,
            "scroll stays at zero before the first render"
        );

        state.set_help_metrics(30, 10);
        state.scroll_help(100);
        assert_eq!(state.help_scroll(), 20, "clamped to content - viewport");
        state.scroll_help(-100);
        assert_eq!(state.help_scroll(), 0, "clamped at the top");
        state.scroll_help(3);
        assert_eq!(state.help_scroll(), 3);

        // A resize that shrinks the content: the next render writes new
        // metrics and scroll re-clamps from there.
        state.set_help_metrics(4, 10);
        state.scroll_help(0);
        assert_eq!(state.help_scroll(), 0);
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
            kind_idx,
            agent_idx,
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
            assert_eq!(*kind_idx, 0, "default spawn kind is folder");
            assert_eq!(*agent_idx, 0);
            assert_eq!(*field, SpawnField::Mode, "focus starts on the MODE field");
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
        state.popup = PopupState::Repo {
            selected: 0,
            area: None,
            repo_start: 0,
        };
        state.close_spawn_input();
        // The other popup must not be touched.
        assert!(matches!(state.popup, PopupState::Repo { .. }));
    }

    #[test]
    fn spawn_input_next_prev_cycle_field() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Default kind is folder (no NAME field): two-field cycle.
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
    fn spawn_input_worktree_kind_unfocuses_name_when_switching_back() {
        // Selecting the worktree kind adds the NAME field; cycling back
        // to the folder kind must make it unreachable again.
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        if let PopupState::SpawnInput { kind_idx, .. } = &mut state.popup {
            *kind_idx = 1; // worktree
        }
        state.spawn_input_next_field(); // Agent
        state.spawn_input_next_field(); // Name
        assert!(matches!(
            state.popup,
            PopupState::SpawnInput {
                field: SpawnField::Name,
                ..
            }
        ));
        if let PopupState::SpawnInput { kind_idx, .. } = &mut state.popup {
            *kind_idx = 0; // folder
        }
        state.spawn_input_next_field(); // from Name: next skips to Mode
        assert!(matches!(
            state.popup,
            PopupState::SpawnInput {
                field: SpawnField::Mode,
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
    fn spawn_input_cycle_agent_wraps() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Switch field to Agent, then cycle to verify agent_idx wraps.
        if let PopupState::SpawnInput { field, .. } = &mut state.popup {
            *field = SpawnField::Agent;
        }
        state.spawn_input_cycle(1);
        if let PopupState::SpawnInput { agent_idx, .. } = &state.popup {
            assert_eq!(*agent_idx, 1 % crate::worktree::AGENTS.len());
        }
        state.spawn_input_cycle(-1);
        if let PopupState::SpawnInput { agent_idx, .. } = &state.popup {
            assert_eq!(*agent_idx, 0);
        }
    }

    #[test]
    fn spawn_input_cycle_mode_field_toggles_spawn_kind() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Focus starts on MODE; cycle flips default ↔ worktree.
        state.spawn_input_cycle(1);
        if let PopupState::SpawnInput { kind_idx, .. } = &state.popup {
            assert_eq!(*kind_idx, 1, "forward cycle picks the worktree kind");
        }
        state.spawn_input_cycle(1);
        if let PopupState::SpawnInput { kind_idx, .. } = &state.popup {
            assert_eq!(*kind_idx, 0, "cycle wraps back to the folder kind");
        }
        state.spawn_input_cycle(-1);
        if let PopupState::SpawnInput { kind_idx, .. } = &state.popup {
            assert_eq!(*kind_idx, 1, "backward cycle wraps to the last kind");
        }
    }

    #[test]
    fn spawn_input_cycle_name_field_is_noop() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Name field (only reachable for worktree spawns) ignores cycles.
        if let PopupState::SpawnInput {
            field, kind_idx, ..
        } = &mut state.popup
        {
            *field = SpawnField::Name;
            *kind_idx = 1;
        }
        state.spawn_input_cycle(1);
        state.spawn_input_cycle(-1);
        if let PopupState::SpawnInput {
            agent_idx,
            kind_idx,
            ..
        } = &state.popup
        {
            assert_eq!(*agent_idx, 0);
            assert_eq!(*kind_idx, 1);
        }
    }

    #[test]
    fn spawn_input_push_pop_char_only_on_name_field() {
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        // Worktree kind so the NAME field is focusable.
        if let PopupState::SpawnInput { kind_idx, .. } = &mut state.popup {
            *kind_idx = 1;
        }

        state.spawn_input_next_field(); // Agent
        state.spawn_input_next_field(); // Name
        state.spawn_input_push_char('h');
        state.spawn_input_push_char('i');
        if let PopupState::SpawnInput { input, .. } = &state.popup {
            assert_eq!(input, "hi");
        }

        // Switch field — push/pop must have no effect on input text.
        state.spawn_input_next_field(); // wraps to Mode
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
        // Worktree spawns need a name (it becomes the branch slug).
        if let PopupState::SpawnInput { kind_idx, .. } = &mut state.popup {
            *kind_idx = 1;
        }
        state.confirm_spawn_input();
        if let PopupState::SpawnInput { error, .. } = &state.popup {
            assert_eq!(error.as_deref(), Some("name is empty"));
        } else {
            panic!("popup must stay open on error");
        }
    }

    #[test]
    fn confirm_spawn_input_folder_kind_needs_no_name() {
        // A folder spawn has no branch / worktree to name, so an empty
        // input must pass the name check. Without a channel (test
        // builds) dispatch is a no-op, and the session lookup fails in
        // a bare test env — what matters is the error is NOT the name
        // validation, i.e. the folder path never demands a name.
        let mut state = AppState::new("%99".into());
        state.open_spawn_input_for_repo("alpha".into(), "/tmp/alpha".into(), None);
        state.confirm_spawn_input();
        if let PopupState::SpawnInput { error, .. } = &state.popup {
            assert_ne!(
                error.as_deref(),
                Some("name is empty"),
                "folder spawns must not require a name"
            );
        } else {
            panic!("expected SpawnInput popup");
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
            label: "agent/x".into(),
            has_worktree: true,
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
    fn remove_warnings_land_on_the_dialog_they_were_opened_for() {
        // The pre-flight warnings arrive on a worker thread after the
        // dialog opens; a result must replace only its own dialog's
        // placeholder, and a closed dialog drops it.
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            label: "agent/x".into(),
            has_worktree: true,
            warning: REMOVE_CHECKING_WARNING
                .iter()
                .map(|l| l.to_string())
                .collect(),
            error: None,
            working: None,
            area: None,
        };
        // A different pane's late result must not overwrite this dialog.
        state.apply_popup_op_result(PopupOpResult::RemoveWarnings {
            pane_id: "%2".into(),
            warnings: vec!["other".into()],
        });
        let PopupState::RemoveConfirm { warning, .. } = &state.popup else {
            panic!("dialog must stay open");
        };
        assert_eq!(warning[0], "Checking worktree…");
        // The matching result replaces the placeholder.
        state.apply_popup_op_result(PopupOpResult::RemoveWarnings {
            pane_id: "%1".into(),
            warnings: vec!["Deletes uncommitted".into()],
        });
        let PopupState::RemoveConfirm { warning, .. } = &state.popup else {
            panic!("dialog must stay open");
        };
        assert_eq!(warning[0], "Deletes uncommitted");
        // A closed popup drops the result entirely.
        state.popup = PopupState::None;
        state.apply_popup_op_result(PopupOpResult::RemoveWarnings {
            pane_id: "%1".into(),
            warnings: vec!["late".into()],
        });
        assert!(matches!(state.popup, PopupState::None));
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
            label: "agent/x".into(),
            has_worktree: true,
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
            label: "agent/x".into(),
            has_worktree: true,
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
            kind_idx: 1,
            agent_idx: 0,
            field: SpawnField::Name,
            anchor_y: None,
            error: None,
            working: Some("creating worktree…"),
            area: None,
        };
        state.apply_popup_op_result(PopupOpResult::Spawn(Ok("agent/task".into())));
        assert!(!state.is_spawn_input_open(), "success must close the popup");

        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            label: "agent/x".into(),
            has_worktree: true,
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
        state.popup = PopupState::Repo {
            selected: 0,
            area: None,
            repo_start: 0,
        };
        state.apply_popup_op_result(PopupOpResult::Spawn(Err("boom".into())));
        assert!(matches!(state.popup, PopupState::Repo { .. }));
        state.apply_popup_op_result(PopupOpResult::Spawn(Ok("agent/task".into())));
        assert!(matches!(state.popup, PopupState::Repo { .. }));
    }

    // ─── Remove confirm popup ────────────────────────────────────────

    #[test]
    fn remove_confirm_accessors_and_close() {
        let mut state = AppState::new("%99".into());
        state.popup = PopupState::RemoveConfirm {
            pane_id: "%1".into(),
            label: "feature/x".into(),
            has_worktree: true,
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
        state.popup = PopupState::Repo {
            selected: 0,
            area: None,
            repo_start: 0,
        };
        state.confirm_remove(crate::worktree::RemoveMode::WindowOnly);
        assert!(matches!(state.popup, PopupState::Repo { .. }));
    }

    #[test]
    fn open_remove_confirm_without_selection_flashes() {
        let mut state = AppState::new("%99".into());
        state.open_remove_confirm();
        assert!(state.take_flash().is_some());
        assert!(!state.is_remove_confirm_open());
    }
}
