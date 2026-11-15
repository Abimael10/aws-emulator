// SPDX-License-Identifier: CC0-1.0

//! The Iceberg METADATA OBJECT an open-table-format `CreateTable` materialises, driven over
//! the wire with Glue and Amazon S3 mounted on one shared engine.
//!
//! The rule under test: a `CreateTable` carrying an `OpenTableFormatInput$IcebergInput` writes
//! the Iceberg metadata file at exactly the key its `metadata_location` parameter names, so the
//! catalog row and the object it points at agree. That document states the `format-version` the
//! create asked for (2 when it asked for none — "Defaults to 2"), a `table-uuid` identifying
//! this table, an EMPTY snapshot list and one schema whose fields are numbered from 1 in the
//! declared column order with `required` false, each Glue (Hive) column type written as its
//! Iceberg type — while `GetTable` keeps the Glue type names on the storage descriptor.
//! `DeleteTable` removes only the catalog row, so the object survives it and a re-create of the
//! same name mints a NEW object with a new identity; and the parameter-only route (a
//! `table_type` parameter with no `OpenTableFormatInput`) labels the row alone and writes
//! nothing.
//!
//! The rows differ along what the rule generalises over: the format version asked for, the
//! column types declared, the form of the table's location and the route the create takes.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ICEBERG_METADATA_LOCATION_PARAMETER, ICEBERG_TABLE_TYPE, ICEBERG_TABLE_TYPE_PARAMETER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, ok, s3_client};

/// The database every row creates its table in.
const DATABASE: &str = "iceberg_object_db";

/// The bucket every row's table is located in.
const BUCKET: &str = "iceberg-metadata-bucket";

/// An emulator with Glue AND Amazon S3 mounted on one shared engine: the create writes the
/// metadata object into the one object store this emulator holds, so that store has to be
/// there for the write to reach anything.
fn emulator_with_s3() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One create case: the version the Iceberg input asks for, the columns the table declares
/// with the Iceberg type each one must be written as, and the location the table is given.
struct Row {
    case: &'static str,
    /// `IcebergInput$Version`, or `None` to leave the member out.
    version: Option<&'static str>,
    /// The expected `format-version` of the written document.
    format_version: u64,
    /// `(column name, declared Glue type, the Iceberg type the document must state)`.
    ///
    /// The declared names are Hive's own data type names, which is what a Glue column's
    /// `Type` carries (`Column$Type`); the Iceberg spellings are Iceberg's primitive types.
    columns: &'static [(&'static str, &'static str, &'static str)],
    /// The table's location, which the metadata file is written under.
    location: &'static str,
}

/// The create cases.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no version at all, so the documented default decides",
            version: None,
            format_version: 2,
            columns: &[
                ("id", "string", "string"),
                ("data", "string", "string"),
                ("timestamp", "timestamp", "timestamp"),
            ],
            location: "s3://iceberg-metadata-bucket/iceberg/events/",
        },
        Row {
            case: "version 2, named explicitly",
            version: Some("2"),
            format_version: 2,
            columns: &[("id", "string", "string")],
            location: "s3://iceberg-metadata-bucket/iceberg/orders",
        },
        Row {
            case: "version 1",
            version: Some("1"),
            format_version: 1,
            columns: &[("id", "bigint", "long")],
            location: "s3://iceberg-metadata-bucket/iceberg/nested/clicks/v1/",
        },
        Row {
            // Every Glue type the mapping names, including entries beyond the ones a
            // single-column table would exercise.
            case: "every mapped column type",
            version: None,
            format_version: 2,
            columns: &[
                ("c_string", "string", "string"),
                ("c_int", "int", "int"),
                ("c_bigint", "bigint", "long"),
                ("c_float", "float", "float"),
                ("c_double", "double", "double"),
                ("c_boolean", "boolean", "boolean"),
                ("c_date", "date", "date"),
                ("c_timestamp", "timestamp", "timestamp"),
                ("c_varchar", "varchar(16)", "string"),
                ("c_smallint", "smallint", "int"),
                ("c_binary", "binary", "binary"),
            ],
            location: "s3://iceberg-metadata-bucket/iceberg/typed/",
        },
    ]
}

/// The `TableInput` a row's create submits.
fn table_input(name: &str, row: &Row) -> Value {
    let columns: Vec<Value> = row
        .columns
        .iter()
        .map(|(column, declared, _)| json!({ "Name": column, "Type": declared }))
        .collect();
    json!({
        "Name": name,
        "TableType": "EXTERNAL_TABLE",
        "StorageDescriptor": { "Columns": columns, "Location": row.location },
    })
}

/// The `OpenTableFormatInput` a row's create submits.
fn open_table_format(row: &Row) -> Value {
    let mut iceberg = json!({ "MetadataOperation": "CREATE" });
    if let Some(version) = row.version {
        iceberg["Version"] = json!(version);
    }
    json!({ "IcebergInput": iceberg })
}

/// The `metadata_location` the stored table reports, read back through `GetTable`.
async fn metadata_location(router: &MultiServiceRouter, table: &str) -> Option<String> {
    let (status, read) = call(
        router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    assert_eq!(status, 200, "GetTable on {table} must succeed, got {read}");
    read["Table"]["Parameters"][ICEBERG_METADATA_LOCATION_PARAMETER]
        .as_str()
        .map(str::to_string)
}

/// The document stored at the `s3://` location `uri`, read back through Amazon S3's own
/// `GetObject` — the read an Iceberg client makes of a `metadata_location`.
async fn stored_document(router: &MultiServiceRouter, case: &str, uri: &str) -> Value {
    let body = stored_bytes(router, case, uri).await;
    serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the metadata object at {uri} must be JSON, got {error}")
    })
}

/// The bytes stored at `uri`.
async fn stored_bytes(router: &MultiServiceRouter, case: &str, uri: &str) -> Vec<u8> {
    let (bucket, key) = split(uri);
    let read = s3_client(router)
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the object {uri} the catalog points at must exist: {error:?}")
        });
    read.body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the object {uri} must be readable: {error}"))
        .to_vec()
}

/// Every key the bucket holds under `prefix`, read through `ListObjectsV2`.
async fn keys_under(router: &MultiServiceRouter, prefix: &str) -> Vec<String> {
    let listed = s3_client(router)
        .list_objects_v2()
        .bucket(BUCKET)
        .prefix(prefix)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the listing under {prefix} must succeed: {error:?}"));
    listed
        .contents()
        .iter()
        .filter_map(|object| object.key().map(str::to_string))
        .collect()
}

/// An `s3://bucket/key` location split into its two parts.
fn split(uri: &str) -> (String, String) {
    let rest = uri
        .strip_prefix("s3://")
        .unwrap_or_else(|| panic!("the location {uri} must be an s3:// URI"));
    let (bucket, key) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("the location {uri} must name a key"));
    (bucket.to_string(), key.to_string())
}

/// Create the bucket every row's table is located in.
async fn create_bucket(router: &MultiServiceRouter) {
    s3_client(router)
        .create_bucket()
        .bucket(BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {BUCKET} must be created: {error:?}"));
}

/// The suite's one test, which drives the three rules in turn.
///
/// One test rather than three: each rule stands up its own emulator, and the suite's work is
/// a handful of calls per rule, so running them in sequence costs the same and adds one unit
/// of concurrency to this crate's test binary rather than three.
#[tokio::test]
async fn the_iceberg_metadata_object_follows_its_catalog_pointer() {
    an_iceberg_create_writes_the_metadata_object_its_pointer_names().await;
    a_deleted_iceberg_table_keeps_its_metadata_and_a_recreate_mints_a_new_one().await;
    the_table_type_parameter_alone_materialises_no_metadata().await;
}

/// RULE: an Iceberg create writes the metadata document at exactly the key the catalog row
/// points at, stating the version asked for, the table's identity, no snapshot and the
/// table's columns as Iceberg fields numbered from 1.
async fn an_iceberg_create_writes_the_metadata_object_its_pointer_names() {
    let router = emulator_with_s3();
    create_bucket(&router).await;
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    let mut identities: Vec<String> = Vec::new();
    for (ordinal, row) in rows().iter().enumerate() {
        let case = row.case;
        let table = format!("table_{ordinal}");
        ok(
            &router,
            "CreateTable",
            &json!({
                "DatabaseName": DATABASE,
                "TableInput": table_input(&table, row),
                "OpenTableFormatInput": open_table_format(row),
            }),
        )
        .await;

        let location = metadata_location(&router, &table)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the create must mint a metadata location"));
        let document = stored_document(&router, case, &location).await;

        assert_eq!(
            document["format-version"].as_u64(),
            Some(row.format_version),
            "case {case:?}: expected format-version {}, got {document}",
            row.format_version
        );
        let uuid = document["table-uuid"].as_str().unwrap_or_default();
        let groups: Vec<usize> = uuid.split('-').map(str::len).collect();
        assert_eq!(
            groups,
            vec![8, 4, 4, 4, 12],
            "case {case:?}: the table-uuid is spelled as a UUID, got {uuid:?}"
        );
        assert_eq!(
            document["snapshots"],
            json!([]),
            "case {case:?}: a created table has committed nothing, got {document}"
        );
        assert_eq!(
            document["location"].as_str(),
            Some(row.location.trim_end_matches('/')),
            "case {case:?}: the document states the table's own location, got {document}"
        );

        // The schema: one field per declared column, numbered from 1 in declaration order,
        // optional, and carrying the Iceberg type of the Glue type declared.
        let fields = document["schemas"][0]["fields"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: the document must declare a schema"))
            .clone();
        assert_eq!(
            fields.len(),
            row.columns.len(),
            "case {case:?}: expected {} fields, got {fields:?}",
            row.columns.len()
        );
        for (index, (column, _, iceberg)) in row.columns.iter().enumerate() {
            let field = &fields[index];
            assert_eq!(
                field,
                &json!({
                    "id": index + 1,
                    "name": column,
                    "required": false,
                    "type": iceberg,
                }),
                "case {case:?}: field {column} must be numbered {} and typed {iceberg}, got \
                 {field}",
                index + 1
            );
        }

        // The catalog keeps the GLUE type names: the Iceberg spelling lives in the document
        // alone.
        let (_, read) = call(
            &router,
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": table }),
        )
        .await;
        let declared: Vec<String> = read["Table"]["StorageDescriptor"]["Columns"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|column| column["Type"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected: Vec<String> = row
            .columns
            .iter()
            .map(|(_, glue, _)| (*glue).to_string())
            .collect();
        assert_eq!(
            declared, expected,
            "case {case:?}: GetTable must keep the Glue type names, got {declared:?}"
        );

        identities.push(uuid.to_string());
    }

    // Each table got an identity of its own.
    let unique: std::collections::BTreeSet<&String> = identities.iter().collect();
    assert_eq!(
        unique.len(),
        identities.len(),
        "each Iceberg table gets its own table-uuid, got {identities:?}"
    );
}

/// RULE: `DeleteTable` removes the catalog row and nothing in Amazon S3, so the metadata
/// object survives the delete and a re-create of the same name mints a new one with a new
/// identity beside it.
async fn a_deleted_iceberg_table_keeps_its_metadata_and_a_recreate_mints_a_new_one() {
    let router = emulator_with_s3();
    create_bucket(&router).await;
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    let row = Row {
        case: "delete and re-create",
        version: None,
        format_version: 2,
        columns: &[("id", "string", "string")],
        location: "s3://iceberg-metadata-bucket/iceberg/recreated/",
    };
    let table = "recreated";
    let create = json!({
        "DatabaseName": DATABASE,
        "TableInput": table_input(table, &row),
        "OpenTableFormatInput": open_table_format(&row),
    });

    ok(&router, "CreateTable", &create).await;
    let first = metadata_location(&router, table)
        .await
        .expect("the create mints a metadata location");
    let first_uuid = stored_document(&router, row.case, &first).await["table-uuid"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    ok(
        &router,
        "DeleteTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    // The catalog row is gone.
    let (status, read) = call(
        &router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the deleted table must not read back, got {status} with {read}"
    );
    // The object is not.
    let kept = stored_bytes(&router, "after the delete", &first).await;
    assert!(
        !kept.is_empty(),
        "the delete removes only the catalog row, so the metadata object stays"
    );

    ok(&router, "CreateTable", &create).await;
    let second = metadata_location(&router, table)
        .await
        .expect("the re-create mints a new location");
    assert_ne!(
        second, first,
        "a re-create mints a NEW metadata object, got {second}"
    );
    let second_uuid = stored_document(&router, "the re-create", &second).await["table-uuid"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        second_uuid, first_uuid,
        "a re-created table is a new table, so its identity differs; got {second_uuid}"
    );
    let objects = keys_under(&router, "iceberg/recreated/metadata/").await;
    assert_eq!(
        objects.len(),
        2,
        "the surviving object and the new one must both be there, got {objects:?}"
    );
}

/// RULE: the parameter-only route — a `table_type` of `ICEBERG` set by a client with no
/// `OpenTableFormatInput` — labels the catalog row alone: the service mints no
/// `metadata_location` for it and writes no object at all.
async fn the_table_type_parameter_alone_materialises_no_metadata() {
    let router = emulator_with_s3();
    create_bucket(&router).await;
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    let mut parameters = ::serde_json::Map::new();
    parameters.insert(
        ICEBERG_TABLE_TYPE_PARAMETER.to_string(),
        json!(ICEBERG_TABLE_TYPE),
    );
    parameters.insert("format".to_string(), json!("parquet"));
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": "labelled",
                "TableType": "EXTERNAL_TABLE",
                "Parameters": parameters,
                "StorageDescriptor": {
                    "Columns": [ { "Name": "id", "Type": "string" } ],
                    "Location": "s3://iceberg-metadata-bucket/iceberg/labelled/",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.iceberg.mr.hive.HiveIcebergSerDe"
                    },
                    "InputFormat": "org.apache.iceberg.mr.hive.HiveIcebergInputFormat",
                    "OutputFormat": "org.apache.iceberg.mr.hive.HiveIcebergOutputFormat",
                },
            },
        }),
    )
    .await;

    assert_eq!(
        metadata_location(&router, "labelled").await,
        None,
        "the parameter-only route mints no metadata location"
    );
    let objects = keys_under(&router, "iceberg/labelled/").await;
    assert!(
        objects.is_empty(),
        "the parameter-only route writes no object at all, got {objects:?}"
    );
}
