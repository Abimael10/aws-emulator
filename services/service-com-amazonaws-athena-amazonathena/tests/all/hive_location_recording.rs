// SPDX-License-Identifier: CC0-1.0

//! How the Data Catalog RECORDS the `LOCATION` a Hive DDL statement declares, as Glue's own
//! reads report it.
//!
//! The rule under test: a TABLE's declared location names a PLACE in a store, and the
//! catalog records that place — the path the statement wrote, without the separator that
//! closed it, because the separator follows the path rather than belonging to it. What is
//! dropped is a separator INSIDE the path, so a location whose whole path IS that separator —
//! the ROOT of the store it addresses — keeps it: dropping it there would record the store
//! itself instead of a place in it, and a consumer that joins a key under the recorded
//! location would address a different store path. This is how the Hadoop path a Hive
//! metastore client writes normalises, and it holds for a table's own location
//! (`StorageDescriptor$Location`, "the physical location of the table").
//!
//! A DATABASE's location is recorded by the other rule: its `Database$LocationUri` is the
//! URI the statement declared, byte for byte, separator and all — nothing normalises it, so a
//! client that composes table locations by appending to it reads back exactly the value it
//! submitted.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: how the
//! declared path closes (with one separator, with several, with none), how deep it is, the
//! store it addresses, and whether it names the store's root or a place inside it.
//!
//! Every call goes over the wire the production front door uses: Athena and Glue through
//! `POST /` with `X-Amz-Target` and Amazon S3 through its own REST data plane, all on one
//! `MultiServiceRouter` over ONE shared engine, so what the DDL recorded is read back
//! exactly as a client reads it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{QUERY_STATE_FAILED, QUERY_STATE_SUCCEEDED};
use smithy_server_runtime::MultiServiceRouter;

/// The name Athena reaches the account's AWS Glue Data Catalog by.
const CATALOG: &str = "AwsDataCatalog";

/// The bucket the tables of these suites are declared over, which the object store holds:
/// an external table is registered over storage that exists.
const BUCKET: &str = "athena-location-recording";

/// The database the table statements run in.
const DATABASE: &str = "location_shop";

/// Where a query's results are written, which every submission has to name.
const OUTPUT_LOCATION: &str = "s3://athena-location-recording/results/";

/// How long a suite waits, polling, for a started statement to be readable in a terminal
/// state. Generous: what is asserted is the catalog record, never how fast the host runs.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often that wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A SigV4 `Authorization` header whose credential scope names `service`, which is the
/// routing key the router claims a request by. Neither the key id nor the signature is a
/// credential: this harness turns request authentication off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/{service}/aws4_request, \
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
        .header("host", format!("{service}.us-east-1.amazonaws.com"))
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

/// One Athena call, as a client makes it.
async fn athena(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(router, "athena", "AmazonAthena", operation, body).await
}

/// One Glue call, as a client makes it.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(router, "glue", "AWSGlue", operation, body).await
}

/// Create the bucket the tables are declared over.
async fn create_bucket(router: &MultiServiceRouter) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", "s3.us-east-1.amazonaws.com")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        response.status().is_success(),
        "setup: creating s3://{BUCKET} must succeed, got {} with {}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// Create the database the table statements run in.
async fn create_database(router: &MultiServiceRouter) {
    let (status, created) = glue(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: creating the database must succeed, got {status} with {created:?}"
    );
}

/// Start one statement in the context database and return the `QueryExecutionId` it minted.
async fn start(router: &MultiServiceRouter, case: &str, sql: &str) -> String {
    let (status, answer) = athena(
        router,
        "StartQueryExecution",
        &json!({
            "QueryString": sql,
            "QueryExecutionContext": { "Database": DATABASE, "Catalog": CATALOG },
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: StartQueryExecution({sql}) must be accepted, got {status} with {answer:?}"
    );
    answer["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {answer:?}"))
        .to_string()
}

/// The stored `QueryExecution` of `id`, once it has reached a terminal state.
///
/// Waits for the CONDITION — a state the statement cannot leave — by looking again, so
/// nothing here depends on how fast the host runs.
async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = athena(
            router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetQueryExecution must answer 200, got {status} with {body:?}"
        );
        let found = body["QueryExecution"].clone();
        let state = found["Status"]["State"].as_str().unwrap_or_default();
        if state == QUERY_STATE_SUCCEEDED || state == QUERY_STATE_FAILED {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the statement never settled; last read {found:?}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The stored Glue table `DATABASE`.`table` as `GetTable` reports it, or `None` when the
/// catalog holds none.
async fn glue_table(router: &MultiServiceRouter, table: &str) -> Option<Value> {
    let (status, answer) = glue(
        router,
        "GetTable",
        &json!({ "DatabaseName": DATABASE, "Name": table }),
    )
    .await;
    (status == 200).then(|| answer["Table"].clone())
}

/// The stored Glue database `name` as `GetDatabase` reports it, or `None` when the catalog
/// holds none.
async fn glue_database(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, answer) = glue(router, "GetDatabase", &json!({ "Name": name })).await;
    (status == 200).then(|| answer["Database"].clone())
}

/// One row of the location table: a `LOCATION` a statement declares, and the location the
/// catalog must then record for it.
struct LocationCase {
    /// What this row is about.
    case: &'static str,
    /// The catalog object the statement defines.
    name: &'static str,
    /// The `LOCATION` it declares, as submitted.
    declared: &'static str,
    /// The location every catalog read of that object must report.
    recorded: &'static str,
}

/// A table's `StorageDescriptor$Location` is the place the statement declared: the path it
/// wrote without the separator that closed it, and the bare separator itself where the
/// location names the root of its store.
#[tokio::test]
async fn hive_ddl_records_the_place_a_table_location_names() {
    let router = emulator();
    create_bucket(&router).await;
    create_database(&router).await;

    // The rows differ along how the declared path closes, how deep it is, the store it
    // addresses, and whether it names that store's root or a place inside it.
    let cases: Vec<LocationCase> = vec![
        LocationCase {
            case: "a prefix closed with the separator",
            name: "closed_prefix",
            declared: "s3://athena-location-recording/products/",
            recorded: "s3://athena-location-recording/products",
        },
        LocationCase {
            case: "the same prefix written without one",
            name: "bare_prefix",
            declared: "s3://athena-location-recording/products",
            recorded: "s3://athena-location-recording/products",
        },
        LocationCase {
            case: "a deeper prefix closed with several separators",
            name: "deep_prefix",
            declared: "s3://athena-location-recording/sales/2024/january///",
            recorded: "s3://athena-location-recording/sales/2024/january",
        },
        LocationCase {
            case: "the bucket root, whose whole path is the separator",
            name: "bucket_root",
            declared: "s3://athena-location-recording/",
            recorded: "s3://athena-location-recording/",
        },
        LocationCase {
            case: "the bucket itself, with no path at all",
            name: "bucket_only",
            declared: "s3://athena-location-recording",
            recorded: "s3://athena-location-recording",
        },
        LocationCase {
            case: "a prefix in another store, closed with the separator",
            name: "other_store_prefix",
            declared: "hdfs://cluster/warehouse/items/",
            recorded: "hdfs://cluster/warehouse/items",
        },
        LocationCase {
            case: "the root of another store",
            name: "other_store_root",
            declared: "hdfs://cluster/",
            recorded: "hdfs://cluster/",
        },
    ];

    let mut started: Vec<(&LocationCase, String)> = Vec::new();
    for case in &cases {
        let sql = format!(
            "CREATE EXTERNAL TABLE {} (item_id string, name string) LOCATION '{}'",
            case.name, case.declared
        );
        started.push((case, start(&router, case.case, &sql).await));
    }

    for (case, id) in started {
        let found = settled(&router, case.case, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: a table over storage that exists is registered, got {found:?}",
            case.case
        );
        let table = glue_table(&router, case.name).await.unwrap_or_else(|| {
            panic!(
                "case {:?}: the catalog must hold {DATABASE}.{} after the DDL",
                case.case, case.name
            )
        });
        let actual = table["StorageDescriptor"]["Location"].as_str();
        assert_eq!(
            actual,
            Some(case.recorded),
            "case {:?}: {} must be recorded as {}, got {actual:?}",
            case.case,
            case.declared,
            case.recorded
        );
    }
}

/// A database's `LocationUri` is the URI the statement DECLARED, byte for byte: unlike a
/// table's storage path, it is recorded without normalisation, so the separator that closed
/// it is part of the stored value.
#[tokio::test]
async fn hive_ddl_records_the_place_a_database_location_names() {
    let router = emulator();
    create_bucket(&router).await;
    create_database(&router).await;

    // The rows differ along how the declared path closes, how deep it is, whether it names
    // the store's root or a place inside it, and which store it addresses.
    let cases: Vec<LocationCase> = vec![
        LocationCase {
            case: "a prefix closed with the separator",
            name: "located_closed",
            declared: "s3://athena-location-recording/warehouse/",
            recorded: "s3://athena-location-recording/warehouse/",
        },
        LocationCase {
            case: "the same prefix written without one",
            name: "located_bare",
            declared: "s3://athena-location-recording/warehouse",
            recorded: "s3://athena-location-recording/warehouse",
        },
        LocationCase {
            case: "a deeper prefix closed with the separator",
            name: "located_deep",
            declared: "s3://athena-location-recording/warehouse/eu/2026/",
            recorded: "s3://athena-location-recording/warehouse/eu/2026/",
        },
        LocationCase {
            case: "the bucket root, whose whole path is the separator",
            name: "located_root",
            declared: "s3://athena-location-recording/",
            recorded: "s3://athena-location-recording/",
        },
        LocationCase {
            case: "the root of another store",
            name: "located_other_root",
            declared: "hdfs://cluster/",
            recorded: "hdfs://cluster/",
        },
    ];

    let mut started: Vec<(&LocationCase, String)> = Vec::new();
    for case in &cases {
        let sql = format!(
            "CREATE DATABASE {} LOCATION '{}'",
            case.name, case.declared
        );
        started.push((case, start(&router, case.case, &sql).await));
    }

    for (case, id) in started {
        let found = settled(&router, case.case, &id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {:?}: the database must be registered, got {found:?}",
            case.case
        );
        let database = glue_database(&router, case.name).await.unwrap_or_else(|| {
            panic!(
                "case {:?}: the catalog must hold {} after the DDL",
                case.case, case.name
            )
        });
        let actual = database["LocationUri"].as_str();
        assert_eq!(
            actual,
            Some(case.recorded),
            "case {:?}: {} must be recorded as {}, got {actual:?}",
            case.case,
            case.declared,
            case.recorded
        );
    }
}
