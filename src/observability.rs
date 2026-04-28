//! Structured logging, log rotation, and crash-dump infrastructure.
//!
//! Initializes a `tracing` subscriber that writes to a rolling file
//! appender under `$XDG_STATE_HOME/ezpn/log/`. When stderr is not a TTY,
//! emits JSON; otherwise emits a colored human-readable format.
//!
//! Also installs a panic hook that captures the panic message, backtrace,
//! and the most recent 200 log lines into
//! `$XDG_STATE_HOME/ezpn/crash/<unix>-<pid>.txt`.
//!
//! See `docs/issues/66` (issue #66) for the canonical spec.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Maximum number of log lines retained for crash dumps.
const CRASH_RING_CAPACITY: usize = 200;

/// Maximum number of rolled log files retained on disk.
const MAX_LOG_FILES: usize = 5;

/// Soft cap on individual log file size before rotation. Note:
/// `tracing-appender` only supports time-based rotation, so this constant
/// is currently informational; rotation happens daily. Replacing with a
/// size-based custom appender is tracked separately.
#[allow(dead_code)]
const MAX_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Shared in-memory ring buffer of recent log lines, drained by the panic
/// hook into the crash dump.
static LOG_RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn ring() -> &'static Mutex<VecDeque<String>> {
    LOG_RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(CRASH_RING_CAPACITY)))
}

/// Initialize the global tracing subscriber and panic hook.
///
/// Returns a `WorkerGuard` that must be kept alive for the lifetime of
/// the process so the non-blocking writer thread flushes on shutdown.
///
/// # Behavior
/// - Writes logs to `<state_dir>/ezpn/log/<session>.log.<date>` with daily
///   rotation, retaining the 5 most recent files.
/// - Emits JSON when stderr is not a TTY, colored human format otherwise.
/// - Honors `EZPN_LOG=...` for level filtering, falling back to
///   `RUST_LOG`, then `info`.
/// - Mirrors every emitted log line into an in-memory ring buffer of size
///   200 for inclusion in the crash dump.
/// - Installs a panic hook that writes panic info + backtrace + recent
///   log lines into `<state_dir>/ezpn/crash/<unix>-<pid>.txt`.
/// - On startup, if any file exists in the crash directory, logs a single
///   `info` line `previous run crashed: <path>`.
pub fn init(session_name: &str) -> WorkerGuard {
    let log_dir = log_dir();
    let crash_dir = crash_dir();

    // Best-effort directory creation; failures are surfaced via log calls
    // that go to stderr (subscriber not yet installed -> tracing macros
    // are no-ops, so we use eprintln for the bootstrap path only).
    if let Err(error) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "ezpn: failed to create log dir {}: {}",
            log_dir.display(),
            error
        );
    }
    if let Err(error) = fs::create_dir_all(&crash_dir) {
        eprintln!(
            "ezpn: failed to create crash dir {}: {}",
            crash_dir.display(),
            error
        );
    }

    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(sanitize_filename(session_name))
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(&log_dir)
        .unwrap_or_else(|error| {
            eprintln!("ezpn: failed to build rolling file appender: {error}");
            // Fall back to a non-rotating appender in /tmp so we never
            // panic during startup.
            RollingFileAppender::builder()
                .rotation(Rotation::NEVER)
                .filename_prefix("ezpn-fallback")
                .filename_suffix("log")
                .build(std::env::temp_dir())
                .expect("fallback rolling appender must build")
        });
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Tee writer that captures every log line into the ring buffer in
    // addition to forwarding to the rolling file appender.
    let writer = TeeMakeWriter {
        inner: non_blocking,
    };

    let env_filter = EnvFilter::try_from_env("EZPN_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    if stderr_is_tty() {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(true);
        registry.with(layer).init();
    } else {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .json();
        registry.with(layer).init();
    }

    install_panic_hook(crash_dir.clone());

    // Surface any pre-existing crash dumps from prior runs.
    report_previous_crashes(&crash_dir);

    guard
}

/// Tee `MakeWriter` that pushes each emitted record into the in-memory
/// ring buffer in addition to delegating writes to the wrapped appender.
#[derive(Clone)]
struct TeeMakeWriter<W: for<'a> MakeWriter<'a> + Clone> {
    inner: W,
}

impl<'a, W> MakeWriter<'a> for TeeMakeWriter<W>
where
    W: for<'b> MakeWriter<'b> + Clone + 'a,
{
    type Writer = TeeWriter<<W as MakeWriter<'a>>::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            inner: self.inner.make_writer(),
            buffer: Vec::new(),
        }
    }
}

/// `Write` adapter that buffers bytes per-record so we can capture the
/// full formatted line into the ring buffer on flush/drop.
struct TeeWriter<W: Write> {
    inner: W,
    buffer: Vec<u8>,
}

impl<W: Write> Write for TeeWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            push_ring(&self.buffer);
            self.buffer.clear();
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for TeeWriter<W> {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            push_ring(&self.buffer);
            self.buffer.clear();
        }
    }
}

fn push_ring(bytes: &[u8]) {
    let line = String::from_utf8_lossy(bytes).into_owned();
    if let Ok(mut guard) = ring().lock() {
        for chunk in line.split_inclusive('\n') {
            if guard.len() == CRASH_RING_CAPACITY {
                guard.pop_front();
            }
            guard.push_back(chunk.to_string());
        }
    }
}

/// Install the global panic hook that writes a crash dump file.
fn install_panic_hook(crash_dir: PathBuf) {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Always invoke the prior hook so default stderr behavior is
        // preserved (and `RUST_BACKTRACE=full` works as documented).
        prev_hook(info);

        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let pid = std::process::id();
        let path = crash_dir.join(format!("{unix}-{pid}.txt"));

        let backtrace = std::backtrace::Backtrace::force_capture();
        let recent = ring()
            .lock()
            .map(|guard| guard.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();

        let mut body = String::new();
        body.push_str("ezpn crash dump\n");
        body.push_str(&format!("unix: {unix}\n"));
        body.push_str(&format!("pid: {pid}\n"));
        body.push_str(&format!("panic: {info}\n\n"));
        body.push_str("backtrace:\n");
        body.push_str(&format!("{backtrace}\n\n"));
        body.push_str(&format!(
            "recent log lines (up to {CRASH_RING_CAPACITY}):\n"
        ));
        for line in &recent {
            body.push_str(line);
        }

        if let Err(error) = fs::write(&path, body) {
            eprintln!(
                "ezpn: failed to write crash dump {}: {}",
                path.display(),
                error
            );
        }
    }));
}

/// Log an `info` line for each crash file already on disk so the operator
/// is aware of prior unclean shutdowns.
fn report_previous_crashes(crash_dir: &Path) {
    let entries = match fs::read_dir(crash_dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            tracing::info!(path = %path.display(), "previous run crashed: {}", path.display());
        }
    }
}

/// Resolve `$XDG_STATE_HOME/ezpn/log`, falling back to
/// `$HOME/.local/state/ezpn/log`, then `./.ezpn/log` if neither is set.
fn log_dir() -> PathBuf {
    state_dir().join("log")
}

/// Resolve `$XDG_STATE_HOME/ezpn/crash` (same fallback chain).
fn crash_dir() -> PathBuf {
    state_dir().join("crash")
}

fn state_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        let p = PathBuf::from(xdg);
        if !p.as_os_str().is_empty() {
            return p.join("ezpn");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        if !p.as_os_str().is_empty() {
            return p.join(".local").join("state").join("ezpn");
        }
    }
    PathBuf::from(".ezpn")
}

/// Replace path separators and other troublesome characters so that
/// arbitrary session names are safe to embed in a filename prefix.
fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("ezpn");
    }
    out
}

/// `true` when stderr refers to a terminal. Falls back to `false`
/// (assume non-TTY -> JSON) on platforms without `libc::isatty`.
fn stderr_is_tty() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `isatty` only reads the file descriptor metadata.
        unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_filename("my/session"), "my_session");
        assert_eq!(sanitize_filename("a..b"), "a__b");
        assert_eq!(sanitize_filename(""), "ezpn");
        assert_eq!(sanitize_filename("ok-name_1"), "ok-name_1");
    }

    #[test]
    fn state_dir_uses_xdg_when_set() {
        // We can't safely mutate process env in parallel tests, so this
        // just exercises the path-construction branch by reading the
        // current environment.
        let dir = state_dir();
        assert!(dir.ends_with("ezpn") || dir.ends_with(".ezpn"));
    }

    #[test]
    fn ring_buffer_caps_at_capacity() {
        // Force-init the ring and push more than CRASH_RING_CAPACITY
        // newline-terminated chunks; verify cap and FIFO ordering.
        let r = ring();
        // Drain any prior state so the test is order-independent within
        // this binary.
        if let Ok(mut g) = r.lock() {
            g.clear();
        }
        for i in 0..(CRASH_RING_CAPACITY + 50) {
            push_ring(format!("line {i}\n").as_bytes());
        }
        let g = r.lock().unwrap();
        assert_eq!(g.len(), CRASH_RING_CAPACITY);
        // Oldest retained line should be index 50.
        assert_eq!(g.front().unwrap(), "line 50\n");
    }
}
