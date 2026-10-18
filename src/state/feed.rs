use std::collections::HashMap;
use std::time::SystemTime;

use crate::activity::{self, FeedKind};
use crate::state::ScrollState;
use crate::tmux::AgentType;

/// One line of the cross-agent feed: a notable thing some agent did.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedEntry {
    /// `HH:MM` for display (local time of [`Self::epoch`]; the raw
    /// `HH:MM` for legacy lines).
    pub timestamp: String,
    /// Wall-clock epoch seconds — the sort key. Ordering by the display
    /// string would misplace entries across midnight (`"23:37"` from last
    /// night would sort above `"08:15"` from this morning). `0` for
    /// legacy lines whose date is unknown; they sort oldest.
    pub epoch: u64,
    /// Agent pane the entry came from.
    pub agent: AgentType,
    pub kind: FeedKind,
    pub label: String,
}

/// Cross-agent feed state: merged, filtered activity from every agent
/// pane's log, newest first. Replaces the old per-pane Activity tab — the
/// bottom panel is the "what happened everywhere" surface, while per-pane
/// detail stays in the agent rows and the panes themselves.
#[derive(Debug)]
pub struct FeedState {
    pub entries: Vec<FeedEntry>,
    pub scroll: ScrollState,
    pub max_entries: usize,
    /// Per-pane cache: last-seen log mtime → that file's feed entries.
    /// Lets the merge skip re-parsing logs that have not changed; panes
    /// that leave the snapshot are dropped by [`Self::retain_panes`].
    per_pane: HashMap<String, (Option<SystemTime>, Vec<FeedEntry>)>,
    /// Number of entries already presented to the user. `entries` is
    /// newest-first, so everything past this count is unseen and drives
    /// the badge on the tab title via [`Self::unseen_attention_count`].
    last_seen: usize,
}

impl FeedState {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll: ScrollState::default(),
            max_entries: 100,
            per_pane: HashMap::new(),
            last_seen: 0,
        }
    }

    /// Drop cached per-pane entries for panes no longer in the snapshot.
    pub(crate) fn retain_panes(&mut self, pane_ids: &[String]) {
        self.per_pane
            .retain(|pane_id, _| pane_ids.iter().any(|id| id == pane_id));
    }

    /// Re-read a pane's log when its mtime changed, refreshing that pane's
    /// cache slot. A missing log caches as `None` so a pane that just
    /// started (or whose agent died) does not re-stat every tick.
    pub(crate) fn refresh_pane(&mut self, pane_id: &str, agent: &AgentType) {
        let mtime = activity::log_mtime(pane_id);
        if let Some((cached_mtime, _)) = self.per_pane.get(pane_id)
            && *cached_mtime == mtime
        {
            return;
        }
        let entries = Self::read_pane(pane_id, agent);
        self.per_pane.insert(pane_id.to_string(), (mtime, entries));
    }

    /// Re-merge every pane's cached entries into the display list.
    pub(crate) fn rebuild(&mut self) {
        let mut merged: Vec<FeedEntry> = self
            .per_pane
            .values()
            .flat_map(|(_, entries)| entries.iter().cloned())
            .collect();
        // Epoch seconds order chronologically across days (the display
        // `HH:MM` string would not: it has no date). Legacy entries
        // (epoch 0) sort oldest. The stable sort keeps same-second
        // entries in pane iteration order.
        merged.sort_by(|a, b| b.epoch.cmp(&a.epoch));
        merged.truncate(self.max_entries);
        // The list shrank (log trim, pane removal): clamp so `last_seen`
        // can never point past the newest entry.
        if merged.len() < self.last_seen {
            self.last_seen = merged.len();
        }
        self.entries = merged;
    }

    /// Count attention-worthy entries the user has not laid eyes on yet.
    /// Routine work (edits, task churn) deliberately does not badge — the
    /// badge means "something needs you".
    pub fn unseen_attention_count(&self) -> usize {
        let unseen = self.entries.len().saturating_sub(self.last_seen);
        self.entries
            .iter()
            .take(unseen)
            .filter(|entry| entry.kind.is_attention())
            .count()
    }

    /// Record every entry currently in the list as seen. Called by the
    /// renderer while the feed tab is expanded and on screen.
    pub fn mark_seen(&mut self) {
        self.last_seen = self.entries.len();
    }

    /// Parse a pane's log into feed entries: classified notable events, or
    /// — when the pane did only routine work (shell, reads, searches) — a
    /// single collapsed `Routine` entry so the agent stays visible in the
    /// feed instead of silently vanishing behind the sparse filter. The
    /// collapsed entry is stamped with the newest dropped entry's epoch
    /// and display time; `read_activity_log` returns newest-first, so that
    /// is the first drop seen here.
    fn read_pane(pane_id: &str, agent: &AgentType) -> Vec<FeedEntry> {
        let mut entries = Vec::new();
        let mut routine_count = 0usize;
        let mut routine_stamp: Option<(u64, String)> = None;
        for entry in activity::read_activity_log(pane_id, 0) {
            if entry.tool == activity::TASK_RESET_MARKER {
                continue;
            }
            match FeedKind::classify(&entry.tool, &entry.label) {
                Some(kind) => entries.push(FeedEntry {
                    timestamp: entry.timestamp,
                    epoch: entry.epoch,
                    agent: agent.clone(),
                    kind,
                    label: entry.label,
                }),
                None => {
                    routine_count += 1;
                    if routine_stamp.is_none() {
                        routine_stamp = Some((entry.epoch, entry.timestamp));
                    }
                }
            }
        }
        if entries.is_empty() && routine_count > 0 {
            let (epoch, timestamp) = routine_stamp.unwrap();
            entries.push(FeedEntry {
                timestamp,
                epoch,
                agent: agent.clone(),
                kind: FeedKind::Routine,
                label: format!(
                    "{routine_count} tool call{}",
                    if routine_count == 1 { "" } else { "s" }
                ),
            });
        }
        entries
    }
}

impl Default for FeedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Deterministic in-day sort key for a zero-padded `HH:MM` string, so
    /// tests that rely on same-day ordering read naturally. Cross-day
    /// cases are tested with explicit epochs instead.
    fn ts_epoch(ts: &str) -> u64 {
        let mut it = ts.split(':');
        let h: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let m: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        h * 3600 + m * 60
    }

    fn entry(ts: &str, kind: FeedKind, label: &str) -> FeedEntry {
        FeedEntry {
            timestamp: ts.into(),
            epoch: ts_epoch(ts),
            agent: AgentType::Claude,
            kind,
            label: label.into(),
        }
    }

    // ─── read_pane ───────────────────────────────────────────────────

    #[test]
    fn read_pane_collapses_routine_only_log_into_one_entry() {
        let pane_id = "%FEED_ROUTINE_ONLY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            "10:00|Bash|cargo build\n10:01|Read|main.rs\n10:02|Grep|pattern\n",
        )
        .unwrap();

        let entries = FeedState::read_pane(pane_id, &AgentType::Claude);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, FeedKind::Routine);
        assert_eq!(entries[0].label, "3 tool calls");
        // Newest dropped entry's timestamp, not the oldest's.
        assert_eq!(entries[0].timestamp, "10:02");
        assert_eq!(entries[0].agent, AgentType::Claude);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_pane_singular_label_for_one_routine_call() {
        let pane_id = "%FEED_ROUTINE_ONE";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        fs::write(&path, "10:00|Bash|ls\n").unwrap();

        let entries = FeedState::read_pane(pane_id, &AgentType::Claude);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "1 tool call");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_pane_notable_entries_suppress_the_summary() {
        let pane_id = "%FEED_MIXED";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        fs::write(&path, "10:00|Bash|cargo test\n10:01|Edit|lib.rs\n").unwrap();

        let entries = FeedState::read_pane(pane_id, &AgentType::Claude);

        // The pane already has a notable row; the routine calls stay
        // invisible rather than appending a redundant summary.
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, FeedKind::FileChange);
        assert_eq!(entries[0].label, "lib.rs");

        fs::remove_file(&path).ok();
    }

    #[test]
    fn read_pane_ignores_marker_only_log() {
        let pane_id = "%FEED_MARKER_ONLY";
        let path = crate::activity::log_file_path(pane_id);
        let _ = fs::remove_file(&path);
        fs::write(&path, "10:00|__task_reset__|\n").unwrap();

        let entries = FeedState::read_pane(pane_id, &AgentType::Claude);

        assert!(entries.is_empty());

        fs::remove_file(&path).ok();
    }

    // ─── unseen badge ────────────────────────────────────────────────

    #[test]
    fn unseen_attention_counts_only_attention_kinds() {
        let mut feed = FeedState::new();
        feed.entries = vec![
            entry("10:05", FeedKind::Permission, "needs permission"),
            entry("10:04", FeedKind::FileChange, "src/main.rs"),
            entry("10:03", FeedKind::Failure, "exit 1"),
        ];
        assert_eq!(feed.unseen_attention_count(), 2);
    }

    #[test]
    fn mark_seen_clears_the_badge() {
        let mut feed = FeedState::new();
        feed.entries = vec![entry("10:05", FeedKind::Failure, "exit 1")];
        feed.mark_seen();
        assert_eq!(feed.unseen_attention_count(), 0);
    }

    #[test]
    fn new_entries_after_seen_count_again() {
        let mut feed = FeedState::new();
        feed.entries = vec![entry("10:05", FeedKind::FileChange, "a.rs")];
        feed.mark_seen();
        feed.entries
            .insert(0, entry("10:06", FeedKind::Notification, "waiting"));
        assert_eq!(feed.unseen_attention_count(), 1);
    }

    // ─── rebuild ─────────────────────────────────────────────────────

    #[test]
    fn rebuild_merges_panels_newest_first() {
        let mut feed = FeedState::new();
        feed.per_pane.insert(
            "%1".into(),
            (
                None,
                vec![
                    entry("10:02", FeedKind::FileChange, "old.rs"),
                    entry("10:04", FeedKind::FileChange, "new.rs"),
                ],
            ),
        );
        feed.per_pane.insert(
            "%2".into(),
            (None, vec![entry("10:03", FeedKind::Task, "plan")]),
        );
        feed.rebuild();
        let stamps: Vec<&str> = feed.entries.iter().map(|e| e.timestamp.as_str()).collect();
        assert_eq!(stamps, vec!["10:04", "10:03", "10:02"]);
    }

    #[test]
    fn rebuild_orders_across_midnight_by_epoch() {
        // Regression: sorting by the display `HH:MM` string put a 23:59
        // line from last night above every entry of the new day. Epoch
        // sort keys keep the true chronological order.
        let mut feed = FeedState::new();
        let yesterday_night = FeedEntry {
            timestamp: "23:59".into(),
            epoch: 1_000_000,
            agent: AgentType::Claude,
            kind: FeedKind::FileChange,
            label: "last-night".into(),
        };
        let this_morning = FeedEntry {
            timestamp: "00:01".into(),
            epoch: 1_000_120,
            agent: AgentType::Claude,
            kind: FeedKind::FileChange,
            label: "this-morning".into(),
        };
        // Stored newest-first, as `read_activity_log` yields them.
        feed.per_pane
            .insert("%1".into(), (None, vec![this_morning, yesterday_night]));
        feed.rebuild();
        let labels: Vec<&str> = feed.entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, vec!["this-morning", "last-night"]);
    }

    #[test]
    fn rebuild_puts_legacy_entries_oldest() {
        // A pre-upgrade `HH:MM` line (epoch 0) from last night must not
        // sort above this morning's epoch-stamped entries — the exact
        // report that started this fix.
        let mut feed = FeedState::new();
        let legacy_night = FeedEntry {
            timestamp: "23:37".into(),
            epoch: 0,
            agent: AgentType::Hermes,
            kind: FeedKind::FileChange,
            label: "run5-tail.md".into(),
        };
        let this_morning = FeedEntry {
            timestamp: "08:15".into(),
            epoch: 1_760_000_000,
            agent: AgentType::Hermes,
            kind: FeedKind::FileChange,
            label: "this-morning.md".into(),
        };
        feed.per_pane
            .insert("%1".into(), (None, vec![this_morning, legacy_night]));
        feed.rebuild();
        let stamps: Vec<&str> = feed.entries.iter().map(|e| e.timestamp.as_str()).collect();
        assert_eq!(stamps, vec!["08:15", "23:37"]);
    }

    #[test]
    fn rebuild_truncates_to_max_entries() {
        let mut feed = FeedState::new();
        feed.max_entries = 2;
        feed.per_pane.insert(
            "%1".into(),
            (
                None,
                vec![
                    entry("10:01", FeedKind::FileChange, "a"),
                    entry("10:02", FeedKind::FileChange, "b"),
                    entry("10:03", FeedKind::FileChange, "c"),
                ],
            ),
        );
        feed.rebuild();
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(feed.entries[0].label, "c");
    }

    #[test]
    fn rebuild_clamps_last_seen_when_list_shrinks() {
        let mut feed = FeedState::new();
        feed.entries = (0..5)
            .map(|i| entry(&format!("10:0{i}"), FeedKind::FileChange, "x"))
            .collect();
        feed.mark_seen();
        feed.per_pane.insert(
            "%1".into(),
            (None, vec![entry("10:00", FeedKind::FileChange, "only")]),
        );
        feed.rebuild();
        assert_eq!(feed.unseen_attention_count(), 0);
    }

    // ─── retain_panes ────────────────────────────────────────────────

    #[test]
    fn retain_panes_drops_departed_panes() {
        let mut feed = FeedState::new();
        feed.per_pane.insert("%1".into(), (None, vec![]));
        feed.per_pane.insert("%2".into(), (None, vec![]));
        feed.retain_panes(&["%1".to_string()]);
        assert!(feed.per_pane.contains_key("%1"));
        assert!(!feed.per_pane.contains_key("%2"));
    }
}
