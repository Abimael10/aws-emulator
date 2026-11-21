// SPDX-License-Identifier: CC0-1.0

//! A subscriber whose target is an Amazon SNS TOPIC, end to end over the real
//! `MultiServiceRouter` with EventBridge V2, Amazon SNS, Amazon SQS and IAM
//! mounted on ONE engine — so every outcome is asserted where a client observes
//! it: in the messages a queue subscribed to the topic holds, and in the records
//! on the resource the subscriber's `OnFailureConfiguration` names.
//!
//! Three rules are asserted, each over a TABLE spanning its class rather than
//! one recorded case:
//!
//! * a subscriber delivers each event its filters select to the topic its
//!   `InvokeConfiguration` names, ONE message per event, carrying the event in
//!   the shape its `Transformer` names — the rows differ along how many events
//!   one publish accepts (including more than one batch holds) and along whether
//!   a filter selects them;
//! * a delivery is made UNDER the role `InvokeConfiguration$RoleArn` names
//!   ("IAM role the service assumes to invoke the target"), so the permissions
//!   in force on that role decide the call: a role whose own documents do not
//!   allow the action the target's call performs delivers nothing, and a role
//!   nothing is known against delivers — the rows differ along what the role's
//!   policy states (the action, the resource, the effect, whether it states a
//!   policy at all) and along the KIND of target, since the action the call
//!   performs follows from that kind;
//! * a delivery that did not happen is recorded where
//!   `OnFailureConfiguration.Arn` names ("where a failed delivery is sent"),
//!   once per event, carrying the event and the failure beside it — the rows
//!   differ along the kind of resource that member names, and along naming none.
//!
//! Nothing here waits on the wall clock: a publish performs the deliveries it
//! has made due before it answers, so a read that follows it is a read of
//! settled queues. The one wait is for the bus's own provisioning transition,
//! which is polled for the CONDITION (`ACTIVE`) under a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_schema::codec::{Codec, FinishSerializer};
use aws_smithy_schema::serde::{SerializableStruct, ShapeSerializer};
use aws_smithy_schema_server::{CborCodec, DeserializableStruct};
use engine::InMemoryEngine;
use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, Operation};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::create_subscriber::CreateSubscriber;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::put_events::PutEvents;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput, Filter,
    FilterConfiguration, InvokeConfiguration, OnFailureConfiguration, PutEventsInput,
    PutEventsRequestEntry,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The service principal a role EventBridge V2 assumes trusts, and the other
/// name the same service is written under — its regional endpoint prefix.
const SERVICE_PRINCIPAL: &str = "events.amazonaws.com";
/// See [`SERVICE_PRINCIPAL`].
const ENDPOINT_PRINCIPAL: &str = "eventsv2.amazonaws.com";

/// The name of the one inline policy a row's role carries, so there is one
/// document deciding each row.
const ROLE_POLICY_NAME: &str = "subscriber-delivery";

/// The sentence the record of a delivery this service could not make states,
/// whose only variable part is the target the delivery was for. Written out here
/// rather than read from the service, so this suite pins the wording a client
/// reads off the record.
fn could_not_access(target_arn: &str) -> String {
    format!(
        "EventBridge could not access {target_arn}. Check that it exists and that the \
         subscriber's delivery role is allowed to access it."
    )
}

/// The condition the record of a delivery the acting identity could not make
/// carries, stated here for the same reason.
const ACCESS_DENIED: &str = "ACCESS_DENIED";

/// The IAM action a delivery to a topic performs, and the one a delivery to a
/// queue performs.
const PUBLISH_ACTION: &str = "sns:Publish";
/// See [`PUBLISH_ACTION`].
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

// ---------------------------------------------------------------------------
// The emulator and the wire harnesses
// ---------------------------------------------------------------------------

/// EventBridge V2, Amazon SNS, Amazon SQS and IAM on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in, which is what makes a handler's
/// in-process callback reach the other services.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 `Authorization` header naming `region` and `service`, which is how a
/// request names both.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One rpcv2Cbor call against EventBridge V2, requiring a 200 and decoding the
/// typed output.
async fn eventsv2_ok<O: Operation>(
    router: &MultiServiceRouter,
    input: &O::Input,
    case: &str,
) -> O::Output
where
    O::Output: DeserializableStruct,
{
    let codec = CborCodec::default();
    let mut serializer = codec.create_serializer();
    serializer
        .write_struct(O::INPUT_SCHEMA, input as &dyn SerializableStruct)
        .unwrap_or_else(|e| panic!("{}: encoding the input must succeed, got {e:?}", O::NAME));
    let body = serializer.finish();

    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
        .header("host", format!("eventsv2.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events"))
        .header("content-type", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        .body(bytes::Bytes::from(body))
        .unwrap_or_else(|e| panic!("{}: the request must be well formed, got {e:?}", O::NAME));

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    assert_eq!(
        status,
        200,
        "case {case:?}: {} must answer 200, got {status} with body {body:?}",
        O::NAME
    );
    let codec = CborCodec::default();
    let mut deserializer = codec.create_deserializer(&body);
    O::Output::deserialize(&mut deserializer).unwrap_or_else(|e| {
        panic!(
            "case {case:?}: {}'s body must decode as its modelled output, got {e:?}",
            O::NAME
        )
    })
}

/// One awsJson1_0 call against Amazon SQS, requiring a 200.
async fn sqs_ok(router: &MultiServiceRouter, operation: &str, body: &Value, case: &str) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: SQS {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// `value` as a form body carries it.
fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// One awsQuery call: the parameters travel as a form body and the answer comes
/// back as XML.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One Amazon SNS call, requiring a 200.
async fn sns_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "sns",
        &format!("sns.{REGION}.amazonaws.com"),
        "2010-03-31",
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SNS {action} must answer 200, got {status} with {body}"
    );
    body
}

/// One IAM call, requiring a 200.
async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "iam",
        &format!("iam.{REGION}.amazonaws.com"),
        "2010-05-08",
        action,
        params,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must answer 200, got {status} with {body}"
    );
    body
}

/// The text of the first `<tag>` of an XML body.
fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

// ---------------------------------------------------------------------------
// The resources a row is built from
// ---------------------------------------------------------------------------

/// A queue called `name`: its url, and the ARN a subscriber names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs_ok(router, "CreateQueue", &json!({ "QueueName": name }), case).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue reported no QueueUrl in {created}"))
        .to_string();
    (url, format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"))
}

/// A queue a TOPIC may deliver to: Amazon SNS decides its own `SendMessage`
/// against the queue's policy, so a queue subscribed to a topic carries one
/// granting that service principal.
async fn given_subscriber_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let (url, arn) = given_queue(router, case, name).await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "sns.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        case,
    )
    .await;
    (url, arn)
}

/// A topic called `name` with the queue `queue_arn` subscribed to it; answers
/// the topic's ARN.
async fn given_topic(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_arn: &str,
) -> String {
    let created = sns_ok(router, case, "CreateTopic", &[("Name", name)]).await;
    let arn = xml_text(&created, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateTopic reported no TopicArn in {created}"));
    sns_ok(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", &arn),
            ("Protocol", "sqs"),
            ("Endpoint", queue_arn),
        ],
    )
    .await;
    arn
}

/// A role called `name` whose trust document admits `trusted`, carrying
/// `statements` as its one inline policy — or no policy at all where the row
/// states none, which is the routine state of a role provisioned out of band.
/// Answers the ARN IAM holds it under.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    trusted: &str,
    statements: Option<Value>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": trusted },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam_ok(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole reported no Arn in {created}"));
    if let Some(statements) = statements {
        let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
        iam_ok(
            router,
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", ROLE_POLICY_NAME),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    }
    arn
}

/// A bus called `name`, polled until it is `ACTIVE` — the state
/// `CreateSubscriber` requires of it.
async fn given_active_bus(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = eventsv2_ok::<CreateEventBus<InMemoryEngine>>(
        router,
        &CreateEventBusInput {
            name: name.to_string(),
            client_token: Some(format!("token-for-{name}")),
            ..Default::default()
        },
        case,
    )
    .await;
    let arn = created
        .event_bus_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"));

    let deadline = Instant::now() + PATIENCE;
    loop {
        let described = eventsv2_ok::<DescribeEventBus<InMemoryEngine>>(
            router,
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
            case,
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the bus {arn} never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one row's subscriber is created with.
struct Wanted<'a> {
    name: &'a str,
    event_bus_arn: &'a str,
    role_arn: &'a str,
    target_arn: &'a str,
    /// The resource a failed delivery is recorded on, when the row names one.
    on_failure_arn: Option<&'a str>,
    /// The pattern the subscriber's one `DATA`-scoped filter carries, when the
    /// row states one.
    filter: Option<&'a str>,
}

/// A subscriber as `wanted` describes it; answers its ARN.
async fn given_subscriber(router: &MultiServiceRouter, case: &str, wanted: Wanted<'_>) -> String {
    let created = eventsv2_ok::<CreateSubscriber<InMemoryEngine>>(
        router,
        &CreateSubscriberInput {
            name: wanted.name.to_string(),
            event_bus_arn: wanted.event_bus_arn.to_string(),
            invoke_configuration: InvokeConfiguration {
                role_arn: wanted.role_arn.to_string(),
                target_arn: wanted.target_arn.to_string(),
                ..Default::default()
            },
            filter_configuration: wanted.filter.map(|pattern| FilterConfiguration {
                language: None,
                filters: Some(vec![Filter {
                    pattern: pattern.to_string(),
                    scope: "DATA".to_string(),
                }]),
            }),
            on_failure_configuration: wanted.on_failure_arn.map(|arn| OnFailureConfiguration {
                arn: Some(arn.to_string()),
            }),
            client_token: Some(format!("token-for-{}", wanted.name)),
            ..Default::default()
        },
        case,
    )
    .await;
    created
        .subscriber_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubscriber reported no SubscriberArn"))
}

/// One event entry whose detail carries `kind`, which a row's filter selects on.
fn entry(index: usize, kind: &str) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: "suite.topic-target".to_string(),
        detail_type: "Delivery".to_string(),
        detail: Some(json!({ "kind": kind, "index": index }).to_string()),
        ..Default::default()
    }
}

/// Publish `entries` onto `event_bus_arn`, requiring every entry to be
/// accepted, and answer the `EventId` the publish reported for each.
async fn publish(
    router: &MultiServiceRouter,
    case: &str,
    event_bus_arn: &str,
    entries: Vec<PutEventsRequestEntry>,
) -> Vec<String> {
    let answer = eventsv2_ok::<PutEvents<InMemoryEngine>>(
        router,
        &PutEventsInput {
            event_bus_arn: event_bus_arn.to_string(),
            entries,
            deduplication_configuration: None,
        },
        case,
    )
    .await;
    assert_eq!(
        answer.failed_entry_count, 0,
        "case {case:?}: every entry must be accepted, got {answer:?}"
    );
    answer
        .entries
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            entry
                .event_id
                .unwrap_or_else(|| panic!("case {case:?}: an accepted entry must carry an EventId"))
        })
        .collect()
}

/// One pass over the queue at `queue_url`: every message it holds right now,
/// each with its attributes, deleted as it is read so a later pass reports what
/// is left rather than what this one took.
async fn one_pass(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Vec<Value> {
    let answer = sqs_ok(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": queue_url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
        case,
    )
    .await;
    let mut read = Vec::new();
    for message in answer["Messages"].as_array().unwrap_or(&Vec::new()) {
        sqs_ok(
            router,
            "DeleteMessage",
            &json!({
                "QueueUrl": queue_url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
            case,
        )
        .await;
        read.push(message.clone());
    }
    read
}

/// EXACTLY `expected` messages off the queue at `queue_url`: polled until that
/// many have been read under a generous deadline, and then one more pass, which
/// must find nothing — a queue holding more than the rule allows is a failure
/// too, and `expected` of 0 is asserted by that pass alone.
async fn messages_on(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    expected: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + PATIENCE;
    let mut held: Vec<Value> = Vec::new();
    while held.len() < expected {
        held.extend(one_pass(router, case, queue_url).await);
        if held.len() >= expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {expected} message(s) on {queue_url}, got {}",
            held.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let extra = one_pass(router, case, queue_url).await;
    assert!(
        extra.is_empty(),
        "case {case:?}: expected exactly {expected} message(s) on {queue_url}, got {} more: \
         {extra:?}",
        extra.len()
    );
    held
}

/// The notification document a queue SUBSCRIBED to a topic received: its body is
/// the envelope Amazon SNS delivers, so what the publisher sent is its
/// `Message`.
fn notification(case: &str, message: &Value) -> Value {
    let body = message["Body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a received message carries no Body: {message}"));
    serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body {body:?} is no document: {error}"))
}

/// `text` as the document it is.
fn document(case: &str, text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: {text:?} is no document: {error}")
    })
}

// ---------------------------------------------------------------------------
// One message per event
// ---------------------------------------------------------------------------

/// What one fan-out row publishes and what the topic must then carry.
struct Fanout {
    case: &'static str,
    /// How many events the publish accepts.
    published: usize,
    /// The subscriber's one `DATA`-scoped filter, when the row states one.
    filter: Option<&'static str>,
    /// How many of those events the subscriber is owed.
    delivered: usize,
}

/// RULE: a subscriber delivers each event its filters select to the topic its
/// `InvokeConfiguration` names, as ONE message per event — so a client watching
/// the topic sees exactly as many messages as the bus accepted matching events,
/// each carrying that event.
#[tokio::test]
async fn each_selected_event_reaches_the_topic_a_subscriber_targets_once() {
    // The rows differ along the number of events one publish accepts — including
    // more than one batch of the topic's API carries — and along whether the
    // subscriber's filter selects them.
    let rows: [Fanout; 5] = [
        Fanout {
            case: "one event and no filter",
            published: 1,
            filter: None,
            delivered: 1,
        },
        Fanout {
            case: "three events and no filter",
            published: 3,
            filter: None,
            delivered: 3,
        },
        Fanout {
            // More events than one `PublishBatch` carries (ten), so the fan-out
            // spans two calls and must still be one message per event.
            case: "twelve events, crossing the batch the topic's API takes",
            published: 12,
            filter: None,
            delivered: 12,
        },
        Fanout {
            case: "six events and a filter selecting the shipped ones",
            published: 6,
            filter: Some(r#"{"kind":["ship"]}"#),
            delivered: 3,
        },
        Fanout {
            case: "six events and a filter selecting none of them",
            published: 6,
            filter: Some(r#"{"kind":["return"]}"#),
            delivered: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = emulator();
        let case = row.case;
        let (queue_url, queue_arn) =
            given_subscriber_queue(&router, case, &format!("topic-reader-{index}")).await;
        let topic_arn = given_topic(&router, case, &format!("fanout-{index}"), &queue_arn).await;
        let role_arn = given_role(
            &router,
            case,
            &format!("fanout-role-{index}"),
            SERVICE_PRINCIPAL,
            Some(json!([{
                "Effect": "Allow",
                "Action": PUBLISH_ACTION,
                "Resource": topic_arn,
            }])),
        )
        .await;
        let bus_arn = given_active_bus(&router, case, &format!("fanout-bus-{index}")).await;
        given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("fanout-{index}"),
                event_bus_arn: &bus_arn,
                role_arn: &role_arn,
                target_arn: &topic_arn,
                on_failure_arn: None,
                filter: row.filter,
            },
        )
        .await;

        let entries: Vec<PutEventsRequestEntry> = (0..row.published)
            .map(|position| {
                entry(
                    position,
                    if position % 2 == 0 { "ship" } else { "hold" },
                )
            })
            .collect();
        let published = publish(&router, case, &bus_arn, entries).await;
        assert_eq!(
            published.len(),
            row.published,
            "case {case:?}: every published entry must report an EventId"
        );

        let received = messages_on(&router, case, &queue_url, row.delivered).await;
        let mut delivered_indexes: Vec<i64> = Vec::new();
        for message in &received {
            let envelope = notification(case, message);
            assert_eq!(
                envelope["TopicArn"],
                json!(topic_arn),
                "case {case:?}: a notification must name the topic it was published to, got \
                 {envelope}"
            );
            let text = envelope["Message"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: the notification carries no Message: {envelope}")
            });
            let event = document(case, text);
            assert_eq!(
                event["source"],
                json!("suite.topic-target"),
                "case {case:?}: the message must carry the published event, got {event}"
            );
            assert_eq!(
                event["detail-type"],
                json!("Delivery"),
                "case {case:?}: the message must carry the published event, got {event}"
            );
            if row.filter.is_some() {
                assert_eq!(
                    event["detail"]["kind"],
                    json!("ship"),
                    "case {case:?}: only an event the filter selects may be delivered, got {event}"
                );
            }
            let position = event["detail"]["index"].as_i64().unwrap_or_else(|| {
                panic!("case {case:?}: the delivered event carries no index: {event}")
            });
            delivered_indexes.push(position);
        }
        delivered_indexes.sort_unstable();
        delivered_indexes.dedup();
        assert_eq!(
            delivered_indexes.len(),
            row.delivered,
            "case {case:?}: each selected event must be delivered exactly once, got \
             {delivered_indexes:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The role the delivery is made under
// ---------------------------------------------------------------------------

/// Which kind of resource a row's subscriber targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Topic,
    Queue,
}

/// What IAM holds for the role a row's subscriber delivers under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Record {
    /// IAM is given the role, with the row's trust document and policy.
    Created,
    /// A role of THIS account IAM was never given.
    Absent,
    /// A role of ANOTHER account, which this service may not act under at all.
    AnotherAccount,
}

/// One row of the authorization table: the role the subscriber delivers under,
/// which kind of target it has, and whether the delivery happens.
struct RoleRow {
    case: &'static str,
    kind: Kind,
    /// The service the role's trust document admits.
    trusted: &'static str,
    /// The statements the role carries, written against the ARN of the row's own
    /// target; `None` leaves the role carrying no policy at all.
    policy: Option<fn(&str) -> Value>,
    /// What IAM holds for the role.
    record: Record,
    delivered: bool,
}

/// RULE: a subscriber's delivery is made UNDER the role its `InvokeConfiguration`
/// names, so what IAM answers about that role decides the call the target's own
/// API takes — this service has to be able to assume it, and the permissions in
/// force on it have to allow the action on the target — and a delivery it does
/// not allow does not happen, the event being recorded on the subscriber's
/// on-failure destination instead. A role IAM knows nothing about withholds
/// nothing.
#[tokio::test]
async fn a_delivery_the_subscribers_role_does_not_allow_reaches_nothing_and_is_recorded() {
    // The rows differ along what the role's trust document admits, along what
    // its permissions policy states (the action, the resource, the effect, and
    // whether it states one at all), along whether IAM holds the role, and along
    // the KIND of target, since the action the delivery performs follows from
    // that kind.
    let rows: [RoleRow; 14] = [
        RoleRow {
            case: "a topic target and a grant of the publish on it",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|target| {
                json!([{ "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": target }])
            }),
            record: Record::Created,
            delivered: true,
        },
        RoleRow {
            case: "a topic target and a grant of every action of its service",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|target| {
                json!([{ "Effect": "Allow", "Action": "sns:*", "Resource": target }])
            }),
            record: Record::Created,
            delivered: true,
        },
        RoleRow {
            case: "a topic target and a grant on every resource",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|_target| {
                json!([{ "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": "*" }])
            }),
            record: Record::Created,
            delivered: true,
        },
        RoleRow {
            // The same service under the other name it is written by, which
            // admits it just as well.
            case: "a topic target and a trust naming this service's endpoint",
            kind: Kind::Topic,
            trusted: ENDPOINT_PRINCIPAL,
            policy: Some(|target| {
                json!([{ "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": target }])
            }),
            record: Record::Created,
            delivered: true,
        },
        RoleRow {
            // IAM answers for this role, and nothing in force on it allows the
            // call: a policy set that says nothing about an action grants
            // nothing.
            case: "a topic target and a role carrying no policy at all",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: None,
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            case: "a topic target and a role this account's IAM does not hold",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: None,
            record: Record::Absent,
            delivered: false,
        },
        RoleRow {
            case: "a topic target and a trust naming another service",
            kind: Kind::Topic,
            trusted: "lambda.amazonaws.com",
            policy: Some(|target| {
                json!([{ "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": target }])
            }),
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            case: "a topic target and a grant on another topic",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|_target| {
                json!([{
                    "Effect": "Allow",
                    "Action": PUBLISH_ACTION,
                    "Resource": format!("arn:aws:sns:{REGION}:{ACCOUNT}:somebody-elses-topic"),
                }])
            }),
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            case: "a topic target and a grant of another service's action",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|_target| {
                json!([{ "Effect": "Allow", "Action": SEND_MESSAGE_ACTION, "Resource": "*" }])
            }),
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            case: "a topic target and a grant the role also denies",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|target| {
                json!([
                    { "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": target },
                    { "Effect": "Deny", "Action": PUBLISH_ACTION, "Resource": target },
                ])
            }),
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            // "Must belong to the calling account" (`InvokeConfiguration$RoleArn`):
            // another account's role is one this service may not act under, and
            // another account's IAM is not this emulator's to ask.
            case: "a topic target and a role of another account",
            kind: Kind::Topic,
            trusted: SERVICE_PRINCIPAL,
            policy: None,
            record: Record::AnotherAccount,
            delivered: false,
        },
        RoleRow {
            case: "a queue target and a grant of the send on it",
            kind: Kind::Queue,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|target| {
                json!([{ "Effect": "Allow", "Action": SEND_MESSAGE_ACTION, "Resource": target }])
            }),
            record: Record::Created,
            delivered: true,
        },
        RoleRow {
            case: "a queue target and a grant of another service's action",
            kind: Kind::Queue,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|_target| {
                json!([{ "Effect": "Allow", "Action": PUBLISH_ACTION, "Resource": "*" }])
            }),
            record: Record::Created,
            delivered: false,
        },
        RoleRow {
            case: "a queue target and a grant the role also denies",
            kind: Kind::Queue,
            trusted: SERVICE_PRINCIPAL,
            policy: Some(|target| {
                json!([
                    { "Effect": "Allow", "Action": SEND_MESSAGE_ACTION, "Resource": target },
                    { "Effect": "Deny", "Action": SEND_MESSAGE_ACTION, "Resource": target },
                ])
            }),
            record: Record::Created,
            delivered: false,
        },
    ];

    /// How many events each row publishes: more than one, so the record of a
    /// window is asserted to point at EVERY event it failed to deliver.
    const PUBLISHED: usize = 2;

    for (index, row) in rows.iter().enumerate() {
        let router = emulator();
        let case = row.case;
        // Where the delivery is observed: the target queue itself for a queue
        // target, and the queue subscribed to the topic for a topic target.
        let (reader_url, reader_arn) =
            given_subscriber_queue(&router, case, &format!("role-reader-{index}")).await;
        let (dead_letter_url, dead_letter_arn) =
            given_queue(&router, case, &format!("role-dlq-{index}")).await;
        let target_arn = match row.kind {
            Kind::Topic => {
                given_topic(&router, case, &format!("role-topic-{index}"), &reader_arn).await
            }
            Kind::Queue => reader_arn.clone(),
        };
        let role_name = format!("role-{index}");
        let role_arn = match row.record {
            Record::Created => {
                given_role(
                    &router,
                    case,
                    &role_name,
                    row.trusted,
                    row.policy.map(|policy| policy(&target_arn)),
                )
                .await
            }
            // The ARN of a role of this account IAM was never given, which is
            // the form the member's own pattern pins.
            Record::Absent => format!("arn:aws:iam::{ACCOUNT}:role/{role_name}"),
            // The same form, naming an account that is not the calling one.
            Record::AnotherAccount => format!("arn:aws:iam::210987654321:role/{role_name}"),
        };
        let bus_arn = given_active_bus(&router, case, &format!("role-bus-{index}")).await;
        let subscriber_arn = given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("role-subscriber-{index}"),
                event_bus_arn: &bus_arn,
                role_arn: &role_arn,
                target_arn: &target_arn,
                on_failure_arn: Some(&dead_letter_arn),
                filter: None,
            },
        )
        .await;

        let entries: Vec<PutEventsRequestEntry> =
            (0..PUBLISHED).map(|position| entry(position, "ship")).collect();
        let published = publish(&router, case, &bus_arn, entries).await;

        let expected_deliveries = usize::from(row.delivered) * PUBLISHED;
        let delivered = messages_on(&router, case, &reader_url, expected_deliveries).await;
        assert_eq!(
            delivered.len(),
            expected_deliveries,
            "case {case:?}: expected {expected_deliveries} delivery(ies) to the target"
        );

        // A window whose deliveries all failed the same way is ONE record
        // listing every event of it.
        let expected_records = usize::from(!row.delivered);
        let recorded = messages_on(&router, case, &dead_letter_url, expected_records).await;
        for message in &recorded {
            let record = document(case, message["Body"].as_str().unwrap_or_default());
            assert_eq!(
                record["errorCode"],
                json!(ACCESS_DENIED),
                "case {case:?}: the record must name the condition that stopped the delivery, \
                 got {record}"
            );
            assert_eq!(
                record["errorMessage"],
                json!(could_not_access(&target_arn)),
                "case {case:?}: the record must carry the sentence of that condition, got \
                 {record}"
            );
            assert_eq!(
                record["subscriberArn"],
                json!(subscriber_arn),
                "case {case:?}: the record must name the subscriber whose delivery failed, got \
                 {record}"
            );
            assert_eq!(
                record["targetArn"],
                json!(target_arn),
                "case {case:?}: the record must name the target it was for, got {record}"
            );
            assert_eq!(
                record["retryAttempts"],
                json!(0),
                "case {case:?}: a refusal of the identity is not retried, got {record}"
            );
            let empty = Vec::new();
            let failed: Vec<&str> = record["failedMessages"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .map(|entry| entry["eventId"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                failed,
                published.iter().map(String::as_str).collect::<Vec<&str>>(),
                "case {case:?}: the record must point at every event that was not delivered, \
                 got {record}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Where a failed delivery is recorded
// ---------------------------------------------------------------------------

/// Where one row records its failed deliveries.
#[derive(Clone, Copy, Debug)]
enum Destination {
    /// A queue, which holds the record as one message.
    Queue,
    /// A topic, whose own subscriptions carry the record.
    Topic,
    /// Nothing: the subscriber configures no `OnFailureConfiguration`.
    None,
}

/// RULE: a delivery that did not happen is recorded where the subscriber's
/// `OnFailureConfiguration.Arn` names — whichever of the kinds that member may
/// name — and a subscriber configuring none records nothing anywhere.
#[tokio::test]
async fn a_failed_delivery_is_recorded_where_the_on_failure_configuration_names() {
    // The rows differ along the kind of resource the member names, and along
    // naming none at all.
    let rows: [(&str, Destination); 3] = [
        ("a queue destination", Destination::Queue),
        ("a topic destination", Destination::Topic),
        ("no destination at all", Destination::None),
    ];

    for (index, (case, destination)) in rows.into_iter().enumerate() {
        let router = emulator();
        // The topic the subscriber targets, which its role may not publish to,
        // read through a queue subscribed to it.
        let (target_reader_url, target_reader_arn) =
            given_subscriber_queue(&router, case, &format!("failure-target-{index}")).await;
        let target_arn =
            given_topic(&router, case, &format!("failure-topic-{index}"), &target_reader_arn).await;
        // Where the record is read: the destination queue itself, or a queue
        // subscribed to the destination topic.
        let (record_url, record_arn) =
            given_subscriber_queue(&router, case, &format!("failure-record-{index}")).await;
        let on_failure_arn = match destination {
            Destination::Queue => Some(record_arn.clone()),
            Destination::Topic => Some(
                given_topic(
                    &router,
                    case,
                    &format!("failure-record-topic-{index}"),
                    &record_arn,
                )
                .await,
            ),
            Destination::None => None,
        };
        let role_arn = given_role(
            &router,
            case,
            &format!("failure-role-{index}"),
            SERVICE_PRINCIPAL,
            // A policy that states something, and nothing about the publish the
            // delivery performs.
            Some(json!([{
                "Effect": "Allow",
                "Action": SEND_MESSAGE_ACTION,
                "Resource": "*",
            }])),
        )
        .await;
        let bus_arn = given_active_bus(&router, case, &format!("failure-bus-{index}")).await;
        let subscriber_arn = given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("failure-subscriber-{index}"),
                event_bus_arn: &bus_arn,
                role_arn: &role_arn,
                target_arn: &target_arn,
                on_failure_arn: on_failure_arn.as_deref(),
                filter: None,
            },
        )
        .await;

        let published = publish(&router, case, &bus_arn, vec![entry(0, "ship")]).await;

        // The target holds nothing in every row: the role does not allow the
        // call its API takes.
        messages_on(&router, case, &target_reader_url, 0).await;

        let expected = usize::from(!matches!(destination, Destination::None));
        let recorded = messages_on(&router, case, &record_url, expected).await;
        for message in &recorded {
            // A record on a QUEUE is the message itself; one on a TOPIC arrives
            // inside the envelope that topic's subscriptions carry.
            let text = match destination {
                Destination::Topic => notification(case, message)["Message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                _ => message["Body"].as_str().unwrap_or_default().to_string(),
            };
            let record = document(case, &text);
            assert_eq!(
                record["errorCode"],
                json!(ACCESS_DENIED),
                "case {case:?}: the record must name the condition that stopped the delivery, \
                 got {record}"
            );
            assert_eq!(
                record["targetArn"],
                json!(target_arn),
                "case {case:?}: the record must name the target it was for, got {record}"
            );
            assert_eq!(
                record["subscriberArn"],
                json!(subscriber_arn),
                "case {case:?}: the record must name the subscriber whose delivery failed, got \
                 {record}"
            );
            assert_eq!(
                record["failedMessages"][0]["eventId"],
                json!(published.first().map(String::as_str).unwrap_or_default()),
                "case {case:?}: the record must point at the event that was not delivered, got \
                 {record}"
            );
        }
    }
}
