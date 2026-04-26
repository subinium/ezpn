//! 10-minute soak harness — exercises a long-lived daemon under sustained
//! attach/detach + pane churn to flush out memory leaks and slow deadlocks
//! that perf benchmarks ([`render_hotpaths`]) would never catch.
//!
//! ## Why it's gated
//!
//! Soak takes ~10 minutes on Linux and longer on macOS, so it's behind the
//! `soak` feature flag and is *not* picked up by the default `cargo bench`
//! invocation. Run locally with:
//!
//! ```bash
//! cargo bench --features soak --bench soak_10min
//! ```
//!
//! In CI it runs on the `soak-nightly` cron job (see `.github/workflows/ci.yml`),
//! never on PR checks.
//!
//! ## What it asserts
//!
//! - The daemon survives ~600 attach/detach cycles (1 cycle/sec × 10 min).
//! - Resident set size at end is below `RSS_BUDGET_MB` (default 600 MB).
//! - No cycle hangs longer than `STALL_BUDGET` (default 10 s).
//!
//! ## Limitations
//!
//! - `getrusage(RUSAGE_CHILDREN)` reports the daemon's RSS only after it
//!   exits, so the cap is checked on the post-shutdown total. We don't
//!   sample mid-run because that would require `/proc/self/status` (Linux
//!   only) and the cross-platform check is what matters first.
//! - PTY spawning is shell-only — the soak doesn't drive long-running
//!   programs since that would dominate the runtime in unrelated shell
//!   startup.

#![allow(dead_code)]

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};

const SOAK_DURATION_MIN: u64 = 10;
const CYCLES: u64 = SOAK_DURATION_MIN * 60; // 1 cycle/sec
const RSS_BUDGET_MB: u64 = 600;
const STALL_BUDGET: Duration = Duration::from_secs(10);

const C_PING: u8 = 0x05;
const C_HELLO: u8 = 0x07;
const C_DETACH: u8 = 0x02;
const C_ATTACH: u8 = 0x06;
const S_HELLO_OK: u8 = 0x85;
const S_PONG: u8 = 0x84;

fn ezpn_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ezpn"))
}

fn write_msg(stream: &mut impl Write, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&[tag])?;
    stream.write_all(&len)?;
    if !payload.is_empty() {
        stream.write_all(payload)?;
    }
    stream.flush()
}

fn read_tag(stream: &mut UnixStream) -> Option<u8> {
    use std::io::Read;
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).ok()?;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload).ok()?;
    }
    Some(tag[0])
}

struct SoakDaemon {
    child: Child,
    sock: PathBuf,
    _runtime: tempfile::TempDir,
    _data: tempfile::TempDir,
}

impl SoakDaemon {
    fn spawn(session: &str) -> Self {
        let runtime = tempfile::tempdir().expect("runtime tempdir");
        let data = tempfile::tempdir().expect("data tempdir");
        let runtime_path = runtime.path().to_path_buf();
        let mut child = Command::new(ezpn_bin())
            .args(["--server", session])
            .env("XDG_RUNTIME_DIR", &runtime_path)
            .env("XDG_DATA_HOME", data.path())
            .env_remove("EZPN")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ezpn --server");
        let sock = runtime_path.join(format!("ezpn-session-{session}.sock"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if sock.exists() {
                return Self {
                    child,
                    sock,
                    _runtime: runtime,
                    _data: data,
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("soak daemon socket never appeared");
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn shutdown(&mut self) {
        unsafe {
            libc::kill(self.pid() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for SoakDaemon {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            self.shutdown();
        }
    }
}

fn attach_and_detach(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .is_err()
    {
        return false;
    }
    // Hello
    let hello = br#"{"version":1,"capabilities":7,"client":"soak"}"#;
    if write_msg(&mut stream, C_HELLO, hello).is_err() {
        return false;
    }
    let Some(tag) = read_tag(&mut stream) else {
        return false;
    };
    if tag != S_HELLO_OK {
        return false;
    }
    // Attach (steal mode)
    let attach = br#"{"cols":80,"rows":24,"mode":"steal"}"#;
    if write_msg(&mut stream, C_ATTACH, attach).is_err() {
        return false;
    }
    // Detach immediately. The daemon goes headless on the last detach,
    // which is the path we want exercised here (memory leaks tend to
    // hide in detach handlers).
    let _ = write_msg(&mut stream, C_DETACH, &[]);
    true
}

fn ping(sock: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(sock) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .is_err()
    {
        return false;
    }
    if write_msg(&mut stream, C_PING, &[]).is_err() {
        return false;
    }
    matches!(read_tag(&mut stream), Some(tag) if tag == S_PONG)
}

/// Resident set size of `pid` in MB. Best-effort:
/// - Linux: parse `/proc/<pid>/status` `VmRSS`.
/// - macOS: shell out to `ps -o rss=` (KB).
///
/// Returns `None` when the platform isn't supported or the read fails.
fn rss_mb(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let body = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        let kb: u64 = s.trim().parse().ok()?;
        Some(kb / 1024)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn soak_attach_detach_churn(c: &mut Criterion) {
    let mut group = c.benchmark_group("soak");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs((SOAK_DURATION_MIN * 60) + 30));
    group.bench_function("10min_churn", |b| {
        b.iter_custom(|_iters| {
            let start = Instant::now();
            let mut daemon = SoakDaemon::spawn("soak");
            let mut stalls = 0u64;
            for cycle in 0..CYCLES {
                let cycle_start = Instant::now();
                if !attach_and_detach(&daemon.sock) {
                    panic!("soak attach failed at cycle {cycle}");
                }
                if cycle % 60 == 0 && !ping(&daemon.sock) {
                    panic!("soak ping failed at cycle {cycle}");
                }
                let elapsed = cycle_start.elapsed();
                if elapsed > STALL_BUDGET {
                    stalls += 1;
                    eprintln!("soak: cycle {cycle} took {:?} (stalls={stalls})", elapsed);
                }
                // Pace at ~1 cycle/sec.
                if elapsed < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_secs(1) - elapsed);
                }
            }
            // Sample RSS *before* shutdown — `getrusage` aside, this is
            // the only way to catch a leak that grows during the run.
            if let Some(mb) = rss_mb(daemon.pid()) {
                assert!(
                    mb < RSS_BUDGET_MB,
                    "soak: daemon RSS {mb} MB exceeds budget {RSS_BUDGET_MB} MB"
                );
            } else {
                eprintln!("soak: RSS unavailable on this platform — skipping memory cap check");
            }
            daemon.shutdown();
            assert_eq!(stalls, 0, "soak: {stalls} cycles exceeded {STALL_BUDGET:?}");
            start.elapsed()
        })
    });
    group.finish();
}

criterion_group!(soak, soak_attach_detach_churn);
criterion_main!(soak);
