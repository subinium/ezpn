//! Defense-in-depth helpers for Unix socket and secrets-file hardening.
//!
//! See issue #65. The daemon already chmods its sockets to `0o600` at bind
//! time, but a poorly-set `umask` or a shared `XDG_RUNTIME_DIR` (e.g. NFS
//! home, weirdly-permissioned `/tmp`) can race or outright leak attach
//! access on multi-user hosts. This module centralizes:
//!
//! * **Bind-time hardening** — verify that the directory hosting the
//!   socket is owned by us and not world-writable, and that the socket
//!   itself ends up at mode `0o600` with the right UID after bind.
//! * **Connect-time peer check** — read the connecting peer's UID via
//!   `SO_PEERCRED` (Linux) / `LOCAL_PEERCRED` (macOS) and refuse the
//!   connection if it doesn't match `getuid()`. This catches anyone who
//!   races a chmod between bind and accept on a shared host.
//! * **Secrets file mode check** — `secrets.toml` (and friends) must be
//!   `0o600` and owned by us; otherwise we refuse to load.
//!
//! All helpers are **Unix-only**. The module is gated behind `cfg(unix)`
//! at the call site (currently every supported platform), which means we
//! do not bother with `cfg` shims inside.

#![cfg(unix)]
// `verify_secrets_file` is currently the only helper without an in-tree
// caller — it's the centralized check #63 will adopt for `secrets.toml`
// loading. Keeping the allow() narrow avoids hiding genuinely-dead code.
#![allow(dead_code)]

use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

/// Inspect the directory that will host the daemon socket.
///
/// Refuses to proceed if:
/// * the directory does not exist (or `stat` fails),
/// * it is owned by us **and** has any group/other mode bit set
///   (i.e. a runtime dir we control with sloppy perms),
/// * it is owned by another user **and** lacks the sticky bit
///   (i.e. a shared dir without `/tmp`-style protection).
///
/// `/tmp` is the realistic fallback when `$XDG_RUNTIME_DIR` is unset
/// (default on macOS, common on minimal Linux installs). It is owned by
/// root and `0o1777`, which is *fine* for our purposes — the sticky bit
/// keeps other users from renaming/deleting our socket inode, and we
/// chmod the inode itself to `0o600` immediately after bind.
///
/// What we must refuse:
/// * `~/.ezpn-sockets` owned by the user but `0o755` — anyone can `+x`
///   to traverse and connect.
/// * A shared dir without sticky bit — symlink races and rename attacks.
pub fn harden_socket_dir(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("socket dir stat failed: {}", path.display()))?;

    if !meta.is_dir() {
        bail!("socket dir is not a directory: {}", path.display());
    }

    // SAFETY: getuid() always succeeds and never sets errno.
    let our_uid = unsafe { libc::getuid() };
    let owned_by_us = meta.uid() == our_uid;

    // Permission bits (drop file-type bits). The sticky bit lives in
    // the next nibble up.
    let perm = meta.mode() & 0o777;
    let sticky = meta.mode() & 0o1000 != 0;

    if owned_by_us {
        // Our own dir: nothing in group/other should ever have access.
        if perm & 0o077 != 0 {
            bail!(
                "socket dir {} owned by us but has insecure permissions {:o} (group/other access) — refusing to bind",
                path.display(),
                perm
            );
        }
    } else {
        // Foreign-owned dir (e.g. /tmp owned by root). We accept it only
        // if the sticky bit is set, which is the OS contract that
        // protects against symlink/rename attacks across users.
        if !sticky {
            bail!(
                "socket dir {} is owned by uid {} (not {}) and lacks the sticky bit — refusing to bind",
                path.display(),
                meta.uid(),
                our_uid
            );
        }
    }

    Ok(())
}

/// Re-stat the socket after bind, force `0o600`, and assert ownership.
///
/// Belt-and-suspenders against a permissive `umask` at bind time and
/// against a third party that managed to chmod the inode between our
/// `bind` and our `chmod` calls. Any deviation is a fatal startup error
/// (we'd rather crash than start with a permissive socket).
pub fn fix_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 failed for {}", path.display()))?;

    let meta = std::fs::metadata(path)
        .with_context(|| format!("socket stat failed: {}", path.display()))?;

    let perm = meta.mode() & 0o777;
    if perm != 0o600 {
        bail!(
            "socket {} has unexpected mode {:o} after chmod 0600",
            path.display(),
            perm
        );
    }

    let our_uid = unsafe { libc::getuid() };
    if meta.uid() != our_uid {
        bail!(
            "socket {} is owned by uid {}, not {}",
            path.display(),
            meta.uid(),
            our_uid
        );
    }

    Ok(())
}

/// Compose the abstract-namespace socket name for a session.
///
/// Format: `ezpn-<uid>-<session>`. Callers must prepend the leading NUL
/// byte themselves via `SocketAddr::from_abstract_name` (Linux/Android
/// only). UID is included so two users on the same host don't collide
/// in the abstract namespace.
pub fn abstract_socket_name(session: &str) -> String {
    let uid = unsafe { libc::getuid() };
    format!("ezpn-{uid}-{session}")
}

/// Bind a `UnixListener` in the abstract namespace (Linux only).
///
/// Returns an error on non-Linux platforms; callers should detect that
/// case and either fall back to a path-based bind or refuse to start.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn bind_abstract(name: &str) -> Result<std::os::unix::net::UnixListener> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    let addr = SocketAddr::from_abstract_name(name.as_bytes())
        .with_context(|| format!("from_abstract_name({name}) failed"))?;
    let listener =
        UnixListener::bind_addr(&addr).with_context(|| format!("bind_addr({name}) failed"))?;
    Ok(listener)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn bind_abstract(_name: &str) -> Result<std::os::unix::net::UnixListener> {
    bail!("abstract namespace sockets are Linux-only")
}

/// Read the peer's effective UID for a connected `UnixStream`.
///
/// Uses `SO_PEERCRED` on Linux and `LOCAL_PEERCRED` on macOS / *BSD.
///
/// **macOS caveat**: `LOCAL_PEERCRED` returns *effective* UID, so a peer
/// running under `sudo` reports as `root` even if the real user is the
/// caller. Linux `SO_PEERCRED` reports the UID at `connect(2)` time.
/// Both are fine for our threat model: we only want to refuse cross-UID
/// connections, not enforce strict PID identity.
pub fn peer_uid(stream: &UnixStream) -> Result<u32> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();

    #[cfg(target_os = "linux")]
    {
        // SO_PEERCRED returns a `struct ucred { pid, uid, gid }`.
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `cred` and `len` outlive the syscall; pointer types match
        // the kernel's expectation.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(anyhow!(std::io::Error::last_os_error()))
                .context("getsockopt(SO_PEERCRED) failed");
        }
        Ok(cred.uid)
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // LOCAL_PEERCRED returns `struct xucred { cr_version, cr_uid, ... }`.
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERCRED,
                &mut cred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(anyhow!(std::io::Error::last_os_error()))
                .context("getsockopt(LOCAL_PEERCRED) failed");
        }
        Ok(cred.cr_uid)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    {
        // Fallback: refuse to silently allow connections we can't audit.
        let _ = fd;
        bail!("peer credential check not implemented on this platform");
    }
}

/// Verify that a secrets file is owned by us and mode `0o600`.
///
/// Intended for `$XDG_RUNTIME_DIR/ezpn/secrets.toml` (and any future
/// per-user secret blob). Centralizing the check here keeps callers
/// (#63 env-interp, future config loaders) consistent — leaking secrets
/// is the kind of bug that should fail loud, not be re-implemented in
/// each call site.
pub fn verify_secrets_file(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("secrets file stat failed: {}", path.display()))?;

    if !meta.is_file() {
        bail!("secrets path is not a regular file: {}", path.display());
    }

    let perm = meta.mode() & 0o777;
    if perm != 0o600 {
        bail!(
            "secrets file {} has insecure mode {:o} (must be 0600)",
            path.display(),
            perm
        );
    }

    let our_uid = unsafe { libc::getuid() };
    if meta.uid() != our_uid {
        bail!(
            "secrets file {} is owned by uid {}, not {}",
            path.display(),
            meta.uid(),
            our_uid
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn harden_socket_dir_accepts_0700() {
        let dir = tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        harden_socket_dir(dir.path()).expect("0700 dir is acceptable");
    }

    #[test]
    fn harden_socket_dir_refuses_0755() {
        // tempdir() lives under our UID, so 0o755 trips the
        // "owned by us but group/other accessible" branch.
        let dir = tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = harden_socket_dir(dir.path()).expect_err("0755 must be rejected");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("insecure permissions"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn harden_socket_dir_accepts_tmp_with_sticky_bit() {
        // `/tmp` is owned by root and 0o1777 on every supported platform.
        // We rely on the sticky bit to keep cross-user attacks at bay,
        // so this code path must not fail or test runs would break on
        // macOS where `/tmp` is the default fallback.
        let tmp = Path::new("/tmp");
        if std::fs::metadata(tmp).is_ok() {
            harden_socket_dir(tmp).expect("/tmp must be acceptable");
        }
    }

    #[test]
    fn fix_socket_permissions_chmods_and_verifies() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("sock");
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();

        fix_socket_permissions(&p).expect("chmod must succeed");

        let mode = std::fs::metadata(&p).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn peer_uid_roundtrip_on_pair() {
        // A connected `UnixStream::pair` is the canonical way to exercise
        // the syscall path in tests without listening on a real socket.
        let (a, b) = UnixStream::pair().unwrap();
        let our_uid = unsafe { libc::getuid() };
        assert_eq!(peer_uid(&a).unwrap(), our_uid);
        assert_eq!(peer_uid(&b).unwrap(), our_uid);
    }

    #[test]
    fn verify_secrets_file_accepts_0600() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("secrets.toml");
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        verify_secrets_file(&p).expect("0600 must be accepted");
    }

    #[test]
    fn verify_secrets_file_refuses_0644() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("secrets.toml");
        std::fs::write(&p, b"").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = verify_secrets_file(&p).expect_err("0644 must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("insecure mode"), "unexpected error: {msg}");
    }

    #[test]
    fn abstract_socket_name_includes_uid_and_session() {
        let name = abstract_socket_name("demo");
        let our_uid = unsafe { libc::getuid() };
        assert_eq!(name, format!("ezpn-{our_uid}-demo"));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn abstract_namespace_bind_roundtrips() {
        // Spec acceptance criterion: abstract namespace mode round-trips
        // attach/detach on Linux. We test the bind+connect handshake at
        // the socket level — full client wiring lives in client.rs and
        // is out of scope for this issue.
        use std::io::{Read, Write};
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixStream};

        // Use the test's own pid so parallel test binaries don't collide.
        let name = format!("ezpn-test-{}", std::process::id());
        let listener = bind_abstract(&name).expect("abstract bind must succeed on Linux");

        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let mut client =
            UnixStream::connect_addr(&addr).expect("abstract connect must succeed on Linux");

        let (mut server, _) = listener.accept().expect("accept");
        client.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        server.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping");
    }
}
