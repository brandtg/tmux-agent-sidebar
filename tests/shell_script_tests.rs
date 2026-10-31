//! Regression tests for the shell-side helpers: the bash/zsh shell
//! integration and the install wizard's process cleanup.

use std::process::Command;

const SHELL_EXIT: &str = include_str!("../shell-exit.sh");
const INSTALL_WIZARD: &str = include_str!("../install-wizard.sh");

#[test]
fn shell_exit_guards_outside_tmux() {
    assert!(
        SHELL_EXIT.contains(r#"if [ -z "${TMUX:-}" ] || [ -z "${TMUX_PANE:-}" ]; then"#),
        "shell-exit.sh must bail out when sourced outside tmux"
    );
    assert!(
        SHELL_EXIT.contains("return 0 2>/dev/null || exit 0"),
        "the outside-tmux guard must work for both sourcing and execution"
    );
}

#[test]
fn sourcing_shell_exit_outside_tmux_installs_no_trap() {
    let script = format!("{}/shell-exit.sh", env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "unset TMUX TMUX_PANE; . \"{script}\"; trap -p DEBUG"
        ))
        .output()
        .expect("run bash");
    assert!(
        output.status.success(),
        "sourcing the script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "outside tmux the script must not install a DEBUG trap, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn bash_debug_trap_defers_the_tmux_write_to_precmd() {
    // The DEBUG trap fires for every simple command; writing the pane
    // option there spawned one tmux process per command. It must only
    // remember the command, with the single write living in precmd.
    let debug_start = SHELL_EXIT.find("_tas_debug() {").expect("_tas_debug");
    let debug_end = SHELL_EXIT[debug_start..]
        .find("  trap '_tas_debug' DEBUG")
        .map(|offset| debug_start + offset)
        .expect("DEBUG trap registration");
    let debug_body = &SHELL_EXIT[debug_start..debug_end];
    assert!(
        !debug_body.contains("_interp"),
        "the DEBUG trap must not spawn tmux: {debug_body}"
    );
    assert!(
        debug_body.contains("_tas_pending_cmd=\"$BASH_COMMAND\""),
        "the DEBUG trap must remember the last simple command"
    );

    let precmd_start = SHELL_EXIT.find("_tas_precmd() {").expect("_tas_precmd");
    let precmd_end = SHELL_EXIT[precmd_start..]
        .find("_tas_debug() {")
        .map(|offset| precmd_start + offset)
        .expect("end of precmd");
    let precmd_body = &SHELL_EXIT[precmd_start..precmd_end];
    assert!(
        precmd_body.contains(r#"_interp "@pane_last_cmd""#),
        "precmd must flush the remembered command"
    );
    assert!(
        precmd_body.contains(r#"_interp "@pane_last_exit""#),
        "precmd must still record the exit code"
    );
}

#[test]
fn install_wizard_matches_the_binary_path_literally() {
    assert!(
        !INSTALL_WIZARD.contains(r#"pkill -f "$BINARY""#),
        "the install wizard must not treat the binary path as an ERE"
    );
    assert!(
        INSTALL_WIZARD.contains(r#"sed 's/[][\\.^$*+?(){}|]/\\&/g'"#),
        "the install wizard must regex-escape the binary path"
    );
    assert!(
        INSTALL_WIZARD.contains(r#"pkill -xf "$pattern""#),
        "the install wizard must require an exact full-argv match"
    );
}

#[test]
fn install_wizard_path_escaping_handles_regex_metacharacters() {
    let output = Command::new("sh")
        .arg("-c")
        .arg(r#"printf '%s' '/tmp/a+b(c).bin' | sed 's/[][\\.^$*+?(){}|]/\\&/g'"#)
        .output()
        .expect("run sed");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        r#"/tmp/a\+b\(c\)\.bin"#,
        "regex metacharacters in the install path must be escaped"
    );
}
