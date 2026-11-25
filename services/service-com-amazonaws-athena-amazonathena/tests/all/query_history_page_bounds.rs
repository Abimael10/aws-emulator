// SPDX-License-Identifier: CC0-1.0

//! How `ListQueryExecutions` answers a request for a PAGE of a workgroup's query history,
//! driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`), so every assertion is about what an SDK client
//! observes.
//!
//! Two rules are pinned here, each over a TABLE of inputs rather than one request.
//!
//! THE PAGE-SIZE RULE: `MaxResults` is measured against the published range of the shape it
//! targets, `MaxQueryExecutionsCount` (range 0..50,
//! `smithy_query(query="operation",operation="ListQueryExecutions")`) — a value inside that
//! range, each bound included, is served, and a value past either bound is refused with the
//! client error the operation models (`InvalidRequestException`, HTTP 400) in AWS's validation
//! wording: one clause naming the member as the wire names it and the bound it crossed, with
//! no echo of the value the request carried. The rows differ along which bound is crossed and
//! by how far, and along where inside the range the page size sits.
//!
//! THE REPORTED-MEMBER RULE: a listing reports the history it found, so an accepted listing
//! always carries `QueryExecutionIds` — a workgroup in which nothing ran, and a page that asks
//! for no ids at all, report an EMPTY list rather than omitting the member, so the presence of
//! the member never depends on the page holding ids. A client reading the member off every
//! answer (every SDK paginator does) therefore sees an empty page where there is no history
//! instead of a response it cannot read.
//!
//! Nothing here waits for a span: the listing reports an id as soon as the submission minted
//! it, so no test of this suite polls at all.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, create_work_group, message, router, start};

/// Where the queries of this suite write their results: a bucket the harness holds, since a
/// submission is accepted only against an output location this account can verify.
const OUTPUT_LOCATION: &str = "s3://athena-results/history-pages/";

/// The member a page size travels under, as a refusal's clause names it: the lowerCamel of
/// `MaxResults`.
const MAX_RESULTS_MEMBER: &str = "maxResults";

/// The published bounds of `MaxQueryExecutionsCount`, the shape `ListQueryExecutions$MaxResults`
/// targets (`smithy_query(query="operation",operation="ListQueryExecutions")`).
const MIN_PAGE_SIZE: i32 = 0;
const MAX_PAGE_SIZE: i32 = 50;

/// A `StartQueryExecution` body submitting `sql` in `work_group`, in its own engine database so
/// one test's statements cannot see another's.
fn submission(sql: &str, work_group: &str, database: &str) -> Value {
    json!({
        "QueryString": sql,
        "WorkGroup": work_group,
        "QueryExecutionContext": { "Database": database },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// The ids a listing reported, and `None` where the answer carried no `QueryExecutionIds`
/// member at all — which is the difference this suite's second rule is about.
fn reported_ids(body: &Value) -> Option<Vec<String>> {
    Some(
        body["QueryExecutionIds"]
            .as_array()?
            .iter()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect(),
    )
}

/// One accepted listing: its status asserted, and the ids it reported handed back.
async fn listing(case: &str, router: &MultiServiceRouter, request: &Value) -> (Value, Vec<String>) {
    let (status, body) = call(router, "ListQueryExecutions", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} with {body:?}"
    );
    let reported = reported_ids(&body);
    assert!(
        reported.is_some(),
        "case {case:?}: an accepted listing must report QueryExecutionIds, got {body:?}"
    );
    (body.clone(), reported.unwrap_or_default())
}

/// Run `count` queries in `work_group` and answer the ids they minted.
async fn run_queries(router: &MultiServiceRouter, work_group: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..count {
        let sql = format!("SELECT {index} AS n");
        ids.push(start(router, &submission(&sql, work_group, "history")).await);
    }
    ids
}

/// Every page size inside the published range is served, and the answer carries the ids member
/// whether the page holds ids or not: the count it reports is as many ids as the history holds,
/// capped by the page size.
#[tokio::test]
async fn a_page_size_inside_the_published_range_is_served_and_reports_the_ids_member() {
    let router = router();
    let busy = "history-busy";
    let silent = "history-silent";
    for work_group in [busy, silent] {
        create_work_group(&router, work_group).await;
    }
    let ran = run_queries(&router, busy, 3).await;

    // (case, the workgroup named, how many ids its history holds). The default workgroup —
    // every account has one and nothing ran in it — is a history that is empty without any
    // workgroup having been created for the row.
    let histories: Vec<(&str, Option<&str>, usize)> = vec![
        ("the default workgroup, in which nothing ran", None, 0),
        (
            "the default workgroup named explicitly",
            Some(DEFAULT_WORK_GROUP),
            0,
        ),
        ("a created workgroup in which nothing ran", Some(silent), 0),
        ("a workgroup holding a history", Some(busy), ran.len()),
    ];
    // Each published bound of the range, and page sizes inside it — one, and one between.
    let page_sizes = [MIN_PAGE_SIZE, 1, 7, MAX_PAGE_SIZE];

    for (history_case, work_group, held) in histories {
        for page_size in page_sizes {
            let case = format!("{history_case} at a page size of {page_size}");
            let mut request = json!({ "MaxResults": page_size });
            if let Some(name) = work_group {
                request["WorkGroup"] = json!(name);
            }
            let (body, ids) = listing(&case, &router, &request).await;
            let expected = held.min(usize::try_from(page_size).unwrap_or(0));
            assert_eq!(
                ids.len(),
                expected,
                "case {case:?}: expected {expected} id(s), got {body:?}"
            );
            for id in &ids {
                assert!(
                    ran.contains(id),
                    "case {case:?}: {id:?} is no id this history minted, got {body:?}"
                );
            }
            if expected == 0 {
                // The rule this suite is about: no ids is an EMPTY list, not an absent member.
                assert_eq!(
                    body["QueryExecutionIds"],
                    json!([]),
                    "case {case:?}: a page holding no ids must report an empty list, got \
                     {body:?}"
                );
                // And a page that reported nothing hands back no token, so a caller following
                // tokens always makes progress.
                assert_eq!(
                    body["NextToken"],
                    Value::Null,
                    "case {case:?}: a page of no ids must hand back no token, got {body:?}"
                );
            }
        }
    }
}

/// A page size past either published bound is refused in AWS's validation wording, naming the
/// member and the bound it crossed and echoing no value — and the refusal lists nothing, so the
/// history a following listing reports is exactly the one it held.
#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused_in_awss_validation_wording() {
    let router = router();
    let work_group = "history-bounds";
    create_work_group(&router, work_group).await;
    let ran = run_queries(&router, work_group, 2).await;

    // (case, the page size asked for, the requirement its clause states)
    let rows: Vec<(String, i32, String)> = vec![
        (
            "one past the largest page the range allows".to_string(),
            MAX_PAGE_SIZE + 1,
            format!("Member must have value less than or equal to {MAX_PAGE_SIZE}"),
        ),
        (
            "the largest number the member carries".to_string(),
            i32::MAX,
            format!("Member must have value less than or equal to {MAX_PAGE_SIZE}"),
        ),
        (
            "one short of the smallest page the range allows".to_string(),
            MIN_PAGE_SIZE - 1,
            format!("Member must have value greater than or equal to {MIN_PAGE_SIZE}"),
        ),
        (
            "the smallest number the member carries".to_string(),
            i32::MIN,
            format!("Member must have value greater than or equal to {MIN_PAGE_SIZE}"),
        ),
    ];

    for (case, page_size, requirement) in rows {
        let expected = format!(
            "1 validation error detected: Value at '{MAX_RESULTS_MEMBER}' failed to satisfy \
             constraint: {requirement}"
        );
        let (status, body) = call(
            &router,
            "ListQueryExecutions",
            &json!({ "WorkGroup": work_group, "MaxResults": page_size }),
        )
        .await;
        assert_refusal(&case, status, &body, ERROR_CODE_INVALID_INPUT, &expected);
        let stated = message(&body);
        assert!(
            !stated.contains(&page_size.to_string()),
            "case {case:?}: the clause must echo no submitted page size, got {stated:?}"
        );

        // A refused listing reported nothing and changed nothing: the history is still the one
        // the submissions above built, reported under the member every answer carries.
        let (_, listed) = listing(
            &format!("{case}, read back"),
            &router,
            &json!({ "WorkGroup": work_group, "MaxResults": MAX_PAGE_SIZE }),
        )
        .await;
        assert_eq!(
            listed.len(),
            ran.len(),
            "case {case:?}: a refused listing must leave the history as it was, got {listed:?}"
        );
    }
}

/// Walking a history a page at a time reports the ids member on every page, and the pages
/// concatenate to the one page a listing of the whole history reports.
#[tokio::test]
async fn every_page_of_a_walk_reports_the_ids_member() {
    let router = router();
    let work_group = "history-walk";
    create_work_group(&router, work_group).await;
    let ran = run_queries(&router, work_group, 5).await;

    let (_, whole) = listing(
        "the whole history on one page",
        &router,
        &json!({ "WorkGroup": work_group, "MaxResults": MAX_PAGE_SIZE }),
    )
    .await;
    assert_eq!(
        whole.len(),
        ran.len(),
        "the one-page listing must report every id the history holds, got {whole:?}"
    );

    for page_size in [1, 2, 3] {
        let case = format!("a walk at a page size of {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One look per page plus a generous margin, so a walk that never ends fails rather
        // than looping.
        for _ in 0..(ran.len() + 4) {
            let mut request = json!({ "WorkGroup": work_group, "MaxResults": page_size });
            if let Some(next) = &token {
                request["NextToken"] = json!(next);
            }
            let (body, ids) = listing(&case, &router, &request).await;
            assert!(
                ids.len() <= usize::try_from(page_size).unwrap_or(0),
                "case {case:?}: a page must hold at most {page_size} id(s), got {body:?}"
            );
            walked.extend(ids);
            token = body["NextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the walk must end, after {} id(s)",
            walked.len()
        );
        assert_eq!(
            walked, whole,
            "case {case:?}: the pages must concatenate to the whole history {whole:?}"
        );
    }
}
