//! A bounded, hand-rolled, ONE-SHOT HTTP client for talking to another
//! SuperTerminal instance's companion server as a paired peer.
//!
//! Scope: one-shot requests only -- `/sessions` and `/peer-input/<id>`. A
//! TOTAL round-trip deadline is correct here, matching
//! `companion::blender::capture_once`. It is NOT correct for `/stream/<id>`,
//! which the server holds open forever -- that route needs a different
//! client with different rules and must never reuse this one.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::companion::server::INPUT_CONTENT_TYPE;

mod sse;

/// A cap on the whole status-line-plus-headers section of a response. Our
/// own server's responses are always small (a handful of security headers
/// plus one or two more); this is a defensive ceiling, not a tuned budget.
const MAX_HEAD: usize = 8 * 1024;
/// A cap on the response BODY, checked against the declared `Content-Length`
/// before a single body byte is read -- an oversized response is refused
/// from its header alone, never buffered up to this limit and then refused.
pub const MAX_RESPONSE_BODY: usize = 1024 * 1024;
const READ_CHUNK: usize = 4096;

/// Where a paired peer's companion server lives and the secret that
/// authenticates us to it.
///
/// Not constructed anywhere outside tests yet — wiring a real endpoint into
/// a pane is a later phase (this one only builds the client). The
/// `cfg_attr(not(test), ...)` markers below follow the same convention as
/// `companion::hub::Origin::Attached`.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Endpoint {
    pub addr: SocketAddr,
    pub secret: String,
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub enum PeerError {
    /// The TCP connection itself could not be established (refused,
    /// unreachable, or the deadline elapsed while connecting).
    Connect(std::io::Error),
    /// The TOTAL round-trip deadline elapsed before a complete response was
    /// read -- this is what a peer that accepts and then sends nothing (or
    /// trickles bytes) hits, since every read re-applies the REMAINING
    /// budget rather than a fresh per-read timeout.
    Timeout,
    /// A socket write/read failed for a reason other than the deadline.
    Io(std::io::Error),
    /// The response's declared `Content-Length` exceeded [`MAX_RESPONSE_BODY`].
    /// Refused from the header alone -- the body is never read, let alone
    /// buffered.
    TooLarge,
    /// The response was not a well-formed HTTP/1.1 response this client
    /// understands (ambiguous or missing `Content-Length`, unparseable
    /// status line, non-UTF-8 headers, connection closed early). Rejected
    /// rather than guessed at, matching how `companion::http` treats
    /// ambiguous requests on the server side.
    BadResponse(&'static str),
    /// The server answered with a non-2xx status.
    Status(u16),
}

#[cfg_attr(not(test), allow(dead_code))]
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// One-shot HTTP/1.1 round trip: connect, write the request, read the
/// response. `deadline` is the TOTAL budget for the whole round trip, exactly
/// the pattern in `companion::blender::capture_once` (`blender.rs:55-75`) --
/// the remaining budget is re-applied before every single read, so a peer
/// that accepts the connection and then sends nothing (or trickles one byte
/// at a time) cannot hold this past the deadline the way a per-read timeout
/// alone would allow.
#[cfg_attr(not(test), allow(dead_code))]
fn round_trip(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    deadline: Duration,
) -> Result<(u16, Vec<u8>), PeerError> {
    let deadline_at = Instant::now() + deadline;
    let remaining = |now: Instant| -> Result<Duration, PeerError> {
        let left = deadline_at.saturating_duration_since(now);
        if left.is_zero() {
            Err(PeerError::Timeout)
        } else {
            Ok(left)
        }
    };

    let connect_budget = remaining(Instant::now())?;
    let mut stream =
        TcpStream::connect_timeout(&endpoint.addr, connect_budget).map_err(PeerError::Connect)?;

    let write_budget = remaining(Instant::now())?;
    stream
        .set_write_timeout(Some(write_budget))
        .map_err(PeerError::Io)?;

    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nX-Companion-Token: {}\r\nConnection: close\r\n",
        endpoint.addr, endpoint.secret
    );
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        ));
    } else {
        head.push_str("\r\n");
    }
    stream.write_all(head.as_bytes()).map_err(PeerError::Io)?;
    if let Some(body) = body {
        // Re-applied here rather than reusing `write_budget` above: a peer
        // stalling its TCP receive window between the two writes must not
        // let the write side alone run past the total deadline.
        let body_write_budget = remaining(Instant::now())?;
        stream
            .set_write_timeout(Some(body_write_budget))
            .map_err(PeerError::Io)?;
        stream.write_all(body).map_err(PeerError::Io)?;
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    let header_end = loop {
        if let Some(end) = find_header_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HEAD {
            return Err(PeerError::BadResponse("response headers too large"));
        }
        let left = remaining(Instant::now())?;
        stream
            .set_read_timeout(Some(left.max(Duration::from_millis(10))))
            .map_err(PeerError::Io)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(PeerError::BadResponse("closed before headers completed")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if is_timeout(&e) => return Err(PeerError::Timeout),
            Err(e) => return Err(PeerError::Io(e)),
        }
    };

    let head_str = std::str::from_utf8(&buf[..header_end - 4])
        .map_err(|_| PeerError::BadResponse("headers not utf-8"))?;
    let mut lines = head_str.split("\r\n");
    let status_line = lines
        .next()
        .ok_or(PeerError::BadResponse("empty response"))?;
    let status: u16 = status_line
        .splitn(3, ' ')
        .nth(1)
        .ok_or(PeerError::BadResponse("no status code"))?
        .parse()
        .map_err(|_| PeerError::BadResponse("status code not numeric"))?;

    let mut content_length: Option<usize> = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(PeerError::BadResponse("header without colon"));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(PeerError::BadResponse("duplicate content-length"));
            }
            content_length = Some(
                value
                    .trim()
                    .parse()
                    .map_err(|_| PeerError::BadResponse("bad content-length"))?,
            );
        }
    }
    let content_length = content_length.ok_or(PeerError::BadResponse("missing content-length"))?;
    if content_length > MAX_RESPONSE_BODY {
        return Err(PeerError::TooLarge);
    }

    let mut response_body: Vec<u8> = Vec::with_capacity(content_length);
    response_body.extend_from_slice(&buf[header_end..]);
    if response_body.len() > content_length {
        return Err(PeerError::BadResponse("body exceeded content-length"));
    }
    while response_body.len() < content_length {
        let left = remaining(Instant::now())?;
        stream
            .set_read_timeout(Some(left.max(Duration::from_millis(10))))
            .map_err(PeerError::Io)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(PeerError::BadResponse("closed before body completed")),
            Ok(n) => {
                let take = n.min(content_length - response_body.len());
                response_body.extend_from_slice(&chunk[..take]);
            }
            Err(e) if is_timeout(&e) => return Err(PeerError::Timeout),
            Err(e) => return Err(PeerError::Io(e)),
        }
    }

    Ok((status, response_body))
}

/// `GET` a one-shot route (`/sessions`). The secret rides the
/// `X-Companion-Token` header; `deadline` bounds the ENTIRE round trip.
#[cfg_attr(not(test), allow(dead_code))]
pub fn get(endpoint: &Endpoint, path: &str, deadline: Duration) -> Result<Vec<u8>, PeerError> {
    let (status, body) = round_trip(endpoint, "GET", path, None, deadline)?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(PeerError::Status(status))
    }
}

/// `POST` a one-shot route (`/peer-input/<id>`) with the server's required
/// envelope (`application/companion-input`). The secret rides the
/// `X-Companion-Token` header; `deadline` bounds the ENTIRE round trip.
#[cfg_attr(not(test), allow(dead_code))]
pub fn post(
    endpoint: &Endpoint,
    path: &str,
    body: &[u8],
    deadline: Duration,
) -> Result<(), PeerError> {
    let (status, _body) = round_trip(endpoint, "POST", path, Some(body), deadline)?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(PeerError::Status(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::hub::tests::RegisterLocalPty;
    use crate::companion::hub::Hub;
    use crate::companion::server::{start, ServerConfig};
    use crate::term_session::TermSession;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::time::Instant;

    fn full_grants() -> crate::peers::Grants {
        crate::peers::Grants {
            view: true,
            type_: true,
            spawn: true,
        }
    }

    fn thumbs() -> Arc<crate::companion::thumbs::Thumbnailer> {
        crate::companion::thumbs::Thumbnailer::new(
            std::env::temp_dir().join(format!("st-thumbcache-peerclient-{}", std::process::id())),
        )
    }

    #[test]
    fn a_valid_get_returns_parseable_json() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "peer-client-one", session.input_sender());
        let peer_id = crate::companion::auth::PeerId("peerA".into());
        hub.set_visible_to("t1", &peer_id, true);
        const PEER_SECRET: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>peer-client-test</title>",
                previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
                thumbs: thumbs(),
                peers: vec![crate::peers::PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: PEER_SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: PEER_SECRET.into(),
        };
        let body = get(&endpoint, "/sessions", Duration::from_secs(5)).expect("get succeeds");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
        assert!(json.as_array().is_some_and(|a| !a.is_empty()));
        assert!(
            String::from_utf8_lossy(&body).contains("peer-client-one"),
            "{}",
            String::from_utf8_lossy(&body)
        );

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn a_wrong_secret_returns_the_error_variant_not_a_panic_or_success() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "peer-client-two", session.input_sender());
        let peer_id = crate::companion::auth::PeerId("peerB".into());
        hub.set_visible_to("t1", &peer_id, true);
        const PEER_SECRET: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>peer-client-test</title>",
                previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
                thumbs: thumbs(),
                peers: vec![crate::peers::PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: PEER_SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: "wrongwrongwrongwrongwrongwrongww".into(),
        };
        let result = get(&endpoint, "/sessions", Duration::from_secs(5));
        assert!(matches!(result, Err(PeerError::Status(404))), "{result:?}");

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn a_valid_post_succeeds() {
        let session = TermSession::spawn(80, 24, 8, 16, None).expect("session spawns");
        let hub = Arc::new(Hub::new());
        hub.register("t1", "peer-client-three", session.input_sender());
        let peer_id = crate::companion::auth::PeerId("peerC".into());
        hub.set_visible_to("t1", &peer_id, true);
        const PEER_SECRET: &str = "cccccccccccccccccccccccccccccccc";
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>peer-client-test</title>",
                previews: Arc::new(crate::companion::previews::PreviewStore::new(None)),
                thumbs: thumbs(),
                peers: vec![crate::peers::PeerRecord {
                    id: peer_id,
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: PEER_SECRET.into(),
                    grants: full_grants(),
                }],
            },
        )
        .expect("server starts");
        let endpoint = Endpoint {
            addr: handle.addr(),
            secret: PEER_SECRET.into(),
        };
        let body = serde_json::json!({ "bytes": [104, 105] }).to_string();
        let result = post(
            &endpoint,
            "/peer-input/t1",
            body.as_bytes(),
            Duration::from_secs(5),
        );
        assert!(result.is_ok(), "{result:?}");

        handle.stop();
        session
            .shutdown()
            .join_with_deadline(Duration::from_secs(5));
    }

    #[test]
    fn an_oversized_response_is_refused_not_buffered() {
        // A bare stub that claims a body far past the cap in its headers,
        // then never actually sends it. If the client tried to buffer the
        // whole thing it would block until the deadline waiting on bytes
        // that never arrive; refusing from the headers alone must return
        // almost immediately instead.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                MAX_RESPONSE_BODY + 1
            );
            let _ = stream.write_all(head.as_bytes());
            // Deliberately never writes the (nonexistent) body.
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let started = Instant::now();
        let result = get(&endpoint, "/sessions", Duration::from_secs(3));
        assert!(matches!(result, Err(PeerError::TooLarge)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "refusal must come from the headers, not from waiting on a body that never arrives"
        );
    }

    #[test]
    fn a_connect_to_a_closed_port_fails_within_the_deadline() {
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let started = Instant::now();
        let result = get(&endpoint, "/sessions", Duration::from_millis(500));
        assert!(result.is_err(), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "a closed port must fail fast, not block"
        );
    }

    #[test]
    fn a_peer_that_accepts_then_sends_nothing_fails_at_the_deadline() {
        // This is the one that matters: a per-read socket timeout alone
        // would let a peer that never writes a byte hold the caller
        // forever. The deadline must be a TOTAL budget for the round trip.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            std::thread::sleep(Duration::from_secs(5));
            drop(stream);
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let started = Instant::now();
        let result = get(&endpoint, "/sessions", Duration::from_millis(300));
        assert!(matches!(result, Err(PeerError::Timeout)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an accepted but silent peer must not outlive the total deadline"
        );
    }

    #[test]
    fn trickling_peer_cannot_outlive_the_total_deadline() {
        // Total silence alone cannot tell a correct implementation (remaining
        // budget re-applied before every read) from a naive one (the full
        // deadline re-applied every read): both make exactly one read
        // attempt and both time out at ~the deadline. A trickle -- several
        // partial writes, each individually inside the per-read timeout, but
        // together spanning far past the deadline -- is what actually
        // exercises the difference. Same shape as
        // `companion::blender::tests::trickling_peer_cannot_outlive_the_total_deadline`.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            for _ in 0..200 {
                if stream.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let endpoint = Endpoint {
            addr,
            secret: "whatever".into(),
        };
        let started = Instant::now();
        let result = get(&endpoint, "/sessions", Duration::from_millis(400));
        assert!(matches!(result, Err(PeerError::Timeout)), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the total deadline must cut the trickle off, not the ~10s the peer keeps trickling for"
        );
    }
}
