// SPDX-License-Identifier: CC0-1.0

//! The broker's RETAINED messages, end to end: the publication that outlives the
//! connection that made it, and what a session that subscribes afterwards is handed.
//!
//! The rule under test: the broker keeps the last RETAINED publication of each topic, and
//! hands a NEW subscription the retained message of every topic its filter covers, with the
//! RETAIN flag set — MQTT 3.1.1 section 3.3.1.3 ("the Server MUST store the Application
//! Message and its QoS, so that it can be delivered to future subscribers whose
//! subscriptions match its topic name") and section 3.8.4 ("the Server MUST send the
//! retained messages matching the Topic Filter of the subscription to the Client"). A
//! retained publication carrying no payload CLEARS the topic, a later retained publication
//! replaces it, and a message a subscriber receives on a subscription it already held
//! carries the flag clear. The store is the BROKER's one store, whether the publication
//! arrived over MQTT or through the IoT data plane's `Publish`, which is why
//! `GetRetainedMessage` answers with the same message a subscriber is handed.
//!
//! It needs the WHOLE router: the address a device connects to is published by the IoT
//! control plane (`DescribeEndpoint`), the publications and the retained-message reads go
//! through the IoT data plane (`Publish`, `GetRetainedMessage`), and the broker that keeps
//! them is in neither.
//!
//! The rows differ along what the rule generalises over: the filter form (an exact topic,
//! a single-level wildcard, a multi-level one, the whole-tree filter), how many topics one
//! filter covers (none, one, several), the payload (JSON text, raw bytes, a cleared topic)
//! and the QoS each publication was made at.
//!
//! The session is spoken over a real TCP socket to the published address with the same
//! maintained MQTT codec a client library uses; the device harness is
//! `iot_shadow_reserved_topic_push`'s, which this suite shares. The ABSENCE of a retained
//! delivery is asserted STRUCTURALLY rather than by waiting: a live publication is made on
//! the subscribed filter afterwards and read off the same connection, so a retained message
//! handed over where none was owed would arrive ahead of it.

use serde_json::Value;
// The broker's own reading of a QoS number, reached through the service whose operations
// report on its sessions: the suite names the QoS the way the broker records it rather than
// re-deriving it.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::wire::qos_number;
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    v4, QoS,
};
use smithy_server_runtime::MultiServiceRouter;

// The topic-as-path-label escaping and the device harness this suite drives, both already
// written for the sibling suites that publish over HTTP and speak MQTT on a socket.
use crate::iot_mqtt5_session_and_properties::encode_label;
use crate::iot_shadow_reserved_topic_push::{call, discover, on_the_emulator, Device, SERVED_AT};

/// `POST /topics/{topic}?qos=…&retain=…` through the data plane's `Publish`, asserting it
/// was accepted.
async fn publish(
    case: &str,
    router: &MultiServiceRouter,
    topic: &str,
    payload: &[u8],
    qos: i32,
    retain: bool,
) {
    let (status, rendered) = call(
        router,
        "POST",
        &format!(
            "{SERVED_AT}/topics/{}?qos={qos}&retain={retain}",
            encode_label(topic)
        ),
        "iotdata",
        payload.to_vec(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the publish on {topic} must be accepted, got {status} with {rendered}"
    );
}

/// `GET /retainedMessage/{topic}` through the data plane's `GetRetainedMessage`, as its
/// status and parsed body.
async fn read_retained(router: &MultiServiceRouter, topic: &str) -> (u16, Value) {
    let (status, rendered) = call(
        router,
        "GET",
        &format!("{SERVED_AT}/retainedMessage/{}", encode_label(topic)),
        "iotdata",
        Vec::new(),
    )
    .await;
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    (status, parsed)
}

/// The payload one `GetRetainedMessage` answer carries, decoded: a blob travels base64 in a
/// restJson1 body.
fn retained_payload(case: &str, body: &Value) -> Vec<u8> {
    let encoded = body
        .get("payload")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no payload in {body}"));
    aws_smithy_types::base64::decode(encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the payload is not base64: {error}"))
}

/// Subscribe `device` to `filter` asking for `qos` as its maximum, asserting the
/// subscription was granted at the QoS it asked for.
async fn subscribe(case: &str, device: &mut Device, filter: &str, qos: QoS) {
    let mut subscribe = v4::Subscribe::new(filter, qos);
    subscribe.pkid = 1;
    device.write(|buffer| subscribe.write(buffer)).await;
    let packet = device.read_packet().await;
    let v4::Packet::SubAck(suback) = packet else {
        panic!("case {case:?}: expected a SUBACK for {filter}, got {packet:?}");
    };
    assert_eq!(
        suback.return_codes,
        vec![v4::SubscribeReasonCode::Success(qos)],
        "case {case:?}: the subscription to {filter} must be granted at {qos:?}, got {:?}",
        suback.return_codes
    );
}

/// The next PUBLISH the broker wrote to `device`, acknowledged when it was made at QoS 1.
async fn next_publish(case: &str, device: &mut Device) -> v4::Publish {
    let packet = device.read_packet().await;
    let v4::Packet::Publish(publish) = packet else {
        panic!("case {case:?}: expected a publication, got {packet:?}");
    };
    if publish.qos != QoS::AtMostOnce {
        let pkid = publish.pkid;
        device
            .write(|buffer| v4::PubAck::new(pkid).write(buffer))
            .await;
    }
    publish
}

/// Publish from the DEVICE itself: a PUBLISH at QoS 1 with the RETAIN flag SET, answered
/// by the broker's PUBACK.
async fn publish_from_device(case: &str, device: &mut Device, topic: &str, payload: &[u8]) {
    let mut publish = v4::Publish::new(topic, QoS::AtLeastOnce, payload.to_vec());
    publish.retain = true;
    publish.pkid = 9;
    device.write(|buffer| publish.write(buffer)).await;
    let packet = device.read_packet().await;
    assert!(
        matches!(packet, v4::Packet::PubAck(_)),
        "case {case:?}: expected the PUBACK of the retained publish on {topic}, got {packet:?}"
    );
}

/// What each topic below holds a retained message of: the topic, its payload and the QoS it
/// was published at. The payloads differ in kind — JSON text, raw bytes that are no text at
/// all, a short token — so a delivery that carried another topic's message is visible.
const RETAINED: &[(&str, &[u8], i32)] = &[
    ("fleet/alpha/telemetry", b"{\"t\":21}", 1),
    ("fleet/beta/telemetry", &[0x00, 0x01, 0xff, 0x7f], 0),
    ("fleet/beta/state", b"ready", 1),
    ("warehouse/door", b"closed", 0),
];

/// One subscription, and the retained messages it must be handed.
struct Row {
    case: &'static str,
    client_id: &'static str,
    /// The topic filter the device subscribes to.
    filter: &'static str,
    /// The maximum QoS the subscription asks for, which is the ceiling every delivery it
    /// is handed is made at.
    requested_qos: QoS,
    /// The topics whose retained message the subscription is owed, in the order the broker
    /// walks them (its topic order).
    expected: &'static [&'static str],
}

/// The rows: each a different filter form, covering a different number of the retained
/// topics — including one that covers none of them, and one whose subscription asks for a
/// lower maximum QoS than the publications were made at.
const ROWS: &[Row] = &[
    Row {
        case: "an exact filter",
        client_id: "retained-exact",
        filter: "fleet/alpha/telemetry",
        requested_qos: QoS::AtLeastOnce,
        expected: &["fleet/alpha/telemetry"],
    },
    Row {
        case: "a single-level wildcard",
        client_id: "retained-single-level",
        filter: "fleet/+/telemetry",
        requested_qos: QoS::AtLeastOnce,
        expected: &["fleet/alpha/telemetry", "fleet/beta/telemetry"],
    },
    Row {
        case: "a multi-level wildcard",
        client_id: "retained-multi-level",
        filter: "fleet/#",
        requested_qos: QoS::AtLeastOnce,
        expected: &[
            "fleet/alpha/telemetry",
            "fleet/beta/state",
            "fleet/beta/telemetry",
        ],
    },
    Row {
        case: "a subscription granted QoS 0",
        client_id: "retained-at-most-once",
        filter: "fleet/#",
        requested_qos: QoS::AtMostOnce,
        expected: &[
            "fleet/alpha/telemetry",
            "fleet/beta/state",
            "fleet/beta/telemetry",
        ],
    },
    Row {
        case: "the whole-tree filter",
        client_id: "retained-whole-tree",
        filter: "#",
        requested_qos: QoS::AtLeastOnce,
        expected: &[
            "fleet/alpha/telemetry",
            "fleet/beta/state",
            "fleet/beta/telemetry",
            "warehouse/door",
        ],
    },
    Row {
        case: "a filter covering no retained topic",
        client_id: "retained-no-match",
        filter: "archive/#",
        requested_qos: QoS::AtLeastOnce,
        expected: &[],
    },
];

/// The retained message of each topic a new subscription's filter covers is handed to it,
/// with the RETAIN flag set and at the lower of the published QoS and the QoS the
/// subscription was granted; a subscription that covers none is handed none.
#[test]
fn a_new_subscription_is_handed_the_retained_message_of_every_topic_its_filter_covers() {
    on_the_emulator("iot-retained-on-subscribe", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            for (topic, payload, qos) in RETAINED {
                publish("the retained publications", &router, topic, payload, *qos, true).await;
            }

            for row in ROWS {
                let case = row.case;
                let mut device = Device::connect(case, &address, row.client_id).await;
                subscribe(case, &mut device, row.filter, row.requested_qos).await;

                for topic in row.expected {
                    let (_topic, payload, qos) = RETAINED
                        .iter()
                        .find(|(retained, _payload, _qos)| retained == topic)
                        .unwrap_or_else(|| panic!("case {case:?}: {topic} is a retained topic"));
                    let handed = next_publish(case, &mut device).await;
                    assert_eq!(
                        handed.topic, *topic,
                        "case {case:?}: expected the retained message of {topic}, got {:?}",
                        handed.topic
                    );
                    assert_eq!(
                        handed.payload.as_ref(),
                        *payload,
                        "case {case:?}: expected the published bytes of {topic}, got {:?}",
                        handed.payload.as_ref()
                    );
                    assert!(
                        handed.retain,
                        "case {case:?}: a message handed over because a subscription was just \
                         made must carry the RETAIN flag, got {handed:?}"
                    );
                    // The delivery is made at the LOWER of the QoS the publication was made
                    // at and the maximum the subscription was granted — MQTT's own minimum
                    // of the two.
                    let expected_qos = (*qos).min(qos_number(row.requested_qos));
                    assert_eq!(
                        qos_number(handed.qos),
                        expected_qos,
                        "case {case:?}: expected {topic} delivered at QoS {expected_qos}, got \
                         {:?}",
                        handed.qos
                    );
                }

                // Nothing else was handed over: a LIVE publication on this filter is made
                // now and read off the same connection, so a retained message delivered
                // where none was owed would arrive ahead of it — and this one carries the
                // RETAIN flag CLEAR, because the subscription was already established.
                let live_topic = match row.filter {
                    "#" => "live/probe",
                    "archive/#" => "archive/probe",
                    "fleet/#" | "fleet/+/telemetry" => "fleet/alpha/telemetry",
                    exact => exact,
                };
                publish(case, &router, live_topic, b"live", 1, false).await;
                let live = next_publish(case, &mut device).await;
                assert_eq!(
                    live.topic, live_topic,
                    "case {case:?}: expected the live publication on {live_topic} next, got \
                     {live:?}"
                );
                assert_eq!(
                    live.payload.as_ref(),
                    b"live",
                    "case {case:?}: expected the live publication's bytes, got {live:?}"
                );
                assert!(
                    !live.retain,
                    "case {case:?}: a publication matching an established subscription must \
                     carry the RETAIN flag clear, got {live:?}"
                );
            }
        });
    });
}

/// What one topic's retained message is, as the retained-message reader and a later
/// subscription agree on it: replaced by the next retained publication, cleared by one
/// carrying no payload, and the same store whether a device or the HTTP data plane
/// published it.
#[test]
fn a_retained_message_is_replaced_cleared_and_read_back_from_the_one_store() {
    on_the_emulator("iot-retained-lifecycle", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            let topic = "fleet/gamma/state";

            // Retained, then replaced: what the reader answers with, and what a session
            // that subscribes afterwards is handed, is the LAST publication.
            let case = "a replaced retained message";
            publish(case, &router, topic, b"first", 1, true).await;
            publish(case, &router, topic, b"second", 1, true).await;
            let (status, body) = read_retained(&router, topic).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the retained message must be reported, got {status} with {body}"
            );
            assert_eq!(
                retained_payload(case, &body),
                b"second".to_vec(),
                "case {case:?}: expected the last publication's bytes, got {body}"
            );
            let mut device = Device::connect(case, &address, "retained-replaced").await;
            subscribe(case, &mut device, "fleet/gamma/+", QoS::AtLeastOnce).await;
            let handed = next_publish(case, &mut device).await;
            assert_eq!(
                (handed.topic.as_str(), handed.payload.as_ref(), handed.retain),
                (topic, b"second".as_slice(), true),
                "case {case:?}: expected the retained message as the store holds it, got \
                 {handed:?}"
            );

            // Cleared by a retained publication with no payload: the reader reports the
            // topic absent, and a session that subscribes afterwards is handed nothing —
            // which the live publication read next is the proof of.
            let case = "a cleared retained message";
            publish(case, &router, topic, b"", 1, true).await;
            let (status, body) = read_retained(&router, topic).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a cleared topic holds no retained message, got {status} with \
                 {body}"
            );
            let mut after = Device::connect(case, &address, "retained-cleared").await;
            subscribe(case, &mut after, "fleet/gamma/+", QoS::AtLeastOnce).await;
            publish(case, &router, topic, b"live", 1, false).await;
            let live = next_publish(case, &mut after).await;
            assert_eq!(
                (live.topic.as_str(), live.payload.as_ref(), live.retain),
                (topic, b"live".as_slice(), false),
                "case {case:?}: expected only the live publication, got {live:?}"
            );

            // And a DEVICE's own retained publication is in that same store: the reader
            // answers with it, and the next subscription is handed it. It is published from
            // a connection of its own, which subscribes to nothing — the devices above are
            // still being written to on the filter they subscribed to.
            let case = "a device's own retained publication";
            let device_topic = "fleet/delta/state";
            let mut publisher = Device::connect(case, &address, "retained-publisher").await;
            publish_from_device(case, &mut publisher, device_topic, b"{\"v\":2}").await;
            let (status, body) = read_retained(&router, device_topic).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the device's retained message must be reported, got {status} \
                 with {body}"
            );
            assert_eq!(
                retained_payload(case, &body),
                b"{\"v\":2}".to_vec(),
                "case {case:?}: expected the bytes the device published, got {body}"
            );
            let mut reader = Device::connect(case, &address, "retained-from-device").await;
            subscribe(case, &mut reader, "fleet/delta/#", QoS::AtLeastOnce).await;
            let handed = next_publish(case, &mut reader).await;
            assert_eq!(
                (handed.topic.as_str(), handed.payload.as_ref(), handed.retain),
                (device_topic, b"{\"v\":2}".as_slice(), true),
                "case {case:?}: expected the device's retained message handed over, got \
                 {handed:?}"
            );
        });
    });
}
