# Contributing to ezpn

Thanks for the interest. ezpn is an opinionated tmux successor — small surface, fast iteration, native feel. The bar for contributions is correctness + zero regressions in attach/detach/render.

## Quick start

```bash
git clone https://github.com/subinium/ezpn
cd ezpn
cargo build
cargo test
cargo run -- 2 2          # try a 2x2 grid
```

MSRV: **Rust 1.82**.

## Workflow

1. **Open an issue first** for non-trivial changes (anything that touches `daemon`, `protocol`, `render`, snapshot schema, or `.ezpn.toml` schema). Drive-by PRs without an issue may be closed.
2. **Branch from `main`.** GitHub Flow — no `develop`. Branch name matches the change type:

   | Prefix | Use for |
   |---|---|
   | `feat/` | New user-facing capability |
   | `fix/` | Bug fix |
   | `perf/` | Performance improvement |
   | `refactor/` | Structural change, no behavior change |
   | `chore/` | Build, deps, CI, release |
   | `docs/` | README, translations, comments |
   | `test/` | Tests only |

   Example: `feat/scrollback-persistence`, `fix/borderless-off-by-one`.

3. **Make the change small.** One logical change per PR. If "and" appears in the title, split it. Refactor (structural) and feature (behavioral) commits MUST be separated — Tidy First. The reviewer should be able to verify each commit independently.

4. **Run pre-CI locally before pushing:**

   ```bash
   cargo fmt -- --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   cargo build --release
   ```

   Optional but encouraged for `perf/`:

   ```bash
   cargo bench --bench render_hotpaths
   ```

   Attach before/after numbers in the PR description.

5. **Open the PR.** The template is mandatory — every checkbox is a real gate.

## Automated gates

The following CI checks run on every PR and enforce conventions described above. Get them green before requesting review:

- **Commit Lint** (`.github/workflows/commitlint.yml`) — validates the PR title against Conventional Commits and runs [`wagoid/commitlint-github-action`](https://github.com/wagoid/commitlint-github-action) on each commit. Allowed types: `feat fix perf refactor chore docs test ci style release`.
- **Branch Naming** (`.github/workflows/branch-naming.yml`) — rejects branches that don't match `<type>/<short-description>`. Auto-generated branches (`dependabot/*`, `revert-*`) are skipped.
- **PR Labeler** (`.github/workflows/labeler.yml` + `.github/labeler.yml`) — auto-applies `area:*` labels based on the changed file paths so reviewers can triage quickly.
- **Release Drafter** (`.github/workflows/release-drafter.yml` + `.github/release-drafter.yml`) — runs on every push to `main` and continuously rebuilds the next release's draft notes, grouped by `type:*` label.

## Commit messages

Conventional Commits. Subject in imperative mood, lowercase, no trailing period, ≤ 72 chars.

```
feat(server): negotiate wire-protocol version on attach

Adds a Hello/HelloAck handshake. Older clients fall back to v0
behavior; mismatched majors are rejected with a user-friendly
message instead of silent corruption.

Closes #3
```

Body explains *why*. The diff explains *what*.

## What we will reject

- PRs that mix structural and behavioral changes in one commit.
- PRs that bump MSRV without an issue + justification.
- PRs that add a dependency for a one-line replacement.
- PRs that change snapshot schema or wire protocol without a migration / version bump.
- PRs without `cargo fmt && cargo clippy && cargo test` passing.
- PRs that touch the README without syncing all `docs/README.{ko,ja,zh,es,fr}.md`.

## What we welcome

- Repro fixtures for nasty bugs (a failing test is the best PR).
- Terminal-specific compatibility fixes (with the terminal name + version in the PR body).
- Performance improvements with `criterion` numbers attached.
- Translations and translation fixes — keep the structure identical to English README.

## Release & versioning

- Semantic versioning. `0.MINOR.PATCH` until 1.0.
- Releases are cut from `main` via annotated tags (`vX.Y.Z`) + `gh release create` + `cargo publish`.
- See [`MAINTENANCE.md`](MAINTENANCE.md) for the full release pipeline.

## Code of conduct

Be direct. Be technical. Don't be a jerk. That's the whole rule.
