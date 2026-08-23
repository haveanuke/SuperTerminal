//! Git observation for the buddy: what the gate's probes actually run.
//!
//! `observe` is the cheap-ish per-tick call: repo root, resolved HEAD, and a
//! hash of the FULL working snapshot — `git diff HEAD` (staged + unstaged;
//! plain `git diff` would go blind the moment an agent stages its work) plus
//! `git status --porcelain` (so untracked files count). `working_patch` /
//! `commit_patch` produce the normalized patch used at dispatch time; both
//! strip everything but the diff body so a commit of the just-reviewed
//! working diff hashes identically (dedupe).

use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::buddy_gate::ProbeResult;
use crate::git::process::run_git;

/// Diff flags shared by every patch producer so outputs stay comparable.
const PATCH_FLAGS: [&str; 3] = ["--no-color", "--no-ext-diff", "--no-textconv"];

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Parse NUL-delimited `status --porcelain -z` output for `?? ` entries.
/// -z means literal paths — no C-style quoting to decode, so Unicode,
/// spaces, quotes, and even newlines in names all resolve.
fn untracked_paths(status: &[u8]) -> Vec<String> {
    status
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"?? "))
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

/// Every path in `status -z` output, regardless of code — used to build a
/// content-change fingerprint when the diff itself overflowed. Rename
/// old-path entries lack the "XY " shape and fall out naturally.
fn all_status_paths(status: &[u8]) -> Vec<String> {
    status
        .split(|byte| *byte == 0)
        .filter(|entry| entry.len() > 3 && entry[2] == b' ')
        .map(|entry| String::from_utf8_lossy(&entry[3..]).into_owned())
        .collect()
}

/// Size + mtime per path: a change git cannot (or could not) diff must
/// still reset the stability clock.
fn paths_fingerprint(root: &Path, paths: Vec<String>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        // symlink_metadata: fingerprint the LINK, never its target.
        let meta = std::fs::symlink_metadata(root.join(&path)).ok();
        let size = meta.as_ref().map(|meta| meta.len()).unwrap_or(0);
        let mtime = meta
            .and_then(|meta| meta.modified().ok())
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        bytes.extend_from_slice(format!("{path}\0{size}\0{mtime}\n").as_bytes());
    }
    bytes
}

/// Fingerprint of untracked files only (the normal, non-overflow path —
/// tracked content is covered by the diff there). Every reported path is
/// covered; a capped fingerprint would leave files whose edits never reset
/// stability.
fn untracked_fingerprint(root: &Path, status: &[u8]) -> Vec<u8> {
    paths_fingerprint(root, untracked_paths(status))
}

/// Bounded, symlink-refusing read of an untracked file for the review
/// excerpt. Refusing symlinks keeps files OUTSIDE the repo (credentials a
/// link points at) away from the agent; refusing non-regular files keeps a
/// FIFO from blocking the probe forever; the byte cap bounds allocation.
fn read_untracked_excerpt(root: &Path, rel: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let path = root.join(rel);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .ok()?
        .take(8192)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn stdout_line(repo: Option<&Path>, args: &[&str]) -> Option<String> {
    let out = run_git(repo, args, false).ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Staged (vs the empty tree) plus unstaged patches — the diff view for a
/// repo whose HEAD is unborn. NOTE: this representation intentionally does
/// not dedupe against the eventual first commit's patch (different base);
/// the cost is at most one repeat review at the first commit of a repo.
fn unborn_diff(root: &Path) -> Vec<u8> {
    // Each component degrades to --stat on overflow so a huge first change
    // is reviewed by shape instead of silently vanishing.
    let mut bytes = run_git(
        Some(root),
        &[
            "diff",
            "--cached",
            PATCH_FLAGS[0],
            PATCH_FLAGS[1],
            PATCH_FLAGS[2],
        ],
        false,
    )
    .or_else(|_| {
        run_git(
            Some(root),
            &["diff", "--cached", "--stat", "--no-color"],
            false,
        )
    })
    .map(|out| out.stdout)
    .unwrap_or_default();
    let unstaged = run_git(
        Some(root),
        &["diff", PATCH_FLAGS[0], PATCH_FLAGS[1], PATCH_FLAGS[2]],
        false,
    )
    .or_else(|_| run_git(Some(root), &["diff", "--stat", "--no-color"], false))
    .map(|out| out.stdout)
    .unwrap_or_default();
    bytes.extend_from_slice(&unstaged);
    bytes
}

/// One probe of the directory a pane is sitting in.
pub fn observe(cwd: &Path) -> ProbeResult {
    let Some(root) = stdout_line(Some(cwd), &["rev-parse", "--show-toplevel"]) else {
        return ProbeResult {
            repo_root: None,
            head: None,
            head_committed: false,
            snapshot_hash: None,
        };
    };
    let root_path = Path::new(&root);
    let head = stdout_line(
        root_path.into(),
        &["rev-parse", "--verify", "--quiet", "HEAD"],
    );
    // Classify how HEAD got here: only a commit CREATION (commit, amend,
    // cherry-pick) earns a "just committed" review — checkout/reset/rebase
    // move HEAD to commits that are not new work. The latest HEAD reflog
    // entry carries both the landing oid and the action.
    let head_committed = match (
        &head,
        stdout_line(Some(root_path), &["reflog", "-1", "--format=%H %gs"]),
    ) {
        (Some(head), Some(entry)) => match entry.split_once(' ') {
            Some((oid, action)) => {
                oid == head && (action.starts_with("commit") || action.starts_with("cherry-pick"))
            }
            None => false,
        },
        _ => false,
    };
    // An overflowing status (pathologically many untracked files) must read
    // as a DISTINCT dirty state, never as a clean tree.
    let status = run_git(
        Some(root_path),
        &["status", "--porcelain", "-uall", "-z"],
        false,
    )
    .map(|out| out.stdout)
    .unwrap_or_else(|_| b"status-overflow".to_vec());
    let untracked = untracked_fingerprint(root_path, &status);
    // Full patch is the accurate stability signal; on overflow ("output too
    // large") fall back to --stat so a huge diff still reads as a distinct
    // non-empty state instead of silence. Unborn HEAD has no diff base —
    // status alone carries the snapshot there.
    let diff: Vec<u8> = if head.is_some() {
        match run_git(
            Some(root_path),
            &[
                "diff",
                "HEAD",
                PATCH_FLAGS[0],
                PATCH_FLAGS[1],
                PATCH_FLAGS[2],
            ],
            false,
        ) {
            Ok(out) => out.stdout,
            Err(_) => {
                // --stat alone is not content-sensitive (same files, same
                // line totals hash alike): append a size+mtime fingerprint
                // of EVERY changed path so churning oversized content can
                // never read as stable.
                let mut bytes = run_git(
                    Some(root_path),
                    &["diff", "HEAD", "--stat", "--no-color"],
                    false,
                )
                .map(|out| out.stdout)
                .unwrap_or_default();
                bytes.extend_from_slice(&paths_fingerprint(root_path, all_status_paths(&status)));
                bytes
            }
        }
    } else {
        // Unborn HEAD has no diff base: staged content (vs the empty tree)
        // plus unstaged edits stand in, so post-staging edits still move
        // the snapshot.
        unborn_diff(root_path)
    };
    let snapshot_hash = if diff.is_empty() && status.is_empty() {
        None
    } else {
        let mut bytes = diff;
        bytes.extend_from_slice(&status);
        bytes.extend_from_slice(&untracked);
        Some(hash_bytes(&bytes))
    };
    ProbeResult {
        repo_root: Some(root),
        head,
        head_committed,
        snapshot_hash,
    }
}

/// The normalized working-tree patch (vs HEAD) and its content hash.
/// Returns None when there is nothing to show (clean, or unreadable repo);
/// an untracked-only tree yields Some with the untracked list + excerpts.
///
/// Known limitation, accepted: the untracked representation is synthetic,
/// so the same content re-hashes differently once `git add` turns it into
/// a real diff — at most one repeat review per new file on its way from
/// untracked to staged. (Staged→committed DOES dedupe: both are unified
/// diffs with identical bodies.)
pub fn working_patch(repo_root: &Path) -> Option<(String, u64)> {
    let head_exists = stdout_line(
        Some(repo_root),
        &["rev-parse", "--verify", "--quiet", "HEAD"],
    )
    .is_some();
    let mut text = if head_exists {
        match run_git(
            Some(repo_root),
            &[
                "diff",
                "HEAD",
                PATCH_FLAGS[0],
                PATCH_FLAGS[1],
                PATCH_FLAGS[2],
            ],
            false,
        ) {
            Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
            // Overflowing diff: degrade to --stat so a huge change is
            // reviewed by shape instead of silently consumed.
            Err(_) => run_git(
                Some(repo_root),
                &["diff", "HEAD", "--stat", "--no-color"],
                false,
            )
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default(),
        }
    } else {
        // Unborn HEAD: staged (vs empty tree) + unstaged views.
        String::from_utf8_lossy(&unborn_diff(repo_root)).into_owned()
    };
    let status_result = run_git(
        Some(repo_root),
        &["status", "--porcelain", "-uall", "-z"],
        false,
    );
    let status_overflow = status_result.is_err();
    let status = status_result.map(|out| out.stdout).unwrap_or_default();
    if status_overflow {
        // The tree is dirty beyond enumeration — say so rather than let an
        // untracked-only tree read as "nothing to show" and get consumed.
        text.push_str("\nStatus overflow: too many changed/untracked files to enumerate.\n");
    }
    let untracked = untracked_paths(&status);
    if !untracked.is_empty() {
        text.push_str("\nUntracked files:\n");
        for file in &untracked {
            // Size in the listing: edits beyond the excerpt window still
            // move the review hash (files virtually always change size),
            // so they cannot be deduped away forever.
            let size = std::fs::symlink_metadata(repo_root.join(file))
                .map(|meta| meta.len())
                .unwrap_or(0);
            text.push_str("  ");
            text.push_str(file);
            text.push_str(&format!(" ({size} bytes)\n"));
        }
        // Excerpt textual content of the first few so brand-new code is
        // actually reviewable (and the hash moves when it changes). Binary
        // files stay as list entries only.
        for file in untracked.iter().take(10) {
            let Some(bytes) = read_untracked_excerpt(repo_root, file) else {
                continue;
            };
            if bytes[..bytes.len().min(1024)].contains(&0) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let excerpt: String = content.chars().take(2000).collect();
            text.push_str("\n--- untracked: ");
            text.push_str(file);
            text.push_str(" ---\n");
            text.push_str(&excerpt);
            text.push('\n');
        }
    }
    let normalized = text.trim();
    (!normalized.is_empty()).then(|| (normalized.to_string(), hash_bytes(normalized.as_bytes())))
}

/// The normalized patch of one commit and its content hash. None for merge
/// commits (their combined diffs mislead more than they inform) and for
/// unreadable oids.
pub fn commit_patch(repo_root: &Path, oid: &str) -> Option<(String, u64)> {
    let second_parent = format!("{oid}^2");
    if stdout_line(
        Some(repo_root),
        &["rev-parse", "--verify", "--quiet", &second_parent],
    )
    .is_some()
    {
        return None;
    }
    let out = match run_git(
        Some(repo_root),
        &[
            "show",
            "--format=",
            "--patch",
            PATCH_FLAGS[0],
            PATCH_FLAGS[1],
            PATCH_FLAGS[2],
            oid,
        ],
        false,
    ) {
        Ok(out) => out,
        // Overflowing commit: degrade to --stat so it is reviewed by shape
        // instead of being silently consumed unreviewed.
        Err(_) => run_git(
            Some(repo_root),
            &["show", "--format=", "--stat", "--no-color", oid],
            false,
        )
        .ok()?,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let normalized = text.trim();
    (!normalized.is_empty()).then(|| (normalized.to_string(), hash_bytes(normalized.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo::*;

    #[test]
    fn non_repo_dir_observes_nothing() {
        let dir = tmp_dir();
        let result = observe(&dir);
        assert_eq!(result.repo_root, None);
    }

    #[test]
    fn clean_repo_has_root_head_and_no_snapshot() {
        let dir = tmp_repo();
        let result = observe(&dir);
        assert_eq!(result.repo_root.as_deref(), dir.to_str());
        assert!(result.head.is_some());
        assert_eq!(result.snapshot_hash, None);
    }

    #[test]
    fn observe_from_subdirectory_reports_the_root() {
        let dir = tmp_repo();
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        let result = observe(&dir.join("sub/deeper"));
        assert_eq!(result.repo_root.as_deref(), dir.to_str());
    }

    #[test]
    fn unstaged_edit_is_a_stable_snapshot() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "one");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "base"], false).unwrap();
        write(&dir, "a.txt", "two");
        let first = observe(&dir).snapshot_hash.expect("dirty tree hashes");
        let again = observe(&dir).snapshot_hash.unwrap();
        assert_eq!(first, again, "unchanged tree must hash identically");
        write(&dir, "a.txt", "three");
        assert_ne!(observe(&dir).snapshot_hash.unwrap(), first);
    }

    #[test]
    fn staged_changes_still_count_as_working_changes() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "one");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        assert!(observe(&dir).snapshot_hash.is_some());
    }

    #[test]
    fn untracked_only_tree_counts_as_working_changes() {
        let dir = tmp_repo();
        write(&dir, "new.txt", "hello");
        assert!(observe(&dir).snapshot_hash.is_some());
    }

    #[test]
    fn unborn_repo_observes_none_head_and_sees_files() {
        let dir = tmp_repo_unborn();
        let clean = observe(&dir);
        assert_eq!(clean.head, None);
        assert!(clean.repo_root.is_some());
        write(&dir, "a.txt", "one");
        assert!(observe(&dir).snapshot_hash.is_some());
    }

    #[test]
    fn committed_patch_hashes_like_the_working_patch_it_was() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "one\n");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "base"], false).unwrap();
        write(&dir, "a.txt", "two\n");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        let (_, working_hash) = working_patch(&dir).expect("dirty tree has a patch");
        run_git(Some(&dir), &["commit", "-m", "change"], false).unwrap();
        let head = observe(&dir).head.unwrap();
        let (_, commit_hash) = commit_patch(&dir, &head).expect("plain commit has a patch");
        assert_eq!(
            working_hash, commit_hash,
            "committing the reviewed diff must hash identically for dedupe"
        );
    }

    #[test]
    fn commit_patch_contains_the_change() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "needle-content\n");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "add needle"], false).unwrap();
        let head = observe(&dir).head.unwrap();
        let (text, _) = commit_patch(&dir, &head).unwrap();
        assert!(text.contains("needle-content"));
    }

    #[test]
    fn merge_commits_are_skipped() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "base\n");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "base"], false).unwrap();
        run_git(Some(&dir), &["checkout", "-b", "side"], false).unwrap();
        write(&dir, "b.txt", "side\n");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "side"], false).unwrap();
        run_git(Some(&dir), &["checkout", "main"], false).unwrap();
        write(&dir, "c.txt", "main\n");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "main"], false).unwrap();
        run_git(Some(&dir), &["merge", "--no-ff", "side"], false).unwrap();
        let head = observe(&dir).head.unwrap();
        assert_eq!(commit_patch(&dir, &head), None);
    }

    #[test]
    fn untracked_only_tree_still_has_a_working_patch() {
        let dir = tmp_repo();
        write(&dir, "brand-new.txt", "hello");
        let (text, _) = working_patch(&dir).expect("untracked files are changes");
        assert!(text.contains("brand-new.txt"));
    }

    #[test]
    fn editing_untracked_content_changes_the_snapshot() {
        let dir = tmp_repo();
        write(&dir, "new.txt", "one");
        let first = observe(&dir).snapshot_hash.expect("untracked counts");
        // Same paths, different content: the stability clock must reset —
        // a half-written new file is exactly what must not get reviewed.
        write(&dir, "new.txt", "one plus more content");
        let second = observe(&dir).snapshot_hash.unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn untracked_content_appears_in_working_patch() {
        let dir = tmp_repo();
        write(&dir, "new.txt", "untracked-needle-content");
        let (text, first_hash) = working_patch(&dir).unwrap();
        assert!(text.contains("untracked-needle-content"));
        write(&dir, "new.txt", "untracked-needle-content changed");
        let (_, second_hash) = working_patch(&dir).unwrap();
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn binary_untracked_files_are_listed_not_dumped() {
        let dir = tmp_repo();
        std::fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150, 0, 7]).unwrap();
        let (text, _) = working_patch(&dir).unwrap();
        assert!(text.contains("blob.bin"));
        assert!(!text.contains('\u{0}'));
    }

    #[test]
    fn symlinked_untracked_files_are_never_excerpted() {
        let dir = tmp_repo();
        let outside = tmp_dir();
        std::fs::write(outside.join("secret.txt"), "TOP-SECRET-CREDENTIAL").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();
        let (text, _) = working_patch(&dir).expect("symlink is still a change");
        assert!(text.contains("link.txt"), "the link itself is listed");
        assert!(
            !text.contains("TOP-SECRET-CREDENTIAL"),
            "a symlink's target must never reach the agent"
        );
    }

    #[test]
    fn staged_files_in_unborn_repo_are_reviewable() {
        let dir = tmp_repo_unborn();
        write(&dir, "a.txt", "unborn-needle\n");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        let (text, _) = working_patch(&dir).expect("staged work in a fresh repo is a change");
        assert!(text.contains("unborn-needle"));
    }

    #[test]
    fn worktree_edits_after_staging_in_unborn_repo_reset_snapshot() {
        let dir = tmp_repo_unborn();
        write(&dir, "a.txt", "one\n");
        run_git(Some(&dir), &["add", "a.txt"], false).unwrap();
        write(&dir, "a.txt", "one\ntwo\n");
        let first = observe(&dir).snapshot_hash.unwrap();
        write(&dir, "a.txt", "one\ntwo\nthree\n");
        let second = observe(&dir).snapshot_hash.unwrap();
        assert_ne!(first, second, "post-staging edits must reset stability");
    }

    #[test]
    fn exotic_untracked_filenames_are_fingerprinted() {
        let dir = tmp_repo();
        write(&dir, "spaces and \u{f1}ame.txt", "one");
        let first = observe(&dir).snapshot_hash.unwrap();
        write(&dir, "spaces and \u{f1}ame.txt", "one but longer now");
        let second = observe(&dir).snapshot_hash.unwrap();
        assert_ne!(first, second, "quoted-name files must not be invisible");
    }

    #[test]
    fn untracked_growth_past_excerpt_window_changes_patch_hash() {
        let dir = tmp_repo();
        write(&dir, "big.txt", &"x".repeat(9000));
        let (_, first) = working_patch(&dir).unwrap();
        // The edit lands beyond both the 8KB read and the 2000-char excerpt:
        // the listing's size must still move the hash, or the change gets
        // deduped away forever.
        write(&dir, "big.txt", &"x".repeat(9500));
        let (_, second) = working_patch(&dir).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn oversized_commit_patch_falls_back_to_stat() {
        let dir = tmp_repo();
        // > 1MiB of unique lines so the full patch overflows run_git's cap.
        let big: String = (0..80_000)
            .map(|i| format!("this is padded line number {i}\n"))
            .collect();
        write(&dir, "huge.txt", &big);
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "huge"], false).unwrap();
        let head = observe(&dir).head.unwrap();
        let (text, _) = commit_patch(&dir, &head)
            .expect("an oversized commit must degrade to --stat, not vanish");
        assert!(text.contains("huge.txt"));
    }

    #[test]
    fn oversized_working_diff_falls_back_to_stat() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "small\n");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "base"], false).unwrap();
        let big: String = (0..80_000)
            .map(|i| format!("this is padded line number {i}\n"))
            .collect();
        write(&dir, "a.txt", &big);
        let (text, _) = working_patch(&dir)
            .expect("an oversized working diff must degrade to --stat, not vanish");
        assert!(text.contains("a.txt"));
    }

    #[test]
    fn oversized_edit_with_unchanged_stat_still_resets_snapshot() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "small\n");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "base"], false).unwrap();
        let big: String = (0..80_000)
            .map(|i| format!("this is padded line number {i}\n"))
            .collect();
        write(&dir, "a.txt", &big);
        let first = observe(&dir).snapshot_hash.unwrap();
        // Same file, same line count — the --stat fallback alone cannot see
        // this edit; the path fingerprint must.
        let swapped: String = (0..80_000)
            .map(|i| format!("this is swapped line number {i}\n"))
            .collect();
        write(&dir, "a.txt", &swapped);
        let second = observe(&dir).snapshot_hash.unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn staged_oversized_first_change_in_unborn_repo_is_reviewable() {
        let dir = tmp_repo_unborn();
        let big: String = (0..80_000)
            .map(|i| format!("this is padded line number {i}\n"))
            .collect();
        write(&dir, "huge.txt", &big);
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        let (text, _) = working_patch(&dir)
            .expect("an oversized first change must degrade to --stat, not vanish");
        assert!(text.contains("huge.txt"));
    }

    #[test]
    fn fresh_commit_is_classified_as_committed() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "one");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "work"], false).unwrap();
        assert!(observe(&dir).head_committed);
    }

    #[test]
    fn checkout_and_reset_are_not_classified_as_committed() {
        let dir = tmp_repo();
        write(&dir, "a.txt", "one");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "work"], false).unwrap();
        write(&dir, "a.txt", "two");
        run_git(Some(&dir), &["add", "-A"], false).unwrap();
        run_git(Some(&dir), &["commit", "-m", "more"], false).unwrap();
        run_git(Some(&dir), &["checkout", "HEAD~1"], false).unwrap();
        assert!(
            !observe(&dir).head_committed,
            "checking out an old commit is not a new commit"
        );
        run_git(Some(&dir), &["checkout", "main"], false).unwrap();
        run_git(Some(&dir), &["reset", "--hard", "HEAD~1"], false).unwrap();
        assert!(!observe(&dir).head_committed, "reset is not a new commit");
    }
}
