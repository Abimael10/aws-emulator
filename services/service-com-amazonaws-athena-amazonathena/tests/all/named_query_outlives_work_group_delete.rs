// SPDX-License-Identifier: CC0-1.0

//! A saved query OUTLIVES the workgroup it was saved in, driven end to end over the same
//! router the emulator boots (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`), so every
//! assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: `DeleteWorkGroupInput$RecursiveDeleteOption` only PERMITS the delete of
//! a workgroup that still holds contents, and a named query is addressed by the `NamedQueryId`
//! its create minted — `GetNamedQuery` and `BatchGetNamedQuery` take that id and nothing else
//! (`smithy_query(query="operation",operation="GetNamedQuery,BatchGetNamedQuery")`) — so
//! deleting the workgroup leaves every query saved in it resolvable, member for member, still
//! reporting that workgroup, and still writable and deletable by its own id.
//!
//! The rows differ along what the rule generalises over: how many queries the workgroup held,
//! whether it also held content of another kind (a query execution, which the delete does take
//! with the workgroup), whether the create carried a description, and whether a workgroup is
//! created again under the same name afterwards.
//!
//! No expected value is read out of the answer being checked: every id asserted on is one
//! `CreateNamedQuery` answered with, and every other expected value is what the create sent.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_athena_amazonathena::state::{self, ERROR_CODE_INVALID_INPUT};

use crate::harness::{assert_refusal, call, create_work_group, router};

/// Where the started queries below write their results. A query needs a results location, and
/// this suite asserts nothing about the objects themselves.
const OUTPUT_LOCATION: &str = "s3://athena-results/named-query-outlives-work-group/";

/// One row: a workgroup, the queries saved in it, how much other content it holds, and whether
/// the name is created again once the workgroup is gone.
struct Row {
    case: &'static str,
    work_group: &'static str,
    /// `(the query's Name, its Description)` — one entry per query saved in the workgroup.
    queries: &'static [(&'static str, Option<&'static str>)],
    /// How many queries are started in the workgroup, which is content of another kind.
    executions: usize,
    /// Whether a workgroup is created again under the same name after the delete.
    recreated: bool,
}

/// The rows of the rule.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one saved query, nothing else",
            work_group: "wg-outlives-one",
            queries: &[("daily-errors", Some("the day's failed requests"))],
            executions: 0,
            recreated: false,
        },
        Row {
            case: "three saved queries, one without a description",
            work_group: "wg-outlives-three",
            queries: &[
                ("first", Some("the first")),
                ("second", None),
                ("third", Some("the third")),
            ],
            executions: 0,
            recreated: false,
        },
        Row {
            case: "a saved query beside content of another kind",
            work_group: "wg-outlives-mixed",
            queries: &[("beside-a-run", None)],
            executions: 2,
            recreated: false,
        },
        Row {
            case: "a saved query whose workgroup name is created again afterwards",
            work_group: "wg-outlives-recreated",
            queries: &[("recreated", Some("still here"))],
            executions: 0,
            recreated: true,
        },
    ]
}

/// The `CreateNamedQuery` body for one row's query.
fn create_body(work_group: &str, name: &str, description: Option<&str>, position: usize) -> Value {
    let mut request = json!({
        "Name": name,
        "Database": "logs",
        "QueryString": format!("SELECT {position} FROM requests"),
        "WorkGroup": work_group,
    });
    if let Some(description) = description {
        request["Description"] = json!(description);
    }
    request
}

/// Save the query `request` describes and hand back the `NamedQueryId` Athena minted.
async fn save(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, saved) = call(router, "CreateNamedQuery", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateNamedQuery({request}) to answer 200, got {status} with \
         {saved:?}"
    );
    saved["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted id, got {saved:?}"))
        .to_string()
}

/// Start one query in `work_group` and hand back the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, work_group: &str) -> String {
    let (status, started) = call(
        router,
        "StartQueryExecution",
        &json!({
            "QueryString": "SELECT 1",
            "WorkGroup": work_group,
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a query to start in {work_group}, got {status} with {started:?}"
    );
    started["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted id, got {started:?}"))
        .to_string()
}

/// The `NamedQuery` `id` resolves to, asserting the read answered 200.
async fn read(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, body) = call(router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetNamedQuery({id}) to answer 200, got {status} with {body:?}"
    );
    body["NamedQuery"].clone()
}

/// Assert that `found` is the query `request` saved, in `work_group`.
fn assert_saved(case: &str, id: &str, work_group: &str, request: &Value, found: &Value) {
    for member in ["Name", "Database", "QueryString", "Description"] {
        assert_eq!(
            found[member], request[member],
            "case {case:?}: expected the saved query's {member} to be {:?}, got {:?}",
            request[member], found[member]
        );
    }
    assert_eq!(
        found["NamedQueryId"].as_str(),
        Some(id),
        "case {case:?}: expected the record to report the id it was minted under, got {:?}",
        found["NamedQueryId"]
    );
    assert_eq!(
        found["WorkGroup"].as_str(),
        Some(work_group),
        "case {case:?}: expected the query to report the workgroup it was saved in, got {:?}",
        found["WorkGroup"]
    );
}

/// Every query a workgroup held still reads back, member for member, once the workgroup is
/// deleted with the recursive option — and the content kinds the workgroup itself addressed
/// are gone.
#[tokio::test]
async fn a_saved_query_resolves_after_its_work_group_is_deleted() {
    let router = router();

    for row in rows() {
        create_work_group(&router, row.work_group).await;

        let mut saved: Vec<(Value, String)> = Vec::new();
        for (position, (name, description)) in row.queries.iter().enumerate() {
            let request = create_body(row.work_group, name, *description, position);
            let id = save(&router, row.case, &request).await;
            saved.push((request, id));
        }
        let mut executions: Vec<String> = Vec::new();
        for _ in 0..row.executions {
            executions.push(start(&router, row.case, row.work_group).await);
        }

        // A workgroup holding saved queries is not empty, so a delete that did not ask for its
        // contents is refused and takes nothing.
        let (status, refused) = call(
            &router,
            "DeleteWorkGroup",
            &json!({ "WorkGroup": row.work_group }),
        )
        .await;
        assert_refusal(
            row.case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            &state::work_group_not_empty(row.work_group),
        );

        // The option PERMITS the delete.
        let (status, deleted) = call(
            &router,
            "DeleteWorkGroup",
            &json!({ "WorkGroup": row.work_group, "RecursiveDeleteOption": true }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the recursive delete to answer 200, got {status} with \
             {deleted:?}",
            row.case
        );

        if row.recreated {
            create_work_group(&router, row.work_group).await;
        }

        // Every saved query still resolves by its own id, reporting the workgroup it was
        // saved in and the values its create carried.
        for (request, id) in &saved {
            let found = read(&router, row.case, id).await;
            assert_saved(row.case, id, row.work_group, request, &found);
        }

        // A batch read resolves them all at once, and reports only the id this account holds
        // no query under as unprocessed.
        let unminted = "00000000-0000-4000-8000-000000000000";
        let mut ids: Vec<String> = saved.iter().map(|(_, id)| id.clone()).collect();
        ids.push(unminted.to_string());
        let (status, batch) = call(
            &router,
            "BatchGetNamedQuery",
            &json!({ "NamedQueryIds": ids }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected BatchGetNamedQuery to answer 200, got {status} with {batch:?}",
            row.case
        );
        let resolved: Vec<String> = batch["NamedQueries"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|query| query["NamedQueryId"].as_str().map(str::to_string))
            .collect();
        let mut wanted: Vec<String> = saved.iter().map(|(_, id)| id.clone()).collect();
        wanted.sort();
        let mut answered = resolved.clone();
        answered.sort();
        assert_eq!(
            answered, wanted,
            "case {:?}: expected the batch read to resolve every surviving query, got \
             {resolved:?}",
            row.case
        );
        let unprocessed: Vec<String> = batch["UnprocessedNamedQueryIds"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|entry| entry["NamedQueryId"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            unprocessed,
            vec![unminted.to_string()],
            "case {:?}: expected only the unminted id to be unprocessed, got {unprocessed:?}",
            row.case
        );

        // The query executions the workgroup addressed went with it: the workgroup was their
        // only address.
        for execution in &executions {
            let (status, body) = call(
                &router,
                "GetQueryExecution",
                &json!({ "QueryExecutionId": execution }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {:?}: expected the query execution {execution} to go with its workgroup, \
                 got {status} with {body:?}",
                row.case
            );
        }
    }
}

/// A query whose workgroup is gone is still a query: its id takes an update and a delete, and
/// the delete is what finally makes it resolve to nothing.
#[tokio::test]
async fn a_surviving_query_is_still_written_and_deleted_by_its_id() {
    let router = router();
    let work_group = "wg-outlives-writable";
    create_work_group(&router, work_group).await;

    let request = create_body(work_group, "to-be-revised", Some("as created"), 7);
    let id = save(&router, "the surviving query", &request).await;

    let (status, deleted) = call(
        &router,
        "DeleteWorkGroup",
        &json!({ "WorkGroup": work_group, "RecursiveDeleteOption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the recursive delete to answer 200, got {status} with {deleted:?}"
    );

    // The rows differ along the write the id is asked to take.
    let revised = json!({
        "NamedQueryId": id,
        "Name": "revised",
        "QueryString": "SELECT 8 FROM requests",
        "Description": "as revised",
    });
    let (status, updated) = call(&router, "UpdateNamedQuery", &revised).await;
    assert_eq!(
        status, 200,
        "expected UpdateNamedQuery to answer 200 for a query whose workgroup is gone, got \
         {status} with {updated:?}"
    );
    let found = read(&router, "the revised query", &id).await;
    assert_saved(
        "the revised query",
        &id,
        work_group,
        &json!({
            "Name": "revised",
            "Database": request["Database"],
            "QueryString": "SELECT 8 FROM requests",
            "Description": "as revised",
        }),
        &found,
    );

    let (status, removed) = call(&router, "DeleteNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_eq!(
        status, 200,
        "expected DeleteNamedQuery to answer 200, got {status} with {removed:?}"
    );
    let (status, gone) = call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_refusal(
        "the query its own delete removed",
        status,
        &gone,
        ERROR_CODE_INVALID_INPUT,
        &state::named_query_not_found(&id),
    );
}
