use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::state::{AppState, ChyronKind, ChyronPhase, RepoFilter};
use crate::tmux::PaneStatus;

use crate::ui::text::display_width;

/// Render the chyron: the most recent agent event, named in its own
/// color and fading out. `None` means the toolbar owns the row — no
/// event has fired, the fade has completed, or the event was consumed.
///
/// Consumption is resolved here against live pane state: a pane the
/// user has looked at (attention flag cleared), one that left its event
/// status, or one that vanished ends the display immediately — the
/// remaining fade frames are skipped and the toolbar returns.
pub(super) fn render_chyron<'a>(state: &AppState, width: u16) -> Option<Line<'a>> {
    let event = state.chyron.current.as_ref()?;
    let theme = &state.theme;
    let icons = &state.icons;

    let mut phase = event.phase();
    if state.chyron.is_consumed(state, event) {
        phase = ChyronPhase::Rest;
    }
    if phase == ChyronPhase::Rest {
        return None;
    }

    let label = state.chyron.label(state, event);
    let glyph = match event.kind {
        ChyronKind::Done => icons.done_icon(),
        // Waiting and Failed reuse the status glyphs the rows already
        // teach the user to read; only Done needs its own (statuses
        // have no "done" of their own — a done-but-unseen pane is
        // still idle/running by status).
        ChyronKind::Waiting => icons.status_icon(&PaneStatus::Waiting),
        ChyronKind::Failed => icons.status_icon(&PaneStatus::Error),
    };

    let (fg, bg) = match phase {
        ChyronPhase::Flash { .. } => (
            theme.chyron_flash_fg(event.kind),
            Some(theme.chyron_flash_bg(event.kind)),
        ),
        // Linger: plain colored text on the default background — the
        // readable part of the notification, no eye-catcher.
        ChyronPhase::Linger { .. } => (theme.chyron_linger_fg(event.kind), None),
        ChyronPhase::Fade { frame } => (theme.chyron_color(event.kind, frame), None),
        // Unreachable: rest (and consumed) return None above.
        ChyronPhase::Rest => (theme.filter_inactive, None),
    };

    let mut style = Style::default().fg(fg);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }

    let text = format!("{} {} {}", glyph, label, event.kind.verb());
    // Tail-fit: the verb is the payload's payoff, so truncation keeps
    // the trailing end (and its ellipsis) rather than the head. The
    // line then pads to the full row width so a background flash tints
    // the whole row, not just the text cells.
    let text = super::tail_fit(&text, width as usize);
    let pad = width as usize - display_width(&text);
    let text = format!("{}{}", text, " ".repeat(pad));
    Some(Line::from(Span::styled(text, style)))
}

/// The merged menu button as its three columns: label (`ⓘ` when a
/// notice exists, `—` otherwise), a space, and the dropdown arrow.
/// `flash_bg` tints the cells to match a chyron flash frame so the
/// button can ride on top of the banner without punching a hole in the
/// background.
pub(super) fn menu_button_spans<'a>(
    state: &AppState,
    flash_bg: Option<ratatui::style::Color>,
) -> Vec<Span<'a>> {
    let theme = &state.theme;

    let has_info = crate::ui::notices::has_info(state);
    let menu_open = state.is_repo_popup_open();
    let repo_filtered = !matches!(state.global.repo_filter, RepoFilter::All);

    let (label, label_color) = if has_info {
        ("ⓘ", theme.status_waiting)
    } else {
        ("—", theme.text_muted)
    };
    let arrow_color = if menu_open || repo_filtered {
        theme.text_active
    } else {
        theme.text_muted
    };

    let base = flash_bg
        .map(|bg| Style::default().fg(theme.text_muted).bg(bg))
        .unwrap_or_default();
    vec![
        Span::styled(label, Style::default().fg(label_color).patch(base)),
        Span::styled(" ", base),
        Span::styled("▾", Style::default().fg(arrow_color).patch(base)),
    ]
}

/// Render the toolbar row: the `/` search input flush left, and the
/// merged menu button right-aligned. This is the header's resting
/// state — the chyron overlays the row only while an event is alive
/// (see [`super::render_header_into`]).
pub(super) fn render_secondary_header<'a>(state: &AppState, width: u16) -> (Line<'a>, Option<u16>) {
    let gap = (width as usize).saturating_sub(BUTTON_WIDTH);
    let mut spans: Vec<Span<'a>> = Vec::new();

    // While `/` is open (or a query is applied) the left side is the
    // search input, flush left. The button stays right-aligned so its
    // click column does not move.
    let searching = state.search.is_active() || state.search.has_query();
    if searching {
        let cursor = if state.search.is_active() { "█" } else { "" };
        let cursor_width = display_width(cursor);
        let budget = gap.saturating_sub(cursor_width);
        let raw = format!("/{}", state.search.query);
        let shown = super::tail_fit(&raw, budget);
        let shown_width = display_width(&shown);
        let pad = gap.saturating_sub(shown_width + cursor_width);
        let style = if state.search.is_active() {
            Style::default().fg(state.theme.accent)
        } else {
            Style::default().fg(state.theme.text_active)
        };
        spans.push(Span::styled(shown, style));
        if !cursor.is_empty() {
            spans.push(Span::styled(
                cursor,
                Style::default().fg(state.theme.accent),
            ));
        }
        spans.push(Span::raw(" ".repeat(pad)));
    } else {
        spans.push(Span::raw(" ".repeat(gap)));
    }

    spans.extend(menu_button_spans(state, None));

    let button_col = (width as usize).saturating_sub(BUTTON_WIDTH) as u16;
    (Line::from(spans), Some(button_col))
}

/// Width of the merged menu button: label + space + arrow, all single
/// column. The banner leaves exactly this much room in the corner, and
/// the button's click column is `width - BUTTON_WIDTH`.
pub(super) const BUTTON_WIDTH: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChyronEvent, ChyronState};
    use crate::tmux::PaneAttention;
    use std::time::Instant;

    /// A state whose chyron shows `event`, with the pane present or
    /// absent from `repo_groups` as requested.
    fn state_with_event(event: ChyronEvent, pane_present: bool) -> AppState {
        let mut state = AppState::new("%99".into());
        state.chyron = ChyronState {
            current: Some(event),
        };
        if pane_present {
            state.repo_groups = vec![crate::group::RepoGroup {
                name: "app".into(),
                has_focus: false,
                panes: vec![(
                    crate::tmux::PaneInfo {
                        pane_id: "%1".into(),
                        pane_active: false,
                        status: crate::tmux::PaneStatus::Idle,
                        attention: PaneAttention::Done,
                        agent: crate::tmux::AgentType::Claude,
                        path: String::new(),
                        launch_cwd: String::new(),
                        current_command: String::new(),
                        prompt: String::new(),
                        prompt_is_response: false,
                        started_at: None,
                        wait_reason: String::new(),
                        permission_mode: crate::tmux::PermissionMode::Default,
                        subagents: vec![],
                        pane_pid: None,
                        worktree: crate::tmux::WorktreeMetadata::default(),
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
                    },
                    crate::group::PaneGitInfo::default(),
                )],
            }];
        }
        state
    }

    fn done_event() -> ChyronEvent {
        ChyronEvent {
            pane_id: "%1".into(),
            kind: ChyronKind::Done,
            label: "fix-auth".into(),
            shown_at: Instant::now(),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    // ─── chyron render ───────────────────────────────────────────────

    #[test]
    fn nothing_renders_when_no_event_has_ever_fired() {
        let state = AppState::new("%99".into());
        assert!(render_chyron(&state, 40).is_none());
        assert!(!state.chyron.is_showing(&state));
    }

    #[test]
    fn flash_frame_names_glyph_agent_and_verb() {
        let state = state_with_event(done_event(), true);
        assert!(state.chyron.is_showing(&state));
        let line = render_chyron(&state, 40).expect("an event renders");
        // The banner pads to the full row so the background flash
        // tints edge to edge, not just the text cells.
        assert_eq!(
            line_text(&line),
            format!("✓ fix-auth done{}", " ".repeat(40 - 15))
        );
        // Flash: bright event foreground on the dark green tint.
        let span = &line.spans[0];
        assert_eq!(
            span.style.fg,
            Some(state.theme.chyron_flash_fg(ChyronKind::Done))
        );
        assert_eq!(
            span.style.bg,
            Some(state.theme.chyron_flash_bg(ChyronKind::Done))
        );
    }

    #[test]
    fn linger_frame_is_colored_text_without_background() {
        let mut event = done_event();
        event.shown_at = Instant::now() - (crate::state::CHYRON_FRAME * 4);
        let state = state_with_event(event, true);
        assert!(state.chyron.is_showing(&state));
        let line = render_chyron(&state, 40).expect("an event renders");
        let span = &line.spans[0];
        assert_eq!(
            span.style.fg,
            Some(state.theme.chyron_linger_fg(ChyronKind::Done)),
            "the linger reads in the event hue, one step below the flash"
        );
        assert_eq!(
            span.style.bg, None,
            "no background during the linger - colored text only"
        );
        // The fade ramps from the step after the linger color.
        let fade_color = state.theme.chyron_color(ChyronKind::Done, 0);
        assert_ne!(fade_color, state.theme.chyron_linger_fg(ChyronKind::Done));
    }

    #[test]
    fn rest_frame_renders_nothing_toolbar_owns_the_row() {
        let mut event = done_event();
        event.shown_at = Instant::now() - std::time::Duration::from_secs(5);
        let state = state_with_event(event, true);
        assert!(render_chyron(&state, 40).is_none());
        assert!(!state.chyron.is_showing(&state));
    }

    #[test]
    fn consumed_event_renders_nothing() {
        let mut state = state_with_event(done_event(), true);
        // The user looked: the flag cleared.
        state.repo_groups[0].panes[0].0.attention = PaneAttention::None;
        assert!(render_chyron(&state, 40).is_none());
        assert!(!state.chyron.is_showing(&state));
    }

    #[test]
    fn vanished_pane_ends_the_display() {
        // A pane that disappeared has been consumed: the overlay ends
        // and the toolbar returns. There is no resting chyron text.
        let state = state_with_event(done_event(), false);
        assert!(render_chyron(&state, 40).is_none());
        assert!(!state.chyron.is_showing(&state));
    }

    #[test]
    fn waiting_and_failed_reuse_the_status_glyphs() {
        let mut state = state_with_event(done_event(), true);
        state.chyron.current.as_mut().unwrap().kind = ChyronKind::Waiting;
        let line = render_chyron(&state, 40).unwrap();
        assert_eq!(line_text(&line).trim_end(), "◐ fix-auth waiting");

        // A failed event stays visible only while the pane is still in
        // error; mirror the live state the consumption check reads.
        state.chyron.current.as_mut().unwrap().kind = ChyronKind::Failed;
        state.repo_groups[0].panes[0].0.status = crate::tmux::PaneStatus::Error;
        let line = render_chyron(&state, 40).unwrap();
        assert_eq!(line_text(&line).trim_end(), "✕ fix-auth failed");
    }

    #[test]
    fn long_labels_truncate_to_the_row_width() {
        let mut event = done_event();
        event.label = "a-very-long-session-name-that-will-not-fit".into();
        let mut state = state_with_event(event, true);
        // The label resolves live from the pane's title.
        state.repo_groups[0].panes[0].0.session_name =
            "a-very-long-session-name-that-will-not-fit".into();
        let line = render_chyron(&state, 20).unwrap();
        let text = line_text(&line);
        assert!(
            display_width(&text) <= 20,
            "chyron overflowed its width: {text:?}"
        );
        assert!(text.contains('…'), "truncation carries an ellipsis");
        assert!(text.ends_with("done"), "the verb survives truncation");
    }

    // ─── suppression: search and the menu claim the row ─────────────

    #[test]
    fn open_search_suppresses_the_chyron() {
        let mut state = state_with_event(done_event(), true);
        state.open_search();
        assert!(!state.chyron.is_showing(&state));
        // Dismissal is outright: the event does not come back.
        assert!(state.chyron.current.is_none());
    }

    #[test]
    fn opening_the_menu_suppresses_the_chyron() {
        let mut state = state_with_event(done_event(), true);
        state.repo_groups = vec![crate::group::RepoGroup {
            name: "app".into(),
            has_focus: false,
            panes: vec![],
        }];
        state.toggle_repo_popup();
        assert!(state.is_repo_popup_open());
        assert!(!state.chyron.is_showing(&state));
        assert!(state.chyron.current.is_none());
    }

    // ─── toolbar ─────────────────────────────────────────────────────

    #[test]
    fn toolbar_shows_placeholder_button_flush_right() {
        let state = AppState::new(String::new());
        let (line, col) = render_secondary_header(&state, 30);
        let text = line_text(&line);
        insta::assert_snapshot!(text, @"                           — ▾");
        // The button is the last three columns; its click column is
        // width - 3.
        assert_eq!(col, Some(27));
        assert!(text.ends_with("— ▾"));
    }

    #[test]
    fn toolbar_button_reads_info_when_a_notice_exists() {
        let mut state = AppState::new(String::new());
        state.version_notice = Some(crate::version::UpdateNotice {
            local_version: "0.2.6".into(),
            latest_version: "0.2.7".into(),
        });
        let (line, col) = render_secondary_header(&state, 30);
        let text = line_text(&line);
        insta::assert_snapshot!(text, @"                           ⓘ ▾");
        // The label flip is one column wide either way: the click
        // column — and the button's position — does not move.
        assert_eq!(col, Some(27));
        let label_span = &line.spans[line.spans.len() - 3];
        assert_eq!(label_span.content.as_ref(), "ⓘ");
        assert_eq!(label_span.style.fg, Some(state.theme.status_waiting));
    }

    #[test]
    fn toolbar_search_input_is_flush_left() {
        let mut state = AppState::new(String::new());
        state.search.query = "codex".into();
        state.search.active = true;
        let (line, col) = render_secondary_header(&state, 30);
        let text = line_text(&line);
        insta::assert_snapshot!(text, @"/codex█                    — ▾");
        assert!(text.starts_with("/codex█"), "no leading gutter: {text:?}");
        // The button column is unchanged by the input.
        assert_eq!(col, Some(27));
    }

    #[test]
    fn toolbar_button_column_stable_while_searching() {
        let mut state = AppState::new(String::new());
        let (_, before) = render_secondary_header(&state, 30);
        state.search.query = "a-very-long-query-that-will-truncate".into();
        state.search.active = true;
        let (_, after) = render_secondary_header(&state, 30);
        assert_eq!(
            before, after,
            "the menu button must stay right-aligned while searching"
        );
    }

    #[test]
    fn toolbar_fits_narrow_width() {
        let mut state = AppState::new(String::new());
        state.search.query = "a-very-long-query".into();
        state.search.active = true;
        let (line, _) = render_secondary_header(&state, 12);
        let text = line_text(&line);
        assert!(
            display_width(&text) <= 12,
            "search header overflowed its width: {text:?}"
        );
    }

    #[test]
    fn toolbar_arrow_lights_when_repo_filtered() {
        let mut state = AppState::new(String::new());
        state.global.repo_filter = RepoFilter::Repo("my-app".into());
        let (line, _) = render_secondary_header(&state, 30);
        let text = line_text(&line);
        assert!(text.ends_with("— ▾"), "the label stays —: {text:?}");
        let arrow_span = line.spans.last().unwrap();
        assert_eq!(arrow_span.style.fg, Some(state.theme.text_active));
    }

    #[test]
    fn toolbar_fits_extremely_narrow_width() {
        let state = AppState::new(String::new());
        // Below the button width: nothing panics, the col saturates.
        let (_, col) = render_secondary_header(&state, 2);
        assert_eq!(col, Some(0));
    }
}
