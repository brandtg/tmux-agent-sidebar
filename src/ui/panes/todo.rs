use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::state::{AppState, Focus, TodoRowTarget};
use crate::ui::colors::shade_color;
use crate::ui::text::{display_width, truncate_to_width, wrap_flow, wrap_flow_offsets};

/// Baseline rows the panel occupies when the agent list leaves no spare
/// space, including its header. The panel grows past this into whatever
/// rows the agent list isn't using (see [`panel_height`]), so this only
/// bites on short terminals where the agents fill the sidebar.
const BASE_PANEL_HEIGHT: u16 = 10;
/// Tighter baseline for the narrow popup viewport (phones).
const POPUP_BASE_PANEL_HEIGHT: u16 = 6;
/// Columns used by the `[ ]` / `[✓]` marker plus one separating space.
const CHECKBOX_WIDTH: usize = 4;

/// Wrap the live edit buffer like any other item text, returning each
/// display line plus the char position of the cursor on its line
/// (`None` when the cursor falls elsewhere).
///
/// The cursor covers the char at its position and is drawn in place —
/// no glyph column is spliced into the buffer — so wrapping matches the
/// resting render exactly and nothing shifts as the cursor moves. Only
/// an end-of-buffer cursor on an exactly full line needs an extra row
/// for the block glyph, which `row_width` (the full panel width the
/// row renders into, wrap width plus the checkbox column) detects.
fn wrap_with_cursor(
    input: &str,
    cursor: usize,
    wrap_width: usize,
    row_width: usize,
) -> Vec<(String, Option<usize>)> {
    let cursor = cursor.min(input.chars().count());
    let lines = wrap_flow_offsets(input, wrap_width, wrap_width);
    if lines.is_empty() {
        return vec![(String::new(), Some(0))];
    }
    let mut result: Vec<(String, Option<usize>)> =
        lines.iter().map(|(line, _)| (line.clone(), None)).collect();
    // The cursor belongs to the last line that starts at or before it.
    // A cursor pointing at a break space (a char no line kept) clamps
    // to the end of the previous line — the insertion point a typed
    // char would land at.
    let home = lines
        .iter()
        .enumerate()
        .rfind(|(_, (_, start))| *start <= cursor);
    if let Some((index, (_, start))) = home {
        let line = &mut result[index].0;
        let len = line.chars().count();
        let position = (cursor - start).min(len);
        if position == len && display_width(line) + CHECKBOX_WIDTH + 1 > row_width {
            // No cell left for the block glyph on this row: spill it
            // onto a row of its own.
            result.insert(index + 1, (String::new(), Some(0)));
        } else {
            result[index].1 = Some(position);
        }
    }
    result
}

/// The add line's visible text plus the cursor's char index into it.
///
/// The add line is a single row, so an input wider than it shows a
/// window of itself, readline style. While the cursor sits inside the
/// tail window — the default, since typing appends at the end — the
/// window keeps the text's tail exactly like `tail_fit`, with `…`
/// marking the hidden head. Arrowing left past the window's start
/// slides the window so it ends at the cursor: the caret stays pinned
/// at the right edge, with the hidden head marked the same way and the
/// tail beyond the caret left for the arrow keys to walk back into.
fn fit_input_window(input: &str, cursor: usize, max_width: usize) -> (String, usize) {
    use unicode_width::UnicodeWidthChar;
    if max_width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let total: usize = chars.iter().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max_width {
        return (input.to_string(), cursor);
    }
    // One column of the budget goes to the `…` marking the hidden side.
    let budget = max_width - 1;
    // The tail window: the widest run of trailing chars that fits.
    let mut start = chars.len();
    let mut taken = 0usize;
    for (i, ch) in chars.iter().enumerate().rev() {
        let w = ch.width().unwrap_or(0);
        if taken + w > budget {
            break;
        }
        taken += w;
        start = i;
    }
    // The `…` shifts every visible char one position right, so the
    // cursor's index into the window is its text offset plus one.
    if start <= cursor {
        let mut visible = String::from("\u{2026}");
        visible.extend(chars[start..].iter());
        return (visible, cursor - start + 1);
    }
    // Slide the window left so it ends at the cursor.
    let mut end = cursor;
    let mut taken = 0usize;
    while end > 0 {
        let w = chars[end - 1].width().unwrap_or(0);
        if taken + w > budget {
            break;
        }
        taken += w;
        end -= 1;
    }
    let mut visible = String::from("\u{2026}");
    visible.extend(chars[end..cursor].iter());
    (visible, cursor - end + 1)
}

/// Rows the panel needs, clamped to what the agent list can spare.
///
/// The panel grows with its content, but the sessions/agents take
/// precedence: it may only expand into rows the agent list isn't using
/// (`agent_rows` is the agent list's rendered line count). Once the
/// agent list fills its space the panel keeps its baseline window and
/// scrolls inside it — the header stays pinned and the header indicator
/// reports what is hidden. The final clamp always leaves the two header
/// rows plus at least one agent row.
pub(super) fn panel_height(state: &AppState, area: Rect, agent_rows: usize) -> u16 {
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
            // A block cursor with no cell left on an exactly full line
            // spills onto a row of its own.
            Some(edit) => wrap_with_cursor(&edit.input, edit.cursor, text_width, width).len(),
            None => match state.todos.cached_wrap(index, text_width) {
                Some((lines, _)) => lines.len(),
                None => wrap_flow(&state.todos.items[index].text, text_width, text_width).len(),
            },
        };
        rows += text_rows.max(1);
    }
    if state.todos.adding.is_some() {
        rows += 1;
    }

    let baseline = if state.popup_mode {
        POPUP_BASE_PANEL_HEIGHT
    } else {
        BASE_PANEL_HEIGHT
    };
    // Rows the agent list doesn't use: the panel may expand into them,
    // but never into a row an agent occupies. When the agents alone
    // overflow the sidebar there is no spare space and the panel keeps
    // its baseline window.
    let spare = area
        .height
        .saturating_sub(agent_rows.min(area.height as usize) as u16)
        .saturating_sub(2);
    let room = baseline.max(spare);
    (rows as u16).min(room).min(area.height.saturating_sub(3))
}

/// Render the TODO panel into `area` (already sized by [`panel_height`]).
///
/// The header row is pinned to the top of the panel and always visible;
/// the items, error line and add line scroll in the rows beneath it.
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
    // dimmer than the open block's text in every state. Focus shifts the
    // brightness within the open block; it never lights the graveyard
    // back up.
    let done_text_style = Style::default()
        .fg(theme.text_inactive)
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
    // and selection auto-scroll. Indices are relative to the item area
    // beneath the pinned header.
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();

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
        // The resume glyph rides recognized rows: a dim `↻` marking
        // that `r` will relaunch this item's command. It is a view over
        // the item's text, never a state change — and it hides while
        // the row is being edited, where the live buffer replaces the
        // stored text the glyph describes. Resolution and the wrap
        // below both consult the memoized cache (refilled at the end of
        // the render pass); the wrap used to run twice per frame — here
        // and again in the height sizing (audit 2026-11-16 J6).
        let (is_resume_row, cached) = if is_edited {
            (false, None)
        } else {
            let cached = state.todos.cached_wrap(index, text_width);
            let is_resume = match &cached {
                Some((_, resume)) => *resume,
                // Cache miss: recognize fresh, exactly as before the memo.
                None => crate::park::recognize_resume(&item.text).is_some(),
            };
            (is_resume, cached)
        };
        let checkbox_row_style = if is_edited {
            checkbox_style.bg(edit_wash)
        } else if is_selected {
            checkbox_style.bg(row_bg)
        } else {
            checkbox_style
        };
        let wrapped: Vec<(String, Option<usize>)> = match (edit, cached) {
            (Some(edit), _) => wrap_with_cursor(&edit.input, edit.cursor, text_width, width),
            (None, Some((lines, _))) => lines.iter().map(|line| (line.clone(), None)).collect(),
            (None, None) => wrap_flow(&item.text, text_width, text_width)
                .into_iter()
                .map(|line| (line, None))
                .collect(),
        };
        let wrapped = if wrapped.is_empty() {
            vec![(String::new(), None)]
        } else {
            wrapped
        };
        // The cursor has two looks. Covering a char it paints that char
        // inverted in place — a solid block with the char readable
        // inside, shifting nothing. Past the end of the text there is no
        // char to cover, so it falls back to the accent block glyph,
        // matching the add line's cursor.
        let block_style = accent.bg(edit_wash);
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
                    // Split the chunk around the covered char so it can
                    // carry the inverted style.
                    let position = *position;
                    let mut chars = chunk.chars();
                    let before: String = chars.by_ref().take(position).collect();
                    let under: String = chars.by_ref().take(1).collect();
                    let after: String = chars.collect();
                    spans.push(Span::styled(before, row_style));
                    if under.is_empty() {
                        spans.push(Span::styled("█", block_style));
                    } else {
                        spans.push(Span::styled(
                            under,
                            row_style.add_modifier(Modifier::REVERSED),
                        ));
                        if !after.is_empty() {
                            spans.push(Span::styled(after, row_style));
                        }
                    }
                }
                None => spans.push(Span::styled(chunk.clone(), row_style)),
            }
            let at_end = matches!(cursor, Some(p) if *p >= chunk.chars().count());
            let used = prefix_width + display_width(chunk) + usize::from(at_end);
            // The glyph is right-aligned on the first line, and only
            // when ` ↻` fits without shifting or clipping the text.
            let glyph_here = is_resume_row && i == 0 && width >= used + 2;
            if (is_edited || is_selected) && used < width {
                let bg = if is_edited { edit_wash } else { row_bg };
                let fill = if glyph_here {
                    width - used - 1
                } else {
                    width - used
                };
                spans.push(Span::styled(" ".repeat(fill), Style::default().bg(bg)));
                if glyph_here {
                    spans.push(Span::styled(
                        "↻",
                        Style::default().fg(theme.text_inactive).bg(bg),
                    ));
                }
            } else if glyph_here {
                spans.push(Span::styled(" ".repeat(width - used - 1), Style::default()));
                spans.push(Span::styled("↻", Style::default().fg(theme.text_inactive)));
            }
            lines.push(Line::from(spans));
        }
        ranges.push((index, first_line, lines.len() - 1));
    }
    // Refill the wrap memo for the next frame's height sizing (see the
    // cached_wrap consults above).
    state.todos.fill_wrap_cache(text_width, &visible);

    // Inline add line. New items are appended here; edits happen in
    // place on the item's own rows. The row carries the wash across its
    // full width; no key hints — `?` documents the flow. The cursor
    // moves with the arrow keys like the editor's: past the end of the
    // buffer it is the accent block glyph after the text, over a char
    // it covers that char in place, so nothing shifts as it moves.
    let mut input_line: Option<usize> = None;
    if let Some(input) = &state.todos.adding {
        input_line = Some(lines.len());
        let bg = row_bg;
        // "+ " prefix plus one column for the end-of-buffer glyph.
        let (visible, cursor) =
            fit_input_window(input, state.todos.adding_cursor, width.saturating_sub(3));
        let mut spans = vec![Span::styled("+ ", accent.bg(bg))];
        let mut used = 2usize;
        if cursor >= visible.chars().count() {
            used += display_width(&visible) + 1;
            spans.push(Span::styled(visible, text_style.bg(bg)));
            spans.push(Span::styled("█", accent.bg(bg)));
        } else {
            let mut chars = visible.chars();
            let before: String = chars.by_ref().take(cursor).collect();
            let under: String = chars.by_ref().take(1).collect();
            let after: String = chars.collect();
            let before_w = display_width(&before);
            let under_w = display_width(&under);
            let after_w = display_width(&after);
            spans.push(Span::styled(before, text_style.bg(bg)));
            spans.push(Span::styled(
                under,
                text_style.bg(bg).add_modifier(Modifier::REVERSED),
            ));
            if !after.is_empty() {
                spans.push(Span::styled(after, text_style.bg(bg)));
            }
            used += before_w + under_w + after_w;
        }
        if used < width {
            spans.push(Span::styled(
                " ".repeat(width - used),
                Style::default().bg(bg),
            ));
        }
        lines.push(Line::from(spans));
    }

    // The items viewport is everything below the pinned header row.
    let items_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    let visible_height = items_area.height as usize;
    state.scrolls.todos.total_lines = lines.len();
    state.scrolls.todos.visible_height = visible_height;
    state.scrolls.todos.scroll(0);

    if visible_height > 0 {
        if panel_focused
            && let Some((_, start, end)) = ranges.iter().find(|(i, _, _)| *i == selected)
        {
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
    }

    let offset = state.scrolls.todos.offset;

    // Header: the separator carries the counts. At rest it sinks to the
    // border gray; while the panel owns the keyboard it lights up in the
    // accent so it is obvious whether the agent list or the TODO list is
    // active. Built last so it can carry the scroll indicator — the one
    // row that never scrolls away.
    let header_style = if panel_focused {
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        chrome
    };
    let header = format!(
        "─ □ todos · {}/{}",
        state.todos.open_count(),
        state.todos.total_count()
    );
    let header = truncate_to_width(&header, width);
    let mut spans = vec![Span::styled(header.clone(), header_style)];
    let fill = width.saturating_sub(display_width(&header));
    let indicator = scroll_indicator(lines.len(), visible_height, offset);
    let indicator_width = display_width(&indicator);
    if !indicator.is_empty() && indicator_width < fill {
        // Right-align the indicator inside the separator fill.
        spans.push(Span::styled(
            "─".repeat(fill - indicator_width - 1),
            header_style,
        ));
        spans.push(Span::styled(format!(" {indicator}"), header_style));
    } else {
        spans.push(Span::styled("─".repeat(fill), header_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    );

    if visible_height > 0 {
        let paragraph = Paragraph::new(lines).scroll((offset as u16, 0));
        frame.render_widget(paragraph, items_area);
    }

    // Click targets, clipped to the visible window. The checkbox target
    // covers the marker column of the item's first wrapped line only; a
    // scrolled-away first line (or a continuation line) leaves the whole
    // visible rect as the text target, where a click selects instead of
    // toggling. Rows are offset one below the pinned header.
    for (index, start, end) in ranges {
        let first = start.max(offset);
        let last = end.min(offset + visible_height.saturating_sub(1));
        if first > last {
            continue;
        }
        state.layout.todo_row_targets.push(TodoRowTarget {
            rect: Rect::new(
                area.x,
                area.y + 1 + (first - offset) as u16,
                area.width,
                (last - first + 1) as u16,
            ),
            checkbox_rect: (start >= offset).then(|| {
                Rect::new(
                    area.x,
                    area.y + 1 + (start - offset) as u16,
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
            area.y + 1 + (index - offset) as u16,
            area.width,
            1,
        ));
    }
}

/// What the scroll hides, as a compact directional count: `1 more ↓` at
/// the top, `↑1` at the bottom, `↑1 · 1 more ↓` in between. Empty when
/// everything fits, so a list that needs no scrolling stays clean.
fn scroll_indicator(total_lines: usize, visible_height: usize, offset: usize) -> String {
    if total_lines <= visible_height {
        return String::new();
    }
    let above = offset;
    let below = total_lines.saturating_sub(offset + visible_height);
    let mut parts: Vec<String> = Vec::new();
    if above > 0 {
        parts.push(format!("↑{above}"));
    }
    if below > 0 {
        parts.push(format!("{below} more ↓"));
    }
    parts.join(" · ")
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
                if cell.modifier.contains(Modifier::REVERSED) {
                    attrs.push("rev".into());
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
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20), 0), 0);
    }

    #[test]
    fn panel_renders_open_and_completed_items() {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3─────────────────────────
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
        ─ □ todos · 2/3─────────────────────────
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
        ─ □ todos · 2/3─────────
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
        ─ □ todos · 2/3─────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        + new task█
        ");
    }

    #[test]
    fn add_line_cursor_covers_the_char_it_sits_on() {
        // Left twice from the end: the cursor covers the char at its
        // position in place — the same look as the editor's on-char
        // cursor — instead of the trailing block glyph.
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![TodoItem {
            text: "Buy milk".into(),
            done: false,
        }];
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state.begin_todo_add();
        for c in "hello".chars() {
            state.todo_input_push_char(c);
        }
        state.todo_input_cursor_move(-2);
        let output = render_panel_styled(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]1[fg:153,bold]/[fg:153,bold]1[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:236] [fg:153,bg:236]][fg:153,bg:236] [fg:153,bg:236]B[fg:255,bg:236]u[fg:255,bg:236]y[fg:255,bg:236] [fg:255,bg:236]m[fg:255,bg:236]i[fg:255,bg:236]l[fg:255,bg:236]k[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
        +[fg:153,bg:236] [fg:153,bg:236]h[fg:255,bg:236]e[fg:255,bg:236]l[fg:255,bg:236]l[fg:255,bg:236,rev]o[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]
        ");
    }

    #[test]
    fn add_line_keeps_the_cursor_visible_when_the_input_overflows() {
        // The single add row shows a window of an input wider than the
        // panel: the tail plus `…`, with the end cursor after it. Then
        // arrowing into the hidden head slides the window so it ends at
        // the cursor, keeping the caret on screen.
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state.begin_todo_add();
        for c in "the quick brown fox jumps".chars() {
            state.todo_input_push_char(c);
        }
        let output = render_panel(&mut state, 20, 4);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 0/0─────
        + … brown fox jumps█
        ");

        state.todo_input_cursor_move(-21);
        let output = render_panel(&mut state, 20, 4);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 0/0─────
        + …the █
        ");
    }

    #[test]
    fn edit_renders_in_place_with_a_trailing_cursor() {
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        let output = render_panel(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3─────────────────────────
        [ ] Buy milk█
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn edit_cursor_covers_the_char_it_moves_over() {
        // Left twice, then insert: the cursor highlights the char it
        // sits on in place — no block glyph column, so nothing shifts.
        let mut state = todo_state();
        state.todos.selected = 0;
        state.begin_todo_edit();
        state.todo_input_cursor_move(-2);
        state.todo_input_push_char('X');
        let output = render_panel_styled(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:60] [fg:153,bg:60]][fg:153,bg:60] [fg:153,bg:60]B[fg:255,bg:60]u[fg:255,bg:60]y[fg:255,bg:60] [fg:255,bg:60]m[fg:255,bg:60]i[fg:255,bg:60]X[fg:255,bg:60]l[fg:255,bg:60,rev]k[fg:255,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
        [[fg:153] [fg:153]][fg:153] [fg:153]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:153] [fg:153] [fg:153] [fg:153]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
        ");
    }

    #[test]
    fn editing_wraps_long_text_in_place() {
        let mut state = todo_state();
        state.todos.selected = 2;
        state.begin_todo_edit();
        state.todo_input_cursor_home();
        let output = render_panel_styled(&mut state, 24, 10);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153] [fg:153]][fg:153] [fg:153]B[fg:255]u[fg:255]y[fg:255] [fg:255]m[fg:255]i[fg:255]l[fg:255]k[fg:255]
        [[fg:153,bg:60] [fg:153,bg:60]][fg:153,bg:60] [fg:153,bg:60]R[fg:255,bg:60,rev]e[fg:255,bg:60]a[fg:255,bg:60]d[fg:255,bg:60] [fg:255,bg:60]t[fg:255,bg:60]h[fg:255,bg:60]e[fg:255,bg:60] [fg:255,bg:60]t[fg:255,bg:60]m[fg:255,bg:60]u[fg:255,bg:60]x[fg:255,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
         [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60]c[fg:255,bg:60]h[fg:255,bg:60]a[fg:255,bg:60]n[fg:255,bg:60]g[fg:255,bg:60]e[fg:255,bg:60]l[fg:255,bg:60]o[fg:255,bg:60]g[fg:255,bg:60] [fg:255,bg:60]a[fg:255,bg:60]n[fg:255,bg:60]d[fg:255,bg:60] [fg:255,bg:60]t[fg:255,bg:60]r[fg:255,bg:60]y[fg:255,bg:60] [bg:60] [bg:60] [bg:60]
         [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60]t[fg:255,bg:60]h[fg:255,bg:60]e[fg:255,bg:60] [fg:255,bg:60]n[fg:255,bg:60]e[fg:255,bg:60]w[fg:255,bg:60] [fg:255,bg:60]p[fg:255,bg:60]a[fg:255,bg:60]s[fg:255,bg:60]t[fg:255,bg:60]e[fg:255,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
         [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60] [fg:153,bg:60]b[fg:255,bg:60]e[fg:255,bg:60]h[fg:255,bg:60]a[fg:255,bg:60]v[fg:255,bg:60]i[fg:255,bg:60]o[fg:255,bg:60]u[fg:255,bg:60]r[fg:255,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
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
        ─ □ todos · 2/3─────────────────────────
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
        // so the end-of-buffer cursor glyph spills onto its own row.
        state.begin_todo_edit();
        assert_eq!(panel_height(&state, Rect::new(0, 0, 8, 20), 0), 3);
    }

    #[test]
    fn panel_height_ignores_an_on_char_cursor() {
        let mut state = todo_state();
        state.todos.items = vec![TodoItem {
            text: "abcd".into(),
            done: false,
        }];
        state.todos.selected = 0;
        // The cursor covers a char in place — no extra column, so the
        // exactly full line needs no spill row.
        state.begin_todo_edit();
        state.todo_input_cursor_move(-2);
        assert_eq!(panel_height(&state, Rect::new(0, 0, 8, 20), 0), 2);
    }

    #[test]
    fn empty_list_renders_only_the_header() {
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"─ □ todos · 0/0─────────────────────────");
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
        insta::assert_snapshot!(output, @"─ □ todos · 0/1─────────────────────────");
    }

    #[test]
    fn recognized_resume_rows_carry_the_dim_glyph() {
        // A resume-shaped item shows a dim `↻` right-aligned on its
        // first line — a view over the text, done rows included — while
        // plain items render exactly as before.
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![
            TodoItem {
                text: "claude --resume f4ac0121-58e7-4d9c-a5c6-e0db65c6c49e".into(),
                done: false,
            },
            TodoItem {
                text: "Buy milk".into(),
                done: false,
            },
            TodoItem {
                text: "opencode -s ses_e1eb53dd9ffemw0k3treOa3PBO @ ~/Projects/w".into(),
                done: true,
            },
        ];
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state.todos.selected = 1;
        let output = render_panel(&mut state, 72, 8);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3─────────────────────────────────────────────────────────
        [ ] claude --resume f4ac0121-58e7-4d9c-a5c6-e0db65c6c49e               ↻
        [ ] Buy milk
        [✓] opencode -s ses_e1eb53dd9ffemw0k3treOa3PBO @ ~/Projects/w          ↻
        ");
        // Second frame renders through the wrap memo filled by the first
        // (audit 2026-11-16 J6): identical output, glyph included.
        let again = render_panel(&mut state, 72, 8);
        assert_eq!(again, output);
    }

    #[test]
    fn the_glyph_rides_the_selection_wash_and_survives_an_edit() {
        // Selected: the glyph keeps the selection bg. While the row is
        // being edited the buffer replaces the stored text, so the
        // glyph (which describes the stored text) stands down.
        let mut state = AppState::new("%99".into());
        state.todos.items = vec![TodoItem {
            text: "codex resume 4e71c60a-de52".into(),
            done: false,
        }];
        state.todos.visible = true;
        state.focus_state.focus = Focus::Todos;
        state.todos.selected = 0;
        let output = render_panel_styled(&mut state, 40, 6);
        insta::assert_snapshot!(output, @"
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]1[fg:153,bold]/[fg:153,bold]1[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:236] [fg:153,bg:236]][fg:153,bg:236] [fg:153,bg:236]c[fg:255,bg:236]o[fg:255,bg:236]d[fg:255,bg:236]e[fg:255,bg:236]x[fg:255,bg:236] [fg:255,bg:236]r[fg:255,bg:236]e[fg:255,bg:236]s[fg:255,bg:236]u[fg:255,bg:236]m[fg:255,bg:236]e[fg:255,bg:236] [fg:255,bg:236]4[fg:255,bg:236]e[fg:255,bg:236]7[fg:255,bg:236]1[fg:255,bg:236]c[fg:255,bg:236]6[fg:255,bg:236]0[fg:255,bg:236]a[fg:255,bg:236]-[fg:255,bg:236]d[fg:255,bg:236]e[fg:255,bg:236]5[fg:255,bg:236]2[fg:255,bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236] [bg:236]↻[fg:244,bg:236]
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
        ─ □ todos · 2/2─────────────────────────
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
        ─ □ todos · 2/3─────────────────────────
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
        ─ □ todos · 2/3─────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    // ─── Pinned header + scroll indicator ────────────────────────────
    // The header row never scrolls away: a list longer than the panel
    // scrolls beneath it, and the header reports what is hidden.

    #[test]
    fn scrolled_list_pins_the_header_and_counts_what_is_hidden() {
        // 4 item lines under the header in a 4-row panel: 3 rows of
        // items are visible, one is hidden below.
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 0;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3──────────────── 1 more ↓
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        ");
    }

    #[test]
    fn scrolled_to_the_end_shows_what_is_hidden_above() {
        let mut state = todo_state();
        state.global.todo_hide_completed = false;
        state.todos.selected = 1; // the done item, auto-scrolled into view
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3────────────────────── ↑1
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice
        ");
    }

    #[test]
    fn mid_scroll_shows_hidden_lines_in_both_directions() {
        let mut state = todo_state();
        state.todos.items.push(TodoItem {
            text: "Fourth".into(),
            done: false,
        });
        state.global.todo_hide_completed = false;
        // Open items first, completed last: selecting the fourth item
        // (just above the completed block) leaves lines hidden both
        // above and below the viewport.
        state.todos.selected = 3;
        let output = render_panel(&mut state, 40, 4);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 3/4─────────── ↑1 · 1 more ↓
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [ ] Fourth
        ");
    }

    #[test]
    fn panel_height_ignores_the_completed_count() {
        let mut state = todo_state();
        // Expanded: header + 1 + 2 (wrapped open item) + 1 (done item).
        // No divider row, no resting add row.
        state.global.todo_hide_completed = false;
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20), 0), 5);
        // Collapsed: header + 1 + 2.
        state.global.todo_hide_completed = true;
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20), 0), 4);
    }

    #[test]
    fn panel_height_expands_into_rows_the_agent_list_does_not_use() {
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.global.todo_hide_completed = false;
        // 30 single-line items on a 50-row sidebar with 3 agent rows:
        // the agents only need 3 of the 48 free rows, so the panel takes
        // exactly what its content needs (header + 30 items) — no
        // scrolling.
        state.todos.items = (0..30)
            .map(|i| TodoItem {
                text: format!("task {i}"),
                done: false,
            })
            .collect();
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 50), 3), 31);

        // 60 rows of content overflow the 45 spare rows: the panel stops
        // right where growing one more row would cover the first agent
        // (50 − 2 headers − 3 agents) and scrolls inside that instead.
        state.todos.items = (0..60)
            .map(|i| TodoItem {
                text: format!("task {i}"),
                done: false,
            })
            .collect();
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 50), 3), 45);
    }

    #[test]
    fn panel_height_keeps_its_baseline_when_agents_fill_the_sidebar() {
        let mut state = AppState::new("%99".into());
        state.todos.visible = true;
        state.global.todo_hide_completed = false;
        state.todos.items = (0..30)
            .map(|i| TodoItem {
                text: format!("task {i}"),
                done: false,
            })
            .collect();
        // Agents alone need more rows than the sidebar has: no spare
        // space, so the panel keeps its baseline window and scrolls
        // inside it.
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20), 25), 10);
        // A tiny sidebar still reserves the two header rows plus one
        // agent row before the panel gets anything.
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 6), 25), 3);
        // Zero agents (empty list) leaves the panel the whole sidebar
        // minus the two header rows and one spare agent row.
        assert_eq!(panel_height(&state, Rect::new(0, 0, 40, 20), 0), 17);
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
        ─[fg:240] [fg:240]□[fg:240] [fg:240]t[fg:240]o[fg:240]d[fg:240]o[fg:240]s[fg:240] [fg:240]·[fg:240] [fg:240]2[fg:240]/[fg:240]3[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]─[fg:240]
        [[fg:240] [fg:240]][fg:240] [fg:240]B[fg:255]u[fg:255]y[fg:255] [fg:255]m[fg:255]i[fg:255]l[fg:255]k[fg:255]
        [[fg:240] [fg:240]][fg:240] [fg:240]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:240] [fg:240] [fg:240] [fg:240]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
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
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
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
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
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
        ─[fg:153,bold] [fg:153,bold]□[fg:153,bold] [fg:153,bold]t[fg:153,bold]o[fg:153,bold]d[fg:153,bold]o[fg:153,bold]s[fg:153,bold] [fg:153,bold]·[fg:153,bold] [fg:153,bold]2[fg:153,bold]/[fg:153,bold]3[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]─[fg:153,bold]
        [[fg:153,bg:60] [fg:153,bg:60]][fg:153,bg:60] [fg:153,bg:60]B[fg:255,bg:60]u[fg:255,bg:60]y[fg:255,bg:60] [fg:255,bg:60]m[fg:255,bg:60]i[fg:255,bg:60]l[fg:255,bg:60]k[fg:255,bg:60]█[fg:153,bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60] [bg:60]
        [[fg:153] [fg:153]][fg:153] [fg:153]R[fg:255]e[fg:255]a[fg:255]d[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255] [fg:255]t[fg:255]m[fg:255]u[fg:255]x[fg:255] [fg:255]c[fg:255]h[fg:255]a[fg:255]n[fg:255]g[fg:255]e[fg:255]l[fg:255]o[fg:255]g[fg:255] [fg:255]a[fg:255]n[fg:255]d[fg:255] [fg:255]t[fg:255]r[fg:255]y[fg:255] [fg:255]t[fg:255]h[fg:255]e[fg:255]
         [fg:153] [fg:153] [fg:153] [fg:153]n[fg:255]e[fg:255]w[fg:255] [fg:255]p[fg:255]a[fg:255]s[fg:255]t[fg:255]e[fg:255] [fg:255]b[fg:255]e[fg:255]h[fg:255]a[fg:255]v[fg:255]i[fg:255]o[fg:255]u[fg:255]r[fg:255]
        [[fg:240]✓[fg:240]][fg:240] [fg:240]S[fg:244,crossed]e[fg:244,crossed]n[fg:244,crossed]d[fg:244,crossed] [fg:244,crossed]t[fg:244,crossed]h[fg:244,crossed]e[fg:244,crossed] [fg:244,crossed]i[fg:244,crossed]n[fg:244,crossed]v[fg:244,crossed]o[fg:244,crossed]i[fg:244,crossed]c[fg:244,crossed]e[fg:244,crossed]
        ");
    }

    #[test]
    fn typing_a_trailing_space_advances_the_cursor() {
        // A space typed at the end of the buffer must move the caret one
        // column right. The wrap used to trim it off the rendered line,
        // so the cursor sat on the same cell as before the keypress and
        // the space looked lost.
        let mut state = todo_state();
        state.todos.editing = Some(crate::state::TodoEdit {
            index: 0,
            input: "Buy milk for the cat".into(),
            cursor: 20,
            original_text: "Buy milk for the cat".into(),
        });

        let before = render_panel(&mut state, 30, 10);
        insta::assert_snapshot!(before, @"
        ─ □ todos · 2/3───────────────
        [ ] Buy milk for the cat█
        [ ] Read the tmux changelog
            and try the new paste
            behaviour
        [✓] Send the invoice
        ");

        state.todo_input_push_char(' ');
        let after = render_panel(&mut state, 30, 10);
        insta::assert_snapshot!(after, @"
        ─ □ todos · 2/3───────────────
        [ ] Buy milk for the cat █
        [ ] Read the tmux changelog
            and try the new paste
            behaviour
        [✓] Send the invoice
        ");
    }

    /// Render the panel plus the flash banner in draw order (the banner
    /// rides the whole-sidebar bottom row, after the panel), so the
    /// `copied` yank feedback can be locked in as a frame.
    fn render_panel_with_flash(state: &mut AppState, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_todo_panel(frame, state, area);
                crate::ui::panes::render_flash_banner_into(frame, state, area);
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
    fn yank_flashes_copied_on_the_bottom_row() {
        // `y` under Todos focus copies the selected item's text and
        // confirms with the existing flash banner on the sidebar's
        // bottom row (accent, 4 s) — no new chrome.
        let mut state = todo_state();
        state.set_flash("copied");
        let output = render_panel_with_flash(&mut state, 40, 8);
        insta::assert_snapshot!(output, @"
        ─ □ todos · 2/3─────────────────────────
        [ ] Buy milk
        [ ] Read the tmux changelog and try the
            new paste behaviour
        [✓] Send the invoice


        copied
        ");
    }
}
