// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what every data-catalog path answers when the `Name` it is handed
//! names no catalog this account holds.
//!
//! The rule: such a request is refused with the operation's own MODELLED client error —
//! `InvalidRequestException`, HTTP 400, under this service's `INVALID_INPUT` refusal class
//! — carrying that path's own sentence with the catalog name taken from the request, never
//! with an error shape the operation does not model. Each path words the miss as Athena
//! words it, and the wordings are not interchangeable: a read states the name "was not
//! found.", a delete the same sentence without the closing period, and an update that the
//! catalog "does not exist."
//!
//! Asserted over a TABLE: the rows span the paths that address a catalog by name and the
//! forms a catalog name takes (one character, the documented punctuation — underscore, at
//! sign and hyphen — mixed case with digits, and the longest name the documented bound
//! allows), because the sentence is built from the request rather than from any one
//! recorded name. Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE, DATA_CATALOG_TYPE_LAMBDA,
    ERROR_CODE_INVALID_INPUT, MAX_DATA_CATALOG_NAME_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, message, router as build_router};

/// A Lambda function ARN a catalog parameter names, built from `role` so the rows can be
/// told apart. As long as the ARNs real catalog parameters carry, so a row proves nothing
/// about a short value only.
fn lambda_arn(role: &str) -> String {
    format!("arn:aws:lambda:us-east-1:123456789012:function:athena-federation-{role}-handler")
}

/// One path that addresses a data catalog by name: the operation, the body it is handed
/// for a catalog name, and the sentence a miss must be refused with.
struct MissPath {
    case: &'static str,
    operation: &'static str,
    body: fn(&str) -> Value,
    expected: fn(&str) -> String,
}

/// The paths, each with the wording Athena gives that path's miss. The `Name`/`CatalogName`
/// member is the only variable part of each sentence.
fn miss_paths() -> Vec<MissPath> {
    vec![
        MissPath {
            case: "GetDataCatalog, which reads",
            operation: "GetDataCatalog",
            body: |name| json!({ "Name": name }),
            expected: |name| format!("DataCatalog {name} was not found."),
        },
        MissPath {
            case: "DeleteDataCatalog, which removes",
            operation: "DeleteDataCatalog",
            body: |name| json!({ "Name": name }),
            expected: |name| format!("DataCatalog {name} was not found"),
        },
        MissPath {
            case: "UpdateDataCatalog, which writes",
            operation: "UpdateDataCatalog",
            body: |name| {
                json!({
                    "Name": name,
                    "Type": DATA_CATALOG_TYPE_HIVE,
                    "Parameters": {
                        "metadata-function": lambda_arn("hive"),
                        "sdk-version": "1.0",
                    },
                })
            },
            expected: |name| format!("DataCatalog {name} does not exist."),
        },
        MissPath {
            case: "GetTableMetadata, which reads through the catalog",
            operation: "GetTableMetadata",
            body: |name| {
                json!({
                    "CatalogName": name,
                    "DatabaseName": "some_database",
                    "TableName": "some_table",
                })
            },
            expected: |name| format!("DataCatalog {name} was not found."),
        },
    ]
}

/// The catalog names each path is asked about: none of them is registered, and they differ
/// in length and in which of the documented characters they use — "alphanumeric,
/// underscore, at sign, or hyphen characters", at most
/// [`MAX_DATA_CATALOG_NAME_CHARS`] of them (`UpdateDataCatalogInput$Name`).
fn unheld_names() -> Vec<String> {
    vec![
        "c".to_string(),
        "catalog-with-hyphens".to_string(),
        "catalog_with_underscores".to_string(),
        "catalog@with@at@signs".to_string(),
        "Catalog9MixedCase".to_string(),
        // The longest name the documented bound allows, which is still a miss.
        "l".repeat(MAX_DATA_CATALOG_NAME_CHARS),
    ]
}

#[tokio::test]
async fn every_data_catalog_path_refuses_an_unheld_name_with_its_own_modelled_wording() {
    let router = build_router();

    for name in unheld_names() {
        for path in miss_paths() {
            let body = (path.body)(&name);
            let (status, answer) = call(&router, path.operation, &body).await;
            let case = format!("{} on {name:?}", path.case);
            assert_refusal(
                &case,
                status,
                &answer,
                ERROR_CODE_INVALID_INPUT,
                &(path.expected)(&name),
            );
        }
    }
}

#[tokio::test]
async fn the_update_miss_is_not_worded_as_the_read_or_the_delete_miss() {
    let router = build_router();

    // The same unheld name through the three paths: each sentence is the one its own path
    // states, so none of the three can stand in for another.
    for name in unheld_names() {
        let mut said: Vec<(&str, String)> = Vec::new();
        for path in miss_paths() {
            let (status, answer) = call(&router, path.operation, &((path.body)(&name))).await;
            assert_eq!(
                status, 400,
                "case {:?} on {name:?}: expected HTTP 400, got {status} with {answer:?}",
                path.case
            );
            said.push((path.operation, message(&answer)));
        }
        let update = said
            .iter()
            .find(|(operation, _)| *operation == "UpdateDataCatalog")
            .map(|(_, said)| said.clone())
            .expect("the table drives UpdateDataCatalog");
        for (operation, other) in said
            .iter()
            .filter(|(operation, _)| *operation != "UpdateDataCatalog")
        {
            assert_ne!(
                &update, other,
                "case {operation:?} on {name:?}: the update's wording must differ from this \
                 path's, both said {update:?}"
            );
        }
    }
}

/// One lifecycle row: the catalog type and the parameter maps a create and an update carry
/// for it.
struct LifecycleCase {
    case: &'static str,
    catalog_type: &'static str,
    created: Value,
    updated: Value,
}

/// The rows: every synchronously created `DataCatalogType` whose parameters the
/// documentation states, each with a create map and a different update map. The
/// asynchronously created `FEDERATED` type is left to `data_catalog_update.rs`, which owns
/// what its creation span does to an update.
fn lifecycle_cases() -> Vec<LifecycleCase> {
    vec![
        LifecycleCase {
            case: "a HIVE catalog whose metadata function the update repoints",
            catalog_type: DATA_CATALOG_TYPE_HIVE,
            created: json!({
                "metadata-function": lambda_arn("hive-created"),
                "sdk-version": "1.0",
            }),
            updated: json!({
                "metadata-function": lambda_arn("hive-updated"),
                "sdk-version": "1.0",
            }),
        },
        LifecycleCase {
            case: "a LAMBDA catalog the update moves to the split parameter set",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            created: json!({ "function": lambda_arn("lambda-composite") }),
            updated: json!({
                "metadata-function": lambda_arn("lambda-metadata"),
                "record-function": lambda_arn("lambda-record"),
            }),
        },
        LifecycleCase {
            case: "a GLUE catalog whose catalog id the update replaces",
            catalog_type: DATA_CATALOG_TYPE_GLUE,
            created: json!({ "catalog-id": "123456789012" }),
            updated: json!({ "catalog-id": "210987654321" }),
        },
    ]
}

/// `GetDataCatalog`'s view of one catalog: `(status, DataCatalog)`.
async fn read_catalog(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status, body["DataCatalog"].clone())
}

/// The catalog names `ListDataCatalogs` reports.
async fn listed_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, "ListDataCatalogs", &json!({ "MaxResults": 50 })).await;
    assert_eq!(
        status, 200,
        "ListDataCatalogs must answer 200, got {status} with {body:?}"
    );
    body["DataCatalogsSummary"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|summary| summary["CatalogName"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn the_catalog_lifecycle_runs_past_the_update_step() {
    let router = build_router();

    for case in lifecycle_cases() {
        let name = format!("lifecycle-{}", case.catalog_type.to_lowercase());

        let (status, created) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Description": "created by the lifecycle row",
                "Parameters": case.created,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must be created, got {status} with {created:?}",
            case.case
        );

        // The created map round-trips unaltered, key for key and value for value.
        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the created catalog must be readable, got {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Parameters"], case.created,
            "case {:?}: expected the created parameters {:?}, got {:?}",
            case.case, case.created, read["Parameters"]
        );

        // The update is SERVED, and what it wrote is what the next read reports.
        let (status, updated) = call(
            &router,
            "UpdateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Parameters": case.updated,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the update must be served, got {status} with {updated:?}",
            case.case
        );
        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the updated catalog must be readable, got {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Parameters"], case.updated,
            "case {:?}: expected the updated parameters {:?}, got {:?}",
            case.case, case.updated, read["Parameters"]
        );
        assert_eq!(
            read["Description"].as_str(),
            Some("created by the lifecycle row"),
            "case {:?}: an update naming no description must change none, got {read:?}",
            case.case
        );

        // Once the catalog is gone, the read and the update each refuse it with their own
        // sentence, and the update writes nothing: the catalog does not come back.
        let (status, removed) = call(&router, "DeleteDataCatalog", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must be deleted, got {status} with {removed:?}",
            case.case
        );

        let (status, answer) = call(&router, "GetDataCatalog", &json!({ "Name": name })).await;
        assert_refusal(
            &format!("{}: the read of the deleted catalog", case.case),
            status,
            &answer,
            ERROR_CODE_INVALID_INPUT,
            &format!("DataCatalog {name} was not found."),
        );

        let (status, answer) = call(
            &router,
            "UpdateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Parameters": case.updated,
            }),
        )
        .await;
        assert_refusal(
            &format!("{}: the update of the deleted catalog", case.case),
            status,
            &answer,
            ERROR_CODE_INVALID_INPUT,
            &format!("DataCatalog {name} does not exist."),
        );

        let names = listed_names(&router).await;
        assert!(
            !names.contains(&name),
            "case {:?}: the refused update must have registered nothing, the listing reports \
             {names:?}",
            case.case
        );

        // The name is free again, which is what shows the refused update left no record
        // behind: a create of it is accepted rather than refused as already registered.
        let (status, recreated) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Parameters": case.created,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the name must be free after the refused update, got {status} with \
             {recreated:?}",
            case.case
        );
    }
}
