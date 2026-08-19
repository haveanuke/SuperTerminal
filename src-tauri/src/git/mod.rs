pub mod process;

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::State;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    pub repo_id: String,
    pub display_name: String,
    pub root: String,
}

pub struct RepoEntry {
    pub root: PathBuf,
    /// Held for the duration of any mutating action (stage/commit/push/...).
    pub action_lock: Arc<Mutex<()>>,
    /// True while a status refresh for this repo is running.
    pub status_inflight: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct GitState {
    by_root: Mutex<HashMap<PathBuf, String>>,
    by_id: Mutex<HashMap<String, Arc<RepoEntry>>>,
    next_id: AtomicU64,
}

impl GitState {
    /// Resolve a working directory to its enclosing repo. Interned: the same
    /// canonical root always yields the same repo_id, so per-repo locks and
    /// busy-state can't be bypassed via duplicate handles.
    pub fn resolve(&self, cwd: &str) -> Option<RepoInfo> {
        let out = process::run_git(
            Some(Path::new(cwd)),
            &["rev-parse", "--show-toplevel"],
            false,
        )
        .ok()?;
        let root_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if root_str.is_empty() {
            return None;
        }
        let root = std::fs::canonicalize(&root_str).ok()?;
        let mut by_root = self.by_root.lock().unwrap();
        let id = by_root
            .entry(root.clone())
            .or_insert_with(|| {
                let id = format!("repo-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
                self.by_id.lock().unwrap().insert(
                    id.clone(),
                    Arc::new(RepoEntry {
                        root: root.clone(),
                        action_lock: Arc::new(Mutex::new(())),
                        status_inflight: Arc::new(AtomicBool::new(false)),
                    }),
                );
                id
            })
            .clone();
        let display_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root_str.clone());
        Some(RepoInfo {
            repo_id: id,
            display_name,
            root: root.to_string_lossy().into_owned(),
        })
    }

    pub fn entry(&self, repo_id: &str) -> Option<Arc<RepoEntry>> {
        self.by_id.lock().unwrap().get(repo_id).cloned()
    }
}

#[tauri::command]
pub fn git_resolve_repo(cwd: String, state: State<'_, GitState>) -> Option<RepoInfo> {
    state.resolve(&cwd)
}

#[cfg(test)]
pub(crate) mod test_repo {
    use super::process::run_git;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// Fresh temp git repo with identity configured and one empty commit.
    pub fn tmp_repo() -> PathBuf {
        let dir = tmp_dir();
        run_git(None, &["init", dir.to_str().unwrap()], false).unwrap();
        config_identity(&dir);
        run_git(
            Some(&dir),
            &["commit", "--allow-empty", "-m", "initial"],
            false,
        )
        .unwrap();
        dir
    }

    /// Fresh temp git repo with identity but NO commits (unborn HEAD).
    pub fn tmp_repo_unborn() -> PathBuf {
        let dir = tmp_dir();
        run_git(None, &["init", dir.to_str().unwrap()], false).unwrap();
        config_identity(&dir);
        dir
    }

    pub fn tmp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "st-git-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        // canonicalize so registry comparisons work on macOS /tmp symlink
        std::fs::canonicalize(&d).unwrap()
    }

    pub fn config_identity(dir: &std::path::Path) {
        run_git(Some(dir), &["config", "user.email", "t@example.com"], false).unwrap();
        run_git(Some(dir), &["config", "user.name", "Test"], false).unwrap();
    }

    pub fn write(dir: &std::path::Path, rel: &str, contents: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, contents).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_repo::*;

    #[test]
    fn resolves_and_interns_repo_ids() {
        let dir = tmp_repo();
        let state = GitState::default();
        let a = state.resolve(dir.to_str().unwrap()).expect("repo");
        let sub = dir.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let b = state.resolve(sub.to_str().unwrap()).expect("repo from subdir");
        assert_eq!(a.repo_id, b.repo_id);
        assert_eq!(a.root, b.root);
        assert!(state.entry(&a.repo_id).is_some());
        assert!(state.resolve("/").is_none());
    }
}
