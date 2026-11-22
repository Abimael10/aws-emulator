// SPDX-License-Identifier: CC0-1.0

//! What a DEVICE that follows the documented discovery path gets at the address
//! `DescribeEndpoint(endpointType="iot:Data-ATS")` publishes: an MQTT session, on either half
//! of that one address.
//!
//! RULE: the account's data endpoint carries the message broker, so a client that dials the
//! published address completes the MQTT handshake there — CONNECT answered with a CONNACK
//! accepting the session, SUBSCRIBE answered with a SUBACK granting a QoS, PUBLISH
//! acknowledged — whichever way the connection's encryption was negotiated, the TLS a device
//! dials the data endpoint with or the clear; and the publication it then makes is evaluated
//! by the account's topic rules the same way on either, so every enabled rule whose statement
//! selects it performs its actions for the message that statement selected, while a
//! publication addressed to the reserved Basic Ingest space `$aws/rules/<ruleName>/<topic>`
//! runs THAT rule and is delivered to no subscriber at all.
//!
//! Nothing about a rule's evaluation can read the transport, which is exactly what makes the
//! two legs below a differential: the same rules, the same publications and the same
//! assertions are driven over a plaintext connection and over a real TLS session on the same
//! published address, so a broker that served only one of them fails here.
//!
//! The rows differ along what the rule generalises over: the connection's encryption
//! (plaintext, TLS); whether the publication is ordinary or addressed to a rule by name; and
//! what the matched rule's statement states (the whole message, and a projection under a
//! condition that holds and one that does not). Every row asserts BOTH what the queue holds
//! and what the subscriber was handed, so a publication that fired the rule without bypassing
//! the subscribers — or bypassed them without firing the rule — fails.
//!
//! The services an `sqs` action reaches are mounted beside the IoT control plane on one
//! engine, the shape `core/router/src/lib.rs` mounts them in: the IoT data plane that reports
//! on the sessions the broker opens, Amazon SQS that takes the send, and the IAM store the
//! action's role lives in. Neither assertion waits on a span: each row publishes a SENTINEL
//! message after its own, on a topic whose rule is known to forward, and the row is judged
//! once that sentinel has arrived at the subscriber and in the queue.
//!
//! Why the suite is HERE, beside the broker's own control plane, rather than a transport row
//! added to the whole-router suite that drives the same rule in the clear
//! (`core/router/tests/all/iot_basic_ingest_mqtt_topic_rules.rs`, which spans more rule forms
//! and is the home of that half): the client side of a TLS handshake is a maintained crate's
//! (`rustls`/`tokio-rustls`, declared for this crate's tests), and a test target reaches only
//! the dependencies ITS OWN crate manifest declares — the router's declares no TLS connector,
//! and the only alternative there would be a hand-written TLS driver, which is the one thing a
//! test must not grow. So the encrypted half of the published address is driven from the crate
//! that can declare the connector, and that is what this suite is for.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    rules_engine::SQS_SEND_MESSAGE_ACTION, SERVICE_PRINCIPAL,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
// The MQTT codec the broker itself reads and writes the protocol with, re-exported by the
// crate that holds the broker (`service_common::iot_mqtt_broker`): the suite speaks the wire a
// client library speaks rather than laying packets out by hand.
use service_common::iot_mqtt_broker::mqttbytes::{self, v4, QoS};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

// The percent-encoding of an `awsQuery` form value and the reader of one XML element, as this
// crate's own test tree already exports them (`tests/all/topic_rule_kinesis_action.rs`):
// `automod` declares every suite as a sibling module, so the IAM calls below are written with
// those two helpers rather than with copies of them.
use crate::topic_rule_kinesis_action::{form_encode, xml_text};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// The address this emulator's API is served at, as the core publishes it from its data port.
/// The broker's own socket is a DIFFERENT address — one port serves one wire — and which one
/// that is, is the core's to choose, so the device reads it back rather than naming it.
const SERVED_AT: &str = "https://127.0.0.1:41733";

/// How long a leg of the exchange may take before the case fails, and how long between two
/// looks at the queue. Generous: both bound a wait on a CONDITION (a packet the broker writes,
/// a body the queue serves), not the speed of the host.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(60);
/// See [`EXCHANGE_DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The queue every rule below forwards to, and the role every `sqs` action is performed
/// under: it trusts the rules engine's own service principal and allows the send on that
/// queue.
const QUEUE: &str = "iot-mqtt-transport-forwarded";
/// See [`QUEUE`].
const ROLE: &str = "iot-mqtt-transport-rule";

/// The SQL version every rule is stored with (`TopicRulePayload$awsIotSqlVersion`).
const SQL_VERSION: &str = "2016-03-23";

/// The rule a row reaches by its topic filter and through Basic Ingest alike, and the topic
/// its `FROM` clause covers. A rule name carries letters, digits and underscores alone
/// (`RuleName`, `pattern ^[a-zA-Z0-9_]+$`).
const FORWARD_RULE: &str = "mqtt_transport_forward";
/// See [`FORWARD_RULE`].
const FORWARD_TOPIC: &str = "mqtttransport/telemetry";

/// The rule whose statement TRANSFORMS what it forwards — a projection under a condition —
/// and the topic its `FROM` clause covers.
const TRANSFORM_RULE: &str = "mqtt_transport_transform";
/// See [`TRANSFORM_RULE`].
const TRANSFORM_TOPIC: &str = "mqtttransport/transform";

/// The SENTINEL rule and its topic: an ordinary publication whose forward is known to work,
/// published after each row's own message, so a row expecting NOTHING is judged against a
/// queue and a subscriber that have both taken a LATER message.
const SENTINEL_RULE: &str = "mqtt_transport_sentinel";
/// See [`SENTINEL_RULE`].
const SENTINEL_TOPIC: &str = "mqtttransport/sentinel";

/// What the listening device subscribes to: every topic the rows publish on, and the whole
/// reserved Basic Ingest space — so a Basic Ingest publication that was delivered to ANY
/// subscriber is visible here.
const SUBSCRIBED_FILTERS: &[(&str, u8)] = &[("mqtttransport/#", 1), ("$aws/rules/#", 1)];

/// How many bodies one drain of the queue reads, and the long poll it makes: none, because the
/// broker performs the rule's action before it acknowledges the publication, so a message that
/// was forwarded is already in the queue by the time the first drain asks. The waiting a slow
/// host needs is the drain LOOP's ([`EXCHANGE_DEADLINE`]).
const RECEIVE_BATCH: u64 = 10;
/// See [`RECEIVE_BATCH`].
const WAIT_TIME_SECONDS: u64 = 0;

/// The message every row publishes unless it needs its own: a document whose members the
/// transforming rule's projection and condition read.
const MESSAGE: &str = r#"{"marker":"m-1","temp":42,"extra":"carried"}"#;

/// What the transforming rule SELECTS out of [`MESSAGE`]: the two attributes its projection
/// names, and nothing else.
const TRANSFORMED: &str = r#"{"marker":"m-1","temp":42}"#;

/// A message the transforming rule's condition does NOT hold for.
const COLD_MESSAGE: &str = r#"{"marker":"m-cold","temp":4,"extra":"carried"}"#;

/// How a device's connection to the published address is encrypted: the two halves one
/// announced address serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// The connection a client makes in the clear.
    Clear,
    /// The TLS a client of AWS IoT Core's message broker dials the data endpoint with, which
    /// the core terminates.
    Tls,
}

/// Both halves of the published address, which is what the legs below span.
const TRANSPORTS: &[Transport] = &[Transport::Clear, Transport::Tls];

impl Transport {
    /// How a case names this transport.
    const fn label(self) -> &'static str {
        match self {
            Self::Clear => "in the clear",
            Self::Tls => "over TLS",
        }
    }

    /// The suffix a leg's client ids carry, so the two legs' sessions are distinct clients
    /// rather than one client id taken over twice.
    const fn client_suffix(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Tls => "tls",
        }
    }

    /// Open one connection to `address` over this transport, as a device's own client does.
    async fn open(self, address: &str) -> Session {
        let port = loopback_port(address);
        let dialling = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
        let socket = tokio::time::timeout(EXCHANGE_DEADLINE, dialling)
            .await
            .unwrap_or_else(|_| panic!("transport {}: dialling {address} timed out", self.label()))
            .unwrap_or_else(|error| {
                panic!(
                    "transport {}: the published address {address} must accept a connection: \
                     {error}",
                    self.label()
                )
            });
        match self {
            Self::Clear => Box::new(socket),
            Self::Tls => Box::new(upgraded(socket, self.label()).await),
        }
    }
}

/// Run the client side of a TLS handshake on `socket`, answering with the encrypted session —
/// and asserting a TLS version really was negotiated, so the leg is an encrypted connection
/// rather than an assumption about one.
async fn upgraded(socket: TcpStream, label: &str) -> tokio_rustls::client::TlsStream<TcpStream> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a server name");
    let session = connector
        .connect(server_name, socket)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "transport {label}: the published address must complete a TLS handshake, got \
                 {error}"
            )
        });
    let negotiated = session.get_ref().1.protocol_version();
    assert!(
        matches!(
            negotiated,
            Some(rustls::ProtocolVersion::TLSv1_2 | rustls::ProtocolVersion::TLSv1_3)
        ),
        "transport {label}: expected a negotiated TLS version, got {negotiated:?}"
    );
    session
}

/// The verifier a client of the emulator's OWN socket uses: the certificate is the one the
/// core generated for its listeners, self-signed and naming loopback, and this client is the
/// emulator's own test connecting to a port the same process opened — so there is no authority
/// to check it against.
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// One connection to the published address, whatever its encryption: what a device reads its
/// packets off and writes them to.
trait Wire: AsyncRead + AsyncWrite + Unpin + Send {}

impl<Stream: AsyncRead + AsyncWrite + Unpin + Send> Wire for Stream {}

/// See [`Wire`].
type Session = Box<dyn Wire>;

/// One row: the topic the device publishes on, the message it sends, and what each side must
/// then hold.
struct Row {
    case: &'static str,
    /// The topic the PUBLISH packet carries, which for a Basic Ingest row is the reserved form.
    topic: String,
    /// The message as published.
    payload: &'static str,
    /// The message bodies the queue must hold afterwards, besides the sentinel's.
    forwarded: &'static [&'static str],
    /// The payloads the subscriber must be handed before the sentinel's, in order.
    delivered: &'static [&'static str],
}

/// What each leg publishes, and what the queue and the subscriber must then hold: the rows
/// span whether the publication is ordinary or addressed to a rule by name, and what the
/// matched rule's statement states.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an ordinary publication on the forwarding rule's own topic",
            topic: FORWARD_TOPIC.to_string(),
            payload: MESSAGE,
            forwarded: &[MESSAGE],
            delivered: &[MESSAGE],
        },
        Row {
            case: "a Basic Ingest publication naming the forwarding rule",
            topic: format!("$aws/rules/{FORWARD_RULE}/{FORWARD_TOPIC}"),
            payload: MESSAGE,
            forwarded: &[MESSAGE],
            delivered: &[],
        },
        Row {
            case: "an ordinary publication the transforming rule's condition selects",
            topic: TRANSFORM_TOPIC.to_string(),
            payload: MESSAGE,
            forwarded: &[TRANSFORMED],
            delivered: &[MESSAGE],
        },
        Row {
            case: "an ordinary publication the transforming rule's condition does not select",
            topic: TRANSFORM_TOPIC.to_string(),
            payload: COLD_MESSAGE,
            forwarded: &[],
            delivered: &[COLD_MESSAGE],
        },
        Row {
            case: "a Basic Ingest publication the transforming rule's condition selects",
            topic: format!("$aws/rules/{TRANSFORM_RULE}/{TRANSFORM_TOPIC}"),
            payload: MESSAGE,
            forwarded: &[TRANSFORMED],
            delivered: &[],
        },
    ]
}

/// The emulator: the IoT control plane, the IoT data plane, Amazon SQS and IAM on ONE engine —
/// the shape `core/router/src/lib.rs` mounts them in — served at [`SERVED_AT`].
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope is what routes a request here.
    router.set_request_authentication(false);
    router
        .set_url_base(EndpointUrl::parse(SERVED_AT).expect("the served base is a usable endpoint"));
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iotdataplane_iotmoonrakerservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    Emulator { router }
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`] — the signal the router picks
/// the owning service by. The signature is never checked in this harness.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

impl Emulator {
    /// One in-process API call through the whole router, as its status and body.
    async fn send(
        &self,
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
        let response = self
            .router
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
    async fn sqs(&self, operation: &str, body: &Value) -> Value {
        let (status, rendered) = self
            .send(
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

    /// One IAM call over the `awsQuery` protocol that must be served, answered with its XML
    /// body.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, rendered) = self
            .send(
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

    /// `GET /endpoint?endpointType=iot:Data-ATS`: the address the control plane publishes for
    /// the account's data endpoint, which is the address a device then dials.
    async fn discover(&self) -> String {
        let (status, rendered) = self
            .send(
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
    /// suite's queue, as `CreateTopicRule` takes it: the body IS the `topicRulePayload`.
    async fn create_topic_rule(&self, rule: &str, sql: &str, role_arn: &str, queue_url: &str) {
        let (status, rendered) = self
            .send(
                "POST",
                &format!("/rules/{rule}"),
                "iot",
                "application/json",
                &[],
                json!({
                    "sql": sql,
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
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

    /// The role every `sqs` action runs under: it trusts the rules engine's service principal
    /// and allows the send on `queue_arn`. Answered with the ARN IAM persisted.
    async fn given_role(&self, queue_arn: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": SERVICE_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let created = self
            .iam(
                "CreateRole",
                &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
            )
            .await;
        let arn = xml_text(&created, "Arn")
            .unwrap_or_else(|| panic!("setup: CreateRole reported no Arn: {created}"));
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": SQS_SEND_MESSAGE_ACTION,
                "Resource": queue_arn,
            }],
        })
        .to_string();
        self.iam(
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

    /// One drain of the queue: the bodies it served this time, which is none once everything
    /// sent to it has been read.
    async fn drained_once(&self, queue_url: &str) -> Vec<String> {
        let received = self
            .sqs(
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
    /// The sentinel is forwarded by a rule whose action is known to be performable and is
    /// published after the row's own message, so a row that expects NOTHING is judged against a
    /// queue that has already taken a LATER publication — rather than against a queue nothing
    /// has reached yet, which would pass whatever the engine did.
    async fn forwarded_bodies(&self, case: &str, queue_url: &str, sentinel: &str) -> Vec<String> {
        let giving_up_at = Instant::now() + EXCHANGE_DEADLINE;
        let mut held: Vec<String> = Vec::new();
        loop {
            let batch = self.drained_once(queue_url).await;
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
            "case {case:?}: the sentinel publication was never forwarded, so what the queue \
             holds says nothing about this row; it held {held:?}"
        );
        held.retain(|body| !same_message(sentinel, body));
        held
    }
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

/// The QoS a number names, as a client's own library reads it. A number outside the three MQTT
/// defines names no QoS at all, so a filter table carrying one asks for a subscription the
/// protocol has no word for: it fails here rather than silently becoming QoS 2.
fn qos_of(number: u8) -> QoS {
    match number {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        unmodelled => panic!("MQTT defines no QoS {unmodelled}"),
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

/// A DEVICE: a real connection to the published address, speaking MQTT 3.1.1 on it.
struct Device {
    client_id: String,
    session: Session,
    buffer: BytesMut,
    /// The packet identifier the next packet of this device carries.
    next_packet_id: u16,
}

impl Device {
    /// Dial the published address over `transport` and send the CONNECT, answering with the
    /// connected device — which is what the CONNACK accepting the session says it is.
    async fn connect(transport: Transport, address: &str, role: &str) -> Self {
        let client_id = format!("mqtt-transport-{role}-{}", transport.client_suffix());
        let mut device = Self {
            client_id,
            session: transport.open(address).await,
            buffer: BytesMut::new(),
            next_packet_id: 1,
        };
        let mut connect = v4::Connect::new(device.client_id.clone());
        connect.clean_session = true;
        connect.keep_alive = 60;
        device.write(|buffer| connect.write(buffer)).await;
        let answered = device.expect_packet().await;
        let v4::Packet::ConnAck(connack) = answered else {
            panic!(
                "device {} {}: expected a CONNACK, got {answered:?}",
                device.client_id,
                transport.label()
            );
        };
        assert_eq!(
            connack.code,
            v4::ConnectReturnCode::Success,
            "device {} {}: the broker must accept the CONNECT, got {:?}",
            device.client_id,
            transport.label(),
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
        self.session
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
                    Err(error) => {
                        panic!("device {client_id}: the broker wrote no packet: {error}")
                    }
                }
                let mut chunk = [0u8; 4096];
                match self.session.read(&mut chunk).await {
                    Ok(0) => return None,
                    Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                    Err(error) => {
                        panic!("device {client_id}: reading the session failed: {error}")
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
            panic!(
                "device {}: expected a SUBACK, got {answered:?}",
                self.client_id
            );
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

/// A device's MQTT session at the published data endpoint is served on either half of that
/// address, and the topic rules evaluate what it publishes the same way on both — Basic
/// Ingest bypassing the subscribers, the statement's projection and condition deciding what
/// the queue holds.
#[tokio::test]
async fn a_published_data_endpoint_serves_the_rules_engine_on_either_half_of_its_address() {
    let emulator = emulator();

    // The target of every action, and the role it is performed under.
    let queue_url = emulator.sqs("CreateQueue", &json!({ "QueueName": QUEUE })).await["QueueUrl"]
        .as_str()
        .expect("CreateQueue reports a QueueUrl")
        .to_string();
    let queue_arn = emulator
        .sqs(
            "GetQueueAttributes",
            &json!({ "QueueUrl": queue_url, "AttributeNames": ["QueueArn"] }),
        )
        .await["Attributes"]["QueueArn"]
        .as_str()
        .expect("GetQueueAttributes reports the QueueArn")
        .to_string();
    let role_arn = emulator.given_role(&queue_arn).await;

    for (rule, sql) in [
        (FORWARD_RULE, format!("SELECT * FROM '{FORWARD_TOPIC}'")),
        (
            TRANSFORM_RULE,
            format!("SELECT marker, temp FROM '{TRANSFORM_TOPIC}' WHERE temp > 30"),
        ),
        (SENTINEL_RULE, format!("SELECT * FROM '{SENTINEL_TOPIC}'")),
    ] {
        emulator
            .create_topic_rule(rule, &sql, &role_arn, &queue_url)
            .await;
    }

    // Where this account's data plane answers, which is the address a device dials — and the
    // call that opens the broker's socket.
    let address = emulator.discover().await;

    for transport in TRANSPORTS {
        let leg = transport.label();
        let mut subscriber = Device::connect(*transport, &address, "subscriber").await;
        let granted = subscriber.subscribe(SUBSCRIBED_FILTERS).await;
        assert_eq!(
            granted,
            vec![1, 1],
            "leg {leg:?}: expected QoS 1 granted for both filters, got {granted:?}"
        );
        let mut publisher = Device::connect(*transport, &address, "publisher").await;

        for (index, row) in rows().iter().enumerate() {
            let case = format!("{} {leg}", row.case);
            let sentinel = json!({ "sentinel": index, "leg": leg }).to_string();

            publisher.publish(&row.topic, row.payload).await;
            publisher.publish(SENTINEL_TOPIC, &sentinel).await;

            // What the SUBSCRIBER was handed: everything before the sentinel, which the broker
            // delivers after the row's own publication over the same connection.
            let delivered = subscriber.deliveries_before_sentinel(&sentinel).await;
            let payloads: Vec<String> = delivered
                .iter()
                .map(|one| String::from_utf8_lossy(&one.payload).to_string())
                .collect();
            assert_eq!(
                payloads.len(),
                row.delivered.len(),
                "case {case:?}: expected {} delivery(ies) to the subscriber, got {delivered:?}",
                row.delivered.len()
            );
            for (expected, actual) in row.delivered.iter().zip(payloads.iter()) {
                assert!(
                    same_message(expected, actual),
                    "case {case:?}: expected the subscriber handed {expected:?}, got {actual:?}"
                );
            }

            // What the rule's action sent, read where a client reads it: out of the queue.
            let forwarded = emulator
                .forwarded_bodies(&case, &queue_url, &sentinel)
                .await;
            assert_eq!(
                forwarded.len(),
                row.forwarded.len(),
                "case {case:?}: expected {} message(s) forwarded to the queue, got {forwarded:?}",
                row.forwarded.len()
            );
            for expected in row.forwarded {
                assert!(
                    forwarded.iter().any(|body| same_message(expected, body)),
                    "case {case:?}: expected {expected:?} in the queue, got {forwarded:?}"
                );
            }
        }
    }
}
