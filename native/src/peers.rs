//! Paired peer instances: another SuperTerminal on another machine.
//!
//! Pairing rather than tailnet-membership because the tailnet includes a WORK
//! MacBook, which may carry MDM or IT admin access. Per-peer secrets give
//! individual revocation and a label the user established rather than one a
//! peer asserts about itself.
//!
//! A bearer secret is a capability, not an identity proof: an administrator on
//! the peer machine who can read the stored secret can replay it. Keypairs
//! would prevent that; this is a stated limit, not an oversight.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::companion::auth::PeerId;

/// What a peer is allowed to do here. Every grant defaults OFF: a record
/// missing its grants must never mean "allow".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Grants {
    /// See broadcast sessions at all.
    pub view: bool,
    /// Send input to them. Named `type_` because `type` is a keyword.
    #[serde(rename = "type")]
    pub type_: bool,
    /// Create new terminals here.
    pub spawn: bool,
}

/// Hand-written so `secret` cannot reach a log line, a panic message or a
/// `{:?}` in a test failure. Same reasoning as `peer_client::Endpoint`'s,
/// which redacts the same value one file over — deriving it here left the
/// hazard that type exists to avoid.
impl std::fmt::Debug for PeerRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerRecord")
            .field("id", &self.id)
            .field("host", &self.host)
            .field("label", &self.label)
            .field("secret", &"<redacted>")
            .field("grants", &self.grants)
            .finish()
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerRecord {
    pub id: PeerId,
    /// The tailnet hostname this peer was paired from (`Candidate::host` at
    /// pairing time, see [`pair`]). Distinct from `label`, which is
    /// user-facing and may one day be renamed: `offerable_candidates`
    /// matches against THIS field, never `label`, so renaming a peer can
    /// never make an already-paired machine reappear as offerable.
    /// `#[serde(default)]` so a peer record saved before this field
    /// existed still loads — its origin host is simply unknown until the
    /// peer is re-paired.
    #[serde(default)]
    pub host: String,
    pub label: String,
    /// 32 lowercase hex chars. Compared in constant time at auth.
    pub secret: String,
    #[serde(default)]
    pub grants: Grants,
}

#[derive(Debug, PartialEq)]
pub struct PeerProblem {
    pub label: String,
    pub reason: String,
}

const SECRET_LEN: usize = 32;

pub fn new_peer_secret() -> String {
    crate::companion::auth::generate_token()
}

/// Fresh identifier for a newly paired peer. Same generator as the secret:
/// an id carries no confidentiality requirement, only uniqueness, and a
/// second generator would just be a second place to get the hex format
/// wrong.
pub fn new_peer_id() -> String {
    crate::companion::auth::generate_token()
}

fn secret_ok(secret: &str) -> bool {
    secret.len() == SECRET_LEN
        && secret
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Permissive loading, mirroring `hosts::load_profiles`' reasoning: settings fall
/// back to `Settings::default()` on ANY serde error, so one hand-edited peer
/// must not be able to reset every unrelated setting.
///
/// Quarantines EVERY member of a duplicate id — and every member of a duplicate
/// SECRET, because two peers sharing a secret are indistinguishable at auth
/// time and neither could be trusted to carry its own grants.
pub fn load_peers(raw: &serde_json::Value) -> (Vec<PeerRecord>, Vec<PeerProblem>) {
    let mut problems = Vec::new();
    let Some(items) = raw.as_array() else {
        if !raw.is_null() {
            problems.push(PeerProblem {
                label: String::new(),
                reason: "peers is not a list".to_string(),
            });
        }
        return (Vec::new(), problems);
    };
    let mut candidates: Vec<PeerRecord> = Vec::new();
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let parsed: PeerRecord = match serde_json::from_value(item.clone()) {
            Ok(peer) => peer,
            Err(error) => {
                problems.push(PeerProblem {
                    label,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if parsed.id.0.is_empty() {
            problems.push(PeerProblem {
                label,
                reason: "empty id".into(),
            });
            continue;
        }
        if !secret_ok(&parsed.secret) {
            problems.push(PeerProblem {
                label,
                reason: "bad secret".into(),
            });
            continue;
        }
        candidates.push(parsed);
    }
    let mut kept = Vec::new();
    for peer in &candidates {
        let id_dupes = candidates.iter().filter(|o| o.id == peer.id).count();
        let secret_dupes = candidates
            .iter()
            .filter(|o| o.secret == peer.secret)
            .count();
        if id_dupes > 1 {
            problems.push(PeerProblem {
                label: peer.label.clone(),
                reason: format!("duplicate id {}", peer.id.0),
            });
        } else if secret_dupes > 1 {
            problems.push(PeerProblem {
                label: peer.label.clone(),
                reason: "duplicate secret".to_string(),
            });
        } else {
            kept.push(peer.clone());
        }
    }
    (kept, problems)
}

// ---------------------------------------------------------------------
// Discovery: candidates from `tailscale status --json`, pairing, and the
// decision of whether a peer-settings mutation must reach the running
// companion immediately.
// ---------------------------------------------------------------------

/// A tailnet peer that COULD be paired: not yet a `PeerRecord`, just
/// something `tailscale status --json` reported as an online desktop
/// machine. Promotion to a peer is always an explicit user action — the
/// tailnet also holds an Android phone, which must never become offerable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub host: String,
    pub addr: String,
    pub os: String,
}

/// Tailscale-reported `OS` values this app treats as "another desktop that
/// could run SuperTerminal". Everything else (the tailnet's Android phone,
/// iOS, etc.) is excluded here, not by the caller.
fn is_desktop_os(os: &str) -> bool {
    matches!(os, "macOS" | "linux" | "windows")
}

/// Parse `tailscale status --json` into candidates: online, desktop peers
/// only. Never panics — a peer entry missing or misusing a field is simply
/// dropped, and a malformed container (wrong shape, invalid JSON, `null`,
/// non-object) yields no candidates, matching `companion::blender`'s
/// temperament for absent or malformed input.
pub fn parse_tailscale_status(raw: &str) -> Vec<Candidate> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(peers) = value.get("Peer").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for peer in peers.values() {
        let Some(host) = peer.get("HostName").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(os) = peer.get("OS").and_then(|v| v.as_str()) else {
            continue;
        };
        let online = peer
            .get("Online")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !online || !is_desktop_os(os) {
            continue;
        }
        let Some(addr) = peer
            .get("TailscaleIPs")
            .and_then(|v| v.as_array())
            .and_then(|ips| ips.first())
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        candidates.push(Candidate {
            host: host.to_string(),
            addr: addr.to_string(),
            os: os.to_string(),
        });
    }
    // A JSON object backed by a hash map iterates in arbitrary order; a
    // candidate list that reshuffles on every scan would look broken.
    candidates.sort_by(|a, b| a.host.cmp(&b.host));
    candidates
}

/// Total budget for the `tailscale status` subprocess — a wedged
/// `tailscaled` costs one click, never a hung settings sheet.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Never slurp an unbounded reply.
const SCAN_MAX_BYTES: usize = 1024 * 1024;

/// Shell `tailscale status --json` and parse it. One attempt, no hot
/// retry: `tailscale` missing from PATH, a wedged daemon, or output past
/// the cap all yield an empty list — the feature is simply absent, never
/// an error dialog. Same bounded-probe discipline as
/// `companion::blender::capture_once`.
/// Absolute locations the Tailscale CLI is actually installed to, tried in
/// order. A GUI-launched macOS app inherits a minimal
/// `PATH=/usr/bin:/bin:/usr/sbin:/sbin` from launchd, which contains NONE
/// of these — so resolving the bare name `tailscale` through `PATH` finds
/// the CLI only when the app happened to be started from a shell.
///
/// That is what made peer discovery asymmetric in practice: two machines on
/// the same tailnet, the same build, and one could see the other while the
/// reverse found nothing at all, purely because of where Tailscale was
/// installed and how the app was launched.
///
/// `/usr/local/bin/tailscale` is the shim the standalone app installs (it
/// execs the path below it); Homebrew on Apple Silicon uses
/// `/opt/homebrew/bin`; the last entry is the binary inside the app bundle
/// itself, which exists even when no shim was ever installed.
const TAILSCALE_BINARIES: &[&str] = &[
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

/// First candidate that exists, else the bare name so a `PATH` that DOES
/// carry it (a shell-launched app, or Linux) still works.
fn resolve_binary(candidates: &[&str], exists: impl Fn(&str) -> bool) -> String {
    candidates
        .iter()
        .find(|c| exists(c))
        .map(|c| (*c).to_string())
        .unwrap_or_else(|| "tailscale".to_string())
}

fn tailscale_binary() -> String {
    resolve_binary(TAILSCALE_BINARIES, |p| std::path::Path::new(p).exists())
}

pub fn scan_candidates() -> Vec<Candidate> {
    match shell_bounded(
        &tailscale_binary(),
        &["status", "--json"],
        SCAN_TIMEOUT,
        SCAN_MAX_BYTES,
    ) {
        Some(raw) => parse_tailscale_status(&raw),
        None => Vec::new(),
    }
}

/// Bounded subprocess call: run `program` with `args`, capturing stdout up
/// to `max_bytes` within a hard total `timeout`. `None` on any failure —
/// missing binary, non-UTF8 output, a process still running past the
/// deadline, or output over the cap. The reader runs on its own thread so
/// the deadline is real wall-clock time, not a per-read timeout a trickling
/// process could keep resetting forever.
fn shell_bounded(
    program: &str,
    args: &[&str],
    timeout: std::time::Duration,
    max_bytes: usize,
) -> Option<String> {
    use std::io::Read;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let ok = loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break true,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.len() > max_bytes {
                        break false;
                    }
                }
                Err(_) => break false,
            }
        };
        // The receiver may already be gone (deadline blew past this send);
        // that is not this thread's problem to report.
        let _ = tx.send(ok.then_some(buf));
    });
    let result = rx.recv_timeout(timeout).ok().flatten();
    // Always reap: past the deadline the reader thread may still be
    // blocked on a wedged pipe, but the child itself must never be left
    // running loose — a later scan must not stack up abandoned processes.
    let _ = child.kill();
    let _ = child.wait();
    String::from_utf8(result?).ok()
}

/// Which candidates the settings UI should offer. A host already paired
/// (matched by `PeerRecord::host`, its stable origin, never by the
/// user-facing `label`) is not offered again — pairing the same machine
/// twice would just mint a second, indistinguishable credential for it.
/// Promotion itself stays an explicit user action; this only prunes the
/// list they choose from.
pub fn offerable_candidates(candidates: &[Candidate], paired: &[PeerRecord]) -> Vec<Candidate> {
    candidates
        .iter()
        .filter(|candidate| !paired.iter().any(|peer| peer.host == candidate.host))
        .cloned()
        .collect()
}

/// A brand-new pairing: fresh id, fresh secret, every grant OFF. The
/// running companion still needs an explicit restart before this record is
/// actually recognized — pairing alone only produces the record; see
/// `peer_mutation_requires_restart`.
impl Grants {
    /// What a peer gets the moment you pair it, which is deliberately NOT
    /// [`Grants::default`].
    ///
    /// `default` must stay deny-all because it is what `#[serde(default)]`
    /// hands a record whose grants are missing or malformed on disk — there,
    /// silence must never mean "allow". Pairing is the opposite situation:
    /// it is an explicit act, with a secret exchanged by hand, and a peer
    /// that can do nothing afterwards just reads as broken.
    ///
    /// `view` and `type_` are on because broadcasting is ALREADY opt-in per
    /// terminal — nothing is exposed until you share it, so these two only
    /// govern what happens to something you already chose to share. Making
    /// them a second closed gate means two switches for the ordinary case.
    ///
    /// `spawn` stays off, because it is a different kind of permission: it
    /// lets a peer start new processes on this machine without you having
    /// shared anything at all.
    pub fn on_pair() -> Self {
        Grants {
            view: true,
            type_: true,
            spawn: false,
        }
    }
}

/// The one shape a peer record is ever built in, whether its secret was
/// minted here ([`pair`]) or pasted from the other Mac
/// ([`accept_pasted_pairing`]). Written once so the two can never drift:
/// an accepted pairing is deliberately no more and no less trusted than a
/// minted one, and a second constructor is how that stops being true.
fn record_for(host: &str, secret: String) -> PeerRecord {
    PeerRecord {
        id: PeerId(new_peer_id()),
        host: host.to_string(),
        label: host.to_string(),
        secret,
        grants: Grants::on_pair(),
    }
}

pub fn pair(host: &str) -> PeerRecord {
    record_for(host, new_peer_secret())
}

// ---------------------------------------------------------------------
// Accepting a pairing minted on ANOTHER machine.
//
// `pair` mints a fresh secret, which is the right thing for exactly one of
// the two Macs. If BOTH mint, each holds a secret the other has never
// seen and neither recognises the other at all: `companion::auth::
// principal_for` matches on the SECRET alone, so there is nothing else for
// it to fall back on. The counterpart of that same fact is what makes the
// fix a single paste -- one shared secret authenticates BOTH directions,
// because ids, labels and grants are local and never have to agree between
// the two machines.
// ---------------------------------------------------------------------

/// Why a pasted pairing string could not be used.
///
/// Fixed strings, never built from what was pasted: a reason that echoed
/// the paste would put a secret into a dialog, a log line, or a failing
/// test's output -- the same hazard `PeerRecord`'s hand-written `Debug`
/// exists to close.
pub const PASTE_EMPTY: &str = "nothing was copied - copy the pairing link on the other Mac first";
pub const PASTE_URL_WITHOUT_CODE: &str = "that link carries no pairing code after its #";
pub const PASTE_NOT_A_CODE: &str = "a pairing code is 32 lowercase hex characters";

/// What a pasted pairing string carried.
///
/// `Debug` is hand-written for the same reason `PeerRecord`'s is: this
/// type holds a secret for the moment between a clipboard read and a
/// settings write, which is exactly the window in which a `{:?}` would
/// leak it.
#[derive(Clone, PartialEq)]
pub struct PastedPairing {
    /// The address a pairing URL pointed at (`100.x.x.x`), when the paste
    /// was a URL rather than a bare code.
    ///
    /// Deliberately NOT what gets stored as `PeerRecord::host`: a record's
    /// host must be a tailnet HOSTNAME, because `Workspace::probe_peer`
    /// finds a peer's address by matching that field against a scanned
    /// `Candidate::host`. A record holding `100.64.0.2` there would match
    /// no candidate and be permanently unreachable. The address is used
    /// only to cross-check the machine the user picked -- see
    /// [`accept_pasted_pairing`].
    pub addr: Option<String>,
    /// Already validated by `secret_ok`: no path constructs this struct
    /// without passing that check first.
    pub secret: String,
}

impl std::fmt::Debug for PastedPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PastedPairing")
            .field("addr", &self.addr)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Split a pasted URL into `(host, fragment)`. `None` when the text is not
/// a URL at all, which is how a bare code is told apart from a link.
///
/// Hand-rolled rather than pulling in a URL crate: the only shape that has
/// to be understood is the one `settings_ui::peer_pairing_url` produces --
/// `http://<addr>:<port>/#<secret>` -- and anything this cannot make sense
/// of falls through to being refused, never guessed at.
fn split_pairing_url(text: &str) -> Option<(&str, Option<&str>)> {
    let after_scheme = text.split_once("://")?.1;
    let (before_fragment, fragment) = match after_scheme.split_once('#') {
        Some((head, tail)) => (head, Some(tail)),
        None => (after_scheme, None),
    };
    let authority = before_fragment
        .split(['/', '?'])
        .next()
        .unwrap_or(before_fragment);
    // Userinfo first, then the port -- and a bracketed IPv6 literal keeps
    // its own colons, which is why the port is only stripped when there
    // are no brackets to be inside of.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.rfind(']') {
        Some(end) => host[..=end].trim_start_matches('[').trim_end_matches(']'),
        None => host.rsplit_once(':').map_or(host, |(host, _)| host),
    };
    (!host.is_empty()).then_some((host, fragment))
}

/// Parse what the user pasted into "an address, maybe, and a secret".
///
/// Both forms the design calls for are accepted because both occur: a full
/// pairing URL, which carries the address as well, and a bare 32-hex code
/// that arrived over a message rather than a scan.
///
/// The secret is validated with the SAME `secret_ok` the settings loader
/// uses, so a paste that `load_peers` would quarantine on the next launch
/// is refused now, with a reason, rather than stored to fail mysteriously
/// later.
pub fn parse_pairing_paste(pasted: &str) -> Result<PastedPairing, &'static str> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() {
        return Err(PASTE_EMPTY);
    }
    if let Some((host, fragment)) = split_pairing_url(trimmed) {
        let Some(fragment) = fragment.filter(|f| !f.is_empty()) else {
            return Err(PASTE_URL_WITHOUT_CODE);
        };
        if !secret_ok(fragment) {
            return Err(PASTE_NOT_A_CODE);
        }
        return Ok(PastedPairing {
            addr: Some(host.to_string()),
            secret: fragment.to_string(),
        });
    }
    if !secret_ok(trimmed) {
        return Err(PASTE_NOT_A_CODE);
    }
    Ok(PastedPairing {
        addr: None,
        secret: trimmed.to_string(),
    })
}

/// A peer record carrying a secret that came from the OTHER Mac, for the
/// discovered candidate `host` the user pointed at. The missing half of
/// pairing: without it, both machines mint and neither can authenticate.
///
/// `host` comes from a `Candidate`, never from the paste, for the reason
/// spelled out on [`PastedPairing::addr`]. A URL's address is used only to
/// catch the user accepting the right link on the wrong row.
///
/// Refusals, in the order they are checked:
///
/// - a malformed code, so nothing unusable is ever written;
/// - a secret some other record already holds, because two peers sharing
///   one are indistinguishable at auth time and `load_peers` would
///   quarantine BOTH on the next launch;
/// - a link whose address belongs to a DIFFERENT discovered Mac;
/// - a host already paired, which would mean a second, redundant
///   credential for one machine.
pub fn accept_pasted_pairing(
    pasted: &str,
    host: &str,
    candidates: &[Candidate],
    paired: &[PeerRecord],
) -> Result<PeerRecord, String> {
    let parsed = parse_pairing_paste(pasted).map_err(str::to_string)?;
    // Plain `==`, not `token_matches`: a local uniqueness check against
    // records this machine already holds, not an authentication boundary,
    // and there is no remote party whose timing it could leak to.
    if let Some(existing) = paired.iter().find(|p| p.secret == parsed.secret) {
        return Err(format!("{} already holds that code", existing.label));
    }
    if let Some(addr) = parsed.addr.as_deref() {
        // Only when the address is one the scan actually reported: a link
        // built from a LAN address, or from a scan since gone stale, is
        // not evidence that the user picked the wrong machine, and the row
        // they clicked is an explicit choice either way.
        if let Some(named) = candidates.iter().find(|c| c.addr == addr) {
            if named.host != host {
                return Err(format!("that link is for {}, not {host}", named.host));
            }
        }
    }
    if paired.iter().any(|p| p.host == host) {
        return Err(format!("{host} is already paired"));
    }
    Ok(record_for(host, parsed.secret))
}

/// Which grant a peer row's toggle acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantKind {
    View,
    Type,
    Spawn,
}

/// What a grant toggle means: flip exactly the one field named, leave the
/// other two exactly as they were.
pub fn toggled_grants(grants: Grants, which: GrantKind) -> Grants {
    match which {
        GrantKind::View => Grants {
            view: !grants.view,
            ..grants
        },
        GrantKind::Type => Grants {
            type_: !grants.type_,
            ..grants
        },
        GrantKind::Spawn => Grants {
            spawn: !grants.spawn,
            ..grants
        },
    }
}

/// THE blocking decision for peer mutations: whether swapping the
/// companion's frozen peer snapshot from `before` to `after` must happen
/// through an immediate stop+restart rather than waiting for the next
/// manual toggle. `ServerConfig.peers` (see `companion::server`) is
/// resolved once at server start and never refreshed — nothing restarts
/// the companion when settings change on its own — so without this, a
/// peer deleted, or narrowed, keeps its OLD authority live until the
/// server is next toggled by hand, possibly the whole app session.
/// Shipping deletion as "revocation" on top of that would silently reopen
/// the exact hole per-peer pairing exists to close.
///
/// Deliberately unconditional on direction: a narrowed grant or a deleted
/// peer must restart because the old snapshot would otherwise keep
/// authorizing exactly what was just revoked, but a widened grant or a
/// freshly paired peer restarts too — a peer mutation that "mostly" takes
/// effect immediately is one nobody can reason about, and it is also the
/// only way a fresh pairing's secret becomes recognizable at all. See
/// `workspace::settings_ui::apply_peer_mutation`, which is `regenerate_
/// companion_token`'s stop-then-toggle pattern applied to every peer
/// mutation.
pub fn peer_mutation_requires_restart(before: &[PeerRecord], after: &[PeerRecord]) -> bool {
    before != after
}

/// Peers a per-terminal share control should offer: only those whose
/// grants let them actually view a shared session. Sharing with a peer
/// that cannot view would be a silent no-op the user would have to debug
/// — `/sessions` and `/stream` are gated on `Grants::view` at the auth
/// layer (`companion::auth::admits_with_grants`) regardless of
/// `BroadcastMap` visibility, so offering a peer without it would just be
/// a control that lies about what it does.
pub fn shareable_peers(peers: &[PeerRecord]) -> Vec<&PeerRecord> {
    peers.iter().filter(|p| p.grants.view).collect()
}

// ---------------------------------------------------------------------
// BroadcastMap: which terminals a peer may see, held on the Workspace.
// ---------------------------------------------------------------------

/// Which peers each still-live terminal is shared with. This is the
/// Workspace's own record of visibility — deliberately NOT the
/// `companion::hub::Hub`'s, because the hub is rebuilt from scratch on
/// every companion start and every forced restart (a peer grant narrowed or
/// revoked forces exactly that restart; see `peer_mutation_requires_restart`
/// and `workspace::settings_ui::apply_peer_mutation`). Without a copy that
/// outlives the hub, editing ANY peer's grants would silently un-share
/// every terminal from every OTHER peer too.
///
/// This cannot instead be persisted to disk: terminal ids are only stable
/// within one running `Workspace` (`Workspace::fresh_id` restarts the
/// counter at `term-1` in a new process, and `load_session` deliberately
/// mints fresh ids per leaf — see its doc comment). A map keyed by
/// `terminal_id` that survived a process restart would point at ids that no
/// longer mean anything. Ids ARE stable across a companion restart within
/// one app run, which is the exact scope of the bug this exists to fix — so
/// this lives on the `Workspace`, which outlives the companion, and is
/// replayed into a freshly built `Hub` in
/// `workspace::companion_ui::prepare_companion_hub`.
///
/// Every production mutation of hub visibility must mirror through here in
/// the same operation, and every terminal-removal path must prune it — see
/// that module and `Workspace::close_terminal` / `close_tab` /
/// `load_session`.
#[derive(Debug, Clone, Default)]
pub struct BroadcastMap(HashMap<String, HashSet<PeerId>>);

impl BroadcastMap {
    /// Record `peer` as allowed to see `id`. Idempotent.
    ///
    /// Deliberately does NOT refuse a non-local id itself, unlike
    /// `companion::hub::Hub::set_visible_to`'s refusal of a non-`LocalPty`
    /// origin: `Hub` can self-check because it already tracks each
    /// registered id's `Origin`, but `BroadcastMap` is pure `id -> peers`
    /// bookkeeping with no notion of a pane's target at all — giving it one
    /// here would mean threading `Target` through every call site (and
    /// every existing test) just to duplicate a fact the `Workspace`
    /// already holds on `self.panes`. The guard instead lives at
    /// `BroadcastMap`'s one production caller,
    /// `workspace::companion_ui::toggle_share`, which already has that pane
    /// in hand — see `workspace::may_share_terminal`. That still closes the
    /// gap this doc warns about: a caller reaching `share` directly, rather
    /// than through the (also gated) sidebar icon, gets no enforcement from
    /// this type alone. `Hub::set_visible_to` remains the backstop that
    /// actually matters for authority — it fails closed regardless of what
    /// this map records — so the risk of a second bypass here is a
    /// misleading "shared" in the UI, not a data leak.
    pub fn share(&mut self, id: &str, peer: &PeerId) {
        self.0
            .entry(id.to_string())
            .or_default()
            .insert(peer.clone());
    }

    /// Revoke `peer`'s visibility of `id`. A no-op — never an empty
    /// leftover entry — if `id` was never shared with anyone. Called by
    /// the sidebar's per-terminal share toggle
    /// (`workspace::companion_ui::toggle_share`).
    pub fn unshare(&mut self, id: &str, peer: &PeerId) {
        if let Some(peers) = self.0.get_mut(id) {
            peers.remove(peer);
        }
    }

    /// Peers `id` is currently shared with, sorted by peer id string so
    /// callers (tests, and the sidebar share row) see a stable order — the
    /// backing `HashSet`'s own iteration order is not stable.
    pub fn peers_for(&self, id: &str) -> Vec<PeerId> {
        let mut peers: Vec<PeerId> = self
            .0
            .get(id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        peers.sort_by(|a, b| a.0.cmp(&b.0));
        peers
    }

    /// Drop every recorded terminal id that is not in `live_ids`. Call this
    /// on every removal path (single close, tab close, session-load
    /// rebuild) — a stale id left behind would silently re-share a future
    /// terminal that happens to reuse it.
    pub fn prune_to(&mut self, live_ids: &[String]) {
        let live: HashSet<&str> = live_ids.iter().map(String::as_str).collect();
        self.0.retain(|id, _| live.contains(id.as_str()));
    }

    /// Remove `peer` from every terminal's share set. Call this on peer
    /// deletion: the design deliberately allows recreating a deleted peer
    /// WITH THE SAME id, for identity recovery, so without this a
    /// recreated peer would silently inherit shares granted to its
    /// predecessor. Unlike a deleted PEER, a deleted terminal has no such
    /// recovery path — that stays `prune_to`'s job.
    pub fn forget_peer(&mut self, peer: &PeerId) {
        for peers in self.0.values_mut() {
            peers.remove(peer);
        }
    }

    /// Every `(terminal_id, peers)` pair currently recorded, for replaying
    /// into a freshly built `Hub`. Iteration order is not meaningful — the
    /// caller applies one `set_visible_to` per peer regardless of order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &HashSet<PeerId>)> {
        self.0.iter().map(|(id, peers)| (id.as_str(), peers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    // -------------------------------------------------------------
    // BroadcastMap: the Workspace-side record of who a still-live terminal
    // is shared with, replayed into a freshly built Hub on every companion
    // (re)start. See the type's doc comment for why this cannot live on the
    // Hub itself.
    // -------------------------------------------------------------

    #[test]
    fn sharing_is_per_terminal_and_per_peer() {
        let mut map = BroadcastMap::default();
        let p1 = PeerId("p1".into());
        let p2 = PeerId("p2".into());
        map.share("t1", &p1);
        assert_eq!(map.peers_for("t1"), vec![p1.clone()]);
        assert!(
            map.peers_for("t2").is_empty(),
            "sharing leaked to another terminal"
        );
        map.share("t1", &p2);
        assert_eq!(map.peers_for("t1").len(), 2);
        map.unshare("t1", &p1);
        assert_eq!(map.peers_for("t1"), vec![p2]);
    }

    #[test]
    fn nothing_is_shared_by_default() {
        let map = BroadcastMap::default();
        assert!(map.peers_for("t1").is_empty());
    }

    #[test]
    fn pruning_drops_terminals_that_no_longer_exist() {
        // A closed terminal's id must not linger and be re-shared if a future
        // id ever collides with it.
        let mut map = BroadcastMap::default();
        let p1 = PeerId("p1".into());
        map.share("gone", &p1);
        map.share("alive", &p1);
        map.prune_to(&["alive".to_string()]);
        assert!(map.peers_for("gone").is_empty());
        assert_eq!(map.peers_for("alive"), vec![p1]);
    }

    #[test]
    fn unsharing_a_terminal_never_shared_is_a_no_op() {
        let mut map = BroadcastMap::default();
        map.unshare("t1", &PeerId("p1".into()));
        assert!(map.peers_for("t1").is_empty());
    }

    #[test]
    fn deleting_a_peer_forgets_every_share_it_held() {
        // The design deliberately allows recreating a deleted peer WITH THE
        // SAME id for identity recovery. Without this, a recreated peer
        // would silently inherit shares granted to its predecessor.
        let mut map = BroadcastMap::default();
        let gone = PeerId("gone".into());
        let stays = PeerId("stays".into());
        map.share("t1", &gone);
        map.share("t1", &stays);
        map.share("t2", &gone);
        map.forget_peer(&gone);
        assert!(map.peers_for("t1") == vec![stays.clone()]);
        assert!(map.peers_for("t2").is_empty());
    }

    #[test]
    fn forgetting_a_peer_never_shared_is_a_no_op() {
        let mut map = BroadcastMap::default();
        map.share("t1", &PeerId("stays".into()));
        map.forget_peer(&PeerId("never-shared".into()));
        assert_eq!(map.peers_for("t1"), vec![PeerId("stays".into())]);
    }

    // -------------------------------------------------------------
    // shareable_peers: which peers a per-terminal share control offers.
    // -------------------------------------------------------------

    fn sample(label: &str) -> PeerRecord {
        PeerRecord {
            id: PeerId(format!("id-{label}")),
            host: format!("{label}.local"),
            label: label.to_string(),
            secret: "aabbccddeeff00112233445566778899".to_string(),
            grants: Grants::default(),
        }
    }

    #[test]
    fn only_peers_that_may_view_are_offered_a_share() {
        // Sharing with a peer that cannot view is a no-op the user would
        // have to debug. Offer only peers whose grants let them actually
        // see it.
        let can = PeerRecord {
            grants: Grants {
                view: true,
                ..Default::default()
            },
            ..sample("a")
        };
        let cannot = PeerRecord {
            grants: Grants::default(),
            ..sample("b")
        };
        let all = vec![can.clone(), cannot];
        let offered: Vec<&str> = shareable_peers(&all)
            .iter()
            .map(|p| p.label.as_str())
            .collect();
        assert_eq!(offered, vec!["a"]);
    }

    #[test]
    fn no_peers_means_nothing_to_offer() {
        assert!(shareable_peers(&[]).is_empty());
    }

    const OK: &str = r#"[{"id":"p1","label":"work","secret":"aabbccddeeff00112233445566778899","grants":{"view":true,"type":false,"spawn":false}}]"#;

    #[test]
    fn a_well_formed_peer_loads_with_its_grants() {
        let (ok, problems) = load_peers(&raw(OK));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "work");
        assert!(ok[0].grants.view);
        assert!(!ok[0].grants.type_);
        assert!(!ok[0].grants.spawn);
        assert!(problems.is_empty());
    }

    #[test]
    fn every_grant_defaults_off_when_absent() {
        // A peer record missing its grants must not silently mean "allow".
        let (ok, _) = load_peers(&raw(
            r#"[{"id":"p1","label":"work","secret":"aabbccddeeff00112233445566778899"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert!(!ok[0].grants.view);
        assert!(!ok[0].grants.type_);
        assert!(!ok[0].grants.spawn);
    }

    #[test]
    fn a_partial_grants_object_leaves_the_rest_off() {
        let json = r#"[{"id":"p1","label":"a","secret":"aabbccddeeff00112233445566778899","grants":{"view":true}}]"#;
        let (ok, _) = load_peers(&raw(json));
        assert_eq!(ok.len(), 1);
        assert!(ok[0].grants.view);
        assert!(!ok[0].grants.type_, "type defaulted ON");
        assert!(!ok[0].grants.spawn, "spawn defaulted ON");
    }

    #[test]
    fn duplicate_ids_quarantine_every_colliding_peer() {
        // Never first-wins or last-wins: an ambiguous id must not be able to
        // resolve to the wrong machine's grants.
        let (ok, problems) = load_peers(&raw(
            r#"[{"id":"dup","label":"a","secret":"aabbccddeeff00112233445566778899"},
                {"id":"dup","label":"b","secret":"99887766554433221100ffeeddccbbaa"},
                {"id":"solo","label":"c","secret":"0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "c");
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn a_duplicate_secret_quarantines_every_peer_sharing_it() {
        // Two peers with the same secret are indistinguishable at auth time,
        // so neither may be trusted to carry its own grants.
        let (ok, problems) = load_peers(&raw(
            r#"[{"id":"p1","label":"a","secret":"aabbccddeeff00112233445566778899"},
                {"id":"p2","label":"b","secret":"aabbccddeeff00112233445566778899"}]"#,
        ));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn a_malformed_container_yields_no_peers_rather_than_an_error() {
        let (ok, problems) = load_peers(&raw("5"));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_short_or_non_hex_secret_is_refused() {
        for bad in [
            "",
            "abc",
            "ZZZZccddeeff00112233445566778899",
            "aabbccddeeff0011223344556677889",
        ] {
            let json = format!(r#"[{{"id":"p1","label":"a","secret":"{bad}"}}]"#);
            let (ok, problems) = load_peers(&raw(&json));
            assert!(ok.is_empty(), "accepted secret {bad:?}");
            assert_eq!(problems.len(), 1);
        }
    }

    #[test]
    fn an_uppercase_secret_is_refused() {
        // Valid hex, correct length, wrong case. `is_ascii_hexdigit()` is
        // TRUE for A-F, so this case is what the explicit uppercase guard
        // exists for -- without it, the same secret would be storable in two
        // forms and a revocation could miss one.
        let json = r#"[{"id":"p1","label":"a","secret":"AABBCCDDEEFF00112233445566778899"}]"#;
        let (ok, problems) = load_peers(&raw(json));
        assert!(ok.is_empty(), "uppercase hex secret was accepted");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn generated_secrets_are_unique_and_full_width() {
        let a = new_peer_secret();
        let b = new_peer_secret();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn generated_ids_are_unique() {
        assert_ne!(new_peer_id(), new_peer_id());
    }

    // -------------------------------------------------------------
    // Discovery
    // -------------------------------------------------------------

    #[test]
    fn tailscale_peers_become_candidates() {
        let json = r#"{"Peer":{"k1":{"HostName":"work-mbp","TailscaleIPs":["100.64.0.2"],"OS":"macOS","Online":true}}}"#;
        let found = parse_tailscale_status(json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].host, "work-mbp");
        assert_eq!(found[0].addr, "100.64.0.2");
    }

    #[test]
    fn offline_and_non_desktop_peers_are_not_offered() {
        // The tailnet also holds an Android phone, which is not a peer host.
        let json = r#"{"Peer":{
            "k1":{"HostName":"pixel","TailscaleIPs":["100.64.0.3"],"OS":"android","Online":true},
            "k2":{"HostName":"off","TailscaleIPs":["100.64.0.4"],"OS":"macOS","Online":false}}}"#;
        assert!(parse_tailscale_status(json).is_empty());
    }

    #[test]
    fn malformed_status_yields_no_candidates_rather_than_panicking() {
        for bad in ["", "null", "{}", "not json", r#"{"Peer":5}"#] {
            assert!(
                parse_tailscale_status(bad).is_empty(),
                "panicked or accepted {bad:?}"
            );
        }
    }

    #[test]
    fn multiple_online_desktop_peers_come_back_sorted_by_host() {
        let json = r#"{"Peer":{
            "k1":{"HostName":"zeta","TailscaleIPs":["100.64.0.9"],"OS":"linux","Online":true},
            "k2":{"HostName":"alpha","TailscaleIPs":["100.64.0.8"],"OS":"windows","Online":true}}}"#;
        let found = parse_tailscale_status(json);
        let hosts: Vec<&str> = found.iter().map(|c| c.host.as_str()).collect();
        assert_eq!(hosts, vec!["alpha", "zeta"]);
    }

    #[test]
    fn a_peer_missing_an_ip_is_dropped_not_panicked_on() {
        let json = r#"{"Peer":{"k1":{"HostName":"work-mbp","TailscaleIPs":[],"OS":"macOS","Online":true}}}"#;
        assert!(parse_tailscale_status(json).is_empty());
    }

    #[test]
    fn shell_bounded_returns_stdout_on_a_quick_command() {
        let out = shell_bounded(
            "/bin/echo",
            &["hi"],
            std::time::Duration::from_secs(2),
            1024,
        );
        assert_eq!(out.as_deref(), Some("hi\n"));
    }

    #[test]
    fn shell_bounded_gives_up_at_the_deadline_rather_than_hanging() {
        let start = std::time::Instant::now();
        let out = shell_bounded(
            "/bin/sleep",
            &["5"],
            std::time::Duration::from_millis(100),
            1024,
        );
        assert!(out.is_none());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "the deadline was not enforced -- a wedged tailscaled would hang the settings sheet"
        );
    }

    #[test]
    fn shell_bounded_refuses_output_past_the_cap() {
        // /usr/bin/yes floods stdout forever; a real cap must cut it off
        // long before the (generous) timeout would.
        let out = shell_bounded("/usr/bin/yes", &[], std::time::Duration::from_secs(2), 16);
        assert!(out.is_none());
    }

    #[test]
    fn shell_bounded_absent_binary_yields_none_not_a_panic() {
        assert!(shell_bounded(
            "definitely-not-a-real-binary-xyz",
            &[],
            std::time::Duration::from_secs(1),
            1024
        )
        .is_none());
    }

    // -------------------------------------------------------------
    // Pairing surface: pure decisions the UI only renders.
    // -------------------------------------------------------------

    fn candidate(host: &str) -> Candidate {
        Candidate {
            host: host.to_string(),
            addr: "100.64.0.2".to_string(),
            os: "macOS".to_string(),
        }
    }

    fn other_candidate(host: &str, addr: &str) -> Candidate {
        Candidate {
            addr: addr.to_string(),
            ..candidate(host)
        }
    }

    #[test]
    fn pairing_mints_a_labelled_record_with_a_valid_secret() {
        // Grants are asserted separately in
        // `pairing_grants_view_and_type_but_never_spawn` — this one covers
        // the identity half of `pair()`.
        let record = pair("work-mbp");
        assert_eq!(record.label, "work-mbp");
        assert!(secret_ok(&record.secret), "pair() must mint a valid secret");
        assert!(!record.id.0.is_empty());
    }

    #[test]
    fn pairing_the_same_host_twice_mints_different_credentials() {
        // Two pairings of the same machine are two separate secrets; the
        // caller decides whether to offer a re-pair, not this function.
        let a = pair("work-mbp");
        let b = pair("work-mbp");
        assert_ne!(a.id, b.id);
        assert_ne!(a.secret, b.secret);
    }

    #[test]
    fn an_already_paired_host_is_not_offered_again() {
        let candidates = vec![candidate("work-mbp"), candidate("other-mac")];
        let paired = vec![pair("work-mbp")];
        let offered = offerable_candidates(&candidates, &paired);
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].host, "other-mac");
    }

    #[test]
    fn with_no_peers_paired_every_candidate_is_offerable() {
        let candidates = vec![candidate("a"), candidate("b")];
        assert_eq!(offerable_candidates(&candidates, &[]).len(), 2);
    }

    #[test]
    fn two_peers_sharing_a_label_are_matched_by_their_own_host_distinctly() {
        // Labels are user-editable and not unique (unlike ids, which are
        // opaque and quarantined on collision — see `load_peers`). Simulate
        // two paired peers that ended up sharing a display label — a future
        // rename, or a hand-edited settings file — while their real origin
        // hosts stay distinct.
        let mut a = pair("mac-a");
        a.label = "shared-name".to_string();
        let mut b = pair("mac-b");
        b.label = "shared-name".to_string();
        assert_eq!(a.label, b.label, "test setup: labels must collide");
        assert_ne!(a.host, b.host, "test setup: hosts must not collide");

        let candidates = vec![candidate("mac-a"), candidate("mac-b"), candidate("mac-c")];
        let offered = offerable_candidates(&candidates, &[a, b]);

        // Each already-paired host is suppressed by ITS OWN host, not by
        // the label the two peers happen to share; the untouched candidate
        // is unaffected.
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].host, "mac-c");
    }

    #[test]
    fn a_renamed_peer_still_suppresses_its_origin_host() {
        // Matching used to key off `label`, which the settings sheet may
        // one day let a user edit. If that match had stayed on `label`,
        // renaming a peer away from its origin hostname would make that
        // machine reappear as offerable — inviting a second, redundant
        // pairing of a machine already paired.
        let mut renamed = pair("work-mbp");
        renamed.label = "Tomas's other Mac".to_string();

        let candidates = vec![candidate("work-mbp")];
        let offered = offerable_candidates(&candidates, &[renamed]);
        assert!(
            offered.is_empty(),
            "a renamed peer's origin host must stay suppressed"
        );
    }

    #[test]
    fn a_grant_toggle_flips_only_the_named_field() {
        let base = Grants::default();
        let viewed = toggled_grants(base, GrantKind::View);
        assert!(viewed.view);
        assert!(!viewed.type_);
        assert!(!viewed.spawn);

        let typed = toggled_grants(viewed, GrantKind::Type);
        assert!(typed.view, "an unrelated toggle must not clear view");
        assert!(typed.type_);
        assert!(!typed.spawn);

        let back = toggled_grants(typed, GrantKind::View);
        assert!(!back.view, "toggling twice must return to the original");
    }

    #[test]
    fn spawn_toggle_is_independent_of_the_other_two() {
        let base = Grants {
            view: true,
            type_: true,
            spawn: false,
        };
        let toggled = toggled_grants(base, GrantKind::Spawn);
        assert!(toggled.spawn);
        assert!(toggled.view);
        assert!(toggled.type_);
    }

    // -------------------------------------------------------------
    // Accepting a pairing minted on the OTHER Mac. `pair` above covers
    // the machine that mints; these cover the one that pastes, which is
    // the half that did not exist and without which two Macs could not
    // authenticate at all.
    // -------------------------------------------------------------

    /// Stands in for the secret the other Mac minted and showed. Fixed,
    /// so a test failure cannot be mistaken for a real credential.
    const PASTED: &str = "0123456789abcdef0123456789abcdef";

    fn link_to(addr: &str, secret: &str) -> String {
        format!("http://{addr}:43110/#{secret}")
    }

    #[test]
    fn a_pairing_url_carries_both_its_address_and_its_code() {
        let parsed = parse_pairing_paste(&link_to("100.64.0.2", PASTED)).unwrap();
        assert_eq!(parsed.addr.as_deref(), Some("100.64.0.2"));
        assert_eq!(parsed.secret, PASTED);
    }

    #[test]
    fn a_bare_code_carries_no_address_to_cross_check_against() {
        let parsed = parse_pairing_paste(PASTED).unwrap();
        assert_eq!(parsed.addr, None);
        assert_eq!(parsed.secret, PASTED);
    }

    #[test]
    fn a_code_survives_the_whitespace_a_copy_brings_with_it() {
        assert_eq!(
            parse_pairing_paste(&format!("  {PASTED}\n"))
                .unwrap()
                .secret,
            PASTED
        );
        assert_eq!(
            parse_pairing_paste(&format!("\t{}  \n", link_to("100.64.0.2", PASTED)))
                .unwrap()
                .secret,
            PASTED
        );
    }

    #[test]
    fn an_uppercase_code_is_refused_exactly_as_the_loader_refuses_one() {
        // Same `secret_ok` on both sides. Hex case does not change the
        // value, but `token_matches` compares STRINGS -- an uppercase copy
        // would never match at auth, and would be a second storable form
        // of one credential that a revocation could miss.
        assert_eq!(
            parse_pairing_paste(&PASTED.to_uppercase()),
            Err(PASTE_NOT_A_CODE)
        );
    }

    #[test]
    fn a_link_with_no_code_in_it_says_so_rather_than_blaming_the_code() {
        for bare in [
            "http://100.64.0.2:43110/",
            "http://100.64.0.2:43110/#",
            "https://example.com",
        ] {
            assert_eq!(
                parse_pairing_paste(bare),
                Err(PASTE_URL_WITHOUT_CODE),
                "wrong reason for {bare:?}"
            );
        }
    }

    #[test]
    fn a_link_whose_fragment_is_not_a_secret_is_refused() {
        for bad in [
            "http://100.64.0.2:43110/#nope",
            "http://100.64.0.2:43110/#zzzzccddeeff00112233445566778899",
        ] {
            assert_eq!(
                parse_pairing_paste(bad),
                Err(PASTE_NOT_A_CODE),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn an_empty_paste_is_refused_with_its_own_reason() {
        for empty in ["", "   ", "\n\t"] {
            assert_eq!(parse_pairing_paste(empty), Err(PASTE_EMPTY));
        }
    }

    #[test]
    fn something_that_is_neither_a_link_nor_a_code_is_refused() {
        for junk in [
            "hello world",
            "0123456789abcdef",
            "0123456789abcdef0123456789abcdefff",
        ] {
            assert_eq!(
                parse_pairing_paste(junk),
                Err(PASTE_NOT_A_CODE),
                "accepted {junk:?}"
            );
        }
    }

    #[test]
    fn accepting_stores_the_pasted_secret_instead_of_minting_one() {
        // THE gap this closes: two Macs that each minted hold secrets
        // neither has ever seen, so neither recognises the other.
        let record = accept_pasted_pairing(
            &link_to("100.64.0.2", PASTED),
            "mac-a",
            &[candidate("mac-a")],
            &[],
        )
        .unwrap();
        assert_eq!(record.secret, PASTED, "a fresh secret was minted instead");
        assert_eq!(record.host, "mac-a");
        assert_eq!(record.label, "mac-a");
        assert!(!record.id.0.is_empty());
    }

    #[test]
    fn an_accepted_pairing_is_trusted_exactly_as_much_as_a_minted_one() {
        let accepted = accept_pasted_pairing(PASTED, "mac-a", &[], &[]).unwrap();
        assert_eq!(accepted.grants, Grants::on_pair());
        assert!(
            !accepted.grants.spawn,
            "spawn must stay opt-in whichever way a pairing was made"
        );
        let minted = pair("mac-a");
        assert_eq!(
            PeerRecord {
                id: minted.id.clone(),
                secret: minted.secret.clone(),
                ..accepted
            },
            minted,
            "an accepted record must differ from a minted one only in where its secret came from"
        );
    }

    #[test]
    fn a_secret_another_record_already_holds_is_refused() {
        // `load_peers` quarantines EVERY member of a duplicate secret, so
        // creating one by hand would revoke both peers at the next launch
        // -- and until then the two would be indistinguishable at auth.
        let mut held = pair("mac-b");
        held.secret = PASTED.to_string();
        assert!(accept_pasted_pairing(PASTED, "mac-a", &[], &[held]).is_err());
    }

    #[test]
    fn a_link_for_a_different_discovered_mac_is_refused() {
        let candidates = vec![candidate("mac-a"), other_candidate("mac-b", "100.64.0.3")];
        let err = accept_pasted_pairing(&link_to("100.64.0.3", PASTED), "mac-a", &candidates, &[])
            .unwrap_err();
        assert!(
            err.contains("mac-b"),
            "the reason must name the machine the link is really for: {err}"
        );
    }

    #[test]
    fn a_link_from_an_address_no_scan_reported_still_pairs_the_chosen_row() {
        // A link built from a LAN address, or a scan since gone stale, is
        // not evidence that the user picked the wrong machine -- and the
        // row's host is the only one `probe_peer` could ever resolve.
        let record = accept_pasted_pairing(
            &link_to("192.168.1.5", PASTED),
            "mac-a",
            &[candidate("mac-a")],
            &[],
        )
        .unwrap();
        assert_eq!(record.host, "mac-a");
    }

    #[test]
    fn a_host_already_paired_is_not_paired_a_second_time() {
        let err = accept_pasted_pairing(PASTED, "mac-a", &[], &[pair("mac-a")]).unwrap_err();
        assert!(err.contains("mac-a"), "{err}");
    }

    #[test]
    fn a_malformed_paste_never_becomes_a_record() {
        let upper = PASTED.to_uppercase();
        for bad in [
            "",
            "hello",
            upper.as_str(),
            "http://100.64.0.2:43110/",
            "http://100.64.0.2:43110/#nope",
        ] {
            assert!(
                accept_pasted_pairing(bad, "mac-a", &[], &[]).is_err(),
                "stored {bad:?}"
            );
        }
    }

    #[test]
    fn no_refusal_ever_echoes_the_pasted_secret() {
        // A reason is shown to the user and can reach a log line or a
        // failing test's output; it must never carry the credential.
        let mut held = pair("mac-b");
        held.secret = PASTED.to_string();
        let candidates = vec![candidate("mac-a"), other_candidate("mac-b", "100.64.0.3")];
        let wrong_row = link_to("100.64.0.3", PASTED);
        let cases: Vec<(&str, Vec<PeerRecord>)> = vec![
            (wrong_row.as_str(), Vec::new()),
            (PASTED, vec![held]),
            (PASTED, vec![pair("mac-a")]),
        ];
        for (pasted, paired) in cases {
            let err = accept_pasted_pairing(pasted, "mac-a", &candidates, &paired).unwrap_err();
            assert!(
                !err.contains(PASTED),
                "a refusal leaked the pasted secret: {err}"
            );
        }
    }

    #[test]
    fn a_pasted_pairing_never_shows_its_secret_in_debug_output() {
        let parsed = parse_pairing_paste(&link_to("100.64.0.2", PASTED)).unwrap();
        let rendered = format!("{parsed:?}");
        assert!(
            !rendered.contains(PASTED),
            "the pasted secret leaked into Debug output: {rendered}"
        );
        assert!(
            rendered.contains("<redacted>"),
            "the field must stay visible as redacted, not silently dropped: {rendered}"
        );
        assert!(
            rendered.contains("100.64.0.2"),
            "the rest must stay debuggable: {rendered}"
        );
    }

    #[test]
    fn one_accepted_secret_authenticates_both_directions() {
        // The claim the whole feature rests on: `principal_for` matches on
        // the SECRET alone, so ids, labels and grants stay local and one
        // paste on the second machine completes the pair.
        use crate::companion::auth::{principal_for, Principal};
        // Mac A mints for Mac B and shows the link.
        let minted = pair("mac-b");
        // Mac B accepts it, for Mac A.
        let accepted = accept_pasted_pairing(
            &link_to("100.64.0.2", &minted.secret),
            "mac-a",
            &[candidate("mac-a")],
            &[],
        )
        .unwrap();
        assert_ne!(accepted.id, minted.id, "ids are local and need not agree");
        assert_eq!(
            principal_for("phone-token", &accepted.secret, &[minted.clone()]),
            Some(Principal::Peer(minted.id.clone())),
            "B presenting the shared secret must be recognised by A"
        );
        assert_eq!(
            principal_for("phone-token", &minted.secret, &[accepted.clone()]),
            Some(Principal::Peer(accepted.id.clone())),
            "A presenting the same secret must be recognised by B"
        );
    }

    #[test]
    fn an_accepted_record_survives_the_loader_that_reads_it_back() {
        // Accepting writes through the same settings file the loader
        // reads; a record `load_peers` would quarantine is a pairing that
        // silently stops working at the next launch.
        let record = accept_pasted_pairing(PASTED, "mac-a", &[], &[]).unwrap();
        let raw = serde_json::to_value(vec![record]).unwrap();
        let (kept, problems) = load_peers(&raw);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].secret, PASTED);
        assert_eq!(kept[0].grants, Grants::on_pair());
    }

    // -------------------------------------------------------------
    // THE blocking criterion: does a peer mutation need to reach the
    // running companion immediately, rather than at the next toggle?
    // -------------------------------------------------------------

    #[test]
    fn deleting_a_peer_requires_a_restart() {
        let peer = pair("work-mbp");
        let before = vec![peer];
        let after: Vec<PeerRecord> = Vec::new();
        assert!(peer_mutation_requires_restart(&before, &after));
    }

    #[test]
    fn narrowing_a_grant_requires_a_restart() {
        let mut peer = pair("work-mbp");
        peer.grants.view = true;
        let before = vec![peer.clone()];
        peer.grants.view = false;
        let after = vec![peer];
        assert!(
            peer_mutation_requires_restart(&before, &after),
            "narrowing a grant is a partial revocation -- it must not wait for a toggle"
        );
    }

    #[test]
    fn widening_a_grant_also_requires_a_restart() {
        // Deliberately unconditional on direction -- see the doc comment.
        let mut peer = pair("work-mbp");
        let before = vec![peer.clone()];
        peer.grants.spawn = true;
        let after = vec![peer];
        assert!(peer_mutation_requires_restart(&before, &after));
    }

    #[test]
    fn pairing_a_new_peer_requires_a_restart() {
        // Otherwise the fresh secret is unrecognized by the running server
        // until the next manual toggle -- "pairing" that silently doesn't
        // work yet.
        let before: Vec<PeerRecord> = Vec::new();
        let after = vec![pair("work-mbp")];
        assert!(peer_mutation_requires_restart(&before, &after));
    }

    #[test]
    fn an_unchanged_snapshot_needs_no_restart() {
        let peers = vec![pair("work-mbp")];
        assert!(!peer_mutation_requires_restart(&peers, &peers));
    }

    #[test]
    fn a_peer_secret_never_appears_in_debug_output() {
        // A paired secret is the credential that lets another machine drive
        // terminals here. Deriving Debug put it one `{:?}` away from a log
        // line, a panic message, or a failing test's output.
        let record = pair("work-mbp");
        let rendered = format!("{record:?}");
        assert!(
            !rendered.contains(&record.secret),
            "the secret leaked into Debug output: {rendered}"
        );
        assert!(
            rendered.contains("<redacted>"),
            "the field must still be visible as redacted, not silently dropped: {rendered}"
        );
        assert!(
            rendered.contains("work-mbp"),
            "the rest of the record must stay debuggable: {rendered}"
        );
    }

    #[test]
    fn pairing_grants_view_and_type_but_never_spawn() {
        let p = pair("work-mbp");
        assert!(
            p.grants.view,
            "a paired peer that cannot see reads as broken"
        );
        assert!(
            p.grants.type_,
            "sharing a terminal you cannot type into is half a feature"
        );
        assert!(
            !p.grants.spawn,
            "spawn starts processes on this machine without anything being shared - it must stay opt-in"
        );
    }

    #[test]
    fn the_serde_default_stays_deny_all_even_though_pairing_does_not() {
        // These two must not be allowed to drift into each other. `default`
        // is what a record with missing or malformed grants deserializes
        // to, so if it ever inherited `on_pair`'s values, a corrupted line
        // in the peers file would silently GRANT access instead of
        // withholding it.
        let d = Grants::default();
        assert!(
            !d.view && !d.type_ && !d.spawn,
            "serde default must deny everything"
        );
        assert_ne!(
            d,
            Grants::on_pair(),
            "if these ever become equal, the deny-by-default guarantee is gone"
        );
    }

    #[test]
    fn the_binary_resolver_prefers_an_absolute_path_over_the_bare_name() {
        // The bug this fixes: a GUI-launched app gets
        // PATH=/usr/bin:/bin:/usr/sbin:/sbin, which contains none of the
        // places Tailscale installs to, so the bare name resolved to
        // nothing and discovery silently returned an empty list.
        let found = resolve_binary(TAILSCALE_BINARIES, |p| {
            p == "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
        });
        assert_eq!(
            found,
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
        );
    }

    #[test]
    fn the_binary_resolver_takes_the_first_candidate_that_exists() {
        // Order is the contract, not an accident: the shim at
        // /usr/local/bin execs the bundle binary, so preferring it keeps
        // whatever indirection the user's install chose.
        let all = |_: &str| true;
        assert_eq!(
            resolve_binary(TAILSCALE_BINARIES, all),
            TAILSCALE_BINARIES[0]
        );
        let not_first = |p: &str| p != TAILSCALE_BINARIES[0];
        assert_eq!(
            resolve_binary(TAILSCALE_BINARIES, not_first),
            TAILSCALE_BINARIES[1]
        );
    }

    #[test]
    fn the_binary_resolver_falls_back_to_the_bare_name_when_nothing_exists() {
        // Not a failure case: a shell-launched app, or Linux, has a PATH
        // that carries `tailscale` even though none of the macOS absolute
        // paths exist. Returning the bare name keeps those working.
        assert_eq!(resolve_binary(TAILSCALE_BINARIES, |_| false), "tailscale");
    }

    #[test]
    fn every_candidate_path_is_absolute() {
        // A relative entry would silently reintroduce the PATH dependence
        // this list exists to remove, and would resolve differently
        // depending on the app's working directory.
        for c in TAILSCALE_BINARIES {
            assert!(
                std::path::Path::new(c).is_absolute(),
                "{c} must be absolute"
            );
        }
    }
}
