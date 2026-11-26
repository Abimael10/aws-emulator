// SPDX-License-Identifier: CC0-1.0

//! What a device gets from the RETAINED messages of the broker behind the address
//! `DescribeEndpoint(endpointType="iot:Data-ATS")` publishes, driven the whole way a device
//! drives it: the registry identity is provisioned through the control plane, the address is
//! discovered, and the session is spoken on a real connection to it.
//!
//! RULE: the message broker at the account's data endpoint keeps the LAST publication each
//! topic was sent with the RETAIN flag set, hands it to every session that subscribes to a
//! filter covering that topic afterwards with the RETAIN flag SET and at the lower of the
//! published QoS and the granted one, delivers every other publication to the sessions already
//! subscribed with that flag CLEAR, and holds the retained message until a retained publication
//! carrying NO payload clears it — after which that topic has no retained message to read or to
//! replay (MQTT 3.1.1 sections 3.3.1.3 and 3.8.4). The store is the broker's one store, so the
//! IoT data plane's own read of a topic's retained message (`GetRetainedMessage`) answers with
//! what a subscriber is handed, and answers a cleared topic with its not-found refusal.
//!
//! The rows differ along what the rule generalises over: the payload (a JSON document, bytes
//! that are no text at all, a single byte, kilobytes of them), the QoS the retained publication
//! was made at (0, 1), the depth of the topic name, and the half of the published address the
//! session is spoken over (the TLS a device dials the data endpoint with, and the clear).
//!
//! Why HERE, beside the broker's own control plane, rather than in the whole-router suites that
//! cover the retained store over the HTTP publish path
//! (`core/router/tests/all/iot_retained_message_on_subscribe.rs`,
//! `core/router/tests/all/iot_mqtt_retain_will_and_presence.rs`): this suite asserts the rule
//! on the ENCRYPTED half of the published address as well, and the client side of a TLS
//! handshake is a maintained crate's (`rustls`/`tokio-rustls`, declared for this crate's
//! tests) — a test target reaches only the dependencies its own crate manifest declares. The
//! device harness, the two transports and the emulator assembly are the sibling suite's
//! (`tests/all/data_endpoint_mqtt_transport.rs`), shared rather than copied.
//!
//! Nothing here waits on a span: every QoS 1 packet is answered by the broker's own
//! acknowledgement, and the two conditions that have no packet of their own — a QoS 0
//! publication reaching the store, and the store having let a cleared topic go — are polled for
//! until they hold, under a deadline generous enough that a loaded host still passes.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

// The device harness, the two halves of the published address and the emulator assembly, as the
// sibling suite that drives a session on that same address already writes them: `automod`
// declares every suite as a sibling module, so this one shares them rather than carrying a
// second copy.
use crate::data_endpoint_mqtt_transport::{emulator, Delivered, Device, Emulator, Transport};
// The one query/label percent-encoder this test crate has: a `{topic}` path label carries no
// `/` of its own, so a topic name's separators travel encoded, which is how an SDK sends one.
use crate::describe_endpoint::percent_encode;

/// How long a condition the broker makes true may take before a case fails, and how long
/// between two looks at it. Generous: both bound a wait on a CONDITION (a retained message the
/// store took, a topic it let go), not the speed of the host.
const SETTLING_DEADLINE: Duration = Duration::from_secs(60);
/// See [`SETTLING_DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The thing the device below is registered as, the policy its certificate holds, and the
/// policy's own document: the identity the documented path provisions before a device connects
/// — a thing, an ACTIVE certificate, and a policy allowing the connection, the publication of a
/// retained message, the subscription and the delivery.
const THING_NAME: &str = "retained-replay-device";
/// See [`THING_NAME`].
const POLICY_NAME: &str = "retained-replay-policy";

/// The service's fixed answer when a topic holds no retained message, and the error that
/// carries it, written out here rather than taken from the code under test.
const RETAINED_MESSAGE_NOT_FOUND: &str = "Retained message not found";
/// See [`RETAINED_MESSAGE_NOT_FOUND`].
const NOT_FOUND_ERROR: &str = "ResourceNotFoundException";

/// The status `GetRetainedMessage` answers a topic that holds a retained message with, and the
/// one it answers a topic that holds none with (`smithy_query(query="operation",
/// operation="GetRetainedMessage")`: `ResourceNotFoundException` is HTTP 404).
const REPORTED: u16 = 200;
/// See [`REPORTED`].
const ABSENT: u16 = 404;

/// One row: one topic's whole retained-message life, over one half of the published address.
struct Row {
    case: &'static str,
    /// The topic the retained publication is made on. The rows span its depth.
    topic: &'static str,
    /// The bytes the retained publication carries. The rows span their kind and size.
    retained: Vec<u8>,
    /// The QoS the retained publication is made at, which is the ceiling its replay is
    /// delivered at.
    qos: u8,
    /// The half of the published address this row's session is spoken over.
    transport: Transport,
}

impl Row {
    /// The topic a row's own CONTROL message is published on: a sibling of the retained topic,
    /// which the row's last subscriber also subscribes to. A delivery arriving there is what
    /// says the broker has nothing left to replay on the cleared topic, without waiting out a
    /// span to prove an absence.
    fn control_topic(&self) -> String {
        format!("{}/control", self.topic)
    }

    /// The role naming this row's own devices, so each row's client ids are its own.
    fn role(&self) -> String {
        self.topic.replace('/', "-")
    }
}

/// The rows: the payload kinds and sizes a retained message can carry, the two QoS levels AWS
/// IoT Core's broker serves, topic names of three depths, and both halves of the one published
/// address.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a JSON state document retained at QoS 1, over TLS",
            topic: "sim2real/retain/json",
            retained: br#"{"state":"on","v":1}"#.to_vec(),
            qos: 1,
            transport: Transport::Tls,
        },
        Row {
            case: "bytes that are no text at all, retained at QoS 1, over TLS",
            topic: "sim2real/retain/bytes/leaf",
            retained: vec![0x00, 0x01, 0xff, 0x7f],
            qos: 1,
            transport: Transport::Tls,
        },
        Row {
            case: "a single byte retained at QoS 0, over TLS",
            topic: "sim2real/retain/one-byte",
            retained: b"x".to_vec(),
            qos: 0,
            transport: Transport::Tls,
        },
        Row {
            case: "kilobytes of payload retained at QoS 1, in the clear",
            topic: "sim2real/retain/deep/levels/leaf",
            retained: vec![b'p'; 4096],
            qos: 1,
            transport: Transport::Clear,
        },
        Row {
            case: "a single byte retained at QoS 0, in the clear",
            topic: "sim2real/retain/one-byte-clear",
            retained: b"y".to_vec(),
            qos: 0,
            transport: Transport::Clear,
        },
    ]
}

/// One control-plane call that must be served, answered with its document.
async fn control_plane(
    emulator: &Emulator,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> Value {
    let (status, rendered) = emulator
        .send(
            method,
            path,
            "iot",
            "application/json",
            headers,
            body.map(|body| body.to_string().into_bytes())
                .unwrap_or_default(),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: {method} {path} answered {status} with {rendered}"
    );
    serde_json::from_str(&rendered).unwrap_or(Value::Null)
}

/// `GET /retainedMessage/{topic}` through the IoT data plane's `GetRetainedMessage`, as its
/// status and parsed body.
async fn read_retained(emulator: &Emulator, topic: &str) -> (u16, Value) {
    let (status, rendered) = emulator
        .send(
            "GET",
            &format!("/retainedMessage/{}", percent_encode(topic)),
            "iotdata",
            "application/json",
            &[],
            Vec::new(),
        )
        .await;
    (status, serde_json::from_str(&rendered).unwrap_or(Value::Null))
}

/// `GetRetainedMessage` on `topic` once it answers `wanted`, which is the CONDITION a QoS 0
/// publication and a cleared topic are observed through: looked at again until it holds or
/// [`SETTLING_DEADLINE`] passes, never slept through.
async fn retained_once(case: &str, emulator: &Emulator, topic: &str, wanted: u16) -> Value {
    let giving_up_at = Instant::now() + SETTLING_DEADLINE;
    loop {
        let (status, body) = read_retained(emulator, topic).await;
        if status == wanted {
            return body;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected GetRetainedMessage({topic}) to answer {wanted}, got \
             {status} with {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The payload one `GetRetainedMessage` answer carries, decoded: a blob travels base64 in a
/// restJson1 body.
fn reported_payload(case: &str, body: &Value) -> Vec<u8> {
    let encoded = body
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no payload in {body}"));
    aws_smithy_types::base64::decode(encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the payload is not base64: {error}"))
}

/// The modelled error code one refusal names, which restJson1 carries in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence one refusal carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Provision the identity a device connects under, the documented way: a thing, a policy
/// allowing the connection and the retained publication, an ACTIVE certificate, and the two
/// attachments that bind them — each read back through the control plane's own readers, so the
/// registry really holds what the connection below is made with.
async fn provision(emulator: &Emulator) {
    control_plane(
        emulator,
        "POST",
        &format!("/things/{THING_NAME}"),
        &[],
        Some(json!({})),
    )
    .await;

    // The actions AWS IoT Core authorizes a device's own session by: the connection, an
    // ordinary publication, a publication carrying the RETAIN flag, the subscription and the
    // delivery (`CreatePolicy`'s `policyDocument`).
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "iot:Connect",
                "iot:Publish",
                "iot:RetainPublish",
                "iot:Subscribe",
                "iot:Receive",
            ],
            "Resource": "*",
        }],
    })
    .to_string();
    control_plane(
        emulator,
        "POST",
        &format!("/policies/{POLICY_NAME}"),
        &[],
        Some(json!({ "policyDocument": document })),
    )
    .await;

    let issued = control_plane(
        emulator,
        "POST",
        "/keys-and-certificate?setAsActive=true",
        &[],
        None,
    )
    .await;
    let certificate_arn = issued
        .get("certificateArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: CreateKeysAndCertificate reported no ARN: {issued}"))
        .to_string();
    let certificate_id = issued
        .get("certificateId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: CreateKeysAndCertificate reported no id: {issued}"))
        .to_string();

    control_plane(
        emulator,
        "PUT",
        &format!("/target-policies/{POLICY_NAME}"),
        &[],
        Some(json!({ "target": certificate_arn })),
    )
    .await;
    control_plane(
        emulator,
        "PUT",
        &format!("/things/{THING_NAME}/principals"),
        &[("x-amzn-principal", certificate_arn.as_str())],
        None,
    )
    .await;

    // Read back: the certificate the device holds is ACTIVE, the policy is attached to it, and
    // the thing names it as a principal.
    let described = control_plane(
        emulator,
        "GET",
        &format!("/certificates/{certificate_id}"),
        &[],
        None,
    )
    .await;
    assert_eq!(
        described["certificateDescription"]["status"]
            .as_str()
            .unwrap_or_default(),
        "ACTIVE",
        "setup: the certificate a device connects with must be ACTIVE, got {described}"
    );
    let attached = control_plane(
        emulator,
        "POST",
        &format!("/attached-policies/{}", percent_encode(&certificate_arn)),
        &[],
        None,
    )
    .await;
    let policy_names: Vec<String> = attached["policies"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|policy| policy["policyName"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        policy_names.iter().any(|name| name == POLICY_NAME),
        "setup: expected {POLICY_NAME} attached to the certificate, got {policy_names:?}"
    );
    let principals = control_plane(
        emulator,
        "GET",
        &format!("/things/{THING_NAME}/principals"),
        &[],
        None,
    )
    .await;
    let listed: Vec<String> = principals["principals"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|principal| principal.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        listed.contains(&certificate_arn),
        "setup: expected the certificate among the thing's principals, got {listed:?}"
    );
}

/// A retained publication made on the published data endpoint is replayed to a session that
/// subscribes afterwards with the RETAIN flag SET, an ordinary publication reaches an
/// established subscription with the flag CLEAR and leaves the retained message alone, and a
/// retained publication carrying no payload clears the topic — after which the data plane
/// reports the topic as holding no retained message and a fresh subscription is replayed
/// nothing.
#[tokio::test]
async fn a_retained_publication_is_replayed_with_the_retain_flag_until_an_empty_one_clears_it() {
    let emulator = emulator();
    provision(&emulator).await;

    // Where this account's data plane answers, which is the address a device dials — and the
    // call that opens the broker's socket.
    let address = emulator.discover().await;

    for row in rows() {
        let case = row.case;
        let topic = row.topic;
        let control_topic = row.control_topic();
        let role = row.role();

        // The device that publishes: its CONNECT is answered with a CONNACK accepting the
        // session, which is what `Device::connect` asserts.
        let mut publisher =
            Device::connect(row.transport, &address, &format!("{role}-publisher")).await;

        // The retained publication, and what the one store then holds for that topic — read
        // where a client reads it, through the data plane's own retained-message operation.
        publisher
            .publish_at(topic, &row.retained, row.qos, true)
            .await;
        let reported = retained_once(case, &emulator, topic, REPORTED).await;
        assert_eq!(
            reported.get("topic").and_then(Value::as_str),
            Some(topic),
            "case {case:?}: expected the retained message of {topic}, got {reported}"
        );
        assert_eq!(
            reported_payload(case, &reported),
            row.retained,
            "case {case:?}: expected the published bytes retained, got {} byte(s)",
            reported_payload(case, &reported).len()
        );
        assert_eq!(
            reported.get("qos").and_then(Value::as_i64),
            Some(i64::from(row.qos)),
            "case {case:?}: expected the QoS the message was published at, got {reported}"
        );
        assert!(
            reported
                .get("lastModifiedTime")
                .and_then(Value::as_i64)
                .is_some_and(|stored| stored > 0),
            "case {case:?}: expected the instant the message was stored, got {reported}"
        );

        // A session that subscribes AFTERWARDS is handed it, with the RETAIN flag SET and at
        // the lower of the published QoS and the QoS it was granted.
        let mut late = Device::connect(row.transport, &address, &format!("{role}-late")).await;
        let granted = late.subscribe(&[(topic, 1)]).await;
        assert_eq!(
            granted,
            vec![1],
            "case {case:?}: expected the subscription to {topic} granted QoS 1, got {granted:?}"
        );
        let replayed = late.next_delivery().await;
        assert_eq!(
            replayed,
            Delivered {
                topic: topic.to_string(),
                payload: row.retained.clone(),
                qos: row.qos,
                retain: true,
            },
            "case {case:?}: expected the retained message replayed with the RETAIN flag set, \
             got {} byte(s) on {:?} at QoS {} with retain {}",
            replayed.payload.len(),
            replayed.topic,
            replayed.qos,
            replayed.retain
        );

        // An ORDINARY publication reaches that established subscription with the flag CLEAR,
        // and changes nothing about what the topic has retained.
        publisher.publish_at(topic, b"live", 1, false).await;
        let live = late.next_delivery().await;
        assert_eq!(
            live,
            Delivered {
                topic: topic.to_string(),
                payload: b"live".to_vec(),
                qos: 1,
                retain: false,
            },
            "case {case:?}: expected the live publication with the RETAIN flag clear, got \
             {live:?}"
        );
        let (status, unchanged) = read_retained(&emulator, topic).await;
        assert_eq!(
            status, REPORTED,
            "case {case:?}: an ordinary publication must leave the retained message, got \
             {status} with {unchanged}"
        );
        assert_eq!(
            reported_payload(case, &unchanged),
            row.retained,
            "case {case:?}: expected the retained bytes unchanged by an ordinary publication, \
             got {} byte(s)",
            reported_payload(case, &unchanged).len()
        );

        // A retained publication carrying NO payload clears the topic. It is still a
        // publication, so the established subscription is handed it — with the flag clear, like
        // any other live delivery.
        publisher.publish_at(topic, b"", row.qos, true).await;
        let cleared_delivery = late.next_delivery().await;
        assert_eq!(
            cleared_delivery,
            Delivered {
                topic: topic.to_string(),
                payload: Vec::new(),
                qos: row.qos,
                retain: false,
            },
            "case {case:?}: expected the clearing publication delivered to the established \
             subscription, got {cleared_delivery:?}"
        );

        // …and the topic now holds no retained message: the data plane refuses the read under
        // the one code and the one sentence that condition has.
        let refused = retained_once(case, &emulator, topic, ABSENT).await;
        assert_eq!(
            error_code(&refused),
            NOT_FOUND_ERROR,
            "case {case:?}: expected {NOT_FOUND_ERROR} for a cleared topic, got {refused}"
        );
        assert_eq!(
            error_message(&refused),
            RETAINED_MESSAGE_NOT_FOUND,
            "case {case:?}: expected {RETAINED_MESSAGE_NOT_FOUND:?}, got {:?}",
            error_message(&refused)
        );
        assert!(
            !error_message(&refused).contains(topic),
            "case {case:?}: the refusal is one fixed sentence, got {:?}",
            error_message(&refused)
        );

        // And a FRESH session is replayed nothing on that topic: it subscribes to the cleared
        // topic and to a sibling, a control message is published on the sibling, and that
        // control message is the FIRST thing the session is handed — so a retained message
        // replayed where none was owed would have arrived ahead of it.
        let mut fresh = Device::connect(row.transport, &address, &format!("{role}-fresh")).await;
        let granted = fresh
            .subscribe(&[(topic, 1), (control_topic.as_str(), 1)])
            .await;
        assert_eq!(
            granted,
            vec![1, 1],
            "case {case:?}: expected both subscriptions granted QoS 1, got {granted:?}"
        );
        publisher
            .publish_at(&control_topic, b"control", 1, false)
            .await;
        let first = fresh.next_delivery().await;
        assert_eq!(
            first,
            Delivered {
                topic: control_topic.clone(),
                payload: b"control".to_vec(),
                qos: 1,
                retain: false,
            },
            "case {case:?}: expected nothing replayed on the cleared topic and the control \
             message first, got {first:?}"
        );
    }
}
