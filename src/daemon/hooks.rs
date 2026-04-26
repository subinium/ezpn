//! SPEC 08 — Hooks system.
//!
//! User-defined shell commands triggered by daemon state changes. Per
//! SPEC 08 §4.4 the design mirrors the SPEC 04 / SPEC 07 worker-pool
//! pattern: the main loop is allergic to blocking, so every hook runs
//! on a worker thread with a hard timeout, and a saturated queue drops
//! the new job rather than block the producer.
//!
//! Pipeline:
//!
//! ```text
//!  main loop ── dispatch(event, vars) ──► HookManager
//!                                             │
//!                                             ▼ for every matching HookDef:
//!                                          sync_channel(64)
//!                                             │  try_send (drop on full)
//!                                             ▼
//!                                          one of N worker threads
//!                                             │  variable expansion
//!                                             │  Command::spawn
//!                                             │  wait_timeout(timeout_ms)
//!                                             ▼
//!                                          stdout/stderr → /dev/null
//!                                          exit !=0 / timeout → warn! log
//! ```
//!
//! v0.10 ships shell-string `command` only (per PRD §9-Q2). A structured
//! action enum lands with v0.11.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Worker count. Mirrors SPEC 04's snapshot pool.
pub(crate) const HOOK_POOL_SIZE: usize = 4;

/// Bounded queue depth for pending hook invocations. Sized for ~250 ms
/// of bursty activity at 256 events/sec; overflow drops the job and logs
/// once per second (rate-limited).
pub(crate) const HOOK_QUEUE_CAPACITY: usize = 64;

/// Default per-hook timeout (ms). Above the user-overridable max the
/// validator rejects at config load.
pub(crate) const HOOK_DEFAULT_TIMEOUT_MS: u32 = 5000;

/// Hard ceiling for `timeout_ms`. Above this the validator rejects at
/// config load — keeps a misconfigured 1-hour `sleep` from squatting a
/// worker forever.
pub(crate) const HOOK_MAX_TIMEOUT_MS: u32 = 30_000;

/// Grace period after `SIGTERM` before escalating to `SIGKILL`. A
/// well-behaved child that traps `SIGTERM` gets time to flush; a hung
/// one gets killed.
pub(crate) const HOOK_KILL_GRACE: Duration = Duration::from_millis(500);

/// Daemon events that can trigger hooks. The wire spelling is
/// kebab-case to match the user's TOML (`event = "pane-died"`).
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookEvent {
    BeforePaneSpawn,
    AfterPaneSpawn,
    PaneDied,
    PaneExited,
    ClientAttached,
    ClientDetached,
    LayoutChanged,
    TabCreated,
    TabClosed,
    SessionRenamed,
}

impl HookEvent {
    /// Human-readable name used in log lines.
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::BeforePaneSpawn => "before-pane-spawn",
            HookEvent::AfterPaneSpawn => "after-pane-spawn",
            HookEvent::PaneDied => "pane-died",
            HookEvent::PaneExited => "pane-exited",
            HookEvent::ClientAttached => "client-attached",
            HookEvent::ClientDetached => "client-detached",
            HookEvent::LayoutChanged => "layout-changed",
            HookEvent::TabCreated => "tab-created",
            HookEvent::TabClosed => "tab-closed",
            HookEvent::SessionRenamed => "session-renamed",
        }
    }
}

/// One `[[hooks]]` block from `~/.config/ezpn/config.toml`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HookDef {
    pub event: HookEvent,
    /// Either a single string (shell or argv parsed via `shell_words`-
    /// equivalent rules below) or an array of strings (exec'd directly).
    /// Untagged enum so the TOML stays ergonomic.
    pub command: HookCommand,
    #[serde(default)]
    pub shell: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
}

fn default_timeout_ms() -> u32 {
    HOOK_DEFAULT_TIMEOUT_MS
}

/// Either a string command (shell or single-word) or an exec'd argv.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum HookCommand {
    String(String),
    Argv(Vec<String>),
}

/// Validate one parsed `[[hooks]]` block. Rejects invalid event names
/// (handled by serde), unsupported `timeout_ms`, empty commands, and
/// shell-metachar in `shell = false` string commands.
pub fn validate(def: &HookDef, index: usize) -> Result<(), String> {
    if def.timeout_ms > HOOK_MAX_TIMEOUT_MS {
        return Err(format!(
            "hooks[{index}].timeout_ms must be <= {HOOK_MAX_TIMEOUT_MS}",
        ));
    }
    match &def.command {
        HookCommand::String(s) => {
            if s.is_empty() {
                return Err(format!("hooks[{index}].command must be non-empty"));
            }
            if !def.shell && contains_shell_metachar(s) {
                return Err(format!(
                    "hooks[{index}].command has shell metachars but shell=false (use shell=true or pass an argv array)",
                ));
            }
        }
        HookCommand::Argv(v) => {
            if v.is_empty() || v[0].is_empty() {
                return Err(format!(
                    "hooks[{index}].command argv must have a non-empty program",
                ));
            }
            if def.shell {
                return Err(format!(
                    "hooks[{index}].command is an argv but shell=true (set shell=false or pass a string)",
                ));
            }
        }
    }
    Ok(())
}

/// Conservative shell-metachar detection. The full POSIX set would also
/// flag `~`, `*`, `?`, etc., but we accept those as literals in argv-
/// style strings (they reach the child as bytes). The set below covers
/// what would actually break / inject under `shell = false` parsing.
fn contains_shell_metachar(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '|' | '&' | ';' | '<' | '>' | '`' | '$' | '(' | ')'))
}

/// One queued hook invocation.
struct HookJob {
    def: Arc<HookDef>,
    /// Pre-computed `{name} → value` substitution table. Built on the
    /// main loop *before* enqueue so the worker thread does not need
    /// access to per-event daemon state.
    vars: HashMap<&'static str, String>,
}

/// Owner of the hook worker pool. One per daemon process.
pub struct HookManager {
    /// Shared snapshot of currently-active hook defs. Reload swaps the
    /// `Arc`; in-flight workers see the snapshot they were dispatched
    /// against.
    defs: Arc<Vec<Arc<HookDef>>>,
    tx: Option<mpsc::SyncSender<HookJob>>,
    workers: Vec<JoinHandle<()>>,
}

impl HookManager {
    /// Spawn the worker pool. `defs` is the initial set; subsequent
    /// reloads use [`replace_defs`].
    pub fn spawn(defs: Vec<HookDef>) -> Self {
        let arc_defs: Vec<Arc<HookDef>> = defs.into_iter().map(Arc::new).collect();
        let (tx, rx) = mpsc::sync_channel::<HookJob>(HOOK_QUEUE_CAPACITY);
        // crossbeam was an option here for fan-out; a plain mpsc with one
        // shared receiver behind a Mutex matches SPEC 04's pattern more
        // closely. We use crossbeam_channel's `Receiver` because std mpsc
        // `Receiver` is `!Sync`. Reusing the SPEC-01 pool model.
        //
        // Channel choice: std mpsc + Mutex<Receiver> is the lowest-dep
        // option. We already pull in crossbeam-channel for SPEC 01, so
        // use it here for cleaner fan-out.
        let (work_tx, work_rx) = crossbeam_channel::bounded::<HookJob>(HOOK_QUEUE_CAPACITY);
        let mut workers = Vec::with_capacity(HOOK_POOL_SIZE);
        for worker_id in 0..HOOK_POOL_SIZE {
            let rx = work_rx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("ezpn-hooks-{worker_id}"))
                .spawn(move || run_worker(rx))
                .expect("spawn ezpn-hooks worker");
            workers.push(handle);
        }
        // Forwarder: drain the bounded mpsc into the crossbeam channel.
        // Lets the producer side use std mpsc's `try_send` semantics
        // identically to the rest of the daemon while workers fan-out
        // through crossbeam.
        std::thread::Builder::new()
            .name("ezpn-hooks-forward".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if work_tx.send(job).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn hooks forwarder");
        Self {
            defs: Arc::new(arc_defs),
            tx: Some(tx),
            workers,
        }
    }

    /// Hot-reload: atomically swap the active hook list. In-flight
    /// workers continue against their captured snapshot; new dispatches
    /// match against the new set. Wired up to `prefix r` in a follow-up
    /// (the daemon reload path lives in `keys.rs:396-408`); the API
    /// lands here so SPEC 08's reload story is testable today.
    #[allow(dead_code)]
    pub fn replace_defs(&mut self, new_defs: Vec<HookDef>) {
        let arc_defs: Vec<Arc<HookDef>> = new_defs.into_iter().map(Arc::new).collect();
        self.defs = Arc::new(arc_defs);
    }

    /// Number of currently-active hook definitions. Diagnostic only.
    #[allow(dead_code)]
    pub fn def_count(&self) -> usize {
        self.defs.len()
    }

    /// Fire `event` with the given variable map. Hooks matching `event`
    /// are enqueued via `try_send`; queue saturation drops the job and
    /// is left to the worker pool to log (rate-limited externally).
    pub fn dispatch(&self, event: HookEvent, vars: HashMap<&'static str, String>) {
        let Some(tx) = self.tx.as_ref() else { return };
        for def in self.defs.iter() {
            if def.event != event {
                continue;
            }
            let job = HookJob {
                def: Arc::clone(def),
                vars: vars.clone(),
            };
            if tx.try_send(job).is_err() {
                eprintln!(
                    "ezpn: hooks queue full, dropping {} invocation",
                    event.name()
                );
            }
        }
    }
}

impl Drop for HookManager {
    fn drop(&mut self) {
        // Drop tx first so the forwarder exits, which closes the
        // crossbeam side, which lets workers exit. Then join in
        // best-effort fashion (bounded by `HOOK_KILL_GRACE` already
        // applied per child).
        drop(self.tx.take());
        for h in self.workers.drain(..) {
            let _ = h.join();
        }
    }
}

fn run_worker(rx: crossbeam_channel::Receiver<HookJob>) {
    while let Ok(job) = rx.recv() {
        // Isolate per-hook panics so a buggy `Command::spawn` doesn't
        // tear the whole worker down.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_hook(&job);
        }));
        if let Err(_payload) = result {
            eprintln!("ezpn: hook worker recovered from panic");
        }
    }
}

fn run_hook(job: &HookJob) {
    let mut cmd = match build_command(&job.def, &job.vars) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ezpn: hook {} skipped: {e}", job.def.event.name());
            return;
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // New process group so SIGTERM/SIGKILL reach grandchildren too.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            // setsid() is a no-op if we're already a session leader, but
            // ensures the child becomes a process group leader so kill(-pid)
            // hits the whole tree.
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ezpn: hook {} spawn failed: {e}", job.def.event.name());
            return;
        }
    };
    let timeout = Duration::from_millis(job.def.timeout_ms.max(1) as u64);
    use wait_timeout::ChildExt;
    match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => {
            // happy path
        }
        Ok(Some(status)) => {
            eprintln!(
                "ezpn: hook {} exited {}",
                job.def.event.name(),
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "<signal>".to_string())
            );
        }
        Ok(None) => {
            // Timed out — SIGTERM, brief grace, SIGKILL.
            #[cfg(unix)]
            kill_pgrp(child.id() as libc::pid_t, libc::SIGTERM);
            std::thread::sleep(HOOK_KILL_GRACE);
            if matches!(child.try_wait(), Ok(None)) {
                #[cfg(unix)]
                kill_pgrp(child.id() as libc::pid_t, libc::SIGKILL);
                let _ = child.wait();
            }
            eprintln!(
                "ezpn: hook {} timed out after {} ms",
                job.def.event.name(),
                job.def.timeout_ms
            );
        }
        Err(e) => {
            eprintln!("ezpn: hook {} wait failed: {e}", job.def.event.name());
            let _ = child.kill();
        }
    }
}

#[cfg(unix)]
fn kill_pgrp(pid: libc::pid_t, sig: libc::c_int) {
    // -pid targets the process group; safe even if the child already exited.
    unsafe {
        libc::kill(-pid, sig);
    }
}

fn build_command(def: &HookDef, vars: &HashMap<&'static str, String>) -> Result<Command, String> {
    match &def.command {
        HookCommand::String(s) => {
            let expanded = expand_vars(s, vars);
            if def.shell {
                let mut c = Command::new("/bin/sh");
                c.arg("-c").arg(expanded);
                Ok(c)
            } else {
                // shell=false + string: must be a single word with no shell
                // metachars (validated at load time). Treat as the program
                // name with no arguments. Users wanting args should use the
                // argv form.
                if expanded.contains(char::is_whitespace) {
                    return Err(format!(
                        "string command must be a single word when shell=false (got '{expanded}')",
                    ));
                }
                Ok(Command::new(expanded))
            }
        }
        HookCommand::Argv(argv) => {
            if argv.is_empty() {
                return Err("argv command must have at least one element".to_string());
            }
            let expanded: Vec<String> = argv.iter().map(|a| expand_vars(a, vars)).collect();
            let mut c = Command::new(&expanded[0]);
            for arg in &expanded[1..] {
                c.arg(arg);
            }
            Ok(c)
        }
    }
}

/// Replace `{name}` placeholders with values from `vars`. Unknown keys
/// are left as-is (matches tmux's `#{undefined}` behaviour); empty keys
/// (e.g. `{}`) are also passed through verbatim.
pub fn expand_vars(template: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        // Look for matching '}'.
        let mut name = String::new();
        let mut closed = false;
        for nc in chars.by_ref() {
            if nc == '}' {
                closed = true;
                break;
            }
            name.push(nc);
        }
        if !closed || name.is_empty() {
            // `{` with no `}`, or `{}` — passthrough verbatim.
            out.push('{');
            out.push_str(&name);
            if closed {
                out.push('}');
            }
            continue;
        }
        // Lookup; unknown → leave the original `{name}` intact.
        match vars.get(name.as_str()) {
            Some(v) => out.push_str(v),
            None => {
                out.push('{');
                out.push_str(&name);
                out.push('}');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars_of(pairs: &[(&'static str, &str)]) -> HashMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    }

    #[test]
    fn expand_substitutes_known_vars() {
        let out = expand_vars(
            "pane {pane_id} died with exit {exit_code}",
            &vars_of(&[("pane_id", "3"), ("exit_code", "42")]),
        );
        assert_eq!(out, "pane 3 died with exit 42");
    }

    #[test]
    fn expand_passes_through_unknown_keys() {
        let out = expand_vars("{pane_id} and {nope}", &vars_of(&[("pane_id", "7")]));
        assert_eq!(out, "7 and {nope}");
    }

    #[test]
    fn expand_handles_empty_braces_and_unclosed() {
        let out = expand_vars("a{}b{unclosed", &vars_of(&[]));
        assert_eq!(out, "a{}b{unclosed");
    }

    #[test]
    fn validate_rejects_excessive_timeout() {
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::String("true".to_string()),
            shell: false,
            timeout_ms: 60_000,
        };
        let e = validate(&def, 0).unwrap_err();
        assert!(e.contains("must be <="), "got: {e}");
    }

    #[test]
    fn validate_rejects_shell_metachar_without_shell_flag() {
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::String("echo hi | tee out".to_string()),
            shell: false,
            timeout_ms: HOOK_DEFAULT_TIMEOUT_MS,
        };
        let e = validate(&def, 2).unwrap_err();
        assert!(e.contains("shell metachars"), "got: {e}");
    }

    #[test]
    fn validate_rejects_argv_with_shell_true() {
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::Argv(vec!["echo".into(), "hi".into()]),
            shell: true,
            timeout_ms: HOOK_DEFAULT_TIMEOUT_MS,
        };
        let e = validate(&def, 0).unwrap_err();
        assert!(e.contains("argv but shell=true"), "got: {e}");
    }

    #[test]
    fn validate_rejects_empty_argv() {
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::Argv(vec![]),
            shell: false,
            timeout_ms: HOOK_DEFAULT_TIMEOUT_MS,
        };
        let e = validate(&def, 0).unwrap_err();
        assert!(e.contains("non-empty program"), "got: {e}");
    }

    #[test]
    fn validate_accepts_well_formed_string() {
        let def = HookDef {
            event: HookEvent::ClientAttached,
            command: HookCommand::String("notify-send".to_string()),
            shell: false,
            timeout_ms: HOOK_DEFAULT_TIMEOUT_MS,
        };
        validate(&def, 0).unwrap();
    }

    #[test]
    fn validate_accepts_well_formed_argv() {
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::Argv(vec!["echo".into(), "{pane_id}".into()]),
            shell: false,
            timeout_ms: HOOK_DEFAULT_TIMEOUT_MS,
        };
        validate(&def, 0).unwrap();
    }

    #[test]
    fn dispatch_fires_only_matching_event() {
        // Use an argv hook that touches a tempfile; assert it ran for the
        // matching event and not for others.
        let dir = tempfile::tempdir().unwrap();
        let touch = dir.path().join("client-attached.flag");
        let other = dir.path().join("pane-died.flag");
        let def_match = HookDef {
            event: HookEvent::ClientAttached,
            command: HookCommand::Argv(vec![
                "/usr/bin/touch".into(),
                touch.to_string_lossy().into_owned(),
            ]),
            shell: false,
            timeout_ms: 2000,
        };
        let def_other = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::Argv(vec![
                "/usr/bin/touch".into(),
                other.to_string_lossy().into_owned(),
            ]),
            shell: false,
            timeout_ms: 2000,
        };
        let mgr = HookManager::spawn(vec![def_match, def_other]);
        mgr.dispatch(HookEvent::ClientAttached, vars_of(&[]));
        // Wait for the worker to run + drop on EXIT.
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline && !touch.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(touch.exists(), "matching hook must run");
        // Other hook should NOT have fired.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!other.exists(), "non-matching hook must not run");
        drop(mgr);
    }

    #[test]
    fn timeout_kills_runaway_child() {
        // sleep 30 with timeout 200ms — must come back inside ~1s.
        let def = HookDef {
            event: HookEvent::PaneDied,
            command: HookCommand::Argv(vec!["/bin/sleep".into(), "30".into()]),
            shell: false,
            timeout_ms: 200,
        };
        let mgr = HookManager::spawn(vec![def]);
        let t0 = std::time::Instant::now();
        mgr.dispatch(HookEvent::PaneDied, vars_of(&[]));
        // Drop forces the workers to drain; budget ~1.5s for SIGTERM grace
        // (500 ms) + SIGKILL + join.
        drop(mgr);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_secs(3),
            "timeout must reap runaway child quickly (got {elapsed:?})"
        );
    }
}
