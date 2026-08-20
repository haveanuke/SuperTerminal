//! Pure keyboard/mouse -> PTY byte translation.
//!
//! Port of the input handling in `src/renderer/xterm-registry.ts`:
//! the `attachCustomKeyEventHandler` block (SuperTerminal's custom Cmd/Alt
//! bindings) plus xterm.js's standard keydown translation, and the
//! click-to-move-cursor handler that synthesizes arrow-key presses.
//!
//! This module is intentionally free of gpui (or any other) dependencies so
//! it can be unit-tested in isolation. Callers describe the input with
//! [`KeyInput`] and write the returned bytes to the PTY.

/// A keyboard event, described with gpui keystroke naming: `"a"`, `"enter"`,
/// `"backspace"`, `"delete"`, `"left"`, `"right"`, `"up"`, `"down"`, `"tab"`,
/// `"escape"`, `"home"`, `"end"`, `"pageup"`, `"pagedown"`, `"f1"`..`"f12"`,
/// and single characters for printables.
pub struct KeyInput<'a> {
    pub key: &'a str,
    pub cmd: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// Translate a key event into the bytes to write to the PTY.
///
/// Returns `None` when the key should not reach the PTY (unhandled Cmd
/// combos are reserved for app shortcuts; unrecognized named keys are
/// ignored).
///
/// * `app_cursor` — DECCKM application cursor keys mode: arrows/home/end use
///   SS3 (`ESC O x`) instead of CSI (`ESC [ x`).
/// * `option_as_meta` — Alt+printable sends `ESC` + the character.
pub fn key_to_bytes(input: &KeyInput, app_cursor: bool, option_as_meta: bool) -> Option<Vec<u8>> {
    // SuperTerminal custom Cmd bindings (metaKey && !ctrlKey && !altKey in
    // the TS handler). Every other Cmd combo is reserved for app shortcuts.
    if input.cmd {
        if !input.ctrl && !input.alt {
            return match input.key {
                "backspace" => Some(vec![0x15]),    // NAK: kill line
                "left" => Some(vec![0x01]),         // SOH: line start
                "right" => Some(vec![0x05]),        // ENQ: line end
                "delete" => Some(vec![0x0b]),       // VT: kill to end of line
                "enter" => Some(vec![0x1b, b'\r']), // ESC CR
                _ => None,
            };
        }
        return None;
    }

    // SuperTerminal custom Alt bindings (altKey && !metaKey && !ctrlKey).
    if input.alt && !input.ctrl {
        match input.key {
            "backspace" => return Some(vec![0x17]), // ETB: kill word back
            "delete" => return Some(b"\x1bd".to_vec()), // ESC d: kill word fwd
            "left" => return Some(b"\x1bb".to_vec()), // ESC b: word back
            "right" => return Some(b"\x1bf".to_vec()), // ESC f: word fwd
            _ => {}
        }
    }

    // Standard named-key translation.
    match input.key {
        "enter" => return Some(vec![b'\r']),
        "tab" => return Some(vec![b'\t']),
        "backspace" => return Some(vec![0x7f]),
        "escape" => return Some(vec![0x1b]),
        "space" => return Some(vec![if input.ctrl { 0x00 } else { b' ' }]),
        "up" => return Some(cursor_seq(b'A', app_cursor)),
        "down" => return Some(cursor_seq(b'B', app_cursor)),
        "right" => return Some(cursor_seq(b'C', app_cursor)),
        "left" => return Some(cursor_seq(b'D', app_cursor)),
        "home" => return Some(cursor_seq(b'H', app_cursor)),
        "end" => return Some(cursor_seq(b'F', app_cursor)),
        "pageup" => return Some(b"\x1b[5~".to_vec()),
        "pagedown" => return Some(b"\x1b[6~".to_vec()),
        "delete" => return Some(b"\x1b[3~".to_vec()),
        "f1" => return Some(b"\x1bOP".to_vec()),
        "f2" => return Some(b"\x1bOQ".to_vec()),
        "f3" => return Some(b"\x1bOR".to_vec()),
        "f4" => return Some(b"\x1bOS".to_vec()),
        "f5" => return Some(b"\x1b[15~".to_vec()),
        "f6" => return Some(b"\x1b[17~".to_vec()),
        "f7" => return Some(b"\x1b[18~".to_vec()),
        "f8" => return Some(b"\x1b[19~".to_vec()),
        "f9" => return Some(b"\x1b[20~".to_vec()),
        "f10" => return Some(b"\x1b[21~".to_vec()),
        "f11" => return Some(b"\x1b[23~".to_vec()),
        "f12" => return Some(b"\x1b[24~".to_vec()),
        _ => {}
    }

    // Printable: exactly one char. Anything longer is an unrecognized named
    // key and is ignored.
    let mut chars = input.key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }

    // Ctrl+char control codes.
    if input.ctrl {
        if let Some(byte) = ctrl_byte(ch) {
            return Some(vec![byte]);
        }
        // Unmapped Ctrl combos (e.g. Ctrl+1) fall through and send the
        // plain character, matching xterm.
    }

    // Plain (or Option-as-Meta prefixed) printable character.
    let mut out = Vec::with_capacity(5);
    if input.alt && option_as_meta {
        out.push(0x1b);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    Some(out)
}

/// Port of the plain-click cursor-move handler: clicking within the line
/// being typed jumps the shell cursor there via synthesized arrow keys.
///
/// The caller is responsible for the environment guards from the TS handler
/// (no modifier keys, no selection, normal buffer, scrolled to bottom, no
/// mouse tracking). This function applies the geometric guards: the clicked
/// column is clamped to `cols - 1`, and `None` is returned when the click is
/// on a different row than the cursor or the column delta is zero.
pub fn click_to_move_bytes(
    clicked_col: usize,
    clicked_row: usize,
    cursor_col: usize,
    cursor_row: usize,
    cols: usize,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    if cols == 0 {
        return None;
    }
    let col = clicked_col.min(cols - 1);
    if clicked_row != cursor_row {
        return None; // same-line moves only
    }
    let (count, arrow): (usize, &[u8]) = if col > cursor_col {
        (
            col - cursor_col,
            if app_cursor { b"\x1bOC" } else { b"\x1b[C" },
        )
    } else if col < cursor_col {
        (
            cursor_col - col,
            if app_cursor { b"\x1bOD" } else { b"\x1b[D" },
        )
    } else {
        return None;
    };
    let mut out = Vec::with_capacity(count * arrow.len());
    for _ in 0..count {
        out.extend_from_slice(arrow);
    }
    Some(out)
}

/// Arrow/home/end sequence: CSI (`ESC [ x`) normally, SS3 (`ESC O x`) in
/// application cursor keys mode.
fn cursor_seq(final_byte: u8, app_cursor: bool) -> Vec<u8> {
    vec![0x1b, if app_cursor { b'O' } else { b'[' }, final_byte]
}

/// Ctrl+char -> C0 control byte.
fn ctrl_byte(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        '@' | ' ' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> KeyInput<'_> {
        KeyInput {
            key: k,
            cmd: false,
            alt: false,
            ctrl: false,
            shift: false,
        }
    }

    fn cmd(k: &str) -> KeyInput<'_> {
        KeyInput {
            cmd: true,
            ..key(k)
        }
    }

    fn alt(k: &str) -> KeyInput<'_> {
        KeyInput {
            alt: true,
            ..key(k)
        }
    }

    fn ctrl(k: &str) -> KeyInput<'_> {
        KeyInput {
            ctrl: true,
            ..key(k)
        }
    }

    fn bytes(input: &KeyInput, app_cursor: bool, option_as_meta: bool) -> Option<Vec<u8>> {
        key_to_bytes(input, app_cursor, option_as_meta)
    }

    #[test]
    fn custom_cmd_bindings() {
        let cases: &[(&str, &[u8])] = &[
            ("backspace", &[0x15]),
            ("left", &[0x01]),
            ("right", &[0x05]),
            ("delete", &[0x0b]),
            ("enter", &[0x1b, b'\r']),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&cmd(k), false, false).as_deref(),
                Some(*expected),
                "cmd+{k}"
            );
        }
    }

    #[test]
    fn other_cmd_combos_are_reserved() {
        for k in ["a", "c", "k", "t", "w", "up", "down", "tab", "home"] {
            assert_eq!(bytes(&cmd(k), false, false), None, "cmd+{k}");
        }
        // Cmd combined with Ctrl or Alt never reaches the PTY either.
        let mut i = cmd("backspace");
        i.ctrl = true;
        assert_eq!(bytes(&i, false, false), None);
        let mut i = cmd("left");
        i.alt = true;
        assert_eq!(bytes(&i, false, false), None);
    }

    #[test]
    fn custom_alt_bindings() {
        let cases: &[(&str, &[u8])] = &[
            ("backspace", &[0x17]),
            ("delete", b"\x1bd"),
            ("left", b"\x1bb"),
            ("right", b"\x1bf"),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&alt(k), false, false).as_deref(),
                Some(*expected),
                "alt+{k}"
            );
            // The bindings hold regardless of option_as_meta and app_cursor.
            assert_eq!(
                bytes(&alt(k), true, true).as_deref(),
                Some(*expected),
                "alt+{k} (app_cursor, option_as_meta)"
            );
        }
    }

    #[test]
    fn ctrl_letter_math() {
        let cases: &[(&str, u8)] = &[
            ("a", 0x01),
            ("c", 0x03),
            ("d", 0x04),
            ("h", 0x08),
            ("i", 0x09),
            ("m", 0x0d),
            ("r", 0x12),
            ("z", 0x1a),
            ("A", 0x01),
            ("Z", 0x1a),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&ctrl(k), false, false).as_deref(),
                Some(&[*expected][..]),
                "ctrl+{k}"
            );
        }
    }

    #[test]
    fn ctrl_symbol_variants() {
        let cases: &[(&str, u8)] = &[
            ("@", 0x00),
            ("space", 0x00),
            ("[", 0x1b),
            ("\\", 0x1c),
            ("]", 0x1d),
            ("^", 0x1e),
            ("_", 0x1f),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&ctrl(k), false, false).as_deref(),
                Some(&[*expected][..]),
                "ctrl+{k}"
            );
        }
        // Unmapped Ctrl combos send the plain character, like xterm.
        assert_eq!(bytes(&ctrl("1"), false, false).as_deref(), Some(&b"1"[..]));
    }

    #[test]
    fn printables_as_utf8() {
        assert_eq!(bytes(&key("a"), false, false).as_deref(), Some(&b"a"[..]));
        assert_eq!(bytes(&key("Z"), false, false).as_deref(), Some(&b"Z"[..]));
        assert_eq!(
            bytes(&key("space"), false, false).as_deref(),
            Some(&b" "[..])
        );
        assert_eq!(
            bytes(&key("\u{e9}"), false, false).as_deref(),
            Some("\u{e9}".as_bytes())
        );
    }

    #[test]
    fn option_as_meta_toggle() {
        // On: Alt+printable sends ESC + char.
        assert_eq!(
            bytes(&alt("a"), false, true).as_deref(),
            Some(&b"\x1ba"[..])
        );
        assert_eq!(
            bytes(&alt("\u{e9}"), false, true).as_deref(),
            Some(b"\x1b\xc3\xa9".as_slice())
        );
        // Off: the (already composed) character passes through unprefixed.
        assert_eq!(bytes(&alt("a"), false, false).as_deref(), Some(&b"a"[..]));
    }

    #[test]
    fn simple_named_keys() {
        let cases: &[(&str, &[u8])] = &[
            ("enter", b"\r"),
            ("tab", b"\t"),
            ("backspace", &[0x7f]),
            ("escape", &[0x1b]),
            ("pageup", b"\x1b[5~"),
            ("pagedown", b"\x1b[6~"),
            ("delete", b"\x1b[3~"),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&key(k), false, false).as_deref(),
                Some(*expected),
                "{k}"
            );
            // These are unaffected by app_cursor mode.
            assert_eq!(
                bytes(&key(k), true, false).as_deref(),
                Some(*expected),
                "{k}"
            );
        }
    }

    #[test]
    fn arrows_home_end_both_cursor_modes() {
        let cases: &[(&str, &[u8], &[u8])] = &[
            ("up", b"\x1b[A", b"\x1bOA"),
            ("down", b"\x1b[B", b"\x1bOB"),
            ("right", b"\x1b[C", b"\x1bOC"),
            ("left", b"\x1b[D", b"\x1bOD"),
            ("home", b"\x1b[H", b"\x1bOH"),
            ("end", b"\x1b[F", b"\x1bOF"),
        ];
        for (k, normal, app) in cases {
            assert_eq!(
                bytes(&key(k), false, false).as_deref(),
                Some(*normal),
                "{k} normal"
            );
            assert_eq!(
                bytes(&key(k), true, false).as_deref(),
                Some(*app),
                "{k} app"
            );
        }
    }

    #[test]
    fn function_keys() {
        let cases: &[(&str, &[u8])] = &[
            ("f1", b"\x1bOP"),
            ("f2", b"\x1bOQ"),
            ("f3", b"\x1bOR"),
            ("f4", b"\x1bOS"),
            ("f5", b"\x1b[15~"),
            ("f6", b"\x1b[17~"),
            ("f7", b"\x1b[18~"),
            ("f8", b"\x1b[19~"),
            ("f9", b"\x1b[20~"),
            ("f10", b"\x1b[21~"),
            ("f11", b"\x1b[23~"),
            ("f12", b"\x1b[24~"),
        ];
        for (k, expected) in cases {
            assert_eq!(
                bytes(&key(k), false, false).as_deref(),
                Some(*expected),
                "{k}"
            );
        }
    }

    #[test]
    fn unknown_named_key_is_ignored() {
        assert_eq!(bytes(&key("insert"), false, false), None);
    }

    #[test]
    fn click_move_right() {
        // Cursor at col 5, click col 9, same row: 4x right arrow.
        assert_eq!(
            click_to_move_bytes(9, 2, 5, 2, 80, false).as_deref(),
            Some(b"\x1b[C\x1b[C\x1b[C\x1b[C".as_slice())
        );
        assert_eq!(
            click_to_move_bytes(9, 2, 5, 2, 80, true).as_deref(),
            Some(b"\x1bOC\x1bOC\x1bOC\x1bOC".as_slice())
        );
    }

    #[test]
    fn click_move_left() {
        // Cursor at col 5, click col 2, same row: 3x left arrow.
        assert_eq!(
            click_to_move_bytes(2, 0, 5, 0, 80, false).as_deref(),
            Some(b"\x1b[D\x1b[D\x1b[D".as_slice())
        );
        assert_eq!(
            click_to_move_bytes(2, 0, 5, 0, 80, true).as_deref(),
            Some(b"\x1bOD\x1bOD\x1bOD".as_slice())
        );
    }

    #[test]
    fn click_other_row_is_none() {
        assert_eq!(click_to_move_bytes(9, 3, 5, 2, 80, false), None);
    }

    #[test]
    fn click_zero_delta_is_none() {
        assert_eq!(click_to_move_bytes(5, 2, 5, 2, 80, false), None);
    }

    #[test]
    fn click_col_is_clamped() {
        // Click past the right edge clamps to cols - 1 (79): 79 - 76 = 3 moves.
        assert_eq!(
            click_to_move_bytes(200, 4, 76, 4, 80, false).as_deref(),
            Some(b"\x1b[C\x1b[C\x1b[C".as_slice())
        );
        // Clamping can also collapse the delta to zero.
        assert_eq!(click_to_move_bytes(200, 4, 79, 4, 80, false), None);
    }

    #[test]
    fn click_zero_cols_is_none() {
        assert_eq!(click_to_move_bytes(0, 0, 0, 0, 0, false), None);
    }
}
