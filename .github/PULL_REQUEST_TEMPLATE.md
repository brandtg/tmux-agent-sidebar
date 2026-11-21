## Summary

<!-- What changed, in a sentence or two. -->

## Motivation

<!-- Why: the problem, the linked issue, or the track/batch this belongs to (docs/tmux-3.8.md, docs/superpowers/*.md). -->

## How tested

<!-- Which of these ran green locally, and anything non-obvious you did. -->

- [ ] `make ci` (fmt-check, clippy -D warnings, tests, shellcheck, tmux integration tests)
- [ ] `node --test .opencode/plugins/tmux-agent-sidebar.test.mjs` (OpenCode bridge changes)
- [ ] `cargo insta accept` run for intentional UI changes (snapshots included in the PR)
- [ ] Verified in a live tmux (binary copied into the plugin's bin/, sidebar toggled off → on)

## Checklist

- [ ] `cargo fmt` run before the last commit
- [ ] Conventional commit subject — it becomes the release-notes line
- [ ] Docs updated if user-visible (website/src/content/docs) or if this lands work from a track (docs/*.md progress log)
