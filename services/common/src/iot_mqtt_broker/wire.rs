// SPDX-License-Identifier: CC0-1.0

//! The broker's WIRE: what one connection to the published endpoint is served with.
//!
//! The endpoint serves two protocols, and byte 0 says which (after the core has
//! terminated TLS for a client that dialled it, [`crate::in_band_tls`]): an MQTT CONNECT
//! opens an MQTT session here, and anything else is an HTTP request this emulator's own
//! router answers ([`super::https_plane`]) — the two wires AWS serves on an account's data
//! endpoint host.
//!
//! The MQTT half is a BROKER, not a transcript: a CONNECT opens or resumes the client's
//! session in the shared store, a SUBSCRIBE records its topic filters, a PUBLISH is handed
//! to every session subscribed to that topic, a PINGREQ is answered, and the connection
//! closing marks the session disconnected with the reason it closed for. Every fact the
//! IoT data plane then reports about that client — connected, since when, which filters,
//! what is queued — is read from the session this wire wrote.
//!
//! The protocol itself is read and written with the maintained MQTT codec
//! ([`::mqttbytes`]): this module decides what the broker DOES with each packet, not how a
//! packet is laid out. BOTH versions AWS IoT Core's message broker serves are served here —
//! MQTT 3.1.1 and MQTT 5, the one a connection speaks being the one its own CONNECT
//! announces ([`protocol`]) — and a client announcing a version this broker serves no
//! protocol for is answered with the CONNACK the protocol reserves for exactly that,
//! because a CONNACK of a version the client did not speak is unreadable to it.
//!
//! Bounds: the core owns the socket, the connection cap, the handshake deadline and the
//! idle timeout, so a connection that goes silent is dropped without this module timing
//! it. What this module bounds is what ONE connection may make the process hold: a packet
//! larger than [`MAX_PACKET_BYTES`] ends the connection, and a client that stops reading
//! its socket collects at most [`super::MAX_PENDING_DELIVERIES`] deliveries before the
//! rest go onto its session's own bounded queue.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use ::bytes::BytesMut;
use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};
use ::mqttbytes::QoS;
use ::smithy_server_runtime::EmbeddedEmulator;
use ::tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use ::tokio::sync::mpsc::Receiver;
use ::tokio::sync::Notify;

use super::{
    endpoint, now_millis, Delivery, DirectMessage, LiveConnection, MqttBrokerState,
    MAX_PENDING_DELIVERIES,
};

pub mod protocol;

/// The first byte of an MQTT CONNECT: packet type 1 in the high nibble, with no flags —
/// the only packet a client may open an MQTT connection with. A TLS record opens with
/// `0x16` and an HTTP request with a method's first letter, so one byte tells the three
/// apart.
pub const MQTT_CONNECT_FIRST_BYTE: u8 = 0x10;

/// Which MQTT versions this broker speaks, and the level each announces — see
/// [`protocol::Version`].
pub use protocol::{Version, PROTOCOL_LEVEL_V311, PROTOCOL_LEVEL_V5};

/// The most bytes one MQTT packet may carry on this endpoint.
///
/// The AWS IoT Core message-broker quota "Maximum size of a message" is 128 KiB of
/// payload; this is that payload with room for the packet's own topic, properties and
/// framing around it, so a publication the HTTP data plane accepts is one this wire
/// accepts too, and nothing larger is buffered per connection.
pub const MAX_PACKET_BYTES: usize = 256 * 1024;

/// How many sessions the broker holds at once.
///
/// CHOSEN as a process bound, not an IoT Core quota: a client decides how many client ids
/// it connects under and a disconnected client's record is kept for as long as the service
/// reports it, so without a ceiling a sequence of connections would size this emulator's
/// store. A CONNECT that arrives when the table is full is refused rather than served.
pub const MAX_SESSIONS: usize = 1024;

/// How many topic filters one session may subscribe to.
///
/// The AWS IoT Core message-broker quota "Maximum subscriptions per session" is 50, so a
/// SUBSCRIBE asking for one past it is refused for that filter — a SUBACK may grant some
/// of its filters and refuse others, which is what its per-filter return codes are for —
/// and one connection cannot grow its session's own table without limit.
pub const MAX_SUBSCRIPTIONS_PER_SESSION: usize = 50;

/// The most bytes one topic filter may carry.
///
/// The AWS IoT Core message-broker quota "Topic size" is 256 bytes of UTF-8 encoded topic,
/// and a filter names topics, so one longer than a topic can be matches none and is
/// refused — which also bounds what one subscription costs this emulator.
pub const MAX_TOPIC_FILTER_BYTES: usize = 256;

/// The most PAYLOAD bytes one publication may carry: the AWS IoT Core message-broker quota
/// "Maximum size of a message" (128 KiB), which is the same ceiling the HTTP data plane
/// holds a `Publish` to.
pub const MAX_PAYLOAD_BYTES: usize = 128 * 1024;

/// How many bytes are read off the socket at once.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// The highest QoS this broker grants a subscription and delivers at.
///
/// AWS IoT Core's message broker "supports QoS levels 0 and 1" and does not support QoS 2
/// ("Quality of Service (QoS) options", AWS IoT Core developer guide), so a subscription
/// asking for 2 is granted 1 — the maximum the server supports, which is what a SUBACK
/// granting a lower QoS than the client asked for says.
pub const MAX_GRANTED_QOS: i32 = 1;

/// The endpoint's connection handler: one call per connection the core accepts, and one
/// per connection an in-process client opens to the published address.
pub struct MqttEndpoint {
    /// The broker's store — the sessions this wire opens and reports into.
    pub(crate) state: ::engine::ServiceState<MqttBrokerState>,
    /// The core's TLS server, for the half of the address a client dials with TLS.
    pub(crate) upgrader: Option<TlsUpgrader>,
    /// The `host:port` this endpoint is published at, written once the core has bound the
    /// port: a session's TARGET socket information is the address its client reached.
    pub(crate) published: Arc<OnceLock<String>>,
    /// The emulator this endpoint's HTTP half is served by — the same router, engine and
    /// state as the call that opened it.
    pub(crate) callback: EmbeddedEmulator,
}

impl NativeHandler for MqttEndpoint {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let state = self.state.clone();
        let upgrader = self.upgrader.clone();
        let published = Arc::clone(&self.published);
        let callback = self.callback.clone();
        Box::pin(async move {
            let Some(mut conn) = crate::in_band_tls::negotiated(conn, upgrader).await else {
                return;
            };
            // Byte 0 of the decrypted stream says which wire the client speaks, and is
            // replayed to whoever reads it next — the MQTT codec needs the packet's own
            // first byte, and an HTTP parser the method's.
            let mut first = [0u8; 1];
            if !matches!(conn.read(&mut first).await, Ok(1)) {
                return;
            }
            let stream: ListenerConn =
                Box::new(crate::in_band_tls::Prefixed::new(first.to_vec(), conn));
            if first[0] == MQTT_CONNECT_FIRST_BYTE {
                serve_mqtt(state, published, stream).await;
            } else {
                super::https_plane::serve(callback, stream).await;
            }
        })
    }
}

/// The reason a session carries once its client's own DISCONNECT closed the connection.
/// One of the AWS IoT Core lifecycle-event `disconnectReason` values.
pub const CLIENT_INITIATED_DISCONNECT: &str = "CLIENT_INITIATED_DISCONNECT";

/// The reason a session carries when the connection ended without a DISCONNECT.
pub const CONNECTION_LOST: &str = "CONNECTION_LOST";

/// Serve one MQTT connection: open its session, then answer its packets until it closes.
async fn serve_mqtt(
    state: ::engine::ServiceState<MqttBrokerState>,
    published: Arc<OnceLock<String>>,
    conn: ListenerConn,
) {
    let (mut reader, mut writer) = ::tokio::io::split(conn);
    let mut buffer = BytesMut::with_capacity(READ_CHUNK_BYTES);

    // The CONNECT: until it has been read there is no session, and it is also what says
    // WHICH version of MQTT the rest of this connection is spoken in.
    let (version, connect) = match read_connect(&mut reader, &mut buffer).await {
        Some(Ok(opened)) => opened,
        Some(Err(version)) => {
            // A CONNECT whose version this broker serves no protocol for, and one the
            // version's own codec could not read, are answered with the refusal the
            // protocol reserves for exactly that.
            let _ = write_connack(
                version,
                &mut writer,
                protocol::Connack::UnsupportedProtocolVersion,
            )
            .await;
            return;
        }
        None => return,
    };

    let client_id = connect.client_id.clone();
    if client_id.is_empty() {
        // A session is keyed by its client id, so a connection that names none cannot be
        // reported on at all: MQTT's own refusal for an unusable client id.
        let _ = write_connack(version, &mut writer, protocol::Connack::ClientIdNotValid).await;
        return;
    }

    let deliveries = ::tokio::sync::mpsc::channel::<Delivery>(MAX_PENDING_DELIVERIES);
    let (handed, mut arriving) = deliveries;
    let closing = Arc::new(Notify::new());
    let opened_at = now_millis();
    let target = published.get().and_then(|authority| endpoint::target_of(authority));

    // The session table is BOUNDED: a client chooses how many client ids it connects
    // under, and a disconnected client's record is kept for as long as the service reports
    // it, so without a ceiling a sequence of connections would size the broker's store.
    // The admission and the insertion are decided under ONE guard, so concurrent CONNECTs
    // cannot each pass the check and push the table past the bound; the CONNACK either way
    // is written after it, because no lock is held across a write.
    let opened = {
        let mut held = state.lock();
        // The sessions the clock has already retired are released first, so a record the
        // service no longer reports holds no place in the table.
        held.release_retired_sessions(opened_at);
        if held.sessions.len() >= MAX_SESSIONS && !held.sessions.contains_key(&client_id) {
            None
        } else {
            let id = held.accept_connection();
            let connection = LiveConnection {
                id,
                deliveries: handed,
                closing: Arc::clone(&closing),
            };
            // A client that connects again under a client id the broker already holds a
            // live connection for TAKES OVER: MQTT requires the existing connection to be
            // closed, and the session it leaves behind is the one this connection
            // continues.
            let resumed = held.sessions.remove(&client_id);
            if let Some(displaced) = resumed
                .as_ref()
                .and_then(|session| session.connection.clone())
            {
                displaced.close();
            }
            // `cleanSession` decides whether the stored session is continued or
            // discarded: a clean session starts with no subscriptions and nothing queued,
            // and a persistent one resumes both — which is what `sessionPresent` tells the
            // client. State is kept only when the connection that left it asked for a
            // persistent session AND this CONNECT does not ask for a clean one; a
            // disconnected client's record is still REPORTED either way, which is what
            // the connection operations read.
            let continued =
                resumed.filter(|stored| !connect.clean_session && !stored.clean_session);
            let session_present = continued.is_some();
            let mut session = continued.unwrap_or_default();
            session.client_id = client_id.clone();
            session.connected = true;
            session.clean_session = connect.clean_session;
            session.keep_alive_seconds = connect.keep_alive_seconds;
            // The Session Expiry Interval an MQTT 5 CONNECT configured, which is a fact
            // about this connection; a 3.1.1 client's protocol carries none, so it
            // configures none.
            session.session_expiry_seconds = connect.session_expiry_seconds;
            session.connected_since_millis = opened_at;
            session.disconnected_since_millis = 0;
            session.disconnect_reason = None;
            // The address the client reached the broker AT, which is a fact about this
            // connection.
            if let Some((host, port)) = target.clone() {
                session.target_ip = Some(host);
                session.target_port = port;
            }
            // The address it connected FROM. A client that stated one — an in-process
            // client of this emulator — is taken at its word, with the port it stated.
            // For a connection the core accepted on a socket, the interface the listener
            // is BOUND on decides it: a listener on the loopback interface can only have
            // been reached from that same interface, so that is the address this
            // connection came from. The core hands a handler no peer, so a listener an
            // operator bound on another interface — reachable from any address — has none
            // to report, and the port of a socket connection is not knowable here either.
            match crate::dialable_data_planes::connected_from() {
                Some(from) => {
                    session.source_ip = Some(from.ip().to_string());
                    session.source_port = i32::from(from.port());
                }
                None => {
                    session.source_ip = target
                        .map(|(host, _port)| host)
                        .filter(|host| ::emulator_server::is_loopback_host(host));
                }
            }
            session.connection = Some(connection);
            let queued = std::mem::take(&mut session.queued_messages);
            held.sessions.insert(client_id.clone(), session);
            Some((id, session_present, queued))
        }
    };
    let Some((connection_id, session_present, queued)) = opened else {
        // The protocol's own "server unavailable" — what a broker at its connection quota
        // answers — and no session was opened.
        let _ = write_connack(version, &mut writer, protocol::Connack::ServerUnavailable).await;
        return;
    };

    if !write_connack(
        version,
        &mut writer,
        protocol::Connack::Accepted { session_present },
    )
    .await
    {
        close_session(&state, &client_id, connection_id, CONNECTION_LOST);
        return;
    }

    // What the session was holding for this client while it was away is written to it now,
    // which is what a persistent session is FOR — each under its own packet identifier,
    // as a redelivery on a resumed session carries.
    let mut next_packet_id: u16 = 1;
    for message in queued {
        let packet_id = next_packet_id;
        next_packet_id = next_packet_id.checked_add(1).unwrap_or(1);
        if !write_publish(version, &mut writer, &message, packet_id).await {
            close_session(&state, &client_id, connection_id, CONNECTION_LOST);
            return;
        }
    }

    let reason = serve_session(
        version,
        &state,
        &client_id,
        connection_id,
        &closing,
        &mut reader,
        &mut writer,
        &mut buffer,
        &mut arriving,
        next_packet_id,
    )
    .await;
    if let Some(reason) = reason {
        close_session(&state, &client_id, connection_id, reason);
    }
}

/// Answer one open session's packets, and hand it the deliveries the broker makes to it,
/// until the connection ends.
///
/// The reason the session is left with, or `None` when the session is no longer this
/// connection's to write — a client id taken over by a later connection, or a session the
/// IoT data plane's `DeleteConnection` has already closed and recorded its own reason for.
#[allow(clippy::too_many_arguments)]
async fn serve_session<R, W>(
    version: protocol::Version,
    state: &::engine::ServiceState<MqttBrokerState>,
    client_id: &str,
    connection_id: u64,
    closing: &Notify,
    reader: &mut R,
    writer: &mut W,
    buffer: &mut BytesMut,
    arriving: &mut Receiver<Delivery>,
    first_packet_id: u16,
) -> Option<&'static str>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // The packet identifier the next delivery at QoS 1 carries: non-zero, and distinct
    // across the deliveries in flight on this one connection, which is all a packet
    // identifier has to be.
    let mut next_packet_id: u16 = first_packet_id.max(1);
    // The callers waiting for the client to acknowledge a delivery, by the packet
    // identifier it was written under. Bounded by [`MAX_PENDING_DELIVERIES`], which is how
    // many deliveries one connection can have in flight.
    let mut awaiting: ::std::collections::HashMap<u16, ::tokio::sync::oneshot::Sender<()>> =
        ::std::collections::HashMap::new();
    loop {
        let packet = ::tokio::select! {
            // The service closed this connection (`DeleteConnection`): the record already
            // says so and carries its own reason, so nothing is written over it here.
            () = closing.notified() => return None,
            // A message the broker handed this client: written to it in the order the
            // broker handed them over.
            delivery = arriving.recv() => {
                // The session this connection belongs to is gone from the store when the
                // deliveries end, so there is nothing left to serve and nothing to record.
                let delivery = delivery?;
                let packet_id = next_packet_id;
                next_packet_id = next_packet_id.checked_add(1).unwrap_or(1);
                // A delivery somebody is waiting on is acknowledged by the CLIENT's own
                // PUBACK, so the waiter is kept until that arrives (and dropped with the
                // connection, which is what tells a waiter it never will). Only a delivery
                // made at QoS 1 is acknowledged at all.
                let confirmed = delivery.message.qos >= MAX_GRANTED_QOS;
                if let Some(acknowledged) = delivery.acknowledged.filter(|_| confirmed) {
                    awaiting.insert(packet_id, acknowledged);
                }
                if !write_publish(version, writer, &delivery.message, packet_id).await {
                    return Some(CONNECTION_LOST);
                }
                continue;
            }
            packet = read_packet(version, reader, buffer) => packet,
        };
        let Some(packet) = packet else {
            return Some(CONNECTION_LOST);
        };
        match packet {
            // A publication: handed to every session subscribed to its topic, and
            // acknowledged to the publisher when it asked for an acknowledgement. The
            // message is the one the client sent, message properties and all: an MQTT 5
            // subscriber of that topic is handed each of them.
            protocol::ClientPacket::Publish {
                mut message,
                packet_id,
            } => {
                if message.qos > MAX_GRANTED_QOS {
                    // AWS IoT Core supports QoS 0 and 1 only, so there is no exchange
                    // this broker could complete for a QoS 2 publication.
                    return Some(CONNECTION_LOST);
                }
                if !::mqttbytes::valid_topic(&message.topic) {
                    // A PUBLISH whose topic is no topic NAME — it is empty, or carries a
                    // subscription wildcard, which only a filter may — is a protocol
                    // violation, and MQTT 3.1.1 gives a server no way to refuse one
                    // packet: the connection ends.
                    return Some(CONNECTION_LOST);
                }
                if message.payload.len() > MAX_PAYLOAD_BYTES {
                    // Larger than the broker's own message quota, so there is no
                    // publication to make; the packet was framed, so the connection ends
                    // rather than the message being delivered in part.
                    return Some(CONNECTION_LOST);
                }
                let at_least_once = message.qos >= MAX_GRANTED_QOS;
                message.delivered_at_millis = now_millis();
                super::publish_to_subscribers(state, &message);
                if at_least_once
                    && !write_packet(writer, |buffer| {
                        protocol::render_puback(version, buffer, packet_id)
                    })
                    .await
                {
                    return Some(CONNECTION_LOST);
                }
            }
            // The client acknowledged a delivery: the caller that asked to hear about it
            // is told. Nothing is retransmitted by this broker, so the acknowledgement
            // completes the exchange and is recorded nowhere else.
            protocol::ClientPacket::PubAck { packet_id } => {
                if let Some(waiting) = awaiting.remove(&packet_id) {
                    let _ = waiting.send(());
                }
            }
            protocol::ClientPacket::Subscribe { packet_id, filters } => {
                let mut granted = Vec::with_capacity(filters.len());
                {
                    let mut held = state.lock();
                    // The session this connection opened, while it is still this
                    // connection's: a client id a later connection has taken over belongs
                    // to that connection, and there is nothing for this one to record.
                    let session = held.sessions.get_mut(client_id)?;
                    if !session
                        .connection
                        .as_ref()
                        .is_some_and(|connection| connection.id == connection_id)
                    {
                        return None;
                    }
                    for filter in &filters {
                        // A filter longer than a topic can be, one that is no filter at
                        // all, and one past the session's subscription quota are each
                        // refused for that filter alone — which is what a SUBACK's
                        // per-filter return codes say, each naming its own reason where
                        // the version has one for it.
                        if filter.path.len() > MAX_TOPIC_FILTER_BYTES
                            || !::mqttbytes::valid_filter(&filter.path)
                        {
                            granted.push(protocol::Grant::TopicFilterInvalid);
                            continue;
                        }
                        if session.subscriptions.len() >= MAX_SUBSCRIPTIONS_PER_SESSION
                            && !session.subscriptions.contains_key(&filter.path)
                        {
                            granted.push(protocol::Grant::QuotaExceeded);
                            continue;
                        }
                        let qos = filter.qos.clamp(0, MAX_GRANTED_QOS);
                        session.subscriptions.insert(filter.path.clone(), qos);
                        granted.push(protocol::Grant::Qos(qos));
                    }
                }
                if !write_packet(writer, |buffer| {
                    protocol::render_suback(version, buffer, packet_id, &granted)
                })
                .await
                {
                    return Some(CONNECTION_LOST);
                }
            }
            protocol::ClientPacket::Unsubscribe { packet_id, filters } => {
                {
                    let mut held = state.lock();
                    let session = held.sessions.get_mut(client_id)?;
                    for filter in &filters {
                        session.subscriptions.remove(filter);
                    }
                }
                if !write_packet(writer, |buffer| {
                    protocol::render_unsuback(version, buffer, packet_id, filters.len())
                })
                .await
                {
                    return Some(CONNECTION_LOST);
                }
            }
            protocol::ClientPacket::PingReq => {
                if !write_packet(writer, |buffer| protocol::render_pingresp(version, buffer))
                    .await
                {
                    return Some(CONNECTION_LOST);
                }
            }
            protocol::ClientPacket::Disconnect => return Some(CLIENT_INITIATED_DISCONNECT),
            // A second CONNECT on an open connection, a packet only a server sends, and
            // one of an exchange this broker does not run (QoS 2): a client that sends one
            // is not speaking the protocol this endpoint serves.
            protocol::ClientPacket::Connect(_) | protocol::ClientPacket::Unserved => {
                return Some(CONNECTION_LOST)
            }
        }
    }
}

/// Record that this connection's client is gone, with the reason it went.
///
/// Only the session this connection still owns is written: a client id another connection
/// has taken over belongs to that connection, and a session the service disconnected
/// already carries its own reason.
fn close_session(
    state: &::engine::ServiceState<MqttBrokerState>,
    client_id: &str,
    connection_id: u64,
    reason: &str,
) {
    let mut held = state.lock();
    let Some(session) = held.sessions.get_mut(client_id) else {
        return;
    };
    if !session
        .connection
        .as_ref()
        .is_some_and(|connection| connection.id == connection_id)
    {
        return;
    }
    session.disconnect(reason, now_millis());
    // A client that connected with a clean session keeps no state past its connection.
    if session.clean_session {
        session.subscriptions.clear();
        session.queued_messages.clear();
    }
}

/// Read the opening CONNECT: the MQTT version it announces, and what it asks for.
///
/// The version is read off the packet's own bytes BEFORE any codec frames it
/// ([`protocol::announced_protocol_level`]), because the codec that frames a packet is the
/// one of the version the client speaks and each version's CONNACK is readable only to a
/// client of that version.
///
/// `Err(version)` is a connection there is no session to open for — a version this broker
/// serves no protocol for, or a CONNECT its own codec could not read — carrying the version
/// its refusal is to be written in. `None` is a connection there is nothing to answer at
/// all: the client closed it, the read failed, or it opened with some other packet.
async fn read_connect<R: AsyncRead + Unpin>(
    reader: &mut R,
    buffer: &mut BytesMut,
) -> Option<Result<(protocol::Version, protocol::Connect), protocol::Version>> {
    // Enough of the packet to carry the protocol level, read before a codec is chosen.
    let announced = loop {
        if let Some(level) = protocol::announced_protocol_level(buffer) {
            break protocol::Version::of_protocol_level(level);
        }
        if buffer.len() >= protocol::MAX_PROTOCOL_LEVEL_OFFSET {
            break None;
        }
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    };
    // A client of a version this broker serves no protocol for is refused the way MQTT
    // 3.1.1 does it: the shortest CONNACK, and the one a client of an unknown version has
    // the best chance of reading.
    let Some(version) = announced else {
        return Some(Err(protocol::Version::V311));
    };
    match read_packet(version, reader, buffer).await {
        Some(protocol::ClientPacket::Connect(connect)) => Some(Ok((version, connect))),
        Some(_not_a_connect) => None,
        None => Some(Err(version)),
    }
}

/// Read one packet off `reader` with `version`'s codec, buffering what arrives until the
/// codec can frame one.
///
/// `None` ends the connection: the client closed it, the read failed, or the bytes are no
/// packet this codec can read — including one larger than [`MAX_PACKET_BYTES`].
async fn read_packet<R: AsyncRead + Unpin>(
    version: protocol::Version,
    reader: &mut R,
    buffer: &mut BytesMut,
) -> Option<protocol::ClientPacket> {
    loop {
        match protocol::frame(version, buffer, MAX_PACKET_BYTES) {
            protocol::Framing::Packet(packet) => return Some(packet),
            protocol::Framing::Incomplete => {}
            protocol::Framing::Unreadable => return None,
        }
        // What one connection may make this process hold, stated here rather than left to
        // the codec: a client that keeps sending bytes the codec cannot frame yet is read
        // only up to one packet's worth.
        if buffer.len() >= MAX_PACKET_BYTES {
            return None;
        }
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
}

/// Write one packet, rendered by `render` into the codec's own buffer. `false` when the
/// connection could not take it, which ends the connection.
async fn write_packet<W, Render>(writer: &mut W, render: Render) -> bool
where
    W: AsyncWrite + Unpin,
    Render: FnOnce(&mut BytesMut) -> Result<usize, ::mqttbytes::Error>,
{
    let mut rendered = BytesMut::new();
    if render(&mut rendered).is_err() {
        return false;
    }
    writer.write_all(&rendered).await.is_ok() && writer.flush().await.is_ok()
}

/// Answer a CONNECT with its CONNACK, in the codes of the version the client speaks.
async fn write_connack<W: AsyncWrite + Unpin>(
    version: protocol::Version,
    writer: &mut W,
    outcome: protocol::Connack,
) -> bool {
    write_packet(writer, |buffer| {
        protocol::render_connack(version, buffer, outcome)
    })
    .await
}

/// Write one delivery to the client as a PUBLISH, at the QoS the delivery was made at and
/// with the message properties the client's own version can carry.
async fn write_publish<W: AsyncWrite + Unpin>(
    version: protocol::Version,
    writer: &mut W,
    message: &DirectMessage,
    packet_id: u16,
) -> bool {
    let now = now_millis();
    write_packet(writer, |buffer| {
        protocol::render_publish(version, buffer, message, packet_id, now)
    })
    .await
}

/// The QoS number a session records, which is what `ListSubscriptions` reports.
#[must_use]
pub fn qos_number(qos: QoS) -> i32 {
    match qos {
        QoS::AtMostOnce => 0,
        QoS::AtLeastOnce => 1,
        QoS::ExactlyOnce => 2,
    }
}

/// The QoS a number names, bounded by what this broker grants ([`MAX_GRANTED_QOS`]).
#[must_use]
pub fn qos_of(number: i32) -> QoS {
    if number >= MAX_GRANTED_QOS {
        QoS::AtLeastOnce
    } else {
        QoS::AtMostOnce
    }
}
