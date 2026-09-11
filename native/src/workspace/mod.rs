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

use superterminal_core::activity::Activity;
use superterminal_core::session::SessionManager;

use crate::buddy_pet::Companion;
use crate::git_panel::GitPanel;
use crate::layout::{
    collect_terminal_ids, insert_split, remove_terminal, Layout, PaneNode, SplitDirection, Tab,
};
use crate::pane::{BroadcastHub, PaneEvent, TerminalPane};
use crate::peer_client::sessions::SessionPoller as PeerSessionPoller;
use crate::settings::Settings;
use crate::term_session::ShutdownHandle;
use crate::text_field::{TextField, TextFieldEvent};
use crate::themes::{self, Theme};

mod companion_ui;
mod settings_ui;

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

/// Which settings section shows in the settings sheet.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SettingsSection {
    /// Everything about how the app LOOKS: palette, text, background, and
    /// the theme files that carry all three. These were four separate nav
    /// items; one pane with group labels beats hunting across tabs.
    Appearance,
    Buddy,
    Alerts,
    /// Phone-side configuration: the watched gallery folder and the live
    /// Blender viewport. Previously mis-filed under "background".
    Companion,
}

impl SettingsSection {
    /// Nav order, top to bottom. The renderer walks this, so a new section
    /// cannot ship without a label.
    const ALL: [SettingsSection; 4] = [
        SettingsSection::Appearance,
        SettingsSection::Buddy,
        SettingsSection::Alerts,
        SettingsSection::Companion,
    ];

    /// The pane the sheet opens on.
    const DEFAULT: SettingsSection = SettingsSection::Appearance;

    fn label(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "appearance",
            SettingsSection::Buddy => "buddy",
            SettingsSection::Alerts => "alerts",
            SettingsSection::Companion => "companion",
        }
    }
}

/// Which view the left sidebar shows; the rail tabs between them.
#[derive(Clone, Copy, PartialEq)]
enum SidebarView {
    Projects,
    Git,
    Files,
    /// Paired Macs and the terminals they are sharing with this one.
    Peers,
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

// Cue decisions live in `superterminal_core::cue` (pure, unit-tested): Ping
// fires only on a real terminal bell, Glass on the tcgetpgrp busy→prompt
// transition. The old output-timing heuristic is gone — it mistook idle-TUI
// redraw blips for work and pinged repeatedly.

/// One line of `say -v ?`: name (may contain spaces and suffixes like
/// "(Enhanced)"), then a locale token, then "# sample". The name is
/// everything before the locale token.
/// Break every `[[` pair (repeatedly — overlapping runs like `[[[` must
/// not survive a single pass) so note text cannot open an embedded
/// speech command.
fn neutralize_say_commands(text: &str) -> String {
    let mut out = text.to_string();
    while out.contains("[[") {
        out = out.replace("[[", "[ [");
    }
    out
}

fn parse_voice_name(line: &str) -> Option<String> {
    let before_hash = line.split('#').next()?.trim_end();
    let name = before_hash
        .rsplit_once(|c: char| c.is_whitespace())
        .map(|(name, _locale)| name.trim_end())
        .unwrap_or(before_hash);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// The buddy's voice: a named character, substance first, personality as
/// seasoning (Tomas's 40% feedback / 60% character calibration). Reviews
/// (change present) must land one concrete observation; reactions stay
/// grounded in what actually happened. Notes are spoken by TTS, so exactly
/// one short speakable line, no emoji (rendered UI never uses them).
fn buddy_prompt(name: &str, tail: &str, change: Option<(&str, &str)>) -> String {
    let identity = format!(
        "You are {name}, a dry-witted senior engineer who lives in this \
         terminal as its resident pet. Reply with exactly ONE short, \
         speakable line. No preamble, no markdown, no emoji."
    );
    match change {
        Some((label, patch)) => format!(
            "{identity} Review the change below against the recent terminal \
             output: deliver one concrete, actionable observation (a bug, a \
             risk, or the next step). Personality is welcome, but the \
             finding always comes first — never a quip instead of substance.\
             \n\nTERMINAL OUTPUT:\n{tail}\n\n{label}:\n{patch}"
        ),
        None => format!(
            "{identity} A job just finished in the terminal; its output is \
             below. React in character to what ACTUALLY happened — ground \
             every word in the output and never invent events. If something \
             failed, point at the likely cause.\n\nTERMINAL OUTPUT:\n{tail}"
        ),
    }
}

/// Spawn a system sound; the caller keeps the child for reaping.
fn play_sound(name: &str) -> Option<std::process::Child> {
    std::process::Command::new("/usr/bin/afplay")
        .arg(format!("/System/Library/Sounds/{name}.aiff"))
        .spawn()
        .ok()
}

/// Whether a picked LOCAL path may be typed into this pane.
///
/// Two independent guards. The activity check is the old best-effort race
/// gate, tightened so `Unknown` never authorises. The target check does not
/// depend on the activity signal being honest at all, which is what makes
/// this safe once a remote host can report its own state.
fn may_write_cd(target: &crate::hosts::Target, activity: Activity) -> bool {
    target.is_local() && activity.is_idle()
}

/// Which per-peer `/sessions` pollers to stop, given the ones currently
/// held and the peers some open pane still needs polled.
///
/// This is what "one poller per PEER, not per attachment" actually means at
/// runtime: closing ONE of two panes open on the same machine must leave
/// that machine's poller running for the other, and only the last pane
/// closing stops it. Keyed on the peer, so a pane closing has no effect
/// unless it was the last one there.
fn pollers_to_drop(
    held: &[crate::companion::auth::PeerId],
    needed: &[crate::companion::auth::PeerId],
) -> Vec<crate::companion::auth::PeerId> {
    held.iter()
        .filter(|peer| !needed.contains(peer))
        .cloned()
        .collect()
}

/// Whether this pane gives the app a usable LOCAL directory: the gate for
/// buddy repo probing and the focused-bar directory control.
fn local_context_available(target: &crate::hosts::Target, cwd: Option<String>) -> bool {
    target.is_local() && cwd.is_some()
}

/// Whether this pane may be offered — or granted — peer-share visibility at
/// all. Gates the sidebar's Share icon and share row (`render_projects_view`)
/// AND the mutation those drive (`companion_ui::toggle_share`) on the pane's
/// `target`, never on whether a companion sender happens to exist for it:
/// `spawn_dead_pane` never registers with the hub, so a naive "has a sender"
/// check would let a dead remote pane's Share control claim success while
/// sharing nothing. Mirrors `pane::may_broadcast_locally`'s reasoning for the
/// LOCAL keystroke fan-out — kept as its own predicate rather than reused
/// because the two gate different hubs for different reasons, and a future
/// attached pane (a local view of a terminal running on another Mac) is
/// exactly where they are expected to diverge.
fn may_share_terminal(target: &crate::hosts::Target) -> bool {
    target.is_local()
}

// ---------------------------------------------------------------------------
// Task 7: opening a peer's terminal as a pane.
//
// Everything below decides something the UI then carries out. None of it
// touches a socket, an entity or a `Context`, because there is no gpui test
// harness and none may be introduced — the wiring that calls these is
// verified by reading and says so where it happens.
// ---------------------------------------------------------------------------

/// Peers still worth asking `/sessions`.
///
/// Two reasons a poller must live: a PANE is attached to that peer, or the
/// sidebar is showing that peer's session list so one can be opened. Both
/// go in, because `prune_peer_pollers` runs on the ~900ms sweep and would
/// otherwise kill a browsing poller between the click that started it and
/// the click that uses it — the list would flicker empty and never fill.
fn peer_pollers_needed(
    attached: &[crate::companion::auth::PeerId],
    browsing: Option<&crate::companion::auth::PeerId>,
) -> Vec<crate::companion::auth::PeerId> {
    let mut needed = attached.to_vec();
    if let Some(peer) = browsing {
        needed.push(peer.clone());
    }
    needed
}

/// What a non-local pane's target is CALLED.
///
/// A peer-attached pane and an ssh-profile pane both carry
/// `Target::Remote(ProfileId)`, and the id namespaces are the same
/// generator, so this asks both lists. Profiles first, keeping the previous
/// answer for every target that existed before this phase; peers next, so
/// an attached pane names the Mac it is watching; and only then the
/// "missing" fallback, which must never read as local.
fn remote_target_label(
    id: &crate::hosts::ProfileId,
    profiles: &[crate::hosts::RemoteProfile],
    peers: &[crate::peers::PeerRecord],
) -> String {
    if let Some(profile) = profiles.iter().find(|profile| &profile.id == id) {
        return profile.label.clone();
    }
    if let Some(peer) = peers.iter().find(|peer| peer.id.0 == id.0) {
        return peer.label.clone();
    }
    format!("missing host ({})", id.0)
}

/// Whether the reviewer may observe this pane at all.
///
/// D4's reasoning, applied to the buddy. An attached pane is a VIEW of
/// another machine's terminal, and handing its contents to an agent process
/// running on THIS Mac exports that machine's screen as surely as
/// re-publishing it to the phone would. Refused rather than left to produce
/// an empty review: `TerminalPane::visible_text` reads the local
/// placeholder grid, so without this the reviewer is handed an empty screen
/// and comments confidently on nothing — plausible, wrong, and silent,
/// which is the shape this phase exists to stop.
///
/// The repo PROBE was already gated (`local_context_available`); the
/// utterance beside it was not.
fn may_review_pane(target: &crate::hosts::Target) -> bool {
    target.is_local()
}

/// The `Target` a pane attached to `peer` carries.
///
/// One function so the pane that is BUILT and the poller that is later
/// looked up cannot disagree about which peer a pane belongs to. The
/// mapping is deliberately the identity on the id string: `ProfileId` and
/// `PeerId` are both `generate_token()` output, and inventing a prefix here
/// would make the reverse lookup a parser rather than a comparison.
fn peer_target(peer: &crate::companion::auth::PeerId) -> crate::hosts::Target {
    crate::hosts::Target::Remote(crate::hosts::ProfileId(peer.0.clone()))
}

/// What the search sheet offers the focused pane.
#[derive(Debug, PartialEq, Eq)]
enum SearchOffer {
    /// A live field over a grid this Mac owns.
    Field,
    /// Disabled, with the reason shown in place of the field.
    Refused(&'static str),
}

/// D5, at the point of refusal. Search reads the grid the SESSION owns, and
/// an attached pane has none — so `set_search` was a no-op there. A box
/// that quietly swallows what is typed into it is worse than one that is
/// visibly disabled and says why, which is the whole reason this phase
/// exists.
fn search_offer(focused_pane_owns_grid: Option<bool>) -> SearchOffer {
    match focused_pane_owns_grid {
        Some(true) => SearchOffer::Field,
        Some(false) => SearchOffer::Refused(
            "search runs on the Mac that owns the terminal - not on an attached pane",
        ),
        None => SearchOffer::Refused("no terminal is focused"),
    }
}

/// What the peers sidebar shows under one peer.
#[derive(Debug, PartialEq)]
enum PeerListing {
    /// The endpoint probe has not finished.
    Probing,
    /// The probe failed, with `Reach`'s own reason.
    Unreachable(&'static str),
    /// Reachable; the first `/sessions` poll has not landed yet.
    Waiting,
    /// A poll completed and could not be read as a session list. NEVER
    /// folded into "sharing nothing": the same distinction `parse_sessions`
    /// exists to keep, one layer up. The likeliest cause is the `view`
    /// grant, which is the one thing a user can act on.
    Unreadable,
    /// The peer answered, and is sharing nothing with this Mac.
    Nothing,
    /// The sessions on offer.
    Sessions(Vec<crate::peer_client::sessions::PeerSession>),
}

/// Fold the endpoint probe and the last `/sessions` poll into one thing to
/// draw. Every failure mode gets its own answer, because the alternative —
/// an empty list — is what "pick a peer and see nothing happen" looks like.
/// How tall a bottom sheet may get in a window this tall, in pixels.
///
/// Split out from [`Workspace::sheet_max_height`] so the rule is testable:
/// a `Window` cannot be built outside a running app, and there is no gpui
/// test harness here.
fn sheet_max_px(viewport_height: f32) -> f32 {
    (viewport_height * 0.72).max(260.0)
}

/// How tall the settings sheet asks to be in a window this tall, in pixels.
///
/// `0.58` is the share of the window it wants -- over half, deliberately.
/// `320.0` is the floor that keeps the panel usable in a short window; it
/// can exceed the window, which is why this is only what the sheet ASKS
/// for. See [`settings_sheet_effective_px`] for what it measures.
fn settings_sheet_px(viewport_height: f32) -> f32 {
    (viewport_height * 0.58).max(320.0)
}

/// What the settings sheet actually measures: what it asks for, clamped by
/// the `max_h` every sheet carries.
///
/// Two numbers are in play and neither alone answers "how tall is it" --
/// below roughly a 444px window the shared cap is the smaller of the two
/// and wins, above it the fixed height does. Both are here so the answer
/// can be asserted rather than reasoned about at each call site.
fn settings_sheet_effective_px(viewport_height: f32) -> f32 {
    settings_sheet_px(viewport_height).min(sheet_max_px(viewport_height))
}

fn peer_listing(
    reach: Option<&crate::peer_client::discover::Reach>,
    poll: Option<crate::peer_client::sessions::LastPoll>,
) -> PeerListing {
    use crate::peer_client::sessions::LastPoll;
    let Some(reach) = reach else {
        return PeerListing::Probing;
    };
    if reach.endpoint().is_none() {
        return PeerListing::Unreachable(reach.note());
    }
    match poll {
        None | Some(LastPoll::Pending) => PeerListing::Waiting,
        Some(LastPoll::Failed) => PeerListing::Unreadable,
        Some(LastPoll::Listed(sessions)) => {
            // A retired session (`alive: false`) is one the broadcaster is
            // already tearing down. Offering it would open a pane straight
            // onto an ended terminal.
            let live: Vec<_> = sessions.into_iter().filter(|s| s.alive).collect();
            if live.is_empty() {
                PeerListing::Nothing
            } else {
                PeerListing::Sessions(live)
            }
        }
    }
}

/// A project row's SECOND line: what the project is, muted and smaller
/// than the name above it.
///
/// One helper for both row kinds. The live tabs and the remembered
/// projects are separate renderers, and a detail line that was 9px muted
/// in one and something else in the other is exactly the sibling drift
/// this file has produced repeatedly.
fn project_detail_line(text: String, theme: &'static Theme) -> impl IntoElement {
    div()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(px(9.0))
        .text_color(rgb(theme.ui_text_muted))
        .child(SharedString::from(text))
}

/// The activity a PROJECT row's dot reports: its terminals', reduced.
///
/// An empty set is `Unknown`, NOT `Activity::aggregate`'s `Idle`. That
/// difference is the whole function: a remembered project has no terminals
/// to observe, and `Idle` on its row would draw a green dot claiming a
/// shell is sitting at a prompt in a project that is not even open. Both
/// row kinds go through this, so the one closed-project rule cannot be
/// applied to one of them and missed on the other.
fn project_activity(terminals: &[Activity]) -> Activity {
    if terminals.is_empty() {
        return Activity::Unknown;
    }
    Activity::aggregate(terminals.iter().copied())
}

/// Heartbeats between per-project git refreshes. The heartbeat is 300ms,
/// so this is every 4.5 seconds — deliberately slower than the sidebar
/// poll it hangs off, because each refresh spawns two git processes PER
/// PROJECT and the branch a project sits on does not move on a sub-second
/// scale.
///
/// A MULTIPLE of that poll's own 3, and it has to be: the two gates are
/// nested, so `tick % 3 == 0 && tick % N == 0` fires at their lowest
/// common multiple. A co-prime N like 20 would have made this 18 seconds
/// rather than the 6 it read as.
const PROJECT_GIT_TICKS: u32 = 15;

/// The status dot a row leads with: one beside a peer's session, one
/// beside a project.
///
/// Green idle, yellow busy, hollow when there is no trustworthy signal.
/// The terminal rows' extra "quiet" cyan state is deliberately NOT here:
/// that one is derived from a single pane's LOCAL output timing, and
/// neither a peer's poll nor a project's aggregate observes it. Hollow for
/// `Unknown` for the same reason a colour is never invented — a filled dot
/// asserts a state, and `Unknown` is the absence of one.
///
/// One function for both callers on purpose: a dot that meant green-is-idle
/// in one list and something else in the other would be unreadable, and
/// two copies of these six lines is exactly how this codebase has drifted
/// siblings apart before.
/// Inset on BOTH sides of every sidebar row, project and nested alike.
///
/// Shared so a row nested under a project can never be wider than the card
/// it hangs under. Terminal rows used to carry no inset at all while the
/// project card had 4, which drew the child 8px WIDER than its parent.
const SIDEBAR_ROW_INSET: f32 = 4.0;
/// Padding between a row's own edge and its first mark.
const SIDEBAR_ROW_PAD: f32 = 6.0;
/// The fold triangle's column on a project row. A project with nothing to
/// fold still spends it, so every project's dot lands in one column.
const SIDEBAR_FOLD_W: f32 = 12.0;
/// Gap between the marks on a row.
const SIDEBAR_GAP: f32 = 6.0;
/// One step of nesting.
const SIDEBAR_INDENT: f32 = 14.0;

/// Distance from the sidebar's left edge to a row's leading dot, by
/// nesting depth: 0 is a project, 1 a window or a terminal hanging
/// straight off the project, 2 a terminal under a window row.
///
/// Depth 0 is not a free number -- it is where the project card's own box
/// lands its dot, walking its inset, padding, fold column and gap. Every
/// deeper row is measured from it, which is the property that keeps the
/// tree the right way up.
fn sidebar_bullet_x(depth: u8) -> f32 {
    SIDEBAR_ROW_INSET
        + SIDEBAR_ROW_PAD
        + SIDEBAR_FOLD_W
        + SIDEBAR_GAP
        + f32::from(depth) * SIDEBAR_INDENT
}

/// The left padding a nested row needs to land its dot at `depth`, given
/// the row itself already starts at the shared inset.
fn sidebar_child_pad_left(depth: u8) -> f32 {
    sidebar_bullet_x(depth) - SIDEBAR_ROW_INSET
}

fn activity_dot(activity: Activity, theme: &'static Theme) -> impl IntoElement {
    let color = match activity {
        Activity::Idle => theme.green,
        Activity::Busy => theme.yellow,
        Activity::Unknown => theme.ui_text_muted,
    };
    let hollow = matches!(activity, Activity::Unknown);
    div()
        .flex_none()
        .w(px(6.0))
        .h(px(6.0))
        .rounded(px(3.0))
        .when(hollow, |d| d.border_1().border_color(rgb(color)))
        .when(!hollow, |d| d.bg(rgb(color)))
}

/// The colour a project's generated mark sits on, resolved through the
/// ACTIVE theme.
///
/// The palette is the theme's own six hues, never a fixed set of hex
/// colours: a custom theme can be any palette at all, and a mark hard-coded
/// to look right against Tokyo Night would clash with it — or vanish into
/// it. The bright variants are deliberately left out; several presets
/// define them equal to their base colour (Tokyo Night's `bright_red` IS
/// its `red`), so including them would collapse twelve slots back into six
/// while pretending to spread further.
///
/// `projects::MARK_SLOTS` is the count this palette owes; the modulo keeps
/// a slot past the end wrapping rather than panicking a render.
fn project_mark_color(theme: &Theme, slot: usize) -> u32 {
    let palette = [
        theme.blue,
        theme.magenta,
        theme.cyan,
        theme.green,
        theme.yellow,
        theme.red,
    ];
    palette[slot % palette.len()]
}

/// One project's generated mark: its label's first character over its
/// hashed colour. `projects::project_mark` makes both decisions; this only
/// draws them.
fn project_mark_badge(
    mark: crate::projects::ProjectMark,
    theme: &'static Theme,
) -> impl IntoElement {
    div()
        .flex_none()
        .w(px(16.0))
        .h(px(16.0))
        .rounded(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(project_mark_color(theme, mark.slot)))
        .text_size(px(9.0))
        // The theme's own background, so the character reads against every
        // hue in the palette without picking a contrast colour per slot.
        .text_color(rgb(theme.ui_background))
        .child(SharedString::from(mark.ch.to_string()))
}

/// The tab label for a freshly opened peer terminal: the peer, then the
/// session it named itself. A session with no label still names its
/// machine, never a bare id the user has no way to recognise.
fn attached_tab_label(peer_label: &str, session_label: &str) -> String {
    if session_label.trim().is_empty() {
        peer_label.to_string()
    } else {
        format!("{peer_label} \u{b7} {session_label}")
    }
}

/// Which tab is active once the tab at `removed` is taken out and
/// `remaining` are left.
///
/// One function because there are two callers — `close_terminal` losing a
/// tab's last terminal and `close_tab` taking the whole tab — and the two
/// had the identical five lines written out twice. That duplication is
/// this codebase's recurring defect: a decision made at one site and
/// missed at its sibling.
///
/// `remaining == 0` is now a real answer rather than an impossible one.
/// Closing the last terminal leaves the workspace empty instead of
/// force-spawning a shell in `$HOME` that the user never asked for, so
/// this used to be reached only after that respawn had already made it
/// untrue. Zero is returned as a RESTING value, not a selection: with no
/// tabs, every read of `active_tab` goes through `Vec::get` and answers
/// `None` for it, and the next tab created overwrites it outright.
///
/// The old inline version's `self.tabs.len() - 1` on an empty `Vec` is a
/// usize underflow — a panic, not a wrong index.
fn active_tab_after_close(removed: usize, active: usize, remaining: usize) -> usize {
    if remaining == 0 {
        return 0;
    }
    // Removing a tab BEFORE the active one shifts every later index down;
    // follow the shift so the same tab stays selected.
    let shifted = active - usize::from(removed < active);
    shifted.min(remaining - 1)
}

/// Where a new terminal goes when the user asks for a WINDOW (cmd-n, and
/// the folder picker's fallback when the focused shell is busy).
#[derive(Debug, PartialEq, Clone, Copy)]
enum NewWindowTarget {
    /// Add a window to the project at this index.
    InTab(usize),
    /// There is no project to put a window in — open a tab instead.
    AsNewTab,
}

/// A window belongs to a project, and with nothing open there is no
/// project to put one in. cmd-n must still produce a terminal from the
/// empty state — a shortcut that silently does nothing is how a user gets
/// stuck in a workspace they cannot leave — so it opens a tab instead.
///
/// The folder picker already made this choice inline for its own fallback;
/// both go through here now so the two cannot drift apart.
fn new_window_target(active: usize, tabs: usize) -> NewWindowTarget {
    if active < tabs {
        NewWindowTarget::InTab(active)
    } else {
        NewWindowTarget::AsNewTab
    }
}

/// What the empty main area says under its heading, and whether it offers
/// to bring the projects list into view.
#[derive(Debug, PartialEq, Clone, Copy)]
struct EmptyState {
    /// The line beneath the heading. Never empty: this screen is the first
    /// thing a brand-new user sees, so there is always something true and
    /// useful to say.
    hint: &'static str,
    /// Whether to draw the button that opens the sidebar on projects.
    show_projects_button: bool,
}

/// The empty main area deliberately does NOT list projects: the sidebar
/// already lists them, with pins, reopen-on-click and the live tabs above
/// them, and a second copy in the middle of the window would be the same
/// rows twice with two sets of click behaviour to keep in step.
///
/// What the main area adds is the part the sidebar cannot say: that having
/// no terminal open is a state and not a failure, and where to go from it.
/// So it states the fact, offers the one action that always works (a new
/// terminal, with the key that does it), and otherwise only POINTS at the
/// sidebar.
///
/// This is also the launch screen — startup opens no terminal at all — so
/// the no-projects case is the one written for first, not the leftover:
///
/// * Nothing remembered — a first-ever launch, with an empty sidebar
///   beside it. The hint says what the empty sidebar is FOR, which is the
///   only thing that screen can honestly offer; the button is not drawn,
///   because opening an empty list is not a way forward.
/// * Remembered, sidebar already on projects — say where they are and draw
///   no button; the button would do nothing visible.
/// * Remembered, sidebar closed or on another view — the list is not on
///   screen, so offer to put it there.
fn empty_state(remembered_projects: usize, showing_projects: bool) -> EmptyState {
    if remembered_projects == 0 {
        return EmptyState {
            hint: "start one - the folders you work in are remembered here",
            show_projects_button: false,
        };
    }
    if showing_projects {
        EmptyState {
            hint: "pick up a project from the sidebar, or start fresh",
            show_projects_button: false,
        }
    } else {
        EmptyState {
            hint: "your projects are still remembered",
            show_projects_button: true,
        }
    }
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
    /// (cwd, activity) per terminal for the projects view, refreshed on the
    /// sidebar poll — rendering must never do per-pane process queries.
    sidebar_status_cache: HashMap<String, (String, Activity)>,
    /// Projects collapsed in the sidebar (by tab id).
    collapsed_projects: std::collections::HashSet<String>,
    /// Tabs the user has NAMED (by tab id), from any of the three places a
    /// rename can land: the inline field's Enter, the same field's
    /// click-away commit, and a rename asked for from the phone.
    ///
    /// A capture of one of these carries the user's own label and sets
    /// `Project::renamed`, which is the only thing that makes that flag
    /// mean anything: an auto-capture labels a project from its anchor
    /// folder's basename, so without this the very next close would
    /// quietly take the name back off it.
    ///
    /// Keyed by tab id and pruned in `prune_closed_tab_state` alongside
    /// `collapsed_projects`, so a future tab that reuses an id does not
    /// inherit someone else's rename.
    renamed_tabs: std::collections::HashSet<String>,
    /// When each open tab started accruing `active_secs` (unix seconds, by
    /// tab id): its creation, its reopen, or its last capture. Reset on
    /// capture rather than cleared, so a tab that is captured twice — the
    /// window closing AND the app quitting, say — cannot count the same
    /// minutes twice.
    tab_opened_at: HashMap<String, u64>,
    /// What each open tab remembers about its OWN panes (by tab id): every
    /// pane it has had, and where each of those panes was last seen.
    ///
    /// Capture reads this instead of the panes still alive at the moment
    /// the tab dies. A tab dies when its LAST terminal goes, so reading
    /// the survivors recorded a four-folder project closed pane by pane as
    /// a one-folder project — see `projects::TabPaneDirs`, which holds the
    /// decision and the reasoning.
    ///
    /// Keyed by tab id and pruned in `prune_closed_tab_state` exactly like
    /// `tab_opened_at`: the memory dies with the tab, so it can neither
    /// leak entries for tabs that no longer exist nor hand a future tab
    /// that reused an id someone else's folders.
    tab_pane_dirs: HashMap<String, crate::projects::TabPaneDirs>,
    /// The persisted store, read on the sidebar poll and never during
    /// render — the projects view redraws every frame and must not touch
    /// the filesystem to do it.
    projects_cache: crate::projects::ProjectStore,
    /// What the last reopen has to say for itself: which remembered
    /// folders were gone, and that their shells opened in `~` instead.
    /// `None` once a reopen finds every folder where it left it.
    projects_note: Option<String>,
    /// What each project row's SECOND line says about its repo, keyed by
    /// the project's anchor directory. Filled by `refresh_project_git` on
    /// a slow poll from the background executor and read synchronously by
    /// render — every git call shells out and can block, and a row redraws
    /// every frame (see `project_git`).
    ///
    /// Pruned against the projects the sidebar actually lists, so a branch
    /// can never be drawn for a folder that has left it.
    project_git: crate::project_git::GitCache,
    /// Anchors with a probe running, so a poll cannot stack a second git
    /// process on a project the previous one has not answered for yet. An
    /// anchor is removed from here by the probe that owns it; a probe that
    /// finds its slot already gone was pruned mid-flight and drops its
    /// answer rather than resurrecting an entry.
    /// Anchor -> the token of the probe that owns its slot. See the
    /// insertion site for why this is a token and not a presence flag.
    project_git_inflight: std::collections::HashMap<PathBuf, u64>,
    /// Monotonic source for those tokens.
    project_git_token: u64,
    /// The repo registry the project probes go through — interning, the
    /// per-repo action lock and the in-flight guard, exactly as the git
    /// panel uses for its own refresh.
    project_git_state: Arc<superterminal_core::git::GitState>,
    /// Per-terminal cue gates (bell → Ping, long-job finish → Glass).
    cue_gates: HashMap<String, superterminal_core::cue::CueGate>,
    /// The currently speaking `say` process (killed before a new note).
    tts_child: Option<std::process::Child>,
    /// Keep-awake `caffeinate -dimsu` child; Some = a hold is active.
    caffeinate_child: Option<std::process::Child>,
    /// Who wants the Mac awake: manual rail toggle and/or busy terminals.
    awake: crate::awake::AwakeHold,
    /// Phone companion: hub shared with panes while the server runs.
    companion_hub: Option<Arc<crate::companion::hub::Hub>>,
    companion_server: Option<crate::companion::server::ServerHandle>,
    /// The live preview catalog while the server runs; settings changes swap
    /// its watched dir in place (the server never restarts for that).
    companion_previews: Option<std::sync::Arc<crate::companion::previews::PreviewStore>>,
    companion_error: Option<String>,
    /// Which peers each still-live terminal is shared with. Outlives the
    /// companion (the `Hub` does not — it is rebuilt on every start and
    /// every forced restart) and is replayed into a freshly built hub in
    /// `companion_ui::prepare_companion_hub`. See `peers::BroadcastMap`'s
    /// doc comment for why this cannot live on the hub or be persisted to
    /// disk. Every production `hub.set_visible_to` call must mirror through
    /// here, and every terminal-removal path must prune it.
    broadcasts: crate::peers::BroadcastMap,
    /// Peers a per-terminal share toggle may offer at all: those with the
    /// `view` grant (`peers::shareable_peers`), cloned from `settings.peers`
    /// once here rather than on every projects-sidebar render — that render
    /// used to re-parse the peers JSON and clone every `PeerRecord`,
    /// including its secret, on every frame. Refreshed at startup
    /// (`Workspace::new`) and on the one path peers are ever mutated,
    /// `settings_ui::apply_peer_mutation`.
    shareable_peers_cache: Vec<crate::peers::PeerRecord>,
    /// Terminal ids whose inline share control is expanded in the sidebar.
    /// Purely a UI convenience (which panel is open) — never authority;
    /// pruned alongside `broadcasts` on every terminal-removal path so a
    /// reused id never opens already-expanded.
    share_open: std::collections::HashSet<String>,
    /// Terminal awaiting a window-aware focus handoff: phone spawns happen
    /// on the tick (no Window), so render — which has one — completes the
    /// focus, keeping keyboard focus consistent with the visible tab.
    companion_pending_focus: Option<String>,
    /// The workspace root awaiting the same window-aware handoff, set when
    /// the last terminal goes.
    ///
    /// Not every close path has a `Window`: a shell that EXITS on its own
    /// (typing `exit`, the commonest way to close a terminal) reaches
    /// `close_terminal` through a pane event, which has none. Focus would
    /// then be left on the pane that just died, and since gpui dispatches
    /// actions along the focus path, cmd-t would stop working on exactly
    /// the screen whose whole job is to offer a new terminal.
    pending_root_focus: bool,
    /// Phone-link flyout anchored to the rail icon.
    companion_flyout: bool,
    /// Transient "copied" confirmation on the flyout's copy chip. The
    /// generation ties each clear-timer to its own copy, and advances when
    /// the link is revoked — a stale "copied" must never vouch for a URL
    /// that no longer works.
    companion_copied: bool,
    companion_copy_gen: u64,
    /// Spawned afplay children, reaped on the poll (no zombies).
    audio_children: Vec<std::process::Child>,
    /// Installed `say` voices, loaded lazily for the alerts row.
    tts_voices: Option<Vec<String>>,
    tts_voices_loading: bool,
    /// Voice dropdown open in the alerts section.
    tts_voice_list_open: bool,
    settings_section: SettingsSection,
    /// Transient status line for the theme sheet (import/export results).
    theme_action_note: Option<String>,
    /// Buddy reviewer: latest note.
    buddy_note: Option<String>,
    /// The terminal the current note reviewed — "insert" targets it.
    buddy_source_pane: Option<String>,
    /// When the buddy speaks (diff stability, commits, reactions) — pure
    /// state machine in core; this file only wires probes and dispatches.
    buddy_gate: superterminal_core::buddy_gate::BuddyGate,
    /// The pane the gate's observations describe. A focus move invalidates
    /// location-bound gate state SYNCHRONOUSLY — otherwise a candidate from
    /// the old repo could dispatch with the new pane's terminal context.
    buddy_observed_pane: Option<String>,
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
    /// Tailnet peers found by the last scan (see `peers::scan_candidates`).
    peer_candidates: Vec<crate::peers::Candidate>,
    /// A scan is in flight — guards against stacking up scans.
    peer_scanning: bool,
    /// One scan has completed (even if it found nothing), so the settings
    /// sheet auto-scans at most once per session instead of re-triggering
    /// on every render of an empty result.
    peer_scanned_once: bool,
    /// The id, label, and secret of a peer just paired, shown once for QR
    /// transfer (the phone-link flyout's `qr::matrix` pattern) until
    /// dismissed or replaced by the next pairing. Keyed by id, not label —
    /// see `Workspace::delete_peer`.
    peer_pairing_secret: Option<(crate::companion::auth::PeerId, String, String)>,
    /// One `/sessions` poller per PEER we have a pane attached to — never
    /// one per pane. Two panes open on the same machine ask that machine
    /// one question, not two, and the answer is identical for both, so the
    /// poller is shared by `Arc` (see `peer_client::sessions`).
    ///
    /// The `Workspace` holds a clone alongside each pane's, so a peer whose
    /// last pane just closed still has its poller dropped by
    /// `prune_peer_pollers` on the next tick rather than lingering until
    /// something else happens to touch the map.
    peer_sessions: HashMap<crate::companion::auth::PeerId, Arc<PeerSessionPoller>>,
    /// Which peer's shared terminals the peers sidebar is listing, if any.
    /// Also keeps that peer's poller alive while nothing is attached yet —
    /// see `peer_pollers_needed`.
    peer_browse: Option<crate::companion::auth::PeerId>,
    /// Where each peer's companion was found, or why it was not. Absent
    /// means no probe has finished; the probe is one `/version` round trip
    /// per candidate port and always runs on the background executor.
    peer_reach: HashMap<crate::companion::auth::PeerId, crate::peer_client::discover::Reach>,
    /// Probes in flight, so a re-render (or an impatient second click)
    /// cannot stack a second scan on the same peer.
    peer_probing: std::collections::HashSet<crate::companion::auth::PeerId>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        // Before any pane spawns a shell: adapters ride the spawn env.
        crate::term_session::set_tool_adapters(settings.tool_adapters);
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
        let (paired_peers, _peer_problems) = settings.peers();
        let shareable_peers_cache: Vec<crate::peers::PeerRecord> =
            crate::peers::shareable_peers(&paired_peers)
                .into_iter()
                .cloned()
                .collect();
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
            renamed_tabs: std::collections::HashSet::new(),
            tab_opened_at: HashMap::new(),
            tab_pane_dirs: HashMap::new(),
            // Read once at startup: the sidebar opens on the projects view,
            // so the pinned and recent lists are there on the first frame
            // rather than after the first poll.
            projects_cache: crate::projects::ProjectStore::load(),
            projects_note: None,
            project_git: HashMap::new(),
            project_git_inflight: std::collections::HashMap::new(),
            project_git_token: 0,
            project_git_state: Arc::new(superterminal_core::git::GitState::default()),
            cue_gates: HashMap::new(),
            tts_child: None,
            caffeinate_child: None,
            awake: crate::awake::AwakeHold::default(),
            companion_hub: None,
            companion_server: None,
            companion_previews: None,
            companion_error: None,
            broadcasts: crate::peers::BroadcastMap::default(),
            shareable_peers_cache,
            share_open: std::collections::HashSet::new(),
            companion_pending_focus: None,
            pending_root_focus: false,
            companion_flyout: false,
            companion_copied: false,
            companion_copy_gen: 0,
            audio_children: Vec::new(),
            tts_voices: None,
            tts_voices_loading: false,
            tts_voice_list_open: false,
            settings_section: SettingsSection::DEFAULT,
            theme_action_note: None,
            buddy_note: None,
            buddy_source_pane: None,
            buddy_gate: superterminal_core::buddy_gate::BuddyGate::new(),
            buddy_observed_pane: None,
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
            peer_candidates: Vec::new(),
            peer_scanning: false,
            peer_scanned_once: false,
            peer_pairing_secret: None,
            peer_sessions: HashMap::new(),
            peer_browse: None,
            peer_reach: HashMap::new(),
            peer_probing: std::collections::HashSet::new(),
        };
        // First launch (or a healed save): persist the hatched identity so
        // the same pet comes back next session.
        if this.settings.buddy_companion.as_ref() != Some(&this.companion.save) {
            this.save_companion();
        }
        // Launch lands on the empty state. No `add_tab` here, and no
        // condition on one either.
        //
        // The obvious alternative — spawn a terminal only when the project
        // store is empty, so a first-ever launch has something — was
        // considered and rejected. It gives the app two startup paths, and
        // the one a user meets ONCE is the one that would never be
        // exercised again; the app would behave differently on day one
        // from every day after. It also contradicts the direction: the
        // home screen IS the product's front door, not a fallback for when
        // there is nothing to put on it.
        //
        // That makes the empty state's own "new terminal" affordance
        // load-bearing rather than a courtesy — it is the first thing a
        // new user ever sees, on a workspace with no projects and no
        // peers. `empty_state` is written for that case first.
        //
        // cmd-q does not close the window: it reaches AppKit's `terminate:`,
        // which fires `applicationWillTerminate:` -> gpui's quit observers
        // and only then clears the windows. `shutdown_all` hangs off
        // `on_window_closed` instead, so without THIS hook a project still
        // open at quit time would be the one case capture missed. The
        // callback does its work synchronously and returns an already-ready
        // future, so gpui's 100ms shutdown budget never bounds it.
        cx.on_app_quit(|ws: &mut Workspace, cx: &mut Context<Workspace>| {
            ws.record_open_projects(cx);
            async {}
        })
        .detach();
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

    /// Sample every pane's cue gate (~900ms): a bell from an unattended pane
    /// plays Ping, a job that worked 5s+ returning to the prompt plays Glass.
    /// Runs with audio cues OFF too — bells are then drained and discarded
    /// (never queued into stale dings) and the focused pane's long-job
    /// finishes still feed the buddy's reaction trigger.
    fn cue_tick(&mut self, cx: &mut Context<Self>) {
        use superterminal_core::cue::CueKind;
        let now = std::time::Instant::now();
        // Reap finished sound players so they never linger as zombies.
        self.audio_children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_)) | Err(_)));
        let audio_on = self.settings.audio_cues;
        let mut cue_kinds: Vec<&'static str> = Vec::new();
        let mut focused_long_job = false;
        for (id, pane) in &self.panes {
            let (activity, bell) =
                pane.update(cx, |pane, _| (pane.foreground_activity(), pane.take_bell()));
            let gate = self.cue_gates.entry(id.clone()).or_default();
            let outcome = gate.tick(now, activity, bell && audio_on);
            if outcome.long_job_finished {
                if self.focused_terminal.as_ref() == Some(id) {
                    focused_long_job = true;
                }
                // The phone's attention alerts diff this counter — cue-gate
                // semantics, so silent-but-running jobs never false-finish.
                if let Some(hub) = &self.companion_hub {
                    hub.bump_finished(id);
                }
            }
            if audio_on {
                if let Some(kind) = outcome.cue {
                    cue_kinds.push(match kind {
                        CueKind::Ping => "Ping",
                        CueKind::Glass => "Glass",
                    });
                }
            }
        }
        let live: std::collections::HashSet<&String> = self.panes.keys().collect();
        self.cue_gates.retain(|id, _| live.contains(id));
        // Each DISTINCT chime kind plays once, so every cued terminal's
        // sound is really heard even when several finish together.
        cue_kinds.sort_unstable();
        cue_kinds.dedup();
        for kind in cue_kinds {
            if let Some(child) = play_sound(kind) {
                self.audio_children.push(child);
            }
        }
        if focused_long_job {
            self.buddy_gate.job_finished(now);
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
        let capped = neutralize_say_commands(&text.chars().take(400).collect::<String>());
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

    /// Flip the manual keep-awake hold (the rail coffee toggle). While the
    /// auto hold is also active, releasing manual won't kill caffeinate —
    /// it lets go on its own once the terminals go quiet.
    fn toggle_manual_awake(&mut self) {
        self.awake.toggle_manual();
        self.sync_caffeinate();
    }

    /// Make the `caffeinate -dimsu` child match the hold state
    /// (idle/display/disk/system sleep prevented, user-active asserted).
    /// `-w` ties it to our pid so a crash or force-quit that skips
    /// shutdown_all can never leave the Mac held awake.
    fn sync_caffeinate(&mut self) {
        if self.awake.held() {
            if self.caffeinate_child.is_none() {
                self.caffeinate_child = std::process::Command::new("/usr/bin/caffeinate")
                    .arg("-dimsu")
                    .arg("-w")
                    .arg(std::process::id().to_string())
                    .spawn()
                    .ok();
            }
        } else if let Some(mut child) = self.caffeinate_child.take() {
            let _ = child.kill();
            let _ = child.wait(); // reap
        }
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
                        .filter(|out| out.status.success())
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

    /// The poller for one peer, spawning it the first time a pane attaches
    /// to that machine and handing out the SAME one to every pane after.
    ///
    /// `endpoint` is only used on a first spawn: an existing poller keeps
    /// the endpoint it was created with, so a peer whose address changed
    /// needs its pollers dropped (every pane on it closed) rather than
    /// re-derived here, and a caller must never assume this re-points one.
    #[allow(dead_code)] // the attach flow that calls this arrives in Task 7
    fn peer_session_poller(
        &mut self,
        peer: &crate::companion::auth::PeerId,
        endpoint: crate::peer_client::Endpoint,
    ) -> Arc<PeerSessionPoller> {
        if let Some(existing) = self.peer_sessions.get(peer) {
            return Arc::clone(existing);
        }
        let poller = crate::peer_client::sessions::spawn(peer.clone(), endpoint);
        self.peer_sessions.insert(peer.clone(), Arc::clone(&poller));
        poller
    }

    /// Open the peers sidebar on `peer`, or close it if it is already the
    /// one being shown. Kicks the endpoint probe the first time.
    fn browse_peer(&mut self, peer: crate::companion::auth::PeerId, cx: &mut Context<Self>) {
        if self.peer_browse.as_ref() == Some(&peer) {
            self.peer_browse = None;
        } else {
            self.peer_browse = Some(peer.clone());
            self.probe_peer(peer, cx);
        }
        cx.notify();
    }

    /// Forget what we know about how to reach `peer` and look again. The
    /// one control that recovers from a peer that has moved address, been
    /// restarted onto a different port, or simply was not running the first
    /// time it was asked — `peer_session_poller` deliberately never
    /// re-points an existing poller, so the workspace's clone goes too.
    ///
    /// An ALREADY-ATTACHED pane keeps the poller (and the endpoint) it was
    /// attached with: its own `Arc` outlives this removal, and re-pointing
    /// a live pane's stream at a newly discovered address is a reconnect,
    /// not a refresh. So a re-probe while a pane is open leaves that peer
    /// polled twice until the pane closes. Stated rather than prevented —
    /// the alternative is silently changing where an open pane is reading
    /// from.
    fn reprobe_peer(&mut self, peer: crate::companion::auth::PeerId, cx: &mut Context<Self>) {
        self.peer_reach.remove(&peer);
        self.peer_sessions.remove(&peer);
        self.probe_peer(peer, cx);
        cx.notify();
    }

    /// Find `peer`'s companion, on the background executor.
    ///
    /// EVERY call in here blocks: `discover::find` is up to eleven one-shot
    /// round trips, and `peers::scan_candidates` shells out to `tailscale`.
    /// Neither may run on the gpui thread — a peer that accepts a
    /// connection and then stalls would otherwise freeze every local pane
    /// too. Re-entrant calls are dropped rather than queued.
    fn probe_peer(&mut self, peer: crate::companion::auth::PeerId, cx: &mut Context<Self>) {
        if self.peer_reach.contains_key(&peer) || self.peer_probing.contains(&peer) {
            return;
        }
        let (peers, _problems) = self.settings.peers();
        let Some(record) = peers.iter().find(|p| p.id == peer).cloned() else {
            return;
        };
        // A peer record carries the tailnet HOST it was paired from; the
        // ADDRESS comes from a scan. With no scan yet there is nothing to
        // probe, so one is started and `scan_peer_candidates`' completion
        // comes back through `probe_browsed_peer`.
        let Some(candidate) = self
            .peer_candidates
            .iter()
            .find(|c| c.host == record.host)
            .cloned()
        else {
            self.scan_peer_candidates(cx);
            return;
        };
        self.peer_probing.insert(peer.clone());
        cx.spawn(async move |ws, cx| {
            let reach = cx
                .background_executor()
                .spawn(async move {
                    crate::peer_client::discover::find(&candidate.addr, &record.secret)
                })
                .await;
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                ws.peer_probing.remove(&peer);
                ws.peer_reach.insert(peer.clone(), reach);
                ws.ensure_peer_poller(&peer);
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// After a tailnet scan lands, probe whichever peer the sidebar is
    /// showing — the scan is what a first probe was waiting for.
    pub(super) fn probe_browsed_peer(&mut self, cx: &mut Context<Self>) {
        if let Some(peer) = self.peer_browse.clone() {
            self.probe_peer(peer, cx);
        }
    }

    /// Start (or reuse) the `/sessions` poller for a peer whose endpoint is
    /// known. A no-op while the probe has not produced one.
    fn ensure_peer_poller(&mut self, peer: &crate::companion::auth::PeerId) {
        let Some(endpoint) = self
            .peer_reach
            .get(peer)
            .and_then(|reach| reach.endpoint())
            .cloned()
        else {
            return;
        };
        let _ = self.peer_session_poller(peer, endpoint);
    }

    /// Open one of a peer's shared terminals as a pane in a new tab.
    ///
    /// The ONLY production path that produces a non-local pane. It builds
    /// the pane through `spawn_dead_pane` — which never touches a local
    /// shell — and only then hands it an attachment, so there is no instant
    /// at which a pane pointed at another Mac could have a shell on this
    /// one. Both background threads (the stream and the peer's `/sessions`
    /// poll) are started by the calls below and neither blocks here.
    fn open_peer_session(
        &mut self,
        peer: crate::companion::auth::PeerId,
        session_id: String,
        session_label: String,
        cx: &mut Context<Self>,
    ) {
        let Some(endpoint) = self
            .peer_reach
            .get(&peer)
            .and_then(|reach| reach.endpoint())
            .cloned()
        else {
            return;
        };
        let peer_label = self.peer_label(&peer);
        let target = peer_target(&peer);
        let terminal_id = self.fresh_id();
        self.spawn_dead_pane(terminal_id.clone(), target.clone(), cx);
        // `attach::spawn` and `sessions::spawn` both return immediately,
        // handing their blocking work to a thread of their own.
        let attachment = crate::peer_client::attach::spawn(endpoint.clone(), session_id);
        let poller = self.peer_session_poller(&peer, endpoint);
        if let Some(pane) = self.panes.get(&terminal_id).cloned() {
            // Frames and the session list together, never separately —
            // `set_attachment` says why, and it refuses a local pane.
            pane.update(cx, |pane, _| pane.set_attachment(attachment, poller));
        }
        let tab_id = format!("tab-{}", self.next_id);
        self.next_id += 1;
        self.mark_tab_opened(&tab_id);
        self.tabs.push(Tab::single(
            tab_id,
            attached_tab_label(&peer_label, &session_label),
            PaneNode::terminal_at(&terminal_id, target),
        ));
        self.active_tab = self.tabs.len() - 1;
        self.set_focused_terminal(Some(terminal_id), cx);
        cx.notify();
    }

    /// A paired peer's user-facing label, or its id when the record has
    /// gone (deleted while a pane on it was open).
    fn peer_label(&self, peer: &crate::companion::auth::PeerId) -> String {
        let (peers, _problems) = self.settings.peers();
        peers
            .iter()
            .find(|p| &p.id == peer)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| peer.0.clone())
    }

    /// Stop polling peers nothing needs any more. Cheap and unconditional:
    /// with no attached panes and no peer being browsed the map is empty
    /// and this returns before touching a single pane.
    fn prune_peer_pollers(&mut self, cx: &App) {
        if self.peer_sessions.is_empty() {
            return;
        }
        let attached: Vec<crate::companion::auth::PeerId> = self
            .panes
            .values()
            .filter_map(|pane| pane.read(cx).attached_peer())
            .collect();
        // The sidebar's open peer counts too: its session list IS this
        // poller's output, so pruning on attachment alone would stop the
        // list the user is reading. See `peer_pollers_needed`.
        let needed = peer_pollers_needed(&attached, self.peer_browse.as_ref());
        let held: Vec<crate::companion::auth::PeerId> =
            self.peer_sessions.keys().cloned().collect();
        for peer in pollers_to_drop(&held, &needed) {
            self.peer_sessions.remove(&peer);
        }
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
        // A caffeinate killed externally must not leave the toggle lying —
        // checked on the unconditional heartbeat, not the cue tick, so it
        // holds with audio cues off. Only a definite exit clears the state;
        // on a try_wait error the process may still live, so keep the handle.
        if let Some(child) = &mut self.caffeinate_child {
            if matches!(child.try_wait(), Ok(Some(_))) {
                self.caffeinate_child = None;
                // Manual-only hold: honor the kill. With the auto hold
                // active the child respawns right away regardless, so
                // keep the manual latch instead of silently losing it.
                if !self.awake.auto_held() {
                    self.awake.clear_manual();
                }
                self.sync_caffeinate();
                cx.notify();
            }
        }
        // Peers nothing is attached to any more stop being polled. On the
        // same ~900ms cadence as the other sweeps, and independent of the
        // companion server: this is us calling OUT to a peer, which has
        // nothing to do with whether we are serving anything ourselves.
        if self.pet_tick_count.is_multiple_of(3) {
            self.prune_peer_pollers(cx);
        }
        // Auto keep-awake: probe on the ~900ms cadence (one ioctl per pane,
        // skipped entirely while the setting is off). The hold machine keeps
        // a 10s idle grace so back-to-back commands don't flap caffeinate.
        if self.pet_tick_count.is_multiple_of(3) {
            let activity = if self.settings.auto_caffeinate {
                Activity::aggregate(
                    self.panes
                        .values()
                        .map(|pane| pane.read(cx).foreground_activity()),
                )
            } else {
                Activity::Idle
            };
            let was_held = self.caffeinate_child.is_some();
            self.awake.tick(
                self.settings.auto_caffeinate,
                activity,
                std::time::Instant::now(),
            );
            self.sync_caffeinate();
            if was_held != self.caffeinate_child.is_some() {
                cx.notify();
            }
        }
        // Keep the sidebar following the focused terminal's cwd (a `cd`
        // changes it without any focus event) — INDEPENDENT of pet
        // visibility. The panels dedupe unchanged paths, so this is cheap.
        if self.pet_tick_count.is_multiple_of(3) {
            self.cue_tick(cx);
        }
        // Phone-requested terminals: PTY spawn is main-thread-only, so the
        // server queues and this tick materializes — same flow as the Mac's
        // own "+" (new tab, focused).
        if let Some(hub) = self.companion_hub.clone() {
            for request in hub.drain_spawns() {
                self.add_tab(None, cx);
                // add_tab set the logical focus; the gpui FocusHandle needs
                // a Window, which the next render supplies.
                self.companion_pending_focus = self.focused_terminal.clone();
                if let crate::companion::auth::Principal::Peer(peer_id) = request.principal {
                    // A peer-spawned terminal is visible to exactly the
                    // peer that asked, never broadcast to every paired
                    // peer. No UI path produces a peer request yet, so this
                    // arm is unreachable in this phase — it must still do
                    // the right thing once one does.
                    //
                    // D3e, DISCHARGED HERE. `BroadcastMap::share` refuses
                    // nothing (its doc says why), and this call site was
                    // safe only because `spawn_pane` structurally could not
                    // produce a non-local pane — an invariant nothing
                    // encoded. This task produces a second shape of pane
                    // (`open_peer_session` → `spawn_dead_pane` with a
                    // `Target::Remote`), so the invariant is now checked
                    // rather than assumed: sharing a pane that is itself a
                    // VIEW of another Mac would offer a peer a remote view
                    // of a remote view, attributed to this machine.
                    //
                    // `add_tab` above is still local-only, so this gate is
                    // a backstop today, not a live filter. That is the
                    // point — it stays correct when `add_tab` is not.
                    let shareable_id = self.companion_pending_focus.clone().filter(|id| {
                        self.panes
                            .get(id)
                            .is_some_and(|pane| may_share_terminal(pane.read(cx).target()))
                    });
                    if let Some(new_id) = shareable_id {
                        hub.set_visible_to(&new_id, &peer_id, true);
                        // Mirrored so this survives a companion restart —
                        // the hub itself is rebuilt from scratch on every
                        // one (see `peers::BroadcastMap`'s doc comment).
                        self.broadcasts.share(&new_id, &peer_id);
                    }
                }
            }
            // Phone-requested renames: tab state is main-thread-only. The
            // rename lands on the TAB holding that terminal (same label the
            // Mac's inline rename edits); the metadata sweep below
            // republishes it to the phone.
            for (terminal_id, label) in hub.take_renames() {
                // Rename site 3 of 3 (from the phone). The tab id is
                // collected rather than marked in place: `self.tabs` is
                // borrowed mutably here, and a name given from the phone
                // is no less the user's than one typed on the Mac.
                let mut named = None;
                for tab in &mut self.tabs {
                    if tab.all_terminal_ids().iter().any(|id| *id == terminal_id) {
                        tab.label = label;
                        named = Some(tab.id.clone());
                        cx.notify();
                        break;
                    }
                }
                self.renamed_tabs.extend(named);
            }
            // Phone-requested closes: tearing down a PTY is main-thread work,
            // so it goes through the same path as closing from the Mac.
            for terminal_id in hub.take_closes() {
                self.close_terminal(&terminal_id, cx);
            }
        }
        if self.pet_tick_count.is_multiple_of(3) {
            if let (Some(hub), Some(handle)) = (&self.companion_hub, &self.companion_server) {
                for (id, pane) in &self.panes {
                    let label = {
                        // Label lookup needs &self only.
                        let mut found = id.clone();
                        for tab in &self.tabs {
                            let ids = tab.all_terminal_ids();
                            if let Some(position) = ids.iter().position(|tid| tid == id) {
                                found = if ids.len() == 1 {
                                    tab.label.clone()
                                } else {
                                    format!("{} · {}", tab.label, position + 1)
                                };
                                break;
                            }
                        }
                        found
                    };
                    // The sibling of the publish gate, and the sixth
                    // instance of this phase's recurring bug: this was
                    // ungated and safe only because `spawn_dead_pane` never
                    // registers a remote id — true by coincidence, which is
                    // exactly what D3e exists to remove. Without it, giving
                    // a remote pane an id for any future reason would
                    // announce the PEER's activity in this Mac's session
                    // list on the phone.
                    if !may_share_terminal(pane.read(cx).target()) {
                        continue;
                    }
                    let activity = pane.read(cx).companion_activity();
                    hub.set_meta_activity(id, &label, true, activity);
                }
                // Prune entries whose pane is gone: their streams end and
                // further input turns 404 (they answered 410 since retire).
                for id in hub.ids() {
                    if !self.panes.contains_key(&id) {
                        hub.unregister(&id);
                    }
                }
                // The bound address disappearing (Tailscale off) must never
                // leave the toggle claiming "on".
                let bound_ip = handle
                    .url
                    .trim_start_matches("http://")
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let live = crate::companion::net::tailnet_ipv4()
                    .map(|ip| ip.to_string() == bound_ip)
                    .unwrap_or(false);
                if !live {
                    self.stop_companion(cx);
                    self.companion_error =
                        Some("Tailscale interface lost — companion stopped".to_string());
                    cx.notify();
                }
            }
        }
        if self.sidebar_open && self.pet_tick_count.is_multiple_of(3) {
            self.push_git_cwd(cx);
            // The projects view's activity dots decay with time alone and
            // its cwd column comes from this cache — refresh both on the
            // poll while it's open, never during render.
            if self.sidebar_view == SidebarView::Projects {
                // Pinned and recent come from the file, and the file is
                // written by other paths (a capture, a quit). Re-read it on
                // the poll, never during render.
                self.projects_cache = crate::projects::ProjectStore::load();
                // Which projects are OPEN decides what both remembered
                // sections hide, and a
                // shell that has just `cd`ed can change that. Fold here for
                // the same reason the cache is reloaded here: the render
                // must not go asking panes anything.
                self.remember_pane_dirs(cx);
                let home = std::env::var("HOME").unwrap_or_default();
                self.sidebar_status_cache = self
                    .panes
                    .iter()
                    .map(|(id, pane)| {
                        let (cwd, activity) = pane.read(cx).status_activity();
                        let cwd = cwd.map(|cwd| cwd.replace(&home, "~")).unwrap_or_default();
                        (id.clone(), (cwd, activity))
                    })
                    .collect();
                // The rows' git lines, on their own slower gate: this one
                // spawns git PROCESSES, and a branch does not move on the
                // same scale a cwd does.
                if self.pet_tick_count.is_multiple_of(PROJECT_GIT_TICKS) {
                    self.refresh_project_git(cx);
                }
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

    /// Buddy-as-reviewer/companion: observe the focused pane's repo every
    /// tick (single-flight background probes feeding the pure gate in core),
    /// and dispatch at most one utterance at a time — a review of a settled
    /// working diff, a review of a fresh commit, or an in-character reaction
    /// to a finished job.
    fn buddy_tick(&mut self, cx: &mut Context<Self>) {
        use superterminal_core::buddy_gate::{Trigger, Utterance};
        if !self.settings.buddy_enabled || self.settings.buddy_command.trim().is_empty() {
            return;
        }
        let focused_id = self.focused_terminal.clone();
        if self.buddy_observed_pane != focused_id {
            self.buddy_gate.focus_changed();
            self.buddy_observed_pane = focused_id.clone();
        }
        let Some(pane) = focused_id.as_ref().and_then(|id| self.panes.get(id)) else {
            return;
        };
        let (quiet, cwd, target, text) = {
            let pane_ref = pane.read(cx);
            // Quiet = no PTY output AND no keystrokes for 3s. The input clock
            // covers echo-less typing (password prompts) that the output
            // clock cannot see.
            let quiet = pane_ref.last_activity.elapsed() >= Duration::from_secs(3)
                && pane_ref.last_input.elapsed() >= Duration::from_secs(3);
            (
                quiet,
                pane_ref.cwd(),
                pane_ref.target().clone(),
                pane_ref.visible_text(),
            )
        };
        // An attached pane is not this Mac's to review. See
        // `may_review_pane` — the probe below was already gated on the
        // target; the utterance was not.
        if !may_review_pane(&target) {
            return;
        }
        // Observation never stops for an in-flight utterance; probes are
        // single-flight on their own (one can outlive several ticks on a
        // slow repo; the gate drops stale generations).
        //
        // A remote pane has no local repo to probe: skip entirely rather
        // than probing the Mac's tree while the user is on another host,
        // which would produce confidently wrong buddy notes.
        if quiet && local_context_available(&target, cwd.clone()) {
            if let Some(generation) = self.buddy_gate.want_probe() {
                let cwd = cwd.expect("local_context_available guarantees a cwd");
                cx.spawn(async move |ws, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            superterminal_core::buddy_probe::observe(std::path::Path::new(&cwd))
                        })
                        .await;
                    let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                        ws.buddy_gate
                            .probe_done(generation, result, std::time::Instant::now());
                        // Composing state may have changed.
                        cx.notify();
                    });
                    Ok::<(), ()>(())
                })
                .detach();
            }
        }
        if self
            .buddy_backoff_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        let Some(utterance) = self.buddy_gate.take_dispatch(std::time::Instant::now()) else {
            return;
        };
        let tail: String = text
            .chars()
            .rev()
            .take(2000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let name = self
            .settings
            .buddy_companion
            .as_ref()
            .map(|companion| companion.name.clone())
            .unwrap_or_else(|| "Buddy".to_string());
        let source_pane = self.focused_terminal.clone();
        let command = self.settings.buddy_command.clone();
        let args = self.settings.buddy_args.clone();
        cx.spawn(async move |ws, cx| {
            enum Prepared {
                Review {
                    root: String,
                    committed: bool,
                    patch: String,
                    hash: u64,
                },
                Reaction,
                /// Nothing to show (clean tree, merge commit, unreadable).
                Cancel,
                /// The tree moved between approval and the git read — the
                /// materialized patch is NOT the approved snapshot.
                Stale,
            }
            // Stage 1 (background): materialize the utterance's content.
            let prepared = cx
                .background_executor()
                .spawn(async move {
                    match utterance {
                        Utterance::Review(root, trigger) => {
                            let committed = matches!(trigger, Trigger::Commit(_));
                            let patch = match &trigger {
                                Trigger::WorkingDiff(_) => {
                                    superterminal_core::buddy_probe::working_patch(
                                        std::path::Path::new(&root),
                                    )
                                }
                                Trigger::Commit(oid) => {
                                    superterminal_core::buddy_probe::commit_patch(
                                        std::path::Path::new(&root),
                                        oid,
                                    )
                                }
                            };
                            // TOCTOU guard: the patch was read from the LIVE
                            // tree. Re-observe and require it to still be the
                            // approved snapshot — editing may have resumed
                            // between 7s-stability approval and the git read.
                            if let Trigger::WorkingDiff(approved) = &trigger {
                                let fresh = superterminal_core::buddy_probe::observe(
                                    std::path::Path::new(&root),
                                );
                                if fresh.snapshot_hash != Some(*approved) {
                                    return Prepared::Stale;
                                }
                            }
                            match patch {
                                Some((patch, hash)) => Prepared::Review {
                                    root,
                                    committed,
                                    patch,
                                    hash,
                                },
                                None => Prepared::Cancel,
                            }
                        }
                        Utterance::Reaction => Prepared::Reaction,
                    }
                })
                .await;
            // Stage 2 (main): dedupe — a commit of the just-reviewed diff
            // (or vice versa) must not be reviewed twice.
            let proceed = ws.update(cx, |ws: &mut Workspace, _| match &prepared {
                Prepared::Review { root, hash, .. } => {
                    if ws.buddy_gate.already_reviewed(root, *hash) {
                        ws.buddy_gate.utterance_cancelled();
                        false
                    } else {
                        true
                    }
                }
                Prepared::Reaction => true,
                Prepared::Cancel => {
                    ws.buddy_gate.utterance_cancelled();
                    false
                }
                Prepared::Stale => {
                    // Un-consume WITHOUT backoff: once the tree holds still
                    // again, the normal stability path re-candidates it.
                    ws.buddy_gate.utterance_failed();
                    false
                }
            });
            if !matches!(proceed, Ok(true)) {
                return Ok(());
            }
            let (prompt, reviewed) = match prepared {
                Prepared::Review {
                    root,
                    committed,
                    patch,
                    hash,
                } => {
                    let excerpt: String = patch.chars().take(6000).collect();
                    let label = if committed {
                        "JUST-COMMITTED CHANGE"
                    } else {
                        "WORKING DIFF"
                    };
                    (
                        buddy_prompt(&name, &tail, Some((label, &excerpt))),
                        Some((root, hash)),
                    )
                }
                Prepared::Reaction => (buddy_prompt(&name, &tail, None), None),
                Prepared::Cancel | Prepared::Stale => unreachable!("cancelled above"),
            };
            // Stage 3 (background): run the agent CLI.
            let result = cx
                .background_executor()
                .spawn(async move {
                    superterminal_core::buddy::run(superterminal_core::buddy::BuddyRequest {
                        command,
                        args,
                        prompt,
                        timeout_ms: Some(30_000),
                    })
                })
                .await;
            // Stage 4 (main): surface the note or back off.
            let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                if result.ok {
                    ws.buddy_gate.utterance_succeeded(reviewed);
                    ws.buddy_backoff_until = None;
                    ws.buddy_note = Some(result.text.clone());
                    ws.buddy_source_pane = source_pane;
                    if ws.settings.buddy_tts {
                        ws.speak_note(&result.text);
                    }
                    ws.pet_bubble = Some((result.text, std::time::Instant::now()));
                } else {
                    // A timeout or launch failure requeues the review (only
                    // success consumes content) and holds off retries so a
                    // broken command can't respawn every tick.
                    ws.buddy_gate.utterance_failed();
                    ws.buddy_backoff_until =
                        Some(std::time::Instant::now() + Duration::from_secs(30));
                }
                cx.notify();
            });
            Ok::<(), ()>(())
        })
        .detach();
    }

    /// Collect shutdown handles for every live pane plus any pending ones.
    /// The caller joins them OFF the UI thread with a bounded deadline.
    pub fn shutdown_all(&mut self, cx: &mut Context<Self>) -> Vec<ShutdownHandle> {
        // Every open project is about to cease to exist. Remember them all
        // before a single shell is killed below.
        self.record_open_projects(cx);
        // Companion first: cancel streams before their sessions die.
        self.stop_companion(cx);
        // Flush a debounced pet-count save so quitting mid-pet loses nothing.
        if self.pet_save_at.take().is_some() {
            self.save_companion();
        }
        // Never leave a stray caffeinate holding the Mac awake after quit.
        if let Some(mut child) = self.caffeinate_child.take() {
            let _ = child.kill();
            let _ = child.wait();
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
        self.spawn_pane_restoring(id, cwd, None, cx)
    }

    /// `spawn_pane`, with the terminal's stored text already above its
    /// first prompt.
    ///
    /// The shell is spawned exactly as it always was and is genuinely
    /// live; the restored rows are printed into the terminal's own parser
    /// before it says anything, so they land in scrollback rather than
    /// over the top of it (see [`TerminalPane::new`]). A
    /// `restore_key` of `None`, or one with no readable file behind it,
    /// gives back precisely `spawn_pane`.
    fn spawn_pane_restoring(
        &mut self,
        id: String,
        cwd: Option<PathBuf>,
        restore_key: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        let pane_id = id.clone();
        let hub = Arc::clone(&self.broadcast);
        let pane = cx.new(|pane_cx| {
            TerminalPane::new(
                pane_id,
                cwd,
                restore_key.as_deref(),
                theme,
                family,
                size,
                hub,
                pane_cx,
            )
        });
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
        if let Some(hub) = &self.companion_hub {
            let label = self.companion_label_for(&id);
            pane.update(cx, |pane, _| {
                if let Some(sender) = pane.input_sender() {
                    // Origin is stated, never inferred from `input_sender()`
                    // being Some: an attached pane forwards keystrokes and so
                    // also has a sender. See `Origin` in companion/hub.rs.
                    hub.register_with_origin(
                        &pane.id,
                        &label,
                        sender,
                        crate::companion::hub::Origin::LocalPty,
                    );
                }
                pane.set_companion(Some(Arc::clone(hub)));
            });
        }
        self.panes.insert(id, pane.clone());
        pane
    }

    /// A pane for a saved `target` that this slice cannot (or can no
    /// longer) reach: constructed through `TerminalPane::dead`, which never
    /// touches a local shell, unlike `spawn_pane`/`TerminalPane::new`.
    fn spawn_dead_pane(
        &mut self,
        id: String,
        target: crate::hosts::Target,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPane> {
        let theme = self.theme;
        let family = self.settings.font_family.clone();
        let size = self.settings.font_size;
        let translucent = self.settings.background_image.is_some();
        let pane_id = id.clone();
        let hub = Arc::clone(&self.broadcast);
        let pane = cx
            .new(|pane_cx| TerminalPane::dead(pane_id, target, theme, family, size, hub, pane_cx));
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
        // No session, so no input sender to register with the companion hub —
        // a dead pane has nothing to mirror or type into.
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
                self.set_focused_terminal(Some(pane_id), cx);
                self.push_git_cwd(cx);
                cx.notify();
            }
            PaneEvent::TitleChanged => cx.notify(),
            PaneEvent::Exited => {
                self.close_terminal(&pane_id, cx);
            }
        }
    }

    /// Route keyboard focus to the currently-focused terminal's pane, or
    /// to the workspace itself when there is no terminal at all.
    ///
    /// The fallback is what keeps the empty state escapable. gpui
    /// dispatches actions along the FOCUS path, and every binding —
    /// cmd-t, cmd-n, cmd-o, the settings sheet — is registered on the
    /// workspace root, which `track_focus` puts on that path only while it
    /// holds focus. This used to do nothing without a pane, which was
    /// invisible while a pane always existed: startup opened one and
    /// closing the last respawned one. Now launch lands here, and so does
    /// closing the last terminal — with focus otherwise left on a pane
    /// that was just torn down, the shortcut that gets the user OUT would
    /// be the one that stopped working.
    pub fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
        {
            pane.read(cx).focus(window);
        } else {
            window.focus(&self.focus_handle);
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
        self.set_focused_terminal(Some(terminal_id), cx);
        self.push_git_cwd(cx);
        cx.notify();
    }

    fn add_tab(&mut self, cwd: Option<PathBuf>, cx: &mut Context<Self>) {
        let terminal_id = self.fresh_id();
        self.spawn_pane(terminal_id.clone(), cwd, cx);
        let tab_id = format!("tab-{}", self.next_id);
        self.next_id += 1;
        self.mark_tab_opened(&tab_id);
        self.tabs.push(Tab::single(
            tab_id,
            "terminal",
            PaneNode::terminal(&terminal_id),
        ));
        self.active_tab = self.tabs.len() - 1;
        self.set_focused_terminal(Some(terminal_id), cx);
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
        self.set_focused_terminal(Some(new_id), cx);
        cx.notify();
    }

    /// Drop per-tab state for projects that no longer exist: the sidebar's
    /// collapsed set, the set of tabs the user has renamed, the
    /// `active_secs` marks and each tab's memory of its own panes alike.
    /// All four are keyed by tab id, so all four would otherwise hand a
    /// future tab that reused the id someone else's state — a collapsed
    /// row, a name the user never gave it, an uptime measured from a
    /// project that closed hours ago, or another project's folders.
    fn prune_closed_tab_state(&mut self) {
        let live: std::collections::HashSet<String> =
            self.tabs.iter().map(|tab| tab.id.clone()).collect();
        self.collapsed_projects.retain(|id| live.contains(id));
        self.renamed_tabs.retain(|id| live.contains(id));
        self.tab_opened_at.retain(|id, _| live.contains(id));
        self.tab_pane_dirs.retain(|id, _| live.contains(id));
    }

    /// Start (or restart) a tab's `active_secs` clock. Every path that
    /// brings a tab into existence calls this: `add_tab`, the peer-attach
    /// tab, the `load_session` rebuild and `open_project`.
    fn mark_tab_opened(&mut self, tab_id: &str) {
        self.tab_opened_at
            .insert(tab_id.to_string(), crate::projects::now_secs());
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

    /// Everything a tab removal leaves to fix up, in ONE place: the
    /// in-progress rename's index, the per-tab state keyed by tab id, the
    /// active index, and where focus lands.
    ///
    /// Call immediately after `self.tabs.remove(removed)`, with
    /// `was_active` computed BEFORE it. Both close paths — the one that
    /// loses a tab's last terminal and the one that takes the whole tab —
    /// had these five steps written out separately, which is exactly the
    /// shape of bug this repo keeps producing: the empty case was fixed in
    /// one and would have been missed in the other.
    ///
    /// An empty workspace is a legal resting state now, so the branch that
    /// used to respawn a shell in `$HOME` instead lets focus go. Nothing
    /// is left pointing at a pane that no longer exists: with no tabs
    /// there are no panes, so `None` is the only honest answer, and the
    /// panels retarget to `Detached` through `set_focused_terminal`.
    fn settle_after_tab_removal(
        &mut self,
        removed: usize,
        was_active: bool,
        cx: &mut Context<Self>,
    ) {
        self.fix_rename_after_removal(removed);
        self.prune_closed_tab_state();
        self.active_tab = active_tab_after_close(removed, self.active_tab, self.tabs.len());
        if self.tabs.is_empty() {
            self.set_focused_terminal(None, cx);
            self.pending_root_focus = true;
        } else if was_active {
            // Only an active-tab close moves focus.
            let next = collect_terminal_ids(self.tabs[self.active_tab].active_pane())
                .into_iter()
                .next();
            self.set_focused_terminal(next, cx);
        }
    }

    // --- projects: remembering a tab before it disappears ---

    /// Fold what every open tab's panes are doing right now into that
    /// tab's own memory of itself (`tab_pane_dirs`).
    ///
    /// Uses `last_known_cwd`, not `cwd`. A shell that has already exited —
    /// which is what typing `exit`, the commonest way to close a terminal,
    /// leaves behind — reports no cwd at all, because the process whose
    /// directory it would read is gone. Reading `cwd()` therefore silently
    /// dropped exactly those projects. The session keeps its last
    /// successful reading for this, refreshed on a slow tick.
    ///
    /// Every path that is about to make a pane unreadable calls this
    /// FIRST: a pane already dropped from `self.panes` cannot answer at
    /// all, cache or no cache. `close_terminal` calls it at its very top —
    /// including for a close that does NOT kill the tab, which is the
    /// whole point: that pane's folder has to be remembered now, because
    /// nothing will be able to ask it again.
    ///
    /// Folding rather than replacing is what makes a project the union of
    /// its panes instead of a snapshot of its survivors; `TabPaneDirs`
    /// holds that decision, and is where it is tested.
    fn remember_pane_dirs(&mut self, cx: &App) {
        type Observed = (String, crate::hosts::Target, Option<String>);
        // Collected first: writing into `tab_pane_dirs` needs `&mut self`,
        // and reading the panes borrows `self` immutably.
        let observed: Vec<(String, Vec<Observed>)> = self
            .tabs
            .iter()
            .map(|tab| {
                let panes = tab
                    .all_terminal_targets()
                    .into_iter()
                    .map(|(terminal_id, target)| {
                        let cwd = self
                            .panes
                            .get(&terminal_id)
                            .and_then(|pane| pane.read(cx).last_known_cwd());
                        (terminal_id, target, cwd)
                    })
                    .collect();
                (tab.id.clone(), panes)
            })
            .collect();
        for (tab_id, panes) in observed {
            let remembered = self.tab_pane_dirs.entry(tab_id).or_default();
            for (terminal_id, target, cwd) in panes {
                remembered.saw(&terminal_id, &target, cwd);
            }
        }
    }

    /// Every directory `tab` has had a local pane in, in first-seen pane
    /// order — its panes' union, not the survivors' snapshot.
    ///
    /// The decision itself — which panes count, which directory each one
    /// offers, dedupe, order — lives in `projects::TabPaneDirs` and
    /// `projects::project_dirs`, where it is testable without a gpui
    /// harness.
    fn tab_dirs(&self, tab: &Tab) -> Vec<PathBuf> {
        self.tab_pane_dirs
            .get(&tab.id)
            .map(|remembered| remembered.dirs())
            .unwrap_or_default()
    }

    /// Refresh the per-project git cache: one probe per project the
    /// sidebar can draw, on the background executor.
    ///
    /// Never per render and never on the UI thread — every git call shells
    /// out and can block (see `project_git`). Run from the sidebar poll
    /// only while the projects view is actually open, on a slower gate
    /// than the rest of that poll: this is two `git` processes per project
    /// (`rev-parse`, then `status`), and the branch a project is on does
    /// not change on a 900ms scale.
    ///
    /// Single-flight per anchor: a project whose previous probe has not
    /// answered is skipped rather than given a second git process, so a
    /// slow repo cannot stack them up poll after poll.
    fn refresh_project_git(&mut self, cx: &mut Context<Self>) {
        let mut anchors: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for tab in &self.tabs {
            let dirs = self.tab_dirs(tab);
            // The SAME key its row draws with — the tab's matched record
            // first, its folders' derivation only as a fallback. See
            // `project_git::row_anchor` for why deriving here would probe
            // a different folder from the remembered row's.
            let record = self.projects_cache.matching(&dirs);
            if let Some(anchor) = crate::project_git::row_anchor(&dirs, record) {
                anchors.insert(anchor.clone());
            }
        }
        // The remembered projects the sidebar can LIST, not every record
        // the store holds: `recent()` is capped, and probing records past
        // the cap would spend git processes on rows nothing draws.
        for project in self
            .projects_cache
            .pinned()
            .into_iter()
            .chain(self.projects_cache.recent())
        {
            if let Some(anchor) = crate::project_git::row_anchor(&project.dirs, Some(project)) {
                anchors.insert(anchor.clone());
            }
        }
        // An entry is dropped the moment its project leaves the list, so a
        // stale branch can never be drawn for a folder that has gone.
        crate::project_git::prune(&mut self.project_git, &anchors);
        // A probe whose slot is pruned here lands to find it gone — or
        // reissued — and drops its answer rather than resurrecting the
        // entry.
        self.project_git_inflight
            .retain(|anchor, _| anchors.contains(anchor));
        for anchor in anchors {
            if self.project_git_inflight.contains_key(&anchor) {
                continue; // already asked; not asked twice
            }
            // A TOKEN, not a bare presence flag. Presence alone let a
            // stale probe claim a newer one's slot: a project that leaves
            // the sidebar and comes straight back is pruned and reinserted
            // while the first probe is still running, so that probe lands,
            // finds a marker, and writes its now-stale answer into the
            // slot belonging to the second — which then lands, finds
            // nothing, and drops. The row shows the older answer.
            //
            // Same shape as `hosts::accepts_completion`, which exists for
            // exactly this on the git panel: a completion may only write
            // if the token it carries is still the current one.
            self.project_git_token = self.project_git_token.wrapping_add(1);
            let token = self.project_git_token;
            self.project_git_inflight.insert(anchor.clone(), token);
            let state = Arc::clone(&self.project_git_state);
            cx.spawn(async move |ws, cx| {
                let probe = {
                    let state = Arc::clone(&state);
                    let anchor = anchor.clone();
                    cx.background_executor()
                        .spawn(async move { crate::project_git::probe(&state, &anchor) })
                        .await
                };
                let _ = ws.update(cx, |ws: &mut Workspace, cx| {
                    // Owning the slot is what makes the write safe, and
                    // ownership is the TOKEN matching — not merely a slot
                    // existing. A slot reissued to a newer probe while this
                    // one ran carries that probe's token, so this lands,
                    // does not match, and discards its stale answer without
                    // stealing the newer one's slot.
                    if !crate::project_git::probe_owns_slot(
                        ws.project_git_inflight.get(&anchor).copied(),
                        token,
                    ) {
                        return;
                    }
                    ws.project_git_inflight.remove(&anchor);
                    match probe {
                        // Busy is not an answer about the repo — someone is
                        // committing. Keep whatever the row already had.
                        crate::project_git::Probe::Busy => {}
                        crate::project_git::Probe::Blank => {
                            ws.project_git.insert(anchor, None);
                            cx.notify();
                        }
                        crate::project_git::Probe::Summary(summary) => {
                            ws.project_git.insert(anchor, Some(summary));
                            cx.notify();
                        }
                    }
                });
                Ok::<(), ()>(())
            })
            .detach();
        }
    }

    /// What one project row's git line says, or `None` when it draws none.
    /// A pure cache read — `refresh_project_git` is the only thing that
    /// talks to git.
    fn project_git_line(&self, anchor: Option<&PathBuf>) -> Option<String> {
        let entry = self.project_git.get(anchor?)?;
        crate::project_git::git_line(entry.as_ref())
    }

    /// The stats a capture carries beyond its directories: how many
    /// terminals the tab had, and how long it has been open since the mark
    /// laid at its creation, reopen or last capture.
    ///
    /// The terminal count is the tab's remembered pane count, not its LIVE
    /// one, for the same reason the directories are: a project closed one
    /// pane at a time has a single pane left by the time it dies, and
    /// "1 terminal" is not what the user had. Counting the same population
    /// the directories come from also keeps the two halves of a project's
    /// row from ever disagreeing.
    ///
    /// A tab with no mark reports zero seconds rather than a guess — the
    /// spec's rule that a session which never closes cleanly loses its
    /// increment instead of inventing one.
    fn tab_stats(&self, tab: &Tab, now: u64) -> (usize, u64) {
        let active = self
            .tab_opened_at
            .get(&tab.id)
            .map(|opened_at| crate::projects::session_secs(*opened_at, now))
            .unwrap_or(0);
        let terminals = self
            .tab_pane_dirs
            .get(&tab.id)
            .map(|remembered| remembered.terminals())
            // Only reachable for a tab this workspace has never observed;
            // every capture path folds first. Falling back to the live
            // count keeps it a measurement rather than a zero.
            .unwrap_or_else(|| tab.all_terminal_ids().len());
        (terminals, active)
    }

    /// Write the tabs at `indices` into the persistent project store, so
    /// closing them is not the same as losing them.
    ///
    /// One load and one save for the whole batch: quitting with eight tabs
    /// open must not rewrite `projects.json` eight times. What is worth
    /// keeping, what merges with an existing record and what gets evicted
    /// are all `ProjectStore::record`'s call, not this one's.
    ///
    /// Takes INDICES rather than tabs because it also has to write back to
    /// `tab_opened_at`, and a borrowed slice of `self.tabs` would hold the
    /// workspace immutably for the whole call.
    ///
    /// Each captured tab's terminals write their SCROLLBACK here too, and
    /// here only. The design's rule is that scrollback saves on exactly the
    /// paths project capture already runs on and never on a new one — and
    /// every one of those paths (closing the last terminal in a tab,
    /// closing a tab, pinning a live tab, `shutdown_all`, the quit hook and
    /// the load-session teardown) reaches this function and nothing else
    /// does. Putting the save anywhere else would be the sixth path the
    /// spec forbids.
    fn record_projects(&mut self, indices: &[usize], cx: &mut App) {
        // Last chance to ask the shells where they are: below, and in
        // every caller, they are about to be torn down.
        self.remember_pane_dirs(cx);
        let now = crate::projects::now_secs();
        let mut captured: Vec<crate::projects::Project> = Vec::new();
        let mut marked: Vec<String> = Vec::new();
        for index in indices {
            let Some(tab) = self.tabs.get(*index) else {
                continue;
            };
            // The mark moves for every tab that was ASKED to be captured,
            // including one that turned out to have no directories at all
            // (an all-remote tab). Otherwise a tab that spends an hour on a
            // peer and then opens one local pane would hand that hour to
            // the first project it ever manages to record.
            marked.push(tab.id.clone());
            let dirs = self.tab_dirs(tab);
            // Before the capture, while every shell is still alive to be
            // asked what it said. `save_tab_scrollback` borrows `self`
            // immutably, exactly as the lines around it do.
            self.save_tab_scrollback(*index, &dirs, cx);
            let Some(mut project) = crate::projects::project_for_dirs(dirs, now) else {
                continue;
            };
            let (terminals, active_secs) = self.tab_stats(tab, now);
            project.terminals = terminals;
            project.active_secs = active_secs;
            // A tab the user NAMED carries that name into the store, and
            // says so. `project_for_dirs` labels from the anchor folder's
            // basename, which is the right default and the wrong answer
            // for a project the user has already named — see
            // `renamed_tabs` and `ProjectStore::record`.
            if self.renamed_tabs.contains(&tab.id) {
                project.label = tab.label.clone();
                project.renamed = true;
            }
            captured.push(project);
        }
        // Reset rather than clear: a tab captured twice (the window closing
        // AND the app quitting) must not count the same minutes twice, and
        // one that survives its capture keeps accruing from here.
        for id in marked {
            self.tab_opened_at.insert(id, now);
        }
        if captured.is_empty() {
            return;
        }
        let mut store = crate::projects::ProjectStore::load();
        for project in captured {
            store.record(project);
        }
        // A failed write is an inconvenience, never a reason to interrupt
        // a close or a quit — the same discipline `settings.rs` uses.
        let _ = store.save();
        // The sidebar reads this, never the file, so it has to learn about
        // a capture from the same call that made it.
        self.projects_cache = store;
        // A project that loses a directory — or is evicted past the recent
        // cap altogether — leaves a scrollback file nobody will ever read
        // again, and nothing else in the app is ever going to notice. Reaped
        // against the store as it now stands, straight after the write that
        // made it so, so the directory can only ever hold text a project
        // still claims.
        let referenced: std::collections::HashSet<String> = self
            .projects_cache
            .all()
            .iter()
            .filter_map(|project| {
                project
                    .anchor
                    .as_deref()
                    .map(|anchor| crate::scrollback::project_keys(anchor, &project.dirs))
            })
            .flatten()
            .collect();
        crate::scrollback::reap(&referenced);
    }

    /// Store the scrollback of every live pane in the tab at `index` whose
    /// folder is one of the project's.
    ///
    /// The anchor is the record's FROZEN one wherever there is a record.
    /// The anchor a capture derives moves the moment the project gains a
    /// folder that ranks ahead of the current one, and a moved anchor is a
    /// different key — the text would be written where the next reopen does
    /// not look. `or_else` only fires for a project being recorded for the
    /// very first time, which by definition has nothing stored to miss.
    ///
    /// A pane whose folder is not in `dirs` — one sitting in `$HOME`, or
    /// viewing another machine — stores nothing. There is no index to key
    /// it by, and it is not part of the project's directory set.
    fn save_tab_scrollback(&self, index: usize, dirs: &[PathBuf], cx: &mut App) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let anchor = self
            .projects_cache
            .matching(dirs)
            .and_then(|record| record.anchor.clone())
            .or_else(|| crate::projects::anchor_dir(dirs).cloned());
        let Some(anchor) = anchor else {
            return;
        };
        for terminal_id in tab.all_terminal_ids() {
            let Some(pane) = self.panes.get(&terminal_id).cloned() else {
                continue;
            };
            pane.update(cx, |pane, _| {
                let Some(cwd) = pane.last_known_cwd() else {
                    return;
                };
                let Some(i) = crate::scrollback::dir_index_of(dirs, std::path::Path::new(&cwd))
                else {
                    return;
                };
                pane.save_scrollback(&crate::scrollback::terminal_key(&anchor, &dirs[i]));
            });
        }
    }

    /// Remember the tab at `index`, if there is one.
    fn record_project_at(&mut self, index: usize, cx: &mut App) {
        self.record_projects(&[index], cx);
    }

    /// Remember every open project: the quit and load-session paths, where
    /// the whole workspace goes at once.
    fn record_open_projects(&mut self, cx: &mut App) {
        let all: Vec<usize> = (0..self.tabs.len()).collect();
        self.record_projects(&all, cx);
    }

    // --- projects: pinning ---

    /// The folders the tab at `index` has had, or none for an index that
    /// no longer names a tab. The rows resolve a tab by INDEX at render
    /// time and by id at click time, so both need this.
    fn tab_dirs_at(&self, index: usize) -> Vec<PathBuf> {
        self.tabs
            .get(index)
            .map(|tab| self.tab_dirs(tab))
            .unwrap_or_default()
    }

    /// Pin or unpin the stored record with `id`.
    ///
    /// Reloaded from disk rather than mutated in the cache, the same way
    /// `open_project` and `record_projects` write: `projects_cache` is a
    /// render-time copy that another window may already have moved past,
    /// and a pin must not carry a stale list back over it.
    fn set_project_pinned(&mut self, id: &str, pinned: bool, cx: &mut Context<Self>) {
        let mut store = crate::projects::ProjectStore::load();
        if store.set_pinned(id, pinned) {
            // A failed write is an inconvenience, never a reason to
            // interrupt anything — `settings.rs`'s discipline throughout.
            let _ = store.save();
        }
        self.projects_cache = store;
        cx.notify();
    }

    /// Pin or unpin a LIVE tab — the project the user is looking at rather
    /// than one sitting in recents.
    ///
    /// A live tab is not a record, and until it closes it has none, so
    /// pinning one records it first: pinning is a promise that the project
    /// will be in that list, and a promise about something the store has
    /// never heard of is not one. A tab with nothing worth remembering
    /// (the bare `$HOME` starter tab) is refused outright rather than
    /// captured — its row draws no pin at all.
    fn toggle_tab_pin(&mut self, tab_index: usize, cx: &mut Context<Self>) {
        if !crate::projects::worth_remembering(&self.tab_dirs_at(tab_index)) {
            return;
        }
        if self
            .projects_cache
            .matching(&self.tab_dirs_at(tab_index))
            .is_none()
        {
            self.record_project_at(tab_index, cx);
        }
        let Some(project) = self.projects_cache.matching(&self.tab_dirs_at(tab_index)) else {
            return; // the capture found nothing to keep; nothing to pin
        };
        let (id, pinned) = (project.id.clone(), project.pinned);
        self.set_project_pinned(&id, !pinned, cx);
    }

    /// Reopen a remembered project: one terminal per remembered directory,
    /// all in one new tab under the project's own label.
    ///
    /// A directory that is no longer there does NOT stop the others — its
    /// shell opens in `$HOME` and `projects_note` says which folder and
    /// where it went instead. The decisions are `projects::plan_reopen`
    /// (where each shell lands), `projects::missing_dirs_note` (what the
    /// user is told) and `layout::grid_of` (the shape of the tab); all
    /// three are pure and tested. This function is the wiring.
    /// Open a project, or select the tab it is ALREADY open in.
    ///
    /// A guard against a STALE render rather than an everyday path:
    /// `sidebar_sections` now hides an open project from both sections, so
    /// no freshly drawn row can be clicked while its project is open. A row
    /// built while it was CLOSED can be, and without this that click
    /// spawned a second copy of every terminal the project had, and a third
    /// on the next click, without limit.
    fn open_or_switch_to_project(
        &mut self,
        project: &crate::projects::Project,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open_now: Vec<Vec<PathBuf>> = self
            .tabs
            .iter()
            .map(|tab| self.tab_dirs(tab))
            .filter(|dirs| !dirs.is_empty())
            .collect();
        // `open_now` skips tabs with no directories, so its indices are not
        // `self.tabs`' indices — resolve back through the same predicate
        // rather than indexing with the wrong one.
        if crate::projects::open_tab_for(project, &open_now).is_some() {
            if let Some(index) = self.tabs.iter().position(|tab| {
                crate::projects::open_tab_for(project, &[self.tab_dirs(tab)]).is_some()
            }) {
                self.select_tab(index, cx);
                self.focus_active_pane(window, cx);
                return;
            }
        }
        self.open_project(project, cx);
        self.focus_active_pane(window, cx);
    }

    fn open_project(&mut self, project: &crate::projects::Project, cx: &mut Context<Self>) {
        if project.dirs.is_empty() {
            return; // nothing to reopen; the store should never hold one
        }
        let plan = crate::projects::plan_reopen(&project.dirs, |dir| dir.is_dir());
        let mut terminal_ids: Vec<String> = Vec::with_capacity(plan.spawns.len());
        // The tab's memory of its own panes, seeded as they are spawned.
        let mut remembered = crate::projects::TabPaneDirs::default();
        for (cwd, wanted) in plan.spawns.iter().zip(project.dirs.iter()) {
            let terminal_id = self.fresh_id();
            // Keyed on the FOLDER, not its position: a project's `dirs`
            // can reorder between the save and the reopen, and a pinned
            // record keeps its own list while later captures move on.
            let restore_key = project
                .anchor
                .as_deref()
                .map(|anchor| crate::scrollback::terminal_key(anchor, wanted));
            self.spawn_pane_restoring(terminal_id.clone(), cwd.clone(), restore_key, cx);
            // Seeded even for a folder that IS there, so the pane counts as
            // one of the project's terminals from the instant it opens
            // rather than from the first fold.
            remembered.saw(&terminal_id, &crate::hosts::Target::Local, None);
            if cwd.is_none() {
                // This folder was gone, so the shell landed in `$HOME`.
                // Remembering what it was ASKED for keeps the capture's
                // folders equal to the project's — otherwise `$HOME`
                // enters `dirs`, and if the missing folder was the
                // project's PRIMARY one the record would be identified by
                // whatever came after it instead.
                remembered.asked_for(&terminal_id, &crate::hosts::Target::Local, wanted.clone());
            }
            terminal_ids.push(terminal_id);
        }
        let Some(tree) = crate::layout::grid_of(&terminal_ids) else {
            return; // unreachable: dirs is non-empty, so ids is too
        };
        let tab_id = format!("tab-{}", self.next_id);
        self.next_id += 1;
        // The reopen starts the clock, exactly as a fresh tab does.
        self.mark_tab_opened(&tab_id);
        self.tab_pane_dirs.insert(tab_id.clone(), remembered);
        self.tabs
            .push(Tab::single(tab_id, project.label.clone(), tree));
        self.active_tab = self.tabs.len() - 1;
        self.set_focused_terminal(terminal_ids.first().cloned(), cx);
        self.projects_note = crate::projects::missing_dirs_note(&plan.missing);
        // Record the use NOW rather than waiting for the tab to close, so a
        // crash still leaves "you just used this" behind. `record` merges it
        // into the existing entry by its ANCHOR folder — carried forward by
        // `touch_for_reopen` rather than re-derived — which is why reopening
        // a PINNED project moves it up the pinned list instead of dropping an
        // unpinned twin of itself into recents.
        let mut store = crate::projects::ProjectStore::load();
        store.record(crate::projects::touch_for_reopen(
            project,
            crate::projects::now_secs(),
            terminal_ids.len(),
        ));
        let _ = store.save();
        self.projects_cache = store;
        self.push_git_cwd(cx);
        cx.notify();
    }

    fn close_terminal(&mut self, terminal_id: &str, cx: &mut Context<Self>) {
        // THIS pane is about to stop existing, whether or not its tab goes
        // with it, so its folder has to be folded into the tab's memory
        // now — after the `self.panes.remove` below nothing can ask it
        // again. Closing a project's panes one at a time is completely
        // ordinary, and without this the tab would remember only whoever
        // happened to be last.
        self.remember_pane_dirs(cx);
        // A tab dies exactly when its LAST terminal goes, and every
        // directory in it dies with it. Capture here, at the top, while the
        // pane is still in `self.panes` and its shell still running — the
        // `self.tabs.remove` below is far too late to ask a dead process
        // where it was.
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.window_of(terminal_id).is_some())
        {
            if self.tabs[index].all_terminal_ids().len() == 1 {
                self.record_project_at(index, cx);
            }
        }
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
            // Drop this terminal's share state along with the pane — a
            // future id reusing this string must never inherit an old
            // peer's visibility.
            let live: Vec<String> = self.panes.keys().cloned().collect();
            self.broadcasts.prune_to(&live);
            self.share_open.retain(|id| self.panes.contains_key(id));
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
                    let next = collect_terminal_ids(&self.tabs[tab_index].windows[window_index])
                        .into_iter()
                        .next();
                    self.set_focused_terminal(next, cx);
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
                    let next = collect_terminal_ids(self.tabs[tab_index].active_pane())
                        .into_iter()
                        .next();
                    self.set_focused_terminal(next, cx);
                }
            }
            None => {
                let was_active = tab_index == self.active_tab
                    || self.focused_terminal.as_deref() == Some(terminal_id);
                self.tabs.remove(tab_index);
                // Closing the LAST terminal leaves the workspace empty and
                // stays there. It used to force a fresh shell in `$HOME`,
                // which is the one thing a user who has just closed all of
                // their work did not ask for.
                self.settle_after_tab_removal(tab_index, was_active, cx);
            }
        }
        cx.notify();
    }

    /// Close a whole tab: every terminal in its tree shuts down (the old
    /// app's removeTab). Closing the last one leaves the workspace with no
    /// tabs and no focused terminal — see `settle_after_tab_removal`.
    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(ids) = self.tabs.get(index).map(|tab| tab.all_terminal_ids()) else {
            return;
        };
        // The sibling of the capture in `close_terminal`: here the whole
        // project goes at once, so it is remembered unconditionally — and,
        // for the same reason, before the shutdown loop below kills the
        // shells that know where they are.
        self.record_project_at(index, cx);
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
        // A tab close can drop several terminals at once; prune all of
        // their share state together, same reasoning as `close_terminal`.
        let live: Vec<String> = self.panes.keys().cloned().collect();
        self.broadcasts.prune_to(&live);
        self.share_open.retain(|id| self.panes.contains_key(id));
        self.tabs.remove(index);
        // The sibling of the same decision in `close_terminal`, and made in
        // the same call: closing the last project leaves the workspace
        // empty rather than respawning a shell in `$HOME`.
        self.settle_after_tab_removal(index, was_active, cx);
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
                        // Rename site 1 of 3 (Enter). Every one of them
                        // has to mark the tab, or a capture takes the
                        // user's name straight back off the project —
                        // see `renamed_tabs`.
                        let mut named = None;
                        if let Some(tab) = ws.tabs.get_mut(idx) {
                            let trimmed = name.trim();
                            if !trimmed.is_empty() {
                                tab.label = trimmed.to_string();
                                named = Some(tab.id.clone());
                            }
                        }
                        ws.renamed_tabs.extend(named);
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
            let next = collect_terminal_ids(self.tabs[index].active_pane())
                .into_iter()
                .next();
            self.set_focused_terminal(next, cx);
            self.push_git_cwd(cx);
            cx.notify();
        }
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
        self.tts_voice_list_open = false;
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
                SidebarView::Peers => crate::icons::Icon::Peers,
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
                SidebarView::Peers => Some(self.render_peers_view(cx)),
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
                        .pb(px(6.0))
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
                        ))
                        .child(rail_item(
                            self,
                            "rail-peers",
                            "peers",
                            SidebarView::Peers,
                            cx,
                        ))
                        .child(div().flex_grow())
                        .child({
                            // Phone link: outline handset = off, filled =
                            // companion serving. Click opens the flyout with
                            // the link and controls; red tint = it died.
                            let running = self.companion_server.is_some();
                            let errored = self.companion_error.is_some();
                            div()
                                .id("rail-phone")
                                .cursor_pointer()
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded(px(5.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(self.companion_flyout, |d| d.bg(rgb(theme.ui_background)))
                                .hover(|style| style.bg(rgb(theme.ui_border)))
                                .child(crate::icons::icon(
                                    crate::icons::Icon::Phone { filled: running },
                                    if running {
                                        theme.ui_accent
                                    } else if errored {
                                        theme.red
                                    } else {
                                        theme.ui_text_muted
                                    },
                                ))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|ws, _, _window, cx| {
                                        ws.companion_flyout = !ws.companion_flyout;
                                        cx.notify();
                                    }),
                                )
                        })
                        .child({
                            // Keep-awake: outline cup = released, filled =
                            // caffeinate holding (manual or auto). Click
                            // flips only the manual hold.
                            let held = self.caffeinate_child.is_some();
                            div()
                                .id("rail-awake")
                                .cursor_pointer()
                                .w(px(26.0))
                                .h(px(26.0))
                                .rounded(px(5.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|style| style.bg(rgb(theme.ui_border)))
                                .child(crate::icons::icon(
                                    crate::icons::Icon::Coffee { filled: held },
                                    if held {
                                        theme.ui_accent
                                    } else {
                                        theme.ui_text_muted
                                    },
                                ))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|ws, _, _window, cx| {
                                        ws.toggle_manual_awake();
                                        cx.notify();
                                    }),
                                )
                        }),
                )
                .children(active_view),
        )
    }

    /// Paired Macs and what each is sharing with this one. Click a peer to
    /// ask it; click one of its terminals to open it as a pane.
    ///
    /// The degraded contract (D5) is stated HERE, above the list, rather
    /// than left to be discovered after a pane is open: what an attached
    /// pane cannot do is a property of the choice being made on this
    /// screen. It is repeated on the focused bar once a pane is open, for
    /// the same reason.
    fn render_peers_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        // The sidebar needs an ADDRESS per peer, which only a tailnet scan
        // supplies. One automatic scan per session, exactly like the
        // settings sheet's; after that it is the rescan button.
        if !self.peer_scanned_once && !self.peer_scanning {
            self.scan_peer_candidates(cx);
        }
        let theme = self.theme;
        let (paired, _problems) = self.settings.peers();
        let browsing = self.peer_browse.clone();
        let scanning = self.peer_scanning;

        let row = |label: SharedString, muted: bool| {
            div()
                .py(px(2.0))
                .pl(px(10.0))
                .text_size(px(10.0))
                .text_color(rgb(if muted {
                    theme.ui_text_muted
                } else {
                    theme.ui_text
                }))
                .child(label)
        };

        let mut peer_rows: Vec<gpui::AnyElement> = Vec::new();
        for peer in &paired {
            let open = browsing.as_ref() == Some(&peer.id);
            let header_id = peer.id.clone();
            peer_rows.push(
                div()
                    .id(SharedString::from(format!("peer-browse-{}", peer.id.0)))
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(4.0))
                    .py(px(3.0))
                    .rounded(px(4.0))
                    .when(open, |d| d.bg(rgb(theme.ui_surface)))
                    .hover(|style| style.bg(rgb(theme.ui_border)))
                    .child(
                        div()
                            .w(px(8.0))
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(if open { "v" } else { ">" }),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .text_size(px(11.0))
                            .text_color(rgb(theme.ui_text))
                            .child(SharedString::from(peer.label.clone())),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.browse_peer(header_id.clone(), cx)),
                    )
                    .into_any_element(),
            );
            if !open {
                continue;
            }
            let listing = peer_listing(
                self.peer_reach.get(&peer.id),
                self.peer_sessions.get(&peer.id).map(|p| p.last_poll()),
            );
            // Every non-list outcome says which one it is. An empty list
            // under a peer name is what "I clicked and nothing happened"
            // looks like, and it is indistinguishable from four different
            // failures — see `peer_listing`.
            match listing {
                PeerListing::Probing => {
                    peer_rows.push(row("looking for it...".into(), true).into_any_element())
                }
                PeerListing::Waiting => {
                    peer_rows.push(row("asking what it shares...".into(), true).into_any_element())
                }
                PeerListing::Nothing => peer_rows
                    .push(row("sharing nothing with this Mac yet".into(), true).into_any_element()),
                PeerListing::Unreadable => peer_rows.push(
                    row(
                        "answered, but not with a list - grant this Mac view there".into(),
                        true,
                    )
                    .into_any_element(),
                ),
                PeerListing::Unreachable(note) => {
                    peer_rows.push(row(note.into(), true).into_any_element())
                }
                PeerListing::Sessions(sessions) => {
                    for session in sessions {
                        let peer_id = peer.id.clone();
                        let session_id = session.id.clone();
                        let session_label = session.label.clone();
                        let shown = if session.label.trim().is_empty() {
                            session.id.clone()
                        } else {
                            session.label.clone()
                        };
                        peer_rows.push(
                            div()
                                .id(SharedString::from(format!(
                                    "peer-session-{}-{}",
                                    peer.id.0, session.id
                                )))
                                .cursor_pointer()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.0))
                                .ml(px(10.0))
                                .px(px(4.0))
                                .py(px(2.0))
                                .rounded(px(3.0))
                                .hover(|style| style.bg(rgb(theme.ui_border)))
                                .child(activity_dot(session.activity, theme))
                                .child(
                                    div()
                                        .flex_grow()
                                        .text_size(px(10.0))
                                        .text_color(rgb(theme.ui_text))
                                        .child(SharedString::from(shown)),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _, window, cx| {
                                        ws.open_peer_session(
                                            peer_id.clone(),
                                            session_id.clone(),
                                            session_label.clone(),
                                            cx,
                                        );
                                        ws.focus_active_pane(window, cx);
                                    }),
                                )
                                .into_any_element(),
                        );
                    }
                }
            }
            let retry_peer = peer.id.clone();
            peer_rows.push(
                div()
                    .id(SharedString::from(format!("peer-retry-{}", peer.id.0)))
                    .cursor_pointer()
                    .ml(px(10.0))
                    .px(px(4.0))
                    .py(px(1.0))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .hover(|style| style.text_color(rgb(theme.ui_accent)))
                    .child("look again")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| ws.reprobe_peer(retry_peer.clone(), cx)),
                    )
                    .into_any_element(),
            );
        }

        // Same shell as the projects view beside it: fixed width, its own
        // scroll, a quiet header rule. A second sidebar that sized itself
        // differently would read as a different kind of surface.
        div()
            .id("peers-view")
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
                            .child("PEERS"),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .id("peers-rescan")
                            .cursor_pointer()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .hover(|style| style.text_color(rgb(theme.ui_accent)))
                            .child(if scanning { "scanning" } else { "rescan" })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, _, cx| ws.scan_peer_candidates(cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .id("peers-list")
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .p(px(8.0))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(settings_ui::hints::PEER_TERMINALS)),
                    )
                    // D5, stated before the choice rather than after it.
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(SharedString::from(crate::pane::remote_limits_line())),
                    )
                    .children(paired.is_empty().then(|| {
                        div()
                            .mt(px(6.0))
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child("None yet. Pair a Mac in settings to see its terminals here.")
                    }))
                    .children(peer_rows),
            )
            .into_any_element()
    }

    /// Orca-style projects view: every tab is a project, with quick status
    /// of the terminals inside it. Click a project to switch tabs; click a
    /// terminal to jump straight to it.
    fn render_projects_view(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            let active_tab = tab_index == self.active_tab;
            let project_tab_id = tab.id.clone();
            let close_tab_id = tab.id.clone();
            let pin_tab_id = tab.id.clone();
            // A live tab's pin state lives in the STORE, not on the tab —
            // a tab is a window on a project, and pinning is something
            // the project carries across quits. `matching` finds the
            // record this tab stands for by the same anchor rule capture
            // uses; a tab with nothing worth remembering has no record and
            // gets no pin.
            let tab_dirs = self.tab_dirs(tab);
            let can_pin = crate::projects::worth_remembering(&tab_dirs);
            let record = self.projects_cache.matching(&tab_dirs);
            let tab_pinned = record.is_some_and(|record| record.pinned);
            // The same mark the remembered row below draws, so a project
            // does not change its face the moment it is open. The stored
            // record's `icon` when there is one — that field is what
            // auto-detection will later write into — and the default
            // otherwise, since a tab this store has never seen is still a
            // project with a name.
            let tab_mark = crate::projects::project_mark(
                &tab.label,
                record.map(|record| record.icon).unwrap_or_default(),
            );
            let label_element = if let Some((_, field)) = self
                .rename_field
                .as_ref()
                .filter(|(rename_index, _)| *rename_index == tab_index)
            {
                div().w(px(140.0)).child(field.clone()).into_any_element()
            } else {
                div()
                    // The row carries a mark and a pin now as well as the
                    // + and x it always had; a long name gives way to them
                    // rather than pushing them off the sidebar.
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
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
            // The dot leads the row: what this project IS, scannable
            // without reading it. Its terminals' activity, reduced —
            // read from the sidebar cache, so no pane is queried and no
            // process is probed to draw a frame. A terminal the poll has
            // not reached yet reports `Unknown` rather than being dropped:
            // absence of an observation is not evidence of an idle shell.
            let tab_activity = project_activity(
                &tab.windows
                    .iter()
                    .flat_map(collect_terminal_ids)
                    .map(|id| {
                        self.sidebar_status_cache
                            .get(&id)
                            .map(|(_, activity)| *activity)
                            .unwrap_or(Activity::Unknown)
                    })
                    .collect::<Vec<_>>(),
            );
            // The second line. `None` until the poll has probed this
            // project, and `None` forever for a folder that is not a repo
            // — in both cases the row simply has no git line.
            let tab_git = self.project_git_line(crate::project_git::row_anchor(&tab_dirs, record));
            rows.push(
                div()
                    .id(SharedString::from(format!("project-{}", tab.id)))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    // A CARD, not a highlighted line: the selected project
                    // is a shape with edges, which is what lets the two
                    // lines inside it read as one thing. Inset so the
                    // rounding is visible against the sidebar edge.
                    .mx(px(SIDEBAR_ROW_INSET))
                    .px(px(SIDEBAR_ROW_PAD))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .when(active_tab, |d| d.bg(rgb(theme.ui_surface)))
                    .hover(|style| style.bg(rgb(theme.ui_surface)))
                    .child(
                        div()
                            .id(SharedString::from(format!("project-fold-{}", tab.id)))
                            .cursor_pointer()
                            .w(px(SIDEBAR_FOLD_W))
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
                    .child(activity_dot(tab_activity, theme))
                    .child(project_mark_badge(tab_mark, theme))
                    // Identity on top, context beneath. The column takes
                    // the row's slack so the name gives way to the
                    // controls rather than pushing them off the sidebar,
                    // and so the git line indents under the name by
                    // itself. Room for the nested children and the buddy
                    // line the next slice adds is here, beneath.
                    //
                    // No terminal count on either line. It sat between the
                    // project's name and its controls in a narrow sidebar,
                    // so the name truncated to make room for it —
                    // "SuperTermin 1 terminal" — and the name is the only
                    // part anyone scans for. The row already expands to
                    // list the terminals themselves, which says the same
                    // thing without spending the width.
                    //
                    // Its sibling in `project_summary` (the REMEMBERED
                    // project rows) was removed first and this one was
                    // missed, which is why the count appeared to survive
                    // its own deletion.
                    .child(
                        div()
                            .flex_grow()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(label_element)
                            .children(tab_git.map(|line| project_detail_line(line, theme))),
                    )
                    .children(can_pin.then(|| {
                        div()
                            .id(SharedString::from(format!("project-pin-{}", tab.id)))
                            .flex_none()
                            .cursor_pointer()
                            .px(px(2.0))
                            .opacity(if tab_pinned { 1.0 } else { 0.45 })
                            .hover(|style| style.opacity(1.0))
                            .child(crate::icons::icon(
                                crate::icons::Icon::Pin { filled: tab_pinned },
                                if tab_pinned {
                                    theme.ui_accent
                                } else {
                                    theme.ui_text_muted
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, _, cx| {
                                    cx.stop_propagation();
                                    // The STABLE id, resolved at click
                                    // time: a captured index goes stale
                                    // the moment another tab closes.
                                    if let Some(index) =
                                        ws.tabs.iter().position(|t| t.id == pin_tab_id)
                                    {
                                        ws.toggle_tab_pin(index, cx);
                                    }
                                }),
                            )
                    }))
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
            // A terminal hangs one step under its project, or two when a
            // window row stands between them. The share panel hangs one
            // step under the terminal. See `sidebar_bullet_x`.
            let terminal_depth: u8 = if multi_window { 2 } else { 1 };
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
                            // One step under the project, sharing its
                            // inset. See `sidebar_bullet_x`.
                            .mx(px(SIDEBAR_ROW_INSET))
                            .pl(px(sidebar_child_pad_left(1)))
                            .pr(px(8.0))
                            .rounded(px(4.0))
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
                    // Encode, don't infer: a remote-target pane offers no
                    // Share control at all, rather than one that silently
                    // no-ops (see `may_share_terminal`).
                    let shareable_target = may_share_terminal(pane_ref.target());
                    let focused = self.focused_terminal.as_deref() == Some(terminal_id.as_str());
                    let title = pane_ref.title();
                    let (cwd, activity) = self
                        .sidebar_status_cache
                        .get(&terminal_id)
                        .cloned()
                        .unwrap_or((String::new(), Activity::Idle));
                    let cwd: SharedString = cwd.into();
                    // Quick status dot, Orca-style. tcgetpgrp alone can't
                    // tell "computing" from "interactive program awaiting
                    // input", so output silence disambiguates: a claude
                    // session that finished and sits at its input box goes
                    // cyan within seconds.
                    //   green  = shell prompt, ready for commands
                    //   yellow = foreground job producing output (working)
                    //   cyan   = foreground job quiet - awaiting input
                    //   hollow = no trustworthy signal (Unknown; remote
                    //            panes in a later slice — unreachable for
                    //            local panes today)
                    let quiet =
                        pane_ref.last_activity.elapsed() >= std::time::Duration::from_secs(3);
                    let dot_color = match activity {
                        Activity::Idle => theme.green,
                        Activity::Busy if quiet => theme.cyan,
                        Activity::Busy => theme.yellow,
                        Activity::Unknown => theme.ui_text_muted,
                    };
                    let dot_hollow = matches!(activity, Activity::Unknown);
                    let jump_id = terminal_id.clone();
                    rows.push(
                        div()
                            .id(SharedString::from(format!("project-term-{terminal_id}")))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(20.0))
                            // One step under the project, or two when a
                            // window row stands between them. Sharing the
                            // project's inset is what stops a terminal
                            // being drawn WIDER than the project it
                            // belongs to. See `sidebar_bullet_x`.
                            .mx(px(SIDEBAR_ROW_INSET))
                            .pl(px(sidebar_child_pad_left(terminal_depth)))
                            .pr(px(8.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .when(focused, |d| d.bg(rgb(theme.ui_surface)))
                            .hover(|style| style.bg(rgb(theme.ui_surface)))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .when(dot_hollow, |d| d.border_1().border_color(rgb(dot_color)))
                                    .when(!dot_hollow, |d| d.bg(rgb(dot_color))),
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
                            .children(shareable_target.then(|| {
                                let toggle_id = terminal_id.clone();
                                let shared_now =
                                    !self.broadcasts.peers_for(&terminal_id).is_empty();
                                div()
                                    .id(SharedString::from(format!(
                                        "project-term-share-{terminal_id}"
                                    )))
                                    .flex_none()
                                    .cursor_pointer()
                                    .opacity(if shared_now { 1.0 } else { 0.45 })
                                    .hover(|style| style.opacity(1.0))
                                    .child(crate::icons::icon(
                                        crate::icons::Icon::Share { active: shared_now },
                                        if shared_now {
                                            theme.ui_accent
                                        } else {
                                            theme.ui_text_muted
                                        },
                                    ))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |ws, _, _, cx| {
                                            cx.stop_propagation();
                                            ws.toggle_share_open(&toggle_id, cx);
                                        }),
                                    )
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.focus_terminal_by_id(&jump_id, window, cx);
                                }),
                            )
                            .into_any_element(),
                    );
                    // Defense in depth alongside the icon gate above: a
                    // remote-target pane's id must never open a share row
                    // even if `share_open` somehow already held it (e.g. a
                    // reused id across a session reload).
                    if shareable_target && self.share_open.contains(&terminal_id) {
                        rows.push(self.render_share_row(
                            &terminal_id,
                            &self.shareable_peers_cache,
                            terminal_depth + 1,
                            cx,
                        ));
                    }
                }
            }
        }
        // Remembered projects, beneath the live ones. Cloned out of the
        // cache: a row's click handler outlives this borrow, and a
        // `Project` is self-contained data (unlike a tab INDEX, which goes
        // stale the moment a tab closes and so is always re-resolved).
        //
        // A project that is open right now is the live tab above, so it is
        // filtered out of BOTH sections — `sidebar_sections` says why.
        // The open sets come from `tab_pane_dirs`, which is
        // plain data this workspace already holds: no pane is read and no
        // process is queried to render a frame.
        let open_now: Vec<Vec<PathBuf>> = self
            .tabs
            .iter()
            .map(|tab| self.tab_dirs(tab))
            .filter(|dirs| !dirs.is_empty())
            .collect();
        let sections = crate::projects::sidebar_sections(&self.projects_cache, &open_now);
        let pinned: Vec<crate::projects::Project> = sections.pinned.into_iter().cloned().collect();
        let recent: Vec<crate::projects::Project> = sections.recent.into_iter().cloned().collect();
        for (heading, section) in [("PINNED", &pinned), ("RECENT", &recent)] {
            if section.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .px(px(8.0))
                    .pt(px(8.0))
                    .pb(px(2.0))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(heading)
                    .into_any_element(),
            );
            for project in section {
                rows.push(self.render_project_row(project, cx));
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
            .children(self.projects_note.clone().map(|note| {
                // What the last reopen could not do, said where the user
                // just clicked. Click to dismiss.
                div()
                    .id("projects-note")
                    .cursor_pointer()
                    .px(px(8.0))
                    .py(px(4.0))
                    .border_b_1()
                    .border_color(rgb(theme.ui_border))
                    .text_size(px(9.0))
                    .text_color(rgb(theme.yellow))
                    .child(SharedString::from(note))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, _, cx| {
                            ws.projects_note = None;
                            cx.notify();
                        }),
                    )
            }))
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

    /// One remembered project: its name, and what it IS — its branch and
    /// working state when the cache has probed it, and the folder count
    /// and time worked when it has not. The terminal count was there and
    /// was dropped: in a narrow sidebar it truncated the name, which is
    /// the only part anyone scans for.
    ///
    /// Same anatomy as the LIVE tab rows above — dot, mark, name, then one
    /// muted line beneath — because a project must not change its face the
    /// moment it is opened. The two are separate renderers and this file's
    /// recurring defect is a decision made in one of them and missed in
    /// the other, so everything they share is a named helper rather than
    /// two copies: `activity_dot`, `project_mark_badge`,
    /// `project_detail_line`, `project_git_line`.
    ///
    /// Clicking reopens it; clicking its pin keeps it (or lets it go).
    fn render_project_row(
        &self,
        project: &crate::projects::Project,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.theme;
        // The git line when this project's repo has been probed; the
        // folder/time summary when it has not, or when the folder is not a
        // repo at all. Never both: one muted line under the name is the
        // anatomy, and the branch is the more current of the two facts.
        let detail = self
            .project_git_line(crate::project_git::row_anchor(&project.dirs, Some(project)))
            .unwrap_or_else(|| {
                crate::projects::project_summary(project.dirs.len(), project.active_secs)
            });
        let pinned = project.pinned;
        let pin_id = project.id.clone();
        let mark = crate::projects::project_mark(&project.label, project.icon);
        let to_open = project.clone();
        div()
            .id(SharedString::from(format!("project-open-{}", project.id)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .mx(px(SIDEBAR_ROW_INSET))
            // No fold triangle -- a project that is not open has no
            // children to fold -- but the triangle's column is spent
            // anyway, so a PINNED dot lands in the same column as an open
            // project's rather than 18px to its left.
            .pl(px(SIDEBAR_ROW_PAD + SIDEBAR_FOLD_W + SIDEBAR_GAP))
            .pr(px(SIDEBAR_ROW_PAD))
            .py(px(3.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(theme.ui_surface)))
            // A project that is not open has no terminals to observe, so
            // its dot is hollow. `project_activity` is what makes that the
            // same rule the live rows use rather than a second one.
            .child(activity_dot(project_activity(&[]), theme))
            .child(project_mark_badge(mark, theme))
            .child(
                div()
                    .flex_grow()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(rgb(theme.ui_text))
                            .child(SharedString::from(project.label.clone())),
                    )
                    .child(project_detail_line(detail, theme)),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "project-pin-row-{}",
                        project.id
                    )))
                    .flex_none()
                    .cursor_pointer()
                    .px(px(2.0))
                    .opacity(if pinned { 1.0 } else { 0.45 })
                    .hover(|style| style.opacity(1.0))
                    .child(crate::icons::icon(
                        crate::icons::Icon::Pin { filled: pinned },
                        if pinned {
                            theme.ui_accent
                        } else {
                            theme.ui_text_muted
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _, _, cx| {
                            // Never also reopen the project: the pin is a
                            // control ON the row, not the row.
                            cx.stop_propagation();
                            ws.set_project_pinned(&pin_id, !pinned, cx);
                        }),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, window, cx| {
                    ws.open_or_switch_to_project(&to_open, window, cx);
                }),
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
        self.set_focused_terminal(Some(terminal_id.to_string()), cx);
        self.push_git_cwd(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn open_sidebar(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.sidebar_view = view;
        match view {
            // Opening the view must not wait for the next poll to show
            // what is in the store — nor to know which of those projects
            // is already open, which is what BOTH remembered sections hide.
            SidebarView::Projects => {
                self.projects_cache = crate::projects::ProjectStore::load();
                self.remember_pane_dirs(cx);
                // And their git lines, off-thread, rather than up to one
                // slow tick after the view appears. Single-flight, so an
                // impatient reopen cannot stack a second probe per project.
                self.refresh_project_git(cx);
            }
            SidebarView::Peers => {}
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

    /// Open (or replace) the docked file viewer. Refuses unless the
    /// currently focused pane's target is local: gpui dispatches against
    /// the last painted frame, so the row click that produced `path` can
    /// be stale by the time this runs — re-checking here is the D6 guard
    /// against reading a locally-derived path through a pane that has
    /// since gone remote (or unfocused).
    fn open_file_viewer(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let target_is_local = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .is_some_and(|pane| pane.read(cx).target().is_local());
        if !target_is_local {
            return;
        }
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
                // Focus can change while the native dialog was open, so the
                // guard is re-checked here rather than trusted from before
                // the await.
                let focused = ws
                    .focused_terminal
                    .as_ref()
                    .and_then(|id| ws.panes.get(id))
                    .cloned();
                match focused {
                    Some(pane)
                        if pane.read(cx).has_live_shell()
                            && may_write_cd(
                                pane.read(cx).target(),
                                pane.read(cx).foreground_activity(),
                            ) =>
                    {
                        // Shell at its prompt: Ctrl+U first, so any
                        // half-typed input is cleared instead of being
                        // SUBMITTED with the cd appended; then a plain cd,
                        // single-quoted with the POSIX '\'' escape.
                        // Best-effort: a job starting between the idle probe
                        // and this write can still receive the text — full
                        // certainty needs shell integration.
                        let quoted = path.replace('\'', "'\\''");
                        pane.read(cx).send_text(&format!("\u{15}cd '{quoted}'\r"));
                        ws.focus_active_pane(window, cx);
                    }
                    Some(pane) if !pane.read(cx).target().is_local() => {
                        // A remote focused pane must never fall through to
                        // opening a new LOCAL window at the picked path.
                    }
                    _ => {
                        // Busy (or no) local terminal: open a new window in
                        // the current project at that directory — or, with
                        // no project open at all, a tab there. Same rule as
                        // cmd-n, made in the same function so the two
                        // cannot drift.
                        let cwd = Some(PathBuf::from(path));
                        match new_window_target(ws.active_tab, ws.tabs.len()) {
                            NewWindowTarget::InTab(index) => ws.new_window(index, cwd, cx),
                            NewWindowTarget::AsNewTab => ws.add_tab(cwd, cx),
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

    /// Resolve a non-local target's id to its display label. A missing
    /// profile still yields a string that names the id — never anything
    /// that could read as local (that's the safety property established
    /// with `ResolvedTarget::MissingProfile`) and never a bare opaque id
    /// that looks fine at a glance when the host it names is actually gone.
    ///
    /// Asks the PEER list too, since this phase: a pane attached to another
    /// Mac carries `Target::Remote` with that peer's id, and answering
    /// "missing host (a1b2...)" for a machine that is right there and
    /// working would be a confident lie. See `remote_target_label` for the
    /// lookup order.
    fn profile_label(&self, id: &crate::hosts::ProfileId) -> String {
        let (profiles, _problems) = self.settings.profiles();
        let (peers, _peer_problems) = self.settings.peers();
        remote_target_label(id, &profiles, &peers)
    }

    /// The ONLY writer of `focused_terminal`. Focus and panel identity
    /// change together, in one update, before any other UI action can
    /// dispatch — the periodic refresh at `pet_tick_count % 3` is gated on
    /// the sidebar being open and must never be responsible for identity.
    fn set_focused_terminal(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.focused_terminal = id;
        self.retarget_panels(cx);
    }

    /// Point both panels (and the docked file viewer) at the focused
    /// pane's target: a local pane's cwd, or (once the focus is on
    /// another host) that pane's remote label — never the previous pane's
    /// local path.
    fn retarget_panels(&mut self, cx: &mut Context<Self>) {
        let target = self
            .focused_terminal
            .as_ref()
            .and_then(|id| self.panes.get(id))
            .map(|pane| {
                let pane = pane.read(cx);
                let pane_target = pane.target().clone();
                let cwd = pane.cwd();
                let label = match &pane_target {
                    crate::hosts::Target::Local => String::new(),
                    crate::hosts::Target::Remote(id) => self.profile_label(id),
                };
                // `from_pane` is the only place a `PanelTarget` is derived
                // — it's what guarantees a remote pane can never produce a
                // local path, whatever cwd it reports.
                crate::hosts::PanelTarget::from_pane(&pane_target, cwd, &label)
            })
            .unwrap_or(crate::hosts::PanelTarget::Detached);
        if let Some(panel) = self.git_panel.clone() {
            panel.update(cx, |panel, panel_cx| {
                panel.set_target(target.clone(), panel_cx)
            });
        }
        if let Some(panel) = self.files_panel.clone() {
            panel.update(cx, |panel, panel_cx| {
                panel.set_target(target.clone(), panel_cx)
            });
        }
        // A retarget away from Local must not leave a file opened under
        // the previous local pane sitting in the docked viewer, readable
        // and actionable, once the panels have moved on.
        if !matches!(target, crate::hosts::PanelTarget::Local(_)) {
            self.close_file_viewer(cx);
        }
    }

    /// Close the docked file viewer the same way its own `ViewerClosed`
    /// event does: drop the entity and notify. No other cleanup exists on
    /// that path today, so this stays a two-line mirror of it rather than
    /// a second implementation to keep in sync.
    fn close_file_viewer(&mut self, cx: &mut Context<Self>) {
        self.file_viewer = None;
        cx.notify();
    }

    /// Kept for the six existing call sites that refresh panel CONTENT
    /// (e.g. the periodic tick while the sidebar is open) without a focus
    /// change of their own. No longer owns identity — just forwards to
    /// the single source of truth.
    fn push_git_cwd(&mut self, cx: &mut Context<Self>) {
        self.retarget_panels(cx);
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
        // Loading a session replaces the whole workspace, so it drops every
        // open project just as surely as closing each tab would. Same rule,
        // same place in the order: remember them before the teardown.
        self.record_open_projects(cx);
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
        // Every surviving terminal below gets a FRESH id (`fresh_id`,
        // called per leaf just below) — none of the ones just torn down
        // mean anything anymore. Clear share state now, while `self.panes`
        // is empty, rather than carrying it forward: if it were left in
        // place, a later fresh id that happened to reuse an old string
        // would silently inherit that old id's peers.
        self.broadcasts.prune_to(&[]);
        self.share_open.clear();
        self.tabs.clear();
        // Resolved once per load: a saved target whose profile is gone must
        // restore as a dead pane, never silently as a local shell.
        let (profiles, _problems) = self.settings.profiles();
        for tab in layout.tabs {
            let mut mapping = HashMap::new();
            for (old_id, target) in tab.all_terminal_targets() {
                let new_id = self.fresh_id();
                match crate::hosts::resolve_target(&target, &profiles) {
                    crate::hosts::ResolvedTarget::Local => {
                        self.spawn_pane(new_id.clone(), None, cx);
                    }
                    crate::hosts::ResolvedTarget::Remote(_)
                    | crate::hosts::ResolvedTarget::MissingProfile(_) => {
                        // Slice 2 gives a resolved Remote a real connection;
                        // here both restore dead rather than ever spawning
                        // a local shell for a non-local target.
                        self.spawn_dead_pane(new_id.clone(), target, cx);
                    }
                }
                mapping.insert(old_id, new_id);
            }
            let windows: Vec<PaneNode> = tab
                .windows
                .iter()
                .map(|window| remap_ids(window, &mapping))
                .collect();
            let active_window = tab.active_window.min(windows.len() - 1);
            self.mark_tab_opened(&tab.id);
            self.tabs.push(Tab {
                id: tab.id,
                label: tab.label,
                windows,
                active_window,
            });
        }
        // A saved layout carries its OWN tab ids, so the marks and the
        // collapsed set can both be holding ids no live tab answers to.
        self.prune_closed_tab_state();
        if self.tabs.is_empty() {
            // A session saved from an empty workspace restores as one.
            // Every pane above was torn down and `self.panes` is empty, so
            // the id `focused_terminal` still holds names nothing — it has
            // to be dropped here rather than left pointing at a dead pane.
            self.active_tab = 0;
            self.set_focused_terminal(None, cx);
            self.pending_root_focus = true;
        } else {
            let wanted = layout.active_tab_id;
            self.active_tab = self.tabs.iter().position(|t| t.id == wanted).unwrap_or(0);
            let next = collect_terminal_ids(self.tabs[self.active_tab].active_pane())
                .into_iter()
                .next();
            self.set_focused_terminal(next, cx);
        }
        self.overlay = Overlay::None;
        cx.notify();
    }

    // --- rendering ---

    /// The main area with no terminal open — a state the user chose, so it
    /// says so plainly rather than leaving a blank rectangle that reads as
    /// a crash.
    ///
    /// It does NOT list projects. The sidebar beside it already does, with
    /// pins, click-to-reopen and the live tabs above them; a second copy in
    /// the middle of the window would be the same rows twice with two sets
    /// of click behaviour to keep in step, which is precisely the kind of
    /// duplication this file keeps getting wrong. What the main area adds
    /// is the one action that always works — a new terminal, with the key
    /// that does it — and, when the list is not on screen, a way to put it
    /// there. `empty_state` decides which of those apply.
    fn render_empty_state(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = self.theme;
        let showing_projects = self.sidebar_open && self.sidebar_view == SidebarView::Projects;
        let state = empty_state(self.projects_cache.all().len(), showing_projects);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .text_size(px(11.0))
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgb(theme.ui_text))
                    .child("no terminals open"),
            )
            .child(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(theme.ui_text_muted))
                    .child(state.hint),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.chip_button(
                        "new terminal",
                        false,
                        |ws, window, cx| {
                            ws.add_tab(None, cx);
                            ws.focus_active_pane(window, cx);
                        },
                        cx,
                    ))
                    .children(state.show_projects_button.then(|| {
                        self.chip_button(
                            "show projects",
                            false,
                            |ws, window, cx| {
                                // Opens the sidebar AND switches it to
                                // projects, so this is the right button
                                // whether it was closed or on another view.
                                ws.open_sidebar(SidebarView::Projects, cx);
                                ws.focus_active_pane(window, cx);
                            },
                            cx,
                        )
                    }))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child("cmd-t"),
                    ),
            )
            .into_any_element()
    }

    fn render_tree(
        &self,
        node: &PaneNode,
        tab_index: usize,
        path: Vec<usize>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match node {
            PaneNode::Terminal { terminal_id, .. } => {
                let Some(pane) = self.panes.get(terminal_id) else {
                    return div().size_full().into_any_element();
                };
                let focused = self
                    .focused_terminal
                    .as_deref()
                    .is_some_and(|f| f == terminal_id);
                let views_local = pane.read(cx).target().is_local();
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
                        // Which machine this pane's terminal runs on, marked
                        // on the pane itself. The focused bar carries the
                        // full contract, but only for the FOCUSED pane —
                        // with a split full of terminals, one of them being
                        // somebody else's is not something to have to click
                        // to find out.
                        let views_remote = !views_local;
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
                            .children(views_remote.then(|| {
                                div()
                                    .px(px(4.0 * scale))
                                    .h(px(15.0 * scale))
                                    .flex()
                                    .items_center()
                                    .text_size(px(9.0 * scale))
                                    .text_color(rgb(theme.ui_accent))
                                    .child("remote")
                            }))
                            // Not on a remote pane: broadcast membership is
                            // keyed on local panes, so `toggle_member`
                            // matches nothing and the chip is a control that
                            // reports a state it cannot change. The Share
                            // icon is already hidden for the same reason.
                            .children((bc_on && !views_remote).then(|| {
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
                                    ws.set_focused_terminal(Some(id_split_h.clone()), cx);
                                    ws.split_focused(SplitDirection::Horizontal, cx);
                                    ws.focus_active_pane(window, cx);
                                }),
                            ))
                            .child(pane_btn("split-v").on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _, window, cx| {
                                    ws.set_focused_terminal(Some(id_split_v.clone()), cx);
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
                            // No timer on a pane that views another Mac.
                            // `set_auto_run` refuses it, so offering the
                            // chip meant the sheet opened, accepted a
                            // command, closed on Enter, and nothing ever
                            // ran — the silent no-op the search box was
                            // already fixed for. Named in
                            // `remote_limits_line` as well, so it reads as
                            // a stated limit rather than a broken button.
                            .children((!views_remote).then(|| {
                                pane_btn("timer").on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _, _, cx| {
                                        // Clear highlights BEFORE retargeting
                                        // focus, or the wrong pane gets cleared.
                                        ws.leave_search_highlights(cx);
                                        ws.set_focused_terminal(Some(id_timer.clone()), cx);
                                        ws.overlay = if ws.overlay == Overlay::AutoRun {
                                            Overlay::None
                                        } else {
                                            Overlay::AutoRun
                                        };
                                        cx.notify();
                                    }),
                                )
                            }))
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
        let target = pane.read(cx).target().clone();
        let cwd = pane.read(cx).cwd();
        let has_local_dir = local_context_available(&target, cwd.clone());
        // `None` for every local pane, so the bar below is byte-identical
        // for one. See `TerminalPane::remote_state`.
        let remote_state = pane.read(cx).remote_state();
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
                    // Directory control: for a local pane, shows the cwd and
                    // opens a folder picker (new tab at the chosen folder).
                    // A remote pane has no local directory to show or move
                    // — the control is disabled rather than silently
                    // opening a new LOCAL window at a picked path.
                    let display: SharedString = if has_local_dir {
                        cwd.expect("has_local_dir guarantees a cwd")
                            .replace(&std::env::var("HOME").unwrap_or_default(), "~")
                            .into()
                    } else {
                        match &target {
                            crate::hosts::Target::Local => "choose folder".into(),
                            crate::hosts::Target::Remote(id) => self.profile_label(id).into(),
                        }
                    };
                    let control = div()
                        .id("bar-cwd")
                        .max_w(px(280.0))
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(rgb(theme.ui_border))
                        .bg(rgb(theme.ui_surface))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(format!("dir: {display}")));
                    if target.is_local() {
                        control
                            .cursor_pointer()
                            .text_color(rgb(theme.ui_accent))
                            .hover(|style| style.border_color(rgb(theme.ui_accent)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _, window, cx| ws.open_folder(window, cx)),
                            )
                    } else {
                        control.text_color(rgb(theme.ui_text_muted))
                    }
                })
                // D5, while the pane is open. The peers sidebar states the
                // same contract before one is chosen; this is the copy that
                // is still on screen when the user reaches for Cmd+F, drags
                // a divider, or wonders why the scrollback stops. It also
                // carries the connection word, which is the only signal a
                // pane that never attached (`Refused`, `Incompatible`) has.
                .children(remote_state.map(|state| {
                    div()
                        .flex_grow()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.ui_text_muted))
                        .child(SharedString::from(format!(
                            "attached \u{b7} {state} \u{b7} {}",
                            crate::pane::remote_limits_line()
                        )))
                }))
                .children(self.swap_source.is_some().then(|| {
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(theme.yellow))
                        .child("click a pane to swap")
                })),
        )
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
    /// Copy the full buddy note (the surfaces show it truncated/ellipsized).
    fn copy_buddy_note(&self, cx: &mut Context<Self>) {
        if let Some(note) = self.buddy_note.clone() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(note));
        }
    }

    /// Stage the buddy note as input in the terminal it reviewed (or the
    /// focused one if that pane is gone) — no trailing newline, the user
    /// edits and submits. Same raw-bytes path as Cmd+V paste.
    fn insert_buddy_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.buddy_note.clone() else {
            return;
        };
        let Some(id) = self
            .buddy_source_pane
            .clone()
            .filter(|id| self.panes.contains_key(id))
            .or_else(|| self.focused_terminal.clone())
        else {
            return;
        };
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        // Same hazard class as the folder-picker guard: a locally-derived
        // note must never be typed into a pane running on another
        // machine. The `or_else` fallback above can land on
        // `focused_terminal` even when that pane isn't the local one the
        // note was drafted about, so the target is re-checked here right
        // before the write, not assumed from where the note came from.
        if !may_write_cd(pane.read(cx).target(), pane.read(cx).foreground_activity()) {
            return;
        }
        pane.update(cx, |pane, _| pane.write(note.into_bytes()));
        self.focus_terminal_by_id(&id, window, cx);
    }

    fn render_pet_bubble(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.settings.buddy_pet_visible {
            return None;
        }
        let note = self.pet_bubble.as_ref().map(|(text, _)| text.clone());
        let composing =
            note.is_none() && self.settings.buddy_enabled && self.buddy_gate.is_composing();
        if note.is_none() && !composing {
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
        if note.is_none() {
            // Composing: the buddy has seen changes and a note is on the
            // way — animated dots stepping on the pet tick. Note and dots
            // share one bubble surface, so the note replaces them in place.
            let dots = ".".repeat(1 + ((self.pet_tick_count / 2) % 3) as usize);
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
                .text_color(rgb(theme.ui_text_muted))
                .child(SharedString::from(dots));
            return Some(if above {
                bubble.bottom(px(vh - y + 6.0)).into_any_element()
            } else {
                bubble.top(px(y + PET_H + 6.0)).into_any_element()
            });
        }
        let text = note.expect("note present past the composing branch");
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
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                // Long notes scroll within the bubble's height budget
                // (the chip row below keeps a fixed slice of it).
                div()
                    .id("buddy-bubble-text")
                    .max_h(px((space - 34.0).max(16.0)))
                    .overflow_y_scroll()
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
                    .child(SharedString::from(text)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(self.chip_button(
                        "copy",
                        false,
                        |ws, _window, cx| ws.copy_buddy_note(cx),
                        cx,
                    ))
                    .child(self.chip_button(
                        "insert",
                        false,
                        |ws, window, cx| ws.insert_buddy_note(window, cx),
                        cx,
                    )),
            );
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
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .id("buddy-note")
                                    .max_w(px(420.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(10.0))
                                    .text_color(rgb(theme.ui_text_muted))
                                    .child(SharedString::from(note)),
                            )
                            .child(self.chip_button(
                                "copy",
                                false,
                                |ws, _window, cx| ws.copy_buddy_note(cx),
                                cx,
                            ))
                            .child(self.chip_button(
                                "insert",
                                false,
                                |ws, window, cx| ws.insert_buddy_note(window, cx),
                                cx,
                            ))
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
                    ws.tts_voice_list_open = false;
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
        // One cap for every sheet this function can open, so they cannot
        // drift to different heights.
        let sheet_max = Self::sheet_max_height(window);
        match self.overlay {
            Overlay::None => None,
            Overlay::SettingsSheet => {
                let current = self.settings.theme.clone();
                // Built only while the themes section is active — hidden
                // sections must not pay for the whole chip grid per render.
                let on_appearance = self.settings_section == SettingsSection::Appearance;
                let chips: Vec<_> = if !on_appearance {
                    Vec::new()
                } else {
                    themes::all_themes()
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
                        .collect()
                };
                let font_size = self.settings.font_size;

                // Section content — exactly ONE section shows at a time; the
                // nav column switches. This keeps every section fully in
                // view no matter how settings grow.
                let content: Vec<gpui::AnyElement> = match self.settings_section {
                    SettingsSection::Appearance => vec![
                        self.group_label("theme").into_any_element(),
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(6.0))
                            .children(chips)
                            .into_any_element(),
                        self.group_label("text").into_any_element(),
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_color(rgb(theme.ui_text_muted))
                            .child(div().w(px(72.0)).child("size"))
                            .child(self.stepper(
                                "font-size",
                                format!("{font_size:.0} px"),
                                |ws, _window, cx| ws.set_font_size(ws.settings.font_size - 1.0, cx),
                                |ws, _window, cx| ws.set_font_size(ws.settings.font_size + 1.0, cx),
                                cx,
                            ))
                            .into_any_element(),
                        self.render_font_family_row(window, cx).into_any_element(),
                        self.group_label("background").into_any_element(),
                        self.render_background_row(cx).into_any_element(),
                        self.group_label("theme file").into_any_element(),
                        self.render_theme_file_row(cx).into_any_element(),
                    ],
                    SettingsSection::Buddy => vec![self.render_buddy_row(cx).into_any_element()],
                    SettingsSection::Alerts => vec![self.render_alerts_row(cx).into_any_element()],
                    SettingsSection::Companion => vec![
                        self.render_previews_row(cx).into_any_element(),
                        self.group_label("peers").into_any_element(),
                        self.render_peers_row(cx).into_any_element(),
                    ],
                };

                let nav_item = |ws: &Self,
                                label: &'static str,
                                section: SettingsSection,
                                cx: &mut Context<Self>| {
                    let active = ws.settings_section == section;
                    div()
                        .id(SharedString::from(label))
                        .cursor_pointer()
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(4.0))
                        .text_color(rgb(if active {
                            theme.ui_accent
                        } else {
                            theme.ui_text_muted
                        }))
                        .when(active, |d| d.bg(rgb(theme.ui_surface)))
                        .hover(|style| style.bg(rgb(theme.ui_border)))
                        .child(SharedString::from(label))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, _, _, cx| {
                                ws.settings_section = section;
                                ws.tts_voice_list_open = false;
                                cx.notify();
                            }),
                        )
                };

                Some(
                    self.sheet("settings", "esc closes", sheet_max, cx)
                        // Fixed, not capped: see `settings_sheet_height`.
                        .h(Self::settings_sheet_height(window))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap(px(14.0))
                                // The body takes what the fixed frame has
                                // left and NOT a pixel more. `flex_grow`
                                // claims the leftover; `min_h(0)` is what
                                // lets it shrink below its own content --
                                // without it flexbox's automatic minimum
                                // size pins the row to its tallest child,
                                // so a long section lays out taller than
                                // the sheet and spills past the frame with
                                // no scroll path. Children stretch to this
                                // height (hence no `items_start`), which is
                                // what gives the scrolling column below a
                                // BOUNDED viewport to scroll inside; a
                                // scroll container sized to its own content
                                // never scrolls.
                                .flex_grow()
                                .min_h(px(0.0))
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(96.0))
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.0))
                                        // Rendered from ALL: a section
                                        // without a nav entry is a section
                                        // nobody can open.
                                        .children(SettingsSection::ALL.map(|section| {
                                            nav_item(self, section.label(), section, cx)
                                        }))
                                        // Build identity: the way to confirm
                                        // WHICH build is running (the hash
                                        // moves every commit; version won't).
                                        .child(
                                            div()
                                                .mt(px(10.0))
                                                .px(px(8.0))
                                                .flex()
                                                .flex_col()
                                                .gap(px(1.0))
                                                .text_size(px(9.0))
                                                .text_color(rgb(theme.ui_text_muted))
                                                .child(SharedString::from(format!(
                                                    "v{}",
                                                    crate::settings::APP_VERSION
                                                )))
                                                .child(SharedString::from(
                                                    crate::settings::build_hash(),
                                                ))
                                                .child(SharedString::from(
                                                    crate::settings::build_time(),
                                                )),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("settings-content")
                                        .flex_grow()
                                        // No max of its own: the row above is
                                        // bounded to the fixed frame and this
                                        // stretches to it, so the viewport
                                        // this scrolls inside is already the
                                        // right size. A second cap here would
                                        // either fight the frame or leave a
                                        // gap under short sections — the
                                        // thing a fixed frame exists to
                                        // stop. `min_h(0)` for the same
                                        // reason as the row: a tall section
                                        // must not push its own scroll
                                        // container open.
                                        .min_h(px(0.0))
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap(px(8.0))
                                        .children(content),
                                ),
                        )
                        .into_any_element(),
                )
            }
            Overlay::Search => {
                // D5 at the point of refusal. Search reads the grid the
                // SESSION owns; an attached pane has none, so `set_search`
                // was a silent no-op there — a box that swallows what is
                // typed into it. Refused with the reason instead, and the
                // field is never built, so there is nothing to type into.
                if let SearchOffer::Refused(reason) = search_offer(
                    self.focused_terminal
                        .as_ref()
                        .and_then(|id| self.panes.get(id))
                        .map(|pane| pane.read(cx).owns_grid()),
                ) {
                    return Some(
                        self.sheet("search", "esc closes", sheet_max, cx)
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(theme.ui_text_muted))
                                    .child(SharedString::from(reason)),
                            )
                            .into_any_element(),
                    );
                }
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
                    self.sheet(
                        "search",
                        "enter jumps to older matches - esc clears",
                        sheet_max,
                        cx,
                    )
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
                    self.sheet("auto-run", "enter starts - esc closes", sheet_max, cx)
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
                    self.sheet(
                        "sessions",
                        "enter saves - click loads - esc closes",
                        sheet_max,
                        cx,
                    )
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
            Overlay::PetCard => Some(self.render_pet_card(sheet_max, cx)),
        }
    }

    fn render_pet_card(
        &mut self,
        sheet_max: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
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

        self.sheet("buddy", "enter saves name - esc closes", sheet_max, cx)
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
    /// How tall a bottom sheet may get: most of the window, not a fixed
    /// 360px.
    ///
    /// 360 was the same absolute height whatever the window, so on anything
    /// tall it left the sheet squeezed into the bottom fifth while the
    /// terminal above it sat empty. A sheet is something the user opened on
    /// purpose; it can have the room.
    ///
    /// Floored so a very short window still gets a usable panel rather than
    /// a sliver, and capped below the full height so it always reads as a
    /// panel OVER the terminal rather than a screen of its own.
    fn sheet_max_height(window: &Window) -> gpui::Pixels {
        px(sheet_max_px(f32::from(window.viewport_size().height)))
    }

    /// The settings sheet's FIXED height — always this, never sized to its
    /// content.
    ///
    /// A max-height sheet grows to fit what is in it, and the settings
    /// sections hold very different amounts, so every section switch
    /// resized the panel under the pointer. Somewhere over half the window
    /// and CONSTANT is what a settings panel should be: the content scrolls
    /// inside it, the frame does not move.
    ///
    /// The other sheets keep the max-height behaviour deliberately — a
    /// one-line search box has no business occupying half the screen.
    fn settings_sheet_height(window: &Window) -> gpui::Pixels {
        px(settings_sheet_px(f32::from(window.viewport_size().height)))
    }

    /// A bottom sheet. `max_height` is the WINDOW-relative cap the caller
    /// computed; see [`Self::sheet_max_height`].
    fn sheet(
        &self,
        title: &'static str,
        hint: &'static str,
        max_height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        div()
            .id(SharedString::from(format!("sheet-{title}")))
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .max_h(max_height)
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
        PaneNode::Terminal {
            terminal_id,
            target,
        } => PaneNode::Terminal {
            terminal_id: mapping
                .get(terminal_id)
                .cloned()
                .unwrap_or_else(|| terminal_id.clone()),
            target: target.clone(),
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
        if let Some(id) = self.companion_pending_focus.take() {
            self.focus_terminal_by_id(&id, window, cx);
        }
        if self.pending_root_focus {
            self.pending_root_focus = false;
            // A sheet owns the keyboard while it is up, and its own
            // `close_overlay` calls `focus_active_pane` on the way out —
            // which lands on the root for exactly this reason. So drop the
            // request rather than yanking focus out of a field the user is
            // typing in.
            if self.overlay == Overlay::None {
                window.focus(&self.focus_handle);
            }
        }
        let theme = self.theme;
        let active_tree = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.active_pane().clone());

        let content = match active_tree {
            Some(tree) => self.render_tree(&tree, self.active_tab, Vec::new(), cx),
            // No tabs at all: the empty state. Also covers an `active_tab`
            // that somehow outran the list, which `Vec::get` answers the
            // same way rather than panicking.
            None => self.render_empty_state(cx),
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
                // Rename site 2 of 3 (click-away). Marked exactly as the
                // Enter path is — a flag set at one site and not its
                // siblings protects nothing.
                let mut named = None;
                if !name.is_empty() {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.label = name;
                        named = Some(tab.id.clone());
                    }
                }
                self.renamed_tabs.extend(named);
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
                if event.keystroke.key == "escape" {
                    if ws.overlay != Overlay::None {
                        ws.close_overlay(window, cx);
                    } else if ws.companion_flyout {
                        ws.companion_flyout = false;
                        cx.notify();
                    }
                }
            }))
            .on_action(cx.listener(|ws, _: &NewTab, window, cx| {
                ws.add_tab(None, cx);
                ws.focus_active_pane(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &NewWindow, window, cx| {
                // From the empty state there is no project to put a window
                // IN, so cmd-n opens a tab rather than doing nothing.
                match new_window_target(ws.active_tab, ws.tabs.len()) {
                    NewWindowTarget::InTab(index) => ws.new_window(index, None, cx),
                    NewWindowTarget::AsNewTab => ws.add_tab(None, cx),
                }
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
                ws.tts_voice_list_open = false;
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
            .children(self.render_companion_flyout(cx))
            .children(self.render_pet(window, cx))
            .children(self.render_pet_bubble(window, cx))
            .children(self.render_overlay(window, cx))
    }
}

#[cfg(test)]
mod peer_attach_tests {
    use super::{
        attached_tab_label, may_review_pane, may_share_terminal, peer_listing, peer_pollers_needed,
        peer_target, remote_target_label, search_offer, PeerListing, SearchOffer,
    };
    use crate::companion::auth::PeerId;
    use crate::hosts::{HostOs, ProfileId, RemoteProfile, ShellKind, Target};
    use crate::peer_client::discover::Reach;
    use crate::peer_client::sessions::{LastPoll, PeerSession};
    use crate::peers::{Grants, PeerRecord};
    use superterminal_core::activity::Activity;

    fn peer(id: &str, label: &str) -> PeerRecord {
        PeerRecord {
            id: PeerId(id.to_string()),
            host: format!("{label}.tail"),
            label: label.to_string(),
            secret: "abcdef0123456789abcdef0123456789".to_string(),
            grants: Grants::default(),
        }
    }

    fn profile(id: &str, label: &str) -> RemoteProfile {
        RemoteProfile {
            id: ProfileId(id.to_string()),
            label: label.to_string(),
            destination: "example.com".to_string(),
            user: None,
            port: None,
            os: HostOs::MacOs,
            shell: ShellKind::Zsh,
        }
    }

    fn session(id: &str, label: &str, alive: bool) -> PeerSession {
        PeerSession {
            id: id.to_string(),
            label: label.to_string(),
            alive,
            activity: Activity::Idle,
        }
    }

    fn ready() -> Reach {
        Reach::Ready(crate::peer_client::Endpoint {
            addr: "127.0.0.1:43110".parse().unwrap(),
            secret: "abcdef0123456789abcdef0123456789".to_string(),
        })
    }

    // --- D3e: a second shape of pane exists now ----------------------------

    #[test]
    fn only_a_pane_whose_terminal_runs_here_may_be_shared_with_a_peer() {
        // D3e. `BroadcastMap::share` is ungated, and was safe ONLY because
        // `spawn_pane` structurally could not produce a non-local pane.
        // This task produces one, so the invariant is asserted at the
        // writer instead of being true by coincidence.
        assert!(may_share_terminal(&Target::Local));
        assert!(!may_share_terminal(&peer_target(&PeerId("p1".into()))));
    }

    #[test]
    fn a_panes_peer_is_recoverable_from_the_target_it_was_built_with() {
        // The pane is built from the peer and the poller is later looked up
        // by it; if these two disagreed, a pane would report activity for a
        // terminal it is not showing.
        let id = PeerId("abc123".into());
        assert_eq!(
            peer_target(&id).profile_id().map(|p| p.0.clone()),
            Some("abc123".to_string())
        );
        assert!(!peer_target(&id).is_local(), "a peer pane is never local");
    }

    #[test]
    fn the_reviewer_never_observes_a_terminal_on_another_mac() {
        // The fourth site of the phase's recurring shape, and the only one
        // that was not inert: `buddy_tick` gates its repo PROBE on the
        // target but dispatched its utterance regardless, handing the agent
        // `visible_text()` — the empty local placeholder — for an attached
        // pane. The reviewer then comments on a screen that is not there.
        assert!(may_review_pane(&Target::Local));
        assert!(!may_review_pane(&peer_target(&PeerId("p1".into()))));
    }

    // --- one poller per peer, browsing included ----------------------------

    #[test]
    fn browsing_a_peer_keeps_its_poller_alive_before_any_pane_exists() {
        // The session list IS the poller's output, so the sweep that prunes
        // pollers no pane needs would otherwise kill the list being read.
        let a = PeerId("peer-a".into());
        let b = PeerId("peer-b".into());
        assert_eq!(peer_pollers_needed(&[], Some(&a)), vec![a.clone()]);
        assert_eq!(
            peer_pollers_needed(std::slice::from_ref(&b), Some(&a)),
            vec![b.clone(), a.clone()]
        );
        assert!(peer_pollers_needed(&[], None).is_empty());
        assert_eq!(
            peer_pollers_needed(std::slice::from_ref(&b), None),
            vec![b],
            "closing the sidebar must not disturb an attached pane's poller"
        );
    }

    // --- naming a remote pane ----------------------------------------------

    #[test]
    fn an_attached_pane_is_named_after_the_mac_it_is_watching() {
        let peers = vec![peer("abc", "mac studio")];
        let id = ProfileId("abc".into());
        assert_eq!(remote_target_label(&id, &[], &peers), "mac studio");
    }

    #[test]
    fn an_ssh_profile_still_wins_and_an_unknown_id_never_reads_as_local() {
        let profiles = vec![profile("abc", "build box")];
        let peers = vec![peer("abc", "mac studio")];
        let id = ProfileId("abc".into());
        assert_eq!(
            remote_target_label(&id, &profiles, &peers),
            "build box",
            "the pre-existing answer for every target that existed before this phase"
        );
        let orphan = ProfileId("zzz".into());
        let label = remote_target_label(&orphan, &profiles, &peers);
        assert!(label.contains("zzz"), "an unknown target must name its id");
        assert!(label.contains("missing"));
    }

    #[test]
    fn a_tab_for_an_opened_session_names_the_machine_first() {
        assert_eq!(
            attached_tab_label("mac studio", "work"),
            "mac studio \u{b7} work"
        );
        assert_eq!(
            attached_tab_label("mac studio", "  "),
            "mac studio",
            "a session with no label still names its machine"
        );
    }

    // --- D5 at the point of refusal ----------------------------------------

    #[test]
    fn the_search_sheet_refuses_an_attached_pane_out_loud() {
        assert_eq!(search_offer(Some(true)), SearchOffer::Field);
        match search_offer(Some(false)) {
            SearchOffer::Refused(reason) => {
                assert!(reason.contains("attached"), "{reason}");
                assert!(!reason.is_empty());
            }
            SearchOffer::Field => panic!("an attached pane must not be offered a search field"),
        }
        assert!(matches!(search_offer(None), SearchOffer::Refused(_)));
    }

    // --- what the sidebar shows under a peer -------------------------------

    #[test]
    fn a_peer_being_probed_says_so_rather_than_showing_an_empty_list() {
        assert_eq!(peer_listing(None, None), PeerListing::Probing);
        assert_eq!(
            peer_listing(Some(&ready()), None),
            PeerListing::Waiting,
            "reachable but not yet polled is not the same as sharing nothing"
        );
        assert_eq!(
            peer_listing(Some(&ready()), Some(LastPoll::Pending)),
            PeerListing::Waiting
        );
    }

    #[test]
    fn a_peer_that_could_not_be_found_shows_the_probes_own_reason() {
        for reach in [Reach::Unreachable, Reach::Refused, Reach::Incompatible] {
            let note = reach.note();
            assert_eq!(
                peer_listing(Some(&reach), None),
                PeerListing::Unreachable(note)
            );
            assert!(!note.is_empty());
        }
    }

    #[test]
    fn a_poll_that_could_not_be_read_is_never_shown_as_sharing_nothing() {
        // The hazard `parse_sessions` guards, one layer up. "Nothing
        // shared" invites the user to go turn sharing on; "could not be
        // read" is what a missing `view` grant actually looks like.
        assert_eq!(
            peer_listing(Some(&ready()), Some(LastPoll::Failed)),
            PeerListing::Unreadable
        );
        assert_eq!(
            peer_listing(Some(&ready()), Some(LastPoll::Listed(Vec::new()))),
            PeerListing::Nothing
        );
    }

    #[test]
    fn a_retired_session_is_never_offered_to_open() {
        // `alive: false` is the broadcaster already tearing that pane down.
        // Opening it would attach to a terminal that is ending.
        let poll = LastPoll::Listed(vec![
            session("t1", "work", true),
            session("t2", "closing", false),
        ]);
        match peer_listing(Some(&ready()), Some(poll)) {
            PeerListing::Sessions(sessions) => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, "t1");
            }
            other => panic!("expected a session list, got {other:?}"),
        }
        // ...and a list of nothing BUT retired sessions is not a list.
        assert_eq!(
            peer_listing(
                Some(&ready()),
                Some(LastPoll::Listed(vec![session("t2", "closing", false)]))
            ),
            PeerListing::Nothing
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::{ProfileId, Target};

    #[test]
    fn a_closed_project_reports_unknown_not_idle() {
        // The one rule `project_activity` exists for. `Activity::aggregate`
        // answers `Idle` for an empty set ("nothing is running"), which on
        // a project row would draw a GREEN dot — a shell sitting at a
        // prompt — for a project that is not even open.
        assert_eq!(project_activity(&[]), Activity::Unknown);
        assert_ne!(project_activity(&[]), Activity::Idle);
    }

    #[test]
    fn a_live_project_reports_its_terminals_aggregate() {
        assert_eq!(project_activity(&[Activity::Idle]), Activity::Idle);
        assert_eq!(
            project_activity(&[Activity::Idle, Activity::Busy]),
            Activity::Busy,
            "one working terminal makes the project working"
        );
        assert_eq!(
            project_activity(&[Activity::Idle, Activity::Unknown]),
            Activity::Unknown,
            "an unobserved terminal is not evidence of an idle one"
        );
    }

    #[test]
    fn a_peers_poller_survives_until_its_last_pane_closes() {
        // The whole point of keying pollers on the PEER: two panes open on
        // the same machine ask it one question. Closing one of them must
        // not stop the other's activity signal — and only when the last one
        // goes does the peer stop being polled at all.
        use crate::companion::auth::PeerId;
        let a = PeerId("peer-a".into());
        let b = PeerId("peer-b".into());
        let held = vec![a.clone(), b.clone()];

        // Two panes on A, one on B: nothing is dropped.
        assert!(pollers_to_drop(&held, &[a.clone(), a.clone(), b.clone()]).is_empty());
        // One of A's two panes closes: A is STILL needed.
        assert!(pollers_to_drop(&held, &[a.clone(), b.clone()]).is_empty());
        // A's last pane closes: only A goes, and B is untouched.
        assert_eq!(
            pollers_to_drop(&held, std::slice::from_ref(&b)),
            vec![a.clone()]
        );
        // Every pane closes: both go.
        let mut dropped = pollers_to_drop(&held, &[]);
        dropped.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(dropped, vec![a, b.clone()]);
        // A peer we hold no poller for is never "dropped" into existence.
        assert!(pollers_to_drop(&[], std::slice::from_ref(&b)).is_empty());
    }

    #[test]
    fn only_the_helper_assigns_focus() {
        // Focus and panel identity must change together. The periodic
        // refresh is gated on `sidebar_open && pet_tick_count % 3`, so any
        // raw assignment reintroduces a window where a remote pane is
        // focused while the panels still hold local authority — including
        // the git panel's destructive actions.
        //
        // The pattern deliberately has no trailing space: three of the
        // original fourteen sites assigned across a line break and a
        // trailing-space grep missed them.
        //
        // `focused_terminal` is a private field of `Workspace`, but
        // `settings_ui.rs` and `companion_ui.rs` are CHILD MODULES that
        // can write it directly (settings_ui.rs already reads it, at
        // line ~156) — a raw assignment added in either file is invisible
        // to a scan of mod.rs alone. So all three are scanned.
        let marker = "\nmod tests {";
        // Each entry: (file, source, expected `mod tests {}` anchors in
        // that file). mod.rs and settings_ui.rs each have their own test
        // module; companion_ui.rs has none — verified here, not assumed,
        // so a future test module added there fails loudly instead of
        // silently hiding raw assignments below it.
        let sources = [
            ("mod.rs", include_str!("mod.rs"), 1),
            ("settings_ui.rs", include_str!("settings_ui.rs"), 1),
            ("companion_ui.rs", include_str!("companion_ui.rs"), 0),
        ];

        let mut total = 0usize;
        let mut by_file = Vec::new();
        for (name, source, expected_anchors) in sources {
            let anchor_count = source.matches(marker).count();
            assert_eq!(
                anchor_count, expected_anchors,
                "expected {expected_anchors} `mod tests {{` anchor(s) in \
                 {name}; the scan below depends on this"
            );
            let production = if expected_anchors == 1 {
                source.split(marker).next().expect("anchor present")
            } else {
                source
            };
            let count = production.matches("focused_terminal =").count();
            total += count;
            by_file.push((name, count));
        }

        assert_eq!(
            total, 1,
            "found {total} raw `focused_terminal =` assignments in \
             production code across mod.rs, settings_ui.rs, and \
             companion_ui.rs; exactly one may exist, inside \
             set_focused_terminal; by file: {by_file:?}"
        );
    }

    #[test]
    fn the_helper_retargets_the_panels() {
        // Guards against the helper being reduced to a bare assignment.
        let source = include_str!("mod.rs");
        let helper = source
            .split("fn set_focused_terminal")
            .nth(1)
            .expect("set_focused_terminal must exist");
        let body_end = helper.find("\n    fn ").unwrap_or(helper.len());
        assert!(
            helper[..body_end].contains("retarget_panels"),
            "set_focused_terminal must retarget the panels in the same update"
        );
    }

    #[test]
    fn voice_names_survive_spaces_and_variants() {
        assert_eq!(
            parse_voice_name("Albert              en_US    # Hello!"),
            Some("Albert".to_string())
        );
        assert_eq!(
            parse_voice_name("Bad News            en_US    # ..."),
            Some("Bad News".to_string())
        );
        assert_eq!(
            parse_voice_name("Ava (Premium)       en_US    # ..."),
            Some("Ava (Premium)".to_string())
        );
        assert_eq!(parse_voice_name(""), None);
    }

    #[test]
    fn every_settings_section_is_reachable_from_the_nav() {
        // The nav renders from ALL, so a section missing here is a section
        // the user can never open.
        assert_eq!(SettingsSection::ALL.len(), 4);
        let labels: Vec<&str> = SettingsSection::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            vec!["appearance", "buddy", "alerts", "companion"],
            "nav order is deliberate: look first, then the assistants, then the phone"
        );
        for label in &labels {
            assert!(!label.is_empty());
            assert_eq!(*label, label.to_lowercase(), "sheet labels are lowercase");
        }
        let mut unique = labels.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "labels must be distinguishable");
    }

    #[test]
    fn the_sheet_opens_on_a_real_nav_entry() {
        // Opening on a section the nav cannot highlight would leave the
        // sidebar with nothing selected.
        assert_eq!(SettingsSection::DEFAULT, SettingsSection::Appearance);
        assert_eq!(SettingsSection::ALL[0], SettingsSection::DEFAULT);
    }

    #[test]
    fn say_command_injection_is_neutralized() {
        assert!(!neutralize_say_commands("[[rate 500]]").contains("[["));
        assert!(!neutralize_say_commands("[[[[volm 1]]").contains("[["));
        assert!(!neutralize_say_commands("[[[").contains("[["));
        assert_eq!(neutralize_say_commands("plain [text]"), "plain [text]");
    }

    #[test]
    fn the_folder_picker_refuses_a_remote_pane_even_when_idle() {
        // Two independent guards. This one does not trust the activity
        // signal at all, so a forged remote report cannot defeat it.
        assert!(!may_write_cd(
            &Target::Remote(ProfileId("p1".into())),
            Activity::Idle
        ));
        assert!(!may_write_cd(
            &Target::Remote(ProfileId("p1".into())),
            Activity::Busy
        ));
        assert!(!may_write_cd(
            &Target::Remote(ProfileId("p1".into())),
            Activity::Unknown
        ));
    }

    #[test]
    fn the_folder_picker_requires_a_positively_idle_local_pane() {
        assert!(may_write_cd(&Target::Local, Activity::Idle));
        assert!(!may_write_cd(&Target::Local, Activity::Busy));
        assert!(
            !may_write_cd(&Target::Local, Activity::Unknown),
            "Unknown must never authorise a write"
        );
    }

    #[test]
    fn a_remote_pane_offers_no_local_directory_context() {
        // Buddy probing and the focused-bar control both key off this.
        assert!(!local_context_available(
            &Target::Remote(ProfileId("p1".into())),
            None
        ));
        assert!(!local_context_available(
            &Target::Remote(ProfileId("p1".into())),
            Some("/tmp/repo".to_string())
        ));
        assert!(local_context_available(
            &Target::Local,
            Some("/tmp/repo".to_string())
        ));
        assert!(!local_context_available(&Target::Local, None));
    }

    #[test]
    fn a_remote_pane_may_never_be_offered_a_share_control() {
        // The sidebar Share icon, the share row, and `toggle_share`'s
        // mutation all key off this — a remote-target pane (including a
        // dead one from `spawn_dead_pane`, which never registers with the
        // hub) must never look shareable.
        assert!(!may_share_terminal(&Target::Remote(ProfileId("p1".into()))));
        assert!(may_share_terminal(&Target::Local));
    }

    #[test]
    fn every_mark_slot_lands_on_a_colour_of_its_own() {
        // `projects::project_mark` promises a slot below `MARK_SLOTS` and
        // spreads labels across all of them; that promise is worth nothing
        // if the palette here is shorter than the count, because the
        // modulo would quietly fold two slots onto one colour and the
        // spread the hash bought would be spent.
        for theme in [crate::themes::TOKYO_NIGHT, crate::themes::DRACULA] {
            let colors: Vec<u32> = (0..crate::projects::MARK_SLOTS)
                .map(|slot| project_mark_color(&theme, slot))
                .collect();
            let mut distinct = colors.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(
                distinct.len(),
                crate::projects::MARK_SLOTS,
                "{} gives two slots the same colour: {colors:x?}",
                theme.name
            );
            // A slot past the end wraps rather than panicking a render.
            assert_eq!(
                project_mark_color(&theme, crate::projects::MARK_SLOTS),
                colors[0]
            );
        }
    }

    #[test]
    fn a_marks_colour_is_the_themes_own_and_never_a_fixed_one() {
        // The whole reason the palette is resolved through the theme: a
        // custom theme can be any colours at all, and a mark hard-coded to
        // look right against one preset would clash with — or vanish into
        // — another.
        let slot =
            crate::projects::project_mark("chat", crate::projects::ProjectIcon::Generated).slot;
        assert_ne!(
            project_mark_color(&crate::themes::TOKYO_NIGHT, slot),
            project_mark_color(&crate::themes::DRACULA, slot),
            "two themes that share no palette must not paint one mark alike"
        );
    }

    #[test]
    fn closing_the_last_tab_leaves_no_tab_active_rather_than_underflowing() {
        // The whole point of this change: zero tabs is a legal state. The
        // code this replaced ended in `self.tabs.len() - 1`, which on an
        // empty Vec is a usize UNDERFLOW — a panic in an app that runs all
        // day, reachable the moment the last terminal closes.
        assert_eq!(active_tab_after_close(0, 0, 0), 0);
        // A stale active index cannot conjure a panic either.
        assert_eq!(active_tab_after_close(3, 7, 0), 0);

        // Removing a tab BEFORE the active one shifts it down, so the same
        // tab stays selected.
        assert_eq!(active_tab_after_close(0, 2, 3), 1);
        assert_eq!(active_tab_after_close(1, 2, 3), 1);
        // Removing one AFTER it leaves it where it is.
        assert_eq!(active_tab_after_close(2, 1, 3), 1);
        // Removing the active one keeps the index, which is now the tab
        // that took its place...
        assert_eq!(active_tab_after_close(1, 1, 3), 1);
        // ...unless it was the last, in which case it clamps back.
        assert_eq!(active_tab_after_close(2, 2, 2), 1);
        // Down to one tab, everything lands on it.
        assert_eq!(active_tab_after_close(0, 0, 1), 0);
        assert_eq!(active_tab_after_close(1, 1, 1), 0);
    }

    #[test]
    fn the_shortcut_for_a_new_window_still_produces_a_terminal_with_nothing_open() {
        // A window belongs to a project. With none open there is no
        // project to put one in, and `new_window` returns early on an
        // out-of-range index — so without this, cmd-n from the empty state
        // (which is now also the LAUNCH state) would silently do nothing
        // and the shortcut would be a dead key on the first screen a new
        // user ever sees.
        assert_eq!(new_window_target(0, 0), NewWindowTarget::AsNewTab);
        // A stale active index is the same case, never an index used raw.
        assert_eq!(new_window_target(4, 2), NewWindowTarget::AsNewTab);
        // With projects open it is still a window in the active one.
        assert_eq!(new_window_target(0, 1), NewWindowTarget::InTab(0));
        assert_eq!(new_window_target(2, 5), NewWindowTarget::InTab(2));
    }

    #[test]
    fn the_launch_screen_says_something_useful_before_any_project_exists() {
        // Startup opens no terminal, so this screen is the first thing a
        // brand-new user sees: no projects, no peers, an empty sidebar
        // beside it. It must not degrade to a bare heading — and it must
        // not offer to "show projects" when there are none, which would
        // open an empty list and read as a broken button.
        let first_launch = empty_state(0, true);
        assert!(!first_launch.hint.is_empty());
        assert!(!first_launch.show_projects_button);
        // The sidebar being shut changes nothing while there is nothing to
        // show in it.
        assert_eq!(empty_state(0, false), first_launch);

        // With projects remembered and the list already on screen, the
        // button would do nothing visible — so it is not drawn, and the
        // line points at the list instead.
        let listed = empty_state(3, true);
        assert!(!listed.show_projects_button);
        assert!(!listed.hint.is_empty());

        // With projects remembered and the list NOT on screen (sidebar
        // closed, or open on git/files/peers), offer to put it there.
        let hidden = empty_state(3, false);
        assert!(hidden.show_projects_button);
        assert!(!hidden.hint.is_empty());
        assert_ne!(hidden.hint, listed.hint);
    }

    #[test]
    fn nothing_respawns_a_shell_the_user_did_not_ask_for() {
        // The user's complaint, in one assertion: "when I close out of all
        // my work it opens up another terminal in ~ dir". Two close paths
        // did it and startup did it, and the recurring failure in this
        // repo is fixing one site and missing its sibling — so all of them
        // are scanned, together, in one test.
        //
        // `load_session` is here too: restoring a session saved from an
        // empty workspace must restore AN EMPTY WORKSPACE.
        let production = include_str!("mod.rs")
            .split("\nmod tests {")
            .next()
            .expect("the test module anchor `only_the_helper_assigns_focus` also depends on");

        for name in [
            "pub fn new(cx: &mut Context<Self>) -> Self {",
            "fn settle_after_tab_removal(",
            "fn close_terminal(",
            "fn close_tab(",
            "fn load_session(",
        ] {
            let after = production
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} must exist in mod.rs"));
            let body = &after[..after.find("\n    fn ").unwrap_or(after.len())];
            assert!(
                !body.contains("add_tab("),
                "{name} spawns a terminal nobody asked for"
            );
        }
    }

    #[test]
    fn the_empty_state_can_always_be_escaped_from_the_keyboard() {
        // gpui dispatches actions along the FOCUS path, and every binding
        // (cmd-t included) is registered on the workspace root, which is on
        // that path only while it holds focus. With no terminal there is no
        // pane to focus, so if nothing claims it the one screen whose whole
        // job is to offer a new terminal is the one where the shortcut for
        // a new terminal is dead. Both halves of the fix are asserted here
        // because they cover different paths and either alone leaves a hole.
        let production = include_str!("mod.rs")
            .split("\nmod tests {")
            .next()
            .expect("test module anchor");
        let body_of = |name: &str| {
            let after = production
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} must exist in mod.rs"));
            after[..after.find("\n    fn ").unwrap_or(after.len())].to_string()
        };

        // Paths that HAVE a Window (every key binding and click handler
        // calls this after acting).
        assert!(
            body_of("pub fn focus_active_pane(").contains("window.focus(&self.focus_handle)"),
            "with no pane to focus this must claim focus for the workspace \
             root, not quietly do nothing"
        );
        // Paths that do NOT: a shell that exits on its own arrives through
        // a pane event, which carries no Window. Render has one and
        // consumes this flag (verified by reading `impl Render`).
        assert!(
            body_of("fn settle_after_tab_removal(").contains("self.pending_root_focus = true"),
            "emptying the workspace from a Window-less path must still hand \
             focus back to the root on the next frame"
        );
        // Setting the flag is only half of it. Nothing consumes it except
        // render, and a flag nobody reads leaves the same dead screen as
        // never setting one — so the consumption is asserted too.
        assert!(
            production.contains("if self.pending_root_focus {"),
            "render must CONSUME pending_root_focus; setting it alone \
             changes nothing"
        );
    }

    #[test]
    fn the_two_ways_to_ask_for_a_window_make_the_same_choice() {
        // cmd-n and the folder picker's busy-terminal fallback both have to
        // decide "a window in the active project, or a whole new tab?", and
        // both are now reachable with no project open. They had the answer
        // written out separately, which is exactly how this codebase has
        // produced ten near-identical bugs: one site updated, its twin
        // left behind. One function, two call sites, asserted here.
        let production = include_str!("mod.rs")
            .split("\nmod tests {")
            .next()
            .expect("test module anchor");
        assert_eq!(
            production
                .matches("new_window_target(ws.active_tab, ws.tabs.len())")
                .count(),
            2,
            "cmd-n and the folder picker must both route through new_window_target"
        );
    }

    #[test]
    fn the_post_close_active_index_is_decided_in_exactly_one_place() {
        // The underflow lived in five lines that were duplicated verbatim
        // across `close_terminal` and `close_tab`. Making them a function
        // is only worth something while nothing computes the answer
        // inline again — the second copy is where the bug always comes
        // back.
        let production = include_str!("mod.rs")
            .split("\nmod tests {")
            .next()
            .expect("test module anchor");
        assert!(
            !production.contains("self.active_tab -= 1"),
            "the index shift belongs to active_tab_after_close"
        );
        assert!(
            !production.contains("self.active_tab.min("),
            "the clamp belongs to active_tab_after_close - on an empty Vec \
             its `len() - 1` argument is a usize underflow"
        );
        assert_eq!(
            production.matches("active_tab_after_close(").count(),
            2,
            "one definition, one call site: both close paths go through \
             settle_after_tab_removal"
        );
    }

    #[test]
    fn the_settings_sheet_takes_over_half_the_window() {
        // The requirement in the user's own words: settings should "always
        // take up over 50% of the screen, not adjust over and over". The
        // second half (not adjusting) is the fixed `.h()`; THIS is the
        // first half, and it is the part a later tweak to the ratio could
        // silently undo.
        for viewport in [600.0, 900.0, 1200.0, 1600.0, 2400.0] {
            let height = settings_sheet_effective_px(viewport);
            assert!(
                height > viewport * 0.5,
                "settings sheet is {height} in a {viewport} window, not over half"
            );
        }
    }

    #[test]
    fn the_settings_sheet_never_out_grows_the_sheet_cap() {
        // Two independent numbers decide this height and they cross over,
        // so "which one wins" is not the same answer at every size: below
        // roughly 444px the shared cap is smaller and governs, above it the
        // fixed height does. Either way the sheet may never exceed the cap
        // every other sheet obeys.
        for viewport in [300.0, 400.0, 444.0, 500.0, 800.0, 1600.0] {
            assert!(
                settings_sheet_effective_px(viewport) <= sheet_max_px(viewport),
                "settings sheet escapes the sheet cap at {viewport}"
            );
        }
        // The crossover is real, not theoretical — both branches are live.
        assert_eq!(settings_sheet_effective_px(400.0), sheet_max_px(400.0));
        assert_eq!(
            settings_sheet_effective_px(1600.0),
            settings_sheet_px(1600.0)
        );
    }

    #[test]
    fn the_settings_sheet_fits_inside_the_window_it_floats_over() {
        // A sheet taller than its own window has no scroll path out: the
        // rows past the bottom edge are simply unreachable. Both floors
        // (320 asked for, 260 capped) are absolute, so this holds only
        // while the window is at least as tall as the CAP's floor -- and
        // it is: `main.rs` sets `window_min_size` to 400x300, so 300 is
        // the shortest window that exists and the first case below.
        for viewport in [260.0, 300.0, 361.0, 400.0, 768.0, 1440.0] {
            let height = settings_sheet_effective_px(viewport);
            assert!(
                height <= viewport,
                "settings sheet is {height} in a {viewport} window — taller than the window"
            );
        }
        // Below 260 the shared cap stops tracking the window and the sheet
        // would overhang -- pinned here as a landmine, not a live bug:
        // `window_min_size` puts that size out of reach today, and the day
        // someone lowers that minimum this is what fails. The fix would
        // belong in `sheet_max_px`, which every sheet shares, not in the
        // settings-only height.
        assert!(settings_sheet_effective_px(200.0) > 200.0);
    }

    #[test]
    fn a_nested_sidebar_row_sits_right_of_the_row_it_hangs_under() {
        // The reported bug, in one assertion: "the project title is
        // positioned like a sub-bullet of the terminal rather than the
        // other way around". Terminal rows used a bare `pl(20)` while the
        // project card above them was inset 4, padded 6, and spent 12 on a
        // fold triangle -- so the PROJECT's dot sat at 28 and its own
        // TERMINAL's at 20. The child bullet was left of the parent's and
        // the tree read upside down.
        let project = sidebar_bullet_x(0);
        for depth in 1..=2u8 {
            assert!(
                sidebar_bullet_x(depth) > project,
                "depth {depth} bullet at {} is not right of the project's {project}",
                sidebar_bullet_x(depth)
            );
        }
        // Strictly deeper each step, not merely different.
        assert!(sidebar_bullet_x(2) > sidebar_bullet_x(1));
    }

    #[test]
    fn a_nested_sidebar_row_is_never_wider_than_its_parent() {
        // The other half of what was reported: "the Project is skinnier
        // than the terminals under it". The project card carried
        // `mx(4)` and the rows under it carried none, so each child was
        // 8px wider than the card it belonged to. Every row now starts at
        // the same inset, and `sidebar_child_pad_left` subtracts exactly
        // that inset -- which is the whole reason it is not just
        // `sidebar_bullet_x`.
        for depth in 0..=2u8 {
            assert_eq!(
                SIDEBAR_ROW_INSET + sidebar_child_pad_left(depth),
                sidebar_bullet_x(depth),
                "a depth {depth} row does not start from the shared inset"
            );
        }
    }

    #[test]
    fn no_sidebar_row_hard_codes_its_own_indent() {
        // Arithmetic tests cannot see a row that never consults the
        // ladder, and a bare `pl` is exactly how this broke: both child
        // rows had one, each plausible on its own, neither agreeing with
        // the project card. Source-text, because gpui layout is not
        // testable here.
        // EVERY row builder in the sidebar, not just the ones in this
        // file. `render_share_row` lives in companion_ui.rs, which is
        // exactly why the first pass missed it: it kept a bare `pl(30)`
        // and drew the share panel left of, and wider than, the terminal
        // it hangs under -- the reported bug surviving its own fix, one
        // module over.
        let mod_rs = include_str!("mod.rs")
            .split("\nmod tests {")
            .next()
            .expect("the test module anchor");
        let companion = include_str!("companion_ui.rs");
        // Bound at the next item at the SAME indentation, whatever its
        // visibility. `render_share_row` is `pub(super) fn`, so a lone
        // "\n    fn " needle never finds its end and the scan runs
        // silently to the end of the file -- passing for the wrong
        // reason, over a region it does not mean.
        fn body_of<'a>(source: &'a str, name: &str) -> &'a str {
            let after = source
                .split(name)
                .nth(1)
                .unwrap_or_else(|| panic!("{name} must exist"));
            let end = ["\n    fn ", "\n    pub(super) fn ", "\n    pub fn "]
                .iter()
                .filter_map(|needle| after.find(needle))
                .min()
                .unwrap_or(after.len());
            &after[..end]
        }

        for (source, name) in [
            (mod_rs, "fn render_projects_view"),
            (companion, "fn render_share_row"),
        ] {
            let body = body_of(source, name);
            assert!(
                body.len() > 400,
                "{name} body came out at {} chars — the scan is not \
                 bounding what it thinks it is",
                body.len()
            );
            // The bound actually held: no second item at this indent.
            for needle in ["\n    fn ", "\n    pub(super) fn ", "\n    pub fn "] {
                assert!(
                    !body.contains(needle),
                    "{name} body ran past its own end into the next item"
                );
            }
            for literal in [
                ".pl(px(16.0))",
                ".pl(px(20.0))",
                ".pl(px(30.0))",
                ".mx(px(4.0))",
            ] {
                assert!(
                    !body.contains(literal),
                    "{name} hard-codes {literal} instead of going through \
                     the SIDEBAR_* ladder"
                );
            }
        }
    }
}
