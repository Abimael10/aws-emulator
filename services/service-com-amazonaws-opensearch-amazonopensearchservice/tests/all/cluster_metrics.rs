// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ES` METRICS a provisioned domain's cluster publishes about itself, asserted
//! where a client reads them: through Amazon CloudWatch's OWN `ListMetrics`,
//! `GetMetricStatistics`, `PutMetricAlarm` and `DescribeAlarms`, with CloudWatch mounted
//! beside this service on ONE shared engine.
//!
//! ONE rule, over a SET of clusters and every gauge rather than one recorded call:
//!
//! > a domain whose cluster is up publishes the `AWS/ES` cluster gauges this service holds
//! > the records to compute — the searchable documents its indexes hold, the nodes its
//! > cluster configuration provisions and one 0/1 indicator per health colour its shard
//! > allocation leaves it in — dimensioned on the account (`ClientId`) and the
//! > domain (`DomainName`), as GAUGES observed on a cadence while the domain exists: so the
//! > series is readable before anything is indexed (the reading is then zero, which is a
//! > measurement), it follows the quantity up AND down as the cluster's contents change,
//! > and an alarm over it is evaluated from those datapoints like any other.
//!
//! The rows span the class the rule generalises over: how many documents the cluster holds
//! (none, one, several spread over two indexes), how those documents arrive and leave (a
//! bulk load, a bulk delete, an index dropped), how many nodes the configuration provisions
//! (one data node, two, and a cluster with dedicated master nodes beside its data nodes),
//! how many shards and replica copies its indexes ask for — and so which health colour the
//! allocation leaves it in — and which of the series is read. The recorded case the issue was
//! filed about — ten
//! documents bulk-loaded into one domain, with an alarm at a threshold of ten over
//! `SearchableDocuments` — is one row of it.
//!
//! Nothing here reaches into a store to arrange what it then asserts: the domains are
//! created through `CreateDomain`, the documents loaded over the domain's own REST data
//! plane, and every reading taken through CloudWatch's own operations. The harness is this
//! crate's own ([`super::domain_lifecycle`], [`super::domain_rest_data_plane`],
//! [`super::domain_rest_bulk_and_index_apis`]).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::cluster_metrics::{
    CLIENT_ID_DIMENSION, DOMAIN_NAME_DIMENSION, METRIC_NAMESPACE, NODES_METRIC,
    SEARCHABLE_DOCUMENTS_METRIC,
};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_bulk_and_index_apis::{bulk, created_index};
use super::domain_rest_data_plane::{endpoint_of, rest_prefixed};

/// How long a wait for a domain to be provisioned, for a reading to appear, or for an alarm
/// to reach a state may take at most.
///
/// Generous on purpose: the spans these waits are for are seconds at most (the provisioning
/// the clock owes a domain, the sampling cadence the gauges are observed on, the span an
/// alarm's evaluation is owed), so the first or second look finds what it waits for. This is
/// only what keeps a loaded machine from hanging the suite, and nothing here asserts on how
/// long anything took.
const DEADLINE: Duration = Duration::from_secs(90);

/// How long each look waits before looking again. Short enough that a wait costs little, and
/// non-zero so the runtime gets to run the sampler the wait is for.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `Period` every reading below is aggregated into, in seconds: the one-minute windows
/// standard-resolution datapoints are stored at ("which CloudWatch stores at 1-minute
/// resolution", `MetricDatum$StorageResolution`).
const PERIOD_SECS: i64 = 60;

/// How far back a reading looks for the series: well past one period, so the newest window
/// is in range wherever in a minute the suite happens to run.
const LOOKBACK_SECS: f64 = 600.0;

/// How far PAST now a reading's range reaches, in seconds: one period, so the window the
/// newest datapoint falls in is covered whole.
const PERIOD_ROOM_SECS: f64 = 60.0;

/// How long a wait for a WHOLE window of readings to carry one value may take at most.
///
/// Longer than [`DEADLINE`] because such a window is one that OPENED after the change being
/// waited for: the readings taken before it fall in the window the change happened in, so the
/// wait is for the next window of the period these datapoints are stored at
/// ([`PERIOD_SECS`]) — a span the suite waits out rather than asserts on. Generous on top of
/// that, so a loaded machine cannot fail the suite, and nothing here reads the clock to
/// decide anything.
const SETTLED_DEADLINE: Duration = Duration::from_secs(240);

/// Which bound of a window's readings a changed quantity shows up as.
///
/// A gauge sampled several times a minute leaves several readings in one window, so a
/// quantity that ROSE into the window is its highest reading and one that FELL into it is
/// its lowest — which is how a fall is observable at all without waiting for the window the
/// change happened in to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Highest,
    Lowest,
}

/// One cluster of the table: what it is, the configuration it is provisioned with, the
/// documents each of its indexes is loaded with, and the node census that configuration
/// provisions.
struct Row {
    case: &'static str,
    domain: &'static str,
    cluster: Value,
    /// One entry per index, each the number of documents bulk-loaded into it.
    documents: &'static [usize],
    /// The nodes the configuration above provisions — the census `DescribeDomainNodes`
    /// reports for it.
    nodes: f64,
}

/// The table: the clusters the rule is asserted over.
///
/// The node counts are the ones the cluster configuration of each row provisions: data nodes
/// always (`ClusterConfig$InstanceCount` is the "number of data nodes in the cluster"), plus
/// the dedicated master nodes of the row that turns them on
/// (`ClusterConfig$DedicatedMasterCount`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster nothing has been indexed into",
            domain: "gauges-empty",
            cluster: json!({ "InstanceType": "t3.small.search", "InstanceCount": 1 }),
            documents: &[],
            nodes: 1.0,
        },
        Row {
            case: "one index holding a single document",
            domain: "gauges-single",
            cluster: json!({ "InstanceType": "t3.small.search", "InstanceCount": 1 }),
            documents: &[1],
            nodes: 1.0,
        },
        Row {
            case: "ten documents spread over two indexes",
            domain: "gauges-spread",
            cluster: json!({ "InstanceType": "t3.small.search", "InstanceCount": 2 }),
            documents: &[3, 7],
            nodes: 2.0,
        },
        Row {
            case: "a cluster with dedicated master nodes beside its data nodes",
            domain: "gauges-master",
            cluster: json!({
                "InstanceType": "t3.small.search",
                "InstanceCount": 2,
                "DedicatedMasterEnabled": true,
                "DedicatedMasterType": "t3.small.search",
                "DedicatedMasterCount": 3,
            }),
            documents: &[2],
            nodes: 5.0,
        },
    ]
}

/// Wall-clock now as epoch seconds — the clock the metric windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// `CreateDomain` for `name` with the cluster configuration `cluster`, asserting it was
/// accepted.
async fn given_domain(emulator: &Emulator, name: &str, cluster: &Value) {
    let answer = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": cluster.clone(),
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: expected 200 from CreateDomain, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Wait until `DescribeDomain` reports the domain provisioned — the `Created && !Processing`
/// pair a client's readiness poll waits for — and answer the endpoint host it published.
///
/// The wait is for the CONDITION, not for a span of time: it looks again until the status the
/// clock owes the domain is there.
async fn provisioned(emulator: &Emulator, name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = emulator.describe(name).await;
        let status = &answer.body["DomainStatus"];
        if status["Created"].as_bool() == Some(true)
            && status["Processing"].as_bool() == Some(false)
        {
            return endpoint_of(emulator, name);
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the domain never reported itself provisioned, last read {:?}",
            answer.body
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The index of the `position`th index of a row — one name per entry of its document list.
fn index_name(position: usize) -> String {
    format!("catalog-{position}")
}

/// Bulk-load `documents` documents into `index` of the cluster at `host`, asserting the load
/// was served.
async fn given_documents(emulator: &Emulator, host: &str, index: &str, documents: usize) {
    created_index(emulator, host, index, json!({})).await;
    if documents == 0 {
        return;
    }
    let mut lines = Vec::new();
    for id in 0..documents {
        lines.push(json!({ "index": { "_index": index, "_id": id.to_string() } }));
        lines.push(json!({ "position": id }));
    }
    let answered = bulk(emulator, host, "/_bulk", &lines).await;
    assert_eq!(
        answered.status, 200,
        "case loading {documents} documents into {index:?}: expected 200 from the bulk API, \
         got {} with {:?}",
        answered.status, answered.body
    );
}

/// One `GetMetricStatistics` read of the series `metric` of `domain`, over the windows of the
/// last [`LOOKBACK_SECS`] seconds.
async fn read_series(emulator: &Emulator, domain: &str, metric: &str) -> Answer {
    // One period past now, so the window the newest datapoint falls in is inside the range
    // whichever instant of a minute this read is taken at.
    let end = now_secs() + PERIOD_ROOM_SECS;
    emulator
        .metrics_call(
            "GetMetricStatistics",
            json!({
                "Namespace": METRIC_NAMESPACE,
                "MetricName": metric,
                "Dimensions": [
                    { "Name": CLIENT_ID_DIMENSION, "Value": ::engine::emulator_account_id() },
                    { "Name": DOMAIN_NAME_DIMENSION, "Value": domain },
                ],
                "StartTime": end - LOOKBACK_SECS,
                "EndTime": end,
                "Period": PERIOD_SECS,
                "Statistics": ["Minimum", "Maximum", "SampleCount"],
            }),
        )
        .await
}

/// The NEWEST window the series holds, as the `(Minimum, Maximum)` readings it carries, or
/// `None` while the series holds no datapoint at all.
async fn newest_window(emulator: &Emulator, domain: &str, metric: &str) -> Option<(f64, f64)> {
    let answer = read_series(emulator, domain, metric).await;
    assert_eq!(
        answer.status, 200,
        "case {domain:?}/{metric}: expected 200 from GetMetricStatistics, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .max_by(|left, right| {
            left["Timestamp"]
                .as_f64()
                .unwrap_or_default()
                .total_cmp(&right["Timestamp"].as_f64().unwrap_or_default())
        })
        .map(|point| {
            (
                point["Minimum"].as_f64().unwrap_or(f64::NAN),
                point["Maximum"].as_f64().unwrap_or(f64::NAN),
            )
        })
}

/// Wait until the series' newest window carries `expected` as its `bound` reading, and fail
/// with the readings it did carry when it never does.
async fn observed(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    metric: &str,
    expected: f64,
    bound: Bound,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let window = newest_window(emulator, domain, metric).await;
        let reading = window.map(|(lowest, highest)| match bound {
            Bound::Highest => highest,
            Bound::Lowest => lowest,
        });
        if reading == Some(expected) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the {bound:?} reading of {domain:?}'s {metric} to be \
             {expected}, got {window:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The dimension sets `ListMetrics` reports for the `AWS/ES` series named `metric`, each as
/// the `(name, value)` pairs it carries, sorted.
async fn listed_dimensions(emulator: &Emulator, metric: &str) -> Vec<Vec<(String, String)>> {
    let answer = emulator
        .metrics_call(
            "ListMetrics",
            json!({ "Namespace": METRIC_NAMESPACE, "MetricName": metric }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case ListMetrics({metric}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let mut listed: Vec<Vec<(String, String)>> = answer.body["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|series| {
            let mut dimensions: Vec<(String, String)> = series["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            dimensions.sort();
            dimensions
        })
        .collect();
    listed.sort();
    listed
}

/// The dimension set a cluster-level series of `domain` is keyed by: the account and the
/// domain, which is what every `AWS/ES` cluster metric carries.
fn expected_dimensions(domain: &str) -> Vec<(String, String)> {
    let mut pairs = vec![
        (
            CLIENT_ID_DIMENSION.to_string(),
            ::engine::emulator_account_id().to_string(),
        ),
        (DOMAIN_NAME_DIMENSION.to_string(), domain.to_string()),
    ];
    pairs.sort();
    pairs
}

/// RULE: a domain whose cluster is up publishes the `AWS/ES` cluster gauges computed from its
/// own records — the documents its indexes hold and the nodes its configuration provisions —
/// under the account and domain dimensions every cluster metric carries.
#[tokio::test]
async fn every_provisioned_cluster_publishes_the_gauges_its_own_records_compute() {
    let emulator = Emulator::with_metrics();
    let table = rows();

    // Every domain is created first, so the one span the clock owes a provisioning runs for
    // all of them at once rather than once per row.
    for row in &table {
        given_domain(&emulator, row.domain, &row.cluster).await;
    }
    for row in &table {
        let host = provisioned(&emulator, row.domain).await;
        for (position, documents) in row.documents.iter().enumerate() {
            given_documents(&emulator, &host, &index_name(position), *documents).await;
        }
    }

    for row in &table {
        let documents: usize = row.documents.iter().sum();
        // The documents only ever rose on this cluster, so the loaded total is the highest
        // reading of the window it landed in.
        observed(
            &emulator,
            row.case,
            row.domain,
            SEARCHABLE_DOCUMENTS_METRIC,
            documents as f64,
            Bound::Highest,
        )
        .await;
        // The node census never changed, so both bounds of the window read it.
        observed(
            &emulator,
            row.case,
            row.domain,
            NODES_METRIC,
            row.nodes,
            Bound::Lowest,
        )
        .await;
    }

    // And the series are the ones CloudWatch LISTS for the namespace, one per domain and
    // metric, each under the account and domain dimensions.
    for metric in [SEARCHABLE_DOCUMENTS_METRIC, NODES_METRIC] {
        let listed = listed_dimensions(&emulator, metric).await;
        let mut expected: Vec<Vec<(String, String)>> = table
            .iter()
            .map(|row| expected_dimensions(row.domain))
            .collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case ListMetrics({metric}): expected one series per provisioned domain under the \
             account and domain dimensions, got {listed:?}"
        );
    }
}

/// RULE (the gauge half): the series FOLLOWS what the cluster holds, up and down — so
/// removing documents is observable on it, which a counter could never report.
#[tokio::test]
async fn the_published_gauge_follows_the_documents_the_cluster_holds() {
    let emulator = Emulator::with_metrics();
    let domain = "gauges-changing";
    let index = "orders";
    given_domain(
        &emulator,
        domain,
        &json!({ "InstanceType": "t3.small.search", "InstanceCount": 1 }),
    )
    .await;
    let host = provisioned(&emulator, domain).await;

    // Nothing indexed yet: the cluster holds no document, and that is a reading rather than
    // an absent series.
    observed(
        &emulator,
        "an empty cluster",
        domain,
        SEARCHABLE_DOCUMENTS_METRIC,
        0.0,
        Bound::Lowest,
    )
    .await;

    // A bulk load of ten documents — the shape the recorded case loads them in.
    given_documents(&emulator, &host, index, 10).await;
    observed(
        &emulator,
        "ten documents bulk-loaded",
        domain,
        SEARCHABLE_DOCUMENTS_METRIC,
        10.0,
        Bound::Highest,
    )
    .await;

    // Four of them deleted: the quantity FELL, so the window it fell in reads it as its
    // lowest reading.
    let deletes: Vec<Value> = (0..4)
        .map(|id| json!({ "delete": { "_index": index, "_id": id.to_string() } }))
        .collect();
    let deleted = bulk(&emulator, &host, "/_bulk", &deletes).await;
    assert_eq!(
        deleted.status, 200,
        "case four documents deleted: expected 200 from the bulk API, got {} with {:?}",
        deleted.status, deleted.body
    );
    observed(
        &emulator,
        "four documents deleted",
        domain,
        SEARCHABLE_DOCUMENTS_METRIC,
        6.0,
        Bound::Lowest,
    )
    .await;

    // And the whole index dropped: the cluster is back to holding nothing.
    let dropped = rest_prefixed(&emulator, domain, "DELETE", &format!("/{index}"), None).await;
    assert_eq!(
        dropped.status, 200,
        "case the index dropped: expected 200 from the index delete, got {} with {:?}",
        dropped.status, dropped.body
    );
    observed(
        &emulator,
        "the index dropped",
        domain,
        SEARCHABLE_DOCUMENTS_METRIC,
        0.0,
        Bound::Lowest,
    )
    .await;
}

/// The `AWS/ES` cluster-status INDICATOR series, each beside the health colour the engine
/// reports for it.
///
/// Written out here rather than taken from the crate, so the test pins the names a client
/// subscribes to: the cluster metrics of the Amazon OpenSearch Service developer guide
/// (`managedomains-cloudwatchmetrics`) name them `ClusterStatus.green`,
/// `ClusterStatus.yellow` and `ClusterStatus.red`, and the colours are the ones the engine's
/// own `_cluster/health` answers with.
const CLUSTER_STATUS_SERIES: [(&str, &str); 3] = [
    ("ClusterStatus.green", "green"),
    ("ClusterStatus.yellow", "yellow"),
    ("ClusterStatus.red", "red"),
];

/// One cluster of the cluster-status table: what it is, how many data nodes it is
/// provisioned with, the settings of the one index it is given (none at all for a cluster
/// holding no index), and the health colour that allocation leaves it in.
struct StatusRow {
    case: &'static str,
    domain: &'static str,
    nodes: i64,
    /// The `settings` section the index is created with, or `None` for a cluster that is
    /// given no index at all.
    index_settings: Option<Value>,
    /// The colour of [`CLUSTER_STATUS_SERIES`] whose indicator reads 1.
    status: &'static str,
}

/// The cluster-status table: the allocations the rule is asserted over.
///
/// The rows differ along what the rule generalises over — how many data nodes the cluster
/// provisions, how many shards and replica COPIES of each its index asks for, and whether it
/// holds an index at all — and so over both colours a cluster of this emulator reaches: a
/// replica copy needs a node other than the one holding its primary, so an index asking for
/// more copies than the topology can hold leaves copies unassigned (`yellow`) and one whose
/// copies all fit is fully allocated (`green`).
fn status_rows() -> Vec<StatusRow> {
    vec![
        StatusRow {
            case: "a cluster holding no index at all",
            domain: "status-bare",
            nodes: 1,
            index_settings: None,
            status: "green",
        },
        StatusRow {
            case: "one node and an index asking for the default replica copy",
            domain: "status-default",
            nodes: 1,
            index_settings: Some(json!({})),
            status: "yellow",
        },
        StatusRow {
            case: "one node and an index asking for no replica copy",
            domain: "status-unreplicated",
            nodes: 1,
            index_settings: Some(json!({ "number_of_shards": 3, "number_of_replicas": 0 })),
            status: "green",
        },
        StatusRow {
            case: "two nodes and an index whose single copy they can hold",
            domain: "status-fitting",
            nodes: 2,
            index_settings: Some(json!({ "number_of_shards": 2, "number_of_replicas": 1 })),
            status: "green",
        },
        StatusRow {
            case: "two nodes and more replica copies than they can hold",
            domain: "status-over",
            nodes: 2,
            index_settings: Some(json!({ "number_of_shards": 1, "number_of_replicas": 2 })),
            status: "yellow",
        },
    ]
}

/// The cluster configuration of a row: `nodes` data nodes of the instance type every row
/// uses.
fn cluster_of(nodes: i64) -> Value {
    json!({ "InstanceType": "t3.small.search", "InstanceCount": nodes })
}

/// The status the cluster's OWN `GET /_cluster/health` reports for `domain`.
async fn reported_health(emulator: &Emulator, case: &str, domain: &str) -> String {
    let answer = rest_prefixed(emulator, domain, "GET", "/_cluster/health", None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from GET /_cluster/health, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Wait until a WHOLE window of the series reads `expected` — its lowest and its highest
/// reading alike — and fail with the readings it did carry when it never does.
///
/// A window every reading of which is `expected` is one that OPENED after the change being
/// waited for, since the readings taken before it fall in the window the change happened in.
/// That is what makes a value which changed BACK to one the series already carried
/// observable, rather than hidden under the earlier readings of the window it changed in: the
/// series is read at the period its datapoints are stored at ([`PERIOD_SECS`]) — the only
/// granularity a standard-resolution series is served at — and the wait is for the next such
/// window to fill ([`SETTLED_DEADLINE`]).
async fn settled_at(emulator: &Emulator, case: &str, domain: &str, metric: &str, expected: f64) {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let window = newest_window(emulator, domain, metric).await;
        if window == Some((expected, expected)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected every reading of {domain:?}'s newest {metric} window to be \
             {expected}, got {window:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until every cluster-status series of `domain` carries the indicator the colour
/// `status` implies: 1 for that colour and 0 for the other two.
async fn status_indicators(emulator: &Emulator, case: &str, domain: &str, status: &str) {
    for (metric, colour) in CLUSTER_STATUS_SERIES {
        let expected = if colour == status { 1.0 } else { 0.0 };
        settled_at(emulator, case, domain, metric, expected).await;
    }
}

/// RULE: a domain whose cluster is up publishes one `AWS/ES` cluster-status series per health
/// colour, each a 0/1 indicator of whether the cluster's own shard allocation leaves it in
/// that colour — the same allocation its `_cluster/health` reports — dimensioned on the
/// account (`ClientId`) and the domain (`DomainName`).
#[tokio::test]
async fn every_provisioned_cluster_publishes_the_status_indicator_of_its_own_allocation() {
    let emulator = Emulator::with_metrics();
    let table = status_rows();

    // Every domain is created first, so the one span the clock owes a provisioning runs for
    // all of them at once rather than once per row.
    for row in &table {
        given_domain(&emulator, row.domain, &cluster_of(row.nodes)).await;
    }
    for row in &table {
        let host = provisioned(&emulator, row.domain).await;
        if let Some(settings) = &row.index_settings {
            created_index(
                &emulator,
                &host,
                "catalog",
                json!({ "settings": settings.clone() }),
            )
            .await;
        }
    }

    for row in &table {
        // The colour the cluster's own health read reports, which is what the indicators are
        // the published reading of.
        let health = reported_health(&emulator, row.case, row.domain).await;
        assert_eq!(
            health, row.status,
            "case {:?}: expected the cluster's own health to be {:?}, got {health:?}",
            row.case, row.status
        );
        status_indicators(&emulator, row.case, row.domain, row.status).await;
    }

    // And every colour's series is one CloudWatch LISTS for the namespace, one per domain,
    // each under the account and domain dimensions and no others.
    for (metric, _) in CLUSTER_STATUS_SERIES {
        let listed = listed_dimensions(&emulator, metric).await;
        let mut expected: Vec<Vec<(String, String)>> = table
            .iter()
            .map(|row| expected_dimensions(row.domain))
            .collect();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case ListMetrics({metric}): expected one series per provisioned domain under the \
             account and domain dimensions, got {listed:?}"
        );
    }
}

/// RULE (the gauge half): the cluster-status indicators FOLLOW the allocation, in both
/// directions — an index asking for more replica copies than the topology can hold turns the
/// cluster yellow, and lowering the count to what fits turns it green again.
#[tokio::test]
async fn the_published_cluster_status_follows_the_allocation_in_both_directions() {
    let emulator = Emulator::with_metrics();
    let domain = "status-changing";
    let index = "orders";
    given_domain(&emulator, domain, &cluster_of(2)).await;
    let host = provisioned(&emulator, domain).await;

    // Nothing indexed yet: every copy of nothing is allocated, which is a green cluster and
    // a reading rather than an absent series.
    status_indicators(&emulator, "a cluster holding no index", domain, "green").await;

    // An index asking for two copies of its shard on a cluster of two nodes: one copy has
    // nowhere to go, so the cluster is yellow.
    created_index(
        &emulator,
        &host,
        index,
        json!({ "settings": { "number_of_shards": 1, "number_of_replicas": 2 } }),
    )
    .await;
    let yellow = reported_health(&emulator, "an over-replicated index", domain).await;
    assert_eq!(
        yellow, "yellow",
        "case \"an over-replicated index\": expected the cluster's own health to be yellow, \
         got {yellow:?}"
    );
    status_indicators(&emulator, "an over-replicated index", domain, "yellow").await;

    // Lowered to what the topology can hold: every copy is assigned again, so the cluster is
    // green — the transition a client waits for, observable on the series.
    let lowered = rest_prefixed(
        &emulator,
        domain,
        "PUT",
        &format!("/{index}/_settings"),
        Some(json!({ "index": { "number_of_replicas": 1 } })),
    )
    .await;
    assert_eq!(
        (lowered.status, lowered.body["acknowledged"].as_bool()),
        (200, Some(true)),
        "case \"lowering the replica count\": expected the settings update to be acknowledged, \
         got {} with {:?}",
        lowered.status,
        lowered.body
    );
    let green = reported_health(&emulator, "the replica count lowered", domain).await;
    assert_eq!(
        green, "green",
        "case \"the replica count lowered\": expected the cluster's own health to be green, \
         got {green:?}"
    );
    status_indicators(&emulator, "the replica count lowered", domain, "green").await;
}

/// One alarm of the alarm table: what it watches, the threshold and comparison it is
/// configured with, and the state it must reach once the cluster is loaded.
struct AlarmRow {
    case: &'static str,
    metric: &'static str,
    threshold: f64,
    comparison: &'static str,
    loaded_state: &'static str,
}

/// `PutMetricAlarm` over the `AWS/ES` series of `domain`, asserting it was accepted.
async fn given_alarm(emulator: &Emulator, name: &str, domain: &str, row: &AlarmRow) {
    let answer = emulator
        .metrics_call(
            "PutMetricAlarm",
            json!({
                "AlarmName": name,
                "Namespace": METRIC_NAMESPACE,
                "MetricName": row.metric,
                "Dimensions": [
                    { "Name": CLIENT_ID_DIMENSION, "Value": ::engine::emulator_account_id() },
                    { "Name": DOMAIN_NAME_DIMENSION, "Value": domain },
                ],
                "Statistic": "Maximum",
                "Period": PERIOD_SECS,
                "EvaluationPeriods": 1,
                "Threshold": row.threshold,
                "ComparisonOperator": row.comparison,
                "TreatMissingData": "ignore",
            }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {}: expected 200 from PutMetricAlarm, got {} with {:?}",
        row.case, answer.status, answer.body
    );
}

/// Wait until `DescribeAlarms` reports `name` in `state`, and fail with the alarm it did
/// report when it never does.
async fn alarm_reaches(emulator: &Emulator, case: &str, name: &str, state: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let answer = emulator
            .metrics_call("DescribeAlarms", json!({ "AlarmNames": [name] }))
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200 from DescribeAlarms, got {} with {:?}",
            answer.status, answer.body
        );
        let alarm = answer.body["MetricAlarms"][0].clone();
        if alarm["StateValue"].as_str() == Some(state) {
            return alarm;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected alarm {name:?} to reach {state:?}, got {alarm:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE (the alarm half): an alarm over a published series is evaluated from the datapoints
/// the cluster publishes — so it reads `OK` while the gauge is below its threshold and moves
/// to `ALARM` once the cluster's own contents cross it.
///
/// The rows span both published series and both directions of the comparison: the recorded
/// case's `SearchableDocuments` alarm at a threshold of ten, the same series under a
/// threshold the load does not reach, and a `Nodes` alarm over the census the cluster
/// configuration provisions.
#[tokio::test]
async fn an_alarm_over_a_published_series_evaluates_from_the_published_datapoints() {
    let emulator = Emulator::with_metrics();
    let domain = "gauges-alarmed";
    given_domain(
        &emulator,
        domain,
        &json!({ "InstanceType": "t3.small.search", "InstanceCount": 2 }),
    )
    .await;
    let host = provisioned(&emulator, domain).await;

    let table = [
        AlarmRow {
            case: "the documents reaching the threshold of the recorded case",
            metric: SEARCHABLE_DOCUMENTS_METRIC,
            threshold: 10.0,
            comparison: "GreaterThanOrEqualToThreshold",
            loaded_state: "ALARM",
        },
        AlarmRow {
            case: "a threshold the load does not reach",
            metric: SEARCHABLE_DOCUMENTS_METRIC,
            threshold: 100.0,
            comparison: "GreaterThanOrEqualToThreshold",
            loaded_state: "OK",
        },
        AlarmRow {
            case: "the node census of the provisioned cluster",
            metric: NODES_METRIC,
            threshold: 2.0,
            comparison: "GreaterThanOrEqualToThreshold",
            loaded_state: "ALARM",
        },
    ];
    let names: Vec<String> = table
        .iter()
        .enumerate()
        .map(|(position, _)| format!("{domain}-{position}"))
        .collect();
    for (row, name) in table.iter().zip(&names) {
        given_alarm(&emulator, name, domain, row).await;
    }

    // Before anything is indexed, every alarm over the documents reads OK: the series is
    // flowing and it is below each threshold. The node alarm is already breaching, since the
    // census it watches is what the cluster really runs.
    for (row, name) in table.iter().zip(&names) {
        let expected = if row.metric == NODES_METRIC {
            "ALARM"
        } else {
            "OK"
        };
        alarm_reaches(&emulator, row.case, name, expected).await;
    }

    // The load the recorded case makes: ten documents over the domain's own endpoint.
    given_documents(&emulator, &host, "orders", 10).await;

    for (row, name) in table.iter().zip(&names) {
        let alarm = alarm_reaches(&emulator, row.case, name, row.loaded_state).await;
        // The explanation names the datapoints the evaluation graded and the threshold it
        // compared them against, so a client can tell an alarm that saw this cluster's data
        // from one that saw none. The expected text is built from the ROW's own threshold —
        // the sentence states a measurement, so a whole number carries its fraction.
        let reason = alarm["StateReason"].as_str().unwrap_or_default();
        let names_threshold = format!("the threshold ({:.1})", row.threshold);
        assert!(
            reason.contains(&names_threshold),
            "case {:?}: expected the evaluated state's explanation to name {names_threshold:?}, \
             got {alarm:?}",
            row.case
        );
    }

    // The transition the load drove is in the alarm's own history, as a `StateUpdate` item.
    let history = emulator
        .metrics_call(
            "DescribeAlarmHistory",
            json!({ "AlarmName": names[0], "HistoryItemType": "StateUpdate" }),
        )
        .await;
    assert_eq!(
        history.status, 200,
        "case the alarm history: expected 200 from DescribeAlarmHistory, got {} with {:?}",
        history.status, history.body
    );
    let items = history.body["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !items.is_empty(),
        "case the alarm history: expected the state updates the evaluation recorded, got {:?}",
        history.body
    );
}
