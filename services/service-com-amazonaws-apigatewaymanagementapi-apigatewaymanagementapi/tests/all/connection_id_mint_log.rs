// SPDX-License-Identifier: CC0-1.0

//! Whether THIS process issued a `ConnectionId` has no part in the answer a `@connections`
//! call gets, driven over HTTP through the same `MultiServiceRouter` the production emulator
//! serves with.
//!
//! The rule these cases assert:
//!
//! > The `@connections` refusal split is a property of the VALUE the request sent, not of the
//! > server's own record of what it handed out: a `ConnectionId` of the shape API Gateway
//! > issues (`mgmt::state::is_well_formed_connection_id`) always reaches the connection
//! > registry, and the registry not holding it is the modelled `GoneException` (HTTP 410,
//! > memberless, so no message at all) — including on a registry that has never held a
//! > connection at all, and for an id some OTHER registry holds; a value that encoding could
//! > never have produced is refused HTTP 400 `BadRequestException`, message
//! > `Invalid connectionId: ` plus the `ConnectionId` the request sent, on exactly the same
//! > registry.
//!
//! WHY this axis has a suite of its own: a backend keeps its connection table outside the
//! service (in a database, across processes and across restarts), so the ids it posts to are
//! routinely ones the server it reaches never minted itself — one a sibling process issued,
//! one held over a restart, one whose socket closed before this server existed. 410 is the
//! signal that prunes such a row and 400 the signal that keeps it, so deciding between them
//! from a per-process mint log would make the prune branch fire in production and never here,
//! and would answer 400 for every id on a registry that has just come up — the state a
//! freshly deployed stage is in, where a caller polling for the store to answer at all would
//! then wait forever.
//!
//! The rows span what the rule generalises over: WHOSE registry holds the id (none at all,
//! another process's, this one's), the FORM of the id (every token of the minted catalog, and
//! every value of the not-minted catalog), the operation (all three), and the number of times
//! the call is repeated — a poll must keep getting the same answer, since a report may not
//! change what a later caller sees.
//!
//! The router, request builder and response readers live in `crate::support`.

use std::sync::Arc;

use engine::InMemoryEngine;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

// The refusal shapes, the operation table, the registration instant and the path spelling all
// come from the crate's shared harness, so this suite cannot drift from its siblings about
// what either refusal looks like or how a token travels in a path.
use crate::support::{
    call_with_headers, connected_at, error_code, error_message, ids_api_gateway_does_not_mint,
    minted, operations, path, register, rendered, GONE_STATUS, GONE_TYPE, HOST, INVALID_MESSAGE,
    INVALID_STATUS, INVALID_TYPE, MINTED_IDS, NO_MESSAGE,
};

/// How many times a case repeats the identical call: more than one, so an answer that only
/// held on the first call fails here.
const REPEATS: usize = 3;

/// Assert that `method` on `connection_id` against `engine` is the modelled `GoneException`,
/// and that it registered nothing.
async fn assert_gone(
    engine: &Arc<InMemoryEngine>,
    case: &str,
    method: &str,
    payload: Vec<u8>,
    connection_id: &str,
) {
    let (status, headers, body) =
        call_with_headers(engine, HOST, method, &path(connection_id), payload).await;
    assert_eq!(
        status, GONE_STATUS,
        "case {case:?}: an id of the issued shape that no registry here holds must answer \
         HTTP {GONE_STATUS}, got {status} {}",
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
        mgmt::state::connection(&**engine, connection_id).is_none(),
        "case {case:?}: a refused call must not register the id it refused"
    );
}

/// A registry that has NEVER held a connection — the state a stage that has just been
/// deployed is in — answers the modelled `GoneException` for every id of the issued shape,
/// and keeps answering it however often it is asked.
#[tokio::test]
async fn an_id_no_registry_ever_held_is_gone_rather_than_invalid() {
    for (id_form, connection_id) in MINTED_IDS {
        for (method, payload) in operations() {
            // Nothing is registered here, ever: no connection was opened and no id was
            // minted in this process, so the only thing left to decide the answer with is
            // the value the request sent.
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            assert!(
                mgmt::state::registry(&*engine)
                    .expect("the engine carries the registry")
                    .lock()
                    .connections
                    .is_empty(),
                "case {id_form:?}: this case starts from a registry holding nothing"
            );

            for repeat in 1..=REPEATS {
                let case = format!("{method} on {id_form} ({connection_id}), call {repeat}");
                assert_gone(&engine, &case, method, payload.clone(), connection_id).await;
            }
        }
    }
}

/// An id another registry holds — a sibling process's connection, or this process's own from
/// before a restart — is the modelled `GoneException` here, while the registry that does hold
/// it still serves it. The two answers differ only in WHICH registry was asked, which is what
/// makes the id's provenance irrelevant to the refusal split.
#[tokio::test]
async fn an_id_another_registry_holds_is_gone_here_and_served_there() {
    for (id_form, connection_id) in MINTED_IDS {
        // The process that minted the id and is carrying the connection.
        let elsewhere: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        register(
            &elsewhere,
            connection_id,
            "198.51.100.9",
            "sibling/1",
            connected_at(),
        );

        // This process, which minted nothing and holds an unrelated connection of its own.
        let here: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let bystander = minted("bystander");
        register(
            &here,
            &bystander,
            "192.0.2.1",
            "bystander/1",
            connected_at(),
        );

        for (method, payload) in operations() {
            let case = format!("{method} on {id_form} ({connection_id}) minted elsewhere");
            assert_gone(&here, &case, method, payload, connection_id).await;

            let bystander_record = mgmt::state::connection(&*here, &bystander)
                .unwrap_or_else(|| panic!("case {case:?}: the open connection must still exist"));
            assert_eq!(
                bystander_record.delivered_count, 0,
                "case {case:?}: a refused call must deliver nothing, got {} deliveries",
                bystander_record.delivered_count
            );
        }

        // The registry that holds it still serves it, so the id itself was addressable all
        // along.
        let case = format!("{id_form} ({connection_id}) at the registry that holds it");
        let (status, _headers, body) =
            call_with_headers(&elsewhere, HOST, "GET", &path(connection_id), Vec::new()).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetConnection must answer 200 where the connection is open, got \
             {status} {}",
            rendered(&body)
        );
    }
}

/// On that same never-used registry the 400 is still given — to every value the encoding
/// could not have produced, and to nothing else. So an empty registry does not make every
/// call `GoneException` any more than it makes every call `BadRequestException`: the form of
/// the value is what decides, on an empty registry exactly as on a populated one.
#[tokio::test]
async fn the_split_on_a_never_used_registry_is_decided_by_the_form_alone() {
    for (id_form, connection_id) in ids_api_gateway_does_not_mint() {
        for (method, payload) in operations() {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());

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
            assert!(
                mgmt::state::connection(&*engine, &connection_id).is_none(),
                "case {case:?}: a refused call must not register the value it refused"
            );
        }
    }
}
