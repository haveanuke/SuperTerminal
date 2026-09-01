//! Phone input: literal text or symbolic keys, translated host-side so the
//! page never needs to know escape sequences — and arrows honor the
//! session's application-cursor mode.

use serde::Deserialize;

#[derive(Debug, PartialEq)]
pub enum InputMsg {
    Text(String),
    Key(String),
}

#[derive(Deserialize)]
struct RawBody {
    text: Option<String>,
    key: Option<String>,
}

/// Accept exactly one of {"text": …} / {"key": …}.
pub fn parse_body(body: &[u8]) -> Option<InputMsg> {
    let raw: RawBody = serde_json::from_slice(body).ok()?;
    match (raw.text, raw.key) {
        (Some(text), None) => Some(InputMsg::Text(text)),
        (None, Some(key)) => Some(InputMsg::Key(key)),
        _ => None,
    }
}

/// Literal text for the pty. When the session's foreground app enabled
/// bracketed paste (DECSET 2004), frame the body in ESC[200~/201~ so
/// paste-aware TUIs treat it as inserted text instead of guessing from
/// read-chunk timing — while a trailing \r (the page's Send appends one to
/// submit) stays OUTSIDE the frame, arriving as a real Enter keypress.
/// ESC bytes are stripped from the framed body (as real terminals do when
/// bracketing pastes): the protocol has no escaping, so an embedded
/// ESC[201~ would otherwise terminate the frame early and turn the rest
/// into live keystrokes. Mode off: bytes pass through untouched.
pub fn text_bytes(text: &str, bracketed_paste: bool) -> Vec<u8> {
    if !bracketed_paste {
        return text.as_bytes().to_vec();
    }
    let (body, submit) = match text.strip_suffix('\r') {
        Some(body) => (body, &b"\r"[..]),
        None => (text, &b""[..]),
    };
    let mut bytes = Vec::with_capacity(body.len() + 12 + submit.len());
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend(body.bytes().filter(|&b| b != 0x1b));
    bytes.extend_from_slice(b"\x1b[201~");
    bytes.extend_from_slice(submit);
    bytes
}

/// Rename body: `{"label": …}` — edge whitespace (including stray
/// newlines from pastes) is trimmed; control characters INSIDE the name
/// are rejected (escape-sequence smuggling; a mangled name is worse than
/// an error); truncated to 64 chars. None = 400.
pub fn parse_rename(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct RenameBody {
        label: Option<String>,
    }
    let raw: RenameBody = serde_json::from_slice(body).ok()?;
    let label = raw.label?;
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return None;
    }
    Some(trimmed.chars().take(64).collect())
}

/// A peer's raw-byte payload: `{"bytes": [u8]}`. Unlike the phone's
/// symbolic vocabulary, a peer already ran `keys.rs::key_to_bytes` — the
/// same encoder a local pane uses — so these bytes are opaque PTY input,
/// not something this server interprets. The only job here is refusing
/// anything that is not cleanly an array of bytes: a missing field, a
/// non-array, or an out-of-range/non-integer element is rejected outright
/// rather than coerced (clamped, truncated, or best-effort parsed).
#[derive(Deserialize)]
struct PeerBody {
    bytes: Option<Vec<u8>>,
}

pub fn parse_peer_bytes(body: &[u8]) -> Option<Vec<u8>> {
    let raw: PeerBody = serde_json::from_slice(body).ok()?;
    raw.bytes
}

pub fn symbolic_bytes(key: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |ch: u8| {
        if app_cursor {
            vec![0x1b, b'O', ch]
        } else {
            vec![0x1b, b'[', ch]
        }
    };
    Some(match key {
        "enter" => vec![b'\r'],
        "ctrl-c" => vec![0x03],
        // Kill-line: the phone's only way to take back text that landed in
        // an agent's compose box and is sitting there unsent.
        "ctrl-u" => vec![0x15],
        "tab" => vec![b'\t'],
        "esc" => vec![0x1b],
        "up" => arrow(b'A'),
        "down" => arrow(b'B'),
        "right" => arrow(b'C'),
        "left" => arrow(b'D'),
        "y" => vec![b'y'],
        "n" => vec![b'n'],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_follow_cursor_mode() {
        assert_eq!(symbolic_bytes("up", false), Some(vec![0x1b, b'[', b'A']));
        assert_eq!(symbolic_bytes("up", true), Some(vec![0x1b, b'O', b'A']));
        assert_eq!(symbolic_bytes("left", false), Some(vec![0x1b, b'[', b'D']));
        assert_eq!(symbolic_bytes("left", true), Some(vec![0x1b, b'O', b'D']));
    }

    #[test]
    fn control_keys_translate() {
        assert_eq!(symbolic_bytes("enter", false), Some(vec![b'\r']));
        assert_eq!(symbolic_bytes("ctrl-c", false), Some(vec![0x03]));
        assert_eq!(symbolic_bytes("tab", false), Some(vec![b'\t']));
        assert_eq!(symbolic_bytes("esc", false), Some(vec![0x1b]));
    }

    #[test]
    fn kill_line_clears_a_typed_but_unsent_message() {
        // Phone-sent text can land in an agent's compose box and sit there;
        // without a kill-line the phone has no way to take it back.
        assert_eq!(symbolic_bytes("ctrl-u", false), Some(vec![0x15]));
        assert_eq!(symbolic_bytes("ctrl-u", true), Some(vec![0x15]));
    }

    #[test]
    fn unknown_key_is_none() {
        assert_eq!(symbolic_bytes("ctrl-alt-del", false), None);
    }

    #[test]
    fn text_passes_through_without_bracketed_paste() {
        assert_eq!(text_bytes("ls\r", false), b"ls\r");
    }

    #[test]
    fn text_wraps_in_paste_markers_with_enter_outside() {
        assert_eq!(text_bytes("hi\r", true), b"\x1b[200~hi\x1b[201~\r".to_vec());
    }

    #[test]
    fn interior_newlines_stay_inside_the_paste() {
        assert_eq!(
            text_bytes("a\nb\r", true),
            b"\x1b[200~a\nb\x1b[201~\r".to_vec()
        );
    }

    #[test]
    fn text_without_trailing_enter_is_fully_wrapped() {
        assert_eq!(text_bytes("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
    }

    #[test]
    fn embedded_frame_terminator_cannot_break_out() {
        // ESC is stripped from the framed body, so an embedded end-of-paste
        // sequence survives only as inert literal text.
        assert_eq!(
            text_bytes("a\x1b[201~evil\r", true),
            b"\x1b[200~a[201~evil\x1b[201~\r".to_vec()
        );
    }

    #[test]
    fn rename_body_parses_trims_and_rejects_junk() {
        assert_eq!(
            parse_rename(br#"{"label":" build "}"#),
            Some("build".into())
        );
        assert_eq!(parse_rename(br#"{"label":"   "}"#), None);
        assert_eq!(parse_rename(br#"{}"#), None);
        assert_eq!(parse_rename(b"not json"), None);
        // Interior control characters (escape-sequence smuggling) are
        // rejected, not stripped — a mangled name is worse than an error.
        assert_eq!(parse_rename(b"{\"label\":\"a\\u001b[31mred\"}"), None);
        assert_eq!(parse_rename(b"{\"label\":\"a\\tb\"}"), None);
        // Edge whitespace (stray paste newlines) trims away harmlessly.
        assert_eq!(
            parse_rename(b"{\"label\":\"build\\n\"}"),
            Some("build".into())
        );
        // Over-long names truncate to 64 chars.
        let long = format!("{{\"label\":\"{}\"}}", "x".repeat(90));
        assert_eq!(parse_rename(long.as_bytes()).unwrap().chars().count(), 64);
    }

    #[test]
    fn peer_bytes_parses_a_clean_array() {
        assert_eq!(
            parse_peer_bytes(br#"{"bytes":[27,91,65]}"#),
            Some(vec![0x1b, b'[', b'A'])
        );
        assert_eq!(parse_peer_bytes(br#"{"bytes":[]}"#), Some(vec![]));
    }

    #[test]
    fn peer_bytes_rejects_a_missing_field() {
        assert_eq!(parse_peer_bytes(br#"{}"#), None);
    }

    #[test]
    fn peer_bytes_rejects_a_non_array() {
        assert_eq!(parse_peer_bytes(br#"{"bytes":"hi"}"#), None);
        assert_eq!(parse_peer_bytes(br#"{"bytes":42}"#), None);
    }

    #[test]
    fn peer_bytes_rejects_out_of_range_or_non_integer_elements() {
        // 256 does not fit a byte, and neither does a negative number or a
        // fraction — none of these are clamped, they are refused.
        assert_eq!(parse_peer_bytes(br#"{"bytes":[256]}"#), None);
        assert_eq!(parse_peer_bytes(br#"{"bytes":[-1]}"#), None);
        assert_eq!(parse_peer_bytes(br#"{"bytes":[1.5]}"#), None);
    }

    #[test]
    fn peer_bytes_rejects_malformed_json() {
        assert_eq!(parse_peer_bytes(b"not json"), None);
    }

    #[test]
    fn body_accepts_exactly_one_form() {
        assert_eq!(
            parse_body(br#"{"text":"ls\r"}"#),
            Some(InputMsg::Text("ls\r".into()))
        );
        assert_eq!(
            parse_body(br#"{"key":"up"}"#),
            Some(InputMsg::Key("up".into()))
        );
        assert_eq!(parse_body(br#"{}"#), None);
        assert_eq!(parse_body(br#"{"text":"a","key":"up"}"#), None);
        assert_eq!(parse_body(b"not json"), None);
    }
}
