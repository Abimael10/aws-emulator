// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal `RunStatement` answers when the `SessionId` it was
//! addressed by resolves to no session at all.
//!
//! RULE: a Glue operation refuses a miss with the sentence THAT operation is recorded
//! answering, so a surface whose sentence is the service's fixed, generic entity miss
//! answers that constant text and nothing else — no part of the request appears in it, and
//! the differently-worded sentences of the operations that resolve a session their own way
//! are not borrowed for it.
//!
//! The statement RUN is one of those surfaces. Its rows below differ along what the rule
//! generalises over: the id's value, length, casing and character set (each published bound
//! of `SessionId` included), whether the id never existed or named a session that was
//! created, driven to `READY` and then deleted (the record fully removed), and what else the
//! request carried — so a template creeping back into the sentence, or a sentence that
//! varies with the request, fails here. The sibling operations are asserted on the SAME
//! absent id, each keeping its own wording.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.

use std::sync::Arc;

use serde_json::json;

use crate::not_found_wording::assert_fixed_miss;
use crate::statements::ready_session;
use crate::wire::{call, emulator_on, error_message, ok};

/// The whole sentence `RunStatement` refuses an unresolvable `SessionId` with: the
/// service's fixed, generic entity miss, written out as a literal so this suite pins the
/// wording rather than restating the service's own helper.
const RUN_STATEMENT_MISS: &str = "Entity not found";

/// Ids that span what this sentence generalises over, every one of them inside
/// `NameString` ("len 1..255" — `smithy_query operation=RunStatement`): the floor of that
/// length, a plain name, mixed case, the punctuation a session id is minted with, an id
/// shaped like a minted one, and the 255 ceiling.
fn absent_session_ids() -> Vec<(String, String)> {
    vec![
        ("at the 1 length floor".to_string(), "s".to_string()),
        (
            "an id nothing created".to_string(),
            "no-such-session".to_string(),
        ),
        ("mixed case".to_string(), "HeldSession".to_string()),
        (
            "dots, underscores and digits".to_string(),
            "team.data_lake-2026.session.42".to_string(),
        ),
        (
            "shaped like a minted session id".to_string(),
            "11111111-2222-3333-4444-555555555555".to_string(),
        ),
        ("at the 255 ceiling".to_string(), "s".repeat(255)),
    ]
}

/// `RunStatement` on an id this Region holds no session under answers the fixed sentence,
/// whatever the id is and whatever else the request carries.
#[tokio::test]
async fn the_run_refuses_an_unresolvable_session_with_the_fixed_entity_miss() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());
    // A session that DOES exist, so every refusal below is about the id it asked for.
    ready_session(&router, &engine, "held").await;

    let mut cases = absent_session_ids();
    // An id matched exactly as sent: one differing from the stored session only in case is
    // a miss too.
    cases.push((
        "differs only in case from a stored session".to_string(),
        "HELD".to_string(),
    ));
    cases.push((
        "a prefix of a stored id".to_string(),
        "hel".to_string(),
    ));

    for (case, id) in &cases {
        // The code and the origin the run carries are no part of the address it failed to
        // resolve, so the sentence is the same for each of them: the modelled length floor
        // of `Code` ("len 0..68000"), a statement, and a run that names its origin.
        for (shape, request) in [
            ("code at the length floor", json!({ "SessionId": id, "Code": "" })),
            (
                "a statement",
                json!({ "SessionId": id, "Code": "spark.sql('select 1')" }),
            ),
            (
                "an origin named",
                json!({
                    "SessionId": id,
                    "Code": "print(1)",
                    "RequestOrigin": "test-origin",
                }),
            ),
        ] {
            let label = format!("{case} ({shape})");
            assert_fixed_miss(
                &router,
                &label,
                "RunStatement",
                &request,
                id,
                RUN_STATEMENT_MISS,
            )
            .await;
        }
    }

    // The session that exists still takes a run, so the refusals above are about the id.
    let answer = ok(
        &router,
        "RunStatement",
        &json!({ "SessionId": "held", "Code": "print('held')" }),
    )
    .await;
    assert!(
        answer.get("Id").is_some(),
        "a READY session must still take a run, got {answer}"
    );
}

/// A session created, driven to `READY` and then deleted — its record fully removed — is
/// the same miss as an id nothing ever created: the run answers the fixed sentence, which
/// still carries no part of the request.
#[tokio::test]
async fn a_deleted_session_is_the_same_fixed_miss_as_one_never_created() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(engine.clone());

    for (case, id) in absent_session_ids() {
        ready_session(&router, &engine, &id).await;
        // The session is there and serving before the delete.
        let before = ok(&router, "GetSession", &json!({ "Id": &id })).await;
        assert_eq!(
            before["Session"]["Id"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the created session must be readable before the delete, got {before}"
        );

        ok(&router, "DeleteSession", &json!({ "Id": &id })).await;
        // The row is gone: the read of it is refused.
        let (status, read) = call(&router, "GetSession", &json!({ "Id": &id })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the deleted session must no longer be readable, got {status} with \
             {read}"
        );

        assert_fixed_miss(
            &router,
            &format!("{case} (deleted)"),
            "RunStatement",
            &json!({ "SessionId": &id, "Code": "1" }),
            &id,
            RUN_STATEMENT_MISS,
        )
        .await;
    }
}

/// The operations that resolve a session another way keep their own sentences for the SAME
/// absent id: the session READ names the session, the STATEMENT reads name the member that
/// resolved to nothing, and only the RUN answers the fixed, generic sentence.
#[tokio::test]
async fn the_sibling_session_operations_keep_their_own_wording() {
    let router = emulator_on(Arc::new(engine::InMemoryEngine::new()));

    for (case, id) in absent_session_ids() {
        for (operation, request, expected) in [
            (
                "GetSession",
                json!({ "Id": &id }),
                format!("Session {id} not found."),
            ),
            (
                "ListStatements",
                json!({ "SessionId": &id }),
                format!("Session ID {id} not found"),
            ),
            (
                "GetStatement",
                json!({ "SessionId": &id, "Id": 1 }),
                format!("Session ID {id} not found"),
            ),
        ] {
            let (status, body) = call(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: {operation} on an absent session must be refused, got {status} \
                 with {body}"
            );
            assert_eq!(
                error_message(&body),
                expected,
                "case {case:?}: {operation} must keep its own sentence {expected:?}, got {body}"
            );
            assert_ne!(
                error_message(&body),
                RUN_STATEMENT_MISS,
                "case {case:?}: {operation} must not borrow the run's fixed sentence, got {body}"
            );
        }
    }
}
