// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for the permission SET a Lake Formation grant may ask for.
//!
//! Every call goes over the wire the way an SDK client does — a `restJson1` `POST` of a
//! JSON body through the router that claims `lakeformation` — so a response can only be
//! what the handler computed from the single shared engine store.
//!
//! Two rules, with one table each:
//!
//! * `Permission` is ONE vocabulary shared by every member of `Resource`, and each member
//!   names a DIFFERENT kind of resource, so only the privileges that kind has mean anything
//!   on it: a grant naming a permission the kind of resource it addresses does not hold is
//!   refused with HTTP 400, the modelled `InvalidInputException` and the service's
//!   invalid-modification sentence, with nothing recorded.
//! * `PermissionsWithGrantOption` "may only be a subset of the permissions granted in the
//!   Privileges" (`smithy_query(query="doc",shape="GrantPermissionsRequest",
//!   member="PermissionsWithGrantOption")`), so a grant handing out the grant option for a
//!   permission it does not grant is refused the same way.
//!
//! Each table's rows differ along what its rule generalises over: the kind of resource
//! addressed, which privilege class the permission named belongs to, which of the two
//! permission lists it sits in, and how the two lists overlap — with accepted rows for the
//! same kinds, so the refusal is about the combination rather than about the resource or the
//! permission alone. Both write paths are driven: `GrantPermissions` and one
//! `BatchGrantPermissions` entry, which must be refused for exactly the same reason.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one copy of the harness this crate's permission suites share: the Glue+LakeFormation
// emulator, the seeded catalog, the `restJson1` wire path and the error-code extractor.
use crate::glue_catalog::{
    account_catalog, emulator_with_catalog, error_code, lake_formation as call,
    lake_formation_response, lake_formation_wire, list_permissions, revoke_default_database_grant,
    revoke_table_default_grants,
};

/// The whole sentence Lake Formation refuses a permission set it cannot record with.
///
/// Wholly fixed text: no principal, resource, permission or limit is interpolated into it,
/// so the same sentence answers every such grant. Spelled out here rather than read back
/// out of the crate, the way this crate's other refusal suites spell theirs.
const PERMISSIONS_MODIFICATION_INVALID: &str = "Permissions modification is invalid.";

/// One row of either table: what the grant names, and whether Lake Formation can record it.
struct ModificationCase {
    case: &'static str,
    resource: Value,
    permissions: Vec<&'static str>,
    with_grant_option: Vec<&'static str>,
    /// `true` when the row is a modification the service records, `false` when it is
    /// refused with [`PERMISSIONS_MODIFICATION_INVALID`].
    accepted: bool,
}

/// A Data Catalog holding the `sales` database with its `orders` table, with the grants
/// their CREATION made taken away again — the creator's Super and the
/// `IAM_ALLOWED_PRINCIPALS` defaults a database and its tables are created carrying — so a
/// listing reports only what a row's own grant wrote. Those grants have their own suites
/// (`creator_table_grants.rs`, `table_default_grants.rs`, `default_database_grant.rs`).
async fn seeded() -> MultiServiceRouter {
    let router = emulator_with_catalog(&[("sales", &["orders"])]).await;
    revoke_table_default_grants(&router, "sales", "orders").await;
    revoke_default_database_grant(&router, "sales").await;
    router
}

/// The permissions `principal` holds, across every record and every scope.
async fn held(router: &MultiServiceRouter, case: &str, principal: &str) -> BTreeSet<String> {
    let listed = list_permissions(
        router,
        case,
        &json!({ "Principal": { "DataLakePrincipalIdentifier": principal } }),
    )
    .await;
    listed
        .iter()
        .flat_map(|entry| {
            let permissions = entry
                .get("Permissions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let with_grant_option = entry
                .get("PermissionsWithGrantOption")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            permissions
                .into_iter()
                .chain(with_grant_option)
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect::<Vec<String>>()
        })
        .collect()
}

/// Drives one row through `GrantPermissions`, asserting the status, the error shape, the
/// whole message and what the grantee holds afterwards.
async fn assert_row(router: &MultiServiceRouter, row: &ModificationCase, principal: &str) {
    let body = json!({
        "Principal": { "DataLakePrincipalIdentifier": principal },
        "Resource": row.resource,
        "Permissions": row.permissions,
        "PermissionsWithGrantOption": row.with_grant_option,
    });
    let (status, error_type, payload) =
        lake_formation_response(router, "GrantPermissions", &body).await;
    let case = row.case;

    if row.accepted {
        assert_eq!(
            status, 200,
            "case {case:?}: expected GrantPermissions to answer 200, got {status} {payload:?}"
        );
        // A grant the service CAN record is readable back, which is what makes the
        // refusals above about the combination rather than about the call being refused
        // everywhere.
        let expected: BTreeSet<String> = row
            .permissions
            .iter()
            .chain(row.with_grant_option.iter())
            .map(|value| (*value).to_string())
            .collect();
        let stored = held(router, case, principal).await;
        assert_eq!(
            stored, expected,
            "case {case:?}: expected the grantee to hold {expected:?}, got {stored:?}"
        );
        return;
    }

    assert_eq!(
        status, 400,
        "case {case:?}: expected GrantPermissions to answer 400, got {status} {payload:?}"
    );
    assert_eq!(
        error_type, "InvalidInputException",
        "case {case:?}: expected x-amzn-errortype to name InvalidInputException, got \
         {error_type:?}"
    );
    assert_eq!(
        error_code(&payload),
        "InvalidInputException",
        "case {case:?}: expected the modelled InvalidInputException, got {payload:?}"
    );
    let message = payload.get("Message").and_then(Value::as_str);
    assert_eq!(
        message,
        Some(PERMISSIONS_MODIFICATION_INVALID),
        "case {case:?}: expected Message {PERMISSIONS_MODIFICATION_INVALID:?}, got {message:?}"
    );
    let stored = held(router, case, principal).await;
    assert!(
        stored.is_empty(),
        "case {case:?}: a refused grant records nothing, got {stored:?}"
    );

    // The batch path reads and plans a grant the same way, so the same entry is reported
    // as failed, with the same error class and the same sentence.
    let mut entry = body.clone();
    entry["Id"] = json!("one");
    let (status, payload) = call(
        router,
        "BatchGrantPermissions",
        &json!({ "Entries": [entry] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected BatchGrantPermissions to answer 200, got {status} {payload:?}"
    );
    let failures = payload
        .get("Failures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Failures list, got {payload:?}"));
    let reported: Vec<(Option<&str>, Option<&str>)> = failures
        .iter()
        .map(|failure| {
            let error = failure.get("Error");
            (
                error
                    .and_then(|error| error.get("ErrorCode"))
                    .and_then(Value::as_str),
                error
                    .and_then(|error| error.get("ErrorMessage"))
                    .and_then(Value::as_str),
            )
        })
        .collect();
    assert_eq!(
        reported,
        vec![(
            Some("InvalidInputException"),
            Some(PERMISSIONS_MODIFICATION_INVALID)
        )],
        "case {case:?}: expected the batch entry to be reported as failed with the same \
         refusal, got {failures:?}"
    );
}

/// The rows of the permission-to-resource-kind table.
///
/// The applicable sets are Lake Formation's privileges per kind of resource: a DATABASE
/// holds creating a table in it and altering, dropping and describing itself; a TABLE holds
/// reading and writing its data and altering, dropping and describing itself; a
/// `TableWithColumns` is "only used when granting a SELECT permission"
/// (`smithy_query(query="doc",shape="TableWithColumnsResource")`); a registered data
/// location holds access to it; the Data Catalog itself holds creating what lives in it; and
/// an LF-tag holds attaching it and the privileges over the tag. Every row names a value of
/// `Permission` (`smithy_query(query="shape",shape="Permission")`).
fn kind_cases() -> Vec<ModificationCase> {
    let table = json!({ "Table": { "DatabaseName": "sales", "Name": "orders" } });
    let database = json!({ "Database": { "Name": "sales" } });
    let location = json!({ "DataLocation": { "ResourceArn": "arn:aws:s3:::lake/raw" } });
    let columns = json!({
        "TableWithColumns": {
            "DatabaseName": "sales",
            "Name": "orders",
            "ColumnNames": ["order_id"],
        }
    });
    let tag = json!({ "LFTag": { "TagKey": "env", "TagValues": ["prod"] } });
    let tag_policy = json!({
        "LFTagPolicy": {
            "ResourceType": "TABLE",
            "Expression": [{ "TagKey": "env", "TagValues": ["prod"] }],
        }
    });

    vec![
        // Refused: a privilege over a thing the kind addressed does not have.
        ModificationCase {
            case: "a data-location permission on a table",
            resource: table.clone(),
            permissions: vec!["DATA_LOCATION_ACCESS"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a database-scoped create on a table",
            resource: table.clone(),
            permissions: vec!["CREATE_TABLE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a catalog-scoped create beside a table permission",
            resource: table.clone(),
            permissions: vec!["SELECT", "CREATE_DATABASE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a tag permission on a table",
            resource: table.clone(),
            permissions: vec!["ASSOCIATE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a catalog-wide role on a table",
            resource: table.clone(),
            permissions: vec!["SUPER_USER"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "an inapplicable permission named in both lists",
            resource: table.clone(),
            permissions: vec!["DATA_LOCATION_ACCESS"],
            with_grant_option: vec!["DATA_LOCATION_ACCESS"],
            accepted: false,
        },
        ModificationCase {
            case: "a table-scoped read on a database",
            resource: database.clone(),
            permissions: vec!["SELECT"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "table-scoped writes on a database",
            resource: database.clone(),
            permissions: vec!["INSERT", "DELETE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a data-location permission on a database",
            resource: database.clone(),
            permissions: vec!["DATA_LOCATION_ACCESS"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a table permission on a data location",
            resource: location.clone(),
            permissions: vec!["SELECT"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a non-column permission on a table with columns",
            resource: columns.clone(),
            permissions: vec!["DESCRIBE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a table permission on an lf-tag",
            resource: tag.clone(),
            permissions: vec!["SELECT"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a database-scoped create on a table-kind tag policy",
            resource: tag_policy.clone(),
            permissions: vec!["CREATE_TABLE"],
            with_grant_option: vec![],
            accepted: false,
        },
        ModificationCase {
            case: "a catalog-scoped create on a database",
            resource: database.clone(),
            permissions: vec!["CREATE_DATABASE"],
            with_grant_option: vec![],
            accepted: false,
        },
        // Accepted: the same permissions where the kind addressed DOES hold them.
        ModificationCase {
            case: "every table permission on a table",
            resource: table.clone(),
            permissions: vec!["SELECT", "INSERT", "DELETE", "DESCRIBE", "ALTER", "DROP"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the whole-resource permission on a table",
            resource: table,
            permissions: vec!["ALL"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "every database permission on a database",
            resource: database.clone(),
            permissions: vec!["CREATE_TABLE", "DESCRIBE", "ALTER", "DROP"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the whole-resource permission on a database",
            resource: database,
            permissions: vec!["ALL"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the location permission on a data location",
            resource: location,
            permissions: vec!["DATA_LOCATION_ACCESS"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the column-scoped permission on a table with columns",
            resource: columns,
            permissions: vec!["SELECT"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the tag permission on an lf-tag",
            resource: tag,
            permissions: vec!["ASSOCIATE"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "a table permission on a table-kind tag policy",
            resource: tag_policy,
            permissions: vec!["SELECT", "DESCRIBE"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "a catalog-scoped create on the catalog",
            resource: json!({ "Catalog": {} }),
            permissions: vec!["CREATE_DATABASE"],
            with_grant_option: vec![],
            accepted: true,
        },
    ]
}

#[tokio::test]
async fn a_permission_the_resource_kind_does_not_hold_is_refused() {
    for (index, row) in kind_cases().iter().enumerate() {
        // A fresh emulator per row, so what a row reads back can only be its own grant,
        // and one principal per row for the same reason.
        let router = seeded().await;
        let principal = format!("arn:aws:iam::{}:role/kind-{index}", account_catalog());
        assert_row(&router, row, &principal).await;
    }
}

/// The rows of the grant-option table: how the two permission lists overlap.
fn grant_option_cases() -> Vec<ModificationCase> {
    let table = json!({ "Table": { "DatabaseName": "sales", "Name": "orders" } });
    let database = json!({ "Database": { "Name": "sales" } });

    vec![
        ModificationCase {
            case: "the grant option for a permission that is not granted",
            resource: table.clone(),
            permissions: vec!["SELECT"],
            with_grant_option: vec!["DESCRIBE"],
            accepted: false,
        },
        ModificationCase {
            case: "the grant option alone",
            resource: table.clone(),
            permissions: vec![],
            with_grant_option: vec!["SELECT"],
            accepted: false,
        },
        ModificationCase {
            case: "the grant option naming more than is granted",
            resource: table.clone(),
            permissions: vec!["SELECT", "DESCRIBE"],
            with_grant_option: vec!["SELECT", "DESCRIBE", "ALTER"],
            accepted: false,
        },
        ModificationCase {
            case: "the grant option for another database permission",
            resource: database.clone(),
            permissions: vec!["DESCRIBE"],
            with_grant_option: vec!["ALTER"],
            accepted: false,
        },
        // Accepted: a grant option that IS a subset of what the grant grants — a strict
        // subset, the whole set, and the empty one.
        ModificationCase {
            case: "the grant option for one of the granted permissions",
            resource: table.clone(),
            permissions: vec!["SELECT", "DESCRIBE"],
            with_grant_option: vec!["SELECT"],
            accepted: true,
        },
        ModificationCase {
            case: "the grant option for every granted permission",
            resource: table.clone(),
            permissions: vec!["SELECT", "DESCRIBE"],
            with_grant_option: vec!["SELECT", "DESCRIBE"],
            accepted: true,
        },
        ModificationCase {
            case: "no grant option at all",
            resource: table,
            permissions: vec!["SELECT", "DESCRIBE"],
            with_grant_option: vec![],
            accepted: true,
        },
        ModificationCase {
            case: "the grant option for the one granted database permission",
            resource: database,
            permissions: vec!["DESCRIBE"],
            with_grant_option: vec!["DESCRIBE"],
            accepted: true,
        },
    ]
}

#[tokio::test]
async fn the_grant_option_may_only_name_a_permission_the_grant_grants() {
    for (index, row) in grant_option_cases().iter().enumerate() {
        let router = seeded().await;
        let principal = format!("arn:aws:iam::{}:role/option-{index}", account_catalog());
        assert_row(&router, row, &principal).await;
    }
}

#[test]
fn every_privilege_set_names_only_permissions_the_model_carries() {
    use service_com_amazonaws_lakeformation_awslakeformation::state::{
        CATALOG_PERMISSIONS, DATABASE_PERMISSIONS, DATA_LOCATION_PERMISSIONS,
        FILTERED_TABLE_PERMISSIONS, LF_TAG_EXPRESSION_PERMISSIONS, LF_TAG_PERMISSIONS,
        TABLE_PERMISSIONS,
    };

    /// Every value of `Permission` (`smithy_query(query="shape",shape="Permission")`),
    /// spelled out here rather than read back out of the crate.
    const MODELLED: [&str; 16] = [
        "ALL",
        "SELECT",
        "ALTER",
        "DROP",
        "DELETE",
        "INSERT",
        "DESCRIBE",
        "CREATE_DATABASE",
        "CREATE_TABLE",
        "DATA_LOCATION_ACCESS",
        "CREATE_LF_TAG",
        "ASSOCIATE",
        "GRANT_WITH_LF_TAG_EXPRESSION",
        "CREATE_LF_TAG_EXPRESSION",
        "CREATE_CATALOG",
        "SUPER_USER",
    ];

    // A value outside the enum in one of these sets would make a modelled permission
    // unreachable on that kind of resource instead of failing, so every entry is held to
    // the enum — and every set carries the whole-resource permission, which Lake Formation
    // holds on every kind.
    let sets: [(&str, &[&str]); 7] = [
        ("Catalog", CATALOG_PERMISSIONS),
        ("Database", DATABASE_PERMISSIONS),
        ("Table", TABLE_PERMISSIONS),
        ("DataLocation", DATA_LOCATION_PERMISSIONS),
        ("LFTag", LF_TAG_PERMISSIONS),
        ("LFTagExpression", LF_TAG_EXPRESSION_PERMISSIONS),
        ("DataCellsFilter/RLSPolicy", FILTERED_TABLE_PERMISSIONS),
    ];
    for (case, set) in sets {
        for permission in set {
            assert!(
                MODELLED.contains(permission),
                "case {case:?}: {permission:?} is not a value of Permission, so it names no \
                 permission at all"
            );
        }
        let distinct: BTreeSet<&&str> = set.iter().collect();
        assert_eq!(
            distinct.len(),
            set.len(),
            "case {case:?}: expected no permission named twice, got {set:?}"
        );
        assert!(
            set.contains(&"ALL"),
            "case {case:?}: expected the whole-resource permission to be held on every kind, \
             got {set:?}"
        );
    }
}

#[tokio::test]
async fn the_invalid_modification_body_carries_the_message_member_and_nothing_else() {
    // restJson1 names the error SHAPE in the `x-amzn-errortype` header, and
    // `InvalidInputException` declares the single member `Message`, so the body of this
    // refusal is exactly that one key. The rows differ along which rule refused the grant.
    let router = seeded().await;
    let catalog = account_catalog();
    let cases: Vec<(&str, Value)> = vec![
        (
            "a permission the resource kind does not hold",
            json!({
                "Principal": { "DataLakePrincipalIdentifier": format!("arn:aws:iam::{catalog}:role/body-kind") },
                "Resource": { "Table": { "DatabaseName": "sales", "Name": "orders" } },
                "Permissions": ["CREATE_TABLE"],
            }),
        ),
        (
            "a grant option for a permission that is not granted",
            json!({
                "Principal": { "DataLakePrincipalIdentifier": format!("arn:aws:iam::{catalog}:role/body-option") },
                "Resource": { "Table": { "DatabaseName": "sales", "Name": "orders" } },
                "Permissions": ["SELECT"],
                "PermissionsWithGrantOption": ["ALTER"],
            }),
        ),
    ];

    for (case, body) in cases {
        let (status, error_type, raw) =
            lake_formation_wire(&router, "GrantPermissions", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {:?}",
            String::from_utf8_lossy(&raw)
        );
        assert_eq!(
            error_type, "InvalidInputException",
            "case {case:?}: expected x-amzn-errortype to name InvalidInputException, got \
             {error_type:?}"
        );
        let parsed: Value = serde_json::from_slice(&raw).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the refusal body must be JSON, got {:?} ({error})",
                String::from_utf8_lossy(&raw)
            )
        });
        assert_eq!(
            parsed,
            json!({ "Message": PERMISSIONS_MODIFICATION_INVALID }),
            "case {case:?}: expected a body of exactly one Message member, got {parsed:?}"
        );
    }
}
