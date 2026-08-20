use super::actions::ActionResult;
use super::process::run_git;
use super::{run_status, GitState};

fn network_action(state: &GitState, repo_id: &str, args: &[&str]) -> Result<ActionResult, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let _guard = entry.action_lock.lock().unwrap();
    run_git(Some(&entry.root), args, true)?;
    Ok(ActionResult {
        report: run_status(&entry.root)?,
        skipped: 0,
    })
}

pub fn run_push(
    state: &GitState,
    repo_id: &str,
    set_upstream: bool,
) -> Result<ActionResult, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    if set_upstream {
        let branch = {
            let _guard = entry.action_lock.lock().unwrap();
            let report = run_status(&entry.root)?;
            report
                .branch
                .ok_or("no branch to publish (detached HEAD)")?
        };
        return network_action(state, repo_id, &["push", "-u", "origin", &branch]);
    }
    // Distinguish "no upstream" so the renderer can offer to publish.
    {
        let _guard = entry.action_lock.lock().unwrap();
        let report = run_status(&entry.root)?;
        if report.upstream.is_none() && !report.unborn {
            return Err("no upstream".to_string());
        }
    }
    network_action(state, repo_id, &["push"])
}

pub fn run_pull(state: &GitState, repo_id: &str) -> Result<ActionResult, String> {
    network_action(state, repo_id, &["pull", "--ff-only"]).map_err(|e| {
        let lower = e.to_lowercase();
        if lower.contains("fast-forward") || lower.contains("diverg") || lower.contains("not possible") {
            format!("branches have diverged — pull cannot fast-forward; resolve in the terminal (merge or rebase). [{e}]")
        } else {
            e
        }
    })
}

pub fn run_fetch(state: &GitState, repo_id: &str) -> Result<ActionResult, String> {
    network_action(state, repo_id, &["fetch", "--prune"])
}

#[cfg(test)]
mod tests {
    use super::super::actions::{run_action, ActionKind};
    use super::super::test_repo::*;
    use super::*;
    use std::path::{Path, PathBuf};

    /// Work repo + local bare "origin" — push/pull/fetch fully offline.
    fn repo_with_remote() -> (PathBuf, PathBuf) {
        let bare = tmp_dir();
        run_git(
            None,
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
            false,
        )
        .unwrap();
        let work = tmp_repo();
        run_git(
            Some(&work),
            &["remote", "add", "origin", bare.to_str().unwrap()],
            false,
        )
        .unwrap();
        run_git(Some(&work), &["push", "-u", "origin", "main"], false).unwrap();
        (work, bare)
    }

    fn clone_of(bare: &Path) -> PathBuf {
        let dir = tmp_dir();
        run_git(
            None,
            &["clone", bare.to_str().unwrap(), dir.to_str().unwrap()],
            false,
        )
        .unwrap();
        config_identity(&dir);
        dir
    }

    fn commit_file(state: &GitState, id: &str, dir: &Path, name: &str) {
        write(dir, name, name);
        run_action(state, id, None, ActionKind::Stage).unwrap();
        run_git(Some(dir), &["commit", "-m", name], false).unwrap();
    }

    fn state_for(dir: &Path) -> (GitState, String) {
        let state = GitState::default();
        let info = state.resolve(dir.to_str().unwrap()).expect("repo");
        (state, info.repo_id)
    }

    #[test]
    fn push_pull_fetch_against_local_bare_remote() {
        let (work, bare) = repo_with_remote();
        let (state, id) = state_for(&work);
        commit_file(&state, &id, &work, "one.txt");
        let r = git_push_inner(&state, &id, false).unwrap();
        assert_eq!(r.report.ahead, 0);

        // second clone advances the remote
        let other = clone_of(&bare);
        let (state2, id2) = state_for(&other);
        commit_file(&state2, &id2, &other, "two.txt");
        git_push_inner(&state2, &id2, false).unwrap();

        // first repo: fetch -> behind 1; pull --ff-only -> caught up
        let r = network_action(&state, &id, &["fetch", "--prune"]).unwrap();
        assert_eq!(r.report.behind, 1, "{:?}", r.report);
        let r = network_action(&state, &id, &["pull", "--ff-only"]).unwrap();
        assert_eq!(r.report.behind, 0);
        assert!(work.join("two.txt").exists());
    }

    fn git_push_inner(
        state: &GitState,
        id: &str,
        set_upstream: bool,
    ) -> Result<ActionResult, String> {
        // mirror of the command body without tauri State
        let entry = state.entry(id).unwrap();
        if set_upstream {
            let branch = run_status(&entry.root)?.branch.ok_or("no branch")?;
            return network_action(state, id, &["push", "-u", "origin", &branch]);
        }
        if run_status(&entry.root)?.upstream.is_none() {
            return Err("no upstream".to_string());
        }
        network_action(state, id, &["push"])
    }

    #[test]
    fn push_without_upstream_errors_then_set_upstream_works() {
        let bare = tmp_dir();
        run_git(
            None,
            &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
            false,
        )
        .unwrap();
        let work = tmp_repo();
        run_git(
            Some(&work),
            &["remote", "add", "origin", bare.to_str().unwrap()],
            false,
        )
        .unwrap();
        let (state, id) = state_for(&work);
        assert_eq!(
            git_push_inner(&state, &id, false).unwrap_err(),
            "no upstream"
        );
        let r = git_push_inner(&state, &id, true).unwrap();
        assert_eq!(r.report.upstream.as_deref(), Some("origin/main"));
    }

    #[test]
    fn divergent_pull_reports_clear_error() {
        let (work, bare) = repo_with_remote();
        let (state, id) = state_for(&work);
        let other = clone_of(&bare);
        let (state2, id2) = state_for(&other);
        commit_file(&state2, &id2, &other, "theirs.txt");
        git_push_inner(&state2, &id2, false).unwrap();
        commit_file(&state, &id, &work, "ours.txt"); // diverged
        let err = network_action(&state, &id, &["pull", "--ff-only"]).unwrap_err();
        let e = err.to_lowercase();
        assert!(
            e.contains("fast-forward") || e.contains("divergent") || e.contains("diverging"),
            "{err}"
        );
    }
}
