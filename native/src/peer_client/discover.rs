//! Finding the companion server on a paired peer.
//!
//! A `PeerRecord` carries the tailnet HOST it was paired from and the shared
//! secret — it does not carry a port, and it cannot: `companion_ui` binds
//! the first free port in [`COMPANION_PORTS`], so which one a peer ended up
//! on depends on what else was running on that machine when it started. So
//! the port is discovered, once, by asking each candidate `/version` — which
//! is the compatibility gate this phase already requires before a stream
//! opens, done here where its verdict can still be shown to a user instead
//! of surfacing as a dead pane.
//!
//! Every function that touches a socket here BLOCKS. None may run on the
//! gpui thread; the workspace probes on the background executor.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use super::version::{check_version, VersionCheck};
use super::{Endpoint, PeerError};

/// The ports `workspace::companion_ui::start_companion` tries, in order.
/// Shared with that module rather than written twice: a peer is found by
/// looking exactly where this app would have put its own server.
pub const COMPANION_PORTS: std::ops::RangeInclusive<u16> = 43110..=43120;

/// TOTAL round trip for ONE port's `/version`. Deliberately shorter than
/// `attach`'s five seconds: this is multiplied by however many ports are
/// silent before the live one, and a peer that is simply off should cost a
/// few seconds of background thread, not a minute of it.
const PROBE_DEADLINE: Duration = Duration::from_secs(2);

/// What one port said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// `/version` answered, and this build can speak its wire shape.
    Compatible,
    /// `/version` answered with a protocol or capability set this build
    /// cannot speak.
    Incompatible,
    /// Something is listening and turned us away. The companion answers 404
    /// both for an unknown route and for a request it will not authorize —
    /// deliberately indistinguishable — so this is "a server, but not one
    /// that knows us", most often a pairing the other Mac never completed.
    Refused,
    /// Nothing answered on this port at all.
    Silent,
}

/// Where a peer's companion is, or why it could not be found. Every
/// non-`Ready` variant exists so the UI can say something a user can ACT on
/// rather than showing an empty list.
#[derive(Debug, Clone)]
pub enum Reach {
    Ready(Endpoint),
    /// Something answered `/version` on every tried port, and none spoke a
    /// wire shape this build understands.
    Incompatible,
    /// Something is listening but refused us everywhere. Almost always the
    /// pairing: this Mac holds a record the other one does not.
    Refused,
    /// Nothing answered anywhere — that Mac is off, asleep, or its
    /// companion is not running.
    Unreachable,
}

impl Reach {
    pub fn endpoint(&self) -> Option<&Endpoint> {
        match self {
            Reach::Ready(endpoint) => Some(endpoint),
            _ => None,
        }
    }

    /// What to show instead of a session list. Empty for `Ready`, which has
    /// a list to show instead.
    pub fn note(&self) -> &'static str {
        match self {
            Reach::Ready(_) => "",
            Reach::Incompatible => "that Mac runs a SuperTerminal this one cannot talk to",
            Reach::Refused => "that Mac does not recognise this pairing - pair it there too",
            Reach::Unreachable => "no SuperTerminal answered - is it open on that Mac?",
        }
    }
}

/// Every address a peer's companion could be listening on, in the order
/// this app itself would have bound them.
///
/// `addr` comes from `tailscale status --json` and is always an IP literal,
/// so it is PARSED, never resolved: a name lookup here would put a DNS
/// round trip on whatever thread called, and MagicDNS is not something a
/// peer's reachability should depend on. An address that does not parse
/// yields no candidates rather than a guess.
pub fn candidate_endpoints(addr: &str, secret: &str) -> Vec<Endpoint> {
    let Ok(ip) = addr.parse::<IpAddr>() else {
        return Vec::new();
    };
    COMPANION_PORTS
        .map(|port| Endpoint {
            addr: SocketAddr::new(ip, port),
            secret: secret.to_string(),
        })
        .collect()
}

/// Pick the endpoint to talk to, given a way to probe one.
///
/// Scanning does NOT stop at the first port that answers, only at the first
/// COMPATIBLE one: anything else listening on 43110 (another app, a stale
/// process, a peer mid-restart) would otherwise mask the real companion two
/// ports up. What the scan saw is then reported by precedence —
/// compatible beats incompatible beats refused beats silence — so the
/// reason shown is the most specific one any port gave.
pub fn choose_endpoint(
    candidates: &[Endpoint],
    mut probe: impl FnMut(&Endpoint) -> Probe,
) -> Reach {
    let mut saw_incompatible = false;
    let mut saw_refused = false;
    for candidate in candidates {
        match probe(candidate) {
            Probe::Compatible => return Reach::Ready(candidate.clone()),
            Probe::Incompatible => saw_incompatible = true,
            Probe::Refused => saw_refused = true,
            Probe::Silent => {}
        }
    }
    if saw_incompatible {
        Reach::Incompatible
    } else if saw_refused {
        Reach::Refused
    } else {
        Reach::Unreachable
    }
}

/// Ask one endpoint `/version`. BLOCKING.
pub fn probe_endpoint(endpoint: &Endpoint) -> Probe {
    match super::get(endpoint, "/version", PROBE_DEADLINE) {
        Ok(body) => match check_version(&body) {
            VersionCheck::Compatible => Probe::Compatible,
            VersionCheck::Incompatible => Probe::Incompatible,
        },
        // A status came back, so something is serving HTTP there; it just
        // will not serve US. See `Probe::Refused`.
        Err(PeerError::Status(_)) => Probe::Refused,
        // A malformed response is still a response, but from something that
        // is not this protocol — reported as refused rather than silence so
        // the UI does not claim the machine is off.
        Err(PeerError::BadResponse(_)) | Err(PeerError::TooLarge) => Probe::Refused,
        Err(_) => Probe::Silent,
    }
}

/// Find a peer's companion. BLOCKING: one round trip per candidate port,
/// each bounded by [`PROBE_DEADLINE`].
pub fn find(addr: &str, secret: &str) -> Reach {
    choose_endpoint(&candidate_endpoints(addr, secret), probe_endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::hub::Hub;
    use crate::companion::server::{start, ServerConfig};
    use crate::peers::{Grants, PeerRecord};
    use std::sync::Arc;

    const SECRET: &str = "abcdef0123456789abcdef0123456789";

    fn thumbs() -> Arc<crate::companion::thumbs::Thumbnailer> {
        crate::companion::thumbs::Thumbnailer::new(
            std::env::temp_dir().join(format!("st-thumbcache-discover-{}", std::process::id())),
        )
    }

    fn previews() -> Arc<crate::companion::previews::PreviewStore> {
        Arc::new(crate::companion::previews::PreviewStore::new(None))
    }

    fn endpoint(port: u16) -> Endpoint {
        Endpoint {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            secret: SECRET.to_string(),
        }
    }

    // --- which addresses are even tried ------------------------------------

    #[test]
    fn a_peer_is_looked_for_on_exactly_the_ports_this_app_binds() {
        let candidates = candidate_endpoints("100.64.0.7", SECRET);
        let ports: Vec<u16> = candidates.iter().map(|e| e.addr.port()).collect();
        assert_eq!(ports, COMPANION_PORTS.collect::<Vec<u16>>());
        assert_eq!(
            ports.first().copied(),
            Some(43110),
            "the scan must start where this app's own first bind attempt does"
        );
        assert!(candidates.iter().all(|e| e.secret == SECRET));
        assert!(candidates
            .iter()
            .all(|e| e.addr.ip().to_string() == "100.64.0.7"));
    }

    #[test]
    fn an_ipv6_tailnet_address_is_probed_too() {
        let candidates = candidate_endpoints("fd7a:115c:a1e0::1", SECRET);
        assert_eq!(candidates.len(), COMPANION_PORTS.count());
        assert!(candidates[0].addr.is_ipv6());
    }

    #[test]
    fn an_address_that_is_not_an_ip_literal_is_never_guessed_at() {
        // `tailscale status --json` always reports IP literals. A hostname
        // here would mean a DNS round trip on the caller's thread, and a
        // MagicDNS outage would look like an offline peer. Refuse instead.
        for addr in ["mac-studio", "mac-studio.tail1234.ts.net", "", "1.2.3"] {
            assert!(
                candidate_endpoints(addr, SECRET).is_empty(),
                "guessed an endpoint for {addr:?}"
            );
        }
        // ...and an empty candidate list is unreachable, never "refused".
        assert!(matches!(
            choose_endpoint(&[], |_| Probe::Compatible),
            Reach::Unreachable
        ));
    }

    // --- the scan's verdict ------------------------------------------------

    #[test]
    fn the_first_compatible_port_wins_and_the_scan_stops_there() {
        let candidates = vec![endpoint(1), endpoint(2), endpoint(3)];
        let mut probed = Vec::new();
        let reach = choose_endpoint(&candidates, |e| {
            probed.push(e.addr.port());
            if e.addr.port() == 2 {
                Probe::Compatible
            } else {
                Probe::Silent
            }
        });
        assert_eq!(reach.endpoint().map(|e| e.addr.port()), Some(2));
        assert_eq!(probed, vec![1, 2], "the scan kept going past a live peer");
    }

    #[test]
    fn something_else_squatting_the_first_port_does_not_hide_the_real_peer() {
        // The reason the scan does not stop at the first port that ANSWERS.
        // 43110 is the first port this app tries, so it is exactly the one
        // another process is most likely to be holding.
        let candidates = vec![endpoint(1), endpoint(2)];
        let reach = choose_endpoint(&candidates, |e| match e.addr.port() {
            1 => Probe::Refused,
            _ => Probe::Compatible,
        });
        assert_eq!(reach.endpoint().map(|e| e.addr.port()), Some(2));
    }

    #[test]
    fn the_reason_reported_is_the_most_specific_one_any_port_gave() {
        let candidates = vec![endpoint(1), endpoint(2)];
        assert!(
            matches!(
                choose_endpoint(&candidates, |e| if e.addr.port() == 1 {
                    Probe::Refused
                } else {
                    Probe::Incompatible
                }),
                Reach::Incompatible
            ),
            "a build mismatch says more than a 404"
        );
        assert!(
            matches!(
                choose_endpoint(&candidates, |e| if e.addr.port() == 1 {
                    Probe::Silent
                } else {
                    Probe::Refused
                }),
                Reach::Refused
            ),
            "a 404 says more than silence"
        );
        assert!(matches!(
            choose_endpoint(&candidates, |_| Probe::Silent),
            Reach::Unreachable
        ));
    }

    #[test]
    fn every_failure_explains_itself_and_no_two_read_the_same() {
        let notes = [
            Reach::Incompatible.note(),
            Reach::Refused.note(),
            Reach::Unreachable.note(),
        ];
        for note in notes {
            assert!(!note.is_empty());
        }
        assert_ne!(notes[0], notes[1]);
        assert_ne!(notes[1], notes[2]);
        assert_ne!(notes[0], notes[2]);
        assert!(
            Reach::Ready(endpoint(1)).note().is_empty(),
            "a reachable peer shows its sessions, not a note"
        );
    }

    // --- against a real companion server -----------------------------------

    #[test]
    fn a_real_companion_answers_compatible_and_a_wrong_secret_is_refused() {
        let hub = Arc::new(Hub::new());
        let handle = start(
            Arc::clone(&hub),
            crate::themes::default_theme(),
            ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
                token: "phonephonephonephonephonephoneph".into(),
                page: "<title>discover-test</title>",
                previews: previews(),
                thumbs: thumbs(),
                peers: vec![PeerRecord {
                    id: crate::companion::auth::PeerId("p1".into()),
                    host: "peer.local".into(),
                    label: "peer".into(),
                    secret: SECRET.into(),
                    // Deliberately deny-all: `/version` needs no grant, so a
                    // peer that has been paired but not yet granted `view`
                    // must still be FOUND — it is the case where the note
                    // shown next matters most.
                    grants: Grants::default(),
                }],
            },
        )
        .expect("server starts");

        let live = Endpoint {
            addr: handle.addr(),
            secret: SECRET.into(),
        };
        assert_eq!(probe_endpoint(&live), Probe::Compatible);

        let wrong = Endpoint {
            addr: handle.addr(),
            secret: "0000000000000000000000000000ffff".into(),
        };
        assert_eq!(
            probe_endpoint(&wrong),
            Probe::Refused,
            "an unrecognised secret must read as a pairing problem, not as an absent Mac"
        );

        handle.stop();
    }

    #[test]
    fn a_port_with_nothing_on_it_is_silent_rather_than_refused() {
        // Bind and immediately drop, so the port is almost certainly free
        // and definitely not ours.
        let free = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        assert_eq!(probe_endpoint(&endpoint(free)), Probe::Silent);
    }
}
