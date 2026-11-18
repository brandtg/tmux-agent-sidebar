# OSS Hygiene Track

Living record for the contributor-experience and project-hygiene work identified
in the 2026-11-18 review (after the 2026-11-16 audit's bug/jank batches all
landed). The bug surface is covered by docs/audit-2026-11-16.md; this track is
everything that makes the adopted project smoother for outside contributors and
safer to release.

Method: parallel review of CI, Cargo metadata, the release pipeline, contributor
docs, test seams, and the docs tree, with every finding re-checked against the
current bytes before being recorded here.

Status: **H1, H2 done** — see progress log. H3-H4 open.

## Why this matters

The project ships four release targets (darwin-aarch64, darwin-x86_64,
linux-x86_64, linux-aarch64) but CI only ever runs on ubuntu-latest, so macOS
breakage surfaces at release time, not at PR time. The shell layer — the
primary fresh-install failure surface — has no linter. And there is no
contributor funnel at all: no CONTRIBUTING.md, no templates, no SECURITY.md.

## Batch H1 — CI hardening (highest value, mechanical)

- [x] macOS CI leg. .github/workflows/ci.yml:9 runs a single
      ubuntu-latest job; darwin is a first-class release target (release.yml
      build matrix) and carries macOS-specific behavior (ad-hoc codesign per
      AGENTS.md, xattr strip in install-wizard.sh:77-82, arboard clipboard).
      A `runs-on: macos-latest` job running `cargo test` catches platform
      breakage before a release build does.
- [x] Tighten clippy: `cargo clippy --all-targets -- -D warnings`. Current CI
      runs bare `cargo clippy` (ci.yml:14), so warnings never fail the build
      and the ~5.4k lines under tests/ are never linted at all.
- [x] Add cargo-audit (or cargo-deny with an advisories section) as a CI step.
      Renovate has `:enableVulnerabilityAlerts` (renovate.json:6) but that is
      advisory-only; there is no CI gate and no deny.toml.
- [x] Add shellcheck over the shell layer in CI: hook.sh, install-wizard.sh,
      shell-exit.sh, tmux-agent-sidebar.tmux, scripts/build-assets.sh,
      fixtures/scenarios/common/_lib.sh. Zero shellcheck hits anywhere in the
      repo today. Expect an initial cleanup pass; add suppresions deliberately
      rather than ignoring the whole file.
- [x] Run the two `#[ignore = "requires local tmux"]` integration tests in CI
      (tests/capture_tests.rs:194-289). They already isolate via TMUX_TMPDIR
      against a throwaway scoped server, which an ubuntu runner can do:
      `cargo test -- --ignored --test-threads=1`.
- [x] Website build check on PRs. deploy-website.yml:4-5 deliberately disabled
      the push trigger ("the site is disabled for now"), which also means no
      PR validation at all — a change can break the Astro build/`docs` content
      and nothing notices until a manual dispatch. Add a `website: npm run
      build` job to CI.

## Batch H2 — Contributor funnel

- [x] CONTRIBUTING.md. Should
      cover: build/test commands (cargo test / clippy / fmt, the node bridge
      suite `node --test .opencode/plugins/tmux-agent-sidebar.test.mjs`),
      `make ci`, the `#[ignore]`d tmux integration tests and how to run them,
      the debug-vs-deployed checkout rules from AGENTS.md (condensed), and the
      `cargo fmt` before every commit rule.
- [x] Issue + PR templates (.github/ISSUE_TEMPLATE, PULL_REQUEST_TEMPLATE).
      Useful prompts: tmux version (`tmux -V`), OS, agent(s) in use, terminal —
      the plugin's behavior genuinely differs across tmux 3.2/3.3/3.8.
- [x] SECURITY.md with a private-contact route.
- [x] CHANGELOG policy. Release notes are commit-list style only
      (release.yml:270, `git log --pretty=format:"- %s"`); there is no
      in-repo CHANGELOG.md. Either adopt one or document the Releases-only
      policy in CONTRIBUTING.md so nobody invents a competing convention.
- [x] README Development section (README.md:86-99): add `make ci`, the bridge
      node suite, and the ignored integration tests.

## Batch H3 — Metadata and small correctness

- [ ] `rust-version` in Cargo.toml. Edition 2024 implies rustc >= 1.85 but
      nothing pins it; the install wizard's build-from-source path surfaces a
      confusing rustc error on older toolchains. Consider rust-toolchain.toml
      alongside it (pinning vs floor is a one-line policy decision).
- [ ] .claude-plugin/plugin.json:4 description still reads "Monitor Claude
      Code agents across tmux panes" while the plugin tracks five agents
      (README.md:3, 9). A `fix-plugin-description` branch existed; it never
      landed here. Align the wording.
- [ ] .gitignore: lines 5 and 9 both list `*.pending-snap` (line 5 predates
      the explanatory comment block). Drop the stray line 5. Also
      `.opencode/worktrees` appears at both line 14 (bare) and line 22 (with
      comment) — keep the commented one.
- [ ] Delete the stray `default` tmux socket file at the repo root (untracked,
      dated Oct 26 — residue of an earlier scoped-server test that didn't use
      a private TMUX_TMPDIR). Nothing should ever write a socket into the
      source tree; worth a one-line note in the scoped-server testing section
      of CONTRIBUTING.md.
- [ ] Optional, cosmetic only: `keywords`/`categories` in Cargo.toml (only
      relevant if crates.io publishing is ever intended), .editorconfig.

## Batch H4 — Testing seams (start after H1)

- [ ] hook.sh end-to-end test: pipe a representative JSON payload per agent
      through hook.sh into the CLI and assert the pane-option writes. Today
      hook.sh is only indirectly covered (plugin_hooks_tests.rs:83-88 pins the
      quoting of its command line; no test executes it with a payload).
- [ ] Consider reusing fixtures/scenarios/common/_lib.sh's `start_sidebar`
      helper (fixtures/scenarios/common/_lib.sh:24) for a real TUI smoke test
      against a scoped server. The helper already exists for capture
      generation; nothing in the test suite reuses it.

## Explicitly not doing

- Dependency freshness work: deps are exact-pinned (Cargo.toml:15-28) with
  Renovate pin-range + automerge (renovate.json:8-31) and Cargo.lock is
  committed. Deliberate and maintained — leave it.
- Binary signing/notarization for macOS releases: install-wizard.sh:77-82
  already strips quarantine and ad-hoc signs. Real signing needs an Apple
  Developer identity and CI secrets — a maintainers' decision, not hygiene.
- Moving docs/: the dated audit/handoff files are internal working notes; the
  user-facing docs live in website/src/content/docs. Splitting them is churn
  without a payoff; if the repo grows more internal docs, revisit.

## Progress log

- 2026-11-18 — Track created from the post-audit OSS-hygiene review. All
  batches open. Findings re-verified against main @ 566eeae (v1.3.3).
- 2026-11-18 — Batch H1 complete (worktree h1-ci-hardening). Details:
  - ci.yml rewritten into six parallel jobs: test (ubuntu; clippy tightened
    to `--all-targets -- -D warnings`), test-macos (cargo test only — the
    platform canary), lint-shell (runner's preinstalled shellcheck 0.9.0, no
    install step), tmux-integration (apt-installs tmux, runs the two ignored
    capture tests with `-- --ignored --test-threads=1`), website-build
    (npm ci + astro build on every PR), audit (rustsec/audit-check v2.0.0
    pinned by SHA). Added a PR concurrency group with cancel-in-progress.
  - Shellcheck wave was 10 findings, all fixed rather than suppressed except
    one deliberate disable: SC2162 `read -r` in install-wizard.sh:34;
    SC2148 `# shellcheck shell=bash` directives on shell-exit.sh and
    _lib.sh (sourced libraries); SC2317 disable-comment on shell-exit.sh's
    `return 0 || exit 0` guard (reachable only when executed, not sourced —
    the line is pinned verbatim by shell_script_tests.rs, so restructuring
    it would break the test for no behavioral gain); SC2206 quoting of the
    zsh precmd/preexec array appends in shell-exit.sh:98-99 (array elements
    are literal hook names, quoting is safe); four SC2155 declare/assign
    splits in _lib.sh (MAIN_LOG/HERMES_LOG/FOCUSED_LOG exports, painted
    local).
  - clippy wave was zero: the crate (including tests/) is already clean
    under `--all-targets -- -D warnings`; the CI tightening is pure config.
  - Makefile: clippy target now matches CI's flags; new lint-shell and
    test-ignored targets; make ci runs all five checks (website build
    intentionally left out of make ci — separate toolchain, `cd website &&
    npm ci && npm run build` when touching website/).
  - Verified locally before commit: make ci green, website build green,
    shellcheck clean at style severity on all six scripts, 1,787 + 2
    integration tests passing.
  - Known first-run risks: the macOS job may surface real platform test
    failures (that is its purpose — triage in a follow-up); the audit job
    fails if the lockfile carries any advisory — pre-checked locally with
    cargo-audit before merge: 0 vulnerabilities, 5 warnings that do not
    fail the run (unsound: anyhow, lru, rand, rustix; yanked:
    clipboard-win).
- 2026-11-18 — Batch H2 complete (worktree h2-contributor-funnel). Details:
  - CONTRIBUTING.md: prereqs, the make target table (including lint-shell
    and test-ignored), the bridge Node suite, CI's six jobs, UI-snapshot
    testing rules, the scoped-tmux-server hard rule, shell-change rules,
    conventional-commit style with the release-notes-from-subjects policy
    (the changelog decision), a condensed test-your-build section pointing
    at AGENTS.md for the full playbook, and the English-docs guideline.
  - Issue templates as GitHub forms: bug_report.yml (sidebar version,
    tmux -V, OS dropdown, agent multi-select, terminal, repro steps, debug
    trace pointer), feature_request.yml (problem/proposal/alternatives +
    tmux version), config.yml with blank issues disabled. PR template with
    a how-tested checklist mirroring make ci.
  - SECURITY.md: supported line (latest 1.3.x), GitHub private vulnerability
    reporting as the channel, and a scope note listing the standing
    invariants (argv vectors everywhere, hardened log opens) so reports can
    target the right classes.
  - README Development section expanded (make ci, integration tests, bridge
    suite) with a pointer to CONTRIBUTING.md.
  - Templates validated: all three issue YAMLs parse (js-yaml); make ci
    green (1,789 passing incl. integration tests).
