// SPDX-License-Identifier: CC0-1.0

//! What a message a DEVICE publishes over its own MQTT connection does to the account's topic
//! rules, and what the Basic Ingest topic space does differently.
//!
//! RULE: a publication the message broker takes is evaluated by the rules engine whatever wire
//! it arrived on, so every enabled topic rule whose statement selects it performs each of its
//! actions for the message that statement selected — for an `sqs` action, one send of that
//! message to the queue the action's `queueUrl` addresses under its own role. A publication
//! addressed to the reserved Basic Ingest space `$aws/rules/<ruleName>/<topic>` is routed to
//! the rule that name identifies INSTEAD of to the rules whose statements cover the topic — the
//! levels after the rule name are the topic its statement reads — and it is published to no
//! subscriber at all, not even one subscribed to the reserved topic itself, which is what makes
//! the path cheaper than an ordinary publication.
//!
//! Four crates have to agree for that to hold — the IoT control plane publishes the broker's
//! address and holds the rules, the broker itself serves the MQTT connection, IAM holds the
//! action's role and Amazon SQS takes the send — so the suite lives here: a crate-local suite
//! can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the publication is ordinary or
//! addressed to a rule by name; how many levels follow the rule name (a topic the named rule's
//! own `FROM` clause covers, one it does not, and none at all); whether the rule named exists,
//! and whether it is enabled; what the rule's statement SELECTS (the whole message, and a
//! projection under a condition that holds and one that does not); and a topic no rule covers
//! at all. Every row asserts BOTH what the queue holds and what the subscriber was handed, so a
//! publication that fired the rule without bypassing the subscribers — or bypassed them without
//! firing the rule — fails here.
//!
//! Neither assertion waits on a span: each row publishes a SENTINEL message after its own, on
//! an ordinary topic whose rule is known to forward, and the row is judged once that sentinel
//! has arrived at the subscriber and in the queue. The connection is spoken over a real TCP
//! socket to the published address with the packets laid out by the same maintained MQTT codec
//! a client library uses. Assembling the router costs stack (see `boot_serve.rs`), so it is
//! built — and every leg driven — on an explicitly sized thread.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::BytesMut;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    rules_engine::SQS_SEND_MESSAGE_ACTION, SERVICE_PRINCIPAL,
};
// The MQTT codec the broker itself reads and writes the protocol with, reached through the
// service whose operations report on the sessions it opens (`service_common::iot_mqtt_broker`,
// re-exported there): the suite speaks the wire a client library speaks rather than laying
// packets out by hand.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::query_wire::{element, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The address this emulator's API is served at, as the core publishes it from its data port.
/// The broker's own socket is a DIFFERENT address — one port serves one wire — and which one
/// that is, is the core's to choose, so the device reads it back rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:39371";

/// How long a leg of the exchange may take before the case fails, and how long between two
/// looks at the queue. Generous: both bound a wait on a CONDITION (a packet the broker writes,
/// a message the queue serves), not the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`EXCHANGE_DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The queue every rule below forwards to.
const QUEUE: &str = "iot-basic-ingest-forwarded";

/// The role every `sqs` action is performed under: it trusts the rules engine's own service
/// principal and allows the send on that queue.
const ROLE: &str = "iot-basic-ingest-rule";

/// The SQL version a rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The rule a row names by its topic filter and through Basic Ingest alike, and the topic its
/// `FROM` clause covers. A rule name carries letters, digits and underscores alone (`RuleName`,
/// `pattern ^[a-zA-Z0-9_]+$`).
const RULE: &str = "basic_ingest_forward";
/// See [`RULE`].
const TOPIC: &str = "basicingest/telemetry";

/// The rule whose statement TRANSFORMS what it forwards — a projection under a condition — and
/// the topic its `FROM` clause covers.
const TRANSFORM_RULE: &str = "basic_ingest_transform";
/// See [`TRANSFORM_RULE`].
const TRANSFORM_TOPIC: &str = "basicingest/transform";

/// A rule stored DISABLED, and the topic its `FROM` clause covers: a rule that does nothing,
/// whether a publication matches its filter or names it outright.
const DISABLED_RULE: &str = "basic_ingest_disabled";
/// See [`DISABLED_RULE`].
const DISABLED_TOPIC: &str = "basicingest/disabled";

/// A rule name the account holds no rule under, which a Basic Ingest publication names.
const ABSENT_RULE: &str = "basic_ingest_absent";

/// The SENTINEL rule, its topic, and the marker member its message carries: an ordinary
/// publication whose forward is known to work, published after each row's own message, so a row
/// expecting NOTHING is judged against a queue and a subscriber that have both taken a LATER
/// message.
const SENTINEL_RULE: &str = "basic_ingest_sentinel";
/// See [`SENTINEL_RULE`].
const SENTINEL_TOPIC: &str = "basicingest/sentinel";

/// A topic no rule's `FROM` clause covers, which the subscriber is still subscribed to.
const UNRULED_TOPIC: &str = "basicingest/unruled";

/// The client ids the two devices connect under: one publishes, one listens.
const PUBLISHER_CLIENT_ID: &str = "basic-ingest-publisher";
/// See [`PUBLISHER_CLIENT_ID`].
const SUBSCRIBER_CLIENT_ID: &str = "basic-ingest-subscriber";

/// What the listening device subscribes to: every topic the rows publish on, and the whole
/// reserved Basic Ingest space — so a Basic Ingest publication that was delivered to ANY
/// subscriber is visible here.
const SUBSCRIBED_FILTERS: &[(&str, u8)] = &[("basicingest/#", 1), ("$aws/rules/#", 1)];

/// How many messages one drain of the queue reads, and the long poll it makes: none, because
/// the broker performs the rule's action before it acknowledges the publication, so a message
/// that was forwarded is already in the queue by the time the first drain asks. The waiting a
/// slow host needs is the drain LOOP's ([`EXCHANGE_DEADLINE`]).
const RECEIVE_BATCH: u64 = 10;
/// See [`RECEIVE_BATCH`].
const WAIT_TIME_SECONDS: u64 = 0;

/// One row: the topic the device publishes on, the message it sends, and what each side must
/// then hold.
struct Row {
    case: &'static str,
    /// The topic the PUBLISH packet carries, which for a Basic Ingest row is the reserved form.
    topic: &'static str,
    /// The message as published.
    payload: &'static str,
    /// The message bodies the queue must hold afterwards, besides the sentinel's.
    forwarded: &'static [&'static str],
    /// The payloads the subscriber must be handed before the sentinel's, in order.
    delivered: &'static [&'static str],
}

/// The message every row publishes unless it needs its own: a document whose members the
/// transforming rule's projection and condition read.
const MESSAGE: &str = r#"{"marker":"m-1","temp":42,"extra":"carried"}"#;

/// What the transforming rule SELECTS out of [`MESSAGE`]: the two attributes its projection
/// names, and nothing else.
const TRANSFORMED: &str = r#"{"marker":"m-1","temp":42}"#;

/// A message the transforming rule's condition does NOT hold for.
const COLD_MESSAGE: &str = r#"{"marker":"m-cold","temp":4,"extra":"carried"}"#;

const ROWS: &[Row] = &[
    Row {
        case: "an ordinary publication on the rule's own topic",
        topic: TOPIC,
        payload: MESSAGE,
        forwarded: &[MESSAGE],
        delivered: &[MESSAGE],
    },
    Row {
        case: "a Basic Ingest publication naming the rule and the topic its FROM clause covers",
        topic: "$aws/rules/basic_ingest_forward/basicingest/telemetry",
        payload: MESSAGE,
        forwarded: &[MESSAGE],
        delivered: &[],
    },
    Row {
        case: "a Basic Ingest publication whose topic the named rule's FROM clause does not \
               cover",
        topic: "$aws/rules/basic_ingest_forward/elsewhere/deep/leaf",
        payload: MESSAGE,
        forwarded: &[MESSAGE],
        delivered: &[],
    },
    Row {
        case: "a Basic Ingest publication naming the rule and no topic at all",
        topic: "$aws/rules/basic_ingest_forward",
        payload: MESSAGE,
        forwarded: &[MESSAGE],
        delivered: &[],
    },
    Row {
        case: "a Basic Ingest publication naming a rule the account holds none of",
        topic: "$aws/rules/basic_ingest_absent/basicingest/telemetry",
        payload: MESSAGE,
        forwarded: &[],
        delivered: &[],
    },
    Row {
        case: "a Basic Ingest publication naming a disabled rule",
        topic: "$aws/rules/basic_ingest_disabled/basicingest/disabled",
        payload: MESSAGE,
        forwarded: &[],
        delivered: &[],
    },
    Row {
        case: "an ordinary publication on the disabled rule's own topic",
        topic: DISABLED_TOPIC,
        payload: MESSAGE,
        forwarded: &[],
        delivered: &[MESSAGE],
    },
    Row {
        case: "an ordinary publication on a topic no rule's FROM clause covers",
        topic: UNRULED_TOPIC,
        payload: MESSAGE,
        forwarded: &[],
        delivered: &[MESSAGE],
    },
    Row {
        case: "an ordinary publication the transforming rule's condition selects",
        topic: TRANSFORM_TOPIC,
        payload: MESSAGE,
        forwarded: &[TRANSFORMED],
        delivered: &[MESSAGE],
    },
    Row {
        case: "a Basic Ingest publication the transforming rule's condition selects",
        topic: "$aws/rules/basic_ingest_transform/basicingest/transform",
        payload: MESSAGE,
        forwarded: &[TRANSFORMED],
        delivered: &[],
    },
    Row {
        case: "a Basic Ingest publication the transforming rule's condition does not select",
        topic: "$aws/rules/basic_ingest_transform/basicingest/transform",
        payload: COLD_MESSAGE,
        forwarded: &[],
        delivered: &[],
    },
    Row {
        case: "an ordinary publication the transforming rule's condition does not select",
        topic: TRANSFORM_TOPIC,
        payload: COLD_MESSAGE,
        forwarded: &[],
        delivered: &[COLD_MESSAGE],
    },
];

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router picks
/// the owning service by. The signature is never checked in this harness.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One in-process API call through the whole router, as its status and body.
async fn send(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    signing_name: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("{SERVED_AT}{path}"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router
        .dispatch(
            request
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, rendered)
}

/// One Amazon SQS call (`awsJson1_0`) that must be served, answered with its document.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, rendered) = send(
        router,
        "POST",
        "/",
        "sqs",
        "application/x-amz-json-1.0",
        &[("x-amz-target", &format!("AmazonSQS.{operation}"))],
        serde_json::to_vec(body).expect("the request serializes"),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: SQS {operation} answered {status} with {rendered}"
    );
    serde_json::from_str(&rendered).unwrap_or(Value::Null)
}

/// One IAM call over the `awsQuery` protocol that must be served, answered with its XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, rendered) = send(
        router,
        "POST",
        "/",
        "iam",
        "application/x-www-form-urlencoded",
        &[],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} answered {status} with {rendered}"
    );
    rendered
}

/// `GET /endpoint?endpointType=iot:Data-ATS`: the address the control plane publishes for the
/// account's data endpoint, which is the address a device then dials.
async fn discover(router: &MultiServiceRouter) -> String {
    let (status, rendered) = send(
        router,
        "GET",
        &format!("/endpoint?endpointType={}", form_encode("iot:Data-ATS")),
        "iot",
        "application/json",
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeEndpoint answered {status} with {rendered}"
    );
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    parsed
        .get("endpointAddress")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: no endpointAddress in {rendered}"))
        .to_string()
}

/// Store one topic rule whose single action sends the message its statement selected to the
/// row's queue, as `CreateTopicRule` takes it: the body IS the `topicRulePayload`.
async fn create_topic_rule(
    router: &MultiServiceRouter,
    rule: &str,
    sql: &str,
    disabled: bool,
    role_arn: &str,
    queue_url: &str,
) {
    let (status, rendered) = send(
        router,
        "POST",
        &format!("/rules/{rule}"),
        "iot",
        "application/json",
        &[],
        json!({
            "sql": sql,
            "awsIotSqlVersion": SQL_VERSION,
            "ruleDisabled": disabled,
            "actions": [{
                "sqs": { "roleArn": role_arn, "queueUrl": queue_url, "useBase64": false },
            }],
        })
        .to_string()
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTopicRule({rule}) answered {status} with {rendered}"
    );
}

/// The role every `sqs` action runs under: it trusts the rules engine's service principal and
/// allows the send on `queue_arn`. Answered with the ARN IAM persisted.
async fn given_role(router: &MultiServiceRouter, queue_arn: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": SERVICE_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = element(&created, "Arn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": SQS_SEND_MESSAGE_ACTION,
            "Resource": queue_arn,
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "rule-action"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// One drain of the queue: the bodies it served this time, which is none once everything sent
/// to it has been read.
async fn drained_once(router: &MultiServiceRouter, queue_url: &str) -> Vec<String> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": queue_url,
            "MaxNumberOfMessages": RECEIVE_BATCH,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Everything the queue holds BESIDE the sentinel, read once the sentinel is in it and a
/// further look finds nothing.
///
/// The sentinel is forwarded by a rule whose action is known to be performable and is published
/// after the row's own message, so a row that expects NOTHING is judged against a queue that has
/// already taken a LATER publication — rather than against a queue nothing has reached yet,
/// which would pass whatever the engine did.
async fn forwarded_bodies(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    sentinel: &str,
) -> Vec<String> {
    let giving_up_at = Instant::now() + EXCHANGE_DEADLINE;
    let mut held: Vec<String> = Vec::new();
    loop {
        let batch = drained_once(router, queue_url).await;
        let empty = batch.is_empty();
        held.extend(batch);
        if empty && held.iter().any(|body| same_message(sentinel, body)) {
            break;
        }
        if Instant::now() >= giving_up_at {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert!(
        held.iter().any(|body| same_message(sentinel, body)),
        "case {case:?}: the sentinel publication was never forwarded, so what the queue holds \
         says nothing about this row; it held {held:?}"
    );
    held.retain(|body| !same_message(sentinel, body));
    held
}

/// Whether `actual` is the message `expected`: the same text, or the same JSON DOCUMENT where
/// both are documents — a projected document's members are a set, and the order a JSON object
/// writes them in carries no meaning.
fn same_message(expected: &str, actual: &str) -> bool {
    if expected == actual {
        return true;
    }
    match (
        serde_json::from_str::<Value>(expected),
        serde_json::from_str::<Value>(actual),
    ) {
        (Ok(expected), Ok(actual)) => expected == actual,
        _ => false,
    }
}

/// The QoS a number names, as a client's own library reads it.
fn qos_of(number: u8) -> QoS {
    match number {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

/// The port a published address names, checked to be on the LOOPBACK interface this emulator's
/// listeners are bound on — the only host a client of this emulator reaches it at.
fn loopback_port(address: &str) -> u16 {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("the published address {address:?} names no port"));
    assert_eq!(
        host,
        Ipv4Addr::LOCALHOST.to_string(),
        "expected the published address on the loopback interface, got {address:?}"
    );
    port.parse()
        .unwrap_or_else(|error| panic!("{address:?} names no port: {error}"))
}

/// One delivery the broker wrote to a subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Delivered {
    topic: String,
    payload: Vec<u8>,
}

/// A DEVICE: a real socket to the published address, speaking MQTT 3.1.1 on it.
struct Device {
    client_id: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
    /// The packet identifier the next publication of this device carries.
    next_packet_id: u16,
}

impl Device {
    /// Dial `address` and send the CONNECT, answering with the connected device.
    async fn connect(address: &str, client_id: &'static str) -> Self {
        let port = loopback_port(address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("device {client_id}: dialling {address} timed out"))
            .unwrap_or_else(|error| {
                panic!(
                    "device {client_id}: the published address {address} must accept a \
                     connection: {error}"
                )
            });
        let mut device = Self {
            client_id,
            stream,
            buffer: BytesMut::new(),
            next_packet_id: 1,
        };
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = true;
        connect.keep_alive = 60;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.expect_packet().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!("device {client_id}: expected a CONNACK, got {answered:?}");
        };
        assert_eq!(
            connack.code,
            v4::ConnectReturnCode::Success,
            "device {client_id}: the broker must accept the CONNECT, got {:?}",
            connack.code
        );
        device
    }

    /// Write one packet, rendered by the codec.
    async fn write<Render>(&mut self, render: Render)
    where
        Render: FnOnce(&mut BytesMut) -> Result<usize, mqttbytes::Error>,
    {
        let mut rendered = BytesMut::new();
        render(&mut rendered).unwrap_or_else(|error| {
            panic!("device {}: the packet renders: {error}", self.client_id)
        });
        self.stream.write_all(&rendered).await.unwrap_or_else(|error| {
            panic!("device {}: the socket takes it: {error}", self.client_id)
        });
    }

    /// Read one packet the broker wrote, waiting for it until [`EXCHANGE_DEADLINE`].
    async fn expect_packet(&mut self) -> v4::Packet {
        let client_id = self.client_id;
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match v4::read(&mut self.buffer, 256 * 1024) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => {
                        panic!("device {client_id}: the broker wrote no packet: {error}")
                    }
                }
                let mut chunk = [0u8; 4096];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => {
                        panic!("device {client_id}: reading the socket failed: {error}")
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("device {client_id}: the broker wrote nothing in time"));
        read.unwrap_or_else(|| panic!("device {client_id}: the broker closed the connection"))
    }

    /// Subscribe to `filters`, answering with the QoS granted for each.
    async fn subscribe(&mut self, filters: &[(&str, u8)]) -> Vec<i32> {
        let mut subscribe = v4::Subscribe::new(filters[0].0, qos_of(filters[0].1));
        subscribe.pkid = self.take_packet_id();
        for (path, qos) in &filters[1..] {
            subscribe
                .filters
                .push(v4::SubscribeFilter::new((*path).to_string(), qos_of(*qos)));
        }
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.expect_packet().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("device {}: expected a SUBACK, got {answered:?}", self.client_id);
        };
        suback
            .return_codes
            .iter()
            .map(|code| match code {
                v4::SubscribeReasonCode::Success(qos) => match qos {
                    QoS::AtMostOnce => 0,
                    QoS::AtLeastOnce => 1,
                    QoS::ExactlyOnce => 2,
                },
                refused => {
                    panic!("device {}: a filter was refused: {refused:?}", self.client_id)
                }
            })
            .collect()
    }

    /// Publish one message at QoS 1 over this device's own connection, waiting for the PUBACK
    /// that says the broker took it — which is what makes the row's assertions need no span:
    /// the broker performs the rules' actions before it acknowledges the publication.
    async fn publish(&mut self, topic: &str, payload: &str) {
        let packet_id = self.take_packet_id();
        let mut publish = v4::Publish::new(topic, QoS::AtLeastOnce, payload.as_bytes().to_vec());
        publish.pkid = packet_id;
        self.write(|buffer| publish.write(buffer)).await;
        let answered = self.expect_packet().await;
        let v4::Packet::PubAck(puback) = answered else {
            panic!(
                "device {}: expected the PUBACK of {topic}, got {answered:?}",
                self.client_id
            );
        };
        assert_eq!(
            puback.pkid, packet_id,
            "device {}: expected the PUBACK of the publication, got {puback:?}",
            self.client_id
        );
    }

    /// The packet identifier the next packet of this device carries: non-zero and distinct
    /// across the packets in flight, which is all one has to be.
    fn take_packet_id(&mut self) -> u16 {
        let packet_id = self.next_packet_id;
        self.next_packet_id = self.next_packet_id.checked_add(1).unwrap_or(1);
        packet_id
    }

    /// The deliveries this device is handed up to, and NOT including, the one carrying
    /// `sentinel` on [`SENTINEL_TOPIC`] — which is published after the row's own message, so
    /// the sentinel's arrival is the condition that says every earlier delivery has arrived and
    /// a row expecting none needs no wall clock.
    async fn deliveries_before_sentinel(&mut self, sentinel: &str) -> Vec<Delivered> {
        let mut seen = Vec::new();
        loop {
            let packet = self.expect_packet().await;
            let v4::Packet::Publish(publish) = packet else {
                panic!(
                    "device {}: expected a delivery, got {packet:?}",
                    self.client_id
                );
            };
            let delivered = Delivered {
                topic: publish.topic,
                payload: publish.payload.to_vec(),
            };
            if delivered.topic == SENTINEL_TOPIC
                && same_message(sentinel, &String::from_utf8_lossy(&delivered.payload))
            {
                return seen;
            }
            seen.push(delivered);
        }
    }
}

/// The emulator for this suite: the whole router on one shared engine, served at [`SERVED_AT`].
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here.
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"),
    );
    router
}

/// Run `body` on a thread with the stack the router's assembly needs, inside a runtime with the
/// IO the broker's own socket is bound on.
fn on_the_emulator<Body>(name: &'static str, body: Body)
where
    Body: FnOnce(MultiServiceRouter, tokio::runtime::Runtime) + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime with IO");
            let router = emulator();
            body(router, runtime);
        })
        .unwrap_or_else(|error| panic!("spawn the {name} thread: {error}"))
        .join()
        .unwrap_or_else(|_| panic!("{name} must hold"));
}

/// The message published on a row's own topic reaches the rules the account holds, and a
/// message addressed to a rule by name reaches THAT rule and no subscriber.
#[test]
fn a_devices_publication_runs_the_topic_rules_and_basic_ingest_bypasses_the_subscribers() {
    on_the_emulator("iot-basic-ingest", |router, runtime| {
        runtime.block_on(async {
            // The target of every action, and the role it is performed under.
            let queue_url = sqs(&router, "CreateQueue", &json!({ "QueueName": QUEUE }))
                .await["QueueUrl"]
                .as_str()
                .expect("CreateQueue reports a QueueUrl")
                .to_string();
            let queue_arn = sqs(
                &router,
                "GetQueueAttributes",
                &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
            )
            .await["Attributes"]["QueueArn"]
                .as_str()
                .expect("GetQueueAttributes reports the QueueArn")
                .to_string();
            let role_arn = given_role(&router, &queue_arn).await;

            for (rule, sql, disabled) in [
                (RULE, format!("SELECT * FROM '{TOPIC}'"), false),
                (
                    TRANSFORM_RULE,
                    format!("SELECT marker, temp FROM '{TRANSFORM_TOPIC}' WHERE temp > 30"),
                    false,
                ),
                (
                    DISABLED_RULE,
                    format!("SELECT * FROM '{DISABLED_TOPIC}'"),
                    true,
                ),
                (
                    SENTINEL_RULE,
                    format!("SELECT * FROM '{SENTINEL_TOPIC}'"),
                    false,
                ),
            ] {
                create_topic_rule(&router, rule, &sql, disabled, &role_arn, &queue_url).await;
            }

            // The premise of the row that names a rule the account holds none of: the control
            // plane resolves no rule under that name, so the publication addressed to it has
            // nothing to run.
            let (status, rendered) = send(
                &router,
                "GET",
                &format!("/rules/{ABSENT_RULE}"),
                "iot",
                "application/json",
                &[],
                Vec::new(),
            )
            .await;
            assert_ne!(
                status, 200,
                "setup: the account must hold no rule named {ABSENT_RULE}, got {rendered}"
            );

            // Where this account's data plane answers, which is the address a device dials —
            // and the call that opens the broker's socket.
            let address = discover(&router).await;

            let mut subscriber = Device::connect(&address, SUBSCRIBER_CLIENT_ID).await;
            let granted = subscriber.subscribe(SUBSCRIBED_FILTERS).await;
            assert_eq!(
                granted,
                vec![1, 1],
                "setup: expected QoS 1 granted for both filters, got {granted:?}"
            );
            let mut publisher = Device::connect(&address, PUBLISHER_CLIENT_ID).await;

            for (index, row) in ROWS.iter().enumerate() {
                let case = row.case;
                let sentinel = json!({ "sentinel": index }).to_string();

                publisher.publish(row.topic, row.payload).await;
                publisher.publish(SENTINEL_TOPIC, &sentinel).await;

                // What the SUBSCRIBERS were handed: everything before the sentinel, which the
                // broker delivers after the row's own publication over the same connection.
                let delivered = subscriber.deliveries_before_sentinel(&sentinel).await;
                let payloads: Vec<String> = delivered
                    .iter()
                    .map(|one| String::from_utf8_lossy(&one.payload).to_string())
                    .collect();
                assert_eq!(
                    payloads.len(),
                    row.delivered.len(),
                    "case {case:?}: expected {} delivery(ies) to the subscriber, got \
                     {delivered:?}",
                    row.delivered.len()
                );
                for (expected, actual) in row.delivered.iter().zip(payloads.iter()) {
                    assert!(
                        same_message(expected, actual),
                        "case {case:?}: expected the subscriber handed {expected:?}, got \
                         {actual:?}"
                    );
                }

                // What the rule's action sent, read where a client reads it: out of the queue.
                let forwarded = forwarded_bodies(&router, case, &queue_url, &sentinel).await;
                assert_eq!(
                    forwarded.len(),
                    row.forwarded.len(),
                    "case {case:?}: expected {} message(s) forwarded to the queue, got \
                     {forwarded:?}",
                    row.forwarded.len()
                );
                for expected in row.forwarded {
                    assert!(
                        forwarded.iter().any(|body| same_message(expected, body)),
                        "case {case:?}: expected {expected:?} in the queue, got {forwarded:?}"
                    );
                }
            }
        });
    });
}
