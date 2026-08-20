use serde::Serialize;

use super::process::run_git;
use super::GitState;

#[derive(Clone, Debug, Default)]
pub struct CommitNode {
    pub hash: String,
    pub parents: Vec<String>,
    /// `%D` kept verbatim as a display string — commas are legal in ref names,
    /// so no logic may depend on splitting this.
    pub refs_display: String,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from_lane: usize,
    pub to_lane: usize,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct GraphRow {
    pub hash: String,
    pub lane: usize,
    /// Connections drawn between THIS row and the NEXT row.
    pub edges: Vec<Edge>,
    pub refs_display: String,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct GraphData {
    pub rows: Vec<GraphRow>,
    pub lane_count: usize,
}

const FIELD_SEP: char = '\u{1f}';
pub const MAX_LIMIT: u32 = 1000;

/// Records are NUL-separated; six fields split on \x1f with the subject LAST
/// and parsed with a bounded split, so a subject containing \x1f survives.
pub fn parse_log(bytes: &[u8]) -> Vec<CommitNode> {
    let mut nodes = Vec::new();
    for record in bytes.split(|&b| b == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        let fields: Vec<&str> = text.splitn(6, FIELD_SEP).collect();
        if fields.len() < 6 {
            continue;
        }
        nodes.push(CommitNode {
            hash: fields[0].to_string(),
            parents: fields[1].split_whitespace().map(String::from).collect(),
            refs_display: fields[2].to_string(),
            author: fields[3].to_string(),
            time: fields[4].parse().unwrap_or(0),
            subject: fields[5].to_string(),
        });
    }
    nodes
}

/// Lane assignment over topo-ordered commits (newest first). `active[j]` holds
/// the hash lane j expects next; freed slots are reused to keep the graph narrow.
pub fn layout(commits: &[CommitNode]) -> GraphData {
    let mut active: Vec<Option<String>> = Vec::new();
    let mut rows: Vec<GraphRow> = Vec::new();
    let mut lane_count = 0usize;

    for commit in commits {
        // 1. This commit's lane: leftmost expecting it, else first free, else append.
        let lane = match active
            .iter()
            .position(|s| s.as_deref() == Some(&commit.hash))
        {
            Some(j) => j,
            None => match active.iter().position(|s| s.is_none()) {
                Some(j) => {
                    active[j] = Some(commit.hash.clone());
                    j
                }
                None => {
                    active.push(Some(commit.hash.clone()));
                    active.len() - 1
                }
            },
        };

        let mut edges: Vec<Edge> = Vec::new();

        // 2. Fold any OTHER lane expecting this same hash into `lane` on this row.
        #[allow(clippy::needless_range_loop)] // active[j] is mutated mid-scan
        for j in 0..active.len() {
            if j != lane && active[j].as_deref() == Some(&commit.hash) {
                edges.push(Edge {
                    from_lane: j,
                    to_lane: lane,
                });
                active[j] = None;
            }
        }

        // 3. Continuation edges for every other occupied lane.
        for (j, slot) in active.iter().enumerate() {
            if j != lane && slot.is_some() {
                edges.push(Edge {
                    from_lane: j,
                    to_lane: j,
                });
            }
        }

        // 4. Assign parents.
        let mut parents = commit.parents.iter();
        match parents.next() {
            None => {
                // Root: lane closes, no downward edge.
                active[lane] = None;
            }
            Some(p0) => {
                if let Some(k) = active
                    .iter()
                    .enumerate()
                    .position(|(j, s)| j != lane && s.as_deref() == Some(p0.as_str()))
                {
                    // First parent already expected elsewhere: fold into it NOW
                    // (this is what merges a feature branch into its base on the
                    // feature commit's own row).
                    edges.push(Edge {
                        from_lane: lane,
                        to_lane: k,
                    });
                    active[lane] = None;
                } else {
                    active[lane] = Some(p0.clone());
                    edges.push(Edge {
                        from_lane: lane,
                        to_lane: lane,
                    });
                }
                for pi in parents {
                    if let Some(k) = active
                        .iter()
                        .position(|s| s.as_deref() == Some(pi.as_str()))
                    {
                        edges.push(Edge {
                            from_lane: lane,
                            to_lane: k,
                        });
                    } else {
                        let k = match active.iter().position(|s| s.is_none()) {
                            Some(k) => {
                                active[k] = Some(pi.clone());
                                k
                            }
                            None => {
                                active.push(Some(pi.clone()));
                                active.len() - 1
                            }
                        };
                        edges.push(Edge {
                            from_lane: lane,
                            to_lane: k,
                        });
                    }
                }
            }
        }

        lane_count = lane_count.max(active.iter().filter(|s| s.is_some()).count().max(lane + 1));

        rows.push(GraphRow {
            hash: commit.hash.clone(),
            lane,
            edges,
            refs_display: commit.refs_display.clone(),
            author: commit.author.clone(),
            time: commit.time,
            subject: commit.subject.clone(),
        });
    }

    GraphData { rows, lane_count }
}

pub fn run_graph(state: &GitState, repo_id: &str, limit: u32) -> Result<GraphData, String> {
    let entry = state.entry(repo_id).ok_or("unknown repo")?;
    let limit = limit.clamp(1, MAX_LIMIT).to_string();
    let out = run_git(
        Some(&entry.root),
        &[
            "log",
            "--all",
            "--topo-order",
            "-z",
            "--format=%H\u{1f}%P\u{1f}%D\u{1f}%an\u{1f}%at\u{1f}%s",
            "-n",
            &limit,
        ],
        false,
    )?;
    Ok(layout(&parse_log(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hash: &str, parents: &[&str]) -> CommitNode {
        CommitNode {
            hash: hash.to_string(),
            parents: parents.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn linear_history_is_single_lane() {
        let g = layout(&[c("c3", &["c2"]), c("c2", &["c1"]), c("c1", &[])]);
        assert!(g.rows.iter().all(|r| r.lane == 0));
        assert_eq!(g.lane_count, 1);
    }

    #[test]
    fn branch_and_merge_opens_and_closes_a_lane() {
        // m merges f into main: m(p: a, f), f(p: a), a(p: root), root
        let g = layout(&[
            c("m", &["a", "f"]),
            c("f", &["a"]),
            c("a", &["root"]),
            c("root", &[]),
        ]);
        assert_eq!(g.rows[0].lane, 0);
        assert_eq!(g.rows[1].lane, 1); // f on its own lane
        assert_eq!(g.rows[2].lane, 0); // a back on lane 0
                                       // f's line folds into lane 0 on f's own row:
        assert!(
            g.rows[1]
                .edges
                .iter()
                .any(|e| e.from_lane == 1 && e.to_lane == 0),
            "{:?}",
            g.rows[1].edges
        );
        assert_eq!(g.lane_count, 2);
    }

    #[test]
    fn octopus_merge_opens_a_lane_per_extra_parent() {
        let g = layout(&[
            c("m", &["a", "b", "d"]),
            c("a", &[]),
            c("b", &[]),
            c("d", &[]),
        ]);
        assert_eq!(g.lane_count, 3);
        // m's row carries an edge to each parent lane
        assert_eq!(g.rows[0].edges.len(), 3);
    }

    #[test]
    fn orphan_branch_gets_fresh_lane() {
        let g = layout(&[c("x2", &["x1"]), c("o1", &[]), c("x1", &[])]);
        assert_ne!(g.rows[1].lane, g.rows[0].lane);
    }

    #[test]
    fn freed_lanes_are_reused() {
        // Two sequential feature branches must not widen the graph forever.
        let g = layout(&[
            c("m2", &["m1", "f2"]),
            c("f2", &["m1"]),
            c("m1", &["m0", "f1"]),
            c("f1", &["m0"]),
            c("m0", &[]),
        ]);
        assert_eq!(g.lane_count, 2, "{:?}", g.rows);
    }

    #[test]
    fn parse_log_bounded_split_preserves_odd_subjects() {
        let raw = "h1\u{1f}p1 p2\u{1f}HEAD -> main, tag: v1, weird,ref\u{1f}Alice\u{1f}1700000000\u{1f}subject with \u{1f} and, commas\0h2\u{1f}\u{1f}\u{1f}Bob\u{1f}1700000001\u{1f}root commit\0".to_string();
        let nodes = parse_log(raw.as_bytes());
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].parents, vec!["p1", "p2"]);
        assert_eq!(nodes[0].refs_display, "HEAD -> main, tag: v1, weird,ref");
        assert_eq!(nodes[0].subject, "subject with \u{1f} and, commas");
        assert!(nodes[1].parents.is_empty());
        assert_eq!(nodes[1].subject, "root commit");
    }
}
