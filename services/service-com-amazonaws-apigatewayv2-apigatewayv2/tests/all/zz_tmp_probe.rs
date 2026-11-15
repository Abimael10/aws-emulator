// SPDX-License-Identifier: CC0-1.0

//! The `$connect` identity-source refusal read by a client that dialled the announced address
//! as `wss://` — TLS first, then the opening handshake over the encrypted stream.
//!
//! The rule is the one `websocket_connect_identity_source_socket_answer.rs` states and spans
//! with its table; this file adds the one axis that table cannot vary, because it is a
//! property of the TRANSPORT rather than of the request:
//!
//! > The api's ONE announced address serves both a client that negotiates TLS first and one
//! > that dials in the clear (the encryption is negotiated in band), so the refusal the
//! > `$connect` route's authorization owes a handshake — `401` `{"message":"Unauthorized"}`
//! > when an `IdentitySource` the authorizer declares is absent — reaches either client
//! > unchanged, and the handshake is answered rather than left hanging.
//!
//! Everything else is shared rather than restated: the deployed api and the raw-wire reader
//! come from that suite, and the TLS client from `websocket_tls_transport.rs`.
//!
//! (This file's name predates what it holds: the tooling this branch had could neither rename
//! nor delete a file once written.)

use crate::websocket_connect_identity_source_socket_answer::{
    answered_over, deploy, Answered, LET_THROUGH, LET_THROUGH_MESSAGE, UNAUTHORIZED,
    UNAUTHORIZED_MESSAGE,
};
use crate::websocket_tls_transport::upgrade_to_tls;

/// Dial the announced socket, negotiate TLS, then write the opening handshake and read what
/// the api answered it with.
async fn answered_over_tls(
    case: &str,
    authority: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> Answered {
    let stream = crate::websocket_transport_data_plane::dial_loopback(case, authority)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the announced socket {authority:?} must accept a connection, \
                 got {error}"
            )
        });
    let tls = upgrade_to_tls(stream).await;
    answered_over(case, tls, authority, query, headers).await
}

/// One row: what the handshake presents for the authorizer's one declared header source, and
/// the status and sentence the step owes it.
struct Row {
    case: &'static str,
    /// The raw query string the handshake carries.
    query: &'static str,
    /// The header fields the handshake carries beyond the RFC 6455 ones.
    headers: &'static [(&'static str, &'static str)],
    expected: (u16, &'static str),
}

#[tokio::test]
async fn a_wss_client_reads_the_same_identity_source_refusal() {
    // Both sides of the precondition, over the encrypted half of the one announced address:
    // the source absent, and the same authorizer with it presented.
    let rows: &[Row] = &[
        Row {
            case: "the declared header is absent from the handshake",
            query: "",
            headers: &[],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "the declared header carries only whitespace, which is not a non-empty value",
            query: "",
            headers: &[("Auth", "  ")],
            expected: (UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
        },
        Row {
            case: "the declared header is presented, so the authorizer's own function decides",
            query: "",
            headers: &[("Auth", "let-me-in")],
            expected: (LET_THROUGH, LET_THROUGH_MESSAGE),
        },
    ];

    let emulator = crate::api_lifecycle::emulator();
    for row in rows {
        let case = row.case;
        let (status, message) = row.expected;
        let deployed = deploy(&emulator, case, &["route.request.header.Auth"], &[]).await;
        let answered = answered_over_tls(case, &deployed.authority, row.query, row.headers).await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected the handshake answered {status} over TLS, got {} with \
             head {:?}",
            answered.status, answered.head
        );
        assert_eq!(
            answered.message().as_deref(),
            Some(message),
            "case {case:?}: expected the body to word the refusal {message:?}, got {:?}",
            answered.body
        );
        assert!(
            !answered
                .head
                .to_ascii_lowercase()
                .contains("sec-websocket-accept"),
            "case {case:?}: a refused handshake carries no accept field, got {:?}",
            answered.head
        );
    }
}
