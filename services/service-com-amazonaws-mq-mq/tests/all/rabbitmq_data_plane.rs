// SPDX-License-Identifier: CC0-1.0

//! The DATA PLANE of an Amazon MQ for RabbitMQ broker: that the `amqps://` endpoint the
//! control plane publishes is an address an AMQP 0-9-1 client of THIS emulator can open
//! a session on, and that what the session publishes is what another session consumes.
//!
//! The rules under test:
//!
//! * an endpoint a broker publishes is an address the broker answers on, so a RUNNING
//!   RabbitMQ broker's one `BrokerInstance` carries an `amqps://` endpoint on a socket
//!   this emulator serves AMQP 0-9-1 on, plus the console URL, and a client dialling it
//!   authenticates as one of the broker's own users;
//! * a published message is routed by the EXCHANGE it names and waits in each queue it
//!   reached, in publication order, until a consumer acknowledges it — across
//!   connections, which is what a durable queue and publisher confirms are for;
//! * a message a consumer rejects without requeueing, one that outlives its expiration
//!   and one that outlives its queue's length bound all leave for the queue's
//!   dead-letter exchange carrying the reason they died.
//!
//! Every assertion goes over a real socket the core opened for the broker, dialled at
//! the address the control plane published, and reads only what a client can see.
//!
//! The frames are built and parsed with the same `amq-protocol` codec the server uses,
//! which is what a client library does: nothing here reaches into the service's state.

use std::sync::Arc;
use std::time::Duration;

use amq_protocol::frame::{gen_frame, parse_frame, AMQPContentHeader, AMQPFrame};
use amq_protocol::protocol::basic::AMQPProperties;
use amq_protocol::protocol::{basic, channel, confirm, connection, exchange, queue, AMQPClass};
use amq_protocol::types::{AMQPValue, FieldTable, LongString, ShortString};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::broker_lifecycle::{create_body, string_at, Mq};

/// The address the core binds its listeners on by default, which is the address a
/// broker of this emulator publishes and the only one these cases dial.
const LOOPBACK: &str = "127.0.0.1";

/// The frame size this suite's client negotiates, which is the one the broker offers.
const FRAME_MAX: usize = 131_072;

/// The user every broker in this suite is created with.
const USERNAME: &str = "mq-plane-user";

/// That user's password. A broker is created with its users inline, and this is the one
/// the session authenticates with.
const PASSWORD: &str = "Str0ng-Plane-Pass";

/// How long a step waits for the condition it needs before the test gives up. Generous:
/// what is being waited for is the emulator's own clock-owed transition, and a loaded
/// host must not fail a case.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long one frame read waits. A session that is owed a frame gets it immediately;
/// this only bounds a test that would otherwise hang.
const FRAME_WAIT: Duration = Duration::from_secs(10);

/// How often a poll loop looks again. Nothing is asserted about elapsed time.
const POLL_EVERY: Duration = Duration::from_millis(10);

/// A byte stream an AMQP session runs on: the TLS stream over the broker's socket.
type Stream = Box<dyn Transport>;

/// What a session needs of its connection.
trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}

/// One message as a client reads it off a delivery: the method that carried it, its
/// properties and its body.
#[derive(Debug, Clone)]
struct Received {
    exchange: String,
    routing_key: String,
    delivery_tag: u64,
    properties: AMQPProperties,
    body: Vec<u8>,
}

impl Received {
    /// The body as text, which is what the cases publish.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The reason the newest entry of this message's `x-death` history carries, or
    /// `None` for a message that never died.
    fn death_reason(&self) -> Option<String> {
        let headers = self.properties.headers().clone()?;
        let death = headers
            .inner()
            .iter()
            .find(|(name, _)| name.as_str() == "x-death")
            .map(|(_, value)| value.clone())?;
        let AMQPValue::FieldArray(history) = death else {
            return None;
        };
        let AMQPValue::FieldTable(newest) = history.as_slice().first()?.clone() else {
            return None;
        };
        newest
            .inner()
            .iter()
            .find(|(name, _)| name.as_str() == "reason")
            .and_then(|(_, value)| match value {
                AMQPValue::LongString(text) => {
                    Some(String::from_utf8_lossy(text.as_bytes()).to_string())
                }
                AMQPValue::ShortString(text) => Some(text.as_str().to_string()),
                _ => None,
            })
    }

    /// The queue the newest `x-death` entry names.
    fn death_queue(&self) -> Option<String> {
        let headers = self.properties.headers().clone()?;
        let death = headers
            .inner()
            .iter()
            .find(|(name, _)| name.as_str() == "x-death")
            .map(|(_, value)| value.clone())?;
        let AMQPValue::FieldArray(history) = death else {
            return None;
        };
        let AMQPValue::FieldTable(newest) = history.as_slice().first()?.clone() else {
            return None;
        };
        newest
            .inner()
            .iter()
            .find(|(name, _)| name.as_str() == "queue")
            .and_then(|(_, value)| match value {
                AMQPValue::LongString(text) => {
                    Some(String::from_utf8_lossy(text.as_bytes()).to_string())
                }
                _ => None,
            })
    }
}

/// One AMQP session: the connection, and the bytes read off it that are not a frame yet.
struct Session {
    case: String,
    stream: Stream,
    buffered: Vec<u8>,
}

impl Session {
    /// Open a TLS connection to `host:port` — an `amqps://` endpoint is TLS from the
    /// first byte — and run the AMQP handshake on it as `username`.
    ///
    /// The address dialled is the loopback one the control plane published for the
    /// broker: the core binds its listeners on the operator-chosen interface, loopback
    /// by default, so the port a broker publishes is a port on THIS machine, which is
    /// what the assertion below pins before anything is dialled.
    ///
    /// Answers the refusal's reply code instead of a session when the broker closed the
    /// connection during the handshake, which is what a client library surfaces as an
    /// authentication failure.
    async fn open(
        case: &str,
        host: &str,
        port: u16,
        username: &str,
        password: &str,
    ) -> Result<Self, u16> {
        assert_eq!(
            host, LOOPBACK,
            "case {case:?}: the published endpoint must be this machine's loopback address, got \
             {host:?}"
        );
        let tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: {host}:{port} must accept a connection, got {error}")
            });
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("the provider supports the default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .expect("localhost is a server name");
        let tls = connector
            .connect(server_name, tcp)
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the broker's socket must complete a TLS handshake, got {error}")
            });
        let mut session = Self {
            case: case.to_string(),
            stream: Box::new(tls),
            buffered: Vec::new(),
        };
        session.handshake(username, password).await?;
        Ok(session)
    }

    /// The client half of the AMQP 0-9-1 handshake.
    async fn handshake(&mut self, username: &str, password: &str) -> Result<(), u16> {
        self.write_bytes(&[b'A', b'M', b'Q', b'P', 0, 0, 9, 1]).await;
        match self.read_frame().await {
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Start(start))) => {
                assert_eq!(
                    String::from_utf8_lossy(start.mechanisms.as_bytes()),
                    "PLAIN",
                    "case {:?}: expected the broker to offer PLAIN authentication",
                    self.case
                );
            }
            other => panic!(
                "case {:?}: expected connection.start, got {other:?}",
                self.case
            ),
        }
        let mut response = Vec::new();
        response.push(0u8);
        response.extend_from_slice(username.as_bytes());
        response.push(0u8);
        response.extend_from_slice(password.as_bytes());
        self.send(AMQPFrame::Method(
            0,
            AMQPClass::Connection(connection::AMQPMethod::StartOk(connection::StartOk {
                client_properties: FieldTable::default(),
                mechanism: ShortString::from("PLAIN"),
                response: LongString::from(response),
                locale: ShortString::from("en_US"),
            })),
        ))
        .await;
        match self.read_frame().await {
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Tune(_))) => {}
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(close))) => {
                return Err(close.reply_code);
            }
            other => panic!("case {:?}: expected connection.tune, got {other:?}", self.case),
        }
        self.send(AMQPFrame::Method(
            0,
            AMQPClass::Connection(connection::AMQPMethod::TuneOk(connection::TuneOk {
                channel_max: 0,
                frame_max: u32::try_from(FRAME_MAX).unwrap_or(u32::MAX),
                // Heartbeats off: a test's session is open for a moment, and nothing
                // here is asserted against the clock.
                heartbeat: 0,
            })),
        ))
        .await;
        self.send(AMQPFrame::Method(
            0,
            AMQPClass::Connection(connection::AMQPMethod::Open(connection::Open {
                virtual_host: ShortString::from("/"),
            })),
        ))
        .await;
        match self.read_frame().await {
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::OpenOk(_))) => {}
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(close))) => {
                return Err(close.reply_code);
            }
            other => panic!(
                "case {:?}: expected connection.open-ok, got {other:?}",
                self.case
            ),
        }
        self.send(AMQPFrame::Method(
            1,
            AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open::default())),
        ))
        .await;
        match self.read_frame().await {
            AMQPFrame::Method(1, AMQPClass::Channel(channel::AMQPMethod::OpenOk(_))) => Ok(()),
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(close))) => {
                Err(close.reply_code)
            }
            other => panic!(
                "case {:?}: expected channel.open-ok, got {other:?}",
                self.case
            ),
        }
    }

    /// Send one frame.
    async fn send(&mut self, frame: AMQPFrame) {
        let (bytes, _written) = gen_frame(&frame)(Vec::new().into())
            .unwrap_or_else(|error| {
                panic!("case {:?}: {frame:?} must serialize, got {error}", self.case)
            })
            .into_inner();
        self.write_bytes(&bytes).await;
    }

    async fn write_bytes(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap_or_else(|error| {
            panic!("case {:?}: the session must be writable, got {error}", self.case)
        });
        self.stream.flush().await.unwrap_or_else(|error| {
            panic!("case {:?}: the session must flush, got {error}", self.case)
        });
    }

    /// Send a method frame and read the next frame, which is its synchronous reply.
    async fn call(&mut self, method: AMQPClass) -> AMQPFrame {
        self.send(AMQPFrame::Method(1, method)).await;
        self.read_frame().await
    }

    /// Declare a queue on channel 1 with the arguments given.
    async fn declare_queue(&mut self, name: &str, durable: bool, arguments: FieldTable) -> String {
        let answer = self
            .call(AMQPClass::Queue(queue::AMQPMethod::Declare(
                queue::Declare {
                    queue: ShortString::from(name),
                    passive: false,
                    durable,
                    exclusive: false,
                    auto_delete: false,
                    nowait: false,
                    arguments,
                },
            )))
            .await;
        match answer {
            AMQPFrame::Method(_, AMQPClass::Queue(queue::AMQPMethod::DeclareOk(ok))) => {
                ok.queue.as_str().to_string()
            }
            other => panic!(
                "case {:?}: expected queue.declare-ok for {name:?}, got {other:?}",
                self.case
            ),
        }
    }

    /// Declare an exchange on channel 1.
    async fn declare_exchange(&mut self, name: &str, kind: &str) {
        let answer = self
            .call(AMQPClass::Exchange(exchange::AMQPMethod::Declare(
                exchange::Declare {
                    exchange: ShortString::from(name),
                    kind: ShortString::from(kind),
                    passive: false,
                    durable: true,
                    auto_delete: false,
                    internal: false,
                    nowait: false,
                    arguments: FieldTable::default(),
                },
            )))
            .await;
        assert!(
            matches!(
                answer,
                AMQPFrame::Method(_, AMQPClass::Exchange(exchange::AMQPMethod::DeclareOk(_)))
            ),
            "case {:?}: expected exchange.declare-ok for {name:?}/{kind:?}, got {answer:?}",
            self.case
        );
    }

    /// Bind a queue to an exchange on channel 1.
    async fn bind_queue(&mut self, name: &str, exchange: &str, routing_key: &str) {
        self.bind_queue_with(name, exchange, routing_key, FieldTable::default())
            .await;
    }

    /// Bind a queue to an exchange with binding arguments, which is what a headers
    /// exchange matches on.
    async fn bind_queue_with(
        &mut self,
        name: &str,
        exchange: &str,
        routing_key: &str,
        arguments: FieldTable,
    ) {
        let answer = self
            .call(AMQPClass::Queue(queue::AMQPMethod::Bind(queue::Bind {
                queue: ShortString::from(name),
                exchange: ShortString::from(exchange),
                routing_key: ShortString::from(routing_key),
                nowait: false,
                arguments,
            })))
            .await;
        assert!(
            matches!(
                answer,
                AMQPFrame::Method(_, AMQPClass::Queue(queue::AMQPMethod::BindOk(_)))
            ),
            "case {:?}: expected queue.bind-ok for {name:?} on {exchange:?}, got {answer:?}",
            self.case
        );
    }

    /// Put the channel in publisher-confirm mode.
    async fn select_confirms(&mut self) {
        let answer = self
            .call(AMQPClass::Confirm(confirm::AMQPMethod::Select(
                confirm::Select { nowait: false },
            )))
            .await;
        assert!(
            matches!(
                answer,
                AMQPFrame::Method(_, AMQPClass::Confirm(confirm::AMQPMethod::SelectOk(_)))
            ),
            "case {:?}: expected confirm.select-ok, got {answer:?}",
            self.case
        );
    }

    /// Publish one message: the method, its content header and its body.
    async fn publish(
        &mut self,
        exchange: &str,
        routing_key: &str,
        properties: AMQPProperties,
        body: &[u8],
    ) {
        self.send(AMQPFrame::Method(
            1,
            AMQPClass::Basic(basic::AMQPMethod::Publish(basic::Publish {
                exchange: ShortString::from(exchange),
                routing_key: ShortString::from(routing_key),
                mandatory: false,
                immediate: false,
            })),
        ))
        .await;
        self.send(AMQPFrame::Header(
            1,
            AMQPContentHeader {
                class_id: 60,
                body_size: body.len() as u64,
                properties,
            },
        ))
        .await;
        if !body.is_empty() {
            // Split into frames the connection negotiated room for, which is what a
            // client library does: a frame carries its own framing bytes besides the
            // payload.
            for chunk in body.chunks(FRAME_MAX - 8) {
                self.send(AMQPFrame::Body(1, chunk.to_vec())).await;
            }
        }
    }

    /// Read the publisher confirmation of the message whose sequence number is `expected`.
    async fn expect_confirm(&mut self, expected: u64) {
        let frame = self.read_frame().await;
        match frame {
            AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::Ack(ack))) => assert_eq!(
                ack.delivery_tag, expected,
                "case {:?}: expected the confirmation of publish {expected}, got {ack:?}",
                self.case
            ),
            other => panic!(
                "case {:?}: expected a publisher confirmation, got {other:?}",
                self.case
            ),
        }
    }

    /// `basic.get` one message off a queue, polling until one is there or the deadline
    /// passes — which is how a client waits for a message another session, or the
    /// broker's own clock, owes the queue.
    async fn get(&mut self, name: &str) -> Option<Received> {
        let deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            let answer = self
                .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
                    queue: ShortString::from(name),
                    no_ack: false,
                })))
                .await;
            match answer {
                AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetOk(ok))) => {
                    let (properties, body) = self.read_content().await;
                    return Some(Received {
                        exchange: ok.exchange.as_str().to_string(),
                        routing_key: ok.routing_key.as_str().to_string(),
                        delivery_tag: ok.delivery_tag,
                        properties,
                        body,
                    });
                }
                AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetEmpty(_))) => {
                    if tokio::time::Instant::now() >= deadline {
                        return None;
                    }
                    tokio::time::sleep(POLL_EVERY).await;
                }
                other => panic!(
                    "case {:?}: expected basic.get-ok or basic.get-empty for {name:?}, got \
                     {other:?}",
                    self.case
                ),
            }
        }
    }

    /// Start consuming a queue, answering the consumer tag the broker assigned.
    async fn consume(&mut self, name: &str) -> String {
        let answer = self
            .call(AMQPClass::Basic(basic::AMQPMethod::Consume(
                basic::Consume {
                    queue: ShortString::from(name),
                    consumer_tag: ShortString::from(""),
                    no_local: false,
                    no_ack: false,
                    exclusive: false,
                    nowait: false,
                    arguments: FieldTable::default(),
                },
            )))
            .await;
        match answer {
            AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::ConsumeOk(ok))) => {
                ok.consumer_tag.as_str().to_string()
            }
            other => panic!(
                "case {:?}: expected basic.consume-ok for {name:?}, got {other:?}",
                self.case
            ),
        }
    }

    /// Read the next delivery a consumer is handed.
    async fn next_delivery(&mut self) -> Received {
        let frame = self.read_frame().await;
        match frame {
            AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::Deliver(deliver))) => {
                let (properties, body) = self.read_content().await;
                Received {
                    exchange: deliver.exchange.as_str().to_string(),
                    routing_key: deliver.routing_key.as_str().to_string(),
                    delivery_tag: deliver.delivery_tag,
                    properties,
                    body,
                }
            }
            other => panic!("case {:?}: expected basic.deliver, got {other:?}", self.case),
        }
    }

    /// Acknowledge one delivery.
    async fn ack(&mut self, delivery_tag: u64) {
        self.send(AMQPFrame::Method(
            1,
            AMQPClass::Basic(basic::AMQPMethod::Ack(basic::Ack {
                delivery_tag,
                multiple: false,
            })),
        ))
        .await;
    }

    /// Reject one delivery without requeueing it, with `basic.nack` or `basic.reject`.
    async fn reject(&mut self, delivery_tag: u64, with_nack: bool) {
        let method = if with_nack {
            AMQPClass::Basic(basic::AMQPMethod::Nack(basic::Nack {
                delivery_tag,
                multiple: false,
                requeue: false,
            }))
        } else {
            AMQPClass::Basic(basic::AMQPMethod::Reject(basic::Reject {
                delivery_tag,
                requeue: false,
            }))
        };
        self.send(AMQPFrame::Method(1, method)).await;
    }

    /// The content of a delivery: its header frame and however many body frames carry
    /// the bytes the header announced.
    async fn read_content(&mut self) -> (AMQPProperties, Vec<u8>) {
        let (properties, size) = match self.read_frame().await {
            AMQPFrame::Header(_, header) => (
                header.properties,
                usize::try_from(header.body_size).unwrap_or_default(),
            ),
            other => panic!(
                "case {:?}: expected a content header, got {other:?}",
                self.case
            ),
        };
        let mut body = Vec::new();
        while body.len() < size {
            match self.read_frame().await {
                AMQPFrame::Body(_, bytes) => body.extend_from_slice(&bytes),
                other => panic!(
                    "case {:?}: expected a content body, got {other:?}",
                    self.case
                ),
            }
        }
        (properties, body)
    }

    /// The next whole frame off the session, skipping heartbeats as a client does.
    async fn read_frame(&mut self) -> AMQPFrame {
        let deadline = tokio::time::Instant::now() + FRAME_WAIT;
        loop {
            let parsed = match parse_frame(self.buffered.as_slice()) {
                Ok((rest, frame)) => Some((self.buffered.len() - rest.len(), frame)),
                Err(error) => {
                    assert!(
                        error.is_incomplete(),
                        "case {:?}: the broker must answer frames, got {error:?}",
                        self.case
                    );
                    None
                }
            };
            if let Some((consumed, frame)) = parsed {
                self.buffered.drain(..consumed);
                if matches!(frame, AMQPFrame::Heartbeat) {
                    continue;
                }
                return frame;
            }
            let mut chunk = vec![0u8; 16 * 1024];
            let read = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk))
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "case {:?}: the broker must answer the frame it owes",
                        self.case
                    )
                })
                .unwrap_or_else(|error| {
                    panic!("case {:?}: the session must be readable, got {error}", self.case)
                });
            assert!(
                read > 0,
                "case {:?}: the broker closed the session with a frame still owed",
                self.case
            );
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }

    /// Close the connection the way a client does, so the broker gives back everything
    /// the session held.
    async fn close(mut self) {
        self.send(AMQPFrame::Method(
            0,
            AMQPClass::Connection(connection::AMQPMethod::Close(connection::Close {
                reply_code: 200,
                reply_text: ShortString::from("Normal shutdown"),
                class_id: 0,
                method_id: 0,
            })),
        ))
        .await;
        let _ = self.read_frame().await;
    }
}

/// The verifier a client of the emulator's OWN socket uses: the certificate is the one
/// the core generated for its listeners, self-signed and naming loopback, and this
/// client is the emulator's own test connecting to a port the same process opened — so
/// there is no authority to check it against.
///
/// Shared with the ActiveMQ suite, whose client dials the same core-owned TLS.
#[derive(Debug)]
pub(crate) struct AcceptTheEmulatorsCertificate(pub(crate) Arc<rustls::crypto::CryptoProvider>);

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

/// Create one RabbitMQ broker with one user and poll `DescribeBroker` until the clock
/// has made it RUNNING, then answer `(broker_id, host, port)` for the `amqps://`
/// endpoint it publishes.
async fn running_rabbitmq_broker(mq: &Mq, name: &str) -> (String, String, u16) {
    let mut body = create_body(name);
    body["engineType"] = json!("RABBITMQ");
    body["engineVersion"] = json!("3.13");
    body["deploymentMode"] = json!("SINGLE_INSTANCE");
    body["hostInstanceType"] = json!("mq.m5.large");
    body["publiclyAccessible"] = json!(false);
    body["users"] = json!([{ "username": USERNAME, "password": PASSWORD }]);
    let (status, created) = mq.create_broker(body).await;
    assert_eq!(
        status, 200,
        "case {name:?}: expected 200 from create, got {status} {created:?}"
    );
    let broker_id = string_at(&created, "/brokerId");
    let described = poll_until_running(mq, &broker_id).await;
    let instances = described
        .pointer("/brokerInstances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        instances.len(),
        1,
        "case {name:?}: a RUNNING RabbitMQ broker publishes the one instance allocated to it, \
         got {described:?}"
    );
    let instance = &instances[0];
    let console_url = string_at(instance, "/consoleURL");
    assert!(
        console_url.starts_with("https://"),
        "case {name:?}: a RUNNING RabbitMQ broker publishes its management console URL, got \
         {console_url:?}"
    );
    let endpoints: Vec<String> = instance
        .pointer("/endpoints")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    let amqps: Vec<&String> = endpoints
        .iter()
        .filter(|endpoint| endpoint.starts_with("amqps://"))
        .collect();
    assert_eq!(
        amqps.len(),
        1,
        "case {name:?}: a RUNNING RabbitMQ broker publishes one amqps:// endpoint, got \
         {endpoints:?}"
    );
    let authority = amqps[0].trim_start_matches("amqps://").to_string();
    let (host, port) = authority.rsplit_once(':').unwrap_or_else(|| {
        panic!("case {name:?}: expected an amqps endpoint naming a port, got {authority:?}")
    });
    let port: u16 = port.parse().unwrap_or_else(|_| {
        panic!("case {name:?}: expected an amqps endpoint naming a port, got {authority:?}")
    });
    (broker_id.clone(), host.to_string(), port)
}

/// Poll `DescribeBroker` until the broker reports RUNNING, which is the state its
/// instance is published in. Waits for the CONDITION, never for a fixed time.
async fn poll_until_running(mq: &Mq, broker_id: &str) -> Value {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        let (status, described) = mq.describe_broker(broker_id).await;
        assert_eq!(
            status, 200,
            "describe {broker_id}: expected 200, got {status} {described:?}"
        );
        if described.pointer("/brokerState").and_then(Value::as_str) == Some("RUNNING") {
            return described;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "describe {broker_id}: the broker never reached RUNNING, last answer {described:?}"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// A field table from `(name, value)` pairs, as a declaration's arguments.
fn arguments(entries: &[(&str, AMQPValue)]) -> FieldTable {
    let mut table = FieldTable::default();
    for (name, value) in entries {
        table.insert(ShortString::from(*name), value.clone());
    }
    table
}

/// RULE: the `amqps://` endpoint a RUNNING RabbitMQ broker publishes is an address this
/// emulator answers AMQP 0-9-1 on, and the session opened on it authenticates as one of
/// the broker's OWN users.
///
/// The rows span who dials: the user the broker was created with, that user with the
/// wrong password, and a user the broker does not have.
#[tokio::test]
async fn the_published_amqps_endpoint_answers_amqp_to_the_brokers_users() {
    let mq = Mq::new("us-east-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-auth").await;
    assert_eq!(
        host, LOOPBACK,
        "the published endpoint must be an address on this machine, got {host:?}"
    );

    // The broker's own user opens a session, and it serves the whole handshake.
    let session = Session::open("the broker's user", &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| {
            panic!("case \"the broker's user\": expected a session, got a close with code {code}")
        });
    session.close().await;

    // Credentials that are not a user of this broker are refused with the reply code a
    // client library reads as an authentication failure.
    let refused: &[(&str, &str, &str)] = &[
        ("the right user with the wrong password", USERNAME, "not-the-password"),
        ("a user the broker does not have", "nobody", PASSWORD),
    ];
    for (case, username, password) in refused {
        let outcome = Session::open(case, &host, port, username, password).await;
        let code = match outcome {
            Err(code) => code,
            Ok(_) => panic!("case {case:?}: expected the connection to be refused, got a session"),
        };
        assert_eq!(
            code, 403,
            "case {case:?}: expected the 403 ACCESS_REFUSED a refused login is closed with, got \
             {code}"
        );
    }
}

/// One row of the publish-and-consume table: what it publishes and how.
struct DeliveryCase {
    case: &'static str,
    /// The exchange the messages are published to, and its type; the default exchange
    /// is published to by the empty name and needs no declaration.
    exchange: &'static str,
    exchange_kind: &'static str,
    /// Whether the queue is declared durable.
    durable: bool,
    /// `delivery_mode` 2 is a persistent message, 1 a transient one.
    delivery_mode: u8,
    /// The bodies published, in order.
    bodies: &'static [&'static str],
}

/// The rows: they differ along what the rule generalises over — the exchange a message
/// is routed through, whether the queue is durable, whether the message is persistent,
/// how many messages are published and how large they are. The recorded case (a durable
/// queue, persistent messages, publisher confirms, read back on another connection) is
/// one row of it.
const DELIVERY_CASES: &[DeliveryCase] = &[
    DeliveryCase {
        case: "a durable queue of persistent messages on the default exchange",
        exchange: "",
        exchange_kind: "direct",
        durable: true,
        delivery_mode: 2,
        bodies: &["first", "second", "third"],
    },
    DeliveryCase {
        case: "one persistent message through a direct exchange",
        exchange: "plane.direct",
        exchange_kind: "direct",
        durable: true,
        delivery_mode: 2,
        bodies: &["only"],
    },
    DeliveryCase {
        case: "transient messages on a queue that is not durable",
        exchange: "",
        exchange_kind: "direct",
        durable: false,
        delivery_mode: 1,
        bodies: &["a", "b"],
    },
    DeliveryCase {
        case: "a fanout exchange and a body that spans more than one frame",
        exchange: "plane.fanout",
        exchange_kind: "fanout",
        durable: true,
        delivery_mode: 2,
        bodies: &["x", "payload"],
    },
];

/// RULE: a message published to a queue waits there, in publication order, until a
/// consumer acknowledges it — so a session that opens the connection AFTER the
/// publisher closed its own reads every message the publisher was confirmed for, in the
/// order it published them.
#[tokio::test]
async fn a_queue_holds_confirmed_messages_across_connections_in_order() {
    let mq = Mq::new("us-west-2");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-order").await;

    for (index, row) in DELIVERY_CASES.iter().enumerate() {
        let case = row.case;
        let queue = format!("holds-{index}");
        // The publisher's connection: declare, confirm-select, publish, read every
        // confirmation, then leave.
        let mut publisher = Session::open(case, &host, port, USERNAME, PASSWORD)
            .await
            .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
        publisher
            .declare_queue(&queue, row.durable, FieldTable::default())
            .await;
        let routing_key = if row.exchange.is_empty() {
            queue.clone()
        } else {
            publisher
                .declare_exchange(row.exchange, row.exchange_kind)
                .await;
            publisher.bind_queue(&queue, row.exchange, "holds").await;
            "holds".to_string()
        };
        publisher.select_confirms().await;
        for (sequence, body) in row.bodies.iter().enumerate() {
            // A body that spans more than one frame on the way back out: the longest row
            // is published as-is, and the server frames it by what the handshake
            // negotiated.
            let properties = AMQPProperties::default()
                .with_delivery_mode(row.delivery_mode)
                .with_content_type(ShortString::from("text/plain"));
            publisher
                .publish(row.exchange, &routing_key, properties, body.as_bytes())
                .await;
            publisher
                .expect_confirm(u64::try_from(sequence).unwrap_or_default() + 1)
                .await;
        }
        publisher.close().await;

        // A SECOND connection, opened after the first one is gone, reads what the queue
        // holds — in publication order.
        let mut consumer = Session::open(case, &host, port, USERNAME, PASSWORD)
            .await
            .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
        let declared = consumer
            .declare_queue(&queue, row.durable, FieldTable::default())
            .await;
        assert_eq!(
            declared, queue,
            "case {case:?}: expected the declaration to report the queue's name, got \
             {declared:?}"
        );
        consumer.consume(&queue).await;
        for expected in row.bodies {
            let delivery = consumer.next_delivery().await;
            assert_eq!(
                delivery.text(),
                *expected,
                "case {case:?}: expected the messages in publication order, got {:?}",
                delivery.text()
            );
            assert_eq!(
                delivery.exchange, row.exchange,
                "case {case:?}: expected the exchange the message was published to, got {:?}",
                delivery.exchange
            );
            assert_eq!(
                delivery.properties.delivery_mode(),
                &Some(row.delivery_mode),
                "case {case:?}: expected the delivery mode the publisher set, got {:?}",
                delivery.properties
            );
            consumer.ack(delivery.delivery_tag).await;
        }
        // Everything the queue held has been acknowledged, so a read finds nothing left.
        let left = consumer
            .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
                queue: ShortString::from(queue.as_str()),
                no_ack: true,
            })))
            .await;
        assert!(
            matches!(
                left,
                AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetEmpty(_)))
            ),
            "case {case:?}: expected the queue to be empty once every message was acknowledged, \
             got {left:?}"
        );
        consumer.close().await;
    }
}

/// One row of the dead-letter table: how the message dies, and the reason its `x-death`
/// must carry.
struct DeathCase {
    case: &'static str,
    /// The queue arguments beyond its dead-letter exchange: a time to live, a length
    /// bound, or neither.
    ttl_millis: Option<i32>,
    max_length: Option<i32>,
    /// The per-message expiration, in milliseconds, when the row sets one.
    expiration: Option<&'static str>,
    /// How many messages are published: a length bound is reached by the second one.
    published: usize,
    /// Whether the row rejects the message it consumed, and with which method.
    reject_with_nack: Option<bool>,
    reason: &'static str,
}

/// The rows: every way a message leaves a queue for its dead-letter exchange — rejected
/// without requeueing (by `basic.nack` and by `basic.reject`), expired by the queue's own
/// time to live, expired by the publisher's own expiration, and dropped because the queue
/// was full. The recorded case (nack without requeue, and an expiry) is two rows of it.
const DEATH_CASES: &[DeathCase] = &[
    DeathCase {
        case: "rejected with basic.nack and not requeued",
        ttl_millis: None,
        max_length: None,
        expiration: None,
        published: 1,
        reject_with_nack: Some(true),
        reason: "rejected",
    },
    DeathCase {
        case: "rejected with basic.reject and not requeued",
        ttl_millis: None,
        max_length: None,
        expiration: None,
        published: 1,
        reject_with_nack: Some(false),
        reason: "rejected",
    },
    DeathCase {
        case: "expired by the queue's own message time to live",
        ttl_millis: Some(1),
        max_length: None,
        expiration: None,
        published: 1,
        reject_with_nack: None,
        reason: "expired",
    },
    DeathCase {
        case: "expired by the expiration the publisher set",
        ttl_millis: None,
        max_length: None,
        expiration: Some("1"),
        published: 1,
        reject_with_nack: None,
        reason: "expired",
    },
    DeathCase {
        case: "dropped because the queue was already full",
        ttl_millis: None,
        max_length: Some(1),
        expiration: None,
        published: 2,
        reject_with_nack: None,
        reason: "maxlen",
    },
];

/// RULE: a message that leaves its queue without being acknowledged — rejected without
/// requeueing, outliving its expiration, or pushed out by the queue's length bound — is
/// republished to that queue's dead-letter exchange carrying the reason it died and the
/// queue it died in.
#[tokio::test]
async fn a_message_that_dies_reaches_its_dead_letter_exchange_with_its_reason() {
    let mq = Mq::new("eu-west-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-death").await;

    for (index, row) in DEATH_CASES.iter().enumerate() {
        let case = row.case;
        let dead_letters = format!("deaths-{index}");
        let dead_queue = format!("dead-{index}");
        let queue = format!("alive-{index}");
        let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
            .await
            .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
        // The dead-letter exchange and the queue bound to it, which is where a dead
        // message is republished.
        session.declare_exchange(&dead_letters, "fanout").await;
        session
            .declare_queue(&dead_queue, true, FieldTable::default())
            .await;
        session.bind_queue(&dead_queue, &dead_letters, "").await;
        // The queue the messages are published to, declared with its dead-letter
        // exchange and whatever bound this row tests.
        let mut declared: Vec<(&str, AMQPValue)> = vec![(
            "x-dead-letter-exchange",
            AMQPValue::LongString(LongString::from(dead_letters.as_str())),
        )];
        if let Some(ttl) = row.ttl_millis {
            declared.push(("x-message-ttl", AMQPValue::LongInt(ttl)));
        }
        if let Some(max_length) = row.max_length {
            declared.push(("x-max-length", AMQPValue::LongInt(max_length)));
        }
        session
            .declare_queue(&queue, true, arguments(&declared))
            .await;

        for sequence in 0..row.published {
            let mut properties = AMQPProperties::default().with_delivery_mode(2);
            if let Some(expiration) = row.expiration {
                properties = properties.with_expiration(ShortString::from(expiration));
            }
            let body = format!("{case}#{sequence}");
            session
                .publish("", &queue, properties, body.as_bytes())
                .await;
        }

        if let Some(with_nack) = row.reject_with_nack {
            let delivery = session.get(&queue).await.unwrap_or_else(|| {
                panic!("case {case:?}: expected the published message to be in {queue:?}")
            });
            session.reject(delivery.delivery_tag, with_nack).await;
        }

        // The dead-letter queue is read by polling for the message the broker owes it,
        // which for an expiry is owed by the clock rather than by a call.
        let dead = session.get(&dead_queue).await.unwrap_or_else(|| {
            panic!("case {case:?}: expected the dead message to reach {dead_queue:?}")
        });
        let reason = dead.death_reason();
        assert_eq!(
            reason.as_deref(),
            Some(row.reason),
            "case {case:?}: expected x-death to carry the reason the message died, got {:?}",
            dead.properties
        );
        let died_in = dead.death_queue();
        assert_eq!(
            died_in.as_deref(),
            Some(queue.as_str()),
            "case {case:?}: expected x-death to name the queue the message died in, got {:?}",
            dead.properties
        );
        assert_eq!(
            dead.routing_key, queue,
            "case {case:?}: expected the dead message to keep the routing key it was published \
             under, got {:?}",
            dead.routing_key
        );
        assert!(
            dead.text().starts_with(case),
            "case {case:?}: expected the dead message to carry the body it was published with, \
             got {:?}",
            dead.text()
        );
        session.ack(dead.delivery_tag).await;
        session.close().await;
    }
}

/// One header a case publishes or binds on: `(name, value)`, both text.
type Header = (&'static str, &'static str);

/// One publish of a routing case: the routing key, the headers it carries, and whether
/// the message must reach the queue bound by the case's binding.
type RoutedPublish = (&'static str, &'static [Header], bool);

/// One row of the routing table: an exchange type, a binding, and which routing keys and
/// headers reach the queue bound by it.
struct RoutingCase {
    case: &'static str,
    kind: &'static str,
    binding_key: &'static str,
    /// The binding arguments, which is what a headers exchange matches on.
    binding_arguments: &'static [Header],
    publishes: &'static [RoutedPublish],
}

/// The rows span the exchange types AMQP 0-9-1 defines and, for each, a message that
/// reaches the bound queue and one that does not.
const ROUTING_CASES: &[RoutingCase] = &[
    RoutingCase {
        case: "a direct exchange routes by the whole routing key",
        kind: "direct",
        binding_key: "orders",
        binding_arguments: &[],
        publishes: &[("orders", &[], true), ("other", &[], false)],
    },
    RoutingCase {
        case: "a fanout exchange routes every message to every bound queue",
        kind: "fanout",
        binding_key: "ignored",
        binding_arguments: &[],
        publishes: &[("anything", &[], true), ("", &[], true)],
    },
    RoutingCase {
        case: "a topic exchange matches the pattern word by word",
        kind: "topic",
        binding_key: "orders.*.eu",
        binding_arguments: &[],
        publishes: &[
            ("orders.new.eu", &[], true),
            ("orders.new.us", &[], false),
            ("orders.eu", &[], false),
        ],
    },
    RoutingCase {
        case: "a topic exchange's hash stands for any number of words",
        kind: "topic",
        binding_key: "orders.#",
        binding_arguments: &[],
        publishes: &[
            ("orders", &[], true),
            ("orders.new.eu", &[], true),
            ("invoices.new", &[], false),
        ],
    },
    RoutingCase {
        case: "a headers exchange matches the binding's own arguments",
        kind: "headers",
        binding_key: "",
        binding_arguments: &[("x-match", "all"), ("region", "eu")],
        publishes: &[
            ("", &[("region", "eu")], true),
            ("", &[("region", "us")], false),
            ("", &[], false),
        ],
    },
];

/// RULE: which queues a published message reaches is decided by the type of the exchange
/// it was published to and by the bindings that exchange holds.
#[tokio::test]
async fn an_exchange_routes_a_message_by_its_type() {
    let mq = Mq::new("ap-southeast-2");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-routing").await;

    for (index, row) in ROUTING_CASES.iter().enumerate() {
        let case = row.case;
        let exchange = format!("routes-{index}");
        let queue = format!("routed-{index}");
        let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
            .await
            .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
        session.declare_exchange(&exchange, row.kind).await;
        session
            .declare_queue(&queue, true, FieldTable::default())
            .await;
        let binding: Vec<(&str, AMQPValue)> = row
            .binding_arguments
            .iter()
            .map(|(name, value)| {
                (
                    *name,
                    AMQPValue::LongString(LongString::from(*value)),
                )
            })
            .collect();
        session
            .bind_queue_with(&queue, &exchange, row.binding_key, arguments(&binding))
            .await;

        for (routing_key, headers, reaches) in row.publishes {
            let body = format!("{case}/{routing_key}");
            let table: Vec<(&str, AMQPValue)> = headers
                .iter()
                .map(|(name, value)| {
                    (
                        *name,
                        AMQPValue::LongString(LongString::from(*value)),
                    )
                })
                .collect();
            let properties = AMQPProperties::default()
                .with_delivery_mode(2)
                .with_headers(arguments(&table));
            session
                .publish(&exchange, routing_key, properties, body.as_bytes())
                .await;
            // A publish is answered by no method, so what is read back is the queue: the
            // message is there, or the queue is empty and the message went nowhere.
            let answer = session
                .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
                    queue: ShortString::from(queue.as_str()),
                    no_ack: true,
                })))
                .await;
            match answer {
                AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetOk(ok))) => {
                    let (_, read) = session.read_content().await;
                    assert!(
                        *reaches,
                        "case {case:?}: {routing_key:?} with headers {headers:?} must not reach \
                         the bound queue, got {:?}",
                        String::from_utf8_lossy(&read)
                    );
                    assert_eq!(
                        String::from_utf8_lossy(&read),
                        body,
                        "case {case:?}: expected the body published under {routing_key:?}, got \
                         {ok:?}"
                    );
                }
                AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetEmpty(_))) => {
                    assert!(
                        !*reaches,
                        "case {case:?}: {routing_key:?} with headers {headers:?} must reach the \
                         bound queue, got an empty queue"
                    );
                }
                other => panic!(
                    "case {case:?}: expected basic.get-ok or basic.get-empty, got {other:?}"
                ),
            }
        }
        session.close().await;
    }
}

/// RULE: a request a channel cannot carry out closes THAT channel with the AMQP reply
/// code for it, and leaves the connection open — so a client can open another channel
/// and carry on.
#[tokio::test]
async fn a_request_a_channel_cannot_serve_closes_the_channel_with_its_reply_code() {
    let mq = Mq::new("us-east-2");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-refusals").await;
    let case = "a queue that does not exist";
    let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));

    // Consuming a queue nothing declared: 404, on the channel.
    let answer = session
        .call(AMQPClass::Basic(basic::AMQPMethod::Consume(
            basic::Consume {
                queue: ShortString::from("never-declared"),
                consumer_tag: ShortString::from(""),
                no_local: false,
                no_ack: false,
                exclusive: false,
                nowait: false,
                arguments: FieldTable::default(),
            },
        )))
        .await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 404,
                "case {case:?}: expected the 404 a missing queue is refused with, got {close:?}"
            );
            assert!(
                close.reply_text.as_str().contains("never-declared"),
                "case {case:?}: expected the refusal to name the queue, got {:?}",
                close.reply_text.as_str()
            );
        }
        other => panic!("case {case:?}: expected channel.close, got {other:?}"),
    }
    session
        .send(AMQPFrame::Method(
            1,
            AMQPClass::Channel(channel::AMQPMethod::CloseOk(channel::CloseOk::default())),
        ))
        .await;

    // Declaring a queue whose arguments contradict the ones it was declared with: 406,
    // also on the channel, and on a channel of its own so the first refusal is not what
    // is being read.
    session
        .send(AMQPFrame::Method(
            1,
            AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open::default())),
        ))
        .await;
    let reopened = session.read_frame().await;
    assert!(
        matches!(
            reopened,
            AMQPFrame::Method(1, AMQPClass::Channel(channel::AMQPMethod::OpenOk(_)))
        ),
        "case {case:?}: the connection must stay open after a channel refusal, got {reopened:?}"
    );
    let case = "a queue redeclared with another durability";
    session
        .declare_queue("inequivalent", true, FieldTable::default())
        .await;
    let answer = session
        .call(AMQPClass::Queue(queue::AMQPMethod::Declare(
            queue::Declare {
                queue: ShortString::from("inequivalent"),
                passive: false,
                durable: false,
                exclusive: false,
                auto_delete: false,
                nowait: false,
                arguments: FieldTable::default(),
            },
        )))
        .await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 406,
                "case {case:?}: expected the 406 an inequivalent redeclaration is refused with, \
                 got {close:?}"
            );
        }
        other => panic!("case {case:?}: expected channel.close, got {other:?}"),
    }
}

/// RULE: a queue declared EXCLUSIVE belongs to the connection that declared it — no
/// other connection may use it, and it is deleted when that connection ends.
#[tokio::test]
async fn an_exclusive_queue_belongs_to_the_connection_that_declared_it() {
    let mq = Mq::new("eu-central-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-exclusive").await;
    let case = "the connection that declared it";
    let mut owner = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    let answer = owner
        .call(AMQPClass::Queue(queue::AMQPMethod::Declare(
            queue::Declare {
                queue: ShortString::from("exclusive-queue"),
                passive: false,
                durable: false,
                exclusive: true,
                auto_delete: false,
                nowait: false,
                arguments: FieldTable::default(),
            },
        )))
        .await;
    assert!(
        matches!(
            answer,
            AMQPFrame::Method(_, AMQPClass::Queue(queue::AMQPMethod::DeclareOk(_)))
        ),
        "case {case:?}: expected queue.declare-ok, got {answer:?}"
    );

    // Another connection asking for the same queue is refused it, on its channel.
    let case = "another connection";
    let mut other = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    let answer = other
        .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
            queue: ShortString::from("exclusive-queue"),
            no_ack: true,
        })))
        .await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 405,
                "case {case:?}: expected the 405 a locked queue is refused with, got {close:?}"
            );
        }
        other => panic!("case {case:?}: expected channel.close, got {other:?}"),
    }
    other.close().await;

    // Once the owning connection is gone, the queue is gone with it: a declaration that
    // only CHECKS for it is answered the 404 of a queue that does not exist.
    owner.close().await;
    let case = "after the owning connection ended";
    let mut later = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        let answer = later
            .call(AMQPClass::Queue(queue::AMQPMethod::Declare(
                queue::Declare {
                    queue: ShortString::from("exclusive-queue"),
                    passive: true,
                    durable: false,
                    exclusive: false,
                    auto_delete: false,
                    nowait: false,
                    arguments: FieldTable::default(),
                },
            )))
            .await;
        match answer {
            AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
                assert_eq!(
                    close.reply_code, 404,
                    "case {case:?}: expected the 404 of a queue that is gone, got {close:?}"
                );
                break;
            }
            AMQPFrame::Method(_, AMQPClass::Queue(queue::AMQPMethod::DeclareOk(_))) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case {case:?}: the exclusive queue must go with the connection that \
                     declared it"
                );
                tokio::time::sleep(POLL_EVERY).await;
                // A refused declaration closes the channel, so a retry needs a new one.
                later
                    .send(AMQPFrame::Method(
                        1,
                        AMQPClass::Channel(channel::AMQPMethod::Close(channel::Close {
                            reply_code: 200,
                            reply_text: ShortString::from("retry"),
                            class_id: 0,
                            method_id: 0,
                        })),
                    ))
                    .await;
                let _ = later.read_frame().await;
                later
                    .send(AMQPFrame::Method(
                        1,
                        AMQPClass::Channel(channel::AMQPMethod::Open(channel::Open::default())),
                    ))
                    .await;
                let _ = later.read_frame().await;
            }
            other => panic!("case {case:?}: expected a declaration answer, got {other:?}"),
        }
    }
}

/// RULE: a consumer tag names one consumer of a connection, so a tag already in use is
/// refused rather than registered twice.
#[tokio::test]
async fn a_consumer_tag_already_in_use_is_refused() {
    let mq = Mq::new("ca-central-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-tags").await;
    let case = "a consumer tag already in use";
    let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    session
        .declare_queue("tagged", true, FieldTable::default())
        .await;
    let consume = |tag: &str| {
        AMQPClass::Basic(basic::AMQPMethod::Consume(basic::Consume {
            queue: ShortString::from("tagged"),
            consumer_tag: ShortString::from(tag),
            no_local: false,
            no_ack: false,
            exclusive: false,
            nowait: false,
            arguments: FieldTable::default(),
        }))
    };
    let answer = session.call(consume("mine")).await;
    assert!(
        matches!(
            answer,
            AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::ConsumeOk(_)))
        ),
        "case {case:?}: expected the first consume to be accepted, got {answer:?}"
    );
    let answer = session.call(consume("mine")).await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 530,
                "case {case:?}: expected the 530 a reused consumer tag is refused with, got \
                 {close:?}"
            );
        }
        other => panic!("case {case:?}: expected connection.close, got {other:?}"),
    }
}

/// RULE: a refusal reaches the client with its reply code even when the text that names
/// what was refused is longer than a reply text can carry — the text is held to the
/// limit, the code is not.
#[tokio::test]
async fn a_refusal_naming_a_long_resource_still_carries_its_reply_code() {
    let mq = Mq::new("eu-north-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-long-text").await;
    // A queue name at the length an AMQP short string can carry, so the refusal that
    // names it is longer than one.
    let name = "q".repeat(200);
    let case = "a locked queue with a long name";
    let mut owner = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    let answer = owner
        .call(AMQPClass::Queue(queue::AMQPMethod::Declare(
            queue::Declare {
                queue: ShortString::from(name.as_str()),
                passive: false,
                durable: false,
                exclusive: true,
                auto_delete: false,
                nowait: false,
                arguments: FieldTable::default(),
            },
        )))
        .await;
    assert!(
        matches!(
            answer,
            AMQPFrame::Method(_, AMQPClass::Queue(queue::AMQPMethod::DeclareOk(_)))
        ),
        "case {case:?}: expected queue.declare-ok, got {answer:?}"
    );
    let mut other = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    let answer = other
        .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
            queue: ShortString::from(name.as_str()),
            no_ack: true,
        })))
        .await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 405,
                "case {case:?}: expected the 405 a locked queue is refused with, got {close:?}"
            );
            assert!(
                close.reply_text.as_str().len() <= 255,
                "case {case:?}: a reply text must fit the short string it is carried in, got {} \
                 bytes",
                close.reply_text.as_str().len()
            );
        }
        other => panic!("case {case:?}: expected channel.close, got {other:?}"),
    }
    owner.close().await;
}

/// RULE: what one channel holds UNSETTLED is bounded by the server, not by the prefetch
/// the client asked for: past that bound a read is refused with the engine's resource
/// error rather than the broker holding more and more on the client's behalf.
#[tokio::test]
async fn a_channel_that_never_acknowledges_is_refused_more_than_it_may_hold() {
    use service_com_amazonaws_mq_mq::state::message_broker::MAX_MESSAGE_BYTES;

    let mq = Mq::new("us-west-2");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-unacked").await;
    let case = "a channel that never acknowledges";
    let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    session
        .declare_queue("unacked", true, FieldTable::default())
        .await;
    // Bodies at the largest size the broker accepts, so the bound is reached in a few
    // messages rather than a thousand.
    let body = vec![b'u'; MAX_MESSAGE_BYTES];
    let mut refused = None;
    for sequence in 0..16u32 {
        session
            .publish(
                "",
                "unacked",
                AMQPProperties::default().with_delivery_mode(2),
                &body,
            )
            .await;
        // Read it without acknowledging it: the channel keeps holding it.
        let answer = session
            .call(AMQPClass::Basic(basic::AMQPMethod::Get(basic::Get {
                queue: ShortString::from("unacked"),
                no_ack: false,
            })))
            .await;
        match answer {
            AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::GetOk(_))) => {
                let _ = session.read_content().await;
            }
            AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Close(close))) => {
                refused = Some((sequence, close.reply_code));
                break;
            }
            other => panic!("case {case:?}: expected basic.get-ok or a refusal, got {other:?}"),
        }
    }
    let (at, code) = refused.unwrap_or_else(|| {
        panic!(
            "case {case:?}: a channel that never acknowledges must be refused more than it may \
             hold"
        )
    });
    assert_eq!(
        code, 506,
        "case {case:?}: expected the 506 the broker's own resource bound refuses with, got {code} \
         at read {at}"
    );
    assert!(
        at > 0,
        "case {case:?}: the bound must leave room for the deliveries a client does hold, got a \
         refusal at read {at}"
    );
}

/// RULE: a message body larger than the broker accepts is refused on the content HEADER
/// that declares its size, before any of it is held — the bound a client cannot talk the
/// broker past.
#[tokio::test]
async fn a_body_larger_than_the_broker_accepts_is_refused_on_its_declared_size() {
    use service_com_amazonaws_mq_mq::state::message_broker::MAX_MESSAGE_BYTES;

    let mq = Mq::new("us-west-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-too-large").await;
    let case = "a body larger than the broker accepts";
    let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    session
        .declare_queue("too-large", true, FieldTable::default())
        .await;
    session
        .send(AMQPFrame::Method(
            1,
            AMQPClass::Basic(basic::AMQPMethod::Publish(basic::Publish {
                exchange: ShortString::from(""),
                routing_key: ShortString::from("too-large"),
                mandatory: false,
                immediate: false,
            })),
        ))
        .await;
    // The header DECLARES a body past the bound; no body frame follows it, which is the
    // point: the refusal cannot depend on the bytes arriving.
    session
        .send(AMQPFrame::Header(
            1,
            AMQPContentHeader {
                class_id: 60,
                body_size: MAX_MESSAGE_BYTES as u64 + 1,
                properties: AMQPProperties::default(),
            },
        ))
        .await;
    let answer = session.read_frame().await;
    match answer {
        AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
            assert_eq!(
                close.reply_code, 311,
                "case {case:?}: expected the 311 an oversized body is refused with, got {close:?}"
            );
        }
        other => panic!("case {case:?}: expected channel.close, got {other:?}"),
    }
}

/// RULE: a publish whose `user_id` property names a user other than the one the
/// connection authenticated as is refused, so a publisher cannot claim to be another
/// user.
#[tokio::test]
async fn a_publish_claiming_another_user_is_refused() {
    let mq = Mq::new("sa-east-1");
    let (_, host, port) = running_rabbitmq_broker(&mq, "plane-user-id").await;
    let rows: &[(&str, &str, bool)] = &[
        ("the user the connection authenticated as", USERNAME, true),
        ("another user", "someone-else", false),
    ];
    for (case, user_id, accepted) in rows {
        let mut session = Session::open(case, &host, port, USERNAME, PASSWORD)
            .await
            .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
        let queue = format!("user-id-{}", user_id);
        session
            .declare_queue(&queue, true, FieldTable::default())
            .await;
        session.select_confirms().await;
        let properties = AMQPProperties::default()
            .with_delivery_mode(2)
            .with_user_id(ShortString::from(*user_id));
        session
            .publish("", &queue, properties, b"claimed".as_slice())
            .await;
        let answer = session.read_frame().await;
        if *accepted {
            assert!(
                matches!(
                    answer,
                    AMQPFrame::Method(_, AMQPClass::Basic(basic::AMQPMethod::Ack(_)))
                ),
                "case {case:?}: expected the publish to be confirmed, got {answer:?}"
            );
        } else {
            match answer {
                AMQPFrame::Method(_, AMQPClass::Channel(channel::AMQPMethod::Close(close))) => {
                    assert_eq!(
                        close.reply_code, 406,
                        "case {case:?}: expected the 406 a claimed user is refused with, got \
                         {close:?}"
                    );
                    assert!(
                        close.reply_text.as_str().contains(user_id),
                        "case {case:?}: expected the refusal to name the claimed user, got {:?}",
                        close.reply_text.as_str()
                    );
                }
                other => panic!("case {case:?}: expected channel.close, got {other:?}"),
            }
        }
    }
}

/// RULE: the socket a broker publishes lives exactly as long as the broker — deleting
/// the broker closes the port, so the address it published answers nothing.
#[tokio::test]
async fn deleting_the_broker_closes_the_socket_it_published() {
    let mq = Mq::new("us-east-1");
    let (broker_id, host, port) = running_rabbitmq_broker(&mq, "plane-delete").await;
    let case = "before the delete";
    let session = Session::open(case, &host, port, USERNAME, PASSWORD)
        .await
        .unwrap_or_else(|code| panic!("case {case:?}: expected a session, got close {code}"));
    session.close().await;

    let (status, deleted) = mq
        .call("DELETE", &format!("/v1/brokers/{broker_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected 200 from the delete, got {status} {deleted:?}"
    );

    // Poll for the port to stop answering: the listener is torn down with the record,
    // and what is waited for is the condition, not a span of time.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        match TcpStream::connect(("127.0.0.1", port)).await {
            // The port this emulator published for the broker it just deleted.
            Err(_) => break,
            Ok(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case \"after the delete\": {host}:{port} must stop accepting connections \
                     once the broker is deleted"
                );
                tokio::time::sleep(POLL_EVERY).await;
            }
        }
    }
}
