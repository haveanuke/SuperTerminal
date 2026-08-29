//! File explorer sidebar view: a lazy tree over the focused terminal's
//! working directory. Directory listings load on the background executor;
//! clicking a file opens it with the system default app.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{div, px, rgb, Context, SharedString, Window};

use crate::hosts::PanelTarget;
use crate::themes::Theme;

/// Cap per directory so a node_modules can't flood the panel.
const MAX_ENTRIES: usize = 500;
/// Hard scan bound: stop READING a directory after this many raw entries,
/// so the cap limits work, not just display.
const MAX_SCAN: usize = 2000;

#[derive(Clone, Debug)]
struct FileEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

/// Listing outcome for one directory (capped, with the overflow noted).
#[derive(Clone, Debug)]
struct DirListing {
    entries: Vec<FileEntry>,
    truncated: bool,
}

/// Emitted when a file row is clicked; the workspace opens the viewer.
pub struct OpenFile(pub PathBuf);

impl gpui::EventEmitter<OpenFile> for FilesPanel {}

pub struct FilesPanel {
    theme: &'static Theme,
    target: PanelTarget,
    listings: HashMap<PathBuf, DirListing>,
    expanded: HashSet<PathBuf>,
    /// Bumped whenever the tree resets (root change, refresh); results from
    /// an older tree never attach.
    root_gen: u64,
    /// Newest request token per directory; an older in-flight load for the
    /// same directory loses to a newer one.
    load_tokens: HashMap<PathBuf, u64>,
    next_token: u64,
}

fn read_dir_sorted(path: &Path) -> DirListing {
    let mut scanned = 0usize;
    let mut entries: Vec<FileEntry> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .take(MAX_SCAN)
        .inspect(|_| scanned += 1)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().ok()?.is_dir();
            Some(FileEntry {
                path: entry.path(),
                name,
                is_dir,
            })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let truncated = entries.len() > MAX_ENTRIES || scanned >= MAX_SCAN;
    entries.truncate(MAX_ENTRIES);
    DirListing { entries, truncated }
}

impl FilesPanel {
    pub fn new(theme: &'static Theme, _cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            target: PanelTarget::Detached,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            root_gen: 0,
            load_tokens: HashMap::new(),
            next_token: 0,
        }
    }

    pub fn set_theme(&mut self, theme: &'static Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Point the panel at a new target. Unlike the old `Option<cwd>`
    /// setter, which returned early on `None` and left the previous tree
    /// live and browsable, this ALWAYS applies: Detached and Remote both
    /// reset the tree instead of leaving the previous root's listing up.
    pub fn set_target(&mut self, target: PanelTarget, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.listings.clear();
        self.expanded.clear();
        self.load_tokens.clear();
        self.root_gen = self.root_gen.wrapping_add(1);
        cx.notify();
        if let PanelTarget::Local(path) = self.target.clone() {
            self.load_dir(path, cx);
        }
    }

    /// Re-list the root and every expanded directory.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.target.is_local() {
            return;
        }
        self.listings.clear();
        self.load_tokens.clear();
        self.root_gen = self.root_gen.wrapping_add(1);
        if let Some(root) = self.target.local_path().map(Path::to_path_buf) {
            self.load_dir(root, cx);
        }
        for dir in self.expanded.clone() {
            self.load_dir(dir, cx);
        }
    }

    fn load_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.next_token += 1;
        let token = self.next_token;
        let root_gen = self.root_gen;
        self.load_tokens.insert(path.clone(), token);
        cx.spawn(async move |panel, cx| {
            let listing_path = path.clone();
            let listing = cx
                .background_executor()
                .spawn(async move { read_dir_sorted(&listing_path) })
                .await;
            let _ = panel.update(cx, |panel: &mut FilesPanel, cx| {
                // Attach only when this is still the newest request for the
                // path AND the tree hasn't reset since it started.
                if panel.root_gen == root_gen && panel.load_tokens.get(&path) == Some(&token) {
                    panel.listings.insert(path, listing);
                    cx.notify();
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.target.is_local() {
            return;
        }
        if self.expanded.remove(&path) {
            cx.notify();
            return;
        }
        self.expanded.insert(path.clone());
        if !self.listings.contains_key(&path) {
            self.load_dir(path, cx);
        }
        cx.notify();
    }

    /// Depth-first rows for every visible entry.
    fn push_rows(
        &self,
        dir: &Path,
        depth: usize,
        rows: &mut Vec<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme;
        let Some(listing) = self.listings.get(dir).cloned() else {
            rows.push(
                div()
                    .pl(px(10.0 + depth as f32 * 12.0))
                    .h(px(20.0))
                    .text_size(px(10.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child("loading...")
                    .into_any_element(),
            );
            return;
        };
        for entry in &listing.entries {
            let expanded = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            let is_dir = entry.is_dir;
            let row_id = {
                use std::hash::{Hash, Hasher};
                use std::os::unix::ffi::OsStrExt;
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                entry.path.as_os_str().as_bytes().hash(&mut hasher);
                hasher.finish()
            };
            let row = div()
                .id(SharedString::from(format!("file-{row_id:x}")))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .h(px(20.0))
                .pl(px(10.0 + depth as f32 * 12.0))
                .pr(px(8.0))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(theme.ui_surface)))
                .child(
                    div()
                        .flex_none()
                        .w(px(10.0))
                        .text_size(px(8.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .children(is_dir.then(|| {
                            SharedString::from(if expanded { "\u{25be}" } else { "\u{25b8}" })
                        })),
                )
                .child(
                    div()
                        .flex_grow()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(11.0))
                        .text_color(rgb(if is_dir {
                            theme.ui_text
                        } else {
                            theme.ui_text_muted
                        }))
                        .child(SharedString::from(entry.name.clone())),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |panel, _, _, cx| {
                        if is_dir {
                            panel.toggle_dir(path.clone(), cx);
                        } else {
                            // The workspace opens the real viewer beside
                            // the terminals.
                            cx.emit(OpenFile(path.clone()));
                        }
                    }),
                );
            rows.push(row.into_any_element());
            if expanded {
                self.push_rows(&entry.path, depth + 1, rows, cx);
            }
        }
        if listing.truncated {
            rows.push(
                div()
                    .pl(px(10.0 + depth as f32 * 12.0))
                    .h(px(18.0))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(SharedString::from(format!(
                        "showing first {MAX_ENTRIES} entries"
                    )))
                    .into_any_element(),
            );
        }
    }
}

use gpui::MouseButton;

impl Render for FilesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let base = div()
            .w(px(280.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .border_r_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .text_color(rgb(theme.ui_text));

        let Some(root) = self.target.local_path().map(Path::to_path_buf) else {
            let message: SharedString = match &self.target {
                PanelTarget::Remote(label) => {
                    format!("{label}: remote file browser not available in this release").into()
                }
                _ => "focus a terminal to browse its directory".into(),
            };
            return base.child(
                div()
                    .p(px(12.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(message),
            );
        };

        let root_label: SharedString = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string())
            .into();

        let mut rows = Vec::new();
        self.push_rows(&root, 0, &mut rows, cx);

        base.child(
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
                        .text_size(px(9.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .child("FILES"),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(theme.ui_accent))
                        .child(root_label),
                )
                .child(div().flex_grow())
                .child(
                    div()
                        .id("files-refresh")
                        .cursor_pointer()
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(theme.ui_border))
                        .bg(rgb(theme.ui_surface))
                        .text_size(px(10.0))
                        .hover(|style| style.border_color(rgb(theme.ui_accent)))
                        .child("refresh")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|panel, _, _, cx| panel.refresh(cx)),
                        ),
                ),
        )
        .child(
            div()
                .id("files-scroll")
                .flex_grow()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
    }
}
