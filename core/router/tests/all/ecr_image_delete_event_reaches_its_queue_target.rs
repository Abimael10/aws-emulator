// SPDX-License-Identifier: CC0-1.0

//! A manual `BatchDeleteImage` on an Amazon ECR repository reaches the EventBridge rule that
//! matches it, through the WHOLE emulator.
//!
//! The RULE under test: **every image action a call really performs on a repository — a
//! push that stores a manifest, a delete that takes an image or one of its tags off it — is
//! published ONCE on the account's default EventBridge bus as `source` `aws.ecr` /
//! `detail-type` `ECR Image Action`, carrying in `detail` the outcome of the action
//! (`result`), the repository, the image's digest, the action that was performed
//! (`action-type`) and, when the action involved one, the image's tag; EventBridge then
//! routes it by its CONTENT, so a rule whose `EventPattern` matches delivers it to that
//! rule's targets — an identifier that names no stored image performs no action and is
//! reported in `failures` instead, publishing nothing, and no action nobody performed is
//! published at all, which is why a repository that carries NO lifecycle policy (one whose
//! `GetLifecyclePolicy` is refused `LifecyclePolicyNotFoundException`) produces no `DELETE`
//! event of its own.**
//!
//! Three services have to agree for that to hold (Amazon ECR performs and publishes,
//! EventBridge routes and delivers, Amazon SQS holds what arrived), so this suite lives
//! here: a crate-local test can only ever see one side of it. The crate-local suite
//! `services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/tests/all/image_action_events.rs`
//! pins the `detail` of one action member for member.
//!
//! The rows differ along what the rule generalises over: how the delete names what it
//! removes (by digest, by tag), whether the named tag is the image's last one, how many tags
//! the image carries, whether it carries any at all, how many identifiers one call names, and
//! whether an identifier resolves to a stored image.
//!
//! The wire harness — the router, the per-service RPC helpers, a granted queue, a rule's
//! target and a drain of what arrived — is the sibling suite
//! [`super::ecr_scan_on_push_event_reaches_its_queue_target`]'s, imported rather than written
//! again; what is this suite's own is the table of deletes, the pattern that filters by action
//! type, and the counting of what was delivered.
//!
//! Nothing here sleeps for an event: it POLLS the queues, exactly as a client does — and
//! every "nothing was delivered" read is made at the instant the deliveries that WERE
//! expected have already arrived, rather than after a span of the clock.

use std::collections::{BTreeMap, BTreeSet};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecr_scan_on_push_event_reaches_its_queue_target::{
    authorization, ecr, events, member, push, queue, router, sqs, target, DEFAULT_BUS,
    DELIVERY_DEADLINE, ECR_TARGET_PREFIX, POLL_INTERVAL, REGION, SOURCE,
};

/// The `detail-type` Amazon ECR publishes an image action under, spelled out here rather than
/// imported from the service crate, so a wrong constant in the implementation cannot make
/// this suite pass.
const IMAGE_ACTION: &str = "ECR Image Action";

/// The `detail.action-type` of a push and of a delete, and the `detail.result` of an action
/// the registry carried out.
const PUSH: &str = "PUSH";
const DELETE: &str = "DELETE";
const SUCCESS: &str = "SUCCESS";

/// The `failureCode` an `imageIds` entry that resolves to no stored image is reported under,
/// and the `failureReason` beside it — Amazon ECR's own wording for an `ImageFailure` whose
/// identifier names nothing in the repository, stated here as the service contract this suite
/// holds the answer to.
const IMAGE_NOT_FOUND: &str = "ImageNotFound";
const IMAGE_NOT_FOUND_REASON: &str = "Requested image not found";

/// One published image action, as a rule's targets observe it: the action performed, the
/// image's digest and the tag the action involved, if any.
type ActionKey = (String, String, Option<String>);

/// One `X-Amz-Target` call against Amazon ECR, answering the status and the parsed body —
/// which is what a REFUSAL is read from, and the one thing the shared harness's own helpers,
/// each of which asserts a 200, cannot answer.
async fn ecr_answer(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecr.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ecr"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon ECR call that must be REFUSED with the error code `expected`.
async fn ecr_refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    expected: &str,
) {
    let (status, parsed) = ecr_answer(router, operation, body).await;
    assert_ne!(
        status, 200,
        "case {case:?}: expected {operation} to be refused {expected}, got HTTP 200 with \
         {parsed}"
    );
    // An awsJson error code travels in the body's `__type`, which may be namespace
    // qualified (`namespace#Name`).
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or("<no __type>")
        .to_string();
    assert_eq!(
        code, expected,
        "case {case:?}: expected {operation} to be refused {expected}, got {code} with \
         {parsed}"
    );
}

/// An ENABLED rule on the default bus matching the image actions of `repository` — every one
/// of them, or only those whose `action-type` is `action` — delivering to the queue
/// `queue_arn`.
async fn rule_delivering_to(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    repository: &str,
    action: Option<&str>,
    queue_arn: &str,
) {
    let mut detail = json!({"repository-name": [repository]});
    if let Some(action) = action {
        detail["action-type"] = json!([action]);
    }
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [IMAGE_ACTION],
                "detail": detail,
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    target(router, case, name, queue_arn).await;
}

/// Every envelope now waiting on the queue at `url`, each TAKEN OFF the queue.
///
/// This suite counts deliveries, so a message it has already read must not be handed to it a
/// second time when its visibility window closes: each one is deleted as it is read, which is
/// what the shared drain — written for a suite that only asks whether something arrived — does
/// not do.
async fn take(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut envelopes = Vec::new();
    for message in &messages {
        let handle = member(case, message, "/ReceiptHandle");
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({"QueueUrl": url, "ReceiptHandle": handle}),
        )
        .await;
        let raw = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
    }
    envelopes
}

/// The action one envelope reports: its `action-type`, the image's digest and the tag the
/// action involved, if any.
fn key_of(envelope: &Value) -> ActionKey {
    (
        envelope
            .pointer("/detail/action-type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        envelope
            .pointer("/detail/image-digest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        envelope
            .pointer("/detail/image-tag")
            .and_then(Value::as_str)
            .map(ToString::to_string),
    )
}

/// How many times each action of `arrived` was delivered.
fn deliveries(arrived: &[Value]) -> BTreeMap<ActionKey, usize> {
    let mut counts: BTreeMap<ActionKey, usize> = BTreeMap::new();
    for envelope in arrived {
        *counts.entry(key_of(envelope)).or_default() += 1;
    }
    counts
}

/// Collect envelopes off the queue at `url` until every action of `expected` has arrived or
/// [`DELIVERY_DEADLINE`] passes, and answer everything that arrived.
///
/// One more round is read after the expected set is complete, so a second copy of an action
/// that was published a moment behind the first is in what this answers rather than left on
/// the queue for nobody to count.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: &BTreeSet<ActionKey>,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(take(router, case, url).await);
        let seen: BTreeSet<ActionKey> = arrived.iter().map(key_of).collect();
        if expected.is_subset(&seen) {
            tokio::time::sleep(POLL_INTERVAL).await;
            arrived.extend(take(router, case, url).await);
            return arrived;
        }
        if tokio::time::Instant::now() >= deadline {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One `BatchDeleteImage` call of the table: the identifiers it names, the `(digest, tag)`
/// pairs it must report as deleted — which are the actions it must publish — and the
/// identifiers it must report as failures.
struct Deletion {
    /// What distinguishes this row from the others.
    case: &'static str,
    /// The `imageIds` the call names.
    ids: Vec<Value>,
    /// The `(digest, tag)` identifiers the call must report in `imageIds`.
    deleted: Vec<(String, Option<String>)>,
    /// The entries of `ids` that resolve to no stored image, which the call must report in
    /// `failures` and must publish nothing for.
    failures: Vec<Value>,
}

/// The `(digest, tag)` pairs a response reports at `pointer`, as a set.
fn reported(body: &Value, pointer: &str) -> BTreeSet<(String, Option<String>)> {
    body.pointer(pointer)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|id| {
            (
                id["imageDigest"].as_str().unwrap_or_default().to_string(),
                id["imageTag"].as_str().map(ToString::to_string),
            )
        })
        .collect()
}

/// RULE: every image action a call performs is published once on the default bus and
/// delivered to the rule whose pattern matches it — so the deletes a `BatchDeleteImage`
/// performs arrive at the queue the matching rule targets, an identifier that named no stored
/// image is reported as a failure and delivers nothing, and a repository with no lifecycle
/// policy, whose images nobody deleted, has nothing delivered for it at all.
///
/// Nothing here waits a fixed span for a delivery that must NOT happen: the rule on the
/// untouched repository is targeted from the start, so the arrival of the deletes on the
/// repository under test is the instant at which the publication path has already carried and
/// routed events — and every "received nothing" read is made at that instant.
#[tokio::test]
async fn a_manual_batch_delete_image_reaches_the_matching_rules_queue_target() {
    let case = "a manual delete of images from a repository with no lifecycle policy";
    let router = router();
    let repository = "sim-ecr-image-actions";
    let untouched = "sim-ecr-image-actions-untouched";

    // Three queues: the one the DELETE-only rule targets, the one the rule on every action
    // of the same repository targets, and the one the rule on the untouched repository's
    // deletes targets.
    let (deletes_url, deletes_arn) = queue(&router, case, "ecr-image-deletes").await;
    let (actions_url, actions_arn) = queue(&router, case, "ecr-image-actions").await;
    let (untouched_url, untouched_arn) = queue(&router, case, "ecr-image-untouched").await;

    for name in [repository, untouched] {
        ecr(
            &router,
            case,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
    }

    rule_delivering_to(
        &router,
        case,
        "ecr-image-deletes",
        repository,
        Some(DELETE),
        &deletes_arn,
    )
    .await;
    rule_delivering_to(
        &router,
        case,
        "ecr-image-actions",
        repository,
        None,
        &actions_arn,
    )
    .await;
    rule_delivering_to(
        &router,
        case,
        "ecr-image-untouched",
        untouched,
        Some(DELETE),
        &untouched_arn,
    )
    .await;

    // Neither repository carries a lifecycle policy, which is what the registry's own read
    // reports: there is no policy to expire anything, so no DELETE is owed to either of
    // them.
    for name in [repository, untouched] {
        ecr_refused(
            &router,
            case,
            "GetLifecyclePolicy",
            &json!({"repositoryName": name}),
            "LifecyclePolicyNotFoundException",
        )
        .await;
    }

    // The images the deletes below name, and the pushes that stored them. `multi` and
    // `retained` are each stored once and tagged twice, `untagged` carries no tag at all,
    // and `keep` is never deleted.
    let victim = push(&router, case, repository, "victim", Some("victim")).await;
    let keep = push(&router, case, repository, "keep", Some("keep")).await;
    let multi = push(&router, case, repository, "multi", Some("m1")).await;
    let multi_again = push(&router, case, repository, "multi", Some("m2")).await;
    assert_eq!(
        multi, multi_again,
        "case {case:?}: one manifest is stored under one digest, however many tags point at \
         it, got {multi} and then {multi_again}"
    );
    let retained = push(&router, case, repository, "retained", Some("r1")).await;
    push(&router, case, repository, "retained", Some("r2")).await;
    let untagged = push(&router, case, repository, "untagged", None).await;
    // The untouched repository holds an image too, so the only difference between it and
    // the repository under test is that nothing deletes from it.
    push(&router, case, untouched, "untouched", Some("v1")).await;

    // The pushes above are image actions as well, so the rule on EVERY action of the
    // repository under test must carry one event each.
    let expected_pushes: BTreeSet<ActionKey> = [
        (PUSH.to_string(), victim.clone(), Some("victim".to_string())),
        (PUSH.to_string(), keep.clone(), Some("keep".to_string())),
        (PUSH.to_string(), multi.clone(), Some("m1".to_string())),
        (PUSH.to_string(), multi.clone(), Some("m2".to_string())),
        (PUSH.to_string(), retained.clone(), Some("r1".to_string())),
        (PUSH.to_string(), retained.clone(), Some("r2".to_string())),
        (PUSH.to_string(), untagged.clone(), None),
    ]
    .into_iter()
    .collect();

    // THE TABLE: the rows differ in how the delete names what it removes, whether the tag
    // it names is the image's last, how many identifiers one call carries, and whether an
    // identifier resolves to a stored image at all.
    let table = vec![
        Deletion {
            case: "a delete naming the last tag of an image, which removes the image",
            ids: vec![json!({"imageTag": "victim"})],
            deleted: vec![(victim.clone(), Some("victim".to_string()))],
            failures: vec![],
        },
        Deletion {
            case: "a delete by digest of an image carrying two tags",
            ids: vec![json!({"imageDigest": &multi})],
            deleted: vec![
                (multi.clone(), Some("m1".to_string())),
                (multi.clone(), Some("m2".to_string())),
            ],
            failures: vec![],
        },
        Deletion {
            case: "a batch naming one tag of a two-tag image, an untagged image by digest, \
                   and a tag nothing is stored under",
            ids: vec![
                json!({"imageTag": "r1"}),
                json!({"imageDigest": &untagged}),
                json!({"imageTag": "no-such-tag"}),
            ],
            deleted: vec![
                (retained.clone(), Some("r1".to_string())),
                (untagged.clone(), None),
            ],
            failures: vec![json!({"imageTag": "no-such-tag"})],
        },
    ];

    let mut expected_deletes: BTreeSet<ActionKey> = BTreeSet::new();
    for row in &table {
        let answered = ecr(
            &router,
            row.case,
            "BatchDeleteImage",
            &json!({"repositoryName": repository, "imageIds": row.ids}),
        )
        .await;
        let expected_ids: BTreeSet<(String, Option<String>)> =
            row.deleted.iter().cloned().collect();
        let reported_ids = reported(&answered, "/imageIds");
        assert_eq!(
            reported_ids, expected_ids,
            "case {:?}: expected the delete to report {expected_ids:?}, got {answered}",
            row.case
        );

        // An identifier that resolves to no stored image is reported against its own entry,
        // echoed as the request named it, and nothing is deleted or published for it.
        let failures = answered
            .pointer("/failures")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            failures.len(),
            row.failures.len(),
            "case {:?}: expected {} failure(s), got {answered}",
            row.case,
            row.failures.len()
        );
        for (failure, requested) in failures.iter().zip(&row.failures) {
            for (pointer, expected) in [
                ("/imageId", requested.clone()),
                ("/failureCode", json!(IMAGE_NOT_FOUND)),
                ("/failureReason", json!(IMAGE_NOT_FOUND_REASON)),
            ] {
                let actual = failure.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {:?}: expected {pointer} of the failure to be {expected}, got \
                     {actual} in {failure}",
                    row.case
                );
            }
        }

        for (digest, tag) in &row.deleted {
            expected_deletes.insert((DELETE.to_string(), digest.clone(), tag.clone()));
        }
    }

    // The deletes the calls performed are delivered to the rule that matches them, each
    // exactly once.
    let delivered = collect_until(&router, case, &deletes_url, &expected_deletes).await;
    let counted = deliveries(&delivered);
    let delivered_keys: BTreeSet<ActionKey> = counted.keys().cloned().collect();
    assert_eq!(
        delivered_keys, expected_deletes,
        "case {case:?}: expected the deletes {expected_deletes:?} and nothing else on the \
         rule's queue, got {delivered_keys:?} from {delivered:?}"
    );
    for (action, count) in &counted {
        assert_eq!(
            *count, 1,
            "case {case:?}: expected the action {action:?} to be published once, got {count} \
             deliveries of it"
        );
    }

    // What each delivered envelope states: the event's identity, and the outcome and the
    // repository of the action it reports.
    for envelope in &delivered {
        let (_, digest, tag) = key_of(envelope);
        let described = format!("the delete of {digest} (tag {tag:?})");
        for (pointer, expected) in [
            ("/source", json!(SOURCE)),
            ("/detail-type", json!(IMAGE_ACTION)),
            ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
            ("/region", json!(REGION)),
            ("/detail/result", json!(SUCCESS)),
            ("/detail/repository-name", json!(repository)),
            ("/detail/action-type", json!(DELETE)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} of {described} to be {expected}, got \
                 {actual} in {envelope}"
            );
        }
        if tag.is_none() {
            assert!(
                envelope.pointer("/detail/image-tag").is_none(),
                "case {case:?}: an action involving no tag must state no image-tag, got \
                 {envelope}"
            );
        }
    }

    // The same publication path carries the pushes, which the rule on every action of this
    // repository receives — and the DELETE-only rule above received none of them, which the
    // set equality asserted on its queue is what states.
    let mut expected_actions = expected_pushes.clone();
    expected_actions.extend(expected_deletes.iter().cloned());
    let all_actions = collect_until(&router, case, &actions_url, &expected_actions).await;
    let all_counted = deliveries(&all_actions);
    let all_keys: BTreeSet<ActionKey> = all_counted.keys().cloned().collect();
    assert_eq!(
        all_keys, expected_actions,
        "case {case:?}: expected every push and every delete of this repository on the \
         unfiltered rule's queue ({expected_actions:?}), got {all_keys:?}"
    );
    for (action, count) in &all_counted {
        assert_eq!(
            *count, 1,
            "case {case:?}: expected the action {action:?} to be published once, got {count} \
             deliveries of it"
        );
    }

    // THE DIFFERENTIAL, read at the instant the manual deletes have already been delivered:
    // the repository nobody deleted from carries no lifecycle policy either, so nothing
    // expired an image of it and its own rule — targeted since before every push — has
    // received nothing.
    let autonomous = take(&router, case, &untouched_url).await;
    assert!(
        autonomous.is_empty(),
        "case {case:?}: a repository with no lifecycle policy must have no image deleted on \
         its own, got {autonomous:?}"
    );

    // The registry's own reads agree with what was published: what the deletes removed is
    // gone, and what they did not remove is still there.
    let described = ecr(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": repository}),
    )
    .await;
    let remaining: BTreeSet<String> = described
        .pointer("/imageDetails")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|detail| {
            detail["imageDigest"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    let expected_remaining: BTreeSet<String> =
        [keep.clone(), retained.clone()].into_iter().collect();
    assert_eq!(
        remaining, expected_remaining,
        "case {case:?}: expected the repository to still hold {expected_remaining:?}, got \
         {remaining:?} from {described}"
    );
    let still_tagged = ecr(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": repository, "imageIds": [{"imageTag": "r2"}]}),
    )
    .await;
    assert_eq!(
        still_tagged.pointer("/imageDetails/0/imageDigest"),
        Some(&json!(retained)),
        "case {case:?}: a delete of one tag leaves the image at its other tag, got \
         {still_tagged}"
    );
    ecr_refused(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": repository, "imageIds": [{"imageTag": "victim"}]}),
        "ImageNotFoundException",
    )
    .await;
}
