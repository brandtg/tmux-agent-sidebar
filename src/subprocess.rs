//! Bounded subprocess execution shared by everything on the render path.
//!
//! Every tmux/git/ps/lsof call runs synchronously inside the TUI event loop,
//! so a hung child (stale NFS mount, blocked index.lock, credential prompt)
//! would otherwise freeze the sidebar indefinitely. [`run_with_timeout`]
//! guarantees the call returns within `timeout`.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for output-drain threads to finish after the child (or
/// its process group) is gone before abandoning them. Generous on the
/// success path where EOF arrives with child exit; output is discarded on
/// the timeout path anyway.
const DRAIN_COLLECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `command` to completion, killing it (and, on Unix, its whole process
/// group) if it outlives `timeout`.
///
/// Stdio is forced to the `Command::output()` defaults — stdin null, stdout
/// and stderr piped — so callers get captured output and children reading
/// stdin can't block. Output is drained on side threads while we wait so a
/// chatty child filling the OS pipe buffer cannot deadlock the wait.
///
/// `Ok` carries the child's [`Output`] regardless of exit status; `Err`
/// covers spawn failure, wait failure, and timeout.
pub(crate) fn run_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so the kill below sweeps grandchildren too
        // (credential helpers, askpass, hooks) instead of orphaning them
        // with inherited pipe handles.
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to spawn {program}: {err}"))?;
    let pid = child.id();

    let (drain_tx, drain_rx) = mpsc::channel();
    let mut pending_drains = 0usize;
    if let Some(stdout) = child.stdout.take() {
        spawn_drain(stdout, 0, drain_tx.clone());
        pending_drains += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_drain(stderr, 1, drain_tx.clone());
        pending_drains += 1;
    }
    drop(drain_tx);

    let (status_tx, status_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = status_tx.send(child.wait());
    });

    match status_rx.recv_timeout(timeout) {
        Ok(Ok(status)) => {
            let (stdout, stderr) = collect_drains(&drain_rx, pending_drains);
            Ok(Output {
                status,
                stdout,
                stderr,
            })
        }
        Ok(Err(err)) => Err(format!("failed to wait on {program}: {err}")),
        Err(_) => {
            kill_process(pid);
            // Reap so the killed child doesn't linger as a zombie; SIGKILL
            // makes the wait thread return promptly.
            let _ = status_rx.recv_timeout(Duration::from_secs(2));
            let _ = collect_drains(&drain_rx, pending_drains);
            Err(format!("{program} timed out after {}s", timeout.as_secs()))
        }
    }
}

fn spawn_drain<R: Read + Send + 'static>(mut pipe: R, tag: u8, tx: mpsc::Sender<(u8, Vec<u8>)>) {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send((tag, buf));
    });
}

/// Gather drained stdout/stderr buffers, giving up after
/// [`DRAIN_COLLECT_TIMEOUT`] if a pipe is still held open (e.g. a surviving
/// grandchild on platforms without process-group kill). Abandoned drain
/// threads exit whenever EOF finally arrives.
fn collect_drains(drain_rx: &mpsc::Receiver<(u8, Vec<u8>)>, pending: usize) -> (Vec<u8>, Vec<u8>) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let deadline = Instant::now() + DRAIN_COLLECT_TIMEOUT;
    let mut pending = pending;
    while pending > 0 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match drain_rx.recv_timeout(remaining) {
            Ok((0, buf)) => {
                stdout = buf;
                pending -= 1;
            }
            Ok((_, buf)) => {
                stderr = buf;
                pending -= 1;
            }
            Err(_) => break,
        }
    }
    (stdout, stderr)
}

#[cfg(unix)]
fn kill_process(pid: u32) {
    unsafe {
        // Negative pid targets the whole process group; the child is its
        // leader. Belt-and-braces direct kill in case group signalling is
        // unavailable (ESRCH etc.).
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

// No process-group API in std; without a kill the timeout path can only
// abandon the child (bounded by the drain collection above). Acceptable:
// tmux sidebars effectively only run on Unix.
#[cfg(not(unix))]
fn kill_process(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn run(args: &[&str], timeout: Duration) -> Result<Output, String> {
        let mut command = Command::new(args[0]);
        command.args(&args[1..]);
        run_with_timeout(&mut command, timeout)
    }

    #[test]
    fn captures_stdout_stderr_and_status() {
        let output = run(
            &["sh", "-c", "echo out; echo err >&2; exit 3"],
            Duration::from_secs(5),
        )
        .expect("command succeeds");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "err\n");
    }

    #[test]
    fn missing_binary_reports_spawn_failure() {
        let err = run(
            &["definitely-missing-binary-xyz", "--flag"],
            Duration::from_secs(5),
        )
        .expect_err("spawn must fail");
        assert!(err.contains("failed to spawn"), "unexpected: {err}");
    }

    #[test]
    fn kills_command_that_outlives_timeout() {
        let start = Instant::now();
        let err =
            run(&["sleep", "30"], Duration::from_millis(150)).expect_err("sleep must time out");
        assert!(err.contains("timed out"), "unexpected: {err}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn kills_whole_process_group_so_drains_finish() {
        // The backgrounded `sleep` inherits the shell's stdout pipe; it only
        // releases it (giving the drain threads EOF) if the group kill works.
        let start = Instant::now();
        let err = run(&["sh", "-c", "sleep 30 & wait"], Duration::from_millis(150))
            .expect_err("shell must time out");
        assert!(err.contains("timed out"), "unexpected: {err}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
