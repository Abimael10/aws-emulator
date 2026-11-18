// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `StartCrawler` DOES: the crawl of a crawler's data stores, and
//! the Data Catalog tables it populates from what it found there.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, and the objects
//! a crawl reads are staged through Amazon S3's own `PutObject`, so what is asserted is HTTP
//! behaviour over state the data plane really holds.
//!
//! The rule under test: `StartCrawler` "starts a crawl using the specified crawler" — it
//! refuses a name no crawler in the account has (`EntityNotFoundException`) and a crawler
//! whose crawl is already in progress (`CrawlerRunningException`); otherwise the crawler
//! leaves the state with no run in it, the data behind each of its targets is read, and each
//! dataset found behind them is written into the crawler's target database as one table whose
//! columns are read off that data. For an Amazon S3 target the data is the objects under its
//! path, whose folder levels below the target are the table's partition keys (named off a
//! Hive-style `key=value` segment where the path carries one, by depth where it does not) with
//! one partition per distinct value of those levels; for an Amazon DynamoDB target it is the
//! items of the table the target names, which is one unpartitioned dataset. The clock owes the
//! crawler its way back to `READY` one span later, which is when the crawl's `LastCrawl` and
//! its table counts become readable.
//!
//! The recorded case — a crawler over one Amazon S3 prefix whose Hive-style folders are
//! detected as partitions — is ONE row of the table below; the rows differ along what the
//! rule generalises over: how many folder levels the objects sit at, whether those levels are
//! Hive-style, the layout of the data (comma-separated with and without a header, another
//! delimiter, one JSON document per line), whether the target path ends in the separator,
//! how many objects sit behind the dataset, and which of them the target's `Exclusions`
//! exclude.
//!
//! The due instant a crawl's finish falls at is moved THROUGH THE STORE rather than slept
//! for ([`set_due_instant`]), so every transition here is deterministic.
//!
//! The records those crawls leave on the crawler are what `ListCrawls` reports, which the last
//! section of this file is about; the rule for that listing is stated there.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType, PutRequest,
    ScalarAttributeType, WriteRequest,
};
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    crawl_target_past_the_item_ceiling, crawl_target_unreadable, fold_name,
    ComAmazonawsGlueAwsglueState, AVERAGE_RECORD_SIZE_PARAMETER, CLASSIFICATION_CSV,
    CLASSIFICATION_DYNAMODB, CLASSIFICATION_JSON, CRAWLER_HISTORY_STATE_COMPLETED,
    CRAWLER_HISTORY_STATE_FAILED, CRAWLER_HISTORY_STATE_RUNNING, CRAWLER_STATE_READY,
    CRAWLER_STATE_RUNNING, CRAWL_WITHOUT_TARGET_DATABASE, DEPRECATED_BY_CRAWLER_PARAMETER,
    LAST_CRAWL_STATUS_FAILED, LAST_CRAWL_STATUS_SUCCEEDED, MAX_CRAWLED_EXCLUSIONS,
    MAX_CRAWLED_ITEMS, MAX_CRAWL_HISTORY, RECORD_COUNT_PARAMETER, SAMPLED_CRAWLED_ITEMS,
    SIZE_KEY_PARAMETER, TYPE_OF_DATA_PARAMETER, TYPE_OF_DATA_TABLE, UPDATED_BY_CRAWLER_PARAMETER,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, error_code, error_message, ok, ACCOUNT_ID, REGION};

/// The role every crawler here is created with.
pub(crate) const ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AWSGlueServiceRole-crawl";

/// The database every crawl here writes its output into.
pub(crate) const DATABASE: &str = "crawled_db";

/// An emulator with Glue AND Amazon S3 mounted on one shared engine, plus the engine itself
/// so a test can move the due instant a crawl's finish falls at.
///
/// Amazon S3 is mounted because a crawl READS the objects under its targets: it reaches them
/// over the emulator's own in-process callback, so the objects a test stages with `PutObject`
/// are the objects the crawl classifies.
fn emulator_with_s3() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// An Amazon S3 client over this emulator's own in-process callback — the seam a crawl reads
/// through, so what a test stages here is what the crawl reads. The shared harness owns the
/// one client configuration ([`crate::wire::s3_client`]).
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    crate::wire::s3_client(router)
}

/// Create `bucket` and put every `(key, body)` into it.
pub(crate) async fn stage(router: &MultiServiceRouter, bucket: &str, objects: &[(&str, &str)]) {
    let client = s3(router);
    client
        .create_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {bucket} must be created, got {error:?}"));
    put(router, bucket, objects).await;
}

/// Remove `bucket` from the object store, which a crawler's stored target may then name
/// nothing: the resolution a crawler WRITE makes answers for the store as it was then.
pub(crate) async fn remove_bucket(router: &MultiServiceRouter, bucket: &str) {
    s3(router)
        .delete_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {bucket} must be removed, got {error:?}"));
}

/// Put further objects into a bucket that already exists.
async fn put(router: &MultiServiceRouter, bucket: &str, objects: &[(&str, &str)]) {
    let client = s3(router);
    for (key, body) in objects {
        client
            .put_object()
            .bucket(bucket)
            .key(*key)
            .body(aws_sdk_s3::primitives::ByteStream::from(
                body.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("the object {key} must be stored, got {error:?}"));
    }
}

/// Create the database every crawl here writes into, tolerating one a previous row made.
pub(crate) async fn given_database(router: &MultiServiceRouter, name: &str) {
    let (status, parsed) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": name } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&parsed).ends_with("AlreadyExistsException"),
        "CreateDatabase must answer 200 or report the database is already stored, got {status} \
         with {parsed}"
    );
}

/// Create one crawler over the wire: the required members, the target `target` describes, and
/// whatever `extra` adds.
async fn given_crawler(router: &MultiServiceRouter, name: &str, target: Value, extra: Value) {
    given_crawler_over(router, name, json!([target]), extra).await;
}

/// The same, for a crawler carrying SEVERAL Amazon S3 targets: `targets` is the whole
/// `S3Targets` list.
pub(crate) async fn given_crawler_over(
    router: &MultiServiceRouter,
    name: &str,
    targets: Value,
    extra: Value,
) {
    let mut body = json!({
        "Name": name,
        "Role": ROLE,
        "DatabaseName": DATABASE,
        "Targets": { "S3Targets": targets },
    });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    ok(router, "CreateCrawler", &body).await;
}

/// Move the due instant of every crawl in flight to `at`, so a test can hold the transient
/// window open or make the owed finish due without sleeping.
pub(crate) fn set_due_instant(engine: &Arc<engine::InMemoryEngine>, at: SystemTime) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    for crawler in store.crawlers.values_mut() {
        if crawler.crawl_settles_at.is_some() {
            crawler.crawl_settles_at = Some(at);
        }
    }
}

/// An instant already past, so the next call finishes what the clock owed.
pub(crate) fn already_past() -> SystemTime {
    SystemTime::now() - Duration::from_secs(1)
}

/// An instant far enough out that no owed finish falls due during a test.
pub(crate) fn far_future() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3600)
}

/// Start the crawl of `name` and make the finish the clock owes it due, so the next read
/// reports the crawl that finished.
async fn crawl(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>, name: &str) {
    let body = ok(router, "StartCrawler", &json!({ "Name": name })).await;
    assert_eq!(
        body,
        json!({}),
        "StartCrawler reports no member of its own, got {body}"
    );
    set_due_instant(engine, already_past());
}

/// The `Crawler` structure `GetCrawler` reports for `name`.
async fn crawler_of(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "GetCrawler", &json!({ "Name": name })).await;
    body.get("Crawler")
        .cloned()
        .unwrap_or_else(|| panic!("GetCrawler must report a Crawler, got {body}"))
}

/// The `Table` structure `GetTable` reports, or `None` when the catalog holds no such table.
async fn table_of(router: &MultiServiceRouter, database: &str, table: &str) -> Option<Value> {
    let (status, parsed) = call(
        router,
        "GetTable",
        &json!({ "DatabaseName": database, "Name": table }),
    )
    .await;
    if status != 200 {
        return None;
    }
    parsed.get("Table").cloned()
}

/// The `(name, type)` of every column of one table's storage descriptor, in the order the
/// table declares them.
fn columns_of(table: &Value) -> Vec<(String, String)> {
    named_types(table.pointer("/StorageDescriptor/Columns"))
}

/// The `(name, type)` of every partition key of one table, in the order it declares them.
fn partition_keys_of(table: &Value) -> Vec<(String, String)> {
    named_types(table.get("PartitionKeys"))
}

/// The `(Name, Type)` pairs of a `Column` list, in the order it holds them.
fn named_types(columns: Option<&Value>) -> Vec<(String, String)> {
    columns
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .map(|column| {
                    (
                        column
                            .get("Name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        column
                            .get("Type")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One table parameter, or `None` when the table carries none of that name.
fn parameter_of(table: &Value, name: &str) -> Option<String> {
    table
        .pointer("/Parameters")
        .and_then(Value::as_object)
        .and_then(|parameters| parameters.get(name))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The `Values` of every partition the catalog holds for one table, in the order it holds
/// them.
///
/// Read from the store rather than over the wire because the operation that PAGES over a
/// table's partitions (`GetPartitions`) is still a scaffold; every partition this answers is
/// also read back over the wire, one at a time, by [`served_partition`].
fn stored_partitions(
    engine: &Arc<engine::InMemoryEngine>,
    database: &str,
    table: &str,
) -> Vec<Vec<String>> {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    store
        .database(ACCOUNT_ID, database)
        .and_then(|database| database.tables.get(&fold_name(table)))
        .map(|table| table.partitions.keys().cloned().collect())
        .unwrap_or_default()
}

/// The `Partition` structure `GetPartition` serves for one value list, or `None` when the
/// catalog holds no partition at that address.
async fn served_partition(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    values: &[String],
) -> Option<Value> {
    let (status, parsed) = call(
        router,
        "GetPartition",
        &json!({
            "DatabaseName": database,
            "TableName": table,
            "PartitionValues": values,
        }),
    )
    .await;
    if status != 200 {
        return None;
    }
    parsed.get("Partition").cloned()
}

/// One row of the discovery table: the objects staged, the target crawled, and the table the
/// crawl must write for them.
struct Discovery {
    /// What the row is about.
    case: &'static str,
    /// The bucket the row's objects live in (no underscore: `CreateBucket` refuses one).
    bucket: &'static str,
    /// The objects staged, as `(key, body)`.
    objects: Vec<(&'static str, &'static str)>,
    /// The `S3Target` the crawler carries.
    target: Value,
    /// The name of the table the crawl must write.
    table: &'static str,
    /// The columns that table must declare, as `(name, type)`.
    columns: Vec<(&'static str, &'static str)>,
    /// The partition keys it must declare, as `(name, type)`.
    partition_keys: Vec<(&'static str, &'static str)>,
    /// The partitions it must hold, as the value lists that address them.
    partitions: Vec<Vec<&'static str>>,
    /// What the data must be classified as.
    classification: &'static str,
}

// ---------------------------------------------------------------------------
// What a crawl discovers behind its targets
// ---------------------------------------------------------------------------

/// A crawl writes one table per dataset behind its targets, with the columns the data carries
/// and the partitions its folder levels hold.
///
/// The rows differ along what the rule generalises over: the number of folder levels below
/// the target (none, one, two), whether those levels are Hive-style `key=value` segments, the
/// layout of the data (comma-separated with a header, comma-separated without one, tab
/// separated, one JSON document per line), whether the target path ends in the separator, how
/// many objects sit behind the dataset, and which of them the target's `Exclusions` exclude.
/// The recorded case — a Hive-partitioned comma-separated prefix — is the first row.
#[tokio::test]
async fn a_crawl_writes_the_datasets_its_targets_hold() {
    let rows: Vec<Discovery> = vec![
        Discovery {
            case: "hive-style partitions, one level, csv with a header",
            bucket: "crawl-hive-one",
            objects: vec![
                (
                    "sales/year=2024/part-0.csv",
                    "order_id,amount\n1,10.5\n2,11.25\n",
                ),
                (
                    "sales/year=2025/part-0.csv",
                    "order_id,amount\n3,12.75\n",
                ),
            ],
            target: json!({ "Path": "s3://crawl-hive-one/sales/" }),
            table: "sales",
            columns: vec![("order_id", "bigint"), ("amount", "double")],
            partition_keys: vec![("year", "string")],
            partitions: vec![vec!["2024"], vec!["2025"]],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "hive-style partitions, two levels, target without a trailing separator",
            bucket: "crawl-hive-two",
            objects: vec![
                (
                    "events/year=2024/month=01/a.csv",
                    "id,name,active\n1,alpha,true\n",
                ),
                (
                    "events/year=2024/month=02/a.csv",
                    "id,name,active\n2,beta,false\n",
                ),
                (
                    "events/year=2025/month=01/a.csv",
                    "id,name,active\n3,gamma,true\n",
                ),
            ],
            target: json!({ "Path": "s3://crawl-hive-two/events" }),
            table: "events",
            columns: vec![("id", "bigint"), ("name", "string"), ("active", "boolean")],
            partition_keys: vec![("year", "string"), ("month", "string")],
            partitions: vec![
                vec!["2024", "01"],
                vec!["2024", "02"],
                vec!["2025", "01"],
            ],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "folder levels that are not hive-style are named by their depth",
            bucket: "crawl-plain-folders",
            objects: vec![
                ("logs/2024/part.csv", "line\nfirst\n"),
                ("logs/2025/part.csv", "line\nsecond\n"),
            ],
            target: json!({ "Path": "s3://crawl-plain-folders/logs/" }),
            table: "logs",
            columns: vec![("line", "string")],
            partition_keys: vec![("partition_0", "string")],
            partitions: vec![vec!["2024"], vec!["2025"]],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "objects directly under the target are one unpartitioned dataset",
            bucket: "crawl-flat",
            objects: vec![
                ("flat/one.csv", "id,label\n7,seven\n"),
                ("flat/two.csv", "id,label\n8,eight\n"),
            ],
            target: json!({ "Path": "s3://crawl-flat/flat/" }),
            table: "flat",
            columns: vec![("id", "bigint"), ("label", "string")],
            partition_keys: vec![],
            partitions: vec![],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "one JSON document per line, members as columns",
            bucket: "crawl-json",
            objects: vec![(
                "docs/region=eu/a.json",
                "{\"amount\": 1.5, \"id\": 3, \"ok\": true, \"tags\": [\"x\"], \"who\": \"me\"}\n",
            )],
            target: json!({ "Path": "s3://crawl-json/docs/" }),
            table: "docs",
            // The members are reported in the order the parsed document hands them over,
            // which is by name.
            columns: vec![
                ("amount", "double"),
                ("id", "int"),
                ("ok", "boolean"),
                ("tags", "array<string>"),
                ("who", "string"),
            ],
            partition_keys: vec![("region", "string")],
            partitions: vec![vec!["eu"]],
            classification: CLASSIFICATION_JSON,
        },
        Discovery {
            case: "a headerless delimited object names its columns by position",
            bucket: "crawl-headerless",
            objects: vec![(
                "raw/day=01/rows.csv",
                "1,alpha\n2,beta\n",
            )],
            target: json!({ "Path": "s3://crawl-headerless/raw/" }),
            table: "raw",
            columns: vec![("col0", "bigint"), ("col1", "string")],
            partition_keys: vec![("day", "string")],
            partitions: vec![vec!["01"]],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "another delimiter of the classifier's own set",
            bucket: "crawl-tabs",
            objects: vec![(
                "tsv/day=02/rows.tsv",
                "id\tlabel\n5\tfive\n",
            )],
            target: json!({ "Path": "s3://crawl-tabs/tsv/" }),
            table: "tsv",
            columns: vec![("id", "bigint"), ("label", "string")],
            partition_keys: vec![("day", "string")],
            partitions: vec![vec!["02"]],
            classification: CLASSIFICATION_CSV,
        },
        Discovery {
            case: "an excluded object's folder is no partition of the dataset",
            bucket: "crawl-exclusions",
            objects: vec![
                ("mixed/day=01/rows.csv", "id\n1\n"),
                ("mixed/day=02/rows.tmp", "id\n2\n"),
            ],
            target: json!({
                "Path": "s3://crawl-exclusions/mixed/",
                "Exclusions": ["**/*.tmp"],
            }),
            table: "mixed",
            columns: vec![("id", "bigint")],
            partition_keys: vec![("day", "string")],
            partitions: vec![vec!["01"]],
            classification: CLASSIFICATION_CSV,
        },
    ];

    for row in &rows {
        let (router, engine) = emulator_with_s3();
        let case = row.case;
        stage(&router, row.bucket, &row.objects).await;
        given_database(&router, DATABASE).await;
        given_crawler(&router, "crawler", row.target.clone(), json!({})).await;

        crawl(&router, &engine, "crawler").await;

        let crawler = crawler_of(&router, "crawler").await;
        assert_eq!(
            crawler.get("State").and_then(Value::as_str),
            Some(CRAWLER_STATE_READY),
            "case {case:?}: the finished crawl leaves the crawler ready, got {crawler}"
        );
        assert_eq!(
            crawler.pointer("/LastCrawl/Status").and_then(Value::as_str),
            Some(LAST_CRAWL_STATUS_SUCCEEDED),
            "case {case:?}: the crawl read its target, got {crawler}"
        );
        assert!(
            crawler.pointer("/LastCrawl/ErrorMessage").is_none(),
            "case {case:?}: a crawl that succeeded carries no error message, got {crawler}"
        );

        let table = table_of(&router, DATABASE, row.table)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the crawl must write the table {}", row.table));
        let expected_columns: Vec<(String, String)> = row
            .columns
            .iter()
            .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
            .collect();
        let actual_columns = columns_of(&table);
        assert_eq!(
            actual_columns, expected_columns,
            "case {case:?}: expected the columns {expected_columns:?}, got {actual_columns:?}"
        );
        let expected_keys: Vec<(String, String)> = row
            .partition_keys
            .iter()
            .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
            .collect();
        let actual_keys = partition_keys_of(&table);
        assert_eq!(
            actual_keys, expected_keys,
            "case {case:?}: expected the partition keys {expected_keys:?}, got {actual_keys:?}"
        );
        assert_eq!(
            parameter_of(&table, "classification").as_deref(),
            Some(row.classification),
            "case {case:?}: expected the data classified as {}, got {table}",
            row.classification
        );
        assert_eq!(
            parameter_of(&table, UPDATED_BY_CRAWLER_PARAMETER).as_deref(),
            Some("crawler"),
            "case {case:?}: the table records the crawler that wrote it, got {table}"
        );

        let expected_partitions: Vec<Vec<String>> = row
            .partitions
            .iter()
            .map(|values| values.iter().map(|value| (*value).to_string()).collect())
            .collect();
        let actual_partitions = stored_partitions(&engine, DATABASE, row.table);
        assert_eq!(
            actual_partitions, expected_partitions,
            "case {case:?}: expected the partitions {expected_partitions:?}, got \
             {actual_partitions:?}"
        );
        // Every discovered partition is really served, and its own location is the folder its
        // slice of the data sits in, under the table's location.
        let location = table
            .pointer("/StorageDescriptor/Location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for values in &expected_partitions {
            let partition = served_partition(&router, DATABASE, row.table, values)
                .await
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the partition {values:?} must be served")
                });
            let partition_location = partition
                .pointer("/StorageDescriptor/Location")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                partition_location.starts_with(&location) && partition_location != location,
                "case {case:?}: expected the partition {values:?} under {location}, got \
                 {partition_location}"
            );
            for value in values {
                assert!(
                    partition_location.contains(value.as_str()),
                    "case {case:?}: the partition {values:?} sits in the folder its values \
                     name, got {partition_location}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The crawl is in flight until the clock finishes it
// ---------------------------------------------------------------------------

/// A crawl is IN PROGRESS from the start until the instant the clock owed it its finish: the
/// crawler reports the running state and no report of this crawl, a second start is refused,
/// and the finish the clock owed is materialised by the next read.
#[tokio::test]
async fn a_crawl_is_in_flight_until_the_clock_finishes_it() {
    let (router, engine) = emulator_with_s3();
    stage(
        &router,
        "crawl-in-flight",
        &[("data/year=2024/a.csv", "id\n1\n")],
    )
    .await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "crawler",
        json!({ "Path": "s3://crawl-in-flight/data/" }),
        json!({}),
    )
    .await;

    ok(&router, "StartCrawler", &json!({ "Name": "crawler" })).await;
    // Held open, so the transient window is observable without sleeping for it.
    set_due_instant(&engine, far_future());

    let running = crawler_of(&router, "crawler").await;
    assert_eq!(
        running.get("State").and_then(Value::as_str),
        Some(CRAWLER_STATE_RUNNING),
        "case \"in flight\": the crawl is in progress, got {running}"
    );
    assert!(
        running.get("LastCrawl").is_none(),
        "case \"in flight\": a first crawl still running has produced no report, got {running}"
    );

    // "If the crawler is already running, returns a CrawlerRunningException."
    let (status, parsed) = call(&router, "StartCrawler", &json!({ "Name": "crawler" })).await;
    assert_eq!(
        status, 400,
        "case \"second start\": expected 400, got {status} with {parsed}"
    );
    assert!(
        error_code(&parsed).ends_with("CrawlerRunningException"),
        "case \"second start\": expected CrawlerRunningException, got {parsed}"
    );
    assert_eq!(
        error_message(&parsed),
        "Crawler with name crawler has already started",
        "case \"second start\": expected the refusal to name the crawler, got {parsed}"
    );

    // A write to the crawler's definition is refused for the same reason, and the delete too:
    // both are decided by the state this crawl put the crawler in.
    for operation in ["UpdateCrawler", "DeleteCrawler"] {
        let (status, parsed) = call(&router, operation, &json!({ "Name": "crawler" })).await;
        assert_eq!(
            status, 400,
            "case {operation:?}: a crawl in progress refuses it, got {status} with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with("CrawlerRunningException"),
            "case {operation:?}: expected CrawlerRunningException, got {parsed}"
        );
    }

    // The finish the clock owed is materialised by the next read, whoever reads it.
    set_due_instant(&engine, already_past());
    let finished = crawler_of(&router, "crawler").await;
    assert_eq!(
        finished.get("State").and_then(Value::as_str),
        Some(CRAWLER_STATE_READY),
        "case \"finished\": the owed finish is materialised on read, got {finished}"
    );
    assert_eq!(
        finished.pointer("/LastCrawl/Status").and_then(Value::as_str),
        Some(LAST_CRAWL_STATUS_SUCCEEDED),
        "case \"finished\": the finished crawl reports its status, got {finished}"
    );
    // A second read of a finished crawl reports the same crawl: nothing about it is derived
    // from the instant somebody asked.
    let again = crawler_of(&router, "crawler").await;
    assert_eq!(
        again, finished,
        "case \"finished\": two reads of one finished crawl agree, got {again} and {finished}"
    );

    // The crawl that finished is the one the metrics report on: one table created, none
    // updated and none deleted, by the crawl that wrote it.
    let metrics = ok(
        &router,
        "GetCrawlerMetrics",
        &json!({ "CrawlerNameList": ["crawler"] }),
    )
    .await;
    let reported = metrics
        .pointer("/CrawlerMetricsList/0")
        .cloned()
        .unwrap_or_else(|| panic!("GetCrawlerMetrics must report the crawler, got {metrics}"));
    assert_eq!(
        reported.get("TablesCreated").and_then(Value::as_i64),
        Some(1),
        "case \"metrics\": the crawl created one table, got {reported}"
    );
    assert_eq!(
        reported.get("TablesUpdated").and_then(Value::as_i64).unwrap_or(0),
        0,
        "case \"metrics\": the crawl updated no table, got {reported}"
    );
    assert_eq!(
        reported.get("TablesDeleted").and_then(Value::as_i64).unwrap_or(0),
        0,
        "case \"metrics\": the crawl deleted no table, got {reported}"
    );
}

// ---------------------------------------------------------------------------
// What a second crawl does to the tables the first one wrote
// ---------------------------------------------------------------------------

/// One row of the update-behaviour table: the policy the crawler states, and what a second
/// crawl of a dataset whose schema changed leaves behind.
struct SchemaChange {
    /// What the row is about.
    case: &'static str,
    /// The `SchemaChangePolicy` the crawler is created with, if any.
    policy: Value,
    /// The columns the stored table must declare after the second crawl.
    columns: Vec<(&'static str, &'static str)>,
    /// How many tables that crawl must report as updated.
    updates: i64,
}

/// A second crawl of the same crawler follows its `SchemaChangePolicy$UpdateBehavior`: the
/// default writes the schema it inferred over the stored table, and the logging behaviour
/// leaves the stored table exactly as the first crawl wrote it.
///
/// The rows are the modelled `UpdateBehavior` values, from
/// `smithy_query(query="shape",shape="UpdateBehavior")`.
#[tokio::test]
async fn a_second_crawl_follows_the_crawler_update_behaviour() {
    let rows: Vec<SchemaChange> = vec![
        SchemaChange {
            case: "no policy takes the update behaviour",
            policy: json!({}),
            columns: vec![("id", "bigint"), ("label", "string")],
            updates: 1,
        },
        SchemaChange {
            case: "UPDATE_IN_DATABASE writes the new schema",
            policy: json!({ "SchemaChangePolicy": { "UpdateBehavior": "UPDATE_IN_DATABASE" } }),
            columns: vec![("id", "bigint"), ("label", "string")],
            updates: 1,
        },
        SchemaChange {
            case: "LOG leaves the stored table alone",
            policy: json!({ "SchemaChangePolicy": { "UpdateBehavior": "LOG" } }),
            columns: vec![("id", "bigint")],
            updates: 0,
        },
    ];

    for row in &rows {
        let (case, policy, expected_columns, expected_updates) =
            (row.case, &row.policy, &row.columns, row.updates);
        let (router, engine) = emulator_with_s3();
        stage(&router, "crawl-rerun", &[("data/day=01/a.csv", "id\n1\n")]).await;
        given_database(&router, DATABASE).await;
        given_crawler(
            &router,
            "crawler",
            json!({ "Path": "s3://crawl-rerun/data/" }),
            policy.clone(),
        )
        .await;

        crawl(&router, &engine, "crawler").await;
        let first = table_of(&router, DATABASE, "data")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the first crawl writes the table"));
        assert_eq!(
            columns_of(&first),
            vec![("id".to_string(), "bigint".to_string())],
            "case {case:?}: the first crawl reads the columns the data carried, got {first}"
        );

        // The same dataset, now carrying a second column.
        put(
            &router,
            "crawl-rerun",
            &[("data/day=01/a.csv", "id,label\n1,alpha\n")],
        )
        .await;
        crawl(&router, &engine, "crawler").await;

        let second = table_of(&router, DATABASE, "data")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the table is still there"));
        let expected: Vec<(String, String)> = expected_columns
            .iter()
            .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
            .collect();
        let actual = columns_of(&second);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the columns {expected:?}, got {actual:?}"
        );
        // One table, not two: a second crawl of the same dataset writes the same table.
        let tables = ok(
            &router,
            "GetTables",
            &json!({ "DatabaseName": DATABASE }),
        )
        .await;
        assert_eq!(
            crate::wire::table_names(&tables),
            vec!["data".to_string()],
            "case {case:?}: the second crawl writes no second table, got {tables}"
        );

        let metrics = ok(
            &router,
            "GetCrawlerMetrics",
            &json!({ "CrawlerNameList": ["crawler"] }),
        )
        .await;
        let updated = metrics
            .pointer("/CrawlerMetricsList/0/TablesUpdated")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        assert_eq!(
            updated, expected_updates,
            "case {case:?}: expected {expected_updates} table(s) updated, got {metrics}"
        );
    }
}

/// One row of the delete-behaviour table: the policy the crawler states, and what becomes of
/// a table it wrote whose data a later crawl no longer finds.
struct Abandoned {
    /// What the row is about.
    case: &'static str,
    /// The `SchemaChangePolicy` the crawler is created with, if any.
    policy: Value,
    /// Whether the abandoned table is still in the database afterwards.
    survives: bool,
    /// Whether it is marked as deprecated by this crawler.
    deprecated: bool,
    /// How many tables the crawl must report as deleted.
    deletes: i64,
}

/// A table THIS crawler wrote whose data a later crawl no longer finds follows the crawler's
/// `SchemaChangePolicy$DeleteBehavior`: the default marks it deprecated and keeps it, the
/// deleting behaviour removes it, and the logging behaviour leaves it untouched.
///
/// The rows are the modelled `DeleteBehavior` values, from
/// `smithy_query(query="shape",shape="DeleteBehavior")`.
#[tokio::test]
async fn a_crawl_follows_the_crawler_delete_behaviour_for_data_that_is_gone() {
    let rows: Vec<Abandoned> = vec![
        Abandoned {
            case: "no policy takes the deprecating behaviour",
            policy: json!({}),
            survives: true,
            deprecated: true,
            deletes: 0,
        },
        Abandoned {
            case: "DEPRECATE_IN_DATABASE keeps the table and marks it",
            policy: json!({ "SchemaChangePolicy": { "DeleteBehavior": "DEPRECATE_IN_DATABASE" } }),
            survives: true,
            deprecated: true,
            deletes: 0,
        },
        Abandoned {
            case: "DELETE_FROM_DATABASE removes the table",
            policy: json!({ "SchemaChangePolicy": { "DeleteBehavior": "DELETE_FROM_DATABASE" } }),
            survives: false,
            deprecated: false,
            deletes: 1,
        },
        Abandoned {
            case: "LOG leaves the table untouched",
            policy: json!({ "SchemaChangePolicy": { "DeleteBehavior": "LOG" } }),
            survives: true,
            deprecated: false,
            deletes: 0,
        },
    ];

    for row in &rows {
        let (case, policy, survives, deprecated, expected_deletes) = (
            row.case,
            &row.policy,
            row.survives,
            row.deprecated,
            row.deletes,
        );
        let (router, engine) = emulator_with_s3();
        stage(
            &router,
            "crawl-abandoned",
            &[
                ("first/day=01/a.csv", "id\n1\n"),
                ("second/day=01/a.csv", "id\n2\n"),
            ],
        )
        .await;
        given_database(&router, DATABASE).await;
        given_crawler(
            &router,
            "crawler",
            json!({ "Path": "s3://crawl-abandoned/first/" }),
            policy.clone(),
        )
        .await;
        crawl(&router, &engine, "crawler").await;
        assert!(
            table_of(&router, DATABASE, "first").await.is_some(),
            "case {case:?}: the first crawl writes the table it found"
        );

        // The crawler is pointed at another dataset, so the table it wrote before is one
        // whose data the next crawl does not find.
        ok(
            &router,
            "UpdateCrawler",
            &json!({
                "Name": "crawler",
                "Targets": { "S3Targets": [{ "Path": "s3://crawl-abandoned/second/" }] },
            }),
        )
        .await;
        crawl(&router, &engine, "crawler").await;

        let abandoned = table_of(&router, DATABASE, "first").await;
        assert_eq!(
            abandoned.is_some(),
            survives,
            "case {case:?}: expected the abandoned table to survive: {survives}, got \
             {abandoned:?}"
        );
        if let Some(table) = abandoned.as_ref() {
            let marked = parameter_of(table, DEPRECATED_BY_CRAWLER_PARAMETER);
            assert_eq!(
                marked.is_some(),
                deprecated,
                "case {case:?}: expected the table marked deprecated: {deprecated}, got {table}"
            );
        }
        assert!(
            table_of(&router, DATABASE, "second").await.is_some(),
            "case {case:?}: the second crawl writes the dataset it did find"
        );
        let metrics = ok(
            &router,
            "GetCrawlerMetrics",
            &json!({ "CrawlerNameList": ["crawler"] }),
        )
        .await;
        let deleted = metrics
            .pointer("/CrawlerMetricsList/0/TablesDeleted")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        assert_eq!(
            deleted, expected_deletes,
            "case {case:?}: expected {expected_deletes} table(s) deleted, got {metrics}"
        );
    }
}

// ---------------------------------------------------------------------------
// A crawl that cannot be carried out
// ---------------------------------------------------------------------------

/// A crawl whose output has nowhere to go, whose target holds nothing, or whose target
/// carries more exclusion patterns than one crawl matches against is reported for what it
/// was: the first and the last FAILED carrying the reason and writing no table, the middle one
/// SUCCEEDED having written none — a target that holds no dataset is not a failure.
#[tokio::test]
async fn a_crawl_reports_what_it_could_and_could_not_do() {
    let (router, engine) = emulator_with_s3();
    stage(&router, "crawl-outcomes", &[("full/day=01/a.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;

    // A crawler that names no database has nowhere to write the tables it finds.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "no-database",
            "Role": ROLE,
            "Targets": { "S3Targets": [{ "Path": "s3://crawl-outcomes/full/" }] },
        }),
    )
    .await;
    crawl(&router, &engine, "no-database").await;
    let failed = crawler_of(&router, "no-database").await;
    assert_eq!(
        failed.pointer("/LastCrawl/Status").and_then(Value::as_str),
        Some(LAST_CRAWL_STATUS_FAILED),
        "case \"no database\": the crawl failed, got {failed}"
    );
    assert_eq!(
        failed
            .pointer("/LastCrawl/ErrorMessage")
            .and_then(Value::as_str),
        Some(CRAWL_WITHOUT_TARGET_DATABASE),
        "case \"no database\": the crawl reports why it failed, got {failed}"
    );
    assert!(
        table_of(&router, DATABASE, "full").await.is_none(),
        "case \"no database\": a crawl that failed writes no table"
    );

    // A target that holds no object at all: there is nothing there to describe, which is a
    // crawl that found nothing rather than one that failed.
    given_crawler(
        &router,
        "empty-target",
        json!({ "Path": "s3://crawl-outcomes/absent/" }),
        json!({}),
    )
    .await;
    crawl(&router, &engine, "empty-target").await;
    let empty = crawler_of(&router, "empty-target").await;
    assert_eq!(
        empty.pointer("/LastCrawl/Status").and_then(Value::as_str),
        Some(LAST_CRAWL_STATUS_SUCCEEDED),
        "case \"empty target\": a target holding nothing is no failure, got {empty}"
    );
    assert!(
        table_of(&router, DATABASE, "absent").await.is_none(),
        "case \"empty target\": a crawl that found nothing writes no table"
    );

    // A target carrying more exclusion patterns than one crawl matches its objects against is
    // data this crawl did not read, so the crawl fails saying so rather than reading the
    // target with some of its patterns dropped. The ceiling itself and one past it are both
    // exercised: the row above this one crawls a target with none, the discovery table crawls
    // one with a single pattern, and this is the first count past the bound.
    let too_many: Vec<String> = (0..=MAX_CRAWLED_EXCLUSIONS)
        .map(|index| format!("**/*.skip{index}"))
        .collect();
    given_crawler(
        &router,
        "too-many-exclusions",
        json!({ "Path": "s3://crawl-outcomes/full/", "Exclusions": too_many }),
        json!({}),
    )
    .await;
    crawl(&router, &engine, "too-many-exclusions").await;
    let unmatched = crawler_of(&router, "too-many-exclusions").await;
    assert_eq!(
        unmatched
            .pointer("/LastCrawl/Status")
            .and_then(Value::as_str),
        Some(LAST_CRAWL_STATUS_FAILED),
        "case \"past the exclusion ceiling\": the crawl did not read the target, got {unmatched}"
    );
    assert_eq!(
        unmatched
            .pointer("/LastCrawl/ErrorMessage")
            .and_then(Value::as_str),
        Some(crawl_target_unreadable("s3://crawl-outcomes/full/").as_str()),
        "case \"past the exclusion ceiling\": the crawl names the target it could not read, \
         got {unmatched}"
    );
    assert!(
        table_of(&router, DATABASE, "full").await.is_none(),
        "case \"past the exclusion ceiling\": a crawl that failed writes no table"
    );
}

/// A name no crawler in the account has is a miss, whatever its spelling: "a specified entity
/// does not exist", with the catalog's own wording for a crawler.
///
/// The rows differ along the spelling of the absent name: one never created, and the stored
/// crawler's name in another casing — a crawler is addressed by name exactly as it was
/// created, so another casing addresses no crawler.
#[tokio::test]
async fn starting_a_crawler_that_does_not_exist_is_a_miss() {
    let (router, _engine) = emulator_with_s3();
    given_database(&router, DATABASE).await;
    // The crawler's target is resolved against the object store when it is created, so the
    // bucket it names is one the store holds.
    stage(&router, "crawl-miss", &[("data/rows.csv", "id\n1\n")]).await;
    given_crawler(
        &router,
        "stored",
        json!({ "Path": "s3://crawl-miss/data/" }),
        json!({}),
    )
    .await;

    for (case, name) in [
        ("a name nothing created", "absent"),
        ("the stored name in another casing", "STORED"),
    ] {
        let (status, parsed) = call(&router, "StartCrawler", &json!({ "Name": name })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            format!("Crawler entry with name {name} does not exist"),
            "case {case:?}: expected the miss to name the crawler, got {parsed}"
        );
    }
}

// ---------------------------------------------------------------------------
// The crawls a crawler has run, as `ListCrawls` reports them
// ---------------------------------------------------------------------------
//
// RULE: `ListCrawls` "returns all the crawls of a specified crawler" — it answers 200 with one
// `CrawlerHistory` per crawl the addressed crawler has run, the one in flight included, newest
// first, each computed from that crawl's OWN record (the id minted for it, the state it stands
// in, the instants it ran between, the summary of what it wrote into the catalog and the reason
// it failed), narrowed to the crawls every `CrawlsFilter` criterion admits and served one page
// of at most `MaxResults` at a time with a `NextToken` that resumes the walk. A name no crawler
// in the account has is `EntityNotFoundException`, a `Filters` entry that is no comparison on a
// field of a crawl is `InvalidInputException`, and the call changes nothing a later caller sees.

/// The largest page `ListCrawls` serves, and the page it serves when the request names no
/// `MaxResults`.
///
/// Source: `ListCrawlsRequest$MaxResults` itself — "the maximum number of results to return.
/// The default is 20, and maximum is 100"
/// (`smithy_query(query="operation",operation="ListCrawls")`).
const MAX_CRAWLS_PAGE: usize = 100;
/// See [`MAX_CRAWLS_PAGE`].
const DEFAULT_CRAWLS_PAGE: usize = 20;

/// The published upper bound of `MaxResults` as a MEMBER: it targets `PageSize`, range
/// 1..1000, so a value between the documented maximum above and this one is accepted and
/// served as pages of that maximum, and one past this is refused at the boundary.
const MAX_PAGE_SIZE_MEMBER: i32 = 1_000;

/// One `ListCrawls` request body: the crawler the row addresses, plus the filters, the page
/// size and the cursor the row names.
fn crawls_request(
    crawler: &str,
    filters: Value,
    page_size: Option<i32>,
    token: Option<&str>,
) -> Value {
    let mut body = json!({ "CrawlerName": crawler });
    if !filters.is_null() {
        body["Filters"] = filters;
    }
    if let Some(size) = page_size {
        body["MaxResults"] = json!(size);
    }
    if let Some(cursor) = token {
        body["NextToken"] = json!(cursor);
    }
    body
}

/// The `Crawls` of one answer, which must be present even when empty.
fn crawls_of(body: &Value) -> Vec<Value> {
    body.get("Crawls")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ListCrawls must report a Crawls list, got {body}"))
        .clone()
}

/// The `CrawlId` of every crawl reported, in the order the service served them.
fn crawl_ids(body: &Value) -> Vec<String> {
    crawls_of(body)
        .iter()
        .map(|crawl| {
            crawl
                .get("CrawlId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("every crawl carries a CrawlId, got {crawl}"))
                .to_string()
        })
        .collect()
}

/// One text member of a reported crawl, or `None` when it carries none.
fn crawl_member(crawl: &Value, member: &str) -> Option<String> {
    crawl.get(member).and_then(Value::as_str).map(str::to_string)
}

/// One instant member of a reported crawl, as the seconds since the epoch the protocol puts
/// a timestamp on the wire as, or `None` when the crawl carries none.
fn crawl_instant(crawl: &Value, member: &str) -> Option<f64> {
    crawl.get(member).and_then(Value::as_f64)
}

/// The `Summary` of a reported crawl, read as the document it is: "a run summary for the
/// specific crawl in JSON".
fn crawl_summary(crawl: &Value) -> Value {
    let text = crawl_member(crawl, "Summary")
        .unwrap_or_else(|| panic!("a finished crawl reports a Summary, got {crawl}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("a crawl's Summary is JSON, got {text:?} ({error})"))
}

/// One count of a crawl's run summary.
fn summarised(summary: &Value, member: &str) -> i64 {
    summary
        .get(member)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("a run summary states {member}, got {summary}"))
}

/// The crawls of `crawler`, unfiltered and in one page large enough for the whole history.
async fn listed_crawls(router: &MultiServiceRouter, crawler: &str) -> Value {
    ok(
        router,
        "ListCrawls",
        &crawls_request(
            crawler,
            Value::Null,
            Some(i32::try_from(MAX_CRAWLS_PAGE).unwrap_or(1)),
            None,
        ),
    )
    .await
}

/// Run one crawl of `name` to its end, with the finish the clock owes it falling due at an
/// instant AFTER the crawl began — so the crawl reports the span it really ran over, and
/// nothing is slept for.
async fn crawl_to_the_end(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) {
    ok(router, "StartCrawler", &json!({ "Name": name })).await;
    set_due_instant(engine, SystemTime::now());
    // The owed finish is materialised by the next call, whoever makes it.
    ok(router, "GetCrawler", &json!({ "Name": name })).await;
}

/// One `CrawlsFilter` of a request.
fn filter(field: &str, operator: &str, value: &str) -> Value {
    json!({ "FieldName": field, "FilterOperator": operator, "FieldValue": value })
}

/// Walk every page the listing hands out for one page size, following its own token, and
/// answer the crawl ids in the order they were served.
async fn walk_crawls(
    router: &MultiServiceRouter,
    case: &str,
    crawler: &str,
    page_size: Option<i32>,
    ceiling: usize,
) -> Vec<String> {
    let mut served: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = ok(
            router,
            "ListCrawls",
            &crawls_request(crawler, Value::Null, page_size, token.as_deref()),
        )
        .await;
        let ids = crawl_ids(&page);
        let bound = page_size
            .map(|size| usize::try_from(size).unwrap_or(MAX_CRAWLS_PAGE).min(MAX_CRAWLS_PAGE))
            .unwrap_or(DEFAULT_CRAWLS_PAGE);
        assert!(
            ids.len() <= bound,
            "case {case:?}: a page must not exceed {bound} crawls, got {} of them",
            ids.len()
        );
        served.extend(ids);
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            break;
        }
        assert!(
            served.len() <= ceiling,
            "case {case:?}: the walk must terminate, got {served:?} for {ceiling} crawls"
        );
    }
    served
}

/// Every crawl the crawler has run is reported, newest first, each from its own record — and
/// a crawler nothing has crawled reports an empty history rather than an absent one.
///
/// The rows differ along how many crawls the crawler has run (none, which is the state a
/// freshly created crawler is in, then one, two and three), and the assertions after the walk
/// read the members each crawl produced: a run summary whose counts are the ones THAT crawl's
/// writes made, which is why the first crawl of the data created a table and a partition and
/// the later ones re-wrote the table and found no new partition.
#[tokio::test]
async fn the_crawls_a_crawler_ran_are_reported_newest_first() {
    let (router, engine) = emulator_with_s3();
    stage(
        &router,
        "crawl-history",
        &[("data/year=2024/a.csv", "id\n1\n")],
    )
    .await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "historied",
        json!({ "Path": "s3://crawl-history/data/" }),
        json!({}),
    )
    .await;
    given_crawler(
        &router,
        "idle",
        json!({ "Path": "s3://crawl-history/data/" }),
        json!({}),
    )
    .await;

    let mut reported_before: Vec<String> = Vec::new();
    for crawls in 0..=3usize {
        if crawls > 0 {
            crawl_to_the_end(&router, &engine, "historied").await;
        }
        let body = listed_crawls(&router, "historied").await;
        let ids = crawl_ids(&body);
        assert_eq!(
            ids.len(),
            crawls,
            "case {crawls} crawl(s) run: expected that many crawls reported, got {body}"
        );
        assert!(
            body.get("NextToken").is_none(),
            "case {crawls} crawl(s) run: one page holds the whole history, so it carries no \
             token, got {body}"
        );
        // Newest first: the crawl just run is served ahead of the ones already reported, and
        // each of those keeps its place.
        assert_eq!(
            ids.iter().skip(1).cloned().collect::<Vec<String>>(),
            reported_before,
            "case {crawls} crawl(s) run: expected the crawls already reported behind the \
             newest, got {ids:?}"
        );
        let starts: Vec<f64> = crawls_of(&body)
            .iter()
            .map(|crawl| {
                crawl_instant(crawl, "StartTime")
                    .unwrap_or_else(|| panic!("every crawl reports when it started, got {crawl}"))
            })
            .collect();
        assert!(
            starts.windows(2).all(|pair| pair[0] >= pair[1]),
            "case {crawls} crawl(s) run: expected the start instants in descending order, got \
             {starts:?}"
        );
        reported_before = ids;
    }

    let body = listed_crawls(&router, "historied").await;
    let listed = crawls_of(&body);
    let ids = crawl_ids(&body);
    let distinct: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "case \"three crawls\": every crawl carries an id of its own, got {ids:?}"
    );
    for crawl in &listed {
        let case = crawl_member(crawl, "CrawlId").unwrap_or_default();
        assert_eq!(
            crawl_member(crawl, "State").as_deref(),
            Some(CRAWLER_HISTORY_STATE_COMPLETED),
            "case {case:?}: a crawl that ran to its end COMPLETED, got {crawl}"
        );
        let (start, end) = (
            crawl_instant(crawl, "StartTime"),
            crawl_instant(crawl, "EndTime"),
        );
        assert!(
            matches!((start, end), (Some(start), Some(end)) if end >= start),
            "case {case:?}: a finished crawl ran from its start to its end, got {crawl}"
        );
        assert_eq!(
            crawl.get("DPUHour").and_then(Value::as_f64).unwrap_or(0.0),
            0.0,
            "case {case:?}: no data processing unit was provisioned for the crawl, got {crawl}"
        );
        assert!(
            crawl.get("ErrorMessage").is_none(),
            "case {case:?}: a crawl that failed at nothing carries no error message, got {crawl}"
        );
        // The CloudWatch Logs stream a crawl writes its log to: nothing here writes one, so
        // naming one would name a record no log read can serve.
        for absent in ["LogGroup", "LogStream", "MessagePrefix"] {
            assert!(
                crawl.get(absent).is_none(),
                "case {case:?}: no log stream is written, so {absent} is absent, got {crawl}"
            );
        }
    }

    // The summary is what THAT crawl's writes did: the first crawl of the data created the
    // table it found and the partition below it, and a later crawl of the same data re-wrote
    // the table and found no new partition.
    let oldest = crawl_summary(&listed[listed.len() - 1]);
    assert_eq!(
        (
            summarised(&oldest, "TablesCreated"),
            summarised(&oldest, "TablesUpdated"),
            summarised(&oldest, "TablesDeleted"),
            summarised(&oldest, "PartitionsCreated"),
        ),
        (1, 0, 0, 1),
        "case \"the first crawl of the data\": expected the table and the partition it wrote, \
         got {oldest}"
    );
    let newest = crawl_summary(&listed[0]);
    assert_eq!(
        (
            summarised(&newest, "TablesCreated"),
            summarised(&newest, "TablesUpdated"),
            summarised(&newest, "TablesDeleted"),
            summarised(&newest, "PartitionsCreated"),
        ),
        (0, 1, 0, 0),
        "case \"a later crawl of the same data\": expected the table re-written and no new \
         partition, got {newest}"
    );

    // The newest crawl of the listing is the crawl `GetCrawler` reports as the crawler's
    // last: one record, read two ways.
    let crawler = crawler_of(&router, "historied").await;
    assert_eq!(
        crawler
            .pointer("/LastCrawl/StartTime")
            .and_then(Value::as_f64),
        crawl_instant(&listed[0], "StartTime"),
        "case \"the newest crawl\": expected the crawl GetCrawler reports as the last one, got \
         {crawler}"
    );

    // A read does not change what a later caller sees, and the listing is scoped to the
    // crawler addressed: the sibling crawler has run nothing.
    let again = listed_crawls(&router, "historied").await;
    assert_eq!(
        again, body,
        "case \"read twice\": ListCrawls must be repeatable, got {again}"
    );
    let idle = listed_crawls(&router, "idle").await;
    assert!(
        crawls_of(&idle).is_empty(),
        "case \"another crawler\": a crawler that has run nothing reports an empty history, got \
         {idle}"
    );
}

/// Each crawl is reported in the state it stands in: the one in flight is RUNNING and has
/// neither ended nor summarised anything, the same record COMPLETED once the finish the clock
/// owed it fell due — under the same id, because the record moves state rather than being
/// replaced — and a crawl that could not be carried out is FAILED carrying its reason.
///
/// The rows are the states a crawl of this emulator reaches, of the modelled
/// `CrawlerHistoryState` (`RUNNING | COMPLETED | FAILED | STOPPED`, from
/// `smithy_query(query="shape",shape="CrawlerHistoryState")`); `STOPPED` is the state a crawl
/// somebody stopped stands in, which the operation that stops one owns.
#[tokio::test]
async fn a_crawl_is_reported_in_the_state_it_stands_in() {
    let (router, engine) = emulator_with_s3();
    stage(&router, "crawl-states", &[("data/a.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "in-flight",
        json!({ "Path": "s3://crawl-states/data/" }),
        json!({}),
    )
    .await;
    // A crawler that names no database has nowhere to write what it finds, so its crawl is
    // carried out and fails.
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "failing",
            "Role": ROLE,
            "Targets": { "S3Targets": [{ "Path": "s3://crawl-states/data/" }] },
        }),
    )
    .await;

    ok(&router, "StartCrawler", &json!({ "Name": "in-flight" })).await;
    // Held open, so the transient window is observable without sleeping for it.
    set_due_instant(&engine, far_future());
    let before = crawler_of(&router, "in-flight").await;

    let running = crawls_of(&listed_crawls(&router, "in-flight").await);
    assert_eq!(
        running.len(),
        1,
        "case \"in flight\": the crawl that started is reported, got {running:?}"
    );
    assert_eq!(
        crawl_member(&running[0], "State").as_deref(),
        Some(CRAWLER_HISTORY_STATE_RUNNING),
        "case \"in flight\": the crawl is still under way, got {}",
        running[0]
    );
    assert!(
        crawl_instant(&running[0], "StartTime").is_some(),
        "case \"in flight\": a crawl under way reports when it started, got {}",
        running[0]
    );
    assert!(
        running[0].get("EndTime").is_none(),
        "case \"in flight\": a crawl still running has not ended, got {}",
        running[0]
    );
    assert!(
        running[0].get("Summary").is_none(),
        "case \"in flight\": a crawl still running has published no summary, got {}",
        running[0]
    );

    // Listing the crawls of a crawl in flight does not advance it: its run state, the
    // definition's `Version`, the instant of its last write and the previous crawl's report
    // are all exactly what they were. (`CrawlElapsedTime` is measured from the instant the
    // read is served, so it is the one member a second read may differ in.)
    let after = crawler_of(&router, "in-flight").await;
    for member in ["State", "Version", "LastUpdated", "LastCrawl"] {
        assert_eq!(
            after.get(member),
            before.get(member),
            "case \"in flight\": the read must leave {member} untouched, expected {before}, got \
             {after}"
        );
    }

    // The finish the clock owed is materialised by the next call: the SAME crawl, now
    // completed.
    set_due_instant(&engine, SystemTime::now());
    let finished = crawls_of(&listed_crawls(&router, "in-flight").await);
    assert_eq!(
        crawl_member(&finished[0], "CrawlId"),
        crawl_member(&running[0], "CrawlId"),
        "case \"finished\": the crawl that finished is the one that was running, got {}",
        finished[0]
    );
    assert_eq!(
        crawl_member(&finished[0], "State").as_deref(),
        Some(CRAWLER_HISTORY_STATE_COMPLETED),
        "case \"finished\": the crawl ran to its end, got {}",
        finished[0]
    );
    assert!(
        crawl_instant(&finished[0], "EndTime").is_some(),
        "case \"finished\": a crawl that finished reports when it ended, got {}",
        finished[0]
    );
    assert_eq!(
        summarised(&crawl_summary(&finished[0]), "TablesCreated"),
        1,
        "case \"finished\": the summary states the table the crawl wrote, got {}",
        finished[0]
    );

    crawl_to_the_end(&router, &engine, "failing").await;
    let failed = crawls_of(&listed_crawls(&router, "failing").await);
    assert_eq!(
        crawl_member(&failed[0], "State").as_deref(),
        Some(CRAWLER_HISTORY_STATE_FAILED),
        "case \"failed\": a crawl that could not be carried out FAILED, got {}",
        failed[0]
    );
    assert_eq!(
        crawl_member(&failed[0], "ErrorMessage").as_deref(),
        Some(CRAWL_WITHOUT_TARGET_DATABASE),
        "case \"failed\": the crawl reports why it failed, got {}",
        failed[0]
    );
}

/// Seed the crawls of `crawler` directly: one FINISHED record per `(start, end)` row, in the
/// order given (newest first), plus a crawl IN FLIGHT when `in_flight` names the instant it
/// started. Answers every crawl id in the order a listing serves them.
///
/// Seeded rather than crawled for the reason the metrics suite seeds its crawlers: what the
/// rows below are about is which crawls a criterion admits and how a page is cut, not how a
/// crawl produces a record — and a filter on an instant is read against milliseconds this
/// test chose rather than ones a crawl happened to take.
fn seed_crawls(
    engine: &Arc<engine::InMemoryEngine>,
    crawler: &str,
    finished: &[(SystemTime, SystemTime)],
    in_flight: Option<SystemTime>,
) -> Vec<String> {
    use service_com_amazonaws_glue_awsglue::state::{CrawlCounts, CrawlRecord};

    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let entry = store
        .crawlers
        .get_mut(crawler)
        .unwrap_or_else(|| panic!("the suite created {crawler}"));
    entry.crawl_history.clear();
    let mut ids: Vec<String> = Vec::new();
    for (start, end) in finished {
        let mut record = CrawlRecord::succeeded(*start, CrawlCounts::default());
        record.finished_at = Some(*end);
        ids.push(record.crawl_id.clone());
        entry.crawl_history.push_back(record);
    }
    if let Some(start) = in_flight {
        let record = CrawlRecord::succeeded(start, CrawlCounts::default());
        let id = record.crawl_id.clone();
        entry.start_crawl(record, start, REGION);
        // The crawl in flight is newer than every crawl that has finished, which is the order
        // a listing serves them in.
        ids.insert(0, id);
    }
    ids
}

/// The epoch millisecond the seeded crawls below are stamped from, how far apart they are and
/// how long each ran: values this test chose, so a criterion on an instant is read against
/// known milliseconds.
const SEEDED_START_MS: u64 = 1_760_000_000_000;
/// See [`SEEDED_START_MS`].
const SEEDED_STEP_MS: u64 = 60_000;
/// See [`SEEDED_START_MS`].
const SEEDED_RAN_MS: u64 = 5_000;

/// `Filters` narrows the crawls to the ones every criterion admits: the crawls reported are
/// those the whole list of criteria selects, and an empty list narrows nothing.
///
/// The rows differ along what the rule generalises over: the field keyed on (every value of
/// the modelled `FieldName` — `CRAWL_ID`, `STATE`, `START_TIME`, `END_TIME`, `DPU_HOUR`, from
/// `smithy_query(query="shape",shape="FieldName")`), the comparator (every value of
/// `FilterOperator` — `EQ`, `NE`, `GT`, `GE`, `LT`, `LE`), the kind the field is compared as
/// (text or number), how many criteria are stated at once, and whether the crawl carries a
/// value for the field at all.
#[tokio::test]
async fn filters_narrow_the_crawls_to_what_every_criterion_admits() {
    let (router, engine) = emulator_with_s3();
    stage(&router, "crawl-filters", &[("data/a.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "filtered",
        json!({ "Path": "s3://crawl-filters/data/" }),
        json!({}),
    )
    .await;

    // Three finished crawls a minute apart, newest first, and one still in flight.
    let finished: Vec<(SystemTime, SystemTime)> = (0..3)
        .map(|index| {
            let start = SystemTime::UNIX_EPOCH
                + Duration::from_millis(SEEDED_START_MS - index * SEEDED_STEP_MS);
            (start, start + Duration::from_millis(SEEDED_RAN_MS))
        })
        .collect();
    let ids = seed_crawls(&engine, "filtered", &finished, Some(SystemTime::now()));
    let (running, newest, middle, oldest) = (
        ids[0].clone(),
        ids[1].clone(),
        ids[2].clone(),
        ids[3].clone(),
    );
    // The crawl in flight is held in its transient window, so the rows read a store that
    // holds one crawl of every state.
    set_due_instant(&engine, far_future());

    let middle_start = (SEEDED_START_MS - SEEDED_STEP_MS).to_string();
    let newest_end = (SEEDED_START_MS + SEEDED_RAN_MS).to_string();
    let everything = vec![
        running.clone(),
        newest.clone(),
        middle.clone(),
        oldest.clone(),
    ];

    // (case, the Filters sent, the crawl ids that must be reported, in order)
    let rows: Vec<(&str, Value, Vec<String>)> = vec![
        ("no Filters member", Value::Null, everything.clone()),
        ("an empty filter list", json!([]), everything.clone()),
        (
            "one crawl by its id",
            json!([filter("CRAWL_ID", "EQ", &middle)]),
            vec![middle.clone()],
        ),
        (
            "every crawl but one id",
            json!([filter("CRAWL_ID", "NE", &middle)]),
            vec![running.clone(), newest.clone(), oldest.clone()],
        ),
        (
            "an id no crawl carries",
            json!([filter("CRAWL_ID", "EQ", "not-a-crawl-of-this-crawler")]),
            vec![],
        ),
        (
            "the crawls that completed",
            json!([filter("STATE", "EQ", CRAWLER_HISTORY_STATE_COMPLETED)]),
            vec![newest.clone(), middle.clone(), oldest.clone()],
        ),
        (
            "the crawl still running",
            json!([filter("STATE", "EQ", CRAWLER_HISTORY_STATE_RUNNING)]),
            vec![running.clone()],
        ),
        (
            "the crawls that are not running",
            json!([filter("STATE", "NE", CRAWLER_HISTORY_STATE_RUNNING)]),
            vec![newest.clone(), middle.clone(), oldest.clone()],
        ),
        (
            "a state no crawl of this crawler stands in",
            json!([filter("STATE", "EQ", "STOPPED")]),
            vec![],
        ),
        (
            "every crawl started after the epoch",
            json!([filter("START_TIME", "GT", "0")]),
            everything.clone(),
        ),
        (
            // Compared as a NUMBER and not as text: these instants are 13 digits, so a text
            // comparison would order them below this 12-digit value.
            "a start compared against fewer digits",
            json!([filter("START_TIME", "GT", "999999999999")]),
            everything.clone(),
        ),
        (
            "the crawls started at or after the middle one",
            json!([filter("START_TIME", "GE", &middle_start)]),
            vec![running.clone(), newest.clone(), middle.clone()],
        ),
        (
            "the crawls started before the middle one",
            json!([filter("START_TIME", "LT", &middle_start)]),
            vec![oldest.clone()],
        ),
        (
            // A crawl in flight has not ended, so it carries no value of this field to
            // compare.
            "the crawls that have ended",
            json!([filter("END_TIME", "GT", "0")]),
            vec![newest.clone(), middle.clone(), oldest.clone()],
        ),
        (
            "the crawls that ended at or before the newest one",
            json!([filter("END_TIME", "LE", &newest_end)]),
            vec![newest.clone(), middle.clone(), oldest.clone()],
        ),
        (
            "the crawls that ended after the newest one",
            json!([filter("END_TIME", "GT", &newest_end)]),
            vec![],
        ),
        (
            "no data processing unit hour was used",
            json!([filter("DPU_HOUR", "EQ", "0")]),
            everything.clone(),
        ),
        (
            "a fractional DPU hour nothing reached",
            json!([filter("DPU_HOUR", "GT", "0.5")]),
            vec![],
        ),
        (
            "a fractional DPU hour every crawl is below",
            json!([filter("DPU_HOUR", "LE", "0.5")]),
            everything.clone(),
        ),
        (
            "two criteria, both of which must hold",
            json!([
                filter("STATE", "EQ", CRAWLER_HISTORY_STATE_COMPLETED),
                filter("CRAWL_ID", "NE", &newest),
            ]),
            vec![middle.clone(), oldest.clone()],
        ),
        (
            "two criteria no crawl satisfies at once",
            json!([
                filter("STATE", "EQ", CRAWLER_HISTORY_STATE_RUNNING),
                filter("END_TIME", "GT", "0"),
            ]),
            vec![],
        ),
    ];

    for (case, filters, expected) in rows {
        let body = ok(
            &router,
            "ListCrawls",
            &crawls_request(
                "filtered",
                filters,
                Some(i32::try_from(MAX_CRAWLS_PAGE).unwrap_or(1)),
                None,
            ),
        )
        .await;
        let served = crawl_ids(&body);
        assert_eq!(
            served, expected,
            "case {case:?}: expected the crawls {expected:?}, got {served:?} in {body}"
        );
    }
}

/// The crawls of one crawler are served one bounded page at a time, in the listing's own
/// order, with no crawl served twice and none left out — and the store keeps the records of
/// the last [`MAX_CRAWL_HISTORY`] crawls, dropping the oldest first.
///
/// The rows differ along the page size asked for: absent (the documented default), each
/// published bound of the documented range and sizes between them, and one above that range
/// which is served as pages of the documented maximum rather than refused.
#[tokio::test]
async fn the_crawl_history_is_walked_one_bounded_page_at_a_time() {
    let (router, engine) = emulator_with_s3();
    stage(&router, "crawl-pages", &[("data/a.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "paged",
        json!({ "Path": "s3://crawl-pages/data/" }),
        json!({}),
    )
    .await;

    // Two crawls past the bound, so the bound is read on the path a crawl really takes and
    // the walk below covers a full history.
    let mut minted: Vec<String> = Vec::new();
    for _ in 0..MAX_CRAWL_HISTORY + 2 {
        crawl_to_the_end(&router, &engine, "paged").await;
        let newest = crawl_ids(&listed_crawls(&router, "paged").await);
        minted.push(
            newest
                .first()
                .cloned()
                .unwrap_or_else(|| panic!("the crawl that just ran is reported")),
        );
    }

    let whole = crawl_ids(&listed_crawls(&router, "paged").await);
    let kept: Vec<String> = minted
        .iter()
        .rev()
        .take(MAX_CRAWL_HISTORY)
        .cloned()
        .collect();
    assert_eq!(
        whole, kept,
        "case \"past the bound\": expected the newest {MAX_CRAWL_HISTORY} crawls, newest first, \
         got {whole:?}"
    );
    for dropped in minted.iter().take(minted.len() - MAX_CRAWL_HISTORY) {
        assert!(
            !whole.contains(dropped),
            "case \"past the bound\": the oldest crawl {dropped:?} is dropped first, got {whole:?}"
        );
    }

    // (case, the page size asked for)
    let rows: &[(&str, Option<i32>)] = &[
        ("no page size at all", None),
        ("the smallest page the documented range allows", Some(1)),
        ("a page smaller than the history", Some(3)),
        ("a page one short of the history", Some(19)),
        ("a page the size of the history", Some(20)),
        (
            "the largest page the documentation allows",
            Some(i32::try_from(MAX_CRAWLS_PAGE).unwrap_or(1)),
        ),
        (
            "a page size above the documented maximum",
            Some(MAX_PAGE_SIZE_MEMBER),
        ),
    ];
    for (case, page_size) in rows {
        let served = walk_crawls(&router, case, "paged", *page_size, MAX_CRAWL_HISTORY).await;
        assert_eq!(
            served, whole,
            "case {case:?}: expected the whole history in one order, got {served:?}"
        );
    }

    // A cursor past the end of the listing is not an error: the walk resumes past every crawl,
    // so the page is empty and final. A cursor before all of them resumes at the first.
    for (case, token, expected) in [
        ("a cursor past every crawl", "0", Vec::new()),
        ("a cursor before every crawl", "99999999999999999999:", whole.clone()),
    ] {
        let page = ok(
            &router,
            "ListCrawls",
            &crawls_request(
                "paged",
                Value::Null,
                Some(i32::try_from(MAX_CRAWLS_PAGE).unwrap_or(1)),
                Some(token),
            ),
        )
        .await;
        assert_eq!(
            crawl_ids(&page),
            expected,
            "case {case:?}: expected {expected:?}, got {page}"
        );
    }
}

/// A crawl listing the service cannot serve is refused and changes nothing: a name no crawler
/// in the account has is the operation's own miss, a `Filters` entry that is no comparison on
/// a field of a crawl is its own input refusal, and a `MaxResults` outside the member's
/// modelled range is refused at the request boundary.
#[tokio::test]
async fn a_crawl_listing_the_service_cannot_serve_is_refused() {
    let (router, engine) = emulator_with_s3();
    stage(&router, "crawl-refusals", &[("data/a.csv", "id\n1\n")]).await;
    given_database(&router, DATABASE).await;
    given_crawler(
        &router,
        "listed",
        json!({ "Path": "s3://crawl-refusals/data/" }),
        json!({}),
    )
    .await;
    crawl_to_the_end(&router, &engine, "listed").await;
    let before = listed_crawls(&router, "listed").await;

    // The rows differ along the spelling of the absent name: one nothing created, and the
    // stored crawler's name in another casing — a crawler is addressed by name exactly as it
    // was created.
    for (case, name) in [
        ("a name nothing created", "absent"),
        ("the stored name in another casing", "LISTED"),
    ] {
        let (status, parsed) = call(
            &router,
            "ListCrawls",
            &crawls_request(name, Value::Null, None, None),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with("EntityNotFoundException"),
            "case {case:?}: expected EntityNotFoundException, got {parsed}"
        );
        assert_eq!(
            error_message(&parsed),
            format!("Crawler entry with name {name} does not exist"),
            "case {case:?}: expected the miss to name the crawler, got {parsed}"
        );
    }

    // (case, the Filters sent) — each states no comparison on a field of a crawl.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a filter stating no field",
            json!([{ "FilterOperator": "EQ", "FieldValue": "COMPLETED" }]),
        ),
        (
            "a filter stating no comparator",
            json!([{ "FieldName": "STATE", "FieldValue": "COMPLETED" }]),
        ),
        (
            "a filter stating no value",
            json!([{ "FieldName": "STATE", "FilterOperator": "EQ" }]),
        ),
        (
            "a start instant that is no number",
            json!([filter("START_TIME", "GT", "yesterday")]),
        ),
        (
            "a DPU hour that is no number",
            json!([filter("DPU_HOUR", "GE", "a few")]),
        ),
        (
            "one criterion the listing can serve beside one it cannot",
            json!([
                filter("STATE", "EQ", CRAWLER_HISTORY_STATE_COMPLETED),
                filter("END_TIME", "LT", "the day before"),
            ]),
        ),
    ];
    for (case, filters) in rows {
        let (status, parsed) = call(
            &router,
            "ListCrawls",
            &crawls_request("listed", filters, None, None),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {parsed}"
        );
    }

    // A field name or a comparator the model does not declare, and a page size outside the
    // member's modelled range, break a modelled constraint: each is refused at the request
    // boundary, before the listing is reached.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a field no crawl has",
            crawls_request(
                "listed",
                json!([filter("CRAWLER_NAME", "EQ", "listed")]),
                None,
                None,
            ),
        ),
        (
            "a comparator the model does not declare",
            crawls_request(
                "listed",
                json!([filter("STATE", "LIKE", CRAWLER_HISTORY_STATE_COMPLETED)]),
                None,
                None,
            ),
        ),
        (
            "a page size below the minimum",
            crawls_request("listed", Value::Null, Some(0), None),
        ),
        (
            "a page size above the maximum",
            crawls_request("listed", Value::Null, Some(MAX_PAGE_SIZE_MEMBER + 1), None),
        ),
    ];
    for (case, body) in rows {
        let (status, parsed) = call(&router, "ListCrawls", &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {parsed}"
        );
        assert!(
            !error_code(&parsed).is_empty(),
            "case {case:?}: the refusal must name an error, got {parsed}"
        );
    }

    // The refusals changed nothing: the crawl the crawler ran is still reported, exactly as
    // it was.
    let after = listed_crawls(&router, "listed").await;
    assert_eq!(
        after, before,
        "case \"after the refusals\": expected the listing to be untouched, got {after}"
    );
}

// ---------------------------------------------------------------------------
// A crawl of an Amazon DynamoDB target
// ---------------------------------------------------------------------------
//
// `DynamoDBTarget$Path` is "the name of the DynamoDB table to crawl", so the dataset behind
// such a target is the ITEMS of that table, read through DynamoDB's own data plane: the
// crawl writes one catalog table whose columns are the attributes those items carry, typed by
// what each attribute holds, with the table's own key attributes leading them and every other
// attribute after them by name. A DynamoDB table is no path, so the dataset has no partition
// keys and no partitions.

/// An emulator with Glue AND Amazon DynamoDB mounted on one shared engine, plus the engine
/// itself so a test can move the due instant a crawl's finish falls at.
///
/// DynamoDB is mounted because a crawl of a `DynamoDBTarget` READS that table: it reaches it
/// over the emulator's own in-process callback, so the items a test writes with `PutItem` are
/// the items the crawl classifies.
fn emulator_with_dynamodb() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// An Amazon DynamoDB client over this emulator's own in-process callback — the seam a crawl
/// of a `DynamoDBTarget` reads through, so what a test writes here is what the crawl reads.
fn dynamodb(router: &MultiServiceRouter) -> aws_sdk_dynamodb::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_dynamodb::Config::builder()
        .behavior_version(aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(aws_sdk_dynamodb::config::Region::new(REGION))
        .endpoint_url(format!("https://dynamodb.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_dynamodb::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_dynamodb::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_dynamodb::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_dynamodb::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_dynamodb::Client::from_conf(config)
}

/// How long a test waits for a DynamoDB table to leave `CREATING`.
///
/// Generous: the wait is for the CONDITION — the status the table reports — and never for a
/// fixed span, so a loaded host only loops once more. `DescribeTable` is the call an SDK
/// waiter polls, and it answers no sooner than the transition the clock owes the table.
const TABLE_READY_DEADLINE: Duration = Duration::from_secs(120);

/// Create the DynamoDB table `name` with the key schema `keys` — `(attribute, key type,
/// attribute type)` — and return once it reports `ACTIVE`, which is the only status its data
/// plane serves a write on.
async fn given_dynamodb_table(
    client: &aws_sdk_dynamodb::Client,
    name: &str,
    keys: &[(&str, KeyType, ScalarAttributeType)],
) {
    let mut request = client
        .create_table()
        .table_name(name)
        .billing_mode(BillingMode::PayPerRequest);
    for (attribute, key_type, attribute_type) in keys {
        request = request
            .key_schema(
                KeySchemaElement::builder()
                    .attribute_name(*attribute)
                    .key_type(key_type.clone())
                    .build()
                    .expect("the key schema element is complete"),
            )
            .attribute_definitions(
                AttributeDefinition::builder()
                    .attribute_name(*attribute)
                    .attribute_type(attribute_type.clone())
                    .build()
                    .expect("the attribute definition is complete"),
            );
    }
    request
        .send()
        .await
        .unwrap_or_else(|error| panic!("the table {name} must be created, got {error:?}"));

    let deadline = Instant::now() + TABLE_READY_DEADLINE;
    loop {
        let described = client
            .describe_table()
            .table_name(name)
            .send()
            .await
            .unwrap_or_else(|error| panic!("the table {name} must be described, got {error:?}"));
        let status = described
            .table()
            .and_then(|table| table.table_status())
            .map(|status| status.as_str().to_string())
            .unwrap_or_default();
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the table {name} must reach ACTIVE, last reported {status:?}"
        );
        ::tokio::task::yield_now().await;
    }
}

/// Write `items` into the DynamoDB table `name`, through that service's own data plane.
///
/// In BATCHES, because a row of the tables below stages more items than a crawl scans: a
/// batch write is the call DynamoDB serves a set of items with, so staging a large table costs
/// a request per [`ITEMS_PER_BATCH`] items rather than one per item.
async fn put_items(
    client: &aws_sdk_dynamodb::Client,
    name: &str,
    items: &[HashMap<String, AttributeValue>],
) {
    for batch in items.chunks(ITEMS_PER_BATCH) {
        let requests: Vec<WriteRequest> = batch
            .iter()
            .map(|item| {
                WriteRequest::builder()
                    .put_request(
                        PutRequest::builder()
                            .set_item(Some(item.clone()))
                            .build()
                            .expect("the put request is complete"),
                    )
                    .build()
            })
            .collect();
        let answered = client
            .batch_write_item()
            .request_items(name.to_string(), requests)
            .send()
            .await
            .unwrap_or_else(|error| panic!("the items must be stored in {name}, got {error:?}"));
        assert!(
            answered.unprocessed_items().is_none_or(|unprocessed| {
                unprocessed.values().all(|requests| requests.is_empty())
            }),
            "every item staged in {name} must be stored, got {answered:?}"
        );
    }
}

/// How many items one `BatchWriteItem` carries: "a single call can write up to 16 MB of data,
/// which can comprise as many as 25 put or delete requests"
/// (`BatchWriteItem`), so the staging above asks for the most a call serves.
const ITEMS_PER_BATCH: usize = 25;

/// One item from `(attribute, value)` pairs.
fn item(attributes: Vec<(&str, AttributeValue)>) -> HashMap<String, AttributeValue> {
    attributes
        .into_iter()
        .map(|(attribute, value)| (attribute.to_string(), value))
        .collect()
}

/// A number attribute, which DynamoDB carries as the text of the number.
fn number(value: &str) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

/// A string attribute.
fn text(value: &str) -> AttributeValue {
    AttributeValue::S(value.to_string())
}

/// Create one crawler whose only target is the DynamoDB table `target` describes.
async fn given_dynamodb_crawler(
    router: &MultiServiceRouter,
    name: &str,
    target: Value,
    extra: Value,
) {
    let mut body = json!({
        "Name": name,
        "Role": ROLE,
        "DatabaseName": DATABASE,
        "Targets": { "DynamoDBTargets": [target] },
    });
    if let Some(members) = extra.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    ok(router, "CreateCrawler", &body).await;
}

/// One row of the DynamoDB discovery table: the table staged, the items written into it, the
/// target crawled, and the catalog table the crawl must write for them.
struct ItemDiscovery {
    /// What the row is about.
    case: &'static str,
    /// The DynamoDB table the row's items live in.
    table: &'static str,
    /// Its key schema, as `(attribute, key type, attribute type)`.
    keys: Vec<(&'static str, KeyType, ScalarAttributeType)>,
    /// The items written into it.
    items: Vec<HashMap<String, AttributeValue>>,
    /// The `DynamoDBTarget` the crawler carries.
    target: Value,
    /// Whatever the create adds to the crawler beside its targets.
    extra: Value,
    /// The name of the catalog table the crawl must write.
    catalog_table: &'static str,
    /// The columns that table must declare, as `(name, type)`, in order.
    columns: Vec<(&'static str, &'static str)>,
    /// How many records the crawl must report having read.
    record_count: usize,
}

/// A crawl of a DynamoDB target writes one catalog table for the items of the table it names,
/// with the columns those items carry and no partitions at all.
///
/// The rows differ along what the rule generalises over: which attribute types the items
/// carry (text, numbers whole and fractional, booleans, binary, the three set types, a list,
/// a nested map, a null), how many key attributes the table has and which attributes they are,
/// whether two items type one attribute differently, the form of the table's name (lowercase
/// with a hyphen, mixed case), whether the target asks for the whole table or a sample
/// (`scanAll`), how many items sit behind it, and whether the crawler carries a `TablePrefix`.
#[tokio::test]
async fn a_crawl_of_a_dynamodb_target_writes_the_items_it_scanned() {
    let sampled: Vec<HashMap<String, AttributeValue>> = (0..SAMPLED_CRAWLED_ITEMS + 5)
        .map(|index| {
            item(vec![
                ("id", text(&format!("item-{index}"))),
                ("rank", number(&index.to_string())),
            ])
        })
        .collect();

    let rows: Vec<ItemDiscovery> = vec![
        ItemDiscovery {
            case: "a string partition key and scalar attributes",
            table: "ddb-orders",
            keys: vec![("order_id", KeyType::Hash, ScalarAttributeType::S)],
            items: vec![item(vec![
                ("order_id", text("o-1")),
                ("amount", number("10.5")),
                ("quantity", number("3")),
                ("paid", AttributeValue::Bool(true)),
            ])],
            target: json!({ "Path": "ddb-orders" }),
            extra: json!({}),
            // The catalog holds a crawled name the way it holds every crawled name:
            // lowercased, with what it does not hold a name in replaced by an underscore.
            catalog_table: "ddb_orders",
            // The key attribute leads, then the rest by name.
            columns: vec![
                ("order_id", "string"),
                ("amount", "double"),
                ("paid", "boolean"),
                ("quantity", "bigint"),
            ],
            record_count: 1,
        },
        ItemDiscovery {
            case: "a hash and a range key lead the columns in key order",
            table: "Sessions",
            keys: vec![
                ("user_id", KeyType::Hash, ScalarAttributeType::S),
                ("started_at", KeyType::Range, ScalarAttributeType::N),
            ],
            items: vec![
                item(vec![
                    ("user_id", text("u-1")),
                    ("started_at", number("1700000000")),
                    ("ip", text("10.0.0.1")),
                    (
                        "tags",
                        AttributeValue::Ss(vec!["beta".to_string(), "eu".to_string()]),
                    ),
                ]),
                item(vec![
                    ("user_id", text("u-2")),
                    ("started_at", number("1700000600")),
                    ("ip", text("10.0.0.2")),
                    ("tags", AttributeValue::Ss(vec!["beta".to_string()])),
                ]),
            ],
            target: json!({ "Path": "Sessions", "scanAll": true }),
            extra: json!({}),
            // The catalog folds a crawled name to lowercase, as it does every table name.
            catalog_table: "sessions",
            columns: vec![
                ("user_id", "string"),
                ("started_at", "bigint"),
                ("ip", "string"),
                ("tags", "array<string>"),
            ],
            record_count: 2,
        },
        ItemDiscovery {
            case: "the collection, binary and null attribute types",
            table: "ddb_shapes",
            keys: vec![("id", KeyType::Hash, ScalarAttributeType::S)],
            items: vec![item(vec![
                ("id", text("a")),
                ("blob", AttributeValue::B(Blob::new("payload".as_bytes()))),
                (
                    "blobs",
                    AttributeValue::Bs(vec![Blob::new("one".as_bytes())]),
                ),
                ("flags", AttributeValue::L(vec![AttributeValue::Bool(false)])),
                ("missing", AttributeValue::Null(true)),
                (
                    "numbers",
                    AttributeValue::Ns(vec!["1".to_string(), "2".to_string()]),
                ),
                (
                    "profile",
                    AttributeValue::M(
                        item(vec![("city", text("lisbon")), ("zip", number("1000"))]),
                    ),
                ),
            ])],
            target: json!({ "Path": "ddb_shapes" }),
            extra: json!({}),
            catalog_table: "ddb_shapes",
            columns: vec![
                ("id", "string"),
                ("blob", "binary"),
                ("blobs", "array<binary>"),
                ("flags", "array<boolean>"),
                ("missing", "string"),
                ("numbers", "array<bigint>"),
                ("profile", "struct<city:string,zip:bigint>"),
            ],
            record_count: 1,
        },
        ItemDiscovery {
            case: "an attribute two items type differently reads as text",
            table: "ddb_mixed",
            keys: vec![("id", KeyType::Hash, ScalarAttributeType::S)],
            items: vec![
                item(vec![("id", text("1")), ("value", number("5"))]),
                item(vec![("id", text("2")), ("value", text("five"))]),
            ],
            target: json!({ "Path": "ddb_mixed" }),
            extra: json!({}),
            catalog_table: "ddb_mixed",
            columns: vec![("id", "string"), ("value", "string")],
            record_count: 2,
        },
        ItemDiscovery {
            case: "a target asking to be sampled reads the sample and not the whole table",
            table: "ddb_sampled",
            keys: vec![("id", KeyType::Hash, ScalarAttributeType::S)],
            items: sampled,
            target: json!({ "Path": "ddb_sampled", "scanAll": false }),
            extra: json!({}),
            catalog_table: "ddb_sampled",
            columns: vec![("id", "string"), ("rank", "bigint")],
            record_count: SAMPLED_CRAWLED_ITEMS,
        },
        ItemDiscovery {
            case: "the crawler's TablePrefix names the table the crawl writes",
            table: "ddb_prefixed",
            keys: vec![("id", KeyType::Hash, ScalarAttributeType::S)],
            items: vec![item(vec![("id", text("p-1"))])],
            target: json!({ "Path": "ddb_prefixed" }),
            extra: json!({ "TablePrefix": "staging_" }),
            catalog_table: "staging_ddb_prefixed",
            columns: vec![("id", "string")],
            record_count: 1,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (router, engine) = emulator_with_dynamodb();
        let client = dynamodb(&router);
        given_dynamodb_table(&client, row.table, &row.keys).await;
        put_items(&client, row.table, &row.items).await;
        given_database(&router, DATABASE).await;
        given_dynamodb_crawler(&router, "crawler", row.target.clone(), row.extra.clone()).await;

        crawl(&router, &engine, "crawler").await;

        let crawler = crawler_of(&router, "crawler").await;
        assert_eq!(
            crawler.get("State").and_then(Value::as_str),
            Some(CRAWLER_STATE_READY),
            "case {case:?}: the finished crawl leaves the crawler ready, got {crawler}"
        );
        assert_eq!(
            crawler.pointer("/LastCrawl/Status").and_then(Value::as_str),
            Some(LAST_CRAWL_STATUS_SUCCEEDED),
            "case {case:?}: the crawl read its target, got {crawler}"
        );

        let table = table_of(&router, DATABASE, row.catalog_table)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the crawl must write the table {}",
                    row.catalog_table
                )
            });
        let expected_columns: Vec<(String, String)> = row
            .columns
            .iter()
            .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
            .collect();
        let actual_columns = columns_of(&table);
        assert_eq!(
            actual_columns, expected_columns,
            "case {case:?}: expected the columns {expected_columns:?}, got {actual_columns:?}"
        );
        assert_eq!(
            partition_keys_of(&table),
            Vec::<(String, String)>::new(),
            "case {case:?}: a DynamoDB table is one unpartitioned dataset, got {table}"
        );
        assert_eq!(
            stored_partitions(&engine, DATABASE, row.catalog_table),
            Vec::<Vec<String>>::new(),
            "case {case:?}: a DynamoDB table holds no partition, got {table}"
        );
        assert_eq!(
            parameter_of(&table, "classification").as_deref(),
            Some(CLASSIFICATION_DYNAMODB),
            "case {case:?}: expected the data classified as {CLASSIFICATION_DYNAMODB}, got {table}"
        );
        assert_eq!(
            parameter_of(&table, TYPE_OF_DATA_PARAMETER).as_deref(),
            Some(TYPE_OF_DATA_TABLE),
            "case {case:?}: the data sits in another store's table, got {table}"
        );
        assert_eq!(
            parameter_of(&table, UPDATED_BY_CRAWLER_PARAMETER).as_deref(),
            Some("crawler"),
            "case {case:?}: the table records the crawler that wrote it, got {table}"
        );
        assert_eq!(
            parameter_of(&table, RECORD_COUNT_PARAMETER).as_deref(),
            Some(row.record_count.to_string().as_str()),
            "case {case:?}: expected {} records read, got {table}",
            row.record_count
        );
        // The size and the average are of the records the crawl itself read: the average is
        // exactly the size over the count.
        let size: u64 = parameter_of(&table, SIZE_KEY_PARAMETER)
            .and_then(|size| size.parse().ok())
            .unwrap_or_else(|| panic!("case {case:?}: the table must record its size, got {table}"));
        assert!(
            size > 0,
            "case {case:?}: the items the crawl read hold bytes, got {size}"
        );
        assert_eq!(
            parameter_of(&table, AVERAGE_RECORD_SIZE_PARAMETER).as_deref(),
            Some(
                (size / u64::try_from(row.record_count).expect("the row's count fits"))
                    .to_string()
                    .as_str()
            ),
            "case {case:?}: the average record is the size over the count, got {table}"
        );
        // The data stays in the DynamoDB table, which is what the catalog table's location
        // names: the resource that addresses that table in the store that owns it. There is
        // no file layout to read it with, so neither a format nor a serde is reported.
        assert_eq!(
            table
                .pointer("/StorageDescriptor/Location")
                .and_then(Value::as_str),
            Some(format!("arn:aws:dynamodb:{REGION}:{ACCOUNT_ID}:table/{}", row.table).as_str()),
            "case {case:?}: the location is the table the items sit in, got {table}"
        );
        for member in ["InputFormat", "OutputFormat", "SerdeInfo"] {
            assert!(
                table
                    .pointer(&format!("/StorageDescriptor/{member}"))
                    .is_none(),
                "case {case:?}: a table-shaped dataset reports no {member}, got {table}"
            );
        }
    }
}

/// One row of the table below: what the case is, the table the target names, how many items
/// the store holds it with (`None` for a table the store does not hold at all), and the status
/// and reason the crawl of it must report.
type UnreadableTarget = (&'static str, &'static str, Option<usize>, &'static str, Option<String>);

/// A crawl of a DynamoDB target whose table the store does not serve is a crawl that FAILED,
/// reporting why and writing no table; a target holding more items than one crawl scans is a
/// crawl that failed too, reporting THAT instead, because the two are different conditions —
/// and a target whose table holds no item is a crawl that succeeded and wrote none, because
/// there is nothing there to read a column off.
#[tokio::test]
async fn a_dynamodb_target_nothing_can_be_read_from_writes_no_table() {
    let rows: Vec<UnreadableTarget> = vec![
        (
            "a table the store does not hold",
            "ddb_absent",
            None,
            LAST_CRAWL_STATUS_FAILED,
            Some(crawl_target_unreadable("ddb_absent")),
        ),
        (
            "a table holding no item",
            "ddb_empty",
            Some(0),
            LAST_CRAWL_STATUS_SUCCEEDED,
            None,
        ),
        (
            "a table holding one item more than a crawl scans",
            "ddb_past_the_ceiling",
            Some(MAX_CRAWLED_ITEMS + 1),
            LAST_CRAWL_STATUS_FAILED,
            Some(crawl_target_past_the_item_ceiling("ddb_past_the_ceiling")),
        ),
    ];

    for (case, table, held, status, reason) in rows {
        let (router, engine) = emulator_with_dynamodb();
        if let Some(count) = held {
            let client = dynamodb(&router);
            given_dynamodb_table(
                &client,
                table,
                &[("id", KeyType::Hash, ScalarAttributeType::S)],
            )
            .await;
            let items: Vec<HashMap<String, AttributeValue>> = (0..count)
                .map(|index| item(vec![("id", text(&format!("item-{index}")))]))
                .collect();
            put_items(&client, table, &items).await;
        }
        given_database(&router, DATABASE).await;
        given_dynamodb_crawler(&router, "crawler", json!({ "Path": table }), json!({})).await;

        crawl(&router, &engine, "crawler").await;

        let crawler = crawler_of(&router, "crawler").await;
        assert_eq!(
            crawler.get("State").and_then(Value::as_str),
            Some(CRAWLER_STATE_READY),
            "case {case:?}: the finished crawl leaves the crawler ready, got {crawler}"
        );
        assert_eq!(
            crawler.pointer("/LastCrawl/Status").and_then(Value::as_str),
            Some(status),
            "case {case:?}: expected the crawl to report {status}, got {crawler}"
        );
        assert_eq!(
            crawler
                .pointer("/LastCrawl/ErrorMessage")
                .and_then(Value::as_str)
                .map(str::to_string),
            reason,
            "case {case:?}: expected the crawl to report {reason:?}, got {crawler}"
        );
        assert!(
            table_of(&router, DATABASE, table).await.is_none(),
            "case {case:?}: the crawl wrote no table for data it did not read"
        );
    }
}
