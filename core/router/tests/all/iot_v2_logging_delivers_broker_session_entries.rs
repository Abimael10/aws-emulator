// SPDX-License-Identifier: CC0-1.0

//! What V2 LOGGING delivers once an account has configured it: the IoT message broker's own
//! session events, as CloudWatch Logs entries an operator reads.
//!
//! The rule this suite pins, one level above the recorded case:
//!
//! > `SetV2LoggingOptions` "sets the logging options for the V2 logging service" — the role IoT
//! > writes as, the `defaultLogLevel` and `disableAllLogs` — and from the moment it is accepted
//! > every event the message broker serves for a session (the CONNECT that opens it, each topic
//! > filter a SUBSCRIBE asks for, each publication it takes, each message it writes back, the
//! > close that ends it) is DELIVERED as one entry into the service-owned CloudWatch Logs group
//! > `AWSIotLogsV2`, naming the client id of the session, the certificate this account
//! > identifies that session's device by, the topic the event is about and the outcome. Which
//! > events are delivered is what the configured level admits: a served event is high-level flow
//! > information (`INFO`) and a refused one is an error (`ERROR`), so `DEBUG` and `INFO` carry a
//! > session's whole flow, `ERROR` and `WARN` carry its refusals alone and `DISABLED` carries
//! > nothing — and `disableAllLogs` suppresses every entry whatever level is configured, so the
//! > same options with that switch set deliver nothing and with it clear deliver again.
//!
//! Only a cross-service suite can pin this: the logging options and the broker belong to the
//! IoT control plane, and whether a log group holds an entry at all is Amazon CloudWatch Logs'.
//! Every assertion reads the outcome back through CloudWatch Logs' own reader
//! (`FilterLogEvents`), so an entry that never reached that store cannot pass, and every session
//! is a REAL MQTT connection to the address `DescribeEndpoint` published, spoken with the codec
//! the broker itself reads the protocol with — nothing of the wire is short-circuited.
//!
//! The rows differ along what the rule generalises over: the level configured (every value
//! `LogLevel` declares), whether all logs are disabled, the event each entry is about (connect,
//! subscribe, publish in, publish out, disconnect) and how the account identifies the session's
//! device (a certificate attached to the thing the client id names, a certificate the account's
//! own policy authorizes for that client id, and a client id no record identifies a device for).
//! The recorded case is one row of it: `defaultLogLevel=DEBUG` with `disableAllLogs=false`, then
//! the same configuration with the flag set.
//!
//! A negative leg never waits on a clock: after a suppressed session the suite configures
//! logging again and runs a SENTINEL session, and the entries of that sentinel arriving is the
//! condition that says every earlier delivery has been made — so "nothing was delivered" is
//! asserted at a point where something certainly would have been. One leg does read the TIME an
//! entry was ingested at, which is a fact about the entry and not about the host: an entry
//! belongs to the instant of the event it reports.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::{Bytes, BytesMut};
use serde_json::{json, Value};
// The MQTT codec the broker itself reads and writes the protocol with, reached through the
// service whose operations report on the sessions it holds: the suite speaks the wire a client
// library speaks rather than laying packets out by hand.
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, QoS,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Region every call below is signed for; the account's logging options are account-wide,
/// so no assertion depends on it.
const REGION: &str = "eu-west-1";

/// The address this emulator's API is served at. The broker's own socket is a DIFFERENT
/// address — one port serves one wire — and which one that is, is the core's to choose, so the
/// device reads it back rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:39421";

/// The service-owned CloudWatch Logs group AWS IoT delivers V2 log entries into.
const V2_LOG_GROUP: &str = "AWSIotLogsV2";

/// How long a leg waits for a CONDITION (a packet the broker writes, an entry a delivery puts
/// in the group), and how long between two looks. Generous: neither bounds the speed of the
/// host, so a loaded machine takes longer and still passes.
const DEADLINE: Duration = Duration::from_secs(60);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The `eventType` of each broker event an entry is about, as AWS IoT names them.
const CONNECT_EVENT: &str = "Connect";
/// See [`CONNECT_EVENT`].
const DISCONNECT_EVENT: &str = "Disconnect";
/// See [`CONNECT_EVENT`].
const SUBSCRIBE_EVENT: &str = "Subscribe";
/// See [`CONNECT_EVENT`].
const PUBLISH_IN_EVENT: &str = "Publish-In";
/// See [`CONNECT_EVENT`].
const PUBLISH_OUT_EVENT: &str = "Publish-Out";

/// What one call answered.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The two services this rule spans, over ONE shared engine: the IoT control plane that holds
/// the logging options and publishes the broker's address, and the CloudWatch Logs the entries
/// are delivered into.
struct Emulator {
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
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iot_awsiotservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire, routed by the signing name its
    /// credential scope carries.
    async fn dispatch(
        &self,
        method: &str,
        path: &str,
        signing_name: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> Reply {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut builder = http::Request::builder()
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
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(builder.body(body).expect("the request is well formed"))
            .await;
        let (parts, body) = response.into_parts();
        Reply {
            status: parts.status.as_u16(),
            body,
        }
    }

    /// `POST /v2LoggingOptions`: the call that configures V2 logging.
    async fn set_v2_logging_options(&self, case: &str, options: &Value) {
        let reply = self
            .dispatch(
                "POST",
                "/v2LoggingOptions",
                "iot",
                &[],
                Bytes::from(options.to_string()),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: SetV2LoggingOptions({options}) must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
    }

    /// `GET /v2LoggingOptions`: what the account's configured options read back as.
    async fn get_v2_logging_options(&self, case: &str) -> Value {
        let reply = self
            .dispatch("GET", "/v2LoggingOptions", "iot", &[], Bytes::new())
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: GetV2LoggingOptions must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
        reply.json()
    }

    /// `POST /keys-and-certificate?setAsActive=true`: the X.509 identity AWS IoT issues for a
    /// device, as its `(certificateId, certificateArn)`.
    async fn create_keys_and_certificate(&self, case: &str) -> (String, String) {
        let reply = self
            .dispatch(
                "POST",
                "/keys-and-certificate?setAsActive=true",
                "iot",
                &[],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: CreateKeysAndCertificate must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
        let issued = reply.json();
        let member = |name: &str| {
            issued
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no {name} in {issued}"))
                .to_string()
        };
        (member("certificateId"), member("certificateArn"))
    }

    /// `POST /things/{thingName}`: the device's record in the registry, whose name is the
    /// client id its device connects under.
    async fn create_thing(&self, case: &str, thing_name: &str) {
        let reply = self
            .dispatch(
                "POST",
                &format!("/things/{thing_name}"),
                "iot",
                &[],
                Bytes::from(json!({}).to_string()),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: CreateThing {thing_name} must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
    }

    /// `PUT /things/{thingName}/principals`: the certificate the thing is the account's device
    /// by.
    async fn attach_thing_principal(&self, case: &str, thing_name: &str, principal: &str) {
        let reply = self
            .dispatch(
                "PUT",
                &format!("/things/{thing_name}/principals"),
                "iot",
                &[("x-amzn-principal", principal)],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: AttachThingPrincipal {principal} must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
    }

    /// `POST /policies/{policyName}` then `PUT /target-policies/{policyName}`: the policy that
    /// lets `principal` connect under `client_id`, which is what makes a certificate the one
    /// this account authorizes for that connection.
    async fn given_connect_policy(
        &self,
        case: &str,
        policy_name: &str,
        principal: &str,
        client_id: &str,
    ) {
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": ["iot:Connect"],
                "Resource": [format!(
                    "arn:aws:iot:{REGION}:{account}:client/{client_id}",
                    account = engine::emulator_account_id()
                )],
            }],
        });
        let created = self
            .dispatch(
                "POST",
                &format!("/policies/{policy_name}"),
                "iot",
                &[],
                Bytes::from(json!({ "policyDocument": document.to_string() }).to_string()),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: CreatePolicy {policy_name} must answer 200, got {} with {}",
            created.status,
            created.text()
        );
        let attached = self
            .dispatch(
                "PUT",
                &format!("/target-policies/{policy_name}"),
                "iot",
                &[],
                Bytes::from(json!({ "target": principal }).to_string()),
            )
            .await;
        assert_eq!(
            attached.status,
            200,
            "case {case:?}: AttachPolicy {policy_name} must answer 200, got {} with {}",
            attached.status,
            attached.text()
        );
    }

    /// `GET /endpoint?endpointType=iot:Data-ATS`: the address a device opens its MQTT
    /// connection to, which this call creates.
    async fn data_endpoint(&self) -> String {
        let reply = self
            .dispatch(
                "GET",
                "/endpoint?endpointType=iot%3AData-ATS",
                "iot",
                &[],
                Bytes::new(),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "setup: DescribeEndpoint must answer 200, got {} with {}",
            reply.status,
            reply.text()
        );
        reply
            .json()
            .get("endpointAddress")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("setup: no endpointAddress in {}", reply.text()))
            .to_string()
    }

    /// One Amazon CloudWatch Logs call over its own `awsJson1_1` wire.
    async fn logs(&self, operation: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            "/",
            "logs",
            &[
                ("content-type", "application/x-amz-json-1.1"),
                ("x-amz-target", &format!("Logs_20140328.{operation}")),
            ],
            Bytes::from(body.to_string()),
        )
        .await
    }

    /// Every entry the V2 logging group holds, as the JSON document each event's message is —
    /// and an empty list for a group CloudWatch Logs does not hold at all, which is what
    /// "nothing has been delivered" looks like to a client.
    async fn delivered_entries(&self) -> Vec<Value> {
        let reply = self
            .logs("FilterLogEvents", &json!({ "logGroupName": V2_LOG_GROUP }))
            .await;
        if reply.status != 200 {
            return Vec::new();
        }
        reply
            .json()
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.get("message").and_then(Value::as_str))
                    .map(|message| serde_json::from_str(message).unwrap_or(Value::Null))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The entries the group holds about the session of `client_id`.
    async fn entries_of(&self, client_id: &str) -> Vec<Value> {
        self.delivered_entries()
            .await
            .into_iter()
            .filter(|entry| entry.get("clientId").and_then(Value::as_str) == Some(client_id))
            .collect()
    }

    /// The entries the group holds that CloudWatch Logs ingested at or after `start_millis`, as
    /// a reader narrowing the group by time reads them (`FilterLogEvents$startTime`, "the start
    /// of the time range").
    async fn entries_since(&self, start_millis: i64) -> Vec<Value> {
        let reply = self
            .logs(
                "FilterLogEvents",
                &json!({ "logGroupName": V2_LOG_GROUP, "startTime": start_millis }),
            )
            .await;
        if reply.status != 200 {
            return Vec::new();
        }
        reply
            .json()
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| event.get("message").and_then(Value::as_str))
                    .map(|message| serde_json::from_str(message).unwrap_or(Value::Null))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Wait until the group holds an entry about `client_id` whose `eventType` is `event_type`,
    /// answering with it — the condition a delivery brings about, looked for again until
    /// [`DEADLINE`] passes.
    async fn await_entry(&self, case: &str, client_id: &str, event_type: &str) -> Value {
        let deadline = SystemTime::now() + DEADLINE;
        let mut seen = Vec::new();
        while SystemTime::now() < deadline {
            seen = self.entries_of(client_id).await;
            if let Some(entry) = seen
                .iter()
                .find(|entry| entry.get("eventType").and_then(Value::as_str) == Some(event_type))
            {
                return entry.clone();
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        panic!(
            "case {case:?}: no {event_type} entry for {client_id} was ever delivered to \
             {V2_LOG_GROUP}; the group holds {seen:?}"
        );
    }
}

/// The account's options as `SetV2LoggingOptions` takes them: the logging role, the default
/// level and whether all logs are disabled.
fn options(level: &str, disable_all_logs: bool) -> Value {
    json!({
        "roleArn": format!(
            "arn:aws:iam::{account}:role/iot-v2-logging",
            account = engine::emulator_account_id()
        ),
        "defaultLogLevel": level,
        "disableAllLogs": disable_all_logs,
    })
}

/// A DEVICE: a real connection to the published address, speaking MQTT 3.1.1 on it with the
/// codec the broker itself reads and writes the protocol with.
struct Device {
    client_id: String,
    socket: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial the published address and send the CONNECT, answering with the connected device.
    async fn connect(address: &str, client_id: &str) -> Self {
        let port = loopback_port(address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let socket = tokio::time::timeout(DEADLINE, dialling)
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

    /// Subscribe to `filter` at QoS 1 and read the SUBACK that grants it.
    async fn subscribe(&mut self, filter: &str) {
        let mut subscribe = v4::Subscribe::new(filter, QoS::AtLeastOnce);
        subscribe.pkid = 1;
        self.write(|buffer| subscribe.write(buffer)).await;
        let answered = self.expect_packet().await;
        assert!(
            matches!(answered, v4::Packet::SubAck(_)),
            "device {}: expected a SUBACK, got {answered:?}",
            self.client_id
        );
    }

    /// Publish `payload` on `topic` at QoS 1 and read the PUBACK that says the broker took it.
    async fn publish(&mut self, topic: &str, payload: &str) {
        let mut publish = v4::Publish::new(topic, QoS::AtLeastOnce, payload.as_bytes().to_vec());
        publish.pkid = 2;
        self.write(|buffer| publish.write(buffer)).await;
        // The broker answers its own delivery to this session (it subscribes to the topic) and
        // the PUBACK of the publication, in either order; both are read, so the exchange is
        // complete before the case looks at the group.
        for _ in 0..2 {
            let answered = self.expect_packet().await;
            assert!(
                matches!(answered, v4::Packet::PubAck(_) | v4::Packet::Publish(_)),
                "device {}: expected the PUBACK and the delivery, got {answered:?}",
                self.client_id
            );
        }
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
        render(&mut rendered).unwrap_or_else(|error| {
            panic!("device {}: the packet renders: {error}", self.client_id)
        });
        self.socket
            .write_all(&rendered)
            .await
            .unwrap_or_else(|error| {
                panic!("device {}: the session takes it: {error}", self.client_id)
            });
    }

    /// Read one packet the broker wrote, waiting for it until [`DEADLINE`].
    async fn expect_packet(&mut self) -> v4::Packet {
        let client_id = self.client_id.clone();
        let read = tokio::time::timeout(DEADLINE, async {
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
/// listeners are bound on.
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

/// Run one whole session of `client_id`: connect, subscribe to its own topic, publish on it and
/// hang up — the five events a session of this broker serves.
async fn one_session(address: &str, client_id: &str, topic: &str) {
    let mut device = Device::connect(address, client_id).await;
    device.subscribe(topic).await;
    device.publish(topic, "{\"temperature\":21}").await;
    device.disconnect().await;
}

/// RULE: with V2 logging configured and enabled, every event the broker serves for a session is
/// delivered into the service-owned group, naming the session's client id, the topic the event
/// is about and the outcome.
#[tokio::test]
async fn a_configured_account_gets_its_brokers_session_events_in_the_service_owned_group() {
    let emulator = Emulator::new();
    let case = "defaultLogLevel=DEBUG, disableAllLogs=false";
    emulator
        .set_v2_logging_options(case, &options("DEBUG", false))
        .await;

    // The configuration reads back as it was set, which is what a client confirms before it
    // expects anything in the group.
    let configured = emulator.get_v2_logging_options(case).await;
    assert_eq!(
        (
            configured.get("roleArn").and_then(Value::as_str),
            configured.get("disableAllLogs").and_then(Value::as_bool),
        ),
        (
            options("DEBUG", false)
                .get("roleArn")
                .and_then(Value::as_str),
            Some(false),
        ),
        "case {case:?}: expected the options to read back as set, got {configured}"
    );

    let address = emulator.data_endpoint().await;
    let client_id = "v2-logging-device";
    let topic = "v2logging/devices/sensor-1/telemetry";
    // The device's IDENTITY: a certificate this account issued, attached to the thing whose
    // name is the client id the device connects under — which is what makes that certificate
    // the principal of the session.
    let (certificate_id, certificate_arn) = emulator.create_keys_and_certificate(case).await;
    emulator.create_thing(case, client_id).await;
    emulator
        .attach_thing_principal(case, client_id, &certificate_arn)
        .await;
    one_session(&address, client_id, topic).await;

    // Every event of that one session, each entry found in the group by the reader a client
    // uses. The disconnect is the LAST of them, so waiting for it is also what says the session
    // is over.
    let expected: &[(&str, Option<&str>)] = &[
        (CONNECT_EVENT, None),
        (SUBSCRIBE_EVENT, Some(topic)),
        (PUBLISH_IN_EVENT, Some(topic)),
        (PUBLISH_OUT_EVENT, Some(topic)),
        (DISCONNECT_EVENT, None),
    ];
    for (event_type, about) in expected {
        let entry = emulator.await_entry(case, client_id, event_type).await;
        assert_eq!(
            entry.get("clientId").and_then(Value::as_str),
            Some(client_id),
            "case {case:?} / {event_type}: expected the session's client id, got {entry}"
        );
        assert_eq!(
            entry.get("topicName").and_then(Value::as_str),
            *about,
            "case {case:?} / {event_type}: expected the topic {about:?}, got {entry}"
        );
        assert_eq!(
            (
                entry.get("protocol").and_then(Value::as_str),
                entry.get("status").and_then(Value::as_str),
                entry.get("logLevel").and_then(Value::as_str),
                entry.get("accountId").and_then(Value::as_str),
            ),
            (
                Some("MQTT"),
                Some("Success"),
                Some("INFO"),
                Some(engine::emulator_account_id()),
            ),
            "case {case:?} / {event_type}: expected an entry of a served MQTT event of this \
             account, got {entry}"
        );
        assert_eq!(
            entry.get("principalId").and_then(Value::as_str),
            Some(certificate_id.as_str()),
            "case {case:?} / {event_type}: expected the certificate the session is this \
             account's device by, got {entry}"
        );
        assert!(
            entry
                .get("traceId")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty()),
            "case {case:?} / {event_type}: expected a trace id, got {entry}"
        );
        assert!(
            entry
                .get("timestamp")
                .and_then(Value::as_str)
                .is_some_and(|at| at.len() >= "0000-00-00 00:00:00.000".len()),
            "case {case:?} / {event_type}: expected the instant it happened, got {entry}"
        );
    }

    // The payload a device published is NOT in the group: an entry names the topic, which is
    // what the published entry format carries.
    let entries = emulator.entries_of(client_id).await;
    for entry in &entries {
        assert!(
            !entry.to_string().contains("temperature"),
            "case {case:?}: expected no published payload in an entry, got {entry}"
        );
    }

    // A device that comes BACK is a second session of the same client id, and every event of it
    // is delivered too: an operator watching one device reads every one of its sessions, not
    // just the first.
    one_session(&address, client_id, topic).await;
    let deadline = SystemTime::now() + DEADLINE;
    let mut connects = 0;
    while SystemTime::now() < deadline {
        connects = emulator
            .entries_of(client_id)
            .await
            .iter()
            .filter(|entry| entry.get("eventType").and_then(Value::as_str) == Some(CONNECT_EVENT))
            .count();
        if connects >= 2 {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert!(
        connects >= 2,
        "case \"a device that reconnects\": expected a connect entry per session, got {connects}"
    );

    // An entry belongs to the INSTANT OF ITS EVENT and not to the instant its delivery was
    // made, so a reader narrowing the group to the time after the last session ended reads none
    // of that session's entries — however long the delivery itself took. The close is the last
    // event of a session, so waiting for its entry is what says every entry of this one has
    // been delivered.
    emulator
        .await_entry("the time an entry belongs to", client_id, DISCONNECT_EVENT)
        .await;
    let after_the_session = epoch_millis(SystemTime::now()) + 1;
    let later = emulator.entries_since(after_the_session).await;
    let of_this_device: Vec<&Value> = later
        .iter()
        .filter(|entry| entry.get("clientId").and_then(Value::as_str) == Some(client_id))
        .collect();
    assert!(
        of_this_device.is_empty(),
        "case \"the time an entry belongs to\": expected every entry of the session to be \
         ingested at the instant of its own event, got {of_this_device:?}"
    );
}

/// `at` in milliseconds since the Unix epoch, which is the instant a log event carries.
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// RULE: the principal an entry names is the certificate this account's own records say the
/// session's device is — so a device with no thing at all is named by the certificate the
/// account's policies authorize to connect under its client id, and a client id no record
/// identifies a device for is named by no principal at all.
#[tokio::test]
async fn an_entry_names_the_certificate_this_account_identifies_the_session_by() {
    let emulator = Emulator::new();
    let case = "a device authorized by a policy on its certificate";
    emulator
        .set_v2_logging_options(case, &options("DEBUG", false))
        .await;
    let address = emulator.data_endpoint().await;

    // No thing and no attachment: the certificate is the device's identity because the
    // account's own policy lets it connect under this client id.
    let client_id = "policy-authorized-device";
    let (certificate_id, certificate_arn) = emulator.create_keys_and_certificate(case).await;
    emulator
        .given_connect_policy(case, "v2-logging-connect", &certificate_arn, client_id)
        .await;
    one_session(&address, client_id, "v2logging/policy/telemetry").await;
    let entry = emulator.await_entry(case, client_id, CONNECT_EVENT).await;
    assert_eq!(
        entry.get("principalId").and_then(Value::as_str),
        Some(certificate_id.as_str()),
        "case {case:?}: expected the certificate the account authorizes for this client id, \
         got {entry}"
    );

    // A client id no record of this account identifies a device for: the entries are delivered
    // and name no principal, rather than one nothing presented.
    let unidentified = "unidentified-device";
    one_session(&address, unidentified, "v2logging/unidentified/telemetry").await;
    let entry = emulator
        .await_entry("a client id no record identifies", unidentified, CONNECT_EVENT)
        .await;
    assert!(
        entry.get("principalId").is_none(),
        "case \"a client id no record identifies\": expected no principal, got {entry}"
    );
}

/// The events a session has certainly been REPORTED for by the time its device has read the
/// PUBACK of its publication: the broker serves one connection on one task, in packet order, so
/// the connect, the subscription and the publication it took are all reported before it
/// acknowledges that publication. The two that follow it — the delivery written back to the
/// subscriber and the close — are reported after, which is why a table that compares one
/// configuration against the next asserts over these three and the case above waits for all
/// five.
const ORDERED_BEFORE_THE_PUBACK: &[&str] = &[CONNECT_EVENT, SUBSCRIBE_EVENT, PUBLISH_IN_EVENT];

/// One row of the level table: the configuration, and whether a session's SERVED events are
/// delivered under it.
struct LevelRow {
    case: &'static str,
    level: &'static str,
    disable_all_logs: bool,
    client_id: &'static str,
    delivered: bool,
}

/// RULE: a session's served events are delivered exactly when the configured level admits an
/// event of their severity — `INFO` and `DEBUG` do, `WARN`, `ERROR` and `DISABLED` do not — and
/// `disableAllLogs` suppresses them whatever the level says, while clearing it makes delivery
/// resume.
#[tokio::test]
async fn the_configured_level_and_the_disable_flag_decide_what_is_delivered() {
    let emulator = Emulator::new();
    let address = emulator.data_endpoint().await;

    // The rows span every value `LogLevel` declares, and the flag that overrides all of them.
    let rows: &[LevelRow] = &[
        LevelRow {
            case: "DEBUG",
            level: "DEBUG",
            disable_all_logs: false,
            client_id: "level-debug",
            delivered: true,
        },
        LevelRow {
            case: "INFO",
            level: "INFO",
            disable_all_logs: false,
            client_id: "level-info",
            delivered: true,
        },
        LevelRow {
            case: "WARN",
            level: "WARN",
            disable_all_logs: false,
            client_id: "level-warn",
            delivered: false,
        },
        LevelRow {
            case: "ERROR",
            level: "ERROR",
            disable_all_logs: false,
            client_id: "level-error",
            delivered: false,
        },
        LevelRow {
            case: "DISABLED",
            level: "DISABLED",
            disable_all_logs: false,
            client_id: "level-disabled",
            delivered: false,
        },
        LevelRow {
            case: "DEBUG with every log disabled",
            level: "DEBUG",
            disable_all_logs: true,
            client_id: "level-all-disabled",
            delivered: false,
        },
    ];
    for row in rows {
        emulator
            .set_v2_logging_options(row.case, &options(row.level, row.disable_all_logs))
            .await;
        one_session(
            &address,
            row.client_id,
            &format!("v2logging/{}/telemetry", row.client_id),
        )
        .await;
    }

    // The SENTINEL: logging is configured again and one more session runs, so the arrival of
    // its entries is the condition that says every delivery owed to the rows above has been
    // made — a negative row is then asserted at a point where something certainly would have
    // been delivered, with no wall clock involved.
    let sentinel = "level-sentinel";
    emulator
        .set_v2_logging_options("the sentinel", &options("DEBUG", false))
        .await;
    one_session(&address, sentinel, "v2logging/sentinel/telemetry").await;
    emulator
        .await_entry("the sentinel", sentinel, PUBLISH_IN_EVENT)
        .await;

    for row in rows {
        let entries = emulator.entries_of(row.client_id).await;
        let delivered: Vec<&str> = entries
            .iter()
            .filter_map(|entry| entry.get("eventType").and_then(Value::as_str))
            .filter(|event_type| ORDERED_BEFORE_THE_PUBACK.contains(event_type))
            .collect();
        if row.delivered {
            for event_type in ORDERED_BEFORE_THE_PUBACK {
                assert!(
                    delivered.contains(event_type),
                    "case {:?}: expected a {event_type} entry, got {delivered:?}",
                    row.case
                );
            }
        } else {
            assert!(
                delivered.is_empty(),
                "case {:?}: expected no entry delivered, got {delivered:?}",
                row.case
            );
        }
    }
}

/// RULE: `disableAllLogs` is what decides whether an account that states a level delivers at
/// all: the SAME options with the switch set suspend every entry, and with it clear the same
/// session's events reach the group again.
#[tokio::test]
async fn the_kill_switch_suspends_delivery_and_clearing_it_resumes_it() {
    let emulator = Emulator::new();
    let case = "the account is configured at DEBUG, then the switch is toggled";
    emulator
        .set_v2_logging_options(case, &options("DEBUG", false))
        .await;
    let address = emulator.data_endpoint().await;

    // With the switch SET — the same options in every other member — a fresh session's events
    // are not delivered.
    emulator
        .set_v2_logging_options("the switch set", &options("DEBUG", true))
        .await;
    let suspended = "switch-suspended-device";
    one_session(&address, suspended, "v2logging/suspended/telemetry").await;

    // With the switch CLEARED again, the next session's events reach the group. Those entries
    // arriving is the condition that says every delivery owed to the suspended session has been
    // made, so the negative assertion below waits on no clock.
    emulator
        .set_v2_logging_options("the switch cleared", &options("DEBUG", false))
        .await;
    let resumed = "switch-resumed-device";
    one_session(&address, resumed, "v2logging/resumed/telemetry").await;
    for event_type in ORDERED_BEFORE_THE_PUBACK {
        emulator
            .await_entry("the switch cleared", resumed, event_type)
            .await;
    }

    let entries = emulator.entries_of(suspended).await;
    let delivered: Vec<&str> = entries
        .iter()
        .filter_map(|entry| entry.get("eventType").and_then(Value::as_str))
        .filter(|event_type| ORDERED_BEFORE_THE_PUBACK.contains(event_type))
        .collect();
    assert!(
        delivered.is_empty(),
        "case \"the switch set\": expected no entry delivered, got {delivered:?}"
    );
}
