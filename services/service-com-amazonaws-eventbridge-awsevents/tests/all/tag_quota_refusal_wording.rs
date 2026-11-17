// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a refusal on the per-resource TAG ceiling, end to end
//! over the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: a write that would leave a resource carrying more tags
//! than the per-resource ceiling admits is refused with EventBridge's TAG-quota
//! sentence — its own refusal class, distinct from the sentence a request for one
//! RESOURCE too many answers with — on HTTP 400 `ValidationException`, for every
//! tag write of this service and however the ceiling is crossed.
//!
//! The tables span the class rather than one request: the rows differ along the
//! tag write (`TagResource`, and the `Tags` a `CreateEventBus` or a `PutRule`
//! create carries), along the resource kind tagged (a custom bus, a rule on the
//! `default` bus, a rule on a custom bus), and along HOW the ceiling is crossed
//! (tags already held plus new keys, one key onto a full set, one oversized
//! request onto a resource holding none). The accepted rows sit at the ceiling —
//! a merge landing exactly on it, and an overwrite of a key a full resource
//! already holds — and the distinctness row crosses a resource-COUNT quota, which
//! keeps its own sentence.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "eu-central-1";

/// Largest number of tags one resource carries: "You can associate as many as 50
/// tags with a resource" (`TagResource`'s documentation, via
/// `smithy_query(query="doc", shape="TagResource")`). Spelled out here, not
/// imported, so a wrong bound in the implementation cannot make a test pass.
const TAG_QUOTA: usize = 50;

/// The sentence a tag write past [`TAG_QUOTA`] is refused with, written out here
/// rather than imported from the implementation.
const TAG_QUOTA_MESSAGE: &str = "The request exceeds the maximum number of tags allowed per resource.";

/// The sentence a request past a resource-COUNT quota is refused with — the other
/// refusal class, which must stay distinguishable from the tag one.
const COUNT_QUOTA_MESSAGE: &str = "The requested resource exceeds the maximum number allowed.";

/// Largest number of targets one rule holds: the EventBridge service quota
/// "Targets per rule" (5, a hard limit), used by the distinctness row below.
const TARGETS_PER_RULE: usize = 5;

/// One row of a refusal table: the case, the operation called, the request body.
struct Row {
    case: String,
    operation: &'static str,
    body: Value,
}

/// The emulator: one router carrying only this service over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to this service.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` a failure body carries.
fn message(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `arn:aws:events:<region>:<account>:event-bus/<name>`, built here rather than
/// read from the implementation.
fn bus_arn(name: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:event-bus/{name}",
        engine::emulator_account_id()
    )
}

/// The ARN of a rule on the account's `default` bus, which carries no bus segment.
fn default_bus_rule_arn(name: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:rule/{name}",
        engine::emulator_account_id()
    )
}

/// The ARN of a rule on a custom bus, which names that bus.
fn rule_arn(bus: &str, name: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:rule/{bus}/{name}",
        engine::emulator_account_id()
    )
}

/// `count` distinct tags whose keys start at `from`.
fn tags(from: usize, count: usize) -> Value {
    json!((from..from + count)
        .map(|index| json!({ "Key": format!("k{index}"), "Value": format!("v{index}") }))
        .collect::<Vec<Value>>())
}

/// Call `operation` and fail unless it succeeded.
async fn accepted(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    let (status, answered) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} expected 200, got {status} {answered:?}"
    );
}

/// Create the bus `name`, failing the test if it was not created.
async fn given_bus(router: &MultiServiceRouter, name: &str) {
    accepted(
        router,
        &format!("the bus {name}"),
        "CreateEventBus",
        &json!({ "Name": name }),
    )
    .await;
}

/// Create the rule `name` on `bus` (the `default` bus when `bus` is `None`).
async fn given_rule(router: &MultiServiceRouter, bus: Option<&str>, name: &str) {
    let mut body = json!({
        "Name": name,
        "EventPattern": json!({ "source": ["aws.s3"] }).to_string(),
    });
    if let Some(bus) = bus {
        body["EventBusName"] = json!(bus);
    }
    accepted(router, &format!("the rule {name}"), "PutRule", &body).await;
}

/// The tags `ListTagsForResource` reports for `arn`, in the order it reports them.
async fn stored_tags(router: &MultiServiceRouter, arn: &str) -> Vec<(String, String)> {
    let (status, body) = call(router, "ListTagsForResource", &json!({ "ResourceARN": arn })).await;
    assert_eq!(
        status, 200,
        "reading the tags of {arn}: expected 200, got {status} {body:?}"
    );
    body.get("Tags")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ListTagsForResource({arn}) reports no Tags list: {body:?}"))
        .iter()
        .map(|entry| {
            (
                entry
                    .get("Key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry
                    .get("Value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Every tag write of this service, past the per-resource ceiling and however it
/// is crossed, is refused HTTP 400 `ValidationException` with the tag-quota
/// sentence.
#[tokio::test]
async fn every_tag_write_past_the_per_resource_ceiling_states_the_tag_quota_sentence() {
    let router = emulator();

    // The resources the `TagResource` rows write onto: one per resource kind this
    // service tags, each brought to the tag count its row needs.
    given_bus(&router, "held-forty").await;
    given_rule(&router, None, "rule-held-forty").await;
    given_bus(&router, "custom").await;
    given_rule(&router, Some("custom"), "rule-full").await;
    given_bus(&router, "untagged").await;

    let partly_filled = [
        ("a custom event bus", bus_arn("held-forty")),
        ("a rule on the default bus", default_bus_rule_arn("rule-held-forty")),
    ];
    for (resource, arn) in &partly_filled {
        accepted(
            &router,
            &format!("{resource} holding forty tags"),
            "TagResource",
            &json!({ "ResourceARN": arn, "Tags": tags(0, 40) }),
        )
        .await;
    }
    // A rule on a custom bus, filled to exactly the ceiling.
    let full = rule_arn("custom", "rule-full");
    accepted(
        &router,
        "a rule on a custom bus holding the whole ceiling",
        "TagResource",
        &json!({ "ResourceARN": &full, "Tags": tags(0, TAG_QUOTA) }),
    )
    .await;

    let mut rows: Vec<Row> = Vec::new();
    for (resource, arn) in &partly_filled {
        // Tags already held plus new keys: the ceiling is charged on the set the
        // write would leave, so forty held and eleven named is one past it.
        rows.push(Row {
            case: format!("{resource}: eleven new keys onto forty held"),
            operation: "TagResource",
            body: json!({ "ResourceARN": arn, "Tags": tags(40, 11) }),
        });
    }
    rows.push(Row {
        case: "a rule on a custom bus: one new key onto a full set".to_string(),
        operation: "TagResource",
        body: json!({ "ResourceARN": &full, "Tags": [{ "Key": "one-too-many", "Value": "x" }] }),
    });
    rows.push(Row {
        case: "one request past the ceiling, onto a resource holding no tags".to_string(),
        operation: "TagResource",
        body: json!({ "ResourceARN": bus_arn("untagged"), "Tags": tags(0, TAG_QUOTA + 1) }),
    });
    // The Tags a CREATE carries go through the same ceiling.
    rows.push(Row {
        case: "CreateEventBus naming one tag too many".to_string(),
        operation: "CreateEventBus",
        body: json!({ "Name": "oversized-bus", "Tags": tags(0, TAG_QUOTA + 1) }),
    });
    rows.push(Row {
        case: "CreateEventBus naming far more tags than the ceiling".to_string(),
        operation: "CreateEventBus",
        body: json!({ "Name": "very-oversized-bus", "Tags": tags(0, TAG_QUOTA * 3) }),
    });
    rows.push(Row {
        case: "PutRule naming one tag too many".to_string(),
        operation: "PutRule",
        body: json!({
            "Name": "oversized-rule",
            "EventPattern": json!({ "source": ["aws.s3"] }).to_string(),
            "Tags": tags(0, TAG_QUOTA + 1),
        }),
    });

    for Row {
        case,
        operation,
        body,
    } in &rows
    {
        let (status, refused) = call(&router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {:?} {refused:?}",
            error_code(&refused)
        );
        assert_eq!(
            message(&refused),
            TAG_QUOTA_MESSAGE,
            "case {case:?}: expected {TAG_QUOTA_MESSAGE:?}, got {:?}",
            message(&refused)
        );
    }
}

/// The ceiling is charged on the set a write would LEAVE, so a merge landing on
/// it and an overwrite of a key a full resource already holds are accepted, and a
/// refused write leaves the stored set and the named resource exactly as they
/// were.
#[tokio::test]
async fn a_write_at_the_ceiling_is_accepted_and_a_refused_one_changes_nothing() {
    let router = emulator();
    given_bus(&router, "merging").await;
    let arn = bus_arn("merging");

    accepted(
        &router,
        "forty tags",
        "TagResource",
        &json!({ "ResourceARN": &arn, "Tags": tags(0, 40) }),
    )
    .await;
    // Ten more lands exactly on the ceiling.
    accepted(
        &router,
        "ten more onto forty, landing on the ceiling",
        "TagResource",
        &json!({ "ResourceARN": &arn, "Tags": tags(40, 10) }),
    )
    .await;
    let at_the_ceiling = stored_tags(&router, &arn).await;
    assert_eq!(
        at_the_ceiling.len(),
        TAG_QUOTA,
        "case \"ten more onto forty\": expected {TAG_QUOTA} stored tags, got {}",
        at_the_ceiling.len()
    );

    // An overwrite does not grow the set, so it is accepted at the ceiling.
    accepted(
        &router,
        "an overwrite of a key already held, at the ceiling",
        "TagResource",
        &json!({ "ResourceARN": &arn, "Tags": [{ "Key": "k0", "Value": "rewritten" }] }),
    )
    .await;
    let after_overwrite = stored_tags(&router, &arn).await;
    assert_eq!(
        after_overwrite.len(),
        TAG_QUOTA,
        "case \"an overwrite at the ceiling\": expected {TAG_QUOTA} stored tags, got {}",
        after_overwrite.len()
    );
    assert_eq!(
        after_overwrite
            .first()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        Some(("k0", "rewritten")),
        "case \"an overwrite at the ceiling\": expected k0 rewritten in place, got \
         {after_overwrite:?}"
    );

    // One new key past the ceiling: refused in AWS's words, and nothing applied.
    let (status, refused) = call(
        &router,
        "TagResource",
        &json!({ "ResourceARN": &arn, "Tags": [{ "Key": "one-too-many", "Value": "x" }] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"one new key past the ceiling\": expected 400, got {status} {refused:?}"
    );
    assert_eq!(
        message(&refused),
        TAG_QUOTA_MESSAGE,
        "case \"one new key past the ceiling\": expected {TAG_QUOTA_MESSAGE:?}, got {:?}",
        message(&refused)
    );
    let unchanged = stored_tags(&router, &arn).await;
    assert_eq!(
        unchanged, after_overwrite,
        "case \"one new key past the ceiling\": a refused write changes nothing, expected \
         {after_overwrite:?}, got {unchanged:?}"
    );

    // A create refused on the ceiling leaves no resource behind, so the read of it
    // reports the resource missing rather than an empty one.
    let (status, refused) = call(
        &router,
        "CreateEventBus",
        &json!({ "Name": "never-created", "Tags": tags(0, TAG_QUOTA + 1) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"CreateEventBus past the ceiling\": expected 400, got {status} {refused:?}"
    );
    let (status, described) = call(
        &router,
        "DescribeEventBus",
        &json!({ "Name": "never-created" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"CreateEventBus past the ceiling\": the bus must not exist, got {status} \
         {described:?}"
    );
    assert_eq!(
        error_code(&described),
        "ResourceNotFoundException",
        "case \"CreateEventBus past the ceiling\": expected ResourceNotFoundException, got {:?} \
         {described:?}",
        error_code(&described)
    );
    assert_eq!(
        message(&described),
        "Event bus never-created does not exist.",
        "case \"CreateEventBus past the ceiling\": expected the missing-bus sentence, got {:?}",
        message(&described)
    );
}

/// The tag ceiling and a resource-COUNT quota are different refusal classes: each
/// states its own sentence, so a consumer can tell "too many tags on this
/// resource" from "too many of this resource".
#[tokio::test]
async fn the_tag_ceiling_and_a_resource_count_quota_state_different_sentences() {
    let router = emulator();
    given_bus(&router, "fanning-out").await;
    given_rule(&router, Some("fanning-out"), "fanout").await;

    // The per-rule target quota: a resource COUNT, refused with the count-quota
    // sentence on the modelled quota error.
    let targets: Vec<Value> = (0..=TARGETS_PER_RULE)
        .map(|index| {
            json!({
                "Id": format!("t{index}"),
                "Arn": format!(
                    "arn:aws:sqs:{REGION}:{}:orders",
                    engine::emulator_account_id()
                ),
            })
        })
        .collect();
    let (status, refused) = call(
        &router,
        "PutTargets",
        &json!({ "Rule": "fanout", "EventBusName": "fanning-out", "Targets": targets }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"targets past the per-rule quota\": expected 400, got {status} {refused:?}"
    );
    assert_eq!(
        error_code(&refused),
        "LimitExceededException",
        "case \"targets past the per-rule quota\": expected LimitExceededException, got {:?} \
         {refused:?}",
        error_code(&refused)
    );
    let count_quota = message(&refused).to_string();
    assert_eq!(
        count_quota, COUNT_QUOTA_MESSAGE,
        "case \"targets past the per-rule quota\": expected {COUNT_QUOTA_MESSAGE:?}, got \
         {count_quota:?}"
    );

    // The tag ceiling on the same bus: the other sentence.
    let (status, refused) = call(
        &router,
        "TagResource",
        &json!({ "ResourceARN": bus_arn("fanning-out"), "Tags": tags(0, TAG_QUOTA + 1) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"tags past the per-resource ceiling\": expected 400, got {status} {refused:?}"
    );
    let tag_quota = message(&refused).to_string();
    assert_eq!(
        tag_quota, TAG_QUOTA_MESSAGE,
        "case \"tags past the per-resource ceiling\": expected {TAG_QUOTA_MESSAGE:?}, got \
         {tag_quota:?}"
    );
    assert_ne!(
        tag_quota, count_quota,
        "the two quota classes must not share one sentence, but both state {tag_quota:?}"
    );
}
