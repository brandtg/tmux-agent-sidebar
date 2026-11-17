//! The chyron: the agents panel's header-row event channel.
//!
//! The list rows are a *state* channel — they show what is. The chyron
//! is the *event* channel: when a pane's attention flag or status
//! transitions, the row names the event in the event's color — a short
//! bright flash, then a readable stretch of plain colored text — and
//! the toolbar takes the row back. A banner longer than the row
//! becomes a ticker: it holds the opening words first, scrolls
//! horizontally at a slow, readable pace, pauses on the tail, and only
//! then fades. Nothing else in the sidebar remembers "what just
//! changed": attention flags clear on focus and statuses mutate
//! silently, so once a moment has passed only the chyron can still
//! name it.
//!
//! Events are diffed per tick against the previous snapshot (see
//! [`AppState::observe_chyron_transitions`]); rendering is a pure
//! function of the stored event + the pane's current state + elapsed
//! time, so no timers or threads are added — the 200ms spinner tick
//! already redraws through any fade.

use std::time::{Duration, Instant};

use super::AppState;
use crate::tmux::{PaneAttention, PaneStatus};

/// Frame duration of the chyron lifecycle. Matches the spinner tick in
/// `app.rs`, which already redraws at this cadence — phases are counted
/// in these frames so flash + scroll + fade land on rendered frames.
pub const CHYRON_FRAME: Duration = Duration::from_millis(200);

/// Full-brightness frames: the eye-catcher. Deliberately short — it
/// says "look", not "read". Brightest ramp step, no background: a
/// same-hue tint under the text vibrates and reads worse than plain
/// bright text on the terminal background.
pub const CHYRON_FLASH_FRAMES: usize = 3;
/// Static frames after the flash when the banner fits the row: the
/// readable part of the notification, ~3 s.
pub const CHYRON_LINGER_FRAMES: usize = 15;
/// When the banner overflows, scrolling waits this long after the
/// flash so the opening words are readable before the ticker starts.
pub const CHYRON_HOLD_FRAMES: usize = 8;
/// Pause with the banner's tail visible before the fade — the end of
/// a scroll should not blink away the moment it arrives.
pub const CHYRON_END_HOLD_FRAMES: usize = 5;
/// Fade-out frames: the ramp walks down and the toolbar takes the row
/// back.
pub const CHYRON_FADE_FRAMES: usize = 4;
/// Scroll speed: columns per frame. 200 ms per column is a slow,
/// readable ticker.
pub const CHYRON_SCROLL_COLS_PER_FRAME: usize = 1;

/// Kind of agent event the chyron names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChyronKind {
    /// A turn finished and the user has not looked at it yet
    /// (`@pane_attention` `None → Done`).
    Done,
    /// A hook wants the user's eye (permission prompt, question,
    /// teammate idle) (`None → Notification`).
    Waiting,
    /// The pane transitioned to `PaneStatus::Error`.
    Failed,
}

impl ChyronKind {
    /// One-word banner verb. Fixed strings on purpose: hook-provided
    /// `wait_reason` text is arbitrary and can be long; the list row
    /// remains the place for detail.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Waiting => "waiting",
            Self::Failed => "failed",
        }
    }

    /// Severity for same-tick tiebreaks: the most eye-worthy event wins.
    pub fn severity(self) -> u8 {
        match self {
            Self::Failed => 2,
            Self::Waiting => 1,
            Self::Done => 0,
        }
    }
}

/// The event currently (or last) shown by the chyron.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChyronEvent {
    pub pane_id: String,
    pub kind: ChyronKind,
    /// Session title at fire time; the renderer re-resolves from the
    /// live pane when it still exists so renames stay current, falling
    /// back to this once the pane is gone.
    pub label: String,
    pub shown_at: Instant,
}

impl ChyronEvent {
    /// Which animation phase the event is in. The timeline depends on
    /// the banner's width (`text_w`) against the row width it may
    /// occupy (`avail_w`): a fitting banner holds static and reads,
    /// an overflowing one turns into a ticker — the scroll always
    /// completes before the fade (and thus the close) begins.
    /// Consumption (the pane being looked at / gone) is resolved by
    /// the renderer against live pane state and forces
    /// [`ChyronPhase::Rest`].
    pub fn phase(&self, text_w: usize, avail_w: usize) -> ChyronPhase {
        let frame = self.shown_at.elapsed().as_millis() / CHYRON_FRAME.as_millis();
        let frame = frame as usize;
        let scrolls = text_w > avail_w;
        let hold = if scrolls {
            CHYRON_HOLD_FRAMES
        } else {
            CHYRON_LINGER_FRAMES
        };
        if frame < CHYRON_FLASH_FRAMES {
            return ChyronPhase::Flash;
        }
        let f = frame - CHYRON_FLASH_FRAMES;
        if f < hold {
            return ChyronPhase::Hold;
        }
        let mut f = f - hold;
        if scrolls {
            let scroll_frames = (text_w - avail_w).div_ceil(CHYRON_SCROLL_COLS_PER_FRAME);
            if f < scroll_frames {
                return ChyronPhase::Scroll {
                    offset: f * CHYRON_SCROLL_COLS_PER_FRAME,
                };
            }
            f -= scroll_frames;
            if f < CHYRON_END_HOLD_FRAMES {
                return ChyronPhase::EndHold;
            }
            f -= CHYRON_END_HOLD_FRAMES;
        }
        if f < CHYRON_FADE_FRAMES {
            return ChyronPhase::Fade { frame: f };
        }
        ChyronPhase::Rest
    }
}

/// Total frames a banner of `text_w` columns stays visible in an
/// `avail_w`-column row. The fade — and with it the close — only
/// starts after the scroll has fully revealed the tail.
pub fn display_frames(text_w: usize, avail_w: usize) -> usize {
    if text_w <= avail_w {
        CHYRON_FLASH_FRAMES + CHYRON_LINGER_FRAMES + CHYRON_FADE_FRAMES
    } else {
        CHYRON_FLASH_FRAMES
            + CHYRON_HOLD_FRAMES
            + (text_w - avail_w).div_ceil(CHYRON_SCROLL_COLS_PER_FRAME)
            + CHYRON_END_HOLD_FRAMES
            + CHYRON_FADE_FRAMES
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChyronPhase {
    /// Brightest ramp step, no background: the eye-catcher.
    Flash,
    /// Static colored text — the whole show when the banner fits, the
    /// pre-scroll delay when it overflows.
    Hold,
    /// Horizontal ticker; the value is the column offset the text is
    /// shifted left by.
    Scroll { offset: usize },
    /// The tail is fully visible; brief pause before the fade.
    EndHold,
    /// Ramp-down frames; the value is the fade frame index.
    Fade { frame: usize },
    /// Display over: the toolbar owns the row again.
    Rest,
}

/// Chyron state for one sidebar process. Deliberately transient —
/// like [`super::SearchState`], it is not persisted to tmux options
/// and not part of [`super::GlobalState`]. Every sidebar diffs the
/// same global pane flags, so open sidebars converge on the same
/// event; consumption via focus is already global through tmux.
///
/// The chyron owns the single header row only while an event is
/// alive: it overlays the toolbar for the flash/fade window and the
/// toolbar returns afterwards (or immediately on consumption, or when
/// the `/` input or header menu opens). There is no resting state —
/// the row is never reserved for the chyron alone.
///
/// Startup floods are impossible by construction: the first snapshot
/// diffs against an empty baseline (`repo_groups` is still empty), so
/// no transition exists yet — only changes observed *after* the first
/// refresh can fire.
#[derive(Debug, Default)]
pub struct ChyronState {
    pub current: Option<ChyronEvent>,
}

impl ChyronState {
    /// True when the event's pane no longer carries its condition:
    /// the flag cleared (the user looked), the status moved off
    /// error, or the pane disappeared entirely. A consumed event
    /// renders as rest (not blank) so the "last event" line stays
    /// stable; it is replaced on the next transition.
    pub fn is_consumed(&self, state: &AppState, event: &ChyronEvent) -> bool {
        let Some(pane) = state.pane_by_id(&event.pane_id) else {
            return true;
        };
        match event.kind {
            ChyronKind::Done | ChyronKind::Waiting => pane.attention == PaneAttention::None,
            ChyronKind::Failed => pane.status != PaneStatus::Error,
        }
    }

    /// Whether the chyron currently owns the header row. The single
    /// header row is shared with the toolbar: the chyron shows only
    /// while an unconsumed event is inside its display window — which,
    /// for an overflowing banner, lasts until the ticker has fully
    /// revealed the tail — and yields when the `/` input or the header
    /// menu is open (opening either dismisses the event outright —
    /// this guard covers any other path that opens them).
    pub fn is_showing(&self, state: &AppState) -> bool {
        self.visible_banner(state).is_some()
    }

    /// [`Self::banner`] restricted to the event's display window: the
    /// returned banner is the one this frame renders — `None` when the
    /// toolbar owns the row. The render path builds the line once through
    /// here; the old render called `is_showing` and then rebuilt the same
    /// banner for the actual drawing, twice per frame while scrolling
    /// (audit 2026-11-16 J6).
    pub fn visible_banner(&self, state: &AppState) -> Option<(String, ChyronKind)> {
        let (text, kind) = self.banner(state)?;
        let text_w = crate::ui::text::display_width(&text);
        let avail_w = state.layout.header_width.unwrap_or(0) as usize;
        let frame = self
            .current
            .as_ref()
            .unwrap()
            .shown_at
            .elapsed()
            .as_millis()
            / CHYRON_FRAME.as_millis();
        if (frame as usize) >= display_frames(text_w, avail_w) {
            return None;
        }
        Some((text, kind))
    }

    /// The banner text and its kind, when an unconsumed event exists.
    /// Both the renderer and the display-window math build the line
    /// through here so the width they agree on is the width that
    /// renders.
    pub fn banner(&self, state: &AppState) -> Option<(String, ChyronKind)> {
        let event = self.current.as_ref()?;
        if self.is_consumed(state, event) {
            return None;
        }
        if state.search.is_active() || state.is_repo_popup_open() {
            return None;
        }
        let glyph = match event.kind {
            ChyronKind::Done => state.icons.done_icon(),
            ChyronKind::Waiting => state.icons.status_icon(&PaneStatus::Waiting),
            ChyronKind::Failed => state.icons.status_icon(&PaneStatus::Error),
        };
        let label = self.label(state, event);
        Some((
            format!("{} {} {}", glyph, label, event.kind.verb()),
            event.kind,
        ))
    }

    /// Display label for the event: the pane's current title when it
    /// still exists, the stored one otherwise.
    pub fn label(&self, state: &AppState, event: &ChyronEvent) -> String {
        state
            .pane_by_id(&event.pane_id)
            .map(chyron_label)
            .unwrap_or_else(|| event.label.clone())
    }
}

/// The title a pane renders with: its session name when one is
/// resolved, the agent label otherwise — the same fallback the rows
/// use (a row with no session title renders the agent label).
fn chyron_label(pane: &crate::tmux::PaneInfo) -> String {
    let name = pane.session_name.trim();
    if name.is_empty() {
        pane.agent.label().to_string()
    } else {
        name.to_string()
    }
}

impl AppState {
    /// Diff the previous snapshot's agent panes against the fresh one
    /// and record the winning transition as the chyron event. Called
    /// from [`Self::apply_session_snapshot`] after the new panes are in
    /// hand but conceptually *before* `repo_groups` is replaced — the
    /// old map is captured by the caller from the still-old
    /// `repo_groups`.
    ///
    /// Selection rule: one slot, latest wins. Higher severity always
    /// displaces; among equal severities the first pane in snapshot
    /// order wins, which is the stable choice.
    pub(crate) fn observe_chyron_transitions(
        &mut self,
        old: &std::collections::HashMap<String, (PaneStatus, PaneAttention, String)>,
        sessions: &[crate::tmux::SessionInfo],
    ) {
        let mut candidate: Option<ChyronEvent> = None;
        for pane in sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .flat_map(|window| window.panes.iter())
        {
            let Some((old_status, old_attention, old_label)) = old.get(&pane.pane_id) else {
                continue;
            };
            // Attention transitions take priority; the status check only
            // applies when no attention kind fired (including when the
            // old attention was already set — a flag that persists is
            // not a new event, but a fresh failure still is).
            let attention_kind = if *old_attention == PaneAttention::None {
                match pane.attention {
                    PaneAttention::Done => Some(ChyronKind::Done),
                    PaneAttention::Notification => Some(ChyronKind::Waiting),
                    PaneAttention::None => None,
                }
            } else {
                None
            };
            let kind = attention_kind.or_else(|| {
                if *old_status != PaneStatus::Error && pane.status == PaneStatus::Error {
                    Some(ChyronKind::Failed)
                } else {
                    None
                }
            });
            let Some(kind) = kind else {
                continue;
            };
            // The fresh parse's session name is empty until
            // `refresh_session_names` runs; the previous title fills in
            // so the banner never shows a bare agent label for a pane
            // that had a name one tick ago. The renderer re-resolves
            // live labels anyway.
            let fresh_name = pane.session_name.trim();
            let label = if fresh_name.is_empty() {
                old_label.clone()
            } else {
                fresh_name.to_string()
            };
            if candidate
                .as_ref()
                .is_some_and(|current: &ChyronEvent| current.kind.severity() >= kind.severity())
            {
                continue;
            }
            candidate = Some(ChyronEvent {
                pane_id: pane.pane_id.clone(),
                kind,
                label,
                shown_at: Instant::now(),
            });
        }
        if let Some(event) = candidate {
            self.chyron.current = Some(event);
        }
    }

    /// Capture the per-pane chyron inputs of the current snapshot:
    /// status, attention, and the resolved title. Called before
    /// `repo_groups` is replaced.
    pub(crate) fn chyron_baseline(
        &self,
    ) -> std::collections::HashMap<String, (PaneStatus, PaneAttention, String)> {
        self.repo_groups
            .iter()
            .flat_map(|group| group.panes.iter())
            .map(|(pane, _)| {
                (
                    pane.pane_id.clone(),
                    (pane.status.clone(), pane.attention, chyron_label(pane)),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{PaneGitInfo, RepoGroup};
    use crate::tmux::{AgentType, PaneInfo, PermissionMode, WorktreeMetadata};

    fn pane(id: &str, status: PaneStatus, attention: PaneAttention) -> PaneInfo {
        PaneInfo {
            pane_id: id.into(),
            pane_active: false,
            status,
            attention,
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
            session_name: "fix-auth".into(),
            tmux_session: "work".into(),
            window_index: 0,
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    fn session(panes: Vec<PaneInfo>) -> Vec<crate::tmux::SessionInfo> {
        vec![crate::tmux::SessionInfo {
            session_name: "work".into(),
            windows: vec![crate::tmux::WindowInfo {
                window_id: "@0".into(),
                window_name: "test".into(),
                window_active: true,
                auto_rename: false,
                panes,
            }],
        }]
    }

    fn state_with(panes: Vec<PaneInfo>) -> AppState {
        let mut state = AppState::new("%99".into());
        state.repo_groups = vec![RepoGroup {
            name: "app".into(),
            has_focus: false,
            panes: panes
                .into_iter()
                .map(|p| (p, PaneGitInfo::default()))
                .collect(),
        }];
        state
    }

    // ─── phase timing ────────────────────────────────────────────────

    #[test]
    fn phase_walks_flash_hold_fade_rest_when_the_banner_fits() {
        let flash = ChyronEvent {
            pane_id: "%1".into(),
            kind: ChyronKind::Done,
            label: "fix-auth".into(),
            shown_at: Instant::now(),
        };
        assert_eq!(flash.phase(16, 40), ChyronPhase::Flash);
        // Past the flash window (3 frames = 600ms): static hold.
        let holding = ChyronEvent {
            shown_at: Instant::now() - (CHYRON_FRAME * 3 + Duration::from_millis(100)),
            ..flash.clone()
        };
        assert!(matches!(holding.phase(16, 40), ChyronPhase::Hold));
        // Past flash + linger (3 + 15 frames): the fade.
        let fading = ChyronEvent {
            shown_at: Instant::now() - (CHYRON_FRAME * 18 + Duration::from_millis(100)),
            ..flash.clone()
        };
        assert!(matches!(
            fading.phase(16, 40),
            ChyronPhase::Fade { frame: 0 }
        ));
        // Past flash + linger + fade (22 frames = 4.4s): rest.
        let rested = ChyronEvent {
            shown_at: Instant::now() - (CHYRON_FRAME * 22 + Duration::from_millis(100)),
            ..flash
        };
        assert_eq!(rested.phase(16, 40), ChyronPhase::Rest);
    }

    #[test]
    fn overflowing_banner_scrolls_to_the_tail_before_closing() {
        let flash = ChyronEvent {
            pane_id: "%1".into(),
            kind: ChyronKind::Done,
            label: "fix-auth".into(),
            shown_at: Instant::now(),
        };
        // 16 columns of banner in a 10-column row: 6 columns to scroll.
        let (text_w, avail_w) = (16, 10);

        // Flash, then the shorter pre-scroll hold.
        assert_eq!(flash.phase(text_w, avail_w), ChyronPhase::Flash);
        let holding = ChyronEvent {
            shown_at: Instant::now() - (CHYRON_FRAME * 4 + Duration::from_millis(50)),
            ..flash.clone()
        };
        assert!(matches!(holding.phase(text_w, avail_w), ChyronPhase::Hold));

        // The scroll: one column per frame, offsets 0..6.
        let scrolling = ChyronEvent {
            shown_at: Instant::now()
                - (CHYRON_FRAME * (CHYRON_FLASH_FRAMES + CHYRON_HOLD_FRAMES + 1) as u32),
            ..flash.clone()
        };
        assert_eq!(
            scrolling.phase(text_w, avail_w),
            ChyronPhase::Scroll { offset: 1 }
        );

        // The end hold shows the tail; only then the fade. Mid-frame
        // offsets (+50ms) so a slow runner cannot shift the phase.
        let end = CHYRON_FLASH_FRAMES + CHYRON_HOLD_FRAMES + 6;
        let end_hold = ChyronEvent {
            shown_at: Instant::now() - (CHYRON_FRAME * end as u32 + Duration::from_millis(50)),
            ..flash.clone()
        };
        assert!(matches!(
            end_hold.phase(text_w, avail_w),
            ChyronPhase::EndHold
        ));
        let fading = ChyronEvent {
            shown_at: Instant::now()
                - (CHYRON_FRAME * (end + CHYRON_END_HOLD_FRAMES) as u32
                    + Duration::from_millis(50)),
            ..flash.clone()
        };
        assert!(matches!(
            fading.phase(text_w, avail_w),
            ChyronPhase::Fade { frame: 0 }
        ));

        // The display window covers the whole ticker; the close never
        // preempts the scroll.
        assert_eq!(
            display_frames(text_w, avail_w),
            CHYRON_FLASH_FRAMES
                + CHYRON_HOLD_FRAMES
                + 6
                + CHYRON_END_HOLD_FRAMES
                + CHYRON_FADE_FRAMES
        );
    }

    // ─── transition diffing ──────────────────────────────────────────

    #[test]
    fn first_tick_is_baseline_and_fires_nothing() {
        // The very first refresh diffs against an empty baseline (the
        // pre-refresh repo_groups), so a pane that already carries a
        // flag cannot fire — only changes observed after the first
        // snapshot can.
        let mut state = state_with(vec![]);
        let old = state.chyron_baseline();
        assert!(old.is_empty());
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane("%1", PaneStatus::Idle, PaneAttention::Done)]),
        );
        assert!(
            state.chyron.current.is_none(),
            "panes absent from the baseline are not transitions"
        );
    }

    #[test]
    fn done_and_waiting_transitions_fire_events() {
        let mut state = state_with(vec![
            pane("%1", PaneStatus::Idle, PaneAttention::None),
            pane("%2", PaneStatus::Running, PaneAttention::None),
        ]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![
                pane("%1", PaneStatus::Idle, PaneAttention::Done),
                pane("%2", PaneStatus::Running, PaneAttention::Notification),
            ]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        // Waiting outranks Done on the same tick.
        assert_eq!(event.pane_id, "%2");
        assert_eq!(event.kind, ChyronKind::Waiting);
        assert_eq!(event.label, "fix-auth");
    }

    #[test]
    fn equal_severity_keeps_the_first_pane() {
        let mut state = state_with(vec![
            pane("%1", PaneStatus::Idle, PaneAttention::None),
            pane("%2", PaneStatus::Idle, PaneAttention::None),
        ]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![
                pane("%1", PaneStatus::Idle, PaneAttention::Done),
                pane("%2", PaneStatus::Idle, PaneAttention::Done),
            ]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        assert_eq!(event.pane_id, "%1", "snapshot order breaks the tie");
    }

    #[test]
    fn severity_displaces_earlier_events() {
        let mut state = state_with(vec![
            pane("%1", PaneStatus::Running, PaneAttention::None),
            pane("%2", PaneStatus::Running, PaneAttention::None),
            pane("%3", PaneStatus::Running, PaneAttention::None),
        ]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![
                pane("%1", PaneStatus::Running, PaneAttention::Done),
                pane("%2", PaneStatus::Running, PaneAttention::Notification),
                pane("%3", PaneStatus::Error, PaneAttention::None),
            ]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        assert_eq!(event.pane_id, "%3", "failed outranks waiting and done");
        assert_eq!(event.kind, ChyronKind::Failed);
    }

    #[test]
    fn status_error_transition_fires_failed() {
        let mut state = state_with(vec![pane("%1", PaneStatus::Running, PaneAttention::None)]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane("%1", PaneStatus::Error, PaneAttention::None)]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        assert_eq!(event.kind, ChyronKind::Failed);
    }

    #[test]
    fn no_transition_no_event() {
        let mut state = state_with(vec![pane("%1", PaneStatus::Running, PaneAttention::None)]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane("%1", PaneStatus::Idle, PaneAttention::None)]),
        );
        assert!(state.chyron.current.is_none());
    }

    #[test]
    fn unnamed_new_parse_falls_back_to_previous_title() {
        let mut state = state_with(vec![pane("%1", PaneStatus::Idle, PaneAttention::None)]);
        let old = state.chyron_baseline();
        // Fresh parse: session_name empty until refresh_session_names runs.
        let mut fresh = pane("%1", PaneStatus::Idle, PaneAttention::Done);
        fresh.session_name.clear();
        state.observe_chyron_transitions(&old, &session(vec![fresh]));
        let event = state.chyron.current.as_ref().expect("an event fired");
        assert_eq!(event.label, "fix-auth", "the previous title fills in");
    }

    #[test]
    fn a_persistent_flag_is_not_a_new_event_but_a_fresh_failure_is() {
        let mut state = state_with(vec![pane(
            "%1",
            PaneStatus::Running,
            PaneAttention::Notification,
        )]);
        let old = state.chyron_baseline();
        // The Notification flag persists — no new attention event — but
        // the status drops to Error: the failure still fires.
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane(
                "%1",
                PaneStatus::Error,
                PaneAttention::Notification,
            )]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        assert_eq!(event.kind, ChyronKind::Failed);
    }

    // ─── consumption ─────────────────────────────────────────────────

    #[test]
    fn consumed_when_flag_clears_or_pane_vanishes() {
        let mut state = state_with(vec![pane("%1", PaneStatus::Idle, PaneAttention::None)]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane("%1", PaneStatus::Idle, PaneAttention::Done)]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        // The refresh lands: the live pane now carries the flag the
        // event names (the renderer reads post-refresh state).
        state.repo_groups[0].panes[0].0.attention = PaneAttention::Done;
        assert!(!state.chyron.is_consumed(&state, event));

        // The user looks at the pane: the flag clears, the event rests.
        state.repo_groups[0].panes[0].0.attention = PaneAttention::None;
        assert!(state.chyron.is_consumed(&state, event));

        // A vanished pane is consumed too.
        state.repo_groups.clear();
        assert!(state.chyron.is_consumed(&state, event));
    }

    #[test]
    fn failed_event_consumes_when_status_leaves_error() {
        let mut state = state_with(vec![pane("%1", PaneStatus::Running, PaneAttention::None)]);
        let old = state.chyron_baseline();
        state.observe_chyron_transitions(
            &old,
            &session(vec![pane("%1", PaneStatus::Error, PaneAttention::None)]),
        );
        let event = state.chyron.current.as_ref().expect("an event fired");
        state.repo_groups[0].panes[0].0.status = PaneStatus::Error;
        assert!(!state.chyron.is_consumed(&state, event));
        state.repo_groups[0].panes[0].0.status = PaneStatus::Running;
        assert!(state.chyron.is_consumed(&state, event));
    }

    // ─── labels ──────────────────────────────────────────────────────

    #[test]
    fn label_falls_back_to_agent_name() {
        let mut p = pane("%1", PaneStatus::Idle, PaneAttention::None);
        p.session_name.clear();
        assert_eq!(chyron_label(&p), "claude");
        p.session_name = "  ".into();
        assert_eq!(chyron_label(&p), "claude", "blank names count as none");
    }
}
