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
//! 2. Subagent entry: the pane whose `@pane_subagents` list contains an
//!    entry ending in `:<session id>` (registered by `SubagentStart`,
//!    which fires from the parent and does have `TMUX_PANE`). A guess —
//!    the child shares the parent's pane but must not rewrite its
//!    identity, so these routes run with `TMUX_AGENT_SIDEBAR_BIND_SESSION=0`,
//!    the same suppression the OpenCode bridge applies to guessed panes.
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
}

/// Resolve the pane a hook event belongs to.
///
/// `TMUX_PANE` wins when present (the parent CLI always has it). Without
/// it, the payload's session id is matched against the panes' recorded
/// identities (see the module docs). An empty return means "drop the
/// event" — either no session id to match on or no unambiguous pane.
pub(crate) fn resolve_hook_pane(agent: &str, session_id: Option<&str>) -> (String, PaneRoute) {
    if let Ok(pane) = std::env::var("TMUX_PANE")
        && !pane.is_empty()
    {
        return (pane, PaneRoute::Env);
    }
    let Some(sid) = session_id.filter(|sid| !sid.is_empty()) else {
        return (String::new(), PaneRoute::Env);
    };
    let rows = query_pane_rows();
    if let Some(pane) = pick_bound(&rows, agent, sid) {
        return (pane.to_string(), PaneRoute::Bound);
    }
    match pick_subagent(&rows, agent, sid) {
        Some(pane) => (pane.to_string(), PaneRoute::Guessed),
        None => (String::new(), PaneRoute::Guessed),
    }
}

fn query_pane_rows() -> Vec<PaneRow> {
    let Some(output) = tmux::run_tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}|#{@pane_agent}|#{@pane_session_id}|#{@pane_subagents}",
    ]) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(4, '|');
            let pane = fields.next()?.to_string();
            if pane.is_empty() || !pane.starts_with('%') {
                return None;
            }
            Some(PaneRow {
                pane,
                agent: fields.next().unwrap_or_default().to_string(),
                session: fields.next().unwrap_or_default().to_string(),
                subagents: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
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

/// The pane whose subagent list registered this session id (async
/// subagent hosts run with a scrubbed environment; their `SubagentStart`
/// landed on the parent pane, which does know it).
fn pick_subagent<'a>(rows: &'a [PaneRow], agent: &str, sid: &str) -> Option<&'a str> {
    let mut matches = rows
        .iter()
        .filter(|row| row.agent == agent && subagent_list_contains(&row.subagents, sid));
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
        PaneRow {
            pane: pane.to_string(),
            agent: agent.to_string(),
            session: session.to_string(),
            subagents: subagents.to_string(),
        }
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
    fn pick_subagent_requires_exact_id_suffix_not_prefix() {
        let rows = vec![row("%1", "claude", "parent", ",Explore:sub-1")];
        // "sub-1" must not match an id like "sub-11", and a bare prefix
        // without the entry separator must not match either.
        assert_eq!(pick_subagent(&rows, "claude", "sub-11"), None);
        assert_eq!(pick_subagent(&rows, "claude", "ub-1"), None);
    }

    #[test]
    fn pick_subagent_rejects_agent_mismatch_and_ambiguity() {
        let rows = vec![
            row("%1", "claude", "p1", ",Explore:child"),
            row("%2", "codex", "p2", ",Explore:child"),
        ];
        assert_eq!(pick_subagent(&rows, "codex", "child"), Some("%2"));
        // The codex pane's registration must not count for claude, but
        // claude still resolves via its own pane.
        assert_eq!(pick_subagent(&rows, "claude", "child"), Some("%1"));
        // Two same-agent panes registering the same child id is
        // ambiguous state: route nowhere.
        let ambiguous = vec![
            row("%1", "claude", "p1", ",Explore:child"),
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
}
