//! Bounded, deadline-limited pre-attach handshakes off the daemon main loop.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::{protocol, socket_security};

const MAX_PENDING: usize = 16;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CONTROL_LIMIT: usize = 64 * 1024;

pub(super) enum Incoming {
    Attach {
        conn: UnixStream,
        cols: u16,
        rows: u16,
        mode: protocol::AttachMode,
    },
    Kill,
}

pub(super) struct Acceptor {
    rx: mpsc::Receiver<(Instant, Incoming)>,
    stopped: Arc<AtomicBool>,
}

impl Acceptor {
    pub(super) fn try_recv(&self) -> Result<Incoming, mpsc::TryRecvError> {
        loop {
            let (ready_at, incoming) = self.rx.try_recv()?;
            if ready_at.elapsed() < HANDSHAKE_TIMEOUT {
                return Ok(incoming);
            }
        }
    }
}

impl Drop for Acceptor {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

struct PendingGuard(Arc<AtomicUsize>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn start(listener: UnixListener, session_name: &str) -> io::Result<Acceptor> {
    listener.set_nonblocking(true)?;
    let (tx, rx) = mpsc::sync_channel(MAX_PENDING);
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&stopped);
    let pending = Arc::new(AtomicUsize::new(0));
    let session = session_name.to_owned();
    std::thread::Builder::new()
        .name("ezpn-accept".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let mut poll = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut poll, 1, 250) };
                if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return;
                }
                if ready <= 0 {
                    continue;
                }
                for _ in 0..MAX_PENDING {
                    let (conn, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(_) => return,
                    };
                    if pending
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                            (n < MAX_PENDING).then_some(n + 1)
                        })
                        .is_err()
                    {
                        continue;
                    }
                    let guard = PendingGuard(Arc::clone(&pending));
                    let accepted_at = Instant::now();
                    let tx = tx.clone();
                    let session = session.clone();
                    let _ = std::thread::Builder::new()
                        .name("ezpn-handshake".into())
                        .spawn(move || {
                            let _guard = guard;
                            if let Ok(Some(incoming)) = negotiate(
                                conn,
                                &session,
                                HANDSHAKE_TIMEOUT.saturating_sub(accepted_at.elapsed()),
                            ) {
                                if tx.try_send((Instant::now(), incoming)).is_ok() {
                                    crate::pane::wake_main_loop();
                                }
                            }
                        });
                }
            }
        })?;
    Ok(Acceptor { rx, stopped })
}

fn negotiate(conn: UnixStream, session: &str, timeout: Duration) -> io::Result<Option<Incoming>> {
    conn.set_nonblocking(false)?;
    if socket_security::peer_uid(&conn).map_err(io::Error::other)? != unsafe { libc::getuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "peer uid mismatch",
        ));
    }
    let mut io = protocol::DeadlineStream::new(&conn, timeout);
    io.write_all(&protocol::server_hello())?;
    let mut first = [0];
    io.read_exact(&mut first)?;
    if protocol::classify_first_byte(first[0]) != protocol::FirstByteKind::Tag {
        io.write_all(&protocol::incompat_for_legacy_client(session))?;
        return Ok(None);
    }
    let (mut tag, mut payload) =
        protocol::read_msg_limited(&mut first.as_slice().chain(&mut io), CONTROL_LIMIT)?;
    if tag == protocol::C_HELLO {
        let hello = protocol::parse_client_hello(&payload)?;
        if hello.proto_major != protocol::PROTO_MAJOR {
            io.write_all(&protocol::incompat_for_major_mismatch(&hello, session))?;
            return Ok(None);
        }
        (tag, payload) = protocol::read_msg_limited(&mut io, CONTROL_LIMIT)?;
    }
    let incoming = match tag {
        protocol::C_PING if payload.is_empty() => {
            protocol::write_msg(&mut io, protocol::S_PONG, &[])?;
            return Ok(None);
        }
        protocol::C_KILL if payload.is_empty() => Incoming::Kill,
        protocol::C_RESIZE => {
            let (cols, rows) = protocol::decode_resize(&payload)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid resize"))?;
            let (cols, rows) = protocol::normalize_size(cols, rows);
            Incoming::Attach {
                conn,
                cols,
                rows,
                mode: protocol::AttachMode::Steal,
            }
        }
        protocol::C_ATTACH => {
            let request: protocol::AttachRequest =
                serde_json::from_slice(&payload).map_err(io::Error::other)?;
            let (cols, rows) = protocol::normalize_size(request.cols, request.rows);
            Incoming::Attach {
                conn,
                cols,
                rows,
                mode: request.mode,
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected handshake frame",
            ))
        }
    };
    Ok(Some(incoming))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiated_v1_attach_and_legacy_probe() {
        for negotiated in [false, true] {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let worker =
                std::thread::spawn(move || negotiate(server, "test", Duration::from_secs(1)));
            assert_eq!(
                protocol::read_msg(&mut client).unwrap().0,
                protocol::S_VERSION
            );
            if negotiated {
                let hello = protocol::ClientHello {
                    proto_major: 1,
                    proto_minor: 9,
                    client_build: "test".into(),
                    supported_features: vec![],
                };
                protocol::write_msg(
                    &mut client,
                    protocol::C_HELLO,
                    &serde_json::to_vec(&hello).unwrap(),
                )
                .unwrap();
                protocol::write_msg(
                    &mut client,
                    protocol::C_RESIZE,
                    &protocol::encode_resize(80, 24),
                )
                .unwrap();
                assert!(matches!(
                    worker.join().unwrap().unwrap(),
                    Some(Incoming::Attach {
                        cols: 80,
                        rows: 24,
                        ..
                    })
                ));
            } else {
                protocol::write_msg(&mut client, protocol::C_PING, &[]).unwrap();
                assert_eq!(protocol::read_msg(&mut client).unwrap().0, protocol::S_PONG);
                assert!(worker.join().unwrap().unwrap().is_none());
            }
        }
    }

    #[test]
    fn mismatch_rejected_before_attach() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || negotiate(server, "test", Duration::from_secs(1)));
        protocol::read_msg(&mut client).unwrap();
        let hello = protocol::ClientHello {
            proto_major: 2,
            proto_minor: 0,
            client_build: "test".into(),
            supported_features: vec![],
        };
        protocol::write_msg(
            &mut client,
            protocol::C_HELLO,
            &serde_json::to_vec(&hello).unwrap(),
        )
        .unwrap();
        assert_eq!(
            protocol::read_msg(&mut client).unwrap().0,
            protocol::S_INCOMPAT
        );
        assert!(worker.join().unwrap().unwrap().is_none());
    }

    #[test]
    fn partial_handshake_expires_without_waiting_for_remaining_bytes() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let start = std::time::Instant::now();
        let worker =
            std::thread::spawn(move || negotiate(server, "test", Duration::from_millis(50)));
        protocol::read_msg(&mut client).unwrap();
        client.write_all(&[protocol::C_HELLO, 0]).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
