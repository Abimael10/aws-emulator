// SPDX-License-Identifier: CC0-1.0

//! A RAW publish reaches a subscriber whose `Transformer` is a JSONATA one: end to end
//! over the real `MultiServiceRouter`, with Amazon SQS mounted beside EventBridge V2 on
//! ONE engine, so the delivery is asserted where a client observes it — in the message
//! the subscriber's target queue holds.
//!
//! RULE: an event a bus accepts from `PutRawEvents` is owed to every subscriber of that
//! bus exactly as a structured one is, and a `JSONATA` transformer hands the target what
//! its expression produces over that event — evaluated with the function library the
//! language defines, whichever of those functions the expression calls — where the raw
//! entry's `Data` is the document its bytes spell when its `ContentType` declares JSON
//! and the Base64 text of the bytes as sent for every other content type.
//!
//! The rows span that class rather than one recorded case: they differ along the
//! `ContentType` the entry declares (the binary media type, the JSON media type, a
//! `+json` structured-syntax suffix), along the FORM of the bytes (a JSON document,
//! bytes that are no UTF-8 text at all), and along the LIBRARY FUNCTION the expression
//! calls (the type of a value, a mean, an object split into its pairs, the pairs a
//! predicate keeps, a URL's escaping, and a plain path that calls none).
//!
//! Nothing here waits on the wall clock: a publish performs the deliveries it has made
//! due before it answers, so a read that follows it is a read of a settled queue. The
//! one wait is for each bus's own provisioning transition, which is polled for the
//! CONDITION (`ACTIVE`) under a generous deadline.

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
use eventsv2::ops::put_raw_events::PutRawEvents;
use eventsv2::ops::types::{
    CreateEventBusInput, CreateSubscriberInput, DescribeEventBusInput, InvokeConfiguration,
    JsonataConfiguration, PutRawEventsInput, PutRawEventsRequestEntry, PutRawEventsSystemMetadata,
    SqsParameters, Transformer,
};

/// The account segment every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The role a subscriber's `InvokeConfiguration` names. No IAM is mounted here, so
/// nothing is known against it and the delivery is made — what a row is about is the
/// transformation, not what a role may do.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/event-delivery";

/// The `TransformerType` every row configures.
const JSONATA: &str = "JSONATA";

/// How long a poll waits for a condition the emulator owes: far above the provisioning
/// span a bus takes, so a loaded host has ample room.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long one pass of a poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// The emulator and the two wire harnesses
// ---------------------------------------------------------------------------

/// EventBridge V2 and Amazon SQS on ONE engine — the shape `core/router/src/lib.rs`
/// mounts them in, which is what makes a handler's in-process callback reach the other
/// service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// A SigV4 `Authorization` header naming the region and the service, which is how a
/// request names both.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One rpcv2Cbor call against EventBridge V2, requiring a 200 and decoding the typed
/// output.
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

// ---------------------------------------------------------------------------
// The resources one row is built from
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

/// A bus called `name`, created but not yet waited for: its ARN.
async fn given_bus(router: &MultiServiceRouter, case: &str, name: &str) -> String {
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
    created
        .event_bus_arn
        .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus reported no EventBusArn"))
}

/// Wait for the bus at `arn` to be `ACTIVE` — the state `CreateSubscriber` requires of
/// it — polling for that CONDITION under a generous deadline.
async fn once_active(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let described = eventsv2_ok::<DescribeEventBus<InMemoryEngine>>(
            router,
            &DescribeEventBusInput {
                event_bus_arn: arn.to_string(),
            },
            case,
        )
        .await;
        if described.state.as_deref() == Some("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the bus {arn} never became ACTIVE, last state {:?}",
            described.state
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A subscriber of `event_bus_arn` delivering to the queue `target_arn` under the
/// JSONATA transformer `expression`.
async fn given_subscriber(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event_bus_arn: &str,
    target_arn: &str,
    expression: &str,
) {
    eventsv2_ok::<CreateSubscriber<InMemoryEngine>>(
        router,
        &CreateSubscriberInput {
            name: name.to_string(),
            event_bus_arn: event_bus_arn.to_string(),
            invoke_configuration: InvokeConfiguration {
                role_arn: ROLE_ARN.to_string(),
                target_arn: target_arn.to_string(),
                // The parameters a queue target takes, named and left to their
                // defaults, exactly as a client that wants a plain delivery writes
                // them.
                sqs_parameters: Some(SqsParameters::default()),
                ..Default::default()
            },
            transformer: Some(Transformer {
                r#type: Some(JSONATA.to_string()),
                jsonata_configuration: Some(JsonataConfiguration {
                    expression: expression.to_string(),
                }),
            }),
            client_token: Some(format!("token-for-{name}")),
            ..Default::default()
        },
        case,
    )
    .await;
}

/// Publish ONE raw entry onto `event_bus_arn`, requiring it to be accepted, and answer
/// the `EventId` the publish reported for it.
async fn publish_raw(
    router: &MultiServiceRouter,
    case: &str,
    event_bus_arn: &str,
    content_type: &str,
    data: &[u8],
) -> String {
    let answer = eventsv2_ok::<PutRawEvents<InMemoryEngine>>(
        router,
        &PutRawEventsInput {
            event_bus_arn: event_bus_arn.to_string(),
            entries: vec![PutRawEventsRequestEntry {
                data: data.to_vec(),
                metadata: None,
                system_metadata: PutRawEventsSystemMetadata {
                    content_type: content_type.to_string(),
                    deduplication_id: None,
                    event_group_id: None,
                },
            }],
            ..Default::default()
        },
        case,
    )
    .await;
    assert_eq!(
        answer.failed_entry_count, 0,
        "case {case:?}: the entry must be accepted, got {answer:?}"
    );
    answer
        .entries
        .unwrap_or_default()
        .into_iter()
        .next()
        .and_then(|entry| entry.event_id)
        .unwrap_or_else(|| panic!("case {case:?}: an accepted entry must carry an EventId"))
}

/// One pass over the queue at `queue_url`: every message it holds right now, deleted as
/// it is read so a later pass reports what is left rather than what this one took.
async fn one_pass(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Vec<Value> {
    let answer = sqs_ok(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
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

/// EXACTLY one message off the queue at `queue_url`, as its body: polled until it
/// arrives under a generous deadline, and then one more pass, which must find nothing —
/// a queue holding more than the rule allows is a failure too.
async fn one_body(router: &MultiServiceRouter, case: &str, queue_url: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    let mut held: Vec<Value> = Vec::new();
    while held.is_empty() {
        held.extend(one_pass(router, case, queue_url).await);
        if !held.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no delivery ever arrived on {queue_url}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let extra = one_pass(router, case, queue_url).await;
    assert!(
        extra.is_empty() && held.len() == 1,
        "case {case:?}: expected exactly one delivery on {queue_url}, got {} and {} more: \
         {held:?} {extra:?}",
        held.len(),
        extra.len()
    );
    held[0]["Body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a received message carries no Body: {held:?}"))
        .to_string()
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// One row: the case, the entry's `ContentType`, its `Data`, the subscriber's JSONATA
/// expression, and the body the target queue must hold — `None` where the body is the
/// Base64 of the entry's own bytes, which the row cannot spell as a literal.
struct Row {
    case: &'static str,
    content_type: &'static str,
    data: &'static [u8],
    expression: &'static str,
    /// The expected body, or `None` for "the type of the data, a colon, and the Base64
    /// of the bytes as sent" — what the `$type` rows produce for a binary entry.
    body: Option<&'static str>,
}

/// The document the JSON-typed rows publish.
const JSON_DATA: &[u8] = br#"{"marker":"m-1","values":[1,2,3],"a":1,"b":2,"q":"%3Fx%3Dtest"}"#;

/// RULE: an event accepted by `PutRawEvents` is delivered to every subscriber of its
/// bus, and a `JSONATA` transformer hands the target what its expression produces over
/// that event — evaluated with the language's own function library — reading a raw
/// entry's `Data` as the document its bytes spell for a JSON `ContentType` and as the
/// Base64 text of the bytes as sent for any other.
#[tokio::test]
async fn a_raw_publish_reaches_a_jsonata_subscriber() {
    let rows: [Row; 10] = [
        Row {
            case: "binary data whose bytes spell JSON is the Base64 text, not the document",
            content_type: "application/octet-stream",
            data: br#"{"marker": "m-1"}"#,
            expression: "{% $type($events.Data) & ':' & $events.Data %}",
            body: None,
        },
        Row {
            case: "binary data that is no UTF-8 text at all",
            content_type: "application/octet-stream",
            data: &[0xFF, 0xFE, 0x00, 0x01],
            expression: "{% $type($events.Data) & ':' & $events.Data %}",
            body: None,
        },
        Row {
            case: "an empty binary payload",
            content_type: "application/octet-stream",
            data: b"",
            expression: "{% $type($events.Data) & ':' & $events.Data %}",
            body: None,
        },
        Row {
            case: "the JSON media type, whose data is the document",
            content_type: "application/json",
            data: JSON_DATA,
            expression: "{% $type($events.Data) & ':' & $events.Data.marker %}",
            body: Some("object:m-1"),
        },
        Row {
            // The example `PutRawEventsSystemMetadata$ContentType`'s own documentation
            // gives for a structured-syntax suffix.
            case: "a +json structured-syntax suffix, whose data is the document too",
            content_type: "application/cloudevents+json",
            data: JSON_DATA,
            expression: "{% $type($events.Data.values[0]) %}",
            body: Some("number"),
        },
        Row {
            case: "a mean over the data's own members",
            content_type: "application/json",
            data: JSON_DATA,
            expression: "{% $string($average($events.Data.values)) %}",
            body: Some("2"),
        },
        Row {
            case: "the pairs of the data a predicate keeps",
            content_type: "application/json",
            data: br#"{"a":1,"b":2}"#,
            expression: "{% $sift($events.Data, function($v) { $v > 1 }) %}",
            body: Some(r#"{"b":2}"#),
        },
        Row {
            case: "the data split into its pairs",
            content_type: "application/json",
            data: br#"{"only":7}"#,
            expression: "{% $spread($events.Data) %}",
            body: Some(r#"{"only":7}"#),
        },
        Row {
            case: "a URL escape inside the data, resolved",
            content_type: "application/json",
            data: JSON_DATA,
            expression: "{% $decodeUrlComponent($events.Data.q) %}",
            body: Some("?x=test"),
        },
        Row {
            case: "an expression that calls no function at all",
            content_type: "application/json",
            data: JSON_DATA,
            expression: "{% $events.Data.marker %}",
            body: Some("m-1"),
        },
    ];

    let router = emulator();

    // Every bus is created FIRST and then waited for, so the rows share one
    // provisioning span instead of one each.
    let mut resources = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (queue_url, queue_arn) =
            given_queue(&router, row.case, &format!("raw-jsonata-{index}")).await;
        let bus_arn = given_bus(&router, row.case, &format!("raw-jsonata-bus-{index}")).await;
        resources.push((queue_url, queue_arn, bus_arn));
    }

    for (index, row) in rows.iter().enumerate() {
        let (queue_url, queue_arn, bus_arn) = &resources[index];
        once_active(&router, row.case, bus_arn).await;
        given_subscriber(
            &router,
            row.case,
            &format!("raw-jsonata-{index}"),
            bus_arn,
            queue_arn,
            row.expression,
        )
        .await;

        let event_id = publish_raw(&router, row.case, bus_arn, row.content_type, row.data).await;
        assert!(
            !event_id.is_empty(),
            "case {:?}: the publish must report an EventId",
            row.case
        );

        let delivered = one_body(&router, row.case, queue_url).await;
        let expected = match row.body {
            Some(body) => body.to_string(),
            // The Base64 of the bytes as sent, which is how "the event data as a
            // base64-encoded blob" reads for a content type this service does not
            // interpret.
            None => format!("string:{}", aws_smithy_types::base64::encode(row.data)),
        };
        assert_eq!(
            delivered, expected,
            "case {:?}: expected the delivered body {expected:?}, got {delivered:?}",
            row.case
        );
    }
}
