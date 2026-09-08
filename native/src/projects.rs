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
    pub pinned: bool,
    /// Unix timestamp, seconds. A plain integer rather than `SystemTime` so
    /// it stays comparable, sortable and human-readable once serialized.
    pub last_opened: u64,
    pub icon: ProjectIcon,
    /// Set once the user renames the project away from its auto-assigned
    /// label. Together with `pinned`, this protects the record's `id`: once
    /// either is true, a later auto-capture of the same directory set must
    /// never merge into it (see `ProjectStore::record`) — the record is the
    /// user's now, identified by `id`, not by its paths.
    pub renamed: bool,
    /// How many terminals the project had when it was LAST captured —
    /// "how big is this thing" before you open it. Latest capture wins:
    /// unlike the label, this is a measurement, not something the user
    /// made theirs, so a pinned record's count still moves.
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

/// Directory-set equality: same members, order irrelevant, case-insensitive.
/// Used to decide whether an incoming auto-capture is "the same project" as
/// an existing record.
fn same_dir_set(a: &[PathBuf], b: &[PathBuf]) -> bool {
    let a: HashSet<String> = a.iter().map(|p| dir_key(p)).collect();
    let b: HashSet<String> = b.iter().map(|p| dir_key(p)).collect();
    a == b
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
    match std::env::var_os("HOME") {
        Some(home) => dir_key(&dirs[0]) != dir_key(Path::new(&home)),
        None => true,
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

/// The record an auto-capture writes for `dirs`, or `None` when there is
/// nothing to reopen — an all-remote project is not persisted at all.
///
/// The label is the first directory's basename: a fine default for one
/// directory and useless for four, which is exactly why it is only a
/// default. `renamed` stays false so the user's own name, once given,
/// wins over every later capture (see `ProjectStore::record`).
pub fn project_for_dirs(dirs: Vec<PathBuf>, now: u64) -> Option<Project> {
    let first = dirs.first()?;
    let label = first
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| first.to_string_lossy().to_string());
    Some(Project {
        id: project_id(&dirs),
        label,
        dirs,
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
/// Time is omitted entirely at zero. A record written before `active_secs`
/// existed, or one whose only session never closed cleanly, has no
/// measurement to report — and "under a minute" would invent one.
pub fn project_summary(dirs: usize, terminals: usize, active_secs: u64) -> String {
    let mut parts = vec![
        count_label(dirs, "folder"),
        count_label(terminals, "terminal"),
    ];
    if active_secs > 0 {
        parts.push(duration_label(active_secs));
    }
    parts.join(" \u{b7} ")
}

/// A capture's id: FNV-1a over the directory keys, sorted so pane order
/// cannot change it.
///
/// `record` matches on the directory SET, so an id only has to be unique.
/// Deriving it from that same set buys one more thing for nothing: a
/// project whose `projects.json` was lost comes back under the id it
/// always had, instead of a fresh one on every launch. Hand-rolled rather
/// than `DefaultHasher`, whose output std does not promise to keep stable
/// across releases — a persisted id must not change under the app.
fn project_id(dirs: &[PathBuf]) -> String {
    let mut keys: Vec<String> = dirs.iter().map(|dir| dir_key(dir)).collect();
    keys.sort();
    keys.dedup();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in keys.join("\u{0}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("proj-{hash:016x}")
}

impl ProjectStore {
    pub fn load() -> ProjectStore {
        Self::load_from(&projects_path())
    }

    /// Missing or corrupt files load defaults (an empty store) rather than
    /// erroring — losing the projects file must never be worse than an
    /// inconvenience.
    pub fn load_from(path: &Path) -> ProjectStore {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ProjectStore::default(),
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
    /// - a `dirs` set matching an existing UNPINNED, UNRENAMED record
    ///   updates that record's `last_opened` rather than adding a duplicate,
    ///   regardless of order within `dirs`;
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
        // A pinned or renamed record still MATCHES — it just is not
        // overwritten. Only its `last_opened` moves, which is what keeps the
        // pinned list ordered by use and stops a second, unpinned copy of
        // the same project accumulating in recents every time it is opened.
        // The spec's rule is that such a record keeps its `id`, and it does.
        if let Some(existing) = self
            .projects
            .iter_mut()
            .find(|p| same_dir_set(&p.dirs, &project.dirs))
        {
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
                existing.label = project.label;
                existing.dirs = project.dirs;
                existing.icon = project.icon;
            }
            return;
        }
        self.projects.push(project);
        let just_added = self.projects.len() - 1;
        self.evict_past_cap(just_added);
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

    /// Unpinned projects, newest first, capped at `RECENT_CAP`.
    pub fn recent(&self) -> Vec<&Project> {
        let mut v: Vec<&Project> = self.projects.iter().filter(|p| !p.pinned).collect();
        v.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
        v.truncate(RECENT_CAP);
        v
    }
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
    fn record_match_ignores_directory_order() {
        let mut store = ProjectStore::default();
        store.record(project("orig", &["/chat", "/board-kid", "/penpot"], 100));
        // Same set, different order.
        store.record(project(
            "would-be-new-id",
            &["/penpot", "/chat", "/board-kid"],
            500,
        ));
        let recent = store.recent();
        assert_eq!(recent.len(), 1, "order must not defeat the match");
        assert_eq!(recent[0].id, "orig");
        assert_eq!(recent[0].last_opened, 500);
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
    fn record_does_not_merge_different_dir_sets() {
        let mut store = ProjectStore::default();
        store.record(project("a", &["/a", "/b"], 100));
        store.record(project("b", &["/a", "/c"], 200));
        assert_eq!(
            store.recent().len(),
            2,
            "overlapping but unequal sets are different projects"
        );
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
    fn the_default_label_is_the_first_directorys_basename() {
        // A basename is a fine default for one directory and useless for
        // four — "chat" is not derivable from those four paths — so the
        // FIRST one names the project until the user renames it.
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
    fn one_project_gets_one_id_however_its_panes_were_ordered() {
        // The store matches on the directory SET, so a capture's id only
        // has to be unique — but deriving it from that same set means a
        // project whose store file was lost comes back under the id it
        // always had, rather than a fresh one each launch.
        let a = project_for_dirs(vec![PathBuf::from("/chat"), PathBuf::from("/penpot")], 1)
            .expect("dirs present");
        let b = project_for_dirs(vec![PathBuf::from("/Penpot"), PathBuf::from("/chat")], 2)
            .expect("dirs present");
        assert_eq!(a.id, b.id, "same directory set, same project");
        let other = project_for_dirs(vec![PathBuf::from("/chat")], 3).expect("dirs present");
        assert_ne!(a.id, other.id, "a different set is a different project");
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
        let summary = project_summary(1, 1, 0);
        assert_eq!(summary, "1 folder \u{b7} 1 terminal");
        assert!(!summary.contains("minute"));
    }

    #[test]
    fn the_summary_states_folders_terminals_and_time_together() {
        assert_eq!(
            project_summary(4, 6, 11_520),
            "4 folders \u{b7} 6 terminals \u{b7} 3h 12m"
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
        let mut store = ProjectStore::default();
        let mut first = project("chat", &["/chat", "/penpot"], 100);
        first.active_secs = 100;
        store.record(first);
        let mut second = project("chat-again", &["/penpot", "/chat"], 200);
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
