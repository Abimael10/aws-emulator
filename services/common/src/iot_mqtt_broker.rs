// SPDX-License-Identifier: CC0-1.0

//! The MQTT MESSAGE BROKER of this emulator's IoT surface: the sessions it holds, and the
//! address a device opens one at.
//!
//! An IoT device does not reach the broker by any API call: it discovers an address
//! (`DescribeEndpoint`), opens a transport connection to it and sends an MQTT CONNECT, and
//! every later fact about that client — that it is connected, which topic filters its
//! session subscribes to, what a message sent to it is handed to — follows from that one
//! live connection. The two emulated services on either side of it see different halves:
//! the IoT control plane PUBLISHES the address and the IoT data plane REPORTS on the
//! sessions (`GetConnection`, `ListSubscriptions`, `SendDirectMessage`,
//! `DeleteConnection`), and neither of them is the broker. So the broker is here: ONE
//! store in the shared engine, so a session a connection opened is the session the data
//! plane reports and the connection the data plane closes is the one that closes.
//!
//! What it is made of:
//!
//! * [`MqttSession`] — one client's session, which is what the connection operations
//!   answer from, and [`MqttBrokerState`] — the sessions by client id, plus the endpoint
//!   the broker answers at.
//! * [`wire`] — the protocol: a connection's CONNECT opens or resumes a session, its
//!   SUBSCRIBE records topic filters on it, its PUBLISH is handed to the sessions
//!   subscribed to that topic, and its close (or a `DeleteConnection`) marks the session
//!   disconnected.
//! * [`endpoint`] — the socket the core opens for it and the `host:port` that socket is
//!   published at, which is the address `DescribeEndpoint` answers with.
//!
//! LOCK ORDER: the store is taken LAST and never held across an await. A connection's own
//! task takes it to read or write a session and releases it before it writes a byte;
//! deliveries leave the lock as a handle ([`LiveConnection`]) that is used after the guard
//! is dropped.

pub mod endpoint;
pub mod https_plane;
pub mod wire;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use ::tokio::sync::mpsc::Sender;
use ::tokio::sync::oneshot;
use ::tokio::sync::Notify;

pub use endpoint::{ensure_broker_endpoint, published_broker_endpoint};
/// The MQTT codec this broker reads and writes the protocol with, re-exported: a client
/// of this emulator written in this workspace speaks the same wire through it rather than
/// through a second declaration of the codec.
pub use ::mqttbytes;

/// How long a disconnected client's connection information is still reported, in
/// MILLISECONDS.
///
/// Source: `GetConnectionResponse$disconnectedSince`'s own documentation — "This
/// information is available for 30 minutes after the client disconnects"
/// (`smithy_query(query="doc", shape="GetConnectionResponse", member="disconnectedSince")`
/// on the IoT data plane's model). Past it the session record is no longer something the
/// service reports, so the connection operations answer their not-found condition and the
/// record is released.
pub const DISCONNECT_INFORMATION_RETENTION_MILLIS: i64 = 30 * 60 * 1000;

/// How many messages one session queues for a client that is offline. The IoT Core
/// message-broker quota "Maximum number of queued messages per persistent session" is
/// 100, and a 101st delivery pushes the oldest out, as a broker's bounded queue does.
pub const MAX_QUEUED_SESSION_MESSAGES: usize = 100;

/// How many payload BYTES one session queues for a client that is offline.
///
/// CHOSEN as a process bound, not an IoT Core quota: a message count alone leaves the
/// product of the count and the message size unbounded, so a publisher could size this
/// process's memory with large payloads on a filter many offline sessions subscribe to.
/// With it, what the whole store can hold is this times the session table's own ceiling
/// ([`wire::MAX_SESSIONS`]). The oldest message leaves first, exactly as it does at the
/// message count.
pub const MAX_QUEUED_SESSION_BYTES: usize = 256 * 1024;

/// How many deliveries one LIVE connection may have waiting to be written before the
/// broker stops handing it more.
///
/// CHOSEN as a process bound, not an IoT Core quota: a connection that stops reading its
/// socket would otherwise let publishers size this process's memory. A delivery that finds
/// the channel full is queued on the session instead ([`MqttSession::deliver`]), which is
/// the same place a delivery to an offline client goes.
const MAX_PENDING_DELIVERIES: usize = 64;

/// One message delivered to a client, kept verbatim: the message a session queues for a
/// client that is offline is exactly the bytes and MQTT5 properties that were sent, and so
/// is the one a live connection is handed.
#[derive(Debug, Clone, Default)]
pub struct DirectMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    /// The QoS the delivery used: 1 when the sender asked for confirmation, else 0 —
    /// what `SendDirectMessageRequest$confirmation` documents.
    pub qos: i32,
    pub content_type: Option<String>,
    pub response_topic: Option<String>,
    pub user_properties: Option<String>,
    pub payload_format_indicator: Option<String>,
    /// The correlation data, BASE64-encoded — the form the HTTP data plane carries it in
    /// ("The base64-encoded binary data used by the sender of the request message to
    /// identify which request the response message is for",
    /// `PublishRequest$correlationData`). On the MQTT wire the property is raw bytes, so
    /// the wire decodes this on its way out and encodes what a publisher sent on its way
    /// in; one form is kept, so a message read back is the message that was sent.
    pub correlation_data: Option<String>,
    /// The message expiry interval the publisher set, in SECONDS, or `None` for a message
    /// that does not expire ("A user-defined integer value that represents the message
    /// expiry interval in seconds. If absent, the message doesn't expire",
    /// `PublishRequest$messageExpiry`). What a subscriber is FORWARDED is this less the
    /// time the message waited here — see [`DirectMessage::forwarded_message_expiry`].
    pub message_expiry_seconds: Option<u32>,
    /// When the message was delivered, in epoch MILLISECONDS.
    pub delivered_at_millis: i64,
}

impl DirectMessage {
    /// The message expiry interval this message is FORWARDED with at `now_millis`: what
    /// the publisher set, less the whole seconds the message waited in the broker, and
    /// `None` when the publisher set none.
    ///
    /// MQTT 5 section 3.3.2.3.3 states it: "The PUBLISH packet sent to a Client by the
    /// Server MUST contain a Message Expiry Interval set to the received value minus the
    /// time that the Application Message has been waiting in the Server" — so a message
    /// handed straight to a live subscriber carries what was published, and one that waited
    /// on an offline client's session carries less. `Some(0)` says the interval is spent.
    #[must_use]
    pub fn forwarded_message_expiry(&self, now_millis: i64) -> Option<u32> {
        let published = self.message_expiry_seconds?;
        let waited_millis = now_millis.saturating_sub(self.delivered_at_millis).max(0);
        let waited = u32::try_from(waited_millis / 1_000).unwrap_or(u32::MAX);
        Some(published.saturating_sub(waited))
    }
}

/// One message on its way to a client over its live connection, and the caller waiting to
/// hear that the client acknowledged it.
///
/// `acknowledged` is fired when the client's own PUBACK for this delivery arrives, which is
/// what a `SendDirectMessage` asking for delivery confirmation waits on; `None` for a
/// delivery nobody is waiting on, which is every publication.
#[derive(Debug)]
pub struct Delivery {
    pub message: Box<DirectMessage>,
    pub acknowledged: Option<oneshot::Sender<()>>,
}

/// The LIVE network connection of one session: what the broker hands a delivery to, and
/// what a service-initiated disconnect closes.
///
/// A handle, not the connection: the connection itself is a task of its own, serving one
/// client's socket. Holding this on the session is what makes "the client is connected" a
/// fact about a real transport rather than a flag — and a session whose
/// [`MqttSession::connection`] is `None` has no transport, so nothing can be handed to it.
#[derive(Debug, Clone)]
pub struct LiveConnection {
    /// Which connection this is. A client that connects again under the same client id
    /// takes the session over, and the connection it displaced reads this to see that the
    /// session is no longer its own.
    pub id: u64,
    /// The deliveries the connection's own task writes to the client, bounded by
    /// [`MAX_PENDING_DELIVERIES`].
    deliveries: Sender<Delivery>,
    /// Signalled to CLOSE the connection: the connection's task is waiting on it and ends
    /// when it fires, which is how a `DeleteConnection` reaches a live socket.
    closing: Arc<Notify>,
}

impl LiveConnection {
    /// Hand `message` to this connection, with the caller waiting for the client's
    /// acknowledgement of it where there is one. `Err` gives the message back for a
    /// connection that is gone or already holding as many deliveries as it takes, so the
    /// caller can decide what becomes of it instead.
    ///
    /// The message travels BOXED, which is also how it sits on the channel: it is the
    /// larger half of this result, and a caller handed it back puts it somewhere else
    /// rather than reading it.
    pub fn deliver(
        &self,
        message: Box<DirectMessage>,
        acknowledged: Option<oneshot::Sender<()>>,
    ) -> Result<(), Box<DirectMessage>> {
        self.deliveries
            .try_send(Delivery {
                message,
                acknowledged,
            })
            .map_err(|refused| refused.into_inner().message)
    }

    /// Close this connection. The connection's task ends and the socket closes; the
    /// session record is left as the caller wrote it, which is what lets a
    /// service-initiated disconnect keep its own reason.
    pub fn close(&self) {
        self.closing.notify_one();
    }
}

/// One MQTT client session the broker is holding: what the connection operations report
/// on, and what `DeleteConnection` disconnects or discards.
///
/// A session is opened by an MQTT CONNECT on the broker's own endpoint ([`wire`]), so a
/// client id that has never connected has no session — which is why every connection
/// operation answers it with the service's not-found condition. Every field is a fact
/// about the session, so each operation's response is computed from the record and never
/// from the request that asked for it.
#[derive(Debug, Clone, Default)]
pub struct MqttSession {
    /// The client id the session was opened under, which `GetConnection` reports back.
    pub client_id: String,
    /// Whether the client's network connection is open. A session outlives it when the
    /// client asked for a persistent session.
    pub connected: bool,
    /// Whether the client connected with a clean session.
    pub clean_session: bool,
    /// The thing associated with the client's principal, when there is one.
    pub thing_name: Option<String>,
    /// The keep-alive interval the client asked for, in SECONDS.
    pub keep_alive_seconds: i32,
    /// The session expiry interval the client configured, in SECONDS.
    pub session_expiry_seconds: i64,
    /// When the client connected, in epoch MILLISECONDS — the unit
    /// `GetConnectionResponse$connectedSince` documents.
    pub connected_since_millis: i64,
    /// When the client disconnected, in epoch MILLISECONDS, or 0 while it is connected.
    pub disconnected_since_millis: i64,
    /// Why the client last disconnected, when it is disconnected.
    pub disconnect_reason: Option<String>,
    /// The socket information, which `GetConnection` reports only to a request that
    /// asked for it (`includeSocketInformation`).
    pub source_ip: Option<String>,
    pub source_port: i32,
    pub target_ip: Option<String>,
    pub target_port: i32,
    /// The VPC endpoint the client connected through, for a client that connected
    /// through one.
    pub vpc_endpoint_id: Option<String>,
    /// The topic filters this session subscribes to, each with its QoS, in stable
    /// filter order — the order `ListSubscriptions` pages over.
    pub subscriptions: BTreeMap<String, i32>,
    /// What was delivered to this session while the client was offline, oldest first and
    /// bounded by [`MAX_QUEUED_SESSION_MESSAGES`]. A message to a CONNECTED client goes to
    /// its connection and queues nothing, exactly as a publish to a live subscriber leaves
    /// no trace.
    pub queued_messages: VecDeque<DirectMessage>,
    /// The client's live connection, while it has one — see [`LiveConnection`].
    pub connection: Option<LiveConnection>,
}

impl MqttSession {
    /// Whether the service still reports this session at `now_millis`: always while the
    /// client is connected, and for [`DISCONNECT_INFORMATION_RETENTION_MILLIS`] after it
    /// disconnected.
    pub fn is_reported_at(&self, now_millis: i64) -> bool {
        self.connected
            || now_millis.saturating_sub(self.disconnected_since_millis)
                < DISCONNECT_INFORMATION_RETENTION_MILLIS
    }

    /// Close this session's network connection at `at_millis`, recording why. The
    /// session state (its subscriptions and queued messages) is kept: that is what makes
    /// it a persistent session.
    ///
    /// The live connection is given back, for a caller that has to close the socket too —
    /// the record says the client is gone the moment this returns, whether or not the
    /// transport has finished going.
    pub fn disconnect(&mut self, reason: &str, at_millis: i64) -> Option<LiveConnection> {
        self.connected = false;
        self.disconnected_since_millis = at_millis;
        self.disconnect_reason = Some(reason.to_string());
        self.connection.take()
    }

    /// Queue `message` for this offline session, dropping the oldest once the queue is at
    /// one of its bounds: [`MAX_QUEUED_SESSION_MESSAGES`] messages or
    /// [`MAX_QUEUED_SESSION_BYTES`] of payload.
    pub fn queue(&mut self, message: DirectMessage) {
        self.queued_messages.push_back(message);
        while self.queued_messages.len() > MAX_QUEUED_SESSION_MESSAGES
            || self.queued_bytes() > MAX_QUEUED_SESSION_BYTES
        {
            if self.queued_messages.pop_front().is_none() {
                return;
            }
        }
    }

    /// How many payload bytes this session is holding for its client.
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.queued_messages
            .iter()
            .map(|message| message.payload.len())
            .sum()
    }

    /// Hand `message` to this session: over the client's live connection when it has one,
    /// and onto the session's own bounded queue when the client is OFFLINE, which is what
    /// a persistent session is for. A message to a CONNECTED client is not queued — it is
    /// handed to its connection, exactly as a publish to a live subscriber leaves no
    /// trace.
    ///
    /// `acknowledged` is fired once the client has acknowledged this delivery, for a
    /// caller waiting on that confirmation. `true` when a live connection took the
    /// message, which is the only case in which a client is really being written to.
    pub fn deliver(
        &mut self,
        message: DirectMessage,
        acknowledged: Option<oneshot::Sender<()>>,
    ) -> bool {
        if let Some(connection) = self.connection.clone() {
            match connection.deliver(Box::new(message), acknowledged) {
                Ok(()) => return true,
                Err(refused) => {
                    if !self.connected {
                        self.queue(*refused);
                    }
                    return false;
                }
            }
        }
        if !self.connected {
            self.queue(message);
        }
        false
    }

    /// Whether this session subscribes to a filter that covers `topic`, which is what
    /// makes it one of a publication's recipients.
    pub fn subscribes_to(&self, topic: &str) -> bool {
        self.subscriptions
            .keys()
            .any(|filter| topic_filter_covers(filter, topic))
    }
}

/// The level separator of a topic and of a filter, and the prefix of the topic space the
/// broker reserves for its own system topics.
const TOPIC_LEVEL_SEPARATOR: char = '/';
/// See [`TOPIC_LEVEL_SEPARATOR`].
const RESERVED_TOPIC_PREFIX: char = '$';

/// The wildcards a topic FILTER may carry: one level, and the rest of the levels.
const SINGLE_LEVEL_WILDCARD: &str = "+";
/// See [`SINGLE_LEVEL_WILDCARD`].
const MULTI_LEVEL_WILDCARD: &str = "#";

/// Whether the topic FILTER `filter` covers the topic NAME `topic`.
///
/// The rule is MQTT's own topic matching (MQTT 3.1.1 section 4.7): a filter and a topic are
/// compared level by level, `+` matches exactly one level, `#` matches the remaining levels
/// and its own (so `a/#` covers `a`), and a filter carrying no wildcard covers only its own
/// topic. Section 4.7.2 adds the reserved space: a filter that BEGINS with a wildcard
/// covers no topic under [`RESERVED_TOPIC_PREFIX`], which is what keeps a `#` subscription
/// from receiving the broker's own system topics.
///
/// Read here rather than through the codec's own matcher, which indexes the topic's first
/// BYTE for that reserved-space rule: a topic whose first character is multi-byte is a
/// topic MQTT allows — a topic is any UTF-8 string — and byte 1 is no character boundary
/// in it. Whether a string is a topic name or a filter at all is still the codec's reading
/// ([`::mqttbytes::valid_topic`] and [`::mqttbytes::valid_filter`]), so a topic carrying a
/// wildcard (a filter, not a name) and a filter that is none are covered by nothing.
#[must_use]
pub fn topic_filter_covers(filter: &str, topic: &str) -> bool {
    if !::mqttbytes::valid_topic(topic) || !::mqttbytes::valid_filter(filter) {
        return false;
    }
    if topic.starts_with(RESERVED_TOPIC_PREFIX)
        && (filter.starts_with(SINGLE_LEVEL_WILDCARD) || filter.starts_with(MULTI_LEVEL_WILDCARD))
    {
        return false;
    }
    let mut topic_levels = topic.split(TOPIC_LEVEL_SEPARATOR);
    for level in filter.split(TOPIC_LEVEL_SEPARATOR) {
        match level {
            // The multi-level wildcard is a filter's last level and covers every level of
            // the topic left, including none at all.
            MULTI_LEVEL_WILDCARD => return true,
            // The single-level wildcard covers exactly one level, so a topic that has no
            // level left is not covered by it.
            SINGLE_LEVEL_WILDCARD => {
                if topic_levels.next().is_none() {
                    return false;
                }
            }
            named => {
                if topic_levels.next() != Some(named) {
                    return false;
                }
            }
        }
    }
    // Every level of the filter is spent, so the topic matches only if its own are too.
    topic_levels.next().is_none()
}

/// The broker's state: the sessions it holds, and the endpoint it answers at.
#[derive(Default)]
pub struct MqttBrokerState {
    /// The session the broker holds per client id — see [`MqttSession`].
    pub sessions: BTreeMap<String, MqttSession>,
    /// The broker's endpoint, once a call has opened it — see [`endpoint`].
    pub(crate) endpoint: Option<endpoint::BrokerEndpoint>,
    /// How many connections this broker has accepted, which is what the next one is
    /// identified by ([`LiveConnection::id`]).
    connections_accepted: u64,
}

/// Written by hand rather than derived: the endpoint holds a core listener, which carries
/// no `Debug`. What is diagnosable about the broker is how many sessions it holds and
/// where it answers — never a client's own session in a log line.
impl std::fmt::Debug for MqttBrokerState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MqttBrokerState")
            .field("sessions", &self.sessions.len())
            .field("endpoint", &self.authority())
            .finish()
    }
}

impl MqttBrokerState {
    /// Release every session whose record the service no longer reports — see
    /// [`MqttSession::is_reported_at`]. A release the CLOCK already made due: it was owed
    /// at this instant whether or not anybody called, so a reporting operation may
    /// materialise it before it answers.
    pub fn release_retired_sessions(&mut self, now_millis: i64) {
        self.sessions
            .retain(|_, session| session.is_reported_at(now_millis));
    }

    /// The session the broker holds for `client_id`, after the sessions the clock has
    /// already retired are released. `None` is the condition every connection operation
    /// answers with its not-found error.
    pub fn session(&mut self, client_id: &str, now_millis: i64) -> Option<&mut MqttSession> {
        self.release_retired_sessions(now_millis);
        self.sessions.get_mut(client_id)
    }

    /// The `host:port` the broker answers at, or `None` while no call has opened it.
    #[must_use]
    pub fn authority(&self) -> Option<String> {
        self.endpoint
            .as_ref()
            .map(|endpoint| endpoint.authority().to_string())
    }

    /// Identify the next connection: a number no other connection of this broker carries,
    /// so a session taken over can tell its new connection from the one it displaced.
    pub(crate) fn accept_connection(&mut self) -> u64 {
        self.connections_accepted = self.connections_accepted.saturating_add(1);
        self.connections_accepted
    }
}

/// Now, in epoch MILLISECONDS — the unit every instant a session reports is carried in
/// (`connectedSince`, `disconnectedSince`).
#[must_use]
pub fn now_millis() -> i64 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// The broker's store in the shared engine — the SAME one both IoT services reach, so a
/// session one of them reports is the session the other's connection opened.
///
/// `None` for an engine this emulator did not build, which vends no store; a caller
/// answers that with a modelled server error, never with a store of its own.
#[must_use]
pub fn broker_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<MqttBrokerState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<MqttBrokerState>)
}

/// Hand `message` to every session subscribed to a filter that covers its topic, and
/// answer how many sessions it was handed to — written to a live connection, or queued on
/// a persistent session for an offline client's return.
///
/// This is a PUBLICATION: a message published on a topic goes to the subscribers of that
/// topic, whoever published it — a device over its own connection or the HTTP data plane's
/// `Publish`. A subscriber that is offline with a persistent session has it queued, exactly
/// as its own broker queues one.
///
/// The store is taken once, for the fan-out itself, and released before the caller does
/// anything else: a delivery is handed to a connection's channel, never written on this
/// call's own thread.
pub fn publish_to_subscribers(
    state: &::engine::ServiceState<MqttBrokerState>,
    message: &DirectMessage,
) -> usize {
    let mut subscribers = 0;
    let mut held = state.lock();
    for session in held.sessions.values_mut() {
        if session.subscribes_to(&message.topic) {
            session.deliver(message.clone(), None);
            subscribers += 1;
        }
    }
    subscribers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a filter covers a topic by MQTT's own topic matching — exact levels, `+` for
    /// one level and `#` for the rest, and no wildcard-leading filter over the reserved
    /// space — which is what decides a publication's recipients.
    ///
    /// The rows span what the rule generalises over: a filter with no wildcard, a
    /// single-level wildcard at the front, in the middle and at the end, a multi-level
    /// wildcard (including over the parent level it also covers), the whole-tree filter,
    /// the level counts that make a filter miss, a topic in the broker's reserved `$`
    /// space, a topic whose first character is MULTI-BYTE (any UTF-8 string is a topic), a
    /// trailing empty level, and the strings that are no topic name or no filter at all.
    #[test]
    fn a_filter_covers_the_topics_mqtt_matching_gives_it() {
        let rows: &[(&str, &str, bool)] = &[
            ("devices/sensor-1/telemetry", "devices/sensor-1/telemetry", true),
            ("devices/sensor-1/telemetry", "devices/sensor-2/telemetry", false),
            ("devices/+/telemetry", "devices/sensor-2/telemetry", true),
            ("devices/+/telemetry", "devices/sensor-2/state", false),
            ("+/sensor-1/telemetry", "devices/sensor-1/telemetry", true),
            ("devices/sensor-1/+", "devices/sensor-1/telemetry", true),
            ("devices/#", "devices/sensor-1/telemetry/raw", true),
            ("devices/#", "fleet/sensor-1", false),
            // `a/#` covers the parent level itself (MQTT 3.1.1 section 4.7.1.2).
            ("devices/#", "devices", true),
            ("#", "devices/sensor-1", true),
            ("devices/sensor-1", "devices/sensor-1/telemetry", false),
            // A topic in the broker's reserved space is covered by a filter that names it
            // and by none that begins with a wildcard (section 4.7.2).
            ("$aws/things/sensor-1/shadow", "$aws/things/sensor-1/shadow", true),
            ("#", "$aws/things/sensor-1/shadow", false),
            ("+/things/sensor-1/shadow", "$aws/things/sensor-1/shadow", false),
            ("$aws/#", "$aws/things/sensor-1/shadow", true),
            // A topic is any UTF-8 string, so one whose first character is multi-byte is a
            // topic like any other — and the filters that cover a topic cover it too.
            ("#", "é/telemetry", true),
            ("é/+", "é/telemetry", true),
            ("é/telemetry", "é/telemetry", true),
            ("devices/#", "é/telemetry", false),
            // A trailing separator is an empty last level, which is a level like any other.
            ("devices/sensor-1/", "devices/sensor-1/", true),
            ("devices/sensor-1/+", "devices/sensor-1/", true),
            ("devices/sensor-1", "devices/sensor-1/", false),
            // Neither a topic carrying a wildcard (a filter, not a name) nor a string that
            // is no filter at all is covered by anything.
            ("#", "devices/+/telemetry", false),
            ("devices/#/telemetry", "devices/sensor-1/telemetry", false),
            ("", "devices/sensor-1", false),
            ("devices/sensor-1", "", false),
        ];
        for (filter, topic, expected) in rows {
            let covered = topic_filter_covers(filter, topic);
            assert_eq!(
                covered, *expected,
                "case ({filter:?}, {topic:?}): expected {expected}, got {covered}"
            );
        }
    }

    /// RULE: a session hands a delivery to the client's live connection when it has one,
    /// and queues it on the session when it has none — bounded, so the oldest goes first.
    #[tokio::test]
    async fn a_delivery_reaches_a_live_connection_and_queues_without_one() {
        let (deliveries, mut arriving) = ::tokio::sync::mpsc::channel(MAX_PENDING_DELIVERIES);
        let mut session = MqttSession {
            client_id: "sensor-1".to_string(),
            connected: true,
            connection: Some(LiveConnection {
                id: 1,
                deliveries,
                closing: Arc::new(Notify::new()),
            }),
            ..MqttSession::default()
        };
        let message = DirectMessage {
            topic: "devices/sensor-1/commands".to_string(),
            payload: b"run".to_vec(),
            ..DirectMessage::default()
        };
        assert!(
            session.deliver(message.clone(), None),
            "case \"a live connection\": expected the connection to take the delivery"
        );
        let handed = arriving.try_recv().expect("the connection was handed it");
        assert_eq!(
            handed.message.payload, message.payload,
            "case \"a live connection\": expected the published bytes, got {:?}",
            handed.message.payload
        );
        assert!(
            session.queued_messages.is_empty(),
            "case \"a live connection\": nothing may be queued for a client being written to"
        );

        // The same session once its client is gone: the delivery is kept for its return.
        session.disconnect("CLIENT_INITIATED_DISCONNECT", 10);
        assert!(
            !session.deliver(message.clone(), None),
            "case \"no connection\": expected no live connection to take it"
        );
        assert_eq!(
            session.queued_messages.len(),
            1,
            "case \"no connection\": expected the message queued on the session"
        );

        // And the queue is bounded: the oldest leaves rather than the session growing.
        for index in 0..MAX_QUEUED_SESSION_MESSAGES {
            session.queue(DirectMessage {
                topic: format!("devices/sensor-1/commands/{index}"),
                ..DirectMessage::default()
            });
        }
        assert_eq!(
            session.queued_messages.len(),
            MAX_QUEUED_SESSION_MESSAGES,
            "case \"a full queue\": expected the queue to hold at most its bound, got {}",
            session.queued_messages.len()
        );
    }
}
