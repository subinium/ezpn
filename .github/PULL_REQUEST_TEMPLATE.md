<!--
PR title MUST follow Conventional Commits:
  feat(scope): ...   fix(scope): ...   perf(scope): ...   refactor(scope): ...
  chore(scope): ...  docs(scope): ...  test(scope): ...   ci(scope): ...

Scope examples: daemon, render, layout, input, copy-mode, config, protocol, repo
-->

## Summary

<!-- 1–3 bullets. What changed and why. The diff shows what — body explains why. -->

-

## Linked issues

<!-- Closes #123  /  Refs #456 -->

Closes #

## Type of change

- [ ] feat — new user-facing capability
- [ ] fix — bug fix
- [ ] perf — performance improvement (include before/after)
- [ ] refactor — structure only, no behavior change (Tidy First)
- [ ] docs / chore / test / ci

## Pre-CI checklist (run locally before pushing)

- [ ] `cargo fmt -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test`
- [ ] `cargo build --release`
- [ ] `cargo bench` (if perf-related; attach before/after numbers)

## Behavior verification

<!-- For changes that touch the daemon / IPC / PTY / render: how did you verify? -->

- [ ] Manual attach/detach loop, ≥ 3 iterations
- [ ] Resize loop (small ↔ large), no flicker / no panic
- [ ] Multi-client attach (Shared mode), if applicable
- [ ] SIGTERM/SIGHUP graceful shutdown, if applicable

## Risk / blast radius

<!-- Anything that could break existing sessions, snapshot compatibility, or .ezpn.toml schema. -->

- [ ] No snapshot schema change, OR migration added (`workspace::migrate_*`)
- [ ] No wire-protocol change, OR version bump + handshake compatibility
- [ ] No `.ezpn.toml` breaking change, OR documented in CHANGELOG

## Docs

- [ ] README.md updated (if user-facing)
- [ ] All `docs/README.{ko,ja,zh,es,fr}.md` synced
- [ ] CHANGELOG.md `[Unreleased]` entry added (functional-only style)

## Reviewer focus

<!-- Tell the reviewer what to look at hardest. -->

-
