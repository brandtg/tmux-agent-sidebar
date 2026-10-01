use std::time::Instant;

/// Periodic-refresh bookkeeping. Bundles the wall/monotonic clocks that
/// gate refresh cadence in `state/refresh.rs` (port scan, filter-bar
/// debounce, and the "first port scan completed" flag) so they live as
/// a unit instead of cluttering [`AppState`].
///
/// `session_names` is intentionally NOT here: the polling lives in a
/// dedicated background thread (`session_poll_loop` in `main.rs`) so the
/// TUI thread never performs blocking filesystem I/O.
#[derive(Debug, Clone)]
pub struct RefreshTimers {
    /// Last time a mouse click was processed on the filter bar (debounce).
    pub last_filter_click: Instant,
    /// Timestamp of the last port/command scan.
    pub last_port_refresh: Instant,
    /// Whether the first port scan has completed; the first scan must
    /// always run regardless of the elapsed-time gate.
    pub port_scan_initialized: bool,
    /// Whether the first snapshot has primed the path→git-info cache.
    /// The priming runs synchronously once (matching the old inline
    /// resolver) so the first frame groups by repo root; afterwards the
    /// cache is maintained exclusively by `git_info_poll_loop`.
    pub git_info_primed: bool,
    /// Timestamp of the last background-shell liveness sweep.
    pub last_bg_shell_sweep: Option<Instant>,
}

impl Default for RefreshTimers {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            last_filter_click: now,
            last_port_refresh: now,
            port_scan_initialized: false,
            git_info_primed: false,
            last_bg_shell_sweep: None,
        }
    }
}
