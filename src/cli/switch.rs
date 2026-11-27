//! The `switch` subcommand: a native quick switcher over live agent panes.
//!
//! fzf-shaped but self-contained — the plugin ships a single binary with no
//! runtime deps, so the fuzzy finder is a small scorer of our own (see
//! [`fuzzy_score`]) and the UI is the same ratatui-in-a-tmux-popup pattern
//! the overview uses. The pane list (one `list-panes -a` snapshot), the
//! jump (`select_pane`'s switch-client → select-window → select-pane
//! chain, proven from inside the overview's popup), and the popup spawn
//! (the `--popup --popup-client` pattern shared with the overview) are all
//! reused; the new surface is just the query → rank → select loop.
//!
//! Agents only, like the sidebar: panes whose `@pane_agent` (or foreground
//! command, or process ancestry for shimmed launchers) resolves to an
//! `AgentType`. The snapshot refreshes once a second while open so an
//! agent that starts or dies mid-query still lands in (or leaves) the
//! list, and the selection stays pinned to its pane across refreshes —
//! an index would silently move it onto a different agent, the lesson the
//! overview's grid learned first.
//!
//! Ordering is recency-first so `<key> <enter> <key> <enter>` bounces
//! between the two agents you are working in, alt-tab style. Every jump
//! is recorded in the `@sidebar_switch_mru` global option (most recent
//! first, capped); on open the list sorts by that recency (never-jumped
//! panes keep tmux order behind them) and the default selection is the
//! first entry that is not the pane you are on — the "last agent". With
//! a query typed, ranking is relevance instead (recency is no opinion
//! about what you are searching for), and clearing the query falls back
//! to the recency default.
//!
//! Keys: type to filter, ↑/↓ (or Ctrl-N/Ctrl-P) to move, Enter to jump —
//! the popup closes and the invoking client lands on the pane — Esc or
//! Ctrl-C to leave without jumping. Line editing is the shared
//! [`LineEdit`] vocabulary (arrows, home/end, Ctrl-U clear, Ctrl-W kill
//! word).

use std::collections::HashSet;
use std::io;
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::line_edit::LineEdit;
use crate::tmux::{self, AgentType, PaneInfo, PaneStatus};
use crate::ui::colors::ColorTheme;
use crate::ui::icons::StatusIcons;
use crate::ui::text::display_width;

/// How often the pane snapshot rebuilds while the switcher is open. A
/// quick switcher is a seconds-long interaction; one spawn a second keeps
/// statuses and the agent set honest without the overview's drive cadence.
const REFRESH: Duration = Duration::from_millis(1000);

/// Popup geometry defaults: compact and centered (tmux centers when -x/-y
/// are omitted). A quick switcher is a dialog, not a dashboard — the
/// overview owns the fullscreen end of the spectrum. Overridable with the
/// `@sidebar_switch_width` / `@sidebar_switch_height` global options
/// (tmux popup size strings: percentages or column/row counts).
const POPUP_WIDTH_DEFAULT: &str = "80%";
const POPUP_HEIGHT_DEFAULT: &str = "60%";
const SWITCH_WIDTH_OPTION: &str = "@sidebar_switch_width";
const SWITCH_HEIGHT_OPTION: &str = "@sidebar_switch_height";

/// The switcher's own recency memory: a global option holding the jumped-
/// to pane ids, most recent first. Only switcher jumps write it — manual
/// tmux movement is not tracked — but the alt-tab default selection reads
/// the pane you are actually on from tmux, so the flow survives manual
/// movement between switcher uses.
const MRU_OPTION: &str = "@sidebar_switch_mru";
/// Cap on stored recency entries: pane ids are a few bytes each; 32
/// covers any real agent set and keeps the option trivially small.
const MRU_MAX: usize = 32;

/// One agent pane as the switcher sees it: the identity the jump needs
/// (the pane id) plus the searchable and renderable text.
struct SwitchEntry {
    pane_id: String,
    status: PaneStatus,
    agent: AgentType,
    /// Repo basename, plus the worktree name in parens when the pane has
    /// one — the same identity the sidebar's row leads with.
    repo_label: String,
    /// Row title: the agent session's name when known, else the window
    /// name, else the tmux session.
    title: String,
    /// Last prompt/response text the hook layer captured, whitespace-
    /// collapsed to one line.
    prompt: String,
    /// Everything searchable as one char sequence: the fields above plus
    /// the worktree branch and the tmux session/window names. Pre-lowered
    /// matching happens against this ([`fuzzy_score`] folds case itself).
    haystack: Vec<char>,
}

/// The last path segment of a repo anchor — `/home/u/proj` → `proj`, with
/// a trailing slash tolerated (`/home/u/proj/` → `proj`).
fn path_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    let start = trimmed.rfind('/').map(|i| i + 1).unwrap_or(0);
    &trimmed[start..]
}

fn entry_from_info(info: &PaneInfo) -> SwitchEntry {
    let repo = path_basename(info.grouping_anchor()).to_string();
    let repo_label = if info.worktree.name.is_empty() {
        repo
    } else {
        format!("{} ({})", repo, info.worktree.name)
    };
    let title = if !info.session_name.is_empty() {
        info.session_name.clone()
    } else if !info.window_name.is_empty() {
        info.window_name.clone()
    } else {
        info.tmux_session.clone()
    };
    let prompt = info.prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut haystack = String::new();
    for field in [
        info.agent.label(),
        &repo_label,
        &title,
        info.worktree.name.as_str(),
        info.worktree.branch.as_str(),
        info.window_name.as_str(),
        info.tmux_session.as_str(),
        prompt.as_str(),
    ] {
        if field.is_empty() {
            continue;
        }
        if !haystack.is_empty() {
            haystack.push(' ');
        }
        haystack.push_str(field);
    }
    SwitchEntry {
        pane_id: info.pane_id.clone(),
        status: info.status.clone(),
        agent: info.agent.clone(),
        repo_label,
        title,
        prompt,
        haystack: haystack.chars().collect(),
    }
}

/// One fresh `list-panes -a` pass, flattened to agent entries in tmux's
/// own session → window → pane order (recency re-orders it in
/// [`SwitchState::refresh`]). `None` when the listing spawn failed — the
/// caller keeps its previous frame.
fn collect_entries() -> Option<Vec<SwitchEntry>> {
    let listing = tmux::fetch_pane_listing()?;
    let process_snapshot = tmux::process_snapshot_for_panes(&listing);
    let snapshot = tmux::build_session_snapshot_with(&listing, "%switch-none", process_snapshot);
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for session in &snapshot.sessions {
        for window in &session.windows {
            for info in &window.panes {
                if info.agent == AgentType::Unknown {
                    continue;
                }
                // Grouped sessions can surface the same pane twice; the
                // pane id is the dedup key (same as the overview's grid).
                if seen.insert(info.pane_id.clone()) {
                    entries.push(entry_from_info(info));
                }
            }
        }
    }
    Some(entries)
}

// ─── Recency (MRU) memory ────────────────────────────────────────────────────

/// Read the jump-recency list from the global option. Empty when unset —
/// a fresh install, or any session the switcher has not jumped in yet.
fn load_mru() -> Vec<String> {
    tmux::run_tmux(&["show-options", "-gv", MRU_OPTION])
        .map(|out| out.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Persist the recency list, most recent first. Best-effort: a failed
/// write only degrades the next open's default ordering.
fn save_mru(mru: &[String]) {
    if mru.is_empty() {
        return;
    }
    let joined = mru.join(" ");
    let _ = tmux::run_tmux(&["set", "-g", MRU_OPTION, &joined]);
}

/// Record a jump: the target moves to the front, duplicates collapse,
/// and the list keeps its cap.
fn push_mru(mru: &mut Vec<String>, pane_id: &str) {
    mru.retain(|id| id != pane_id);
    mru.insert(0, pane_id.to_string());
    mru.truncate(MRU_MAX);
}

// ─── Fuzzy matching ──────────────────────────────────────────────────────────

/// Word-boundary match bonus: the query char sits at the target's start
/// or right after a non-alphanumeric separator (`/`, `-`, `_`, space...).
const BOUNDARY_BONUS: i64 = 8;
/// Consecutive-match bonus: this query char landed directly after the
/// previous one's position.
const CONSECUTIVE_BONUS: i64 = 4;
/// Gap penalty cap: each skipped target char costs one point, but a long
/// gap stops punishing after this many so one big jump cannot dominate.
const GAP_PENALTY_CAP: i64 = 6;

/// Greedy subsequence score: each query char matches the earliest still-
/// available target position, then earns bonuses for boundaries and
/// streaks and pays for the gap it jumped. Case-insensitive. `None` when
/// the query is not a subsequence at all; `Some(0)` for an empty query —
/// everything matches, and the stable sort keeps collection order.
///
/// Greedy-earliest is deliberately simple: the target is one short line
/// of fields, not a document, and the stable tiebreak (score desc, then
/// encounter order) is the behavior users expect from a switcher.
fn fuzzy_score(query: &[char], target: &[char]) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let mut score = 0i64;
    let mut prev: Option<usize> = None;
    let mut ti = 0usize;
    for &qc in query {
        let ql = fold(qc);
        let mut hit = None;
        while ti < target.len() {
            if fold(target[ti]) == ql {
                hit = Some(ti);
                break;
            }
            ti += 1;
        }
        let at = hit?;
        if at == 0 || !target[at - 1].is_alphanumeric() {
            score += BOUNDARY_BONUS;
        }
        if prev.is_some_and(|p| at == p + 1) {
            score += CONSECUTIVE_BONUS;
        }
        if let Some(p) = prev {
            let gap = (at - p - 1) as i64;
            score -= gap.min(GAP_PENALTY_CAP);
        }
        prev = Some(at);
        ti = at + 1;
    }
    Some(score)
}

// ─── State ───────────────────────────────────────────────────────────────────

/// Query, entries, and the ranked view between them. `ranked` holds
/// `(entry index, score)` sorted score-descending; `selected` indexes
/// into `ranked`.
///
/// `entries` is kept in recency order (see [`SwitchState::refresh`]), so
/// an empty query — every entry matches at score 0 — displays the MRU
/// order; a typed query re-ranks by relevance on top of it.
struct SwitchState {
    query: LineEdit,
    entries: Vec<SwitchEntry>,
    ranked: Vec<(usize, i64)>,
    selected: usize,
    /// The pane the user is on right now (resolved before the TUI
    /// started). The empty-query default selection skips it — jumping
    /// there would be a no-op — which is what makes `<key> <enter>`
    /// mean "go to the last agent".
    current_pane: Option<String>,
    /// Jump recency, most recent first. Orders the list on open and is
    /// rewritten on every jump.
    mru: Vec<String>,
}

impl SwitchState {
    fn new(entries: Vec<SwitchEntry>, mru: Vec<String>, current_pane: Option<String>) -> Self {
        let mut state = Self {
            query: LineEdit::new(),
            entries: Vec::new(),
            ranked: Vec::new(),
            selected: 0,
            current_pane,
            mru,
        };
        state.refresh(entries);
        state
    }

    /// Re-rank against the current query. Ties keep entry order (the
    /// sort is stable), so an empty query displays the recency order.
    fn requery(&mut self) {
        let query: Vec<char> = self.query.chars().to_vec();
        let mut ranked: Vec<(usize, i64)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| fuzzy_score(&query, &e.haystack).map(|score| (i, score)))
            .collect();
        ranked.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        self.ranked = ranked;
    }

    /// The query changed: re-rank and reselect. A typed query selects
    /// the top relevance match — recency says nothing about what the
    /// user is searching for. An empty (or newly cleared) query falls
    /// back to the alt-tab default.
    fn query_changed(&mut self) {
        self.requery();
        self.selected = if self.query.is_empty() {
            self.default_selection()
        } else {
            0
        };
    }

    /// The empty-query default: the first ranked entry that is not the
    /// pane the user is on — "the last agent" for the
    /// `<key> <enter> <key> <enter>` bounce. Falls back to the top when
    /// every entry is the current pane (or nothing is ranked).
    fn default_selection(&self) -> usize {
        self.ranked
            .iter()
            .position(|(i, _)| {
                Some(self.entries[*i].pane_id.as_str()) != self.current_pane.as_deref()
            })
            .unwrap_or(0)
    }

    /// The pane id behind the current selection, if any.
    fn selected_pane_id(&self) -> Option<&str> {
        self.ranked
            .get(self.selected)
            .map(|(i, _)| self.entries[*i].pane_id.as_str())
    }

    /// Swap in a fresh snapshot. Entries are (re-)ordered by jump
    /// recency — never-jumped panes keep their tmux order behind the
    /// jumped-to ones — then the selection stays pinned to its pane: the
    /// refresh may reorder or drop entries under the cursor, and an
    /// index would silently move the selection onto a different agent.
    /// No pinned pane (first load, or the selection's pane died) falls
    /// back to the alt-tab default.
    fn refresh(&mut self, entries: Vec<SwitchEntry>) {
        let pinned = self
            .ranked
            .get(self.selected)
            .map(|(i, _)| self.entries[*i].pane_id.clone());
        let mut ordered = entries;
        ordered.sort_by_key(|e| {
            self.mru
                .iter()
                .position(|id| id == &e.pane_id)
                .unwrap_or(usize::MAX)
        });
        self.entries = ordered;
        self.requery();
        self.selected = match pinned.as_deref().and_then(|id| {
            self.ranked
                .iter()
                .position(|(i, _)| self.entries[*i].pane_id == id)
        }) {
            Some(pos) => pos,
            None => self.default_selection(),
        };
    }

    /// Record a jump: recency moves the target to the front and the
    /// list persists for the next open.
    fn record_jump(&mut self, pane_id: &str) {
        push_mru(&mut self.mru, pane_id);
        save_mru(&self.mru);
    }

    /// Move the selection, clamping at both ends — off either edge is the
    /// neutral state, not a wraparound jump (same convention as the
    /// overview's grid).
    fn move_selection(&mut self, delta: i32) {
        let len = self.ranked.len();
        if len == 0 {
            return;
        }
        let selected = self.selected as i64 + delta as i64;
        self.selected = selected.clamp(0, len as i64 - 1) as usize;
    }
}

// ─── Keys ────────────────────────────────────────────────────────────────────

/// What one key press asks the run loop to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyOutcome {
    Continue,
    /// Leave without jumping.
    Quit,
    /// Jump to this pane, then leave.
    Jump(String),
}

fn handle_key(state: &mut SwitchState, key: KeyEvent) -> KeyOutcome {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => KeyOutcome::Quit,
        KeyCode::Char('c') if ctrl => KeyOutcome::Quit,
        KeyCode::Enter => match state.selected_pane_id().map(str::to_string) {
            Some(pane_id) => KeyOutcome::Jump(pane_id),
            None => KeyOutcome::Continue,
        },
        KeyCode::Up => {
            state.move_selection(-1);
            KeyOutcome::Continue
        }
        KeyCode::Down => {
            state.move_selection(1);
            KeyOutcome::Continue
        }
        KeyCode::Char('p') if ctrl => {
            state.move_selection(-1);
            KeyOutcome::Continue
        }
        KeyCode::Char('n') if ctrl => {
            state.move_selection(1);
            KeyOutcome::Continue
        }
        KeyCode::Left => {
            state.query.left();
            KeyOutcome::Continue
        }
        KeyCode::Right => {
            state.query.right();
            KeyOutcome::Continue
        }
        KeyCode::Home => {
            state.query.home();
            KeyOutcome::Continue
        }
        KeyCode::End => {
            state.query.end();
            KeyOutcome::Continue
        }
        KeyCode::Backspace => {
            state.query.backspace();
            state.query_changed();
            KeyOutcome::Continue
        }
        KeyCode::Char('u') if ctrl => {
            state.query.clear();
            state.query_changed();
            KeyOutcome::Continue
        }
        KeyCode::Char('w') if ctrl => {
            state.query.pop_word();
            state.query_changed();
            KeyOutcome::Continue
        }
        // Plain typed text lands in the query — everything else (modified
        // keys the arms above did not claim) is ignored.
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            state.query.push(c);
            state.query_changed();
            KeyOutcome::Continue
        }
        _ => KeyOutcome::Continue,
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, state: &SwitchState, theme: &ColorTheme, icons: &StatusIcons) {
    let area = f.area();
    if area.width < 8 || area.height < 4 {
        return;
    }
    let block = Block::bordered()
        .title(" switch ")
        .title_style(Style::default().fg(theme.text_muted))
        .border_style(Style::default().fg(theme.border_inactive));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // query line
            Constraint::Fill(1),   // results
            Constraint::Length(1), // key hints
        ])
        .split(inner);

    draw_input(f, state, rows[0], theme);
    draw_results(f, state, rows[1], theme, icons);
    let hints = Paragraph::new("↑↓ select · enter switch · esc close")
        .style(Style::default().fg(theme.text_muted));
    f.render_widget(hints, rows[2]);
}

/// The query line: a muted `> ` prompt glyph, the query text, and the
/// real terminal cursor. Once the query outgrows the row the visible
/// window slides so the cursor stays inside (parked on the last column
/// when the text runs to the edge).
fn draw_input(f: &mut Frame, state: &SwitchState, area: Rect, theme: &ColorTheme) {
    let prefix = "> ";
    let prefix_w = prefix.len() as u16;
    let avail = area.width.saturating_sub(prefix_w) as usize;
    let chars: Vec<char> = state.query.chars().to_vec();
    let cursor = state.query.cursor();
    let start = if chars.len() <= avail || cursor < avail {
        0
    } else {
        cursor + 1 - avail
    };
    let end = (start + avail).min(chars.len());
    let visible: String = chars[start..end].iter().collect();
    let line = Line::from(vec![
        Span::styled(prefix, Style::default().fg(theme.text_muted)),
        Span::styled(visible, Style::default().fg(theme.text_active)),
    ]);
    f.render_widget(Paragraph::new(line), area);
    let column = (cursor - start).min(avail.saturating_sub(1)) as u16;
    f.set_cursor_position(Position {
        x: area.x + prefix_w + column,
        y: area.y,
    });
}

fn draw_results(
    f: &mut Frame,
    state: &SwitchState,
    area: Rect,
    theme: &ColorTheme,
    icons: &StatusIcons,
) {
    if area.height == 0 {
        return;
    }
    if state.ranked.is_empty() {
        let message = if state.entries.is_empty() {
            "no agent panes"
        } else {
            "no match"
        };
        f.render_widget(
            Paragraph::new(message).style(Style::default().fg(theme.text_muted)),
            area,
        );
        return;
    }
    // Stateless scroll window: slide only when the selection leaves the
    // visible rows, so ordinary ↑/↓ moves never shift the whole list.
    let height = area.height as usize;
    let selected = state.selected;
    let offset = selected.saturating_sub(height.saturating_sub(1));
    for (row, (entry_idx, _)) in state.ranked.iter().enumerate().skip(offset).take(height) {
        let entry = &state.entries[*entry_idx];
        let is_selected = row == selected;
        let bg = is_selected.then_some(theme.selection_bg);
        let line = entry_line(entry, area.width as usize, theme, icons, bg);
        f.render_widget(
            Paragraph::new(line),
            Rect {
                x: area.x,
                y: area.y + (row - offset) as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// One result row: status icon, agent, repo (with worktree), session
/// title, prompt excerpt — left to right, truncated at the row edge with
/// an ellipsis on the field that crosses it. The selection bar is a row-
/// wide background, so the line pads out to the full width.
fn entry_line(
    entry: &SwitchEntry,
    width: usize,
    theme: &ColorTheme,
    icons: &StatusIcons,
    bg: Option<Color>,
) -> Line<'static> {
    let mut pieces: Vec<(String, Style)> = vec![
        (
            icons.status_icon(&entry.status).to_string(),
            Style::default().fg(theme.status_color(&entry.status)),
        ),
        ("  ".to_string(), Style::default()),
        (
            entry.agent.label().to_string(),
            Style::default().fg(agent_color(theme, &entry.agent)),
        ),
        ("  ".to_string(), Style::default()),
        (
            entry.repo_label.clone(),
            Style::default().fg(theme.text_active),
        ),
    ];
    if !entry.title.is_empty() {
        pieces.push(("  ".to_string(), Style::default()));
        pieces.push((entry.title.clone(), Style::default().fg(theme.text_active)));
    }
    if !entry.prompt.is_empty() {
        pieces.push(("  ".to_string(), Style::default()));
        pieces.push((entry.prompt.clone(), Style::default().fg(theme.text_muted)));
    }

    let mut used = 0usize;
    let mut spans = Vec::new();
    for (text, style) in pieces {
        if used >= width {
            break;
        }
        let piece_width = display_width(&text);
        if used + piece_width <= width {
            used += piece_width;
            spans.push(Span::styled(text, style));
        } else {
            // The field that crosses the edge is cut to the remaining
            // columns, with the ellipsis replacing any space the cut
            // would leave dangling.
            let cut = fit_field(&text, width - used);
            spans.push(Span::styled(cut, style));
            used = width;
        }
    }
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), Style::default()));
    }
    let mut style = Style::default();
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    Line::from(spans).style(style)
}

/// Cut `text` to at most `room` display columns, ending in an ellipsis,
/// without leaving a space dangling before it. Mirrors ui::text's
/// truncate_to_width budgeting (kept content + `…` within `room`) but
/// right-trims the kept content first — a cut like "that keeps …" reads
/// sloppy; "that keeps…" is the shape a switcher row wants.
fn fit_field(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if display_width(text) <= room {
        return text.to_string();
    }
    let mut kept = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > room {
            break;
        }
        kept.push(ch);
        w += cw;
    }
    format!("{}…", kept.trim_end())
}

fn agent_color(theme: &ColorTheme, agent: &AgentType) -> Color {
    match agent {
        AgentType::Claude => theme.agent_claude,
        AgentType::Codex => theme.agent_codex,
        AgentType::OpenCode => theme.agent_opencode,
        AgentType::Hermes => theme.agent_hermes,
        AgentType::Antigravity => theme.agent_antigravity,
        AgentType::Unknown => theme.text_muted,
    }
}

// ─── Run loop ────────────────────────────────────────────────────────────────

fn run_tui() -> io::Result<i32> {
    // One tmux spawn each for the palette and the icon set; the switcher
    // renders with the same status/agent colors as the sidebar.
    let theme = ColorTheme::from_tmux();
    let icons = StatusIcons::from_tmux();
    enable_raw_mode()?;
    let result = (|| -> io::Result<i32> {
        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        // The pane the user is on: TMUX_PANE when running inside a pane
        // (bare TUI mode), or passed through the popup's environment by
        // spawn_popup — tmux does not set TMUX_PANE for popup children,
        // so spawn_popup resolves it (from the invoking shell's own env,
        // or a probe of the pressing client) and hands it over with -e.
        // The empty-query default selection skips this pane, so it must
        // be known for the alt-tab flow.
        let current_pane = std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty());

        let mut state = SwitchState::new(Vec::new(), load_mru(), current_pane);
        if let Some(entries) = collect_entries() {
            state.refresh(entries);
        }
        let mut last_refresh = Instant::now();

        loop {
            if last_refresh.elapsed() >= REFRESH {
                // A failed listing keeps the previous frame — one tmux
                // hiccup must not blank the list mid-query.
                if let Some(entries) = collect_entries() {
                    state.refresh(entries);
                }
                last_refresh = Instant::now();
            }

            terminal.draw(|f| draw(f, &state, &theme, &icons))?;

            let timeout = REFRESH.saturating_sub(last_refresh.elapsed());
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match handle_key(&mut state, key) {
                            KeyOutcome::Continue => {}
                            KeyOutcome::Quit => return Ok(0),
                            KeyOutcome::Jump(pane_id) => {
                                // Recency first (best-effort write), then
                                // the jump: the popup closes as this
                                // process exits (`-E`) and the invoking
                                // client lands on the pane — the
                                // overview's `f` path does the same.
                                state.record_jump(&pane_id);
                                tmux::select_pane(&pane_id);
                                return Ok(0);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    })();

    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}

/// Popup geometry from the global options, falling back to the defaults
/// when unset or empty. Values pass through raw — tmux popup size strings
/// (percentages or column/row counts); a bogus value fails display-popup,
/// which surfaces as the command's error message.
fn popup_geometry() -> (String, String) {
    let read = |option: &str, default: &str| {
        tmux::run_tmux(&["show-options", "-gv", option])
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    (
        read(SWITCH_WIDTH_OPTION, POPUP_WIDTH_DEFAULT),
        read(SWITCH_HEIGHT_OPTION, POPUP_HEIGHT_DEFAULT),
    )
}

/// The pane the popup child should consider "current": the invoking
/// shell's TMUX_PANE when present (a manual invocation from a pane), else
/// a probe of the pressing client (the keybinding path — a run-shell
/// child has no pane env of its own). `None` when neither answers; the
/// switcher then falls back to plain top-of-list selection.
fn resolve_current_pane(client: Option<&str>) -> Option<String> {
    if let Ok(pane) = std::env::var("TMUX_PANE")
        && !pane.is_empty()
    {
        return Some(pane);
    }
    client.and_then(|client| {
        let pane = tmux::display_message(client, "#{pane_id}");
        (!pane.is_empty()).then_some(pane)
    })
}

/// Open the switcher in a tmux popup. With `-E` the popup closes as soon
/// as the TUI exits, so Enter (jump) and Esc (leave) both tear the whole
/// container down. `client` targets the popup at a specific tmux client —
/// the keybinding passes `#{client_name}` at keypress time, because a
/// run-shell child has no current client of its own; without it, tmux
/// resolves the client from `TMUX_PANE` (a manual invocation from any
/// pane shell).
fn spawn_popup(client: Option<String>) -> Result<(), String> {
    if client.is_none() && std::env::var("TMUX_PANE").unwrap_or_default().is_empty() {
        return Err("--popup must be run from inside tmux".into());
    }
    let (width, height) = popup_geometry();
    let exe = std::env::current_exe().map_err(|e| format!("resolve current exe: {e}"))?;
    let cwd = std::env::current_dir().map_err(|e| format!("resolve cwd: {e}"))?;
    // Through the shared exec path so the IPC gate applies (audit
    // 2026-11-19 C3). `run_tmux_blocking` matches this call's contract:
    // display-popup's invoking process owns the popup for its lifetime,
    // so the command intentionally blocks until it closes.
    let mut args: Vec<String> = vec![
        "display-popup".into(),
        "-E".into(),
        "-w".into(),
        width,
        "-h".into(),
        height,
        "-d".into(),
        cwd.to_string_lossy().to_string(),
    ];
    // tmux does not set TMUX_PANE for popup children at all — pin the
    // resolved current pane explicitly (same pattern as the detached
    // overview popup), so the child's alt-tab default knows where the
    // user is.
    if let Some(pane) = resolve_current_pane(client.as_deref()) {
        args.push("-e".into());
        args.push(format!("TMUX_PANE={pane}"));
    }
    if let Some(client) = client {
        args.push("-t".into());
        args.push(client);
    }
    // Two positionals: tmux execs them directly, no `sh -c` (tmux
    // 3.8) — the raw path is safe, quoting would corrupt argv[0].
    args.push(exe.to_string_lossy().to_string());
    args.push("switch".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if tmux::run_tmux_blocking(&args).is_some() {
        Ok(())
    } else {
        Err("tmux display-popup failed".to_string())
    }
}

pub fn cmd_switch(args: &[String]) -> i32 {
    let mut popup = false;
    let mut popup_client: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--popup" => popup = true,
            "--popup-client" => {
                let value = iter.next().map(|s| s.to_string()).unwrap_or_default();
                // A missing value or a following flag (a value starting
                // with `--`) is a caller mistake: silently consuming the
                // next flag as the client name drops it and targets a
                // client that does not exist (same shape as the
                // overview's audit 2026-11-25 L-cli-4).
                if value.is_empty() || value.starts_with("--") {
                    eprintln!("switch: --popup-client requires a client name");
                    return 2;
                }
                popup_client = Some(value);
            }
            other => {
                eprintln!("switch: unknown argument '{other}'");
                return 2;
            }
        }
    }

    if popup {
        return match spawn_popup(popup_client) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("switch: {e}");
                1
            }
        };
    }

    match run_tui() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("switch: {e}");
            1
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{PaneAttention, PermissionMode, WorktreeMetadata};
    use insta::assert_snapshot;
    use ratatui::{backend::TestBackend, buffer::Buffer};

    fn make_info(agent: AgentType, status: PaneStatus, pane_id: &str) -> PaneInfo {
        PaneInfo {
            pane_id: pane_id.into(),
            pane_active: false,
            status,
            attention: PaneAttention::None,
            agent,
            path: "/home/user/project".into(),
            launch_cwd: String::new(),
            current_command: String::new(),
            prompt: String::new(),
            prompt_is_response: false,
            started_at: None,
            wait_reason: String::new(),
            permission_mode: PermissionMode::Default,
            subagents: vec![],
            pane_pid: None,
            worktree: WorktreeMetadata::default(),
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
        }
    }

    fn entry(pane_id: &str) -> SwitchEntry {
        entry_from_info(&make_info(AgentType::Claude, PaneStatus::Running, pane_id))
    }

    /// A state with no recency history and no known current pane.
    fn state(entries: Vec<SwitchEntry>) -> SwitchState {
        SwitchState::new(entries, Vec::new(), None)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn render_frame(state: &SwitchState, w: u16, h: u16) -> String {
        let theme = ColorTheme::default();
        let icons = StatusIcons::default();
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, state, &theme, &icons)).unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area;
        let mut lines = Vec::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }

    // ─── fuzzy_score ─────────────────────────────────────────────────

    fn score(query: &str, target: &str) -> Option<i64> {
        let query: Vec<char> = query.chars().collect();
        let target: Vec<char> = target.chars().collect();
        fuzzy_score(&query, &target)
    }

    #[test]
    fn fuzzy_matches_subsequences_case_insensitively() {
        assert!(score("ccs", "Claude Code session").is_some());
        assert!(score("SW", "repo switcher").is_some());
        assert!(score("z", "Claude Code session").is_none());
        // Order matters: a permutation is not a subsequence.
        assert!(score("oc", "co").is_none());
    }

    #[test]
    fn fuzzy_empty_query_matches_everything_at_zero() {
        assert_eq!(score("", "anything"), Some(0));
        assert_eq!(score("", ""), Some(0));
    }

    #[test]
    fn fuzzy_scores_consecutive_over_scattered() {
        // "ab" in "ab": boundary + consecutive = 12.
        assert_eq!(score("ab", "ab"), Some(12));
        // "ab" in "axb": boundary, then a one-char gap.
        assert_eq!(score("ab", "axb"), Some(7));
        // Same letters scattered with no boundary at all: two non-boundary
        // hits minus the two skipped chars between them.
        assert_eq!(score("ab", "xa yb"), Some(-2));
    }

    #[test]
    fn fuzzy_prefers_word_boundaries() {
        // "sw" at the start of "switcher": boundary + consecutive.
        let at_boundary = score("sw", "switcher").unwrap();
        // "sw" buried mid-word: no boundary.
        let buried = score("sw", "xxswitcher").unwrap();
        assert!(at_boundary > buried, "{at_boundary} vs {buried}");
    }

    #[test]
    fn fuzzy_gap_penalty_is_capped() {
        let small_gap = score("ab", "axb").unwrap();
        let huge_gap = score("ab", "axxxxxxxxxxxxxxxb").unwrap();
        // A longer gap scores worse, but the difference is bounded.
        assert!(huge_gap < small_gap);
        assert!(small_gap - huge_gap <= GAP_PENALTY_CAP);
    }

    // ─── ranking ─────────────────────────────────────────────────────

    fn info_with_haystack(pane_id: &str, agent: AgentType, title: &str) -> SwitchEntry {
        let mut info = make_info(agent, PaneStatus::Running, pane_id);
        info.session_name = title.into();
        entry_from_info(&info)
    }

    #[test]
    fn ranking_orders_by_score_then_entry_order() {
        // Titles only — the agent label ("claude") must not win the match
        // for every entry and flatten the ranking to a tie.
        let mut state = SwitchState::new(
            vec![
                info_with_haystack("%1", AgentType::Claude, "xwombat"),
                info_with_haystack("%2", AgentType::Codex, "wombat"),
                info_with_haystack("%3", AgentType::Hermes, "wombat"),
            ],
            Vec::new(),
            None,
        );
        state.query.set("wom");
        state.query_changed();
        // "%2" and "%3" score the same (boundary + consecutive); "%1"
        // scores lower (buried 'cl'). Ties keep entry order.
        let ids: Vec<&str> = state
            .ranked
            .iter()
            .map(|(i, _)| state.entries[*i].pane_id.as_str())
            .collect();
        assert_eq!(ids, vec!["%2", "%3", "%1"]);
    }

    #[test]
    fn empty_query_lists_everything_in_entry_order() {
        let state = state(vec![entry("%1"), entry("%2"), entry("%3")]);
        let ids: Vec<&str> = state
            .ranked
            .iter()
            .map(|(i, _)| state.entries[*i].pane_id.as_str())
            .collect();
        assert_eq!(ids, vec!["%1", "%2", "%3"]);
    }

    #[test]
    fn query_filters_non_matching_entries() {
        let mut state = SwitchState::new(
            vec![
                info_with_haystack("%1", AgentType::Claude, "alpha"),
                info_with_haystack("%2", AgentType::Codex, "beta"),
            ],
            Vec::new(),
            None,
        );
        state.query.set("alp");
        state.query_changed();
        assert_eq!(state.ranked.len(), 1);
        assert_eq!(state.selected_pane_id(), Some("%1"));
    }

    // ─── recency ordering & the alt-tab flow ─────────────────────────

    #[test]
    fn mru_orders_the_list_most_recent_first() {
        let state = SwitchState::new(
            vec![entry("%1"), entry("%2"), entry("%3")],
            vec!["%3".into(), "%1".into()],
            None,
        );
        let ids: Vec<&str> = state
            .ranked
            .iter()
            .map(|(i, _)| state.entries[*i].pane_id.as_str())
            .collect();
        // Jumped-to panes lead in recency order; never-jumped keep
        // tmux order behind them.
        assert_eq!(ids, vec!["%3", "%1", "%2"]);
    }

    #[test]
    fn default_selection_skips_the_current_pane() {
        // On %1 with no history: the alt-tab default is the next entry —
        // Enter means "go to the other agent", not "stay here".
        let state = SwitchState::new(
            vec![entry("%1"), entry("%2")],
            Vec::new(),
            Some("%1".into()),
        );
        assert_eq!(state.selected_pane_id(), Some("%2"));
    }

    #[test]
    fn default_selection_falls_back_to_top_without_a_current_pane() {
        let state = state(vec![entry("%1"), entry("%2")]);
        assert_eq!(state.selected_pane_id(), Some("%1"));
    }

    #[test]
    fn default_selection_prefers_recency_over_tmux_order() {
        // On %2, having jumped %1 last and %3 before that: the "last
        // agent" is %1, not %3 and not the tmux-order neighbor.
        let state = SwitchState::new(
            vec![entry("%1"), entry("%2"), entry("%3")],
            vec!["%1".into(), "%3".into()],
            Some("%2".into()),
        );
        assert_eq!(state.selected_pane_id(), Some("%1"));
    }

    #[test]
    fn alt_tab_flow_bounces_between_two_agents() {
        // Open 1: on %1, no history — selection is %2, Enter jumps there.
        let mut state = SwitchState::new(
            vec![entry("%1"), entry("%2")],
            Vec::new(),
            Some("%1".into()),
        );
        assert_eq!(state.selected_pane_id(), Some("%2"));
        let target = state.selected_pane_id().unwrap().to_string();
        state.record_jump(&target);

        // Open 2 (a fresh process, the persisted recency): on %2 now —
        // selection is %1, the bounce target.
        let mut state = SwitchState::new(
            vec![entry("%1"), entry("%2")],
            state.mru.clone(),
            Some("%2".into()),
        );
        assert_eq!(state.selected_pane_id(), Some("%1"));
        let target = state.selected_pane_id().unwrap().to_string();
        state.record_jump(&target);

        // Open 3: on %1 again — selection is %2. <key> <enter> toggles.
        let state = SwitchState::new(
            vec![entry("%1"), entry("%2")],
            state.mru.clone(),
            Some("%1".into()),
        );
        assert_eq!(state.selected_pane_id(), Some("%2"));
    }

    #[test]
    fn typed_query_selects_top_and_clearing_restores_alt_tab() {
        let mut state = SwitchState::new(
            vec![
                info_with_haystack("%1", AgentType::Claude, "alpha"),
                info_with_haystack("%2", AgentType::Codex, "beta"),
            ],
            Vec::new(),
            Some("%1".into()),
        );
        // Empty query: the alt-tab default skips the current pane.
        assert_eq!(state.selected_pane_id(), Some("%2"));
        // Typing switches to relevance ranking: top match.
        state.query.set("alp");
        state.query_changed();
        assert_eq!(state.selected_pane_id(), Some("%1"));
        // Clearing falls back to the alt-tab default again.
        state.query.clear();
        state.query_changed();
        assert_eq!(state.selected_pane_id(), Some("%2"));
    }

    #[test]
    fn push_mru_moves_to_front_dedups_and_caps() {
        let mut mru: Vec<String> = vec!["%1".into(), "%2".into()];
        push_mru(&mut mru, "%3");
        assert_eq!(mru, vec!["%3", "%1", "%2"]);
        push_mru(&mut mru, "%1");
        assert_eq!(mru, vec!["%1", "%3", "%2"]);
        let last = format!("%dup{}", MRU_MAX + 4);
        for i in 0..(MRU_MAX + 5) {
            push_mru(&mut mru, &format!("%dup{i}"));
        }
        assert_eq!(mru.len(), MRU_MAX);
        assert_eq!(mru[0], last);
    }

    // ─── entry fields ────────────────────────────────────────────────

    #[test]
    fn path_basename_takes_the_last_segment() {
        assert_eq!(path_basename("/home/u/proj"), "proj");
        assert_eq!(path_basename("/home/u/proj/"), "proj");
        assert_eq!(path_basename("proj"), "proj");
        assert_eq!(path_basename(""), "");
    }

    #[test]
    fn entry_repo_label_includes_the_worktree() {
        let mut info = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        info.worktree = WorktreeMetadata {
            name: "feature-x".into(),
            branch: "feat/x".into(),
        };
        let entry = entry_from_info(&info);
        assert_eq!(entry.repo_label, "project (feature-x)");
    }

    #[test]
    fn entry_title_falls_back_through_session_window_tmux() {
        let mut info = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        info.tmux_session = "sess".into();
        info.window_name = "win".into();
        let entry = entry_from_info(&info);
        assert_eq!(entry.title, "win");
        info.session_name = "agent title".into();
        let entry = entry_from_info(&info);
        assert_eq!(entry.title, "agent title");
    }

    #[test]
    fn entry_prompt_is_collapsed_to_one_line() {
        let mut info = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        info.prompt = "fix  the\nlogin   bug".into();
        let entry = entry_from_info(&info);
        assert_eq!(entry.prompt, "fix the login bug");
    }

    #[test]
    fn entry_haystack_spans_the_searchable_fields() {
        let mut info = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        info.tmux_session = "sess".into();
        info.window_name = "win".into();
        info.worktree = WorktreeMetadata {
            name: "wt".into(),
            branch: "feat/wt".into(),
        };
        info.session_name = "title".into();
        info.prompt = "do the thing".into();
        let entry = entry_from_info(&info);
        let haystack: String = entry.haystack.iter().collect();
        assert_eq!(
            haystack,
            "claude project (wt) title wt feat/wt win sess do the thing"
        );
        // The pane id is deliberately absent: jumping needs it, matching
        // does not (a typed "%1" must not summon pane %1).
        assert!(!haystack.contains("%1"));
    }

    // ─── keys ────────────────────────────────────────────────────────

    #[test]
    fn enter_jumps_to_the_selected_pane() {
        let mut state = state(vec![entry("%1"), entry("%2")]);
        state.move_selection(1);
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter)),
            KeyOutcome::Jump("%2".into())
        );
    }

    #[test]
    fn enter_without_results_is_inert() {
        let mut state = state(vec![entry("%1")]);
        state.query.set("zzzz");
        state.query_changed();
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter)),
            KeyOutcome::Continue
        );
    }

    #[test]
    fn esc_and_ctrl_c_quit_without_jumping() {
        let mut state = state(vec![entry("%1")]);
        assert_eq!(handle_key(&mut state, key(KeyCode::Esc)), KeyOutcome::Quit);
        assert_eq!(
            handle_key(&mut state, ctrl(KeyCode::Char('c'))),
            KeyOutcome::Quit
        );
    }

    #[test]
    fn typing_lands_in_the_query_and_resets_the_selection() {
        let mut state = state(vec![entry("%1"), entry("%2")]);
        state.move_selection(1);
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('a'))),
            KeyOutcome::Continue
        );
        assert_eq!(state.query.content(), "a");
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn arrow_keys_move_and_clamp() {
        let mut state = state(vec![entry("%1"), entry("%2")]);
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.selected, 1);
        // Off the end: the neutral state, not a wraparound jump.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.selected, 1);
        handle_key(&mut state, key(KeyCode::Up));
        handle_key(&mut state, key(KeyCode::Up));
        assert_eq!(state.selected, 0);
        // Ctrl-N / Ctrl-P mirror the arrows.
        handle_key(&mut state, ctrl(KeyCode::Char('n')));
        assert_eq!(state.selected, 1);
        handle_key(&mut state, ctrl(KeyCode::Char('p')));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn ctrl_u_clears_and_ctrl_w_kills_a_word() {
        let mut state = state(vec![entry("%1")]);
        for c in "one two".chars() {
            state.query.push(c);
        }
        state.query_changed();
        handle_key(&mut state, ctrl(KeyCode::Char('w')));
        assert_eq!(state.query.content(), "one ");
        handle_key(&mut state, ctrl(KeyCode::Char('u')));
        assert_eq!(state.query.content(), "");
    }

    #[test]
    fn shift_modified_chars_still_type() {
        let mut state = state(vec![entry("%1")]);
        let key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        handle_key(&mut state, key);
        assert_eq!(state.query.content(), "A");
    }

    // ─── refresh ─────────────────────────────────────────────────────

    #[test]
    fn refresh_keeps_the_selection_on_the_same_pane() {
        let mut state = state(vec![entry("%1"), entry("%2"), entry("%3")]);
        state.move_selection(2);
        // The refresh reorders and drops a pane; the selection follows
        // its pane id, not its index.
        state.refresh(vec![entry("%3"), entry("%1")]);
        assert_eq!(state.selected_pane_id(), Some("%3"));
    }

    #[test]
    fn refresh_after_a_vanished_selection_falls_back_to_default() {
        let mut state = state(vec![entry("%1"), entry("%2")]);
        state.move_selection(1);
        // The selected pane dies: the selection lands on the alt-tab
        // default (first non-current; no current known → the top).
        state.refresh(vec![entry("%1")]);
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_pane_id(), Some("%1"));
    }

    // ─── rendering (snapshots) ───────────────────────────────────────

    #[test]
    fn snapshot_populated_list_with_selection() {
        let mut claude = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        claude.session_name = "fix the switcher".into();
        claude.prompt = "add a quick switcher".into();
        let mut codex = make_info(AgentType::Codex, PaneStatus::Idle, "%2");
        codex.path = "/home/user/other-repo".into();
        codex.worktree = WorktreeMetadata {
            name: "wt".into(),
            branch: "feat/wt".into(),
        };
        codex.session_name = "build".into();
        let state = state(vec![entry_from_info(&claude), entry_from_info(&codex)]);
        assert_snapshot!(render_frame(&state, 80, 10), @"
        ┌ switch ──────────────────────────────────────────────────────────────────────┐
        │>                                                                             │
        │●  claude  project  fix the switcher  add a quick switcher                    │
        │○  codex  other-repo (wt)  build                                              │
        │                                                                              │
        │                                                                              │
        │                                                                              │
        │                                                                              │
        │↑↓ select · enter switch · esc close                                          │
        └──────────────────────────────────────────────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_query_narrows_and_marks_the_match() {
        let mut claude = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        claude.session_name = "fix the switcher".into();
        let mut codex = make_info(AgentType::Codex, PaneStatus::Idle, "%2");
        codex.session_name = "build".into();
        let mut state = state(vec![entry_from_info(&claude), entry_from_info(&codex)]);
        state.query.set("swi");
        state.query_changed();
        assert_snapshot!(render_frame(&state, 80, 10), @"
        ┌ switch ──────────────────────────────────────────────────────────────────────┐
        │> swi                                                                         │
        │●  claude  project  fix the switcher                                          │
        │                                                                              │
        │                                                                              │
        │                                                                              │
        │                                                                              │
        │                                                                              │
        │↑↓ select · enter switch · esc close                                          │
        └──────────────────────────────────────────────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_no_match_message() {
        let mut state = state(vec![entry("%1")]);
        state.query.set("zzzz");
        state.query_changed();
        assert_snapshot!(render_frame(&state, 40, 6), @"
        ┌ switch ──────────────────────────────┐
        │> zzzz                                │
        │no match                              │
        │                                      │
        │↑↓ select · enter switch · esc close  │
        └──────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_empty_state() {
        let state = state(Vec::new());
        assert_snapshot!(render_frame(&state, 40, 6), @"
        ┌ switch ──────────────────────────────┐
        │>                                     │
        │no agent panes                        │
        │                                      │
        │↑↓ select · enter switch · esc close  │
        └──────────────────────────────────────┘
        ");
    }

    #[test]
    fn snapshot_long_row_truncates_with_an_ellipsis() {
        let mut claude = make_info(AgentType::Claude, PaneStatus::Running, "%1");
        claude.session_name = "a very long session title that keeps going".into();
        claude.prompt = "and a long prompt after it too".into();
        let state = state(vec![entry_from_info(&claude)]);
        assert_snapshot!(render_frame(&state, 60, 6), @"
        ┌ switch ──────────────────────────────────────────────────┐
        │>                                                         │
        │●  claude  project  a very long session title that keeps… │
        │                                                          │
        │↑↓ select · enter switch · esc close                      │
        └──────────────────────────────────────────────────────────┘
        ");
    }

    // ─── field fitting ───────────────────────────────────────────────

    #[test]
    fn fit_field_trims_before_the_ellipsis() {
        // The cut would otherwise end in the second space: "ab  …".
        assert_eq!(fit_field("ab  cd", 4), "ab…");
        // Fitting text passes through untouched.
        assert_eq!(fit_field("short", 10), "short");
        // Zero room is empty, never a lone ellipsis.
        assert_eq!(fit_field("abc", 0), "");
        // A wide char that cannot fit is dropped entirely.
        assert_eq!(fit_field("漢字", 1), "…");
    }

    // ─── arg parsing ─────────────────────────────────────────────────

    #[test]
    fn popup_client_requires_a_value() {
        assert_eq!(cmd_switch(&["--popup-client".into()]), 2);
        // A following flag is not a client name (audit L-cli-4 shape).
        assert_eq!(cmd_switch(&["--popup-client".into(), "--popup".into()]), 2);
    }

    #[test]
    fn unknown_argument_exits_2() {
        assert_eq!(cmd_switch(&["--bogus".into()]), 2);
    }
}
