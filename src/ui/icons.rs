use std::collections::HashMap;

use crate::tmux::{self, PaneStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusIcons {
    running: String,
    background: String,
    waiting: String,
    idle: String,
    error: String,
    unknown: String,
    /// Chyron glyph for a finished-turn event (see `state::chyron`).
    done: String,
}

impl Default for StatusIcons {
    fn default() -> Self {
        Self {
            running: "●".into(),
            background: "◎".into(),
            waiting: "◐".into(),
            idle: "○".into(),
            error: "✕".into(),
            unknown: "·".into(),
            done: "✓".into(),
        }
    }
}

impl StatusIcons {
    fn preset(set: &str) -> Option<Self> {
        match set {
            "nerd" => Some(Self {
                running: "\u{f111}".into(),
                // FA bullseye (f140), not the notched spinner (f1ce): it
                // matches the default ◎ shape while staying single-cell.
                background: "\u{f140}".into(),
                waiting: "\u{f042}".into(),
                idle: "\u{f10c}".into(),
                error: "\u{f00d}".into(),
                unknown: "\u{f1db}".into(),
                // FA check.
                done: "\u{f00c}".into(),
            }),
            "ascii" => Some(Self {
                running: "*".into(),
                background: "+".into(),
                waiting: "~".into(),
                idle: "-".into(),
                error: "x".into(),
                unknown: ".".into(),
                done: "*".into(),
            }),
            _ => None,
        }
    }

    /// Load status icons from tmux @sidebar_icon_set (preset) and
    /// @sidebar_icon_* variables, falling back to defaults.
    pub fn from_tmux() -> Self {
        let all_opts = tmux::get_all_global_options();
        Self::from_options(&all_opts)
    }

    pub fn from_options(all_opts: &HashMap<String, String>) -> Self {
        let mut icons = all_opts
            .get(tmux::SIDEBAR_ICON_SET)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .and_then(Self::preset)
            .unwrap_or_default();

        let read = |var: &str, fallback: &str| -> String {
            all_opts
                .get(var)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| fallback.to_string())
        };

        icons.running = read(tmux::SIDEBAR_ICON_RUNNING, &icons.running);
        icons.background = read(tmux::SIDEBAR_ICON_BACKGROUND, &icons.background);
        icons.waiting = read(tmux::SIDEBAR_ICON_WAITING, &icons.waiting);
        icons.idle = read(tmux::SIDEBAR_ICON_IDLE, &icons.idle);
        icons.error = read(tmux::SIDEBAR_ICON_ERROR, &icons.error);
        icons.unknown = read(tmux::SIDEBAR_ICON_UNKNOWN, &icons.unknown);
        icons.done = read(tmux::SIDEBAR_ICON_DONE, &icons.done);
        icons
    }

    /// Chyron glyph for a finished-turn event.
    pub fn done_icon(&self) -> &str {
        self.done.as_str()
    }

    pub fn status_icon(&self, status: &PaneStatus) -> &str {
        match status {
            PaneStatus::Running => self.running.as_str(),
            PaneStatus::Background => self.background.as_str(),
            PaneStatus::Waiting => self.waiting.as_str(),
            PaneStatus::Idle => self.idle.as_str(),
            PaneStatus::Error => self.error.as_str(),
            PaneStatus::Unknown => self.unknown.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_icons_match_current_glyphs() {
        let icons = StatusIcons::default();
        assert_eq!(icons.status_icon(&PaneStatus::Running), "●");
        assert_eq!(icons.status_icon(&PaneStatus::Background), "◎");
        assert_eq!(icons.status_icon(&PaneStatus::Waiting), "◐");
        assert_eq!(icons.status_icon(&PaneStatus::Idle), "○");
        assert_eq!(icons.status_icon(&PaneStatus::Error), "✕");
        assert_eq!(icons.status_icon(&PaneStatus::Unknown), "·");
    }

    #[test]
    fn tmux_options_override_defaults() {
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_ICON_RUNNING.into(), "◉".into());
        opts.insert(tmux::SIDEBAR_ICON_BACKGROUND.into(), "⊙".into());
        opts.insert(tmux::SIDEBAR_ICON_UNKNOWN.into(), "∎".into());

        let icons = StatusIcons::from_options(&opts);
        assert_eq!(icons.status_icon(&PaneStatus::Running), "◉");
        assert_eq!(icons.status_icon(&PaneStatus::Background), "⊙");
        assert_eq!(icons.status_icon(&PaneStatus::Unknown), "∎");
        assert_eq!(icons.status_icon(&PaneStatus::Waiting), "◐");
    }

    #[test]
    fn nerd_preset_applies_nerd_font_glyphs() {
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_ICON_SET.into(), "nerd".into());

        let icons = StatusIcons::from_options(&opts);
        assert_eq!(icons.status_icon(&PaneStatus::Running), "\u{f111}");
        assert_eq!(icons.status_icon(&PaneStatus::Background), "\u{f140}");
        assert_eq!(icons.status_icon(&PaneStatus::Waiting), "\u{f042}");
        assert_eq!(icons.status_icon(&PaneStatus::Idle), "\u{f10c}");
        assert_eq!(icons.status_icon(&PaneStatus::Error), "\u{f00d}");
        assert_eq!(icons.status_icon(&PaneStatus::Unknown), "\u{f1db}");
    }

    #[test]
    fn ascii_preset_applies_ascii_glyphs() {
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_ICON_SET.into(), "ascii".into());

        let icons = StatusIcons::from_options(&opts);
        assert_eq!(icons.status_icon(&PaneStatus::Running), "*");
        assert_eq!(icons.status_icon(&PaneStatus::Background), "+");
        assert_eq!(icons.status_icon(&PaneStatus::Waiting), "~");
        assert_eq!(icons.status_icon(&PaneStatus::Idle), "-");
        assert_eq!(icons.status_icon(&PaneStatus::Error), "x");
        assert_eq!(icons.status_icon(&PaneStatus::Unknown), ".");
    }

    #[test]
    fn unknown_icon_set_falls_back_to_defaults() {
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_ICON_SET.into(), "bogus".into());

        let icons = StatusIcons::from_options(&opts);
        assert_eq!(icons.status_icon(&PaneStatus::Running), "●");
    }

    #[test]
    fn per_icon_overrides_take_precedence_over_preset() {
        let mut opts = HashMap::new();
        opts.insert(tmux::SIDEBAR_ICON_SET.into(), "nerd".into());
        opts.insert(tmux::SIDEBAR_ICON_RUNNING.into(), "◉".into());

        let icons = StatusIcons::from_options(&opts);
        assert_eq!(icons.status_icon(&PaneStatus::Running), "◉");
        assert_eq!(icons.status_icon(&PaneStatus::Idle), "\u{f10c}");
    }
}
