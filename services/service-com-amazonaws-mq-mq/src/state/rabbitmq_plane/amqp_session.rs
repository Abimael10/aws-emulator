// SPDX-License-Identifier: CC0-1.0

//! The AMQP 0-9-1 SERVER one connection to a RabbitMQ broker's endpoint is served by:
//! the handshake, the channels opened on it, and the methods a client sends on them.
//!
//! RULE: a client that opens a connection to a RabbitMQ broker's AMQP endpoint
//! authenticates as one of the broker's own users, opens channels on it and is answered
//! the AMQP 0-9-1 method each of its methods is answered with — the synchronous reply for
//! a declaration, a publisher confirmation for a publish once the message is in its
//! queues, a delivery for every message a consumer is next in line for — out of the one
//! message broker that connection's broker holds ([`super::message_broker`]).
//!
//! The frames themselves are parsed and written by the `amq-protocol` codec, so what is
//! on the wire is the protocol as the specification defines it rather than a
//! reimplementation of it. What this module owns is the SERVER side of the conversation:
//! which method answers which, what each one does to the broker's state, and which reply
//! code closes a channel or the connection when a request cannot be served.
//!
//! The core owns the socket, the TLS (an `amqps://` endpoint is TLS from the first byte,
//! which is what `listeners().tcp` serves), the connection cap and the idle timeout; a
//! session only ever reads and writes its own connection.
//!
//! # Bounds
//!
//! * a connection may buffer at most [`MAX_BUFFERED_BYTES`] of unparsed frames, and one
//!   read takes at most [`READ_CHUNK`] bytes;
//! * the frame size and channel count the handshake offers are [`FRAME_MAX`] and
//!   [`CHANNEL_MAX`], so a client cannot ask the server to hold a larger frame;
//! * one pump delivers at most [`MAX_DELIVERIES_PER_PUMP`] messages before reading the
//!   connection again, so a full queue cannot starve the session's own input;
//! * a publish's content is held only up to the body size the broker accepts
//!   ([`message_broker::MAX_MESSAGE_BYTES`]), checked against the size the content
//!   header DECLARES before a byte of body is kept;
//! * a channel holds at most [`MAX_UNACKED_PER_CHANNEL`] unsettled deliveries and
//!   [`MAX_UNACKED_BYTES_PER_CHANNEL`] of their bodies, and one CONNECTION at most
//!   [`MAX_UNACKED_PER_CONNECTION`] and [`MAX_UNACKED_BYTES_PER_CONNECTION`] across
//!   every channel of it — a message a client never acknowledges is out of its queue, so
//!   the broker's own caps no longer account for it and these bounds do, whatever
//!   prefetch the client asked for;
//! * one connection carries at most [`MAX_PENDING_BYTES_PER_CONNECTION`] of publish
//!   content at once, and opens at most [`CHANNEL_MAX`] channels, so what it can hold is
//!   bounded by construction as well as by the totals above;
//! * a channel registers at most [`MAX_CONSUMERS_PER_CHANNEL`] consumers;
//! * everything the session stores lands in the broker, which is bounded in its own
//!   module.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime};

use ::amq_protocol::frame::{gen_frame, parse_frame, AMQPContentHeader, AMQPFrame, ProtocolVersion};
use ::amq_protocol::protocol::basic::AMQPProperties;
use ::amq_protocol::protocol::{basic, channel, confirm, connection, exchange, queue, AMQPClass};
use ::amq_protocol::types::{AMQPValue, FieldTable, LongString, ShortString};
use ::emulator_server::{ListenerConn, NativeHandler};
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::message_broker::{BrokerError, Message, REASON_REJECTED};
use super::message_broker;
use crate::state::BrokerSocket;

/// How many bytes one read off a connection takes at most.
const READ_CHUNK: usize = 16 * 1024;

/// How many bytes of not-yet-complete frames one connection may buffer. A client that
/// fills it is sending something that is not a frame this server can parse, or a frame
/// larger than the handshake allows, and the connection is closed with a frame error.
const MAX_BUFFERED_BYTES: usize = 2 * FRAME_MAX as usize;

/// The largest frame this server accepts, offered in the handshake's `tune`.
const FRAME_MAX: u32 = 131_072;

/// How many channels one connection may open, offered in the handshake's `tune`.
///
/// What a channel may hold is bounded per channel, so the number of channels is what
/// decides what one CONNECTION can hold: this is deliberately a small number, ample for
/// a client of this emulator, so the aggregate is bounded by construction as well as by
/// [`MAX_UNACKED_BYTES_PER_CONNECTION`].
const CHANNEL_MAX: u16 = 32;

/// The heartbeat interval this server offers, in seconds. The client's `tune-ok` is
/// what settles it, and zero turns heartbeats off.
const HEARTBEAT_SECS: u16 = 60;

/// How long the session waits for the next frame before it looks for work of its own:
/// messages to deliver to its consumers, expirations the clock has made due and the
/// heartbeat it owes.
const TICK: Duration = Duration::from_millis(20);

/// How many messages one pump hands to consumers before the connection is read again.
const MAX_DELIVERIES_PER_PUMP: usize = 64;

/// How many consumers one channel may register. A consumer is registered per
/// `basic.consume`, which a client may send as often as it likes, so the list one
/// channel holds is bounded.
const MAX_CONSUMERS_PER_CHANNEL: usize = 64;

/// How many unsettled deliveries one channel may hold.
///
/// A delivery a client never acknowledges is a message OUT of its queue and in this
/// session's hands, so the broker's own message caps no longer account for it: this is
/// the server-side bound on it, which holds whether or not the client set a `basic.qos`
/// prefetch (a prefetch of zero asks for no limit of its own).
const MAX_UNACKED_PER_CHANNEL: usize = 1_000;

/// How many bytes of message bodies one channel's unsettled deliveries may hold — the
/// same bound as [`MAX_UNACKED_PER_CHANNEL`], by size rather than by count.
const MAX_UNACKED_BYTES_PER_CHANNEL: usize = 8 * 1024 * 1024;

/// How many unsettled deliveries one CONNECTION may hold across every channel of it.
///
/// The per-channel bound alone would let a client multiply what it holds by the number
/// of channels it opens, so the aggregate is bounded too.
const MAX_UNACKED_PER_CONNECTION: usize = 2 * MAX_UNACKED_PER_CHANNEL;

/// How many bytes of message bodies one CONNECTION may hold unsettled across every
/// channel of it — see [`MAX_UNACKED_PER_CONNECTION`].
const MAX_UNACKED_BYTES_PER_CONNECTION: usize = 2 * MAX_UNACKED_BYTES_PER_CHANNEL;

/// How many bytes of publish content one CONNECTION may have in flight across every
/// channel of it. One publish's body is bounded by what the broker accepts; this bounds
/// the publishes being carried at once.
const MAX_PENDING_BYTES_PER_CONNECTION: usize = 4 * message_broker::MAX_MESSAGE_BYTES;

/// The authentication mechanism this server offers: the one Amazon MQ for RabbitMQ
/// authenticates a client's user name and password with.
const MECHANISM_PLAIN: &str = "PLAIN";

/// The locale offered in the handshake, which is the one every broker offers.
const LOCALE: &str = "en_US";

/// The virtual host a broker serves. Amazon MQ for RabbitMQ creates the broker with the
/// default virtual host, and a connection asking for another one is refused rather than
/// served the default's queues under a name that does not exist.
const DEFAULT_VHOST: &str = "/";

/// The reply code a `mandatory` publish that reached no queue is returned with.
const NO_ROUTE: u16 = 312;

/// The AMQP server one broker's socket answers with.
pub struct AmqpWire {
    socket: BrokerSocket,
}

impl AmqpWire {
    pub fn new(socket: BrokerSocket) -> Self {
        Self { socket }
    }
}

impl NativeHandler for AmqpWire {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let socket = self.socket.clone();
        Box::pin(async move {
            let mut session = Session::new(socket);
            session.run(conn).await;
        })
    }
}

/// One consumer a channel registered with `basic.consume`.
#[derive(Debug, Clone)]
struct Consumer {
    tag: String,
    queue: String,
    /// Whether the consumer asked for messages to be considered acknowledged as soon as
    /// they are sent.
    no_ack: bool,
}

/// One message a channel has been handed and has not acknowledged yet.
#[derive(Debug, Clone)]
struct Delivery {
    queue: String,
    message: Message,
}

/// A publish whose method frame has arrived and whose content has not (all of it) yet.
#[derive(Debug, Clone)]
struct PendingPublish {
    exchange: String,
    routing_key: String,
    mandatory: bool,
    properties: AMQPProperties,
    body: Vec<u8>,
    body_size: usize,
}

/// One channel of a connection.
#[derive(Debug, Default)]
struct ChannelState {
    /// Whether `confirm.select` put this channel in publisher-confirm mode.
    confirm_mode: bool,
    /// The sequence number of the last publish, which is the delivery tag its
    /// confirmation carries.
    publish_sequence: u64,
    /// The delivery tag of the last message handed to a consumer on this channel.
    delivery_tag: u64,
    /// The deliveries this channel holds and has not settled. A held message is OUT of
    /// its queue, so this is what the session itself keeps, bounded by
    /// [`MAX_UNACKED_PER_CHANNEL`] and [`MAX_UNACKED_BYTES_PER_CHANNEL`] whatever the
    /// client asked for with `basic.qos`.
    unacked: BTreeMap<u64, Delivery>,
    /// The bytes of message bodies [`Self::unacked`] holds, kept beside it so the bound
    /// is a count this channel can test rather than a walk of the map.
    unacked_bytes: usize,
    consumers: Vec<Consumer>,
    pending: Option<PendingPublish>,
    /// How many unacknowledged messages this channel may hold, or zero for no limit.
    prefetch_count: u16,
    /// The sequence a consumer tag this server generates is built from.
    next_consumer_tag: u64,
}

impl ChannelState {
    /// Whether this channel may be handed another delivery at all: the server's own
    /// bound, which holds however the client set its prefetch.
    fn holds_its_limit(&self) -> bool {
        self.unacked.len() >= MAX_UNACKED_PER_CHANNEL
            || self.unacked_bytes >= MAX_UNACKED_BYTES_PER_CHANNEL
    }

    /// Whether the client's own `basic.qos` prefetch is reached as well.
    fn reached_its_prefetch(&self) -> bool {
        self.prefetch_count > 0 && self.unacked.len() >= usize::from(self.prefetch_count)
    }

    /// Hold one delivery until the client settles it.
    fn hold(&mut self, tag: u64, delivery: Delivery) {
        self.unacked_bytes = self.unacked_bytes.saturating_add(delivery.message.body.len());
        self.unacked.insert(tag, delivery);
    }

    /// Give back one held delivery, so what the channel holds shrinks with it.
    fn release(&mut self, tag: u64) -> Option<Delivery> {
        let delivery = self.unacked.remove(&tag)?;
        self.unacked_bytes = self
            .unacked_bytes
            .saturating_sub(delivery.message.body.len());
        Some(delivery)
    }

    /// Give back every held delivery, as a channel that closes does.
    fn release_all(&mut self) -> Vec<Delivery> {
        self.unacked_bytes = 0;
        ::std::mem::take(&mut self.unacked)
            .into_values()
            .collect()
    }
}

/// Whether the session carries on reading or is done with the connection.
enum Flow {
    Continue,
    Close,
}

/// One broker's exclusive-queue record for one session, plus the rest of what a session
/// holds.
struct Session {
    socket: BrokerSocket,
    channels: BTreeMap<u16, ChannelState>,
    /// The negotiated heartbeat interval; zero when heartbeats are off.
    heartbeat: Duration,
    /// The largest frame this connection agreed to carry: the lower of what the server
    /// offered and what the client's `tune-ok` named.
    frame_max: u32,
    /// The highest channel this connection agreed to open, settled the same way.
    channel_max: u16,
    /// The user this connection authenticated as.
    user: String,
    /// The queues THIS connection declared exclusive. An exclusive queue belongs to the
    /// connection that declared it: no other connection may use it, and it is deleted
    /// when this one ends.
    exclusive_queues: Vec<String>,
}

impl Session {
    fn new(socket: BrokerSocket) -> Self {
        Self {
            socket,
            channels: BTreeMap::new(),
            heartbeat: Duration::ZERO,
            frame_max: FRAME_MAX,
            channel_max: CHANNEL_MAX,
            user: String::new(),
            exclusive_queues: Vec::new(),
        }
    }

    /// Serve one connection: the handshake, then every frame until the client closes the
    /// connection, the frames stop making sense, or the core severs it.
    async fn run(&mut self, conn: ListenerConn) {
        let mut io = Io::new(conn);
        if !self.handshake(&mut io).await {
            return;
        }
        loop {
            if !self.pump(&mut io).await {
                break;
            }
            match io.next_frame(TICK).await {
                Read::Frame(frame) => match self.dispatch(&mut io, *frame).await {
                    Flow::Continue => {}
                    Flow::Close => break,
                },
                Read::Idle => {
                    if !io.beat(self.heartbeat).await {
                        break;
                    }
                }
                Read::Closed => break,
                Read::Malformed => {
                    let _ = io
                        .write_frame(&connection_close(
                            501,
                            "FRAME_ERROR - the frame could not be parsed".to_string(),
                            0,
                            0,
                        ))
                        .await;
                    break;
                }
            }
        }
        self.release_everything();
    }

    /// The connection handshake: `start` / `start-ok`, `tune` / `tune-ok`, `open` /
    /// `open-ok`. Answers `false` when the connection did not get through it, in which
    /// case the refusal has already been written.
    async fn handshake(&mut self, io: &mut Io) -> bool {
        match io.next_frame(HANDSHAKE_WAIT).await {
            Read::Frame(frame) => match *frame {
                AMQPFrame::ProtocolHeader(version) => {
                    if (version.major, version.minor, version.revision) != (0, 9, 1) {
                        // The specification's own answer to a version this server does
                        // not speak: the header of the version it does, then the
                        // connection is closed.
                        let _ = io
                            .write_frame(&AMQPFrame::ProtocolHeader(ProtocolVersion::amqp_0_9_1()))
                            .await;
                        return false;
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        if !io.write_frame(&start_method()).await {
            return false;
        }
        let Read::Frame(frame) = io.next_frame(HANDSHAKE_WAIT).await else {
            return false;
        };
        let AMQPFrame::Method(
            _,
            AMQPClass::Connection(connection::AMQPMethod::StartOk(start_ok)),
        ) = *frame
        else {
            return false;
        };
        if start_ok.mechanism.as_str() != MECHANISM_PLAIN {
            let _ = io
                .write_frame(&connection_close(
                    403,
                    format!(
                        "ACCESS_REFUSED - unsupported authentication mechanism '{}'; this broker \
                         offers '{MECHANISM_PLAIN}'",
                        start_ok.mechanism.as_str()
                    ),
                    CLASS_ID_CONNECTION,
                    START_OK_METHOD_ID,
                ))
                .await;
            return false;
        }
        let Some((username, password)) = plain_credentials(start_ok.response.as_bytes()) else {
            ::tracing::debug!(
                broker_id = self.socket.broker_id,
                "a connection to this broker presented no PLAIN credentials and was refused"
            );
            let _ = io.write_frame(&access_refused()).await;
            return false;
        };
        if !self.authenticate(&username, &password) {
            // The user name only: a password never reaches a log line.
            ::tracing::debug!(
                broker_id = self.socket.broker_id,
                user = username,
                "a login on this broker was refused"
            );
            let _ = io.write_frame(&access_refused()).await;
            return false;
        }
        ::tracing::debug!(
            broker_id = self.socket.broker_id,
            user = username,
            "a session was opened on this broker"
        );
        self.user = username;
        if !io.write_frame(&tune_method()).await {
            return false;
        }
        let Read::Frame(frame) = io.next_frame(HANDSHAKE_WAIT).await else {
            return false;
        };
        let AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::TuneOk(tune_ok))) =
            *frame
        else {
            return false;
        };
        // The client's `tune-ok` settles the connection's parameters, within what the
        // server offered: a value of zero means "no limit of mine", which is the
        // server's own, and a value above the offer is held to it, so what is in force
        // is always the lower of the two.
        self.heartbeat = Duration::from_secs(u64::from(tune_ok.heartbeat.min(HEARTBEAT_SECS)));
        self.frame_max = if tune_ok.frame_max == 0 {
            FRAME_MAX
        } else {
            tune_ok.frame_max.min(FRAME_MAX)
        };
        self.channel_max = if tune_ok.channel_max == 0 {
            CHANNEL_MAX
        } else {
            tune_ok.channel_max.min(CHANNEL_MAX)
        };
        let Read::Frame(frame) = io.next_frame(HANDSHAKE_WAIT).await else {
            return false;
        };
        let AMQPFrame::Method(_, AMQPClass::Connection(connection::AMQPMethod::Open(open))) =
            *frame
        else {
            return false;
        };
        let vhost = open.virtual_host.as_str();
        if vhost != DEFAULT_VHOST && !vhost.is_empty() {
            let _ = io
                .write_frame(&connection_close(
                    530,
                    format!("NOT_ALLOWED - vhost {vhost} not found"),
                    CLASS_ID_CONNECTION,
                    OPEN_METHOD_ID,
                ))
                .await;
            return false;
        }
        io.write_frame(&AMQPFrame::Method(
            0,
            AMQPClass::Connection(connection::AMQPMethod::OpenOk(connection::OpenOk::default())),
        ))
        .await
    }

    /// Whether `username` and `password` are a user of the broker this socket serves.
    ///
    /// Read from the control plane's store on every connection, so the users the broker
    /// HAS are the users that can open a session on it.
    fn authenticate(&self, username: &str, password: &str) -> bool {
        let store = self.socket.state.lock();
        store
            .brokers
            .get(&self.socket.broker_id)
            .and_then(|broker| broker.users.get(username))
            .is_some_and(|user| user.password == password)
    }

    /// Hand every message its consumers are next in line for to the client, and
    /// materialise the expirations the clock has made due. Answers `false` when the
    /// connection could not be written.
    async fn pump(&mut self, io: &mut Io) -> bool {
        let now = SystemTime::now();
        self.socket.broker.lock().expire_due(now);
        let mut delivered = 0usize;
        let channels: Vec<u16> = self.channels.keys().copied().collect();
        for id in channels {
            while let Some(state) = self.channels.get(&id) {
                if state.consumers.is_empty() {
                    break;
                }
                // Two bounds, and the lower one wins: the prefetch the client asked for,
                // and the server's own bound on what one channel and one connection may
                // hold unsettled.
                if state.reached_its_prefetch() {
                    break;
                }
                if self.holds_its_limit(id) {
                    break;
                }
                if delivered >= MAX_DELIVERIES_PER_PUMP {
                    return true;
                }
                // The consumer with something waiting, taken in registration order so
                // one busy queue does not starve another on the same channel.
                let mut taken: Option<(Consumer, Message)> = None;
                {
                    let mut broker = self.socket.broker.lock();
                    for consumer in &state.consumers {
                        if let Some(message) = broker.take_next(&consumer.queue, now) {
                            taken = Some((consumer.clone(), message));
                            break;
                        }
                    }
                }
                let Some((consumer, message)) = taken else {
                    break;
                };
                delivered += 1;
                let Some(state) = self.channels.get_mut(&id) else {
                    break;
                };
                state.delivery_tag = state.delivery_tag.saturating_add(1);
                let tag = state.delivery_tag;
                let deliver = AMQPFrame::Method(
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::Deliver(basic::Deliver {
                        consumer_tag: ShortString::from(consumer.tag.as_str()),
                        delivery_tag: tag,
                        redelivered: message.redelivered,
                        exchange: ShortString::from(message.exchange.as_str()),
                        routing_key: ShortString::from(message.routing_key.as_str()),
                    })),
                );
                if !consumer.no_ack {
                    state.hold(
                        tag,
                        Delivery {
                            queue: consumer.queue.clone(),
                            message: message.clone(),
                        },
                    );
                }
                if !io.write_frame(&deliver).await {
                    return false;
                }
                if !io.write_content(id, &message, self.frame_max).await {
                    return false;
                }
            }
        }
        true
    }

    /// Answer one frame.
    async fn dispatch(&mut self, io: &mut Io, frame: AMQPFrame) -> Flow {
        match frame {
            AMQPFrame::Heartbeat | AMQPFrame::InvalidHeartbeat(_) => Flow::Continue,
            AMQPFrame::ProtocolHeader(_) => self.fail_connection(io, 505, "UNEXPECTED_FRAME - a protocol header arrived on an open connection".to_string(), 0, 0).await,
            AMQPFrame::Method(0, AMQPClass::Connection(method)) => {
                self.connection_method(io, method).await
            }
            AMQPFrame::Method(0, _) => {
                self.fail_connection(
                    io,
                    503,
                    "COMMAND_INVALID - only connection methods are allowed on channel 0"
                        .to_string(),
                    0,
                    0,
                )
                .await
            }
            AMQPFrame::Method(id, method) => self.channel_method(io, id, method).await,
            AMQPFrame::Header(id, header) => self.content_header(io, id, header).await,
            AMQPFrame::Body(id, bytes) => self.content_body(io, id, bytes).await,
        }
    }

    /// A method on channel 0.
    async fn connection_method(&mut self, io: &mut Io, method: connection::AMQPMethod) -> Flow {
        match method {
            connection::AMQPMethod::Close(_) => {
                let _ = io
                    .write_frame(&AMQPFrame::Method(
                        0,
                        AMQPClass::Connection(connection::AMQPMethod::CloseOk(
                            connection::CloseOk::default(),
                        )),
                    ))
                    .await;
                Flow::Close
            }
            connection::AMQPMethod::CloseOk(_) => Flow::Close,
            // A client telling the server it is blocked or unblocked needs no answer.
            connection::AMQPMethod::Blocked(_) | connection::AMQPMethod::Unblocked(_) => {
                Flow::Continue
            }
            other => {
                self.fail_connection(
                    io,
                    503,
                    format!(
                        "COMMAND_INVALID - the connection is already open, '{}' is not expected \
                         on it",
                        method_name(&AMQPClass::Connection(other))
                    ),
                    CLASS_ID_CONNECTION,
                    0,
                )
                .await
            }
        }
    }

    /// A method on a channel other than 0.
    async fn channel_method(&mut self, io: &mut Io, id: u16, method: AMQPClass) -> Flow {
        // `channel.open` is the one method allowed on a channel that is not open, and
        // the one that is not allowed on a channel that is.
        if let AMQPClass::Channel(channel::AMQPMethod::Open(_)) = &method {
            if self.channels.contains_key(&id) {
                return self
                    .fail_connection(
                        io,
                        504,
                        format!("CHANNEL_ERROR - second 'channel.open' seen on channel {id}"),
                        CLASS_ID_CHANNEL,
                        OPEN_METHOD_ID,
                    )
                    .await;
            }
            if id > self.channel_max {
                return self
                    .fail_connection(
                        io,
                        504,
                        format!(
                            "CHANNEL_ERROR - channel {id} is above the {} channels this \
                             connection negotiated",
                            self.channel_max
                        ),
                        CLASS_ID_CHANNEL,
                        OPEN_METHOD_ID,
                    )
                    .await;
            }
            self.channels.insert(id, ChannelState::default());
            let written = io
                .write_frame(&AMQPFrame::Method(
                    id,
                    AMQPClass::Channel(channel::AMQPMethod::OpenOk(channel::OpenOk::default())),
                ))
                .await;
            return if written { Flow::Continue } else { Flow::Close };
        }
        if !self.channels.contains_key(&id) {
            // `channel.close-ok` closes a channel this server already dropped, which is
            // the answer to its own `channel.close`; everything else on a channel that
            // is not open is a connection error.
            if matches!(method, AMQPClass::Channel(channel::AMQPMethod::CloseOk(_))) {
                return Flow::Continue;
            }
            return self
                .fail_connection(
                    io,
                    504,
                    format!("CHANNEL_ERROR - expected 'channel.open' on channel {id}"),
                    method.get_amqp_class_id(),
                    method.get_amqp_method_id(),
                )
                .await;
        }
        let class_id = method.get_amqp_class_id();
        let method_id = method.get_amqp_method_id();
        match self.serve_channel_method(io, id, method).await {
            Ok(flow) => flow,
            Err(BrokerError::Channel { code, text }) => {
                self.close_channel(io, id, code, text, class_id, method_id)
                    .await
            }
            Err(BrokerError::Connection { code, text }) => {
                self.fail_connection(io, code, text, class_id, method_id)
                    .await
            }
        }
    }

    /// The channel methods themselves, each answered out of the broker's state.
    async fn serve_channel_method(
        &mut self,
        io: &mut Io,
        id: u16,
        method: AMQPClass,
    ) -> Result<Flow, BrokerError> {
        let now = SystemTime::now();
        match method {
            AMQPClass::Channel(channel::AMQPMethod::Close(_)) => {
                self.release_channel(id);
                self.channels.remove(&id);
                self.reply(
                    io,
                    id,
                    AMQPClass::Channel(channel::AMQPMethod::CloseOk(channel::CloseOk::default())),
                )
                .await
            }
            AMQPClass::Channel(channel::AMQPMethod::CloseOk(_)) => Ok(Flow::Continue),
            AMQPClass::Channel(channel::AMQPMethod::Flow(flow)) => {
                self.reply(
                    io,
                    id,
                    AMQPClass::Channel(channel::AMQPMethod::FlowOk(channel::FlowOk {
                        active: flow.active,
                    })),
                )
                .await
            }
            AMQPClass::Confirm(confirm::AMQPMethod::Select(select)) => {
                if let Some(state) = self.channels.get_mut(&id) {
                    state.confirm_mode = true;
                }
                if select.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Confirm(confirm::AMQPMethod::SelectOk(confirm::SelectOk::default())),
                )
                .await
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Declare(declare)) => {
                self.socket.broker.lock().declare_exchange(
                    declare.exchange.as_str(),
                    declare.kind.as_str(),
                    declare.passive,
                    declare.durable,
                    declare.auto_delete,
                    declare.internal,
                )?;
                if declare.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Exchange(exchange::AMQPMethod::DeclareOk(
                        exchange::DeclareOk::default(),
                    )),
                )
                .await
            }
            AMQPClass::Exchange(exchange::AMQPMethod::Delete(delete)) => {
                self.socket
                    .broker
                    .lock()
                    .delete_exchange(delete.exchange.as_str(), delete.if_unused)?;
                if delete.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Exchange(exchange::AMQPMethod::DeleteOk(
                        exchange::DeleteOk::default(),
                    )),
                )
                .await
            }
            AMQPClass::Queue(queue::AMQPMethod::Declare(declare)) => {
                self.exclusive_access(declare.queue.as_str())?;
                let declaration = super::message_broker::QueueDeclaration {
                    name: declare.queue.as_str(),
                    passive: declare.passive,
                    durable: declare.durable,
                    exclusive: declare.exclusive,
                    auto_delete: declare.auto_delete,
                    arguments: &declare.arguments,
                };
                let state = self
                    .socket
                    .broker
                    .lock()
                    .declare_queue(&declaration, now)?;
                // An exclusive queue belongs to this connection from here on: no other
                // may use it, and it goes when this connection does.
                if declare.exclusive && !self.exclusive_queues.contains(&state.name) {
                    self.exclusive_queues.push(state.name.clone());
                }
                if declare.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Queue(queue::AMQPMethod::DeclareOk(queue::DeclareOk {
                        queue: ShortString::from(state.name.as_str()),
                        message_count: state.message_count,
                        consumer_count: state.consumer_count,
                    })),
                )
                .await
            }
            AMQPClass::Queue(queue::AMQPMethod::Bind(bind)) => {
                self.exclusive_access(bind.queue.as_str())?;
                self.socket.broker.lock().bind_queue(
                    bind.queue.as_str(),
                    bind.exchange.as_str(),
                    bind.routing_key.as_str(),
                    &bind.arguments,
                )?;
                if bind.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Queue(queue::AMQPMethod::BindOk(queue::BindOk::default())),
                )
                .await
            }
            AMQPClass::Queue(queue::AMQPMethod::Unbind(unbind)) => {
                self.exclusive_access(unbind.queue.as_str())?;
                self.socket.broker.lock().unbind_queue(
                    unbind.queue.as_str(),
                    unbind.exchange.as_str(),
                    unbind.routing_key.as_str(),
                )?;
                self.reply(
                    io,
                    id,
                    AMQPClass::Queue(queue::AMQPMethod::UnbindOk(queue::UnbindOk::default())),
                )
                .await
            }
            AMQPClass::Queue(queue::AMQPMethod::Purge(purge)) => {
                self.exclusive_access(purge.queue.as_str())?;
                let purged = self
                    .socket
                    .broker
                    .lock()
                    .purge_queue(purge.queue.as_str())?;
                if purge.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Queue(queue::AMQPMethod::PurgeOk(queue::PurgeOk {
                        message_count: purged,
                    })),
                )
                .await
            }
            AMQPClass::Queue(queue::AMQPMethod::Delete(delete)) => {
                self.exclusive_access(delete.queue.as_str())?;
                let removed = self.socket.broker.lock().delete_queue(
                    delete.queue.as_str(),
                    delete.if_unused,
                    delete.if_empty,
                )?;
                if delete.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Queue(queue::AMQPMethod::DeleteOk(queue::DeleteOk {
                        message_count: removed,
                    })),
                )
                .await
            }
            AMQPClass::Basic(basic::AMQPMethod::Qos(qos)) => {
                if let Some(state) = self.channels.get_mut(&id) {
                    state.prefetch_count = qos.prefetch_count;
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::QosOk(basic::QosOk::default())),
                )
                .await
            }
            AMQPClass::Basic(basic::AMQPMethod::Consume(consume)) => {
                self.exclusive_access(consume.queue.as_str())?;
                // A consumer tag names one consumer of this connection, so a tag
                // already in use is refused rather than registered twice — a second
                // consumer under one tag would make every later delivery and cancel
                // under it ambiguous.
                let asked = consume.consumer_tag.as_str().to_string();
                let held = self
                    .channels
                    .get(&id)
                    .map_or(0, |state| state.consumers.len());
                if !asked.is_empty() && self.consumer_tag_in_use(&asked) {
                    return Err(BrokerError::Connection {
                        code: 530,
                        text: format!("NOT_ALLOWED - attempt to reuse consumer tag '{asked}'"),
                    });
                }
                if held >= MAX_CONSUMERS_PER_CHANNEL {
                    return Err(BrokerError::Connection {
                        code: 530,
                        text: format!(
                            "NOT_ALLOWED - this channel already holds its limit of \
                             {MAX_CONSUMERS_PER_CHANNEL} consumers"
                        ),
                    });
                }
                self.socket
                    .broker
                    .lock()
                    .add_consumer(consume.queue.as_str())?;
                let tag = if asked.is_empty() {
                    let state = self.channels.get_mut(&id);
                    let sequence = state.map_or(0, |state| {
                        state.next_consumer_tag = state.next_consumer_tag.saturating_add(1);
                        state.next_consumer_tag
                    });
                    format!("amq.ctag-{id}-{sequence}")
                } else {
                    asked
                };
                if let Some(state) = self.channels.get_mut(&id) {
                    state.consumers.push(Consumer {
                        tag: tag.clone(),
                        queue: consume.queue.as_str().to_string(),
                        no_ack: consume.no_ack,
                    });
                }
                if consume.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::ConsumeOk(basic::ConsumeOk {
                        consumer_tag: ShortString::from(tag.as_str()),
                    })),
                )
                .await
            }
            AMQPClass::Basic(basic::AMQPMethod::Cancel(cancel)) => {
                let tag = cancel.consumer_tag.as_str().to_string();
                let mut cancelled = Vec::new();
                if let Some(state) = self.channels.get_mut(&id) {
                    state.consumers.retain(|consumer| {
                        if consumer.tag == tag {
                            cancelled.push(consumer.queue.clone());
                            false
                        } else {
                            true
                        }
                    });
                }
                {
                    let mut broker = self.socket.broker.lock();
                    for queue in cancelled {
                        broker.remove_consumer(&queue);
                    }
                }
                if cancel.nowait {
                    return Ok(Flow::Continue);
                }
                self.reply(
                    io,
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::CancelOk(basic::CancelOk {
                        consumer_tag: ShortString::from(tag.as_str()),
                    })),
                )
                .await
            }
            AMQPClass::Basic(basic::AMQPMethod::Publish(publish)) => {
                if let Some(state) = self.channels.get_mut(&id) {
                    state.pending = Some(PendingPublish {
                        exchange: publish.exchange.as_str().to_string(),
                        routing_key: publish.routing_key.as_str().to_string(),
                        mandatory: publish.mandatory,
                        properties: AMQPProperties::default(),
                        body: Vec::new(),
                        body_size: 0,
                    });
                }
                Ok(Flow::Continue)
            }
            AMQPClass::Basic(basic::AMQPMethod::Get(get)) => {
                self.exclusive_access(get.queue.as_str())?;
                // A read the channel has no room to hold the answer of is refused before
                // the message leaves its queue, so nothing is taken and lost.
                if !get.no_ack && self.holds_its_limit(id) {
                    return Err(BrokerError::Connection {
                        code: 506,
                        text: format!(
                            "RESOURCE_ERROR - this connection already holds its limit of \
                             {MAX_UNACKED_PER_CHANNEL} unacknowledged deliveries on a channel \
                             or {MAX_UNACKED_PER_CONNECTION} across them; acknowledge them \
                             before reading more"
                        ),
                    });
                }
                let got = self.socket.broker.lock().get(get.queue.as_str(), now)?;
                let Some((message, left)) = got else {
                    return self
                        .reply(
                            io,
                            id,
                            AMQPClass::Basic(basic::AMQPMethod::GetEmpty(
                                basic::GetEmpty::default(),
                            )),
                        )
                        .await;
                };
                let tag = {
                    let Some(state) = self.channels.get_mut(&id) else {
                        return Ok(Flow::Continue);
                    };
                    state.delivery_tag = state.delivery_tag.saturating_add(1);
                    let tag = state.delivery_tag;
                    if !get.no_ack {
                        state.hold(
                            tag,
                            Delivery {
                                queue: get.queue.as_str().to_string(),
                                message: message.clone(),
                            },
                        );
                    }
                    tag
                };
                let written = io
                    .write_frame(&AMQPFrame::Method(
                        id,
                        AMQPClass::Basic(basic::AMQPMethod::GetOk(basic::GetOk {
                            delivery_tag: tag,
                            redelivered: message.redelivered,
                            exchange: ShortString::from(message.exchange.as_str()),
                            routing_key: ShortString::from(message.routing_key.as_str()),
                            message_count: left,
                        })),
                    ))
                    .await
                    && io.write_content(id, &message, self.frame_max).await;
                Ok(if written { Flow::Continue } else { Flow::Close })
            }
            AMQPClass::Basic(basic::AMQPMethod::Ack(ack)) => {
                self.settle(id, ack.delivery_tag, ack.multiple, Settlement::Ack, now)?;
                Ok(Flow::Continue)
            }
            AMQPClass::Basic(basic::AMQPMethod::Nack(nack)) => {
                let settlement = if nack.requeue {
                    Settlement::Requeue
                } else {
                    Settlement::DeadLetter
                };
                self.settle(id, nack.delivery_tag, nack.multiple, settlement, now)?;
                Ok(Flow::Continue)
            }
            AMQPClass::Basic(basic::AMQPMethod::Reject(reject)) => {
                let settlement = if reject.requeue {
                    Settlement::Requeue
                } else {
                    Settlement::DeadLetter
                };
                self.settle(id, reject.delivery_tag, false, settlement, now)?;
                Ok(Flow::Continue)
            }
            AMQPClass::Basic(basic::AMQPMethod::Recover(recover)) => {
                requeue_only(recover.requeue)?;
                self.recover(id);
                self.reply(
                    io,
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::RecoverOk(basic::RecoverOk::default())),
                )
                .await
            }
            AMQPClass::Basic(basic::AMQPMethod::RecoverAsync(recover)) => {
                requeue_only(recover.requeue)?;
                self.recover(id);
                Ok(Flow::Continue)
            }
            other => Err(BrokerError::Connection {
                code: 540,
                text: format!(
                    "NOT_IMPLEMENTED - this broker does not serve '{}'",
                    method_name(&other)
                ),
            }),
        }
    }

    /// The content header of a publish: its properties and how long its body is.
    async fn content_header(&mut self, io: &mut Io, id: u16, header: AMQPContentHeader) -> Flow {
        // A body larger than this broker accepts is refused on the HEADER, before any of
        // it is held: the size a client declares is what would otherwise size the
        // buffer the body accumulates in.
        if header.body_size > message_broker::MAX_MESSAGE_BYTES as u64 {
            if let Some(state) = self.channels.get_mut(&id) {
                state.pending = None;
            }
            let text = format!(
                "CONTENT_TOO_LARGE - message body of {} bytes is larger than the {} bytes this \
                 broker accepts",
                header.body_size,
                message_broker::MAX_MESSAGE_BYTES
            );
            return self
                .close_channel(io, id, 311, text, CLASS_ID_BASIC, PUBLISH_METHOD_ID)
                .await;
        }
        let step = match self.channels.get_mut(&id) {
            None => ContentStep::NoChannel,
            Some(state) => match state.pending.as_mut() {
                None => ContentStep::NoPublish,
                Some(pending) => {
                    pending.properties = header.properties;
                    pending.body_size = usize::try_from(header.body_size).unwrap_or(usize::MAX);
                    ContentStep::Carried(pending.body.len() >= pending.body_size)
                }
            },
        };
        self.carry(io, id, step, "content header").await
    }

    /// One content body frame of a publish.
    async fn content_body(&mut self, io: &mut Io, id: u16, bytes: Vec<u8>) -> Flow {
        // What this CONNECTION is already carrying, read before the channel is borrowed:
        // one publish's body is bounded by what the broker accepts, and this bounds the
        // publishes in flight across every channel of the connection.
        let carried = self.pending_bytes();
        let step = match self.channels.get_mut(&id) {
            None => ContentStep::NoChannel,
            Some(state) => match state.pending.as_mut() {
                None => ContentStep::NoPublish,
                Some(pending) => {
                    // Never more than the broker accepts, whatever the header declared
                    // and however many body frames arrive.
                    if pending.body.len().saturating_add(bytes.len())
                        > message_broker::MAX_MESSAGE_BYTES
                        || carried.saturating_add(bytes.len()) > MAX_PENDING_BYTES_PER_CONNECTION
                    {
                        ContentStep::TooLarge
                    } else {
                        pending.body.extend_from_slice(&bytes);
                        ContentStep::Carried(pending.body.len() >= pending.body_size)
                    }
                }
            },
        };
        self.carry(io, id, step, "content body").await
    }

    /// What one content frame leaves the session doing: nothing, the publish it
    /// completed, or the refusal of a content frame that belongs to no publish.
    async fn carry(&mut self, io: &mut Io, id: u16, step: ContentStep, frame: &str) -> Flow {
        match step {
            ContentStep::NoChannel => Flow::Continue,
            ContentStep::NoPublish => {
                self.fail_connection(
                    io,
                    505,
                    format!("UNEXPECTED_FRAME - a {frame} arrived with no publish to carry it"),
                    0,
                    0,
                )
                .await
            }
            ContentStep::Carried(false) => Flow::Continue,
            ContentStep::Carried(true) => self.finish_publish(io, id).await,
            ContentStep::TooLarge => {
                if let Some(state) = self.channels.get_mut(&id) {
                    state.pending = None;
                }
                let text = format!(
                    "CONTENT_TOO_LARGE - message body is larger than the {} bytes this broker \
                     accepts",
                    message_broker::MAX_MESSAGE_BYTES
                );
                self.close_channel(io, id, 311, text, CLASS_ID_BASIC, PUBLISH_METHOD_ID)
                    .await
            }
        }
    }

    /// Route the publish whose content has now all arrived, and confirm it when the
    /// channel is in confirm mode.
    async fn finish_publish(&mut self, io: &mut Io, id: u16) -> Flow {
        let now = SystemTime::now();
        let Some(state) = self.channels.get_mut(&id) else {
            return Flow::Continue;
        };
        let Some(pending) = state.pending.take() else {
            return Flow::Continue;
        };
        let confirm_mode = state.confirm_mode;
        // A message whose `user_id` names a user other than the one this connection
        // authenticated as is refused: RabbitMQ validates that property against the
        // connection's own user so a publisher cannot claim to be another.
        let claimed = pending
            .properties
            .user_id()
            .as_ref()
            .map(|user| user.as_str().to_string());
        if let Some(claimed) = claimed.filter(|claimed| *claimed != self.user) {
            let text = format!(
                "PRECONDITION_FAILED - user_id property set to '{claimed}' but authenticated user \
                 was '{}'",
                self.user
            );
            return self
                .close_channel(io, id, 406, text, CLASS_ID_BASIC, PUBLISH_METHOD_ID)
                .await;
        }
        let routed = self.socket.broker.lock().publish(
            &pending.exchange,
            &pending.routing_key,
            pending.properties.clone(),
            pending.body.clone(),
            now,
        );
        let routed = match routed {
            Ok(routed) => routed,
            Err(BrokerError::Channel { code, text }) => {
                return self
                    .close_channel(io, id, code, text, CLASS_ID_BASIC, PUBLISH_METHOD_ID)
                    .await;
            }
            Err(BrokerError::Connection { code, text }) => {
                return self
                    .fail_connection(io, code, text, CLASS_ID_BASIC, PUBLISH_METHOD_ID)
                    .await;
            }
        };
        // A `mandatory` message that reached no queue comes back to its publisher,
        // before the confirmation that it was dealt with.
        if !routed && pending.mandatory {
            let returned = Message::published(
                &pending.exchange,
                &pending.routing_key,
                pending.properties.clone(),
                pending.body.clone(),
            );
            let written = io
                .write_frame(&AMQPFrame::Method(
                    id,
                    AMQPClass::Basic(basic::AMQPMethod::Return(basic::Return {
                        reply_code: NO_ROUTE,
                        reply_text: ShortString::from("NO_ROUTE"),
                        exchange: ShortString::from(pending.exchange.as_str()),
                        routing_key: ShortString::from(pending.routing_key.as_str()),
                    })),
                ))
                .await
                && io.write_content(id, &returned, self.frame_max).await;
            if !written {
                return Flow::Close;
            }
        }
        if !confirm_mode {
            return Flow::Continue;
        }
        let tag = {
            let Some(state) = self.channels.get_mut(&id) else {
                return Flow::Continue;
            };
            state.publish_sequence = state.publish_sequence.saturating_add(1);
            state.publish_sequence
        };
        let written = io
            .write_frame(&AMQPFrame::Method(
                id,
                AMQPClass::Basic(basic::AMQPMethod::Ack(basic::Ack {
                    delivery_tag: tag,
                    multiple: false,
                })),
            ))
            .await;
        if written {
            Flow::Continue
        } else {
            Flow::Close
        }
    }

    /// Settle one delivery (or every delivery up to it, for `multiple`).
    fn settle(
        &mut self,
        id: u16,
        delivery_tag: u64,
        multiple: bool,
        settlement: Settlement,
        now: SystemTime,
    ) -> Result<(), BrokerError> {
        let Some(state) = self.channels.get_mut(&id) else {
            return Ok(());
        };
        let tags: Vec<u64> = if multiple {
            // A cumulative settlement names the last delivery it covers, so a tag this
            // channel never issued is as much a precondition failure as an unknown one
            // on the single-delivery path.
            if delivery_tag != 0 && delivery_tag > state.delivery_tag {
                return Err(BrokerError::Channel {
                    code: 406,
                    text: format!("PRECONDITION_FAILED - unknown delivery tag {delivery_tag}"),
                });
            }
            state
                .unacked
                .keys()
                .copied()
                .filter(|tag| delivery_tag == 0 || *tag <= delivery_tag)
                .collect()
        } else {
            if !state.unacked.contains_key(&delivery_tag) {
                return Err(BrokerError::Channel {
                    code: 406,
                    text: format!("PRECONDITION_FAILED - unknown delivery tag {delivery_tag}"),
                });
            }
            vec![delivery_tag]
        };
        let settled: Vec<Delivery> = tags.into_iter().filter_map(|tag| state.release(tag)).collect();
        let mut broker = self.socket.broker.lock();
        for delivery in settled {
            match settlement {
                Settlement::Ack => {}
                Settlement::Requeue => broker.requeue(&delivery.queue, delivery.message),
                Settlement::DeadLetter => {
                    broker.dead_letter(&delivery.queue, delivery.message, REASON_REJECTED, now);
                }
            }
        }
        Ok(())
    }

    /// `basic.recover`: every message this channel holds unacknowledged goes back to its
    /// queue, so the client sees them again.
    fn recover(&mut self, id: u16) {
        let Some(state) = self.channels.get_mut(&id) else {
            return;
        };
        let held = state.release_all();
        let mut broker = self.socket.broker.lock();
        for delivery in held {
            broker.requeue(&delivery.queue, delivery.message);
        }
    }

    /// Write one synchronous reply.
    async fn reply(
        &mut self,
        io: &mut Io,
        id: u16,
        method: AMQPClass,
    ) -> Result<Flow, BrokerError> {
        Ok(if io.write_frame(&AMQPFrame::Method(id, method)).await {
            Flow::Continue
        } else {
            Flow::Close
        })
    }

    /// Close one channel with a reply code, as AMQP answers a request the channel could
    /// not carry out. The connection stays open.
    async fn close_channel(
        &mut self,
        io: &mut Io,
        id: u16,
        code: u16,
        text: String,
        class_id: u16,
        method_id: u16,
    ) -> Flow {
        self.release_channel(id);
        self.channels.remove(&id);
        let written = io
            .write_frame(&AMQPFrame::Method(
                id,
                AMQPClass::Channel(channel::AMQPMethod::Close(channel::Close {
                    reply_code: code,
                    reply_text: reply_text(&text),
                    class_id,
                    method_id,
                })),
            ))
            .await;
        if written {
            Flow::Continue
        } else {
            Flow::Close
        }
    }

    /// Close the whole connection with a reply code.
    async fn fail_connection(
        &mut self,
        io: &mut Io,
        code: u16,
        text: String,
        class_id: u16,
        method_id: u16,
    ) -> Flow {
        let _ = io
            .write_frame(&connection_close(code, text, class_id, method_id))
            .await;
        Flow::Close
    }

    /// Whether any channel of this connection already consumes under `tag`. A consumer
    /// tag is unique per CONNECTION, which is what makes a delivery's tag resolve to one
    /// consumer.
    fn consumer_tag_in_use(&self, tag: &str) -> bool {
        self.channels
            .values()
            .any(|state| state.consumers.iter().any(|consumer| consumer.tag == tag))
    }

    /// Whether this session may be handed another delivery on `id` at all: the
    /// channel's own bound, and the whole connection's, which hold however the client
    /// set its prefetch.
    fn holds_its_limit(&self, id: u16) -> bool {
        if self
            .channels
            .get(&id)
            .is_some_and(ChannelState::holds_its_limit)
        {
            return true;
        }
        let (count, bytes) = self.held_unsettled();
        count >= MAX_UNACKED_PER_CONNECTION || bytes >= MAX_UNACKED_BYTES_PER_CONNECTION
    }

    /// What this connection holds unsettled across every channel: `(count, bytes)`.
    fn held_unsettled(&self) -> (usize, usize) {
        self.channels.values().fold((0, 0), |(count, bytes), state| {
            (
                count.saturating_add(state.unacked.len()),
                bytes.saturating_add(state.unacked_bytes),
            )
        })
    }

    /// The publish content this connection is carrying across every channel.
    fn pending_bytes(&self) -> usize {
        self.channels
            .values()
            .map(|state| state.pending.as_ref().map_or(0, |pending| pending.body.len()))
            .fold(0usize, usize::saturating_add)
    }

    /// Whether this session may use `queue` at all: an EXCLUSIVE queue belongs to the
    /// connection that declared it, and every other connection is refused it.
    fn exclusive_access(&self, queue: &str) -> Result<(), BrokerError> {
        let locked = self.socket.broker.lock().is_exclusive(queue);
        if locked && !self.exclusive_queues.iter().any(|held| held == queue) {
            return Err(BrokerError::Channel {
                code: 405,
                text: format!(
                    "RESOURCE_LOCKED - cannot obtain exclusive access to locked queue '{queue}' \
                     in vhost '/'. It could be originally declared on another connection or the \
                     exclusive property value does not match that of the original declaration."
                ),
            });
        }
        Ok(())
    }

    /// Give back everything one channel holds: its consumers' registrations and the
    /// messages it never acknowledged, which go back to their queues.
    fn release_channel(&mut self, id: u16) {
        let Some(state) = self.channels.get_mut(&id) else {
            return;
        };
        let consumers: Vec<String> = state
            .consumers
            .drain(..)
            .map(|consumer| consumer.queue)
            .collect();
        let held = state.release_all();
        let mut broker = self.socket.broker.lock();
        for queue in consumers {
            broker.remove_consumer(&queue);
        }
        for delivery in held {
            broker.requeue(&delivery.queue, delivery.message);
        }
    }

    /// The same for every channel, when the connection ends: a message a client never
    /// acknowledged is still in its queue for the next consumer, and the queues this
    /// connection declared EXCLUSIVE go with it.
    fn release_everything(&mut self) {
        let channels: Vec<u16> = self.channels.keys().copied().collect();
        for id in channels {
            self.release_channel(id);
        }
        self.channels.clear();
        let exclusive: Vec<String> = ::std::mem::take(&mut self.exclusive_queues);
        let mut broker = self.socket.broker.lock();
        for queue in exclusive {
            let _ = broker.delete_queue(&queue, false, false);
        }
    }
}

/// What one content frame leaves the session doing.
enum ContentStep {
    /// The channel it arrived on is not open.
    NoChannel,
    /// No publish on that channel is waiting for content.
    NoPublish,
    /// The content was taken; `true` once all of the body has arrived.
    Carried(bool),
    /// The body is larger than this broker accepts.
    TooLarge,
}

/// `basic.recover` is only served with `requeue` set, which is the one form RabbitMQ
/// implements: redelivering to the same consumer without requeueing is not.
fn requeue_only(requeue: bool) -> Result<(), BrokerError> {
    if requeue {
        return Ok(());
    }
    Err(BrokerError::Connection {
        code: 540,
        text: "NOT_IMPLEMENTED - basic.recover with requeue=false".to_string(),
    })
}

/// What becomes of a settled delivery.
#[derive(Debug, Clone, Copy)]
enum Settlement {
    /// Acknowledged: the message is gone.
    Ack,
    /// Rejected with requeue: back to the head of its queue.
    Requeue,
    /// Rejected without requeue: to the queue's dead-letter exchange.
    DeadLetter,
}

/// How long a handshake step waits for the client's next frame. The core's handshake
/// deadline and idle timeout bound the connection either way; this one keeps a client
/// that opened a socket and said nothing from holding a session task.
const HANDSHAKE_WAIT: Duration = Duration::from_secs(30);

/// The `connection` class id, for the `class-id` a close carries.
const CLASS_ID_CONNECTION: u16 = 10;

/// The `channel` class id.
const CLASS_ID_CHANNEL: u16 = 20;

/// The `basic` class id.
const CLASS_ID_BASIC: u16 = 60;

/// `connection.start-ok`'s method id.
const START_OK_METHOD_ID: u16 = 11;

/// `connection.open`'s method id, which is also `channel.open`'s.
const OPEN_METHOD_ID: u16 = 40;

/// `basic.publish`'s method id.
const PUBLISH_METHOD_ID: u16 = 40;

/// The `connection.start` this server opens with: what it is, what it can do, and how a
/// client may authenticate.
fn start_method() -> AMQPFrame {
    let mut properties = FieldTable::default();
    properties.insert(
        ShortString::from("product"),
        AMQPValue::LongString(LongString::from("RabbitMQ")),
    );
    properties.insert(
        ShortString::from("platform"),
        AMQPValue::LongString(LongString::from("Amazon MQ")),
    );
    let mut capabilities = FieldTable::default();
    for capability in [
        "publisher_confirms",
        "basic.nack",
        "consumer_cancel_notify",
        "connection.blocked",
        "authentication_failure_close",
    ] {
        capabilities.insert(ShortString::from(capability), AMQPValue::Boolean(true));
    }
    properties.insert(
        ShortString::from("capabilities"),
        AMQPValue::FieldTable(capabilities),
    );
    AMQPFrame::Method(
        0,
        AMQPClass::Connection(connection::AMQPMethod::Start(connection::Start {
            version_major: 0,
            version_minor: 9,
            server_properties: properties,
            mechanisms: LongString::from(MECHANISM_PLAIN),
            locales: LongString::from(LOCALE),
        })),
    )
}

/// The `connection.tune` this server offers: the limits a session runs under.
fn tune_method() -> AMQPFrame {
    AMQPFrame::Method(
        0,
        AMQPClass::Connection(connection::AMQPMethod::Tune(connection::Tune {
            channel_max: CHANNEL_MAX,
            frame_max: FRAME_MAX,
            heartbeat: HEARTBEAT_SECS,
        })),
    )
}

/// The refusal a connection whose credentials are not a user of this broker is closed
/// with — the 403 a client library reads as an authentication failure.
fn access_refused() -> AMQPFrame {
    connection_close(
        403,
        "ACCESS_REFUSED - Login was refused using authentication mechanism PLAIN. For details \
         see the broker logfile."
            .to_string(),
        CLASS_ID_CONNECTION,
        START_OK_METHOD_ID,
    )
}

/// The longest text an AMQP reply code carries. `reply_text` is a short string, whose
/// wire length is a single byte, so a refusal that named more than this would fail to
/// serialize and the client would see a severed socket instead of the reply code it
/// branches on. Every refusal's text goes through [`reply_text`].
const MAX_REPLY_TEXT: usize = 255;

/// A reply text as a frame can carry it: held to [`MAX_REPLY_TEXT`] on a character
/// boundary, so a text that interpolates a client-supplied name still reaches the client
/// with its reply code.
fn reply_text(text: &str) -> ShortString {
    let mut end = text.len().min(MAX_REPLY_TEXT);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    ShortString::from(text.get(..end).unwrap_or_default())
}

/// A `connection.close` frame.
fn connection_close(code: u16, text: String, class_id: u16, method_id: u16) -> AMQPFrame {
    AMQPFrame::Method(
        0,
        AMQPClass::Connection(connection::AMQPMethod::Close(connection::Close {
            reply_code: code,
            reply_text: reply_text(&text),
            class_id,
            method_id,
        })),
    )
}

/// The user name and password a PLAIN `start-ok` response carries:
/// `\0username\0password`, as the SASL mechanism defines it.
fn plain_credentials(response: &[u8]) -> Option<(String, String)> {
    let mut parts = response.split(|byte| *byte == 0);
    // The authorization identity, which PLAIN leaves empty.
    parts.next()?;
    let username = parts.next()?;
    let password = parts.next()?;
    Some((
        String::from_utf8_lossy(username).to_string(),
        String::from_utf8_lossy(password).to_string(),
    ))
}

/// A method's name as a refusal names it (`basic.publish`), for the one case where the
/// method itself is what cannot be served.
fn method_name(method: &AMQPClass) -> String {
    let class = match method {
        AMQPClass::Connection(_) => "connection",
        AMQPClass::Channel(_) => "channel",
        AMQPClass::Access(_) => "access",
        AMQPClass::Exchange(_) => "exchange",
        AMQPClass::Queue(_) => "queue",
        AMQPClass::Basic(_) => "basic",
        AMQPClass::Tx(_) => "tx",
        AMQPClass::Confirm(_) => "confirm",
    };
    format!("{class} method {}", method.get_amqp_method_id())
}

/// What one read off the connection produced.
enum Read {
    /// A whole frame. Boxed because a method frame is far larger than the other
    /// outcomes, and every read returns one of these.
    Frame(Box<AMQPFrame>),
    /// Nothing arrived before the deadline.
    Idle,
    /// The client closed the connection, or it could not be read.
    Closed,
    /// The bytes on the connection are not a frame this server can parse.
    Malformed,
}

/// The connection itself: the byte stream, what has been read off it and not parsed yet,
/// and when it was last written to (which is what the heartbeat is owed against).
struct Io {
    conn: ListenerConn,
    buffered: Vec<u8>,
    last_write: Instant,
}

impl Io {
    fn new(conn: ListenerConn) -> Self {
        Self {
            conn,
            buffered: Vec::new(),
            last_write: Instant::now(),
        }
    }

    /// Write one frame, answering `false` when the connection could not take it.
    async fn write_frame(&mut self, frame: &AMQPFrame) -> bool {
        let Ok(context) = gen_frame(frame)(Vec::new().into()) else {
            return false;
        };
        let (bytes, _written) = context.into_inner();
        if self.conn.write_all(&bytes).await.is_err() {
            return false;
        }
        if self.conn.flush().await.is_err() {
            return false;
        }
        self.last_write = Instant::now();
        true
    }

    /// Write one message's content: its header frame and its body, split into frames no
    /// larger than what the handshake negotiated.
    async fn write_content(&mut self, id: u16, message: &Message, frame_max: u32) -> bool {
        let header = AMQPFrame::Header(
            id,
            AMQPContentHeader {
                class_id: CLASS_ID_BASIC,
                body_size: u64::try_from(message.body.len()).unwrap_or(u64::MAX),
                properties: message.properties.clone(),
            },
        );
        if !self.write_frame(&header).await {
            return false;
        }
        // A zero-length body is carried by no body frame at all, which is what a client
        // reading `body_size` bytes expects. A body frame carries the frame's own 8
        // bytes of framing besides its payload, which is what is subtracted here.
        let payload = usize::try_from(frame_max)
            .unwrap_or(usize::MAX)
            .saturating_sub(8)
            .max(1);
        for chunk in message.body.chunks(payload) {
            if !self.write_frame(&AMQPFrame::Body(id, chunk.to_vec())).await {
                return false;
            }
        }
        true
    }

    /// Send the heartbeat the connection owes, when the negotiated interval has passed
    /// since the last write. Answers `false` when the connection could not take it.
    async fn beat(&mut self, heartbeat: Duration) -> bool {
        if heartbeat.is_zero() {
            return true;
        }
        if self.last_write.elapsed() < heartbeat / 2 {
            return true;
        }
        self.write_frame(&AMQPFrame::Heartbeat).await
    }

    /// The next whole frame, or [`Read::Idle`] when none arrived within `wait`.
    async fn next_frame(&mut self, wait: Duration) -> Read {
        loop {
            match self.parse_buffered() {
                Ok(Some(frame)) => return Read::Frame(Box::new(frame)),
                Ok(None) => {}
                Err(()) => return Read::Malformed,
            }
            if self.buffered.len() > MAX_BUFFERED_BYTES {
                return Read::Malformed;
            }
            let mut chunk = vec![0u8; READ_CHUNK];
            match ::tokio::time::timeout(wait, self.conn.read(&mut chunk)).await {
                Err(_elapsed) => return Read::Idle,
                Ok(Ok(0)) | Ok(Err(_)) => return Read::Closed,
                Ok(Ok(read)) => self.buffered.extend_from_slice(&chunk[..read]),
            }
        }
    }

    /// One frame off what is already buffered: `Ok(None)` when the next frame has not
    /// all arrived, and `Err` when what is buffered is not a frame at all.
    fn parse_buffered(&mut self) -> Result<Option<AMQPFrame>, ()> {
        if self.buffered.is_empty() {
            return Ok(None);
        }
        let parsed = match parse_frame(self.buffered.as_slice()) {
            Ok((rest, frame)) => Some((self.buffered.len() - rest.len(), frame)),
            Err(error) => {
                if error.is_incomplete() {
                    None
                } else {
                    return Err(());
                }
            }
        };
        match parsed {
            Some((consumed, frame)) => {
                self.buffered.drain(..consumed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}
