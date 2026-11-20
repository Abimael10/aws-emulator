// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOLUTION every instant this service reports is served at.
//!
//! The rule: an Athena timestamp is an epoch-seconds number carrying at most millisecond
//! resolution — every `Date` member of this model, whichever record it is projected from,
//! on the awsJson1_1 wire. The instant itself is the clock's and is not under test: what is
//! asserted is the wire form, so a client that formats the value, compares it, or
//! round-trips it through a type with millisecond resolution gets a value this service
//! really served.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots. The rows span what the rule generalises over: the record
//! the instant comes from (workgroup, prepared statement, query execution, capacity
//! reservation, notebook), whether it is reported by a detailed read or by a listing's
//! summary, and whether the instant is the one a write set or one the clock later made due.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, router as build_router, settled, start};

/// The most fractional digits an epoch-seconds timestamp of this service carries: three,
/// which is one millisecond.
const MAX_FRACTIONAL_DIGITS: usize = 3;

/// Where the queries in this suite write their results. Nothing is written to it.
const OUTPUT_LOCATION: &str = "s3://athena-results/instants/";

/// How many fractional digits the JSON number `value` was rendered with, trailing zeros
/// dropped — the form a client reads off the wire.
fn fractional_digits(rendered: &str) -> usize {
    rendered
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.trim_end_matches('0').len())
}

/// Assert `value` is an instant this service may serve: a JSON number in plain decimal
/// form, naming a real instant, at no finer than millisecond resolution.
fn assert_millisecond_instant(case: &str, member: &str, value: &Value) {
    let seconds = value.as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be an epoch-seconds number, got {value:?}")
    });
    assert!(
        seconds > 0.0,
        "case {case:?}: {member} must name a real instant, got {value:?}"
    );
    let rendered = value.to_string();
    assert!(
        !rendered.contains(['e', 'E']),
        "case {case:?}: {member} must be rendered in plain decimal form, got {rendered}"
    );
    assert!(
        fractional_digits(&rendered) <= MAX_FRACTIONAL_DIGITS,
        "case {case:?}: {member} carries at most {MAX_FRACTIONAL_DIGITS} fractional digits \
         (one millisecond), got {rendered}"
    );
}

/// Assert every member of `members` found under `document` is such an instant, and that
/// each one named as required is there at all.
fn assert_instants(case: &str, document: &Value, members: &[(&str, bool)]) {
    for (pointer, required) in members {
        let value = document.pointer(pointer).unwrap_or(&Value::Null);
        if value.is_null() {
            assert!(
                !required,
                "case {case:?}: expected an instant at {pointer}, got {document:?}"
            );
            continue;
        }
        assert_millisecond_instant(case, pointer, value);
    }
}

/// The body of one call, asserted to have succeeded.
async fn succeeds(router: &MultiServiceRouter, operation: &str, request: &Value) -> Value {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 200,
        "{operation}({request}) must answer 200, got {status} with {body:?}"
    );
    body
}

#[tokio::test]
async fn every_work_group_instant_is_reported_at_millisecond_resolution() {
    let router = build_router();

    // The rows differ in how the workgroup came to be: created bare, created with a
    // configuration, updated after creation, and the account's own default workgroup, which
    // no create ever wrote.
    let created: Vec<(&str, Value)> = vec![
        ("a workgroup created bare", json!({ "Name": "instants-bare" })),
        (
            "a workgroup created with a configuration",
            json!({
                "Name": "instants-configured",
                "Configuration": { "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION } },
            }),
        ),
    ];
    for (case, request) in &created {
        succeeds(&router, "CreateWorkGroup", request).await;
        let name = request["Name"].as_str().expect("the row names the workgroup");
        let read = succeeds(&router, "GetWorkGroup", &json!({ "WorkGroup": name })).await;
        assert_instants(case, &read, &[("/WorkGroup/CreationTime", true)]);
    }

    succeeds(
        &router,
        "UpdateWorkGroup",
        &json!({ "WorkGroup": "instants-bare", "Description": "updated" }),
    )
    .await;
    let read = succeeds(
        &router,
        "GetWorkGroup",
        &json!({ "WorkGroup": "instants-bare" }),
    )
    .await;
    assert_instants(
        "a workgroup read back after an update",
        &read,
        &[("/WorkGroup/CreationTime", true)],
    );

    // Every summary a listing reports, including the account's default workgroup's.
    let listed = succeeds(&router, "ListWorkGroups", &json!({})).await;
    let summaries = listed["WorkGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        summaries.len() >= created.len(),
        "the listing must report every workgroup, got {listed:?}"
    );
    for summary in &summaries {
        let name = summary["Name"].as_str().unwrap_or("<no Name>").to_string();
        assert_instants(&name, summary, &[("/CreationTime", true)]);
    }
}

#[tokio::test]
async fn every_prepared_statement_instant_is_reported_at_millisecond_resolution() {
    let router = build_router();
    succeeds(
        &router,
        "CreateWorkGroup",
        &json!({ "Name": "instants-statements" }),
    )
    .await;

    // The rows differ in what wrote the instant: the create, and a later update.
    for (case, operation) in [
        ("the instant a create wrote", "CreatePreparedStatement"),
        ("the instant an update wrote", "UpdatePreparedStatement"),
    ] {
        succeeds(
            &router,
            operation,
            &json!({
                "StatementName": "instants_statement",
                "WorkGroup": "instants-statements",
                "QueryStatement": "SELECT 1 AS one",
            }),
        )
        .await;
        let read = succeeds(
            &router,
            "GetPreparedStatement",
            &json!({
                "StatementName": "instants_statement",
                "WorkGroup": "instants-statements",
            }),
        )
        .await;
        assert_instants(
            case,
            &read,
            &[("/PreparedStatement/LastModifiedTime", true)],
        );

        let listed = succeeds(
            &router,
            "ListPreparedStatements",
            &json!({ "WorkGroup": "instants-statements" }),
        )
        .await;
        for summary in listed["PreparedStatements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            assert_instants(case, &summary, &[("/LastModifiedTime", true)]);
        }
    }
}

#[tokio::test]
async fn every_query_execution_instant_is_reported_at_millisecond_resolution() {
    let router = build_router();

    // Two instants of one record, and the rule holds for both: the one the submit wrote and
    // the one the clock made due when the query completed.
    let id = start(
        &router,
        &json!({
            "QueryString": "SELECT 1 AS one",
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;

    let read = succeeds(
        &router,
        "GetQueryExecution",
        &json!({ "QueryExecutionId": id }),
    )
    .await;
    assert_instants(
        "a query read straight back",
        &read,
        &[
            ("/QueryExecution/Status/SubmissionDateTime", true),
            // A query that has not settled yet reports no completion.
            ("/QueryExecution/Status/CompletionDateTime", false),
        ],
    );

    // Waits for the CONDITION — the execution reaching a terminal state — rather than for a
    // span.
    let found = settled(&router, &id).await;
    for pointer in ["/Status/SubmissionDateTime", "/Status/CompletionDateTime"] {
        assert_instants("a settled query", &found, &[(pointer, true)]);
    }

    let batch = succeeds(
        &router,
        "BatchGetQueryExecution",
        &json!({ "QueryExecutionIds": [id] }),
    )
    .await;
    for execution in batch["QueryExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        assert_instants(
            "a query a batch read reports",
            &execution,
            &[
                ("/Status/SubmissionDateTime", true),
                ("/Status/CompletionDateTime", true),
            ],
        );
    }
}

#[tokio::test]
async fn every_capacity_reservation_instant_is_reported_at_millisecond_resolution() {
    let router = build_router();
    // The rows differ in the amount reserved, which is what decides nothing about the
    // instants — so both rows must report the same wire form.
    for (case, name, target_dpus) in [
        ("the smallest reservation", "instants-reservation-small", 4),
        ("a larger reservation", "instants-reservation-large", 24),
    ] {
        succeeds(
            &router,
            "CreateCapacityReservation",
            &json!({ "Name": name, "TargetDpus": target_dpus }),
        )
        .await;
        let read = succeeds(
            &router,
            "GetCapacityReservation",
            &json!({ "Name": name }),
        )
        .await;
        assert_instants(
            case,
            &read,
            &[
                ("/CapacityReservation/CreationTime", true),
                ("/CapacityReservation/LastAllocation/RequestTime", true),
                // Reported once the allocation is complete, which a reservation read
                // straight back is not.
                (
                    "/CapacityReservation/LastAllocation/RequestCompletionTime",
                    false,
                ),
                ("/CapacityReservation/LastSuccessfulAllocationTime", false),
            ],
        );
    }
}

#[tokio::test]
async fn every_notebook_instant_is_reported_at_millisecond_resolution() {
    let router = build_router();
    succeeds(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": "instants-spark",
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": "PySpark engine version 3" },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
            },
        }),
    )
    .await;
    let created = succeeds(
        &router,
        "CreateNotebook",
        &json!({ "WorkGroup": "instants-spark", "Name": "instants-notebook" }),
    )
    .await;
    let notebook_id = created["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNotebook must answer an id, got {created:?}"))
        .to_string();

    let read = succeeds(
        &router,
        "GetNotebookMetadata",
        &json!({ "NotebookId": notebook_id }),
    )
    .await;
    assert_instants(
        "a created notebook",
        &read,
        &[
            ("/NotebookMetadata/CreationTime", true),
            ("/NotebookMetadata/LastModifiedTime", true),
        ],
    );

    let listed = succeeds(
        &router,
        "ListNotebookMetadata",
        &json!({ "WorkGroup": "instants-spark" }),
    )
    .await;
    for metadata in listed["NotebookMetadataList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        assert_instants(
            "a listed notebook",
            &metadata,
            &[("/CreationTime", true), ("/LastModifiedTime", true)],
        );
    }
}
