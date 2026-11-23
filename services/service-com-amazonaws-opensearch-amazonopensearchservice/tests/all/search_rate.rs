// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ES` SEARCH-REQUEST COUNT a provisioned domain publishes for the searches its data
//! plane served, asserted where a client reads it: through Amazon CloudWatch's OWN
//! `GetMetricData` and `ListMetrics`, with CloudWatch mounted beside this service on ONE
//! shared engine.
//!
//! ONE rule, over a SET of clusters and every form a search arrives in rather than one
//! recorded call:
//!
//! > every request a domain's cluster EXECUTES a search query for — a `_search`, a `_count`,
//! > whichever index set it names and whichever form it carries its query in — is counted once
//! > into that domain's `SearchRate` series of the `AWS/ES` namespace, dimensioned on the
//! > account (`ClientId`) and the domain (`DomainName`) alone and stored at the namespace's
//! > one-minute resolution: so the `Sum` of the series over the window the requests fell in is
//! > the number of searches the cluster served, and a request it REFUSED before executing one
//! > — like work of another kind, a document load — is counted into nothing.
//!
//! The rows span the class the rule generalises over: how many searches the cluster served
//! (none, one, a few, the twenty of the recorded case), which API served them (`_search`,
//! `_count`, and the two mixed), which index set each named (one index, and the whole cluster),
//! the form the query arrived in (a request body, and the URI's own `q`), and whether the
//! cluster executed the search at all (a refused page window and a method the route does not
//! take). The recorded case the issue was filed about — twenty searches over an index holding
//! twenty-five bulk-loaded documents — is one row of it.
//!
//! Nothing here reaches into a store to arrange what it then asserts: the domains are created
//! through `CreateDomain`, the documents loaded and the searches issued over the domain's own
//! REST data plane, and every reading taken through CloudWatch's own operations. The harness is
//! this crate's own ([`super::domain_lifecycle`], [`super::domain_rest_data_plane`],
//! [`super::domain_rest_bulk_and_index_apis`]).

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state::{
    cluster_metrics::{CLIENT_ID_DIMENSION, DOMAIN_NAME_DIMENSION, METRIC_NAMESPACE},
    search_rate::SEARCH_RATE_METRIC,
};

use super::domain_lifecycle::{settled_domain, Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use super::domain_rest_bulk_and_index_apis::{bulk, created_index};
use super::domain_rest_data_plane::{endpoint_of, rest};

/// The index every row loads its documents into and searches.
const INDEX: &str = "catalog";

/// How many documents every row's index is loaded with — the volume of the recorded case,
/// which is also what the searches below select.
const DOCUMENTS: usize = 25;

/// The `Period` every reading below is aggregated into, in seconds: the one-minute windows
/// standard-resolution datapoints are stored at ("which CloudWatch stores at 1-minute
/// resolution", `MetricDatum$StorageResolution`).
const PERIOD_SECS: i64 = 60;

/// How far back a reading looks for the series: well past one period, so every window the
/// searches of a row could have fallen in is in range.
const LOOKBACK_SECS: f64 = 1_800.0;

/// How far PAST now a reading's range reaches, in seconds: one period, so the window the newest
/// datapoint falls in is covered whole wherever in a minute the suite happens to run.
const PERIOD_ROOM_SECS: f64 = 60.0;

/// One data-plane call of a row: what it asks, and the status the cluster must answer it with.
struct Call {
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    /// How many times the row makes this call.
    repeat: usize,
}

/// One cluster of the table: what it is, the calls its data plane is driven with, and how many
/// searches those calls had the cluster execute.
struct Row {
    case: &'static str,
    domain: &'static str,
    calls: Vec<Call>,
    /// The `Sum` the domain's `SearchRate` series must carry over the window the calls fell in.
    searches: f64,
}

/// A `POST` of `body` to `path`, made `repeat` times, answered 200.
fn searched(path: &str, body: Value, repeat: usize) -> Call {
    Call {
        method: "POST",
        path: path.to_string(),
        body: Some(body),
        status: 200,
        repeat,
    }
}

/// The query every row's searches carry: the one that selects every document loaded below, so
/// each search really reads the index rather than matching nothing.
fn match_all() -> Value {
    json!({ "query": { "match_all": {} } })
}

/// The table: the clusters the rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a cluster whose documents were loaded and never searched",
            domain: "rate-unsearched",
            calls: Vec::new(),
            searches: 0.0,
        },
        Row {
            case: "one index-scoped search carrying its query in the body",
            domain: "rate-single",
            calls: vec![searched(&format!("/{INDEX}/_search"), match_all(), 1)],
            searches: 1.0,
        },
        Row {
            case: "the twenty searches of the recorded case",
            domain: "rate-recorded",
            calls: vec![searched(&format!("/{INDEX}/_search"), match_all(), 20)],
            searches: 20.0,
        },
        Row {
            case: "searches naming no index, which read the whole cluster",
            domain: "rate-cluster-wide",
            calls: vec![searched("/_search", match_all(), 3)],
            searches: 3.0,
        },
        Row {
            case: "searches carrying their query in the URI instead of a body",
            domain: "rate-uri-query",
            calls: vec![Call {
                method: "GET",
                path: format!("/{INDEX}/_search?q=position:1"),
                body: None,
                status: 200,
                repeat: 2,
            }],
            searches: 2.0,
        },
        Row {
            case: "counts, which execute the same query and report only its total",
            domain: "rate-counts",
            calls: vec![
                searched(&format!("/{INDEX}/_count"), match_all(), 2),
                Call {
                    method: "GET",
                    path: format!("/{INDEX}/_count"),
                    body: None,
                    status: 200,
                    repeat: 1,
                },
            ],
            searches: 3.0,
        },
        Row {
            case: "searches and counts mixed, which are counted into the one series",
            domain: "rate-mixed",
            calls: vec![
                searched(&format!("/{INDEX}/_search"), match_all(), 4),
                searched(&format!("/{INDEX}/_count"), match_all(), 2),
                searched("/_search", match_all(), 1),
            ],
            searches: 7.0,
        },
        Row {
            case: "searches the cluster refused before it executed one",
            domain: "rate-refused",
            calls: vec![
                // A page past the result window the engine serves, and a method the search
                // route does not take: neither reaches the documents, so neither is a search
                // the cluster executed.
                Call {
                    method: "POST",
                    path: format!("/{INDEX}/_search"),
                    body: Some(json!({ "from": 0, "size": 10_001 })),
                    status: 400,
                    repeat: 2,
                },
                Call {
                    method: "PUT",
                    path: format!("/{INDEX}/_search"),
                    body: Some(match_all()),
                    status: 405,
                    repeat: 1,
                },
                // And a search of an index the domain does not hold.
                Call {
                    method: "POST",
                    path: "/absent/_search".to_string(),
                    body: Some(match_all()),
                    status: 404,
                    repeat: 1,
                },
            ],
            searches: 0.0,
        },
    ]
}

/// Wall-clock now as epoch seconds — the clock the metric windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// `CreateDomain` for `name`, asserting it was accepted: the configuration of the recorded
/// case — one `t3.small.search` data node on gp3 storage.
async fn given_domain(emulator: &Emulator, name: &str) {
    let answer = emulator
        .create(json!({
            "DomainName": name,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {name:?}: expected 200 from CreateDomain, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Bulk-load [`DOCUMENTS`] documents into [`INDEX`] of the cluster at `host`, asserting the
/// load was served — work of another kind than a search, so it is counted into nothing.
async fn given_documents(emulator: &Emulator, case: &str, host: &str) {
    created_index(emulator, host, INDEX, json!({})).await;
    let mut lines = Vec::new();
    for id in 0..DOCUMENTS {
        lines.push(json!({ "index": { "_index": INDEX, "_id": id.to_string() } }));
        lines.push(json!({ "position": id }));
    }
    let answered = bulk(emulator, host, "/_bulk?refresh=true", &lines).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected 200 from the bulk API, got {} with {:?}",
        answered.status, answered.body
    );
}

/// Make every call of `row` against the cluster at `host`, asserting each is answered with the
/// status the row states.
async fn drive(emulator: &Emulator, row: &Row, host: &str) {
    for call in &row.calls {
        for attempt in 0..call.repeat {
            let answer = rest(
                emulator,
                DEFAULT_ACCESS_KEY_ID,
                host,
                call.method,
                &call.path,
                call.body.clone(),
            )
            .await;
            assert_eq!(
                answer.status, call.status,
                "case {:?}: expected {} from {} {} (call {}), got {} with {:?}",
                row.case,
                call.status,
                call.method,
                call.path,
                attempt + 1,
                answer.status,
                answer.body
            );
        }
    }
}

/// The dimension set a cluster-level series of `domain` is keyed by: the account and the
/// domain, which is what every `AWS/ES` series of a cluster carries.
fn dimensions_of(domain: &str) -> Vec<(String, String)> {
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

/// Those dimensions as a request states them.
fn stated_dimensions(domain: &str) -> Value {
    Value::Array(
        dimensions_of(domain)
            .into_iter()
            .map(|(name, value)| json!({ "Name": name, "Value": value }))
            .collect(),
    )
}

/// One `GetMetricData` read of `domain`'s `SearchRate` series, exactly as a client reading the
/// load watches it: the per-minute `Sum` over the windows of the last [`LOOKBACK_SECS`]
/// seconds.
async fn read_rate(emulator: &Emulator, domain: &str) -> Answer {
    let end = now_secs() + PERIOD_ROOM_SECS;
    emulator
        .metrics_call(
            "GetMetricData",
            json!({
                "MetricDataQueries": [{
                    "Id": "rate",
                    "MetricStat": {
                        "Metric": {
                            "Namespace": METRIC_NAMESPACE,
                            "MetricName": SEARCH_RATE_METRIC,
                            "Dimensions": stated_dimensions(domain),
                        },
                        "Period": PERIOD_SECS,
                        "Stat": "Sum",
                    },
                }],
                "StartTime": end - LOOKBACK_SECS,
                "EndTime": end,
                "ScanBy": "TimestampAscending",
            }),
        )
        .await
}

/// The TOTAL the per-minute sums of that series carry over the whole window — what a client
/// summing the series over the minutes its searches ran reads.
async fn windowed_sum(emulator: &Emulator, case: &str, domain: &str) -> f64 {
    let answer = read_rate(emulator, domain).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from GetMetricData, got {} with {:?}",
        answer.status, answer.body
    );
    let results = answer.body["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        1,
        "case {case:?}: a one-query read answers one result, got {:?}",
        answer.body
    );
    results[0]["Values"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_f64)
        .sum()
}

/// RULE: every request a domain's cluster executes a search query for is counted once into
/// that domain's `AWS/ES` `SearchRate` series, so the windowed `Sum` of the series is the
/// number of searches the cluster served — and nothing the cluster refused or served as work
/// of another kind is counted.
#[tokio::test]
async fn every_search_a_cluster_executes_is_counted_into_its_search_rate() {
    let emulator = Emulator::with_metrics();
    let table = rows();

    // Every domain is created first, so the one span the clock owes a provisioning runs for
    // all of them at once rather than once per row.
    for row in &table {
        given_domain(&emulator, row.domain).await;
    }
    let mut hosts = Vec::new();
    for row in &table {
        settled_domain(&emulator, row.domain).await;
        let host = endpoint_of(&emulator, row.domain);
        given_documents(&emulator, row.case, &host).await;
        hosts.push(host);
    }

    for (row, host) in table.iter().zip(&hosts) {
        drive(&emulator, row, host).await;
        let counted = windowed_sum(&emulator, row.case, row.domain).await;
        assert_eq!(
            counted, row.searches,
            "case {:?}: expected the windowed Sum of {:?}'s {SEARCH_RATE_METRIC} to be {}, \
             got {counted}",
            row.case, row.domain, row.searches
        );
    }
}

/// The dimension sets `ListMetrics` reports for the `AWS/ES` `SearchRate` series, each as the
/// `(name, value)` pairs it carries, sorted.
async fn listed_dimensions(emulator: &Emulator) -> Vec<Vec<(String, String)>> {
    let answer = emulator
        .metrics_call(
            "ListMetrics",
            json!({ "Namespace": METRIC_NAMESPACE, "MetricName": SEARCH_RATE_METRIC }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case ListMetrics({SEARCH_RATE_METRIC}): expected 200, got {} with {:?}",
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

/// RULE (the same one, read through the metric CATALOGUE): the `SearchRate` series a cluster
/// publishes is listed under the account and domain dimensions ALONE, and only for a cluster
/// that served a search.
#[tokio::test]
async fn the_search_rate_catalogue_lists_the_clusters_that_served_a_search() {
    let emulator = Emulator::with_metrics();
    // Two clusters, one searched and one not, so what the catalogue reports is asserted
    // against a cluster that must be in it and one that must not.
    let searched_domain = "catalogue-searched";
    let idle_domain = "catalogue-idle";
    for domain in [searched_domain, idle_domain] {
        given_domain(&emulator, domain).await;
    }
    for domain in [searched_domain, idle_domain] {
        settled_domain(&emulator, domain).await;
        let host = endpoint_of(&emulator, domain);
        given_documents(&emulator, domain, &host).await;
    }

    let host = endpoint_of(&emulator, searched_domain);
    let answered = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(match_all()),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {searched_domain:?}: expected 200 from the search, got {} with {:?}",
        answered.status, answered.body
    );

    let listed = listed_dimensions(&emulator).await;
    assert_eq!(
        listed,
        vec![dimensions_of(searched_domain)],
        "case {searched_domain:?}: expected the catalogue to list the searched cluster's \
         series under the account and domain alone, got {listed:?}"
    );
}
