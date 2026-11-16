use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::state::{AppState, ChyronPhase, RepoFilter};

use crate::ui::text::{display_width, truncate_to_width};

/// Render the chyron: the most recent agent event, named in its own
/// color. `None` means the toolbar owns the row — no event has fired,
/// the fade has completed, or the event was consumed.
///
/// Consumption is resolved here against live pane state: a pane the
/// user has looked at (attention flag cleared), one that left its event
/// status, or one that vanished ends the display immediately — the
/// remaining frames are skipped and the toolbar returns.
///
/// A banner wider than `width` scrolls: the text holds still through
/// the flash and a short hold (the opening words stay readable), then
/// ticks left one slow column at a time until the tail is visible,
/// pauses, and only then fades. A fitting banner just holds.
pub(super) fn render_chyron<'a>(state: &AppState, width: u16) -> Option<Line<'a>> {
    let (text, kind) = state.chyron.banner(state)?;
    let theme = &state.theme;

    let text_w = display_width(&text);
    let avail_w = width as usize;
    let phase = state
        .chyron
        .current
        .as_ref()
        .unwrap()
        .phase(text_w, avail_w);
    if phase == ChyronPhase::Rest {
        return None;
    }

    let fg = match phase {
        ChyronPhase::Flash => theme.chyron_flash_fg(kind),
        ChyronPhase::Hold | ChyronPhase::Scroll { .. } | ChyronPhase::EndHold => {
            theme.chyron_linger_fg(kind)
        }
        ChyronPhase::Fade { frame } => theme.chyron_color(kind, frame),
        // Unreachable: rest (and consumed) return None above.
        ChyronPhase::Rest => theme.filter_inactive,
    };
    // Plain colored text on the default background throughout: a
    // same-hue tint under the text vibrates and reads worse than the
    // text alone.
    let style = Style::default().fg(fg);

    let shown = match phase {
        // The ticker and its end-hold show the scrolled window; the
        // end-hold rests at the tail so it does not blink away the
        // moment the scroll completes.
        ChyronPhase::Scroll { offset } => scroll_window(&text, offset, avail_w),
        ChyronPhase::EndHold => scroll_window(&text, text_w - avail_w, avail_w),
        _ => {
            // Pad to the row width so stale toolbar cells from the
            // previous frame are overwritten.
            let clipped = truncate_to_width(&text, avail_w);
            format!(
                "{}{}",
                clipped,
                " ".repeat(avail_w.saturating_sub(display_width(&clipped)))
            )
        }
    };
    Some(Line::from(Span::styled(shown, style)))
}

/// The column window of `text` starting at `offset`, `avail` columns
/// wide. Wide (double-column) characters straddling either edge are
/// dropped rather than clipped; the result is padded back out to
/// `avail` columns so the row renders over any stale cells.
fn scroll_window(text: &str, offset: usize, avail: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if avail == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        let start = col;
        col += w;
        if col <= offset {
            continue;
        }
        if start >= offset + avail {
            break;
        }
        if start < offset || col > offset + avail {
            continue;
        }
        out.push(ch);
        if display_width(&out) >= avail {
            break;
        }
    }
    let pad = avail.saturating_sub(display_width(&out));
    format!("{}{}", out, " ".repeat(pad))
}

/// The merged menu button as its three columns: label (`ⓘ` when a
/// notice exists, `—` otherwise), a space, and the dropdown arrow.
pub(super) fn menu_button_spans<'a>(state: &AppState) -> Vec<Span<'a>> {
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

    vec![
        Span::styled(label, Style::default().fg(label_color)),
        Span::raw(" "),
        Span::styled("▾", Style::default().fg(arrow_color)),
    ]
}

/// Render the toolbar row: the `/` search input flush left, and the
/// merged menu button right-aligned. This is the header's resting
/// state — the chyron overlays the row only while an event is alive
/// (see [`super::render_header_into`]).
pub(super) fn render_secondary_header<'a>(state: &AppState, width: u16) -> (Line<'a>, Option<u16>) {
    let gap = (width as usize)
        .saturating_sub(BUTTON_WIDTH)
        .saturating_sub(crate::state::OVERVIEW_BUTTON_WIDTH as usize);
    let mut spans: Vec<Span<'a>> = Vec::new();

    // The `▦` overview button owns the row's first columns in every
    // state; the search input and the banner start after it.
    spans.extend(overview_button_spans(state));

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

    spans.extend(menu_button_spans(state));

    let button_col = (width as usize).saturating_sub(BUTTON_WIDTH) as u16;
    (Line::from(spans), Some(button_col))
}

/// The overview-grid button as its two columns: the `▦` glyph and a
/// trailing space. Rendered at the header row's left edge in every
/// state (toolbar, search, chyron banner); clicks there open the grid.
pub(super) fn overview_button_spans<'a>(state: &AppState) -> Vec<Span<'a>> {
    vec![
        Span::styled("▦", Style::default().fg(state.theme.text_muted)),
        Span::raw(" "),
    ]
}

/// Width of the merged menu button: label + space + arrow, all single
/// column. The banner leaves exactly this much room in the corner, and
/// the button's click column is `width - BUTTON_WIDTH`.
pub(super) const BUTTON_WIDTH: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChyronEvent, ChyronKind, ChyronState};
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
        // The banner pads to the full row so stale toolbar cells from
        // the previous frame are overwritten.
        assert_eq!(
            line_text(&line),
            format!("✓ fix-auth done{}", " ".repeat(40 - 15))
        );
        // Flash: brightest ramp step, no background - a same-hue tint
        // under the text vibrates and reads worse than the text alone.
        let span = &line.spans[0];
        assert_eq!(
            span.style.fg,
            Some(state.theme.chyron_flash_fg(ChyronKind::Done))
        );
        assert_eq!(span.style.bg, None, "no background, ever");
    }

    #[test]
    fn hold_frame_is_colored_text_without_background() {
        let mut event = done_event();
        event.shown_at = Instant::now() - (crate::state::CHYRON_FRAME * 4);
        let state = state_with_event(event, true);
        assert!(state.chyron.is_showing(&state));
        let line = render_chyron(&state, 40).expect("an event renders");
        let span = &line.spans[0];
        assert_eq!(
            span.style.fg,
            Some(state.theme.chyron_linger_fg(ChyronKind::Done)),
            "the hold reads in the event hue, one step below the flash"
        );
        assert_eq!(span.style.bg, None, "no background - colored text only");
        // The fade ramps from the step after the hold color.
        let fade_color = state.theme.chyron_color(ChyronKind::Done, 0);
        assert_ne!(fade_color, state.theme.chyron_linger_fg(ChyronKind::Done));
    }

    #[test]
    fn overflowing_banner_scrolls_slowly_after_a_hold() {
        // "✓ a-very-long-session-name-that-will-not-fit done" is 45
        // columns; in a 20-column row it must tick left after the
        // flash + hold, one column per frame.
        let mut event = done_event();
        event.label = "a-very-long-session-name-that-will-not-fit".into();
        let mut state = state_with_event(event, true);
        state.repo_groups[0].panes[0].0.session_name =
            "a-very-long-session-name-that-will-not-fit".into();

        let text = "✓ a-very-long-session-name-that-will-not-fit done";
        let text_w = display_width(text);
        assert_eq!(text_w, 49);

        // Flash + hold: the opening words sit still, head-clipped with
        // an ellipsis marking the cut (no scroll yet).
        let line = render_chyron(&state, 20).unwrap();
        assert!(line_text(&line).starts_with("✓ a-very-long-s"));
        assert!(line_text(&line).ends_with('…'));

        // Land mid-frame (+50ms) so a slow test runner cannot shift
        // the phase index across a 200ms boundary.
        let frames_ago = |n: u32| {
            Instant::now() - (crate::state::CHYRON_FRAME * n + std::time::Duration::from_millis(50))
        };

        // One hold frame in: the scroll has moved one column. The ✓ is
        // three bytes but one column, so slice the ASCII tail and
        // reason in columns.
        let frames_in = crate::state::CHYRON_FLASH_FRAMES + crate::state::CHYRON_HOLD_FRAMES + 1;
        state.chyron.current.as_mut().unwrap().shown_at = frames_ago(frames_in as u32);
        let line = render_chyron(&state, 20).unwrap();
        let ascii = &text["✓".len()..];
        assert_eq!(line_text(&line), &ascii[..20]);

        // Deep into the scroll: the tail is in view.
        let tail_frame =
            crate::state::CHYRON_FLASH_FRAMES + crate::state::CHYRON_HOLD_FRAMES + (text_w - 20);
        state.chyron.current.as_mut().unwrap().shown_at = frames_ago(tail_frame as u32);
        let line = render_chyron(&state, 20).unwrap();
        assert_eq!(line_text(&line).trim_end(), &ascii[ascii.len() - 20..]);
    }

    #[test]
    fn rest_frame_renders_nothing_toolbar_owns_the_row() {
        let mut event = done_event();
        event.shown_at = Instant::now() - std::time::Duration::from_secs(5);
        let mut state = state_with_event(event, true);
        state.layout.header_width = Some(40);
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
        insta::assert_snapshot!(text, @"▦                          — ▾");
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
        insta::assert_snapshot!(text, @"▦                          ⓘ ▾");
        // The label flip is one column wide either way: the click
        // column — and the button's position — does not move.
        assert_eq!(col, Some(27));
        let label_span = &line.spans[line.spans.len() - 3];
        assert_eq!(label_span.content.as_ref(), "ⓘ");
        assert_eq!(label_span.style.fg, Some(state.theme.status_waiting));
    }

    #[test]
    fn toolbar_search_input_starts_after_the_overview_button() {
        let mut state = AppState::new(String::new());
        state.search.query = "codex".into();
        state.search.active = true;
        let (line, col) = render_secondary_header(&state, 30);
        let text = line_text(&line);
        insta::assert_snapshot!(text, @"▦ /codex█                  — ▾");
        assert!(text.starts_with("▦ /codex█"), "no leading gutter: {text:?}");
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
