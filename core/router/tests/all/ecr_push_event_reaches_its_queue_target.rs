// SPDX-License-Identifier: CC0-1.0

//! An image PUSH into an Amazon ECR repository reaches the SQS queue an EventBridge rule
//! targets, through the WHOLE emulator.
//!
//! The RULE under test: **every push that really stores a manifest in a repository is
//! published ONCE on the account's default EventBridge bus as `source` `aws.ecr` /
//! `detail-type` `ECR Image Action`, carrying in `detail` the outcome of the action
//! (`result`), the repository it targeted, the digest the manifest was stored under
//! (`image-digest`), the action performed (`action-type` `PUSH`) and — when the push named
//! one — the tag it attached (`image-tag`); EventBridge then routes that event by its
//! CONTENT, so a rule whose `EventPattern` matches delivers it to that rule's targets, and
//! an SQS target receives the envelope on a queue that granted `events.amazonaws.com`
//! `sqs:SendMessage`. A push nothing performed — one refused because the manifest is
//! already stored under the tag it names — publishes nothing, and a rule whose pattern
//! names another repository, another tag or another action type receives none of it.**
//!
//! Three services have to agree for that to hold (Amazon ECR performs and publishes,
//! EventBridge routes and delivers, Amazon SQS holds what arrived), so this suite lives
//! here: a crate-local test can only ever see one side of it. The crate-local suite
//! `services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/tests/all/image_action_events.rs`
//! pins the `detail` of one action member for member, and the sibling suite
//! [`super::ecr_image_delete_event_reaches_its_queue_target`] pins the DELETE half of the
//! same rule; what is this suite's own is the PUSH half against a rule that filters on the
//! action type, and against one that filters on the pushed TAG.
//!
//! The rows differ along what the rule generalises over: whether the push names a tag, what
//! the tag looks like, whether the manifest is new to the repository or already stored under
//! another tag, which repository the push targets, and whether the push is performed at all.
//!
//! The wire harness — the router, the per-service RPC helpers, a granted queue, a rule's
//! target and a push — is the sibling suite
//! [`super::ecr_scan_on_push_event_reaches_its_queue_target`]'s, imported rather than
//! written again.
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

/// The `detail-type` Amazon ECR publishes an image action under, spelled out here rather
/// than imported from the service crate, so a wrong constant in the implementation cannot
/// make this suite pass.
const IMAGE_ACTION: &str = "ECR Image Action";

/// The `detail.action-type` of a push and of a delete, and the `detail.result` of an action
/// the registry carried out.
const PUSH: &str = "PUSH";
const DELETE: &str = "DELETE";
const SUCCESS: &str = "SUCCESS";

/// One published image action, as a rule's targets observe it: the digest the manifest is
/// stored under and the tag the action involved, if any.
type ActionKey = (String, Option<String>);

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
        "case {case:?}: expected {operation} to be refused {expected}, got {code} with {parsed}"
    );
}

/// An ENABLED rule on the default bus matching the image actions of `repository` whose
/// `action-type` is `action` — and, where the row filters on one, whose `image-tag` is `tag`
/// — delivering to the queue `queue_arn`.
async fn rule_delivering_to(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    repository: &str,
    action: &str,
    tag: Option<&str>,
    queue_arn: &str,
) {
    let mut detail = json!({"repository-name": [repository], "action-type": [action]});
    if let Some(tag) = tag {
        detail["image-tag"] = json!([tag]);
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
/// second time when its visibility window closes: each one is deleted as it is read.
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

/// The action one envelope reports: the image's digest and the tag the action involved, if
/// any.
fn key_of(envelope: &Value) -> ActionKey {
    (
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

/// The digest a registry addresses a manifest by: `sha256:<hex>` over the manifest bytes,
/// computed here independently of the services under test, so a published digest cannot be
/// an invented string the registry and the event merely agree on.
fn manifest_digest(marker: &str) -> String {
    use sha2::{Digest, Sha256};
    // The manifest bytes the shared `push` helper sends for this row.
    let manifest = json!({"schemaVersion": 2, "marker": marker}).to_string();
    format!("sha256:{}", hex::encode(Sha256::digest(manifest.as_bytes())))
}

/// One push of the table: what distinguishes the row, the manifest it sends and the tag it
/// names, if any.
struct Push {
    /// What distinguishes this row from the others.
    case: &'static str,
    /// The marker the row's manifest carries, which is what makes its digest its own.
    marker: &'static str,
    /// The tag the push attaches, when it attaches one.
    tag: Option<&'static str>,
}

/// The tag the rule that filters by tag names, which exactly one row of the table pushes.
const FILTERED_TAG: &str = "release";

/// RULE: every push that stores a manifest is published once on the default bus and
/// delivered to the queue the rule whose pattern matches it targets — so each push of a
/// repository arrives at the rule filtering that repository's pushes exactly once, carrying
/// the digest the registry's own read reports and the tag the push named (and no `image-tag`
/// when it named none); only the push carrying the filtered tag reaches the tag-filtered
/// rule; a push reaches no rule naming another repository or another action type; and a push
/// that is refused because the manifest is already stored under that tag publishes nothing.
///
/// Nothing here waits a fixed span for a delivery that must NOT happen: every negative rule
/// is targeted from the start, so the arrival of the pushes on the rule under test is the
/// instant at which the publication path has already carried and routed events — and every
/// "received nothing" read is made at that instant.
#[tokio::test]
async fn a_push_reaches_the_matching_rules_queue_target() {
    let case = "a push into a repository whose pushes a rule matches";
    let router = router();
    let repository = "sim-ecr-push-events";
    let other = "sim-ecr-push-events-other";

    // Four queues: the one the PUSH rule on the repository under test targets, the one the
    // rule filtering that repository's pushes BY TAG targets, the one the DELETE-only rule
    // on the same repository targets, and the one the PUSH rule on the other repository
    // targets.
    let (pushes_url, pushes_arn) = queue(&router, case, "ecr-push-events").await;
    let (tagged_url, tagged_arn) = queue(&router, case, "ecr-push-events-tagged").await;
    let (deletes_url, deletes_arn) = queue(&router, case, "ecr-push-events-deletes").await;
    let (other_url, other_arn) = queue(&router, case, "ecr-push-events-other").await;

    for name in [repository, other] {
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
        "ecr-push-events",
        repository,
        PUSH,
        None,
        &pushes_arn,
    )
    .await;
    rule_delivering_to(
        &router,
        case,
        "ecr-push-events-tagged",
        repository,
        PUSH,
        Some(FILTERED_TAG),
        &tagged_arn,
    )
    .await;
    rule_delivering_to(
        &router,
        case,
        "ecr-push-events-deletes",
        repository,
        DELETE,
        None,
        &deletes_arn,
    )
    .await;
    rule_delivering_to(
        &router,
        case,
        "ecr-push-events-other",
        other,
        PUSH,
        None,
        &other_arn,
    )
    .await;

    // THE TABLE: the rows differ in whether the push names a tag, what the tag looks like,
    // and whether the manifest is new to the repository or already stored under another tag.
    let table = vec![
        Push {
            case: "a push of a new manifest under a plain tag",
            marker: "first",
            tag: Some("v1"),
        },
        Push {
            case: "a second tag attached to a manifest the repository already holds",
            marker: "first",
            tag: Some(FILTERED_TAG),
        },
        Push {
            case: "a push of another manifest under a dotted, hyphenated tag",
            marker: "second",
            tag: Some("1.2.3-rc.1"),
        },
        Push {
            case: "a push naming no tag at all",
            marker: "third",
            tag: None,
        },
    ];

    let mut expected: BTreeSet<ActionKey> = BTreeSet::new();
    for row in &table {
        let digest = push(&router, row.case, repository, row.marker, row.tag).await;
        let computed = manifest_digest(row.marker);
        assert_eq!(
            digest, computed,
            "case {:?}: expected the push to store the manifest under {computed}, got {digest}",
            row.case
        );
        expected.insert((digest, row.tag.map(ToString::to_string)));
    }

    // A push nothing performed: the first row's manifest is already stored under the very
    // tag it names, so the call is refused and no second event is owed for that action —
    // which is what the per-action delivery count below states.
    let repeated = &table[0];
    ecr_refused(
        &router,
        "a push of a manifest already stored under the tag it names",
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": json!({"schemaVersion": 2, "marker": repeated.marker}).to_string(),
            "imageTag": repeated.tag,
        }),
        "ImageAlreadyExistsException",
    )
    .await;

    // The other repository's own push, which its own rule must receive and the rule under
    // test must not.
    let elsewhere = push(&router, case, other, "elsewhere", Some("v1")).await;

    // Every push of the repository under test is delivered to the rule that matches it, each
    // exactly once, and nothing else is.
    let delivered = collect_until(&router, case, &pushes_url, &expected).await;
    let counted = deliveries(&delivered);
    let delivered_keys: BTreeSet<ActionKey> = counted.keys().cloned().collect();
    assert_eq!(
        delivered_keys, expected,
        "case {case:?}: expected the pushes {expected:?} and nothing else on the rule's queue, \
         got {delivered_keys:?} from {delivered:?}"
    );
    for (action, count) in &counted {
        assert_eq!(
            *count, 1,
            "case {case:?}: expected the push {action:?} to be published once, got {count} \
             deliveries of it"
        );
    }

    // What each delivered envelope states: the event's identity, and the outcome, the
    // repository and the action of the push it reports.
    for envelope in &delivered {
        let (digest, tag) = key_of(envelope);
        let described = format!("the push of {digest} (tag {tag:?})");
        for (pointer, expected) in [
            ("/source", json!(SOURCE)),
            ("/detail-type", json!(IMAGE_ACTION)),
            ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
            ("/region", json!(REGION)),
            ("/detail/result", json!(SUCCESS)),
            ("/detail/repository-name", json!(repository)),
            ("/detail/action-type", json!(PUSH)),
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
                "case {case:?}: a push naming no tag must state no image-tag, got {envelope}"
            );
        }
    }

    // The tag-filtered rule received only the one push that carried that tag.
    let only_filtered: BTreeSet<ActionKey> = expected
        .iter()
        .filter(|(_, tag)| tag.as_deref() == Some(FILTERED_TAG))
        .cloned()
        .collect();
    let tagged = collect_until(&router, case, &tagged_url, &only_filtered).await;
    let tagged_keys: BTreeSet<ActionKey> = deliveries(&tagged).keys().cloned().collect();
    assert_eq!(
        tagged_keys, only_filtered,
        "case {case:?}: expected a rule filtering on image-tag {FILTERED_TAG:?} to receive \
         {only_filtered:?} and nothing else, got {tagged_keys:?} from {tagged:?}"
    );

    // The other repository's rule received that repository's push, and only it.
    let elsewhere_expected: BTreeSet<ActionKey> =
        [(elsewhere.clone(), Some("v1".to_string()))].into();
    let abroad = collect_until(&router, case, &other_url, &elsewhere_expected).await;
    let abroad_keys: BTreeSet<ActionKey> = deliveries(&abroad).keys().cloned().collect();
    assert_eq!(
        abroad_keys, elsewhere_expected,
        "case {case:?}: expected a rule naming another repository to receive only that \
         repository's own push {elsewhere_expected:?}, got {abroad_keys:?} from {abroad:?}"
    );

    // THE DIFFERENTIAL, read at the instant every push above has already been delivered: a
    // rule on the same repository that filters on the DELETE action type matched none of
    // them, because nothing deleted an image.
    let deletes = take(&router, case, &deletes_url).await;
    assert!(
        deletes.is_empty(),
        "case {case:?}: a rule filtering on the DELETE action type must receive no push, got \
         {deletes:?}"
    );

    // The registry's own reads agree with what was published: each digest an event named is
    // the digest the repository reports for the tag the event named.
    for (digest, tag) in &expected {
        let Some(tag) = tag else { continue };
        let described = ecr(
            &router,
            case,
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [{"imageTag": tag}]}),
        )
        .await;
        assert_eq!(
            described.pointer("/imageDetails/0/imageDigest"),
            Some(&json!(digest)),
            "case {case:?}: expected the repository to report {digest} at the tag {tag:?}, got \
             {described}"
        );
    }
    let untagged = ecr(
        &router,
        case,
        "DescribeImages",
        &json!({"repositoryName": repository}),
    )
    .await;
    let stored: BTreeSet<String> = untagged
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
    let published: BTreeSet<String> = expected.iter().map(|(digest, _)| digest.clone()).collect();
    assert_eq!(
        stored, published,
        "case {case:?}: expected the repository to hold exactly the manifests whose pushes were \
         published ({published:?}), got {stored:?} from {untagged}"
    );
}
