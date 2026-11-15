// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a `ConnectionId` earns, driven over HTTP through the same
//! `MultiServiceRouter` the production emulator serves with.
//!
//! The rule these cases assert, one level above the recorded case (one token of 11 bytes
//! that carries no format marker):
//!
//! > A `ConnectionId` is not an arbitrary base64 token: API Gateway's connection identifier
//! > is a fixed-width encoded value whose decoding carries a constant FORMAT MARKER
//! > (`mgmt::state::CONNECTION_ID_FORMAT_MARKER` at
//! > `mgmt::state::CONNECTION_ID_FORMAT_OFFSET`), present in every id AWS hands out. So the
//! > three `@connections` operations read the VALUE first: one whose decoding that encoding
//! > could never have produced — the wrong length, outside the alphabet, unpadded, or 11
//! > bytes without the marker — is refused HTTP 400 `BadRequestException` with the message
//! > `Invalid connectionId: ` plus the `ConnectionId` the request sent, before the registry
//! > is consulted; one that COULD have been issued reaches the registry, and the registry
//! > not holding it is the modelled `GoneException` (HTTP 410, memberless, so no message at
//! > all) whether it was issued elsewhere, issued here and closed, or belongs to another
//! > api.
//!
//! WHY the split matters: a backend that fans a message out to its stored connection table
//! prunes the row it posted to when it is told 410, and keeps the row on a 400. Answering
//! 410 for a value that could never have been issued makes it delete a row AWS would have
//! kept, and the memberless `GoneException`
//! (`smithy_query(query="shape",shape="GoneException")` declares no members) carries no
//! sentence either, so nothing says what was wrong with the request. Answering 400 for an
//! id that COULD have been issued is the mirror fault: the prune a real deployment performs
//! would never fire here.
//!
//! The rows span what the rule generalises over: WHERE the decoding fails (length,
//! alphabet, padding, the format marker), the id's history (never registered here, open,
//! closed), and the operation (all three).
//!
//! The router, request builder and response readers live in `crate::support`.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use engine::InMemoryEngine;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::support::{
    call_with_headers, error_code, error_message, minted, rendered, register, ALL_A_MARKED_ID,
    HOST, MINTED_IDS, RECORDED_ID,
};

/// The refusal for a value API Gateway's encoding could not have produced: the status, the
/// shape and the fixed text of its message.
const INVALID_STATUS: u16 = 400;
const INVALID_TYPE: &str = "BadRequestException";
const INVALID_MESSAGE: &str = "Invalid connectionId: ";

/// The modelled refusal for an id of that encoding the registry does not hold
/// (`GoneException`, `@httpError(410)`, memberless).
const GONE_STATUS: u16 = 410;
const GONE_TYPE: &str = "GoneException";

/// What `crate::support::error_message` answers for a body with no `message` member, which
/// is every body of the memberless `GoneException`.
const NO_MESSAGE: &str = "<no message>";

/// The three operations, by the method each is reached with, and the payload each sends.
fn operations() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("GET", Vec::new()),
        ("POST", b"fan-out".to_vec()),
        ("DELETE", Vec::new()),
    ]
}

/// A registration instant in the past, so an instant the service writes later is strictly
/// greater.
fn connected_at() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// The modelled request path for `connection_id`.
fn path(connection_id: &str) -> String {
    format!("/@connections/{}", as_path_segment(connection_id))
}

/// `connection_id` as a path SEGMENT carries it: the base64 alphabet's `+` and `/` and the
/// padding `=` are percent-encoded, which is how a client sends a token holding them.
fn as_path_segment(connection_id: &str) -> String {
    connection_id
        .replace('%', "%25")
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}

/// A token of 11 bytes whose format marker is `marker`, as a `ConnectionId` travels: the
/// bytes carry `label` so each case addresses a value of its own.
fn token_marked(label: &str, marker: u8) -> String {
    use base64::Engine as _;

    let mut bytes = label.as_bytes().to_vec();
    bytes.resize(mgmt::state::CONNECTION_ID_BYTES, b'-');
    bytes[mgmt::state::CONNECTION_ID_FORMAT_OFFSET] = marker;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The values whose decoding API Gateway's connection-id encoding could never have
/// produced, by WHERE the decoding fails. The marker rows are the recorded case's class: 11
/// bytes of the right length, carrying some other value where the format marker belongs.
///
/// The marker values span the byte either side of the one API Gateway stamps, the zero byte
/// the recorded case carries, and the high bit; `0x00` is the recorded value (the all-`A`
/// token decodes to eleven zero bytes).
fn ids_that_could_not_have_been_issued() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = vec![
        ("a single character".to_string(), "x".to_string()),
        (
            "characters outside the base64 alphabet".to_string(),
            "cid-with-dashes".to_string(),
        ),
        (
            "an unpadded token".to_string(),
            "0123456789abcdef".to_string(),
        ),
        (
            "base64 of a shorter token".to_string(),
            "AAAAAAAAAAA=".to_string(),
        ),
        (
            "a 200-character token".to_string(),
            "A".repeat(200).to_string(),
        ),
        (
            "the all-A token, eleven zero bytes".to_string(),
            "AAAAAAAAAAAAAAA=".to_string(),
        ),
    ];
    for marker in [0x00u8, 0x01, 0x03, 0x04, 0x80, 0xff] {
        rows.push((
            format!("eleven bytes whose format byte is {marker:#04x}"),
            token_marked(&format!("m{marker}"), marker),
        ));
    }
    rows
}

/// Every value in the table is one the shape rule refuses, and every token the shared
/// catalog of MINTED ids holds is one it accepts — so the two tables the cases below are
/// asserted over cannot drift into each other.
#[tokio::test]
async fn the_tables_are_the_forms_they_claim_to_be() {
    for (case, connection_id) in ids_that_could_not_have_been_issued() {
        assert!(
            !mgmt::state::is_well_formed_connection_id(&connection_id),
            "case {case:?}: {connection_id:?} must NOT be a value API Gateway could issue"
        );
    }
    for (case, connection_id) in MINTED_IDS {
        assert!(
            mgmt::state::is_well_formed_connection_id(connection_id),
            "case {case:?}: {connection_id:?} must be a value API Gateway could issue"
        );
    }
}

#[tokio::test]
async fn a_value_that_could_not_have_been_issued_is_refused_as_an_invalid_connection_id() {
    for (id_form, connection_id) in ids_that_could_not_have_been_issued() {
        for (method, payload) in operations() {
            // One OTHER connection is open, so each refusal below is about the value that
            // was asked for and not about an empty registry.
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let open = minted("bystander");
            register(&engine, &open, "192.0.2.1", "bystander/1", connected_at());

            let case = format!("{method} on {id_form} ({connection_id})");
            let (status, headers, body) =
                call_with_headers(&engine, HOST, method, &path(&connection_id), payload).await;
            assert_eq!(
                status, INVALID_STATUS,
                "case {case:?}: expected HTTP {INVALID_STATUS}, got {status} {}",
                rendered(&body)
            );
            assert_eq!(
                error_code(&headers),
                INVALID_TYPE,
                "case {case:?}: expected {INVALID_TYPE}, got {}",
                error_code(&headers)
            );
            assert_eq!(
                error_message(&body),
                format!("{INVALID_MESSAGE}{connection_id}"),
                "case {case:?}: expected the message to name the value the request sent, got {}",
                rendered(&body)
            );

            // The refusal changed nothing: nothing was registered for the value, and the
            // open connection is untouched — which is what a backend's connection table
            // depends on.
            assert!(
                mgmt::state::connection(&*engine, &connection_id).is_none(),
                "case {case:?}: a refused call must not register the value it refused"
            );
            let bystander = mgmt::state::connection(&*engine, &open)
                .unwrap_or_else(|| panic!("case {case:?}: the open connection must still exist"));
            assert_eq!(
                bystander.delivered_count, 0,
                "case {case:?}: a refused call must deliver nothing, got {} deliveries",
                bystander.delivered_count
            );
        }
    }
}

/// A value API Gateway COULD have issued reaches the registry, so it is the modelled
/// `GoneException` when nothing here holds it — never the 400 that would tell a caller its
/// request was wrong, and never a prune a real deployment would not perform.
#[tokio::test]
async fn a_value_that_could_have_been_issued_is_gone_when_the_registry_does_not_hold_it() {
    // The rows span the recorded token, the marker-carrying form of the all-A id, and a
    // token this crate's own encoder built.
    let rows: [(&str, String); 3] = [
        (
            "the form the recorded calls use",
            RECORDED_ID.to_string(),
        ),
        (
            "an all-A token carrying the format marker",
            ALL_A_MARKED_ID.to_string(),
        ),
        ("a token this emulator's encoder built", minted("encoded-1")),
    ];

    for (id_form, connection_id) in rows {
        for (method, payload) in operations() {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let open = minted("bystander");
            register(&engine, &open, "192.0.2.1", "bystander/1", connected_at());

            let case = format!("{method} on {id_form} ({connection_id})");
            let (status, headers, body) =
                call_with_headers(&engine, HOST, method, &path(&connection_id), payload).await;
            assert_eq!(
                status, GONE_STATUS,
                "case {case:?}: expected HTTP {GONE_STATUS}, got {status} {}",
                rendered(&body)
            );
            assert_eq!(
                error_code(&headers),
                GONE_TYPE,
                "case {case:?}: expected {GONE_TYPE}, got {}",
                error_code(&headers)
            );
            assert_eq!(
                error_message(&body),
                NO_MESSAGE,
                "case {case:?}: GoneException is memberless, so it carries no message, got {}",
                rendered(&body)
            );
            assert!(
                mgmt::state::connection(&*engine, &connection_id).is_none(),
                "case {case:?}: a refused call must not register the id it refused"
            );
        }
    }
}

/// The encoding rule is what the REGISTRATION seam reads too, so the registry can only ever
/// hold ids the three operations address: a value the rule refuses cannot be registered,
/// and one it accepts is served while it is open and gone once it closes.
#[tokio::test]
async fn only_a_value_the_rule_accepts_can_be_registered() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let registry = mgmt::state::registry(&*engine).expect("the engine carries the registry");

    for (id_form, connection_id) in ids_that_could_not_have_been_issued() {
        assert!(
            !mgmt::state::register_connection(
                &*engine,
                connection_id.clone(),
                mgmt::state::Identity {
                    source_ip: "192.0.2.4".to_string(),
                    user_agent: Some("encoding/1".to_string()),
                },
                connected_at(),
            ),
            "case {id_form:?}: the seam must refuse to register {connection_id:?}, which no \
             caller could address"
        );
    }

    let addressable = minted("addressable");
    register(
        &engine,
        &addressable,
        "192.0.2.5",
        "encoding/2",
        connected_at(),
    );
    let (status, _headers, body) =
        call_with_headers(&engine, HOST, "GET", &path(&addressable), Vec::new()).await;
    assert_eq!(
        status, 200,
        "case \"a registered id\": GetConnection must answer 200, got {status} {}",
        rendered(&body)
    );

    assert!(
        mgmt::state::close_connection(&registry, &addressable),
        "the close seam must close the registered connection"
    );
    let (status, headers, body) =
        call_with_headers(&engine, HOST, "GET", &path(&addressable), Vec::new()).await;
    assert_eq!(
        status, GONE_STATUS,
        "case \"a closed id\": expected HTTP {GONE_STATUS}, got {status} {}",
        rendered(&body)
    );
    assert_eq!(
        error_code(&headers),
        GONE_TYPE,
        "case \"a closed id\": expected {GONE_TYPE}, got {}",
        error_code(&headers)
    );
}
