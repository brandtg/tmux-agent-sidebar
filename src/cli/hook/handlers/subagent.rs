use crate::tmux;

use super::super::context::remove_subagent;

/// Run `f` while holding the pane's subagent lock. Fail-open (see
/// `paths::with_pane_lock`): if the lockfile cannot be opened or
/// flocked, proceed unlocked — a skipped write would strand the
/// subagent guard, while an unlocked write is only the pre-fix race.
///
/// Held by both subagent-list writers (`on_subagent_start` /
/// `on_subagent_stop`), by `on_stop`'s stale-list clear, by the
/// session-binding read-modify-write in `sync_pane_location` and by
/// `on_session_title`'s check+write, so identity writers on one pane
/// cannot interleave their read-modify-writes across independent hook
/// processes (audit 2026-11-16: unguarded session-title get-check-set).
pub(in crate::cli::hook) fn with_subagent_lock<T>(pane: &str, f: impl FnOnce() -> T) -> T {
    crate::paths::with_pane_lock("subagent", pane, f)
}

pub(in crate::cli::hook) fn on_subagent_start(
    pane: &str,
    agent_type: &str,
    agent_id: Option<&str>,
) -> i32 {
    // Claude Code always sends agent_id per the hooks spec; drop the
    // event silently if it's missing so the tree never gains an
    // untrackable entry.
    let Some(id) = agent_id.filter(|s| !s.is_empty()) else {
        return 0;
    };
    // Atomic server-side append (`set -a`). Hook events are independent
    // processes sharing this pane option, and the old get→append→set
    // round trip lost an entry whenever two SubagentStart hooks ran
    // concurrently — leaving either a missing row or a subagent guard
    // stuck open/closed. The chunk carries its own joining comma; on an
    // unset option tmux stores it verbatim (leading comma), which every
    // reader tolerates by filtering empty entries.
    //
    // The append alone cannot interleave with `on_subagent_stop`'s
    // read-modify-write: a start landing between a stop's read and its
    // write would be clobbered by the stale rewrite. Both sides take
    // the same per-pane flock; only appends-vs-appends rely on `set -a`.
    let entry = tmux::sanitize_option_value(&format!("{agent_type}:{id}"));
    with_subagent_lock(pane, || {
        tmux::append_pane_option(pane, tmux::PANE_SUBAGENTS, &format!(",{entry}"));
    });
    0
}

pub(in crate::cli::hook) fn on_subagent_stop(pane: &str, agent_id: Option<&str>) -> i32 {
    let Some(id) = agent_id.filter(|s| !s.is_empty()) else {
        return 0;
    };
    // The removal is a get → rewrite → set round trip across
    // independent hook processes; concurrent stops lost updates (9/25
    // rounds with 6 concurrent hooks), stranding `pane_writes_allowed`
    // and blocking the parent's SessionEnd. Serialize on the same
    // per-pane flock the start path holds.
    with_subagent_lock(pane, || {
        let current = tmux::get_pane_option_value(pane, tmux::PANE_SUBAGENTS);
        match remove_subagent(&current, id) {
            None => {}
            Some(new_val) if new_val.is_empty() => {
                tmux::unset_pane_option(pane, tmux::PANE_SUBAGENTS);
                // The last async child finished. If the parent's Stop
                // resolved to `background` because of these children
                // (`on_stop` keeps the list for Hermes), nothing else
                // will drop the pane — do it here, but only when no bg
                // shell keeps `background` alive on its own. Direct
                // option write, mirroring the sweep's downgrade so the
                // `done` attention flag set at Stop survives.
                if tmux::get_pane_option_value(pane, tmux::PANE_BG_CMD).is_empty()
                    && tmux::get_pane_option_value(pane, tmux::PANE_STATUS) == "background"
                {
                    tmux::set_pane_option(pane, tmux::PANE_STATUS, "idle");
                }
            }
            Some(new_val) => {
                tmux::set_pane_option(pane, tmux::PANE_SUBAGENTS, &new_val);
            }
        };
    });
    0
}

#[cfg(test)]
mod tests {
    use super::super::run::on_stop;
    use super::super::session::on_session_end;
    use super::*;
    use crate::cli::hook::activity::lock_exclusive;
    use crate::cli::hook::context::PENDING_SESSION_END;
    use crate::desktop_notification;
    use std::fs;

    fn default_notifications() -> desktop_notification::DesktopNotificationSettings {
        desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        }
    }

    #[test]
    fn on_subagent_start_appends_atomically() {
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_START";
        on_subagent_start(pane, "Explore", Some("sub-1"));
        // The chunk carries its own comma; on an unset option tmux stores
        // it verbatim, so the value starts with a leading comma. Readers
        // filter the empty entry.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SUBAGENTS).as_deref(),
            Some(",Explore:sub-1")
        );
        on_subagent_start(pane, "Plan", Some("sub-2"));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SUBAGENTS).as_deref(),
            Some(",Explore:sub-1,Plan:sub-2")
        );
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }

    #[test]
    fn subagent_lock_excludes_concurrent_holders() {
        // The stop path is a read-modify-write across independent hook
        // processes; the flock is what keeps concurrent rewrites from
        // losing entries. Prove mutual exclusion of the lock itself with
        // real threads: at most one thread may be inside the critical
        // section at any moment, even though every iteration sleeps
        // while holding it.
        let pane = "%SUB_LOCK_RACE";
        let lock = crate::paths::pane_lock_path("subagent", pane);
        let _ = fs::remove_file(&lock);

        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;
        let inside = Arc::new(AtomicUsize::new(0));
        let max_inside = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let inside = inside.clone();
                let max_inside = max_inside.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        with_subagent_lock(pane, || {
                            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                            max_inside.fetch_max(now, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_micros(200));
                            inside.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            max_inside.load(Ordering::SeqCst),
            1,
            "flock must exclude concurrent subagent writers"
        );
        fs::remove_file(&lock).ok();
    }

    #[test]
    fn on_stop_stale_clear_waits_for_the_subagent_lock() {
        // Audit 2026-11-16 #4: the parent Stop's stale-list clear used to
        // read+unset `@pane_subagents` with no flock, so an interleaved
        // SubagentStop's read-modify-write could resurrect the removed
        // children — stranding `pane_writes_allowed` and tripping the
        // SessionEnd subagent guard until the next parent Stop. Hold the
        // lock from a helper thread: on_stop must block until it is
        // released, and only then clear the list.
        //
        // The helper touches nothing but the lockfile — the tmux mock is
        // thread-local, so every pane-option access stays on this thread.
        let _guard = tmux::test_mock::install();
        let pane = "%STOP_LOCKED";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, ",Explore:sub-1,Plan:sub-2");
        let lock = crate::paths::pane_lock_path("subagent", pane);
        let _ = fs::remove_file(&lock);

        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let lock_for_helper = lock.clone();
        let helper = std::thread::spawn(move || {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_for_helper)
                .unwrap();
            assert!(lock_exclusive(&file));
            locked_tx.send(()).unwrap();
            // Hold long enough that an unlocked clear would finish
            // entirely inside this window.
            std::thread::sleep(std::time::Duration::from_millis(300));
            // fd drops here, releasing the flock.
        });

        locked_rx.recv().unwrap();
        let ctx = crate::cli::hook::context::AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let started = std::time::Instant::now();
        let exit = on_stop(pane, &ctx, "", None, &default_notifications());
        let elapsed = started.elapsed();

        assert_eq!(exit, 0);
        assert!(
            elapsed >= std::time::Duration::from_millis(100),
            "the stale-list clear must wait for the subagent lock (took {elapsed:?})"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS),
            "Stop must still clear the stale list once the lock is available"
        );

        helper.join().unwrap();
        fs::remove_file(&lock).ok();
        fs::remove_file(crate::activity::log_file_path(pane)).ok();
    }

    #[test]
    fn on_subagent_stop_cleans_leading_comma_left_by_atomic_append() {
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_COMMA";
        on_subagent_start(pane, "Explore", Some("sub-1"));
        on_subagent_start(pane, "Plan", Some("sub-2"));
        on_subagent_stop(pane, Some("sub-1"));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_SUBAGENTS).as_deref(),
            Some("Plan:sub-2"),
            "stop must rewrite the list without the empty leading entry"
        );
        on_subagent_stop(pane, Some("sub-2"));
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS),
            "drained list must be unset"
        );
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }

    #[test]
    fn on_subagent_start_drops_event_without_id() {
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_NO_ID";
        on_subagent_start(pane, "Explore", None);
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS));
        on_subagent_start(pane, "Explore", Some(""));
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS));
    }

    #[test]
    fn on_subagent_stop_drains_last_async_child_to_idle() {
        // Hermes async delegation: the parent's Stop resolved to
        // `background` while children ran; when the last child stops, the
        // pane must drop back to idle — nothing else will.
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_DRAIN_IDLE";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, ",leaf:child-1");
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "background");

        on_subagent_stop(pane, Some("child-1"));

        assert!(!tmux::test_mock::contains(pane, tmux::PANE_SUBAGENTS));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "the last child finishing must drop the pane out of background"
        );
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }

    #[test]
    fn on_subagent_stop_keeps_background_while_bg_shell_alive() {
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_DRAIN_BG_ALIVE";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, ",leaf:child-1");
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "background");
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "npm run dev");

        on_subagent_stop(pane, Some("child-1"));

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background"),
            "a live bg shell still owns the background status"
        );
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }

    #[test]
    fn on_subagent_stop_leaves_non_background_status_alone() {
        let _guard = tmux::test_mock::install();
        let pane = "%SUB_DRAIN_RUNNING";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, ",leaf:child-1");
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");

        on_subagent_stop(pane, Some("child-1"));

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("running"),
            "the downgrade is scoped to the children-live background state"
        );
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }

    // ─── subagent-guarded SessionEnd regression test ────────────────
    //
    // SessionEnd does NOT participate in any deferred-drain dance: we
    // can't tell a parent SessionEnd from a child's, so the safer
    // default is to skip the event entirely while subagents are active
    // and leave the parent's state alone.

    #[test]
    fn session_end_while_subagents_active_is_a_no_op() {
        // Regression: previously `on_session_end` set PENDING_SESSION_END
        // whenever `@pane_subagents` was non-empty, and the next
        // `on_subagent_stop` would turn that marker into
        // `run_session_end_teardown`. Because subagents share the
        // parent's `$TMUX_PANE`, there is no way to guarantee the
        // SessionEnd came from the parent — so the safer default is to
        // skip the event entirely and leave the parent's state alone.
        let _guard = tmux::test_mock::install();
        let pane = "%CHILD_SESSIONEND";
        tmux::test_mock::set(pane, tmux::PANE_SUBAGENTS, "Explore:sub-1");
        tmux::test_mock::set(pane, tmux::PANE_AGENT, "claude");
        tmux::test_mock::set(pane, tmux::PANE_CWD, "/repo/parent");
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "running");
        let log_path = crate::activity::log_file_path(pane);
        let _ = fs::create_dir_all(log_path.parent().unwrap());
        fs::write(&log_path, "1234567890|Read|main.rs\n").unwrap();

        on_session_end(pane, "claude", "", None, &default_notifications());
        assert!(
            !tmux::test_mock::contains(pane, PENDING_SESSION_END),
            "child SessionEnd must not record a pending teardown"
        );
        // Every parent field must survive.
        assert!(tmux::test_mock::contains(pane, tmux::PANE_AGENT));
        assert!(tmux::test_mock::contains(pane, tmux::PANE_CWD));
        assert!(tmux::test_mock::contains(pane, tmux::PANE_STATUS));
        assert!(log_path.exists());

        // Subsequent subagent stop must not trigger a teardown either.
        on_subagent_stop(pane, Some("sub-1"));
        assert!(
            tmux::test_mock::contains(pane, tmux::PANE_AGENT),
            "SubagentStop draining an empty list must not tear down a live parent"
        );
        assert!(log_path.exists());

        fs::remove_file(&log_path).ok();
        fs::remove_file(crate::paths::pane_lock_path("subagent", pane)).ok();
    }
}
