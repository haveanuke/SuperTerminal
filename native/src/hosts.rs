//! Remote host profiles. Identity is opaque and app-generated so a label
//! or destination can be edited without silently repointing a saved pane.
//!
//! Nothing here spawns anything: this slice can describe a remote host but
//! cannot open one. See `docs/superpowers/specs/2026-08-28-remote-hosts-design.md`.

use serde::{Deserialize, Serialize};
use superterminal_core::activity::Activity;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    MacOs,
    Linux,
    Windows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
    PowerShell,
    Cmd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProfile {
    pub id: ProfileId,
    pub label: String,
    /// Already normalised: bare IPv6, no brackets, validated charset.
    pub destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub os: HostOs,
    pub shell: ShellKind,
}

#[derive(Debug, PartialEq)]
pub struct ProfileProblem {
    pub label: String,
    pub reason: String,
}

/// Where a pane's shell runs. `Local` is the default and is omitted from
/// serialized layouts so existing session files are unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
// Adjacent (not internal) tagging: `ProfileId` serializes as a bare JSON
// string, and serde cannot internally-tag a newtype variant whose payload
// isn't a map. `content` puts the payload in its own field instead.
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum Target {
    #[default]
    Local,
    Remote(ProfileId),
}

impl Target {
    pub fn is_local(&self) -> bool {
        matches!(self, Target::Local)
    }

    pub fn profile_id(&self) -> Option<&ProfileId> {
        match self {
            Target::Local => None,
            Target::Remote(id) => Some(id),
        }
    }
}

/// What `TerminalPane::foreground_activity`/`status_activity` report,
/// given the pane's target and what its live session (if any) itself
/// reports. `session_activity` is `None` exactly when `TerminalPane`'s
/// `session` field is — no shell spawned, the shell exited, or (today,
/// since this slice ships no remote capability) a restored `Target::Remote`
/// pane built via `TerminalPane::dead`, which never has a session.
///
/// A local pane with no session is definitively idle: a shell that never
/// started, or has exited, is not working — this is the pre-slice
/// behaviour for every local pane, unchanged. A remote pane with no
/// session carries no trustworthy signal about a machine this app is not
/// talking to: `Unknown`, never `Idle`. That is the tri-state invariant
/// this slice exists to hold — `Activity::Unknown` must never be treated
/// as `Activity::Idle` — extended to cover the "no session at all" case
/// that a bare `Option<cwd>`-shaped check would otherwise silently fold
/// into local's answer.
pub fn pane_activity(target: &Target, session_activity: Option<Activity>) -> Activity {
    session_activity.unwrap_or(if target.is_local() {
        Activity::Idle
    } else {
        Activity::Unknown
    })
}

/// The outcome of resolving a saved `Target` against the currently loaded
/// profiles.
#[derive(Debug, PartialEq)]
pub enum ResolvedTarget<'a> {
    Local,
    Remote(&'a RemoteProfile),
    /// The pane restores dead, labelled with the missing id, reconnect
    /// disabled. It must never silently fall back to a local shell.
    MissingProfile(ProfileId),
}

pub fn resolve_target<'a>(target: &Target, profiles: &'a [RemoteProfile]) -> ResolvedTarget<'a> {
    match target {
        Target::Local => ResolvedTarget::Local,
        Target::Remote(id) => match profiles.iter().find(|p| &p.id == id) {
            Some(profile) => ResolvedTarget::Remote(profile),
            None => ResolvedTarget::MissingProfile(id.clone()),
        },
    }
}

/// What a side panel (git, files) is pointed at. `Detached` is "nothing
/// focused, or the focused pane has no cwd yet"; `Remote` is "the focused
/// pane is on another host", which this slice renders as disabled rather
/// than remoting. Unlike the old `Option<cwd>` panel setters — which
/// treated `None` as "do nothing" and left the previous target's data live
/// and actionable — every panel target change goes through here and is
/// always applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelTarget {
    Local(std::path::PathBuf),
    Remote(String),
    Detached,
}

impl PanelTarget {
    /// The ONLY way a panel target is derived. A remote pane can never
    /// produce a local path, regardless of what cwd it reports.
    pub fn from_pane(target: &Target, cwd: Option<String>, label: &str) -> PanelTarget {
        match target {
            Target::Remote(_) => PanelTarget::Remote(label.to_string()),
            Target::Local => match cwd {
                Some(cwd) => PanelTarget::Local(cwd.into()),
                None => PanelTarget::Detached,
            },
        }
    }

    pub fn local_path(&self) -> Option<&std::path::Path> {
        match self {
            PanelTarget::Local(path) => Some(path.as_path()),
            _ => None,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, PanelTarget::Local(_))
    }
}

/// Whether a panel retarget is same-repo `Local` cwd churn (`cd src`)
/// rather than a change of KIND (Local <-> Remote, Local <-> Detached, or
/// a Remote host change). Only cwd churn may skip the eager retarget
/// wipe — a real repo-identity check, made once the new cwd resolves,
/// decides whether that case needs a wipe too. Every other transition is
/// a kind change and must always wipe immediately; this predicate must
/// never be used to weaken that.
pub fn is_local_cwd_move(prev: &PanelTarget, next: &PanelTarget) -> bool {
    matches!((prev, next), (PanelTarget::Local(_), PanelTarget::Local(_)))
}

/// Whether an async completion still owns the panel. Checked BEFORE any
/// field is written, `busy` included — a stale completion from a target
/// that has since been switched away from must not touch panel state.
pub fn accepts_completion(current: u64, token: u64) -> bool {
    current == token
}

#[derive(Debug, PartialEq)]
pub enum DestinationError {
    Empty,
    TooLong,
    LeadingDash,
    BadCharacter,
}

const MAX_DESTINATION: usize = 253;
const MAX_LABEL: usize = 63;

/// 128 bits from /dev/urandom, matching the existing dependency-free
/// pattern in `companion/auth.rs`.
pub fn new_profile_id() -> ProfileId {
    ProfileId(crate::companion::auth::generate_token())
}

/// Validate and normalise an ssh destination. Accepts a dot-separated
/// hostname/FQDN, an IPv4 literal, or a BARE IPv6 literal; brackets around
/// an IPv6 literal are accepted from settings and stripped, because
/// `ssh -- '[::1]'` resolves the literal string "[::1]" and fails.
pub fn validate_destination(raw: &str) -> Result<String, DestinationError> {
    let trimmed = raw.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    if unbracketed.is_empty() {
        return Err(DestinationError::Empty);
    }
    if unbracketed.len() > MAX_DESTINATION {
        return Err(DestinationError::TooLong);
    }
    if unbracketed.starts_with('-') {
        return Err(DestinationError::LeadingDash);
    }
    // IPv6 literal: hex groups and colons only.
    if unbracketed.contains(':') {
        let ok = unbracketed
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':');
        return if ok {
            Ok(unbracketed.to_string())
        } else {
            Err(DestinationError::BadCharacter)
        };
    }
    for label in unbracketed.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL {
            return Err(DestinationError::TooLong);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(DestinationError::BadCharacter);
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(DestinationError::BadCharacter);
        }
    }
    Ok(unbracketed.to_string())
}

/// Permissive profile loading. NEVER returns an error: `settings.rs` falls
/// back to `Settings::default()` on any serde failure, so one hand-edited
/// profile must not be able to reset every unrelated setting. A malformed
/// container degrades to "no profiles", and duplicate ids quarantine EVERY
/// colliding profile rather than picking one.
pub fn load_profiles(raw: &serde_json::Value) -> (Vec<RemoteProfile>, Vec<ProfileProblem>) {
    let mut problems = Vec::new();
    let Some(items) = raw.as_array() else {
        if !raw.is_null() {
            problems.push(ProfileProblem {
                label: String::new(),
                reason: "remoteProfiles is not a list".to_string(),
            });
        }
        return (Vec::new(), problems);
    };
    let mut candidates: Vec<RemoteProfile> = Vec::new();
    for item in items {
        let label = item
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let parsed: RemoteProfile = match serde_json::from_value(item.clone()) {
            Ok(profile) => profile,
            Err(error) => {
                problems.push(ProfileProblem {
                    label,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        if parsed.id.0.is_empty() {
            problems.push(ProfileProblem {
                label,
                reason: "empty id".to_string(),
            });
            continue;
        }
        match validate_destination(&parsed.destination) {
            Ok(destination) => candidates.push(RemoteProfile {
                destination,
                ..parsed
            }),
            Err(error) => problems.push(ProfileProblem {
                label,
                reason: format!("bad destination: {error:?}"),
            }),
        }
    }
    // Quarantine every member of an id collision.
    let mut kept = Vec::new();
    for profile in &candidates {
        let count = candidates
            .iter()
            .filter(|other| other.id == profile.id)
            .count();
        if count > 1 {
            problems.push(ProfileProblem {
                label: profile.label.clone(),
                reason: format!("duplicate id {}", profile.id.0),
            });
        } else {
            kept.push(profile.clone());
        }
    }
    (kept, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_hostname_is_accepted() {
        assert_eq!(validate_destination("pc1").unwrap(), "pc1");
    }

    #[test]
    fn a_magicdns_fqdn_is_accepted() {
        let d = "pc1.tail1a2b3c.ts.net";
        assert_eq!(validate_destination(d).unwrap(), d);
    }

    #[test]
    fn shell_metacharacters_are_rejected() {
        for bad in ["a;rm -rf /", "a$(id)", "a`id`", "a b", "a\nb", "a|b", "a&b"] {
            assert!(validate_destination(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn a_leading_dash_is_rejected() {
        // ssh would read it as an option.
        assert!(validate_destination("-oProxyCommand=id").is_err());
        assert!(validate_destination("-pc1").is_err());
    }

    #[test]
    fn empty_and_overlong_are_rejected() {
        assert!(validate_destination("").is_err());
        assert!(validate_destination(&"a".repeat(64)).is_err());
        let long_fqdn = std::iter::repeat("abcdefghij")
            .take(30)
            .collect::<Vec<_>>()
            .join(".");
        assert!(validate_destination(&long_fqdn).is_err());
    }

    #[test]
    fn ipv6_is_stored_bare_and_brackets_are_stripped() {
        // `ssh -- '[::1]'` tries to resolve the literal "[::1]" and fails.
        assert_eq!(validate_destination("::1").unwrap(), "::1");
        assert_eq!(validate_destination("[::1]").unwrap(), "::1");
        assert_eq!(
            validate_destination("[fe80::1ff:fe23:4567:890a]").unwrap(),
            "fe80::1ff:fe23:4567:890a"
        );
    }

    #[test]
    fn generated_ids_are_unique_and_opaque() {
        let a = new_profile_id();
        let b = new_profile_id();
        assert_ne!(a, b);
        assert_eq!(a.0.len(), 32);
        assert!(a
            .0
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    fn raw(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn one_invalid_profile_does_not_discard_the_valid_ones() {
        let (ok, problems) = load_profiles(&raw(
            r#"[{"id":"aa","label":"good","destination":"pc1","os":"linux","shell":"bash"},
                {"id":"bb","label":"bad","destination":"a;id","os":"linux","shell":"bash"}]"#,
        ));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].label, "good");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn a_malformed_container_yields_no_profiles_rather_than_an_error() {
        // settings.rs falls back to Settings::default() on any serde error,
        // so a bad container must never reach serde as a hard failure.
        let (ok, problems) = load_profiles(&raw("5"));
        assert!(ok.is_empty());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn duplicate_ids_quarantine_every_colliding_profile() {
        // Never first-wins or last-wins: an ambiguous id must not be able
        // to resolve to the wrong host.
        let (ok, problems) = load_profiles(&raw(
            r#"[{"id":"dup","label":"one","destination":"pc1","os":"linux","shell":"bash"},
                {"id":"dup","label":"two","destination":"pc2","os":"linux","shell":"bash"},
                {"id":"solo","label":"three","destination":"pc3","os":"linux","shell":"bash"}]"#,
        ));
        assert_eq!(ok.len(), 1, "only the non-colliding profile survives");
        assert_eq!(ok[0].label, "three");
        assert_eq!(problems.len(), 2, "both colliding profiles are reported");
    }

    #[test]
    fn a_target_whose_profile_is_missing_does_not_become_local() {
        let profiles: Vec<RemoteProfile> = Vec::new();
        let target = Target::Remote(ProfileId("gone".into()));
        assert_eq!(
            resolve_target(&target, &profiles),
            ResolvedTarget::MissingProfile(ProfileId("gone".into()))
        );
        // The critical assertion: it is not Local.
        assert_ne!(resolve_target(&target, &profiles), ResolvedTarget::Local);
    }

    #[test]
    fn a_local_pane_with_a_cwd_targets_that_path() {
        let t = PanelTarget::from_pane(&Target::Local, Some("/tmp/repo".into()), "");
        assert_eq!(t.local_path(), Some(std::path::Path::new("/tmp/repo")));
    }

    #[test]
    fn a_local_pane_without_a_cwd_detaches() {
        let t = PanelTarget::from_pane(&Target::Local, None, "");
        assert_eq!(t, PanelTarget::Detached);
        assert_eq!(t.local_path(), None);
    }

    #[test]
    fn a_remote_pane_never_yields_a_local_path() {
        // Even if a cwd string is somehow present, a remote pane must not
        // hand the panels a local path to act on.
        let remote = Target::Remote(ProfileId("p1".into()));
        let t = PanelTarget::from_pane(&remote, Some("/tmp/repo".into()), "pc1");
        assert_eq!(t, PanelTarget::Remote("pc1".to_string()));
        assert_eq!(t.local_path(), None, "remote pane leaked a local path");
    }

    #[test]
    fn local_to_local_is_cwd_move_everything_else_is_a_kind_change() {
        let local_a = PanelTarget::Local("/tmp/repo-a".into());
        let local_b = PanelTarget::Local("/tmp/repo-b".into());
        assert!(is_local_cwd_move(&local_a, &local_b));

        let remote_a = PanelTarget::Remote("host-a".to_string());
        let remote_b = PanelTarget::Remote("host-b".to_string());
        assert!(!is_local_cwd_move(&local_a, &remote_a), "Local -> Remote");
        assert!(!is_local_cwd_move(&remote_a, &local_a), "Remote -> Local");
        assert!(
            !is_local_cwd_move(&local_a, &PanelTarget::Detached),
            "Local -> Detached"
        );
        assert!(
            !is_local_cwd_move(&PanelTarget::Detached, &local_a),
            "Detached -> Local"
        );
        assert!(
            !is_local_cwd_move(&remote_a, &remote_b),
            "Remote -> Remote, different host"
        );
    }

    #[test]
    fn pane_activity_exhaustive_over_target_and_session() {
        let local = Target::Local;
        let remote = Target::Remote(ProfileId("host".into()));

        // No session: the invariant under test. Local -> Idle (unchanged
        // pre-slice behaviour: a shell that never started is not working).
        // Remote -> Unknown, never Idle (no trustworthy signal about a
        // machine this app isn't talking to yet) — this is what a restored
        // dead `Target::Remote` pane (no session, no telemetry) reports.
        assert_eq!(pane_activity(&local, None), Activity::Idle);
        assert_eq!(pane_activity(&remote, None), Activity::Unknown);

        // A live session's own answer always passes through untouched,
        // regardless of target — the fallback only applies when there is
        // no session to ask.
        for session_activity in [Activity::Idle, Activity::Busy, Activity::Unknown] {
            assert_eq!(
                pane_activity(&local, Some(session_activity)),
                session_activity
            );
            assert_eq!(
                pane_activity(&remote, Some(session_activity)),
                session_activity
            );
        }
    }

    #[test]
    fn a_dead_remote_pane_and_an_idle_local_pane_aggregate_to_unknown() {
        // Models the exact workspace `Activity::aggregate` call at
        // workspace/mod.rs (auto-caffeinate) and the sidebar dots: one
        // restored, unconnected `Target::Remote` pane (no session) beside
        // one ordinary idle local pane. The dead remote pane's Unknown must
        // survive the aggregate, not get folded into the local pane's Idle.
        let dead_remote = pane_activity(&Target::Remote(ProfileId("host".into())), None);
        let idle_local = pane_activity(&Target::Local, Some(Activity::Idle));
        assert_eq!(dead_remote, Activity::Unknown);
        assert_eq!(idle_local, Activity::Idle);
        assert_eq!(
            Activity::aggregate([dead_remote, idle_local].into_iter()),
            Activity::Unknown
        );
    }

    #[test]
    fn a_stale_token_is_refused_and_the_current_one_accepted() {
        // git_panel.rs:354 clears `busy` BEFORE checking generation, so a
        // completion from a previous target could unbusy the new one.
        assert!(!accepts_completion(7, 6), "stale token accepted");
        assert!(!accepts_completion(7, 8), "future token accepted");
        assert!(accepts_completion(7, 7));
    }

    #[test]
    fn a_target_with_a_live_profile_resolves_to_it() {
        let profiles = vec![RemoteProfile {
            id: ProfileId("p1".into()),
            label: "pc1".into(),
            destination: "pc1".into(),
            user: None,
            port: None,
            os: HostOs::Linux,
            shell: ShellKind::Bash,
        }];
        match resolve_target(&Target::Remote(ProfileId("p1".into())), &profiles) {
            ResolvedTarget::Remote(profile) => assert_eq!(profile.label, "pc1"),
            other => panic!("expected a resolved profile, got {other:?}"),
        }
    }
}
