// SPDX-License-Identifier: CC0-1.0

//! The Session Manager sessions this endpoint holds: the record `StartSession` persists,
//! what ends one, and how a report filters and pages them.
//!
//! A session is "a Session Manager connection to a managed node" (`Session`), so one exists
//! only because a caller started it against a node this endpoint reports as managed
//! ([`super::node_registration`]) — nothing here invents one, and a target no node of this
//! Region answers for is refused rather than recorded. The record is what
//! `DescribeSessions` reports, in the two sets `SessionState` declares: a session that is
//! still connected is `Active`, and one that has ended is `History`.
//!
//! The members a session carries about ITSELF — its id, its owner, the instant it began and
//! the instant it ended — are this service's own; the ones the caller supplied (the target,
//! the document, the reason) are persisted as given. The ones Session Manager fills in from
//! a preferences document it holds none of (`MaxSessionDuration`), and the two the model
//! documents as "Reserved for future use" (`Details`, `OutputUrl`), are left unset rather
//! than invented.

use std::time::SystemTime;

use crate::ops::types::Session;

/// The `SessionStatus` a session a caller has just started carries: the connection to the
/// node is established, which is what the token and the stream URL the start answered with
/// are for (from the modelled enum, `crate::ops::types::SESSION_STATUS_VALUES`).
pub const SESSION_STATUS_CONNECTED: &str = "Connected";

/// The `SessionStatus` of a session that has been ended: "Permanently ends a session"
/// (`TerminateSession`), so the end is performed by that call rather than owed by the clock.
pub const SESSION_STATUS_TERMINATED: &str = "Terminated";

/// The `SessionState` of a session a client can still use (from the modelled enum,
/// `crate::ops::types::SESSION_STATE_VALUES`).
pub const SESSION_STATE_ACTIVE: &str = "Active";

/// The `SessionState` of a session that has ended.
pub const SESSION_STATE_HISTORY: &str = "History";

/// The `SessionStatus` values that are no longer a usable connection, and so the ones a
/// `History` report holds: the session has ended, is ending, or never came up (from the
/// modelled enum).
pub const ENDED_SESSION_STATUSES: &[&str] = &["Terminated", "Terminating", "Failed"];

/// The refusal a session that is in no state for the change a request asks of it is answered
/// with: a FIXED sentence with no variable parts, which is how Session Manager words this one
/// — the id a caller sent is not repeated back to it.
///
/// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, unit
/// `ssm.terminate-session-already-terminated-noop`): a second `TerminateSession` on a session
/// already in the ended set is answered 400 `ValidationException` with exactly this sentence.
/// `TerminateSession` models no error of its own for the condition, so it travels as this
/// service's boundary refusal ([`crate::scaffolded`]'s validation channel).
pub const SESSION_NOT_IN_A_VALID_STATE: &str = "Session is not in a valid state";

/// The `AccessType` of a session a caller started directly: "`Standard` access type is the
/// default for Session Manager sessions" (`Session$AccessType`).
pub const ACCESS_TYPE_STANDARD: &str = "Standard";

/// The `ConnectionStatus` of a managed node Session Manager can reach: it "is running and
/// ready to receive Session Manager connections" (`GetConnectionStatus`), which is the
/// state an agent that is reporting itself puts its node in (from the modelled enum,
/// `crate::ops::types::CONNECTION_STATUS_VALUES`).
pub const CONNECTION_STATUS_CONNECTED: &str = "connected";

/// The `ConnectionStatus` of a target this endpoint cannot reach (same enum).
pub const CONNECTION_STATUS_NOTCONNECTED: &str = "notconnected";

/// How far back a report of ENDED sessions reaches: "Retrieves a list of all active
/// sessions (both connected and disconnected) or terminated sessions from the past 30
/// days" (`DescribeSessions`), so a session that ended before that window is no longer
/// reported under `History`.
pub const SESSION_HISTORY_WINDOW: ::std::time::Duration =
    ::std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// The largest number of sessions this in-memory store holds, across every Region.
///
/// What keeps this process finite: a terminated session stays in the store because
/// `DescribeSessions` reports it under `History`, so nothing reclaims one, and the model
/// bounds how many an account may start by nothing at all.
pub const MAX_SESSIONS_TOTAL: usize = 1_000;

/// How many sessions a page holds when the request names no `MaxResults`.
///
/// `DescribeSessionsRequest$MaxResults` documents no default ("The maximum number of items
/// to return for this call"), so this is the emulator's CHOSEN page size, inside the
/// modelled 1..200 range.
pub const DEFAULT_SESSION_PAGE_SIZE: i32 = 50;

/// The bounds the modelled `SessionMaxResults` carries (range 1..200).
pub const MIN_SESSION_PAGE_SIZE: i32 = 1;

/// See [`MIN_SESSION_PAGE_SIZE`].
pub const MAX_SESSION_PAGE_SIZE: i32 = 200;

/// The page size a session report serves, or the refusal message for a `MaxResults` outside
/// the modelled range — which the protocol's boundary validator already refuses, so this is
/// a backstop rather than the first line of defence.
pub fn session_page_size(max_results: Option<i32>) -> Result<usize, String> {
    let requested = max_results.unwrap_or(DEFAULT_SESSION_PAGE_SIZE);
    if !(MIN_SESSION_PAGE_SIZE..=MAX_SESSION_PAGE_SIZE).contains(&requested) {
        return Err(format!(
            "Value '{requested}' at 'maxResults' failed to satisfy constraint: Member must be \
             between {MIN_SESSION_PAGE_SIZE} and {MAX_SESSION_PAGE_SIZE}"
        ));
    }
    Ok(requested as usize)
}

/// One Session Manager session, as the record this store holds it by.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// "The ID of the session" (`Session$SessionId`), minted by [`mint_session_id`].
    pub session_id: String,
    /// "The managed node that the Session Manager session connected to"
    /// (`Session$Target`), as the request named it.
    pub target: String,
    /// `Session$Status`, a `SessionStatus` value.
    pub status: String,
    /// "The date and time … when the session began" (`Session$StartDate`).
    pub start_date: SystemTime,
    /// "The date and time … when the session was terminated" (`Session$EndDate`), unset
    /// while the session is still connected.
    pub end_date: Option<SystemTime>,
    /// "The name of the Session Manager SSM document used to define the parameters and
    /// plugin settings for the session" (`Session$DocumentName`), when the request named
    /// one; a session started without one runs the shell the operation launches by default,
    /// which is no document of this account.
    pub document_name: Option<String>,
    /// "The ID of the Amazon Web Services user that started the session"
    /// (`Session$Owner`) — the principal the request spoke for, as
    /// `sts:GetCallerIdentity` reports it.
    pub owner: String,
    /// "The reason for connecting to the instance" (`Session$Reason`), as supplied.
    pub reason: Option<String>,
}

impl SessionRecord {
    /// The session a start records: connected, from this instant, with no end.
    pub fn started(
        session_id: String,
        target: String,
        owner: String,
        document_name: Option<String>,
        reason: Option<String>,
        at: SystemTime,
    ) -> Self {
        Self {
            session_id,
            target,
            status: SESSION_STATUS_CONNECTED.to_string(),
            start_date: at,
            end_date: None,
            document_name,
            owner,
            reason,
        }
    }

    /// Which of the two sets `SessionState` declares this session is reported in: one that
    /// has ended is `History`, and every other one is still `Active`.
    pub fn state(&self) -> &'static str {
        if ENDED_SESSION_STATUSES.contains(&self.status.as_str()) {
            SESSION_STATE_HISTORY
        } else {
            SESSION_STATE_ACTIVE
        }
    }

    /// End this session: "Permanently ends a session and closes the data connection between
    /// the Session Manager client and SSM Agent on the managed node" (`TerminateSession`).
    ///
    /// A session that has already ended keeps the instant it ended at: the end happened
    /// once, and a second request to end it changes nothing. Such a request is not served at
    /// all ([`Self::can_be_terminated`]), so this is the one end of a session there is.
    pub fn terminate(&mut self, at: SystemTime) {
        if self.has_ended() {
            return;
        }
        self.status = SESSION_STATUS_TERMINATED.to_string();
        self.end_date = Some(at);
    }

    /// Whether this session has ENDED: its status is one of the set a `History` report holds
    /// ([`ENDED_SESSION_STATUSES`]), which is the one reading every question about a session's
    /// usefulness is answered from, so no two of them can drift apart.
    fn has_ended(&self) -> bool {
        self.state() == SESSION_STATE_HISTORY
    }

    /// Whether an END of this session is a state it can be put in: a session that has not
    /// ended can be ended, and one that has is in no state to be ended again.
    ///
    /// RECORDED BEHAVIOUR (corpus `simparity-behavioral-aws-v1`, unit
    /// `ssm.terminate-session-already-terminated-noop`): a second `TerminateSession` on a
    /// session already in the ended set is refused `ValidationException`
    /// ([`SESSION_NOT_IN_A_VALID_STATE`]) and the history keeps exactly the one record of it.
    /// It is the SET rather than the one status, because every status in it is a session this
    /// service holds no live connection for ([`ENDED_SESSION_STATUSES`]) and so none of them
    /// is one an end still applies to — the same reading [`Self::is_resumable`] takes, which
    /// is why both read it from [`Self::has_ended`].
    pub fn can_be_terminated(&self) -> bool {
        !self.has_ended()
    }

    /// Whether a connection to this session can still be re-established: "connections can be
    /// resumed for disconnected sessions, but not terminated sessions" (`ResumeSession`), so
    /// every session that has not ended can be, and no session that has.
    ///
    /// A session still reported as connected is one of those: this service learns of a
    /// client's data channel dropping from nothing, so the status a session carries says what
    /// Session Manager holds of it rather than what the client's end of the connection is
    /// doing — which is why a client reconnecting "during intermittent network issues" is
    /// served whatever the session was last reported as.
    pub fn is_resumable(&self) -> bool {
        !self.has_ended()
    }

    /// Reconnect this session: "Reconnects a session to a managed node after it has been
    /// disconnected. Connections can be resumed for disconnected sessions, but not
    /// terminated sessions" (`ResumeSession`).
    ///
    /// Answers whether the session was reconnected: a session no connection can be
    /// re-established to ([`SessionRecord::is_resumable`]) is not, and nothing about it
    /// changes. A reconnect continues the session it was asked for rather than starting
    /// another, so the instant it began is the one its start recorded; what it writes is the
    /// connected status, which is the status every session this store holds a live connection
    /// for carries, whichever of the other live statuses it was in.
    pub fn resume(&mut self) -> bool {
        if !self.is_resumable() {
            return false;
        }
        self.status = SESSION_STATUS_CONNECTED.to_string();
        true
    }

    /// Whether a report of this session's own state still reaches it at `now`.
    ///
    /// A session that is still usable is always reported; an ENDED one is reported for
    /// [`SESSION_HISTORY_WINDOW`] after it ended. A session whose end instant was somehow
    /// not recorded is reported from its start, which is no later than its end.
    pub fn within_report_window(&self, now: SystemTime) -> bool {
        if self.state() == SESSION_STATE_ACTIVE {
            return true;
        }
        let ended = self.end_date.unwrap_or(self.start_date);
        now.duration_since(ended)
            .is_ok_and(|since| since <= SESSION_HISTORY_WINDOW)
            || ended >= now
    }

    /// This session as the API reports it — the `Session` shape.
    pub fn to_session(&self) -> Session {
        Session {
            session_id: Some(self.session_id.clone()),
            target: Some(self.target.clone()),
            status: Some(self.status.clone()),
            start_date: Some(::aws_smithy_types::DateTime::from(self.start_date)),
            end_date: self.end_date.map(::aws_smithy_types::DateTime::from),
            document_name: self.document_name.clone(),
            owner: Some(self.owner.clone()),
            reason: self.reason.clone(),
            access_type: Some(ACCESS_TYPE_STANDARD.to_string()),
            // Reserved for future use, per the model, and filled by nothing here.
            details: None,
            output_url: None,
            // Set from the account's Session Manager preferences document, which this
            // store holds none of.
            max_session_duration: None,
        }
    }

    /// Whether this session satisfies one `SessionFilter`: the value a filter key reads off
    /// a session, compared with the value the filter carries.
    ///
    /// Every key is one of the modelled `SessionFilterKey` values, each compared as its own
    /// documentation says (`SessionFilter$value`): the two instant keys bound the session's
    /// start, and every other key compares a whole value. A key outside that enum — which
    /// the boundary validator has already refused — matches nothing.
    pub fn matches_filter(&self, key: &str, value: &str) -> bool {
        match key {
            "SessionId" => self.session_id == value,
            "Target" => self.target == value,
            "Owner" => self.owner == value,
            "Status" => self.status == value,
            "AccessType" => ACCESS_TYPE_STANDARD == value,
            // "Specify a timestamp to limit your results … to see sessions that started
            // August 29, 2024, and later", and the mirror of it for `InvokedBefore`.
            "InvokedAfter" => instant(value).is_some_and(|bound| self.start_date >= bound),
            "InvokedBefore" => instant(value).is_some_and(|bound| self.start_date < bound),
            _ => false,
        }
    }
}

/// The two filter keys whose value is "a timestamp to limit your results"
/// (`SessionFilter$value`).
pub const INSTANT_SESSION_FILTER_KEYS: &[&str] = &["InvokedAfter", "InvokedBefore"];

/// Why one `SessionFilter` could not be served, or `None` for a filter this service can
/// compare.
///
/// The keys themselves are the modelled `SessionFilterKey` enum, so the boundary validator
/// refuses one outside it before a handler runs; what is left is a VALUE that could not be
/// a value of the attribute its key filters — an instant that is not an instant, and a
/// status outside the set that member's own documentation lists.
pub fn filter_rejection(key: &str, value: &str) -> Option<String> {
    if INSTANT_SESSION_FILTER_KEYS.contains(&key) && instant(value).is_none() {
        return Some(format!(
            "Value '{value}' at 'filters.value' failed to satisfy constraint: Member must be a \
             timestamp"
        ));
    }
    if key == "Status" && !SESSION_STATUS_FILTER_VALUES.contains(&value) {
        // The sentence this service refuses a filter value outside its key's published set
        // with, so a caller reads the same wording whichever report refused the filter.
        return Some(super::node_filter_value_set_rejection(
            key,
            SESSION_STATUS_FILTER_VALUES,
        ));
    }
    None
}

/// The values the `Status` filter key accepts, as `SessionFilter$value` publishes them:
/// "Status values you can specify include: Connected, Connecting, Disconnected, Terminated,
/// Terminating, Failed" — the modelled `SessionStatus` enum, listed in the order that
/// member's own documentation lists it, which is the order a refusal lists them in.
pub const SESSION_STATUS_FILTER_VALUES: &[&str] = &[
    "Connected",
    "Connecting",
    "Disconnected",
    "Terminated",
    "Terminating",
    "Failed",
];

/// The instant `value` names — "specify a timestamp to limit your results. For example,
/// specify 2024-08-29T00:00:00Z" (`SessionFilter$value`) — or `None` for a value that names
/// no instant.
///
/// Read by this service's ONE reading of a caller-supplied timestamp
/// ([`super::request_instant`]), so the two instant keys here and the time keys of another
/// report accept the same spellings of one instant.
fn instant(value: &str) -> Option<SystemTime> {
    super::request_instant(value)
}

/// Whether `value` could be a `SessionId` this service minted, which is what a paged
/// report's continuation token is: the modelled `SessionId` is a string of 1..96
/// characters, so a longer value is no id of ours and therefore no token of ours either.
pub fn could_be_session_id(value: &str) -> bool {
    (MIN_SESSION_ID_CHARS..=MAX_SESSION_ID_CHARS).contains(&value.chars().count())
}

/// The bounds the modelled `SessionId` carries (length 1..96).
const MIN_SESSION_ID_CHARS: usize = 1;

/// See [`MIN_SESSION_ID_CHARS`].
const MAX_SESSION_ID_CHARS: usize = 96;

/// How long the token a start hands its caller is: the maximum the modelled `TokenValue`
/// carries (length 0..300), which is how much of the secret minted for a session the token
/// is.
pub const MAX_TOKEN_VALUE_CHARS: usize = 300;

/// The `SessionId` a started session is identified by: the name of the user that started it,
/// a hyphen, and a draw from this crate's ONE identifier source ([`super::new_uuid`]).
///
/// That is the form Session Manager mints — a session id names who started the session — and
/// the draw is what makes it unique, so nothing of the request's target or document is read
/// back out of it. `owner` is the principal's ARN, whose last segment is the user or session
/// name; a character no IAM entity name carries is dropped rather than put in an id.
pub fn mint_session_id(owner: &str) -> String {
    let name: String = owner
        .split('/')
        .next_back()
        .unwrap_or_default()
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '@')
        })
        .take(MAX_SESSION_ID_NAME_CHARS)
        .collect();
    let unique = super::new_uuid();
    if name.is_empty() {
        return unique;
    }
    format!("{name}-{unique}")
}

/// How much of the owner's name a session id carries, so that the id stays inside the
/// modelled `SessionId` length (1..96) whatever the principal is called.
const MAX_SESSION_ID_NAME_CHARS: usize = 32;

/// "A URL back to SSM Agent on the managed node that the Session Manager client uses to send
/// commands and receive output from the node" (`StartSessionResponse$StreamUrl`), on the
/// data-channel path the model documents that URL's format with.
///
/// `authority` is the `host[:port]` THIS emulator is served at
/// (`::service_common::data_plane_addressing::emulator_endpoint_authority`), never a
/// Systems Manager hostname: a URL this emulator hands a client to connect to later has to
/// lead back to this emulator, which is the only address a client on this machine can
/// reach.
pub fn stream_url(authority: &str, session_id: &str) -> String {
    format!("wss://{authority}/v1/data-channel/{session_id}?stream=input")
}

/// The token a call hands the caller of a session it opened or reconnected: "an encrypted
/// token value containing session and caller information … used to authenticate the
/// connection to the managed node" (`StartSessionResponse$TokenValue`), or `None` when the
/// value minted for it carries no secret.
///
/// A secret, so it is drawn from the one component that generates secrets rather than
/// constructed here. It is NOT a credential of this emulator and must never be accepted as
/// one, so the entry minted for it is removed again. The modelled `TokenValue` carries
/// 0..300 characters, so the token is a PREFIX of the generated secret
/// ([`MAX_TOKEN_VALUE_CHARS`]) — never a value stretched to fill the member.
///
/// Every call that hands a caller a session token draws it here, so a start and a reconnect
/// cannot hand out tokens of two different kinds.
pub fn mint_token_value(ctx: &::smithy_server_runtime::OperationContext<'_>) -> Option<String> {
    let minted = ctx
        .credential_store()
        .create(&::emulator_server::aws_credentials::AWS_SESSION);
    let token_value = minted
        .secret(::emulator_server::aws_credentials::SESSION_TOKEN)
        .map(|secret| secret.chars().take(MAX_TOKEN_VALUE_CHARS).collect::<String>());
    // Removing the entry again is what keeps the token from being accepted as a credential
    // of this emulator. The one answer other than success is that the store holds no entry
    // under that id, which is the state the removal exists to reach, so both answers leave
    // nothing of this token in the store.
    let _ = ctx.credential_store().delete(&minted.id);
    token_value
}

/// How this service refuses a `SessionId` there is no session to reconnect behind:
/// "connections can be resumed for disconnected sessions, but not terminated sessions"
/// (`ResumeSession`), so what the refusal states is that the session is in no state a
/// reconnect can act on.
///
/// One sentence covers every such request — an id this endpoint holds no session under as
/// much as one whose session has ended — because the refusal is about the state a reconnect
/// needs and says nothing else of the request: the text is fixed and echoes no id.
pub const SESSION_NOT_RESUMABLE_MESSAGE: &str = "Session is not in a valid state";

impl super::ComAmazonawsSsmAmazonssmState {
    /// The `ConnectionStatus` this endpoint reports for `target`: "whether it is running and
    /// ready to receive Session Manager connections" (`GetConnectionStatus`).
    ///
    /// A target is connected exactly when this Region holds it as a managed node whose agent
    /// is responding ([`super::ComAmazonawsSsmAmazonssmState::node_agent_is_responding`]),
    /// which is the same fact that decides whether a session can be opened to it — so a
    /// report and a start cannot disagree about one target. Nothing about the SESSIONS of
    /// the target is read: a connected node is ready to RECEIVE a connection whether or not
    /// one has been opened to it.
    pub fn connection_status(&self, region: &str, target: &str) -> &'static str {
        if self.node_agent_is_responding(region, target) {
            CONNECTION_STATUS_CONNECTED
        } else {
            CONNECTION_STATUS_NOTCONNECTED
        }
    }

    /// Whether this store has room for another session ([`MAX_SESSIONS_TOTAL`]).
    pub fn can_hold_another_session(&self) -> bool {
        self.sessions.len() < MAX_SESSIONS_TOTAL
    }

    /// Store `session` as a session of `region`, keyed by the id it carries.
    pub fn insert_session(&mut self, region: &str, session: SessionRecord) {
        let key = (region.to_string(), session.session_id.clone());
        self.sessions.insert(key, session);
    }

    /// The session `session_id` names through `region`'s endpoint, or `None` when that
    /// Region holds none: a session is reached through the endpoint it was started at.
    pub fn session(&self, region: &str, session_id: &str) -> Option<&SessionRecord> {
        self.sessions
            .get(&(region.to_string(), session_id.to_string()))
    }

    /// The session `session_id` names through `region`'s endpoint, to be changed, or `None`
    /// when that Region holds none: a session is reached through the endpoint it was
    /// started at.
    pub fn session_mut(&mut self, region: &str, session_id: &str) -> Option<&mut SessionRecord> {
        self.sessions
            .get_mut(&(region.to_string(), session_id.to_string()))
    }

    /// Every session `region` holds, in ascending session-id order, as
    /// `(session id, record)`.
    ///
    /// The order is what makes a report's resume cursor total: it is a session id, not an
    /// offset into a list that may have changed between two pages.
    pub fn sessions_in_region<'a>(
        &'a self,
        region: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a SessionRecord)> + 'a {
        self.sessions
            .iter()
            .filter(move |((held, _), _)| held == region)
            .map(|((_, id), record)| (id.as_str(), record))
    }
}

#[cfg(test)]
mod tests {
    //! The listings this module states by hand, held to the model they come from, so a
    //! model revision cannot leave one of them behind.

    use super::*;

    /// The set the `Status` filter publishes is the modelled `SessionStatus` enum: the
    /// listing here differs from the generated constant only in ORDER, which is the order
    /// the filter's own documentation lists the values in.
    #[test]
    fn the_status_filter_publishes_the_modelled_statuses() {
        let mut published = SESSION_STATUS_FILTER_VALUES.to_vec();
        published.sort_unstable();
        let mut modelled = crate::ops::types::SESSION_STATUS_VALUES.to_vec();
        modelled.sort_unstable();
        assert_eq!(
            published, modelled,
            "expected the published set to be the modelled SessionStatus values, got \
             {published:?} against {modelled:?}"
        );
    }

    /// Every status a session is reported in the HISTORY set under is one the model
    /// declares, and the two sets it is sorted into are the modelled `SessionState` values.
    #[test]
    fn the_ended_statuses_and_the_two_states_come_from_the_model() {
        for status in ENDED_SESSION_STATUSES {
            assert!(
                crate::ops::types::SESSION_STATUS_VALUES.contains(status),
                "expected {status} to be a modelled SessionStatus value"
            );
        }
        let mut states = vec![SESSION_STATE_ACTIVE, SESSION_STATE_HISTORY];
        states.sort_unstable();
        let mut modelled = crate::ops::types::SESSION_STATE_VALUES.to_vec();
        modelled.sort_unstable();
        assert_eq!(
            states, modelled,
            "expected the two reported states to be the modelled SessionState values, got \
             {states:?} against {modelled:?}"
        );
    }

    /// The two statuses a connection report answers with are the modelled
    /// `ConnectionStatus` values, and nothing else is.
    #[test]
    fn the_reported_connection_statuses_come_from_the_model() {
        let mut reported = vec![CONNECTION_STATUS_CONNECTED, CONNECTION_STATUS_NOTCONNECTED];
        reported.sort_unstable();
        let mut modelled = crate::ops::types::CONNECTION_STATUS_VALUES.to_vec();
        modelled.sort_unstable();
        assert_eq!(
            reported, modelled,
            "expected the two reported connection statuses to be the modelled \
             ConnectionStatus values, got {reported:?} against {modelled:?}"
        );
    }

    /// A reconnect is served for a session in EVERY status that is still a connection
    /// ("connections can be resumed for disconnected sessions, but not terminated
    /// sessions"), putting it back in the connected status and keeping the instant it began,
    /// and for no status that has ended — where it changes nothing at all.
    #[test]
    fn a_reconnect_serves_every_live_status_and_no_ended_one() {
        let began = SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_700_000_000);
        let ended = began + ::std::time::Duration::from_secs(60);
        for status in crate::ops::types::SESSION_STATUS_VALUES {
            let mut session = SessionRecord::started(
                "a-session".to_string(),
                "i-0123456789abcdef0".to_string(),
                "arn:aws:sts::123456789012:assumed-role/role/session".to_string(),
                None,
                None,
                began,
            );
            session.status = (*status).to_string();
            session.end_date = ENDED_SESSION_STATUSES
                .contains(status)
                .then_some(ended);
            let was_ended = session.state() == SESSION_STATE_HISTORY;
            let before = session.clone();

            assert_eq!(
                session.is_resumable(),
                !was_ended,
                "case {status}: expected a session in this status to be resumable exactly \
                 when it has not ended"
            );
            assert_eq!(
                session.resume(),
                !was_ended,
                "case {status}: expected the reconnect to be served exactly when the session \
                 has not ended"
            );
            if was_ended {
                assert_eq!(
                    (session.status.as_str(), session.end_date),
                    (before.status.as_str(), before.end_date),
                    "case {status}: expected a refused reconnect to change nothing, got \
                     {session:?}"
                );
                continue;
            }
            assert_eq!(
                session.status, SESSION_STATUS_CONNECTED,
                "case {status}: expected the session to be connected again, got {session:?}"
            );
            assert_eq!(
                (session.start_date, session.end_date),
                (before.start_date, None),
                "case {status}: expected the reconnect to continue the session it was asked \
                 for, got {session:?}"
            );
        }
    }
}
