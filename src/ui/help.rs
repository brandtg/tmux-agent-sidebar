use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::state::AppState;
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
            ("h / l", "Status filter"),
            ("Tab", "Cycle status filter"),
            ("[ / ]", "Prev / next group"),
            ("z", "Collapse / expand group"),
            ("Z", "Expand all groups"),
            ("s", "Repo / session grouping"),
            ("Enter / →", "Jump to selected pane"),
            ("n", "Spawn worktree + agent"),
            ("x", "Remove spawn pane"),
            ("f", "Flag / unflag agent"),
            ("r", "Open repo filter"),
        ],
    ),
    (
        "todos",
        &[
            ("Enter / e", "Edit the selected task"),
            ("Enter (editing)", "Save, then next task"),
            ("a", "New task (insert mode)"),
            ("Space", "Complete / uncomplete"),
            ("Ctrl-↓ / Ctrl-j", "Move task down"),
            ("Ctrl-↑ / Ctrl-k", "Move task up"),
            ("d / x", "Delete the selected task"),
            ("D", "Clear completed"),
            ("u / Ctrl-z", "Undo delete/clear (4 s)"),
            ("H", "Show / hide completed"),
            ("Esc", "Cancel / back to list"),
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

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (section, rows) in HELP_SECTIONS {
        if !lines.is_empty() {
            lines.push(Line::from(Span::raw("")));
        }
        lines.push(Line::from(Span::styled(
            format!(" {section}"),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )));
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
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(text_active)),
                    Span::styled(chunk, Style::default().fg(text_muted)),
                ]));
            }
        }
    }

    let content = lines.len() as u16;
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
    frame.render_widget(block, area);

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
        │ h / l            Status filter           │
        │ Tab              Cycle status filter     │
        │ [ / ]            Prev / next group       │
        │ z                Collapse / expand group │
        │ Z                Expand all groups       │
        │ s                Repo / session grouping │
        │ Enter / →        Jump to selected pane   │
        │ n                Spawn worktree + agent  │
        │ x                Remove spawn pane       │
        │ f                Flag / unflag agent     │
        │ r                Open repo filter        │
        │                                          │
        │ todos                                    │
        │ Enter / e        Edit the selected task  │
        │ Enter (editing)  Save, then next task    │
        │ a                New task (insert mode)  │
        │ Space            Complete / uncomplete   │
        │ Ctrl-↓ / Ctrl-j  Move task down          │
        │ Ctrl-↑ / Ctrl-k  Move task up            │
        │ d / x            Delete the selected task│
        │ D                Clear completed         │
        │ u / Ctrl-z       Undo delete/clear (4 s) │
        │ H                Show / hide completed   │
        │ Esc              Cancel / back to list   │
        │                                          │
        │ global                                   │
        │ t                Open / close TODO panel │
        │ c                Toggle compact view     │
        │ w                List non-agent panes    │
        │ ?                Toggle this help        │
        │ q                Quit                    │
        │                                          │
        │                                          │
        │                                          │
        │                                          │
        │                                          │
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
        │ h / l        Status    │
        │              filter    │
        │ Tab          Cycle     │
        │              status    │
        │              filter    │
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
        │ Esc              Cancel / back to list   │
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
