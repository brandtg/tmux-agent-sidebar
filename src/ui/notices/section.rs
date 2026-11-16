//! Notices content as composable lines for the header menu.
//!
//! The old standalone notices popup became a section of the merged
//! header menu (see the 2026-11-15 chyron spec): the menu renderer asks
//! for the section's metrics first, sizes the popup, then requests the
//! lines at the final inner width so the right-edge `[copy]` / `[prompt]`
//! label slots line up with the popup border.

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::button::missing_hooks_has_copy_button;
use crate::state::{AppState, ClaudePluginNotice, NoticesCopyTarget, debug_forced_display};
use crate::tmux::CLAUDE_AGENT;
use crate::ui::text::{display_width, truncate_to_width};

const COPY_LABEL: &str = "[copy]";
const COPIED_LABEL: &str = "[copied]";
const PROMPT_LABEL: &str = "[prompt]";

/// How long the `[copied]` label remains after a successful copy before
/// reverting to `[copy]`.
const COPIED_FEEDBACK_DURATION: std::time::Duration = std::time::Duration::from_secs(1);

/// Maximum display width of any clickable label
/// (`[copy]` / `[copied]` / `[prompt]`). Used to reserve constant space
/// so the menu layout does not shift when switching between states.
pub(in crate::ui) const LABEL_MAX_WIDTH: usize = {
    let a = COPY_LABEL.len();
    let b = COPIED_LABEL.len();
    let c = PROMPT_LABEL.len();
    let ab = if a > b { a } else { b };
    if ab > c { ab } else { c }
};

const SECTION_INDENT: &str = "   ";
const ITEM_INDENT: &str = "     ";
const SUBITEM_INDENT: &str = "     ";

fn version_text(notice: Option<&crate::version::UpdateNotice>) -> Option<String> {
    if debug_forced_display() {
        Some(match notice {
            Some(notice) => format!("v{} -> v{}", notice.local_version, notice.latest_version),
            None => format!("v{} -> v{}", crate::VERSION, crate::VERSION),
        })
    } else {
        notice.map(|notice| format!("v{} -> v{}", notice.local_version, notice.latest_version))
    }
}

/// Description of how the `Plugin / claude` sub-item should render.
/// `body` is the text that appears after `- ` on the sub-item line, and
/// `show_prompt_button` toggles the right-aligned `[prompt]` clickable
/// label (set for both `InstallRecommended` and `DuplicateHooks`, since
/// both states are resolved by the same migration recipe).
struct PluginSubItem {
    body: String,
    show_prompt_button: bool,
}

fn plugin_subitem(notice: Option<&ClaudePluginNotice>) -> Option<PluginSubItem> {
    match (debug_forced_display(), notice) {
        (_, Some(ClaudePluginNotice::InstallRecommended)) => Some(PluginSubItem {
            body: "migrate".to_string(),
            show_prompt_button: true,
        }),
        (_, Some(ClaudePluginNotice::DuplicateHooks)) => Some(PluginSubItem {
            body: "cleanup".to_string(),
            show_prompt_button: true,
        }),
        (_, Some(ClaudePluginNotice::Stale)) => Some(PluginSubItem {
            body: "run /plugin update".to_string(),
            show_prompt_button: false,
        }),
        // Debug forced-display fallback when no real notice is set: show
        // the Stale hint so layout is exercised.
        (true, None) => Some(PluginSubItem {
            body: "run /plugin update".to_string(),
            show_prompt_button: false,
        }),
        (false, None) => None,
    }
}

/// Width (display columns) and line count the section renders at, given
/// the widest inner width it is allowed to use. Call before
/// [`section_lines`] so the menu can size itself.
pub(in crate::ui) fn section_metrics(state: &AppState, max_inner_width: usize) -> (usize, usize) {
    let mut widest = 0usize;
    let mut count = 0usize;
    let probe = |text: &str| display_width(text).min(max_inner_width);
    if let Some(text) = version_text(state.version_notice.as_ref()) {
        widest = widest.max(probe(&format!("{}New Version", SECTION_INDENT)));
        widest = widest.max(probe(&format!("{}{}", ITEM_INDENT, text)));
        count += 2;
    }
    if let Some(sub) = plugin_subitem(state.notices.claude_plugin_notice.as_ref()) {
        widest = widest.max(probe(&format!("{}Plugin", SECTION_INDENT)));
        widest = widest.max(probe(&format!("{}claude", ITEM_INDENT)));
        let head = format!("{}  {}", ITEM_INDENT, sub.body);
        let sub_width = if sub.show_prompt_button {
            display_width(&head) + 2 + LABEL_MAX_WIDTH
        } else {
            display_width(&head)
        };
        widest = widest.max(probe(&head).max(sub_width.min(max_inner_width)));
        count += 3;
    }
    let groups = &state.notices.missing_hook_groups;
    widest = widest.max(probe(&format!("{}Missing hooks", SECTION_INDENT)));
    count += 1;
    if groups.is_empty() {
        widest = widest.max(probe(&format!("{}No missing hooks", ITEM_INDENT)));
        count += 1;
    }
    for group in groups {
        let group_width = if missing_hooks_has_copy_button(&group.agent) {
            display_width(ITEM_INDENT) + display_width(&group.agent) + 2 + LABEL_MAX_WIDTH
        } else {
            display_width(ITEM_INDENT) + display_width(&group.agent)
        };
        widest = widest.max(probe(&group.agent).max(group_width.min(max_inner_width)));
        count += 1;
        for hook in &group.hooks {
            widest = widest.max(probe(&format!("{}- {}", SUBITEM_INDENT, hook)));
            count += 1;
        }
    }
    (widest, count)
}

/// A clickable label row: `row` is the line index the slot sits on
/// (relative to the section's first line); the slot spans
/// `LABEL_MAX_WIDTH` ending at the menu's right inner edge.
pub(in crate::ui) struct CopySlot {
    pub(in crate::ui) row: usize,
    pub(in crate::ui) agent: String,
}

/// Build the section lines at exactly `inner_width` display columns.
/// Returns the lines plus the click slots; the caller offsets them into
/// screen coordinates and drops any that fall outside the clipped
/// popup. Returns an empty vec when there is nothing to show.
pub(in crate::ui) fn section_lines(
    state: &AppState,
    inner_width: usize,
) -> (Vec<Line<'static>>, Vec<CopySlot>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut slots: Vec<CopySlot> = Vec::new();
    if inner_width == 0 {
        return (lines, slots);
    }

    let push_padded = |lines: &mut Vec<Line<'static>>, text: String, style: Style| {
        let padding = " ".repeat(inner_width.saturating_sub(display_width(&text)));
        lines.push(Line::from(Span::styled(
            format!("{}{}", text, padding),
            style,
        )));
    };

    let copied_agent: Option<String> = state
        .notices
        .copied_at
        .as_ref()
        .filter(|(_, at)| at.elapsed() < COPIED_FEEDBACK_DURATION)
        .map(|(agent, _)| agent.clone());

    if let Some(text) = version_text(state.version_notice.as_ref()) {
        push_padded(
            &mut lines,
            format!("{}New Version", SECTION_INDENT),
            Style::default().fg(state.theme.accent),
        );
        push_padded(
            &mut lines,
            format!("{}{}", ITEM_INDENT, text),
            Style::default().fg(state.theme.status_waiting),
        );
    }

    if let Some(sub) = plugin_subitem(state.notices.claude_plugin_notice.as_ref()) {
        push_padded(
            &mut lines,
            format!("{}Plugin", SECTION_INDENT),
            Style::default().fg(state.theme.accent),
        );
        push_padded(
            &mut lines,
            format!("{}claude", ITEM_INDENT),
            Style::default()
                .fg(state.theme.accent)
                .add_modifier(ratatui::style::Modifier::BOLD),
        );
        let head = format!("{}  {}", ITEM_INDENT, sub.body);
        if sub.show_prompt_button && inner_width >= display_width(&head) + 2 + LABEL_MAX_WIDTH {
            // Space-between layout: body on the left, `[prompt]` slot
            // pinned to the right edge so the click hit region stays in
            // a constant column even if the body width changes.
            let is_copied = copied_agent.as_deref() == Some(CLAUDE_AGENT);
            let (label_text, label_color) = if is_copied {
                (COPIED_LABEL, state.theme.status_running)
            } else {
                (PROMPT_LABEL, state.theme.status_waiting)
            };
            let head_width = display_width(&head);
            let label_width = display_width(label_text);
            let gap_before_label = inner_width - head_width - label_width;
            slots.push(CopySlot {
                row: lines.len(),
                agent: CLAUDE_AGENT.to_string(),
            });
            lines.push(Line::from(vec![
                Span::styled(head, Style::default().fg(state.theme.status_waiting)),
                Span::raw(" ".repeat(gap_before_label)),
                Span::styled(label_text.to_string(), Style::default().fg(label_color)),
            ]));
        } else {
            push_padded(
                &mut lines,
                head,
                Style::default().fg(state.theme.status_waiting),
            );
        }
    }

    let groups = &state.notices.missing_hook_groups;
    push_padded(
        &mut lines,
        format!("{}Missing hooks", SECTION_INDENT),
        Style::default().fg(state.theme.accent),
    );

    for group in groups {
        let agent_text = truncate_to_width(&group.agent, inner_width.saturating_sub(1));
        let agent_width = display_width(&agent_text);
        let has_prompt = missing_hooks_has_copy_button(&group.agent);

        if has_prompt
            && inner_width >= display_width(ITEM_INDENT) + agent_width + 2 + LABEL_MAX_WIDTH
        {
            let is_copied = copied_agent.as_deref() == Some(group.agent.as_str());
            let (label_text, label_color) = if is_copied {
                (COPIED_LABEL, state.theme.status_running)
            } else {
                (COPY_LABEL, state.theme.status_waiting)
            };

            // Space-between layout: agent name stays on the left, the
            // label is pushed to the right edge of the menu. The click
            // target always covers the full `LABEL_MAX_WIDTH` slot so
            // the hit region does not shift when the label flips
            // between `[copy]` (6) and `[copied]` (8).
            let head = format!("{}{}", ITEM_INDENT, agent_text);
            let head_width = display_width(&head);
            let label_width = display_width(label_text);
            let gap_before_label = inner_width - head_width - label_width;

            slots.push(CopySlot {
                row: lines.len(),
                agent: group.agent.clone(),
            });

            lines.push(Line::from(vec![
                Span::styled(
                    head,
                    Style::default()
                        .fg(state.theme.accent)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::raw(" ".repeat(gap_before_label)),
                Span::styled(label_text.to_string(), Style::default().fg(label_color)),
            ]));
        } else {
            push_padded(
                &mut lines,
                format!("{}{}", ITEM_INDENT, agent_text),
                Style::default()
                    .fg(state.theme.accent)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            );
        }

        for hook in &group.hooks {
            let truncated = truncate_to_width(hook, inner_width.saturating_sub(3));
            push_padded(
                &mut lines,
                format!("{}- {}", SUBITEM_INDENT, truncated),
                Style::default().fg(state.theme.text_muted),
            );
        }
    }
    if groups.is_empty() {
        let empty = truncate_to_width("No missing hooks", inner_width.saturating_sub(1));
        push_padded(
            &mut lines,
            format!("{}{}", ITEM_INDENT, empty),
            Style::default().fg(state.theme.text_muted),
        );
    }

    (lines, slots)
}

/// Materialize the section's click slots into absolute-rect targets,
/// dropping any whose row was clipped by the popup height.
pub(in crate::ui) fn materialize_copy_targets(
    state: &mut AppState,
    slots: &[CopySlot],
    inner: ratatui::layout::Rect,
    section_first_row: usize,
) {
    let label_slot_start = inner.width.saturating_sub(LABEL_MAX_WIDTH as u16);
    state.notices.copy_targets = slots
        .iter()
        .filter(|slot| section_first_row + slot.row < inner.height as usize)
        .map(|slot| NoticesCopyTarget {
            area: ratatui::layout::Rect::new(
                inner.x + label_slot_start,
                inner.y + (section_first_row + slot.row) as u16,
                LABEL_MAX_WIDTH as u16,
                1,
            ),
            agent: slot.agent.clone(),
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::NoticesMissingHookGroup;

    fn state_with(version: Option<(&str, &str)>, groups: Vec<(&str, Vec<&str>)>) -> AppState {
        let mut state = AppState::new(String::new());
        state.version_notice = version.map(|(local, latest)| crate::version::UpdateNotice {
            local_version: local.into(),
            latest_version: latest.into(),
        });
        state.notices.missing_hook_groups = groups
            .into_iter()
            .map(|(agent, hooks)| NoticesMissingHookGroup {
                agent: agent.into(),
                hooks: hooks.into_iter().map(String::from).collect(),
            })
            .collect();
        state
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn section_lines_render_version_and_hooks() {
        let state = state_with(Some(("0.2.6", "0.2.7")), vec![("codex", vec!["Stop"])]);
        let (lines, slots) = section_lines(&state, 40);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts[0].trim_end() == "   New Version");
        assert!(texts[1].trim_end() == "     v0.2.6 -> v0.2.7");
        assert!(texts[2].trim_end() == "   Missing hooks");
        assert!(texts[3].starts_with("     codex"), "got: {}", texts[3]);
        assert!(texts[3].trim_end().ends_with("[copy]"));
        assert!(texts[4].trim_end() == "     - Stop");
        // One clickable slot, on the codex row.
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].row, 3);
        assert_eq!(slots[0].agent, "codex");
    }

    #[test]
    fn section_lines_empty_when_padding_is_zero() {
        let state = state_with(Some(("0.2.6", "0.2.7")), vec![]);
        let (lines, slots) = section_lines(&state, 0);
        assert!(lines.is_empty());
        assert!(slots.is_empty());
    }

    #[test]
    fn section_metrics_counts_every_line() {
        let state = state_with(Some(("0.2.6", "0.2.7")), vec![("codex", vec!["a", "b"])]);
        let (_, count) = section_metrics(&state, 60);
        // version header + version text + missing hooks header + codex + 2 hooks
        assert_eq!(count, 6);
    }
}
