// SPDX-License-Identifier: CC0-1.0

//! The MQTT 5 half of the broker a device DISCOVERS at the published data endpoint: a
//! session opened by a client of the newer protocol version, and the message PROPERTIES a
//! publication's own parameters reach that client as.
//!
//! The rule under test: the broker at the address `DescribeEndpoint` publishes serves BOTH
//! MQTT versions AWS IoT Core's message broker serves — a CONNECT announcing protocol level
//! 4 (MQTT 3.1.1) or 5 (MQTT 5) opens a session, and one announcing any other level is
//! REFUSED with a CONNACK rather than left silent — and a publication is forwarded to each
//! subscriber with every message property the publisher set, as far as that subscriber's own
//! version can carry it. A publication that set no property arrives carrying none, and the
//! Message Expiry Interval a subscriber is handed is never larger than the one published
//! (MQTT 5 section 3.3.2.3.3: the received value minus the time the message waited).
//!
//! It needs the WHOLE router: the address is published by the IoT control-plane crate, the
//! publication is made through the IoT data plane's `Publish`, and the broker that forwards
//! it is in neither — so a single-service suite cannot see the three agree.
//!
//! The rows differ along what the rule generalises over: which of the modelled publish
//! parameters are set (each alone, all together, and none at all), the payload format
//! indicator, how many user properties there are, what the correlation data decodes to
//! (text, bytes that are no UTF-8, nothing), the message expiry asked for, and the QoS.
//!
//! The session is spoken over a real TCP socket to the published address, with the packets
//! laid out by the same maintained MQTT codec a client library uses, so nothing of the
//! protocol is short-circuited. Assembling the router costs stack (see `boot_serve.rs`), so
//! it is built — and every leg driven — on an explicitly sized thread.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::BytesMut;
use serde_json::Value;
// The MQTT codec the broker itself reads and writes the protocol with, reached through the
// service whose operations report on the sessions it opens
// (`service_common::iot_mqtt_broker`, re-exported there): the suite speaks the wire a
// client library speaks rather than laying packets out by hand.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v5, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "us-east-2";

/// The address this emulator's API is served at, as the core publishes it from its data
/// port. The broker's own socket is a DIFFERENT address — one port serves one wire — and
/// which one that is, is the core's to choose, so every assertion about it reads it back
/// rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:39357";

/// How long a leg of the exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes), not the speed of the host — a loaded
/// machine takes longer and still passes.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// The client id the subscriber below connects under, and the topic tree it subscribes to.
const SUBSCRIBER_CLIENT_ID: &str = "mqtt5-property-subscriber";
/// See [`SUBSCRIBER_CLIENT_ID`].
const SUBSCRIBED_FILTER: &str = "devices/mqtt5/#";

/// One publication made through the data plane's `Publish`, and the message properties the
/// MQTT 5 subscriber must then receive.
struct Row {
    case: &'static str,
    /// The topic published on — covered by [`SUBSCRIBED_FILTER`].
    topic: &'static str,
    payload: &'static str,
    /// The QoS the publication asks for.
    qos: i32,
    /// `messageExpiry`, in seconds; 0 is the member's default and names none.
    message_expiry: i64,
    content_type: Option<&'static str>,
    response_topic: Option<&'static str>,
    /// The `userProperties` document the request's header carries, and the name and value
    /// pairs the subscriber must receive from it, in order.
    user_properties: Option<&'static str>,
    expected_user_properties: &'static [(&'static str, &'static str)],
    /// `payloadFormatIndicator`, and the MQTT 5 indicator it names (section 3.3.2.3.2).
    payload_format_indicator: Option<&'static str>,
    expected_payload_format: Option<u8>,
    /// The `correlationData` header's base64 text, and the raw bytes it decodes to — which
    /// is what the wire carries. Written out as a pair rather than computed, so the case
    /// states the encoding rather than borrowing it from the code under test.
    correlation_data: Option<(&'static str, &'static [u8])>,
}

/// The rows: each a different set of publish parameters, so no single answer satisfies two
/// of them.
const ROWS: &[Row] = &[
    Row {
        case: "every modelled property at once",
        topic: "devices/mqtt5/sensor-1/telemetry",
        payload: r#"{"temperature":21.5}"#,
        qos: 1,
        message_expiry: 300,
        content_type: Some("application/json"),
        response_topic: Some("devices/mqtt5/sensor-1/reply"),
        user_properties: Some(r#"[{"a": "alpha"}, {"b": "beta"}]"#),
        expected_user_properties: &[("a", "alpha"), ("b", "beta")],
        payload_format_indicator: Some("UTF8_DATA"),
        expected_payload_format: Some(1),
        correlation_data: Some(("Y29yci03", b"corr-7")),
    },
    Row {
        case: "no property at all, at QoS 0",
        topic: "devices/mqtt5/sensor-2/telemetry",
        payload: "plain",
        qos: 0,
        message_expiry: 0,
        content_type: None,
        response_topic: None,
        user_properties: None,
        expected_user_properties: &[],
        payload_format_indicator: None,
        expected_payload_format: None,
        correlation_data: None,
    },
    Row {
        case: "the unspecified-bytes format, with correlation data that is no UTF-8",
        topic: "devices/mqtt5/sensor-3/raw",
        payload: "\u{0}\u{1}",
        qos: 1,
        message_expiry: 1,
        content_type: Some("application/octet-stream"),
        response_topic: None,
        user_properties: None,
        expected_user_properties: &[],
        payload_format_indicator: Some("UNSPECIFIED_BYTES"),
        expected_payload_format: Some(0),
        correlation_data: Some(("AP8Q", &[0x00, 0xff, 0x10])),
    },
    Row {
        case: "one user property alone",
        topic: "devices/mqtt5/sensor-4/state",
        payload: "{}",
        qos: 0,
        message_expiry: 0,
        content_type: None,
        response_topic: None,
        user_properties: Some(r#"[{"deviceCnt": "45"}]"#),
        expected_user_properties: &[("deviceCnt", "45")],
        payload_format_indicator: None,
        expected_payload_format: None,
        correlation_data: None,
    },
    Row {
        case: "a response topic and empty correlation data alone",
        topic: "devices/mqtt5/sensor-5/request",
        payload: r#"{"ask":"status"}"#,
        qos: 1,
        message_expiry: 0,
        content_type: None,
        response_topic: Some("devices/mqtt5/sensor-5/response"),
        user_properties: None,
        expected_user_properties: &[],
        payload_format_indicator: None,
        expected_payload_format: None,
        correlation_data: Some(("3q0=", &[0xde, 0xad])),
    },
    Row {
        case: "a message expiry alone, at the top of a day",
        topic: "devices/mqtt5/sensor-6/telemetry",
        payload: "86400",
        qos: 1,
        message_expiry: 86_400,
        content_type: None,
        response_topic: None,
        user_properties: None,
        expected_user_properties: &[],
        payload_format_indicator: None,
        expected_payload_format: None,
        correlation_data: None,
    },
];

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by. The signature is never checked in this harness.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One in-process API call through the whole router, as its status and body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
    signing_name: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, rendered)
}

/// `GET /endpoint?endpointType=iot:Data-ATS`, answering with the address the control plane
/// publishes for the account's ATS data endpoint.
async fn discover(router: &MultiServiceRouter) -> String {
    let (status, rendered) = call(
        router,
        "GET",
        &format!("{SERVED_AT}/endpoint?endpointType=iot%3AData-ATS"),
        "iot",
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "endpoint discovery must answer 200, got {status} with {rendered}"
    );
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    parsed
        .get("endpointAddress")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no endpointAddress in {rendered}"))
        .to_string()
}

/// The topic as one URI path LABEL: `Publish` binds the topic to `/topics/{topic}`, a
/// single path segment, so a multi-level topic's separators travel percent-encoded (as do
/// the other characters that would otherwise be read as path syntax).
fn encode_label(topic: &str) -> String {
    topic
        .chars()
        .map(|character| match character {
            '/' => "%2F".to_string(),
            '$' => "%24".to_string(),
            '?' => "%3F".to_string(),
            '#' => "%23".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// Make one row's publication through the data plane's `Publish`, with each parameter in
/// the binding the model gives it: `qos`, `contentType`, `responseTopic` and
/// `messageExpiry` as query parameters, and the user properties, payload format indicator
/// and correlation data as their own headers.
async fn publish(router: &MultiServiceRouter, row: &Row) {
    let mut query = format!("?qos={}", row.qos);
    if let Some(content_type) = row.content_type {
        query.push_str(&format!("&contentType={content_type}"));
    }
    if let Some(response_topic) = row.response_topic {
        query.push_str(&format!("&responseTopic={response_topic}"));
    }
    if row.message_expiry > 0 {
        query.push_str(&format!("&messageExpiry={}", row.message_expiry));
    }
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(properties) = row.user_properties {
        headers.push(("x-amz-mqtt5-user-properties", properties));
    }
    if let Some(indicator) = row.payload_format_indicator {
        headers.push(("x-amz-mqtt5-payload-format-indicator", indicator));
    }
    if let Some((encoded, _raw)) = row.correlation_data {
        headers.push(("x-amz-mqtt5-correlation-data", encoded));
    }
    let (status, rendered) = call(
        router,
        "POST",
        &format!("{SERVED_AT}/topics/{}{query}", encode_label(row.topic)),
        "iotdata",
        &headers,
        row.payload.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: the publish must be accepted, got {status} with {rendered}",
        row.case
    );
}

/// The port a published address names, checked to be on the LOOPBACK interface this
/// emulator's listeners are bound on — which is the host every connection below dials.
fn loopback_port(address: &str) -> u16 {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("{address:?} names no port"));
    assert_eq!(
        host,
        Ipv4Addr::LOCALHOST.to_string(),
        "expected the published address on the loopback interface, got {address:?}"
    );
    port.parse()
        .unwrap_or_else(|error| panic!("{address:?} names no port: {error}"))
}

/// A DEVICE speaking MQTT 5 over a real socket to the published address.
struct Device {
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial `address` and send the MQTT 5 CONNECT, answering with the device and the
    /// CONNACK the broker wrote.
    async fn connect(address: &str, client_id: &str) -> (Self, v5::ConnAck) {
        let mut device = Self::dial(address).await;
        let mut connect = v5::Connect::new(client_id);
        connect.clean_session = true;
        connect.keep_alive = 30;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.read_packet().await;
        let v5::Packet::ConnAck(connack) = answered else {
            panic!("expected a CONNACK, got {answered:?}");
        };
        (device, connack)
    }

    /// Open the socket alone, for a case that writes its own opening bytes.
    async fn dial(address: &str) -> Self {
        let port = loopback_port(address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("dialling {address} timed out"))
            .unwrap_or_else(|error| {
                panic!("the published address {address} must accept a connection: {error}")
            });
        Self {
            stream,
            buffer: BytesMut::new(),
        }
    }

    /// Write one packet, rendered by the codec.
    async fn write<Render>(&mut self, render: Render)
    where
        Render: FnOnce(&mut BytesMut) -> Result<usize, mqttbytes::Error>,
    {
        let mut rendered = BytesMut::new();
        render(&mut rendered).unwrap_or_else(|error| panic!("the packet renders: {error}"));
        self.write_bytes(&rendered).await;
    }

    /// Write bytes as they stand, for a case whose packet no codec here lays out.
    async fn write_bytes(&mut self, bytes: &[u8]) {
        self.stream
            .write_all(bytes)
            .await
            .unwrap_or_else(|error| panic!("the socket takes it: {error}"));
    }

    /// Read one MQTT 5 packet the broker wrote, waiting for it until
    /// [`EXCHANGE_DEADLINE`].
    async fn read_packet(&mut self) -> v5::Packet {
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match v5::read(&mut self.buffer, 256 * 1024) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => panic!("the broker wrote no packet: {error}"),
                }
                let mut chunk = [0u8; 4096];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => panic!("reading the socket failed: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the broker wrote nothing in time"));
        read.unwrap_or_else(|| panic!("the broker closed the connection"))
    }

    /// Read the raw bytes the broker wrote, for a case whose answer no codec here frames.
    async fn read_bytes(&mut self, wanted: usize) -> Vec<u8> {
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            while self.buffer.len() < wanted {
                let mut chunk = [0u8; 64];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => panic!("reading the socket failed: {error}"),
                }
            }
            self.buffer.to_vec()
        })
        .await;
        read.unwrap_or_else(|_| panic!("the broker wrote nothing in time"))
    }

    /// Subscribe to one filter at QoS 1, answering with the SUBACK.
    async fn subscribe(&mut self, filter: &str) -> v5::SubAck {
        let mut subscribe = v5::Subscribe::new(filter, QoS::AtLeastOnce);
        subscribe.pkid = 1;
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_packet().await;
        let v5::Packet::SubAck(suback) = answered else {
            panic!("expected a SUBACK, got {answered:?}");
        };
        suback
    }
}

/// The emulator for one case: the whole router on one shared engine, served at
/// [`SERVED_AT`].
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router
        .set_url_base(EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"));
    router
}

/// Run `body` on a thread with the stack the router's assembly needs, inside a runtime with
/// the IO the broker's own socket is bound on.
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

/// An MQTT 5 client opens a session at the published data endpoint, and a publication's
/// message properties reach it as the publisher set them.
#[test]
fn an_mqtt5_subscriber_is_handed_every_property_a_publication_set() {
    on_the_emulator("iot-mqtt5-properties", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            let (mut device, connack) = Device::connect(&address, SUBSCRIBER_CLIENT_ID).await;
            assert_eq!(
                connack.code,
                v5::ConnectReturnCode::Success,
                "case \"the MQTT 5 session\": expected the CONNECT accepted, got {:?}",
                connack.code
            );

            let suback = device.subscribe(SUBSCRIBED_FILTER).await;
            assert_eq!(
                suback.return_codes,
                vec![v5::SubscribeReasonCode::QoS1],
                "case \"the MQTT 5 session\": expected QoS 1 granted for \
                 {SUBSCRIBED_FILTER:?}, got {:?}",
                suback.return_codes
            );

            for row in ROWS {
                let case = row.case;
                publish(&router, row).await;
                let delivered = device.read_packet().await;
                let v5::Packet::Publish(publish) = delivered else {
                    panic!("case {case:?}: expected the publication, got {delivered:?}");
                };
                assert_eq!(
                    publish.topic, row.topic,
                    "case {case:?}: expected the topic published on, got {:?}",
                    publish.topic
                );
                assert_eq!(
                    publish.payload.as_ref(),
                    row.payload.as_bytes(),
                    "case {case:?}: expected the published bytes, got {:?}",
                    publish.payload
                );

                // A publication that set NO property arrives carrying no property section
                // at all, which is what tells a request-response client that none was set.
                let carries_one = row.content_type.is_some()
                    || row.response_topic.is_some()
                    || row.user_properties.is_some()
                    || row.payload_format_indicator.is_some()
                    || row.correlation_data.is_some()
                    || row.message_expiry > 0;
                assert_eq!(
                    publish.properties.is_some(),
                    carries_one,
                    "case {case:?}: expected a property section only where one was set, got \
                     {:?}",
                    publish.properties
                );
                let properties = publish.properties.unwrap_or_else(|| {
                    v5::PublishProperties {
                        payload_format_indicator: None,
                        message_expiry_interval: None,
                        topic_alias: None,
                        response_topic: None,
                        correlation_data: None,
                        user_properties: Vec::new(),
                        subscription_identifiers: Vec::new(),
                        content_type: None,
                    }
                });

                assert_eq!(
                    properties.content_type.as_deref(),
                    row.content_type,
                    "case {case:?}: expected the published content type, got {:?}",
                    properties.content_type
                );
                assert_eq!(
                    properties.response_topic.as_deref(),
                    row.response_topic,
                    "case {case:?}: expected the published response topic, got {:?}",
                    properties.response_topic
                );
                assert_eq!(
                    properties.payload_format_indicator, row.expected_payload_format,
                    "case {case:?}: expected the published payload format, got {:?}",
                    properties.payload_format_indicator
                );
                let expected_user_properties: Vec<(String, String)> = row
                    .expected_user_properties
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect();
                assert_eq!(
                    properties.user_properties, expected_user_properties,
                    "case {case:?}: expected the published user properties, got {:?}",
                    properties.user_properties
                );
                // The correlation data reaches the wire DECODED: the API carries it
                // base64-encoded, and what identifies a request to the receiver is the
                // bytes the sender chose.
                assert_eq!(
                    properties.correlation_data.as_deref(),
                    row.correlation_data.map(|(_encoded, raw)| raw),
                    "case {case:?}: expected the decoded correlation data, got {:?}",
                    properties.correlation_data
                );
                // The forwarded Message Expiry Interval is the published one less the time
                // the message waited here, so it is positive and never larger than what was
                // published.
                match row.message_expiry {
                    0 => assert!(
                        properties.message_expiry_interval.is_none(),
                        "case {case:?}: a publication that set no expiry must carry none, got \
                         {:?}",
                        properties.message_expiry_interval
                    ),
                    published => {
                        let forwarded = properties.message_expiry_interval.unwrap_or_else(|| {
                            panic!("case {case:?}: expected a forwarded message expiry")
                        });
                        assert!(
                            forwarded > 0 && i64::from(forwarded) <= published,
                            "case {case:?}: expected a forwarded expiry in (0, {published}], \
                             got {forwarded}"
                        );
                    }
                }

                if row.qos > 0 {
                    // A well-behaved client acknowledges a delivery made at QoS 1.
                    device
                        .write(|buffer| v5::PubAck::new(publish.pkid).write(buffer))
                        .await;
                }
            }
        });
    });
}

/// A CONNECT announcing a protocol level the broker serves no protocol for is REFUSED with
/// a CONNACK, not left silent: a client blocked on a CONNACK has no error to branch on.
///
/// The packet is laid out by hand because no codec here writes a version it does not
/// implement: the fixed header, the protocol name, the level, the connect flags, the
/// keep-alive and the client id (MQTT 3.1.1 section 3.1).
#[test]
fn a_connect_of_a_version_the_broker_does_not_serve_is_refused() {
    on_the_emulator("iot-mqtt-unsupported-version", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            // The levels this broker serves no protocol for, spanning both sides of the two
            // it does: MQTT 3.1's own level, and one past MQTT 5's.
            for level in [3u8, 6u8] {
                let case = format!("protocol level {level}");
                let client_id = b"unsupported-version-probe";
                let mut connect: Vec<u8> = vec![0x10];
                let mut body: Vec<u8> = vec![0x00, 0x04, b'M', b'Q', b'T', b'T', level, 0x02];
                // The keep-alive, then the client id as a length-prefixed string.
                body.extend_from_slice(&30u16.to_be_bytes());
                body.extend_from_slice(
                    &u16::try_from(client_id.len())
                        .expect("the client id is short")
                        .to_be_bytes(),
                );
                body.extend_from_slice(client_id);
                connect.push(u8::try_from(body.len()).expect("the packet is short"));
                connect.extend_from_slice(&body);

                let mut device = Device::dial(&address).await;
                device.write_bytes(&connect).await;
                // The CONNACK of MQTT 3.1.1, which is the one a client of an unknown
                // version has the best chance of reading: the packet type, its length, no
                // session present, and return code 1 (unacceptable protocol version).
                let answered = device.read_bytes(4).await;
                assert_eq!(
                    answered,
                    vec![0x20, 0x02, 0x00, 0x01],
                    "case {case:?}: expected a CONNACK refusing the protocol version, got \
                     {answered:?}"
                );
            }
        });
    });
}
