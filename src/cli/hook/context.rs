mod labels;
mod location;
mod meta;
mod pending;
mod subagents;

pub(super) use labels::{
    branch_label_from_ctx, branch_label_from_pane, repo_label_from_ctx, repo_label_from_pane,
};
pub(super) use location::{
    pane_writes_allowed, session_bind_allowed, sync_launch_anchor, sync_pane_location,
};
pub(super) use meta::{
    AgentContext, clear_run_state, is_system_message, make_ctx, mark_task_reset, set_agent_meta,
};
pub(super) use pending::{PENDING_SESSION_END, run_session_end_teardown};
pub(super) use subagents::remove_subagent;

#[cfg(test)]
pub(crate) mod test_env {
    //! Per-thread stand-in for the process-global
    //! `TMUX_AGENT_SIDEBAR_BIND_SESSION` flag. std::env is process-global
    //! and cargo runs a crate's tests on parallel threads; tests that flip
    //! the variable would otherwise leak their set/unset window into any
    //! concurrent test whose call path reads it (the flaky
    //! `on_session_start_sets_agent_and_idle_status` session-id failure,
    //! and the earlier flaky `on_session_title_*` failures under load).
    //! Two module-local mutexes were not enough: a test's call path can
    //! read the flag without its module knowing. With the override, each
    //! test thread gets its own value and the real env var is never
    //! mutated by tests.
    use std::cell::Cell;

    thread_local! {
        static BIND_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
    }

    /// Some(allowed): `session_bind_allowed` returns `allowed` regardless
    /// of the env var. None (the default): defer to the real env var —
    /// the production path.
    pub(crate) fn set_bind_override(allowed: Option<bool>) {
        BIND_OVERRIDE.with(|cell| cell.set(allowed));
    }

    pub(crate) fn bind_override() -> Option<bool> {
        BIND_OVERRIDE.with(|cell| cell.get())
    }
}
