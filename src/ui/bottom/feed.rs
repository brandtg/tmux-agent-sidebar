use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::AppState;
use crate::ui::text::{display_width, wrap_flow};

/// Column width reserved for the agent label. `opencode` is the longest.
const AGENT_COL: usize = 8;

pub(super) fn draw_feed_content(frame: &mut Frame, state: &mut AppState, inner: Rect) {
    let theme = &state.theme;

    if state.feed.entries.is_empty() {
        super::render_centered(frame, inner, "No recent activity", theme.text_muted);
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::new();
    let inner_w = inner.width as usize;

    // Leave one blank row above the first entry for breathing room,
    // matching the old per-pane Activity tab.
    lines.push(Line::from(""));

    for entry in &state.feed.entries {
        let ts_dw = display_width(&entry.timestamp);
        let agent_label = entry.agent.label();
        let agent_pad = AGENT_COL.saturating_sub(display_width(agent_label));
        let icon = entry.kind.icon();

        // ts + two spaces + agent column + one space + icon + one space.
        let prefix_w = ts_dw + 2 + AGENT_COL + 1 + display_width(icon) + 1;
        let first_max = inner_w.saturating_sub(prefix_w);
        // Flow the label across the full row width: the first line shares
        // the row with the timestamp/agent/icon prefix, and wrapped lines
        // continue at the panel edge with every column available. When the
        // prefix already fills the row (first_max == 0) the label moves
        // entirely to the continuation lines.
        let chunks = wrap_flow(&entry.label, first_max, inner_w);
        let mut chunks = chunks.into_iter();

        let mut spans = vec![
            Span::styled(
                entry.timestamp.clone(),
                Style::default().fg(theme.activity_timestamp),
            ),
            Span::raw("  "),
            Span::styled(
                agent_label.to_string(),
                Style::default().fg(theme.agent_color(&entry.agent)),
            ),
            Span::raw(" ".repeat(agent_pad + 1)),
            Span::styled(
                icon.to_string(),
                Style::default().fg(Color::Indexed(entry.kind.color_index())),
            ),
        ];
        if first_max > 0
            && let Some(first) = chunks.next()
            && !first.is_empty()
        {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(first, Style::default().fg(theme.text_active)));
        }
        lines.push(Line::from(spans));

        for chunk in chunks {
            lines.push(Line::from(vec![Span::styled(
                chunk,
                Style::default().fg(theme.text_active),
            )]));
        }
    }

    state.feed.scroll.total_lines = lines.len();
    state.feed.scroll.visible_height = inner.height as usize;
    // Clamp `offset` to the new total/visible. When entries shrink (log
    // trim, pane removal) the stale offset would otherwise produce an
    // empty or over-scrolled paragraph.
    state.feed.scroll.scroll(0);

    let scroll_offset = state.feed.scroll.offset as u16;
    let paragraph = Paragraph::new(lines).scroll((scroll_offset, 0));
    frame.render_widget(paragraph, inner);
}
