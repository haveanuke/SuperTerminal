//! Phone-companion lifecycle and UI glue (server start/stop, link
//! flyout, preview-folder plumbing), split from the workspace. Pure
//! move — behavior identical.

use gpui::{div, px, rgb, Context, MouseButton, SharedString};
use std::sync::Arc;

use super::*;

impl Workspace {
    /// Tab label for a terminal id ("label · n" when the tab holds several).
    pub(super) fn companion_label_for(&self, terminal_id: &str) -> String {
        for tab in &self.tabs {
            let ids = tab.all_terminal_ids();
            if let Some(position) = ids.iter().position(|id| id == terminal_id) {
                return if ids.len() == 1 {
                    tab.label.clone()
                } else {
                    format!("{} · {}", tab.label, position + 1)
                };
            }
        }
        terminal_id.to_string()
    }

    pub(super) fn companion_url(&self) -> Option<String> {
        let handle = self.companion_server.as_ref()?;
        let token = self.settings.companion_token.as_deref()?;
        Some(format!("{}#{token}", handle.url))
    }

    /// Copy the full URL (with token) for the one-time phone bookmark,
    /// flipping the copy chip to "copied" for a beat so the click visibly
    /// landed.
    pub(super) fn copy_companion_url(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self.companion_url() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
        self.companion_copied = true;
        self.companion_copy_gen += 1;
        let generation = self.companion_copy_gen;
        cx.notify();
        cx.spawn(async move |ws, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1500))
                .await;
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                // Only clear our own copy's confirmation: a newer copy (or a
                // revocation) already moved the generation on.
                if ws.companion_copy_gen == generation {
                    ws.companion_copied = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn toggle_companion(&mut self, cx: &mut Context<Self>) {
        use crate::companion::{net, server};
        if self.companion_server.is_some() {
            self.stop_companion(cx);
            cx.notify();
            return;
        }
        self.companion_error = None;
        let token = match &self.settings.companion_token {
            Some(token) => token.clone(),
            None => {
                let token = crate::companion::auth::generate_token();
                self.settings.companion_token = Some(token.clone());
                let _ = self.settings.save();
                token
            }
        };
        let Some(ip) = net::tailnet_ipv4() else {
            self.companion_error =
                Some("no Tailscale interface found — is Tailscale on?".to_string());
            cx.notify();
            return;
        };
        let hub = Arc::new(crate::companion::hub::Hub::new());
        let ids: Vec<String> = self.panes.keys().cloned().collect();
        for id in ids {
            let label = self.companion_label_for(&id);
            if let Some(pane) = self.panes.get(&id) {
                pane.update(cx, |pane, _| {
                    if let Some(sender) = pane.input_sender() {
                        // Origin is stated explicitly, never inferred from
                        // "has a sender": a Phase C attached pane will also
                        // have one (it forwards keystrokes to its peer), and
                        // treating that as publishable would re-publish a
                        // remote view, producing a remote view of a remote
                        // view. Every pane here is a local PTY today.
                        hub.register_with_origin(
                            &pane.id,
                            &label,
                            sender,
                            crate::companion::hub::Origin::LocalPty,
                        );
                    }
                    pane.set_companion(Some(Arc::clone(&hub)));
                });
            }
        }
        hub.bump_generation();
        let previews = Arc::new(crate::companion::previews::PreviewStore::new(
            crate::settings::prepare_preview_dir(&self.settings),
        ));
        previews.set_viewport_enabled(self.settings.blender_viewport);
        let cache_dir = crate::companion::thumbs::default_cache_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("st-thumbs"));
        // The bridge gates itself on the setting + phone demand and dies
        // with the store, so spawning unconditionally is free when off.
        crate::companion::blender::spawn(Arc::downgrade(&previews), cache_dir.clone());
        let thumbs = crate::companion::thumbs::Thumbnailer::new(cache_dir);
        let mut started = None;
        for port in 43110..43121u16 {
            match server::start(
                Arc::clone(&hub),
                self.theme,
                server::ServerConfig {
                    bind: std::net::SocketAddr::from((ip, port)),
                    token: token.clone(),
                    page: include_str!("../companion/page.html"),
                    previews: Arc::clone(&previews),
                    thumbs: Arc::clone(&thumbs),
                },
            ) {
                Ok(handle) => {
                    started = Some(handle);
                    break;
                }
                Err(_) => continue,
            }
        }
        match started {
            Some(handle) => {
                self.companion_hub = Some(hub);
                self.companion_server = Some(handle);
                self.companion_previews = Some(previews);
            }
            None => {
                for pane in self.panes.values() {
                    pane.update(cx, |pane, _| pane.set_companion(None));
                }
                self.companion_error = Some(format!("could not bind {ip} (ports 43110-43120)"));
            }
        }
        cx.notify();
    }

    /// Cancellation is flipped SYNCHRONOUSLY (streams start dying before any
    /// pane teardown that follows); the joins happen off the UI thread (a
    /// worker mid-read can take its full deadline; the UI must not wait).
    pub(super) fn stop_companion(&mut self, cx: &mut Context<Self>) {
        // The link this state vouched for is dying (stop, or a regenerate
        // that revokes the token) — "copied" must not outlive it.
        self.companion_copied = false;
        self.companion_copy_gen += 1;
        for pane in self.panes.values() {
            pane.update(cx, |pane, _| pane.set_companion(None));
        }
        self.companion_hub = None;
        self.companion_previews = None;
        if let Some(handle) = self.companion_server.take() {
            handle.cancel();
            std::thread::spawn(move || handle.stop());
        }
    }

    /// Point the live catalog at the (re)configured folder; no-op while the
    /// server is down (the next start resolves the setting itself).
    pub(super) fn apply_preview_dir(&self) {
        if let Some(store) = &self.companion_previews {
            store.set_dir(crate::settings::prepare_preview_dir(&self.settings));
        }
    }

    /// Native directory picker, same osascript pattern as the background
    /// image picker — no dependencies, run off the UI thread.
    pub(super) fn pick_preview_dir(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose folder with prompt \"Preview folder\")",
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
                    ws.settings.preview_dir = Some(path);
                    let _ = ws.settings.save();
                    ws.apply_preview_dir();
                    cx.notify();
                });
            }
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Regenerate the capability token: old bookmarks die, the server
    /// restarts with the new link.
    pub(super) fn regenerate_companion_token(&mut self, cx: &mut Context<Self>) {
        let was_running = self.companion_server.is_some();
        if was_running {
            self.stop_companion(cx);
        }
        self.settings.companion_token = Some(crate::companion::auth::generate_token());
        let _ = self.settings.save();
        if was_running {
            self.toggle_companion(cx);
        }
        cx.notify();
    }
    /// Phone-link flyout, anchored next to the rail's phone icon. Gives the
    /// link room to show in full, with an explicit copy chip — the old bar
    /// crammed a truncated URL whose click-to-copy nobody could discover.
    pub(super) fn render_companion_flyout(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.companion_flyout {
            return None;
        }
        let theme = self.theme;
        let running = self.companion_server.is_some();
        let copied = self.companion_copied;
        Some(
            div()
                .absolute()
                .left(px(40.0))
                .bottom(px(34.0))
                .w(px(320.0))
                .p(px(10.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(theme.ui_border))
                .bg(rgb(theme.ui_surface))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .text_size(px(11.0))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .child(div().text_color(rgb(theme.ui_text)).child("Phone link"))
                        .child(div().flex_grow())
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(if running {
                                    theme.ui_accent
                                } else {
                                    theme.ui_text_muted
                                }))
                                .child(if running { "serving" } else { "off" }),
                        ),
                )
                .children(self.companion_error.clone().map(|error| {
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.red))
                        .child(SharedString::from(error))
                }))
                .children(self.companion_url().map(|url| {
                    div()
                        .id("companion-url")
                        .cursor_pointer()
                        .p(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(if copied {
                            theme.ui_accent
                        } else {
                            theme.ui_border
                        }))
                        .bg(rgb(theme.ui_background))
                        .hover(|style| style.border_color(rgb(theme.ui_accent)))
                        .text_size(px(10.0))
                        .text_color(rgb(theme.ui_text))
                        .child(SharedString::from(url))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _window, cx| {
                                ws.copy_companion_url(cx);
                                cx.stop_propagation();
                            }),
                        )
                }))
                .children(
                    self.companion_url()
                        .filter(|_| running)
                        .and_then(|url| crate::companion::qr::matrix(&url))
                        .map(|(modules, size)| {
                            // Scan instead of typing a tailnet IP plus a
                            // 32-hex token on a phone keyboard. Dark modules
                            // are painted as quads in a single canvas over a
                            // white card laid out as a (size + 8)-module
                            // grid: the 4-module offset IS the spec-minimum
                            // quiet zone, at every QR version.
                            div().flex().justify_center().child(
                                div()
                                    .w(px(176.0))
                                    .h(px(176.0))
                                    .rounded(px(4.0))
                                    .bg(gpui::white())
                                    .child(
                                        gpui::canvas(
                                            |_, _, _| {},
                                            move |bounds, _, window, _| {
                                                let side = f32::from(bounds.size.width);
                                                let cell = side / (size + 8) as f32;
                                                let snap =
                                                    |n: usize| ((n + 4) as f32 * cell).round();
                                                for row in 0..size {
                                                    for col in 0..size {
                                                        if !modules[row * size + col] {
                                                            continue;
                                                        }
                                                        let (x0, x1) =
                                                            (snap(col), snap(col + 1));
                                                        let (y0, y1) =
                                                            (snap(row), snap(row + 1));
                                                        window.paint_quad(gpui::fill(
                                                            gpui::Bounds::new(
                                                                gpui::point(
                                                                    bounds.origin.x + px(x0),
                                                                    bounds.origin.y + px(y0),
                                                                ),
                                                                gpui::size(
                                                                    px(x1 - x0),
                                                                    px(y1 - y0),
                                                                ),
                                                            ),
                                                            gpui::black(),
                                                        ));
                                                    }
                                                }
                                            },
                                        )
                                        .size_full(),
                                    ),
                            )
                        }),
                )
                .children(running.then(|| {
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .child("Scan with the phone camera, or open the link on the same tailnet; bookmark it once.")
                }))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .children(running.then(|| {
                            self.chip_button(
                                if copied { "copied" } else { "copy link" },
                                copied,
                                |ws, _window, cx| ws.copy_companion_url(cx),
                                cx,
                            )
                        }))
                        .children(running.then(|| {
                            self.chip_button(
                                "new link",
                                false,
                                |ws, _window, cx| ws.regenerate_companion_token(cx),
                                cx,
                            )
                        }))
                        .child(div().flex_grow())
                        .child(self.chip_button(
                            if running { "stop" } else { "start" },
                            false,
                            |ws, _window, cx| ws.toggle_companion(cx),
                            cx,
                        )),
                )
                .into_any_element(),
        )
    }
}
