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
