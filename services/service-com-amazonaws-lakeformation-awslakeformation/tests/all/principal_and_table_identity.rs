// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for two rules the permission surface holds every request to, whichever
//! operation carries it:
//!
//! * a `DataLakePrincipalIdentifier` names a Lake Formation principal — an Amazon Resource
//!   Name, or one of the non-ARN forms Lake Formation defines (the IAM-allowed group, an
//!   AWS account id, an AWS Organizations organization or organizational unit) — and a
//!   value that names none is refused with the service's ARN-shape sentence carrying the
//!   rejected value;
//! * a `Resource.Table` names a table by `Name` or by `TableWildcard`, never both, and one
//!   naming both is refused with the service's fixed sentence for that pair.
//!
//! Each table's rows are driven through every operation that reads the member, so the read,
//! the write and the batch cannot drift on it: `ListPermissions`, `GrantPermissions`,
//! `RevokePermissions` and one `BatchGrantPermissions` entry — where the refusal is the
//! entry's reported failure rather than the call's, because a batch reports a bad entry
//! instead of refusing the request.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::glue_catalog::{
    account_catalog, emulator_with_catalog, error_code, invalid_arn_message, lake_formation,
    lake_formation_response, list_permissions,
};

/// The refusal for a `Resource.Table` naming both of its alternatives: wholly fixed text,
/// naming neither the table nor the database, trailing period included.
const TABLE_NAME_AND_WILDCARD: &str = "Table name and table wildcard cannot both be present.";

/// The error class every refusal in this suite is reported as.
const INVALID_INPUT: &str = "InvalidInputException";

/// The operations a row is driven through, in the order they are asserted.
const OPERATIONS: [&str; 4] = [
    "ListPermissions",
    "GrantPermissions",
    "RevokePermissions",
    "BatchGrantPermissions",
];

/// The whole message `operation` refuses `body` with.
///
/// `BatchGrantPermissions` carries the body as ONE entry and reports the refusal in
/// `Failures`, with HTTP 200 for the batch itself; every other operation answers HTTP 400
/// and the modelled `InvalidInputException`, whose shape restJson1 resolves from the
/// `x-amzn-errortype` header.
async fn refusal_message(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> String {
    if operation == "BatchGrantPermissions" {
        let mut entry = body.clone();
        entry["Id"] = json!("one");
        let (status, payload) =
            lake_formation(router, operation, &json!({ "Entries": [entry] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the batch itself to answer 200, got {status} {payload:?}"
        );
        let failures = payload
            .get("Failures")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected the entry to be reported as failed, got {payload:?}"
        );
        let error = failures[0].get("Error").cloned().unwrap_or(Value::Null);
        assert_eq!(
            error.get("ErrorCode").and_then(Value::as_str),
            Some(INVALID_INPUT),
            "case {case:?}: expected the entry's failure to name {INVALID_INPUT}, got {error:?}"
        );
        return error
            .get("ErrorMessage")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
    }

    let (status, error_type, payload) = lake_formation_response(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to answer 400, got {status} {payload:?}"
    );
    assert_eq!(
        error_code(&payload),
        INVALID_INPUT,
        "case {case:?}: expected {operation} to refuse with {INVALID_INPUT}, got {payload:?}"
    );
    assert_eq!(
        error_type, INVALID_INPUT,
        "case {case:?}: expected {operation} to name {INVALID_INPUT} in x-amzn-errortype, got \
         {error_type:?}"
    );
    payload
        .get("Message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_principal_that_identifies_nobody_is_refused_with_the_arn_shape_sentence() {
    // Rows differ along the form the value takes: a bare role name, a role path with no ARN
    // fields around it, an ARN prefix in another case, an account id one digit short and
    // one digit long, an organization prefix with no id body, and a value that merely
    // contains "arn:" without starting with it.
    let rejected = [
        "analyst",
        "role/analyst",
        "ARN:aws:iam::210987654321:role/analyst",
        "12345678901",
        "1234567890123",
        "o-",
        "ou-",
        "iam:arn:aws:role/analyst",
    ];

    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    let database = json!({ "Database": { "Name": "sales" } });
    for identifier in rejected {
        let principal = json!({ "DataLakePrincipalIdentifier": identifier });
        for operation in OPERATIONS {
            // A listing reads the principal as a FILTER and names no resource; a write
            // names the resource it grants or revokes on.
            let body = if operation == "ListPermissions" {
                json!({ "Principal": principal })
            } else {
                json!({
                    "Principal": principal,
                    "Resource": database,
                    "Permissions": ["DESCRIBE"],
                })
            };
            let expected = invalid_arn_message(identifier);
            let actual = refusal_message(&router, identifier, operation, &body).await;
            assert_eq!(
                actual, expected,
                "case {identifier:?}: expected {operation} to answer Message {expected:?}, got \
                 {actual:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_form_of_lake_formation_principal_is_accepted() {
    // The forms that DO name a principal: an IAM role and an IAM user ARN, an ARN of
    // another identity system, the IAM-allowed group, an AWS account id, an organization id
    // and an organizational-unit id. Each is accepted as a listing's filter and as a
    // grantee — a value AWS accepts must not draw the ARN-shape refusal.
    let catalog = account_catalog();
    let accepted = [
        format!("arn:aws:iam::{catalog}:role/analyst"),
        format!("arn:aws:iam::{catalog}:user/analyst"),
        format!("arn:aws:quicksight:us-east-1:{catalog}:user/default/analyst"),
        "IAM_ALLOWED_PRINCIPALS".to_string(),
        "210987654321".to_string(),
        "o-abc123def4".to_string(),
        "ou-ab12-cdefg345".to_string(),
    ];

    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    let database = json!({ "Database": { "Name": "sales" } });
    for identifier in &accepted {
        let principal = json!({ "DataLakePrincipalIdentifier": identifier });
        // The listing answers a page rather than a refusal.
        list_permissions(&router, identifier, &json!({ "Principal": principal })).await;

        let (status, payload) = lake_formation(
            &router,
            "GrantPermissions",
            &json!({
                "Principal": principal,
                "Resource": database,
                "Permissions": ["DESCRIBE"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {identifier:?}: expected GrantPermissions to answer 200, got {status} \
             {payload:?}"
        );

        // What was granted is readable back under exactly that identifier.
        let listed = list_permissions(
            &router,
            identifier,
            &json!({ "Principal": principal, "Resource": database }),
        )
        .await;
        assert!(
            listed.iter().any(|entry| {
                entry
                    .get("Principal")
                    .and_then(|principal| principal.get("DataLakePrincipalIdentifier"))
                    .and_then(Value::as_str)
                    == Some(identifier.as_str())
            }),
            "case {identifier:?}: expected the grant to be listed for that principal, got \
             {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_principal_naming_no_identifier_is_refused_on_a_write_and_filters_nothing_on_a_read() {
    // `DataLakePrincipal$DataLakePrincipalIdentifier` is itself optional, so a request can
    // name a `Principal` carrying nothing. A write has no grantee then and is refused with
    // the ARN-shape sentence carrying the empty value; a listing simply names no principal
    // filter, which is not a refusal. (An identifier that is PRESENT and empty is refused by
    // the boundary validator instead: `DataLakePrincipalString` is modelled `len 1..255`.)
    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    let database = json!({ "Database": { "Name": "sales" } });
    for operation in ["GrantPermissions", "RevokePermissions", "BatchGrantPermissions"] {
        let body = json!({
            "Principal": {},
            "Resource": database,
            "Permissions": ["DESCRIBE"],
        });
        let expected = invalid_arn_message("");
        let actual = refusal_message(&router, "no identifier", operation, &body).await;
        assert_eq!(
            actual, expected,
            "case \"no identifier\": expected {operation} to answer Message {expected:?}, \
             got {actual:?}"
        );
    }

    // The read: no filter, so the listing answers the grants of the catalog.
    let (status, payload) =
        lake_formation(&router, "ListPermissions", &json!({ "Principal": {} })).await;
    assert_eq!(
        status, 200,
        "case \"no identifier\": expected ListPermissions to answer 200, got {status} \
         {payload:?}"
    );
}

#[tokio::test]
async fn a_table_naming_both_a_name_and_a_wildcard_is_refused_with_the_fixed_sentence() {
    // Rows differ along what the two alternatives are spelled with — a table that exists
    // and one that does not, and a `TableWildcard` beside each — because the refusal is
    // about the PAIR being present and names neither the table nor the database.
    let both = [
        json!({ "Table": { "DatabaseName": "sales", "Name": "orders", "TableWildcard": {} } }),
        json!({ "Table": { "DatabaseName": "sales", "Name": "absent", "TableWildcard": {} } }),
        json!({
            "Table": {
                "CatalogId": account_catalog(),
                "DatabaseName": "sales",
                "Name": "orders",
                "TableWildcard": {},
            },
        }),
    ];

    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    let principal = json!({
        "DataLakePrincipalIdentifier": format!("arn:aws:iam::{}:role/analyst", account_catalog()),
    });
    for resource in &both {
        for operation in OPERATIONS {
            let body = if operation == "ListPermissions" {
                json!({ "Resource": resource })
            } else {
                json!({
                    "Principal": principal,
                    "Resource": resource,
                    "Permissions": ["DESCRIBE"],
                })
            };
            let case = format!("{resource}");
            let actual = refusal_message(&router, &case, operation, &body).await;
            assert_eq!(
                actual, TABLE_NAME_AND_WILDCARD,
                "case {case:?}: expected {operation} to answer Message \
                 {TABLE_NAME_AND_WILDCARD:?}, got {actual:?}"
            );
        }
    }

    // The control: either alternative ALONE names a table, so neither is refused.
    for resource in [
        json!({ "Table": { "DatabaseName": "sales", "Name": "orders" } }),
        json!({ "Table": { "DatabaseName": "sales", "TableWildcard": {} } }),
    ] {
        let case = format!("{resource}");
        list_permissions(&router, &case, &json!({ "Resource": resource })).await;
        let (status, payload) = lake_formation(
            &router,
            "GrantPermissions",
            &json!({
                "Principal": principal,
                "Resource": resource,
                "Permissions": ["DESCRIBE"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GrantPermissions to answer 200, got {status} {payload:?}"
        );
    }
}
