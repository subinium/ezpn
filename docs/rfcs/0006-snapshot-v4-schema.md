# RFC 0006 — Snapshot v4 schema

| | |
|---|---|
| **Status** | Proposed |
| **Tracks issue** | #106 |
| **Supersedes on disk** | Snapshot v3 (still readable; v4 is additive) |
| **Depends on** | #91 (named copy buffers — landed in v0.13.0), #85 (theme system — landed in v0.13.0), RFC 0004 (`HistoryRow` shape for round-trip) |
| **Required for** | every v0.14 / v0.15 feature wanting persistence |
| **Owner** | @subinium |

## Summary

Snapshot v3 (current `SNAPSHOT_VERSION = 3`, see `src/workspace.rs:17`) added per-pane scrollback persistence (#69). v0.13.x and v0.14.x have four follow-on features that each want to persist additional state: named copy buffers (#91), theme state (#85), per-pane cwd history (#75), and a forward-looking opaque plugin-state slot.

Bumping the schema once per feature creates churn. v3 → v4 lands once, with all four slots, and the schema policy commits to **additive-only thereafter** until v1.0.

## Motivation

### Today's surface

`src/workspace.rs:17`:
```rust
pub const SNAPSHOT_VERSION: u32 = 3;
pub const MIN_SUPPORTED_VERSION: u32 = 1;
```

`src/workspace.rs:36-47` — `WorkspaceSnapshot` carries: `version`, `shell`, `border_style`, `show_status_bar`, `show_tab_bar`, `scrollback`, `active_tab`, `tabs`. None of: named buffers, theme, plugin state, cwd history.

`src/workspace.rs:69-100` — `PaneSnapshot` carries: `id`, `launch`, `name`, `cwd`, `env`, `restart`, `shell`, `scrollback` (v3 addition), `cursor_pos`. None of: cwd *history*, OSC 7 freshness, OSC 52 cached decision.

The v3 design intentionally skipped these — issue #69 was scoped to scrollback. v0.13 wiring landed #91 and #85 functionally, but their state is in-memory only; a daemon restart wipes named buffers and per-session theme overrides.

### What four bumps would cost

Without this RFC, v0.13.x → v0.16 looks like:
- v0.13.x: snapshot v4 — named buffers
- v0.14.0: snapshot v5 — theme state
- v0.14.x: snapshot v6 — cwd history
- v0.15.0: snapshot v7 — plugin slot

Four migration ladders to maintain. Four CHANGELOG breakage notes. Four `ezpn upgrade-snapshot` paths (#70). One v4 with future-proof slots is strictly cheaper.

## Design

### Schema additions

```rust
// src/workspace.rs (additions only — v3 fields unchanged)

pub const SNAPSHOT_VERSION: u32 = 4;
// MIN_SUPPORTED_VERSION stays at 1 (v0.13 N-2 window unchanged)

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    // === v1–v3 fields (unchanged, see src/workspace.rs:36-47) ===
    pub version: u32,
    pub shell: String,
    pub border_style: BorderStyle,
    pub show_status_bar: bool,
    #[serde(default = "default_true")]
    pub show_tab_bar: bool,
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    pub active_tab: usize,
    pub tabs: Vec<TabSnapshot>,

    // === v4 additions (all optional; absent on v1/v2/v3 docs) ===

    /// Named copy buffers (#91). Survives daemon restart so paste-buffer
    /// references are stable across reboots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffers: Option<NamedBufferBlob>,

    /// Last-applied theme + per-session overrides (#85).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_state: Option<ThemeStateBlob>,

    /// Forward-looking opaque plugin-state slot. Plugins are responsible
    /// for versioning the bytes they store under their key; ezpn just
    /// round-trips. Empty `HashMap` skipped on serialise.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub plugin_state: HashMap<String, Vec<u8>>,

    /// Per-pane cwd history (#75). Keyed by *current-process* pane id —
    /// not stable across restarts; cleared on snapshot load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_history: Option<HashMap<u64, Vec<PathBuf>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NamedBufferBlob {
    /// The unnamed default buffer (most-recent yank).
    pub default: Option<Vec<u8>>,
    /// Named buffers, ordered by name for stable JSON output.
    pub named: BTreeMap<String, Vec<u8>>,
    /// Last-modified epoch (seconds). Used by the LRU eviction across
    /// restarts so 100-buffer cap survives reboot.
    pub mtime: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThemeStateBlob {
    /// Theme name resolved at save time. Empty string means "system default".
    pub active_theme_name: String,
    /// Per-session override (`session_name → theme_name`). Used by the
    /// hot-reload path to restore the user's last per-session pick.
    pub per_session_override: BTreeMap<String, String>,
}
```

`PaneSnapshot` is unchanged in v4 — none of the new fields are per-pane. The `cwd_history` map is keyed by pane id at the workspace level; if/when stable pane identity lands (separate RFC), the key will move under `PaneSnapshot` in a v5 bump.

### Wire compat — additive-only

Every v4 addition is `Option<T>` or `HashMap<K,V>` with `#[serde(default, skip_serializing_if = ...)]`. Two compat directions:

1. **v3 reader on v4 doc.** The `version: 4` field is read first; if a v3 reader checks `version == 3` strictly it errors out. The reader at `src/workspace.rs:336-340` accepts `[MIN_SUPPORTED_VERSION..=SNAPSHOT_VERSION]` — it is *forward*-restrictive (v3 won't read v4). Mitigation: bump `SNAPSHOT_VERSION` to 4 only after the migration ladder lands.
2. **v4 reader on v3 doc.** All v4 additions deserialise to `None` / empty `HashMap`. v3 docs round-trip cleanly through the v4 type. This is the load-bearing direction for the `MIN_SUPPORTED_VERSION = 1` N-2 window.

The `bincode = "1.3"` pin (`Cargo.toml:49`) is **not** affected. Snapshot v4 is a JSON-level schema change inside `WorkspaceSnapshot`; the `ScrollbackBlob.payload` is still gzip-bincode and its bincode pin stays. The pin's CHANGELOG entry (`Cargo.toml:48`) explicitly says bincode v2 is a snapshot-format break — v4 does not need v2; v4 is JSON additions.

### Migration ladder

`src/workspace.rs:379` already has `migrate_v1`; v2→v3 was a no-op (scrollback is `Option`, absent on v2 docs deserialises to `None`). v3→v4 follows the same pattern:

```rust
fn migrate_v3(v3: WorkspaceSnapshot) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        version: 4,
        // v4 additions all default; daemon populates them on next save
        buffers: None,
        theme_state: None,
        plugin_state: HashMap::new(),
        cwd_history: None,
        ..v3
    }
}
```

`ezpn upgrade-snapshot` (#70) gets one new arm calling `migrate_v3`. The CLI already handles the v1→v2→v3 chain; v4 appends.

### Plugin state — opaque-bytes contract

`plugin_state: HashMap<String, Vec<u8>>` is intentionally opaque. Plugins do not exist yet — the slot is forward-looking per RFC 0006's "predict the next two milestones" principle. Contract:

1. **Plugins version their own bytes.** First byte is a plugin-defined schema tag; ezpn does not parse.
2. **Unknown keys round-trip.** A plugin uninstalled between save and load leaves a key in `plugin_state` that no consumer reads; the snapshot still loads. The orphan key persists across saves until explicitly cleared via `ezpn plugin-state purge <key>` (future CLI).
3. **Size cap.** Each plugin's `Vec<u8>` is capped at 1 MiB at write time (warn on overflow, hard-fail at 4 MiB). Prevents a misbehaving plugin from inflating snapshots.

Until the plugin SDK lands, this slot is unused but reserved. If RFC 0006's prediction is wrong (plugin SDK never ships), the slot stays empty and `skip_serializing_if = "HashMap::is_empty"` keeps it off disk.

### CWD history — pane-id key caveat

`cwd_history: HashMap<u64 /*pane_id*/, Vec<PathBuf>>` is keyed by the **current daemon's** pane id. Pane ids are sequential allocations starting at 0 each daemon start; they are not stable across restarts. The contract:

- **Save path**: dump the current map verbatim.
- **Load path**: ignore the map (clear after deserialisation). The history is for the live daemon's lifetime only.

This is a deliberate v4 limitation. Stable pane identity is its own design problem (a UUID or content-derived hash) and out of scope. The slot exists so v0.14's OSC 7 history feature has somewhere to land without a v5 bump; the load-side clear means snapshots are smaller and cheaper while still allowing in-process state to survive `ezpn save && ezpn load` within one daemon lifetime.

## Risks & Mitigations

| Risk | Impact | Mitigation | Verify In Step |
|---|---|---|---|
| `plugin_state` round-trips garbage when plugin ABI changes | Snapshot loads but plugin state corrupted | Plugins version their own bytes (contract above); ezpn returns Err if plugin's deserialiser fails, leaving the rest of the workspace intact | step 4 |
| `cwd_history` pane ids unstable across restarts | Confusing UX (history "disappears" after load) | Document load-side clear; emit `tracing::debug!` event when load drops cwd_history | step 5 |
| Schema-creep — additional v4 slots get squeezed in during PR review | v4 ships bloated and brittle | Lock the four slots in this RFC; new v4 fields require a follow-up RFC and explicit roadmap link | RFC review |
| `NamedBufferBlob.named` byte-size unbounded on disk | Multi-GB snapshots from a user with 100 huge buffers | Per-buffer cap stays 16 MiB (already enforced in-memory by #91); add validate() warning at 50 MB total | step 3 |
| v3 → v4 migration accidentally loses fields | Silent corruption | Property test: random v3 doc → migrate → re-encode → JSON-equal to original v3 doc on overlapping fields | step 6 |

## Implementation Steps

| # | Step | Files | Depends On | Scope |
|---|------|-------|------------|-------|
| 1 | Write `docs/protocol/snapshot-v4.md` (byte-level layout, slot policy, plugin-state contract) | `docs/protocol/snapshot-v4.md` | — | M |
| 2 | Add `NamedBufferBlob`, `ThemeStateBlob` types to `src/workspace.rs` | `src/workspace.rs` | 1 | S |
| 3 | Wire `buffers`, `theme_state`, `plugin_state`, `cwd_history` fields into `WorkspaceSnapshot` | `src/workspace.rs` | 2 | S |
| 4 | Update `from_live` capture path to populate the new fields from live state | `src/workspace.rs` | 3, in-tree #91 + #85 | M |
| 5 | Update load path: hydrate buffers + theme; clear `cwd_history` per contract | `src/workspace.rs`, `src/server/mod.rs` | 4 | S |
| 6 | Property test — round-trip + v3 forward compat | `tests/property/snapshot.rs` | 3 | S |
| 7 | Bump `SNAPSHOT_VERSION = 4` only after 6 passes | `src/workspace.rs` | 6 | S |
| 8 | Extend `ezpn upgrade-snapshot` (#70) with v3 → v4 arm | `src/bin/ezpn-ctl.rs` (or wherever `upgrade-snapshot` lives) | 7 | S |
| 9 | RFC 0004 `HistoryRow` round-trip integration (lossless SGR) | `src/workspace.rs` | RFC 0004, 7 | M |

Steps 2 and 6 form parallel groups within the same module — order them sequentially per file ownership.

## Acceptance criteria (per issue #106)

- [ ] `docs/protocol/snapshot-v4.md` written before any code change.
- [ ] `workspace.rs` migration ladder gains `migrate_v3` (sets `version: 4`, leaves new fields `None` / `HashMap::new()`).
- [ ] Round-trip property test: random v3 doc → migrate to v4 → re-encode → decode produces equivalent state.
- [ ] `SNAPSHOT_VERSION = 4` only after all migration tests pass.
- [ ] `ezpn upgrade-snapshot` (#70) extended to handle v3 → v4.
- [ ] v4 readers continue loading v3 docs (additive-only) — verified via property test on a v3 corpus.

## Open Questions

- **Encrypted snapshots** — out of scope here; if it lands, the encryption envelope wraps the entire JSON and is a v5 schema event, not a v4 slot. Captured as future work.
- **`cwd_history` value type** — `Vec<PathBuf>` is unbounded; cap at last-50 entries per pane to prevent runaway growth on long-running shells. Hard-coded at the source level (collector trims).
- **Multi-user snapshots** — today snapshot is per-user. v4 schema adds nothing here; if multi-user lands it gets its own envelope.
- **Theme palette in `ThemeStateBlob`** — should we persist the resolved RGB palette or just the theme name? Persisting the name is forward-compat with theme-asset updates; persisting the palette pins the user to the saved colours regardless of asset changes. Pick name; document the trade-off.

## Decision Path / Recommendation

**Adopt.** Ship v4 in v0.13.x as a single schema bump covering all four slots. Lock additive-only policy for v0.14–v0.15.

### Numbers

- **v3 → v4 migration overhead per snapshot**: zero new bytes on disk for unused slots (`skip_serializing_if`). Adds ~80 bytes JSON envelope when all four slots populated with empty defaults.
- **Read-time cost**: one extra `serde` field per slot. Negligible (< 1 ms for typical 100 KiB snapshots).
- **Plugin-state cap**: 1 MiB warn, 4 MiB hard-fail per plugin key. Total snapshot size remains practical (< 50 MiB) at typical use.

### Reversibility

Removing a slot before v1.0 is allowed (additive-only is forward; subtractive needs N-1 deprecation per the existing schema policy). After v1.0, slot removal is a v2 schema event.

## References

- Issue #106 — this RFC's tracking issue
- Issue #69 — snapshot v3 (current state)
- Issue #70 — `ezpn upgrade-snapshot` (gets a v3→v4 arm)
- Issue #91 — named copy buffers (consumes `NamedBufferBlob`)
- Issue #85 — theme system (consumes `ThemeStateBlob`)
- Issue #75 — OSC 7 cwd history (consumes `cwd_history`)
- RFC 0004 — vt100-independent scrollback (`HistoryRow` round-trips losslessly)
- `src/workspace.rs:17` — `SNAPSHOT_VERSION` constant
- `src/workspace.rs:28` — `MIN_SUPPORTED_VERSION` (N-2 window)
- `src/workspace.rs:36-47` — current `WorkspaceSnapshot`
- `src/workspace.rs:69-100` — current `PaneSnapshot`
- `src/workspace.rs:102-160` — `ScrollbackBlob` / `RowSnapshot` (v3)
- `src/workspace.rs:336-340` — version range check
- `src/workspace.rs:379-414` — `migrate_v1` (template for `migrate_v3`)
- `Cargo.toml:46-49` — bincode pin rationale (unchanged for v4)

Closes #106
