// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ONE refusal a query with nowhere to write its results is
//! answered with.
//!
//! `ResultConfiguration$OutputLocation` states the rule
//! (`smithy_query(query="doc", shape="ResultConfiguration", member="OutputLocation")`):
//! "To run the query, you must specify the query results location using one of the ways:
//! either for individual queries using either this setting (client-side), or in the
//! workgroup […] If none of them is set, Athena issues an error that no output location is
//! provided." A workgroup that keeps its results in Athena-owned storage
//! (`ManagedQueryResultsConfiguration$Enabled`) is the other way of satisfying it. So
//! `StartQueryExecution` is refused — HTTP 400, `InvalidRequestException`, the class code
//! `INVALID_INPUT`, and the service's own sentence for that class — whenever neither the
//! request's `ResultConfiguration` nor the workgroup configuration in force at submit time
//! names an output location and that workgroup does not hold results in Athena's storage.
//!
//! The sentence is WHOLLY FIXED text: it states the rule and the two ways of satisfying
//! it, and interpolates nothing from the request or from the store. It is spelled out in
//! this suite rather than read from the crate, so that a change to the wording the service
//! emits fails here instead of travelling along with the constant.
//!
//! The rule is asserted over a TABLE rather than one request: the rows differ along where
//! the location is missing from (the request, the workgroup it was never given, the
//! workgroup it was removed from), the form the request's `ResultConfiguration` takes (left
//! out, sent empty, carrying other members, carrying an empty location), whether the
//! workgroup enforces its configuration or has Athena-owned storage turned off, the class
//! and size of the statement submitted, and the name the workgroup carries — so a sentence
//! that must not vary with any of those is seen not to, and the refusal is seen to precede
//! any planning of the statement. The second test is the other side of the rule: each of
//! the ways of naming a location runs a query instead.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::ERROR_CODE_INVALID_INPUT;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, history, message, request_token, router};

/// The sentence Athena refuses a query with nowhere to write its results with, word for
/// word — including the doubled space after "for", which is how the service emits it.
const NO_OUTPUT_LOCATION: &str = concat!(
    "No output location provided. You did not provide an output location for  your query ",
    "results. Either specify an S3 bucket location or enable Athena managed query results ",
    "in your workgroup settings.",
);

/// Where a query that HAS a location writes its results. Nothing is read back from it
/// here: what each row turns on is whether a location was named at all.
const OUTPUT_LOCATION: &str = "s3://athena-results/no-output-location/";

/// A workgroup created with no configuration at all, so nothing names a location for the
/// queries submitted to it.
const LOCATIONLESS: &str = "locationless";

/// A workgroup created WITH a location whose location is then removed — the state a
/// `ResultConfigurationUpdates.RemoveOutputLocation` leaves it in.
const REMOVED: &str = "location-removed";

/// A workgroup whose result configuration carries other members but no location.
const ENCRYPTED_ONLY: &str = "encryption-without-location";

/// A workgroup with Athena-owned storage explicitly turned OFF and no location: the
/// second way of satisfying the rule is not satisfied by a configuration that names it
/// disabled.
const MANAGED_OFF: &str = "managed-results-off";

/// A workgroup that enforces its own configuration while specifying no location of its
/// own: there is no setting of the workgroup's to use, so nothing names a location.
const ENFORCING: &str = "enforcing-without-location";

/// Create `name` with `configuration`, or with none where `configuration` is `None`.
async fn create_work_group(router: &MultiServiceRouter, name: &str, configuration: Option<Value>) {
    let mut request = json!({ "Name": name });
    if let Some(configuration) = configuration {
        request["Configuration"] = configuration;
    }
    let (status, created) = call(router, "CreateWorkGroup", &request).await;
    assert_eq!(
        status, 200,
        "the workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Apply `updates` to `name`'s configuration.
async fn update_work_group(router: &MultiServiceRouter, name: &str, updates: Value) {
    let (status, updated) = call(
        router,
        "UpdateWorkGroup",
        &json!({ "WorkGroup": name, "ConfigurationUpdates": updates }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup {name} must be updatable, got {status} with {updated:?}"
    );
}

/// The `OutputLocation` the workgroup `name` reports, or `None` where it reports none.
async fn reported_output_location(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let (status, body) = call(router, "GetWorkGroup", &json!({ "WorkGroup": name })).await;
    assert_eq!(
        status, 200,
        "GetWorkGroup({name}) must answer 200, got {status} with {body:?}"
    );
    body["WorkGroup"]["Configuration"]["ResultConfiguration"]["OutputLocation"]
        .as_str()
        .map(ToString::to_string)
}

/// Mount the service and bring every workgroup this suite submits to into the state its
/// rows describe. Answers the longest name a workgroup may carry, which is created
/// locationless too.
async fn prepared() -> (MultiServiceRouter, String) {
    let router = router();

    create_work_group(&router, LOCATIONLESS, None).await;
    create_work_group(&router, MANAGED_OFF, None).await;
    update_work_group(
        &router,
        MANAGED_OFF,
        json!({ "ManagedQueryResultsConfigurationUpdates": { "Enabled": false } }),
    )
    .await;
    create_work_group(
        &router,
        ENCRYPTED_ONLY,
        Some(json!({
            "ResultConfiguration": {
                "EncryptionConfiguration": { "EncryptionOption": "SSE_S3" },
            },
        })),
    )
    .await;
    create_work_group(
        &router,
        ENFORCING,
        Some(json!({ "EnforceWorkGroupConfiguration": true })),
    )
    .await;

    // The recorded path: a workgroup that HAD a location, with the location removed.
    create_work_group(
        &router,
        REMOVED,
        Some(json!({ "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION } })),
    )
    .await;
    assert_eq!(
        reported_output_location(&router, REMOVED).await.as_deref(),
        Some(OUTPUT_LOCATION),
        "the workgroup {REMOVED} must start out naming a location"
    );
    update_work_group(
        &router,
        REMOVED,
        json!({ "ResultConfigurationUpdates": { "RemoveOutputLocation": true } }),
    )
    .await;
    assert_eq!(
        reported_output_location(&router, REMOVED).await,
        None,
        "the removal must leave the workgroup {REMOVED} naming no location"
    );

    // The model's `WorkGroupName` is `^[a-zA-Z0-9._-]{1,128}$`, so the longest name a
    // client can submit under is a row of this table too.
    let longest = "w".repeat(128);
    create_work_group(&router, &longest, None).await;

    (router, longest)
}

/// A query neither the request nor the workgroup in force names a location for is refused
/// with the service's sentence, whatever the statement, the form of the request's result
/// configuration or the workgroup it was submitted to — and nothing is minted for it.
#[tokio::test]
async fn a_query_with_nowhere_to_write_its_results_is_refused_with_the_services_sentence() {
    let (router, longest) = prepared().await;

    // A statement far longer than any row needs to be readable, to show the refusal does
    // not depend on the size of the SQL it was asked about.
    let wide = format!(
        "SELECT {}",
        (0..200)
            .map(|column| format!("{column} AS c{column}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // (case, the workgroup the row submits to — where it names one, the request)
    let rows: Vec<(&str, Option<&str>, Value)> = vec![
        (
            "no workgroup named and no result configuration",
            None,
            json!({ "QueryString": "SELECT 1 AS one" }),
        ),
        (
            "a workgroup created with no configuration",
            Some(LOCATIONLESS),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": LOCATIONLESS }),
        ),
        (
            "a result configuration sent empty",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": LOCATIONLESS,
                "ResultConfiguration": {},
            }),
        ),
        (
            "a result configuration whose location is the empty string",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": LOCATIONLESS,
                "ResultConfiguration": { "OutputLocation": "" },
            }),
        ),
        (
            "a result configuration that encrypts results it names no location for",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": LOCATIONLESS,
                "ResultConfiguration": {
                    "EncryptionConfiguration": { "EncryptionOption": "SSE_S3" },
                    "ExpectedBucketOwner": "111122223333",
                },
            }),
        ),
        (
            "a workgroup whose location was removed",
            Some(REMOVED),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": REMOVED }),
        ),
        (
            "a workgroup whose location was removed, under an idempotency token",
            Some(REMOVED),
            json!({
                "QueryString": "SELECT ? AS parameter",
                "WorkGroup": REMOVED,
                "ClientRequestToken": request_token("no-location"),
                "ExecutionParameters": ["1"],
                "QueryExecutionContext": { "Catalog": "awsdatacatalog", "Database": "default" },
            }),
        ),
        (
            "a workgroup whose result configuration names only encryption",
            Some(ENCRYPTED_ONLY),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": ENCRYPTED_ONLY }),
        ),
        (
            "a workgroup with Athena-owned storage turned off",
            Some(MANAGED_OFF),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": MANAGED_OFF }),
        ),
        (
            "a workgroup that enforces a configuration naming no location",
            Some(ENFORCING),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": ENFORCING,
                "ResultConfiguration": {},
            }),
        ),
        (
            "a statement that creates a database rather than reading rows",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "CREATE DATABASE no_output_location_db",
                "WorkGroup": LOCATIONLESS,
            }),
        ),
        (
            "a statement that writes a table from a query",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "CREATE TABLE ctas_target AS SELECT 1 AS one",
                "WorkGroup": LOCATIONLESS,
            }),
        ),
        (
            "a statement of two hundred projections",
            Some(LOCATIONLESS),
            json!({ "QueryString": wide, "WorkGroup": LOCATIONLESS }),
        ),
        (
            "the longest name a workgroup may carry",
            Some(longest.as_str()),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": longest }),
        ),
    ];

    let mut submitted_to: Vec<String> = Vec::new();
    for (case, work_group, request) in rows {
        let (status, body) = call(&router, "StartQueryExecution", &request).await;
        assert_refusal(case, status, &body, ERROR_CODE_INVALID_INPUT, NO_OUTPUT_LOCATION);
        // Nothing of the request or of the store is in the sentence: the workgroup it
        // named and the statement it carried appear nowhere in it.
        let stated = message(&body);
        if let Some(work_group) = work_group {
            assert!(
                !stated.contains(work_group),
                "case {case:?}: the sentence must name nothing of the request, got {stated:?}"
            );
            submitted_to.push(work_group.to_string());
        }
        assert!(
            !stated.contains("SELECT") && !stated.contains("CREATE"),
            "case {case:?}: the sentence must name nothing of the statement, got {stated:?}"
        );
    }

    // A refused submission mints no execution: every workgroup these rows addressed holds
    // an empty query history.
    submitted_to.sort();
    submitted_to.dedup();
    for work_group in submitted_to {
        let listed = history(&router, &work_group).await;
        assert!(
            listed.is_empty(),
            "case {work_group:?}: a refused submission must mint no execution, got {listed:?}"
        );
    }
}

/// The other side of the rule: each of the ways of naming where results go runs a query —
/// the workgroup's own location, the request's own location (including against a workgroup
/// that names none and one that enforces a configuration naming none), Athena-owned
/// storage, and a location restored after a removal.
#[tokio::test]
async fn a_query_that_names_where_its_results_go_is_accepted() {
    let (router, _longest) = prepared().await;

    // The workgroup that names a location of its own, and the one that holds its results
    // in Athena-owned storage: the two ways the refusal's sentence points a caller at.
    const WITH_LOCATION: &str = "with-location";
    const MANAGED_ON: &str = "managed-results-on";
    create_work_group(
        &router,
        WITH_LOCATION,
        Some(json!({ "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION } })),
    )
    .await;
    create_work_group(
        &router,
        MANAGED_ON,
        Some(json!({ "ManagedQueryResultsConfiguration": { "Enabled": true } })),
    )
    .await;
    // The removal is undone by naming a location again, which is the same workgroup the
    // refusal table submitted to.
    update_work_group(
        &router,
        REMOVED,
        json!({ "ResultConfigurationUpdates": { "OutputLocation": OUTPUT_LOCATION } }),
    )
    .await;

    // (case, the workgroup the row submits to — where it names one, the request)
    let rows: Vec<(&str, Option<&str>, Value)> = vec![
        (
            "the workgroup's own location",
            Some(WITH_LOCATION),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": WITH_LOCATION }),
        ),
        (
            "the request's own location, against a workgroup naming none",
            Some(LOCATIONLESS),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": LOCATIONLESS,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
        ),
        (
            "the request's own location, with no workgroup named",
            None,
            json!({
                "QueryString": "SELECT 1 AS one",
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
        ),
        (
            "the request's own location, against a workgroup enforcing a configuration \
             naming none",
            Some(ENFORCING),
            json!({
                "QueryString": "SELECT 1 AS one",
                "WorkGroup": ENFORCING,
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
            }),
        ),
        (
            "Athena-owned storage, with no location anywhere",
            Some(MANAGED_ON),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": MANAGED_ON }),
        ),
        (
            "a location restored after a removal",
            Some(REMOVED),
            json!({ "QueryString": "SELECT 1 AS one", "WorkGroup": REMOVED }),
        ),
    ];

    for (case, work_group, request) in rows {
        let (status, body) = call(&router, "StartQueryExecution", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a query that names where its results go must be accepted, got \
             {status} with {body:?}"
        );
        let id = body["QueryExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an execution id, got {body:?}"))
            .to_string();
        if let Some(work_group) = work_group {
            let listed = history(&router, work_group).await;
            assert!(
                listed.contains(&id),
                "case {case:?}: expected {work_group}'s history to hold {id}, got {listed:?}"
            );
        }
    }
}
