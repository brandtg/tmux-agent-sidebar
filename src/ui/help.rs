use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::state::AppState;
use crate::state::HelpCachedLine;
use crate::ui::paint_background;
use crate::ui::text::{display_width, truncate_to_width, wrap_flow};

/// The `?` overlay's content, grouped like Google's shortcut sheet: a
/// section name, then `(key, action)` rows. The TODO section describes
/// the Google Tasks flow — Enter edits, Enter again starts the next
/// task, Space completes.
const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "list",
        &[
            ("j / k / ↑ / ↓", "Move selection"),
            ("g / G", "Jump to top / bottom"),
            ("PgUp / PgDn", "Page the list"),
            ("a", "Next attention row"),
            ("/", "Search agents"),
            ("[ / ]", "Prev / next group"),
            ("z", "Collapse / expand group (or click header)"),
            ("Z", "Collapse / expand all groups"),
            ("s", "Repo / session grouping"),
            ("Enter / →", "Jump to selected pane"),
            ("n", "Spawn agent (folder or worktree)"),
            ("x", "Remove spawn pane"),
            ("f", "Flag / unflag agent"),
            ("r", "Open repo menu"),
            ("o", "Agent overview grid (popup)"),
        ],
    ),
    (
        "todos",
        &[
            ("Enter / e", "Edit the selected task"),
            ("Enter (editing)", "Save, then next task"),
            ("a", "New task (insert mode)"),
            ("Space", "Complete / uncomplete"),
            ("y", "Copy the selected task"),
            ("r", "Launch resume command"),
            ("Ctrl-↓ / Ctrl-j", "Move task down"),
            ("Ctrl-↑ / Ctrl-k", "Move task up"),
            ("d / x", "Delete the selected task"),
            ("D", "Clear completed"),
            ("u / Ctrl-z", "Undo delete/clear (4 s)"),
            ("H", "Show / hide completed"),
            ("Esc", "Save / back to list"),
        ],
    ),
    (
        "global",
        &[
            ("t", "Open / close TODO panel"),
            ("c", "Toggle compact view"),
            ("w", "List non-agent panes"),
            ("?", "Toggle this help"),
            ("q", "Quit"),
        ],
    ),
];

/// Minimum description columns the key column must leave behind before
/// it starts truncating keys. Below this the sidebar is narrower than
/// any key reference can help with anyway.
const MIN_ACTION_WIDTH: usize = 10;

/// Wrap the whole reference into cacheable lines at this geometry: a
/// section header, one entry per wrapped description chunk (the
/// continuation lines carry an all-space prefix that aligns under the
/// description column), and blank separators between sections. The
/// theme is applied when the renderer materializes the ratatui lines —
/// the cache stays style-free (audit 2026-11-25 L-UI-7: the old render
/// re-wrapped every description every frame the overlay stayed open).
fn build_help_lines(key_col: usize, action_width: usize) -> Vec<HelpCachedLine> {
    let mut lines: Vec<HelpCachedLine> = Vec::new();
    for (section, rows) in HELP_SECTIONS {
        if !lines.is_empty() {
            lines.push(HelpCachedLine::Blank);
        }
        lines.push(HelpCachedLine::Header(format!(" {section}")));
        for (key, action) in rows.iter() {
            let key_text = truncate_to_width(key, key_col);
            let key_pad = " ".repeat(key_col.saturating_sub(display_width(&key_text)));
            let mut wrapped = wrap_flow(action, action_width, action_width);
            if wrapped.is_empty() {
                wrapped.push(String::new());
            }
            for (i, chunk) in wrapped.into_iter().enumerate() {
                let prefix = if i == 0 {
                    format!(" {key_text}{key_pad}  ")
                } else {
                    // Hanging indent: continuation lines align under the
                    // description column, never under the key.
                    " ".repeat(key_col + 3)
                };
                lines.push(HelpCachedLine::Entry { prefix, chunk });
            }
        }
    }
    lines
}

/// Read-only key reference rendered over the whole sidebar, opened with
/// `?`. The sidebar is a narrow column: a centered box would truncate
/// every description on the right, so the overlay spans the full width,
/// wraps descriptions with a hanging indent, and scrolls (`j` / `k`,
/// PgUp / PgDn) when the terminal is too short for all of it. No
/// clickable content, so nothing besides the rendered rect is stored.
pub(in crate::ui) fn render_help_popup(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // Copy the palette out of `state` first: the metrics write-back below
    // needs `&mut state` while the styles still reference the theme.
    let (accent, text_active, text_muted) = {
        let theme = &state.theme;
        (theme.accent, theme.text_active, theme.text_muted)
    };
    state.popup.set_help_area(Some(area));

    let inner_width = area.width.saturating_sub(2) as usize;

    // The key column sizes itself from the widest key, capped so a
    // clamped overlay always keeps a usable description column.
    let widest_key = HELP_SECTIONS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(key, _)| display_width(key))
        .max()
        .unwrap_or(1);
    let key_col = widest_key.min(inner_width.saturating_sub(3 + MIN_ACTION_WIDTH).max(1));
    // The row prefix is ` key<pad>  ` = key_col + 3 columns.
    let action_width = inner_width.saturating_sub(key_col + 3).max(1);

    // Wrapped content, memoized per geometry: the wrap is pure output of
    // (key_col, action_width), so a resize is the only invalidation.
    if !state.help_cache_matches(key_col, action_width) {
        let lines = build_help_lines(key_col, action_width);
        state.store_help_cache(key_col, action_width, lines);
    }
    let content = state.help_cache_len(key_col, action_width);

    let viewport = area.height.saturating_sub(2);
    // Write the metrics back first: the write-back re-clamps scroll
    // against the fresh layout (a resize can shrink the content).
    state.set_help_metrics(content, viewport);
    let scrollable = content > viewport;
    let scroll = state.help_scroll();

    let mut title = "shortcuts · esc to close".to_string();
    if scrollable {
        title.push_str(" · j/k scrolls");
    }
    let title = truncate_to_width(&title, inner_width.max(1));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent),
        ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    paint_background(frame, &state.theme, area);
    frame.render_widget(block, area);

    // Materialize the styled lines from the cache: the strings are only
    // referenced, so nothing is re-wrapped or re-allocated here.
    let lines: Vec<Line<'_>> = state
        .help_cache(key_col, action_width)
        .unwrap_or(&[])
        .iter()
        .map(|line| match line {
            HelpCachedLine::Blank => Line::from(Span::raw("")),
            HelpCachedLine::Header(text) => Line::from(Span::styled(
                text.as_str(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
            HelpCachedLine::Entry { prefix, chunk } => Line::from(vec![
                Span::styled(prefix.as_str(), Style::default().fg(text_active)),
                Span::styled(chunk.as_str(), Style::default().fg(text_muted)),
            ]),
        })
        .collect();

    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_help_text(state: &mut AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_help_popup(frame, state, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let area = buf.area;
        let mut lines = Vec::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    #[test]
    fn overlay_spans_the_full_sidebar_without_truncation() {
        // At the sidebar's typical width every description renders in
        // full — the old centered box truncated them on the right.
        let mut state = AppState::new("%99".into());
        state.toggle_help_popup();
        let output = render_help_text(&mut state, 44, 44);
        insta::assert_snapshot!(output, @"
        ╭ shortcuts · esc to close ────────────────╮
        │ list                                     │
        │ j / k / ↑ / ↓    Move selection          │
        │ g / G            Jump to top / bottom    │
        │ PgUp / PgDn      Page the list           │
        │ a                Next attention row      │
        │ /                Search agents           │
        │ [ / ]            Prev / next group       │
        │ z                Collapse / expand group │
        │                  (or click header)       │
        │ Z                Collapse / expand all   │
        │                  groups                  │
        │ s                Repo / session grouping │
        │ Enter / →        Jump to selected pane   │
        │ n                Spawn agent (folder or  │
        │                  worktree)               │
        │ x                Remove spawn pane       │
        │ f                Flag / unflag agent     │
        │ r                Open repo menu          │
        │ o                Agent overview grid     │
        │                  (popup)                 │
        │                                          │
        │ todos                                    │
        │ Enter / e        Edit the selected task  │
        │ Enter (editing)  Save, then next task    │
        │ a                New task (insert mode)  │
        │ Space            Complete / uncomplete   │
        │ y                Copy the selected task  │
        │ r                Launch resume command   │
        │ Ctrl-↓ / Ctrl-j  Move task down          │
        │ Ctrl-↑ / Ctrl-k  Move task up            │
        │ d / x            Delete the selected task│
        │ D                Clear completed         │
        │ u / Ctrl-z       Undo delete/clear (4 s) │
        │ H                Show / hide completed   │
        │ Esc              Save / back to list     │
        │                                          │
        │ global                                   │
        │ t                Open / close TODO panel │
        │ c                Toggle compact view     │
        │ w                List non-agent panes    │
        │ ?                Toggle this help        │
        │ q                Quit                    │
        ╰──────────────────────────────────────────╯
        ");
    }

    #[test]
    fn narrow_sidebar_wraps_descriptions_with_a_hanging_indent() {
        // Half the width: long descriptions wrap onto continuation lines
        // aligned under the description column instead of truncating.
        let mut state = AppState::new("%99".into());
        state.toggle_help_popup();
        let output = render_help_text(&mut state, 26, 20);
        insta::assert_snapshot!(output, @"
        ╭ shortcuts · esc to clos╮
        │ list                   │
        │ j / k / ↑ …  Move      │
        │              selection │
        │ g / G        Jump to   │
        │              top /     │
        │              bottom    │
        │ PgUp / PgDn  Page the  │
        │              list      │
        │ a            Next      │
        │              attention │
        │              row       │
        │ /            Search    │
        │              agents    │
        │ [ / ]        Prev /    │
        │              next group│
        │ z            Collapse  │
        │              / expand  │
        │              group (or │
        ╰────────────────────────╯
        ");
    }

    #[test]
    fn overlay_scrolls_when_the_terminal_is_short() {
        let mut state = AppState::new("%99".into());
        state.toggle_help_popup();
        let first = render_help_text(&mut state, 44, 12);
        assert!(
            first.contains("j/k scrolls"),
            "clipped content advertises scrolling in the title"
        );
        // The renderer wrote back the metrics; scroll to the end and
        // re-render: the tail sections come into view.
        state.scroll_help(100);
        let last = render_help_text(&mut state, 44, 12);
        insta::assert_snapshot!(last, @"
        ╭ shortcuts · esc to close · j/k scrolls ──╮
        │ u / Ctrl-z       Undo delete/clear (4 s) │
        │ H                Show / hide completed   │
        │ Esc              Save / back to list     │
        │                                          │
        │ global                                   │
        │ t                Open / close TODO panel │
        │ c                Toggle compact view     │
        │ w                List non-agent panes    │
        │ ?                Toggle this help        │
        │ q                Quit                    │
        ╰──────────────────────────────────────────╯
        ");
    }
}
