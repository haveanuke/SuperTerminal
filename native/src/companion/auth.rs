//! Capability token: the page-level authorization on top of Tailscale.
//! 128 bits from /dev/urandom, carried in the bookmark's URL fragment
//! (fragments never appear in HTTP requests or referrers). Comparison is
//! constant-time so timing can't leak prefix matches.

use crate::companion::http::Method;
use serde::{Deserialize, Serialize};

/// Identity of a paired peer instance. Phase A never constructs one; it
/// exists so the admission table is written once rather than retrofitted
/// when Phase B adds pairing.
///
/// Serialize/Deserialize (mirroring `hosts::ProfileId`) so Phase B's
/// `peers::PeerRecord` can round-trip through the raw JSON stored in
/// `Settings::peers`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(pub String);

/// Who is making a request. Every protected route states which principals
/// it admits, because a single shared token would otherwise let the phone
/// reach peer-only surfaces and let a peer reach phone-only management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Phone,
    /// A paired peer instance, resolved by `principal_for` once a presented
    /// secret matches a configured `peers::PeerRecord`.
    Peer(PeerId),
}

/// The admission table. Deny by default: an unknown path admits nobody, so
/// a new route is unreachable until it is listed here deliberately.
pub fn admits(path: &str, method: Method, principal: &Principal) -> bool {
    let phone_only = matches!(
        (path, method),
        ("/close", Method::Post)
            | ("/rename", Method::Post)
            | ("/previews", Method::Get)
            | ("/preview", Method::Get)
    );
    let shared = matches!(
        (path, method),
        ("/sessions", Method::Get)
            | ("/stream", Method::Get)
            | ("/input", Method::Post)
            | ("/spawn", Method::Post)
            | ("/version", Method::Get)
    );
    // The raw-byte sink: a peer runs the local key encoder itself and ships
    // the resulting bytes, which the phone's symbolic keypad has no use
    // for and must never reach — a raw sink is a much bigger attack surface
    // than ten named keys.
    let peer_only = matches!((path, method), ("/peer-input", Method::Post));
    match principal {
        Principal::Phone => phone_only || shared,
        Principal::Peer(_) => shared || peer_only,
    }
}

/// Admission's third layer: not just "may this principal use this route"
/// (Phase A's table above) but "does this peer hold the grant the route
/// needs". Phase A is consulted FIRST — if it denies, this denies; grants
/// can only narrow what Phase A already admitted, never widen it, so a peer
/// with every grant set still cannot reach `/close` or `/previews`.
///
/// `Principal::Phone` ignores grants entirely — grants describe peers, not
/// the phone. `grants: None` means the presented principal resolved to no
/// peer record at all (revoked, or never existed) and is refused
/// everything, even routes Phase A would otherwise admit a peer to.
pub fn admits_with_grants(
    path: &str,
    method: Method,
    principal: &Principal,
    grants: Option<crate::peers::Grants>,
) -> bool {
    if !admits(path, method, principal) {
        return false;
    }
    match principal {
        Principal::Phone => true,
        Principal::Peer(_) => {
            let Some(grants) = grants else {
                return false;
            };
            match (path, method) {
                ("/sessions", Method::Get) | ("/stream", Method::Get) => grants.view,
                // Typing into a session you cannot see is meaningless;
                // requiring both stops a half-configured peer typing blind.
                ("/peer-input", Method::Post) => grants.view && grants.type_,
                // Independent of `view`: creating a terminal is a separate
                // authority from watching one.
                ("/spawn", Method::Post) => grants.spawn,
                _ => true,
            }
        }
    }
}

/// Resolve a presented token to a principal. The phone token is checked
/// FIRST, exactly as before pairing existed, and wins outright: a peer
/// secret must never be able to shadow or impersonate the phone principal.
/// Only once that check fails is `presented` compared against every
/// configured peer's secret (see `peer_secret_matches`).
pub fn principal_for(
    phone_token: &str,
    presented: &str,
    peers: &[crate::peers::PeerRecord],
) -> Option<Principal> {
    if token_matches(phone_token, presented) {
        return Some(Principal::Phone);
    }
    peer_secret_matches(presented, peers)
}

/// Compare `presented` against EVERY peer's secret rather than stopping at
/// the first hit. `token_matches` is already constant-time per comparison;
/// a loop that returns as soon as one matches would still leak, through how
/// many comparisons ran before the response, how many peers are configured
/// and roughly where in the list a match sits. Running the full loop
/// unconditionally keeps that shape identical for a match, a miss, and an
/// empty list.
fn peer_secret_matches(presented: &str, peers: &[crate::peers::PeerRecord]) -> Option<Principal> {
    let mut hit: Option<PeerId> = None;
    for peer in peers {
        if token_matches(&peer.secret, presented) && hit.is_none() {
            hit = Some(peer.id.clone());
        }
    }
    hit.map(Principal::Peer)
}

pub fn generate_token() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("/dev/urandom is always readable on macOS");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality: accumulate XOR over the longer length (zero
/// padded) and fold in the length difference, so neither content nor length
/// short-circuits.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let expected = expected.as_bytes();
    let presented = presented.as_bytes();
    let len = expected.len().max(presented.len());
    let mut diff = (expected.len() ^ presented.len()) as u8;
    for i in 0..len {
        let a = expected.get(i).copied().unwrap_or(0);
        let b = presented.get(i).copied().unwrap_or(0);
        diff |= a ^ b;
    }
    diff == 0 && !expected.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_32_lowercase_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 32);
        assert!(token
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn two_generations_differ() {
        assert_ne!(generate_token(), generate_token());
    }

    #[test]
    fn matches_only_exact() {
        assert!(token_matches("abcd1234", "abcd1234"));
        assert!(!token_matches("abcd1234", "abcd1235"));
        assert!(!token_matches("abcd1234", "abcd123"));
        assert!(!token_matches("abcd1234", "abcd12345"));
        assert!(!token_matches("abcd1234", ""));
        assert!(!token_matches("", "abcd1234"));
    }

    use crate::companion::http::Method;

    fn peer() -> Principal {
        Principal::Peer(PeerId("p1".to_string()))
    }

    #[test]
    fn the_phone_keeps_every_route_it_has_today() {
        // Phase A must not remove a single phone capability.
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/stream", Method::Get),
            ("/input", Method::Post),
            ("/spawn", Method::Post),
            ("/close", Method::Post),
            ("/rename", Method::Post),
            ("/previews", Method::Get),
            ("/preview", Method::Get),
            ("/version", Method::Get),
        ] {
            assert!(admits(path, method, &Principal::Phone), "phone lost {path}");
        }
    }

    #[test]
    fn a_peer_may_view_type_and_spawn_but_not_manage() {
        assert!(admits("/sessions", Method::Get, &peer()));
        assert!(admits("/stream", Method::Get, &peer()));
        assert!(admits("/spawn", Method::Post, &peer()));
        // Management and the preview gallery are phone-only for now.
        assert!(!admits("/close", Method::Post, &peer()), "peer got /close");
        assert!(
            !admits("/rename", Method::Post, &peer()),
            "peer got /rename"
        );
        assert!(
            !admits("/previews", Method::Get, &peer()),
            "peer got /previews"
        );
        assert!(
            !admits("/preview", Method::Get, &peer()),
            "peer got /preview"
        );
    }

    #[test]
    fn only_a_peer_may_use_the_raw_byte_sink() {
        assert!(admits("/peer-input", Method::Post, &peer()));
        assert!(
            !admits("/peer-input", Method::Post, &Principal::Phone),
            "the phone reached the peer's raw-byte sink"
        );
    }

    #[test]
    fn an_unknown_route_admits_nobody() {
        assert!(!admits("/nonexistent", Method::Get, &Principal::Phone));
        assert!(!admits("/nonexistent", Method::Get, &peer()));
    }

    #[test]
    fn only_the_phone_token_resolves_to_a_principal_in_this_phase() {
        assert_eq!(
            principal_for("abc123", "abc123", &[]),
            Some(Principal::Phone)
        );
        assert_eq!(principal_for("abc123", "wrong", &[]), None);
        assert_eq!(principal_for("abc123", "", &[]), None);
    }

    /// Test-only shape adapter: the brief's helper takes `(id, secret)`
    /// pairs rather than full `PeerRecord`s, since auth has no business
    /// caring about labels or grants when resolving a principal. Builds the
    /// records `principal_for` actually takes and delegates to it.
    fn principal_for_with_peers(
        phone_token: &str,
        presented: &str,
        peers: &[(&str, &str)],
    ) -> Option<Principal> {
        let records: Vec<crate::peers::PeerRecord> = peers
            .iter()
            .map(|(id, secret)| crate::peers::PeerRecord {
                id: PeerId((*id).to_string()),
                label: String::new(),
                secret: (*secret).to_string(),
                grants: crate::peers::Grants::default(),
            })
            .collect();
        principal_for(phone_token, presented, &records)
    }

    #[test]
    fn a_paired_secret_resolves_to_that_peer() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(
            principal_for_with_peers("phone-token", "aabbccddeeff00112233445566778899", &peers),
            Some(Principal::Peer(PeerId("p1".into())))
        );
    }

    #[test]
    fn the_phone_token_still_wins_and_is_unchanged() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(
            principal_for_with_peers("phone-token", "phone-token", &peers),
            Some(Principal::Phone)
        );
    }

    #[test]
    fn an_unknown_secret_resolves_to_nobody() {
        let peers = vec![("p1", "aabbccddeeff00112233445566778899")];
        assert_eq!(
            principal_for_with_peers("phone-token", "nope", &peers),
            None
        );
        assert_eq!(principal_for_with_peers("phone-token", "", &peers), None);
    }

    #[test]
    fn a_peer_secret_cannot_shadow_the_phone_even_on_an_exact_collision() {
        // If a peer's secret were ever equal to the phone token, the phone
        // must still be who gets resolved — the phone check runs first and
        // returns immediately, so the peer loop never even runs.
        let peers = vec![("p1", "phone-token")];
        assert_eq!(
            principal_for_with_peers("phone-token", "phone-token", &peers),
            Some(Principal::Phone)
        );
    }

    fn all_off() -> crate::peers::Grants {
        crate::peers::Grants::default()
    }

    #[test]
    fn a_peer_with_no_grants_can_do_nothing() {
        let p = peer();
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/stream", Method::Get),
            ("/peer-input", Method::Post),
            ("/spawn", Method::Post),
        ] {
            assert!(
                !admits_with_grants(path, method, &p, Some(all_off())),
                "no-grant peer reached {path}"
            );
        }
    }

    #[test]
    fn view_alone_permits_looking_but_not_typing_or_spawning() {
        let g = crate::peers::Grants {
            view: true,
            ..Default::default()
        };
        let p = peer();
        assert!(admits_with_grants("/sessions", Method::Get, &p, Some(g)));
        assert!(admits_with_grants("/stream", Method::Get, &p, Some(g)));
        assert!(!admits_with_grants(
            "/peer-input",
            Method::Post,
            &p,
            Some(g)
        ));
        assert!(!admits_with_grants("/spawn", Method::Post, &p, Some(g)));
    }

    #[test]
    fn typing_requires_view_as_well() {
        // Input into a session you cannot see is meaningless; requiring both
        // stops a half-configured peer from typing blind.
        let g = crate::peers::Grants {
            type_: true,
            ..Default::default()
        };
        assert!(!admits_with_grants(
            "/peer-input",
            Method::Post,
            &peer(),
            Some(g)
        ));
    }

    #[test]
    fn spawn_is_independent_of_view() {
        let g = crate::peers::Grants {
            spawn: true,
            ..Default::default()
        };
        assert!(admits_with_grants("/spawn", Method::Post, &peer(), Some(g)));
    }

    #[test]
    fn the_phone_is_unaffected_by_grants() {
        // Grants describe peers. The phone's table is unchanged.
        for (path, method) in [
            ("/sessions", Method::Get),
            ("/close", Method::Post),
            ("/previews", Method::Get),
        ] {
            assert!(admits_with_grants(path, method, &Principal::Phone, None));
        }
    }

    #[test]
    fn a_peer_with_no_record_is_refused_everything() {
        // `None` grants means the peer resolved to no record at all.
        assert!(!admits_with_grants("/sessions", Method::Get, &peer(), None));
    }

    #[test]
    fn grants_can_only_narrow_never_widen() {
        // The route table is consulted FIRST. A peer holding every grant must
        // still not reach a phone-only route — otherwise grants become an
        // escalation path rather than a restriction.
        let all = crate::peers::Grants {
            view: true,
            type_: true,
            spawn: true,
        };
        for (path, method) in [
            ("/close", Method::Post),
            ("/rename", Method::Post),
            ("/previews", Method::Get),
            ("/preview", Method::Get),
        ] {
            assert!(
                !admits_with_grants(path, method, &peer(), Some(all)),
                "a fully-granted peer reached {path}"
            );
        }
    }
}
