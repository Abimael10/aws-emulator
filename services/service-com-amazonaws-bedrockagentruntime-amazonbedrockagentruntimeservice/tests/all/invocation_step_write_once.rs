// SPDX-License-Identifier: CC0-1.0

//! `PutInvocationStep`'s WRITE-ONCE key, asserted end-to-end over the wire through the
//! `MultiServiceRouter` the production emulator serves with (the harness in
//! [`crate::sessions`]).
//!
//! THE RULE UNDER TEST: `(invocationIdentifier, invocationStepId)` is a write-once key. The
//! FIRST write under a step id claims it; every later write under the same pair is refused
//! with the modelled 409 `ConflictException`
//! (`smithy_query(query="operation",operation="PutInvocationStep")` -> `ConflictException`
//! httpError 409) whatever content it carries — byte-identical content included — and the
//! refusal is keyed on the two ids alone, so no comparison of the two payloads enters it.
//! Exactly one step stays stored under the pair, it is the one first written, and a write
//! under a FRESH step id is accepted and adds a step.
//!
//! The rows below span what a second write can differ in — nothing at all, the instant, the
//! text, the number of content blocks, the member of the content union, the case of the id —
//! because the rule generalises over the CONTENT: none of it is read when the key is already
//! taken.

use serde_json::{json, Value};

use crate::sessions::{call, create_invocation, create_session, date_time, error_code, label, router};

/// The status and error code every second write here expects.
const CONFLICT: (u16, &str) = (409, "ConflictException");

/// The instant the stored step carries, as epoch seconds.
const STEP_TIME: i64 = 1_783_501_811;

/// A second instant, for the row whose only difference from the stored step is when it
/// happened.
const OTHER_STEP_TIME: i64 = 1_783_509_004;

/// A client-supplied id in the canonical form every `Uuid`-shaped member of this model
/// requires (`smithy_query(query="shape",shape="Uuid")` -> `@pattern
/// ^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
fn uuid_n(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

/// The sentence the service states for a pair already stored: the frame it also uses for the
/// not-found refusal over the same pair, closing in ` already exists`, with the two ids as
/// the request carried them.
fn step_pair_already_exists(invocation_id: &str, step_id: &str) -> String {
    format!(
        "Combination of Invocation with ID {invocation_id} and Invcoation Step with ID \
         {step_id} already exists"
    )
}

/// The payload the FIRST write stores, which every row's second write is measured against.
fn stored_payload() -> Value {
    json!({ "contentBlocks": [{ "text": "the first write" }] })
}

/// Write one step under an explicit id, asserting the claim was accepted.
async fn claim_step(
    router: &smithy_server_runtime::MultiServiceRouter,
    session_id: &str,
    invocation_id: &str,
    step_id: &str,
    step_time: i64,
    payload: &Value,
) {
    let (status, out) = call(
        router,
        "PUT",
        &format!("/sessions/{}/invocationSteps/", label(session_id)),
        json!({
            "invocationIdentifier": invocation_id,
            "invocationStepId": step_id,
            "invocationStepTime": date_time(step_time),
            "payload": payload,
        }),
    )
    .await;
    assert_eq!(
        (status, out["invocationStepId"].as_str()),
        (201, Some(step_id)),
        "claiming {step_id}: expected 201 echoing the id, got {status} {out:?}"
    );
}

/// How many steps the invocation holds, read back through `ListInvocationSteps`.
async fn stored_step_count(
    router: &smithy_server_runtime::MultiServiceRouter,
    session_id: &str,
    invocation_id: &str,
) -> usize {
    let (status, listed) = call(
        router,
        "POST",
        &format!("/sessions/{}/invocationSteps/", label(session_id)),
        json!({ "invocationIdentifier": invocation_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListInvocationSteps: expected 200, got {status} {listed:?}"
    );
    listed["invocationStepSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("ListInvocationSteps: no summaries in {listed:?}"))
        .len()
}

/// The rule over the whole class of second writes: each row claims a step id, writes again
/// under it with the row's own content, and reads the refusal, the stored step and the count
/// back.
#[tokio::test]
async fn a_second_write_under_a_stored_step_id_is_refused_whatever_content_it_carries() {
    // (case, the second write's instant, the second write's payload)
    let rows: Vec<(&str, i64, Value)> = vec![
        (
            "byte-identical content: the same payload at the same instant",
            STEP_TIME,
            stored_payload(),
        ),
        (
            "the same payload at another instant",
            OTHER_STEP_TIME,
            stored_payload(),
        ),
        (
            "other text in the one content block",
            STEP_TIME,
            json!({ "contentBlocks": [{ "text": "a rewrite" }] }),
        ),
        (
            "the stored text plus one more block",
            STEP_TIME,
            json!({
                "contentBlocks": [
                    { "text": "the first write" },
                    { "text": "an addendum" },
                ],
            }),
        ),
        (
            // `BedrockSessionContentBlock` is a union of `text` and `image`
            // (`smithy_query(query="shape",shape="BedrockSessionContentBlock")`), so a
            // content block of the OTHER member is as far from the stored one as a payload
            // gets — and it is refused on the key just the same.
            "an image block instead of text",
            OTHER_STEP_TIME,
            json!({
                "contentBlocks": [{
                    "image": {
                        "format": "png",
                        "source": { "bytes": "aW1hZ2UtYnl0ZXM=" },
                    },
                }],
            }),
        ),
    ];

    for (case, step_time, payload) in rows {
        let router = router();
        let (session_id, _) = create_session(&router, Value::Null).await;
        let invocation_id = create_invocation(&router, &session_id, json!({})).await;
        let step_id = uuid_n(1);
        let stored = stored_payload();
        claim_step(
            &router,
            &session_id,
            &invocation_id,
            &step_id,
            STEP_TIME,
            &stored,
        )
        .await;

        let (status, refused) = call(
            &router,
            "PUT",
            &format!("/sessions/{}/invocationSteps/", label(&session_id)),
            json!({
                "invocationIdentifier": invocation_id,
                "invocationStepId": step_id,
                "invocationStepTime": date_time(step_time),
                "payload": payload,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            CONFLICT,
            "case {case:?}: expected {CONFLICT:?}, got {status} {refused:?}"
        );
        let expected = step_pair_already_exists(&invocation_id, &step_id);
        let stated = refused["message"].as_str().unwrap_or_default();
        assert_eq!(
            stated, expected,
            "case {case:?}: expected message {expected:?}, got {stated:?}"
        );
        assert!(
            !stated.contains("content") && !stated.contains(&session_id),
            "case {case:?}: the refusal is keyed on the two ids alone, so it states neither \
             the content nor the session, got {stated:?}"
        );

        // The step the first write stored is still exactly that step.
        let (status, got) = call(
            &router,
            "POST",
            &format!(
                "/sessions/{}/invocationSteps/{step_id}",
                label(&session_id)
            ),
            json!({ "invocationIdentifier": invocation_id }),
        )
        .await;
        assert_eq!(
            (status, &got["invocationStep"]["payload"]),
            (200, &stored),
            "case {case:?}: the stored step must be the one first written, got {status} {got:?}"
        );
        let held = stored_step_count(&router, &session_id, &invocation_id).await;
        assert_eq!(
            held, 1,
            "case {case:?}: a refused write adds no step, got {held} stored"
        );
    }
}

/// The other half of the rule: a write under a step id the invocation does NOT hold is
/// accepted and adds a step, whether the id is the caller's or the service's to mint — so the
/// refusal above is about the KEY and not about writing twice.
#[tokio::test]
async fn a_write_under_a_fresh_step_id_is_accepted_and_adds_a_step() {
    let router = router();
    let (session_id, _) = create_session(&router, Value::Null).await;
    let invocation_id = create_invocation(&router, &session_id, json!({})).await;
    let stored = stored_payload();
    claim_step(
        &router,
        &session_id,
        &invocation_id,
        &uuid_n(1),
        STEP_TIME,
        &stored,
    )
    .await;

    // The SAME content under a fresh client-supplied id: a second step, not a replay.
    claim_step(
        &router,
        &session_id,
        &invocation_id,
        &uuid_n(2),
        STEP_TIME,
        &stored,
    )
    .await;
    let held = stored_step_count(&router, &session_id, &invocation_id).await;
    assert_eq!(
        held, 2,
        "case \"a fresh client-supplied id\": expected 2 stored steps, got {held}"
    );

    // And with no id at all, where the service mints one: a third step.
    let (status, minted) = call(
        &router,
        "PUT",
        &format!("/sessions/{}/invocationSteps/", label(&session_id)),
        json!({
            "invocationIdentifier": invocation_id,
            "invocationStepTime": date_time(OTHER_STEP_TIME),
            "payload": stored,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case \"an id the service mints\": expected 201, got {status} {minted:?}"
    );
    let minted_id = minted["invocationStepId"].as_str().unwrap_or_default();
    assert!(
        !minted_id.is_empty() && minted_id != uuid_n(1) && minted_id != uuid_n(2),
        "case \"an id the service mints\": expected a fresh id, got {minted_id:?}"
    );
    let held = stored_step_count(&router, &session_id, &invocation_id).await;
    assert_eq!(
        held, 3,
        "case \"an id the service mints\": expected 3 stored steps, got {held}"
    );
}
