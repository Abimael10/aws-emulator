// SPDX-License-Identifier: CC0-1.0

//! What the BROKER itself publishes about a session: its presence events, and the client's
//! own last will.
//!
//! Both are publications the broker makes rather than a client — nobody sends them on a
//! connection — and both are made on the ordinary publication path
//! ([`super::publish`]), so a subscriber receives them over its own subscription like any
//! other message:
//!
//! * the PRESENCE events of the reserved `$aws/events/presence/{connected,disconnected}/`
//!   space, one per connect and one per disconnect, carrying the session they are about and
//!   (for a disconnect) why it ended, which is what tells a client's own disconnect from a
//!   takeover of its client id;
//! * the client's LAST WILL, the publication its CONNECT asked the broker to make for it
//!   when the session ends any way but the client's own DISCONNECT.
//!
//! The reasons a session ends are named here too, because a disconnect event reports the
//! same reason the session record carries: one vocabulary for both.

use ::serde_json::json;

use super::{now_millis, DirectMessage, MqttBrokerState, MqttSession};

/// Why a session ended, as the `disconnectReason` of its disconnect event and of the
/// session record: the client sent its own DISCONNECT.
///
/// The four reasons this broker can end a session for, out of the values
/// `DisconnectReasonValue` models for the IoT control plane's own connectivity reporting.
pub const CLIENT_INITIATED_DISCONNECT: &str = "CLIENT_INITIATED_DISCONNECT";

/// The connection ended without a DISCONNECT — see [`CLIENT_INITIATED_DISCONNECT`].
pub const CONNECTION_LOST: &str = "CONNECTION_LOST";

/// Another connection connected under the same client id and took the session over — see
/// [`CLIENT_INITIATED_DISCONNECT`].
pub const DUPLICATE_CLIENTID: &str = "DUPLICATE_CLIENTID";

/// The SERVICE closed the connection (`DeleteConnection`) — see
/// [`CLIENT_INITIATED_DISCONNECT`].
pub const SERVER_INITIATED_DISCONNECT: &str = "SERVER_INITIATED_DISCONNECT";

/// The reserved topic space the broker publishes its presence events in, and the two event
/// kinds under it: `$aws/events/presence/connected/<clientId>` and
/// `$aws/events/presence/disconnected/<clientId>`.
pub const PRESENCE_TOPIC_PREFIX: &str = "$aws/events/presence";

/// The `eventType` a connect event carries, which is also its topic's own level — see
/// [`PRESENCE_TOPIC_PREFIX`].
pub const CONNECTED_EVENT: &str = "connected";

/// The `eventType` a disconnect event carries — see [`PRESENCE_TOPIC_PREFIX`].
pub const DISCONNECTED_EVENT: &str = "disconnected";

/// The QoS a lifecycle event and a will are published at when nothing else says: the
/// broker promises the event to a subscriber that is away, so it survives on a persistent
/// session (a QoS 0 publication to an offline session is discarded).
const LIFECYCLE_EVENT_QOS: i32 = super::QOS_AT_LEAST_ONCE;

/// The topic one session's event of `event_type` is published on: the reserved presence
/// space, the event kind, and the client id it is about.
#[must_use]
pub fn presence_topic(event_type: &str, client_id: &str) -> String {
    format!("{PRESENCE_TOPIC_PREFIX}/{event_type}/{client_id}")
}

/// A session identifier: the id one CONNECTION's session is known by in its own lifecycle
/// events, so two connections of the same client id are told apart by their events.
///
/// Minted per session and stored on it, never derived from the client id or the request.
#[must_use]
pub fn new_session_identifier() -> String {
    ::uuid::Uuid::new_v4().to_string()
}

/// Publish the CONNECTED event of `session`: the client is connected, under the session
/// identifier its own connection was given.
///
/// The event's `versionNumber` orders the events of one session, so it is taken from the
/// session and advanced.
pub fn publish_connected(
    state: &::engine::ServiceState<MqttBrokerState>,
    client_id: &str,
    session_identifier: &str,
    source_ip: Option<&str>,
    version_number: i64,
    at_millis: i64,
) {
    let mut event = json!({
        "clientId": client_id,
        "timestamp": at_millis,
        "eventType": CONNECTED_EVENT,
        "sessionIdentifier": session_identifier,
        "versionNumber": version_number,
    });
    // The address the client connected FROM, for a connection whose peer this emulator
    // knows; a member nothing is known for is left out rather than filled in.
    if let (Some(address), Some(members)) = (source_ip, event.as_object_mut()) {
        members.insert("ipAddress".to_string(), json!(address));
    }
    publish_event(state, presence_topic(CONNECTED_EVENT, client_id), &event);
}

/// Publish the DISCONNECTED event of a session that has ended, carrying the reason it
/// ended: `clientInitiatedDisconnect` says whether the client hung up itself, and
/// `disconnectReason` names which of the ways it was.
pub fn publish_disconnected(
    state: &::engine::ServiceState<MqttBrokerState>,
    client_id: &str,
    session_identifier: &str,
    reason: &str,
    version_number: i64,
    at_millis: i64,
) {
    let event = json!({
        "clientId": client_id,
        "timestamp": at_millis,
        "eventType": DISCONNECTED_EVENT,
        "sessionIdentifier": session_identifier,
        "clientInitiatedDisconnect": reason == CLIENT_INITIATED_DISCONNECT,
        "disconnectReason": reason,
        "versionNumber": version_number,
    });
    publish_event(state, presence_topic(DISCONNECTED_EVENT, client_id), &event);
}

/// One lifecycle event, published in the broker: a JSON document on a reserved presence
/// topic, at the QoS that makes it survive on a subscriber's persistent session.
fn publish_event(
    state: &::engine::ServiceState<MqttBrokerState>,
    topic: String,
    event: &::serde_json::Value,
) {
    super::publish(
        state,
        &DirectMessage {
            topic,
            payload: event.to_string().into_bytes(),
            qos: LIFECYCLE_EVENT_QOS,
            delivered_at_millis: now_millis(),
            ..DirectMessage::default()
        },
    );
}

/// A session that has ENDED, and what its termination publishes: the disconnect event
/// naming the reason, and the client's will where that termination publishes one.
///
/// Read off the session UNDER the store's lock and published once the lock is released,
/// which is how the broker makes a publication without holding the store across it.
#[derive(Debug, Clone)]
pub struct EndedSession {
    /// The client id whose session ended.
    pub client_id: String,
    /// The identifier of the session that ended, which its events are about.
    pub session_identifier: String,
    /// Why it ended: one of the reasons above, which is what the event reports.
    pub reason: String,
    /// The `versionNumber` its disconnect event carries.
    pub version_number: i64,
    /// The will to publish for it, and `None` for a termination that publishes none.
    pub will: Option<DirectMessage>,
}

impl EndedSession {
    /// What `session` ending for `reason` publishes: its own disconnect event, and its
    /// stored will unless the client hung up itself ([`MqttSession::will_published_on`]).
    ///
    /// `publish_will` is the caller's own suppression — `DeleteConnection`'s
    /// `preventWillMessage` is a caller asking for the will NOT to be published — and never
    /// publishes one the session's own termination does not.
    #[must_use]
    pub fn of(session: &mut MqttSession, reason: &str, publish_will: bool) -> Self {
        let version_number = session.lifecycle_version;
        session.lifecycle_version = version_number.saturating_add(1);
        Self {
            client_id: session.client_id.clone(),
            session_identifier: session.session_identifier.clone(),
            reason: reason.to_string(),
            version_number,
            will: session.will_published_on(reason).filter(|_| publish_will),
        }
    }
}

/// Publish what the end of one session publishes: its disconnect event, and then the will
/// it left for exactly this — the order a subscriber of both sees them in.
///
/// Takes the store itself, so it is called with no guard held.
pub fn publish_session_ended(
    state: &::engine::ServiceState<MqttBrokerState>,
    ended: &EndedSession,
    at_millis: i64,
) {
    publish_disconnected(
        state,
        &ended.client_id,
        &ended.session_identifier,
        &ended.reason,
        ended.version_number,
        at_millis,
    );
    if let Some(will) = &ended.will {
        let mut published = will.clone();
        published.delivered_at_millis = at_millis;
        super::publish(state, &published);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a presence event is published on the reserved presence topic of its event kind
    /// and client id, and carries the session it is about — a disconnect event also naming
    /// the reason and whether the client hung up itself.
    #[test]
    fn a_presence_event_names_its_session_and_how_it_ended() {
        let engine = ::engine::InMemoryEngine::new();
        let state = engine.state::<MqttBrokerState>();
        // A subscriber of the whole presence space, so every event below is delivered to
        // something and can be read back off its session.
        {
            let mut held = state.lock();
            let mut watcher = MqttSession {
                client_id: "presence-watcher".to_string(),
                ..MqttSession::default()
            };
            watcher
                .subscriptions
                .insert(format!("{PRESENCE_TOPIC_PREFIX}/#"), super::LIFECYCLE_EVENT_QOS);
            held.sessions.insert(watcher.client_id.clone(), watcher);
        }

        publish_connected(&state, "sensor-1", "a-session", Some("127.0.0.1"), 0, 1_000);
        // The rows span the ways a session ends: the client's own DISCONNECT, a lost
        // connection, a client-id takeover and the service closing it.
        let rows: &[(&str, bool)] = &[
            (CLIENT_INITIATED_DISCONNECT, true),
            (CONNECTION_LOST, false),
            (DUPLICATE_CLIENTID, false),
            (SERVER_INITIATED_DISCONNECT, false),
        ];
        for (index, (reason, _client_initiated)) in rows.iter().enumerate() {
            publish_disconnected(
                &state,
                "sensor-1",
                "a-session",
                reason,
                index as i64 + 1,
                2_000,
            );
        }

        let queued: Vec<(String, ::serde_json::Value)> = state
            .lock()
            .sessions
            .get("presence-watcher")
            .expect("the watcher's session")
            .queued_messages
            .iter()
            .map(|message| {
                (
                    message.topic.clone(),
                    ::serde_json::from_slice(&message.payload).unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            queued.len(),
            rows.len() + 1,
            "case \"every event\": expected one event per publication, got {queued:?}"
        );
        let (topic, event) = &queued[0];
        assert_eq!(
            topic, "$aws/events/presence/connected/sensor-1",
            "case \"the connect event\": expected the reserved connected topic, got {topic}"
        );
        assert_eq!(
            event.get("eventType").and_then(|value| value.as_str()),
            Some(CONNECTED_EVENT),
            "case \"the connect event\": expected the connected event type, got {event}"
        );
        assert_eq!(
            event.get("sessionIdentifier").and_then(|value| value.as_str()),
            Some("a-session"),
            "case \"the connect event\": expected the session it is about, got {event}"
        );
        for (index, (reason, client_initiated)) in rows.iter().enumerate() {
            let (topic, event) = &queued[index + 1];
            assert_eq!(
                topic, "$aws/events/presence/disconnected/sensor-1",
                "case {reason:?}: expected the reserved disconnected topic, got {topic}"
            );
            assert_eq!(
                event.get("disconnectReason").and_then(|value| value.as_str()),
                Some(*reason),
                "case {reason:?}: expected the reason the session ended, got {event}"
            );
            assert_eq!(
                event
                    .get("clientInitiatedDisconnect")
                    .and_then(::serde_json::Value::as_bool),
                Some(*client_initiated),
                "case {reason:?}: expected clientInitiatedDisconnect {client_initiated}, got \
                 {event}"
            );
            assert_eq!(
                event
                    .get("versionNumber")
                    .and_then(::serde_json::Value::as_i64),
                Some(index as i64 + 1),
                "case {reason:?}: expected the version the events are ordered by, got {event}"
            );
        }
    }

    /// RULE: the end of a session publishes its disconnect event and, unless the client
    /// hung up itself or the caller suppressed it, the will the client left.
    #[test]
    fn the_end_of_a_session_publishes_its_event_and_its_will() {
        let will = DirectMessage {
            topic: "devices/sensor-1/status".to_string(),
            payload: b"offline".to_vec(),
            qos: super::LIFECYCLE_EVENT_QOS,
            ..DirectMessage::default()
        };
        // The rows span what the suppression generalises over: a termination that publishes
        // the will, the client's own DISCONNECT, and a caller that asked for no will.
        let rows: &[(&str, &str, bool, bool)] = &[
            ("a lost connection", CONNECTION_LOST, true, true),
            ("a takeover", DUPLICATE_CLIENTID, true, true),
            (
                "the client's own disconnect",
                CLIENT_INITIATED_DISCONNECT,
                true,
                false,
            ),
            (
                "a caller that suppressed it",
                SERVER_INITIATED_DISCONNECT,
                false,
                false,
            ),
        ];
        for (case, reason, publish_will, expected) in rows {
            let mut session = MqttSession {
                client_id: "sensor-1".to_string(),
                session_identifier: "a-session".to_string(),
                will: Some(will.clone()),
                ..MqttSession::default()
            };
            let ended = EndedSession::of(&mut session, reason, *publish_will);
            assert_eq!(
                ended.will.is_some(),
                *expected,
                "case {case:?}: expected the will published {expected}"
            );
            assert_eq!(
                session.lifecycle_version, 1,
                "case {case:?}: expected the session's event version advanced, got {}",
                session.lifecycle_version
            );
        }
    }
}
