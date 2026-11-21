// SPDX-License-Identifier: CC0-1.0

//! The DATA PLANE of an Amazon MQ for ActiveMQ broker: that the `stomp+ssl://` endpoint
//! the control plane publishes is an address a STOMP client of THIS emulator can open a
//! session on, and that what one session sends to a destination is what another is handed
//! back.
//!
//! The rules under test:
//!
//! * an endpoint a broker publishes for a wire protocol is an address the broker ANSWERS
//!   that protocol on, so a RUNNING ActiveMQ broker's one `BrokerInstance` carries a
//!   `stomp+ssl://` endpoint on a socket this emulator serves STOMP over TLS on, while
//!   every protocol it does not answer keeps the broker's own host and the port Amazon MQ
//!   publishes it on;
//! * a session on that endpoint authenticates as one of the broker's OWN users, and runs
//!   the version of the protocol the client and the server both serve;
//! * a `/queue/<name>` destination holds what was sent to it until a subscription takes
//!   it — whoever was connected at the time — in the order it was sent, with the body and
//!   the headers it was sent with; a `/topic/<name>` destination gives every subscription
//!   open at the time its own copy and keeps nothing for one that is not;
//! * a message a subscription does not acknowledge waits in its queue again, and one it
//!   acknowledges is gone;
//! * a frame that cannot be served is answered with an ERROR naming why.
//!
//! Every assertion goes over a real socket the core opened for the broker, dialled at the
//! address the control plane published, and reads only what a client can see. The frames
//! are written and parsed here as the wire carries them — command line, header lines, an
//! empty line, the body and a NUL — so nothing in this suite shares code with the server
//! it tests.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::rabbitmq_data_plane::AcceptTheEmulatorsCertificate;

/// The address the core binds its listeners on by default, which is the address a broker
/// of this emulator publishes and the only one these cases dial.
const LOOPBACK: &str = "127.0.0.1";

/// The user every broker in this suite is created with.
const USERNAME: &str = "mq-stomp-user";

/// That user's password.
const PASSWORD: &str = "Str0ng-Stomp-Pass";

/// The engine version every broker in this suite is created with. The broker reports the
/// full version it resolves to, which is what the session's `server` header names, so each
/// case reads that off the broker rather than assuming it.
const ENGINE_VERSION: &str = "5.18";

/// How long a step waits for the condition it needs before the test gives up. Generous:
/// what is being waited for is the emulator's own clock-owed transition, and a loaded host
/// must not fail a case.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long one frame read waits. A session that is owed a frame gets it immediately; this
/// only bounds a test that would otherwise hang.
const FRAME_WAIT: Duration = Duration::from_secs(30);

/// How often a poll loop looks again. Nothing is asserted about elapsed time.
const POLL_EVERY: Duration = Duration::from_millis(10);

/// The protocols an ActiveMQ broker publishes, with the scheme and the port Amazon MQ
/// publishes each on (the Developer Guide's ActiveMQ endpoint table), in the order they are
/// published in — and whether this emulator answers that protocol, which is the one case
/// the endpoint names a socket of the emulator's own rather than the broker's host.
const PUBLISHED_PROTOCOLS: &[(&str, u16, bool)] = &[
    ("ssl", 61_617, false),
    ("amqp+ssl", 5_671, false),
    ("stomp+ssl", 61_614, true),
    ("mqtt+ssl", 8_883, false),
    ("wss", 61_619, false),
];

/// A byte stream a STOMP session runs on: the TLS stream over the broker's socket.
type Stream = Box<dyn Transport>;

/// What a session needs of its connection.
trait Transport: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Transport for T {}

/// One frame the server sent, as a client reads it.
#[derive(Debug, Clone)]
struct ServerFrame {
    command: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ServerFrame {
    /// The value of a header the frame carries.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body as text, which is what most cases send.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// One STOMP session: the connection, what has been read off it and not parsed yet, and
/// whether the version it settled on escapes header values.
struct Stomp {
    case: String,
    stream: Stream,
    buffered: Vec<u8>,
    escaped: bool,
}

impl Stomp {
    /// Open a TLS connection to `host:port` — a `stomp+ssl://` endpoint is TLS from the
    /// first byte — without sending anything on it yet.
    ///
    /// The address dialled is the loopback one the control plane published for the broker:
    /// the core binds its listeners on the operator-chosen interface, loopback by default,
    /// so the port a broker publishes is a port on THIS machine.
    async fn dial(case: &str, host: &str, port: u16) -> Self {
        assert_eq!(
            host, LOOPBACK,
            "case {case:?}: the published endpoint must be this machine's loopback address, \
             got {host:?}"
        );
        // The target is this machine's loopback address, named here as the literal the
        // assertion above pinned the published endpoint to.
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
        let server_name =
            rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a name");
        let tls = connector.connect(server_name, tcp).await.unwrap_or_else(|error| {
            panic!("case {case:?}: the broker's socket must complete a TLS handshake, got {error}")
        });
        Self {
            case: case.to_string(),
            stream: Box::new(tls),
            buffered: Vec::new(),
            escaped: false,
        }
    }

    /// Dial the endpoint and send a CONNECT frame, answering the session and whatever the
    /// server answered it with — a CONNECTED for a session that was opened, an ERROR for
    /// one that was refused.
    async fn open(
        case: &str,
        host: &str,
        port: u16,
        accept_version: Option<&str>,
        login: &str,
        passcode: &str,
    ) -> (Self, ServerFrame) {
        let mut session = Self::dial(case, host, port).await;
        let mut headers: Vec<(&str, &str)> = vec![("host", LOOPBACK)];
        if let Some(accepted) = accept_version {
            headers.push(("accept-version", accepted));
        }
        headers.push(("login", login));
        headers.push(("passcode", passcode));
        session.write_frame("CONNECT", &headers, &[]).await;
        let answer = session.next_frame().await;
        // From STOMP 1.1 on, a header value carries the protocol's escapes; the CONNECT
        // and CONNECTED frames are exempt either way.
        session.escaped = answer.header("version").is_some_and(|version| version != "1.0");
        (session, answer)
    }

    /// Open a session as the broker's own user over the newest version of the protocol,
    /// failing the case when the server refuses it.
    async fn session(case: &str, host: &str, port: u16) -> Self {
        let (session, answer) = Self::open(case, host, port, Some("1.2"), USERNAME, PASSWORD).await;
        assert_eq!(
            answer.command, "CONNECTED",
            "case {case:?}: expected a CONNECTED frame, got {answer:?}"
        );
        session
    }

    /// Write one frame: its command, its headers and its body, ended by the NUL that ends
    /// every STOMP frame.
    async fn write_frame(&mut self, command: &str, headers: &[(&str, &str)], body: &[u8]) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(command.as_bytes());
        bytes.push(b'\n');
        for (name, value) in headers {
            bytes.extend_from_slice(escape(name, self.escaped).as_bytes());
            bytes.push(b':');
            bytes.extend_from_slice(escape(value, self.escaped).as_bytes());
            bytes.push(b'\n');
        }
        bytes.push(b'\n');
        bytes.extend_from_slice(body);
        bytes.push(0);
        self.stream.write_all(&bytes).await.unwrap_or_else(|error| {
            panic!("case {:?}: the session must be writable, got {error}", self.case)
        });
        self.stream.flush().await.unwrap_or_else(|error| {
            panic!("case {:?}: the session must flush, got {error}", self.case)
        });
    }

    /// Send a message to `destination`, with a body given by its byte length so that a
    /// body holding a NUL is carried whole.
    async fn send(&mut self, destination: &str, headers: &[(&str, &str)], body: &[u8]) {
        let length = body.len().to_string();
        let mut all: Vec<(&str, &str)> = vec![
            ("destination", destination),
            ("content-length", length.as_str()),
        ];
        all.extend_from_slice(headers);
        self.write_frame("SEND", &all, body).await;
    }

    /// Subscribe to `destination` with the acknowledgement mode given, waiting for the
    /// receipt that says the subscription is registered.
    async fn subscribe(&mut self, id: &str, destination: &str, ack: &str) {
        let receipt = format!("sub-{id}");
        self.write_frame(
            "SUBSCRIBE",
            &[
                ("id", id),
                ("destination", destination),
                ("ack", ack),
                ("receipt", receipt.as_str()),
            ],
            &[],
        )
        .await;
        let answer = self.next_frame().await;
        assert_eq!(
            answer.command, "RECEIPT",
            "case {:?}: expected a RECEIPT for the subscription {id}, got {answer:?}",
            self.case
        );
        assert_eq!(
            answer.header("receipt-id"),
            Some(receipt.as_str()),
            "case {:?}: expected the receipt {receipt:?}, got {answer:?}",
            self.case
        );
    }

    /// The next whole frame the server sent, polling until one arrives or the deadline
    /// passes. Waits for the FRAME, never for a fixed time.
    async fn next_frame(&mut self) -> ServerFrame {
        let deadline = tokio::time::Instant::now() + FRAME_WAIT;
        loop {
            if let Some(frame) = self.take_frame() {
                return frame;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "case {:?}: the broker must answer a frame; read {:?} so far",
                self.case,
                String::from_utf8_lossy(&self.buffered)
            );
            let mut chunk = vec![0u8; 16 * 1024];
            let read = match tokio::time::timeout(POLL_EVERY, self.stream.read(&mut chunk)).await {
                Err(_elapsed) => continue,
                Ok(Ok(read)) => read,
                Ok(Err(error)) => panic!(
                    "case {:?}: the session must stay readable, got {error}",
                    self.case
                ),
            };
            assert!(
                read > 0,
                "case {:?}: the broker closed the connection with {:?} read",
                self.case,
                String::from_utf8_lossy(&self.buffered)
            );
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }

    /// One frame off what has already been read, or `None` when it has not all arrived.
    fn take_frame(&mut self) -> Option<ServerFrame> {
        // A bare end of line between frames is a heart-beat, not a frame.
        while self.buffered.first() == Some(&b'\n') {
            self.buffered.remove(0);
        }
        let header_end = self
            .buffered
            .windows(2)
            .position(|pair| pair == *b"\n\n")?;
        let section = String::from_utf8_lossy(&self.buffered[..header_end]).to_string();
        let mut lines = section.split('\n');
        let command = lines.next().unwrap_or_default().to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| {
                (
                    unescape(name, self.escaped),
                    unescape(value, self.escaped),
                )
            })
            .collect();
        let body_at = header_end + 2;
        let rest = &self.buffered[body_at..];
        let length = match headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse::<usize>().ok())
        {
            Some(length) => length,
            None => rest.iter().position(|byte| *byte == 0)?,
        };
        if rest.len() < length + 1 {
            return None;
        }
        let body = rest[..length].to_vec();
        self.buffered.drain(..body_at + length + 1);
        Some(ServerFrame {
            command,
            headers,
            body,
        })
    }
}

/// A header name or value as the wire carries it, from STOMP 1.1 on.
fn escape(text: &str, escaped: bool) -> String {
    if !escaped {
        return text.to_string();
    }
    text.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace(':', "\\c")
}

/// A header name or value as it was meant.
fn unescape(text: &str, escaped: bool) -> String {
    if !escaped {
        return text.to_string();
    }
    let mut decoded = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('c') => decoded.push(':'),
            Some('\\') => decoded.push('\\'),
            other => panic!("the server wrote an undefined escape sequence {other:?}"),
        }
    }
    decoded
}

/// Create one ActiveMQ broker with one user and poll `DescribeBroker` until the clock has
/// made it RUNNING, then answer the endpoints it publishes, the console URL it reports and
/// the engine version it runs.
async fn running_activemq_broker(mq: &Mq, name: &str) -> (Vec<String>, String, String) {
    let mut body = create_body(name);
    body["engineType"] = json!("ACTIVEMQ");
    body["engineVersion"] = json!(ENGINE_VERSION);
    body["deploymentMode"] = json!("SINGLE_INSTANCE");
    body["hostInstanceType"] = json!("mq.t3.micro");
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
        "case {name:?}: a RUNNING SINGLE_INSTANCE broker reports one instance, got \
         {instances:?}"
    );
    let console_url = string_at(&instances[0], "/consoleURL");
    let endpoints: Vec<String> = instances[0]
        .pointer("/endpoints")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (endpoints, console_url, string_at(&described, "/engineVersion"))
}

/// Poll `DescribeBroker` until the broker reports RUNNING, which is the state its instance
/// is published in. Waits for the CONDITION, never for a fixed time.
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
            "describe {broker_id}: expected the broker to reach RUNNING, last saw {described:?}"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// The `host:port` a published endpoint names.
fn authority_of(case: &str, endpoint: &str) -> (String, u16) {
    let authority = endpoint
        .split_once("://")
        .map(|(_scheme, rest)| rest.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected a scheme in {endpoint:?}"));
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a port in {endpoint:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port in {endpoint:?}"));
    (host.to_string(), port)
}

/// The `host:port` of the one `stomp+ssl://` endpoint a broker publishes, and the engine
/// version that broker runs.
async fn running_broker_stomp_endpoint(mq: &Mq, name: &str) -> (String, u16, String) {
    let (endpoints, _console, engine_version) = running_activemq_broker(mq, name).await;
    let stomp: Vec<&String> = endpoints
        .iter()
        .filter(|endpoint| endpoint.starts_with("stomp+ssl://"))
        .collect();
    assert_eq!(
        stomp.len(),
        1,
        "case {name:?}: a RUNNING ActiveMQ broker publishes one stomp+ssl:// endpoint, got \
         {endpoints:?}"
    );
    let (host, port) = authority_of(name, stomp[0]);
    (host, port, engine_version)
}

/// RULE: an endpoint a broker publishes for a wire protocol is an address the broker
/// ANSWERS that protocol on — so the `stomp+ssl://` endpoint names a socket of this
/// emulator's own, on this machine, that completes a STOMP handshake, while every protocol
/// this emulator does not answer keeps the broker's own published host and the port Amazon
/// MQ publishes it on.
#[tokio::test]
async fn the_published_stomp_endpoint_names_a_socket_this_emulator_answers_stomp_on() {
    let mq = Mq::new("us-west-2");
    let (endpoints, console_url, _engine_version) =
        running_activemq_broker(&mq, "stomp-endpoint").await;
    assert_eq!(
        endpoints.len(),
        PUBLISHED_PROTOCOLS.len(),
        "expected one endpoint per protocol an ActiveMQ broker publishes, got {endpoints:?}"
    );
    let (console_host, _console_port) = authority_of("the console URL", &console_url);

    // The rows are every protocol the broker publishes: each keeps its scheme and its
    // place in the list, and only the one this emulator answers names a socket of its own.
    for (index, (scheme, port, served)) in PUBLISHED_PROTOCOLS.iter().enumerate() {
        let endpoint = &endpoints[index];
        assert!(
            endpoint.starts_with(&format!("{scheme}://")),
            "case {scheme:?}: expected the endpoint at {index} to carry the scheme {scheme:?}, \
             got {endpoint:?}"
        );
        let (host, published) = authority_of(scheme, endpoint);
        if *served {
            assert_eq!(
                host, LOOPBACK,
                "case {scheme:?}: a protocol this emulator answers publishes the address of \
                 its own socket, got {endpoint:?}"
            );
            // The port is the core's to assign, so what is asserted is that it answers.
            let session = Stomp::session(scheme, &host, published).await;
            drop(session);
        } else {
            assert_eq!(
                (host.as_str(), published),
                (console_host.as_str(), *port),
                "case {scheme:?}: a protocol this emulator does not answer keeps the broker's \
                 own host and the port Amazon MQ publishes it on, got {endpoint:?}"
            );
        }
    }
}

/// One client a case dials with: who it is, which versions it accepts, and the version the
/// session must settle on — `None` for a client the server refuses.
struct Dialler {
    case: &'static str,
    accept_version: Option<&'static str>,
    login: &'static str,
    passcode: &'static str,
    expected_version: Option<&'static str>,
}

/// RULE: a session on the published endpoint authenticates as one of the broker's OWN
/// users, and runs the newest version of the protocol both the client and the server
/// serve.
#[tokio::test]
async fn a_session_authenticates_as_a_broker_user_over_a_version_both_serve() {
    let mq = Mq::new("eu-west-1");
    let (host, port, engine_version) = running_broker_stomp_endpoint(&mq, "stomp-auth").await;

    // The rows span who dials and which versions they accept: the broker's own user over
    // each version it can ask for, a client that asks for a version this server does not
    // serve, that user with the wrong password, and a user the broker does not have.
    let rows: &[Dialler] = &[
        Dialler {
            case: "a client of one version",
            accept_version: Some("1.1"),
            login: USERNAME,
            passcode: PASSWORD,
            expected_version: Some("1.1"),
        },
        Dialler {
            case: "a client of the newest version",
            accept_version: Some("1.2"),
            login: USERNAME,
            passcode: PASSWORD,
            expected_version: Some("1.2"),
        },
        Dialler {
            case: "a client accepting several versions",
            accept_version: Some("1.0,1.1,1.2"),
            login: USERNAME,
            passcode: PASSWORD,
            expected_version: Some("1.2"),
        },
        Dialler {
            case: "a client with no accept-version",
            accept_version: None,
            login: USERNAME,
            passcode: PASSWORD,
            expected_version: Some("1.0"),
        },
        Dialler {
            case: "a client of a version beyond this server",
            accept_version: Some("1.3"),
            login: USERNAME,
            passcode: PASSWORD,
            expected_version: None,
        },
        Dialler {
            case: "the user with the wrong password",
            accept_version: Some("1.2"),
            login: USERNAME,
            passcode: "not-the-password",
            expected_version: None,
        },
        Dialler {
            case: "a user the broker does not have",
            accept_version: Some("1.2"),
            login: "nobody",
            passcode: PASSWORD,
            expected_version: None,
        },
    ];
    for row in rows {
        let case = row.case;
        let (session, answer) = Stomp::open(
            case,
            &host,
            port,
            row.accept_version,
            row.login,
            row.passcode,
        )
        .await;
        match row.expected_version {
            Some(version) => {
                assert_eq!(
                    answer.command, "CONNECTED",
                    "case {case:?}: expected a CONNECTED frame, got {answer:?}"
                );
                assert_eq!(
                    answer.header("version"),
                    Some(version),
                    "case {case:?}: expected the session to run STOMP {version}, got {answer:?}"
                );
                assert_eq!(
                    answer.header("server"),
                    Some(format!("ActiveMQ/{engine_version}").as_str()),
                    "case {case:?}: expected the broker's own engine and version, got {answer:?}"
                );
                assert!(
                    answer.header("session").is_some_and(|id| !id.is_empty()),
                    "case {case:?}: expected the connection to carry a session id, got {answer:?}"
                );
            }
            None => {
                assert_eq!(
                    answer.command, "ERROR",
                    "case {case:?}: expected the session to be refused, got {answer:?}"
                );
                assert!(
                    answer.header("message").is_some_and(|why| !why.is_empty()),
                    "case {case:?}: expected the refusal to say why, got {answer:?}"
                );
            }
        }
        drop(session);
    }
}

/// One message a case sends and expects back.
struct Payload {
    case: &'static str,
    body: &'static [u8],
    content_type: &'static str,
}

/// The bodies a case sends: text, nothing at all, bytes that are not text (which only a
/// `content-length` can carry), text beyond ASCII, and a body far larger than one read off
/// the socket.
const PAYLOADS: &[Payload] = &[
    Payload {
        case: "short text",
        body: b"hello from the control plane",
        content_type: "text/plain",
    },
    Payload {
        case: "an empty body",
        body: b"",
        content_type: "text/plain",
    },
    Payload {
        case: "bytes that are not text",
        body: &[0x00, 0x01, 0xff, 0x00, 0x7f],
        content_type: "application/octet-stream",
    },
    Payload {
        case: "text beyond ASCII",
        body: "héllo — ωμέγα".as_bytes(),
        content_type: "text/plain; charset=utf-8",
    },
    Payload {
        case: "a body far larger than one read off the socket",
        body: &[b'x'; 70_000],
        content_type: "application/octet-stream",
    },
];

/// RULE: a `/queue/<name>` destination HOLDS what was sent to it until a subscription
/// takes it — whoever was connected when it was sent — and hands it over with the body,
/// the content type and the headers it was sent with, in the order it was sent.
#[tokio::test]
async fn a_queue_destination_holds_what_was_sent_until_a_subscription_takes_it() {
    let mq = Mq::new("us-east-1");
    let (host, port, _engine_version) = running_broker_stomp_endpoint(&mq, "stomp-queue").await;

    for (index, payload) in PAYLOADS.iter().enumerate() {
        let case = payload.case;
        let destination = format!("/queue/holds-{index}");
        // The producer's session: it sends, is told the message is stored, and leaves —
        // nobody is subscribed while any of this happens.
        let mut producer = Stomp::session(case, &host, port).await;
        producer
            .send(
                &destination,
                &[
                    ("content-type", payload.content_type),
                    ("x-order", "first"),
                    ("receipt", "stored-first"),
                    ("persistent", "true"),
                ],
                payload.body,
            )
            .await;
        let receipt = producer.next_frame().await;
        assert_eq!(
            (receipt.command.as_str(), receipt.header("receipt-id")),
            ("RECEIPT", Some("stored-first")),
            "case {case:?}: expected the receipt of the first message, got {receipt:?}"
        );
        producer
            .send(
                &destination,
                &[("content-type", payload.content_type), ("x-order", "second")],
                b"the second message",
            )
            .await;
        producer.write_frame("DISCONNECT", &[], &[]).await;
        drop(producer);

        // A session opened AFTER the producer closed its own takes both messages, in the
        // order they were sent.
        let mut consumer = Stomp::session(case, &host, port).await;
        consumer.subscribe("s1", &destination, "auto").await;
        let first = consumer.next_frame().await;
        assert_eq!(
            first.command, "MESSAGE",
            "case {case:?}: expected a MESSAGE frame, got {first:?}"
        );
        assert_eq!(
            first.body, payload.body,
            "case {case:?}: expected the body that was sent, got {:?}",
            first.text()
        );
        assert_eq!(
            first.header("destination"),
            Some(destination.as_str()),
            "case {case:?}: expected the delivery to name its destination, got {first:?}"
        );
        assert_eq!(
            first.header("subscription"),
            Some("s1"),
            "case {case:?}: expected the delivery to name its subscription, got {first:?}"
        );
        assert_eq!(
            first.header("content-type"),
            Some(payload.content_type),
            "case {case:?}: expected the content type that was sent, got {first:?}"
        );
        assert_eq!(
            first.header("x-order"),
            Some("first"),
            "case {case:?}: expected the header that was sent, got {first:?}"
        );
        assert_eq!(
            first.header("persistent"),
            Some("true"),
            "case {case:?}: expected a persistent message to report it, got {first:?}"
        );
        assert!(
            first.header("message-id").is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the delivery to carry a message id, got {first:?}"
        );
        let second = consumer.next_frame().await;
        assert_eq!(
            (second.command.as_str(), second.header("x-order")),
            ("MESSAGE", Some("second")),
            "case {case:?}: expected the second message after the first, got {second:?}"
        );
        assert_eq!(
            second.text(),
            "the second message",
            "case {case:?}: expected the second body, got {:?}",
            second.text()
        );
        drop(consumer);
    }
}

/// RULE: a `/topic/<name>` destination gives EVERY subscription open at the time its own
/// copy of a message, and keeps nothing for a subscription that was not open yet.
#[tokio::test]
async fn a_topic_destination_gives_every_open_subscription_its_own_copy() {
    let mq = Mq::new("ap-northeast-1");
    let (host, port, _engine_version) = running_broker_stomp_endpoint(&mq, "stomp-topic").await;
    let destination = "/topic/prices";
    let case = "two subscriptions on one topic";

    // Sent while nobody is subscribed: a topic keeps nothing, so this message reaches
    // nobody — which the order of what the subscriptions DO receive is what shows.
    let mut producer = Stomp::session(case, &host, port).await;
    producer
        .send(destination, &[("x-order", "before")], b"nobody is listening")
        .await;
    let mut first = Stomp::session(case, &host, port).await;
    first.subscribe("a", destination, "auto").await;
    let mut second = Stomp::session(case, &host, port).await;
    second.subscribe("b", destination, "auto").await;
    producer
        .send(destination, &[("x-order", "after")], b"both are listening")
        .await;

    for (name, session) in [("a", &mut first), ("b", &mut second)] {
        let delivery = session.next_frame().await;
        assert_eq!(
            delivery.command, "MESSAGE",
            "case {case:?}: subscription {name} must be handed a copy, got {delivery:?}"
        );
        assert_eq!(
            delivery.header("x-order"),
            Some("after"),
            "case {case:?}: subscription {name} must be handed only what was sent while it \
             was open, got {delivery:?}"
        );
        assert_eq!(
            delivery.text(),
            "both are listening",
            "case {case:?}: subscription {name} expected the body that was sent, got {:?}",
            delivery.text()
        );
        assert_eq!(
            delivery.header("subscription"),
            Some(name),
            "case {case:?}: expected the delivery to name subscription {name}, got {delivery:?}"
        );
    }
    drop(producer);
}

/// RULE: a message a subscription does not acknowledge waits in its queue again and is
/// handed over once more, marked as a redelivery; one it acknowledges is gone.
///
/// The rows span how a client acknowledges: by the delivery's own `ack` id (STOMP 1.2) and
/// by its `message-id` (1.1), and both acknowledgement modes that leave it to the client.
#[tokio::test]
async fn an_unacknowledged_message_waits_in_its_queue_again() {
    let mq = Mq::new("eu-central-1");
    let (host, port, _engine_version) = running_broker_stomp_endpoint(&mq, "stomp-ack").await;

    let rows: &[(&str, &str, &str, bool)] = &[
        ("STOMP 1.2 acknowledging by ack id", "1.2", "client-individual", true),
        ("STOMP 1.1 acknowledging by message id", "1.1", "client-individual", false),
        ("STOMP 1.1 cumulative acknowledgement", "1.1", "client", false),
    ];
    for (index, (case, version, ack_mode, acks_by_ack_header)) in rows.iter().enumerate() {
        let destination = format!("/queue/acks-{index}");
        let (mut session, connected) =
            Stomp::open(case, &host, port, Some(version), USERNAME, PASSWORD).await;
        assert_eq!(
            connected.header("version"),
            Some(*version),
            "case {case:?}: expected a session on STOMP {version}, got {connected:?}"
        );
        session
            .send(&destination, &[("x-note", "once")], b"needs acknowledging")
            .await;
        session.subscribe("s", &destination, ack_mode).await;

        let delivery = session.next_frame().await;
        assert_eq!(
            delivery.command, "MESSAGE",
            "case {case:?}: expected a MESSAGE frame, got {delivery:?}"
        );
        assert_eq!(
            delivery.header("redelivered"),
            None,
            "case {case:?}: a first delivery is not a redelivery, got {delivery:?}"
        );
        let settle_id = if *acks_by_ack_header {
            delivery
                .header("ack")
                .unwrap_or_else(|| panic!("case {case:?}: STOMP 1.2 delivers an ack id"))
                .to_string()
        } else {
            delivery
                .header("message-id")
                .unwrap_or_else(|| panic!("case {case:?}: a delivery carries a message id"))
                .to_string()
        };
        let id_header = if *acks_by_ack_header { "id" } else { "message-id" };
        session
            .write_frame("NACK", &[(id_header, settle_id.as_str()), ("subscription", "s")], &[])
            .await;

        let again = session.next_frame().await;
        assert_eq!(
            again.command, "MESSAGE",
            "case {case:?}: a message nobody acknowledged is handed over again, got {again:?}"
        );
        assert_eq!(
            again.header("redelivered"),
            Some("true"),
            "case {case:?}: expected the redelivery to say so, got {again:?}"
        );
        assert_eq!(
            again.text(),
            "needs acknowledging",
            "case {case:?}: expected the same body on the redelivery, got {:?}",
            again.text()
        );
        let settle_id = if *acks_by_ack_header {
            again.header("ack").unwrap_or_default().to_string()
        } else {
            again.header("message-id").unwrap_or_default().to_string()
        };
        session
            .write_frame("ACK", &[(id_header, settle_id.as_str()), ("subscription", "s")], &[])
            .await;

        // The acknowledged message is GONE: a marker sent now is the next thing the
        // subscription is handed, which nothing about elapsed time has to be assumed for.
        session
            .send(&destination, &[("x-note", "marker")], b"the marker")
            .await;
        let marker = session.next_frame().await;
        assert_eq!(
            (marker.command.as_str(), marker.header("x-note")),
            ("MESSAGE", Some("marker")),
            "case {case:?}: an acknowledged message must not be handed over again, got \
             {marker:?}"
        );
        drop(session);
    }
}

/// One frame a case sends that cannot be served: its command and its headers.
struct RefusedFrame {
    case: &'static str,
    command: &'static str,
    headers: &'static [(&'static str, &'static str)],
}

/// RULE: a frame that cannot be served is answered with an ERROR naming why, and the
/// connection closes — a server never carries out part of what such a frame asked for.
#[tokio::test]
async fn a_frame_that_cannot_be_served_is_answered_with_an_error() {
    let mq = Mq::new("us-east-2");
    let (host, port, _engine_version) =
        running_broker_stomp_endpoint(&mq, "stomp-refusals").await;

    // The rows span what a client can get wrong: a destination that names no destination
    // kind this broker serves, no destination at all, a subscription with no id (which
    // STOMP 1.1 on requires), an acknowledgement of a delivery the session does not hold,
    // a command this broker does not serve, and a command that is no STOMP command.
    let rows: &[RefusedFrame] = &[
        RefusedFrame {
            case: "a destination with no kind",
            command: "SEND",
            headers: &[("destination", "orders")],
        },
        RefusedFrame {
            case: "a temporary destination",
            command: "SEND",
            headers: &[("destination", "/temp-queue/orders")],
        },
        RefusedFrame {
            case: "no destination at all",
            command: "SEND",
            headers: &[],
        },
        RefusedFrame {
            case: "a subscription with no id",
            command: "SUBSCRIBE",
            headers: &[("destination", "/queue/refused")],
        },
        RefusedFrame {
            case: "an unknown ack mode",
            command: "SUBSCRIBE",
            headers: &[
                ("id", "s"),
                ("destination", "/queue/refused"),
                ("ack", "sometimes"),
            ],
        },
        RefusedFrame {
            case: "an acknowledgement of nothing",
            command: "ACK",
            headers: &[("id", "ID:nothing-1")],
        },
        RefusedFrame {
            case: "a transaction",
            command: "BEGIN",
            headers: &[("transaction", "t1")],
        },
        RefusedFrame {
            case: "a command that is no STOMP command",
            command: "FROB",
            headers: &[],
        },
    ];
    for row in rows {
        let case = row.case;
        let mut session = Stomp::session(case, &host, port).await;
        session.write_frame(row.command, row.headers, b"").await;
        let answer = session.next_frame().await;
        assert_eq!(
            answer.command, "ERROR",
            "case {case:?}: expected an ERROR frame, got {answer:?}"
        );
        assert!(
            answer.header("message").is_some_and(|why| !why.is_empty()),
            "case {case:?}: expected the ERROR to say why, got {answer:?}"
        );
        drop(session);
    }
}
