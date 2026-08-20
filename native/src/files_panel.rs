//! File explorer sidebar view: a lazy tree over the focused terminal's
//! working directory. Directory listings load on the background executor;
//! clicking a file opens it with the system default app.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{div, px, rgb, Context, SharedString, Window};

use crate::themes::Theme;

/// Cap per directory so a node_modules can't flood the panel.
const MAX_ENTRIES: usize = 500;

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

pub struct FilesPanel {
    theme: &'static Theme,
    root: Option<PathBuf>,
    listings: HashMap<PathBuf, DirListing>,
    expanded: HashSet<PathBuf>,
    /// Monotonic guard: listings only apply while their load is current.
    load_seq: u64,
}

fn read_dir_sorted(path: &Path) -> DirListing {
    let mut entries: Vec<FileEntry> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
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
    let truncated = entries.len() > MAX_ENTRIES;
    entries.truncate(MAX_ENTRIES);
    DirListing { entries, truncated }
}

impl FilesPanel {
    pub fn new(theme: &'static Theme, _cx: &mut Context<Self>) -> Self {
        Self {
            theme,
            root: None,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            load_seq: 0,
        }
    }

    pub fn set_theme(&mut self, theme: &'static Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Follow the focused terminal's cwd; a new root resets the tree.
    pub fn set_root(&mut self, cwd: Option<String>, cx: &mut Context<Self>) {
        let Some(cwd) = cwd.map(PathBuf::from) else {
            return;
        };
        if self.root.as_ref() == Some(&cwd) {
            return;
        }
        self.root = Some(cwd.clone());
        self.listings.clear();
        self.expanded.clear();
        self.load_dir(cwd, cx);
    }

    /// Re-list the root and every expanded directory.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.listings.clear();
        if let Some(root) = self.root.clone() {
            self.load_dir(root, cx);
        }
        for dir in self.expanded.clone() {
            self.load_dir(dir, cx);
        }
    }

    fn load_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load_seq += 1;
        let seq = self.load_seq;
        cx.spawn(async move |panel, cx| {
            let listing_path = path.clone();
            let listing = cx
                .background_executor()
                .spawn(async move { read_dir_sorted(&listing_path) })
                .await;
            let _ = panel.update(cx, |panel: &mut FilesPanel, cx| {
                // A root change bumps load_seq via its own loads and clears
                // the map, so stale listings from an old root never attach.
                if panel.load_seq >= seq && panel.root.is_some() {
                    let belongs = panel
                        .root
                        .as_ref()
                        .is_some_and(|root| path.starts_with(root));
                    if belongs {
                        panel.listings.insert(path, listing);
                        cx.notify();
                    }
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    fn toggle_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
            let row = div()
                .id(SharedString::from(format!("file-{}", entry.path.display())))
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
                            // Files open with the system default app.
                            let _ = std::process::Command::new("/usr/bin/open")
                                .arg(&path)
                                .spawn();
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
            .w(px(320.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .border_r_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .text_color(rgb(theme.ui_text));

        let Some(root) = self.root.clone() else {
            return base.child(
                div()
                    .p(px(12.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child("focus a terminal to browse its directory"),
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
