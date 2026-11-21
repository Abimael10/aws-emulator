// SPDX-License-Identifier: CC0-1.0

//! The USER AUTHORIZATION SESSION of this plane's OAuth2 resource-token flow: what a
//! `sessionUri` stands for, and who may confirm the session it names.
//!
//! That flow runs in two halves. The call that INITIATES it hands the caller an address for a
//! user to authorize at, together with the `sessionUri` naming the session that authorization
//! belongs to; `CompleteResourceTokenAuth` then CONFIRMS that session once the user has
//! authorized, so the tokens of the resource can be obtained under it. A session is therefore
//! a record of this plane's own making ([`ResourceTokenAuthSessions`]), keyed by the URI it
//! was handed out under ([`mint_resource_token_session_uri`]) and holding the workload
//! identity, the resource credential provider and the USER the authorization was started for.
//!
//! What decides a confirmation is that record, never the bytes the caller sent. A URI this
//! plane never handed out, one whose window the clock has already closed, and one handed out
//! for a different user are each refused as ONE class, with the single fixed sentence AWS
//! words that refusal with ([`INVALID_OR_EXPIRED_SESSION_MESSAGE`]): a caller holding a
//! foreign or stale session learns nothing about the sessions this account has open, and an
//! unrecognized session is never read as a completed authorization. Nothing of the presented
//! URI is echoed back — it is the handle a pending authorization is completed with.
//!
//! The registry is bounded by the window of each session and by
//! [`MAX_RESOURCE_TOKEN_AUTH_SESSIONS`], both materialised on the WRITE path, so the sessions
//! a caller can open cannot grow the process heap without end.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as control_plane;

/// The fixed text AWS refuses a resource-token session it does not accept with. It carries no
/// request- or state-derived part: the session URI asked about is deliberately NOT quoted
/// back, and the sentence names no user, provider or account.
///
/// One sentence for every way a session fails to resolve, so a caller cannot tell a URI this
/// account never had issued from one issued for somebody else.
pub const INVALID_OR_EXPIRED_SESSION_MESSAGE: &str = "Invalid or expired session";

/// The prefix every `sessionUri` of this flow carries, fixed by `RequestUri`'s pattern
/// `^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$` (source: smithy_query query="shape"
/// shape="RequestUri") — the `request_uri` URN a pushed authorization request is named by.
const SESSION_URI_PREFIX: &str = "urn:ietf:params:oauth:request_uri:";

/// How long a user authorization session this plane hands out stays confirmable.
///
/// AWS publishes no lifetime for one, so this window is the EMULATOR's own: long enough for a
/// user to authorize at the address the initiation handed out, short enough that a caller can
/// observe a session run out. The window is what [`ResourceTokenAuthSessions`] resolves a
/// session until, and the clock alone ends it.
pub const RESOURCE_TOKEN_AUTH_SESSION_TTL: Duration = Duration::from_secs(600);

/// The most authorization sessions this emulator holds at once.
///
/// An EMULATOR ceiling, not an AWS quota: every initiation of the flow appends one record, so
/// without a ceiling a caller looping on it would grow the process heap without end. The
/// least recently recorded session gives way first, so the sessions a live flow has just
/// opened are the ones that stay confirmable.
pub const MAX_RESOURCE_TOKEN_AUTH_SESSIONS: usize = 10_000;

/// One user authorization session this plane handed out: what the authorization was started
/// for, the user it was started for, the window it is confirmable in, and whether it has been
/// confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceTokenAuthSession {
    /// The `workloadName` of the workload identity the flow was initiated from.
    pub workload_name: String,
    /// The resource credential provider the flow obtains tokens of.
    pub resource_credential_provider_name: String,
    /// The user the authorization was started for: the `userId` the initiating call named, or
    /// the subject of the user token it presented. A `userIdentifier` resolving to any other
    /// user does not complete this session.
    pub user_id: String,
    /// When the session was handed out.
    pub issued_at: SystemTime,
    /// When the session stops being confirmable.
    pub expires_at: SystemTime,
    /// When the user authorization was confirmed, and `None` while the session is still
    /// pending.
    pub confirmed_at: Option<SystemTime>,
    /// The engine-wide sequence the record was written with, which orders the records the
    /// ceiling evicts.
    pub seq: u64,
}

/// Every user authorization session this plane handed out and has not pruned, keyed by the
/// `sessionUri` it was handed out under.
#[derive(Default)]
pub struct ResourceTokenAuthSessions {
    sessions: HashMap<String, ResourceTokenAuthSession>,
}

/// Reports how many sessions are held and nothing else. The KEY of this registry is the URI a
/// pending authorization is completed with — a handle that stands for one — so a derived
/// rendering would put every open session's handle into whatever formatted it, a log line
/// included. The sessions stay readable through [`ResourceTokenAuthSessions::issued`] by a
/// caller that already holds the URI.
impl ::std::fmt::Debug for ResourceTokenAuthSessions {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ResourceTokenAuthSessions")
            .field("sessions", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

impl ResourceTokenAuthSessions {
    /// Record that this plane handed `session_uri` out for `session`.
    ///
    /// Prunes first: a session the clock has already closed is dropped, which is owed at this
    /// instant whether or not anybody asked for one, and at the ceiling the least recently
    /// recorded session gives way, with the key breaking a tie so eviction is deterministic.
    pub fn record(
        &mut self,
        session_uri: String,
        session: ResourceTokenAuthSession,
        now: SystemTime,
    ) {
        self.prune(now);
        while self.sessions.len() >= MAX_RESOURCE_TOKEN_AUTH_SESSIONS {
            match super::least_recently_written(&self.sessions, |held| held.seq) {
                Some(key) => {
                    self.sessions.remove(&key);
                }
                None => break,
            }
        }
        self.sessions.insert(session_uri, session);
    }

    /// The session `session_uri` stands for, when this plane handed it out and its window has
    /// not closed at `now`. `None` for a URI this plane never handed out, and for a session
    /// the clock has run out on.
    ///
    /// A READ: nothing is written here, so two callers asking about one session are answered
    /// alike.
    #[must_use]
    pub fn issued(
        &self,
        session_uri: &str,
        now: SystemTime,
    ) -> Option<&ResourceTokenAuthSession> {
        self.sessions
            .get(session_uri)
            .filter(|held| now < held.expires_at)
    }

    /// Confirm the user authorization of the session `session_uri` names, on behalf of
    /// `user_id`.
    ///
    /// `Some` is the session as it stands after the confirmation — confirming the session is
    /// what the operation DOES, so the instant is written, and a session confirmed a second
    /// time keeps the instant of the first. `None` is the gate refusing the call: a URI this
    /// plane never handed out, a session whose window the clock has closed, or one handed out
    /// for another user, which a handler answers with
    /// [`INVALID_OR_EXPIRED_SESSION_MESSAGE`].
    ///
    /// Prunes first, for the reason [`Self::record`] does.
    pub fn confirm(
        &mut self,
        session_uri: &str,
        user_id: &str,
        now: SystemTime,
    ) -> Option<ResourceTokenAuthSession> {
        self.prune(now);
        let held = self.sessions.get_mut(session_uri)?;
        if held.user_id != user_id {
            return None;
        }
        held.confirmed_at = Some(held.confirmed_at.unwrap_or(now));
        Some(held.clone())
    }

    /// How many sessions the registry holds, closed ones included until the next write prunes
    /// them.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the registry holds no session at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Drop what the clock has already made unconfirmable.
    fn prune(&mut self, now: SystemTime) {
        self.sessions.retain(|_, held| now < held.expires_at);
    }
}

/// Mint the `sessionUri` a user authorization session is handed out under: the prefix
/// `RequestUri`'s pattern fixes, then the instant the session was handed out in epoch
/// milliseconds and the engine-wide sequence, both in lowercase hex — characters the pattern's
/// `[a-zA-Z0-9-._~]` admits — so two sessions handed out inside one millisecond are still two
/// sessions.
#[must_use]
pub fn mint_resource_token_session_uri(issued_at: SystemTime, seq: u64) -> String {
    let millis = issued_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_millis());
    format!("{SESSION_URI_PREFIX}{millis:016x}{seq:016x}")
}

/// The resource a call about this account's resource-token sessions addresses: the account's
/// default TOKEN VAULT, which is the resource of this account the OAuth2 credential providers
/// and the authorization sessions over them belong to.
///
/// Built from the REQUEST alone — the region it was signed for — because the request carries
/// no name of its own for one, so the authorization decision taken on it discloses nothing
/// about what this account holds.
#[must_use]
pub fn token_vault_resource(region: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!(
            "{TOKEN_VAULT_ARN_TYPE}/{}",
            control_plane::state::DEFAULT_TOKEN_VAULT_ID
        ),
    )
}

/// The ARN resource type of a token vault, as the plane that owns one spells it
/// (`arn:…:bedrock-agentcore:…:token-vault/<vaultId>/…`).
const TOKEN_VAULT_ARN_TYPE: &str = "token-vault";

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant every case is decided at.
    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// A pending session for `user`, valid from `issued_at` for [`RESOURCE_TOKEN_AUTH_SESSION_TTL`].
    fn pending(user: &str, issued_at: SystemTime, seq: u64) -> ResourceTokenAuthSession {
        ResourceTokenAuthSession {
            workload_name: "demo.agent".to_string(),
            resource_credential_provider_name: "partner-api".to_string(),
            user_id: user.to_string(),
            issued_at,
            expires_at: issued_at + RESOURCE_TOKEN_AUTH_SESSION_TTL,
            confirmed_at: None,
            seq,
        }
    }

    /// A registry holding one pending session for `user`, handed out at `issued_at`.
    fn with_session(
        user: &str,
        issued_at: SystemTime,
        seq: u64,
    ) -> (ResourceTokenAuthSessions, String) {
        let mut registry = ResourceTokenAuthSessions::default();
        let uri = mint_resource_token_session_uri(issued_at, seq);
        registry.record(uri.clone(), pending(user, issued_at, seq), issued_at);
        (registry, uri)
    }

    /// RULE: only a session this plane handed out, still inside its own window, and handed out
    /// for the very user the confirmation acts for is confirmable; every other session URI
    /// resolves to nothing at all.
    #[test]
    fn only_a_live_session_of_this_plane_for_the_same_user_confirms() {
        let now = now();
        let issued_at = now - Duration::from_secs(60);
        let (mut registry, live) = with_session("user-1", issued_at, 1);
        let closed_at = now - RESOURCE_TOKEN_AUTH_SESSION_TTL;
        let closed = mint_resource_token_session_uri(closed_at, 2);
        registry.record(closed.clone(), pending("user-1", closed_at, 2), closed_at);

        let confirmed = registry.confirm(&live, "user-1", now);
        assert!(
            confirmed.is_some_and(|session| session.confirmed_at == Some(now)
                && session.user_id == "user-1"),
            "case \"a live session of its own user\": expected the session confirmed at this \
             instant"
        );

        let cases: Vec<(&str, String, &str)> = vec![
            ("its window closed at this very instant", closed, "user-1"),
            (
                "a URI this plane never handed out",
                mint_resource_token_session_uri(issued_at, 99),
                "user-1",
            ),
            ("handed out for another user", live.clone(), "user-2"),
            (
                "a user id that differs only in case",
                live.clone(),
                "USER-1",
            ),
            (
                "a value that is no session URI at all",
                "not-a-session".to_string(),
                "user-1",
            ),
            ("an empty value", String::new(), "user-1"),
        ];

        for (case, session_uri, user) in cases {
            let read = registry.confirm(&session_uri, user, now);
            assert!(
                read.is_none(),
                "case {case:?}: expected no session to confirm, got {read:?}"
            );
        }

        // The clock alone ends a session: the live one stops confirming once `now` passes the
        // instant it was handed out until, with no call made in between.
        let past = issued_at + RESOURCE_TOKEN_AUTH_SESSION_TTL;
        let read = registry.confirm(&live, "user-1", past);
        assert!(
            read.is_none(),
            "case \"past its own window\": expected no session to confirm, got {read:?}"
        );
    }

    /// RULE: confirming a session is what records the confirmation, and confirming one twice
    /// states the same confirmation rather than a second one; a READ of a session changes
    /// nothing a later caller sees.
    #[test]
    fn a_confirmation_is_recorded_once_and_a_read_changes_nothing() {
        let now = now();
        let issued_at = now - Duration::from_secs(5);
        let (mut registry, uri) = with_session("user-1", issued_at, 1);

        let pending_before = registry.issued(&uri, now).cloned();
        assert!(
            pending_before.is_some_and(|session| session.confirmed_at.is_none()),
            "case \"before the confirmation\": expected a pending session"
        );
        let read_again = registry.issued(&uri, now).cloned();
        assert_eq!(
            read_again.and_then(|session| session.confirmed_at),
            None,
            "case \"read twice\": expected reading a session to confirm nothing"
        );

        let first = registry.confirm(&uri, "user-1", now);
        let later = now + Duration::from_secs(30);
        let second = registry.confirm(&uri, "user-1", later);
        assert_eq!(
            first.map(|session| session.confirmed_at),
            Some(Some(now)),
            "case \"the first confirmation\": expected the instant it was confirmed at"
        );
        assert_eq!(
            second.map(|session| session.confirmed_at),
            Some(Some(now)),
            "case \"confirmed again\": expected the instant of the first confirmation to stand"
        );
        let held = registry.issued(&uri, later).cloned();
        assert_eq!(
            held.and_then(|session| session.confirmed_at),
            Some(now),
            "case \"read after the confirmation\": expected the recorded confirmation"
        );
    }

    /// RULE: a minted session URI is inside the shape the model fixes for one, and two mints
    /// are two sessions.
    #[test]
    fn a_minted_session_uri_is_inside_the_modelled_shape() {
        // The pattern `RequestUri` publishes (source: smithy_query query="shape"
        // shape="RequestUri"), run against the minted value rather than restated by hand.
        let modelled = ::regex::Regex::new("^urn:ietf:params:oauth:request_uri:[a-zA-Z0-9-._~]+$")
            .expect("the modelled pattern compiles");
        let issued_at = now();
        let mut minted = Vec::new();
        for (case, at, seq) in [
            ("the first of a millisecond", issued_at, 1_u64),
            ("the second of a millisecond", issued_at, 2),
            (
                "a later millisecond",
                issued_at + Duration::from_millis(1),
                3,
            ),
            ("the epoch itself", SystemTime::UNIX_EPOCH, 0),
            ("a far sequence", issued_at, u64::MAX),
        ] {
            let uri = mint_resource_token_session_uri(at, seq);
            assert!(
                modelled.is_match(&uri) && uri.len() <= 1_024,
                "case {case:?}: expected a URI inside the modelled shape, got {uri:?}"
            );
            minted.push(uri);
        }
        let distinct: ::std::collections::BTreeSet<&String> = minted.iter().collect();
        assert_eq!(
            distinct.len(),
            minted.len(),
            "expected one URI per mint, got {} distinct of {}",
            distinct.len(),
            minted.len()
        );
    }

    /// RULE: both bounds of the registry are materialised on the WRITE path — a later write
    /// drops what the clock has closed, and at the ceiling the least recently recorded session
    /// gives way while the rest stay confirmable.
    #[test]
    fn a_write_prunes_what_closed_and_holds_the_ceiling() {
        let now = now();
        let closed_at = now - RESOURCE_TOKEN_AUTH_SESSION_TTL - Duration::from_secs(1);
        let mut registry = ResourceTokenAuthSessions::default();
        let closed = mint_resource_token_session_uri(closed_at, 1);
        registry.record(closed.clone(), pending("user-1", closed_at, 1), closed_at);
        assert_eq!(
            registry.len(),
            1,
            "case \"nothing due yet\": expected the session written inside its own window to be \
             held"
        );

        let live = mint_resource_token_session_uri(now, 2);
        registry.record(live.clone(), pending("user-1", now, 2), now);
        assert_eq!(
            registry.len(),
            1,
            "case \"a later write\": expected the closed session to be pruned by it"
        );
        assert!(
            registry.issued(&live, now).is_some() && registry.issued(&closed, now).is_none(),
            "case \"a later write\": expected only the live session to resolve"
        );

        for index in 0..MAX_RESOURCE_TOKEN_AUTH_SESSIONS {
            let seq = u64::try_from(index).unwrap_or(0) + 3;
            registry.record(
                mint_resource_token_session_uri(now, seq),
                pending("user-1", now, seq),
                now,
            );
        }
        assert_eq!(
            registry.len(),
            MAX_RESOURCE_TOKEN_AUTH_SESSIONS,
            "case \"at the ceiling\": expected the registry never to grow past it"
        );
        assert!(
            registry.issued(&live, now).is_none(),
            "case \"at the ceiling\": expected the least recently recorded session to give way"
        );
    }

    /// RULE: the resource a call about this account's sessions is authorized on is the
    /// account's default token vault in the region the request was signed for.
    #[test]
    fn the_addressed_resource_is_the_accounts_token_vault() {
        for (case, region) in [("a us region", "us-east-1"), ("another region", "eu-west-1")] {
            let addressed = token_vault_resource(region);
            assert_eq!(
                addressed,
                ::engine::emulator_arn(
                    super::super::ARN_SERVICE,
                    region,
                    &format!(
                        "token-vault/{}",
                        control_plane::state::DEFAULT_TOKEN_VAULT_ID
                    )
                ),
                "case {case:?}: expected the token vault's own ARN, got {addressed:?}"
            );
            assert!(
                addressed.contains(&format!(":{region}:")),
                "case {case:?}: expected the region the request was signed for, got \
                 {addressed:?}"
            );
        }
    }
}
