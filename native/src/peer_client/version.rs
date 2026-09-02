//! Deciding whether a peer's `/version` response is a wire shape this
//! build can speak, BEFORE `attach::run` ever opens `/stream/<id>` — see
//! that module's `check_peer_version`. The whole point of checking up
//! front: without it, an incompatible peer is discovered by failing to
//! parse EVERY frame (`stream::StreamConn::next_frame`'s
//! `BadResponse("frame was not a valid snapshot")`), repeated per frame,
//! mid-stream, with no reason a caller can act on. `check_version` here
//! gives a single, distinguishable verdict instead.

use crate::companion::wire::{CAP_SNAPSHOT_BACKGROUND, PROTOCOL_VERSION};

/// The verdict [`check_version`] reaches from an already-fetched
/// `/version` response body. Two verdicts, because the caller acts on
/// exactly two: attach, or refuse. What matters is that the refusal is
/// reached HERE, before a stream opens, so it stays distinguishable from
/// the generic per-frame parse failure at `stream.rs`'s
/// `BadResponse("frame was not a valid snapshot")` (see
/// `attach::Status::Incompatible`'s doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCheck {
    /// `protocol` matches [`PROTOCOL_VERSION`] AND `capabilities` lists
    /// [`CAP_SNAPSHOT_BACKGROUND`] — BOTH checked, not `protocol` alone, so
    /// the capability list stays the actual source of truth for what a
    /// peer's wire shape contains rather than a number a peer could get
    /// out of sync with its own capabilities.
    Compatible,
    /// Anything else: a peer speaking a different protocol, one missing
    /// the capability this build needs, one whose `/version` is not valid
    /// JSON, and one that answers with JSON lacking `protocol` entirely.
    ///
    /// These were once three variants. They are one because the only
    /// caller (`attach::check_peer_version`) mapped every non-`Compatible`
    /// verdict to the same terminal `Status::Incompatible`, so the extra
    /// variants distinguished nothing any caller could observe — and the
    /// test that pinned them asserted this enum's shape rather than any
    /// behaviour. Split them again when something actually says a
    /// different thing to the user for each; not before.
    Incompatible,
}

/// Pure: takes an already-fetched `/version` response body and returns a
/// verdict. No I/O here — `attach::check_peer_version` is what performs
/// the actual GET (`peer_client::get`) and hands this function the bytes.
pub fn check_version(body: &[u8]) -> VersionCheck {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return VersionCheck::Incompatible;
    };
    let Some(protocol) = value.get("protocol").and_then(|v| v.as_u64()) else {
        return VersionCheck::Incompatible;
    };
    let has_capability = value
        .get("capabilities")
        .and_then(|v| v.as_array())
        .is_some_and(|caps| {
            caps.iter()
                .any(|c| c.as_str() == Some(CAP_SNAPSHOT_BACKGROUND))
        });
    if protocol == PROTOCOL_VERSION as u64 && has_capability {
        VersionCheck::Compatible
    } else {
        VersionCheck::Incompatible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_protocol_with_the_capability_is_compatible() {
        let body = serde_json::json!({
            "version": "1.0",
            "build": "test",
            "protocol": PROTOCOL_VERSION,
            "capabilities": ["principals", "origin", "peer-input", CAP_SNAPSHOT_BACKGROUND],
        })
        .to_string();
        assert_eq!(check_version(body.as_bytes()), VersionCheck::Compatible);
    }

    #[test]
    fn the_exact_old_shape_protocol_1_without_the_capability_is_incompatible() {
        // The literal payload `server.rs` advertised before this task —
        // named here because the brief calls out these exact values.
        let body = r#"{"version":"0.1","build":"old","protocol":1,"capabilities":["principals","origin","peer-input"]}"#;
        assert_eq!(check_version(body.as_bytes()), VersionCheck::Incompatible);
    }

    #[test]
    fn a_mismatched_protocol_number_is_incompatible_even_with_the_capability_present() {
        // Checking the capability alone would wrongly accept this — the
        // number matters independently, per `VersionCheck::Compatible`'s
        // doc on why both are checked.
        let body = serde_json::json!({
            "protocol": PROTOCOL_VERSION + 1,
            "capabilities": ["principals", "origin", "peer-input", CAP_SNAPSHOT_BACKGROUND],
        })
        .to_string();
        assert_eq!(check_version(body.as_bytes()), VersionCheck::Incompatible);
    }

    #[test]
    fn the_matching_protocol_number_without_the_capability_is_incompatible() {
        // Checking the number alone would wrongly accept this — the
        // capability matters independently, same reasoning as above.
        let body = serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "capabilities": ["principals", "origin", "peer-input"],
        })
        .to_string();
        assert_eq!(check_version(body.as_bytes()), VersionCheck::Incompatible);
    }

    #[test]
    fn a_response_that_is_not_version_json_at_all_is_refused() {
        // The BEHAVIOUR, not this enum's shape: whatever the reason, a
        // peer whose `/version` cannot be read as a compatible answer must
        // be refused rather than optimistically attached. Pointing the
        // client at something that is not a companion server is the real
        // case here.
        assert_eq!(
            check_version(b"not json at all"),
            VersionCheck::Incompatible
        );
    }

    #[test]
    fn valid_json_missing_the_protocol_field_is_refused() {
        // JSON, but not a companion `/version`: absent `protocol` must
        // never read as "compatible by default".
        let body = serde_json::json!({ "capabilities": [CAP_SNAPSHOT_BACKGROUND] }).to_string();
        assert_eq!(check_version(body.as_bytes()), VersionCheck::Incompatible);
    }
}
