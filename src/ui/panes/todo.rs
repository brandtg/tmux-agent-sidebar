use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::tail_fit;
use crate::state::{AppState, Focus, TodoRowTarget};
use crate::ui::colors::shade_color;
use crate::ui::text::{display_width, truncate_to_width, wrap_flow};

/// Maximum rows the panel may occupy, including its header. Kept small so
/// the agent list keeps most of the sidebar even on a short terminal.
const MAX_PANEL_HEIGHT: u16 = 10;
/// Tighter cap for the narrow popup viewport (phones).
const POPUP_MAX_PANEL_HEIGHT: u16 = 6;
/// Columns used by the `[ ]` / `[✓]` marker plus one separating space.
const CHECKBOX_WIDTH: usize = 4;
/// Private-use marker spliced into the edit buffer at the cursor before
/// wrapping, swapped for the visible block glyph afterwards so a `█`
/// typed into the todo text can never be mistaken for the cursor.
const CURSOR_MARK: char = '\u{E000}';

/// Wrap the live edit buffer like any other item text, returning each
/// display line plus the char position of the cursor on its line
/// (`None` when the cursor falls elsewhere). The cursor occupies one
/// extra display column between the chars it sits between, so a line
/// that was exactly full gains a continuation line for the glyph.
fn wrap_with_cursor(input: &str, cursor: usize, width: usize) -> Vec<(String, Option<usize>)> {
    let mut marked = String::with_capacity(input.len() + 4);
    let cursor = cursor.min(input.chars().count());
    for (i, ch) in input.chars().enumerate() {
        if i == cursor {
            marked.push(CURSOR_MARK);
        }
        marked.push(ch);
    }
    if cursor >= input.chars().count() {
        marked.push(CURSOR_MARK);
    }
    wrap_flow(&marked, width, width)
        .into_iter()
        .map(|line| {
            let position = line.chars().position(|c| c == CURSOR_MARK);
            let line = line.chars().filter(|&c| c != CURSOR_MARK).collect();
            (line, position)
        })
        .collect()
}

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
    for &index in &visible {
        let text_rows = match state
            .todos
            .editing
            .as_ref()
            .filter(|edit| edit.index == index)
        {
            // The live buffer carries one extra cursor column, which
            // can spill onto its own line.
            Some(edit) => wrap_with_cursor(&edit.input, edit.cursor, text_width).len(),
            None => wrap_flow(&state.todos.items[index].text, text_width, text_width).len(),
        };
        rows += text_rows.max(1);
    }
    if state.todos.adding.is_some() {
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

    // Accent discipline: the accent is a focused-affordance only. At rest
    // the panel's chrome — header separator, checkbox column, done
    // checkboxes — sits in the border gray; open text keeps its full
    // brightness so the work list still reads.
    let chrome = Style::default().fg(theme.border_inactive);
    let accent = Style::default().fg(theme.accent);
    let error_style = Style::default().fg(theme.status_error);
    let text_style = Style::default().fg(theme.text_active);
    // The completed block is the graveyard: struck through, and one step
    // dimmer (text_inactive) while the panel owns the keyboard so the
    // open block keeps the eye.
    let done_text_style = Style::default()
        .fg(if panel_focused {
            theme.text_inactive
        } else {
            theme.text_muted
        })
        .add_modifier(Modifier::CROSSED_OUT);
    // Row washes sit well below the shared selection gray: the todo rows
    // paint their background edge to edge (unlike the agent list, where
    // selection_bg only covers the text spans), so the palette value
    // reads as a bright band. Both washes are derived so tmux theme
    // overrides still land.
    let row_bg = shade_color(theme.selection_bg, 0.35);
    // Edit wash: the accent darkened toward black — a faint steel-blue
    // that still reads as the accent hue next to the selection gray, so
    // "this row is being edited" is visible without a white row
    // flashing on every Enter.
    let edit_wash = shade_color(theme.accent, 0.5);

    let mut lines: Vec<Line<'static>> = Vec::new();
    // (item index, first line index, last line index) for click targets
    // and selection auto-scroll.
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();

    // Header: the separator carries the counts. At rest it sinks to the
    // border gray; while the panel owns the keyboard it lights up in the
    // accent so it is obvious whether the agent list or the TODO list is
    // active.
    let header_style = if panel_focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        chrome
    };
    let header = format!(
        "─ todos · {}/{}",
        state.todos.open_count(),
        state.todos.total_count()
    );
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

    let text_width = width.saturating_sub(CHECKBOX_WIDTH).max(1);
    for &index in &visible {
        let item = &state.todos.items[index];
        let is_selected = panel_focused && index == selected;
        let checkbox = if item.done { "[✓]" } else { "[ ]" };
        // The checkbox column lights up in the accent only for open items
        // while the panel is focused; resting and done checkboxes sit in
        // the border gray.
        let checkbox_style = if item.done || !panel_focused {
            chrome
        } else {
            accent
        };
        let body_style = if item.done {
            done_text_style
        } else {
            text_style
        };

        // The item being edited renders its live buffer in place of its
        // stored text, with the cursor spliced at the insert position. It
        // carries the accent wash instead of the selection bg so "being
        // edited" reads apart from "selected".
        let edit = state
            .todos
            .editing
            .as_ref()
            .filter(|edit| edit.index == index);
        let is_edited = edit.is_some();
        let row_style = if is_edited {
            body_style.bg(edit_wash)
        } else if is_selected {
            body_style.bg(row_bg)
        } else {
            body_style
        };
        let checkbox_row_style = if is_edited {
            checkbox_style.bg(edit_wash)
        } else if is_selected {
            checkbox_style.bg(row_bg)
        } else {
            checkbox_style
        };
        let wrapped: Vec<(String, Option<usize>)> = match edit {
            Some(edit) => wrap_with_cursor(&edit.input, edit.cursor, text_width),
            None => wrap_flow(&item.text, text_width, text_width)
                .into_iter()
                .map(|line| (line, None))
                .collect(),
        };
        let wrapped = if wrapped.is_empty() {
            vec![(String::new(), None)]
        } else {
            wrapped
        };
        // The cursor glyph only appears on the edited item; it keeps the
        // accent fg over the wash.
        let cursor_style = accent.bg(edit_wash);
        let first_line = lines.len();
        for (i, (chunk, cursor)) in wrapped.iter().enumerate() {
            let prefix = if i == 0 {
                format!("{checkbox} ")
            } else {
                " ".repeat(CHECKBOX_WIDTH)
            };
            let prefix = truncate_to_width(&prefix, width);
            let prefix_width = display_width(&prefix);
            let mut spans = vec![Span::styled(prefix, checkbox_row_style)];
            match cursor {
                Some(position) => {
                    // Split the chunk around the cursor so the glyph can
                    // carry its own accent style.
                    let position = *position;
                    let before: String = chunk.chars().take(position).collect();
                    let after: String = chunk.chars().skip(position).collect();
                    spans.push(Span::styled(before, row_style));
                    spans.push(Span::styled("█", cursor_style));
                    if !after.is_empty() {
                        spans.push(Span::styled(after, row_style));
                    }
                }
                None => spans.push(Span::styled(chunk.clone(), row_style)),
            }
            let used = prefix_width + display_width(chunk) + usize::from(cursor.is_some());
            if (is_edited || is_selected) && used < width {
                let bg = if is_edited { edit_wash } else { row_bg };
                spans.push(Span::styled(
                    " ".repeat(width - used),
                    Style::default().bg(bg),
                ));
            }
            lines.push(Line::from(spans));
        }
        ranges.push((index, first_line, lines.len() - 1));
    }

    // Inline add line. New items are appended here; edits happen in
    // place on the item's own rows. The row carries the wash across its
    // full width; no key hints — `?` documents the flow.
    let mut input_line: Option<usize> = None;
    if let Some(input) = &state.todos.adding {
        input_line = Some(lines.len());
        let bg = row_bg;
        // "+ " prefix plus the cursor column.
        let input_text = tail_fit(input, width.saturating_sub(3));
        let used = 2 + display_width(&input_text) + 1;
        let mut spans = vec![
            Span::styled("+ ", accent.bg(bg)),
            Span::styled(input_text, text_style.bg(bg)),
            Span::styled("█", accent.bg(bg)),
        ];
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        lines.push(Line::from(spans));
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
    // Keep the add line visible while typing.
    if input_line.is_some() && lines.len() > state.scrolls.todos.offset + visible_height {
        state.scrolls.todos.offset = lines.len() - visible_height;
    }

    let offset = state.scrolls.todos.offset;
    let paragraph = Paragraph::new(lines).scroll((offset as u16, 0));
    frame.render_widget(paragraph, area);

    // Click targets, clipped to the visible window. The checkbox target
    // covers the marker column of the item's first wrapped line only; a
    // scrolled-away first line (or a continuation line) leaves the whole
    // visible rect as the text target, where a click selects instead of
    // toggling.
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
            checkbox_rect: (start >= offset).then(|| {
                Rect::new(
                    area.x,
                    area.y + (start - offset) as u16,
                    CHECKBOX_WIDTH as u16,
                    1,
                )
            }),
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
    use ratatui::style::Color;

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

    /// Render the panel and dump the buffer WITH style annotations
    /// (`sym[fg:255,bg:239,bold]`), so the state-chrome snapshots lock in
    /// the exact fg/bg/modifier of every cell — color is the whole point
    /// of those snapshots.
    fn render_panel_styled(state: &mut AppState, width: u16, height: u16) -> String {
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
                let cell = &buf[(x, y)];
                line.push_str(cell.symbol());
                let mut attrs = Vec::new();
                if let Color::Indexed(n) = cell.fg {
                    attrs.push(format!("fg:{n}"));
                }
                if let Color::Indexed(n) = cell.bg {
                    attrs.push(format!("bg:{n}"));
                }
                if cell.modifier.contains(Modifier::BOLD) {
                    attrs.push("bold".into());
                }
                if cell.modifier.contains(Modifier::CROSSED_OUT) {
                    attrs.push("crossed".into());
                }
                if !attrs.is_empty() {
                    line.push_str(&format!("[{}]", attrs.join(",")));
                }
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    /// Shared fixture for the state-chrome snapshots: two open items
    /// (the second wraps) and one done item, completed section expanded,
    /// first item selected.
    fn state_chrome_state() -> AppState {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 0;
        state
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
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn hide_completed_collapses_the_completed_section() {
        let mut state = todo_state();
        state.global.todo_hide_completed = true;
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
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
        ─ todos · 2/3───────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        + new task█
        ");
    }

    #[test]
    fn edit_renders_in_place_with_a_trailing_cursor() {
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] Buy milk█
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn edit_cursor_moves_with_the_buffer() {
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        // Left twice, then insert: the glyph lands mid-word.
        state.todo_edit_cursor_move(-2);
        state.todo_input_push_char('X');
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] Buy miX█lk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn editing_wraps_long_text_in_place() {
        let mut state = todo_state();
        state.todos.selected = 2;
        state.begin_todo_edit();
        state.todo_edit_cursor_home();
        let output = render_panel(&mut state, 24, 10);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────
        [ ] Buy milk
        [ ] █Read the tmux
            changelog and try
            the new paste
            behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn editing_an_emptied_item_keeps_the_cursor_visible() {
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        state.todo_input_clear();
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] █
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn panel_height_counts_the_extra_cursor_line() {
        let mut state = todo_state();
        state.todos.items = vec![TodoItem {
            text: "abcd".into(),
            done: false,
        }];
        state.todos.selected = 0;
        // 8 columns − 4 checkbox = 4 text columns: "abcd" is exactly full,
        // so the cursor glyph spills onto its own row.
        state.begin_todo_edit();
        assert_eq!(panel_height(&state, Rect::new(0, 0, 8, 20)), 3);
    }

    #[test]
    fn empty_list_renders_only_the_header() {
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ todos · 0/0───────────────────────────
        ");
    }

    #[test]
    fn all_completed_collapsed_shows_only_the_struck_item() {
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
        ─ todos · 0/1───────────────────────────
        ");
    }

    #[test]
    fn open_only_list_renders_no_divider_row() {
        let mut state = todo_state();
        state.todos.items = vec![
            TodoItem {
                text: "First".into(),
                done: false,
            },
            TodoItem {
                text: "Second".into(),
                done: false,
            },
        ];
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 6);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/2───────────────────────────
        [ ] First
        [ ] Second
        ");
    }

    #[test]
    fn focused_panel_renders_no_add_affordance() {
        // The panel owns the keyboard with nothing being typed: just the
        // items — `a` opens insert mode, no resting row advertises it.
        let mut state = todo_state();
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn selected_done_item_renders_in_the_completed_block() {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 1; // the completed item
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ todos · 2/3───────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn panel_height_ignores_the_completed_count() {
        let mut state = todo_state();
        // Expanded: header + 1 + 2 (wrapped open item) + 1 (done item).
        // No divider row, no resting add row.
        state.global.todo_hide_completed = false;
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20)), 5);
        // Collapsed: header + 1 + 2.
        state.global.todo_hide_completed = true;
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20)), 4);
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
        // The checkbox target is the marker column of each item's first
        // line; the rest of the row rect is the text target.
        assert_eq!(
            state.layout.todo_row_targets[0].checkbox_rect,
            Some(Rect::new(0, 1, 4, 1)),
        );
        assert_eq!(
            state.layout.todo_row_targets[1].checkbox_rect,
            Some(Rect::new(0, 2, 4, 1)),
        );
        // No input rect while nothing is being typed: the resting add
        // affordance is gone, `a` / Enter open insert mode.
        assert_eq!(state.layout.todo_input_rect, None);
    }

    #[test]
    fn scrolled_first_line_leaves_no_checkbox_target() {
        // The long open item's first line scrolls out of the view; its
        // visible continuation line must not carry the checkbox target,
        // while the fully visible done item keeps its own.
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 1; // the done item, scrolled into view
        render_panel(&mut state, 24, 3);

        let targets: Vec<&TodoRowTarget> = state.layout.todo_row_targets.iter().collect();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].index, 2);
        assert_eq!(
            targets[0].checkbox_rect, None,
            "a continuation line must not carry the checkbox target"
        );
        assert_eq!(targets[1].index, 1);
        assert_eq!(
            targets[1].checkbox_rect,
            Some(Rect::new(0, 2, 4, 1)),
            "the visible item's first line keeps its checkbox target"
        );
    }

    #[test]
    fn expanded_completed_block_still_registers_every_row_target() {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        render_panel(&mut state, 40, 10);

        // Header, the two open items (the second wraps to two lines),
        // then the completed item — no divider row between the blocks.
        assert_eq!(state.layout.todo_row_targets.len(), 3);
    }

    // ─── State chrome: resting vs focused vs adding vs editing ─────
    // One fixture, four keyboard states. Every state must be
    // distinguishable by color alone; the styled snapshots lock in the
    // exact fg/bg/modifier per cell.

    #[test]
    fn resting_panel_uses_no_accent() {
        // The agent list owns the keyboard: the header separator and the
        // checkbox column sink to the border gray (240), open text keeps
        // its full brightness, the done block stays muted + struck, and
        // no accent (153) appears anywhere.
        let mut state = state_chrome_state();
        state.focus_state.focus = Focus::Panes;
        let output = render_panel_styled(&mut state, 40, 6);
        insta::assert_snapshot!(output, @"
        ─[fg:240] [fg:240]t[fg:240]o[fg:240]d[fg:240]o[fg:240]s[fg:240] [fg:240]·[fg:240] [fg:240]2[fg:240]/[fg:240]3[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]
        [[fg:240] [fg:240]][fg:240] [fg:240]B[fg:255]u[fg:255]y[fg:255] [fg:255]m[fg:255]i[fg:255]l[fg:255]k[fg:255]
        [[fg:240] [fg:240]][fg:240] [fg:240]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:240] [fg:240] [fg:240] [fg:240]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:252,crossed]e[fg:252,crossed]n[fg:252,crossed]d[fg:252,crossed] [fg:252,crossed]t[fg:252,crossed]h[fg:252,crossed]e[fg:252,crossed] [fg:252,crossed]i[fg:252,crossed]n[fg:252,crossed]v[fg:252,crossed]o[fg:252,crossed]i[fg:252,crossed]c[fg:252,crossed]e[fg:252,crossed]
        ");
    }

    #[test]
    fn focused_panel_lights_the_header_and_open_checkboxes() {
        // The panel owns the keyboard: header in accent + bold, open
        // checkboxes in accent, the selected item on the selection bg,
        // and the done block one step dimmer (244) than at rest.
        let mut state = state_chrome_state();
        let output = render_panel_styled(&mut state, 40, 6);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:236] [fg:153,bg:236]][fg:153,bg:236] [fg:153,bg:236]B[fg:255,bg:236]u[fg:255,bg:236]y[fg:255,bg:236] [fg:255,bg:236]m[fg:255,bg:236]i[fg:255,bg:236]l[fg:255,bg:236]k[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
        [[fg:153] [fg:153]][fg:153] [fg:153]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:153] [fg:153] [fg:153] [fg:153]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
        ");
    }

    #[test]
    fn adding_row_carries_the_dimmed_wash() {
        // Adding: the input row wears the dimmed row wash across its
        // full width, no key hint; the rest of the panel renders exactly
        // like the focused state.
        let mut state = state_chrome_state();
        state.begin_todo_add();
        for c in "new task".chars() {
            state.todo_input_push_char(c);
        }
        let output = render_panel_styled(&mut state, 40, 7);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:236] [fg:153,bg:236]][fg:153,bg:236] [fg:153,bg:236]B[fg:255,bg:236]u[fg:255,bg:236]y[fg:255,bg:236] [fg:255,bg:236]m[fg:255,bg:236]i[fg:255,bg:236]l[fg:255,bg:236]k[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
        [[fg:153] [fg:153]][fg:153] [fg:153]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:153] [fg:153] [fg:153] [fg:153]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
        +[fg:153,bg:236] [fg:153,bg:236]n[fg:255,bg:236]e[fg:255,bg:236]w[fg:255,bg:236] [fg:255,bg:236]t[fg:255,bg:236]a[fg:255,bg:236]s[fg:255,bg:236]k[fg:255,bg:236]█[fg:153,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
        ");
    }

    #[test]
    fn editing_row_carries_the_accent_wash() {
        // Editing: the edited item's rows wear the accent wash
        // (shade_color(accent, 0.5) — a faint steel-blue tint of the
        // hue) instead of the selection gray, and the cursor keeps the
        // accent fg over the wash.
        let mut state = state_chrome_state();
        state.begin_todo_edit();
        let output = render_panel_styled(&mut state, 40, 6);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:60] [fg:153,bg:60]][fg:153,bg:60] [fg:153,bg:60]B[fg:255,bg:60]u[fg:255,bg:60]y[fg:255,bg:60] [fg:255,bg:60]m[fg:255,bg:60]i[fg:255,bg:60]l[fg:255,bg:60]k[fg:255,bg:60]█[fg:153,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
        [[fg:153] [fg:153]][fg:153] [fg:153]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:153] [fg:153] [fg:153] [fg:153]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
        ");
    }
}
