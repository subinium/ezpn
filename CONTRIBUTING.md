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

MSRV: **Rust 1.88.0**, tested explicitly rather than through the pinned development toolchain.

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
   python3 scripts/preflight.py --mode ci --ssh optional
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
- Releases are cut from verified `main` via immutable annotated tags (`vX.Y.Z`). The Release workflow is the single publisher; do not also run local `cargo publish`.
- See [`MAINTENANCE.md`](MAINTENANCE.md) for the full release pipeline.

## Module anatomy

The `src/` tree is grouped by concern, not by layer. Start at `main.rs` and follow the dispatch arrows.

```
src/
├── main.rs                # executable dispatch and terminal prerequisites
├── cli.rs                 # shared checked argument parser and help
├── attach.rs              # CLI session/workspace commands and read-only doctor
├── bootstrap.rs           # foreground runtime, pane lifecycle and shared state
├── server/
│   ├── mod.rs             # daemon lifecycle, PTY drain and paced rendering
│   ├── handshake.rs       # bounded accept/handshake workers
│   ├── connection.rs      # client input and ordered byte-bounded output
│   ├── input_modes.rs     # modal keyboard handling
│   ├── actions.rs         # typed command dispatch
│   ├── mouse.rs           # click, selection and mouse-report routing
│   ├── ext_handlers.rs    # extended control commands
│   ├── render_glue.rs     # frame composition and overlays
│   └── status_bar.rs      # bounded declarative status-bar formatting
├── client.rs              # attach client
├── layout.rs              # layout tree, navigation, separator hit-testing
├── pane.rs                # PTY-backed pane
├── render.rs              # crossterm render passes + border cache
├── settings.rs            # in-app settings panel
├── theme.rs               # TOML theme + truecolor/256/16 downgrade
├── workspace.rs           # snapshot save/load, TabSnapshot, PaneSnapshot
├── tab.rs                 # multi-tab manager
├── project.rs             # .ezpn.toml layout and launch resolution
├── env_interp.rs          # expansion and external-value sensitivity tracking
├── session.rs             # session naming, socket paths, auto-attach
├── ipc.rs                 # ezpn-ctl IPC channel
├── protocol.rs            # wire protocol (client ↔ daemon)
├── signals.rs             # signal wakeups and handler lifetime
├── terminal_state.rs      # streaming terminal extension state
├── vt100/                 # private MIT parser with documented boundary fixes
├── copy_mode.rs           # tmux-style copy mode
├── config.rs              # ~/.config/ezpn/config.toml loader
└── bin/
    └── ezpn-ctl.rs        # `ezpn-ctl` companion binary
```

Keep shared pane lifecycle helpers in `bootstrap.rs`, daemon-only rendering in
`server/render_glue.rs`, and CLI session commands in `attach.rs`. Follow existing
ownership boundaries and avoid unrelated extraction while fixing behavior.
Changes to bundled `vt100` require upstream provenance and real boundary
regressions; do not silently replace it with an unpatched registry dependency.

## Code of conduct

Be direct. Be technical. Don't be a jerk. That's the whole rule.
