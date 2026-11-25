//! Pane resolution for hook events that arrive without `$TMUX_PANE`.
//!
//! The parent CLI always runs inside the tmux pane and exports
//! `TMUX_PANE`, which is how every hook event has been routed so far.
//! But Claude Code also fires hooks from processes that deliberately
//! scrub the tmux environment when spawning managed PTYs:
//!
//! - `bg-pty-host` continuation forks — the daemon process that keeps
//!   working after the foreground CLI hands the session off
//!   (`continued-in` in the transcript; observed live 2026-11-20 on a
//!   we-1684 worktree pane whose row stayed stuck in
//!   `waiting`/`agent_completed` for the whole continuation).
//! - async subagent hosts (`--agent claude --inherit-permission-mode`),
//!   spawned the same way.
//!
//! Those events used to be dropped outright ("TMUX_PANE not set"), so a
//! pane whose session continued in the background never heard from it
//! again. The fallback re-resolves the pane from the payload's session
//! id, which every Claude hook payload carries:
//!
//! 1. Exact binding: the pane whose `@pane_session_id` equals the
//!    payload's session id. Trusted — the pane claims this session.
//! 2. Subagent entry: the pane whose `@pane_subagent_routes` list
//!    contains an entry ending in `:<session id>` (registered by
//!    `SubagentStart`, which fires from the parent and does have
//!    `TMUX_PANE`; `@pane_subagents` is matched as a legacy fallback for
//!    lists written before the routing split). A guess — the child
//!    shares the parent's pane but must not rewrite its identity, so
//!    these routes run with `TMUX_AGENT_SIDEBAR_BIND_SESSION=0`, the
//!    same suppression the OpenCode bridge applies to guessed panes.
//! 3. Process ancestry: when the session id matches nothing (the
//!    `continued-in` handoff re-keys the pane's conversation to a fresh
//!    session id and never re-fires a pane-scoped hook, so no binding or
//!    route can know it), the hook's own process tree is walked for an
//!    ancestor whose environment still carries `TMUX_PANE` — the
//!    continuation daemon (`claude daemon run`) inherits it from the
//!    pane's CLI, while the hosts it spawns scrub it. The ancestor's
//!    `TMUX` socket must agree with the socket this hook process would
//!    talk to, so a hook running against a scoped server never adopts a
//!    pane from the user's live server. Claude only: the other agents'
//!    pane-less events are bridge-resolved or always carry the pane, and
//!    an unmatched opencode event must keep dropping rather than land on
//!    the server's pane.
//!
//! Zero matches or an ambiguous match (more than one pane) drops the
//! event: fail-closed, mirroring the bridge's committed-guess doctrine.

use crate::tmux;

/// How the pane for a hook event was determined. Only [`PaneRoute::Guessed`]
/// routes suppress identity writes on the target pane.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PaneRoute {
    /// `$TMUX_PANE` was set by the agent process itself.
    Env,
    /// Resolved via an exact `@pane_session_id` match.
    Bound,
    /// Resolved by walking the hook process's ancestry to a process that
    /// still carries `TMUX_PANE` — the continuation daemon inherited it
    /// from the pane's CLI. The session took over this pane's work
    /// without firing any pane-scoped hook, so the route is allowed to
    /// adopt the session (rewrite `@pane_session_id`), which is what
    /// re-keys the pane to the continued conversation.
    Inherited,
    /// Resolved via a subagent-list entry — a guess that must not
    /// rewrite the pane's session identity.
    Guessed,
}

/// One pane's routing-relevant identity, parsed from a single
/// `list-panes -a` spawn.
struct PaneRow {
    pane: String,
    agent: String,
    session: String,
    subagents: String,
    routes: String,
}

/// Resolve the pane a hook event belongs to.
///
/// `TMUX_PANE` wins when present (the parent CLI always has it). Without
/// it, the payload's session id is matched against the panes' recorded
/// identities, then the process tree is walked (see the module docs). An
/// empty return means "drop the event" — either no session id to match
/// on or no unambiguous pane.
pub(crate) fn resolve_hook_pane(agent: &str, session_id: Option<&str>) -> (String, PaneRoute) {
    if let Ok(pane) = std::env::var("TMUX_PANE")
        && !pane.is_empty()
    {
        return (pane, PaneRoute::Env);
    }
    if let Some(sid) = session_id.filter(|sid| !sid.is_empty()) {
        let rows = query_pane_rows();
        if let Some(pane) = pick_bound(&rows, agent, sid) {
            return (pane.to_string(), PaneRoute::Bound);
        }
        if let Some(pane) = pick_subagent(&rows, agent, sid) {
            return (pane.to_string(), PaneRoute::Guessed);
        }
        // The continuation fallback: only Claude re-keys conversations
        // mid-pane (see the module docs).
        if agent == crate::tmux::CLAUDE_AGENT
            && let Some(pane) = inherited_tmux_pane()
        {
            return (pane, PaneRoute::Inherited);
        }
    }
    (String::new(), PaneRoute::Guessed)
}

fn query_pane_rows() -> Vec<PaneRow> {
    let Some(output) = tmux::run_tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}|#{@pane_agent}|#{@pane_session_id}|#{@pane_subagents}|#{@pane_subagent_routes}",
    ]) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(5, '|');
            let pane = fields.next()?.to_string();
            if pane.is_empty() || !pane.starts_with('%') {
                return None;
            }
            Some(PaneRow {
                pane,
                agent: fields.next().unwrap_or_default().to_string(),
                session: fields.next().unwrap_or_default().to_string(),
                subagents: fields.next().unwrap_or_default().to_string(),
                routes: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

// ─── process-ancestry fallback ──────────────────────────────────────────────

/// How far the ancestry walk ascends before giving up. The continuation
/// daemon sits a handful of levels above the hook process
/// (hook → runtime → bg-pty-host/bg-spare → daemon), with slack for
/// launcher shims; every level costs two tiny `/proc` reads and the walk
/// only runs when session matching failed.
const ANCESTRY_WALK_MAX_DEPTH: u32 = 10;

/// The pane a `TMUX_PANE`-scrubbed hook process inherited, via its
/// process tree.
///
/// The hook's ancestors that still carry `TMUX_PANE` are exactly the
/// ones spawned before the scrub — ultimately the pane's own CLI. A hit
/// is only accepted when the ancestor's `TMUX` socket agrees with the
/// socket this process would talk to: a scoped-server test (or any
/// isolated-socket setup) must never adopt a pane from a differently-
/// socketed ancestor further up the tree.
fn inherited_tmux_pane() -> Option<String> {
    let own_socket = own_tmux_socket()?;
    walk_to_pane(std::process::id(), &own_socket)
}

/// [`inherited_tmux_pane`] from an explicit start pid against an explicit
/// socket — the walk itself, split out so tests can drive it through a
/// spawned process chain instead of the test harness's own ancestors.
/// The start process's own environment is examined first (in production
/// it never carries `TMUX_PANE` — the `Env` route would have won — so
/// the check is free; it makes the walk a plain "scan the chain
/// starting here").
fn walk_to_pane(start_pid: u32, own_socket: &str) -> Option<String> {
    let mut pid = Some(start_pid);
    for _ in 0..ANCESTRY_WALK_MAX_DEPTH {
        let current = pid?;
        if current <= 1 {
            return None;
        }
        let env = proc_environ(current)?;
        let pane = env_value(&env, "TMUX_PANE").filter(|pane| !pane.is_empty());
        let matched = match pane {
            // `TMUX_PANE` without a `TMUX` socket cannot be validated —
            // skip rather than trust an orphaned marker.
            Some(pane) => env_value(&env, "TMUX").and_then(|tmux| {
                (tmux.split(',').next() == Some(own_socket)).then(|| pane.to_string())
            }),
            None => None,
        };
        if matched.is_some() {
            return matched;
        }
        pid = proc_parent(current);
    }
    None
}

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find_map(|(k, v)| (k == key).then_some(v.as_str()))
}

/// The tmux socket path this process's `tmux` CLI calls would resolve
/// to: the socket embedded in `$TMUX` when inside tmux, otherwise
/// `$TMUX_TMPDIR` (or `/tmp/tmux-<uid>`) plus the default socket name —
/// the same resolution tmux's client performs.
fn own_tmux_socket() -> Option<String> {
    if let Ok(tmux) = std::env::var("TMUX")
        && !tmux.is_empty()
    {
        return Some(tmux.split(',').next().unwrap_or_default().to_string());
    }
    let tmpdir = match std::env::var("TMUX_TMPDIR") {
        Ok(dir) if !dir.is_empty() => dir,
        _ => format!("/tmp/tmux-{}", real_uid()?),
    };
    Some(format!("{tmpdir}/default"))
}

/// Real uid from `/proc/self/status` — the same source tmux's
/// `/tmp/tmux-$UID` default derives from. `None` outside Linux (or when
/// the file is unreadable), which disables the ancestry fallback.
fn real_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("Uid:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The parent pid of `pid`, from `/proc/<pid>/stat`. The comm field may
/// contain spaces and parens, so parsing starts after the last `)`.
fn proc_parent(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    let mut fields = after_comm.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

/// The environment of `pid` as `(key, value)` pairs. Fails (→ `None`)
/// when the process is gone or the kernel refuses the read.
fn proc_environ(pid: u32) -> Option<Vec<(String, String)>> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        raw.split(|byte| *byte == 0)
            .filter_map(|entry| {
                let eq = entry.iter().position(|byte| *byte == b'=')?;
                let (key, value) = entry.split_at(eq);
                Some((
                    String::from_utf8_lossy(key).into_owned(),
                    String::from_utf8_lossy(&value[1..]).into_owned(),
                ))
            })
            .collect(),
    )
}

/// The pane that has already bound this exact session id — at most one
/// can exist, and a pane whose agent label differs belongs to another
/// agent's session even if the ids ever collided.
fn pick_bound<'a>(rows: &'a [PaneRow], agent: &str, sid: &str) -> Option<&'a str> {
    let mut matches = rows
        .iter()
        .filter(|row| row.agent == agent && row.session == sid);
    let pane = matches.next()?;
    matches.next().is_none().then_some(pane.pane.as_str())
}

/// The pane whose subagent route list registered this session id (async
/// subagent hosts run with a scrubbed environment; their `SubagentStart`
/// landed on the parent pane, which does know it). Matches the routing
/// list first and the liveness list as a legacy fallback — lists written
/// before the routing split only exist in `@pane_subagents`.
fn pick_subagent<'a>(rows: &'a [PaneRow], agent: &str, sid: &str) -> Option<&'a str> {
    let mut matches = rows.iter().filter(|row| {
        row.agent == agent
            && (subagent_list_contains(&row.routes, sid)
                || subagent_list_contains(&row.subagents, sid))
    });
    let pane = matches.next()?;
    matches.next().is_none().then_some(pane.pane.as_str())
}

/// Whether the `agent_type:agent_id` comma list contains an entry whose
/// id is exactly `sid`. Tolerates the leading comma the atomic append
/// leaves on a previously-unset option.
fn subagent_list_contains(subagents: &str, sid: &str) -> bool {
    subagents
        .split(',')
        .filter_map(|entry| entry.rsplit_once(':'))
        .any(|(_, id)| id == sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pane: &str, agent: &str, session: &str, subagents: &str) -> PaneRow {
        row_routes(pane, agent, session, subagents, "")
    }

    fn row_routes(
        pane: &str,
        agent: &str,
        session: &str,
        subagents: &str,
        routes: &str,
    ) -> PaneRow {
        PaneRow {
            pane: pane.to_string(),
            agent: agent.to_string(),
            session: session.to_string(),
            subagents: subagents.to_string(),
            routes: routes.to_string(),
        }
    }

    /// Spawn a detached `sleep` carrying a custom environment, so the
    /// ancestry walk can be driven against a known process chain instead
    /// of the test harness's own ancestors (which may or may not sit in
    /// a tmux pane depending on where the suite runs).
    ///
    /// Blocks until the child's `/proc/<pid>/environ` actually exposes
    /// `needle`: reading the environ of a just-exec'd process races the
    /// kernel's mm swap and intermittently returns empty bytes (observed
    /// ~15% of spawns). Production ancestors are seconds old, so the
    /// walk itself needs no such tolerance — this is purely a test
    /// fixture concern.
    fn spawn_with_env(env: &[(&str, &str)], needle: &str) -> u32 {
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("5");
        for (key, value) in env {
            cmd.env(key, value);
        }
        // Detach from the terminal plumbing; only the pid matters.
        let child = cmd
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let visible = std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle));
            if visible {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child {pid} environ never exposed {needle:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Keep the handle un-reaped for the walk's lifetime; the test
        // process exits right after and reaps everything.
        std::mem::forget(child);
        pid
    }

    #[test]
    fn pick_bound_matches_single_pane_with_same_agent() {
        let rows = vec![
            row("%1", "claude", "s-1", ""),
            row("%2", "codex", "s-2", ""),
        ];
        assert_eq!(pick_bound(&rows, "claude", "s-1"), Some("%1"));
    }

    #[test]
    fn pick_bound_rejects_agent_mismatch_and_unknown_session() {
        let rows = vec![
            row("%1", "claude", "s-1", ""),
            row("%2", "codex", "s-2", ""),
        ];
        // A session id bound under another agent's label must not be
        // claimed, and unknown ids match nothing.
        assert_eq!(pick_bound(&rows, "codex", "s-1"), None);
        assert_eq!(pick_bound(&rows, "claude", "s-9"), None);
    }

    #[test]
    fn pick_bound_drops_ambiguous_matches() {
        // Two panes bound to the same session id is already corrupted
        // state; routing to either would compound it.
        let rows = vec![
            row("%1", "claude", "s-1", ""),
            row("%2", "claude", "s-1", ""),
        ];
        assert_eq!(pick_bound(&rows, "claude", "s-1"), None);
    }

    #[test]
    fn pick_subagent_matches_registered_child() {
        let rows = vec![row("%1", "claude", "parent", ",Explore:sub-1,Plan:sub-2")];
        assert_eq!(pick_subagent(&rows, "claude", "sub-2"), Some("%1"));
    }

    #[test]
    fn pick_subagent_reads_the_routes_list() {
        // The routing copy is what survives the parent's Stop and the
        // child's SubagentStop; an entry only in `@pane_subagent_routes`
        // must still route.
        let rows = vec![row_routes("%1", "claude", "parent", "", ",Explore:sub-1")];
        assert_eq!(pick_subagent(&rows, "claude", "sub-1"), Some("%1"));
    }

    #[test]
    fn pick_subagent_keeps_the_legacy_list_as_fallback() {
        // Lists written before the routing split only exist in
        // `@pane_subagents`; an upgrade must not strand in-flight hosts.
        let rows = vec![row("%1", "claude", "parent", ",Explore:sub-1")];
        assert_eq!(pick_subagent(&rows, "claude", "sub-1"), Some("%1"));
    }

    #[test]
    fn pick_subagent_requires_exact_id_suffix_not_prefix() {
        let rows = vec![row_routes("%1", "claude", "parent", "", ",Explore:sub-1")];
        // "sub-1" must not match an id like "sub-11", and a bare prefix
        // without the entry separator must not match either.
        assert_eq!(pick_subagent(&rows, "claude", "sub-11"), None);
        assert_eq!(pick_subagent(&rows, "claude", "ub-1"), None);
    }

    #[test]
    fn pick_subagent_rejects_agent_mismatch_and_ambiguity() {
        let rows = vec![
            row_routes("%1", "claude", "p1", "", ",Explore:child"),
            row("%2", "codex", "p2", ",Explore:child"),
        ];
        assert_eq!(pick_subagent(&rows, "codex", "child"), Some("%2"));
        // The codex pane's registration must not count for claude, but
        // claude still resolves via its own pane.
        assert_eq!(pick_subagent(&rows, "claude", "child"), Some("%1"));
        // Two same-agent panes registering the same child id is
        // ambiguous state: route nowhere.
        let ambiguous = vec![
            row_routes("%1", "claude", "p1", "", ",Explore:child"),
            row("%2", "claude", "p2", ",Explore:child"),
        ];
        assert_eq!(pick_subagent(&ambiguous, "claude", "child"), None);
    }

    #[test]
    fn subagent_list_contains_tolerates_leading_comma_and_empty_entries() {
        assert!(subagent_list_contains(",Explore:sub-1", "sub-1"));
        assert!(subagent_list_contains("Explore:sub-1,Plan:sub-2", "sub-2"));
        assert!(!subagent_list_contains("", "sub-1"));
        assert!(!subagent_list_contains(",", "sub-1"));
        // An entry without a colon separator can never match.
        assert!(!subagent_list_contains("garbage", "garbage"));
    }

    // ─── ancestry walk ───────────────────────────────────────────────

    #[test]
    fn proc_parent_and_environ_read_a_spawned_child() {
        let child = spawn_with_env(&[("TMUX_WALK_MARKER", "yes")], "TMUX_WALK_MARKER=yes");
        assert_eq!(proc_parent(child), Some(std::process::id()));
        let env = proc_environ(child).expect("child environ must be readable");
        assert_eq!(env_value(&env, "TMUX_WALK_MARKER"), Some("yes"));
        assert_eq!(env_value(&env, "TMUX_NOPE"), None);
    }

    #[test]
    fn walk_finds_the_pane_on_a_matching_socket_ancestor() {
        // The child carries TMUX + TMUX_PANE for a socket the walk is
        // told is our own — the daemon-shaped hit.
        let child = spawn_with_env(
            &[
                ("TMUX", "/tmp/walk-test-socket,123,0"),
                ("TMUX_PANE", "%99"),
            ],
            "TMUX_PANE=%99",
        );
        let pane = walk_to_pane(child, "/tmp/walk-test-socket").expect("matching ancestor");
        assert_eq!(pane, "%99");
    }

    #[test]
    fn walk_rejects_ancestors_on_a_foreign_socket() {
        // The child's socket differs from the one the walk is told is
        // ours. Every real ancestor above the child (the test harness,
        // cargo, the invoking shell) either carries a different socket
        // or none, so the walk must give up rather than adopt the child's
        // pane. This is the guard that keeps scoped-server hooks from
        // landing on the live server's panes.
        let child = spawn_with_env(
            &[
                ("TMUX", "/tmp/walk-test-foreign-socket,123,0"),
                ("TMUX_PANE", "%99"),
            ],
            "TMUX_PANE=%99",
        );
        assert_eq!(walk_to_pane(child, "/tmp/walk-test-own-socket"), None);
    }

    #[test]
    fn walk_skips_a_pane_marker_without_a_socket_to_validate() {
        // `TMUX_PANE` without `TMUX` is an orphaned marker: no way to
        // prove it belongs to our server, so the walk must not trust it.
        // (Any real ancestor above the child carries no matching socket,
        // so the walk ends with None.)
        let child = spawn_with_env(&[("TMUX_PANE", "%99")], "TMUX_PANE=%99");
        assert_eq!(walk_to_pane(child, "/tmp/walk-test-own-socket"), None);
    }

    #[test]
    fn own_tmux_socket_prefers_the_embedded_tmux_socket() {
        // SAFETY: single-threaded test process env mutation (suite runs
        // with --test-threads=1, see the Makefile's test-ignored target).
        unsafe {
            let saved = std::env::var("TMUX").ok();
            std::env::set_var("TMUX", "/tmp/tmux-1000/default,15263,7");
            assert_eq!(own_tmux_socket().as_deref(), Some("/tmp/tmux-1000/default"));
            match saved {
                Some(v) => std::env::set_var("TMUX", v),
                None => std::env::remove_var("TMUX"),
            }
        }
    }
}
