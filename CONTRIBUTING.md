# Contributing

Thanks for helping out. This is a single-binary Rust TUI plus a shell/Node
wiring layer, distributed as a tmux plugin. The codebase is small and heavily
tested; this doc gets you productive in ten minutes.

## Prerequisites

- Rust (edition 2024 — any recent stable toolchain)
- Node.js (only for the OpenCode bridge test suite)
- tmux (only for the two integration tests)
- shellcheck (optional locally — CI runs it)

## Everyday commands

```sh
make build         # release binary (strip + lto)
make test          # full test suite; ARGS filters, e.g. make test ARGS=run_with_timeout
make clippy        # lint with CI's enforcement flags (--all-targets -D warnings)
make fmt           # auto-format — run this before every commit
make lint-shell    # shellcheck over the shell layer, same command CI runs
make test-ignored  # the #[ignore]d tmux integration tests (see below)
make ci            # everything CI runs: fmt-check + clippy + tests + shellcheck + integration tests
```

The OpenCode bridge (`.opencode/plugins/tmux-agent-sidebar.js`) has its own
Node test suite, run separately from cargo:

```sh
node --test .opencode/plugins/tmux-agent-sidebar.test.mjs
```

CI runs six jobs on every PR (see `.github/workflows/ci.yml`): the Rust suite
with clippy and fmt on Linux, the same Rust suite on macOS, the bridge Node
suite, shellcheck, the tmux integration tests, the website build, and a
RustSec advisory scan. `make ci` covers all the Rust/shell pieces locally.

## Testing notes

- UI tests render frames with Ratatui's `TestBackend` and assert against
  inline snapshots via `insta::assert_snapshot!`. If a UI change is
  intentional, accept the new snapshots with `cargo insta accept` and include
  them in the PR — never downgrade a snapshot test to a substring check.
- Two capture tests are `#[ignore = "requires local tmux"]`; they drive the
  real binary against a throwaway tmux server isolated via `TMUX_TMPDIR`.
  Run them with `make test-ignored` (single-threaded — they mutate
  process-global state).

### tmux safety (hard rule)

Your tmux server holds real work, and so does every contributor's. Never run
bare `tmux kill-server`, `kill-session`, `kill-pane`, or `kill-window` — any
destructive tmux command needs an explicit socket scope:

```sh
tmux -L my-scratch new-session -d -s test   # scoped server
tmux -L my-scratch kill-server              # scoped cleanup, flag always required
```

Never `pkill`/`killall` on patterns that could match a live tmux server or
sidebar process. And never let a test create a socket in the source tree —
tmux resolves sockets relative to `$TMUX_TMPDIR`, so a bare `tmux` call from
a repo checkout can drop a `default` socket file right into it (this has
happened once; it was cleaned up in the same change that added this note).

## Shell changes

`make lint-shell` must pass at style severity. The two sourced libraries
(`shell-exit.sh`, `fixtures/scenarios/common/_lib.sh`) carry
`# shellcheck shell=bash` directives; keep new findings fixed or suppressed
with a per-line comment explaining why — no whole-file ignores.

## Commit and PR style

Run `cargo fmt` before every commit — CI checks formatting and a format
failure wastes a review cycle. Commit subjects use conventional prefixes
(`feat:`, `fix:`, `perf:`, `docs:`, `ci:`, `chore:`) in the imperative mood.
Keep subjects clean: release notes are generated from commit subjects at tag
time (see below), so the subject is the user-visible changelog line.

PRs should say what changed, why, and how it was tested. UI changes should
show before/after snapshots or captures.

## Testing your build in a real tmux

The installed plugin (`~/.tmux/plugins/tmux-agent-sidebar`) is a git clone of
the published release, not a symlink to your checkout. To try local changes:

```sh
cargo build --release
cp target/release/tmux-agent-sidebar ~/.tmux/plugins/tmux-agent-sidebar/bin/
```

then toggle the sidebar off → on. If hook registration or option defaults
changed, copy `agent-sidebar.conf` too (hooks register once at conf load).
If you changed the OpenCode bridge, also copy
`.opencode/plugins/tmux-agent-sidebar.js` into the plugin clone and restart
the OpenCode service daemon — it caches the plugin module for the process
lifetime. The full debugging playbook, including macOS re-signing and the
version-check gotcha, lives in [AGENTS.md](AGENTS.md).

## Changelog policy

There is no in-repo CHANGELOG.md; each GitHub release's notes are generated
from commit subjects between tags. Well-written subjects are the policy.

## Project docs

- User docs live in `website/src/content/docs` (Starlight site).
- `docs/` holds internal engineering notes: dated audits, the state-management
  deep-dive, and the two living work tracks (`oss-hygiene.md`,
  `tmux-3.8.md`). Update a track's checkboxes and progress log when you land
  work from it.
- All documentation is written in English.

## License

By contributing you agree that your contributions are licensed under the
project's [MIT license](LICENSE).
