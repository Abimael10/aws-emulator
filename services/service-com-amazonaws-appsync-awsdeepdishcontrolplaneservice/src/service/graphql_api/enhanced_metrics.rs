// SPDX-License-Identifier: CC0-1.0

//! WHICH of a GraphQL API's enhanced `AWS/AppSync` series are published, as the API's own
//! `enhancedMetricsConfig` asks for them.
//!
//! "Enhanced metrics emit granular data on API usage and performance" — the series carrying
//! one key beside the API's own ([`super::metrics`] publishes them) — and
//! `EnhancedMetricsConfig` is what turns each category of them on, one required member per
//! category:
//!
//! * `resolverLevelMetricsBehavior` — `FULL_REQUEST_RESOLVER_METRICS` "records and emits
//!   metric data for all resolvers in the request", while `PER_RESOLVER_METRICS` "records and
//!   emits metric data for resolvers that have the `metricsConfig` value set to `ENABLED`";
//! * `dataSourceLevelMetricsBehavior` — the same two readings over a data source's own
//!   `metricsConfig` (`FULL_REQUEST_DATA_SOURCE_METRICS` / `PER_DATA_SOURCE_METRICS`);
//! * `operationLevelMetricsConfig` — `ENABLED` or `DISABLED` outright, there being no
//!   per-resource opt-in for an operation.
//!
//! An API that configured NO enhanced metrics at all publishes none of these series: the
//! granular data is what the configuration asks for, so an API that asked for nothing is
//! reported on at the API's own dimension alone.
//!
//! Read ONCE per request, before any field of it resolves, so every field of one request is
//! reported under the configuration the request met rather than under whatever an update left
//! part-way through it.

use crate::state::ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState as ServiceStore;

/// The value a resource's own `metricsConfig` opts IN with:
/// `ResolverLevelMetricsConfig` / `DataSourceLevelMetricsConfig` are each `ENABLED` or
/// `DISABLED` (`smithy_query(query="shape",shape="ResolverLevelMetricsConfig,\
/// DataSourceLevelMetricsConfig")`).
const OPTED_IN: &str = "ENABLED";

/// `resolverLevelMetricsBehavior`'s value that reports every resolver of the request.
const FULL_REQUEST_RESOLVER_METRICS: &str = "FULL_REQUEST_RESOLVER_METRICS";

/// `dataSourceLevelMetricsBehavior`'s value that reports every data source of the request.
const FULL_REQUEST_DATA_SOURCE_METRICS: &str = "FULL_REQUEST_DATA_SOURCE_METRICS";

/// `operationLevelMetricsConfig`'s value that reports the operations executed.
const OPERATION_METRICS_ENABLED: &str = "ENABLED";

/// Which of the API's enhanced series one request publishes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Enhanced {
    /// How the resolvers of the request are reported.
    resolvers: Level,
    /// How the data sources they invoke are reported.
    data_sources: Level,
    /// Whether the operation executed is reported.
    operations: bool,
}

/// How one CATEGORY of enhanced series is reported: for none of its resources, for the ones
/// that opted in themselves, or for all of them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// The API asked for none of this category — including an API that configured no
    /// enhanced metrics at all.
    #[default]
    None,
    /// Only the resources whose own `metricsConfig` is `ENABLED`.
    OptedIn,
    /// Every resource of the request, whatever its own `metricsConfig` says: the behavior
    /// that "records and emits metric data for all resolvers in the request" overrides a
    /// resource's opt-out.
    Everything,
}

impl Level {
    /// Whether a resource whose own `metricsConfig` is `config` is reported at this level.
    fn reports(self, config: Option<&str>) -> bool {
        match self {
            Self::None => false,
            Self::OptedIn => config == Some(OPTED_IN),
            Self::Everything => true,
        }
    }
}

impl Enhanced {
    /// What `api_id`'s `enhancedMetricsConfig` asks for, as the store holds it now — nothing
    /// for an API this store holds no record of.
    pub fn of_api(store: &ServiceStore, api_id: &str) -> Self {
        let Some(configured) = store
            .graphql_apis
            .get(api_id)
            .and_then(|api| api.enhanced_metrics_config.as_ref())
        else {
            return Self::default();
        };
        Self {
            resolvers: match configured.resolver_level_metrics_behavior.as_str() {
                FULL_REQUEST_RESOLVER_METRICS => Level::Everything,
                _ => Level::OptedIn,
            },
            data_sources: match configured.data_source_level_metrics_behavior.as_str() {
                FULL_REQUEST_DATA_SOURCE_METRICS => Level::Everything,
                _ => Level::OptedIn,
            },
            operations: configured.operation_level_metrics_config == OPERATION_METRICS_ENABLED,
        }
    }

    /// Whether a resolver whose own `metricsConfig` is `config` has its series published.
    pub fn reports_resolver(&self, config: Option<&str>) -> bool {
        self.resolvers.reports(config)
    }

    /// Whether a data source whose own `metricsConfig` is `config` has its series published.
    pub fn reports_data_source(&self, config: Option<&str>) -> bool {
        self.data_sources.reports(config)
    }

    /// Whether the operation executed has its series published.
    pub fn reports_operations(&self) -> bool {
        self.operations
    }
}
