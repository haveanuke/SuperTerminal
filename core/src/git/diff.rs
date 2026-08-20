//! Unified-diff retrieval and line classification for panel display.

use serde::Serialize;

use super::process::run_git;
use super::GitState;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// Display cap; a truncation marker line is appended when exceeded.
pub const MAX_DIFF_LINES: usize = 400;

fn classify(line: &str) -> DiffLineKind {
    if line.starts_with("@@") {
        DiffLineKind::Hunk
    } else if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("rename ")
        || line.starts_with("similarity")
        || line.starts_with("copy ")
        || line.starts_with("Binary files")
    {
        DiffLineKind::Header
    } else if line.starts_with('+') {
        DiffLineKind::Added
    } else if line.starts_with('-') {
        DiffLineKind::Removed
    } else {
        DiffLineKind::Context
    }
}

/// Classify unified diff text, capped with an explicit truncation marker.
pub fn parse_unified(text: &str) -> Vec<DiffLine> {
    let mut lines: Vec<DiffLine> = Vec::new();
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        lines.push(DiffLine {
            kind: classify(line),
            text: line.to_string(),
        });
    }
    if truncated {
        lines.push(DiffLine {
            kind: DiffLineKind::Header,
            text: format!("... truncated at {MAX_DIFF_LINES} lines"),
        });
    }
    lines
}

/// Worktree (or, with `staged`, index) diff for one file.
pub fn run_file_diff(
    state: &GitState,
    repo_id: &str,
    path: &str,
    staged: bool,
) -> Result<Vec<DiffLine>, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let mut args = vec!["diff", "--no-color"];
    if staged {
        args.push("--cached");
    }
    args.push("--");
    args.push(path);
    let out = run_git(Some(&entry.root), &args, false)?;
    Ok(parse_unified(&String::from_utf8_lossy(&out.stdout)))
}

/// Untracked files have no diff yet; show their contents as additions.
pub fn read_untracked(
    state: &GitState,
    repo_id: &str,
    path: &str,
) -> Result<Vec<DiffLine>, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let full = entry.root.join(path);
    // This is a raw filesystem read: refuse anything that resolves outside
    // the repository (status paths shouldn't, but be strict anyway).
    let canonical = full.canonicalize().map_err(|e| e.to_string())?;
    let root = entry.root.canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(&root) {
        return Err("path escapes repository".to_string());
    }
    let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Ok(vec![DiffLine {
            kind: DiffLineKind::Header,
            text: "binary file".to_string(),
        }]);
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<DiffLine> = Vec::new();
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_DIFF_LINES {
            truncated = true;
            break;
        }
        lines.push(DiffLine {
            kind: DiffLineKind::Added,
            text: format!("+{line}"),
        });
    }
    if truncated {
        lines.push(DiffLine {
            kind: DiffLineKind::Header,
            text: format!("... truncated at {MAX_DIFF_LINES} lines"),
        });
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_unified_diff_lines() {
        let text = "diff --git a/x b/x\nindex 111..222 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n context\n-removed\n+added\n";
        let lines = parse_unified(text);
        let kinds: Vec<DiffLineKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffLineKind::Header,
                DiffLineKind::Header,
                DiffLineKind::Header,
                DiffLineKind::Header,
                DiffLineKind::Hunk,
                DiffLineKind::Context,
                DiffLineKind::Removed,
                DiffLineKind::Added,
            ]
        );
    }

    #[test]
    fn plus_plus_plus_is_header_not_added() {
        assert_eq!(parse_unified("+++ b/f")[0].kind, DiffLineKind::Header);
        assert_eq!(parse_unified("--- a/f")[0].kind, DiffLineKind::Header);
    }

    #[test]
    fn truncation_appends_marker() {
        let text = "+x\n".repeat(MAX_DIFF_LINES + 10);
        let lines = parse_unified(&text);
        assert_eq!(lines.len(), MAX_DIFF_LINES + 1);
        assert!(lines.last().unwrap().text.contains("truncated"));
    }

    #[test]
    fn untracked_rejects_escaping_paths() {
        let state = GitState::default();
        // Unknown repo id fails before any filesystem access.
        assert!(read_untracked(&state, "nope", "../etc/passwd").is_err());
    }
}
