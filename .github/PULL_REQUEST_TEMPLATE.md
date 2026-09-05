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

- [ ] `python3 scripts/preflight.py --mode ci --ssh optional`
- [ ] Locked default and all-feature test counts attached, zero ignored/failed
- [ ] Genuine declared MSRV and strict all-feature Clippy passed
- [ ] SSH result marked PASS, FAIL or explicitly unavailable (not assumed)
- [ ] Performance evidence attached; missing or interrupted comparisons remain pending
- [ ] Default/all-feature soak artifacts identify executable hash and duration
- [ ] Coverage includes subprocesses and bundled source; floors unchanged

## Behavior verification

<!-- For changes that touch the daemon / IPC / PTY / render: how did you verify? -->

- [ ] Real PTY attach/detach loop, >= 3 iterations; shell PID/state retained
- [ ] Resize loop (small to large), in-bounds output and no panic
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
- [ ] CHANGELOG.md entry added (functional-only style; release section when cutting a release)

## Release Gates

- [ ] Exact PR head passed required CI, fuzz, coverage, performance and soak
- [ ] No merge/tag/publication before required gates pass
- [ ] Registry publishing has one owner; release artifacts verified separately

## Reviewer focus

<!-- Tell the reviewer what to look at hardest. -->

-
