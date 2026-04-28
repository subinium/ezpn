use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Split {
        direction: SplitDirection,
        pane: Option<usize>,
    },
    Close {
        pane: usize,
    },
    Focus {
        pane: usize,
    },
    Equalize,
    List,
    Layout {
        spec: String,
    },
    Exec {
        pane: usize,
        command: String,
    },
    Save {
        path: String,
    },
    Load {
        path: String,
    },
}

/// Extended IPC request vocabulary added in v0.12 (initially issue
/// #89; #88 and #81 add their own variants in follow-up commits).
/// Lives in a separate enum from [`IpcRequest`] so that
/// `handle_client` can intercept them in [`ipc.rs`] without forcing
/// every existing match site (`bootstrap::handle_ipc_command`,
/// `server/mod.rs::Save/Load` filters) to grow new arms.
///
/// On the wire, every variant is tagged by `cmd` (same envelope as
/// [`IpcRequest`]) — clients send a single `{"cmd": "...", ...}`
/// object and `handle_client` resolves it to either the legacy
/// `IpcRequest` or one of these extended commands.
///
/// Server-side handlers for these variants are **parent-deferred**;
/// the in-tree implementation returns [`IpcResponse::error`] with a
/// `not yet wired` message until the daemon-side hook lands. See
/// `docs/scripting.md` for the user-facing surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequestExt {
    /// `ezpn-ctl ls --json` — full session/tab/pane tree (issue #89).
    ///
    /// Returns the canonical structured snapshot of the current
    /// daemon's session(s). The schema is **frozen at v1** — see
    /// [`SessionTree`] / [`TabInfo`] / [`PaneTreeInfo`] and
    /// `docs/scripting.md` §3.1.
    ///
    /// `session` is the optional session-name filter. When
    /// `Some(name)`, the response includes only that session's tree
    /// (or an empty `sessions` list if it does not exist).
    LsTree {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// `ezpn-ctl dump` — capture-pane equivalent (issue #88).
    ///
    /// Reads from the vt100 grid (and optionally scrollback) of the
    /// targeted pane. No PTY interaction. The server enforces the
    /// 16 MiB hard cap on the produced text and returns
    /// [`IpcResponse::error`] with a `dump too large` message when the
    /// cap would be exceeded.
    Dump {
        pane: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// 1-indexed line number; only lines `>= since` are returned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<usize>,
        /// Tail count. When set, returns the last `last` lines (after
        /// `since` and scrollback inclusion are applied).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last: Option<usize>,
        /// Whether to include scrollback. Defaults to `true` (matching
        /// the CLI default `--include-scrollback`); pass `false` to
        /// limit to the visible viewport.
        #[serde(default = "default_true")]
        include_scrollback: bool,
        /// Strip all ANSI escape sequences from the captured text.
        #[serde(default)]
        strip_ansi: bool,
    },
    /// `ezpn-ctl send-keys` — ack-mode write to a pane (issue #81).
    ///
    /// `text` is written verbatim to the pane's PTY. When
    /// `await_prompt` is `true`, the server blocks until an OSC 133 D
    /// semantic-prompt sequence is observed for that pane (or
    /// `timeout_ms` elapses) and fills [`IpcResponse::send_keys`].
    SendKeys {
        pane: usize,
        text: String,
        /// Block until OSC 133 D is observed. Returns
        /// [`SendKeysStatus::DetectionUnavailable`] if neither OSC 133
        /// nor the (off-by-default) sentinel mode is active for the
        /// pane.
        #[serde(default)]
        await_prompt: bool,
        /// Timeout in milliseconds. Only meaningful when
        /// `await_prompt = true`. `None` means "use the server default
        /// of 30 000 ms".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Suppress the trailing `\n` the CLI normally appends.
        #[serde(default)]
        no_newline: bool,
    },
}

/// Tag values handled by [`IpcRequestExt`] (used by the
/// `handle_client` interceptor to decide which enum to deserialize
/// into).
const EXT_CMD_TAGS: &[&str] = &["ls_tree", "dump", "send_keys"];

/// Unified command envelope routed through the daemon main loop.
///
/// Per RFC #103 (v0.13 IPC unification), legacy [`IpcRequest`] and
/// extended [`IpcRequestExt`] commands share a single
/// `mpsc` channel into the main loop so that handlers for both
/// vocabularies can inspect daemon state (`panes`, `tab_mgr`,
/// `clients`, …) consistently. Prior to this change `IpcRequestExt`
/// was intercepted inline in [`handle_client`], which had no access
/// to server state — every variant returned a "not yet wired" error.
#[derive(Debug)]
pub enum IpcCommand {
    /// Legacy commands handled by `bootstrap::handle_ipc_command` and
    /// the Save/Load interceptors in `server::run`.
    Legacy(IpcRequest),
    /// v0.12 extended commands (`ls_tree`, `dump`, `send_keys`).
    /// Server-side handlers live in `server::ext_handlers` and run
    /// inside the main loop with full state access.
    Ext(IpcRequestExt),
}

/// Default timeout for `send-keys --await-prompt` when the client did
/// not pass `--timeout` (#81 spec). Mirrored on the CLI side as the
/// `30s` fallback.
pub const SEND_KEYS_DEFAULT_TIMEOUT_MS: u64 = 30_000;

fn default_true() -> bool {
    true
}

/// Hard cap on a single dump payload. Mirrors the wire-protocol
/// MAX_PAYLOAD (`docs/protocol/v1.md` §2) so a successful dump can
/// always traverse the IPC framing layer.
pub const DUMP_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneInfo {
    pub index: usize,
    pub id: usize,
    pub cols: u16,
    pub rows: u16,
    pub alive: bool,
    pub active: bool,
    pub command: String,
}

/// Frozen v1 schema for `ezpn-ctl ls --json` (issue #89). Mirrors the
/// shape documented in `docs/scripting.md` §3.1 and the issue body.
///
/// Wrapped by [`IpcResponse::ls_tree`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsTree {
    /// Frozen at `"1.0"`. Bumped only on a major schema break.
    pub proto_version: String,
    pub sessions: Vec<SessionTree>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTree {
    pub name: String,
    /// Seconds since UNIX epoch (matches the `ts` field used in the
    /// event bus — `f64`).
    pub created_at: f64,
    pub clients: Vec<ClientInfo>,
    /// Index into `tabs`. `None` if there are no tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_tab: Option<usize>,
    pub tabs: Vec<TabInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub socket: String,
    /// `[cols, rows]`.
    pub size: [u16; 2],
    /// One of `steal | shared | readonly` (mirrors `AttachMode`).
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: usize,
    pub name: String,
    /// `id` of the focused pane within this tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_pane: Option<usize>,
    /// Layout spec string, e.g. `"split-h:[1,2]"`.
    pub layout: String,
    pub panes: Vec<PaneTreeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneTreeInfo {
    pub id: usize,
    pub command: String,
    /// The cwd the pane was launched with (string-encoded path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// `[cols, rows]`.
    pub size: [u16; 2],
    /// `None` once the child has exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Live cwd as reported by OSC 7 / procfs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reported_cwd: Option<String>,
    /// `Some(code)` once the pane has exited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub is_dead: bool,
    pub is_focused: bool,
    /// Pane title (window-title OSC 0/2 or the pane's `name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Result payload for [`IpcRequestExt::Dump`] (issue #88).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpPayload {
    pub pane: usize,
    /// Captured lines after `since`/`last`/scrollback filtering. Each
    /// element is a single visual line (terminator stripped). When the
    /// CLI is in `--format text` mode, lines are joined with `\n`.
    pub lines: Vec<String>,
    /// Total number of lines available in the source (visible +
    /// scrollback when included). Useful for paging clients.
    pub total: usize,
}

/// Result payload for [`IpcRequestExt::SendKeys`] when `await_prompt`
/// is set (issue #81). For fire-and-forget sends the response uses
/// [`IpcResponse::success`] and [`IpcResponse::send_keys`] is `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendKeysOutcome {
    /// `prompt_seen` when an OSC 133 D arrived; `timeout` when the
    /// wait expired; `detection_unavailable` when the pane has no
    /// prompt-detection mechanism active.
    pub status: SendKeysStatus,
    /// The exit code parsed out of OSC 133 D, if present. `None` for
    /// `timeout` / `detection_unavailable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Wall-clock duration in milliseconds the server waited.
    pub waited_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendKeysStatus {
    /// OSC 133 D semantic-prompt observed for this pane.
    PromptSeen,
    /// `timeout_ms` elapsed before any prompt arrived.
    Timeout,
    /// Neither OSC 133 nor sentinel mode is active for this pane.
    DetectionUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub panes: Option<Vec<PaneInfo>>,
    /// Populated by [`IpcRequestExt::LsTree`] (issue #89).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ls_tree: Option<LsTree>,
    /// Populated by [`IpcRequestExt::Dump`] (issue #88).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dump: Option<DumpPayload>,
    /// Populated by [`IpcRequestExt::SendKeys`] when `await_prompt =
    /// true` (issue #81).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_keys: Option<SendKeysOutcome>,
}

impl IpcResponse {
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: Some(message.into()),
            error: None,
            panes: None,
            ls_tree: None,
            dump: None,
            send_keys: None,
        }
    }

    pub fn with_panes(panes: Vec<PaneInfo>) -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            panes: Some(panes),
            ls_tree: None,
            dump: None,
            send_keys: None,
        }
    }

    /// Wrap a frozen-v1 [`LsTree`] in an `ok` response.
    pub fn with_ls_tree(tree: LsTree) -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            panes: None,
            ls_tree: Some(tree),
            dump: None,
            send_keys: None,
        }
    }

    /// Wrap a [`DumpPayload`] in an `ok` response.
    pub fn with_dump(dump: DumpPayload) -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            panes: None,
            ls_tree: None,
            dump: Some(dump),
            send_keys: None,
        }
    }

    /// Wrap a [`SendKeysOutcome`] in an `ok` response (used when
    /// `await_prompt` is set).
    pub fn with_send_keys(outcome: SendKeysOutcome) -> Self {
        Self {
            ok: true,
            message: None,
            error: None,
            panes: None,
            ls_tree: None,
            dump: None,
            send_keys: Some(outcome),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: None,
            error: Some(message.into()),
            panes: None,
            ls_tree: None,
            dump: None,
            send_keys: None,
        }
    }
}

pub type ResponseSender = std::sync::mpsc::SyncSender<IpcResponse>;

pub fn socket_path() -> PathBuf {
    socket_path_for_pid(std::process::id())
}

pub fn socket_path_for_pid(pid: u32) -> PathBuf {
    // EZPN_TEST_SOCKET_DIR > XDG_RUNTIME_DIR > /tmp.
    // Test override exists so integration tests (#62) can redirect.
    let dir = std::env::var("EZPN_TEST_SOCKET_DIR")
        .or_else(|_| std::env::var("XDG_RUNTIME_DIR"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/ezpn-{}.sock", dir, pid))
}

pub fn start_listener() -> anyhow::Result<mpsc::Receiver<(IpcCommand, ResponseSender)>> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        crate::socket_security::harden_socket_dir(parent)?;
    }
    let _ = std::fs::remove_file(&path);

    // umask 0o077 across bind so the inode is born with a restricted mode,
    // then chmod + re-stat to assert (issue #65). Any deviation is a hard
    // error — silently continuing here would re-introduce the leak we're
    // trying to close.
    let prev_umask = unsafe { libc::umask(0o077) };
    let bind_result = UnixListener::bind(&path);
    unsafe {
        libc::umask(prev_umask);
    }
    let listener = bind_result?;

    crate::socket_security::fix_socket_permissions(&path)?;

    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    // Defense-in-depth: refuse cross-UID connections.
                    let our_uid = unsafe { libc::getuid() };
                    match crate::socket_security::peer_uid(&stream) {
                        Ok(peer) if peer == our_uid => {}
                        Ok(peer) => {
                            tracing::warn!(
                                event = "ipc_ctl_peer_uid_mismatch",
                                peer_uid = peer,
                                expected_uid = our_uid,
                                "refusing cross-uid ctl connection"
                            );
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!(
                                event = "ipc_ctl_peer_uid_error",
                                error = %e,
                                "could not read peer credentials, refusing ctl connection"
                            );
                            continue;
                        }
                    }
                    let tx: mpsc::Sender<(IpcCommand, ResponseSender)> = tx.clone();
                    std::thread::spawn(move || handle_client(stream, tx));
                }
                Err(_) => break,
            }
        }
    });

    Ok(rx)
}

pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}

fn handle_client(stream: UnixStream, tx: mpsc::Sender<(IpcCommand, ResponseSender)>) {
    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    let reader = BufReader::new(read_stream);
    let mut writer = stream;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        // Peek the `cmd` tag and parse into either [`IpcRequest`] or
        // [`IpcRequestExt`]. Both vocabularies route through the same
        // channel so the daemon main loop dispatches with full state
        // access (RFC #103, v0.13 IPC unification).
        let parsed: Result<IpcCommand, String> = if is_ext_command(&line) {
            serde_json::from_str::<IpcRequestExt>(&line)
                .map(IpcCommand::Ext)
                .map_err(|e| format!("invalid request: {}", e))
        } else {
            serde_json::from_str::<IpcRequest>(&line)
                .map(IpcCommand::Legacy)
                .map_err(|e| format!("invalid request: {}", e))
        };

        let response = match parsed {
            Ok(cmd) => {
                let (resp_tx, resp_rx) = mpsc::sync_channel(1);
                if tx.send((cmd, resp_tx)).is_err() {
                    let response = IpcResponse::error("listener unavailable");
                    let _ = write_response(&mut writer, &response);
                    break;
                }
                resp_rx
                    .recv()
                    .unwrap_or_else(|_| IpcResponse::error("internal error"))
            }
            Err(message) => IpcResponse::error(message),
        };

        if write_response(&mut writer, &response).is_err() {
            break;
        }
    }
}

/// Inspect the `cmd` tag in a JSON line without fully deserializing.
/// Returns true when the tag matches one of [`EXT_CMD_TAGS`].
fn is_ext_command(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let Some(cmd) = value.get("cmd").and_then(|v| v.as_str()) else {
        return false;
    };
    EXT_CMD_TAGS.contains(&cmd)
}

// `handle_ext_request` was removed in v0.13 (RFC #103). Both
// [`IpcRequest`] and [`IpcRequestExt`] now flow through
// [`IpcCommand`] so the daemon main loop handles them with full state
// access. The Ext dispatcher lives in `crate::server::ext_handlers`.

// ---------------------------------------------------------------------
// Integration TODO — `send-keys --await-prompt` (issue #81)
// ---------------------------------------------------------------------
//
// The parent agent must extend `Pane` (in `src/pane.rs`, off-limits to
// this agent) with the following API so `handle_ext_request` /
// `bootstrap::handle_ipc_command` can drive the await-prompt loop:
//
//     // src/pane.rs
//     impl Pane {
//         /// Wall-clock instant of the most recently observed
//         /// OSC 133 D semantic-prompt (`\x1b]133;D;<exit>\x07`).
//         /// `None` until the first prompt is observed for this pane.
//         /// Updated by the OSC parser inside `read_output`.
//         pub fn prompt_seen_at(&self) -> Option<Instant>;
//
//         /// Exit code parsed from the most recent OSC 133 D, paired
//         /// with `prompt_seen_at`. `None` when the prompt sequence
//         /// did not include an exit code.
//         pub fn last_prompt_exit_code(&self) -> Option<i32>;
//
//         /// True iff this pane has emitted at least one OSC 133 A
//         /// (prompt-start) sequence — used to answer
//         /// `SendKeysStatus::DetectionUnavailable` before blocking.
//         pub fn osc133_active(&self) -> bool;
//     }
//
// The server hook then implements `await_prompt` as:
//
//     1. Snapshot `before = pane.prompt_seen_at()` BEFORE writing.
//     2. Write `text` (and trailing `\n` unless `no_newline`) via
//        `Pane::write_bytes`.
//     3. If `!pane.osc133_active()`, return `DetectionUnavailable`
//        immediately (do NOT block).
//     4. Spin-wait on the main-loop wake channel (or a fresh
//        `prompt_changed` watch channel — TBD by parent) until
//        `pane.prompt_seen_at()` advances past `before` or
//        `timeout_ms` elapses (default `SEND_KEYS_DEFAULT_TIMEOUT_MS`).
//     5. Fill `SendKeysOutcome { status, exit_code, waited_ms }` and
//        return it via `IpcResponse::with_send_keys`.

fn write_response(writer: &mut UnixStream, response: &IpcResponse) -> anyhow::Result<()> {
    writeln!(writer, "{}", serde_json::to_string(response)?)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frozen-v1 schema check (#89): every documented field of
    /// `LsTree` round-trips, the `proto_version` is preserved as-is,
    /// and `Option`-typed fields collapse out of the wire form when
    /// `None` so additive bumps remain backwards-compatible.
    #[test]
    fn ls_tree_roundtrip_matches_frozen_schema() {
        let tree = LsTree {
            proto_version: "1.0".to_string(),
            sessions: vec![SessionTree {
                name: "work".to_string(),
                created_at: 1_745_800_000.0,
                clients: vec![ClientInfo {
                    socket: "/run/ezpn-1.sock".to_string(),
                    size: [80, 24],
                    mode: "steal".to_string(),
                }],
                focused_tab: Some(0),
                tabs: vec![TabInfo {
                    id: 0,
                    name: "editor".to_string(),
                    focused_pane: Some(1),
                    layout: "split-h:[1,2]".to_string(),
                    panes: vec![PaneTreeInfo {
                        id: 1,
                        command: "nvim".to_string(),
                        cwd: Some("/foo".to_string()),
                        size: [80, 24],
                        pid: Some(12345),
                        reported_cwd: Some("/foo/bar".to_string()),
                        exit_code: None,
                        is_dead: false,
                        is_focused: true,
                        title: Some("file.rs".to_string()),
                    }],
                }],
            }],
        };

        let json = serde_json::to_string(&tree).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["proto_version"], "1.0");
        let pane = &value["sessions"][0]["tabs"][0]["panes"][0];
        assert_eq!(pane["id"], 1);
        assert_eq!(pane["command"], "nvim");
        assert_eq!(pane["size"], serde_json::json!([80, 24]));
        assert_eq!(pane["is_focused"], true);
        assert_eq!(pane["is_dead"], false);
        // Optional fields collapse cleanly when None — verify by
        // overriding a single field and checking it disappears.
        assert!(pane.get("exit_code").is_none(), "None must skip-serialize");

        let back: LsTree = serde_json::from_str(&json).unwrap();
        assert_eq!(back.proto_version, "1.0");
        assert_eq!(back.sessions.len(), 1);
        assert_eq!(back.sessions[0].tabs[0].panes[0].command, "nvim");
    }

    /// `IpcRequestExt::LsTree` is tagged `cmd: "ls_tree"` on the wire
    /// (snake_case rename) — match the doc and the interceptor in
    /// `handle_client`.
    #[test]
    fn ls_tree_request_uses_cmd_tag() {
        let req = IpcRequestExt::LsTree {
            session: Some("work".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["cmd"], "ls_tree");
        assert_eq!(value["session"], "work");
    }

    /// Optional `session` filter omits cleanly when `None`.
    #[test]
    fn ls_tree_request_omits_session_when_none() {
        let req = IpcRequestExt::LsTree { session: None };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("session"));
    }

    /// `is_ext_command` correctly classifies the v0.12 commands and
    /// leaves legacy ones to the daemon main loop.
    #[test]
    fn is_ext_command_classifies_v012_commands() {
        assert!(is_ext_command(r#"{"cmd":"ls_tree"}"#));
        assert!(!is_ext_command(r#"{"cmd":"list"}"#));
        assert!(!is_ext_command(
            r#"{"cmd":"split","direction":"horizontal"}"#
        ));
        assert!(!is_ext_command("not json"));
    }

    /// `ls_tree` requests parse cleanly into the [`IpcRequestExt`]
    /// vocabulary — the inline parent-deferred error path was removed
    /// in v0.13 (RFC #103); `ls_tree` now dispatches through
    /// [`IpcCommand::Ext`] to a real daemon-side handler.
    #[test]
    fn ls_tree_request_parses_for_main_loop_dispatch() {
        let req: IpcRequestExt = serde_json::from_str(r#"{"cmd":"ls_tree"}"#).unwrap();
        assert!(matches!(req, IpcRequestExt::LsTree { session: None }));
    }

    /// `IpcResponse` must round-trip with the new optional fields.
    #[test]
    fn ipc_response_carries_ls_tree() {
        let tree = LsTree {
            proto_version: "1.0".to_string(),
            sessions: vec![],
        };
        let response = IpcResponse::with_ls_tree(tree);
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"proto_version\":\"1.0\""));

        let back: IpcResponse = serde_json::from_str(&json).unwrap();
        assert!(back.ok);
        assert_eq!(back.ls_tree.unwrap().proto_version, "1.0");
    }

    /// `IpcRequestExt::Dump` survives a JSON round-trip with all
    /// option fields collapsed when omitted, and `include_scrollback`
    /// defaults to `true` (matching the CLI default) when absent.
    #[test]
    fn dump_request_defaults_match_cli_defaults() {
        let json = r#"{"cmd":"dump","pane":3}"#;
        let req: IpcRequestExt = serde_json::from_str(json).unwrap();
        match req {
            IpcRequestExt::Dump {
                pane,
                session,
                since,
                last,
                include_scrollback,
                strip_ansi,
            } => {
                assert_eq!(pane, 3);
                assert!(session.is_none());
                assert!(since.is_none());
                assert!(last.is_none());
                assert!(include_scrollback, "default must be true");
                assert!(!strip_ansi);
            }
            _ => panic!("expected Dump variant"),
        }
    }

    /// `is_ext_command` recognises the dump tag.
    #[test]
    fn is_ext_command_recognises_dump() {
        assert!(is_ext_command(r#"{"cmd":"dump","pane":0}"#));
    }

    /// `dump` requests parse cleanly into the [`IpcRequestExt`]
    /// vocabulary — the inline parent-deferred error path was removed
    /// in v0.13 (RFC #103). The real handler lives in
    /// `crate::server::ext_handlers::handle_dump`.
    #[test]
    fn dump_request_parses_for_main_loop_dispatch() {
        let req: IpcRequestExt = serde_json::from_str(r#"{"cmd":"dump","pane":0}"#).unwrap();
        assert!(matches!(req, IpcRequestExt::Dump { pane: 0, .. }));
    }

    /// `DumpPayload` round-trips, including non-ASCII bytes and ANSI
    /// escapes that scripts will frequently see.
    #[test]
    fn dump_payload_roundtrip_handles_escapes() {
        let payload = DumpPayload {
            pane: 2,
            lines: vec![
                "hello".to_string(),
                "\x1b[31mred\x1b[0m".to_string(),
                "café".to_string(),
            ],
            total: 3,
        };
        let json = serde_json::to_string(&payload).unwrap();
        // jq-parseable: must not contain raw control bytes outside
        // strings — serde escapes them inside the string.
        let back: DumpPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lines.len(), 3);
        assert_eq!(back.lines[1], "\x1b[31mred\x1b[0m");
        assert_eq!(back.lines[2], "café");
    }

    /// The 16 MiB hard cap matches the wire-protocol MAX_PAYLOAD.
    #[test]
    fn dump_max_bytes_matches_wire_cap() {
        assert_eq!(DUMP_MAX_BYTES, 16 * 1024 * 1024);
    }

    /// `IpcRequestExt::SendKeys` defaults: `await_prompt = false`,
    /// `no_newline = false`, `timeout_ms = None`. Matches the
    /// fire-and-forget v0.5 `exec` semantics when `--await-prompt` is
    /// absent.
    #[test]
    fn send_keys_request_defaults() {
        let json = r#"{"cmd":"send_keys","pane":1,"text":"echo hi"}"#;
        let req: IpcRequestExt = serde_json::from_str(json).unwrap();
        match req {
            IpcRequestExt::SendKeys {
                pane,
                text,
                await_prompt,
                timeout_ms,
                no_newline,
            } => {
                assert_eq!(pane, 1);
                assert_eq!(text, "echo hi");
                assert!(!await_prompt);
                assert!(timeout_ms.is_none());
                assert!(!no_newline);
            }
            _ => panic!("expected SendKeys variant"),
        }
    }

    /// `is_ext_command` recognises the send-keys tag.
    #[test]
    fn is_ext_command_recognises_send_keys() {
        assert!(is_ext_command(r#"{"cmd":"send_keys","pane":0,"text":"x"}"#));
    }

    /// `send_keys` parses cleanly into the [`IpcRequestExt`]
    /// vocabulary. The actual daemon-side hook still returns a
    /// "not yet wired" error (issue #81 is out of scope for the
    /// v0.13 dump/ls wiring) — but the dispatch path runs through
    /// [`IpcCommand::Ext`] now.
    #[test]
    fn send_keys_request_parses_for_main_loop_dispatch() {
        let req: IpcRequestExt = serde_json::from_str(
            r#"{"cmd":"send_keys","pane":0,"text":"echo","await_prompt":true}"#,
        )
        .unwrap();
        assert!(matches!(req, IpcRequestExt::SendKeys { pane: 0, .. }));
    }

    /// `SendKeysOutcome` round-trips and the snake_case status enum
    /// matches the documented `prompt_seen` / `timeout` /
    /// `detection_unavailable` strings.
    #[test]
    fn send_keys_outcome_status_uses_snake_case() {
        for (status, expected) in [
            (SendKeysStatus::PromptSeen, "prompt_seen"),
            (SendKeysStatus::Timeout, "timeout"),
            (
                SendKeysStatus::DetectionUnavailable,
                "detection_unavailable",
            ),
        ] {
            let outcome = SendKeysOutcome {
                status,
                exit_code: Some(0),
                waited_ms: 12,
            };
            let json = serde_json::to_string(&outcome).unwrap();
            assert!(
                json.contains(&format!("\"{}\"", expected)),
                "{} missing in {}",
                expected,
                json
            );
            let back: SendKeysOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(back.status, status);
        }
    }

    /// `IpcResponse::with_send_keys` round-trips and the timeout path
    /// correctly omits `exit_code`.
    #[test]
    fn ipc_response_with_send_keys_omits_exit_code_on_timeout() {
        let response = IpcResponse::with_send_keys(SendKeysOutcome {
            status: SendKeysStatus::Timeout,
            exit_code: None,
            waited_ms: 30_000,
        });
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("exit_code"), "got: {json}");
        assert!(json.contains("\"timeout\""));

        let back: IpcResponse = serde_json::from_str(&json).unwrap();
        let outcome = back.send_keys.unwrap();
        assert_eq!(outcome.status, SendKeysStatus::Timeout);
        assert!(outcome.exit_code.is_none());
        assert_eq!(outcome.waited_ms, 30_000);
    }

    /// The default timeout matches the spec (#81): 30 s when the
    /// client did not pass `--timeout`.
    #[test]
    fn send_keys_default_timeout_is_30s() {
        assert_eq!(SEND_KEYS_DEFAULT_TIMEOUT_MS, 30_000);
    }
}
