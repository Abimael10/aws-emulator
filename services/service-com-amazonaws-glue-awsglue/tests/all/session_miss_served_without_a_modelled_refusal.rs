// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the id that names no session on the Glue session operations whose
//! modelled error set carries NO entity-miss failure.
//!
//! RULE: an operation whose modelled error set carries no entity-miss failure does not refuse
//! a well-formed request whose identifier resolves to no resource — it serves its modelled
//! success response, with the identifier the request sent echoed back, and writes nothing. The
//! operations the rule covers here are `StopSession` and `DeleteSession`, whose modelled error
//! sets are `AccessDeniedException`, `ConcurrentModificationException`,
//! `IllegalSessionStateException`, `InternalServiceException`, `InvalidInputException` and
//! `OperationTimeoutException` (`smithy_query operation=StopSession,DeleteSession`): no member
//! of either list reports a missing entity, and each models its answer as `{ Id }`. An
//! operation that DOES model the miss keeps refusing with it, which is the boundary of the
//! rule: `GetSession` declares `EntityNotFoundException`
//! (`smithy_query operation=GetSession`), so the same id is refused there.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rows differ along what the rule generalises over: which of the two operations is
//! called, how the requested id misses (never created, deleted, a case or substring variant of
//! a stored id, a stored id reached through another Region's endpoint), the form and the length
//! of the id including both published bounds of `NameString`, and how many times the call is
//! repeated. Nothing here sleeps: a session is brought to `READY` by positioning the instant
//! its provisioning is owed at.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::statements::{ready_session, stored_session};
use crate::wire::{call, call_in_region, emulator_on, error_code, ok, REGION};

/// The two session operations whose modelled error set carries no entity-miss failure, each
/// answering `{ Id }`.
const SERVED_ON_A_MISS: [&str; 2] = ["StopSession", "DeleteSession"];

/// The bounds of `Id`: `NameString`, "len 1..255"
/// (`smithy_query operation=StopSession,DeleteSession`).
const MIN_ID_LENGTH: usize = 1;
const MAX_ID_LENGTH: usize = 255;

/// A Region other than the one a session here is created in, so the same id reaches an
/// endpoint that holds nothing under it.
const OTHER_REGION: &str = "eu-west-1";

/// An emulator and the engine behind it, so a case can read the store a call left behind.
fn harness() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    (router, engine)
}

/// An id that names no session, for every shape of miss the rule generalises over.
fn missing_ids(stored: &str) -> Vec<(String, String)> {
    let shortest = "x".repeat(MIN_ID_LENGTH);
    let longest = "i".repeat(MAX_ID_LENGTH);
    vec![
        ("never created".to_string(), "no-such-session".to_string()),
        (
            "a prefix of a stored id".to_string(),
            stored[..stored.len() / 2].to_string(),
        ),
        (
            "a stored id lowercased".to_string(),
            stored.to_lowercase(),
        ),
        (
            "a stored id uppercased".to_string(),
            stored.to_uppercase(),
        ),
        (
            "an id carrying dots, dashes and underscores".to_string(),
            "a.b-c_d.0".to_string(),
        ),
        (
            "an id carrying spaces, which the modelled pattern admits".to_string(),
            "a session that was never created".to_string(),
        ),
        (
            "an id at the modelled length floor".to_string(),
            shortest,
        ),
        (
            "an id at the modelled length ceiling".to_string(),
            longest,
        ),
    ]
}

/// Every operation the rule covers serves an id that names no session: 200 with the requested
/// `Id` echoed back, and no session written under it.
#[tokio::test]
async fn an_id_naming_no_session_is_served_with_the_requested_id() {
    let (router, engine) = harness();
    let stored = "StoredSession";
    ready_session(&router, &engine, stored).await;

    for operation in SERVED_ON_A_MISS {
        for (miss, id) in missing_ids(stored) {
            let case = format!("{operation} on {miss}");
            let (code, body) = call(&router, operation, &json!({ "Id": id })).await;
            assert_eq!(
                code, 200,
                "case {case}: expected 200 for {id:?}, got {code} with {body}"
            );
            assert_eq!(
                body.get("Id").and_then(Value::as_str),
                Some(id.as_str()),
                "case {case}: expected the requested Id echoed, got {body}"
            );
            assert!(
                !stored_session(&engine, REGION, &id),
                "case {case}: expected no session written under {id:?}, got one"
            );
        }
    }

    // The stored id itself is acted on, so the ids above really held no session and the lookup
    // is not simply answering everything.
    let read = ok(&router, "GetSession", &json!({ "Id": stored })).await;
    assert_eq!(
        read["Session"]["Id"].as_str(),
        Some(stored),
        "case the stored session: expected it untouched by the misses, got {read}"
    );
}

/// Repeating the call on an id that names no session answers the same way every time: the rule
/// makes the operations idempotent, so a client retrying a timed-out teardown is served.
#[tokio::test]
async fn repeating_a_call_on_a_missing_session_is_served_every_time() {
    let (router, _engine) = harness();
    // Three attempts: the first, the one that proves the first wrote nothing, and one more.
    const ATTEMPTS: usize = 3;

    for operation in SERVED_ON_A_MISS {
        for attempt in 1..=ATTEMPTS {
            let case = format!("{operation} attempt {attempt}");
            let id = format!("retried-{operation}");
            let (code, body) = call(&router, operation, &json!({ "Id": id })).await;
            assert_eq!(
                code, 200,
                "case {case}: expected 200, got {code} with {body}"
            );
            assert_eq!(
                body.get("Id").and_then(Value::as_str),
                Some(id.as_str()),
                "case {case}: expected the requested Id echoed, got {body}"
            );
        }
    }
}

/// A session is addressed by its id and the Region it was created in together, so a stored id
/// reached through another Region's endpoint names no session there: the call is served, and
/// the record stays where it was created.
#[tokio::test]
async fn a_stored_id_in_another_region_names_no_session() {
    let (router, engine) = harness();
    let stored = "RegionScopedSession";
    ready_session(&router, &engine, stored).await;

    for operation in SERVED_ON_A_MISS {
        let case = format!("{operation} through another Region's endpoint");
        let (code, body) =
            call_in_region(&router, OTHER_REGION, operation, &json!({ "Id": stored })).await;
        assert_eq!(
            code, 200,
            "case {case}: expected 200, got {code} with {body}"
        );
        assert_eq!(
            body.get("Id").and_then(Value::as_str),
            Some(stored),
            "case {case}: expected the requested Id echoed, got {body}"
        );
        assert!(
            !stored_session(&engine, OTHER_REGION, stored),
            "case {case}: expected no session written in {OTHER_REGION}, got one"
        );
        assert!(
            stored_session(&engine, REGION, stored),
            "case {case}: expected the session kept where it was created, and it is gone"
        );
    }
}

/// The boundary of the rule: an operation that DOES model the entity miss still refuses an id
/// naming no session with it, so the rule is read off the modelled error set and not applied to
/// every session lookup.
#[tokio::test]
async fn an_operation_that_models_the_miss_still_refuses_it() {
    let (router, _engine) = harness();

    for (miss, id) in missing_ids("StoredSession") {
        let case = format!("GetSession on {miss}");
        let (code, body) = call(&router, "GetSession", &json!({ "Id": id })).await;
        assert_eq!(
            code, 400,
            "case {case}: expected 400 for {id:?}, got {code} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
    }
}
