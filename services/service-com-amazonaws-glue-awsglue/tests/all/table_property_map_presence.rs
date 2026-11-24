// SPDX-License-Identifier: CC0-1.0

//! Whether a projected table's `Parameters` member is THERE AT ALL, over every read that
//! projects a stored table and across a read-modify-write round trip.
//!
//! `Table$Parameters` ("these key-value pairs define properties associated with the table")
//! targets `ParametersMap` and carries neither `@required` nor `@default` (source:
//! `smithy_query shape=Table,TableInput` on this crate's model), and so do
//! `StorageDescriptor$Parameters` and `SerDeInfo$Parameters`. The catalog therefore reports
//! such a map only where the definition DECLARED one: a definition that declared none is
//! served with the member missing, and one that declared an empty map is served with an
//! empty map. The presence of the member — not its contents — is what a client branches on,
//! which is how an Apache Iceberg marker is probed (`"Parameters" in table`, then
//! `table["Parameters"]["table_type"]`): a table that declares no property at all is not an
//! Iceberg table, and must be distinguishable from one that declares an empty property set.
//!
//! What the maps of a definition must CONTAIN, over the single-table read, the table listing
//! and the version read, and over a partition's own maps, is
//! `tests/all/definition_parameter_maps.rs`. This suite adds the readers that one does not
//! drive — the search, the version listing, the batch partition read and the Lake Formation
//! application-integration read — and the round trip that feeds a read back through
//! `UpdateTable`, where materialising an empty map would write a property set the client never
//! declared. Every row is driven through ALL SIX readers of a projected table, the three the
//! other suite also covers included, because the point is that no reader can answer
//! differently from the rest.
//!
//! The rows differ along what the rule generalises over: which of the three maps the
//! definition declares, whether a declaration is absent or an explicitly empty map, how many
//! entries a declared map carries, whether the definition describes storage and a SerDe at
//! all, and whether the declared properties carry the Iceberg marker. The recorded case (a
//! table registered with no properties anywhere, probed for the marker) is the first row.
//! Every call goes over the wire through the shared [`crate::wire`] harness.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{ok, emulator, ACCOUNT_ID};

/// The database every row of this suite registers its table in.
const DATABASE: &str = "marker_db";

/// The table property an Iceberg table is recognised by, and the value that marks one. A
/// client's marker probe reads exactly this key out of `Table$Parameters`.
const ICEBERG_MARKER: (&str, &str) = ("table_type", "ICEBERG");

/// What a read must report for ONE `Parameters` map.
#[derive(Clone)]
enum Declared {
    /// The definition declared no such map, so the member is not in the response at all.
    Nothing,
    /// The definition declared this map, so the member is in the response carrying it.
    Map(Value),
}

/// One row: a table definition, and what every read of it must report for its three
/// `Parameters` maps.
struct Row {
    case: &'static str,
    /// The `TableInput` the row registers.
    input: Value,
    /// `Table.Parameters`.
    table: Declared,
    /// `Table.StorageDescriptor.Parameters`, or `None` when the definition describes no
    /// storage at all.
    storage: Option<Declared>,
    /// `Table.StorageDescriptor.SerdeInfo.Parameters`, or `None` when the described storage
    /// names no SerDe.
    serde: Option<Declared>,
    /// Whether a client's Iceberg-marker probe must call this table marked.
    marked: bool,
}

/// A map of `entries` properties, for the rows that move the entry COUNT.
fn properties(entries: usize) -> Value {
    let map: serde_json::Map<String, Value> = (0..entries)
        .map(|index| (format!("property_{index}"), json!(format!("value_{index}"))))
        .collect();
    Value::Object(map)
}

/// The rows of this suite.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: a located, SerDe-described external table registered with
            // no property map anywhere, which a marker probe must call unmarked.
            case: "nothing declares a property map",
            input: json!({
                "Name": "no_markers",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://lake/no_markers/",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
            table: Declared::Nothing,
            storage: Some(Declared::Nothing),
            serde: Some(Declared::Nothing),
            marked: false,
        },
        Row {
            case: "the table declares the Iceberg marker and nothing else does",
            input: json!({
                "Name": "marked",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": {
                    ICEBERG_MARKER.0: ICEBERG_MARKER.1,
                    "metadata_location": "s3://lake/marked/metadata/00000-a.metadata.json"
                },
                "StorageDescriptor": { "Location": "s3://lake/marked/" }
            }),
            table: Declared::Map(json!({
                ICEBERG_MARKER.0: ICEBERG_MARKER.1,
                "metadata_location": "s3://lake/marked/metadata/00000-a.metadata.json"
            })),
            storage: Some(Declared::Nothing),
            serde: None,
            marked: true,
        },
        Row {
            // A map the write DECLARED comes back as declared, with nothing in it: an empty
            // property set is a property set, and a probe must tell it from no map at all.
            case: "every map declared explicitly empty",
            input: json!({
                "Name": "explicitly_empty_maps",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": {},
                "StorageDescriptor": {
                    "Location": "s3://lake/explicitly_empty_maps/",
                    "Parameters": {},
                    "SerdeInfo": { "Parameters": {} }
                }
            }),
            table: Declared::Map(json!({})),
            storage: Some(Declared::Map(json!({}))),
            serde: Some(Declared::Map(json!({}))),
            marked: false,
        },
        Row {
            case: "only the described storage and its SerDe declare properties",
            input: json!({
                "Name": "storage_properties_only",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://lake/storage_properties_only/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            table: Declared::Nothing,
            storage: Some(Declared::Map(json!({ "skip.header.line.count": "1" }))),
            serde: Some(Declared::Map(json!({ "field.delim": "," }))),
            marked: false,
        },
        Row {
            case: "nine properties on the table, which describes no storage",
            input: json!({
                "Name": "many_properties",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": properties(9)
            }),
            table: Declared::Map(properties(9)),
            storage: None,
            serde: None,
            marked: false,
        },
    ]
}

/// Assert one projected definition's `Parameters` member is what the definition declared,
/// naming the read that answered it and the member under test.
fn assert_declared(case: &str, reader: &str, member: &str, held: &Value, want: &Declared) {
    let reported = held.get("Parameters");
    match want {
        Declared::Nothing => assert!(
            reported.is_none(),
            "case {case}: {reader} must report no {member} for a definition that declared \
             none, got {reported:?}"
        ),
        Declared::Map(map) => assert_eq!(
            reported,
            Some(map),
            "case {case}: {reader} must report {member} {map}, got {reported:?}"
        ),
    }
}

/// Assert the three maps of one projected table, naming the read that answered it.
fn assert_row(case: &str, reader: &str, table: &Value, row: &Row) {
    assert_declared(case, reader, "Table.Parameters", table, &row.table);
    match &row.storage {
        None => assert!(
            table["StorageDescriptor"].is_null(),
            "case {case}: {reader} must describe no storage for a definition that describes \
             none, got {:?}",
            table["StorageDescriptor"]
        ),
        Some(storage) => assert_declared(
            case,
            reader,
            "StorageDescriptor.Parameters",
            &table["StorageDescriptor"],
            storage,
        ),
    }
    match &row.serde {
        None => assert!(
            table["StorageDescriptor"]["SerdeInfo"].is_null(),
            "case {case}: {reader} must report no SerdeInfo for storage that names none, got \
             {:?}",
            table["StorageDescriptor"]["SerdeInfo"]
        ),
        Some(serde) => assert_declared(
            case,
            reader,
            "SerdeInfo.Parameters",
            &table["StorageDescriptor"]["SerdeInfo"],
            serde,
        ),
    }

    // The client-side consequence the presence rule exists for: a marker probe reads the
    // member first, and only then the key inside it.
    let probed = table
        .get("Parameters")
        .and_then(|declared| declared.get(ICEBERG_MARKER.0))
        .and_then(Value::as_str)
        == Some(ICEBERG_MARKER.1);
    assert_eq!(
        probed, row.marked,
        "case {case}: a marker probe over {reader}'s projection must call the table \
         marked={}, got marked={probed} from {table}",
        row.marked
    );
}

/// Find the element of `list` naming `table`, or fail naming the reader that answered it.
fn element_for(case: &str, reader: &str, list: &Value, member: &str, table: &str) -> Value {
    list[member]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: {reader} must answer a {member} list, got {list}"))
        .iter()
        .find(|element| element["Name"] == json!(table))
        .unwrap_or_else(|| panic!("case {case}: {table} must appear in {reader}'s {member}"))
        .clone()
}

/// Every read that projects a stored table, keyed by the name a failure names it with.
async fn projections(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<(&'static str, Value)> {
    let read = ok(
        router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    let projected = read["Table"].clone();
    let version_id = projected["VersionId"].clone();

    let listed = ok(router, "GetTables", &json!({ "DatabaseName": DATABASE })).await;
    let searched = ok(router, "SearchTables", &json!({})).await;
    let version = ok(
        router,
        "GetTableVersion",
        &json!({ "DatabaseName": DATABASE, "TableName": table }),
    )
    .await;
    let versions = ok(
        router,
        "GetTableVersions",
        &json!({ "DatabaseName": DATABASE, "TableName": table }),
    )
    .await;
    let current = versions["TableVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: GetTableVersions must answer a list, got {versions}"))
        .iter()
        .find(|entry| entry["VersionId"] == version_id)
        .unwrap_or_else(|| {
            panic!("case {case}: the current version {version_id} must be listed, got {versions}")
        })["Table"]
        .clone();
    // The Lake Formation application-integration read is vended by Lake Formation, so it
    // carries the caller's own data permissions on the table, which is what authorizes it
    // when no view context is found.
    let unfiltered = ok(
        router,
        "GetUnfilteredTableMetadata",
        &json!({
            "CatalogId": ACCOUNT_ID,
            "DatabaseName": DATABASE,
            "Name": table,
            "SupportedPermissionTypes": ["COLUMN_PERMISSION"],
            "Permissions": ["SELECT"],
        }),
    )
    .await;

    vec![
        ("GetTable", projected),
        (
            "GetTables",
            element_for(case, "GetTables", &listed, "TableList", table),
        ),
        (
            "SearchTables",
            element_for(case, "SearchTables", &searched, "TableList", table),
        ),
        ("GetTableVersion", version["TableVersion"]["Table"].clone()),
        ("GetTableVersions", current),
        ("GetUnfilteredTableMetadata", unfiltered["Table"].clone()),
    ]
}

/// Every read that projects a stored table reports its three `Parameters` maps as the
/// definition declared them — the member missing where it declared none — so a client's
/// marker probe answers the same over all of them.
#[tokio::test]
async fn every_table_reader_reports_a_parameter_map_only_where_one_was_declared() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    for row in rows() {
        let name = row.input["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": DATABASE, "TableInput": row.input }),
        )
        .await;

        for (reader, projected) in projections(&router, row.case, &name).await {
            assert_row(row.case, reader, &projected, &row);
        }
    }
}

/// The `TableInput` members of a projected table, which is the definition a client
/// read-modify-write feeds back. Source: `smithy_query shape=TableInput` — every member it
/// declares that a stored table also reports.
const TABLE_INPUT_MEMBERS: [&str; 13] = [
    "Name",
    "Description",
    "Owner",
    "LastAccessTime",
    "LastAnalyzedTime",
    "Retention",
    "StorageDescriptor",
    "PartitionKeys",
    "ViewOriginalText",
    "ViewExpandedText",
    "TableType",
    "Parameters",
    "TargetTable",
];

/// `projected` as a `TableInput`: the members `TableInput` declares, and only those a read
/// actually reported — a member the read withheld is one the client has nothing to send for.
fn table_input_from(projected: &Value) -> Value {
    let mut input = serde_json::Map::new();
    for member in TABLE_INPUT_MEMBERS {
        if let Some(value) = projected.get(member) {
            input.insert(member.to_string(), value.clone());
        }
    }
    Value::Object(input)
}

/// A read fed straight back through `UpdateTable` leaves the definition declaring exactly
/// what it declared before: a map the read withheld is one the client cannot send, so the
/// round trip writes no property set that was never declared, and an explicitly empty one
/// survives it.
#[tokio::test]
async fn a_read_fed_back_through_update_table_declares_what_it_declared() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    for row in rows() {
        let name = row.input["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": DATABASE, "TableInput": row.input }),
        )
        .await;

        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": name }),
        )
        .await;
        ok(
            &router,
            "UpdateTable",
            &json!({
                "DatabaseName": DATABASE,
                "TableInput": table_input_from(&read["Table"]),
            }),
        )
        .await;

        let after = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": name }),
        )
        .await;
        assert_row(row.case, "GetTable after the round trip", &after["Table"], &row);
    }
}

/// One row of the partition suite: the `PartitionInput` registered, and the map its reads
/// must report.
struct PartitionRow {
    case: &'static str,
    input: Value,
    parameters: Declared,
}

/// The batch read of partitions projects each one the same way the single read does, so a
/// partition that declared no properties is reported with no `Partition$Parameters` member —
/// `Partition$Parameters` ("these key-value pairs define partition parameters") is optional
/// in the same way `Table$Parameters` is.
///
/// The rows differ along whether the map was declared, how many entries it carries, and
/// whether it was declared empty.
#[tokio::test]
async fn the_batch_partition_read_reports_a_parameter_map_only_where_one_was_declared() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": "events",
                "TableType": "EXTERNAL_TABLE",
                "PartitionKeys": [ { "Name": "dt", "Type": "string" } ],
                "StorageDescriptor": { "Location": "s3://lake/events/" }
            }
        }),
    )
    .await;

    let rows = [
        PartitionRow {
            case: "a partition that declares no properties",
            input: json!({ "Values": ["2026-01-01"] }),
            parameters: Declared::Nothing,
        },
        PartitionRow {
            case: "a partition that declares one property",
            input: json!({ "Values": ["2026-01-02"], "Parameters": { "rows": "17" } }),
            parameters: Declared::Map(json!({ "rows": "17" })),
        },
        PartitionRow {
            case: "a partition that declares an empty property set",
            input: json!({ "Values": ["2026-01-03"], "Parameters": {} }),
            parameters: Declared::Map(json!({})),
        },
        PartitionRow {
            case: "a partition that declares four properties",
            input: json!({ "Values": ["2026-01-04"], "Parameters": properties(4) }),
            parameters: Declared::Map(properties(4)),
        },
    ];

    for row in &rows {
        ok(
            &router,
            "CreatePartition",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": "events",
                "PartitionInput": row.input,
            }),
        )
        .await;
    }

    let batch = ok(
        &router,
        "BatchGetPartition",
        &json!({
            "DatabaseName": DATABASE,
            "TableName": "events",
            "PartitionsToGet": rows
                .iter()
                .map(|row| json!({ "Values": row.input["Values"] }))
                .collect::<Vec<Value>>(),
        }),
    )
    .await;
    let answered = batch["Partitions"]
        .as_array()
        .unwrap_or_else(|| panic!("BatchGetPartition must answer a Partitions list, got {batch}"))
        .clone();

    for row in &rows {
        let values = row.input["Values"].clone();
        let projected = answered
            .iter()
            .find(|partition| partition["Values"] == values)
            .unwrap_or_else(|| {
                panic!("case {}: {values} must be answered, got {batch}", row.case)
            });
        assert_declared(
            row.case,
            "BatchGetPartition",
            "Partition.Parameters",
            projected,
            &row.parameters,
        );
    }
}
