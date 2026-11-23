// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ES` SEARCH-REQUEST COUNT a domain's cluster publishes for the searches it served.
//!
//! A client watches the query load on a domain through CloudWatch: `SearchRate` dashboards,
//! search-throughput alarms, capacity sizing off search volume. Amazon OpenSearch Service
//! publishes that load as the `SearchRate` series of its own namespace, dimensioned on the
//! account the domain belongs to and the domain itself — the same pair every cluster-level
//! series of that namespace carries ([`super::cluster_metrics::dimensions`]) — and nothing
//! else in this API reports how many searches a domain answered, so a domain that serves
//! searches and publishes nothing leaves every reader of that load without data.
//!
//! WHAT IS COUNTED is a search the cluster really EXECUTED, read off the request that executed
//! it and off nothing else: one per request the cluster ran a search query for — a `_search`,
//! and equally a `_count`, which executes the same query over the same documents and reports
//! only how many it selected, the search a `_delete_by_query` resolves the documents it removes
//! with, and the search a monitor's own run asks. That is why the domain's search slow log
//! records all of them alike ([`super::log_publishing`]): one step notes the read, and it notes
//! the count with it. A request the cluster REFUSED — one it did not authenticate, one its
//! security plugin or an identity policy denied, one whose body it could not read — executed
//! no search and is counted into nothing.
//!
//! A COUNT OF EVENTS, not a gauge: the quantity has no value "at an instant", only a total
//! over a window, so a reader lays a period over the series and SUMS it — which is why each
//! served search is counted at the moment it is served rather than observed on the cadence the
//! cluster's gauges are ([`super::cluster_metrics`]), and why a window the cluster served no
//! search in carries no datapoint rather than a zero. The datapoints are stored at the regular
//! one-minute resolution of the namespace's other series, so the `Sum` of one period is the
//! number of searches that period's minute carried.
//!
//! WHERE the datapoints go: into the CloudWatch service's OWN store in the same shared engine,
//! through the one write this service makes into it
//! ([`super::cluster_metrics::publish_readings`]) — so this series, the cluster's gauges and
//! a client's own metrics share one home, and an alarm over any of them is made due by the
//! write.
//!
//! LOCK ORDER: the count is noted while this service's store is held — by the step that served
//! the search — and PUBLISHED once that store is released, so the two stores are never held at
//! once: the order [`super::log_publishing`] delivers under and [`super::cluster_metrics`]
//! publishes under.

use super::cluster_metrics::{publish_readings, Reading};

/// The series the searches a domain's cluster served are counted into: `SearchRate`, the
/// per-minute count of search requests the cluster executed.
pub const SEARCH_RATE_METRIC: &str = "SearchRate";

/// Count the `searches` one data-plane request had `domain_name`'s cluster execute into that
/// domain's [`SEARCH_RATE_METRIC`] series.
///
/// Called with NO store of this service locked (this module's lock order). A request the
/// cluster executed no search query for — a document write, a cluster read, and equally a
/// search it refused before reaching the documents — publishes nothing: a count of no searches
/// is not a datapoint of zero.
pub fn publish_searches<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    searches: u64,
) {
    if searches == 0 {
        return;
    }
    publish_readings(
        engine,
        &[Reading {
            domain_name: domain_name.to_string(),
            metric: SEARCH_RATE_METRIC.to_string(),
            // Exact: one request executes at most a handful of searches, far inside the
            // integers an `f64` represents exactly, so the count is never rounded.
            value: searches as f64,
        }],
    );
}
