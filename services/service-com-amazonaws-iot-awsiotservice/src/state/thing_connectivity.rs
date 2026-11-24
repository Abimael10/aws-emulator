// SPDX-License-Identifier: CC0-1.0

//! The CONNECTIVITY half of the thing index: what this service reports about the
//! connection of one registered thing.
//!
//! The data is not this service's own. A device does not call an API to connect: it opens an
//! MQTT connection to the account's data endpoint, and the session that connection makes
//! lives in the shared message broker
//! ([`::service_common::iot_mqtt_broker`]) — the same session the IoT data plane's connection
//! operations report on. The thing index's connectivity section is a VIEW of those sessions,
//! so this module is how a reader of a thing's connectivity finds the session that belongs to
//! it, and nothing of a connection is recorded a second time here.
//!
//! What a thing's connectivity is read from:
//!
//! * the account's own indexing configuration, which decides whether the index carries
//!   connectivity data at all ([`connectivity_indexing_enabled`]) and whether socket
//!   information may be retrieved through the connectivity API
//!   ([`socket_information_enabled`]);
//! * the broker session the thing's client holds ([`thing_connectivity`]), reduced to the
//!   members a reader reports ([`ThingConnectivityReport`]).
//!
//! A thing whose client has never connected — and one whose disconnect the broker no longer
//! reports — has NO session, which is the `None` here: a reader answers that the thing is not
//! connected and reports no session details for it, rather than inventing any.

use std::time::{SystemTime, UNIX_EPOCH};

use ::service_common::iot_mqtt_broker::{MqttBrokerState, MqttSession};

use crate::ops::types::ThingIndexingConfiguration;

use super::INDEXING_MODE_OFF;

/// The fleet-indexing API socket information is enabled for in an indexing configuration.
///
/// `ConnectivityFilter$includeSocketInformation` is "A list of fleet indexing APIs for which
/// to enable socket information retrieval. Currently, the only supported value is
/// GET_THING_CONNECTIVITY_DATA"
/// (`smithy_query(query="doc",shape="ConnectivityFilter",member="includeSocketInformation")`),
/// and `FleetIndexingApi` declares that one value alone
/// ([`crate::ops::types::FLEET_INDEXING_API_VALUES`]) — so this is the entry a configuration
/// has to carry for the socket members of a connectivity read to be reported at all.
pub const CONNECTIVITY_DATA_API: &str = "GET_THING_CONNECTIVITY_DATA";

/// Whether the account's thing index carries CONNECTIVITY data.
///
/// Two modes decide it, which is what the configuration's own cross-member rule states:
/// "STATUS – Your thing index contains connectivity status. To enable thing connectivity
/// indexing, thingIndexMode must not be set to OFF"
/// (`smithy_query(query="doc",shape="ThingIndexingConfiguration",member="thingConnectivityIndexingMode")`).
/// So the connectivity section is on exactly while the thing index is on and connectivity
/// indexing is not [`INDEXING_MODE_OFF`].
pub fn connectivity_indexing_enabled(configuration: &ThingIndexingConfiguration) -> bool {
    configuration.thing_indexing_mode != INDEXING_MODE_OFF
        && configuration
            .thing_connectivity_indexing_mode
            .as_deref()
            .is_some_and(|mode| mode != INDEXING_MODE_OFF)
}

/// Whether socket information may be retrieved through the connectivity API, as the
/// account's indexing configuration selected it.
///
/// The selection is `filter.connectivity.includeSocketInformation` naming
/// [`CONNECTIVITY_DATA_API`]; a configuration that names no API enables the retrieval for
/// none, so the socket members stay out of a response however the request asked.
pub fn socket_information_enabled(configuration: &ThingIndexingConfiguration) -> bool {
    configuration
        .filter
        .as_ref()
        .and_then(|filter| filter.connectivity.as_ref())
        .and_then(|connectivity| connectivity.include_socket_information.as_ref())
        .is_some_and(|apis| apis.iter().any(|api| api == CONNECTIVITY_DATA_API))
}

/// How this service refuses a connectivity read while the account's thing index carries no
/// connectivity data.
///
/// The fixed text states the condition — the index has no connectivity section, and the call
/// that turns one on is `UpdateIndexingConfiguration` — and the variable parts are the index
/// the read is served from and the mode that drives it, never a value from any one call.
pub fn connectivity_indexing_disabled_message(index_name: &str) -> String {
    format!(
        "Index {index_name} does not contain connectivity status. Please set \
         thingConnectivityIndexingMode to STATUS by calling UpdateIndexingConfiguration"
    )
}

/// `at` in epoch MILLISECONDS — the unit the broker keeps a session's instants in.
pub fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// What a reader reports about a thing's connection, read off the broker session that
/// belongs to the thing.
///
/// Every member is the session's own: nothing here is derived from the request that asked,
/// so two readers of the same thing report the same connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThingConnectivityReport {
    /// The client id the session was opened under.
    pub client_id: String,
    /// Whether the client's connection is open.
    pub connected: bool,
    /// Whether the client connected with a clean session.
    pub clean_session: bool,
    /// The keep-alive interval the client asked for, in SECONDS.
    pub keep_alive_seconds: i32,
    /// The session expiry interval the client configured, in SECONDS.
    pub session_expiry_seconds: i64,
    /// When the device connected or disconnected, in epoch MILLISECONDS: the instant the
    /// open connection was accepted, or the instant the last one closed.
    pub at_millis: i64,
    /// Why the client last disconnected, for a client that is disconnected.
    pub disconnect_reason: Option<String>,
    /// The socket the connection was made over — reported only where both the
    /// configuration and the request allow it.
    pub source_ip: Option<String>,
    /// See [`ThingConnectivityReport::source_ip`].
    pub source_port: i32,
    /// See [`ThingConnectivityReport::source_ip`].
    pub target_ip: Option<String>,
    /// See [`ThingConnectivityReport::source_ip`].
    pub target_port: i32,
    /// The VPC endpoint the client connected through, for a client that connected through
    /// one.
    pub vpc_endpoint_id: Option<String>,
}

impl ThingConnectivityReport {
    /// The report `session` amounts to.
    ///
    /// `at_millis` is the instant of the connection event the session is IN: the connect for
    /// a client that is connected, the disconnect for one that is not — which is what
    /// `GetThingConnectivityDataResponse$timestamp` ("The timestamp of when the device
    /// connected or disconnected") carries. The disconnect reason belongs to the same half:
    /// a connected client is not disconnected for any reason.
    fn of(session: &MqttSession) -> Self {
        Self {
            client_id: session.client_id.clone(),
            connected: session.connected,
            clean_session: session.clean_session,
            keep_alive_seconds: session.keep_alive_seconds,
            session_expiry_seconds: session.session_expiry_seconds,
            at_millis: if session.connected {
                session.connected_since_millis
            } else {
                session.disconnected_since_millis
            },
            disconnect_reason: if session.connected {
                None
            } else {
                session.disconnect_reason.clone()
            },
            source_ip: session.source_ip.clone(),
            source_port: session.source_port,
            target_ip: session.target_ip.clone(),
            target_port: session.target_port,
            vpc_endpoint_id: session.vpc_endpoint_id.clone(),
        }
    }
}

/// The connectivity of the thing `thing_name`, as the session its client holds in `broker`
/// stands at `now_millis`.
///
/// Which session belongs to a thing: the one whose client IDENTIFIES the thing — the session
/// the broker recorded the thing on, and otherwise the session opened under the thing's
/// DEFAULT client id, which is the thing name ("For a typical device, the thing name is also
/// used as the default MQTT client ID", `DescribeThing$defaultClientId`). A session the
/// broker recorded a DIFFERENT thing on is that thing's, whatever client id it carries.
///
/// A thing with more than one such session is reported from its LIVE one, and otherwise from
/// the one that disconnected last: a device that reconnected under a second client id is
/// connected, not disconnected.
///
/// `None` for a thing with no session at all — a client that has never connected, or one
/// whose disconnect the broker no longer reports
/// ([`::service_common::iot_mqtt_broker::DISCONNECT_INFORMATION_RETENTION_MILLIS`]).
///
/// The ONE write this makes is the release of the sessions the broker no longer reports,
/// which is owed at this instant whether or not anybody asked, and it never touches the
/// session being read.
pub fn thing_connectivity(
    broker: &mut MqttBrokerState,
    thing_name: &str,
    default_client_id: &str,
    now_millis: i64,
) -> Option<ThingConnectivityReport> {
    broker.release_retired_sessions(now_millis);
    broker
        .sessions
        .values()
        .filter(|session| match session.thing_name.as_deref() {
            Some(recorded) => recorded == thing_name,
            None => session.client_id == default_client_id,
        })
        .max_by_key(|session| {
            (
                session.connected,
                session
                    .connected_since_millis
                    .max(session.disconnected_since_millis),
            )
        })
        .map(ThingConnectivityReport::of)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ops::types::{ConnectivityFilter, IndexingFilter, FLEET_INDEXING_API_VALUES};

    /// The API name socket information is selected for is one the model declares.
    #[test]
    fn the_connectivity_api_is_a_modeled_fleet_indexing_api() {
        assert!(
            FLEET_INDEXING_API_VALUES.contains(&CONNECTIVITY_DATA_API),
            "expected {CONNECTIVITY_DATA_API} among the modelled fleet indexing apis, got \
             {FLEET_INDEXING_API_VALUES:?}"
        );
    }

    /// A configuration carries connectivity data only while BOTH the thing index and
    /// connectivity indexing are on.
    #[test]
    fn connectivity_indexing_needs_a_thing_index_that_is_on() {
        let cases = [
            ("REGISTRY", Some("STATUS"), true),
            ("REGISTRY_AND_SHADOW", Some("STATUS"), true),
            ("REGISTRY", Some(INDEXING_MODE_OFF), false),
            ("REGISTRY", None, false),
            (INDEXING_MODE_OFF, Some("STATUS"), false),
            (INDEXING_MODE_OFF, Some(INDEXING_MODE_OFF), false),
        ];
        for (thing_mode, connectivity_mode, expected) in cases {
            let configuration = ThingIndexingConfiguration {
                thing_indexing_mode: thing_mode.to_string(),
                thing_connectivity_indexing_mode: connectivity_mode.map(str::to_string),
                ..ThingIndexingConfiguration::default()
            };
            assert_eq!(
                connectivity_indexing_enabled(&configuration),
                expected,
                "case {thing_mode}/{connectivity_mode:?}: expected connectivity indexing \
                 enabled == {expected}, got {}",
                connectivity_indexing_enabled(&configuration)
            );
        }
    }

    /// Socket information is enabled only by a selection that NAMES the connectivity API.
    #[test]
    fn socket_information_follows_the_selected_apis() {
        let cases: [(&str, Option<Vec<String>>, bool); 4] = [
            ("no selection at all", None, false),
            ("an empty selection", Some(Vec::new()), false),
            (
                "a selection naming another api",
                Some(vec!["SEARCH_INDEX".to_string()]),
                false,
            ),
            (
                "a selection naming the connectivity api",
                Some(vec![CONNECTIVITY_DATA_API.to_string()]),
                true,
            ),
        ];
        for (case, selection, expected) in cases {
            let configuration = ThingIndexingConfiguration {
                thing_indexing_mode: "REGISTRY".to_string(),
                thing_connectivity_indexing_mode: Some("STATUS".to_string()),
                filter: Some(IndexingFilter {
                    connectivity: Some(ConnectivityFilter {
                        include_socket_information: selection,
                    }),
                    ..IndexingFilter::default()
                }),
                ..ThingIndexingConfiguration::default()
            };
            assert_eq!(
                socket_information_enabled(&configuration),
                expected,
                "case {case:?}: expected socket information enabled == {expected}, got {}",
                socket_information_enabled(&configuration)
            );
        }
    }

    /// A thing is reported from the session its client holds: the one recorded on the thing,
    /// the one opened under its default client id, and the LIVE one where it has two.
    #[test]
    fn the_live_session_of_the_things_client_is_the_one_reported() {
        let mut broker = MqttBrokerState::default();
        let now = 10_000;
        broker.sessions.insert(
            "sensor".to_string(),
            MqttSession {
                client_id: "sensor".to_string(),
                connected: false,
                disconnected_since_millis: now - 1_000,
                disconnect_reason: Some("CONNECTION_LOST".to_string()),
                ..MqttSession::default()
            },
        );
        broker.sessions.insert(
            "sensor-second".to_string(),
            MqttSession {
                client_id: "sensor-second".to_string(),
                thing_name: Some("sensor".to_string()),
                connected: true,
                connected_since_millis: now - 10,
                ..MqttSession::default()
            },
        );

        let report = thing_connectivity(&mut broker, "sensor", "sensor", now)
            .expect("the thing has a session");
        assert!(
            report.connected && report.client_id == "sensor-second",
            "case two sessions: expected the live one, got {report:?}"
        );

        // A session recorded on ANOTHER thing is not this thing's, whatever its client id.
        broker.sessions.insert(
            "other".to_string(),
            MqttSession {
                client_id: "other".to_string(),
                thing_name: Some("another-thing".to_string()),
                connected: true,
                connected_since_millis: now,
                ..MqttSession::default()
            },
        );
        let report = thing_connectivity(&mut broker, "sensor", "sensor", now)
            .expect("the thing still has its own session");
        assert_eq!(
            report.client_id, "sensor-second",
            "case a session of another thing: expected the thing's own session, got {report:?}"
        );

        assert!(
            thing_connectivity(&mut broker, "never-connected", "never-connected", now).is_none(),
            "case a thing that never connected: expected no session"
        );
    }
}
