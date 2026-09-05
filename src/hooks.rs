//! Declarative hook system (issue #83).
//!
//! Users register external commands to run on lifecycle events
//! (`after_pane_exit`, `on_cwd_change`, ...) via `~/.config/ezpn/config.toml`
//! or per-project `.ezpn.toml`. Hooks are best-effort, fire-and-forget, and
//! cannot abort the triggering action.
//!
//! # Security model
//!
//! - **`exec` is always an array of strings.** No shell-string form. The
//!   resolved argv is passed straight to `Command::new(&argv[0]).args(&argv[1..])`
//!   so the kernel `execve`'s the binary directly without an intermediate
//!   shell parse. Users who want shell expansion must opt in explicitly with
//!   `["sh", "-c", "..."]`.
//! - **Variable substitution is single-shot.** `${pane.cwd}`
//!   is replaced with the literal payload string inside the argv element it
//!   occurs in. The result is *never* re-parsed, re-tokenized, or expanded.
//!   So a payload value of `; rm -rf ~` becomes a single argv string
//!   `; rm -rf ~`, not three additional arguments.
//!   Explicit shell programs still parse their script argument: pass payloads
//!   as positional arguments, not interpolated shell source.
//! - **No env propagation beyond what the daemon already exports.** The
//!   spawned child inherits the daemon's env unchanged; hooks cannot inject
//!   new variables.
//! - **5 s wall-clock timeout.** On Unix, the child process group is killed.
//! - **Bounded execution.** Four workers and 64 queued invocations maximum.
//! - **Output captured.** stdout + stderr go to a per-event rotating log
//!   under `$XDG_STATE_HOME/ezpn/hooks/<event>-<unix>.log`, capped at 1 MB
//!   per file; older files are evicted FIFO.
//!
//! # Integration TODO (server.rs follow-up)
//!
//! The data model and executor live here; the actual *fire* sites live in
//! `server.rs` (off-limits to this PR). To wire it up in a follow-up:
//!
//! 1. Construct a single `HookExecutor` at server boot from
//!    `config::load_hooks()` + project hooks (`project::ResolvedProject::hooks`).
//! 2. Call `executor.fire(HookEvent::AfterAttach, &payload)` at each of the
//!    10 lifecycle points listed in [`HookEvent`].
//! 3. On config hot-reload, swap `executor.replace(new_hooks)`.
//! 4. The executor is `Send + Sync` and uses an internal thread pool; the
//!    server's main loop is not blocked.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// Frozen v1 hook event vocabulary. **Renames are a breaking change post-1.0;
/// additions are allowed.** Keep in sync with the table in `docs/hooks.md`
/// and the parser in [`HookEvent::from_str`].
///
/// All ten events are listed in the order they occur in a typical session
/// lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// Fires after the daemon parses config and before the first pane
    /// spawns. Payload: session.
    AfterSessionCreate,
    /// Fires before a client attaches a TTY. Payload: session, client.
    BeforeAttach,
    /// Fires after a client successfully attaches. Payload: session, client.
    AfterAttach,
    /// Fires before a client detaches. Payload: session, client.
    BeforeDetach,
    /// Fires after a client detaches (the daemon may still be running).
    /// Payload: session, client.
    AfterDetach,
    /// Fires after a new pane is spawned. Payload: session, pane.
    AfterPaneSpawn,
    /// Fires after a pane's child process exits. Payload: session, pane
    /// (with `exit_code`, `command`).
    AfterPaneExit,
    /// Fires when the active pane's tracked cwd changes. Payload: session,
    /// pane (with new `cwd`).
    OnCwdChange,
    /// Fires when the focused pane changes. Payload: session, pane (the
    /// newly-focused one), `previous_pane_id`.
    OnFocusChange,
    /// Fires after a successful config hot-reload. Payload: session,
    /// `config_path`.
    OnConfigReload,
    /// Fires before the daemon tears the session down (for any reason —
    /// last client gone, SIGTERM, etc.). Payload: session.
    BeforeSessionDestroy,
}

impl HookEvent {
    /// Stable wire/log name. Lower-case `snake_case`. **Frozen.**
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::BeforeAttach => "before_attach",
            HookEvent::AfterAttach => "after_attach",
            HookEvent::BeforeDetach => "before_detach",
            HookEvent::AfterDetach => "after_detach",
            HookEvent::AfterSessionCreate => "after_session_create",
            HookEvent::BeforeSessionDestroy => "before_session_destroy",
            HookEvent::AfterPaneSpawn => "after_pane_spawn",
            HookEvent::AfterPaneExit => "after_pane_exit",
            HookEvent::OnCwdChange => "on_cwd_change",
            HookEvent::OnFocusChange => "on_focus_change",
            HookEvent::OnConfigReload => "on_config_reload",
        }
    }

    /// Parse an event name from its frozen wire form. Returns `None` for
    /// unknown names (caller should produce a structured load-time error
    /// pointing at the offending TOML line).
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "before_attach" => Some(HookEvent::BeforeAttach),
            "after_attach" => Some(HookEvent::AfterAttach),
            "before_detach" => Some(HookEvent::BeforeDetach),
            "after_detach" => Some(HookEvent::AfterDetach),
            "after_session_create" => Some(HookEvent::AfterSessionCreate),
            "before_session_destroy" => Some(HookEvent::BeforeSessionDestroy),
            "after_pane_spawn" => Some(HookEvent::AfterPaneSpawn),
            "after_pane_exit" => Some(HookEvent::AfterPaneExit),
            "on_cwd_change" => Some(HookEvent::OnCwdChange),
            "on_focus_change" => Some(HookEvent::OnFocusChange),
            "on_config_reload" => Some(HookEvent::OnConfigReload),
            _ => None,
        }
    }

    /// Full list, useful for documentation and `--help` output.
    pub fn all() -> &'static [HookEvent] {
        &[
            HookEvent::AfterSessionCreate,
            HookEvent::BeforeAttach,
            HookEvent::AfterAttach,
            HookEvent::BeforeDetach,
            HookEvent::AfterDetach,
            HookEvent::AfterPaneSpawn,
            HookEvent::AfterPaneExit,
            HookEvent::OnCwdChange,
            HookEvent::OnFocusChange,
            HookEvent::OnConfigReload,
            HookEvent::BeforeSessionDestroy,
        ]
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A single declarative hook entry.
///
/// Constructed by [`Hook::from_raw`] which validates `event`, ensures `exec`
/// is non-empty, and pre-parses the optional `when` predicate.
#[derive(Debug, Clone)]
pub struct Hook {
    pub event: HookEvent,
    /// argv. Element [0] is the program; [1..] are arguments. **Each element
    /// may contain `${var.path}` placeholders** which are substituted at fire
    /// time. The substitution is single-shot — the placeholder text is
    /// replaced with the literal payload value, and the result is **not**
    /// re-tokenized.
    pub exec: Vec<String>,
    /// Optional predicate. If present, the hook only fires when the
    /// predicate evaluates true against the event payload.
    pub when: Option<WhenPredicate>,
}

/// TOML wire form. Public only so `config.rs` and `project.rs` can pull it
/// out of their schemas.
#[derive(Debug, Clone, Deserialize)]
pub struct RawHook {
    pub event: String,
    pub exec: Vec<String>,
    #[serde(default)]
    pub when: Option<String>,
}

/// Errors produced while validating a [`RawHook`] into a [`Hook`].
#[derive(Debug)]
pub enum HookParseError {
    UnknownEvent(String),
    EmptyExec,
    /// `exec = ["sh", "-c", "..."]` is allowed; `exec = "echo $x"` (string
    /// form) is not. We don't accept the string form at all in TOML, but if
    /// some upstream caller hands us an empty-string program, reject it.
    EmptyProgram,
    InvalidExec,
    InvalidWhen(String),
}

impl fmt::Display for HookParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HookParseError::UnknownEvent(name) => write!(
                f,
                "unknown hook event '{name}' (valid: {})",
                HookEvent::all()
                    .iter()
                    .map(|e| e.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            HookParseError::EmptyExec => f.write_str("hook exec must be a non-empty array"),
            HookParseError::EmptyProgram => {
                f.write_str("hook exec[0] (program name) must not be empty")
            }
            HookParseError::InvalidExec => f.write_str("hook exec contains NUL or exceeds 1 MiB"),
            HookParseError::InvalidWhen(msg) => write!(f, "invalid when predicate: {msg}"),
        }
    }
}

impl std::error::Error for HookParseError {}

impl Hook {
    /// Validate a raw TOML entry into a `Hook`. Performs every load-time
    /// check so fire-time is allocation-light and infallible (modulo the
    /// child spawn itself).
    pub fn from_raw(raw: RawHook) -> Result<Self, HookParseError> {
        let event = HookEvent::from_str(&raw.event)
            .ok_or(HookParseError::UnknownEvent(raw.event.clone()))?;
        if raw.exec.is_empty() {
            return Err(HookParseError::EmptyExec);
        }
        if raw.exec[0].trim().is_empty() {
            return Err(HookParseError::EmptyProgram);
        }
        if raw.exec.iter().any(|s| s.contains('\0'))
            || raw
                .exec
                .iter()
                .fold(0usize, |n, s| n.saturating_add(s.len()))
                > HOOK_ARGV_MAX_BYTES
        {
            return Err(HookParseError::InvalidExec);
        }
        let when = match raw.when {
            Some(s) if !s.trim().is_empty() => Some(WhenPredicate::parse(&s)?),
            _ => None,
        };
        Ok(Hook {
            event,
            exec: raw.exec,
            when,
        })
    }
}

/// Typed event payload. Represented as a flat key/value map keyed by dotted
/// paths (e.g. `pane.cwd`, `session.name`) so the substitution engine can
/// stay trivially simple. The server is responsible for filling the right
/// keys for each event — see [`HookEvent`] doc-comments for the schema.
#[derive(Debug, Default, Clone)]
pub struct HookPayload {
    fields: HashMap<String, String>,
}

impl HookPayload {
    pub fn new() -> Self {
        Self {
            fields: HashMap::new(),
        }
    }

    /// Insert a field. Convenience wrapper.
    pub fn set<K: Into<String>, V: ToString>(mut self, key: K, value: V) -> Self {
        self.fields.insert(key.into(), value.to_string());
        self
    }

    pub fn insert<K: Into<String>, V: ToString>(&mut self, key: K, value: V) {
        self.fields.insert(key.into(), value.to_string());
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

// ─── Variable substitution ─────────────────────────────────────────────

/// Apply payload substitution to a single argv element.
///
/// Replaces every `${dotted.path}` occurrence with the matching payload
/// field. Missing fields substitute to the empty string (consistent with
/// shell `${UNSET}` semantics) so a misconfigured hook degrades gracefully
/// instead of failing to fire. This is a deliberate trade-off — surfacing
/// missing keys would block fire-and-forget execution; the executor logs a
/// debug line for visibility.
///
/// **No re-parsing.** The output is exactly one string. Even if the
/// substituted value contains spaces, quotes, or shell metacharacters, the
/// caller hands the single string to `Command::args` which never invokes a
/// shell.
fn substitute_checked(
    input: &str,
    payload: &HookPayload,
    missing: &mut Vec<String>,
) -> Option<String> {
    if input.len() > HOOK_ARGV_MAX_BYTES {
        return None;
    }
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Match `${...}` (we accept any chars except `}` inside).
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = input[i + 2..].find('}') {
                let key = &input[i + 2..i + 2 + end];
                match payload.get(key) {
                    Some(v) => {
                        if v.len() > HOOK_ARGV_MAX_BYTES.saturating_sub(out.len()) {
                            return None;
                        }
                        out.push_str(v);
                    }
                    None => {
                        missing.push(key.to_string());
                        // Empty substitution; keep going.
                    }
                }
                i += 2 + end + 1; // past the closing '}'
                continue;
            }
        }
        out.push(input[i..].chars().next().unwrap());
        if out.len() > HOOK_ARGV_MAX_BYTES {
            return None;
        }
        i += input[i..].chars().next().unwrap().len_utf8();
    }
    Some(out)
}

#[cfg(test)]
fn substitute(input: &str, payload: &HookPayload, missing: &mut Vec<String>) -> String {
    substitute_checked(input, payload, missing).expect("test substitution within bounds")
}

// ─── `when` predicate ──────────────────────────────────────────────────

/// Minimal `when` predicate language. Supported forms:
///
/// - `${path} == "literal"`
/// - `${path} != "literal"`
/// - `${path} == NUMBER` (integer)
/// - `${path} != NUMBER`
///
/// More elaborate expressions (`&&`, `||`, numeric `<`/`>`) are out of scope
/// for v1; users who need them can wrap their hook in `["sh", "-c", "..."]`
/// and exit non-zero to no-op.
#[derive(Debug, Clone)]
pub struct WhenPredicate {
    lhs: String,
    op: WhenOp,
    rhs: WhenRhs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhenOp {
    Eq,
    Ne,
}

#[derive(Debug, Clone)]
enum WhenRhs {
    Str(String),
    Int(i64),
}

impl WhenPredicate {
    fn parse(src: &str) -> Result<Self, HookParseError> {
        let trimmed = src.trim();
        // Find an operator. Order matters: scan for `==` / `!=` first.
        let lhs_end = trimmed
            .find('}')
            .ok_or_else(|| HookParseError::InvalidWhen("missing variable reference".into()))?
            + 1;
        let after_lhs = trimmed[lhs_end..].trim_start();
        let op_pos = trimmed.len() - after_lhs.len();
        let (op_len, op) = if after_lhs.starts_with("==") {
            (2, WhenOp::Eq)
        } else if after_lhs.starts_with("!=") {
            (2, WhenOp::Ne)
        } else {
            return Err(HookParseError::InvalidWhen(format!(
                "expected `==` or `!=` operator in `{trimmed}`"
            )));
        };
        let lhs_raw = trimmed[..op_pos].trim();
        let rhs_raw = trimmed[op_pos + op_len..].trim();
        let lhs_path = parse_var_ref(lhs_raw).ok_or_else(|| {
            HookParseError::InvalidWhen(format!(
                "left-hand side must be a `${{path}}` reference, got `{lhs_raw}`"
            ))
        })?;
        let rhs = if let Some(s) = strip_quotes(rhs_raw) {
            WhenRhs::Str(s.to_string())
        } else if let Ok(n) = rhs_raw.parse::<i64>() {
            WhenRhs::Int(n)
        } else {
            return Err(HookParseError::InvalidWhen(format!(
                "right-hand side must be a quoted string or integer literal, got `{rhs_raw}`"
            )));
        };
        Ok(WhenPredicate {
            lhs: lhs_path,
            op,
            rhs,
        })
    }

    fn evaluate(&self, payload: &HookPayload) -> bool {
        let lhs_val = payload.get(&self.lhs).unwrap_or("");
        let cmp_eq = match &self.rhs {
            WhenRhs::Str(s) => lhs_val == s,
            WhenRhs::Int(n) => lhs_val.parse::<i64>().map(|v| v == *n).unwrap_or(false),
        };
        match self.op {
            WhenOp::Eq => cmp_eq,
            WhenOp::Ne => !cmp_eq,
        }
    }
}

fn parse_var_ref(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.strip_prefix("${")?.strip_suffix('}')?;
    if s.is_empty()
        || s.chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    {
        return None;
    }
    Some(s.to_string())
}

fn strip_quotes(s: &str) -> Option<&str> {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return Some(&s[1..s.len() - 1]);
        }
    }
    None
}

// ─── Executor ──────────────────────────────────────────────────────────

/// Per-invocation timeout. Children alive after this are killed with
/// SIGKILL (or `kill()` on non-Unix).
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum bytes per per-event log file. Older entries are FIFO-evicted
/// when the next write would exceed this cap.
pub const HOOK_LOG_MAX_BYTES: u64 = 1024 * 1024;
const HOOK_WORKERS: usize = 4;
const HOOK_QUEUE_DEPTH: usize = 64;
const HOOK_ARGV_MAX_BYTES: usize = 1024 * 1024;

/// Thread-safe registry of hooks indexed by event. Used by the server to
/// dispatch each lifecycle event to all matching hooks, asynchronously.
///
/// `HookExecutor` is cheap to clone (`Arc` internally) and `Send + Sync`.
#[derive(Clone)]
pub struct HookExecutor {
    inner: Arc<Mutex<ExecutorInner>>,
    /// Optional override for the log directory. Used by tests; production
    /// code leaves this `None` so [`hooks_log_dir`] picks the XDG path.
    log_dir_override: Option<PathBuf>,
    pool: Arc<HookPool>,
}

struct ExecutorInner {
    by_event: HashMap<HookEvent, Vec<Hook>>,
}

struct HookJob {
    event: &'static str,
    argv: Vec<String>,
    log_dir: PathBuf,
}

struct HookQueue {
    jobs: VecDeque<HookJob>,
    active: usize,
    accepting: bool,
}

struct PoolState {
    queue: Mutex<HookQueue>,
    ready: Condvar,
    cancel: AtomicBool,
}

struct HookPool {
    state: Arc<PoolState>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl HookPool {
    fn new() -> Self {
        let state = Arc::new(PoolState {
            queue: Mutex::new(HookQueue {
                jobs: VecDeque::new(),
                active: 0,
                accepting: true,
            }),
            ready: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let mut workers = Vec::new();
        for i in 0..HOOK_WORKERS {
            let state = Arc::clone(&state);
            if let Ok(worker) =
                thread::Builder::new()
                    .name(format!("ezpn-hook-{i}"))
                    .spawn(move || loop {
                        let job = {
                            let mut queue = state.queue.lock().unwrap_or_else(|e| e.into_inner());
                            while queue.jobs.is_empty() && queue.accepting {
                                queue = state.ready.wait(queue).unwrap_or_else(|e| e.into_inner());
                            }
                            let Some(job) = queue.jobs.pop_front() else {
                                return;
                            };
                            queue.active += 1;
                            job
                        };
                        run_one(job.event, &job.argv, &job.log_dir, &state.cancel);
                        let mut queue = state.queue.lock().unwrap_or_else(|e| e.into_inner());
                        queue.active -= 1;
                        state.ready.notify_all();
                    })
            {
                workers.push(worker);
            }
        }
        Self { state, workers }
    }

    fn submit(&self, job: HookJob) -> bool {
        let mut queue = self.state.queue.lock().unwrap_or_else(|e| e.into_inner());
        if self.workers.is_empty() || !queue.accepting || queue.jobs.len() >= HOOK_QUEUE_DEPTH {
            return false;
        }
        queue.jobs.push_back(job);
        self.state.ready.notify_one();
        true
    }

    fn shutdown(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        let mut queue = self.state.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.accepting = false;
        self.state.ready.notify_all();
        while queue.active > 0 || !queue.jobs.is_empty() {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                self.state.cancel.store(true, Ordering::Release);
                queue.jobs.clear();
                self.state.ready.notify_all();
                return false;
            };
            (queue, _) = self
                .state
                .ready
                .wait_timeout(queue, remaining)
                .unwrap_or_else(|e| e.into_inner());
        }
        true
    }
}

impl Drop for HookPool {
    fn drop(&mut self) {
        self.state.cancel.store(true, Ordering::Release);
        let mut queue = self.state.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.accepting = false;
        queue.jobs.clear();
        self.state.ready.notify_all();
        // Explicit shutdown drains before this point. Drop never waits on
        // external commands; workers observe cancellation on their next tick.
    }
}

impl HookExecutor {
    pub fn new(hooks: Vec<Hook>) -> Self {
        let mut by_event: HashMap<HookEvent, Vec<Hook>> = HashMap::new();
        for h in hooks {
            by_event.entry(h.event).or_default().push(h);
        }
        Self {
            inner: Arc::new(Mutex::new(ExecutorInner { by_event })),
            log_dir_override: None,
            pool: Arc::new(HookPool::new()),
        }
    }

    /// Empty executor — used as a placeholder before config is parsed.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Replace the registered hooks atomically. Called on config hot-reload.
    pub fn replace(&self, hooks: Vec<Hook>) {
        let mut by_event: HashMap<HookEvent, Vec<Hook>> = HashMap::new();
        for h in hooks {
            by_event.entry(h.event).or_default().push(h);
        }
        if let Ok(mut g) = self.inner.lock() {
            g.by_event = by_event;
        }
    }

    /// Number of registered hooks for the given event. For tests/metrics.
    pub fn count_for(&self, event: HookEvent) -> usize {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.by_event.get(&event).map(Vec::len))
            .unwrap_or(0)
    }

    /// Total number of registered hooks across all events.
    pub fn total(&self) -> usize {
        self.inner
            .lock()
            .ok()
            .map(|g| g.by_event.values().map(Vec::len).sum())
            .unwrap_or(0)
    }

    /// Override the hooks log directory. Test-only; production callers
    /// should not invoke this.
    #[doc(hidden)]
    pub fn with_log_dir(mut self, dir: PathBuf) -> Self {
        self.log_dir_override = Some(dir);
        self
    }

    /// Stop accepting events and drain queued jobs for at most `timeout`.
    /// False means remaining jobs were cancelled; active process groups are
    /// killed on the next worker tick. All executor clones share shutdown.
    pub fn shutdown(&self, timeout: Duration) -> bool {
        self.pool.shutdown(timeout)
    }

    /// Fire all hooks registered for `event`, asynchronously.
    ///
    /// Substitution + predicate evaluation happens on the calling thread
    /// (cheap); the actual `Command::spawn` + wait loop runs on a worker
    /// pool with a bounded queue. The triggering action is never blocked.
    ///
    /// Returns the number of hooks dispatched (post-`when` filtering).
    pub fn fire(&self, event: HookEvent, payload: &HookPayload) -> usize {
        let hooks: Vec<Hook> = match self.inner.lock() {
            Ok(g) => g.by_event.get(&event).cloned().unwrap_or_default(),
            Err(_) => return 0,
        };
        if hooks.is_empty() {
            return 0;
        }

        let mut dispatched = 0;
        for hook in hooks {
            if let Some(pred) = &hook.when {
                if !pred.evaluate(payload) {
                    continue;
                }
            }
            // Substitute every argv element ahead of time. The result is
            // owned and moved into the worker — no shared state, no
            // re-tokenization.
            let mut missing: Vec<String> = Vec::new();
            let mut argv = Vec::with_capacity(hook.exec.len().min(1024));
            let mut bytes = 0usize;
            let mut invalid = false;
            for arg in &hook.exec {
                match substitute_checked(arg, payload, &mut missing) {
                    Some(arg)
                        if !arg.contains('\0')
                            && arg.len() <= HOOK_ARGV_MAX_BYTES.saturating_sub(bytes) =>
                    {
                        bytes += arg.len();
                        argv.push(arg);
                    }
                    _ => {
                        invalid = true;
                        break;
                    }
                }
            }
            if invalid || argv.first().is_none_or(|s| s.is_empty()) {
                continue;
            }
            let log_dir = self.log_dir_override.clone().unwrap_or_else(hooks_log_dir);
            let event_name = event.name();
            if !missing.is_empty() {
                tracing::debug!(target: "hooks", event = event_name, missing_count = missing.len(), "missing payload fields");
            }
            if self.pool.submit(HookJob {
                event: event_name,
                argv,
                log_dir,
            }) {
                dispatched += 1;
            } else {
                tracing::warn!(target: "hooks", event = event_name, "hook dropped: queue full or executor stopped");
            }
        }
        dispatched
    }
}

impl fmt::Debug for HookExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.total();
        f.debug_struct("HookExecutor")
            .field("total_hooks", &total)
            .finish()
    }
}

// ─── Single-hook runner ────────────────────────────────────────────────

/// Spawn the child, enforce the timeout, capture output to the rotating
/// log file. Errors are swallowed — hooks are best-effort by design.
fn run_one(event_name: &str, argv: &[String], log_dir: &PathBuf, cancel: &AtomicBool) {
    if argv.is_empty() || cancel.load(Ordering::Acquire) {
        return;
    }
    let _ = fs::create_dir_all(log_dir);

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
    }
    #[cfg(not(unix))]
    {
        // No portable nonblocking pipe API in std. Discard output rather
        // than leave detached reader threads holding pipes indefinitely.
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            append_log(
                log_dir,
                event_name,
                &format!("[ezpn] failed to spawn hook: {e}\n"),
            );
            return;
        }
    };

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    #[cfg(unix)]
    if stdout
        .as_ref()
        .is_some_and(|pipe| set_nonblocking(pipe).is_err())
        || stderr
            .as_ref()
            .is_some_and(|pipe| set_nonblocking(pipe).is_err())
    {
        kill_hook(&mut child);
        let _ = child.wait();
        return;
    }
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    let started = Instant::now();
    let timed_out = loop {
        drain_output(&mut stdout, &mut stdout_bytes);
        drain_output(&mut stderr, &mut stderr_bytes);
        if started.elapsed() >= HOOK_TIMEOUT || cancel.load(Ordering::Acquire) {
            kill_hook(&mut child);
            break true;
        }
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                kill_hook(&mut child);
                break false;
            }
        }
    };
    // Hooks are bounded invocations, not background service launchers.
    // Terminate any same-group descendants even if the direct child exited.
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }

    let status = match child.wait() {
        Ok(s) => format!("status={s}"),
        Err(_) => "status=unknown".to_string(),
    };

    drain_output(&mut stdout, &mut stdout_bytes);
    drain_output(&mut stderr, &mut stderr_bytes);
    let stdout_text = String::from_utf8_lossy(&stdout_bytes);
    let stderr_text = String::from_utf8_lossy(&stderr_bytes);

    let mut buf = String::new();
    // Expanded argv may include credentials; never record it automatically.
    buf.push_str(&format!("[ezpn] event={event_name}\n"));
    if !stdout_text.is_empty() {
        buf.push_str("[stdout]\n");
        buf.push_str(&stdout_text);
        if !stdout_text.ends_with('\n') {
            buf.push('\n');
        }
    }
    if !stderr_text.is_empty() {
        buf.push_str("[stderr]\n");
        buf.push_str(&stderr_text);
        if !stderr_text.ends_with('\n') {
            buf.push('\n');
        }
    }
    if timed_out {
        buf.push_str(&format!(
            "[ezpn] timeout: killed after {:?} ({})\n",
            HOOK_TIMEOUT, status
        ));
    } else {
        buf.push_str(&format!("[ezpn] {}\n", status));
    }

    append_log(log_dir, event_name, &buf);
}

fn kill_hook(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(unix)]
fn set_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn drain_output<R: std::io::Read>(pipe: &mut Option<R>, output: &mut Vec<u8>) {
    let Some(reader) = pipe else {
        return;
    };
    let mut buf = [0u8; 8192];
    // Bound work per tick as well as retained bytes; a noisy child must not
    // keep us in a read loop past the timeout. Keep draining after truncation.
    for _ in 0..8 {
        match reader.read(&mut buf) {
            Ok(0) => {
                *pipe = None;
                break;
            }
            Ok(n) => {
                let keep = n.min((HOOK_LOG_MAX_BYTES as usize / 8).saturating_sub(output.len()));
                output.extend_from_slice(&buf[..keep]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => {
                *pipe = None;
                break;
            }
        }
    }
}

/// Append `body` to `<log_dir>/<event>-<unix>.log`. If the *current* file
/// would exceed [`HOOK_LOG_MAX_BYTES`], roll over by starting a fresh file
/// with a new timestamp. We then GC older files for the same event so the
/// directory does not grow without bound.
fn append_log(log_dir: &PathBuf, event_name: &str, body: &str) {
    static NEXT_LOG: AtomicU64 = AtomicU64::new(0);
    static LOG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let serial = NEXT_LOG.fetch_add(1, Ordering::Relaxed);
    let target = log_dir.join(format!(
        "{event_name}-{now}-{}-{serial}.log",
        std::process::id()
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    if let Ok(mut f) = options.open(&target) {
        let mut end = body.len().min(HOOK_LOG_MAX_BYTES as usize);
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let _ = f.write_all(&body.as_bytes()[..end]);
    }

    // Best-effort GC: keep at most 5 log files per event.
    gc_logs_for(log_dir, event_name, 5);
}

fn gc_logs_for(log_dir: &PathBuf, event_name: &str, keep: usize) {
    let prefix = format!("{event_name}-");
    let entries = match fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if !name.starts_with(&prefix) || !name.ends_with(".log") {
                return None;
            }
            if !e.file_type().ok()?.is_file() {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some((e.path(), meta.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|a| a.1);
    let drop = files.len() - keep;
    for (path, _) in files.into_iter().take(drop) {
        let _ = fs::remove_file(path);
    }
}

/// Resolve `$XDG_STATE_HOME/ezpn/hooks` with the same fallback chain as
/// the rest of the daemon's state directory (`observability::state_dir`).
/// We re-implement instead of importing to avoid a cross-module test
/// coupling for what is a 6-line function.
pub fn hooks_log_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return p.join("ezpn").join("hooks");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p.join(".local").join("state").join("ezpn").join("hooks");
        }
    }
    PathBuf::from(".ezpn").join("hooks")
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn reliability_predicate_operator_inside_literal() {
        let pred = WhenPredicate::parse("${pane.command} != \"a==b\"").unwrap();
        assert!(!pred.evaluate(&payload(&[("pane.command", "a==b")])));
    }

    #[test]
    fn reliability_log_records_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        append_log(
            &dir,
            "after_attach",
            &"x".repeat(HOOK_LOG_MAX_BYTES as usize + 100),
        );
        append_log(
            &dir,
            "after_attach",
            &"y".repeat(HOOK_LOG_MAX_BYTES as usize),
        );
        for entry in fs::read_dir(dir).unwrap() {
            assert!(entry.unwrap().metadata().unwrap().len() <= HOOK_LOG_MAX_BYTES);
        }
    }

    #[cfg(unix)]
    #[test]
    fn reliability_executor_queue_and_shutdown_are_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let hook = Hook::from_raw(RawHook {
            event: "after_attach".into(),
            exec: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
            when: None,
        })
        .unwrap();
        let executor = HookExecutor::new(vec![hook]).with_log_dir(tmp.path().join("hooks"));
        let mut accepted = 0;
        for _ in 0..200 {
            accepted += executor.fire(HookEvent::AfterAttach, &HookPayload::new());
        }
        assert!((HOOK_QUEUE_DEPTH..=HOOK_QUEUE_DEPTH + HOOK_WORKERS).contains(&accepted));
        let start = Instant::now();
        assert!(!executor.shutdown(Duration::from_millis(30)));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(executor.shutdown(Duration::from_secs(2)));
        assert_eq!(
            executor.fire(HookEvent::AfterAttach, &HookPayload::new()),
            0
        );
        assert!(executor.pool.state.queue.lock().unwrap().jobs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn reliability_hooks_do_not_log_expanded_argv_and_logs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("hooks");
        let hook = Hook::from_raw(RawHook {
            event: "after_attach".into(),
            exec: vec!["/usr/bin/true".into(), "${credential}".into()],
            when: None,
        })
        .unwrap();
        let executor = HookExecutor::new(vec![hook]).with_log_dir(dir.clone());
        assert_eq!(
            executor.fire(
                HookEvent::AfterAttach,
                &HookPayload::new().set("credential", "private-token")
            ),
            1
        );
        assert!(executor.shutdown(Duration::from_secs(2)));
        let logs: Vec<_> = fs::read_dir(dir).unwrap().map(Result::unwrap).collect();
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        let body = fs::read_to_string(logs[0].path()).unwrap();
        assert!(body.contains("status="));
        assert!(!body.contains("private-token"));
    }

    #[test]
    fn reliability_substitution_rejects_oversized_output() {
        let p = payload(&[("value", &"x".repeat(HOOK_ARGV_MAX_BYTES))]);
        assert!(substitute_checked("${value}${value}", &p, &mut vec![]).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn reliability_shutdown_kills_descendant_processes() {
        let tmp = tempfile::tempdir().unwrap();
        let started = tmp.path().join("started");
        let sentinel = tmp.path().join("survived");
        let hook = Hook::from_raw(RawHook {
            event: "after_attach".into(),
            exec: vec![
                "/bin/sh".into(),
                "-c".into(),
                "touch \"$1\"; (sleep 0.5; touch \"$2\") & wait".into(),
                "hook".into(),
                started.to_string_lossy().into(),
                sentinel.to_string_lossy().into(),
            ],
            when: None,
        })
        .unwrap();
        let executor = HookExecutor::new(vec![hook]).with_log_dir(tmp.path().join("hooks"));
        executor.fire(HookEvent::AfterAttach, &HookPayload::new());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !started.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.exists());
        assert!(!executor.shutdown(Duration::ZERO));
        assert!(executor.shutdown(Duration::from_secs(2)));
        thread::sleep(Duration::from_millis(700));
        assert!(!sentinel.exists());
    }

    fn payload(pairs: &[(&str, &str)]) -> HookPayload {
        let mut p = HookPayload::new();
        for (k, v) in pairs {
            p.insert(*k, *v);
        }
        p
    }

    // ─── HookEvent vocabulary ──────────────────────────────────────────

    #[test]
    fn event_vocabulary_is_complete_and_round_trips() {
        // The ten frozen v1 events.
        let names = [
            "before_attach",
            "after_attach",
            "before_detach",
            "after_detach",
            "after_session_create",
            "before_session_destroy",
            "after_pane_spawn",
            "after_pane_exit",
            "on_cwd_change",
            "on_focus_change",
            "on_config_reload",
        ];
        // 11 events total (the spec lists 10 categories but session_create
        // and session_destroy are siblings; the issue body counts both).
        assert_eq!(HookEvent::all().len(), names.len());
        for name in names {
            let ev = HookEvent::from_str(name).expect(name);
            assert_eq!(ev.name(), name);
        }
        assert!(HookEvent::from_str("after_pane_spawned").is_none());
    }

    #[test]
    fn raw_hook_with_unknown_event_is_rejected() {
        let raw = RawHook {
            event: "after_typo".to_string(),
            exec: vec!["true".to_string()],
            when: None,
        };
        let err = Hook::from_raw(raw).unwrap_err();
        assert!(matches!(err, HookParseError::UnknownEvent(_)));
    }

    #[test]
    fn raw_hook_with_empty_exec_is_rejected() {
        let raw = RawHook {
            event: "after_attach".to_string(),
            exec: vec![],
            when: None,
        };
        assert!(matches!(
            Hook::from_raw(raw),
            Err(HookParseError::EmptyExec)
        ));
    }

    // ─── Substitution: shell injection safety ──────────────────────────

    #[test]
    fn substitute_does_not_resplit_on_whitespace() {
        let p = payload(&[("user_var", "; rm -rf ~")]);
        let mut missing = Vec::new();
        let out = substitute("${user_var}", &p, &mut missing);
        // Critical: the literal value comes through as a SINGLE string.
        assert_eq!(out, "; rm -rf ~");
        assert!(missing.is_empty());
    }

    #[test]
    fn substitute_preserves_argv_arity_with_hostile_input() {
        // The acceptance test from the issue:
        //   exec = ["sh", "-c", "${user_var}"], user_var = "; rm -rf ~"
        // After substitution, argv must STILL have exactly 3 elements;
        // the third is the literal payload value.
        let p = payload(&[("user_var", "; rm -rf ~")]);
        let exec = [
            "sh".to_string(),
            "-c".to_string(),
            "${user_var}".to_string(),
        ];
        let mut missing = Vec::new();
        let argv: Vec<String> = exec
            .iter()
            .map(|s| substitute(s, &p, &mut missing))
            .collect();
        assert_eq!(argv.len(), 3, "substitution must not change argv length");
        assert_eq!(argv[0], "sh");
        assert_eq!(argv[1], "-c");
        assert_eq!(argv[2], "; rm -rf ~");
        // The shell still re-parses argv[2] when sh runs it — that's the
        // user's explicit choice. The framework's job is only to ensure
        // the daemon itself never invokes a shell.
    }

    #[test]
    fn substitute_handles_multiple_placeholders_in_one_arg() {
        let p = payload(&[("a", "X"), ("b", "Y")]);
        let mut missing = Vec::new();
        let out = substitute("[${a}-${b}]", &p, &mut missing);
        assert_eq!(out, "[X-Y]");
    }

    #[test]
    fn substitute_missing_field_is_empty_and_recorded() {
        let p = payload(&[]);
        let mut missing = Vec::new();
        let out = substitute("v=${nope}", &p, &mut missing);
        assert_eq!(out, "v=");
        assert_eq!(missing, vec!["nope".to_string()]);
    }

    #[test]
    fn substitute_preserves_unrelated_dollar_signs() {
        let p = payload(&[]);
        let mut missing = Vec::new();
        // `$5` and `$VAR` (no braces) are not our syntax.
        let out = substitute("price=$5 home=$HOME", &p, &mut missing);
        assert_eq!(out, "price=$5 home=$HOME");
    }

    // ─── `when` predicate ──────────────────────────────────────────────

    #[test]
    fn when_predicate_eq_int() {
        let pred = WhenPredicate::parse("${pane.exit_code} != 0").unwrap();
        assert!(pred.evaluate(&payload(&[("pane.exit_code", "1")])));
        assert!(!pred.evaluate(&payload(&[("pane.exit_code", "0")])));
    }

    #[test]
    fn when_predicate_eq_string() {
        let pred = WhenPredicate::parse("${pane.command} == \"vim\"").unwrap();
        assert!(pred.evaluate(&payload(&[("pane.command", "vim")])));
        assert!(!pred.evaluate(&payload(&[("pane.command", "nvim")])));
    }

    #[test]
    fn when_predicate_rejects_non_var_lhs() {
        let err = WhenPredicate::parse("foo == 1").unwrap_err();
        assert!(matches!(err, HookParseError::InvalidWhen(_)));
    }

    #[test]
    fn when_predicate_rejects_missing_op() {
        let err = WhenPredicate::parse("${x} 1").unwrap_err();
        assert!(matches!(err, HookParseError::InvalidWhen(_)));
    }

    // ─── Executor end-to-end ───────────────────────────────────────────

    #[test]
    fn executor_fires_only_matching_event() {
        let raw = RawHook {
            event: "after_attach".to_string(),
            exec: vec!["true".to_string()],
            when: None,
        };
        let exec = HookExecutor::new(vec![Hook::from_raw(raw).unwrap()]);
        assert_eq!(exec.count_for(HookEvent::AfterAttach), 1);
        assert_eq!(exec.count_for(HookEvent::AfterDetach), 0);
        // Firing an event with no hooks is a no-op returning 0.
        assert_eq!(exec.fire(HookEvent::AfterDetach, &HookPayload::new()), 0);
    }

    #[test]
    fn executor_hostile_user_var_does_not_invoke_shell() {
        // End-to-end version of the safety test: register a hook with
        // `exec = ["sh", "-c", "echo hi > ${out}"]` where `${out}` resolves
        // to a hostile-but-literal path. After firing, the file at the
        // *literal* path exists; no shell expansion happens at the daemon
        // layer (the `sh -c` inside is the user's explicit choice).
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("hooks");
        let canary_dir = tmp.path().to_path_buf();
        // The "hostile" payload: a path containing a space + semicolon.
        // If the daemon were to shell-split, the second token `;` would
        // become a separate command and the file would not be written
        // verbatim. With proper argv-array exec, the literal path is
        // passed through.
        let canary_path = canary_dir.join("a b;c.txt");

        let raw = RawHook {
            event: "after_pane_exit".to_string(),
            // Use printf which is more deterministic than `echo` across
            // shells, and redirect via shell so we exercise the
            // `["sh", "-c", ...]` opt-in pattern from the issue.
            exec: vec![
                "sh".to_string(),
                "-c".to_string(),
                "printf hi > \"${out}\"".to_string(),
            ],
            when: None,
        };
        let executor = HookExecutor::new(vec![Hook::from_raw(raw).unwrap()]).with_log_dir(log_dir);

        let payload = HookPayload::new().set("out", canary_path.to_string_lossy().to_string());
        let dispatched = executor.fire(HookEvent::AfterPaneExit, &payload);
        assert_eq!(dispatched, 1);

        // Wait briefly for the worker thread; capped well under the
        // executor's 5s timeout.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if canary_path.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            canary_path.exists(),
            "canary file at literal hostile path should exist: {}",
            canary_path.display()
        );
        let body = fs::read_to_string(&canary_path).unwrap();
        assert_eq!(body, "hi");
    }

    #[test]
    fn executor_when_predicate_skips_non_matching() {
        let raw = RawHook {
            event: "after_pane_exit".to_string(),
            exec: vec![
                "sh".to_string(),
                "-c".to_string(),
                "touch ${out}".to_string(),
            ],
            when: Some("${pane.exit_code} != 0".to_string()),
        };
        let tmp = tempfile::tempdir().unwrap();
        let exec = HookExecutor::new(vec![Hook::from_raw(raw).unwrap()])
            .with_log_dir(tmp.path().join("hooks"));

        // exit_code = 0 → predicate false → no fire.
        let canary_zero = tmp.path().join("zero.txt");
        let p = HookPayload::new()
            .set("pane.exit_code", "0")
            .set("out", canary_zero.to_string_lossy().to_string());
        let n = exec.fire(HookEvent::AfterPaneExit, &p);
        assert_eq!(n, 0);
        // Give the (non-existent) thread plenty of room — this assertion
        // is "the file is *never* created", so a small sleep is fine.
        thread::sleep(Duration::from_millis(200));
        assert!(!canary_zero.exists());

        // exit_code = 1 → predicate true → fires.
        let canary_one = tmp.path().join("one.txt");
        let p = HookPayload::new()
            .set("pane.exit_code", "1")
            .set("out", canary_one.to_string_lossy().to_string());
        let n = exec.fire(HookEvent::AfterPaneExit, &p);
        assert_eq!(n, 1);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if canary_one.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(canary_one.exists());
    }

    #[test]
    fn executor_kills_runaway_hook_within_timeout() {
        // `sleep 30` should be killed at ~5 s. We use a sentinel file to
        // tell whether the child got past the sleep — if it did, the
        // timeout did not work.
        let tmp = tempfile::tempdir().unwrap();
        let log_dir = tmp.path().join("hooks");
        let sentinel = tmp.path().join("post_sleep.txt");

        let raw = RawHook {
            event: "after_pane_exit".to_string(),
            // `sleep 30 && touch sentinel` — only touched if sleep
            // completes, which should never happen inside the timeout.
            exec: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("sleep 30 && touch {}", sentinel.to_string_lossy()),
            ],
            when: None,
        };
        let exec =
            HookExecutor::new(vec![Hook::from_raw(raw).unwrap()]).with_log_dir(log_dir.clone());
        let dispatched = exec.fire(HookEvent::AfterPaneExit, &HookPayload::new());
        assert_eq!(dispatched, 1);

        // Wait long enough for the timeout to elapse, then a margin for
        // the worker to write the log line.
        thread::sleep(HOOK_TIMEOUT + Duration::from_secs(2));
        assert!(
            !sentinel.exists(),
            "runaway child must be killed before sleep completes"
        );

        // Log file should exist and contain the timeout marker.
        let entries: Vec<_> = fs::read_dir(&log_dir).unwrap().flatten().collect();
        assert!(!entries.is_empty(), "hook log file must be created");
        let mut found_timeout = false;
        for e in entries {
            let body = fs::read_to_string(e.path()).unwrap_or_default();
            if body.contains("timeout: killed") {
                found_timeout = true;
                break;
            }
        }
        assert!(found_timeout, "log must record timeout kill");
    }

    #[test]
    fn replace_swaps_hooks_atomically() {
        let raw_a = RawHook {
            event: "after_attach".to_string(),
            exec: vec!["true".to_string()],
            when: None,
        };
        let raw_b = RawHook {
            event: "after_detach".to_string(),
            exec: vec!["true".to_string()],
            when: None,
        };
        let exec = HookExecutor::new(vec![Hook::from_raw(raw_a).unwrap()]);
        assert_eq!(exec.count_for(HookEvent::AfterAttach), 1);
        exec.replace(vec![Hook::from_raw(raw_b).unwrap()]);
        assert_eq!(exec.count_for(HookEvent::AfterAttach), 0);
        assert_eq!(exec.count_for(HookEvent::AfterDetach), 1);
    }
}
