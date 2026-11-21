// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ES` METRICS a provisioned domain's cluster publishes about itself.
//!
//! A client that provisions a domain reads what its cluster holds through CloudWatch: the
//! documents it can search, the nodes it runs, the health colour its shard allocation leaves
//! it in. Amazon OpenSearch Service publishes those quantities as its own `AWS/ES` series,
//! dimensioned on the account the domain belongs to
//! ([`CLIENT_ID_DIMENSION`]) and the domain itself ([`DOMAIN_NAME_DIMENSION`]) — the pair
//! every cluster-level series of that namespace carries — and an alarm, a dashboard or an
//! on-call rule over the series is the only way the quantity is observable at all: no
//! operation of this control plane reports how many documents a domain's indexes hold.
//!
//! WHAT is published is computed from this service's own records and from nothing else
//! ([`cluster_gauges`]): the documents the domain's indexes really hold, the nodes its
//! stored `ClusterConfig` really provisions — the same census `DescribeDomainNodes`
//! reports, read through that operation's own projection so the two cannot disagree — and
//! the health its shard allocation really leaves the cluster in, read through the same
//! allocation its own `_cluster/health` answers with ([`super::cluster_health`]). A
//! quantity this emulator holds no state for is published as no series at all rather than
//! as a number nothing measured.
//!
//! EACH OF THEM IS A GAUGE, not a count of events: the quantity has a value at every
//! instant — a health colour the cluster is or is not of is one too, published as the 0/1
//! indicator per colour that AWS reports it as — so a reader laying periods over the series
//! expects a reading IN each period —
//! including every period after the quantity last changed. A series written only on change
//! cannot answer that, so the gauges are OBSERVED ON A CADENCE ([`SAMPLE_INTERVAL`]) for as
//! long as this account holds a domain ([`drive_metric_sampling`]), which is what makes a
//! domain's series readable before anything is indexed into it (the reading is then zero
//! documents, which is a measurement) and what makes the period after a bulk load read the
//! new total.
//!
//! Three properties of that sampler, each of them observable:
//!
//! * IT IS OFF THE RESPONSE PATH. No client call produces these datapoints: the series is
//!   what an operator reads, so it exists whether or not anybody is calling, and nothing on
//!   a read path starts, advances or stops the sampler. It is claimed by the write that
//!   gives this service a cluster to measure — `CreateDomain` — and at most one runs per
//!   store.
//! * NOTHING IS SAMPLED FOR A CLUSTER THAT IS NOT UP. A domain is provisioning until the
//!   span the clock owes it has passed ([`Domain::cluster_exists`]), and a domain that was
//!   deleted is gone from the store: neither carries a datapoint, so the series covers the
//!   time the cluster was there and no more.
//! * THE READINGS ARE THE STORE'S. Every sample is what the records hold AT that moment;
//!   this module keeps no count of its own, so a series and the cluster it measures cannot
//!   drift.
//!
//! WHERE the datapoints go: into the CloudWatch service's OWN store in the same shared
//! engine, the store `ListMetrics`, `GetMetricStatistics` and `GetMetricData` answer from
//! and the store an alarm is evaluated against — so one metric and its reader cannot
//! disagree. They are written there directly rather than sent over `PutMetricData`, which
//! reserves the `AWS/` namespaces for the services that own them ("you should not specify a
//! namespace that begins with `AWS/`", `PutMetricData$Namespace`): a service's own metric
//! never travels over that API. Storing a datapoint is also what makes the evaluation of
//! every alarm watching that series due, exactly as an ingested one does, so an alarm cannot
//! tell this service's own metric from a client's. It is the same seam Amazon Redshift's own
//! health series and the `AWS/AppSync` series of an Event API's data plane are published
//! through.
//!
//! LOCK ORDER: the readings are taken while this service's store is held and PUBLISHED
//! ([`publish`]) once it is released, so the two stores are never held at once — the same
//! order [`super::log_publishing`] delivers under.

use ::std::sync::Arc;
use ::std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::cluster_health::{ClusterStatus, ShardAllocation};
use super::{service_state, Domain};

/// The namespace Amazon OpenSearch Service publishes its own metrics to.
pub const METRIC_NAMESPACE: &str = "AWS/ES";

/// The dimension that names the account a cluster-level series belongs to.
pub const CLIENT_ID_DIMENSION: &str = "ClientId";

/// The dimension that names the domain a cluster-level series is of.
pub const DOMAIN_NAME_DIMENSION: &str = "DomainName";

/// The total number of searchable documents across the cluster's data nodes.
pub const SEARCHABLE_DOCUMENTS_METRIC: &str = "SearchableDocuments";

/// The number of nodes the cluster runs.
pub const NODES_METRIC: &str = "Nodes";

/// The INDICATOR series of one cluster health colour: `ClusterStatus.green`,
/// `ClusterStatus.yellow` or `ClusterStatus.red`.
///
/// A cluster has one health colour at a time and publishes one series per colour, each
/// reading 1 while the cluster is of that colour and 0 while it is not — so a client alarms
/// on the colour it cares about (`ClusterStatus.red` at a maximum of 1) instead of comparing
/// a single enumerated value. The names are the colour the engine reports
/// ([`ClusterStatus::as_str`]) behind the family's own prefix, so no colour's series name is
/// spelled twice.
pub fn cluster_status_metric(status: ClusterStatus) -> String {
    format!("{CLUSTER_STATUS_METRIC_PREFIX}{}", status.as_str())
}

/// The prefix the cluster-status indicator series share — see [`cluster_status_metric`].
pub const CLUSTER_STATUS_METRIC_PREFIX: &str = "ClusterStatus.";

/// The `StandardUnit` a bare count of things carries: `Count`.
const COUNT_UNIT: &str = "Count";

/// The `StorageResolution` these datapoints are published at, in seconds: regular
/// resolution, "which CloudWatch stores at 1-minute resolution"
/// (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// How often the cluster gauges are observed while this account holds a domain.
///
/// An EMULATOR-CHOSEN cadence — the service publishes the series, not a sampling frequency —
/// and deliberately shorter than the one-minute resolution the datapoints are stored at, so
/// every minute of a provisioned domain's life carries several readings and the minute after
/// a change reads what the cluster holds rather than nothing. It is non-zero, so the sampler
/// hands the runtime back between samples instead of spinning.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// The single sampler claim this store carries: whether one runs, the generation that holds
/// the claim, and the task's own handle so a replaced sampler can be stopped.
#[derive(Debug, Default)]
pub struct MetricSampling {
    /// Whether a sampler is running, which is what keeps a second one from starting.
    running: bool,
    /// Which claim is in force: a starter whose generation is no longer this one has
    /// already been stood down.
    generation: u64,
    /// The running sampler's task, kept so it remains abortable.
    task: Option<::tokio::task::JoinHandle<()>>,
}

impl MetricSampling {
    /// Claim the single sampler: the claim's GENERATION when the caller may start one,
    /// `None` when one already runs.
    fn claim(&mut self) -> Option<u64> {
        if self.running {
            return None;
        }
        self.running = true;
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    /// Keep the running sampler's task handle, aborting the one it replaces — and only
    /// while `generation` is still the claim in force, so a starter whose own task has
    /// already stood down installs nothing and aborts nothing.
    fn set_task(&mut self, generation: u64, task: ::tokio::task::JoinHandle<()>) {
        if !self.running || self.generation != generation {
            task.abort();
            return;
        }
        if let Some(previous) = self.task.replace(task) {
            previous.abort();
        }
    }

    /// Whether the claim `generation` holds still has something to sample, RELEASING that
    /// claim when it has not.
    ///
    /// Observing "nothing to sample" and standing down happen in ONE critical section,
    /// which is what makes the hand-over race-free: a create holds this same lock to store
    /// its domain BEFORE it tries to claim the sampler, so either this call sees that
    /// domain and the running sampler keeps it, or the create's claim finds the flag
    /// cleared and starts a sampler of its own.
    fn wanted_or_release(&mut self, wanted: bool, generation: u64) -> bool {
        if !wanted && self.generation == generation {
            self.running = false;
            self.task = None;
        }
        wanted
    }
}

/// ONE reading of one domain's cluster gauges: which domain it is of, and the value each
/// gauge took at the instant it was taken.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterGauges {
    /// The `DomainName` every series of this reading is dimensioned on.
    pub domain_name: String,
    /// How many documents the domain's indexes hold — [`SEARCHABLE_DOCUMENTS_METRIC`].
    pub searchable_documents: f64,
    /// How many nodes its cluster configuration provisions — [`NODES_METRIC`].
    pub nodes: f64,
    /// The health colour its shard allocation leaves the cluster in, which the three
    /// cluster-status indicator series report ([`cluster_status_metric`]).
    pub status: ClusterStatus,
}

impl ClusterGauges {
    /// The series this reading publishes, each with the value it took.
    ///
    /// The two counts, plus one INDICATOR per health colour: every colour is published on
    /// every sample — the cluster has a health at every instant, and a reader asking about
    /// the colour the cluster is not in is answered 0 rather than an empty series — and
    /// exactly one of them reads 1.
    fn series(&self) -> Vec<(String, f64)> {
        let mut series = vec![
            (
                SEARCHABLE_DOCUMENTS_METRIC.to_string(),
                self.searchable_documents,
            ),
            (NODES_METRIC.to_string(), self.nodes),
        ];
        series.extend(
            ClusterStatus::ALL
                .into_iter()
                .map(|colour| (cluster_status_metric(colour), colour.indicator(self.status))),
        );
        series
    }
}

/// The gauges `domain`'s cluster reads AT THIS MOMENT, computed from its own records.
///
/// The documents are the ones its indexes really hold ([`Domain::indexes`]), the nodes are
/// the census its stored `ClusterConfig` provisions — read through the projection
/// `DescribeDomainNodes` reports (`crate::ops::describe_domain_nodes::domain_nodes`), so the
/// series and that operation cannot describe one cluster differently — and the health colour
/// is the one its SHARD ALLOCATION leaves it in
/// ([`ShardAllocation`](super::cluster_health::ShardAllocation)), the same reading its own
/// `_cluster/health` answers with. Both counts are bounded by this store's own quotas (the
/// indexes a domain may hold, the documents an index may hold and the node census bound), so
/// neither loses anything as a metric value.
pub fn cluster_gauges(domain: &Domain) -> ClusterGauges {
    let documents: usize = domain
        .indexes
        .values()
        .map(|index| index.documents.len())
        .sum();
    let nodes = crate::ops::describe_domain_nodes::domain_nodes(domain).len();
    ClusterGauges {
        domain_name: domain.domain_name.clone(),
        searchable_documents: metric_value(documents),
        nodes: metric_value(nodes),
        status: ShardAllocation::of_cluster(domain).status(),
    }
}

/// A counted quantity as the value a datapoint carries.
///
/// Exact: every count published here is bounded by this store's own quotas — the indexes a
/// domain may hold times the documents an index may hold, and the node census bound — all of
/// them far inside the integers an `f64` represents exactly, so a reading is never rounded
/// and never clamped to a number the cluster does not hold.
fn metric_value(count: usize) -> f64 {
    count as f64
}

/// The dimension set of a cluster-level series: the account the domain belongs to and the
/// domain itself, which together identify the cluster the quantity is of.
pub fn dimensions(domain_name: &str) -> cloudwatch::DimensionSet {
    [
        (
            CLIENT_ID_DIMENSION.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
        (DOMAIN_NAME_DIMENSION.to_string(), domain_name.to_string()),
    ]
    .into_iter()
    .collect()
}

/// Start the cluster-metric sampler of this engine's store, if one is not already running.
///
/// Called by the write that gives this service a cluster to measure — `CreateDomain`. At
/// most one sampler runs per store (the store carries the claim), it samples for as long as
/// the store holds a domain, and with no async runtime to spawn onto there is no sampler and
/// so no series: nothing is invented for a cluster nobody is observing.
pub fn drive_metric_sampling<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Some(generation) = state.lock().metric_sampling.claim() else {
        return;
    };
    let sampling = Arc::clone(engine);
    let task = runtime.spawn(async move {
        // The sampler ends once the store holds no domain left to observe, which it sees —
        // and releases its claim in — inside one critical section.
        while wanted(&*sampling, generation) {
            // Every sample waits first, which is what hands the runtime back between
            // them. No store lock is held across it.
            ::tokio::time::sleep(SAMPLE_INTERVAL).await;
            publish_sample(&*sampling);
        }
    });
    state.lock().metric_sampling.set_task(generation, task);
}

/// Whether this store still has a cluster to sample, releasing the claim `generation` holds
/// when it has not.
fn wanted<Eng: Send + Sync + 'static>(engine: &Eng, generation: u64) -> bool {
    let Some(state) = service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    let holds_domain = !store.domains.is_empty();
    store
        .metric_sampling
        .wanted_or_release(holds_domain, generation)
}

/// Take ONE sample of every provisioned domain's gauges and publish it.
///
/// The readings are READ from this service's store and published as they stand: taking a
/// sample materialises only what the clock already owed each domain — the configuration
/// change that had fallen due whether or not anybody called — and writes nothing else, so a
/// sample changes nothing a later caller sees. A domain whose cluster is not up yet carries
/// no reading at all rather than a reading of a cluster that does not exist.
fn publish_sample<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    let observed: Vec<ClusterGauges> = {
        let mut store = state.lock();
        store.settle_all(now);
        store
            .domains
            .values()
            .filter(|domain| domain.cluster_exists())
            .map(cluster_gauges)
            .collect()
    };
    publish(engine, &observed);
}

/// Append `observed` to the CloudWatch store of the SAME engine, and make every alarm
/// watching one of those series due for evaluation.
///
/// Call it with NO store of this service locked (this module's lock order). An engine
/// carrying no CloudWatch has nothing to publish to, and a CloudWatch store already holding
/// its full complement of series drops the reading rather than failing anything: the series
/// is published off every answer, so it can never be what refuses a call.
pub fn publish<Eng: Send + Sync + 'static>(engine: &Eng, observed: &[ClusterGauges]) {
    if observed.is_empty() {
        return;
    }
    let Some(state) = cloudwatch::service_state(engine) else {
        // An engine with no CloudWatch mounted has nothing to publish to — a deployment
        // nobody can read the series from — so nothing is published and no second home for
        // the datapoints is invented.
        return;
    };
    let at_secs = cloudwatch::now_secs();
    let mut store = state.lock();
    let mut written = Vec::new();
    // How many readings this pass could not store, reported ONCE for the pass rather than
    // once per series: a store at capacity stays at capacity, so a line per series per
    // sample would bury the log the emulator shares with everything else.
    let mut dropped = 0usize;
    for gauges in observed {
        for (metric, value) in gauges.series() {
            let key = cloudwatch::MetricKey {
                namespace: METRIC_NAMESPACE.to_string(),
                metric_name: metric,
                dimensions: dimensions(&gauges.domain_name),
            };
            let datum = cloudwatch::StoredDatum {
                timestamp_secs: at_secs,
                sample_count: 1.0,
                sum: value,
                minimum: value,
                maximum: value,
                unit: Some(COUNT_UNIT.to_string()),
                storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
            };
            let unit = datum.unit().to_string();
            if store.append_datapoints(key.clone(), vec![datum]).is_err() {
                dropped += 1;
                continue;
            }
            written.push(cloudwatch::WrittenSeries {
                key,
                units: vec![unit],
            });
        }
    }
    if dropped > 0 {
        ::tracing::warn!(
            dropped,
            "the CloudWatch store is at capacity, so this pass's AWS/ES readings were dropped"
        );
    }
    // The datapoints are what an alarm over the series grades, so storing them is what
    // makes the evaluation of an alarm watching one of them due.
    store.make_alarms_due(&written, at_secs);
}
