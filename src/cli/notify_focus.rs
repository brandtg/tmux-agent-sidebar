use crate::tmux;

pub(crate) fn cmd_notify_focus(args: &[String]) -> i32 {
    let Some(window) = args.first().map(|s| s.as_str()).filter(|s| !s.is_empty()) else {
        return 0;
    };
    let Some(out) = tmux::run_tmux(&["list-panes", "-t", window, "-F", "#{@sidebar_pid}"]) else {
        return 0;
    };
    for line in out.lines() {
        let Ok(pid) = line.trim().parse::<u32>() else {
            continue;
        };
        if is_sidebar_process(pid) {
            signal_usr1(pid);
        }
    }
    0
}

/// Exe basename for the identity check on Linux (`/proc/<pid>/exe`).
const EXE_STEM: &str = "tmux-agent-sidebar";
/// Process-name stem for the fallback check. `ps -o comm` truncates at
/// 15 characters on both Linux and macOS, so the full binary name never
/// fits — match the longest unambiguous prefix instead.
const COMM_STEM: &str = "tmux-agent-side";

/// Whether `pid` still refers to a running sidebar binary.
///
/// `@sidebar_pid` outlives the process that wrote it: if the sidebar
/// crashes and the OS recycles the pid, an unrelated process would
/// receive SIGUSR1 — whose default disposition is terminate. On Linux,
/// `/proc/<pid>/exe` is authoritative (a recycled pid points at the new
/// owner's binary). Platforms without /proc (macOS) fall back to the
/// process name reported by `ps`.
fn is_sidebar_process(pid: u32) -> bool {
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

fn signal_usr1(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGUSR1);
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
