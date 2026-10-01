use std::collections::HashMap;

/// Session-name map scanned by a background thread so the TUI thread never
/// blocks on `~/.claude/sessions/*.json` reads.
///
/// The map is re-applied to every pane on every tick rather than gated by a
/// change flag: `apply_session_snapshot` rebuilds `repo_groups` with an empty
/// `session_name` on each fresh tmux parse, so `refresh` must restore labels
/// every pass whether or not the map itself changed. The walk is one HashMap
/// lookup per pane; the filesystem scan lives in the polling thread.
#[derive(Debug, Clone, Default)]
pub struct SessionNamesState {
    pub names: HashMap<String, String>,
}
