mod feed;
mod repos;

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::state::{AppState, BottomTab, Focus};

use super::text::display_width;

fn render_centered(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    // Vertically center: pad with empty lines above
    let top_pad = area.height.saturating_sub(1) / 2;
    let mut lines: Vec<Line<'_>> = Vec::new();
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    let paragraph = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(paragraph, area);
}

pub fn draw_bottom(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let theme = &state.theme;
    let border_color = if state.focus_state.focus == Focus::BottomPanel {
        theme.accent
    } else {
        theme.border_inactive
    };

    // Unseen attention-event count, resolved before rendering so the
    // minimized header row carries the badge too.
    let unseen = state.feed.unseen_attention_count();
    let (tab_title, click_cols) = build_tab_title(state, unseen);
    // Click ranges must track this frame's title (the badge changes the
    // Feed label's width); hit-testing reads the previous frame's values.
    state.layout.bottom_tab_click_cols = click_cols;

    let title_spans = tab_title.spans;
    let title_dw = title_spans
        .iter()
        .map(|span| display_width(&span.content))
        .sum::<usize>();
    // Row layout: `╭ ` + title + ` ` + fill + `╮`.
    let top_fill_len = (area.width as usize).saturating_sub(title_dw + 4);
    let mut top_line_spans = vec![Span::styled("╭ ", Style::default().fg(border_color))];
    top_line_spans.extend(title_spans);
    top_line_spans.push(Span::styled(
        format!(" {}╮", "─".repeat(top_fill_len)),
        Style::default().fg(border_color),
    ));
    let top_line = Line::from(top_line_spans);
    let top_rect = Rect::new(area.x, area.y, area.width, 1);

    // Minimized: collapse to just the tab header row. The Feed/Repos
    // labels stay visible and clickable so tabs can still be switched
    // without expanding the panel, and the badge keeps counting.
    if state.bottom_minimized {
        frame.render_widget(Paragraph::new(top_line), top_rect);
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    frame.render_widget(Paragraph::new(top_line), top_rect);

    let bottom_line = Line::from(Span::styled(
        format!("╰{}╯", "─".repeat((area.width as usize).saturating_sub(2))),
        Style::default().fg(border_color),
    ));
    let bottom_rect = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    frame.render_widget(Paragraph::new(bottom_line), bottom_rect);

    match state.bottom_tab {
        BottomTab::Feed => feed::draw_feed_content(frame, state, inner),
        BottomTab::Repos => repos::draw_repos_content(frame, state, inner),
    }

    // The feed was on screen this frame: everything it showed is seen.
    // Runs after rendering so the badge computed above still lands.
    if state.bottom_tab == BottomTab::Feed {
        state.feed.mark_seen();
    }
}

/// Clickable column ranges of the two tab labels, `(feed, repos)`, each
/// a half-open `(start, end)` pair of terminal columns.
pub type TabClickCols = ((usize, usize), (usize, usize));

/// Title spans for the two tab labels plus their clickable column ranges.
///
/// Layout: `╭ Feed[ ·N] │ Repos ╮` — the `╭ ` border span occupies
/// columns 0–1, so the Feed label starts at column 2. The ranges are
/// stored on `layout` for `handle_bottom_tab_click`; they cannot be
/// hardcoded because the Feed label's width varies with the unseen badge.
fn build_tab_title(state: &AppState, unseen: usize) -> (Line<'static>, TabClickCols) {
    let theme = &state.theme;
    let feed_style = if state.bottom_tab == BottomTab::Feed {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.text_muted)
    };

    let repos_style = if state.bottom_tab == BottomTab::Repos {
        Style::default().fg(theme.accent)
    } else {
        Style::default().fg(theme.text_muted)
    };

    let sep_style = Style::default().fg(theme.border_inactive);

    let mut feed_text = String::from("Feed");
    if unseen > 0 {
        feed_text.push_str(&format!(" ·{unseen}"));
    }
    // `╭ ` occupies columns 0–1.
    let feed_start = 2;
    let feed_end = feed_start + display_width(&feed_text);
    let sep = " │ ";
    let repos_start = feed_end + display_width(sep);
    let repos_text = "Repos";
    let repos_end = repos_start + display_width(repos_text);

    let spans = vec![
        Span::styled(feed_text, feed_style),
        Span::styled(sep.to_string(), sep_style),
        Span::styled(repos_text.to_string(), repos_style),
    ];
    (
        Line::from(spans),
        ((feed_start, feed_end), (repos_start, repos_end)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_title_ranges_track_badge_width() {
        let state = AppState::new("%99".into());

        let (line, cols) = build_tab_title(&state, 0);
        assert_eq!(line.spans[0].content, "Feed");
        assert_eq!(line.spans[1].content, " │ ");
        assert_eq!(line.spans[2].content, "Repos");
        // Title starts after `╭ `; "Feed" is 4 wide, sep 3, "Repos" 5.
        assert_eq!(cols, ((2, 6), (9, 14)));

        let (line, cols) = build_tab_title(&state, 2);
        assert_eq!(line.spans[0].content, "Feed ·2");
        // The badge widens the Feed label and shifts the Repos range.
        assert_eq!(cols, ((2, 9), (12, 17)));
    }
}
