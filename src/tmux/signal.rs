//! Cross-instance refresh signaling.
//!
//! Sidebar settings that must stay identical across every open instance
//! (compact density, status filter, repo filter) live in tmux global
//! options. A sidebar that changes one of them broadcasts SIGUSR1 to
//! every other live sidebar process so the change lands immediately —
//! the receiving side reloads the global options when the signal's
//! refresh tick runs (see `app::run`).
//!
//! Every pid is identity-checked before signaling, exactly like the
//! focus hook: `@sidebar_pid` outlives the process that wrote it, and a
//! recycled pid would otherwise receive SIGUSR1 — whose default
//! disposition is terminate. The check-then-kill gap is closed on Linux
//! by pinning the target with a pidfd before the identity check (see
//! [`signal_usr1`]); elsewhere the exe is re-verified immediately before
//! the kill.

use std::time::Duration;

/// Exe basename for the identity check on Linux (`/proc/<pid>/exe`).
const EXE_STEM: &str = "tmux-agent-sidebar";
/// Process-name stem for the fallback check. `ps -o comm` truncates at
/// 15 characters on both Linux and macOS, so the full binary name never
/// fits — match the longest unambiguous prefix instead.
const COMM_STEM: &str = "tmux-agent-side";

/// Deadline for the fallback `ps` probe; the /proc path above is a pure
/// readlink, but on platforms without /proc (or for dead pids) this spawn
/// must never hang the caller — `broadcast_refresh` runs on the UI thread
/// when a shared-option toggle is pressed (audit 2026-11-16 J3: the bare
/// `.output()` had no timeout at all).
const PS_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether `pid` still refers to a running sidebar binary.
///
/// On Linux, `/proc/<pid>/exe` is authoritative (a recycled pid points
/// at the new owner's binary). Platforms without /proc (macOS) fall
/// back to the process name reported by `ps`.
pub(crate) fn is_sidebar_process(pid: u32) -> bool {
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return exe
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with(EXE_STEM));
    }
    let mut command = std::process::Command::new("ps");
    command.args(["-p", &pid.to_string(), "-o", "comm="]);
    let Ok(output) = crate::subprocess::run_with_timeout(&mut command, PS_PROBE_TIMEOUT) else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let comm = String::from_utf8_lossy(&output.stdout);
    let name = comm.trim().rsplit('/').next().unwrap_or("");
    name.starts_with(COMM_STEM)
}

/// Send SIGUSR1 to a pid that the caller has vetted with
/// [`is_sidebar_process`].
///
/// That vetting is a point-in-time answer, and the pid can be recycled
/// between the check and the kill — SIGUSR1's default disposition is
/// terminate, so the new owner would die (audit 2026-11-26 L-tmux-2).
/// On Linux the check and the kill are fused: `pidfd_open` pins the
/// signal target first, the exe is verified while the pin holds, and
/// the signal goes to the fd — a recycled pid can never receive it. On
/// pre-5.3 kernels (ENOSYS) and off-Linux, the exe is re-verified
/// immediately before the kill, shrinking the race window to the
/// smallest the platform offers.
pub(crate) fn signal_usr1(pid: u32) {
    #[cfg(target_os = "linux")]
    if signal_usr1_pidfd(pid) {
        return;
    }
    if is_sidebar_process(pid) {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGUSR1);
        }
    }
}

/// Pin `pid` with a pidfd and send SIGUSR1 to the pinned process.
///
/// Ordering is load-bearing: the fd must be opened *before* the exe is
/// verified. The pidfd forever refers to the process alive under that
/// pid at open time, so whatever the exe check reads afterwards, the
/// signal can only ever hit the pinned process — a pid that died after
/// pinning makes the send fail with ESRCH (harmless), and a pid
/// recycled after pinning is rejected by the exe check (or, if the
/// recycler is itself a sidebar, hit by a benign refresh).
///
/// Returns `false` when the pidfd path is unavailable (ENOSYS on a
/// pre-5.3 kernel) or the process is gone / no longer a sidebar, so the
/// caller can fall back to re-verify + kill.
#[cfg(target_os = "linux")]
fn signal_usr1_pidfd(pid: u32) -> bool {
    unsafe {
        let fd = libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint);
        if fd < 0 {
            return false;
        }
        let fd = fd as libc::c_int;
        let sent = if is_sidebar_process(pid) {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd,
                libc::SIGUSR1,
                std::ptr::null::<libc::c_void>(),
                0 as libc::c_uint,
            ) == 0
        } else {
            // The pinned process is no longer a sidebar (exec'd away, or
            // the exe check now resolves to a recycled pid's owner): do
            // not signal.
            false
        };
        libc::close(fd);
        sent
    }
}

/// Send SIGUSR1 to every live sidebar process except the one anchored
/// at `except_pane` (the broadcaster itself). Popups participate too:
/// their pid is published on the anchor pane's `@sidebar_pid`. A failed
/// `list-panes` (no server, tmux busy) silently skips the broadcast —
/// receivers still converge via the focus-change reload.
pub(crate) fn broadcast_refresh(except_pane: &str) {
    let Some(output) =
        crate::tmux::run_tmux(&["list-panes", "-a", "-F", "#{pane_id}|#{@sidebar_pid}"])
    else {
        return;
    };
    for line in output.lines() {
        let Some((pane, pid)) = line.split_once('|') else {
            continue;
        };
        if pane == except_pane {
            continue;
        }
        let Ok(pid) = pid.trim().parse::<u32>() else {
            continue;
        };
        if is_sidebar_process(pid) {
            signal_usr1(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_1_is_not_the_sidebar() {
        // init / systemd / launchd — stable across platforms, and never
        // the sidebar binary. Exercises both the /proc path (Linux) and
        // the ps fallback (macOS) depending on the host.
        assert!(!is_sidebar_process(1));
    }

    #[test]
    fn nonexistent_pid_is_not_the_sidebar() {
        // Above every plausible pid_max; /proc/<pid>/exe cannot exist and
        // `ps` reports no such process.
        assert!(!is_sidebar_process(u32::MAX - 2));
    }

    #[test]
    fn signaling_a_nonexistent_pid_is_a_noop() {
        // pidfd_open fails with ESRCH (or ENOSYS), the fallback re-verify
        // fails, and no signal leaves the process. Must not panic.
        signal_usr1(u32::MAX - 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pidfd_path_reports_unavailable_for_a_dead_pid() {
        // No such process: pidfd_open fails and the caller falls back to
        // re-verify + kill. The pinned-fd contract is exercised by
        // signaling a dead pid rather than a real one — no live process
        // is ever touched from a test.
        assert!(!signal_usr1_pidfd(u32::MAX - 2));
    }
}
