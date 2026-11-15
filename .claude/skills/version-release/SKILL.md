---
name: version-release
description: Use when bumping version, creating a release tag, or pushing a version tag. Triggers on "version up", "release", "tag push", "bump version".
---

# Version Release

Release mechanics are automated by scripts/create-release: it bumps
Cargo.toml, .claude-plugin/plugin.json, and Cargo.lock in lockstep (via
make bump), runs make ci, commits and pushes the bump, tags vX.Y.Z, pushes
the tag, and waits for the GitHub Actions Release workflow to build the four
platform binaries and publish the release. The agent's job is choosing the
bump type; the script does the rest.

## Workflow

1. **Propose the next version from commits since the last tag**:
   - Find the previous release tag: `git describe --tags --abbrev=0 --match 'v*'`
   - List every commit since that tag: `git log <prev-tag>..HEAD --oneline`
   - Classify the commits using the Bump Policy below, decide `patch` /
     `minor` / `major`, and present the proposed version number to the user
     **before** running anything. Do not skip this step even for a single
     commit — the user should always see the commit range and the
     classification you used.
   - If the range is empty (no commits since the last tag), stop and tell
     the user there is nothing to release.
2. **Cut the release**: `scripts/create-release <patch|minor|major> -y`
   from anywhere in the repo. It prints the plan, asks for confirmation
   unless -y, and finishes by printing the release URL and verifying all
   four binary assets are attached.
3. **Report the release URL** to the user. If the build fails, surface the
   cause from the logs command the script prints
   (`gh run view <id> --log-failed`).

## Bump Policy

| Signal in commit range                                                | Bump  | Example       |
|-----------------------------------------------------------------------|-------|---------------|
| Breaking change (`feat!:`, `BREAKING CHANGE` footer)                  | major | 1.x → 2.0.0   |
| Any `feat:` / new user-visible behavior                               | minor | 1.1.3 → 1.2.0 |
| Only `fix:` / `refactor:` / `chore:` / `docs:` / test-only            | patch | 1.1.3 → 1.1.4 |

Rules of thumb when classifying:

- Treat any `feat:` commit as a minor bump trigger, even if the rest are
  fixes.
- Docs-only or CI-only commit ranges still ship as a patch (never skip the
  version bump — the tag is what the Claude Code plugin manifest and plugin
  managers key off).
- When in doubt between patch and minor, show the commit list and ask the
  user.

## Flags

- `--no-bump` — release the version already in Cargo.toml without a bump
  commit.
- `--no-watch` — push the tag and exit; don't wait for the build.
- `--no-checks` — skip make ci (avoid; CI runs the same checks).

## Notes

- A partially-completed run resumes itself: if the tag already exists and
  points at HEAD, the script skips the bump and continues the release. If
  the tag points anywhere else, that is a hard failure.
- .claude-plugin/plugin.json must stay in lockstep with Cargo.toml —
  tests/plugin_hooks_tests.rs enforces this (Claude Code uses the manifest's
  version for update detection). The script bumps both via make bump.
- The script refuses to run on a dirty tree, a disabled Release workflow, or
  an already-published release; follow its printed instructions.
- Tags use the `v` prefix (e.g., `v1.2.0`).
