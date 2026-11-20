// SPDX-License-Identifier: CC0-1.0

//! One chat's customer-authentication session: what the `authenticationInitiated` event
//! minted for it, the window the participant may act on it in, and the sign-in URL it
//! leads to.
//!
//! `GetAuthenticationUrl` "Retrieves the AuthenticationUrl for the current authentication
//! session for the AuthenticateCustomer flow block" and `CancelParticipantAuthentication`
//! "Cancels the authentication session", and both name that session by "The sessionId
//! provided in the authenticationInitiated event". So the session is minted where
//! authentication is INITIATED — the contact flow running the AuthenticateCustomer block,
//! with the Amazon Cognito user pool that block is configured with — and no call this
//! service's API carries creates one, exactly as no call here creates the chat session
//! itself or shows a view. The component that initiates authentication therefore writes
//! this record into this store (the engine keys state by TYPE, so a copy of its own would
//! be invisible to the readers here) and the two operations only report it and close it;
//! nothing in this service invents a session, a user pool or an identity provider for one.
//!
//! The store is bounded: a session is reclaimed once its window has closed, and with the
//! chat session whose contact it belongs to (`State::prune_expired`). Nothing a caller can
//! drive adds one.

use super::*;

/// How long after the `authenticationInitiated` event a session may be acted on.
///
/// `GetAuthenticationUrl`'s own documentation: "This API can only be called within one
/// minute of receiving the authenticationInitiated event." Past that the session is no
/// longer the current one, so neither operation reaches it and
/// [`ComAmazonawsConnectparticipantAmazonconnectparticipantservicelambdaState::prune_expired`]
/// reclaims it.
pub const AUTHENTICATION_SESSION_WINDOW: Duration = Duration::from_secs(60);

/// The longest `AuthenticationUrl` this service may report: `AuthenticationUrl` is
/// modelled `length 1..2083`, so a composed sign-in URL past it is not a value this
/// operation can answer with.
pub const MAX_AUTHENTICATION_URL_CHARS: usize = 2083;

/// The `ValidationException$Message` the two authentication operations refuse a request
/// whose parameters they cannot act on with.
///
/// The sentence is FIXED: nothing in it comes from the request or from this store. One
/// wording covers every reason a `SessionId` is not the chat's current authentication
/// session (unknown, another contact's, already cancelled, past its window) and every
/// other parameter these operations reject, so the refusal neither names the rejected
/// value nor says which sessions exist for whom — a caller holding no such session learns
/// only that what it submitted was invalid.
pub const INVALID_REQUEST_PARAMETERS_MESSAGE: &str = "Specified request parameter(s) are invalid.";

/// The OAuth 2.0 `response_type` an Amazon Cognito authorize endpoint is sent for the
/// authorization-code flow the AuthenticateCustomer block completes on the redirect.
const RESPONSE_TYPE_CODE: &str = "code";

/// One customer-authentication session of one chat, as the flow block that initiated it
/// recorded it.
///
/// Every member is part of what the sign-in URL is composed of, so
/// [`Self::authentication_url`] derives nothing from the request but the redirect the
/// caller asked for.
#[derive(Debug, Clone)]
pub struct AuthenticationSession {
    /// The contact whose participant this session authenticates. Only a connection token
    /// that authenticates THIS contact's participant may reach it: a session minted for
    /// one chat is not a session of another's.
    pub contact_id: String,
    /// When the `authenticationInitiated` event was raised, which is what the window in
    /// [`AUTHENTICATION_SESSION_WINDOW`] is measured from.
    pub initiated_at: SystemTime,
    /// The authorize endpoint of the Cognito user pool the flow block is configured with:
    /// "This URL contains the authorize endpoint for the Cognito UserPool used in the
    /// authentication" (`GetAuthenticationUrlResponse$AuthenticationUrl`).
    pub authorize_endpoint: String,
    /// The user-pool app client the flow block authenticates the customer through.
    pub client_id: String,
    /// The OAuth scopes that app client asks for, in the order it asks for them.
    pub scopes: Vec<String>,
    /// True once `CancelParticipantAuthentication` ran: the opted-out branch of the flow
    /// block has been taken, so the session is no longer one a sign-in URL is reported for.
    pub cancelled: bool,
}

impl AuthenticationSession {
    /// When this session stops being the current authentication session.
    ///
    /// Added with `checked_add`, so an `initiated_at` the clock could never produce cannot
    /// panic here: an instant whose window cannot be represented is treated as one whose
    /// window has closed, the safe direction for a session nothing may act on.
    #[must_use]
    pub fn window_closes_at(&self) -> SystemTime {
        self.initiated_at
            .checked_add(AUTHENTICATION_SESSION_WINDOW)
            .unwrap_or(self.initiated_at)
    }

    /// Whether this session is still the one the participant may act on at `now`: inside
    /// its window and not cancelled.
    #[must_use]
    pub fn is_current(&self, now: SystemTime) -> bool {
        !self.cancelled && now < self.window_closes_at()
    }

    /// The URL the customer signs in to the identity provider at: this session's authorize
    /// endpoint, carrying the app client, the scopes it asks for, `session_id` as the
    /// OAuth `state` that ties the redirect back to this session, and `redirect_uri` —
    /// "The URL where the customer will be redirected after Amazon Cognito authorizes the
    /// user" — as the request named it.
    ///
    /// Composed through the `url` crate, so every value is percent-encoded where the query
    /// string needs it rather than pasted in. `None` when the stored authorize endpoint is
    /// not a URL at all: the caller reports that as a server-side failure rather than
    /// handing out an address that leads nowhere.
    ///
    /// The redirect is carried as the request named it. Whether it is one of the callback
    /// URLs the app client registered is a question for the component that records those
    /// URLs alongside the session, and nothing in this workspace writes them yet, so no
    /// such check is made here — a registered-callback refusal belongs with the record it
    /// would be checked against.
    #[must_use]
    pub fn authentication_url(&self, session_id: &str, redirect_uri: &str) -> Option<String> {
        let mut url = ::url::Url::parse(&self.authorize_endpoint).ok()?;
        url.query_pairs_mut()
            .append_pair("response_type", RESPONSE_TYPE_CODE)
            .append_pair("client_id", &self.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", session_id);
        if !self.scopes.is_empty() {
            url.query_pairs_mut()
                .append_pair("scope", &self.scopes.join(" "));
        }
        Some(url.into())
    }
}

impl ComAmazonawsConnectparticipantAmazonconnectparticipantservicelambdaState {
    /// The authentication session `session_id` names, when it is `contact_id`'s own and is
    /// still current at `now`.
    ///
    /// An id this store holds no session for, one of another contact's chat, one already
    /// cancelled and one past its window all resolve to nothing: which authentication
    /// session a caller can reach is decided by the contact its connection token
    /// authenticates and by the clock.
    pub fn authentication_session(
        &self,
        session_id: &str,
        contact_id: &str,
        now: SystemTime,
    ) -> Option<&AuthenticationSession> {
        self.authentication_sessions
            .get(session_id)
            .filter(|session| session.contact_id == contact_id && session.is_current(now))
    }

    /// [`Self::authentication_session`], for the caller that closes the session.
    pub fn authentication_session_mut(
        &mut self,
        session_id: &str,
        contact_id: &str,
        now: SystemTime,
    ) -> Option<&mut AuthenticationSession> {
        self.authentication_sessions
            .get_mut(session_id)
            .filter(|session| session.contact_id == contact_id && session.is_current(now))
    }
}
