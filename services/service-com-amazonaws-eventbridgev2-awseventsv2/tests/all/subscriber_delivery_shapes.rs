// SPDX-License-Identifier: CC0-1.0

//! What a subscriber's CONFIGURATION makes of the event it delivers: the
//! expressions its target parameters carry, the transformation its `Transformer`
//! names, the ordering a FIFO target takes, the forward onto another event bus,
//! and the record a delivery that did not happen leaves.
//!
//! End to end over the real `MultiServiceRouter`, with Amazon SQS (and, where a
//! row is about what a role may do, IAM) mounted beside EventBridge V2 on ONE
//! engine — so every outcome is asserted where a client observes it: in the
//! messages the target queue holds, and in the records on the resource the
//! subscriber's `OnFailureConfiguration` names.
//!
//! The rules, each over a TABLE spanning its class rather than one recorded
//! case:
//!
//! * a configured value of a subscriber's target parameters is "a literal or a
//!   JSONata expression" resolved against the event being delivered, once per
//!   event, and a `BinaryValue` is the "base64-encoded literal binary attribute
//!   value" its member documents — the rows differ along the TYPE the expression
//!   produces, along the part of the event it reads, along the declared data
//!   type and along being a literal at all;
//! * a `JSONATA` transformer delivers what its expression produces over the
//!   event: a string unquoted, every other value its JSON serialization — the
//!   rows differ along the produced type;
//! * a FIFO target takes every delivery in the group the event names, in
//!   publication order;
//! * a subscriber whose target is another event bus publishes the matched event
//!   onto that bus as a new event carrying the metadata its
//!   `EventBusV2Parameters` resolves, so a subscriber of that bus receives it;
//! * a delivery that did not happen is recorded where
//!   `OnFailureConfiguration.Arn` names, under the condition the answer decided
//!   — the identity was refused, or the target is not there.
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
    CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput, EventBusV2Parameters,
    EventBusV2SystemMetadata, InvokeConfiguration, JsonataConfiguration, OnFailureConfiguration,
    PutEventsInput, PutEventsRequestEntry, PutEventsSystemMetadata, RetryPolicy,
    SqsMessageAttributeValue, SqsParameters, Transformer,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names where the row is not
/// about what a role may do. No IAM is mounted for those rows, so nothing is
/// known against it and the delivery is made.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// How long a poll waits for a condition the emulator owes: far above the
/// provisioning span a bus takes, so a loaded host has ample room.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The service principal a role EventBridge V2 delivers under is assumed by.
const SERVICE_PRINCIPAL: &str = "events.amazonaws.com";

// ---------------------------------------------------------------------------
// The emulator and the wire harnesses
// ---------------------------------------------------------------------------

/// EventBridge V2 and Amazon SQS on ONE engine, with IAM beside them when the
/// row needs a role IAM can answer about — the shape `core/router/src/lib.rs`
/// mounts them in, which is what makes a handler's in-process callback reach the
/// other service.
fn emulator(with_iam: bool) -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    if with_iam {
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
    }
    router
}

/// A SigV4 `Authorization` header naming the region and the service, which is
/// how a request names both.
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

/// One IAM call, requiring a 200: the parameters travel as a form body and the
/// answer comes back as XML.
async fn iam_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must answer 200, got {status} with {answered}"
    );
    answered
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

/// A queue called `name`: its url, and the ARN a subscriber names it by. A name
/// ending in `.fifo` makes a FIFO queue, which is what orders its messages.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let attributes = if name.ends_with(".fifo") {
        json!({ "FifoQueue": "true" })
    } else {
        json!({})
    };
    let created = sqs_ok(
        router,
        "CreateQueue",
        &json!({ "QueueName": name, "Attributes": attributes }),
        case,
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue reported no QueueUrl in {created}"))
        .to_string();
    (url, format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"))
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

/// A role called `name` whose trust document admits this service, carrying no
/// policy of its own — the routine state of a role provisioned out of band,
/// which IAM's default deny means allows nothing. Answers its ARN.
async fn given_unprivileged_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
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
    xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole reported no Arn in {created}"))
}

/// What one row's subscriber is created with. Every member left `None` is one
/// the row does not state.
#[derive(Default)]
struct Wanted<'a> {
    name: &'a str,
    event_bus_arn: &'a str,
    target_arn: &'a str,
    role_arn: Option<&'a str>,
    transformer: Option<Transformer>,
    sqs_parameters: Option<SqsParameters>,
    event_bus_parameters: Option<EventBusV2Parameters>,
    on_failure_arn: Option<&'a str>,
    retry_policy: Option<RetryPolicy>,
}

/// A subscriber as `wanted` describes it; answers its ARN.
async fn given_subscriber(router: &MultiServiceRouter, case: &str, wanted: Wanted<'_>) -> String {
    let created = eventsv2_ok::<CreateSubscriber<InMemoryEngine>>(
        router,
        &CreateSubscriberInput {
            name: wanted.name.to_string(),
            event_bus_arn: wanted.event_bus_arn.to_string(),
            invoke_configuration: InvokeConfiguration {
                role_arn: wanted.role_arn.unwrap_or(ROLE_ARN).to_string(),
                target_arn: wanted.target_arn.to_string(),
                sqs_parameters: wanted.sqs_parameters,
                event_bus_v2_parameters: wanted.event_bus_parameters,
                ..Default::default()
            },
            transformer: wanted.transformer,
            retry_policy: wanted.retry_policy,
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

/// One event entry: the payload below is what every expression in this suite
/// reads.
fn entry(detail: Value, group: Option<&str>) -> PutEventsRequestEntry {
    PutEventsRequestEntry {
        source: "suite.delivery-shapes".to_string(),
        detail_type: "Delivery".to_string(),
        detail: Some(detail.to_string()),
        system_metadata: group.map(|group| PutEventsSystemMetadata {
            event_group_id: Some(group.to_string()),
            deduplication_id: None,
        }),
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
            "MessageSystemAttributeNames": ["All"],
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

/// The body of a received message as the document it is.
fn body_document(case: &str, message: &Value) -> Value {
    let body = message["Body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a received message carries no Body: {message}"));
    serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body {body:?} is no document: {error}"))
}

// ---------------------------------------------------------------------------
// A configured value is a literal or an expression over the event
// ---------------------------------------------------------------------------

/// One message-attribute row: the case, the attribute the subscriber configures
/// and what the delivered message must carry under that name.
struct AttributeRow {
    case: &'static str,
    configured: SqsMessageAttributeValue,
    /// The `DataType` and the value the queue reports, or `None` for an
    /// attribute the delivery carries no value for.
    expected: Option<(&'static str, Value)>,
}

/// A `String`-typed attribute carrying `value`.
fn string_attribute(data_type: Option<&str>, value: &str) -> SqsMessageAttributeValue {
    SqsMessageAttributeValue {
        data_type: data_type.map(str::to_string),
        string_value: Some(value.to_string()),
        binary_value: None,
    }
}

/// The payload every row of this suite publishes.
fn payload() -> Value {
    json!({ "orderId": "A-1", "quantity": 3, "lines": [{ "sku": "s-1" }] })
}

/// RULE: a configured value of a subscriber's target parameters is a literal or
/// a `{% %}` JSONata expression resolved against the event being delivered —
/// once per delivered event — and its text is the value the expression produced:
/// a string unquoted, every other value its JSON serialization. A `BinaryValue`
/// is the base64-encoded LITERAL bytes of the attribute, read as no expression
/// at all.
///
/// The rows differ along the type the expression produces, along the part of the
/// event it reads, along the declared data type, along being a literal rather
/// than an expression, and along producing nothing at all.
#[tokio::test]
async fn a_configured_attribute_is_its_expressions_value_or_its_own() {
    // The base64 of `{% not an expression %}`: a BinaryValue is a literal, so
    // the bytes the queue reports are exactly these and no evaluation happens.
    let binary_literal = "eyUgbm90IGFuIGV4cHJlc3Npb24gJX0=";
    let rows: [AttributeRow; 11] = [
        AttributeRow {
            case: "a literal",
            configured: string_attribute(Some("String"), "fixed-value"),
            expected: Some(("String", json!("fixed-value"))),
        },
        AttributeRow {
            case: "an expression over the event's data",
            configured: string_attribute(Some("String"), "{% $events.Data.detail.orderId %}"),
            expected: Some(("String", json!("A-1"))),
        },
        AttributeRow {
            case: "an expression over the event's own envelope members",
            configured: string_attribute(Some("String"), "{% $events.Data.source %}"),
            expected: Some(("String", json!("suite.delivery-shapes"))),
        },
        AttributeRow {
            case: "an expression over the publisher's metadata",
            configured: string_attribute(Some("String"), "{% $events.Metadata.DetailType %}"),
            expected: Some(("String", json!("Delivery"))),
        },
        AttributeRow {
            case: "a number-typed expression, whose value is its own text",
            configured: string_attribute(Some("Number"), "{% $events.Data.detail.quantity %}"),
            expected: Some(("Number", json!("3"))),
        },
        AttributeRow {
            case: "an expression producing a document, serialized",
            configured: string_attribute(Some("String"), "{% $events.Data.detail.lines %}"),
            expected: Some(("String", json!(r#"[{"sku":"s-1"}]"#))),
        },
        AttributeRow {
            case: "a base64 binary literal, which is no expression",
            configured: SqsMessageAttributeValue {
                data_type: Some("Binary".to_string()),
                string_value: None,
                binary_value: Some(binary_literal.to_string()),
            },
            expected: Some(("Binary", json!(binary_literal))),
        },
        AttributeRow {
            case: "an expression the event has no value for",
            configured: string_attribute(Some("String"), "{% $events.Data.detail.missing %}"),
            expected: None,
        },
        AttributeRow {
            case: "a binary type whose base64 the subscriber put in StringValue",
            configured: SqsMessageAttributeValue {
                data_type: Some("Binary".to_string()),
                string_value: Some(binary_literal.to_string()),
                binary_value: None,
            },
            expected: Some(("Binary", json!(binary_literal))),
        },
        AttributeRow {
            case: "a binary type carrying a custom label",
            configured: SqsMessageAttributeValue {
                data_type: Some("Binary.gzip".to_string()),
                string_value: None,
                binary_value: Some(binary_literal.to_string()),
            },
            expected: Some(("Binary.gzip", json!(binary_literal))),
        },
        AttributeRow {
            case: "a value the subscriber left empty, which no attribute may carry",
            configured: string_attribute(Some("String"), ""),
            expected: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = emulator(false);
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("attributes-{index}")).await;
        let bus_arn = given_active_bus(&router, case, &format!("attributes-bus-{index}")).await;
        given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("attributes-{index}"),
                event_bus_arn: &bus_arn,
                target_arn: &queue_arn,
                sqs_parameters: Some(SqsParameters {
                    message_attributes: Some(
                        [("probe".to_string(), row.configured.clone())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

        publish(&router, case, &bus_arn, vec![entry(payload(), None)]).await;
        let delivered = messages_on(&router, case, &queue_url, 1).await;
        let attributes = &delivered[0]["MessageAttributes"];
        match &row.expected {
            Some((data_type, value)) => {
                let member = if data_type.split('.').next() == Some("Binary") {
                    "BinaryValue"
                } else {
                    "StringValue"
                };
                assert_eq!(
                    attributes["probe"]["DataType"],
                    json!(data_type),
                    "case {case:?}: expected the attribute's declared type {data_type:?}, got {attributes}"
                );
                assert_eq!(
                    attributes["probe"][member], *value,
                    "case {case:?}: expected {value} under {member}, got {attributes}"
                );
            }
            None => assert!(
                attributes.get("probe").is_none(),
                "case {case:?}: an attribute whose expression produced nothing must not travel, \
                 got {attributes}"
            ),
        }
    }
}

/// RULE: the NAME an attribute travels under is a configured value too — an
/// attribute map's keys and values are both unconstrained strings, so either
/// side can carry a JSONata expression — so a name written as one is resolved
/// against the event being delivered, and an attribute whose name produced
/// nothing does not travel at all.
///
/// The rows differ along whether the name is a literal, an expression over the
/// event, an expression composing one, and an expression the event has no value
/// for.
#[tokio::test]
async fn a_configured_attribute_name_is_resolved_against_the_event() {
    // (case, the name the subscriber configures, the name the queue reports)
    let rows: [(&str, &str, Option<&str>); 4] = [
        ("a literal name", "plain-name", Some("plain-name")),
        (
            "a name read off the event",
            "{% $events.Data.detail.orderId %}",
            Some("A-1"),
        ),
        (
            "a name composed around a value of the event",
            "{% 'k_' & $events.Data.detail.orderId %}",
            Some("k_A-1"),
        ),
        (
            "a name the event has no value for",
            "{% $events.Data.detail.missing %}",
            None,
        ),
    ];

    for (index, (case, configured, expected)) in rows.into_iter().enumerate() {
        let router = emulator(false);
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("named-attribute-{index}")).await;
        let bus_arn = given_active_bus(&router, case, &format!("named-bus-{index}")).await;
        given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("named-attribute-{index}"),
                event_bus_arn: &bus_arn,
                target_arn: &queue_arn,
                sqs_parameters: Some(SqsParameters {
                    message_attributes: Some(
                        [(
                            configured.to_string(),
                            string_attribute(Some("String"), "carried"),
                        )]
                        .into_iter()
                        .collect(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

        publish(&router, case, &bus_arn, vec![entry(payload(), None)]).await;
        let delivered = messages_on(&router, case, &queue_url, 1).await;
        let attributes = &delivered[0]["MessageAttributes"];
        match expected {
            Some(name) => assert_eq!(
                attributes[name]["StringValue"],
                json!("carried"),
                "case {case:?}: expected the attribute to travel as {name:?}, got {attributes}"
            ),
            None => assert_eq!(
                *attributes,
                Value::Null,
                "case {case:?}: an attribute whose name produced nothing must not travel, got \
                 {attributes}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// A value that applies only to a FIFO target does not travel to a standard one
// ---------------------------------------------------------------------------
/// RULE: a configured value that applies only to a FIFO target —
/// `MessageDeduplicationId`, "message deduplication ID for FIFO queues" — does
/// not travel to a standard target, which takes no such parameter at all, so a
/// subscriber configuring one still delivers to a standard queue.
#[tokio::test]
async fn a_fifo_only_value_does_not_travel_to_a_standard_target() {
    let case = "a standard target under a subscriber configuring FIFO-only values";
    let router = emulator(false);
    let (queue_url, queue_arn) = given_queue(&router, case, "standard-target").await;
    let bus_arn = given_active_bus(&router, case, "standard-bus").await;
    given_subscriber(
        &router,
        case,
        Wanted {
            name: "standard-target",
            event_bus_arn: &bus_arn,
            target_arn: &queue_arn,
            sqs_parameters: Some(SqsParameters {
                message_group_id: Some("{% $events.Data.detail.orderId %}".to_string()),
                message_deduplication_id: Some("{% $events.Data.id %}".to_string()),
                delay_seconds: Some("0".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;

    publish(&router, case, &bus_arn, vec![entry(payload(), None)]).await;
    let delivered = messages_on(&router, case, &queue_url, 1).await;
    assert_eq!(
        body_document(case, &delivered[0])["detail"]["orderId"],
        json!("A-1"),
        "case {case:?}: the event must be delivered, got {:?}",
        delivered[0]
    );
    // The group the subscriber named still travels, since Amazon SQS takes one
    // on a standard queue too.
    assert_eq!(
        delivered[0]["Attributes"]["MessageGroupId"],
        json!("A-1"),
        "case {case:?}: the resolved group must travel, got {:?}",
        delivered[0]
    );
}

// ---------------------------------------------------------------------------
// A JSONATA transformer delivers what its expression produced
// ---------------------------------------------------------------------------

/// RULE: a `JSONATA` transformer hands the target what its
/// `JsonataConfiguration$Expression` produces over the event — a string
/// unquoted, every other value its JSON serialization — and a transformer whose
/// expression produces nothing hands it nothing at all.
///
/// The rows differ along the type the expression produces, which is what the
/// rule generalises over.
#[tokio::test]
async fn a_jsonata_transformer_delivers_what_its_expression_produced() {
    // (case, the expression, the text the target is handed, or `None` for a
    //  transformer that delivers nothing)
    let rows: [(&str, &str, Option<&str>); 6] = [
        (
            "a string, unquoted",
            "{% $events.Data.detail.orderId %}",
            Some("A-1"),
        ),
        (
            "a computed string",
            "{% 'order-' & $events.Data.detail.orderId %}",
            Some("order-A-1"),
        ),
        (
            "an object, serialized",
            r#"{% { "id": $events.Data.detail.orderId, "n": $events.Data.detail.quantity } %}"#,
            Some(r#"{"id":"A-1","n":3}"#),
        ),
        (
            "an array, serialized",
            "{% $events.Data.detail.lines %}",
            Some(r#"[{"sku":"s-1"}]"#),
        ),
        ("a number, serialized", "{% $events.Data.detail.quantity %}", Some("3")),
        ("a path the event has no value for", "{% $events.Data.detail.missing %}", None),
    ];

    for (index, (case, expression, expected)) in rows.into_iter().enumerate() {
        let router = emulator(false);
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("transformed-{index}")).await;
        let bus_arn = given_active_bus(&router, case, &format!("transformed-bus-{index}")).await;
        given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("transformed-{index}"),
                event_bus_arn: &bus_arn,
                target_arn: &queue_arn,
                transformer: Some(Transformer {
                    r#type: Some("JSONATA".to_string()),
                    jsonata_configuration: Some(JsonataConfiguration {
                        expression: expression.to_string(),
                    }),
                }),
                ..Default::default()
            },
        )
        .await;

        publish(&router, case, &bus_arn, vec![entry(payload(), None)]).await;
        let delivered = messages_on(&router, case, &queue_url, usize::from(expected.is_some()))
            .await;
        match expected {
            Some(text) => {
                let body = delivered[0]["Body"].as_str().unwrap_or_default();
                // A document the expression produced is compared AS a
                // document: the order a JSON object's members are serialized in
                // is no part of the value, and only a value that is no document
                // is compared as the text it is.
                match (
                    serde_json::from_str::<Value>(body).ok(),
                    serde_json::from_str::<Value>(text).ok(),
                ) {
                    (Some(delivered_value), Some(expected_value)) => assert_eq!(
                        delivered_value, expected_value,
                        "case {case:?}: expected the body {text:?}, got {body:?}"
                    ),
                    _ => assert_eq!(
                        body, text,
                        "case {case:?}: expected the body {text:?}, got {body:?}"
                    ),
                }
            }
            None => assert!(
                delivered.is_empty(),
                "case {case:?}: a transformer producing nothing delivers nothing, got {delivered:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// A FIFO target keeps the group's publication order
// ---------------------------------------------------------------------------

/// RULE: a delivery to a FIFO target carries the group the event belongs to —
/// the `EventGroupId` its publisher named, or the one the subscriber's
/// `SqsParameters` resolves — so the queue takes every delivery of one group and
/// reports them in publication order.
///
/// The rows differ along where the group comes from: the event's own system
/// metadata, and a subscriber's own expression over the event.
#[tokio::test]
async fn a_fifo_target_takes_every_delivery_in_publication_order() {
    // (case, the MessageGroupId the subscriber configures, the group the
    //  delivered messages must carry)
    let rows: [(&str, Option<&str>, &str); 3] = [
        ("the event's own group", None, "group-1"),
        (
            "a group the subscriber's expression reads off the event",
            Some("{% $events.SystemMetadata.EventGroupId %}"),
            "group-1",
        ),
        (
            "a group the subscriber names as a literal",
            Some("one-group"),
            "one-group",
        ),
    ];
    let order = ["A-1", "A-2", "A-3"];

    for (index, (case, configured, expected_group)) in rows.into_iter().enumerate() {
        let router = emulator(false);
        let (queue_url, queue_arn) =
            given_queue(&router, case, &format!("ordered-{index}.fifo")).await;
        let bus_arn = given_active_bus(&router, case, &format!("ordered-bus-{index}")).await;
        given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("ordered-{index}"),
                event_bus_arn: &bus_arn,
                target_arn: &queue_arn,
                sqs_parameters: Some(SqsParameters {
                    message_group_id: configured.map(str::to_string),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;

        // Published one at a time, so the order the queue reports is the order
        // the events were accepted in rather than one batch's own.
        for id in order {
            publish(
                &router,
                case,
                &bus_arn,
                vec![entry(json!({ "orderId": id }), Some("group-1"))],
            )
            .await;
        }

        let delivered = messages_on(&router, case, &queue_url, order.len()).await;
        let ids: Vec<String> = delivered
            .iter()
            .map(|message| {
                body_document(case, message)["detail"]["orderId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids, order,
            "case {case:?}: a FIFO target must report the group in publication order, got {ids:?}"
        );
        for message in &delivered {
            assert_eq!(
                message["Attributes"]["MessageGroupId"],
                json!(expected_group),
                "case {case:?}: expected the group {expected_group:?}, got {message}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// An event-bus target forwards onto the downstream bus
// ---------------------------------------------------------------------------

/// RULE: a subscriber whose target is another event bus publishes each matched
/// event onto that bus as a NEW event of it, carrying the metadata its
/// `EventBusV2Parameters` resolves against the event — so a subscriber of the
/// downstream bus receives it, and the event is an event of that bus with its
/// own identity.
#[tokio::test]
async fn an_event_bus_target_forwards_the_event_onto_the_downstream_bus() {
    let case = "a forward carrying resolved metadata";
    let router = emulator(false);
    let (queue_url, queue_arn) = given_queue(&router, case, "forwarded-reader").await;
    let upstream = given_active_bus(&router, case, "forward-upstream").await;
    let downstream = given_active_bus(&router, case, "forward-downstream").await;

    // The subscriber of the DOWNSTREAM bus, which is what observes the forward:
    // its metadata envelope is where the forwarded metadata is read.
    given_subscriber(
        &router,
        case,
        Wanted {
            name: "forwarded-reader",
            event_bus_arn: &downstream,
            target_arn: &queue_arn,
            transformer: Some(Transformer {
                r#type: Some("WITH_METADATA".to_string()),
                jsonata_configuration: None,
            }),
            ..Default::default()
        },
    )
    .await;
    given_subscriber(
        &router,
        case,
        Wanted {
            name: "forwarder",
            event_bus_arn: &upstream,
            target_arn: &downstream,
            event_bus_parameters: Some(EventBusV2Parameters {
                metadata: Some(
                    [
                        ("stage".to_string(), "forwarded".to_string()),
                        (
                            "order".to_string(),
                            "{% $events.Data.detail.orderId %}".to_string(),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
                system_metadata: Some(EventBusV2SystemMetadata {
                    event_group_id: Some("{% $events.Data.detail.orderId %}".to_string()),
                    deduplication_id: None,
                }),
                deduplication_configuration: None,
            }),
            ..Default::default()
        },
    )
    .await;

    let published = publish(&router, case, &upstream, vec![entry(payload(), None)]).await;
    let delivered = messages_on(&router, case, &queue_url, 1).await;
    let body = body_document(case, &delivered[0]);

    assert_eq!(
        body["Data"]["detail"]["orderId"],
        json!("A-1"),
        "case {case:?}: the forwarded event must carry the event that was published, got {body}"
    );
    assert_eq!(
        body["Metadata"]["stage"],
        json!("forwarded"),
        "case {case:?}: a literal of the forwarding metadata must travel, got {body}"
    );
    assert_eq!(
        body["Metadata"]["order"],
        json!("A-1"),
        "case {case:?}: an expression of the forwarding metadata must be resolved against the \
         event, got {body}"
    );
    assert_eq!(
        body["SystemMetadata"]["EventGroupId"],
        json!("A-1"),
        "case {case:?}: the forwarded event's own system metadata must be resolved too, got {body}"
    );
    let forwarded_id = body["SystemMetadata"][eventsv2::state::SYSTEM_EVENT_ID]
        .as_str()
        .unwrap_or_default();
    assert!(
        !forwarded_id.is_empty() && forwarded_id != published[0],
        "case {case:?}: a forwarded event is a new event of the downstream bus, so it carries an \
         identity of its own, got {forwarded_id:?} against {:?}",
        published[0]
    );
}

// ---------------------------------------------------------------------------
// A delivery that did not happen leaves a record
// ---------------------------------------------------------------------------

/// RULE: a delivery that did not happen is recorded where the subscriber's
/// `OnFailureConfiguration.Arn` names, naming the bus, the subscriber, the
/// target, the events that were not delivered and the CONDITION the answer
/// decided: the identity was refused, or the target is not there. The target
/// itself holds nothing.
///
/// The rows differ along which of those two conditions ended the delivery, which
/// is what the rule generalises over.
#[tokio::test]
async fn a_delivery_that_did_not_happen_is_recorded_with_its_condition() {
    // (case, whether the target queue is deleted before the publish, the
    //  condition the record must name)
    let rows: [(&str, bool, &str); 2] = [
        (
            "a role whose documents allow nothing",
            false,
            "ACCESS_DENIED",
        ),
        ("a target queue that is not there", true, "RESOURCE_NOT_FOUND"),
    ];

    for (index, (case, delete_target, condition)) in rows.into_iter().enumerate() {
        // The ACCESS_DENIED row needs an IAM to answer about the role; the
        // RESOURCE_NOT_FOUND row is decided by the target's own service, so
        // nothing is known against its role.
        let router = emulator(!delete_target);
        let (target_url, target_arn) = given_queue(&router, case, &format!("dlq-target-{index}"))
            .await;
        let (dlq_url, dlq_arn) = given_queue(&router, case, &format!("dlq-{index}")).await;
        let bus_arn = given_active_bus(&router, case, &format!("dlq-bus-{index}")).await;
        let role_arn = if delete_target {
            None
        } else {
            Some(given_unprivileged_role(&router, case, &format!("dlq-role-{index}")).await)
        };
        let subscriber_arn = given_subscriber(
            &router,
            case,
            Wanted {
                name: &format!("dlq-{index}"),
                event_bus_arn: &bus_arn,
                target_arn: &target_arn,
                role_arn: role_arn.as_deref(),
                on_failure_arn: Some(&dlq_arn),
                retry_policy: Some(RetryPolicy {
                    max_retry_attempts: Some(0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await;
        if delete_target {
            sqs_ok(
                &router,
                "DeleteQueue",
                &json!({ "QueueUrl": target_url }),
                case,
            )
            .await;
        }

        let published = publish(&router, case, &bus_arn, vec![entry(payload(), None)]).await;
        let recorded = messages_on(&router, case, &dlq_url, 1).await;
        let record = body_document(case, &recorded[0]);

        assert_eq!(
            record["busArn"],
            json!(bus_arn),
            "case {case:?}: the record must name the bus the event was published to, got {record}"
        );
        assert_eq!(
            record["subscriberArn"],
            json!(subscriber_arn),
            "case {case:?}: the record must name the subscriber, got {record}"
        );
        assert_eq!(
            record["targetArn"],
            json!(target_arn),
            "case {case:?}: the record must name the target, got {record}"
        );
        assert_eq!(
            record["errorCode"],
            json!(condition),
            "case {case:?}: expected the condition {condition:?}, got {record}"
        );
        assert_eq!(
            record["errorMessage"],
            json!(format!(
                "EventBridge could not access {target_arn}. Check that it exists and that the \
                 subscriber's delivery role is allowed to access it."
            )),
            "case {case:?}: the record must state the sentence of its class, got {record}"
        );
        assert_eq!(
            record["retryAttempts"],
            json!(0),
            "case {case:?}: the record must name the retries spent, got {record}"
        );
        assert_eq!(
            record["exhaustedRetryCondition"],
            json!("MaximumRetryAttempts"),
            "case {case:?}: the record must name the retry bound that was exhausted, got {record}"
        );
        assert_eq!(
            record["failedMessages"][0]["eventId"],
            json!(published[0]),
            "case {case:?}: the record must name the event that was not delivered, got {record}"
        );
        if !delete_target {
            // The target holds nothing: a delivery that did not happen must not
            // be reported as one that did.
            assert!(
                messages_on(&router, case, &target_url, 0).await.is_empty(),
                "case {case:?}: the target must hold nothing"
            );
        }
    }
}
