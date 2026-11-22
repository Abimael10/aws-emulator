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
//! session in the shared store, a SUBSCRIBE records its topic filters and is answered with
//! what the topics they cover have retained, a PUBLISH is handed to every session
//! subscribed to that topic (and becomes that topic's retained message when it carried the
//! RETAIN flag), a PINGREQ is answered, and the connection closing marks the session
//! disconnected with the reason it closed for. Every publication this wire serves is also
//! EVALUATED by the account's topic rules ([`super::served_publication`]) — including one
//! the client addressed to a rule by name, which is published to no subscriber at all
//! ([`super::basic_ingest`]). The broker PUBLISHES about the session as
//! well — its presence events, and the will the client left for a termination that was not
//! its own DISCONNECT ([`super::lifecycle`]). Every fact the IoT data plane then reports
//! about that client — connected, since when, which filters, what is queued — is read from
//! the session this wire wrote.
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
    endpoint, lifecycle, now_millis, Delivery, DirectMessage, LiveConnection, MqttBrokerState,
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
    /// The IoT COMMANDS plane this endpoint's reserved `$aws/commands/*` topics are
    /// served from ([`super::commands`]), or `None` for an engine that vends no such
    /// stores — in which case those topics carry nothing and every other topic is served
    /// exactly as before.
    pub(crate) commands: Option<super::commands::CommandsPlane>,
}

impl NativeHandler for MqttEndpoint {
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let state = self.state.clone();
        let upgrader = self.upgrader.clone();
        let published = Arc::clone(&self.published);
        let callback = self.callback.clone();
        let commands = self.commands.clone();
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
                serve_mqtt(state, published, commands, stream).await;
            } else {
                super::https_plane::serve(callback, stream).await;
            }
        })
    }
}

/// The reasons a session carries once it has ended — the client's own DISCONNECT, a
/// connection that ended without one, and a client id another connection took over. One
/// vocabulary for the session record and for the disconnect event that reports it, so both
/// are read from [`lifecycle`].
pub use lifecycle::{CLIENT_INITIATED_DISCONNECT, CONNECTION_LOST, DUPLICATE_CLIENTID};

/// What opening one session left for the connection to do once the store is released: the
/// connection it is, what its CONNACK says, and what the broker publishes about it.
struct SessionOpened {
    /// Which connection of this broker opened it — see [`LiveConnection::id`].
    connection_id: u64,
    /// Whether the session continues a stored one, which is what `sessionPresent` says.
    session_present: bool,
    /// The session's own identifier, which its lifecycle events are about.
    identifier: String,
    /// The address the client connected from, where this emulator knows it: the connect
    /// event's `ipAddress`.
    source_ip: Option<String>,
    /// The `versionNumber` the connect event carries.
    version_number: i64,
    /// The session this CONNECT took the client id over from, when it displaced a live one:
    /// its end is published before this session's connect event.
    displaced: Option<lifecycle::EndedSession>,
}

/// Serve one MQTT connection: open its session, then answer its packets until it closes.
async fn serve_mqtt(
    state: ::engine::ServiceState<MqttBrokerState>,
    published: Arc<OnceLock<String>>,
    commands: Option<super::commands::CommandsPlane>,
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
    if connect
        .last_will
        .as_ref()
        .is_some_and(|will| will.payload.len() > MAX_PAYLOAD_BYTES)
    {
        // A will larger than the broker's own message quota is a publication the broker
        // could never make, so there is no session to open holding it — and MQTT 3.1.1
        // gives a server no CONNACK code for it, exactly as it gives none for the
        // over-size PUBLISH this ends the connection like.
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
            let mut resumed = held.sessions.remove(&client_id);
            // What the displaced session's own end publishes: its disconnect event names
            // the takeover, and the will it left is published for it, because a client id
            // taken over is not the client saying goodbye. Read here, under the guard, and
            // published once it is released; the connection it held is closed with it.
            let mut displaced = None;
            if let Some(session) = resumed.as_mut().filter(|session| session.connected) {
                displaced = Some(lifecycle::EndedSession::of(
                    session,
                    DUPLICATE_CLIENTID,
                    true,
                ));
                if let Some(live) = session.disconnect(DUPLICATE_CLIENTID, opened_at) {
                    live.close();
                }
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
            // This CONNECTION's own session identifier, which its lifecycle events are
            // about: a client id a second connection takes over is a second session, and
            // the identifier is what tells them apart.
            session.session_identifier = lifecycle::new_session_identifier();
            // The will this CONNECT configured, which replaces whatever the session it
            // continues configured: a will belongs to the connection that left it.
            session.will = connect.last_will.clone().map(|will| *will);
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
            let opening = SessionOpened {
                connection_id: id,
                session_present,
                identifier: session.session_identifier.clone(),
                source_ip: session.source_ip.clone(),
                version_number: session.lifecycle_version,
                displaced,
            };
            session.lifecycle_version = session.lifecycle_version.saturating_add(1);
            held.sessions.insert(client_id.clone(), session);
            Some((opening, queued))
        }
    };
    let Some((opening, queued)) = opened else {
        // The protocol's own "server unavailable" — what a broker at its connection quota
        // answers — and no session was opened.
        let _ = write_connack(version, &mut writer, protocol::Connack::ServerUnavailable).await;
        return;
    };
    let connection_id = opening.connection_id;

    if !write_connack(
        version,
        &mut writer,
        protocol::Connack::Accepted {
            session_present: opening.session_present,
        },
    )
    .await
    {
        close_session(&state, &client_id, connection_id, CONNECTION_LOST);
        return;
    }

    // What the broker PUBLISHES about this connection, now that the store is no longer
    // held: the displaced session's own end (its takeover event and the will it left),
    // then this session's connect event.
    if let Some(displaced) = &opening.displaced {
        lifecycle::publish_session_ended(&state, displaced, opened_at);
    }
    lifecycle::publish_connected(
        &state,
        &client_id,
        &opening.identifier,
        opening.source_ip.as_deref(),
        opening.version_number,
        opened_at,
    );

    // What the session was holding for this client while it was away is written to it now,
    // which is what a persistent session is FOR — each under its own packet identifier,
    // as a redelivery on a resumed session carries.
    let mut next_packet_id: u16 = 1;
    for message in queued {
        let packet_id = next_packet_id;
        next_packet_id = next_packet_id.checked_add(1).unwrap_or(1);
        // A redelivery is a message forwarded by ordinary publishing, so its RETAIN flag
        // is clear: only the push that answers a new subscription carries one.
        if !write_publish(version, &mut writer, &message, packet_id, false).await {
            close_session(&state, &client_id, connection_id, CONNECTION_LOST);
            return;
        }
    }

    let reason = serve_session(
        version,
        &state,
        commands.as_ref(),
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
    commands: Option<&super::commands::CommandsPlane>,
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
                if !write_publish(version, writer, &delivery.message, packet_id, false).await {
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
                // The whole publication: for an ordinary topic the RETAIN flag the client
                // set makes this the topic's retained message (and an empty retained
                // payload clears it) and the message goes to the topic's subscribers either
                // way, while one the client addressed to a rule by name
                // (`$aws/rules/<ruleName>/<topic>`) is published to nobody at all — and
                // both are then evaluated by the account's topic rules.
                super::served_publication(state, &message).await;
                // A publication on a topic the broker RESERVES for one of its own
                // exchanges is served as well as carried: a device's report on a command
                // execution's reserved response topic is applied to that execution and
                // answered on the execution's own `accepted` / `rejected` topic
                // ([`super::commands`]). Every other topic is carried and nothing more.
                if let Some(answer) = commands
                    .and_then(|commands| commands.apply_report(&message.topic, &message.payload))
                {
                    super::publish_to_subscribers(state, &answer);
                }
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
                // The DISTINCT filters this SUBSCRIBE subscribed, each with the QoS it was
                // granted: at most [`MAX_SUBSCRIPTIONS_PER_SESSION`] of them, however many
                // entries the packet repeated, which is what bounds the retained-message
                // matching below and the reserved-topic replay after it.
                let mut subscribed: ::std::collections::BTreeMap<String, i32> =
                    ::std::collections::BTreeMap::new();
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
                        let subscribed_qos = subscribed.entry(filter.path.clone()).or_insert(qos);
                        *subscribed_qos = (*subscribed_qos).max(qos);
                    }
                }
                if !write_packet(writer, |buffer| {
                    protocol::render_suback(version, buffer, packet_id, &granted)
                })
                .await
                {
                    return Some(CONNECTION_LOST);
                }
                // What the new subscriptions are answered with out of the retain store: the
                // topics they cover, matched OFF the store's lock over the bounded list of
                // retained topics ([`MqttBrokerState::retained_topics`]) and over the
                // DISTINCT filters above — so one packet cannot make the broker walk the
                // store once per entry it repeated, and no caller-chosen batch of filters is
                // walked with the store held.
                let answered = if subscribed.is_empty() {
                    Vec::new()
                } else {
                    retained_topics_answering(state.lock().retained_topics(), &subscribed)
                };
                // Each retained message is read on its own and written with the RETAIN flag
                // SET: that is what tells the client the message was retained for it rather
                // than published while it was listening (MQTT 3.1.1 section 3.3.1.3), and
                // reading one at a time is what keeps one packet from making the broker hold
                // a copy of the whole store.
                for (topic, qos) in answered {
                    let Some(mut message) = state.lock().retained_messages.get(&topic).cloned()
                    else {
                        // The topic was cleared between the SUBACK and this write, so there
                        // is no retained message for it any more.
                        continue;
                    };
                    message.qos = message.qos.min(qos);
                    let delivered_id = next_packet_id;
                    next_packet_id = next_packet_id.checked_add(1).unwrap_or(1);
                    if !write_publish(version, writer, &message, delivered_id, true).await {
                        return Some(CONNECTION_LOST);
                    }
                }
                // A filter this packet subscribed over one of the broker's own reserved
                // exchanges may already be owed messages: a command execution created for
                // this device before it subscribed is delivered now, which is what makes
                // the order of a device's own calls immaterial ([`super::commands`]).
                // Published after the SUBACK, so the client is told its subscription
                // exists before anything arrives on it.
                //
                // BOUNDED as a PRODUCT: it runs over the DISTINCT filters above (at most
                // [`MAX_SUBSCRIPTIONS_PER_SESSION`], whatever the packet repeated) and one
                // allowance of messages and of payload bytes is spent across all of them
                // ([`super::commands::ReplayBudget`]); the fan-out yields between
                // deliveries, so it stays cancellable.
                if let Some(commands) = commands {
                    let mut budget = super::commands::ReplayBudget::of_one_subscribe();
                    for path in subscribed.keys() {
                        if budget.is_spent() {
                            break;
                        }
                        for message in commands.pending_requests(path, &mut budget) {
                            super::publish_to_subscribers(state, &message);
                            ::tokio::task::yield_now().await;
                        }
                    }
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

/// Which of `topics` a SUBSCRIBE that subscribed `subscribed` (each distinct filter with the
/// QoS it was granted) is answered with, and the QoS each is sent at: the highest QoS a
/// covering filter was granted, so a topic several filters of one packet cover is answered
/// ONCE.
///
/// What one SUBSCRIBE costs is the product of two bounds and nothing else: the retained
/// topics the store holds ([`super::MAX_RETAINED_TOPICS`]) and the DISTINCT filters a session
/// may subscribe ([`MAX_SUBSCRIPTIONS_PER_SESSION`]) — not the filter entries the packet
/// carried, which a client chooses. It runs off the store's lock, on the connection's own
/// task, over the topic list copied out of it.
fn retained_topics_answering(
    topics: Vec<String>,
    subscribed: &::std::collections::BTreeMap<String, i32>,
) -> Vec<(String, i32)> {
    topics
        .into_iter()
        .filter_map(|topic| {
            subscribed
                .iter()
                .filter(|(filter, _qos)| super::topic_filter_covers(filter, &topic))
                .map(|(_filter, qos)| *qos)
                .max()
                .map(|qos| (topic, qos))
        })
        .collect()
}

/// Record that this connection's client is gone, with the reason it went, and publish what
/// the end of that session publishes: its disconnect event, and the will the client left
/// for any termination but its own DISCONNECT.
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
    let at_millis = now_millis();
    // The record is written under the guard and what it publishes is read off it there;
    // the publication itself is made once the guard is gone, because a delivery is never
    // handed out with the store held.
    let ended = {
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
        let ended = lifecycle::EndedSession::of(session, reason, true);
        session.disconnect(reason, at_millis);
        // A client that connected with a clean session keeps no state past its connection.
        if session.clean_session {
            session.subscriptions.clear();
            session.queued_messages.clear();
        }
        ended
    };
    lifecycle::publish_session_ended(state, &ended, at_millis);
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
///
/// `retain` is this DELIVERY's RETAIN flag: set on the push that answers a new subscription
/// with what its topics retained, and clear on a message forwarded by ordinary publishing.
async fn write_publish<W: AsyncWrite + Unpin>(
    version: protocol::Version,
    writer: &mut W,
    message: &DirectMessage,
    packet_id: u16,
    retain: bool,
) -> bool {
    let now = now_millis();
    write_packet(writer, |buffer| {
        protocol::render_publish(version, buffer, message, packet_id, retain, now)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The filters a SUBSCRIBE subscribed, as the broker holds them: distinct, each with the
    /// QoS it was granted.
    fn subscribed(
        filters: &[(&str, i32)],
    ) -> ::std::collections::BTreeMap<String, i32> {
        filters
            .iter()
            .map(|(filter, qos)| ((*filter).to_string(), *qos))
            .collect()
    }

    /// One row of the retained-answer cases: what it is, the filters the SUBSCRIBE
    /// subscribed with the QoS each was granted, and the topics it is answered with.
    type RetainedAnswerRow = (
        &'static str,
        &'static [(&'static str, i32)],
        &'static [(&'static str, i32)],
    );

    /// RULE: a new subscription is answered with the retained message of every topic its
    /// filters cover, ONCE per topic and at the highest QoS a covering filter was granted —
    /// and the matching is over the DISTINCT filters the session subscribed, so what one
    /// SUBSCRIBE costs does not grow with the entries its packet repeated.
    #[test]
    fn a_subscription_is_answered_with_each_covered_retained_topic_once() {
        let topics: Vec<String> = ["devices/sensor-1/state", "devices/sensor-2/state", "fleet/state"]
            .iter()
            .map(|topic| (*topic).to_string())
            .collect();
        // The rows span what the matching generalises over: one filter covering a subset, a
        // filter covering a topic another filter covers too (answered once, at the higher
        // granted QoS), the whole-tree filter, a filter covering nothing retained, and a
        // SUBSCRIBE that subscribed nothing at all.
        let rows: &[RetainedAnswerRow] = &[
            (
                "one wildcard filter",
                &[("devices/#", 1)],
                &[("devices/sensor-1/state", 1), ("devices/sensor-2/state", 1)],
            ),
            (
                "two filters covering one topic",
                &[("devices/#", 0), ("devices/sensor-1/state", 1)],
                &[("devices/sensor-1/state", 1), ("devices/sensor-2/state", 0)],
            ),
            (
                "the whole-tree filter",
                &[("#", 1)],
                &[
                    ("devices/sensor-1/state", 1),
                    ("devices/sensor-2/state", 1),
                    ("fleet/state", 1),
                ],
            ),
            ("a filter covering nothing retained", &[("orders/+", 1)], &[]),
            ("nothing subscribed", &[], &[]),
        ];
        for (case, filters, expected) in rows {
            let answered = retained_topics_answering(topics.clone(), &subscribed(filters));
            let expected: Vec<(String, i32)> = expected
                .iter()
                .map(|(topic, qos)| ((*topic).to_string(), *qos))
                .collect();
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
