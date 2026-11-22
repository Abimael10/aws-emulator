// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a crawl whose role may not READ its target leaves in the Data
//! Catalog: nothing of its own, beside the tables a permitted crawler of the same prefix wrote.
//!
//! The rule under test: a crawl reads the data behind an Amazon S3 target AS the crawler's
//! `Role` — "the IAM role or Amazon Resource Name (ARN) of an IAM role used by the new crawler
//! to access customer resources" (`CreateCrawler$Role`) — so the listing of the bucket
//! (`s3:ListBucket`) and the read of the object a schema is inferred from (`s3:GetObject`) are
//! both decided against the documents the account holds on that role together with the bucket's
//! own policy. IAM answers by default-deny, so a role the account's records can be READ for
//! whose documents allow neither call reads nothing: the crawl catalogues NO table under that
//! crawler's `TablePrefix` and leaves the database exactly as it was, while the crawl itself is
//! carried out — it reached every target it was permitted — so the crawler comes to rest at
//! `READY` with `LastCrawl.Status` `SUCCEEDED` and no `ErrorMessage`. An otherwise identical
//! crawler whose role IS granted the read publishes its own table from the same data.
//!
//! The rows differ along what the unpermitted role carries, which is what the rule generalises
//! over: no document at all, a document granting another service's action, a document granting
//! the same read on another bucket, and a document granting the read beside an explicit deny of
//! the object read. Every row drives BOTH crawlers over one prefix in ONE database, so what the
//! unpermitted crawl left behind is read beside what the permitted one wrote. The last test
//! takes the rule down one level: a grant narrowed to one CHILD prefix of the include path
//! leaves the other prefix out of the table the crawl folds.
//!
//! Glue, Amazon S3 and IAM are mounted on ONE shared engine and driven over the wire, so the
//! documents the decision is made against are the records IAM holds.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::TABLE_ENTITY_NOT_FOUND;
use service_com_amazonaws_glue_awsglue::state::{
    CRAWLER_STATE_READY, LAST_CRAWL_STATUS_SUCCEEDED,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState, InlinePolicy,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::crawler_runs::{already_past, set_due_instant};
use crate::wire::{call, error_message, ok, s3_client};

/// The prefix both crawlers of a row are pointed at.
const TARGET_PREFIX: &str = "rows";

/// The object the dataset's schema is read from, and the bytes it holds.
const OBJECT_KEY: &str = "rows/people.csv";
/// The data staged there.
const OBJECT_BODY: &str = "id,name\n1,alpha\n2,beta\n";

/// The columns a crawl that READ that object publishes, as `(name, type)`.
const EXPECTED_COLUMNS: [(&str, &str); 2] = [("id", "bigint"), ("name", "string")];

/// The `TablePrefix` of the crawler whose role may not read the data.
const DENIED_PREFIX: &str = "denied_";

/// The `TablePrefix` of the crawler whose role may.
const CONTROL_PREFIX: &str = "control_";

/// An emulator with Glue, Amazon S3 and IAM on ONE shared engine, plus that engine.
fn emulator_with_s3_and_iam() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// Create `bucket` through Amazon S3's own API and put the row's object into it.
async fn stage(router: &MultiServiceRouter, bucket: &str) {
    put_objects(router, bucket, &[(OBJECT_KEY, OBJECT_BODY)]).await;
}

/// Create `bucket` and put every `(key, body)` into it.
async fn put_objects(router: &MultiServiceRouter, bucket: &str, objects: &[(&str, &str)]) {
    let client = s3_client(router);
    client
        .create_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {bucket} must be created, got {error:?}"));
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

/// Record `document` as an inline policy of `role_name` — the record `PutRolePolicy` writes.
fn put_role_policy(engine: &Arc<engine::InMemoryEngine>, role_name: &str, document: &str) {
    let state = engine.state::<IamState>();
    let mut store = state.lock();
    store
        .role_inline_policies
        .entry(name_key(role_name))
        .or_default()
        .insert(
            name_key("crawl-read"),
            InlinePolicy {
                policy_name: "crawl-read".to_string(),
                policy_document: document.to_string(),
            },
        );
}

/// A policy document holding `statements`.
fn document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// One `Allow` statement over `actions` and `resources`, as a whole document.
fn grant(actions: Value, resources: Value) -> String {
    document(json!([{ "Effect": "Allow", "Action": actions, "Resource": resources }]))
}

/// The document that grants the whole read of `bucket`: the listing on the bucket and the
/// object read on everything under it.
fn read_grant(bucket: &str) -> String {
    grant(
        json!(["s3:ListBucket", "s3:GetObject"]),
        json!([
            format!("arn:aws:s3:::{bucket}"),
            format!("arn:aws:s3:::{bucket}/*"),
        ]),
    )
}

/// One row: what the crawler whose crawl must catalogue nothing carries.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The bucket the row's data lives in (no underscore: `CreateBucket` refuses one).
    bucket: &'static str,
    /// The inline document the unpermitted role carries, as a function of the bucket.
    denied: fn(&str) -> Option<String>,
}

/// The rows: every way the account's records can fail to permit the crawl's two calls.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role carrying no document at all",
            bucket: "crawl-denied-bare",
            denied: |_bucket| None,
        },
        Row {
            case: "a role granted another service's action only",
            bucket: "crawl-denied-elsewhere-service",
            denied: |_bucket| Some(grant(json!("glue:GetTables"), json!("*"))),
        },
        Row {
            case: "a role granted the same read on another bucket",
            bucket: "crawl-denied-other-bucket",
            denied: |_bucket| Some(read_grant("crawl-denied-somewhere-else")),
        },
        Row {
            case: "a role granted the read beside an explicit deny of the object read",
            bucket: "crawl-denied-explicitly",
            denied: |bucket| {
                Some(document(json!([
                    {
                        "Effect": "Allow",
                        "Action": ["s3:ListBucket", "s3:GetObject"],
                        "Resource": [
                            format!("arn:aws:s3:::{bucket}"),
                            format!("arn:aws:s3:::{bucket}/*"),
                        ],
                    },
                    {
                        "Effect": "Deny",
                        "Action": "s3:GetObject",
                        "Resource": format!("arn:aws:s3:::{bucket}/*"),
                    },
                ])))
            },
        },
    ]
}

/// Create one crawler over the row's prefix, running as `role`, writing into `database`.
async fn given_crawler(
    router: &MultiServiceRouter,
    name: &str,
    role: &str,
    database: &str,
    bucket: &str,
    table_prefix: &str,
) {
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": name,
            "Role": role,
            "DatabaseName": database,
            "TablePrefix": table_prefix,
            "Targets": { "S3Targets": [{ "Path": format!("s3://{bucket}/{TARGET_PREFIX}/") }] },
        }),
    )
    .await;
}

/// Start the crawl of `name` and make the finish the clock owes it due, so the next read
/// reports the crawl that finished.
async fn crawl(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>, name: &str) {
    ok(router, "StartCrawler", &json!({ "Name": name })).await;
    set_due_instant(engine, already_past());
}

/// The names of the tables a database holds, in the order `GetTables` reports them.
async fn table_names_of(router: &MultiServiceRouter, database: &str) -> Vec<String> {
    let body = ok(router, "GetTables", &json!({ "DatabaseName": database })).await;
    crate::wire::table_names(&body)
}

/// The `(name, type)` of every column one table declares.
fn columns_of(table: &Value) -> Vec<(String, String)> {
    table
        .pointer("/StorageDescriptor/Columns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .map(|column| {
                    (
                        column["Name"].as_str().unwrap_or_default().to_string(),
                        column["Type"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A crawl whose role the account's records do not permit to read its target catalogues
/// nothing, while an otherwise identical crawler whose role is granted the read publishes the
/// dataset from the same data.
#[tokio::test]
async fn a_crawl_its_role_may_not_read_catalogues_nothing() {
    for row in &rows() {
        let case = row.case;
        let (router, engine) = emulator_with_s3_and_iam();
        let database = format!("{}_db", row.bucket.replace('-', "_"));
        stage(&router, row.bucket).await;

        // Two roles the account holds, both trusted by Glue so either crawler may be written:
        // one carrying whatever the row gives it, the other granted the whole read.
        let denied_role = format!("{}-denied", row.bucket);
        let control_role = format!("{}-control", row.bucket);
        let denied_arn = seed_role(&engine, &denied_role, Some(glue_trust()));
        let control_arn = seed_role(&engine, &control_role, Some(glue_trust()));
        if let Some(document) = (row.denied)(row.bucket) {
            put_role_policy(&engine, &denied_role, &document);
        }
        put_role_policy(&engine, &control_role, &read_grant(row.bucket));

        ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": database } }),
        )
        .await;
        given_crawler(
            &router,
            "denied-crawler",
            &denied_arn,
            &database,
            row.bucket,
            DENIED_PREFIX,
        )
        .await;
        given_crawler(
            &router,
            "control-crawler",
            &control_arn,
            &database,
            row.bucket,
            CONTROL_PREFIX,
        )
        .await;

        crawl(&router, &engine, "denied-crawler").await;

        // The crawl was carried out: it reached every target it was permitted, so it comes to
        // rest having succeeded and reports nothing about the data it could not read.
        let reported = ok(&router, "GetCrawler", &json!({ "Name": "denied-crawler" })).await;
        assert_eq!(
            reported.pointer("/Crawler/State").and_then(Value::as_str),
            Some(CRAWLER_STATE_READY),
            "case {case:?}: the finished crawl leaves the crawler ready, got {reported}"
        );
        assert_eq!(
            reported
                .pointer("/Crawler/LastCrawl/Status")
                .and_then(Value::as_str),
            Some(LAST_CRAWL_STATUS_SUCCEEDED),
            "case {case:?}: the crawl reached every target it was permitted, got {reported}"
        );
        assert_eq!(
            reported.pointer("/Crawler/LastCrawl/ErrorMessage"),
            None,
            "case {case:?}: a crawl that failed at nothing carries no error message, got \
             {reported}"
        );

        // And the database is untouched: no table of this crawler's at all.
        let after_denied = table_names_of(&router, &database).await;
        assert!(
            after_denied.is_empty(),
            "case {case:?}: the unpermitted crawl must catalogue nothing, got {after_denied:?}"
        );
        let would_be = format!("{DENIED_PREFIX}{TARGET_PREFIX}");
        let (status, body) = call(
            &router,
            "GetTable",
            &json!({ "DatabaseName": database, "Name": would_be }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the would-be table {would_be} must not be served, got {status} with \
             {body}"
        );
        assert!(
            error_message(&body).contains(TABLE_ENTITY_NOT_FOUND),
            "case {case:?}: expected the table read to report {TABLE_ENTITY_NOT_FOUND:?}, got \
             {body}"
        );

        // The one thing that differs: the other crawler's role may read the same prefix, so it
        // publishes the dataset with the columns the data carries.
        crawl(&router, &engine, "control-crawler").await;
        let published = table_names_of(&router, &database).await;
        let expected_name = format!("{CONTROL_PREFIX}{TARGET_PREFIX}");
        assert_eq!(
            published,
            vec![expected_name.clone()],
            "case {case:?}: expected only the permitted crawler's table, got {published:?}"
        );
        let read = ok(
            &router,
            "GetTable",
            &json!({ "DatabaseName": database, "Name": expected_name }),
        )
        .await;
        let expected_columns: Vec<(String, String)> = EXPECTED_COLUMNS
            .iter()
            .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
            .collect();
        let actual_columns = columns_of(&read["Table"]);
        assert_eq!(
            actual_columns, expected_columns,
            "case {case:?}: expected the columns {expected_columns:?} read off the data, got \
             {actual_columns:?}"
        );
    }
}

/// The rule holds PER CHILD PREFIX of one include path, not only per target: where the role is
/// granted the object read under one child prefix and not the other, the table the crawl folds
/// describes only the prefix it read — the withheld prefix is no partition of it and no table of
/// its own.
#[tokio::test]
async fn a_prefix_the_role_may_not_read_is_no_partition_of_the_folded_table() {
    let case = "an object-read grant narrowed to one child prefix of the include path";
    let bucket = "crawl-denied-one-prefix";
    let database = "crawl_denied_one_prefix_db";
    let (router, engine) = emulator_with_s3_and_iam();
    // Two child prefixes carrying the SAME schema, so nothing but the grant can decide what the
    // crawl describes: similar schemas are one folded table of the include path.
    put_objects(
        &router,
        bucket,
        &[
            ("rows/part1/people.csv", OBJECT_BODY),
            ("rows/part2/people.csv", OBJECT_BODY),
        ],
    )
    .await;

    let role = "crawl-denied-one-prefix-role";
    let role_arn = seed_role(&engine, role, Some(glue_trust()));
    // The listing of the whole bucket, but the object read only under the first child prefix.
    put_role_policy(
        &engine,
        role,
        &document(json!([
            {
                "Effect": "Allow",
                "Action": "s3:ListBucket",
                "Resource": format!("arn:aws:s3:::{bucket}"),
            },
            {
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": format!("arn:aws:s3:::{bucket}/{TARGET_PREFIX}/part1/*"),
            },
        ])),
    );

    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": database } }),
    )
    .await;
    given_crawler(
        &router,
        "one-prefix-crawler",
        &role_arn,
        database,
        bucket,
        "",
    )
    .await;
    crawl(&router, &engine, "one-prefix-crawler").await;

    let published = table_names_of(&router, database).await;
    assert_eq!(
        published,
        vec![TARGET_PREFIX.to_string()],
        "case {case:?}: expected the one folded table of the include path, got {published:?}"
    );
    // The prefix the crawl read is a partition of that table; the one it was not permitted to
    // read is not.
    for (values, served) in [(vec!["part1"], true), (vec!["part2"], false)] {
        let (status, body) = call(
            &router,
            "GetPartition",
            &json!({
                "DatabaseName": database,
                "TableName": TARGET_PREFIX,
                "PartitionValues": values,
            }),
        )
        .await;
        assert_eq!(
            status == 200,
            served,
            "case {case:?}: expected the partition {values:?} to be served: {served}, got \
             {status} with {body}"
        );
    }
}
