//! Durable storage for the sidebar's manual TODO list.
//!
//! The list is a plain-text file shared by every sidebar instance (one
//! file per user account, not per window or session). Writes go through
//! an exclusive `flock` and an atomic temp-file rename so concurrent
//! sidebars cannot lose each other's edits and readers never observe a
//! partial write.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::paths;

const FILE_NAME: &str = "todos.txt";
const LOCK_NAME: &str = "todos.lock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub text: String,
    pub done: bool,
}

/// Resolve the on-disk list path, or `None` when no state directory is
/// available (persistence is disabled and the list stays in memory).
pub fn todos_path() -> Option<PathBuf> {
    paths::state_dir().map(|dir| dir.join(FILE_NAME))
}

/// Parse the canonical line format: `[ ] text` for open items and
/// `[✓] text` for done ones. `[x]` / `[X]` are accepted too so a
/// hand-edited file keeps working. Lines without a checkbox are ignored
/// rather than turned into items.
pub fn parse(contents: &str) -> Vec<TodoItem> {
    contents.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<TodoItem> {
    let line = line.trim();
    let rest = line.strip_prefix('[')?;
    let close = rest.find(']')?;
    let mark = rest[..close].trim();
    let done = match mark {
        "" => false,
        "x" | "X" | "✓" => true,
        _ => return None,
    };
    let text = rest[close + 1..].trim();
    if text.is_empty() {
        return None;
    }
    Some(TodoItem {
        text: text.to_string(),
        done,
    })
}

/// Serialize items in display order, one per line, always using the
/// canonical `[ ]` / `[✓]` markers.
pub fn serialize(items: &[TodoItem]) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str(if item.done { "[✓] " } else { "[ ] " });
        out.push_str(item.text.trim());
        out.push('\n');
    }
    out
}

/// Load the list for the current user. A missing file means an empty
/// list, not an error.
pub fn load() -> io::Result<Vec<TodoItem>> {
    match paths::state_dir() {
        Some(dir) => load_in(&dir),
        None => Ok(Vec::new()),
    }
}

/// Stamp of the current list file, used to detect edits made by another
/// sidebar instance.
pub fn stamp() -> Option<(SystemTime, u64)> {
    paths::state_dir().and_then(|dir| stamp_in(&dir))
}

/// Locked read-modify-write against the current user's list.
pub fn update<F>(f: F) -> io::Result<Vec<TodoItem>>
where
    F: FnOnce(&mut Vec<TodoItem>),
{
    let Some(dir) = paths::state_dir() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no state directory available",
        ));
    };
    update_in(&dir, f)
}

pub(crate) fn load_in(dir: &Path) -> io::Result<Vec<TodoItem>> {
    let path = dir.join(FILE_NAME);
    match paths::open_read(&path) {
        Ok(mut file) => {
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            Ok(parse(&contents))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

pub(crate) fn stamp_in(dir: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(dir.join(FILE_NAME)).ok()?;
    let modified = meta.modified().ok()?;
    Some((modified, meta.len()))
}

pub(crate) fn update_in<F>(dir: &Path, f: F) -> io::Result<Vec<TodoItem>>
where
    F: FnOnce(&mut Vec<TodoItem>),
{
    create_dir(dir)?;
    let lock = paths::open_read_write_private(&dir.join(LOCK_NAME))?;
    // Best-effort lock: a failed flock degrades to an unlocked write
    // (same policy as the subagent activity lock) instead of failing the
    // user's edit outright.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0;
    let result = (|| {
        let mut items = load_in(dir)?;
        f(&mut items);
        write_atomic(dir, &items)?;
        Ok(items)
    })();
    if locked {
        unsafe {
            libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
        }
    }
    result
}

fn create_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

fn write_atomic(dir: &Path, items: &[TodoItem]) -> io::Result<()> {
    let path = dir.join(FILE_NAME);
    let tmp = dir.join(format!(".{FILE_NAME}.tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(serialize(items).as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parse_accepts_canonical_and_hand_edited_markers() {
        let items = parse(
            "[ ] Buy milk\n\
             [✓] Send the invoice\n\
             [x] lowercase done\n\
             [X] uppercase done\n\
             [  ] padded open\n",
        );
        assert_eq!(
            items,
            vec![
                TodoItem {
                    text: "Buy milk".into(),
                    done: false
                },
                TodoItem {
                    text: "Send the invoice".into(),
                    done: true
                },
                TodoItem {
                    text: "lowercase done".into(),
                    done: true
                },
                TodoItem {
                    text: "uppercase done".into(),
                    done: true
                },
                TodoItem {
                    text: "padded open".into(),
                    done: false
                },
            ]
        );
    }

    #[test]
    fn parse_ignores_lines_without_a_checkbox() {
        let items = parse("# a comment\n[?] unknown\n[]\nnot a todo\n[ ] real\n");
        assert_eq!(
            items,
            vec![TodoItem {
                text: "real".into(),
                done: false
            }]
        );
    }

    #[test]
    fn serialize_round_trips_through_parse() {
        let items = vec![
            TodoItem {
                text: "one".into(),
                done: false,
            },
            TodoItem {
                text: "two".into(),
                done: true,
            },
        ];
        let text = serialize(&items);
        assert_eq!(text, "[ ] one\n[✓] two\n");
        assert_eq!(parse(&text), items);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_in(dir.path()).unwrap(), Vec::<TodoItem>::new());
        assert_eq!(stamp_in(dir.path()), None);
    }

    #[test]
    fn update_creates_private_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("tmux-agent-sidebar");

        let items = update_in(&dir, |items| {
            items.push(TodoItem {
                text: "first".into(),
                done: false,
            })
        })
        .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(load_in(&dir).unwrap(), items);
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn load_refuses_a_symlinked_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        fs::write(&target, "[ ] sneaky\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(FILE_NAME)).unwrap();

        assert!(load_in(dir.path()).is_err());
    }

    #[test]
    fn concurrent_updates_do_not_lose_items() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        const PER_THREAD: usize = 25;
        std::thread::scope(|scope| {
            for t in 0..2 {
                let dir = &dir;
                scope.spawn(move || {
                    for i in 0..PER_THREAD {
                        update_in(dir, |items| {
                            items.push(TodoItem {
                                text: format!("t{t}-{i}"),
                                done: false,
                            });
                        })
                        .unwrap();
                    }
                });
            }
        });

        let items = load_in(&dir).unwrap();
        assert_eq!(items.len(), PER_THREAD * 2);
    }

    #[test]
    fn update_toggles_by_index() {
        let dir = tempfile::tempdir().unwrap();
        update_in(dir.path(), |items| {
            items.push(TodoItem {
                text: "a".into(),
                done: false,
            });
            items.push(TodoItem {
                text: "b".into(),
                done: false,
            });
        })
        .unwrap();

        update_in(dir.path(), |items| items[1].done = true).unwrap();
        let items = load_in(dir.path()).unwrap();
        assert!(!items[0].done);
        assert!(items[1].done);
    }
}
