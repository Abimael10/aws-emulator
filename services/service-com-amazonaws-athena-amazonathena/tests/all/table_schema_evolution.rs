// SPDX-License-Identifier: CC0-1.0

//! Changing the SCHEMA of a Data Catalog table, and registering the partitions its location
//! already holds the data of — driven end to end over the wire through the same router the
//! emulator boots.
//!
//! THE RULES UNDER TEST:
//!
//! * Athena serves the Hive DDL statements that change a table's definition —
//!   `ALTER TABLE … ADD COLUMNS`, `ALTER TABLE … REPLACE COLUMNS` and
//!   `ALTER TABLE … SET TBLPROPERTIES` — against the Data Catalog entry, and
//!   `MSCK REPAIR TABLE` registers every Hive-style partition directory under the table's
//!   location that the catalog has no record of. Each settles SUCCEEDED and changes nothing
//!   about the OBJECTS already there.
//! * A column of a self-describing columnar object is then resolved against the file by its
//!   NAME — or, where the table's properties declare index access, by its POSITION — and read
//!   as its declared type when that type can hold the stored value: a column the file does not
//!   carry is NULL, a wider declaration over a narrower stored number still reads, a
//!   declaration of another type family cannot be read from those bytes, and a stored value the
//!   declared type has no room for is not truncated to fit.
//!
//! Every object here is a REAL Parquet file, written by the Apache implementation of the
//! format through its own row-group and column writers, so what a read decodes is a file any
//! Parquet reader reads and never bytes assembled to look like one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parquet::basic::{Compression, ConvertedType, Repetition, Type as PhysicalType};
use parquet::data_type::{ByteArray, ByteArrayType, Int32Type, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type as SchemaType;
use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its objects in.
const BUCKET: &str = "athena-evolution-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-evolution-data/results/";

/// The database every table here lives in.
const DATABASE: &str = "evolving_lake";

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
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, "athena", "AmazonAthena", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object into [`BUCKET`], whose bytes are what a table's location then holds.
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

/// Register `table` over `location`, declaring `columns` as `(name, Hive type)` and the
/// Parquet storage format, plus the partition keys `partitions`.
async fn given_parquet_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    columns: &[(&str, &str)],
    partitions: &[(&str, &str)],
) {
    let column_list = |columns: &[(&str, &str)]| {
        columns
            .iter()
            .map(|(name, hive)| json!({ "Name": name, "Type": hive }))
            .collect::<Vec<Value>>()
    };
    glue_ok(
        router,
        case,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "TableType": "EXTERNAL_TABLE",
                "PartitionKeys": column_list(partitions),
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": column_list(columns),
                    "InputFormat":
                        "org.apache.hadoop.hive.ql.io.parquet.MapredParquetInputFormat",
                    "OutputFormat":
                        "org.apache.hadoop.hive.ql.io.parquet.MapredParquetOutputFormat",
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.ql.io.parquet.serde.ParquetHiveSerDe",
                    },
                },
            },
        }),
    )
    .await;
}

/// Run `sql` to completion and answer its terminal `Status` beside the execution's id.
async fn settled(router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
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
    let id = started["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {started}"))
        .to_string();
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let body = athena_ok(
            router,
            case,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return json!({ "Status": reported, "QueryExecutionId": id });
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql`, require SUCCEEDED, and answer its rows: one entry per data row, each cell
/// `None` where the datum carries no value (SQL NULL) and `Some(text)` otherwise.
///
/// `labelled` says whether the first row of the first page is the column-label row Athena
/// puts in front of a DML query's data rows — a statement it classifies as DDL (`SHOW
/// PARTITIONS` among them) writes no such row, so its first row is data.
async fn answered(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
    labelled: bool,
) -> Vec<Vec<Option<String>>> {
    let settled = settled(router, case, sql).await;
    assert_eq!(
        settled["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {}",
        settled["Status"]
    );
    let id = settled["QueryExecutionId"].as_str().unwrap_or_default();
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
        .skip(usize::from(labelled))
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

/// The rows a statement Athena classifies as DML — a projection — answers with.
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Vec<Vec<Option<String>>> {
    answered(router, case, sql, true).await
}

/// The rows a statement Athena classifies as DDL answers with, which carry no label row.
async fn ddl_rows_of(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
) -> Vec<Vec<Option<String>>> {
    answered(router, case, sql, false).await
}

/// Run `sql`, require it to settle SUCCEEDED, and answer nothing: what a DDL statement
/// produces is the change it made to the catalog, which the caller then reads back.
async fn performed(router: &MultiServiceRouter, case: &str, sql: &str) {
    let settled = settled(router, case, sql).await;
    assert_eq!(
        settled["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {}",
        settled["Status"]
    );
}

/// Run `sql`, require it to settle FAILED, and answer the reason it reported.
async fn failure_of(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let settled = settled(router, case, sql).await;
    assert_eq!(
        settled["Status"]["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: {sql} must fail, got {}",
        settled["Status"]
    );
    settled["Status"]["StateChangeReason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The leaf one column's values are written as in the files below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    /// A byte array annotated as UTF-8 text.
    Utf8,
    /// A 32-bit integer.
    Int32,
    /// A 64-bit integer.
    Int64,
}

/// One value a record carries.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    /// Text, for a UTF-8 leaf.
    Text(String),
    /// A 32-bit number.
    Int(i32),
    /// A 64-bit number.
    Long(i64),
}

/// One Parquet object holding `records`, whose leaves are `columns` as `(name, leaf)`.
fn parquet_object(columns: &[(&str, Leaf)], records: &[Vec<Cell>]) -> Vec<u8> {
    let fields: Vec<Arc<SchemaType>> = columns
        .iter()
        .map(|(name, leaf)| {
            let (physical, converted) = match leaf {
                Leaf::Utf8 => (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
                Leaf::Int32 => (PhysicalType::INT32, ConvertedType::NONE),
                Leaf::Int64 => (PhysicalType::INT64, ConvertedType::NONE),
            };
            Arc::new(
                SchemaType::primitive_type_builder(name, physical)
                    .with_repetition(Repetition::OPTIONAL)
                    .with_converted_type(converted)
                    .build()
                    .expect("a well-formed leaf"),
            )
        })
        .collect();
    let schema = SchemaType::group_type_builder("record")
        .with_fields(fields)
        .build()
        .expect("a well-formed message type");
    let properties = WriterProperties::builder()
        .set_compression(Compression::UNCOMPRESSED)
        .build();
    let mut writer = SerializedFileWriter::new(Vec::new(), Arc::new(schema), Arc::new(properties))
        .expect("a writer over a buffer");
    {
        let mut group = writer.next_row_group().expect("a row group");
        for (index, (_, leaf)) in columns.iter().enumerate() {
            let mut column = group
                .next_column()
                .expect("the next column")
                .expect("one column per leaf");
            let cells: Vec<Cell> = records
                .iter()
                .map(|record| {
                    record
                        .get(index)
                        .cloned()
                        .unwrap_or(Cell::Text(String::new()))
                })
                .collect();
            // Every record carries a value for every leaf here, which an OPTIONAL leaf
            // records with the definition level that says so.
            let definitions: Vec<i16> = cells.iter().map(|_| 1).collect();
            let definitions = Some(definitions.as_slice());
            match leaf {
                Leaf::Utf8 => {
                    let values: Vec<ByteArray> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Text(text) => Some(ByteArray::from(text.as_bytes().to_vec())),
                            _ => None,
                        })
                        .collect();
                    column
                        .typed::<ByteArrayType>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Int32 => {
                    let values: Vec<i32> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Int(value) => Some(*value),
                            _ => None,
                        })
                        .collect();
                    column
                        .typed::<Int32Type>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Int64 => {
                    let values: Vec<i64> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Long(value) => Some(*value),
                            _ => None,
                        })
                        .collect();
                    column
                        .typed::<Int64Type>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
            }
            column.close().expect("the column closes");
        }
        group.close().expect("the row group closes");
    }
    writer.into_inner().expect("the file's footer")
}

/// The leaves the files of the evolving table carry: a 32-bit number, text, and a 64-bit
/// number.
const STORED_LEAVES: &[(&str, Leaf)] = &[("id", Leaf::Int32), ("name", Leaf::Utf8), ("size", Leaf::Int64)];

/// The one record those files hold.
fn stored_record() -> Vec<Vec<Cell>> {
    vec![vec![
        Cell::Int(7),
        Cell::Text("desk".to_string()),
        Cell::Long(4_000_000_000),
    ]]
}

/// The cell `text` names, for an expectation written as text.
fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

/// Each statement that CHANGES a table's column list is performed against the catalog, and
/// the objects already at the table's location are then read through the new declaration:
/// every column is resolved by NAME, one the files do not carry is NULL, and a wider
/// declaration over a narrower stored number still reads its values.
///
/// The rows differ along the change made (a column added, the list replaced to widen a type,
/// to drop a column, to reorder the columns, and to re-add a dropped one), so each row's
/// projection reads the SAME file through a different declaration.
#[tokio::test]
async fn a_column_list_change_is_read_against_the_objects_already_there() {
    let case = "a column list changed over the objects already there";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(
        &router,
        case,
        "stock",
        &[("id", "int"), ("name", "string"), ("size", "bigint")],
        &[],
    )
    .await;
    put_object(
        &router,
        case,
        "stock/part-0.parquet",
        parquet_object(STORED_LEAVES, &stored_record()),
    )
    .await;

    /// One row of the change table: what the row is about, the statement that changes the
    /// table (where the row makes one), the projection to read it back with, and the one row
    /// that projection must answer.
    type Change = (
        &'static str,
        Option<&'static str>,
        &'static str,
        Vec<Option<String>>,
    );

    // (case, the statement that changes the table, the projection, the row it answers)
    let cases: Vec<Change> = vec![
        (
            "the declaration the table was registered with",
            None,
            "SELECT id, name, size FROM stock",
            vec![some("7"), some("desk"), some("4000000000")],
        ),
        (
            "a column added, which no older file carries",
            Some("ALTER TABLE stock ADD COLUMNS (label string)"),
            "SELECT id, label FROM stock",
            vec![some("7"), None],
        ),
        (
            "the list replaced to widen the stored 32-bit number",
            Some("ALTER TABLE stock REPLACE COLUMNS (id bigint, name string, size bigint)"),
            "SELECT id, size FROM stock",
            vec![some("7"), some("4000000000")],
        ),
        (
            "the list replaced to drop a column",
            Some("ALTER TABLE stock REPLACE COLUMNS (id bigint, size bigint)"),
            "SELECT id, size FROM stock",
            vec![some("7"), some("4000000000")],
        ),
        (
            "the dropped column re-added, which recovers its stored values",
            Some("ALTER TABLE stock ADD COLUMNS (name string)"),
            "SELECT id, name FROM stock",
            vec![some("7"), some("desk")],
        ),
        (
            "the list replaced in another order than the file declares",
            Some("ALTER TABLE stock REPLACE COLUMNS (size bigint, name string, id int)"),
            "SELECT id, name, size FROM stock",
            vec![some("7"), some("desk"), some("4000000000")],
        ),
    ];

    for (case, change, projection, expected) in cases {
        if let Some(change) = change {
            performed(&router, case, change).await;
        }
        let rows = rows_of(&router, case, projection).await;
        assert_eq!(
            rows,
            vec![expected.clone()],
            "case {case:?}: expected the single row {expected:?}, got {rows:?}"
        );
    }
}

/// What a column-list change wrote is the CATALOG's own entry, so the Data Catalog reports
/// the changed definition: `ADD COLUMNS` appends to the columns it already held and
/// `REPLACE COLUMNS` is the whole list from then on.
#[tokio::test]
async fn a_column_list_change_is_what_the_catalog_reports() {
    let case = "the catalog reports the changed column list";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(
        &router,
        case,
        "ledger",
        &[("id", "int"), ("name", "string")],
        &[],
    )
    .await;

    let declared = |table: &Value| -> Vec<(String, String)> {
        table["Table"]["StorageDescriptor"]["Columns"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|column| {
                (
                    column["Name"].as_str().unwrap_or_default().to_string(),
                    column["Type"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };
    let read_back = || async {
        glue_ok(
            &router,
            case,
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": "ledger" }),
        )
        .await
    };

    performed(
        &router,
        case,
        "ALTER TABLE ledger ADD COLUMNS (label string COMMENT 'what it is')",
    )
    .await;
    let appended = declared(&read_back().await);
    assert_eq!(
        appended,
        vec![
            ("id".to_string(), "int".to_string()),
            ("name".to_string(), "string".to_string()),
            ("label".to_string(), "string".to_string()),
        ],
        "case {case:?}: expected the appended column list, got {appended:?}"
    );

    performed(
        &router,
        case,
        "ALTER TABLE ledger REPLACE COLUMNS (id bigint, label string)",
    )
    .await;
    let replaced = declared(&read_back().await);
    assert_eq!(
        replaced,
        vec![
            ("id".to_string(), "bigint".to_string()),
            ("label".to_string(), "string".to_string()),
        ],
        "case {case:?}: expected the replaced column list, got {replaced:?}"
    );

    // No column list the catalog holds carries one name twice, whether the second
    // declaration collides with a column the table already held or with another of the
    // statement's own — and the list the entry holds is left exactly as it was.
    for (sql, twice) in [
        ("ALTER TABLE ledger ADD COLUMNS (label string)", "label"),
        ("ALTER TABLE ledger ADD COLUMNS (n int, n bigint)", "n"),
        (
            "ALTER TABLE ledger REPLACE COLUMNS (id int, ID string)",
            // The catalog addresses a column name without regard to case, and folds the name
            // it records, so the second declaration of this name is the same column.
            "id",
        ),
    ] {
        let refused = failure_of(&router, sql, sql).await;
        let expected = format!("Duplicate column name: {twice}");
        assert!(
            refused.contains(&expected),
            "case {sql:?}: expected a refusal carrying {expected:?}, got {refused:?}"
        );
        let unchanged = declared(&read_back().await);
        assert_eq!(
            unchanged, replaced,
            "case {sql:?}: a refused change leaves the column list as it was, got \
             {unchanged:?}"
        );
    }
}

/// A declaration the stored leaf cannot be read as fails the query rather than
/// reinterpreting the bytes or truncating the value, and the failure names the column, what
/// the FILE stores it as and the object it is in.
///
/// The rows differ along the family of the declared type against the family of the stored
/// leaf, the direction of the mismatch, and a narrowing the stored value does and does not
/// fit.
#[tokio::test]
async fn a_declaration_the_stored_leaf_cannot_be_read_as_fails_the_query() {
    let case = "a declaration the stored leaf cannot be read as";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(
        &router,
        case,
        "spills",
        &[("id", "int"), ("name", "string"), ("size", "bigint")],
        &[],
    )
    .await;
    put_object(
        &router,
        case,
        "spills/part-0.parquet",
        parquet_object(STORED_LEAVES, &stored_record()),
    )
    .await;
    let split = format!("s3://{BUCKET}/spills/part-0.parquet");

    // (case, the declaration to replace the list with, the text the failure must carry, or
    //  none where the read still answers)
    let cases: Vec<(&str, &str, Option<String>)> = vec![
        (
            "text declared as a number, which is another type family",
            "ALTER TABLE spills REPLACE COLUMNS (id int, name int, size bigint)",
            Some(format!(
                "HIVE_BAD_DATA: Field name's type BINARY in parquet file {split} is \
                 incompatible with type integer defined in table schema"
            )),
        ),
        (
            "a number declared as text, the same mismatch the other way",
            "ALTER TABLE spills REPLACE COLUMNS (id string, name string, size bigint)",
            Some(format!(
                "HIVE_BAD_DATA: Field id's type INT32 in parquet file {split} is \
                 incompatible with type varchar defined in table schema"
            )),
        ),
        (
            "a stored value the narrowed type has no room for",
            "ALTER TABLE spills REPLACE COLUMNS (id int, name string, size int)",
            Some(format!(
                "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split {split} (offset=0, length="
            )),
        ),
        (
            "a narrowing the stored value still fits",
            "ALTER TABLE spills REPLACE COLUMNS (id tinyint, name string, size bigint)",
            None,
        ),
    ];

    for (case, change, expected) in cases {
        performed(&router, case, change).await;
        match expected {
            Some(expected) => {
                let reason = failure_of(&router, case, "SELECT id, name, size FROM spills").await;
                assert!(
                    reason.starts_with(&expected),
                    "case {case:?}: expected a failure opening with {expected:?}, got {reason:?}"
                );
                // Each one names the object the values it could not read are in.
                assert!(
                    reason.contains(&split),
                    "case {case:?}: expected the failure to name {split:?}, got {reason:?}"
                );
            }
            None => {
                let rows = rows_of(&router, case, "SELECT id FROM spills").await;
                assert_eq!(
                    rows,
                    vec![vec![some("7")]],
                    "case {case:?}: expected the stored value to read, got {rows:?}"
                );
            }
        }
    }
}

/// A table whose properties declare INDEX ACCESS resolves a Parquet column by its position in
/// the file rather than by its name, so a column list reordered against the file reads the
/// leaf at that place — and fails where the leaf at that place is of another type family,
/// while the same reordering resolves by name on a table that declares no such property.
#[tokio::test]
async fn index_access_resolves_a_parquet_column_by_its_position() {
    let case = "index access resolves a column by position";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    for table in ["by_name", "by_index"] {
        given_parquet_table(
            &router,
            case,
            table,
            &[("id", "int"), ("name", "string"), ("size", "bigint")],
            &[],
        )
        .await;
        put_object(
            &router,
            case,
            &format!("{table}/part-0.parquet"),
            parquet_object(STORED_LEAVES, &stored_record()),
        )
        .await;
        performed(
            &router,
            case,
            &format!("ALTER TABLE {table} REPLACE COLUMNS (name string, id int, size bigint)"),
        )
        .await;
    }
    performed(
        &router,
        case,
        "ALTER TABLE by_index SET TBLPROPERTIES ('parquet.column.index.access'='true')",
    )
    .await;

    // By NAME the reordered list still reads each column's own leaf.
    let rows = rows_of(&router, "the reordered list resolved by name", "SELECT id, name FROM by_name")
        .await;
    assert_eq!(
        rows,
        vec![vec![some("7"), some("desk")]],
        "case {case:?}: expected the values the file holds under each name, got {rows:?}"
    );

    // By INDEX the first declared column reads the file's first leaf, which holds a number
    // where the declaration now says text — and the next one the other way round.
    let reason = failure_of(
        &router,
        "the reordered list resolved by position",
        "SELECT id, name FROM by_index",
    )
    .await;
    assert!(
        reason.contains("is incompatible with type"),
        "case {case:?}: expected the incompatible-type failure, got {reason:?}"
    );
}

/// `MSCK REPAIR TABLE` registers every Hive-style partition directory under the table's
/// location that the catalog has no record of: `SHOW PARTITIONS` then reports them, a read
/// answers the rows they hold, and a read narrowed to one partition answers only its rows.
#[tokio::test]
async fn msck_repair_registers_the_partitions_under_the_location() {
    let case = "the partitions under the location registered";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(
        &router,
        case,
        "arrivals",
        &[("id", "int"), ("name", "string"), ("size", "bigint")],
        &[("day", "string")],
    )
    .await;
    // One object per partition directory, laid out the way Hive lays a partitioned table
    // out, plus a nested directory that names no value for the key at all.
    for (day, id) in [("2024-01-01", 1), ("2024-01-02", 2)] {
        put_object(
            &router,
            case,
            &format!("arrivals/day={day}/part-0.parquet"),
            parquet_object(
                STORED_LEAVES,
                &[vec![
                    Cell::Int(id),
                    Cell::Text(format!("arrival-{id}")),
                    Cell::Long(1),
                ]],
            ),
        )
        .await;
    }

    // Before the repair the catalog holds no partition of the table at all.
    let before = ddl_rows_of(&router, "no partition registered yet", "SHOW PARTITIONS arrivals").await;
    assert!(
        before.is_empty(),
        "case {case:?}: expected no partition before the repair, got {before:?}"
    );

    performed(&router, case, "MSCK REPAIR TABLE arrivals").await;

    let reported = ddl_rows_of(&router, "the partitions reported", "SHOW PARTITIONS arrivals").await;
    let mut specs: Vec<Option<String>> = reported.into_iter().flatten().collect();
    specs.sort();
    assert_eq!(
        specs,
        vec![some("day=2024-01-01"), some("day=2024-01-02")],
        "case {case:?}: expected one spec per discovered partition, got {specs:?}"
    );

    // The partitions the repair registered are the ones the catalog's own reads report.
    let partitions = glue_ok(
        &router,
        case,
        "GetPartitions",
        &json!({ "DatabaseName": DATABASE, "TableName": "arrivals" }),
    )
    .await;
    let held = partitions["Partitions"].as_array().map(Vec::len);
    assert_eq!(
        held,
        Some(2),
        "case {case:?}: expected the catalog to hold both partitions, got {partitions}"
    );

    // And a read now answers the rows under them, one partition at a time.
    let narrowed = rows_of(
        &router,
        "a read narrowed to one partition",
        "SELECT id, day FROM arrivals WHERE day = '2024-01-02'",
    )
    .await;
    assert_eq!(
        narrowed,
        vec![vec![some("2"), some("2024-01-02")]],
        "case {case:?}: expected only the selected partition's row, got {narrowed:?}"
    );

    // A repeat repair registers nothing new: the records are already there.
    performed(&router, "a repeated repair", "MSCK REPAIR TABLE arrivals").await;
    let again = ddl_rows_of(&router, "the partitions after a repeat", "SHOW PARTITIONS arrivals").await;
    assert_eq!(
        again.len(),
        2,
        "case {case:?}: expected the same two partitions after a repeat, got {again:?}"
    );
}

/// A statement over a table the catalog does not hold is the not-found refusal for it,
/// whichever of these statements it is — the table has to be there for its definition to
/// change or its partitions to be registered.
#[tokio::test]
async fn a_change_to_a_table_the_catalog_does_not_hold_is_refused() {
    let case = "a change to a table the catalog does not hold";
    let router = emulator();
    given_bucket_and_database(&router, case).await;

    for sql in [
        "ALTER TABLE absent ADD COLUMNS (label string)",
        "ALTER TABLE absent REPLACE COLUMNS (label string)",
        "ALTER TABLE absent SET TBLPROPERTIES ('classification'='parquet')",
        "MSCK REPAIR TABLE absent",
    ] {
        let reason = failure_of(&router, sql, sql).await;
        assert!(
            reason.contains("TABLE_NOT_FOUND") && reason.contains("absent"),
            "case {sql:?}: expected the not-found refusal for the table, got {reason:?}"
        );
    }
}
