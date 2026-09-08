//! Per-terminal scrollback on disk: one file per terminal id, in a
//! `scrollback/` directory beside `projects.json` (see `settings::settings_dir`).
//!
//! Storage only — this module has no gpui, no `Workspace` and no `TerminalPane`.
//! It turns a [`RenderableSnapshot`] into a file and a file back into rows of
//! [`SnapshotCell`], so a reopened project can show what its terminals said
//! (`docs/superpowers/specs/2026-09-02-scrollback-restore-design.md`).
//!
//! Three decisions the spec makes, kept here rather than at the call sites:
//!
//! * **Colours are stored UNRESOLVED.** [`CellColor`] as-is, never the theme's
//!   hex: resolving at save time would freeze the old palette into the file, so
//!   a restored pane would keep painting last month's theme.
//! * **The row cap is [`HISTORY_TAIL`]**, reused rather than reinvented — it is
//!   already this codebase's "a few screens", and a second scrollback limit
//!   would be a second thing to reason about.
//! * **A missing, corrupt, oversized or foreign-version file loads NOTHING**,
//!   the same discipline as `settings.rs` and `projects.json`: losing scrollback
//!   must never be worse than not having the feature.
//!
//! One file per terminal, NOT a field in `projects.json`: the project list is
//! read at every launch, and inlining tens of kilobytes of grid per terminal
//! would make listing projects pay for text nobody has asked to see yet.

// The save/load/reap entry points are called by the tab-close and restore
// wiring that lands next; nothing outside this module calls them yet, and
// the tests drive the `*_in` halves so they never touch the real store.
#![allow(dead_code)]

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::term_session::{CellColor, CellStyle, RenderableSnapshot, SnapshotCell, HISTORY_TAIL};

/// On-disk format. A file that does not say exactly this loads nothing:
/// a build reading a shape it does not know must show an empty pane, never
/// a misread one.
const FORMAT_VERSION: u32 = 1;

/// Widest grid stored or restored. A hostile file can otherwise ask for
/// `usize::MAX` columns and be answered with an allocation that kills the
/// app; a real terminal is an order of magnitude narrower than this.
const MAX_COLS: usize = 2000;

/// Biggest file read or written. [`HISTORY_TAIL`] rows of [`MAX_COLS`] cells
/// that share no styling is the only way to approach it, which real terminal
/// output does not do — but a file that has, gets ignored rather than parsed.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Longest file name minted for a terminal id (macOS allows 255 bytes; ids
/// are `term-N`). An id that does not fit is refused rather than truncated:
/// truncation would let two ids collide on one file.
const MAX_NAME_BYTES: usize = 128;

/// How old a leftover `.tmp` must be before the reaper takes it. A write in
/// flight is milliseconds old; anything this stale is a crashed one.
const TMP_STALE: Duration = Duration::from_secs(3600);

const ATTR_BOLD: u8 = 1 << 0;
const ATTR_ITALIC: u8 = 1 << 1;
const ATTR_DIM: u8 = 1 << 2;
const ATTR_UNDERLINE: u8 = 1 << 3;
const ATTR_INVERSE: u8 = 1 << 4;
const ATTR_HIDDEN: u8 = 1 << 5;

/// `scrollback/`, beside `projects.json` and `settings.json`.
///
/// A directory of its own rather than loose files in the app-support dir, so
/// [`reap_in`] can only ever delete something this module wrote.
pub fn scrollback_dir() -> PathBuf {
    crate::settings::settings_dir().join("scrollback")
}

/// The file a terminal id maps to, or `None` when the id cannot be named.
///
/// The mapping is escaped and injective: every byte outside `[A-Za-z0-9_-]`
/// becomes `%xx`, `%` included. So an id can carry `/`, `..` or a NUL and
/// still land inside `dir` (see the tests), and two different ids can never
/// arrive at one file.
pub fn path_for(dir: &Path, id: &str) -> Option<PathBuf> {
    Some(dir.join(file_name(id)?))
}

fn file_name(id: &str) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    let mut name = String::with_capacity(id.len() + 5);
    for byte in id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => name.push(byte as char),
            _ => name.push_str(&format!("%{byte:02x}")),
        }
    }
    name.push_str(".json");
    (name.len() <= MAX_NAME_BYTES).then_some(name)
}

/// What a terminal said, ready to paint: rows oldest-first, each exactly
/// `cols` cells wide, at most [`HISTORY_TAIL`] of them.
#[derive(Debug, Clone)]
pub struct RestoredScrollback {
    pub cols: usize,
    pub rows: Vec<Vec<SnapshotCell>>,
}

/// The file itself: a version, the grid width, and the rows as
/// style-coalesced runs. Short keys because this is machine-read and its
/// size is the reason it is not inside `projects.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedScrollback {
    version: u32,
    cols: usize,
    rows: Vec<Vec<SavedRun>>,
}

/// A run of adjacent cells sharing everything but their characters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedRun {
    /// One character per grid column the run covers.
    t: String,
    /// Foreground, UNRESOLVED. Absent = [`CellColor::Default`].
    #[serde(skip_serializing_if = "Option::is_none")]
    fg: Option<SavedColor>,
    /// Background, UNRESOLVED. Absent = [`CellColor::Default`].
    #[serde(skip_serializing_if = "Option::is_none")]
    bg: Option<SavedColor>,
    /// Attribute bits: bold, italic, dim, underline, inverse, hidden.
    #[serde(skip_serializing_if = "is_zero")]
    a: u8,
    /// True when these cells are the spacer after a wide glyph. Part of the
    /// run's identity, not a side note: without it a restored CJK line
    /// shifts left by one column per wide character.
    #[serde(skip_serializing_if = "is_false")]
    w: bool,
}

/// A colour as the terminal meant it: a palette index, or raw channels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum SavedColor {
    Indexed(u8),
    Rgb([u8; 3]),
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn to_saved(color: CellColor) -> Option<SavedColor> {
    match color {
        CellColor::Default => None,
        CellColor::Indexed(index) => Some(SavedColor::Indexed(index)),
        CellColor::Rgb(r, g, b) => Some(SavedColor::Rgb([r, g, b])),
    }
}

fn from_saved(color: Option<SavedColor>) -> CellColor {
    match color {
        None => CellColor::Default,
        Some(SavedColor::Indexed(index)) => CellColor::Indexed(index),
        Some(SavedColor::Rgb([r, g, b])) => CellColor::Rgb(r, g, b),
    }
}

fn attrs_of(style: &CellStyle) -> u8 {
    let mut attrs = 0;
    for (set, bit) in [
        (style.bold, ATTR_BOLD),
        (style.italic, ATTR_ITALIC),
        (style.dim, ATTR_DIM),
        (style.underline, ATTR_UNDERLINE),
        (style.inverse, ATTR_INVERSE),
        (style.hidden, ATTR_HIDDEN),
    ] {
        if set {
            attrs |= bit;
        }
    }
    attrs
}

/// A cell that says nothing: an unstyled space. Trimmed from the end of a
/// row on the way out and padded back on the way in, which is what keeps a
/// mostly-empty 200-column grid measured in bytes rather than kilobytes.
/// A space with a background colour or an attribute is NOT blank — it is
/// visible — so only a wholly default cell qualifies.
fn is_blank(cell: &SnapshotCell) -> bool {
    !cell.wide_spacer
        && (cell.ch == ' ' || cell.ch == '\0')
        && cell.style.fg == CellColor::Default
        && cell.style.bg == CellColor::Default
        && attrs_of(&cell.style) == 0
}

fn blank_cell() -> SnapshotCell {
    SnapshotCell {
        ch: ' ',
        style: CellStyle {
            fg: CellColor::Default,
            bg: CellColor::Default,
            bold: false,
            italic: false,
            dim: false,
            underline: false,
            inverse: false,
            hidden: false,
        },
        wide_spacer: false,
    }
}

/// Save one terminal's text under [`scrollback_dir`].
pub fn save(id: &str, snapshot: &RenderableSnapshot) -> io::Result<()> {
    save_in(&scrollback_dir(), id, snapshot)
}

/// Save into `dir` (the injectable half of [`save`]).
///
/// A terminal that said nothing writes no file at all, and clears any file
/// it had: there is nothing to restore, and an empty file would only be
/// something for the reaper to find later.
pub fn save_in(dir: &Path, id: &str, snapshot: &RenderableSnapshot) -> io::Result<()> {
    let path = path_for(dir, id)
        .ok_or_else(|| io::Error::other(format!("terminal id cannot name a file: {id:?}")))?;
    let saved = encode(snapshot);
    if saved.rows.is_empty() {
        return remove_if_present(&path);
    }
    let json = serde_json::to_string(&saved).map_err(io::Error::other)?;
    if json.len() as u64 > MAX_FILE_BYTES {
        // Writing it would only produce a file that can never be loaded,
        // and a stale one left beside it would restore the wrong text.
        remove_if_present(&path)?;
        return Err(io::Error::other("scrollback is too large to store"));
    }
    std::fs::create_dir_all(dir)?;
    // Atomic write (tmp + rename), same discipline as `settings.rs` and
    // `projects.rs`. The pid keeps two instances saving at once apart.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Load one terminal's text from under [`scrollback_dir`].
pub fn load(id: &str) -> Option<RestoredScrollback> {
    load_in(&scrollback_dir(), id)
}

/// Load from `dir` (the injectable half of [`load`]). `None` for every
/// unhappy case there is — no file, unreadable, too big, not JSON, the
/// wrong shape, another format version — because the caller's answer to all
/// of them is the same: open an empty pane.
pub fn load_in(dir: &Path, id: &str) -> Option<RestoredScrollback> {
    let text = read_capped(&path_for(dir, id)?)?;
    decode(serde_json::from_str(&text).ok()?)
}

/// Read a file only if it is small enough to be one of ours. The length is
/// checked twice — the metadata before reading, the bytes after — so a file
/// that grows between the two still cannot be read past the cap.
fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_FILE_BYTES).then_some(text)
}

fn encode(snapshot: &RenderableSnapshot) -> SavedScrollback {
    // Scrollback first, live screen after: oldest at the top, which is the
    // order a pane paints them in.
    let mut rows: Vec<&Vec<SnapshotCell>> = snapshot
        .history_rows
        .iter()
        .chain(snapshot.rows.iter())
        .collect();
    if rows.len() > HISTORY_TAIL {
        rows.drain(..rows.len() - HISTORY_TAIL);
    }
    // Trailing blank rows are the absence of output, not output. Keeping
    // them would restore a screenful of nothing under the last line.
    while rows.last().is_some_and(|row| row.iter().all(is_blank)) {
        rows.pop();
    }
    let widest = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    let cols = snapshot.cols.max(widest).min(MAX_COLS);
    SavedScrollback {
        version: FORMAT_VERSION,
        cols,
        rows: rows.iter().map(|row| encode_row(row, cols)).collect(),
    }
}

fn encode_row(row: &[SnapshotCell], cols: usize) -> Vec<SavedRun> {
    let mut end = row.len().min(cols);
    while end > 0 && is_blank(&row[end - 1]) {
        end -= 1;
    }
    let mut runs: Vec<SavedRun> = Vec::new();
    for cell in &row[..end] {
        let fg = to_saved(cell.style.fg);
        let bg = to_saved(cell.style.bg);
        let a = attrs_of(&cell.style);
        let w = cell.wide_spacer;
        // A NUL cell is a space, exactly as both renderers draw it.
        let ch = if cell.ch == '\0' { ' ' } else { cell.ch };
        match runs.last_mut() {
            Some(run) if run.fg == fg && run.bg == bg && run.a == a && run.w == w => run.t.push(ch),
            _ => runs.push(SavedRun {
                t: ch.to_string(),
                fg,
                bg,
                a,
                w,
            }),
        }
    }
    runs
}

fn decode(saved: SavedScrollback) -> Option<RestoredScrollback> {
    if saved.version != FORMAT_VERSION {
        return None;
    }
    if saved.cols == 0 || saved.cols > MAX_COLS {
        return None;
    }
    let mut rows = saved.rows;
    if rows.len() > HISTORY_TAIL {
        rows.drain(..rows.len() - HISTORY_TAIL);
    }
    Some(RestoredScrollback {
        cols: saved.cols,
        rows: rows
            .iter()
            .map(|runs| decode_row(runs, saved.cols))
            .collect(),
    })
}

fn decode_row(runs: &[SavedRun], cols: usize) -> Vec<SnapshotCell> {
    let mut cells: Vec<SnapshotCell> = Vec::with_capacity(cols);
    'row: for run in runs {
        let style = CellStyle {
            fg: from_saved(run.fg),
            bg: from_saved(run.bg),
            bold: run.a & ATTR_BOLD != 0,
            italic: run.a & ATTR_ITALIC != 0,
            dim: run.a & ATTR_DIM != 0,
            underline: run.a & ATTR_UNDERLINE != 0,
            inverse: run.a & ATTR_INVERSE != 0,
            hidden: run.a & ATTR_HIDDEN != 0,
        };
        for ch in run.t.chars() {
            if cells.len() == cols {
                break 'row;
            }
            cells.push(SnapshotCell {
                ch,
                style,
                wide_spacer: run.w,
            });
        }
    }
    cells.resize_with(cols, blank_cell);
    cells
}

/// Delete every stored terminal whose id is no longer referenced, plus any
/// temp file a crashed write left behind. Returns how many files went.
pub fn reap(referenced: &HashSet<String>) -> usize {
    reap_in(&scrollback_dir(), referenced)
}

/// The injectable half of [`reap`]. Touches only files this module could
/// have written: a `.json` whose name is not one the referenced ids mint,
/// and a `.json.tmp.` older than [`TMP_STALE`] (a write in flight is
/// milliseconds old, and may belong to another running instance).
pub fn reap_in(dir: &Path, referenced: &HashSet<String>) -> usize {
    let keep: HashSet<String> = referenced.iter().filter_map(|id| file_name(id)).collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let doomed = if name.ends_with(".json") {
            !keep.contains(name)
        } else if name.contains(".json.tmp.") {
            is_stale(&entry)
        } else {
            false
        };
        if doomed && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn is_stale(entry: &std::fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > TMP_STALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term_session::{CellColor, CellStyle, CursorStyle, SnapshotCursor, HISTORY_TAIL};
    use std::time::{Duration, SystemTime};

    /// A fresh, empty directory of this test's own.
    fn dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("st-scrollback-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plain() -> CellStyle {
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

    fn spacer() -> SnapshotCell {
        SnapshotCell {
            ch: ' ',
            style: plain(),
            wide_spacer: true,
        }
    }

    fn text_row(text: &str, cols: usize) -> Vec<SnapshotCell> {
        let mut row: Vec<SnapshotCell> = text.chars().map(|ch| cell(ch, plain())).collect();
        while row.len() < cols {
            row.push(cell(' ', plain()));
        }
        row
    }

    fn snapshot(cols: usize, rows: Vec<Vec<SnapshotCell>>) -> RenderableSnapshot {
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

    /// One cell, in a form `assert_eq!` can print: char, both UNRESOLVED
    /// colours, the attribute bits, and whether it is a wide-glyph spacer.
    type CellPrint = (char, CellColor, CellColor, u8, bool);

    /// Everything a restored row must carry, cell by cell.
    fn fingerprint(rows: &[Vec<SnapshotCell>]) -> Vec<Vec<CellPrint>> {
        rows.iter()
            .map(|row| {
                row.iter()
                    .map(|c| {
                        let s = &c.style;
                        let attrs = (s.bold as u8)
                            | (s.italic as u8) << 1
                            | (s.dim as u8) << 2
                            | (s.underline as u8) << 3
                            | (s.inverse as u8) << 4
                            | (s.hidden as u8) << 5;
                        (c.ch, s.fg, s.bg, attrs, c.wide_spacer)
                    })
                    .collect()
            })
            .collect()
    }

    fn read_file(dir: &Path, id: &str) -> String {
        let path = path_for(dir, id).expect("a usable id has a path");
        std::fs::read_to_string(path).expect("saved file")
    }

    #[test]
    fn a_round_trip_preserves_text_styling_and_colours() {
        let dir = dir("round");
        let loud = CellStyle {
            fg: CellColor::Indexed(4),
            bg: CellColor::Rgb(10, 20, 30),
            bold: true,
            italic: true,
            dim: true,
            underline: true,
            inverse: true,
            hidden: true,
        };
        let rows = vec![
            vec![
                cell('h', plain()),
                cell('i', loud),
                cell('!', plain()),
                cell(' ', plain()),
            ],
            vec![
                cell('漢', plain()),
                spacer(),
                cell('x', loud),
                cell(' ', plain()),
            ],
        ];
        let snap = snapshot(4, rows.clone());
        save_in(&dir, "term-1", &snap).unwrap();

        let restored = load_in(&dir, "term-1").expect("saved scrollback loads back");
        assert_eq!(restored.cols, 4);
        assert_eq!(fingerprint(&restored.rows), fingerprint(&rows));
    }

    #[test]
    fn colours_are_stored_unresolved_never_as_theme_hex() {
        let dir = dir("unresolved");
        let styled = CellStyle {
            fg: CellColor::Indexed(4),
            bg: CellColor::Rgb(10, 20, 30),
            ..plain()
        };
        save_in(&dir, "term-1", &snapshot(1, vec![vec![cell('x', styled)]])).unwrap();

        let text = read_file(&dir, "term-1");
        assert!(
            !text.contains('#'),
            "a theme-resolved hex colour must never reach the file: {text}"
        );
        assert!(
            text.contains('4'),
            "the palette INDEX is what is stored: {text}"
        );
        assert!(
            text.contains("10") && text.contains("20") && text.contains("30"),
            "an rgb colour keeps its channels: {text}"
        );
    }

    #[test]
    fn the_row_cap_is_the_history_tail_and_the_newest_rows_win() {
        let dir = dir("cap");
        let rows: Vec<Vec<SnapshotCell>> =
            (0..500).map(|i| text_row(&format!("row{i}"), 10)).collect();
        save_in(&dir, "term-1", &snapshot(10, rows)).unwrap();

        let stored: serde_json::Value = serde_json::from_str(&read_file(&dir, "term-1")).unwrap();
        assert_eq!(
            stored["rows"].as_array().map(|rows| rows.len()),
            Some(HISTORY_TAIL),
            "the cap is enforced on SAVE, not only on the way back"
        );
        let restored = load_in(&dir, "term-1").expect("loads");
        assert_eq!(restored.rows.len(), HISTORY_TAIL);
        assert_eq!(
            HISTORY_TAIL, 150,
            "the cap is term_session's, not a second one"
        );
        let first: String = restored.rows[0].iter().map(|c| c.ch).collect();
        assert_eq!(first.trim_end(), "row350", "the TAIL is what is kept");
        let last: String = restored.rows[HISTORY_TAIL - 1]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(last.trim_end(), "row499");
    }

    #[test]
    fn history_rows_are_saved_ahead_of_the_live_screen() {
        let dir = dir("history");
        let mut snap = snapshot(10, vec![text_row("live", 10)]);
        snap.history_rows = vec![text_row("older", 10), text_row("newer", 10)];
        save_in(&dir, "term-1", &snap).unwrap();

        let restored = load_in(&dir, "term-1").expect("loads");
        let text: Vec<String> = restored
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        assert_eq!(text, vec!["older", "newer", "live"]);
    }

    #[test]
    fn a_file_for_an_unknown_id_loads_nothing() {
        let dir = dir("unknown");
        save_in(&dir, "term-1", &snapshot(4, vec![text_row("hi", 4)])).unwrap();
        assert!(load_in(&dir, "term-2").is_none());
        assert!(
            load_in(&dir, "term-1").is_some(),
            "the saved id still loads"
        );
    }

    #[test]
    fn a_corrupt_file_loads_nothing() {
        let dir = dir("corrupt");
        let path = path_for(&dir, "term-1").expect("path");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_in(&dir, "term-1").is_none());
    }

    #[test]
    fn an_oversized_file_loads_nothing() {
        let dir = dir("oversized");
        let path = path_for(&dir, "term-1").expect("path");
        // Valid JSON of exactly our shape, just far too big to be real.
        let huge = "x".repeat(5 * 1024 * 1024);
        let text = format!(r#"{{"version":1,"cols":10,"rows":[[{{"t":"{huge}"}}]]}}"#);
        std::fs::write(&path, text).unwrap();
        assert!(load_in(&dir, "term-1").is_none());
    }

    #[test]
    fn a_file_from_another_format_version_loads_nothing() {
        let dir = dir("version");
        let path = path_for(&dir, "term-1").expect("path");
        std::fs::write(&path, r#"{"version":99,"cols":4,"rows":[[{"t":"hi"}]]}"#).unwrap();
        assert!(load_in(&dir, "term-1").is_none());
        std::fs::write(&path, r#"{"cols":4,"rows":[[{"t":"hi"}]]}"#).unwrap();
        assert!(load_in(&dir, "term-1").is_none(), "an unversioned file too");
    }

    #[test]
    fn a_hostile_file_can_neither_panic_nor_allocate_unboundedly() {
        let dir = dir("hostile");
        let path = path_for(&dir, "term-1").expect("path");
        let hostile = [
            // Wrong types throughout.
            r#"{"version":1,"cols":"wide","rows":"nope"}"#.to_string(),
            r#"{"version":1,"cols":10,"rows":[[{"t":42,"fg":"blue"}]]}"#.to_string(),
            r#"{"version":1,"cols":10,"rows":[[{"t":"a","fg":[1,2]}]]}"#.to_string(),
            // Truncated mid-object.
            r#"{"version":1,"cols":10,"rows":[[{"t":"ab""#.to_string(),
            // A column count that would allocate the machine to death.
            r#"{"version":1,"cols":18446744073709551615,"rows":[[{"t":"a"}]]}"#.to_string(),
            r#"{"version":1,"cols":-1,"rows":[[{"t":"a"}]]}"#.to_string(),
            // A row count far past the cap, in a tiny file.
            format!(
                r#"{{"version":1,"cols":10,"rows":[{}]}}"#,
                vec!["[]"; 100_000].join(",")
            ),
            // Deep nesting: a stack overflow would take the app with it.
            format!(
                r#"{{"version":1,"cols":10,"rows":{}{}}}"#,
                "[".repeat(10_000),
                "]".repeat(10_000)
            ),
        ];
        for text in hostile {
            std::fs::write(&path, &text).unwrap();
            let loaded = load_in(&dir, "term-1");
            let rows = loaded.map(|r| r.rows.len()).unwrap_or(0);
            assert!(
                rows <= HISTORY_TAIL,
                "a hostile file must never restore more than the cap: {rows}"
            );
        }
    }

    #[test]
    fn the_reaper_deletes_exactly_the_unreferenced_files() {
        let dir = dir("reap");
        for id in ["term-1", "term-2", "term-3"] {
            save_in(&dir, id, &snapshot(4, vec![text_row("hi", 4)])).unwrap();
        }
        let referenced: HashSet<String> = ["term-1".to_string(), "term-3".to_string()].into();
        assert_eq!(reap_in(&dir, &referenced), 1);
        assert!(load_in(&dir, "term-1").is_some());
        assert!(
            load_in(&dir, "term-2").is_none(),
            "the unreferenced file is gone"
        );
        assert!(load_in(&dir, "term-3").is_some());
        assert!(
            path_for(&dir, "term-2").is_some_and(|p| !p.exists()),
            "gone from disk, not just unreadable"
        );
        assert_eq!(
            reap_in(&dir, &referenced),
            0,
            "a second pass has nothing to do"
        );
    }

    #[test]
    fn the_reaper_clears_a_stale_temp_file_but_not_a_write_in_flight() {
        let dir = dir("reap_tmp");
        let fresh = dir.join("term-9.json.tmp.4242");
        let stale = dir.join("term-8.json.tmp.4243");
        std::fs::write(&fresh, "{}").unwrap();
        std::fs::write(&stale, "{}").unwrap();
        let old = SystemTime::now() - Duration::from_secs(24 * 3600);
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        assert_eq!(reap_in(&dir, &HashSet::new()), 1);
        assert!(fresh.exists(), "another process may be mid-rename");
        assert!(
            !stale.exists(),
            "a crashed write leaves nothing behind forever"
        );
    }

    #[test]
    fn an_id_can_never_escape_the_scrollback_directory() {
        let root = dir("escape");
        let dir = root.join("store");
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["../escaped", "..", "/etc/passwd", "term/../../x", "a b?c*"] {
            save_in(&dir, id, &snapshot(4, vec![text_row("hi", 4)])).unwrap();
            let path = path_for(&dir, id).expect("every id gets a name inside the dir");
            assert_eq!(
                path.parent(),
                Some(dir.as_path()),
                "{id} escaped to {path:?}"
            );
            assert!(
                load_in(&dir, id).is_some(),
                "{id} must still round trip through its encoded name"
            );
        }
        let outside: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "store")
            .collect();
        assert!(
            outside.is_empty(),
            "an id wrote outside its own directory: {outside:?}"
        );
    }

    #[test]
    fn distinct_ids_can_never_share_a_file() {
        let dir = dir("distinct");
        save_in(&dir, "a/b", &snapshot(8, vec![text_row("first", 8)])).unwrap();
        save_in(&dir, "a%2fb", &snapshot(8, vec![text_row("second", 8)])).unwrap();
        let first: String = load_in(&dir, "a/b").expect("first").rows[0]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(first.trim_end(), "first", "unclobbered by the other id");
        let second: String = load_in(&dir, "a%2fb").expect("second").rows[0]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(second.trim_end(), "second");
    }

    #[test]
    fn trailing_blank_cells_are_not_stored_but_come_back() {
        let dir = dir("trailing");
        save_in(&dir, "term-1", &snapshot(200, vec![text_row("hi", 200)])).unwrap();

        let text = read_file(&dir, "term-1");
        assert!(
            text.len() < 200,
            "198 blank cells must not reach the file ({} bytes)",
            text.len()
        );
        let restored = load_in(&dir, "term-1").expect("loads");
        assert_eq!(restored.rows[0].len(), 200, "the row comes back full width");
        let line: String = restored.rows[0].iter().map(|c| c.ch).collect();
        assert_eq!(line, format!("hi{}", " ".repeat(198)));
    }

    #[test]
    fn a_terminal_that_said_nothing_leaves_no_file() {
        let dir = dir("empty");
        let blank = snapshot(80, vec![text_row("", 80); 24]);
        save_in(&dir, "term-1", &blank).unwrap();
        assert!(
            path_for(&dir, "term-1").is_some_and(|p| !p.exists()),
            "an empty terminal writes nothing"
        );
        assert!(load_in(&dir, "term-1").is_none());
    }

    #[test]
    fn a_later_save_replaces_the_earlier_one_and_leaves_no_temp_file() {
        let dir = dir("replace");
        save_in(&dir, "term-1", &snapshot(6, vec![text_row("first", 6)])).unwrap();
        save_in(&dir, "term-1", &snapshot(6, vec![text_row("second", 6)])).unwrap();
        let line: String = load_in(&dir, "term-1").expect("loads").rows[0]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(line.trim_end(), "second");
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1, "no .tmp left behind: {names:?}");
    }

    #[test]
    fn the_store_lives_beside_the_project_store() {
        let dir = scrollback_dir();
        assert_eq!(
            dir.parent(),
            crate::projects::projects_path().parent(),
            "scrollback sits under the same app-support directory"
        );
    }

    #[test]
    fn saving_creates_the_directory_it_needs() {
        let dir = dir("mkdir").join("nested");
        save_in(&dir, "term-1", &snapshot(4, vec![text_row("hi", 4)])).unwrap();
        assert!(load_in(&dir, "term-1").is_some());
    }
}
