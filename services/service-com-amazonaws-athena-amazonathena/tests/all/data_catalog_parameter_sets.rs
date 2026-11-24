// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ONE rule a LAMBDA data catalog's `Parameters` map is held to,
//! as `DataCatalog$Parameters` documents it: the map carries either the
//! `metadata-function` and `record-function` pair or the `function` key on its own, never
//! both and never neither. Driven over the wire (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`) through the same router the emulator boots.
//!
//! The rule is asserted over a TABLE of maps rather than one recorded request — the rows
//! differ in which keys they carry, how many they carry, and whether they carry half of a
//! set — and each row is sent to BOTH operations that take a `Parameters` map, so the
//! refusal a client sees does not depend on which path measured the map.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::DATA_CATALOG_TYPE_LAMBDA;

use crate::harness::{call, error_code, message, router as build_router};

/// The wire error code a refused request must carry: the client error both operations
/// model (HTTP 400).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The sentence Athena refuses a LAMBDA catalog's `Parameters` map with, whichever way the
/// map breaks the rule. Written out here rather than read from the crate, so the test pins
/// the wording instead of agreeing with whatever the service happens to say.
const PARAMETER_SETS_MESSAGE: &str = "Lambda catalogs require that either the \
     metadata-function and record-function parameters or the function parameter be set, \
     but not both.";

/// A Lambda function ARN a catalog parameter names, built from `role` so the rows can be
/// told apart.
fn lambda_arn(role: &str) -> String {
    format!("arn:aws:lambda:us-east-1:123456789012:function:{role}")
}

/// One row: a `Parameters` map and whether it describes a LAMBDA catalog.
struct ParameterSetCase {
    case: &'static str,
    parameters: Value,
    accepted: bool,
    /// What a read of the catalog named `name` must report for this map: the map itself
    /// where it already names a function per role, and the wiring a composite function
    /// resolves to where it names one function for both. Unused by a refused row.
    read_back: fn(&str) -> Value,
}

/// The rows. The two sets the documentation names are accepted, on their own and beside
/// keys that belong to neither set (`sdk-version` and `catalog`, both from the same
/// `DataCatalog$Parameters` documentation); a map carrying half of the split set, both
/// sets at once, or neither is refused.
fn parameter_set_cases() -> Vec<ParameterSetCase> {
    vec![
        ParameterSetCase {
            case: "the composite function key on its own",
            parameters: json!({ "function": lambda_arn("composite") }),
            accepted: true,
            // One function serving both roles is held under both role keys, beside the
            // name of the catalog it is asked about.
            read_back: |name| {
                json!({
                    "catalog": name,
                    "metadata-function": lambda_arn("composite"),
                    "record-function": lambda_arn("composite"),
                })
            },
        },
        ParameterSetCase {
            case: "the composite function key beside a key of neither set",
            parameters: json!({
                "function": lambda_arn("composite"),
                "sdk-version": "1.0",
            }),
            accepted: true,
            read_back: |name| {
                json!({
                    "catalog": name,
                    "metadata-function": lambda_arn("composite"),
                    "record-function": lambda_arn("composite"),
                    "sdk-version": "1.0",
                })
            },
        },
        ParameterSetCase {
            case: "the split set",
            parameters: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
            }),
            accepted: true,
            read_back: |_| {
                json!({
                    "metadata-function": lambda_arn("metadata"),
                    "record-function": lambda_arn("records"),
                })
            },
        },
        ParameterSetCase {
            case: "the split set beside two keys of neither set",
            parameters: json!({
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
                "sdk-version": "1.0",
                "catalog": "analytics",
            }),
            accepted: true,
            // Every key the request named keeps the value it gave, the catalog key
            // included.
            read_back: |_| {
                json!({
                    "metadata-function": lambda_arn("metadata"),
                    "record-function": lambda_arn("records"),
                    "sdk-version": "1.0",
                    "catalog": "analytics",
                })
            },
        },
        ParameterSetCase {
            case: "the metadata function alone, half of the split set",
            parameters: json!({ "metadata-function": lambda_arn("metadata") }),
            accepted: false,
            read_back: |_| Value::Null,
        },
        ParameterSetCase {
            case: "the record function alone, the other half of the split set",
            parameters: json!({ "record-function": lambda_arn("records") }),
            accepted: false,
            read_back: |_| Value::Null,
        },
        ParameterSetCase {
            case: "both sets at once",
            parameters: json!({
                "function": lambda_arn("composite"),
                "metadata-function": lambda_arn("metadata"),
                "record-function": lambda_arn("records"),
            }),
            accepted: false,
            read_back: |_| Value::Null,
        },
        ParameterSetCase {
            case: "the composite key together with half of the split set",
            parameters: json!({
                "function": lambda_arn("composite"),
                "metadata-function": lambda_arn("metadata"),
            }),
            accepted: false,
            read_back: |_| Value::Null,
        },
        ParameterSetCase {
            case: "a map of keys that belong to neither set",
            parameters: json!({ "sdk-version": "1.0", "catalog": "analytics" }),
            accepted: false,
            read_back: |_| Value::Null,
        },
        ParameterSetCase {
            case: "a key whose name only contains one of the set's keys",
            parameters: json!({ "lambda-function": lambda_arn("composite") }),
            accepted: false,
            read_back: |_| Value::Null,
        },
    ]
}

/// `GetDataCatalog`'s view of one catalog: `(status, DataCatalog)`.
async fn read_catalog(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> (u16, Value) {
    let (status, body) = call(router, "GetDataCatalog", &json!({ "Name": name })).await;
    (status, body["DataCatalog"].clone())
}

#[tokio::test]
async fn create_holds_a_lambda_catalogs_parameters_to_one_of_the_two_sets() {
    let router = build_router();

    for (index, case) in parameter_set_cases().into_iter().enumerate() {
        let name = format!("create-parameter-set-{index}");
        let (status, body) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": case.parameters,
            }),
        )
        .await;

        if case.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: expected the catalog to be created, got {status} with {body:?}",
                case.case
            );
            let (status, read) = read_catalog(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected GetDataCatalog to answer 200, got {status} with \
                 {read:?}",
                case.case
            );
            let expected = (case.read_back)(&name);
            assert_eq!(
                read["Parameters"], expected,
                "case {:?}: expected Parameters {expected:?}, got {:?}",
                case.case, read["Parameters"]
            );
            continue;
        }

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
            PARAMETER_SETS_MESSAGE,
            "case {:?}: expected the message {PARAMETER_SETS_MESSAGE:?}, got {:?}",
            case.case,
            message(&body)
        );
        // The refused create registered nothing.
        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the refused create to register nothing, got {status} \
             with {read:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn update_holds_a_lambda_catalogs_parameters_to_the_same_two_sets() {
    let router = build_router();

    // The map every row starts from: one registered LAMBDA catalog per row, so a row that
    // is accepted cannot change what the next row reads. It states one composite function,
    // so the catalog holds the wiring that resolves to (`registered_wiring`).
    let registered = json!({ "function": lambda_arn("registered") });
    let registered_wiring = |name: &str| {
        json!({
            "catalog": name,
            "metadata-function": lambda_arn("registered"),
            "record-function": lambda_arn("registered"),
        })
    };

    for (index, case) in parameter_set_cases().into_iter().enumerate() {
        let name = format!("update-parameter-set-{index}");
        let (status, created) = call(
            &router,
            "CreateDataCatalog",
            &json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Description": "the created description",
                "Parameters": registered,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must be created, got {status} with {created:?}",
            case.case
        );

        let (status, body) = call(
            &router,
            "UpdateDataCatalog",
            &json!({
                "Name": name,
                "Type": DATA_CATALOG_TYPE_LAMBDA,
                "Parameters": case.parameters,
            }),
        )
        .await;

        if case.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: expected the update to be applied, got {status} with {body:?}",
                case.case
            );
            let (status, read) = read_catalog(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected GetDataCatalog to answer 200, got {status} with \
                 {read:?}",
                case.case
            );
            let expected = (case.read_back)(&name);
            assert_eq!(
                read["Parameters"], expected,
                "case {:?}: expected Parameters {expected:?}, got {:?}",
                case.case, read["Parameters"]
            );
            continue;
        }

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
            PARAMETER_SETS_MESSAGE,
            "case {:?}: expected the message {PARAMETER_SETS_MESSAGE:?}, got {:?}",
            case.case,
            message(&body)
        );
        // The refused update wrote nothing: the registered wiring still stands.
        let (status, read) = read_catalog(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {:?}: the catalog must still be readable, got {status} with {read:?}",
            case.case
        );
        let standing = registered_wiring(&name);
        assert_eq!(
            read["Parameters"], standing,
            "case {:?}: expected the registered Parameters {standing:?} to stand, got \
             {:?}",
            case.case, read["Parameters"]
        );
    }
}
