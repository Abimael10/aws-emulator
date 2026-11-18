// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of three Athena refusals whose sentence is the
//! service's own and carries nothing of the request beyond what Athena interpolates:
//!
//! * a create of a data catalog under a name this account already holds one under, which
//!   states that the named catalog has already been created;
//! * a create or update whose `Parameters` map carries NOTHING for a catalog type whose
//!   parameters are required, which states that that type's parameters cannot be empty —
//!   a different rule from the missing-key one, which names the keys a non-empty map
//!   lacks;
//! * a read of the results of a query a caller INTERRUPTED, which reports that there are
//!   no results to find rather than the state the execution ended in.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! catalog type, the form and length of the name, whether the map was sent empty or left
//! out, the class of statement submitted, and the state the execution is in — so a
//! sentence that must not vary with any of those is seen not to.
//!
//! Every call goes over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_TYPE_FEDERATED, DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE,
    DATA_CATALOG_TYPE_LAMBDA, ERROR_CODE_INVALID_INPUT, ERROR_CODE_INVALID_QUERY_EXECUTION_STATE,
    MAX_DATA_CATALOG_NAME_CHARS, QUERY_STATE_FAILED, QUERY_STATE_QUEUED, QUERY_STATE_RUNNING,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, message, router, settled, start};

/// Where the queries in this suite write their results, so the refusal each row is about
/// is the one it names rather than the missing-location one.
const OUTPUT_LOCATION: &str = "s3://athena-results/refusal-wording/";

/// A `StartQueryExecution` body for `sql` that names where its results go.
fn query(sql: &str) -> Value {
    json!({
        "QueryString": sql,
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// The `DataCatalog` the registry holds under `name`, or `None` where it holds none.
async fn held_catalog(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status == 200).then(|| body["DataCatalog"].clone())
}

/// A create of a data catalog under a name this account already holds one under is
/// refused as already created, whatever the type or the form of the name, and the catalog
/// the registry already held is left exactly as it was.
#[tokio::test]
async fn a_create_under_a_name_already_held_says_the_catalog_has_already_been_created() {
    let router = router();

    // (case, the Type, the Parameters, the Name) — the rows differ along the type whose
    // parameters the map describes, and along the form and length of the name: one
    // character, each punctuation character the name documentation allows, and the longest
    // name a non-federated catalog may carry.
    let longest = "l".repeat(MAX_DATA_CATALOG_NAME_CHARS);
    let cases: Vec<(&str, &str, Value, String)> = vec![
        (
            "a GLUE catalog",
            DATA_CATALOG_TYPE_GLUE,
            json!({ "catalog-id": "111122223333" }),
            "dup_glue".to_string(),
        ),
        (
            "a HIVE catalog named with every allowed punctuation character",
            DATA_CATALOG_TYPE_HIVE,
            json!({ "metadata-function": "arn:aws:lambda:us-east-1:111122223333:function:hms" }),
            "dup_hive@2-x".to_string(),
        ),
        (
            "a LAMBDA catalog named by one character",
            DATA_CATALOG_TYPE_LAMBDA,
            json!({ "function": "arn:aws:lambda:us-east-1:111122223333:function:both" }),
            "d".to_string(),
        ),
        (
            "a LAMBDA catalog under the longest name a catalog may carry",
            DATA_CATALOG_TYPE_LAMBDA,
            json!({
                "metadata-function": "arn:aws:lambda:us-east-1:111122223333:function:meta",
                "record-function": "arn:aws:lambda:us-east-1:111122223333:function:rec"
            }),
            longest,
        ),
        (
            "a FEDERATED catalog reusing a connection",
            DATA_CATALOG_TYPE_FEDERATED,
            json!({ "connection-arn": "arn:aws:glue:us-east-1:111122223333:connection/c" }),
            "dup-federated".to_string(),
        ),
    ];

    for (case, catalog_type, parameters, name) in cases {
        let create = json!({
            "Name": name,
            "Type": catalog_type,
            "Description": "the created description",
            "Parameters": parameters,
        });
        let (status, created) = call(&router, "CreateDataCatalog", &create).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first create must be accepted, got {status} with {created:?}"
        );
        let registered = held_catalog(&router, &name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the created catalog must be readable"));

        // Fixed text, the Name the request carried, and Athena's verb for a name already
        // registered.
        let expected = format!("DataCatalog {name} has already been created.");

        // The same request again, and the same name under ANOTHER type with another
        // parameter map: both are a name already taken, so both carry this one sentence.
        let second_of_another_type = json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_GLUE,
            "Parameters": { "catalog-id": "444455556666" },
        });
        let attempts = [
            ("the same request", &create),
            ("a create of another type under the same name", &second_of_another_type),
        ];
        for (attempt, request) in attempts {
            let (status, body) = call(&router, "CreateDataCatalog", request).await;
            assert_refusal(
                &format!("{case} / {attempt}"),
                status,
                &body,
                ERROR_CODE_INVALID_INPUT,
                &expected,
            );
        }

        // The refused creates wrote nothing: the catalog reads back exactly as created.
        assert_eq!(
            held_catalog(&router, &name).await,
            Some(registered.clone()),
            "case {case:?}: expected the registered catalog {registered:?} to stand"
        );
    }
}

/// A `Parameters` map that carries nothing is refused by its OWN rule — the type's
/// parameters cannot be empty — while a map that carries something but not the keys the
/// type documents is refused by the missing-key rule, which names those keys. Neither
/// create registers anything.
#[tokio::test]
async fn an_empty_parameters_map_is_refused_as_empty_and_a_lacking_one_by_its_keys() {
    let router = router();

    // Every value of the model's `DataCatalogType` enum
    // (smithy_query(query="shape",shape="DataCatalogType")), each paired with the sentence
    // that states its parameters cannot be empty: fixed text with the type named as the
    // sentence names it, and nothing else of the request in it.
    let empty_rule: Vec<(&str, &str)> = vec![
        (DATA_CATALOG_TYPE_LAMBDA, "Lambda catalog parameters cannot be empty."),
        (DATA_CATALOG_TYPE_GLUE, "Glue catalog parameters cannot be empty."),
        (DATA_CATALOG_TYPE_HIVE, "Hive catalog parameters cannot be empty."),
        (
            DATA_CATALOG_TYPE_FEDERATED,
            "Federated catalog parameters cannot be empty.",
        ),
    ];

    for (catalog_type, expected) in empty_rule {
        // A map sent empty and a map left out are the same map: neither names a parameter.
        let requests: [(&str, Option<Value>); 2] = [
            ("Parameters sent empty", Some(json!({}))),
            ("Parameters left out", None),
        ];
        for (index, (form, parameters)) in requests.into_iter().enumerate() {
            let case = format!("{catalog_type} with {form}");
            let name = format!("empty_{catalog_type}_{index}");
            let mut request = json!({ "Name": name, "Type": catalog_type });
            if let Some(parameters) = parameters {
                request["Parameters"] = parameters;
            }
            let (status, body) = call(&router, "CreateDataCatalog", &request).await;
            assert_refusal(&case, status, &body, ERROR_CODE_INVALID_INPUT, expected);
            assert_eq!(
                held_catalog(&router, &name).await,
                None,
                "case {case:?}: the refused create must have registered nothing"
            );
        }
    }

    // (case, the Type, a non-empty map that does not carry the keys the type documents, the
    // opening of the sentence that names what the map lacks) — the OTHER rule: the sentence
    // names the parameters instead, and says nothing about the map being empty. The Lambda
    // type's two required sets are one rule stated in one sentence, which is why its opening
    // differs from the single-key types'.
    let missing_keys: Vec<(&str, &str, Value, String)> = vec![
        (
            "LAMBDA with only one half of the split form",
            DATA_CATALOG_TYPE_LAMBDA,
            json!({ "metadata-function": "arn:aws:lambda:us-east-1:111122223333:function:meta" }),
            "Lambda catalogs require that either the ".to_string(),
        ),
        (
            "HIVE with only the optional sdk version",
            DATA_CATALOG_TYPE_HIVE,
            json!({ "sdk-version": "1.0" }),
            format!("Parameters for a {DATA_CATALOG_TYPE_HIVE} data catalog must "),
        ),
        (
            "GLUE without its catalog id",
            DATA_CATALOG_TYPE_GLUE,
            json!({ "sdk-version": "1.0" }),
            format!("Parameters for a {DATA_CATALOG_TYPE_GLUE} data catalog must "),
        ),
        (
            "FEDERATED with neither connection parameter",
            DATA_CATALOG_TYPE_FEDERATED,
            json!({ "sdk-version": "1.0" }),
            format!("Parameters for a {DATA_CATALOG_TYPE_FEDERATED} data catalog must "),
        ),
    ];

    for (index, (case, catalog_type, parameters, expected_opening)) in
        missing_keys.into_iter().enumerate()
    {
        let name = format!("lacking_{index}");
        let (status, body) = call(
            &router,
            "CreateDataCatalog",
            &json!({ "Name": name, "Type": catalog_type, "Parameters": parameters }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        let stated = message(&body);
        assert!(
            stated.starts_with(&expected_opening),
            "case {case:?}: expected the sentence naming what the map lacks, opening \
             {expected_opening:?}, got {stated:?}"
        );
        assert!(
            !stated.contains("cannot be empty"),
            "case {case:?}: a map that carries a parameter is not the empty-map rule, got \
             {stated:?}"
        );
        assert_eq!(
            held_catalog(&router, &name).await,
            None,
            "case {case:?}: the refused create must have registered nothing"
        );
    }
}

/// An UPDATE is held to the same two rules as a create: a map sent empty is refused as
/// empty, and the catalog it addressed is left as it was.
#[tokio::test]
async fn an_update_whose_parameters_map_is_sent_empty_is_refused_as_empty() {
    let router = router();
    let name = "update_empty_parameters";
    let created = json!({
        "Name": name,
        "Type": DATA_CATALOG_TYPE_LAMBDA,
        "Parameters": { "function": "arn:aws:lambda:us-east-1:111122223333:function:both" },
    });
    let (status, body) = call(&router, "CreateDataCatalog", &created).await;
    assert_eq!(
        status, 200,
        "the catalog must be created, got {status} with {body:?}"
    );
    let registered = held_catalog(&router, name)
        .await
        .expect("the created catalog must be readable");

    let (status, refused) = call(
        &router,
        "UpdateDataCatalog",
        &json!({ "Name": name, "Type": DATA_CATALOG_TYPE_LAMBDA, "Parameters": {} }),
    )
    .await;
    assert_refusal(
        "an update that sends an empty parameter map",
        status,
        &refused,
        ERROR_CODE_INVALID_INPUT,
        "Lambda catalog parameters cannot be empty.",
    );
    assert_eq!(
        held_catalog(&router, name).await,
        Some(registered.clone()),
        "the refused update must leave {registered:?} standing"
    );
}

/// A read of results the service cannot serve says WHY in the words of that reason: a
/// query a caller interrupted has no results to find, a query still on its way to a
/// terminal state has not finished, and a query that ran to an unsuccessful end reports
/// that final state.
#[tokio::test]
async fn a_read_of_results_reports_absence_for_an_interrupted_query() {
    let router = router();

    // The rows differ along the class of statement submitted — one whose results are rows
    // and one whose result is the engine's text output — so the sentence is seen not to
    // vary with it either.
    let cancelled: Vec<(&str, &str)> = vec![
        ("a projection a caller stopped", "SELECT 1 AS one"),
        (
            "a statement with text output a caller stopped",
            "CREATE DATABASE refusal_wording_db",
        ),
    ];

    for (case, sql) in cancelled {
        let id = start(&router, &query(sql)).await;
        let (status, stopped) = call(
            &router,
            "StopQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the stop must be accepted, got {status} with {stopped:?}"
        );
        let (status, body) =
            call(&router, "GetQueryResults", &json!({ "QueryExecutionId": id })).await;
        // Wholly fixed text: neither the execution nor the state it is in is in it.
        assert_refusal(
            case,
            status,
            &body,
            ERROR_CODE_INVALID_QUERY_EXECUTION_STATE,
            "Could not find results",
        );
    }

    // A query that has not finished names the state it is in instead. Which of the two
    // in-flight states it is in is the service's to decide, so both sentences are
    // acceptable here — nothing in this row turns on which, and reading the state from a
    // second call could not bind the one the refusal reports anyway.
    let unfinished = start(&router, &query("SELECT 2 AS two")).await;
    let (status, body) = call(
        &router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": unfinished }),
    )
    .await;
    let in_flight: Vec<String> = [QUERY_STATE_QUEUED, QUERY_STATE_RUNNING]
        .iter()
        .map(|state| format!("Query has not yet finished. Current state: {state}"))
        .collect();
    assert_eq!(
        status, 400,
        "a read of a query that has not finished must answer 400, got {status} with {body:?}"
    );
    assert_eq!(
        body["AthenaErrorCode"].as_str(),
        Some(ERROR_CODE_INVALID_QUERY_EXECUTION_STATE),
        "a read of a query that has not finished must carry its class, got {body:?}"
    );
    assert!(
        in_flight.contains(&message(&body)),
        "a read of a query that has not finished must state one of {in_flight:?}, got {:?}",
        message(&body)
    );

    // A query that ran to an unsuccessful end reports that final state.
    let failed = start(&router, &query("SELECT * FROM nowhere_at_all")).await;
    let found = settled(&router, &failed).await;
    assert_eq!(
        found["Status"]["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "the query must have failed, got {found:?}"
    );
    let (status, body) = call(
        &router,
        "GetQueryResults",
        &json!({ "QueryExecutionId": failed }),
    )
    .await;
    assert_refusal(
        "a read of a query that failed",
        status,
        &body,
        ERROR_CODE_INVALID_QUERY_EXECUTION_STATE,
        &format!("Query did not finish successfully. Final query state: {QUERY_STATE_FAILED}"),
    );
}
