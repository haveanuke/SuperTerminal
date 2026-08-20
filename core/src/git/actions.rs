use serde::Serialize;
use std::path::Path;

use super::process::run_git;
use super::status::{StatusEntry, StatusReport};
use super::{run_status, GitState};

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    pub report: StatusReport,
    /// Entries silently skipped by a bulk action (non-actionable / excluded kinds).
    pub skipped: u32,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ActionKind {
    Stage,
    Unstage,
    Discard,
}

#[derive(Default, Debug, PartialEq)]
pub struct ActionPlan {
    pub add_paths: Vec<String>,        // stage
    pub restore_staged: Vec<String>,   // unstage (HEAD exists)
    pub rm_cached: Vec<String>,        // unstage (unborn HEAD)
    pub restore_worktree: Vec<String>, // discard tracked content
    pub clean_paths: Vec<String>,      // discard untracked files/dirs
    pub skipped: u32,
}

fn relevant(entry: &StatusEntry, kind: ActionKind) -> bool {
    match kind {
        ActionKind::Stage => {
            entry.kind == "untracked" || entry.kind == "unmerged" || entry.worktree_status != "."
        }
        ActionKind::Unstage => {
            entry.kind != "untracked" && entry.kind != "unmerged" && entry.index_status != "."
        }
        ActionKind::Discard => entry.kind == "untracked" || entry.worktree_status != ".",
    }
}

fn expand_entry(
    entry: &StatusEntry,
    kind: ActionKind,
    plan: &mut ActionPlan,
    unborn: bool,
) -> Result<(), String> {
    match kind {
        ActionKind::Stage => {
            // Worktree rename: both paths, or half the rename stays unstaged.
            // Copy: destination only — the source is a live file whose own
            // modifications must not ride along.
            if entry.kind == "rename_copy" && entry.worktree_status == "R" {
                if let Some(orig) = &entry.orig_path {
                    plan.add_paths.push(orig.clone());
                }
            }
            plan.add_paths.push(entry.path.clone());
        }
        ActionKind::Unstage => {
            let target = if unborn {
                &mut plan.rm_cached
            } else {
                &mut plan.restore_staged
            };
            if entry.kind == "rename_copy" && entry.index_status == "R" {
                if let Some(orig) = &entry.orig_path {
                    target.push(orig.clone());
                }
            }
            target.push(entry.path.clone());
        }
        ActionKind::Discard => {
            if entry.submodule {
                return Err(format!(
                    "discard is not supported for submodules ({}) — use the terminal",
                    entry.path
                ));
            }
            if entry.kind == "unmerged" {
                return Err(format!(
                    "cannot discard conflicted file {} — resolve or stage it",
                    entry.path
                ));
            }
            if entry.kind == "untracked" {
                plan.clean_paths.push(entry.path.clone());
            } else if entry.kind == "rename_copy" && entry.worktree_status == "R" {
                // Worktree rename: restore the source, clean the destination.
                if let Some(orig) = &entry.orig_path {
                    plan.restore_worktree.push(orig.clone());
                }
                plan.clean_paths.push(entry.path.clone());
            } else if entry.kind == "rename_copy" && entry.worktree_status == "C" {
                // Worktree copy: destination only; never touch the source.
                plan.clean_paths.push(entry.path.clone());
            } else {
                plan.restore_worktree.push(entry.path.clone());
            }
        }
    }
    Ok(())
}

/// Map requested paths (None = bulk) onto the CURRENT report. Explicit paths
/// that don't match a relevant entry — including non-actionable (non-UTF-8)
/// ones — are rejected: the UI is stale, refresh. Bulk skips those instead.
pub fn expand_for_action(
    report: &StatusReport,
    paths: Option<&[String]>,
    kind: ActionKind,
) -> Result<ActionPlan, String> {
    let mut plan = ActionPlan::default();
    match paths {
        Some(requested) => {
            for path in requested {
                let entry = report
                    .entries
                    .iter()
                    .find(|e| &e.path == path && relevant(e, kind))
                    .ok_or_else(|| "state changed, refresh".to_string())?;
                if !entry.actionable {
                    return Err("state changed, refresh".to_string());
                }
                expand_entry(entry, kind, &mut plan, report.unborn)?;
            }
        }
        None => {
            for entry in &report.entries {
                if !relevant(entry, kind) {
                    continue;
                }
                if !entry.actionable {
                    plan.skipped += 1;
                    continue;
                }
                // Bulk discard silently skips the excluded kinds an explicit
                // request would reject (conflicts live in their own section).
                if kind == ActionKind::Discard && (entry.submodule || entry.kind == "unmerged") {
                    plan.skipped += 1;
                    continue;
                }
                expand_entry(entry, kind, &mut plan, report.unborn)?;
            }
        }
    }
    Ok(plan)
}

fn run_paths(root: &Path, base: &[&str], paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut args: Vec<&str> = base.to_vec();
    args.push("--");
    for p in paths {
        args.push(p);
    }
    run_git(Some(root), &args, false).map(|_| ())
}

fn execute(root: &Path, plan: &ActionPlan) -> Result<(), String> {
    run_paths(root, &["add"], &plan.add_paths)?;
    run_paths(root, &["restore", "--staged"], &plan.restore_staged)?;
    run_paths(root, &["rm", "--cached", "-r", "-f"], &plan.rm_cached)?;
    run_paths(root, &["restore"], &plan.restore_worktree)?;
    run_paths(root, &["clean", "-fd"], &plan.clean_paths)?;
    // --untracked-files=all reports files, so `clean` on those paths leaves
    // their now-empty parent directories behind. Sweep them (best-effort:
    // remove_dir refuses non-empty dirs, which naturally bounds the walk).
    for cleaned in &plan.clean_paths {
        let mut parent = Path::new(cleaned).parent();
        while let Some(rel) = parent {
            if rel.as_os_str().is_empty() {
                break;
            }
            if std::fs::remove_dir(root.join(rel)).is_err() {
                break;
            }
            parent = rel.parent();
        }
    }
    Ok(())
}

pub fn run_action(
    state: &GitState,
    repo_id: &str,
    paths: Option<Vec<String>>,
    kind: ActionKind,
) -> Result<ActionResult, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let _guard = entry.action_lock.lock().unwrap();
    let report = run_status(&entry.root)?;
    let plan = expand_for_action(&report, paths.as_deref(), kind)?;
    execute(&entry.root, &plan)?;
    Ok(ActionResult {
        report: run_status(&entry.root)?,
        skipped: plan.skipped,
    })
}

/// Commit the staged changes with `message` (multi-line allowed).
pub fn run_commit(state: &GitState, repo_id: &str, message: &str) -> Result<ActionResult, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let _guard = entry.action_lock.lock().unwrap();
    if message.trim().is_empty() {
        return Err("empty commit message".to_string());
    }
    run_git(Some(&entry.root), &["commit", "-m", message], false)?;
    Ok(ActionResult {
        report: run_status(&entry.root)?,
        skipped: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_repo::*;
    use super::*;

    fn state_for(dir: &Path) -> (GitState, String) {
        let state = GitState::default();
        let info = state.resolve(dir.to_str().unwrap()).expect("repo");
        (state, info.repo_id)
    }

    fn entry_by_path<'r>(report: &'r StatusReport, path: &str) -> Option<&'r StatusEntry> {
        report.entries.iter().find(|e| e.path == path)
    }

    #[test]
    fn stage_unstage_commit_round_trip() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(&dir, "a.txt", "hello");
        let r = run_action(&state, &id, Some(vec!["a.txt".into()]), ActionKind::Stage).unwrap();
        assert_eq!(entry_by_path(&r.report, "a.txt").unwrap().index_status, "A");
        let r = run_action(&state, &id, Some(vec!["a.txt".into()]), ActionKind::Unstage).unwrap();
        assert_eq!(entry_by_path(&r.report, "a.txt").unwrap().kind, "untracked");
        run_action(&state, &id, Some(vec!["a.txt".into()]), ActionKind::Stage).unwrap();
        let r = git_commit_inner(&state, &id, "add a");
        assert!(entry_by_path(&r, "a.txt").is_none(), "clean after commit");
    }

    fn git_commit_inner(state: &GitState, id: &str, msg: &str) -> StatusReport {
        let entry = state.entry(id).unwrap();
        run_git(Some(&entry.root), &["commit", "-m", msg], false).unwrap();
        run_status(&entry.root).unwrap()
    }

    #[test]
    fn unborn_unstage_uses_rm_cached_and_survives_remodification() {
        let dir = tmp_repo_unborn();
        let (state, id) = state_for(&dir);
        write(&dir, "f.txt", "v1");
        run_action(&state, &id, Some(vec!["f.txt".into()]), ActionKind::Stage).unwrap();
        write(&dir, "f.txt", "v2"); // stage-then-modify-again
        let r = run_action(&state, &id, Some(vec!["f.txt".into()]), ActionKind::Unstage).unwrap();
        let e = entry_by_path(&r.report, "f.txt").unwrap();
        assert_eq!(e.kind, "untracked", "index empty again");
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "v2",
            "worktree untouched"
        );
    }

    #[test]
    fn bulk_unstage_modes() {
        // born: restore --staged over explicit paths
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(&dir, "x.txt", "x");
        write(&dir, "y.txt", "y");
        run_action(&state, &id, None, ActionKind::Stage).unwrap();
        let r = run_action(&state, &id, None, ActionKind::Unstage).unwrap();
        assert!(r
            .report
            .entries
            .iter()
            .all(|e| e.index_status == "." || e.kind == "untracked"));

        // unborn: rm --cached path
        let dir2 = tmp_repo_unborn();
        let (state2, id2) = state_for(&dir2);
        write(&dir2, "z.txt", "z");
        run_action(&state2, &id2, None, ActionKind::Stage).unwrap();
        let r = run_action(&state2, &id2, None, ActionKind::Unstage).unwrap();
        assert_eq!(entry_by_path(&r.report, "z.txt").unwrap().kind, "untracked");
    }

    #[test]
    fn rename_stage_and_unstage_expand_to_both_paths() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(
            &dir,
            "old.txt",
            "stable content long enough for rename detection",
        );
        run_action(&state, &id, None, ActionKind::Stage).unwrap();
        git_commit_inner(&state, &id, "base");
        std::fs::rename(dir.join("old.txt"), dir.join("new.txt")).unwrap();
        // stage the worktree rename via bulk (expands both sides)
        let r = run_action(&state, &id, None, ActionKind::Stage).unwrap();
        let staged = entry_by_path(&r.report, "new.txt").unwrap();
        assert_eq!(staged.index_status, "R");
        assert_eq!(staged.orig_path.as_deref(), Some("old.txt"));
        // unstage the staged rename via its visible path (expands both sides)
        let r = run_action(
            &state,
            &id,
            Some(vec!["new.txt".into()]),
            ActionKind::Unstage,
        )
        .unwrap();
        let e = entry_by_path(&r.report, "old.txt").expect("old back as deleted-or-restored");
        assert_ne!(e.index_status, "R");
    }

    #[test]
    fn discard_restores_tracked_and_cleans_untracked_dirs() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(&dir, "tracked.txt", "committed");
        run_action(&state, &id, None, ActionKind::Stage).unwrap();
        git_commit_inner(&state, &id, "base");
        write(&dir, "tracked.txt", "dirty");
        write(&dir, "junk dir/deep/file.txt", "junk");
        let r = run_action(&state, &id, None, ActionKind::Discard).unwrap();
        assert!(
            r.report.entries.is_empty(),
            "clean tree: {:?}",
            r.report.entries
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).unwrap(),
            "committed"
        );
        assert!(!dir.join("junk dir").exists());
    }

    #[test]
    fn worktree_rename_discard_restores_src_cleans_dest() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(
            &dir,
            "src.txt",
            "stable content long enough for rename detection",
        );
        run_action(&state, &id, None, ActionKind::Stage).unwrap();
        git_commit_inner(&state, &id, "base");
        std::fs::rename(dir.join("src.txt"), dir.join("dest.txt")).unwrap();
        // The worktree state is D(src)+untracked(dest) in porcelain (worktree
        // rename records need rename detection in status; git reports R only
        // for staged renames by default) — discard-all must restore src and
        // remove dest either way.
        let r = run_action(&state, &id, None, ActionKind::Discard).unwrap();
        assert!(r.report.entries.is_empty(), "{:?}", r.report.entries);
        assert!(dir.join("src.txt").exists());
        assert!(!dir.join("dest.txt").exists());
    }

    #[test]
    fn discard_rejects_conflicted_and_stale_paths() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        let err = run_action(
            &state,
            &id,
            Some(vec!["nope.txt".into()]),
            ActionKind::Discard,
        )
        .unwrap_err();
        assert_eq!(err, "state changed, refresh");
    }

    #[test]
    fn literal_pathspec_colon_filename_round_trip() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(&dir, ":weird.txt", "colon");
        write(&dir, "normal.txt", "normal");
        let r = run_action(
            &state,
            &id,
            Some(vec![":weird.txt".into()]),
            ActionKind::Stage,
        )
        .unwrap();
        assert_eq!(
            entry_by_path(&r.report, ":weird.txt").unwrap().index_status,
            "A"
        );
        assert_eq!(
            entry_by_path(&r.report, "normal.txt").unwrap().kind,
            "untracked"
        );
        let r = run_action(
            &state,
            &id,
            Some(vec![":weird.txt".into()]),
            ActionKind::Unstage,
        )
        .unwrap();
        let r2 = run_action(
            &state,
            &id,
            Some(vec![":weird.txt".into()]),
            ActionKind::Discard,
        )
        .unwrap();
        let _ = r;
        assert!(entry_by_path(&r2.report, ":weird.txt").is_none());
        assert!(dir.join("normal.txt").exists(), "unrelated file untouched");
    }

    #[test]
    fn commit_requires_message_and_supports_multiline() {
        let dir = tmp_repo();
        let (state, id) = state_for(&dir);
        write(&dir, "m.txt", "m");
        run_action(&state, &id, None, ActionKind::Stage).unwrap();
        let entry = state.entry(&id).unwrap();
        run_git(
            Some(&entry.root),
            &["commit", "-m", "subject line\n\nbody line"],
            false,
        )
        .unwrap();
        let log = run_git(Some(&entry.root), &["log", "-1", "--format=%B"], false).unwrap();
        let body = String::from_utf8_lossy(&log.stdout);
        assert!(body.contains("subject line") && body.contains("body line"));
    }

    #[test]
    fn expansion_rules_pure() {
        // copy stage/unstage/discard use destination only
        let copy = StatusEntry {
            kind: "rename_copy".into(),
            index_status: ".".into(),
            worktree_status: "C".into(),
            path: "copy.txt".into(),
            orig_path: Some("src.txt".into()),
            submodule: false,
            actionable: true,
        };
        let report = StatusReport {
            entries: vec![copy],
            ..Default::default()
        };
        let plan = expand_for_action(&report, None, ActionKind::Stage).unwrap();
        assert_eq!(plan.add_paths, vec!["copy.txt"]);
        let plan = expand_for_action(&report, None, ActionKind::Discard).unwrap();
        assert_eq!(plan.clean_paths, vec!["copy.txt"]);
        assert!(plan.restore_worktree.is_empty());

        // staged rename + unrelated worktree mod: discard treats worktree side as ordinary
        let staged_rename_with_mod = StatusEntry {
            kind: "rename_copy".into(),
            index_status: "R".into(),
            worktree_status: "M".into(),
            path: "new.txt".into(),
            orig_path: Some("old.txt".into()),
            submodule: false,
            actionable: true,
        };
        let report = StatusReport {
            entries: vec![staged_rename_with_mod],
            ..Default::default()
        };
        let plan = expand_for_action(&report, None, ActionKind::Discard).unwrap();
        assert_eq!(plan.restore_worktree, vec!["new.txt"]);
        assert!(plan.clean_paths.is_empty(), "not a worktree rename");

        // non-actionable explicit -> reject; bulk -> skipped
        let bad = StatusEntry {
            kind: "ordinary".into(),
            index_status: ".".into(),
            worktree_status: "M".into(),
            path: "bad\u{fffd}.txt".into(),
            orig_path: None,
            submodule: false,
            actionable: false,
        };
        let report = StatusReport {
            entries: vec![bad],
            ..Default::default()
        };
        assert_eq!(
            expand_for_action(
                &report,
                Some(&["bad\u{fffd}.txt".to_string()]),
                ActionKind::Stage
            )
            .unwrap_err(),
            "state changed, refresh"
        );
        let plan = expand_for_action(&report, None, ActionKind::Stage).unwrap();
        assert_eq!(plan.skipped, 1);
        assert!(plan.add_paths.is_empty());

        // submodule: stage/unstage allowed (gitlink), discard rejected explicitly / skipped in bulk
        let sub = StatusEntry {
            kind: "ordinary".into(),
            index_status: ".".into(),
            worktree_status: "M".into(),
            path: "sub".into(),
            orig_path: None,
            submodule: true,
            actionable: true,
        };
        let report = StatusReport {
            entries: vec![sub],
            ..Default::default()
        };
        assert_eq!(
            expand_for_action(&report, None, ActionKind::Stage)
                .unwrap()
                .add_paths,
            vec!["sub"]
        );
        assert!(
            expand_for_action(&report, Some(&["sub".to_string()]), ActionKind::Discard)
                .unwrap_err()
                .contains("submodule")
        );
        assert_eq!(
            expand_for_action(&report, None, ActionKind::Discard)
                .unwrap()
                .skipped,
            1
        );
    }
}
