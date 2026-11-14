// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the refusals of its INVOCATION slice, asserted end-to-end over
//! the wire.
//!
//! Every case drives a real `restJson1` request through the `MultiServiceRouter` the
//! production emulator serves with (the harness in [`crate::sessions`]), so what is
//! pinned here is the status, the error code and the `message` a Bedrock Agent Runtime
//! client reads — not a handler's return value.
//!
//! THE RULE UNDER TEST: a refusal of a request addressed to an invocation or to an
//! invocation step is worded from the REQUEST alone — one fixed template per rule, with
//! the identifiers the caller submitted as its only variable parts. It names neither the
//! session the lookup ran under (state the request did not carry) nor which scope of a
//! nested lookup missed, so a client matches one rule per refusal and cannot read a
//! refusal as an oracle for which ids exist under which parent. The three rules:
//!
//! 1. an `invocationIdentifier` naming no invocation: `Invocation with ID <id> not
//!    found`, identically for every operation that resolves one;
//! 2. a `CreateInvocation` whose `invocationId` is already held: `Invocation with ID <id>
//!    already exists`;
//! 3. a `GetInvocationStep` whose `(invocationIdentifier, invocationStepId)` pair
//!    resolves to no stored step: `Combination of Invocation with ID <id> and Invcoation
//!    Step with ID <step> not found` — the SAME sentence whichever half of the pair
//!    missed.

use serde_json::{json, Value};

use crate::sessions::{
    call, create_invocation, create_session, date_time, error_code, label, put_step, router,
    ACCOUNT, REGION,
};

/// The status and error code of each refusal class, from the model:
/// `smithy_query(query="operation",operation="PutInvocationStep,CreateInvocation,GetInvocationStep")`
/// -> `ResourceNotFoundException` httpError 404, `ConflictException` 409.
const NOT_FOUND: (u16, &str) = (404, "ResourceNotFoundException");
const CONFLICT: (u16, &str) = (409, "ConflictException");

/// UUIDs no test ever creates, for the rows that must name something absent.
const ABSENT: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";
const ABSENT_TOO: &str = "1a400015-c2b4-45b9-996e-1953e0e39268";
const ABSENT_THIRD: &str = "7c1d9a62-0f3b-4a58-9d2e-6b8c5f10a344";

/// A step time every row can send, as epoch seconds.
const STEP_TIME: i64 = 1_783_501_811;

/// RULE 1: the template every operation that resolves an `invocationIdentifier` states
/// for one naming no invocation. Built from the request, with no trailing period.
fn invocation_not_found(invocation_id: &str) -> String {
    format!("Invocation with ID {invocation_id} not found")
}

/// RULE 2: the template `CreateInvocation` states for an `invocationId` the session
/// already holds.
fn invocation_already_exists(invocation_id: &str) -> String {
    format!("Invocation with ID {invocation_id} already exists")
}

/// RULE 3: the ONE template `GetInvocationStep` states for a pair that resolves to no
/// stored step, whichever half of the pair missed.
fn step_combination_not_found(invocation_id: &str, step_id: &str) -> String {
    format!(
        "Combination of Invocation with ID {invocation_id} and Invcoation Step with ID \
         {step_id} not found"
    )
}

/// The `message` member of an error body, or `""` when there is none.
fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or("")
}

/// Assert that `stated` leaks neither the session id nor any segment of its ARN: the
/// refusal is worded from the request, and the session was resolved out of the store.
fn assert_names_no_session(case: &str, stated: &str, session_id: &str) {
    assert!(
        !stated.contains(session_id),
        "case {case:?}: the refusal must not name the session {session_id}, got {stated:?}"
    );
    for word in ["session", "Session"] {
        assert!(
            !stated.contains(word),
            "case {case:?}: the refusal must not mention the session at all, got {stated:?}"
        );
    }
}

/// RULE 1, over every operation that resolves an `invocationIdentifier`, both session
/// identifier forms, and every way an identifier can name no invocation of the session
/// addressed — including one that names a real invocation of ANOTHER session, which this
/// session does not hold either.
#[tokio::test]
async fn an_unknown_invocation_is_refused_with_one_not_found_template_everywhere() {
    let router = router();
    let (session_id, session_arn) = create_session(&router, Value::Null).await;
    // A second session holding an invocation, so a row can address an id that exists but
    // not here.
    let (other_session, _) = create_session(&router, Value::Null).await;
    let elsewhere = create_invocation(&router, &other_session, json!({})).await;

    let identifiers: Vec<(&str, String)> = vec![
        ("an absent uuid", ABSENT.to_string()),
        ("a second absent uuid", ABSENT_TOO.to_string()),
        ("a third absent uuid", ABSENT_THIRD.to_string()),
        ("another session's invocation", elsewhere.clone()),
    ];

    for (form, invocation_id) in &identifiers {
        for (session_form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
            // (operation, method, the path tail, the body)
            let calls: Vec<(&str, &str, String, Value)> = vec![
                (
                    "PutInvocationStep",
                    "PUT",
                    "/invocationSteps/".to_string(),
                    json!({
                        "invocationIdentifier": invocation_id,
                        "invocationStepTime": date_time(STEP_TIME),
                        "payload": { "contentBlocks": [{ "text": "x" }] },
                    }),
                ),
                (
                    "ListInvocationSteps",
                    "POST",
                    "/invocationSteps/".to_string(),
                    json!({ "invocationIdentifier": invocation_id }),
                ),
            ];
            for (operation, method, tail, body) in calls {
                let case =
                    format!("{operation} naming {form} in a session addressed by {session_form}");
                let path = format!("/sessions/{}{tail}", label(identifier));
                let (status, refused) = call(&router, method, &path, body).await;
                assert_eq!(
                    (status, error_code(&refused)),
                    NOT_FOUND,
                    "case {case:?}: expected {NOT_FOUND:?}, got {status} {refused:?}"
                );
                let expected = invocation_not_found(invocation_id);
                assert_eq!(
                    message(&refused),
                    expected,
                    "case {case:?}: expected message {expected:?}, got {:?}",
                    message(&refused)
                );
                assert_names_no_session(&case, message(&refused), &session_id);
            }
        }
    }
}

/// RULE 2, over several `invocationId` values and both session identifier forms: the
/// second create of an id the session already holds states one template built from the
/// id the caller chose, and nothing is overwritten.
#[tokio::test]
async fn a_duplicate_invocation_id_is_refused_with_one_conflict_template() {
    let router = router();
    let (session_id, session_arn) = create_session(&router, Value::Null).await;

    // An id the service minted, re-submitted: the rule is about the id being held, not
    // about who chose it.
    let minted = create_invocation(&router, &session_id, json!({ "description": "minted" })).await;
    let chosen: Vec<(&str, &str)> = vec![
        ("a client-supplied uuid", ABSENT),
        ("a second client-supplied uuid", ABSENT_TOO),
    ];
    let mut held: Vec<(&str, String)> = vec![("a service-minted uuid", minted)];
    for (form, invocation_id) in chosen {
        let created = create_invocation(
            &router,
            &session_id,
            json!({ "invocationId": invocation_id }),
        )
        .await;
        assert_eq!(
            created, invocation_id,
            "case {form:?}: CreateInvocation must keep the supplied invocationId, got {created}"
        );
        held.push((form, invocation_id.to_string()));
    }

    for (form, invocation_id) in &held {
        for (session_form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
            let case = format!("re-creating {form} in a session addressed by {session_form}");
            let (status, refused) = call(
                &router,
                "PUT",
                &format!("/sessions/{}/invocations/", label(identifier)),
                json!({ "invocationId": invocation_id, "description": "second" }),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                CONFLICT,
                "case {case:?}: expected {CONFLICT:?}, got {status} {refused:?}"
            );
            let expected = invocation_already_exists(invocation_id);
            assert_eq!(
                message(&refused),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                message(&refused)
            );
            assert_names_no_session(&case, message(&refused), &session_id);
        }
    }

    // A refused create is not a write: the session still holds exactly the invocations
    // the successful creates made, each once.
    let (status, listed) = call(
        &router,
        "POST",
        &format!("/sessions/{session_id}/invocations/"),
        json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListInvocations: expected 200, got {status} {listed:?}"
    );
    let mut stored: Vec<String> = listed["invocationSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("ListInvocations: no invocationSummaries in {listed:?}"))
        .iter()
        .map(|summary| {
            summary["invocationId"]
                .as_str()
                .unwrap_or_else(|| panic!("ListInvocations: no invocationId in {summary:?}"))
                .to_string()
        })
        .collect();
    stored.sort();
    let mut expected: Vec<String> = held
        .iter()
        .map(|(_, invocation_id)| invocation_id.clone())
        .collect();
    expected.sort();
    assert_eq!(
        stored, expected,
        "the refused creates must have added and removed nothing, got {listed:?}"
    );
}

/// RULE 3, over EVERY shape the miss can take: the pair resolves to no stored step
/// because the invocation is unknown, because the step is unknown, or because the step
/// belongs to another invocation or another session — and all of them state the one
/// sentence, so the refusal says nothing about which half missed.
#[tokio::test]
async fn an_unresolvable_invocation_step_pair_states_one_combination_template() {
    let router = router();
    let (session_id, session_arn) = create_session(&router, Value::Null).await;
    let invocation = create_invocation(&router, &session_id, json!({})).await;
    let step = put_step(
        &router,
        &session_id,
        &invocation,
        STEP_TIME,
        &json!({ "contentBlocks": [{ "text": "stored" }] }),
    )
    .await;
    // A second invocation of the same session, holding a step of its own.
    let sibling = create_invocation(&router, &session_id, json!({})).await;
    let sibling_step = put_step(
        &router,
        &session_id,
        &sibling,
        STEP_TIME,
        &json!({ "contentBlocks": [{ "text": "sibling" }] }),
    )
    .await;
    // A step of another session entirely.
    let (other_session, _) = create_session(&router, Value::Null).await;
    let other_invocation = create_invocation(&router, &other_session, json!({})).await;
    let other_step = put_step(
        &router,
        &other_session,
        &other_invocation,
        STEP_TIME,
        &json!({ "contentBlocks": [{ "text": "elsewhere" }] }),
    )
    .await;

    // (case, the invocationIdentifier sent, the invocationStepId sent)
    let cases: Vec<(&str, &str, &str)> = vec![
        ("neither half is stored", ABSENT, ABSENT_TOO),
        ("an unknown invocation, a stored step id", ABSENT, &step),
        ("a stored invocation, an unknown step", &invocation, ABSENT),
        (
            "a stored invocation, a second unknown step",
            &invocation,
            ABSENT_THIRD,
        ),
        (
            "a stored invocation, the step of its sibling",
            &invocation,
            &sibling_step,
        ),
        (
            "the sibling invocation, the first invocation's step",
            &sibling,
            &step,
        ),
        (
            "a stored invocation, a step of another session",
            &invocation,
            &other_step,
        ),
        (
            "another session's invocation and its own step",
            &other_invocation,
            &other_step,
        ),
    ];

    for (case, invocation_id, step_id) in cases {
        for (session_form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
            let case = format!("{case}, in a session addressed by {session_form}");
            let (status, refused) = call(
                &router,
                "POST",
                &format!(
                    "/sessions/{}/invocationSteps/{step_id}",
                    label(identifier)
                ),
                json!({ "invocationIdentifier": invocation_id }),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND:?}, got {status} {refused:?}"
            );
            let expected = step_combination_not_found(invocation_id, step_id);
            assert_eq!(
                message(&refused),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                message(&refused)
            );
            assert_names_no_session(&case, message(&refused), &session_id);
            assert!(
                !message(&refused).contains(&format!("{REGION}:{ACCOUNT}")),
                "case {case:?}: the refusal must not echo the session ARN, got {:?}",
                message(&refused)
            );
        }
    }

    // The stored step is still readable through the pair that really names it, so none
    // of those refusals touched the store.
    let (status, got) = call(
        &router,
        "POST",
        &format!("/sessions/{session_id}/invocationSteps/{step}"),
        json!({ "invocationIdentifier": invocation }),
    )
    .await;
    assert_eq!(
        (status, got["invocationStep"]["invocationStepId"].as_str()),
        (200, Some(step.as_str())),
        "the stored step must still be readable, got {status} {got:?}"
    );
}
