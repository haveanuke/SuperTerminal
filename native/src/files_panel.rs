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

pub struct FilesPanel {
    theme: &'static Theme,
    root: Option<PathBuf>,
    listings: HashMap<PathBuf, DirListing>,
    expanded: HashSet<PathBuf>,
    /// Bumped whenever the tree resets (root change, refresh); results from
    /// an older tree never attach.
    root_gen: u64,
    /// Newest request token per directory; an older in-flight load for the
    /// same directory loses to a newer one.
    load_tokens: HashMap<PathBuf, u64>,
    next_token: u64,
    /// Inline previews open under file rows: path -> lines once loaded.
    previews: HashMap<PathBuf, Option<Vec<String>>>,
    /// Newest preview request per path (same pattern as `load_tokens`).
    preview_tokens: HashMap<PathBuf, u64>,
}

/// Byte/line caps for inline previews.
const MAX_PREVIEW_BYTES: u64 = 256 * 1024;
const MAX_PREVIEW_LINES: usize = 400;

/// Bounded, binary-safe text preview of a file.
fn read_preview(path: &Path) -> Vec<String> {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return vec!["could not open file".to_string()];
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_PREVIEW_BYTES)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return vec!["could not read file".to_string()];
    }
    if bytes.contains(&0) {
        return vec!["binary file - use 'open' to view".to_string()];
    }
    let capped = bytes.len() as u64 >= MAX_PREVIEW_BYTES;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .lines()
        .take(MAX_PREVIEW_LINES)
        .map(String::from)
        .collect();
    if capped || text.lines().count() > MAX_PREVIEW_LINES {
        lines.push("... truncated".to_string());
    }
    if lines.is_empty() {
        lines.push("empty file".to_string());
    }
    lines
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
            root: None,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            root_gen: 0,
            load_tokens: HashMap::new(),
            next_token: 0,
            previews: HashMap::new(),
            preview_tokens: HashMap::new(),
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
        self.load_tokens.clear();
        self.previews.clear();
        self.preview_tokens.clear();
        self.root_gen += 1;
        self.load_dir(cwd, cx);
    }

    /// Re-list the root and every expanded directory.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.listings.clear();
        self.load_tokens.clear();
        self.root_gen += 1;
        if let Some(root) = self.root.clone() {
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

    /// Toggle the inline preview under a file row.
    fn toggle_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.previews.remove(&path).is_some() {
            self.preview_tokens.remove(&path);
            cx.notify();
            return;
        }
        self.previews.insert(path.clone(), None);
        self.next_token += 1;
        let token = self.next_token;
        self.preview_tokens.insert(path.clone(), token);
        let root_gen = self.root_gen;
        cx.notify();
        cx.spawn(async move |panel, cx| {
            let read_path = path.clone();
            let lines = cx
                .background_executor()
                .spawn(async move { read_preview(&read_path) })
                .await;
            let _ = panel.update(cx, |panel: &mut FilesPanel, cx| {
                // Same discipline as directory loads: newest token per path,
                // same tree generation — a close/reopen races cleanly.
                if panel.root_gen == root_gen && panel.preview_tokens.get(&path) == Some(&token) {
                    if let Some(slot) = panel.previews.get_mut(&path) {
                        *slot = Some(lines);
                        cx.notify();
                    }
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// The preview block spliced under an open file row.
    fn render_preview(
        &self,
        path: &Path,
        depth: usize,
        lines: &Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.theme;
        let indent = 10.0 + depth as f32 * 12.0;
        let open_path = path.to_path_buf();
        let chip_id = {
            use std::hash::{Hash, Hasher};
            use std::os::unix::ffi::OsStrExt;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            path.as_os_str().as_bytes().hash(&mut hasher);
            hasher.finish()
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pl(px(indent))
            .h(px(18.0))
            .child(
                div()
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child("PREVIEW"),
            )
            .child(
                div()
                    .id(SharedString::from(format!("preview-open-{chip_id:x}")))
                    .cursor_pointer()
                    .px(px(6.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.ui_border))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text))
                    .hover(|style| style.border_color(rgb(theme.ui_accent)))
                    .child("open")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            let _ = std::process::Command::new("/usr/bin/open")
                                .arg(&open_path)
                                .spawn();
                        }),
                    ),
            );
        let body: Vec<gpui::AnyElement> = match lines {
            None => vec![div()
                .pl(px(indent))
                .h(px(14.0))
                .text_color(rgb(theme.ui_text_muted))
                .child("loading...")
                .into_any_element()],
            Some(lines) => lines
                .iter()
                .map(|line| {
                    div()
                        .pl(px(indent))
                        .pr(px(4.0))
                        .min_h(px(14.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(theme.ui_text))
                        .child(SharedString::from(line.clone()))
                        .into_any_element()
                })
                .collect(),
        };
        div()
            .flex()
            .flex_col()
            .py(px(2.0))
            .bg(rgb(theme.ui_surface))
            .font_family("Menlo")
            .text_size(px(10.0))
            .child(header)
            .children(body)
            .into_any_element()
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
                            // Files preview INSIDE the panel; the preview
                            // header offers external open.
                            panel.toggle_preview(path.clone(), cx);
                        }
                    }),
                );
            rows.push(row.into_any_element());
            if !entry.is_dir {
                if let Some(lines) = self.previews.get(&entry.path) {
                    let lines = lines.clone();
                    rows.push(self.render_preview(&entry.path, depth + 1, &lines, cx));
                }
            }
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
