use std::io::{self, Write as _};

use crossterm::{cursor::MoveTo, execute};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::state::{AppState, HyperlinkOverlay};
use crate::ui;

pub(super) fn render_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> io::Result<()> {
    terminal.draw(|frame| ui::draw(frame, state))?;

    // Write OSC 8 hyperlink overlays after frame render.
    write_hyperlink_overlays(terminal.backend_mut(), &state.layout.hyperlink_overlays)?;

    Ok(())
}

/// Write OSC 8 hyperlink escape sequences over already-rendered PR text.
pub(super) fn write_hyperlink_overlays(
    backend: &mut CrosstermBackend<io::Stdout>,
    overlays: &[HyperlinkOverlay],
) -> io::Result<()> {
    for overlay in overlays {
        execute!(backend, MoveTo(overlay.x, overlay.y))?;
        // OSC 8: open hyperlink
        write!(backend, "\x1b]8;;{}\x1b\\", overlay.url)?;
        // Re-write the text so the terminal associates these cells with the link
        write!(backend, "{}", overlay.text)?;
        // OSC 8: close hyperlink
        write!(backend, "\x1b]8;;\x1b\\")?;
        backend.flush()?;
    }
    Ok(())
}
