use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::state::AppState;
use crate::ui::text::{display_width, truncate_to_width};

/// Rows of the `?` help popup: `(key, action)`. The ordering and wording
/// follow the keybindings doc so the two can be diffed by eye.
const HELP_ROWS: &[(&str, &str)] = &[
    ("j / k", "Move selection"),
    ("/", "Search agents"),
    ("PgUp / PgDn", "Page the list"),
    ("h / l", "Previous / next status filter"),
    ("Tab", "Cycle status filter"),
    ("Enter / →", "Jump to the selected pane"),
    ("n", "Spawn worktree + agent"),
    ("x", "Remove the selected spawn pane"),
    ("f", "Flag / unflag the selected agent"),
    ("r", "Open repo filter"),
    ("c", "Toggle compact view"),
    ("w", "List non-agent panes"),
    ("Esc", "Return focus / close popup"),
    ("?", "Toggle this help"),
    ("q", "Quit"),
];

/// Read-only key reference centered on the pane area, opened with `?`.
/// No clickable content, so nothing besides the rendered rect is stored.
pub(in crate::ui) fn render_help_popup(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let theme = &state.theme;

    let key_w = HELP_ROWS
        .iter()
        .map(|(key, _)| display_width(key))
        .max()
        .unwrap_or(1);
    let action_w = HELP_ROWS
        .iter()
        .map(|(_, action)| display_width(action))
        .max()
        .unwrap_or(1);
    // Column layout inside the borders: ` key<key_w>  action `.
    let inner_w = 1 + key_w + 2 + action_w + 1;
    // Borders (2) on top of the content; clamp LAST to the pane area so a
    // narrow sidebar gets a clamped popup, not an overflowing one.
    let popup_width = ((inner_w as u16).saturating_add(2).max(12)).min(area.width);
    let lines = HELP_ROWS.len() as u16 + 1; // +1 for the title row
    let popup_height = ((lines + 2).max(3)).min(area.height);

    let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
    state.popup.set_help_area(Some(popup_rect));

    frame.render_widget(Clear, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let inner_width = inner.width as usize;
    let title = truncate_to_width("Help", inner_width.saturating_sub(1));
    let title_pad = " ".repeat(inner_width.saturating_sub(display_width(&title)));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {title}{title_pad}"),
            Style::default().fg(theme.text_active),
        ))),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );

    for (i, (key, action)) in HELP_ROWS.iter().enumerate() {
        let row_y = inner.y + 1 + i as u16;
        if row_y >= inner.y + inner.height {
            break;
        }
        let key_col = format!("{key:<key_w$}");
        let action = truncate_to_width(
            action,
            inner_width.saturating_sub(1 + display_width(&key_col) + 2),
        );
        let pad =
            " ".repeat(inner_width.saturating_sub(display_width(&key_col) + 2 + action.len()));
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {key_col}  "),
                    Style::default().fg(theme.text_active),
                ),
                Span::styled(
                    format!("{action}{pad}"),
                    Style::default().fg(theme.text_muted),
                ),
            ])),
            Rect::new(inner.x, row_y, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render_help_text(width: u16, height: u16) -> String {
        let mut state = AppState::new("%99".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_help_popup(frame, &mut state, area);
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
    fn help_popup_renders_centered_key_reference() {
        let output = render_help_text(60, 20);
        insta::assert_snapshot!(output, @"

        ┌───────────────────────────────────────────────┐
        │ Help                                          │
        │ j / k        Move selection                   │
        │ /            Search agents                    │
        │ PgUp / PgDn  Page the list                    │
        │ h / l        Previous / next status filter    │
        │ Tab          Cycle status filter              │
        │ Enter / →    Jump to the selected pane        │
        │ n            Spawn worktree + agent           │
        │ x            Remove the selected spawn pane   │
        │ f            Flag / unflag the selected agent │
        │ r            Open repo filter                 │
        │ c            Toggle compact view              │
        │ w            List non-agent panes             │
        │ Esc          Return focus / close popup       │
        │ ?            Toggle this help                 │
        │ q            Quit                             │
        └───────────────────────────────────────────────┘
        ");
    }

    #[test]
    fn help_popup_clamps_to_narrow_area() {
        // A narrow pane must not push the popup past the buffer.
        let output = render_help_text(20, 10);
        insta::assert_snapshot!(output, @"
        ┌──────────────────┐
        │ Help             │
        │ j / k        Mov…│
        │ /            Sea…│
        │ PgUp / PgDn  Pag…│
        │ h / l        Pre…│
        │ Tab          Cyc…│
        │ Enter / →    Jum…│
        │ n            Spa…│
        └──────────────────┘
        ");
    }
}
