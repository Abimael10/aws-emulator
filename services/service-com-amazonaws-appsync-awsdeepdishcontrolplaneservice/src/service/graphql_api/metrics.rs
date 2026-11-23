// SPDX-License-Identifier: CC0-1.0

//! The `AWS/AppSync` metrics a GraphQL API publishes about its own work: what its REQUEST
//! plane answered, what the resolvers and data sources of those requests did, and what its
//! REAL-TIME plane admitted, refused, subscribed and delivered.
//!
//! Neither plane leaves a response a later caller can read — nothing in the control plane
//! reports that a request was refused for a stale key, that a resolver raised, that a
//! subscriber attached or that a mutation reached it. The only way a client learns any of it
//! is the series AppSync publishes about itself, so this module publishes one datapoint per
//! thing that happened, as it happens, and the two GAUGES
//! ([`ACTIVE_CONNECTIONS_METRIC`], [`ACTIVE_SUBSCRIPTIONS_METRIC`]) with the counts the
//! store holds AT that moment — every time one changes AND on the cadence the plane observes
//! them at while a connection is open ([`super::gauges`]), which is what lets a reader see
//! subscriptions fall back to zero, and STAY there, while a connection stays open.
//!
//! WHICH dimension set they land under is part of the contract:
//!
//! * what happens on the API — a request answered, a connect admitted or refused, a
//!   subscription started or ended, a `data` message delivered — is reported under
//!   [`GRAPHQL_API_ID_DIMENSION`] alone, the dimension that names a GraphQL API (as opposed
//!   to the `EventAPIId` an Event API's own series carry,
//!   [`super::super::event_api::metrics`]);
//! * what happens WITHIN one request — one resolver's invocation, one data source's
//!   invocation, one named operation's execution — is reported under that dimension AND
//!   exactly one key naming the thing it is of ([`RESOLVER_DIMENSION`],
//!   [`DATA_SOURCE_DIMENSION`], [`OPERATION_DIMENSION`]). These are the ENHANCED series: an
//!   API publishes them only where its `enhancedMetricsConfig` asks for them, which
//!   [`super::enhanced_metrics`] decides.
//!
//! The datapoints go into the CloudWatch store directly rather than through
//! `PutMetricData`, which reserves the `AWS/` namespaces for the services that own them
//! ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`); the append and the alarm wake-up it owes are the Event API
//! plane's own ([`super::super::event_api::metrics::EventApiMetrics`] documents both), so
//! one service's metrics have ONE way into the store they are read from.
//!
//! Each series about a REQUEST and about the work within it is published with no unit of its
//! own — `StandardUnit`'s `None` — beside the one measurement that has one: the span a
//! [`LATENCY_METRIC`] datapoint reports, in `Milliseconds`.

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

/// One unit of work the API performed: one GraphQL request it answered, one invocation of a
/// resolver it ran, one invocation of a data source one of those made, or one GraphQL
/// operation it executed — WHICH of them is said by the dimension set the series is published
/// under ([`api_dimensions`], [`resolver_dimensions`], [`data_source_dimensions`],
/// [`operation_dimensions`]).
pub const REQUESTS_METRIC: &str = "Requests";

/// How long that unit of work took, in [`appsync_metrics::MILLISECONDS_UNIT`]: the span the
/// API spent answering one request, running one resolver invocation or making one data source
/// invocation, again said by the dimension set.
pub const LATENCY_METRIC: &str = "Latency";

/// One request the API answered with a CLIENT error — a `4xx` status: material that
/// authorizes no call on the API, a document the API could not accept, a route it does not
/// serve.
pub const CLIENT_ERROR_METRIC: &str = "4XXError";

/// One request the API answered with a SERVER error — a `5xx` status.
pub const SERVER_ERROR_METRIC: &str = "5XXError";

/// The GraphQL errors one unit of work produced: the entries a result's own `errors` list
/// carries for the resolver invocation, or for the operation, the series is published of.
pub const GRAPHQL_ERROR_METRIC: &str = "GraphQLError";

/// The errors ONE data source invocation produced — "Errors: The number of errors that
/// occurred during a data source invocation" (`EnhancedMetricsConfig$dataSourceLevelMetricsBehavior`).
pub const DATA_SOURCE_ERROR_METRIC: &str = "Errors";

/// The dimension that names the RESOLVER an enhanced resolver-level series is of: the field
/// the resolver is attached to, as `<typeName>.<fieldName>`.
pub const RESOLVER_DIMENSION: &str = "Resolver";

/// The dimension that names the DATA SOURCE an enhanced data-source-level series is of: the
/// name the API holds it under.
pub const DATA_SOURCE_DIMENSION: &str = "DataSource";

/// The dimension that names the OPERATION an enhanced operation-level series is of: the
/// ROOT OPERATION TYPE the executed operation is of.
pub const OPERATION_DIMENSION: &str = "Operation";

/// The publisher of the metrics one GraphQL API produces about its own work: the store the
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

    /// Publish what ONE GraphQL request the API itself answered came to: that it was
    /// answered at all, how long answering it took, and whether the status it carried was a
    /// client or a server error.
    ///
    /// Both error series carry a datapoint for EVERY such request, a zero included: "how many
    /// of this minute's requests were refused" is as much a measurement when it is none as
    /// when it is many, and a series that skipped its zeros would leave a reader unable to
    /// tell an API that is refusing nothing from one that is serving no request at all.
    pub fn request_answered(&self, status: u16, latency: ::std::time::Duration) {
        let dimensions = || api_dimensions(&self.api_id);
        self.tallied(REQUESTS_METRIC, dimensions(), 1.0);
        self.measure(LATENCY_METRIC, dimensions(), latency);
        self.tallied(
            CLIENT_ERROR_METRIC,
            dimensions(),
            errored(status, CLIENT_ERROR_CLASS),
        );
        self.tallied(
            SERVER_ERROR_METRIC,
            dimensions(),
            errored(status, SERVER_ERROR_CLASS),
        );
    }

    /// Publish what ONE invocation of a resolver came to, under the resolver's own dimension:
    /// the invocation, the span it took and the GraphQL errors it produced — the three
    /// resolver-level series an API with enhanced metrics on emits
    /// ([`super::enhanced_metrics`] decides WHETHER it does).
    pub fn resolver_invoked(
        &self,
        resolver: &str,
        latency: ::std::time::Duration,
        errors: u32,
    ) {
        let dimensions = || resolver_dimensions(&self.api_id, resolver);
        self.tallied(REQUESTS_METRIC, dimensions(), 1.0);
        self.measure(LATENCY_METRIC, dimensions(), latency);
        self.tallied(GRAPHQL_ERROR_METRIC, dimensions(), f64::from(errors));
    }

    /// Publish what ONE invocation of a data source came to, under the data source's own
    /// dimension: the invocation, the span it took and the errors it produced.
    pub fn data_source_invoked(
        &self,
        data_source: &str,
        latency: ::std::time::Duration,
        errors: u32,
    ) {
        let dimensions = || data_source_dimensions(&self.api_id, data_source);
        self.tallied(REQUESTS_METRIC, dimensions(), 1.0);
        self.measure(LATENCY_METRIC, dimensions(), latency);
        self.tallied(
            DATA_SOURCE_ERROR_METRIC,
            dimensions(),
            f64::from(errors),
        );
    }

    /// Publish what ONE execution of a GraphQL operation came to, under the dimension naming
    /// the ROOT OPERATION TYPE it is of: that it was called, and the GraphQL errors it
    /// produced — the two operation-level series the model names ("Requests: The number of
    /// times a specified GraphQL operation was called", "GraphQL errors: The number of GraphQL
    /// errors that occurred during a specified GraphQL operation").
    pub fn operation_executed(&self, operation: &str, errors: u32) {
        let dimensions = || operation_dimensions(&self.api_id, operation);
        self.tallied(REQUESTS_METRIC, dimensions(), 1.0);
        self.tallied(GRAPHQL_ERROR_METRIC, dimensions(), f64::from(errors));
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

    /// Append ONE observation of `metric`, dimensioned on the API alone, as a count of things.
    fn publish(&self, metric: &str, value: f64) {
        self.written(
            metric,
            api_dimensions(&self.api_id),
            value,
            appsync_metrics::COUNT_UNIT,
        );
    }

    /// Append ONE tally of `metric` under `dimensions`: a count AppSync publishes with no
    /// unit of its own, which is what the series about a REQUEST and about the work within it
    /// carry.
    fn tallied(&self, metric: &str, dimensions: cloudwatch::DimensionSet, value: f64) {
        self.written(metric, dimensions, value, appsync_metrics::NO_UNIT);
    }

    /// Append ONE measurement of `metric` under `dimensions`: a SPAN, reported in the unit
    /// AppSync's latency series carry.
    fn measure(
        &self,
        metric: &str,
        dimensions: cloudwatch::DimensionSet,
        latency: ::std::time::Duration,
    ) {
        self.written(
            metric,
            dimensions,
            latency.as_secs_f64() * MILLISECONDS_PER_SECOND,
            appsync_metrics::MILLISECONDS_UNIT,
        );
    }

    /// Write one datapoint into the store, or report that the store could not hold it.
    fn written(
        &self,
        metric: &str,
        dimensions: cloudwatch::DimensionSet,
        value: f64,
        unit: &str,
    ) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        if !appsync_metrics::append_measurement(store, metric, dimensions, value, unit) {
            ::tracing::warn!(
                api_id = %self.api_id,
                %metric,
                "the CloudWatch store is at capacity; an AppSync metric could not be published"
            );
        }
    }
}

/// How many milliseconds one second is, for the series reported in them.
const MILLISECONDS_PER_SECOND: f64 = 1_000.0;

/// The first status of the CLIENT error class and of the SERVER error class, as HTTP defines
/// them: a status is of a class when it is in the hundred the class begins at.
const CLIENT_ERROR_CLASS: u16 = 400;
const SERVER_ERROR_CLASS: u16 = 500;

/// Whether `status` is of the error class beginning at `class`, as the datapoint such a
/// series carries for one request: one when it is, zero when it is not.
fn errored(status: u16, class: u16) -> f64 {
    match (class..class.saturating_add(100)).contains(&status) {
        true => 1.0,
        false => 0.0,
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

/// The dimension set of an enhanced series about one RESOLVER of the API: the API and the
/// field the resolver is attached to, which together identify it.
pub fn resolver_dimensions(api_id: &str, resolver: &str) -> cloudwatch::DimensionSet {
    enhanced_dimensions(api_id, RESOLVER_DIMENSION, resolver)
}

/// The dimension set of an enhanced series about one DATA SOURCE of the API.
pub fn data_source_dimensions(api_id: &str, data_source: &str) -> cloudwatch::DimensionSet {
    enhanced_dimensions(api_id, DATA_SOURCE_DIMENSION, data_source)
}

/// The dimension set of an enhanced series about one OPERATION executed on the API.
pub fn operation_dimensions(api_id: &str, operation: &str) -> cloudwatch::DimensionSet {
    enhanced_dimensions(api_id, OPERATION_DIMENSION, operation)
}

/// The dimension set of one enhanced series: the API, and EXACTLY ONE key beside it naming
/// the thing within the API the series is of.
fn enhanced_dimensions(api_id: &str, name: &str, value: &str) -> cloudwatch::DimensionSet {
    [
        (
            GRAPHQL_API_ID_DIMENSION.to_string(),
            api_id.to_string(),
        ),
        (name.to_string(), value.to_string()),
    ]
    .into_iter()
    .collect()
}
