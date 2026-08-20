//! Source-control side panel: the native UI over `superterminal_core::git`.
//!
//! Every git call shells out and can block, so all engine work runs on the
//! background executor; the entity is updated from the async side. The panel
//! follows the focused terminal's working directory (the workspace pushes
//! cwd changes in via [`GitPanel::set_target_cwd`]).

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{div, px, rgb, Context, Entity, EventEmitter, MouseButton, SharedString, Window};

use superterminal_core::git::actions::{ActionKind, ActionResult};
use superterminal_core::git::graph::GraphData;
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
    /// Two-click discard confirm: path armed by the first click. Discarding
    /// an untracked entry deletes it permanently, so nothing runs unarmed.
    pending_discard: Option<String>,
}

pub struct PanelClosed;

impl EventEmitter<PanelClosed> for GitPanel {}

impl GitPanel {
    pub fn new(theme: &'static Theme, cx: &mut Context<Self>) -> Self {
        let commit_field = cx.new(|field_cx| TextField::new("commit message", theme, field_cx));
        cx.subscribe(
            &commit_field,
            |panel, _field, event: &TextFieldEvent, cx| match event {
                TextFieldEvent::Submitted(message) => {
                    let message = message.trim().to_string();
                    if !message.is_empty() {
                        // The field keeps the message; it only clears once the
                        // commit lands (see the success path in `run`).
                        panel.run(GitOp::Commit(message), cx);
                    }
                }
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
        }
    }

    pub fn set_theme(&mut self, theme: &'static Theme, cx: &mut Context<Self>) {
        self.theme = theme;
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
                        // Disarm a pending discard unless the armed entry is
                        // still exactly what it was — the second click must
                        // never delete contents the user hasn't seen.
                        if let Some(armed) = panel.pending_discard.clone() {
                            let entry_state = |r: &StatusReport| {
                                r.entries
                                    .iter()
                                    .find(|e| e.path == armed)
                                    .map(|e| (e.kind.clone(), e.worktree_status.clone()))
                            };
                            let prev = panel.report.as_ref().and_then(&entry_state);
                            let now = entry_state(&report);
                            if branch_changed || now.is_none() || prev != now {
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

    fn button(&self, label: &'static str, op: GitOp, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let disabled = self.busy;
        div()
            .id(SharedString::from(format!("git-{label}")))
            .cursor_pointer()
            .px(px(5.0))
            .rounded(px(3.0))
            .text_color(rgb(theme.ui_text_muted))
            .opacity(if disabled { 0.4 } else { 1.0 })
            .hover(|style| style.bg(rgb(theme.ui_border)))
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

    /// Discard is destructive (untracked entries are deleted outright, via
    /// `git clean`), so it arms on the first click and only runs on the
    /// second. Any other panel action disarms it.
    fn discard_button(
        &self,
        path: String,
        untracked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let armed = self.pending_discard.as_deref() == Some(path.as_str());
        let label = if !armed {
            "discard"
        } else if untracked {
            "delete file?"
        } else {
            "sure?"
        };
        div()
            .id(SharedString::from(format!("git-discard-{path}")))
            .cursor_pointer()
            .px(px(5.0))
            .rounded(px(3.0))
            .text_color(rgb(if armed {
                theme.red
            } else {
                theme.ui_text_muted
            }))
            .opacity(if self.busy { 0.4 } else { 1.0 })
            .hover(|style| style.bg(rgb(theme.ui_border)))
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |panel, _, _, cx| {
                    if panel.busy {
                        return;
                    }
                    if panel.pending_discard.as_deref() == Some(path.as_str()) {
                        panel.run(GitOp::Discard(vec![path.clone()]), cx);
                    } else {
                        panel.pending_discard = Some(path.clone());
                        cx.notify();
                    }
                }),
            )
    }

    fn entry_row(
        &self,
        entry: &StatusEntry,
        staged: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let letter = if staged {
            entry.index_status.clone()
        } else if entry.kind == "untracked" {
            "U".to_string()
        } else {
            entry.worktree_status.clone()
        };
        let color = match letter.as_str() {
            "M" => theme.yellow,
            "A" => theme.green,
            "D" => theme.red,
            "U" => theme.magenta,
            "R" | "C" => theme.blue,
            _ => theme.ui_text_muted,
        };
        let display = match &entry.orig_path {
            Some(orig) => format!("{orig} -> {}", entry.path),
            None => entry.path.clone(),
        };
        let path = entry.path.clone();
        let actionable = entry.actionable;
        let conflict = entry.kind == "unmerged";
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .py(px(1.0))
            .rounded(px(3.0))
            .opacity(if actionable { 1.0 } else { 0.5 })
            .hover(|style| style.bg(rgb(theme.ui_surface)))
            .child(
                div()
                    .w(px(12.0))
                    .flex_none()
                    .text_color(rgb(color))
                    .child(SharedString::from(letter)),
            )
            .child(
                div()
                    .flex_grow()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(display)),
            )
            .children(actionable.then(|| {
                if conflict {
                    self.button("resolve", GitOp::Stage(vec![path.clone()]), cx)
                        .into_any_element()
                } else if staged {
                    self.button("unstage", GitOp::Unstage(vec![path.clone()]), cx)
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.0))
                        .child(self.discard_button(path.clone(), entry.kind == "untracked", cx))
                        .child(self.button("stage", GitOp::Stage(vec![path]), cx))
                        .into_any_element()
                }
            }))
    }

    fn section(
        &self,
        title: &'static str,
        entries: Vec<StatusEntry>,
        staged: bool,
        bulk: Option<(&'static str, GitOp)>,
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
                .gap(px(1.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .px(px(6.0))
                        .pt(px(6.0))
                        .text_size(px(10.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .child(SharedString::from(format!("{title} ({count})")))
                        .child(div().flex_grow())
                        .children(bulk.map(|(label, op)| self.button(label, op, cx))),
                )
                .children(
                    entries
                        .into_iter()
                        .map(|entry| self.entry_row(&entry, staged, cx)),
                ),
        )
    }

    fn render_graph(&self) -> impl IntoElement {
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
        let rows: Vec<_> = self
            .graph
            .iter()
            .flat_map(|graph| graph.rows.iter())
            .take(120)
            .map(|row| {
                let color = lane_colors[row.lane % lane_colors.len()];
                let indent = px(4.0 + row.lane as f32 * 8.0);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(6.0))
                    .h(px(18.0))
                    .child(
                        div()
                            .ml(indent)
                            .w(px(7.0))
                            .h(px(7.0))
                            .flex_none()
                            .rounded(px(4.0))
                            .bg(rgb(color)),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(10.0))
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
            })
            .collect();
        div()
            .flex_grow()
            .overflow_hidden()
            .border_t_1()
            .border_color(rgb(theme.ui_border))
            .flex()
            .flex_col()
            .children(rows)
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

        base
            // header
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(rgb(theme.ui_border))
                    .child(
                        div()
                            .text_color(rgb(theme.ui_accent))
                            .child(SharedString::from(repo.display_name.clone())),
                    )
                    .child(
                        div()
                            .text_color(rgb(theme.ui_text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(branch_label),
                    )
                    .child(div().flex_grow())
                    .children((ahead > 0 || behind > 0).then(|| {
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(format!("+{ahead} -{behind}")))
                    }))
                    .child(self.button("pull", GitOp::Pull, cx))
                    .child(self.button(
                        "push",
                        GitOp::Push {
                            set_upstream: false,
                        },
                        cx,
                    ))
                    .child(self.button("publish", GitOp::Push { set_upstream: true }, cx))
                    .child(self.button("fetch", GitOp::Fetch, cx)),
            )
            // commit line
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(6.0))
                    .child(div().text_color(rgb(theme.ui_accent)).child("commit >"))
                    .child(div().flex_grow().child(self.commit_field.clone())),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .px(px(8.0))
                    .py(px(2.0))
                    .text_size(px(10.0))
                    .text_color(rgb(theme.red))
                    .child(SharedString::from(error))
            }))
            .children(self.section("merge conflicts", conflicts, false, None, cx))
            .children(self.section(
                "staged",
                staged,
                true,
                Some(("unstage all", GitOp::UnstageAll)),
                cx,
            ))
            .children(self.section(
                "changes",
                changes,
                false,
                Some(("stage all", GitOp::StageAll)),
                cx,
            ))
            .child(self.render_graph())
    }
}
