// SPDX-License-Identifier: CC0-1.0

//! The MQTT broker a device DISCOVERS and then connects to, across the two IoT services:
//! the control plane publishes the account's data endpoint (`DescribeEndpoint`), a device
//! opens a real MQTT connection to that address, and the data plane's connection
//! operations (`GetConnection`, `ListSubscriptions`, `SendDirectMessage`,
//! `DeleteConnection`) report on and act on THAT session.
//!
//! The rule under test: the address published for a data endpoint is one this emulator
//! accepts an MQTT connection on, and the session that connection opens is the session the
//! connection APIs read and write. A device that does what the AWS documentation
//! prescribes — discover the endpoint, connect to it, subscribe — is a connected client,
//! and nothing of what the connection APIs then report is taken from the request that
//! asked.
//!
//! It needs the WHOLE router: discovery is served by the IoT control-plane crate, the
//! connection operations by the IoT data-plane crate, and the broker that holds the
//! session is in neither — so a single-service suite cannot see the three agree.
//!
//! The rows differ along what the rule generalises over: which endpoint type was
//! discovered, the client id, whether the session is clean or persistent, the keep-alive
//! interval, how many topic filters are subscribed and of which form (exact, `+`, `#`),
//! and the QoS each asks for — including the QoS 2 AWS IoT Core does not support, which is
//! granted at the maximum it does.
//!
//! The connection is spoken over a real TCP socket to the published address, with the
//! packets laid out by the same maintained MQTT codec a client library uses, so nothing of
//! the protocol is short-circuited. Assembling the router costs stack (see
//! `boot_serve.rs`), so it is built — and every leg driven — on an explicitly sized
//! thread.

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
const SERVED_AT: &str = "https://127.0.0.1:39355";

/// How long a leg of the exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes, a socket it closes), not the speed of
/// the host — a loaded machine takes longer and still passes.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// The lifecycle reason a session carries once `DeleteConnection` closed it, and the one it
/// carries after the client's own DISCONNECT. Both are AWS IoT Core lifecycle-event
/// `disconnectReason` values.
const SERVER_INITIATED_DISCONNECT: &str = "SERVER_INITIATED_DISCONNECT";
/// See [`SERVER_INITIATED_DISCONNECT`].
const CLIENT_INITIATED_DISCONNECT: &str = "CLIENT_INITIATED_DISCONNECT";

/// One device's connection: what it discovers, how it connects, and what it subscribes to.
struct Row {
    case: &'static str,
    /// The `endpointType` the device discovered its broker with, or none at all.
    endpoint_type: Option<&'static str>,
    client_id: &'static str,
    clean_session: bool,
    /// The keep-alive interval the CONNECT asks for, in seconds.
    keep_alive: u16,
    /// The topic filters the device subscribes to, each with the QoS it asks for.
    subscriptions: &'static [(&'static str, u8)],
    /// The QoS the broker grants each of them, in the same order.
    granted: &'static [i32],
    /// A topic covered by one of those filters, which a publication is made on — and
    /// `None` for a device that subscribed to nothing.
    published_topic: Option<&'static str>,
}

/// The rows: each a different endpoint type, client id, session kind, keep-alive,
/// subscription set and QoS, so no single answer satisfies two of them.
const ROWS: &[Row] = &[
    Row {
        case: "a clean session on the ATS data endpoint, one exact filter at QoS 0",
        endpoint_type: Some("iot:Data-ATS"),
        client_id: "broker-probe-ats",
        clean_session: true,
        keep_alive: 30,
        subscriptions: &[("devices/broker-probe-ats/telemetry", 0)],
        granted: &[0],
        published_topic: Some("devices/broker-probe-ats/telemetry"),
    },
    Row {
        case: "a persistent session on the plain data endpoint, a single-level wildcard at \
               QoS 1",
        endpoint_type: Some("iot:Data"),
        client_id: "broker-probe-data",
        clean_session: false,
        keep_alive: 60,
        subscriptions: &[("devices/+/commands", 1)],
        granted: &[1],
        published_topic: Some("devices/sensor-9/commands"),
    },
    Row {
        case: "a device that discovered the endpoint with no type, two filters, one asking \
               for the QoS 2 the broker does not serve",
        endpoint_type: None,
        client_id: "broker-probe-default",
        clean_session: true,
        keep_alive: 15,
        subscriptions: &[("fleet/#", 2), ("fleet/alpha/state", 0)],
        granted: &[1, 0],
        published_topic: Some("fleet/alpha/raw/temperature"),
    },
    Row {
        case: "a connected device that subscribed to nothing",
        endpoint_type: Some("iot:Data-ATS"),
        client_id: "broker-probe-silent-device-with-a-longer-client-id",
        clean_session: false,
        keep_alive: 1200,
        subscriptions: &[],
        granted: &[],
        published_topic: None,
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

/// `GET /endpoint[?endpointType=…]`, answering with the address the control plane
/// publishes.
async fn discover(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_type: Option<&str>,
) -> String {
    let query = match endpoint_type {
        Some(value) => format!("?endpointType={}", value.replace(':', "%3A")),
        None => String::new(),
    };
    let (status, rendered, parsed) = call(
        router,
        "GET",
        &format!("https://127.0.0.1:39355/endpoint{query}"),
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

/// The QoS a number names, as a client's own library reads it.
fn qos_of(number: u8) -> QoS {
    match number {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

/// The QoS number a granted subscription carries.
fn qos_number(qos: QoS) -> i32 {
    match qos {
        QoS::AtMostOnce => 0,
        QoS::AtLeastOnce => 1,
        QoS::ExactlyOnce => 2,
    }
}

/// The port a published address names, checked to be on the LOOPBACK interface this
/// emulator's listeners are bound on — which is the host every connection below dials, and
/// the only host a client of this emulator reaches it at.
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

/// A DEVICE: a real socket to the published address, speaking MQTT on it.
struct Device {
    case: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial `address` and send the CONNECT, answering with the device and the CONNACK the
    /// broker wrote.
    async fn connect(
        case: &'static str,
        address: &str,
        client_id: &str,
        clean_session: bool,
        keep_alive: u16,
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
            case,
            stream,
            buffer: BytesMut::new(),
        };
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = clean_session;
        connect.keep_alive = keep_alive;
        device
            .write(|buffer| connect.write(buffer))
            .await;
        let answered = device.read_packet().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!("case {case:?}: expected a CONNACK, got {answered:?}");
        };
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
    async fn read_packet(&mut self) -> v4::Packet {
        let case = self.case;
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

    /// Subscribe to `filters`, answering with the SUBACK.
    async fn subscribe(&mut self, filters: &[(&str, u8)]) -> v4::SubAck {
        let mut subscribe = v4::Subscribe::new(filters[0].0, qos_of(filters[0].1));
        subscribe.pkid = 1;
        for (path, qos) in &filters[1..] {
            subscribe
                .filters
                .push(v4::SubscribeFilter::new((*path).to_string(), qos_of(*qos)));
        }
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_packet().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("case {:?}: expected a SUBACK, got {answered:?}", self.case);
        };
        suback
    }

    /// Wait for the socket to be CLOSED by the broker, which is what a disconnect the
    /// service performed does to the client's connection.
    async fn wait_for_close(&mut self) {
        let case = self.case;
        let closed = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            let mut chunk = [0u8; 256];
            loop {
                match self.stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(_more) => {}
                }
            }
        })
        .await;
        assert!(
            closed.is_ok(),
            "case {case:?}: the broker must close the connection it disconnected"
        );
    }
}

/// The emulator for one case: the whole router on one shared engine, served at
/// [`SERVED_AT`].
fn emulator() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"),
    );
    router
}

/// Run `body` on a thread with the stack the router's assembly needs, inside a runtime
/// with the IO the broker's own socket is bound on.
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

/// A device that discovers its endpoint and connects to it IS a connected client: the
/// connection operations report that live session, a publication reaches its subscription,
/// and `DeleteConnection` closes its connection.
#[test]
fn a_discovered_endpoint_accepts_an_mqtt_connection_the_connection_apis_report() {
    on_the_emulator("iot-mqtt-broker-sessions", |router, runtime| {
        runtime.block_on(async {
            for row in ROWS {
                let case = row.case;
                let address = discover(&router, case, row.endpoint_type).await;

                // The address is one a client of this emulator can dial, and not the API
                // endpoint: one port serves one wire.
                assert!(
                    !address.contains("amazonaws.com"),
                    "case {case:?}: the published address must be the emulator's own, got \
                     {address:?}"
                );
                assert_ne!(
                    address, "127.0.0.1:39355",
                    "case {case:?}: a data endpoint must name the broker's own socket"
                );

                let (mut device, connack) = Device::connect(
                    case,
                    &address,
                    row.client_id,
                    row.clean_session,
                    row.keep_alive,
                )
                .await;
                assert_eq!(
                    connack.code,
                    v4::ConnectReturnCode::Success,
                    "case {case:?}: the broker must accept the CONNECT, got {:?}",
                    connack.code
                );
                assert!(
                    !connack.session_present,
                    "case {case:?}: a client id that never connected has no session to resume"
                );

                if !row.subscriptions.is_empty() {
                    let suback = device.subscribe(row.subscriptions).await;
                    let granted: Vec<i32> = suback
                        .return_codes
                        .iter()
                        .map(|code| match code {
                            v4::SubscribeReasonCode::Success(qos) => qos_number(*qos),
                            other => panic!("case {case:?}: a filter was refused: {other:?}"),
                        })
                        .collect();
                    assert_eq!(
                        granted, row.granted,
                        "case {case:?}: expected the granted QoS {:?}, got {granted:?}",
                        row.granted
                    );
                }

                // What the data plane reports about that client is read from the session
                // the connection opened.
                let (status, rendered, parsed) = call(
                    &router,
                    "GET",
                    &format!(
                        "https://{address}/connections/{}?includeSocketInformation=true",
                        row.client_id
                    ),
                    "iotdata",
                    "",
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: GetConnection must report the live session, got {status} \
                     with {rendered}"
                );
                assert_eq!(
                    parsed.get("connected").and_then(Value::as_bool),
                    Some(true),
                    "case {case:?}: the connected client must be reported connected, got \
                     {rendered}"
                );
                assert_eq!(
                    parsed.get("clientId").and_then(Value::as_str),
                    Some(row.client_id),
                    "case {case:?}: expected the client id the CONNECT carried, got {rendered}"
                );
                assert_eq!(
                    parsed.get("cleanSession").and_then(Value::as_bool),
                    Some(row.clean_session),
                    "case {case:?}: expected the session kind the CONNECT asked for, got \
                     {rendered}"
                );
                assert_eq!(
                    parsed.get("keepAliveDuration").and_then(Value::as_i64),
                    Some(i64::from(row.keep_alive)),
                    "case {case:?}: expected the keep-alive the CONNECT asked for, got \
                     {rendered}"
                );
                assert_eq!(
                    parsed.get("targetPort").and_then(Value::as_i64),
                    address
                        .rsplit_once(':')
                        .and_then(|(_host, port)| port.parse::<i64>().ok()),
                    "case {case:?}: expected the port the device reached the broker at, got \
                     {rendered}"
                );

                // The subscriptions the SUBSCRIBE recorded are the ones listed.
                let (status, rendered, parsed) = call(
                    &router,
                    "GET",
                    &format!(
                        "https://{address}/connections/{}/subscriptions",
                        row.client_id
                    ),
                    "iotdata",
                    "",
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: ListSubscriptions must answer 200, got {status} with \
                     {rendered}"
                );
                let listed: Vec<(String, i64)> = parsed
                    .get("subscriptions")
                    .and_then(Value::as_array)
                    .unwrap_or_else(|| panic!("case {case:?}: no subscriptions in {rendered}"))
                    .iter()
                    .map(|summary| {
                        (
                            summary
                                .get("topicFilter")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            summary.get("qos").and_then(Value::as_i64).unwrap_or(-1),
                        )
                    })
                    .collect();
                let mut expected: Vec<(String, i64)> = row
                    .subscriptions
                    .iter()
                    .zip(row.granted)
                    .map(|((filter, _asked), granted)| {
                        ((*filter).to_string(), i64::from(*granted))
                    })
                    .collect();
                // The listing walks the filters in stable filter order.
                expected.sort();
                assert_eq!(
                    listed, expected,
                    "case {case:?}: expected the subscribed filters {expected:?}, got {listed:?}"
                );

                if let Some(topic) = row.published_topic {
                    // A publication on a topic the session subscribes to reaches the
                    // device over its own connection.
                    let (status, rendered, _parsed) = call(
                        &router,
                        "POST",
                        &format!(
                            "https://{address}/topics/{}?qos=0",
                            form_encode(topic)
                        ),
                        "iotdata",
                        "lamp-on",
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: publishing on {topic} must answer 200, got {status} \
                         with {rendered}"
                    );
                    let delivered = device.read_packet().await;
                    let v4::Packet::Publish(publish) = delivered else {
                        panic!("case {case:?}: expected the published message, got {delivered:?}");
                    };
                    assert_eq!(
                        publish.topic, topic,
                        "case {case:?}: expected the published topic, got {:?}",
                        publish.topic
                    );
                    assert_eq!(
                        publish.payload.as_ref(),
                        b"lamp-on",
                        "case {case:?}: expected the published bytes, got {:?}",
                        publish.payload
                    );

                    // And so does a message sent to the client id itself.
                    let (status, rendered, _parsed) = call(
                        &router,
                        "POST",
                        &format!(
                            "https://{address}/connections/{}/messages?topic={}&confirmation=false&timeout=5",
                            row.client_id,
                            form_encode(topic)
                        ),
                        "iotdata",
                        "direct",
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: SendDirectMessage must answer 200, got {status} with \
                         {rendered}"
                    );
                    let delivered = device.read_packet().await;
                    let v4::Packet::Publish(publish) = delivered else {
                        panic!("case {case:?}: expected the direct message, got {delivered:?}");
                    };
                    assert_eq!(
                        publish.payload.as_ref(),
                        b"direct",
                        "case {case:?}: expected the sent bytes, got {:?}",
                        publish.payload
                    );
                }

                // `DeleteConnection` closes the client's connection: the record says the
                // SERVICE closed it, and the socket itself is closed.
                let (status, rendered, _parsed) = call(
                    &router,
                    "DELETE",
                    &format!(
                        "https://{address}/connections/{}?cleanSession=false&preventWillMessage=false",
                        row.client_id
                    ),
                    "iotdata",
                    "",
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DeleteConnection must answer 200, got {status} with \
                     {rendered}"
                );
                device.wait_for_close().await;

                let (status, rendered, parsed) = call(
                    &router,
                    "GET",
                    &format!(
                        "https://{address}/connections/{}?includeSocketInformation=false",
                        row.client_id
                    ),
                    "iotdata",
                    "",
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the disconnected client is still reported, got {status} \
                     with {rendered}"
                );
                assert_eq!(
                    parsed.get("connected").and_then(Value::as_bool),
                    Some(false),
                    "case {case:?}: expected the client reported disconnected, got {rendered}"
                );
                assert_eq!(
                    parsed.get("disconnectReason").and_then(Value::as_str),
                    Some(SERVER_INITIATED_DISCONNECT),
                    "case {case:?}: expected the service's own disconnect reason, got {rendered}"
                );
            }
        });
    });
}

/// A client's OWN disconnect is recorded as its own, and what it leaves behind is what the
/// session kind says: a persistent session keeps its subscriptions for the client's return
/// and resumes them, a clean one keeps none.
#[test]
fn a_clients_own_disconnect_leaves_the_session_its_kind_prescribes() {
    on_the_emulator("iot-mqtt-broker-disconnect", |router, runtime| {
        runtime.block_on(async {
            // The rows span the two session kinds: the filter a persistent session resumes,
            // and the clean session that keeps nothing.
            let rows: &[(&'static str, bool, &'static str)] = &[
                (
                    "a persistent session",
                    false,
                    "devices/persistent-probe/commands",
                ),
                ("a clean session", true, "devices/clean-probe/commands"),
            ];
            let address = discover(&router, "the broker's address", Some("iot:Data-ATS")).await;
            for (case, clean_session, filter) in rows {
                let client_id = if *clean_session {
                    "clean-probe"
                } else {
                    "persistent-probe"
                };
                let (mut device, _connack) =
                    Device::connect(case, &address, client_id, *clean_session, 45).await;
                device.subscribe(&[(filter, 1)]).await;
                device
                    .write(|buffer| v4::Disconnect.write(buffer))
                    .await;

                // The record is reported for as long as a disconnected client's
                // information is, and says the CLIENT hung up. It is polled rather than
                // assumed: the broker writes the record when the connection's own task
                // reaches the end of the socket.
                let deadline = tokio::time::Instant::now() + EXCHANGE_DEADLINE;
                let reported = loop {
                    let (status, rendered, parsed) = call(
                        &router,
                        "GET",
                        &format!("https://{address}/connections/{client_id}?includeSocketInformation=false"),
                        "iotdata",
                        "",
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: the session must still be reported, got {status} with \
                         {rendered}"
                    );
                    if parsed.get("connected").and_then(Value::as_bool) == Some(false) {
                        break parsed;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "case {case:?}: the client's own disconnect was never recorded"
                    );
                    tokio::task::yield_now().await;
                };
                assert_eq!(
                    reported.get("disconnectReason").and_then(Value::as_str),
                    Some(CLIENT_INITIATED_DISCONNECT),
                    "case {case:?}: expected the client's own disconnect reason, got {reported}"
                );

                // What the session kept: a persistent session's filters are still listed
                // and are resumed by the client's next connection; a clean session's are
                // gone.
                let (_status, rendered, parsed) = call(
                    &router,
                    "GET",
                    &format!("https://{address}/connections/{client_id}/subscriptions"),
                    "iotdata",
                    "",
                )
                .await;
                let listed = parsed
                    .get("subscriptions")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_else(|| panic!("case {case:?}: no subscriptions in {rendered}"));
                let expected = usize::from(!*clean_session);
                assert_eq!(
                    listed, expected,
                    "case {case:?}: expected {expected} kept subscription(s), got {listed} in \
                     {rendered}"
                );

                // The client returns: a persistent session is resumed, which is what
                // `sessionPresent` tells it.
                let (_again, connack) =
                    Device::connect(case, &address, client_id, false, 45).await;
                assert_eq!(
                    connack.session_present,
                    !*clean_session,
                    "case {case:?}: expected sessionPresent {}, got {}",
                    !*clean_session,
                    connack.session_present
                );
            }
        });
    });
}

/// The published address serves the IoT HTTPS data plane as well as MQTT — one address,
/// both wires, as the host AWS publishes for an account's data endpoint does.
#[test]
fn the_published_endpoint_serves_the_https_api_beside_mqtt() {
    on_the_emulator("iot-mqtt-broker-https", |router, runtime| {
        runtime.block_on(async {
            let case = "the HTTPS half of the published address";
            let address = discover(&router, case, Some("iot:Data-ATS")).await;
            // A request made on the published address itself, over its own socket, rather
            // than through the in-process router: what it proves is that the socket the
            // device connects to also answers the API a client builds from that address.
            let port = loopback_port(case, &address);
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap_or_else(|error| panic!("case {case:?}: the address accepts it: {error}"));
            let body = r#"{"state":{"desired":{"lamp":"on"}}}"#;
            let request = format!(
                "POST /things/broker-http-probe/shadow HTTP/1.1\r\nHost: {address}\r\n\
                 Authorization: {}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                authorization("iotdata"),
                body.len()
            );
            stream
                .write_all(request.as_bytes())
                .await
                .unwrap_or_else(|error| panic!("case {case:?}: the socket takes it: {error}"));
            let mut answered = Vec::new();
            tokio::time::timeout(EXCHANGE_DEADLINE, stream.read_to_end(&mut answered))
                .await
                .unwrap_or_else(|_| panic!("case {case:?}: the address answered nothing in time"))
                .unwrap_or_else(|error| panic!("case {case:?}: reading the answer: {error}"));
            let answer = String::from_utf8_lossy(&answered).to_string();
            assert!(
                answer.starts_with("HTTP/1.1 200"),
                "case {case:?}: the published address must serve the data plane, got {answer:?}"
            );
            assert!(
                answer.contains("\"lamp\""),
                "case {case:?}: expected the updated shadow in the answer, got {answer:?}"
            );

            // And the shadow really was written, which is what makes the answer the data
            // plane's own and not the socket's.
            let (status, rendered, parsed) = call(
                &router,
                "GET",
                &format!("https://{address}/things/broker-http-probe/shadow"),
                "iotdata",
                "",
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the shadow written over the socket must read back, got \
                 {status} with {rendered}"
            );
            assert_eq!(
                parsed.pointer("/state/desired/lamp").and_then(Value::as_str),
                Some("on"),
                "case {case:?}: expected the shadow written over the published address, got \
                 {rendered}"
            );
        });
    });
}

/// A `SendDirectMessage` that asks for delivery confirmation is answered once the CLIENT
/// has acknowledged the delivery — and the acknowledgement is the client's own PUBACK.
#[test]
fn a_delivery_confirmation_is_the_clients_own_acknowledgement() {
    on_the_emulator("iot-mqtt-broker-confirmation", |router, runtime| {
        runtime.block_on(async {
            let case = "a client that acknowledges its delivery";
            let address = discover(&router, case, Some("iot:Data-ATS")).await;
            let (mut device, _connack) =
                Device::connect(case, &address, "confirming-probe", true, 30).await;

            let url = format!(
                "https://{address}/connections/confirming-probe/messages?topic={}&confirmation=true&timeout=15",
                form_encode("devices/confirming-probe/commands")
            );
            // The call and the client run together: the API answers only once the device
            // has acknowledged, so the device has to be reading while the call is in
            // flight — which is exactly the exchange a real client is in.
            let sending = call(&router, "POST", &url, "iotdata", "reboot");
            let acknowledging = async {
                let delivered = device.read_packet().await;
                let v4::Packet::Publish(publish) = delivered else {
                    panic!("case {case:?}: expected the delivery, got {delivered:?}");
                };
                assert_eq!(
                    publish.qos,
                    QoS::AtLeastOnce,
                    "case {case:?}: a delivery to be confirmed is made at QoS 1, got {:?}",
                    publish.qos
                );
                device
                    .write(|buffer| v4::PubAck::new(publish.pkid).write(buffer))
                    .await;
            };
            let ((status, rendered, parsed), ()) = tokio::join!(sending, acknowledging);
            assert_eq!(
                status, 200,
                "case {case:?}: an acknowledged delivery must answer 200, got {status} with \
                 {rendered}"
            );
            assert!(
                parsed.get("traceId").and_then(Value::as_str).is_some(),
                "case {case:?}: expected the data plane's envelope, got {rendered}"
            );
        });
    });
}
