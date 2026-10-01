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
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("/tmp"),
    }
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
}
