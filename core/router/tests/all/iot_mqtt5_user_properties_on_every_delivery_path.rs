// SPDX-License-Identifier: CC0-1.0

//! The MQTT 5 USER PROPERTIES a publication carries, on every path a message reaches a
//! subscriber by.
//!
//! The rule under test: a message's MQTT 5 properties belong to the MESSAGE, not to the
//! delivery that carries it — so every PUBLISH the broker writes to a subscriber that
//! negotiated MQTT 5 carries one User Property per entry of the `userProperties` document
//! the publication set (MQTT 5 section 3.3.2.3.7), with the names and values unchanged and
//! in the order the document wrote them, whichever path the message reached that subscriber
//! by: forwarded to a live subscription, pushed because a NEW subscription covers the topic
//! the message was retained on, or redelivered off a persistent session's queue when the
//! client returns. A document carrying no entry names no property. The member is a
//! `@mediaType("application/json")` string bound to the `x-amz-mqtt5-user-properties`
//! header (`PublishRequest$userProperties`), so the document travels either as itself or
//! base64-encoded, which is what an SDK sends — both name the same properties. A subscriber
//! of a version with no field for them (MQTT 3.1.1) receives the publication as its own
//! protocol frames it, with no property section at all.
//!
//! It needs the WHOLE router: the address a device connects to is published by the IoT
//! control plane (`DescribeEndpoint`), the publications are made through the IoT data
//! plane's `Publish` and the client's own offline/online state is read back through its
//! `GetConnection`, and the broker that forwards, retains and queues the message is in
//! neither.
//!
//! The rows differ along what the rule generalises over: how many entries the document
//! carries (none, one, two, five), whether their order is one a sort would also produce,
//! whether a name repeats, what a value holds (a JSON escape, the empty string) and which
//! encoding the header carries the document in.
//!
//! The sessions are spoken over real TCP sockets to the published address, with the packets
//! laid out by the same maintained MQTT codec a client library uses, so nothing of the
//! protocol is short-circuited. Assembling the router costs stack (see `boot_serve.rs`), so
//! it is built — and every leg driven — on an explicitly sized thread. The offline leg waits
//! for the CONDITION it needs (the broker reporting the client disconnected), polled through
//! the data plane rather than slept out.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::BytesMut;
use serde_json::Value;
// The MQTT codec the broker itself reads and writes the protocol with, reached through the
// service whose operations report on the sessions it opens: the suite speaks the wire a
// client library speaks rather than laying packets out by hand.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, v5, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// The topic-as-path-label escaping the sibling suite that publishes over HTTP already
// writes: the escaping lives in one place rather than being written a second time here.
use crate::iot_mqtt5_session_and_properties::encode_label;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "eu-west-3";

/// The address this emulator's API is served at, as the core publishes it from its data
/// port. The broker's own socket is a DIFFERENT address — one port serves one wire — and
/// which one that is, is the core's to choose, so every connection below reads it back
/// rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:39383";

/// How long a leg of the exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes, a session it reports offline), not the
/// speed of the host — a loaded machine takes longer and still passes.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// How long the suite leaves between two reads of a condition it is waiting for. It is a
/// POLLING interval, not a wait: the deadline above is what bounds the wait.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The largest packet read off a socket here, matching the broker's own ceiling.
const MAX_PACKET_BYTES: usize = 256 * 1024;

/// The topic tree every publication below is made in, and the filter the subscribers of
/// each leg subscribe to.
const TOPIC_BASE: &str = "fleet/mqtt5-properties";
/// See [`TOPIC_BASE`].
const SUBSCRIBED_FILTER: &str = "fleet/mqtt5-properties/+";

/// The payload every publication below carries. Non-empty, because a RETAINED publication
/// with an empty payload CLEARS the topic instead of retaining anything.
const PAYLOAD: &[u8] = b"{\"t\":21}";

/// One publication's `userProperties` document, and the User Properties an MQTT 5
/// subscriber must then be handed.
struct Row {
    case: &'static str,
    /// The last level of this row's own topic, so each row publishes on a topic of its own
    /// and a delivery carrying another row's properties is visible.
    leaf: &'static str,
    /// The `x-amz-mqtt5-user-properties` header value, exactly as the request carries it.
    header: &'static str,
    /// The name and value pairs the subscriber must receive, in this order.
    expected: &'static [(&'static str, &'static str)],
}

/// The rows: each a different document, so no single answer satisfies two of them.
const ROWS: &[Row] = &[
    Row {
        // The form the member's own documentation gives: "A JSON string that contains an
        // array of JSON objects ... `[{"deviceName": "alpha"}, {"deviceCnt": "45"}]`"
        // (`smithy_query(query="doc", shape="PublishRequest", member="userProperties")` on
        // the IoT data plane's model).
        case: "the two-entry document the member's own example names",
        leaf: "example",
        header: r#"[{"deviceName": "alpha"}, {"deviceCnt": "45"}]"#,
        expected: &[("deviceName", "alpha"), ("deviceCnt", "45")],
    },
    Row {
        case: "one entry alone",
        leaf: "single",
        header: r#"[{"tenant": "acme"}]"#,
        expected: &[("tenant", "acme")],
    },
    Row {
        // The names are in an order no sort produces, so a delivery that reordered them —
        // alphabetically, or by any other key — is visible.
        case: "five entries, in an order no sorting produces",
        leaf: "five",
        header: r#"[{"zulu": "1"}, {"alpha": "2"}, {"mike": "3"}, {"bravo": "4"}, {"yankee": "5"}]"#,
        expected: &[
            ("zulu", "1"),
            ("alpha", "2"),
            ("mike", "3"),
            ("bravo", "4"),
            ("yankee", "5"),
        ],
    },
    Row {
        // MQTT 5 section 3.3.2.3.7: "The same name is allowed to appear more than once", so
        // a repeated name is two properties and not one.
        case: "one name repeated with different values",
        leaf: "repeated",
        header: r#"[{"trace": "first"}, {"trace": "second"}]"#,
        expected: &[("trace", "first"), ("trace", "second")],
    },
    Row {
        // The document is JSON, so a value may be written with an escape and may be empty;
        // what travels is the value the JSON spells.
        case: "a value written with a JSON escape, and an empty value",
        leaf: "escaped",
        header: r#"[{"city": "Montr\u00e9al"}, {"blank": ""}]"#,
        expected: &[("city", "Montréal"), ("blank", "")],
    },
    Row {
        // The same document as "one entry alone", base64-encoded: the member is a
        // `@mediaType("application/json")` string bound to a header, so "you must encode
        // the JSON string to base64 format before adding it to the HTTP header", which is
        // the encoding an SDK applies for the caller. The base64 is written out rather than
        // computed, so the case states the encoding instead of borrowing it from the code
        // under test: it is `[{"tenant": "acme"}]`.
        case: "the document base64-encoded, as an SDK sends a JSON header",
        leaf: "base64",
        header: "W3sidGVuYW50IjogImFjbWUifV0=",
        expected: &[("tenant", "acme")],
    },
    Row {
        case: "an array with no entry, which names no property",
        leaf: "empty",
        header: "[]",
        expected: &[],
    },
];

/// This row's own topic.
fn topic_of(row: &Row) -> String {
    format!("{TOPIC_BASE}/{}", row.leaf)
}

/// The pairs one row expects, as the codec hands them over.
fn expected_pairs(row: &Row) -> Vec<(String, String)> {
    row.expected
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// The User Properties one delivered PUBLISH carries, in the order the packet wrote them. A
/// packet with no property section at all carries none.
fn delivered_user_properties(delivered: &v5::Publish) -> Vec<(String, String)> {
    delivered
        .properties
        .as_ref()
        .map(|properties| properties.user_properties.clone())
        .unwrap_or_default()
}

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
/// publishes for the account's ATS data endpoint — which is also the call that OPENS the
/// broker's socket.
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

/// Make one row's publication through the data plane's `Publish`, at QoS 1 and with the
/// row's document in the header the member is bound to.
async fn publish(router: &MultiServiceRouter, row: &Row, retain: bool) {
    let topic = topic_of(row);
    let (status, rendered) = call(
        router,
        "POST",
        &format!(
            "{SERVED_AT}/topics/{}?qos=1&retain={retain}",
            encode_label(&topic)
        ),
        "iotdata",
        &[("x-amz-mqtt5-user-properties", row.header)],
        PAYLOAD.to_vec(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: the publish on {topic} must be accepted, got {status} with {rendered}",
        row.case
    );
}

/// Wait until the broker REPORTS `client_id` disconnected, read through the data plane's
/// `GetConnection`. That is the condition a message's queueing depends on, so it is polled
/// for until [`EXCHANGE_DEADLINE`] rather than waited out for a fixed time.
async fn until_reported_offline(router: &MultiServiceRouter, client_id: &str) {
    let waited = tokio::time::timeout(EXCHANGE_DEADLINE, async {
        loop {
            let (status, rendered) = call(
                router,
                "GET",
                &format!("{SERVED_AT}/connections/{client_id}"),
                "iotdata",
                &[],
                Vec::new(),
            )
            .await;
            let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
            if status == 200 && parsed.get("connected").and_then(Value::as_bool) == Some(false) {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await;
    waited.unwrap_or_else(|_| {
        panic!("the broker must report {client_id} disconnected once its socket is gone")
    });
}

/// The port a published address names, checked to be on the LOOPBACK interface this
/// emulator's listeners are bound on — which is the host every connection below dials.
fn loopback_port(case: &str, address: &str) -> u16 {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {address:?} names no port"));
    assert_eq!(
        host,
        Ipv4Addr::LOCALHOST.to_string(),
        "case {case:?}: expected the published address on the loopback interface, got \
         {address:?}"
    );
    port.parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {address:?} names no port: {error}"))
}

/// A DEVICE: a real socket to the published address, speaking one of the two MQTT versions
/// the broker serves on it.
struct Device {
    case: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial the published address, with no byte written yet.
    async fn dial(case: &'static str, address: &str) -> Self {
        let port = loopback_port(case, address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("case {case:?}: dialling {address} timed out"))
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the published address {address} must accept a connection: \
                     {error}"
                )
            });
        Self {
            case,
            stream,
            buffer: BytesMut::new(),
        }
    }

    /// Open an MQTT 5 session, answering with the device and the CONNACK the broker wrote.
    /// `clean_session` is MQTT 5's Clean Start: a session that is to survive the connection
    /// asks for none.
    async fn connect_v5(
        case: &'static str,
        address: &str,
        client_id: &str,
        clean_session: bool,
    ) -> (Self, v5::ConnAck) {
        let mut device = Self::dial(case, address).await;
        let mut connect = v5::Connect::new(client_id);
        connect.clean_session = clean_session;
        connect.keep_alive = 30;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.read_v5().await;
        let v5::Packet::ConnAck(connack) = answered else {
            panic!("case {case:?}: expected a CONNACK, got {answered:?}");
        };
        assert_eq!(
            connack.code,
            v5::ConnectReturnCode::Success,
            "case {case:?}: expected the CONNECT accepted, got {:?}",
            connack.code
        );
        (device, connack)
    }

    /// Open an MQTT 3.1.1 session — the version that has no field for a message property.
    async fn connect_v311(case: &'static str, address: &str, client_id: &str) -> Self {
        let mut device = Self::dial(case, address).await;
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = true;
        connect.keep_alive = 30;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.read_v4().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!("case {case:?}: expected a CONNACK, got {answered:?}");
        };
        assert_eq!(
            connack.code,
            v4::ConnectReturnCode::Success,
            "case {case:?}: expected the CONNECT accepted, got {:?}",
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
        render(&mut rendered)
            .unwrap_or_else(|error| panic!("case {:?}: the packet renders: {error}", self.case));
        self.stream
            .write_all(&rendered)
            .await
            .unwrap_or_else(|error| panic!("case {:?}: the socket takes it: {error}", self.case));
    }

    /// Read one packet the broker wrote, framed by `frame`, waiting for it until
    /// [`EXCHANGE_DEADLINE`].
    async fn read_packet<Frame, Packet>(&mut self, frame: Frame) -> Packet
    where
        Frame: Fn(&mut BytesMut) -> Result<Packet, mqttbytes::Error>,
    {
        let case = self.case;
        let stream = &mut self.stream;
        let buffer = &mut self.buffer;
        let framed = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match frame(buffer) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => panic!("case {case:?}: the broker wrote no packet: {error}"),
                }
                let mut chunk = [0u8; 4096];
                match stream.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(taken) => buffer.extend_from_slice(&chunk[..taken]),
                    Err(error) => panic!("case {case:?}: reading the socket failed: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the broker wrote nothing in time"));
        framed.unwrap_or_else(|| panic!("case {case:?}: the broker closed the connection"))
    }

    /// Read one MQTT 5 packet.
    async fn read_v5(&mut self) -> v5::Packet {
        self.read_packet(|buffer| v5::read(buffer, MAX_PACKET_BYTES))
            .await
    }

    /// Read one MQTT 3.1.1 packet.
    async fn read_v4(&mut self) -> v4::Packet {
        self.read_packet(|buffer| v4::read(buffer, MAX_PACKET_BYTES))
            .await
    }

    /// Subscribe to one filter at QoS 1 over MQTT 5, asserting it was granted.
    async fn subscribe_v5(&mut self, filter: &str) {
        let case = self.case;
        let mut subscribe = v5::Subscribe::new(filter, QoS::AtLeastOnce);
        subscribe.pkid = 1;
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_v5().await;
        let v5::Packet::SubAck(suback) = answered else {
            panic!("case {case:?}: expected a SUBACK, got {answered:?}");
        };
        assert_eq!(
            suback.return_codes,
            vec![v5::SubscribeReasonCode::QoS1],
            "case {case:?}: expected QoS 1 granted for {filter:?}, got {:?}",
            suback.return_codes
        );
    }

    /// Subscribe to one filter at QoS 1 over MQTT 3.1.1, asserting it was granted.
    async fn subscribe_v311(&mut self, filter: &str) {
        let case = self.case;
        let mut subscribe = v4::Subscribe::new(filter, QoS::AtLeastOnce);
        subscribe.pkid = 1;
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_v4().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("case {case:?}: expected a SUBACK, got {answered:?}");
        };
        assert_eq!(
            suback.return_codes,
            vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce)],
            "case {case:?}: expected QoS 1 granted for {filter:?}, got {:?}",
            suback.return_codes
        );
    }

    /// The next PUBLISH the broker wrote over MQTT 5, acknowledged as a well-behaved client
    /// acknowledges a delivery made at QoS 1.
    async fn next_v5_publish(&mut self, case: &str) -> v5::Publish {
        let packet = self.read_v5().await;
        let v5::Packet::Publish(delivered) = packet else {
            panic!("case {case:?}: expected a publication, got {packet:?}");
        };
        if delivered.qos != QoS::AtMostOnce {
            let pkid = delivered.pkid;
            self.write(|buffer| v5::PubAck::new(pkid).write(buffer))
                .await;
        }
        delivered
    }

    /// The next PUBLISH the broker wrote over MQTT 3.1.1, acknowledged the same way.
    async fn next_v311_publish(&mut self, case: &str) -> v4::Publish {
        let packet = self.read_v4().await;
        let v4::Packet::Publish(delivered) = packet else {
            panic!("case {case:?}: expected a publication, got {packet:?}");
        };
        if delivered.qos != QoS::AtMostOnce {
            let pkid = delivered.pkid;
            self.write(|buffer| v4::PubAck::new(pkid).write(buffer))
                .await;
        }
        delivered
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

/// What each row's topic was delivered with, as a delivered PUBLISH is read off a socket:
/// its User Properties and its RETAIN flag.
type Delivered = BTreeMap<String, (Vec<(String, String)>, bool)>;

/// FORWARDED to a live subscription: every entry of the published document reaches the MQTT
/// 5 subscriber as one User Property, in published order — while the MQTT 3.1.1 subscriber
/// of the same publication is handed the message its own protocol can frame, with no
/// property section at all.
#[test]
fn published_user_properties_reach_a_live_mqtt5_subscriber_in_order() {
    on_the_emulator("iot-mqtt5-user-properties-live", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            let (mut subscriber, _connack) =
                Device::connect_v5("the MQTT 5 subscription", &address, "props-live-v5", true)
                    .await;
            subscriber.subscribe_v5(SUBSCRIBED_FILTER).await;
            let mut older =
                Device::connect_v311("the MQTT 3.1.1 subscription", &address, "props-live-v311")
                    .await;
            older.subscribe_v311(SUBSCRIBED_FILTER).await;

            for row in ROWS {
                let case = row.case;
                publish(&router, row, false).await;

                let delivered = subscriber.next_v5_publish(case).await;
                assert_eq!(
                    delivered.topic,
                    topic_of(row),
                    "case {case:?}: expected the topic published on, got {:?}",
                    delivered.topic
                );
                assert_eq!(
                    delivered.payload.as_ref(),
                    PAYLOAD,
                    "case {case:?}: expected the published bytes, got {:?}",
                    delivered.payload
                );
                assert_eq!(
                    delivered_user_properties(&delivered),
                    expected_pairs(row),
                    "case {case:?}: expected the published user properties, got {:?}",
                    delivered_user_properties(&delivered)
                );

                // The same publication over the version that has no field for a message
                // property: the 3.1.1 subscriber is handed the topic, the payload and the
                // QoS, read by its own codec — so nothing of the property section reaches a
                // client that could not read one.
                let plain = older.next_v311_publish(case).await;
                assert_eq!(
                    (plain.topic.as_str(), plain.payload.as_ref()),
                    (topic_of(row).as_str(), PAYLOAD),
                    "case {case:?}: expected the MQTT 3.1.1 subscriber handed the same \
                     publication, got {plain:?}"
                );
            }
        });
    });
}

/// RETAINED: the document a retained publication carried is delivered to a subscription
/// made AFTERWARDS, with the RETAIN flag set — the properties outlive the publication, as
/// the retained message itself does.
#[test]
fn retained_user_properties_reach_a_subscription_made_afterwards() {
    on_the_emulator("iot-mqtt5-user-properties-retained", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            for row in ROWS {
                publish(&router, row, true).await;
            }

            // The subscription is made after every publication, so each delivery below is a
            // retained message handed over because this filter now covers its topic.
            let (mut subscriber, _connack) =
                Device::connect_v5("the later subscription", &address, "props-retained-v5", true)
                    .await;
            subscriber.subscribe_v5(SUBSCRIBED_FILTER).await;
            let mut delivered: Delivered = BTreeMap::new();
            for _ in ROWS {
                let push = subscriber.next_v5_publish("the retained push").await;
                assert_eq!(
                    push.payload.as_ref(),
                    PAYLOAD,
                    "case \"the retained push\": expected the published bytes on {}, got {:?}",
                    push.topic,
                    push.payload
                );
                delivered.insert(
                    push.topic.clone(),
                    (delivered_user_properties(&push), push.retain),
                );
            }

            for row in ROWS {
                let case = row.case;
                let topic = topic_of(row);
                let (properties, retained) = delivered.get(&topic).unwrap_or_else(|| {
                    panic!("case {case:?}: nothing was handed over for {topic}")
                });
                assert_eq!(
                    *properties,
                    expected_pairs(row),
                    "case {case:?}: expected the retained user properties of {topic}, got \
                     {properties:?}"
                );
                assert!(
                    *retained,
                    "case {case:?}: a message handed over because a subscription was just \
                     made must carry the RETAIN flag, got {retained}"
                );
            }
        });
    });
}

/// QUEUED: a message published while the client is away is redelivered when its persistent
/// session resumes, carrying the User Properties the publication set — the queue holds the
/// message, not just its bytes.
#[test]
fn queued_user_properties_reach_a_resumed_persistent_session() {
    on_the_emulator("iot-mqtt5-user-properties-queued", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            let client_id = "props-queued-v5";
            let case = "the persistent session";

            // A session that is to survive its connection asks for no clean start, and keeps
            // the subscriptions it made while it is away.
            let (mut subscriber, _connack) =
                Device::connect_v5(case, &address, client_id, false).await;
            subscriber.subscribe_v5(SUBSCRIBED_FILTER).await;
            drop(subscriber);
            until_reported_offline(&router, client_id).await;

            for row in ROWS {
                publish(&router, row, false).await;
            }

            let (mut resumed, connack) = Device::connect_v5(case, &address, client_id, false).await;
            assert!(
                connack.session_present,
                "case {case:?}: expected the stored session resumed, got {connack:?}"
            );
            let mut delivered: Delivered = BTreeMap::new();
            for _ in ROWS {
                let redelivered = resumed.next_v5_publish("the queued redelivery").await;
                assert_eq!(
                    redelivered.payload.as_ref(),
                    PAYLOAD,
                    "case \"the queued redelivery\": expected the published bytes on {}, got \
                     {:?}",
                    redelivered.topic,
                    redelivered.payload
                );
                delivered.insert(
                    redelivered.topic.clone(),
                    (delivered_user_properties(&redelivered), redelivered.retain),
                );
            }

            for row in ROWS {
                let row_case = row.case;
                let topic = topic_of(row);
                let (properties, retained) = delivered.get(&topic).unwrap_or_else(|| {
                    panic!("case {row_case:?}: nothing was redelivered for {topic}")
                });
                assert_eq!(
                    *properties,
                    expected_pairs(row),
                    "case {row_case:?}: expected the queued user properties of {topic}, got \
                     {properties:?}"
                );
                assert!(
                    !*retained,
                    "case {row_case:?}: a message redelivered off the queue was forwarded by \
                     ordinary publishing, so its RETAIN flag must be clear, got {retained}"
                );
            }
        });
    });
}
