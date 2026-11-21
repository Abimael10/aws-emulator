// SPDX-License-Identifier: CC0-1.0

//! What PLANNING a query against the AWS Glue Data Catalog decides, driven end to end over
//! the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the same router
//! the emulator boots, with Athena, Glue and Amazon S3 on ONE shared engine — so the catalog
//! entries under test are the ones Glue's own API wrote and the data is the objects a client
//! stored.
//!
//! THE RULES UNDER TEST, each one about what the CATALOG says rather than about the bytes
//! under a table's location:
//!
//! 1. **Whose permissions the catalog is reached with.** Athena reads and writes the Data
//!    Catalog a statement addresses with the CALLER's identity, so every Glue action the
//!    statement performs — on the catalog, on the database it addresses and on the table in
//!    it — has to be allowed by the caller's own identity-based policies. A statement whose
//!    catalog actions are not allowed is still taken: the execution is minted and settles
//!    FAILED with Athena's insufficient-permissions reason naming the first refused action
//!    and the resource it was asked about, and `GetQueryResults` on it answers the
//!    did-not-finish-successfully refusal.
//! 2. **What type a column has.** A column's type in a result is the type the CATALOG
//!    declares for it, in Athena's own SQL type names, whatever the values in the objects
//!    are — so `typeof()` over such a column answers the declared type for every row,
//!    including for a column whose every field reads as NULL.
//! 3. **A location that cannot be read.** The catalog never dereferences the location it
//!    records, so a read is what finds it missing: such a query fails with Athena's
//!    FILESYSTEM class (`HIVE_FILESYSTEM_ERROR`, user category, the location-not-found error
//!    type) naming the location — not with an access decision.
//! 4. **A declaration that cannot be read.** The catalog stores a column's type as text and
//!    never reads it, so a read is what finds it unreadable: a query over a table one of
//!    whose columns carries a type text naming no type fails with Athena's INVALID-METADATA
//!    class naming that column and that text.
//!
//! Each suite is a TABLE whose rows differ along what its rule generalises over, with a
//! positive control beside the refusals so a row can tell a refusal from a read that never
//! worked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, which is the Region the ARNs a decision is
/// taken on carry.
const REGION: &str = "us-east-1";

/// The bucket the tables keep their data in, and the one nothing ever creates.
const BUCKET: &str = "athena-catalog-planning";
const ABSENT_BUCKET: &str = "athena-catalog-planning-absent";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-catalog-planning/results/";

/// The database every table below is registered in.
const DATABASE: &str = "planning_logs";

/// The access key id of a caller neither STS nor IAM holds a record for, which is the
/// position the account's own published credential is in.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The columns the TYPED table declares, each with the type name Athena's SQL reports for
/// it: the DDL names whose SQL name differs (`int`, `float`, `string`), the ones where it
/// does not (`date`, `bigint`, `double`, `timestamp`), and a parameterised one.
const TYPED_COLUMNS: &[(&str, &str, &str)] = &[
    ("day", "date", "date"),
    ("bytes_sent", "bigint", "bigint"),
    ("time_taken", "double", "double"),
    ("status", "int", "integer"),
    ("label", "string", "varchar"),
    ("price", "decimal(10,2)", "decimal(10,2)"),
    ("ratio", "float", "real"),
    ("seen_at", "timestamp", "timestamp"),
    // Every field of this column is the dash a log writes where it has no value, so every
    // value of it reads as NULL — and the column is a 64-bit integer all the same.
    ("range_start", "bigint", "bigint"),
];

/// The data object under the typed table's location: one record per line, fields separated
/// by a comma, with the dash in the last field of every record.
const TYPED_DATA: &str = "2026-01-01,1024,0.5,200,desk,10.25,1.5,2026-01-01 10:00:00,-\n\
                          2026-01-02,2048,1.25,404,lamp,3.00,0.25,2026-01-02 11:30:00,-\n";

/// A SigV4 `Authorization` header naming `service` in its credential scope — the routing key
/// the router claims a request by — signed with `access_key_id`, which is what names the
/// principal the request speaks for. Nothing is verified: this harness turns request
/// authentication off.
fn authorization(service: &str, access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=not-verified"
    )
}

/// Athena, Glue and Amazon S3 mounted on ONE shared engine, the way the router mounts every
/// service.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
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
        Arc::clone(&engine),
    ));
    (engine, router)
}

/// One awsJson1_1 call to `service`, signed with `access_key_id`.
async fn call_as(
    router: &MultiServiceRouter,
    service: &str,
    target: &str,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target}.{operation}"))
        .header("authorization", authorization(service, access_key_id))
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

/// One Glue call by the account's own credential, which must succeed.
async fn glue_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) =
        call_as(router, "glue", "AWSGlue", ACCOUNT_KEY_ID, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Glue {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

/// Write one object at `path`, which for a bare bucket name creates the bucket.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3", ACCOUNT_KEY_ID))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// Register the table `table` over `location`, declaring `columns` as `(name, type)` pairs.
///
/// Written through Glue's own API, which stores a column's type as the TEXT it was handed —
/// the catalog does not read it, which is what makes an unreadable declaration something a
/// query is what finds.
async fn create_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    columns: &[(&str, &str)],
    location: &str,
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
                    "Location": location,
                    "Columns": columns
                        .iter()
                        .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                        .collect::<Vec<Value>>(),
                    // The data objects are comma-separated text, which is the SerDe the
                    // table's own definition has to say it is read by.
                    "SerdeInfo": {
                        "SerializationLibrary":
                            "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
                        "Parameters": { "field.delim": "," },
                    },
                },
            },
        }),
    )
    .await;
}

/// The database, the bucket and the typed table every suite reads, with its data object.
async fn given_catalog(router: &MultiServiceRouter, case: &str) {
    s3_put(router, case, &format!("/{BUCKET}"), "").await;
    s3_put(router, case, &format!("/{BUCKET}/typed/data.csv"), TYPED_DATA).await;
    glue_ok(
        router,
        case,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    let typed: Vec<(&str, &str)> = TYPED_COLUMNS
        .iter()
        .map(|(name, declared, _)| (*name, *declared))
        .collect();
    create_table(
        router,
        case,
        "typed",
        &typed,
        &format!("s3://{BUCKET}/typed/"),
    )
    .await;
}

/// Start `sql` as `access_key_id` and answer the `QueryExecutionId` it minted.
async fn start_as(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    sql: &str,
) -> String {
    let (status, answer) = call_as(
        router,
        "athena",
        "AmazonAthena",
        access_key_id,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({sql}) must be accepted, got {status} with {answer}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// The `Status` of `id` once the execution has reached a TERMINAL state, polled until it
/// does or [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so
/// what is waited for is the condition and never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = call_as(
            router,
            "athena",
            "AmazonAthena",
            ACCOUNT_KEY_ID,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body}"
        );
        let reported = body["QueryExecution"]["Status"].clone();
        let state = reported["State"].as_str().unwrap_or_default().to_string();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return reported;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {reported}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The data rows of `id`'s result set, with the label row a DML result set opens with
/// dropped: `None` stands for a datum carrying no value, which is the SQL NULL.
async fn result_rows(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> Vec<Vec<Option<String>>> {
    let (status, body) = call_as(
        router,
        "athena",
        "AmazonAthena",
        ACCOUNT_KEY_ID,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueryResults must answer 200, got {status} with {body}"
    );
    body["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().map(str::to_string))
                .collect()
        })
        .collect()
}

/// One row of the typing table: a statement and the one row of type names it must answer.
struct TypeCase {
    /// What the row is about.
    case: String,
    /// The statement submitted.
    sql: String,
    /// The type names its one result row must carry, in projection order.
    expected: Vec<String>,
}

/// RULE 2: a column's type in a result is the one the CATALOG declares for it, in Athena's
/// own SQL type names — read when the statement is planned, so it is the same answer for
/// every row and does not depend on what any value turned out to be.
///
/// The rows span what the rule generalises over: the declared type (both integer widths, the
/// two approximate types, the fixed-point type with its parameters, the character type, the
/// date and the instant), whether the DDL name and the SQL name differ, a column whose every
/// field reads as NULL, several calls in one statement, and the type a conversion names.
#[tokio::test]
async fn a_catalog_columns_type_is_the_one_the_catalog_declares() {
    let (_engine, router) = emulator();
    given_catalog(&router, "the typed table").await;

    let mut cases: Vec<TypeCase> = TYPED_COLUMNS
        .iter()
        .map(|(column, declared, reported)| TypeCase {
            case: format!("a column declared {declared}"),
            sql: format!("SELECT typeof({column}) FROM typed LIMIT 1"),
            expected: vec![(*reported).to_string()],
        })
        .collect();
    cases.push(TypeCase {
        case: "every column of the table in one projection".to_string(),
        sql: format!(
            "SELECT {} FROM typed LIMIT 1",
            TYPED_COLUMNS
                .iter()
                .map(|(column, _, _)| format!("typeof({column})"))
                .collect::<Vec<String>>()
                .join(", ")
        ),
        expected: TYPED_COLUMNS
            .iter()
            .map(|(_, _, reported)| (*reported).to_string())
            .collect(),
    });
    cases.push(TypeCase {
        case: "the type a conversion names".to_string(),
        sql: "SELECT typeof(CAST(label AS bigint)) FROM typed LIMIT 1".to_string(),
        expected: vec!["bigint".to_string()],
    });
    cases.push(TypeCase {
        case: "a qualified reference to a column of another case".to_string(),
        sql: format!("SELECT typeof(typed.Bytes_Sent) FROM {DATABASE}.typed LIMIT 1"),
        expected: vec!["bigint".to_string()],
    });

    // Every statement is started before any of them is waited on, so the suite waits once
    // for the span a completion is owed rather than once per row.
    let mut started: Vec<(&TypeCase, String)> = Vec::new();
    for case in &cases {
        let id = start_as(&router, &case.case, ACCOUNT_KEY_ID, &case.sql).await;
        started.push((case, id));
    }
    for (TypeCase { case, sql, expected }, id) in &started {
        let status = settled_status(&router, case, id).await;
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: {sql} must succeed, got {status}"
        );
        let rows = result_rows(&router, case, id).await;
        let actual: Vec<Option<String>> = rows.first().cloned().unwrap_or_default();
        let wanted: Vec<Option<String>> =
            expected.iter().map(|name| Some(name.clone())).collect();
        assert_eq!(
            actual, wanted,
            "case {case:?}: expected the declared types {wanted:?}, got {actual:?}"
        );
    }

    // The same table read as DATA: the dash fields of the 64-bit column read as NULL — which
    // is why no value could have told its type — and the values beside them still aggregate.
    let id = start_as(
        &router,
        "the values behind the types",
        ACCOUNT_KEY_ID,
        "SELECT count(*), sum(bytes_sent), count(range_start) FROM typed",
    )
    .await;
    let status = settled_status(&router, "the values behind the types", &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case \"the values behind the types\": the read must succeed, got {status}"
    );
    let rows = result_rows(&router, "the values behind the types", &id).await;
    assert_eq!(
        rows,
        vec![vec![
            Some("2".to_string()),
            Some("3072".to_string()),
            Some("0".to_string()),
        ]],
        "case \"the values behind the types\": expected two rows, 3072 bytes and no value at \
         all in the dash column, got {rows:?}"
    );
}

/// One row of the location table: a table over a location, and the location a failure must
/// name.
struct LocationCase {
    /// What the row is about.
    case: &'static str,
    /// The table registered over it.
    table: &'static str,
    /// The location its catalog entry declares.
    location: String,
    /// The location the failure must name, or `None` for a location that reads.
    named: Option<String>,
}

/// RULE 3: the catalog never dereferences the location it records, so a READ is what finds
/// it missing — and such a query fails with Athena's FILESYSTEM class naming the location,
/// never as an access decision.
///
/// The rows span what the rule generalises over: a location in a bucket nothing created (at
/// a prefix and at the bucket root), a location in a bucket that IS there with nothing under
/// the prefix — storage that can be read and holds no rows — and the table whose data is
/// there.
#[tokio::test]
async fn a_location_that_cannot_be_read_fails_as_a_filesystem_error() {
    let (_engine, router) = emulator();
    given_catalog(&router, "an unreadable location").await;

    let rows: Vec<LocationCase> = vec![
        LocationCase {
            case: "a prefix of a bucket nothing created",
            table: "absent_prefix",
            location: format!("s3://{ABSENT_BUCKET}/logs/"),
            named: Some(format!("s3://{ABSENT_BUCKET}/logs/")),
        },
        LocationCase {
            case: "the root of a bucket nothing created",
            table: "absent_root",
            location: format!("s3://{ABSENT_BUCKET}"),
            named: Some(format!("s3://{ABSENT_BUCKET}")),
        },
        LocationCase {
            case: "a prefix with nothing under it, in a bucket that is there",
            table: "empty_prefix",
            location: format!("s3://{BUCKET}/nothing-here/"),
            named: None,
        },
        LocationCase {
            case: "the location whose data is there",
            table: "present",
            location: format!("s3://{BUCKET}/typed/"),
            named: None,
        },
    ];

    let mut started: Vec<(&LocationCase, String)> = Vec::new();
    for row in &rows {
        create_table(
            &router,
            row.case,
            row.table,
            &[("day", "date")],
            &row.location,
        )
        .await;
        let id = start_as(
            &router,
            row.case,
            ACCOUNT_KEY_ID,
            &format!("SELECT count(*) FROM {}", row.table),
        )
        .await;
        started.push((row, id));
    }

    for (row, id) in &started {
        let case = row.case;
        let status = settled_status(&router, case, id).await;
        let Some(location) = &row.named else {
            assert_eq!(
                status["State"].as_str(),
                Some(QUERY_STATE_SUCCEEDED),
                "case {case:?}: a location that reads must not fail, got {status}"
            );
            continue;
        };
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: the read of {} must fail, got {status}",
            row.table
        );
        let expected =
            format!("HIVE_FILESYSTEM_ERROR: Partition location does not exist: {location}");
        assert_eq!(
            status["StateChangeReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the reason {expected:?}, got {status}"
        );
        let error = &status["AthenaError"];
        assert_eq!(
            (
                error["ErrorCategory"].as_i64(),
                error["ErrorType"].as_i64(),
                error["ErrorMessage"].as_str(),
            ),
            (Some(2), Some(1306), Some(expected.as_str())),
            "case {case:?}: expected the filesystem error class, got {error}"
        );
    }
}

/// RULE 4: the catalog stores a column's type as text and never reads it, so a READ is what
/// finds a declaration unreadable — and such a query fails with Athena's INVALID-METADATA
/// class naming the column and the text it carries.
///
/// The rows span what the rule generalises over: the form of the unreadable text (a name
/// that is no type, a parameterised type whose parameters are not numbers, an unclosed one,
/// a collection whose element type is no type, empty text) and the declarations that ARE
/// types — including the ones the reported issue never cited: a collection, a record, a
/// bounded character type, the narrow integer types, a zoned instant and an interval.
#[tokio::test]
async fn a_column_type_the_catalog_cannot_read_fails_as_invalid_metadata() {
    let (_engine, router) = emulator();
    given_catalog(&router, "an unreadable declaration").await;

    // Each row is one table whose second column carries the declaration under test.
    struct DeclarationCase {
        /// What the row is about.
        case: &'static str,
        /// The table whose second column carries the declaration.
        table: &'static str,
        /// The type text the catalog holds for that column.
        data_type: &'static str,
        /// Whether that text names a type, and the read therefore succeeds.
        names_a_type: bool,
    }
    let rows: Vec<DeclarationCase> = vec![
        DeclarationCase {
            case: "a name that is no type",
            table: "bad_name",
            data_type: "notatype",
            names_a_type: false,
        },
        DeclarationCase {
            case: "a fixed-point type whose parameters are not numbers",
            table: "bad_sizes",
            data_type: "decimal(ten,2)",
            names_a_type: false,
        },
        DeclarationCase {
            case: "an unclosed parameter list",
            table: "bad_open",
            data_type: "varchar(20",
            names_a_type: false,
        },
        DeclarationCase {
            case: "a collection whose element type is no type",
            table: "bad_element",
            data_type: "array<notatype>",
            names_a_type: false,
        },
        DeclarationCase {
            case: "no type text at all",
            table: "bad_empty",
            data_type: "",
            names_a_type: false,
        },
        DeclarationCase {
            case: "a collection of a type",
            table: "good_array",
            data_type: "array<string>",
            names_a_type: true,
        },
        DeclarationCase {
            case: "a record of typed fields",
            table: "good_struct",
            data_type: "struct<id:int,label:string>",
            names_a_type: true,
        },
        DeclarationCase {
            case: "a bounded character type",
            table: "good_char",
            data_type: "char(1)",
            names_a_type: true,
        },
        DeclarationCase {
            case: "the narrowest integer type",
            table: "good_tinyint",
            data_type: "tinyint",
            names_a_type: true,
        },
        DeclarationCase {
            case: "a zoned instant",
            table: "good_zoned",
            data_type: "timestamp with time zone",
            names_a_type: true,
        },
        DeclarationCase {
            case: "an interval the metastore spells with underscores",
            table: "good_interval",
            data_type: "interval_day_time",
            names_a_type: true,
        },
    ];

    let mut started: Vec<(&DeclarationCase, String)> = Vec::new();
    for row in &rows {
        create_table(
            &router,
            row.case,
            row.table,
            &[("day", "date"), ("sc_bytes", row.data_type)],
            &format!("s3://{BUCKET}/typed/"),
        )
        .await;
        let id = start_as(
            &router,
            row.case,
            ACCOUNT_KEY_ID,
            &format!("SELECT count(*) FROM {}", row.table),
        )
        .await;
        started.push((row, id));
    }

    for (row, id) in &started {
        let (case, table, data_type) = (row.case, row.table, row.data_type);
        let status = settled_status(&router, case, id).await;
        if row.names_a_type {
            assert_eq!(
                status["State"].as_str(),
                Some(QUERY_STATE_SUCCEEDED),
                "case {case:?}: a column declared {data_type:?} must read, got {status}"
            );
            continue;
        }
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: the read of {table} must fail, got {status}"
        );
        let expected =
            format!("HIVE_INVALID_METADATA: Invalid column type for column sc_bytes: {data_type}");
        assert_eq!(
            status["StateChangeReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the reason {expected:?}, got {status}"
        );
        let error = &status["AthenaError"];
        assert_eq!(
            (
                error["ErrorCategory"].as_i64(),
                error["ErrorType"].as_i64(),
                error["ErrorMessage"].as_str(),
            ),
            (Some(2), Some(1100), Some(expected.as_str())),
            "case {case:?}: expected the invalid-metadata error class, got {error}"
        );
    }
}

/// One caller of the authorization table: the principal it signs as, what its policy allows,
/// the statement it submits and what the execution must report.
struct Submitter {
    /// What the row is about.
    case: &'static str,
    /// The access key id it signs with, and the IAM user that key belongs to.
    key: &'static str,
    name: &'static str,
    /// The statements of the user's inline policy, beside the `athena:*` every row holds —
    /// a caller has to be allowed to SUBMIT before its catalog permissions are decided.
    catalog_policy: Vec<Value>,
    /// What it submits.
    sql: &'static str,
    /// The `(action, resource portion)` the refusal must name, or `None` for a statement
    /// this caller may perform.
    refused: Option<(&'static str, &'static str)>,
    /// Whether the refusal is IAM's EXPLICIT deny rather than its default one.
    explicit: bool,
}

/// The ARN the account holds an IAM user under, which a refusal names as the caller.
fn user_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:user/{name}", ::engine::emulator_account_id())
}

/// One Glue ARN of this account and Region.
fn glue_arn(resource: &str) -> String {
    format!(
        "arn:aws:glue:{REGION}:{}:{resource}",
        ::engine::emulator_account_id()
    )
}

/// The reason an execution whose catalog access IAM refused reports.
fn insufficient_permissions(
    principal: &str,
    action: &str,
    resource: &str,
    explicit: bool,
) -> String {
    let because = if explicit {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action ")
    };
    format!(
        "Insufficient permissions to execute the query. User: {principal} is not authorized \
         to perform: {action} on resource: {resource} {because}"
    )
}

/// RULE 1: Athena reaches the Data Catalog with the CALLER's identity, so each Glue action a
/// statement performs has to be allowed by the caller's own identity-based policies — and a
/// statement whose catalog actions are not allowed settles FAILED carrying IAM's refusal.
///
/// The rows span what the rule generalises over: the statement (a read of a relation, and
/// each kind of catalog statement, which perform different actions), the document in force
/// (none beyond `athena:*`, one allowing only the first action, one allowing every catalog
/// read, one scoped to another database's resources, one carrying an explicit deny), and the
/// principal (an IAM user and the account's own published credential).
#[tokio::test]
async fn a_query_reaches_the_catalog_only_with_the_callers_glue_permissions() {
    let (engine, router) = emulator();
    given_catalog(&router, "the catalog gate").await;

    let every_read = || {
        vec![json!({
            "Effect": "Allow",
            "Action": ["glue:Get*", "glue:Create*", "glue:Delete*"],
            "Resource": "*",
        })]
    };
    let submitters: Vec<Submitter> = vec![
        Submitter {
            case: "a caller holding no catalog permission at all",
            key: "AKIAEXAMPLEPLANNINGNONE",
            name: "planning-none",
            catalog_policy: Vec::new(),
            sql: "SELECT * FROM typed",
            refused: Some(("glue:GetDatabase", "catalog")),
            explicit: false,
        },
        Submitter {
            case: "a caller allowed every catalog action",
            key: "AKIAEXAMPLEPLANNINGALL",
            name: "planning-all",
            catalog_policy: every_read(),
            sql: "SELECT * FROM typed",
            refused: None,
            explicit: false,
        },
        Submitter {
            case: "a caller allowed only the database read a plan starts with",
            key: "AKIAEXAMPLEPLANNINGDB",
            name: "planning-database",
            catalog_policy: vec![json!({
                "Effect": "Allow",
                "Action": "glue:GetDatabase",
                "Resource": "*",
            })],
            sql: "SELECT * FROM typed",
            refused: Some(("glue:GetTable", "catalog")),
            explicit: false,
        },
        Submitter {
            case: "a caller whose allow is scoped to another database",
            key: "AKIAEXAMPLEPLANNINGOTHER",
            name: "planning-other",
            catalog_policy: vec![json!({
                "Effect": "Allow",
                "Action": "glue:Get*",
                "Resource": [glue_arn("catalog"), glue_arn("database/other_logs")],
            })],
            sql: "SELECT * FROM typed",
            refused: Some(("glue:GetDatabase", "database/planning_logs")),
            explicit: false,
        },
        Submitter {
            case: "a caller carrying an explicit deny of the table read",
            key: "AKIAEXAMPLEPLANNINGDENY",
            name: "planning-deny",
            catalog_policy: vec![
                json!({ "Effect": "Allow", "Action": "glue:Get*", "Resource": "*" }),
                json!({ "Effect": "Deny", "Action": "glue:GetTable", "Resource": "*" }),
            ],
            sql: "SELECT * FROM typed",
            refused: Some(("glue:GetTable", "catalog")),
            explicit: true,
        },
        Submitter {
            case: "a database definition by a caller allowed only catalog reads",
            key: "AKIAEXAMPLEPLANNINGCREATEDB",
            name: "planning-create-database",
            catalog_policy: vec![json!({
                "Effect": "Allow",
                "Action": "glue:Get*",
                "Resource": "*",
            })],
            sql: "CREATE DATABASE planning_new",
            refused: Some(("glue:CreateDatabase", "catalog")),
            explicit: false,
        },
        Submitter {
            case: "the same database definition by a caller allowed to write the catalog",
            key: "AKIAEXAMPLEPLANNINGWRITEDB",
            name: "planning-write-database",
            catalog_policy: every_read(),
            sql: "CREATE DATABASE planning_written",
            refused: None,
            explicit: false,
        },
        Submitter {
            case: "a table dropped by a caller allowed only catalog reads",
            key: "AKIAEXAMPLEPLANNINGDROP",
            name: "planning-drop",
            catalog_policy: vec![json!({
                "Effect": "Allow",
                "Action": "glue:Get*",
                "Resource": "*",
            })],
            sql: "DROP TABLE planning_logs.typed",
            refused: Some(("glue:DeleteTable", "catalog")),
            explicit: false,
        },
        Submitter {
            case: "the databases listed by a caller allowed only one database's reads",
            key: "AKIAEXAMPLEPLANNINGSHOW",
            name: "planning-show",
            catalog_policy: vec![json!({
                "Effect": "Allow",
                "Action": "glue:Get*",
                "Resource": glue_arn("database/planning_logs"),
            })],
            sql: "SHOW DATABASES",
            refused: Some(("glue:GetDatabases", "catalog")),
            explicit: false,
        },
        Submitter {
            case: "a statement that reaches no catalog at all",
            key: "AKIAEXAMPLEPLANNINGNOCATALOG",
            name: "planning-no-catalog",
            catalog_policy: Vec::new(),
            sql: "SELECT 1 AS one",
            refused: None,
            explicit: false,
        },
    ];

    // Each row signs as its OWN principal, so one emulator carries every document under
    // test and the statements can be started together.
    {
        let minted = ::aws_smithy_types::DateTime::from_secs(0);
        let store = iam::service_state(&*engine).expect("the IAM store");
        let mut store = store.lock();
        for submitter in &submitters {
            store.users.insert(
                iam::name_key(submitter.name),
                ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User {
                    path: "/".to_string(),
                    user_name: submitter.name.to_string(),
                    user_id: format!(
                        "AIDAEXAMPLE{}",
                        submitter.name.to_ascii_uppercase().replace('-', "")
                    ),
                    arn: user_arn(submitter.name),
                    create_date: minted,
                    password_last_used: None,
                    permissions_boundary: None,
                    tags: None,
                },
            );
            let mut statements = vec![
                json!({ "Effect": "Allow", "Action": "athena:*", "Resource": "*" }),
                // Athena saves a query's results with the CALLER's identity, so a principal
                // that runs queries holds the object write too — the permission AWS's own
                // Athena policy grants beside the service's own actions. What this suite
                // varies is the CATALOG half of the decision, below.
                json!({ "Effect": "Allow", "Action": "s3:PutObject", "Resource": "*" }),
            ];
            statements.extend(submitter.catalog_policy.iter().cloned());
            store.user_inline_policies.insert(
                iam::name_key(submitter.name),
                BTreeMap::from([(
                    iam::name_key("inline"),
                    iam::InlinePolicy {
                        policy_name: "inline".to_string(),
                        policy_document: json!({
                            "Version": "2012-10-17",
                            "Statement": statements,
                        })
                        .to_string(),
                    },
                )]),
            );
            store.access_keys.insert(
                submitter.key.to_string(),
                iam::AccessKeyRecord {
                    user_key: iam::name_key(submitter.name),
                    status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
                    create_date: minted,
                },
            );
        }
    }

    let mut started: Vec<(&Submitter, String)> = Vec::new();
    for submitter in &submitters {
        let id = start_as(&router, submitter.case, submitter.key, submitter.sql).await;
        started.push((submitter, id));
    }
    // The account's own published credential, which the IAM store holds no policies for, is
    // served the same read the caller holding every catalog action is.
    let unbounded = start_as(
        &router,
        "the account's own credential",
        ACCOUNT_KEY_ID,
        "SELECT count(*) FROM typed",
    )
    .await;

    for (submitter, id) in &started {
        let case = submitter.case;
        let status = settled_status(&router, case, id).await;
        let Some((action, resource)) = submitter.refused else {
            assert_eq!(
                status["State"].as_str(),
                Some(QUERY_STATE_SUCCEEDED),
                "case {case:?}: an allowed statement must be performed, got {status}"
            );
            continue;
        };
        let expected = insufficient_permissions(
            &user_arn(submitter.name),
            action,
            &glue_arn(resource),
            submitter.explicit,
        );
        assert_eq!(
            status["State"].as_str(),
            Some(QUERY_STATE_FAILED),
            "case {case:?}: a refused statement must settle FAILED, got {status}"
        );
        assert_eq!(
            status["StateChangeReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the reason {expected:?}, got {status}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorMessage"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the same message on the error, got {status}"
        );
        assert_eq!(
            status["AthenaError"]["ErrorCategory"].as_i64(),
            Some(2),
            "case {case:?}: expected the caller's own error category, got {status}"
        );

        // And there are no results to read from it: the refusal is the execution's, so a
        // read of the rows it never produced is refused for the state it finished in. Only
        // a statement whose results are ROWS is read that way — one Athena classifies as
        // DDL answers with the engine's own text output whether its run reported the
        // statement done or in error, which is that class's own result.
        if !submitter.sql.starts_with("SELECT") {
            continue;
        }
        let (code, body) = call_as(
            &router,
            "athena",
            "AmazonAthena",
            submitter.key,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            code, 400,
            "case {case:?}: reading the results must be refused, got {code} with {body}"
        );
        let stated = format!("Query did not finish successfully. Final query state: {QUERY_STATE_FAILED}");
        assert_eq!(
            body["Message"].as_str().or_else(|| body["message"].as_str()),
            Some(stated.as_str()),
            "case {case:?}: expected the refusal {stated:?}, got {body}"
        );
    }

    let status = settled_status(&router, "the account's own credential", &unbounded).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case \"the account's own credential\": it must be served, got {status}"
    );

    // The refused definition wrote nothing: the catalog holds no database for it.
    let (code, listed) = call_as(
        &router,
        "glue",
        "AWSGlue",
        ACCOUNT_KEY_ID,
        "GetDatabase",
        &json!({ "Name": "planning_new" }),
    )
    .await;
    assert_eq!(
        code, 400,
        "case \"a refused definition writes nothing\": expected the catalog to hold no such \
         database, got {code} with {listed}"
    );
    // While the one the allowed caller submitted IS there.
    let (code, written) = call_as(
        &router,
        "glue",
        "AWSGlue",
        ACCOUNT_KEY_ID,
        "GetDatabase",
        &json!({ "Name": "planning_written" }),
    )
    .await;
    assert_eq!(
        code, 200,
        "case \"an allowed definition writes the database\": expected it to be there, got \
         {code} with {written}"
    );
}
