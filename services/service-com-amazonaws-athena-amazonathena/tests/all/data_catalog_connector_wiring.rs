// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the wiring a data catalog HOLDS, as against the shorthand a request
//! states it with: a LAMBDA catalog given one composite function holds that function under
//! both role keys plus the name of the catalog it is asked about, and every other map is
//! held as it was given. Driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots, so
//! every assertion is about what an SDK client observes.
//!
//! The rule is asserted over a TABLE rather than one recorded request: the rows differ in
//! the catalog type, which of the forms `DataCatalog$Parameters` documents the map states,
//! how many keys it carries, whether it carries keys of neither form, and whether it names
//! the catalog key itself — and each row is driven through BOTH operations that take a map,
//! so what a catalog holds does not depend on which path wrote it.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    DATA_CATALOG_TYPE_FEDERATED, DATA_CATALOG_TYPE_GLUE, DATA_CATALOG_TYPE_HIVE,
    DATA_CATALOG_TYPE_LAMBDA,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, router as build_router};

/// A Lambda function ARN a catalog parameter names, built from `role` so the rows can be
/// told apart.
fn lambda_arn(role: &str) -> String {
    format!("arn:aws:lambda:us-east-1:123456789012:function:{role}")
}

/// What a row writes where it means "the name this row registered the catalog under", which
/// it cannot write out because the name is built from the row's position. Replaced by
/// [`with_catalog_name`] before the row is asserted.
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

/// One row: a catalog type, the `Parameters` map a request states, and the wiring the
/// catalog must then hold.
struct WiringCase {
    case: &'static str,
    catalog_type: &'static str,
    /// The map a create or an update carries.
    submitted: Value,
    /// The map every read of the catalog must report, with
    /// [`CATALOG_NAME_PLACEHOLDER`] standing for the catalog's own name.
    held: Value,
}

/// The rows.
///
/// The forms a request may state come from the `DataCatalog$Parameters` documentation, one
/// per type: `metadata-function` (with the optional `sdk-version`) for HIVE,
/// `metadata-function` with `record-function` or the composite `function` for LAMBDA,
/// `catalog-id` for GLUE, and `connection-arn` or `connection-type` for FEDERATED. The
/// `catalog` key a composite LAMBDA catalog also holds is RECORDED BEHAVIOUR: the
/// documentation gives the request syntax and not what the catalog then holds.
fn wiring_cases() -> Vec<WiringCase> {
    vec![
        WiringCase {
            case: "a LAMBDA catalog given one composite function",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            submitted: json!({ "function": lambda_arn("composite") }),
            held: json!({
                "catalog": CATALOG_NAME_PLACEHOLDER,
                "metadata-function": lambda_arn("composite"),
                "record-function": lambda_arn("composite"),
            }),
        },
        WiringCase {
            case: "a LAMBDA catalog given a composite function beside keys of neither form",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            submitted: json!({
                "function": lambda_arn("one-for-both"),
                "sdk-version": "1.0",
                "spill-bucket": "spill",
            }),
            held: json!({
                "catalog": CATALOG_NAME_PLACEHOLDER,
                "metadata-function": lambda_arn("one-for-both"),
                "record-function": lambda_arn("one-for-both"),
                "sdk-version": "1.0",
                "spill-bucket": "spill",
            }),
        },
        WiringCase {
            case: "a LAMBDA catalog given a composite function and a catalog key of its own",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            submitted: json!({
                "function": lambda_arn("composite"),
                "catalog": "the-catalog-the-request-named",
            }),
            // What the request states, it states: the value it gave stands.
            held: json!({
                "catalog": "the-catalog-the-request-named",
                "metadata-function": lambda_arn("composite"),
                "record-function": lambda_arn("composite"),
            }),
        },
        WiringCase {
            case: "a LAMBDA catalog given a function per role",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            submitted: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
            }),
            // The map already names which function serves which role, so there is nothing
            // to resolve.
            held: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
            }),
        },
        WiringCase {
            case: "a LAMBDA catalog given a function per role beside a key of neither form",
            catalog_type: DATA_CATALOG_TYPE_LAMBDA,
            submitted: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
                "sdk-version": "1.0",
            }),
            held: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
                "sdk-version": "1.0",
            }),
        },
        WiringCase {
            case: "a HIVE catalog, whose single function is a metastore handler",
            catalog_type: DATA_CATALOG_TYPE_HIVE,
            submitted: json!({
                "metadata-function": lambda_arn("hive-metastore"),
                "sdk-version": "1.0",
            }),
            held: json!({
                "metadata-function": lambda_arn("hive-metastore"),
                "sdk-version": "1.0",
            }),
        },
        WiringCase {
            case: "a GLUE catalog, which names a catalog id rather than a function",
            catalog_type: DATA_CATALOG_TYPE_GLUE,
            submitted: json!({ "catalog-id": "210987654321" }),
            held: json!({ "catalog-id": "210987654321" }),
        },
        WiringCase {
            case: "a FEDERATED catalog carrying a function key, which is not a form of its \
                   type",
            catalog_type: DATA_CATALOG_TYPE_FEDERATED,
            // The composite form belongs to the LAMBDA type, so for this type the key is
            // just another key of the map and is held as it was given.
            submitted: json!({
                "connection-arn": "arn:aws:glue:us-east-1:123456789012:connection/mysql",
                "function": lambda_arn("not-a-lambda-catalog"),
            }),
            held: json!({
                "connection-arn": "arn:aws:glue:us-east-1:123456789012:connection/mysql",
                "function": lambda_arn("not-a-lambda-catalog"),
            }),
        },
        WiringCase {
            case: "a FEDERATED catalog describing a new connection",
            catalog_type: DATA_CATALOG_TYPE_FEDERATED,
            submitted: json!({ "connection-type": "MYSQL" }),
            held: json!({ "connection-type": "MYSQL" }),
        },
    ]
}

/// `GetDataCatalog`'s view of one catalog: `(status, DataCatalog)`.
async fn read_catalog(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status, body["DataCatalog"].clone())
}

/// RULE: a catalog holds the connector wiring its parameters resolve to — one composite
/// LAMBDA function under both role keys beside the name of the catalog it is asked about,
/// every other map as it was given — and that is what the create answers with and what
/// every later read reports.
#[tokio::test]
async fn a_created_catalog_holds_the_wiring_its_parameters_resolve_to() {
    let router = build_router();

    for (index, case) in wiring_cases().into_iter().enumerate() {
        let name = format!("wiring-create-{index}");
        let held = with_catalog_name(&case.held, &name);

        let (status, created) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Parameters": case.submitted,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the catalog to be created, got {status} with {created:?}",
            case.case
        );
        assert_eq!(
            created["DataCatalog"]["Parameters"], held,
            "case {:?}: expected the create response to report {held:?}, got {:?}",
            case.case, created["DataCatalog"]["Parameters"]
        );

        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected GetDataCatalog to answer 200, got {status} with {read:?}",
            case.case
        );
        assert_eq!(
            read["Parameters"], held,
            "case {:?}: expected the read to report {held:?}, got {:?}",
            case.case, read["Parameters"]
        );

        // A read changes nothing: the second one reports the same wiring as the first.
        let (status, again) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the second read to answer 200, got {status} with {again:?}",
            case.case
        );
        assert_eq!(
            again["Parameters"], held,
            "case {:?}: expected the second read to report {held:?}, got {:?}",
            case.case, again["Parameters"]
        );
    }
}

/// RULE: the same wiring, written by the update path — and because what a catalog holds is
/// the resolved wiring, restating it leaves it where it is: an update that names no
/// parameters keeps it, and one that states the resolved map itself is accepted and reads
/// back unchanged.
#[tokio::test]
async fn an_update_holds_the_same_wiring_and_restating_it_changes_nothing() {
    let router = build_router();

    for (index, case) in wiring_cases().into_iter().enumerate() {
        let name = format!("wiring-update-{index}");
        let held = with_catalog_name(&case.held, &name);

        // The catalog starts from a map of its type that states no wiring to resolve, so
        // what the assertions below observe is what the UPDATE wrote.
        let (status, created) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": case.catalog_type,
                "Parameters": starting_parameters(case.catalog_type),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must be created, got {status} with {created:?}",
            case.case
        );

        for (step, parameters) in [
            ("the submitted map", Some(case.submitted.clone())),
            // An update that names no map of its own keeps the one the catalog holds.
            ("no map at all", None),
            // The wiring itself: a map the service holds is a map it accepts.
            ("the wiring the catalog holds", Some(held.clone())),
        ] {
            let mut update = json!({ "Name": name, "Type": case.catalog_type });
            if let Some(parameters) = parameters {
                update["Parameters"] = parameters;
            }
            let (status, answered) = call(&router, "UpdateDataCatalog", &update).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected the update naming {step} to be applied, got {status} \
                 with {answered:?}",
                case.case
            );

            let (status, read) = read_catalog(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected GetDataCatalog to answer 200 after the update naming \
                 {step}, got {status} with {read:?}",
                case.case
            );
            assert_eq!(
                read["Parameters"], held,
                "case {:?}: after the update naming {step}, expected {held:?}, got {:?}",
                case.case, read["Parameters"]
            );
        }
    }
}

/// A `Parameters` map of `catalog_type` that states a wiring with nothing to resolve, which
/// is where every row of the update test starts. One per type, from the forms
/// `DataCatalog$Parameters` documents.
fn starting_parameters(catalog_type: &str) -> Value {
    match catalog_type {
        DATA_CATALOG_TYPE_HIVE => {
            json!({ "metadata-function": lambda_arn("starting-metastore") })
        }
        DATA_CATALOG_TYPE_GLUE => json!({ "catalog-id": "123456789012" }),
        DATA_CATALOG_TYPE_FEDERATED => json!({ "connection-type": "REDSHIFT" }),
        _ => json!({
            "metadata-function": lambda_arn("starting-metadata"),
            "record-function": lambda_arn("starting-records"),
        }),
    }
}
