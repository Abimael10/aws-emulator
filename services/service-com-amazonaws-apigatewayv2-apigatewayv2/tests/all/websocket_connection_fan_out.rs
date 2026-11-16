// SPDX-License-Identifier: CC0-1.0

//! A backend's FAN-OUT over the `@connections` management API of one deployed WEBSOCKET
//! stage: several connection ids addressed in one pass, of which some are live, some name
//! nothing and some could never have been issued at all.
//!
//! The rule these cases assert, one level above the recorded case (one planted token of
//! eleven zero bytes and one live connection):
//!
//! > Every `@connections` call is decided by the `ConnectionId` it names and by the
//! > connection that id names, and by nothing else: a value API Gateway's encoding could
//! > never have produced is refused HTTP 400 `BadRequestException` with `Invalid
//! > connectionId: ` and the id the request sent, before the registry is consulted; a value
//! > of that encoding naming no connection the endpoint holds — issued elsewhere, or issued
//! > here and since closed — is the memberless `GoneException` (410); and a value naming a
//! > connection the endpoint holds is SERVED, the posted payload delivered over that
//! > connection's own socket. So a fan-out answers each id on its own: one id's refusal
//! > neither closes, forgets nor skips any other connection of the same stage, whatever order
//! > the ids are addressed in, and each live connection reads back exactly the payloads
//! > addressed to it, in the order they were posted.
//!
//! WHY the independence is the rule and not an accident: a backend that keeps its own table
//! of connections prunes a row on the 410 ("stop writing to this connection") and keeps it on
//! the 400 ("this request is wrong"), and it learns both from a single loop over that table.
//! A refusal that took another connection down with it, or that made the loop skip the
//! connections after it, would have the backend drop live clients it had just been told
//! about.
//!
//! The rows span what the rule generalises over: the FORM of the id (the shape this stage
//! issues, a token of that shape it never held, one it held and closed, and — refused by the
//! alphabet, by the decoded length and by the format marker — three it could not have
//! issued), the COUNT of live connections on the stage (two, addressed independently), and
//! the ORDER (a live id before every refusal, between them, and after all of them).
//!
//! The api, the handshake, the `@connections` caller and the frame reader are
//! `websocket_endpoint_connections`'s, which pins the answer of ONE call per id form; this
//! suite drives what a whole fan-out does to the connections it does not name.

use tokio_tungstenite::tungstenite::Message;

use crate::path_label::addressed;
use crate::websocket_endpoint_connections::{
    connections_call, deploy, emulator_with_connections, minted, next_frame, open_connection,
};
use crate::websocket_transport_data_plane::ClientSocket;

/// The statuses and `x-amzn-errortype` values the three outcomes of the rule carry.
const SERVED_STATUS: u16 = 200;
const DELETED_STATUS: u16 = 204;
const INVALID_STATUS: u16 = 400;
const INVALID_TYPE: &str = "BadRequestException";
const GONE_STATUS: u16 = 410;
const GONE_TYPE: &str = "GoneException";

/// The fixed text of the refusal for a value that is no connection identifier at all; the
/// rest of that sentence is the `ConnectionId` the request sent.
const INVALID_MESSAGE: &str = "Invalid connectionId: ";

/// What the answer to one addressed id must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The id names a connection this endpoint holds: served, and the payload delivered to
    /// that client.
    Served,
    /// The id could never have been issued: `400` `BadRequestException` naming it.
    Invalid,
    /// The id is of the issued shape and names no connection here: `410` `GoneException`.
    Gone,
}

/// One id the fan-out addresses.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// The id as the request SENDS it — percent-encoded where a path segment has to be.
    sent: String,
    /// The id the service reads back out of the path, which a refusal names verbatim.
    read: String,
    expected: Expected,
    /// Which live client must read this row's payload back, for a served row.
    client: Option<usize>,
    /// The `Data` this row posts.
    payload: String,
}

/// `ConnectionId` values API Gateway's encoding could never have produced: the form, the
/// value a request SENDS, and the value the service reads out of the path.
///
/// The source of the forms is the encoding rule itself — `is_well_formed_connection_id` of
/// the management API's `state` module (standard padded base64 of `CONNECTION_ID_BYTES`
/// bytes carrying the format marker at `CONNECTION_ID_FORMAT_OFFSET`) — and the rows fail it
/// by the alphabet, by the decoded length and by the marker in turn.
/// [`the_fan_out_catalog_is_the_forms_it_claims_to_be`] holds every row to that reader.
const MALFORMED_IDS: [(&str, &str, &str); 3] = [
    (
        "a single character, which that encoding could never have produced",
        "x",
        "x",
    ),
    (
        "base64 of eleven zero bytes, so no format marker",
        "AAAAAAAAAAAAAAA=",
        "AAAAAAAAAAAAAAA=",
    ),
    (
        "text that is not base64 at all, percent-encoded as a path segment carries it",
        "not%20base64%21%21",
        "not base64!!",
    ),
];

/// What a refusal row posts: a refusal is decided by the id, so the payload is the same for
/// every one of them.
const REFUSED_PAYLOAD: &str = r#"{"notify":"refused"}"#;

/// The next frame this client reads, as the text a delivered payload arrives as.
async fn next_delivery(case: &str, socket: &mut ClientSocket) -> String {
    match next_frame(case, socket).await {
        Message::Text(text) => text.as_str().to_string(),
        Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        other => panic!(
            "case {case:?}: expected the payload addressed to this connection, got {other:?}"
        ),
    }
}

/// A row addressing the live connection `connection_id`, whose client is `client`.
fn served(case: &'static str, connection_id: &str, client: usize, payload: &str) -> Row {
    Row {
        case,
        sent: addressed(connection_id),
        read: connection_id.to_string(),
        expected: Expected::Served,
        client: Some(client),
        payload: payload.to_string(),
    }
}

/// A row addressing one [`MALFORMED_IDS`] value.
fn refused(form: (&'static str, &'static str, &'static str)) -> Row {
    let (case, sent, read) = form;
    Row {
        case,
        sent: sent.to_string(),
        read: read.to_string(),
        expected: Expected::Invalid,
        client: None,
        payload: REFUSED_PAYLOAD.to_string(),
    }
}

/// A row addressing a well-formed id this endpoint holds no connection for.
fn gone(case: &'static str, connection_id: &str) -> Row {
    Row {
        case,
        sent: addressed(connection_id),
        read: connection_id.to_string(),
        expected: Expected::Gone,
        client: None,
        payload: REFUSED_PAYLOAD.to_string(),
    }
}

/// A fan-out over one stage: each id is answered on its own, and the live connections are
/// served before, between and after every refusal.
#[tokio::test]
async fn a_refusal_for_one_id_leaves_every_live_connection_of_the_stage_servable() {
    let case = "a fan-out over two live connections, a closed one and ids of other forms";
    let emulator = emulator_with_connections();
    let api = deploy(&emulator, case, "fan-out-api").await;

    // Three clients complete the handshake on the api's own socket; the third's connection is
    // then CLOSED through the management API, so its id is one this stage issued and no
    // longer holds.
    let (mut first, first_id) = open_connection(case, &api.authority).await;
    let (mut second, second_id) = open_connection(case, &api.authority).await;
    let (closed_socket, closed_id) = open_connection(case, &api.authority).await;
    // The id a handshake mints is addressable from the value alone: it is of the shape the
    // management API accepts, so no call below can be the invalid-id refusal for a reason the
    // test itself introduced.
    for connection_id in [&first_id, &second_id, &closed_id] {
        assert!(
            service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::state::
                is_well_formed_connection_id(connection_id),
            "case {case:?}: the id a handshake minted must be the shape the management API \
             accepts, got {connection_id:?}"
        );
    }
    for (left, right) in [
        (&first_id, &second_id),
        (&first_id, &closed_id),
        (&second_id, &closed_id),
    ] {
        assert_ne!(
            left, right,
            "case {case:?}: each handshake must mint its own ConnectionId, got {left:?} twice"
        );
    }
    let answer = connections_call(case, &api.authority, "DELETE", &addressed(&closed_id), b"").await;
    assert_eq!(
        answer.status, DELETED_STATUS,
        "case {case:?}: DeleteConnection must answer {DELETED_STATUS}, got {} {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    drop(closed_socket);

    let unknown_id = minted("never-held");
    let [single_character, no_format_marker, not_base64] = MALFORMED_IDS;
    let rows = vec![
        served(
            "the first live connection, before any refusal",
            &first_id,
            0,
            r#"{"notify":"first-1"}"#,
        ),
        refused(single_character),
        served(
            "the second live connection, after a refusal",
            &second_id,
            1,
            r#"{"notify":"second-1"}"#,
        ),
        refused(no_format_marker),
        gone(
            "the connection this stage issued and has since closed",
            &closed_id,
        ),
        refused(not_base64),
        gone(
            "an id of the issued shape this stage never held",
            &unknown_id,
        ),
        // The LAST payload each live connection is sent: it arrives only after every frame
        // posted before it, so a payload delivered to the wrong connection — or delivered
        // twice — is read in its place.
        served(
            "the first live connection, after every refusal",
            &first_id,
            0,
            r#"{"notify":"first-last"}"#,
        ),
        served(
            "the second live connection, after every refusal",
            &second_id,
            1,
            r#"{"notify":"second-last"}"#,
        ),
    ];

    // The fan-out itself: one pass over the table, each answer read before the next id is
    // addressed, exactly as a backend's loop over its own table runs.
    let mut owed: [Vec<String>; 2] = [Vec::new(), Vec::new()];
    for row in &rows {
        let case = row.case;
        let answer = connections_call(
            case,
            &api.authority,
            "POST",
            &row.sent,
            row.payload.as_bytes(),
        )
        .await;
        let body = String::from_utf8_lossy(&answer.body).to_string();
        match row.expected {
            Expected::Served => {
                assert_eq!(
                    answer.status, SERVED_STATUS,
                    "case {case:?}: a live connection must be posted to with {SERVED_STATUS}, \
                     got {} {body:?}",
                    answer.status
                );
                let frames = row
                    .client
                    .and_then(|client| owed.get_mut(client))
                    .unwrap_or_else(|| panic!("case {case:?}: a served row names its client"));
                frames.push(row.payload.clone());
            }
            Expected::Invalid => {
                assert_eq!(
                    answer.status, INVALID_STATUS,
                    "case {case:?}: a value that could never have been issued must be refused \
                     {INVALID_STATUS}, got {} {body:?}",
                    answer.status
                );
                assert_eq!(
                    answer.error_type, INVALID_TYPE,
                    "case {case:?}: expected x-amzn-errortype {INVALID_TYPE}, got {:?}",
                    answer.error_type
                );
                assert_eq!(
                    answer.message(),
                    format!("{INVALID_MESSAGE}{}", row.read),
                    "case {case:?}: the refusal must name the id the request sent, got {body:?}"
                );
            }
            Expected::Gone => {
                assert_eq!(
                    answer.status, GONE_STATUS,
                    "case {case:?}: an issued shape naming no connection here must answer \
                     {GONE_STATUS}, got {} {body:?}",
                    answer.status
                );
                assert_eq!(
                    answer.error_type, GONE_TYPE,
                    "case {case:?}: expected x-amzn-errortype {GONE_TYPE}, got {:?}",
                    answer.error_type
                );
                assert_eq!(
                    answer.message(),
                    "<no message>",
                    "case {case:?}: GoneException is memberless, so it carries no message, got \
                     {body:?}"
                );
            }
        }
    }

    // Each live client read back exactly the payloads addressed to IT, in the order they were
    // posted: nothing a refusal named reached it, and nothing addressed to the other
    // connection did either.
    let [first_owed, second_owed] = owed;
    for (label, socket, expected_frames) in [
        ("the first live connection", &mut first, first_owed),
        ("the second live connection", &mut second, second_owed),
    ] {
        assert!(
            !expected_frames.is_empty(),
            "case {label:?}: the table must post to this connection at all"
        );
        for expected in expected_frames {
            let delivered = next_delivery(label, socket).await;
            assert_eq!(
                delivered, expected,
                "case {label:?}: expected the payload {expected:?} this connection was posted, \
                 got {delivered:?}"
            );
        }
    }

    // And the registry is where the fan-out left it: both live connections are still
    // reported, and neither refused id names one.
    for (label, connection_id, expected_status) in [
        ("the first live connection", &first_id, SERVED_STATUS),
        ("the second live connection", &second_id, SERVED_STATUS),
        ("the connection that was closed", &closed_id, GONE_STATUS),
        ("the id never held here", &unknown_id, GONE_STATUS),
    ] {
        let answer =
            connections_call(label, &api.authority, "GET", &addressed(connection_id), b"").await;
        assert_eq!(
            answer.status, expected_status,
            "case {label:?}: after the fan-out GetConnection must answer {expected_status}, got \
             {} {:?}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
    }
}

/// Every id the table above calls malformed is one API Gateway's encoding could not have
/// produced, by the service's own reading of a `ConnectionId` — so a row cannot drift into
/// the well-formed class and quietly stop testing the refusal it names.
#[test]
fn the_fan_out_catalog_is_the_forms_it_claims_to_be() {
    use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi::state::is_well_formed_connection_id;

    for (form, _sent, read) in MALFORMED_IDS {
        assert!(
            !is_well_formed_connection_id(read),
            "case {form:?}: {read:?} must NOT be the shape API Gateway issues"
        );
    }
}
