//! Spawn / remove flow for the sidebar `n` / `x` keybindings. Owns the
//! handful of writes (git worktree, tmux new-window) that turn this
//! otherwise read-only sidebar into an agent multiplexer: the spawn can
//! drop an agent straight into the repo folder or carve out a dedicated
//! worktree first.

mod config;
mod env;
mod flow;
mod markers;
mod slug;

pub use config::{
    AGENT_OPTION, AGENT_PANE_PERCENT, AGENTS, BRANCH_PREFIX_OPTION, DEFAULT_AGENT,
    DEFAULT_BRANCH_PREFIX, DEFAULT_WORKTREE_DIR, RemoveMode, SPAWN_MODES, WORKTREE_DIR_OPTION,
    agent_command,
};
pub use flow::{SpawnKind, SpawnRequest, removal_warnings, remove, spawn};
pub use markers::{
    SPAWNED_BRANCH_OPTION, SPAWNED_FROM_OPTION, SPAWNED_OPTION, SPAWNED_WORKTREE_OPTION,
    SpawnMarkers, read_spawn_markers, spawn_markers_template,
};
pub use slug::{pick_unique_slug, slugify, worktree_path_for};
