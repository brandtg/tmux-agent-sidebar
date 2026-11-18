# Security Policy

## Supported versions

Security fixes land on the latest release line (`v1.3.x` at the moment).
Older releases get fixes best-effort at most — users on older versions should
update through their plugin manager or the install wizard.

## Reporting a vulnerability

Please do not open a public issue for anything security-relevant.

Use GitHub's private vulnerability reporting: on the repository page,
**Security → Report a vulnerability** (or start a private security advisory).
This reaches the maintainer directly and keeps the detail out of the public
tracker until a fix ships.

Include what you can of:

- affected version (`tmux-agent-sidebar --version`) and `tmux -V`
- OS and terminal
- a minimal reproduction or PoC, and the impact you see
- whether the plugin manager / install method matters

## Scope notes for this project

- The binary shells out to `tmux` and `git` throughout. Injection-class
  findings (unquoted interpolation reaching an argv vector) are in scope and
  taken seriously — the codebase's standing invariant is that all tmux/git
  invocations are argv vectors, never shell strings.
- The hook pipeline ingests JSON from five different agent CLIs; deserialization
  surprises are in scope.
- The activity log and debug trace are written to the runtime dir with
  hardened opens (O_NOFOLLOW, 0600); anything that widens that surface is in
  scope.

## What to expect

Best-effort response from a single maintainer — acknowledgment within a few
days, a fix or a plan as fast as the severity warrants. Credit in the release
notes unless you prefer otherwise.
