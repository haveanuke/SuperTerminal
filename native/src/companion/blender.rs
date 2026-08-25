//! Live Blender viewport bridge (preview-gallery phase 2): a single poller
//! thread that, ONLY while the phone is watching the gallery and the
//! setting is on, asks the blender-mcp addon (JSON over localhost TCP) for
//! an offscreen viewport capture and publishes the frame into the preview
//! store. HTTP workers never drive Blender — the spec's seam, verbatim.
//! Blender absent or busy: the tile simply stays absent; nothing retries
//! hot (one attempt per interval).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

use super::previews::PreviewStore;

/// The blender-mcp addon's default listener.
pub const BLENDER_ADDR: &str = "127.0.0.1:9876";
/// One capture attempt per interval while wanted.
pub const CAPTURE_INTERVAL: Duration = Duration::from_secs(2);
/// Longest edge of the captured frame (phone tile + full view).
const CAPTURE_MAX_SIZE: u32 = 800;
/// Socket + response budget per attempt; a wedged Blender costs one tick.
const IO_TIMEOUT: Duration = Duration::from_millis(2500);
/// Never slurp an unbounded capture file.
const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;

/// Spawn the poller; it holds only a Weak store handle and exits within one
/// interval of the companion server (and its store) shutting down — the
/// same lifecycle pattern as the thumbnail worker.
pub fn spawn(store: Weak<PreviewStore>, scratch_dir: PathBuf) {
    let _ = std::thread::Builder::new()
        .name("companion-blender".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&scratch_dir);
            let frame_path = scratch_dir.join(format!("st-viewport-{}.png", std::process::id()));
            loop {
                let Some(store) = store.upgrade() else { break };
                if store.viewport_wanted() {
                    if let Some(bytes) = capture_once(BLENDER_ADDR, &frame_path, IO_TIMEOUT) {
                        store.set_viewport_frame(bytes);
                    }
                }
                drop(store);
                std::thread::sleep(CAPTURE_INTERVAL);
            }
            let _ = std::fs::remove_file(&frame_path);
        });
}

/// One capture round-trip against the addon protocol (verified live):
/// send `{"type":"get_viewport_screenshot","params":{...}}`, read the JSON
/// reply, then read the PNG the addon wrote at our requested path. None on
/// any failure or timeout — the caller just tries again next interval.
pub(crate) fn capture_once(addr: &str, frame_path: &Path, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Write;
    let mut stream = std::net::TcpStream::connect_timeout(&addr.parse().ok()?, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    let command = serde_json::json!({
        "type": "get_viewport_screenshot",
        "params": {
            "max_size": CAPTURE_MAX_SIZE,
            "filepath": frame_path.to_string_lossy(),
            "format": "png",
        },
    })
    .to_string();
    stream.write_all(command.as_bytes()).ok()?;
    // The addon replies with one JSON object; read until it parses (bounded).
    let mut reply = Vec::new();
    let mut buf = [0u8; 4096];
    let ok = loop {
        let read = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break false,
            Ok(read) => read,
        };
        reply.extend_from_slice(&buf[..read]);
        if reply.len() > 64 * 1024 {
            break false;
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&reply) {
            break value.get("status").and_then(|s| s.as_str()) == Some("success");
        }
    };
    if !ok {
        return None;
    }
    let meta = std::fs::metadata(frame_path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_FRAME_BYTES {
        let _ = std::fs::remove_file(frame_path);
        return None;
    }
    let bytes = std::fs::read(frame_path).ok()?;
    let _ = std::fs::remove_file(frame_path);
    // Only genuine PNG frames reach the store (the route serves image/png).
    bytes
        .starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
        .then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// Stub addon: accepts one connection, parses the request, writes a
    /// tiny PNG at the requested filepath, replies success.
    fn stub_addon(respond: bool) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let Ok(read) = stream.read(&mut buf) else {
                return;
            };
            if !respond {
                // Wedged Blender: hold the socket open, never answer.
                std::thread::sleep(std::time::Duration::from_secs(5));
                return;
            }
            let request: serde_json::Value = serde_json::from_slice(&buf[..read]).unwrap();
            let path = request["params"]["filepath"].as_str().unwrap().to_string();
            let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
            png.extend_from_slice(b"stub-frame");
            std::fs::write(&path, png).unwrap();
            let _ = stream.write_all(br#"{"status": "success", "result": {"success": true}}"#);
        });
        addr
    }

    fn scratch_frame(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("st-blender-{}-{name}.png", std::process::id()))
    }

    #[test]
    fn capture_round_trips_through_the_addon_protocol() {
        let addr = stub_addon(true);
        let frame = scratch_frame("ok");
        let bytes = capture_once(&addr, &frame, Duration::from_secs(3)).expect("capture");
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(bytes.ends_with(b"stub-frame"));
        assert!(!frame.exists(), "scratch frame is cleaned up");
    }

    #[test]
    fn wedged_blender_costs_one_bounded_timeout() {
        let addr = stub_addon(false);
        let frame = scratch_frame("wedged");
        let started = std::time::Instant::now();
        assert!(capture_once(&addr, &frame, Duration::from_millis(300)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "must give up within the io timeout, not hang"
        );
    }

    #[test]
    fn refused_connection_is_a_quiet_none() {
        let frame = scratch_frame("refused");
        // Reserved-but-closed port: bind then drop.
        let addr = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().to_string()
        };
        assert!(capture_once(&addr, &frame, Duration::from_millis(300)).is_none());
    }
}
