// SPDX-License-Identifier: CC0-1.0

//! WHICH rule on the account's default EventBridge bus receives which CloudFormation
//! status-change event, observed where a client observes it: on the Amazon SQS queue each rule
//! delivers to.
//!
//! The rule pinned here: **an event CloudFormation publishes reaches exactly the rules of the
//! bus whose `EventPattern` matches it, and no others** — so a rule on both status-change
//! detail-types receives a failing create's resource `CREATE_FAILED` and its stack's
//! `ROLLBACK_COMPLETE`; a rule that names the stack detail-type and the status
//! `CREATE_COMPLETE` receives the one stack-level event of the create that completed and
//! neither the resource events of the same status nor the rollback; a rule scoped to one
//! stack's id prefix receives that stack's events and nothing of the other stack's; and a rule
//! on the drift-detection detail-type receives the two events of a detection and no stack or
//! resource change at all.
//!
//! Why it matters: a quiet queue and a bus nothing publishes to look identical from the
//! consumer's side. Each rule here is the POSITIVE control of its own pattern and the near-miss
//! control of the others' — every event this emulator publishes is matched against all four
//! patterns — so a rule that receives too much and a rule that receives nothing both fail.
//!
//! Four crates have to agree — CloudFormation publishes, EventBridge matches each event against
//! the rules of the bus and hands it to their targets, Amazon SQS holds what was delivered, and
//! AWS Systems Manager owns the parameters whose creation succeeds or is refused — so the suite
//! lives here rather than in any one of them, and nothing is asserted on an emulator internal:
//! every observation is a client call.
//!
//! Nothing here waits on the wall clock, and nothing sleeps for a span: each queue is drained
//! again until the events it is owed are on it, under a deadline that is a safety net rather
//! than a measurement. The counted assertions are made once every queue that is owed something
//! has it, so an event delivered to the wrong queue is already on that queue when its count is
//! read.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which owns the queues and the stacks.
const ACCOUNT: &str = "000000000000";

/// The `source` every event CloudFormation publishes carries.
const SOURCE: &str = "aws.cloudformation";

/// The `detail-type` a STACK's own status changes are published under.
const STACK_CHANGE: &str = "CloudFormation Stack Status Change";

/// The `detail-type` one RESOURCE's status changes are published under.
const RESOURCE_CHANGE: &str = "CloudFormation Resource Status Change";

/// The `detail-type` every status of a DRIFT DETECTION is published under.
const DRIFT_CHANGE: &str = "CloudFormation Drift Detection Status Change";

/// The stack whose create is refused by the service that owns one of its resources, so it
/// rolls back.
const ROLLED_BACK_STACK: &str = "cfnprec-rolled-back";

/// The twin stack whose create carries through.
const COMPLETED_STACK: &str = "cfnprec-completed";

/// How long a drain keeps LOOKING, and how often. Generous: the deadline is a safety net,
/// never a measurement — what is asserted is what arrived, and each loop stops the instant its
/// condition holds.
const BUDGET: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation call over its awsQuery wire that must answer 200, as its body.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {text}"
    );
    text
}

/// One `X-Amz-Target` RPC call that must answer 200, as its parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One EventBridge call that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One `(key, value)` request member.
fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// A queue whose own policy admits the delivering service, and a rule on the default bus that
/// delivers every event matching `pattern` to it: the queue's url.
///
/// "For Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`), so the queue carries the statement that admits
/// `events.amazonaws.com`; every rule is created BEFORE the stacks, since an event published
/// while the bus carries no matching rule reaches no target.
async fn wired_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
) -> String {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    let rule = events(
        router,
        case,
        "PutRule",
        &json!({ "Name": format!("{name}-rule"), "EventPattern": pattern.to_string() }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {rule}"))
        .to_string();
    // The grant is scoped to the RULE that delivers, which is how the documented policy for an
    // Amazon SQS target is written: "aws:SourceArn ... the ARN of the rule".
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": format!("{name}-rule"),
            "Targets": [{ "Id": "queue", "Arn": arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// Every event body waiting on the queue at `url`, parsed, and taken off the queue.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message["Body"].as_str())
        .map(|body| serde_json::from_str(body).unwrap_or(Value::Null))
        .collect()
}

/// Drain the queue at `url` into `seen` until `enough` is satisfied or [`BUDGET`] runs out.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut Vec<Value>,
    enough: impl Fn(&[Value]) -> bool,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        seen.extend(drain(router, case, url).await);
        if enough(seen) || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What one received event states, as the members a rule's pattern filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Published {
    detail_type: String,
    stack_id: String,
    logical: String,
    status: String,
    detection_id: String,
    detection_status: String,
}

/// What one received event states.
fn published(event: &Value) -> Published {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    Published {
        detail_type: text(&event["detail-type"]),
        stack_id: text(&event["detail"]["stack-id"]),
        logical: text(&event["detail"]["logical-resource-id"]),
        status: text(&event["detail"]["status-details"]["status"]),
        detection_id: text(&event["detail"]["stack-drift-detection-id"]),
        detection_status: text(&event["detail"]["status-details"]["detection-status"]),
    }
}

/// Whether `seen` holds an event of `detail_type` reporting `status` for `logical`.
fn holds(seen: &[Value], detail_type: &str, logical: &str, status: &str) -> bool {
    seen.iter().map(published).any(|event| {
        event.detail_type == detail_type && event.logical == logical && event.status == status
    })
}

/// The prefix every id of the stack `name` carries: a stack id is
/// `arn:aws:cloudformation:<region>:<account>:stack/<name>/<id>`, so a rule scoped to one stack
/// names everything up to and including that slash — which is knowable before the stack exists,
/// and is how a rule is scoped to a deployment that has not started yet.
fn stack_id_prefix(name: &str) -> String {
    format!("arn:aws:cloudformation:{REGION}:{ACCOUNT}:stack/{name}/")
}

/// A template whose second parameter's value breaks the `AllowedPattern` the declaration
/// states, so AWS Systems Manager refuses it and the create rolls back.
///
/// `AllowedPattern` is "a regular expression used to validate the parameter value"
/// (`PutParameter`), so a value outside it is refused by the service that owns the resource —
/// which fails the resource, and with it the stack operation.
fn refused_template() -> String {
    json!({
        "Resources": {
            "GoodParam": {
                "Type": "AWS::SSM::Parameter",
                "Properties": { "Type": "String", "Value": "42" },
            },
            "RejectedParam": {
                "Type": "AWS::SSM::Parameter",
                "DependsOn": "GoodParam",
                "Properties": {
                    "Type": "String",
                    "Value": "not-a-number",
                    "AllowedPattern": "^[0-9]+$",
                },
            },
        },
    })
    .to_string()
}

/// The twin of [`refused_template`] with nothing for the owning service to refuse.
fn accepted_template() -> String {
    json!({
        "Resources": {
            "GoodParam": {
                "Type": "AWS::SSM::Parameter",
                "Properties": { "Type": "String", "Value": "42" },
            },
        },
    })
    .to_string()
}

/// Each rule of the bus receives exactly the events its own pattern matches: the broad rule the
/// rollback, the status-scoped rule the one stack-level `CREATE_COMPLETE`, the stack-scoped rule
/// one stack's events only, and the drift rule the detection's two events and nothing else.
#[tokio::test]
async fn each_rule_receives_exactly_the_events_its_pattern_matches() {
    let case = "the pattern-precision matrix of one bus";
    let router = router();

    // The four rules, each on the default bus, created before anything is deployed.
    let broad = wired_queue(
        &router,
        case,
        "cfnprec-broad",
        &json!({ "source": [SOURCE], "detail-type": [STACK_CHANGE, RESOURCE_CHANGE] }),
    )
    .await;
    let only_create_complete = wired_queue(
        &router,
        case,
        "cfnprec-create-complete",
        &json!({
            "source": [SOURCE],
            "detail-type": [STACK_CHANGE],
            "detail": { "status-details": { "status": ["CREATE_COMPLETE"] } },
        }),
    )
    .await;
    let only_one_stack = wired_queue(
        &router,
        case,
        "cfnprec-one-stack",
        &json!({
            "source": [SOURCE],
            "detail-type": [STACK_CHANGE, RESOURCE_CHANGE],
            "detail": { "stack-id": [{ "prefix": stack_id_prefix(ROLLED_BACK_STACK) }] },
        }),
    )
    .await;
    let only_drift = wired_queue(
        &router,
        case,
        "cfnprec-drift",
        &json!({ "source": [SOURCE], "detail-type": [DRIFT_CHANGE] }),
    )
    .await;
    // ...and one scoped by the RESOURCES the event names rather than by its detail, which is
    // the other place an event states the stack it is about.
    let only_one_resource = wired_queue(
        &router,
        case,
        "cfnprec-one-resource",
        &json!({
            "source": [SOURCE],
            "resources": [{ "prefix": stack_id_prefix(ROLLED_BACK_STACK) }],
        }),
    )
    .await;

    // The create one owning service refuses, which rolls back...
    let refused = cloudformation(
        &router,
        case,
        "CreateStack",
        &[
            member("StackName", ROLLED_BACK_STACK),
            member("TemplateBody", &refused_template()),
        ],
    )
    .await;
    let rolled_back_id = opt_element(&refused, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {refused}"));
    // ...and the twin that carries through.
    let accepted = cloudformation(
        &router,
        case,
        "CreateStack",
        &[
            member("StackName", COMPLETED_STACK),
            member("TemplateBody", &accepted_template()),
        ],
    )
    .await;
    let completed_id = opt_element(&accepted, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {accepted}"));

    // The broad rule is the positive control for both deployments: it is owed the rollback's
    // own conclusion and the twin's.
    let mut broad_seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &broad, &mut broad_seen, |seen| {
        holds(seen, STACK_CHANGE, ROLLED_BACK_STACK, "ROLLBACK_COMPLETE")
            && holds(seen, STACK_CHANGE, COMPLETED_STACK, "CREATE_COMPLETE")
    })
    .await;
    let broad_received: Vec<Published> = broad_seen.iter().map(published).collect();
    for (logical, status) in [
        (ROLLED_BACK_STACK, "CREATE_IN_PROGRESS"),
        (ROLLED_BACK_STACK, "ROLLBACK_COMPLETE"),
        (COMPLETED_STACK, "CREATE_COMPLETE"),
    ] {
        assert!(
            holds(&broad_seen, STACK_CHANGE, logical, status),
            "case {case:?}: the broad rule must receive {logical}'s {status:?}, got \
             {broad_received:?}"
        );
    }
    assert!(
        holds(&broad_seen, RESOURCE_CHANGE, "RejectedParam", "CREATE_FAILED"),
        "case {case:?}: the broad rule must receive the refused resource's CREATE_FAILED, got \
         {broad_received:?}"
    );

    // The drift detection of the completed stack, started once that stack is reported complete
    // — which the broad rule has already shown.
    let detected = cloudformation(
        &router,
        case,
        "DetectStackDrift",
        &[member("StackName", COMPLETED_STACK)],
    )
    .await;
    let detection_id = opt_element(&detected, "StackDriftDetectionId").unwrap_or_else(|| {
        panic!("case {case:?}: expected a StackDriftDetectionId, got {detected}")
    });

    // The drift rule is the positive control of its own pattern: both statuses of that one
    // detection.
    let mut drift_seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &only_drift, &mut drift_seen, |seen| {
        seen.iter()
            .map(published)
            .any(|event| event.detection_status == "DETECTION_COMPLETE")
    })
    .await;
    let drift_received: Vec<Published> = drift_seen.iter().map(published).collect();
    for status in ["DETECTION_IN_PROGRESS", "DETECTION_COMPLETE"] {
        assert!(
            drift_received
                .iter()
                .any(|event| event.detection_status == status),
            "case {case:?}: the drift rule must receive the detection's {status:?}, got \
             {drift_received:?}"
        );
    }
    // ...and NOTHING else: every event on that queue is a drift status of this detection, about
    // the stack it ran on.
    for event in &drift_received {
        assert_eq!(
            (
                event.detail_type.as_str(),
                event.detection_id.as_str(),
                event.stack_id.as_str()
            ),
            (DRIFT_CHANGE, detection_id.as_str(), completed_id.as_str()),
            "case {case:?}: the drift rule must receive only this detection's own events, got \
             {event:?}"
        );
    }

    // Every queue that is owed something now has it, so what the near-miss queues hold is
    // final: the status-scoped rule received the ONE stack-level CREATE_COMPLETE — not the
    // resource events of the same status, and not the rollback.
    let mut scoped_seen: Vec<Value> = Vec::new();
    collect_until(
        &router,
        case,
        &only_create_complete,
        &mut scoped_seen,
        |seen| !seen.is_empty(),
    )
    .await;
    let scoped_received: Vec<Published> = scoped_seen.iter().map(published).collect();
    assert_eq!(
        scoped_received.len(),
        1,
        "case {case:?}: the CREATE_COMPLETE rule must receive exactly one event, got \
         {scoped_received:?}"
    );
    assert_eq!(
        (
            scoped_received[0].detail_type.as_str(),
            scoped_received[0].status.as_str(),
            scoped_received[0].stack_id.as_str()
        ),
        (STACK_CHANGE, "CREATE_COMPLETE", completed_id.as_str()),
        "case {case:?}: the one event must be the completed stack's own CREATE_COMPLETE, got \
         {scoped_received:?}"
    );

    // ...and the stack-scoped rule received the rolled-back stack's events and NONE of the
    // twin's, however alike the two deployments are.
    let mut one_stack_seen: Vec<Value> = Vec::new();
    collect_until(&router, case, &only_one_stack, &mut one_stack_seen, |seen| {
        holds(seen, STACK_CHANGE, ROLLED_BACK_STACK, "ROLLBACK_COMPLETE")
    })
    .await;
    let one_stack_received: Vec<Published> = one_stack_seen.iter().map(published).collect();
    assert!(
        holds(
            &one_stack_seen,
            STACK_CHANGE,
            ROLLED_BACK_STACK,
            "ROLLBACK_COMPLETE"
        ),
        "case {case:?}: the stack-scoped rule must receive its own stack's conclusion, got \
         {one_stack_received:?}"
    );
    for event in &one_stack_received {
        assert_eq!(
            event.stack_id, rolled_back_id,
            "case {case:?}: the stack-scoped rule must receive nothing of another stack, got \
             {event:?}"
        );
    }

    // ...and the rule scoped by the event's `resources` saw the same stack's changes, so the
    // stack an event names in `resources` is the stack its `detail` is about.
    let mut one_resource_seen: Vec<Value> = Vec::new();
    collect_until(
        &router,
        case,
        &only_one_resource,
        &mut one_resource_seen,
        |seen| holds(seen, STACK_CHANGE, ROLLED_BACK_STACK, "ROLLBACK_COMPLETE"),
    )
    .await;
    let one_resource_received: Vec<Published> = one_resource_seen.iter().map(published).collect();
    assert!(
        holds(
            &one_resource_seen,
            STACK_CHANGE,
            ROLLED_BACK_STACK,
            "ROLLBACK_COMPLETE"
        ),
        "case {case:?}: the resource-scoped rule must receive its own stack's conclusion, got \
         {one_resource_received:?}"
    );
    for event in &one_resource_received {
        assert_eq!(
            event.stack_id, rolled_back_id,
            "case {case:?}: the resource-scoped rule must receive nothing of another stack, got \
             {event:?}"
        );
    }
}
