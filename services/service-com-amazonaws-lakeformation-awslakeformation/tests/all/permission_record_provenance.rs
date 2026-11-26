// SPDX-License-Identifier: CC0-1.0

//! End-to-end suite for the update PROVENANCE a `ListPermissions` entry reports: none.
//!
//! RULE: `PrincipalResourcePermissions$LastUpdated` ("The date and time when the resource was
//! last updated") and `$LastUpdatedBy` ("The user who updated the record") are optional
//! members with no default
//! (`smithy_query(query="shape",shape="PrincipalResourcePermissions")`), and no operation of
//! this API carries a value for either — neither `GrantPermissions` and its batch form nor
//! `RevokePermissions` and its batch form declare a member for them — so a listing reports a
//! record WITHOUT that provenance, whichever write made the record and whatever else the
//! entry carries.
//!
//! The rows span what the rule generalises over: the WRITE PATH that made the record
//! (`GrantPermissions`, `BatchGrantPermissions`, a second grant that added to a record, the
//! revoke paths that trim one, and the create of the resource for a grant it was created
//! carrying), the resource FORM the record is held on (a database, a table, a table's
//! columns), and the FILTER the listing narrows by (the principal, the resource, nothing at
//! all) — because the projection is one for every entry of every page.
//!
//! The rule is about the ABSENCE of members, so each row also asserts the members a record
//! does carry are reported, which is what tells "nothing to report provenance from" apart
//! from "nothing reported at all".

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::glue_catalog::{
    account_catalog, create_database_with_input, create_table, emulator, lake_formation as call,
    list_permissions as list,
};

/// The database every row grants on, created handing its tables no default permissions, so
/// what a listing reports is this suite's own grants and what the table's creation made.
const DATABASE: &str = "ledger";

/// Its table.
const TABLE: &str = "entries";

/// The two members a listed entry never carries.
const PROVENANCE: [&str; 2] = ["LastUpdated", "LastUpdatedBy"];

/// An emulator whose catalog holds [`DATABASE`] and its [`TABLE`].
async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_database_with_input(
        &router,
        &json!({ "Name": DATABASE, "CreateTableDefaultPermissions": [] }),
    )
    .await;
    create_table(&router, DATABASE, TABLE).await;
    router
}

/// The resource forms a record is held on.
fn on_database() -> Value {
    json!({ "Database": { "Name": DATABASE } })
}

fn on_table() -> Value {
    json!({ "Table": { "DatabaseName": DATABASE, "Name": TABLE } })
}

fn on_columns() -> Value {
    json!({
        "TableWithColumns": { "DatabaseName": DATABASE, "Name": TABLE, "ColumnWildcard": {} }
    })
}

/// Assert every entry of `listed` reports no provenance, and that each still reports the
/// members its record holds.
fn assert_entries_without_provenance(case: &str, listed: &[Value]) {
    assert!(
        !listed.is_empty(),
        "case {case:?}: expected the listing to report the record, got no entry"
    );
    for entry in listed {
        for member in PROVENANCE {
            assert_eq!(
                entry.get(member),
                None,
                "case {case:?}: expected no {member} on the entry, got {entry:?}"
            );
        }
        assert!(
            entry.get("Principal").is_some()
                && entry.get("Resource").is_some()
                && entry
                    .get("Permissions")
                    .and_then(Value::as_array)
                    .is_some_and(|held| !held.is_empty()),
            "case {case:?}: expected the entry to report what the record holds, got {entry:?}"
        );
    }
}

/// Call `operation` with `body`, asserting the service accepted it.
async fn accepted(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    let (status, payload) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to be accepted, got {status} {payload:?}"
    );
}

/// One row: the case, the resource form the record is held on, and the calls that make the
/// record — each an operation name and the body it sends.
type Row = (&'static str, Value, Vec<(&'static str, Value)>);

#[tokio::test]
async fn no_write_path_leaves_a_record_reporting_provenance() {
    let catalog = account_catalog();
    let analyst = format!("arn:aws:iam::{catalog}:role/analyst");

    // Each row: the case, the resource form the record is held on, and the calls that make
    // the record — the write paths this API has for writing one.
    let rows: Vec<Row> = vec![
        (
            "a database record one grant wrote",
            on_database(),
            vec![(
                "GrantPermissions",
                json!({
                    "Principal": { "DataLakePrincipalIdentifier": analyst },
                    "Resource": on_database(),
                    "Permissions": ["DESCRIBE"],
                }),
            )],
        ),
        (
            "a table record a batch grant wrote",
            on_table(),
            vec![(
                "BatchGrantPermissions",
                json!({ "Entries": [{
                    "Id": "one",
                    "Principal": { "DataLakePrincipalIdentifier": analyst },
                    "Resource": on_table(),
                    "Permissions": ["DESCRIBE"],
                }]}),
            )],
        ),
        (
            "a column record one grant wrote",
            on_columns(),
            vec![(
                "GrantPermissions",
                json!({
                    "Principal": { "DataLakePrincipalIdentifier": analyst },
                    "Resource": on_columns(),
                    "Permissions": ["SELECT"],
                }),
            )],
        ),
        (
            "a table record a second grant added to",
            on_table(),
            vec![
                (
                    "GrantPermissions",
                    json!({
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["DESCRIBE"],
                    }),
                ),
                (
                    "GrantPermissions",
                    json!({
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["ALTER"],
                    }),
                ),
            ],
        ),
        (
            "a table record a revoke trimmed",
            on_table(),
            vec![
                (
                    "GrantPermissions",
                    json!({
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["DESCRIBE", "ALTER"],
                    }),
                ),
                (
                    "RevokePermissions",
                    json!({
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["ALTER"],
                    }),
                ),
            ],
        ),
        (
            "a table record a batch revoke trimmed",
            on_table(),
            vec![
                (
                    "GrantPermissions",
                    json!({
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["DESCRIBE", "ALTER"],
                    }),
                ),
                (
                    "BatchRevokePermissions",
                    json!({ "Entries": [{
                        "Id": "one",
                        "Principal": { "DataLakePrincipalIdentifier": analyst },
                        "Resource": on_table(),
                        "Permissions": ["ALTER"],
                    }]}),
                ),
            ],
        ),
    ];

    for (case, resource, writes) in rows {
        let router = seeded().await;
        for (operation, body) in &writes {
            accepted(&router, case, operation, body).await;
        }

        // Read back under each filter a listing can be narrowed by, since the projection is
        // one for every entry of every answer.
        for (filter_case, filter) in [
            (
                "by principal",
                json!({ "Principal": { "DataLakePrincipalIdentifier": analyst } }),
            ),
            ("by resource", json!({ "Resource": resource })),
            ("unfiltered", json!({})),
        ] {
            let case = format!("{case}, listed {filter_case}");
            let listed = list(&router, &case, &filter).await;
            assert_entries_without_provenance(&case, &listed);
        }
    }
}

#[tokio::test]
async fn a_record_a_resource_was_created_carrying_reports_no_provenance() {
    // The records no call of this API wrote: creating the table grants its creator Super,
    // and creating a database grants the IAM-access-control group. The create supplies no
    // provenance for a permission record either, so these entries carry none.
    let router = emulator();
    create_database_with_input(&router, &json!({ "Name": DATABASE })).await;
    create_table(&router, DATABASE, TABLE).await;

    for (case, filter) in [
        ("the records a table's creation made", json!({ "Resource": on_table() })),
        (
            "the record a database's creation made",
            json!({ "Resource": on_database() }),
        ),
        ("every creation-time record", json!({})),
    ] {
        let listed = list(&router, case, &filter).await;
        assert_entries_without_provenance(case, &listed);
    }
}
