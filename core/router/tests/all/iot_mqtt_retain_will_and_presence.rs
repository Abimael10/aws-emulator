// SPDX-License-Identifier: CC0-1.0

//! What the IoT message broker does for a session BESIDE carrying its packets, across the
//! two IoT services: the control plane publishes the account's data endpoint
//! (`DescribeEndpoint`), a device opens a real MQTT connection to that address, and what
//! the broker then retains, pushes, wills and announces is observed on that wire and
//! through the data plane's own retained-message operations.
//!
//! The rules under test, each in its own case:
//!
//! * a subscription's multi-level wildcard stands for the levels BELOW the filter's
//!   literal prefix, so `<prefix>/#` covers the subtree and not the prefix's own topic,
//!   while the whole-tree filter `#` covers every topic;
//! * a persistent session holds the QoS 1 messages addressed to it while its client is
//!   away and DISCARDS the QoS 0 ones, so a resumed session is handed only what was
//!   promised delivery;
//! * a message published with the RETAIN flag is the topic's retained message until it is
//!   overwritten or cleared by a retained publication with an empty payload, and every
//!   later subscription whose filter covers that topic is pushed it with the RETAIN flag
//!   SET — the same store the data plane's `GetRetainedMessage` reports from, whichever
//!   wire retained the message;
//! * a session's last will is published when the session ends WITHOUT the client's own
//!   DISCONNECT, and is suppressed when the client disconnects cleanly;
//! * the broker publishes a connect and a disconnect event on the reserved presence topics
//!   of the client id, and the disconnect event's reason tells the client's own disconnect
//!   from a takeover of its client id and from a disconnect the service performed.
//!
//! It needs the WHOLE router: the endpoint is published by the IoT control-plane crate, the
//! retained-message and connection operations by the IoT data-plane crate, and the broker
//! that holds sessions and retained messages is in neither.
//!
//! The connection is spoken over a real TCP socket to the published address, with the
//! packets laid out by the same maintained MQTT codec a client library uses, so nothing of
//! the protocol is short-circuited. Assembling the router costs stack (see
//! `boot_serve.rs`), so it is built — and every leg driven — on an explicitly sized thread.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::BytesMut;
use serde_json::Value;
// The MQTT codec the broker itself reads and writes the protocol with, reached through the
// service whose operations report on the sessions and retained messages it holds
// (`service_common::iot_mqtt_broker`, re-exported there): the suite speaks the wire a client
// library speaks rather than laying packets out by hand.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::query_wire::form_encode;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "eu-west-1";

/// The address this emulator's API is served at, as the core publishes it from its data
/// port. The broker's own socket is a DIFFERENT address — one port serves one wire — and
/// which one that is, is the core's to choose, so every assertion about it reads it back
/// rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:39377";

/// How long one leg of an exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes, a record it updates), not the speed of
/// the host — a loaded machine takes longer and still passes.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// The reserved topic space the broker publishes its presence events in, and the reasons a
/// disconnect event reports — the values `disconnectReason` takes for a client that hung up
/// itself, a client id another connection took over, and a disconnect the service
/// performed.
const PRESENCE_PREFIX: &str = "$aws/events/presence";
/// See [`PRESENCE_PREFIX`].
const CLIENT_INITIATED_DISCONNECT: &str = "CLIENT_INITIATED_DISCONNECT";
/// See [`PRESENCE_PREFIX`].
const DUPLICATE_CLIENTID: &str = "DUPLICATE_CLIENTID";
/// See [`PRESENCE_PREFIX`].
const SERVER_INITIATED_DISCONNECT: &str = "SERVER_INITIATED_DISCONNECT";

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by. The signature is never checked in this harness.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One in-process API call through the whole router, as its status and parsed body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
    signing_name: &str,
    body: &str,
) -> (u16, String, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    (status, rendered, parsed)
}

/// `GET /endpoint?endpointType=iot:Data-ATS`, answering with the broker address the control
/// plane publishes.
async fn discover(router: &MultiServiceRouter, case: &str) -> String {
    let (status, rendered, parsed) = call(
        router,
        "GET",
        &format!("{SERVED_AT}/endpoint?endpointType=iot%3AData-ATS"),
        "iot",
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: endpoint discovery must answer 200, got {status} with {rendered}"
    );
    parsed
        .get("endpointAddress")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no endpointAddress in {rendered}"))
        .to_string()
}

/// `POST /topics/{topic}` on the data plane: a publication made over the HTTPS wire, with
/// the RETAIN flag the caller asked for.
async fn publish_over_http(
    router: &MultiServiceRouter,
    case: &str,
    address: &str,
    topic: &str,
    qos: u8,
    retain: bool,
    payload: &str,
) {
    let (status, rendered, _parsed) = call(
        router,
        "POST",
        &format!(
            "https://{address}/topics/{}?qos={qos}&retain={retain}",
            form_encode(topic)
        ),
        "iotdata",
        payload,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: publishing on {topic} must answer 200, got {status} with {rendered}"
    );
}

/// `GET /retainedMessage/{topic}`: the status and the body the data plane reports a topic's
/// retained message with.
async fn retained_message(
    router: &MultiServiceRouter,
    address: &str,
    topic: &str,
) -> (u16, String, Value) {
    call(
        router,
        "GET",
        &format!("https://{address}/retainedMessage/{}", form_encode(topic)),
        "iotdata",
        "",
    )
    .await
}

/// Wait until the data plane reports `client_id` disconnected, which is the record the
/// broker writes when a connection ends. Polled rather than waited on: the broker writes it
/// when the connection's own task reaches the end of the socket.
async fn await_disconnected(router: &MultiServiceRouter, case: &str, address: &str, client_id: &str) {
    let deadline = tokio::time::Instant::now() + EXCHANGE_DEADLINE;
    loop {
        let (status, rendered, parsed) = call(
            router,
            "GET",
            &format!("https://{address}/connections/{client_id}?includeSocketInformation=false"),
            "iotdata",
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session of {client_id} must still be reported, got {status} \
             with {rendered}"
        );
        if parsed.get("connected").and_then(Value::as_bool) == Some(false) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the end of {client_id}'s connection was never recorded"
        );
        tokio::task::yield_now().await;
    }
}

/// The port a published address names, checked to be on the LOOPBACK interface this
/// emulator's listeners are bound on.
fn loopback_port(case: &str, address: &str) -> u16 {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {address:?} names no port"));
    assert_eq!(
        host,
        Ipv4Addr::LOCALHOST.to_string(),
        "case {case:?}: expected the published address on the loopback interface, got {address:?}"
    );
    port.parse()
        .unwrap_or_else(|error| panic!("case {case:?}: {address:?} names no port: {error}"))
}

/// The QoS a number names, as a client's own library reads it.
fn qos_of(number: u8) -> QoS {
    match number {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

/// One Last Will and Testament a device's CONNECT configures.
struct Will {
    topic: &'static str,
    payload: &'static str,
    qos: u8,
    retain: bool,
}

/// One delivery a device read off its connection: the topic, the payload and whether the
/// PUBLISH carried the RETAIN flag.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Delivered {
    topic: String,
    payload: Vec<u8>,
    retained: bool,
}

/// A DEVICE: a real socket to the published address, speaking MQTT 3.1.1 on it.
struct Device {
    case: String,
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial `address` and send the CONNECT, answering with the device and the CONNACK the
    /// broker wrote.
    async fn connect(
        case: &str,
        address: &str,
        client_id: &str,
        clean_session: bool,
        will: Option<&Will>,
    ) -> (Self, v4::ConnAck) {
        let port = loopback_port(case, address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("case {case:?}: dialling {address} timed out"))
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the published address {address} must accept a connection: {error}")
            });
        let mut device = Self {
            case: case.to_string(),
            stream,
            buffer: BytesMut::new(),
        };
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = clean_session;
        connect.keep_alive = 60;
        if let Some(will) = will {
            connect.last_will = Some(v4::LastWill::new(
                will.topic,
                will.payload.as_bytes().to_vec(),
                qos_of(will.qos),
                will.retain,
            ));
        }
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.expect_packet().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!("case {case:?}: expected a CONNACK, got {answered:?}");
        };
        assert_eq!(
            connack.code,
            v4::ConnectReturnCode::Success,
            "case {case:?}: the broker must accept the CONNECT of {client_id}, got {:?}",
            connack.code
        );
        (device, connack)
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

    /// Read one packet the broker wrote, waiting for it until [`EXCHANGE_DEADLINE`].
    async fn expect_packet(&mut self) -> v4::Packet {
        let case = self.case.clone();
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match v4::read(&mut self.buffer, 256 * 1024) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => panic!("case {case:?}: the broker wrote no packet: {error}"),
                }
                let mut chunk = [0u8; 4096];
                match self.stream.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => panic!("case {case:?}: reading the socket failed: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the broker wrote nothing in time"));
        read.unwrap_or_else(|| panic!("case {case:?}: the broker closed the connection"))
    }

    /// Read one PUBLISH the broker delivered, as its topic, payload and RETAIN flag.
    async fn expect_delivery(&mut self) -> Delivered {
        let packet = self.expect_packet().await;
        let v4::Packet::Publish(publish) = packet else {
            panic!("case {:?}: expected a delivery, got {packet:?}", self.case);
        };
        Delivered {
            topic: publish.topic,
            payload: publish.payload.to_vec(),
            retained: publish.retain,
        }
    }

    /// Read the deliveries up to and INCLUDING the one on `sentinel`, which is published
    /// after whatever the case is waiting for: the sentinel is the condition that says
    /// every earlier delivery has arrived, so a negative assertion needs no wall clock.
    async fn deliveries_until(&mut self, sentinel: &str) -> Vec<Delivered> {
        let mut seen = Vec::new();
        loop {
            let delivered = self.expect_delivery().await;
            let reached = delivered.topic == sentinel;
            seen.push(delivered);
            if reached {
                return seen;
            }
        }
    }

    /// Subscribe to `filters`, answering with the granted QoS of each.
    async fn subscribe(&mut self, packet_id: u16, filters: &[(&str, u8)]) -> Vec<i32> {
        let mut subscribe = v4::Subscribe::new(filters[0].0, qos_of(filters[0].1));
        subscribe.pkid = packet_id;
        for (path, qos) in &filters[1..] {
            subscribe
                .filters
                .push(v4::SubscribeFilter::new((*path).to_string(), qos_of(*qos)));
        }
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.expect_packet().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("case {:?}: expected a SUBACK, got {answered:?}", self.case);
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
                refused => panic!("case {:?}: a filter was refused: {refused:?}", self.case),
            })
            .collect()
    }

    /// Publish one message over this device's own connection, waiting for the PUBACK that
    /// says the broker took it when the publication asked for one.
    async fn publish(&mut self, packet_id: u16, topic: &str, payload: &str, qos: u8, retain: bool) {
        let mut publish = v4::Publish::new(topic, qos_of(qos), payload.as_bytes().to_vec());
        publish.retain = retain;
        if qos > 0 {
            publish.pkid = packet_id;
        }
        self.write(|buffer| publish.write(buffer)).await;
        if qos > 0 {
            let answered = self.expect_packet().await;
            let v4::Packet::PubAck(puback) = answered else {
                panic!("case {:?}: expected a PUBACK, got {answered:?}", self.case);
            };
            assert_eq!(
                puback.pkid, packet_id,
                "case {:?}: expected the PUBACK of the publication, got {puback:?}",
                self.case
            );
        }
    }

    /// Send the client's own DISCONNECT: the goodbye that suppresses the session's will.
    async fn disconnect(&mut self) {
        self.write(|buffer| v4::Disconnect.write(buffer)).await;
    }
}

/// The emulator for one case: the whole router on one shared engine, served at
/// [`SERVED_AT`].
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router.set_url_base(EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"));
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

/// One row of the wildcard cases: the filter a subscriber asks for, and the topics of the
/// published set it must be handed.
struct WildcardRow {
    case: &'static str,
    client_id: &'static str,
    filter: &'static str,
    received: &'static [&'static str],
}

/// The published set every row is measured against: a parent topic, one level below it and
/// two levels below it — the three depths a filter's wildcard can reach differently.
const WILDCARD_TOPICS: &[&str] = &["wild/parent", "wild/parent/one", "wild/parent/one/two"];

/// The topic prefix each wildcard row's own SENTINEL publication is made on: a topic
/// outside the published set, which the row subscribes to beside its filter, so reading up
/// to it says every delivery that filter was owed has arrived.
const WILDCARD_SENTINEL_PREFIX: &str = "sentinel";

/// The rows: the multi-level wildcard at a prefix and at the root, the single-level wildcard
/// at the end and in the middle, and a filter with no wildcard at all.
const WILDCARD_ROWS: &[WildcardRow] = &[
    WildcardRow {
        case: "a multi-level wildcard below a prefix",
        client_id: "wildcard-subtree",
        filter: "wild/parent/#",
        received: &["wild/parent/one", "wild/parent/one/two"],
    },
    WildcardRow {
        case: "the whole-tree filter",
        client_id: "wildcard-everything",
        filter: "#",
        received: &["wild/parent", "wild/parent/one", "wild/parent/one/two"],
    },
    WildcardRow {
        case: "a multi-level wildcard a level deeper",
        client_id: "wildcard-deeper",
        filter: "wild/parent/one/#",
        received: &["wild/parent/one/two"],
    },
    WildcardRow {
        case: "a single-level wildcard at the end",
        client_id: "wildcard-one-level",
        filter: "wild/parent/+",
        received: &["wild/parent/one"],
    },
    WildcardRow {
        case: "a single-level wildcard in the middle",
        client_id: "wildcard-middle",
        filter: "wild/+/one",
        received: &["wild/parent/one"],
    },
    WildcardRow {
        case: "a filter with no wildcard",
        client_id: "wildcard-exact",
        filter: "wild/parent",
        received: &["wild/parent"],
    },
];

/// A subscription's multi-level wildcard stands for the levels BELOW the filter's prefix:
/// `<prefix>/#` is handed the subtree and not the prefix's own topic, while `#` is handed
/// every topic.
#[test]
fn a_multi_level_wildcard_covers_only_the_levels_below_its_prefix() {
    on_the_emulator("iot-mqtt-wildcard-depth", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router, "the broker's address").await;
            for row in WILDCARD_ROWS {
                let case = row.case;
                // A clean session, so the subscriber the row disconnects leaves no
                // subscription behind for the next row's publications.
                let (mut device, _connack) =
                    Device::connect(case, &address, row.client_id, true, None).await;
                // Beside the filter under test, the row's own sentinel topic: an exact
                // filter on a topic outside the published set.
                let sentinel = format!("{WILDCARD_SENTINEL_PREFIX}/{}", row.client_id);
                let granted = device
                    .subscribe(1, &[(row.filter, 1), (sentinel.as_str(), 1)])
                    .await;
                assert_eq!(
                    granted,
                    vec![1, 1],
                    "case {case:?}: expected QoS 1 granted for {:?}, got {granted:?}",
                    row.filter
                );

                for topic in WILDCARD_TOPICS {
                    publish_over_http(&router, case, &address, topic, 1, false, "measurement")
                        .await;
                }
                // The sentinel is published last, so reading up to it says every delivery
                // of this row has arrived.
                publish_over_http(&router, case, &address, &sentinel, 1, false, "sentinel").await;

                let mut delivered: Vec<String> = device
                    .deliveries_until(&sentinel)
                    .await
                    .into_iter()
                    .map(|delivery| delivery.topic)
                    .collect();
                // The sentinel's own delivery is the stop marker, not part of the set.
                delivered.pop();
                assert_eq!(
                    delivered,
                    row.received.iter().map(|topic| (*topic).to_string()).collect::<Vec<String>>(),
                    "case {case:?}: expected the filter {:?} to be handed {:?}, got {delivered:?}",
                    row.filter,
                    row.received
                );
                device.disconnect().await;
                await_disconnected(&router, case, &address, row.client_id).await;
            }
        });
    });
}

/// A persistent session holds what was promised delivery while its client was away: the
/// QoS 1 messages addressed to it are handed over on resume, and the QoS 0 ones — which
/// promise no delivery at all — are discarded rather than replayed.
#[test]
fn an_offline_persistent_session_keeps_only_what_was_published_at_qos_1() {
    on_the_emulator("iot-mqtt-offline-queue", |router, runtime| {
        runtime.block_on(async {
            let case = "an offline persistent session";
            let address = discover(&router, case).await;
            let client_id = "resume-probe";
            let sentinel = format!("{WILDCARD_SENTINEL_PREFIX}/{client_id}");

            // A PERSISTENT session (cleanSession=false), so its subscriptions and what is
            // addressed to it outlive the connection.
            let (mut device, connack) =
                Device::connect(case, &address, client_id, false, None).await;
            assert!(
                !connack.session_present,
                "case {case:?}: a client id that never connected has no session to resume"
            );
            device
                .subscribe(1, &[("resume/#", 1), (sentinel.as_str(), 1)])
                .await;
            device.disconnect().await;
            await_disconnected(&router, case, &address, client_id).await;

            // Published while the client is away: two at QoS 0 and one at QoS 1, on topics
            // the session's own filter covers.
            for (topic, qos, payload) in [
                ("resume/zero-one", 0, "at most once 1"),
                ("resume/one", 1, "at least once"),
                ("resume/zero-two", 0, "at most once 2"),
            ] {
                publish_over_http(&router, case, &address, topic, qos, false, payload).await;
            }

            // The client returns to the SAME session, and what it is handed is what the
            // session kept: the QoS 1 message alone, and then the sentinel published after
            // the resume — so nothing else was waiting for it.
            let (mut resumed, connack) =
                Device::connect(case, &address, client_id, false, None).await;
            assert!(
                connack.session_present,
                "case {case:?}: expected the persistent session resumed"
            );
            publish_over_http(&router, case, &address, &sentinel, 1, false, "sentinel").await;
            let handed = resumed.deliveries_until(&sentinel).await;
            assert_eq!(
                handed,
                vec![
                    Delivered {
                        topic: "resume/one".to_string(),
                        payload: b"at least once".to_vec(),
                        retained: false,
                    },
                    Delivered {
                        topic: sentinel.clone(),
                        payload: b"sentinel".to_vec(),
                        retained: false,
                    },
                ],
                "case {case:?}: expected only the QoS 1 message replayed, got {handed:?}"
            );
        });
    });
}

/// A retained message is pushed to every later subscription its topic is covered by, with
/// the RETAIN flag set — whichever wire retained it, and until it is overwritten or cleared.
#[test]
fn a_retained_message_is_pushed_to_a_new_subscription_with_the_retain_flag() {
    on_the_emulator("iot-mqtt-retained-push", |router, runtime| {
        runtime.block_on(async {
            let case = "a retained message on two wires";
            let address = discover(&router, case).await;

            // One topic retained over the MQTT wire, one over the HTTPS data plane: the
            // store is the broker's, so both are pushed to a later subscriber.
            let (mut publisher, _connack) =
                Device::connect(case, &address, "retain-publisher", true, None).await;
            publisher
                .publish(1, "retain/config/alpha", "alpha-1", 1, true)
                .await;
            publish_over_http(&router, case, &address, "retain/config/beta", 1, true, "beta-1")
                .await;

            // What the data plane reports for the message the MQTT wire retained — the same
            // store, read through the operation a client reads it with.
            let (status, rendered, parsed) =
                retained_message(&router, &address, "retain/config/alpha").await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetRetainedMessage must report the MQTT-retained message, got \
                 {status} with {rendered}"
            );
            let reported = parsed
                .get("payload")
                .and_then(Value::as_str)
                .map(|encoded| {
                    aws_smithy_types::base64::decode(encoded)
                        .unwrap_or_else(|error| panic!("case {case:?}: payload is not base64: {error}"))
                })
                .unwrap_or_else(|| panic!("case {case:?}: no payload in {rendered}"));
            assert_eq!(
                reported, b"alpha-1".to_vec(),
                "case {case:?}: expected the retained bytes, got {reported:?}"
            );
            assert_eq!(
                parsed.get("qos").and_then(Value::as_i64),
                Some(1),
                "case {case:?}: expected the QoS the message was published at, got {rendered}"
            );
            assert!(
                parsed
                    .get("lastModifiedTime")
                    .and_then(Value::as_i64)
                    .is_some_and(|stored| stored > 0),
                "case {case:?}: expected the instant the message was stored, got {rendered}"
            );

            // A fresh subscription is pushed both retained messages, in topic order, each
            // with the RETAIN flag SET — that is what tells the client the message was
            // retained for it rather than published while it was listening.
            let (mut subscriber, _connack) =
                Device::connect(case, &address, "retain-subscriber", true, None).await;
            subscriber.subscribe(1, &[("retain/config/+", 1)]).await;
            let pushed = vec![
                subscriber.expect_delivery().await,
                subscriber.expect_delivery().await,
            ];
            assert_eq!(
                pushed,
                vec![
                    Delivered {
                        topic: "retain/config/alpha".to_string(),
                        payload: b"alpha-1".to_vec(),
                        retained: true,
                    },
                    Delivered {
                        topic: "retain/config/beta".to_string(),
                        payload: b"beta-1".to_vec(),
                        retained: true,
                    },
                ],
                "case {case:?}: expected both retained messages pushed with the RETAIN flag, \
                 got {pushed:?}"
            );

            // An ordinary publication on the same topic reaches the live subscriber with the
            // flag CLEAR, which is what distinguishes the two.
            publish_over_http(&router, case, &address, "retain/config/alpha", 1, false, "live")
                .await;
            let live = subscriber.expect_delivery().await;
            assert_eq!(
                (live.topic.as_str(), live.payload.as_slice(), live.retained),
                ("retain/config/alpha", &b"live"[..], false),
                "case {case:?}: expected a live delivery with no RETAIN flag, got {live:?}"
            );

            // The retained message is REPLACED by the next retained publication on its
            // topic: one topic holds one.
            publisher
                .publish(2, "retain/config/alpha", "alpha-2", 1, true)
                .await;
            let (mut later, _connack) =
                Device::connect(case, &address, "retain-later", true, None).await;
            later.subscribe(1, &[("retain/config/alpha", 1)]).await;
            let replaced = later.expect_delivery().await;
            assert_eq!(
                (
                    replaced.topic.as_str(),
                    replaced.payload.as_slice(),
                    replaced.retained
                ),
                ("retain/config/alpha", &b"alpha-2"[..], true),
                "case {case:?}: expected the newest retained message, got {replaced:?}"
            );

            // A retained publication with an EMPTY payload clears the topic: nothing is
            // pushed to a later subscription, and the data plane reports the topic as
            // holding no retained message.
            publisher.publish(3, "retain/config/alpha", "", 1, true).await;
            publish_over_http(&router, case, &address, "retain/config/beta", 1, true, "").await;
            let (status, rendered, _parsed) =
                retained_message(&router, &address, "retain/config/alpha").await;
            assert_eq!(
                status, 404,
                "case {case:?}: a cleared topic holds no retained message, got {status} with \
                 {rendered}"
            );
            let (mut cleared, _connack) =
                Device::connect(case, &address, "retain-cleared", true, None).await;
            cleared.subscribe(1, &[("retain/config/+", 1)]).await;
            publish_over_http(
                &router,
                case,
                &address,
                "retain/config/alpha",
                1,
                false,
                "sentinel",
            )
            .await;
            let after_clear = cleared.deliveries_until("retain/config/alpha").await;
            assert_eq!(
                after_clear,
                vec![Delivered {
                    topic: "retain/config/alpha".to_string(),
                    payload: b"sentinel".to_vec(),
                    retained: false,
                }],
                "case {case:?}: expected nothing retained pushed after the topics were \
                 cleared, got {after_clear:?}"
            );
        });
    });
}

/// A session's last will is published when the session ends without the client's own
/// DISCONNECT, and is suppressed when the client disconnects cleanly.
#[test]
fn a_will_fires_on_an_ungraceful_close_and_not_on_a_clean_disconnect() {
    on_the_emulator("iot-mqtt-last-will", |router, runtime| {
        runtime.block_on(async {
            let case = "a session with a will";
            let address = discover(&router, case).await;

            // The consumer of the will topics, subscribed before any of them ends.
            let (mut watcher, _connack) =
                Device::connect(case, &address, "will-watcher", true, None).await;
            watcher.subscribe(1, &[("will/#", 1)]).await;

            // A session whose socket goes without a DISCONNECT: its will is published.
            let crasher_will = Will {
                topic: "will/crasher",
                payload: "crasher is gone",
                qos: 1,
                retain: false,
            };
            let (crasher, _connack) =
                Device::connect(case, &address, "will-crasher", true, Some(&crasher_will)).await;
            drop(crasher);
            let fired = watcher.expect_delivery().await;
            assert_eq!(
                (fired.topic.as_str(), fired.payload.as_slice()),
                ("will/crasher", &b"crasher is gone"[..]),
                "case {case:?}: expected the will of the lost connection, got {fired:?}"
            );

            // A session whose client says goodbye: its will is suppressed. The sentinel is
            // published once the end of that connection is recorded, so a delivery read up
            // to it would have carried the will if one had been made.
            let polite_will = Will {
                topic: "will/polite",
                payload: "polite is gone",
                qos: 1,
                retain: false,
            };
            let (mut polite, _connack) =
                Device::connect(case, &address, "will-polite", true, Some(&polite_will)).await;
            polite.disconnect().await;
            await_disconnected(&router, case, &address, "will-polite").await;
            publish_over_http(&router, case, &address, "will/sentinel", 1, false, "sentinel").await;
            let after_goodbye = watcher.deliveries_until("will/sentinel").await;
            assert_eq!(
                after_goodbye,
                vec![Delivered {
                    topic: "will/sentinel".to_string(),
                    payload: b"sentinel".to_vec(),
                    retained: false,
                }],
                "case {case:?}: a clean DISCONNECT publishes no will, got {after_goodbye:?}"
            );

            // A will published with the RETAIN flag is the will topic's retained message,
            // exactly as any other retained publication is.
            let retained_will = Will {
                topic: "will/retained",
                payload: "retainer is gone",
                qos: 1,
                retain: true,
            };
            let (retainer, _connack) =
                Device::connect(case, &address, "will-retainer", true, Some(&retained_will)).await;
            drop(retainer);
            let retained_fired = watcher.expect_delivery().await;
            assert_eq!(
                retained_fired.topic, "will/retained",
                "case {case:?}: expected the retained will published, got {retained_fired:?}"
            );
            let (status, rendered, parsed) =
                retained_message(&router, &address, "will/retained").await;
            assert_eq!(
                status, 200,
                "case {case:?}: a retained will must be the topic's retained message, got \
                 {status} with {rendered}"
            );
            assert_eq!(
                parsed.get("topic").and_then(Value::as_str),
                Some("will/retained"),
                "case {case:?}: expected the will's own topic, got {rendered}"
            );
        });
    });
}

/// The broker publishes a connect and a disconnect event on the reserved presence topics of
/// the client id, and the disconnect event's reason separates the client's own disconnect
/// from a takeover of its client id and from a disconnect the service performed.
#[test]
fn presence_events_separate_a_clean_disconnect_from_a_client_id_takeover() {
    on_the_emulator("iot-mqtt-presence-events", |router, runtime| {
        runtime.block_on(async {
            let case = "the reserved presence topics";
            let address = discover(&router, case).await;

            let (mut watcher, _connack) =
                Device::connect(case, &address, "presence-watcher", true, None).await;
            let connected_filter = format!("{PRESENCE_PREFIX}/connected/#");
            let disconnected_filter = format!("{PRESENCE_PREFIX}/disconnected/#");
            let granted = watcher
                .subscribe(
                    1,
                    &[
                        (connected_filter.as_str(), 1),
                        (disconnected_filter.as_str(), 1),
                    ],
                )
                .await;
            assert_eq!(
                granted,
                vec![1, 1],
                "case {case:?}: expected both presence filters granted, got {granted:?}"
            );

            // A client that connects and then hangs up itself: two events, in that order,
            // and the disconnect names the client as the one that ended it.
            let (mut probe, _connack) =
                Device::connect(case, &address, "presence-probe", true, None).await;
            probe.disconnect().await;
            let connected = presence_event(&mut watcher, case).await;
            let disconnected = presence_event(&mut watcher, case).await;
            let probe_connected_topic = format!("{PRESENCE_PREFIX}/connected/presence-probe");
            let probe_disconnected_topic =
                format!("{PRESENCE_PREFIX}/disconnected/presence-probe");
            assert_eq!(
                (
                    connected.0.as_str(),
                    connected.1.get("eventType").and_then(Value::as_str),
                    connected.1.get("clientId").and_then(Value::as_str)
                ),
                (
                    probe_connected_topic.as_str(),
                    Some("connected"),
                    Some("presence-probe")
                ),
                "case {case:?}: expected the connect event of the probe, got {connected:?}"
            );
            assert!(
                connected
                    .1
                    .get("sessionIdentifier")
                    .and_then(Value::as_str)
                    .is_some_and(|identifier| !identifier.is_empty()),
                "case {case:?}: expected the session the event is about, got {:?}",
                connected.1
            );
            assert!(
                connected
                    .1
                    .get("timestamp")
                    .and_then(Value::as_i64)
                    .is_some_and(|at| at > 0),
                "case {case:?}: expected the instant the event was published, got {:?}",
                connected.1
            );
            assert_eq!(
                (
                    disconnected.0.as_str(),
                    disconnected.1.get("eventType").and_then(Value::as_str),
                    disconnected
                        .1
                        .get("disconnectReason")
                        .and_then(Value::as_str),
                    disconnected
                        .1
                        .get("clientInitiatedDisconnect")
                        .and_then(Value::as_bool)
                ),
                (
                    probe_disconnected_topic.as_str(),
                    Some("disconnected"),
                    Some(CLIENT_INITIATED_DISCONNECT),
                    Some(true)
                ),
                "case {case:?}: expected the client's own disconnect event, got {disconnected:?}"
            );

            // A client id another connection TAKES OVER: the displaced session's disconnect
            // event names the duplicate client id and says the client did not end it. The
            // three events are read as a set: two connections' own tasks publish them.
            let (_first, _connack) =
                Device::connect(case, &address, "presence-twin", true, None).await;
            let (_second, _connack) =
                Device::connect(case, &address, "presence-twin", true, None).await;
            let mut takeover = Vec::new();
            for _ in 0..3 {
                takeover.push(presence_event(&mut watcher, case).await);
            }
            let connects: Vec<&(String, Value)> = takeover
                .iter()
                .filter(|(topic, _event)| topic.contains("/connected/"))
                .collect();
            let disconnects: Vec<&(String, Value)> = takeover
                .iter()
                .filter(|(topic, _event)| topic.contains("/disconnected/"))
                .collect();
            assert_eq!(
                (connects.len(), disconnects.len()),
                (2, 1),
                "case {case:?}: expected two connects and one disconnect for a takeover, got \
                 {takeover:?}"
            );
            let identifiers: Vec<&str> = connects
                .iter()
                .map(|(_topic, event)| {
                    event
                        .get("sessionIdentifier")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                })
                .collect();
            assert_ne!(
                identifiers[0], identifiers[1],
                "case {case:?}: expected the two connections to be separate sessions, got \
                 {identifiers:?}"
            );
            let twin_disconnected_topic = format!("{PRESENCE_PREFIX}/disconnected/presence-twin");
            assert_eq!(
                (
                    disconnects[0].0.as_str(),
                    disconnects[0]
                        .1
                        .get("disconnectReason")
                        .and_then(Value::as_str),
                    disconnects[0]
                        .1
                        .get("clientInitiatedDisconnect")
                        .and_then(Value::as_bool)
                ),
                (
                    twin_disconnected_topic.as_str(),
                    Some(DUPLICATE_CLIENTID),
                    Some(false)
                ),
                "case {case:?}: expected the takeover's own disconnect event, got {:?}",
                disconnects[0]
            );

            // And a disconnect the SERVICE performed names itself, which is the third way a
            // session ends.
            let (status, rendered, _parsed) = call(
                &router,
                "DELETE",
                &format!(
                    "https://{address}/connections/presence-twin?cleanSession=false&preventWillMessage=false"
                ),
                "iotdata",
                "",
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeleteConnection must answer 200, got {status} with {rendered}"
            );
            let closed = presence_event(&mut watcher, case).await;
            assert_eq!(
                (
                    closed.1.get("disconnectReason").and_then(Value::as_str),
                    closed
                        .1
                        .get("clientInitiatedDisconnect")
                        .and_then(Value::as_bool)
                ),
                (Some(SERVER_INITIATED_DISCONNECT), Some(false)),
                "case {case:?}: expected the service's own disconnect event, got {closed:?}"
            );
        });
    });
}

/// One presence event the watcher was delivered: the topic it arrived on, and the event
/// document it carries.
async fn presence_event(watcher: &mut Device, case: &str) -> (String, Value) {
    let delivered = watcher.expect_delivery().await;
    let event: Value = serde_json::from_slice(&delivered.payload).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: a presence event must carry a JSON document, got {:?}: {error}",
            String::from_utf8_lossy(&delivered.payload)
        )
    });
    (delivered.topic, event)
}
