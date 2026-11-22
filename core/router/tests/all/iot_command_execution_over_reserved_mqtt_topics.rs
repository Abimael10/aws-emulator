// SPDX-License-Identifier: CC0-1.0

//! The IoT COMMANDS data plane over its reserved MQTT topics: how a started command
//! execution reaches its device, and how the device's own report on it drives the
//! execution the control plane then reports, refuses to delete and deletes.
//!
//! The rule under test: a command execution is carried out over the device's own reserved
//! topics. The command's payload is published verbatim on
//! `$aws/commands/things/{thingName}/executions/{executionId}/request/{format}` — by the
//! start for a device that is already subscribed, and on the SUBSCRIBE of a device that
//! was not, so the order of the device's own calls does not decide whether it is handed
//! the command — and a device's report on
//! `…/executions/{executionId}/response/{format}` is applied to that very execution and
//! answered on its `…/response/accepted/{format}` topic. From then on the control plane's
//! `GetCommandExecution` reports the status, the reason and the result the DEVICE sent,
//! and `DeleteCommandExecution` refuses the execution until the device's report has made
//! it terminal.
//!
//! It needs the WHOLE router: the command is created by the IoT control-plane crate, the
//! execution is started by the IoT Jobs data plane, and the broker that carries the
//! reserved topics is in neither.
//!
//! The rows differ along what the rule generalises over: whether the device subscribed
//! before or after the execution was started, the payload format the command's content
//! type puts the request on, the bytes of the payload (a JSON document, bytes that are no
//! text at all), whether the device reports progress before finishing, the terminal status
//! it finishes with, and whether it reports a reason, a result, both or neither.
//!
//! The session is spoken over a real TCP socket to the published address with the same
//! maintained MQTT codec a device library uses, and every wait is on a CONDITION (a packet
//! the broker writes) under a generous deadline.

use std::net::Ipv4Addr;
use std::time::Duration;

use bytes::BytesMut;
use serde_json::{json, Value};
use service_com_amazonaws_iotdataplane_iotmoonrakerservice::state::iot_mqtt_broker::mqttbytes::{
    self, v4, QoS,
};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// The router, the runtime and the published-address discovery this suite drives, and the
// one percent-encoder this test crate has: nothing new writes a second of either.
use crate::iot_shadow_reserved_topic_push::{discover, on_the_emulator};
use crate::query_wire::form_encode;

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// How long one leg of an exchange may take before the case fails. Generous: it bounds a
/// wait on a CONDITION (a packet the broker writes), not the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);

/// The `statusReason` a row's device reports: the reason code, and the description when it
/// gives one. Named rather than written inline, so the row's own type stays readable.
type ReportedReason = (&'static str, Option<&'static str>);

/// One command execution driven end to end over the reserved topics.
struct Row {
    case: &'static str,
    command_id: &'static str,
    thing_name: &'static str,
    client_id: &'static str,
    /// The literal payload the command is created with, which is what the device must be
    /// handed on the request topic.
    payload: &'static [u8],
    /// `CommandPayload$contentType`, when the row sets one.
    content_type: Option<&'static str>,
    /// The payload-format level the request must arrive on, which the content type
    /// decides: `json` for a JSON content type and for none at all, `cbor` for CBOR.
    format: &'static str,
    /// Whether the device subscribes BEFORE the execution is started. A row that does not
    /// subscribes afterwards and must still be handed the request.
    subscribes_before_start: bool,
    /// Whether the device reports `IN_PROGRESS` before it finishes. A device may only
    /// REJECT work it has not taken up, so the rejecting row finishes straight away.
    reports_progress: bool,
    /// The terminal status the device finishes with.
    terminal_status: &'static str,
    /// The `statusReason` the device reports with it, when it reports one.
    reason: Option<ReportedReason>,
    /// The `result` the device shares, as the report renders it, when it shares one.
    result: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a device already subscribed, succeeding with a reason and a result",
        command_id: "mqtt-reboot",
        thing_name: "mqtt-cmd-laser-1",
        client_id: "mqtt-cmd-device-1",
        payload: br#"{"action":"reboot"}"#,
        content_type: Some("application/json"),
        format: "json",
        subscribes_before_start: true,
        reports_progress: true,
        terminal_status: "SUCCEEDED",
        reason: Some(("DEVICE_OK", Some("the device rebooted"))),
        // `CommandExecutionResult` carries one member per attribute type (`S`, `B`,
        // `BIN`); a blob travels base64 (`AAEC` is 0x00 0x01 0x02).
        result: Some(r#"{"text":{"S":"ok"},"flag":{"B":true},"blob":{"BIN":"AAEC"}}"#),
    },
    Row {
        case: "a device that subscribes only after the execution was started, failing",
        command_id: "mqtt_set-temperature",
        thing_name: "mqtt-cmd-laser:line-2_A",
        client_id: "mqtt-cmd-device-2",
        payload: br#"{"temperature":21}"#,
        content_type: Some("application/json"),
        format: "json",
        subscribes_before_start: false,
        reports_progress: false,
        terminal_status: "FAILED",
        reason: Some(("SENSOR_UNREACHABLE", None)),
        result: None,
    },
    Row {
        case: "a payload the content type puts on the CBOR topic, rejected by the device",
        command_id: "mqtt-eject",
        thing_name: "mqtt-cmd-laser-3",
        client_id: "mqtt-cmd-device-3",
        // Bytes that are no text at all: the payload travels verbatim.
        payload: &[0xa1, 0x66, 0x61, 0x63, 0x74, 0x69, 0x6f, 0x6e, 0x00, 0xff],
        content_type: Some("application/cbor"),
        format: "cbor",
        subscribes_before_start: true,
        reports_progress: false,
        terminal_status: "REJECTED",
        reason: None,
        result: None,
    },
    Row {
        case: "a payload created with no content type, succeeding with a result alone",
        command_id: "mqtt-blink",
        thing_name: "mqtt-cmd-laser-4",
        client_id: "mqtt-cmd-device-4",
        payload: b"blink",
        content_type: None,
        format: "json",
        subscribes_before_start: false,
        reports_progress: true,
        terminal_status: "SUCCEEDED",
        reason: None,
        result: Some(r#"{"blinks":{"S":"3"}}"#),
    },
];

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router
/// picks the owning service by.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One in-process API call through the whole router: its status, its error type and its
/// body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    url: String,
    signing_name: &str,
    body: String,
) -> (u16, String, String, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let rendered = String::from_utf8_lossy(response.body()).to_string();
    let parsed: Value = serde_json::from_str(&rendered).unwrap_or(Value::Null);
    (status, error_code, rendered, parsed)
}

/// A control-plane call, signed for `iot`.
async fn control(
    router: &MultiServiceRouter,
    method: &str,
    path: String,
    body: String,
) -> (u16, String, String, Value) {
    call(
        router,
        method,
        format!("https://iot.{REGION}.amazonaws.com{path}"),
        "iot",
        body,
    )
    .await
}

/// A call on the Jobs data plane the device's own operations live on, signed for
/// `iot-jobs-data`.
async fn jobs_data(
    router: &MultiServiceRouter,
    method: &str,
    path: String,
    body: String,
) -> (u16, String, String, Value) {
    call(
        router,
        method,
        format!("https://data.jobs.iot.{REGION}.amazonaws.com{path}"),
        "iot-jobs-data",
        body,
    )
    .await
}

/// `GET /command-executions/{executionId}`, with the device the execution belongs to and
/// whether the device-reported result is asked for.
async fn read_execution(
    router: &MultiServiceRouter,
    execution_id: &str,
    target_arn: &str,
    include_result: bool,
) -> (u16, String, String, Value) {
    let result = if include_result {
        "&includeResult=true"
    } else {
        ""
    };
    control(
        router,
        "GET",
        format!(
            "/command-executions/{execution_id}?targetArn={}{result}",
            form_encode(target_arn)
        ),
        String::new(),
    )
    .await
}

/// `DELETE /command-executions/{executionId}` of one device's execution.
async fn delete_execution(
    router: &MultiServiceRouter,
    execution_id: &str,
    target_arn: &str,
) -> (u16, String, String, Value) {
    control(
        router,
        "DELETE",
        format!(
            "/command-executions/{execution_id}?targetArn={}",
            form_encode(target_arn)
        ),
        String::new(),
    )
    .await
}

/// The reserved topic prefix of one execution, written out here rather than read from the
/// service: `$aws/commands/things/{thingName}/executions/{executionId}`, the namespace the
/// IoT Commands topics hang off.
fn execution_prefix(thing_name: &str, execution_id: &str) -> String {
    format!("$aws/commands/things/{thing_name}/executions/{execution_id}")
}

/// A DEVICE: a real socket to the published address, speaking MQTT 3.1.1 on it, reading
/// every publication as the BYTES it carries.
struct Device {
    case: &'static str,
    stream: TcpStream,
    buffer: BytesMut,
}

impl Device {
    /// Dial `address`, send the CONNECT and assert the session was opened.
    async fn connect(case: &'static str, address: &str, client_id: &str) -> Self {
        let (host, port) = address
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("case {case:?}: {address:?} names no port"));
        assert_eq!(
            host,
            Ipv4Addr::LOCALHOST.to_string(),
            "case {case:?}: expected the published address on the loopback interface, got \
             {address:?}"
        );
        let port: u16 = port
            .parse()
            .unwrap_or_else(|error| panic!("case {case:?}: {address:?} names no port: {error}"));
        let stream = tokio::time::timeout(EXCHANGE_DEADLINE, TcpStream::connect((Ipv4Addr::LOCALHOST, port)))
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
    async fn subscribe(&mut self, filters: &[String]) -> Vec<v4::SubscribeReasonCode> {
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

    /// Publish `payload` on `topic` at QoS 0 — a device's own message to the service,
    /// which needs no acknowledgement of its own.
    async fn publish(&mut self, topic: &str, payload: Vec<u8>) {
        let publish = v4::Publish::new(topic, QoS::AtMostOnce, payload);
        self.write(|buffer| publish.write(buffer)).await;
    }

    /// Read the next publication the broker pushed, as its topic and the BYTES it
    /// carries, acknowledging it when it was made at QoS 1.
    async fn read_publication(&mut self) -> (String, Vec<u8>) {
        let packet = self.read_packet().await;
        let v4::Packet::Publish(publish) = packet else {
            panic!("case {:?}: expected a publication, got {packet:?}", self.case);
        };
        if publish.qos != QoS::AtMostOnce {
            let pkid = publish.pkid;
            self.write(|buffer| v4::PubAck::new(pkid).write(buffer))
                .await;
        }
        (publish.topic, publish.payload.to_vec())
    }

    /// Read the next publication and parse its body as the JSON document it must be.
    async fn read_json_publication(&mut self) -> (String, Value) {
        let (topic, payload) = self.read_publication().await;
        let body: Value = serde_json::from_slice(&payload).unwrap_or_else(|error| {
            panic!(
                "case {:?}: the message on {topic} must be a JSON document: {error}",
                self.case
            )
        });
        (topic, body)
    }
}

/// `PUT /commands/{commandId}` with the literal payload the command hands its devices.
async fn create_command(
    router: &MultiServiceRouter,
    case: &str,
    command_id: &str,
    payload: &[u8],
    content_type: Option<&str>,
) {
    let mut body = json!({ "payload": { "content": aws_smithy_types::base64::encode(payload) } });
    if let Some(content_type) = content_type {
        body["payload"]["contentType"] = json!(content_type);
    }
    let (status, _, rendered, _) = control(
        router,
        "PUT",
        format!("/commands/{command_id}"),
        body.to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateCommand must answer 200, got {status} with {rendered}"
    );
}

/// `POST /things/{thingName}`, so the device the executions address is one this account
/// holds.
async fn create_thing(router: &MultiServiceRouter, case: &str, thing_name: &str) {
    let (status, _, rendered, _) =
        control(router, "POST", format!("/things/{thing_name}"), "{}".to_string()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateThing({thing_name}) must answer 200, got {status} with {rendered}"
    );
}

/// `POST /command-executions` on the Jobs data plane, answering the minted `executionId`.
async fn start_execution(
    router: &MultiServiceRouter,
    case: &str,
    target_arn: &str,
    command_arn: &str,
) -> String {
    let body = json!({ "targetArn": target_arn, "commandArn": command_arn });
    let (status, _, rendered, parsed) = jobs_data(
        router,
        "POST",
        "/command-executions".to_string(),
        body.to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartCommandExecution must answer 200, got {status} with {rendered}"
    );
    let execution_id = parsed["executionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !execution_id.is_empty(),
        "case {case:?}: the start must mint an executionId, got {rendered}"
    );
    execution_id
}

/// The report body a row's device publishes with `status`.
fn report_body(row: &Row, status: &str) -> Value {
    let mut body = json!({ "status": status });
    if let Some((code, description)) = row.reason {
        let mut reason = json!({ "reasonCode": code });
        if let Some(description) = description {
            reason["reasonDescription"] = json!(description);
        }
        body["statusReason"] = reason;
    }
    if let Some(result) = row.result {
        body["result"] = serde_json::from_str(result)
            .unwrap_or_else(|error| panic!("case {:?}: the row's result is JSON: {error}", row.case));
    }
    body
}

/// A device is handed the command's payload on its execution's reserved request topic —
/// whether it subscribed before or after the execution was started — and the report it
/// publishes back is applied to that execution, acknowledged on the execution's own
/// `accepted` topic and reported by the control plane.
#[test]
fn a_command_execution_is_delivered_to_its_device_and_driven_by_the_devices_report() {
    on_the_emulator("iot-command-reserved-topics", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            for row in ROWS {
                let case = row.case;
                let target = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{}", row.thing_name);
                let command =
                    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/{}", row.command_id);

                create_thing(&router, case, row.thing_name).await;
                create_command(&router, case, row.command_id, row.payload, row.content_type).await;

                // The filters a device of this command listens on: the request topic of
                // any of its own executions, and the two answers to its own reports.
                let request_filter = format!(
                    "$aws/commands/things/{}/executions/+/request/{}",
                    row.thing_name, row.format
                );
                let accepted_filter = format!(
                    "$aws/commands/things/{}/executions/+/response/accepted/json",
                    row.thing_name
                );
                let filters = vec![request_filter.clone(), accepted_filter];

                let mut device = if row.subscribes_before_start {
                    let mut device = Device::connect(case, &address, row.client_id).await;
                    let granted = device.subscribe(&filters).await;
                    assert_eq!(
                        granted,
                        vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
                        "case {case:?}: expected QoS 1 granted for every reserved filter, got \
                         {granted:?}"
                    );
                    Some(device)
                } else {
                    None
                };

                let execution_id = start_execution(&router, case, &target, &command).await;

                // A device that was not subscribed when the execution was started is
                // handed the request when it subscribes, so the order of its own calls
                // does not decide whether it receives the command.
                let mut device = match device.take() {
                    Some(device) => device,
                    None => {
                        let mut device = Device::connect(case, &address, row.client_id).await;
                        let granted = device.subscribe(&filters).await;
                        assert_eq!(
                            granted,
                            vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
                            "case {case:?}: expected QoS 1 granted for every reserved filter, \
                             got {granted:?}"
                        );
                        device
                    }
                };

                let prefix = execution_prefix(row.thing_name, &execution_id);
                let (topic, delivered) = device.read_publication().await;
                assert_eq!(
                    topic,
                    format!("{prefix}/request/{}", row.format),
                    "case {case:?}: expected the request on the execution's own reserved topic, \
                     got {topic:?}"
                );
                assert_eq!(
                    delivered,
                    row.payload.to_vec(),
                    "case {case:?}: expected the command's own payload bytes, got {:?}",
                    String::from_utf8_lossy(&delivered)
                );

                let response_topic = format!("{prefix}/response/json");
                let accepted_topic = format!("{prefix}/response/accepted/json");

                if row.reports_progress {
                    device
                        .publish(
                            &response_topic,
                            serde_json::to_vec(&json!({ "status": "IN_PROGRESS" }))
                                .expect("the report serializes"),
                        )
                        .await;
                    let (topic, body) = device.read_json_publication().await;
                    assert_eq!(
                        (topic.as_str(), body["executionId"].as_str(), body["status"].as_str()),
                        (
                            accepted_topic.as_str(),
                            Some(execution_id.as_str()),
                            Some("IN_PROGRESS")
                        ),
                        "case {case:?}: expected the progress report accepted for this \
                         execution, got {topic} with {body}"
                    );
                    let (status, _, rendered, reported) =
                        read_execution(&router, &execution_id, &target, false).await;
                    assert_eq!(
                        (status, reported["status"].as_str()),
                        (200, Some("IN_PROGRESS")),
                        "case {case:?}: the control plane must report the status the device \
                         reported, got {status} with {rendered}"
                    );

                    // "Only command executions that enter a terminal state can be
                    // deleted": the device has not finished, so the delete is refused and
                    // changes nothing.
                    let (status, error_code, rendered, refusal) =
                        delete_execution(&router, &execution_id, &target).await;
                    assert_eq!(
                        (status, error_code.as_str(), refusal["message"].as_str()),
                        (
                            400,
                            "ValidationException",
                            Some(
                                "Failed to delete command execution as it hasn't become terminal."
                            ),
                        ),
                        "case {case:?}: deleting a non-terminal execution must be refused, got \
                         {status} with {rendered}"
                    );
                }

                // The device finishes the work, with the reason and the result the row
                // reports.
                device
                    .publish(
                        &response_topic,
                        serde_json::to_vec(&report_body(row, row.terminal_status))
                            .expect("the report serializes"),
                    )
                    .await;
                let (topic, body) = device.read_json_publication().await;
                assert_eq!(
                    (topic.as_str(), body["status"].as_str()),
                    (accepted_topic.as_str(), Some(row.terminal_status)),
                    "case {case:?}: expected the terminal report accepted, got {topic} with \
                     {body}"
                );

                // The control plane reports what the DEVICE sent, and reports the result
                // only to a caller that asked for it.
                let (status, _, rendered, reported) =
                    read_execution(&router, &execution_id, &target, true).await;
                assert_eq!(
                    (status, reported["status"].as_str()),
                    (200, Some(row.terminal_status)),
                    "case {case:?}: expected the device's terminal status, got {status} with \
                     {rendered}"
                );
                let expected_reason = row.reason.map(|(code, description)| {
                    let mut reason = json!({ "reasonCode": code });
                    if let Some(description) = description {
                        reason["reasonDescription"] = json!(description);
                    }
                    reason
                });
                assert_eq!(
                    reported.get("statusReason").cloned().filter(|value| !value.is_null()),
                    expected_reason,
                    "case {case:?}: expected the reason the device reported, got {rendered}"
                );
                let expected_result: Option<Value> = row
                    .result
                    .map(|result| serde_json::from_str(result).expect("the row's result is JSON"));
                assert_eq!(
                    reported.get("result").cloned().filter(|value| !value.is_null()),
                    expected_result,
                    "case {case:?}: expected the result the device shared, got {rendered}"
                );
                assert!(
                    reported["completedAt"].as_f64().is_some(),
                    "case {case:?}: a finished execution reports when it completed, got {rendered}"
                );
                let (_, _, rendered, without_result) =
                    read_execution(&router, &execution_id, &target, false).await;
                assert!(
                    without_result.get("result").is_none_or(Value::is_null),
                    "case {case:?}: the result is reported only to a caller that asked for it, \
                     got {rendered}"
                );

                // Terminal, so the delete goes through — and the execution is then gone.
                let (status, _, rendered, _) =
                    delete_execution(&router, &execution_id, &target).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: deleting a terminal execution must answer 200, got {status} \
                     with {rendered}"
                );
                let (status, error_code, rendered, gone) =
                    read_execution(&router, &execution_id, &target, false).await;
                assert_eq!(
                    (status, error_code.as_str(), gone["message"].as_str()),
                    (
                        404,
                        "ResourceNotFoundException",
                        Some(format!("Command execution {execution_id} cannot be found.").as_str()),
                    ),
                    "case {case:?}: a deleted execution is no longer reported, got {status} with \
                     {rendered}"
                );
            }
        });
    });
}

/// One report the service will not apply, and the refusal it is answered with.
struct Refusal {
    case: &'static str,
    /// The execution the report addresses: `true` for the one the start minted, `false`
    /// for an id the device holds nothing under.
    on_the_started_execution: bool,
    /// The report's body, as it goes on the wire.
    body: &'static str,
    code: &'static str,
}

/// The rows span what the rule generalises over: an execution that does not exist, a body
/// that is no report at all, a status no device may report (the service's own `CREATED`,
/// the clock's own `TIMED_OUT`, a value the enum does not declare), a reason outside what
/// `StatusReason` admits, and a status the execution cannot move BACK to.
const REFUSALS: &[Refusal] = &[
    Refusal {
        case: "an execution the device holds nothing under",
        on_the_started_execution: false,
        body: r#"{"status":"SUCCEEDED"}"#,
        code: "ResourceNotFound",
    },
    Refusal {
        case: "a body that is no JSON document",
        on_the_started_execution: true,
        body: "not a document",
        code: "InvalidRequest",
    },
    Refusal {
        case: "a report that carries no status",
        on_the_started_execution: true,
        body: r#"{"result":{"text":{"S":"ok"}}}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "the status the service's own start put the execution in",
        on_the_started_execution: true,
        body: r#"{"status":"CREATED"}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "the status only the cloud's own timer reaches",
        on_the_started_execution: true,
        body: r#"{"status":"TIMED_OUT"}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "a status the enum does not declare",
        on_the_started_execution: true,
        body: r#"{"status":"DONE"}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "a reasonCode outside the pattern the member admits",
        on_the_started_execution: true,
        body: r#"{"status":"SUCCEEDED","statusReason":{"reasonCode":"lower case"}}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "a reason with no reasonCode at all",
        on_the_started_execution: true,
        body: r#"{"status":"SUCCEEDED","statusReason":{"reasonDescription":"why"}}"#,
        code: "InvalidRequest",
    },
    Refusal {
        case: "a result value that is no attribute object",
        on_the_started_execution: true,
        body: r#"{"status":"SUCCEEDED","result":{"text":"ok"}}"#,
        code: "InvalidRequest",
    },
];

/// A report the service cannot apply is REFUSED on the execution's own `rejected` topic,
/// naming why — and the execution it addressed is left exactly as it stood.
#[test]
fn a_report_the_service_cannot_apply_is_refused_on_the_rejected_topic() {
    const THING_NAME: &str = "mqtt-cmd-refusals";
    const COMMAND_ID: &str = "mqtt-refused-report";
    const CLIENT_ID: &str = "mqtt-cmd-refusal-device";
    const ABSENT_EXECUTION: &str = "no-such-execution";

    on_the_emulator("iot-command-reserved-topic-refusals", |router, runtime| {
        runtime.block_on(async {
            let address = discover(&router).await;
            let target = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{THING_NAME}");
            let command = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:command/{COMMAND_ID}");
            create_thing(&router, "the refusal table", THING_NAME).await;
            create_command(
                &router,
                "the refusal table",
                COMMAND_ID,
                br#"{"action":"noop"}"#,
                Some("application/json"),
            )
            .await;

            let mut device = Device::connect("the refusal table", &address, CLIENT_ID).await;
            let filters = vec![
                format!("$aws/commands/things/{THING_NAME}/executions/+/request/json"),
                format!("$aws/commands/things/{THING_NAME}/executions/+/response/accepted/json"),
                format!("$aws/commands/things/{THING_NAME}/executions/+/response/rejected/json"),
            ];
            let granted = device.subscribe(&filters).await;
            assert_eq!(
                granted,
                vec![v4::SubscribeReasonCode::Success(QoS::AtLeastOnce); filters.len()],
                "expected QoS 1 granted for every reserved filter, got {granted:?}"
            );

            let execution_id =
                start_execution(&router, "the refusal table", &target, &command).await;
            let (topic, _) = device.read_publication().await;
            assert_eq!(
                topic,
                format!(
                    "{}/request/json",
                    execution_prefix(THING_NAME, &execution_id)
                ),
                "the started execution's request must reach the device first, got {topic:?}"
            );

            for refusal in REFUSALS {
                let case = refusal.case;
                let addressed = if refusal.on_the_started_execution {
                    execution_id.clone()
                } else {
                    ABSENT_EXECUTION.to_string()
                };
                let prefix = execution_prefix(THING_NAME, &addressed);
                device
                    .publish(
                        &format!("{prefix}/response/json"),
                        refusal.body.as_bytes().to_vec(),
                    )
                    .await;
                let (topic, body) = device.read_json_publication().await;
                assert_eq!(
                    (
                        topic.as_str(),
                        body["executionId"].as_str(),
                        body["code"].as_str()
                    ),
                    (
                        format!("{prefix}/response/rejected/json").as_str(),
                        Some(addressed.as_str()),
                        Some(refusal.code)
                    ),
                    "case {case:?}: expected the report refused on the execution's rejected \
                     topic, got {topic} with {body}"
                );
                assert!(
                    body["message"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty()),
                    "case {case:?}: a refusal says why, got {body}"
                );
                if !refusal.on_the_started_execution {
                    assert_eq!(
                        body["message"].as_str(),
                        Some(
                            format!("Command execution {ABSENT_EXECUTION} cannot be found.")
                                .as_str()
                        ),
                        "case {case:?}: expected the sentence a missing command execution reads \
                         with, got {body}"
                    );
                }
                // Nothing was applied: the execution is still the one the start created.
                let (status, _, rendered, reported) =
                    read_execution(&router, &execution_id, &target, true).await;
                assert_eq!(
                    (
                        status,
                        reported["status"].as_str(),
                        reported.get("statusReason").cloned().unwrap_or(Value::Null),
                        reported.get("result").cloned().unwrap_or(Value::Null),
                    ),
                    (200, Some("CREATED"), Value::Null, Value::Null),
                    "case {case:?}: a refused report leaves the execution as it was, got {status} \
                     with {rendered}"
                );
            }

            // A report the service DOES apply, so the table's refusals are not the only
            // thing this device can do: and a status the execution cannot move BACK to is
            // then refused as the transition it is.
            device
                .publish(
                    &format!(
                        "{}/response/json",
                        execution_prefix(THING_NAME, &execution_id)
                    ),
                    serde_json::to_vec(&json!({ "status": "SUCCEEDED" }))
                        .expect("the report serializes"),
                )
                .await;
            let (topic, body) = device.read_json_publication().await;
            assert_eq!(
                (topic.as_str(), body["status"].as_str()),
                (
                    format!(
                        "{}/response/accepted/json",
                        execution_prefix(THING_NAME, &execution_id)
                    )
                    .as_str(),
                    Some("SUCCEEDED")
                ),
                "expected the report accepted, got {topic} with {body}"
            );
            device
                .publish(
                    &format!(
                        "{}/response/json",
                        execution_prefix(THING_NAME, &execution_id)
                    ),
                    serde_json::to_vec(&json!({ "status": "IN_PROGRESS" }))
                        .expect("the report serializes"),
                )
                .await;
            let (topic, body) = device.read_json_publication().await;
            assert_eq!(
                (topic.as_str(), body["code"].as_str()),
                (
                    format!(
                        "{}/response/rejected/json",
                        execution_prefix(THING_NAME, &execution_id)
                    )
                    .as_str(),
                    Some("InvalidStateTransition")
                ),
                "a finished execution moves no further, got {topic} with {body}"
            );
            let (status, _, rendered, reported) =
                read_execution(&router, &execution_id, &target, false).await;
            assert_eq!(
                (status, reported["status"].as_str()),
                (200, Some("SUCCEEDED")),
                "the refused transition left the terminal status as it was, got {status} with \
                 {rendered}"
            );
        });
    });
}
