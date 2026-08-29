//! Tab/pane-tree layout logic, ported from
//! `src/renderer/stores/terminal-store.ts`.
//!
//! Pure data structures — no UI dependencies. The JSON representation matches
//! the session-file schema enforced by `src-tauri/src/session.rs::validate_pane`
//! exactly: `{"type":"terminal","terminalId":...}` and
//! `{"type":"split","direction":"horizontal"|"vertical","children":[..,..],"sizes":[..,..]?}`,
//! with tabs as `{"id","label","pane"}` under `{"tabs":[..],"activeTabId":..}`.

use serde::{Deserialize, Serialize};

use crate::hosts::Target;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PaneNode {
    Terminal {
        terminal_id: String,
        /// Omitted when Local, so existing saved layouts are byte-identical
        /// and older files load as Local.
        #[serde(default, skip_serializing_if = "Target::is_local")]
        target: Target,
    },
    Split {
        direction: SplitDirection,
        /// Always exactly 2 children (enforced on session-JSON load).
        children: Vec<PaneNode>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sizes: Option<[f32; 2]>,
    },
}

impl PaneNode {
    pub fn terminal(terminal_id: impl Into<String>) -> Self {
        PaneNode::Terminal {
            terminal_id: terminal_id.into(),
            target: Target::Local,
        }
    }

    /// True when every split in the tree has exactly two children.
    fn is_valid(&self) -> bool {
        match self {
            PaneNode::Terminal { .. } => true,
            PaneNode::Split { children, .. } => {
                children.len() == 2 && children.iter().all(PaneNode::is_valid)
            }
        }
    }
}

/// Replace the terminal node `target` with a split whose first child is the
/// old terminal and whose second child is a new terminal `new_id`.
///
/// Nodes that don't contain `target` are returned unchanged; the new split
/// carries no explicit sizes (even 50/50 by convention), while existing splits
/// keep their direction and sizes.
pub fn insert_split(
    pane: &PaneNode,
    target: &str,
    direction: SplitDirection,
    new_id: &str,
) -> PaneNode {
    match pane {
        PaneNode::Terminal { terminal_id, .. } if terminal_id == target => PaneNode::Split {
            direction,
            children: vec![pane.clone(), PaneNode::terminal(new_id)],
            sizes: None,
        },
        PaneNode::Terminal { .. } => pane.clone(),
        PaneNode::Split {
            direction: dir,
            children,
            sizes,
        } => PaneNode::Split {
            direction: *dir,
            children: children
                .iter()
                .map(|child| insert_split(child, target, direction, new_id))
                .collect(),
            sizes: *sizes,
        },
    }
}

/// Remove the terminal `terminal_id` from the tree.
///
/// A split left with a single child collapses to that child (dropping the
/// split's direction/sizes); removing the last terminal yields `None`.
pub fn remove_terminal(pane: &PaneNode, terminal_id: &str) -> Option<PaneNode> {
    match pane {
        PaneNode::Terminal {
            terminal_id: id, ..
        } => {
            if id == terminal_id {
                None
            } else {
                Some(pane.clone())
            }
        }
        PaneNode::Split {
            direction,
            children,
            sizes,
        } => {
            let left = children
                .first()
                .and_then(|c| remove_terminal(c, terminal_id));
            let right = children
                .get(1)
                .and_then(|c| remove_terminal(c, terminal_id));
            match (left, right) {
                (None, None) => None,
                (Some(only), None) | (None, Some(only)) => Some(only),
                (Some(left), Some(right)) => Some(PaneNode::Split {
                    direction: *direction,
                    children: vec![left, right],
                    sizes: *sizes,
                }),
            }
        }
    }
}

/// The target currently attached to `id` in the tree, if `id` appears.
fn target_of(pane: &PaneNode, id: &str) -> Option<Target> {
    match pane {
        PaneNode::Terminal {
            terminal_id,
            target,
        } => (terminal_id == id).then(|| target.clone()),
        PaneNode::Split { children, .. } => children.iter().find_map(|c| target_of(c, id)),
    }
}

/// Swap the positions of terminals `a` and `b` in the tree.
///
/// If only one of the two ids is present, that node is relabeled with the
/// other id (matching the TS store's behavior).
///
/// A terminal's target belongs to its id, not to its tree position: the
/// node that becomes `b` must carry `b`'s own target (found elsewhere in
/// the tree before either id is relabeled), and likewise for `a`. Reusing
/// the position's OLD target here would silently attach the wrong target
/// to a relabeled id — the same "becomes Local by accident" bug this slice
/// exists to prevent, just approached from the other direction.
#[allow(dead_code)] // pane-swap UI is v1.x; logic + tests kept for parity
pub fn swap_terminals(pane: &PaneNode, a: &str, b: &str) -> PaneNode {
    let target_a = target_of(pane, a);
    let target_b = target_of(pane, b);
    swap_terminals_inner(pane, a, b, target_a.as_ref(), target_b.as_ref())
}

fn swap_terminals_inner(
    pane: &PaneNode,
    a: &str,
    b: &str,
    target_a: Option<&Target>,
    target_b: Option<&Target>,
) -> PaneNode {
    match pane {
        PaneNode::Terminal {
            terminal_id,
            target,
        } => {
            if terminal_id == a {
                PaneNode::Terminal {
                    terminal_id: b.to_string(),
                    // b's own target moves in with it; if b doesn't exist
                    // anywhere in the tree, there is nothing to swap with,
                    // so this node keeps its own target rather than
                    // inventing one.
                    target: target_b.cloned().unwrap_or_else(|| target.clone()),
                }
            } else if terminal_id == b {
                PaneNode::Terminal {
                    terminal_id: a.to_string(),
                    target: target_a.cloned().unwrap_or_else(|| target.clone()),
                }
            } else {
                pane.clone()
            }
        }
        PaneNode::Split {
            direction,
            children,
            sizes,
        } => PaneNode::Split {
            direction: *direction,
            children: children
                .iter()
                .map(|child| swap_terminals_inner(child, a, b, target_a, target_b))
                .collect(),
            sizes: *sizes,
        },
    }
}

/// All (terminal id, target) pairs in the tree, depth-first, left to right.
/// Used on restore, where each leaf must be resolved against the live
/// profile list rather than unconditionally respawned locally. The sole
/// recursion over the tree for this purpose — `collect_terminal_ids` is
/// defined in terms of it below so the two can never drift apart.
pub fn collect_terminal_targets(pane: &PaneNode) -> Vec<(String, Target)> {
    match pane {
        PaneNode::Terminal {
            terminal_id,
            target,
        } => vec![(terminal_id.clone(), target.clone())],
        PaneNode::Split { children, .. } => {
            children.iter().flat_map(collect_terminal_targets).collect()
        }
    }
}

/// All terminal ids in the tree, depth-first, left to right.
pub fn collect_terminal_ids(pane: &PaneNode) -> Vec<String> {
    collect_terminal_targets(pane)
        .into_iter()
        .map(|(terminal_id, _)| terminal_id)
        .collect()
}

/// A project: one or more full-pane WINDOWS (each its own split tree), with
/// exactly one showing at a time.
///
/// Serialization stays compatible with the shared session schema: `pane`
/// carries the active window (what the Tauri app understands), and the full
/// window list rides alongside in `windows`/`activeWindow` (ignored by old
/// readers, defaulted by this one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TabDto", into = "TabDto")]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub windows: Vec<PaneNode>,
    pub active_window: usize,
}

impl Tab {
    pub fn single(id: impl Into<String>, label: impl Into<String>, pane: PaneNode) -> Tab {
        Tab {
            id: id.into(),
            label: label.into(),
            windows: vec![pane],
            active_window: 0,
        }
    }

    /// The window currently shown.
    pub fn active_pane(&self) -> &PaneNode {
        &self.windows[self.active_window.min(self.windows.len() - 1)]
    }

    pub fn active_pane_mut(&mut self) -> &mut PaneNode {
        let index = self.active_window.min(self.windows.len() - 1);
        &mut self.windows[index]
    }

    /// Every terminal in every window of this project.
    pub fn all_terminal_ids(&self) -> Vec<String> {
        self.windows.iter().flat_map(collect_terminal_ids).collect()
    }

    /// Every (terminal id, target) pair in every window of this project.
    pub fn all_terminal_targets(&self) -> Vec<(String, Target)> {
        self.windows
            .iter()
            .flat_map(collect_terminal_targets)
            .collect()
    }

    /// Index of the window containing `terminal_id`.
    pub fn window_of(&self, terminal_id: &str) -> Option<usize> {
        self.windows
            .iter()
            .position(|window| collect_terminal_ids(window).contains(&terminal_id.to_string()))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TabDto {
    id: String,
    label: String,
    /// Legacy single-tree field; always written as the ACTIVE window so
    /// old readers show something sensible.
    #[serde(default)]
    pane: Option<PaneNode>,
    #[serde(default)]
    windows: Option<Vec<PaneNode>>,
    #[serde(default)]
    active_window: usize,
}

impl From<TabDto> for Tab {
    fn from(dto: TabDto) -> Tab {
        let windows = match dto.windows {
            Some(windows) if !windows.is_empty() => windows,
            _ => match dto.pane {
                Some(pane) => vec![pane],
                // Neither source: leave empty — `from_session_json`
                // REJECTS empty-window tabs, preserving the old contract
                // that malformed tabs fail to load.
                None => Vec::new(),
            },
        };
        let active_window = dto.active_window.min(windows.len().saturating_sub(1));
        Tab {
            id: dto.id,
            label: dto.label,
            windows,
            active_window,
        }
    }
}

impl From<Tab> for TabDto {
    fn from(tab: Tab) -> TabDto {
        TabDto {
            pane: Some(tab.active_pane().clone()),
            active_window: tab.active_window,
            windows: Some(tab.windows),
            id: tab.id,
            label: tab.label,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub active_tab_id: String,
}

impl Layout {
    /// Serialize to the session-file layout schema
    /// (`{"tabs":[{"id","label","pane","windows","activeWindow"}],"activeTabId":..}`).
    pub fn to_session_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("layout serialization cannot fail")
    }

    /// Parse a session-file layout value; `None` when it doesn't match the
    /// schema (including any split without exactly two children).
    pub fn from_session_json(value: &serde_json::Value) -> Option<Layout> {
        let layout: Layout = Layout::deserialize(value).ok()?;
        let valid = layout
            .tabs
            .iter()
            .all(|tab| !tab.windows.is_empty() && tab.windows.iter().all(PaneNode::is_valid));
        if valid {
            Some(layout)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::ProfileId;
    use serde_json::json;

    #[test]
    fn a_local_terminal_serialises_exactly_as_before() {
        // Existing saved layouts must round-trip byte-identically.
        let node = PaneNode::terminal("t1");
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(json, r#"{"type":"terminal","terminalId":"t1"}"#);
    }

    #[test]
    fn a_layout_without_a_target_loads_as_local() {
        let node: PaneNode =
            serde_json::from_str(r#"{"type":"terminal","terminalId":"t1"}"#).unwrap();
        match node {
            PaneNode::Terminal { target, .. } => assert!(target.is_local()),
            _ => panic!("expected a terminal"),
        }
    }

    #[test]
    fn a_remote_target_round_trips() {
        let node = PaneNode::Terminal {
            terminal_id: "t1".to_string(),
            target: Target::Remote(ProfileId("abc".to_string())),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(json.contains(r#""target""#), "{json}");
        let back: PaneNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, node);
    }

    fn term(id: &str) -> PaneNode {
        PaneNode::terminal(id)
    }

    fn split(direction: SplitDirection, left: PaneNode, right: PaneNode) -> PaneNode {
        PaneNode::Split {
            direction,
            children: vec![left, right],
            sizes: None,
        }
    }

    fn split_sized(
        direction: SplitDirection,
        left: PaneNode,
        right: PaneNode,
        sizes: [f32; 2],
    ) -> PaneNode {
        PaneNode::Split {
            direction,
            children: vec![left, right],
            sizes: Some(sizes),
        }
    }

    // insert_split

    #[test]
    fn insert_split_replaces_target_terminal_with_split() {
        let pane = term("a");
        let result = insert_split(&pane, "a", SplitDirection::Horizontal, "b");
        assert_eq!(
            result,
            split(SplitDirection::Horizontal, term("a"), term("b"))
        );
    }

    #[test]
    fn insert_split_targets_the_right_node_in_a_nested_tree() {
        let pane = split(
            SplitDirection::Horizontal,
            term("a"),
            split(SplitDirection::Vertical, term("b"), term("c")),
        );
        let result = insert_split(&pane, "b", SplitDirection::Horizontal, "d");
        assert_eq!(
            result,
            split(
                SplitDirection::Horizontal,
                term("a"),
                split(
                    SplitDirection::Vertical,
                    split(SplitDirection::Horizontal, term("b"), term("d")),
                    term("c"),
                ),
            )
        );
    }

    #[test]
    fn insert_split_preserves_existing_split_sizes_and_direction() {
        let pane = split_sized(SplitDirection::Vertical, term("a"), term("b"), [0.25, 0.75]);
        let result = insert_split(&pane, "b", SplitDirection::Horizontal, "c");
        assert_eq!(
            result,
            split_sized(
                SplitDirection::Vertical,
                term("a"),
                split(SplitDirection::Horizontal, term("b"), term("c")),
                [0.25, 0.75],
            )
        );
    }

    #[test]
    fn insert_split_with_unknown_target_leaves_tree_unchanged() {
        let pane = split(SplitDirection::Horizontal, term("a"), term("b"));
        let result = insert_split(&pane, "nope", SplitDirection::Vertical, "c");
        assert_eq!(result, pane);
    }

    // remove_terminal

    #[test]
    fn remove_collapses_single_child_split_to_the_sibling() {
        let pane = split(SplitDirection::Horizontal, term("a"), term("b"));
        assert_eq!(remove_terminal(&pane, "b"), Some(term("a")));
        assert_eq!(remove_terminal(&pane, "a"), Some(term("b")));
    }

    #[test]
    fn remove_collapses_nested_split_and_keeps_outer_split_intact() {
        let pane = split_sized(
            SplitDirection::Horizontal,
            split(SplitDirection::Vertical, term("a"), term("b")),
            term("c"),
            [0.5, 0.5],
        );
        let result = remove_terminal(&pane, "a");
        assert_eq!(
            result,
            Some(split_sized(
                SplitDirection::Horizontal,
                term("b"),
                term("c"),
                [0.5, 0.5],
            ))
        );
    }

    #[test]
    fn remove_last_terminal_returns_none() {
        assert_eq!(remove_terminal(&term("a"), "a"), None);
    }

    #[test]
    fn remove_unknown_terminal_leaves_tree_unchanged() {
        let pane = split(SplitDirection::Vertical, term("a"), term("b"));
        assert_eq!(remove_terminal(&pane, "nope"), Some(pane.clone()));
    }

    // swap_terminals

    #[test]
    fn swap_exchanges_two_terminals_anywhere_in_the_tree() {
        let pane = split(
            SplitDirection::Horizontal,
            term("a"),
            split(SplitDirection::Vertical, term("b"), term("c")),
        );
        let result = swap_terminals(&pane, "a", "c");
        assert_eq!(
            result,
            split(
                SplitDirection::Horizontal,
                term("c"),
                split(SplitDirection::Vertical, term("b"), term("a")),
            )
        );
    }

    #[test]
    fn swap_preserves_split_directions_and_sizes() {
        let pane = split_sized(SplitDirection::Vertical, term("a"), term("b"), [0.25, 0.75]);
        let result = swap_terminals(&pane, "a", "b");
        assert_eq!(
            result,
            split_sized(SplitDirection::Vertical, term("b"), term("a"), [0.25, 0.75])
        );
    }

    #[test]
    fn swap_with_uninvolved_ids_leaves_tree_unchanged() {
        let pane = split(SplitDirection::Horizontal, term("a"), term("b"));
        assert_eq!(swap_terminals(&pane, "x", "y"), pane);
    }

    #[test]
    fn swapping_terminals_carries_each_target_with_its_terminal() {
        // A remote pane that silently became Local on a swap would inherit
        // local authority — the exact failure this slice exists to prevent.
        let remote = Target::Remote(ProfileId("p1".into()));
        let tree = PaneNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                PaneNode::Terminal {
                    terminal_id: "a".into(),
                    target: Target::Local,
                },
                PaneNode::Terminal {
                    terminal_id: "b".into(),
                    target: remote.clone(),
                },
            ],
            sizes: None,
        };
        let swapped = swap_terminals(&tree, "a", "b");
        let PaneNode::Split { children, .. } = &swapped else {
            panic!("expected a split")
        };
        match (&children[0], &children[1]) {
            (
                PaneNode::Terminal {
                    terminal_id: first,
                    target: first_target,
                },
                PaneNode::Terminal {
                    terminal_id: second,
                    target: second_target,
                },
            ) => {
                assert_eq!(first, "b");
                assert_eq!(
                    first_target, &remote,
                    "the remote target did not travel with 'b'"
                );
                assert_eq!(second, "a");
                assert_eq!(second_target, &Target::Local);
            }
            other => panic!("expected two terminals, got {other:?}"),
        }
    }

    // collect_terminal_ids

    #[test]
    fn collect_returns_ids_depth_first_left_to_right() {
        let pane = split(
            SplitDirection::Horizontal,
            split(SplitDirection::Vertical, term("a"), term("b")),
            split(SplitDirection::Vertical, term("c"), term("d")),
        );
        assert_eq!(collect_terminal_ids(&pane), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn collect_single_terminal() {
        assert_eq!(collect_terminal_ids(&term("only")), vec!["only"]);
    }

    // session JSON

    #[test]
    fn session_json_round_trip_matches_schema_literal() {
        let value = json!({
            "tabs": [
                {
                    "id": "tab-1",
                    "label": "Terminal",
                    "pane": {
                        "type": "split",
                        "direction": "horizontal",
                        "children": [
                            { "type": "terminal", "terminalId": "t1" },
                            {
                                "type": "split",
                                "direction": "vertical",
                                "children": [
                                    { "type": "terminal", "terminalId": "t2" },
                                    { "type": "terminal", "terminalId": "t3" }
                                ],
                                "sizes": [0.25, 0.75]
                            }
                        ]
                    }
                },
                {
                    "id": "tab-2",
                    "label": "Build",
                    "pane": { "type": "terminal", "terminalId": "t4" }
                }
            ],
            "activeTabId": "tab-1"
        });

        let layout = Layout::from_session_json(&value).expect("valid session layout");
        assert_eq!(layout.active_tab_id, "tab-1");
        assert_eq!(layout.tabs.len(), 2);
        assert_eq!(*layout.tabs[1].active_pane(), term("t4"));
        assert_eq!(
            collect_terminal_ids(layout.tabs[0].active_pane()),
            vec!["t1", "t2", "t3"]
        );

        // The new schema is a superset: re-serializing keeps the legacy
        // `pane` field (as the active window) and round-trips losslessly.
        let reserialized = layout.to_session_json();
        assert_eq!(
            reserialized["tabs"][1]["pane"], value["tabs"][1]["pane"],
            "legacy pane field must mirror the active window"
        );
        assert_eq!(Layout::from_session_json(&reserialized), Some(layout));
    }

    #[test]
    fn multi_window_tabs_round_trip_and_degrade_gracefully() {
        let mut tab = Tab::single("tab-1", "proj", term("t1"));
        tab.windows.push(term("t2"));
        tab.active_window = 1;
        let layout = Layout {
            tabs: vec![tab],
            active_tab_id: "tab-1".into(),
        };
        let json = layout.to_session_json();
        // Old readers see the ACTIVE window in `pane`.
        assert_eq!(json["tabs"][0]["pane"]["terminalId"], "t2");
        let back = Layout::from_session_json(&json).expect("round trip");
        assert_eq!(back, layout);

        // An out-of-range activeWindow clamps instead of panicking.
        let mut clamped = json.clone();
        clamped["tabs"][0]["activeWindow"] = serde_json::json!(9);
        let loaded = Layout::from_session_json(&clamped).expect("clamped load");
        assert_eq!(loaded.tabs[0].active_window, 1);
    }

    #[test]
    fn serialized_json_uses_exact_field_casing() {
        let layout = Layout {
            tabs: vec![Tab::single(
                "tab-1",
                "Terminal",
                split(SplitDirection::Horizontal, term("t1"), term("t2")),
            )],
            active_tab_id: "tab-1".into(),
        };
        let text = serde_json::to_string(&layout.to_session_json()).unwrap();
        assert!(text.contains("\"type\":\"terminal\""));
        assert!(text.contains("\"terminalId\":\"t1\""));
        assert!(text.contains("\"type\":\"split\""));
        assert!(text.contains("\"direction\":\"horizontal\""));
        assert!(text.contains("\"activeTabId\":\"tab-1\""));
        assert!(text.contains("\"windows\""));
        assert!(text.contains("\"activeWindow\""));
        // sizes: None is omitted entirely, matching the optional schema field.
        assert!(!text.contains("sizes"));
    }

    #[test]
    fn from_session_json_rejects_malformed_layouts() {
        // Split with three children.
        let three_children = json!({
            "tabs": [{
                "id": "tab-1",
                "label": "Terminal",
                "pane": {
                    "type": "split",
                    "direction": "horizontal",
                    "children": [
                        { "type": "terminal", "terminalId": "a" },
                        { "type": "terminal", "terminalId": "b" },
                        { "type": "terminal", "terminalId": "c" }
                    ]
                }
            }],
            "activeTabId": "tab-1"
        });
        assert_eq!(Layout::from_session_json(&three_children), None);

        // Unknown node type.
        let bad_type = json!({
            "tabs": [{
                "id": "tab-1",
                "label": "Terminal",
                "pane": { "type": "pane", "terminalId": "a" }
            }],
            "activeTabId": "tab-1"
        });
        assert_eq!(Layout::from_session_json(&bad_type), None);

        // Invalid direction.
        let bad_direction = json!({
            "tabs": [{
                "id": "tab-1",
                "label": "Terminal",
                "pane": {
                    "type": "split",
                    "direction": "diagonal",
                    "children": [
                        { "type": "terminal", "terminalId": "a" },
                        { "type": "terminal", "terminalId": "b" }
                    ]
                }
            }],
            "activeTabId": "tab-1"
        });
        assert_eq!(Layout::from_session_json(&bad_direction), None);

        // Missing activeTabId.
        let missing_active = json!({ "tabs": [] });
        assert_eq!(Layout::from_session_json(&missing_active), None);
    }
}
