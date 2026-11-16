//! Clipboard write shims shared by every copy surface in the sidebar
//! (the notices popup's LLM prompts, the TODO panel's `y` yank).
//!
//! One entry point, [`copy_to_clipboard`], so every surface applies the
//! same policy: `tmux set-buffer -w` (with a plain `set-buffer` fallback)
//! on all platforms, plus `arboard` additionally on non-Linux.

use std::time::Duration;

/// Copy `text` to the clipboard surfaces reachable from inside tmux.
/// Returns true only when at least one *verifiable* destination
/// succeeded so the caller can decide whether to show the `copied`
/// feedback.
pub(crate) fn copy_to_clipboard(text: &str) -> bool {
    let tmux_ok = copy_via_tmux_buffer(text);
    #[cfg(not(target_os = "linux"))]
    let ok = tmux_ok || copy_via_os_clipboard(text);
    #[cfg(target_os = "linux")]
    let ok = tmux_ok;
    ok
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

#[cfg(test)]
mod tests {
    // The shims shell out to real `tmux` / the OS clipboard; calling
    // them from a test would clobber the user's actual paste buffer.
    // The policy (success → feedback, failure → silent) is exercised
    // through the callers' `record_*` seams instead.
}
