//! What a project row's SECOND line says: the branch it is on, how it sits
//! against its upstream, and how much is uncommitted.
//!
//! **Every git call shells out and can block** — `git_panel`'s module doc
//! states the same cost and runs every engine call on the background
//! executor because of it. A sidebar row redraws on every frame, so a row
//! must never ask git anything. It reads the cache this module defines,
//! which a slow poll fills from the background executor: a project the
//! probe has not reached yet draws its first line and no git line, and
//! gains one when its probe lands.
//!
//! The cache is keyed by the project's ANCHOR directory — the one folder a
//! project is identified by (`projects::project_anchor`) — so a project
//! with four folders open asks git about one of them, and two rows that
//! name the same project share one answer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use superterminal_core::git::status::StatusReport;
use superterminal_core::git::{self, GitState};

/// One project's repo, reduced to exactly what a row draws.
///
/// A reduction rather than the whole [`StatusReport`] on purpose: the
/// report carries an entry per changed file, and the cache holds one of
/// these per project for as long as the sidebar lists it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitSummary {
    /// The branch, or the short oid when HEAD is detached. `None` only for
    /// a report that names neither — nothing git emits today, but the row
    /// drops the part rather than drawing an empty one.
    pub head: Option<String>,
    /// Commits ahead of / behind upstream. Both are 0 when there is no
    /// upstream at all, which is why "no upstream" needs no state of its
    /// own: the part is dropped by the same rule that drops it when a
    /// tracked branch is in sync.
    pub ahead: i64,
    pub behind: i64,
    /// Changed paths — staged, unstaged and untracked alike. The row says
    /// how much is uncommitted, not which of the three kinds it is; the
    /// git panel is where that breakdown lives.
    pub dirty: usize,
}

/// Anchor directory -> what its row draws.
///
/// Three states, and the difference between two of them matters:
/// * absent — not probed yet. The row draws no git line and gains one.
/// * `Some(None)` — probed, nothing to say: not a repo, or git refused.
///   The row draws no git line, NOT an empty row and not a label.
/// * `Some(Some(summary))` — a repo.
pub type GitCache = HashMap<PathBuf, Option<GitSummary>>;

/// Reduce a fresh status report to what a row draws.
///
/// `branch` first, `detached` second: a detached HEAD has no branch, and
/// the parser already shortens its oid to eight characters.
pub fn summarize(report: &StatusReport) -> GitSummary {
    GitSummary {
        head: report.branch.clone().or_else(|| report.detached.clone()),
        ahead: report.ahead.max(0),
        behind: report.behind.max(0),
        dirty: report.entries.len(),
    }
}

/// The row's second line, or `None` when there is nothing to draw.
///
/// `None` in — a folder the cache says is not a repo — is `None` out: a
/// folder that is not a repo gets no git line at all, rather than an empty
/// row or a "not a repo" label.
///
/// Each part is dropped when it does not apply, in one fixed order:
/// head, then ahead/behind, then the dirty count. Fixed so the eye can
/// scan a column of rows and find the branch in the same place on each.
pub fn git_line(summary: Option<&GitSummary>) -> Option<String> {
    let summary = summary?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(head) = summary.head.as_ref().map(|head| head.trim()) {
        if !head.is_empty() {
            parts.push(head.to_string());
        }
    }
    // One part, not two: "up 2 down 1" is one fact about one upstream, and
    // splitting it across two separators reads as two unrelated numbers.
    let mut tracking = String::new();
    if summary.ahead > 0 {
        tracking.push_str(&format!("\u{2191}{}", summary.ahead));
    }
    if summary.behind > 0 {
        if !tracking.is_empty() {
            tracking.push(' ');
        }
        tracking.push_str(&format!("\u{2193}{}", summary.behind));
    }
    if !tracking.is_empty() {
        parts.push(tracking);
    }
    if summary.dirty > 0 {
        parts.push(format!("{} dirty", summary.dirty));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" \u{b7} "))
}

/// What one probe found. Three outcomes, because "busy" is not an answer
/// about the repo and must not be written into the cache as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing to draw: the folder is not a repo, or git failed to answer.
    /// Both land here deliberately — a row has nowhere to put an error,
    /// and a line saying why would cost the width the name needs.
    Blank,
    /// A repo.
    Summary(GitSummary),
    /// The repo is held by an action or another status run
    /// (`git::status_guarded` refuses with "busy"). The row keeps whatever
    /// it already had rather than blinking to nothing while someone
    /// commits.
    Busy,
}

/// Ask git about one anchor directory. **Blocks** — callers run it on the
/// background executor, never on the UI thread.
///
/// Goes through [`GitState`] rather than shelling out directly so probes
/// share the interning, the per-repo action lock and the in-flight guard
/// the git panel's own refresh uses: a probe can never run while a commit
/// or a discard holds the repo.
pub fn probe(state: &GitState, anchor: &Path) -> Probe {
    let Some(repo) = state.resolve(&anchor.to_string_lossy()) else {
        return Probe::Blank;
    };
    match git::status_guarded(state, &repo.repo_id) {
        Ok(report) => Probe::Summary(summarize(&report)),
        Err(err) if err == "busy" => Probe::Busy,
        Err(_) => Probe::Blank,
    }
}

/// The cache key for one project row — the folder its git line is about.
///
/// The matched record's STORED anchor when the store knows this project,
/// the folders' DERIVED anchor otherwise.
///
/// That order is the whole point, and a test found out why: derivation
/// reads the folder SET (`projects::anchor_dir` picks the shallowest, then
/// alphabetically), so a tab that has a second terminal sitting in a
/// folder that ranks ahead of the project's own derives a DIFFERENT anchor
/// from the one its own record stores. Keyed by derivation, a live tab and
/// the remembered row that reopens it would probe two folders and hold two
/// cache entries for one project. Keyed by the record first, they agree —
/// and the fallback still covers a tab the store has never seen.
///
/// `None` for a tab with nothing but `$HOME` open (every launch opens one)
/// and for a record with no anchor at all: no folder to ask git about, so
/// no git line.
pub fn row_anchor<'a>(
    dirs: &'a [PathBuf],
    record: Option<&'a crate::projects::Project>,
) -> Option<&'a PathBuf> {
    record
        .and_then(crate::projects::project_anchor)
        .or_else(|| crate::projects::anchor_dir(dirs))
}

/// Drop every entry whose project has left the sidebar.
///
/// The whole reason the cache is pruned rather than left to grow: a row
/// must never draw a branch for a folder that is gone. An entry outlives
/// its project only for as long as it takes the next poll to run, and this
/// is that poll.
pub fn prune(cache: &mut GitCache, live: &HashSet<PathBuf>) {
    cache.retain(|anchor, _| live.contains(anchor));
}

/// The only non-ASCII characters `git_line` may ever emit: the separator,
/// and the two tracking arrows.
///
/// This UI takes SVG icons, never emoji — and a branch NAME is attacker-ish
/// input in the sense that matters here: it is whatever someone called a
/// branch, including emoji, and it is rendered verbatim. That is fine (a
/// branch called 🚀 should display as its name), so this guards the parts
/// this module AUTHORS, not the parts it quotes.
#[cfg(test)]
pub const LINE_GLYPHS: [char; 3] = ['\u{b7}', '\u{2191}', '\u{2193}'];

#[cfg(test)]
mod tests {
    use super::LINE_GLYPHS;
    use super::*;
    use superterminal_core::git::status::parse_status;

    fn summary(head: &str, ahead: i64, behind: i64, dirty: usize) -> GitSummary {
        GitSummary {
            head: Some(head.to_string()),
            ahead,
            behind,
            dirty,
        }
    }

    fn line(summary: &GitSummary) -> Option<String> {
        git_line(Some(summary))
    }

    /// Join records with NUL and append the trailing NUL `-z` emits —
    /// real `git status --porcelain=v2 --branch -z` output.
    fn z(parts: &[&str]) -> Vec<u8> {
        let mut v = parts.join("\0").into_bytes();
        v.push(0);
        v
    }

    #[test]
    fn clean_tracked_branch_is_the_branch_alone() {
        assert_eq!(line(&summary("main", 0, 0, 0)).as_deref(), Some("main"));
    }

    #[test]
    fn dirty_count_follows_the_branch() {
        assert_eq!(
            line(&summary("main", 0, 0, 3)).as_deref(),
            Some("main \u{b7} 3 dirty")
        );
    }

    #[test]
    fn ahead_alone() {
        assert_eq!(
            line(&summary("main", 2, 0, 0)).as_deref(),
            Some("main \u{b7} \u{2191}2")
        );
    }

    #[test]
    fn behind_alone() {
        assert_eq!(
            line(&summary("main", 0, 5, 0)).as_deref(),
            Some("main \u{b7} \u{2193}5")
        );
    }

    #[test]
    fn ahead_and_behind_are_one_part() {
        assert_eq!(
            line(&summary("main", 2, 1, 0)).as_deref(),
            Some("main \u{b7} \u{2191}2 \u{2193}1")
        );
    }

    #[test]
    fn every_part_at_once_keeps_its_order() {
        assert_eq!(
            line(&summary("main", 2, 1, 4)).as_deref(),
            Some("main \u{b7} \u{2191}2 \u{2193}1 \u{b7} 4 dirty")
        );
    }

    #[test]
    fn no_upstream_drops_the_tracking_part() {
        // No upstream means ahead and behind are both 0 — the same rule
        // that drops them for a branch in sync.
        let report = parse_status(&z(&[
            "# branch.oid 1234567890abcdef",
            "# branch.head feature",
        ]));
        assert!(report.upstream.is_none());
        assert_eq!(line(&summarize(&report)).as_deref(), Some("feature"));
    }

    #[test]
    fn detached_head_draws_its_short_oid() {
        let report = parse_status(&z(&[
            "# branch.oid 1234567890abcdef",
            "# branch.head (detached)",
        ]));
        assert_eq!(line(&summarize(&report)).as_deref(), Some("12345678"));
    }

    #[test]
    fn detached_head_still_reports_its_dirt() {
        let report = parse_status(&z(&[
            "# branch.oid 1234567890abcdef",
            "# branch.head (detached)",
            "1 .M N... 100644 100644 100644 aaaa bbbb a.txt",
        ]));
        assert_eq!(
            line(&summarize(&report)).as_deref(),
            Some("12345678 \u{b7} 1 dirty")
        );
    }

    #[test]
    fn a_folder_that_is_not_a_repo_has_no_line_at_all() {
        assert_eq!(git_line(None), None);
    }

    #[test]
    fn a_summary_with_nothing_to_say_has_no_line() {
        // Not reachable from git today (a report always names a head), but
        // the row must not draw a separator with no parts around it.
        assert_eq!(git_line(Some(&GitSummary::default())), None);
    }

    #[test]
    fn a_blank_head_is_dropped_like_a_missing_one() {
        let summary = GitSummary {
            head: Some("   ".to_string()),
            ahead: 0,
            behind: 0,
            dirty: 2,
        };
        assert_eq!(line(&summary).as_deref(), Some("2 dirty"));
    }

    #[test]
    fn summarize_counts_staged_unstaged_and_untracked_alike() {
        let report = parse_status(&z(&[
            "# branch.oid 1234567890abcdef",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +1 -2",
            "1 M. N... 100644 100644 100644 aaaa bbbb staged.txt",
            "1 .M N... 100644 100644 100644 cccc dddd unstaged.txt",
            "? untracked.txt",
        ]));
        let summary = summarize(&report);
        assert_eq!(summary.head.as_deref(), Some("main"));
        assert_eq!(summary.ahead, 1);
        assert_eq!(summary.behind, 2);
        assert_eq!(summary.dirty, 3);
        assert_eq!(
            git_line(Some(&summary)).as_deref(),
            Some("main \u{b7} \u{2191}1 \u{2193}2 \u{b7} 3 dirty")
        );
    }

    #[test]
    fn an_unborn_branch_still_names_itself() {
        let report = parse_status(&z(&["# branch.oid (initial)", "# branch.head main"]));
        assert_eq!(line(&summarize(&report)).as_deref(), Some("main"));
    }

    fn project(anchor: &str, dirs: &[&str]) -> crate::projects::Project {
        crate::projects::Project {
            anchor: Some(PathBuf::from(anchor)),
            dirs: dirs.iter().map(PathBuf::from).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_tab_the_store_has_never_seen_derives_its_anchor() {
        let dirs = vec![
            PathBuf::from("/work/repo/crate"),
            PathBuf::from("/work/repo"),
        ];
        assert_eq!(row_anchor(&dirs, None), Some(&PathBuf::from("/work/repo")));
    }

    #[test]
    fn a_matched_record_keys_by_its_stored_anchor() {
        // Its `dirs` would DERIVE `/other` (same depth, earlier name); the
        // stored anchor wins, so the record keeps answering to the folder
        // it was first known by.
        let record = project("/work/repo", &["/other", "/work/repo"]);
        assert_eq!(
            row_anchor(&record.dirs, Some(&record)),
            Some(&PathBuf::from("/work/repo"))
        );
    }

    #[test]
    fn a_project_open_and_remembered_is_one_key() {
        // The live tab has picked up a second terminal in a folder that
        // ranks AHEAD of the project's own; derivation alone would key it
        // to `/tmp/aux` and probe a different folder from its own record.
        let record = project("/work/repo", &["/work/repo"]);
        let open = vec![PathBuf::from("/work/repo"), PathBuf::from("/tmp/aux")];
        assert_eq!(
            crate::projects::anchor_dir(&open),
            Some(&PathBuf::from("/tmp/aux")),
            "the derivation this rule has to override"
        );
        assert_eq!(
            row_anchor(&open, Some(&record)),
            row_anchor(&record.dirs, Some(&record)),
            "one project, one cache entry, whichever row draws it"
        );
    }

    #[test]
    fn prune_drops_entries_whose_project_left_the_list() {
        let kept = PathBuf::from("/tmp/kept");
        let gone = PathBuf::from("/tmp/gone");
        let mut cache: GitCache = HashMap::new();
        cache.insert(kept.clone(), Some(summary("main", 0, 0, 0)));
        cache.insert(gone.clone(), Some(summary("other", 0, 0, 0)));
        prune(&mut cache, &HashSet::from([kept.clone()]));
        assert!(cache.contains_key(&kept));
        assert!(
            !cache.contains_key(&gone),
            "a folder that has left the sidebar must not keep a branch"
        );
    }

    #[test]
    fn prune_keeps_a_probed_not_a_repo_entry() {
        // `Some(None)` is an ANSWER ("probed, nothing to draw"), not an
        // empty slot — pruning it would re-probe the same folder forever.
        let dir = PathBuf::from("/tmp/plain");
        let mut cache: GitCache = HashMap::new();
        cache.insert(dir.clone(), None);
        prune(&mut cache, &HashSet::from([dir.clone()]));
        assert_eq!(cache.get(&dir), Some(&None));
    }

    #[test]
    fn the_line_authors_no_emoji_of_its_own() {
        // The hints module has a guard like this; the git line did not, so
        // nothing stopped a later edit reaching for an emoji here. Driven
        // over every shape the line can take rather than a sample.
        let shapes = [
            summary("main", 0, 0, 0),
            summary("main", 0, 0, 3),
            summary("main", 2, 0, 0),
            summary("main", 0, 5, 0),
            summary("main", 2, 1, 4),
        ];
        for summary in shapes {
            let line = git_line(Some(&summary)).expect("a summary yields a line");
            for ch in line.chars() {
                assert!(
                    ch.is_ascii() || LINE_GLYPHS.contains(&ch),
                    "git_line emitted {ch:?}, which is neither ASCII nor one of \
                     its three allowed glyphs: {line}"
                );
            }
        }
    }

    #[test]
    fn a_branch_named_with_an_emoji_is_still_shown_verbatim() {
        // The guard above is about what this module AUTHORS. A branch name
        // is quoted, not authored: someone called it that, and silently
        // mangling it would be worse than rendering it.
        let summary = summary("feature/\u{1f680}-launch", 0, 0, 0);
        assert_eq!(
            git_line(Some(&summary)).as_deref(),
            Some("feature/\u{1f680}-launch")
        );
    }
}
