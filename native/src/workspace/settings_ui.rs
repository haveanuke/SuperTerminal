//! Settings-sheet section renderers and appearance mutators, split from
//! the workspace (same module tree: private Workspace fields stay
//! reachable). Pure move — behavior identical.

use gpui::{div, px, rgb, Context, MouseButton, SharedString, Window};

use super::*;
use crate::companion::auth::PeerId;
use crate::peers;

/// One-line explanations for the settings controls whose names cannot
/// carry their own meaning. "tool adapters: on" says nothing about what a
/// tool adapter IS; the chip stays terse and the hint does the explaining.
/// Kept together so they hold one voice and one length budget.
pub(super) mod hints {
    pub const TOOL_ADAPTERS: &str =
        "Shims that let claude and codex ring the bell when a long job ends. New terminals only.";
    pub const GALLERY: &str = "Images saved to this folder show up in the phone companion.";
    pub const LIVE_VIEWPORT: &str =
        "Sends Blender's viewport to the phone every 2s, only while the gallery is open.";
    pub const BUDDY: &str = "An agent that watches your terminals and comments on what it sees.";
    pub const THEME_FILE: &str = "Import a palette from JSON, or export the current one to share.";
    pub const CUES: &str = "Glass when a terminal finishes, Ping when one is waiting on you.";
    pub const AWAKE: &str = "Holds the Mac awake while a terminal is still working.";
    pub const PEER_CANDIDATES: &str =
        "Other online Macs on the tailnet. Pairing is manual -- nothing here is automatic.";
    pub const PAIRED_PEERS: &str =
        "Each grant starts off. Delete revokes a peer immediately, even mid-session.";

    #[cfg(test)]
    pub const ALL: [&str; 9] = [
        TOOL_ADAPTERS,
        GALLERY,
        LIVE_VIEWPORT,
        BUDDY,
        THEME_FILE,
        CUES,
        AWAKE,
        PEER_CANDIDATES,
        PAIRED_PEERS,
    ];

    /// Text budget that keeps hints short enough to read as captions. This
    /// is a character count, NOT a guarantee about wrapping — actual line
    /// breaks depend on font metrics and window width, and a narrow window
    /// will still wrap the longest of these.
    #[cfg(test)]
    pub const MAX_LEN: usize = 92;
    /// The UI takes SVG icons, never emoji; these are the only non-ASCII
    /// characters a hint may use.
    #[cfg(test)]
    pub const ALLOWED_NON_ASCII: [char; 3] = ['\u{2014}', '\u{00b7}', '\u{2026}'];
}

impl Workspace {
    /// Push the current settings to every pane and persist them.
    pub(super) fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        let _ = self.settings.save();
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        for pane in self.panes.values() {
            pane.update(cx, |pane, pane_cx| {
                pane.set_appearance(theme, &family, size, translucent, pane_cx)
            });
        }
        if let Some(panel) = self.git_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_theme(theme, panel_cx));
        }
        if let Some(panel) = self.files_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_theme(theme, panel_cx));
        }
        if let Some(viewer) = self.file_viewer.clone() {
            viewer.update(cx, |viewer, viewer_cx| {
                viewer.set_appearance(theme, &family, size, viewer_cx)
            });
        }
        // Text fields capture the theme at creation; keep them current.
        let fields = [
            self.session_field.clone(),
            self.auto_run_field.clone(),
            self.search_field.clone(),
            self.buddy_field.clone(),
            self.pet_name_field.clone(),
            self.rename_field.as_ref().map(|(_, field)| field.clone()),
        ];
        for field in fields.into_iter().flatten() {
            field.update(cx, |field, field_cx| field.set_theme(theme, field_cx));
        }
        cx.notify();
    }

    pub(super) fn apply_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(theme) = themes::by_name(name) {
            self.theme = theme;
            self.settings.theme = name.to_string();
            self.apply_appearance(cx);
        }
    }

    pub(super) fn set_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.settings.font_family = family.to_string();
        self.apply_appearance(cx);
    }

    pub(super) fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.settings.background_opacity = opacity.clamp(0.05, 1.0);
        self.apply_appearance(cx);
    }

    /// Native file picker without dependencies: osascript's choose-file,
    /// run off the UI thread.
    pub(super) fn pick_background_image(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose file of type {\"public.image\"} with prompt \"Background image\")",
                        ])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|path| !path.is_empty())
                })
                .await;
            if let Some(path) = picked {
                let _ = ws.update(cx, |ws, cx| {
                    ws.settings.background_image = Some(path);
                    ws.apply_appearance(cx);
                });
            }
            Ok::<(), ()>(())
        })
        .detach();
    }

    pub(super) fn clear_background_image(&mut self, cx: &mut Context<Self>) {
        self.settings.background_image = None;
        self.apply_appearance(cx);
    }

    pub(super) fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.settings.font_size = size.clamp(8.0, 32.0);
        self.apply_appearance(cx);
    }

    pub(super) fn apply_auto_run(&mut self, command: String, cx: &mut Context<Self>) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        let config = Some((
            command,
            self.auto_run_interval,
            self.auto_run_escape,
            self.auto_run_escape_delay,
        ));
        if let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        {
            pane.update(cx, |pane, _| pane.set_auto_run(config));
        }
        self.overlay = Overlay::None;
        cx.notify();
    }
    pub(super) fn import_theme(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose file of type {\"public.json\", \"public.plain-text\"} with prompt \"Theme JSON\")",
                        ])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|path| !path.is_empty())
                        .and_then(|path| std::fs::read_to_string(path).ok())
                })
                .await;
            let _ = ws.update(cx, |ws, cx| {
                let note = match picked
                    .ok_or_else(|| "no file chosen".to_string())
                    .and_then(|raw| {
                        serde_json::from_str::<serde_json::Value>(&raw)
                            .map_err(|_| "invalid JSON file".to_string())
                    })
                    .and_then(|json| {
                        themes::import_custom(&json).map(|theme| (json, theme.name))
                    }) {
                    Ok((json, name)) => {
                        ws.settings.custom_themes.push(json);
                        ws.apply_theme(name, cx);
                        format!("imported and applied: {name}")
                    }
                    Err(err) => err,
                };
                ws.theme_action_note = Some(note);
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    pub(super) fn export_theme(&mut self, cx: &mut Context<Self>) {
        let theme = self.theme;
        let json = themes::export_json(theme);
        let name: String = theme
            .name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let dest = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
            .join("Downloads")
            .join(format!("{name}-theme.json"));
        let note = match serde_json::to_string_pretty(&json)
            .map_err(|e| e.to_string())
            .and_then(|raw| std::fs::write(&dest, raw).map_err(|e| e.to_string()))
        {
            Ok(()) => format!("exported to {}", dest.display()),
            Err(err) => format!("export failed: {err}"),
        };
        self.theme_action_note = Some(note);
        cx.notify();
    }
    /// The explanatory line under a control, indented past the 72px label
    /// gutter so it reads as belonging to the row above it.
    pub(super) fn hint(&self, text: &'static str) -> impl IntoElement {
        div()
            .pl(px(80.0))
            .text_size(px(10.0))
            .text_color(rgb(self.theme.ui_text_muted))
            .child(SharedString::from(text))
    }

    /// A quiet label above a group of controls. The appearance pane holds
    /// four formerly separate sections; without these it reads as one
    /// undifferentiated pile of chips.
    pub(super) fn group_label(&self, text: &'static str) -> impl IntoElement {
        div()
            .text_size(px(9.0))
            .text_color(rgb(self.theme.ui_text_muted))
            .child(SharedString::from(text))
    }

    /// Theme import/export — the old "custom" section. It acts on the very
    /// palette shown above it, so it belongs in the appearance pane.
    pub(super) fn render_theme_file_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_color(rgb(self.theme.ui_text_muted))
                    .child(self.chip_button(
                        "import theme",
                        false,
                        |ws, _window, cx| ws.import_theme(cx),
                        cx,
                    ))
                    .child(self.chip_button(
                        "export current",
                        false,
                        |ws, _window, cx| ws.export_theme(cx),
                        cx,
                    ))
                    .children(
                        self.theme_action_note
                            .clone()
                            .map(|note| div().text_size(px(10.0)).child(SharedString::from(note))),
                    ),
            )
            .child(self.hint(hints::THEME_FILE))
    }

    pub(super) fn render_font_family_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let current = self.settings.font_family.clone();
        const MONO_HINTS: [&str; 10] = [
            "Mono", "Menlo", "Monaco", "Courier", "Consolas", "Code", "Term", "Hack", "Fira",
            "Input",
        ];
        let mut families: Vec<String> = window
            .text_system()
            .all_font_names()
            .into_iter()
            .filter(|name| MONO_HINTS.iter().any(|hint| name.contains(hint)))
            // Name hints admit traps like "Fira Sans"; verify by measuring.
            .filter(|name| TerminalPane::family_is_monospace(name, self.settings.font_size, window))
            .collect();
        families.sort();
        families.dedup();
        let chips: Vec<_> = families
            .into_iter()
            .map(|family| {
                let selected = family == current;
                let apply = family.clone();
                div()
                    .id(SharedString::from(format!("font-{family}")))
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(if selected {
                        theme.ui_accent
                    } else {
                        theme.ui_border
                    }))
                    .text_size(px(11.0))
                    .font_family(SharedString::from(family.clone()))
                    .child(SharedString::from(family))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.set_font_family(&apply, cx)),
                    )
            })
            .collect();
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).pt(px(3.0)).child("font"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(4.0))
                    .children(chips),
            )
    }

    /// The companion pane: what the phone can see. Two labelled rows, each
    /// with its own hint — "previews" and "live viewport" name mechanisms
    /// nobody can guess from the label alone.
    pub(super) fn render_previews_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let label: SharedString = match &self.settings.preview_dir {
            Some(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone())
                .into(),
            None => "Pictures/SuperTerminal".into(),
        };
        let overridden = self.settings.preview_dir.is_some();
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("gallery"))
                    .child(div().text_size(px(11.0)).child(label))
                    .child(self.chip_button(
                        "choose",
                        false,
                        |ws, _window, cx| ws.pick_preview_dir(cx),
                        cx,
                    ))
                    .children(overridden.then(|| {
                        self.chip_button(
                            "default",
                            false,
                            |ws, _window, cx| {
                                ws.settings.preview_dir = None;
                                let _ = ws.settings.save();
                                ws.apply_preview_dir();
                                cx.notify();
                            },
                            cx,
                        )
                    })),
            )
            .child(self.hint(hints::GALLERY))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("viewport"))
                    .child(self.chip_button(
                        if self.settings.blender_viewport {
                            "live viewport: on"
                        } else {
                            "live viewport: off"
                        },
                        self.settings.blender_viewport,
                        |ws, _window, cx| {
                            ws.settings.blender_viewport = !ws.settings.blender_viewport;
                            let _ = ws.settings.save();
                            if let Some(store) = &ws.companion_previews {
                                store.set_viewport_enabled(ws.settings.blender_viewport);
                            }
                            cx.notify();
                        },
                        cx,
                    )),
            )
            .child(self.hint(hints::LIVE_VIEWPORT))
    }

    pub(super) fn render_background_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let opacity = self.settings.background_opacity;
        let has_image = self.settings.background_image.is_some();
        let label: SharedString = match &self.settings.background_image {
            Some(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone())
                .into(),
            None => "none".into(),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).child("background"))
            .child(div().text_size(px(11.0)).child(label))
            .child(self.chip_button(
                "choose",
                false,
                |ws, _window, cx| ws.pick_background_image(cx),
                cx,
            ))
            .children(has_image.then(|| {
                self.chip_button(
                    "clear",
                    false,
                    |ws, _window, cx| ws.clear_background_image(cx),
                    cx,
                )
            }))
            .children(has_image.then(|| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(div().text_size(px(10.0)).child("opacity"))
                    .child(self.stepper(
                        "bg-opacity",
                        format!("{:.0}%", opacity * 100.0),
                        |ws, _window, cx| {
                            ws.set_background_opacity(ws.settings.background_opacity - 0.1, cx)
                        },
                        |ws, _window, cx| {
                            ws.set_background_opacity(ws.settings.background_opacity + 0.1, cx)
                        },
                        cx,
                    ))
            }))
    }

    pub(super) fn render_buddy_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        if self.buddy_field.is_none() {
            let theme_ref = self.theme;
            let field = cx.new(|field_cx| {
                TextField::new(
                    "agent command, e.g. claude -p {prompt}",
                    theme_ref,
                    field_cx,
                )
            });
            cx.subscribe(&field, |ws, _field, event: &TextFieldEvent, cx| {
                if let TextFieldEvent::Submitted(line) = event {
                    let mut parts = line.split_whitespace().map(String::from);
                    if let Some(command) = parts.next() {
                        ws.settings.buddy_command = command;
                        let args: Vec<String> = parts.collect();
                        ws.settings.buddy_args = if args.is_empty() {
                            vec!["-p".to_string(), "{prompt}".to_string()]
                        } else {
                            args
                        };
                        ws.settings.buddy_enabled = true;
                        let _ = ws.settings.save();
                        cx.notify();
                    }
                }
            })
            .detach();
            self.buddy_field = Some(field);
        }
        let enabled = self.settings.buddy_enabled;
        let configured = !self.settings.buddy_command.trim().is_empty();
        // Quick agent presets (old-app parity): one click configures and
        // enables the reviewer; the field stays for custom commands.
        let local_active = self.settings.buddy_command == "ollama";
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("buddy"))
                    .child(self.chip_button(
                        if enabled {
                            "reviewer: on"
                        } else {
                            "reviewer: off"
                        },
                        enabled,
                        |ws, _window, cx| {
                            ws.settings.buddy_enabled = !ws.settings.buddy_enabled;
                            let _ = ws.settings.save();
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(div().text_size(px(10.0)).child("agent:"))
                    .child(self.chip_button(
                        "claude",
                        self.settings.buddy_command == "claude",
                        |ws, _window, cx| {
                            ws.set_buddy_agent("claude", &["-p", "{prompt}"], cx);
                        },
                        cx,
                    ))
                    .child(self.chip_button(
                        "codex",
                        self.settings.buddy_command == "codex",
                        |ws, _window, cx| {
                            ws.set_buddy_agent("codex", &["exec", "{prompt}"], cx);
                        },
                        cx,
                    ))
                    .child(self.chip_button(
                        "local",
                        local_active,
                        |ws, _window, cx| {
                            ws.set_buddy_agent("ollama", &["run", "llama3", "{prompt}"], cx);
                        },
                        cx,
                    ))
                    .child(self.chip_button(
                        if self.settings.buddy_pet_visible {
                            "pet: shown"
                        } else {
                            "pet: hidden"
                        },
                        self.settings.buddy_pet_visible,
                        |ws, _window, cx| {
                            ws.settings.buddy_pet_visible = !ws.settings.buddy_pet_visible;
                            let _ = ws.settings.save();
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(self.chip_button(
                        "pet card",
                        false,
                        |ws, window, cx| ws.open_pet_card(window, cx),
                        cx,
                    )),
            )
            .child(self.hint(hints::BUDDY))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)))
                    .child(div().flex_grow().child(self.buddy_field.clone().unwrap()))
                    .children(configured.then(|| {
                        div().text_size(px(10.0)).child(SharedString::from(format!(
                            "using: {} {}",
                            self.settings.buddy_command,
                            self.settings.buddy_args.join(" ")
                        )))
                    })),
            )
    }

    pub(super) fn render_alerts_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        if self.settings.buddy_tts {
            self.load_tts_voices(cx);
        }
        let voice_label = self
            .settings
            .buddy_tts_voice
            .clone()
            .unwrap_or_else(|| "system voice".to_string());
        let rate = self.settings.buddy_tts_rate;
        let pitch = self.settings.buddy_tts_pitch;
        let voice_line = self.settings.buddy_tts.then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(72.0)))
                .child(
                    // Dropdown toggle, fixed width so the row never shifts.
                    div()
                        .id("tts-voice-toggle")
                        .cursor_pointer()
                        .flex_none()
                        .w(px(170.0))
                        .px(px(7.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(if self.tts_voice_list_open {
                            theme.ui_accent
                        } else {
                            theme.ui_border
                        }))
                        .bg(rgb(theme.ui_surface))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(theme.ui_text))
                        .hover(|style| style.border_color(rgb(theme.ui_accent)))
                        .child(SharedString::from(format!(
                            "{} voice: {voice_label}",
                            if self.tts_voice_list_open {
                                "\u{25be}"
                            } else {
                                "\u{25b8}"
                            }
                        )))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.tts_voice_list_open = !ws.tts_voice_list_open;
                                cx.notify();
                            }),
                        ),
                )
                .child(self.stepper(
                    "tts-rate",
                    format!("{rate} wpm"),
                    |ws, _window, _cx| {
                        ws.settings.buddy_tts_rate =
                            ws.settings.buddy_tts_rate.saturating_sub(10).max(80);
                        let _ = ws.settings.save();
                    },
                    |ws, _window, _cx| {
                        ws.settings.buddy_tts_rate = (ws.settings.buddy_tts_rate + 10).min(300);
                        let _ = ws.settings.save();
                    },
                    cx,
                ))
                .child(self.stepper(
                    "tts-pitch",
                    format!("pitch {pitch:.1}"),
                    |ws, _window, _cx| {
                        ws.settings.buddy_tts_pitch = (ws.settings.buddy_tts_pitch - 0.1).max(0.5);
                        let _ = ws.settings.save();
                    },
                    |ws, _window, _cx| {
                        ws.settings.buddy_tts_pitch = (ws.settings.buddy_tts_pitch + 0.1).min(2.0);
                        let _ = ws.settings.save();
                    },
                    cx,
                ))
                .child(self.chip_button(
                    "preview",
                    false,
                    |ws, _window, _cx| {
                        ws.speak_note("Hello! This is your buddy's voice.");
                    },
                    cx,
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("alerts"))
                    .child(self.chip_button(
                        if self.settings.audio_cues {
                            "cues: on"
                        } else {
                            "cues: off"
                        },
                        self.settings.audio_cues,
                        |ws, _window, cx| {
                            ws.settings.audio_cues = !ws.settings.audio_cues;
                            let _ = ws.settings.save();
                            // Gates keep sampling while cues are off (bells
                            // are drained and discarded), so re-enabling can
                            // never chime retroactively — just confirm.
                            if ws.settings.audio_cues {
                                if let Some(child) = play_sound("Glass") {
                                    ws.audio_children.push(child);
                                }
                            }
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(self.chip_button(
                        if self.settings.buddy_tts {
                            "buddy voice: on"
                        } else {
                            "buddy voice: off"
                        },
                        self.settings.buddy_tts,
                        |ws, _window, cx| {
                            ws.settings.buddy_tts = !ws.settings.buddy_tts;
                            let _ = ws.settings.save();
                            if ws.settings.buddy_tts {
                                ws.speak_note("buddy voice on");
                            } else if let Some(mut child) = ws.tts_child.take() {
                                let _ = child.kill();
                                let _ = child.wait(); // reap
                            }
                            cx.notify();
                        },
                        cx,
                    )),
            )
            .child(self.hint(hints::CUES))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("adapters"))
                    .child(self.chip_button(
                        if self.settings.tool_adapters {
                            "tool adapters: on"
                        } else {
                            "tool adapters: off"
                        },
                        self.settings.tool_adapters,
                        |ws, _window, cx| {
                            ws.settings.tool_adapters = !ws.settings.tool_adapters;
                            let _ = ws.settings.save();
                            // New terminals only — a live shell keeps its PATH.
                            crate::term_session::set_tool_adapters(ws.settings.tool_adapters);
                            cx.notify();
                        },
                        cx,
                    )),
            )
            .child(self.hint(hints::TOOL_ADAPTERS))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("awake"))
                    .child(self.chip_button(
                        if self.settings.auto_caffeinate {
                            "auto: on"
                        } else {
                            "auto: off"
                        },
                        self.settings.auto_caffeinate,
                        |ws, _window, cx| {
                            ws.settings.auto_caffeinate = !ws.settings.auto_caffeinate;
                            let _ = ws.settings.save();
                            cx.notify();
                        },
                        cx,
                    )),
            )
            .child(self.hint(hints::AWAKE))
            .children(voice_line)
            .children(self.render_voice_list(cx))
    }

    /// Scrollable dropdown list of installed voices (below the toggle).
    pub(super) fn render_voice_list(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !(self.settings.buddy_tts && self.tts_voice_list_open) {
            return None;
        }
        let theme = self.theme;
        let selected = self.settings.buddy_tts_voice.clone();
        let mut entries: Vec<(Option<String>, String)> = vec![(None, "system voice".to_string())];
        if let Some(voices) = &self.tts_voices {
            for voice in voices {
                entries.push((Some(voice.clone()), voice.clone()));
            }
        }
        Some(
            div()
                .flex()
                .flex_row()
                .child(div().w(px(72.0)).flex_none())
                .child(
                    div()
                        .id("tts-voice-list")
                        .w(px(240.0))
                        .max_h(px(150.0))
                        .overflow_y_scroll()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(theme.ui_border))
                        .bg(rgb(theme.ui_surface))
                        .flex()
                        .flex_col()
                        .children(entries.into_iter().enumerate().map(
                            |(index, (value, label))| {
                                let active = selected == value;
                                div()
                                    .id(index)
                                    .cursor_pointer()
                                    .px(px(8.0))
                                    .h(px(20.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .text_color(rgb(if active {
                                        theme.ui_accent
                                    } else {
                                        theme.ui_text
                                    }))
                                    .when(active, |d| d.bg(rgb(theme.ui_background)))
                                    .hover(|style| style.bg(rgb(theme.ui_border)))
                                    .child(SharedString::from(label))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, _, cx| {
                                            ws.settings.buddy_tts_voice = value.clone();
                                            let _ = ws.settings.save();
                                            ws.tts_voice_list_open = false;
                                            ws.speak_note("Hello! This is your buddy's voice.");
                                            cx.notify();
                                        }),
                                    )
                            },
                        )),
                ),
        )
    }

    /// Where a peer connects: the same base URL + fragment-secret scheme
    /// the phone bookmark uses, so `qr::matrix` and the eventual peer
    /// client need no second protocol. `None` while the companion is off —
    /// there is no live server for the other Mac to reach yet.
    fn peer_pairing_url(&self, secret: &str) -> Option<String> {
        let handle = self.companion_server.as_ref()?;
        Some(format!("{}#{secret}", handle.url))
    }

    /// Smaller sibling of the phone flyout's QR paint (`companion_ui`'s
    /// `render_companion_flyout`) for the pairing panel's tighter width.
    fn render_peer_qr(&self, url: &str) -> Option<impl IntoElement> {
        let (modules, size) = crate::companion::qr::matrix(url)?;
        Some(
            div().flex().justify_center().child(
                div()
                    .w(px(140.0))
                    .h(px(140.0))
                    .rounded(px(4.0))
                    .bg(gpui::white())
                    .child(
                        gpui::canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let side = f32::from(bounds.size.width);
                                let cell = side / (size + 8) as f32;
                                let snap = |n: usize| ((n + 4) as f32 * cell).round();
                                for row in 0..size {
                                    for col in 0..size {
                                        if !modules[row * size + col] {
                                            continue;
                                        }
                                        let (x0, x1) = (snap(col), snap(col + 1));
                                        let (y0, y1) = (snap(row), snap(row + 1));
                                        window.paint_quad(gpui::fill(
                                            gpui::Bounds::new(
                                                gpui::point(
                                                    bounds.origin.x + px(x0),
                                                    bounds.origin.y + px(y0),
                                                ),
                                                gpui::size(px(x1 - x0), px(y1 - y0)),
                                            ),
                                            gpui::black(),
                                        ));
                                    }
                                }
                            },
                        )
                        .size_full(),
                    ),
            ),
        )
    }

    /// Kick off a tailnet scan in the background — shelling `tailscale
    /// status --json` blocks on I/O, so it must never run on the UI
    /// thread. Re-entrant clicks (or the one-shot auto-scan racing a
    /// manual click) are ignored while a scan is already in flight.
    pub(super) fn scan_peer_candidates(&mut self, cx: &mut Context<Self>) {
        if self.peer_scanning {
            return;
        }
        self.peer_scanning = true;
        cx.notify();
        cx.spawn(async move |ws, cx| {
            let found = cx
                .background_executor()
                .spawn(async { peers::scan_candidates() })
                .await;
            let _ = ws.update(cx, |ws, cx| {
                ws.peer_candidates = found;
                ws.peer_scanning = false;
                ws.peer_scanned_once = true;
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Pair a discovered candidate: fresh id, fresh secret, every grant
    /// off (`peers::pair`), written to settings and forced live if the
    /// companion is running — see `apply_peer_mutation`. An unrecognized
    /// secret on the running server would just be a pairing that silently
    /// doesn't work yet.
    pub(super) fn pair_peer(&mut self, host: &str, cx: &mut Context<Self>) {
        let (mut current, _problems) = self.settings.peers();
        let record = peers::pair(host);
        self.peer_pairing_secret = Some((
            record.id.clone(),
            record.label.clone(),
            record.secret.clone(),
        ));
        current.push(record);
        self.apply_peer_mutation(current, cx);
    }

    /// Delete a peer — the revocation. Forced live immediately, never left
    /// to the next manual companion toggle: see `apply_peer_mutation`.
    pub(super) fn delete_peer(&mut self, id: &PeerId, cx: &mut Context<Self>) {
        let (mut current, _problems) = self.settings.peers();
        current.retain(|peer| &peer.id != id);
        // Pairing deliberately allows recreating a deleted peer WITH THE
        // SAME id for identity recovery, so without this a recreated peer
        // would silently inherit shares granted to its predecessor. Must
        // run before `apply_peer_mutation` below: a forced restart rebuilds
        // the hub by replaying `self.broadcasts`, so this has to be pruned
        // first or the dead peer's visibility would be replayed right back.
        self.broadcasts.forget_peer(id);
        // A just-shown pairing secret for the peer being deleted must not
        // linger onscreen as if it still meant something. Compared by id,
        // not label — labels are user-editable and not unique (two peers
        // can share one), while ids are opaque and quarantined on
        // collision.
        if self
            .peer_pairing_secret
            .as_ref()
            .is_some_and(|(pid, _, _)| pid == id)
        {
            self.peer_pairing_secret = None;
        }
        self.apply_peer_mutation(current, cx);
    }

    /// Flip one grant on one peer. Narrowing a grant is a partial
    /// revocation, so it goes through the same forced-restart path as
    /// deletion — see `apply_peer_mutation`.
    pub(super) fn toggle_peer_grant(
        &mut self,
        id: &PeerId,
        which: peers::GrantKind,
        cx: &mut Context<Self>,
    ) {
        let (mut current, _problems) = self.settings.peers();
        for peer in &mut current {
            if &peer.id == id {
                peer.grants = peers::toggled_grants(peer.grants, which);
            }
        }
        self.apply_peer_mutation(current, cx);
    }

    pub(super) fn dismiss_peer_pairing_secret(&mut self, cx: &mut Context<Self>) {
        self.peer_pairing_secret = None;
        cx.notify();
    }

    /// Write a peer-list mutation and, when it changes what the running
    /// companion would authorize, force it through stop+restart so the new
    /// snapshot is live immediately — `regenerate_companion_token`'s
    /// pattern applied to every peer mutation. `ServerConfig.peers` is
    /// resolved once at server start and never refreshed on its own (see
    /// `companion_ui::toggle_companion`), so without this a deleted peer,
    /// or one whose grant was just narrowed, would keep its OLD authority
    /// live until the server was next toggled by hand — possibly the
    /// whole app session. The decision itself is
    /// `peers::peer_mutation_requires_restart`, a pure predicate tested in
    /// `peers.rs`; this only carries it out.
    ///
    /// Also the one place `shareable_peers_cache` is refreshed after
    /// startup — this is the only path that ever writes `settings.peers`,
    /// so it is the only path that can leave that cache stale.
    fn apply_peer_mutation(&mut self, updated: Vec<peers::PeerRecord>, cx: &mut Context<Self>) {
        let (before, _problems) = self.settings.peers();
        let restart = peers::peer_mutation_requires_restart(&before, &updated);
        let restart_addr = if restart && self.companion_server.is_some() {
            self.stop_companion_for_restart(cx)
        } else {
            None
        };
        self.shareable_peers_cache = peers::shareable_peers(&updated)
            .into_iter()
            .cloned()
            .collect();
        self.settings.peers = serde_json::to_value(&updated).unwrap_or(serde_json::Value::Null);
        let _ = self.settings.save();
        if let Some(addr) = restart_addr {
            self.restart_companion_pinned(cx, addr);
        }
        cx.notify();
    }

    /// Tailnet peers: discovered candidates plus already-paired peers with
    /// their per-grant toggles and a delete action. The settings sheet
    /// auto-scans once per session (`peer_scanned_once` guards against
    /// re-triggering on every render of an empty result); after that,
    /// scanning is a manual click.
    pub(super) fn render_peers_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.peer_scanned_once && !self.peer_scanning {
            self.scan_peer_candidates(cx);
        }
        let theme = self.theme;
        let (paired, _problems) = self.settings.peers();
        let offerable = peers::offerable_candidates(&self.peer_candidates, &paired);
        let scanning = self.peer_scanning;

        let candidate_rows: Vec<_> = offerable
            .into_iter()
            .map(|candidate| {
                let host = candidate.host.clone();
                let pair_host = host.clone();
                div()
                    .id(SharedString::from(format!("peer-candidate-{host}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .py(px(2.0))
                    .child(
                        div()
                            .flex_grow()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.ui_text))
                            .child(SharedString::from(format!(
                                "{}  \u{b7}  {}",
                                candidate.host, candidate.addr
                            ))),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("peer-pair-{host}")))
                            .cursor_pointer()
                            .px(px(7.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(theme.ui_border))
                            .bg(rgb(theme.ui_surface))
                            .text_color(rgb(theme.ui_text))
                            .hover(|style| {
                                style
                                    .border_color(rgb(theme.ui_accent))
                                    .bg(rgb(theme.ui_border))
                            })
                            .child("pair")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| ws.pair_peer(&pair_host, cx)),
                            ),
                    )
            })
            .collect();
        let candidates_empty = candidate_rows.is_empty();

        let peer_rows: Vec<_> = paired
            .iter()
            .map(|peer| {
                let label = peer.label.clone();
                let grants = peer.grants;
                let view_id = peer.id.clone();
                let type_id = peer.id.clone();
                let spawn_id = peer.id.clone();
                let delete_id = peer.id.clone();
                let grant_chip = |tag: &'static str, on: bool| {
                    div()
                        .px(px(7.0))
                        .py(px(1.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(if on { theme.ui_accent } else { theme.ui_border }))
                        .bg(rgb(theme.ui_surface))
                        .text_color(rgb(if on { theme.ui_accent } else { theme.ui_text }))
                        .child(SharedString::from(format!(
                            "{tag}: {}",
                            if on { "on" } else { "off" }
                        )))
                };
                div()
                    .id(SharedString::from(format!("peer-{}", peer.id.0)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .py(px(2.0))
                    .child(
                        div()
                            .flex_grow()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.ui_text))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        grant_chip("view", grants.view)
                            .id(SharedString::from(format!("peer-grant-view-{}", peer.id.0)))
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .border_color(rgb(theme.ui_accent))
                                    .bg(rgb(theme.ui_border))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.toggle_peer_grant(&view_id, peers::GrantKind::View, cx)
                                }),
                            ),
                    )
                    .child(
                        grant_chip("type", grants.type_)
                            .id(SharedString::from(format!("peer-grant-type-{}", peer.id.0)))
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .border_color(rgb(theme.ui_accent))
                                    .bg(rgb(theme.ui_border))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.toggle_peer_grant(&type_id, peers::GrantKind::Type, cx)
                                }),
                            ),
                    )
                    .child(
                        grant_chip("spawn", grants.spawn)
                            .id(SharedString::from(format!(
                                "peer-grant-spawn-{}",
                                peer.id.0
                            )))
                            .cursor_pointer()
                            .hover(|style| {
                                style
                                    .border_color(rgb(theme.ui_accent))
                                    .bg(rgb(theme.ui_border))
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.toggle_peer_grant(&spawn_id, peers::GrantKind::Spawn, cx)
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("peer-delete-{}", peer.id.0)))
                            .cursor_pointer()
                            .px(px(5.0))
                            .rounded(px(3.0))
                            .opacity(0.6)
                            .hover(|style| style.opacity(1.0).bg(rgb(theme.ui_surface)))
                            .text_color(rgb(theme.red))
                            .child("x")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| ws.delete_peer(&delete_id, cx)),
                            ),
                    )
            })
            .collect();
        let peers_empty = peer_rows.is_empty();

        let pairing_panel = self
            .peer_pairing_secret
            .clone()
            .map(|(_id, label, secret)| {
                let url = self.peer_pairing_url(&secret);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .p(px(8.0))
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(theme.ui_accent))
                    .bg(rgb(theme.ui_surface))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.ui_text))
                            .child(SharedString::from(format!(
                                "Paired {label} \u{2014} scan on that Mac to finish:"
                            ))),
                    )
                    .children(url.as_deref().and_then(|url| self.render_peer_qr(url)))
                    .children((url.is_none()).then(|| {
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child("Start the phone link to get a scannable code.")
                    }))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(format!("secret: {secret}"))),
                    )
                    .child(self.chip_button(
                        "done",
                        false,
                        |ws, _window, cx| ws.dismiss_peer_pairing_secret(cx),
                        cx,
                    ))
            });

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(div().w(px(72.0)).child("candidates"))
                    .child(self.chip_button(
                        if scanning { "scanning" } else { "rescan" },
                        scanning,
                        |ws, _window, cx| ws.scan_peer_candidates(cx),
                        cx,
                    )),
            )
            .child(self.hint(hints::PEER_CANDIDATES))
            .children(
                candidates_empty.then(|| self.hint("No other Macs found on the tailnet yet.")),
            )
            .children(candidate_rows)
            .child(self.group_label("paired"))
            .children(peers_empty.then(|| self.hint("No peers paired yet.")))
            .children(peer_rows)
            .child(self.hint(hints::PAIRED_PEERS))
            .children(pairing_panel)
    }
}

#[cfg(test)]
mod tests {
    use super::hints;

    #[test]
    fn hints_stay_within_the_text_budget() {
        for hint in hints::ALL {
            assert!(!hint.trim().is_empty(), "an empty hint explains nothing");
            assert!(
                hint.len() <= hints::MAX_LEN,
                "hint is too long to read as a caption ({} chars): {hint}",
                hint.len()
            );
        }
    }

    #[test]
    fn hints_carry_no_emoji() {
        // This UI uses SVG icons, never emoji.
        for hint in hints::ALL {
            for ch in hint.chars() {
                assert!(
                    ch.is_ascii() || hints::ALLOWED_NON_ASCII.contains(&ch),
                    "non-ASCII {ch:?} in hint: {hint}"
                );
            }
        }
    }

    #[test]
    fn hints_read_as_sentences() {
        for hint in hints::ALL {
            let first = hint.chars().next().unwrap();
            assert!(first.is_uppercase(), "hint should open a sentence: {hint}");
            assert!(hint.ends_with('.'), "hint should close a sentence: {hint}");
        }
    }
}
