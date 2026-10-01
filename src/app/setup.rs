use crate::cli::plugin_state;
use crate::session;
use crate::state::AppState;
use crate::ui;

/// Construct and prime the initial [`AppState`] before the event loop starts.
///
/// Equivalent to the original `run_app` prelude in `src/main.rs`: installs the
/// color theme/icons from tmux options, loads global filter state, resolves
/// the Claude plugin install version once at startup, seeds session names
/// synchronously so `/rename` labels render on the first frame, and performs
/// the first refresh pass.
pub(super) fn init_state(tmux_pane: String) -> AppState {
    let mut state = AppState::new(tmux_pane);
    state.theme = ui::colors::ColorTheme::from_tmux();
    state.icons = ui::icons::StatusIcons::from_tmux();
    state.bottom_panel_height = ui::bottom_panel_height_from_tmux();
    state.pet_enabled = ui::pet_enabled_from_tmux();
    // The `toggle` subcommand launches the sidebar inside a tmux popup on
    // narrow (mobile) viewports with `SIDEBAR_POPUP=1`. Popup mode hides
    // the bottom panel (activity/git tabs) so the compact viewport stays
    // readable; the pet band is hidden with it since it only renders
    // between the pane list and the bottom panel.
    state.popup_mode = std::env::var("SIDEBAR_POPUP").as_deref() == Ok("1");
    if state.popup_mode {
        state.bottom_panel_height = 0;
    }
    state.global.load_from_tmux();
    // A new sidebar lands on `@sidebar_default_view` rather than the last
    // filter some other window persisted; see GlobalState::apply_default_view.
    state.global.apply_default_view_from_tmux();
    state.refresh();

    super::render::refresh_git_for_focused_pane(&mut state);

    // Resolve the installed Claude Code plugin status once at startup,
    // matching the version_notice pattern. Restart the sidebar after a
    // /plugin install, /plugin uninstall, or /plugin update to pick up
    // the new state.
    state.notices.claude_plugin_status = plugin_state::installed_plugin_status();
    // Likewise resolve whether the user still has legacy
    // tmux-agent-sidebar/hook.sh entries in ~/.claude/settings.json so
    // the notices popup can warn about duplicate hook execution.
    state.notices.claude_settings_has_residual_hooks =
        plugin_state::claude_settings_has_residual_hooks();
    // The `@sidebar_agents` allow-list narrows which agents the notices
    // system evaluates (e.g. `set -g @sidebar_agents "claude,opencode"`
    // silences the Codex missing-hooks warning for non-Codex users).
    // Empty/unset means "evaluate every supported agent".
    state.notices.enabled_agents = crate::state::parse_enabled_agents(
        crate::tmux::get_option(crate::tmux::SIDEBAR_AGENTS).as_deref(),
    );
    // Notice inputs are static after the two lines above, so compute
    // them once here instead of from the per-tick refresh loop. This
    // also decouples the ⓘ badge from `focused_pane_id`, so killing
    // the last agent pane no longer drops outstanding setup warnings.
    state.refresh_notices();
    // Populate session names synchronously before the first draw so
    // `/rename`-assigned labels show up without waiting for the first
    // background scan tick.
    state.sessions.names = session::scan_session_names();
    state.sessions.dirty = true;
    state.refresh();

    state
}
