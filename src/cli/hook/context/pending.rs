use super::meta::clear_all_meta;
use crate::cli::{set_attention, set_status};
use crate::tmux;

/// Legacy pane-option name. Nothing sets this any more — a child
/// SessionEnd can't be distinguished from the parent's, so the
/// deferred-teardown dance was too dangerous to keep. The constant is
/// retained so `clear_all_meta` and `on_session_start` can still sweep
/// a stale marker left behind by a pre-fix install.
pub(in crate::cli::hook) const PENDING_SESSION_END: &str = tmux::PANE_PENDING_SESSION_END;

/// Side-effect body of the SessionEnd teardown. Invoked by
/// `on_session_end` when no subagents are active; subagent-active
/// SessionEnds are short-circuited before they reach this point.
pub(in crate::cli::hook) fn run_session_end_teardown(pane: &str) {
    set_attention(pane, "clear");
    clear_all_meta(pane);
    set_status(pane, "clear");
    let log_path = crate::activity::log_file_path(pane);
    let _ = std::fs::remove_file(log_path);
}
