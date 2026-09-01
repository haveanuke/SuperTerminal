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

    /// Rebuild the hub from the current panes and resolve the preview
    /// store/thumbnailer a new companion server will use. Shared by a cold
    /// [`Self::toggle_companion`] and a pinned
    /// [`Self::restart_companion_pinned`] — they differ only in which
    /// port(s) they are willing to bind afterward.
    fn prepare_companion_hub(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (
        Arc<crate::companion::hub::Hub>,
        Arc<crate::companion::previews::PreviewStore>,
        Arc<crate::companion::thumbs::Thumbnailer>,
    ) {
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
        // Re-apply share state recorded before this (freshly built) hub
        // existed. MUST run after the registration loop above:
        // `set_visible_to` only mutates an entry that is already
        // registered, so replaying before registration would find no
        // entry and silently do nothing — see
        // `companion::hub::CompanionHub::set_visible_to`. Skips any id no
        // longer in `self.panes`; that should already be pruned (see
        // `Workspace::close_terminal`/`close_tab`/`load_session`), but a
        // freshly-built hub is exactly the place a stale entry would first
        // do visible harm, so this checks rather than trusts.
        for (id, peers) in self.broadcasts.iter() {
            if !self.panes.contains_key(id) {
                continue;
            }
            for peer in peers {
                hub.set_visible_to(id, peer, true);
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
        (hub, previews, thumbs)
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
        let (hub, previews, thumbs) = self.prepare_companion_hub(cx);
        // Resolved ONCE here and frozen for the server's lifetime. Nothing
        // refreshes it on its own — a peer edited or deleted after this
        // point would keep its old grants until the server is next
        // toggled by hand, possibly the whole app session, if nothing else
        // intervened. Nothing else DOES leave it to chance: every peer
        // mutation goes through `Workspace::apply_peer_mutation`, which
        // forces a stop+restart immediately (via
        // `stop_companion_for_restart`) whenever the mutation changes what
        // the running companion would authorize — the same pattern
        // `regenerate_companion_token` uses for a rotated token. If a
        // future reader is checking whether revocation actually works,
        // `apply_peer_mutation` in `settings_ui.rs` is where that lives.
        let (peers, _peer_problems) = self.settings.peers();
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
                    peers: peers.clone(),
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
    ///
    /// This is the plain "turn the companion off" path — nothing rebinds
    /// right after. A caller that is about to immediately restart the
    /// server on the same port (token regeneration, a forced restart after
    /// a peer-grant edit) must use [`Self::stop_companion_for_restart`]
    /// instead: firing the port release off-thread here would let the
    /// rebind race the old listener's drop.
    pub(super) fn stop_companion(&mut self, cx: &mut Context<Self>) {
        self.stop_companion_inner(cx, false);
    }

    /// Same teardown as [`Self::stop_companion`], but waits — with a bound,
    /// never the UI thread hanging forever — for the old listener's port to
    /// actually free up (see
    /// [`crate::companion::server::ServerHandle::stop_blocking_on_port_release`]).
    /// Every caller that stops the companion only to immediately restart it
    /// on the same address must go through this, or the rebind can lose
    /// the race with the old listener's drop and silently break the
    /// phone's saved bookmark (it encodes host:port).
    ///
    /// Returns the address that was bound, for [`Self::restart_companion_pinned`]
    /// to rebind on — the caller MUST pin the restart to exactly this
    /// address rather than searching the port range the way
    /// [`Self::toggle_companion`] does: the wait above is bounded and can
    /// return without the port being confirmed free, and searching onward
    /// in that case is exactly the silent-move failure mode this exists to
    /// avoid. `None` means nothing was running, so there is nothing to
    /// restart.
    pub(super) fn stop_companion_for_restart(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<std::net::SocketAddr> {
        self.stop_companion_inner(cx, true)
    }

    fn stop_companion_inner(
        &mut self,
        cx: &mut Context<Self>,
        block_for_port: bool,
    ) -> Option<std::net::SocketAddr> {
        // The link this state vouched for is dying (stop, or a regenerate
        // that revokes the token) — "copied" must not outlive it.
        self.companion_copied = false;
        self.companion_copy_gen += 1;
        for pane in self.panes.values() {
            pane.update(cx, |pane, _| pane.set_companion(None));
        }
        self.companion_hub = None;
        self.companion_previews = None;
        let handle = self.companion_server.take()?;
        let addr = handle.addr();
        handle.cancel();
        if block_for_port {
            // The bound wait's return only distinguishes "confirmed free"
            // from "unconfirmed — may still be bound"; either way,
            // `restart_companion_pinned` makes exactly one attempt at
            // `addr` and reports a visible error instead of guessing at a
            // different port.
            let _ = handle.stop_blocking_on_port_release();
            Some(addr)
        } else {
            std::thread::spawn(move || handle.stop());
            None
        }
    }

    /// Rebind the companion on EXACTLY `addr` after a forced restart (token
    /// rotation, a peer revoked or narrowed). Unlike a cold
    /// [`Self::toggle_companion`], this never searches `43110..43121`:
    /// falling through to the next port after losing the old listener's
    /// release race would silently move the server out from under the
    /// phone's saved bookmark, which encodes host:port. A visible error the
    /// user can retry is the safe failure mode; a server that quietly moved
    /// is not.
    pub(super) fn restart_companion_pinned(
        &mut self,
        cx: &mut Context<Self>,
        addr: std::net::SocketAddr,
    ) {
        use crate::companion::server;
        // No token means nothing could have been serving it either — there
        // is nothing to restart.
        let Some(token) = self.settings.companion_token.clone() else {
            return;
        };
        let (hub, previews, thumbs) = self.prepare_companion_hub(cx);
        let (peers, _peer_problems) = self.settings.peers();
        match server::start(
            Arc::clone(&hub),
            self.theme,
            server::ServerConfig {
                bind: addr,
                token,
                page: include_str!("../companion/page.html"),
                previews: Arc::clone(&previews),
                thumbs,
                peers,
            },
        ) {
            Ok(handle) => {
                self.companion_error = None;
                self.companion_hub = Some(hub);
                self.companion_server = Some(handle);
                self.companion_previews = Some(previews);
            }
            Err(_) => {
                for pane in self.panes.values() {
                    pane.update(cx, |pane, _| pane.set_companion(None));
                }
                self.companion_error = Some(format!(
                    "companion port {} did not free up after the change — phone link is OFF; reopen it from the rail to retry",
                    addr.port()
                ));
            }
        }
        cx.notify();
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
        let restart_addr = if self.companion_server.is_some() {
            self.stop_companion_for_restart(cx)
        } else {
            None
        };
        self.settings.companion_token = Some(crate::companion::auth::generate_token());
        let _ = self.settings.save();
        if let Some(addr) = restart_addr {
            self.restart_companion_pinned(cx, addr);
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
