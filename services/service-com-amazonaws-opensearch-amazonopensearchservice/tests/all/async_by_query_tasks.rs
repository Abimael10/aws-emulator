// SPDX-License-Identifier: CC0-1.0

//! What a BY-QUERY write of a domain's data plane does with the parameters the request sends
//! it — driven over the real wire path: a signed request to the endpoint a domain publishes,
//! through the multi-service router, into the mounted service.
//!
//! One rule, asserted over tables of inputs rather than one recorded call:
//!
//! > a by-query write is performed in the FORM the request asked for. A request that waits
//! > for it (`wait_for_completion` absent or true) is answered with the finished
//! > bulk-by-query report; a request that does not is answered with ONE member, the
//! > `<node>:<id>` handle of the run the cluster started, while the write proceeds in the
//! > background at the rate the request asked for and is observed through
//! > `GET /_tasks/<handle>` — `completed` false while it runs and true once it is done, a
//! > task whose `node` and `id` recompose the polled handle, whose `action` is the engine's
//! > own for the write, and whose `status.deleted` only ever increases, with the report
//! > itself carried as `response` once the run finishes. Either form's `batches` is the
//! > documents it paged over the `scroll_size` it was asked to page in, rounded up, and its
//! > `requests_per_second` is the throttle the request named.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either. Nothing
//! here sleeps for a span or asserts on how long anything took: a case that waits for the
//! clock polls the CONDITION through `poll_until`.

use serde_json::{json, Value};

use super::domain_lifecycle::{poll_until, Answer, Emulator};
use super::domain_rest_data_plane::rest_prefixed;

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "by-query-plane";

/// The engine action a delete-by-query runs under, which a poll of its handle reports.
const DELETE_BY_QUERY_ACTION: &str = "indices:data/write/delete/byquery";

/// The query every delete below selects by: the documents of one class.
fn purge_query() -> Value {
    json!({ "query": { "term": { "kind": "purge" } } })
}

/// The mapping every index below is created with: the keyword a delete selects by, and a
/// number beside it.
fn purge_mapping() -> Value {
    json!({
        "settings": { "index": { "number_of_shards": 1, "number_of_replicas": 0 } },
        "mappings": { "properties": {
            "kind": { "type": "keyword" },
            "n": { "type": "integer" },
        } }
    })
}

/// `CreateDomain` for the domain every case drives.
async fn given_domain(emulator: &Emulator) {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "EngineVersion": "OpenSearch_2.11",
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
}

/// One index holding `purge` documents the query above selects and `keep` it does not.
async fn given_index(emulator: &Emulator, index: &str, purge: usize, keep: usize) {
    let created = rest_prefixed(
        emulator,
        DOMAIN,
        "PUT",
        &format!("/{index}"),
        Some(purge_mapping()),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case PUT /{index}: expected 200, got {} with {:?}",
        created.status, created.body
    );
    for at in 0..(purge + keep) {
        let kind = if at < purge { "purge" } else { "keep" };
        let written = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{index}/_doc/d{at}"),
            Some(json!({ "kind": kind, "n": at })),
        )
        .await;
        assert_eq!(
            written.status, 201,
            "case write /{index}/_doc/d{at} ({kind}): expected 201, got {} with {:?}",
            written.status, written.body
        );
    }
}

/// How many documents `index` holds now.
async fn held(emulator: &Emulator, index: &str) -> u64 {
    let counted = rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{index}/_count"),
        Some(json!({ "query": { "match_all": {} } })),
    )
    .await;
    assert_eq!(
        counted.status, 200,
        "case POST /{index}/_count: expected 200, got {} with {:?}",
        counted.status, counted.body
    );
    counted.body["count"]
        .as_u64()
        .unwrap_or_else(|| panic!("case POST /{index}/_count: expected a count, got {:?}", counted.body))
}

/// One `POST /{index}/_delete_by_query` carrying `query_string`.
async fn delete_by_query(emulator: &Emulator, index: &str, query_string: &str) -> Answer {
    let path = if query_string.is_empty() {
        format!("/{index}/_delete_by_query")
    } else {
        format!("/{index}/_delete_by_query?{query_string}")
    };
    rest_prefixed(emulator, DOMAIN, "POST", &path, Some(purge_query())).await
}

/// One `GET /_tasks/{handle}`.
async fn polled(emulator: &Emulator, handle: &str) -> Answer {
    rest_prefixed(emulator, DOMAIN, "GET", &format!("/_tasks/{handle}"), None).await
}

/// The handle an offloaded write answered with, asserted to be the ONLY member of that answer
/// and to carry the `<node>:<id>` form a client recomposes it from.
fn handle_of(case: &str, answered: &Answer) -> String {
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected 200 from the offloaded delete, got {} with {:?}",
        answered.status, answered.body
    );
    let members: Vec<&String> = answered
        .body
        .as_object()
        .map(|body| body.keys().collect())
        .unwrap_or_default();
    assert_eq!(
        members,
        vec!["task"],
        "case {case:?}: expected the offloaded delete to answer the task handle and nothing \
         else, got {:?}",
        answered.body
    );
    let handle = answered.body["task"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a string task handle, got {:?}",
                answered.body["task"]
            )
        })
        .to_string();
    let (node, number) = handle.split_once(':').unwrap_or_else(|| {
        panic!("case {case:?}: expected a '<node>:<id>' handle, got {handle:?}")
    });
    assert!(
        !node.is_empty() && number.parse::<u64>().is_ok(),
        "case {case:?}: expected a '<node>:<id>' handle whose halves are a node and a \
         number, got {handle:?}"
    );
    handle
}

/// The `response` report a finished run carries, polled for until the run reports itself
/// finished.
async fn finished(emulator: &Emulator, case: &str, handle: &str) -> Value {
    poll_until(case, || async {
        polled(emulator, handle).await.body["completed"].as_bool() == Some(true)
    })
    .await;
    let answered = polled(emulator, handle).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected 200 from the finished task, got {} with {:?}",
        answered.status, answered.body
    );
    answered.body["response"].clone()
}

/// One row of the table the OFFLOADED form is read over: the index the run is submitted
/// against, the parameters it is submitted with, and the figures its report has to come to.
#[derive(Debug, Clone, Copy)]
struct Offloaded {
    case: &'static str,
    /// How many documents of the index the query selects.
    selected: usize,
    /// How many it leaves alone.
    left_alone: usize,
    /// The query string the delete is submitted with.
    query_string: &'static str,
    /// The documents the run reports it processed.
    total: u64,
    /// The ones it reports deleted.
    deleted: u64,
    /// The batches it reports that took.
    batches: u64,
    /// The throttle it reports it ran at.
    throttle: f64,
}

/// RULE: a delete a client OFFLOADS is answered with its handle alone, runs in the
/// background, and reports — once it has finished — the documents it really removed, paged in
/// the batches the request asked for and at the throttle it named.
///
/// The rows differ along what the rule generalises over: how many documents the query selects
/// of how many the index holds, the batch size the scroll is paged in, whether a throttle was
/// asked for, and whether the request capped the delete with `max_docs`.
#[tokio::test]
async fn an_offloaded_delete_answers_a_handle_and_runs_in_the_background() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;

    let rows: &[Offloaded] = &[
        Offloaded {
            case: "a subset of a mixed index, paged two at a time and throttled to one a \
                   second",
            selected: 6,
            left_alone: 4,
            query_string:
                "wait_for_completion=false&refresh=true&scroll_size=2&requests_per_second=1",
            total: 6,
            deleted: 6,
            batches: 3,
            throttle: 1.0,
        },
        Offloaded {
            case: "every document of the index, in one default-sized batch and unthrottled",
            selected: 10,
            left_alone: 0,
            query_string: "wait_for_completion=false",
            total: 10,
            deleted: 10,
            batches: 1,
            throttle: -1.0,
        },
        Offloaded {
            case: "one document, paged one at a time",
            selected: 1,
            left_alone: 3,
            query_string: "wait_for_completion=false&scroll_size=1",
            total: 1,
            deleted: 1,
            batches: 1,
            throttle: -1.0,
        },
        Offloaded {
            case: "a query that selects nothing",
            selected: 0,
            left_alone: 5,
            query_string: "wait_for_completion=false&scroll_size=5",
            total: 0,
            deleted: 0,
            batches: 0,
            throttle: -1.0,
        },
        Offloaded {
            case: "a subset capped by max_docs, paged three at a time",
            selected: 6,
            left_alone: 2,
            query_string: "wait_for_completion=false&scroll_size=3&max_docs=4",
            total: 4,
            deleted: 4,
            batches: 2,
            throttle: -1.0,
        },
        Offloaded {
            case: "a subset throttled with a fractional rate",
            selected: 3,
            left_alone: 1,
            query_string: "wait_for_completion=false&scroll_size=3&requests_per_second=2.5",
            total: 3,
            deleted: 3,
            batches: 1,
            throttle: 2.5,
        },
    ];
    for (at, row) in rows.iter().enumerate() {
        let Offloaded {
            case,
            selected,
            left_alone,
            query_string,
            total,
            deleted,
            batches,
            throttle,
        } = *row;
        let index = format!("purge-{at}");
        given_index(&emulator, &index, selected, left_alone).await;
        let submitted = delete_by_query(&emulator, &index, query_string).await;
        let handle = handle_of(case, &submitted);

        let report = finished(&emulator, case, &handle).await;
        assert_eq!(
            (
                report["total"].as_u64(),
                report["deleted"].as_u64(),
                report["batches"].as_u64(),
                report["requests_per_second"].as_f64(),
                report["version_conflicts"].as_u64(),
                report["failures"].as_array().map(Vec::len),
            ),
            (
                Some(total),
                Some(deleted),
                Some(batches),
                Some(throttle),
                Some(0),
                Some(0)
            ),
            "case {case:?}: expected the run to report {total} processed, {deleted} deleted \
             in {batches} batches at {throttle} a second, got {report:?}"
        );
        // The documents the run reported removed are the documents the index no longer
        // holds: the report is backed by the index's own state.
        let left = held(&emulator, &index).await;
        let expected = u64::try_from(selected + left_alone).unwrap_or_default() - deleted;
        assert_eq!(
            left, expected,
            "case {case:?}: expected {expected} documents left in {index}, got {left}"
        );
    }
}

/// RULE: while an offloaded delete runs, a poll of its handle reports the run — not finished,
/// with the task's own identity and action, and a `status.deleted` that only increases and
/// never claims more documents removed than the index has actually lost.
#[tokio::test]
async fn polling_an_offloaded_delete_reports_a_run_that_only_moves_forward() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;
    let index = "purge-progress";
    // Throttled to one document a second over six, so the run is one a client observes in
    // flight rather than one that is over before it can look.
    given_index(&emulator, index, 6, 4).await;
    let case = "a delete throttled to one document a second";
    let submitted = delete_by_query(
        &emulator,
        index,
        "wait_for_completion=false&scroll_size=2&requests_per_second=1",
    )
    .await;
    let handle = handle_of(case, &submitted);
    let (node, number) = handle
        .split_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a '<node>:<id>' handle"));

    let mut observations = Vec::new();
    let mut complete = false;
    while !complete {
        let answered = polled(&emulator, &handle).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200 from the poll, got {} with {:?}",
            answered.status, answered.body
        );
        let task = answered.body["task"].clone();
        // The task NAMES itself: the two halves of the handle the client polled, and the
        // action the engine registers the write under.
        assert_eq!(
            (
                task["node"].as_str(),
                task["id"].as_u64(),
                task["action"].as_str(),
            ),
            (
                Some(node),
                number.parse::<u64>().ok(),
                Some(DELETE_BY_QUERY_ACTION),
            ),
            "case {case:?}: expected the task to recompose the handle {handle:?} and name the \
             delete-by-query action, got {task:?}"
        );
        let reported = task["status"]["deleted"].as_u64().unwrap_or_else(|| {
            panic!("case {case:?}: expected a status.deleted, got {task:?}")
        });
        complete = answered.body["completed"].as_bool().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a completed flag, got {:?}",
                answered.body
            )
        });
        // What the run says it has removed, the index has really lost: the count read after
        // the poll can only have fallen further, never less far.
        let left = held(&emulator, index).await;
        assert!(
            (4..=10 - reported).contains(&left),
            "case {case:?}: expected the index to hold between 4 and {} documents while the \
             run reported {reported} deleted, got {left}",
            10 - reported
        );
        observations.push((reported, complete));
    }

    let deleted: Vec<u64> = observations.iter().map(|(deleted, _)| *deleted).collect();
    assert!(
        deleted.windows(2).all(|pair| pair[0] <= pair[1]),
        "case {case:?}: expected status.deleted never to fall back, got {deleted:?}"
    );
    assert_eq!(
        observations.first().map(|(_, finished)| *finished),
        Some(false),
        "case {case:?}: expected the first poll to find the run still going, got \
         {observations:?}"
    );
    assert_eq!(
        deleted.last(),
        Some(&6),
        "case {case:?}: expected the finished run to report all six documents deleted, got \
         {deleted:?}"
    );
    let report = polled(&emulator, &handle).await.body["response"].clone();
    assert_eq!(
        (
            report["total"].as_u64(),
            report["deleted"].as_u64(),
            report["batches"].as_u64(),
            report["requests_per_second"].as_f64(),
        ),
        (Some(6), Some(6), Some(3), Some(1.0)),
        "case {case:?}: expected the finished run's own report, got {report:?}"
    );
    assert_eq!(
        held(&emulator, index).await,
        4,
        "case {case:?}: expected the four documents the query did not select to be left"
    );
}

/// RULE: a request that WAITS for the write is answered with the report itself, and that
/// report's `batches` and `requests_per_second` are read from the request's own `scroll_size`
/// and `requests_per_second` exactly as an offloaded run's are.
///
/// The rows differ along the batch size — unstated, smaller than the subset, a divisor of it,
/// larger than it — and along whether a throttle was named.
#[tokio::test]
async fn a_waiting_delete_reports_the_paging_and_throttle_it_was_asked_for() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;

    // (the case, the query string, the batches the report states, the throttle it states)
    let rows: &[(&str, &str, u64, f64)] = &[
        ("no paging and no throttle named", "", 1, -1.0),
        ("a batch smaller than the subset", "scroll_size=2", 3, -1.0),
        ("a batch that divides the subset", "scroll_size=3", 2, -1.0),
        ("a batch larger than the subset", "scroll_size=100", 1, -1.0),
        (
            "a batch and a throttle",
            "scroll_size=2&requests_per_second=4",
            3,
            4.0,
        ),
        (
            "waiting stated explicitly",
            "wait_for_completion=true&scroll_size=4",
            2,
            -1.0,
        ),
    ];
    for (at, row) in rows.iter().enumerate() {
        let (case, query_string, batches, throttle) = *row;
        let index = format!("waited-{at}");
        given_index(&emulator, &index, 6, 4).await;
        let answered = delete_by_query(&emulator, &index, query_string).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            (
                answered.body["total"].as_u64(),
                answered.body["deleted"].as_u64(),
                answered.body["batches"].as_u64(),
                answered.body["requests_per_second"].as_f64(),
                answered.body.get("task").is_some(),
            ),
            (Some(6), Some(6), Some(batches), Some(throttle), false),
            "case {case:?}: expected the finished report with {batches} batches at {throttle} \
             a second and no task handle, got {:?}",
            answered.body
        );
        assert_eq!(
            held(&emulator, &index).await,
            4,
            "case {case:?}: expected the delete to have run in the call itself"
        );
    }

    // ONE reading of the figures serves both forms, so a request capped by `max_docs` reports
    // the same processed total whichever form it asked for — the figure the offloaded row of
    // the same shape reports above.
    let case = "a subset capped by max_docs, waited for";
    let index = "waited-capped";
    given_index(&emulator, index, 6, 2).await;
    let answered = delete_by_query(&emulator, index, "scroll_size=3&max_docs=4").await;
    assert_eq!(
        (
            answered.status,
            answered.body["total"].as_u64(),
            answered.body["deleted"].as_u64(),
            answered.body["batches"].as_u64(),
        ),
        (200, Some(4), Some(4), Some(2)),
        "case {case:?}: expected the cap to be the processed total, in two batches of three, \
         got {} with {:?}",
        answered.status,
        answered.body
    );
    assert_eq!(
        held(&emulator, index).await,
        4,
        "case {case:?}: expected the four documents past the cap to be left"
    );
}

/// RULE: a handle no run of the cluster answers to, and a parameter that names no paging or
/// no rate at all, are refused — and a refused submit starts nothing.
#[tokio::test]
async fn the_task_api_and_the_write_parameters_refuse_what_names_no_run() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;
    let index = "purge-refusals";
    given_index(&emulator, index, 6, 4).await;

    // (the case, the method, the task path, the status, the engine error type)
    let polls: &[(&str, &str, &str, u16, &str)] = &[
        (
            "a task id that is not a handle",
            "GET",
            "/_tasks/not-a-handle",
            400,
            "illegal_argument_exception",
        ),
        (
            "a well-formed handle no run answers to",
            "GET",
            "/_tasks/thisnodeneverran:17",
            404,
            "resource_not_found_exception",
        ),
    ];
    for &(case, method, path, status, error_type) in polls {
        let answered = rest_prefixed(&emulator, DOMAIN, method, path, None).await;
        assert_eq!(
            (answered.status, answered.body["error"]["type"].as_str()),
            (status, Some(error_type)),
            "case {case:?}: expected {status} {error_type}, got {} with {:?}",
            answered.status,
            answered.body
        );
    }
    let wrong_method = rest_prefixed(&emulator, DOMAIN, "DELETE", "/_tasks/anode:1", None).await;
    assert_eq!(
        wrong_method.status, 405,
        "case \"a method the task API does not take\": expected 405, got {} with {:?}",
        wrong_method.status, wrong_method.body
    );

    // (the case, the query string the write is refused for)
    let refused: &[(&str, &str)] = &[
        ("a batch size of zero", "wait_for_completion=false&scroll_size=0"),
        ("a batch size that is not a number", "scroll_size=many"),
        ("a rate of zero", "wait_for_completion=false&requests_per_second=0"),
        ("a negative rate that is not the sentinel", "requests_per_second=-5"),
        ("a rate that is not a number", "requests_per_second=fast"),
        ("a wait flag that is neither word", "wait_for_completion=yes"),
        ("a wait flag spelled as a number", "wait_for_completion=0"),
    ];
    for &(case, query_string) in refused {
        let answered = delete_by_query(&emulator, index, query_string).await;
        assert_eq!(
            (answered.status, answered.body["error"]["type"].as_str()),
            (400, Some("illegal_argument_exception")),
            "case {case:?}: expected the refusal of a parameter naming no value, got {} with \
             {:?}",
            answered.status,
            answered.body
        );
    }
    // Nothing was started and nothing was deleted by any of them.
    assert_eq!(
        held(&emulator, index).await,
        10,
        "case \"the index after the refused writes\": expected every document left"
    );

    // The sentinel IS a rate: it is how a client says it wants no throttle at all.
    let unthrottled = delete_by_query(&emulator, index, "requests_per_second=-1").await;
    assert_eq!(
        (
            unthrottled.status,
            unthrottled.body["requests_per_second"].as_f64()
        ),
        (200, Some(-1.0)),
        "case \"the no-throttle sentinel\": expected 200 reporting -1.0, got {} with {:?}",
        unthrottled.status,
        unthrottled.body
    );
}
