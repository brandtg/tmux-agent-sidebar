pub mod activity;
pub mod adapter;
pub mod app;
pub mod cli;
pub(crate) mod clipboard;
pub(crate) mod debug;
pub mod desktop_notification;
pub mod event;
pub mod git;
pub mod group;
pub mod line_edit;
pub mod park;
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
/// One-shot ramps for the chyron row: when an agent event fires (turn
/// finished / waiting on the user / failed), the row's foreground reads
/// the ramp by phase (see `state::chyron`). Step 0 is the flash
/// foreground — brightest, paired with the dark background tint; step 1
/// is the linger foreground, plain colored text on the default
/// background in the same hue family as the row status colors; the
/// remaining steps are the fade-out. Same hot-to-pale personality as
/// [`SPINNER_PULSE`], per hue.
pub const CHYRON_DONE: &[u8] = &[157, 114, 78, 71, 65, 59];
pub const CHYRON_WAITING: &[u8] = &[215, 179, 143, 137, 130, 124];
pub const CHYRON_FAILED: &[u8] = &[210, 167, 131, 95, 88, 80];
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
