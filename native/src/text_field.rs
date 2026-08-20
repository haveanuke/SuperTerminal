//! Minimal single-line text input for overlay forms (session names).
//!
//! Deliberately simple: character insertion via `key_char`, backspace,
//! submit/cancel callbacks. Full IME-grade editing is not required for the
//! short ASCII-ish names this collects.

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, SharedString,
    Window,
};

use crate::themes::Theme;

#[derive(Clone, Debug)]
pub enum TextFieldEvent {
    Submitted(String),
    Cancelled,
}

pub struct TextField {
    pub value: String,
    placeholder: SharedString,
    focus_handle: FocusHandle,
    theme: &'static Theme,
    /// Slim single-line style for inline use (tab rename in the bar).
    compact: bool,
}

impl TextField {
    pub fn new(placeholder: &str, theme: &'static Theme, cx: &mut Context<Self>) -> Self {
        Self {
            value: String::new(),
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
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
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
                self.value.pop();
                cx.notify();
                return;
            }
            _ => {}
        }
        if ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        if let Some(key_char) = &ks.key_char {
            if !key_char.is_empty() && !key_char.chars().any(char::is_control) {
                self.value.push_str(key_char);
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
        let showing_placeholder = self.value.is_empty();
        let text: SharedString = if showing_placeholder {
            self.placeholder.clone()
        } else {
            SharedString::from(self.value.clone())
        };
        let (pad_x, pad_y) = if self.compact { (4.0, 0.0) } else { (8.0, 4.0) };
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
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
            .text_color(rgb(if showing_placeholder {
                theme.ui_text_muted
            } else {
                theme.ui_text
            }))
            .child(text)
            .child(div().w(px(1.5)).h(px(14.0)).bg(rgb(if focused {
                theme.cursor
            } else {
                theme.ui_background
            })))
            .flex()
            .flex_row()
            .items_center()
    }
}
