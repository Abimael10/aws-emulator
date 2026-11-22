// SPDX-License-Identifier: CC0-1.0

//! The Data Catalog ROUND-TRIPS a table: a read of a table serves the definition that was
//! written plus the members the catalog itself determines, and nothing else.
//!
//! The rule under test: a `TableInput` member the model declares optional and gives no
//! `@default` is the CLIENT's to supply, so a table read reports it only where a write
//! supplied it — an omitted map, list or structure is ABSENT from the response rather than
//! materialised empty. What the read adds is only what the catalog itself determines: the
//! attribution of the table (`DatabaseName`, `CatalogId`, `CatalogIdentifier`, `DatabaseId`),
//! the instants and the version of the write, who made it, the view and row-filtering flags,
//! and the members whose value the catalog documents for every table it holds —
//! `Table.Retention` 0 (a non-nullable integer), `Table.VersionId` "0" for the first version,
//! `Table.IsRegisteredWithLakeFormation` false, and, inside a stored descriptor,
//! `StorageDescriptor.Columns` and `StorageDescriptor.SortColumns` as lists, `Compressed`
//! false, `NumberOfBuckets` 0 and `StoredAsSubDirectories` false. Sources: `smithy_query
//! shape=TableInput,Table,StorageDescriptor,SerDeInfo` on this crate's model, where none of
//! `Table$Owner`, `Table$LastAccessTime`, `Table$PartitionKeys`, `Table$Parameters`,
//! `StorageDescriptor$Parameters`, `StorageDescriptor$BucketColumns`,
//! `StorageDescriptor$SkewedInfo` or `SerDeInfo$Parameters` carries `@required` or `@default`.
//!
//! WHY it matters: a client that diffs a read against the input it submitted — the converge
//! loop of a deployment tool, or any catalog-sync — reads a member it never set as drift and
//! re-applies for ever, and a client that branches on the PRESENCE of a property map to decide
//! whether anything was configured reads a false positive.
//!
//! The rows differ along what the written definition omits: everything optional at once, a
//! descriptor without its property map, a serde with nothing but its library, a table with
//! partition keys but no owner, and a definition that supplies every one of those members (the
//! control, where nothing may be dropped either). Calls go over the wire through the shared
//! [`crate::wire`] harness.

use serde_json::{json, Map, Value};

use crate::wire::{emulator, ok};

/// The database every row's table is created in.
const DATABASE: &str = "round_trip_db";

/// The members of a returned `Table` the CATALOG determines rather than the client: its
/// attribution, the bookkeeping of the write, and the flags and defaults the catalog answers
/// for every table it holds. Any OTHER member a read reports is one the write supplied.
const CATALOG_DETERMINED: [&str; 14] = [
    "Name",
    "DatabaseName",
    "CatalogId",
    "CatalogIdentifier",
    "DatabaseId",
    "CreateTime",
    "UpdateTime",
    "VersionId",
    "CreatedBy",
    "Retention",
    "IsRegisteredWithLakeFormation",
    "IsMultiDialectView",
    "IsMaterializedView",
    "IsRowFilteringEnabled",
];

/// The members of a returned `StorageDescriptor` the catalog answers definitely for every
/// descriptor it stores: its two lists, and the three scalars whose default it documents.
const DESCRIPTOR_DETERMINED: [&str; 5] = [
    "Columns",
    "SortColumns",
    "Compressed",
    "NumberOfBuckets",
    "StoredAsSubDirectories",
];

/// The keys of one structure a read reported.
fn keys(value: &Value) -> Vec<String> {
    value
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new)
        .keys()
        .cloned()
        .collect()
}

/// The members `read` reports that neither `written` supplied nor `determined` names — the
/// members the catalog would have INVENTED.
fn invented(written: &Value, read: &Value, determined: &[&str]) -> Vec<String> {
    keys(read)
        .into_iter()
        .filter(|member| {
            written.get(member).is_none() && !determined.contains(&member.as_str())
        })
        .collect()
}

/// The rows: the `TableInput` each one writes.
fn written_definitions() -> Vec<(&'static str, Value)> {
    vec![
        (
            // The recorded case: a located, serde-described external table and nothing else.
            "a located external table whose descriptor supplies no property map",
            json!({
                "Name": "cloudfront_logs",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Columns": [
                        { "Name": "date", "Type": "date" },
                        { "Name": "time", "Type": "string" },
                        { "Name": "bytes", "Type": "bigint" }
                    ],
                    "Location": "s3://round-trip-bucket/cloudfront/",
                    "InputFormat": "org.apache.hadoop.mapred.TextInputFormat",
                    "OutputFormat":
                        "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
        ),
        (
            "a table that supplies nothing but its name",
            json!({ "Name": "bare_table" }),
        ),
        (
            "a descriptor with a serde that supplies only its properties",
            json!({
                "Name": "serde_properties_only",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://round-trip-bucket/serde/",
                    "SerdeInfo": { "Parameters": { "field.delim": "," } }
                }
            }),
        ),
        (
            "a partitioned table that names no owner",
            json!({
                "Name": "partitioned_table",
                "TableType": "EXTERNAL_TABLE",
                "PartitionKeys": [ { "Name": "dt", "Type": "string" } ],
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://round-trip-bucket/partitioned/"
                }
            }),
        ),
        (
            // The control: a definition that supplies every member the rows above omit, where
            // the read must drop none of them.
            "a definition that supplies every optional member",
            json!({
                "Name": "fully_declared",
                "TableType": "EXTERNAL_TABLE",
                "Owner": "data-engineering",
                "Description": "every optional member supplied",
                "LastAccessTime": 1_700_000_000,
                "Retention": 7,
                "Parameters": { "classification": "csv" },
                "PartitionKeys": [ { "Name": "dt", "Type": "string" } ],
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://round-trip-bucket/full/",
                    "BucketColumns": [ "id" ],
                    "NumberOfBuckets": 4,
                    "Compressed": true,
                    "StoredAsSubDirectories": true,
                    "Parameters": { "skip.header.line.count": "1" },
                    "SkewedInfo": {
                        "SkewedColumnNames": [ "id" ],
                        "SkewedColumnValues": [ "ada" ],
                        "SkewedColumnValueLocationMaps": { "ada": "s3://b/ada/" }
                    },
                    "SerdeInfo": {
                        "Name": "csv",
                        "SerializationLibrary": "org.apache.hadoop.hive.serde2.OpenCSVSerde",
                        "Parameters": { "separatorChar": "," }
                    }
                }
            }),
        ),
    ]
}

/// A read of a created table invents no member the write never supplied, at the table, at its
/// storage descriptor and at that descriptor's SerDe — while the members the catalog itself
/// determines are all there, carrying the values it documents.
#[tokio::test]
async fn a_table_read_invents_no_member_the_write_never_supplied() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    for (case, written) in written_definitions() {
        let name = written["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": DATABASE, "TableInput": written }),
        )
        .await;
        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": name }),
        )
        .await["Table"]
            .clone();

        let extra = invented(&written, &read, &CATALOG_DETERMINED);
        assert!(
            extra.is_empty(),
            "case {case:?}: the catalog reported members the write never supplied: {extra:?}, \
             in {read}"
        );

        match written.get("StorageDescriptor") {
            None => assert!(
                read["StorageDescriptor"].is_null(),
                "case {case:?}: a descriptor the write never supplied stays absent, got {:?}",
                read["StorageDescriptor"]
            ),
            Some(descriptor) => {
                let stored = read["StorageDescriptor"].clone();
                let extra = invented(descriptor, &stored, &DESCRIPTOR_DETERMINED);
                assert!(
                    extra.is_empty(),
                    "case {case:?}: the descriptor reported members the write never supplied: \
                     {extra:?}, in {stored}"
                );
                for member in DESCRIPTOR_DETERMINED {
                    assert!(
                        stored.get(member).is_some(),
                        "case {case:?}: the catalog answers {member} for every descriptor it \
                         stores, got {stored}"
                    );
                }
                match descriptor.get("SerdeInfo") {
                    None => assert!(
                        stored["SerdeInfo"].is_null(),
                        "case {case:?}: a SerDe the write never supplied stays absent, got {:?}",
                        stored["SerdeInfo"]
                    ),
                    Some(serde) => {
                        let extra = invented(serde, &stored["SerdeInfo"], &[]);
                        assert!(
                            extra.is_empty(),
                            "case {case:?}: the SerDe reported members the write never \
                             supplied: {extra:?}, in {:?}",
                            stored["SerdeInfo"]
                        );
                    }
                }
            }
        }

        // Nothing the write DID supply is dropped on the way back.
        crate::wire::assert_echoed(case, &written, &read);
    }
}

/// The values the catalog documents for every table it holds are filled in whatever the write
/// supplied: the scalar defaults of the table and of a stored descriptor, and the descriptor's
/// two lists.
#[tokio::test]
async fn the_documented_catalog_defaults_are_filled_in() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "defaults_db" } }),
    )
    .await;

    // Each row: the written definition, and the table members the read must report.
    struct DefaultsRow {
        case: &'static str,
        written: Value,
        reported: Vec<(&'static str, Value)>,
    }

    let rows: Vec<DefaultsRow> = vec![
        DefaultsRow {
            case: "a located external table that supplies none of them",
            written: json!({
                "Name": "defaults_minimal",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": "s3://round-trip-bucket/defaults/",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe"
                    }
                }
            }),
            reported: vec![
                ("Retention", json!(0)),
                ("VersionId", json!("0")),
                ("IsRegisteredWithLakeFormation", json!(false)),
            ],
        },
        DefaultsRow {
            case: "a table whose write supplies a retention of its own",
            written: json!({
                "Name": "defaults_retained",
                "TableType": "EXTERNAL_TABLE",
                "Retention": 30,
                "StorageDescriptor": { "Location": "s3://round-trip-bucket/retained/" }
            }),
            reported: vec![
                ("Retention", json!(30)),
                ("VersionId", json!("0")),
                ("IsRegisteredWithLakeFormation", json!(false)),
            ],
        },
    ];

    for row in &rows {
        let case = row.case;
        let written = &row.written;
        let name = written["Name"].as_str().expect("a table name").to_string();
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": "defaults_db", "TableInput": written }),
        )
        .await;
        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": "defaults_db", "Name": name }),
        )
        .await["Table"]
            .clone();

        for (member, value) in &row.reported {
            assert_eq!(
                &read[member], value,
                "case {case}: expected Table.{member} {value}, got {:?}",
                read[member]
            );
        }
        for (member, value) in [
            ("Compressed", json!(false)),
            ("NumberOfBuckets", json!(0)),
            ("StoredAsSubDirectories", json!(false)),
            ("SortColumns", json!([])),
            ("Columns", json!([])),
        ] {
            assert_eq!(
                read["StorageDescriptor"][member], value,
                "case {case}: expected StorageDescriptor.{member} {value}, got {:?}",
                read["StorageDescriptor"][member]
            );
        }
    }
}
