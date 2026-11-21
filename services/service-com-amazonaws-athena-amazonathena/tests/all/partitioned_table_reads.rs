// SPDX-License-Identifier: CC0-1.0

//! What a query over a PARTITIONED Data Catalog table reads.
//!
//! The rule under test: a Data Catalog table's PARTITION KEYS (`Table$PartitionKeys`) are
//! columns of the table like any others — a statement projects them, filters on them and
//! groups by them — and what distinguishes them is only where their values come from: the
//! partition the object a row was read from belongs to, rather than that object's own
//! records. Which partition that is comes from the catalog where the catalog holds one
//! (`Partition$Values`, read under the location that partition records), and otherwise from
//! the object's own Hive `<key>=<value>` path components — the layout the partitions Athena
//! PROJECTS are laid out in — so an object under no partition of a partitioned table is no
//! part of that table's data and is not read at all.
//!
//! Every table, partition and object here is created over the real wire of the service that
//! owns it — Glue's `CreateTable`/`CreatePartition`, S3's `PutObject` — against the one
//! shared engine all three services are mounted on, and every Athena call goes over Athena's
//! own wire through the same `MultiServiceRouter` the production front door uses.
//!
//! The suites are tables: each row is one partitioned table along the axis the rule
//! generalises over (whether the catalog holds the partitions, where a held partition keeps
//! its data, how many partition keys there are and of what types, and whether an object
//! belongs to a partition at all).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The bucket every table below keeps its data in.
const BUCKET: &str = "athena-partitioned-data";

/// Where the queries write their results.
const OUTPUT_LOCATION: &str = "s3://athena-partitioned-data/results/";

/// The database every table below is registered in.
const DATABASE: &str = "partitioned_logs";

/// The columns every table below declares in its storage descriptor, which are the ones its
/// objects carry fields for.
const DATA_COLUMNS: &[(&str, &str)] = &[("id", "int"), ("label", "string")];

/// The SerDe every table below is read by: one record per line, fields separated by a comma.
fn serde_info() -> Value {
    json!({
        "SerializationLibrary": "org.apache.hadoop.hive.serde2.lazy.LazySimpleSerDe",
        "Parameters": { "field.delim": "," },
    })
}

/// How long a poll waits for a started query to settle. Generous: what is asserted is the
/// state the execution reaches, never how fast the host reached it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll sleeps before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The rows a result set reports, each a row of values where `None` is the SQL NULL.
type Rows = Vec<Vec<Option<String>>>;

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

/// Register the table `table` with `partition_keys`, over `s3://BUCKET/<table>/`.
async fn create_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    partition_keys: &[(&str, &str)],
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
                "PartitionKeys": partition_keys
                    .iter()
                    .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                    .collect::<Vec<Value>>(),
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{table}/"),
                    "Columns": DATA_COLUMNS
                        .iter()
                        .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                        .collect::<Vec<Value>>(),
                    "SerdeInfo": serde_info(),
                },
            },
        }),
    )
    .await;
}

/// Register the partition `values` of `table`, at `location` where the row names one.
async fn create_partition(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    values: &[&str],
    location: Option<&str>,
) {
    let mut partition = json!({ "Values": values });
    if let Some(location) = location {
        partition["StorageDescriptor"] = json!({
            "Location": location,
            "Columns": DATA_COLUMNS
                .iter()
                .map(|(name, data_type)| json!({ "Name": name, "Type": data_type }))
                .collect::<Vec<Value>>(),
            "SerdeInfo": serde_info(),
        });
    }
    glue_ok(
        router,
        case,
        "CreatePartition",
        &json!({
            "DatabaseName": DATABASE,
            "TableName": table,
            "PartitionInput": partition,
        }),
    )
    .await;
}

/// Start one query and answer the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let (status, answer) = call(
        router,
        "athena",
        "AmazonAthena",
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

/// The `Status` of `id` once the execution is TERMINAL, polled until it is or
/// [`SETTLE_DEADLINE`] passes — the completion is owed a span after the submit, so what is
/// waited for is the condition and never a fixed time.
async fn settled_status(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = call(
            router,
            "athena",
            "AmazonAthena",
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
async fn result_rows(router: &MultiServiceRouter, case: &str, id: &str) -> Rows {
    let (status, body) = call(
        router,
        "athena",
        "AmazonAthena",
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
        .unwrap_or_else(|| panic!("case {case:?}: expected a result set, got {body}"))
        .iter()
        .skip(1)
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a row of data, got {row}"))
                .iter()
                .map(|datum| datum["VarCharValue"].as_str().map(str::to_string))
                .collect()
        })
        .collect()
}

/// Run `sql`, assert it SUCCEEDED and answer its data rows.
async fn rows_of(router: &MultiServiceRouter, case: &str, sql: &str) -> Rows {
    let id = start(router, case, sql).await;
    let status = settled_status(router, case, &id).await;
    assert_eq!(
        status["State"].as_str(),
        Some(QUERY_STATE_SUCCEEDED),
        "case {case:?}: the query {sql} must succeed, got {status}"
    );
    result_rows(router, case, &id).await
}

/// One partitioned table under test: how it is partitioned, what its objects hold, and the
/// rows a read of it must report.
struct Partitioned {
    /// What the row is.
    case: &'static str,
    /// The table's name, which is also its own directory under the suite's bucket.
    table: &'static str,
    /// The partition keys it declares, in key order.
    keys: &'static [(&'static str, &'static str)],
    /// The partitions the CATALOG holds for it: the values, and the location the partition
    /// records for itself where it records one.
    partitions: &'static [(&'static [&'static str], Option<&'static str>)],
    /// The objects written under the suite's bucket: `(key, body)`.
    objects: &'static [(&'static str, &'static str)],
    /// The statement read back, and the rows it must report.
    sql: &'static str,
    rows: &'static [&'static [&'static str]],
}

/// A partition key is a COLUMN of the table whose values come from the partition the object
/// a row was read from belongs to.
///
/// The rows differ along whether the catalog holds the partitions, where a held partition
/// keeps its data, how many partition keys the table declares and of what types, and whether
/// an object belongs to a partition of the table at all.
#[tokio::test]
async fn a_partition_key_is_a_column_carrying_the_partitions_own_value() {
    let cases: Vec<Partitioned> = vec![
        Partitioned {
            case: "partitions the catalog holds, in the Hive layout under the table",
            table: "held_partitions",
            keys: &[("dt", "string")],
            partitions: &[(&["2026-01-01"], None), (&["2026-01-02"], None)],
            objects: &[
                ("held_partitions/dt=2026-01-01/part-0.csv", "1,alpha\n"),
                ("held_partitions/dt=2026-01-02/part-0.csv", "2,beta\n"),
            ],
            sql: "SELECT id, label, dt FROM held_partitions ORDER BY id",
            rows: &[&["1", "alpha", "2026-01-01"], &["2", "beta", "2026-01-02"]],
        },
        Partitioned {
            case: "a partition that records a location of its own",
            table: "relocated_partition",
            keys: &[("dt", "string")],
            partitions: &[(
                &["2026-02-01"],
                Some("s3://athena-partitioned-data/elsewhere/february/"),
            )],
            objects: &[("elsewhere/february/part-0.csv", "3,gamma\n")],
            sql: "SELECT id, label, dt FROM relocated_partition ORDER BY id",
            rows: &[&["3", "gamma", "2026-02-01"]],
        },
        Partitioned {
            case: "a partitioned table the catalog holds no partition of",
            table: "projected_partitions",
            keys: &[("dt", "string")],
            partitions: &[],
            objects: &[
                ("projected_partitions/dt=2026-03-01/part-0.csv", "4,delta\n"),
                (
                    "projected_partitions/dt=2026-03-02/part-0.csv",
                    "5,epsilon\n",
                ),
            ],
            sql: "SELECT id, label, dt FROM projected_partitions ORDER BY id",
            rows: &[
                &["4", "delta", "2026-03-01"],
                &["5", "epsilon", "2026-03-02"],
            ],
        },
        Partitioned {
            case: "two partition keys, one of them an exact number",
            table: "two_keys",
            keys: &[("region", "string"), ("year", "int")],
            partitions: &[(&["eu", "2026"], None)],
            objects: &[("two_keys/region=eu/year=2026/part-0.csv", "6,zeta\n")],
            sql: "SELECT id, label, region, year FROM two_keys ORDER BY id",
            rows: &[&["6", "zeta", "eu", "2026"]],
        },
        Partitioned {
            case: "an object under no partition of the table",
            table: "loose_objects",
            keys: &[("dt", "string")],
            partitions: &[],
            objects: &[
                ("loose_objects/stray.csv", "7,eta\n"),
                ("loose_objects/dt=2026-05-01/part-0.csv", "8,theta\n"),
            ],
            sql: "SELECT id, label, dt FROM loose_objects ORDER BY id",
            rows: &[&["8", "theta", "2026-05-01"]],
        },
    ];

    let router = emulator();
    s3_put(&router, "setup", &format!("/{BUCKET}"), "").await;
    glue_ok(
        &router,
        "setup",
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;

    for case in &cases {
        create_table(&router, case.case, case.table, case.keys).await;
        for (values, location) in case.partitions {
            create_partition(&router, case.case, case.table, values, *location).await;
        }
        for (key, body) in case.objects {
            s3_put(&router, case.case, &format!("/{BUCKET}/{key}"), body).await;
        }
    }

    for case in &cases {
        let actual = rows_of(&router, case.case, case.sql).await;
        let expected: Rows = case
            .rows
            .iter()
            .map(|row| row.iter().map(|value| Some((*value).to_string())).collect())
            .collect();
        assert_eq!(
            actual, expected,
            "case {:?}: expected the rows {expected:?}, got {actual:?}",
            case.case
        );
    }
}

/// A partition key is addressable wherever a column is: in a predicate, in a grouping and in
/// the wildcard a projection expands.
#[tokio::test]
async fn a_partition_key_is_addressable_wherever_a_column_is() {
    let router = emulator();
    s3_put(&router, "setup", &format!("/{BUCKET}"), "").await;
    glue_ok(
        &router,
        "setup",
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    create_table(&router, "setup", "addressed", &[("dt", "string")]).await;
    for day in ["2026-04-01", "2026-04-02"] {
        create_partition(&router, "setup", "addressed", &[day], None).await;
    }
    s3_put(
        &router,
        "setup",
        &format!("/{BUCKET}/addressed/dt=2026-04-01/part-0.csv"),
        "1,alpha\n2,beta\n",
    )
    .await;
    s3_put(
        &router,
        "setup",
        &format!("/{BUCKET}/addressed/dt=2026-04-02/part-0.csv"),
        "3,gamma\n",
    )
    .await;

    // (case, the statement, the rows it must report)
    let cases: Vec<(&str, &str, Rows)> = vec![
        (
            "a predicate on the partition key",
            "SELECT id FROM addressed WHERE dt = '2026-04-02'",
            vec![vec![Some("3".to_string())]],
        ),
        (
            "a count grouped by the partition key",
            "SELECT dt, count(*) AS rows_read FROM addressed GROUP BY dt ORDER BY dt",
            vec![
                vec![Some("2026-04-01".to_string()), Some("2".to_string())],
                vec![Some("2026-04-02".to_string()), Some("1".to_string())],
            ],
        ),
        (
            "the wildcard, which expands to the partition key last",
            "SELECT * FROM addressed ORDER BY id",
            vec![
                vec![
                    Some("1".to_string()),
                    Some("alpha".to_string()),
                    Some("2026-04-01".to_string()),
                ],
                vec![
                    Some("2".to_string()),
                    Some("beta".to_string()),
                    Some("2026-04-01".to_string()),
                ],
                vec![
                    Some("3".to_string()),
                    Some("gamma".to_string()),
                    Some("2026-04-02".to_string()),
                ],
            ],
        ),
    ];

    for (case, sql, expected) in &cases {
        let actual = rows_of(&router, case, sql).await;
        assert_eq!(
            &actual, expected,
            "case {case:?}: expected the rows {expected:?}, got {actual:?}"
        );
    }
}
