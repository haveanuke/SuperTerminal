//! Persistent project store: `projects.json`, beside `settings.json` in the
//! same app-support directory (see `settings::settings_dir`).
//!
//! A "project" is several directories opened together (see
//! `docs/superpowers/specs/2026-09-02-projects-design.md`) — not a single
//! path. This module is pure and file-backed only: no gpui, no `Workspace`,
//! no `TerminalPane`. Later work wires capture (closing a tab records a
//! project) and the sidebar UI (pinned/recent lists, reopening) on top of
//! this store.

// Wired by Task 2 (capture) and Task 3 (the sidebar). Until then the whole
// module is built but uncalled, and the repo's convention for staged code is
// this attribute rather than leaving warnings to accumulate — expected
// warnings sharing space with real ones is how a real one gets missed.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
        if project.dirs.is_empty() {
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
}
