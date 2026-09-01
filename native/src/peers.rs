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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
pub fn scan_candidates() -> Vec<Candidate> {
    match shell_bounded(
        "tailscale",
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
pub fn pair(host: &str) -> PeerRecord {
    PeerRecord {
        id: PeerId(new_peer_id()),
        host: host.to_string(),
        label: host.to_string(),
        secret: new_peer_secret(),
        grants: Grants::default(),
    }
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

    #[test]
    fn pairing_starts_with_every_grant_off() {
        let record = pair("work-mbp");
        assert_eq!(record.label, "work-mbp");
        assert!(!record.grants.view);
        assert!(!record.grants.type_);
        assert!(!record.grants.spawn);
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
}
