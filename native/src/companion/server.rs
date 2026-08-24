//! The companion server: bounded plain-thread HTTP/1.1 + SSE over the
//! tailnet. One acceptor, per-connection workers capped hard, read/write
//! deadlines everywhere, one ordinary request per connection.
//!
//! Authorization: capability token (query `t` for EventSource, header
//! `X-Companion-Token` for POST) checked before routing; the embedded page
//! itself is the only tokenless route (it is static and secret-free — the
//! token rides the URL fragment, which browsers never send). Exact-Host
//! validation closes DNS rebinding; POST additionally requires our exact
//! Origin (when present) and the non-safelisted companion content type, so
//! a cross-origin page cannot reach it without a preflight — which OPTIONS
//! rejection denies.

use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::themes::Theme;

use super::auth::token_matches;
use super::http::{parse_request, Method, ParseError, Request};
use super::hub::CompanionHub;
use super::input::{parse_body, symbolic_bytes, text_bytes, InputMsg};

pub const MAX_CONNS: usize = 8;
pub const MAX_SSE: usize = 4;
const READ_DEADLINE: Duration = Duration::from_secs(10);
const WRITE_DEADLINE: Duration = Duration::from_secs(10);
const SSE_POLL: Duration = Duration::from_millis(50);
const SSE_FLOOR: Duration = Duration::from_millis(200);
const SSE_HEARTBEAT: Duration = Duration::from_secs(2);
pub const INPUT_CONTENT_TYPE: &str = "application/companion-input";

/// How the server delivers input bytes to a session — the production sender
/// is alacritty's `EventLoopSender`; tests use an mpsc sender.
pub trait InputSink: Clone + Send + 'static {
    fn send_bytes(&self, bytes: Vec<u8>) -> bool;
}

impl InputSink for alacritty_terminal::event_loop::EventLoopSender {
    fn send_bytes(&self, bytes: Vec<u8>) -> bool {
        self.send(alacritty_terminal::event_loop::Msg::Input(bytes.into()))
            .is_ok()
    }
}

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub page: &'static str,
}

pub struct ServerHandle {
    /// Base URL without the fragment (the caller appends `#token`).
    pub url: String,
    addr: SocketAddr,
    cancel: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl ServerHandle {
    /// Flip cancellation and unblock the acceptor WITHOUT joining — cheap
    /// enough for the UI thread, so streams start dying before pane
    /// teardown even though the joins happen elsewhere.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr);
    }

    /// Stop accepting, cancel streams, join everything. Bounded: SSE loops
    /// observe the flag within one poll; readers hit their deadlines.
    pub fn stop(mut self) {
        self.cancel.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.addr); // unblock accept()
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        let workers: Vec<JoinHandle<()>> = std::mem::take(&mut *self.workers.lock().unwrap());
        for worker in workers {
            let _ = worker.join();
        }
    }
}

struct Shared<S: Clone> {
    hub: Arc<CompanionHub<S>>,
    theme: &'static Theme,
    token: String,
    host: String,
    page: &'static str,
    cancel: Arc<AtomicBool>,
    conns: AtomicUsize,
    sse: AtomicUsize,
}

pub fn start<S: InputSink>(
    hub: Arc<CompanionHub<S>>,
    theme: &'static Theme,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let listener = TcpListener::bind(cfg.bind)?;
    let addr = listener.local_addr()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let workers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::new(Shared {
        hub,
        theme,
        token: cfg.token,
        host: format!("{addr}"),
        page: cfg.page,
        cancel: Arc::clone(&cancel),
        conns: AtomicUsize::new(0),
        sse: AtomicUsize::new(0),
    });
    let acceptor_workers = Arc::clone(&workers);
    let acceptor_shared = Arc::clone(&shared);
    let acceptor = std::thread::Builder::new()
        .name("companion-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if acceptor_shared.cancel.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                if acceptor_shared.conns.load(Ordering::Acquire) >= MAX_CONNS {
                    let _ = respond(&stream, "503 Service Unavailable", &[], b"");
                    continue;
                }
                acceptor_shared.conns.fetch_add(1, Ordering::AcqRel);
                let worker_shared = Arc::clone(&acceptor_shared);
                let handle = std::thread::Builder::new()
                    .name("companion-conn".into())
                    .spawn(move || {
                        serve_connection(&worker_shared, stream);
                        worker_shared.conns.fetch_sub(1, Ordering::AcqRel);
                    });
                if let Ok(handle) = handle {
                    let mut list = acceptor_workers.lock().unwrap();
                    // Reap finished workers so the vec stays bounded.
                    list.retain(|h| !h.is_finished());
                    list.push(handle);
                }
            }
        })?;
    Ok(ServerHandle {
        url: format!("http://{addr}/"),
        addr,
        cancel,
        acceptor: Some(acceptor),
        workers,
    })
}

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("Referrer-Policy", "no-referrer"),
    ("Cross-Origin-Resource-Policy", "same-origin"),
    ("X-Content-Type-Options", "nosniff"),
    (
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'",
    ),
];

fn respond(
    mut stream: &TcpStream,
    status: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let _ = stream.set_write_timeout(Some(WRITE_DEADLINE));
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for (name, value) in SECURITY_HEADERS.iter().chain(extra) {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)
}

fn token_of(req: &Request) -> &str {
    req.header("x-companion-token")
        .or_else(|| req.query_param("t"))
        .unwrap_or("")
}

fn serve_connection<S: InputSink>(shared: &Shared<S>, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_DEADLINE));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let request = match parse_request(&mut reader) {
        Ok(request) => request,
        Err(ParseError::Unsupported) => {
            let _ = respond(&stream, "405 Method Not Allowed", &[], b"");
            return;
        }
        Err(ParseError::TooLarge) => {
            let _ = respond(&stream, "413 Content Too Large", &[], b"");
            return;
        }
        Err(ParseError::BadRequest(_)) => {
            let _ = respond(&stream, "400 Bad Request", &[], b"");
            return;
        }
    };
    let path = request.path.clone();
    let host_ok = request.header("host") == Some(shared.host.as_str());
    // Token FIRST on protected routes (constant-time; a bad token learns
    // nothing, not even that the Host was wrong), then exact Host closes
    // DNS rebinding. The static page is the one tokenless route.
    if !(request.method == Method::Get && path == "/")
        && !token_matches(&shared.token, token_of(&request))
    {
        let _ = respond(&stream, "404 Not Found", &[], b"");
        return;
    }
    if !host_ok {
        let _ = respond(&stream, "400 Bad Request", &[], b"");
        return;
    }
    match (request.method, path.as_str()) {
        (Method::Get, "/") => {
            let _ = respond(
                &stream,
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8")],
                shared.page.as_bytes(),
            );
        }
        (Method::Get, "/sessions") => {
            let sessions = shared.hub.sessions();
            let json = serde_json::to_string(
                &sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id, "label": s.label, "alive": s.alive, "busy": s.busy
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| "[]".into());
            let _ = respond(
                &stream,
                "200 OK",
                &[("Content-Type", "application/json")],
                json.as_bytes(),
            );
        }
        (Method::Get, _) if path.starts_with("/stream/") => {
            let id = path["/stream/".len()..].to_string();
            if shared.hub.revision(&id).is_none() {
                let _ = respond(&stream, "404 Not Found", &[], b"");
                return;
            }
            // CAS admission: concurrent connects must not overshoot the cap.
            let admitted = shared
                .sse
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < MAX_SSE).then_some(n + 1)
                })
                .is_ok();
            if !admitted {
                let _ = respond(&stream, "503 Service Unavailable", &[], b"");
                return;
            }
            serve_stream(shared, &stream, &id);
            shared.sse.fetch_sub(1, Ordering::AcqRel);
        }
        (Method::Post, "/spawn") => {
            let our_origin = format!("http://{}", shared.host);
            if let Some(origin) = request.header("origin") {
                if origin != our_origin {
                    let _ = respond(&stream, "403 Forbidden", &[], b"");
                    return;
                }
            }
            if request.header("content-type") != Some(INPUT_CONTENT_TYPE) {
                let _ = respond(&stream, "415 Unsupported Media Type", &[], b"");
                return;
            }
            if shared.hub.request_spawn() {
                let _ = respond(&stream, "202 Accepted", &[], b"");
            } else {
                let _ = respond(&stream, "429 Too Many Requests", &[], b"");
            }
        }
        (Method::Post, _) if path.starts_with("/input/") => {
            let our_origin = format!("http://{}", shared.host);
            if let Some(origin) = request.header("origin") {
                if origin != our_origin {
                    let _ = respond(&stream, "403 Forbidden", &[], b"");
                    return;
                }
            }
            if request.header("content-type") != Some(INPUT_CONTENT_TYPE) {
                let _ = respond(&stream, "415 Unsupported Media Type", &[], b"");
                return;
            }
            let id = path["/input/".len()..].to_string();
            let Some(message) = parse_body(&request.body) else {
                let _ = respond(&stream, "400 Bad Request", &[], b"");
                return;
            };
            let bytes = match message {
                InputMsg::Text(text) => text_bytes(&text, shared.hub.bracketed_paste(&id)),
                InputMsg::Key(key) => match symbolic_bytes(&key, shared.hub.app_cursor(&id)) {
                    Some(bytes) => bytes,
                    None => {
                        let _ = respond(&stream, "400 Bad Request", &[], b"");
                        return;
                    }
                },
            };
            match shared.hub.input_sender(&id) {
                None => {
                    let _ = respond(&stream, "404 Not Found", &[], b"");
                }
                Some((false, _)) => {
                    let _ = respond(&stream, "410 Gone", &[], b"");
                }
                Some((true, sender)) => {
                    if sender.send_bytes(bytes) {
                        let _ = respond(&stream, "204 No Content", &[], b"");
                    } else {
                        let _ = respond(&stream, "410 Gone", &[], b"");
                    }
                }
            }
        }
        _ => {
            let _ = respond(&stream, "404 Not Found", &[], b"");
        }
    }
}

fn serve_stream<S: InputSink>(shared: &Shared<S>, mut stream: &TcpStream, id: &str) {
    let _ = stream.set_write_timeout(Some(WRITE_DEADLINE));
    let mut head = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n",
    );
    for (name, value) in SECURITY_HEADERS {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut sent_revision: Option<u64> = None;
    let mut last_write = Instant::now();
    let mut last_event = Instant::now() - SSE_FLOOR; // first send is immediate
    loop {
        if shared.cancel.load(Ordering::Acquire) {
            return;
        }
        let Some(current) = shared.hub.revision(id) else {
            return; // session unregistered: end the stream
        };
        let fresh = sent_revision != Some(current);
        if fresh && last_event.elapsed() >= SSE_FLOOR {
            if let Some((revision, json)) = shared.hub.snapshot_json(id, shared.theme) {
                let frame = format!("data: {json}\n\n");
                if stream.write_all(frame.as_bytes()).is_err() {
                    return;
                }
                sent_revision = Some(revision);
                last_event = Instant::now();
                last_write = last_event;
            }
        } else if last_write.elapsed() >= SSE_HEARTBEAT {
            if stream.write_all(b":hb\n\n").is_err() {
                return;
            }
            last_write = Instant::now();
        }
        std::thread::sleep(SSE_POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::hub::CompanionHub;
    use std::io::Read;
    use std::sync::mpsc;

    impl InputSink for mpsc::Sender<Vec<u8>> {
        fn send_bytes(&self, bytes: Vec<u8>) -> bool {
            self.send(bytes).is_ok()
        }
    }

    type TestHub = CompanionHub<mpsc::Sender<Vec<u8>>>;

    const PAGE: &str = "<title>companion-test-page</title>";
    const TOKEN: &str = "cafebabecafebabecafebabecafebabe";

    fn theme() -> &'static Theme {
        crate::themes::default_theme()
    }

    fn boot(hub: Arc<TestHub>) -> ServerHandle {
        start(
            hub,
            theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: TOKEN.into(),
                page: PAGE,
            },
        )
        .expect("server starts on loopback")
    }

    fn host_of(handle: &ServerHandle) -> String {
        handle
            .url
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string()
    }

    /// One raw request; returns the full response (connection closes).
    fn roundtrip(host: &str, raw: &str) -> String {
        let mut stream = TcpStream::connect(host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        out
    }

    fn get(host: &str, target: &str) -> String {
        roundtrip(
            host,
            &format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        )
    }

    fn post_input(
        host: &str,
        id: &str,
        body: &str,
        content_type: &str,
        origin: Option<&str>,
    ) -> String {
        let origin_header = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        roundtrip(
            host,
            &format!(
                "POST /input/{id} HTTP/1.1\r\nHost: {host}\r\nX-Companion-Token: {TOKEN}\r\n{origin_header}Content-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn seeded_hub(app_cursor: bool) -> (Arc<TestHub>, mpsc::Receiver<Vec<u8>>) {
        let hub = Arc::new(TestHub::new());
        let (tx, rx) = mpsc::channel();
        hub.register("t1", "work", tx);
        hub.publish_snapshot("t1", Arc::new(seeded_snapshot(app_cursor)));
        (hub, rx)
    }

    fn seeded_snapshot(app_cursor: bool) -> crate::term_session::RenderableSnapshot {
        let mut snapshot = crate::term_session::RenderableSnapshot {
            cols: 5,
            lines: 1,
            rows: vec![vec![
                crate::term_session::SnapshotCell {
                    ch: 'h',
                    style: crate::term_session::CellStyle {
                        fg: crate::term_session::CellColor::Default,
                        bg: crate::term_session::CellColor::Default,
                        bold: false,
                        italic: false,
                        dim: false,
                        underline: false,
                        inverse: false,
                        hidden: false,
                    },
                    wide_spacer: false,
                };
                5
            ]],
            cursor: crate::term_session::SnapshotCursor {
                col: 0,
                row: Some(0),
                style: crate::term_session::CursorStyle::Block,
            },
            display_offset: 0,
            selection: Vec::new(),
            app_cursor_mode: app_cursor,
            bracketed_paste: false,
            mouse_tracking: false,
            alt_screen: false,
            focused_title: None,
            exited: None,
            selection_text: None,
            search_matches: Vec::new(),
        };
        snapshot.rows[0][1].ch = 'e';
        snapshot.rows[0][2].ch = 'l';
        snapshot.rows[0][3].ch = 'l';
        snapshot.rows[0][4].ch = 'o';
        snapshot
    }

    #[test]
    fn missing_or_wrong_token_is_404_and_correct_token_lists_sessions() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        assert!(get(&host, "/sessions").starts_with("HTTP/1.1 404"));
        assert!(get(&host, "/sessions?t=wrong").starts_with("HTTP/1.1 404"));
        let ok = get(&host, &format!("/sessions?t={TOKEN}"));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("\"label\":\"work\""));
        handle.stop();
    }

    #[test]
    fn page_is_served_without_token_with_security_headers() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        let page = get(&host, "/");
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(page.contains("companion-test-page"));
        assert!(page.contains("Referrer-Policy: no-referrer"));
        assert!(page.contains("Content-Security-Policy:"));
        handle.stop();
    }

    #[test]
    fn wrong_host_is_400() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        let response = roundtrip(
            &host,
            &format!("GET /sessions?t={TOKEN} HTTP/1.1\r\nHost: rebound.evil:80\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        handle.stop();
    }

    #[test]
    fn options_is_405() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        let response = roundtrip(
            &host,
            &format!("OPTIONS / HTTP/1.1\r\nHost: {host}\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 405"), "{response}");
        handle.stop();
    }

    #[test]
    fn input_requires_companion_content_type_and_same_origin() {
        let (hub, rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        // Safelisted content type (what a cross-origin form/fetch can send
        // without preflight) must be refused.
        let plain = post_input(&host, "t1", r#"{"key":"up"}"#, "text/plain", None);
        assert!(plain.starts_with("HTTP/1.1 415"), "{plain}");
        // Foreign origin refused even with the right content type.
        let foreign = post_input(
            &host,
            "t1",
            r#"{"key":"up"}"#,
            INPUT_CONTENT_TYPE,
            Some("http://evil.example"),
        );
        assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
        assert!(rx.try_recv().is_err(), "no input may have been delivered");
        handle.stop();
    }

    #[test]
    fn symbolic_input_honors_app_cursor_mode_and_text_passes_through() {
        let (hub, rx) = seeded_hub(false);
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let ok = post_input(&host, "t1", r#"{"key":"up"}"#, INPUT_CONTENT_TYPE, None);
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            vec![0x1b, b'[', b'A']
        );
        let ok = post_input(&host, "t1", r#"{"text":"ls\r"}"#, INPUT_CONTENT_TYPE, None);
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), b"ls\r");
        handle.stop();

        let (hub2, rx2) = seeded_hub(true);
        let handle2 = boot(hub2);
        let host2 = host_of(&handle2);
        let ok = post_input(&host2, "t1", r#"{"key":"up"}"#, INPUT_CONTENT_TYPE, None);
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(
            rx2.recv_timeout(Duration::from_secs(2)).unwrap(),
            vec![0x1b, b'O', b'A']
        );
        handle2.stop();
    }

    #[test]
    fn text_is_paste_bracketed_when_the_session_mode_is_on() {
        let (hub, rx) = seeded_hub(false);
        let mut snap = seeded_snapshot(false);
        snap.bracketed_paste = true;
        hub.publish_snapshot("t1", Arc::new(snap));
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let ok = post_input(&host, "t1", r#"{"text":"hi\r"}"#, INPUT_CONTENT_TYPE, None);
        assert!(ok.starts_with("HTTP/1.1 204"), "{ok}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"\x1b[200~hi\x1b[201~\r".to_vec()
        );
        handle.stop();
    }

    #[test]
    fn spawn_queues_with_guards_and_caps() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let spawn = |content_type: &str, origin: Option<&str>| {
            let origin_header = origin
                .map(|o| format!("Origin: {o}\r\n"))
                .unwrap_or_default();
            roundtrip(
                &host,
                &format!(
                    "POST /spawn HTTP/1.1\r\nHost: {host}\r\nX-Companion-Token: {TOKEN}\r\n{origin_header}Content-Type: {content_type}\r\nContent-Length: 2\r\n\r\n{{}}"
                ),
            )
        };
        // Guards mirror /input.
        assert!(spawn("text/plain", None).starts_with("HTTP/1.1 415"));
        assert!(spawn(INPUT_CONTENT_TYPE, Some("http://evil.example")).starts_with("HTTP/1.1 403"));
        let no_token = roundtrip(
            &host,
            &format!("POST /spawn HTTP/1.1\r\nHost: {host}\r\nContent-Type: {INPUT_CONTENT_TYPE}\r\nContent-Length: 2\r\n\r\n{{}}"),
        );
        assert!(no_token.starts_with("HTTP/1.1 404"));
        assert_eq!(hub.take_spawns(), 0, "no guard failure may queue a spawn");
        // Queue until the cap answers 429.
        for _ in 0..crate::companion::hub::MAX_PENDING_SPAWNS {
            assert!(spawn(INPUT_CONTENT_TYPE, None).starts_with("HTTP/1.1 202"));
        }
        assert!(spawn(INPUT_CONTENT_TYPE, None).starts_with("HTTP/1.1 429"));
        assert_eq!(hub.take_spawns(), crate::companion::hub::MAX_PENDING_SPAWNS);
        handle.stop();
    }

    #[test]
    fn dead_session_is_410_unknown_is_404() {
        let (hub, _rx) = seeded_hub(false);
        hub.set_meta("t1", "work", false, false);
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let dead = post_input(&host, "t1", r#"{"key":"up"}"#, INPUT_CONTENT_TYPE, None);
        assert!(dead.starts_with("HTTP/1.1 410"), "{dead}");
        let unknown = post_input(&host, "ghost", r#"{"key":"up"}"#, INPUT_CONTENT_TYPE, None);
        assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
        handle.stop();
    }

    fn open_sse(host: &str, id: &str) -> (TcpStream, BufReader<TcpStream>) {
        let stream = TcpStream::connect(host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        writer
            .write_all(
                format!("GET /stream/{id}?t={TOKEN} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
            )
            .unwrap();
        (stream, BufReader::new(writer.try_clone().unwrap()))
    }

    fn next_data_line(reader: &mut BufReader<TcpStream>) -> String {
        use std::io::BufRead;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("stream stays open");
            if line.starts_with("data:") {
                return line;
            }
        }
    }

    #[test]
    fn sse_sends_immediate_snapshot_then_updates_and_second_client_works() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let (_s1, mut reader1) = open_sse(&host, "t1");
        let first = next_data_line(&mut reader1);
        assert!(first.contains("hello"), "{first}");
        // Second client + a plain request while the first stream lives.
        let (_s2, mut reader2) = open_sse(&host, "t1");
        assert!(next_data_line(&mut reader2).contains("hello"));
        assert!(get(&host, &format!("/sessions?t={TOKEN}")).starts_with("HTTP/1.1 200"));
        // A new publish reaches the attached stream.
        let (fresh_hub, _keep) = seeded_hub(false); // borrow a second snapshot easily
        let _ = fresh_hub;
        let mut updated = crate::term_session::RenderableSnapshot {
            cols: 5,
            lines: 1,
            rows: vec![Vec::new()],
            cursor: crate::term_session::SnapshotCursor {
                col: 0,
                row: Some(0),
                style: crate::term_session::CursorStyle::Block,
            },
            display_offset: 0,
            selection: Vec::new(),
            app_cursor_mode: false,
            bracketed_paste: false,
            mouse_tracking: false,
            alt_screen: false,
            focused_title: None,
            exited: None,
            selection_text: None,
            search_matches: Vec::new(),
        };
        updated.rows[0] = "WORLD"
            .chars()
            .map(|ch| crate::term_session::SnapshotCell {
                ch,
                style: crate::term_session::CellStyle {
                    fg: crate::term_session::CellColor::Default,
                    bg: crate::term_session::CellColor::Default,
                    bold: false,
                    italic: false,
                    dim: false,
                    underline: false,
                    inverse: false,
                    hidden: false,
                },
                wide_spacer: false,
            })
            .collect();
        hub.publish_snapshot("t1", Arc::new(updated));
        assert!(next_data_line(&mut reader1).contains("WORLD"));
        handle.stop();
    }

    #[test]
    fn ninth_connection_gets_503() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        // Hold MAX_CONNS workers hostage with idle (unfinished) requests.
        let held: Vec<TcpStream> = (0..MAX_CONNS)
            .map(|_| {
                let mut s = TcpStream::connect(&host).unwrap();
                s.write_all(b"GET / HTTP/1.1\r\n").unwrap(); // never finished
                s
            })
            .collect();
        std::thread::sleep(Duration::from_millis(300)); // let workers spawn
        let response = get(&host, "/");
        assert!(response.starts_with("HTTP/1.1 503"), "{response}");
        drop(held);
        handle.stop();
    }

    #[test]
    fn stop_joins_with_attached_stream_within_deadline() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(hub);
        let host = host_of(&handle);
        let (_stream, mut reader) = open_sse(&host, "t1");
        assert!(next_data_line(&mut reader).contains("hello"));
        let started = Instant::now();
        handle.stop();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stop() must join promptly"
        );
    }

    #[test]
    fn stream_of_unregistered_session_ends() {
        let (hub, _rx) = seeded_hub(false);
        let handle = boot(Arc::clone(&hub));
        let host = host_of(&handle);
        let (stream, mut reader) = open_sse(&host, "t1");
        assert!(next_data_line(&mut reader).contains("hello"));
        hub.unregister("t1");
        // The stream must END (read returns 0) rather than hang.
        let mut rest = String::new();
        let mut raw = stream;
        raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let ended = std::io::Read::read_to_string(&mut raw, &mut rest).is_ok();
        assert!(ended, "stream should close after unregister");
        handle.stop();
    }
}
