//! `todo` subcommand — append to and manage the shared TODO list from
//! scripts, slash commands, and agents.
//!
//! The list is the same plain `todos.txt` the TUI panel edits (one file
//! per user account, not per pane), so anything that can exec the binary
//! can hand work to every open sidebar — no IPC, no daemon. Open sidebars
//! re-parse the file within one refresh tick via the stamp poll.
//!
//! Exit codes follow the house pattern (`spawn`): bad usage exits 2, a
//! runtime failure exits 1 with a stderr line, success exits 0. Indexing
//! is positional against the file *at the time of the call*; concurrent
//! edits between a `list` and a `done` can shift numbers — acceptable for
//! a hand-scale list.

use std::path::Path;

use crate::todo::TodoItem;

const USAGE: &str = "\
usage: tmux-agent-sidebar todo add <text...>  append one open item
       tmux-agent-sidebar todo list           print the list (1-based)
       tmux-agent-sidebar todo done <n>       mark item n done
       tmux-agent-sidebar todo open <n>       mark item n open
       tmux-agent-sidebar todo rm <n>         delete item n";

/// Resolve the user's state directory and dispatch. No `TMUX_PANE`
/// requirement: the list is per-user, not per-pane.
pub fn cmd_todo(args: &[String]) -> i32 {
    let Some(dir) = crate::paths::state_dir() else {
        eprintln!("error: no state directory available");
        return 1;
    };
    cmd_todo_in(&dir, args)
}

/// Testable core: operates on an explicit state directory so tests never
/// touch the real `~/.local/state`.
pub(crate) fn cmd_todo_in(dir: &Path, args: &[String]) -> i32 {
    let Some(sub) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return 2;
    };
    match sub {
        "add" => cmd_add(dir, &args[1..]),
        "list" => cmd_list(dir),
        "done" | "open" | "rm" => cmd_mutate(dir, sub, &args[1..]),
        _ => {
            eprintln!("error: unknown todo subcommand {sub:?}\n{USAGE}");
            2
        }
    }
}

/// Join the args with spaces (so both quoted and bare multi-word calls
/// work), trim, and push one open item. Silent on success.
fn cmd_add(dir: &Path, args: &[String]) -> i32 {
    let text = args.join(" ").trim().to_string();
    if text.is_empty() {
        eprintln!("error: todo text is empty\n{USAGE}");
        return 2;
    }
    match crate::todo::update_in(dir, |items| {
        items.push(TodoItem { text, done: false });
    }) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Print `1. [ ] text` per line, 1-based, file order — the same numbering
/// `done` / `open` / `rm` accept.
fn cmd_list(dir: &Path) -> i32 {
    match crate::todo::load_in(dir) {
        Ok(items) => {
            print!("{}", render_list(&items));
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn render_list(items: &[TodoItem]) -> String {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        let mark = if item.done { "✓" } else { " " };
        out.push_str(&format!("{}. [{mark}] {}\n", i + 1, item.text));
    }
    out
}

/// Apply an index-based mutation under the todo lock. A malformed index
/// is a usage error (exit 2); a well-formed index past the end of the
/// list is a runtime failure (exit 1) detected while holding the lock so
/// the check races with concurrent writers, not the reader.
fn cmd_mutate(dir: &Path, sub: &str, args: &[String]) -> i32 {
    let [arg] = args else {
        eprintln!("error: todo {sub} needs exactly one item number\n{USAGE}");
        return 2;
    };
    let Ok(n) = arg.parse::<usize>() else {
        eprintln!("error: todo {sub} needs a 1-based item number, got {arg:?}\n{USAGE}");
        return 2;
    };

    // `update_in`'s closure cannot fail, so an out-of-range index is
    // reported through this captured flag instead. Skipping the edit
    // leaves the items unchanged; the rewrite of identical content is
    // harmless (the stamp poll re-parses and renders the same list).
    let mut failure = None;
    let result = crate::todo::update_in(dir, |items| {
        let Some(idx) = n.checked_sub(1).filter(|i| *i < items.len()) else {
            let plural = if items.len() == 1 { "" } else { "s" };
            failure = Some(format!(
                "no todo item {n} (the list has {} item{plural})",
                items.len()
            ));
            return;
        };
        match sub {
            "done" => items[idx].done = true,
            "open" => items[idx].done = false,
            _ => {
                items.remove(idx);
            }
        }
    });
    match result {
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
        Ok(_) => match failure {
            Some(msg) => {
                eprintln!("error: {msg}");
                1
            }
            None => 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_pushes_one_open_item_and_is_silent_on_success() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            cmd_todo_in(dir.path(), &args(&["add", "fix the login bug"])),
            0
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("todos.txt")).unwrap(),
            "[ ] fix the login bug\n"
        );
    }

    #[test]
    fn add_joins_bare_words_and_trims_ends() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            cmd_todo_in(dir.path(), &args(&["add", "ship", "the", "release"])),
            0
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("todos.txt")).unwrap(),
            "[ ] ship the release\n"
        );
        // Interior spacing is preserved; only the ends are trimmed (same
        // normalization as the TUI's add path).
        assert_eq!(
            cmd_todo_in(dir.path(), &args(&["add", "  padded  words  "])),
            0
        );
        assert!(
            crate::todo::load_in(dir.path())
                .unwrap()
                .iter()
                .any(|i| i.text == "padded  words")
        );
    }

    #[test]
    fn add_without_text_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cmd_todo_in(dir.path(), &args(&["add"])), 2);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["add", "   "])), 2);
    }

    #[test]
    fn unknown_or_missing_subcommand_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cmd_todo_in(dir.path(), &args(&[])), 2);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["frobnicate"])), 2);
    }

    #[test]
    fn list_numbers_items_one_based_in_file_order() {
        let items = vec![
            TodoItem {
                text: "first".into(),
                done: false,
            },
            TodoItem {
                text: "second".into(),
                done: true,
            },
        ];
        assert_eq!(render_list(&items), "1. [ ] first\n2. [✓] second\n");
    }

    #[test]
    fn list_of_nothing_prints_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cmd_todo_in(dir.path(), &args(&["list"])), 0);
        assert_eq!(render_list(&[]), "");
    }

    #[test]
    fn done_open_and_rm_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cmd_todo_in(dir.path(), &args(&["add", "a"])), 0);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["add", "b"])), 0);

        assert_eq!(cmd_todo_in(dir.path(), &args(&["done", "1"])), 0);
        assert_eq!(
            crate::todo::load_in(dir.path()).unwrap(),
            vec![
                TodoItem {
                    text: "a".into(),
                    done: true,
                },
                TodoItem {
                    text: "b".into(),
                    done: false,
                },
            ]
        );

        assert_eq!(cmd_todo_in(dir.path(), &args(&["open", "1"])), 0);
        assert_eq!(
            crate::todo::load_in(dir.path()).unwrap(),
            vec![
                TodoItem {
                    text: "a".into(),
                    done: false,
                },
                TodoItem {
                    text: "b".into(),
                    done: false,
                },
            ]
        );

        assert_eq!(cmd_todo_in(dir.path(), &args(&["rm", "1"])), 0);
        assert_eq!(
            crate::todo::load_in(dir.path()).unwrap(),
            vec![TodoItem {
                text: "b".into(),
                done: false,
            },]
        );
    }

    #[test]
    fn mutation_rejects_bad_indexes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cmd_todo_in(dir.path(), &args(&["add", "a"])), 0);

        // Malformed: usage error.
        assert_eq!(cmd_todo_in(dir.path(), &args(&["done"])), 2);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["done", "1", "2"])), 2);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["rm", "x"])), 2);
        // Well-formed but out of range: runtime failure.
        assert_eq!(cmd_todo_in(dir.path(), &args(&["open", "0"])), 1);
        assert_eq!(cmd_todo_in(dir.path(), &args(&["done", "2"])), 1);
        // The list survived all of it unchanged.
        assert_eq!(
            crate::todo::load_in(dir.path()).unwrap(),
            vec![TodoItem {
                text: "a".into(),
                done: false,
            },]
        );
    }
}
