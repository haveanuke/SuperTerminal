//! Read-only file viewer that occupies the content area beside the terminal
//! tree. Real text selection (drag, Cmd+A, Cmd+C) over a monospace grid —
//! the inline sidebar preview couldn't be copied from, this can.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, SharedString, Window,
};

use crate::themes::Theme;

/// Byte / line caps: enough for logs and sources, never a gigabyte.
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINES: usize = 20_000;
const PAD: f32 = 8.0;

pub struct ViewerClosed;

impl EventEmitter<ViewerClosed> for FileViewer {}

/// (line, column) position in the loaded text, columns in chars.
type TextPos = (usize, usize);

pub struct FileViewer {
    theme: &'static Theme,
    font_family: String,
    font_size: f32,
    path: PathBuf,
    lines: Option<Vec<String>>,
    notice: Option<&'static str>,
    /// Selection anchor and head; normalized on use.
    anchor: Option<TextPos>,
    head: Option<TextPos>,
    selecting: bool,
    /// Text-area origin in window coords, written during prepaint by a
    /// measuring canvas; read by mouse handlers for column math.
    origin: Arc<Mutex<(f32, f32)>>,
    cell_width: f32,
    focus_handle: FocusHandle,
    /// Grab keyboard focus on the first render so esc/Cmd+C work
    /// immediately after opening (one-shot: never steals afterwards).
    needs_focus: bool,
}

fn load_file(path: &PathBuf) -> (Option<Vec<String>>, Option<&'static str>) {
    let Ok(file) = std::fs::File::open(path) else {
        return (None, Some("could not open file"));
    };
    let mut bytes = Vec::new();
    if file.take(MAX_BYTES).read_to_end(&mut bytes).is_err() {
        return (None, Some("could not read file"));
    }
    if bytes.contains(&0) {
        return (None, Some("binary file - use 'open' to view externally"));
    }
    let capped = bytes.len() as u64 >= MAX_BYTES;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().take(MAX_LINES).map(String::from).collect();
    let truncated = capped || text.lines().count() > MAX_LINES;
    let notice = truncated.then_some("truncated - use 'open' for the full file");
    if lines.is_empty() {
        lines.push(String::new());
    }
    (Some(lines), notice)
}

fn byte_index(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

impl FileViewer {
    pub fn new(
        path: PathBuf,
        theme: &'static Theme,
        font_family: String,
        font_size: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let load_path = path.clone();
        cx.spawn(async move |viewer, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_file(&load_path) })
                .await;
            let _ = viewer.update(cx, |viewer: &mut FileViewer, cx| {
                let (lines, notice) = loaded;
                viewer.lines = lines;
                viewer.notice = notice;
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
        Self {
            theme,
            font_family,
            font_size,
            path,
            lines: None,
            notice: None,
            anchor: None,
            head: None,
            selecting: false,
            origin: Arc::new(Mutex::new((0.0, 0.0))),
            cell_width: font_size * 0.6,
            focus_handle: cx.focus_handle(),
            needs_focus: true,
        }
    }

    pub fn set_appearance(
        &mut self,
        theme: &'static Theme,
        font_family: &str,
        font_size: f32,
        cx: &mut Context<Self>,
    ) {
        self.theme = theme;
        self.font_family = font_family.to_string();
        self.font_size = font_size;
        cx.notify();
    }

    /// Normalized selection (start <= end), None while empty.
    fn selection(&self) -> Option<(TextPos, TextPos)> {
        let (anchor, head) = (self.anchor?, self.head?);
        if anchor == head {
            return None;
        }
        Some(if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        })
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        let lines = self.lines.as_ref()?;
        let mut out = String::new();
        let last = end.0.min(lines.len().saturating_sub(1));
        for (line_index, line) in lines.iter().enumerate().take(last + 1).skip(start.0) {
            let len = line.chars().count();
            let from = if line_index == start.0 {
                start.1.min(len)
            } else {
                0
            };
            let to = if line_index == end.0 {
                end.1.min(len)
            } else {
                len
            };
            if line_index > start.0 {
                out.push('\n');
            }
            out.push_str(&line[byte_index(line, from)..byte_index(line, to)]);
        }
        Some(out)
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let text = self
            .selected_text()
            .or_else(|| self.lines.as_ref().map(|lines| lines.join("\n")));
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn column_at(&self, x: f32, line: &str) -> usize {
        let origin_x = self.origin.lock().unwrap().0;
        let col = ((x - origin_x - PAD) / self.cell_width.max(1.0)).round();
        (col.max(0.0) as usize).min(line.chars().count())
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform {
            match ks.key.as_str() {
                "c" => self.copy(cx),
                "a" => {
                    if let Some(lines) = &self.lines {
                        let last = lines.len().saturating_sub(1);
                        self.anchor = Some((0, 0));
                        self.head = Some((last, lines[last].chars().count()));
                        cx.notify();
                    }
                }
                _ => {}
            }
            return;
        }
        if ks.key == "escape" {
            cx.emit(ViewerClosed);
        }
    }
}

impl Focusable for FileViewer {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        if self.needs_focus {
            self.focus_handle.focus(window);
            self.needs_focus = false;
        }
        // Measure through the shaping pipeline so column math matches glyphs.
        let probe: SharedString = "MMMMMMMMMM".into();
        let run = gpui::TextRun {
            len: probe.len(),
            font: gpui::font(self.font_family.clone()),
            color: gpui::Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_line(probe, px(self.font_size), &[run], None);
        if f32::from(shaped.width) > 0.0 {
            self.cell_width = f32::from(shaped.width) / 10.0;
        }
        let line_height = px((self.font_size * 1.4).round());

        let name: SharedString = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();
        let dir: SharedString = self
            .path
            .parent()
            .map(|p| {
                p.display()
                    .to_string()
                    .replace(&std::env::var("HOME").unwrap_or_default(), "~")
            })
            .unwrap_or_default()
            .into();

        let selection = self.selection();
        let mut body: Vec<gpui::AnyElement> = Vec::new();
        match &self.lines {
            None => {
                body.push(
                    div()
                        .px(px(PAD))
                        .text_color(rgb(theme.ui_text_muted))
                        .child(self.notice.unwrap_or("loading..."))
                        .into_any_element(),
                );
            }
            Some(lines) => {
                for (index, line) in lines.iter().enumerate() {
                    let len = line.chars().count();
                    // Selected span on this line, in chars.
                    let span = selection.and_then(|(start, end)| {
                        if index < start.0 || index > end.0 {
                            return None;
                        }
                        let from = if index == start.0 {
                            start.1.min(len)
                        } else {
                            0
                        };
                        let to = if index == end.0 { end.1.min(len) } else { len };
                        Some((from, to))
                    });
                    let line_for_down = line.clone();
                    let line_for_move = line.clone();
                    let mut row = div()
                        .id(index)
                        .h(line_height)
                        .px(px(PAD))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .flex()
                        .flex_row()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |viewer, event: &gpui::MouseDownEvent, window, cx| {
                                viewer.focus_handle.focus(window);
                                window.prevent_default();
                                let col =
                                    viewer.column_at(f32::from(event.position.x), &line_for_down);
                                viewer.anchor = Some((index, col));
                                viewer.head = Some((index, col));
                                viewer.selecting = true;
                                cx.notify();
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |viewer, event: &gpui::MouseMoveEvent, _, cx| {
                                if viewer.selecting {
                                    let col = viewer
                                        .column_at(f32::from(event.position.x), &line_for_move);
                                    viewer.head = Some((index, col));
                                    cx.notify();
                                }
                            },
                        ));
                    match span {
                        Some((from, to)) if from < to || len == 0 => {
                            let b0 = byte_index(line, from);
                            let b1 = byte_index(line, to);
                            let pre = line[..b0].to_string();
                            let mid = line[b0..b1].to_string();
                            let post = line[b1..].to_string();
                            if !pre.is_empty() {
                                row = row.child(SharedString::from(pre));
                            }
                            row = row.child(
                                div()
                                    .bg(rgb(theme.selection))
                                    // Full-line middle rows still show a
                                    // selected empty width.
                                    .min_w(px(4.0))
                                    .child(SharedString::from(mid)),
                            );
                            if !post.is_empty() {
                                row = row.child(SharedString::from(post));
                            }
                        }
                        _ => {
                            row = row.child(SharedString::from(line.clone()));
                        }
                    }
                    body.push(row.into_any_element());
                }
                if let Some(notice) = self.notice {
                    body.push(
                        div()
                            .px(px(PAD))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(notice)
                            .into_any_element(),
                    );
                }
            }
        }

        let origin = Arc::clone(&self.origin);
        let measure = gpui::canvas(
            move |bounds, _, _| {
                *origin.lock().unwrap() = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let open_path = self.path.clone();
        div()
            .flex_grow()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .bg(rgb(theme.background))
            .border_l_1()
            .border_color(rgb(theme.ui_border))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|viewer, _, _, cx| {
                    if viewer.selecting {
                        viewer.selecting = false;
                        cx.notify();
                    }
                }),
            )
            // header
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .bg(rgb(theme.ui_background))
                    .border_b_1()
                    .border_color(rgb(theme.ui_border))
                    .text_size(px(11.0))
                    .child(div().text_color(rgb(theme.ui_accent)).child(name))
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(dir),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .id("viewer-copy")
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(theme.ui_border))
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text))
                            .hover(|style| style.border_color(rgb(theme.ui_accent)))
                            .child("copy")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|viewer, _, _, cx| {
                                    cx.stop_propagation();
                                    viewer.copy(cx);
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("viewer-open")
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(theme.ui_border))
                            .text_size(px(10.0))
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
                    )
                    .child(
                        div()
                            .id("viewer-close")
                            .cursor_pointer()
                            .px(px(5.0))
                            .rounded(px(3.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .hover(|style| style.text_color(rgb(theme.red)))
                            .child("x")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|_, _, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(ViewerClosed);
                                }),
                            ),
                    ),
            )
            // body
            .child(
                div()
                    .id("viewer-scroll")
                    .flex_grow()
                    .relative()
                    .overflow_y_scroll()
                    .py(px(4.0))
                    .font_family(self.font_family.clone())
                    .text_size(px(self.font_size))
                    .line_height(line_height)
                    .text_color(rgb(theme.foreground))
                    .cursor_text()
                    .child(measure)
                    .children(body),
            )
    }
}
