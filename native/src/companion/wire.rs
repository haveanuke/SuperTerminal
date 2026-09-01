//! Wire format for the phone page: rows of style-coalesced runs with
//! RESOLVED RGB colors and explicit grid positions.
//!
//! Resolution happens host-side BEFORE merging (theme palette, inverse, dim,
//! hidden), so cells that differ only in those attributes can never merge
//! incorrectly. Selection and search highlighting are local UI state and
//! deliberately never reach the wire. Explicit `col`/`width` per run keep the
//! phone's font metrics from drifting the grid — the page positions runs, it
//! never flows text.

use serde::{Deserialize, Serialize};

use crate::term_session::{CellColor, CellStyle, CursorStyle, RenderableSnapshot};
use crate::themes::{ansi_256, Theme};

/// `Deserialize` is derived alongside `Serialize` on all three types here so
/// the peer client can parse exactly what this server emits, from a single
/// definition: a field added for the server is automatically understood by
/// the client, and a rename breaks compilation instead of silently
/// producing empty frames on the peer side.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireSnapshot {
    pub cols: u16,
    pub lines: u16,
    pub cursor: Option<WireCursor>,
    pub app_cursor: bool,
    pub rows: Vec<Vec<WireRun>>,
    /// Scrollback tail rendered above the live rows, oldest first. Empty
    /// (and omitted from the JSON) when there is no history yet.
    /// `#[serde(default)]` so a client parsing an emission that omitted it
    /// (because it was empty) sees an empty Vec rather than a parse error.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub history: Vec<Vec<WireRun>>,
    // The two mode booleans stay required (no `#[serde(default)]`): they
    // are always serialized, so a missing field means the peer is running a
    // build that predates it — defaulting to `false` would silently
    // mis-handle paste/mouse mode rather than failing loudly.
    pub bracketed_paste: bool,
    pub mouse_tracking: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireCursor {
    pub col: u16,
    pub row: u16,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WireRun {
    pub col: u16,
    pub width: u16,
    pub text: String,
    /// "#rrggbb"
    pub fg: String,
    /// "#rrggbb"; None = page background shows through.
    pub bg: Option<String>,
    pub b: bool,
    pub i: bool,
    pub u: bool,
}

fn resolve_color(color: CellColor, theme: &Theme) -> Option<u32> {
    match color {
        CellColor::Default => None,
        CellColor::Indexed(index) => Some(ansi_256(index, theme)),
        CellColor::Rgb(r, g, b) => Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32),
    }
}

fn halve_channels(rgb: u32) -> u32 {
    (((rgb >> 16) & 0xff) / 2) << 16 | (((rgb >> 8) & 0xff) / 2) << 8 | ((rgb & 0xff) / 2)
}

/// Resolved visual identity of one cell: concrete fg, optional bg, and the
/// attributes that survive to the wire. Cells merge iff these are equal.
#[derive(PartialEq, Clone, Copy)]
struct Resolved {
    fg: u32,
    bg: Option<u32>,
    bold: bool,
    italic: bool,
    underline: bool,
}

fn resolve(style: &CellStyle, theme: &Theme) -> (Resolved, bool) {
    let mut fg = resolve_color(style.fg, theme).unwrap_or(theme.foreground);
    let mut bg = resolve_color(style.bg, theme);
    // Inverse BEFORE dim, matching the native renderer — a dim+inverse cell
    // must dim the swapped foreground, not the original one.
    if style.inverse {
        let old_fg = fg;
        fg = bg.unwrap_or(theme.background);
        bg = Some(old_fg);
    }
    if style.dim {
        fg = halve_channels(fg);
    }
    let mut blank = false;
    if style.hidden {
        fg = bg.unwrap_or(theme.background);
        blank = true;
    }
    (
        Resolved {
            fg,
            bg,
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
        },
        blank,
    )
}

fn hex(v: u32) -> String {
    format!("#{v:06x}")
}

fn row_runs(row: &[crate::term_session::SnapshotCell], theme: &Theme) -> Vec<WireRun> {
    let mut runs: Vec<WireRun> = Vec::new();
    let mut current: Option<(Resolved, u16, String, u16)> = None; // (style, col, text, width)
    for (col_index, cell) in row.iter().enumerate() {
        if cell.wide_spacer {
            // The spacer extends the previous glyph's footprint.
            if let Some((_, _, _, width)) = current.as_mut() {
                *width += 1;
            }
            continue;
        }
        let (resolved, blank) = resolve(&cell.style, theme);
        // NUL cells render as spaces, exactly like the native renderer.
        let ch = if blank || cell.ch == '\0' {
            ' '
        } else {
            cell.ch
        };
        match current.as_mut() {
            Some((style, _, text, width)) if *style == resolved => {
                text.push(ch);
                *width += 1;
            }
            _ => {
                if let Some(run) = current.take() {
                    runs.push(finish_run(run));
                }
                current = Some((resolved, col_index as u16, ch.to_string(), 1));
            }
        }
    }
    if let Some(run) = current.take() {
        runs.push(finish_run(run));
    }
    runs
}

pub fn serialize_snapshot(snapshot: &RenderableSnapshot, theme: &Theme) -> WireSnapshot {
    let rows = snapshot
        .rows
        .iter()
        .map(|row| row_runs(row, theme))
        .collect();
    let history = snapshot
        .history_rows
        .iter()
        .map(|row| row_runs(row, theme))
        .collect();
    let cursor = match (&snapshot.cursor.row, snapshot.cursor.style) {
        (Some(row), style) if style != CursorStyle::Hidden => Some(WireCursor {
            col: snapshot.cursor.col as u16,
            row: *row as u16,
        }),
        _ => None,
    };
    WireSnapshot {
        cols: snapshot.cols as u16,
        lines: snapshot.lines as u16,
        cursor,
        app_cursor: snapshot.app_cursor_mode,
        rows,
        history,
        bracketed_paste: snapshot.bracketed_paste,
        mouse_tracking: snapshot.mouse_tracking,
    }
}

fn finish_run((style, col, text, width): (Resolved, u16, String, u16)) -> WireRun {
    WireRun {
        col,
        width,
        text,
        fg: hex(style.fg),
        bg: style.bg.map(hex),
        b: style.bold,
        i: style.italic,
        u: style.underline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term_session::{SnapshotCell, SnapshotCursor};

    fn style() -> CellStyle {
        CellStyle {
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            inverse: false,
            hidden: false,
        }
    }

    fn cell(ch: char, style: CellStyle) -> SnapshotCell {
        SnapshotCell {
            ch,
            style,
            wide_spacer: false,
        }
    }

    fn snap(rows: Vec<Vec<SnapshotCell>>) -> RenderableSnapshot {
        let cols = rows.first().map(|r| r.len()).unwrap_or(0);
        RenderableSnapshot {
            cols,
            lines: rows.len(),
            rows,
            cursor: SnapshotCursor {
                col: 0,
                row: Some(0),
                style: CursorStyle::Block,
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
            history_rows: Vec::new(),
        }
    }

    fn theme() -> &'static Theme {
        crate::themes::default_theme()
    }

    fn hex(v: u32) -> String {
        format!("#{v:06x}")
    }

    #[test]
    fn history_rows_ride_a_separate_history_field() {
        let mut s = snap(vec![vec![cell('l', style()), cell('i', style())]]);
        s.history_rows = vec![vec![cell('h', style()), cell('i', style())]];
        let wire = serialize_snapshot(&s, theme());
        assert_eq!(wire.history.len(), 1);
        assert_eq!(wire.history[0][0].text, "hi");
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"history\""), "{json}");
    }

    #[test]
    fn same_style_neighbors_merge_into_one_run() {
        let s = snap(vec![vec![
            cell('a', style()),
            cell('b', style()),
            cell('c', style()),
        ]]);
        let wire = serialize_snapshot(&s, theme());
        assert_eq!(wire.rows.len(), 1);
        assert_eq!(wire.rows[0].len(), 1);
        let run = &wire.rows[0][0];
        assert_eq!((run.col, run.width, run.text.as_str()), (0, 3, "abc"));
        assert_eq!(run.fg, hex(theme().foreground));
        assert_eq!(run.bg, None);
    }

    #[test]
    fn inverse_swaps_resolved_colors_before_merge() {
        let mut inv = style();
        inv.inverse = true;
        let s = snap(vec![vec![cell('a', style()), cell('b', inv)]]);
        let wire = serialize_snapshot(&s, theme());
        let runs = &wire.rows[0];
        assert_eq!(runs.len(), 2, "inverse cell must not merge with plain");
        assert_eq!(runs[1].fg, hex(theme().background));
        assert_eq!(runs[1].bg, Some(hex(theme().foreground)));
    }

    #[test]
    fn dim_darkens_and_hidden_blanks() {
        let mut dim = style();
        dim.dim = true;
        let mut hidden = style();
        hidden.hidden = true;
        let s = snap(vec![vec![cell('x', dim), cell('y', hidden)]]);
        let wire = serialize_snapshot(&s, theme());
        let runs = &wire.rows[0];
        let f = theme().foreground;
        let halved = ((f >> 16 & 0xff) / 2) << 16 | ((f >> 8 & 0xff) / 2) << 8 | ((f & 0xff) / 2);
        assert_eq!(runs[0].fg, hex(halved));
        // Hidden: blank text, fg equals resolved background.
        assert_eq!(runs[1].text, " ");
        assert_eq!(runs[1].fg, hex(theme().background));
    }

    #[test]
    fn dim_plus_inverse_dims_the_swapped_foreground() {
        // Native order (pane.rs): inverse first, THEN dim — the dim applies
        // to the swapped foreground (old background), not the original.
        let mut both = style();
        both.inverse = true;
        both.dim = true;
        let s = snap(vec![vec![cell('x', both)]]);
        let wire = serialize_snapshot(&s, theme());
        let run = &wire.rows[0][0];
        let f = theme().background;
        let halved = ((f >> 16 & 0xff) / 2) << 16 | ((f >> 8 & 0xff) / 2) << 8 | ((f & 0xff) / 2);
        assert_eq!(run.fg, hex(halved));
        assert_eq!(run.bg, Some(hex(theme().foreground)));
    }

    #[test]
    fn nul_cells_render_as_spaces() {
        let s = snap(vec![vec![cell('\0', style()), cell('b', style())]]);
        let wire = serialize_snapshot(&s, theme());
        assert_eq!(wire.rows[0][0].text, " b");
    }

    #[test]
    fn wide_glyph_spans_two_columns_spacer_skipped() {
        let mut spacer = cell(' ', style());
        spacer.wide_spacer = true;
        let mut bolded = style();
        bolded.bold = true;
        let s = snap(vec![vec![cell('世', style()), spacer, cell('z', bolded)]]);
        let wire = serialize_snapshot(&s, theme());
        let runs = &wire.rows[0];
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].col, runs[0].width, runs[0].text.as_str()),
            (0, 2, "世")
        );
        assert_eq!((runs[1].col, runs[1].width), (2, 1));
        assert!(runs[1].b);
    }

    #[test]
    fn default_bg_serializes_as_none_and_rgb_as_hex() {
        let mut red_on_blue = style();
        red_on_blue.fg = CellColor::Rgb(255, 0, 0);
        red_on_blue.bg = CellColor::Rgb(0, 0, 255);
        let s = snap(vec![vec![cell('r', red_on_blue)]]);
        let wire = serialize_snapshot(&s, theme());
        let run = &wire.rows[0][0];
        assert_eq!(run.fg, "#ff0000");
        assert_eq!(run.bg, Some("#0000ff".to_string()));
    }

    #[test]
    fn cursor_out_of_viewport_or_hidden_is_none() {
        let mut s = snap(vec![vec![cell('a', style())]]);
        s.cursor.row = None;
        assert_eq!(serialize_snapshot(&s, theme()).cursor, None);
        let mut s2 = snap(vec![vec![cell('a', style())]]);
        s2.cursor.style = CursorStyle::Hidden;
        assert_eq!(serialize_snapshot(&s2, theme()).cursor, None);
        let s3 = snap(vec![vec![cell('a', style())]]);
        assert_eq!(
            serialize_snapshot(&s3, theme()).cursor,
            Some(WireCursor { col: 0, row: 0 })
        );
    }

    #[test]
    fn selection_and_search_are_not_on_the_wire() {
        let plain = snap(vec![vec![cell('a', style()), cell('b', style())]]);
        let mut selected = snap(vec![vec![cell('a', style()), cell('b', style())]]);
        selected.selection = vec![(0, 0), (1, 0)];
        selected.search_matches = vec![(1, 0)];
        assert_eq!(
            serialize_snapshot(&plain, theme()),
            serialize_snapshot(&selected, theme())
        );
    }

    #[test]
    fn indexed_colors_resolve_through_the_theme_palette() {
        let mut red = style();
        red.fg = CellColor::Indexed(1);
        let s = snap(vec![vec![cell('e', red)]]);
        let wire = serialize_snapshot(&s, theme());
        assert_eq!(wire.rows[0][0].fg, hex(ansi_256(1, theme())));
    }

    #[test]
    fn the_snapshot_carries_paste_and_mouse_modes() {
        let mut snapshot = snap(vec![vec![cell('x', style())]]);
        snapshot.bracketed_paste = true;
        snapshot.mouse_tracking = true;
        let wire = serialize_snapshot(&snapshot, theme());
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"bracketedPaste\":true"), "{json}");
        assert!(json.contains("\"mouseTracking\":true"), "{json}");
    }

    #[test]
    fn both_modes_are_always_present_even_when_false() {
        // A missing field and a false field must not be ambiguous to a
        // client deciding whether to wrap a paste.
        let snapshot = snap(vec![vec![cell('x', style())]]);
        let wire = serialize_snapshot(&snapshot, theme());
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"bracketedPaste\":false"), "{json}");
        assert!(json.contains("\"mouseTracking\":false"), "{json}");
    }

    #[test]
    fn a_snapshot_survives_a_round_trip() {
        // The client parses exactly what the server emits. Deriving both
        // directions on one struct is what stops them drifting.
        let mut snapshot = snap(vec![vec![cell('x', style())]]);
        snapshot.bracketed_paste = true;
        let wire = serialize_snapshot(&snapshot, theme());
        let json = serde_json::to_string(&wire).unwrap();
        let parsed: WireSnapshot =
            serde_json::from_str(&json).expect("server output must parse as WireSnapshot");
        assert_eq!(parsed.cols, 1);
        assert_eq!(parsed.lines, 1);
        assert!(parsed.bracketed_paste);
        assert_eq!(parsed.rows.len(), 1);
    }

    #[test]
    fn an_absent_history_field_parses_as_empty() {
        // `history` is skipped when empty, so the client must tolerate it
        // being absent rather than treating that as malformed.
        let json = r#"{"cols":1,"lines":1,"cursor":null,"appCursor":false,"rows":[[]],"bracketedPaste":false,"mouseTracking":false}"#;
        let parsed: WireSnapshot = serde_json::from_str(json).expect("must parse");
        assert!(parsed.history.is_empty());
    }

    #[test]
    fn the_two_modes_are_not_transposed() {
        // Both-true and both-false fixtures cannot catch a swap: identical
        // values serialize identically either way. One of each pins it.
        let mut snapshot = snap(vec![vec![cell('x', style())]]);
        snapshot.bracketed_paste = true;
        snapshot.mouse_tracking = false;
        let wire = serialize_snapshot(&snapshot, theme());
        let json = serde_json::to_string(&wire).unwrap();
        assert!(json.contains("\"bracketedPaste\":true"), "{json}");
        assert!(json.contains("\"mouseTracking\":false"), "{json}");
    }
}
