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

/// The key one terminal's scrollback is stored under.
///
/// **Terminal ids cannot be used and this is the whole reason this
/// function exists.** `Workspace::fresh_id` mints `term-{n}` from a counter
/// that restarts at 1 every launch, so today's `term-1` and last session's
/// `term-1` are different terminals wearing one name — a restore keyed on
/// that would hand a pane ANOTHER terminal's output and present it as its
/// own. That is worse than restoring nothing, so the key is built from the
/// two things that do survive a quit:
///
/// * **the project's `anchor`** — chosen at first capture and never moved
///   after (`projects::Project::anchor`), which is also what
///   `ProjectStore::record` matches a capture to its record by; and
/// * **the terminal's index in the project's `dirs`**, which is the order
///   `open_project` spawns them in and the order `TabPaneDirs::dirs`
///   captures them in.
///
/// Hashed rather than spelled out, so the key is short and safe whatever
/// the path holds — spaces, `/`, `..`, unicode, or four thousand
/// characters of it. FNV-1a over the LOWERCASED path, matching
/// `projects::dir_key`'s rule that two spellings of one directory on a
/// case-insensitive volume are one directory; hand-rolled for the same
/// reason `projects::project_id` is, that `DefaultHasher`'s output is not
/// promised to be stable across std releases and a persisted key must not
/// change under the app.
///
/// Keyed on the ANCHOR and the FOLDER, and deliberately NOT on the folder's
/// index within the project.
///
/// The index was in the key first, to stop a reordered `dirs` handing one
/// terminal its sibling's output — the user shown text that was never in
/// that directory, presented as if it were. Folding the folder in already
/// prevents that: the pair is unique, because `ProjectStore::record`
/// dedupes `dirs`, so one folder appears at most once per project and no
/// two projects share an anchor.
///
/// With the folder in the key the index only ADDS failure. A pinned or
/// renamed record keeps its own folder list while later captures move on,
/// so its indices drift apart from the store's and every one of its
/// terminals then misses — the pinned projects, the ones the user cared
/// enough to keep, restoring nothing. Dropping the index turns those
/// misses back into correct hits without ever risking a wrong one.
///
/// A miss is recoverable and wrong text is not; this change makes fewer of
/// both.
pub fn terminal_key(anchor: &Path, dir: &Path) -> String {
    format!("sb-{:016x}-{:016x}", path_hash(anchor), path_hash(dir))
}

/// Every key a project holding `dirs` can hold, which is what [`reap`] has
/// to be told to KEEP. A project contributes exactly its own keys: nothing
/// else in the store can mint them, because no other anchor hashes here
/// (see the tests).
pub fn project_keys(anchor: &Path, dirs: &[PathBuf]) -> Vec<String> {
    dirs.iter().map(|dir| terminal_key(anchor, dir)).collect()
}

/// Which of a project's `dirs` a pane sitting in `cwd` belongs to, or
/// `None` when it is in none of them.
///
/// The save side needs this and the restore side does not: a reopen walks
/// `dirs` and so knows each terminal's index by construction, while a
/// capture starts from panes and has to find each one's place in the list.
/// Offered here rather than left to the caller so both sides compare
/// directories the ONE way — case-insensitively, `projects::dir_key`'s
/// rule — instead of two spellings of the rule drifting apart.
pub fn dir_index_of(dirs: &[PathBuf], cwd: &Path) -> Option<usize> {
    let wanted = path_key(cwd);
    dirs.iter().position(|dir| path_key(dir) == wanted)
}

/// A path as it is compared: lossy UTF-8, lowercased. Mirrors
/// `projects::dir_key`, which is private to that module.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// FNV-1a over [`path_key`]. Stable across processes and releases by
/// construction — it is spelled out here rather than borrowed.
fn path_hash(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path_key(path).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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

// --- putting the text back into a LIVE terminal ---------------------------

/// SGR that clears every attribute and returns both colours to the theme's.
const SGR_RESET: &str = "\x1b[0m";

/// The rule drawn between restored history and the live shell's first
/// prompt. Everything ABOVE it came out of a file; everything below it is
/// this session. One dim line, drawn with a box-drawing rule, because this
/// is a terminal — the boundary has to be legible at a glance without
/// becoming a banner.
///
/// It says the TEXT was restored, never that the session was: the shell
/// below the rule is a new shell, and the spec's worst outcome is a user
/// who believes otherwise. And it stays true where it ENDS UP — a rule
/// drawn today is captured by tomorrow's save and comes back inside
/// tomorrow's history, where "the end of the restored scrollback" would be
/// a lie but "restored from a previous session" is still exactly what
/// happened at that line.
const SEPARATOR_LABEL: &str = "──── restored from a previous session ";

/// The bytes that put stored text into a terminal that is about to get a
/// live shell.
///
/// **The whole feature turns on this function.** The spec's restore paints
/// a dead pane, and a reopened project full of dead panes would make the
/// user revive every terminal before working — so a reopened project gets
/// a WORKING SHELL with its previous scrollback sitting above the first
/// prompt, which is what tmux does and what the product this chases does.
/// The only way to get text into a terminal's scrollback is to have the
/// terminal print it, so the saved cells are re-encoded as the ANSI that
/// would have produced them and fed to the terminal's own parser before
/// the shell says anything.
///
/// Three properties this has to have, each of which is a test:
///
/// * **Every attribute survives** — fg, bg, bold, italic, underline,
///   inverse, dim, hidden — because a scrollback that comes back
///   monochrome is not the scrollback.
/// * **Nothing leaks.** Each row is opened AND closed with a full reset,
///   so the live shell's first byte lands on a terminal in its default
///   state whatever the last stored cell was wearing.
/// * **A file can never inject an escape sequence.** Cell characters are
///   the only attacker-controlled bytes here, and one of them holding
///   `\x1b` would let a corrupt or hand-edited file drive the cursor,
///   clear the screen, or set the title of a live terminal. Every control
///   character is replaced by a space (see [`seed_char`]) — the file
///   contributes TEXT and nothing else.
///
/// Colours stay UNRESOLVED all the way through: an indexed colour is
/// re-encoded as an indexed colour, so the grid holds the index and the
/// painter resolves it through the ACTIVE theme at paint time
/// (`pane::resolve_fg`). A theme changed since the save is therefore
/// honoured, which is the reason [`CellColor`] is stored the way it is.
///
/// `cols` is the width of the terminal being seeded, and only sizes the
/// separator rule.
pub fn ansi_seed(restored: &RestoredScrollback, cols: usize) -> Vec<u8> {
    if restored.rows.is_empty() {
        // Nothing was restored, so there is no boundary to mark: the pane
        // must be indistinguishable from one that never had a file.
        return Vec::new();
    }
    let mut out = String::new();
    for row in &restored.rows {
        seed_row(row, &mut out);
        out.push_str(SGR_RESET);
        out.push_str("\r\n");
    }
    out.push_str("\x1b[2m");
    out.push_str(&separator_line(cols));
    out.push_str(SGR_RESET);
    out.push_str("\r\n");
    out.into_bytes()
}

/// One row's cells as ANSI, with no trailing reset (the caller adds it).
///
/// Trailing blank cells are dropped rather than printed: a restored row
/// padded out to the full width would carry its background colour to the
/// edge of a terminal that may now be a different width, and printing the
/// last column is also what puts a terminal into its pending-wrap state.
fn seed_row(row: &[SnapshotCell], out: &mut String) {
    let mut end = row.len();
    while end > 0 && is_blank(&row[end - 1]) {
        end -= 1;
    }
    let mut current: Option<CellStyle> = None;
    for cell in &row[..end] {
        // The spacer half of a wide glyph is NOT printed: the terminal
        // creates it itself when the wide character before it is printed,
        // and printing a second character would shift the rest of the row
        // one column left per wide glyph.
        if cell.wide_spacer {
            continue;
        }
        if current != Some(cell.style) {
            out.push_str(&sgr_for(&cell.style));
            current = Some(cell.style);
        }
        out.push(seed_char(cell.ch));
    }
}

/// A cell's character as it may be fed to a parser.
///
/// Anything that is not printable text becomes a space. A stored cell holds
/// what a grid held, so in practice this only ever rewrites the NUL of an
/// untouched cell — but the bytes here are going into a LIVE terminal, and
/// a file is the one thing in this path that a user (or a corruption) can
/// hand-edit. An `\x1b` that survived would be a stored file driving the
/// cursor of a running shell's terminal.
fn seed_char(ch: char) -> char {
    match ch {
        // C0 and DEL.
        '\0'..='\u{1f}' | '\u{7f}' => ' ',
        // C1: an 8-bit control set that a parser also acts on.
        '\u{80}'..='\u{9f}' => ' ',
        other => other,
    }
}

/// The full SGR for `style`, always starting from a reset.
///
/// Absolute rather than a diff against what came before: the sequence
/// describes the style completely, so no earlier attribute can survive
/// into it however the rows were ordered or truncated.
fn sgr_for(style: &CellStyle) -> String {
    let mut params = String::from("0");
    for (set, code) in [
        (style.bold, "1"),
        (style.dim, "2"),
        (style.italic, "3"),
        (style.underline, "4"),
        (style.inverse, "7"),
        (style.hidden, "8"),
    ] {
        if set {
            params.push(';');
            params.push_str(code);
        }
    }
    // 38/48 rather than 30-37/90-97: an INDEX has to come back as an index
    // so the painter can resolve it through the active theme, and the
    // 30-37 range parses back as a named colour instead.
    for (color, base) in [(style.fg, 38), (style.bg, 48)] {
        match color {
            // Left unsaid: the reset above already restored the theme's
            // own foreground and background.
            CellColor::Default => {}
            CellColor::Indexed(index) => params.push_str(&format!(";{base};5;{index}")),
            CellColor::Rgb(r, g, b) => params.push_str(&format!(";{base};2;{r};{g};{b}")),
        }
    }
    format!("\x1b[{params}m")
}

/// The rule itself, sized to the terminal it is going into.
fn separator_line(cols: usize) -> String {
    let label: Vec<char> = SEPARATOR_LABEL.chars().collect();
    // A rule wider than the terminal would wrap onto a second line and stop
    // reading as one boundary, so a narrow pane gets the label alone.
    let fill = cols.saturating_sub(label.len());
    let mut line = String::from(SEPARATOR_LABEL);
    line.push_str(&"─".repeat(fill));
    line
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

    // --- the key a restore is allowed to trust ----------------------------

    #[test]
    fn a_key_is_the_same_in_every_process_that_ever_derives_it() {
        // Pinned to a LITERAL, which is the whole claim: the key is not
        // whatever this build's hasher happens to produce, it is this
        // string, in every process, in every release. A run-to-run
        // comparison inside one process would prove nothing — `term-1`
        // passes that too, and `term-1` is the bug this replaces.
        let anchor = Path::new("/Users/tomas/repo");
        let native = Path::new("/Users/tomas/repo/native");
        assert_eq!(
            terminal_key(anchor, anchor),
            "sb-464d47d65b2a4d86-464d47d65b2a4d86"
        );
        assert_eq!(
            terminal_key(anchor, native),
            "sb-464d47d65b2a4d86-6a0fa3acf4760d88"
        );
        // The same directory in the case a shell's `cd` happened to leave
        // it in is the same directory — `projects::dir_key`'s rule, and it
        // has to hold for BOTH halves of the key.
        assert_eq!(
            terminal_key(
                Path::new("/users/TOMAS/Repo"),
                Path::new("/USERS/tomas/repo")
            ),
            terminal_key(anchor, anchor)
        );
    }

    #[test]
    fn reordering_a_projects_folders_still_finds_each_folders_own_text() {
        // The key was once anchor + INDEX + folder, to stop a reordered
        // `dirs` handing one terminal its sibling's output. The folder
        // alone already prevents that — `record` dedupes `dirs`, so a
        // folder appears at most once per project — and the index only
        // added failure: a pinned record keeps its own folder list while
        // later captures move on, so its indices drift and every one of
        // its terminals missed. The projects the user cared enough to pin
        // were the ones restoring nothing.
        let anchor = Path::new("/Users/tomas/repo");
        let dirs = [
            PathBuf::from("/Users/tomas/repo"),
            PathBuf::from("/Users/tomas/repo/native"),
        ];
        let reordered = [dirs[1].clone(), dirs[0].clone()];
        let mut saved = project_keys(anchor, &dirs);
        let mut reopened = project_keys(anchor, &reordered);
        saved.sort();
        reopened.sort();
        assert_eq!(
            saved, reopened,
            "a reorder must find the same text, not miss it"
        );
        // And the guarantee that made the index look necessary still
        // holds: two folders never share a key, so no terminal can be
        // handed another folder's output.
        assert_ne!(
            terminal_key(anchor, &dirs[0]),
            terminal_key(anchor, &dirs[1])
        );
    }

    #[test]
    fn a_reordered_project_restores_each_folders_own_text() {
        // The same drift, all the way through the store. When the key
        // carried the folder's INDEX this was a miss — safe, but the wrong
        // kind of safe: a pinned record keeps its own folder list while
        // later captures move on, so its indices drift and it restored
        // nothing at all. Keyed on the folder, a reorder is simply found.
        //
        // What must NEVER happen is a folder being handed its neighbour's
        // output, so this asserts the text each key returns, not merely
        // that something came back.
        let store = dir("reorder");
        let anchor = Path::new("/Users/tomas/repo");
        let dirs = [
            PathBuf::from("/Users/tomas/repo"),
            PathBuf::from("/Users/tomas/repo/native"),
        ];
        for (i, key) in project_keys(anchor, &dirs).iter().enumerate() {
            save_in(
                &store,
                key,
                &snapshot(8, vec![text_row(&format!("dir{i}"), 8)]),
            )
            .unwrap();
        }
        let reordered = [dirs[1].clone(), dirs[0].clone()];
        let keys = project_keys(anchor, &reordered);
        // `reordered` is [native, repo], so its keys come back in that
        // order and must carry dir1's and dir0's text respectively.
        for (key, expected) in keys.iter().zip(["dir1", "dir0"]) {
            let back = load_in(&store, key).expect("a reorder is found, not missed");
            let text: String = back.rows[0].iter().map(|c| c.ch).collect();
            assert!(
                text.starts_with(expected),
                "{key} returned {text:?}, wanted {expected}"
            );
        }
    }

    #[test]
    fn an_indexed_colour_is_seeded_as_an_index_so_the_active_theme_resolves_it() {
        // The reason `CellColor` is stored unresolved in the first place.
        // Re-encoded as an index, the grid holds an index and the painter
        // resolves it through the theme that is active NOW
        // (`pane::resolve_fg`), so a theme changed since the save is
        // honoured. Baked to the saved palette's channels here, a restored
        // pane would keep painting last month's colours forever.
        let mut style = plain();
        style.fg = CellColor::Indexed(4);
        style.bg = CellColor::Indexed(11);
        let sgr = sgr_for(&style);
        assert_eq!(sgr, "\x1b[0;38;5;4;48;5;11m", "{sgr}");
        // A colour the terminal gave as channels stays channels: there is
        // no palette entry for it to be resolved through.
        let mut rgb = plain();
        rgb.fg = CellColor::Rgb(1, 2, 3);
        assert_eq!(sgr_for(&rgb), "\x1b[0;38;2;1;2;3m");
        // Default says nothing at all: the leading reset already put both
        // colours back to the theme's own.
        assert_eq!(sgr_for(&plain()), "\x1b[0m");
    }

    #[test]
    fn two_projects_can_never_share_a_terminals_key() {
        let anchors = [
            "/Users/tomas/repo",
            "/Users/tomas/other",
            "/Users/tomas/repo/native",
            "/Users/tomas/rep",
            "/Users/tomas/repo ",
            "/",
            "/Users/tomas/\u{4f60}\u{597d}",
        ];
        // Every DISTINCT (anchor, folder) pair, including the shapes most
        // likely to collide under a weak hash: a prefix of another path, a
        // trailing space, the filesystem root, and non-ASCII.
        let mut seen: HashSet<String> = HashSet::new();
        for anchor in anchors {
            for dir in anchors {
                let key = terminal_key(Path::new(anchor), Path::new(dir));
                assert!(
                    seen.insert(key.clone()),
                    "{anchor} in {dir} collided: {key}"
                );
            }
        }
        assert_eq!(seen.len(), anchors.len() * anchors.len());
    }

    #[test]
    fn a_key_names_a_file_whatever_the_path_holds() {
        let dir = dir("keys");
        let hostile = [
            "/Users/tomas/two words/a-b",
            "/Users/tomas/../../etc",
            "/Users/tomas/\u{1f600}/\u{4f60}\u{597d}/\u{e9}t\u{e9}",
            "/Users/tomas/quote\"and'apostrophe",
            "/Users/tomas/new\nline",
            "relative/not/absolute",
        ];
        let long = format!("/Users/tomas/{}", "deep/".repeat(1000));
        for anchor in hostile
            .iter()
            .copied()
            .chain(std::iter::once(long.as_str()))
        {
            let key = terminal_key(Path::new(anchor), Path::new(anchor));
            assert!(
                key.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{anchor} minted an unsafe key: {key}"
            );
            // The store's own escaping must have nothing left to do, and
            // the name must fit: `file_name` refuses one that does not.
            let path = path_for(&dir, &key).expect("a key always names a file");
            assert_eq!(path.parent(), Some(dir.as_path()));
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(format!("{key}.json").as_str()),
                "a key must need no escaping"
            );
            save_in(&dir, &key, &snapshot(4, vec![text_row("hi", 4)])).unwrap();
            assert!(load_in(&dir, &key).is_some(), "{anchor} round trips");
        }
    }

    #[test]
    fn a_pane_finds_its_own_folder_in_the_projects_dirs() {
        let dirs = vec![
            PathBuf::from("/Users/tomas/repo"),
            PathBuf::from("/Users/tomas/other"),
        ];
        assert_eq!(dir_index_of(&dirs, Path::new("/Users/tomas/repo")), Some(0));
        assert_eq!(
            dir_index_of(&dirs, Path::new("/Users/tomas/other")),
            Some(1)
        );
        // Same case rule as the key itself, or a pane whose shell reports
        // a different spelling would save under a key nothing restores.
        assert_eq!(dir_index_of(&dirs, Path::new("/USERS/TOMAS/REPO")), Some(0));
        assert_eq!(dir_index_of(&dirs, Path::new("/Users/tomas")), None);
        assert_eq!(dir_index_of(&[], Path::new("/Users/tomas/repo")), None);
    }

    #[test]
    fn a_projects_keys_are_kept_by_the_reaper_and_every_other_is_taken() {
        let dir = dir("project_keys");
        let mine = Path::new("/Users/tomas/repo");
        let theirs = Path::new("/Users/tomas/other");
        let my_dirs = [
            PathBuf::from("/Users/tomas/repo"),
            PathBuf::from("/Users/tomas/repo/native"),
        ];
        let their_dirs = [
            PathBuf::from("/Users/tomas/other"),
            PathBuf::from("/Users/tomas/other/core"),
        ];
        for key in project_keys(mine, &my_dirs)
            .iter()
            .chain(&project_keys(theirs, &their_dirs))
        {
            save_in(&dir, key, &snapshot(4, vec![text_row("hi", 4)])).unwrap();
        }
        let referenced: HashSet<String> = project_keys(mine, &my_dirs).into_iter().collect();
        assert_eq!(referenced.len(), 2, "one key per folder, and no more");
        assert_eq!(reap_in(&dir, &referenced), 2, "the other project goes");
        for key in project_keys(mine, &my_dirs) {
            assert!(load_in(&dir, &key).is_some(), "{key} survives");
        }
        for key in project_keys(theirs, &their_dirs) {
            assert!(load_in(&dir, &key).is_none(), "{key} was evicted");
        }
        // A project that LOSES a folder stops referencing its key, which is
        // what stops the directory growing for the life of the install.
        let shrunk: HashSet<String> = project_keys(mine, &my_dirs[..1]).into_iter().collect();
        assert_eq!(reap_in(&dir, &shrunk), 1, "the dropped folder's file goes");
        assert!(load_in(&dir, &project_keys(mine, &my_dirs)[0]).is_some());
        assert!(load_in(&dir, &project_keys(mine, &my_dirs)[1]).is_none());
    }

    // --- the ANSI a restored terminal is seeded with -----------------------

    fn restored(cols: usize, rows: Vec<Vec<SnapshotCell>>) -> RestoredScrollback {
        RestoredScrollback { cols, rows }
    }

    fn seed_text(restored: &RestoredScrollback, cols: usize) -> String {
        String::from_utf8(ansi_seed(restored, cols)).expect("the seed is text")
    }

    #[test]
    fn a_scrollback_with_no_rows_seeds_nothing_at_all() {
        // The empty and the unreadable file arrive here the same way — one
        // as no rows, the other as a `load` that returned `None` — and both
        // must leave a terminal that is indistinguishable from one that
        // never had a file. Not even the rule: a boundary drawn above a
        // prompt with nothing above it is a lie about what happened.
        assert!(ansi_seed(&restored(80, Vec::new()), 80).is_empty());
    }

    #[test]
    fn every_seeded_row_is_closed_before_the_next_line_begins() {
        // The live shell's first byte lands on the terminal this seed left
        // behind. A row that set a background and did not clear it would
        // paint the prompt — and everything the user typed after it.
        let mut styled = plain();
        styled.bg = CellColor::Indexed(1);
        styled.bold = true;
        let rows = vec![
            vec![cell('a', styled), cell('b', styled)],
            vec![cell('c', styled)],
        ];
        let text = seed_text(&restored(4, rows), 40);
        for line in text.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            assert!(
                line.ends_with(SGR_RESET),
                "a line left styling behind: {line:?}"
            );
        }
        assert!(
            text.ends_with(&format!("{SGR_RESET}\r\n")),
            "the seed itself must end reset: {text:?}"
        );
    }

    #[test]
    fn the_boundary_is_one_rule_and_it_comes_last() {
        // One line, at the bottom, so everything ABOVE it is the restored
        // history and everything below it is this session. Anything more
        // than one line and a terminal starts looking like a chat app.
        let rows = vec![text_row("one", 8), text_row("two", 8)];
        let text = seed_text(&restored(8, rows), 40);
        let lines: Vec<&str> = text.trim_end_matches("\r\n").split("\r\n").collect();
        assert_eq!(lines.len(), 3, "two rows and the rule: {lines:?}");
        assert_eq!(text.matches(SEPARATOR_LABEL).count(), 1);
        assert!(lines[2].contains(SEPARATOR_LABEL), "{:?}", lines[2]);
        // Dim, so it reads as a mark on the terminal rather than as output.
        assert!(lines[2].starts_with("\x1b[2m"), "{:?}", lines[2]);
    }

    #[test]
    fn the_rule_fits_the_terminal_it_is_going_into() {
        // Wider than the pane and it wraps onto a second line, which stops
        // it reading as one boundary.
        let visible = |cols: usize| separator_line(cols).chars().count();
        assert_eq!(visible(80), 80);
        assert_eq!(visible(40), 40);
        // Narrower than the label itself: the label alone, never truncated
        // into something that does not say what it is.
        assert_eq!(visible(4), SEPARATOR_LABEL.chars().count());
        assert!(separator_line(10).starts_with(SEPARATOR_LABEL));
    }

    #[test]
    fn a_stored_character_can_never_become_an_escape_sequence() {
        // Cell text is the only attacker-controlled thing in the seed, and
        // it is going into a LIVE terminal. An `\x1b` that survived would
        // let a hand-edited file move the cursor, clear the screen or set
        // the window title of a running shell's terminal.
        for ch in ['\u{1b}', '\u{7}', '\r', '\n', '\0', '\u{7f}', '\u{9b}'] {
            assert_eq!(seed_char(ch), ' ', "{ch:?} survived");
        }
        assert_eq!(seed_char('a'), 'a');
        assert_eq!(seed_char('\u{4f60}'), '\u{4f60}');
        let hostile = vec![vec![
            cell('\u{1b}', plain()),
            cell('[', plain()),
            cell('2', plain()),
            cell('J', plain()),
        ]];
        let text = seed_text(&restored(8, hostile), 40);
        assert!(!text.contains("\x1b[2J"), "the file drove the terminal");
    }

    #[test]
    fn the_spacer_half_of_a_wide_glyph_is_left_to_the_terminal() {
        // The terminal creates the spacer itself when it prints the wide
        // character. Printing a second one would shift the rest of the row
        // one column left per wide glyph.
        let rows = vec![vec![
            cell('\u{4f60}', plain()),
            spacer(),
            cell('!', plain()),
        ]];
        let text = seed_text(&restored(8, rows), 40);
        let first = text.split("\r\n").next().unwrap();
        assert!(first.ends_with("\u{4f60}!\x1b[0m"), "{first:?}");
    }

    #[test]
    fn saving_creates_the_directory_it_needs() {
        let dir = dir("mkdir").join("nested");
        save_in(&dir, "term-1", &snapshot(4, vec![text_row("hi", 4)])).unwrap();
        assert!(load_in(&dir, "term-1").is_some());
    }
}
