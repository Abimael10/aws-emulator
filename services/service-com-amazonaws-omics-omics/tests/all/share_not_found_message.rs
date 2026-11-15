// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests pinning the WORDING of the share not-found refusal.
//!
//! The rule under test: when a share-addressed operation cannot act on the share the
//! request names, the service answers HTTP 404 `ResourceNotFoundException` whose message
//! is ONE sentence of fixed text for the resource class — it names the kind of resource
//! that was requested and echoes nothing of the request or of state, so every such
//! refusal of every share-addressed operation carries byte-identical text whatever id was
//! presented and whichever condition produced it (no share carries the id, or the share's
//! delete has already been asked for).
//!
//! The cases below therefore vary along everything the rule generalises over — the form,
//! length and value of the presented id, the operation it is presented to, and the state
//! the addressed share is in — and assert the single sentence for all of them.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::private_workflow_runs::{
    call, emulator, error_code, get_workflow, workflow_for_runs, POLL_INTERVAL, SETTLE_DEADLINE,
};

/// The sentence the service answers a share operation with when the share the request
/// names is not one it can act on.
///
/// Source: the recorded `ResourceNotFoundException` of a second `DeleteShare` on a share
/// already deleted — fixed text with no variable part, written out here rather than read
/// from the emulator's own constant so the test pins the wording independently.
const SHARE_NOT_FOUND_SENTENCE: &str = "Requested share not found";

/// Accounts a share can be offered to: twelve-digit ids, none of them the account this
/// emulator serves (the resource owner, which cannot also be the subscriber).
const SUBSCRIBERS: &[&str] = &["123456789012", "210987654321"];

/// `GET /share/{shareId}`.
async fn get_share(router: &MultiServiceRouter, share_id: &str) -> (u16, Value) {
    call(router, "GET", &format!("/share/{share_id}"), None).await
}

/// `DELETE /share/{shareId}`.
async fn delete_share(router: &MultiServiceRouter, share_id: &str) -> (u16, Value) {
    call(router, "DELETE", &format!("/share/{share_id}"), None).await
}

/// A workflow this emulator holds and the ARN its own reader reports for it: a share is
/// offered against the ARN the service hands out, never one spelled by hand here.
async fn shareable_workflow(router: &MultiServiceRouter, request_id: &str) -> (String, String) {
    let id = workflow_for_runs(router, request_id).await;
    let (status, body) = get_workflow(router, &id).await;
    assert_eq!(
        status, 200,
        "fixture {request_id:?}: expected 200 from GetWorkflow, got {body:?}"
    );
    let arn = body
        .get("arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {request_id:?}: workflow carried no arn, got {body:?}"))
        .to_string();
    (id, arn)
}

/// Offer `resource_arn` to `subscriber` and answer the minted `shareId`.
async fn standing_share(
    router: &MultiServiceRouter,
    resource_arn: &str,
    subscriber: &str,
) -> String {
    let body = json!({ "resourceArn": resource_arn, "principalSubscriber": subscriber });
    let (status, response) = call(router, "POST", "/share", Some(&body)).await;
    assert_eq!(
        status, 200,
        "fixture {subscriber:?}: expected 200 from CreateShare, got {response:?}"
    );
    response
        .get("shareId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fixture {subscriber:?}: no shareId in {response:?}"))
        .to_string()
}

/// Assert one refusal: 404, the modelled error shape, the fixed sentence, and no part of
/// the presented id anywhere in the message. Answers the message it observed, so the
/// caller can check every case agreed on it.
fn assert_refusal(case: &str, presented_id: &str, status: u16, body: &Value) -> String {
    assert_eq!(status, 404, "case {case:?}: expected 404, got {body:?}");
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {body:?}"
    );
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carried no message, got {body:?}"))
        .to_string();
    assert_eq!(
        message, SHARE_NOT_FOUND_SENTENCE,
        "case {case:?}: expected the service's fixed share not-found sentence, got {message:?}"
    );
    assert!(
        !message.contains(presented_id),
        "case {case:?}: the refusal must not echo the requested id {presented_id:?}, \
         got {message:?}"
    );
    message
}

/// Read the share until it reports `expected`. Polling the CONDITION, not sleeping a
/// span: the terminal status is owed to the clock, so what is asserted is that it
/// arrives.
async fn await_share_status(router: &MultiServiceRouter, share_id: &str, expected: &str) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = get_share(router, share_id).await;
        assert_eq!(
            status, 200,
            "share {share_id}: expected 200 from GetShare while awaiting {expected}, \
             got {body:?}"
        );
        let reported = body
            .get("share")
            .and_then(|share| share.get("status"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if reported.as_deref() == Some(expected) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "share {share_id}: expected status {expected} within {SETTLE_DEADLINE:?}, \
             last read {body:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Ids no share this emulator holds can carry, varying along the form, length and value
/// of the presented label. The modelled `shareId` is a plain string with no pattern, so
/// the rule generalises over the shape of the id as well as over its value.
fn absent_ids(real_share_ids: &[String], workflow_id: &str) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "a well-formed uuid no share carries".to_string(),
            "b3c1a8d0-0000-4000-8000-000000000000".to_string(),
        ),
        ("a single digit".to_string(), "7".to_string()),
        (
            "a label of no id form at all".to_string(),
            "not-a-share-id".to_string(),
        ),
        ("a very long label".to_string(), "4".repeat(256)),
        (
            "a label of a different resource type".to_string(),
            workflow_id.to_string(),
        ),
        (
            "an uppercased real share id".to_string(),
            real_share_ids[0].to_uppercase(),
        ),
        (
            "a real share id with a suffix".to_string(),
            format!("{}-x", real_share_ids[0]),
        ),
        (
            "a real share id without its first character".to_string(),
            real_share_ids[0].chars().skip(1).collect(),
        ),
    ];
    // No row may accidentally name a share the fixture really created, or it would be
    // testing a standing share instead of an absent one.
    rows.retain(|(_, id)| !real_share_ids.contains(id));
    rows
}

#[tokio::test]
async fn every_share_not_found_refusal_carries_the_same_fixed_sentence() {
    let router = emulator();
    let (workflow_id, workflow_arn) = shareable_workflow(&router, "share-wording").await;
    // Two standing shares, so the refusals below are about the share each call addresses
    // and not about an empty registry.
    let mut real_ids = Vec::new();
    for subscriber in SUBSCRIBERS {
        real_ids.push(standing_share(&router, &workflow_arn, subscriber).await);
    }

    let mut messages: BTreeSet<String> = BTreeSet::new();

    // Condition one: no share carries the presented id — asserted for both operations
    // that resolve a `shareId`.
    for (case, id) in absent_ids(&real_ids, &workflow_id) {
        for (operation, (status, body)) in [
            ("GetShare", get_share(&router, &id).await),
            ("DeleteShare", delete_share(&router, &id).await),
        ] {
            let case = format!("{case} via {operation}");
            messages.insert(assert_refusal(&case, &id, status, &body));
        }
    }

    // Condition two: the share's delete has already been asked for, so it is no longer
    // one a delete can act on — both while the delete is in flight and once the clock has
    // settled it. The record itself stays readable throughout, so the refusal has state
    // it could have quoted and still quotes nothing.
    let spent = real_ids[0].clone();
    let (status, body) = delete_share(&router, &spent).await;
    assert_eq!(
        status, 200,
        "fixture: expected 200 from the first DeleteShare, got {body:?}"
    );

    let (status, body) = delete_share(&router, &spent).await;
    messages.insert(assert_refusal(
        "a share whose delete was already asked for",
        &spent,
        status,
        &body,
    ));

    await_share_status(&router, &spent, "DELETED").await;
    let (status, body) = delete_share(&router, &spent).await;
    messages.insert(assert_refusal(
        "a share the clock has settled to DELETED",
        &spent,
        status,
        &body,
    ));

    // The refusal has NO variable part: every case above, across both operations, both
    // conditions and every presented id, produced one and the same sentence.
    assert_eq!(
        messages.iter().cloned().collect::<Vec<_>>(),
        vec![SHARE_NOT_FOUND_SENTENCE.to_string()],
        "the share not-found refusal must be one fixed sentence for the whole class, \
         got {messages:?}"
    );

    // The share nobody deleted is untouched by every refusal above.
    let (status, body) = get_share(&router, &real_ids[1]).await;
    assert_eq!(
        status, 200,
        "the standing share must still read back, got {body:?}"
    );
    assert_eq!(
        body.get("share")
            .and_then(|share| share.get("status"))
            .and_then(Value::as_str),
        Some("PENDING"),
        "the standing share must still be PENDING, got {body:?}"
    );
}
