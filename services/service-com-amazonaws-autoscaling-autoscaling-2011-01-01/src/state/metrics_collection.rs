// SPDX-License-Identifier: CC0-1.0

//! The group metrics collection enabled for an Auto Scaling group: which of the service's
//! own group metrics one group aggregates to CloudWatch, and at what granularity.
//!
//! Metrics collection is a fact about ONE group, so it is stored on [`StoredGroup`] itself
//! ([`StoredGroup::enabled_metrics`]): the one collection `EnableMetricsCollection` writes
//! into, `DisableMetricsCollection` takes out of, `DescribeAutoScalingGroups` projects
//! from, and the group's own delete takes with the group. Nothing in this crate holds a
//! second copy.
//!
//! # What a request names
//!
//! The metrics a request may name are the service's own catalog
//! ([`super::METRIC_COLLECTION_TYPES`], the same set `DescribeMetricCollectionTypes`
//! publishes, checked through [`super::is_valid_metric_collection_type`]), and a request
//! that names none names every one of them: "if you specify Granularity and don't specify
//! any metrics, all metrics are enabled" (`EnableMetricsCollectionQuery$Metrics`) and "if
//! you omit this property, all metrics are disabled"
//! (`DisableMetricsCollectionQuery$Metrics`). A name outside the catalog names no metric of
//! this service, so the request is refused and nothing is written
//! ([`MetricsRefusal::UnknownMetrics`]).
//!
//! The granularity an enable asks for is checked the same way, against the granularities
//! the same catalog publishes ([`super::METRIC_GRANULARITIES`]): "the only valid value is
//! 1Minute" (`EnableMetricsCollectionQuery$Granularity`).
//!
//! # What the collection holds
//!
//! It is keyed by metric name, so naming a metric twice in one request, or naming one a
//! later request names again, leaves ONE entry: a metric is either collected or it is not.
//! Beside the name it holds the granularity that metric is collected at, which is what the
//! group reports for it. Disabling a metric that is not collected takes nothing out, and is
//! served rather than refused — the group is left in the state the request asked for either
//! way.

use std::collections::BTreeMap;

use crate::ops::types::EnabledMetric;

use super::StoredGroup;

/// The group metrics collected for ONE group: the granularity each is collected at, keyed
/// by the metric name.
///
/// A map rather than a list, because a metric is either collected or it is not: a request
/// naming the same metric twice enables one, and the group reports one entry per collected
/// metric.
pub type EnabledMetrics = BTreeMap<String, String>;

/// Why a metrics-collection change could not be recorded, for the operation to answer the
/// error class the situation belongs to.
///
/// Neither variant carries anything from the request: this service answers a request that
/// names a value outside an enumerated member's own set by reporting that set
/// ([`valid_metrics_message`], [`valid_metric_granularities_message`]), the same form it
/// refuses an unserved policy type with.
#[derive(Debug, PartialEq, Eq)]
pub enum MetricsRefusal {
    /// The request names at least one metric outside this service's own catalog.
    UnknownMetrics,
    /// The request asks for a granularity the service does not aggregate at.
    UnknownGranularity,
}

impl MetricsRefusal {
    /// What the caller is told. ONE spelling per situation, so no two writers of a group's
    /// metrics collection refuse the same request differently.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownMetrics => valid_metrics_message(),
            Self::UnknownGranularity => valid_metric_granularities_message(),
        }
    }
}

impl StoredGroup {
    /// Collect the metrics `requested` names — or every metric of the catalog when it names
    /// none — at `granularity`.
    ///
    /// The granularity and every metric name are checked BEFORE the first write, so a
    /// refused request leaves the group's metrics collection exactly as it was. A metric
    /// already collected is left collected, at the granularity this request asks for.
    pub fn enable_metrics_collection(
        &mut self,
        requested: &[String],
        granularity: &str,
    ) -> Result<(), MetricsRefusal> {
        if !super::is_valid_metric_granularity(granularity) {
            return Err(MetricsRefusal::UnknownGranularity);
        }
        let named = named_metrics(requested)?;
        for metric in named {
            self.enabled_metrics
                .insert(metric, granularity.to_string());
        }
        Ok(())
    }

    /// Stop collecting the metrics `requested` names — or every metric of the catalog when
    /// it names none.
    ///
    /// Every name is checked BEFORE the first write, for the same reason. A metric that is
    /// not collected is left uncollected.
    pub fn disable_metrics_collection(
        &mut self,
        requested: &[String],
    ) -> Result<(), MetricsRefusal> {
        let named = named_metrics(requested)?;
        for metric in named {
            self.enabled_metrics.remove(&metric);
        }
        Ok(())
    }

    /// This group's metrics collection as the API reports it — one `EnabledMetric` each.
    ///
    /// THE ONE projection of a stored entry onto the wire shape, so every read of the group
    /// reports a collected metric identically. Reported in the order the service's own
    /// catalog publishes the metrics, so what a group reports depends on which metrics are
    /// collected and never on the order the requests that enabled them arrived in.
    #[must_use]
    pub fn enabled_metric_views(&self) -> Vec<EnabledMetric> {
        super::METRIC_COLLECTION_TYPES
            .iter()
            .filter_map(|metric| {
                self.enabled_metrics
                    .get(*metric)
                    .map(|granularity| EnabledMetric {
                        metric: Some((*metric).to_string()),
                        granularity: Some(granularity.clone()),
                    })
            })
            .collect()
    }
}

/// The metrics a request names: the ones it listed, or the whole catalog when it listed
/// none.
///
/// A name outside the catalog is refused here, so neither an enable nor a disable can write
/// a metric this service does not have.
pub fn named_metrics(requested: &[String]) -> Result<Vec<String>, MetricsRefusal> {
    if requested.is_empty() {
        return Ok(super::METRIC_COLLECTION_TYPES
            .iter()
            .map(|metric| (*metric).to_string())
            .collect());
    }
    if requested
        .iter()
        .any(|candidate| !super::is_valid_metric_collection_type(candidate))
    {
        return Err(MetricsRefusal::UnknownMetrics);
    }
    Ok(requested.to_vec())
}

/// The sentence a request naming a metric outside this service's own catalog is refused
/// with: the metrics the caller has to choose from, read from the catalog the service
/// publishes, and nothing from the request.
#[must_use]
pub fn valid_metrics_message() -> String {
    format!(
        "Valid metrics are: [{}].",
        super::METRIC_COLLECTION_TYPES.join(", ")
    )
}

/// The sentence a request asking for a granularity this service does not aggregate at is
/// refused with.
///
/// The counterpart of [`valid_metrics_message`] for the granularity member, in the wording
/// this refusal class carries: the subject stays plural (`metrics`) while the thing named
/// is singular (`granularity type is`). Every word of it is fixed text; its ONE variable
/// part is the bracketed enumeration of the granularities the service accepts, read from
/// the catalog it publishes ([`super::METRIC_GRANULARITIES`], what
/// `DescribeMetricCollectionTypes` reports as `Granularities`) rather than written out
/// here, so the sentence states the very set the request was checked against.
#[must_use]
pub fn valid_metric_granularities_message() -> String {
    format!(
        "Valid metrics granularity type is: [{}].",
        super::METRIC_GRANULARITIES.join(", ")
    )
}
