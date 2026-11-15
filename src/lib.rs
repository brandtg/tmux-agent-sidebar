pub mod activity;
pub mod adapter;
pub mod app;
pub mod cli;
pub(crate) mod debug;
pub mod desktop_notification;
pub mod event;
pub mod git;
pub mod group;
pub(crate) mod paths;
pub mod port;
pub(crate) mod process;
pub mod session;
pub mod state;
pub(crate) mod subprocess;
pub mod time;
pub mod tmux;
pub mod todo;
pub mod tool_name;
pub mod ui;
pub mod version;
pub mod worktree;

pub const SPINNER_ICON: &str = "●";
pub const SPINNER_PULSE: &[u8] = &[82, 78, 114, 150, 186, 150, 114, 78];
/// Breathing green cycle for the "done and unseen" indicator: the idle
/// glyph fades between near-black and the running green on the 200ms
/// spinner tick until the user focuses the pane.
pub const DONE_PULSE: &[u8] = &[22, 28, 34, 114, 34, 28];
/// Breathing magenta cycle for working process triangles. Same pulse
/// personality and luminance range as [`SPINNER_PULSE`] (whose darkest
/// frame is still a bright green) — the ladder stays in the hot-to-pale
/// pinks and never drops toward dark orchid, which read as a flicker
/// rather than a breathe.
pub const PROCESS_PULSE: &[u8] = &[213, 207, 212, 218, 219, 218, 212, 207];
/// One-shot decay ramps for the chyron row: when an agent event fires
/// (turn finished / waiting on the user / failed), the row's foreground
/// starts at the ramp's brightest step and walks down toward the muted
/// rest color over the fade frames (see `state::chyron`). Unlike the
/// breathe ramps above, these are indexed once per fade frame and then
/// stop — a notification is transient, not a status. Same hot-to-pale
/// personality as [`SPINNER_PULSE`], per hue.
pub const CHYRON_DONE: &[u8] = &[157, 114, 78, 71, 65];
pub const CHYRON_WAITING: &[u8] = &[215, 179, 143, 137, 130];
pub const CHYRON_FAILED: &[u8] = &[210, 167, 131, 95, 88];
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
