// SPDX-License-Identifier: CC0-1.0

//! What a domain's enabled `LogPublishingOptions` DELIVER into the CloudWatch Logs log
//! groups they name, driven over the real wire path: the domain and its log publishing are
//! created through this service's own `CreateDomain`, the work is done through signed
//! requests to the endpoint `DescribeDomain` published, and the records are read back
//! through the CloudWatch Logs service's OWN `FilterLogEvents` and `DescribeLogStreams` on
//! the same shared engine — so what is asserted is what a client of the log group sees.
//!
//! The rule under test, asserted over a table rather than one call: a domain publishes into
//! the log group an ENABLED `LogPublishingOption` names the records that log type is the log
//! OF — a slow-log record for a read or a write whose own duration reaches the threshold the
//! INDEX's settings state for that log type, and the cluster's application-log line for a
//! change to the indexes it holds — and nothing else; a log type the domain does not publish
//! and a threshold the operation does not reach leave the group untouched.
//!
//! The rows differ along what the rule generalises over: which log type carries the work,
//! which operation does it, which threshold setting and which LEVEL of it is set, the
//! threshold's value and the unit it is written in (including the two forms that disable a
//! level), and whether the work reaches it — and, for a write, WHICH ROUTE performed it,
//! since the cluster applies every write through one path and logs them alike.

use serde_json::{json, Value};

use super::domain_lifecycle::{
    baseline_create, log_group_arn, Emulator, DEFAULT_ACCESS_KEY_ID, PREFIX, SETTLE_WAIT,
};
use super::domain_rest_bulk_and_index_apis::bulk;
use super::domain_rest_data_plane::{endpoint_of, rest};

/// The log group the domain publishes its INDEX_SLOW_LOGS to.
const INDEX_SLOW_GROUP: &str = "/aws/opensearch/domains/index-slow";

/// The log group it publishes its SEARCH_SLOW_LOGS to.
const SEARCH_SLOW_GROUP: &str = "/aws/opensearch/domains/search-slow";

/// The log group it publishes its ES_APPLICATION_LOGS to.
const APPLICATION_GROUP: &str = "/aws/opensearch/domains/application";

/// A group nothing publishes to, so a row can show that a log type the domain does not
/// publish delivers nowhere.
const UNPUBLISHED_GROUP: &str = "/aws/opensearch/domains/unpublished";

/// The index every row reads and writes.
const INDEX: &str = "logged";

/// The indexing slow-log threshold setting, as the engine names it below `index.`.
const INDEX_THRESHOLD: &str = "index.indexing.slowlog.threshold.index";

/// The search slow-log threshold setting for the query phase.
const SEARCH_THRESHOLD: &str = "index.search.slowlog.threshold.query";

/// The messages `FilterLogEvents` reports for the whole of `group`, in the order it pages
/// them.
///
/// `pub(crate)` because a sibling suite reads a published record the same way rather than
/// growing a second copy of this.
pub(crate) async fn events(emulator: &Emulator, group: &str) -> Vec<String> {
    let answer = emulator
        .logs_call("FilterLogEvents", json!({ "logGroupName": group }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case FilterLogEvents({group:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    event["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The stream names `DescribeLogStreams` reports for `group`.
async fn streams(emulator: &Emulator, group: &str) -> Vec<String> {
    let answer = emulator
        .logs_call("DescribeLogStreams", json!({ "logGroupName": group }))
        .await;
    assert_eq!(
        answer.status, 200,
        "case DescribeLogStreams({group:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["logStreams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .map(|stream| {
                    stream["logStreamName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One emulator with the four groups above created through CloudWatch Logs itself and a
/// domain publishing three of the log types, each to its own group, with one index created
/// on it.
async fn with_publishing(domain: &str) -> (Emulator, String) {
    let emulator = Emulator::with_log_groups();
    for group in [
        INDEX_SLOW_GROUP,
        SEARCH_SLOW_GROUP,
        APPLICATION_GROUP,
        UNPUBLISHED_GROUP,
    ] {
        emulator.created_log_group(group).await;
    }
    let mut body = baseline_create(domain);
    // The data-plane work below is signed by this emulator's own credential, so the
    // domain's access policy has to admit it: the baseline body carries a policy with no
    // statement at all, which admits nothing.
    body["AccessPolicies"] = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Principal": { "AWS": "*" }, "Action": "es:ESHttp*", "Resource": "*" }],
    })
    .to_string()
    .into();
    body["LogPublishingOptions"] = json!({
        "INDEX_SLOW_LOGS": {
            "Enabled": true,
            "CloudWatchLogsLogGroupArn": log_group_arn(INDEX_SLOW_GROUP),
        },
        "SEARCH_SLOW_LOGS": {
            "Enabled": true,
            "CloudWatchLogsLogGroupArn": log_group_arn(SEARCH_SLOW_GROUP),
        },
        "ES_APPLICATION_LOGS": {
            "Enabled": true,
            "CloudWatchLogsLogGroupArn": log_group_arn(APPLICATION_GROUP),
        },
        // A DISABLED option publishes nothing, so the group it names stays empty however
        // much work the domain does.
        "AUDIT_LOGS": {
            "Enabled": false,
            "CloudWatchLogsLogGroupArn": log_group_arn(UNPUBLISHED_GROUP),
        },
    });
    let created = emulator.create(body).await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({domain:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    let host = endpoint_of(&emulator, domain);
    (emulator, host)
}

/// Create `INDEX` on the domain at `host`.
async fn created_index(emulator: &Emulator, host: &str) {
    let created = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{INDEX}"),
        Some(json!({ "settings": { "number_of_shards": 2, "number_of_replicas": 1 } })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case PUT /{INDEX}: expected 200, got {} with {:?}",
        created.status, created.body
    );
}

/// Write the settings `body` on `INDEX`, asserting the update was taken.
async fn given_settings(emulator: &Emulator, host: &str, case: &str, body: Value) {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{INDEX}/_settings"),
        Some(body),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the settings update to be taken, got {} with {:?}",
        answer.status, answer.body
    );
}

#[tokio::test]
async fn each_log_type_delivers_its_own_records_to_the_group_its_option_names() {
    let domain = "logs-delivery";
    let (emulator, host) = with_publishing(domain).await;

    // An index the cluster did not hold before is a change to the cluster's own state,
    // which its APPLICATION log states — and is no slow-log record, so the slow-log
    // groups stay empty.
    created_index(&emulator, &host).await;
    let application = events(&emulator, APPLICATION_GROUP).await;
    assert!(
        application
            .iter()
            .any(|message| message.contains("creating index") && message.contains(INDEX)),
        "case \"an index created\": expected the application log to state the create, got \
         {application:?}"
    );
    for group in [INDEX_SLOW_GROUP, SEARCH_SLOW_GROUP, UNPUBLISHED_GROUP] {
        assert!(
            events(&emulator, group).await.is_empty(),
            "case \"an index created\": expected {group} to carry no record, got {:?}",
            events(&emulator, group).await
        );
    }

    // With both thresholds set to a duration every operation reaches, the write goes to
    // the indexing group and the search goes to the search group — each to the group its
    // own log type names and to no other.
    given_settings(
        &emulator,
        &host,
        "thresholds every operation reaches",
        json!({
            format!("{INDEX_THRESHOLD}.warn"): "0ms",
            format!("{SEARCH_THRESHOLD}.warn"): "0ms",
        }),
    )
    .await;

    let written = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "PUT",
        &format!("/{INDEX}/_doc/first"),
        Some(json!({ "title": "a slow document" })),
    )
    .await;
    assert_eq!(
        written.status, 201,
        "case \"a document written\": expected 201, got {} with {:?}",
        written.status, written.body
    );
    let indexing = events(&emulator, INDEX_SLOW_GROUP).await;
    assert!(
        indexing.iter().any(|message| {
            message.contains("index.indexing.slowlog.index")
                && message.contains(INDEX)
                && message.contains("id[first]")
                && message.contains("took_millis[")
        }),
        "case \"a document written\": expected the indexing slow log to state the write, got \
         {indexing:?}"
    );
    assert!(
        events(&emulator, SEARCH_SLOW_GROUP).await.is_empty(),
        "case \"a document written\": expected the search group to carry no record of a write"
    );

    let searched = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        searched.status, 200,
        "case \"a search\": expected 200, got {} with {:?}",
        searched.status, searched.body
    );
    let search = events(&emulator, SEARCH_SLOW_GROUP).await;
    assert!(
        search.iter().any(|message| {
            message.contains("index.search.slowlog.query")
                && message.contains(INDEX)
                && message.contains("took_millis[")
        }),
        "case \"a search\": expected the search slow log to state the read, got {search:?}"
    );
    assert_eq!(
        indexing.len(),
        events(&emulator, INDEX_SLOW_GROUP).await.len(),
        "case \"a search\": expected the indexing group to carry no record of a search"
    );
    // A disabled option delivers nothing, whatever work the domain does.
    assert!(
        events(&emulator, UNPUBLISHED_GROUP).await.is_empty(),
        "case \"a disabled option\": expected its group to stay empty, got {:?}",
        events(&emulator, UNPUBLISHED_GROUP).await
    );

    // A record comes from a NODE of the domain's cluster, so the stream it is written to
    // is one the control plane reports as a node of that domain.
    tokio::time::sleep(SETTLE_WAIT).await;
    let nodes = emulator
        .call(
            "GET",
            &format!("{PREFIX}/opensearch/domain/{domain}/nodes"),
            None,
        )
        .await;
    let reported: Vec<String> = nodes.body["DomainNodesStatusList"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|node| node["NodeId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    for group in [INDEX_SLOW_GROUP, SEARCH_SLOW_GROUP, APPLICATION_GROUP] {
        for stream in streams(&emulator, group).await {
            assert!(
                reported.contains(&stream),
                "case \"the stream a record is written to\": expected {stream:?} in {group} to \
                 be a node of the domain, got {reported:?}"
            );
        }
    }
}

/// One threshold row: the value the level carries, the level it is set on, and whether a
/// write of that index is recorded with it in force.
struct Threshold {
    case: &'static str,
    /// The level the row sets, and the value it sets it to.
    level: &'static str,
    value: Value,
    /// Whether the write that follows produces a record.
    records: bool,
}

/// The thresholds a row may set, so each row turns the levels it does not name OFF rather
/// than inheriting the previous row's.
const LEVELS: [&str; 4] = ["warn", "info", "debug", "trace"];

#[tokio::test]
async fn a_slow_log_record_is_published_only_for_work_that_reaches_the_index_threshold() {
    let domain = "logs-thresholds";
    let (emulator, host) = with_publishing(domain).await;
    created_index(&emulator, &host).await;

    // The values span the engine's time-value units and both forms that turn a level off
    // (its documented `-1` default, and no value at all), plus a duration no in-memory
    // write reaches.
    let rows: &[Threshold] = &[
        Threshold {
            case: "zero milliseconds on warn",
            level: "warn",
            value: json!("0ms"),
            records: true,
        },
        Threshold {
            case: "zero seconds on warn",
            level: "warn",
            value: json!("0s"),
            records: true,
        },
        Threshold {
            case: "zero microseconds on info",
            level: "info",
            value: json!("0micros"),
            records: true,
        },
        Threshold {
            case: "a bare zero, which the engine reads as milliseconds, on trace",
            level: "trace",
            value: json!("0"),
            records: true,
        },
        Threshold {
            case: "the engine's own disabled default",
            level: "warn",
            value: json!("-1"),
            records: false,
        },
        Threshold {
            case: "a negative duration",
            level: "warn",
            value: json!("-1s"),
            records: false,
        },
        Threshold {
            case: "a duration no write reaches",
            level: "warn",
            value: json!("1h"),
            records: false,
        },
        Threshold {
            case: "thirty seconds",
            level: "warn",
            value: json!("30s"),
            records: false,
        },
        Threshold {
            case: "no threshold at all",
            level: "warn",
            value: Value::Null,
            records: false,
        },
    ];

    for (position, row) in rows.iter().enumerate() {
        // Every level the row does not name is reset, so what decides the outcome is the
        // one value under test.
        let mut body = ::serde_json::Map::new();
        for level in LEVELS {
            let value = if level == row.level {
                row.value.clone()
            } else {
                Value::Null
            };
            body.insert(format!("{INDEX_THRESHOLD}.{level}"), value);
        }
        given_settings(&emulator, &host, row.case, Value::Object(body)).await;

        let before = events(&emulator, INDEX_SLOW_GROUP).await.len();
        let written = rest(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            &host,
            "PUT",
            &format!("/{INDEX}/_doc/{position}"),
            Some(json!({ "title": "a document" })),
        )
        .await;
        assert_eq!(
            written.status, 201,
            "case {}: expected the write to be taken, got {} with {:?}",
            row.case, written.status, written.body
        );
        let after = events(&emulator, INDEX_SLOW_GROUP).await;
        let produced = after.len() - before;
        assert_eq!(
            produced,
            usize::from(row.records),
            "case {}: expected {} record(s) from the write, got {:?}",
            row.case,
            usize::from(row.records),
            &after[before..]
        );
        // A record is stated at the level whose threshold the work reached.
        if row.records {
            let stated = after.last().cloned().unwrap_or_default();
            assert!(
                stated.contains(&row.level.to_uppercase()),
                "case {}: expected the record to be stated at {}, got {stated:?}",
                row.case,
                row.level.to_uppercase()
            );
        }
    }
}

/// How ONE write arrives: as a document route of its own, or as an item of a bulk stream.
///
/// The cluster applies every write through one path, so these are the forms a client's
/// write takes and not two sets of rules.
enum Arrival {
    /// One document route: the method, the path below the endpoint, and the body.
    Document {
        method: &'static str,
        path: String,
        body: Value,
    },
    /// A bulk stream: the path it is sent to, and the action/source lines it carries.
    Bulk { path: String, lines: Vec<Value> },
}

/// One write ROUTE as a row of the table: the request that performs the write, and the
/// records the index's own indexing slow log has of it.
struct WriteRoute {
    case: &'static str,
    arrival: Arrival,
    /// The status the request answers, so a row that produces no record is shown to have
    /// been taken rather than refused.
    status: u16,
    /// How many indexing slow-log records the request produces.
    records: usize,
    /// The `id[…]` of each record the request's own ids name — empty where the cluster
    /// minted the id.
    ids: Vec<&'static str>,
}

/// The lines of a bulk stream for `action` on `id`, with the source a write carries.
fn bulk_lines(action: &str, meta: Value, source: Option<Value>) -> Vec<Value> {
    let mut lines = vec![json!({ action: meta })];
    lines.extend(source);
    lines
}

#[tokio::test]
async fn every_route_that_writes_a_document_publishes_the_indexing_slow_log_record_of_it() {
    let domain = "logs-write-routes";
    let (emulator, host) = with_publishing(domain).await;
    created_index(&emulator, &host).await;
    given_settings(
        &emulator,
        &host,
        "thresholds every operation reaches",
        json!({
            format!("{INDEX_THRESHOLD}.warn"): "0ms",
            format!("{SEARCH_THRESHOLD}.warn"): "0ms",
        }),
    )
    .await;

    // The rows span the routes a write arrives on, the actions a bulk stream names, how
    // many documents one request writes, and a request whose write the cluster did not
    // take — which is what says the records follow the WRITE and not the request.
    let rows: Vec<WriteRoute> = vec![
        WriteRoute {
            case: "one document addressed by its path",
            arrival: Arrival::Document {
                method: "PUT",
                path: format!("/{INDEX}/_doc/one"),
                body: json!({ "title": "first write" }),
            },
            status: 201,
            records: 1,
            ids: vec!["one"],
        },
        WriteRoute {
            case: "one document whose id the cluster mints",
            arrival: Arrival::Document {
                method: "POST",
                path: format!("/{INDEX}/_doc"),
                body: json!({ "title": "minted" }),
            },
            status: 201,
            records: 1,
            ids: Vec::new(),
        },
        WriteRoute {
            case: "the partial update of one document",
            arrival: Arrival::Document {
                method: "POST",
                path: format!("/{INDEX}/_update/one"),
                body: json!({ "doc": { "title": "updated" } }),
            },
            status: 200,
            records: 1,
            ids: vec!["one"],
        },
        WriteRoute {
            case: "a bulk index action naming its own index",
            arrival: Arrival::Bulk {
                path: "/_bulk".to_string(),
                lines: bulk_lines(
                    "index",
                    json!({ "_index": INDEX, "_id": "b1" }),
                    Some(json!({ "title": "in bulk" })),
                ),
            },
            status: 200,
            records: 1,
            ids: vec!["b1"],
        },
        WriteRoute {
            case: "a bulk stream of two actions on the path's index",
            arrival: Arrival::Bulk {
                path: format!("/{INDEX}/_bulk"),
                lines: vec![
                    json!({ "create": { "_id": "b2" } }),
                    json!({ "title": "created in bulk" }),
                    json!({ "index": { "_id": "b3" } }),
                    json!({ "title": "indexed in bulk" }),
                ],
            },
            status: 200,
            records: 2,
            ids: vec!["b2", "b3"],
        },
        WriteRoute {
            case: "a bulk update action",
            arrival: Arrival::Bulk {
                path: "/_bulk".to_string(),
                lines: bulk_lines(
                    "update",
                    json!({ "_index": INDEX, "_id": "b1" }),
                    Some(json!({ "doc": { "title": "updated in bulk" } })),
                ),
            },
            status: 200,
            records: 1,
            ids: vec!["b1"],
        },
        WriteRoute {
            case: "a bulk delete action, which writes no document",
            arrival: Arrival::Bulk {
                path: "/_bulk".to_string(),
                lines: bulk_lines("delete", json!({ "_index": INDEX, "_id": "b3" }), None),
            },
            status: 200,
            records: 0,
            ids: Vec::new(),
        },
        WriteRoute {
            case: "a bulk create the cluster refuses, since the id is already held",
            arrival: Arrival::Bulk {
                path: "/_bulk".to_string(),
                lines: bulk_lines(
                    "create",
                    json!({ "_index": INDEX, "_id": "b1" }),
                    Some(json!({ "title": "a second create" })),
                ),
            },
            status: 200,
            records: 0,
            ids: Vec::new(),
        },
    ];

    for row in &rows {
        let before = events(&emulator, INDEX_SLOW_GROUP).await.len();
        let searched = events(&emulator, SEARCH_SLOW_GROUP).await.len();
        let answer = match &row.arrival {
            Arrival::Document { method, path, body } => {
                rest(
                    &emulator,
                    DEFAULT_ACCESS_KEY_ID,
                    &host,
                    method,
                    path,
                    Some(body.clone()),
                )
                .await
            }
            Arrival::Bulk { path, lines } => bulk(&emulator, &host, path, lines).await,
        };
        assert_eq!(
            answer.status, row.status,
            "case {}: expected {}, got {} with {:?}",
            row.case, row.status, answer.status, answer.body
        );
        let after = events(&emulator, INDEX_SLOW_GROUP).await;
        let produced = &after[before.min(after.len())..];
        assert_eq!(
            produced.len(),
            row.records,
            "case {}: expected {} indexing slow-log record(s), got {produced:?}",
            row.case,
            row.records
        );
        for message in produced {
            assert!(
                message.contains("index.indexing.slowlog.index")
                    && message.contains(INDEX)
                    && message.contains("took_millis["),
                "case {}: expected an indexing slow-log record of {INDEX}, got {message:?}",
                row.case
            );
        }
        for id in &row.ids {
            assert!(
                produced
                    .iter()
                    .any(|message| message.contains(&format!("id[{id}]"))),
                "case {}: expected a record naming id[{id}], got {produced:?}",
                row.case
            );
        }
        // A write is no read, so the search slow log has nothing of it whichever route it
        // arrived on.
        assert_eq!(
            events(&emulator, SEARCH_SLOW_GROUP).await.len(),
            searched,
            "case {}: expected the search group to carry no record of a write",
            row.case
        );
    }
}

/// The index a write creates for itself, which no `CreateIndex` call asked for.
const AUTO_CREATED: &str = "auto-made";

#[tokio::test]
async fn an_index_a_write_creates_is_stated_in_the_application_log_with_the_writes_cause() {
    let domain = "logs-auto-created";
    let (emulator, host) = with_publishing(domain).await;

    // An index the cluster was ASKED for states the call as its cause.
    created_index(&emulator, &host).await;
    let application = events(&emulator, APPLICATION_GROUP).await;
    assert!(
        application
            .iter()
            .any(|message| message.contains(INDEX) && message.contains("cause [api]")),
        "case \"an index a call asked for\": expected the application log to state the call as \
         the cause, got {application:?}"
    );

    // An index a WRITE creates is the same change to the cluster's state, stated with the
    // cause the write gives it. It carries no slow-log threshold yet — nothing has set one
    // — so the write that created it is no slow-log record.
    let before = events(&emulator, INDEX_SLOW_GROUP).await.len();
    let loaded = bulk(
        &emulator,
        &host,
        "/_bulk",
        &bulk_lines(
            "index",
            json!({ "_index": AUTO_CREATED, "_id": "first" }),
            Some(json!({ "title": "into an index nobody created" })),
        ),
    )
    .await;
    assert_eq!(
        loaded.status, 200,
        "case \"a write into an index the cluster does not hold\": expected 200, got {} with {:?}",
        loaded.status, loaded.body
    );
    let application = events(&emulator, APPLICATION_GROUP).await;
    assert!(
        application.iter().any(|message| {
            message.contains(AUTO_CREATED) && message.contains("cause [auto(bulk api)]")
        }),
        "case \"an index a write created\": expected the application log to state the create, got \
         {application:?}"
    );
    assert_eq!(
        events(&emulator, INDEX_SLOW_GROUP).await.len(),
        before,
        "case \"an index a write created\": expected no slow-log record for an index carrying no \
         threshold"
    );

    // With a threshold on THAT index, the next write into it is recorded like any other.
    let written = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "PUT",
        &format!("/{AUTO_CREATED}/_settings"),
        Some(json!({ format!("{INDEX_THRESHOLD}.warn"): "0ms" })),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case \"a threshold on the created index\": expected 200, got {} with {:?}",
        written.status, written.body
    );
    let loaded = bulk(
        &emulator,
        &host,
        &format!("/{AUTO_CREATED}/_bulk"),
        &bulk_lines(
            "index",
            json!({ "_id": "second" }),
            Some(json!({ "title": "a logged write" })),
        ),
    )
    .await;
    assert_eq!(
        loaded.status, 200,
        "case \"a second write into the created index\": expected 200, got {} with {:?}",
        loaded.status, loaded.body
    );
    let indexing = events(&emulator, INDEX_SLOW_GROUP).await;
    assert!(
        indexing.iter().any(|message| {
            message.contains(AUTO_CREATED) && message.contains("id[second]")
        }),
        "case \"a second write into the created index\": expected the indexing slow log to state \
         it, got {indexing:?}"
    );
}

/// The events `GetLogEvents` reports for one stream of `group` — the way a client reads a
/// log group: the streams it holds, then the events of one.
async fn stream_events(emulator: &Emulator, group: &str, stream: &str) -> Vec<Value> {
    let answer = emulator
        .logs_call(
            "GetLogEvents",
            json!({ "logGroupName": group, "logStreamName": stream }),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case GetLogEvents({group:?}, {stream:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["events"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn a_published_record_is_read_back_through_get_log_events_on_the_stream_it_went_to() {
    let domain = "logs-read-back";
    let (emulator, host) = with_publishing(domain).await;
    created_index(&emulator, &host).await;
    given_settings(
        &emulator,
        &host,
        "thresholds every operation reaches",
        json!({
            format!("{INDEX_THRESHOLD}.warn"): "0ms",
            format!("{SEARCH_THRESHOLD}.warn"): "0ms",
        }),
    )
    .await;
    let loaded = bulk(
        &emulator,
        &host,
        "/_bulk",
        &bulk_lines(
            "index",
            json!({ "_index": INDEX, "_id": "read-back" }),
            Some(json!({ "title": "a document" })),
        ),
    )
    .await;
    assert_eq!(
        loaded.status, 200,
        "case \"a bulk load\": expected 200, got {} with {:?}",
        loaded.status, loaded.body
    );
    let searched = rest(
        &emulator,
        DEFAULT_ACCESS_KEY_ID,
        &host,
        "POST",
        &format!("/{INDEX}/_search"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        searched.status, 200,
        "case \"a search\": expected 200, got {} with {:?}",
        searched.status, searched.body
    );

    // Each group's records are read the way a client reads them: the streams the group
    // holds, then the events of each — and each event carries the instant the work happened
    // as the epoch milliseconds a log event is timestamped with.
    for (group, logger) in [
        (INDEX_SLOW_GROUP, "index.indexing.slowlog.index"),
        (SEARCH_SLOW_GROUP, "index.search.slowlog.query"),
        (APPLICATION_GROUP, "MetadataCreateIndexService"),
    ] {
        let held = streams(&emulator, group).await;
        assert!(
            !held.is_empty(),
            "case {group:?}: expected the group to hold the stream the records went to"
        );
        let mut messages: Vec<String> = Vec::new();
        for stream in &held {
            for event in stream_events(&emulator, group, stream).await {
                let at = event["timestamp"].as_i64().unwrap_or_default();
                assert!(
                    at > 0,
                    "case {group:?}: expected the event to carry the instant it happened, got \
                     {event:?}"
                );
                messages.push(event["message"].as_str().unwrap_or_default().to_string());
            }
        }
        assert!(
            messages.iter().any(|message| message.contains(logger)),
            "case {group:?}: expected a record of {logger} in the group, got {messages:?}"
        );
    }
}

/// The TWIN pair the differential is about: two indexes of ONE domain, publishing to the
/// same groups, that differ only in the threshold their own settings state.
const OVER_THRESHOLD: &str = "twin-over";

/// See [`OVER_THRESHOLD`].
const UNDER_THRESHOLD: &str = "twin-under";

/// One form of "this index records nothing": the value the quiet twin's levels carry.
struct Quiet {
    case: &'static str,
    /// The value every level of the quiet twin is set to — a threshold no operation reaches,
    /// or no threshold at all.
    threshold: Value,
}

/// Create the index `name` on the domain at `host`.
async fn created(emulator: &Emulator, host: &str, name: &str) {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{name}"),
        Some(json!({ "settings": { "number_of_shards": 1, "number_of_replicas": 1 } })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case PUT /{name}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Write `body` as the settings of the index `name`, as a client configures a threshold.
async fn settings_of(emulator: &Emulator, host: &str, name: &str, body: Value) {
    let answer = rest(
        emulator,
        DEFAULT_ACCESS_KEY_ID,
        host,
        "PUT",
        &format!("/{name}/_settings"),
        Some(body),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case PUT /{name}/_settings: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
}

/// Every level of both slow-log thresholds set to `value` — so what decides the outcome is
/// that one value and not a level the row left standing.
fn thresholds(value: &Value) -> Value {
    let mut body = ::serde_json::Map::new();
    for level in LEVELS {
        for setting in [INDEX_THRESHOLD, SEARCH_THRESHOLD] {
            body.insert(format!("{setting}.{level}"), value.clone());
        }
    }
    Value::Object(body)
}

#[tokio::test]
async fn only_the_twin_whose_own_threshold_the_work_reaches_delivers_a_record() {
    // The two forms that turn a level off: the engine's documented `-1` default, and no
    // threshold at all.
    let rows = [
        Quiet {
            case: "the engine's disabled default",
            threshold: json!("-1"),
        },
        Quiet {
            case: "no threshold at all",
            threshold: Value::Null,
        },
    ];
    for row in &rows {
        let (emulator, host) = with_publishing("logs-twins").await;
        for name in [OVER_THRESHOLD, UNDER_THRESHOLD] {
            created(&emulator, &host, name).await;
        }
        settings_of(&emulator, &host, OVER_THRESHOLD, thresholds(&json!("0ms"))).await;
        settings_of(&emulator, &host, UNDER_THRESHOLD, thresholds(&row.threshold)).await;

        // The SAME work on each twin: one document written, then one query read.
        for name in [OVER_THRESHOLD, UNDER_THRESHOLD] {
            let written = rest(
                &emulator,
                DEFAULT_ACCESS_KEY_ID,
                &host,
                "PUT",
                &format!("/{name}/_doc/one"),
                Some(json!({ "title": "a document" })),
            )
            .await;
            assert_eq!(
                written.status, 201,
                "case {} on {name}: expected the write to be taken, got {} with {:?}",
                row.case, written.status, written.body
            );
            let searched = rest(
                &emulator,
                DEFAULT_ACCESS_KEY_ID,
                &host,
                "POST",
                &format!("/{name}/_search"),
                Some(json!({ "query": { "match_all": {} } })),
            )
            .await;
            assert_eq!(
                searched.status, 200,
                "case {} on {name}: expected the query to be served, got {} with {:?}",
                row.case, searched.status, searched.body
            );
        }

        // Each log type's group carries the twin OVER its threshold and nothing of the twin
        // under it — the one difference between them being that threshold.
        for (group, logger) in [
            (INDEX_SLOW_GROUP, "index.indexing.slowlog.index"),
            (SEARCH_SLOW_GROUP, "index.search.slowlog.query"),
        ] {
            let delivered = events(&emulator, group).await;
            assert!(
                delivered.iter().any(|message| {
                    message.contains(logger) && message.contains(OVER_THRESHOLD)
                }),
                "case {}: expected {group} to carry the {logger} record of {OVER_THRESHOLD}, got \
                 {delivered:?}",
                row.case
            );
            assert!(
                !delivered
                    .iter()
                    .any(|message| message.contains(UNDER_THRESHOLD)),
                "case {}: expected {group} to carry no record of {UNDER_THRESHOLD}, got \
                 {delivered:?}",
                row.case
            );
        }
    }
}
