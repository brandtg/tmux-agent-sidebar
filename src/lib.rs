pub mod activity;
pub mod adapter;
pub mod app;
pub mod cli;
pub mod desktop_notification;
pub mod event;
pub mod git;
pub mod group;
pub mod port;
pub(crate) mod process;
pub mod session;
pub mod state;
pub(crate) mod subprocess;
pub mod time;
pub mod tmux;
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
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
