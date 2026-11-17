//! Shared filesystem locations and hardening helpers for the sidebar's
//! scratch files (per-pane activity logs, opt-in debug trace).
//!
//! Hook processes (writers) and the TUI (reader) must resolve the same
//! directory for a given file, which holds as long as both run under the
//! same user account.

use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Directory for ephemeral state files shared between hook processes and
/// the sidebar.
///
/// `$XDG_RUNTIME_DIR` when set — per-user, `0700`, typically tmpfs — so
/// predictably-named files stay off world-writable `/tmp` on multi-user
/// hosts. Falls back to `/tmp` for environments that leave it unset
/// (macOS, minimal containers).
pub(crate) fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => {
            let path = PathBuf::from(dir);
            if path.is_dir() {
                path
            } else {
                PathBuf::from("/tmp")
            }
        }
        _ => PathBuf::from("/tmp"),
    }
}

/// Directory for durable user state (the manual TODO list), as opposed
/// to [`runtime_dir`], which holds ephemeral hook/TUI scratch files.
///
/// `$XDG_STATE_HOME` when set, otherwise `~/.local/state` — the same
/// layout on Linux and macOS (macOS has no native XDG state directory,
/// and tmux tooling conventionally uses `~/.local/state`). Returns
/// `None` when neither variable is usable, in which case callers fall
/// back to in-memory-only state.
pub(crate) fn state_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME")?;
            if home.is_empty() {
                return None;
            }
            PathBuf::from(home).join(".local").join("state")
        }
    };
    Some(base.join("tmux-agent-sidebar"))
}

const NOFOLLOW: i32 = libc::O_NOFOLLOW;

/// Read-only open of an existing file, refusing to follow a symlink
/// planted at `path`.
pub(crate) fn open_read(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(NOFOLLOW)
        .open(path)
}

/// Open for read/write in place, creating with mode `0600` when missing
/// (umask can only narrow the mode further). Refuses to follow symlinks.
/// Used by the activity-log writer, which seeks and rewrites in place
/// under an flock instead of relying on `O_APPEND`.
pub(crate) fn open_read_write_private(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(path)
}

/// Open for append, creating with mode `0600` when missing. Refuses to
/// follow symlinks. `O_APPEND` keeps concurrent writers' lines intact.
pub(crate) fn open_append_private(path: &Path) -> io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(NOFOLLOW)
        .open(path)
}

/// Exclusive flock on an open file; false if the lock could not be
/// taken (callers fail open — an unlocked write is only the pre-lock
/// race, while a skipped write would strand state).
pub(crate) fn lock_exclusive(file: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) == 0 }
}

/// Lockfile path for one pane's `scope`-scoped writers across
/// independent hook processes (`%` → `_`, user runtime dir). Never
/// unlinked: removing a flock target while another process waits on the
/// old inode reopens the race the lock exists to prevent.
pub(crate) fn pane_lock_path(scope: &str, pane: &str) -> PathBuf {
    let encoded = pane.replace('%', "_");
    runtime_dir().join(format!("tmux-agent-{scope}-{encoded}.lock"))
}

/// Run `f` while holding the pane's `scope` lock. The lock is released
/// when the fd drops at the end of this function.
///
/// Fail-open, like every flock user here: if the lockfile cannot be
/// opened or flocked, proceed unlocked — a skipped critical section
/// would strand state, while an unlocked run is only the pre-lock race.
pub(crate) fn with_pane_lock<T>(scope: &str, pane: &str, f: impl FnOnce() -> T) -> T {
    let path = pane_lock_path(scope, pane);
    match open_read_write_private(&path) {
        Ok(file) => {
            if !lock_exclusive(&file) {
                crate::debug::log(&format!(
                    "paths: flock failed on {} for pane {pane}; proceeding unlocked",
                    path.display()
                ));
            }
            f()
        }
        Err(err) => {
            crate::debug::log(&format!(
                "paths: failed to open {} for pane {pane} ({err}); proceeding unlocked",
                path.display()
            ));
            f()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tas_paths_{name}_{}", std::process::id()))
    }

    #[test]
    fn opens_refuse_symlink_at_path() {
        let target = scratch("symlink_target");
        let link = scratch("symlink_link");
        let _ = fs::remove_file(&target);
        let _ = fs::remove_file(&link);
        fs::write(&target, "real\n").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(open_read(&link).is_err());
        assert!(open_read_write_private(&link).is_err());
        assert!(open_append_private(&link).is_err());
        // The real target behind the link is still readable.
        assert!(open_read(&target).is_ok());

        fs::remove_file(&link).ok();
        fs::remove_file(&target).ok();
    }

    #[test]
    fn created_files_are_owner_only() {
        let path = scratch("mode");
        let _ = fs::remove_file(&path);

        drop(open_read_write_private(&path).unwrap());
        let rw_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(rw_mode, 0o600, "read/write open must create 0600");
        fs::remove_file(&path).ok();

        drop(open_append_private(&path).unwrap());
        let append_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(append_mode, 0o600, "append open must create 0600");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_write_open_appends_in_place() {
        let path = scratch("rw");
        let _ = fs::remove_file(&path);
        {
            let mut f = open_read_write_private(&path).unwrap();
            use std::io::{Seek, SeekFrom, Write};
            let _ = f.seek(SeekFrom::End(0));
            let _ = f.write_all(b"one\n");
        }
        {
            let mut f = open_read_write_private(&path).unwrap();
            use std::io::{Seek, SeekFrom, Write};
            let _ = f.seek(SeekFrom::End(0));
            let _ = f.write_all(b"two\n");
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn with_pane_lock_excludes_concurrent_holders() {
        // The per-pane lock serializes read-modify-writes across
        // independent hook processes (subagent list, session binding +
        // title, notification stamp, bg cmd/pid pair). Prove mutual
        // exclusion of the shared helper with real threads: at most one
        // thread may be inside the critical section at any moment.
        let pane = "%LOCK_RACE";
        let lock = pane_lock_path("paths_test", pane);
        let _ = fs::remove_file(&lock);

        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let inside = Arc::new(AtomicUsize::new(0));
        let max_inside = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let inside = inside.clone();
                let max_inside = max_inside.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        with_pane_lock("paths_test", pane, || {
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
            "flock must exclude concurrent holders of the same pane lock"
        );
        fs::remove_file(&lock).ok();
    }
}
