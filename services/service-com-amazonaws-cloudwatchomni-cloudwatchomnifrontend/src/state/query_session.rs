// SPDX-License-Identifier: CC0-1.0

//! The telemetry query SESSION record: what a started session holds, how many of them
//! the store keeps, and how a listing walks the ones one caller owns.
//!
//! `StartTelemetryQuerySession`'s model doc reads "A session provides a logical grouping
//! for one or more telemetry queries. The returned session ID is required when starting
//! queries via StartTelemetryQuery", and `ListTelemetryQuerySessions`'s reads "Returns a
//! list of telemetry query sessions owned by the caller". So a session is a record of
//! THIS store, minted by a start, owned by the principal that started it, and addressed
//! afterwards by the id the backend minted for it — which is why the record below is
//! keyed by that id and carries the owner beside it.
//!
//! It lives beside the store rather than in one operation's file because the start that
//! mints a session and the listing that reports it read the same record, and because the
//! quota, the id form and the page cursor are properties of the STORE rather than of a
//! single request.

/// How many sessions the store holds before `StartTelemetryQuerySession` refuses with its
/// modelled `ServiceQuotaExceededException`.
///
/// CloudWatchOmni publishes an adjustable quota on concurrent query sessions PER ACCOUNT,
/// and this emulator models neither Service Quotas nor an account's adjustments, so this
/// is the emulator's OWN bound rather than a claim about any account's limit — the same
/// reason [`super::SPACE_QUOTA`] is the emulator's own. One emulator serves one account,
/// so the bound is on the store as a whole rather than on one caller's share of it: it is
/// what stops
/// [`super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::query_sessions`]
/// growing without end however many principals call, and with it the per-request work
/// [`query_sessions_after`] and [`query_session_named`] do under the store lock, which is
/// linear in the number of stored sessions.
///
/// [`query_sessions_after`]: super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::query_sessions_after
/// [`query_session_named`]: super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::query_session_named
pub const QUERY_SESSION_QUOTA: usize = 10_000;

/// A telemetry query session as persisted.
///
/// The model declares no session structure beyond `SessionSummary` — the id, the name and
/// the two activity instants — so the record holds that member set plus the owner a
/// listing is scoped by and the start order a page cursor resumes from.
#[derive(Debug, Clone)]
pub struct StoredQuerySession {
    /// Start order, so a listing and its pages are stable as the store grows.
    pub seq: u64,
    /// The id the backend minted for this session, which is what a caller addresses it
    /// by ([`mint_query_session_id`]).
    pub session_id: String,
    /// The human-readable name the start carried, when it carried one. A start may name
    /// no session, and such a session has no name to report.
    pub session_name: Option<String>,
    /// The principal that started the session ([`super::caller_principal`]), which is the
    /// caller a listing reports it to.
    pub owner: String,
    /// When the session was started.
    pub created_at: ::aws_smithy_types::DateTime,
    /// The most recent activity in the session; the start instant until something moves
    /// it, because starting the session is the first activity in it.
    pub last_activity_at: ::aws_smithy_types::DateTime,
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// The session `owner` holds under `session_name`, or `None` when that caller holds
    /// none under it.
    ///
    /// A session's name identifies it among the sessions ONE caller holds open, so the
    /// lookup is scoped by owner: two callers naming a session the same thing are naming
    /// two different sessions.
    ///
    /// Linear in the number of stored sessions, which [`QUERY_SESSION_QUOTA`] bounds.
    pub fn query_session_named(
        &self,
        owner: &str,
        session_name: &str,
    ) -> Option<&StoredQuerySession> {
        self.query_sessions.values().find(|stored| {
            stored.owner == owner
                && stored
                    .session_name
                    .as_deref()
                    .is_some_and(|name| name == session_name)
        })
    }

    /// Every session `owner` holds, in start order, skipping everything at or before
    /// `after_seq`.
    ///
    /// Linear in the number of stored sessions, which [`QUERY_SESSION_QUOTA`] bounds.
    pub fn query_sessions_after(&self, owner: &str, after_seq: u64) -> Vec<&StoredQuerySession> {
        let mut selected: Vec<&StoredQuerySession> = self
            .query_sessions
            .values()
            .filter(|stored| stored.owner == owner)
            .filter(|stored| stored.seq > after_seq)
            .collect();
        selected.sort_by_key(|stored| stored.seq);
        selected
    }
}

/// The name prefix a session name is reserved under.
///
/// `StartTelemetryQuerySessionRequest$sessionName`'s model doc: "A human-readable name
/// for the session. Names under `/aws/` are reserved for service integrations" — the
/// member's `@pattern` admits such a name, so the reservation is a rule of the service
/// rather than of the shape, and this is where it is stated.
pub const RESERVED_SESSION_NAME_PREFIX: &str = "/aws/";

/// The whole text a start under a name the caller may not use is refused with.
///
/// Recorded behavior: wholly FIXED text, a sentence with a trailing period, naming the
/// member that was wrong and withholding the name the request carried — so the refusal
/// states which parameter to fix and nothing about what the namespace holds.
pub const SESSION_NAME_NOT_VALID_MESSAGE: &str = "The specified session name is not valid.";

/// Why `session_name` may not name a caller's session, or `None` when it may.
///
/// The modelled length and pattern are refused before a handler runs, so what is left is
/// the reservation the member's doc states: a name in the reserved namespace belongs to a
/// service integration and a caller may not take one.
pub fn session_name_rejection(session_name: &str) -> Option<String> {
    session_name
        .starts_with(RESERVED_SESSION_NAME_PREFIX)
        .then(|| SESSION_NAME_NOT_VALID_MESSAGE.to_string())
}

/// A fresh session id.
///
/// The model states no format for it beyond `@length(1..64)` and `^[a-z0-9-]+$` on
/// `StartTelemetryQuerySessionResponse$sessionId`; every other id this backend mints and
/// documents no prefix for is a v4 UUID, and a canonical lowercase UUID is 36 characters
/// drawn from exactly that character class, so that is what this mints.
pub fn mint_query_session_id() -> String {
    super::fresh_uuid()
}

/// The prefix a session-list page token carries, so a token minted for one collection
/// cannot be replayed against another.
const QUERY_SESSION_PAGE_TOKEN_PREFIX: &str = "telemetry-query-sessions:";

/// Mint the opaque `nextToken` that resumes a session listing after `seq`.
pub fn encode_query_session_page_token(seq: u64) -> String {
    super::encode_page_token(QUERY_SESSION_PAGE_TOKEN_PREFIX, seq)
}

/// Read a session-list `nextToken` back, or `None` when it is not one this service minted
/// for a session listing.
pub fn decode_query_session_page_token(token: &str) -> Option<u64> {
    super::decode_page_token(QUERY_SESSION_PAGE_TOKEN_PREFIX, token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState as State;

    /// A stored session of `owner`, named `session_name`, at start order `seq`.
    fn stored(seq: u64, owner: &str, session_name: Option<&str>) -> StoredQuerySession {
        let at = ::aws_smithy_types::DateTime::from_secs(1_767_225_600 + seq as i64);
        StoredQuerySession {
            seq,
            session_id: format!("00000000-0000-4000-8000-{seq:012}"),
            session_name: session_name.map(str::to_string),
            owner: owner.to_string(),
            created_at: at,
            last_activity_at: at,
        }
    }

    /// A store holding `sessions`, keyed by id the way the handlers key them.
    fn store_of(sessions: Vec<StoredQuerySession>) -> State {
        let mut store = State::default();
        for session in sessions {
            store
                .query_sessions
                .insert(session.session_id.clone(), session);
        }
        store
    }

    #[test]
    fn a_name_in_the_reserved_namespace_is_not_a_callers_to_take() {
        // Rows span the `sessionName` class the model admits (`@length(1..128)`,
        // `^([a-zA-Z0-9_-]+|/aws/[a-zA-Z0-9_-]+(/[a-zA-Z0-9_-]+)?)$`): the reservation is
        // on the `/aws/` namespace, in both the forms that pattern allows, and on nothing
        // else.
        for (case, session_name, reserved) in [
            ("a plain name", "nightly-error-scan", false),
            ("the shortest name the model admits", "s", false),
            ("a digits-only name", "20260101", false),
            ("a name that merely mentions aws", "aws-nightly-scan", false),
            ("a name holding the prefix later on", "nightly-aws-scan", false),
            ("a one-segment reserved name", "/aws/incident-detection", true),
            ("a two-segment reserved name", "/aws/incident-detection/triage", true),
            ("the shortest reserved name the pattern allows", "/aws/a", true),
        ] {
            let rejection = session_name_rejection(session_name);
            assert_eq!(
                rejection.is_some(),
                reserved,
                "case {case:?}: expected a refusal of {reserved}, got {rejection:?}"
            );
            if let Some(reason) = rejection {
                assert_eq!(
                    reason, SESSION_NAME_NOT_VALID_MESSAGE,
                    "case {case:?}: the refusal states the rule once, got {reason:?}"
                );
            }
        }

        // Both published length bounds, which the rows above cannot carry as literals.
        assert_eq!(
            session_name_rejection(&"s".repeat(128)),
            None,
            "the longest name the model admits is a caller's to take"
        );
        assert_eq!(
            session_name_rejection(&format!("/aws/{}", "s".repeat(123))),
            Some(SESSION_NAME_NOT_VALID_MESSAGE.to_string()),
            "the longest reserved name the model admits is still reserved"
        );
    }

    #[test]
    fn a_listing_reports_one_callers_sessions_in_start_order() {
        // Rows differ along what a caller's sessions can differ in: how many there are,
        // whether they are named, and how many belong to another caller — because the
        // rule is "the sessions this caller holds, in the order they started".
        for (case, sessions, owner, expected) in [
            ("a caller holding none", vec![], "mine", vec![]),
            (
                "one named session",
                vec![stored(1, "mine", Some("first"))],
                "mine",
                vec![1],
            ),
            (
                "sessions inserted out of start order",
                vec![
                    stored(3, "mine", Some("third")),
                    stored(1, "mine", None),
                    stored(2, "mine", Some("second")),
                ],
                "mine",
                vec![1, 2, 3],
            ),
            (
                "another caller's sessions interleaved",
                vec![
                    stored(1, "theirs", Some("first")),
                    stored(2, "mine", Some("first")),
                    stored(3, "theirs", None),
                    stored(4, "mine", None),
                ],
                "mine",
                vec![2, 4],
            ),
            (
                "a caller with nothing of its own in a full store",
                vec![stored(1, "theirs", Some("first"))],
                "mine",
                vec![],
            ),
        ] {
            let store = store_of(sessions);
            let actual: Vec<u64> = store
                .query_sessions_after(owner, 0)
                .iter()
                .map(|stored| stored.seq)
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the start order {expected:?}, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_name_identifies_a_session_only_among_its_own_callers() {
        let store = store_of(vec![
            stored(1, "mine", Some("shared-name")),
            stored(2, "theirs", Some("theirs-only")),
            stored(3, "mine", None),
        ]);
        // (case, owner, name, whether that caller holds a session under it)
        for (case, owner, name, expected) in [
            ("the caller's own name", "mine", "shared-name", true),
            ("another caller's name", "mine", "theirs-only", false),
            ("a name nobody holds", "mine", "absent", false),
            ("the other caller's own name", "theirs", "theirs-only", true),
            ("the other caller asking for mine", "theirs", "shared-name", false),
        ] {
            let found = store.query_session_named(owner, name).is_some();
            assert_eq!(
                found, expected,
                "case {case:?}: expected a hit of {expected}, got {found}"
            );
        }
    }

    #[test]
    fn a_page_cursor_resumes_after_the_session_it_names() {
        let store = store_of(vec![
            stored(1, "mine", Some("first")),
            stored(2, "mine", Some("second")),
            stored(3, "mine", Some("third")),
        ]);
        for (case, after_seq, expected) in [
            ("from the start", 0, vec![1, 2, 3]),
            ("after the first", 1, vec![2, 3]),
            ("after the last", 3, vec![]),
        ] {
            let actual: Vec<u64> = store
                .query_sessions_after("mine", after_seq)
                .iter()
                .map(|stored| stored.seq)
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            let token = encode_query_session_page_token(after_seq);
            assert_eq!(
                decode_query_session_page_token(&token),
                Some(after_seq),
                "case {case:?}: the token {token:?} must read back as the cursor it carries"
            );
        }
        assert_eq!(
            decode_query_session_page_token(&crate::state::encode_view_page_token(1)),
            None,
            "a token minted for another collection must not resume a session listing"
        );
    }
}
