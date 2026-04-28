use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::pane::{Pane, PaneLaunch};
use crate::project::RestartPolicy;
use crate::render::BorderStyle;
use crate::tab::TabManager;

/// Current on-disk snapshot schema version. Bumped to 3 in v0.13 to add
/// optional scrollback persistence (#69). The reader still accepts v1 and
/// v2 (see `MIN_SUPPORTED_VERSION` and the deprecation table in
/// `CHANGELOG.md`).
pub const SNAPSHOT_VERSION: u32 = 3;

/// Oldest snapshot version still readable by the current daemon.
///
/// Per the N-2 backward window declared in #70:
/// - v0.13 (this release): reads v1, v2, v3.
/// - v0.16: drops v1.
/// - v1.0:  drops v2.
/// Anything older than this constant produces a hard error pointing the
/// user at `ezpn upgrade-snapshot`.
pub const MIN_SUPPORTED_VERSION: u32 = 1;

/// Soft warning thresholds for an oversized scrollback payload (#69 AC).
/// `validate()` warns to stderr but does not fail — the user opted into
/// persistence and may legitimately have huge logs.
const SCROLLBACK_SOFT_WARN_BYTES: u32 = 100 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
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
}

fn default_true() -> bool {
    true
}

fn default_scrollback() -> usize {
    10_000
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TabSnapshot {
    pub name: String,
    pub layout: Layout,
    pub active_pane: usize,
    #[serde(default)]
    pub zoomed_pane: Option<usize>,
    #[serde(default)]
    pub broadcast: bool,
    pub panes: Vec<PaneSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub id: usize,
    pub launch: PaneLaunch,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub shell: Option<String>,
    /// Optional gzip-compressed bincode payload of the pane's scrollback
    /// at save time (#69). Always `None` on v1/v2 snapshots; `None` on v3
    /// when `[global] persist_scrollback = false` (the default) and the
    /// per-pane `[[pane]] persist_scrollback = true` override is unset.
    ///
    /// `#[serde(default, skip_serializing_if)]` keeps v3 snapshots
    /// byte-compatible with a v2 reader when the field is absent — i.e.
    /// turning persistence off produces a JSON document that lacks the
    /// `scrollback` key entirely instead of writing `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<ScrollbackBlob>,
    /// Optional cursor position `(row, col)` captured at save time (#69).
    /// Used by the future restore path to reposition the cursor inside
    /// the replayed scrollback. Same `skip_serializing_if` rationale as
    /// `scrollback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_pos: Option<(u16, u16)>,
}

/// Compressed scrollback payload attached to a v3+ `PaneSnapshot` (#69).
///
/// Wire shape is intentionally a struct (not a raw `Vec<u8>`) so that
/// future encodings can be added without another schema bump — the
/// `encoding` discriminator is enumerated in [`ScrollbackEncoding`].
///
/// Memory layout for the default `BincodeGz` encoding:
///   payload = gzip(bincode::serialize(&Vec<RowSnapshot>))
///
/// `bytes_uncompressed` is the size of the bincode buffer **before**
/// gzip and is recorded so the reader can pre-allocate the inflate
/// buffer and so `validate()` can warn on pathological sizes without
/// having to actually decompress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScrollbackBlob {
    pub encoding: ScrollbackEncoding,
    pub rows: u32,
    pub bytes_uncompressed: u32,
    pub payload: Vec<u8>,
}

/// Wire-format discriminator for [`ScrollbackBlob::payload`]. Adding a
/// new encoding is *not* a snapshot-version bump — readers must treat
/// any unknown variant as a recoverable error and skip the scrollback
/// rather than failing the whole snapshot load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollbackEncoding {
    /// `gzip(bincode(Vec<RowSnapshot>))` — current default.
    #[default]
    BincodeGz,
}

/// One scrollback line as captured for `ScrollbackBlob` (#69).
///
/// Only the text + a packed SGR attribute byte are persisted; mouse
/// mode, alt-screen state and OSC-set palette overrides are explicitly
/// out of scope per the issue's "Risks" section.
///
/// **No `skip_serializing_if`** on these fields: bincode is a
/// non-self-describing format, so any serializer-side skip would leave
/// the deserializer expecting bytes that never appear (UnexpectedEOF).
/// Compression handles the empty-`attrs` case adequately.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowSnapshot {
    /// UTF-8 text of the row, with trailing whitespace preserved so the
    /// renderer's column width matches the original.
    pub text: String,
    /// Packed SGR attributes per cell. Empty when the row is plain.
    /// The exact bit layout is owned by the future restore path — it is
    /// stored as opaque bytes here to avoid pinning the renderer's
    /// internal `Cell` struct to the wire format.
    pub attrs: Vec<u8>,
}

impl ScrollbackBlob {
    /// Encode `rows` into a compressed blob with the default
    /// `BincodeGz` encoding. Returns the constructed blob ready for
    /// embedding in a `PaneSnapshot`.
    pub fn encode_bincode_gz(rows: &[RowSnapshot]) -> anyhow::Result<Self> {
        let raw = bincode::serialize(rows)
            .map_err(|e| anyhow::anyhow!("bincode encode scrollback: {e}"))?;
        let bytes_uncompressed = u32::try_from(raw.len()).unwrap_or(u32::MAX);
        let mut encoder = flate2::write::GzEncoder::new(
            Vec::with_capacity(raw.len() / 4),
            flate2::Compression::default(),
        );
        encoder
            .write_all(&raw)
            .map_err(|e| anyhow::anyhow!("gzip encode scrollback: {e}"))?;
        let payload = encoder
            .finish()
            .map_err(|e| anyhow::anyhow!("gzip finish scrollback: {e}"))?;
        Ok(Self {
            encoding: ScrollbackEncoding::BincodeGz,
            rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            bytes_uncompressed,
            payload,
        })
    }

    /// Decode the payload back into `Vec<RowSnapshot>`. Returns `Ok(None)`
    /// for unknown encodings so callers can degrade gracefully (load
    /// layout + commands, drop scrollback) per the encoding-discriminator
    /// contract above.
    pub fn decode(&self) -> anyhow::Result<Option<Vec<RowSnapshot>>> {
        match self.encoding {
            ScrollbackEncoding::BincodeGz => {
                let mut decoder = flate2::read::GzDecoder::new(&self.payload[..]);
                let mut buf =
                    Vec::with_capacity(self.bytes_uncompressed.min(64 * 1024 * 1024) as usize);
                decoder
                    .read_to_end(&mut buf)
                    .map_err(|e| anyhow::anyhow!("gzip decode scrollback: {e}"))?;
                let rows: Vec<RowSnapshot> = bincode::deserialize(&buf)
                    .map_err(|e| anyhow::anyhow!("bincode decode scrollback: {e}"))?;
                Ok(Some(rows))
            }
        }
    }
}

impl WorkspaceSnapshot {
    /// Create a snapshot from live state.
    ///
    /// The active tab is "unpacked" (its state is in separate variables),
    /// while inactive tabs are stored in the `TabManager`.
    #[allow(clippy::too_many_arguments)]
    pub fn from_live(
        tab_mgr: &TabManager,
        tab_name: &str,
        layout: &Layout,
        panes: &HashMap<usize, Pane>,
        active_pane: usize,
        zoomed_pane: Option<usize>,
        broadcast: bool,
        restart_policies: &HashMap<usize, RestartPolicy>,
        shell: &str,
        border_style: BorderStyle,
        show_status_bar: bool,
        show_tab_bar: bool,
        scrollback: usize,
    ) -> Self {
        let mut tabs = Vec::with_capacity(tab_mgr.count);

        for i in 0..tab_mgr.count {
            if i == tab_mgr.active_idx {
                // Active tab: build from unpacked state
                tabs.push(TabSnapshot {
                    name: tab_name.to_string(),
                    layout: layout.clone(),
                    active_pane,
                    zoomed_pane,
                    broadcast,
                    panes: snapshot_panes(layout, panes, restart_policies),
                });
            } else if let Some(tab) = tab_mgr.get_inactive(i) {
                tabs.push(TabSnapshot {
                    name: tab.name.clone(),
                    layout: tab.layout.clone(),
                    active_pane: tab.active_pane,
                    zoomed_pane: tab.zoomed_pane,
                    broadcast: tab.broadcast,
                    panes: snapshot_panes(&tab.layout, &tab.panes, &tab.restart_policies),
                });
            }
        }

        Self {
            version: SNAPSHOT_VERSION,
            shell: shell.to_string(),
            border_style,
            show_status_bar,
            show_tab_bar,
            scrollback,
            active_tab: tab_mgr.active_idx,
            tabs,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !is_supported_version(self.version) {
            anyhow::bail!(
                "unsupported snapshot version: {found} (current = {current}, \
                 minimum supported = {min}). Run `ezpn upgrade-snapshot <path>` \
                 on older files.",
                found = self.version,
                current = SNAPSHOT_VERSION,
                min = MIN_SUPPORTED_VERSION
            );
        }

        if self.tabs.is_empty() {
            anyhow::bail!("snapshot has no tabs");
        }

        for (ti, tab) in self.tabs.iter().enumerate() {
            let mut snapshot_ids: Vec<usize> = tab.panes.iter().map(|pane| pane.id).collect();
            snapshot_ids.sort_unstable();
            snapshot_ids.dedup();

            let mut layout_ids = tab.layout.pane_ids();
            layout_ids.sort_unstable();

            if snapshot_ids != layout_ids {
                anyhow::bail!("snapshot panes do not match layout leaves in tab {}", ti);
            }

            if !layout_ids.contains(&tab.active_pane) {
                anyhow::bail!(
                    "snapshot active pane does not exist in layout in tab {}",
                    ti
                );
            }

            // Soft warning for pathological scrollback payloads (#69 AC).
            // Don't fail — the user opted into persistence.
            for pane in &tab.panes {
                if let Some(blob) = &pane.scrollback {
                    if blob.bytes_uncompressed > SCROLLBACK_SOFT_WARN_BYTES {
                        eprintln!(
                            "ezpn: snapshot tab {ti} pane {pid} scrollback is \
                             {mb} MB uncompressed (soft cap {cap} MB)",
                            pid = pane.id,
                            mb = blob.bytes_uncompressed / (1024 * 1024),
                            cap = SCROLLBACK_SOFT_WARN_BYTES / (1024 * 1024),
                        );
                    }
                }
            }
        }

        if self.active_tab >= self.tabs.len() {
            anyhow::bail!("snapshot active_tab index out of range");
        }

        Ok(())
    }
}

/// True for any snapshot version this build can read. The current window
/// is `[MIN_SUPPORTED_VERSION, SNAPSHOT_VERSION]` inclusive — keep this
/// in lockstep with the deprecation table in `CHANGELOG.md`.
pub fn is_supported_version(v: u32) -> bool {
    v >= MIN_SUPPORTED_VERSION && v <= SNAPSHOT_VERSION
}

/// Build PaneSnapshot vec for a set of panes.
///
/// Scrollback (`scrollback` / `cursor_pos`) is intentionally left as
/// `None` here — capture is plumbed in by the caller after the snapshot
/// has been built, so we can keep the persistence policy
/// (`[global] persist_scrollback` plus per-pane override) outside this
/// pure-data layer.
fn snapshot_panes(
    layout: &Layout,
    panes: &HashMap<usize, Pane>,
    restart_policies: &HashMap<usize, RestartPolicy>,
) -> Vec<PaneSnapshot> {
    layout
        .pane_ids()
        .into_iter()
        .map(|id| {
            let pane = panes.get(&id);
            PaneSnapshot {
                id,
                launch: pane
                    .map(|p| p.launch().clone())
                    .unwrap_or(PaneLaunch::Shell),
                name: pane.and_then(|p| p.name().map(|s| s.to_string())),
                cwd: pane
                    .and_then(|p| p.live_cwd())
                    .map(|p| p.to_string_lossy().to_string()),
                env: pane.map(|p| p.initial_env().clone()).unwrap_or_default(),
                restart: restart_policies.get(&id).cloned().unwrap_or_default(),
                shell: pane.and_then(|p| p.initial_shell().map(|s| s.to_string())),
                scrollback: None,
                cursor_pos: None,
            }
        })
        .collect()
}

/// Migrate a v1 snapshot to v2 format.
fn migrate_v1(v1_json: &serde_json::Value) -> anyhow::Result<WorkspaceSnapshot> {
    // V1 had flat fields: shell, active_pane, border_style, show_status_bar, layout, panes
    let shell = v1_json["shell"].as_str().unwrap_or("/bin/sh").to_string();
    let active_pane = v1_json["active_pane"].as_u64().unwrap_or(0) as usize;
    let border_style: BorderStyle =
        serde_json::from_value(v1_json["border_style"].clone()).unwrap_or(BorderStyle::Rounded);
    let show_status_bar = v1_json["show_status_bar"].as_bool().unwrap_or(true);
    let layout: Layout = serde_json::from_value(v1_json["layout"].clone())?;
    let v1_panes: Vec<serde_json::Value> = v1_json["panes"].as_array().cloned().unwrap_or_default();

    let panes: Vec<PaneSnapshot> = v1_panes
        .into_iter()
        .map(|p| PaneSnapshot {
            id: p["id"].as_u64().unwrap_or(0) as usize,
            launch: serde_json::from_value(p["launch"].clone()).unwrap_or(PaneLaunch::Shell),
            name: None,
            cwd: None,
            env: HashMap::new(),
            restart: RestartPolicy::default(),
            shell: None,
            scrollback: None,
            cursor_pos: None,
        })
        .collect();

    let tab = TabSnapshot {
        name: "1".to_string(),
        layout,
        active_pane,
        zoomed_pane: None,
        broadcast: false,
        panes,
    };

    Ok(WorkspaceSnapshot {
        version: SNAPSHOT_VERSION,
        shell,
        border_style,
        show_status_bar,
        show_tab_bar: true,
        scrollback: 10_000,
        active_tab: 0,
        tabs: vec![tab],
    })
}

/// Decode the JSON document at `path` into a `WorkspaceSnapshot`,
/// migrating legacy schemas (v1, v2) into the current shape on the way
/// in. The returned snapshot always carries `version = SNAPSHOT_VERSION`
/// — `version_on_disk` lives in [`load_snapshot_with_meta`] for callers
/// that need it (e.g. the upgrade subcommand).
pub fn load_snapshot(path: impl AsRef<Path>) -> anyhow::Result<WorkspaceSnapshot> {
    let (snapshot, _on_disk) = load_snapshot_with_meta(path)?;
    Ok(snapshot)
}

/// Same as [`load_snapshot`] but also returns the version that was
/// found on disk *before* migration. Used by `ezpn upgrade-snapshot` to
/// report whether it actually changed anything (the idempotency AC).
pub fn load_snapshot_with_meta(path: impl AsRef<Path>) -> anyhow::Result<(WorkspaceSnapshot, u32)> {
    validate_path(path.as_ref())?;
    let content = std::fs::read_to_string(path)?;
    let (snapshot, on_disk) = parse_snapshot_str(&content)?;
    snapshot.validate()?;
    Ok((snapshot, on_disk))
}

/// Pure JSON-to-snapshot decoder, factored out so the CLI and IPC paths
/// share one migration ladder. Returns `(migrated_snapshot, on_disk_version)`.
fn parse_snapshot_str(content: &str) -> anyhow::Result<(WorkspaceSnapshot, u32)> {
    let raw: serde_json::Value = serde_json::from_str(content)?;
    let version = raw["version"].as_u64().unwrap_or(0) as u32;
    if !is_supported_version(version) {
        anyhow::bail!(
            "unsupported snapshot version: {found} (current = {current}, \
             minimum supported = {min}). Run `ezpn upgrade-snapshot <path>` \
             on older files.",
            found = version,
            current = SNAPSHOT_VERSION,
            min = MIN_SUPPORTED_VERSION
        );
    }
    let snapshot = match version {
        1 => migrate_v1(&raw)?,
        2 => migrate_v2(raw)?,
        // v3 (current) — straight deserialize, no migration step.
        _ => serde_json::from_value::<WorkspaceSnapshot>(raw)?,
    };
    Ok((snapshot, version))
}

/// Lift a v2 JSON document into the v3 schema. v3 is an additive bump
/// (`scrollback` + `cursor_pos` fields default to `None`), so the
/// payload deserializes verbatim — we only touch the version stamp.
fn migrate_v2(mut raw: serde_json::Value) -> anyhow::Result<WorkspaceSnapshot> {
    if let serde_json::Value::Object(map) = &mut raw {
        map.insert(
            "version".into(),
            serde_json::Value::Number(SNAPSHOT_VERSION.into()),
        );
    }
    let snap: WorkspaceSnapshot = serde_json::from_value(raw)?;
    Ok(snap)
}

pub fn save_snapshot(path: impl AsRef<Path>, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    validate_path(path.as_ref())?;
    save_snapshot_raw(path, snapshot)
}

/// Save without `validate_path`. Used by auto-save where the path is managed
/// by ezpn itself (e.g. `~/.local/share/ezpn/sessions/`).
fn save_snapshot_raw(path: impl AsRef<Path>, snapshot: &WorkspaceSnapshot) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(snapshot)?;

    // Atomic write: write to temp file, then rename
    let path = path.as_ref();
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, &json)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Clean up temp file on rename failure
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Auto-save directory for session snapshots.
pub fn auto_save_dir() -> Option<std::path::PathBuf> {
    let dir = if let Ok(data_dir) = std::env::var("XDG_DATA_HOME") {
        std::path::PathBuf::from(data_dir)
            .join("ezpn")
            .join("sessions")
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ezpn")
            .join("sessions")
    } else {
        return None;
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Auto-save a snapshot for the given session name.
/// Uses `save_snapshot_raw` to bypass `validate_path` since the auto-save
/// directory is managed by ezpn itself (e.g. `~/.local/share/ezpn/sessions/`).
pub fn auto_save(session_name: &str, snapshot: &WorkspaceSnapshot) {
    if let Some(dir) = auto_save_dir() {
        let path = dir.join(format!("{}.json", session_name));
        if let Err(e) = save_snapshot_raw(&path, snapshot) {
            eprintln!("ezpn: auto-save failed: {e}");
        }
    }
}

/// Load an auto-saved snapshot for the given session name.
#[allow(dead_code)] // Public API for future session resume feature
pub fn auto_load(session_name: &str) -> Option<WorkspaceSnapshot> {
    let dir = auto_save_dir()?;
    let path = dir.join(format!("{}.json", session_name));
    if !path.exists() {
        return None;
    }
    // For auto-load, we skip validate_path since it's our own managed directory
    let content = std::fs::read_to_string(&path).ok()?;
    let (snapshot, _on_disk) = parse_snapshot_str(&content).ok()?;
    snapshot.validate().ok()?;
    Some(snapshot)
}

/// Reject paths that could be dangerous when invoked via IPC.
fn validate_path(path: &Path) -> anyhow::Result<()> {
    let s = path.to_string_lossy();
    if s.contains("..") {
        anyhow::bail!("path traversal (..) not allowed: {}", s);
    }

    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name = name.to_string_lossy();
            if name.starts_with('.') && !name.contains("ezpn") {
                anyhow::bail!("refusing to use hidden path: {}", s);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Layout;

    fn make_v2_snapshot() -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".to_string(),
            border_style: BorderStyle::Double,
            show_status_bar: false,
            show_tab_bar: true,
            scrollback: 10_000,
            active_tab: 0,
            tabs: vec![TabSnapshot {
                name: "1".to_string(),
                layout: Layout::from_grid(1, 2),
                active_pane: 1,
                zoomed_pane: None,
                broadcast: false,
                panes: vec![
                    PaneSnapshot {
                        id: 0,
                        launch: PaneLaunch::Shell,
                        name: None,
                        cwd: None,
                        env: HashMap::new(),
                        restart: RestartPolicy::Never,
                        shell: None,
                        scrollback: None,
                        cursor_pos: None,
                    },
                    PaneSnapshot {
                        id: 1,
                        launch: PaneLaunch::Command("cargo test".to_string()),
                        name: Some("tests".to_string()),
                        cwd: Some("/tmp".to_string()),
                        env: HashMap::new(),
                        restart: RestartPolicy::OnFailure,
                        shell: None,
                        scrollback: None,
                        cursor_pos: None,
                    },
                ],
            }],
        }
    }

    #[test]
    fn snapshot_validation_rejects_mismatched_panes() {
        let mut snapshot = make_v2_snapshot();
        snapshot.tabs[0].panes.pop(); // Remove one pane
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn snapshot_round_trips_json() {
        let snapshot = make_v2_snapshot();
        let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
        let decoded =
            serde_json::from_str::<WorkspaceSnapshot>(&json).expect("deserialize snapshot");

        assert_eq!(decoded.tabs.len(), 1);
        assert_eq!(decoded.tabs[0].active_pane, 1);
        assert_eq!(decoded.tabs[0].panes.len(), 2);
        assert_eq!(
            decoded.tabs[0].panes[1].launch,
            PaneLaunch::Command("cargo test".to_string())
        );
        assert_eq!(decoded.tabs[0].panes[1].name, Some("tests".to_string()));
        assert_eq!(decoded.tabs[0].panes[1].restart, RestartPolicy::OnFailure);
    }

    #[test]
    fn validate_path_rejects_relative_dotfiles() {
        assert!(validate_path(Path::new(".bashrc")).is_err());
        assert!(validate_path(Path::new(".ssh/config")).is_err());
        assert!(validate_path(Path::new(".ezpn-session.json")).is_ok());
        assert!(validate_path(Path::new("sessions/.ezpn/dev.json")).is_ok());
    }

    #[test]
    fn v1_migration_produces_valid_v2() {
        // Use a real Layout to get correct serialization format
        let layout = Layout::from_grid(1, 1);
        let layout_json = serde_json::to_value(&layout).expect("serialize layout");
        let v1_json = serde_json::json!({
            "version": 1,
            "shell": "/bin/bash",
            "active_pane": 0,
            "border_style": "rounded",
            "show_status_bar": true,
            "layout": layout_json,
            "panes": [{ "id": 0, "launch": "shell" }]
        });
        let snapshot = migrate_v1(&v1_json).expect("migration");
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].active_pane, 0);
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn validate_rejects_out_of_range_active_tab() {
        let mut snapshot = make_v2_snapshot();
        snapshot.active_tab = 99;
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn multi_tab_round_trip() {
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".to_string(),
            border_style: BorderStyle::Rounded,
            show_status_bar: true,
            show_tab_bar: false,
            scrollback: 5000,
            active_tab: 1,
            tabs: vec![
                TabSnapshot {
                    name: "editor".to_string(),
                    layout: Layout::from_grid(1, 1),
                    active_pane: 0,
                    zoomed_pane: None,
                    broadcast: false,
                    panes: vec![PaneSnapshot {
                        id: 0,
                        launch: PaneLaunch::Command("nvim .".to_string()),
                        name: Some("nvim".to_string()),
                        cwd: Some("/home/user/project".to_string()),
                        env: HashMap::new(),
                        restart: RestartPolicy::Never,
                        shell: None,
                        scrollback: None,
                        cursor_pos: None,
                    }],
                },
                TabSnapshot {
                    name: "server".to_string(),
                    layout: Layout::from_grid(1, 2),
                    active_pane: 1,
                    zoomed_pane: Some(1),
                    broadcast: true,
                    panes: vec![
                        PaneSnapshot {
                            id: 0,
                            launch: PaneLaunch::Command("npm run dev".to_string()),
                            name: Some("dev".to_string()),
                            cwd: Some("/tmp".to_string()),
                            env: [("PORT".to_string(), "3000".to_string())].into(),
                            restart: RestartPolicy::OnFailure,
                            shell: Some("/bin/bash".to_string()),
                            scrollback: None,
                            cursor_pos: None,
                        },
                        PaneSnapshot {
                            id: 1,
                            launch: PaneLaunch::Shell,
                            name: None,
                            cwd: None,
                            env: HashMap::new(),
                            restart: RestartPolicy::Never,
                            shell: None,
                            scrollback: None,
                            cursor_pos: None,
                        },
                    ],
                },
            ],
        };

        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        decoded.validate().unwrap();

        assert_eq!(decoded.active_tab, 1);
        assert_eq!(decoded.scrollback, 5000);
        assert!(!decoded.show_tab_bar);
        assert_eq!(decoded.tabs.len(), 2);

        // Tab 0
        assert_eq!(decoded.tabs[0].name, "editor");
        assert_eq!(decoded.tabs[0].panes[0].name, Some("nvim".to_string()));
        assert_eq!(
            decoded.tabs[0].panes[0].cwd,
            Some("/home/user/project".to_string())
        );

        // Tab 1
        assert_eq!(decoded.tabs[1].name, "server");
        assert_eq!(decoded.tabs[1].zoomed_pane, Some(1));
        assert!(decoded.tabs[1].broadcast);
        assert_eq!(decoded.tabs[1].panes[0].restart, RestartPolicy::OnFailure);
        assert_eq!(
            decoded.tabs[1].panes[0].shell,
            Some("/bin/bash".to_string())
        );
        assert_eq!(
            decoded.tabs[1].panes[0].env.get("PORT"),
            Some(&"3000".to_string())
        );
    }

    #[test]
    fn pane_metadata_defaults_on_missing_fields() {
        // Simulate a v2 snapshot with minimal pane fields (serde defaults kick in)
        let json = serde_json::json!({
            "version": 2,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": serde_json::to_value(Layout::from_grid(1, 1)).unwrap(),
                "active_pane": 0,
                "panes": [{
                    "id": 0,
                    "launch": "shell"
                }]
            }]
        });
        let snapshot: WorkspaceSnapshot = serde_json::from_value(json).unwrap();
        snapshot.validate().unwrap();

        let pane = &snapshot.tabs[0].panes[0];
        assert_eq!(pane.name, None);
        assert_eq!(pane.cwd, None);
        assert!(pane.env.is_empty());
        assert_eq!(pane.restart, RestartPolicy::Never);
        assert_eq!(pane.shell, None);
        assert_eq!(snapshot.scrollback, 10_000); // default_scrollback()
        assert!(snapshot.show_tab_bar); // default_true()
    }

    // ─── v3 schema (#69, #70) ──────────────────────────────────

    #[test]
    fn supported_version_window_includes_v1_v2_v3() {
        // Whatever the constants land on, v1..=v3 must be readable from
        // v0.13. Anything outside that window is the explicit hard error
        // path documented in #70.
        assert!(is_supported_version(1));
        assert!(is_supported_version(2));
        assert!(is_supported_version(3));
        assert!(!is_supported_version(0));
        assert!(!is_supported_version(SNAPSHOT_VERSION + 1));
    }

    #[test]
    fn scrollback_blob_round_trips_text_lossless() {
        // 1k mixed lines, including blanks and a wide-character stretch,
        // round-trip through bincode + gzip exactly.
        let rows: Vec<RowSnapshot> = (0..1000)
            .map(|i| RowSnapshot {
                text: if i % 7 == 0 {
                    String::new()
                } else if i % 11 == 0 {
                    "한글 wide テスト".repeat(4)
                } else {
                    format!("line {i:04}: the quick brown fox")
                },
                attrs: if i % 5 == 0 {
                    vec![1, 2, 3, 4]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let blob = ScrollbackBlob::encode_bincode_gz(&rows).expect("encode");
        assert_eq!(blob.encoding, ScrollbackEncoding::BincodeGz);
        assert_eq!(blob.rows, 1000);
        // Compression: text-heavy content should hit ≥4× per #69 AC.
        let compressed = blob.payload.len() as f64;
        let raw = blob.bytes_uncompressed as f64;
        assert!(
            raw / compressed >= 4.0,
            "expected ≥4× compression on shell-history-like text, got {ratio:.2}× ({compressed} / {raw})",
            ratio = raw / compressed,
        );
        let decoded = blob.decode().expect("decode").expect("known encoding");
        assert_eq!(decoded, rows);
    }

    #[test]
    fn v3_snapshot_round_trips_with_scrollback() {
        let layout = Layout::from_grid(1, 1);
        let blob = ScrollbackBlob::encode_bincode_gz(&[
            RowSnapshot {
                text: "$ make".into(),
                attrs: Vec::new(),
            },
            RowSnapshot {
                text: "ok".into(),
                attrs: Vec::new(),
            },
        ])
        .unwrap();
        let snapshot = WorkspaceSnapshot {
            version: SNAPSHOT_VERSION,
            shell: "/bin/zsh".into(),
            border_style: BorderStyle::Rounded,
            show_status_bar: true,
            show_tab_bar: true,
            scrollback: 10_000,
            active_tab: 0,
            tabs: vec![TabSnapshot {
                name: "1".into(),
                layout,
                active_pane: 0,
                zoomed_pane: None,
                broadcast: false,
                panes: vec![PaneSnapshot {
                    id: 0,
                    launch: PaneLaunch::Shell,
                    name: None,
                    cwd: None,
                    env: HashMap::new(),
                    restart: RestartPolicy::Never,
                    shell: None,
                    scrollback: Some(blob.clone()),
                    cursor_pos: Some((1, 2)),
                }],
            }],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: WorkspaceSnapshot = serde_json::from_str(&json).unwrap();
        decoded.validate().unwrap();
        let pane = &decoded.tabs[0].panes[0];
        let restored = pane.scrollback.as_ref().unwrap().decode().unwrap().unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].text, "$ make");
        assert_eq!(pane.cursor_pos, Some((1, 2)));
    }

    #[test]
    fn v3_writer_omits_scrollback_field_when_none() {
        // Backward-compat: a v3 doc with no scrollback must look exactly
        // like a v2 doc (modulo the `version` stamp). A v2 reader
        // shouldn't find an unexpected `scrollback: null` key.
        let snapshot = make_v2_snapshot();
        let json = serde_json::to_value(&snapshot).unwrap();
        let pane0 = &json["tabs"][0]["panes"][0];
        assert!(
            pane0.get("scrollback").is_none(),
            "scrollback must be skipped when None (got {pane0:?})"
        );
        assert!(pane0.get("cursor_pos").is_none());
    }

    #[test]
    fn parse_snapshot_str_migrates_v2_to_v3() {
        let layout = Layout::from_grid(1, 1);
        let layout_json = serde_json::to_value(&layout).unwrap();
        let v2_doc = serde_json::json!({
            "version": 2,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "show_tab_bar": true,
            "scrollback": 10000,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": layout_json,
                "active_pane": 0,
                "panes": [{ "id": 0, "launch": "shell" }]
            }]
        });
        let s = serde_json::to_string(&v2_doc).unwrap();
        let (snap, on_disk) = parse_snapshot_str(&s).unwrap();
        assert_eq!(on_disk, 2);
        assert_eq!(snap.version, SNAPSHOT_VERSION);
        // v3 fields default to None.
        assert!(snap.tabs[0].panes[0].scrollback.is_none());
        assert!(snap.tabs[0].panes[0].cursor_pos.is_none());
    }

    #[test]
    fn parse_snapshot_str_migration_is_idempotent() {
        // Migrating v2 → v3 then re-encoding should produce a doc that
        // parses without further migration. (#70 idempotency AC.)
        let layout = Layout::from_grid(1, 1);
        let layout_json = serde_json::to_value(&layout).unwrap();
        let v2_doc = serde_json::json!({
            "version": 2,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "show_tab_bar": true,
            "scrollback": 10000,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": layout_json,
                "active_pane": 0,
                "panes": [{ "id": 0, "launch": "shell" }]
            }]
        });
        let v2_str = serde_json::to_string(&v2_doc).unwrap();
        let (s1, on_disk1) = parse_snapshot_str(&v2_str).unwrap();
        let v3_str = serde_json::to_string(&s1).unwrap();
        let (s2, on_disk2) = parse_snapshot_str(&v3_str).unwrap();
        assert_eq!(on_disk1, 2);
        assert_eq!(on_disk2, SNAPSHOT_VERSION);
        assert_eq!(s1.version, s2.version);
        assert_eq!(s1.tabs.len(), s2.tabs.len());
    }

    #[test]
    fn parse_snapshot_str_rejects_unknown_version_with_pointer_to_upgrade_cli() {
        let layout = Layout::from_grid(1, 1);
        let layout_json = serde_json::to_value(&layout).unwrap();
        let bad_doc = serde_json::json!({
            "version": 9999,
            "shell": "/bin/sh",
            "border_style": "single",
            "show_status_bar": true,
            "show_tab_bar": true,
            "scrollback": 10000,
            "active_tab": 0,
            "tabs": [{
                "name": "1",
                "layout": layout_json,
                "active_pane": 0,
                "panes": [{ "id": 0, "launch": "shell" }]
            }]
        });
        let s = serde_json::to_string(&bad_doc).unwrap();
        let err = parse_snapshot_str(&s).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("9999"),
            "error must mention bad version: {msg}"
        );
        assert!(
            msg.contains("upgrade-snapshot"),
            "error must point at the CLI: {msg}"
        );
    }
}
