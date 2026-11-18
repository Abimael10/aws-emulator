// SPDX-License-Identifier: CC0-1.0

//! The STACK and RESOURCE STATUS-CHANGE EVENTS CloudFormation publishes while NOBODY is
//! reading the stack, observed where a client observes them: on the Amazon SQS queue a rule on
//! the account's default EventBridge bus delivers to.
//!
//! The rule pinned here: **a status change CloudFormation records on a stack reaches the
//! account's default event bus because the change HAPPENED, so every transition a stack
//! operation owes — the ones its own call recorded and the ones the stack's clock makes due
//! after that call has answered — is published without any further call to CloudFormation.**
//! A client driven by these events never polls the stack: it creates a rule, starts the
//! operation and waits for the events, which is the whole point of a rule on the bus.
//!
//! The suite therefore makes exactly ONE CloudFormation call per stage — the call that starts
//! the operation — and reads the stack nowhere: the stack's id comes off `CreateStack`'s own
//! answer, and every later observation is an Amazon SQS receive. A row that only passes
//! because something polled `DescribeStacks` cannot pass here.
//!
//! The rows differ along what the rule generalises over: WHICH operation records the changes (a
//! create, a delete), whether it carried through or rolled back, and how many resources the
//! stack declares. Three crates have to agree — CloudFormation publishes, EventBridge matches
//! each event against the rules of the bus and hands it to their targets, Amazon SQS holds what
//! was delivered, and Amazon S3 owns the resources whose creation succeeds or is refused — so
//! the suite lives here rather than in any one of them, and nothing is asserted on an emulator
//! internal: every observation is a client call.
//!
//! Nothing here waits on the wall clock: each queue is drained again until the events a row
//! expects are on it, under a deadline that is a safety net rather than a measurement.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every call below is signed for, which is the Region the stacks belong to and the
/// Region their events are published in.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which owns the queue the rule delivers to.
const ACCOUNT: &str = "000000000000";

/// The `source` every event CloudFormation publishes carries.
const SOURCE: &str = "aws.cloudformation";

/// The `detail-type` a STACK's own status changes are published under.
const STACK_CHANGE: &str = "CloudFormation Stack Status Change";

/// The `detail-type` one RESOURCE's status changes are published under.
const RESOURCE_CHANGE: &str = "CloudFormation Resource Status Change";

/// The resource type of every resource these templates declare.
const BUCKET_TYPE: &str = "AWS::S3::Bucket";

/// The reason CloudFormation records on the event that OPENS a stack operation.
const USER_INITIATED: &str = "User Initiated";

/// The event a bucket's notification configuration subscribes to. Which event it is does not
/// enter the rule: what matters is that Amazon S3 validates the DESTINATION when the
/// configuration is installed, so a configuration naming a queue it does not hold is refused
/// and the create that asked for it fails.
const SUBSCRIBED_EVENT: &str = "s3:ObjectCreated:*";

/// How long a drain keeps LOOKING, and how often. Generous: the deadline is a safety net,
/// never a measurement — what is asserted is what arrived, and the loop stops the instant its
/// condition holds.
const BUDGET: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it: which service claims
/// each call — CloudFormation's own in-process `PutEvents` and EventBridge's delivery into
/// Amazon SQS included — is decided by the production mount.
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

/// One `(key, value)` request member, owned so a caller can build names like `Parameters.*`.
fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// A queue whose own policy admits the delivering service, and a rule on the default bus that
/// delivers every event matching `pattern` to it: the queue's url.
///
/// "For Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`), so the queue carries the statement that admits
/// `events.amazonaws.com`; the rule is created BEFORE the stack, since an event published while
/// the bus carries no matching rule reaches no target.
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
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": format!("{name}-rule"), "EventPattern": pattern.to_string() }),
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

/// The pattern of the rule a client watching a deployment creates: both status-change
/// detail-types of this service, which is the pair that reports a deployment's progress.
fn every_status_change() -> Value {
    json!({ "source": [SOURCE], "detail-type": [STACK_CHANGE, RESOURCE_CHANGE] })
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
///
/// The CONDITION is what is waited for, not a span, and no CloudFormation call is made while
/// waiting: this is the client that only listens.
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

/// One published event, as the members a rule's pattern filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Published {
    detail_type: String,
    logical_resource_id: String,
    status: String,
    status_reason: String,
    client_request_token: String,
    physical_resource_id: String,
    stack_id: String,
}

/// What one received event states.
fn published(event: &Value) -> Published {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    Published {
        detail_type: text(&event["detail-type"]),
        logical_resource_id: text(&event["detail"]["logical-resource-id"]),
        status: text(&event["detail"]["status-details"]["status"]),
        status_reason: text(&event["detail"]["status-details"]["status-reason"]),
        client_request_token: text(&event["detail"]["client-request-token"]),
        physical_resource_id: text(&event["detail"]["physical-resource-id"]),
        stack_id: text(&event["detail"]["stack-id"]),
    }
}

/// Whether `seen` holds an event of `detail_type` reporting `status` for `logical`.
fn holds(seen: &[Value], detail_type: &str, logical: &str, status: &str) -> bool {
    seen.iter().map(published).any(|event| {
        event.detail_type == detail_type
            && event.logical_resource_id == logical
            && event.status == status
    })
}

/// A template declaring `count` plain buckets named after `prefix`.
fn healthy_template(prefix: &str, count: usize) -> String {
    let mut resources = serde_json::Map::new();
    for index in 0..count {
        resources.insert(
            format!("Bucket{index}"),
            json!({
                "Type": BUCKET_TYPE,
                "Properties": { "BucketName": format!("{prefix}-{index}") },
            }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// A template whose last bucket's notification names a queue Amazon SQS does not hold, so that
/// service refuses the configuration and the create that asked for it fails.
///
/// "After Amazon S3 receives this request, it first verifies that any Amazon Simple Notification
/// Service (Amazon SNS) or Amazon Simple Queue Service (Amazon SQS) destination exists, and that
/// the bucket owner has permission to publish to it"
/// (`PutBucketNotificationConfiguration`).
fn broken_template(prefix: &str, count: usize) -> String {
    let mut resources = serde_json::Map::new();
    for index in 0..count {
        let mut properties = json!({ "BucketName": format!("{prefix}-{index}") });
        if index + 1 == count {
            properties["NotificationConfiguration"] = json!({
                "QueueConfigurations": [{
                    "Queue": format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{prefix}-no-such-queue"),
                    "Event": SUBSCRIBED_EVENT,
                }],
            });
        }
        resources.insert(
            format!("Bucket{index}"),
            json!({ "Type": BUCKET_TYPE, "Properties": properties }),
        );
    }
    json!({ "Resources": Value::Object(resources) }).to_string()
}

/// Which template a row's create carries, which is WHAT decides the create's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Template {
    /// One every owning service accepts.
    Healthy,
    /// One whose last bucket's notification names a queue Amazon SQS does not hold, so the
    /// service that owns the bucket refuses it.
    RefusedByItsOwner,
}

/// One transition a row must see on the bus: the `detail-type` it is published under, the
/// logical resource it is about (the stack's own name, for the stack's own changes) and the
/// status it reports.
struct Expected {
    detail_type: &'static str,
    logical: &'static str,
    status: &'static str,
}

/// One row: the stack, what is done to it, how many resources its template declares, and every
/// transition that must reach the bus with nobody reading the stack.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// How many buckets a bucket template declares; ignored by the wait-condition template,
    /// which declares its own two resources.
    resources: usize,
    template: Template,
    /// Whether the row DELETES the stack once its create has been reported complete.
    deletes: bool,
    /// The stack's own status the row waits for, which is the operation's conclusion.
    concludes_in: &'static str,
    /// The transitions the row must find on the queue once that conclusion is on it.
    expects: &'static [Expected],
}

/// The class the rule covers: a create that carries through, a create one owning service
/// refuses, a create CloudFormation's own wait fails — both of which roll back — and a delete,
/// over templates declaring different numbers and types of resources.
const ROWS: &[Row] = &[
    Row {
        case: "a create every owning service accepts",
        stack: "cfnnopoll-create-ok",
        resources: 2,
        template: Template::Healthy,
        deletes: false,
        concludes_in: "CREATE_COMPLETE",
        expects: &[
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket0",
                status: "CREATE_COMPLETE",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket1",
                status: "CREATE_COMPLETE",
            },
        ],
    },
    Row {
        case: "a create one owning service refuses, which rolls back",
        stack: "cfnnopoll-create-rolled-back",
        resources: 1,
        template: Template::RefusedByItsOwner,
        deletes: false,
        concludes_in: "ROLLBACK_COMPLETE",
        expects: &[Expected {
            detail_type: RESOURCE_CHANGE,
            logical: "Bucket0",
            status: "CREATE_FAILED",
        }],
    },
    Row {
        case: "a three-resource create one owning service refuses, which rolls back",
        stack: "cfnnopoll-create-rolled-back-3",
        resources: 3,
        template: Template::RefusedByItsOwner,
        deletes: false,
        concludes_in: "ROLLBACK_COMPLETE",
        expects: &[
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket0",
                status: "CREATE_COMPLETE",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket1",
                status: "CREATE_COMPLETE",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket2",
                status: "CREATE_FAILED",
            },
            Expected {
                detail_type: STACK_CHANGE,
                logical: "cfnnopoll-create-rolled-back-3",
                status: "ROLLBACK_IN_PROGRESS",
            },
        ],
    },
    Row {
        case: "a delete of a stack that was created healthy",
        stack: "cfnnopoll-delete",
        resources: 3,
        template: Template::Healthy,
        deletes: true,
        concludes_in: "DELETE_COMPLETE",
        expects: &[
            Expected {
                detail_type: STACK_CHANGE,
                logical: "cfnnopoll-delete",
                status: "DELETE_IN_PROGRESS",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket0",
                status: "DELETE_COMPLETE",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket1",
                status: "DELETE_COMPLETE",
            },
            Expected {
                detail_type: RESOURCE_CHANGE,
                logical: "Bucket2",
                status: "DELETE_COMPLETE",
            },
        ],
    },
];

/// The token the delete runs under: "all events triggered by a given stack operation are
/// assigned the same client request token", so it is what a rule scoped to one delete filters
/// on and every event of that delete must carry it.
const DELETE_TOKEN: &str = "cfnnopoll-delete-token";

/// Every status change a stack operation records reaches a rule on the default bus with no
/// CloudFormation read anywhere: the client starts the operation and only listens.
#[tokio::test]
async fn status_changes_reach_the_bus_with_nobody_reading_the_stack() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so a bucket or stack name one row spells cannot decide
        // another row's deployment.
        let router = router();
        let url = wired_queue(
            &router,
            case,
            &format!("{}-bus", row.stack),
            &every_status_change(),
        )
        .await;

        let template = match row.template {
            Template::Healthy => healthy_template(row.stack, row.resources),
            Template::RefusedByItsOwner => broken_template(row.stack, row.resources),
        };
        let created = cloudformation(
            &router,
            case,
            "CreateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &template),
            ],
        )
        .await;
        // The stack's id comes off the create's OWN answer: reading it back would be the very
        // call this suite must not make.
        let stack_id = opt_element(&created, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a StackId, got {created}"));

        let mut seen: Vec<Value> = Vec::new();
        if row.deletes {
            // The delete is asked for once the create has been REPORTED complete — on the bus,
            // which is the only thing this client reads.
            collect_until(&router, case, &url, &mut seen, |seen| {
                holds(seen, STACK_CHANGE, row.stack, "CREATE_COMPLETE")
            })
            .await;
            assert!(
                holds(&seen, STACK_CHANGE, row.stack, "CREATE_COMPLETE"),
                "case {case:?}: the create must be reported complete on the bus, got {:?}",
                seen.iter().map(published).collect::<Vec<_>>()
            );
            seen.clear();
            cloudformation(
                &router,
                case,
                "DeleteStack",
                &[
                    member("StackName", row.stack),
                    member("ClientRequestToken", DELETE_TOKEN),
                ],
            )
            .await;
        }

        // What the bus received, collected until every transition the row expects is on it.
        collect_until(&router, case, &url, &mut seen, |seen| {
            holds(seen, STACK_CHANGE, row.stack, row.concludes_in)
                && row.expects.iter().all(|expected| {
                    holds(
                        seen,
                        expected.detail_type,
                        expected.logical,
                        expected.status,
                    )
                })
        })
        .await;
        let received: Vec<Published> = seen.iter().map(published).collect();

        assert!(
            holds(&seen, STACK_CHANGE, row.stack, row.concludes_in),
            "case {case:?}: the stack's own {:?} must reach the bus with nobody reading the \
             stack, got {received:?}",
            row.concludes_in
        );
        // ...and every other transition the row expects, each under its own detail-type.
        for expected in row.expects {
            assert!(
                holds(
                    &seen,
                    expected.detail_type,
                    expected.logical,
                    expected.status
                ),
                "case {case:?}: {:?} must be reported {:?} under {:?} on the bus, got \
                 {received:?}",
                expected.logical,
                expected.status,
                expected.detail_type
            );
        }
        // ...and every event names the stack it is about, so a rule scoped to one stack id
        // receives this operation and nothing else.
        for event in &received {
            assert_eq!(
                event.stack_id, stack_id,
                "case {case:?}: every event must name the stack id {stack_id:?}, got {event:?}"
            );
        }

        if row.deletes {
            // "All events triggered by a given stack operation are assigned the same client
            // request token": the delete's own token, on every event the delete published.
            for event in &received {
                assert_eq!(
                    event.client_request_token, DELETE_TOKEN,
                    "case {case:?}: every event of the delete must carry {DELETE_TOKEN:?}, got \
                     {event:?}"
                );
            }
            // The event that OPENS the delete carries the reason CloudFormation records for a
            // deletion a client asked for, and names the stack's own physical id.
            let opening = received
                .iter()
                .find(|event| {
                    event.detail_type == STACK_CHANGE && event.status == "DELETE_IN_PROGRESS"
                })
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the delete's opening event, got {received:?}")
                });
            assert_eq!(
                opening.status_reason, USER_INITIATED,
                "case {case:?}: the delete's opening event must state {USER_INITIATED:?}, got \
                 {opening:?}"
            );
            assert_eq!(
                opening.physical_resource_id, stack_id,
                "case {case:?}: the stack's own events name the stack as their physical \
                 resource, got {opening:?}"
            );
            // ...and each resource's deletion names the physical resource it HELD, which is the
            // bucket the create made rather than an empty member.
            for index in 0..row.resources {
                let logical = format!("Bucket{index}");
                let deleted = received
                    .iter()
                    .find(|event| {
                        event.detail_type == RESOURCE_CHANGE
                            && event.logical_resource_id == logical
                            && event.status == "DELETE_COMPLETE"
                    })
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected {logical}'s deletion, got {received:?}")
                    });
                let expected = format!("{}-{index}", row.stack);
                assert_eq!(
                    deleted.physical_resource_id, expected,
                    "case {case:?}: {logical}'s deletion must name the physical id it held \
                     ({expected:?}), got {deleted:?}"
                );
            }
        }
    }
}
