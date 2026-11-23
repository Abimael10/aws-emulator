// SPDX-License-Identifier: CC0-1.0

//! How a client REACHES one session: the endpoint it connects to and the token it presents.
//!
//! `GetSessionEndpoint` "Gets a connection endpoint and authentication token for a given
//! session Id", and its response carries exactly those three things —
//! `GetSessionEndpointResponse$EndpointUrl` ("The endpoint for connecting to the session"),
//! `$AuthToken` ("Authentication token for the connection") and
//! `$AuthTokenExpirationTime` ("Expiration time of the auth token"). Three rules follow, and
//! this module states each once:
//!
//! * a client connects to a session that is RUNNING, so a session whose resources are no
//!   longer up has no connection to hand out and the call is refused
//!   ([`SESSION_ENDPOINT_UNAVAILABLE`]);
//! * the endpoint is an address the caller then CONNECTS to, so it leads back to this
//!   emulator ([`session_endpoint_url`]) rather than to a host AWS publishes its own
//!   endpoints at;
//! * the token is minted per call, over the session it is issued for and under a secret only
//!   this store holds ([`ComAmazonawsAthenaAmazonathenaState::session_endpoint_auth_token`]),
//!   so a token is a value only this store could have produced, it names which session it
//!   was issued for
//!   ([`ComAmazonawsAthenaAmazonathenaState::session_endpoint_auth_token_session`]), and it
//!   is good for a bounded span ([`SESSION_ENDPOINT_AUTH_TOKEN_LIFETIME`]) — which is what
//!   an expiring token asks a client to call again for.
//!
//! What this module does NOT hold is the session's own data plane: this emulator runs no
//! Spark server for a session, so what answers AT the address below is this emulator's own
//! API rather than a session protocol. The address is still this process's, which is what
//! the member is for — an endpoint naming a host nothing here answers on would take a client
//! out of the emulated environment carrying the token it is holding.

use super::*;

/// How long the authentication token one call issues is good for.
///
/// CHOSEN, not an AWS duration: `AuthTokenExpirationTime` is documented as the "Expiration
/// time of the auth token" and names no span, so this is a bounded lifetime of this
/// emulator's own — the same ten minutes Athena publishes for the other token it hands out
/// for a session ([`NOTEBOOK_AUTH_TOKEN_LIFETIME`], "must be called every 10 minutes to
/// refresh the authentication token"), so one session's two grants expire on one span.
pub const SESSION_ENDPOINT_AUTH_TOKEN_LIFETIME: Duration = NOTEBOOK_AUTH_TOKEN_LIFETIME;

/// The path segment naming the session a connection endpoint addresses.
const SESSIONS_PATH_SEGMENT: &str = "sessions";

/// The message a request for the connection endpoint of a session that is no longer running
/// is refused with.
///
/// The SAME sentence Athena refuses the other grant over a session that is not open with
/// ([`NOTEBOOK_SESSION_NOT_OPEN`]), held under this name so each operation states where its
/// refusal comes from: a session whose resources are gone accepts no connection, which is
/// the same condition as its serving no notebook, and Athena words that class as fixed text
/// carrying nothing of the request.
pub const SESSION_ENDPOINT_UNAVAILABLE: &str = NOTEBOOK_SESSION_NOT_OPEN;

/// The endpoint a client connects to one session at: the address THIS emulator is reached
/// at, with the path naming the session being connected to.
///
/// A client reads this value and then CONNECTS to it, so it has to lead back to this
/// emulator: `served_base` is [`::smithy_server_runtime::OperationContext::endpoint`], "the
/// base URL a later request addresses", and no part of the value is derived from a host AWS
/// publishes its own endpoints at — an address under one of those resolves nowhere near this
/// process or, worse, to genuine AWS infrastructure.
///
/// The session's own path is kept, so every call about one session reports the same endpoint
/// and two sessions never share one.
pub fn session_endpoint_url(served_base: &str, session_id: &str) -> String {
    let base = served_base.trim_end_matches('/');
    format!("{base}/{SESSIONS_PATH_SEGMENT}/{session_id}")
}

/// The cursor a session connection token is computed over: the session the token is issued
/// for and the nonce of this one issue.
fn session_endpoint_auth_cursor(session_id: &str, nonce: &str) -> String {
    format!("sessionendpointauth{CURSOR_SEPARATOR}{session_id}{CURSOR_SEPARATOR}{nonce}")
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// A FRESH authentication token for connecting to session `session_id`: this store's own
    /// tag over which session the connection is for, rendered as hexadecimal.
    ///
    /// Each call mints another one — the nonce it carries is drawn per issue — so a caller
    /// that asks again is handed a different token for the same session, which is what an
    /// expiring token asks of a client. Nothing is written: the token carries everything
    /// needed to resolve it ([`Self::session_endpoint_auth_token_session`]), so handing one
    /// out leaves the store exactly as the previous caller saw it.
    pub fn session_endpoint_auth_token(&self, session_id: &str) -> String {
        let nonce = ::uuid::Uuid::new_v4().as_hyphenated().to_string();
        ::hex::encode(
            self.session_endpoint_key
                .mint(&session_endpoint_auth_cursor(session_id, &nonce)),
        )
    }

    /// The `SessionId` the connection token `token` was issued for, or `None` for any value
    /// this store did not mint as one: a token of another kind — a notebook address's
    /// authenticator included — one tampered with, and one another store minted all name no
    /// session.
    pub fn session_endpoint_auth_token_session(&self, token: &str) -> Option<String> {
        let cursor = self
            .session_endpoint_key
            .cursor_of(&::hex::decode(token).ok()?)?;
        let mut parts = cursor.split(CURSOR_SEPARATOR);
        let kind = parts.next()?;
        let session_id = parts.next()?;
        let nonce = parts.next()?;
        if parts.next().is_some() || kind != "sessionendpointauth" || nonce.is_empty() {
            return None;
        }
        Some(session_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint leads back to the base this emulator is served at and names the
    /// session under it, whatever form the base takes.
    #[test]
    fn the_endpoint_leads_back_to_this_emulator_and_names_the_session() {
        // (case, the base this emulator reported) — the served base, the same with a
        // trailing slash, and the base an emulator serving no socket reports.
        let rows = [
            ("the served base", "https://127.0.0.1:9297"),
            ("a base with a trailing slash", "https://127.0.0.1:9297/"),
            ("an unserved base", ::smithy_server_runtime::UNSERVED_URL_BASE),
        ];
        for (case, base) in rows {
            let url = session_endpoint_url(base, "session-1");
            assert!(
                url.starts_with(base.trim_end_matches('/')),
                "case {case:?}: expected an address under the base, got {url:?}"
            );
            assert!(
                url.ends_with("/sessions/session-1"),
                "case {case:?}: expected the session's own path, got {url:?}"
            );
        }
    }

    /// RULE: a connection token names the session it was issued for, each issue is a new
    /// token, and a value this store did not mint as one names no session.
    #[test]
    fn a_connection_token_names_the_session_it_was_issued_for() {
        let store = ComAmazonawsAthenaAmazonathenaState::default();
        // The rows differ along the form of the session id a token is issued over.
        for case in ["session-1", "ffffffff-ffff-ffff-ffff-ffffffffffff", "s"] {
            let first = store.session_endpoint_auth_token(case);
            let second = store.session_endpoint_auth_token(case);
            assert_ne!(
                first, second,
                "case {case:?}: expected each issue to mint its own token, got {first:?} twice"
            );
            for token in [&first, &second] {
                assert_eq!(
                    store.session_endpoint_auth_token_session(token).as_deref(),
                    Some(case),
                    "case {case:?}: expected the token to name the session it was issued \
                     for, got {token:?}"
                );
            }
        }

        // Values this store did not mint as a connection token name no session: a value
        // that is not hexadecimal, hexadecimal nothing minted, and a token minted for the
        // OTHER grant this store hands out over a session.
        let notebook_token = store.notebook_auth_token("session-1");
        for (case, token) in [
            ("a value that is not a token at all", "not-a-token"),
            ("hexadecimal this store did not mint", "0011223344556677"),
            ("a notebook address's authenticator", notebook_token.as_str()),
        ] {
            assert_eq!(
                store.session_endpoint_auth_token_session(token),
                None,
                "case {case:?}: expected {token:?} to name no session"
            );
        }
    }
}
