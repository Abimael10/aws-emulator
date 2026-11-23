// SPDX-License-Identifier: CC0-1.0

//! What a metadata read answers across a DROP and a RECREATE of the same table in the
//! Data Catalog.
//!
//! The rule under test: Athena registers no tables of its own, so `GetTableMetadata` and
//! `ListTableMetadata` resolve their address by CALLING the Data Catalog on every call and
//! report whatever that catalog holds AT THAT MOMENT — a table the catalog holds is served
//! from the definition the catalog currently holds for it, a table the catalog no longer
//! holds comes back as that catalog's own `EntityNotFoundException`, relayed as
//! `MetadataException` whose message is the catalog's sentence followed by the clause the
//! relaying SDK layer states around a caught service exception (the component that
//! answered, the status it raised, the error code it answered under, that call's own
//! request id and the proxy it came through), and a table created again under the same
//! address is served again, from the NEW definition. So nothing of a dropped table
//! survives the drop, and a client parsing the clause recovers the underlying
//! `EntityNotFoundException` for the window the table is gone.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: how the
//! catalog is addressed (its own name, that name lowercased, a registered `GLUE` catalog
//! naming the same catalog id), how the table name is spelled (lower case, mixed case, one
//! character), what the recreated definition changes (its column set, its column types,
//! its table type, whether it is partitioned) and how many drop/recreate cycles the same
//! address goes through. The recorded case — one `GetTableMetadata` after a Glue drop,
//! then the same call after the recreate — is one row among them.
//!
//! The wire path is the one the metadata suites beside this one mount
//! ([`crate::table_metadata`]): Athena and Glue on ONE shared engine, reached through the
//! same `MultiServiceRouter` the production front door uses, so every table here is
//! created and dropped over GLUE's real wire and every Athena call goes over Athena's own.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{error_code, message};
use crate::relayed_catalog_refusals::{register_second_name, relayed_request_id, TABLE_MISS};
use crate::table_metadata::{
    athena, athena_ok, emulator, glue_ok, reported_names, GLUE_CATALOG,
};

/// How one row addresses the data catalog the read goes to.
#[derive(Debug, Clone, Copy)]
enum Addressing {
    /// `AwsDataCatalog`, the name Athena reaches the account's Glue Data Catalog by.
    OwnName,
    /// That same name lowercased, which addresses the same catalog.
    Lowercased,
    /// A `GLUE` data catalog the client REGISTERED, naming this account's catalog id, so a
    /// second name reaches the very same catalog.
    RegisteredSecondName,
}

/// The name a registered `GLUE` catalog of this account's catalog id is created under.
const SECOND_NAME: &str = "cycle_second_name";

impl Addressing {
    /// The `CatalogName` a request carries for this addressing.
    fn request_name(self) -> &'static str {
        match self {
            Self::OwnName => GLUE_CATALOG,
            Self::Lowercased => "awsdatacatalog",
            Self::RegisteredSecondName => SECOND_NAME,
        }
    }
}

/// One drop-and-recreate cycle: the database and table it runs over, the definition the
/// table is first created with and the DIFFERENT definition it is created again with.
struct Cycle {
    case: &'static str,
    catalog: Addressing,
    /// The database, as the creating Glue call and the reading Athena call spell it.
    database: &'static str,
    /// The table, as the creating Glue call and the reading Athena call spell it.
    table: &'static str,
    /// The `TableInput` the table is created with before the drop.
    first: Value,
    /// The `TableInput` it is created with after the drop, differing from `first` in what
    /// the read must then report.
    again: Value,
}

/// A `TableInput` with `name` and the given `(column name, column type)` columns, under
/// `table_type`.
///
/// Columns carry only the members Athena's `Column` reports (its name and its type), so
/// what the catalog was given is exactly what the read must come back with.
fn table_input(name: &str, table_type: &str, columns: &[(&str, &str)]) -> Value {
    json!({
        "Name": name,
        "TableType": table_type,
        "StorageDescriptor": {
            "Columns": columns
                .iter()
                .map(|(column, kind)| json!({ "Name": column, "Type": kind }))
                .collect::<Vec<Value>>()
        }
    })
}

/// The same `TableInput` with one partition key added, so a recreate can differ in whether
/// the table is partitioned at all.
fn partitioned_by(mut input: Value, key: &str, kind: &str) -> Value {
    input["PartitionKeys"] = json!([{ "Name": key, "Type": kind }]);
    input
}

/// The rows, differing along what the rule generalises over: the addressing of the
/// catalog, the spelling of the table name, and what the recreated definition changes.
fn cycles() -> Vec<Cycle> {
    vec![
        Cycle {
            case: "the recorded case: a table dropped and created again with a new column \
                   set",
            catalog: Addressing::OwnName,
            database: "cycle_db",
            table: "sample_data",
            first: table_input(
                "sample_data",
                "EXTERNAL_TABLE",
                &[("id", "string"), ("amount", "double")],
            ),
            again: table_input(
                "sample_data",
                "EXTERNAL_TABLE",
                &[("id", "string"), ("amount", "double"), ("dt", "date")],
            ),
        },
        Cycle {
            case: "a mixed-case table name, recreated with a different column type",
            catalog: Addressing::OwnName,
            database: "Cycle_Mixed_DB",
            table: "Sales_By_Day",
            first: table_input("Sales_By_Day", "EXTERNAL_TABLE", &[("region", "string")]),
            again: table_input(
                "Sales_By_Day",
                "EXTERNAL_TABLE",
                &[("region", "varchar(20)")],
            ),
        },
        Cycle {
            case: "a one-character table name, recreated under a different table type",
            catalog: Addressing::Lowercased,
            database: "cycle_short_db",
            table: "x",
            first: table_input("x", "EXTERNAL_TABLE", &[("v", "int")]),
            again: table_input("x", "GOVERNED", &[("v", "int")]),
        },
        Cycle {
            case: "read through a registered GLUE catalog of the same catalog id, \
                   recreated as a partitioned table",
            catalog: Addressing::RegisteredSecondName,
            database: "cycle_second_db",
            table: "events",
            first: table_input("events", "EXTERNAL_TABLE", &[("id", "string")]),
            again: partitioned_by(
                table_input("events", "EXTERNAL_TABLE", &[("id", "string")]),
                "dt",
                "string",
            ),
        },
        Cycle {
            case: "a table recreated with no columns at all",
            catalog: Addressing::OwnName,
            database: "cycle_empty_db",
            table: "shrinking",
            first: table_input("shrinking", "EXTERNAL_TABLE", &[("id", "string")]),
            again: json!({ "Name": "shrinking", "TableType": "EXTERNAL_TABLE" }),
        },
    ]
}

/// The table name as the Hive-compatible catalog KEYS it, which is the name a read reports
/// — rendered through the catalog's own fold rather than a folding of this suite's own.
fn folded(name: &str) -> String {
    ::service_com_amazonaws_glue_awsglue::state::fold_name(name)
}

/// The `TableMetadata` members a definition decides, as the read must report them: the
/// folded name, the table type, the columns, and the partition keys where the definition
/// has any (a member the catalog holds nothing for is absent rather than reported empty).
fn expected_metadata(name: &str, input: &Value) -> Value {
    let mut expected = json!({ "Name": folded(name) });
    for (member, from) in [
        ("TableType", input.get("TableType")),
        ("Columns", input.pointer("/StorageDescriptor/Columns")),
        ("PartitionKeys", input.get("PartitionKeys")),
    ] {
        if let Some(value) = from {
            expected[member] = value.clone();
        }
    }
    expected
}

/// The reported metadata narrowed to the members a definition decides, so the comparison
/// states the definition and not the instants the catalog sets.
fn reported_metadata(case: &str, body: &Value) -> Value {
    let table = body
        .get("TableMetadata")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TableMetadata, got {body}"));
    let mut reported = json!({ "Name": table["Name"].clone() });
    for member in ["TableType", "Columns", "PartitionKeys"] {
        if let Some(value) = table.get(member) {
            reported[member] = value.clone();
        }
    }
    reported
}

/// Assert that the single-table read of this address is the relayed catalog miss, and that
/// the listing of its database no longer names it.
async fn assert_dropped(
    router: &MultiServiceRouter,
    case: &str,
    catalog: &str,
    database: &str,
    table: &str,
) {
    let (status, body) = athena(
        router,
        "GetTableMetadata",
        &json!({
            "CatalogName": catalog,
            "DatabaseName": database,
            "TableName": table
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for the dropped table, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "MetadataException",
        "case {case:?}: expected MetadataException for the dropped table, got {body}"
    );
    relayed_request_id(case, &message(&body), TABLE_MISS);

    let listing = athena_ok(
        router,
        "ListTableMetadata",
        &json!({ "CatalogName": catalog, "DatabaseName": database }),
    )
    .await;
    let names = reported_names(&listing);
    assert!(
        !names.contains(&folded(table)),
        "case {case:?}: expected the dropped table gone from the listing, got {names:?}"
    );
}

/// Read the table back and assert it is served from `definition`, and that the listing of
/// its database names it.
async fn assert_served(
    router: &MultiServiceRouter,
    case: &str,
    catalog: &str,
    database: &str,
    table: &str,
    definition: &Value,
) {
    let body = athena_ok(
        router,
        "GetTableMetadata",
        &json!({
            "CatalogName": catalog,
            "DatabaseName": database,
            "TableName": table
        }),
    )
    .await;
    let expected = expected_metadata(table, definition);
    let reported = reported_metadata(case, &body);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the metadata {expected}, got {reported}"
    );

    let listing = athena_ok(
        router,
        "ListTableMetadata",
        &json!({ "CatalogName": catalog, "DatabaseName": database }),
    )
    .await;
    let names = reported_names(&listing);
    assert!(
        names.contains(&folded(table)),
        "case {case:?}: expected the listing to name the table, got {names:?}"
    );
}

/// A metadata read follows the catalog across a drop and a recreate: served, then relayed
/// as the catalog's own miss, then served again from the NEW definition.
#[tokio::test]
async fn a_metadata_read_follows_a_catalog_drop_and_recreate() {
    let router = emulator();
    // The second name is registered through the sibling suite's one registration path, so
    // there is a single place a `GLUE` catalog of this account's catalog id is created.
    register_second_name(&router, SECOND_NAME).await;

    for cycle in cycles() {
        let case = cycle.case;
        let catalog = cycle.catalog.request_name();
        glue_ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": cycle.database } }),
        )
        .await;
        glue_ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": cycle.database, "TableInput": cycle.first }),
        )
        .await;

        assert_served(
            &router,
            case,
            catalog,
            cycle.database,
            cycle.table,
            &cycle.first,
        )
        .await;

        glue_ok(
            &router,
            "DeleteTable",
            &json!({ "DatabaseName": cycle.database, "Name": cycle.table }),
        )
        .await;
        assert_dropped(&router, case, catalog, cycle.database, cycle.table).await;

        glue_ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": cycle.database, "TableInput": cycle.again }),
        )
        .await;
        assert_served(
            &router,
            case,
            catalog,
            cycle.database,
            cycle.table,
            &cycle.again,
        )
        .await;
    }
}

/// The same address goes through the cycle repeatedly: every drop is relayed as the
/// catalog's own miss and every recreate is served from the definition of THAT recreate,
/// so no answer is remembered from an earlier turn of the cycle.
#[tokio::test]
async fn each_turn_of_a_repeated_cycle_answers_from_the_catalog_as_it_then_stands() {
    let router = emulator();
    let database = "repeated_cycle_db";
    let table = "ledger";
    glue_ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;

    // Each turn adds one column, so the definition served differs on every turn and an
    // answer carried over from an earlier turn cannot pass.
    let all_columns = [("id", "string"), ("amount", "double"), ("dt", "date")];
    for turn in 1..=all_columns.len() {
        let case = format!("turn {turn}");
        let definition = table_input(table, "EXTERNAL_TABLE", &all_columns[..turn]);
        glue_ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": database, "TableInput": definition }),
        )
        .await;
        assert_served(&router, &case, GLUE_CATALOG, database, table, &definition).await;

        glue_ok(
            &router,
            "DeleteTable",
            &json!({ "DatabaseName": database, "Name": table }),
        )
        .await;
        assert_dropped(&router, &case, GLUE_CATALOG, database, table).await;
    }
}
