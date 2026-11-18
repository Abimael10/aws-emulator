// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AUTHORIZATION of a crawl's reads: whether the account's own
//! records let the crawler's role read the data behind an Amazon S3 target.
//!
//! The rule under test: a crawl is performed BY Glue acting as the crawler's `Role`, so every
//! object-store call it makes is a call by that role — the listing of the target's bucket
//! (`s3:ListBucket`) and the read of the object a dataset's schema is inferred from
//! (`s3:GetObject`, on that object's own ARN). Each is decided, before it is made, against the
//! identity-based policies in force on the role together with the target bucket's own resource
//! policy, in IAM's order: an explicit deny from either side refuses, and within one account an
//! allow from either is enough. A call the records withhold is one the crawl does not make, so
//! the data behind that target is never read: the crawl writes NO table and no partition for
//! it and leaves the catalog as it was. The crawl is still carried out — it reached every
//! target it was permitted to read — so it comes to rest having SUCCEEDED, with the crawler
//! back at `READY` and nothing to report about what it could not read. A refusal has to be
//! grounded in a document the account HOLDS: a role carrying a grant this account cannot read,
//! and a crawl no document is in force over at all (a role with no policy of its own reading a
//! bucket with no policy of its own), withhold nothing — which is also why every other crawl
//! suite in this crate, which mounts no IAM, still crawls.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: what the
//! role's own documents say (nothing at all, a grant of both calls, a grant of one of them, a
//! grant written with wildcard actions, a grant scoped to another bucket, a grant scoped to
//! another prefix of the same bucket, an explicit deny, the AWS-managed Glue service-role
//! policy whose object grant reaches only `aws-glue-*` paths), what the bucket's own policy
//! says (none, a deny of every principal but another role, a grant to this role alone, a grant
//! of an action the crawl does not make, a deny of this role beside documents the account
//! cannot read), and whether anything is in force over the crawl at all.
//!
//! Glue, Amazon S3 and IAM are mounted on ONE shared engine and driven over the wire, so the
//! documents the decision is made against are the records IAM holds and the policy the
//! decision reads is the one `PutBucketPolicy` wrote.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::TABLE_ENTITY_NOT_FOUND;
use service_com_amazonaws_glue_awsglue::state::{
    CRAWLER_STATE_READY, LAST_CRAWL_STATUS_SUCCEEDED, UPDATED_BY_CRAWLER_PARAMETER,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{
    name_key, ComAmazonawsIamAwsidentitymanagementv20100508State as IamState, InlinePolicy,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role};
use crate::crawler_runs::{already_past, set_due_instant};
use crate::wire::{call, error_message, ok, s3_client};

/// The AWS-managed policy an account attaches to a Glue service role: its object grant
/// (`s3:GetObject`) reaches only `aws-glue-*` paths, while its listing grant covers every
/// bucket — so a crawler carrying only it can list a bucket of its own naming and read nothing
/// under it. Source: IAM's own catalogue of AWS-managed policies, under the path AWS files
/// service-role policies at.
const GLUE_SERVICE_ROLE_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueServiceRole";

/// An AWS-managed policy ARN this emulator's IAM carries no document for: a grant the account
/// cannot read, so nothing can be SHOWN about a role that carries it — and nothing is withheld
/// from it.
const UNHELD_MANAGED_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSGlueNoSuchPolicy";

/// The prefix every crawler here is pointed at, and the dataset's own name under it.
const TARGET_PREFIX: &str = "data";

/// The object the dataset's schema is read from, and the columns it carries.
const OBJECT_KEY: &str = "data/rows.csv";
/// The bytes of that object.
const OBJECT_BODY: &str = "id,label\n1,alpha\n";
/// The columns a crawl that read it writes, as `(name, type)`.
const EXPECTED_COLUMNS: [(&str, &str); 2] = [("id", "bigint"), ("label", "string")];

/// An emulator with Glue, Amazon S3 and IAM mounted on ONE shared engine, plus that engine:
/// the crawl reads its objects out of that S3 and resolves its role against that IAM.
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
    let client = s3_client(router);
    client
        .create_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("the bucket {bucket} must be created, got {error:?}"));
    client
        .put_object()
        .bucket(bucket)
        .key(OBJECT_KEY)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            OBJECT_BODY.as_bytes().to_vec(),
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the object {OBJECT_KEY} must be stored, got {error:?}"));
}

/// Attach `document` to `bucket` as its resource policy, through Amazon S3's own
/// `PutBucketPolicy` — so the policy the crawl's decision reads is the one the object store
/// holds.
async fn put_bucket_policy(router: &MultiServiceRouter, bucket: &str, document: &str) {
    s3_client(router)
        .put_bucket_policy()
        .bucket(bucket)
        .policy(document)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("the bucket policy of {bucket} must be stored, got {error:?}")
        });
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

/// Attach the managed policy `arn` to `role_name` — the record `AttachRolePolicy` writes.
fn attach_role_policy(engine: &Arc<engine::InMemoryEngine>, role_name: &str, arn: &str) {
    let state = engine.state::<IamState>();
    let mut store = state.lock();
    store
        .role_attached_policies
        .entry(name_key(role_name))
        .or_default()
        .insert(arn.to_string());
}

/// One identity-based statement, as a whole policy document.
fn policy(effect: &str, actions: Value, resources: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": actions, "Resource": resources }],
    })
    .to_string()
}

/// The ARNs a grant covering the objects under `prefix` of `bucket` names: the bucket itself,
/// which is what a listing is taken on, and every object under the prefix.
fn read_scope(bucket: &str, prefix: &str) -> Value {
    json!([
        format!("arn:aws:s3:::{bucket}"),
        format!("arn:aws:s3:::{bucket}/{prefix}/*"),
    ])
}

/// A document granting the whole read of `prefix` in `bucket`: the listing and the object
/// read.
fn read_grant(bucket: &str, prefix: &str) -> String {
    policy(
        "Allow",
        json!(["s3:ListBucket", "s3:GetObject"]),
        read_scope(bucket, prefix),
    )
}

/// One row of the authorization table.
struct Row {
    /// What the row is about.
    case: &'static str,
    /// The bucket the row's data lives in (no underscore: `CreateBucket` refuses one).
    bucket: &'static str,
    /// The inline policy document the row's role carries, if any.
    identity: Option<String>,
    /// The managed policies attached to the row's role.
    attached: Vec<&'static str>,
    /// The row's bucket policy, if any.
    bucket_policy: Option<String>,
    /// Whether the crawl must publish the dataset behind the target as a table.
    published: bool,
}

/// The rows: what the role's documents, the bucket's policy and the account's records together
/// permit decides whether the crawl reads the data at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role granted the listing and the object read",
            bucket: "crawl-auth-granted",
            identity: Some(read_grant("crawl-auth-granted", TARGET_PREFIX)),
            attached: Vec::new(),
            bucket_policy: None,
            published: true,
        },
        Row {
            case: "a role carrying only the AWS-managed Glue service-role policy",
            bucket: "crawl-auth-managed",
            identity: None,
            attached: vec![GLUE_SERVICE_ROLE_POLICY],
            bucket_policy: None,
            published: false,
        },
        Row {
            case: "a role the account holds no document about, reading a bucket with no policy",
            bucket: "crawl-auth-bare",
            identity: None,
            attached: Vec::new(),
            bucket_policy: None,
            // Nothing is in force over this call, so nothing withholds it: the same position a
            // crawl is in where no IAM is mounted to ask.
            published: true,
        },
        Row {
            case: "a grant of both calls, scoped to another bucket",
            bucket: "crawl-auth-elsewhere",
            identity: Some(read_grant("crawl-auth-other", TARGET_PREFIX)),
            attached: Vec::new(),
            bucket_policy: None,
            published: false,
        },
        Row {
            case: "a grant scoped to another prefix of the same bucket",
            bucket: "crawl-auth-prefix",
            identity: Some(read_grant("crawl-auth-prefix", "elsewhere")),
            attached: Vec::new(),
            bucket_policy: None,
            published: false,
        },
        Row {
            case: "a grant of the listing alone",
            bucket: "crawl-auth-list-only",
            identity: Some(policy(
                "Allow",
                json!("s3:ListBucket"),
                read_scope("crawl-auth-list-only", TARGET_PREFIX),
            )),
            attached: Vec::new(),
            bucket_policy: None,
            published: false,
        },
        Row {
            case: "a grant of the object read alone, with no listing",
            bucket: "crawl-auth-read-only",
            identity: Some(policy(
                "Allow",
                json!("s3:GetObject"),
                read_scope("crawl-auth-read-only", TARGET_PREFIX),
            )),
            attached: Vec::new(),
            bucket_policy: None,
            published: false,
        },
        Row {
            case: "a grant written with wildcard actions",
            bucket: "crawl-auth-wildcard",
            identity: Some(policy(
                "Allow",
                json!(["s3:Get*", "s3:List*"]),
                read_scope("crawl-auth-wildcard", TARGET_PREFIX),
            )),
            attached: Vec::new(),
            bucket_policy: None,
            published: true,
        },
        Row {
            case: "a bucket policy denying every principal but another role",
            bucket: "crawl-auth-fenced",
            identity: Some(read_grant("crawl-auth-fenced", TARGET_PREFIX)),
            attached: Vec::new(),
            bucket_policy: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:*",
                        "Resource": [
                            "arn:aws:s3:::crawl-auth-fenced",
                            "arn:aws:s3:::crawl-auth-fenced/*",
                        ],
                        "Condition": {
                            "StringNotEquals": {
                                "aws:PrincipalArn":
                                    "arn:aws:iam::000000000000:role/crawl-auth-admitted",
                            },
                        },
                    }],
                })
                .to_string(),
            ),
            published: false,
        },
        Row {
            case: "a bucket policy granting this role, with no identity grant at all",
            bucket: "crawl-auth-resource",
            identity: None,
            attached: Vec::new(),
            bucket_policy: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {
                            "AWS": "arn:aws:iam::000000000000:role/crawl-auth-resource-role",
                        },
                        "Action": ["s3:ListBucket", "s3:GetObject"],
                        "Resource": [
                            "arn:aws:s3:::crawl-auth-resource",
                            "arn:aws:s3:::crawl-auth-resource/*",
                        ],
                    }],
                })
                .to_string(),
            ),
            published: true,
        },
        Row {
            case: "a bucket policy granting another action only, with no identity document",
            bucket: "crawl-auth-other-action",
            identity: None,
            attached: Vec::new(),
            bucket_policy: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {
                            "AWS": "arn:aws:iam::000000000000:role/crawl-auth-other-action-role",
                        },
                        "Action": "s3:PutObject",
                        "Resource": "arn:aws:s3:::crawl-auth-other-action/*",
                    }],
                })
                .to_string(),
            ),
            published: false,
        },
        Row {
            case: "a role carrying an attachment whose document the account cannot read",
            bucket: "crawl-auth-unreadable",
            identity: None,
            attached: vec![UNHELD_MANAGED_POLICY],
            bucket_policy: None,
            published: true,
        },
        Row {
            // The other half of that row: what the role carries cannot be read, so only the
            // bucket's own explicit deny — a document the account holds whatever the role
            // carries — is left to withhold the crawl's reads.
            case: "an unreadable attachment, and a bucket policy that denies this role",
            bucket: "crawl-auth-unreadable-denied",
            identity: None,
            attached: vec![UNHELD_MANAGED_POLICY],
            bucket_policy: Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Principal": "*",
                        "Action": "s3:*",
                        "Resource": [
                            "arn:aws:s3:::crawl-auth-unreadable-denied",
                            "arn:aws:s3:::crawl-auth-unreadable-denied/*",
                        ],
                        "Condition": {
                            "StringEquals": {
                                "aws:PrincipalArn":
                                    "arn:aws:iam::000000000000:role/crawl-auth-unreadable-denied-role",
                            },
                        },
                    }],
                })
                .to_string(),
            ),
            published: false,
        },
    ]
}

/// The role name a row's crawler runs as: its own, so one row's documents decide only its own
/// crawl.
fn role_name_of(row: &Row) -> String {
    format!("{}-role", row.bucket)
}

/// The `TableList` of a database, as `GetTables` reports it.
async fn tables_of(router: &MultiServiceRouter, database: &str) -> Vec<Value> {
    let body = ok(router, "GetTables", &json!({ "DatabaseName": database })).await;
    body.get("TableList")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("GetTables must report a TableList, got {body}"))
}

/// The `(name, type)` of every column one table's storage descriptor declares.
fn columns_of(table: &Value) -> Vec<(String, String)> {
    table
        .pointer("/StorageDescriptor/Columns")
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

/// A crawl reads a target only where the account's records permit its role the calls it
/// makes: a target they withhold is not read, so no table is written for it, while a target
/// they permit is catalogued exactly as an unauthorized crawl is not.
#[tokio::test]
async fn a_crawl_reads_only_the_targets_its_role_is_permitted() {
    for row in &rows() {
        let case = row.case;
        let (router, engine) = emulator_with_s3_and_iam();
        let database = format!("{}_db", row.bucket.replace('-', "_"));
        let crawler = format!("{}-crawler", row.bucket);
        let role = role_name_of(row);

        stage(&router, row.bucket).await;
        // The role as the account holds it: trusted by Glue, so the crawler write is admitted,
        // and carrying exactly the documents this row gives it. Recorded BEFORE the bucket
        // policy, because the object store resolves the principal a policy names.
        let role_arn = seed_role(&engine, &role, Some(glue_trust()));
        if let Some(document) = row.identity.as_deref() {
            put_role_policy(&engine, &role, document);
        }
        for attached in &row.attached {
            attach_role_policy(&engine, &role, attached);
        }
        if let Some(document) = row.bucket_policy.as_deref() {
            put_bucket_policy(&router, row.bucket, document).await;
        }

        ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": database } }),
        )
        .await;
        ok(
            &router,
            "CreateCrawler",
            &json!({
                "Name": crawler,
                "Role": role_arn,
                "DatabaseName": database,
                "Targets": { "S3Targets": [{ "Path": format!("s3://{}/{TARGET_PREFIX}/", row.bucket) }] },
            }),
        )
        .await;
        ok(&router, "StartCrawler", &json!({ "Name": crawler })).await;
        // The finish the clock owes the crawler is made due through the store, so the read
        // below reports the crawl that has finished without waiting for a span to pass.
        set_due_instant(&engine, already_past());

        let reported = ok(&router, "GetCrawler", &json!({ "Name": crawler })).await;
        let crawler_state = reported.pointer("/Crawler/State").and_then(Value::as_str);
        assert_eq!(
            crawler_state,
            Some(CRAWLER_STATE_READY),
            "case {case:?}: the finished crawl leaves the crawler ready, got {reported}"
        );
        let status = reported
            .pointer("/Crawler/LastCrawl/Status")
            .and_then(Value::as_str);
        let error = reported
            .pointer("/Crawler/LastCrawl/ErrorMessage")
            .and_then(Value::as_str);
        let tables = tables_of(&router, &database).await;

        // The crawl was carried out either way: it read every target it was permitted to
        // read, so it comes to rest having succeeded and reports nothing about the data it
        // was not permitted to read.
        assert_eq!(
            status,
            Some(LAST_CRAWL_STATUS_SUCCEEDED),
            "case {case:?}: the crawl reached every target it was permitted, got {reported}"
        );
        assert_eq!(
            error, None,
            "case {case:?}: a crawl that failed at nothing carries no error message, got \
             {reported}"
        );

        if row.published {
            assert_eq!(
                tables.len(),
                1,
                "case {case:?}: expected exactly the one dataset behind the target, got {tables:?}"
            );
            let table = &tables[0];
            assert_eq!(
                table.get("Name").and_then(Value::as_str),
                Some(TARGET_PREFIX),
                "case {case:?}: expected the table {TARGET_PREFIX:?}, got {table}"
            );
            let expected: Vec<(String, String)> = EXPECTED_COLUMNS
                .iter()
                .map(|(name, declared)| ((*name).to_string(), (*declared).to_string()))
                .collect();
            let actual = columns_of(table);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the columns {expected:?} read off the data, got \
                 {actual:?}"
            );
            assert_eq!(
                table
                    .pointer("/Parameters")
                    .and_then(Value::as_object)
                    .and_then(|parameters| parameters.get(UPDATED_BY_CRAWLER_PARAMETER))
                    .and_then(Value::as_str),
                Some(crawler.as_str()),
                "case {case:?}: the table records the crawler that wrote it, got {table}"
            );
        } else {
            assert!(
                tables.is_empty(),
                "case {case:?}: a crawl its role may not read must publish no table, got \
                 {tables:?}"
            );
            // And the would-be table resolves to nothing, through the catalog's own reader.
            let (read_status, read_body) = call(
                &router,
                "GetTable",
                &json!({ "DatabaseName": database, "Name": TARGET_PREFIX }),
            )
            .await;
            assert_eq!(
                read_status, 400,
                "case {case:?}: the would-be table must not be served, got {read_status} with \
                 {read_body}"
            );
            assert!(
                error_message(&read_body).contains(TABLE_ENTITY_NOT_FOUND),
                "case {case:?}: expected the table read to report {TABLE_ENTITY_NOT_FOUND:?}, \
                 got {read_body}"
            );
        }
    }
}
