// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a data-catalog MUTATION reports, driven over the wire
//! (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the same router the
//! emulator boots, so every assertion is about what an SDK client observes.
//!
//! The rule under test: a mutation of a data catalog reports no account of the resource it
//! mutated — its response document carries no member at all, the optional
//! `DataCatalog` of `CreateDataCatalogOutput` and `DeleteDataCatalogOutput` included — so
//! the catalog, its `Status` among the rest, is only ever what a READ
//! (`GetDataCatalog`, `ListDataCatalogs`) answers with. A mutation that MINTS an
//! identifier its caller has no other way to learn still reports that identifier: the rule
//! is about echoing back the mutated resource, not about every mutation being silent.
//!
//! Each test asserts the rule over a TABLE of inputs rather than one recorded request: the
//! rows differ along the catalog type, the parameter set that type requires, the name's
//! length and character set, whether a description and tags are carried, and which form of
//! the delete removes the catalog.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_STATUS_CREATE_COMPLETE, DATA_CATALOG_STATUS_CREATE_IN_PROGRESS,
    DATA_CATALOG_TYPE_FEDERATED, DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE,
    DATA_CATALOG_TYPE_LAMBDA, MAX_DATA_CATALOG_NAME_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, router};

/// A valid catalog name of `length` characters, all of them inside the documented
/// `CreateDataCatalogInput$Name` set (alphanumeric, underscore, at sign, hyphen).
fn name_of_length(length: usize) -> String {
    "aA0_@-".chars().cycle().take(length).collect()
}

/// A Lambda function ARN a catalog parameter names, built from `role` so the rows can be
/// told apart.
fn lambda_arn(role: &str) -> String {
    format!("arn:aws:lambda:us-east-1:123456789012:function:{role}")
}

/// One row: a catalog a create registers, and whether the delete that removes it carries
/// `DeleteCatalogOnly` — which `DeleteDataCatalogInput$DeleteCatalogOnly` documents as
/// usable only with a FEDERATED catalog.
struct MutationCase {
    case: &'static str,
    name: String,
    create: Value,
    delete_catalog_only: bool,
}

/// The rows. Every `DataCatalogType` the model declares is represented, each with a
/// parameter set its type documents; the names span one character to the longest a catalog
/// name may use; a description and a tag list are present on some rows and absent on
/// others; and the federated row is removed by the other form of the delete.
fn mutation_cases() -> Vec<MutationCase> {
    let shortest = name_of_length(1);
    let longest = name_of_length(MAX_DATA_CATALOG_NAME_CHARS);
    vec![
        MutationCase {
            case: "a GLUE catalog in another account, named with one character and \
                   carrying neither a description nor tags",
            name: shortest.clone(),
            create: json!({
                "Name": shortest,
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "111122223333" }
            }),
            delete_catalog_only: false,
        },
        MutationCase {
            case: "a HIVE catalog with a description and an optional sdk-version",
            name: "hive_metastore@eu-1".to_string(),
            create: json!({
                "Name": "hive_metastore@eu-1",
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Description": "an external Apache Hive metastore",
                "Parameters": {
                    "metadata-function": lambda_arn("hms"),
                    "sdk-version": "1.0"
                }
            }),
            delete_catalog_only: false,
        },
        MutationCase {
            case: "a LAMBDA catalog with one composite function, tags and the longest \
                   name allowed",
            name: longest.clone(),
            create: json!({
                "Name": longest,
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": { "function": lambda_arn("both") },
                "Tags": [{ "Key": "env", "Value": "prod" }]
            }),
            delete_catalog_only: false,
        },
        MutationCase {
            case: "a LAMBDA catalog with split metadata and record functions",
            name: "lambda-split".to_string(),
            create: json!({
                "Name": "lambda-split",
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "metadata-function": lambda_arn("meta"),
                    "record-function": lambda_arn("rec")
                }
            }),
            delete_catalog_only: false,
        },
        MutationCase {
            case: "a FEDERATED catalog reusing an existing Glue connection, removed with \
                   DeleteCatalogOnly",
            name: "federated_reused".to_string(),
            create: json!({
                "Name": "federated_reused",
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": {
                    "connection-arn": "arn:aws:glue:us-east-1:111122223333:connection/snowflake"
                }
            }),
            delete_catalog_only: true,
        },
        MutationCase {
            case: "a FEDERATED catalog describing a new connection",
            name: "federated_new".to_string(),
            create: json!({
                "Name": "federated_new",
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": {
                    "connection-type": "MYSQL",
                    "connection-properties": "{\"spill_bucket\":\"my_spill\"}"
                }
            }),
            delete_catalog_only: false,
        },
    ]
}

/// Assert that `body` is a document with no member at all, naming `case` and the operation
/// that answered it.
fn assert_no_member(case: &str, operation: &str, body: &Value) {
    assert_eq!(
        body,
        &json!({}),
        "case {case:?}: expected {operation} to report no member, got {body:?}"
    );
    let members = body
        .as_object()
        .map(|document| document.len())
        .unwrap_or_else(|| panic!("case {case:?}: expected {operation} to answer a JSON object, got {body:?}"));
    assert_eq!(
        members, 0,
        "case {case:?}: expected {operation} to answer a document with no member, got \
         {body:?}"
    );
    assert_eq!(
        body["DataCatalog"],
        Value::Null,
        "case {case:?}: expected no DataCatalog on the {operation} response, got {:?}",
        body["DataCatalog"]
    );
}

/// `GetDataCatalog`'s view of one catalog: `(status, DataCatalog)`.
async fn read_catalog(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status, body["DataCatalog"].clone())
}

/// A create commits the catalog and reports nothing about it: the response carries no
/// member, and the registered resource — including the status its provisioning is in — is
/// the read's to report.
#[tokio::test]
async fn a_create_reports_no_member_and_the_read_reports_the_catalog() {
    let router = router();

    for case in mutation_cases() {
        let (status, created) = call(&router, "CreateDataCatalog", &case.create).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreateDataCatalog to answer 200, got {status} with \
             {created:?}",
            case.case
        );
        assert_no_member(case.case, "CreateDataCatalog", &created);

        // What the create committed is what the read answers with, member for member.
        let (status, read) = read_catalog(&router, &case.name).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetDataCatalog to read the created catalog back, got \
             {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Name"].as_str(),
            Some(case.name.as_str()),
            "case {:?}: expected the read to report the catalog's own name, got {read:?}",
            case.case
        );
        assert_eq!(
            read["Type"], case.create["Type"],
            "case {:?}: expected the read to report Type {:?}, got {:?}",
            case.case, case.create["Type"], read["Type"]
        );
        assert_eq!(
            read["Description"],
            case.create
                .get("Description")
                .cloned()
                .unwrap_or(Value::Null),
            "case {:?}: expected the read to report the created description, got {read:?}",
            case.case
        );
        // The status is the read's account of the catalog, and the create reported none of
        // it: a synchronously created type is complete, and the one documented as created
        // asynchronously is still in progress or already complete, never anything else.
        let status_reported = read["Status"].as_str();
        let synchronous = case.create["Type"] != json!(DATA_CATALOG_TYPE_FEDERATED);
        let allowed: &[&str] = if synchronous {
            &[DATA_CATALOG_STATUS_CREATE_COMPLETE]
        } else {
            &[
                DATA_CATALOG_STATUS_CREATE_IN_PROGRESS,
                DATA_CATALOG_STATUS_CREATE_COMPLETE,
            ]
        };
        assert!(
            status_reported.is_some_and(|reported| allowed.contains(&reported)),
            "case {:?}: expected the read to report one of {allowed:?}, got {read:?}",
            case.case
        );
    }
}

/// A delete commits the removal and reports nothing about what it removed: the response
/// carries no member, and that the catalog is gone is what the registry no longer
/// resolving its name says.
#[tokio::test]
async fn a_delete_reports_no_member_and_the_name_stops_resolving() {
    let router = router();

    // One registry holding every row at once, so each delete below also shows that the
    // other catalogs survive it.
    let cases = mutation_cases();
    for case in &cases {
        let (status, created) = call(&router, "CreateDataCatalog", &case.create).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected CreateDataCatalog to answer 200, got {status} with \
             {created:?}",
            case.case
        );
    }

    for (position, case) in cases.iter().enumerate() {
        let request = if case.delete_catalog_only {
            json!({ "Name": case.name, "DeleteCatalogOnly": true })
        } else {
            json!({ "Name": case.name })
        };
        let (status, removed) = call(&router, "DeleteDataCatalog", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected DeleteDataCatalog to answer 200, got {status} with \
             {removed:?}",
            case.case
        );
        assert_no_member(case.case, "DeleteDataCatalog", &removed);

        // The removal is real: the name resolves for nothing afterwards.
        let (read_status, read) = read_catalog(&router, &case.name).await;
        assert_eq!(
            read_status, 400,
            "case {:?}: expected the deleted catalog's name to stop resolving, got \
             {read_status} with {read:?}",
            case.case
        );

        // Every row not yet deleted is still exactly where it was.
        for later in cases.iter().skip(position + 1) {
            let (status, read) = read_catalog(&router, &later.name).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected {:?} to survive the delete of {:?}, got {status} with \
                 {read:?}",
                case.case, later.name, case.name
            );
        }
    }
}

/// The rule is about echoing the mutated resource, not about silence: a mutation whose
/// output models an identifier the service MINTS still reports it, because a caller has no
/// other way to learn it.
#[tokio::test]
async fn a_mutation_that_mints_an_identifier_still_reports_it() {
    let router = router();

    // The rows differ along the resource minted and the member its create models:
    // `CreateNamedQueryOutput$NamedQueryId` and `CreateNotebookOutput$NotebookId`.
    let spark_work_group = "minted-identifiers-spark";
    let (status, created) = call(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": spark_work_group,
            "Configuration": {
                "EngineVersion": { "SelectedEngineVersion": "PySpark engine version 3" },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the Spark workgroup the notebook row lives in must be created, got {status} with \
         {created:?}"
    );

    let cases: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a named query",
            "CreateNamedQuery",
            json!({
                "Name": "minted_query",
                "Database": "logs",
                "QueryString": "SELECT 1"
            }),
            "NamedQueryId",
        ),
        (
            "a notebook",
            "CreateNotebook",
            json!({ "WorkGroup": spark_work_group, "Name": "minted_notebook" }),
            "NotebookId",
        ),
    ];

    for (case, operation, request, member) in cases {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} with {body:?}"
        );
        let minted = body[member].as_str();
        assert!(
            minted.is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected {operation} to report the {member} it minted, got \
             {body:?}"
        );
    }
}
