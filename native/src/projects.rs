//! Persistent project store: `projects.json`, beside `settings.json` in the
//! same app-support directory (see `settings::settings_dir`).
//!
//! A "project" is several directories opened together (see
//! `docs/superpowers/specs/2026-09-02-projects-design.md`) — not a single
//! path. This module is pure and file-backed only: no gpui, no `Workspace`,
//! no `TerminalPane` — only `hosts::Target`, itself a plain enum.
//! `Workspace` calls [`project_dirs`] and [`project_for_dirs`] to turn a
//! tab that is about to disappear into a record, and [`ProjectStore::pinned`],
//! [`ProjectStore::recent`], [`plan_reopen`] and [`project_summary`] to list
//! those records and open them again.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hosts::Target;

/// Unpinned ("recent") projects retained. Pinned projects are exempt from
/// this cap entirely — see `ProjectStore::recent` and `ProjectStore::record`.
const RECENT_CAP: usize = 10;

/// How a project's mark is drawn. A field rather than a computed value so
/// auto-detection (`Cargo.toml` -> Rust) and a hand-picked icon can later
/// write into it without touching `Project`'s shape. `Generated` is the only
/// variant for now: the label's first character over a colour hashed from
/// the label, resolved through the active theme (drawn elsewhere; this type
/// only names the choice).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectIcon {
    #[default]
    Generated,
}

/// One project: a set of directories opened together, reopened together.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Project {
    pub id: String,
    pub label: String,
    pub dirs: Vec<PathBuf>,
    /// The one folder this project is IDENTIFIED by: chosen when the record
    /// was FIRST written (see [`anchor_dir`]) and never moved after.
    ///
    /// Persisted rather than re-derived on every capture, and that is the
    /// whole point. Derivation reads the directory SET, so opening one more
    /// folder that ranks ahead of the current one would move the anchor —
    /// and the project would stop matching its own record and fork into a
    /// second entry, losing the pin, the name and the accumulated time the
    /// first one carried. Frozen, the record keeps answering to the folder
    /// it was first known by however the folders around it change.
    ///
    /// `None` only for a record written before this field existed, and only
    /// until it is loaded: [`ProjectStore::load_from`] derives one from
    /// `dirs` for every record that has none, so a loaded store always has
    /// its anchors. A set with nothing but `$HOME` in it has no anchor at
    /// all — and is not [`worth_remembering`] anyway.
    pub anchor: Option<PathBuf>,
    pub pinned: bool,
    /// Unix timestamp, seconds. A plain integer rather than `SystemTime` so
    /// it stays comparable, sortable and human-readable once serialized.
    pub last_opened: u64,
    pub icon: ProjectIcon,
    /// Set once the user renames the project away from its auto-assigned
    /// label. Together with `pinned`, this protects the record's `id`: once
    /// either is true, a later auto-capture of the same project must never
    /// merge into it (see `ProjectStore::record`) — the record is the
    /// user's now, identified by `id`, not by its paths.
    pub renamed: bool,
    /// How many terminals the project had when it was LAST captured.
    /// Latest capture wins: unlike the label, this is a measurement, not
    /// something the user made theirs, so a pinned record's count moves.
    ///
    /// Still recorded, no longer SHOWN. It was in the row summary and
    /// truncated the project's NAME in a narrow sidebar, which cost more
    /// than it told anyone. Kept in the record because it comes for free
    /// from the same pane map the directories do, and dropping a persisted
    /// field to save nothing would only have to be added back.
    pub terminals: usize,
    /// Wall-clock seconds this project has been open, SUMMED across every
    /// session. On the way into [`ProjectStore::record`] the field carries
    /// one session's increment; in the store it carries the total. A
    /// session that never closes cleanly loses its increment rather than
    /// inventing one, so 0 means "never measured", not "no time spent".
    pub active_secs: u64,
}

/// The whole persisted set: every recorded project, pinned or not.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectStore {
    projects: Vec<Project>,
}

/// `projects.json`, beside `settings.json`.
pub fn projects_path() -> PathBuf {
    crate::settings::settings_dir().join("projects.json")
}

/// The key a directory matches on: its path, lowercased.
///
/// A decision, not an oversight. macOS volumes are case-insensitive by
/// default, so `/Users/me/Documents` and `/Users/me/documents` are ONE
/// directory that a case-sensitive comparison would record as two separate
/// projects — and both spellings genuinely occur, since a shell's `cd`
/// preserves whatever the user typed.
///
/// Not `canonicalize`: that hits the filesystem, fails outright for a
/// directory that has since been deleted (a case this feature must survive,
/// since a remembered project outlives its folder), and resolves symlinks,
/// which would silently merge two projects a user deliberately keeps apart.
/// Lowercasing is wrong only on a case-SENSITIVE volume, where it can merge
/// two directories differing only in case — rare, and a far smaller harm
/// than splitting one project in two on the default configuration.
fn dir_key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// The directory a project is IDENTIFIED by, derived from the SET it is
/// given: not `$HOME`, then the shallowest path, then first by `dir_key`.
///
/// Deterministic over the set, deliberately. The obvious rule — "the first
/// directory that is not `$HOME`" — reads `dirs` in pane order, and pane
/// order is LAYOUT, not intent: rebalancing splits, closing and reopening a
/// pane, or restoring a tab a different way all reorder it, and every one
/// of those would silently change which project this is.
///
/// Shallowest first because a project's own folder is an ANCESTOR of the
/// folders its terminals wander into: `/repo` beats `/repo/native`, and
/// `/chat` beats `/chat/packages/foo`. Ties break on `dir_key`, the same
/// case-insensitive key every other comparison here uses, so two spellings
/// of one path can never rank differently from each other.
///
/// Deliberately NOT the git root. It was suggested and is declined: finding
/// it means touching the filesystem, and this decision has to keep working
/// for a folder that has since been deleted, renamed or unmounted — which
/// is exactly when a remembered project matters most. The spec records the
/// same decision so it is not re-litigated.
///
/// `None` when the set holds no non-`$HOME` directory — an empty capture,
/// or a tab that only ever sat in `$HOME`, which [`worth_remembering`]
/// refuses to record at all. The fallback is deliberately not "then use
/// `$HOME`": every launch opens a starter tab there, and letting `$HOME` be
/// an anchor would make every such tab the same project as every other.
pub fn anchor_dir(dirs: &[PathBuf]) -> Option<&PathBuf> {
    dirs.iter()
        .filter(|dir| !is_home(dir))
        .min_by_key(|dir| anchor_rank(dir.as_path()))
}

/// How two candidate anchors are ordered: the shallower path wins, then the
/// lower `dir_key`. One function, so choosing an anchor and choosing
/// between two records that both match can never disagree about the order.
fn anchor_rank(dir: &Path) -> (usize, String) {
    (dir.components().count(), dir_key(dir))
}

/// A record's anchor: the stored one, or — for a record built before the
/// field existed and not yet through [`ProjectStore::load_from`] — the one
/// its directories derive. Never `None` for a record the store has loaded
/// or recorded, except the hand-edited all-`$HOME` case.
fn project_anchor(project: &Project) -> Option<&PathBuf> {
    project
        .anchor
        .as_ref()
        .or_else(|| anchor_dir(&project.dirs))
}

/// Whether a capture of `dirs` is the SAME project as `project`: the
/// project's own ANCHOR is still one of the folders the capture has open.
///
/// One folder decides, not the whole set. Matching the whole set forked a
/// project on ordinary use: a tab with the repo open and a second terminal
/// sitting in `~` to run `brew upgrade` records `{repo, ~}`, the same tab
/// tomorrow without that terminal records `{repo}`, and the two are
/// different sets — so recents fill with near-duplicates that differ only
/// by which incidental terminal happened to be open at close time.
/// `cd`-ing that second terminal anywhere forks it again.
///
/// Note what this is NOT: a comparison of the two sets' DERIVED anchors.
/// The capture's own derivation names a NEW project and nothing else. A
/// derived-to-derived comparison would make the stored anchor decorative,
/// because a matched record's `dirs` are replaced by the capture that
/// matched it — so its derived anchor would follow the capture around,
/// which is precisely the drift the stored anchor exists to stop. Open one
/// folder that ranks ahead of the project's own and the record would
/// re-anchor itself, then fail to match the plain project tomorrow.
///
/// The cost, and it is a real one: two genuinely separate projects that
/// both keep one project's anchor folder open merge into one record. That
/// is the deliberate trade — a project is the folder you work in, and the
/// terminals beside it come and go.
///
/// A record with no anchor matches nothing, itself included: there is no
/// folder to be the same as. `load_from` gives every record that predates
/// the field one, so in practice that is the all-`$HOME` case alone.
fn capture_has_anchor_of(dirs: &[PathBuf], project: &Project) -> bool {
    let Some(anchor) = project_anchor(project) else {
        return false;
    };
    let anchor = dir_key(anchor);
    dirs.iter().any(|dir| dir_key(dir) == anchor)
}

/// Unix seconds, now — the one place `last_opened` is minted, so a capture
/// and the store agree on what the number means.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The directories a project has, from every one of its panes given as
/// `(where its shell runs, the directory it reports)`, in first-seen pane
/// order.
///
/// This is the whole decision a capture makes, kept here — away from
/// `Workspace` — because there is no gpui test harness to reach it through
/// there. Three rules, each of which has a reason:
///
/// - **Local panes only.** A remote pane's directory exists on ANOTHER
///   machine. Reopening the project here would spawn a local shell in a
///   path that may not exist, or — worse — does exist and is something
///   else entirely. The TARGET decides this, not the cwd: `cwd()` already
///   returns `None` for a remote pane, but a rule that leans on that would
///   silently stop holding the moment a pane learned to report a peer's
///   directory.
/// - **A pane with no directory contributes none.** A shell that never
///   started or has gone reports `None`, and `pid_cwd` can hand back an
///   empty path when the syscall succeeds with an empty buffer; reopening
///   on that would land in the filesystem root.
/// - **Deduped, in first-seen order.** Two panes in one folder are one
///   folder, matched the same case-insensitive way `dir_key` matches
///   projects. The order survives because reopening spawns one terminal
///   per directory, and that is the order the user gets their shells back
///   in.
/// Whether a captured directory set is worth remembering as a project.
///
/// A bare `$HOME` is not. Every launch opens a starter tab there, so
/// recording it would put "home" at the top of recents after any
/// launch-then-quit — the list filling itself with the one entry that
/// carries no intent. A project that ALSO contains other directories is
/// kept whole, `$HOME` included: the exclusion is about a tab nobody did
/// anything with, not about the folder being forbidden.
pub fn worth_remembering(dirs: &[PathBuf]) -> bool {
    if dirs.is_empty() {
        return false;
    }
    if dirs.len() > 1 {
        return true;
    }
    !is_home(&dirs[0])
}

/// Whether `dir` is the user's home directory, matched the same
/// case-insensitive way every other directory comparison here is.
///
/// Two rules lean on this: a bare-`$HOME` tab is not a project
/// ([`worth_remembering`]), and a shell sitting in the `$HOME` fallback is
/// a shell that never reached the folder it was asked for
/// ([`pane_capture_cwd`]). With `HOME` unset nothing is home, which leaves
/// both rules erring towards remembering rather than discarding.
fn is_home(dir: &Path) -> bool {
    match std::env::var_os("HOME") {
        Some(home) => dir_key(dir) == dir_key(Path::new(&home)),
        None => false,
    }
}

pub fn project_dirs(panes: &[(Target, Option<String>)]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for (target, cwd) in panes {
        if !target.is_local() {
            continue;
        }
        let Some(cwd) = cwd else { continue };
        if cwd.trim().is_empty() {
            continue;
        }
        let dir = PathBuf::from(cwd);
        if seen.insert(dir_key(&dir)) {
            dirs.push(dir);
        }
    }
    dirs
}

/// One pane of a tab, as the tab remembers it.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneDir {
    /// Where the pane's shell runs. Only a local one contributes a
    /// directory — see [`project_dirs`], which still owns that rule.
    pub target: Target,
    /// The last directory this pane was SEEN in, exactly as its shell
    /// reported it. `None` for a remote pane, and for a pane that has not
    /// answered yet.
    pub seen: Option<String>,
    /// The folder a project reopen ASKED this pane for and could not give
    /// it, because the folder was gone at the time. `None` for every
    /// ordinary pane. See [`pane_capture_cwd`].
    pub requested: Option<PathBuf>,
}

/// What a pane contributes to its project's directory set.
///
/// Normally the last directory it was seen in — the spec's "live cwd, not
/// spawn cwd", so a shell that `cd`s into the subdirectory the user
/// actually works in reopens there.
///
/// A pane a reopen spawned for a folder that had GONE is the exception.
/// Its shell landed in `$HOME` (the fallback `TermSession::spawn` uses
/// when given no cwd), so letting that `$HOME` stand would put a folder
/// in the record that the user never chose — and if the missing folder
/// were the project's PRIMARY one (see `same_project`), the capture would
/// be identified by whatever came next instead. The folder it was asked
/// for stands instead: a project is those folders, and one of them being
/// temporarily missing does not change which project it is.
///
/// It stands only while the shell is still sitting in that fallback. Once
/// the user moves that terminal somewhere real, where it actually is is
/// the truth again, and the ordinary rule resumes.
pub fn pane_capture_cwd(pane: &PaneDir) -> Option<String> {
    let Some(requested) = &pane.requested else {
        return pane.seen.clone();
    };
    match &pane.seen {
        Some(seen) if !is_home(Path::new(seen)) => Some(seen.clone()),
        _ => Some(requested.to_string_lossy().to_string()),
    }
}

/// Every pane a tab has HAD, and where each of them was last seen.
///
/// Capture reads this rather than the panes still alive when the tab dies,
/// and the difference is the whole point: a tab dies when its LAST
/// terminal goes, so a four-folder project closed one pane at a time was
/// captured as a ONE-folder project with one terminal — the survivor —
/// and reopened as a single shell. Closing panes individually is
/// completely ordinary, so reading the survivors defeated the feature in
/// its commonest case.
///
/// A union over PANES, never over time. A pane's entry is REPLACED when
/// its shell moves, so a terminal that `cd`s around all day contributes
/// one directory rather than every directory it ever visited, and the
/// whole structure is bounded by panes opened rather than by `cd`s.
///
/// It belongs to one tab and dies with it: `Workspace` keys these by tab
/// id and prunes them in `prune_closed_tab_state`, alongside
/// `tab_opened_at`, so no entry outlives the tab it describes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabPaneDirs {
    /// Ordered, not a `HashMap`: reopening spawns one terminal per
    /// directory, so first-seen pane order is the order the user gets
    /// their shells back in — and a hashed order would scramble it
    /// differently on every run.
    entries: Vec<(String, PaneDir)>,
}

impl TabPaneDirs {
    fn entry_mut(&mut self, pane_id: &str, target: &Target) -> &mut PaneDir {
        if let Some(index) = self.entries.iter().position(|(id, _)| id == pane_id) {
            return &mut self.entries[index].1;
        }
        self.entries.push((
            pane_id.to_string(),
            PaneDir {
                target: target.clone(),
                seen: None,
                requested: None,
            },
        ));
        &mut self.entries.last_mut().expect("just pushed").1
    }

    /// Note that `pane_id` is one of this tab's panes, and where its shell
    /// is now.
    ///
    /// A `None` cwd never erases a directory already seen. A shell that
    /// has exited stops answering, and forgetting where it was is exactly
    /// the failure `last_known_cwd` exists to prevent. It still creates
    /// the entry, so a pane that never reports a directory at all — a
    /// remote one — is still counted among the terminals the project had.
    pub fn saw(&mut self, pane_id: &str, target: &Target, cwd: Option<String>) {
        let entry = self.entry_mut(pane_id, target);
        if cwd.is_some() {
            entry.seen = cwd;
        }
    }

    /// Record the folder a project reopen asked `pane_id` for but could
    /// not give it. See [`pane_capture_cwd`].
    pub fn asked_for(&mut self, pane_id: &str, target: &Target, dir: PathBuf) {
        self.entry_mut(pane_id, target).requested = Some(dir);
    }

    /// How many terminals the project has had — every pane that has ever
    /// belonged to this tab, not just the ones still alive. That is the
    /// honest answer to "how big is this thing", and it is the same
    /// population the directories are drawn from, so the two halves of a
    /// project's row can never disagree.
    pub fn terminals(&self) -> usize {
        self.entries.len()
    }

    /// Every directory this tab's panes contribute, deduped, in first-seen
    /// pane order. The rules — local panes only, no empty paths, deduped
    /// case-insensitively — stay in [`project_dirs`]; this only decides
    /// WHICH cwd each pane offers it.
    pub fn dirs(&self) -> Vec<PathBuf> {
        let panes: Vec<(Target, Option<String>)> = self
            .entries
            .iter()
            .map(|(_, pane)| (pane.target.clone(), pane_capture_cwd(pane)))
            .collect();
        project_dirs(&panes)
    }
}

/// The record an auto-capture writes for `dirs`, or `None` when there is
/// nothing to reopen — an all-remote project is not persisted at all.
///
/// The label is the ANCHOR's basename: a fine default for one directory and
/// useless for four, which is exactly why it is only a default. Naming it
/// after `dirs[0]` instead read pane order, so a project whose first pane
/// happened to sit in `$HOME` was labelled with home's basename — the one
/// folder that carries no intent, and never the anchor. `renamed` stays
/// false so the user's own name, once given, wins over every later capture
/// (see `ProjectStore::record`).
pub fn project_for_dirs(dirs: Vec<PathBuf>, now: u64) -> Option<Project> {
    let anchor = anchor_dir(&dirs).cloned();
    // `dirs.first()` is the fallback for a capture that has no anchor at
    // all — every directory is `$HOME` — which `record` refuses to store
    // anyway. It only keeps this function from having to answer "what is a
    // project with no folders called".
    let named_by = anchor.clone().or_else(|| dirs.first().cloned())?;
    let label = named_by
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| named_by.to_string_lossy().to_string());
    Some(Project {
        id: project_id(&dirs),
        label,
        dirs,
        anchor,
        pinned: false,
        last_opened: now,
        icon: ProjectIcon::default(),
        renamed: false,
        // Both are the caller's to fill: only `Workspace` knows how many
        // panes the tab had and how long it was open. Left at zero here so
        // a capture that cannot answer reports nothing rather than a
        // number it made up.
        terminals: 0,
        active_secs: 0,
    })
}

/// One session's worth of open time, from the mark laid when the tab was
/// created or reopened to the moment of capture.
///
/// Saturating, deliberately. Both ends come from the system clock, and a
/// clock that steps backwards — NTP correcting, a sleep/wake, a restored
/// backup — would otherwise wrap `u64` into hundreds of billions of years
/// of "work" and poison the accumulated total permanently.
pub fn session_secs(opened_at: u64, now: u64) -> u64 {
    now.saturating_sub(opened_at)
}

/// The record a REOPEN writes: the project as it stands, marked as used
/// now, with the terminal count it just opened with.
///
/// `active_secs` is zero because this is the increment, not the total —
/// opening a project has not yet spent any time in it. Recording this
/// (rather than waiting for the tab to close) means a crash still leaves
/// "you just used this" behind; and because it carries the project's own
/// `dirs`, `record` merges it into the existing entry — a pinned project
/// reopened does not also appear as an unpinned twin in recents.
pub fn touch_for_reopen(project: &Project, now: u64, terminals: usize) -> Project {
    Project {
        last_opened: now,
        terminals,
        active_secs: 0,
        ..project.clone()
    }
}

/// Where each remembered directory actually reopens.
#[derive(Debug, Clone, PartialEq)]
pub struct ReopenPlan {
    /// One entry per remembered directory, in order: the directory to
    /// spawn the shell in, or `None` for "the shell's default", which is
    /// `$HOME` (see `TermSession::spawn`).
    pub spawns: Vec<Option<PathBuf>>,
    /// The remembered directories that are no longer there, in order —
    /// what the note tells the user about.
    pub missing: Vec<PathBuf>,
}

/// Decide where a project's terminals open, given a way to ask whether a
/// directory is still there.
///
/// A directory that has been deleted, renamed or unmounted falls back to
/// `$HOME` — the project still opens, and every folder that IS there opens
/// where it belongs. Refusing the whole project because one of four
/// folders went would lose the three that survived; spawning in a gone
/// path fails at the PTY, leaving a dead pane and no explanation.
///
/// `exists` is a parameter rather than a `Path::is_dir` call so the
/// decision is testable without creating and deleting real directories.
/// The caller passes `|p| p.is_dir()`.
pub fn plan_reopen(dirs: &[PathBuf], exists: impl Fn(&Path) -> bool) -> ReopenPlan {
    let mut spawns = Vec::with_capacity(dirs.len());
    let mut missing = Vec::new();
    for dir in dirs {
        if exists(dir) {
            spawns.push(Some(dir.clone()));
        } else {
            spawns.push(None);
            missing.push(dir.clone());
        }
    }
    ReopenPlan { spawns, missing }
}

/// The visible note a reopen leaves when folders were missing, or `None`
/// when they were all there.
///
/// Names the folders. "Some folders are gone" would leave the user with
/// four terminals and no way to tell which one is not where they think it
/// is — the silent failure the spec rules out, only wordier.
pub fn missing_dirs_note(missing: &[PathBuf]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let names: Vec<String> = missing
        .iter()
        .map(|dir| dir.to_string_lossy().to_string())
        .collect();
    Some(format!(
        "{} gone \u{2014} opened in ~ instead: {}",
        count_label(missing.len(), "folder"),
        names.join(", ")
    ))
}

/// "no folders" / "1 folder" / "4 folders".
///
/// Zero is a word rather than a bare `0`, which reads like a value that
/// failed to load instead of one that was stated.
pub fn count_label(n: usize, noun: &str) -> String {
    match n {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{n} {noun}s"),
    }
}

/// How long a project has been worked in, at the coarsest unit that still
/// says something true.
///
/// Sub-minute reads as sub-minute: rounding it into hours gives "0h",
/// which looks like a bug rather than a measurement. Only the two largest
/// units ever appear — "3h 12m", never "3h 12m 7s".
pub fn duration_label(secs: u64) -> String {
    if secs < 60 {
        return "under a minute".to_string();
    }
    let minutes = secs / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let odd_minutes = minutes % 60;
    if hours < 24 {
        return match odd_minutes {
            0 => format!("{hours}h"),
            m => format!("{hours}h {m}m"),
        };
    }
    let days = hours / 24;
    match hours % 24 {
        0 => format!("{days}d"),
        h => format!("{days}d {h}h"),
    }
}

/// What a project row says about itself beneath its name.
///
/// The TERMINAL count is deliberately not here. It was, and it cost the
/// thing the row exists for: the sidebar is narrow, so "SuperTerminal ·
/// 1 terminal" truncated to "SuperTermin 1 terminal" — the count pushing
/// out the name, which is the only part a user scans for. Folders is the
/// number that says how big the project is; terminals is a detail worth
/// less than the letters it eats.
///
/// Time is omitted entirely at zero. A record written before `active_secs`
/// existed, or one whose only session never closed cleanly, has no
/// measurement to report — and "under a minute" would invent one.
pub fn project_summary(dirs: usize, active_secs: u64) -> String {
    let mut parts = vec![count_label(dirs, "folder")];
    if active_secs > 0 {
        parts.push(duration_label(active_secs));
    }
    parts.join(" \u{b7} ")
}

/// A capture's id: FNV-1a over the directory keys, sorted so pane order
/// cannot change it.
///
/// `record` matches on the stored ANCHOR (see `capture_has_anchor_of`), so
/// an id only has to be unique. Deriving it from the whole set buys one more
/// thing for nothing: a project whose `projects.json` was lost comes back
/// under the id it always had, instead of a fresh one on every launch —
/// and a record that IS matched keeps the id it already has, so the two
/// rules never disagree about a project that still exists. Hand-rolled rather
/// than `DefaultHasher`, whose output std does not promise to keep stable
/// across releases — a persisted id must not change under the app.
fn project_id(dirs: &[PathBuf]) -> String {
    let mut keys: Vec<String> = dirs.iter().map(|dir| dir_key(dir)).collect();
    keys.sort();
    keys.dedup();
    format!("proj-{:016x}", fnv1a(keys.join("\u{0}").as_bytes()))
}

/// FNV-1a, 64-bit. Hand-rolled rather than `DefaultHasher`, whose output
/// std does not promise to keep stable across releases: a project's id is
/// PERSISTED, so it must not change under the app, and its mark's colour
/// must not change under the user.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// How many colours a generated mark can land on. The palette itself is
/// the ACTIVE THEME's (see `workspace::project_mark_color`) — a fixed set
/// of hex colours would clash with a custom theme, and this module has no
/// business knowing what colour anything is.
pub const MARK_SLOTS: usize = 6;

/// What a project's row draws for itself: one character, and which slot of
/// the theme's palette colours it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectMark {
    /// Always a real character. A label that is empty or nothing but
    /// whitespace — only a hand-edited file can produce one — still gets a
    /// mark rather than a hole in the row.
    pub ch: char,
    /// Always less than [`MARK_SLOTS`].
    pub slot: usize,
}

/// The mark a project shows, per its [`ProjectIcon`].
///
/// A `match` with one arm today, deliberately: `icon` is the field that
/// auto-detection (`Cargo.toml` -> Rust) and a hand-picked set will later
/// write into, and this is where those arms land. Nothing else in the app
/// asks what a project looks like.
pub fn project_mark(label: &str, icon: ProjectIcon) -> ProjectMark {
    match icon {
        ProjectIcon::Generated => generated_mark(label),
    }
}

/// The label's first character over a colour hashed from the WHOLE label.
///
/// Hashing the whole label, not the character: half a user's projects
/// start with the same letter, and colouring by the initial would collapse
/// them onto one colour — which is the one thing a mark exists to prevent.
///
/// Trimmed and lowercased first, matching `dir_key`'s case-insensitivity,
/// so `chat` and `Chat` are one project's mark rather than two. The
/// character itself is upper-cased for legibility at 9px.
///
/// `chars().next()`, never `&label[..1]`: a label beginning with an
/// accented letter or a CJK character is multi-byte, and slicing by byte
/// would panic mid-character on exactly the labels a user is least likely
/// to be able to work around.
fn generated_mark(label: &str) -> ProjectMark {
    let trimmed = label.trim();
    let ch = trimmed
        .chars()
        .next()
        .and_then(|first| first.to_uppercase().next())
        // A label with no characters at all still gets a mark. Nothing
        // writes one — a project is named for its anchor folder — but a
        // hand-edited `projects.json` can, and a blank square reads as a
        // failure to render rather than as a project.
        .unwrap_or('?');
    let slot = (fnv1a(trimmed.to_lowercase().as_bytes()) % MARK_SLOTS as u64) as usize;
    ProjectMark { ch, slot }
}

impl ProjectStore {
    pub fn load() -> ProjectStore {
        Self::load_from(&projects_path())
    }

    /// Missing or corrupt files load defaults (an empty store) rather than
    /// erroring — losing the projects file must never be worse than an
    /// inconvenience.
    ///
    /// Every record written before `anchor` existed gets one here, derived
    /// from the `dirs` it does have. Deriving it once, on the way in, is
    /// what makes the field's promise — set at first capture, never moved
    /// after — true for projects first captured before there was a field to
    /// set. The alternatives are both worse: leaving it `None` and deriving
    /// on every comparison is the drift this change removes, and dropping
    /// such records would throw away every project the user already had.
    pub fn load_from(path: &Path) -> ProjectStore {
        let mut store: ProjectStore = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ProjectStore::default(),
        };
        store.adopt_anchors();
        store
    }

    /// Give every record without a stored anchor the one its directories
    /// derive. A record holding nothing but `$HOME` keeps `None`: there is
    /// no folder to anchor it to, and asking must not panic.
    fn adopt_anchors(&mut self) {
        for project in &mut self.projects {
            if project.anchor.is_none() {
                project.anchor = anchor_dir(&project.dirs).cloned();
            }
        }
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&projects_path())
    }

    /// Atomic write (tmp + rename), same discipline as `settings.rs`.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Record a project after it closes (or on quit). Encodes:
    /// - an empty `dirs` project is not recorded — there is nothing to
    ///   reopen;
    /// - a capture that still has an existing record's ANCHOR (see
    ///   `capture_has_anchor_of`) updates that record rather than adding a
    ///   duplicate — the folders beside it may differ;
    /// - the matched record KEEPS its anchor. A capture that has picked up
    ///   a folder ranking ahead of it does not re-anchor the project;
    /// - a pinned or renamed record is never merged into: it keeps its `id`
    ///   and the incoming capture becomes its own new record instead;
    /// - recording evicts only the oldest UNPINNED project once the unpinned
    ///   count exceeds the cap.
    pub fn record(&mut self, mut project: Project) {
        // Deduped on the way in, not just for matching. Two panes in the
        // same directory are one directory, and storing it twice would make
        // reopening spawn two shells in the same place.
        let mut seen = HashSet::new();
        project.dirs.retain(|d| seen.insert(dir_key(d)));
        if !worth_remembering(&project.dirs) {
            return;
        }
        // Every record in the store carries its own anchor, whether it was
        // built by `project_for_dirs` (which derives one), carried forward
        // by `touch_for_reopen` (which keeps the stored one), or written by
        // a build that predates the field.
        if project.anchor.is_none() {
            project.anchor = anchor_dir(&project.dirs).cloned();
        }
        // A pinned or renamed record still MATCHES — it just is not
        // overwritten. Only its `last_opened` moves, which is what keeps the
        // pinned list ordered by use and stops a second, unpinned copy of
        // the same project accumulating in recents every time it is opened.
        // The spec's rule is that such a record keeps its `id`, and it does.
        if let Some(index) = self.match_index(&project.dirs) {
            let existing = &mut self.projects[index];
            existing.last_opened = project.last_opened;
            // Both stats move even for a pinned or renamed record. What
            // that rule protects is what the user MADE theirs — the id,
            // the name, the folders. How many terminals it had and how
            // long it has been worked in are measurements; freezing them
            // at pin time would make a pinned project's row go stale and
            // stop answering the question it exists to answer.
            existing.terminals = project.terminals;
            // Summed, never replaced: `active_secs` answers "how much have
            // I actually worked here" across every session, so the
            // incoming value is one session's increment.
            existing.active_secs = existing.active_secs.saturating_add(project.active_secs);
            if !existing.pinned && !existing.renamed {
                existing.label = project.label.clone();
                existing.dirs = std::mem::take(&mut project.dirs);
                existing.icon = project.icon;
                // `anchor` is deliberately NOT among them, pinned or not.
                // The folders a project has move all the time; which one it
                // IS was settled at its first capture, and a later capture
                // that re-derived it would silently make this record answer
                // to a different folder — after which the plain project,
                // captured tomorrow, would no longer find it.
            }
            // A capture the user has NAMED speaks for the user, so its
            // label goes through whatever the record already says — and
            // marks the record theirs from here on.
            //
            // Without this the flag would never reach the store at all,
            // and would protect nothing: an auto-capture labels a project
            // from its anchor folder's basename, so the very next close
            // would quietly take the user's name back off it. Only the
            // NAME crosses; the record's folders stay its own, exactly as
            // they do for a record already marked.
            if project.renamed {
                existing.label = project.label;
                existing.renamed = true;
            }
            return;
        }
        self.projects.push(project);
        let just_added = self.projects.len() - 1;
        self.evict_past_cap(just_added);
    }

    /// Which stored record an incoming capture belongs to, if any.
    ///
    /// A record qualifies when the capture still has its anchor folder
    /// open. Two records can qualify at once — a capture holding both their
    /// anchors — so the tie is settled deterministically rather than by
    /// storage order, which is insertion order and says nothing about the
    /// project: the record anchored at the capture's OWN derived anchor
    /// first (the folder this capture is most plausibly rooted in), then by
    /// the same rank `anchor_dir` chooses with, then by position.
    ///
    /// Takes the DIRECTORIES rather than a whole `Project` because a live
    /// tab is not a record and has none: [`ProjectStore::matching`] asks
    /// this same question on behalf of a row the user is looking at, and
    /// the two must never drift into answering it differently.
    fn match_index(&self, dirs: &[PathBuf]) -> Option<usize> {
        let derived = anchor_dir(dirs).map(|dir| dir_key(dir));
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, candidate)| capture_has_anchor_of(dirs, candidate))
            .min_by_key(|(index, candidate)| {
                let anchor = project_anchor(candidate).expect("a match has an anchor");
                let (depth, key) = anchor_rank(anchor);
                // `false` sorts first, so "this IS the capture's own
                // derived anchor" has to be stated the other way up.
                (derived.as_deref() != Some(key.as_str()), depth, key, *index)
            })
            .map(|(index, _)| index)
    }

    /// The stored record a LIVE tab's folders belong to, if any.
    ///
    /// The same question `record` asks — is the record's own anchor folder
    /// still one this tab has open — asked on behalf of a row the user is
    /// looking at. A live tab is not a record, so pinning one has to find
    /// the record it stands for first, and a tab that has since opened a
    /// scratch terminal elsewhere must still find itself.
    pub fn matching(&self, dirs: &[PathBuf]) -> Option<&Project> {
        self.match_index(dirs).map(|index| &self.projects[index])
    }

    /// Pin or unpin the record with `id`; `false` when there is no such
    /// record, so nothing changed.
    ///
    /// `last_opened` is deliberately left alone in BOTH directions. A pin
    /// is a statement about keeping a project, never a use of it: bumping
    /// the timestamp on unpin would drop a project the user has just let
    /// go of at the TOP of recents, above everything they have actually
    /// worked in since; bumping it on pin would reshuffle the pinned list
    /// (`pinned()` sorts on it) on a click that said nothing about what
    /// the user is working on.
    ///
    /// Unpinning past the cap evicts nothing here. The record stays,
    /// `recent()` shows the newest `RECENT_CAP` of them, and the next
    /// capture that adds a project trims the rest — unpinning must not be
    /// a way to delete something.
    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool {
        match self.projects.iter_mut().find(|p| p.id == id) {
            Some(project) => {
                project.pinned = pinned;
                true
            }
            None => false,
        }
    }

    /// Drop the oldest unpinned project(s) until the unpinned count is back
    /// at or under `RECENT_CAP`. Pinned projects are never candidates, and
    /// neither is `keep`.
    ///
    /// `keep` is the project just recorded, and excluding it is not a
    /// nicety: `last_opened` comes from the system clock, so a store whose
    /// entries carry future timestamps — clock skew, a restored backup, a
    /// hand-edited file — would make every new capture the "oldest" and
    /// silently discard it, permanently and invisibly. A capture the user
    /// just made must survive the write that made it.
    fn evict_past_cap(&mut self, keep: usize) {
        while self.projects.iter().filter(|p| !p.pinned).count() > RECENT_CAP {
            let oldest = self
                .projects
                .iter()
                .enumerate()
                .filter(|(i, p)| !p.pinned && *i != keep)
                .min_by_key(|(_, p)| p.last_opened)
                .map(|(i, _)| i);
            match oldest {
                Some(i) => {
                    self.projects.remove(i);
                }
                None => break,
            }
        }
    }

    /// Pinned projects, newest first. Never capped.
    pub fn pinned(&self) -> Vec<&Project> {
        let mut v: Vec<&Project> = self.projects.iter().filter(|p| p.pinned).collect();
        v.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        v
    }

    /// Every record the store holds, in storage order.
    ///
    /// NOT `pinned()` + `recent()`: `recent()` shows the newest
    /// `RECENT_CAP` of the unpinned ones, and unpinning past the cap
    /// deliberately evicts nothing — so a store can hold records those two
    /// together do not name. A caller reaping files against "what the store
    /// still claims" has to see all of them, or unpinning an eleventh
    /// project would silently delete its terminals' text.
    pub fn all(&self) -> &[Project] {
        &self.projects
    }

    /// Unpinned projects, newest first, capped at `RECENT_CAP`.
    pub fn recent(&self) -> Vec<&Project> {
        let mut v: Vec<&Project> = self.projects.iter().filter(|p| !p.pinned).collect();
        v.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        v.truncate(RECENT_CAP);
        v
    }
}

/// The two remembered sections the sidebar lists beneath the live tabs.
#[derive(Debug, PartialEq)]
pub struct SidebarSections<'a> {
    pub pinned: Vec<&'a Project>,
    pub recent: Vec<&'a Project>,
}

/// Split the store into what the sidebar shows, given the directory sets
/// of the projects that are open RIGHT NOW.
///
/// A project that is open is already listed above as a live tab, so
/// repeating it in either section shows the user the same thing twice and
/// invites them to "reopen" what they are looking at.
///
/// PINNED is filtered too, which reverses an earlier decision. That
/// version argued pinning is a promise the project is always in the list,
/// so hiding it would look broken. In use the duplicate was the thing that
/// looked broken. Pinning promises the project is there when you come
/// BACK; it returns the moment the tab closes.
///
/// Matched on the project's stored ANCHOR, the same way `record` decides
/// two captures are one project — so a tab that has picked up an extra
/// terminal in `~` since the record was written still counts as open, and
/// a tab rooted somewhere else hides nothing, however many other folders
/// the two happen to share.
pub fn sidebar_sections<'a>(store: &'a ProjectStore, open: &[Vec<PathBuf>]) -> SidebarSections<'a> {
    // A project that is OPEN is already on screen, as a live tab above
    // these sections. Listing it again below is the same project twice,
    // and an earlier version did exactly that for pinned ones on the
    // reasoning that hiding a pinned project would make the pin look
    // broken. It reads as a duplicate instead — the user sees their
    // project in the active list AND under PINNED and cannot tell what
    // the second one is for.
    //
    // Both sections filter now. A pinned project reappears the moment it
    // is closed, which is the promise pinning actually makes: it will be
    // there when you come back, not that it will be listed twice while
    // you are already in it.
    let is_open = |project: &Project| open.iter().any(|dirs| capture_has_anchor_of(dirs, project));
    SidebarSections {
        pinned: store
            .pinned()
            .into_iter()
            .filter(|project| !is_open(project))
            .collect(),
        recent: store
            .recent()
            .into_iter()
            .filter(|project| !is_open(project))
            .collect(),
    }
}

/// Which already-open tab IS this project, if any.
///
/// Now that `sidebar_sections` hides an open project from both sections,
/// no steady-state row can be clicked while its project is open — so this
/// is a guard against a STALE render rather than the everyday path it was
/// written as. A row built while the project was closed can be clicked
/// after it has opened, and without this that click would spawn a second
/// copy of every one of its terminals, and another on the next click,
/// without limit.
///
/// Matched on the stored anchor, the same rule `record` and
/// `sidebar_sections` use, so "the same project" means one thing
/// everywhere.
pub fn open_tab_for(project: &Project, open: &[Vec<PathBuf>]) -> Option<usize> {
    open.iter()
        .position(|dirs| capture_has_anchor_of(dirs, project))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::{ProfileId, Target};

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "st-native-projects-{}-{}",
            std::process::id(),
            name
        ))
    }

    fn project(id: &str, dirs: &[&str], last_opened: u64) -> Project {
        Project {
            id: id.to_string(),
            label: id.to_string(),
            dirs: dirs.iter().map(PathBuf::from).collect(),
            // Left unset on purpose: a hand-built record is the shape a
            // record written before the field had, so these tests exercise
            // the derive-on-the-way-in path as well as the rules they name.
            anchor: None,
            pinned: false,
            last_opened,
            icon: ProjectIcon::default(),
            renamed: false,
            terminals: 0,
            active_secs: 0,
        }
    }

    // --- load/save, mirroring settings.rs's discipline ---

    #[test]
    fn missing_file_loads_defaults() {
        let store = ProjectStore::load_from(&tmp("nope").join("projects.json"));
        assert_eq!(store, ProjectStore::default());
        assert!(store.pinned().is_empty());
        assert!(store.recent().is_empty());
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let dir = tmp("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(ProjectStore::load_from(&path), ProjectStore::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn round_trip_preserves_values() {
        let dir = tmp("roundtrip");
        let path = dir.join("projects.json");
        let mut store = ProjectStore::default();
        store.record(project("p1", &["/chat", "/board-kid"], 100));
        let mut with_pin = project("p2", &["/solo"], 200);
        with_pin.pinned = true;
        with_pin.renamed = true;
        with_pin.label = "Renamed Solo".to_string();
        store.record(with_pin);
        store.save_to(&path).unwrap();
        let loaded = ProjectStore::load_from(&path);
        assert_eq!(loaded, store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = tmp("mkdir_parent");
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("nested").join("projects.json");
        let store = ProjectStore::default();
        store.save_to(&path).unwrap();
        assert!(path.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let dir = tmp("no_tmp_leftover");
        let path = dir.join("projects.json");
        let mut store = ProjectStore::default();
        store.record(project("p1", &["/a"], 1));
        store.save_to(&path).unwrap();
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["projects.json".to_string()], "{entries:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn projects_path_is_beside_settings_path() {
        let projects = projects_path();
        let settings = crate::settings::settings_path();
        assert_eq!(projects.parent(), settings.parent());
        assert_eq!(projects.file_name().unwrap(), "projects.json");
    }

    // --- recent()/pinned() ---

    #[test]
    fn recent_returns_unpinned_newest_first() {
        let store = ProjectStore {
            projects: vec![
                project("mid", &["/mid"], 200),
                project("old", &["/old"], 100),
                project("new", &["/new"], 300),
            ],
        };
        let ids: Vec<&str> = store.recent().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "mid", "old"]);
    }

    #[test]
    fn recent_excludes_pinned() {
        let mut pinned = project("pin", &["/pin"], 999);
        pinned.pinned = true;
        let store = ProjectStore {
            projects: vec![pinned, project("a", &["/a"], 1), project("b", &["/b"], 2)],
        };
        let recent_ids: Vec<&str> = store.recent().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(recent_ids, vec!["b", "a"]);
        let pinned_ids: Vec<&str> = store.pinned().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(pinned_ids, vec!["pin"]);
    }

    #[test]
    fn recent_is_capped_at_ten_even_without_going_through_record() {
        let projects: Vec<Project> = (0..12)
            .map(|i| project(&format!("p{i}"), &[&format!("/d{i}")], i as u64))
            .collect();
        let store = ProjectStore { projects };
        let recent = store.recent();
        assert_eq!(recent.len(), 10);
        let ids: Vec<&str> = recent.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["p11", "p10", "p9", "p8", "p7", "p6", "p5", "p4", "p3", "p2"]
        );
    }

    #[test]
    fn pinned_is_not_capped() {
        let projects: Vec<Project> = (0..12)
            .map(|i| {
                let mut p = project(&format!("p{i}"), &[&format!("/d{i}")], i as u64);
                p.pinned = true;
                p
            })
            .collect();
        let store = ProjectStore { projects };
        assert_eq!(store.pinned().len(), 12);
        assert!(store.recent().is_empty());
    }

    // --- record(): cap eviction ---

    // NOTE: these two assert on `store.projects` directly (the raw
    // persisted storage, reachable from this in-module test), not on
    // `recent()`. `recent()` applies its own cap on every call regardless
    // of what's stored, so asserting only through it cannot tell "record()
    // actually trims storage" apart from "storage grows unboundedly and
    // recent() just hides the excess" — a real bug (unbounded projects.json
    // growth) that a recent()-only assertion would miss entirely.

    #[test]
    fn record_evicts_oldest_unpinned_past_cap() {
        let projects: Vec<Project> = (0..10)
            .map(|i| project(&format!("p{i}"), &[&format!("/d{i}")], i as u64))
            .collect();
        let mut store = ProjectStore { projects };
        store.record(project("newcomer", &["/newcomer"], 999));
        assert_eq!(
            store.projects.len(),
            10,
            "storage itself must be trimmed, not just recent()'s view"
        );
        let stored_ids: HashSet<&str> = store.projects.iter().map(|p| p.id.as_str()).collect();
        assert!(
            !stored_ids.contains("p0"),
            "oldest unpinned must be evicted"
        );
        assert!(stored_ids.contains("newcomer"));
        assert!(
            stored_ids.contains("p1"),
            "only the single oldest is evicted"
        );
    }

    #[test]
    fn record_never_evicts_pinned_even_when_oldest() {
        let mut projects: Vec<Project> = (1..=10)
            .map(|i| project(&format!("p{i}"), &[&format!("/d{i}")], i as u64))
            .collect();
        let mut ancient_pinned = project("ancient", &["/ancient"], 0);
        ancient_pinned.pinned = true;
        projects.push(ancient_pinned);
        let mut store = ProjectStore { projects };
        store.record(project("newcomer", &["/newcomer"], 999));
        assert_eq!(
            store.projects.len(),
            11,
            "10 unpinned survivors + the untouched pinned one"
        );
        let stored_ids: HashSet<&str> = store.projects.iter().map(|p| p.id.as_str()).collect();
        assert!(stored_ids.contains("ancient"), "pinned survives eviction");
        assert!(!stored_ids.contains("p1"), "oldest UNPINNED is evicted");
        assert!(stored_ids.contains("newcomer"));
    }

    // --- record(): identity / merge rules ---

    #[test]
    fn record_ignores_empty_dirs() {
        let mut store = ProjectStore::default();
        store.record(project("nothing-to-reopen", &[], 1));
        assert!(store.pinned().is_empty());
        assert!(store.recent().is_empty());
    }

    #[test]
    fn record_merges_matching_dir_set_updates_last_opened_not_id() {
        let mut store = ProjectStore::default();
        store.record(project("orig", &["/chat", "/board-kid"], 100));
        store.record(project("would-be-new-id", &["/chat", "/board-kid"], 500));
        let recent = store.recent();
        assert_eq!(recent.len(), 1, "must update, not duplicate");
        assert_eq!(recent[0].id, "orig", "existing id is kept");
        assert_eq!(recent[0].last_opened, 500);
    }

    #[test]
    fn record_matches_on_the_anchor_whatever_order_the_rest_arrive_in() {
        // REWRITTEN TWICE. First when identity moved from the whole
        // directory set to one directory; then again when that directory
        // stopped being "the first non-$HOME one". Pane order is LAYOUT,
        // so identity may not read it at all: the anchor is derived from
        // the SET (shallowest, then by key) and then frozen on the record.
        // Everything beside it may be reordered, added or dropped freely,
        // which is the point (see `capture_has_anchor_of`).
        let mut store = ProjectStore::default();
        store.record(project("orig", &["/chat", "/board-kid", "/penpot"], 100));
        store.record(project(
            "would-be-new-id",
            &["/chat", "/penpot", "/board-kid"],
            500,
        ));
        let recent = store.recent();
        assert_eq!(recent.len(), 1, "the folders beside the anchor are free");
        assert_eq!(recent[0].id, "orig");
        assert_eq!(recent[0].last_opened, 500);
    }

    #[test]
    fn a_project_that_is_already_open_resolves_to_its_tab() {
        // The million-terminals bug. The pinned section keeps showing a
        // project while it is open — that is what pinning is for — and the
        // row unconditionally REOPENED it, spawning a second copy of every
        // terminal it had, then a third, without limit. For an open
        // project the row is a switcher, not a launcher.
        let mut project = project("chat", &["/chat", "/board-kid"], 100);
        project.anchor = Some(PathBuf::from("/chat"));
        let open = vec![
            vec![PathBuf::from("/other")],
            vec![PathBuf::from("/chat"), PathBuf::from("/board-kid")],
        ];
        assert_eq!(open_tab_for(&project, &open), Some(1));
    }

    #[test]
    fn an_open_tab_that_picked_up_another_folder_is_still_the_same_project() {
        // Matched on the ANCHOR, so the tab counts as open even after a
        // `cd` or an extra terminal in ~ — otherwise the row would decide
        // the project was closed and reopen a duplicate anyway, which is
        // the same bug wearing a different hat.
        let mut project = project("chat", &["/chat"], 100);
        project.anchor = Some(PathBuf::from("/chat"));
        let open = vec![vec![
            PathBuf::from("/chat"),
            PathBuf::from("/tmp"),
            PathBuf::from("/Users/tomas"),
        ]];
        assert_eq!(open_tab_for(&project, &open), Some(0));
    }

    #[test]
    fn a_project_that_is_not_open_resolves_to_no_tab() {
        // And the other half: a genuinely closed project must still open,
        // or the fix would trade a duplicate for a dead row.
        let mut project = project("chat", &["/chat"], 100);
        project.anchor = Some(PathBuf::from("/chat"));
        let open = vec![vec![PathBuf::from("/other"), PathBuf::from("/elsewhere")]];
        assert_eq!(open_tab_for(&project, &open), None);
        assert_eq!(open_tab_for(&project, &[]), None);
    }

    #[test]
    fn a_bare_home_starter_tab_is_not_a_project() {
        // Every launch opens a starter tab in $HOME. Recording it would put
        // "home" at the top of recents after any launch-then-quit — the
        // list filling itself with the one entry carrying no intent.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let mut store = ProjectStore::default();
        store.record(project("starter", &[&home], 100));
        assert!(
            store.projects.is_empty(),
            "a tab that only ever sat in $HOME is not a project"
        );
    }

    #[test]
    fn home_alongside_real_work_is_still_remembered() {
        // The exclusion is about a tab nobody did anything with, not about
        // $HOME being forbidden. A project spanning home AND a repo is a
        // project, and dropping half of it would be worse than the noise.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let mut store = ProjectStore::default();
        store.record(project("real", &[&home, "/work/repo"], 100));
        assert_eq!(store.projects.len(), 1);
        assert_eq!(store.projects[0].dirs.len(), 2, "both directories survive");
    }

    #[test]
    fn a_just_recorded_project_survives_even_against_future_timestamps() {
        // `last_opened` comes from the system clock. A store carrying FUTURE
        // timestamps — clock skew, a restored backup, a hand-edited file —
        // would make every new capture the "oldest" and evict it the instant
        // it was recorded, silently and permanently: the user would close a
        // project and find it had never been remembered, forever.
        let mut store = ProjectStore::default();
        for i in 0..RECENT_CAP {
            store.projects.push(project(
                &format!("old-{i}"),
                &[&format!("/p{i}")],
                1_000 + i as u64,
            ));
        }
        store.record(project("newcomer", &["/newcomer"], 5));
        assert!(
            store.projects.iter().any(|p| p.id == "newcomer"),
            "the capture the user just made must not be the one evicted"
        );
        assert_eq!(
            store.projects.iter().filter(|p| !p.pinned).count(),
            RECENT_CAP,
            "and the cap still holds"
        );
    }

    #[test]
    fn two_spellings_of_one_directory_are_one_project() {
        // macOS volumes are case-insensitive by default, and both spellings
        // genuinely occur because a shell's `cd` keeps whatever was typed.
        // Matching case-sensitively split one project into two records that
        // the user would see as duplicates of the same thing.
        let mut store = ProjectStore::default();
        store.record(project("a", &["/Users/me/Documents/proj"], 100));
        store.record(project("b", &["/Users/me/documents/proj"], 200));
        assert_eq!(store.projects.len(), 1, "one directory, one project");
        assert_eq!(store.projects[0].id, "a", "the first record keeps its id");
        assert_eq!(store.projects[0].last_opened, 200);
    }

    #[test]
    fn the_same_directory_twice_is_stored_once() {
        // Two panes in one folder are one folder. Storing it twice would
        // make reopening the project spawn two shells in the same place.
        let mut store = ProjectStore::default();
        store.record(project("a", &["/x", "/x", "/y"], 100));
        assert_eq!(
            store.projects[0].dirs,
            vec![PathBuf::from("/x"), PathBuf::from("/y")],
            "deduped, and in first-seen order"
        );
    }

    #[test]
    fn a_partial_file_fills_defaults_rather_than_being_discarded() {
        // The sibling of the corrupt-file test, and the one that actually
        // pins `#[serde(default)]`: a record written by an older build is
        // missing fields a newer one expects, and must load with defaults
        // rather than taking the whole list down with it.
        let dir = std::env::temp_dir().join(format!("st-projects-partial-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("projects.json");
        std::fs::write(
            &path,
            r#"{"projects":[{"id":"p1","label":"chat","dirs":["/chat"]}]}"#,
        )
        .unwrap();
        let store = ProjectStore::load_from(&path);
        assert_eq!(store.projects.len(), 1, "the record must survive");
        assert_eq!(store.projects[0].id, "p1");
        assert!(
            !store.projects[0].pinned,
            "absent `pinned` defaults to false"
        );
        assert!(
            !store.projects[0].renamed,
            "absent `renamed` defaults to false"
        );
        assert_eq!(store.projects[0].last_opened, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pinned_project_keeps_its_identity_but_still_tracks_when_it_was_used() {
        // The rule is that a pinned record keeps what the user made
        // theirs — its id, label and dirs — NOT that reopening it is
        // invisible to the store.
        //
        // An earlier reading froze `last_opened` too, and a review showed
        // what that costs: `pinned()` sorts on it, so the pinned list's
        // order would be stuck at pin time forever, AND every reopen would
        // leave a second, unpinned copy of the same project in recents.
        let mut store = ProjectStore::default();
        let mut original = project("pinned-1", &["/chat", "/board-kid"], 100);
        original.pinned = true;
        original.label = "chat".to_string();
        store.projects.push(original);

        store.record(project("auto-2", &["/chat", "/board-kid"], 500));

        assert_eq!(store.pinned().len(), 1);
        assert_eq!(store.pinned()[0].id, "pinned-1", "the id is the identity");
        assert_eq!(
            store.pinned()[0].label,
            "chat",
            "and the name the user gave it is not overwritten by a capture"
        );
        assert_eq!(
            store.pinned()[0].last_opened,
            500,
            "but using it must move it up the pinned list"
        );
        assert!(
            store.recent().is_empty(),
            "and it must NOT also appear as a duplicate in recents"
        );
    }

    #[test]
    fn a_renamed_project_keeps_the_name_the_user_gave_it() {
        // Same rule as pinned: the user's label survives a capture that
        // would otherwise overwrite it with a folder basename, and the
        // record is not duplicated — but reopening still counts as use.
        let mut store = ProjectStore::default();
        let mut original = project("renamed-1", &["/chat", "/board-kid"], 100);
        original.renamed = true;
        original.label = "Chat stack".to_string();
        store.projects.push(original);

        store.record(project("auto-2", &["/chat", "/board-kid"], 500));

        let recent = store.recent();
        assert_eq!(recent.len(), 1, "no duplicate of the same project");
        assert_eq!(recent[0].id, "renamed-1");
        assert_eq!(
            recent[0].label, "Chat stack",
            "an auto-capture must never rename a project back"
        );
        assert_eq!(recent[0].last_opened, 500, "but it did just get used");
    }

    #[test]
    fn record_does_not_merge_projects_rooted_in_different_folders() {
        // REWRITTEN when identity moved from the whole directory set to
        // a single anchor folder. `{/a, /b}` and `{/a, /c}`, which this
        // used to hold apart, are now ONE project — that is exactly the
        // fork the change removes. What still separates two projects is
        // being anchored somewhere else, and sharing every other folder
        // does not bring them back together.
        let mut store = ProjectStore::default();
        store.record(project("a", &["/a", "/shared"], 100));
        store.record(project("b", &["/b", "/shared"], 200));
        assert_eq!(
            store.recent().len(),
            2,
            "different anchor folders are different projects"
        );
        let mut merging = ProjectStore::default();
        merging.record(project("one", &["/a", "/b"], 100));
        merging.record(project("also-one", &["/a", "/c"], 200));
        assert_eq!(
            merging.recent().len(),
            1,
            "the same anchor folder is one project, whatever sits beside it"
        );
        assert_eq!(merging.recent()[0].id, "one");
    }

    // --- project_dirs(): what a closing tab contributes ---
    //
    // These cover the whole DECISION a capture makes. The wiring that
    // feeds them — reading each live pane's `cwd()` before its shell is
    // torn down — has no test harness (there is no gpui one, and none may
    // be introduced); it is verified by reading, and the report says so.

    fn remote() -> Target {
        Target::Remote(ProfileId("work-mac".to_string()))
    }

    fn dirs_of(panes: &[(Target, Option<&str>)]) -> Vec<PathBuf> {
        let panes: Vec<(Target, Option<String>)> = panes
            .iter()
            .map(|(t, c)| (t.clone(), c.map(String::from)))
            .collect();
        project_dirs(&panes)
    }

    #[test]
    fn a_tab_of_only_remote_panes_has_no_directories() {
        // A peer pane's directory exists on ANOTHER machine. Reopening it
        // here would spawn a local shell in a path that may not exist —
        // or, worse, does exist and is something else entirely. The cwd is
        // passed in as `Some` deliberately: the rule is the TARGET, not an
        // incidental `None` from a pane that happens not to report one.
        let dirs = dirs_of(&[
            (remote(), Some("/home/tomas/work")),
            (remote(), Some("/home/tomas/other")),
        ]);
        assert!(dirs.is_empty(), "{dirs:?}");
    }

    #[test]
    fn a_remote_pane_beside_local_ones_contributes_nothing() {
        let dirs = dirs_of(&[
            (Target::Local, Some("/chat")),
            (remote(), Some("/home/tomas/work")),
            (Target::Local, Some("/penpot")),
        ]);
        assert_eq!(
            dirs,
            vec![PathBuf::from("/chat"), PathBuf::from("/penpot")],
            "only the local panes' directories"
        );
    }

    #[test]
    fn two_panes_in_one_folder_are_one_directory() {
        // Storing it twice would make reopening spawn two shells in the
        // same place. Case-insensitively, for the same reason `dir_key`
        // is: a shell's `cd` keeps whatever spelling was typed.
        let dirs = dirs_of(&[
            (Target::Local, Some("/Users/me/Documents/chat")),
            (Target::Local, Some("/Users/me/documents/chat")),
            (Target::Local, Some("/Users/me/Documents/chat")),
        ]);
        assert_eq!(dirs, vec![PathBuf::from("/Users/me/Documents/chat")]);
    }

    #[test]
    fn directories_keep_the_order_their_panes_appear_in() {
        // Reopening spawns one terminal per directory, so the order is
        // the order the user gets their shells back in. A HashSet-shaped
        // implementation would scramble it differently on every run.
        let dirs = dirs_of(&[
            (Target::Local, Some("/chat")),
            (Target::Local, Some("/board-kid")),
            (Target::Local, Some("/penpot")),
            (Target::Local, Some("/forgejo")),
        ]);
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("/chat"),
                PathBuf::from("/board-kid"),
                PathBuf::from("/penpot"),
                PathBuf::from("/forgejo"),
            ]
        );
    }

    #[test]
    fn a_pane_that_reports_no_directory_contributes_nothing() {
        // `pid_cwd` returns `None` for a shell that never started or has
        // gone, and can hand back an EMPTY path when the syscall succeeds
        // with an empty buffer. Neither is a directory to reopen, and an
        // empty one would reopen as the filesystem root.
        let dirs = dirs_of(&[
            (Target::Local, None),
            (Target::Local, Some("")),
            (Target::Local, Some("   ")),
            (Target::Local, Some("/chat")),
        ]);
        assert_eq!(dirs, vec![PathBuf::from("/chat")]);
    }

    // --- project_for_dirs(): the record a capture builds ---

    #[test]
    fn a_tab_with_no_local_directories_is_not_worth_recording() {
        // The all-remote tab from above, carried through: there is
        // nothing to reopen, so nothing is written down.
        assert_eq!(project_for_dirs(Vec::new(), 1_000), None);
    }

    #[test]
    fn the_default_label_is_the_anchors_basename() {
        // REWRITTEN when the label stopped coming from `dirs[0]`. A
        // basename is a fine default for one directory and useless for
        // four — "chat" is not derivable from those four paths — so the
        // folder the project IS names it until the user renames it.
        let captured = project_for_dirs(
            vec![PathBuf::from("/a/chat"), PathBuf::from("/b/penpot")],
            42,
        )
        .expect("dirs present");
        assert_eq!(captured.label, "chat");
        assert_eq!(captured.last_opened, 42);
        assert!(!captured.pinned);
        assert!(
            !captured.renamed,
            "an auto-capture is never the user's name"
        );
    }

    #[test]
    fn a_project_whose_first_pane_sits_in_home_is_not_called_home() {
        // The defect the anchor label fixes. `dirs[0]` is pane order, and
        // a tab whose first pane never left `$HOME` handed the project the
        // one basename that carries no intent — "tomas" — for a project
        // that is plainly the repo beside it.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let home_name = Path::new(&home)
            .file_name()
            .expect("$HOME has a basename")
            .to_string_lossy()
            .to_string();
        let captured =
            project_for_dirs(vec![PathBuf::from(&home), PathBuf::from("/work/chat")], 42)
                .expect("dirs present");
        assert_eq!(captured.label, "chat", "the anchor names it");
        assert_ne!(captured.label, home_name, "never the home folder");
        assert_eq!(captured.anchor, Some(PathBuf::from("/work/chat")));
    }

    #[test]
    fn one_project_gets_one_id_however_its_panes_were_ordered() {
        // The store matches on the PRIMARY directory, so a capture's id
        // only has to be unique — but deriving it from the whole set means
        // a project whose store file was lost comes back under the id it
        // always had, rather than a fresh one each launch. A project that
        // still exists is matched and keeps its stored id either way.
        let a = project_for_dirs(vec![PathBuf::from("/chat"), PathBuf::from("/penpot")], 1)
            .expect("dirs present");
        let b = project_for_dirs(vec![PathBuf::from("/Penpot"), PathBuf::from("/chat")], 2)
            .expect("dirs present");
        assert_eq!(a.id, b.id, "same directory set, same id");
        let other = project_for_dirs(vec![PathBuf::from("/chat")], 3).expect("dirs present");
        assert_ne!(a.id, other.id, "a different set mints a different id");
        assert!(!a.id.is_empty());
    }

    #[test]
    fn a_captured_project_records_into_the_store_it_was_built_for() {
        // The seam between the two halves of a capture: whatever
        // `project_dirs` decides is exactly what `record` stores, in the
        // same order, under the same label.
        let dirs = dirs_of(&[
            (Target::Local, Some("/chat")),
            (remote(), Some("/home/tomas/work")),
            (Target::Local, Some("/chat")),
            (Target::Local, Some("/penpot")),
        ]);
        let captured = project_for_dirs(dirs, 77).expect("local dirs present");
        let mut store = ProjectStore::default();
        store.record(captured);
        let recent = store.recent();
        assert_eq!(recent.len(), 1);
        assert_eq!(
            recent[0].dirs,
            vec![PathBuf::from("/chat"), PathBuf::from("/penpot")]
        );
        assert_eq!(recent[0].label, "chat");
        assert_eq!(recent[0].last_opened, 77);
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    fn project(id: &str, dirs: &[&str], last_opened: u64) -> Project {
        Project {
            id: id.to_string(),
            label: id.to_string(),
            dirs: dirs.iter().map(PathBuf::from).collect(),
            // Left unset on purpose: a hand-built record is the shape a
            // record written before the field had, so these tests exercise
            // the derive-on-the-way-in path as well as the rules they name.
            anchor: None,
            pinned: false,
            last_opened,
            icon: ProjectIcon::default(),
            renamed: false,
            terminals: 0,
            active_secs: 0,
        }
    }

    // --- how a row states what the project is ---

    #[test]
    fn folder_and_terminal_counts_read_as_none_one_or_many() {
        // "1 folders" is the tell that nobody looked at the row. Zero is
        // its own word rather than a bare 0, which reads as a missing
        // value instead of a stated one.
        assert_eq!(count_label(0, "folder"), "no folders");
        assert_eq!(count_label(1, "folder"), "1 folder");
        assert_eq!(count_label(4, "folder"), "4 folders");
        assert_eq!(count_label(0, "terminal"), "no terminals");
        assert_eq!(count_label(1, "terminal"), "1 terminal");
        assert_eq!(count_label(6, "terminal"), "6 terminals");
    }

    #[test]
    fn a_sub_minute_project_does_not_read_as_zero_hours() {
        // The whole point of the duration is "how much have I worked
        // here". "0h" answers that with a number that looks like a bug.
        assert_eq!(duration_label(1), "under a minute");
        assert_eq!(duration_label(59), "under a minute");
        assert_eq!(duration_label(60), "1m");
        assert_eq!(duration_label(3_599), "59m");
        for secs in [1, 59, 60, 3_599] {
            assert!(
                !duration_label(secs).contains('h'),
                "{secs}s must not claim hours: {}",
                duration_label(secs)
            );
        }
        assert_eq!(duration_label(3_600), "1h");
        assert_eq!(duration_label(3_660), "1h 1m");
        assert_eq!(duration_label(86_400), "1d");
        assert_eq!(duration_label(90_000), "1d 1h");
    }

    #[test]
    fn a_project_with_no_recorded_time_says_nothing_about_time() {
        // A record written before `active_secs` existed, or one whose only
        // session never closed cleanly, has NO time to report. Saying
        // "under a minute" would invent a measurement that was never made.
        let summary = project_summary(1, 0);
        assert_eq!(summary, "1 folder");
        assert!(!summary.contains("minute"));
    }

    #[test]
    fn the_summary_states_folders_and_time_but_not_terminals() {
        // The terminal count used to be here and cost the row its name:
        // the sidebar is narrow, so "SuperTerminal - 1 terminal" truncated
        // to "SuperTermin 1 terminal", the count eating the only part a
        // user scans for.
        let summary = project_summary(4, 11_520);
        assert_eq!(summary, "4 folders \u{b7} 3h 12m");
        assert!(
            !summary.contains("terminal"),
            "the terminal count must not come back: {summary}"
        );
    }

    // --- time open ---

    #[test]
    fn time_open_cannot_run_backwards() {
        // `last_opened` and the mark both come from the system clock, and a
        // clock that steps backwards (NTP, a sleep/wake) would otherwise
        // wrap into ~584 billion years of "work".
        assert_eq!(session_secs(100, 160), 60);
        assert_eq!(session_secs(100, 100), 0);
        assert_eq!(session_secs(100, 90), 0);
    }

    #[test]
    fn a_project_captured_twice_accumulates_its_time() {
        // "how much have I actually worked here", summed across sessions —
        // not "how long was the last session", which overwriting gives.
        // The second capture drops the incidental second folder rather
        // than reordering it, which is how a real second session differs
        // and what `same_project` now matches on. The assertion below is
        // untouched: the same 150 seconds, in the same one project.
        let mut store = ProjectStore::default();
        let mut first = project("chat", &["/chat", "/penpot"], 100);
        first.active_secs = 100;
        store.record(first);
        let mut second = project("chat-again", &["/chat"], 200);
        second.active_secs = 50;
        store.record(second);
        assert_eq!(store.recent().len(), 1, "still one project");
        assert_eq!(store.recent()[0].active_secs, 150, "summed, not replaced");
    }

    #[test]
    fn a_pinned_project_accumulates_time_like_any_other() {
        // Time worked is a measurement, not something the user made
        // theirs — the rule that protects a pinned record's id, label and
        // dirs has nothing to say about it.
        let mut store = ProjectStore::default();
        let mut original = project("pinned-1", &["/chat"], 100);
        original.pinned = true;
        original.active_secs = 600;
        store.projects.push(original);
        let mut capture = project("auto-2", &["/chat"], 200);
        capture.active_secs = 60;
        store.record(capture);
        assert_eq!(store.pinned().len(), 1);
        assert_eq!(store.pinned()[0].active_secs, 660);
    }

    #[test]
    fn a_capture_states_how_many_terminals_the_project_had() {
        // Latest wins: the count answers "how big is this thing" for the
        // shape it was in when it was last closed, not the first time.
        let mut store = ProjectStore::default();
        let mut first = project("chat", &["/chat"], 100);
        first.terminals = 2;
        store.record(first);
        let mut second = project("chat-again", &["/chat"], 200);
        second.terminals = 5;
        store.record(second);
        assert_eq!(store.recent()[0].terminals, 5);
    }

    // --- reopening ---

    #[test]
    fn reopening_a_pinned_project_does_not_leave_a_copy_in_recents() {
        // The reopen path records the project it just opened, so "you just
        // used this" survives a crash. That record must merge into the
        // pinned one, not sit beneath it as an unpinned twin of itself.
        let mut store = ProjectStore::default();
        let mut original = project("pinned-1", &["/chat", "/board-kid"], 100);
        original.pinned = true;
        original.label = "Chat stack".to_string();
        original.active_secs = 600;
        store.projects.push(original.clone());

        store.record(touch_for_reopen(&original, 500, 2));

        assert_eq!(store.pinned().len(), 1);
        assert!(
            store.recent().is_empty(),
            "reopening must not create a recent entry"
        );
        assert_eq!(store.pinned()[0].id, "pinned-1");
        assert_eq!(store.pinned()[0].label, "Chat stack");
        assert_eq!(store.pinned()[0].last_opened, 500, "it did just get used");
        assert_eq!(
            store.pinned()[0].active_secs,
            600,
            "opening it adds no worked time on its own"
        );
        assert_eq!(store.pinned()[0].terminals, 2);
    }

    #[test]
    fn a_missing_folder_falls_back_to_home_and_the_others_still_open() {
        // Refusing the whole project because one of four folders was
        // deleted would lose the three that are still there.
        let dirs = [
            PathBuf::from("/chat"),
            PathBuf::from("/gone"),
            PathBuf::from("/penpot"),
            PathBuf::from("/forgejo"),
        ];
        let plan = plan_reopen(&dirs, |p| p != Path::new("/gone"));
        assert_eq!(plan.spawns.len(), 4, "one terminal per remembered folder");
        assert_eq!(plan.spawns[0], Some(PathBuf::from("/chat")));
        assert_eq!(plan.spawns[1], None, "the gone one falls back to $HOME");
        assert_eq!(plan.spawns[2], Some(PathBuf::from("/penpot")));
        assert_eq!(plan.spawns[3], Some(PathBuf::from("/forgejo")));
        assert_eq!(plan.missing, vec![PathBuf::from("/gone")]);
    }

    #[test]
    fn a_project_whose_folders_are_all_gone_still_opens() {
        let dirs = [PathBuf::from("/gone-a"), PathBuf::from("/gone-b")];
        let plan = plan_reopen(&dirs, |_| false);
        assert_eq!(plan.spawns, vec![None, None], "two shells, both in $HOME");
        assert_eq!(plan.missing.len(), 2, "and both are named in the note");
    }

    #[test]
    fn a_note_names_the_folders_that_are_gone() {
        // "Failing silently" is the thing the spec forbids: the user must
        // be told WHICH folder they are not in.
        assert_eq!(missing_dirs_note(&[]), None, "nothing gone, nothing said");
        let one = missing_dirs_note(&[PathBuf::from("/gone")]).expect("a note");
        assert!(one.contains("/gone"), "{one}");
        assert!(one.contains('~'), "and where it opened instead: {one}");
        assert!(one.contains("1 folder"), "{one}");
        let two = missing_dirs_note(&[PathBuf::from("/gone-a"), PathBuf::from("/gone-b")])
            .expect("a note");
        assert!(two.contains("2 folders"), "{two}");
        assert!(two.contains("/gone-a") && two.contains("/gone-b"), "{two}");
    }
}

/// The three decisions behind "closing a project pane by pane, reopening
/// one whose folder is gone, and listing it once rather than twice".
///
/// The wiring that feeds these — folding every live pane's `last_known_cwd`
/// into `Workspace::tab_pane_dirs` before anything is torn down, pruning
/// that map with the tab, and seeding it in `open_project` — has no test
/// harness (there is no gpui one, and none may be introduced). It is
/// verified by reading, and the report says which paths.
#[cfg(test)]
mod tab_memory_tests {
    use super::*;
    use crate::hosts::{ProfileId, Target};

    fn remote() -> Target {
        Target::Remote(ProfileId("work-mac".to_string()))
    }

    fn project(id: &str, dirs: &[&str], last_opened: u64, pinned: bool) -> Project {
        Project {
            id: id.to_string(),
            label: id.to_string(),
            dirs: dirs.iter().map(PathBuf::from).collect(),
            anchor: None,
            pinned,
            last_opened,
            icon: ProjectIcon::default(),
            renamed: false,
            terminals: 0,
            active_secs: 0,
        }
    }

    /// One fold of `Workspace::remember_pane_dirs`: the panes that are
    /// still alive, each reporting where its shell is.
    fn fold(remembered: &mut TabPaneDirs, live: &[(&str, Option<&str>)]) {
        for (pane_id, cwd) in live {
            remembered.saw(pane_id, &Target::Local, cwd.map(String::from));
        }
    }

    // --- a project closed one pane at a time keeps every folder ---

    #[test]
    fn a_project_closed_one_pane_at_a_time_keeps_every_folder() {
        // THE defect. A tab dies when its LAST terminal goes, so a capture
        // that reads whoever is still alive sees one pane and records a
        // four-folder project as a one-folder one — reopened, the user
        // gets a single shell back. Closing panes individually is entirely
        // ordinary, so reading the survivors defeats the feature outright.
        let mut remembered = TabPaneDirs::default();
        fold(
            &mut remembered,
            &[
                ("p1", Some("/chat")),
                ("p2", Some("/board-kid")),
                ("p3", Some("/penpot")),
                ("p4", Some("/forgejo")),
            ],
        );
        // Three panes closed, one at a time. Each later fold sees only who
        // is left, exactly as the workspace's does.
        fold(
            &mut remembered,
            &[
                ("p2", Some("/board-kid")),
                ("p3", Some("/penpot")),
                ("p4", Some("/forgejo")),
            ],
        );
        fold(
            &mut remembered,
            &[("p3", Some("/penpot")), ("p4", Some("/forgejo"))],
        );
        fold(&mut remembered, &[("p4", Some("/forgejo"))]);
        assert_eq!(
            remembered.dirs(),
            vec![
                PathBuf::from("/chat"),
                PathBuf::from("/board-kid"),
                PathBuf::from("/penpot"),
                PathBuf::from("/forgejo"),
            ],
            "all four folders, in the order their panes first appeared"
        );
        assert_eq!(
            remembered.terminals(),
            4,
            "and four terminals, not the one that happened to be last"
        );
    }

    #[test]
    fn a_shell_that_cds_around_contributes_one_folder() {
        // The union is over PANES, never over time. A terminal that walks
        // a repo all day is still one terminal in one place — the place it
        // ended up. Accumulating every directory it ever visited would
        // reopen the project as a dozen shells and grow without bound.
        let mut remembered = TabPaneDirs::default();
        fold(&mut remembered, &[("p1", Some("/chat"))]);
        fold(&mut remembered, &[("p1", Some("/chat/src"))]);
        fold(&mut remembered, &[("p1", Some("/chat/src/net"))]);
        assert_eq!(
            remembered.dirs(),
            vec![PathBuf::from("/chat/src/net")],
            "where it is now, not everywhere it has been"
        );
        assert_eq!(remembered.terminals(), 1, "still one terminal");
    }

    #[test]
    fn a_pane_that_stops_answering_keeps_where_it_was() {
        // Typing `exit` is the commonest way to close a terminal and
        // leaves no process to ask. Letting that `None` erase the folder
        // would undo the very thing `last_known_cwd` exists for.
        let mut remembered = TabPaneDirs::default();
        fold(&mut remembered, &[("p1", Some("/chat"))]);
        fold(&mut remembered, &[("p1", None)]);
        assert_eq!(remembered.dirs(), vec![PathBuf::from("/chat")]);
    }

    #[test]
    fn a_remote_pane_is_a_terminal_the_project_had_but_not_a_folder() {
        // Its directory is on ANOTHER machine, so reopening it here would
        // spawn a local shell in a path that may not exist — but it was
        // still a terminal in this project, and the count says so.
        let mut remembered = TabPaneDirs::default();
        remembered.saw("p1", &Target::Local, Some("/chat".to_string()));
        remembered.saw("p2", &remote(), Some("/home/tomas/work".to_string()));
        assert_eq!(
            remembered.dirs(),
            vec![PathBuf::from("/chat")],
            "only the local pane's folder"
        );
        assert_eq!(remembered.terminals(), 2, "both were terminals");
    }

    // --- a folder that has gone does not fork the project ---

    #[test]
    fn a_folder_that_was_gone_still_counts_as_itself() {
        // The reopened shell for a deleted folder sits in `$HOME`. Letting
        // that stand makes the captured directory SET differ from the
        // project's own — and identity is matched on that set, so the
        // project splits into a second record the moment it is reopened.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let mut remembered = TabPaneDirs::default();
        remembered.asked_for("p1", &Target::Local, PathBuf::from("/gone"));
        remembered.saw("p1", &Target::Local, Some(home.clone()));
        assert_eq!(
            remembered.dirs(),
            vec![PathBuf::from("/gone")],
            "the folder it was asked for, not the fallback it landed in"
        );
    }

    #[test]
    fn a_shell_moved_off_the_fallback_reports_where_it_actually_is() {
        // The asked-for folder stands only while the shell is still
        // sitting in the fallback. Once the user takes that terminal
        // somewhere real, "live cwd, not spawn cwd" is the rule again.
        let mut remembered = TabPaneDirs::default();
        remembered.asked_for("p1", &Target::Local, PathBuf::from("/gone"));
        remembered.saw("p1", &Target::Local, Some("/chat".to_string()));
        assert_eq!(remembered.dirs(), vec![PathBuf::from("/chat")]);
    }

    #[test]
    fn reopening_a_project_with_a_missing_folder_does_not_fork_the_record() {
        // PTY spawn/teardown is process-global; see `term_session::PTY_TEST_LOCK`.
        let _pty = crate::term_session::pty_test_guard();
        // End to end over the decisions `open_project` and the capture use
        // between them: plan the reopen, seed the tab's memory the way
        // `open_project` does, let the fallback shell report `$HOME`, then
        // capture. One record must come back out, not two.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let dirs = vec![
            PathBuf::from("/chat"),
            PathBuf::from("/gone"),
            PathBuf::from("/penpot"),
        ];
        let mut store = ProjectStore::default();
        store.record(project("orig", &["/chat", "/gone", "/penpot"], 100, false));

        let plan = plan_reopen(&dirs, |dir| dir != Path::new("/gone"));
        let mut remembered = TabPaneDirs::default();
        for (index, (spawn, wanted)) in plan.spawns.iter().zip(dirs.iter()).enumerate() {
            let pane_id = format!("p{index}");
            remembered.saw(&pane_id, &Target::Local, None);
            if spawn.is_none() {
                remembered.asked_for(&pane_id, &Target::Local, wanted.clone());
            }
            // What each shell reports once it is up: its folder, or the
            // `$HOME` `TermSession::spawn` falls back to.
            let landed = spawn
                .clone()
                .map(|dir| dir.to_string_lossy().to_string())
                .unwrap_or_else(|| home.clone());
            remembered.saw(&pane_id, &Target::Local, Some(landed));
        }

        let captured =
            project_for_dirs(remembered.dirs(), 200).expect("the reopened tab is a project");
        store.record(captured);
        assert_eq!(
            store.recent().len(),
            1,
            "one project, not one plus a $HOME-shaped twin"
        );
        assert_eq!(store.recent()[0].id, "orig", "and it is the same record");
        assert_eq!(
            store.recent()[0].dirs,
            dirs,
            "with the folder that is temporarily missing still in it"
        );
    }

    // --- what makes two captures the same project ---

    #[test]
    fn an_incidental_terminal_in_home_does_not_fork_a_project() {
        // The case that forced this rule. One tab, two terminals: the repo,
        // and a second one sitting in `~` to run `brew upgrade`. Closing it
        // records `{repo, ~}`; closing it tomorrow without that second
        // terminal records `{repo}`. Under whole-set identity those are two
        // projects, and recents fill with near-duplicates that differ only
        // by which side-terminal happened to be open at close time.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let mut store = ProjectStore::default();
        store.record(project("repo", &["/repo", &home], 100, false));
        // And again with the home terminal opened FIRST, which is what
        // makes "the first non-$HOME directory" different from "the first
        // directory".
        store.record(project("would-be-a-twin", &[&home, "/repo"], 150, false));
        store.record(project("would-be-another", &["/repo"], 200, false));
        assert_eq!(store.recent().len(), 1, "one project, not three");
        assert_eq!(store.recent()[0].id, "repo", "and it is the first record");
        assert_eq!(store.recent()[0].last_opened, 200, "which just got used");
    }

    #[test]
    fn an_incidental_terminal_anywhere_does_not_fork_a_project() {
        // The same terminal `cd`ed out of `~` and into `/tmp` forked the
        // record a SECOND time under whole-set identity. The folders
        // beside the anchor one are free to be anything.
        let mut store = ProjectStore::default();
        store.record(project("repo", &["/repo", "/tmp"], 100, false));
        store.record(project("would-be-a-twin", &["/repo"], 200, false));
        assert_eq!(store.recent().len(), 1, "still one project");
        assert_eq!(store.recent()[0].id, "repo");
    }

    #[test]
    fn a_tab_that_only_ever_sat_in_home_is_still_not_a_project() {
        // Excluding `$HOME` must not fall back to it when there is no
        // other candidate — every launch opens a starter tab there, and a
        // `$HOME` anchor would make every one of them the same project.
        // Nor may asking such a capture for its anchor panic.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let home_only = [PathBuf::from(&home)];
        assert_eq!(anchor_dir(&home_only), None, "$HOME is never an anchor");
        assert_eq!(anchor_dir(&[]), None, "nor is nothing at all");
        let mut store = ProjectStore::default();
        store.record(project("starter", &[&home], 100, false));
        store.record(project("another-starter", &[&home], 200, false));
        assert!(
            store.recent().is_empty(),
            "a bare home tab is recorded no more than it ever was"
        );
    }

    #[test]
    fn two_projects_rooted_in_different_folders_stay_apart() {
        // Sharing every folder BUT the anchor one is not sharing an
        // identity — otherwise a scratch folder both projects happen to
        // open would silently merge them.
        let mut store = ProjectStore::default();
        store.record(project("a", &["/a", "/shared"], 100, false));
        store.record(project("b", &["/b", "/shared"], 200, false));
        let ids: Vec<&str> = store.recent().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"], "two projects, newest first");
    }

    #[test]
    fn a_pinned_project_matched_by_its_anchor_keeps_what_is_its_own() {
        // REWRITTEN when the anchor stopped being "the first non-$HOME
        // directory": under that rule `{/repo, /docs}` was anchored at
        // /repo because /repo came first, and this test leaned on it. The
        // set derives /docs now (same depth, lower key), so the folders
        // here say what they mean — /repo is the ancestor, and the
        // shallowest path wins.
        //
        // What is being asserted is unchanged: matching more loosely must
        // not loosen what a pinned or renamed record protects. Its id, its
        // label and its folders are still the user's, and only "you just
        // used this" moves.
        let home = std::env::var("HOME").expect("HOME is set in this environment");
        let mut store = ProjectStore::default();
        let mut fav = project("fav-1", &["/repo", "/repo/docs"], 100, true);
        fav.label = "Chat stack".to_string();
        fav.renamed = true;
        store.record(fav);
        store.record(project("auto-capture", &["/repo", &home], 500, false));
        assert_eq!(store.pinned().len(), 1, "no unpinned twin appeared");
        assert!(store.recent().is_empty());
        let kept = store.pinned()[0];
        assert_eq!(kept.id, "fav-1", "its id is its own");
        assert_eq!(kept.label, "Chat stack", "so is the name the user gave it");
        assert_eq!(
            kept.dirs,
            vec![PathBuf::from("/repo"), PathBuf::from("/repo/docs")],
            "and so are its folders — the capture's $HOME did not get in"
        );
        assert_eq!(kept.last_opened, 500, "but it did just get used");
    }

    // --- a project that is open is listed once, not twice ---

    #[test]
    fn a_project_that_is_open_is_not_also_listed_under_recent() {
        // It is the live tab above. Offering it again under RECENT shows
        // the same thing twice and invites the user to "reopen" what they
        // are already looking at.
        let mut store = ProjectStore::default();
        store.record(project("open", &["/chat", "/penpot"], 200, false));
        store.record(project("closed", &["/other"], 100, false));
        // The live tab has since picked up a terminal in /tmp and lost the
        // one in /penpot. Same anchor folder, so it is the same project.
        let open = vec![vec![PathBuf::from("/chat"), PathBuf::from("/tmp")]];
        let sections = sidebar_sections(&store, &open);
        let ids: Vec<&str> = sections.recent.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["closed"], "the open one is the tab above");
    }

    #[test]
    fn an_open_project_is_listed_once_even_when_pinned() {
        // An earlier version kept a pinned project in PINNED while it was
        // open, arguing that hiding it would make pinning look broken.
        // Using it proved otherwise: the project appears in the live tab
        // list above AND under PINNED, and the second copy reads as a
        // duplicate nobody can explain.
        //
        // Pinning promises the project is there when you come BACK, not
        // that it is listed twice while you are already in it.
        let mut store = ProjectStore::default();
        store.record(project("fav", &["/chat"], 200, true));
        let open = vec![vec![PathBuf::from("/chat")]];
        let sections = sidebar_sections(&store, &open);
        assert!(
            sections.pinned.is_empty(),
            "an open project is already on screen as a live tab"
        );
        assert!(sections.recent.is_empty(), "and pinned is never in recents");
    }

    #[test]
    fn a_pinned_project_comes_back_the_moment_it_is_closed() {
        // The other half, and the one that makes hiding it safe: nothing
        // was forgotten, it was only not shown twice.
        let mut store = ProjectStore::default();
        store.record(project("fav", &["/chat"], 200, true));
        let sections = sidebar_sections(&store, &[]);
        let ids: Vec<&str> = sections.pinned.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["fav"], "closed, so it is listed again");
    }

    #[test]
    fn a_tab_rooted_elsewhere_hides_nothing_however_much_it_shares() {
        // Matched on the PRIMARY directory, the way `record` matches. A
        // tab rooted in another folder is another project, and sharing
        // every later folder with a remembered one must not make that one
        // vanish from the list.
        let mut store = ProjectStore::default();
        store.record(project("mine", &["/chat", "/shared"], 200, false));
        let open = vec![vec![PathBuf::from("/other"), PathBuf::from("/shared")]];
        let sections = sidebar_sections(&store, &open);
        let ids: Vec<&str> = sections.recent.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["mine"], "a shared folder is not shared identity");
    }
}

/// What a project IS: the one folder it is anchored to, derived from the
/// set of folders it was first captured with and frozen there.
///
/// Two rules, tested apart from each other: [`anchor_dir`] chooses (and
/// must not read pane order), and `ProjectStore` keeps (and must not
/// re-derive). Both are pure — no gpui harness is involved or needed.
#[cfg(test)]
mod anchor_tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("st-native-anchor-{}-{}", std::process::id(), name))
    }

    fn dirs(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn capture(paths: &[&str], now: u64) -> Project {
        project_for_dirs(dirs(paths), now).expect("dirs present")
    }

    fn home() -> String {
        std::env::var("HOME").expect("HOME is set in this environment")
    }

    // --- choosing: over the SET, never over pane order ---

    #[test]
    fn the_shallowest_folder_anchors_the_project_in_whatever_order_it_arrives() {
        // `dirs` comes out of the split tree, so its order is LAYOUT.
        // Rebalancing splits, closing and reopening a pane, or restoring a
        // tab differently all reorder it — and under "the first non-$HOME
        // directory" every one of those silently changed which project
        // this was. A project's own folder is the ANCESTOR of the folders
        // its terminals wander into, so the shallowest path is the one.
        let repo = PathBuf::from("/repo");
        assert_eq!(anchor_dir(&dirs(&["/repo/native", "/repo"])), Some(&repo));
        assert_eq!(anchor_dir(&dirs(&["/repo", "/repo/native"])), Some(&repo));

        // Depth decides BEFORE the key does, and the two genuinely
        // disagree: `/apps/thing` sorts first alphabetically, `/zed` is the
        // shallower path. Ancestor pairs like the one above cannot show
        // this — a prefix sorts ahead of the longer path anyway, so the
        // tie-break alone would answer them correctly and "shallowest"
        // would be untested.
        let zed = PathBuf::from("/zed");
        assert_eq!(anchor_dir(&dirs(&["/apps/thing", "/zed"])), Some(&zed));
        assert_eq!(anchor_dir(&dirs(&["/zed", "/apps/thing"])), Some(&zed));

        let chat = PathBuf::from("/chat");
        for order in [
            ["/chat/packages/foo", "/chat/packages", "/chat"],
            ["/chat", "/chat/packages/foo", "/chat/packages"],
            ["/chat/packages", "/chat", "/chat/packages/foo"],
        ] {
            assert_eq!(
                anchor_dir(&dirs(&order)),
                Some(&chat),
                "order must not decide: {order:?}"
            );
        }
    }

    #[test]
    fn two_folders_at_one_depth_break_their_tie_the_same_way_in_any_order() {
        // A tie has to be settled by something stated, or the set's order
        // leaks back in through the back door.
        let alpha = PathBuf::from("/alpha");
        assert_eq!(anchor_dir(&dirs(&["/beta", "/alpha"])), Some(&alpha));
        assert_eq!(anchor_dir(&dirs(&["/alpha", "/beta"])), Some(&alpha));

        // On `dir_key` — the same case-insensitive key everything else here
        // matches on — not on raw bytes, where '/B' sorts before '/a' and
        // the two rules disagree.
        let lower = PathBuf::from("/alpha");
        assert_eq!(
            anchor_dir(&dirs(&["/Beta", "/alpha"])),
            Some(&lower),
            "ranked case-insensitively, not by byte order"
        );
        assert_eq!(anchor_dir(&dirs(&["/alpha", "/Beta"])), Some(&lower));
    }

    #[test]
    fn home_is_excluded_before_depth_is_ever_considered() {
        // $HOME is usually the shallowest path in the set, so a rule that
        // ranked before it excluded would anchor half the user's projects
        // to their home folder — and every launch opens a starter tab
        // there, which would make all of them one project.
        let home = home();
        let deep = PathBuf::from("/work/chat/packages/foo");
        assert_eq!(
            anchor_dir(&dirs(&[&home, "/work/chat/packages/foo"])),
            Some(&deep),
            "$HOME is shallower and still not the anchor"
        );
        assert_eq!(
            anchor_dir(&[PathBuf::from(&home)]),
            None,
            "$HOME alone anchors nothing"
        );
        assert_eq!(anchor_dir(&[]), None, "nor does nothing at all");
    }

    // --- keeping: the stored anchor decides, and never moves ---

    #[test]
    fn a_repo_alone_beside_home_or_beside_a_scratch_folder_is_one_project() {
        // The three shapes one project takes across three days: the repo
        // on its own, the repo with a terminal left in `~`, and the repo
        // with one in /tmp. One record, not three.
        //
        // /tmp is the interesting one: it is SHALLOWER than the repo, so a
        // freshly derived anchor would name it — the incidental terminal
        // stealing the project's identity. The stored anchor is what stops
        // that.
        let home = home();
        let mut store = ProjectStore::default();
        store.record(capture(&["/work/repo"], 100));
        store.record(capture(&["/work/repo", &home], 200));
        store.record(capture(&["/work/repo", "/tmp"], 300));
        assert_eq!(store.projects.len(), 1, "{:?}", store.projects);
        assert_eq!(
            store.projects[0].anchor,
            Some(PathBuf::from("/work/repo")),
            "still the folder it was first captured as"
        );
        assert_eq!(store.projects[0].last_opened, 300);
    }

    #[test]
    fn adding_a_shallower_folder_does_not_re_anchor_the_project() {
        // The failure a per-capture derivation hides. Open one terminal at
        // the top of the repo you have been working inside, and the newly
        // derived anchor is that shallower folder — so the record answers
        // to a different folder from tomorrow on, and the project as it
        // usually looks no longer finds it. It forks, losing its pin, its
        // name and its accumulated time.
        let mut store = ProjectStore::default();
        store.record(capture(&["/repo/native"], 100));
        assert_eq!(
            store.projects[0].anchor,
            Some(PathBuf::from("/repo/native")),
            "first capture settles it"
        );

        store.record(capture(&["/repo/native", "/repo"], 200));
        assert_eq!(store.projects.len(), 1, "still one project");
        assert_eq!(
            store.projects[0].anchor,
            Some(PathBuf::from("/repo/native")),
            "a later capture must not move the anchor"
        );
        assert_eq!(
            store.projects[0].dirs,
            dirs(&["/repo/native", "/repo"]),
            "its folders DO move — only the anchor is frozen"
        );

        // And the point of freezing it: the plain project, captured
        // tomorrow, still finds its own record.
        store.record(capture(&["/repo/native"], 300));
        assert_eq!(store.projects.len(), 1, "no fork");
        assert_eq!(store.projects[0].last_opened, 300);
    }

    #[test]
    fn a_reopen_finds_the_record_it_came_from_by_its_stored_anchor() {
        // A record can hold folders whose derived anchor is NOT its own —
        // that is exactly what freezing the anchor produces. Matching has
        // to ask "is this record's anchor still open", not "do the two
        // sets derive the same anchor", or reopening a project would fork
        // the very record it was opened from.
        let mut store = ProjectStore::default();
        store.record(capture(&["/repo/native"], 100));
        store.record(capture(&["/repo/native", "/repo"], 200));
        let stored = store.recent()[0].clone();
        assert_eq!(stored.anchor, Some(PathBuf::from("/repo/native")));
        assert_eq!(
            anchor_dir(&stored.dirs),
            Some(&PathBuf::from("/repo")),
            "the two genuinely differ, which is what makes this a test"
        );

        store.record(touch_for_reopen(&stored, 300, 2));
        assert_eq!(store.projects.len(), 1, "reopening must not fork it");
        assert_eq!(store.projects[0].last_opened, 300);
    }

    #[test]
    fn two_projects_with_different_anchors_stay_apart_even_sharing_folders() {
        // A scratch folder both projects happen to keep open is not a
        // shared identity. Only the anchor is.
        let mut store = ProjectStore::default();
        store.record(capture(&["/work/chat", "/work/shared"], 100));
        store.record(capture(&["/work/penpot", "/work/shared"], 200));
        assert_eq!(store.projects.len(), 2, "{:?}", store.projects);
        assert_eq!(store.projects[0].anchor, Some(PathBuf::from("/work/chat")));
        assert_eq!(
            store.projects[1].anchor,
            Some(PathBuf::from("/work/penpot"))
        );

        // And each one still finds itself rather than the other.
        store.record(capture(&["/work/chat"], 300));
        assert_eq!(store.projects.len(), 2, "no third record");
        assert_eq!(store.projects[0].last_opened, 300);
        assert_eq!(store.projects[1].last_opened, 200, "the other is untouched");
    }

    #[test]
    fn a_capture_holding_two_anchors_picks_the_same_record_every_time() {
        // Both records qualify, so the tie needs a rule: the record
        // anchored at the capture's OWN derived anchor wins. Storage order
        // would answer it too, and answer it arbitrarily — insertion order
        // says nothing about which project the user is in.
        let mut store = ProjectStore::default();
        store.record(capture(&["/work/alpha"], 100));
        store.record(capture(&["/beta"], 200));
        // /beta is shallower, so it is what this capture derives.
        store.record(capture(&["/work/alpha", "/beta"], 300));
        assert_eq!(store.projects.len(), 2, "no third record");
        assert_eq!(
            store.projects[0].last_opened, 100,
            "the record it did NOT derive is untouched"
        );
        assert_eq!(
            store.projects[1].last_opened, 300,
            "the derived one matched"
        );
    }

    // --- records written before the field existed ---

    #[test]
    fn a_record_written_before_anchors_gets_one_when_it_loads() {
        // `projects.json` on disk today has no `anchor`. Such a record must
        // not be orphaned (matching nothing, so every capture forks it) and
        // must not be dropped (that is the user's whole recents list). It
        // is given the anchor its own folders derive, once, on the way in.
        let dir = tmp("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.json");
        std::fs::write(
            &path,
            r#"{"projects":[{"id":"p1","label":"native","dirs":["/repo/native","/repo"],"lastOpened":5}]}"#,
        )
        .unwrap();

        let mut store = ProjectStore::load_from(&path);
        assert_eq!(store.projects.len(), 1, "the record must survive");
        assert_eq!(
            store.projects[0].anchor,
            Some(PathBuf::from("/repo")),
            "derived from the dirs it does have, not left empty"
        );

        // And it is a real identity, not a decoration: the next capture of
        // that project updates it instead of adding a twin.
        store.record(capture(&["/repo", "/repo/native"], 9));
        assert_eq!(store.projects.len(), 1, "no twin");
        assert_eq!(store.projects[0].id, "p1", "it kept its own id");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_home_only_record_has_no_anchor_and_asking_does_not_panic() {
        // Nothing writes one — a bare `$HOME` tab is not worth
        // remembering — but a hand-edited or hand-copied file can hold
        // one, and load, matching and the sidebar all have to survive it.
        let home = home();
        let mut store = ProjectStore::default();
        store.record(capture(&[&home], 100));
        assert!(
            store.projects.is_empty(),
            "a bare $HOME tab is still not a project"
        );

        let dir = tmp("home-only");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("projects.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"projects":[{{"id":"h1","label":"home","dirs":[{home:?}],"lastOpened":5}}]}}"#
            ),
        )
        .unwrap();
        let store = ProjectStore::load_from(&path);
        assert_eq!(store.projects.len(), 1, "loaded, not dropped");
        assert_eq!(
            store.projects[0].anchor, None,
            "there is no folder to anchor it to"
        );
        // It matches nothing, itself included — so the open tab does not
        // hide it, and nothing panics looking for an anchor it has not got.
        let sections = sidebar_sections(&store, &[vec![PathBuf::from(&home)]]);
        assert_eq!(sections.recent.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    fn dirs(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    /// A capture of `paths` under a known id, so the assertions can name
    /// the record they mean rather than the hash it happens to get.
    fn capture(id: &str, paths: &[&str], last_opened: u64) -> Project {
        let mut project = project_for_dirs(dirs(paths), last_opened).expect("dirs present");
        project.id = id.to_string();
        project
    }

    #[test]
    fn pinning_moves_a_project_out_of_recents_and_into_pinned() {
        let mut store = ProjectStore::default();
        store.record(capture("chat", &["/chat"], 100));
        store.record(capture("other", &["/other"], 200));

        assert!(store.set_pinned("chat", true), "the record was there");

        let pinned: Vec<&str> = store.pinned().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(pinned, vec!["chat"]);
        let recent: Vec<&str> = store.recent().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(recent, vec!["other"], "pinned is no longer also recent");
    }

    #[test]
    fn unpinning_returns_a_project_to_recents_where_its_last_use_puts_it() {
        // The one thing unpinning must NOT do is look like a use. Bumping
        // `last_opened` would drop a project the user just let go of at the
        // TOP of recents, above everything they have actually worked in
        // since — and pinning must not reorder the pinned list either.
        let mut store = ProjectStore::default();
        store.record(capture("old", &["/old"], 100));
        store.record(capture("new", &["/new"], 300));

        store.set_pinned("old", true);
        assert!(store.set_pinned("old", false), "and back again");

        let recent: Vec<&str> = store.recent().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            recent,
            vec!["new", "old"],
            "unpinned goes back where its last use puts it, not on top"
        );
        assert_eq!(
            store.recent()[1].last_opened,
            100,
            "and the timestamp itself is untouched"
        );
    }

    #[test]
    fn a_pinned_project_survives_the_cap_that_evicts_the_recents_around_it() {
        // The cap is what pinning buys you an exemption from: the oldest
        // project in the store is the first thing evicted, and pinning it
        // has to take it out of that queue permanently.
        let mut store = ProjectStore::default();
        for n in 1..=RECENT_CAP {
            store.record(capture(&format!("p{n}"), &[&format!("/p{n}")], n as u64));
        }
        assert!(store.set_pinned("p1", true), "the oldest of them all");

        // Three more captures push the UNPINNED count past the cap three
        // times over; each one evicts the oldest unpinned project.
        for n in 90..=92 {
            store.record(capture(&format!("p{n}"), &[&format!("/p{n}")], n as u64));
        }

        let ids: Vec<&str> = store
            .pinned()
            .iter()
            .chain(store.recent().iter())
            .map(|p| p.id.as_str())
            .collect();
        assert!(ids.contains(&"p1"), "the pinned one is exempt: {ids:?}");
        assert!(!ids.contains(&"p2"), "the oldest UNPINNED went instead");
        assert!(!ids.contains(&"p3"), "and then the next oldest");
        assert!(ids.contains(&"p4"), "eviction stopped at the cap: {ids:?}");
    }

    #[test]
    fn setting_a_pin_on_an_id_the_store_has_not_got_changes_nothing() {
        let mut store = ProjectStore::default();
        store.record(capture("chat", &["/chat"], 100));
        assert!(!store.set_pinned("ghost", true), "nothing to pin");
        assert!(
            store.pinned().is_empty(),
            "and no innocent bystander was pinned in its place"
        );
    }

    #[test]
    fn a_live_tabs_folders_find_the_record_they_stand_for() {
        // A live tab is not a record, so pinning one has to find the record
        // it belongs to — by the same anchor rule `record` matches on, so a
        // tab that has since opened a scratch terminal somewhere still
        // finds itself.
        let mut store = ProjectStore::default();
        store.record(capture("chat", &["/chat", "/penpot"], 100));
        store.record(capture("other", &["/other"], 200));

        assert_eq!(
            store
                .matching(&dirs(&["/chat", "/tmp"]))
                .map(|p| p.id.as_str()),
            Some("chat"),
            "the anchor folder is still open, so it is still that project"
        );
        assert!(
            store.matching(&dirs(&["/nowhere"])).is_none(),
            "and a tab rooted somewhere else belongs to no record"
        );
    }

    #[test]
    fn a_capture_the_user_named_writes_its_name_through_and_keeps_it() {
        // The rename flag has to REACH the store, or it protects nothing:
        // an auto-capture labels a project from its anchor's basename, so
        // the very next close would take the user's name back off it.
        let mut store = ProjectStore::default();
        store.record(capture("chat", &["/chat"], 100));
        assert_eq!(store.recent()[0].label, "chat", "the folder basename");

        let mut named = capture("ignored", &["/chat"], 200);
        named.label = "Chat stack".to_string();
        named.renamed = true;
        store.record(named);

        assert_eq!(store.recent()[0].id, "chat", "still the same record");
        assert_eq!(store.recent()[0].label, "Chat stack");
        assert!(store.recent()[0].renamed, "and it is the user's now");

        store.record(capture("ignored", &["/chat"], 300));
        assert_eq!(
            store.recent()[0].label,
            "Chat stack",
            "a later auto-capture must not take the name back"
        );
    }
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    fn mark(label: &str) -> ProjectMark {
        project_mark(label, ProjectIcon::Generated)
    }

    #[test]
    fn one_label_always_marks_the_same_way_however_it_is_written() {
        // The mark is the project's face in the sidebar, so it has to be
        // the same face every launch — and the same face for two spellings
        // of one name, matching the case-insensitivity every directory
        // comparison in this module already uses.
        let plain = mark("chat");
        for variant in ["chat", "Chat", "CHAT", "  chat  ", "\tchat\n"] {
            assert_eq!(
                mark(variant),
                plain,
                "{variant:?} is the same project as \"chat\""
            );
        }
        assert_eq!(plain.ch, 'C', "upper-cased for legibility at 9px");
    }

    #[test]
    fn labels_spread_across_the_whole_palette() {
        // A mark that lands on one colour for everything is a mark that
        // tells the user nothing. Every slot has to be reachable.
        let labels = [
            "chat",
            "board-kid",
            "penpot",
            "forgejo",
            "native",
            "superterminal",
            "docs",
            "notes",
            "zed",
            "dotfiles",
            "scripts",
            "website",
            "api",
            "infra",
            "blog",
            "sandbox",
            "photos",
            "music",
            "games",
            "tools",
            "client-a",
            "client-b",
            "research",
            "archive",
        ];
        let mut slots: Vec<usize> = labels.iter().map(|label| mark(label).slot).collect();
        assert!(
            slots.iter().all(|slot| *slot < MARK_SLOTS),
            "a slot outside the palette has no colour to be"
        );
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(
            slots.len(),
            MARK_SLOTS,
            "two dozen real project names must reach every slot, not {}",
            slots.len()
        );
    }

    #[test]
    fn projects_sharing_a_first_letter_do_not_share_a_colour() {
        // The reason the WHOLE label is hashed rather than the character
        // being drawn: a user's projects are not evenly spread over the
        // alphabet, and colouring by the initial would put every C
        // project on one colour — the collapse the mark exists to avoid.
        let mut slots: Vec<usize> = ["chat", "chess", "client-a", "code"]
            .iter()
            .map(|label| mark(label).slot)
            .collect();
        assert!(
            ["chat", "chess", "client-a", "code"]
                .iter()
                .all(|label| mark(label).ch == 'C'),
            "same letter, by construction"
        );
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 4, "and four different colours");
    }

    #[test]
    fn a_label_with_nothing_in_it_still_gets_a_mark() {
        // Only a hand-edited file can produce one, and a blank square in
        // the row reads as a failure to render rather than as a project.
        for empty in ["", " ", "\t\n  "] {
            let mark = mark(empty);
            assert!(
                !mark.ch.is_whitespace(),
                "{empty:?} must still draw something"
            );
            assert!(mark.slot < MARK_SLOTS, "and land on a real colour");
        }
    }

    #[test]
    fn a_multi_byte_first_character_is_drawn_whole_and_never_sliced() {
        // `&label[..1]` panics mid-character on every one of these, and a
        // panic in a sidebar row takes the window with it.
        assert_eq!(mark("Émile").ch, 'É');
        assert_eq!(mark("émile").ch, 'É', "and still upper-cased");
        assert_eq!(mark("日本語").ch, '日');
        assert_eq!(mark("ñoño").ch, 'Ñ');
        // Four bytes, not just three: a CJK ideograph is three, and the
        // supplementary planes are where a byte-slice goes wrong last and
        // loudest.
        assert_eq!(mark("\u{20000}-notes").ch, '\u{20000}');
        for label in ["Émile", "日本語", "ñoño"] {
            assert!(mark(label).slot < MARK_SLOTS);
        }
    }
}
