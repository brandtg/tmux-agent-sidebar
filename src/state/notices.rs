use std::time::{Duration, Instant};

use super::AppState;
use crate::cli::plugin_state::ClaudePluginStatus;

/// Sub-state for the ⓘ notices popup, lifted out of [`AppState`] so its
/// eight related fields (button column, missing-hook groups, plugin
/// status, legacy hook flag, plugin notice, agent allow-list, copy
/// targets, copy feedback) travel as a single unit.
#[derive(Debug, Clone, Default)]
pub struct NoticesState {
    /// Column of the ⓘ button in the secondary header, or `None` when the
    /// button is hidden. Used for click hit-testing.
    pub button_col: Option<u16>,
    /// Missing hooks grouped per agent, shown in the "Missing hooks"
    /// section of the popup.
    pub missing_hook_groups: Vec<NoticesMissingHookGroup>,
    /// Status of the `tmux-agent-sidebar` Claude Code plugin install
    /// (whether it is installed, and whether any tracked file in its
    /// cache differs from the copy embedded into this binary). Resolved
    /// once from `~/.claude/plugins/installed_plugins.json` and cached
    /// for the lifetime of the TUI process — restart the sidebar after
    /// a `/plugin install`, `/plugin uninstall`, or `/plugin update` to
    /// pick up the change. `claude_plugin_notice` and the missing-hooks
    /// Claude filter are derived from this field.
    pub claude_plugin_status: ClaudePluginStatus,
    /// Whether `~/.claude/settings.json` still contains residual
    /// `tmux-agent-sidebar/hook.sh` entries from the legacy manual
    /// setup. Resolved once at startup. When this is `true` AND the
    /// plugin is installed, every hook fires twice and the popup must
    /// keep nagging the user to clean up.
    pub claude_settings_has_residual_hooks: bool,
    /// Drives the `Plugin / claude` section in the notices popup. See
    /// [`ClaudePluginNotice`] for the full set of variants. Derived from
    /// `claude_plugin_status` in `refresh_notices`.
    pub claude_plugin_notice: Option<ClaudePluginNotice>,
    /// Agents the user wants the sidebar to track, parsed from the
    /// `@sidebar_agents` tmux option once at startup. Empty means "no
    /// restriction" — every supported agent is evaluated. Agents outside
    /// the list are skipped by the missing-hooks diagnostics and never
    /// surface a `Plugin / claude` notice.
    pub enabled_agents: Vec<String>,
    /// Click regions for the `copy` label on each agent row in the popup.
    pub copy_targets: Vec<NoticesCopyTarget>,
    /// Agent name and timestamp of the most recent successful copy, shown
    /// as a transient `copied` label next to the popup title.
    pub copied_at: Option<(String, Instant)>,
}

/// Missing hooks grouped by agent name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticesMissingHookGroup {
    pub agent: String,
    pub hooks: Vec<String>,
}

/// Notice surfaced in the popup's `Plugin / claude` section. The
/// variants are mutually exclusive and ordered by urgency:
/// `DuplicateHooks` > `InstallRecommended` > `Stale`. When the plugin
/// is installed, its cached hooks match the embedded snapshot, and the
/// user has no residual manual hook entries, no notice is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudePluginNotice {
    /// The Claude Code plugin is not installed. The popup offers a
    /// `[prompt]` copy button that hands an LLM the migration recipe
    /// (clean up `~/.claude/settings.json` then run `/plugin install`).
    InstallRecommended,
    /// The plugin is installed AND the user still has legacy
    /// `tmux-agent-sidebar/hook.sh` entries in `~/.claude/settings.json`.
    /// Every hook fires twice in this state — once via the plugin, once
    /// via the manual setting. Takes precedence over `Stale` because it
    /// is an actively-broken state, not just a pending update.
    DuplicateHooks,
    /// The plugin is installed but at least one file tracked by
    /// `EMBEDDED_PLUGIN_FILES` differs between its cache and the
    /// snapshot embedded in the running binary. The user needs
    /// `/plugin update` so Claude Code re-reads the affected files.
    /// Comparing file content (rather than the manifest `version`
    /// string) means the notice only fires when an update actually
    /// changes fork behavior — the `hook.sh` wrapper already runs the
    /// latest binary on every invocation, so a bare version bump with
    /// no content changes is silent.
    Stale,
}

/// Click target for the `copy` label next to an agent in the notices popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticesCopyTarget {
    pub area: ratatui::layout::Rect,
    pub agent: String,
}

impl AppState {
    /// Resolve the notices popup inputs once.
    ///
    /// Every input is static for the sidebar's lifetime:
    /// `claude_plugin_status` and `claude_settings_has_residual_hooks`
    /// are resolved at `main.rs` startup, and `settings.json` /
    /// `hooks.json` edits only take effect after a sidebar restart —
    /// matching the restart-required contract already documented for
    /// `/plugin install`. So this runs once from `main.rs` instead of
    /// being pinned to the per-tick refresh loop, and the ⓘ badge no
    /// longer depends on which pane happens to be focused.
    ///
    /// Both Claude and Codex are always evaluated by default so a user
    /// who closes their last agent pane still sees any outstanding hook
    /// setup warnings. A non-empty `enabled_agents` allow-list (from the
    /// `@sidebar_agents` tmux option) narrows the evaluation to the
    /// agents the user actually uses.
    pub fn refresh_notices(&mut self) {
        // The Plugin / claude section is a Claude-specific nag, so the
        // allow-list gates it too: a user who excluded claude does not
        // want to hear about plugin installs for an agent they never
        // run.
        let claude_enabled = agent_enabled(&self.notices.enabled_agents, crate::tmux::CLAUDE_AGENT);
        self.notices.claude_plugin_notice = if claude_enabled {
            compute_claude_plugin_notice(
                &self.notices.claude_plugin_status,
                self.notices.claude_settings_has_residual_hooks,
            )
        } else {
            None
        };

        // Suppress Claude from the missing-hooks list whenever the
        // plugin is installed. Residual legacy entries are already
        // surfaced by the Plugin section's `DuplicateHooks` notice, so
        // re-adding Claude here would only duplicate the warning.
        let claude_plugin_present = self.notices.claude_plugin_status.installed;

        let resolved_hook = crate::cli::setup::resolve_hook_script();
        let force_missing = debug_forced_display();
        let load_config = |agent: &str| -> serde_json::Value {
            if force_missing {
                serde_json::Value::Null
            } else {
                crate::cli::setup::load_current_config(agent)
            }
        };
        // When `resolve_hook_script` could not actually locate the
        // installed `hook.sh` it returns a fallback path that is unlikely
        // to match what the user wrote in their config. Verifying against
        // that fallback would flag every custom install as "Missing hooks"
        // — skip the check unless detection succeeded (debug overrides
        // still force the warning so the popup remains testable).
        self.notices.missing_hook_groups = if force_missing || resolved_hook.detected {
            let agents = apply_agent_allowlist(
                vec![
                    crate::tmux::CLAUDE_AGENT.to_string(),
                    crate::tmux::CODEX_AGENT.to_string(),
                ],
                &self.notices.enabled_agents,
            );
            compute_missing_hook_groups(
                claude_plugin_present,
                agents,
                &resolved_hook.path,
                load_config,
            )
        } else {
            Vec::new()
        };
    }

    /// Return the agent name if the given (row, col) hits a `[copy]` label
    /// in the currently rendered notices popup. Pure lookup — no side effects.
    pub fn notices_copy_target_at(&self, row: u16, col: u16) -> Option<&str> {
        self.notices
            .copy_targets
            .iter()
            .find(|t| super::layout::point_in_rect(row, col, t.area))
            .map(|t| t.agent.as_str())
    }

    /// Copy the LLM setup prompt for the given agent (`claude` / `codex`)
    /// to the clipboard surfaces reachable from inside tmux: `tmux
    /// set-buffer -w` sets the tmux paste buffer and lets tmux itself
    /// forward OSC 52 to the upstream terminal, and off-Linux builds
    /// additionally write the OS clipboard via `arboard`. Returns true
    /// only when at least one *verifiable* destination succeeded so the
    /// caller can decide whether to show the `[copied]` feedback.
    pub fn copy_notices_prompt(&mut self, agent: &str) -> bool {
        let Some(prompt) = crate::ui::notices::prompt_for_agent(agent) else {
            return false;
        };
        let tmux_ok = copy_via_tmux_buffer(&prompt);
        #[cfg(not(target_os = "linux"))]
        let ok = tmux_ok || copy_via_os_clipboard(&prompt);
        #[cfg(target_os = "linux")]
        let ok = tmux_ok;
        self.record_notices_copy_result(agent, ok)
    }

    /// Stamp the `[copied]` feedback state. Pure separation from the I/O
    /// above so the success policy is unit-testable without touching the
    /// real clipboard or tmux.
    pub fn record_notices_copy_result(&mut self, agent: &str, success: bool) -> bool {
        if success {
            self.notices.copied_at = Some((agent.to_string(), Instant::now()));
        }
        success
    }
}

/// Write `text` to the tmux paste buffer, asking tmux to also set the
/// upstream terminal clipboard (`-w` makes **tmux** emit the OSC 52
/// sequence to the client terminal). Writing an OSC 52 from inside a
/// pane does not work: tmux parses the sequence, stores the payload in
/// its own buffer, and never forwards it to the outer terminal, so an
/// application-emitted escape is a silent no-op — the `-w` delegation is
/// the only path that actually reaches Alacritty, kitty, iTerm2, etc.
/// Falls back to plain `set-buffer` for tmux versions predating 3.2 that
/// reject `-w`. Returns whether the paste buffer was set.
fn copy_via_tmux_buffer(text: &str) -> bool {
    let run_set_buffer = |args: &[&str]| {
        let mut command = std::process::Command::new("tmux");
        command.args(args);
        crate::subprocess::run_with_timeout(&mut command, Duration::from_secs(3))
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    run_set_buffer(&["set-buffer", "-w", text]) || run_set_buffer(&["set-buffer", text])
}

/// Best-effort write to the OS clipboard via `arboard`. Only compiled
/// off Linux: with the crate's `default-features = false` there is no
/// Wayland data-control backend, so Linux builds use X11 exclusively —
/// and an X11 selection owner must stay alive to serve paste requests.
/// The `Clipboard` created here dies at the end of the call, taking the
/// content with it (and the compositor's X11/Wayland bridge then wipes
/// whatever the user had on their clipboard). tmux's `set-buffer -w`
/// covers Linux correctly via `copy_via_tmux_buffer`.
#[cfg(not(target_os = "linux"))]
fn copy_via_os_clipboard(text: &str) -> bool {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_string()))
        .is_ok()
}

/// Compute the per-agent missing-hook list shown in the notices popup.
///
/// `claude_plugin_present` gates Claude visibility: when the plugin is
/// installed, it owns the hook wiring so Claude is filtered out (the
/// `Plugin / claude` section reports stale-version state instead). When
/// the plugin is **not** installed, Claude must surface concrete
/// missing-hook diagnostics for users still on the manual
/// `~/.claude/settings.json` path. Codex is unaffected — Codex CLI has
/// no plugin mechanism upstream.
///
/// Pure function: takes the agent list and a config loader as inputs so
/// tests do not need to manipulate `/tmp` files or `~/.claude/`.
pub(super) fn compute_missing_hook_groups(
    claude_plugin_present: bool,
    agents: Vec<String>,
    hook_script: &str,
    load_config: impl Fn(&str) -> serde_json::Value,
) -> Vec<NoticesMissingHookGroup> {
    agents
        .into_iter()
        .filter(|agent| !(claude_plugin_present && agent == "claude"))
        .filter_map(|agent| {
            let config = load_config(&agent);
            let hooks = crate::cli::setup::missing_hooks(&agent, &config, hook_script);
            if hooks.is_empty() {
                None
            } else {
                Some(NoticesMissingHookGroup { agent, hooks })
            }
        })
        .collect()
}

/// Whether `agent` passes the `@sidebar_agents` allow-list. An empty
/// allow-list means "no restriction".
fn agent_enabled(enabled: &[String], agent: &str) -> bool {
    enabled.is_empty() || enabled.iter().any(|e| e.eq_ignore_ascii_case(agent))
}

/// Narrow `all` to the user's `@sidebar_agents` allow-list. An empty
/// allow-list means "no restriction" and returns `all` unchanged so the
/// default stays "evaluate every supported agent".
fn apply_agent_allowlist(all: Vec<String>, enabled: &[String]) -> Vec<String> {
    if enabled.is_empty() {
        return all;
    }
    all.into_iter()
        .filter(|agent| agent_enabled(enabled, agent))
        .collect()
}

/// Parse the raw `@sidebar_agents` tmux option value into an agent
/// allow-list. `None` / empty means "no restriction". Values are
/// trimmed, case-folded, and matched against the supported agent set so
/// a typo degrades to "agent ignored" (falling back to no restriction
/// when nothing valid remains) instead of disabling every notice.
pub(crate) fn parse_enabled_agents(raw: Option<&str>) -> Vec<String> {
    const SUPPORTED: [&str; 3] = [
        crate::tmux::CLAUDE_AGENT,
        crate::tmux::CODEX_AGENT,
        crate::tmux::OPENCODE_AGENT,
    ];
    let Some(raw) = raw else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|part| {
            let name = part.trim().to_ascii_lowercase();
            if name.is_empty() {
                return None;
            }
            SUPPORTED
                .iter()
                .find(|candidate| name == **candidate)
                .map(|candidate| (*candidate).to_string())
        })
        .collect()
}

/// Build the `Plugin / claude` notice based on the recorded plugin
/// status and whether residual manual hook entries remain in
/// `~/.claude/settings.json`. Priority order:
///
/// - No plugin install → `InstallRecommended` (the migration prompt
///   handles legacy cleanup as part of the same step, so `has_residual`
///   does not matter here).
/// - Plugin installed + residual entries → `DuplicateHooks`. Takes
///   precedence over `Stale` because hooks are firing twice right now
///   and the cleanup is more urgent than a pending update.
/// - Plugin installed, no residual, any tracked cached file differs
///   from its embedded snapshot → `Stale`.
/// - Plugin installed, no residual, every tracked cached file matches
///   → no notice.
pub(super) fn compute_claude_plugin_notice(
    status: &ClaudePluginStatus,
    has_residual_hooks: bool,
) -> Option<ClaudePluginNotice> {
    if !status.installed {
        return Some(ClaudePluginNotice::InstallRecommended);
    }
    if has_residual_hooks {
        return Some(ClaudePluginNotice::DuplicateHooks);
    }
    if status.cache_outdated {
        return Some(ClaudePluginNotice::Stale);
    }
    None
}

pub(crate) fn debug_forced_display() -> bool {
    cfg!(feature = "debug")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── compute_missing_hook_groups: claude_plugin_present gating ───

    /// Return a config loader that always reports an empty config — i.e.
    /// every hook the adapter expects is reported as missing. This keeps
    /// these tests focused on the agent filtering logic instead of on
    /// the `missing_hooks` algorithm itself.
    fn empty_config_loader() -> impl Fn(&str) -> serde_json::Value {
        |_agent: &str| serde_json::Value::Null
    }

    #[test]
    fn missing_hook_groups_includes_claude_when_plugin_not_installed() {
        // Manual `~/.claude/settings.json` path — Claude must surface
        // concrete missing-hook diagnostics so the user knows what to
        // wire up. The Plugin section's InstallRecommended notice is a
        // companion, not a substitute.
        let groups = compute_missing_hook_groups(
            /* claude_plugin_present */ false,
            vec!["claude".to_string()],
            "/fake/hook.sh",
            empty_config_loader(),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].agent, "claude");
        assert!(
            !groups[0].hooks.is_empty(),
            "claude should report missing hooks against an empty config"
        );
    }

    #[test]
    fn missing_hook_groups_skips_claude_when_plugin_installed() {
        // The plugin owns the hook wiring once installed, so Claude
        // must NOT appear under Missing hooks (the Plugin section
        // reports stale-version state separately).
        let groups = compute_missing_hook_groups(
            /* claude_plugin_present */ true,
            vec!["claude".to_string()],
            "/fake/hook.sh",
            empty_config_loader(),
        );
        assert!(
            groups.is_empty(),
            "claude must be filtered out when the plugin is detected, got {:?}",
            groups
        );
    }

    #[test]
    fn missing_hook_groups_keeps_codex_regardless_of_plugin() {
        let agents = vec!["codex".to_string()];

        let without = compute_missing_hook_groups(
            false,
            agents.clone(),
            "/fake/hook.sh",
            empty_config_loader(),
        );
        let with =
            compute_missing_hook_groups(true, agents, "/fake/hook.sh", empty_config_loader());
        assert_eq!(without, with);
        assert_eq!(without.len(), 1);
        assert_eq!(without[0].agent, "codex");
    }

    #[test]
    fn missing_hook_groups_drops_only_claude_when_both_agents_present_and_plugin_installed() {
        // Forced-debug rendering passes both agents; verify the filter
        // hits Claude alone without affecting the Codex row.
        let groups = compute_missing_hook_groups(
            true,
            vec!["claude".to_string(), "codex".to_string()],
            "/fake/hook.sh",
            empty_config_loader(),
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].agent, "codex");
    }

    // ─── compute_claude_plugin_notice ────────────────────────────────

    const STATUS_ABSENT: ClaudePluginStatus = ClaudePluginStatus {
        installed: false,
        cache_outdated: false,
    };
    const STATUS_IN_SYNC: ClaudePluginStatus = ClaudePluginStatus {
        installed: true,
        cache_outdated: false,
    };
    const STATUS_OUTDATED: ClaudePluginStatus = ClaudePluginStatus {
        installed: true,
        cache_outdated: true,
    };

    #[test]
    fn plugin_notice_install_recommended_when_plugin_missing() {
        // Plugin not installed → the user has not run `/plugin install`
        // yet, so the popup should encourage them to. Residual hooks do
        // not change this — the migration prompt cleans them up too.
        assert_eq!(
            compute_claude_plugin_notice(&STATUS_ABSENT, false),
            Some(ClaudePluginNotice::InstallRecommended)
        );
        assert_eq!(
            compute_claude_plugin_notice(&STATUS_ABSENT, true),
            Some(ClaudePluginNotice::InstallRecommended)
        );
    }

    #[test]
    fn plugin_notice_none_when_hooks_match_and_no_residual() {
        assert_eq!(compute_claude_plugin_notice(&STATUS_IN_SYNC, false), None);
    }

    #[test]
    fn plugin_notice_stale_when_cached_hooks_differ_and_no_residual() {
        assert_eq!(
            compute_claude_plugin_notice(&STATUS_OUTDATED, false),
            Some(ClaudePluginNotice::Stale)
        );
    }

    #[test]
    fn plugin_notice_duplicate_hooks_when_residual_overrides_stale() {
        // Plugin is installed AND legacy entries are still in
        // settings.json. Hooks fire twice. The DuplicateHooks notice
        // takes precedence over Stale even when the cached hooks.json
        // is also out of date — cleanup is the more urgent action.
        assert_eq!(
            compute_claude_plugin_notice(&STATUS_OUTDATED, true),
            Some(ClaudePluginNotice::DuplicateHooks)
        );
        assert_eq!(
            compute_claude_plugin_notice(&STATUS_IN_SYNC, true),
            Some(ClaudePluginNotice::DuplicateHooks)
        );
    }

    // ─── copy feedback policy ────────────────────────────────────────

    #[test]
    fn record_notices_copy_result_success_sets_copied_feedback() {
        let mut state = AppState::new(String::new());
        assert!(state.record_notices_copy_result("claude", true));
        let entry = state
            .notices
            .copied_at
            .as_ref()
            .expect("success path must set notices_copied_at");
        assert_eq!(entry.0, "claude");
    }

    #[test]
    fn record_notices_copy_result_failure_does_not_set_copied_feedback() {
        let mut state = AppState::new(String::new());
        // Pre-populate to assert the failure path does not overwrite it.
        state.notices.copied_at = None;
        assert!(!state.record_notices_copy_result("claude", false));
        assert!(
            state.notices.copied_at.is_none(),
            "`[copied]` must not flash when every clipboard sink failed"
        );
    }

    #[test]
    fn record_notices_copy_result_failure_preserves_previous_feedback() {
        let mut state = AppState::new(String::new());
        let earlier = (
            "codex".to_string(),
            Instant::now() - std::time::Duration::from_millis(10),
        );
        state.notices.copied_at = Some(earlier.clone());
        // A later copy that fails should not clobber an earlier success,
        // but more importantly it must not fabricate a success for itself.
        assert!(!state.record_notices_copy_result("claude", false));
        let still = state
            .notices
            .copied_at
            .as_ref()
            .expect("prior success should survive a subsequent failure");
        assert_eq!(still.0, earlier.0);
    }

    #[test]
    fn copy_notices_prompt_short_circuits_for_unknown_agent() {
        // `gemini` has no prompt definition, so the function must return
        // early with `false` and leave `notices_copied_at` untouched —
        // without touching the real clipboard or tmux at all.
        let mut state = AppState::new(String::new());
        state.notices.copied_at = None;
        assert!(!state.copy_notices_prompt("gemini"));
        assert!(state.notices.copied_at.is_none());
    }

    // ─── notices_copy_target_at hit detection ───────────────────────

    fn copy_target_fixture() -> AppState {
        let mut state = AppState::new(String::new());
        state.notices.copy_targets = vec![
            NoticesCopyTarget {
                area: ratatui::layout::Rect::new(10, 5, 8, 1),
                agent: "claude".into(),
            },
            NoticesCopyTarget {
                area: ratatui::layout::Rect::new(10, 7, 8, 1),
                agent: "codex".into(),
            },
        ];
        state
    }

    #[test]
    fn notices_copy_target_at_finds_claude_row() {
        let state = copy_target_fixture();
        assert_eq!(state.notices_copy_target_at(5, 10), Some("claude"));
        assert_eq!(state.notices_copy_target_at(5, 17), Some("claude"));
    }

    #[test]
    fn notices_copy_target_at_finds_codex_row() {
        let state = copy_target_fixture();
        assert_eq!(state.notices_copy_target_at(7, 12), Some("codex"));
    }

    #[test]
    fn notices_copy_target_at_misses_outside_target_bounds() {
        let state = copy_target_fixture();
        // Same row but to the left of the slot
        assert_eq!(state.notices_copy_target_at(5, 9), None);
        // Same row but to the right of the slot
        assert_eq!(state.notices_copy_target_at(5, 18), None);
        // Gap row between the two targets
        assert_eq!(state.notices_copy_target_at(6, 12), None);
    }

    #[test]
    fn notices_copy_target_at_returns_none_when_no_targets_tracked() {
        let state = AppState::new(String::new());
        assert_eq!(state.notices_copy_target_at(5, 10), None);
    }

    // ─── refresh_notices (integration-lite) ──────────────────────────

    #[test]
    fn refresh_notices_derives_plugin_notice_from_status() {
        let mut state = AppState::new(String::new());
        state.notices.claude_plugin_status = STATUS_ABSENT;
        state.refresh_notices();
        assert_eq!(
            state.notices.claude_plugin_notice,
            Some(ClaudePluginNotice::InstallRecommended)
        );

        state.notices.claude_plugin_status = STATUS_IN_SYNC;
        state.notices.claude_settings_has_residual_hooks = false;
        state.refresh_notices();
        assert_eq!(state.notices.claude_plugin_notice, None);

        state.notices.claude_plugin_status = STATUS_IN_SYNC;
        state.notices.claude_settings_has_residual_hooks = true;
        state.refresh_notices();
        assert_eq!(
            state.notices.claude_plugin_notice,
            Some(ClaudePluginNotice::DuplicateHooks)
        );
    }

    #[test]
    fn refresh_notices_drops_claude_plugin_notice_when_claude_excluded() {
        // A user who removed claude from `@sidebar_agents` does not use
        // Claude — the Plugin / claude section must stay silent even in
        // the not-installed state that would normally nag.
        let mut state = AppState::new(String::new());
        state.notices.enabled_agents = vec!["codex".to_string()];
        state.notices.claude_plugin_status = STATUS_ABSENT;
        state.refresh_notices();
        assert_eq!(state.notices.claude_plugin_notice, None);

        // Codex-only allow-list must not affect the claude gating.
        state.notices.enabled_agents = vec!["claude".to_string()];
        state.refresh_notices();
        assert_eq!(
            state.notices.claude_plugin_notice,
            Some(ClaudePluginNotice::InstallRecommended)
        );
    }

    // ─── agent allow-list ────────────────────────────────────────────

    #[test]
    fn parse_enabled_agents_empty_for_none_and_blank() {
        assert!(parse_enabled_agents(None).is_empty());
        assert!(parse_enabled_agents(Some("")).is_empty());
        assert!(parse_enabled_agents(Some("   ")).is_empty());
        assert!(parse_enabled_agents(Some(", ,")).is_empty());
    }

    #[test]
    fn parse_enabled_agents_splits_and_trims() {
        assert_eq!(
            parse_enabled_agents(Some("claude, opencode")),
            vec!["claude".to_string(), "opencode".to_string()]
        );
        // Case-folded and whitespace-tolerant.
        assert_eq!(
            parse_enabled_agents(Some("  Claude ,CODEX ")),
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn parse_enabled_agents_filters_unknown_names() {
        // Typos degrade to "agent ignored"; garbage-only input falls
        // back to no restriction (empty list) rather than hiding every
        // notice.
        assert_eq!(parse_enabled_agents(Some("claude,gemini")), vec!["claude"]);
        assert!(parse_enabled_agents(Some("gemini;codex")).is_empty());
    }

    #[test]
    fn apply_agent_allowlist_empty_means_no_restriction() {
        let all = vec!["claude".to_string(), "codex".to_string()];
        assert_eq!(apply_agent_allowlist(all.clone(), &[]), all);
    }

    #[test]
    fn apply_agent_allowlist_narrows_to_enabled() {
        let all = vec!["claude".to_string(), "codex".to_string()];
        let enabled = vec!["claude".to_string()];
        assert_eq!(apply_agent_allowlist(all, &enabled), vec!["claude"]);
    }

    #[test]
    fn apply_agent_allowlist_keeps_all_when_no_enabled_agent_matches() {
        // The allow-list names only agents outside the evaluated set
        // (e.g. opencode, which has no hook config) — nothing to
        // evaluate, so the result is empty rather than unrestricted.
        let all = vec!["claude".to_string(), "codex".to_string()];
        let enabled = vec!["opencode".to_string()];
        assert!(apply_agent_allowlist(all, &enabled).is_empty());
    }
}
