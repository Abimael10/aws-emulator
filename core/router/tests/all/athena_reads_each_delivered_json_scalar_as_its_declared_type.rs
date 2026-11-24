// SPDX-License-Identifier: CC0-1.0

//! Reading back through Athena what a Firehose delivery stream wrote: the Data Catalog table
//! registered over the stream's destination prefix is resolved, every object the delivery left
//! under that prefix is scanned, and each JSON member is held as the column's DECLARED type.
//!
//! THE RULE UNDER TEST: Athena resolves `<catalog>.<database>.<table>` through the data catalog
//! and reads a Hive table by scanning every non-hidden object under its
//! `StorageDescriptor$Location`, the objects of its nested sub-prefixes included — which is
//! where a delivery's date-partitioned keys put them — so the execution settles
//! `QueryExecutionState` SUCCEEDED (Smithy: com.amazonaws.athena#QueryExecutionState) with one
//! row per delivered record. Each field of a record is held as a value of its column's declared
//! type: a scalar whose text spells a value of that type converts to it, and one that is not a
//! value of that type is NULL for THAT COLUMN alone, which is what Hive reads a value it cannot
//! hold as rather than failing the read — so a record carrying a non-convertible scalar still
//! answers its row, with its convertible siblings intact. A relation the catalog does not hold
//! is the other side of the same resolution: the execution settles FAILED carrying
//! `AthenaError$ErrorCategory` 2 with `AthenaError$ErrorType` 1301 and the not-found sentence
//! naming the catalog, the database and the table.
//!
//! Four services have to agree for that to hold — Firehose decides when to flush and under which
//! key, Amazon S3 holds the object, Glue holds the table whose location that key lies under and
//! answers whether a relation exists at all, and Athena resolves and scans it, with the delivery
//! made under an IAM role — so the suite lives here rather than in any of their crates, over ONE
//! engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! Nothing here writes into a store to arrange what it asserts: every object read comes from a
//! Firehose delivery of records a client put, and every reading is `StartQueryExecution`,
//! `GetQueryExecution` and `GetQueryResults`.
//!
//! The rows differ along what the rule generalises over: the JSON form of the delivered scalar
//! (a number, whole and fractional, negative and wider than 32 bits; the same value as text; a
//! text that is no number at all; the empty text; a boolean and the text of one; an explicit
//! null; and a member the record omits) read against four declared types at once — a 64-bit
//! integer, a double, a boolean and character data.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_CANCELLED, QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use super::events_rule_firehose_target_delivery::{given_role, send, REGION};
use super::firehose_buffer_flush_carries_mid_interval_arrivals::{body_of, json_call, keys_under};

/// The bucket the stream delivers into and the queries write their results under.
const BUCKET: &str = "athena-delivered-scalar-types";

/// Where the queries write their results: a prefix no table is registered at, so a result
/// object is never one of the objects a scan reads.
const OUTPUT_LOCATION: &str = "s3://athena-delivered-scalar-types/athena-results/";

/// The database the table lives in.
const DATABASE: &str = "scalar_lake";

/// The table registered over the stream's destination prefix.
const TABLE: &str = "ticks";

/// A table of [`DATABASE`] nothing registers, so the catalog holds no relation of that name.
const ABSENT_TABLE: &str = "absent_ticks";

/// The delivery stream, and the prefix of [`BUCKET`] it delivers under.
const STREAM: &str = "scalar-ticks";
const PREFIX: &str = "ticks/";

/// The role the stream delivers under: Firehose has to be able to assume it, and it has to
/// allow the object write a delivery makes.
const DELIVERY_ROLE: &str = "athena-scalar-delivery-role";

/// The action that role allows.
const PUT_OBJECT_ACTION: &str = "s3:PutObject";

/// The serialization library of a SerDe that reads one JSON document per line, which is what a
/// table over newline-delimited delivered records carries.
const JSON_SERDE: &str = "org.openx.data.jsonserde.JsonSerDe";

/// The input format of a table whose objects are text, and the output format beside it.
const TEXT_INPUT_FORMAT: &str = "org.apache.hadoop.mapred.TextInputFormat";
const TEXT_OUTPUT_FORMAT: &str = "org.apache.hadoop.hive.ql.io.HiveIgnoreKeyTextOutputFormat";

/// `BufferingHints$SizeInMBs` for the destination: far above anything put here, so the INTERVAL
/// is what flushes the buffer.
const SIZE_IN_MBS: u64 = 64;

/// `BufferingHints$IntervalInSeconds` for it.
const INTERVAL_SECONDS: u64 = 5;

/// How long a wait for the stream to settle and for every record to be delivered runs before it
/// gives up: many times the buffering interval, so a loaded host never fails the suite where the
/// clock would have served it.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);

/// How long a wait for a started execution to reach a terminal state runs. Generous: what is
/// asserted is that the execution settles, never how fast the host got there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// `AthenaError$ErrorCategory` of a failure the caller's own input is the cause of: the
/// documented category list is "1 - System, 2 - User, 3 - Other".
const ERROR_CATEGORY_USER: i64 = 2;

/// `AthenaError$ErrorType` Athena publishes for a statement naming a relation the data catalog
/// does not hold.
const ERROR_TYPE_ENTITY_NOT_FOUND: i64 = 1301;

/// The columns the delivered scalar is read through, each declaring a different Hive type, and
/// the Athena type name a result set reports for a column of it (`ColumnInfo$Type`).
const TYPED_COLUMNS: &[(&str, &str, &str)] = &[
    ("as_bigint", "bigint", "bigint"),
    ("as_double", "double", "double"),
    ("as_boolean", "boolean", "boolean"),
    ("as_string", "string", "varchar"),
];

/// One row: the scalar one delivered record carries, and what each declared type holds it as.
struct Row {
    /// What this row is about, named in every assertion it makes.
    case: &'static str,
    /// The record's `id`, which is also where its row stands in the ordered read.
    id: i64,
    /// The JSON value every typed member of the record carries, or `None` for a record that
    /// omits those members altogether.
    delivered: Option<Value>,
    /// What the 64-bit integer column holds it as, `None` for SQL NULL.
    bigint: Option<&'static str>,
    /// What the double column holds it as.
    double: Option<&'static str>,
    /// What the boolean column holds it as.
    boolean: Option<&'static str>,
    /// What the character column holds it as.
    string: Option<&'static str>,
}

/// The rows: one delivered record per form a JSON scalar takes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a whole JSON number",
            id: 1,
            delivered: Some(json!(42)),
            bigint: Some("42"),
            double: Some("42.0"),
            boolean: None,
            string: Some("42"),
        },
        Row {
            case: "the same whole number written as text",
            id: 2,
            delivered: Some(json!("42")),
            bigint: Some("42"),
            double: Some("42.0"),
            boolean: None,
            string: Some("42"),
        },
        Row {
            case: "a fractional JSON number",
            id: 3,
            delivered: Some(json!(12.5)),
            bigint: None,
            double: Some("12.5"),
            boolean: None,
            string: Some("12.5"),
        },
        Row {
            case: "a fractional number written as text, with a trailing zero",
            id: 4,
            delivered: Some(json!("12.50")),
            bigint: None,
            double: Some("12.5"),
            boolean: None,
            string: Some("12.50"),
        },
        Row {
            case: "a negative whole number",
            id: 5,
            delivered: Some(json!(-7)),
            bigint: Some("-7"),
            double: Some("-7.0"),
            boolean: None,
            string: Some("-7"),
        },
        Row {
            case: "a whole number wider than 32 bits",
            id: 6,
            delivered: Some(json!(4_294_967_296_i64)),
            bigint: Some("4294967296"),
            double: Some("4294967296.0"),
            boolean: None,
            string: Some("4294967296"),
        },
        Row {
            case: "a text that is no number at all",
            id: 7,
            delivered: Some(json!("n/a")),
            bigint: None,
            double: None,
            boolean: None,
            string: Some("n/a"),
        },
        Row {
            case: "the empty text",
            id: 8,
            delivered: Some(json!("")),
            bigint: None,
            double: None,
            boolean: None,
            string: Some(""),
        },
        Row {
            case: "a JSON boolean",
            id: 9,
            delivered: Some(json!(true)),
            bigint: None,
            double: None,
            boolean: Some("true"),
            string: Some("true"),
        },
        Row {
            case: "a boolean written as text",
            id: 10,
            delivered: Some(json!("false")),
            bigint: None,
            double: None,
            boolean: Some("false"),
            string: Some("false"),
        },
        Row {
            case: "an explicit JSON null",
            id: 11,
            delivered: Some(Value::Null),
            bigint: None,
            double: None,
            boolean: None,
            string: None,
        },
        Row {
            case: "a record omitting the members altogether",
            id: 12,
            delivered: None,
            bigint: None,
            double: None,
            boolean: None,
            string: None,
        },
    ]
}

impl Row {
    /// The newline-terminated JSON document this row's record delivers: its id, and the row's
    /// scalar under every typed member — one record read against all four declared types.
    fn document(&self) -> String {
        let mut document = json!({ "id": self.id });
        if let Some(value) = &self.delivered {
            for (member, _, _) in TYPED_COLUMNS {
                document[*member] = value.clone();
            }
        }
        format!("{document}\n")
    }

    /// The result row this record is owed: its id, then each typed column's value.
    fn expected(&self) -> Vec<Option<String>> {
        vec![
            Some(self.id.to_string()),
            self.bigint.map(ToString::to_string),
            self.double.map(ToString::to_string),
            self.boolean.map(ToString::to_string),
            self.string.map(ToString::to_string),
        ]
    }
}

/// Athena, Glue, Firehose, Amazon S3 and IAM mounted on ONE shared engine, the way the router
/// mounts every service: the objects a delivery writes are the data Athena reads, and the
/// catalog Glue holds is the catalog Athena resolves against.
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
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One Glue call that must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    json_call(router, case, "glue", &format!("AWSGlue.{operation}"), body).await
}

/// One Athena call that must succeed.
async fn athena_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    json_call(
        router,
        case,
        "athena",
        &format!("AmazonAthena.{operation}"),
        body,
    )
    .await
}

/// One Firehose call that must succeed.
async fn firehose_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    json_call(
        router,
        case,
        "firehose",
        &format!("Firehose_20150804.{operation}"),
        body,
    )
    .await
}

/// One Glue call whose answer is read whatever it is: its HTTP status and its document.
async fn glue_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let (status, answered) = send(
        router,
        "glue",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AWSGlue.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&answered).to_string();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Create the bucket the stream delivers into and the queries write their results under.
async fn given_bucket(router: &MultiServiceRouter, case: &str) {
    let (status, body) = send(
        router,
        "s3",
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: creating s3://{BUCKET} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Register the table its delivered objects are read through: one JSON document per line, over
/// the destination prefix the stream writes under, with one column per declared type the rule is
/// read against.
async fn given_table(router: &MultiServiceRouter, case: &str) {
    let mut columns = vec![json!({ "Name": "id", "Type": "int" })];
    for (member, hive_type, _) in TYPED_COLUMNS {
        columns.push(json!({ "Name": member, "Type": hive_type }));
    }
    glue_ok(
        router,
        case,
        "CreateTable",
        json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": TABLE,
                "TableType": "EXTERNAL_TABLE",
                "Parameters": { "classification": "json", "EXTERNAL": "TRUE" },
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{PREFIX}"),
                    "Columns": columns,
                    "InputFormat": TEXT_INPUT_FORMAT,
                    "OutputFormat": TEXT_OUTPUT_FORMAT,
                    "SerdeInfo": { "SerializationLibrary": JSON_SERDE },
                },
            },
        }),
    )
    .await;
}

/// Create the stream and answer once it is `ACTIVE` — a stream still `CREATING` takes no record.
async fn given_active_stream(router: &MultiServiceRouter, case: &str, role_arn: &str) {
    firehose_ok(
        router,
        case,
        "CreateDeliveryStream",
        json!({
            "DeliveryStreamName": STREAM,
            "DeliveryStreamType": "DirectPut",
            "ExtendedS3DestinationConfiguration": {
                "RoleARN": role_arn,
                "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
                "Prefix": PREFIX,
                "BufferingHints": {
                    "SizeInMBs": SIZE_IN_MBS,
                    "IntervalInSeconds": INTERVAL_SECONDS,
                },
            },
        }),
    )
    .await;

    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let described = firehose_ok(
            router,
            case,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": STREAM }),
        )
        .await;
        let status = described["DeliveryStreamDescription"]["DeliveryStreamStatus"].clone();
        if status == json!("ACTIVE") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stream must reach ACTIVE, got {status}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every object the bucket holds under the destination prefix, in key order: the key
/// `ListObjectsV2` reports for it against the non-empty lines it holds.
async fn delivered_lines(router: &MultiServiceRouter, case: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    for key in keys_under(router, case, BUCKET, PREFIX).await {
        let body = body_of(router, case, BUCKET, &key).await;
        let lines = body
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ToString::to_string)
            .collect();
        found.push((key, lines));
    }
    found
}

/// The objects under the destination prefix once they hold exactly `records` lines between them:
/// the condition that everything put so far has been delivered, rather than a snapshot of a
/// delivery still in flight.
async fn delivered_objects(
    router: &MultiServiceRouter,
    case: &str,
    records: usize,
) -> Vec<(String, Vec<String>)> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let objects = delivered_lines(router, case).await;
        let delivered: usize = objects.iter().map(|(_, lines)| lines.len()).sum();
        assert!(
            delivered <= records,
            "case {case:?}: a delivery writes each accepted record once; {records} record(s) \
             were put and {PREFIX:?} already holds {delivered}"
        );
        if delivered == records {
            return objects;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every record a put accepted must be delivered under {PREFIX:?}; \
             {delivered} of {records} arrived"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Start `sql` against [`DATABASE`] and answer the execution id it mints.
async fn started(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let answered = athena_ok(
        router,
        case,
        "StartQueryExecution",
        json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": "awsdatacatalog" },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    answered["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answered}"))
        .to_string()
}

/// `QueryExecution$Status` once the execution reached a terminal state.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = athena_ok(
            router,
            case,
            "GetQueryExecution",
            json!({ "QueryExecutionId": id }),
        )
        .await;
        let status = reported["QueryExecution"]["Status"].clone();
        let state = status["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED
            || state == QUERY_STATE_FAILED
            || state == QUERY_STATE_CANCELLED
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the execution never reached a terminal state, last saw {status}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run `sql` to completion and answer the types its columns report against its data rows: one
/// entry per row, each cell `None` where the datum carries no value (SQL NULL).
async fn result_of(
    router: &MultiServiceRouter,
    case: &str,
    sql: &str,
) -> (Vec<String>, Vec<Vec<Option<String>>>) {
    let id = started(router, case, sql).await;
    let status = settled(router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {status}"
    );

    let answered = athena_ok(
        router,
        case,
        "GetQueryResults",
        json!({ "QueryExecutionId": id, "MaxResults": 1000 }),
    )
    .await;
    let types = answered["ResultSet"]["ResultSetMetadata"]["ColumnInfo"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|column| column["Type"].as_str().unwrap_or_default().to_string())
        .collect();
    let rows = answered["ResultSet"]["Rows"]
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
        .collect();
    (types, rows)
}

/// A read of a table over a delivery stream's destination prefix answers one row per delivered
/// record, scanning the nested keys the delivery wrote, and holds every member as a value of its
/// column's declared type — converting a scalar that spells one and reading a scalar that does
/// not as NULL for that column alone. A relation the catalog does not hold fails the execution
/// with Athena's own not-found classification instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn athena_holds_every_delivered_json_scalar_as_its_declared_type() {
    let case = "the delivered scalars";
    let router = emulator();
    given_bucket(&router, case).await;
    glue_ok(
        &router,
        case,
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    given_table(&router, case).await;
    let role_arn = given_role(
        &router,
        case,
        DELIVERY_ROLE,
        "firehose.amazonaws.com",
        Some(PUT_OBJECT_ACTION),
    )
    .await;
    given_active_stream(&router, case, &role_arn).await;

    let rows = rows();
    let records: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "Data": ::aws_smithy_types::base64::encode(row.document().as_bytes()),
            })
        })
        .collect();
    let put = firehose_ok(
        &router,
        case,
        "PutRecordBatch",
        json!({ "DeliveryStreamName": STREAM, "Records": records }),
    )
    .await;
    assert_eq!(
        put["FailedPutCount"].as_i64(),
        Some(0),
        "case {case:?}: every record of the batch must be accepted, got {put}"
    );

    let objects = delivered_objects(&router, case, rows.len()).await;

    // The delivery's own keys lie under SUB-prefixes of the registered location — Firehose
    // writes each object under a date-partitioned path of its destination prefix — so a scan
    // that read only the location's immediate children would read none of them.
    for (key, _) in &objects {
        let under = key.strip_prefix(PREFIX).unwrap_or_else(|| {
            panic!("case {case:?}: expected every delivered key to lie under {PREFIX:?}, got {key:?}")
        });
        assert!(
            under.contains('/'),
            "case {case:?}: expected the delivery to write under a nested sub-prefix of \
             {PREFIX:?}, got {key:?}"
        );
    }

    // The data the read is owed is the lines the objects hold, whichever object holds which.
    let delivered: Vec<String> = objects
        .iter()
        .flat_map(|(_, lines)| lines.iter().cloned())
        .collect();
    assert_eq!(
        delivered.len(),
        rows.len(),
        "case {case:?}: expected every put record to be delivered once, got {delivered:?}"
    );

    let projected: Vec<String> = ::std::iter::once("id".to_string())
        .chain(
            TYPED_COLUMNS
                .iter()
                .map(|(member, _, _)| format!("\"{member}\"")),
        )
        .collect();
    let sql = format!(
        "SELECT {} FROM \"{DATABASE}\".\"{TABLE}\" ORDER BY id",
        projected.join(", ")
    );
    let (types, answered) = result_of(&router, case, &sql).await;

    // Every column reports the type the catalog DECLARED for it, whatever the delivered
    // scalars were.
    let expected_types: Vec<String> = ::std::iter::once("integer".to_string())
        .chain(
            TYPED_COLUMNS
                .iter()
                .map(|(_, _, reported)| (*reported).to_string()),
        )
        .collect();
    assert_eq!(
        types, expected_types,
        "case {case:?}: expected each column to report its declared type, got {types:?}"
    );

    // And every delivered record is one row, holding its scalar as each declared type holds it.
    let expected: BTreeMap<i64, Vec<Option<String>>> =
        rows.iter().map(|row| (row.id, row.expected())).collect();
    let read: BTreeMap<i64, Vec<Option<String>>> = answered
        .iter()
        .map(|cells| {
            let id = cells
                .first()
                .cloned()
                .flatten()
                .and_then(|text| text.parse::<i64>().ok())
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected every row to carry its id, got {answered:?}")
                });
            (id, cells.clone())
        })
        .collect();
    for row in &rows {
        let actual = read.get(&row.id);
        assert_eq!(
            actual,
            expected.get(&row.id),
            "case {:?}: expected {:?}, got {actual:?}",
            row.case,
            row.expected()
        );
    }
    assert_eq!(
        read.len(),
        rows.len(),
        "case {case:?}: expected one row per delivered record, got {answered:?}"
    );

    // The same resolution decides the other way for a name the catalog does not hold: Glue
    // answers that the relation is not there, and the execution carries Athena's own
    // classification of that class rather than any engine-level diagnostic.
    let (status, refused) = glue_call(
        &router,
        "GetTable",
        json!({ "DatabaseName": DATABASE, "Name": ABSENT_TABLE }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the catalog must not hold {ABSENT_TABLE:?}, got {status} {refused}"
    );
    let code = refused["__type"]
        .as_str()
        .or_else(|| refused["code"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        code.ends_with("EntityNotFoundException"),
        "case {case:?}: expected EntityNotFoundException for {ABSENT_TABLE:?}, got {refused}"
    );

    let absent_sql = format!("SELECT id FROM \"{DATABASE}\".\"{ABSENT_TABLE}\"");
    let id = started(&router, case, &absent_sql).await;
    let failed = settled(&router, case, &id).await;
    assert_eq!(
        failed["State"].as_str(),
        Some(QUERY_STATE_FAILED),
        "case {case:?}: {absent_sql} must fail, got {failed}"
    );
    // The position is the one the statement wrote the name at: `SELECT id FROM ` is 15
    // characters, so the relation starts at column 16 of line 1.
    let expected_message = format!(
        "TABLE_NOT_FOUND: line 1:16: Table 'awsdatacatalog.{DATABASE}.{ABSENT_TABLE}' does not \
         exist"
    );
    assert_eq!(
        failed["StateChangeReason"].as_str(),
        Some(expected_message.as_str()),
        "case {case:?}: expected Athena's not-found sentence, got {failed}"
    );
    assert_eq!(
        failed["AthenaError"]["ErrorMessage"].as_str(),
        Some(expected_message.as_str()),
        "case {case:?}: expected the error to carry the same sentence, got {failed}"
    );
    assert_eq!(
        failed["AthenaError"]["ErrorCategory"].as_i64(),
        Some(ERROR_CATEGORY_USER),
        "case {case:?}: expected the caller's own category, got {failed}"
    );
    assert_eq!(
        failed["AthenaError"]["ErrorType"].as_i64(),
        Some(ERROR_TYPE_ENTITY_NOT_FOUND),
        "case {case:?}: expected the entity-not-found type, got {failed}"
    );
}
