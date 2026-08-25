//! Settings-sheet section renderers and appearance mutators, split from
//! the workspace (same module tree: private Workspace fields stay
//! reachable). Pure move — behavior identical.

use gpui::{div, px, rgb, Context, MouseButton, SharedString, Window};

use super::*;

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
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .text_color(rgb(theme.ui_text_muted))
            .child(div().w(px(72.0)).child("previews"))
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
            }))
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
            ))
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
                    ))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .child("terminal done: Glass - awaiting input: Ping"),
                    ),
            )
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
                    ))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .child("hold the Mac awake while a terminal is working"),
                    ),
            )
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
}
