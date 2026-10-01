use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tmux_agent_sidebar::{app, tmux};

static NEEDS_REFRESH: AtomicBool = AtomicBool::new(false);

/// Publishes the sidebar's pid in `@sidebar_pid` for the lifetime of the
/// TUI session and clears it on exit (normal return, error, or panic
/// unwind). Without the cleanup, a crashed sidebar leaves a stale pid
/// behind that the focus hooks would keep signaling — potentially an
/// unrelated process after pid recycling.
struct SidebarPidGuard {
    pane: String,
    pid: u32,
}

impl SidebarPidGuard {
    fn new(pane: String) -> Self {
        let pid = std::process::id();
        tmux::set_pane_option(&pane, tmux::SIDEBAR_PID, &pid.to_string());
        Self { pane, pid }
    }
}

impl Drop for SidebarPidGuard {
    fn drop(&mut self) {
        // Only clear while the option still points at us — a restarted
        // sidebar in the same pane has already written its own pid, and
        // unsetting it would blind the new instance's refresh hook.
        if tmux::get_pane_option_value(&self.pane, tmux::SIDEBAR_PID) == self.pid.to_string() {
            tmux::unset_pane_option(&self.pane, tmux::SIDEBAR_PID);
        }
    }
}

struct TuiSession {
    entered_alt_screen: bool,
}

impl TuiSession {
    fn enter(stdout: &mut io::Stdout) -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        Ok(Self {
            entered_alt_screen: true,
        })
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        if self.entered_alt_screen {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
        }
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tmux_agent_sidebar::cli::run(&args) {
        std::process::exit(code);
    }

    let tmux_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    if tmux_pane.is_empty() {
        eprintln!("TMUX_PANE not set");
        std::process::exit(1);
    }

    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = sigusr1_handler as *const () as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
    }

    let _pid_guard = SidebarPidGuard::new(tmux_pane.clone());

    let mut stdout = io::stdout();
    let _tui_session = TuiSession::enter(&mut stdout)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app::run(&mut terminal, tmux_pane, &NEEDS_REFRESH)
}

extern "C" fn sigusr1_handler(_: libc::c_int) {
    NEEDS_REFRESH.store(true, Ordering::Relaxed);
}
