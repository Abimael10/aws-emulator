// SPDX-License-Identifier: CC0-1.0

//! The `Parameters` maps the Data Catalog reports on a definition it holds.
//!
//! The key-value properties of a table, of a partition, and of the storage and SerDe a
//! definition describes are the CLIENT's to supply — `Table$Parameters` "these key-value
//! pairs define properties associated with the table", `Partition$Parameters` "these
//! key-value pairs define partition parameters", `StorageDescriptor$Parameters` "the
//! user-supplied properties in key-value form", and `SerDeInfo$Parameters` (source:
//! `smithy_query shape=Table,Partition,StorageDescriptor,SerDeInfo` on this crate's model,
//! where none of the four carries `@required` or `@default`). So the catalog serves a
//! definition as it was WRITTEN: a map the write supplied comes back as supplied, an
//! explicitly empty one included, and a map no write ever supplied is ABSENT from the
//! response rather than materialised empty. A client that diffs a read against the input it
//! submitted then sees no property it never set, and one that branches on the PRESENCE of a
//! property map reads what the catalog was told.
//!
//! Each suite is a table: the rows differ along what the rule generalises over — which of
//! the three maps a definition declares, whether the declaration is absent or an explicitly
//! empty map, how many entries a declared map carries, and whether the definition carries a
//! storage descriptor or a SerDe at all. The recorded case (a table created with none of the
//! three) is one row. Every row is checked through each read that projects the definition, so
//! the single-resource read and the list cannot answer differently. Calls go over the wire
//! through the shared [`crate::wire`] harness.

use serde_json::{json, Value};

use crate::wire::{emulator, ok};

/// What a read must report for one `Parameters` map: the map the definition was written
/// with, or ABSENT for a definition that supplied none.
#[derive(Clone)]
enum Properties {
    /// The member must not be in the response at all.
    Absent,
    /// The member must be in the response, carrying this map.
    Reported(Value),
}

/// What a read must report for the three `Parameters` maps of one table definition.
struct Expected {
    /// `Table.Parameters`.
    parameters: Properties,
    /// `Table.StorageDescriptor.Parameters`, or `None` when the definition carries no
    /// storage descriptor at all — the catalog reports the properties OF a descriptor it
    /// stores, not whether there is one.
    storage: Option<Properties>,
    /// `Table.StorageDescriptor.SerdeInfo.Parameters`, or `None` when the stored
    /// descriptor names no SerDe.
    serde: Option<Properties>,
}

/// One row of the table suite: the definition created, and what the reads must report.
struct TableCase {
    case: &'static str,
    input: Value,
    expected: Expected,
}

/// One row of the partition suite: the definition created, and the two maps the reads must
/// report for it.
struct PartitionCase {
    case: &'static str,
    input: Value,
    /// `Partition.Parameters`.
    parameters: Properties,
    /// `Partition.StorageDescriptor.Parameters`, or `None` for a partition stored without
    /// a descriptor.
    storage: Option<Properties>,
    /// `Partition.StorageDescriptor.SerdeInfo.Parameters`, or `None` when the stored
    /// descriptor names no SerDe.
    serde: Option<Properties>,
}

/// A map of `entries` properties, for the rows that move the entry COUNT.
fn properties(entries: usize) -> Value {
    let map: serde_json::Map<String, Value> = (0..entries)
        .map(|index| (format!("property_{index}"), json!(format!("value_{index}"))))
        .collect();
    Value::Object(map)
}

/// Assert one projected definition's `Parameters` member is what the definition was written
/// with, naming which read answered it and which member is under test.
fn assert_properties(case: &str, reader: &str, member: &str, held: &Value, want: &Properties) {
    let reported = held.get("Parameters");
    match want {
        Properties::Absent => assert!(
            reported.is_none(),
            "case {case}: {reader} must report no {member} for a definition that supplied \
             none, got {reported:?}"
        ),
        Properties::Reported(map) => assert_eq!(
            reported,
            Some(map),
            "case {case}: {reader} must report {member} {map}, got {reported:?}"
        ),
    }
}

/// Assert the three maps of one projected table, naming which read answered it.
fn assert_table_maps(case: &str, reader: &str, table: &Value, expected: &Expected) {
    assert_properties(case, reader, "Table.Parameters", table, &expected.parameters);
    match &expected.storage {
        None => assert!(
            table["StorageDescriptor"].is_null(),
            "case {case}: {reader} must report no storage descriptor for a definition that \
             stores none, got {:?}",
            table["StorageDescriptor"]
        ),
        Some(storage) => assert_properties(
            case,
            reader,
            "StorageDescriptor.Parameters",
            &table["StorageDescriptor"],
            storage,
        ),
    }
    match &expected.serde {
        None => assert!(
            table["StorageDescriptor"]["SerdeInfo"].is_null(),
            "case {case}: {reader} must report no SerdeInfo for a descriptor that stores none, \
             got {:?}",
            table["StorageDescriptor"]["SerdeInfo"]
        ),
        Some(serde) => assert_properties(
            case,
            reader,
            "SerdeInfo.Parameters",
            &table["StorageDescriptor"]["SerdeInfo"],
            serde,
        ),
    }
}

/// The rows of the table suite.
fn table_cases() -> Vec<TableCase> {
    vec![
        TableCase {
            // The recorded case: a table registered with a located, serde-described
            // storage descriptor and no properties anywhere.
            case: "no properties declared anywhere",
            input: json!({
                "Name": "no_properties",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://bucket/no_properties/",
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
            expected: Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Absent),
                serde: Some(Properties::Absent),
            },
        },
        TableCase {
            case: "no storage descriptor at all",
            input: json!({ "Name": "no_storage", "TableType": "EXTERNAL_TABLE" }),
            expected: Expected {
                parameters: Properties::Absent,
                storage: None,
                serde: None,
            },
        },
        TableCase {
            case: "a storage descriptor that names no serde",
            input: json!({
                "Name": "no_serde",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": { "Location": "s3://bucket/no_serde/" }
            }),
            expected: Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Absent),
                serde: None,
            },
        },
        TableCase {
            case: "the table declares properties, its storage and serde none",
            input: json!({
                "Name": "table_properties_only",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/table_properties_only/",
                    "SerdeInfo": { "Name": "csv" }
                }
            }),
            expected: Expected {
                parameters: Properties::Reported(json!({ "classification": "csv" })),
                storage: Some(Properties::Absent),
                serde: Some(Properties::Absent),
            },
        },
        TableCase {
            case: "the storage and serde declare properties, the table none",
            input: json!({
                "Name": "storage_properties_only",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://bucket/storage_properties_only/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            expected: Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Reported(
                    json!({ "skip.header.line.count": "1" }),
                )),
                serde: Some(Properties::Reported(json!({ "field.delim": "," }))),
            },
        },
        TableCase {
            // A map the write SUPPLIED is reported as supplied, even with nothing in it:
            // the catalog drops no property set it was told about.
            case: "all three maps sent explicitly empty",
            input: json!({
                "Name": "explicitly_empty",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": {},
                "StorageDescriptor": {
                    "Location": "s3://bucket/explicitly_empty/",
                    "Parameters": {},
                    "SerdeInfo": { "Parameters": {} }
                }
            }),
            expected: Expected {
                parameters: Properties::Reported(json!({})),
                storage: Some(Properties::Reported(json!({}))),
                serde: Some(Properties::Reported(json!({}))),
            },
        },
        TableCase {
            case: "twelve properties on the table, one on its storage, none on its serde",
            input: json!({
                "Name": "many_properties",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": properties(12),
                "StorageDescriptor": {
                    "Location": "s3://bucket/many_properties/",
                    "Parameters": properties(1),
                    "SerdeInfo": {
                        "SerializationLibrary": "org.openx.data.jsonserde.JsonSerDe"
                    }
                }
            }),
            expected: Expected {
                parameters: Properties::Reported(properties(12)),
                storage: Some(Properties::Reported(properties(1))),
                serde: Some(Properties::Absent),
            },
        },
        TableCase {
            case: "a view-typed table with no properties",
            input: json!({
                "Name": "view_typed",
                "TableType": "VIRTUAL_VIEW",
                "ViewOriginalText": "/* Presto View */",
                "StorageDescriptor": { "SerdeInfo": { "Name": "view" } }
            }),
            expected: Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Absent),
                serde: Some(Properties::Absent),
            },
        },
    ]
}

/// Every read that projects a stored TABLE reports its three `Parameters` maps as the
/// definition was written: the properties it declared, and no member at all where it
/// declared none.
#[tokio::test]
async fn every_table_read_reports_the_definitions_parameter_maps() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "parameters_db" } }),
    )
    .await;

    for row in table_cases() {
        let case = row.case;
        let name = row.input["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": "parameters_db", "TableInput": row.input }),
        )
        .await;

        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": "parameters_db", "Name": name }),
        )
        .await;
        let table = read["Table"].clone();
        assert_table_maps(case, "GetTable", &table, &row.expected);

        let page = ok(
            &router,
            "GetTables",
            &json!({ "DatabaseName": "parameters_db" }),
        )
        .await;
        let listed = page["TableList"]
            .as_array()
            .expect("TableList is a list")
            .iter()
            .find(|entry| entry["Name"] == json!(name))
            .unwrap_or_else(|| panic!("case {case}: {name} must appear in TableList"))
            .clone();
        assert_table_maps(case, "GetTables", &listed, &row.expected);
        assert_eq!(
            listed, table,
            "case {case}: the TableList element must be the GetTable projection, got {listed:?} \
             against {table:?}"
        );

        let version = ok(
            &router,
            "GetTableVersion",
            &json!({ "DatabaseName": "parameters_db", "TableName": name }),
        )
        .await;
        let versioned = version["TableVersion"]["Table"].clone();
        assert_table_maps(case, "GetTableVersion", &versioned, &row.expected);
    }
}

/// The rows of the partition suite.
fn partition_cases() -> Vec<PartitionCase> {
    vec![
        PartitionCase {
            case: "a partition that declares nothing and stores no descriptor",
            input: json!({ "Values": ["2026-01-01"] }),
            parameters: Properties::Absent,
            storage: None,
            serde: None,
        },
        PartitionCase {
            case: "a partition whose descriptor and serde declare nothing",
            input: json!({
                "Values": ["2026-01-02"],
                "StorageDescriptor": {
                    "Location": "s3://bucket/events/dt=2026-01-02/",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
            parameters: Properties::Absent,
            storage: Some(Properties::Absent),
            serde: Some(Properties::Absent),
        },
        PartitionCase {
            case: "a partition that declares properties everywhere",
            input: json!({
                "Values": ["2026-01-03"],
                "Parameters": { "rows": "17" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/events/dt=2026-01-03/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "separatorChar": "," } }
                }
            }),
            parameters: Properties::Reported(json!({ "rows": "17" })),
            storage: Some(Properties::Reported(
                json!({ "skip.header.line.count": "1" }),
            )),
            serde: Some(Properties::Reported(json!({ "separatorChar": "," }))),
        },
        PartitionCase {
            case: "a partition whose maps are sent explicitly empty",
            input: json!({
                "Values": ["2026-01-04"],
                "Parameters": {},
                "StorageDescriptor": {
                    "Location": "s3://bucket/events/dt=2026-01-04/",
                    "Parameters": {},
                    "SerdeInfo": { "Parameters": {} }
                }
            }),
            parameters: Properties::Reported(json!({})),
            storage: Some(Properties::Reported(json!({}))),
            serde: Some(Properties::Reported(json!({}))),
        },
        PartitionCase {
            case: "a partition carrying eight properties",
            input: json!({
                "Values": ["2026-01-05"],
                "Parameters": properties(8),
                "StorageDescriptor": { "Location": "s3://bucket/events/dt=2026-01-05/" }
            }),
            parameters: Properties::Reported(properties(8)),
            storage: Some(Properties::Absent),
            serde: None,
        },
    ]
}

/// A PARTITION is a definition the catalog holds the same way, so every read that projects
/// one reports its own properties and those of the storage it describes exactly as the write
/// declared them, and no member where it declared none.
#[tokio::test]
async fn every_partition_read_reports_the_definitions_parameter_maps() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "partition_parameters_db" } }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": "partition_parameters_db",
            "TableInput": {
                "Name": "events",
                "TableType": "EXTERNAL_TABLE",
                "PartitionKeys": [ { "Name": "dt", "Type": "string" } ],
                "StorageDescriptor": { "Location": "s3://bucket/events/" }
            }
        }),
    )
    .await;

    let rows = partition_cases();
    for row in &rows {
        ok(
            &router,
            "CreatePartition",
            &json!({
                "DatabaseName": "partition_parameters_db",
                "TableName": "events",
                "PartitionInput": row.input,
            }),
        )
        .await;
    }

    let page = ok(
        &router,
        "GetPartitions",
        &json!({ "DatabaseName": "partition_parameters_db", "TableName": "events" }),
    )
    .await;
    let listed = page["Partitions"]
        .as_array()
        .expect("Partitions is a list")
        .clone();

    for row in &rows {
        let case = row.case;
        let values = row.input["Values"].clone();
        let read = ok(
            &router,
            "GetPartition",
            &json!({
                "DatabaseName": "partition_parameters_db",
                "TableName": "events",
                "PartitionValues": values,
            }),
        )
        .await;
        let partition = read["Partition"].clone();
        let from_list = listed
            .iter()
            .find(|entry| entry["Values"] == values)
            .unwrap_or_else(|| panic!("case {case}: {values} must appear in Partitions"))
            .clone();

        for (reader, projected) in [("GetPartition", &partition), ("GetPartitions", &from_list)] {
            assert_properties(
                case,
                reader,
                "Partition.Parameters",
                projected,
                &row.parameters,
            );
            match &row.storage {
                None => assert!(
                    projected["StorageDescriptor"].is_null(),
                    "case {case}: {reader} must report no storage descriptor for a partition \
                     that stores none, got {:?}",
                    projected["StorageDescriptor"]
                ),
                Some(storage) => assert_properties(
                    case,
                    reader,
                    "StorageDescriptor.Parameters",
                    &projected["StorageDescriptor"],
                    storage,
                ),
            }
            match &row.serde {
                None => assert!(
                    projected["StorageDescriptor"]["SerdeInfo"].is_null(),
                    "case {case}: {reader} must report no SerdeInfo for a descriptor that \
                     stores none, got {:?}",
                    projected["StorageDescriptor"]["SerdeInfo"]
                ),
                Some(serde) => assert_properties(
                    case,
                    reader,
                    "SerdeInfo.Parameters",
                    &projected["StorageDescriptor"]["SerdeInfo"],
                    serde,
                ),
            }
        }
        assert_eq!(
            from_list, partition,
            "case {case}: a Partitions element must be the GetPartition projection, got \
             {from_list:?} against {partition:?}"
        );
    }
}

/// An UPDATE that omits the properties the create declared leaves the definition declaring
/// none, which every read reports by withholding the member: the projection is of the
/// definition as it now stands, not of the write that made it.
///
/// The rows differ along which maps the create declared and which the update keeps.
#[tokio::test]
async fn a_cleared_parameter_map_is_reported_absent() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "cleared_db" } }),
    )
    .await;

    // (case, the definition created, the definition updated over it, the three maps the
    // read must then report)
    let rows: Vec<(&str, Value, Value, Expected)> = vec![
        (
            "the table's own properties dropped",
            json!({
                "Name": "cleared_table",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/cleared_table/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            json!({
                "Name": "cleared_table",
                "StorageDescriptor": {
                    "Location": "s3://bucket/cleared_table/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Reported(
                    json!({ "skip.header.line.count": "1" }),
                )),
                serde: Some(Properties::Reported(json!({ "field.delim": "," }))),
            },
        ),
        (
            "the storage and serde properties dropped",
            json!({
                "Name": "cleared_storage",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/cleared_storage/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            json!({
                "Name": "cleared_storage",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/cleared_storage/",
                    "SerdeInfo": { "Name": "csv" }
                }
            }),
            Expected {
                parameters: Properties::Reported(json!({ "classification": "csv" })),
                storage: Some(Properties::Absent),
                serde: Some(Properties::Absent),
            },
        ),
        (
            "every map dropped at once",
            json!({
                "Name": "cleared_everything",
                "Parameters": { "classification": "csv" },
                "StorageDescriptor": {
                    "Location": "s3://bucket/cleared_everything/",
                    "Parameters": { "skip.header.line.count": "1" },
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
            json!({
                "Name": "cleared_everything",
                "StorageDescriptor": { "Location": "s3://bucket/cleared_everything/" }
            }),
            Expected {
                parameters: Properties::Absent,
                storage: Some(Properties::Absent),
                serde: None,
            },
        ),
    ];

    for (case, created, updated, expected) in &rows {
        let name = created["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": "cleared_db", "TableInput": created }),
        )
        .await;
        ok(
            &router,
            "UpdateTable",
            &json!({ "DatabaseName": "cleared_db", "TableInput": updated }),
        )
        .await;

        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": "cleared_db", "Name": name }),
        )
        .await;
        assert_table_maps(case, "GetTable", &read["Table"], expected);
    }
}
