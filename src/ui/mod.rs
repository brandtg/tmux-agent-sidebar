pub mod colors;
pub mod help;
pub mod icons;
pub mod notices;
pub mod panes;
pub mod text;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use crate::state::AppState;

// ── public entry point ──────────────────────────────────────────────

pub fn draw(frame: &mut Frame, state: &mut AppState) {
    state.layout.hyperlink_overlays.clear();
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Min(1)])
        .split(area);

    panes::draw_agents(frame, state, chunks[0]);
}
