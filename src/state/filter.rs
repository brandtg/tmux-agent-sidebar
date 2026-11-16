use super::AppState;

#[derive(Debug, Clone, PartialEq)]
pub enum RepoFilter {
    All,
    Repo(String),
}

/// Mirror of `WORKTREE_MARK` in `crate::group` — the prefix a non-main
/// worktree group's display name carries.
const WORKTREE_GROUP_MARK: &str = "⎇ ";

impl RepoFilter {
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Repo(name) => name.as_str(),
        }
    }

    /// Parse a tmux-option label into a `RepoFilter`. `""` and `"all"`
    /// map to `All`; any other value is stored as `Repo(name)`.
    pub fn from_label(s: &str) -> Self {
        match s {
            "all" | "" => Self::All,
            name => Self::Repo(name.to_string()),
        }
    }

    pub fn matches_group(&self, group_name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Repo(name) => {
                if name == group_name {
                    return true;
                }
                // Non-main worktree groups are titled `⎇ <name>` (or
                // `⎇ <name> (<repo>)`, see `WORKTREE_MARK` in group.rs).
                // A bare `<name>` typed into the filter must still match
                // them. Only the glyph-marked names get this treatment —
                // plain group names keep their exact-match semantics.
                match group_name.strip_prefix(WORKTREE_GROUP_MARK) {
                    Some(rest) => {
                        rest == name || rest.split_once(" (").is_some_and(|(n, _)| n == name)
                    }
                    None => false,
                }
            }
        }
    }
}

impl AppState {
    /// Return list of repo names for the popup: ["All", repo1, repo2, ...]
    pub fn repo_names(&self) -> Vec<String> {
        let mut names = vec!["All".to_string()];
        for group in &self.repo_groups {
            names.push(group.name.clone());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── RepoFilter tests ─────────────────────────────────────

    #[test]
    fn repo_filter_persistence_roundtrip() {
        assert_eq!(RepoFilter::from_label("all"), RepoFilter::All);
        assert_eq!(RepoFilter::from_label(""), RepoFilter::All);
        assert_eq!(
            RepoFilter::from_label("my-app"),
            RepoFilter::Repo("my-app".into())
        );
        assert_eq!(RepoFilter::All.as_str(), "all");
        assert_eq!(RepoFilter::Repo("my-app".into()).as_str(), "my-app");
    }

    #[test]
    fn repo_filter_matches_group() {
        assert!(RepoFilter::All.matches_group("anything"));
        assert!(RepoFilter::Repo("app".into()).matches_group("app"));
        assert!(!RepoFilter::Repo("app".into()).matches_group("other"));
    }

    #[test]
    fn repo_filter_matches_glyph_prefixed_worktree_group() {
        // Worktree groups render as `⎇ <name>`; a bare name typed into
        // the filter must still match them (the glyph rides inside
        // RepoGroup.name, the user types the repo/worktree name).
        assert!(RepoFilter::Repo("tzcalc-m10".into()).matches_group("⎇ tzcalc-m10"));
        // The parenthesized long form matches on the worktree name.
        assert!(RepoFilter::Repo("fix-auth".into()).matches_group("⎇ fix-auth (flapjack)"));
        // The full glyph-prefixed name (as selected from the repo popup)
        // still matches via exact equality.
        assert!(RepoFilter::Repo("⎇ tzcalc-m10".into()).matches_group("⎇ tzcalc-m10"));
        // Non-glyph names keep exact-match semantics: no substring drift.
        assert!(!RepoFilter::Repo("app".into()).matches_group("application"));
        assert!(!RepoFilter::Repo("tzcalc".into()).matches_group("tzcalc-m10"));
        assert!(!RepoFilter::Repo("other".into()).matches_group("⎇ tzcalc-m10"));
    }

    // ─── AppState repo_names ──────────────────────────────────

    use crate::group::RepoGroup;

    #[test]
    fn repo_names_leads_with_all_sentinel() {
        let mut state = AppState::new("%99".into());
        assert_eq!(state.repo_names(), vec!["All"]);
        state.repo_groups = vec![
            RepoGroup {
                name: "alpha".into(),
                has_focus: true,
                panes: vec![],
            },
            RepoGroup {
                name: "beta".into(),
                has_focus: false,
                panes: vec![],
            },
        ];
        assert_eq!(state.repo_names(), vec!["All", "alpha", "beta"]);
    }
}
