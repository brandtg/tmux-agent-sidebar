use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::AppState;
use crate::ui::text::{display_width, wrap_flow};

/// Column width reserved for the agent label. `opencode` is the longest.
const AGENT_COL: usize = 8;

/// One row per agent pane, grouped under its repo: branch, upstream
/// movement, dirty count. Data comes from the worker-maintained
/// per-path git-info cache, so the tab costs the render thread nothing.
///
/// Rows for panes without resolvable git metadata (repo root and branch
/// both unknown — the resolver has not answered yet, or the pane sits in
/// a plain directory) are skipped; a repo group made entirely of those
/// renders no header either.
pub(super) fn draw_repos_content(frame: &mut Frame, state: &mut AppState, inner: Rect) {
    let theme = &state.theme;
    let inner_w = inner.width as usize;

    let mut lines: Vec<Line<'_>> = Vec::new();
    // One blank row above the first group for breathing room.
    lines.push(Line::from(""));

    let mut rendered_any = false;
    for group in &state.repo_groups {
        let rows: Vec<_> = group
            .panes
            .iter()
            .filter(|(_, git)| git.repo_root.is_some() || git.branch.is_some())
            .collect();
        if rows.is_empty() {
            continue;
        }
        rendered_any = true;

        lines.push(Line::from(Span::styled(
            group.name.clone(),
            Style::default().fg(theme.section_title),
        )));

        for (pane, git) in rows {
            let agent_label = pane.agent.label();
            let agent_pad = AGENT_COL.saturating_sub(display_width(agent_label));

            // Right cluster: upstream movement then dirty count, built
            // first so the left side knows its budget. Pieces are joined
            // by single spaces; `right_w` accounts for those separators.
            let mut pieces: Vec<(String, ratatui::style::Color)> = Vec::new();
            if let Some((ahead, behind)) = git.ahead_behind {
                if ahead > 0 {
                    pieces.push((format!("↑{ahead}"), theme.diff_added));
                }
                if behind > 0 {
                    pieces.push((format!("↓{behind}"), theme.diff_deleted));
                }
            }
            if git.dirty_count > 0 {
                pieces.push((format!("~{}", git.dirty_count), theme.text_muted));
            }
            let right_w = if pieces.is_empty() {
                0
            } else {
                pieces
                    .iter()
                    .map(|(text, _)| display_width(text))
                    .sum::<usize>()
                    + pieces.len()
                    - 1
            };

            let branch = git.branch.clone().unwrap_or_default();
            let branch_max = inner_w
                .saturating_sub(AGENT_COL + 1)
                .saturating_sub(right_w);
            let agent_span = Span::styled(
                agent_label.to_string(),
                Style::default().fg(theme.agent_color(&pane.agent)),
            );
            let pad_span = Span::raw(" ".repeat(agent_pad + 1));

            if display_width(&branch) <= branch_max {
                // Fits beside the right-aligned stats: single row.
                let branch_w = display_width(&branch);
                let mut spans = vec![
                    agent_span,
                    pad_span,
                    Span::styled(branch, Style::default().fg(theme.branch)),
                ];
                if !pieces.is_empty() {
                    let left_w = AGENT_COL + 1 + branch_w;
                    spans.push(Span::raw(
                        " ".repeat(inner_w.saturating_sub(left_w + right_w)),
                    ));
                    for (i, (text, color)) in pieces.into_iter().enumerate() {
                        if i > 0 {
                            spans.push(Span::raw(" "));
                        }
                        spans.push(Span::styled(text, Style::default().fg(color)));
                    }
                }
                lines.push(Line::from(spans));
            } else {
                // Too long to share the row with the stats: wrap the
                // branch across the full row width and set the stats
                // inline after the final chunk (own line when full).
                let flow_first = inner_w.saturating_sub(AGENT_COL + 1);
                let mut chunk_iter = wrap_flow(&branch, flow_first, inner_w).into_iter();
                // Rows built so far, with each row's current display width.
                let mut rows: Vec<(Vec<Span<'static>>, usize)> = Vec::new();
                let mut head = vec![agent_span, pad_span];
                let mut head_w = AGENT_COL + 1;
                if flow_first > 0
                    && let Some(first) = chunk_iter.next()
                {
                    head_w += display_width(&first);
                    head.push(Span::styled(first, Style::default().fg(theme.branch)));
                }
                rows.push((head, head_w));
                for chunk in chunk_iter {
                    let w = display_width(&chunk);
                    rows.push((
                        vec![Span::styled(chunk, Style::default().fg(theme.branch))],
                        w,
                    ));
                }
                for (text, color) in &pieces {
                    let piece_w = display_width(text) + 1; // separator space
                    if let Some((spans, w)) = rows.last_mut()
                        && *w + piece_w <= inner_w
                    {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(text.clone(), Style::default().fg(*color)));
                        *w += piece_w;
                    } else {
                        rows.push((
                            vec![Span::styled(text.clone(), Style::default().fg(*color))],
                            piece_w - 1,
                        ));
                    }
                }
                for (spans, _) in rows {
                    lines.push(Line::from(spans));
                }
            }
        }
    }

    if !rendered_any {
        super::render_centered(frame, inner, "No agents running", theme.text_muted);
        return;
    }

    state.scrolls.repos.total_lines = lines.len();
    state.scrolls.repos.visible_height = inner.height as usize;
    // Clamp `offset` to the new viewport so shrinking content (an agent
    // closing mid-scroll) cannot leave the paragraph past its last line.
    state.scrolls.repos.scroll(0);

    let scroll_offset = state.scrolls.repos.offset as u16;
    let paragraph = Paragraph::new(lines).scroll((scroll_offset, 0));
    frame.render_widget(paragraph, inner);
}
