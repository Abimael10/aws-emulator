// SPDX-License-Identifier: CC0-1.0

//! `GetThingConnectivityData`: the live connectivity of one registered thing, end to end over
//! HTTP.
//!
//! The rule these cases assert: a thing's connectivity IS the MQTT session its client holds in
//! the shared message broker — the session a device's own connection to the published data
//! endpoint opened, and the one the IoT data plane's `GetConnection` reports on — read off that
//! session and never taken from the request that asked. Which session belongs to a thing is the
//! client that identifies it: the session the broker recorded the thing on, and otherwise the
//! one opened under the thing's default client id, which is the thing name
//! (`DescribeThing$defaultClientId`).
//!
//! What follows from it, and is asserted here:
//!
//! * a thing with a LIVE session is connected, and the session's own client id, clean-session
//!   flag, keep-alive interval, session expiry and connect instant are what is reported; a
//!   thing whose session is closed is not connected, and the disconnect instant and the reason
//!   the broker recorded are what is reported instead;
//! * a thing with NO session — a client that has never connected, and one whose disconnect the
//!   broker no longer reports — is answered "not connected" with no session details at all,
//!   rather than with a session that does not exist;
//! * the connectivity section of the thing index is what this read is served from, so an
//!   account whose index does not carry it, and one whose index is still being built, are
//!   refused with the modelled `IndexNotReadyException` (400) rather than answered from
//!   nothing;
//! * a name the registry does not hold is the thing family's `ResourceNotFoundException` (404);
//! * socket information is reported only to a request that asked for it under a configuration
//!   that enabled its retrieval for this API;
//! * the read is a report: a later caller sees exactly what the earlier one saw.
//!
//! Most cases put the session in the broker's table directly — which is what the broker's own
//! CONNECT handling does — and drive the operation over HTTP. The LAST case stands up the whole
//! path instead: a device discovers the account's data endpoint, opens a real MQTT connection
//! to it over a socket, and the read is asserted against that live connection and against what
//! the data plane reports for the same session.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::{Bytes, BytesMut};
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::DISCONNECT_REASON_VALUE_VALUES;
use service_com_amazonaws_iot_awsiotservice::state::{
    ComAmazonawsIotAwsiotserviceState, INDEX_BUILD_SPAN, INDEX_STATUS_BUILDING, THING_INDEX_NAME,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_common::iot_mqtt_broker::mqttbytes::{self, v4};
use service_common::iot_mqtt_broker::{
    broker_state, now_millis, MqttBrokerState, MqttSession,
    DISCONNECT_INFORMATION_RETENTION_MILLIS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Region every request below is signed for. Connectivity is account-wide, so no
/// assertion depends on it.
const REGION: &str = "us-east-2";

/// The address this emulator's API is published at, which the last case needs so the data
/// endpoint it discovers is an address on this host. The broker's own socket is a different
/// one — one port serves one wire — and the device reads which back rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:41791";

/// How long the last case waits for a CONDITION the broker's own task brings about (a packet
/// written, a session marked closed), and how long between two looks. Generous: neither bounds
/// the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`EXCHANGE_DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// A thing indexing configuration whose index carries connectivity status, as
/// `UpdateIndexingConfiguration` takes it.
fn connectivity_configuration(socket_apis: Option<&[&str]>) -> Value {
    let mut section = json!({
        "thingIndexingMode": "REGISTRY",
        "thingConnectivityIndexingMode": "STATUS",
    });
    if let Some(apis) = socket_apis {
        section["filter"] = json!({ "connectivity": { "includeSocketInformation": apis } });
    }
    json!({ "thingIndexingConfiguration": section })
}

/// One emulator: the shared engine, the IoT control plane and the IoT data plane mounted on
/// it — the shape `core/router/src/lib.rs` mounts them in, so the sessions one reports on are
/// the sessions the other reports on.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the front door's concern: the credential scope is what
        // routes a request in this harness.
        router.set_request_authentication(false);
        router.set_url_base(
            EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"),
        );
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iotdataplane_iotmoonrakerservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { engine, router }
    }

    /// This service's own store, as its handlers reach it.
    fn state(&self) -> engine::ServiceState<ComAmazonawsIotAwsiotserviceState> {
        self.engine.state::<ComAmazonawsIotAwsiotserviceState>()
    }

    /// The broker's session table, in the SAME shared engine: the sessions a connectivity
    /// read reports on belong to the broker a device connects to, not to this service.
    fn broker(&self) -> engine::ServiceState<MqttBrokerState> {
        broker_state(&*self.engine).expect("the engine vends the broker's store")
    }

    async fn dispatch(
        &self,
        method: &str,
        path: &str,
        signing_name: &str,
        body: Option<&Value>,
    ) -> Reply {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("{SERVED_AT}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `POST /things/{thingName}/connectivity-data` — the operation under test.
    async fn connectivity(&self, thing_name: &str, include_socket_information: bool) -> Reply {
        self.dispatch(
            "POST",
            &format!("/things/{thing_name}/connectivity-data"),
            "iot",
            Some(&json!({ "includeSocketInformation": include_socket_information })),
        )
        .await
    }

    /// The connectivity a successful read reports, as JSON.
    async fn reported(&self, case: &str, thing_name: &str, include_socket: bool) -> Value {
        let reply = self.connectivity(thing_name, include_socket).await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: expected GetThingConnectivityData to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()
    }

    /// Register `thing_name` through `CreateThing`, which is what puts a document in the
    /// index this read is served from.
    async fn create_thing(&self, thing_name: &str) {
        let reply = self
            .dispatch("POST", &format!("/things/{thing_name}"), "iot", Some(&json!({})))
            .await;
        assert_eq!(
            reply.status,
            200,
            "setup: CreateThing {thing_name} answered {} with {}",
            reply.status,
            reply.text()
        );
    }

    /// Record `configuration` as the account's indexing configuration.
    async fn update_indexing_configuration(&self, configuration: &Value) -> Reply {
        self.dispatch("POST", "/indexing/config", "iot", Some(configuration))
            .await
    }

    /// Turn the connectivity section of the thing index on and finish the build it owes, so
    /// the index is the ACTIVE one a reader finds — without the case waiting out
    /// [`INDEX_BUILD_SPAN`].
    async fn given_connectivity_index(&self, socket_apis: Option<&[&str]>) {
        let reply = self
            .update_indexing_configuration(&connectivity_configuration(socket_apis))
            .await;
        assert_eq!(
            reply.status,
            200,
            "setup: UpdateIndexingConfiguration answered {} with {}",
            reply.status,
            reply.text()
        );
        self.move_build(THING_INDEX_NAME, SystemTime::now() - INDEX_BUILD_SPAN);
    }

    /// Put the build of `index_name` at `started_at`, so the index reports the status a
    /// caller at that point in the build would find: a build whose due instant has passed is
    /// finished, and one due later is still running.
    fn move_build(&self, index_name: &str, started_at: SystemTime) {
        let state = self.state();
        let mut store = state.lock();
        let build = store
            .index_builds
            .get_mut(index_name)
            .unwrap_or_else(|| panic!("no build is recorded for {index_name}"));
        build.active_at = started_at + INDEX_BUILD_SPAN;
    }

    /// Open `session` in the broker's table, which is what the broker's CONNECT handling
    /// does.
    fn connect(&self, session: MqttSession) {
        self.broker()
            .lock()
            .sessions
            .insert(session.client_id.clone(), session);
    }

    /// The session the broker holds for `client_id`, as it stands now.
    fn stored_session(&self, client_id: &str) -> Option<MqttSession> {
        self.broker().lock().sessions.get(client_id).cloned()
    }

    /// `GET /connections/{clientId}` on the IoT DATA plane: what that service reports for the
    /// same session.
    async fn get_connection(&self, client_id: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/connections/{client_id}?includeSocketInformation=false"),
            "iotdata",
            None,
        )
        .await
    }

    /// `GET /endpoint?endpointType=iot:Data-ATS`: the address the control plane publishes for
    /// the account's data endpoint, and the call that opens the broker's socket.
    async fn discover(&self) -> String {
        let reply = self
            .dispatch(
                "GET",
                "/endpoint?endpointType=iot%3AData-ATS",
                "iot",
                None,
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "setup: DescribeEndpoint answered {} with {}",
            reply.status,
            reply.text()
        );
        reply.json()["endpointAddress"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: no endpointAddress in {}", reply.text()))
            .to_string()
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("the body is not JSON ({error}): {}", self.text()))
    }

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The message a restJson1 error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// The instant a reported `timestamp` names, in epoch MILLISECONDS: a restJson1 timestamp
/// travels as epoch seconds, fractions included.
fn reported_millis(case: &str, reported: &Value) -> i64 {
    let seconds = reported
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a timestamp number, got {reported}"));
    (seconds * 1_000.0).round() as i64
}

/// Which member of the output names socket information: the five the input's own
/// documentation lists ("socket information (sourcePort, targetPort, sourceIp, targetIp,
/// vpcEndpointId)",
/// `smithy_query(query="doc",shape="GetThingConnectivityDataRequest",member="includeSocketInformation")`).
const SOCKET_MEMBERS: &[&str] = &[
    "sourceIp",
    "sourcePort",
    "targetIp",
    "targetPort",
    "vpcEndpointId",
];

/// Every member a response carries only for a thing that HAS a session, so a thing without
/// one can be asserted to carry none of them.
const SESSION_MEMBERS: &[&str] = &[
    "timestamp",
    "disconnectReason",
    "clientId",
    "cleanSession",
    "keepAliveDuration",
    "sessionExpiry",
    "sourceIp",
    "sourcePort",
    "targetIp",
    "targetPort",
    "vpcEndpointId",
];

/// One row: a thing, the session its client holds, and whether that session is live.
struct Row {
    case: &'static str,
    thing_name: &'static str,
    session: MqttSession,
}

/// A session with socket information filled in, so a row that asks for it has something to
/// report.
fn session(client_id: &str, thing_name: Option<&str>) -> MqttSession {
    MqttSession {
        client_id: client_id.to_string(),
        thing_name: thing_name.map(str::to_string),
        source_ip: Some("198.51.100.17".to_string()),
        source_port: 51_823,
        target_ip: Some("203.0.113.9".to_string()),
        target_port: 8883,
        vpc_endpoint_id: Some("vpce-0a1b2c3d4e5f67890".to_string()),
        ..MqttSession::default()
    }
}

/// The sessions the rows below span: a live one and closed ones, under the thing's default
/// client id and under a client id the broker recorded the thing on, with clean and persistent
/// sessions, different keep-alive intervals and expiries, and disconnect reasons from the
/// modelled `DisconnectReasonValue` catalog — including one the issue did not cite.
fn rows(now: i64) -> Vec<Row> {
    vec![
        Row {
            case: "a live clean session under the thing's default client id",
            thing_name: "sensor-1",
            session: MqttSession {
                connected: true,
                clean_session: true,
                keep_alive_seconds: 60,
                session_expiry_seconds: 0,
                connected_since_millis: now - 5_000,
                ..session("sensor-1", None)
            },
        },
        Row {
            case: "a live persistent session the broker recorded the thing on",
            thing_name: "Rack:4_B",
            session: MqttSession {
                connected: true,
                clean_session: false,
                keep_alive_seconds: 1_200,
                session_expiry_seconds: 3_600,
                connected_since_millis: now - 120_000,
                ..session("rack-4-b-gateway", Some("Rack:4_B"))
            },
        },
        Row {
            case: "a session the client itself closed",
            thing_name: "device9",
            session: MqttSession {
                connected: false,
                clean_session: true,
                keep_alive_seconds: 30,
                session_expiry_seconds: 0,
                connected_since_millis: now - 90_000,
                disconnected_since_millis: now - 30_000,
                disconnect_reason: Some("CLIENT_INITIATED_DISCONNECT".to_string()),
                ..session("device9", None)
            },
        },
        Row {
            case: "a session the keep-alive timed out",
            thing_name: "press_7",
            session: MqttSession {
                connected: false,
                clean_session: false,
                keep_alive_seconds: 0,
                session_expiry_seconds: 7_200,
                connected_since_millis: now - 600_000,
                disconnected_since_millis: now - 1_000,
                disconnect_reason: Some("MQTT_KEEP_ALIVE_TIMEOUT".to_string()),
                ..session("press_7", None)
            },
        },
        Row {
            case: "a session a second client id took over",
            thing_name: "forklift-12",
            session: MqttSession {
                connected: false,
                clean_session: false,
                keep_alive_seconds: 300,
                session_expiry_seconds: 86_400,
                connected_since_millis: now - 10_000,
                disconnected_since_millis: now - 500,
                disconnect_reason: Some("DUPLICATE_CLIENTID".to_string()),
                ..session("forklift-12", None)
            },
        },
    ]
}

/// A thing's connectivity is the session its client holds: every member reported is that
/// session's own, for a live session and for a closed one alike.
#[tokio::test]
async fn a_things_connectivity_is_the_session_its_client_holds() {
    let emulator = Emulator::new();
    emulator.given_connectivity_index(None).await;

    let now = now_millis();
    for row in rows(now) {
        let case = row.case;
        emulator.create_thing(row.thing_name).await;
        emulator.connect(row.session.clone());

        let reported = emulator.reported(case, row.thing_name, false).await;
        let session = &row.session;

        assert_eq!(
            reported["thingName"].as_str(),
            Some(row.thing_name),
            "case {case:?}: expected the thing asked about, got {reported}"
        );
        assert_eq!(
            reported["connected"].as_bool(),
            Some(session.connected),
            "case {case:?}: expected connected == {}, got {reported}",
            session.connected
        );
        assert_eq!(
            reported["clientId"].as_str(),
            Some(session.client_id.as_str()),
            "case {case:?}: expected the session's client id, got {reported}"
        );
        assert_eq!(
            reported["cleanSession"].as_bool(),
            Some(session.clean_session),
            "case {case:?}: expected cleanSession == {}, got {reported}",
            session.clean_session
        );
        assert_eq!(
            reported["keepAliveDuration"].as_i64(),
            Some(i64::from(session.keep_alive_seconds)),
            "case {case:?}: expected the session's keep-alive interval, got {reported}"
        );
        assert_eq!(
            reported["sessionExpiry"].as_i64(),
            Some(session.session_expiry_seconds),
            "case {case:?}: expected the session's expiry interval, got {reported}"
        );

        // The instant reported is the connection event the session is IN: the connect for a
        // live session, the disconnect for a closed one.
        let expected_at = if session.connected {
            session.connected_since_millis
        } else {
            session.disconnected_since_millis
        };
        assert_eq!(
            reported_millis(case, &reported["timestamp"]),
            expected_at,
            "case {case:?}: expected the instant of the connection event, got {reported}"
        );

        if session.connected {
            assert!(
                reported.get("disconnectReason").is_none(),
                "case {case:?}: expected no disconnect reason for a connected client, got \
                 {reported}"
            );
        } else {
            let reason = session.disconnect_reason.as_deref().unwrap_or_default();
            assert!(
                DISCONNECT_REASON_VALUE_VALUES.contains(&reason),
                "case {case:?}: the row's reason {reason:?} must be a modelled one, of \
                 {DISCONNECT_REASON_VALUE_VALUES:?}"
            );
            assert_eq!(
                reported["disconnectReason"].as_str(),
                Some(reason),
                "case {case:?}: expected the recorded disconnect reason, got {reported}"
            );
        }

        // Socket information was not asked for, so none of its members is reported however
        // much of it the session holds.
        for member in SOCKET_MEMBERS {
            assert!(
                reported.get(*member).is_none(),
                "case {case:?}: expected no {member} without a request for socket \
                 information, got {reported}"
            );
        }
    }
}

/// A thing with no session at all is not connected and carries no session details: a client
/// that never connected, one whose disconnect the broker no longer reports, and a thing whose
/// name is a client id the broker recorded ANOTHER thing on.
#[tokio::test]
async fn a_thing_without_a_session_is_not_connected_and_carries_no_details() {
    let emulator = Emulator::new();
    emulator.given_connectivity_index(None).await;
    let now = now_millis();

    for thing_name in ["never-connected", "long-gone", "lent-out"] {
        emulator.create_thing(thing_name).await;
    }

    // A disconnect older than the window the broker reports one for.
    emulator.connect(MqttSession {
        connected: false,
        disconnected_since_millis: now - DISCONNECT_INFORMATION_RETENTION_MILLIS - 1_000,
        disconnect_reason: Some("CONNECTION_LOST".to_string()),
        ..session("long-gone", None)
    });
    // A session under the thing's name as its client id, but recorded on a DIFFERENT thing:
    // it is that thing's session, not this one's.
    emulator.connect(MqttSession {
        connected: true,
        connected_since_millis: now,
        ..session("lent-out", Some("another-thing"))
    });

    for (case, thing_name) in [
        ("a client that has never connected", "never-connected"),
        ("a disconnect past the reported window", "long-gone"),
        ("a session recorded on another thing", "lent-out"),
    ] {
        let reported = emulator.reported(case, thing_name, true).await;
        assert_eq!(
            reported["connected"].as_bool(),
            Some(false),
            "case {case:?}: expected connected == false, got {reported}"
        );
        assert_eq!(
            reported["thingName"].as_str(),
            Some(thing_name),
            "case {case:?}: expected the thing asked about, got {reported}"
        );
        for member in SESSION_MEMBERS {
            assert!(
                reported.get(*member).is_none(),
                "case {case:?}: expected no {member} for a thing with no session, got \
                 {reported}"
            );
        }
    }
}

/// A `thingName` the registry does not hold is the thing family's 404, whether or not the
/// broker holds a session under that name.
#[tokio::test]
async fn an_unregistered_thing_is_refused_as_a_missing_thing() {
    let emulator = Emulator::new();
    emulator.given_connectivity_index(None).await;
    let now = now_millis();

    emulator.connect(MqttSession {
        connected: true,
        connected_since_millis: now,
        ..session("unregistered-but-connected", None)
    });

    for (case, thing_name) in [
        ("a name nothing was ever created under", "no-such-thing"),
        (
            "a name a connected client holds",
            "unregistered-but-connected",
        ),
    ] {
        let reply = emulator.connectivity(thing_name, false).await;
        assert_eq!(
            reply.status,
            404,
            "case {case:?}: expected 404, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "ResourceNotFoundException",
            "case {case:?}: expected the thing family's not-found error, got {}",
            reply.text()
        );
        assert!(
            reply.error_message().contains(thing_name),
            "case {case:?}: expected the refusal to name the thing asked about, got {}",
            reply.error_message()
        );
    }
}

/// The connectivity section of the thing index is what this read is served from: an account
/// whose index does not carry it, and one whose index is still being built, are refused.
#[tokio::test]
async fn connectivity_data_needs_an_index_that_carries_it() {
    let emulator = Emulator::new();
    let thing_name = "gated-thing";
    emulator.create_thing(thing_name).await;
    emulator.connect(MqttSession {
        connected: true,
        connected_since_millis: now_millis(),
        ..session(thing_name, None)
    });

    // Configurations whose thing index carries NO connectivity status: indexing off
    // altogether, and a thing index that is on with connectivity indexing off or unstated.
    let unusable: &[(&str, Value)] = &[
        (
            "an account that has never enabled indexing",
            Value::Null,
        ),
        (
            "a thing index that is off",
            json!({ "thingIndexingConfiguration": { "thingIndexingMode": "OFF" } }),
        ),
        (
            "a thing index with connectivity indexing off",
            json!({ "thingIndexingConfiguration": {
                "thingIndexingMode": "REGISTRY",
                "thingConnectivityIndexingMode": "OFF",
            } }),
        ),
        (
            "a thing index that states no connectivity mode",
            json!({ "thingIndexingConfiguration": { "thingIndexingMode": "REGISTRY_AND_SHADOW" } }),
        ),
    ];
    for (case, configuration) in unusable {
        if !configuration.is_null() {
            let reply = emulator.update_indexing_configuration(configuration).await;
            assert_eq!(
                reply.status,
                200,
                "case {case:?}: the setup update answered {} with {}",
                reply.status,
                reply.text()
            );
        }
        let reply = emulator.connectivity(thing_name, false).await;
        assert_eq!(
            reply.status,
            400,
            "case {case:?}: expected 400, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "IndexNotReadyException",
            "case {case:?}: expected the index-not-ready refusal, got {}",
            reply.text()
        );
        assert!(
            reply.error_message().contains(THING_INDEX_NAME),
            "case {case:?}: expected the refusal to name the index, got {}",
            reply.error_message()
        );
    }

    // Connectivity indexing on, but the build the change owes has NOT finished: the index
    // carries the section and not yet its contents.
    emulator
        .update_indexing_configuration(&connectivity_configuration(None))
        .await;
    emulator.move_build(THING_INDEX_NAME, SystemTime::now());
    let reply = emulator.connectivity(thing_name, false).await;
    assert_eq!(
        reply.status,
        400,
        "case an index still being built: expected 400, got {} body {}",
        reply.status,
        reply.text()
    );
    assert_eq!(
        reply.error_code(),
        "IndexNotReadyException",
        "case an index still being built: expected the index-not-ready refusal, got {}",
        reply.text()
    );
    assert!(
        reply.error_message().contains(INDEX_STATUS_BUILDING),
        "case an index still being built: expected the refusal to name the status the index \
         is in, got {}",
        reply.error_message()
    );

    // The same account once that build is done: the live session is reported.
    emulator.move_build(THING_INDEX_NAME, SystemTime::now() - INDEX_BUILD_SPAN);
    let reported = emulator.reported("an index that is active", thing_name, false).await;
    assert_eq!(
        reported["connected"].as_bool(),
        Some(true),
        "case an index that is active: expected the live session to be reported, got {reported}"
    );
}

/// Socket information is reported only to a request that asked for it under a configuration
/// that enabled its retrieval for this API.
#[tokio::test]
async fn socket_information_needs_both_the_configuration_and_the_request() {
    let thing_name = "socket-thing";
    // The selections span: none at all, one naming another API, and one naming this API.
    let selections: &[(&str, Option<&[&str]>, bool)] = &[
        ("no connectivity filter at all", None, false),
        ("an empty selection", Some(&[]), false),
        (
            "a selection naming this api",
            Some(&["GET_THING_CONNECTIVITY_DATA"]),
            true,
        ),
    ];

    for (selection_case, socket_apis, selected) in selections {
        for asked in [false, true] {
            let case = format!("{selection_case}, request asked {asked}");
            let emulator = Emulator::new();
            emulator.given_connectivity_index(*socket_apis).await;
            emulator.create_thing(thing_name).await;
            let opened = MqttSession {
                connected: true,
                clean_session: true,
                keep_alive_seconds: 45,
                connected_since_millis: now_millis(),
                ..session(thing_name, None)
            };
            emulator.connect(opened.clone());

            let reported = emulator.reported(&case, thing_name, asked).await;
            let expected = *selected && asked;
            if expected {
                assert_eq!(
                    reported["sourceIp"].as_str(),
                    opened.source_ip.as_deref(),
                    "case {case:?}: expected the session's source address, got {reported}"
                );
                assert_eq!(
                    reported["sourcePort"].as_i64(),
                    Some(i64::from(opened.source_port)),
                    "case {case:?}: expected the session's source port, got {reported}"
                );
                assert_eq!(
                    reported["targetIp"].as_str(),
                    opened.target_ip.as_deref(),
                    "case {case:?}: expected the session's target address, got {reported}"
                );
                assert_eq!(
                    reported["targetPort"].as_i64(),
                    Some(i64::from(opened.target_port)),
                    "case {case:?}: expected the session's target port, got {reported}"
                );
                assert_eq!(
                    reported["vpcEndpointId"].as_str(),
                    opened.vpc_endpoint_id.as_deref(),
                    "case {case:?}: expected the session's VPC endpoint, got {reported}"
                );
            } else {
                for member in SOCKET_MEMBERS {
                    assert!(
                        reported.get(*member).is_none(),
                        "case {case:?}: expected no {member}, got {reported}"
                    );
                }
            }
            // Whatever the socket members do, the connection itself is reported.
            assert_eq!(
                reported["connected"].as_bool(),
                Some(true),
                "case {case:?}: expected the live session to be reported, got {reported}"
            );
        }
    }
}

/// The read is a REPORT: a later caller sees what the earlier one saw, and the session the
/// broker holds is untouched by having been read.
#[tokio::test]
async fn reading_connectivity_data_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    emulator
        .given_connectivity_index(Some(&["GET_THING_CONNECTIVITY_DATA"]))
        .await;
    let thing_name = "read-twice";
    emulator.create_thing(thing_name).await;
    let opened = MqttSession {
        connected: true,
        clean_session: false,
        keep_alive_seconds: 90,
        session_expiry_seconds: 1_800,
        connected_since_millis: now_millis() - 2_000,
        ..session(thing_name, None)
    };
    emulator.connect(opened.clone());

    let first = emulator.reported("the first read", thing_name, true).await;
    let second = emulator.reported("the second read", thing_name, true).await;
    assert_eq!(
        first, second,
        "case two reads: expected the same report, got {first} then {second}"
    );

    let held = emulator
        .stored_session(&opened.client_id)
        .expect("the broker still holds the session");
    assert!(
        held.connected == opened.connected
            && held.clean_session == opened.clean_session
            && held.keep_alive_seconds == opened.keep_alive_seconds
            && held.session_expiry_seconds == opened.session_expiry_seconds
            && held.connected_since_millis == opened.connected_since_millis
            && held.disconnect_reason.is_none(),
        "case two reads: expected the stored session untouched, got {held:?}"
    );
}

/// A DEVICE at the published data endpoint: the connection it opens over a real socket is what
/// the read reports, the data plane reports the same session, and the client's own DISCONNECT
/// is what the read then reports.
#[tokio::test]
async fn a_real_connection_at_the_published_endpoint_is_what_the_read_reports() {
    let emulator = Emulator::new();
    emulator.given_connectivity_index(None).await;
    let thing_name = "field-device-3";
    emulator.create_thing(thing_name).await;

    // Before the device connects, the registered thing is simply not connected.
    let before = emulator.reported("before the device connects", thing_name, false).await;
    assert_eq!(
        before["connected"].as_bool(),
        Some(false),
        "case before the device connects: expected connected == false, got {before}"
    );

    // Where this account's data plane answers, which is the address a device dials.
    let address = emulator.discover().await;
    let mut device = Device::connect(&address, thing_name).await;

    let connected = emulator.reported("a connected device", thing_name, false).await;
    assert_eq!(
        connected["connected"].as_bool(),
        Some(true),
        "case a connected device: expected connected == true, got {connected}"
    );
    assert_eq!(
        connected["clientId"].as_str(),
        Some(thing_name),
        "case a connected device: expected the client id the device connected under, got \
         {connected}"
    );
    assert_eq!(
        connected["cleanSession"].as_bool(),
        Some(true),
        "case a connected device: expected the clean session the CONNECT asked for, got \
         {connected}"
    );
    assert_eq!(
        connected["keepAliveDuration"].as_i64(),
        Some(i64::from(Device::KEEP_ALIVE)),
        "case a connected device: expected the keep-alive the CONNECT asked for, got \
         {connected}"
    );
    assert!(
        connected.get("disconnectReason").is_none(),
        "case a connected device: expected no disconnect reason, got {connected}"
    );

    // The two services report ONE session: what the data plane says of the client is what the
    // control plane says of its thing.
    let plane = emulator.get_connection(thing_name).await;
    assert_eq!(
        plane.status,
        200,
        "case a connected device: GetConnection answered {} with {}",
        plane.status,
        plane.text()
    );
    let plane = plane.json();
    assert_eq!(
        (
            plane["connected"].as_bool(),
            plane["clientId"].as_str(),
            plane["cleanSession"].as_bool(),
            plane["keepAliveDuration"].as_i64(),
        ),
        (
            connected["connected"].as_bool(),
            connected["clientId"].as_str(),
            connected["cleanSession"].as_bool(),
            connected["keepAliveDuration"].as_i64(),
        ),
        "case a connected device: expected the two services to report one session, got \
         {plane} and {connected}"
    );

    // The client's own DISCONNECT ends the session, and the read then reports that — waited
    // for as the CONDITION it is, since the broker's own task notices the close.
    device.disconnect().await;
    let closed = emulator
        .poll_until_closed("a device that disconnected", thing_name)
        .await;
    assert_eq!(
        closed["disconnectReason"].as_str(),
        Some("CLIENT_INITIATED_DISCONNECT"),
        "case a device that disconnected: expected the client's own reason, got {closed}"
    );
    assert!(
        closed["timestamp"].is_number(),
        "case a device that disconnected: expected the disconnect instant, got {closed}"
    );
}

impl Emulator {
    /// Read `thing_name`'s connectivity until it reports the client is no longer connected,
    /// or [`EXCHANGE_DEADLINE`] passes — the condition the broker's own connection task
    /// brings about once the client's DISCONNECT has been read.
    async fn poll_until_closed(&self, case: &str, thing_name: &str) -> Value {
        let deadline = SystemTime::now() + EXCHANGE_DEADLINE;
        let mut last = Value::Null;
        while SystemTime::now() < deadline {
            last = self.reported(case, thing_name, false).await;
            if last["connected"].as_bool() == Some(false) {
                return last;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        panic!("case {case:?}: the session was still reported connected: {last}");
    }
}

/// A DEVICE: a real connection to the published address, speaking MQTT 3.1.1 on it with the
/// codec the broker itself reads and writes the protocol with.
struct Device {
    client_id: String,
    socket: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// The keep-alive interval this device's CONNECT asks for, in seconds.
    const KEEP_ALIVE: u16 = 60;

    /// Dial the published address and send the CONNECT, answering with the connected device —
    /// which is what the CONNACK accepting the session says it is.
    async fn connect(address: &str, client_id: &str) -> Self {
        let port = loopback_port(address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let socket = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("dialling the published address {address} timed out"))
            .unwrap_or_else(|error| {
                panic!("the published address {address} must accept a connection: {error}")
            });
        let mut device = Self {
            client_id: client_id.to_string(),
            socket,
            buffer: BytesMut::new(),
        };
        let mut connect = v4::Connect::new(client_id.to_string());
        connect.clean_session = true;
        connect.keep_alive = Self::KEEP_ALIVE;
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

    /// Send the client's own DISCONNECT and close the socket behind it.
    async fn disconnect(&mut self) {
        self.write(|buffer| v4::Disconnect.write(buffer)).await;
        let _ = self.socket.shutdown().await;
    }

    /// Write one packet, rendered by the codec.
    async fn write<Render>(&mut self, render: Render)
    where
        Render: FnOnce(&mut BytesMut) -> Result<usize, mqttbytes::Error>,
    {
        let mut rendered = BytesMut::new();
        render(&mut rendered)
            .unwrap_or_else(|error| panic!("device {}: the packet renders: {error}", self.client_id));
        self.socket
            .write_all(&rendered)
            .await
            .unwrap_or_else(|error| {
                panic!("device {}: the session takes it: {error}", self.client_id)
            });
    }

    /// Read one packet the broker wrote, waiting for it until [`EXCHANGE_DEADLINE`].
    async fn expect_packet(&mut self) -> v4::Packet {
        let client_id = self.client_id.clone();
        let read = tokio::time::timeout(EXCHANGE_DEADLINE, async {
            loop {
                match v4::read(&mut self.buffer, 256 * 1024) {
                    Ok(packet) => return Some(packet),
                    Err(mqttbytes::Error::InsufficientBytes(_)) => {}
                    Err(error) => panic!("device {client_id}: the broker wrote no packet: {error}"),
                }
                let mut chunk = [0u8; 4096];
                match self.socket.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => panic!("device {client_id}: reading the session failed: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("device {client_id}: the broker wrote nothing in time"));
        read.unwrap_or_else(|| panic!("device {client_id}: the broker closed the connection"))
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
