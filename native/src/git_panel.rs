//! Source-control side panel: the native UI over `superterminal_core::git`.
//!
//! Every git call shells out and can block, so all engine work runs on the
//! background executor; the entity is updated from the async side. The panel
//! follows the focused terminal's working directory (the workspace pushes
//! cwd changes in via [`GitPanel::set_target_cwd`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, rgb, Context, Entity, EventEmitter, MouseButton, SharedString, Window};

use superterminal_core::git::actions::{ActionKind, ActionResult};
use superterminal_core::git::graph::{CommitFileChange, GraphData};
use superterminal_core::git::status::{StatusEntry, StatusReport};
use superterminal_core::git::{self, GitState, RepoInfo};

use crate::text_field::{TextField, TextFieldEvent};
use crate::themes::Theme;

/// Operations the panel can run; all execute off the UI thread.
#[derive(Clone, Debug)]
enum GitOp {
    Stage(Vec<String>),
    StageAll,
    Unstage(Vec<String>),
    UnstageAll,
    Discard(Vec<String>),
    Commit(String),
    Push { set_upstream: bool },
    Pull,
    Fetch,
}

/// What a discard confirm is armed against. A typed target, not a magic
/// path string — a file literally named `*` must stay a single-file discard.
#[derive(Clone, Debug, PartialEq)]
enum DiscardTarget {
    Path(String),
    All,
}

pub struct GitPanel {
    state: Arc<GitState>,
    theme: &'static Theme,
    repo: Option<RepoInfo>,
    target_path: Option<String>,
    report: Option<StatusReport>,
    graph: Option<GraphData>,
    busy: bool,
    error: Option<String>,
    commit_field: Entity<TextField>,
    /// Monotonic guard: results only apply if the repo generation matches.
    generation: u64,
    /// Monotonic guard for graph loads: an older in-flight request must not
    /// overwrite a newer one landing first.
    graph_seq: u64,
    /// Two-click discard confirm: target armed by the first click. Discarding
    /// an untracked entry deletes it permanently, so nothing runs unarmed.
    pending_discard: Option<DiscardTarget>,
    /// Commits expanded in the history view: hash -> files once loaded
    /// (None while the load is in flight). Any number may be open at once;
    /// a commit's files are immutable per hash, so late results are always
    /// safe to attach while the hash stays expanded.
    expanded: HashMap<String, Option<Vec<CommitFileChange>>>,
}

pub struct PanelClosed;

impl EventEmitter<PanelClosed> for GitPanel {}

impl GitPanel {
    pub fn new(theme: &'static Theme, cx: &mut Context<Self>) -> Self {
        let commit_field = cx.new(|field_cx| TextField::new("commit message", theme, field_cx));
        cx.subscribe(
            &commit_field,
            |panel, _field, event: &TextFieldEvent, cx| match event {
                TextFieldEvent::Submitted(_) => panel.try_commit(cx),
                TextFieldEvent::Cancelled => cx.emit(PanelClosed),
            },
        )
        .detach();

        // Refresh poll while the panel lives (the workspace drops the entity
        // when the panel closes, which ends this loop).
        cx.spawn(async move |panel, cx| loop {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            if panel
                .update(cx, |panel: &mut GitPanel, cx| panel.refresh_status(cx))
                .is_err()
            {
                break;
            }
        })
        .detach();

        Self {
            state: Arc::new(GitState::default()),
            theme,
            repo: None,
            target_path: None,
            report: None,
            graph: None,
            busy: false,
            error: None,
            commit_field,
            generation: 0,
            graph_seq: 0,
            pending_discard: None,
            expanded: HashMap::new(),
        }
    }

    pub fn set_theme(&mut self, theme: &'static Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        self.commit_field
            .update(cx, |field, field_cx| field.set_theme(theme, field_cx));
        cx.notify();
    }

    /// Follow the focused terminal: resolve its cwd to a repo (off-thread).
    pub fn set_target_cwd(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        let Some(cwd) = cwd else { return };
        if self.target_path.as_deref() == Some(cwd.as_str()) {
            return;
        }
        self.target_path = Some(cwd.clone());
        let state = Arc::clone(&self.state);
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |panel, cx| {
            let resolved = cx
                .background_executor()
                .spawn(async move { state.resolve(&cwd) })
                .await;
            let _ = panel.update(cx, |panel: &mut GitPanel, cx| {
                if panel.generation != generation {
                    return;
                }
                let changed = panel.repo.as_ref().map(|r| r.repo_id.clone())
                    != resolved.as_ref().map(|r| r.repo_id.clone());
                panel.repo = resolved;
                if changed {
                    panel.report = None;
                    panel.graph = None;
                    panel.pending_discard = None;
                    panel.expanded.clear();
                    panel.refresh_status(cx);
                    panel.refresh_graph(cx);
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        cx.spawn(async move |panel, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { git::status_guarded(&state, &repo.repo_id) })
                .await;
            let _ = panel.update(cx, |panel: &mut GitPanel, cx| {
                if panel.generation != generation {
                    return;
                }
                match result {
                    Ok(report) => {
                        // A checkout done in the terminal changes the branch
                        // under us; the poll must refresh the log too.
                        let branch_changed = panel.report.as_ref().is_some_and(|prev| {
                            prev.branch != report.branch || prev.detached != report.detached
                        });
                        // Disarm a pending discard unless what it targets is
                        // still exactly what it was — the second click must
                        // never delete contents the user hasn't seen.
                        if let Some(armed) = panel.pending_discard.clone() {
                            let disarm = match &armed {
                                DiscardTarget::All => branch_changed,
                                DiscardTarget::Path(path) => {
                                    let entry_state = |r: &StatusReport| {
                                        r.entries
                                            .iter()
                                            .find(|e| &e.path == path)
                                            .map(|e| (e.kind.clone(), e.worktree_status.clone()))
                                    };
                                    let prev = panel.report.as_ref().and_then(entry_state);
                                    let now = entry_state(&report);
                                    branch_changed || now.is_none() || prev != now
                                }
                            };
                            if disarm {
                                panel.pending_discard = None;
                            }
                        }
                        panel.report = Some(report);
                        if branch_changed {
                            panel.refresh_graph(cx);
                        }
                        cx.notify();
                    }
                    Err(err) if err == "busy" => {}
                    Err(err) => {
                        panel.error = Some(err);
                        cx.notify();
                    }
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn refresh_graph(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        self.graph_seq += 1;
        let seq = self.graph_seq;
        cx.spawn(async move |panel, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    superterminal_core::git::graph::run_graph(&state, &repo.repo_id, 300)
                })
                .await;
            let _ = panel.update(cx, |panel: &mut GitPanel, cx| {
                if panel.generation != generation || panel.graph_seq != seq {
                    return;
                }
                if let Ok(graph) = result {
                    panel.graph = Some(graph);
                    cx.notify();
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn run(&mut self, op: GitOp, cx: &mut Context<Self>) {
        let Some(repo) = self.repo.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        self.pending_discard = None;
        cx.notify();
        let commit_message = match &op {
            GitOp::Commit(message) => Some(message.clone()),
            _ => None,
        };
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        cx.spawn(async move |panel, cx| {
            let result: Result<ActionResult, String> = cx
                .background_executor()
                .spawn(async move {
                    let id = repo.repo_id.as_str();
                    match op {
                        GitOp::Stage(paths) => {
                            git::actions::run_action(&state, id, Some(paths), ActionKind::Stage)
                        }
                        GitOp::StageAll => {
                            git::actions::run_action(&state, id, None, ActionKind::Stage)
                        }
                        GitOp::Unstage(paths) => {
                            git::actions::run_action(&state, id, Some(paths), ActionKind::Unstage)
                        }
                        GitOp::UnstageAll => {
                            git::actions::run_action(&state, id, None, ActionKind::Unstage)
                        }
                        GitOp::Discard(paths) => {
                            git::actions::run_action(&state, id, Some(paths), ActionKind::Discard)
                        }
                        GitOp::Commit(message) => git::actions::run_commit(&state, id, &message),
                        GitOp::Push { set_upstream } => {
                            git::network::run_push(&state, id, set_upstream)
                        }
                        GitOp::Pull => git::network::run_pull(&state, id),
                        GitOp::Fetch => git::network::run_fetch(&state, id),
                    }
                })
                .await;
            let _ = panel.update(cx, |panel: &mut GitPanel, cx| {
                panel.busy = false;
                if panel.generation != generation {
                    return;
                }
                match result {
                    Ok(action) => {
                        panel.report = Some(action.report);
                        if let Some(message) = &commit_message {
                            // Clear only the message that was committed — the
                            // user may already be drafting the next one.
                            panel.commit_field.update(cx, |field, field_cx| {
                                if field.value.trim() == message.as_str() {
                                    field.reset(field_cx);
                                }
                            });
                        }
                        panel.refresh_graph(cx);
                    }
                    Err(err) if err == "no upstream" => {
                        // Publish flow: retry with upstream after the caller
                        // confirms; v-parity keeps it one click deep.
                        panel.error =
                            Some("no upstream - use publish to push a new branch".to_string());
                    }
                    Err(err) => {
                        let capped: String = err.chars().take(200).collect();
                        panel.error = Some(capped);
                    }
                }
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Expand a commit to show its changed files (VS Code's history view);
    /// clicking the expanded commit again collapses it.
    /// The ONE commit gate: a message, staged changes, and not busy. Both
    /// the button and Enter in the field go through here, so the disabled
    /// look never lies.
    fn can_commit(&self, cx: &Context<Self>) -> bool {
        !self.busy
            && !self.commit_field.read(cx).value.trim().is_empty()
            && self.report.as_ref().is_some_and(|r| {
                r.entries
                    .iter()
                    .any(|e| e.kind != "untracked" && e.kind != "unmerged" && e.index_status != ".")
            })
    }

    fn try_commit(&mut self, cx: &mut Context<Self>) {
        if !self.can_commit(cx) {
            return;
        }
        // The field keeps the message; it only clears once the commit lands
        // (see the success path in `run`).
        let message = self.commit_field.read(cx).value.trim().to_string();
        self.run(GitOp::Commit(message), cx);
    }

    fn toggle_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        if self.expanded.remove(&hash).is_some() {
            cx.notify();
            return;
        }
        self.expanded.insert(hash.clone(), None);
        let state = Arc::clone(&self.state);
        let Some(repo) = self.repo.clone() else {
            return;
        };
        cx.notify();
        cx.spawn(async move |panel, cx| {
            let request_hash = hash.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    superterminal_core::git::graph::run_commit_files(
                        &state,
                        &repo.repo_id,
                        &request_hash,
                    )
                })
                .await;
            let _ = panel.update(cx, |panel: &mut GitPanel, cx| {
                // Attach only while the hash is still expanded. A repo change
                // clears the map; per-hash file lists are immutable, so any
                // surviving slot accepts the result safely.
                if let (Ok(files), Some(slot)) = (result, panel.expanded.get_mut(&hash)) {
                    *slot = Some(files);
                    cx.notify();
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Bordered chip for header actions (fetch/pull/push/publish, bulk ops).
    fn chip(&self, label: &'static str, op: GitOp, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let disabled = self.busy;
        div()
            .id(SharedString::from(format!("git-chip-{label}")))
            .cursor_pointer()
            .px(px(6.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(theme.ui_border))
            .bg(rgb(theme.ui_surface))
            .text_size(px(10.0))
            .text_color(rgb(theme.ui_text))
            .opacity(if disabled { 0.4 } else { 1.0 })
            .hover(|style| style.border_color(rgb(theme.ui_accent)))
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |panel, _, _, cx| {
                    if !panel.busy {
                        panel.run(op.clone(), cx);
                    }
                }),
            )
    }

    /// Small square row action ("+" stage, "-" unstage), VS Code style.
    fn glyph_button(
        &self,
        id: String,
        glyph: &'static str,
        op: GitOp,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(SharedString::from(id))
            .cursor_pointer()
            .w(px(16.0))
            .h(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .text_color(rgb(theme.ui_text_muted))
            .hover(|style| {
                style
                    .bg(rgb(theme.ui_border))
                    .text_color(rgb(theme.ui_text))
            })
            .child(SharedString::from(glyph))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |panel, _, _, cx| {
                    if !panel.busy {
                        panel.run(op.clone(), cx);
                    }
                }),
            )
    }

    /// Discard with a two-click confirm; `path` may be the `"*"` sentinel
    /// for "discard all changes" (resolved to explicit paths on confirm).
    fn discard_control(
        &self,
        target: DiscardTarget,
        untracked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let armed = self.pending_discard.as_ref() == Some(&target);
        let id: SharedString = match &target {
            DiscardTarget::All => "git-discard-all".into(),
            DiscardTarget::Path(path) => format!("git-discard-{path}").into(),
        };
        let label = match (&target, armed, untracked) {
            (DiscardTarget::All, false, _) => "discard all",
            (DiscardTarget::All, true, _) => "discard all?",
            (DiscardTarget::Path(_), false, _) => "\u{21a9}",
            (DiscardTarget::Path(_), true, true) => "delete?",
            (DiscardTarget::Path(_), true, false) => "sure?",
        };
        let is_all = matches!(target, DiscardTarget::All);
        div()
            .id(id)
            .cursor_pointer()
            .h(px(16.0))
            .px(px(if armed || is_all { 4.0 } else { 0.0 }))
            .min_w(px(16.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .when(is_all, |d| {
                d.border_1()
                    .border_color(rgb(theme.ui_border))
                    .text_size(px(10.0))
            })
            .text_color(rgb(if armed {
                theme.red
            } else {
                theme.ui_text_muted
            }))
            .hover(|style| style.bg(rgb(theme.ui_border)).text_color(rgb(theme.red)))
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |panel, _, _, cx| {
                    if panel.busy {
                        return;
                    }
                    if panel.pending_discard.as_ref() == Some(&target) {
                        let paths = match &target {
                            DiscardTarget::All => panel
                                .report
                                .iter()
                                .flat_map(|r| r.entries.iter())
                                .filter(|e| {
                                    e.actionable
                                        && e.kind != "unmerged"
                                        && (e.kind == "untracked" || e.worktree_status != ".")
                                })
                                .map(|e| e.path.clone())
                                .collect(),
                            DiscardTarget::Path(path) => vec![path.clone()],
                        };
                        if !paths.is_empty() {
                            panel.run(GitOp::Discard(paths), cx);
                        }
                    } else {
                        panel.pending_discard = Some(target.clone());
                        cx.notify();
                    }
                }),
            )
    }

    fn status_color(&self, letter: &str, conflict: bool) -> u32 {
        let theme = self.theme;
        if conflict {
            return theme.red;
        }
        match letter {
            "M" | "T" => theme.yellow,
            "A" | "U" | "?" => theme.green,
            "D" => theme.red,
            "R" | "C" => theme.blue,
            _ => theme.ui_text_muted,
        }
    }

    /// A file row, VS Code style: colored basename, muted directory, then
    /// actions and the status letter on the right.
    fn entry_row(
        &self,
        entry: &StatusEntry,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let conflict = entry.kind == "unmerged";
        let letter = if staged {
            entry.index_status.clone()
        } else if entry.kind == "untracked" {
            "U".to_string()
        } else {
            entry.worktree_status.clone()
        };
        let color = self.status_color(&letter, conflict);
        let display = match &entry.orig_path {
            Some(orig) => format!("{orig} -> {}", entry.path),
            None => entry.path.clone(),
        };
        // basename + parent dir, unless it's a rename arrow display.
        let (name, dir) = if display.contains(" -> ") {
            (display.clone(), String::new())
        } else {
            match display.rsplit_once('/') {
                Some((dir, name)) => (name.to_string(), dir.to_string()),
                None => (display.clone(), String::new()),
            }
        };
        let path = entry.path.clone();
        let actionable = entry.actionable;
        div()
            .id(SharedString::from(format!("git-entry-{}", entry.path)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .h(px(22.0))
            .px(px(8.0))
            .opacity(if actionable { 1.0 } else { 0.6 })
            .hover(|style| style.bg(rgb(theme.ui_surface)))
            .child(
                div()
                    .flex_none()
                    .max_w(px(150.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(rgb(color))
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_grow()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(SharedString::from(dir)),
            )
            .children(actionable.then(|| {
                if conflict {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(self.chip("resolve", GitOp::Stage(vec![path.clone()]), cx))
                        .into_any_element()
                } else if staged {
                    self.glyph_button(
                        format!("git-unstage-{path}"),
                        "-",
                        GitOp::Unstage(vec![path.clone()]),
                        cx,
                    )
                    .into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.0))
                        .child(self.discard_control(
                            DiscardTarget::Path(path.clone()),
                            entry.kind == "untracked",
                            cx,
                        ))
                        .child(self.glyph_button(
                            format!("git-stage-{path}"),
                            "+",
                            GitOp::Stage(vec![path]),
                            cx,
                        ))
                        .into_any_element()
                }
            }))
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_size(px(10.0))
                    .text_color(rgb(color))
                    .child(SharedString::from(letter)),
            )
    }

    fn section(
        &self,
        title: &'static str,
        entries: Vec<StatusEntry>,
        staged: bool,
        header_actions: Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if entries.is_empty() {
            return None;
        }
        let theme = self.theme;
        let count = entries.len();
        Some(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(8.0))
                        .pt(px(8.0))
                        .pb(px(2.0))
                        .text_size(px(9.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .child(SharedString::from(format!(
                            "{} ({count})",
                            title.to_uppercase()
                        )))
                        .child(div().flex_grow())
                        .children(header_actions),
                )
                .children(
                    entries
                        .into_iter()
                        .map(|entry| self.entry_row(&entry, staged, cx)),
                ),
        )
    }

    /// Rows of files for an expanded commit, indented under it.
    fn commit_file_rows(&self, files: &Option<Vec<CommitFileChange>>) -> impl IntoElement {
        let theme = self.theme;
        let rows: Vec<_> = match files {
            None => vec![div()
                .pl(px(6.0))
                .h(px(18.0))
                .text_size(px(10.0))
                .text_color(rgb(theme.ui_text_muted))
                .child("loading...")
                .into_any_element()],
            Some(files) if files.is_empty() => vec![div()
                .pl(px(6.0))
                .h(px(18.0))
                .text_size(px(10.0))
                .text_color(rgb(theme.ui_text_muted))
                .child("no files")
                .into_any_element()],
            Some(files) => files
                .iter()
                .take(80)
                .map(|file| {
                    let color = self.status_color(&file.status, false);
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .pl(px(6.0))
                        .pr(px(8.0))
                        .h(px(18.0))
                        .child(
                            div()
                                .flex_none()
                                .w(px(10.0))
                                .text_size(px(10.0))
                                .text_color(rgb(color))
                                .child(SharedString::from(file.status.clone())),
                        )
                        .child(
                            div()
                                .flex_grow()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(10.0))
                                .text_color(rgb(theme.ui_text))
                                .child(SharedString::from(file.path.clone())),
                        )
                        .into_any_element()
                })
                .collect(),
        };
        // Indented past the graph gutter so the painted branch lines stay
        // visible while the block is open; row heights must stay 18px to
        // match the canvas offset math in `graph_body`.
        div()
            .flex()
            .flex_col()
            .ml(px(78.0))
            .bg(rgb(theme.ui_surface))
            .children(rows)
    }

    /// Commit rows with branch lines painted behind them on ONE canvas:
    /// each row's y is offset by the accumulated heights of every expanded
    /// file block above it, and edges run straight through those blocks'
    /// gutters, so the topology never breaks around expansions.
    fn graph_body(
        &self,
        rows: &[superterminal_core::git::graph::GraphRow],
        offsets: Vec<f32>,
        mut blocks: Vec<Option<gpui::AnyElement>>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        const ROW_H: f32 = 18.0;
        const MAX_LANE: usize = 7;
        let theme = self.theme;
        let lane_colors = [
            theme.ui_accent,
            theme.green,
            theme.yellow,
            theme.magenta,
            theme.cyan,
            theme.blue,
            theme.red,
        ];
        // One capping policy everywhere: dots, edges and colors all use the
        // capped lane, so crowded graphs stay consistent if squeezed.
        let lane_color = move |lane: usize| lane_colors[lane.min(MAX_LANE) % lane_colors.len()];
        let lane_x = |lane: usize| 8.0 + (lane.min(MAX_LANE) as f32) * 8.0;
        let gutter = px(8.0 + 8.0 * 8.0 + 6.0);
        // Rows sit lower by the total height of file blocks spliced above.
        let paint_offsets = offsets.clone();
        let y_of = move |index: usize| {
            (index as f32) * ROW_H + paint_offsets.get(index).copied().unwrap_or(0.0)
        };

        // Geometry for the paint pass: (lane, edges) per row.
        type PaintRow = (usize, Vec<(usize, usize)>);
        let paint_rows: Vec<PaintRow> = rows
            .iter()
            .map(|row| {
                (
                    row.lane,
                    row.edges
                        .iter()
                        .map(|edge| (edge.from_lane, edge.to_lane))
                        .collect(),
                )
            })
            .collect();

        let lines = gpui::canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let ox = f32::from(bounds.origin.x);
                let oy = f32::from(bounds.origin.y);
                let last = paint_rows.len().saturating_sub(1);
                for (index, (_, edges)) in paint_rows.iter().enumerate() {
                    if index >= last {
                        break; // edges connect to the NEXT row
                    }
                    let y_from = oy + y_of(index) + ROW_H / 2.0;
                    let y_to = oy + y_of(index + 1) + ROW_H / 2.0;
                    for (from, to) in edges {
                        let mut builder = gpui::PathBuilder::stroke(px(1.5));
                        builder.move_to(gpui::point(px(ox + lane_x(*from)), px(y_from)));
                        builder.line_to(gpui::point(px(ox + lane_x(*to)), px(y_to)));
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, rgb(lane_color(*from)));
                        }
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let row_divs: Vec<_> = rows
            .iter()
            .map(|row| {
                let color = lane_color(row.lane);
                let hash = row.hash.clone();
                let selected = self.expanded.contains_key(&row.hash);
                div()
                    .id(SharedString::from(format!("commit-{}", row.hash)))
                    .relative()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(ROW_H))
                    .pl(gutter)
                    .pr(px(8.0))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgb(theme.ui_surface)))
                    .hover(|style| style.bg(rgb(theme.ui_surface)))
                    .child(
                        div()
                            .absolute()
                            .left(px(lane_x(row.lane) - 3.5))
                            .top(px(ROW_H / 2.0 - 3.5))
                            .w(px(7.0))
                            .h(px(7.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(theme.ui_background))
                            .bg(rgb(color)),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(10.0))
                            .text_color(rgb(if selected {
                                theme.ui_text
                            } else {
                                theme.ui_text_muted
                            }))
                            .child(SharedString::from(row.subject.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(
                                row.author.split(' ').next().unwrap_or("").to_string(),
                            )),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |panel, _, _, cx| {
                            panel.toggle_commit(hash.clone(), cx);
                        }),
                    )
            })
            .collect();

        let mut children: Vec<gpui::AnyElement> = vec![lines.into_any_element()];
        for (index, row_div) in row_divs.into_iter().enumerate() {
            children.push(row_div.into_any_element());
            if let Some(block) = blocks.get_mut(index).and_then(Option::take) {
                children.push(block);
            }
        }
        div()
            .relative()
            .flex()
            .flex_col()
            .children(children)
            .into_any_element()
    }

    /// The commit history with painted branch lines; every expanded commit
    /// gets its file list spliced in beneath it.
    fn render_graph(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let rows: Vec<superterminal_core::git::graph::GraphRow> = self
            .graph
            .iter()
            .flat_map(|graph| graph.rows.iter())
            .take(300)
            .cloned()
            .collect();

        // Per-row y offset = total heights of the file blocks spliced above;
        // block heights are 18px rows (loading and no-files both one row),
        // mirrored exactly by `commit_file_rows`.
        let mut offsets = Vec::with_capacity(rows.len());
        let mut blocks: Vec<Option<gpui::AnyElement>> = Vec::with_capacity(rows.len());
        let mut accumulated = 0.0f32;
        for row in &rows {
            offsets.push(accumulated);
            match self.expanded.get(&row.hash) {
                Some(files) => {
                    let count = match files {
                        None => 1,
                        Some(list) if list.is_empty() => 1,
                        Some(list) => list.len().min(80),
                    };
                    accumulated += 18.0 * count as f32;
                    let files = files.clone();
                    blocks.push(Some(self.commit_file_rows(&files).into_any_element()));
                }
                None => blocks.push(None),
            }
        }

        let mut children: Vec<gpui::AnyElement> = Vec::new();
        if !rows.is_empty() {
            children.push(self.graph_body(&rows, offsets, blocks, cx));
        }

        div()
            .border_t_1()
            .border_color(rgb(theme.ui_border))
            .flex()
            .flex_col()
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(6.0))
                    .pb(px(2.0))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child("COMMITS"),
            )
            .children(children)
    }
}

impl Render for GitPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let base = div()
            .w(px(320.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .border_l_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .text_color(rgb(theme.ui_text));

        let Some(repo) = self.repo.clone() else {
            return base.child(
                div()
                    .p(px(12.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(match &self.target_path {
                        Some(path) => SharedString::from(format!("not a git repository: {path}")),
                        None => "focus a terminal inside a repository".into(),
                    }),
            );
        };

        let report = self.report.clone();
        let branch_label: SharedString = match &report {
            Some(r) => r
                .branch
                .clone()
                .or_else(|| r.detached.as_ref().map(|d| format!("detached @ {d}")))
                .unwrap_or_else(|| "no commits yet".to_string())
                .into(),
            None => "loading...".into(),
        };
        let (ahead, behind) = report
            .as_ref()
            .map(|r| (r.ahead, r.behind))
            .unwrap_or((0, 0));

        let entries = report.map(|r| r.entries).unwrap_or_default();
        let staged: Vec<StatusEntry> = entries
            .iter()
            .filter(|e| e.kind != "untracked" && e.kind != "unmerged" && e.index_status != ".")
            .cloned()
            .collect();
        let changes: Vec<StatusEntry> = entries
            .iter()
            .filter(|e| e.kind == "untracked" || (e.kind != "unmerged" && e.worktree_status != "."))
            .cloned()
            .collect();
        let conflicts: Vec<StatusEntry> = entries
            .iter()
            .filter(|e| e.kind == "unmerged")
            .cloned()
            .collect();

        let commit_ready = self.can_commit(cx);

        base
            // header: repo, branch, sync counters, network actions
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(rgb(theme.ui_border))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .text_color(rgb(theme.ui_accent))
                                    .child(SharedString::from(repo.display_name.clone())),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.ui_text_muted))
                                    .child(branch_label),
                            )
                            .child(div().flex_grow())
                            .children((ahead > 0 || behind > 0).then(|| {
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.ui_text_muted))
                                    .child(SharedString::from(format!(
                                        "\u{2193}{behind} \u{2191}{ahead}"
                                    )))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(self.chip("fetch", GitOp::Fetch, cx))
                            .child(self.chip("pull", GitOp::Pull, cx))
                            .child(self.chip(
                                "push",
                                GitOp::Push {
                                    set_upstream: false,
                                },
                                cx,
                            ))
                            .child(self.chip("publish", GitOp::Push { set_upstream: true }, cx)),
                    ),
            )
            // commit box: message field + full-width commit button
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .child(self.commit_field.clone())
                    .child(
                        div()
                            .id("git-commit-btn")
                            .cursor_pointer()
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.0))
                            .bg(rgb(theme.ui_accent))
                            .text_color(rgb(theme.ui_background))
                            .opacity(if commit_ready { 1.0 } else { 0.4 })
                            .hover(|style| style.opacity(0.85))
                            .child("Commit")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|panel, _, _, cx| panel.try_commit(cx)),
                            ),
                    ),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .px(px(8.0))
                    .py(px(2.0))
                    .text_size(px(10.0))
                    .text_color(rgb(theme.red))
                    .child(SharedString::from(error))
            }))
            .child(
                // Everything below the commit box scrolls as one column,
                // VS Code style: sections first, then the commit history.
                div()
                    .id("git-scroll")
                    .flex_grow()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(self.section("merge changes", conflicts, false, Vec::new(), cx))
                    .children({
                        let actions = vec![self
                            .chip("unstage all", GitOp::UnstageAll, cx)
                            .into_any_element()];
                        self.section("staged changes", staged, true, actions, cx)
                    })
                    .children({
                        let actions = vec![
                            self.discard_control(DiscardTarget::All, false, cx)
                                .into_any_element(),
                            self.chip("stage all", GitOp::StageAll, cx)
                                .into_any_element(),
                        ];
                        self.section("changes", changes, false, actions, cx)
                    })
                    .child(self.render_graph(cx)),
            )
    }
}
