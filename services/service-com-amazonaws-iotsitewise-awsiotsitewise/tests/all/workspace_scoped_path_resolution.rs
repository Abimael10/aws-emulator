// SPDX-License-Identifier: CC0-1.0

//! Every operation whose route nests a resource inside `/workspaces/{name}`
//! resolves the path's resources OUTSIDE IN, so the refusal names the OUTERMOST
//! resource the request got wrong: the workspace when the store holds no
//! workspace of that name, and only then the pipeline, task, query, search,
//! export job or pipeline execution inside it.
//!
//! The rows span that whole class of routes — the reads, the writes and the
//! deletes, at every nesting depth the service has (one, two and three path
//! resources) — rather than the one request an issue happened to record, and each
//! row carries its own identifier so the expected message is built from what the
//! row sent.
//!
//! A consequence the last three suites below pin for the asynchronous query and
//! search slice, where several operations address the SAME resource: all of them
//! answer the same absence for one request, so no two of them can disagree about
//! which resource a request is wrong about.
//!
//! The expected strings are written out BY HAND as an independent oracle: this
//! suite never calls the crate's message helpers, so a handler that drifts to a
//! template of its own cannot agree with itself.
//!
//! Everything is driven over HTTP through the same `MultiServiceRouter` the
//! emulator boots with, so every assertion is about the wire behaviour a client
//! sees.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::asset_lifecycle::{call, error_code, router};
use super::hierarchies_etags_and_refusals::error_message;
use super::pipeline_executions::{
    create_pipeline, create_task, create_workspace, start_plain, Node,
};

/// The workspace the fixture opens, so a row can be wrong only INSIDE it.
const WORKSPACE: &str = "resolution-workspace";

/// The pipeline the fixture creates in it, and the task its compute node runs.
const PIPELINE: &str = "nightly-rollup";
const TASK: &str = "alpha";

/// Workspace names no workspace answers to, each inside the modelled
/// `WorkspaceName` (len 1..64, pattern `^[a-zA-Z0-9_-]+$`): a hyphenated name, a
/// mixed-case name with an underscore and digits, and one at the published
/// maximum length.
const UNKNOWN_WORKSPACES: [&str; 3] = [
    "no-such-workspace",
    "Absent_Workspace_42",
    "wwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwwww",
];

/// An `ID` (len 36..36, lower-case hex groups) no pipeline execution was ever
/// started under, and which is not the identifier the pattern excludes.
const UNKNOWN_EXECUTION: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// A `DatasetExportJobId` (the same 36-character form) no export job holds.
const UNKNOWN_JOB: &str = "11111111-2222-3333-4444-555555555555";

/// A `QueryId` (len 1..64, pattern `^[a-zA-Z0-9-]+$`) no query holds.
const UNKNOWN_QUERY: &str = "no-such-query";

/// A `SearchId` (len 23..36, pattern `^[a-zA-Z0-9]+(-[a-zA-Z0-9]+)*$`) no search
/// holds.
const UNKNOWN_SEARCH: &str = "no-such-search-0123456789";

/// Resource names nothing in the fixture was created under.
const UNKNOWN_PIPELINE: &str = "no-such-pipeline";
const UNKNOWN_TASK: &str = "no-such-task";

/// The resource KINDS this service words its absences with, as the wire reports
/// them.
const WORKSPACE_KIND: &str = "Workspace";
const PIPELINE_KIND: &str = "Pipeline";
const PIPELINE_EXECUTION_KIND: &str = "Pipeline Execution";
const TASK_KIND: &str = "Task";
const QUERY_KIND: &str = "Query";
const SEARCH_KIND: &str = "Search";
const EXPORT_JOB_KIND: &str = "Dataset Export Job";

/// One row: the case name, the HTTP method, the URI, the request body, and the
/// absence the refusal must state.
type Row = (&'static str, &'static str, String, Option<Value>, String);

/// How this service states an absence, written out by hand.
fn absent(kind: &str, identifier: &str) -> String {
    format!("{kind} with ID {identifier} not found")
}

/// A workspace holding one task and one pipeline, with one execution of that
/// pipeline started, so every row below is wrong in exactly the place it says.
async fn fixture(router: &MultiServiceRouter) -> String {
    create_workspace(router, WORKSPACE).await;
    create_task(router, WORKSPACE, TASK, &[], &[]).await;
    let (status, body) = create_pipeline(
        router,
        WORKSPACE,
        PIPELINE,
        &[],
        &[Node::new("only", TASK)],
    )
    .await;
    assert_eq!(status, 200, "create pipeline: got {status} {body}");
    start_plain(router, WORKSPACE, PIPELINE).await
}

/// Drive `rows` and assert each is the modelled 404 stating the absence the row
/// expects.
async fn assert_rows(router: &MultiServiceRouter, rows: Vec<Row>) {
    for (case, method, uri, body, expected) in rows {
        let (status, answered) = call(router, method, &uri, body).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            expected,
            "case {case:?}: expected {expected:?}, got {answered}"
        );
    }
}

#[tokio::test]
async fn a_workspace_this_service_does_not_hold_is_the_absence_every_route_under_it_reports() {
    let router = router();
    let execution = fixture(&router).await;

    // Each row names a workspace the store holds no record of. The rest of the
    // path is sometimes right and sometimes wrong, which changes nothing: the
    // workspace is the outermost resource the request got wrong, so it is the one
    // the refusal names. The three unknown workspace names rotate through the rows
    // so the expected message is built from the identifier the row sent.
    let [first, second, third] = UNKNOWN_WORKSPACES;
    let rows: Vec<Row> = vec![
        (
            "DescribePipelineExecution, the execution's own pipeline and id",
            "GET",
            format!("/workspaces/{first}/pipelines/{PIPELINE}/executions/{execution}"),
            None,
            absent(WORKSPACE_KIND, first),
        ),
        (
            "DescribePipelineExecution, wrong in all three resources",
            "GET",
            format!(
                "/workspaces/{second}/pipelines/{UNKNOWN_PIPELINE}/executions/{UNKNOWN_EXECUTION}"
            ),
            None,
            absent(WORKSPACE_KIND, second),
        ),
        (
            "DescribePipelineExecution, a workspace name at the modelled maximum length",
            "GET",
            format!("/workspaces/{third}/pipelines/{PIPELINE}/executions/{execution}"),
            None,
            absent(WORKSPACE_KIND, third),
        ),
        (
            "CancelPipelineExecution, the same three values",
            "POST",
            format!("/workspaces/{first}/pipelines/{PIPELINE}/executions/{execution}/cancel"),
            Some(json!({})),
            absent(WORKSPACE_KIND, first),
        ),
        (
            "ListPipelineExecutions",
            "GET",
            format!("/workspaces/{second}/pipelines/{PIPELINE}/executions"),
            None,
            absent(WORKSPACE_KIND, second),
        ),
        (
            "StartPipelineExecution",
            "POST",
            format!("/workspaces/{third}/pipelines/{PIPELINE}/executions"),
            Some(json!({})),
            absent(WORKSPACE_KIND, third),
        ),
        (
            "DescribePipeline",
            "GET",
            format!("/workspaces/{first}/pipelines/{PIPELINE}"),
            None,
            absent(WORKSPACE_KIND, first),
        ),
        (
            "DescribePipeline, an earlier version of it",
            "GET",
            format!("/workspaces/{second}/pipelines/{PIPELINE}?version=1"),
            None,
            absent(WORKSPACE_KIND, second),
        ),
        (
            "UpdatePipeline",
            "PUT",
            format!("/workspaces/{third}/pipelines/{PIPELINE}"),
            Some(json!({ "description": "whatever" })),
            absent(WORKSPACE_KIND, third),
        ),
        (
            "DeletePipeline",
            "DELETE",
            format!("/workspaces/{first}/pipelines/{PIPELINE}"),
            None,
            absent(WORKSPACE_KIND, first),
        ),
        (
            "ListPipelines",
            "GET",
            format!("/workspaces/{second}/pipelines"),
            None,
            absent(WORKSPACE_KIND, second),
        ),
        (
            "DescribeTask",
            "GET",
            format!("/workspaces/{third}/tasks/{TASK}"),
            None,
            absent(WORKSPACE_KIND, third),
        ),
        (
            "UpdateTask",
            "PUT",
            format!("/workspaces/{first}/tasks/{TASK}"),
            Some(json!({ "description": "whatever" })),
            absent(WORKSPACE_KIND, first),
        ),
        (
            "ListTasks",
            "GET",
            format!("/workspaces/{second}/tasks"),
            None,
            absent(WORKSPACE_KIND, second),
        ),
        (
            "DescribeQuery",
            "GET",
            format!("/workspaces/{third}/queries/{UNKNOWN_QUERY}"),
            None,
            absent(WORKSPACE_KIND, third),
        ),
        (
            "DescribeDatasetExportJob",
            "GET",
            format!("/workspaces/{first}/dataset-export-jobs/{UNKNOWN_JOB}"),
            None,
            absent(WORKSPACE_KIND, first),
        ),
    ];
    assert_rows(&router, rows).await;
}

#[tokio::test]
async fn a_workspace_that_resolves_refuses_the_resource_inside_it_the_path_names() {
    let router = router();
    let execution = fixture(&router).await;

    // Each row names the workspace the fixture opened, so the outermost resource
    // the request gets wrong is the one INSIDE it: the pipeline, the task, the
    // query, the export job, or — with the workspace and the pipeline both right —
    // the execution itself.
    let rows: Vec<Row> = vec![
        (
            "DescribePipelineExecution, a pipeline nothing created",
            "GET",
            format!(
                "/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}/executions/{execution}"
            ),
            None,
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "DescribePipelineExecution, an execution id nothing started",
            "GET",
            format!(
                "/workspaces/{WORKSPACE}/pipelines/{PIPELINE}/executions/{UNKNOWN_EXECUTION}"
            ),
            None,
            absent(PIPELINE_EXECUTION_KIND, UNKNOWN_EXECUTION),
        ),
        (
            "CancelPipelineExecution, a pipeline nothing created",
            "POST",
            format!(
                "/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}/executions/{execution}/cancel"
            ),
            Some(json!({})),
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "CancelPipelineExecution, an execution id nothing started",
            "POST",
            format!(
                "/workspaces/{WORKSPACE}/pipelines/{PIPELINE}/executions/{UNKNOWN_EXECUTION}/cancel"
            ),
            Some(json!({})),
            absent(PIPELINE_EXECUTION_KIND, UNKNOWN_EXECUTION),
        ),
        (
            "ListPipelineExecutions, a pipeline nothing created",
            "GET",
            format!("/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}/executions"),
            None,
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "StartPipelineExecution, a pipeline nothing created",
            "POST",
            format!("/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}/executions"),
            Some(json!({})),
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "DescribePipeline, a pipeline nothing created",
            "GET",
            format!("/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}"),
            None,
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "UpdatePipeline, a pipeline nothing created",
            "PUT",
            format!("/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}"),
            Some(json!({ "description": "whatever" })),
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "DeletePipeline, a pipeline nothing created",
            "DELETE",
            format!("/workspaces/{WORKSPACE}/pipelines/{UNKNOWN_PIPELINE}"),
            None,
            absent(PIPELINE_KIND, UNKNOWN_PIPELINE),
        ),
        (
            "DescribeTask, a task nothing created",
            "GET",
            format!("/workspaces/{WORKSPACE}/tasks/{UNKNOWN_TASK}"),
            None,
            absent(TASK_KIND, UNKNOWN_TASK),
        ),
        (
            "UpdateTask, a task nothing created",
            "PUT",
            format!("/workspaces/{WORKSPACE}/tasks/{UNKNOWN_TASK}"),
            Some(json!({ "description": "whatever" })),
            absent(TASK_KIND, UNKNOWN_TASK),
        ),
        (
            "DescribeQuery, a query nothing started",
            "GET",
            format!("/workspaces/{WORKSPACE}/queries/{UNKNOWN_QUERY}"),
            None,
            absent(QUERY_KIND, UNKNOWN_QUERY),
        ),
        (
            "DescribeDatasetExportJob, a job nothing created",
            "GET",
            format!("/workspaces/{WORKSPACE}/dataset-export-jobs/{UNKNOWN_JOB}"),
            None,
            absent(EXPORT_JOB_KIND, UNKNOWN_JOB),
        ),
    ];
    assert_rows(&router, rows).await;
}

#[tokio::test]
async fn resolving_outside_in_still_serves_every_path_the_store_does_hold() {
    let router = router();
    let execution = fixture(&router).await;

    // The added resolution refuses nothing that resolves: each row is the same
    // route as a refusal above, with every resource on it present.
    let served: [(&str, String); 5] = [
        (
            "DescribePipelineExecution",
            format!("/workspaces/{WORKSPACE}/pipelines/{PIPELINE}/executions/{execution}"),
        ),
        (
            "DescribePipeline",
            format!("/workspaces/{WORKSPACE}/pipelines/{PIPELINE}"),
        ),
        (
            "ListPipelineExecutions",
            format!("/workspaces/{WORKSPACE}/pipelines/{PIPELINE}/executions"),
        ),
        (
            "DescribeTask",
            format!("/workspaces/{WORKSPACE}/tasks/{TASK}"),
        ),
        ("ListPipelines", format!("/workspaces/{WORKSPACE}/pipelines")),
    ];
    for (case, uri) in served {
        let (status, body) = call(&router, "GET", &uri, None).await;
        assert_eq!(status, 200, "case {case:?}: got {status} {body}");
    }

    // And the execution the fixture started is still the one its own path reports.
    let (status, body) = call(
        &router,
        "GET",
        &format!("/workspaces/{WORKSPACE}/pipelines/{PIPELINE}/executions/{execution}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "the execution's own path: got {status} {body}");
    assert_eq!(
        body["pipelineExecutionId"].as_str(),
        Some(execution.as_str()),
        "the execution's own path reports it, expected {execution}, got {body}"
    );
    assert_eq!(
        body["workspaceName"].as_str(),
        Some(WORKSPACE),
        "the execution's own path reports its workspace, expected {WORKSPACE}, got {body}"
    );
}

/// A second workspace the store DOES hold, so a query or search of the first one
/// can be addressed through a workspace that resolves but holds it not.
const OTHER_WORKSPACE: &str = "resolution_workspace_2";

/// A statement `StartQuery` takes: the SQL this service's query language is, over
/// a view its catalog names.
const QUERY_STATEMENT: &str = "SELECT asset_name FROM asset";

/// A statement `StartSearch` takes (`SearchQueryStatement`, len 1..5000 — prose
/// rather than SQL).
const SEARCH_STATEMENT: &str = "turbine vibration last week";

/// One route of the asynchronous query/search family, and the resource it
/// addresses INSIDE the workspace when it addresses one.
struct Route {
    label: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// The kind and the identifier of the resource the path names inside the
    /// workspace; `None` for a route that names only the workspace.
    inner: Option<(&'static str, String)>,
    /// Every status this route may answer with once every resource on it
    /// resolves. One value for most of them — the success status `OPERATIONS.md`
    /// reports — and two for the routes whose answer then depends on the status
    /// the CLOCK has made due for the resource, which no suite may pin to one
    /// value: `CancelQuery` refuses a query that already reached a terminal
    /// status with the modelled conflict (409), and a search's results are
    /// "available only after the search has reached the SUCCEEDED status", the
    /// modelled invalid-request refusal (400) before then.
    served: &'static [u16],
}

/// Every operation of a workspace's asynchronous query and search slice, as
/// `OPERATIONS.md` reports its route: the read, the results read, the cancel, the
/// listing and the start of a query, and the read, the results read, the listing
/// and the start of a search. `query` and `search` are the identifiers each row
/// addresses inside `workspace`.
///
/// Several of these rows address the SAME resource, which is the point: a request
/// is wrong about ONE resource, so every operation that takes it must say the same
/// thing about which one.
fn query_and_search_family(workspace: &str, query: &str, search: &str) -> Vec<Route> {
    vec![
        Route {
            label: "DescribeQuery",
            method: "GET",
            uri: format!("/workspaces/{workspace}/queries/{query}"),
            body: None,
            inner: Some((QUERY_KIND, query.to_string())),
            served: &[200],
        },
        Route {
            label: "GetQueryResults",
            method: "GET",
            uri: format!("/workspaces/{workspace}/queries/{query}/results"),
            body: None,
            inner: Some((QUERY_KIND, query.to_string())),
            served: &[200],
        },
        Route {
            label: "CancelQuery",
            method: "POST",
            uri: format!("/workspaces/{workspace}/queries/{query}/cancel"),
            body: Some(json!({})),
            inner: Some((QUERY_KIND, query.to_string())),
            served: &[202, 409],
        },
        Route {
            label: "ListQueries",
            method: "GET",
            uri: format!("/workspaces/{workspace}/queries"),
            body: None,
            inner: None,
            served: &[200],
        },
        Route {
            label: "StartQuery",
            method: "POST",
            uri: format!("/workspaces/{workspace}/queries"),
            body: Some(json!({ "queryStatement": QUERY_STATEMENT })),
            inner: None,
            served: &[202],
        },
        Route {
            label: "DescribeSearch",
            method: "GET",
            uri: format!("/workspaces/{workspace}/searches/{search}"),
            body: None,
            inner: Some((SEARCH_KIND, search.to_string())),
            served: &[200],
        },
        Route {
            label: "GetSearchResults",
            method: "GET",
            uri: format!("/workspaces/{workspace}/searches/{search}/results"),
            body: None,
            inner: Some((SEARCH_KIND, search.to_string())),
            served: &[200, 400],
        },
        Route {
            label: "ListSearches",
            method: "POST",
            uri: format!("/workspaces/{workspace}/searches/list"),
            body: Some(json!({})),
            inner: None,
            served: &[200],
        },
        Route {
            label: "StartSearch",
            method: "POST",
            uri: format!("/workspaces/{workspace}/searches"),
            body: Some(json!({ "queryStatement": SEARCH_STATEMENT })),
            inner: None,
            served: &[202],
        },
    ]
}

/// Two workspaces this store holds, the first carrying one started query and one
/// started search, and the identifiers they were minted under — so every row
/// below is wrong in exactly the place it says.
async fn async_resource_fixture(router: &MultiServiceRouter) -> (String, String) {
    create_workspace(router, WORKSPACE).await;
    create_workspace(router, OTHER_WORKSPACE).await;

    let (status, started) = call(
        router,
        "POST",
        &format!("/workspaces/{WORKSPACE}/queries"),
        Some(json!({ "queryStatement": QUERY_STATEMENT })),
    )
    .await;
    assert_eq!(status, 202, "StartQuery: got {status} {started}");
    let query = started["queryId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartQuery: no queryId in {started}"))
        .to_string();

    let (status, searched) = call(
        router,
        "POST",
        &format!("/workspaces/{WORKSPACE}/searches"),
        Some(json!({ "queryStatement": SEARCH_STATEMENT })),
    )
    .await;
    assert_eq!(status, 202, "StartSearch: got {status} {searched}");
    let search = searched["searchId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartSearch: no searchId in {searched}"))
        .to_string();

    (query, search)
}

#[tokio::test]
async fn every_reader_of_one_query_or_search_names_the_unheld_workspace_it_was_addressed_through() {
    let router = router();
    let (query, search) = async_resource_fixture(&router).await;

    // Each row names a workspace this store holds no record of, and the identifier
    // of a query or a search the store really does hold in the workspace it was
    // started in. The workspace is the outermost resource the request got wrong, so
    // every one of these operations — the two reads of the query, its cancel, the
    // listings and the starts, and the same for the search — reports the WORKSPACE,
    // and none of them reports the resource inside it.
    for workspace in UNKNOWN_WORKSPACES {
        let expected = absent(WORKSPACE_KIND, workspace);
        let rows: Vec<Row> = query_and_search_family(workspace, &query, &search)
            .into_iter()
            .map(|route| {
                (
                    route.label,
                    route.method,
                    route.uri,
                    route.body,
                    expected.clone(),
                )
            })
            .collect();
        assert_rows(&router, rows).await;
    }

    // A refused request changed nothing it named: the query was not cancelled, and
    // the two starts among the rows above started nothing — the workspace that
    // holds them still holds exactly the one query and the one search the fixture
    // started.
    let (status, described) = call(
        &router,
        "GET",
        &format!("/workspaces/{WORKSPACE}/queries/{query}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeQuery after the refusals: got {status} {described}"
    );
    // `status` is a REQUIRED member of `DescribeQueryOutput`, so a body without it
    // fails here rather than satisfying the inequality below by default.
    let reported = described["status"]
        .as_str()
        .unwrap_or_else(|| panic!("DescribeQuery: no status in {described}"));
    assert!(
        reported != "CANCELED" && reported != "CANCELING",
        "a refused CancelQuery cancels nothing: the query reports {reported:?} in {described}"
    );

    let (status, listed) = call(
        &router,
        "GET",
        &format!("/workspaces/{WORKSPACE}/queries"),
        None,
    )
    .await;
    assert_eq!(status, 200, "ListQueries: got {status} {listed}");
    assert_eq!(
        listed["queries"].as_array().map(Vec::len),
        Some(1),
        "the refused StartQuery rows started no query, expected the fixture's one, got {listed}"
    );

    let (status, listed) = call(
        &router,
        "POST",
        &format!("/workspaces/{WORKSPACE}/searches/list"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, 200, "ListSearches: got {status} {listed}");
    assert_eq!(
        listed["searchSummaries"].as_array().map(Vec::len),
        Some(1),
        "the refused StartSearch rows started no search, expected the fixture's one, got {listed}"
    );
}

#[tokio::test]
async fn a_workspace_that_resolves_leaves_the_query_or_search_inside_it_to_report_its_absence() {
    let router = router();
    let (query, search) = async_resource_fixture(&router).await;

    // With the workspace held, the outermost resource the request gets wrong is the
    // one INSIDE it, in both ways a pair can address no resource: an identifier
    // nothing was ever started under, and the identifier of a resource ANOTHER
    // workspace holds.
    let inside: [(&str, &str, &str, &str); 2] = [
        (
            "an identifier nothing was started under",
            WORKSPACE,
            UNKNOWN_QUERY,
            UNKNOWN_SEARCH,
        ),
        (
            "a resource of another workspace",
            OTHER_WORKSPACE,
            query.as_str(),
            search.as_str(),
        ),
    ];
    for (case, workspace, query_id, search_id) in inside {
        let rows: Vec<Row> = query_and_search_family(workspace, query_id, search_id)
            .into_iter()
            .filter_map(|route| {
                let (kind, id) = route.inner?;
                Some((
                    route.label,
                    route.method,
                    route.uri,
                    route.body,
                    absent(kind, &id),
                ))
            })
            .collect();
        assert!(
            !rows.is_empty(),
            "case {case:?}: the family must carry rows addressing a resource inside the workspace"
        );
        assert_rows(&router, rows).await;
    }
}

#[tokio::test]
async fn resolving_outside_in_still_serves_the_query_and_search_the_workspace_holds() {
    let router = router();
    let (query, search) = async_resource_fixture(&router).await;

    // The resolution refuses nothing that resolves: every route of the family, on
    // the workspace that holds the query and the search, answers one of the
    // statuses `OPERATIONS.md` reports for it rather than an absence — a single
    // success status for all but the cancel and the search's results, whose answer
    // then depends on the status the clock has made due for the resource.
    for route in query_and_search_family(WORKSPACE, &query, &search) {
        let label = route.label;
        let served = route.served;
        let (status, body) = call(&router, route.method, &route.uri, route.body).await;
        assert!(
            served.contains(&status),
            "case {label:?}: a path whose every resource the store holds answers one of \
             {served:?}, got {status} {body}"
        );
    }

    // And the query and the search the fixture started are still the ones their own
    // paths report.
    let (status, body) = call(
        &router,
        "GET",
        &format!("/workspaces/{WORKSPACE}/queries/{query}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "DescribeQuery: got {status} {body}");
    assert_eq!(
        body["queryId"].as_str(),
        Some(query.as_str()),
        "DescribeQuery reports the query it was addressed with, expected {query}, got {body}"
    );

    let (status, body) = call(
        &router,
        "GET",
        &format!("/workspaces/{WORKSPACE}/searches/{search}"),
        None,
    )
    .await;
    assert_eq!(status, 200, "DescribeSearch: got {status} {body}");
    assert_eq!(
        body["searchId"].as_str(),
        Some(search.as_str()),
        "DescribeSearch reports the search it was addressed with, expected {search}, got {body}"
    );
    assert_eq!(
        body["workspaceName"].as_str(),
        Some(WORKSPACE),
        "DescribeSearch reports its workspace, expected {WORKSPACE}, got {body}"
    );
}
