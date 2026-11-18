// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of an EventBridge rule whose target is an Amazon SNS topic, when the
//! TOPIC's own access policy is the only document in force over that delivery.
//!
//! The rule pinned here: **a delivery EventBridge makes to an SNS topic target is a
//! `sns:Publish` made as the service principal `events.amazonaws.com` on behalf of the rule
//! being invoked ("for Lambda functions, Amazon SNS topics and Amazon SQS queues,
//! EventBridge relies on resource-based policies", `PutTargets`), so the topic's own access
//! policy alone decides it: with no statement allowing that principal that action on that
//! topic the publish does not happen — no subscriber receives anything and the handover is a
//! FAILED invocation — while a statement that does allow it, including one scoped by
//! `aws:SourceArn` to the rule, delivers the event envelope to every subscriber of the
//! topic. Adding that statement with `SetTopicAttributes(AttributeName=Policy)` unblocks the
//! identical rule and target with nothing else changed.**
//!
//! Three services have to agree for that to hold (EventBridge matches and hands over, Amazon
//! SNS fans the published notification out to its subscriptions, Amazon SQS holds what
//! arrived), so the suite lives here rather than in any one crate, and every observation is
//! the one a client makes: a message on the subscribed queue, and — for a delivery that was
//! withheld — the letter on the target's own dead-letter queue, naming the rule and the
//! condition that stopped it.
//!
//! The rows differ along exactly what the rule generalises over: whether a statement names
//! the delivering principal at all, which ACTION it names and how that action is spelled,
//! whether the grant replaces the policy Amazon SNS generated for the topic's owner or is
//! appended to it, which rule or account a condition scopes the grant to and in which
//! operator, whether the elements are written as single values or as lists, and whether an
//! explicit deny is in force beside a grant.
//! `crate::events_fan_out_to_each_matched_target` is the suite for the MULTIPLICITY of a
//! fan-out with every grant already in place; this one is the suite for the topic policy as
//! the GATE on that branch.
//!
//! The last leg drives the same three facts against the emulator a client STARTS rather than
//! a router assembled in this process: this branch is decided by documents several services
//! read, and the delivery is made over the started binary's own credentials and runtime, so a
//! gate that holds in process and not there is a failure a test reports. It starts an
//! emulator of its own rather than the one
//! `crate::cfn_deployed_pipeline_on_a_started_emulator` shares, because it polls queues over
//! spans of seconds and must not spend another suite's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{decoded_entries, element, form_encode};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The account every emulated resource here belongs to.
const ACCOUNT: &str = "000000000000";

/// The `source` every event this suite publishes carries, and the `detail-type` its rules
/// match on.
const SOURCE: &str = "events.sns.target.gate";
const DETAIL_TYPE: &str = "Gate Marker";

/// The service principal EventBridge hands a resource-policy-governed target over as, and the
/// one Amazon SNS fans a notification out to a subscribed queue as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action a publish onto a topic performs — what a grant for this delivery has to name —
/// and an action of the same service that is NOT the one the delivery performs, which a row
/// grants instead to show that it is the ACTION and not merely a statement that decides it.
const PUBLISH_ACTION: &str = "sns:Publish";
const OTHER_SNS_ACTION: &str = "sns:Subscribe";

/// How long an observation that must SEE a message waits for it, how long one that must see
/// NOTHING looks before concluding nothing came, and how often either looks again. Every wait
/// is on a CONDITION: the delivery SPEED is not what this suite asserts, so the budget is
/// generous rather than tight.
const DELIVERY_BUDGET: Duration = Duration::from_secs(30);
const QUIET_BUDGET: Duration = Duration::from_secs(2);
const LOOK_INTERVAL: Duration = Duration::from_millis(20);

/// How long a client's own poll of the subscriber queue asks Amazon SQS to hold the receive
/// open for, inside the range `ReceiveMessageRequest$WaitTimeSeconds` documents ("an integer
/// from 0 to 20"): a poll loop long-polls rather than spinning, so the emulator has to answer
/// a poll that found nothing as well as one that found a delivery.
const LONG_POLL_SECONDS: i64 = 10;

/// The three services this branch runs through, over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
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
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SNS awsQuery call (API version 2010-03-31), as its status and XML body.
async fn sns(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("sns"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon SNS call that must answer 200.
async fn sns_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = sns(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {body}"
    );
    body
}

/// A queue of this account that admits the SNS fan-out of `topic_arn` and nothing else, as
/// `(its url, its ARN)` — the queue policy AWS documents for a queue subscribed to a topic,
/// scoped to that topic with `aws:SourceArn`.
async fn subscriber_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic_arn: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic_arn } },
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// A queue of this account that admits the EventBridge service principal's own send, as
/// `(its url, its ARN)` — what a dead-letter queue of a rule's target has to carry for the
/// letters of a failed invocation to reach it.
async fn dead_letter_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDeadLetters",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// Write `statements` onto the topic `topic_arn` as its access policy, the way a client sets
/// one: `SetTopicAttributes(AttributeName=Policy)`.
async fn set_topic_policy(
    router: &MultiServiceRouter,
    case: &str,
    topic_arn: &str,
    statements: &[Value],
) {
    let policy = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    sns_ok(
        router,
        case,
        "SetTopicAttributes",
        &[
            ("TopicArn", topic_arn),
            ("AttributeName", "Policy"),
            ("AttributeValue", policy.as_str()),
        ],
    )
    .await;
}

/// Take every message waiting on the queue at `url`, deleting what it hands over so a later
/// look reads only what arrived since.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut drained = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        drained.push(serde_json::from_str(body).unwrap_or(Value::Null));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    drained
}

/// Look at the queue at `url` until one notification carrying `marker` has arrived or
/// `budget` has passed, and answer every notification that arrived.
async fn notifications_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    marker: &str,
    budget: Duration,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + budget;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        if arrived.iter().any(|envelope| carries(envelope, marker)) || Instant::now() >= giving_up_at
        {
            return arrived;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Whether the SNS notification `envelope` carries the event marked with `marker`.
fn carries(envelope: &Value, marker: &str) -> bool {
    envelope["Message"]
        .as_str()
        .is_some_and(|message| message.contains(marker))
}

/// Every dead letter waiting on the queue at `url`, as `(the body, its message attributes)`
/// — what a client reads to find out WHY a handover failed.
async fn dead_letters(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    budget: Duration,
) -> Vec<(String, Value)> {
    let giving_up_at = Instant::now() + budget;
    let mut arrived: Vec<(String, Value)> = Vec::new();
    loop {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "MessageAttributeNames": ["All"],
            }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            arrived.push((
                message["Body"].as_str().unwrap_or_default().to_string(),
                message["MessageAttributes"].clone(),
            ));
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
            }
        }
        if !arrived.is_empty() || Instant::now() >= giving_up_at {
            return arrived;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What EventBridge recorded for each handover of a routed event: the target's ARN and the
/// outcome, rendered as `invoked`, `not-attempted` or the failure's code.
fn handovers(engine: &Arc<engine::InMemoryEngine>) -> Vec<(String, String)> {
    use service_com_amazonaws_eventbridge_awsevents::state::InvocationOutcome;
    let state = engine.state::<service_com_amazonaws_eventbridge_awsevents::state::ComAmazonawsEventbridgeAwseventsState>();
    let store = state.lock();
    store
        .invocations
        .iter()
        .map(|invocation| {
            let outcome = match &invocation.outcome {
                InvocationOutcome::Invoked => "invoked".to_string(),
                InvocationOutcome::NotAttempted(reason) => format!("not-attempted: {reason}"),
                InvocationOutcome::Failed { code, .. } => format!("failed: {code}"),
            };
            (invocation.target_arn.clone(), outcome)
        })
        .collect()
}

/// Publish one matching event, marked with `marker`, onto the default bus.
async fn publish_event(router: &MultiServiceRouter, case: &str, marker: &str) {
    let published = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": marker }).to_string(),
            }]
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the event must be accepted, got {published}"
    );
}

/// A rule matching this suite's events with the topic `topic_arn` as its only target, whose
/// failed handovers go to the dead-letter queue `dead_letter_arn` names, as the rule's own
/// ARN.
async fn rule_targeting(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    topic_arn: &str,
    dead_letter_arn: &str,
) -> String {
    let written = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
            })
            .to_string(),
        }),
    )
    .await;
    let arn = written["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {written}"))
        .to_string();
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{
                "Id": "topic",
                "Arn": topic_arn,
                "DeadLetterConfig": { "Arn": dead_letter_arn },
            }]
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the topic target must be accepted, got {targeted}"
    );
    arn
}

/// One row of the topic-policy table: the statements the topic carries, and whether the
/// rule's publish onto it happens.
struct Row {
    case: &'static str,
    /// The statements this row writes onto the topic, built from the topic's ARN and the ARN
    /// of the rule whose delivery is under test.
    statements: fn(&str, &str) -> Vec<Value>,
    /// Whether the row writes its statements BESIDE the ones the topic already carries — the
    /// read-modify-write a client makes when it adds a statement to the policy Amazon SNS
    /// generated for the topic's owner — rather than replacing the document.
    keeps_generated: bool,
    /// Whether the subscriber receives the event the rule published.
    delivered: bool,
}

/// The statements the topic's access policy carries right now, read the way a client reads it
/// before adding one of its own.
async fn generated_statements(
    router: &MultiServiceRouter,
    case: &str,
    topic_arn: &str,
) -> Vec<Value> {
    let answer = sns_ok(
        router,
        case,
        "GetTopicAttributes",
        &[("TopicArn", topic_arn)],
    )
    .await;
    let policy = decoded_entries(&answer)
        .get(TOPIC_POLICY_ATTRIBUTE)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {TOPIC_POLICY_ATTRIBUTE} attribute in {answer}"));
    let document: Value = serde_json::from_str(&policy).unwrap_or_else(|error| {
        panic!("case {case:?}: the topic's policy is a JSON document, got {policy:?} ({error})")
    });
    document["Statement"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The attribute Amazon SNS reports a topic's access policy under.
const TOPIC_POLICY_ATTRIBUTE: &str = "Policy";

/// The topic's own access policy decides the rule's publish onto it, statement by statement.
#[tokio::test]
async fn the_target_topics_policy_decides_the_rules_publish() {
    let rows = [
        Row {
            case: "a topic carrying only the policy Amazon SNS generates for its owner",
            statements: |_topic, _rule| Vec::new(),
            keeps_generated: false,
            delivered: false,
        },
        Row {
            case: "an unconditioned grant of the publish to the delivering principal",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "a grant scoped by aws:SourceArn to the rule being delivered",
            statements: |topic, rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    // The action as the service's own refusals spell it, which IAM compares
                    // case-insensitively.
                    "Action": "SNS:Publish",
                    "Resource": topic,
                    "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "the same grant appended to the policy Amazon SNS generated for the owner",
            statements: |topic, rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": "SNS:Publish",
                    "Resource": topic,
                    "Condition": { "ArnEquals": { "aws:SourceArn": rule } },
                })]
            },
            keeps_generated: true,
            delivered: true,
        },
        Row {
            case: "a grant whose principal, action and resource are each a single-entry list",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": [EVENTS_PRINCIPAL] },
                    "Action": [PUBLISH_ACTION],
                    "Resource": [topic],
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "a grant whose ArnLike condition covers every rule of the account",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                    "Condition": { "ArnLike": {
                        "aws:SourceArn": format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/*"),
                    } },
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "a grant scoped by aws:SourceAccount to the account the rule belongs to",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                    "Condition": { "StringEquals": { "aws:SourceAccount": ACCOUNT } },
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "a grant scoped by aws:SourceAccount to another account",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                    "Condition": { "StringEquals": { "aws:SourceAccount": "210987654321" } },
                })]
            },
            keeps_generated: false,
            delivered: false,
        },
        Row {
            case: "a grant scoped by aws:SourceArn to another rule of the same account",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgePublish",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                    "Condition": { "ArnEquals": {
                        "aws:SourceArn": format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/another"),
                    } },
                })]
            },
            keeps_generated: false,
            delivered: false,
        },
        Row {
            case: "a grant of another action of the same service to the delivering principal",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgeSubscribe",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": OTHER_SNS_ACTION,
                    "Resource": topic,
                })]
            },
            keeps_generated: false,
            delivered: false,
        },
        Row {
            case: "a grant of the publish to another service principal",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowSnsPublish",
                    "Effect": "Allow",
                    "Principal": { "Service": SNS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic,
                })]
            },
            keeps_generated: false,
            delivered: false,
        },
        Row {
            case: "a grant of every action of the service to the delivering principal",
            statements: |topic, _rule| {
                vec![json!({
                    "Sid": "AllowEventBridgeEverything",
                    "Effect": "Allow",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": "sns:*",
                    "Resource": topic,
                })]
            },
            keeps_generated: false,
            delivered: true,
        },
        Row {
            case: "a grant of the publish beside an explicit deny of it",
            statements: |topic, _rule| {
                vec![
                    json!({
                        "Sid": "AllowEventBridgePublish",
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": PUBLISH_ACTION,
                        "Resource": topic,
                    }),
                    json!({
                        "Sid": "DenyEventBridgePublish",
                        "Effect": "Deny",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": PUBLISH_ACTION,
                        "Resource": topic,
                    }),
                ]
            },
            keeps_generated: false,
            delivered: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, engine) = router();
        let name = format!("sns-target-gate-{index}");
        let topic_arn = element(
            &sns_ok(&router, case, "CreateTopic", &[("Name", &name)]).await,
            "TopicArn",
        );
        let (queue_url, queue_arn) = subscriber_queue(&router, case, &name, &topic_arn).await;
        sns_ok(
            &router,
            case,
            "Subscribe",
            &[
                ("TopicArn", topic_arn.as_str()),
                ("Protocol", "sqs"),
                ("Endpoint", queue_arn.as_str()),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;

        // The positive control: the pipe is proved live before anything is asserted about
        // the rule, so an absence afterwards is the rule's branch and not the pipe's.
        let control = format!("control-{index}");
        sns_ok(
            &router,
            case,
            "Publish",
            &[
                ("TopicArn", topic_arn.as_str()),
                ("Message", control.as_str()),
            ],
        )
        .await;
        let seen = notifications_until(&router, case, &queue_url, &control, DELIVERY_BUDGET).await;
        assert!(
            seen.iter().any(|envelope| carries(envelope, &control)),
            "case {case:?}: the topic owner's own publish must reach the subscribed queue, got \
             {seen:?}"
        );

        let (dead_letter_url, dead_letter_arn) =
            dead_letter_queue(&router, case, &format!("{name}-dlq")).await;
        let rule_arn =
            rule_targeting(&router, case, &name, &topic_arn, &dead_letter_arn).await;
        let mut statements = Vec::new();
        if row.keeps_generated {
            statements.extend(generated_statements(&router, case, &topic_arn).await);
        }
        statements.extend((row.statements)(&topic_arn, &rule_arn));
        if !statements.is_empty() {
            set_topic_policy(&router, case, &topic_arn, &statements).await;
        }

        let marker = format!("marker-{index}");
        publish_event(&router, case, &marker).await;
        let budget = if row.delivered {
            DELIVERY_BUDGET
        } else {
            QUIET_BUDGET
        };
        let arrived = notifications_until(&router, case, &queue_url, &marker, budget).await;
        let carried: Vec<&Value> = arrived
            .iter()
            .filter(|envelope| carries(envelope, &marker))
            .collect();
        let recorded = handovers(&engine);
        // The dead-letter queue is looked at the other way round: a row whose delivery was
        // withheld waits for the letter, and one whose delivery was made looks only long
        // enough to conclude that no letter came.
        let letter_budget = if row.delivered {
            QUIET_BUDGET
        } else {
            DELIVERY_BUDGET
        };
        let letters = dead_letters(&router, case, &dead_letter_url, letter_budget).await;
        if row.delivered {
            assert_eq!(
                carried.len(),
                1,
                "case {case:?}: the rule's publish must reach the subscribed queue, got \
                 {arrived:?} with the handovers {recorded:?}"
            );
            let message = carried[0]["Message"].as_str().unwrap_or_default();
            let event: Value = serde_json::from_str(message).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the notification's Message is the event envelope, got \
                     {message:?} ({error})"
                )
            });
            assert_eq!(
                event["detail-type"].as_str(),
                Some(DETAIL_TYPE),
                "case {case:?}: the published detail-type must survive the branch, got {event}"
            );
            assert_eq!(
                recorded,
                vec![(topic_arn.clone(), "invoked".to_string())],
                "case {case:?}: the handover must be recorded as invoked"
            );
            assert!(
                letters.is_empty(),
                "case {case:?}: a delivery that was made is dead-lettered nowhere, got {letters:?}"
            );
        } else {
            assert!(
                carried.is_empty(),
                "case {case:?}: the topic's policy grants the delivering principal no publish, \
                 so nothing must reach the subscribed queue, got {arrived:?}"
            );
            assert_eq!(
                recorded,
                vec![(
                    topic_arn.clone(),
                    format!(
                        "failed: {}",
                        service_com_amazonaws_eventbridge_awsevents::state::NO_PERMISSIONS
                    )
                )],
                "case {case:?}: the withheld publish must be recorded as a failed invocation"
            );
            // The refusal as a CLIENT sees it: the event the rule could not hand over waits
            // on the target's dead-letter queue, naming the rule it was routed by and the
            // condition that stopped it.
            assert_eq!(
                letters.len(),
                1,
                "case {case:?}: the withheld publish must be dead-lettered, got {letters:?}"
            );
            let (body, attributes) = &letters[0];
            assert!(
                body.contains(&marker),
                "case {case:?}: the dead letter carries the event that was not delivered, got \
                 {body:?}"
            );
            assert_eq!(
                attributes["RULE_ARN"]["StringValue"].as_str(),
                Some(rule_arn.as_str()),
                "case {case:?}: the dead letter names the rule, got {attributes}"
            );
            assert_eq!(
                attributes["ERROR_CODE"]["StringValue"].as_str(),
                Some(service_com_amazonaws_eventbridge_awsevents::state::NO_PERMISSIONS),
                "case {case:?}: the dead letter names the condition that stopped the delivery, \
                 got {attributes}"
            );
        }
    }
}

/// The same three facts on the emulator a client STARTS: the built binary, on its own port,
/// with request authentication on, reached over TLS with SigV4-signed calls.
///
/// The in-process legs above drive a router assembled inside this process with authentication
/// off; the emulator a client runs is a different configuration of the same code — the
/// credentials are verified and the deliveries are made on the binary's own multi-thread
/// runtime — so a branch that holds in process and not there is a failure a test reports
/// rather than one only a client sees.
#[test]
fn the_topic_policy_gate_holds_on_the_emulator_a_client_starts() {
    use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

    let case = "the topic policy gate on the emulator a client starts";
    let emulator = Started::start();
    let name = "sns-target-gate-started";

    let started_sqs = |operation: &str, body: &Value| -> Value {
        let (status, text) = emulator.rpc(
            "sqs",
            &format!("AmazonSQS.{operation}"),
            "application/x-amz-json-1.0",
            body,
        );
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {text}; \
             the log:\n{}",
            emulator.log()
        );
        parsed
    };
    let started_form = |signing_name: &str, version: &str, action: &str, params: &[(&str, &str)]| {
        let (status, text) = emulator.form_call(signing_name, version, action, params);
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {signing_name} {action}, got {status} with \
             {text}; the log:\n{}",
            emulator.log()
        );
        text
    };
    let started_events = |operation: &str, body: &Value| -> Value {
        let (status, text) = emulator.rpc(
            "events",
            &format!("AWSEvents.{operation}"),
            "application/x-amz-json-1.1",
            body,
        );
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
             {text}; the log:\n{}",
            emulator.log()
        );
        parsed
    };

    let topic_arn = element(
        &started_form("sns", "2010-03-31", "CreateTopic", &[("Name", name)]),
        "TopicArn",
    );
    let created = started_sqs("CreateQueue", &json!({ "QueueName": name }));
    let queue_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let attributes = started_sqs(
        "GetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {attributes}"))
        .to_string();
    let queue_policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSNSDelivery",
            "Effect": "Allow",
            "Principal": { "Service": SNS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": topic_arn } },
        }],
    })
    .to_string();
    started_sqs(
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": queue_policy } }),
    );
    started_form(
        "sns",
        "2010-03-31",
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    );

    // A client polling a queue for a delivery long-polls it, which is a request the emulator
    // holds open until the wait is up: an empty one answers with no messages rather than
    // hanging, which is what lets a poll loop conclude that nothing came.
    let empty = started_sqs(
        "ReceiveMessage",
        &json!({
            "QueueUrl": queue_url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": LONG_POLL_SECONDS,
        }),
    );
    assert!(
        empty["Messages"]
            .as_array()
            .is_none_or(|messages| messages.is_empty()),
        "case {case:?}: a long poll of a queue nothing has been sent to must answer with no \
         messages, got {empty}; the log:\n{}",
        emulator.log()
    );

    // Everything waited for here is waited for on a CONDITION, looking again until it holds
    // or a generous deadline passes.
    let drain_started = |marker: &str, budget: Duration| -> Vec<Value> {
        let giving_up_at = Instant::now() + budget;
        let mut arrived: Vec<Value> = Vec::new();
        loop {
            let received = started_sqs(
                "ReceiveMessage",
                &json!({
                    "QueueUrl": queue_url,
                    "MaxNumberOfMessages": 10,
                    "WaitTimeSeconds": LONG_POLL_SECONDS,
                }),
            );
            for message in received["Messages"].as_array().cloned().unwrap_or_default() {
                let body = message["Body"].as_str().unwrap_or_default();
                arrived.push(serde_json::from_str(body).unwrap_or(Value::Null));
                if let Some(handle) = message["ReceiptHandle"].as_str() {
                    started_sqs(
                        "DeleteMessage",
                        &json!({ "QueueUrl": queue_url, "ReceiptHandle": handle }),
                    );
                }
            }
            if arrived.iter().any(|envelope| carries(envelope, marker))
                || Instant::now() >= giving_up_at
            {
                return arrived;
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    };

    // (a) The topic owner's own publish is delivered.
    let control = "started-control";
    started_form(
        "sns",
        "2010-03-31",
        "Publish",
        &[("TopicArn", &topic_arn), ("Message", control)],
    );
    let seen = drain_started(control, DELIVERY_BUDGET);
    assert!(
        seen.iter().any(|envelope| carries(envelope, control)),
        "case {case:?}: the topic owner's own publish must reach the subscribed queue, got \
         {seen:?}; the log:\n{}",
        emulator.log()
    );

    // (b) With no grant for the delivering principal, the rule's publish reaches nobody.
    let written = started_events(
        "PutRule",
        &json!({
            "Name": name,
            "State": "ENABLED",
            "EventPattern": json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] })
                .to_string(),
        }),
    );
    let rule_arn = written["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {written}"))
        .to_string();
    let targeted = started_events(
        "PutTargets",
        &json!({ "Rule": name, "Targets": [{ "Id": "topic", "Arn": topic_arn }] }),
    );
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the topic target must be accepted, got {targeted}"
    );
    let withheld = "started-withheld";
    let published = started_events(
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": withheld }).to_string(),
            }]
        }),
    );
    assert_eq!(
        published["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the event must be accepted, got {published}"
    );
    let quiet = drain_started(withheld, QUIET_BUDGET);
    assert!(
        quiet.is_empty(),
        "case {case:?}: with no grant for the delivering principal nothing must be published, \
         got {quiet:?}; the log:\n{}",
        emulator.log()
    );

    // (c) The grant, added with nothing else changed, and the identical rule and target under
    // a fresh marker.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgePublish",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "SNS:Publish",
            "Resource": topic_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    })
    .to_string();
    started_form(
        "sns",
        "2010-03-31",
        "SetTopicAttributes",
        &[
            ("TopicArn", &topic_arn),
            ("AttributeName", "Policy"),
            ("AttributeValue", &policy),
        ],
    );
    let restored = "started-granted";
    let published = started_events(
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": restored }).to_string(),
            }]
        }),
    );
    assert_eq!(
        published["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the event must be accepted, got {published}"
    );
    let arrived = drain_started(restored, DELIVERY_BUDGET);
    assert!(
        arrived.iter().any(|envelope| carries(envelope, restored)),
        "case {case:?}: granting the topic policy must unblock the identical rule and target, \
         got {arrived:?}; the log:\n{}",
        emulator.log()
    );
}

/// The three facts a client has to be able to tell apart on this branch: the topic owner's
/// own publish is delivered, the rule-triggered publish with no grant is delivered to nobody,
/// and the identical rule and target deliver once `SetTopicAttributes` adds the grant.
#[tokio::test]
async fn granting_the_topic_policy_unblocks_the_identical_rule_and_target() {
    let case = "the grant added to a topic whose rule-triggered publish was refused";
    let (router, engine) = router();
    let name = "sns-target-gate-restored";
    let topic_arn = element(
        &sns_ok(&router, case, "CreateTopic", &[("Name", name)]).await,
        "TopicArn",
    );
    let (queue_url, queue_arn) = subscriber_queue(&router, case, name, &topic_arn).await;
    sns_ok(
        &router,
        case,
        "Subscribe",
        &[
            ("TopicArn", topic_arn.as_str()),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn.as_str()),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;

    // (a) The topic owner's own publish is delivered.
    let control = "restored-control";
    sns_ok(
        &router,
        case,
        "Publish",
        &[("TopicArn", topic_arn.as_str()), ("Message", control)],
    )
    .await;
    let seen = notifications_until(&router, case, &queue_url, control, DELIVERY_BUDGET).await;
    assert!(
        seen.iter().any(|envelope| carries(envelope, control)),
        "case {case:?}: the topic owner's own publish must reach the subscribed queue, got \
         {seen:?}"
    );

    // (b) With no grant for the delivering principal, the rule's publish reaches nobody.
    let (dead_letter_url, dead_letter_arn) =
        dead_letter_queue(&router, case, "sns-target-gate-restored-dlq").await;
    let rule_arn = rule_targeting(&router, case, name, &topic_arn, &dead_letter_arn).await;
    let withheld = "restored-withheld";
    publish_event(&router, case, withheld).await;
    let quiet = notifications_until(&router, case, &queue_url, withheld, QUIET_BUDGET).await;
    assert!(
        quiet.is_empty(),
        "case {case:?}: with no grant for the delivering principal nothing must be published, \
         got {quiet:?}"
    );
    let letters = dead_letters(&router, case, &dead_letter_url, QUIET_BUDGET).await;
    assert_eq!(
        letters.len(),
        1,
        "case {case:?}: the withheld publish must be dead-lettered, got {letters:?}"
    );

    // (c) The grant AWS documents for this delivery, added with nothing else changed, and the
    // identical rule and target under a fresh marker.
    set_topic_policy(
        &router,
        case,
        &topic_arn,
        &[json!({
            "Sid": "AllowEventBridgePublish",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "SNS:Publish",
            "Resource": topic_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        })],
    )
    .await;
    let restored = "restored-granted";
    publish_event(&router, case, restored).await;
    let arrived = notifications_until(&router, case, &queue_url, restored, DELIVERY_BUDGET).await;
    let recorded = handovers(&engine);
    assert!(
        arrived.iter().any(|envelope| carries(envelope, restored)),
        "case {case:?}: granting the topic policy must unblock the identical rule and target, \
         got {arrived:?} with the handovers {recorded:?}"
    );
}
