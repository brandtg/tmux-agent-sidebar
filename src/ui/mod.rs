pub mod colors;
pub mod help;
pub mod icons;
pub mod notices;
pub mod panes;
pub mod text;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
};

use crate::state::AppState;
use colors::ColorTheme;

// ── public entry point ──────────────────────────────────────────────

/// Paint the opt-in custom sidebar background over `area`.
///
/// A no-op when the theme carries no background — the default, where
/// cells keep the terminal's own background and transparent terminals
/// stay transparent. With `@sidebar_color_background` set, the color is
/// stamped onto the buffer at the start of each frame so every later
/// render patches over it (Ratatui styles only override the fields they
/// set), and again after every `Clear` widget, which resets cells back
/// to the default background — without the re-application, popups would
/// punch terminal-default holes in a custom background.
pub fn paint_background(frame: &mut Frame, theme: &ColorTheme, area: Rect) {
    if let Some(bg) = theme.background {
        frame.buffer_mut().set_style(area, Style::default().bg(bg));
    }
}

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Min(1)])
        .split(area);

    paint_background(frame, &state.theme, area);
    panes::draw_agents(frame, state, chunks[0]);
}
