// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for the BODY of a Lake Formation client refusal: the modelled error a
//! 400 names and the `Message` it carries.
//!
//! An HTTP status, an error code and an error message are all part of the contract a
//! client branches on, so this suite reads each refusal the way an SDK does — the status,
//! the `x-amzn-errortype` header restJson1 resolves the error SHAPE from, and the body's
//! `Message` member — and compares the message against the sentence the service words that
//! error class with, spelled out here rather than read back out of the crate.
//!
//! Four rules, one table each:
//!
//! * a value the `Permission` enum does not carry is refused, on BOTH write paths, with
//!   the `InvalidInputException` those operations model, whose message is the service's
//!   invalid-permissions template listing the values it rejected;
//! * an administrator identifier that is not an Amazon Resource Name is refused with the
//!   service's ARN-shape sentence, whose one variable part is the rejected identifier as
//!   the request wrote it;
//! * a refusal about the entity a request ADDRESSED — absent, or already there — carries
//!   wholly fixed text, naming neither the entity nor the catalog;
//! * the BODY of any refusal carries the error shape's own modelled members and nothing
//!   else, because this service speaks `restJson1`, which names the error SHAPE in the
//!   `x-amzn-errortype` header.
//!
//! The rows of each table differ along what its rule generalises over: which list the
//! offending value sits in and what form it takes, which administrator member names the
//! rejected identifier and what shape that identifier has, and which operation of which
//! entity class answers the refusal.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one copy of the harness this crate's permission suites share: the
// Glue+LakeFormation emulator, the seeded catalog, the `restJson1` wire path and the
// error-code extractor.
use crate::glue_catalog::{
    account_catalog, emulator, emulator_with_catalog, error_code, lake_formation_response,
    lake_formation_wire,
};

/// The invalid-permissions refusal as Lake Formation words it: two fixed sentences with
/// the rejected values bracketed between them, comma-separated in the order the request
/// named them. The leading and trailing single spaces are part of the text.
fn invalid_permissions_message(rejected: &[&str]) -> String {
    format!(
        " Please grant only valid Lake Formation permissions. [{}] are not all valid Lake \
         Formation permissions. ",
        rejected.join(", ")
    )
}

/// The ARN-shape refusal as the service words it: fixed text followed by the rejected
/// identifier, unquoted and exactly as the request wrote it.
fn invalid_arn_message(identifier: &str) -> String {
    format!("Invalid ARN:ARNs must start with 'arn:': {identifier}")
}

/// The `Message` member of a refusal body, or `None` when the body carries none.
fn message_of(payload: &Value) -> Option<&str> {
    payload.get("Message").and_then(Value::as_str)
}

/// A refused call: its status, the `x-amzn-errortype` header, the error code of the body
/// and the body itself, asserting only that the status was 400.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> (String, String, Value) {
    let (status, error_type, payload) = lake_formation_response(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} {payload:?}"
    );
    (error_type, error_code(&payload), payload)
}

/// One `DataLakePrincipal` as a request writes it.
fn principal(identifier: &str) -> Value {
    json!({ "DataLakePrincipalIdentifier": identifier })
}

/// One row of the permission table: the case name, the `Permissions` the request names,
/// its `PermissionsWithGrantOption`, and the values the refusal must list — `None` for the
/// control row, which names only values the enum carries and is accepted.
type PermissionCase = (
    &'static str,
    Vec<&'static str>,
    Vec<&'static str>,
    Option<Vec<&'static str>>,
);

#[tokio::test]
async fn an_unmodelled_permission_is_refused_with_the_services_invalid_permissions_message() {
    // Rows differ along the form the unmodelled value takes (an invented name, a
    // lower-cased modelled value, a modelled value with trailing whitespace, the empty
    // string), how many of them one request names, which list they sit in, and whether a
    // modelled value sits beside them — the refusal names the rejected values and only
    // those. The last row is the control: every value it names IS a member of the enum
    // (`smithy_query(query="shape",shape="Permission")`), so it is accepted.
    let cases: Vec<PermissionCase> = vec![
        (
            "an invented value alone",
            vec!["NOSUCHPERMISSION"],
            vec![],
            Some(vec!["NOSUCHPERMISSION"]),
        ),
        (
            "an invented value beside a modelled one",
            vec!["SELECT", "SUPER"],
            vec![],
            Some(vec!["SUPER"]),
        ),
        (
            "two invented values",
            vec!["SUPER", "NOSUCHPERMISSION"],
            vec![],
            Some(vec!["SUPER", "NOSUCHPERMISSION"]),
        ),
        (
            "one invented value named twice",
            vec!["SUPER", "SUPER"],
            vec![],
            Some(vec!["SUPER"]),
        ),
        (
            // `Permission`'s values are upper-case, so a lower-cased spelling is a
            // different string and not a member of the enum.
            "a lower-cased modelled value",
            vec!["select"],
            vec![],
            Some(vec!["select"]),
        ),
        (
            "a modelled value with trailing whitespace",
            vec!["SELECT "],
            vec![],
            Some(vec!["SELECT "]),
        ),
        ("the empty string", vec![""], vec![], Some(vec![""])),
        (
            "an invented value only in PermissionsWithGrantOption",
            vec!["SELECT"],
            vec!["SELECT", "NOSUCHPERMISSION"],
            Some(vec!["NOSUCHPERMISSION"]),
        ),
        (
            "an invented value in each list",
            vec!["ALTER", "FIRST"],
            vec!["SECOND"],
            Some(vec!["FIRST", "SECOND"]),
        ),
        // The control: only values the enum carries, so nothing is rejected.
        (
            "only modelled values",
            vec!["SELECT"],
            vec!["SELECT"],
            None,
        ),
    ];

    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    for (index, (case, permissions, with_grant_option, rejected)) in cases.iter().enumerate() {
        // One principal per row, so a row that IS accepted cannot change what another
        // row's call answers.
        let mut body = json!({
            "Principal": principal(&format!("arn:aws:iam::{}:role/row-{index}", account_catalog())),
            "Resource": { "Database": { "Name": "sales" } },
            "Permissions": permissions,
        });
        if !with_grant_option.is_empty() {
            body["PermissionsWithGrantOption"] = json!(with_grant_option);
        }

        for operation in ["GrantPermissions", "RevokePermissions"] {
            let Some(rejected) = rejected else {
                // The control row is accepted on both paths: the grant records what it
                // names and the revoke takes the same permissions away again.
                let (status, _error_type, payload) =
                    lake_formation_response(&router, operation, &body).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected {operation} to answer 200, got {status} {payload:?}"
                );
                continue;
            };

            let (error_type, code, payload) = refusal(&router, case, operation, &body).await;
            assert_eq!(
                code, "InvalidInputException",
                "case {case:?}: expected {operation} to refuse with InvalidInputException, \
                 got {code:?} {payload:?}"
            );
            assert_eq!(
                error_type, "InvalidInputException",
                "case {case:?}: expected {operation} to name InvalidInputException in \
                 x-amzn-errortype, got {error_type:?}"
            );
            let expected = invalid_permissions_message(rejected);
            let actual = message_of(&payload);
            assert_eq!(
                actual,
                Some(expected.as_str()),
                "case {case:?}: expected {operation} to answer Message {expected:?}, got \
                 {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_administrator_that_is_not_an_arn_is_refused_with_the_arn_shape_message() {
    // Rows differ along the shape of the value that is not an ARN (an account id, the
    // IAM-allowed group, a bare name, a path, an ARN-like value whose prefix is cased
    // differently, the empty value of an entry naming no principal), which administrator
    // member carries it (`DataLakeAdmins`, `ReadOnlyAdmins` — both documented "Supported
    // principals are IAM users or IAM roles."), and where in a longer list it sits: the
    // refusal names the rejected value, so a caller can tell WHICH entry was refused.
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "an administrator named by account id",
            json!({ "DataLakeAdmins": [principal("210987654321")] }),
            "210987654321",
        ),
        (
            "the IAM-allowed group as an administrator",
            json!({ "DataLakeAdmins": [principal("IAM_ALLOWED_PRINCIPALS")] }),
            "IAM_ALLOWED_PRINCIPALS",
        ),
        (
            "an administrator named by a bare name",
            json!({ "DataLakeAdmins": [principal("not-an-arn-or-known-principal")] }),
            "not-an-arn-or-known-principal",
        ),
        (
            // The prefix is matched as written, so a differently cased one is not one.
            "an administrator whose ARN prefix is upper-cased",
            json!({ "DataLakeAdmins": [principal("ARN:aws:iam::210987654321:role/admin")] }),
            "ARN:aws:iam::210987654321:role/admin",
        ),
        (
            "an administrator entry naming no principal",
            json!({ "DataLakeAdmins": [json!({})] }),
            "",
        ),
        (
            "a read-only administrator named by account id",
            json!({ "ReadOnlyAdmins": [principal("210987654321")] }),
            "210987654321",
        ),
        (
            "the second of two administrators",
            json!({
                "DataLakeAdmins": [
                    principal("arn:aws:iam::210987654321:role/lake-admin"),
                    principal("lake-admin"),
                ],
            }),
            "lake-admin",
        ),
        (
            "an administrator supported in one list and unsupported in the other",
            json!({
                "DataLakeAdmins": [principal("arn:aws:iam::210987654321:user/auditor")],
                "ReadOnlyAdmins": [principal("auditor")],
            }),
            "auditor",
        ),
    ];

    let router = emulator();
    for (case, settings, rejected) in &cases {
        let (error_type, code, payload) = refusal(
            &router,
            case,
            "PutDataLakeSettings",
            &json!({ "DataLakeSettings": settings }),
        )
        .await;
        assert_eq!(
            code, "InvalidInputException",
            "case {case:?}: expected InvalidInputException, got {code:?} {payload:?}"
        );
        assert_eq!(
            error_type, "InvalidInputException",
            "case {case:?}: expected InvalidInputException in x-amzn-errortype, got \
             {error_type:?}"
        );
        let expected = invalid_arn_message(rejected);
        let actual = message_of(&payload);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case:?}: expected Message {expected:?}, got {actual:?}"
        );
    }
}

/// One row of the entity-state table: the calls that set the row up, the call that is
/// refused, and the error code and whole message that refusal must carry.
struct EntityStateCase {
    case: &'static str,
    prepare: Vec<(&'static str, Value)>,
    refused: (&'static str, Value),
    code: &'static str,
    message: &'static str,
}

#[tokio::test]
async fn an_entity_state_refusal_carries_the_services_fixed_sentence() {
    // Rows span the entity classes this service refuses on state — a data cells filter, a
    // registered data-lake location, an LF-tag — and, for each, both directions the
    // refusal can run: the entity is not there, or it is already there. Every message
    // below is wholly fixed text: none of them names the filter, the ARN, the tag key or
    // the catalog the request carried, so the same sentence answers every such call.
    let catalog = account_catalog();
    let table_data = |name: &str| {
        json!({
            "TableCatalogId": catalog,
            "DatabaseName": "sales",
            "TableName": "orders",
            "Name": name,
            "ColumnNames": ["order_id"],
            "RowFilter": { "AllRowsWildcard": {} },
        })
    };
    let locator = |name: &str| {
        json!({
            "TableCatalogId": catalog,
            "DatabaseName": "sales",
            "TableName": "orders",
            "Name": name,
        })
    };
    let location = "arn:aws:s3:::lf-bucket/warehouse/";
    let registration = json!({
        "ResourceArn": location,
        "RoleArn": format!("arn:aws:iam::{catalog}:role/my-data-access-role"),
    });
    let cases = vec![
        EntityStateCase {
            case: "a read of a filter that was never created",
            prepare: Vec::new(),
            refused: ("GetDataCellsFilter", locator("absent")),
            code: "EntityNotFoundException",
            message: "Entity Not Found",
        },
        EntityStateCase {
            case: "an update of a filter that was never created",
            prepare: Vec::new(),
            refused: (
                "UpdateDataCellsFilter",
                json!({ "TableData": table_data("absent") }),
            ),
            code: "EntityNotFoundException",
            message: "Entity Not Found",
        },
        EntityStateCase {
            case: "a create of a filter that is already there",
            prepare: vec![(
                "CreateDataCellsFilter",
                json!({ "TableData": table_data("held") }),
            )],
            refused: (
                "CreateDataCellsFilter",
                json!({ "TableData": table_data("held") }),
            ),
            code: "AlreadyExistsException",
            message: "Data cells filter already exists.",
        },
        EntityStateCase {
            case: "a read of a location that was never registered",
            prepare: Vec::new(),
            refused: ("DescribeResource", json!({ "ResourceArn": location })),
            code: "EntityNotFoundException",
            message: "Entity not found",
        },
        EntityStateCase {
            case: "a register of a location that is already registered",
            prepare: vec![("RegisterResource", registration.clone())],
            refused: ("RegisterResource", registration),
            code: "AlreadyExistsException",
            message: "Resource is already registered",
        },
        EntityStateCase {
            case: "a read of a tag key that was never created",
            prepare: Vec::new(),
            refused: ("GetLFTag", json!({ "TagKey": "absent" })),
            code: "EntityNotFoundException",
            message: "Tag key does not exist",
        },
        EntityStateCase {
            case: "a delete of a tag key that was never created",
            prepare: Vec::new(),
            refused: ("DeleteLFTag", json!({ "TagKey": "absent" })),
            code: "EntityNotFoundException",
            message: "Tag key does not exist",
        },
        EntityStateCase {
            case: "an update of a tag key that was never created",
            prepare: Vec::new(),
            refused: (
                "UpdateLFTag",
                json!({ "TagKey": "absent", "TagValuesToAdd": ["eu"] }),
            ),
            code: "EntityNotFoundException",
            message: "Tag key does not exist",
        },
        EntityStateCase {
            case: "a create of a tag key that is already there",
            prepare: vec![(
                "CreateLFTag",
                json!({ "TagKey": "held", "TagValues": ["eu"] }),
            )],
            refused: (
                "CreateLFTag",
                json!({ "TagKey": "held", "TagValues": ["eu"] }),
            ),
            code: "InvalidInputException",
            message: "Tag key already exists",
        },
    ];

    for row in &cases {
        // One emulator per row, so a row's set-up is the only state its refusal sees.
        let router = emulator();
        for (operation, body) in &row.prepare {
            let (status, _error_type, payload) =
                lake_formation_response(&router, operation, body).await;
            assert_eq!(
                status, 200,
                "case {:?}: expected the set-up {operation} to answer 200, got {status} \
                 {payload:?}",
                row.case
            );
        }
        let (operation, body) = &row.refused;
        let (error_type, code, payload) = refusal(&router, row.case, operation, body).await;
        assert_eq!(
            code, row.code,
            "case {:?}: expected {operation} to refuse with {}, got {code:?} {payload:?}",
            row.case, row.code
        );
        assert_eq!(
            error_type, row.code,
            "case {:?}: expected {} in x-amzn-errortype, got {error_type:?}",
            row.case, row.code
        );
        let actual = message_of(&payload);
        assert_eq!(
            actual,
            Some(row.message),
            "case {:?}: expected {operation} to answer Message {:?}, got {actual:?}",
            row.case, row.message
        );
    }
}

/// The member a JSON error envelope of the AWS JSON protocol FAMILY names its error shape
/// in, which a `restJson1` body does not carry: that protocol names the shape in the
/// `x-amzn-errortype` header instead.
const SHAPE_DISCRIMINATOR: &str = "__type";

/// One row of the error-body table: the case, the call that is refused, and the status, the
/// error shape and the one modelled member its body must carry.
struct ErrorBodyCase {
    case: &'static str,
    refused: (&'static str, Value),
    status: u16,
    shape: &'static str,
    message: String,
}

#[tokio::test]
async fn a_refusals_body_carries_the_error_shapes_modelled_members_and_nothing_else() {
    // RULE: this service speaks `restJson1`, which resolves a failure's error SHAPE from
    // the `x-amzn-errortype` response header, so the BODY of a refusal carries the error
    // shape's own modelled members and nothing else. Every error shape of this model
    // declares the single member `Message`, so a refusal's body is exactly that one key —
    // never the AWS JSON families' `__type` discriminator beside it.
    //
    // The rows differ along what the rule generalises over: the error shape answered, the
    // operation and entity class that answers it, and whether the message is wholly fixed
    // text or carries a value from the request.
    let catalog = account_catalog();
    let cases = vec![
        ErrorBodyCase {
            // The grant surface's own refusal, which carries a request value.
            case: "a permission the model does not carry",
            refused: (
                "GrantPermissions",
                json!({
                    "Principal": principal(&format!("arn:aws:iam::{catalog}:role/analyst")),
                    "Resource": { "Database": { "Name": "sales" } },
                    "Permissions": ["SUPER"],
                }),
            ),
            status: 400,
            shape: "InvalidInputException",
            message: invalid_permissions_message(&["SUPER"]),
        },
        ErrorBodyCase {
            case: "an administrator that is not an ARN",
            refused: (
                "PutDataLakeSettings",
                json!({
                    "DataLakeSettings": { "DataLakeAdmins": [principal("lake-admin")] },
                }),
            ),
            status: 400,
            shape: "InvalidInputException",
            message: invalid_arn_message("lake-admin"),
        },
        ErrorBodyCase {
            // Wholly fixed text, and the recorded shape of this service's settings refusal.
            case: "a principal designated in both administrator lists",
            refused: (
                "PutDataLakeSettings",
                json!({
                    "DataLakeSettings": {
                        "DataLakeAdmins": [
                            principal(&format!("arn:aws:iam::{catalog}:role/lake-admin")),
                        ],
                        "ReadOnlyAdmins": [
                            principal(&format!("arn:aws:iam::{catalog}:role/lake-admin")),
                        ],
                    },
                }),
            ),
            status: 400,
            shape: "InvalidInputException",
            message: "Principal cannot be both readOnly and LF admin.".to_string(),
        },
        ErrorBodyCase {
            // A different error SHAPE, on a different entity class.
            case: "a tag key that was never created",
            refused: ("GetLFTag", json!({ "TagKey": "absent" })),
            status: 400,
            shape: "EntityNotFoundException",
            message: "Tag key does not exist".to_string(),
        },
        ErrorBodyCase {
            case: "a data cells filter that was never created",
            refused: (
                "GetDataCellsFilter",
                json!({
                    "TableCatalogId": catalog,
                    "DatabaseName": "sales",
                    "TableName": "orders",
                    "Name": "absent",
                }),
            ),
            status: 400,
            shape: "EntityNotFoundException",
            message: "Entity Not Found".to_string(),
        },
    ];

    let router = emulator();
    for row in &cases {
        let (operation, body) = &row.refused;
        // The response BYTES, with nothing folded into them: this rule is about what the
        // wire body carries, key set included.
        let (status, error_type, raw) = lake_formation_wire(&router, operation, body).await;
        let rendered = String::from_utf8_lossy(&raw).to_string();
        assert_eq!(
            status, row.status,
            "case {:?}: expected {operation} to answer {}, got {status} {rendered}",
            row.case, row.status
        );
        assert_eq!(
            error_type, row.shape,
            "case {:?}: expected {} in x-amzn-errortype, got {error_type:?}",
            row.case, row.shape
        );
        let document: Value = serde_json::from_slice(&raw)
            .unwrap_or_else(|e| panic!("case {:?}: the body must be JSON ({e})", row.case));
        assert_eq!(
            document,
            json!({ "Message": row.message }),
            "case {:?}: expected the body to carry the shape's Message alone, got {document}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_request_naming_no_modelled_operation_carries_no_shape_in_its_body_either() {
    // The same rule at the other seam this service owns: a request it claimed that names no
    // modelled operation is answered by the framework's routing failure, whose body is
    // corrected the same way — the shape travels in `x-amzn-errortype` alone.
    //
    // The rows differ along the form the unknown operation takes.
    let router = emulator();
    for case in ["NoSuchOperation", "grantpermissions", "Grant%20Permissions"] {
        let (status, error_type, raw) = lake_formation_wire(&router, case, &json!({})).await;
        let rendered = String::from_utf8_lossy(&raw).to_string();
        assert_eq!(
            status, 501,
            "case {case:?}: expected the routing failure to answer 501, got {status} {rendered}"
        );
        assert!(
            !error_type.is_empty(),
            "case {case:?}: expected the failure to name its shape in x-amzn-errortype, got \
             {error_type:?}"
        );
        let document: Value = serde_json::from_slice(&raw)
            .unwrap_or_else(|e| panic!("case {case:?}: the body must be JSON ({e})"));
        assert!(
            document.get(SHAPE_DISCRIMINATOR).is_none(),
            "case {case:?}: the body must carry no {SHAPE_DISCRIMINATOR} member, got {document}"
        );
    }
}
