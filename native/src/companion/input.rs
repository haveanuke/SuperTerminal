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
