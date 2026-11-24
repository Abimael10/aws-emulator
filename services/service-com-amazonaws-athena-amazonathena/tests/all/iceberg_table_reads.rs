// SPDX-License-Identifier: CC0-1.0

//! Reading a Data Catalog table of the ICEBERG open table format, driven end to end over the
//! wire through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: a table of an open table format is read THROUGH the metadata its
//! catalog entry points at, so the rows a query over such a table sees are the records of the
//! data files its CURRENT COMMITTED SNAPSHOT reaches — the files its snapshot's manifest
//! lists and manifests name and have not removed — and nothing else at the table's location
//! is its data. A table whose metadata names no current snapshot therefore holds no row and
//! the query goes through no data at all, so an aggregate over it counts its rows as absent
//! and the execution reports zero bytes scanned and zero rows processed.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: whether
//! the metadata names a current snapshot, how many manifests that snapshot names, how many
//! data files each manifest holds, whether a later commit replaces an earlier one, whether an
//! entry is one the snapshot REMOVED, and whether a data file at the location is named by any
//! entry at all. The recorded case — an aggregate over a table a create has just minted, with
//! an object already sitting under its location — is a row among them and the subject of the
//! second suite, which reads the execution's own scan statistics back.
//!
//! Every object written here is the real thing: the data files are Parquet files written by
//! the Apache implementation of that format, and the manifest lists and manifests are Avro
//! containers written by the Apache implementation of that one, each carrying the members the
//! Iceberg table specification declares — the same objects the writers of such a table in
//! this emulator produce.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use apache_avro::types::Value as AvroValue;
use apache_avro::{Schema as AvroSchema, Writer as AvroWriter};
use parquet::basic::{Compression, ConvertedType, Repetition, Type as PhysicalType};
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type as SchemaType;
use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::catalog_sql::{
    ERROR_TYPE_GENERIC_USER, ERROR_TYPE_LOCATION_NOT_FOUND,
};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its objects in.
const BUCKET: &str = "athena-iceberg-lake";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-iceberg-lake/results/";

/// The database every table here lives in.
const DATABASE: &str = "iceberg_lake";

/// The serialization library an Iceberg table's storage descriptor carries.
const ICEBERG_SERDE: &str = "org.apache.iceberg.mr.hive.HiveIcebergSerDe";

/// The `InputFormat` an Iceberg table's storage descriptor carries.
const ICEBERG_INPUT_FORMAT: &str = "org.apache.iceberg.mr.hive.HiveIcebergInputFormat";

/// How long a poll waits for a started query to settle. Generous: what is asserted is that
/// the execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by. Nothing is verified: this harness turns request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service: the catalog Glue holds is the catalog Athena resolves against, and the objects S3
/// holds are the data Athena reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsJson1_1 call to `service`, and its HTTP status and parsed body.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Glue call that must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "glue", "AWSGlue", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// One Athena call that must answer 200.
async fn athena_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object into [`BUCKET`].
async fn put_object(router: &MultiServiceRouter, case: &str, key: &str, body: Vec<u8>) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}/{key}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{BUCKET}/{key} must succeed, got {}",
        response.status()
    );
}

/// Create the bucket and the database every suite here starts from.
async fn given_bucket_and_database(router: &MultiServiceRouter, case: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: creating s3://{BUCKET} must succeed, got {}",
        response.status()
    );
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
}

/// The columns every table in these suites declares, as the catalog records them.
fn declared_columns() -> Value {
    json!([
        { "Name": "item_id", "Type": "bigint" },
        { "Name": "name", "Type": "string" },
    ])
}

/// Register `table` as an Iceberg table whose metadata lives at `metadata_location`: the
/// marker property an engine that writes such a table records, the pointer to the metadata
/// document, and Iceberg's own storage descriptor.
async fn given_iceberg_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    metadata_location: &str,
) {
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "Parameters": {
                    "table_type": "ICEBERG",
                    "metadata_location": metadata_location,
                    "format-version": "2",
                },
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": declared_columns(),
                    "InputFormat": ICEBERG_INPUT_FORMAT,
                    "SerdeInfo": { "SerializationLibrary": ICEBERG_SERDE },
                },
            },
        }),
    )
    .await;
}

/// Start `sql` and answer its `QueryExecutionId`.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let started = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    started["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {started}"))
        .to_string()
}

/// Poll `id` until its execution is terminal, and answer the whole `QueryExecution` view plus
/// the detail projection beside it.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = athena_ok(
            router,
            case,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        let state = body["QueryExecution"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never reached a terminal state, last saw {body}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql` to completion, require SUCCEEDED, and answer its data rows: one entry per row,
/// each cell `None` where the datum carries no value and `Some(text)` otherwise.
async fn rows_of(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
) -> Vec<Vec<Option<String>>> {
    let id = start(router, case, sql).await;
    let settled = settled(router, case, &id).await;
    assert_eq!(
        settled["QueryExecution"]["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {}",
        settled["QueryExecution"]["Status"]
    );
    let body = athena_ok(
        router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    body["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        // The first row of a DML result set's first page is the column labels.
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| {
                    datum["VarCharValue"]
                        .as_str()
                        .map(::std::string::ToString::to_string)
                })
                .collect()
        })
        .collect()
}

/// One DATA FILE at a table's location: the object it is written as, the records it holds,
/// and whether the manifest entry naming it is one the snapshot REMOVED.
#[derive(Debug, Clone)]
struct Written {
    /// The object's name under the table's `data/` directory.
    name: &'static str,
    /// The records the file holds, each as `(item_id, name)`.
    records: Vec<(i64, &'static str)>,
    /// Whether the entry naming it carries the status of a file the snapshot removed.
    removed: bool,
}

impl Written {
    /// A file whose entry the snapshot holds as added.
    fn added(name: &'static str, records: &[(i64, &'static str)]) -> Self {
        Self {
            name,
            records: records.to_vec(),
            removed: false,
        }
    }

    /// A file whose entry the snapshot holds as removed.
    fn removed(name: &'static str, records: &[(i64, &'static str)]) -> Self {
        Self {
            name,
            records: records.to_vec(),
            removed: true,
        }
    }
}

/// One COMMIT of a table: the manifests the snapshot it wrote names, each holding the entries
/// of the files it names.
#[derive(Debug, Clone)]
struct Commit {
    /// One entry list per manifest this commit's snapshot names.
    manifests: Vec<Vec<Written>>,
}

/// One row of the visibility table: how a table's metadata stands, what is written at its
/// location, and the rows a read of it must see.
struct SnapshotCase {
    /// What this row is about.
    case: &'static str,
    /// The table the read addresses.
    table: &'static str,
    /// The commits the table's metadata holds, in order; the LAST one is the snapshot the
    /// document reports as current. EMPTY for a table whose metadata names no current
    /// snapshot, which is the table a create has just minted.
    commits: Vec<Commit>,
    /// Data files written at the table's location that no manifest entry names at all.
    uncommitted: Vec<Written>,
    /// The rows a read of the table must see, ordered by `item_id`.
    expected: Vec<(i64, &'static str)>,
}

/// The Avro schema of a MANIFEST, as Iceberg's manifest-entry schema declares it.
const MANIFEST_ENTRY_SCHEMA: &str = r#"{
  "type": "record",
  "name": "manifest_entry",
  "fields": [
    { "name": "status", "type": "int", "field-id": 0 },
    { "name": "snapshot_id", "type": ["null", "long"], "default": null, "field-id": 1 },
    { "name": "sequence_number", "type": ["null", "long"], "default": null, "field-id": 3 },
    { "name": "data_file", "field-id": 2, "type": {
        "type": "record",
        "name": "r2",
        "fields": [
          { "name": "content", "type": "int", "field-id": 134 },
          { "name": "file_path", "type": "string", "field-id": 100 },
          { "name": "file_format", "type": "string", "field-id": 101 },
          { "name": "partition", "field-id": 102, "type": {
              "type": "record", "name": "r102", "fields": []
          } },
          { "name": "record_count", "type": "long", "field-id": 103 },
          { "name": "file_size_in_bytes", "type": "long", "field-id": 104 }
        ]
    } }
  ]
}"#;

/// The Avro schema of a MANIFEST LIST, as Iceberg's manifest-file schema declares it.
const MANIFEST_FILE_SCHEMA: &str = r#"{
  "type": "record",
  "name": "manifest_file",
  "fields": [
    { "name": "manifest_path", "type": "string", "field-id": 500 },
    { "name": "manifest_length", "type": "long", "field-id": 501 },
    { "name": "partition_spec_id", "type": "int", "field-id": 502 },
    { "name": "content", "type": "int", "field-id": 517 },
    { "name": "sequence_number", "type": "long", "field-id": 515 },
    { "name": "added_snapshot_id", "type": "long", "field-id": 503 },
    { "name": "added_files_count", "type": "int", "field-id": 504 },
    { "name": "added_rows_count", "type": "long", "field-id": 512 }
  ]
}"#;

/// The `content` of a DATA file, and of a manifest holding data files: Iceberg's own value
/// for data rather than for the removals a delete file carries.
const CONTENT_DATA: i32 = 0;

/// The `status` of a manifest entry a commit ADDED, and of one the snapshot REMOVED, from
/// Iceberg's manifest-entry status enumeration.
const STATUS_ADDED: i32 = 1;
/// See [`STATUS_ADDED`].
const STATUS_DELETED: i32 = 2;

/// One count as the number Avro and Iceberg state one as.
fn counted(count: usize) -> i64 {
    i64::try_from(count).expect("a count that fits")
}

/// One Parquet object holding `records` as the two columns every table here declares, written
/// by the format's own writer.
fn parquet_object(records: &[(i64, &'static str)]) -> Vec<u8> {
    let fields: Vec<Arc<SchemaType>> = vec![
        Arc::new(
            SchemaType::primitive_type_builder("item_id", PhysicalType::INT64)
                .with_repetition(Repetition::OPTIONAL)
                .build()
                .expect("a well-formed leaf"),
        ),
        Arc::new(
            SchemaType::primitive_type_builder("name", PhysicalType::BYTE_ARRAY)
                .with_repetition(Repetition::OPTIONAL)
                .with_converted_type(ConvertedType::UTF8)
                .build()
                .expect("a well-formed leaf"),
        ),
    ];
    let schema = SchemaType::group_type_builder("record")
        .with_fields(fields)
        .build()
        .expect("a well-formed message type");
    let properties = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer =
        SerializedFileWriter::new(Vec::new(), Arc::new(schema), Arc::new(properties))
            .expect("a writer over a buffer");
    {
        let mut group = writer.next_row_group().expect("a row group");
        // An OPTIONAL leaf carries a definition level per record; every record here holds a
        // value for both columns.
        let definitions: Vec<i16> = records.iter().map(|_| 1).collect();
        let mut leaf = group
            .next_column()
            .expect("the next column")
            .expect("one column per leaf");
        let ids: Vec<i64> = records.iter().map(|(id, _)| *id).collect();
        leaf.typed::<Int64Type>()
            .write_batch(&ids, Some(&definitions), None)
            .expect("the leaf takes its values");
        leaf.close().expect("the column closes");
        let mut leaf = group
            .next_column()
            .expect("the next column")
            .expect("one column per leaf");
        let names: Vec<ByteArray> = records
            .iter()
            .map(|(_, name)| ByteArray::from(name.as_bytes().to_vec()))
            .collect();
        leaf.typed::<ByteArrayType>()
            .write_batch(&names, Some(&definitions), None)
            .expect("the leaf takes its values");
        leaf.close().expect("the column closes");
        group.close().expect("the row group closes");
    }
    writer.into_inner().expect("the file's footer")
}

/// One MANIFEST object: one entry per file, each naming the data object it is about, the
/// records that object holds and the status the snapshot gives the entry.
fn manifest_object(
    files: &[Written],
    sizes: &BTreeMap<&'static str, usize>,
    table: &str,
    snapshot_id: i64,
) -> Vec<u8> {
    let schema = AvroSchema::parse_str(MANIFEST_ENTRY_SCHEMA).expect("the manifest schema");
    let mut writer = AvroWriter::new(&schema, Vec::new());
    for file in files {
        let data_file = AvroValue::Record(vec![
            ("content".to_string(), AvroValue::Int(CONTENT_DATA)),
            (
                "file_path".to_string(),
                AvroValue::String(data_path(table, file.name)),
            ),
            (
                "file_format".to_string(),
                AvroValue::String("PARQUET".to_string()),
            ),
            ("partition".to_string(), AvroValue::Record(Vec::new())),
            (
                "record_count".to_string(),
                AvroValue::Long(counted(file.records.len())),
            ),
            (
                "file_size_in_bytes".to_string(),
                AvroValue::Long(counted(sizes.get(file.name).copied().unwrap_or(0))),
            ),
        ]);
        let status = if file.removed {
            STATUS_DELETED
        } else {
            STATUS_ADDED
        };
        let entry = AvroValue::Record(vec![
            ("status".to_string(), AvroValue::Int(status)),
            (
                "snapshot_id".to_string(),
                AvroValue::Union(1, Box::new(AvroValue::Long(snapshot_id))),
            ),
            (
                "sequence_number".to_string(),
                AvroValue::Union(1, Box::new(AvroValue::Long(1))),
            ),
            ("data_file".to_string(), data_file),
        ]);
        writer.append(entry).expect("the manifest takes its entry");
    }
    writer.into_inner().expect("the manifest container")
}

/// One MANIFEST LIST object: one entry per manifest the snapshot names.
fn manifest_list_object(manifests: &[(String, usize, usize)], snapshot_id: i64) -> Vec<u8> {
    let schema = AvroSchema::parse_str(MANIFEST_FILE_SCHEMA).expect("the manifest-list schema");
    let mut writer = AvroWriter::new(&schema, Vec::new());
    for (path, length, records) in manifests {
        let entry = AvroValue::Record(vec![
            ("manifest_path".to_string(), AvroValue::String(path.clone())),
            (
                "manifest_length".to_string(),
                AvroValue::Long(counted(*length)),
            ),
            ("partition_spec_id".to_string(), AvroValue::Int(0)),
            ("content".to_string(), AvroValue::Int(CONTENT_DATA)),
            ("sequence_number".to_string(), AvroValue::Long(1)),
            ("added_snapshot_id".to_string(), AvroValue::Long(snapshot_id)),
            ("added_files_count".to_string(), AvroValue::Int(1)),
            (
                "added_rows_count".to_string(),
                AvroValue::Long(counted(*records)),
            ),
        ]);
        writer
            .append(entry)
            .expect("the manifest list takes its entry");
    }
    writer.into_inner().expect("the manifest-list container")
}

/// The METADATA DOCUMENT of a table at `location` whose snapshots are `snapshots`, each
/// `(snapshot id, the manifest list it names)`; the LAST of them is the current one, and a
/// document with none reports no current snapshot at all.
fn metadata_document(location: &str, snapshots: &[(i64, String)]) -> Vec<u8> {
    let held: Vec<Value> = snapshots
        .iter()
        .enumerate()
        .map(|(index, (id, manifest_list))| {
            json!({
                "snapshot-id": id,
                "sequence-number": counted(index + 1),
                "timestamp-ms": 1_767_225_600_000_i64 + counted(index),
                "manifest-list": manifest_list,
                "schema-id": 0,
                "summary": { "operation": "append" },
            })
        })
        .collect();
    let current = snapshots.last().map(|(id, _)| *id);
    let document = json!({
        "format-version": 2,
        "table-uuid": "8f2b2d4e-5c1a-4f9b-9c3d-0a1b2c3d4e5f",
        "location": location.trim_end_matches('/'),
        "last-sequence-number": counted(snapshots.len()),
        "last-updated-ms": 1_767_225_600_000_i64,
        "last-column-id": 2,
        "current-schema-id": 0,
        "schemas": [{
            "type": "struct",
            "schema-id": 0,
            "fields": [
                { "id": 1, "name": "item_id", "required": false, "type": "long" },
                { "id": 2, "name": "name", "required": false, "type": "string" },
            ],
        }],
        "default-spec-id": 0,
        "partition-specs": [{ "spec-id": 0, "fields": [] }],
        "last-partition-id": 999,
        "default-sort-order-id": 0,
        "sort-orders": [{ "order-id": 0, "fields": [] }],
        "properties": {},
        // Iceberg's own "no current snapshot" value for a table nothing has committed to.
        "current-snapshot-id": current.unwrap_or(-1),
        "refs": match current {
            Some(id) => json!({ "main": { "snapshot-id": id, "type": "branch" } }),
            None => json!({}),
        },
        "snapshots": held,
        "snapshot-log": [],
        "metadata-log": [],
    });
    serde_json::to_vec(&document).expect("the metadata document")
}

/// Where one data file of `table` lives.
fn data_path(table: &str, name: &str) -> String {
    format!("s3://{BUCKET}/{table}/data/{name}")
}

/// Where `table`'s metadata document lives.
fn metadata_path(table: &str) -> String {
    format!("s3://{BUCKET}/{table}/metadata/00000-{table}.metadata.json")
}

/// Lay one case's table out in the object store and register it: every data file as a Parquet
/// object, every manifest and manifest list as an Avro container, the metadata document
/// naming the snapshots, and the catalog entry pointing at it.
async fn given_case(router: &MultiServiceRouter, case: &SnapshotCase) {
    // Every data file the case names, written once each: the ones its manifests name and the
    // ones nothing names.
    let mut files: BTreeMap<&'static str, Vec<(i64, &'static str)>> = BTreeMap::new();
    for written in case
        .commits
        .iter()
        .flat_map(|commit| commit.manifests.iter().flatten())
        .chain(case.uncommitted.iter())
    {
        files
            .entry(written.name)
            .or_insert_with(|| written.records.clone());
    }
    let mut sizes: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (name, records) in &files {
        let body = parquet_object(records);
        sizes.insert(name, body.len());
        put_object(
            router,
            case.case,
            &format!("{}/data/{name}", case.table),
            body,
        )
        .await;
    }
    let mut snapshots: Vec<(i64, String)> = Vec::new();
    for (index, commit) in case.commits.iter().enumerate() {
        let snapshot_id = 7_000 + counted(index);
        let mut manifests: Vec<(String, usize, usize)> = Vec::new();
        for (ordinal, entries) in commit.manifests.iter().enumerate() {
            let body = manifest_object(entries, &sizes, case.table, snapshot_id);
            let key = format!("{}/metadata/{index}-{ordinal}-m0.avro", case.table);
            let records = entries.iter().map(|file| file.records.len()).sum();
            manifests.push((
                format!("s3://{BUCKET}/{key}"),
                body.len(),
                records,
            ));
            put_object(router, case.case, &key, body).await;
        }
        let list = manifest_list_object(&manifests, snapshot_id);
        let key = format!("{}/metadata/snap-{snapshot_id}-1.avro", case.table);
        put_object(router, case.case, &key, list).await;
        snapshots.push((snapshot_id, format!("s3://{BUCKET}/{key}")));
    }
    let location = format!("s3://{BUCKET}/{}/", case.table);
    let document = metadata_document(&location, &snapshots);
    let metadata = metadata_path(case.table);
    put_object(
        router,
        case.case,
        &format!("{}/metadata/00000-{}.metadata.json", case.table, case.table),
        document,
    )
    .await;
    given_iceberg_table(router, case.case, case.table, &metadata).await;
}

/// A read of an Iceberg table sees exactly the rows the data files of its CURRENT COMMITTED
/// SNAPSHOT hold — whatever else sits at its location.
#[tokio::test]
async fn an_iceberg_read_sees_the_rows_its_current_snapshot_holds() {
    let router = emulator();
    given_bucket_and_database(&router, "the lake").await;

    let cases: Vec<SnapshotCase> = vec![
        SnapshotCase {
            case: "a table nothing has committed to, with a data file already at its location",
            table: "minted",
            commits: Vec::new(),
            uncommitted: vec![Written::added("part-0.parquet", &[(1, "desk")])],
            expected: Vec::new(),
        },
        SnapshotCase {
            case: "a snapshot that names no manifest",
            table: "empty_snapshot",
            commits: vec![Commit {
                manifests: Vec::new(),
            }],
            uncommitted: vec![Written::added("part-0.parquet", &[(1, "desk")])],
            expected: Vec::new(),
        },
        SnapshotCase {
            case: "one commit of one file holding two records",
            table: "one_commit",
            commits: vec![Commit {
                manifests: vec![vec![Written::added(
                    "part-0.parquet",
                    &[(1, "desk"), (2, "lamp")],
                )]],
            }],
            uncommitted: Vec::new(),
            expected: vec![(1, "desk"), (2, "lamp")],
        },
        SnapshotCase {
            case: "a second file at the location that no entry names",
            table: "half_committed",
            commits: vec![Commit {
                manifests: vec![vec![Written::added("part-0.parquet", &[(1, "desk")])]],
            }],
            uncommitted: vec![Written::added(
                "part-1.parquet",
                &[(2, "lamp"), (3, "chair")],
            )],
            expected: vec![(1, "desk")],
        },
        SnapshotCase {
            case: "two manifests in one snapshot",
            table: "two_manifests",
            commits: vec![Commit {
                manifests: vec![
                    vec![Written::added("part-0.parquet", &[(1, "desk")])],
                    vec![Written::added("part-1.parquet", &[(2, "lamp"), (3, "chair")])],
                ],
            }],
            uncommitted: Vec::new(),
            expected: vec![(1, "desk"), (2, "lamp"), (3, "chair")],
        },
        SnapshotCase {
            case: "a later commit, whose snapshot is the current one",
            table: "two_commits",
            commits: vec![
                Commit {
                    manifests: vec![vec![Written::added("part-0.parquet", &[(1, "desk")])]],
                },
                Commit {
                    manifests: vec![vec![Written::added("part-1.parquet", &[(2, "lamp")])]],
                },
            ],
            uncommitted: Vec::new(),
            expected: vec![(2, "lamp")],
        },
        SnapshotCase {
            case: "an entry the snapshot removed, beside one it added",
            table: "with_removal",
            commits: vec![Commit {
                manifests: vec![vec![
                    Written::added("part-0.parquet", &[(1, "desk")]),
                    Written::removed("part-1.parquet", &[(2, "lamp"), (3, "chair")]),
                ]],
            }],
            uncommitted: Vec::new(),
            expected: vec![(1, "desk")],
        },
    ];

    for case in &cases {
        given_case(&router, case).await;
    }

    for case in &cases {
        let sql = format!(
            "SELECT item_id, name FROM \"{DATABASE}\".\"{}\" ORDER BY item_id",
            case.table
        );
        let actual = rows_of(&router, case.case, &sql).await;
        let expected: Vec<Vec<Option<String>>> = case
            .expected
            .iter()
            .map(|(id, name)| {
                vec![Some(id.to_string()), Some((*name).to_string())]
            })
            .collect();
        assert_eq!(
            actual, expected,
            "case {:?}: expected the rows {expected:?}, got {actual:?}",
            case.case
        );

        // The same visibility decision is what an AGGREGATE over the table is computed from,
        // which is how a client asks whether the table is still empty.
        let counted = rows_of(
            &router,
            case.case,
            &format!(
                "SELECT count(*) FROM \"{DATABASE}\".\"{}\"",
                case.table
            ),
        )
        .await;
        let expected_count = vec![vec![Some(case.expected.len().to_string())]];
        assert_eq!(
            counted, expected_count,
            "case {:?}: expected the count {expected_count:?}, got {counted:?}",
            case.case
        );
    }
}

/// An aggregate over a table a create has just minted answers zero and the execution reports
/// that it went through no data at all — even though an object already sits at the table's
/// location, because no snapshot of the table names it.
///
/// The table is minted the way a client mints one: Glue's own open-table-format create, which
/// records the marker and the metadata pointer and writes the metadata document the read goes
/// through.
#[tokio::test]
async fn an_aggregate_over_a_minted_table_scans_nothing() {
    let router = emulator();
    let case = "a table a create has just minted";
    given_bucket_and_database(&router, case).await;
    glue_ok(
        &router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": "minted_by_glue",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/minted_by_glue/"),
                    "Columns": declared_columns(),
                },
            },
            "OpenTableFormatInput": {
                "IcebergInput": { "MetadataOperation": "CREATE", "Version": "2" },
            },
        }),
    )
    .await;
    // A data object at the table's location that no snapshot names: written as a Parquet file
    // holding one record, exactly as a writer that has not committed yet leaves one.
    put_object(
        &router,
        case,
        "minted_by_glue/data/part-0.parquet",
        parquet_object(&[(1, "desk")]),
    )
    .await;

    let id = start(
        &router,
        case,
        &format!("SELECT count(*) AS n FROM \"{DATABASE}\".\"minted_by_glue\""),
    )
    .await;
    let settled = settled(&router, case, &id).await;
    assert_eq!(
        settled["QueryExecution"]["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the aggregate must succeed, got {}",
        settled["QueryExecution"]["Status"]
    );
    let results = athena_ok(
        &router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 10 }),
    )
    .await;
    let counted = results["ResultSet"]["Rows"]
        .as_array()
        .and_then(|rows| rows.last())
        .map(|row| row["Data"][0]["VarCharValue"].clone());
    assert_eq!(
        counted,
        Some(json!("0")),
        "case {case:?}: the aggregate must count the committed rows, got {results}"
    );
    assert_eq!(
        settled["QueryExecution"]["Statistics"]["DataScannedInBytes"].as_i64(),
        Some(0),
        "case {case:?}: a read that reached no data file scans nothing, got {}",
        settled["QueryExecution"]["Statistics"]
    );
    let stats = settled["QueryExecutionDetail"]["Stats"].clone();
    assert_eq!(
        (
            stats["ProcessedBytes"].as_i64(),
            stats["ProcessedRows"].as_i64(),
            stats["ProcessedInputRows"].as_i64(),
        ),
        (Some(0), Some(0), Some(0)),
        "case {case:?}: a read that reached no data file processed nothing, got {stats}"
    );
}

/// A pointer at a metadata document the object store does not hold is a table whose metadata
/// cannot be dereferenced: the read fails in the FILESYSTEM class, which is neither the
/// generic user error an entry naming NO metadata location is, nor a table that is not there.
#[tokio::test]
async fn an_absent_metadata_document_fails_in_the_filesystem_class() {
    let router = emulator();
    let case = "a metadata pointer at an object nothing wrote";
    given_bucket_and_database(&router, case).await;
    given_iceberg_table(
        &router,
        case,
        "dangling",
        &metadata_path("dangling"),
    )
    .await;
    put_object(
        &router,
        case,
        "dangling/data/part-0.parquet",
        parquet_object(&[(1, "desk")]),
    )
    .await;

    let id = start(
        &router,
        case,
        &format!("SELECT count(*) FROM \"{DATABASE}\".\"dangling\""),
    )
    .await;
    let settled = settled(&router, case, &id).await;
    let status = settled["QueryExecution"]["Status"].clone();
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: the read must fail, got {status}"
    );
    assert_eq!(
        status["AthenaError"]["ErrorType"].as_i64(),
        Some(i64::from(ERROR_TYPE_LOCATION_NOT_FOUND)),
        "case {case:?}: expected the filesystem class, got {status}"
    );
    assert_ne!(
        status["AthenaError"]["ErrorType"].as_i64(),
        Some(i64::from(ERROR_TYPE_GENERIC_USER)),
        "case {case:?}: this is not the class of an entry naming no metadata location, \
         got {status}"
    );
    let message = status["AthenaError"]["ErrorMessage"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.contains(&metadata_path("dangling")),
        "case {case:?}: the failure must name the metadata object, got {message:?}"
    );
}
