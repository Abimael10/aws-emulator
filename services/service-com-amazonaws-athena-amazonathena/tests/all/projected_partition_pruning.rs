// SPDX-License-Identifier: CC0-1.0

//! What a read of a table whose partitions are PROJECTED scans.
//!
//! The rule under test: a table that declares partition projection
//! (`TBLPROPERTIES ('projection.enabled' = 'true', 'projection.<key>.type' = …)`) has its
//! partitions COMPUTED from those properties, so a read of it lists only the prefixes the
//! properties — narrowed by the statement's own predicate on the partition key — address. A
//! prefix under the table's location whose `<key>=<value>` path names a value the projection
//! does not admit is no partition of the table at all: its rows are not answered with, and
//! its bytes are not counted in `QueryExecutionStatistics.DataScannedInBytes`, "the number of
//! bytes in the data that was queried" (Smithy: `QueryExecutionStatistics$DataScannedInBytes`,
//! long).
//!
//! The suite is a TABLE whose rows differ along the KIND of values the projection declares
//! (the dates of a range, a list of values, the whole numbers of a range), with each table
//! holding one prefix inside what it declares, a second one inside it, and a third OUTSIDE
//! it — so every row asserts the three readings the rule makes: one prefix for a predicate
//! that fixes the key, the in-range prefixes for a scan with no predicate, and nothing at all
//! for a predicate naming a value the projection excludes.
//!
//! Every table and object is created over the real wire of the service that owns it, and
//! every Athena call goes through the same `MultiServiceRouter` the emulator boots.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table below keeps its data in.
const BUCKET: &str = "athena-projected-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-projected-data/results/";

/// The database every table below is registered in.
const DATABASE: &str = "projected_logs";

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

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
/// service.
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

/// One Athena call that must answer 200.
async fn athena_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("athena.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonAthena.{operation}"))
        .header("authorization", authorization("athena"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: Athena {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Write one object at `path`, which for a bare bucket name creates the bucket.
async fn s3_put(router: &MultiServiceRouter, case: &str, path: &str, body: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "case {case:?}: writing s3://{path} must succeed, got {}",
        response.status()
    );
}

/// Start one query and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let answer = athena_ok(
        router,
        case,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a query execution id, got {answer}"))
        .to_string()
}

/// The `QueryExecution` of `id` once it is TERMINAL, polled until it is or
/// [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let body = athena_ok(
            router,
            case,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        let execution = body["QueryExecution"].clone();
        let state = execution["Status"]["State"].as_str().unwrap_or_default();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return execution;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the execution never reached a terminal state, last saw {execution}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one read reported: the bytes it scanned and the data rows it answered with.
struct Read {
    scanned: i64,
    rows: Vec<Vec<Option<String>>>,
}

/// Run `sql`, require it SUCCEEDED, and answer what it scanned and the rows it reported.
async fn read_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Read {
    let id = start(router, case, sql).await;
    let execution = settled(router, case, &id).await;
    assert_eq!(
        execution["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: {sql} must succeed, got {execution}"
    );
    let scanned = execution["Statistics"]["DataScannedInBytes"]
        .as_i64()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the execution must report the data it scanned, got {execution}")
        });
    let results = athena_ok(
        router,
        case,
        "GetQueryResults",
        &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
    )
    .await;
    let rows = results["ResultSet"]["Rows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        // The first row of a DML result set's first page is the column labels.
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().map(str::to_string))
                .collect()
        })
        .collect();
    Read { scanned, rows }
}

/// One projected table under test: how its partitions are declared, what lies under it, and
/// which of those prefixes the projection admits.
struct Projected {
    /// What the row is.
    case: &'static str,
    /// The table's name, which is also its own directory under the suite's bucket.
    table: &'static str,
    /// Its partition key.
    key: &'static str,
    /// The projection properties it declares, as `('<name>' = '<value>', …)` entries.
    properties: &'static [(&'static str, &'static str)],
    /// The prefixes under it: the partition value, the object's body, and whether the
    /// declared projection admits that value.
    prefixes: &'static [(&'static str, &'static str, bool)],
}

/// The tables: one per kind of values a projection declares.
fn cases() -> Vec<Projected> {
    vec![
        Projected {
            case: "the dates of a range",
            table: "by_date",
            key: "dt",
            properties: &[
                ("projection.enabled", "true"),
                ("projection.dt.type", "date"),
                ("projection.dt.range", "2024-01-01,2024-01-02"),
                ("projection.dt.format", "yyyy-MM-dd"),
            ],
            // Three objects of three different sizes, so what a read scanned says WHICH
            // prefixes it went through.
            prefixes: &[
                ("2024-01-01", "1\n", true),
                ("2024-01-02", "2\n3\n", true),
                ("2024-01-09", "4\n5\n6\n7\n", false),
            ],
        },
        Projected {
            case: "a list of values",
            table: "by_region",
            key: "region",
            properties: &[
                ("projection.enabled", "true"),
                ("projection.region.type", "enum"),
                ("projection.region.values", "eu,us"),
            ],
            prefixes: &[
                ("eu", "1\n", true),
                ("us", "2\n3\n", true),
                ("ap", "4\n5\n6\n7\n", false),
            ],
        },
        Projected {
            case: "the whole numbers of a range",
            table: "by_shard",
            key: "shard",
            properties: &[
                ("projection.enabled", "true"),
                ("projection.shard.type", "integer"),
                ("projection.shard.range", "1,2"),
            ],
            prefixes: &[
                ("1", "1\n", true),
                ("2", "2\n3\n", true),
                ("9", "4\n5\n6\n7\n", false),
            ],
        },
    ]
}

/// How many bytes the objects of the prefixes `admitted` says hold.
fn bytes_of(case: &Projected, admitted: bool) -> i64 {
    i64::try_from(
        case.prefixes
            .iter()
            .filter(|(_, _, inside)| *inside == admitted)
            .map(|(_, body, _)| body.len())
            .sum::<usize>(),
    )
    .unwrap_or(i64::MAX)
}

/// The rows the object of the prefix `value` holds, as a read of the table reports them.
fn rows_of(case: &Projected, value: &str) -> Vec<Vec<Option<String>>> {
    case.prefixes
        .iter()
        .filter(|(prefix, _, _)| *prefix == value)
        .flat_map(|(prefix, body, _)| {
            body.lines().map(move |id| {
                vec![Some(id.to_string()), Some((*prefix).to_string())]
            })
        })
        .collect()
}

/// A read of a projected table goes through the prefixes its PROPERTIES and its predicate
/// select, and through nothing else.
#[tokio::test]
async fn a_projected_read_goes_through_the_prefixes_the_projection_admits() {
    let cases = cases();
    let router = emulator();
    s3_put(&router, "setup", &format!("/{BUCKET}"), "").await;
    for case in &cases {
        for (value, body, _) in case.prefixes {
            s3_put(
                &router,
                case.case,
                &format!("/{BUCKET}/{}/{}={value}/rows.csv", case.table, case.key),
                body,
            )
            .await;
        }
    }
    // The database and the tables are declared through Athena's own DDL, the way a client
    // declares a projected table.
    let id = start(
        &router,
        "setup",
        &format!("CREATE DATABASE `{DATABASE}` LOCATION 's3://{BUCKET}/db/'"),
    )
    .await;
    let created = settled(&router, "setup", &id).await;
    assert_eq!(
        created["Status"]["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "setup: the database must be created, got {created}"
    );
    for case in &cases {
        let properties: Vec<String> = case
            .properties
            .iter()
            .map(|(name, value)| format!("'{name}' = '{value}'"))
            .collect();
        let sql = format!(
            "CREATE EXTERNAL TABLE `{DATABASE}`.`{}` (`id` int) PARTITIONED BY (`{}` string) \
             ROW FORMAT DELIMITED FIELDS TERMINATED BY ',' STORED AS TEXTFILE \
             LOCATION 's3://{BUCKET}/{}/' TBLPROPERTIES ({})",
            case.table,
            case.key,
            case.table,
            properties.join(", ")
        );
        let id = start(&router, case.case, &sql).await;
        let declared = settled(&router, case.case, &id).await;
        assert_eq!(
            declared["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: the table must be declared, got {declared}",
            case.case
        );
    }

    for case in &cases {
        let (first, _, _) = case.prefixes[0];
        let (outside, _, _) = case.prefixes[case.prefixes.len() - 1];

        // A predicate that FIXES the partition key reads that one prefix and no other.
        let one = read_of(
            &router,
            case.case,
            &format!(
                "SELECT id, {} FROM \"{DATABASE}\".\"{}\" WHERE {} = '{first}' ORDER BY id",
                case.key, case.table, case.key
            ),
        )
        .await;
        let expected = i64::try_from(
            case.prefixes
                .iter()
                .find(|(value, _, _)| *value == first)
                .map_or(0, |(_, body, _)| body.len()),
        )
        .unwrap_or(i64::MAX);
        assert_eq!(
            one.scanned, expected,
            "case {:?}: a predicate on the projected key must read only the {expected} bytes \
             of {}={first}, got {}",
            case.case, case.key, one.scanned
        );
        assert_eq!(
            one.rows,
            rows_of(case, first),
            "case {:?}: expected the rows of {}={first}, got {:?}",
            case.case,
            case.key,
            one.rows
        );

        // With NO predicate, the prefixes the projection admits — and not the one it
        // excludes, whose object is physically there.
        let every = read_of(
            &router,
            case.case,
            &format!(
                "SELECT id, {} FROM \"{DATABASE}\".\"{}\" ORDER BY id",
                case.key, case.table
            ),
        )
        .await;
        let admitted = bytes_of(case, true);
        assert_eq!(
            every.scanned, admitted,
            "case {:?}: an unpredicated scan must read the {admitted} bytes the projection \
             admits, got {}",
            case.case, every.scanned
        );
        let excluded = bytes_of(case, false);
        assert!(
            excluded > 0,
            "case {:?}: the suite must hold an object the projection excludes",
            case.case
        );

        // And a predicate naming a value the projection EXCLUDES reads nothing at all,
        // though an object is there under that prefix.
        let none = read_of(
            &router,
            case.case,
            &format!(
                "SELECT count(*) FROM \"{DATABASE}\".\"{}\" WHERE {} = '{outside}'",
                case.table, case.key
            ),
        )
        .await;
        assert_eq!(
            none.scanned, 0,
            "case {:?}: a value outside the declared projection is no partition of the \
             table, got {} bytes scanned",
            case.case, none.scanned
        );
        assert_eq!(
            none.rows,
            vec![vec![Some("0".to_string())]],
            "case {:?}: expected no row of that prefix, got {:?}",
            case.case,
            none.rows
        );

        // The partitions are the PROPERTIES' own, so the catalog holds none of them. A
        // statement whose result is the engine's text output reports no column-label row, so
        // its rows are read as they stand.
        let id = start(
            &router,
            case.case,
            &format!("SHOW PARTITIONS \"{DATABASE}\".\"{}\"", case.table),
        )
        .await;
        let listed = settled(&router, case.case, &id).await;
        assert_eq!(
            listed["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: the partitions must be listed, got {listed}",
            case.case
        );
        let results = athena_ok(
            &router,
            case.case,
            "GetQueryResults",
            &json!({ "QueryExecutionId": id, "MaxResults": 100 }),
        )
        .await;
        let reported = results["ResultSet"]["Rows"].clone();
        assert_eq!(
            reported,
            json!([]),
            "case {:?}: a projected table has no partition records, got {reported}",
            case.case
        );
    }
}
