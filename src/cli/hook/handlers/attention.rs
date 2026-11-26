use crate::cli::{set_attention, set_status};
use crate::desktop_notification;
use crate::desktop_notification::DesktopNotificationKind;
use crate::tmux;

use super::super::activity::write_activity_entry;
use super::super::context::{AgentContext, set_agent_meta};
use super::super::notifications::{
    NotifyLabels, NotifyPayload, notification_body, notification_fingerprint, notify_lifecycle,
};
use super::status_priority::resolve_notification_status;

pub(in crate::cli::hook) fn on_notification(
    pane: &str,
    ctx: &AgentContext<'_>,
    wait_reason: &str,
    meta_only: bool,
    notifications: Option<desktop_notification::DesktopNotificationSettings>,
) -> i32 {
    set_agent_meta(pane, ctx);
    if meta_only {
        return 0;
    }
    // Claude's `agent_completed` notification fires when the foreground
    // CLI hands the session to a background continuation (`continued-in`
    // in the transcript): the turn's `Stop` already resolved the status,
    // and nothing is waiting on the user. Forcing `waiting` here glued
    // the pane in `waiting`/`agent_completed` for the whole continuation
    // (observed live 2026-11-20): the continuation's own hooks never
    // reach the pane (the daemon's bg-pty-host scrubs TMUX_PANE), so no
    // later event ever cleared it. Keep the Stop-resolved status, drop
    // any stale wait reason, and skip both the Feed entry and the
    // desktop notification — a completion is not a "needs you" event.
    if wait_reason == "agent_completed" {
        tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
        return 0;
    }
    let bg_shell_live = !tmux::get_pane_option_value(pane, tmux::PANE_BG_CMD).is_empty();
    set_status(
        pane,
        resolve_notification_status(wait_reason, bg_shell_live),
    );
    set_attention(pane, "notification");
    if wait_reason.is_empty() {
        tmux::unset_pane_option(pane, tmux::PANE_WAIT_REASON);
    } else {
        tmux::set_pane_option(pane, tmux::PANE_WAIT_REASON, wait_reason);
        // Feed visibility: a notification (permission prompt, question,
        // waiting for input) is exactly the "something needs you" class
        // the Feed tab badges for. `Notify` is a sidebar pseudo-tool —
        // see `FeedKind::classify`.
        write_activity_entry(pane, "Notify", wait_reason);
    }
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromCtx(ctx),
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::PermissionRequired,
            event: desktop_notification::DesktopNotificationEvent::Notification,
            fingerprint_suffix: notification_fingerprint(wait_reason),
            body: &notification_body(wait_reason),
        },
    );
    0
}

pub(in crate::cli::hook) fn on_permission_denied(
    pane: &str,
    ctx: &AgentContext<'_>,
    notifications: Option<desktop_notification::DesktopNotificationSettings>,
) -> i32 {
    set_agent_meta(pane, ctx);
    set_status(pane, "waiting");
    set_attention(pane, "notification");
    tmux::set_pane_option(pane, tmux::PANE_WAIT_REASON, "permission_denied");
    write_activity_entry(pane, "Permission", "permission denied");
    let _ = notify_lifecycle(
        pane,
        NotifyLabels::FromCtx(ctx),
        notifications,
        None,
        NotifyPayload {
            kind: DesktopNotificationKind::PermissionRequired,
            event: desktop_notification::DesktopNotificationEvent::PermissionDenied,
            fingerprint_suffix: "permission_denied",
            body: "Permission required",
        },
    );
    0
}

pub(in crate::cli::hook) fn on_teammate_idle(
    pane: &str,
    teammate_name: &str,
    idle_reason: &str,
) -> i32 {
    set_attention(pane, "notification");
    let reason = if idle_reason.is_empty() {
        format!("teammate_idle:{teammate_name}")
    } else {
        format!("teammate_idle:{teammate_name}:{idle_reason}")
    };
    tmux::set_pane_option(pane, tmux::PANE_WAIT_REASON, &reason);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_teammate_idle_sets_attention_and_reason() {
        let _guard = tmux::test_mock::install();
        let pane = "%TEAM";
        let exit = on_teammate_idle(pane, "alice", "");
        assert_eq!(exit, 0);
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_ATTENTION).as_deref(),
            Some("notification")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("teammate_idle:alice")
        );
    }

    #[test]
    fn on_teammate_idle_includes_idle_reason_when_present() {
        let _guard = tmux::test_mock::install();
        let pane = "%TEAM_REASON";
        on_teammate_idle(pane, "alice", "tokens_exhausted");
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("teammate_idle:alice:tokens_exhausted")
        );
    }

    #[test]
    fn on_notification_meta_only_skips_status_and_attention() {
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_META";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "permission",
            /* meta_only */ true,
            Some(notifications),
        );
        // meta_only=true must short-circuit before status/attention/wait_reason writes.
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_STATUS));
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_ATTENTION));
        assert!(!tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON));
        // Agent meta should still be applied so the sidebar can render the pane.
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_AGENT).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn on_notification_sets_waiting_status_and_reason() {
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_WAIT";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "permission",
            /* meta_only */ false,
            Some(notifications),
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_ATTENTION).as_deref(),
            Some("notification")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission")
        );
    }

    #[test]
    fn on_notification_keeps_background_when_softer_reason_and_bg_shell_live() {
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_BG_PREEMPT";
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "cargo test");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "auth_success",
            /* meta_only */ false,
            Some(notifications),
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("background"),
        );
    }

    #[test]
    fn on_notification_permission_reason_preempts_background() {
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_PERM_OVER_BG";
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "cargo test");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "permission_prompt",
            /* meta_only */ false,
            Some(notifications),
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
        );
    }

    #[test]
    fn on_notification_plain_permission_preempts_background() {
        // Claude's real `notification_type: "permission"` payload must
        // stay in `waiting` even with a live bg shell — the user has to
        // act on the prompt regardless.
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_PERM_PLAIN_OVER_BG";
        tmux::test_mock::set(pane, tmux::PANE_BG_CMD, "cargo test");
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "permission",
            /* meta_only */ false,
            Some(notifications),
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
        );
    }

    #[test]
    fn on_notification_soft_reason_without_bg_still_sets_waiting() {
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_SOFT_NO_BG";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "auth_success",
            /* meta_only */ false,
            Some(notifications),
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting"),
        );
    }

    #[test]
    fn on_notification_empty_wait_reason_clears_stale_value() {
        // Regression: an empty wait_reason used to be a no-op, which
        // left the previously-written reason on the pane. A later
        // notification that genuinely has no reason must drop the
        // stale one so the sidebar does not keep rendering the wrong
        // cause.
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_STALE";
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "permission");

        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_notification(
            pane,
            &ctx,
            "",
            /* meta_only */ false,
            Some(notifications),
        );

        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON),
            "empty wait_reason must clear a prior value"
        );
    }

    #[test]
    fn on_notification_agent_completed_is_not_a_wait_state() {
        // Regression: Claude fires `notification_type: "agent_completed"`
        // when the foreground CLI hands the session to a background
        // continuation. The turn's Stop already resolved the status, but
        // this notification used to force `waiting` + a wait reason —
        // and since the continuation's hooks never reach the pane
        // (bg-pty-host scrubs TMUX_PANE), nothing ever cleared it. The
        // pane must keep its Stop-resolved status, clear any stale wait
        // reason, and leave the done attention flag alone.
        let _guard = tmux::test_mock::install();
        let pane = "%NOTIF_COMPLETED";
        tmux::test_mock::set(pane, tmux::PANE_STATUS, "idle");
        tmux::test_mock::set(pane, tmux::PANE_ATTENTION, "done");
        tmux::test_mock::set(pane, tmux::PANE_WAIT_REASON, "auth_success");

        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "auto",
            worktree: &None,
            session_id: &None,
        };
        // Notifications enabled with the Notification event: if the
        // completion notice ever reaches the desktop-notification gate,
        // the stamp is written on a dev machine with a notifier — so a
        // missing stamp proves the gate was skipped, not that sending
        // failed.
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: true,
            events: [desktop_notification::DesktopNotificationEvent::Notification]
                .into_iter()
                .collect(),
        };
        on_notification(
            pane,
            &ctx,
            "agent_completed",
            /* meta_only */ false,
            Some(notifications),
        );

        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("idle"),
            "a completion notice must not move the pane into waiting"
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_ATTENTION).as_deref(),
            Some("done"),
            "the Stop-set done flag must survive the completion notice"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_WAIT_REASON),
            "the completion notice must not write a wait reason"
        );
        assert!(
            !tmux::test_mock::contains(pane, tmux::PANE_OS_NOTIFY_PERMISSION_REQUIRED),
            "a completion notice must not fire a permission-required notification"
        );
    }

    #[test]
    fn on_permission_denied_records_permission_denied_wait_reason() {
        let _guard = tmux::test_mock::install();
        let pane = "%PD";
        let ctx = AgentContext {
            agent: "claude",
            cwd: "/repo",
            permission_mode: "default",
            worktree: &None,
            session_id: &None,
        };
        let notifications = desktop_notification::DesktopNotificationSettings {
            enabled: false,
            events: Default::default(),
        };
        on_permission_denied(pane, &ctx, Some(notifications));
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_WAIT_REASON).as_deref(),
            Some("permission_denied")
        );
        assert_eq!(
            tmux::test_mock::get(pane, tmux::PANE_STATUS).as_deref(),
            Some("waiting")
        );
    }
}
