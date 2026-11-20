mod chyron;
mod click_targets;
mod popups;
mod row;
mod row_collector;
mod todo;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::state::{AppState, Focus, PopupState, RepoFilter, SpawnField};
use crate::ui::paint_background;

pub(super) const SPAWN_BUTTON: &str = "+";

/// Width of the clickable region around the `×` marker. One column of
/// slack on either side makes it comfortable to hit without stealing
/// clicks from adjacent branch text.
pub(super) const REMOVE_MARKER_HIT_WIDTH: u16 = 3;

use super::text::{display_width, truncate_to_width};

/// Compute a popup Rect centered inside `area`, clamped so it never
/// exceeds the parent (a narrow sidebar can't end up with a popup wider
/// than its own pane, which used to crash ratatui).
fn center_popup(area: Rect, desired_width: u16, desired_height: u16) -> Rect {
    let width = desired_width.min(area.width);
    let height = desired_height.min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// Place a popup directly below screen row `anchor_y`, left-aligned to
/// `area`, and shift upward when it would overflow the bottom edge.
fn anchor_below(area: Rect, anchor_y: u16, desired_width: u16, desired_height: u16) -> Rect {
    let width = desired_width.min(area.width);
    let height = desired_height.min(area.height);
    let below = anchor_y.saturating_add(1);
    let bottom = area.y.saturating_add(area.height);
    let y = if below + height <= bottom {
        below
    } else {
        bottom.saturating_sub(height).max(area.y)
    };
    Rect::new(area.x, y, width, height)
}

struct PaneLayout {
    header_area: Rect,
    list_area: Rect,
    /// Bottom TODO panel; zero-height while hidden.
    todo_area: Rect,
}

impl PaneLayout {
    fn compute_with_todo(area: Rect, todo_height: u16) -> Self {
        // Never let the panel crowd out the header row.
        let todo_height = todo_height.min(area.height.saturating_sub(1));
        // One header row: the toolbar, or the chyron while it overlays
        // the toolbar. The row is always reserved so neither state
        // shifts the list.
        let header_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1.min(area.height),
        };
        let list_area = Rect {
            x: area.x,
            y: area.y + header_area.height,
            width: area.width,
            height: area
                .height
                .saturating_sub(header_area.height)
                .saturating_sub(todo_height),
        };
        let todo_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(todo_height),
            width: area.width,
            height: todo_height,
        };
        Self {
            header_area,
            list_area,
            todo_area,
        }
    }
}

/// Minimum agents-panel height the expanded Vercel-style spawn modal
/// needs. Below this the popup falls back to a compact label-less
/// layout to avoid clipping rows (the default 20-row bottom panel can
/// leave only ~10 rows for the agents panel on short terminals).
const SPAWN_MODAL_EXPANDED_MIN_HEIGHT: u16 = 12;

/// Border rows contributed to the total popup height (top + bottom).
const POPUP_BORDER_ROWS: u16 = 2;

// Row offsets inside the inner area of the compact popup. MODE and
// AGENT always render; NAME only for worktree spawns, so the folder
// layout is one row shorter and the error/status row moves up.
const COMPACT_MODE_Y: u16 = 0;
const COMPACT_AGENT_Y: u16 = 1;
const COMPACT_NAME_Y: u16 = 2;
const COMPACT_ERROR_Y: u16 = 3;
const COMPACT_ERROR_FOLDER_Y: u16 = 2;

// Row offsets inside the inner area of the expanded Vercel popup.
// Each section is label → value with a blank spacer between them.
// Same conditional-NAME rule as the compact layout.
const EXP_MODE_LABEL_Y: u16 = 1;
const EXP_MODE_VALUE_Y: u16 = 2;
const EXP_AGENT_LABEL_Y: u16 = 4;
const EXP_AGENT_VALUE_Y: u16 = 5;
const EXP_NAME_LABEL_Y: u16 = 7;
const EXP_NAME_VALUE_Y: u16 = 8;
const EXP_ERROR_Y: u16 = 10;
const EXP_ERROR_FOLDER_Y: u16 = 7;

pub(super) fn render_spawn_input_popup(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let PopupState::SpawnInput {
        input,
        kind_idx,
        agent_idx,
        field,
        anchor_y,
        error,
        working,
        ..
    } = &state.popup
    else {
        return;
    };
    let input = input.clone();
    let field = *field;
    let anchor_y = *anchor_y;
    let error = error.clone();
    let working = *working;
    let kind = crate::worktree::SPAWN_MODES
        .get(*kind_idx)
        .copied()
        .unwrap_or("");
    let has_name = *kind_idx == 1;
    let agent = crate::worktree::AGENTS
        .get(*agent_idx)
        .copied()
        .unwrap_or("");
    let theme = &state.theme;

    let popup_width = area.width.min(32).max(area.width.min(14));
    let compact = area.height < SPAWN_MODAL_EXPANDED_MIN_HEIGHT;
    // The NAME row exists only for worktree spawns; the folder layout
    // is one section shorter in both orientations.
    let content_rows: u16 = if compact {
        if has_name { 4 } else { 3 }
    } else if has_name {
        10
    } else {
        8
    };
    // The bottom row is either the in-flight status or the inline error.
    let status_rows: u16 = u16::from(working.is_some() || error.is_some());
    let popup_height = content_rows + status_rows + POPUP_BORDER_ROWS;
    let popup_rect = match anchor_y {
        Some(y) => anchor_below(area, y, popup_width, popup_height),
        None => center_popup(area, popup_width, popup_height),
    };
    state.popup.set_spawn_input_area(Some(popup_rect));

    frame.render_widget(Clear, popup_rect);
    paint_background(frame, theme, popup_rect);
    let title_trunc =
        truncate_to_width(" Spawn agent ", popup_rect.width.saturating_sub(2) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            title_trunc,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    // Row 0 is left blank as a top gutter in expanded mode. Content
    // rows get one column of left padding so they don't hug the border.
    let render_at = |frame: &mut Frame, y_offset: u16, spans: Vec<Span<'_>>| {
        if y_offset < inner.height {
            let row = Rect::new(
                inner.x + 1,
                inner.y + y_offset,
                inner.width.saturating_sub(2),
                1,
            );
            frame.render_widget(Paragraph::new(Line::from(spans)), row);
        }
    };

    let label_style = |target: SpawnField| {
        let base = Style::default().add_modifier(Modifier::BOLD);
        if field == target {
            base.fg(theme.accent)
        } else {
            base.fg(theme.text_muted)
        }
    };
    let value_style = |target: SpawnField| {
        if field == target {
            Style::default().fg(theme.text_active)
        } else {
            Style::default().fg(theme.text_muted)
        }
    };

    let content_width = inner.width.saturating_sub(2) as usize;
    let visible_input = tail_fit(&input, content_width.saturating_sub(1));
    let mut name_spans: Vec<Span<'_>> =
        vec![Span::styled(visible_input, value_style(SpawnField::Name))];
    if field == SpawnField::Name {
        name_spans.push(Span::styled("█", Style::default().fg(theme.accent)));
    }
    let agent_value = truncate_to_width(agent, content_width);
    let kind_value = truncate_to_width(kind, content_width);
    // A working status renders in the muted color at the same row the
    // inline error uses; the two never show together (dispatch clears
    // the error, and keys are swallowed until the result lands).
    let status_spans = if let Some(status) = working {
        Some(vec![Span::styled(
            truncate_to_width(status, content_width),
            Style::default().fg(theme.text_muted),
        )])
    } else {
        error.as_ref().map(|err| {
            vec![Span::styled(
                truncate_to_width(err, content_width),
                Style::default().fg(theme.status_error),
            )]
        })
    };

    if compact {
        render_at(
            frame,
            COMPACT_MODE_Y,
            vec![Span::styled(kind_value, value_style(SpawnField::Mode))],
        );
        render_at(
            frame,
            COMPACT_AGENT_Y,
            vec![Span::styled(agent_value, value_style(SpawnField::Agent))],
        );
        if has_name {
            render_at(frame, COMPACT_NAME_Y, name_spans);
        }
        if let Some(status) = status_spans {
            let error_y = if has_name {
                COMPACT_ERROR_Y
            } else {
                COMPACT_ERROR_FOLDER_Y
            };
            render_at(frame, error_y, status);
        }
    } else {
        render_at(
            frame,
            EXP_MODE_LABEL_Y,
            vec![Span::styled("MODE", label_style(SpawnField::Mode))],
        );
        render_at(
            frame,
            EXP_MODE_VALUE_Y,
            vec![Span::styled(kind_value, value_style(SpawnField::Mode))],
        );
        render_at(
            frame,
            EXP_AGENT_LABEL_Y,
            vec![Span::styled("AGENT", label_style(SpawnField::Agent))],
        );
        render_at(
            frame,
            EXP_AGENT_VALUE_Y,
            vec![Span::styled(agent_value, value_style(SpawnField::Agent))],
        );
        if has_name {
            render_at(
                frame,
                EXP_NAME_LABEL_Y,
                vec![Span::styled("NAME", label_style(SpawnField::Name))],
            );
            render_at(frame, EXP_NAME_VALUE_Y, name_spans);
        }
        if let Some(status) = status_spans {
            let error_y = if has_name {
                EXP_ERROR_Y
            } else {
                EXP_ERROR_FOLDER_Y
            };
            render_at(frame, error_y, status);
        }
    }
}

/// Keep only the trailing `max_width` display cells of `text` so the
/// cursor at the end stays visible in a narrow input box. Prepends `…`
/// when truncation is applied.
fn tail_fit(text: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_string();
    }
    let budget = max_width.saturating_sub(1);
    let mut taken = 0usize;
    let mut byte_start = text.len();
    for (i, ch) in text.char_indices().rev() {
        let w = ch.width().unwrap_or(0);
        if taken + w > budget {
            break;
        }
        taken += w;
        byte_start = i;
    }
    let mut out = String::with_capacity(3 + (text.len() - byte_start));
    out.push('…');
    out.push_str(&text[byte_start..]);
    out
}

pub(super) fn render_remove_confirm_popup(frame: &mut Frame, state: &mut AppState, area: Rect) {
    let (label, has_worktree, warning, error, working) = match &state.popup {
        PopupState::RemoveConfirm {
            label,
            has_worktree,
            warning,
            error,
            working,
            ..
        } => (
            label.clone(),
            *has_worktree,
            warning.clone(),
            error.clone(),
            *working,
        ),
        _ => return,
    };
    let theme = &state.theme;

    // Narrow-friendly: put the pane label in the title, keep option
    // rows short enough to fit in ~16 columns. The consequence block
    // grows the popup; an in-flight status or inline error reserves one
    // more row (the two never show together — same contract as the
    // spawn modal). Folder spawns have no worktree to remove, so the
    // dialog degenerates to a plain window-close confirm.
    let option_rows: u16 = if has_worktree { 3 } else { 2 };
    let popup_height: u16 =
        option_rows + 2 + warning.len() as u16 + u16::from(error.is_some() || working.is_some());
    let popup_rect = center_popup(area, area.width.min(28), popup_height);
    state.popup.set_remove_confirm_area(Some(popup_rect));
    frame.render_widget(Clear, popup_rect);
    paint_background(frame, theme, popup_rect);
    let title_text = format!(" {label} ");
    let title = truncate_to_width(&title_text, popup_rect.width.saturating_sub(2) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.status_error))
        .title(Span::styled(title, Style::default().fg(theme.status_error)));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let render_row = |frame: &mut Frame, y_offset: u16, text: &str, style: Style| {
        if y_offset < inner.height {
            let row = Rect::new(inner.x, inner.y + y_offset, inner.width, 1);
            let truncated = truncate_to_width(text, row.width as usize);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(truncated, style))),
                row,
            );
        }
    };

    if has_worktree {
        render_row(
            frame,
            0,
            "[y] remove worktree",
            Style::default().fg(theme.status_error),
        );
        render_row(
            frame,
            1,
            "[c] close window only",
            Style::default().fg(theme.text_active),
        );
        render_row(
            frame,
            2,
            "[n] cancel",
            Style::default().fg(theme.text_muted),
        );
    } else {
        render_row(
            frame,
            0,
            "[y] close window",
            Style::default().fg(theme.status_error),
        );
        render_row(
            frame,
            1,
            "[n] cancel",
            Style::default().fg(theme.text_muted),
        );
    }
    // Force-remove consequences (`worktree remove --force` + `branch -D`
    // run while the agent is still working) — the user must see what is
    // unrecoverable before pressing y. Folder spawns instead show the
    // static "window only" notice.
    let warning_style = Style::default().fg(theme.status_error);
    for (i, line) in warning.iter().enumerate() {
        render_row(frame, option_rows + i as u16, line, warning_style);
    }
    // The bottom row is either the in-flight status or the inline error —
    // same contract as the spawn modal (dispatch clears the error before
    // the operation starts, and keys are swallowed until the result lands).
    if let Some(status) = working {
        render_row(
            frame,
            option_rows + warning.len() as u16,
            status,
            Style::default().fg(theme.text_muted),
        );
    } else if let Some(err) = error {
        render_row(
            frame,
            option_rows + warning.len() as u16,
            &err,
            warning_style,
        );
    }
}

pub(super) fn render_repo_popup(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // Copy the palette out first: the copy-target write-back below
    // needs `&mut state` while the styles still reference the theme.
    let theme = state.theme.clone();
    let repos = state.repo_names();
    if repos.is_empty() {
        return;
    }

    // The menu merges the old standalone notices popup (version /
    // plugin / missing-hooks, with click-to-copy prompt slots) above the
    // repo list. When no notice exists the menu is just the repo list,
    // as before.
    let show_notices = crate::ui::notices::has_info(state);
    let repo_header = show_notices;
    let max_name_len = repos.iter().map(|r| display_width(r)).max().unwrap_or(3);

    // Width: padding(1 left + 1 right) + widest content + borders(2).
    // The floor of 12 keeps tiny menus readable, but the
    // `.min(area.width)` must apply LAST — on a narrow sidebar a `.max`
    // applied after the min would push the popup wider than its own
    // pane (the same clamp-order bug `center_popup`/`anchor_below`
    // already guard against).
    let max_inner = area.width.saturating_sub(4) as usize;
    let (notices_widest, notices_count) = if show_notices {
        crate::ui::notices::section_metrics(state, max_inner)
    } else {
        (0, 0)
    };
    let content_widest = max_name_len
        .max(if repo_header {
            display_width("Repos")
        } else {
            0
        })
        .max(notices_widest);
    let popup_width = (content_widest + 4).max(12).min(area.width as usize) as u16;

    // Height: section lines + a blank separator + optional "Repos"
    // header + one row per repo + borders(2), anchored below the single
    // header row and clamped to the space that remains.
    let repo_start_row = if show_notices {
        notices_count + 1 + 1
    } else {
        0
    };
    let inner_rows = repo_start_row + repos.len();
    let height_budget = area.height.saturating_sub(1);
    let popup_height = ((inner_rows as u16) + 2).max(3).min(height_budget);

    if popup_height < 3 || popup_width < 2 {
        state.popup.set_repo_area(None);
        state.notices.copy_targets.clear();
        return;
    }

    // Right-aligned, below the header row.
    let popup_x = area.x + area.width.saturating_sub(popup_width);
    let popup_y = area.y + 1;
    let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
    state.popup.set_repo_area(Some(popup_rect));

    frame.render_widget(Clear, popup_rect);
    paint_background(frame, &theme, popup_rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let inner_width = inner.width as usize;

    // Notices section first: same content the old popup rendered, with
    // its copy slots materialized at the menu's absolute coords. Rows
    // the popup's height clips must not keep live click targets.
    let mut row = 0usize;
    if show_notices {
        let (notices_lines, slots) = crate::ui::notices::section_lines(state, inner_width);
        for line in &notices_lines {
            if row >= inner.height as usize {
                break;
            }
            let line_rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
            frame.render_widget(Paragraph::new(vec![line.clone()]), line_rect);
            row += 1;
        }
        crate::ui::notices::materialize_copy_targets(state, &slots, inner, 0);
        // Blank separator between the sections.
        if row < inner.height as usize {
            row += 1;
        }
    } else {
        state.notices.copy_targets.clear();
    }

    if repo_header && row < inner.height as usize {
        let line_rect = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    " {}",
                    truncate_to_width("Repos", inner_width.saturating_sub(1))
                ),
                Style::default().fg(theme.accent),
            ))),
            line_rect,
        );
        row += 1;
    }

    // The repo list: selection index maps to `repo_start_row + i`.
    let repo_start = row as u16;
    state.popup.set_repo_start(repo_start);
    let visible_repo_rows = inner.height.saturating_sub(repo_start);
    for (i, name) in repos.iter().enumerate() {
        if i >= visible_repo_rows as usize {
            break;
        }

        let is_highlighted = i == state.repo_popup_selected();
        let is_current = match &state.global.repo_filter {
            RepoFilter::All => i == 0,
            RepoFilter::Repo(n) => *n == *name,
        };

        let truncated = truncate_to_width(name, inner_width.saturating_sub(1));
        let text = format!(" {}", truncated);
        let text_dw = display_width(&text);
        let padding = " ".repeat(inner_width.saturating_sub(text_dw));

        let style = if is_highlighted {
            Style::default()
                .fg(theme.text_active)
                .bg(theme.selection_bg)
        } else if is_current {
            Style::default().fg(theme.text_active)
        } else {
            Style::default().fg(theme.text_muted)
        };

        let line_rect = Rect::new(inner.x, inner.y + repo_start + i as u16, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{}{}", text, padding),
                style,
            ))),
            line_rect,
        );
    }
}

fn render_header_into(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // The chyron overlays the toolbar while an event is alive; the
    // toolbar owns the row the rest of the time. Either way the row is
    // reserved, so the list never shifts.
    if let Some((banner_text, banner_kind)) = state.chyron.visible_banner(state) {
        // The banner leaves the overview button's left columns and the
        // menu button's corner free, so both stay visible — and
        // clickable — over it.
        let button_w = (chyron::BUTTON_WIDTH as u16).min(area.width);
        let overview_w = crate::state::OVERVIEW_BUTTON_WIDTH.min(area.width);
        let banner_w = area
            .width
            .saturating_sub(button_w)
            .saturating_sub(overview_w);
        if banner_w > 0
            && let Some(line) = chyron::render_chyron(state, &banner_text, banner_kind, banner_w)
        {
            let banner_area = Rect::new(area.x + overview_w, area.y, banner_w, area.height);
            frame.render_widget(Paragraph::new(vec![line]), banner_area);
        }
        let overview_area = Rect::new(area.x, area.y, overview_w, area.height);
        let overview_line = Line::from(chyron::overview_button_spans(state));
        frame.render_widget(Paragraph::new(vec![overview_line]), overview_area);
        let button_x = area.x + area.width.saturating_sub(button_w);
        let button_area = Rect::new(button_x, area.y, button_w, area.height);
        let button_line = Line::from(chyron::menu_button_spans(state));
        frame.render_widget(Paragraph::new(vec![button_line]), button_area);
        // Same contract as the toolbar branch: the click column is
        // width - button width. Written on chyron frames too so a
        // first-frame banner still leaves the button clickable. The
        // width feeds the ticker's display-window math (the scroll
        // must finish before the close).
        state.layout.menu_button_col = Some(area.width.saturating_sub(button_w));
        state.layout.header_width = Some(area.width);
    } else {
        let (line, menu_btn_col) = chyron::render_secondary_header(state, area.width);
        state.layout.menu_button_col = menu_btn_col;
        state.layout.header_width = Some(area.width);
        frame.render_widget(Paragraph::new(vec![line]), area);
    }
}

fn compute_scroll_offset(state: &mut AppState, total_lines: usize, list_area: Rect) -> usize {
    state.scrolls.panes.total_lines = total_lines;
    state.scrolls.panes.visible_height = list_area.height as usize;
    // Clamp a stale offset to the new content size. When the list shrinks
    // (view switch, agents disappearing) the offset can point past the
    // last line and render an empty panel; the activity and git panels
    // already clamp the same way.
    state.scrolls.panes.scroll(0);

    // Auto-scroll to keep selected agent visible
    if state.focus_state.sidebar_focused && state.focus_state.focus == Focus::Panes {
        let mut first_line: Option<usize> = None;
        let mut last_line: Option<usize> = None;
        for (i, mapping) in state.layout.line_to_row.iter().enumerate() {
            if *mapping == Some(state.global.selected_pane_row) {
                if first_line.is_none() {
                    first_line = Some(i);
                }
                last_line = Some(i);
            }
        }
        if let (Some(first), Some(last)) = (first_line, last_line) {
            let visible_h = list_area.height as usize;
            let offset = state.scrolls.panes.offset;
            if first < offset {
                state.scrolls.panes.offset = first.saturating_sub(1);
            } else if last >= offset + visible_h {
                state.scrolls.panes.offset = (last + 1).saturating_sub(visible_h);
            }
        }
    }

    state.scrolls.panes.offset
}

fn render_pane_rows(
    frame: &mut Frame,
    lines: Vec<Line<'static>>,
    scroll_offset: usize,
    list_area: Rect,
) {
    let paragraph = Paragraph::new(lines).scroll((scroll_offset as u16, 0));
    frame.render_widget(paragraph, list_area);
}

fn render_flash_banner_into(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // Render flash banner (spawn / remove feedback) before popups so
    // popups stay on top.
    if let Some(text) = state.take_flash() {
        let flash_y = area.y + area.height.saturating_sub(1);
        let flash_rect = Rect::new(area.x, flash_y, area.width, 1);
        frame.render_widget(Clear, flash_rect);
        paint_background(frame, &state.theme, flash_rect);
        let theme = &state.theme;
        let color = if text.contains("failed") {
            theme.status_error
        } else {
            theme.accent
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))),
            flash_rect,
        );
    }
}

pub fn draw_agents(frame: &mut Frame, state: &mut AppState, area: Rect) {
    // Collect the agent rows up front: the TODO panel sizes itself to
    // the rows the agent list doesn't use, so it needs the rendered line
    // count before the layout is computed. Width drives the row count,
    // not height, so `area.width` matches the later `list_area`.
    let row_collector::CollectedRows {
        lines,
        line_to_row,
        pending_spawn,
        pending_remove,
        pending_header,
        pending_window_header,
    } = row_collector::collect(state, area.width);
    let todo_height = todo::panel_height(state, area, lines.len());
    let layout = PaneLayout::compute_with_todo(area, todo_height);
    state.layout.todo_area = layout.todo_area;
    state.layout.toolbar_row = layout.header_area.y;
    render_header_into(frame, state, layout.header_area);

    state.layout.line_to_row = line_to_row;
    let scroll_offset = compute_scroll_offset(state, lines.len(), layout.list_area);
    click_targets::materialize(
        state,
        pending_spawn,
        pending_remove,
        pending_header,
        pending_window_header,
        scroll_offset,
        layout.list_area,
    );
    render_pane_rows(frame, lines, scroll_offset, layout.list_area);

    todo::render_todo_panel(frame, state, layout.todo_area);

    render_flash_banner_into(frame, state, area);
    popups::render_if_open(frame, state, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_layout_splits_area_into_header_and_list() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let layout = PaneLayout::compute_with_todo(area, 0);
        assert_eq!(layout.header_area.x, 0);
        assert_eq!(layout.header_area.y, 0);
        assert_eq!(layout.header_area.width, 40);
        assert_eq!(layout.header_area.height, 1);
        assert_eq!(layout.list_area.y, 1);
        assert_eq!(layout.list_area.height, 19);
        assert_eq!(layout.list_area.width, 40);
    }

    #[test]
    fn pane_layout_handles_tiny_area() {
        // Only 1 row available — the header takes it, the list collapses.
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 1,
        };
        let layout = PaneLayout::compute_with_todo(area, 0);
        assert_eq!(layout.header_area.height, 1);
        assert_eq!(layout.list_area.height, 0);
    }

    #[test]
    fn pane_layout_handles_zero_height() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 0,
        };
        let layout = PaneLayout::compute_with_todo(area, 0);
        assert_eq!(layout.header_area.height, 0);
        assert_eq!(layout.list_area.height, 0);
    }

    #[test]
    fn pane_layout_reserves_todo_panel_at_bottom() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let layout = PaneLayout::compute_with_todo(area, 6);
        assert_eq!(layout.list_area.y, 1);
        assert_eq!(layout.list_area.height, 13);
        assert_eq!(layout.todo_area.y, 14);
        assert_eq!(layout.todo_area.height, 6);
    }

    #[test]
    fn pane_layout_clamps_todo_panel_to_header_space() {
        // A panel taller than the pane leaves the header row intact
        // and collapses the list to zero.
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 5,
        };
        let layout = PaneLayout::compute_with_todo(area, 10);
        assert_eq!(layout.header_area.height, 1);
        assert_eq!(layout.todo_area.height, 4);
        assert_eq!(layout.list_area.height, 0);
    }

    #[test]
    fn pane_layout_respects_non_zero_origin() {
        let area = Rect {
            x: 5,
            y: 10,
            width: 30,
            height: 15,
        };
        let layout = PaneLayout::compute_with_todo(area, 0);
        assert_eq!(layout.header_area.x, 5);
        assert_eq!(layout.header_area.y, 10);
        assert_eq!(layout.list_area.x, 5);
        assert_eq!(layout.list_area.y, 11);
        assert_eq!(layout.list_area.height, 14);
    }

    #[test]
    fn spawn_target_rects_cover_rendered_spawn_buttons() {
        // Contract between the repo-header render in row_collector (title
        // padded so SPAWN_BUTTON sits at the right edge) and the
        // click-target math in click_targets::materialize (rect anchored
        // at list_area.x + width - btn_width): every rendered `+` glyph
        // must land inside one of the registered RepoSpawnTarget rects,
        // mirroring the remove-`×` column contract test in
        // ui/panes/row.rs.
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::layout::Position;

        let mut state = AppState::new("%99".into());
        let pane = crate::tmux::PaneInfo {
            pane_id: "%1".into(),
            pane_active: false,
            status: crate::tmux::PaneStatus::Running,
            attention: crate::tmux::PaneAttention::None,
            agent: crate::tmux::AgentType::Claude,
            path: "/tmp/myrepo".into(),
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
            session_name: String::new(),
            tmux_session: String::new(),
            window_index: 0,
            window_name: String::new(),
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        };
        state.repo_groups = vec![crate::group::RepoGroup {
            name: "myrepo".into(),
            has_focus: false,
            panes: vec![(
                pane,
                crate::group::PaneGitInfo {
                    repo_root: Some("/tmp/myrepo".into()),
                    branch: None,
                    is_worktree: false,
                    ..Default::default()
                },
            )],
        }];

        let area = Rect::new(0, 0, 40, 10);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| draw_agents(frame, &mut state, area))
            .unwrap();
        let buf = terminal.backend().buffer();

        assert_eq!(
            state.layout.repo_spawn_targets.len(),
            1,
            "a repo group with a repo_root must register exactly one spawn target"
        );
        let target = &state.layout.repo_spawn_targets[0];
        assert_eq!(target.repo_name, "myrepo");
        assert_eq!(target.rect.width, SPAWN_BUTTON.len() as u16);
        assert_eq!(
            target.rect.x,
            area.x + area.width - SPAWN_BUTTON.len() as u16,
            "spawn rect must pin to the right edge of the list area"
        );

        let mut rendered_buttons = 0;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if buf[(x, y)].symbol() == SPAWN_BUTTON {
                    rendered_buttons += 1;
                    assert!(
                        target.rect.contains(Position { x, y }),
                        "rendered `+` at ({x},{y}) is not covered by the spawn target rect {:?}",
                        target.rect
                    );
                }
            }
        }
        assert_eq!(
            rendered_buttons, 1,
            "exactly one `+` spawn button should render"
        );
    }
}
