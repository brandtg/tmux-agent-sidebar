use std::cmp::Ordering;
use std::process::Command;

use serde_json::Value;

pub const LOCAL_VERSION: &str = crate::VERSION;
const GITHUB_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/brandtg/tmux-agent-sidebar/releases/latest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub local_version: String,
    pub latest_version: String,
}

pub fn fetch_update_notice() -> Option<UpdateNotice> {
    let latest = fetch_latest_release_version()?;
    if compare_versions(&latest, LOCAL_VERSION) == Ordering::Greater {
        Some(UpdateNotice {
            local_version: LOCAL_VERSION.to_string(),
            latest_version: latest,
        })
    } else {
        None
    }
}

fn fetch_latest_release_version() -> Option<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: tmux-agent-sidebar",
            GITHUB_LATEST_RELEASE_URL,
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .or_else(|| value.get("name").and_then(Value::as_str))?;
    normalize_version(tag)
}

fn normalize_version(tag: &str) -> Option<String> {
    let trimmed = tag.trim().trim_start_matches('v');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    let left_parts = parse_version_parts(left);
    let right_parts = parse_version_parts(right);
    let len = left_parts.len().max(right_parts.len());

    for idx in 0..len {
        let left_part = *left_parts.get(idx).unwrap_or(&(0, true));
        let right_part = *right_parts.get(idx).unwrap_or(&(0, true));
        match left_part.cmp(&right_part) {
            Ordering::Equal => continue,
            other => return other,
        }
    }

    Ordering::Equal
}

/// One dotted segment as `(numeric value, is_final)`. A non-numeric
/// suffix (`3-rc1`, `3a`) marks a pre-release segment, which compares
/// *lower* than the same numeric segment without one — otherwise
/// `1.2.3-rc1` vs `1.2.3` compared Equal and the rc→final transition
/// showed no update notice (audit 2026-11-25 L-tmux-6). Suffix-vs-suffix
/// detail (rc1 vs rc2) still compares Equal; out of scope.
fn parse_version_parts(version: &str) -> Vec<(u64, bool)> {
    version
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            let numeric: String = part.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            let is_final = numeric.len() == part.len();
            let value = numeric.parse::<u64>().unwrap_or(0);
            (value, is_final)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_notice_formats_with_release_prefix() {
        let notice = UpdateNotice {
            local_version: "0.2.6".into(),
            latest_version: "0.2.7".into(),
        };
        let label = format!("new release v{}!", notice.latest_version);
        assert_eq!(label, "new release v0.2.7!");
    }

    #[test]
    fn compare_versions_orders_newer_release_higher() {
        assert_eq!(compare_versions("0.2.7", "0.2.6"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.6", "0.2.7"), Ordering::Less);
    }

    #[test]
    fn compare_versions_handles_prefix_and_missing_segments() {
        assert_eq!(compare_versions("v0.2.7", "0.2.7"), Ordering::Equal);
        assert_eq!(compare_versions("0.3", "0.2.9"), Ordering::Greater);
    }

    #[test]
    fn normalize_version_strips_tag_prefix() {
        assert_eq!(normalize_version("v1.2.3"), Some("1.2.3".into()));
    }

    #[test]
    fn compare_versions_ranks_pre_release_below_final() {
        // The rc→final transition must fire the update notice: a
        // non-numeric suffix compares lower than the same numeric version
        // (audit 2026-11-25 L-tmux-6).
        assert_eq!(compare_versions("1.2.3-rc1", "1.2.3"), Ordering::Less);
        assert_eq!(compare_versions("1.2.3", "1.2.3-rc1"), Ordering::Greater);
        assert_eq!(compare_versions("1.2.3-rc1", "1.2.3-rc1"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.3a", "1.2.3"), Ordering::Less);
        // Pre-release of an older segment still loses to a newer final.
        assert_eq!(compare_versions("1.2.3-rc1", "1.2.4"), Ordering::Less);
    }
}
