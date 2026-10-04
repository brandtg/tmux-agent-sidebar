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
        if tmux::is_sidebar_process(pid) {
            tmux::signal_usr1(pid);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use crate::tmux::is_sidebar_process;

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
