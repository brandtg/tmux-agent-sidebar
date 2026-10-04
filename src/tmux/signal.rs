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
//! disposition is terminate.

/// Exe basename for the identity check on Linux (`/proc/<pid>/exe`).
const EXE_STEM: &str = "tmux-agent-sidebar";
/// Process-name stem for the fallback check. `ps -o comm` truncates at
/// 15 characters on both Linux and macOS, so the full binary name never
/// fits — match the longest unambiguous prefix instead.
const COMM_STEM: &str = "tmux-agent-side";

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
    let Ok(output) = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let comm = String::from_utf8_lossy(&output.stdout);
    let name = comm.trim().rsplit('/').next().unwrap_or("");
    name.starts_with(COMM_STEM)
}

pub(crate) fn signal_usr1(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGUSR1);
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
}
