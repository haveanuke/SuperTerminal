//! Root view: tabs of split-pane terminals, the tmux-style bottom bar, and
//! the theme/sessions overlays.
//!
//! Structure follows the contract: the serializable pane tree is the
//! `layout::PaneNode` DTO (String terminal ids); live gpui entities live in a
//! side map keyed by those ids.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, px, rgb, App, Context, Entity, FocusHandle, Focusable, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Window,
};

use superterminal_core::session::SessionManager;

use crate::buddy_pet::Companion;
use crate::git_panel::GitPanel;
use crate::layout::{
    collect_terminal_ids, insert_split, remove_terminal, Layout, PaneNode, SplitDirection, Tab,
};
use crate::pane::{BroadcastHub, PaneEvent, TerminalPane};
use crate::settings::Settings;
use crate::term_session::ShutdownHandle;
use crate::text_field::{TextField, TextFieldEvent};
use crate::themes::{self, Theme};

gpui::actions!(
    superterminal,
    [
        NewTab,
        NewWindow,
        CloseFocused,
        CloseTab,
        SplitRight,
        SplitDown,
        ToggleSettingsSheet,
        ToggleSessions,
        SaveSessionAs,
        ToggleSearch,
        ToggleGitPanel,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectTab9
    ]
);

/// Split-container bounds captured by measuring canvases: (x, y, w, h).
type SplitBoundsMap = HashMap<String, (Pixels, Pixels, Pixels, Pixels)>;

/// Boxed click handler for sheet chips/steppers.
type BoxedChipHandler = Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// Which view the left sidebar shows; the rail tabs between them.
#[derive(Clone, Copy, PartialEq)]
enum SidebarView {
    Projects,
    Git,
    Files,
}

/// Sessions live in the directory SHARED with the Tauri app (contract rev 2
/// §6) — both apps read and write the same session files.
pub fn sessions_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.tomaspinal.superterminal/sessions")
}

#[derive(Clone, Copy, PartialEq)]
enum Overlay {
    None,
    SettingsSheet,
    Sessions,
    AutoRun,
    Search,
    PetCard,
}

/// An in-progress pet drag: grab offset inside the pet box, the mouse-down
/// position (to tell a click from a drag), and whether it crossed the
/// click threshold.
struct PetDrag {
    offset: (f32, f32),
    down: (f32, f32),
    moved: bool,
}

/// Busy history for one terminal, driving the "done working" cues.
struct CueState {
    busy: bool,
    busy_since: Option<std::time::Instant>,
    /// Whether the running job had already gone output-quiet (cued).
    quiet_cued: bool,
    last_cue: Option<std::time::Instant>,
}

/// One line of `say -v ?`: name (may contain spaces and suffixes like
/// "(Enhanced)"), then a locale token, then "# sample". The name is
/// everything before the locale token.
fn parse_voice_name(line: &str) -> Option<String> {
    let before_hash = line.split('#').next()?.trim_end();
    let name = before_hash
        .rsplit_once(|c: char| c.is_whitespace())
        .map(|(name, _locale)| name.trim_end())
        .unwrap_or(before_hash);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Spawn a system sound; the caller keeps the child for reaping.
fn play_sound(name: &str) -> Option<std::process::Child> {
    std::process::Command::new("/usr/bin/afplay")
        .arg(format!("/System/Library/Sounds/{name}.aiff"))
        .spawn()
        .ok()
}

struct DragState {
    tab_index: usize,
    /// The window this drag started in — resizes must never follow an
    /// active-window switch mid-drag.
    window_index: usize,
    /// Path of child indices from the tab root to the split being resized.
    path: Vec<usize>,
    direction: SplitDirection,
}

pub struct Workspace {
    tabs: Vec<Tab>,
    active_tab: usize,
    panes: HashMap<String, Entity<TerminalPane>>,
    focused_terminal: Option<String>,
    settings: Settings,
    theme: &'static Theme,
    overlay: Overlay,
    session_manager: SessionManager,
    session_names: Vec<String>,
    session_field: Option<Entity<TextField>>,
    rename_field: Option<(usize, Entity<TextField>)>,
    auto_run_field: Option<Entity<TextField>>,
    search_field: Option<Entity<TextField>>,
    buddy_field: Option<Entity<TextField>>,
    auto_run_interval: u32,
    auto_run_escape: bool,
    auto_run_escape_delay: u32,
    focus_handle: FocusHandle,
    drag: Option<DragState>,
    /// Split-container bounds (window coords) written by measuring canvases,
    /// keyed by "tab_index:path" — used for divider drag math.
    split_bounds: Arc<Mutex<SplitBoundsMap>>,
    next_id: u64,
    /// Shutdown handles for panes being torn down; joined on app quit.
    pending_shutdowns: Arc<Mutex<Vec<ShutdownHandle>>>,
    broadcast: Arc<BroadcastHub>,
    git_panel: Option<Entity<GitPanel>>,
    files_panel: Option<Entity<crate::files_panel::FilesPanel>>,
    /// Read-only file viewer docked beside the terminal tree.
    file_viewer: Option<Entity<crate::file_viewer::FileViewer>>,
    sidebar_open: bool,
    sidebar_view: SidebarView,
    /// (cwd, busy) per terminal for the projects view, refreshed on the
    /// sidebar poll — rendering must never do per-pane process queries.
    sidebar_status_cache: HashMap<String, (String, bool)>,
    /// Projects collapsed in the sidebar (by tab id).
    collapsed_projects: std::collections::HashSet<String>,
    /// Per-terminal busy tracking for audio cues.
    cue_track: HashMap<String, CueState>,
    /// The currently speaking `say` process (killed before a new note).
    tts_child: Option<std::process::Child>,
    /// Spawned afplay children, reaped on the poll (no zombies).
    audio_children: Vec<std::process::Child>,
    /// Installed `say` voices, loaded lazily for the alerts row.
    tts_voices: Option<Vec<String>>,
    tts_voices_loading: bool,
    /// Transient status line for the theme sheet (import/export results).
    theme_action_note: Option<String>,
    /// Buddy reviewer: latest note, in-flight flag, last reviewed content hash.
    buddy_note: Option<String>,
    buddy_busy: Arc<std::sync::atomic::AtomicBool>,
    buddy_last_hash: u64,
    /// After a failed run, hold off retries until this instant so a broken
    /// command doesn't respawn every tick.
    buddy_backoff_until: Option<std::time::Instant>,
    /// The pet: visual personality only — its bubble text is reviewer output.
    companion: Companion,
    pet_frame: usize,
    pet_blink: bool,
    /// Hop animation countdown (300ms ticks) after being petted.
    pet_hop: u8,
    pet_tick_count: u32,
    pet_bubble: Option<(String, std::time::Instant)>,
    pet_drag: Option<PetDrag>,
    /// Runtime position (window coords); None = default corner.
    pet_pos: Option<(f32, f32)>,
    pet_name_field: Option<Entity<TextField>>,
    /// Two-click confirm for re-roll (it permanently replaces the pet).
    pet_reroll_armed: bool,
    /// Debounce for pet-count persistence: rapid petting must not write
    /// settings to disk on every click.
    pet_save_at: Option<std::time::Instant>,
    /// The pet card remembers when it was opened from the theme sheet so
    /// closing it steps BACK there instead of dropping every sheet.
    pet_card_from_theme: bool,
    /// Blur-commit for the tab rename arms only once the field has been
    /// OBSERVED focused — protects against a focus race on creation without
    /// ever stealing focus back from the user.
    rename_blur_armed: bool,
    /// Renders survived while waiting for that first observed focus; the
    /// rename dismisses quietly if focus never arrives.
    rename_grace: u8,
    /// Swap mode: the pane waiting to trade places, if any.
    swap_source: Option<String>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        for custom in &settings.custom_themes {
            let _ = themes::import_custom(custom);
        }
        let theme = themes::by_name(&settings.theme).unwrap_or_else(themes::default_theme);
        let companion = settings
            .buddy_companion
            .clone()
            .map(Companion::from_save)
            .unwrap_or_else(Companion::hatch);
        let pet_pos = settings.buddy_pet_pos;
        let mut this = Self {
            tabs: Vec::new(),
            active_tab: 0,
            panes: HashMap::new(),
            focused_terminal: None,
            settings,
            theme,
            overlay: Overlay::None,
            session_manager: SessionManager::new(sessions_dir()),
            session_names: Vec::new(),
            session_field: None,
            rename_field: None,
            auto_run_field: None,
            search_field: None,
            buddy_field: None,
            auto_run_interval: 5,
            auto_run_escape: false,
            auto_run_escape_delay: 2,
            focus_handle: cx.focus_handle(),
            drag: None,
            split_bounds: Arc::new(Mutex::new(HashMap::new())),
            next_id: 1,
            pending_shutdowns: Arc::new(Mutex::new(Vec::new())),
            broadcast: Arc::new(BroadcastHub::default()),
            git_panel: None,
            files_panel: None,
            file_viewer: None,
            sidebar_open: true,
            sidebar_view: SidebarView::Projects,
            sidebar_status_cache: HashMap::new(),
            collapsed_projects: std::collections::HashSet::new(),
            cue_track: HashMap::new(),
            tts_child: None,
            audio_children: Vec::new(),
            tts_voices: None,
            tts_voices_loading: false,
            theme_action_note: None,
            buddy_note: None,
            buddy_busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            buddy_last_hash: 0,
            buddy_backoff_until: None,
            companion,
            pet_frame: 0,
            pet_blink: false,
            pet_hop: 0,
            pet_tick_count: 0,
            pet_bubble: None,
            pet_drag: None,
            pet_pos,
            pet_name_field: None,
            pet_reroll_armed: false,
            pet_save_at: None,
            pet_card_from_theme: false,
            rename_blur_armed: false,
            rename_grace: 0,
            swap_source: None,
        };
        // First launch (or a healed save): persist the hatched identity so
        // the same pet comes back next session.
        if this.settings.buddy_companion.as_ref() != Some(&this.companion.save) {
            this.save_companion();
        }
        this.add_tab(None, cx);
        cx.spawn(async move |ws, cx| loop {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            if ws
                .update(cx, |ws: &mut Workspace, cx| ws.buddy_tick(cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
        cx.spawn(async move |ws, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            if ws
                .update(cx, |ws: &mut Workspace, cx| ws.pet_tick(cx))
                .is_err()
            {
                break;
            }
        })
        .detach();
        this
    }

    fn save_companion(&mut self) {
        self.settings.buddy_companion = Some(self.companion.save.clone());
        let _ = self.settings.save();
    }

    /// Detect "done working" transitions and chime: a terminal that was
    /// busy 5s+ returning to the prompt plays Glass; an interactive job
    /// going output-quiet (awaiting input, e.g. claude finishing) plays
    /// Ping. Quick commands never ding; per-terminal cues are spaced 5s.
    fn audio_cue_tick(&mut self, cx: &mut Context<Self>) {
        const MIN_BUSY: Duration = Duration::from_secs(5);
        const MIN_GAP: Duration = Duration::from_secs(5);
        const QUIET: Duration = Duration::from_secs(3);
        let now = std::time::Instant::now();
        // Reap finished sound players so they never linger as zombies.
        self.audio_children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
        let mut cue_kinds: Vec<&'static str> = Vec::new();
        for (id, pane) in &self.panes {
            let pane_ref = pane.read(cx);
            let busy = pane_ref.foreground_busy();
            let quiet = pane_ref.last_activity.elapsed() >= QUIET;
            let state = self.cue_track.entry(id.clone()).or_insert(CueState {
                busy,
                busy_since: busy.then_some(now),
                quiet_cued: false,
                last_cue: None,
            });
            let worked_long_enough = state
                .busy_since
                .is_some_and(|since| now.duration_since(since) >= MIN_BUSY);
            let gap_ok = state
                .last_cue
                .is_none_or(|last| now.duration_since(last) >= MIN_GAP);
            if state.busy && !busy {
                // Job finished, back at the prompt.
                if worked_long_enough && gap_ok {
                    cue_kinds.push("Glass");
                    state.last_cue = Some(now);
                }
                state.busy_since = None;
                state.quiet_cued = false;
            } else if !state.busy && busy {
                state.busy_since = Some(now);
                state.quiet_cued = false;
            } else if busy && quiet && !state.quiet_cued {
                // Interactive job stopped producing output: awaiting input.
                // Latch ONLY when the cue actually fires — latching while
                // still under MIN_BUSY would suppress the Ping forever.
                if worked_long_enough && gap_ok {
                    cue_kinds.push("Ping");
                    state.last_cue = Some(now);
                    state.quiet_cued = true;
                }
            } else if busy && !quiet {
                state.quiet_cued = false;
            }
            state.busy = busy;
        }
        let live: std::collections::HashSet<&String> = self.panes.keys().collect();
        self.cue_track.retain(|id, _| live.contains(id));
        // Each DISTINCT chime kind plays once, so every cued terminal's
        // sound is really heard even when several finish together.
        cue_kinds.dedup();
        cue_kinds.sort_unstable();
        cue_kinds.dedup();
        for kind in cue_kinds {
            if let Some(child) = play_sound(kind) {
                self.audio_children.push(child);
            }
        }
    }

    /// Speak a buddy note via macOS `say`, replacing any current speech.
    /// Voice and rate are native flags; pitch approximates the old app's
    /// multiplier through say's `[[pbas]]` embedded command (default base
    /// ~47).
    fn speak_note(&mut self, text: &str) {
        if let Some(mut child) = self.tts_child.take() {
            let _ = child.kill();
            let _ = child.wait(); // reap
        }
        // `[[` opens say's embedded-command syntax; model-authored note
        // text must not be able to smuggle rate/volume/etc commands.
        let capped: String = text
            .chars()
            .take(400)
            .collect::<String>()
            .replace("[[", "[ [");
        let mut command = std::process::Command::new("/usr/bin/say");
        if let Some(voice) = &self.settings.buddy_tts_voice {
            command.arg("-v").arg(voice);
        }
        command
            .arg("-r")
            .arg(self.settings.buddy_tts_rate.to_string());
        let pitch = self.settings.buddy_tts_pitch;
        let spoken = if (pitch - 1.0).abs() > 0.01 {
            let pbas = (47.0 * pitch).clamp(20.0, 90.0);
            format!("[[pbas {pbas:.0}]] {capped}")
        } else {
            capped
        };
        self.tts_child = command.arg(spoken).spawn().ok();
    }

    /// Load the installed voice list once (background, `say -v ?`).
    fn load_tts_voices(&mut self, cx: &mut Context<Self>) {
        if self.tts_voices.is_some() || self.tts_voices_loading {
            return;
        }
        self.tts_voices_loading = true;
        cx.spawn(async move |ws, cx| {
            let voices = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/say")
                        .args(["-v", "?"])
                        .output()
                        .ok()
                        .map(|out| {
                            String::from_utf8_lossy(&out.stdout)
                                .lines()
                                .filter_map(parse_voice_name)
                                .collect::<Vec<String>>()
                        })
                })
                .await;
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                // A failed launch stays None so a later open retries.
                ws.tts_voices = voices;
                ws.tts_voices_loading = false;
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Step the configured voice forward/backward through the installed
    /// list ("system" default sits before the first entry).
    fn step_tts_voice(&mut self, delta: i64) {
        let Some(voices) = &self.tts_voices else {
            return;
        };
        if voices.is_empty() {
            // Nothing to step through; at least allow returning to default.
            self.settings.buddy_tts_voice = None;
            let _ = self.settings.save();
            return;
        }
        let current = self
            .settings
            .buddy_tts_voice
            .as_ref()
            .and_then(|v| voices.iter().position(|name| name == v))
            .map(|i| i as i64)
            .unwrap_or(-1); // -1 = system default
        let next = (current + delta).clamp(-1, voices.len() as i64 - 1);
        self.settings.buddy_tts_voice = if next < 0 {
            None
        } else {
            Some(voices[next as usize].clone())
        };
        let _ = self.settings.save();
    }

    /// 300ms heartbeat for the pet: 900ms art frames, occasional blinks, hop
    /// decay, and speech-bubble expiry.
    fn pet_tick(&mut self, cx: &mut Context<Self>) {
        if self
            .pet_save_at
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(1))
        {
            self.pet_save_at = None;
            self.save_companion();
        }
        if self
            .pet_bubble
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(45))
        {
            self.pet_bubble = None;
            cx.notify();
        }
        self.pet_tick_count = self.pet_tick_count.wrapping_add(1);
        // Keep the sidebar following the focused terminal's cwd (a `cd`
        // changes it without any focus event) — INDEPENDENT of pet
        // visibility. The panels dedupe unchanged paths, so this is cheap.
        if self.pet_tick_count.is_multiple_of(3) && self.settings.audio_cues {
            self.audio_cue_tick(cx);
        }
        if self.sidebar_open && self.pet_tick_count.is_multiple_of(3) {
            self.push_git_cwd(cx);
            // The projects view's activity dots decay with time alone and
            // its cwd column comes from this cache — refresh both on the
            // poll while it's open, never during render.
            if self.sidebar_view == SidebarView::Projects {
                let home = std::env::var("HOME").unwrap_or_default();
                self.sidebar_status_cache = self
                    .panes
                    .iter()
                    .map(|(id, pane)| {
                        let (cwd, busy) = pane.read(cx).status();
                        let cwd = cwd.map(|cwd| cwd.replace(&home, "~")).unwrap_or_default();
                        (id.clone(), (cwd, busy))
                    })
                    .collect();
                cx.notify();
            }
        }
        if !self.settings.buddy_pet_visible {
            return;
        }
        if self.pet_hop > 0 {
            self.pet_hop -= 1;
            cx.notify();
        }
        if self.pet_tick_count.is_multiple_of(3) {
            self.pet_frame = (self.pet_frame + 1) % 3;
            // ~1-in-5 frames blink for one tick (300ms), like the old app.
            self.pet_blink = crate::buddy_pet::hash_string(&self.pet_tick_count.to_string(), 7)
                .is_multiple_of(5);
            cx.notify();
        } else if self.pet_blink {
            self.pet_blink = false;
            cx.notify();
        }
    }

    /// Buddy-as-reviewer: after a burst of terminal activity settles, send
    /// the visible output plus the repo's working-tree diff to the configured
    /// agent CLI and surface its one-line note in the bar.
    fn buddy_tick(&mut self, cx: &mut Context<Self>) {
        use std::hash::{Hash, Hasher};
        if !self.settings.buddy_enabled
            || self.settings.buddy_command.trim().is_empty()
            || self.buddy_busy.load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        else {
            return;
        };
        let pane_ref = pane.read(cx);
        let quiet = pane_ref.last_activity.elapsed() >= Duration::from_secs(3);
        if !quiet {
            return;
        }
        let text = pane_ref.visible_text();
        if text.trim().len() < 80 {
            return;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        if hash == self.buddy_last_hash {
            return;
        }
        if self
            .buddy_backoff_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        let cwd = pane_ref.cwd();
        let command = self.settings.buddy_command.clone();
        let args = self.settings.buddy_args.clone();
        let busy = Arc::clone(&self.buddy_busy);
        busy.store(true, std::sync::atomic::Ordering::Relaxed);
        cx.spawn(async move |ws, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Working-tree diff of the enclosing repo, capped.
                    let diff = cwd
                        .and_then(|cwd| {
                            superterminal_core::git::process::run_git(
                                Some(std::path::Path::new(&cwd)),
                                &["diff", "--stat", "--patch", "--no-color"],
                                false,
                            )
                            .ok()
                        })
                        .map(|out| {
                            String::from_utf8_lossy(&out.stdout)
                                .chars()
                                .take(6000)
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    let tail: String = text.chars().rev().take(2000).collect::<String>()
                        .chars().rev().collect();
                    let prompt = format!(
                        "You are a terse code reviewer embedded in a terminal. Given recent terminal output and the current working-tree git diff, reply with ONE short actionable observation (a bug, risk, or next step). No preamble.\n\nTERMINAL OUTPUT:\n{tail}\n\nWORKING DIFF:\n{diff}"
                    );
                    superterminal_core::buddy::run(superterminal_core::buddy::BuddyRequest {
                        command,
                        args,
                        prompt,
                        timeout_ms: Some(30_000),
                    })
                })
                .await;
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                ws.buddy_busy
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                if result.ok {
                    // Only a successful review consumes the content hash: a
                    // timeout or launch failure retries once the backoff ends.
                    ws.buddy_last_hash = hash;
                    ws.buddy_backoff_until = None;
                    ws.buddy_note = Some(result.text.clone());
                    if ws.settings.buddy_tts {
                        ws.speak_note(&result.text);
                    }
                    ws.pet_bubble = Some((result.text, std::time::Instant::now()));
                    cx.notify();
                } else {
                    ws.buddy_backoff_until =
                        Some(std::time::Instant::now() + Duration::from_secs(30));
                }
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Collect shutdown handles for every live pane plus any pending ones.
    /// The caller joins them OFF the UI thread with a bounded deadline.
    pub fn shutdown_all(&mut self, cx: &mut Context<Self>) -> Vec<ShutdownHandle> {
        // Flush a debounced pet-count save so quitting mid-pet loses nothing.
        if self.pet_save_at.take().is_some() {
            self.save_companion();
        }
        let mut handles: Vec<ShutdownHandle> =
            self.pending_shutdowns.lock().unwrap().drain(..).collect();
        for pane in self.panes.values() {
            if let Some(handle) = pane.update(cx, |pane, _| pane.shutdown()) {
                handles.push(handle);
            }
        }
        self.panes.clear();
        handles
    }

    fn fresh_id(&mut self) -> String {
        let id = format!("term-{}", self.next_id);
        self.next_id += 1;
        id
    }

    fn spawn_pane(
        &mut self,
        id: String,
        cwd: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        let pane_id = id.clone();
        let hub = Arc::clone(&self.broadcast);
        let pane =
            cx.new(|pane_cx| TerminalPane::new(pane_id, cwd, theme, family, size, hub, pane_cx));
        pane.update(cx, |pane, pane_cx| {
            pane.set_appearance(
                theme,
                &self.settings.font_family,
                size,
                translucent,
                pane_cx,
            )
        });
        cx.subscribe(&pane, Self::on_pane_event).detach();
        self.panes.insert(id, pane.clone());
        pane
    }

    fn on_pane_event(
        &mut self,
        pane: Entity<TerminalPane>,
        event: &PaneEvent,
        cx: &mut Context<Self>,
    ) {
        let pane_id = pane.read(cx).id.clone();
        match event {
            PaneEvent::Focused => {
                if let Some(source) = self.swap_source.take() {
                    if source != pane_id {
                        for tab in &mut self.tabs {
                            let swapped =
                                crate::layout::swap_terminals(tab.active_pane(), &source, &pane_id);
                            *tab.active_pane_mut() = swapped;
                        }
                    }
                }
                self.focused_terminal = Some(pane_id);
                self.push_git_cwd(cx);
                cx.notify();
            }
            PaneEvent::TitleChanged => cx.notify(),
            PaneEvent::Exited => {
                self.close_terminal(&pane_id, cx);
            }
        }
    }

    /// Route keyboard focus to the currently-focused terminal's pane.
    pub fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        {
            pane.read(cx).focus(window);
        }
    }

    /// A new full-pane WINDOW inside a project — no split, one shows at a
    /// time; the sidebar lists and switches them.
    fn new_window(&mut self, tab_index: usize, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        if tab_index >= self.tabs.len() {
            return;
        }
        let terminal_id = self.fresh_id();
        self.spawn_pane(terminal_id.clone(), cwd, cx);
        let tab = &mut self.tabs[tab_index];
        tab.windows.push(PaneNode::terminal(&terminal_id));
        tab.active_window = tab.windows.len() - 1;
        self.active_tab = tab_index;
        self.focused_terminal = Some(terminal_id);
        self.push_git_cwd(cx);
        cx.notify();
    }

    fn add_tab(&mut self, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        let terminal_id = self.fresh_id();
        self.spawn_pane(terminal_id.clone(), cwd, cx);
        let tab_id = format!("tab-{}", self.next_id);
        self.next_id += 1;
        self.tabs.push(Tab::single(
            tab_id,
            "terminal",
            PaneNode::terminal(&terminal_id),
        ));
        self.active_tab = self.tabs.len() - 1;
        self.focused_terminal = Some(terminal_id);
        cx.notify();
    }

    fn split_focused(&mut self, direction: SplitDirection, cx: &mut Context<Self>) {
        let Some(target) = self.focused_terminal.clone() else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        if !collect_terminal_ids(tab.active_pane()).contains(&target) {
            return;
        }
        // Split inherits the source pane's live working directory.
        let cwd = self
            .panes
            .get(&target)
            .and_then(|p| p.read(cx).cwd())
            .map(PathBuf::from);
        let new_id = self.fresh_id();
        self.spawn_pane(new_id.clone(), cwd, cx);
        let tab = &mut self.tabs[self.active_tab];
        let split = insert_split(tab.active_pane(), &target, direction, &new_id);
        *tab.active_pane_mut() = split;
        self.focused_terminal = Some(new_id);
        cx.notify();
    }

    /// Drop sidebar state for projects that no longer exist.
    fn prune_collapsed_projects(&mut self) {
        let live: std::collections::HashSet<String> =
            self.tabs.iter().map(|tab| tab.id.clone()).collect();
        self.collapsed_projects.retain(|id| live.contains(id));
    }

    /// Keep an in-progress tab rename pointing at the same tab after a tab
    /// removal shifts the indices; drop it if its own tab was closed.
    fn fix_rename_after_removal(&mut self, removed: usize) {
        if let Some((rename_index, field)) = self.rename_field.take() {
            if rename_index != removed {
                let adjusted = rename_index - usize::from(rename_index > removed);
                self.rename_field = Some((adjusted, field));
            }
        }
    }

    fn close_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        if let Some(pane) = self.panes.remove(terminal_id) {
            pane.update(cx, move |pane, _| {
                if let Some(handle) = pane.shutdown() {
                    // Reap immediately on a detached thread (bounded inside);
                    // resources release now, not at app quit.
                    std::thread::spawn(move || {
                        handle.join_with_deadline(std::time::Duration::from_secs(3))
                    });
                }
            });
        }
        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|t| t.window_of(terminal_id).is_some())
        else {
            return;
        };
        let window_index = self.tabs[tab_index]
            .window_of(terminal_id)
            .expect("position() just found it");
        match remove_terminal(&self.tabs[tab_index].windows[window_index], terminal_id) {
            Some(rest) => {
                self.tabs[tab_index].windows[window_index] = rest;
                if self.focused_terminal.as_deref() == Some(terminal_id) {
                    let tab = &self.tabs[tab_index];
                    self.focused_terminal = collect_terminal_ids(&tab.windows[window_index])
                        .into_iter()
                        .next();
                }
            }
            None if self.tabs[tab_index].windows.len() > 1 => {
                // The emptied WINDOW closes; the project lives on.
                let was_focused = self.focused_terminal.as_deref() == Some(terminal_id);
                let tab = &mut self.tabs[tab_index];
                tab.windows.remove(window_index);
                if window_index < tab.active_window {
                    tab.active_window -= 1;
                }
                tab.active_window = tab.active_window.min(tab.windows.len() - 1);
                if was_focused {
                    self.focused_terminal =
                        collect_terminal_ids(self.tabs[tab_index].active_pane())
                            .into_iter()
                            .next();
                }
            }
            None => {
                let was_active = tab_index == self.active_tab
                    || self.focused_terminal.as_deref() == Some(terminal_id);
                self.tabs.remove(tab_index);
                self.fix_rename_after_removal(tab_index);
                self.prune_collapsed_projects();
                if self.tabs.is_empty() {
                    self.add_tab(None, cx);
                } else {
                    // Removing a tab before the active one shifts every later
                    // index down; follow the shift so the same tab stays
                    // selected. Only an active-tab close moves focus.
                    if tab_index < self.active_tab {
                        self.active_tab -= 1;
                    }
                    self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                    if was_active {
                        self.focused_terminal =
                            collect_terminal_ids(self.tabs[self.active_tab].active_pane())
                                .into_iter()
                                .next();
                    }
                }
            }
        }
        cx.notify();
    }

    /// Close a whole tab: every terminal in its tree shuts down (the old
    /// app's removeTab). The last remaining tab is respawned fresh.
    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let ids = tab.all_terminal_ids();
        let was_active = index == self.active_tab
            || self
                .focused_terminal
                .as_ref()
                .is_some_and(|focused| ids.contains(focused));
        for id in ids {
            if let Some(pane) = self.panes.remove(&id) {
                pane.update(cx, move |pane, _| {
                    if let Some(handle) = pane.shutdown() {
                        std::thread::spawn(move || {
                            handle.join_with_deadline(std::time::Duration::from_secs(3))
                        });
                    }
                });
            }
        }
        self.tabs.remove(index);
        self.fix_rename_after_removal(index);
        self.prune_collapsed_projects();
        if self.tabs.is_empty() {
            self.add_tab(None, cx);
        } else {
            if index < self.active_tab {
                self.active_tab -= 1;
            }
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            if was_active {
                self.focused_terminal =
                    collect_terminal_ids(self.tabs[self.active_tab].active_pane())
                        .into_iter()
                        .next();
            }
        }
        cx.notify();
    }

    fn close_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_terminal.clone() {
            self.close_terminal(&id, cx);
        }
    }

    fn start_tab_rename(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme;
        let field = cx.new(|field_cx| TextField::new("tab name", theme, field_cx).compact());
        cx.subscribe(
            &field,
            move |ws, _field, event: &TextFieldEvent, cx| match event {
                TextFieldEvent::Submitted(name) => {
                    if let Some((idx, _)) = ws.rename_field.take() {
                        if let Some(tab) = ws.tabs.get_mut(idx) {
                            let trimmed = name.trim();
                            if !trimmed.is_empty() {
                                tab.label = trimmed.to_string();
                            }
                        }
                    }
                    cx.notify();
                }
                TextFieldEvent::Cancelled => {
                    ws.rename_field = None;
                    cx.notify();
                }
            },
        )
        .detach();
        let current = self
            .tabs
            .get(index)
            .map(|tab| tab.label.clone())
            .unwrap_or_default();
        field.update(cx, |field, field_cx| {
            field.set_text_selected(&current, field_cx)
        });
        field.read(cx).focus(window);
        self.rename_field = Some((index, field));
        self.rename_blur_armed = false;
        self.rename_grace = 0;
        cx.notify();
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() {
            self.active_tab = index;
            self.focused_terminal = collect_terminal_ids(self.tabs[index].active_pane())
                .into_iter()
                .next();
            self.push_git_cwd(cx);
            cx.notify();
        }
    }

    /// Push the current settings to every pane and persist them.
    fn apply_appearance(&mut self, cx: &mut Context<Self>) {
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

    fn apply_theme(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(theme) = themes::by_name(name) {
            self.theme = theme;
            self.settings.theme = name.to_string();
            self.apply_appearance(cx);
        }
    }

    fn set_font_family(&mut self, family: &str, cx: &mut Context<Self>) {
        self.settings.font_family = family.to_string();
        self.apply_appearance(cx);
    }

    fn set_background_opacity(&mut self, opacity: f32, cx: &mut Context<Self>) {
        self.settings.background_opacity = opacity.clamp(0.05, 1.0);
        self.apply_appearance(cx);
    }

    /// Native file picker without dependencies: osascript's choose-file,
    /// run off the UI thread.
    fn pick_background_image(&mut self, cx: &mut Context<Self>) {
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

    fn clear_background_image(&mut self, cx: &mut Context<Self>) {
        self.settings.background_image = None;
        self.apply_appearance(cx);
    }

    fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.settings.font_size = size.clamp(8.0, 32.0);
        self.apply_appearance(cx);
    }

    fn apply_auto_run(&mut self, command: String, cx: &mut Context<Self>) {
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

    /// Close whatever sheet is open. Closing search also clears the pane's
    /// highlights — every close path must go through here, not just the
    /// field's own Escape handler.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A pet card opened from the theme sheet steps back to it.
        if self.overlay == Overlay::PetCard && self.pet_card_from_theme {
            self.pet_card_from_theme = false;
            self.pet_reroll_armed = false;
            self.overlay = Overlay::SettingsSheet;
            window.focus(&self.focus_handle);
            cx.notify();
            return;
        }
        self.leave_search_highlights(cx);
        self.overlay = Overlay::None;
        self.pet_reroll_armed = false;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Clear search highlights whenever the Search sheet is being left —
    /// including sideways switches to another sheet that skip
    /// `close_overlay`.
    fn leave_search_highlights(&mut self, cx: &mut Context<Self>) {
        if self.overlay == Overlay::Search {
            if let Some(pane) = self
                .focused_terminal
                .as_ref()
                .and_then(|id| self.panes.get(id))
            {
                pane.update(cx, |pane, pane_cx| pane.set_search(None, pane_cx));
            }
        }
    }

    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay == Overlay::Search {
            self.close_overlay(window, cx);
        } else {
            self.overlay = Overlay::Search;
            cx.notify();
        }
    }

    /// The left sidebar: the activity rail is ALWAYS visible (so every view
    /// stays one click away); the active view opens beside it.
    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = self.theme;
        let view = self.sidebar_view;
        let open = self.sidebar_open;
        let rail_item = |_ws: &Self,
                         id: &'static str,
                         label: &'static str,
                         item: SidebarView,
                         cx: &mut Context<Self>| {
            let active = open && view == item;
            let color = if active {
                theme.ui_accent
            } else {
                theme.ui_text_muted
            };
            let glyph = match item {
                SidebarView::Projects => crate::icons::Icon::Projects,
                SidebarView::Git => crate::icons::Icon::GitBranch,
                SidebarView::Files => crate::icons::Icon::Files,
            };
            let _ = label;
            div()
                .id(SharedString::from(id))
                .cursor_pointer()
                .w(px(26.0))
                .h(px(26.0))
                .rounded(px(5.0))
                .flex()
                .items_center()
                .justify_center()
                .when(active, |d| d.bg(rgb(theme.ui_background)))
                .hover(|style| style.bg(rgb(theme.ui_border)))
                .child(crate::icons::icon(glyph, color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, _, window, cx| {
                        if ws.sidebar_view == item && ws.sidebar_open {
                            ws.close_sidebar(cx);
                        } else {
                            ws.open_sidebar(item, cx);
                        }
                        ws.focus_active_pane(window, cx);
                    }),
                )
        };
        let active_view: Option<gpui::AnyElement> = if self.sidebar_open {
            match self.sidebar_view {
                SidebarView::Projects => Some(self.render_projects_view(cx)),
                SidebarView::Git => self.git_panel.clone().map(|panel| panel.into_any_element()),
                SidebarView::Files => self
                    .files_panel
                    .clone()
                    .map(|panel| panel.into_any_element()),
            }
        } else {
            None
        };
        Some(
            div()
                .flex_none()
                .h_full()
                .flex()
                .flex_row()
                .child(
                    div()
                        .w(px(34.0))
                        .h_full()
                        .flex_none()
                        .bg(rgb(theme.ui_surface))
                        .border_r_1()
                        .border_color(rgb(theme.ui_border))
                        .flex()
                        .flex_col()
                        .items_center()
                        .pt(px(6.0))
                        .gap(px(4.0))
                        .child(rail_item(
                            self,
                            "rail-projects",
                            "projects",
                            SidebarView::Projects,
                            cx,
                        ))
                        .child(rail_item(self, "rail-git", "git", SidebarView::Git, cx))
                        .child(rail_item(
                            self,
                            "rail-files",
                            "files",
                            SidebarView::Files,
                            cx,
                        )),
                )
                .children(active_view),
        )
    }

    /// Orca-style projects view: every tab is a project, with quick status
    /// of the terminals inside it. Click a project to switch tabs; click a
    /// terminal to jump straight to it.
    fn render_projects_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            let active_tab = tab_index == self.active_tab;
            let terminal_ids = tab.all_terminal_ids();
            let count = terminal_ids.len();
            let project_tab_id = tab.id.clone();
            let close_tab_id = tab.id.clone();
            let label_element = if let Some((_, field)) = self
                .rename_field
                .as_ref()
                .filter(|(rename_index, _)| *rename_index == tab_index)
            {
                div().w(px(140.0)).child(field.clone()).into_any_element()
            } else {
                div()
                    .text_color(rgb(if active_tab {
                        theme.ui_accent
                    } else {
                        theme.ui_text
                    }))
                    .child(SharedString::from(tab.label.clone()))
                    .into_any_element()
            };
            let collapsed = self.collapsed_projects.contains(&tab.id);
            let collapse_tab_id = tab.id.clone();
            rows.push(
                div()
                    .id(SharedString::from(format!("project-{}", tab.id)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(24.0))
                    .px(px(8.0))
                    .cursor_pointer()
                    .when(active_tab, |d| d.bg(rgb(theme.ui_surface)))
                    .hover(|style| style.bg(rgb(theme.ui_surface)))
                    .child(
                        div()
                            .id(SharedString::from(format!("project-fold-{}", tab.id)))
                            .cursor_pointer()
                            .w(px(12.0))
                            .flex_none()
                            .text_size(px(8.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(if collapsed {
                                "\u{25b8}"
                            } else {
                                "\u{25be}"
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    cx.stop_propagation();
                                    if !ws.collapsed_projects.remove(&collapse_tab_id) {
                                        ws.collapsed_projects.insert(collapse_tab_id.clone());
                                    }
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(label_element)
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(format!(
                                "{count} terminal{}",
                                if count == 1 { "" } else { "s" }
                            ))),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .id(SharedString::from(format!("project-new-win-{}", tab.id)))
                            .cursor_pointer()
                            .px(px(3.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .hover(|style| style.text_color(rgb(theme.ui_accent)))
                            .child("+")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let new_win_tab_id = tab.id.clone();
                                    move |ws, _, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(index) =
                                            ws.tabs.iter().position(|t| t.id == new_win_tab_id)
                                        {
                                            ws.new_window(index, None, cx);
                                            ws.focus_active_pane(window, cx);
                                        }
                                    }
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("project-close-{}", tab.id)))
                            .cursor_pointer()
                            .px(px(3.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .hover(|style| style.text_color(rgb(theme.red)))
                            .child("x")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    cx.stop_propagation();
                                    if let Some(index) =
                                        ws.tabs.iter().position(|t| t.id == close_tab_id)
                                    {
                                        ws.close_tab(index, cx);
                                        ws.focus_active_pane(window, cx);
                                    }
                                }),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, event: &gpui::MouseDownEvent, window, cx| {
                            if ws.rename_field.as_ref().is_some_and(|(rename_index, _)| {
                                ws.tabs
                                    .get(*rename_index)
                                    .is_some_and(|t| t.id == project_tab_id)
                            }) {
                                return; // typing into the rename field
                            }
                            // Resolve the STABLE tab id at click time — a
                            // captured index goes stale if tabs close.
                            let Some(index) = ws.tabs.iter().position(|t| t.id == project_tab_id)
                            else {
                                return;
                            };
                            if event.click_count >= 2 {
                                ws.start_tab_rename(index, window, cx);
                                // Keep focus on the rename field past the
                                // root's click-to-focus (see tab rename).
                                window.prevent_default();
                            } else {
                                ws.select_tab(index, cx);
                                ws.focus_active_pane(window, cx);
                            }
                        }),
                    )
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            let multi_window = tab.windows.len() > 1;
            let window_groups: Vec<(usize, Vec<String>)> = tab
                .windows
                .iter()
                .enumerate()
                .map(|(window_index, tree)| (window_index, collect_terminal_ids(tree)))
                .collect();
            for (window_index, window_terminals) in window_groups {
                if multi_window {
                    let window_active = window_index == tab.active_window && active_tab;
                    let first_terminal = window_terminals.first().cloned();
                    rows.push(
                        div()
                            .id(SharedString::from(format!(
                                "project-win-{}-{window_index}",
                                tab.id
                            )))
                            .flex()
                            .flex_row()
                            .items_center()
                            .h(px(16.0))
                            .pl(px(16.0))
                            .cursor_pointer()
                            .text_size(px(8.0))
                            .text_color(rgb(if window_active {
                                theme.ui_accent
                            } else {
                                theme.ui_text_muted
                            }))
                            .hover(|style| style.bg(rgb(theme.ui_surface)))
                            .child(SharedString::from(format!("window {}", window_index + 1)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    if let Some(id) = &first_terminal {
                                        ws.focus_terminal_by_id(id, window, cx);
                                    }
                                }),
                            )
                            .into_any_element(),
                    );
                }
                for terminal_id in window_terminals {
                    let Some(pane) = self.panes.get(&terminal_id) else {
                        continue;
                    };
                    let pane_ref = pane.read(cx);
                    let focused = self.focused_terminal.as_deref() == Some(terminal_id.as_str());
                    let title = pane_ref.title();
                    let (cwd, busy) = self
                        .sidebar_status_cache
                        .get(&terminal_id)
                        .cloned()
                        .unwrap_or_default();
                    let cwd: SharedString = cwd.into();
                    // Quick status dot, Orca-style. tcgetpgrp alone can't
                    // tell "computing" from "interactive program awaiting
                    // input", so output silence disambiguates: a claude
                    // session that finished and sits at its input box goes
                    // cyan within seconds.
                    //   green  = shell prompt, ready for commands
                    //   yellow = foreground job producing output (working)
                    //   cyan   = foreground job quiet - awaiting input
                    let quiet =
                        pane_ref.last_activity.elapsed() >= std::time::Duration::from_secs(3);
                    let dot_color = if !busy {
                        theme.green
                    } else if quiet {
                        theme.cyan
                    } else {
                        theme.yellow
                    };
                    let jump_id = terminal_id.clone();
                    rows.push(
                        div()
                            .id(SharedString::from(format!("project-term-{terminal_id}")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(20.0))
                            .pl(px(20.0))
                            .pr(px(8.0))
                            .cursor_pointer()
                            .when(focused, |d| d.bg(rgb(theme.ui_surface)))
                            .hover(|style| style.bg(rgb(theme.ui_surface)))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .bg(rgb(dot_color)),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(120.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.0))
                                    .text_color(rgb(if focused {
                                        theme.ui_accent
                                    } else {
                                        theme.ui_text
                                    }))
                                    .child(SharedString::from(title)),
                            )
                            .child(
                                div()
                                    .flex_grow()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(9.0))
                                    .text_color(rgb(theme.ui_text_muted))
                                    .child(cwd),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focus_terminal_by_id(&jump_id, window, cx);
                                }),
                            )
                            .into_any_element(),
                    );
                }
            }
        }
        div()
            .w(px(240.0))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .border_r_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .text_color(rgb(theme.ui_text))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(rgb(theme.ui_border))
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child("PROJECTS"),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .id("projects-new")
                            .cursor_pointer()
                            .px(px(6.0))
                            .py(px(1.0))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(theme.ui_border))
                            .bg(rgb(theme.ui_surface))
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text))
                            .hover(|style| style.border_color(rgb(theme.ui_accent)))
                            .child("+ new")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, window, cx| {
                                    ws.add_tab(None, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("projects-scroll")
                    .flex_grow()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .py(px(2.0))
                    .children(rows),
            )
            .into_any_element()
    }

    /// Jump to a specific terminal: resolve its OWNING tab at call time
    /// (captured indices go stale), select it, focus the pane.
    fn focus_terminal_by_id(
        &mut self,
        terminal_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|tab| tab.window_of(terminal_id).is_some())
        else {
            return;
        };
        if let Some(window_index) = self.tabs[tab_index].window_of(terminal_id) {
            self.tabs[tab_index].active_window = window_index;
        }
        self.active_tab = tab_index;
        self.focused_terminal = Some(terminal_id.to_string());
        self.push_git_cwd(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn open_sidebar(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.sidebar_view = view;
        match view {
            SidebarView::Projects => {}
            SidebarView::Git => {
                if self.git_panel.is_none() {
                    let theme = self.theme;
                    let panel = cx.new(|panel_cx| GitPanel::new(theme, panel_cx));
                    cx.subscribe(
                        &panel,
                        |ws, _panel, _event: &crate::git_panel::PanelClosed, cx| {
                            ws.close_sidebar(cx);
                        },
                    )
                    .detach();
                    self.git_panel = Some(panel);
                }
            }
            SidebarView::Files => {
                if self.files_panel.is_none() {
                    let theme = self.theme;
                    let panel =
                        cx.new(|panel_cx| crate::files_panel::FilesPanel::new(theme, panel_cx));
                    cx.subscribe(
                        &panel,
                        |ws, _panel, event: &crate::files_panel::OpenFile, cx| {
                            ws.open_file_viewer(event.0.clone(), cx);
                        },
                    )
                    .detach();
                    self.files_panel = Some(panel);
                }
            }
        }
        self.push_git_cwd(cx);
        cx.notify();
    }

    /// Open (or replace) the docked file viewer.
    fn open_file_viewer(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let viewer = cx.new(|viewer_cx| {
            crate::file_viewer::FileViewer::new(path, theme, family, size, viewer_cx)
        });
        cx.subscribe(
            &viewer,
            |ws, _viewer, _event: &crate::file_viewer::ViewerClosed, cx| {
                ws.file_viewer = None;
                cx.notify();
            },
        )
        .detach();
        self.file_viewer = Some(viewer);
        cx.notify();
    }

    /// Dropping the entities ends their poll loops until reopened.
    fn close_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = false;
        self.git_panel = None;
        self.files_panel = None;
        cx.notify();
    }

    fn toggle_git_panel(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_open && self.sidebar_view == SidebarView::Git {
            self.close_sidebar(cx);
        } else {
            self.open_sidebar(SidebarView::Git, cx);
        }
    }

    /// Pick a directory and MOVE the focused terminal there (the bar's cwd
    /// control): a `cd` typed at its prompt. When a program currently owns
    /// that terminal, a new window opens in the project instead — never
    /// type into a running program.
    fn open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |ws, cx| {
            let picked = cx
                .background_executor()
                .spawn(async {
                    std::process::Command::new("/usr/bin/osascript")
                        .args([
                            "-e",
                            "POSIX path of (choose folder with prompt \"Change directory\")",
                        ])
                        .output()
                        .ok()
                        .filter(|out| out.status.success())
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                        .filter(|path| !path.is_empty())
                })
                .await;
            let _ = ws.update_in(cx, |ws: &mut Workspace, window, cx| {
                let Some(path) = picked else { return };
                let focused = ws
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| ws.panes.get(id))
                    .cloned();
                match focused {
                    Some(pane) if pane.read(cx).has_live_shell() && !pane.read(cx).status().1 => {
                        // Shell at its prompt: Ctrl+U first, so any
                        // half-typed input is cleared instead of being
                        // SUBMITTED with the cd appended; then a plain cd,
                        // single-quoted with the POSIX '\'' escape.
                        // Best-effort: a job starting between the busy probe
                        // and this write can still receive the text — full
                        // certainty needs shell integration.
                        let quoted = path.replace('\'', "'\\''");
                        pane.read(cx).send_text(&format!("\u{15}cd '{quoted}'\r"));
                        ws.focus_active_pane(window, cx);
                    }
                    _ => {
                        // Busy (or no) terminal: open a new window in the
                        // current project at that directory.
                        let index = ws.active_tab;
                        if index < ws.tabs.len() {
                            ws.new_window(index, Some(PathBuf::from(path)), cx);
                        } else {
                            ws.add_tab(Some(PathBuf::from(path)), cx);
                        }
                        ws.focus_active_pane(window, cx);
                    }
                }
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Keep the git panel pointed at the focused terminal's directory.
    fn push_git_cwd(&mut self, cx: &mut Context<Self>) {
        let cwd = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .and_then(|pane| pane.read(cx).cwd());
        if let Some(panel) = self.git_panel.clone() {
            panel.update(cx, |panel, panel_cx| {
                panel.set_target_cwd(cwd.clone(), panel_cx)
            });
        }
        if let Some(panel) = self.files_panel.clone() {
            panel.update(cx, |panel, panel_cx| panel.set_root(cwd, panel_cx));
        }
    }

    fn import_theme(&mut self, cx: &mut Context<Self>) {
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

    fn export_theme(&mut self, cx: &mut Context<Self>) {
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

    fn refresh_sessions(&mut self) {
        self.session_names = self.session_manager.list();
        self.session_names.sort();
    }

    fn current_layout(&self) -> Layout {
        Layout {
            tabs: self.tabs.clone(),
            active_tab_id: self
                .tabs
                .get(self.active_tab)
                .map(|t| t.id.clone())
                .unwrap_or_default(),
        }
    }

    fn save_session(&mut self, name: &str) {
        if name.trim().is_empty() {
            return;
        }
        let layout = self.current_layout();
        let _ = self
            .session_manager
            .save(name.trim(), &layout.to_session_json());
        self.refresh_sessions();
    }

    fn load_session(&mut self, name: &str, cx: &mut Context<Self>) {
        let Ok(Some(data)) = self.session_manager.load(name) else {
            return;
        };
        let Some(layout) = data.get("layout").and_then(Layout::from_session_json) else {
            return;
        };
        // Tear down current panes, then rebuild: fresh terminal per leaf
        // (fresh ids so pane entities and session files never collide).
        let old_ids: Vec<String> = self.panes.keys().cloned().collect();
        for id in old_ids {
            if let Some(pane) = self.panes.remove(&id) {
                pane.update(cx, move |pane, _| {
                    if let Some(handle) = pane.shutdown() {
                        std::thread::spawn(move || {
                            handle.join_with_deadline(std::time::Duration::from_secs(3))
                        });
                    }
                });
            }
        }
        self.tabs.clear();
        for tab in layout.tabs {
            let mut mapping = HashMap::new();
            for old_id in tab.all_terminal_ids() {
                let new_id = self.fresh_id();
                self.spawn_pane(new_id.clone(), None, cx);
                mapping.insert(old_id, new_id);
            }
            let windows: Vec<PaneNode> = tab
                .windows
                .iter()
                .map(|window| remap_ids(window, &mapping))
                .collect();
            let active_window = tab.active_window.min(windows.len() - 1);
            self.tabs.push(Tab {
                id: tab.id,
                label: tab.label,
                windows,
                active_window,
            });
        }
        if self.tabs.is_empty() {
            self.add_tab(None, cx);
        } else {
            let wanted = layout.active_tab_id;
            self.active_tab = self.tabs.iter().position(|t| t.id == wanted).unwrap_or(0);
            self.focused_terminal = collect_terminal_ids(self.tabs[self.active_tab].active_pane())
                .into_iter()
                .next();
        }
        self.overlay = Overlay::None;
        cx.notify();
    }

    // --- rendering ---

    fn render_tree(
        &self,
        node: &PaneNode,
        tab_index: usize,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            PaneNode::Terminal { terminal_id } => {
                let Some(pane) = self.panes.get(terminal_id) else {
                    return div().size_full().into_any_element();
                };
                let focused = self
                    .focused_terminal
                    .as_deref()
                    .is_some_and(|f| f == terminal_id);
                let theme = self.theme;

                div()
                    .size_full()
                    .relative()
                    .border_1()
                    .border_color(rgb(if focused {
                        theme.ui_accent
                    } else {
                        theme.ui_border
                    }))
                    .child(pane.clone())
                    .child({
                        // Per-terminal management cluster: always visible,
                        // acts on THIS pane (no focus dance).
                        let id_split_h = terminal_id.clone();
                        let id_split_v = terminal_id.clone();
                        let id_swap = terminal_id.clone();
                        let id_timer = terminal_id.clone();
                        let id_close = terminal_id.clone();
                        let bc_on = self.broadcast.is_enabled();
                        let bc_member = bc_on && self.broadcast.is_member(terminal_id);
                        let id_bc = terminal_id.clone();
                        // Track the terminal font size a little (dampened, so
                        // the cluster grows with big fonts without ballooning).
                        let scale =
                            (1.0 + (self.settings.font_size / 14.0 - 1.0) * 0.6).clamp(0.8, 1.6);
                        let pane_btn = move |label: &'static str| {
                            div()
                                .id(SharedString::from(format!("{label}-{terminal_id}")))
                                .cursor_pointer()
                                .px(px(4.0 * scale))
                                .h(px(15.0 * scale))
                                .flex()
                                .items_center()
                                .rounded(px(3.0))
                                .text_size(px(9.0 * scale))
                                .text_color(rgb(theme.ui_text_muted))
                                .hover(|style| style.bg(rgb(theme.ui_border)))
                                .child(SharedString::from(label))
                        };
                        div()
                            .absolute()
                            .top(px(2.0))
                            .right(px(2.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(1.0))
                            .px(px(2.0))
                            .rounded(px(4.0))
                            .bg(rgb(theme.ui_surface))
                            .opacity(0.75)
                            .hover(|style| style.opacity(1.0))
                            .children(bc_on.then(|| {
                                pane_btn(if bc_member { "bc:on" } else { "bc:off" }).on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _, _, cx| {
                                        ws.broadcast.toggle_member(&id_bc);
                                        cx.notify();
                                    }),
                                )
                            }))
                            .child(pane_btn("split-h").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focused_terminal = Some(id_split_h.clone());
                                    ws.split_focused(SplitDirection::Horizontal, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                            .child(pane_btn("split-v").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focused_terminal = Some(id_split_v.clone());
                                    ws.split_focused(SplitDirection::Vertical, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                            .child(pane_btn("swap").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.swap_source =
                                        if ws.swap_source.as_deref() == Some(id_swap.as_str()) {
                                            None
                                        } else {
                                            Some(id_swap.clone())
                                        };
                                    cx.notify();
                                }),
                            ))
                            .child(pane_btn("timer").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    // Clear highlights BEFORE retargeting
                                    // focus, or the wrong pane gets cleared.
                                    ws.leave_search_highlights(cx);
                                    ws.focused_terminal = Some(id_timer.clone());
                                    ws.overlay = if ws.overlay == Overlay::AutoRun {
                                        Overlay::None
                                    } else {
                                        Overlay::AutoRun
                                    };
                                    cx.notify();
                                }),
                            ))
                            .child(pane_btn("x").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.close_terminal(&id_close, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                    })
                    .into_any_element()
            }
            PaneNode::Split {
                direction,
                children,
                sizes,
            } => {
                let ratios = sizes.unwrap_or([0.5, 0.5]);
                let horizontal = *direction == SplitDirection::Horizontal;
                let window_index = self
                    .tabs
                    .get(tab_index)
                    .map(|tab| tab.active_window)
                    .unwrap_or(0);
                let key = format!("{tab_index}:{window_index}:{path:?}");
                let bounds_map = Arc::clone(&self.split_bounds);
                let key_for_canvas = key.clone();

                let mut first_path = path.clone();
                first_path.push(0);
                let mut second_path = path.clone();
                second_path.push(1);

                let drag_path = path.clone();
                let drag_dir = *direction;

                let container = if horizontal {
                    div().flex().flex_row()
                } else {
                    div().flex().flex_col()
                };
                container
                    .size_full()
                    .relative()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| {
                                bounds_map.lock().unwrap().insert(
                                    key_for_canvas.clone(),
                                    (
                                        bounds.origin.x,
                                        bounds.origin.y,
                                        bounds.size.width,
                                        bounds.size.height,
                                    ),
                                );
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex_basis(gpui::relative(ratios[0]))
                            .overflow_hidden()
                            .child(self.render_tree(&children[0], tab_index, first_path, cx)),
                    )
                    .child(
                        // Divider: draggable to resize.
                        div()
                            .id(SharedString::from(format!("divider-{key}")))
                            .flex_none()
                            .bg(rgb(self.theme.ui_border))
                            .when(horizontal, |d| d.w(px(3.0)).h_full().cursor_col_resize())
                            .when(!horizontal, |d| d.h(px(3.0)).w_full().cursor_row_resize())
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    ws.drag = Some(DragState {
                                        tab_index,
                                        window_index,
                                        path: drag_path.clone(),
                                        direction: drag_dir,
                                    });
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex_basis(gpui::relative(ratios[1]))
                            .overflow_hidden()
                            .child(self.render_tree(&children[1], tab_index, second_path, cx)),
                    )
                    .into_any_element()
            }
        }
    }

    /// Bordered chip for sheet controls: visibly a button, with an accent
    /// state when the option it represents is active. The bar keeps the
    /// text-style `overlay_button` look; sheets use these.
    fn chip_button(
        &self,
        label: &'static str,
        active: bool,
        on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("chip-{label}")))
            .cursor_pointer()
            .px(px(7.0))
            .py(px(1.0))
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(if active {
                theme.ui_accent
            } else {
                theme.ui_border
            }))
            .bg(rgb(theme.ui_surface))
            .text_color(rgb(if active {
                theme.ui_accent
            } else {
                theme.ui_text
            }))
            .hover(|style| {
                style
                    .border_color(rgb(theme.ui_accent))
                    .bg(rgb(theme.ui_border))
            })
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| on_click(ws, window, cx)),
            )
    }

    /// A `[-] value [+]` control drawn as ONE bordered group, so the
    /// buttons visibly belong to the value they step.
    fn stepper(
        &self,
        id: &'static str,
        value: String,
        on_minus: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        on_plus: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        let step = |suffix: &'static str,
                    label: &'static str,
                    handler: BoxedChipHandler,
                    cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(format!("{id}-{suffix}")))
                .cursor_pointer()
                .px(px(7.0))
                .text_color(rgb(theme.ui_text))
                .hover(|style| style.bg(rgb(theme.ui_border)))
                .child(SharedString::from(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, _, window, cx| handler(ws, window, cx)),
                )
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(theme.ui_border))
            .bg(rgb(theme.ui_surface))
            .child(step("minus", "-", Box::new(on_minus), cx))
            .child(
                div()
                    .px(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(SharedString::from(value)),
            )
            .child(step("plus", "+", Box::new(on_plus), cx))
    }

    fn overlay_button(
        &self,
        label: &'static str,
        on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("btn-{label}")))
            .cursor_pointer()
            .px(px(4.0))
            .rounded(px(3.0))
            .text_color(rgb(theme.ui_text_muted))
            .hover(|style| {
                style
                    .bg(rgb(theme.ui_border))
                    .text_color(rgb(theme.ui_text))
            })
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| on_click(ws, window, cx)),
            )
    }

    fn set_split_sizes(
        &mut self,
        tab_index: usize,
        window_index: usize,
        path: &[usize],
        sizes: [f32; 2],
    ) {
        fn walk(node: &mut PaneNode, path: &[usize], sizes: [f32; 2]) {
            match (node, path) {
                (PaneNode::Split { sizes: s, .. }, []) => *s = Some(sizes),
                (PaneNode::Split { children, .. }, [head, rest @ ..]) => {
                    if let Some(child) = children.get_mut(*head) {
                        walk(child, rest, sizes);
                    }
                }
                _ => {}
            }
        }
        if let Some(window) = self
            .tabs
            .get_mut(tab_index)
            .and_then(|tab| tab.windows.get_mut(window_index))
        {
            walk(window, path, sizes);
        }
    }

    /// Controls for the focused pane, living in the bar (tmux-style): they
    /// act on whichever pane has focus, so they never clip in narrow splits.
    fn render_focused_controls(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let theme = self.theme;
        let focused = self.focused_terminal.clone()?;
        let pane = self.panes.get(&focused)?;
        let title = pane.read(cx).title();
        let cwd = pane.read(cx).cwd();
        Some(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .px(px(6.0))
                .text_color(rgb(theme.ui_text_muted))
                .child(SharedString::from(title))
                .child({
                    // Directory control: shows the focused terminal's cwd and
                    // opens a folder picker (new tab at the chosen folder).
                    let display: SharedString = match &cwd {
                        Some(cwd) => cwd
                            .replace(&std::env::var("HOME").unwrap_or_default(), "~")
                            .into(),
                        None => "choose folder".into(),
                    };
                    div()
                        .id("bar-cwd")
                        .cursor_pointer()
                        .max_w(px(280.0))
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(theme.ui_border))
                        .bg(rgb(theme.ui_surface))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(rgb(theme.ui_accent))
                        .hover(|style| style.border_color(rgb(theme.ui_accent)))
                        .child(SharedString::from(format!("dir: {display}")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, window, cx| ws.open_folder(window, cx)),
                        )
                })
                .children(self.swap_source.is_some().then(|| {
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.yellow))
                        .child("click a pane to swap")
                })),
        )
    }

    fn render_font_family_row(
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

    fn render_background_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn render_buddy_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn render_alerts_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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
                .child(self.stepper(
                    "tts-voice",
                    voice_label,
                    |ws, _window, _cx| ws.step_tts_voice(-1),
                    |ws, _window, _cx| ws.step_tts_voice(1),
                    cx,
                ))
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
                            if ws.settings.audio_cues {
                                // Resnapshot so transitions that happened
                                // while disabled don't chime retroactively.
                                ws.cue_track.clear();
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
                    ))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .child("terminal done: Glass - awaiting input: Ping"),
                    ),
            )
            .children(voice_line)
    }

    /// Configure and enable the reviewer with a preset agent command.
    fn set_buddy_agent(&mut self, command: &str, args: &[&str], cx: &mut Context<Self>) {
        self.settings.buddy_command = command.to_string();
        self.settings.buddy_args = args.iter().map(|arg| (*arg).to_string()).collect();
        self.settings.buddy_enabled = true;
        let _ = self.settings.save();
        cx.notify();
    }

    fn open_pet_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pet_name_field.is_none() {
            let theme_ref = self.theme;
            let field = cx.new(|field_cx| TextField::new("name", theme_ref, field_cx));
            cx.subscribe_in(
                &field,
                window,
                |ws, _field, event: &TextFieldEvent, window, cx| match event {
                    TextFieldEvent::Submitted(name) => {
                        let name: String = name.trim().chars().take(14).collect();
                        if !name.is_empty() {
                            ws.companion.save.name = name;
                            ws.save_companion();
                        }
                        cx.notify();
                    }
                    TextFieldEvent::Cancelled => {
                        ws.close_overlay(window, cx);
                    }
                },
            )
            .detach();
            self.pet_name_field = Some(field);
        }
        let name = self.companion.save.name.clone();
        if let Some(field) = &self.pet_name_field {
            field.update(cx, |field, field_cx| {
                field.set_text_selected(&name, field_cx)
            });
            field.read(cx).focus(window);
        }
        self.pet_reroll_armed = false;
        self.pet_card_from_theme = self.overlay == Overlay::SettingsSheet;
        self.overlay = Overlay::PetCard;
        // Keep the name field focused past the root's click-to-focus.
        window.prevent_default();
        cx.notify();
    }

    /// The floating pet. Drag to move, click to pet, right-click for its
    /// card. The bubble carries reviewer notes only — click copies it.
    fn render_pet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.settings.buddy_pet_visible {
            return None;
        }
        const PET_W: f32 = 110.0;
        const PET_H: f32 = 100.0;
        let theme = self.theme;
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let (x, y) = self
            .pet_pos
            .unwrap_or((vw - PET_W - 24.0, vh - PET_H - 60.0));
        let x = x.clamp(0.0, (vw - PET_W).max(0.0));
        let y = y.clamp(34.0, (vh - PET_H).max(34.0));
        let hop = if self.pet_hop > 0 { 5.0 } else { 0.0 };
        let art = self.companion.art_frame(self.pet_frame, self.pet_blink);
        let color = self.companion.rarity_color();
        Some(
            div()
                .id("buddy-pet")
                .absolute()
                // Anchor the RIGHT and BOTTOM edges of the creature: a speech
                // bubble then grows up and to the left instead of shoving the
                // pet around or spilling off-screen. `(x, y)` stays the
                // creature's top-left for the drag math.
                .right(px((vw - x - PET_W).max(0.0)))
                .bottom(px(vh - PET_H - y + hop))
                .flex()
                .flex_col()
                .items_end()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, event: &gpui::MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let down = (f32::from(event.position.x), f32::from(event.position.y));
                        ws.pet_drag = Some(PetDrag {
                            offset: (down.0 - x, down.1 - y),
                            down,
                            moved: false,
                        });
                        cx.notify();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.open_pet_card(window, cx);
                    }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .font_family(self.settings.font_family.clone())
                        .text_size(px(12.0))
                        .line_height(px(13.0))
                        .text_color(rgb(color))
                        .children(
                            art.into_iter().map(|line| {
                                div().whitespace_nowrap().child(SharedString::from(line))
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(4.0))
                                .text_size(px(10.0))
                                .child(
                                    div()
                                        .text_color(rgb(color))
                                        .child(SharedString::from(self.companion.stars())),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(theme.ui_text))
                                        .child(SharedString::from(
                                            self.companion.save.name.clone(),
                                        )),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The pet's speech bubble as its own absolute element, clamped to the
    /// viewport: it tracks the pet's right edge but never crosses the left
    /// margin, grows upward when there's headroom and flips below otherwise.
    fn render_pet_bubble(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.settings.buddy_pet_visible {
            return None;
        }
        let text = self.pet_bubble.as_ref().map(|(text, _)| text.clone())?;
        const PET_W: f32 = 110.0;
        const PET_H: f32 = 100.0;
        let theme = self.theme;
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let (x, y) = self
            .pet_pos
            .unwrap_or((vw - PET_W - 24.0, vh - PET_H - 60.0));
        let x = x.clamp(0.0, (vw - PET_W).max(0.0));
        let y = y.clamp(34.0, (vh - PET_H).max(34.0));
        let right_edge = (x + PET_W).clamp(96.0f32.min(vw), (vw - 8.0).max(96.0f32.min(vw)));
        let max_w = (right_edge - 8.0).clamp(80.0, 280.0);
        // Pick the roomier side and never claim more height than it really
        // has — a floor here would overlay the titlebar or the bottom bar in
        // small windows. Skip the bubble entirely when neither side fits.
        let headroom = (y - 40.0).max(0.0);
        let below_room = (vh - y - PET_H - 46.0).max(0.0);
        let above = headroom >= 100.0 || headroom >= below_room;
        let space = if above { headroom } else { below_room };
        if space < 16.0 {
            return None;
        }
        let bubble = div()
            .id("buddy-bubble")
            .absolute()
            .right(px(vw - right_edge))
            .max_w(px(max_w))
            .px(px(10.0))
            .py(px(6.0))
            .rounded(px(8.0))
            .bg(rgb(theme.ui_surface))
            .border_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .text_color(rgb(theme.ui_text))
            .overflow_hidden()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _, _, cx| {
                    cx.stop_propagation();
                    // Click a note to copy it; the bubble closes.
                    if let Some((text, _)) = ws.pet_bubble.take() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    }
                    cx.notify();
                }),
            )
            .child(SharedString::from(text));
        Some(if above {
            bubble
                .bottom(px(vh - y + 6.0))
                .max_h(px(space))
                .into_any_element()
        } else {
            bubble
                .top(px(y + PET_H + 6.0))
                .max_h(px(space))
                .into_any_element()
        })
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .px(px(8.0))
            .py(px(3.0))
            .bg(rgb(theme.ui_surface))
            .border_t_1()
            .border_color(rgb(theme.ui_border))
            .text_size(px(11.0))
            .child(div().flex_grow())
            .children(
                self.buddy_note
                    .clone()
                    .filter(|_| !self.settings.buddy_pet_visible)
                    .map(|note| {
                        div()
                            .id("buddy-note")
                            .max_w(px(420.0))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(note))
                    }),
            )
            .children(self.render_focused_controls(cx))
            .child({
                let enabled = self.broadcast.is_enabled();
                let theme = self.theme;
                div()
                    .id("bc-toggle")
                    .cursor_pointer()
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(3.0))
                    .bg(rgb(if enabled {
                        theme.ui_accent
                    } else {
                        theme.ui_surface
                    }))
                    .text_color(rgb(if enabled {
                        theme.ui_background
                    } else {
                        theme.ui_text_muted
                    }))
                    .child("broadcast")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, window, cx| {
                            let now = !ws.broadcast.is_enabled();
                            ws.broadcast
                                .enabled
                                .store(now, std::sync::atomic::Ordering::Relaxed);
                            ws.focus_active_pane(window, cx);
                            cx.notify();
                        }),
                    )
            })
            .child(self.overlay_button("git", |ws, _window, cx| ws.toggle_git_panel(cx), cx))
            .child(self.overlay_button("search", |ws, window, cx| ws.toggle_search(window, cx), cx))
            .child(self.overlay_button(
                "sessions",
                |ws, _window, cx| {
                    ws.refresh_sessions();
                    ws.leave_search_highlights(cx);
                    ws.overlay = if ws.overlay == Overlay::Sessions {
                        Overlay::None
                    } else {
                        Overlay::Sessions
                    };
                    cx.notify();
                },
                cx,
            ))
            .child(self.overlay_button(
                "settings",
                |ws, window, cx| {
                    ws.leave_search_highlights(cx);
                    ws.overlay = if ws.overlay == Overlay::SettingsSheet {
                        Overlay::None
                    } else {
                        window.focus(&ws.focus_handle);
                        Overlay::SettingsSheet
                    };
                    cx.notify();
                },
                cx,
            ))
    }

    fn render_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let theme = self.theme;
        match self.overlay {
            Overlay::None => None,
            Overlay::SettingsSheet => {
                let current = self.settings.theme.clone();
                let chips: Vec<_> = themes::all_themes()
                    .into_iter()
                    .map(|preset| {
                        let name = preset.name;
                        let selected = name == current;
                        // The palette IS the content: bg swatch + four accents.
                        let strip = [
                            preset.background,
                            preset.red,
                            preset.green,
                            preset.blue,
                            preset.magenta,
                        ];
                        div()
                            .id(SharedString::from(format!("theme-{name}")))
                            .cursor_pointer()
                            .px(px(10.0))
                            .py(px(6.0))
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(rgb(if selected {
                                theme.ui_accent
                            } else {
                                theme.ui_border
                            }))
                            .bg(rgb(preset.background))
                            .hover(|style| style.border_color(rgb(theme.ui_accent)))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(div().flex().flex_row().gap(px(3.0)).children(
                                strip.into_iter().map(|color| {
                                    div().w(px(14.0)).h(px(6.0)).rounded(px(2.0)).bg(rgb(color))
                                }),
                            ))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    // Some themes have muted foregrounds that
                                    // vanish on their own background; nudge
                                    // the label away from the chip color.
                                    .text_color(rgb(themes::contrast_boost(
                                        preset.foreground,
                                        preset.background,
                                    )))
                                    .child(SharedString::from(name)),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _window, cx| {
                                    // Apply live but keep the sheet open so
                                    // themes can be browsed; esc closes.
                                    ws.apply_theme(name, cx);
                                }),
                            )
                    })
                    .collect();
                let font_size = self.settings.font_size;
                Some(
                    self.sheet("settings", "click to apply - esc closes", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .gap(px(6.0))
                                .children(chips),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .pt(px(4.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(div().w(px(72.0)).child("size"))
                                .child(self.stepper(
                                    "font-size",
                                    format!("{font_size:.0} px"),
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size - 1.0, cx)
                                    },
                                    |ws, _window, cx| {
                                        ws.set_font_size(ws.settings.font_size + 1.0, cx)
                                    },
                                    cx,
                                )),
                        )
                        .child(self.render_font_family_row(window, cx))
                        .child(self.render_background_row(cx))
                        .child(self.render_buddy_row(cx))
                        .child(self.render_alerts_row(cx))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(div().w(px(72.0)).child("custom"))
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
                                .children(self.theme_action_note.clone().map(|note| {
                                    div().text_size(px(10.0)).child(SharedString::from(note))
                                })),
                        )
                        .into_any_element(),
                )
            }
            Overlay::Search => {
                if self.search_field.is_none() {
                    let theme_ref = self.theme;
                    let field = cx
                        .new(|field_cx| TextField::new("find in scrollback", theme_ref, field_cx));
                    cx.subscribe_in(
                        &field,
                        window,
                        |ws, field, event: &TextFieldEvent, window, cx| match event {
                            TextFieldEvent::Submitted(_) => {
                                // Enter jumps to the next (older) match.
                                let needle = field.read(cx).value.clone();
                                if let Some(pane) =
                                    ws.focused_terminal.as_ref().and_then(|id| ws.panes.get(id))
                                {
                                    pane.update(cx, |pane, pane_cx| {
                                        pane.set_search(Some(&needle), pane_cx);
                                        pane.search_next(pane_cx);
                                    });
                                }
                            }
                            TextFieldEvent::Cancelled => {
                                // Same close path as the toolbar/root-escape:
                                // clears highlights and restores pane focus.
                                ws.close_overlay(window, cx);
                            }
                        },
                    )
                    .detach();
                    self.search_field = Some(field);
                }
                let field = self.search_field.clone().unwrap();
                field.read(cx).focus(window);
                // Live highlight as the needle changes.
                let needle = field.read(cx).value.clone();
                if let Some(pane) = self
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| self.panes.get(id))
                {
                    pane.update(cx, |pane, pane_cx| {
                        pane.set_search((!needle.is_empty()).then_some(needle.as_str()), pane_cx)
                    });
                }
                Some(
                    self.sheet("search", "enter jumps to older matches - esc clears", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("find >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .into_any_element(),
                )
            }
            Overlay::AutoRun => {
                if self.auto_run_field.is_none() {
                    let theme_ref = self.theme;
                    let field = cx.new(|field_cx| {
                        TextField::new("command, e.g. kubectl get pods", theme_ref, field_cx)
                    });
                    cx.subscribe(
                        &field,
                        |ws, _field, event: &TextFieldEvent, cx| match event {
                            TextFieldEvent::Submitted(command) => {
                                ws.apply_auto_run(command.clone(), cx);
                            }
                            TextFieldEvent::Cancelled => {
                                ws.overlay = Overlay::None;
                                cx.notify();
                            }
                        },
                    )
                    .detach();
                    self.auto_run_field = Some(field);
                }
                let field = self.auto_run_field.clone().unwrap();
                field.read(cx).focus(window);
                let interval = self.auto_run_interval;
                let escape = self.auto_run_escape;
                let escape_delay = self.auto_run_escape_delay;
                let active = self
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| self.panes.get(id))
                    .is_some_and(|pane| pane.read(cx).auto_run.is_some());
                Some(
                    self.sheet("auto-run", "enter starts - esc closes", cx)
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("run >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child(self.stepper(
                                    "auto-run-interval",
                                    format!("every {interval}s"),
                                    |ws, _window, cx| {
                                        ws.auto_run_interval =
                                            ws.auto_run_interval.saturating_sub(1).max(1);
                                        cx.notify();
                                    },
                                    |ws, _window, cx| {
                                        ws.auto_run_interval = (ws.auto_run_interval + 1).min(3600);
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .child(self.chip_button(
                                    if escape {
                                        "esc after: on"
                                    } else {
                                        "esc after: off"
                                    },
                                    escape,
                                    |ws, _window, cx| {
                                        ws.auto_run_escape = !ws.auto_run_escape;
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .children(
                                    escape.then(|| {
                                        SharedString::from(format!("{escape_delay}s delay"))
                                    }),
                                )
                                .children(active.then(|| {
                                    self.chip_button(
                                        "stop",
                                        false,
                                        |ws, window, cx| {
                                            if let Some(pane) = ws
                                                .focused_terminal
                                                .as_ref()
                                                .and_then(|id| ws.panes.get(id))
                                            {
                                                pane.update(cx, |pane, _| pane.set_auto_run(None));
                                            }
                                            ws.overlay = Overlay::None;
                                            ws.focus_active_pane(window, cx);
                                            cx.notify();
                                        },
                                        cx,
                                    )
                                })),
                        )
                        .into_any_element(),
                )
            }
            Overlay::Sessions => {
                if self.session_field.is_none() {
                    let theme_ref = self.theme;
                    let field =
                        cx.new(|field_cx| TextField::new("session name", theme_ref, field_cx));
                    cx.subscribe(
                        &field,
                        |ws, field, event: &TextFieldEvent, cx| match event {
                            TextFieldEvent::Submitted(name) => {
                                ws.save_session(name);
                                field.update(cx, |f, cx| f.reset(cx));
                                cx.notify();
                            }
                            TextFieldEvent::Cancelled => {
                                ws.overlay = Overlay::None;
                                cx.notify();
                            }
                        },
                    )
                    .detach();
                    self.session_field = Some(field);
                }
                let field = self.session_field.clone().unwrap();
                field.read(cx).focus(window);

                let rows: Vec<_> = self
                    .session_names
                    .clone()
                    .into_iter()
                    .map(|name| {
                        let load_name = name.clone();
                        let delete_name = name.clone();
                        div()
                            .id(SharedString::from(format!("session-{name}")))
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(4.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .hover(|style| style.bg(rgb(theme.ui_surface)))
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-load-{name}")))
                                    .cursor_pointer()
                                    .flex_grow()
                                    .text_color(rgb(theme.ui_text))
                                    .child(SharedString::from(name.clone()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, window, cx| {
                                            ws.load_session(&load_name, cx);
                                            ws.focus_active_pane(window, cx);
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-del-{name}")))
                                    .cursor_pointer()
                                    .px(px(4.0))
                                    .rounded(px(3.0))
                                    .opacity(0.5)
                                    .hover(|style| style.opacity(1.0).bg(rgb(theme.ui_surface)))
                                    .text_color(rgb(theme.red))
                                    .child("x")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, _, cx| {
                                            ws.session_manager.delete(&delete_name);
                                            ws.refresh_sessions();
                                            cx.notify();
                                        }),
                                    ),
                            )
                    })
                    .collect();
                let empty = rows.is_empty();
                Some(
                    self.sheet("sessions", "enter saves - click loads - esc closes", cx)
                        .child(
                            // Prompt-style save line.
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .child(div().text_color(rgb(theme.ui_accent)).child("save as >"))
                                .child(div().flex_grow().child(field)),
                        )
                        .child(div().flex().flex_col().gap(px(1.0)).children(rows))
                        .children(empty.then(|| {
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(theme.ui_text_muted))
                                .child("no saved sessions yet - type a name and press enter")
                        }))
                        .into_any_element(),
                )
            }
            Overlay::PetCard => Some(self.render_pet_card(cx)),
        }
    }

    fn render_pet_card(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let color = self.companion.rarity_color();
        let art = self.companion.art_frame(0, false);
        let identity = format!(
            "{}{} {} {}",
            if self.companion.bones.shiny {
                "shiny "
            } else {
                ""
            },
            self.companion.rarity_name(),
            self.companion.species_name(),
            self.companion.stars(),
        );
        let pets = format!("pets: {}", self.companion.save.pet_count);

        let stat_rows = crate::buddy_pet::STAT_NAMES
            .iter()
            .enumerate()
            .map(|(index, stat)| {
                let value = self.companion.bones.stats[index];
                let filled = ((value as f64 / 10.0).round() as usize).min(10);
                let bar = "\u{2588}".repeat(filled) + &"\u{2591}".repeat(10 - filled);
                let marker = if index == self.companion.bones.peak {
                    " \u{25b2}"
                } else if index == self.companion.bones.dump {
                    " \u{25bc}"
                } else {
                    ""
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(px(10.0))
                    .child(
                        div()
                            .w(px(72.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(*stat)),
                    )
                    .child(div().text_color(rgb(color)).child(SharedString::from(bar)))
                    .child(SharedString::from(format!("{value}{marker}")))
            })
            .collect::<Vec<_>>();

        let reroll_armed = self.pet_reroll_armed;
        let pet_visible = self.settings.buddy_pet_visible;

        self.sheet("buddy", "enter saves name - esc closes", cx)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(18.0))
                    .items_start()
                    .child(
                        // Portrait, in the terminal font like the pet itself.
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .font_family(self.settings.font_family.clone())
                            .text_size(px(12.0))
                            .line_height(px(13.0))
                            .text_color(rgb(color))
                            .children(art.into_iter().map(|line| {
                                div().whitespace_nowrap().child(SharedString::from(line))
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .flex_grow()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(div().text_color(rgb(theme.ui_accent)).child("name >"))
                                    .child(
                                        div().w(px(180.0)).children(self.pet_name_field.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(color))
                                    .child(SharedString::from(identity)),
                            )
                            .child(div().flex().flex_col().gap(px(2.0)).children(stat_rows))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(8.0))
                                    .text_size(px(10.0))
                                    .child(
                                        div()
                                            .text_color(rgb(theme.ui_text_muted))
                                            .child(SharedString::from(pets)),
                                    )
                                    .child(
                                        div()
                                            .id("pet-reroll")
                                            .cursor_pointer()
                                            .px(px(6.0))
                                            .py(px(2.0))
                                            .rounded(px(3.0))
                                            .border_1()
                                            .border_color(rgb(theme.ui_border))
                                            .text_color(rgb(if reroll_armed {
                                                theme.red
                                            } else {
                                                theme.ui_text_muted
                                            }))
                                            .hover(|style| style.bg(rgb(theme.ui_border)))
                                            .child(if reroll_armed {
                                                "replace this pet?"
                                            } else {
                                                "re-roll"
                                            })
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|ws, _, _, cx| {
                                                    if ws.pet_reroll_armed {
                                                        ws.companion = Companion::hatch();
                                                        ws.save_companion();
                                                        ws.pet_reroll_armed = false;
                                                        if let Some(field) = &ws.pet_name_field {
                                                            let name =
                                                                ws.companion.save.name.clone();
                                                            field.update(cx, |field, field_cx| {
                                                                field.set_text_selected(
                                                                    &name, field_cx,
                                                                );
                                                            });
                                                        }
                                                    } else {
                                                        ws.pet_reroll_armed = true;
                                                    }
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("pet-visibility")
                                            .cursor_pointer()
                                            .px(px(6.0))
                                            .py(px(2.0))
                                            .rounded(px(3.0))
                                            .border_1()
                                            .border_color(rgb(theme.ui_border))
                                            .text_color(rgb(theme.ui_text_muted))
                                            .hover(|style| style.bg(rgb(theme.ui_border)))
                                            .child(if pet_visible {
                                                "hide pet"
                                            } else {
                                                "show pet"
                                            })
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(|ws, _, _, cx| {
                                                    ws.settings.buddy_pet_visible =
                                                        !ws.settings.buddy_pet_visible;
                                                    let _ = ws.settings.save();
                                                    cx.notify();
                                                }),
                                            ),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// Bottom sheet anchored above the bar: panels read as extensions of the
    /// tmux bar, not floating dialogs.
    fn sheet(
        &self,
        title: &'static str,
        hint: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("sheet-{title}")))
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .max_h(px(360.0))
            .bg(rgb(theme.ui_background))
            .border_t_2()
            .border_color(rgb(theme.ui_accent))
            .px(px(14.0))
            .py(px(10.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .text_size(px(12.0))
            .text_color(rgb(theme.ui_text))
            // Swallow clicks so sheet chrome never reaches the terminal
            // beneath (child controls have already handled theirs by the
            // time this bubbles).
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .text_color(rgb(theme.ui_accent))
                            .child(SharedString::from(title)),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(hint)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("sheet-close-{title}")))
                            .cursor_pointer()
                            .ml(px(8.0))
                            .px(px(5.0))
                            .rounded(px(3.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .hover(|style| style.text_color(rgb(theme.red)))
                            .child("x")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, window, cx| {
                                    cx.stop_propagation();
                                    ws.close_overlay(window, cx);
                                }),
                            ),
                    ),
            )
    }
}

fn remap_ids(node: &PaneNode, mapping: &HashMap<String, String>) -> PaneNode {
    match node {
        PaneNode::Terminal { terminal_id } => PaneNode::Terminal {
            terminal_id: mapping
                .get(terminal_id)
                .cloned()
                .unwrap_or_else(|| terminal_id.clone()),
        },
        PaneNode::Split {
            direction,
            children,
            sizes,
        } => PaneNode::Split {
            direction: *direction,
            children: children.iter().map(|c| remap_ids(c, mapping)).collect(),
            sizes: *sizes,
        },
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let active_tree = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.active_pane().clone());

        let content = match active_tree {
            Some(tree) => self.render_tree(&tree, self.active_tab, Vec::new(), cx),
            None => div().size_full().into_any_element(),
        };

        // Clicking away from a tab rename commits it (matching the old
        // app's blur behavior) — never re-steal focus from whatever the
        // user clicked.
        if let Some((index, field)) = self.rename_field.clone() {
            let focused = field.read(cx).is_focused(window);
            if focused {
                // Focus observed at least once: the blur check is armed.
                self.rename_blur_armed = true;
            } else if self.rename_blur_armed {
                // Observed focus was lost: commit (old app's blur behavior).
                let name = field.read(cx).value.trim().to_string();
                if !name.is_empty() {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.label = name;
                    }
                }
                self.rename_field = None;
            } else {
                // Focus never arrived (something stole it at creation): wait
                // a few renders, then dismiss quietly — never steal it back.
                self.rename_grace = self.rename_grace.saturating_add(1);
                if self.rename_grace >= 8 {
                    self.rename_field = None;
                }
            }
        }

        let background_layer = self.settings.background_image.as_ref().map(|path| {
            gpui::img(std::path::PathBuf::from(path))
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(gpui::ObjectFit::Cover)
                .opacity(self.settings.background_opacity)
        });

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(theme.ui_background))
            .children(background_layer)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|ws, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && ws.overlay != Overlay::None {
                    ws.close_overlay(window, cx);
                }
            }))
            .on_action(cx.listener(|ws, _: &NewTab, window, cx| {
                ws.add_tab(None, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &NewWindow, window, cx| {
                let index = ws.active_tab;
                ws.new_window(index, None, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &CloseTab, window, cx| {
                let index = ws.active_tab;
                ws.close_tab(index, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &CloseFocused, window, cx| {
                ws.close_focused(cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SplitRight, window, cx| {
                ws.split_focused(SplitDirection::Horizontal, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SplitDown, window, cx| {
                ws.split_focused(SplitDirection::Vertical, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &ToggleSettingsSheet, window, cx| {
                ws.leave_search_highlights(cx);
                ws.overlay = if ws.overlay == Overlay::SettingsSheet {
                    Overlay::None
                } else {
                    window.focus(&ws.focus_handle);
                    Overlay::SettingsSheet
                };
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ToggleSessions, _, cx| {
                ws.refresh_sessions();
                ws.leave_search_highlights(cx);
                ws.overlay = if ws.overlay == Overlay::Sessions {
                    Overlay::None
                } else {
                    Overlay::Sessions
                };
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &ToggleSearch, window, cx| {
                ws.toggle_search(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &ToggleGitPanel, window, cx| {
                ws.toggle_git_panel(cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SaveSessionAs, _, cx| {
                ws.refresh_sessions();
                ws.leave_search_highlights(cx);
                ws.overlay = Overlay::Sessions;
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &SelectTab1, window, cx| {
                ws.select_tab(0, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab2, window, cx| {
                ws.select_tab(1, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab3, window, cx| {
                ws.select_tab(2, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab4, window, cx| {
                ws.select_tab(3, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab5, window, cx| {
                ws.select_tab(4, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab6, window, cx| {
                ws.select_tab(5, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab7, window, cx| {
                ws.select_tab(6, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab8, window, cx| {
                ws.select_tab(7, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &SelectTab9, window, cx| {
                ws.select_tab(8, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_mouse_move(cx.listener(|ws, event: &MouseMoveEvent, window, cx| {
                if let Some(pet_drag) = &mut ws.pet_drag {
                    let (mx, my) = (f32::from(event.position.x), f32::from(event.position.y));
                    if !pet_drag.moved {
                        let (dx, dy) = (mx - pet_drag.down.0, my - pet_drag.down.1);
                        if dx.abs() + dy.abs() > 3.0 {
                            pet_drag.moved = true;
                        }
                    }
                    if pet_drag.moved {
                        let viewport = window.viewport_size();
                        let x = (mx - pet_drag.offset.0)
                            .clamp(0.0, (f32::from(viewport.width) - 110.0).max(0.0));
                        let y = (my - pet_drag.offset.1)
                            .clamp(34.0, (f32::from(viewport.height) - 100.0).max(34.0));
                        ws.pet_pos = Some((x, y));
                        cx.notify();
                    }
                    return;
                }
                let Some(drag) = &ws.drag else { return };
                let key = format!("{}:{}:{:?}", drag.tab_index, drag.window_index, drag.path);
                let Some((x, y, w, h)) = ws.split_bounds.lock().unwrap().get(&key).copied() else {
                    return;
                };
                let ratio = match drag.direction {
                    SplitDirection::Horizontal => {
                        (f32::from(event.position.x) - f32::from(x)) / f32::from(w).max(1.0)
                    }
                    SplitDirection::Vertical => {
                        (f32::from(event.position.y) - f32::from(y)) / f32::from(h).max(1.0)
                    }
                }
                .clamp(0.1, 0.9);
                let (tab_index, window_index, path) =
                    (drag.tab_index, drag.window_index, drag.path.clone());
                ws.set_split_sizes(tab_index, window_index, &path, [ratio, 1.0 - ratio]);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseUpEvent, _, cx| {
                    if let Some(pet_drag) = ws.pet_drag.take() {
                        if pet_drag.moved {
                            ws.settings.buddy_pet_pos = ws.pet_pos;
                            let _ = ws.settings.save();
                        } else {
                            // A plain click is a pet: bump the count, hop.
                            // The count persists after a 1s quiet debounce.
                            ws.companion.save.pet_count =
                                ws.companion.save.pet_count.saturating_add(1);
                            ws.pet_save_at = Some(std::time::Instant::now());
                            ws.pet_hop = 2;
                        }
                        cx.notify();
                    }
                    if ws.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .children((!window.is_fullscreen()).then(|| {
                // Titlebar drag strip under the traffic lights — macOS hides
                // them in fullscreen, so the strip collapses there too.
                div()
                    .flex_none()
                    .h(px(34.0))
                    .w_full()
                    .bg(rgb(theme.ui_background))
                    .window_control_area(gpui::WindowControlArea::Drag)
            }))
            .child(
                // Content row: the sidebar (activity rail + view) on the
                // left, the terminal tree filling the rest.
                div()
                    .flex_grow()
                    .overflow_hidden()
                    .flex()
                    .flex_row()
                    .children(self.render_sidebar(cx))
                    .child(
                        div()
                            .flex_grow()
                            .overflow_hidden()
                            .relative()
                            .child(content),
                    )
                    .children(self.file_viewer.clone()),
            )
            .child(self.render_bar(cx))
            .children(self.render_pet(window, cx))
            .children(self.render_pet_bubble(window, cx))
            .children(self.render_overlay(window, cx))
    }
}
