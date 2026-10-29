use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::process::{command_basename, strip_exe_suffix};
use crate::tmux::PaneStatus;
use crate::ui::colors::ColorTheme;
use crate::ui::icons::StatusIcons;
use crate::ui::text::{display_width, elapsed_label, truncate_to_width};

mod body;
mod branch;
mod ctx;
mod status;

use body::{
    background_hint_row, idle_hint_row, prompt_rows, subagent_rows, task_progress_row,
    wait_reason_row,
};
use branch::branch_ports_row;
use ctx::{RowCtx, SELECTION_MARKER};
use status::{permission_badge_color, running_icon_for, status_row};

pub(super) use branch::sidebar_remove_marker_col;

#[allow(clippy::too_many_arguments)]
pub(super) fn render_pane_lines_with_ports(
    pane: &crate::tmux::PaneInfo,
    git_info: &crate::group::PaneGitInfo,
    ports: Option<&[u16]>,
    task_progress: Option<&crate::activity::TaskProgress>,
    selected: bool,
    active: bool,
    flagged: bool,
    width: usize,
    icons: &StatusIcons,
    theme: &ColorTheme,
    spinner_frame: usize,
    now: u64,
) -> Vec<Line<'static>> {
    let bg = if selected {
        Some(theme.selection_bg)
    } else {
        None
    };
    let apply_bg = |style: Style| match bg {
        Some(c) => style.bg(c),
        None => style,
    };
    // The left marker `┃` highlights the pane that is currently focused in
    // tmux (`active`). To keep the active accent compact, it only appears on
    // the status row and the branch/ports row (when present) — never on
    // deeper details like task progress or prompt wrapping. The sidebar
    // cursor position (`selected`) still paints the full pane with the
    // selection background. A flagged row ("inbox pin", `f` key) renders
    // the same gutter in the flag color on those same rows — the flag
    // wins when a row is both, so a pinned row stays findable.
    let marker_ctx = RowCtx {
        marker_char: if active || flagged {
            SELECTION_MARKER
        } else {
            " "
        },
        marker_style: if flagged {
            apply_bg(Style::default().fg(theme.flag))
        } else if active {
            apply_bg(Style::default().fg(theme.accent))
        } else {
            apply_bg(Style::default())
        },
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active,
    };
    let plain_ctx = RowCtx {
        marker_char: " ",
        marker_style: Style::default(),
        inner_width: width.saturating_sub(2),
        theme,
        bg: None,
        active,
    };

    let mut out: Vec<Line<'static>> = Vec::with_capacity(8);
    out.push(status_row(pane, &marker_ctx, icons, spinner_frame, now));
    if let Some(line) = branch_ports_row(git_info, ports, pane.sidebar_spawned, &marker_ctx) {
        out.push(line);
    }
    let ctx = &plain_ctx;
    if let Some(line) = task_progress_row(task_progress, ctx) {
        out.push(line);
    }
    out.extend(subagent_rows(&pane.subagents, ctx));
    if let Some(line) = wait_reason_row(&pane.wait_reason, &pane.status, ctx) {
        out.push(line);
    }
    if let Some(cmd) = pane.bg_shell_cmd.as_deref() {
        out.push(background_hint_row(ctx, cmd));
    }
    if !pane.prompt.is_empty() {
        out.extend(prompt_rows(pane, ctx));
    } else if matches!(pane.status, PaneStatus::Idle) {
        out.push(idle_hint_row(ctx));
    }
    out
}

/// One line per pane, used when compact mode is on.
///
/// Density rules, in priority order: the status icon, the session title,
/// and the elapsed counter always render; the permission badge and
/// subagent count render when space remains; the branch fills whatever
/// room is left and is dropped first in narrow sidebars (the session
/// name outranks it, and the full branch/ports row is a detail-only
/// affordance). Wait reasons collapse into the status icon's color — the
/// colored glyph alone is enough in compact mode.
#[allow(clippy::too_many_arguments)]
pub(super) fn compact_row(
    pane: &crate::tmux::PaneInfo,
    git_info: &crate::group::PaneGitInfo,
    selected: bool,
    active: bool,
    flagged: bool,
    width: usize,
    icons: &StatusIcons,
    theme: &ColorTheme,
    spinner_frame: usize,
    now: u64,
) -> Vec<Line<'static>> {
    let bg = if selected {
        Some(theme.selection_bg)
    } else {
        None
    };
    let apply_bg = |style: Style| match bg {
        Some(c) => style.bg(c),
        None => style,
    };
    // Same gutter contract as the full row: flag wins over active (see
    // `render_pane_lines_with_ports`).
    let ctx = RowCtx {
        marker_char: if active || flagged {
            SELECTION_MARKER
        } else {
            " "
        },
        marker_style: if flagged {
            apply_bg(Style::default().fg(theme.flag))
        } else if active {
            apply_bg(Style::default().fg(theme.accent))
        } else {
            apply_bg(Style::default())
        },
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active,
    };

    let (icon, pulse_color) = running_icon_for(&pane.status, spinner_frame, icons);
    let icon_color = pulse_color
        .or_else(|| theme.attention_color(pane.attention, spinner_frame))
        .unwrap_or_else(|| theme.status_color(&pane.status));
    let title_raw: &str = if pane.session_name.is_empty() {
        pane.agent.label()
    } else {
        &pane.session_name
    };
    let badge = pane.permission_mode.badge();
    let elapsed = elapsed_label(pane.started_at, now);

    // The trailing `×` remove affordance for sidebar-spawned worktrees
    // pins to the rightmost column exactly like the full branch row, so
    // the `sidebar_remove_marker_col` click-target math stays valid.
    let remove_marker =
        sidebar_remove_marker_col(git_info, None, pane.sidebar_spawned, ctx.inner_width).is_some();

    let mut right_width = display_width(&elapsed);
    if remove_marker {
        right_width += 2; // " ×"
    }

    let icon_w = display_width(icon);
    let title_budget = ctx.inner_width.saturating_sub(icon_w + 1 + right_width);
    let title = truncate_to_width(title_raw, title_budget);

    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(5);
    let mut left_width = icon_w + 1 + display_width(&title);
    left_spans.push(Span::styled(
        icon.to_string(),
        ctx.apply_bg(Style::default().fg(icon_color)),
    ));
    left_spans.push(Span::styled(
        format!(" {}", title),
        ctx.apply_bg(Style::default().fg(theme.agent_color(&pane.agent))),
    ));

    // Extras render only when they fit, so a narrow sidebar degrades to
    // icon + title + elapsed instead of truncating the title away.
    let mut leftover = ctx.inner_width.saturating_sub(left_width + right_width);
    if !badge.is_empty() {
        let w = 1 + display_width(badge);
        if leftover >= w {
            left_spans.push(Span::styled(
                format!(" {}", badge),
                ctx.apply_bg(
                    Style::default().fg(permission_badge_color(&pane.permission_mode, theme)),
                ),
            ));
            left_width += w;
            leftover -= w;
        }
    }
    let subagent_count = pane.subagents.len();
    if subagent_count > 0 {
        let text = format!(" +{}", subagent_count);
        let w = display_width(&text);
        if leftover >= w {
            left_spans.push(Span::styled(
                text,
                ctx.apply_bg(Style::default().fg(theme.subagent)),
            ));
            left_width += w;
            leftover -= w;
        }
    }
    let branch = crate::ui::text::branch_label(git_info);
    if !branch.is_empty() && leftover >= 3 {
        // One separator space + at least 2 branch characters, else the
        // fragment is noise; truncation is handled by truncate_to_width.
        let text = format!(" {}", truncate_to_width(&branch, leftover - 1));
        let w = display_width(&text);
        left_spans.push(Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(theme.branch)),
        ));
        left_width += w;
    }

    let elapsed_fg = if pane.status.is_active() {
        theme.text_active
    } else {
        theme.text_muted
    };
    let mut right_spans: Vec<Span<'static>> = Vec::with_capacity(2);
    right_spans.push(Span::styled(
        elapsed,
        ctx.apply_bg(Style::default().fg(elapsed_fg)),
    ));
    if remove_marker {
        right_spans.push(Span::styled(
            " ×".to_string(),
            ctx.apply_bg(Style::default().fg(theme.status_error)),
        ));
    }

    vec![ctx.row_line_split(left_spans, left_width, right_spans, right_width)]
}

/// Minimum display columns kept clear between the program name and the
/// right-aligned port list on a window row. Matches the two-space
/// separator the agent branch/ports row uses.
const MIN_PORT_GAP: usize = 2;

/// Build the right-aligned listening-port list for [`window_row`] within
/// `budget` display columns.
///
/// Ports arrive ascending from the scan (they are collected in a
/// `BTreeSet`), so the lowest ports render first. When the full
/// `:p1, p2, …` list does not fit, the highest ports are dropped and a
/// trailing `…` marks the remainder. `None` means there are no ports,
/// or none fit at all.
fn port_text_lowest_first(ports: Option<&[u16]>, budget: usize) -> Option<String> {
    let ports = ports?;
    if ports.is_empty() {
        return None;
    }
    let join = |slice: &[u16]| {
        slice
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let full = format!(":{}", join(ports));
    if display_width(&full) <= budget {
        return Some(full);
    }
    // Keep the lowest ports that still fit beside a trailing ellipsis.
    let mut kept: Vec<u16> = Vec::new();
    for port in ports {
        kept.push(*port);
        let candidate = format!(":{}\u{2026}", join(&kept));
        if display_width(&candidate) > budget {
            kept.pop();
            break;
        }
    }
    if kept.is_empty() {
        return None;
    }
    Some(format!(":{}\u{2026}", join(&kept)))
}

/// Lines for a non-agent window row.
///
/// Line 1 (both densities) uses the same gutter as agent rows. A diamond
/// glyph (`◇`/`◆`) replaces the agent circle so the two kinds of row are
/// distinguishable at a glance while color still conveys status.
///
/// Rendering uses two-tone styling: the binary name is highlighted in
/// [`theme.text_active`], followed by its cleaned arguments in
/// [`theme.text_muted`], giving immediate visual distinction between
/// runtime interpreters (`node`, `python`, `cargo`) and what they are running
/// (`server.js`, `manage.py runserver`, `test`).
///
/// In expanded mode the card grows a second detail line —
/// `<other dimension> · <window_name>` in [`theme.text_inactive`], indented
/// like the agent detail rows. "Other dimension" is whichever of
/// (session, repo) is *not* the group title, which `group_by` selects.
/// The window name is otherwise invisible while a foreground command
/// runs (line 1 only falls back to it when the command is empty), so the
/// card doubles as its label. The detail line is plain — no selection
/// background, no active marker — but still maps back to the row, so the
/// whole card is clickable. The card degrades back to its single status
/// line when line 2 has nothing to show, or when fewer than 3 meaningful
/// columns of it fit after the indent.
///
/// `finished` carries the completed foreground task tracked runtime-side.
/// A finished task overrides the plain status: exit 0 (or an unknown
/// exit, no shell integration) draws a hollow diamond in the
/// flashing-green done-attention color until the user focuses the pane;
/// a non-zero exit draws a solid red diamond plus an `exit N` note.
#[allow(clippy::too_many_arguments)]
pub(super) fn window_row(
    window: &crate::group::OtherWindow,
    ports: Option<&[u16]>,
    finished: Option<&crate::state::WindowFinished>,
    selected: bool,
    active: bool,
    flagged: bool,
    expanded: bool,
    group_by: crate::state::GroupBy,
    width: usize,
    icons: &StatusIcons,
    theme: &ColorTheme,
    spinner_frame: usize,
) -> Vec<Line<'static>> {
    use crate::tmux::WindowStatus;

    let bg = if selected {
        Some(theme.selection_bg)
    } else {
        None
    };
    let apply_bg = |style: Style| match bg {
        Some(c) => style.bg(c),
        None => style,
    };
    // Line 1 is the only gutter row of the window card, so the flag
    // rides on the same shared context as the active marker — and wins
    // over it (see `render_pane_lines_with_ports`).
    let ctx = RowCtx {
        marker_char: if active || flagged {
            SELECTION_MARKER
        } else {
            " "
        },
        marker_style: if flagged {
            apply_bg(Style::default().fg(theme.flag))
        } else if active {
            apply_bg(Style::default().fg(theme.accent))
        } else {
            apply_bg(Style::default())
        },
        inner_width: width.saturating_sub(2),
        theme,
        bg,
        active,
    };

    // Diamond family: hollow = idle, filled = busy, filled+pulse = task.
    // Distinct from the agent circle while still driven by `icon_color`.
    let (icon, icon_color): (&str, ratatui::style::Color) = match window.status {
        WindowStatus::Idle => ("\u{25C7}", theme.status_idle),
        WindowStatus::Busy => ("\u{25C6}", theme.status_running),
        WindowStatus::Task => {
            let (_, pulse) = running_icon_for(&PaneStatus::Running, spinner_frame, icons);
            ("\u{25C6}", pulse.unwrap_or(theme.status_running))
        }
    };

    // A finished tracked task outranks the plain status glyph.
    let mut exit_note: Option<String> = None;
    let (icon, icon_color) = match finished {
        Some(finished) => match finished.exit_code {
            None | Some(0) => {
                let flash = theme
                    .attention_color(crate::tmux::PaneAttention::Done, spinner_frame)
                    .unwrap_or(theme.status_idle);
                ("\u{25C7}", flash)
            }
            Some(code) => {
                exit_note = Some(format!("exit {code}"));
                ("\u{25C6}", theme.status_error)
            }
        },
        None => (icon, icon_color),
    };

    let icon_w = display_width(icon);

    // Non-agent rows represent individual panes (panels), so each panel shows
    // the name of its own foreground program and arguments rather than the tmux window name.
    let (raw_prog, raw_args) = if let Some(finished) = finished {
        if let Some((prog, rest)) = finished.command.split_once(' ') {
            (prog, Some(rest))
        } else {
            (finished.command.as_str(), None)
        }
    } else if !window.command.is_empty() {
        (window.command.as_str(), window.command_args.as_deref())
    } else if !window.window_name.is_empty() && window.window_name != "tmux-agent-sidebar" {
        (window.window_name.as_str(), None)
    } else {
        ("shell", None)
    };
    let program_name = strip_exe_suffix(command_basename(raw_prog));
    let prog_w = display_width(program_name);

    // The program name is the anchor of the row and always renders in
    // full — the listening-port list yields instead. A process that
    // opens a huge port range (e.g. an expo dev server) must not crowd
    // the name off the row: the ports truncate to the lowest ones that
    // fit, a `…` marks the rest, and a minimum gap keeps the two apart.
    let note_reserve = exit_note
        .as_ref()
        .map(|n| display_width(n) + 3)
        .unwrap_or(0);
    let port_budget = ctx
        .inner_width
        .saturating_sub(icon_w + 1)
        .saturating_sub(prog_w + MIN_PORT_GAP)
        .saturating_sub(note_reserve);
    let port_text = port_text_lowest_first(ports, port_budget);
    let mut right_spans: Vec<Span<'static>> = Vec::with_capacity(1);
    let mut right_width = 0usize;
    if let Some(text) = port_text {
        right_width += display_width(&text);
        right_spans.push(Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(theme.port)),
        ));
    }

    // Width left for the program name and its arguments, after the
    // icon, the (possibly truncated) port list, and the port gap. Only
    // a name too long for the entire row still truncates here.
    let port_gap = if right_width > 0 { MIN_PORT_GAP } else { 0 };
    let text_budget = ctx
        .inner_width
        .saturating_sub(icon_w + 1 + right_width + port_gap)
        .saturating_sub(note_reserve);

    let mut left_spans: Vec<Span<'static>> = Vec::with_capacity(4);
    left_spans.push(Span::styled(
        icon,
        ctx.apply_bg(Style::default().fg(icon_color)),
    ));
    let mut left_width = icon_w + 1;

    if prog_w >= text_budget {
        let name = truncate_to_width(program_name, text_budget);
        let name_w = display_width(&name);
        left_spans.push(Span::styled(
            format!(" {}", name),
            ctx.apply_bg(Style::default().fg(theme.text_active)),
        ));
        left_width += name_w;
    } else {
        left_spans.push(Span::styled(
            format!(" {}", program_name),
            ctx.apply_bg(Style::default().fg(theme.text_active)),
        ));
        left_width += prog_w;

        let remaining = text_budget.saturating_sub(prog_w);
        if let Some(args) = raw_args
            && !args.is_empty()
            && remaining >= 2
        {
            let args_budget = remaining.saturating_sub(1);
            let name = truncate_to_width(args, args_budget);
            let name_w = display_width(&name);
            left_spans.push(Span::styled(
                format!(" {}", name),
                ctx.apply_bg(Style::default().fg(theme.text_muted)),
            ));
            left_width += 1 + name_w;
        }
    }

    // Failed-run marker: a note naming the failing exit code keeps
    // the red diamond unambiguous on theme colors that also tint tasks.
    let remaining_for_note = ctx.inner_width.saturating_sub(left_width + right_width);
    if let Some(note) = exit_note
        && remaining_for_note >= 3
    {
        let text = format!(
            " ({})",
            truncate_to_width(&note, remaining_for_note.saturating_sub(3))
        );
        let w = display_width(&text);
        left_spans.push(Span::styled(
            text,
            ctx.apply_bg(Style::default().fg(theme.status_error)),
        ));
        left_width += w;
    }

    // Expanded mode appends the context detail line; compact stays on
    // the single status line. Line 2 renders through a plain context —
    // it is a content row, so no selection background and no active
    // marker ride on it (same treatment as agent detail rows).
    let mut lines = Vec::with_capacity(2);
    lines.push(ctx.row_line_split(left_spans, left_width, right_spans, right_width));
    if expanded {
        let detail_ctx = RowCtx {
            marker_char: " ",
            marker_style: Style::default(),
            inner_width: ctx.inner_width,
            theme,
            bg: None,
            active,
        };
        if let Some(line) = window_context_row(window, group_by, &detail_ctx) {
            lines.push(line);
        }
    }
    lines
}

/// Two-space indent shared with the agent detail rows (`└`/`├`, prompt
/// wrapping) — the expanded window card's second line hangs from it.
const CONTEXT_INDENT: &str = "  ";

/// Minimum meaningful display columns line 2 must have room for after
/// the indent. Below that the truncated fragment would be a couple of
/// characters plus an ellipsis — noise rather than context.
const MIN_CONTEXT_COLUMNS: usize = 3;

/// Last path segment of a repo root — the session view's "other"
/// dimension. `None` for missing repos, path roots (`/`), and empty
/// paths alike: none of them leave a name worth showing.
fn repo_basename(repo_root: Option<&str>) -> Option<String> {
    let root = repo_root?;
    std::path::Path::new(root)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

/// Compose the expanded card's line-2 text: the grouping dimension that
/// is *not* the group title (whichever of session/repo that is, per
/// `group_by`), then the tmux window name. `None` when every segment is
/// empty — there is nothing the line could say.
///
/// Exception — ssh cards: line 1 is `ssh <host>` and the grouping
/// dimensions describe the LOCAL cwd's repo tag, which is useless. tmux's
/// `#{pane_title}` passes through ssh carrying whatever the REMOTE side
/// published, so when it holds real remote content (non-empty, and not
/// the stale local-hostname default the shell set before exec) it
/// becomes line 2 verbatim. The existing truncation/degradation logic in
/// [`window_context_row`] applies unchanged.
fn window_context_text(
    window: &crate::group::OtherWindow,
    group_by: crate::state::GroupBy,
) -> Option<String> {
    let command_base = strip_exe_suffix(command_basename(window.command.as_str()));
    let title = window.pane_title.trim();
    if command_base == "ssh"
        && !title.is_empty()
        && title != crate::process::local_hostname().trim()
    {
        return Some(window.pane_title.clone());
    }
    let window_name = window.window_name.as_str();
    let mut parts: Vec<String> = Vec::with_capacity(2);
    match group_by {
        // Repo-grouped view: the group title is the repo, so line 2
        // names the session and the tmux `index:name` window selector.
        crate::state::GroupBy::Repo => {
            let session = window.session_name.as_str();
            if !session.is_empty() {
                parts.push(session.to_string());
            }
            if !window_name.is_empty() {
                parts.push(format!("{}:{}", window.window_index, window_name));
            }
        }
        // Session-grouped view: the group title is the session, so line
        // 2 names the repo (its basename) and the window name. A pane
        // outside a repo still shows the window name alone.
        crate::state::GroupBy::Session => {
            if let Some(repo) = repo_basename(window.git_info.repo_root.as_deref()) {
                parts.push(repo);
            }
            if !window_name.is_empty() {
                parts.push(window_name.to_string());
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
}

/// The expanded window card's second line: `<other dimension> ·
/// <window_name>` in muted text, indented like the agent detail rows.
/// `None` collapses the card back to its single status line — either
/// there is nothing to show, or the sidebar is too narrow to render at
/// least [`MIN_CONTEXT_COLUMNS`] meaningful columns of it. Truncation
/// cuts the tail, so the window name (the last segment) yields first
/// and the `other` segment stays as long as possible.
fn window_context_row(
    window: &crate::group::OtherWindow,
    group_by: crate::state::GroupBy,
    ctx: &RowCtx,
) -> Option<Line<'static>> {
    let budget = ctx
        .inner_width
        .saturating_sub(display_width(CONTEXT_INDENT));
    if budget < MIN_CONTEXT_COLUMNS {
        return None;
    }
    let text = window_context_text(window, group_by)?;
    if text.trim().is_empty() {
        return None;
    }
    let text = truncate_to_width(&text, budget);
    let content = format!("{CONTEXT_INDENT}{text}");
    let content_w = display_width(&content);
    Some(ctx.row_line(
        vec![Span::styled(
            content,
            // Same gray as the agent prompt/detail rows: the context line
            // must recede behind line 1, not compete with it.
            ctx.apply_bg(Style::default().fg(ctx.theme.text_inactive)),
        )],
        content_w,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::PaneGitInfo;
    use crate::state::GroupBy;
    use crate::tmux::{AgentType, PaneAttention, PaneInfo, PermissionMode, WorktreeMetadata};
    use crate::ui::icons::StatusIcons;
    use crate::ui::text::display_width;
    use ratatui::style::{Color, Modifier};

    fn pane(permission_mode: PermissionMode, status: PaneStatus, prompt: &str) -> PaneInfo {
        pane_with_response(permission_mode, status, prompt, false)
    }

    fn pane_with_response(
        permission_mode: PermissionMode,
        status: PaneStatus,
        prompt: &str,
        is_response: bool,
    ) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".into(),
            pane_active: false,
            status,
            attention: PaneAttention::None,
            agent: AgentType::Codex,
            path: "/tmp/project".into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: prompt.into(),
            prompt_is_response: is_response,
            started_at: None,
            wait_reason: String::new(),
            permission_mode,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
            session_id: None,
            session_name: String::new(),
            tmux_session: String::new(),
            window_index: 0,
            sidebar_spawned: false,
            bg_shell_cmd: None,
            bg_shell_pid: None,
            pane_title: String::new(),
            flagged: false,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn test_ctx<'a>(theme: &'a ColorTheme, inner_width: usize, active: bool) -> RowCtx<'a> {
        RowCtx {
            marker_char: " ",
            marker_style: Style::default(),
            inner_width,
            theme,
            bg: None,
            active,
        }
    }

    #[test]
    fn render_pane_lines_hides_auto_accept_badges() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Auto, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let status = line_text(&lines[0]);
        // Auto-accept is the everyday steady state — unbadged by design.
        assert!(!status.contains(" codex auto"));
    }

    #[test]
    fn render_pane_lines_hides_defer_badge() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Defer, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let status = line_text(&lines[0]);
        assert!(
            !status.contains(" codex defer"),
            "defer is auto-accept family — unbadged, got: {status}"
        );
    }

    #[test]
    fn render_pane_lines_shows_session_name_instead_of_agent() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "fix-csv-aliases".into();
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let status = line_text(&lines[0]);
        assert!(
            status.contains("fix-csv-aliases"),
            "session name should appear in status row, got: {status}"
        );
        assert!(
            !status.contains("codex"),
            "agent label should be replaced by session name, got: {status}"
        );
    }

    #[test]
    fn render_pane_lines_truncates_long_session_name_to_keep_elapsed_visible() {
        // Regression: a user-supplied `/rename` title can be arbitrarily
        // long and would push the elapsed counter off-screen if we did
        // not truncate it first. The width budget reserves room for the
        // status icon, the badge, and the elapsed label.
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "this-is-a-ridiculously-long-session-name-that-will-not-fit".into();
        // started_at must be > 0 for elapsed_label to render.
        // started_at=1, now=66 → elapsed=65s → "1m5s".
        p.started_at = Some(1);
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            30,
            &StatusIcons::default(),
            &theme,
            0,
            66,
        );

        let status = line_text(&lines[0]);
        // The elapsed counter must remain visible — that is the whole
        // point of capping the title width.
        assert!(
            status.contains("1m5s"),
            "elapsed must stay visible when session name is long, got: {status}"
        );
        // The full title must NOT fit; it should be replaced by a
        // truncated form ending in the standard ellipsis character.
        assert!(
            !status.contains("not-fit"),
            "long session name must be truncated, got: {status}"
        );
        // Each rendered cell should fit inside the 30-column width.
        let visible_width = display_width(&status);
        assert!(
            visible_width <= 30,
            "status row width {visible_width} must not exceed inner_width 30: {status}"
        );
    }

    #[test]
    fn render_pane_lines_shows_branch_and_ports_on_same_row() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let ports = vec![3000, 5173];
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo {
                repo_root: Some("/tmp/project".into()),
                branch: Some("feature/sidebar".into()),
                is_worktree: false,
                ..Default::default()
            },
            Some(&ports),
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let branch_port_line = line_text(&lines[1]);
        assert!(branch_port_line.contains("feature/sidebar"));
        assert!(branch_port_line.contains(":3000, 5173"));
        assert!(branch_port_line.find("feature/sidebar") < branch_port_line.find(":3000, 5173"));
    }

    #[test]
    fn render_pane_lines_truncates_long_branch_when_ports_present() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let ports = vec![3000];
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo {
                repo_root: Some("/tmp/project".into()),
                branch: Some("feature/sidebar/really-long-branch-name-that-should-truncate".into()),
                is_worktree: false,
                ..Default::default()
            },
            Some(&ports),
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let branch_port_line = line_text(&lines[1]);
        assert!(
            branch_port_line.contains('…'),
            "long branch should be truncated"
        );
        assert!(branch_port_line.contains(":3000"));
        assert!(
            branch_port_line.find('…') < branch_port_line.find(":3000"),
            "branch truncation should remain left of the port text"
        );
    }

    #[test]
    fn render_pane_lines_uses_injected_now_for_elapsed() {
        let theme = ColorTheme::default();
        let mut pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        pane.started_at = Some(1_000_000 - 125);
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            1_000_000,
        );

        let status = line_text(&lines[0]);
        assert!(status.contains("2m5s"));
    }

    #[test]
    fn running_icon_for_all_statuses() {
        let icons = StatusIcons::default();
        assert_eq!(running_icon_for(&PaneStatus::Idle, 0, &icons), ("○", None));
        assert_eq!(
            running_icon_for(&PaneStatus::Waiting, 0, &icons),
            ("◐", None)
        );
        assert_eq!(running_icon_for(&PaneStatus::Error, 0, &icons), ("✕", None));
        assert_eq!(
            running_icon_for(&PaneStatus::Unknown, 0, &icons),
            ("·", None)
        );
        assert_eq!(
            running_icon_for(&PaneStatus::Background, 0, &icons),
            ("◎", None)
        );

        let (icon, color) = running_icon_for(&PaneStatus::Running, 0, &icons);
        assert_eq!(icon, "●");
        assert_eq!(color, Some(Color::Indexed(82)));
    }

    #[test]
    fn render_pane_lines_shows_idle_prompt_hint() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Idle, "");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert_eq!(lines.len(), 2);
        let hint = line_text(&lines[1]);
        assert!(hint.contains("Waiting for prompt"));
    }

    #[test]
    fn render_pane_lines_shows_bg_command_even_while_running() {
        // A live background shell must stay visible in the pane
        // regardless of the agent's current status — running bursts in
        // the middle of a background task should not make the shell
        // look like it vanished.
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.bg_shell_cmd = Some("npm run dev".into());
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let hint = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            hint.contains("npm run dev"),
            "bg command must render during running state, got:\n{hint}"
        );
    }

    #[test]
    fn render_pane_lines_shows_bg_command_even_while_idle() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Idle, "");
        p.bg_shell_cmd = Some("cargo watch".into());
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let joined = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            joined.contains("cargo watch"),
            "bg command must render during idle state, got:\n{joined}"
        );
    }

    #[test]
    fn render_pane_lines_shows_background_command_when_known() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Background, "");
        p.bg_shell_cmd = Some("cargo build --release".into());
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert_eq!(lines.len(), 2);
        let hint = line_text(&lines[1]);
        assert!(hint.contains("cargo build --release"), "got: {hint}");
        assert!(
            !hint.contains("Background shell running"),
            "fallback text must not appear when a command is known, got: {hint}"
        );
    }

    #[test]
    fn render_pane_lines_truncates_long_background_command() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Background, "");
        p.bg_shell_cmd = Some("cargo run --bin very-long-command-name --flag".into());
        // Narrow width forces the ellipsis path in `truncate_to_width`.
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            20,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        let hint = line_text(&lines[1]);
        assert!(hint.contains("cargo run"), "command missing in {hint}");
        assert!(hint.contains('\u{2026}'), "ellipsis missing in {hint}");
        assert!(
            display_width(&hint) <= 20,
            "row width {w} must not exceed inner_width 20: {hint}",
            w = display_width(&hint)
        );
    }

    #[test]
    fn render_pane_lines_wraps_prompt_when_present() {
        let theme = ColorTheme::default();
        let pane = pane(
            PermissionMode::BypassPermissions,
            PaneStatus::Idle,
            "hello world from codex",
        );
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            18,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let status = line_text(&lines[0]);
        assert!(status.contains(" codex !"));
        assert!(!line_text(&lines[1]).contains("Waiting for prompt"));
    }

    #[test]
    fn render_pane_lines_shows_single_subagent() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "test");
        p.subagents = vec!["Explore".into()];
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 3);
        let sub_line = line_text(&lines[1]);
        assert!(sub_line.contains("└ "));
        assert!(sub_line.contains("Explore #1"));
    }

    #[test]
    fn render_pane_lines_shows_multiple_subagents_tree() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "test");
        p.subagents = vec!["Explore #1".into(), "Plan".into(), "Explore #2".into()];
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 5);
        assert!(line_text(&lines[1]).contains("├ "));
        assert!(line_text(&lines[1]).contains("Explore #1"));
        assert!(line_text(&lines[2]).contains("├ "));
        assert!(line_text(&lines[2]).contains("Plan #2"));
        assert!(line_text(&lines[3]).contains("└ "));
        assert!(line_text(&lines[3]).contains("Explore #2"));
    }

    #[test]
    fn render_pane_lines_subagents_before_wait_reason() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Waiting, "");
        p.subagents = vec!["Explore".into()];
        p.wait_reason = "permission_prompt".into();
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 3);
        let sub_line = line_text(&lines[1]);
        assert!(sub_line.contains("Explore #1"));
        let reason_line = line_text(&lines[2]);
        assert!(reason_line.contains("permission required"));
    }

    #[test]
    fn render_pane_lines_response_shows_arrow() {
        let theme = ColorTheme::default();
        let p = pane_with_response(
            PermissionMode::Default,
            PaneStatus::Idle,
            "Task completed successfully",
            true,
        );
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let response_line = line_text(&lines[1]);
        assert!(response_line.contains("▷"));
        assert!(response_line.contains("Task completed successfully"));
    }

    #[test]
    fn render_pane_lines_response_uses_char_wrap() {
        let theme = ColorTheme::default();
        let p = pane_with_response(
            PermissionMode::Default,
            PaneStatus::Idle,
            "abcdef ghijk lmnop qrstu vwxyz",
            true,
        );
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            20,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let first = line_text(&lines[1]);
        assert!(first.contains("▷"));
        // char-wrap must not trim inter-word spaces like word-wrap does
        let second = line_text(&lines[2]);
        assert!(!second.starts_with("│  ghijk"));
    }

    #[test]
    fn render_pane_lines_normal_prompt_not_detected_as_response() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "fix the bug");
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let prompt_line = line_text(&lines[1]);
        assert!(!prompt_line.contains("▷"));
        assert!(prompt_line.contains("fix the bug"));
    }

    #[test]
    fn render_pane_lines_shows_task_progress() {
        use crate::activity::{TaskProgress, TaskStatus};
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "");
        let progress = TaskProgress {
            tasks: vec![
                ("Task A".into(), TaskStatus::Completed),
                ("Task B".into(), TaskStatus::InProgress),
                ("Task C".into(), TaskStatus::Pending),
            ],
        };
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            Some(&progress),
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(lines.len() >= 2);
        let task_line = line_text(&lines[1]);
        assert!(task_line.contains("✔◼◻"));
        assert!(task_line.contains("1/3"));
    }

    #[test]
    fn render_pane_lines_no_task_line_when_empty() {
        use crate::activity::TaskProgress;
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Idle, "");
        let progress = TaskProgress { tasks: vec![] };
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            Some(&progress),
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert_eq!(lines.len(), 2);
        let hint = line_text(&lines[1]);
        assert!(hint.contains("Waiting for prompt"));
    }

    #[test]
    fn branch_ports_row_renders_port_only_without_branch() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let ports = vec![3000];
        let line = branch_ports_row(&PaneGitInfo::default(), Some(&ports), false, &ctx)
            .expect("should render port line");
        assert!(line_text(&line).contains(":3000"));
    }

    #[test]
    fn branch_ports_row_renders_plus_marker_for_non_spawned_worktree() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            ..Default::default()
        };
        let line = branch_ports_row(&git, None, false, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(text.contains("+ feat/x"), "plain + marker: {text}");
        assert!(!text.contains('×'), "non-spawned must not render ×");
    }

    #[test]
    fn branch_ports_row_pins_trailing_x_to_right_edge_for_sidebar_spawned_worktree() {
        let theme = ColorTheme::default();
        let inner_width = 40usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            ..Default::default()
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(
            text.contains("+ feat/x"),
            "+ worktree marker must be preserved: {text}"
        );
        // The trailing `×` must sit at the very last row column
        // (= inner_width + 1), mirroring the repo header's
        // right-aligned `+` spawn button.
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× should pin to the rightmost column"
        );
        // The `×` suffix must come AFTER the branch text, not before.
        let plus_idx = text.find("+ feat/x").unwrap();
        let x_idx = text.find('×').unwrap();
        assert!(
            plus_idx < x_idx,
            "`+ feat/x` must precede the trailing `×`, got: {text}"
        );
        // Branch text stays in the normal branch color.
        let body_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("feat/x"))
            .expect("branch body span");
        assert_eq!(body_span.style.fg, Some(theme.branch));
        // The trailing `×` span is painted with status_error so the
        // glyph reads as a remove action.
        let marker_span = line
            .spans
            .iter()
            .find(|s| s.content == "×")
            .expect("× span");
        assert_eq!(marker_span.style.fg, Some(theme.status_error));
    }

    /// Display column (in terminal cells, not bytes) where the
    /// first `×` appears in the rendered row. `text.find` returns a
    /// byte index which skews for multibyte glyphs like `…`, so
    /// tests that truncate branches need to measure in display cells.
    fn rendered_x_col(text: &str) -> usize {
        let idx = text.find('×').expect("× should be present");
        display_width(&text[..idx])
    }

    #[test]
    fn branch_ports_row_truncates_long_branch_but_keeps_x_at_right_edge() {
        let theme = ColorTheme::default();
        // Narrow ctx forces the branch text to truncate, but the
        // `×` must still render at the right edge — the action
        // affordance cannot be the thing that gets clipped.
        let inner_width = 18usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feature/really-long-branch-name".into()),
            is_worktree: true,
            ..Default::default()
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(
            text.contains('×'),
            "× must remain visible even when branch truncates: {text}"
        );
        assert!(
            text.contains('…'),
            "branch text should show truncation ellipsis: {text}"
        );
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× stays pinned to right edge under truncation"
        );
        // Total row width = marker(1) + space(1) + inner_width.
        let rendered_width = display_width(text.trim_end());
        assert!(
            rendered_width <= inner_width + 2,
            "row must fit within marker + inner width (={}), got {rendered_width}: {text}",
            inner_width + 2
        );
    }

    #[test]
    fn sidebar_remove_marker_col_matches_branch_ports_row_layout() {
        // The click-target math in panes.rs uses
        // `sidebar_remove_marker_col` to line up the hit region with
        // the rendered `×`. Verify the two agree by counting the `×`
        // position in the rendered text and comparing to the helper.
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/abc".into()),
            is_worktree: true,
            ..Default::default()
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        let computed = sidebar_remove_marker_col(&git, None, true, ctx.inner_width)
            .expect("col should be Some");
        assert_eq!(
            computed as usize,
            rendered_x_col(&text),
            "computed × col must match rendered position"
        );
    }

    #[test]
    fn sidebar_remove_marker_col_does_not_depend_on_branch_length() {
        // Right-edge pinning means the col is determined by the row
        // width alone. A short and a long branch must produce the
        // same col.
        let short = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("x".into()),
            is_worktree: true,
            ..Default::default()
        };
        let long = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feature/really-long-branch-name".into()),
            is_worktree: true,
            ..Default::default()
        };
        let col_short = sidebar_remove_marker_col(&short, None, true, 40);
        let col_long = sidebar_remove_marker_col(&long, None, true, 40);
        assert_eq!(col_short, col_long);
        assert_eq!(col_short, Some(41));
    }

    #[test]
    fn sidebar_remove_marker_col_returns_none_for_non_spawned() {
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            ..Default::default()
        };
        assert_eq!(sidebar_remove_marker_col(&git, None, false, 40), None);
    }

    #[test]
    fn sidebar_remove_marker_col_returns_none_for_non_worktree_branch() {
        // sidebar_spawned=true but the branch label has no `+` prefix
        // (is_worktree=false). No × should be rendered or registered.
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("main".into()),
            is_worktree: false,
            ..Default::default()
        };
        assert_eq!(sidebar_remove_marker_col(&git, None, true, 40), None);
    }

    #[test]
    fn branch_ports_row_keeps_x_at_right_edge_when_ports_are_present() {
        // Ports (`  :3000`) eat space on the right side of the row,
        // but the `×` must still pin to the very last column with
        // ports stacked just to its left.
        let theme = ColorTheme::default();
        let inner_width = 40usize;
        let ctx = test_ctx(&theme, inner_width, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/abc".into()),
            is_worktree: true,
            ..Default::default()
        };
        let ports = [3000u16];
        let line =
            branch_ports_row(&git, Some(&ports), true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert_eq!(
            rendered_x_col(&text),
            inner_width + 1,
            "× must pin to right edge regardless of port presence"
        );
        let port_idx = text.find(":3000").expect(":3000 should be present");
        let x_idx = text.find('×').unwrap();
        assert!(
            port_idx < x_idx,
            "ports should sit to the LEFT of the × marker: {text}"
        );
    }

    #[test]
    fn branch_ports_row_keeps_plain_branch_when_sidebar_spawned_but_not_worktree() {
        // Edge case: sidebar_spawned=true but is_worktree=false.
        // `branch_label` does not emit the "+ " prefix, so
        // branch_ports_row must not try to swap anything and the
        // resulting row must stay plain.
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("main".into()),
            is_worktree: false,
            ..Default::default()
        };
        let line = branch_ports_row(&git, None, true, &ctx).expect("branch row should render");
        let text = line_text(&line);
        assert!(text.contains("main"));
        assert!(!text.contains('×'));
        assert!(!text.contains('+'));
    }

    #[test]
    fn wait_reason_row_uses_error_color_when_status_is_error() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let line = wait_reason_row("permission_prompt", &PaneStatus::Error, &ctx)
            .expect("should render reason line");
        let text_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("permission"))
            .expect("reason text should be present");
        assert_eq!(text_span.style.fg, Some(theme.status_error));
    }

    #[test]
    fn render_pane_lines_selected_applies_background_to_spans() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Auto, PaneStatus::Running, "do work");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            true, // selected
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        // Every inner (non-marker) span on the status line must carry the selection bg.
        // The left marker uses marker_style only.
        let status = &lines[0];
        let has_bg = status
            .spans
            .iter()
            .any(|s| s.style.bg == Some(theme.selection_bg));
        assert!(
            has_bg,
            "selected row should apply selection_bg to inner spans"
        );
    }

    #[test]
    fn render_pane_lines_selected_leaves_content_rows_unhighlighted() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Auto, PaneStatus::Running, "do work");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            true, // selected
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        assert!(
            lines
                .iter()
                .skip(1)
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg != Some(theme.selection_bg)),
            "content rows should not carry the selection background"
        );
    }

    #[test]
    fn render_pane_lines_active_shows_left_marker_on_status_row() {
        let theme = ColorTheme::default();
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &pane,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            true,
            false, // active
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        // The status row (line 0) must start with the SELECTION_MARKER in the
        // accent fg; no BOLD is applied to the title span.
        let marker_span = &lines[0].spans[0];
        assert_eq!(marker_span.content, SELECTION_MARKER);
        assert_eq!(marker_span.style.fg, Some(theme.accent));

        let title_span = lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("codex"))
            .expect("title span should be present");
        assert!(
            !title_span.style.add_modifier.contains(Modifier::BOLD),
            "active pane title should not be BOLD"
        );
    }

    #[test]
    fn status_row_default_permission_mode_omits_badge() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 40, false);
        let pane = pane(PermissionMode::Default, PaneStatus::Running, "");
        let line = status_row(&pane, &ctx, &StatusIcons::default(), 0, 0);
        let text = line_text(&line);
        // Default mode has an empty badge string — no extra badge token should appear.
        assert!(
            !text.contains(" auto") && !text.contains(" plan") && !text.contains(" !"),
            "default permission mode should not render a badge, got: {text}"
        );
    }

    #[test]
    fn prompt_rows_indents_continuation_lines() {
        let theme = ColorTheme::default();
        let ctx = test_ctx(&theme, 20, false);
        let mut p = pane(
            PermissionMode::Default,
            PaneStatus::Running,
            "aaaa bbbb cccc dddd eeee",
        );
        p.prompt_is_response = false;
        let lines = prompt_rows(&p, &ctx);
        assert!(
            lines.len() >= 2,
            "expected prompt to wrap across multiple lines"
        );
        for line in &lines {
            let text = line_text(line);
            // Each line starts with marker(1) + space(1) + indent(2) = "    " for non-selected.
            assert!(
                text.starts_with("    "),
                "each wrapped line should carry the left padding, got: {text}"
            );
        }
    }

    fn compact_row_text(pane: &PaneInfo, git: &PaneGitInfo, width: usize) -> String {
        let theme = ColorTheme::default();
        let lines = compact_row(
            pane,
            git,
            false,
            false,
            false,
            width,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn compact_row_renders_exactly_one_line() {
        // Full mode renders status + branch + subagents + wait reason +
        // prompt rows; compact must collapse all of that into one line.
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Auto, PaneStatus::Waiting, "some prompt");
        p.session_name = "fix-api".into();
        p.wait_reason = "permission_prompt".into();
        p.subagents = vec!["Explore".into(), "Plan".into()];
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/compact".into()),
            is_worktree: false,
            ..Default::default()
        };
        let lines = compact_row(
            &p,
            &git,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        assert_eq!(lines.len(), 1, "compact mode is strictly one line per pane");
        let text = line_text(&lines[0]);
        assert!(text.contains("fix-api"), "title must render: {text}");
        assert!(
            text.contains("+2"),
            "subagent count badge must render: {text}"
        );
        assert!(
            !text.contains(" auto"),
            "auto-accept mode is unbadged steady state: {text}"
        );
        assert!(
            !text.contains("permission required"),
            "wait-reason text must collapse into the glyph: {text}"
        );
    }

    #[test]
    fn compact_row_prefers_session_name_over_branch_when_narrow() {
        // Priority rule: title > elapsed > badges > branch. At 16 columns
        // the branch no longer fits; the session name must stay intact.
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "fix-api".into();
        p.started_at = Some(1);
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feature/very-long-branch-name".into()),
            is_worktree: false,
            ..Default::default()
        };
        let lines = compact_row(
            &p,
            &git,
            false,
            false,
            false,
            16,
            &StatusIcons::default(),
            &theme,
            0,
            66,
        );
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("fix-api"), "name must survive: {text}");
        assert!(
            !text.contains("feature"),
            "branch must be dropped first: {text}"
        );
        assert!(text.contains("1m5s"), "elapsed must survive: {text}");
    }

    #[test]
    fn compact_row_shows_branch_fragment_when_room_remains() {
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "fix-api".into();
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/compact".into()),
            is_worktree: false,
            ..Default::default()
        };
        let text = compact_row_text(&p, &git, 40);
        assert!(
            text.contains(" feat/compact"),
            "branch fragment should fill leftover space: {text}"
        );
    }

    #[test]
    fn compact_row_pins_remove_marker_to_right_edge_for_spawned_worktree() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.session_name = "fix-api".into();
        p.sidebar_spawned = true;
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: true,
            ..Default::default()
        };
        let width = 40usize;
        let lines = compact_row(
            &p,
            &git,
            false,
            false,
            false,
            width,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let text = line_text(&lines[0]);
        // Same column math as the full branch row: marker(1) + space(1) +
        // inner_width, with `×` on the last column.
        let x_col = display_width(&text[..text.find('×').expect("× present")]);
        assert_eq!(x_col, width - 1, "× must pin to the right edge: {text}");
        assert!(
            text.find("fix-api").unwrap() < text.find('×').unwrap(),
            "× comes after the title: {text}"
        );
    }

    #[test]
    fn compact_row_no_remove_marker_for_plain_branch() {
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.sidebar_spawned = true;
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("main".into()),
            is_worktree: false,
            ..Default::default()
        };
        let text = compact_row_text(&p, &git, 40);
        assert!(!text.contains('×'), "non-worktree panes get no ×: {text}");
    }

    #[test]
    fn compact_row_selected_applies_selection_bg() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "");
        let lines = compact_row(
            &p,
            &PaneGitInfo::default(),
            true,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.selection_bg)),
            "the single compact row is also the status row, so selection bg applies"
        );
    }

    // ─── window_row ─────────────────────────────────────────────────

    fn window(
        name: &str,
        command: &str,
        status: crate::tmux::WindowStatus,
    ) -> crate::group::OtherWindow {
        window_with_args(name, command, None, status)
    }

    fn window_with_args(
        name: &str,
        command: &str,
        command_args: Option<&str>,
        status: crate::tmux::WindowStatus,
    ) -> crate::group::OtherWindow {
        crate::group::OtherWindow {
            window_id: "@5".into(),
            window_index: 0,
            window_name: name.into(),
            window_active: false,
            session_name: "main".into(),
            session_attached: true,
            attention: crate::tmux::PaneAttention::None,
            pane_id: "%10".into(),
            pane_active: true,
            pane_pid: None,
            command: command.into(),
            command_args: command_args.map(Into::into),
            path: "/repo".into(),
            git_info: PaneGitInfo::default(),
            status,
            pane_title: String::new(),
            flagged: false,
        }
    }

    #[test]
    fn window_row_renders_program_and_port() {
        let theme = ColorTheme::default();
        let w = window("web", "node", crate::tmux::WindowStatus::Task);
        let lines = window_row(
            &w,
            Some(&[3000]),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains('\u{25C6}'), "diamond glyph missing: {text}");
        assert!(text.contains("node"), "program missing: {text}");
        // Displays the pane's own foreground program, not the window name.
        assert!(!text.contains("web"), "window name must not appear: {text}");
        assert!(text.contains(":3000"), "port missing: {text}");
    }

    #[test]
    fn window_row_omits_port_when_empty() {
        let theme = ColorTheme::default();
        let w = window("nvim", "nvim", crate::tmux::WindowStatus::Busy);
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains("nvim"), "program missing: {text}");
        assert_eq!(
            text.matches("nvim").count(),
            1,
            "program must render exactly once: {text}"
        );
        assert!(!text.contains(':'), "no port expected: {text}");
    }

    #[test]
    fn window_row_finished_shows_task_command_not_reverted_shell() {
        let theme = ColorTheme::default();
        // Pane dropped back to zsh, but the finished task was cargo
        let w = window("zsh", "zsh", crate::tmux::WindowStatus::Idle);
        let finished = crate::state::WindowFinished {
            command: "cargo".into(),
            exit_code: Some(0),
            finished_at: 0,
        };
        let lines = window_row(
            &w,
            None,
            Some(&finished),
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            text.contains("cargo"),
            "finished task command missing: {text}"
        );
        assert!(
            !text.contains("zsh"),
            "reverted shell must not appear: {text}"
        );
    }

    #[test]
    fn window_row_suppresses_tmux_agent_sidebar_window_name() {
        let theme = ColorTheme::default();
        let w = window("tmux-agent-sidebar", "", crate::tmux::WindowStatus::Idle);
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            !text.contains("tmux-agent-sidebar"),
            "sidebar binary name must not appear: {text}"
        );
        assert!(text.contains("shell"), "fallback name missing: {text}");
    }

    #[test]
    fn window_row_never_renders_an_elapsed_counter() {
        let theme = ColorTheme::default();
        // Pick name/command with no "m"/"s" so a stray duration like
        // "2m14s" would be the only source of those characters.
        let w = window("web", "node", crate::tmux::WindowStatus::Task);
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            !text.contains('m') && !text.contains('s'),
            "window rows must not show a time counter: {text}"
        );

        // The invariant extends to the expanded card's second line: it
        // shows session/repo context, never a duration.
        let mut w = w;
        w.session_name = "dev".into();
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2, "expanded window rows render two lines");
        let text = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(
            !text.contains('m') && !text.contains('s'),
            "window rows must not show a time counter: {text}"
        );
    }

    #[test]
    fn window_row_finished_success_flashes_hollow_green_diamond() {
        let theme = ColorTheme::default();
        // A tracked task that finished with exit 0 (or an unknown exit)
        // renders a hollow diamond in the done-attention pulse color —
        // the same "look here" treatment agent rows get.
        let w = window("tests", "cargo", crate::tmux::WindowStatus::Idle);
        let finished = crate::state::WindowFinished {
            command: "cargo".into(),
            exit_code: None,
            finished_at: 0,
        };
        let lines = window_row(
            &w,
            None,
            Some(&finished),
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains('\u{25C7}'), "hollow diamond missing: {text}");
        let icon_span = &line.spans[2];
        assert_eq!(icon_span.content.as_ref(), "\u{25C7}");
        assert_eq!(
            icon_span.style.fg,
            Some(
                theme
                    .attention_color(crate::tmux::PaneAttention::Done, 0)
                    .unwrap()
            )
        );
    }

    #[test]
    fn window_row_finished_nonzero_gets_red_diamond_and_exit_note() {
        let theme = ColorTheme::default();
        let w = window("tests", "cargo", crate::tmux::WindowStatus::Idle);
        let finished = crate::state::WindowFinished {
            command: "cargo".into(),
            exit_code: Some(1),
            finished_at: 0,
        };
        let lines = window_row(
            &w,
            None,
            Some(&finished),
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains("(exit 1)"), "exit note missing: {text}");
        let icon_span = &line.spans[2];
        assert_eq!(icon_span.content.as_ref(), "\u{25C6}");
        assert_eq!(icon_span.style.fg, Some(theme.status_error));
    }

    #[test]
    fn window_row_renders_program_and_args_two_tone() {
        let theme = ColorTheme::default();
        let w = window_with_args(
            "dev",
            "node",
            Some("server.js"),
            crate::tmux::WindowStatus::Task,
        );
        let lines = window_row(
            &w,
            Some(&[3000]),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains("node"), "program missing: {text}");
        assert!(text.contains("server.js"), "args missing: {text}");
        assert!(text.contains(":3000"), "port missing: {text}");

        // Span checks for two-tone styling:
        // spans: marker, icon, program (" node"), args (" server.js")
        let prog_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("node"))
            .expect("node span");
        assert_eq!(prog_span.style.fg, Some(theme.text_active));

        let args_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("server.js"))
            .expect("server.js span");
        assert_eq!(args_span.style.fg, Some(theme.text_muted));
    }

    #[test]
    fn window_row_truncates_args_when_width_constrained() {
        let theme = ColorTheme::default();
        let w = window_with_args(
            "test",
            "cargo",
            Some("test --all --package very-long-name"),
            crate::tmux::WindowStatus::Task,
        );
        // Small width constraint: 20 columns
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            20,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains("cargo"), "program must fit: {text}");
        assert!(text.contains('…'), "args should be truncated: {text}");
    }

    #[test]
    fn window_row_keeps_program_visible_when_ports_are_many() {
        // Regression: a process listening on a huge port range (expo dev
        // server) reserved its entire port list first and crowded the
        // program name off the row. The name must always win; the port
        // list truncates lowest-first behind an ellipsis instead.
        let theme = ColorTheme::default();
        let w = window_with_args(
            "dev",
            "node",
            Some("server.js --port=8081 --host=0.0.0.0"),
            crate::tmux::WindowStatus::Task,
        );
        let ports: Vec<u16> = (8100..8130).collect();
        let lines = window_row(
            &w,
            Some(&ports),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            text.contains(" node "),
            "program name must render in full: {text}"
        );
        assert!(text.contains(":8100"), "lowest ports render first: {text}");
        assert!(
            text.contains('\u{2026}'),
            "dropped ports need an ellipsis: {text}"
        );
        assert!(
            !text.contains("8129"),
            "highest ports must be dropped: {text}"
        );
        assert!(display_width(&text) <= 40, "row must fit its width: {text}");

        // A guaranteed gap separates the program from the port list.
        let port_idx = line
            .spans
            .iter()
            .position(|s| s.content.starts_with(':'))
            .expect("port span should exist");
        let gap = &line.spans[port_idx - 1];
        assert!(
            !gap.content.is_empty()
                && gap.content.chars().all(|c| c == ' ')
                && display_width(&gap.content) >= 2,
            "expected a >= 2 column gap before the ports, got {gap:?}"
        );
    }

    #[test]
    fn window_row_ports_truncate_lowest_first() {
        let theme = ColorTheme::default();
        let w = window("web", "node", crate::tmux::WindowStatus::Task);
        let ports = [3000u16, 3001, 3010, 9050];
        let lines = window_row(
            &w,
            Some(&ports),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            30,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            text.contains(":3000, 3001, 3010\u{2026}"),
            "lowest ports stay, got: {text}"
        );
        assert!(!text.contains("9050"), "highest port dropped: {text}");
        assert!(text.contains("node"), "program must stay visible: {text}");
    }

    #[test]
    fn window_row_shows_full_port_list_when_it_fits() {
        let theme = ColorTheme::default();
        let w = window("web", "node", crate::tmux::WindowStatus::Task);
        let lines = window_row(
            &w,
            Some(&[3000, 5173]),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains(":3000, 5173"), "full list fits: {text}");
        assert!(
            !text.contains('\u{2026}'),
            "no ellipsis when everything fits: {text}"
        );
    }

    #[test]
    fn window_row_drops_ports_when_program_name_fills_the_row() {
        // The name outranks the ports even at the extreme: a program
        // name that needs the whole row leaves no room for ports.
        let theme = ColorTheme::default();
        let w = window(
            "web",
            "a-very-long-program-name-here",
            crate::tmux::WindowStatus::Task,
        );
        let lines = window_row(
            &w,
            Some(&[3000]),
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            20,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(
            !text.contains(":3000"),
            "no room for ports when the name fills the row: {text}"
        );
        assert!(
            text.contains("a-very-long-pro"),
            "name still renders (truncated to the row): {text}"
        );
    }

    #[test]
    fn port_text_lowest_first_returns_full_list_when_it_fits() {
        assert_eq!(
            port_text_lowest_first(Some(&[3000, 5173]), 40),
            Some(":3000, 5173".to_string())
        );
        assert_eq!(port_text_lowest_first(None, 40), None);
        assert_eq!(port_text_lowest_first(Some(&[]), 40), None);
    }

    #[test]
    fn port_text_lowest_first_keeps_lowest_ports_with_ellipsis() {
        let ports: Vec<u16> = (3000..3020).collect();
        let out = port_text_lowest_first(Some(&ports), 20).expect("some ports fit");
        assert!(out.starts_with(":3000"), "lowest first: {out}");
        assert!(out.ends_with('\u{2026}'), "ellipsis marks the rest: {out}");
        assert!(!out.contains("3019"), "highest ports dropped: {out}");
        assert!(display_width(&out) <= 20, "must honor budget: {out}");
    }

    #[test]
    fn port_text_lowest_first_returns_none_when_nothing_fits() {
        assert_eq!(port_text_lowest_first(Some(&[3000]), 2), None);
    }

    #[test]
    fn window_row_finished_shows_split_task_command_and_exit_note() {
        let theme = ColorTheme::default();
        let w = window("tests", "zsh", crate::tmux::WindowStatus::Idle);
        let finished = crate::state::WindowFinished {
            command: "cargo test".into(),
            exit_code: Some(101),
            finished_at: 0,
        };
        let lines = window_row(
            &w,
            None,
            Some(&finished),
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "window rows render a single line");
        let line = &lines[0];
        let text = line_text(line);
        assert!(text.contains("cargo"), "program missing: {text}");
        assert!(text.contains("test"), "args missing: {text}");
        assert!(text.contains("(exit 101)"), "exit note missing: {text}");

        let prog_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("cargo"))
            .expect("cargo span");
        assert_eq!(prog_span.style.fg, Some(theme.text_active));

        let args_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("test"))
            .expect("test span");
        assert_eq!(args_span.style.fg, Some(theme.text_muted));
    }

    // ─── expanded window card: line 2 ───────────────────────────────

    #[test]
    fn window_row_expanded_renders_session_and_window_on_second_line() {
        // Repo-grouped view: the group title is the repo, so line 2
        // names the *other* dimension — the session plus the tmux
        // `index:name` window selector.
        let theme = ColorTheme::default();
        let mut w = window_with_args(
            "web",
            "node",
            Some("server.js"),
            crate::tmux::WindowStatus::Task,
        );
        w.session_name = "main".into();
        w.window_index = 2;
        let lines = window_row(
            &w,
            Some(&[3000]),
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(
            lines.len(),
            2,
            "expanded window rows render a two-line card"
        );
        let second = line_text(&lines[1]);
        assert!(
            second.contains("main · 2:web"),
            "line 2 must carry session · index:name, got: {second}"
        );
        assert!(
            second.starts_with("    "),
            "line 2 must carry the gutter plus 2-column detail indent, got: {second:?}"
        );
        let detail_span = lines[1]
            .spans
            .iter()
            .find(|s| s.content.contains("main · 2:web"))
            .expect("context span");
        assert_eq!(detail_span.style.fg, Some(theme.text_inactive));

        // Line 1 is unchanged: diamond, program, args, ports.
        let first = line_text(&lines[0]);
        assert!(first.contains('\u{25C6}'), "diamond missing: {first}");
        assert!(
            first.contains("node") && first.contains("server.js"),
            "program line changed: {first}"
        );
        assert!(first.contains(":3000"), "ports missing: {first}");
    }

    #[test]
    fn window_row_expanded_session_view_renders_repo_basename() {
        // Session-grouped view: the group title is the session, so line
        // 2 names the repo (basename of its root) plus the window name.
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Busy);
        w.git_info.repo_root = Some("/home/dev/tmux-agent-sidebar".into());
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Session,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2);
        let second = line_text(&lines[1]);
        assert!(
            second.contains("tmux-agent-sidebar · web"),
            "line 2 must carry repo basename · window name, got: {second}"
        );
    }

    #[test]
    fn window_row_expanded_session_view_without_repo_shows_window_name_only() {
        let theme = ColorTheme::default();
        let w = window("web", "node", crate::tmux::WindowStatus::Busy);
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Session,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2);
        let second = line_text(&lines[1]);
        assert!(second.contains("web"), "window name must render: {second}");
        assert!(
            !second.contains('·'),
            "no repo means no separator, got: {second}"
        );
    }

    #[test]
    fn window_row_expanded_second_line_has_no_selection_background() {
        // Mirrors
        // `render_pane_lines_selected_leaves_content_rows_unhighlighted`:
        // the detail line is content, so the selection background stays
        // on the status line only.
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Task);
        w.session_name = "main".into();
        w.window_index = 2;
        let lines = window_row(
            &w,
            None,
            None,
            true,
            false,
            false,
            true,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.bg == Some(theme.selection_bg)),
            "line 1 keeps the selection background"
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .all(|s| s.style.bg != Some(theme.selection_bg)),
            "line 2 must not carry the selection background"
        );
    }

    #[test]
    fn window_row_expanded_second_line_has_no_active_marker() {
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Task);
        w.session_name = "main".into();
        let lines = window_row(
            &w,
            None,
            None,
            false,
            true,
            false,
            true,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].spans[0].content, SELECTION_MARKER,
            "line 1 carries the active marker"
        );
        assert_eq!(
            lines[1].spans[0].content, " ",
            "line 2 is a plain detail row without the active marker"
        );
    }

    #[test]
    fn window_row_expanded_degrades_to_one_line_when_nothing_to_show() {
        let theme = ColorTheme::default();
        // Session view: no repo and an empty window name — both
        // segments of line 2 are empty.
        let w = window("", "node", crate::tmux::WindowStatus::Busy);
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Session,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "nothing to show on line 2 → single line");
        // Repo view: no session name and no window name degrades too.
        let mut w = w;
        w.session_name = String::new();
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "nothing to show on line 2 → single line");
    }

    #[test]
    fn window_row_expanded_degrades_to_one_line_when_too_narrow() {
        // An inner width of 18 leaves 16 columns for line 2 after the
        // indent: the composed text truncates with an ellipsis, keeping
        // the `other` segment (the session) as long as possible — the
        // window name, as the last segment, is the tail that gets cut.
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Busy);
        w.session_name = "a-very-long-session-name".into();
        w.window_index = 2;
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            20,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2);
        let second = line_text(&lines[1]);
        assert!(
            second.contains('\u{2026}'),
            "line 2 must truncate with an ellipsis: {second}"
        );
        assert!(
            display_width(&second) <= 20,
            "line 2 must fit the row width: {second}"
        );
        assert!(
            second.contains("a-very-long-ses\u{2026}"),
            "other segment stays as long as possible: {second}"
        );
        assert!(
            !second.contains("web"),
            "the window name is the truncated tail: {second}"
        );
    }

    #[test]
    fn window_row_expanded_degrades_to_one_line_below_three_columns() {
        // Width 6 leaves only 2 columns for line 2 content (6 − gutter 2
        // − indent 2): fewer than the 3 meaningful columns the fragment
        // needs, so the card stays on its single status line.
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Busy);
        w.session_name = "main".into();
        w.window_index = 2;
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            6,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "a 2-column line 2 is pure noise");
    }

    // ─── ssh cards: remote pane title on line 2 ─────────────────────

    #[test]
    fn window_context_text_ssh_shows_remote_title_verbatim() {
        let mut w = window("shell", "ssh", crate::tmux::WindowStatus::Idle);
        w.pane_title = "✳ React.framework codesign errSecInternalComponent".into();
        assert_eq!(
            window_context_text(&w, GroupBy::Repo).as_deref(),
            Some("✳ React.framework codesign errSecInternalComponent"),
            "a real remote title becomes line 2 exactly"
        );
        assert_eq!(
            window_context_text(&w, GroupBy::Session).as_deref(),
            Some("✳ React.framework codesign errSecInternalComponent"),
            "the remote title replaces both grouping dimensions"
        );
    }

    #[test]
    fn window_context_text_ssh_stale_local_hostname_falls_back() {
        // Before the remote publishes its own title, pane_title still
        // holds what the local shell set before exec: the local
        // hostname. That is not remote content — today's context line
        // stays. (Uses the helper's own value, so the assertion holds
        // whether or not gethostname succeeds on this host.)
        let mut w = window("shell", "ssh", crate::tmux::WindowStatus::Idle);
        w.session_name = "main".into();
        w.window_index = 2;
        w.pane_title = crate::process::local_hostname().to_string();
        assert_eq!(
            window_context_text(&w, GroupBy::Repo).as_deref(),
            Some("main · 2:shell"),
            "stale local-hostname title must keep the fallback line"
        );
    }

    #[test]
    fn window_context_text_ssh_empty_title_falls_back() {
        let mut w = window("shell", "ssh", crate::tmux::WindowStatus::Idle);
        w.session_name = "main".into();
        w.window_index = 2;
        w.pane_title = String::new();
        assert_eq!(
            window_context_text(&w, GroupBy::Repo).as_deref(),
            Some("main · 2:shell")
        );
    }

    #[test]
    fn window_context_text_non_ssh_ignores_pane_title() {
        let mut w = window("web", "node", crate::tmux::WindowStatus::Task);
        w.session_name = "main".into();
        w.window_index = 2;
        w.pane_title = "✳ remote session".into();
        assert_eq!(
            window_context_text(&w, GroupBy::Repo).as_deref(),
            Some("main · 2:web"),
            "the title is honored on ssh cards only"
        );
    }

    #[test]
    fn window_row_expanded_ssh_card_second_line_is_remote_title() {
        let theme = ColorTheme::default();
        let mut w = window("shell", "ssh", crate::tmux::WindowStatus::Idle);
        w.pane_title = "✳ React.framework codesign errSecInternalComponent".into();
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            true,
            GroupBy::Repo,
            80,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 2, "expanded ssh cards stay two-line");
        let second = line_text(&lines[1]);
        assert!(
            second.contains("React.framework codesign errSecInternalComponent"),
            "line 2 must carry the remote title, got: {second}"
        );
        assert!(
            !second.contains("main"),
            "the local-cwd repo tag must not leak onto the ssh card: {second}"
        );
    }

    #[test]
    fn window_row_compact_ssh_card_stays_single_line() {
        let theme = ColorTheme::default();
        let mut w = window("shell", "ssh", crate::tmux::WindowStatus::Idle);
        w.pane_title = "✳ React.framework codesign errSecInternalComponent".into();
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            80,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "compact stays strictly one line");
    }

    #[test]
    fn window_row_compact_stays_single_line_with_context_available() {
        // Density contract: session/repo context is available, but
        // compact mode renders strictly one line.
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Task);
        w.session_name = "main".into();
        w.window_index = 2;
        w.git_info.repo_root = Some("/tmp/project".into());
        let lines = window_row(
            &w,
            None,
            None,
            false,
            false,
            false,
            false,
            GroupBy::Repo,
            40,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert_eq!(lines.len(), 1, "compact window rows are strictly one line");
        let text = line_text(&lines[0]);
        assert!(
            !text.contains("main"),
            "compact must not render the context line: {text}"
        );
    }

    // ─── flag marker ─────────────────────────────────────────────────

    #[test]
    fn flagged_row_renders_orange_marker_on_gutter_rows() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "fix the bug");
        p.flagged = true;
        let git = PaneGitInfo {
            repo_root: Some("/r".into()),
            branch: Some("feat/x".into()),
            is_worktree: false,
            ..Default::default()
        };
        let lines = render_pane_lines_with_ports(
            &p,
            &git,
            None,
            None,
            false, // selected
            false, // active
            true,  // flagged
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );

        fn marker<'a>(line: &'a Line<'a>) -> &'a Span<'a> {
            line.spans.first().expect("marker span")
        }
        // Status row and branch/ports row carry the flag marker…
        assert_eq!(marker(&lines[0]).content, SELECTION_MARKER);
        assert_eq!(marker(&lines[0]).style.fg, Some(theme.flag));
        assert_eq!(marker(&lines[1]).content, SELECTION_MARKER);
        assert_eq!(marker(&lines[1]).style.fg, Some(theme.flag));
        // …but deeper detail rows stay plain, exactly like the active marker.
        assert_eq!(marker(&lines[2]).content, " ");
    }

    #[test]
    fn flagged_wins_over_active_marker() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false, // selected
            true,  // active
            true,  // flagged
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content, SELECTION_MARKER);
        assert_eq!(
            marker.style.fg,
            Some(theme.flag),
            "flag must win the gutter"
        );
        assert_ne!(marker.style.fg, Some(theme.accent));
    }

    #[test]
    fn flagged_selected_row_keeps_selection_bg_on_marker() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "");
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            true,  // selected
            false, // active
            true,  // flagged
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let marker = &lines[0].spans[0];
        assert_eq!(marker.style.fg, Some(theme.flag));
        assert_eq!(
            marker.style.bg,
            Some(theme.selection_bg),
            "the flag marker must carry the selection background"
        );
    }

    #[test]
    fn unflagged_row_keeps_today_s_marker_behavior() {
        let theme = ColorTheme::default();
        let p = pane(PermissionMode::Default, PaneStatus::Running, "");

        // Inactive + unflagged: blank gutter, no flag color anywhere.
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            false,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content, " ");
        assert_ne!(marker.style.fg, Some(theme.flag));

        // Active + unflagged: the blue accent marker, as before.
        let lines = render_pane_lines_with_ports(
            &p,
            &PaneGitInfo::default(),
            None,
            None,
            false,
            true,
            false,
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content, SELECTION_MARKER);
        assert_eq!(marker.style.fg, Some(theme.accent));
    }

    #[test]
    fn compact_row_renders_flag_marker() {
        let theme = ColorTheme::default();
        let mut p = pane(PermissionMode::Default, PaneStatus::Running, "");
        p.flagged = true;
        let lines = compact_row(
            &p,
            &PaneGitInfo::default(),
            false, // selected
            false, // active
            true,  // flagged
            40,
            &StatusIcons::default(),
            &theme,
            0,
            0,
        );
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content, SELECTION_MARKER);
        assert_eq!(marker.style.fg, Some(theme.flag));
    }

    #[test]
    fn window_row_renders_flag_marker_on_line_1_only() {
        let theme = ColorTheme::default();
        let mut w = window("web", "node", crate::tmux::WindowStatus::Task);
        w.flagged = true;
        let lines = window_row(
            &w,
            None,
            None,
            false, // selected
            false, // active
            true,  // flagged
            true,  // expanded
            GroupBy::Repo,
            60,
            &StatusIcons::default(),
            &theme,
            0,
        );
        assert!(lines.len() >= 2, "expanded card renders a detail line");
        let marker = &lines[0].spans[0];
        assert_eq!(marker.content, SELECTION_MARKER);
        assert_eq!(marker.style.fg, Some(theme.flag));
        let detail_marker = &lines[1].spans[0];
        assert_eq!(detail_marker.content, " ", "detail line has no flag marker");
    }
}
