// SPDX-License-Identifier: CC0-1.0

//! The `AWS/AppSync` metrics a GraphQL API's REAL-TIME plane publishes about its own work:
//! the connects it admitted, the ones it refused, the subscriptions started and ended on
//! them, the data messages those subscriptions were delivered, and the disconnects that
//! followed.
//!
//! A real-time connect leaves no response a later caller can read — nothing in the control
//! plane reports that a subscriber attached, that a handshake was turned away, that a
//! subscription is open or that a mutation reached it. The only way a client learns any of
//! it is the series AppSync publishes about itself, so this module publishes one datapoint
//! per thing that happened, as it happens, and the two GAUGES
//! ([`ACTIVE_CONNECTIONS_METRIC`], [`ACTIVE_SUBSCRIPTIONS_METRIC`]) with the counts the
//! store holds AT that moment — every time one changes AND on the cadence the plane observes
//! them at while a connection is open ([`super::gauges`]), which is what lets a reader see
//! subscriptions fall back to zero, and STAY there, while a connection stays open.
//!
//! WHICH dimension set they land under is part of the contract: all of this happens on the
//! API, so every series here is reported under [`GRAPHQL_API_ID_DIMENSION`] alone — the
//! dimension that names a GraphQL API, as opposed to the `EventAPIId` an Event API's own
//! series carry ([`super::super::event_api::metrics`]).
//!
//! The datapoints go into the CloudWatch store directly rather than through
//! `PutMetricData`, which reserves the `AWS/` namespaces for the services that own them
//! ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`); the append and the alarm wake-up it owes are the Event API
//! plane's own ([`super::super::event_api::metrics::EventApiMetrics`] documents both), so
//! one service's metrics have ONE way into the store they are read from.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::super::event_api::metrics::{self as appsync_metrics, CloudWatchState};

/// What the two gauges read at one moment: the connections open on the API and the
/// subscriptions they hold. The same reading either real-time plane reports, so there is one
/// shape for it.
pub use super::super::event_api::metrics::ConnectionCounts;

/// The series both of this service's real-time planes report about their own lifecycle,
/// named once ([`super::super::event_api::metrics`] documents each): AppSync publishes these
/// under the dimension of whichever API they are of, so the NAMES are shared and only the
/// dimension set differs.
pub use super::super::event_api::metrics::{
    ACTIVE_CONNECTIONS_METRIC, ACTIVE_SUBSCRIPTIONS_METRIC, CONNECT_SUCCESS_METRIC,
    DISCONNECT_SUCCESS_METRIC, SUBSCRIBE_SUCCESS_METRIC, UNSUBSCRIBE_SUCCESS_METRIC,
};

/// The dimension that names the GraphQL API every one of these series is of.
pub const GRAPHQL_API_ID_DIMENSION: &str = "GraphQLAPIId";

/// One attempt to establish a real-time connection on the API: every opening handshake
/// this endpoint judged, whether it completed it or refused it.
///
/// So the attempts a client made are reported beside what became of them
/// ([`CONNECT_SUCCESS_METRIC`], [`CONNECT_CLIENT_ERROR_METRIC`]): a connect that was turned
/// away is counted here as much as one that was established, which is what lets a reader
/// tell a client that is being refused from one that is not connecting at all.
pub const CONNECT_REQUESTS_METRIC: &str = "ConnectRequests";

/// One connect this endpoint refused because of the CLIENT: material that authorizes no
/// connection to the API, or a handshake addressed to something this endpoint does not
/// serve.
pub const CONNECT_CLIENT_ERROR_METRIC: &str = "ConnectClientError";

/// One `data` message this endpoint DELIVERED to a subscriber: a mutation published to a
/// subscription of this API that was written to that subscription's own socket.
///
/// It counts deliveries, not publications: a mutation a subscription's arguments or its
/// installed filter excluded reached no subscriber, so nothing of it is counted here, and a
/// mutation that reached two subscribers is counted twice.
pub const PUBLISH_DATA_MESSAGE_SUCCESS_METRIC: &str = "PublishDataMessageSuccess";

/// The publisher of the metrics one GraphQL API's real-time plane produces: the store the
/// datapoints go to, and the API they are all of.
///
/// It holds no counts of its own — every observation is a datapoint in the CloudWatch
/// store, never a second copy kept here, so the total a client reads is the `Sum`
/// CloudWatch computes over a period and either gauge is the reading taken at that moment.
#[derive(Clone)]
pub struct GraphqlApiMetrics {
    /// The store the datapoints are appended to, or `None` when the engine carries no
    /// CloudWatch.
    store: Option<::engine::ServiceState<CloudWatchState>>,
    /// The `apiId` every series this publisher writes is dimensioned on.
    api_id: String,
}

impl ::std::fmt::Debug for GraphqlApiMetrics {
    /// Which API this publisher is of and whether it has a store to append to, which is
    /// all of it that is not that store (a store handle has no rendering of its own).
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("GraphqlApiMetrics")
            .field("api_id", &self.api_id)
            .field("published", &self.store.is_some())
            .finish()
    }
}

impl GraphqlApiMetrics {
    /// The publisher appending `api_id`'s series to `store`.
    pub fn new(store: Option<::engine::ServiceState<CloudWatchState>>, api_id: &str) -> Self {
        Self {
            store,
            api_id: api_id.to_string(),
        }
    }

    /// Publish that one attempt to establish a real-time connection was made.
    pub fn connect_requested(&self) {
        self.publish(CONNECT_REQUESTS_METRIC, 1.0);
    }

    /// Publish that one real-time connection was established on the API, and the counts
    /// that leaves.
    pub fn connect_success(&self, counts: ConnectionCounts) {
        self.publish(CONNECT_SUCCESS_METRIC, 1.0);
        self.sampled(counts);
    }

    /// Publish that one connect was refused on the client's own account.
    pub fn connect_client_error(&self) {
        self.publish(CONNECT_CLIENT_ERROR_METRIC, 1.0);
    }

    /// Publish that one subscription was started on the API, and the counts that leaves.
    pub fn subscribe_success(&self, counts: ConnectionCounts) {
        self.publish(SUBSCRIBE_SUCCESS_METRIC, 1.0);
        self.sampled(counts);
    }

    /// Publish that one subscription was ended by its subscriber, and the counts that
    /// leaves.
    pub fn unsubscribe_success(&self, counts: ConnectionCounts) {
        self.publish(UNSUBSCRIBE_SUCCESS_METRIC, 1.0);
        self.sampled(counts);
    }

    /// Publish that `delivered` data messages were written to subscribers of the API.
    ///
    /// A publication that reached no subscriber is a measurement too: the series carries a
    /// zero for it, so a reader can tell an API whose mutations reach nobody from one that
    /// is serving no mutation at all.
    pub fn data_messages_published(&self, delivered: u32) {
        self.publish(
            PUBLISH_DATA_MESSAGE_SUCCESS_METRIC,
            f64::from(delivered),
        );
    }

    /// Publish that one real-time connection closed, and the counts that leaves.
    pub fn disconnect_success(&self, counts: ConnectionCounts) {
        self.publish(DISCONNECT_SUCCESS_METRIC, 1.0);
        self.sampled(counts);
    }

    /// Publish ONE SAMPLE of the two gauges: the connections open on the API and the
    /// subscriptions they hold, as the store holds them at this moment.
    ///
    /// What every change above reports through, and what [`super::gauges`] calls on its
    /// cadence, so every period of a held-open connection carries a reading of what the
    /// plane holds then rather than only the reading taken at the last change. Both counts
    /// are bounded by the plane's own caps (`crate::state::event_data_plane`'s connections
    /// per API and subscriptions per connection), so neither loses anything as a value.
    pub fn sampled(&self, counts: ConnectionCounts) {
        for (metric, value) in [
            (ACTIVE_CONNECTIONS_METRIC, counts.connections),
            (ACTIVE_SUBSCRIPTIONS_METRIC, counts.subscriptions),
        ] {
            self.publish(metric, f64::from(u32::try_from(value).unwrap_or(u32::MAX)));
        }
    }

    /// Append ONE observation of `metric`, dimensioned on the API alone.
    fn publish(&self, metric: &str, value: f64) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if !appsync_metrics::append_observation(
            store,
            metric,
            api_dimensions(&self.api_id),
            value,
        ) {
            ::tracing::warn!(
                api_id = %self.api_id,
                %metric,
                "the CloudWatch store is at capacity; an AppSync metric could not be published"
            );
        }
    }
}

/// The dimension set of a series about one GraphQL API: the API, and nothing else.
pub fn api_dimensions(api_id: &str) -> cloudwatch::DimensionSet {
    [(
        GRAPHQL_API_ID_DIMENSION.to_string(),
        api_id.to_string(),
    )]
    .into_iter()
    .collect()
}
