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

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
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
}
