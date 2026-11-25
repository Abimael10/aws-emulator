// SPDX-License-Identifier: CC0-1.0

//! What the BROKER reports about the session events it serves, for whatever LOGS them.
//!
//! A device's whole interaction with this emulator's IoT surface happens on the broker's own
//! socket — the CONNECT that opens a session, the SUBSCRIBE that records its filters, the
//! PUBLISH it sends, the message the broker writes back to it, the close that ends it — so an
//! operator auditing that interaction can only be told about it from here. The broker holds no
//! logging configuration of its own, exactly as it holds no topic rules
//! ([`super::PublicationSink`]): the account's logging options live in the IoT control plane,
//! so the service that owns them installs a SINK on the broker's store
//! ([`super::MqttBrokerState::install_session_log_sink`]) and this module hands it one
//! [`SessionEvent`] per event served. A broker with no sink installed reports nothing and
//! serves every packet exactly as before.
//!
//! An event is a statement about the SESSION and the packet, never the packet's payload: the
//! topic a publication was made on is reported, the bytes it carried are not, so what one event
//! can cost is bounded by the client id, the topic and the socket information its session
//! already holds.
//!
//! LOCK ORDER: a sink is read out from under the store's guard and called with it RELEASED —
//! what a sink does is a call of its own (it delivers somewhere) and no guard over this store
//! is held across an await.

use ::std::future::Future;
use ::std::pin::Pin;

use super::{MqttBrokerState, MqttSession};

/// The `eventType` of one session event, as AWS IoT names the events of its message broker:
/// the CONNECT that opened a session.
///
/// The six a client's own packets and the broker's answers amount to. The names are the ones
/// an account's own event-based logging configuration selects an event type by
/// (`LogEventConfiguration$eventType`: "the type of event to log. These include event types
/// like Connect, Publish, and Disconnect").
pub const CONNECT_EVENT: &str = "Connect";

/// The end of a session — see [`CONNECT_EVENT`].
pub const DISCONNECT_EVENT: &str = "Disconnect";

/// One topic filter a SUBSCRIBE asked for — see [`CONNECT_EVENT`].
pub const SUBSCRIBE_EVENT: &str = "Subscribe";

/// One topic filter an UNSUBSCRIBE gave up — see [`CONNECT_EVENT`].
pub const UNSUBSCRIBE_EVENT: &str = "Unsubscribe";

/// A publication the broker took FROM a client — see [`CONNECT_EVENT`].
pub const PUBLISH_IN_EVENT: &str = "Publish-In";

/// A message the broker wrote TO a client — see [`CONNECT_EVENT`].
pub const PUBLISH_OUT_EVENT: &str = "Publish-Out";

/// The transport the events of this broker are served over, which is what an entry about one
/// reports as the protocol it was spoken on.
pub const MQTT_PROTOCOL: &str = "MQTT";

/// ONE event the broker served, as whatever logs it is handed it.
///
/// Every member is a fact the broker itself knows: the session's own record supplies the client
/// id and the socket information ([`SessionEvent::of_session`]), and the packet supplies the
/// event type and the topic. A member the broker knows nothing for is left `None` rather than
/// filled in — a logger writes the members it was given and no others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    /// Which event it is: one of the `*_EVENT` names above.
    pub event_type: String,
    /// The client id whose session it is about.
    pub client_id: String,
    /// The topic or topic filter the event is about, for the events that name one.
    pub topic: Option<String>,
    /// The address the client connected FROM and the port it connected from, where this
    /// emulator knows them — the same values the session reports as its socket information.
    pub source_ip: Option<String>,
    /// See [`SessionEvent::source_ip`]; 0 for a connection whose source port is not known.
    pub source_port: i32,
    /// Whether the broker SERVED the event, which is what its own status says: `false` for a
    /// packet it refused.
    pub served: bool,
    /// WHY, where the event has a reason of its own: the sentence a refused packet was refused
    /// with, and the reason a session ended for an event that reports one.
    pub reason: Option<String>,
    /// When it happened, in epoch MILLISECONDS — the unit every instant in this broker is
    /// carried in ([`super::now_millis`]).
    pub at_millis: i64,
}

/// What became of one event the broker read, which is what its log record reports as the
/// outcome: the broker served it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome<'reason> {
    /// The broker served the event, and there is nothing further to say about it.
    Served,
    /// It served the event, and this is WHY it happened — the reason a session ended, which is
    /// what tells a client's own goodbye from a connection that was lost or taken over.
    ServedFor(&'reason str),
    /// It REFUSED the event, for this reason: the sentence an operator reads an unserved packet
    /// off.
    Refused(&'reason str),
}

impl Outcome<'_> {
    /// Whether the broker served the event.
    #[must_use]
    pub fn served(&self) -> bool {
        !matches!(self, Self::Refused(_))
    }

    /// The reason the event carries, where it has one.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Served => None,
            Self::ServedFor(reason) | Self::Refused(reason) => Some(reason),
        }
    }
}

impl SessionEvent {
    /// The event `event_type` of the session `client_id` with `outcome`, at `at_millis`: the
    /// event of a connection the broker holds no session record for (a CONNECT it refused
    /// before opening one), which is therefore reported by its client id alone.
    #[must_use]
    pub fn of_client(
        event_type: &str,
        client_id: &str,
        outcome: Outcome<'_>,
        at_millis: i64,
    ) -> Self {
        Self {
            event_type: event_type.to_string(),
            client_id: client_id.to_string(),
            topic: None,
            source_ip: None,
            source_port: 0,
            served: outcome.served(),
            reason: outcome.reason().map(str::to_string),
            at_millis,
        }
    }

    /// The event `event_type` of `session` with `outcome`, at `at_millis`: the session's own
    /// record supplies every fact about the client, so an event and the connection operations
    /// cannot disagree about which client made it or where from.
    #[must_use]
    pub fn of_session(
        event_type: &str,
        session: &MqttSession,
        outcome: Outcome<'_>,
        at_millis: i64,
    ) -> Self {
        Self {
            event_type: event_type.to_string(),
            client_id: session.client_id.clone(),
            topic: None,
            source_ip: session.source_ip.clone(),
            source_port: session.source_port,
            served: outcome.served(),
            reason: outcome.reason().map(str::to_string),
            at_millis,
        }
    }

    /// The same event, about `topic`: the topic a publication was made on, or the filter a
    /// SUBSCRIBE or an UNSUBSCRIBE named.
    #[must_use]
    pub fn about_topic(mut self, topic: &str) -> Self {
        self.topic = Some(topic.to_string());
        self
    }
}

/// What the session events this broker serves are LOGGED by.
///
/// The logging configuration belongs to the IoT control plane, which holds the account's V2
/// logging options, and this crate is below it — so the logger is reached through this trait,
/// exactly as the rules engine is ([`super::PublicationSink`]). A broker with none installed
/// logs nothing.
pub trait SessionLogSink: Send + Sync + 'static {
    /// Log `event`. Nothing of it is reported back to the client: the broker answers a packet
    /// the same way whatever became of its log record.
    fn log<'sink>(
        &'sink self,
        event: SessionEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'sink>>;
}

/// Report the event `event_type` of the session `client_id`, about `topic` where the event
/// names one and with what became of it — the one call the wire makes per event it serves.
///
/// The sink and the session are read under ONE guard of the store — so the event carries the
/// socket information and the thing that record holds — and the report is made with the guard
/// RELEASED, for the same reason a publication's evaluation is
/// ([`super::served_publication`]): a sink delivers somewhere, and no guard over this store is
/// held across an await. A client id the store holds no session for is reported by its client
/// id alone: the event happened on a connection, whether or not a session record was opened for
/// it.
///
/// A broker with no sink installed reads nothing off the store and reports nothing, which is
/// what keeps an account that logs nothing paying nothing per packet.
pub async fn reported(
    state: &::engine::ServiceState<MqttBrokerState>,
    event_type: &str,
    client_id: &str,
    topic: Option<&str>,
    outcome: Outcome<'_>,
    at_millis: i64,
) {
    let reporting = {
        let held = state.lock();
        held.session_log_sink().map(|sink| {
            let event = match held.sessions.get(client_id) {
                Some(session) => {
                    SessionEvent::of_session(event_type, session, outcome, at_millis)
                }
                None => SessionEvent::of_client(event_type, client_id, outcome, at_millis),
            };
            match topic {
                Some(topic) => (sink, event.about_topic(topic)),
                None => (sink, event),
            }
        })
    };
    let Some((sink, event)) = reporting else {
        return;
    };
    sink.log(event).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row: the case, the event, the topic it must name, whether the broker served it and
    /// the reason it must carry.
    type EventRow = (
        &'static str,
        SessionEvent,
        Option<&'static str>,
        bool,
        Option<&'static str>,
    );

    /// RULE: an event carries the facts of the session it is about, the topic it names and what
    /// became of it — and nothing else.
    #[test]
    fn an_event_reports_its_session_and_what_became_of_it() {
        let session = MqttSession {
            client_id: "sensor-1".to_string(),
            source_ip: Some("127.0.0.1".to_string()),
            source_port: 51_234,
            ..MqttSession::default()
        };
        // The rows span what the shape generalises over: an event about no topic, one about a
        // topic, one about a filter, one that was served and carries why it happened, a refused
        // one, and an event of a connection with no session record at all.
        let rows: &[EventRow] = &[
            (
                "a connect",
                SessionEvent::of_session(CONNECT_EVENT, &session, Outcome::Served, 1_000),
                None,
                true,
                None,
            ),
            (
                "a publication",
                SessionEvent::of_session(PUBLISH_IN_EVENT, &session, Outcome::Served, 2_000)
                    .about_topic("devices/sensor-1/telemetry"),
                Some("devices/sensor-1/telemetry"),
                true,
                None,
            ),
            (
                "a subscription",
                SessionEvent::of_session(SUBSCRIBE_EVENT, &session, Outcome::Served, 3_000)
                    .about_topic("devices/#"),
                Some("devices/#"),
                true,
                None,
            ),
            (
                "a session that ended",
                SessionEvent::of_session(
                    DISCONNECT_EVENT,
                    &session,
                    Outcome::ServedFor("CLIENT_INITIATED_DISCONNECT"),
                    4_000,
                ),
                None,
                true,
                Some("CLIENT_INITIATED_DISCONNECT"),
            ),
            (
                "a refused publication",
                SessionEvent::of_session(
                    PUBLISH_IN_EVENT,
                    &session,
                    Outcome::Refused("the publication is larger than the broker's quota"),
                    5_000,
                )
                .about_topic("devices/sensor-1/telemetry"),
                Some("devices/sensor-1/telemetry"),
                false,
                Some("the publication is larger than the broker's quota"),
            ),
            (
                "a refused connect, before any session record",
                SessionEvent::of_client(
                    CONNECT_EVENT,
                    "sensor-2",
                    Outcome::Refused("the connection named no client id"),
                    6_000,
                ),
                None,
                false,
                Some("the connection named no client id"),
            ),
        ];
        for (case, event, topic, served, reason) in rows {
            assert_eq!(
                event.topic.as_deref(),
                *topic,
                "case {case:?}: expected the topic {topic:?}, got {:?}",
                event.topic
            );
            assert_eq!(
                event.served, *served,
                "case {case:?}: expected served {served}, got {event:?}"
            );
            assert_eq!(
                event.reason.as_deref(),
                *reason,
                "case {case:?}: expected the reason {reason:?}, got {:?}",
                event.reason
            );
        }
        let connect = &rows[0].1;
        assert_eq!(
            (
                connect.client_id.as_str(),
                connect.source_ip.as_deref(),
                connect.source_port,
            ),
            ("sensor-1", Some("127.0.0.1"), 51_234),
            "case \"a connect\": expected the session's own facts, got {connect:?}"
        );
        let refused = &rows[5].1;
        assert_eq!(
            (refused.client_id.as_str(), refused.source_ip.as_deref()),
            ("sensor-2", None),
            "case \"a refused connect\": expected a connection with no session record to be \
             reported by its client id alone, got {refused:?}"
        );
    }
}
