// SPDX-License-Identifier: CC0-1.0

//! Reading a Data Catalog table whose objects are COMPRESSED text, driven end to end over
//! the same router the emulator boots.
//!
//! THE RULE UNDER TEST: an object at a text table's `StorageDescriptor$Location` is handed to
//! the decompressor its own KEY names before any SerDe reads it — the codec factory holds one
//! codec per file extension, so `*.gz` is a gzip stream, `*.bz2` a bzip2 one, and so on — and
//! the table's delimiter, quoting and `skip.header.line.count` then apply to what that
//! decompressor produced, never to the stored bytes. An object whose key names no codec holds
//! its records as it stores them, and one whose key names a codec its bytes are NOT is a split
//! that cannot be opened, which is a failure of the caller's own data.
//!
//! Every compressed object here is written by the maintained compressor for that codec — the
//! same streams the services that deliver such objects into a bucket write — so what the read
//! decompresses is a stream any reader of that format reads, never bytes assembled to look
//! like one.
//!
//! The first suite is a TABLE whose rows differ along what the rule generalises over: the
//! codec the object's extension names, including an extension naming none at all. The rest
//! span one location holding objects of SEVERAL codecs at once, a compressed object read by a
//! SerDe other than the delimited one, the HIDDEN metadata columns that name the object a row
//! was read from (which is how a query counts the records each object at a location holds),
//! and an object that is not the stream its key names.

use std::sync::Arc;
use std::time::{Duration, Instant};

use std::io::Write;

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table keeps its objects in.
const BUCKET: &str = "athena-compressed-text";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-compressed-text/results/";

/// The database every table here lives in.
const DATABASE: &str = "compressed_lake";

/// The serialization library of the delimited-text SerDe a Hive text table carries.
const LAZY_SIMPLE_SERDE: &str = "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe";

/// The serialization library of a SerDe that reads one JSON document per line.
const JSON_SERDE: &str = "org.openx.data.jsonserde.JsonSerDe";

/// The input format of a table whose objects are text, whatever they are compressed with.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";

/// The output format such a table carries beside it.
const TEXT_OUTPUT_FORMAT: &str = "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat";

/// How long a poll waits for a started query to settle. Generous: what is asserted is that
/// the execution reaches a terminal state, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The two lines a standard web-log object opens with, which such a table declares as header
/// lines rather than data.
const HEADER_LINES: &str = "#Version: 1.0\n#Fields: id item\n";

/// A compression one object can be stored under, named by the extension its key carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compressed {
    /// A gzip stream, keyed `.gz`.
    Gzip,
    /// The zlib stream Hadoop's default codec writes, keyed `.deflate`.
    Deflate,
    /// A bzip2 stream, keyed `.bz2`.
    Bzip2,
    /// A Zstandard stream, keyed `.zst`.
    Zstandard,
    /// Blocks of raw Snappy framed the way Hadoop's block codecs frame them, keyed
    /// `.snappy`.
    Snappy,
    /// Blocks of raw LZ4 framed the same way, keyed `.lz4`.
    Lz4,
    /// No compression at all: the object stores its text as it stands.
    None,
}

impl Compressed {
    /// The extension an object compressed this way is keyed under.
    fn extension(self) -> &'static str {
        match self {
            Self::Gzip => ".gz",
            Self::Deflate => ".deflate",
            Self::Bzip2 => ".bz2",
            Self::Zstandard => ".zst",
            Self::Snappy => ".snappy",
            Self::Lz4 => ".lz4",
            // An extension no codec is keyed under, which is how an uncompressed text
            // object of such a table is named.
            Self::None => ".tsv",
        }
    }

    /// `text` as an object stored under this compression holds it, written by the maintained
    /// compressor for the codec.
    ///
    /// The two block-framed codecs have no stream writer here, so their framing is written
    /// out as those codecs write it: the number of bytes the block decodes to, then one
    /// chunk — the length of its compressed bytes followed by the bytes the codec's own
    /// compressor produced.
    fn object(self, text: &str) -> Vec<u8> {
        let body = text.as_bytes();
        match self {
            Self::Gzip => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body).expect("the encoder takes the text");
                encoder.finish().expect("the gzip stream")
            }
            Self::Deflate => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body).expect("the encoder takes the text");
                encoder.finish().expect("the zlib stream")
            }
            Self::Bzip2 => {
                let mut encoder =
                    bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                encoder.write_all(body).expect("the encoder takes the text");
                encoder.finish().expect("the bzip2 stream")
            }
            Self::Zstandard => ruzstd::encoding::compress_to_vec(
                body,
                ruzstd::encoding::CompressionLevel::Fastest,
            ),
            Self::Snappy | Self::Lz4 => {
                let chunk = if self == Self::Lz4 {
                    lz4_flex::block::compress(body)
                } else {
                    snap::raw::Encoder::new()
                        .compress_vec(body)
                        .expect("the Snappy chunk")
                };
                let mut framed = Vec::new();
                framed.extend_from_slice(
                    &u32::try_from(body.len()).expect("the block length").to_be_bytes(),
                );
                framed.extend_from_slice(
                    &u32::try_from(chunk.len()).expect("the chunk length").to_be_bytes(),
                );
                framed.extend_from_slice(&chunk);
                framed
            }
            Self::None => body.to_vec(),
        }
    }
}

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

/// Register `table` over its own prefix of [`BUCKET`]: a Hive TEXT table read by `library`,
/// whose SerDe parameters are `serde_parameters` and whose own properties are
/// `table_parameters`.
async fn given_text_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    library: &str,
    serde_parameters: Value,
    table_parameters: Value,
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
                "Parameters": table_parameters,
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": [
                        { "Name": "id", "Type": "int" },
                        { "Name": "item", "Type": "string" },
                    ],
                    "InputFormat": TEXT_INPUT_FORMAT,
                    "OutputFormat": TEXT_OUTPUT_FORMAT,
                    "SerdeInfo": {
                        "SerializationLibrary": library,
                        "Parameters": serde_parameters,
                    },
                },
            },
        }),
    )
    .await;
}

/// Register a tab-delimited table whose first two lines are header lines, which is the
/// layout a standard web-log table carries.
async fn given_tab_delimited_table(router: &MultiServiceRouter, case: &str, table: &str) {
    given_text_table(
        router,
        case,
        table,
        LAZY_SIMPLE_SERDE,
        json!({ "field.delim": "\t" }),
        json!({ "EXTERNAL": "TRUE", "skip.header.line.count": "2" }),
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

/// The one cell `text` names, for an expectation written as text.
fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

/// Every codec an object's extension can name reads back the RECORDS the object holds: the
/// rows differ along the codec, and the last one is an extension naming no codec at all.
///
/// The object of each case holds the same text — two header lines the table declares as such
/// and three tab-delimited records — so what the assertion isolates is the compression the
/// key names: the execution SUCCEEDS and the result set is the records, with the header lines
/// left out, whatever the object was compressed with.
#[tokio::test]
async fn a_compressed_text_object_reads_back_the_records_it_holds() {
    let codecs = [
        Compressed::Gzip,
        Compressed::Deflate,
        Compressed::Bzip2,
        Compressed::Zstandard,
        Compressed::Snappy,
        Compressed::Lz4,
        Compressed::None,
    ];
    let router = emulator();
    given_bucket_and_database(&router, "the compressed-text lake").await;
    let records = "1\tdesk\n2\tlamp\n3\tchair\n";
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![some("1"), some("desk")],
        vec![some("2"), some("lamp")],
        vec![some("3"), some("chair")],
    ];

    for codec in codecs {
        let case = format!("an object keyed {}", codec.extension());
        let table = format!("logs{}", codec.extension().replace('.', "_"));
        given_tab_delimited_table(&router, &case, &table).await;
        put_object(
            &router,
            &case,
            &format!("{table}/part-0{}", codec.extension()),
            codec.object(&format!("{HEADER_LINES}{records}")),
        )
        .await;

        let actual = rows_of(
            &router,
            &case,
            &format!("SELECT id, item FROM \"{DATABASE}\".\"{table}\" ORDER BY id"),
        )
        .await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the records {expected:?}, got {actual:?}"
        );
    }
}

/// ONE location holding objects of SEVERAL codecs reads all of them: the codec is chosen per
/// OBJECT, from that object's own key, so a prefix a writer appended to under changing
/// compression still reads as the records all of its objects hold.
#[tokio::test]
async fn a_location_holding_objects_of_several_codecs_reads_every_one() {
    let case = "one prefix, one object per codec";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_tab_delimited_table(&router, case, "mixed").await;
    // (the codec the object's key names, the record it holds)
    let objects = [
        (Compressed::Gzip, "1\tdesk\n"),
        (Compressed::Deflate, "2\tlamp\n"),
        (Compressed::Bzip2, "3\tchair\n"),
        (Compressed::Zstandard, "4\tsofa\n"),
        (Compressed::Snappy, "5\tstool\n"),
        (Compressed::Lz4, "6\tshelf\n"),
        (Compressed::None, "7\tbench\n"),
    ];
    for (codec, record) in objects {
        put_object(
            &router,
            case,
            &format!(
                "mixed/part-{}{}",
                codec.extension().trim_start_matches('.'),
                codec.extension()
            ),
            codec.object(&format!("{HEADER_LINES}{record}")),
        )
        .await;
    }

    let actual = rows_of(
        &router,
        case,
        &format!("SELECT id, item FROM \"{DATABASE}\".\"mixed\" ORDER BY id"),
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![some("1"), some("desk")],
        vec![some("2"), some("lamp")],
        vec![some("3"), some("chair")],
        vec![some("4"), some("sofa")],
        vec![some("5"), some("stool")],
        vec![some("6"), some("shelf")],
        vec![some("7"), some("bench")],
    ];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the records of every object, got {actual:?}"
    );
}

/// The codec is undone before ANY text SerDe reads the object, so a compressed object of a
/// table read by the JSON SerDe reads back the documents it holds — the compression is a
/// property of the object, not of the SerDe above it.
#[tokio::test]
async fn a_compressed_object_of_a_json_table_reads_back_its_documents() {
    let case = "a gzip object of a table read one JSON document per line";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_text_table(
        &router,
        case,
        "documents",
        JSON_SERDE,
        json!({}),
        json!({ "EXTERNAL": "TRUE" }),
    )
    .await;
    put_object(
        &router,
        case,
        "documents/part-0.gz",
        Compressed::Gzip.object("{\"id\": 1, \"item\": \"desk\"}\n{\"id\": 2, \"item\": \"lamp\"}\n"),
    )
    .await;

    let actual = rows_of(
        &router,
        case,
        &format!("SELECT id, item FROM \"{DATABASE}\".\"documents\" ORDER BY id"),
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![some("1"), some("desk")],
        vec![some("2"), some("lamp")],
    ];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the documents the object holds, got {actual:?}"
    );
}

/// Each row carries the HIDDEN metadata columns of the object it was read from: where that
/// object is, how many bytes it stores and when it was last written — whatever the object is
/// compressed with, because they describe the object rather than its records.
///
/// They are hidden: a statement that does not name one does not read it, so `SELECT *` over
/// the same table carries the table's DECLARED columns and nothing else.
#[tokio::test]
async fn the_hidden_metadata_columns_name_the_object_each_row_was_read_from() {
    let case = "two objects under one location, one of them compressed";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_tab_delimited_table(&router, case, "metadata").await;
    // (the key the object is written under, the records it holds)
    let compressed = Compressed::Gzip.object(&format!("{HEADER_LINES}1\tdesk\n2\tlamp\n"));
    let plain = Compressed::None.object(&format!("{HEADER_LINES}3\tchair\n"));
    put_object(&router, case, "metadata/part-a.gz", compressed.clone()).await;
    put_object(&router, case, "metadata/part-b.tsv", plain.clone()).await;

    let actual = rows_of(
        &router,
        case,
        &format!(
            "SELECT id, \"$path\", \"$file_size\", \"$file_modified_time\" \
             FROM \"{DATABASE}\".\"metadata\" ORDER BY id"
        ),
    )
    .await;
    // (the id of the row, the object it was read from, the bytes that object stores)
    let expected = [
        ("1", "metadata/part-a.gz", compressed.len()),
        ("2", "metadata/part-a.gz", compressed.len()),
        ("3", "metadata/part-b.tsv", plain.len()),
    ];
    assert_eq!(
        actual.len(),
        expected.len(),
        "case {case:?}: expected one row per record, got {actual:?}"
    );
    for (row, (id, key, size)) in actual.iter().zip(expected) {
        let read = vec![
            some(id),
            some(&format!("s3://{BUCKET}/{key}")),
            some(&size.to_string()),
        ];
        assert_eq!(
            row[..3].to_vec(),
            read,
            "case {case:?}: expected the row of {id} to name {key}, got {row:?}"
        );
        // The instant is the object's own, so what is asserted is that the column holds one
        // written the way a column of instants is read here — never one run's clock value.
        let modified = row[3].clone().unwrap_or_default();
        assert!(
            chrono::NaiveDateTime::parse_from_str(&modified, "%Y-%m-%d %H:%M:%S%.3f").is_ok(),
            "case {case:?}: expected the row of {id} to carry an instant, got {modified:?}"
        );
    }

    let declared = rows_of(
        &router,
        case,
        &format!("SELECT * FROM \"{DATABASE}\".\"metadata\" ORDER BY id"),
    )
    .await;
    let expected_declared: Vec<Vec<Option<String>>> = vec![
        vec![some("1"), some("desk")],
        vec![some("2"), some("lamp")],
        vec![some("3"), some("chair")],
    ];
    assert_eq!(
        declared, expected_declared,
        "case {case:?}: a hidden column is no part of what `*` expands to, got {declared:?}"
    );
}

/// One group per OBJECT: grouping by the hidden location column counts the records each
/// object at the location holds, which is what a query over a prefix of compressed log
/// objects asks for.
#[tokio::test]
async fn grouping_by_the_object_counts_the_records_each_one_holds() {
    let case = "two compressed objects holding different numbers of records";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_tab_delimited_table(&router, case, "grouped").await;
    put_object(
        &router,
        case,
        "grouped/part-a.gz",
        Compressed::Gzip.object(&format!("{HEADER_LINES}1\tdesk\n2\tlamp\n3\tchair\n")),
    )
    .await;
    put_object(
        &router,
        case,
        "grouped/part-b.gz",
        Compressed::Gzip.object(&format!("{HEADER_LINES}4\tsofa\n")),
    )
    .await;

    let actual = rows_of(
        &router,
        case,
        &format!(
            "SELECT \"$path\", count(*), count(DISTINCT item), min(id) \
             FROM \"{DATABASE}\".\"grouped\" GROUP BY 1 ORDER BY 1"
        ),
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = vec![
        vec![
            some(&format!("s3://{BUCKET}/grouped/part-a.gz")),
            some("3"),
            some("3"),
            some("1"),
        ],
        vec![
            some(&format!("s3://{BUCKET}/grouped/part-b.gz")),
            some("1"),
            some("1"),
            some("4"),
        ],
    ];
    assert_eq!(
        actual, expected,
        "case {case:?}: expected one group per object, got {actual:?}"
    );
}

/// The EXTENSION decides the codec and the bytes never do: an object whose key names a codec
/// its content is not is a split that cannot be opened, reported in that class with the
/// object named — never read as the bytes it stores.
#[tokio::test]
async fn an_object_that_is_not_the_stream_its_key_names_fails_the_split() {
    let case = "an object keyed .gz whose bytes are plain text";
    let router = emulator();
    given_bucket_and_database(&router, case).await;
    given_tab_delimited_table(&router, case, "mislabelled").await;
    put_object(
        &router,
        case,
        "mislabelled/part-0.gz",
        format!("{HEADER_LINES}1\tdesk\n").into_bytes(),
    )
    .await;

    let settled = settled(
        &router,
        case,
        &format!("SELECT id, item FROM \"{DATABASE}\".\"mislabelled\""),
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
        "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split \
         s3://{BUCKET}/mislabelled/part-0.gz:"
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
        "case {case:?}: an object that is not its codec's stream is the caller's own data, \
         got {status}"
    );
}
