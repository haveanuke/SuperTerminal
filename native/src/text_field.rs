//! Single-line text input for overlay forms and inline renames.
//!
//! A real little editor: caret movement, shift-selection, select-all,
//! clipboard copy/cut/paste, home/end. Indices are char-based throughout;
//! conversion to byte offsets happens only at slice boundaries.

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, SharedString, Window,
};

use crate::themes::Theme;

#[derive(Clone, Debug)]
pub enum TextFieldEvent {
    Submitted(String),
    Cancelled,
}

pub struct TextField {
    pub value: String,
    /// Caret position in chars.
    caret: usize,
    /// Selection anchor in chars; selection = anchor..caret (either order).
    anchor: Option<usize>,
    placeholder: SharedString,
    focus_handle: FocusHandle,
    theme: &'static Theme,
    /// Slim single-line style for inline use (tab rename in the bar).
    compact: bool,
}

fn byte_index(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

impl TextField {
    pub fn new(placeholder: &str, theme: &'static Theme, cx: &mut Context<Self>) -> Self {
        Self {
            value: String::new(),
            caret: 0,
            anchor: None,
            placeholder: placeholder.to_string().into(),
            focus_handle: cx.focus_handle(),
            theme,
            compact: false,
        }
    }

    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.value.clear();
        self.caret = 0;
        self.anchor = None;
        cx.notify();
    }

    /// Prefill with `text`, fully selected (typing replaces it wholesale).
    pub fn set_text_selected(&mut self, text: &str, cx: &mut Context<Self>) {
        self.value = text.to_string();
        self.caret = self.value.chars().count();
        self.anchor = (self.caret > 0).then_some(0);
        cx.notify();
    }

    pub fn set_theme(&mut self, theme: &'static Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.caret {
            return None;
        }
        Some((anchor.min(self.caret), anchor.max(self.caret)))
    }

    fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        let (b0, b1) = (byte_index(&self.value, start), byte_index(&self.value, end));
        Some(self.value[b0..b1].to_string())
    }

    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            return false;
        };
        let (b0, b1) = (byte_index(&self.value, start), byte_index(&self.value, end));
        self.value.replace_range(b0..b1, "");
        self.caret = start;
        self.anchor = None;
        true
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let at = byte_index(&self.value, self.caret);
        self.value.insert_str(at, text);
        self.caret += text.chars().count();
    }

    fn move_caret(&mut self, to: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = to.min(self.value.chars().count());
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let len = self.value.chars().count();

        if m.platform {
            match ks.key.as_str() {
                "a" => {
                    self.anchor = (len > 0).then_some(0);
                    self.caret = len;
                    cx.notify();
                }
                "c" => {
                    if let Some(text) = self.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                }
                "x" => {
                    if let Some(text) = self.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        self.delete_selection();
                        cx.notify();
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
                        self.insert(&clean);
                        cx.notify();
                    }
                }
                "left" => {
                    self.move_caret(0, m.shift);
                    cx.notify();
                }
                "right" => {
                    self.move_caret(len, m.shift);
                    cx.notify();
                }
                _ => {}
            }
            return;
        }

        match ks.key.as_str() {
            "enter" => {
                cx.emit(TextFieldEvent::Submitted(self.value.clone()));
                return;
            }
            "escape" => {
                cx.emit(TextFieldEvent::Cancelled);
                return;
            }
            "backspace" => {
                if !self.delete_selection() && self.caret > 0 {
                    let b0 = byte_index(&self.value, self.caret - 1);
                    let b1 = byte_index(&self.value, self.caret);
                    self.value.replace_range(b0..b1, "");
                    self.caret -= 1;
                }
                cx.notify();
                return;
            }
            "delete" => {
                if !self.delete_selection() && self.caret < len {
                    let b0 = byte_index(&self.value, self.caret);
                    let b1 = byte_index(&self.value, self.caret + 1);
                    self.value.replace_range(b0..b1, "");
                }
                cx.notify();
                return;
            }
            "left" => {
                if !m.shift && self.selection().is_some() {
                    let (start, _) = self.selection().unwrap();
                    self.caret = start;
                    self.anchor = None;
                } else {
                    self.move_caret(self.caret.saturating_sub(1), m.shift);
                }
                cx.notify();
                return;
            }
            "right" => {
                if !m.shift && self.selection().is_some() {
                    let (_, end) = self.selection().unwrap();
                    self.caret = end;
                    self.anchor = None;
                } else {
                    self.move_caret(self.caret + 1, m.shift);
                }
                cx.notify();
                return;
            }
            "home" => {
                self.move_caret(0, m.shift);
                cx.notify();
                return;
            }
            "end" => {
                self.move_caret(len, m.shift);
                cx.notify();
                return;
            }
            _ => {}
        }

        if m.control {
            return;
        }
        if let Some(key_char) = &ks.key_char {
            if !key_char.is_empty() && !key_char.chars().any(char::is_control) {
                self.insert(key_char);
                cx.notify();
            }
        }
    }
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let focused = self.focus_handle.is_focused(window);
        let caret_h = if self.compact { 11.0 } else { 14.0 };

        // Value split into [pre-selection][selection][post] with the caret
        // rendered at its char position.
        let mut pieces: Vec<gpui::AnyElement> = Vec::new();
        let showing_placeholder = self.value.is_empty();
        if showing_placeholder {
            if focused {
                pieces.push(
                    div()
                        .w(px(1.5))
                        .h(px(caret_h))
                        .bg(rgb(theme.cursor))
                        .into_any_element(),
                );
            }
            pieces.push(
                div()
                    .text_color(rgb(theme.ui_text_muted))
                    .child(self.placeholder.clone())
                    .into_any_element(),
            );
        } else {
            let (sel_start, sel_end) = self.selection().unwrap_or((self.caret, self.caret));
            let b_start = byte_index(&self.value, sel_start);
            let b_end = byte_index(&self.value, sel_end);
            let pre = self.value[..b_start].to_string();
            let mid = self.value[b_start..b_end].to_string();
            let post = self.value[b_end..].to_string();
            let caret_at_start = self.caret == sel_start;

            let push_caret = |pieces: &mut Vec<gpui::AnyElement>| {
                if focused {
                    pieces.push(
                        div()
                            .w(px(1.5))
                            .h(px(caret_h))
                            .flex_none()
                            .bg(rgb(theme.cursor))
                            .into_any_element(),
                    );
                }
            };

            if !pre.is_empty() {
                pieces.push(div().child(SharedString::from(pre)).into_any_element());
            }
            if caret_at_start {
                push_caret(&mut pieces);
            }
            if !mid.is_empty() {
                pieces.push(
                    div()
                        .bg(rgb(theme.selection))
                        .child(SharedString::from(mid))
                        .into_any_element(),
                );
            }
            if !caret_at_start {
                push_caret(&mut pieces);
            }
            if !post.is_empty() {
                pieces.push(div().child(SharedString::from(post)).into_any_element());
            }
        }

        let (pad_x, pad_y) = if self.compact { (4.0, 0.0) } else { (8.0, 4.0) };
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|field, _, window, cx| {
                    field.focus_handle.focus(window);
                    field.caret = field.value.chars().count();
                    field.anchor = None;
                    cx.notify();
                }),
            )
            .cursor_text()
            .w_full()
            .when(self.compact, |d| d.h(px(15.0)).text_size(px(10.0)))
            .px(px(pad_x))
            .py(px(pad_y))
            .rounded(px(4.0))
            .bg(rgb(theme.ui_background))
            .border_1()
            .border_color(rgb(if focused {
                theme.ui_accent
            } else {
                theme.ui_border
            }))
            .text_color(rgb(theme.ui_text))
            .flex()
            .flex_row()
            .items_center()
            .whitespace_nowrap()
            .overflow_hidden()
            .children(pieces)
    }
}
