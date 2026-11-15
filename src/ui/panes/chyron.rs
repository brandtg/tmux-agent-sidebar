use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::state::{AppState, ChyronKind, ChyronPhase, RepoFilter};
use crate::tmux::PaneStatus;

use crate::ui::text::{display_width, truncate_to_width};

/// Render the chyron row: the most recent agent event, named in its own
/// color and fading to rest. `None` renders a blank row (nothing has
/// fired since the sidebar started).
///
/// Consumption is resolved here against live pane state: a pane the
/// user has looked at (attention flag cleared), one that left its event
/// status, or one that vanished drops the event to rest immediately —
/// the remaining fade frames are skipped, never the rest text.
pub(super) fn render_chyron<'a>(state: &AppState, width: u16) -> Option<Line<'a>> {
    let event = state.chyron.current.as_ref()?;
    let theme = &state.theme;
    let icons = &state.icons;

    let mut phase = event.phase();
    if state.chyron.is_consumed(state, event) {
        phase = ChyronPhase::Rest;
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
        ChyronPhase::Fade { frame } => (theme.chyron_color(event.kind, frame), None),
        ChyronPhase::Rest => (theme.filter_inactive, None),
    };

    let mut style = Style::default().fg(fg);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }

    let text = format!(" {} {} {}", glyph, label, event.kind.verb());
    // Tail-fit: the verb is the payload's payoff, so truncation keeps
    // the trailing end (and its ellipsis) rather than the head.
    Some(Line::from(Span::styled(
        super::tail_fit(&text, width as usize),
        style,
    )))
}

/// Render the merged toolbar row: notices indicator, the `/` search
/// input, and the repo filter button. This is the panel's only toolbar;
/// the chyron owns the row above it.
pub(super) fn render_secondary_header<'a>(
    state: &AppState,
    width: u16,
) -> (Line<'a>, Option<u16>, Option<u16>) {
    let theme = &state.theme;
    let repo_icon = "▾";

    let repo_has_filter = !matches!(state.global.repo_filter, RepoFilter::All);
    let repo_style = if state.is_repo_popup_open() || repo_has_filter {
        Style::default().fg(theme.text_active)
    } else {
        Style::default().fg(theme.text_muted)
    };

    let has_notices_info = crate::ui::notices::has_info(state);
    let notices_button_col = has_notices_info.then_some(0);
    let notices_width = crate::ui::notices::BUTTON_WIDTH;
    let max_repo_label_width = width.saturating_sub((notices_width + 3) as u16) as usize;
    let repo_label = match &state.global.repo_filter {
        RepoFilter::All => "—".to_string(),
        RepoFilter::Repo(name) => truncate_to_width(name, max_repo_label_width),
    };
    let repo_btn_width = display_width(&repo_label) + 2; // label + space + arrow

    let gap = (width as usize).saturating_sub(repo_btn_width + notices_width);
    let repo_button_col = Some((notices_width + gap) as u16);

    let mut spans: Vec<Span<'a>> = Vec::new();
    if has_notices_info {
        spans.push(crate::ui::notices::button_span(state));
        spans.push(Span::raw(" "));
    } else {
        spans.push(Span::raw("  "));
    }

    // While `/` is open (or a query is applied), the left side of this
    // row becomes the search input. The repo button stays right-aligned,
    // so its column — and click target — does not move.
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
            Style::default().fg(theme.accent)
        } else {
            Style::default().fg(theme.text_active)
        };
        spans.push(Span::styled(shown, style));
        if !cursor.is_empty() {
            spans.push(Span::styled(cursor, Style::default().fg(theme.accent)));
        }
        spans.push(Span::raw(" ".repeat(pad)));
    } else {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.push(Span::styled(repo_label, repo_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(repo_icon, repo_style));

    (Line::from(spans), notices_button_col, repo_button_col)
}

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

    #[test]
    fn blank_when_no_event_has_ever_fired() {
        let state = AppState::new("%99".into());
        assert!(render_chyron(&state, 40).is_none());
    }

    #[test]
    fn flash_frame_names_glyph_agent_and_verb() {
        let state = state_with_event(done_event(), true);
        let line = render_chyron(&state, 40).expect("an event renders");
        let text = line_text(&line);
        assert_eq!(text, " ✓ fix-auth done");
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
    fn rest_frame_is_dim_with_no_background() {
        let mut event = done_event();
        event.shown_at = Instant::now() - std::time::Duration::from_secs(5);
        let state = state_with_event(event, true);
        let line = render_chyron(&state, 40).expect("an event renders");
        let span = &line.spans[0];
        assert_eq!(span.style.fg, Some(state.theme.filter_inactive));
        assert_eq!(span.style.bg, None);
        assert_eq!(line_text(&line), " ✓ fix-auth done");
    }

    #[test]
    fn consumed_event_renders_as_rest_not_blank() {
        let mut state = state_with_event(done_event(), true);
        // The user looked: the flag cleared.
        state.repo_groups[0].panes[0].0.attention = PaneAttention::None;
        let line = render_chyron(&state, 40).expect("consumed events still render");
        assert_eq!(line.spans[0].style.fg, Some(state.theme.filter_inactive));
        assert_eq!(line.spans[0].style.bg, None);
    }

    #[test]
    fn vanished_pane_uses_the_stored_label() {
        let state = state_with_event(done_event(), false);
        let line = render_chyron(&state, 40).expect("an event renders");
        assert_eq!(line_text(&line), " ✓ fix-auth done");
    }

    #[test]
    fn waiting_and_failed_reuse_the_status_glyphs() {
        let mut state = state_with_event(done_event(), true);
        state.chyron.current.as_mut().unwrap().kind = ChyronKind::Waiting;
        let line = render_chyron(&state, 40).unwrap();
        assert_eq!(line_text(&line), " ◐ fix-auth waiting");

        state.chyron.current.as_mut().unwrap().kind = ChyronKind::Failed;
        let line = render_chyron(&state, 40).unwrap();
        assert_eq!(line_text(&line), " ✕ fix-auth failed");
    }

    #[test]
    fn long_labels_truncate_to_the_row_width() {
        let mut event = done_event();
        event.label = "a-very-long-session-name-that-will-not-fit".into();
        let state = state_with_event(event, false);
        let line = render_chyron(&state, 20).unwrap();
        let text = line_text(&line);
        assert!(
            display_width(&text) <= 20,
            "chyron overflowed its width: {text:?}"
        );
        assert!(text.contains('…'), "truncation carries an ellipsis");
        assert!(text.ends_with("done"), "the verb survives truncation");
    }

    // ─── toolbar (moved from filter_bar.rs) ──────────────────────────

    #[test]
    fn toolbar_shows_notices_repo_and_search() {
        let mut state = AppState::new(String::new());
        state.version_notice = Some(crate::version::UpdateNotice {
            local_version: "0.2.6".into(),
            latest_version: "0.2.7".into(),
        });
        let text = line_text(&render_secondary_header(&state, 30).0);
        insta::assert_snapshot!(text, @"ⓘ                          — ▾");
    }

    #[test]
    fn toolbar_keeps_repo_position_with_or_without_notices_info() {
        let mut with_info = AppState::new(String::new());
        with_info.version_notice = Some(crate::version::UpdateNotice {
            local_version: "0.2.6".into(),
            latest_version: "0.2.7".into(),
        });
        with_info.notices.missing_hook_groups = vec![crate::state::NoticesMissingHookGroup {
            agent: "claude".into(),
            hooks: vec!["SessionStart".into()],
        }];

        let without_info = AppState::new(String::new());

        let (_, _, with_repo_col) = render_secondary_header(&with_info, 30);
        let (_, _, without_repo_col) = render_secondary_header(&without_info, 30);

        assert_eq!(with_repo_col, without_repo_col);
        assert_eq!(with_repo_col, Some(27));
    }

    #[test]
    fn toolbar_shows_active_search() {
        let mut state = AppState::new(String::new());
        state.search.query = "codex".into();
        state.search.active = true;
        let text = line_text(&render_secondary_header(&state, 30).0);
        insta::assert_snapshot!(text, @"  /codex█                  — ▾");
    }

    #[test]
    fn toolbar_shows_filtered_repo_name() {
        let mut state = AppState::new(String::new());
        state.global.repo_filter = RepoFilter::Repo("my-app".into());
        let text = line_text(&render_secondary_header(&state, 40).0);
        assert!(
            text.contains("my-app"),
            "toolbar should show the filtered repo name, got: {text}"
        );
        assert!(
            text.find("my-app").unwrap() < text.find("▾").unwrap(),
            "repo name should come before the arrow"
        );
    }

    #[test]
    fn toolbar_truncates_long_repo_name() {
        let mut state = AppState::new(String::new());
        state.global.repo_filter =
            RepoFilter::Repo("very-long-repository-name-that-exceeds-width".into());
        let text = line_text(&render_secondary_header(&state, 28).0);
        assert!(text.contains('…'), "repo name should be truncated");
        assert!(text.contains("▾"));
        assert!(
            text.find('…').unwrap() < text.find("▾").unwrap(),
            "repo name should come before the arrow"
        );
    }

    #[test]
    fn toolbar_repo_button_column_stable_while_searching() {
        let mut state = AppState::new(String::new());
        let (_, _, before) = render_secondary_header(&state, 30);
        state.search.query = "a-very-long-query-that-will-truncate".into();
        state.search.active = true;
        let (_, _, after) = render_secondary_header(&state, 30);
        assert_eq!(
            before, after,
            "the repo button must stay right-aligned while searching"
        );
    }

    #[test]
    fn toolbar_fits_narrow_width() {
        let mut state = AppState::new(String::new());
        state.search.query = "a-very-long-query".into();
        state.search.active = true;
        let text = line_text(&render_secondary_header(&state, 12).0);
        assert!(
            display_width(&text) <= 12,
            "search header overflowed its width: {text:?}"
        );
    }

    #[test]
    fn toolbar_repo_button_not_underlined_when_popup_open() {
        let mut state = AppState::new(String::new());
        state.popup = crate::state::PopupState::Repo {
            selected: 0,
            area: None,
        };
        let (line, _, _) = render_secondary_header(&state, 28);
        let last_span = line.spans.last().unwrap();
        assert!(
            !last_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED),
            "repo button should not be underlined when popup is open"
        );
        assert!(
            !last_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "repo button should not be bold when popup is open"
        );
    }
}
