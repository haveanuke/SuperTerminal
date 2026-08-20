use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StatusEntry {
    /// "ordinary" | "rename_copy" | "unmerged" | "untracked"
    pub kind: String,
    /// One char; "." means none.
    pub index_status: String,
    pub worktree_status: String,
    /// Lossy display when the raw path is not valid UTF-8.
    pub path: String,
    pub orig_path: Option<String>,
    pub submodule: bool,
    /// False when path (or orig_path) bytes were not valid UTF-8 — every
    /// action on such an entry is rejected.
    pub actionable: bool,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    pub branch: Option<String>,
    /// Short oid when HEAD is detached.
    pub detached: Option<String>,
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub unborn: bool,
    pub entries: Vec<StatusEntry>,
}

/// Split off `n_meta` space-separated ASCII metadata tokens; the remainder is
/// the raw (possibly non-UTF-8) pathname bytes. Bounded — paths may contain
/// spaces, tabs, and newlines and must never be split.
fn split_meta(record: &[u8], n_meta: usize) -> Option<(Vec<String>, &[u8])> {
    let mut tokens = Vec::with_capacity(n_meta);
    let mut rest = record;
    for _ in 0..n_meta {
        let sp = rest.iter().position(|&b| b == b' ')?;
        tokens.push(String::from_utf8(rest[..sp].to_vec()).ok()?);
        rest = &rest[sp + 1..];
    }
    Some((tokens, rest))
}

fn path_string(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), true),
        Err(_) => (String::from_utf8_lossy(bytes).into_owned(), false),
    }
}

fn xy(token: &str) -> (String, String) {
    let mut chars = token.chars();
    let x = chars.next().unwrap_or('.').to_string();
    let y = chars.next().unwrap_or('.').to_string();
    (x, y)
}

pub fn parse_status(bytes: &[u8]) -> StatusReport {
    let mut report = StatusReport::default();
    let mut oid: Option<String> = None;
    let records: Vec<&[u8]> = bytes.split(|&b| b == 0).filter(|r| !r.is_empty()).collect();
    let mut i = 0;
    while i < records.len() {
        let record = records[i];
        i += 1;
        match record.first() {
            Some(b'#') => {
                let line = String::from_utf8_lossy(record);
                if let Some(v) = line.strip_prefix("# branch.oid ") {
                    oid = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("# branch.head ") {
                    let v = v.trim();
                    if v != "(detached)" {
                        report.branch = Some(v.to_string());
                    }
                } else if let Some(v) = line.strip_prefix("# branch.upstream ") {
                    report.upstream = Some(v.trim().to_string());
                } else if let Some(v) = line.strip_prefix("# branch.ab ") {
                    for part in v.trim().split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            report.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            report.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
            }
            Some(b'1') => {
                // 1 XY sub mH mI mW hH hI <path>  => 8 meta tokens + path
                if let Some((tokens, path_bytes)) = split_meta(record, 8) {
                    let (x, y) = xy(&tokens[1]);
                    let (path, ok) = path_string(path_bytes);
                    report.entries.push(StatusEntry {
                        kind: "ordinary".into(),
                        index_status: x,
                        worktree_status: y,
                        path,
                        orig_path: None,
                        submodule: tokens[2].starts_with('S'),
                        actionable: ok,
                    });
                }
            }
            Some(b'2') => {
                // 2 XY sub mH mI mW hH hI Xscore <path>  => 9 meta tokens + path;
                // the NEXT NUL record is the origin path.
                if let Some((tokens, path_bytes)) = split_meta(record, 9) {
                    let (x, y) = xy(&tokens[1]);
                    let (path, path_ok) = path_string(path_bytes);
                    let (orig, orig_ok) = if i < records.len() {
                        let r = records[i];
                        i += 1;
                        let (p, ok) = path_string(r);
                        (Some(p), ok)
                    } else {
                        (None, false)
                    };
                    report.entries.push(StatusEntry {
                        kind: "rename_copy".into(),
                        index_status: x,
                        worktree_status: y,
                        path,
                        orig_path: orig,
                        submodule: tokens[2].starts_with('S'),
                        actionable: path_ok && orig_ok,
                    });
                }
            }
            Some(b'u') => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 <path> => 10 meta tokens + path
                // (arity pinned by the real-conflict integration fixture below)
                if let Some((tokens, path_bytes)) = split_meta(record, 10) {
                    let (x, y) = xy(&tokens[1]);
                    let (path, ok) = path_string(path_bytes);
                    report.entries.push(StatusEntry {
                        kind: "unmerged".into(),
                        index_status: x,
                        worktree_status: y,
                        path,
                        orig_path: None,
                        submodule: tokens[2].starts_with('S'),
                        actionable: ok,
                    });
                }
            }
            Some(b'?') => {
                let (path, ok) = path_string(&record[2.min(record.len())..]);
                report.entries.push(StatusEntry {
                    kind: "untracked".into(),
                    index_status: ".".into(),
                    worktree_status: "?".into(),
                    path,
                    orig_path: None,
                    submodule: false,
                    actionable: ok,
                });
            }
            _ => {} // `!` ignored records (not requested) and anything unknown
        }
    }
    if let Some(oid) = oid {
        if oid == "(initial)" {
            report.unborn = true;
        } else if report.branch.is_none() {
            report.detached = Some(oid.chars().take(8).collect());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Join records with NUL and append the trailing NUL `-z` emits.
    fn z(parts: &[&str]) -> Vec<u8> {
        let mut v = parts.join("\0").into_bytes();
        v.push(0);
        v
    }

    #[test]
    fn parses_branch_headers_and_ab() {
        let r = parse_status(&z(&[
            "# branch.oid 1234567890abcdef",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
        ]));
        assert_eq!(r.branch.as_deref(), Some("main"));
        assert_eq!(r.upstream.as_deref(), Some("origin/main"));
        assert_eq!(r.ahead, 2);
        assert_eq!(r.behind, 1);
        assert!(!r.unborn);
        assert!(r.detached.is_none());
    }

    #[test]
    fn detached_head() {
        let r = parse_status(&z(&["# branch.oid 1234567890abcdef", "# branch.head (detached)"]));
        assert_eq!(r.branch, None);
        assert_eq!(r.detached.as_deref(), Some("12345678"));
    }

    #[test]
    fn unborn_head() {
        let r = parse_status(&z(&["# branch.oid (initial)", "# branch.head main"]));
        assert!(r.unborn);
        assert_eq!(r.branch.as_deref(), Some("main"));
    }

    #[test]
    fn ordinary_staged_and_unstaged_same_file() {
        let r = parse_status(&z(&[
            "1 MM N... 100644 100644 100644 aaaa bbbb a.txt",
        ]));
        let e = &r.entries[0];
        assert_eq!(e.kind, "ordinary");
        assert_eq!(e.index_status, "M");
        assert_eq!(e.worktree_status, "M");
        assert_eq!(e.path, "a.txt");
        assert!(e.actionable);
    }

    #[test]
    fn rename_carries_orig_path_as_next_record() {
        let r = parse_status(&z(&[
            "2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.txt",
            "old name.txt",
            "1 .M N... 100644 100644 100644 cccc dddd after.txt",
        ]));
        assert_eq!(r.entries.len(), 2);
        let e = &r.entries[0];
        assert_eq!(e.kind, "rename_copy");
        assert_eq!(e.index_status, "R");
        assert_eq!(e.path, "new name.txt");
        assert_eq!(e.orig_path.as_deref(), Some("old name.txt"));
        // and the record AFTER the orig path parses normally
        assert_eq!(r.entries[1].path, "after.txt");
    }

    #[test]
    fn worktree_copy_marker() {
        let r = parse_status(&z(&[
            "2 .C N... 100644 100644 100644 aaaa bbbb C90 copy.txt",
            "src.txt",
        ]));
        assert_eq!(r.entries[0].worktree_status, "C");
    }

    #[test]
    fn unmerged_records_parse_path_and_statuses() {
        for xy in ["UU", "AA", "DU"] {
            let rec = format!(
                "u {xy} N... 100644 100644 100644 100644 aaaa bbbb cccc conflicted file.txt"
            );
            let r = parse_status(&z(&[&rec]));
            let e = &r.entries[0];
            assert_eq!(e.kind, "unmerged", "{xy}");
            assert_eq!(e.index_status, &xy[0..1]);
            assert_eq!(e.worktree_status, &xy[1..2]);
            assert_eq!(e.path, "conflicted file.txt");
        }
    }

    #[test]
    fn untracked_paths_with_spaces_tabs_newlines_unicode() {
        let weird = "some dir/naïve\tfile\nwith newline.txt";
        let rec = format!("? {weird}");
        let r = parse_status(&z(&[&rec]));
        assert_eq!(r.entries[0].kind, "untracked");
        assert_eq!(r.entries[0].path, weird);
    }

    #[test]
    fn submodule_flag() {
        let r = parse_status(&z(&[
            "1 .M SCMU 160000 160000 160000 aaaa bbbb sub/module",
        ]));
        assert!(r.entries[0].submodule);
    }

    #[test]
    fn non_utf8_path_is_lossy_and_not_actionable() {
        let mut rec = b"1 .M N... 100644 100644 100644 aaaa bbbb bad-".to_vec();
        rec.push(0xff);
        rec.extend_from_slice(b".txt");
        let mut bytes = rec;
        bytes.push(0);
        let r = parse_status(&bytes);
        assert!(!r.entries[0].actionable);
        assert!(r.entries[0].path.contains('\u{fffd}'));
    }

    #[test]
    fn rename_with_non_utf8_on_either_side_is_not_actionable() {
        // invalid dest
        let mut bytes = b"2 R. N... 100644 100644 100644 aaaa bbbb R100 bad-".to_vec();
        bytes.push(0xff);
        bytes.push(0);
        bytes.extend_from_slice(b"good-src.txt");
        bytes.push(0);
        let r = parse_status(&bytes);
        assert!(!r.entries[0].actionable);
        // invalid source
        let mut bytes = b"2 R. N... 100644 100644 100644 aaaa bbbb R100 good-dest.txt".to_vec();
        bytes.push(0);
        bytes.extend_from_slice(b"bad-");
        bytes.push(0xff);
        bytes.push(0);
        let r = parse_status(&bytes);
        assert!(!r.entries[0].actionable);
    }
}
