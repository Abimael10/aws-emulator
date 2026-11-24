// SPDX-License-Identifier: CC0-1.0

//! The `AWS/ES` SEARCH-REQUEST COUNT a domain publishes, read the way a client WATCHING the
//! query load reads it: over the BOUNDED window its searches fell in rather than over a wide
//! lookback, through Amazon CloudWatch's own `GetMetricData` with CloudWatch mounted beside
//! this service on ONE shared engine.
//!
//! ONE rule, over a set of clusters and every route that has the cluster run a query:
//!
//! > every request a domain's cluster EXECUTES a search query for is counted once into that
//! > domain's `SearchRate` series of the `AWS/ES` namespace, dimensioned on the account
//! > (`ClientId`) and the domain (`DomainName`), and the series is a COUNT OF EVENTS: the
//! > per-minute `Sum`s over the window from the minute the requests began through two minutes
//! > past the minute they ended total the number of searches the cluster served, whatever width
//! > of window a reader folds the series into, while a request that had the cluster execute no
//! > query at all leaves the window carrying nothing.
//!
//! The axes the rows differ along are the ones the rule generalises over and the SIBLING suite
//! ([`super::search_rate`], which asserts the same rule over the forms a `_search` and a
//! `_count` arrive in and over the requests the cluster refuses) does not: the ROUTE the query
//! was executed by — a `_search`, and the two forms a `_delete_by_query` arrives in, which
//! resolves the documents it removes with a search of its own — the NUMBER of searches a
//! window carries (none, one, two, the twenty of the recorded case), and the PERIOD the reader
//! folds the window into (one minute, and five).
//!
//! Nothing here arranges in a store what it then asserts: the domains are created through
//! `CreateDomain`, the documents loaded and the queries issued over the domain's own REST data
//! plane, and every reading taken through CloudWatch's own operation. The harness is this
//! crate's own ([`super::domain_lifecycle`], [`super::domain_rest_data_plane`],
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

/// The index every row loads its documents into and queries.
const INDEX: &str = "catalog";

/// How many documents every row's index is loaded with — the volume of the recorded case.
const DOCUMENTS: usize = 25;

/// The periods a row's window is folded into, in seconds: the one-minute windows standard
/// resolution datapoints are stored at ("which CloudWatch stores at 1-minute resolution",
/// `MetricDatum$StorageResolution`), and the five-minute windows a wider dashboard lays over the
/// same series. A count of events totals the same over a window either way.
const PERIODS_SECS: [i64; 2] = [60, 300];

/// How far PAST the minute the requests ended in a reading's range reaches, in seconds: the two
/// minutes a client summing the load allows for, which is the window the recorded case reads.
const TRAILING_WINDOW_SECS: f64 = 120.0;

/// One data-plane call of a row: what it asks, and the status the cluster must answer it with.
struct Call {
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    /// How many times the row makes this call.
    repeat: usize,
}

/// One cluster of the table: what it is, the calls its data plane is driven with inside the
/// window, and how many searches those calls had the cluster execute.
struct Row {
    case: &'static str,
    domain: &'static str,
    calls: Vec<Call>,
    /// The total the per-minute `Sum`s of the domain's `SearchRate` series must carry over the
    /// window the calls fell in.
    searches: f64,
}

/// A `POST` of `body` to `path`, made `repeat` times, answered 200.
fn posted(path: &str, body: Value, repeat: usize) -> Call {
    Call {
        method: "POST",
        path: path.to_string(),
        body: Some(body),
        status: 200,
        repeat,
    }
}

/// The query a row's searches carry: the one that selects every document loaded below.
fn match_all() -> Value {
    json!({ "query": { "match_all": {} } })
}

/// The query a row's by-query DELETE carries: one document of the load, so the write really
/// removes something and the search that resolved it is a search of the index.
fn one_document() -> Value {
    json!({ "query": { "term": { "position": 0 } } })
}

/// The table: the clusters the rule is asserted over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a window the cluster executed no query in",
            domain: "window-idle",
            calls: vec![
                // Reading one document by its id and refreshing the index are work of another
                // kind: neither has the cluster run a query.
                Call {
                    method: "GET",
                    path: format!("/{INDEX}/_doc/0"),
                    body: None,
                    status: 200,
                    repeat: 1,
                },
                Call {
                    method: "POST",
                    path: format!("/{INDEX}/_refresh"),
                    body: None,
                    status: 200,
                    repeat: 1,
                },
            ],
            searches: 0.0,
        },
        Row {
            case: "a window carrying one search",
            domain: "window-single",
            calls: vec![posted(&format!("/{INDEX}/_search"), match_all(), 1)],
            searches: 1.0,
        },
        Row {
            case: "the twenty searches of the recorded case",
            domain: "window-recorded",
            calls: vec![posted(&format!("/{INDEX}/_search"), match_all(), 20)],
            searches: 20.0,
        },
        Row {
            case: "a waiting delete-by-query, whose documents one search resolved",
            domain: "window-by-query",
            calls: vec![posted(
                &format!("/{INDEX}/_delete_by_query"),
                one_document(),
                1,
            )],
            searches: 1.0,
        },
        Row {
            case: "an offloaded delete-by-query, whose subset the same search resolved",
            domain: "window-by-query-offloaded",
            calls: vec![posted(
                &format!("/{INDEX}/_delete_by_query?wait_for_completion=false"),
                one_document(),
                1,
            )],
            searches: 1.0,
        },
        Row {
            case: "a search and a by-query delete, counted into the one series",
            domain: "window-mixed",
            calls: vec![
                posted(&format!("/{INDEX}/_search"), match_all(), 1),
                posted(&format!("/{INDEX}/_delete_by_query"), one_document(), 1),
            ],
            searches: 2.0,
        },
    ]
}

/// Wall-clock now as epoch seconds — the clock the metric windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The MINUTE an instant falls in, as the epoch second that minute begins at — the boundary a
/// client states the window of its searches in.
fn minute_of(at_secs: f64) -> f64 {
    (at_secs / 60.0).floor() * 60.0
}

/// The widest of [`PERIODS_SECS`], in seconds: the boundary a row's window opens on
/// ([`window_start`]).
const WIDEST_PERIOD_SECS: f64 = 300.0;

/// The instant a row's window OPENS at: the minute the first request fell in, taken down to a
/// boundary of the widest period the window is folded into, so what each reading answers over is
/// the same span of the series however that reader lays its windows out.
fn window_start(at_secs: f64) -> f64 {
    (minute_of(at_secs) / WIDEST_PERIOD_SECS).floor() * WIDEST_PERIOD_SECS
}

/// `CreateDomain` for `name`, asserting it was accepted: the configuration of the recorded case
/// — one `t3.small.search` data node on gp3 storage.
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

/// Bulk-load [`DOCUMENTS`] documents into [`INDEX`] of the cluster at `host`, asserting the load
/// was served — work of another kind than a search, and done BEFORE a row's window opens.
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
/// status the row states, and answer the WINDOW they fell in: the minute the first began
/// ([`window_start`]) through two minutes past the minute the last ended.
async fn drive(emulator: &Emulator, row: &Row, host: &str) -> (f64, f64) {
    let opened = now_secs();
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
    (
        window_start(opened),
        minute_of(now_secs()) + TRAILING_WINDOW_SECS,
    )
}

/// The dimensions a cluster-level series of `domain` is keyed by, as a request states them: the
/// account and the domain, which is the pair every `AWS/ES` series of a cluster carries.
fn stated_dimensions(domain: &str) -> Value {
    json!([
        { "Name": CLIENT_ID_DIMENSION, "Value": ::engine::emulator_account_id() },
        { "Name": DOMAIN_NAME_DIMENSION, "Value": domain },
    ])
}

/// One `GetMetricData` read of `domain`'s `SearchRate` series over `[start, end)`, folded into
/// `period_secs`-wide windows — exactly the read a client summing the load performs.
async fn read_rate(
    emulator: &Emulator,
    domain: &str,
    (start, end): (f64, f64),
    period_secs: i64,
) -> Answer {
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
                        "Period": period_secs,
                        "Stat": "Sum",
                    },
                }],
                "StartTime": start,
                "EndTime": end,
                "ScanBy": "TimestampAscending",
            }),
        )
        .await
}

/// The TOTAL the sums of that series carry over the whole window — what a client summing the
/// series over the minutes its searches ran reads.
async fn windowed_sum(
    emulator: &Emulator,
    case: &str,
    domain: &str,
    window: (f64, f64),
    period_secs: i64,
) -> f64 {
    let answer = read_rate(emulator, domain, window, period_secs).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?} at a period of {period_secs}s: expected 200 from GetMetricData, got {} \
         with {:?}",
        answer.status, answer.body
    );
    let results = answer.body["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        1,
        "case {case:?} at a period of {period_secs}s: a one-query read answers one result, got \
         {:?}",
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

/// RULE: the searches a cluster executed in a window — whichever route executed them — total
/// into the `Sum`s of that domain's `AWS/ES` `SearchRate` series over the window from the minute
/// they began through two minutes past the minute they ended, at every period a reader folds the
/// series into; a window whose requests had the cluster execute no query carries nothing.
#[tokio::test]
async fn the_searches_a_window_carried_total_into_its_search_rate_sums() {
    let emulator = Emulator::with_metrics();
    let table = rows();

    // Every domain is created first, so the one span the clock owes a provisioning runs for all
    // of them at once rather than once per row.
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
        let window = drive(&emulator, row, host).await;
        for period_secs in PERIODS_SECS {
            let counted = windowed_sum(&emulator, row.case, row.domain, window, period_secs).await;
            assert_eq!(
                counted, row.searches,
                "case {:?}: expected the Sums of {:?}'s {SEARCH_RATE_METRIC} over the window \
                 [{}, {}) at a period of {period_secs}s to total {}, got {counted}",
                row.case, row.domain, window.0, window.1, row.searches
            );
        }
    }
}
