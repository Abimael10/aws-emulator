// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY of a notebook refusal: its class code and its wording, driven end to end
//! over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`).
//!
//! TWO RULES ARE UNDER TEST.
//!
//! 1. An operation that needs a Spark enabled workgroup is refused, when the workgroup it
//!    named runs Athena SQL, with `InvalidRequestException` (HTTP 400) carrying the class
//!    `INVALID_INPUT` under both of the names the wire gives a refusal's code
//!    (`AthenaErrorCode` and the `ErrorCode` alias) and a sentence about the workgroup's
//!    ENGINE VERSION that is fixed text end to end: it carries no name, id or value from
//!    the request, so every such refusal reads identically whichever workgroup was
//!    addressed.
//! 2. An operation that addresses ONE notebook by `NotebookId` is refused, when no
//!    notebook of that id exists, with the same envelope carrying the class
//!    `NOTEBOOK_NOT_FOUND` under both of those names — the one machine-readable signal
//!    that tells a permanent miss from a request malformed some other way — and a sentence
//!    whose one variable part is the id the caller asked for.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along the
//! operation refused and, for rule 1, along the identity of the workgroup named (an Athena
//! SQL workgroup, one named by a single character, one named by the longest name the model
//! admits, and the account's own default workgroup), so a sentence that must not vary with
//! the request is seen not to; for rule 2, along the shape of the id that names no notebook.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DEFAULT_WORK_GROUP, ERROR_CODE_INVALID_INPUT, ERROR_CODE_NOTEBOOK_NOT_FOUND,
    NOTEBOOK_ID_CONFIG, NOTEBOOK_TYPE_IPYNB,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, create_work_group, message, router};

/// The Spark enabled workgroup the notebooks under test live in.
const SPARK_WORK_GROUP: &str = "spark-refusal-identity";

/// An Athena SQL workgroup: it runs no notebook engine, so no notebook lives in it.
const SQL_WORK_GROUP: &str = "sql-refusal-identity";

/// The PySpark engine version a Spark enabled workgroup selects
/// (`state::PYSPARK_ENGINE_VERSION_PREFIX` is the family this belongs to).
const PYSPARK_ENGINE_VERSION: &str = "PySpark engine version 3";

/// AWS's wording for the refusal of a request that needs a Spark enabled workgroup: the
/// whole sentence is fixed text, with no variable part at all.
const ENGINE_VERSION_NOT_SPARK: &str = "Workgroup EngineVersion is not Spark.";

/// AWS's wording for a refusal whose `NotebookId` names no notebook this account holds:
/// fixed text around the one variable part, the id the CALLER asked for.
fn notebook_not_found(notebook_id: &str) -> String {
    format!("Notebook {notebook_id} was not found")
}

/// Create a Spark enabled workgroup called `name`: a PySpark engine version and the
/// execution role a Spark session runs under.
async fn given_spark_work_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": PYSPARK_ENGINE_VERSION },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup {name} must be created, got {status} with {created:?}"
    );
}

/// Import one notebook into `work_group` and answer with the `NotebookId` it minted.
async fn given_notebook(router: &MultiServiceRouter, work_group: &str, name: &str) -> String {
    let (status, imported) = call(
        router,
        "ImportNotebook",
        &json!({
            "WorkGroup": work_group,
            "Name": name,
            "Type": NOTEBOOK_TYPE_IPYNB,
            "Payload": "{}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the notebook {name} must be imported, got {status} with {imported:?}"
    );
    imported["NotebookId"]
        .as_str()
        .unwrap_or_else(|| panic!("ImportNotebook must answer with an id, got {imported:?}"))
        .to_string()
}

/// Rule 1: every operation that needs a Spark enabled workgroup refuses a workgroup running
/// Athena SQL with the `INVALID_INPUT` class under both names and the fixed sentence about
/// the engine version — the same sentence whichever workgroup the request named.
#[tokio::test]
async fn a_non_spark_workgroup_is_refused_with_the_fixed_engine_version_sentence() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    // The rows' SQL workgroups: one plainly named, one named by a single character, and one
    // named by the longest name the modelled `WorkGroupName`
    // (`^[a-zA-Z0-9._-]{1,128}$`) admits.
    let longest_work_group = "s".repeat(128);
    create_work_group(&router, SQL_WORK_GROUP).await;
    create_work_group(&router, "s").await;
    create_work_group(&router, &longest_work_group).await;

    // The workgroup identities the rows differ along, each of them running Athena SQL.
    let work_groups: Vec<(&str, String)> = vec![
        ("Athena SQL workgroup", SQL_WORK_GROUP.to_string()),
        ("single-character workgroup", "s".to_string()),
        ("longest-named workgroup", longest_work_group.clone()),
        (
            "account's own default workgroup",
            DEFAULT_WORK_GROUP.to_string(),
        ),
    ];

    // (the case, the operation, the request naming that workgroup)
    let mut rows: Vec<(String, &str, Value)> = Vec::new();
    for (kind, work_group) in &work_groups {
        rows.push((
            format!("CreateNotebook into the {kind}"),
            "CreateNotebook",
            json!({ "WorkGroup": work_group, "Name": "refused" }),
        ));
        rows.push((
            format!("ImportNotebook into the {kind}"),
            "ImportNotebook",
            json!({
                "WorkGroup": work_group,
                "Name": "refused.ipynb",
                "Type": NOTEBOOK_TYPE_IPYNB,
                "Payload": "{}",
            }),
        ));
        rows.push((
            format!("StartSession in the {kind}"),
            "StartSession",
            json!({ "WorkGroup": work_group, "EngineConfiguration": {} }),
        ));
    }

    let mut sentences: Vec<String> = Vec::new();
    for (case, operation, request) in rows {
        let (status, refused) = call(&router, operation, &request).await;
        assert_refusal(
            &case,
            status,
            &refused,
            ERROR_CODE_INVALID_INPUT,
            ENGINE_VERSION_NOT_SPARK,
        );
        // The sentence is fixed text, so the name the request carried cannot appear in it:
        // the two distinctive names below are the ones a template would have interpolated.
        for name in [SQL_WORK_GROUP, longest_work_group.as_str()] {
            assert!(
                !message(&refused).contains(name),
                "case {case:?}: the refusal must name no workgroup, got {:?}",
                message(&refused)
            );
        }
        sentences.push(message(&refused));
    }
    assert!(
        sentences.windows(2).all(|pair| pair[0] == pair[1]),
        "every non-Spark refusal must read identically, got {sentences:?}"
    );

    // The Spark enabled workgroup, by contrast, takes the identical requests.
    let (status, created) = call(
        &router,
        "CreateNotebook",
        &json!({ "WorkGroup": SPARK_WORK_GROUP, "Name": "accepted" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup must take the identical create, got {status} with {created:?}"
    );
}

/// Rule 2: every operation that addresses one notebook by id refuses an id no import or
/// create minted with the `NOTEBOOK_NOT_FOUND` class under both names, naming the id asked
/// for — and the notebook that IS held still answers.
#[tokio::test]
async fn a_notebook_id_that_names_no_notebook_is_refused_with_the_not_found_class() {
    let router = router();
    given_spark_work_group(&router, SPARK_WORK_GROUP).await;
    let held = given_notebook(&router, SPARK_WORK_GROUP, "held.ipynb").await;

    // Rows differ along the shape of the id that names no notebook: every nibble zero,
    // every nibble f, and one that differs from the held id in its first character — each
    // within the modelled `NotebookId`
    // (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
    let flipped = format!(
        "{}{}",
        if held.starts_with('a') { "b" } else { "a" },
        &held[1..]
    );
    let absent = [
        ("every nibble zero", "00000000-0000-4000-8000-000000000000".to_string()),
        ("every nibble f", "ffffffff-ffff-4fff-8fff-ffffffffffff".to_string()),
        ("one character from a held id", flipped),
    ];

    for (shape, id) in &absent {
        // (the operation, the request addressing that id) — every implemented operation
        // that resolves ONE `NotebookId`.
        let requests: Vec<(&str, Value)> = vec![
            ("ExportNotebook", json!({ "NotebookId": id })),
            ("ListNotebookSessions", json!({ "NotebookId": id })),
            (
                "StartSession",
                json!({
                    "WorkGroup": SPARK_WORK_GROUP,
                    "EngineConfiguration": {
                        "AdditionalConfigs": { NOTEBOOK_ID_CONFIG: id },
                    },
                }),
            ),
        ];
        for (operation, request) in requests {
            let case = format!("{operation} of an id that is {shape}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_refusal(
                &case,
                status,
                &refused,
                ERROR_CODE_NOTEBOOK_NOT_FOUND,
                &notebook_not_found(id),
            );
            assert!(
                message(&refused).contains(id.as_str()),
                "case {case:?}: the refusal must name the id asked for, got {:?}",
                message(&refused)
            );
        }
    }

    // The held notebook still answers both readers: the refusals above changed nothing.
    let (status, exported) = call(&router, "ExportNotebook", &json!({ "NotebookId": held })).await;
    assert_eq!(
        status, 200,
        "the held notebook must still export, got {status} with {exported:?}"
    );
    assert_eq!(
        exported["Payload"].as_str(),
        Some("{}"),
        "the held notebook's content must be unchanged, got {exported:?}"
    );
    let (status, listed) = call(
        &router,
        "ListNotebookSessions",
        &json!({ "NotebookId": held }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the held notebook's session listing must answer, got {status} with {listed:?}"
    );
}
