// SPDX-License-Identifier: CC0-1.0

//! Reading a Data Catalog table whose objects are COLUMNAR Parquet files, driven end to end
//! over the wire through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: Athena answers a query over a Data Catalog table from the OBJECTS at
//! the table's `StorageDescriptor$Location`, decoded by the storage format the catalog
//! records for it — so a table whose descriptor names the Parquet format (its SerDe, its
//! input format, or both) is read by decoding the Parquet files there, the execution reaches
//! `QueryExecutionState` SUCCEEDED, and the result set carries one row per record those files
//! hold with every column's value taken from the record BY NAME and held as the Hive type
//! the column declares. An object at the location that is not a Parquet file at all cannot
//! be opened as a split, which is a failure of the caller's own data rather than a table
//! that is not there.
//!
//! Every object here is a REAL Parquet file, written by the Apache implementation of the
//! format through its own row-group and column writers — the same way the services that
//! deliver such objects into a bucket write them — so what the read decodes is a file any
//! Parquet reader reads and never bytes assembled to look like one.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: the
//! Hive type of the column and the kind of leaf its values are written as, the value itself
//! (text, empty text, a negative number, a number no double holds exactly, either boolean, an
//! instant, a date, binary data) and a record that carries no value for a column at all. The
//! second spans what the OBJECTS differ along: the compression of the file, how many of them
//! the location holds, the prefix each lives under, the case the file spells a column's name
//! in, a file carrying a column the table does not declare, and a file missing one the table
//! does declare.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parquet::basic::{Compression, ConvertedType, GzipLevel, Repetition, Type as PhysicalType};
use parquet::data_type::{
    BoolType, ByteArray, ByteArrayType, DoubleType, Int32Type, Int64Type,
};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type as SchemaType;
use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::query_engine::MAX_MATERIALISED_ROWS;
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its objects in.
const BUCKET: &str = "athena-parquet-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-parquet-data/results/";

/// The database every table here lives in.
const DATABASE: &str = "parquet_lake";

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

/// Write one object into [`BUCKET`], whose bytes are what the table's location then holds.
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

/// Register `table` over its own prefix of [`BUCKET`], declaring `columns` and the Parquet
/// storage format — the SerDe, the input format and the output format a catalog entry for
/// such a table carries.
async fn given_parquet_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    columns: &[Column],
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
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": columns
                        .iter()
                        .map(|column| json!({ "Name": column.name, "Type": column.hive }))
                        .collect::<Vec<Value>>(),
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

/// Run `sql` to completion and answer its terminal `Status`.
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

/// Run `sql` to completion, require SUCCEEDED, and answer its rows: one entry per data row,
/// each cell `None` where the datum carries no value (SQL NULL) and `Some(text)` otherwise.
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Vec<Vec<Option<String>>> {
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

/// One column of a test table: the name both the catalog entry and the file write it under,
/// the Hive type the catalog declares, and the Parquet leaf the file writes its values as.
#[derive(Debug, Clone, Copy)]
struct Column {
    /// The column name.
    name: &'static str,
    /// The Hive type the catalog records for it.
    hive: &'static str,
    /// The leaf its values are written as.
    leaf: Leaf,
}

/// The kind of Parquet leaf one column's values are written as, which is the leaf the Hive
/// type beside it is written as by a writer of such a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    /// A byte array annotated as UTF-8 text.
    Utf8,
    /// A byte array carrying bytes that are not text.
    Binary,
    /// A 32-bit integer.
    Int32,
    /// A 64-bit integer.
    Int64,
    /// A double-precision number.
    Double,
    /// A boolean.
    Boolean,
    /// A 32-bit count of whole days from the epoch.
    Date,
    /// A 64-bit count of milliseconds from the epoch.
    TimestampMillis,
}

impl Leaf {
    /// The physical type this leaf is written as, and the annotation that says how to read
    /// it.
    fn physical(self) -> (PhysicalType, ConvertedType) {
        match self {
            Self::Utf8 => (PhysicalType::BYTE_ARRAY, ConvertedType::UTF8),
            Self::Binary => (PhysicalType::BYTE_ARRAY, ConvertedType::NONE),
            Self::Int32 => (PhysicalType::INT32, ConvertedType::NONE),
            Self::Int64 => (PhysicalType::INT64, ConvertedType::NONE),
            Self::Double => (PhysicalType::DOUBLE, ConvertedType::NONE),
            Self::Boolean => (PhysicalType::BOOLEAN, ConvertedType::NONE),
            Self::Date => (PhysicalType::INT32, ConvertedType::DATE),
            Self::TimestampMillis => (PhysicalType::INT64, ConvertedType::TIMESTAMP_MILLIS),
        }
    }
}

/// One value a record carries for a column, or none at all.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    /// Text, for a UTF-8 leaf.
    Text(&'static str),
    /// Bytes, for a binary leaf.
    Bytes(Vec<u8>),
    /// A 32-bit number, for an integer or a date leaf.
    Int(i32),
    /// A 64-bit number, for a long or a timestamp leaf.
    Long(i64),
    /// A double.
    Double(f64),
    /// A boolean.
    Boolean(bool),
    /// No value: the record holds none for this column, which an OPTIONAL leaf records as a
    /// null.
    Absent,
}

/// One Parquet object holding `records` as `columns`, written by the format's own writer
/// under `compression`.
fn parquet_object(columns: &[Column], records: &[Vec<Cell>], compression: Compression) -> Vec<u8> {
    let fields: Vec<Arc<SchemaType>> = columns
        .iter()
        .map(|column| {
            let (physical, converted) = column.leaf.physical();
            Arc::new(
                SchemaType::primitive_type_builder(column.name, physical)
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
        .set_compression(compression)
        .build();
    let mut writer =
        SerializedFileWriter::new(Vec::new(), Arc::new(schema), Arc::new(properties))
            .expect("a writer over a buffer");
    {
        let mut group = writer.next_row_group().expect("a row group");
        for (index, column) in columns.iter().enumerate() {
            let mut leaf = group
                .next_column()
                .expect("the next column")
                .expect("one column per leaf");
            let cells: Vec<Cell> = records
                .iter()
                .map(|record| record.get(index).cloned().unwrap_or(Cell::Absent))
                .collect();
            // An OPTIONAL leaf carries a definition level per record and a value only for
            // the records that hold one, which is how the format records a null.
            let definitions: Vec<i16> = cells
                .iter()
                .map(|cell| i16::from(*cell != Cell::Absent))
                .collect();
            let definitions = Some(definitions.as_slice());
            match column.leaf {
                Leaf::Utf8 | Leaf::Binary => {
                    let values: Vec<ByteArray> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Text(text) => Some(ByteArray::from(text.as_bytes().to_vec())),
                            Cell::Bytes(data) => Some(ByteArray::from(data.clone())),
                            _ => None,
                        })
                        .collect();
                    leaf.typed::<ByteArrayType>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Int32 | Leaf::Date => {
                    let values: Vec<i32> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Int(value) => Some(*value),
                            _ => None,
                        })
                        .collect();
                    leaf.typed::<Int32Type>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Int64 | Leaf::TimestampMillis => {
                    let values: Vec<i64> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Long(value) => Some(*value),
                            _ => None,
                        })
                        .collect();
                    leaf.typed::<Int64Type>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Double => {
                    let values: Vec<f64> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Double(value) => Some(*value),
                            _ => None,
                        })
                        .collect();
                    leaf.typed::<DoubleType>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
                Leaf::Boolean => {
                    let values: Vec<bool> = cells
                        .iter()
                        .filter_map(|cell| match cell {
                            Cell::Boolean(flag) => Some(*flag),
                            _ => None,
                        })
                        .collect();
                    leaf.typed::<BoolType>()
                        .write_batch(&values, definitions, None)
                        .expect("the leaf takes its values");
                }
            }
            leaf.close().expect("the column closes");
        }
        group.close().expect("the row group closes");
    }
    writer.into_inner().expect("the file's footer")
}

/// The one cell `text` names, for an expectation written as text.
fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

/// A table of every Hive scalar type reads back the value each record carries, and a NULL
/// wherever a record carried none.
///
/// The rows of the result set are the rows the FILE holds, so the assertion is on the whole
/// result set: one row per record, in the order the statement asked for.
#[tokio::test]
async fn a_parquet_table_reads_back_one_row_per_record() {
    let case = "every scalar type, over three records";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    // The leaf beside each Hive type is the one a writer of such a table writes that type
    // as: a date as a day count, a timestamp as a millisecond count, binary data as bytes.
    let columns = [
        Column { name: "marker", hive: "string", leaf: Leaf::Utf8 },
        Column { name: "n", hive: "int", leaf: Leaf::Int32 },
        Column { name: "big", hive: "bigint", leaf: Leaf::Int64 },
        Column { name: "ratio", hive: "double", leaf: Leaf::Double },
        Column { name: "ok", hive: "boolean", leaf: Leaf::Boolean },
        Column { name: "at", hive: "timestamp", leaf: Leaf::TimestampMillis },
        Column { name: "day", hive: "date", leaf: Leaf::Date },
        Column { name: "payload", hive: "binary", leaf: Leaf::Binary },
    ];
    let records = vec![
        vec![
            Cell::Text("alpha"),
            Cell::Int(1),
            // A whole number past what a double holds exactly, which a 64-bit column keeps.
            Cell::Long(9_007_199_254_740_993),
            Cell::Double(1.5),
            Cell::Boolean(true),
            Cell::Long(1_704_164_645_123),
            Cell::Int(19_724),
            Cell::Bytes(vec![0, 159, 2]),
        ],
        vec![
            Cell::Text(""),
            Cell::Int(-2),
            Cell::Long(0),
            Cell::Double(2.0),
            Cell::Boolean(false),
            Cell::Long(0),
            Cell::Int(0),
            Cell::Bytes(Vec::new()),
        ],
        // A record carrying nothing but the one column it is ordered by.
        vec![
            Cell::Absent,
            Cell::Int(3),
            Cell::Absent,
            Cell::Absent,
            Cell::Absent,
            Cell::Absent,
            Cell::Absent,
            Cell::Absent,
        ],
    ];
    given_parquet_table(&router, case, "scalars", &columns).await;
    put_object(
        &router,
        case,
        "scalars/part-0.parquet",
        parquet_object(&columns, &records, Compression::SNAPPY),
    )
    .await;

    let rows = rows_of(
        &router,
        case,
        &format!(
            "SELECT marker, n, big, ratio, ok, at, day, payload \
             FROM \"{DATABASE}\".\"scalars\" ORDER BY n"
        ),
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![
            some(""),
            some("-2"),
            some("0"),
            some("2.0"),
            some("false"),
            some("1970-01-01 00:00:00.000"),
            some("1970-01-01"),
            some(""),
        ],
        vec![
            some("alpha"),
            some("1"),
            some("9007199254740993"),
            some("1.5"),
            some("true"),
            some("2024-01-02 03:04:05.123"),
            some("2024-01-02"),
            // Binary data is reported as the hex of the BYTES the object holds.
            some("009f02"),
        ],
        vec![
            None,
            some("3"),
            None,
            None,
            None,
            None,
            None,
            None,
        ],
    ];
    assert_eq!(
        rows, expected,
        "case {case:?}: expected the rows {expected:?}, got {rows:?}"
    );
}

/// One object of a table's location, as the suite below writes it.
struct ObjectCase {
    /// What this object varies, for the assertion's own words.
    case: &'static str,
    /// Its key under the table's prefix.
    key: &'static str,
    /// The columns the FILE carries, which need be neither all nor only the table's.
    columns: &'static [Column],
    /// The records it holds.
    records: Vec<Vec<Cell>>,
    /// How its data blocks are compressed.
    compression: Compression,
}

/// Every Parquet object at a table's location contributes its records, whatever the file's
/// own compression, prefix, column spelling or column set.
#[tokio::test]
async fn every_parquet_object_at_the_location_contributes_its_records() {
    /// The columns the table declares.
    const DECLARED: &[Column] = &[
        Column { name: "marker", hive: "string", leaf: Leaf::Utf8 },
        Column { name: "n", hive: "int", leaf: Leaf::Int32 },
    ];
    /// The same two columns, spelled in another case by the file that carries them.
    const SPELLED_UPPER: &[Column] = &[
        Column { name: "MARKER", hive: "string", leaf: Leaf::Utf8 },
        Column { name: "N", hive: "int", leaf: Leaf::Int32 },
    ];
    /// A file carrying a column the table does not declare, beside the ones it does.
    const WITH_EXTRA: &[Column] = &[
        Column { name: "marker", hive: "string", leaf: Leaf::Utf8 },
        Column { name: "n", hive: "int", leaf: Leaf::Int32 },
        Column { name: "unknown", hive: "string", leaf: Leaf::Utf8 },
    ];
    /// A file missing a column the table DOES declare.
    const WITHOUT_MARKER: &[Column] =
        &[Column { name: "n", hive: "int", leaf: Leaf::Int32 }];

    let case = "several objects under one location";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(&router, case, "deliveries", DECLARED).await;
    let objects = vec![
        ObjectCase {
            case: "the default compression of such a writer, two records in one file",
            key: "deliveries/snappy.parquet",
            columns: DECLARED,
            records: vec![
                vec![Cell::Text("alpha"), Cell::Int(1)],
                vec![Cell::Text("bravo"), Cell::Int(2)],
            ],
            compression: Compression::SNAPPY,
        },
        ObjectCase {
            case: "a deflate-compressed file, under a nested prefix",
            key: "deliveries/2026/01/02/gzip.parquet",
            columns: DECLARED,
            records: vec![vec![Cell::Text("charlie"), Cell::Int(3)]],
            compression: Compression::GZIP(GzipLevel::default()),
        },
        ObjectCase {
            case: "an uncompressed file",
            key: "deliveries/plain.parquet",
            columns: DECLARED,
            records: vec![vec![Cell::Text("delta"), Cell::Int(4)]],
            compression: Compression::UNCOMPRESSED,
        },
        ObjectCase {
            case: "a file spelling the column names in another case",
            key: "deliveries/upper.parquet",
            columns: SPELLED_UPPER,
            records: vec![vec![Cell::Text("echo"), Cell::Int(5)]],
            compression: Compression::SNAPPY,
        },
        ObjectCase {
            case: "a file carrying a column the table does not declare",
            key: "deliveries/extra.parquet",
            columns: WITH_EXTRA,
            records: vec![vec![
                Cell::Text("foxtrot"),
                Cell::Int(6),
                Cell::Text("ignored"),
            ]],
            compression: Compression::SNAPPY,
        },
        ObjectCase {
            case: "a file missing a column the table declares",
            key: "deliveries/partial.parquet",
            columns: WITHOUT_MARKER,
            records: vec![vec![Cell::Int(7)]],
            compression: Compression::SNAPPY,
        },
    ];
    for object in &objects {
        put_object(
            &router,
            object.case,
            object.key,
            parquet_object(object.columns, &object.records, object.compression),
        )
        .await;
    }

    let rows = rows_of(
        &router,
        case,
        &format!("SELECT marker, n FROM \"{DATABASE}\".\"deliveries\" ORDER BY n"),
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![some("alpha"), some("1")],
        vec![some("bravo"), some("2")],
        vec![some("charlie"), some("3")],
        vec![some("delta"), some("4")],
        vec![some("echo"), some("5")],
        vec![some("foxtrot"), some("6")],
        // The file that carries no `marker` leaf holds no value for that column.
        vec![None, some("7")],
    ];
    assert_eq!(
        rows, expected,
        "case {case:?}: expected the rows {expected:?}, got {rows:?}"
    );

    // The rows are real rows the engine computes over, not a listing of the objects: a
    // predicate and an aggregate over them answer from the values the files hold.
    let computed = rows_of(
        &router,
        case,
        &format!(
            "SELECT count(*), sum(n) FROM \"{DATABASE}\".\"deliveries\" WHERE n > 4"
        ),
    )
    .await;
    assert_eq!(
        computed,
        vec![vec![some("3"), some("18")]],
        "case {case:?}: the aggregate must be computed over the delivered rows, got \
         {computed:?}"
    );
}

/// An object declaring MORE records than one query materialises is refused before a page of
/// it is read: what a columnar object decodes to is declared in its own metadata, and a
/// query holds a bounded number of rows however many the objects at a location carry.
#[tokio::test]
async fn an_object_declaring_more_records_than_a_query_holds_is_refused() {
    const DECLARED: &[Column] = &[Column { name: "n", hive: "int", leaf: Leaf::Int32 }];

    let case = "one record more than a query materialises";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(&router, case, "oversized", DECLARED).await;
    // One record past the bound itself, read from the bound rather than written out here.
    let records: Vec<Vec<Cell>> = (0..=MAX_MATERIALISED_ROWS)
        .map(|row| vec![Cell::Int(i32::try_from(row).unwrap_or(i32::MAX))])
        .collect();
    put_object(
        &router,
        case,
        "oversized/part-0.parquet",
        parquet_object(DECLARED, &records, Compression::SNAPPY),
    )
    .await;

    let settled = settled(
        &router,
        case,
        &format!("SELECT n FROM \"{DATABASE}\".\"oversized\""),
    )
    .await;
    let status = &settled["Status"];
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: the execution must fail, got {status}"
    );
    let reason = status["StateChangeReason"].as_str().unwrap_or_default();
    let expected = format!(
        "reading {DATABASE}.oversized would materialise more than the \
         {MAX_MATERIALISED_ROWS} rows this emulator holds for one query"
    );
    assert_eq!(
        reason, expected,
        "case {case:?}: expected the reason {expected:?}, got {reason:?}"
    );
}

/// An object at the location that is not a Parquet file at all cannot be opened as a split:
/// the execution FAILS reporting that class, with the caller's own data named as the cause,
/// rather than reporting a table that is not there.
#[tokio::test]
async fn an_object_that_is_not_a_parquet_file_fails_the_split_it_belongs_to() {
    const DECLARED: &[Column] = &[
        Column { name: "marker", hive: "string", leaf: Leaf::Utf8 },
        Column { name: "n", hive: "int", leaf: Leaf::Int32 },
    ];

    let case = "an object whose bytes are not a Parquet file";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_parquet_table(&router, case, "corrupt", DECLARED).await;
    put_object(
        &router,
        case,
        "corrupt/notes.parquet",
        b"this is not a Parquet file".to_vec(),
    )
    .await;

    let settled = settled(
        &router,
        case,
        &format!("SELECT marker, n FROM \"{DATABASE}\".\"corrupt\""),
    )
    .await;
    let status = &settled["Status"];
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: the execution must fail, got {status}"
    );
    let reason = status["StateChangeReason"].as_str().unwrap_or_default();
    let opening = format!(
        "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split s3://{BUCKET}/corrupt/notes.parquet:"
    );
    assert!(
        reason.starts_with(&opening),
        "case {case:?}: the reason must open with {opening:?}, got {reason:?}"
    );
    assert_eq!(
        status["AthenaError"]["ErrorMessage"].as_str(),
        Some(reason),
        "case {case:?}: the error message must be the reason reported beside it, got {status}"
    );
    assert_eq!(
        status["AthenaError"]["ErrorCategory"].as_i64(),
        Some(2),
        "case {case:?}: an unreadable object is the caller's own data, got {status}"
    );
}
