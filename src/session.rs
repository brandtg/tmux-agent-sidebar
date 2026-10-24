use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::subprocess;

/// Deadline for the `sqlite3` probe against Hermes' session store — same bound
/// as the git subprocesses, so a wedged read cannot stall the polling thread.
const HERMES_QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the Hermes titles fetched per scan. Recency-ordered, so this only
/// bites on DBs holding more sessions than could plausibly be open in panes.
const HERMES_TITLE_LIMIT: usize = 2000;

/// Scan the agents' session stores for `session_id → title` labels.
///
/// Four sources share one map because the key spaces never collide: Claude
/// Code ids are UUIDs (`~/.claude/sessions/*.json`), Hermes ids are
/// `YYYYMMDD_HHMMSS_hex` (the `sessions` table of `~/.hermes/state.db`),
/// Antigravity ids are UUIDs (the `conversation_summaries` table of
/// `~/.gemini/antigravity-cli/conversation_summaries.db`), and OpenCode ids
/// are `ses_…` (the `session_v2` table of
/// `~/.local/share/opencode/opencode.db`) — and those are the same ids the
/// agents put in their hook payloads, so the map keys match
/// `@pane_session_id` on the pane.
pub fn scan_session_names() -> HashMap<String, String> {
    let mut map = scan_claude_session_names();
    for (session_id, name) in scan_hermes_session_titles() {
        map.insert(session_id, name);
    }
    for (session_id, name) in scan_antigravity_session_titles() {
        map.insert(session_id, name);
    }
    for (session_id, name) in scan_opencode_session_titles() {
        map.insert(session_id, name);
    }
    map
}

/// Return the path to Claude Code's sessions directory.
fn sessions_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".claude").join("sessions");
    if dir.is_dir() { Some(dir) } else { None }
}

fn scan_claude_session_names() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(dir) = sessions_dir() else {
        return map;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some((session_id, name)) = parse_session_file(&path) {
            map.insert(session_id, name);
        }
    }
    map
}

/// Return the path to Hermes' session store, when present.
fn hermes_state_db() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let db = PathBuf::from(home).join(".hermes").join("state.db");
    if db.is_file() { Some(db) } else { None }
}

/// Read `(id, title)` pairs from Hermes' SQLite session store via the
/// `sqlite3` CLI in JSON mode. Hermes auto-titles sessions off the opening
/// message after the first turn (no hook event carries the title), so a
/// background read of the store is the only way to pick titles up.
///
/// Missing `sqlite3` binary, unreadable DB, or any query failure yields an
/// empty map: Hermes rows then keep the plain agent label. `-readonly` keeps
/// the probe from taking write locks on the live WAL database.
fn scan_hermes_session_titles() -> HashMap<String, String> {
    let Some(db) = hermes_state_db() else {
        return HashMap::new();
    };
    let mut command = Command::new("sqlite3");
    command.arg("-json").arg("-readonly").arg(&db).arg(format!(
        "SELECT id, title FROM sessions WHERE title IS NOT NULL \
             ORDER BY started_at DESC LIMIT {HERMES_TITLE_LIMIT}"
    ));
    let Ok(output) = subprocess::run_with_timeout(&mut command, HERMES_QUERY_TIMEOUT) else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    parse_hermes_titles(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `sqlite3 -json` output (`[{"id": "...", "title": "..."}, ...]`).
/// Shared with [`scan_opencode_session_titles`], whose `session_v2` rows
/// carry the same `id`/`title` shape.
/// Empty results print as nothing at all; malformed output, a non-array
/// payload, and rows missing either field all degrade to "skip" rather than
/// erroring, so a schema change upstream can never break the scan.
fn parse_hermes_titles(json: &str) -> HashMap<String, String> {
    let Ok(rows) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return HashMap::new();
    };
    let Some(rows) = rows.as_array() else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for row in rows {
        let (Some(id), Some(title)) = (
            row.get("id").and_then(|v| v.as_str()).map(str::trim),
            row.get("title").and_then(|v| v.as_str()).map(str::trim),
        ) else {
            continue;
        };
        if id.is_empty() || title.is_empty() {
            continue;
        }
        map.insert(id.to_string(), title.to_string());
    }
    map
}

/// Return the path to Antigravity's conversation-summaries store, when present.
fn antigravity_summaries_db() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let db = PathBuf::from(home)
        .join(".gemini")
        .join("antigravity-cli")
        .join("conversation_summaries.db");
    if db.is_file() { Some(db) } else { None }
}

/// Read `(conversation_id, title)` pairs from Antigravity's SQLite
/// conversation-summaries store via the `sqlite3` CLI in JSON mode.
///
/// Antigravity auto-titles conversations after the first turn (no hook event
/// carries a title), so a background read of the store is the only way to
/// pick titles up. The `title` column holds a user-set `/title` label and is
/// usually empty; the auto-generated title lives in `preview`, so it is used
/// as the fallback — matching what the Antigravity UI itself shows.
///
/// Missing `sqlite3` binary, unreadable DB, or any query failure yields an
/// empty map: Antigravity rows then keep the plain agent label. `-readonly`
/// keeps the probe from taking write locks on the live WAL database.
fn scan_antigravity_session_titles() -> HashMap<String, String> {
    let Some(db) = antigravity_summaries_db() else {
        return HashMap::new();
    };
    let mut command = Command::new("sqlite3");
    command.arg("-json").arg("-readonly").arg(&db).arg(format!(
        "SELECT conversation_id, title, preview FROM conversation_summaries \
              ORDER BY last_modified_time DESC LIMIT {HERMES_TITLE_LIMIT}"
    ));
    let Ok(output) = subprocess::run_with_timeout(&mut command, HERMES_QUERY_TIMEOUT) else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    parse_antigravity_titles(&String::from_utf8_lossy(&output.stdout))
}

/// Parse `sqlite3 -json` output
/// (`[{"conversation_id": "...", "title": "...", "preview": "..."}, ...]`).
/// Same degradation rules as [`parse_hermes_titles`]: a row with no usable
/// name is skipped, never an error. `title` wins over `preview` because a
/// user-set `/title` label should not be masked by the auto-title.
fn parse_antigravity_titles(json: &str) -> HashMap<String, String> {
    let Ok(rows) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return HashMap::new();
    };
    let Some(rows) = rows.as_array() else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for row in rows {
        let Some(id) = row
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
        else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        let title = row
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let preview = row
            .get("preview")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let name = if title.is_empty() { preview } else { title };
        if name.is_empty() {
            continue;
        }
        map.insert(id.to_string(), name.to_string());
    }
    map
}

/// Return the path to OpenCode's session store, when present.
fn opencode_session_db() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let db = PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if db.is_file() { Some(db) } else { None }
}

/// Read `(id, title)` pairs from OpenCode's SQLite session store via the
/// `sqlite3` CLI in JSON mode.
///
/// OpenCode auto-titles sessions after the first turn and delivers the
/// rename through the plugin bridge, but the write can be lost — a session
/// switch rebinds the pane and clears `@pane_session_title`, and the
/// auto-title `session.renamed` never fires again. The store read is the
/// self-healing path: every tick re-applies the stored title to any pane
/// whose `@pane_session_id` matches.
///
/// v2.1.x keeps sessions in `session_v2`; the legacy `session` table is no
/// longer written and holds only pre-upgrade rows, so it is ignored. A
/// missing table (older/never-installed versions) just errors the probe
/// into an empty map. `-readonly` keeps the probe from taking write locks
/// on the live WAL database.
fn scan_opencode_session_titles() -> HashMap<String, String> {
    let Some(db) = opencode_session_db() else {
        return HashMap::new();
    };
    let mut command = Command::new("sqlite3");
    command.arg("-json").arg("-readonly").arg(&db).arg(format!(
        "SELECT id, title FROM session_v2 WHERE title IS NOT NULL \
              ORDER BY time_updated DESC LIMIT {HERMES_TITLE_LIMIT}"
    ));
    let Ok(output) = subprocess::run_with_timeout(&mut command, HERMES_QUERY_TIMEOUT) else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    parse_hermes_titles(&String::from_utf8_lossy(&output.stdout))
}

/// Parse a single session JSON file, returning `(sessionId, name)` if both exist.
fn parse_session_file(path: &std::path::Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    let session_id = val.get("sessionId")?.as_str()?.trim();
    let name = val.get("name")?.as_str()?.trim();
    if session_id.is_empty() || name.is_empty() {
        return None;
    }
    Some((session_id.to_string(), name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_antigravity_titles_reads_rows() {
        // Real row shape from conversation_summaries.db: the auto-title is
        // in `preview` and `title` is the (usually empty) /title column.
        let map = parse_antigravity_titles(
            r#"[{"conversation_id":"b1d69df5-58e7","title":"","preview":"Migrating Inference To Uv"},
                {"conversation_id":"39d1b6d7-7983","title":"","preview":"Creating Makefile For Inference"},
                {"conversation_id":"6b5ea64f-27f8","title":"Renamed by /title","preview":"Auto Title"}]"#,
        );
        assert_eq!(map.len(), 3);
        assert_eq!(map["b1d69df5-58e7"], "Migrating Inference To Uv");
        assert_eq!(map["39d1b6d7-7983"], "Creating Makefile For Inference");
        // A user-set title wins over the auto-generated preview.
        assert_eq!(map["6b5ea64f-27f8"], "Renamed by /title");
    }

    #[test]
    fn parse_antigravity_titles_empty_output_is_empty_map() {
        assert!(parse_antigravity_titles("").is_empty());
        assert!(parse_antigravity_titles("[]").is_empty());
    }

    #[test]
    fn parse_antigravity_titles_malformed_output_is_empty_map() {
        assert!(parse_antigravity_titles("not json").is_empty());
        assert!(parse_antigravity_titles("{\"id\":\"x\"}").is_empty());
    }

    #[test]
    fn parse_antigravity_titles_skips_unnamed_and_incomplete_rows() {
        let map = parse_antigravity_titles(
            r#"[{"conversation_id":"a","title":"","preview":""},
                {"conversation_id":"","title":"empty id"},
                {"preview":"no id"},
                {"conversation_id":"b","title":"  spaced  ","preview":""}]"#,
        );
        assert_eq!(map.len(), 1);
        assert_eq!(map["b"], "spaced");
    }

    #[test]
    fn parse_session_file_with_name() {
        let dir = std::env::temp_dir().join("session_test_with_name");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("12345.json");
        fs::write(
            &path,
            r#"{"pid":12345,"sessionId":"abc-def","name":"my-session","cwd":"/tmp"}"#,
        )
        .unwrap();

        let result = parse_session_file(&path);
        assert_eq!(result, Some(("abc-def".into(), "my-session".into())));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_session_file_without_name() {
        let dir = std::env::temp_dir().join("session_test_no_name");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("12345.json");
        fs::write(&path, r#"{"pid":12345,"sessionId":"abc-def","cwd":"/tmp"}"#).unwrap();

        assert!(parse_session_file(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_session_file_empty_name() {
        let dir = std::env::temp_dir().join("session_test_empty_name");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("12345.json");
        fs::write(
            &path,
            r#"{"pid":12345,"sessionId":"abc-def","name":"","cwd":"/tmp"}"#,
        )
        .unwrap();

        assert!(parse_session_file(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_session_file_whitespace_only_name() {
        let dir = std::env::temp_dir().join("session_test_whitespace_name");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("12345.json");
        fs::write(
            &path,
            r#"{"pid":12345,"sessionId":"abc-def","name":"   ","cwd":"/tmp"}"#,
        )
        .unwrap();

        assert!(parse_session_file(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_session_file_malformed_json() {
        let dir = std::env::temp_dir().join("session_test_malformed");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("12345.json");
        fs::write(&path, "not json at all").unwrap();

        assert!(parse_session_file(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_session_file_nonexistent() {
        let path = std::env::temp_dir().join("session_test_nonexistent/99999.json");
        assert!(parse_session_file(&path).is_none());
    }

    #[test]
    fn parse_hermes_titles_reads_rows() {
        let map = parse_hermes_titles(
            r#"[{"id":"20261016_120317_dc6c9a","title":"Fix the login flow"},
                {"id":"20261015_195041_6b2d8f","title":"Sync config"}]"#,
        );
        assert_eq!(map.len(), 2);
        assert_eq!(map["20261016_120317_dc6c9a"], "Fix the login flow");
        assert_eq!(map["20261015_195041_6b2d8f"], "Sync config");
    }

    #[test]
    fn parse_hermes_titles_empty_output_is_empty_map() {
        // `sqlite3 -json` prints nothing at all for an empty result set.
        assert!(parse_hermes_titles("").is_empty());
        assert!(parse_hermes_titles("[]").is_empty());
    }

    #[test]
    fn parse_hermes_titles_malformed_output_is_empty_map() {
        assert!(parse_hermes_titles("not json").is_empty());
        // A schema change upstream must degrade, not panic.
        assert!(parse_hermes_titles("{\"id\":\"x\"}").is_empty());
    }

    #[test]
    fn parse_hermes_titles_skips_incomplete_rows() {
        let map = parse_hermes_titles(
            r#"[{"id":"a"},
                {"id":"","title":"empty id"},
                {"id":"  ","title":"blank id"},
                {"title":"no id"},
                {"id":"b","title":"  padded  "},
                {"id":"c","title":""}]"#,
        );
        assert_eq!(map.len(), 1);
        assert_eq!(map["b"], "padded");
    }
}
