// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateDataCatalog`: what one applies to the registered catalog
//! is what the next `GetDataCatalog` reports, and a name the registry does not hold is
//! refused without anything being written. Driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots,
//! so every assertion is about what an SDK client observes.
//!
//! Each test states a rule and asserts it over a TABLE of inputs rather than one recorded
//! request: the rows differ along which member the update names, the catalog type it
//! states, which keys its `Parameters` map carries (more, fewer, the same keys with other
//! values, the other required set of the same type) and how long the description is.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_STATUS_CREATE_COMPLETE, DATA_CATALOG_TYPE_FEDERATED, DATA_CATALOG_TYPE_GLUE,
    DATA_CATALOG_TYPE_HIVE, DATA_CATALOG_TYPE_LAMBDA, GLUE_DATA_CATALOG_NAME,
};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, error_code, message, router as build_router};

/// The wire error code a refused request must carry: the client error the operation
/// models (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// A Lambda function ARN a catalog parameter names, built from `role` so the rows can be
/// told apart.
fn lambda_arn(role: &str) -> String {
    format!("arn:aws:lambda:us-east-1:123456789012:function:{role}")
}

/// What a row writes where it means "the name this row registered the catalog under",
/// which it cannot write out because the name is built from the row's position. Replaced
/// by [`with_catalog_name`] before the row is asserted.
const CATALOG_NAME_PLACEHOLDER: &str = "<catalog-name>";

/// `parameters` with every [`CATALOG_NAME_PLACEHOLDER`] value replaced by `name`.
fn with_catalog_name(parameters: &Value, name: &str) -> Value {
    let mut filled = parameters.clone();
    if let Some(map) = filled.as_object_mut() {
        for value in map.values_mut() {
            if value.as_str() == Some(CATALOG_NAME_PLACEHOLDER) {
                *value = json!(name);
            }
        }
    }
    filled
}

/// `GetDataCatalog`'s view of one catalog: `(status, DataCatalog)`.
async fn read_catalog(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status, body["DataCatalog"].clone())
}

/// One row of the update table: the catalog to register, the update to send, and the
/// `DataCatalog` a read must report afterwards.
struct UpdateCase {
    case: &'static str,
    /// The `CreateDataCatalog` body the row starts from, minus the name.
    create: Value,
    /// The `UpdateDataCatalog` body the row sends, minus the name.
    update: Value,
    /// The `Type` a read must report once the update is applied.
    catalog_type: &'static str,
    /// The `Description` a read must report, or `None` when it must report none.
    description: Option<&'static str>,
    /// The whole `Parameters` map a read must report, where a
    /// [`CATALOG_NAME_PLACEHOLDER`] value stands for the catalog's own name.
    parameters: Value,
    /// The `ConnectionType` a read must report, which follows from the parameters the
    /// update left the catalog with rather than from anything stored before it.
    connection_type: Option<&'static str>,
}

/// The rows: each names a different combination of the three members an update may carry
/// — a description alone, a parameters map alone, both at once — over catalogs of every
/// `DataCatalogType` the model declares, and each parameters map differs from the created
/// one in a different way (a key added, a key dropped, a value replaced, the other
/// required set of the same type).
fn update_cases() -> Vec<UpdateCase> {
    vec![
        UpdateCase {
            case: "the description alone, on a HIVE catalog",
            create: json!({
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Description": "the created description",
                "Parameters": { "metadata-function": lambda_arn("hive-metadata") },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Description": "the updated description",
            }),
            catalog_type: DATA_CATALOG_TYPE_HIVE,
            description: Some("the updated description"),
            // The request named no `Parameters`, so the created map still stands.
            parameters: json!({ "metadata-function": lambda_arn("hive-metadata") }),
            connection_type: None,
        },
        UpdateCase {
            case: "one more parameter key, with no description",
            create: json!({
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Description": "the created description",
                "Parameters": { "metadata-function": lambda_arn("hive-metadata") },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Parameters": {
                    "metadata-function": lambda_arn("hive-metadata"),
                    "sdk-version": "1.0",
                },
            }),
            catalog_type: DATA_CATALOG_TYPE_HIVE,
            // The request named no `Description`, so the created one still stands.
            description: Some("the created description"),
            parameters: json!({
                "metadata-function": lambda_arn("hive-metadata"),
                "sdk-version": "1.0",
            }),
            connection_type: None,
        },
        UpdateCase {
            case: "a description and a parameters map at once, on a LAMBDA catalog",
            create: json!({
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Description": "the created description",
                "Parameters": { "function": lambda_arn("composite") },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Description": "the updated description",
                "Parameters": {
                    "catalog": "analytics",
                    "metadata-function": lambda_arn("metadata"),
                    "record-function": lambda_arn("records"),
                },
            }),
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            description: Some("the updated description"),
            // The map the update carried REPLACES the stored one, so the composite
            // `function` key the create named is gone.
            parameters: json!({
                "catalog": "analytics",
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
            }),
            connection_type: None,
        },
        UpdateCase {
            case: "the other required parameter set of the same type",
            create: json!({
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "metadata-function": lambda_arn("metadata"),
                    "record-function": lambda_arn("records"),
                },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": { "function": lambda_arn("composite") },
            }),
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            description: None,
            // One composite function is the wiring of both roles, beside the name of the
            // catalog it is asked about — the name this row registered the catalog under.
            parameters: json!({
                "catalog": CATALOG_NAME_PLACEHOLDER,
                "metadata-function": lambda_arn("composite"),
                "record-function": lambda_arn("composite"),
            }),
            connection_type: None,
        },
        UpdateCase {
            case: "one parameter value replaced, on a GLUE catalog",
            create: json!({
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "123456789012" },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Description": "the catalog of another account",
                "Parameters": { "catalog-id": "210987654321" },
            }),
            catalog_type: DATA_CATALOG_TYPE_GLUE,
            description: Some("the catalog of another account"),
            parameters: json!({ "catalog-id": "210987654321" }),
            connection_type: None,
        },
        UpdateCase {
            case: "a FEDERATED catalog moved from a connection type to a connection ARN",
            create: json!({
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": { "connection-type": "MYSQL" },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": {
                    "connection-arn":
                        "arn:aws:glue:us-east-1:123456789012:connection/federated",
                },
            }),
            catalog_type: DATA_CATALOG_TYPE_FEDERATED,
            description: None,
            parameters: json!({
                "connection-arn": "arn:aws:glue:us-east-1:123456789012:connection/federated",
            }),
            // The created catalog reported MYSQL; the map the update carried names a
            // connection rather than a type, so the catalog now reports none.
            connection_type: None,
        },
        UpdateCase {
            case: "a FEDERATED catalog moved onto another connection type",
            create: json!({
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": { "connection-type": "MYSQL" },
            }),
            update: json!({
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Description": "a Redshift connection",
                "Parameters": { "connection-type": "REDSHIFT" },
            }),
            catalog_type: DATA_CATALOG_TYPE_FEDERATED,
            description: Some("a Redshift connection"),
            parameters: json!({ "connection-type": "REDSHIFT" }),
            connection_type: Some("REDSHIFT"),
        },
    ]
}

#[tokio::test]
async fn an_applied_update_is_what_get_data_catalog_reports() {
    let router = build_router();

    for (index, case) in update_cases().into_iter().enumerate() {
        let name = format!("update-case-{index}");

        let mut create = case.create.clone();
        create["Name"] = json!(name);
        let (status, created) = call(&router, "CreateDataCatalog", &create).await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must be created, got {status} with {created:?}",
            case.case
        );

        let mut update = case.update.clone();
        update["Name"] = json!(name);
        let (status, answered) = call(&router, "UpdateDataCatalog", &update).await;
        assert_eq!(
            status, 200,
            "case {:?}: UpdateDataCatalog must answer 200, got {status} with {answered:?}",
            case.case
        );
        assert_eq!(
            answered,
            json!({}),
            "case {:?}: UpdateDataCatalogOutput models no members, got {answered:?}",
            case.case
        );

        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetDataCatalog must answer 200, got {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Type"],
            json!(case.catalog_type),
            "case {:?}: expected Type {:?}, got {:?}",
            case.case,
            case.catalog_type,
            read["Type"]
        );
        assert_eq!(
            read["Description"],
            case.description.map_or(Value::Null, |text| json!(text)),
            "case {:?}: expected Description {:?}, got {:?}",
            case.case,
            case.description,
            read["Description"]
        );
        let expected_parameters = with_catalog_name(&case.parameters, &name);
        assert_eq!(
            read["Parameters"], expected_parameters,
            "case {:?}: expected Parameters {expected_parameters:?}, got {:?}",
            case.case, read["Parameters"]
        );
        assert_eq!(
            read["ConnectionType"],
            case.connection_type.map_or(Value::Null, |text| json!(text)),
            "case {:?}: expected ConnectionType {:?}, got {:?}",
            case.case,
            case.connection_type,
            read["ConnectionType"]
        );
        // An update does not change the name the catalog is addressed by.
        assert_eq!(
            read["Name"],
            json!(name),
            "case {:?}: expected the catalog to still be named {name}, got {:?}",
            case.case,
            read["Name"]
        );
    }
}

#[tokio::test]
async fn a_second_update_builds_on_the_first() {
    let router = build_router();
    let name = "update-twice";

    let (status, created) = call(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Description": "first",
            "Parameters": { "metadata-function": lambda_arn("first") },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the catalog must be created, got {status} with {created:?}"
    );

    // Each update names ONE member, so the second reads the state the first left: the
    // description is the second update's and the parameters are the first's.
    for (step, update) in [
        json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Parameters": { "metadata-function": lambda_arn("second") },
        }),
        json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Description": "second",
        }),
    ]
    .into_iter()
    .enumerate()
    {
        let (status, answered) = call(&router, "UpdateDataCatalog", &update).await;
        assert_eq!(
            status, 200,
            "update {step} must answer 200, got {status} with {answered:?}"
        );
    }

    let (status, read) = read_catalog(&router, name).await;
    assert_eq!(
        status, 200,
        "GetDataCatalog must answer 200, got {status} with {read:?}"
    );
    assert_eq!(
        read["Description"],
        json!("second"),
        "expected the second update's description, got {:?}",
        read["Description"]
    );
    assert_eq!(
        read["Parameters"],
        json!({ "metadata-function": lambda_arn("second") }),
        "expected the first update's parameters to stand, got {:?}",
        read["Parameters"]
    );
}

/// One row of the refusal table: the body to send and the message it must be refused
/// with.
struct RefusalCase {
    case: &'static str,
    update: Value,
    expected: String,
}

#[tokio::test]
async fn an_update_naming_no_registered_catalog_is_refused_and_writes_nothing() {
    let router = build_router();

    // A catalog that exists, created and then deleted, and one that never existed: both
    // are names the registry does not hold when the update arrives.
    let deleted = "update-deleted-catalog";
    let (status, created) = call(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": deleted,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Parameters": { "metadata-function": lambda_arn("deleted") },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the catalog must be created, got {status} with {created:?}"
    );
    let (status, removed) = call(&router, "DeleteDataCatalog", &json!({ "Name": deleted })).await;
    assert_eq!(
        status, 200,
        "the catalog must be deleted, got {status} with {removed:?}"
    );

    let cases = vec![
        RefusalCase {
            case: "a catalog that was deleted",
            update: json!({
                "Name": deleted,
                "Type": DATA_CATALOG_TYPE_HIVE,
                "Parameters": { "metadata-function": lambda_arn("deleted") },
            }),
            expected: format!("DataCatalog {deleted} does not exist."),
        },
        RefusalCase {
            case: "a catalog no create ever registered",
            update: json!({
                "Name": "never-registered",
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "123456789012" },
            }),
            expected: "DataCatalog never-registered does not exist.".to_string(),
        },
        RefusalCase {
            case: "the Glue Data Catalog, which no create registered either",
            update: json!({
                "Name": "AwsDataCatalog",
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "catalog-id": "123456789012" },
            }),
            expected: "DataCatalog AwsDataCatalog does not exist.".to_string(),
        },
    ];

    for case in cases {
        let (status, body) = call(&router, "UpdateDataCatalog", &case.update).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            case.case
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {:?}: expected {INVALID_REQUEST}, got {:?}",
            case.case,
            body
        );
        assert_eq!(
            message(&body),
            case.expected,
            "case {:?}: expected the message {:?}, got {:?}",
            case.case,
            case.expected,
            message(&body)
        );

        // Nothing was written: the deleted catalog is still gone, and the name that was
        // never registered still resolves to nothing.
        let read_name = case.update["Name"].as_str().expect("the row names a catalog");
        let (status, read) = read_catalog(&router, read_name).await;
        if read_name == GLUE_DATA_CATALOG_NAME {
            // The account holds this one catalog without any create having registered it, so
            // the registry resolves its name to nothing: the read is refused like every other
            // name no create registered. What shows the refused update wrote nothing is the
            // LISTING still reporting the catalog exactly as it was — under its own type.
            assert_eq!(
                status, 400,
                "case {:?}: the implicit Glue Data Catalog is not resolvable by name, got \
                 {status} with {read:?}",
                case.case
            );
            let (listed_status, listed) = call(&router, "ListDataCatalogs", &json!({})).await;
            assert_eq!(
                listed_status, 200,
                "case {:?}: the listing must answer 200, got {listed_status} with {listed:?}",
                case.case
            );
            let summary = listed["DataCatalogsSummary"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .find(|summary| summary["CatalogName"] == json!(GLUE_DATA_CATALOG_NAME))
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the account's own Glue Data Catalog must still be listed, \
                         got {listed:?}",
                        case.case
                    )
                });
            assert_eq!(
                summary["Type"].as_str(),
                Some(DATA_CATALOG_TYPE_GLUE),
                "case {:?}: the refused update must have changed no type, got {summary:?}",
                case.case
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {:?}: the refused update must have registered nothing, got {status} \
             with {read:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn an_update_whose_parameters_do_not_describe_the_stated_type_is_refused() {
    let router = build_router();
    let name = "update-parameter-rules";

    let (status, created) = call(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": name,
            "Type": DATA_CATALOG_TYPE_LAMBDA,
            "Description": "the created description",
            "Parameters": { "function": lambda_arn("composite") },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the catalog must be created, got {status} with {created:?}"
    );

    // The same rules a create is held to, each from the `DataCatalog$Parameters`
    // documentation: the LAMBDA type takes one of its two required sets but not both,
    // the GLUE type requires its catalog id, and a FEDERATED catalog names one
    // connection, not two.
    let cases = vec![
        RefusalCase {
            case: "both LAMBDA parameter sets at once",
            update: json!({
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": {
                    "function": lambda_arn("composite"),
                    "metadata-function": lambda_arn("metadata"),
                    "record-function": lambda_arn("records"),
                },
            }),
            expected: "Lambda catalogs require that either the metadata-function and \
                       record-function parameters or the function parameter be set, but \
                       not both."
                .to_string(),
        },
        RefusalCase {
            case: "a type whose required parameter the map does not carry",
            update: json!({
                "Type": DATA_CATALOG_TYPE_GLUE,
                "Parameters": { "function": lambda_arn("composite") },
            }),
            expected: "Parameters for a GLUE data catalog must contain catalog-id."
                .to_string(),
        },
        RefusalCase {
            case: "both FEDERATED connection parameters at once",
            update: json!({
                "Type": DATA_CATALOG_TYPE_FEDERATED,
                "Parameters": {
                    "connection-arn":
                        "arn:aws:glue:us-east-1:123456789012:connection/federated",
                    "connection-type": "MYSQL",
                },
            }),
            expected: "Parameters for a FEDERATED data catalog must contain either \
                       connection-arn or connection-type, but not both."
                .to_string(),
        },
    ];

    for case in cases {
        let mut update = case.update.clone();
        update["Name"] = json!(name);
        let (status, body) = call(&router, "UpdateDataCatalog", &update).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            case.case
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {:?}: expected {INVALID_REQUEST}, got {body:?}",
            case.case
        );
        assert_eq!(
            message(&body),
            case.expected,
            "case {:?}: expected the message {:?}, got {:?}",
            case.case,
            case.expected,
            message(&body)
        );

        // The refused update wrote nothing: the catalog still reads as it was created.
        let (status, read) = read_catalog(&router, name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must still be readable, got {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Description"],
            json!("the created description"),
            "case {:?}: expected the created description to stand, got {:?}",
            case.case,
            read["Description"]
        );
        assert_eq!(
            read["Parameters"],
            json!({
                "catalog": name,
                "metadata-function": lambda_arn("composite"),
                "record-function": lambda_arn("composite"),
            }),
            "case {:?}: expected the created wiring to stand, got {:?}",
            case.case,
            read["Parameters"]
        );
        assert_eq!(
            read["Type"],
            json!(DATA_CATALOG_TYPE_LAMBDA),
            "case {:?}: expected the created type to stand, got {:?}",
            case.case,
            read["Type"]
        );
    }
}

/// How long a poll below waits for a catalog's creation to be complete: generous, so a
/// loaded or slow host does not decide the outcome. The condition is what is waited for,
/// never a span.
const CREATION_DEADLINE: Duration = Duration::from_secs(30);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

#[tokio::test]
async fn a_type_change_reports_the_creation_the_new_type_describes() {
    let router = build_router();

    // FEDERATED -> HIVE. `DataCatalog$Status` documents the federated type as created
    // asynchronously and every other type as created synchronously, so a catalog this
    // update makes HIVE has a creation nothing is still owed for, and the read right
    // after reports it complete — rather than the pending completion the federated
    // create stored.
    let to_hive = "type-change-to-hive";
    let (status, created) = call(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": to_hive,
            "Type": DATA_CATALOG_TYPE_FEDERATED,
            "Parameters": { "connection-type": "MYSQL" },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the federated catalog must be created, got {status} with {created:?}"
    );
    let (status, updated) = call(
        &router,
        "UpdateDataCatalog",
        &json!({
            "Name": to_hive,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Parameters": { "metadata-function": lambda_arn("hive-metadata") },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the type change must be accepted, got {status} with {updated:?}"
    );
    let (status, read) = read_catalog(&router, to_hive).await;
    assert_eq!(
        status, 200,
        "GetDataCatalog must answer 200, got {status} with {read:?}"
    );
    assert_eq!(
        read["Status"],
        json!(DATA_CATALOG_STATUS_CREATE_COMPLETE),
        "case \"FEDERATED -> HIVE\": expected Status {DATA_CATALOG_STATUS_CREATE_COMPLETE}, \
         got {:?} in {read:?}",
        read["Status"]
    );
    assert_eq!(
        read["ConnectionType"],
        Value::Null,
        "case \"FEDERATED -> HIVE\": expected no ConnectionType, got {:?}",
        read["ConnectionType"]
    );

    // HIVE -> FEDERATED, the other direction: the catalog's creation is asynchronous from
    // this update on, and it reaches CREATE_COMPLETE on its own. Polled for, never slept
    // through: the loop looks again until the status holds or the deadline passes.
    let to_federated = "type-change-to-federated";
    let (status, created) = call(
        &router,
        "CreateDataCatalog",
        &json!({
            "Name": to_federated,
            "Type": DATA_CATALOG_TYPE_HIVE,
            "Parameters": { "metadata-function": lambda_arn("hive-metadata") },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the hive catalog must be created, got {status} with {created:?}"
    );
    let (status, updated) = call(
        &router,
        "UpdateDataCatalog",
        &json!({
            "Name": to_federated,
            "Type": DATA_CATALOG_TYPE_FEDERATED,
            "Parameters": { "connection-type": "REDSHIFT" },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the type change must be accepted, got {status} with {updated:?}"
    );

    let mut last = Value::Null;
    let mut complete = false;
    let deadline = tokio::time::Instant::now() + CREATION_DEADLINE;
    while tokio::time::Instant::now() < deadline {
        let (status, read) = read_catalog(&router, to_federated).await;
        assert_eq!(
            status, 200,
            "GetDataCatalog must answer 200 while polling, got {status} with {read:?}"
        );
        if read["Status"] == json!(DATA_CATALOG_STATUS_CREATE_COMPLETE) {
            complete = true;
            last = read;
            break;
        }
        last = read;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        complete,
        "case \"HIVE -> FEDERATED\": expected the catalog to reach \
         {DATA_CATALOG_STATUS_CREATE_COMPLETE}, last read {last:?}"
    );
    assert_eq!(
        last["ConnectionType"],
        json!("REDSHIFT"),
        "case \"HIVE -> FEDERATED\": expected the ConnectionType the update's parameters \
         name, got {:?}",
        last["ConnectionType"]
    );
}
