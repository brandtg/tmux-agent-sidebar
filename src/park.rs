//! Resume support for the TODO panel: recognize resume-shaped todo
//! items, resolve the working directory to relaunch in, and open a new
//! tmux window running the rebuilt command.
//!
//! Two consumers share the per-agent grammar table: the todo-text
//! recognizer (`r` on a resume-shaped item) and, later, the parked-
//! session launcher. The unifying rule is *parse, never execute* — the
//! launched command is rebuilt from (agent, id) via [`resume_command`]
//! with canonical flags; the item's text is only ever read, and ids are
//! pattern-validated so no shell metacharacters can ride along.
//!
//! This module also owns the `parked.json` sidecar read: the cwd
//! resolution for `r` consults it (between the item's `@ <path>` hint
//! and the agent's own store). The `p` write path and the Parked view
//! land with parked sessions proper; the schema below is already final.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::subprocess;
use crate::worktree::AGENT_PANE_PERCENT;

/// Deadline for a `sqlite3` store probe — same bound as session.rs, so
/// a wedged read cannot stall its worker thread.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Window-scope stamp marking a window the sidebar resumed into, and
/// the agent it resumed. Mirrors the spawn markers
/// (`src/worktree/markers.rs`) so a future `x`-style flow can
/// recognize these windows.
pub const RESUMED_OPTION: &str = "@agent-sidebar-resumed";
pub const RESUME_AGENT_OPTION: &str = "@agent-sidebar-resume-agent";

const PARKED_FILE: &str = "parked.json";

/// A recognized resume command, parsed out of a todo item's text
/// (the trimmed text after the checkbox).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRef {
    /// Canonical agent label ("claude", "codex", "opencode").
    pub agent: String,
    /// The session id, normalized per the agent's case rule: claude
    /// ids are rebuilt lowercase, codex and opencode ids verbatim.
    pub id: String,
    /// Explicit working directory from a trailing `@ <path>` suffix,
    /// with a leading `~` left for [`expand_home`].
    pub cwd_hint: Option<PathBuf>,
}

/// An entry in the shared `parked.json` sidecar: a session recorded
/// durably so it can be resumed after its pane is gone. Keyed by
/// (agent, session_id) with upsert-dedupe and a 50-entry cap once the
/// write path lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedSession {
    pub agent: String,
    pub session_id: String,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    /// Last prompt, for context at a glance.
    pub prompt: Option<String>,
    /// Epoch seconds.
    pub parked_at: u64,
    pub origin: ParkOrigin,
}

/// Whether the entry was parked by hand (`p`) or by the auto-park on
/// SessionEnd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkOrigin {
    Manual,
    Auto,
}

impl ParkedSession {
    /// Degrade-to-None row parse: a parked.json written by a newer or
    /// older schema contributes only the rows it can fully parse.
    fn from_value(row: &serde_json::Value) -> Option<Self> {
        let obj = row.as_object()?;
        Some(Self {
            agent: str_field(obj, "agent")?,
            session_id: str_field(obj, "session_id")?,
            cwd: opt_str_field(obj, "cwd"),
            branch: opt_str_field(obj, "branch"),
            title: opt_str_field(obj, "title"),
            prompt: opt_str_field(obj, "prompt"),
            parked_at: obj.get("parked_at").and_then(|v| v.as_u64())?,
            origin: match obj.get("origin").and_then(|v| v.as_str()) {
                Some("auto") => ParkOrigin::Auto,
                _ => ParkOrigin::Manual,
            },
        })
    }
}

fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_string)
}

fn opt_str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

// ─── Recognizer ──────────────────────────────────────────────────────

/// Canonicalize a first-token binary name. `agy` / `antigravity` map to
/// the same label, whose resume grammar is unknown — [`recognize_resume`]
/// declines it and the item stays a plain todo.
fn canonical_agent(token: &str) -> Option<&'static str> {
    match token {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "opencode" => Some("opencode"),
        "agy" | "antigravity" => Some("antigravity"),
        _ => None,
    }
}

/// Claude session ids are UUIDs: hex digits and dashes, at least 8
/// characters. Matched case-insensitively; the ref carries the
/// lowercased form.
fn is_claude_id(id: &str) -> bool {
    id.len() >= 8
        && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && id.chars().any(|c| c.is_ascii_hexdigit())
}

/// Codex accepts UUIDs and thread names: `[A-Za-z0-9._-]+` that does
/// not start with `-` (a `-`-leading token is a flag, not an id).
/// Preserved verbatim.
fn is_codex_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// OpenCode ids are `ses_` + alphanumerics. Case-sensitive and
/// preserved verbatim — ids are mixed-case and never lowercased.
fn is_opencode_id(id: &str) -> bool {
    id.len() > "ses_".len()
        && id.starts_with("ses_")
        && id["ses_".len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric())
}

/// Split a trailing `@ <path>` hint off `text`: the last occurrence of
/// ` @ ` whose suffix is a single whitespace-free token starting with
/// `~`, `/`, `.`, or `$`. Returns `(resume_text, hint)`. This is a
/// documented text convention — the line stays a plain hand-editable
/// todo item.
fn split_cwd_hint(text: &str) -> (&str, Option<PathBuf>) {
    let mut best: Option<(usize, &str)> = None;
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(" @ ") {
        let at = search_from + rel;
        let suffix = &text[at + " @ ".len()..];
        if !suffix.is_empty()
            && !suffix.chars().any(char::is_whitespace)
            && matches!(suffix.chars().next(), Some('~' | '/' | '.' | '$'))
        {
            best = Some((at, suffix));
        }
        search_from = at + 1;
    }
    match best {
        Some((at, suffix)) => (&text[..at], Some(PathBuf::from(suffix))),
        None => (text, None),
    }
}

/// Recognize a resume command in a todo item's text. The first token
/// must be an agent binary name; the resume token and id must match the
/// per-agent grammar. A missing or malformed id means *not recognized*
/// — no half-matches, so unrecognized items keep no glyph and `r` stays
/// inert on them.
pub fn recognize_resume(text: &str) -> Option<ResumeRef> {
    let text = text.trim();
    let (resume_text, hint) = split_cwd_hint(text);
    let mut words = resume_text.split_whitespace();
    let agent = canonical_agent(words.next()?)?;
    if agent == "antigravity" {
        // Resume grammar unknown: the recognizer declines, and the item
        // stays a plain todo.
        return None;
    }
    let rest: Vec<&str> = words.collect();
    let id = match agent {
        // `codex resume <id>` is the one subcommand form: token 2.
        "codex" => {
            if rest.first() != Some(&"resume") {
                return None;
            }
            let id = *rest.get(1)?;
            is_codex_id(id).then(|| id.to_string())?
        }
        // The others take a flag anywhere in the remaining tokens, with
        // the id as the next token.
        "claude" => {
            let pos = rest.iter().position(|t| *t == "--resume" || *t == "-r")?;
            let id = *rest.get(pos + 1)?;
            is_claude_id(id).then(|| id.to_ascii_lowercase())?
        }
        "opencode" => {
            let pos = rest.iter().position(|t| *t == "-s" || *t == "--session")?;
            let id = *rest.get(pos + 1)?;
            is_opencode_id(id).then(|| id.to_string())?
        }
        _ => return None,
    };
    Some(ResumeRef {
        agent: agent.to_string(),
        id,
        cwd_hint: hint,
    })
}

/// Rebuild the canonical resume command for (agent, id). Returns `None`
/// for agents with no known resume grammar (antigravity) — the caller
/// surfaces a "not supported yet" message instead of launching
/// something wrong. Ids are pattern-validated upstream, so the output
/// carries no shell metacharacters.
pub fn resume_command(agent: &str, id: &str) -> Option<String> {
    let command = match agent {
        "claude" => format!("claude --resume {id}"),
        "codex" => format!("codex resume {id}"),
        "opencode" => format!("opencode -s {id}"),
        _ => return None,
    };
    Some(command)
}

/// Expand a leading `~` in a cwd hint against `home` (`~` and `~/…`).
/// Everything else — `/abs`, `./rel`, `$VAR` — passes through verbatim;
/// the convention expands `~` only. `None` means `~` with no usable
/// `$HOME`, i.e. no cwd.
pub(crate) fn expand_home(hint: &Path, home: Option<&str>) -> Option<String> {
    let text = hint.to_str()?;
    match text {
        "~" => home.map(str::to_string),
        _ if text.starts_with("~/") => home.map(|h| format!("{h}{}", &text[1..])),
        _ => Some(text.to_string()),
    }
}

// ─── cwd resolution ──────────────────────────────────────────────────

/// Resolve the cwd for a recognized resume, in priority order: the
/// item's `@ <path>` hint (explicit user intent wins), the shared
/// parked.json sidecar, then the agent's own store. `None` means fail —
/// the caller flashes the escape hatch rather than launching in `$HOME`.
pub(crate) fn resolve_cwd(
    hint: Option<&Path>,
    dir: Option<&Path>,
    agent: &str,
    id: &str,
    home: &Path,
) -> Option<String> {
    if let Some(hint) = hint {
        return expand_home(hint, home.to_str());
    }
    if let Some(cwd) = parked_cwd(dir, agent, id) {
        return Some(cwd);
    }
    probe_cwd(agent, id, home)
}

/// The cwd recorded for (agent, session_id) in the state directory's
/// parked.json sidecar, when it holds a matching entry. Read-only — the
/// `p` write path lands with parked sessions. A missing, unreadable, or
/// malformed file is simply "no record".
pub(crate) fn parked_cwd(dir: Option<&Path>, agent: &str, session_id: &str) -> Option<String> {
    let dir = dir?;
    let mut file = crate::paths::open_read(&dir.join(PARKED_FILE)).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    let entries = serde_json::from_str::<serde_json::Value>(contents.trim())
        .ok()?
        .as_array()?
        .iter()
        .filter_map(ParkedSession::from_value)
        .collect::<Vec<_>>();
    entries
        .iter()
        .find(|e| e.agent == agent && e.session_id == session_id)
        .and_then(|e| e.cwd.clone())
}

/// Probe the agent's own store for the session's cwd. Each probe takes
/// the home dir so tests can point it at a fixture tree; a missing
/// store or any probe failure degrades to "no cwd", never an error
/// (the `@ <path>` suffix is the manual escape hatch).
pub(crate) fn probe_cwd(agent: &str, session_id: &str, home: &Path) -> Option<String> {
    match agent {
        "claude" => claude_cwd(home, session_id),
        "codex" => codex_cwd(home, session_id),
        "opencode" => opencode_cwd(home, session_id),
        _ => None,
    }
}

/// Claude: scan `~/.claude/sessions/*.json` for the matching
/// `.sessionId` and read its `.cwd`. The `.key` files alongside are
/// skipped by the extension check. Ids compare case-insensitively —
/// the recognizer lowercases claude ids, the store may not.
/// `sessions/` is pid-keyed and only holds *running* sessions, so the
/// probe falls back to the transcript store (see
/// [`claude_cwd_from_projects`]) for ended ones.
fn claude_cwd(home: &Path, session_id: &str) -> Option<String> {
    let dir = home.join(".claude").join("sessions");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return claude_cwd_from_projects(home, session_id);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let matches = value
            .get("sessionId")
            .and_then(|v| v.as_str())
            .is_some_and(|sid| sid.eq_ignore_ascii_case(session_id));
        if matches
            && let Some(cwd) = value.get("cwd").and_then(|v| v.as_str())
            && !cwd.is_empty()
        {
            return Some(cwd.to_string());
        }
    }
    claude_cwd_from_projects(home, session_id)
}

/// How many transcript lines the projects-store fallback parses before
/// giving up. The leading entries are session metadata (`last-prompt`,
/// `mode`, `permission-mode`, …) without a cwd; the first message
/// entries — which all carry `cwd` — follow within a few lines.
const PROJECTS_SCAN_LINES: usize = 50;

/// Claude fallback for ended sessions: the transcript store at
/// `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl` outlives the
/// pid-keyed `sessions/` files, and its entries carry the session's
/// exact `cwd` — no decoding of the dash-encoded directory name
/// needed. A missing transcript (session from another machine, or
/// pruned) is simply "no cwd". One unreadable transcript skips to the
/// next candidate directory instead of aborting the whole walk: the
/// read_dir order is not deterministic, so a `?` here made the resume
/// cwd hint flaky (audit 2026-11-16).
fn claude_cwd_from_projects(home: &Path, session_id: &str) -> Option<String> {
    let projects = home.join(".claude").join("projects");
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        let transcript = entry.path().join(format!("{session_id}.jsonl"));
        if !transcript.is_file() {
            continue;
        }
        let Ok(file) = std::fs::File::open(&transcript) else {
            continue;
        };
        for line in std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .take(PROJECTS_SCAN_LINES)
        {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str())
                && !cwd.is_empty()
            {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

/// Codex: `threads` table of `~/.codex/state_5.sqlite`. Thread ids may
/// be UUIDs or thread names; the id equality is exact either way. The
/// documented fallback (`session_index.jsonl`) carries no cwd — skipped.
fn codex_cwd(home: &Path, session_id: &str) -> Option<String> {
    let db = home.join(".codex").join("state_5.sqlite");
    if !db.is_file() {
        return None;
    }
    sqlite_first_cell(
        &db,
        &format!(
            "SELECT cwd FROM threads WHERE id = '{}'",
            sql_literal(session_id)
        ),
    )
}

/// OpenCode: the `directory` column of `session_v2` in
/// `~/.local/share/opencode/opencode.db`, falling back to the legacy
/// `session` table — the schema name varies by version.
fn opencode_cwd(home: &Path, session_id: &str) -> Option<String> {
    let db = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if !db.is_file() {
        return None;
    }
    let id = sql_literal(session_id);
    sqlite_first_cell(
        &db,
        &format!("SELECT directory FROM session_v2 WHERE id = '{id}'"),
    )
    .or_else(|| {
        sqlite_first_cell(
            &db,
            &format!("SELECT directory FROM session WHERE id = '{id}'"),
        )
    })
}

/// Inline a session id into a probe query. The recognizer's patterns
/// already forbid quotes; escaping is defense in depth for ids that
/// arrive from the parked sidecar instead of item text.
fn sql_literal(id: &str) -> String {
    id.replace('\'', "''")
}

/// One `sqlite3 -json -readonly` probe: first cell of the first row, or
/// `None` on any failure (missing binary, missing table, empty result).
/// A schema change upstream can never break the caller — it just reads
/// as "no cwd". `-readonly` keeps the probe off the live WAL database's
/// write locks.
fn sqlite_first_cell(db: &Path, query: &str) -> Option<String> {
    let mut command = Command::new("sqlite3");
    command.arg("-json").arg("-readonly").arg(db).arg(query);
    let output = subprocess::run_with_timeout(&mut command, PROBE_TIMEOUT).ok()?;
    parse_first_cell(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
    )
}

/// Pure tail of [`sqlite_first_cell`] so the JSON handling is testable
/// without a sqlite3 binary: `[{"cwd": "/x"}]` → `Some("/x")`.
fn parse_first_cell(success: bool, stdout: &str) -> Option<String> {
    if !success {
        return None;
    }
    let rows = serde_json::from_str::<serde_json::Value>(stdout.trim()).ok()?;
    let row = rows.as_array()?.first()?.as_object()?;
    let value = row.values().next()?.as_str()?;
    (!value.is_empty()).then(|| value.to_string())
}

// ─── Launch ──────────────────────────────────────────────────────────

/// Side effects the resume launch depends on, extracted so tests can
/// script tmux errors without a real server (same seam as SpawnEnv).
pub(crate) trait ResumeEnv {
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String>;
    /// Raw `@agent-sidebar-spawn-layout` option value (same seam as
    /// [`crate::worktree::SpawnEnv`]); `None` = unset (default layout).
    fn spawn_layout(&self) -> Option<String>;
    /// Split `target_pane` so the new pane sits to its left at `percent`
    /// of its width (same seam as [`crate::worktree::SpawnEnv`]).
    fn split_window_left(
        &self,
        target_pane: &str,
        cwd: &str,
        percent: u32,
    ) -> Result<String, String>;
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String>;
    fn send_command(&self, target: &str, command: &str) -> Result<(), String>;
    fn kill_window(&self, window_id: &str) -> Result<(), String>;
}

struct RealResumeEnv;

impl ResumeEnv for RealResumeEnv {
    fn new_window(&self, session: &str, cwd: &str, name: &str) -> Result<(String, String), String> {
        crate::tmux::new_window(session, cwd, name)
    }
    fn spawn_layout(&self) -> Option<String> {
        crate::tmux::get_all_global_options()
            .get(crate::worktree::SPAWN_LAYOUT_OPTION)
            .cloned()
    }
    fn split_window_left(
        &self,
        target_pane: &str,
        cwd: &str,
        percent: u32,
    ) -> Result<String, String> {
        crate::tmux::split_window_left(target_pane, cwd, percent)
    }
    fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String> {
        crate::tmux::set_window_option(window, key, value)
    }
    fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
        crate::tmux::send_command(target, command)
    }
    fn kill_window(&self, window_id: &str) -> Result<(), String> {
        crate::tmux::kill_window(window_id)
    }
}

pub(crate) struct LaunchRequest<'a> {
    /// The sidebar's own tmux session (resolved from its `TMUX_PANE`) —
    /// the same session spawn uses.
    pub session: &'a str,
    pub cwd: &'a str,
    /// Window name from `slugify` of the item text, fallback `resume`.
    pub window_name: &'a str,
    /// The rebuilt command from [`resume_command`] — never item text.
    pub command: &'a str,
    pub agent: &'a str,
    pub session_id: &'a str,
}

/// Open a new tmux window in `cwd`, stamp its provenance, and type the
/// resume command into it (literal send-keys, then a separate Enter —
/// the established two-call pattern). Any failure past window creation
/// kills the window, so a half-launched resume never leaves an orphan
/// shell behind.
pub(crate) fn launch_resume<E: ResumeEnv>(env: &E, req: &LaunchRequest) -> Result<(), String> {
    let (pane_id, window_id) = env
        .new_window(req.session, req.cwd, req.window_name)
        .map_err(|e| format!("tmux: {e}"))?;
    // Layout per `@agent-sidebar-spawn-layout`: `full` (the default)
    // hands the resumed agent the whole window; `split` keeps the
    // classic side-by-side (one-third agent pane left of a blank shell;
    // the sidebar hook adds its pane by geometry). A failed split
    // degrades to the single-pane layout — the resume command takes
    // over the original pane and the launch still succeeds.
    let agent_pane = match crate::worktree::spawn_layout(env.spawn_layout().as_deref()) {
        crate::worktree::SpawnLayout::Split => {
            match env.split_window_left(&pane_id, req.cwd, AGENT_PANE_PERCENT) {
                Ok(pane) => pane,
                Err(e) => {
                    crate::debug::log(&format!(
                        "resume: agent-pane split failed, single-pane fallback: {e}"
                    ));
                    pane_id.clone()
                }
            }
        }
        crate::worktree::SpawnLayout::Full => pane_id.clone(),
    };
    for (key, value) in [
        (RESUMED_OPTION, req.session_id),
        (RESUME_AGENT_OPTION, req.agent),
    ] {
        if let Err(e) = env.set_window_option(&window_id, key, value) {
            let _ = env.kill_window(&window_id);
            return Err(format!("tmux: failed to set {key}: {e}"));
        }
    }
    if let Err(e) = env.send_command(&agent_pane, req.command) {
        let _ = env.kill_window(&window_id);
        return Err(format!("tmux: {e}"));
    }
    Ok(())
}

/// The whole `r` tail for an already-resolved cwd: rebuild the command,
/// name the window from the item text, launch, and return the flash
/// banner text.
pub(crate) fn launch_with_cwd(
    session: &str,
    cwd: &str,
    item_text: &str,
    agent: &str,
    id: &str,
) -> Result<String, String> {
    launch_with_cwd_env(&RealResumeEnv, session, cwd, item_text, agent, id)
}

/// [`launch_with_cwd`] with the side effects injected, so the request
/// shaping (command rebuild, window naming, banner text) is testable.
pub(crate) fn launch_with_cwd_env<E: ResumeEnv>(
    env: &E,
    session: &str,
    cwd: &str,
    item_text: &str,
    agent: &str,
    id: &str,
) -> Result<String, String> {
    let command =
        resume_command(agent, id).ok_or_else(|| format!("resume for {agent} not supported yet"))?;
    let mut name = crate::worktree::slugify(item_text);
    if name.is_empty() {
        name = "resume".to_string();
    }
    launch_resume(
        env,
        &LaunchRequest {
            session,
            cwd,
            window_name: &name,
            command: &command,
            agent,
            session_id: id,
        },
    )?;
    Ok(format!("resumed {agent} {}", id8(id)))
}

/// The no-hint `r` path, run entirely on the popup-op worker thread so
/// a wedged sqlite probe (5s deadline) cannot stall the render loop:
/// resolve cwd from the parked sidecar then the agent's own store, and
/// launch. Returns the flash banner text on success or failure.
pub(crate) fn resolve_and_launch(
    session: &str,
    item_text: &str,
    dir: Option<&Path>,
    agent: &str,
    id: &str,
) -> Result<String, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let Some(cwd) = resolve_cwd(None, dir, agent, id, Path::new(&home)) else {
        return Err(no_cwd_error(id));
    };
    launch_with_cwd(session, &cwd, item_text, agent, id)
}

/// The fail-flash when no cwd can be resolved: never launch in `$HOME`
/// silently, and name the escape hatches.
pub(crate) fn no_cwd_error(id: &str) -> String {
    format!("no cwd for {id} — append \"@ /path\" or park the pane")
}

/// First 8 chars of an id for flash banners (`resumed claude f4ac0121`).
fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── recognizer: claude ──────────────────────────────────────────

    #[test]
    fn recognizes_claude_resume_with_long_flag() {
        let r = recognize_resume("claude --resume f4ac0121-58e7-4d9c-a5c6-e0db65c6c49e").unwrap();
        assert_eq!(r.agent, "claude");
        assert_eq!(r.id, "f4ac0121-58e7-4d9c-a5c6-e0db65c6c49e");
        assert_eq!(r.cwd_hint, None);
    }

    #[test]
    fn recognizes_claude_resume_with_short_flag_and_uppercase_id() {
        // Matched case-insensitively; rebuilt lowercase.
        let r = recognize_resume("claude -r F4AC0121-58E7").unwrap();
        assert_eq!(r.id, "f4ac0121-58e7");
    }

    #[test]
    fn claude_flag_may_sit_anywhere_and_id_must_follow_it() {
        let r = recognize_resume("claude --model opus --resume f4ac0121-58e7").unwrap();
        assert_eq!(r.id, "f4ac0121-58e7");
    }

    #[test]
    fn claude_rejects_short_or_non_hex_ids() {
        assert!(
            recognize_resume("claude --resume f4ac").is_none(),
            "< 8 chars"
        );
        assert!(
            recognize_resume("claude --resume g4ac0121-58e7").is_none(),
            "not hex"
        );
        assert!(recognize_resume("claude --resume").is_none(), "missing id");
        assert!(
            recognize_resume("claude --resume --model opus").is_none(),
            "another flag is not an id"
        );
    }

    // ─── recognizer: codex ───────────────────────────────────────────

    #[test]
    fn recognizes_codex_resume_with_uuid() {
        let r = recognize_resume("codex resume 4e71c60a-de52-4de1-9f2e-1e3eb9e30e12").unwrap();
        assert_eq!(r.agent, "codex");
        assert_eq!(r.id, "4e71c60a-de52-4de1-9f2e-1e3eb9e30e12");
    }

    #[test]
    fn recognizes_codex_resume_with_thread_name() {
        let r = recognize_resume("codex resume fix.the_login_flow").unwrap();
        assert_eq!(
            r.id, "fix.the_login_flow",
            "thread names preserved verbatim"
        );
    }

    #[test]
    fn codex_rejects_dash_leading_ids_and_other_subcommands() {
        assert!(
            recognize_resume("codex resume -x").is_none(),
            "-leading is a flag"
        );
        assert!(
            recognize_resume("codex exec abc123").is_none(),
            "no half-matches"
        );
        assert!(recognize_resume("codex resume").is_none(), "missing id");
        assert!(
            recognize_resume("codex resume --flag abc123").is_none(),
            "a -leading token is never the id"
        );
        // The id is the next token, full stop: extra trailing tokens are
        // just more flags, and "has" is a valid thread name.
        assert_eq!(
            recognize_resume("codex resume has space").unwrap().id,
            "has"
        );
    }

    // ─── recognizer: opencode ────────────────────────────────────────

    #[test]
    fn recognizes_opencode_resume_with_mixed_case_id() {
        let r = recognize_resume("opencode -s ses_e1eb53dd9ffemw0k3treOa3PBO").unwrap();
        assert_eq!(r.agent, "opencode");
        assert_eq!(
            r.id, "ses_e1eb53dd9ffemw0k3treOa3PBO",
            "mixed-case ids preserved verbatim, never lowercased"
        );
    }

    #[test]
    fn recognizes_opencode_long_form() {
        let r = recognize_resume("opencode --session ses_abc123").unwrap();
        assert_eq!(r.id, "ses_abc123");
    }

    #[test]
    fn opencode_id_is_case_sensitive() {
        assert!(recognize_resume("opencode -s SES_abc123").is_none());
        assert!(recognize_resume("opencode -s ses_").is_none(), "empty body");
    }

    // ─── recognizer: declines and malformed shapes ───────────────────

    #[test]
    fn antigravity_declines_and_stays_a_plain_todo() {
        assert!(recognize_resume("agy --resume something").is_none());
        assert!(recognize_resume("antigravity resume something").is_none());
    }

    #[test]
    fn non_agent_first_tokens_are_not_resume_commands() {
        assert!(recognize_resume("echo claude --resume f4ac0121-58e7").is_none());
        assert!(recognize_resume("review the auth flow").is_none());
        assert!(recognize_resume("").is_none());
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let r = recognize_resume("  claude --resume f4ac0121-58e7  ").unwrap();
        assert_eq!(r.id, "f4ac0121-58e7");
    }

    // ─── recognizer: `@ <path>` hint ─────────────────────────────────

    #[test]
    fn extracts_a_trailing_cwd_hint() {
        let r = recognize_resume("claude --resume f4ac0121-58e7 @ ~/Projects/wisy-cloud").unwrap();
        assert_eq!(r.id, "f4ac0121-58e7");
        assert_eq!(
            r.cwd_hint.as_deref(),
            Some(Path::new("~/Projects/wisy-cloud"))
        );
    }

    #[test]
    fn hint_accepts_tilde_absolute_relative_and_env_starts() {
        for path in ["~/x", "/abs/x", "./rel", "$HOME/x"] {
            let text = format!("codex resume thread.name @ {path}");
            let r = recognize_resume(&text).unwrap();
            assert_eq!(r.cwd_hint.as_deref(), Some(Path::new(path)), "hint {path}");
        }
    }

    #[test]
    fn a_non_path_or_spaced_suffix_is_not_a_hint() {
        let r = recognize_resume("claude --resume f4ac0121-58e7 @ fix the login").unwrap();
        assert_eq!(r.cwd_hint, None);
        let r = recognize_resume("claude --resume f4ac0121-58e7 @ not/a hint").unwrap();
        assert_eq!(r.cwd_hint, None);
    }

    #[test]
    fn the_last_valid_hint_occurrence_wins() {
        let r = recognize_resume("claude --resume f4ac0121-58e7 note @ staging @ /repo").unwrap();
        assert_eq!(r.id, "f4ac0121-58e7");
        assert_eq!(r.cwd_hint.as_deref(), Some(Path::new("/repo")));
    }

    // ─── resume_command ──────────────────────────────────────────────

    #[test]
    fn resume_command_rebuilds_canonical_forms() {
        assert_eq!(
            resume_command("claude", "f4ac0121-58e7").as_deref(),
            Some("claude --resume f4ac0121-58e7")
        );
        assert_eq!(
            resume_command("codex", "fix.the_login").as_deref(),
            Some("codex resume fix.the_login")
        );
        assert_eq!(
            resume_command("opencode", "ses_e1ebOa3PBO").as_deref(),
            Some("opencode -s ses_e1ebOa3PBO")
        );
        assert_eq!(resume_command("antigravity", "x"), None);
    }

    // ─── expand_home ─────────────────────────────────────────────────

    #[test]
    fn expand_home_rewrites_only_a_leading_tilde() {
        assert_eq!(
            expand_home(Path::new("~/x"), Some("/home/u")).as_deref(),
            Some("/home/u/x")
        );
        assert_eq!(
            expand_home(Path::new("~"), Some("/home/u")).as_deref(),
            Some("/home/u")
        );
        assert_eq!(
            expand_home(Path::new("/abs"), Some("/home/u")).as_deref(),
            Some("/abs")
        );
        assert_eq!(
            expand_home(Path::new("./rel"), None).as_deref(),
            Some("./rel")
        );
        assert_eq!(
            expand_home(Path::new("~"), None),
            None,
            "~ with no HOME is no cwd"
        );
        assert_eq!(expand_home(Path::new("~/x"), None), None);
    }

    // ─── claude store probe ──────────────────────────────────────────

    #[test]
    fn claude_probe_finds_the_matching_session_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("1207186.1fedc041.key"),
            "not json, just the key material",
        )
        .unwrap();
        std::fs::write(
            dir.join("1207186.json"),
            r#"{"pid":1207186,"sessionId":"263677c8-b474-4d9c-a5c6-e0db65c6c49e","cwd":"/home/u/wisy-mcp","startedAt":1794786512991}"#,
        )
        .unwrap();

        let cwd = probe_cwd("claude", "263677c8-b474-4d9c-a5c6-e0db65c6c49e", tmp.path()).unwrap();
        assert_eq!(cwd, "/home/u/wisy-mcp");
        // Case-insensitive match: the recognizer lowercases claude ids.
        let cwd = probe_cwd("claude", "263677C8-B474-4D9C-A5C6-E0DB65C6C49E", tmp.path()).unwrap();
        assert_eq!(cwd, "/home/u/wisy-mcp");
    }

    #[test]
    fn claude_probe_degrades_to_none_on_missing_store_or_id() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(probe_cwd("claude", "263677c8-b474", tmp.path()), None);
        let dir = tmp.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("1.json"),
            r#"{"sessionId":"263677c8-b474-4d9c-a5c6-e0db65c6c49e","cwd":"/x"}"#,
        )
        .unwrap();
        assert_eq!(probe_cwd("claude", "other-id-here-999", tmp.path()), None);
    }

    #[test]
    fn claude_probe_skips_unparseable_and_cwdless_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.json"), "not json").unwrap();
        std::fs::write(
            dir.join("nocwd.json"),
            r#"{"sessionId":"aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("good.json"),
            r#"{"sessionId":"aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e","cwd":"/good"}"#,
        )
        .unwrap();
        assert_eq!(
            probe_cwd("claude", "aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e", tmp.path()).as_deref(),
            Some("/good")
        );
    }

    #[test]
    fn projects_store_recovers_ended_sessions() {
        // sessions/ is pid-keyed and holds only running sessions; the
        // transcript store survives the session ending. Its leading
        // lines are metadata without a cwd — the probe must scan past
        // them (real file shape: last-prompt, mode, permission-mode…).
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join(".claude")
            .join("projects")
            .join("-home-greg-brandt-Projects-greg-wisy-ai-intel-eng");
        std::fs::create_dir_all(&dir).unwrap();
        let id = "dd331ab3-83c8-4c07-a44e-118b4dbdfb61";
        std::fs::write(
            dir.join(format!("{id}.jsonl")),
            concat!(
                r#"{"type":"last-prompt","leafUuid":"9eb110f2","sessionId":"dd331ab3-83c8-4c07-a44e-118b4dbdfb61"}"#,
                "\n",
                r#"{"type":"mode","mode":"normal","sessionId":"dd331ab3-83c8-4c07-a44e-118b4dbdfb61"}"#,
                "\n",
                r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"dd331ab3-83c8-4c07-a44e-118b4dbdfb61"}"#,
                "\n",
                r#"{"parentUuid":null,"cwd":"/home/greg-brandt/Projects/greg-wisy-ai/intel-eng","sessionId":"dd331ab3-83c8-4c07-a44e-118b4dbdfb61"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(
            probe_cwd("claude", id, tmp.path()).as_deref(),
            Some("/home/greg-brandt/Projects/greg-wisy-ai/intel-eng")
        );
    }

    #[test]
    fn the_live_sessions_store_wins_over_the_projects_store() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join(".claude").join("sessions");
        let projects = tmp
            .path()
            .join(".claude")
            .join("projects")
            .join("-home-u-stale");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        let id = "aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e";
        std::fs::write(
            sessions.join("1.json"),
            format!(r#"{{"sessionId":"{id}","cwd":"/live"}}"#),
        )
        .unwrap();
        std::fs::write(
            projects.join(format!("{id}.jsonl")),
            format!(r#"{{"cwd":"/stale","sessionId":"{id}"}}"#),
        )
        .unwrap();
        assert_eq!(
            probe_cwd("claude", id, tmp.path()).as_deref(),
            Some("/live")
        );
    }

    #[test]
    fn projects_probe_without_a_transcript_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        // A projects dir exists, but no transcript for this id.
        std::fs::create_dir_all(
            tmp.path()
                .join(".claude")
                .join("projects")
                .join("-home-u-other"),
        )
        .unwrap();
        assert_eq!(
            probe_cwd("claude", "bbbbbbbb-b474-4d9c-a5c6-e0db65c6c49e", tmp.path()),
            None
        );
    }

    #[test]
    fn projects_probe_skips_an_unreadable_transcript() {
        // One candidate directory's transcript cannot be opened (the
        // session may belong to another user after a shared-host move).
        // The walk used to abort on the first such open — and since the
        // read_dir order is not deterministic, the resume cwd hint
        // flapped between None and the cwd (audit 2026-11-16). When the
        // probe runs privileged the chmod is ignored, but then the
        // unreadable transcript is readable and carries no cwd, so the
        // assertion holds either way.
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join(".claude").join("projects");
        let unreadable = projects.join("-home-u-locked");
        let readable = projects.join("-home-u-open");
        std::fs::create_dir_all(&unreadable).unwrap();
        std::fs::create_dir_all(&readable).unwrap();
        let id = "cccccccc-b474-4d9c-a5c6-e0db65c6c49e";
        std::fs::write(unreadable.join(format!("{id}.jsonl")), "").unwrap();
        std::fs::write(
            readable.join(format!("{id}.jsonl")),
            format!(r#"{{"cwd":"/recovered","sessionId":"{id}"}}"#),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            unreadable.join(format!("{id}.jsonl")),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        assert_eq!(
            probe_cwd("claude", id, tmp.path()).as_deref(),
            Some("/recovered")
        );
    }

    // ─── sqlite probe parsing + dispatch ─────────────────────────────

    #[test]
    fn parse_first_cell_reads_the_single_column() {
        assert_eq!(
            parse_first_cell(true, r#"[{"cwd":"/x"}]"#).as_deref(),
            Some("/x")
        );
        assert!(
            parse_first_cell(true, "").is_none(),
            "sqlite3 prints nothing for empty results"
        );
        assert!(
            parse_first_cell(false, r#"[{"cwd":"/x"}]"#).is_none(),
            "non-zero exit is no cwd"
        );
        assert!(parse_first_cell(true, "not json").is_none());
        assert!(parse_first_cell(true, r#"[{}]"#).is_none());
        assert!(
            parse_first_cell(true, r#"[{"cwd":""}]"#).is_none(),
            "empty cwd is no cwd"
        );
    }

    #[test]
    fn probes_without_a_store_are_none_without_spawning_sqlite() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(probe_cwd("codex", "4e71c60a-de52", tmp.path()), None);
        assert_eq!(probe_cwd("opencode", "ses_abc123", tmp.path()), None);
        assert_eq!(probe_cwd("antigravity", "whatever", tmp.path()), None);
    }

    #[test]
    fn sql_literal_escapes_single_quotes() {
        assert_eq!(sql_literal("o'brien"), "o''brien");
    }

    // ─── parked.json sidecar read ────────────────────────────────────

    #[test]
    fn parked_cwd_finds_the_matching_entry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(PARKED_FILE),
            r#"[{"agent":"claude","session_id":"f4ac0121-58e7","cwd":"/repo","branch":null,"title":"fix login","prompt":null,"parked_at":100,"origin":"manual"}]"#,
        )
        .unwrap();
        assert_eq!(
            parked_cwd(Some(tmp.path()), "claude", "f4ac0121-58e7").as_deref(),
            Some("/repo")
        );
        assert_eq!(parked_cwd(Some(tmp.path()), "codex", "f4ac0121-58e7"), None);
    }

    #[test]
    fn parked_cwd_is_none_without_a_readable_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(parked_cwd(Some(tmp.path()), "claude", "x"), None);
        assert_eq!(parked_cwd(None, "claude", "x"), None);
        std::fs::write(tmp.path().join(PARKED_FILE), "not json").unwrap();
        assert_eq!(parked_cwd(Some(tmp.path()), "claude", "x"), None);
    }

    // ─── resolve order ───────────────────────────────────────────────

    #[test]
    fn resolve_cwd_prefers_the_hint_then_sidecar_then_store() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".claude").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("1.json"),
            r#"{"sessionId":"aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e","cwd":"/from-store"}"#,
        )
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            state.path().join(PARKED_FILE),
            r#"[{"agent":"claude","session_id":"aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e","cwd":"/from-parked","parked_at":1,"origin":"manual"}]"#,
        )
        .unwrap();
        let id = "aaaaaaaa-b474-4d9c-a5c6-e0db65c6c49e";

        assert_eq!(
            resolve_cwd(
                Some(Path::new("~/hint")),
                Some(state.path()),
                "claude",
                id,
                tmp.path()
            )
            .as_deref(),
            Some(format!("{}/hint", tmp.path().display())).as_deref(),
            "the hint wins over everything"
        );
        assert_eq!(
            resolve_cwd(None, Some(state.path()), "claude", id, tmp.path()).as_deref(),
            Some("/from-parked"),
            "the sidecar beats the store"
        );
        assert_eq!(
            resolve_cwd(None, None, "claude", id, tmp.path()).as_deref(),
            Some("/from-store"),
            "the store is the last probe"
        );
        assert_eq!(
            resolve_cwd(None, None, "claude", "no-such-id-1234", tmp.path()),
            None
        );
    }

    // ─── launch flow (fake env) ──────────────────────────────────────

    struct FakeEnv {
        calls: std::cell::RefCell<Vec<String>>,
        fail_set_window_option: bool,
        fail_send_command: bool,
        fail_split: bool,
        /// Raw `@agent-sidebar-spawn-layout` value (`None` = unset,
        /// which resolves to the default `full` layout).
        spawn_layout: Option<String>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                calls: std::cell::RefCell::new(Vec::new()),
                fail_set_window_option: false,
                fail_send_command: false,
                fail_split: false,
                spawn_layout: None,
            }
        }
        fn log(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl ResumeEnv for FakeEnv {
        fn new_window(
            &self,
            session: &str,
            cwd: &str,
            name: &str,
        ) -> Result<(String, String), String> {
            self.log(format!("new_window({session},{cwd},{name})"));
            Ok(("%9".into(), "@9".into()))
        }
        fn spawn_layout(&self) -> Option<String> {
            self.spawn_layout.clone()
        }
        fn split_window_left(
            &self,
            target_pane: &str,
            cwd: &str,
            percent: u32,
        ) -> Result<String, String> {
            self.log(format!("split_window_left({target_pane},{cwd},{percent})"));
            if self.fail_split {
                Err("split failed".into())
            } else {
                Ok("%8".into())
            }
        }
        fn set_window_option(&self, window: &str, key: &str, value: &str) -> Result<(), String> {
            self.log(format!("set_window_option({window},{key}={value})"));
            if self.fail_set_window_option {
                Err("set failed".into())
            } else {
                Ok(())
            }
        }
        fn send_command(&self, target: &str, command: &str) -> Result<(), String> {
            self.log(format!("send_command({target},{command})"));
            if self.fail_send_command {
                Err("send failed".into())
            } else {
                Ok(())
            }
        }
        fn kill_window(&self, window_id: &str) -> Result<(), String> {
            self.log(format!("kill_window({window_id})"));
            Ok(())
        }
    }

    fn launch_req<'a>(cwd: &'a str) -> LaunchRequest<'a> {
        LaunchRequest {
            session: "sess",
            cwd,
            window_name: "claude-resume-f4ac0121",
            command: "claude --resume f4ac0121",
            agent: "claude",
            session_id: "f4ac0121-58e7",
        }
    }

    #[test]
    fn launch_stamps_provenance_then_types_the_command() {
        let env = FakeEnv::new();
        launch_resume(&env, &launch_req("/repo")).unwrap();
        let calls = env.calls();
        assert_eq!(
            calls,
            vec![
                "new_window(sess,/repo,claude-resume-f4ac0121)".to_string(),
                // Default `full` layout: no split, the resume command
                // takes over the whole window.
                format!("set_window_option(@9,{RESUMED_OPTION}=f4ac0121-58e7)"),
                format!("set_window_option(@9,{RESUME_AGENT_OPTION}=claude)"),
                "send_command(%9,claude --resume f4ac0121)".to_string(),
            ]
        );
    }

    #[test]
    fn split_layout_types_the_command_into_the_split_pane() {
        let env = FakeEnv {
            spawn_layout: Some("split".into()),
            ..FakeEnv::new()
        };
        launch_resume(&env, &launch_req("/repo")).unwrap();
        let calls = env.calls();
        assert!(calls.contains(&"split_window_left(%9,/repo,33)".to_string()));
        assert!(
            calls.contains(&"send_command(%8,claude --resume f4ac0121)".to_string()),
            "resume command goes to the split agent pane: {calls:?}"
        );
    }

    #[test]
    fn split_layout_degrades_to_single_pane_when_the_split_fails() {
        let env = FakeEnv {
            spawn_layout: Some("split".into()),
            fail_split: true,
            ..FakeEnv::new()
        };
        launch_resume(&env, &launch_req("/repo")).expect("split failure must not fail the launch");
        let calls = env.calls();
        assert!(calls.contains(&"split_window_left(%9,/repo,33)".to_string()));
        assert!(
            calls.contains(&"send_command(%9,claude --resume f4ac0121)".to_string()),
            "resume command takes over the original pane on split failure: {calls:?}"
        );
        assert!(
            !calls.contains(&"kill_window(@9)".to_string()),
            "a usable window must not be rolled back: {calls:?}"
        );
    }

    #[test]
    fn launch_rolls_the_window_back_when_a_marker_write_fails() {
        let env = FakeEnv {
            fail_set_window_option: true,
            ..FakeEnv::new()
        };
        let err = launch_resume(&env, &launch_req("/repo")).unwrap_err();
        assert!(
            err.contains(RESUMED_OPTION),
            "error names the failed marker: {err}"
        );
        assert!(env.calls().contains(&"kill_window(@9)".to_string()));
    }

    #[test]
    fn launch_rolls_the_window_back_when_send_fails() {
        let env = FakeEnv {
            fail_send_command: true,
            ..FakeEnv::new()
        };
        let err = launch_resume(&env, &launch_req("/repo")).unwrap_err();
        assert!(err.contains("send failed"), "error: {err}");
        assert!(env.calls().contains(&"kill_window(@9)".to_string()));
    }

    #[test]
    fn launch_with_cwd_names_the_window_and_returns_the_banner() {
        let env = FakeEnv::new();
        let flash = launch_with_cwd_env(
            &env,
            "sess",
            "/repo",
            "claude --resume f4ac0121-58e7 fix the login",
            "claude",
            "f4ac0121-58e7",
        )
        .unwrap();
        assert_eq!(flash, "resumed claude f4ac0121");
        let calls = env.calls();
        assert!(
            calls[0]
                .starts_with("new_window(sess,/repo,claude-resume-f4ac0121-58e7-fix-the-login)"),
            "the window is named from the item text: {calls:?}"
        );
        assert!(
            calls
                .last()
                .unwrap()
                .starts_with("send_command(%9,claude --resume f4ac0121-58e7)"),
            "the command is rebuilt, never item text: {calls:?}"
        );
    }

    #[test]
    fn launch_with_cwd_falls_back_to_resume_when_the_slug_empties() {
        let env = FakeEnv::new();
        launch_with_cwd_env(&env, "sess", "/repo", "///", "claude", "f4ac0121-58e7").unwrap();
        assert!(env.calls()[0].starts_with("new_window(sess,/repo,resume)"));
    }

    #[test]
    fn launch_with_cwd_declines_agents_without_a_resume_grammar() {
        let env = FakeEnv::new();
        let err =
            launch_with_cwd_env(&env, "sess", "/repo", "agy", "antigravity", "x").unwrap_err();
        assert_eq!(err, "resume for antigravity not supported yet");
        assert!(env.calls().is_empty(), "nothing launched");
    }

    #[test]
    fn id8_truncates_for_the_banner() {
        assert_eq!(id8("f4ac0121-58e7"), "f4ac0121");
        assert_eq!(id8("ses_ab"), "ses_ab");
    }

    #[test]
    fn no_cwd_error_names_both_escape_hatches() {
        let err = no_cwd_error("f4ac0121-58e7");
        assert!(err.contains("no cwd for f4ac0121-58e7"));
        assert!(err.contains("@ /path"));
        assert!(err.contains("park the pane"));
    }
}
