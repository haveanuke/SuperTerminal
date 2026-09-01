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
    /// Constructed from Phase B onward, once pairing exists and
    /// `principal_for` can actually resolve a peer's token. Until then the
    /// admission table below is deliberately ahead of what can be reached.
    #[cfg_attr(not(test), allow(dead_code))]
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

/// Resolve a presented token to a principal. Constant-time against the
/// phone token; Phase A has no peer secrets, so `Peer` is unreachable here.
pub fn principal_for(phone_token: &str, presented: &str) -> Option<Principal> {
    if token_matches(phone_token, presented) {
        Some(Principal::Phone)
    } else {
        None
    }
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
        assert_eq!(principal_for("abc123", "abc123"), Some(Principal::Phone));
        assert_eq!(principal_for("abc123", "wrong"), None);
        assert_eq!(principal_for("abc123", ""), None);
    }
}
