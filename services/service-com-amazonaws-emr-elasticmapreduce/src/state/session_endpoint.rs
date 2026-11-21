// SPDX-License-Identifier: CC0-1.0

//! How a client REACHES one Spark Connect session: the endpoint it dials and the
//! time-limited token it presents, one concern of this service's state.
//!
//! `GetSessionEndpoint` "returns the Spark Connect endpoint URL and a time-limited
//! authentication token for the specified session. Use the endpoint and token to connect a
//! PySpark client to the session. Call this operation again when the token expires to
//! obtain a new one." (its Smithy doc.) Three rules follow from that sentence, and this
//! module states each of them once:
//!
//! * a client connects to a session that is UP, so a session that has not reached a ready
//!   state ([`super::is_ready_session_state`]) has no endpoint to connect to and the call
//!   is refused for the state the session is actually in
//!   ([`session_not_ready_for_endpoint_message`]);
//! * the endpoint is an address the caller then DIALS, so it leads back to this emulator
//!   ([`session_endpoint_url`]) rather than to a host AWS publishes its own endpoints at;
//! * the token is a SECRET handed to that caller, minted per call and good for a bounded
//!   span ([`mint_session_auth_token`], [`SESSION_AUTH_TOKEN_VALIDITY`]), which is what
//!   "call this operation again when the token expires" asks of a client.
//!
//! What this module does NOT hold is the session's data plane: no Spark Connect server is
//! run for a session (see the session surface's own module docs), so the credentials
//! `Credentials` carries — "used to authenticate with the Spark Connect server when
//! connecting directly over VPC peering", the pair that goes with a session's
//! `Session$ServerUrl` — belong to a connection no session here offers and are reported by
//! none.

use super::*;

/// How long the authentication token one call issues is good for.
///
/// CHOSEN, not an AWS duration: AWS documents only that `AuthTokenExpirationTime` is "the
/// time at which the authentication token expires. After this time, call
/// GetSessionEndpoint again to obtain a new token"
/// (`GetSessionEndpointOutput$AuthTokenExpirationTime` doc) and names no span, so this is a
/// bounded token lifetime of this emulator's own — the same hour a cluster's session
/// credentials are good for ([`SESSION_CREDENTIALS_LIFETIME`]), long enough for an
/// interactive session and short enough to be time-limited.
pub const SESSION_AUTH_TOKEN_VALIDITY: Duration = SESSION_CREDENTIALS_LIFETIME;

/// How many characters of the store's secret the authentication token keeps.
///
/// CHOSEN: 64 characters, the same length the browser token of a presigned application-UI
/// URL keeps ([`PRESIGNED_URL_TOKEN_LEN`]), which carries far more entropy than one
/// session connection needs.
const SESSION_AUTH_TOKEN_LEN: usize = PRESIGNED_URL_TOKEN_LEN;

/// The longest endpoint this operation reports: the bound the member declares
/// (`GetSessionEndpointOutput$Endpoint` targets `XmlString`, len 0..10280).
const SESSION_ENDPOINT_MAX_CHARS: usize = 10_280;

/// The path segment naming a cluster in a session's endpoint, as this service names the
/// resource a session belongs to.
const CLUSTERS_PATH_SEGMENT: &str = "clusters";

/// The path segment naming the session itself.
const SESSIONS_PATH_SEGMENT: &str = "sessions";

/// How `GetSessionEndpoint` refuses a session that has not reached a ready state.
///
/// Recorded behavior for the text: fixed apart from two variable parts — the session the
/// request named and the `SessionState` that session is in at the call, read off this
/// emulator's own record rather than off the request.
pub fn session_not_ready_for_endpoint_message(session_id: &str, state: &str) -> String {
    format!("GetSessionEndpoint is not supported because Session {session_id} is in {state} state.")
}

/// The refusal `GetSessionEndpoint` answers a session that is not ready with: that sentence
/// under the client error the operation models.
///
/// No `ErrorCode`: Amazon EMR publishes no discriminator for this class, the way the rest
/// of the session surface's state refusals carry none
/// ([`sessions_not_enabled`]).
pub fn session_not_ready_for_endpoint(session_id: &str, state: &str) -> InvalidRequestException {
    InvalidRequestException {
        error_code: None,
        message: Some(session_not_ready_for_endpoint_message(session_id, state)),
    }
}

/// The endpoint a client dials one session at: the address THIS emulator is reached at,
/// with the path naming the cluster and the session the client is connecting to.
///
/// A client reads this value and then CONNECTS to it, so it has to lead back to this
/// emulator: `served_base` is [`::smithy_server_runtime::OperationContext::endpoint`], "the
/// base URL a later request addresses", and nothing of the value is derived from a host AWS
/// publishes its own endpoints at — an address under one of those resolves nowhere near
/// this process or, worse, to genuine AWS infrastructure, which takes the client out of the
/// emulated environment carrying the credentials it is holding.
///
/// The cluster/session path is kept, so a client that reads the value for the session it
/// names still finds it there: every call about one session reports the same endpoint, and
/// two sessions never share one.
///
/// What answers AT that address is this emulator's own API: it serves this service's
/// control plane and runs no Spark Connect server behind the path, so a client that dials
/// the endpoint reaches this emulator rather than a Spark Connect server. The value's
/// authority is this process's, which is what the member is for — an endpoint naming a host
/// nothing here answers on leads the client out of the emulated environment altogether.
pub fn session_endpoint_url(served_base: &str, cluster_id: &str, session_id: &str) -> String {
    let base = served_base.trim_end_matches('/');
    // The session's whole path, and the shortest form that still names the session, for a
    // base long enough that the first does not fit inside the member's bound: the value
    // stays inside the modelled length while two sessions keep two endpoints.
    for path in [
        format!("/{CLUSTERS_PATH_SEGMENT}/{cluster_id}/{SESSIONS_PATH_SEGMENT}/{session_id}"),
        format!("/{SESSIONS_PATH_SEGMENT}/{session_id}"),
    ] {
        let url = format!("{base}{path}");
        if url.chars().count() <= SESSION_ENDPOINT_MAX_CHARS {
            return url;
        }
    }
    // A base so long that not even that fits is one no emulator of this process reports;
    // the base alone still leads back here rather than past the modelled length.
    base.to_string()
}

/// A fresh authentication token for connecting a client to one session's endpoint, or
/// `None` when the store minted no secret.
///
/// The token authenticates to the SESSION endpoint, not to this emulator, so it is minted
/// and then removed again the way every other non-SigV4 secret this crate vends is
/// ([`mint_store_secret`]): the caller gets the secret, the emulator keeps no credential it
/// would accept, and nothing accumulates per call. Nothing of the request, of the session
/// or of the clock enters it, so one call's token is never predictable from another's.
pub fn mint_session_auth_token(store: &::emulator_server::CredentialStore) -> Option<String> {
    mint_store_secret(store, SESSION_AUTH_TOKEN_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the endpoint leads back to the base this emulator is served at, names the
    /// cluster and the session under it, and stays inside the member's modelled length
    /// whatever the base is.
    #[test]
    fn the_endpoint_leads_back_to_this_emulator_and_names_the_session() {
        let rows: [(&str, &str); 4] = [
            ("the served base", "https://127.0.0.1:9297"),
            ("a base with a trailing slash", "https://127.0.0.1:9297/"),
            ("an unserved base", "https://emulator.invalid"),
            // A base at the member's bound leaves room for no path at all.
            ("a base that leaves no room for a path", "https://"),
        ];
        for (case, base) in rows {
            let base = if case == "a base that leaves no room for a path" {
                format!("{base}{}", "a".repeat(SESSION_ENDPOINT_MAX_CHARS - base.len()))
            } else {
                base.to_string()
            };
            let url = session_endpoint_url(&base, "j-1", "is-1");
            assert!(
                url.chars().count() <= SESSION_ENDPOINT_MAX_CHARS,
                "case {case:?}: expected at most {SESSION_ENDPOINT_MAX_CHARS} characters, \
                 got {} characters",
                url.chars().count()
            );
            assert!(
                url.starts_with(base.trim_end_matches('/')),
                "case {case:?}: expected an address under the base, got {url:?}"
            );
            if case != "a base that leaves no room for a path" {
                assert!(
                    url.ends_with("/clusters/j-1/sessions/is-1"),
                    "case {case:?}: expected the session's own path, got {url:?}"
                );
            }
        }
    }
}
