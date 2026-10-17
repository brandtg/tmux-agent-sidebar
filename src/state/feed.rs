use std::collections::HashMap;
use std::time::SystemTime;

use crate::activity::{self, FeedKind};
use crate::state::ScrollState;
use crate::tmux::AgentType;

/// One line of the cross-agent feed: a notable thing some agent did.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedEntry {
    /// `HH:MM` timestamp copied from the activity log.
    pub timestamp: String,
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
        // Timestamps are zero-padded `HH:MM`, so lexicographic order is
        // chronological order. The stable sort keeps same-minute entries
        // in pane iteration order.
        merged.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
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

    fn read_pane(pane_id: &str, agent: &AgentType) -> Vec<FeedEntry> {
        activity::read_activity_log(pane_id, 0)
            .into_iter()
            .filter(|entry| entry.tool != activity::TASK_RESET_MARKER)
            .filter_map(|entry| {
                FeedKind::classify(&entry.tool, &entry.label).map(|kind| FeedEntry {
                    timestamp: entry.timestamp,
                    agent: agent.clone(),
                    kind,
                    label: entry.label,
                })
            })
            .collect()
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

    fn entry(ts: &str, kind: FeedKind, label: &str) -> FeedEntry {
        FeedEntry {
            timestamp: ts.into(),
            agent: AgentType::Claude,
            kind,
            label: label.into(),
        }
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
