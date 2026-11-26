//! Clipboard write shims shared by every copy surface in the sidebar
//! (the notices popup's LLM prompts, the TODO panel's `y` yank).
//!
//! One entry point, [`copy_to_clipboard`], so every surface applies the
//! same policy: `tmux set-buffer -w` (with a plain `set-buffer` fallback)
//! on all platforms, plus `arboard` additionally on non-Linux.

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
///
/// The `-w` capability is remembered for the process lifetime: without
/// it, a pre-3.2 tmux paid a doomed `-w` spawn *plus* the plain write on
/// every copy — two serial 3s-timeout subprocesses on the UI thread per
/// `y` (audit 2026-11-16 J5). The trade-off: a transient `-w` failure on
/// a modern tmux also pins the plain path until restart, losing the OSC
/// 52 delegation but never the paste buffer itself.
fn copy_via_tmux_buffer(text: &str) -> bool {
    static W_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Through the shared exec path: the IPC gate keeps test builds from
    // clobbering a live paste buffer, and failures land in the debug
    // trace (audit 2026-11-19 C3). `run_tmux` maps success → `Some`,
    // failure → `None`, which is all the status check needs.
    let run_set_buffer = |args: &[&str]| crate::tmux::run_tmux(args).is_some();
    match W_SUPPORTED.get() {
        Some(true) => run_set_buffer(&set_buffer_args(true, text)),
        Some(false) => run_set_buffer(&set_buffer_args(false, text)),
        None => {
            // First copy decides for the process: `-w` first, and the
            // plain fallback both recovers the copy and records the
            // capability.
            let w_ok = run_set_buffer(&set_buffer_args(true, text));
            let _ = W_SUPPORTED.set(w_ok);
            if w_ok {
                true
            } else {
                run_set_buffer(&set_buffer_args(false, text))
            }
        }
    }
}

/// The argv for a `tmux set-buffer` write. `--` ends flag parsing so a
/// payload starting with `-` (dash-bullet todo yanks, a literal
/// `-w hello`) is taken verbatim instead of parsed as a flag — which
/// failed the copy *and* pinned `W_SUPPORTED=false` for the process
/// lifetime (audit 2026-11-25 H-tmux-1).
fn set_buffer_args(with_clipboard: bool, text: &str) -> Vec<&str> {
    if with_clipboard {
        vec!["set-buffer", "-w", "--", text]
    } else {
        vec!["set-buffer", "--", text]
    }
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
    use super::*;

    #[test]
    fn set_buffer_args_end_flag_parsing_before_the_payload() {
        // H-tmux-1 (audit 2026-11-25): every shape carries `--` before
        // the payload so a yank starting with `-` is stored verbatim
        // instead of parsed as a flag. The scoped-server counterpart
        // (hook_pipeline_tests::set_buffer_takes_dash_payload_verbatim)
        // pins tmux's acceptance of these exact shapes.
        assert_eq!(
            set_buffer_args(true, "-w hello"),
            vec!["set-buffer", "-w", "--", "-w hello"]
        );
        assert_eq!(
            set_buffer_args(false, "- fix the login bug"),
            vec!["set-buffer", "--", "- fix the login bug"]
        );
    }
}
