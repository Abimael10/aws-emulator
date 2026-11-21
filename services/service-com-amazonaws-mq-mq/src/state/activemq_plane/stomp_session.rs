// SPDX-License-Identifier: CC0-1.0

//! The STOMP SERVER one connection to an ActiveMQ broker's endpoint is served by: the
//! connect handshake, the destinations a client sends to and subscribes on, and the
//! messages it is handed back.
//!
//! RULE: a client that opens a connection to an ActiveMQ broker's STOMP endpoint
//! authenticates as one of the broker's own users, and every frame it sends is answered
//! the frame STOMP answers it with — a CONNECTED for the handshake, a RECEIPT for a frame
//! that asked for one, a MESSAGE for every message one of its subscriptions is next in
//! line for, and an ERROR (on which the connection closes) for a frame that cannot be
//! served — out of the one store of destinations and queues that broker holds
//! ([`crate::state::message_broker`]).
//!
//! A destination is one of ActiveMQ's two kinds, told apart by the prefix the client
//! names it with: `/queue/<name>` is point to point — a message waits in the queue until
//! one subscription takes it, whether or not anybody was subscribed when it was sent — and
//! `/topic/<name>` is publish and subscribe, where every subscription open at the time
//! gets its own copy and a message sent to a topic nobody is on reaches nobody. Both ride
//! the message broker behind the socket: a queue destination IS a queue of it, and a topic
//! is a fan-out destination each subscription binds a queue of its own to.
//!
//! The core owns the socket, the TLS (a `stomp+ssl://` endpoint is TLS from the first
//! byte, which is what `listeners().tcp` serves), the connection cap and the idle timeout;
//! a session only ever reads and writes its own connection.
//!
//! # Bounds
//!
//! * a connection buffers at most [`MAX_BUFFERED_BYTES`] of unparsed bytes and one read
//!   takes at most [`READ_CHUNK`]; a frame past the frame format's own bounds
//!   ([`stomp_frames`]) is refused rather than buffered;
//! * one session registers at most [`MAX_SUBSCRIPTIONS`] subscriptions, and a destination
//!   it names is at most [`MAX_DESTINATION_BYTES`];
//! * one pump hands at most [`MAX_DELIVERIES_PER_PUMP`] messages to the client before the
//!   connection is read again, so a full queue cannot starve the session's own input;
//! * a session holds at most [`MAX_UNACKED`] unacknowledged deliveries and
//!   [`MAX_UNACKED_BYTES`] of their bodies — a message a client never acknowledges is out
//!   of its queue, so the broker's own caps no longer account for it and these do;
//! * everything a session stores lands in the message broker, which is bounded in its own
//!   module.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::amq_protocol::protocol::basic::AMQPProperties;
use ::amq_protocol::types::{AMQPValue, FieldTable, LongString, ShortString};
use ::emulator_server::{ListenerConn, NativeHandler};
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::stomp_frames::{self, Escaping, Frame, Parsed};
use crate::state::message_broker::{self, BrokerError, Message, QueueDeclaration};
use crate::state::BrokerSocket;

/// How many bytes one read off a connection takes at most.
const READ_CHUNK: usize = 16 * 1024;

/// How many bytes of a not-yet-whole frame one connection may buffer: one frame's command
/// and headers, one body, and the framing around them. A client that fills it is sending
/// something that is not a frame, and the connection is closed with an ERROR.
const MAX_BUFFERED_BYTES: usize =
    stomp_frames::MAX_HEADER_SECTION + stomp_frames::MAX_BODY_BYTES + 1024;

/// How long the session waits for the next frame before it looks for work of its own:
/// messages to hand to its subscriptions and expirations the clock has made due.
const TICK: Duration = Duration::from_millis(20);

/// How long the handshake waits for the client's CONNECT frame. The core's idle timeout
/// bounds the connection either way; this keeps a client that opened a socket and said
/// nothing from holding a session task.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(30);

/// How many messages one pump hands to the client before the connection is read again.
const MAX_DELIVERIES_PER_PUMP: usize = 64;

/// How many subscriptions one session may register.
const MAX_SUBSCRIPTIONS: usize = 64;

/// How many unacknowledged deliveries one session may hold.
const MAX_UNACKED: usize = 1_000;

/// How many bytes of message bodies one session's unacknowledged deliveries may hold.
const MAX_UNACKED_BYTES: usize = 8 * 1024 * 1024;

/// How long a destination a client names may be. A destination becomes the name of a queue
/// in the broker behind the session, whose names and whose refusals both carry it, so it is
/// held well inside the 255 bytes such a name takes.
const MAX_DESTINATION_BYTES: usize = 200;

/// How long a subscription id may be. An id is matched against, echoed on every delivery
/// and acknowledged by, so it is bounded like a header value a server keeps.
const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;

/// The prefix of a point-to-point destination: the queue a message waits in.
const QUEUE_PREFIX: &str = "/queue/";

/// The prefix of a publish-and-subscribe destination.
const TOPIC_PREFIX: &str = "/topic/";

/// The engine a broker of this wire runs, as the CONNECTED frame's `server` header names
/// it — followed by the broker's own engine version.
const SERVER_PRODUCT: &str = "ActiveMQ";

/// The heart-beat this server asks for and offers: none, which is what a server that owes
/// no beat and expects none answers a client's `heart-beat` with.
const NO_HEART_BEAT: &str = "0,0";

/// The headers a SEND frame's own protocol owns: they say where and how the message goes,
/// so they are not carried as the message's user headers.
const RESERVED_SEND_HEADERS: &[&str] = &[
    "destination",
    "content-length",
    "content-type",
    "receipt",
    "transaction",
    "persistent",
    "message-id",
];

/// The sequence every session's own id is drawn from, so two sessions on one broker never
/// mint the same message id.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// The STOMP server one broker's socket answers with.
pub struct StompWire {
    socket: BrokerSocket,
}

impl StompWire {
    pub fn new(socket: BrokerSocket) -> Self {
        Self { socket }
    }
}

impl NativeHandler for StompWire {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let socket = self.socket.clone();
        Box::pin(async move {
            let mut session = Session::new(socket);
            session.run(conn).await;
        })
    }
}

/// The protocol version a connection settled on, which decides how a header is written and
/// which header acknowledges a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Version {
    V10,
    V11,
    V12,
}

/// The versions this server serves, oldest first.
const SUPPORTED_VERSIONS: &[Version] = &[Version::V10, Version::V11, Version::V12];

impl Version {
    /// The version as `accept-version` and the CONNECTED frame's `version` name it.
    fn as_str(self) -> &'static str {
        match self {
            Self::V10 => "1.0",
            Self::V11 => "1.1",
            Self::V12 => "1.2",
        }
    }

    /// The version a client named, or `None` for one this server does not serve.
    fn parse(text: &str) -> Option<Self> {
        SUPPORTED_VERSIONS
            .iter()
            .copied()
            .find(|version| version.as_str() == text)
    }

    /// Whether a frame's headers carry the protocol's escapes. STOMP 1.0 has none.
    fn escaping(self) -> Escaping {
        match self {
            Self::V10 => Escaping::Literal,
            _ => Escaping::Escaped,
        }
    }

    /// Whether a delivery carries the `ack` header a client acknowledges it by, which
    /// STOMP 1.2 introduced.
    fn acks_by_ack_header(self) -> bool {
        self == Self::V12
    }
}

/// What a subscription asks of the server: when a message it was handed is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckMode {
    /// The message is settled as soon as it is sent.
    Auto,
    /// The client acknowledges, and an acknowledgement settles every earlier message of
    /// the subscription too.
    Client,
    /// The client acknowledges one message at a time.
    ClientIndividual,
}

impl AckMode {
    /// The mode a `ack` header names, or `None` for a value STOMP does not define.
    fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "client" => Some(Self::Client),
            "client-individual" => Some(Self::ClientIndividual),
            _ => None,
        }
    }
}

/// One destination a client named: which kind it is and the name inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    /// `/queue/<name>`: one queue, from which one subscription takes each message.
    Queue(String),
    /// `/topic/<name>`: every subscription open at the time gets its own copy.
    Topic(String),
}

impl Destination {
    /// The destination a client named, or `None` for text that names no destination this
    /// broker serves.
    fn parse(raw: &str) -> Option<Self> {
        if raw.len() > MAX_DESTINATION_BYTES {
            return None;
        }
        for (prefix, make) in [
            (QUEUE_PREFIX, Self::Queue as fn(String) -> Self),
            (TOPIC_PREFIX, Self::Topic as fn(String) -> Self),
        ] {
            if let Some(name) = raw.strip_prefix(prefix) {
                if name.is_empty() || name.contains('\0') {
                    return None;
                }
                return Some(make(raw.to_string()));
            }
        }
        None
    }

    /// The destination as the client named it, which is what every delivery from it
    /// reports.
    fn as_str(&self) -> &str {
        match self {
            Self::Queue(raw) | Self::Topic(raw) => raw,
        }
    }
}

/// One subscription a session registered with SUBSCRIBE.
#[derive(Debug, Clone)]
struct Subscription {
    /// The id the client gave it, which every delivery of it carries.
    id: String,
    /// The destination as the client named it.
    destination: Destination,
    /// The queue in the message broker this subscription reads: the destination itself for
    /// a queue, and this subscription's own fan-out queue for a topic.
    queue: String,
    ack: AckMode,
    /// Whether the queue above belongs to this subscription alone, so it goes when the
    /// subscription does.
    owns_queue: bool,
}

/// One message the client has been handed and has not acknowledged.
#[derive(Debug, Clone)]
struct Held {
    /// The id the delivery carried, which the client acknowledges it by.
    id: String,
    subscription: String,
    queue: String,
    message: Message,
}

/// Whether the session carries on reading or is done with the connection.
enum Flow {
    Continue,
    Close,
}

/// One connection's session: what it authenticated as, what it is subscribed to and what
/// it holds unacknowledged.
struct Session {
    socket: BrokerSocket,
    /// The version the handshake settled on.
    version: Version,
    /// The user this connection authenticated as.
    user: String,
    /// This session's own number, which its minted ids carry.
    number: u64,
    /// The sequence the next minted message id and fan-out queue name are built from.
    sequence: u64,
    subscriptions: Vec<Subscription>,
    /// The deliveries this session holds, in delivery order: a held message is OUT of its
    /// queue, so this is what the session itself keeps, bounded by [`MAX_UNACKED`] and
    /// [`MAX_UNACKED_BYTES`].
    held: Vec<Held>,
    /// The bytes of message bodies [`Self::held`] holds.
    held_bytes: usize,
}

impl Session {
    fn new(socket: BrokerSocket) -> Self {
        Self {
            socket,
            version: Version::V10,
            user: String::new(),
            number: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            sequence: 0,
            subscriptions: Vec::new(),
            held: Vec::new(),
            held_bytes: 0,
        }
    }

    /// Serve one connection: the handshake, then every frame until the client disconnects,
    /// a frame cannot be served, or the core severs it.
    async fn run(&mut self, conn: ListenerConn) {
        let mut io = Io::new(conn);
        if !self.connect(&mut io).await {
            return;
        }
        loop {
            if !self.pump(&mut io).await {
                break;
            }
            match io.next_frame(TICK, self.version.escaping()).await {
                Read::Frame(frame) => match self.dispatch(&mut io, *frame).await {
                    Flow::Continue => {}
                    Flow::Close => break,
                },
                Read::Idle => {}
                Read::Closed => break,
                Read::Malformed(why) => {
                    let _ = io.write_frame(&error_frame(&why, None), Escaping::Literal).await;
                    break;
                }
            }
        }
        self.release_everything();
    }

    /// The connect handshake: the client's CONNECT (or STOMP) frame, answered with
    /// CONNECTED once the version is settled and the credentials are a user of this
    /// broker. Answers `false` when the connection did not get through it, in which case
    /// the ERROR has already been written.
    ///
    /// The CONNECT and CONNECTED frames carry no escapes in any version of the protocol,
    /// which is why both are read and written literally.
    async fn connect(&mut self, io: &mut Io) -> bool {
        let frame = match io.next_frame(HANDSHAKE_WAIT, Escaping::Literal).await {
            Read::Frame(frame) => *frame,
            Read::Malformed(why) => {
                let _ = io.write_frame(&error_frame(&why, None), Escaping::Literal).await;
                return false;
            }
            Read::Idle | Read::Closed => return false,
        };
        if frame.command != "CONNECT" && frame.command != "STOMP" {
            let refusal = error_frame(
                &format!(
                    "Expected a CONNECT frame to open the connection, got {}",
                    frame.command
                ),
                None,
            );
            let _ = io.write_frame(&refusal, Escaping::Literal).await;
            return false;
        }
        let Some(version) = negotiate(frame.header("accept-version")) else {
            let offered: Vec<&str> = SUPPORTED_VERSIONS
                .iter()
                .map(|version| version.as_str())
                .collect();
            let refusal = error_frame(
                &format!("Supported protocol versions are {}", offered.join(" ")),
                None,
            )
            .with_header("version", &offered.join(","));
            let _ = io.write_frame(&refusal, Escaping::Literal).await;
            return false;
        };
        let login = frame.header("login").unwrap_or_default().to_string();
        let passcode = frame.header("passcode").unwrap_or_default();
        let Some(engine_version) = self.authenticate(&login, passcode) else {
            // The user name only: a password never reaches a log line.
            ::tracing::debug!(
                broker_id = self.socket.broker_id,
                user = login,
                "a login on this broker was refused"
            );
            let refusal = error_frame(
                &format!("User name [{login}] or password is invalid."),
                None,
            );
            let _ = io.write_frame(&refusal, Escaping::Literal).await;
            return false;
        };
        self.version = version;
        self.user = login;
        ::tracing::debug!(
            broker_id = self.socket.broker_id,
            user = self.user,
            version = version.as_str(),
            "a session was opened on this broker"
        );
        let connected = Frame::new("CONNECTED")
            .with_header("version", version.as_str())
            .with_header("session", &self.session_id())
            .with_header("server", &format!("{SERVER_PRODUCT}/{engine_version}"))
            .with_header("heart-beat", NO_HEART_BEAT);
        io.write_frame(&connected, Escaping::Literal).await
    }

    /// Whether `login` and `passcode` are a user of the broker this socket serves,
    /// answering that broker's engine version when they are.
    ///
    /// Read from the control plane's store on every connection, so the users the broker
    /// HAS are the users that can open a session on it.
    fn authenticate(&self, login: &str, passcode: &str) -> Option<String> {
        let store = self.socket.state.lock();
        let broker = store.brokers.get(&self.socket.broker_id)?;
        let user = broker.users.get(login)?;
        (user.password == passcode).then(|| broker.engine_version.clone())
    }

    /// The id this session reports in its CONNECTED frame, which also prefixes every
    /// message id it mints: the broker it is on and this session's own number.
    fn session_id(&self) -> String {
        format!("ID:{}-{}", self.socket.broker_id, self.number)
    }

    /// Hand every message its subscriptions are next in line for to the client, and
    /// materialise the expirations the clock has made due. Answers `false` when the
    /// connection could not be written.
    async fn pump(&mut self, io: &mut Io) -> bool {
        let now = SystemTime::now();
        self.socket.broker.lock().expire_due(now);
        let mut delivered = 0usize;
        for index in 0..self.subscriptions.len() {
            while let Some(subscription) = self.subscriptions.get(index).cloned() {
                // A delivery a client has to acknowledge is one the session holds, so the
                // bound on what it holds decides whether it may be handed another.
                if subscription.ack != AckMode::Auto && self.holds_its_limit() {
                    break;
                }
                if delivered >= MAX_DELIVERIES_PER_PUMP {
                    return true;
                }
                let taken = self
                    .socket
                    .broker
                    .lock()
                    .take_next(&subscription.queue, now);
                let Some(message) = taken else {
                    break;
                };
                delivered += 1;
                let id = self.message_id_of(&message);
                let frame = self.delivery_frame(&subscription, &message, &id);
                if subscription.ack != AckMode::Auto {
                    self.hold(Held {
                        id,
                        subscription: subscription.id.clone(),
                        queue: subscription.queue.clone(),
                        message,
                    });
                }
                if !io.write_frame(&frame, self.version.escaping()).await {
                    return false;
                }
            }
        }
        true
    }

    /// Answer one frame.
    async fn dispatch(&mut self, io: &mut Io, frame: Frame) -> Flow {
        match frame.command.as_str() {
            "SEND" => self.send(io, &frame).await,
            "SUBSCRIBE" => self.subscribe(io, &frame).await,
            "UNSUBSCRIBE" => self.unsubscribe(io, &frame).await,
            "ACK" => self.settle(io, &frame, true).await,
            "NACK" => self.settle(io, &frame, false).await,
            "DISCONNECT" => {
                self.receipt(io, &frame).await;
                Flow::Close
            }
            // A transaction spans several frames and settles them together, which this
            // broker does not serve: a client is told so rather than having its frames
            // applied one by one as if the transaction had been honoured.
            "BEGIN" | "COMMIT" | "ABORT" => {
                self.refuse(
                    io,
                    &frame,
                    &format!(
                        "This broker does not serve transactions, so the frame {} cannot be \
                         carried out",
                        frame.command
                    ),
                )
                .await
            }
            other => {
                self.refuse(io, &frame, &format!("Unknown command {other}"))
                    .await
            }
        }
    }

    /// A SEND frame: the message goes to the destination it names.
    async fn send(&mut self, io: &mut Io, frame: &Frame) -> Flow {
        if frame.header("transaction").is_some() {
            return self
                .refuse(
                    io,
                    frame,
                    "This broker does not serve transactions, so a SEND cannot name one",
                )
                .await;
        }
        let Some(destination) = frame.header("destination").and_then(Destination::parse) else {
            return self
                .refuse(io, frame, &destination_refusal(frame.header("destination")))
                .await;
        };
        let now = SystemTime::now();
        self.sequence = self.sequence.saturating_add(1);
        let properties = self.published_properties(frame, now);
        let body = frame.body.clone();
        let stored = {
            let mut broker = self.socket.broker.lock();
            match &destination {
                Destination::Queue(name) => broker
                    .declare_queue(&queue_declaration(name), now)
                    .and_then(|_| {
                        broker.publish(
                            message_broker::DEFAULT_EXCHANGE,
                            name,
                            properties,
                            body,
                            now,
                        )
                    }),
                Destination::Topic(name) => broker
                    .declare_exchange(name, FAN_OUT, false, true, false, false)
                    .and_then(|()| broker.publish(name, "", properties, body, now)),
            }
        };
        match stored {
            // A message sent to a topic nobody is subscribed to reaches nobody, which is
            // what a publish-and-subscribe destination means; the SEND itself succeeded.
            Ok(_reached) => {
                self.receipt(io, frame).await;
                Flow::Continue
            }
            Err(error) => self.refuse(io, frame, &refusal_text(error)).await,
        }
    }

    /// The properties a sent message is stored with: its content type, whether it is
    /// persistent, the id this server minted for it, the expiry the client set, and every
    /// header the client sent that the protocol does not own — so a subscription is handed
    /// back what was sent.
    fn published_properties(&self, frame: &Frame, now: SystemTime) -> AMQPProperties {
        let mut headers = FieldTable::default();
        for (name, value) in &frame.headers {
            if RESERVED_SEND_HEADERS.contains(&name.as_str()) {
                continue;
            }
            headers.insert(
                ShortString::from(name.as_str()),
                AMQPValue::LongString(LongString::from(value.as_str())),
            );
        }
        let mut properties = AMQPProperties::default()
            .with_message_id(ShortString::from(self.minted_message_id().as_str()));
        if !headers.inner().is_empty() {
            properties = properties.with_headers(headers);
        }
        if let Some(content_type) = frame.header("content-type") {
            properties = properties.with_content_type(ShortString::from(content_type));
        }
        if frame.header("persistent") == Some("true") {
            properties = properties.with_delivery_mode(PERSISTENT_DELIVERY_MODE);
        }
        if let Some(ttl) = remaining_life(frame.header("expires"), now) {
            properties = properties.with_expiration(ShortString::from(ttl.to_string().as_str()));
        }
        properties
    }

    /// The id this session mints for the message it is sending: the session's own id and
    /// the sequence of the message within it, which is what ActiveMQ's own ids are built
    /// from and what an acknowledgement names.
    fn minted_message_id(&self) -> String {
        format!("{}-{}", self.session_id(), self.sequence)
    }

    /// The id a delivery carries: the one minted when the message was sent, or one minted
    /// now for a message that carries none.
    fn message_id_of(&mut self, message: &Message) -> String {
        match message.properties.message_id() {
            Some(id) => id.to_string(),
            None => {
                self.sequence = self.sequence.saturating_add(1);
                self.minted_message_id()
            }
        }
    }

    /// A SUBSCRIBE frame: the session starts reading the destination it names.
    async fn subscribe(&mut self, io: &mut Io, frame: &Frame) -> Flow {
        let Some(destination) = frame.header("destination").and_then(Destination::parse) else {
            return self
                .refuse(io, frame, &destination_refusal(frame.header("destination")))
                .await;
        };
        // STOMP 1.1 on requires an id; a 1.0 client may leave it to the server, and every
        // delivery reports the one in force either way.
        let id = match frame.header("id") {
            Some(id) if id.len() > MAX_SUBSCRIPTION_ID_BYTES => {
                return self
                    .refuse(
                        io,
                        frame,
                        &format!(
                            "A subscription id may take at most {MAX_SUBSCRIPTION_ID_BYTES} \
                             bytes"
                        ),
                    )
                    .await
            }
            Some(id) => id.to_string(),
            None if self.version == Version::V10 => format!("sub-{}", self.subscriptions.len()),
            None => {
                return self
                    .refuse(io, frame, "A SUBSCRIBE frame must carry an id")
                    .await
            }
        };
        if self.subscriptions.iter().any(|held| held.id == id) {
            return self
                .refuse(
                    io,
                    frame,
                    &format!("This connection already has a subscription with id {id}"),
                )
                .await;
        }
        let ack = match frame.header("ack") {
            None => AckMode::Auto,
            Some(mode) => match AckMode::parse(mode) {
                Some(ack) => ack,
                None => {
                    return self
                        .refuse(io, frame, &format!("Unknown ack mode {mode}"))
                        .await
                }
            },
        };
        if self.subscriptions.len() >= MAX_SUBSCRIPTIONS {
            return self
                .refuse(
                    io,
                    frame,
                    &format!(
                        "This connection already holds its limit of {MAX_SUBSCRIPTIONS} \
                         subscriptions"
                    ),
                )
                .await;
        }
        let now = SystemTime::now();
        self.sequence = self.sequence.saturating_add(1);
        // A queue destination IS a queue, read by every subscription on it; a topic
        // destination fans out, so this subscription gets a queue of its OWN, bound to the
        // destination and gone when the subscription is.
        let (queue, owns_queue) = match &destination {
            Destination::Queue(name) => (name.clone(), false),
            Destination::Topic(name) => (
                format!("{name}#sub-{}-{}", self.number, self.sequence),
                true,
            ),
        };
        let registered = {
            let mut broker = self.socket.broker.lock();
            match &destination {
                Destination::Queue(name) => broker
                    .declare_queue(&queue_declaration(name), now)
                    .map(|_| ()),
                Destination::Topic(name) => broker
                    .declare_exchange(name, FAN_OUT, false, true, false, false)
                    .and_then(|()| {
                        broker
                            .declare_queue(&subscription_declaration(&queue), now)
                            .map(|_| ())
                    })
                    .and_then(|()| broker.bind_queue(&queue, name, "", &FieldTable::default())),
            }
            .and_then(|()| broker.add_consumer(&queue))
        };
        if let Err(error) = registered {
            return self.refuse(io, frame, &refusal_text(error)).await;
        }
        self.subscriptions.push(Subscription {
            id,
            destination,
            queue,
            ack,
            owns_queue,
        });
        self.receipt(io, frame).await;
        Flow::Continue
    }

    /// An UNSUBSCRIBE frame: the session stops reading that destination, and anything it
    /// held for the subscription goes back to the queue it came from.
    async fn unsubscribe(&mut self, io: &mut Io, frame: &Frame) -> Flow {
        let id = match frame.header("id") {
            Some(id) => id.to_string(),
            None => {
                return self
                    .refuse(io, frame, "An UNSUBSCRIBE frame must carry an id")
                    .await
            }
        };
        let Some(at) = self.subscriptions.iter().position(|held| held.id == id) else {
            return self
                .refuse(
                    io,
                    frame,
                    &format!("This connection has no subscription with id {id}"),
                )
                .await;
        };
        let subscription = self.subscriptions.remove(at);
        self.give_back(&subscription.id);
        self.close_subscription(&subscription);
        self.receipt(io, frame).await;
        Flow::Continue
    }

    /// An ACK or a NACK frame: the delivery it names is settled — gone when it was
    /// acknowledged, back at the head of its queue when it was not.
    ///
    /// A subscription in `client` mode settles every earlier delivery of that subscription
    /// with it, which is what STOMP's cumulative acknowledgement means; one in
    /// `client-individual` mode settles only the delivery named.
    async fn settle(&mut self, io: &mut Io, frame: &Frame, acknowledged: bool) -> Flow {
        if frame.header("transaction").is_some() {
            return self
                .refuse(
                    io,
                    frame,
                    &format!(
                        "This broker does not serve transactions, so {} cannot name one",
                        frame.command
                    ),
                )
                .await;
        }
        // STOMP 1.2 acknowledges by the delivery's own `ack` id, which this server mints
        // as the message id; 1.0 and 1.1 acknowledge by `message-id`.
        let Some(id) = frame
            .header("id")
            .or_else(|| frame.header("message-id"))
            .map(str::to_string)
        else {
            return self
                .refuse(
                    io,
                    frame,
                    &format!(
                        "A {} frame must carry the id of the message it settles",
                        frame.command
                    ),
                )
                .await;
        };
        let Some(at) = self.held.iter().position(|held| held.id == id) else {
            return self
                .refuse(
                    io,
                    frame,
                    &format!("This connection holds no delivery with id {id}"),
                )
                .await;
        };
        let subscription = self.held[at].subscription.clone();
        let cumulative = self
            .subscriptions
            .iter()
            .find(|held| held.id == subscription)
            .is_some_and(|held| held.ack == AckMode::Client);
        let settled: Vec<Held> = if cumulative {
            // Every delivery of this subscription up to and including the one named, which
            // is what STOMP's cumulative acknowledgement settles.
            let mut taken = Vec::new();
            let mut kept = Vec::new();
            for (position, delivery) in ::std::mem::take(&mut self.held).into_iter().enumerate() {
                if position <= at && delivery.subscription == subscription {
                    taken.push(delivery);
                } else {
                    kept.push(delivery);
                }
            }
            self.held = kept;
            taken
        } else {
            vec![self.held.remove(at)]
        };
        for delivery in settled {
            self.held_bytes = self.held_bytes.saturating_sub(delivery.message.body.len());
            if !acknowledged {
                self.socket
                    .broker
                    .lock()
                    .requeue(&delivery.queue, delivery.message);
            }
        }
        self.receipt(io, frame).await;
        Flow::Continue
    }

    /// The RECEIPT a frame that asked for one is answered with, once what it asked for is
    /// done.
    async fn receipt(&mut self, io: &mut Io, frame: &Frame) {
        if let Some(receipt) = frame.header("receipt") {
            let answer = Frame::new("RECEIPT").with_header("receipt-id", receipt);
            let _ = io.write_frame(&answer, self.version.escaping()).await;
        }
    }

    /// Refuse a frame: the ERROR it is answered with, after which the connection closes —
    /// which is what STOMP says a server does with an ERROR.
    async fn refuse(&mut self, io: &mut Io, frame: &Frame, message: &str) -> Flow {
        let refusal = error_frame(message, frame.header("receipt"));
        let _ = io.write_frame(&refusal, self.version.escaping()).await;
        Flow::Close
    }

    /// One delivery, as the MESSAGE frame carries it: where it came from, the ids that
    /// name it, and the message's own content type, headers and body.
    fn delivery_frame(&self, subscription: &Subscription, message: &Message, id: &str) -> Frame {
        let mut frame = Frame::new("MESSAGE")
            .with_header("destination", subscription.destination.as_str())
            .with_header("message-id", id)
            .with_header("subscription", &subscription.id);
        if self.version.acks_by_ack_header() && subscription.ack != AckMode::Auto {
            frame = frame.with_header("ack", id);
        }
        if let Some(content_type) = message.properties.content_type() {
            frame = frame.with_header("content-type", content_type.as_str());
        }
        if message.properties.delivery_mode() == &Some(PERSISTENT_DELIVERY_MODE) {
            frame = frame.with_header("persistent", "true");
        }
        if message.redelivered {
            frame = frame.with_header("redelivered", "true");
        }
        if let Some(headers) = message.properties.headers() {
            for (name, value) in headers.inner() {
                if let Some(text) = header_text(value) {
                    frame = frame.with_header(name.as_str(), &text);
                }
            }
        }
        frame
            .with_header("content-length", &message.body.len().to_string())
            .with_body(message.body.clone())
    }

    /// Whether this session already holds as much as it may.
    fn holds_its_limit(&self) -> bool {
        self.held.len() >= MAX_UNACKED || self.held_bytes >= MAX_UNACKED_BYTES
    }

    /// Hold one delivery until the client settles it.
    fn hold(&mut self, delivery: Held) {
        self.held_bytes = self.held_bytes.saturating_add(delivery.message.body.len());
        self.held.push(delivery);
    }

    /// Give back everything this session holds for one subscription, so a message it never
    /// acknowledged waits in its queue again rather than being lost with the
    /// subscription.
    fn give_back(&mut self, subscription: &str) {
        let (returned, kept): (Vec<Held>, Vec<Held>) = ::std::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| held.subscription == subscription);
        self.held = kept;
        self.held_bytes = self.held.iter().map(|held| held.message.body.len()).sum();
        let mut broker = self.socket.broker.lock();
        for delivery in returned {
            broker.requeue(&delivery.queue, delivery.message);
        }
    }

    /// Stop reading a subscription's queue, and take that queue with it when it was this
    /// subscription's own.
    fn close_subscription(&self, subscription: &Subscription) {
        let mut broker = self.socket.broker.lock();
        broker.remove_consumer(&subscription.queue);
        if subscription.owns_queue {
            let _ = broker.delete_queue(&subscription.queue, false, false);
        }
    }

    /// Give back everything the session holds and close every subscription it registered,
    /// which is what the end of a connection does: a message nobody acknowledged waits in
    /// its queue again, and a topic subscription's own queue is gone.
    fn release_everything(&mut self) {
        let subscriptions = ::std::mem::take(&mut self.subscriptions);
        for subscription in &subscriptions {
            self.give_back(&subscription.id);
            self.close_subscription(subscription);
        }
    }
}

/// The exchange kind a topic destination is served by: every subscription's queue is bound
/// to it, so a message sent to the topic reaches each of them.
const FAN_OUT: &str = "fanout";

/// The delivery mode a message a client sent `persistent:true` with carries, which is
/// AMQP's own numbering of a persistent message.
const PERSISTENT_DELIVERY_MODE: u8 = 2;

/// The version a client's `accept-version` settles on: the newest this server serves among
/// the ones the client named, or STOMP 1.0 for a client that named none (a 1.0 client, the
/// version that had no such header).
fn negotiate(accepted: Option<&str>) -> Option<Version> {
    let Some(accepted) = accepted else {
        return Some(Version::V10);
    };
    accepted
        .split(',')
        .filter_map(|version| Version::parse(version.trim()))
        .max()
}

/// How a queue destination is declared: a queue that holds what was sent to it until a
/// subscription takes it, which is what a point-to-point destination means.
fn queue_declaration(name: &str) -> QueueDeclaration<'_> {
    QueueDeclaration {
        name,
        passive: false,
        durable: true,
        exclusive: false,
        auto_delete: false,
        arguments: &EMPTY_ARGUMENTS,
    }
}

/// How one topic subscription's own queue is declared: it belongs to the connection that
/// declared it and goes when the subscription does.
fn subscription_declaration(name: &str) -> QueueDeclaration<'_> {
    QueueDeclaration {
        name,
        passive: false,
        durable: false,
        exclusive: true,
        auto_delete: true,
        arguments: &EMPTY_ARGUMENTS,
    }
}

/// The arguments every destination this wire declares carries: none. A STOMP client names
/// no queue arguments, so there is one empty table rather than one per declaration.
static EMPTY_ARGUMENTS: ::std::sync::LazyLock<FieldTable> =
    ::std::sync::LazyLock::new(FieldTable::default);

/// The ERROR frame a refused frame is answered with: the reason in the `message` header
/// (which is what a client library surfaces) and in the body, plus the receipt the refused
/// frame asked for, as STOMP says an ERROR answering a receipted frame carries.
fn error_frame(message: &str, receipt: Option<&str>) -> Frame {
    let mut frame = Frame::new("ERROR").with_header("message", message);
    if let Some(receipt) = receipt {
        frame = frame.with_header("receipt-id", receipt);
    }
    frame
        .with_header("content-type", "text/plain")
        .with_body(message.as_bytes().to_vec())
}

/// Why a destination a client named cannot be served.
fn destination_refusal(named: Option<&str>) -> String {
    match named {
        None => format!(
            "A frame must carry a destination of the form {QUEUE_PREFIX}<name> or \
             {TOPIC_PREFIX}<name>"
        ),
        Some(raw) => format!(
            "The destination {raw} is not one this broker serves: a destination is of the \
             form {QUEUE_PREFIX}<name> or {TOPIC_PREFIX}<name> and takes at most \
             {MAX_DESTINATION_BYTES} bytes"
        ),
    }
}

/// The message broker's refusal as a STOMP client reads it: its text without the reply
/// code the AMQP wire prefixes it with, that code being no part of this protocol.
fn refusal_text(error: BrokerError) -> String {
    let text = match error {
        BrokerError::Channel { text, .. } | BrokerError::Connection { text, .. } => text,
    };
    match text.split_once(" - ") {
        Some((_code, reason)) => reason.to_string(),
        None => text,
    }
}

/// The time to live left of a message whose sender named the instant it expires at, in
/// milliseconds since the epoch — ActiveMQ's own `expires` header, where zero means the
/// message never expires. A message whose instant has already passed is given no life at
/// all rather than an endless one.
fn remaining_life(expires: Option<&str>, now: SystemTime) -> Option<u64> {
    let at_millis: u64 = expires?.trim().parse().ok()?;
    if at_millis == 0 {
        return None;
    }
    let now_millis = u64::try_from(
        now.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    Some(at_millis.saturating_sub(now_millis))
}

/// One stored header as a STOMP frame carries it: text, every header of the protocol being
/// text. A value of another type reaches no delivery rather than being written as its
/// debug form.
fn header_text(value: &AMQPValue) -> Option<String> {
    match value {
        AMQPValue::LongString(text) => {
            Some(String::from_utf8_lossy(text.as_bytes()).to_string())
        }
        AMQPValue::ShortString(text) => Some(text.as_str().to_string()),
        _ => None,
    }
}

/// What one read off the connection produced.
enum Read {
    /// A whole frame.
    Frame(Box<Frame>),
    /// Nothing arrived before the deadline.
    Idle,
    /// The client closed the connection, or it could not be read.
    Closed,
    /// The bytes on the connection are not a frame this server can serve. The text says
    /// why, which is what the ERROR carries.
    Malformed(String),
}

/// The connection itself: the byte stream and what has been read off it and not parsed
/// yet.
struct Io {
    conn: ListenerConn,
    buffered: Vec<u8>,
}

impl Io {
    fn new(conn: ListenerConn) -> Self {
        Self {
            conn,
            buffered: Vec::new(),
        }
    }

    /// Write one frame, answering `false` when the connection could not take it.
    async fn write_frame(&mut self, frame: &Frame, escaping: Escaping) -> bool {
        let bytes = frame.serialize(escaping);
        if self.conn.write_all(&bytes).await.is_err() {
            return false;
        }
        self.conn.flush().await.is_ok()
    }

    /// The next whole frame, or [`Read::Idle`] when none arrived within `wait`.
    async fn next_frame(&mut self, wait: Duration, escaping: Escaping) -> Read {
        loop {
            // A bare end of line between frames is a heart-beat, not a frame.
            let skipped = stomp_frames::skip_interframe(&self.buffered);
            self.buffered.drain(..skipped);
            match stomp_frames::parse(&self.buffered, escaping) {
                Parsed::Frame(frame, consumed) => {
                    self.buffered.drain(..consumed);
                    return Read::Frame(frame);
                }
                Parsed::Malformed(why) => return Read::Malformed(why),
                Parsed::Incomplete => {}
            }
            if self.buffered.len() > MAX_BUFFERED_BYTES {
                return Read::Malformed(format!(
                    "a frame may take at most {MAX_BUFFERED_BYTES} bytes"
                ));
            }
            let mut chunk = vec![0u8; READ_CHUNK];
            match ::tokio::time::timeout(wait, self.conn.read(&mut chunk)).await {
                Err(_elapsed) => return Read::Idle,
                Ok(Ok(0)) | Ok(Err(_)) => return Read::Closed,
                Ok(Ok(read)) => self.buffered.extend_from_slice(&chunk[..read]),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a destination is one of ActiveMQ's two kinds, told apart by the prefix the
    /// client names it with, and anything else names no destination this broker serves.
    #[test]
    fn a_destination_is_a_queue_or_a_topic() {
        let long_name = format!(
            "{QUEUE_PREFIX}{}",
            "q".repeat(MAX_DESTINATION_BYTES - QUEUE_PREFIX.len())
        );
        let past_the_bound = format!("{QUEUE_PREFIX}{}", "p".repeat(MAX_DESTINATION_BYTES));
        // The rows span what a client can name: both kinds, a name with path characters
        // in it, the longest name there is room for, and the forms that name nothing —
        // no prefix, an ActiveMQ destination kind this broker does not serve, an empty
        // name and one past the bound.
        let rows: &[(&str, &str, Option<bool>)] = &[
            ("a queue", "/queue/orders", Some(true)),
            ("a topic", "/topic/prices", Some(false)),
            ("a name with a slash in it", "/queue/orders/eu", Some(true)),
            ("the longest name there is room for", long_name.as_str(), Some(true)),
            ("no prefix", "orders", None),
            ("a temporary queue", "/temp-queue/orders", None),
            ("an empty queue name", "/queue/", None),
            ("an empty topic name", "/topic/", None),
            (
                "a name past the bound",
                past_the_bound.as_str(),
                None,
            ),
        ];
        for (case, raw, expected) in rows {
            let parsed = Destination::parse(raw);
            match expected {
                None => assert!(
                    parsed.is_none(),
                    "case {case:?}: expected {raw:?} to name no destination, got {parsed:?}"
                ),
                Some(is_queue) => {
                    let parsed = parsed.unwrap_or_else(|| {
                        panic!("case {case:?}: expected {raw:?} to name a destination")
                    });
                    assert_eq!(
                        matches!(parsed, Destination::Queue(_)),
                        *is_queue,
                        "case {case:?}: expected a queue? {is_queue}, got {parsed:?}"
                    );
                    assert_eq!(
                        parsed.as_str(),
                        *raw,
                        "case {case:?}: a destination reports the text the client named"
                    );
                }
            }
        }
    }

    /// RULE: the version a connection runs is the NEWEST the server serves among the ones
    /// the client accepts; a client that names none is a STOMP 1.0 client, and one that
    /// names only versions this server does not serve gets no session.
    #[test]
    fn the_newest_accepted_version_is_the_one_served() {
        // The rows span what `accept-version` can carry: one version, several in either
        // order, a list holding versions beyond this server's, spacing, and nothing at
        // all.
        let rows: &[(&str, Option<&str>, Option<&str>)] = &[
            ("no accept-version at all", None, Some("1.0")),
            ("one version", Some("1.1"), Some("1.1")),
            ("the whole list", Some("1.0,1.1,1.2"), Some("1.2")),
            ("the list in reverse", Some("1.2,1.1,1.0"), Some("1.2")),
            ("a version beyond this server", Some("1.1,2.0"), Some("1.1")),
            ("spacing around the versions", Some(" 1.1 , 1.2 "), Some("1.2")),
            ("only versions beyond this server", Some("2.0,3.0"), None),
            ("an empty list", Some(""), None),
        ];
        for (case, accepted, expected) in rows {
            let negotiated = negotiate(*accepted).map(Version::as_str);
            assert_eq!(
                negotiated, *expected,
                "case {case:?}: expected {expected:?}, got {negotiated:?}"
            );
        }
    }

    /// RULE: an `ack` header names one of the three modes STOMP defines, and nothing else.
    #[test]
    fn an_ack_mode_is_one_of_the_three_stomp_defines() {
        let rows: &[(&str, Option<AckMode>)] = &[
            ("auto", Some(AckMode::Auto)),
            ("client", Some(AckMode::Client)),
            ("client-individual", Some(AckMode::ClientIndividual)),
            ("AUTO", None),
            ("", None),
            ("individual", None),
        ];
        for (text, expected) in rows {
            let parsed = AckMode::parse(text);
            assert_eq!(
                parsed, *expected,
                "case {text:?}: expected {expected:?}, got {parsed:?}"
            );
        }
    }

    /// RULE: a message whose sender named the instant it expires at is given the life it
    /// has LEFT; zero means it never expires, and an instant already past leaves none.
    #[test]
    fn an_expiry_instant_becomes_the_life_left() {
        let now = UNIX_EPOCH + Duration::from_millis(10_000);
        let rows: &[(&str, Option<&str>, Option<u64>)] = &[
            ("no expires header", None, None),
            ("an instant in the future", Some("12000"), Some(2_000)),
            ("zero, which never expires", Some("0"), None),
            ("an instant already past", Some("9000"), Some(0)),
            ("text that is no instant", Some("soon"), None),
        ];
        for (case, expires, expected) in rows {
            let left = remaining_life(*expires, now);
            assert_eq!(
                left, *expected,
                "case {case:?}: expected {expected:?} milliseconds left, got {left:?}"
            );
        }
    }
}
