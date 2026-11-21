// SPDX-License-Identifier: CC0-1.0

//! The Device Shadow service's own PUSH: the reserved MQTT topics a shadow's accepted
//! changes are announced on, and the delta it recomputes against what the device has
//! reported.
//!
//! The rule under test: every accepted change to a shadow is announced to that shadow's
//! subscribers on its own reserved topics — `…/update/accepted` and `…/update/documents`
//! for every accepted update, `…/update/delta` only while the merged desired state is not
//! matched by the merged reported state, and `…/delete/accepted` for a delete. The delta is
//! the difference of the WHOLE desired state against the WHOLE reported state at that
//! moment, not the keys the request happened to write: a leaf the device has reported drops
//! out of it, and a leaf that diverges again comes back alone.
//!
//! It needs the WHOLE router: the broker's address is published by the IoT control-plane
//! crate, the updates are made through the IoT data plane's `UpdateThingShadow`, and the
//! broker that carries the announcements is in neither.
//!
//! The rows differ along what the rule generalises over: the shadow addressed (the unnamed
//! "classic" shadow and a named one, whose reserved namespaces differ), the thing it
//! belongs to, and the leaves written (a scalar, a nested object, one of several keys).
//!
//! The session is spoken over a real TCP socket to the published address with the same
//! maintained MQTT codec a client library uses. The absence of a delta is asserted
//! STRUCTURALLY rather than by waiting: the next leg's own announcements are read off the
//! same connection, and a delta published where none is owed would arrive before them.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::BytesMut;
use serde_json::{json, Value};
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every request below is signed for.
const REGION: &str = "ap-south-1";

/// The address this emulator's API is served at, as the core publishes it from its data
/// port; the broker's own socket is a different address, read back from the control plane.
const SERVED_AT: &str = "https://127.0.0.1:39359";

/// How long a leg of the exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes), not the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// One shadow of one thing, as its announcements are read.
struct Row {
    case: &'static str,
    thing_name: &'static str,
    /// The shadow addressed: `None` for the thing's unnamed ("classic") shadow.
    shadow_name: Option<&'static str>,
    client_id: &'static str,
}

/// The rows: the two shadow namespaces, on two things.
const ROWS: &[Row] = &[
    Row {
        case: "the unnamed shadow",
        thing_name: "thermostat-1",
        shadow_name: None,
        client_id: "shadow-push-probe-classic",
    },
    Row {
        case: "a named shadow",
        thing_name: "valve-7",
        shadow_name: Some("firmware"),
        client_id: "shadow-push-probe-named",
    },
];

/// A SigV4 credential scope naming `signing_name` in [`REGION`].
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
    body: Vec<u8>,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/octet-stream")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    (status, rendered)
}

/// The address the control plane publishes for the account's ATS data endpoint — which is
/// also the call that OPENS the broker's socket.
pub(crate) async fn discover(router: &MultiServiceRouter) -> String {
    let (status, rendered) = call(
        router,
        "GET",
        &format!("{SERVED_AT}/endpoint?endpointType=iot%3AData-ATS"),
        "iot",
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

/// The shadow route of one shadow, with the `name` query parameter only when it is named.
fn shadow_uri(thing_name: &str, shadow_name: Option<&str>) -> String {
    match shadow_name {
        Some(name) => format!("{SERVED_AT}/things/{thing_name}/shadow?name={name}"),
        None => format!("{SERVED_AT}/things/{thing_name}/shadow"),
    }
}

/// `POST /things/{thingName}/shadow` with `state`, asserting the update was accepted.
pub(crate) async fn update_shadow(
    case: &str,
    router: &MultiServiceRouter,
    thing_name: &str,
    shadow_name: Option<&str>,
    state: &Value,
) {
    let document = json!({ "state": state });
    let (status, rendered) = call(
        router,
        "POST",
        &shadow_uri(thing_name, shadow_name),
        "iotdata",
        serde_json::to_vec(&document).expect("the document serializes"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update {document} must be accepted, got {status} with {rendered}"
    );
}

/// `DELETE /things/{thingName}/shadow`, asserting the delete was accepted.
async fn delete_shadow(router: &MultiServiceRouter, row: &Row) {
    let (status, rendered) = call(
        router,
        "DELETE",
        &shadow_uri(row.thing_name, row.shadow_name),
        "iotdata",
        Vec::new(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: the delete must be accepted, got {status} with {rendered}",
        row.case
    );
}

/// The reserved topic prefix of one shadow, written out here rather than read from the
/// service: `$aws/things/{thingName}/shadow` for the unnamed shadow and its
/// `name/{shadowName}` subtree for a named one.
pub(crate) fn reserved_prefix(thing_name: &str, shadow_name: Option<&str>) -> String {
    match shadow_name {
        Some(name) => format!("$aws/things/{thing_name}/shadow/name/{name}"),
        None => format!("$aws/things/{thing_name}/shadow"),
    }
}

/// The port a published address names, checked to be on the loopback interface.
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

/// A DEVICE: a real socket to the published address, speaking MQTT 3.1.1 on it.
pub(crate) struct Device {
    case: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial `address`, send the CONNECT and assert the session was opened.
    pub(crate) async fn connect(case: &'static str, address: &str, client_id: &str) -> Self {
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
        let mut device = Self {
            case,
            stream,
            buffer: BytesMut::new(),
        };
        let mut connect = v4::Connect::new(client_id);
        connect.clean_session = true;
        connect.keep_alive = 30;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.read_packet().await;
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

    /// Subscribe to `filters` at QoS 1, answering with the QoS granted for each.
    pub(crate) async fn subscribe(&mut self, filters: &[String]) -> Vec<v4::SubscribeReasonCode> {
        let mut subscribe = v4::Subscribe::new(filters[0].clone(), QoS::AtLeastOnce);
        subscribe.pkid = 1;
        for path in &filters[1..] {
            subscribe
                .filters
                .push(v4::SubscribeFilter::new(path.clone(), QoS::AtLeastOnce));
        }
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.read_packet().await;
        let v4::Packet::SubAck(suback) = answered else {
            panic!("case {:?}: expected a SUBACK, got {answered:?}", self.case);
        };
        suback.return_codes
    }

    /// Read the next `wanted` publications the broker pushed, each as its topic and its
    /// JSON body, acknowledging every one made at QoS 1.
    pub(crate) async fn read_announcements(&mut self, wanted: usize) -> Vec<(String, Value)> {
        let mut read = Vec::with_capacity(wanted);
        while read.len() < wanted {
            let packet = self.read_packet().await;
            let v4::Packet::Publish(publish) = packet else {
                panic!(
                    "case {:?}: expected an announcement, got {packet:?}",
                    self.case
                );
            };
            if publish.qos != QoS::AtMostOnce {
                let pkid = publish.pkid;
                self.write(|buffer| v4::PubAck::new(pkid).write(buffer))
                    .await;
            }
            let body: Value = serde_json::from_slice(&publish.payload).unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the announcement on {:?} must be a JSON document: {error}",
                    self.case, publish.topic
                )
            });
            read.push((publish.topic, body));
        }
        read
    }
}

/// The emulator for one case: the whole router on one shared engine.
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
pub(crate) fn on_the_emulator<Body>(name: &'static str, body: Body)
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

/// The member one announcement must carry, read with the case in the failure.
pub(crate) fn member<'a>(case: &str, topic: &str, body: &'a Value, path: &[&str]) -> &'a Value {
    let mut node = body;
    for name in path {
        node = node.get(name).unwrap_or_else(|| {
            panic!("case {case:?}: the announcement on {topic} must carry {path:?}, got {body}")
        });
    }
    node
}

/// A subscriber of a shadow's reserved topics is pushed every accepted change to it, and the
/// delta it is pushed holds what the device has not reported — recomputed each time.
#[test]
fn an_accepted_shadow_change_is_announced_on_the_shadows_reserved_topics() {
    on_the_emulator("iot-shadow-reserved-topic-push", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            for row in ROWS {
                let case = row.case;
                let prefix = reserved_prefix(row.thing_name, row.shadow_name);
                let accepted_topic = format!("{prefix}/update/accepted");
                let documents_topic = format!("{prefix}/update/documents");
                let delta_topic = format!("{prefix}/update/delta");
                let deleted_topic = format!("{prefix}/delete/accepted");

                let mut device = Device::connect(case, &address, row.client_id).await;
                let filters = vec![
                    delta_topic.clone(),
                    accepted_topic.clone(),
                    documents_topic.clone(),
                    deleted_topic.clone(),
                ];
                let granted = device.subscribe(&filters).await;
                assert_eq!(
                    granted,
                    vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
                    "case {case:?}: expected QoS 1 granted for every reserved filter, got \
                     {granted:?}"
                );

                // Leg 1 — a desired-only write: the update is announced, and the whole
                // desired state is outstanding because nothing has been reported.
                let desired = json!({"alpha": 1, "beta": {"inner": 2}});
                update_shadow(
                    case,
                    &router,
                    row.thing_name,
                    row.shadow_name,
                    &json!({"desired": desired.clone()}),
                )
                .await;
                let announced = device.read_announcements(3).await;
                let topics: Vec<&str> = announced
                    .iter()
                    .map(|(topic, _body)| topic.as_str())
                    .collect();
                assert_eq!(
                    topics,
                    vec![
                        accepted_topic.as_str(),
                        documents_topic.as_str(),
                        delta_topic.as_str()
                    ],
                    "case {case:?}: expected the accepted update, the documents and the delta, \
                     got {topics:?}"
                );
                let (_topic, accepted) = &announced[0];
                assert_eq!(
                    member(case, &accepted_topic, accepted, &["state", "desired"]),
                    &desired,
                    "case {case:?}: expected the written desired state, got {accepted}"
                );
                assert_eq!(
                    member(case, &accepted_topic, accepted, &["version"]),
                    &json!(1),
                    "case {case:?}: expected the first version, got {accepted}"
                );
                let (_topic, documents) = &announced[1];
                assert_eq!(
                    member(case, &documents_topic, documents, &["previous"]),
                    &Value::Null,
                    "case {case:?}: a created shadow has no previous document, got {documents}"
                );
                assert_eq!(
                    member(
                        case,
                        &documents_topic,
                        documents,
                        &["current", "state", "desired"]
                    ),
                    &desired,
                    "case {case:?}: expected the merged desired state, got {documents}"
                );
                let (_topic, delta) = &announced[2];
                assert_eq!(
                    member(case, &delta_topic, delta, &["state"]),
                    &desired,
                    "case {case:?}: expected every differing leaf in the delta, got {delta}"
                );
                assert_eq!(
                    member(case, &delta_topic, delta, &["version"]),
                    &json!(1),
                    "case {case:?}: expected the delta at the accepted version, got {delta}"
                );
                // The delta's metadata names the delta's own leaves, each with the instant
                // it was written at.
                assert!(
                    member(case, &delta_topic, delta, &["metadata", "alpha", "timestamp"])
                        .as_i64()
                        .is_some_and(|timestamp| timestamp > 1_700_000_000),
                    "case {case:?}: expected an epoch-second timestamp per delta leaf, got \
                     {delta}"
                );

                // Leg 2 — a CONVERGING report: the update is announced, and NOTHING is
                // outstanding, so no delta is pushed. The next leg's own announcements are
                // read off this same connection, so a delta published here would arrive
                // ahead of them and fail the ordering assertion below.
                update_shadow(
                    case,
                    &router,
                    row.thing_name,
                    row.shadow_name,
                    &json!({"reported": desired.clone()}),
                )
                .await;
                let converged = device.read_announcements(2).await;
                let topics: Vec<&str> = converged
                    .iter()
                    .map(|(topic, _body)| topic.as_str())
                    .collect();
                assert_eq!(
                    topics,
                    vec![accepted_topic.as_str(), documents_topic.as_str()],
                    "case {case:?}: a converging report must announce the update and push no \
                     delta, got {topics:?}"
                );
                let (_topic, documents) = &converged[1];
                assert_eq!(
                    member(
                        case,
                        &documents_topic,
                        documents,
                        &["current", "state", "reported"]
                    ),
                    &desired,
                    "case {case:?}: expected the reported state in the current document, got \
                     {documents}"
                );
                assert!(
                    member(case, &documents_topic, documents, &["previous", "state"])
                        .get("reported")
                        .is_none(),
                    "case {case:?}: the document before the report had no reported state, got \
                     {documents}"
                );

                // Leg 3 — one leaf diverges again: the delta holds that leaf ALONE, because
                // it is recomputed against the whole reported state rather than accumulated.
                update_shadow(
                    case,
                    &router,
                    row.thing_name,
                    row.shadow_name,
                    &json!({"desired": {"beta": {"inner": 9}}}),
                )
                .await;
                let rediverged = device.read_announcements(3).await;
                let topics: Vec<&str> = rediverged
                    .iter()
                    .map(|(topic, _body)| topic.as_str())
                    .collect();
                assert_eq!(
                    topics,
                    vec![
                        accepted_topic.as_str(),
                        documents_topic.as_str(),
                        delta_topic.as_str()
                    ],
                    "case {case:?}: expected the update and a recomputed delta, got {topics:?}"
                );
                let (_topic, delta) = &rediverged[2];
                assert_eq!(
                    member(case, &delta_topic, delta, &["state"]),
                    &json!({"beta": {"inner": 9}}),
                    "case {case:?}: expected only what still differs in the delta, got {delta}"
                );
                assert_eq!(
                    member(case, &delta_topic, delta, &["version"]),
                    &json!(3),
                    "case {case:?}: expected the version to advance with every update, got \
                     {delta}"
                );

                // Leg 4 — the delete: announced on the shadow's own delete topic, naming
                // the version the document stood at.
                delete_shadow(&router, row).await;
                let deleted = device.read_announcements(1).await;
                assert_eq!(
                    deleted[0].0, deleted_topic,
                    "case {case:?}: expected the delete announced on {deleted_topic}, got {:?}",
                    deleted[0].0
                );
                assert_eq!(
                    member(case, &deleted_topic, &deleted[0].1, &["version"]),
                    &json!(3),
                    "case {case:?}: expected the deleted document's version, got {:?}",
                    deleted[0].1
                );
            }
        });
    });
}
