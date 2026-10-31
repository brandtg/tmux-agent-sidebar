use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::tail_fit;
use crate::state::{AppState, Focus, TodoRowTarget};
use crate::ui::text::{display_width, truncate_to_width, wrap_flow};

/// Maximum rows the panel may occupy, including its header. Kept small so
/// the agent list keeps most of the sidebar even on a short terminal.
const MAX_PANEL_HEIGHT: u16 = 10;
/// Tighter cap for the narrow popup viewport (phones).
const POPUP_MAX_PANEL_HEIGHT: u16 = 6;
/// Columns used by the `[ ]` / `[✓]` marker plus one separating space.
const CHECKBOX_WIDTH: usize = 4;

/// Rows the panel needs, clamped to its cap and to the space left after
/// the two header rows and at least one agent row.
pub(super) fn panel_height(state: &AppState, area: Rect) -> u16 {
    if !state.todos.visible {
        return 0;
    }
    let width = area.width as usize;
    let text_width = width.saturating_sub(CHECKBOX_WIDTH).max(1);
    let visible = state
        .todos
        .visible_indices(state.global.todo_hide_completed);

    let mut rows = 1usize; // header
    if state.todos.error.is_some() {
        rows += 1;
    }
    if visible.is_empty() {
        rows += 1; // empty-state hint
    } else {
        for &index in &visible {
            rows += wrap_flow(&state.todos.items[index].text, text_width, text_width)
                .len()
                .max(1);
        }
    }
    if state.todos.adding.is_some() || state.todos.editing.is_some() {
        rows += 1;
    }

    let cap = if state.popup_mode {
        POPUP_MAX_PANEL_HEIGHT
    } else {
        MAX_PANEL_HEIGHT
    };
    (rows as u16).min(cap).min(area.height.saturating_sub(3))
}

/// Render the TODO panel into `area` (already sized by [`panel_height`]).
pub(super) fn render_todo_panel(frame: &mut Frame, state: &mut AppState, area: Rect) {
    state.layout.todo_row_targets.clear();
    state.layout.todo_input_rect = None;
    if area.height == 0 || area.width == 0 {
        return;
    }

    let width = area.width as usize;
    let hide_completed = state.global.todo_hide_completed;
    let visible = state.todos.visible_indices(hide_completed);
    let selected = state.todos.selected;
    let panel_focused = state.focus_state.focus == Focus::Todos;
    let theme = &state.theme;

    let muted = Style::default().fg(theme.text_muted);
    let accent = Style::default().fg(theme.accent);
    let error_style = Style::default().fg(theme.status_error);
    let text_style = Style::default().fg(theme.text_active);
    let done_text_style = Style::default()
        .fg(theme.text_muted)
        .add_modifier(Modifier::CROSSED_OUT);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // (item index, first line index, last line index) for click targets
    // and selection auto-scroll.
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();

    // Header: a muted separator carrying the counts; it lights up in the
    // accent color while the panel owns the keyboard so it is obvious
    // whether the agent list or the TODO list is active.
    let header_style = if panel_focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        muted
    };
    let mut header = format!(
        "─ todos · {}/{}",
        state.todos.open_count(),
        state.todos.total_count()
    );
    if hide_completed {
        let candidate = format!("{header} · hide done");
        if display_width(&candidate) <= width {
            header = candidate;
        }
    }
    let header = truncate_to_width(&header, width);
    let fill = width.saturating_sub(display_width(&header));
    lines.push(Line::from(vec![
        Span::styled(header, header_style),
        Span::styled("─".repeat(fill), header_style),
    ]));

    if let Some(err) = &state.todos.error {
        lines.push(Line::from(Span::styled(
            truncate_to_width(&format!("  ! {err}"), width),
            error_style,
        )));
    }

    if visible.is_empty() {
        let hint = if state.todos.items.is_empty() {
            "  (no todos — press a to add)"
        } else {
            "  (no open todos — H shows completed)"
        };
        lines.push(Line::from(Span::styled(
            truncate_to_width(hint, width),
            muted,
        )));
    }

    let text_width = width.saturating_sub(CHECKBOX_WIDTH).max(1);
    for &index in &visible {
        let item = &state.todos.items[index];
        let is_selected = panel_focused && index == selected;
        let checkbox = if item.done { "[✓]" } else { "[ ]" };
        let checkbox_style = if item.done { muted } else { accent };
        let body_style = if item.done {
            done_text_style
        } else {
            text_style
        };
        let row_style = if is_selected {
            body_style.bg(theme.selection_bg)
        } else {
            body_style
        };
        let checkbox_row_style = if is_selected {
            checkbox_style.bg(theme.selection_bg)
        } else {
            checkbox_style
        };

        let wrapped = wrap_flow(&item.text, text_width, text_width);
        let wrapped = if wrapped.is_empty() {
            vec![String::new()]
        } else {
            wrapped
        };
        let first_line = lines.len();
        for (i, chunk) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{checkbox} ")
            } else {
                " ".repeat(CHECKBOX_WIDTH)
            };
            let prefix = truncate_to_width(&prefix, width);
            let prefix_width = display_width(&prefix);
            let mut spans = vec![
                Span::styled(prefix, checkbox_row_style),
                Span::styled(chunk.clone(), row_style),
            ];
            let used = prefix_width + display_width(chunk);
            if is_selected && used < width {
                spans.push(Span::styled(
                    " ".repeat(width - used),
                    Style::default().bg(theme.selection_bg),
                ));
            }
            lines.push(Line::from(spans));
        }
        ranges.push((index, first_line, lines.len() - 1));
    }

    // Inline add/edit line.
    let mut input_line: Option<usize> = None;
    if let Some(edit) = &state.todos.editing {
        input_line = Some(lines.len());
        lines.push(Line::from(vec![
            Span::styled("✎ ", accent),
            Span::styled(tail_fit(&edit.input, width.saturating_sub(2)), text_style),
            Span::styled("█", accent),
        ]));
    } else if let Some(input) = &state.todos.adding {
        input_line = Some(lines.len());
        lines.push(Line::from(vec![
            Span::styled("+ ", accent),
            Span::styled(tail_fit(input, width.saturating_sub(2)), text_style),
            Span::styled("█", accent),
        ]));
    }

    let visible_height = area.height as usize;
    state.scrolls.todos.total_lines = lines.len();
    state.scrolls.todos.visible_height = visible_height;
    state.scrolls.todos.scroll(0);

    if panel_focused && let Some((_, start, end)) = ranges.iter().find(|(i, _, _)| *i == selected) {
        let offset = state.scrolls.todos.offset;
        if *start < offset {
            state.scrolls.todos.offset = *start;
        } else if *end >= offset + visible_height {
            state.scrolls.todos.offset = (end + 1).saturating_sub(visible_height);
        }
    }
    // Keep the add/edit line visible while typing.
    if input_line.is_some() && lines.len() > state.scrolls.todos.offset + visible_height {
        state.scrolls.todos.offset = lines.len() - visible_height;
    }

    let offset = state.scrolls.todos.offset;
    let paragraph = Paragraph::new(lines).scroll((offset as u16, 0));
    frame.render_widget(paragraph, area);

    // Click targets, clipped to the visible window.
    for (index, start, end) in ranges {
        let first = start.max(offset);
        let last = end.min(offset + visible_height.saturating_sub(1));
        if first > last {
            continue;
        }
        state.layout.todo_row_targets.push(TodoRowTarget {
            rect: Rect::new(
                area.x,
                area.y + (first - offset) as u16,
                area.width,
                (last - first + 1) as u16,
            ),
            index,
        });
    }

    if let Some(index) = input_line
        && index >= offset
        && index < offset + visible_height
    {
        state.layout.todo_input_rect = Some(Rect::new(
            area.x,
            area.y + (index - offset) as u16,
            area.width,
            1,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo::TodoItem;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn todo_state() -> AppState {
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![
            TodoItem {
                text: "Buy milk".into(),
                done: false,
            },
            TodoItem {
                text: "Send the invoice".into(),
                done: true,
            },
            TodoItem {
                text: "Read the tmux changelog and try the new paste behaviour".into(),
                done: false,
            },
        ];
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state
    }

    fn render_panel(state: &mut AppState, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_todo_panel(frame, state, area))
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
    fn hidden_panel_needs_no_rows() {
        let state = AppState::new("%99".into());
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20)), 0);
    }

    #[test]
    fn panel_renders_open_and_completed_items() {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] Buy milk
        [✓] Send the invoice
        [ ] Read the tmux changelog and try the
            new paste behaviour
        ");
    }

    #[test]
    fn hide_completed_removes_finished_items() {
        let mut state = todo_state();
        state.global.todo_hide_completed = true;
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3 · hide done───────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        ");
    }

    #[test]
    fn long_items_wrap_and_indent_under_the_text() {
        let mut state = todo_state();
        state.global.todo_hide_completed = true;
        state.todos.selected = 2;
        let output = render_panel(&mut state, 24, 10);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────
        [ ] Buy milk
        [ ] Read the tmux
            changelog and try
            the new paste
            behaviour
        ");
    }

    #[test]
    fn add_line_renders_with_a_cursor() {
        let mut state = todo_state();
        state.begin_todo_add();
        for c in "new task".chars() {
            state.todo_input_push_char(c);
        }
        let output = render_panel(&mut state, 40, 10);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3 · hide done───────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        + new task█
        ");
    }

    #[test]
    fn edit_line_shows_the_prefilled_text() {
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3 · hide done───────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        ✎ Buy milk█
        ");
    }

    #[test]
    fn empty_list_shows_the_add_hint() {
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ todos · 0/0 · hide done───────────────
          (no todos — press a to add)
        ");
    }

    #[test]
    fn all_completed_items_hidden_show_the_open_hint() {
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![TodoItem {
            text: "done thing".into(),
            done: true,
        }];
        state.todos.visible = true;
        state.global.todo_hide_completed = true;
        state.focus_state.focus = Focus::Todos;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ todos · 0/1 · hide done───────────────
          (no open todos — H shows completed)
        ");
    }

    #[test]
    fn rendered_items_register_click_targets() {
        let mut state = todo_state();
        state.global.todo_hide_completed = true;
        render_panel(&mut state, 40, 10);

        assert_eq!(state.layout.todo_row_targets.len(), 2);
        assert_eq!(state.layout.todo_row_targets[0].index, 0);
        assert_eq!(state.layout.todo_row_targets[1].index, 2);
        assert!(
            state.layout.todo_row_targets[1].rect.height > 1,
            "the wrapped item's rect must span every line"
        );
    }
}
