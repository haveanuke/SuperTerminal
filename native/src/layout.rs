//! Tab/pane-tree layout logic, ported from
//! `src/renderer/stores/terminal-store.ts`.
//!
//! Pure data structures — no UI dependencies. The JSON representation matches
//! the session-file schema enforced by `src-tauri/src/session.rs::validate_pane`
//! exactly: `{"type":"terminal","terminalId":...}` and
//! `{"type":"split","direction":"horizontal"|"vertical","children":[..,..],"sizes":[..,..]?}`,
//! with tabs as `{"id","label","pane"}` under `{"tabs":[..],"activeTabId":..}`.

use serde::{Deserialize, Serialize};

/// Numeric terminal handle, monotonically assigned by the caller.
///
/// Note: the pane tree itself keeps terminal ids as `String` (uuid-style) for
/// session-file compatibility; callers that use numeric handles map them to
/// strings at the boundary.
pub type TerminalId = u64;

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
        PaneNode::Terminal { terminal_id } if terminal_id == target => PaneNode::Split {
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
        PaneNode::Terminal { terminal_id: id } => {
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

/// Swap the positions of terminals `a` and `b` in the tree.
///
/// If only one of the two ids is present, that node is relabeled with the
/// other id (matching the TS store's behavior).
pub fn swap_terminals(pane: &PaneNode, a: &str, b: &str) -> PaneNode {
    match pane {
        PaneNode::Terminal { terminal_id } => {
            if terminal_id == a {
                PaneNode::terminal(b)
            } else if terminal_id == b {
                PaneNode::terminal(a)
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
                .map(|child| swap_terminals(child, a, b))
                .collect(),
            sizes: *sizes,
        },
    }
}

/// All terminal ids in the tree, depth-first, left to right.
pub fn collect_terminal_ids(pane: &PaneNode) -> Vec<String> {
    match pane {
        PaneNode::Terminal { terminal_id } => vec![terminal_id.clone()],
        PaneNode::Split { children, .. } => {
            children.iter().flat_map(collect_terminal_ids).collect()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub label: String,
    pub pane: PaneNode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub active_tab_id: String,
}

impl Layout {
    /// Serialize to the session-file layout schema
    /// (`{"tabs":[{"id","label","pane"}],"activeTabId":..}`).
    pub fn to_session_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("layout serialization cannot fail")
    }

    /// Parse a session-file layout value; `None` when it doesn't match the
    /// schema (including any split without exactly two children).
    pub fn from_session_json(value: &serde_json::Value) -> Option<Layout> {
        let layout: Layout = Layout::deserialize(value).ok()?;
        if layout.tabs.iter().all(|tab| tab.pane.is_valid()) {
            Some(layout)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(layout.tabs[1].pane, term("t4"));
        assert_eq!(
            collect_terminal_ids(&layout.tabs[0].pane),
            vec!["t1", "t2", "t3"]
        );

        assert_eq!(layout.to_session_json(), value);
    }

    #[test]
    fn serialized_json_uses_exact_field_casing() {
        let layout = Layout {
            tabs: vec![Tab {
                id: "tab-1".into(),
                label: "Terminal".into(),
                pane: split(SplitDirection::Horizontal, term("t1"), term("t2")),
            }],
            active_tab_id: "tab-1".into(),
        };
        let text = serde_json::to_string(&layout.to_session_json()).unwrap();
        assert!(text.contains("\"type\":\"terminal\""));
        assert!(text.contains("\"terminalId\":\"t1\""));
        assert!(text.contains("\"type\":\"split\""));
        assert!(text.contains("\"direction\":\"horizontal\""));
        assert!(text.contains("\"activeTabId\":\"tab-1\""));
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
