// SPDX-License-Identifier: CC0-1.0

//! What an import task REPORTS about itself: the source it read, the log group its errors
//! are reported in, and how much of the source it processed.
//!
//! The rules these tests pin:
//!
//! 1. the `S3BucketSource` every import reader reports carries the bucket OWNER the source
//!    was read from — "the account number of the S3 bucket that is being imported from. If
//!    the bucket is owned by the requester this is optional" (`S3BucketSource$S3BucketOwner`)
//!    — so a request that named no owner is reported with the requester's own account, and
//!    the member is absent from the REQUEST rather than from the resource;
//! 2. every import reports the `CloudWatchLogGroupArn` of the log group its per-row errors
//!    are reported in ("the ARN of the Cloudwatch Log Group associated with this import
//!    task"), and that group EXISTS where CloudWatch Logs holds log groups, so a caller can
//!    follow the ARN;
//! 3. a finished import reports `ProcessedSizeBytes` — "the total size of data processed
//!    from the source file, in Bytes" — including the 0 of a task that processed nothing, so
//!    a zero-byte import is told apart from an unreported figure;
//! 4. the three readers answer from ONE record: `ImportTable`, `DescribeImport` and
//!    `ListImports` report the same source, owner and log group for the same import.
//!
//! The rows span what the rules generalise over: whether the request names the bucket owner
//! or leaves it to be resolved, whether it names a key prefix, how many items the source
//! carries (so the processed size differs), and whether the source could be read at all.
//!
//! Every DynamoDB and CloudWatch Logs call is driven over the wire (`POST /` with that
//! service's own `X-Amz-Target`) through one `MultiServiceRouter` holding DynamoDB, Amazon
//! S3 and CloudWatch Logs on ONE engine — the source objects and the log group are those
//! services' own resources, so they are read where those services hold them.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{create_bucket, dispatch_as, ok, put_object, REGION};

/// The bucket every source of this suite is read from.
const BUCKET: &str = "import-task-reporting";

/// A bucket the object store holds none of, which is a source nothing can be read from.
const MISSING_BUCKET: &str = "import-task-reporting-absent";

/// One emulator holding this service, Amazon S3 and CloudWatch Logs, over ONE shared
/// engine: an import reads objects S3 holds and reports a log group CloudWatch Logs holds,
/// so both are mounted beside DynamoDB exactly as `core/router` assembles them.
fn emulator_with_s3_and_logs() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// Send one CLOUDWATCH LOGS operation in [`REGION`], signed for that service (`logs`) and
/// named under its own `X-Amz-Target` prefix.
async fn ok_logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = dispatch_as(
        router,
        REGION,
        "logs",
        &format!("Logs_20140328.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case CloudWatch Logs {operation}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// Wait out one transition span, so the `IN_PROGRESS` -> terminal transition the clock owes
/// a started import is due.
async fn settle() {
    tokio::time::sleep(state::STATUS_TRANSITION + Duration::from_millis(250)).await;
}

/// One source object's bytes in the DynamoDB JSON form a table export writes: one item per
/// line, each under the `Item` member.
fn dynamodb_json_lines(items: usize) -> Vec<u8> {
    (0..items)
        .map(|index| {
            format!(
                "{}\n",
                json!({"Item": {"Part": {"S": format!("item-{index}")}}})
            )
        })
        .collect::<String>()
        .into_bytes()
}

/// One import request: the source (bucket, optional owner, optional prefix) and the table
/// it creates.
fn import_request(table: &str, bucket: &str, owner: Option<&str>, prefix: Option<&str>) -> Value {
    let mut source = json!({"S3Bucket": bucket});
    if let Some(owner) = owner {
        source["S3BucketOwner"] = json!(owner);
    }
    if let Some(prefix) = prefix {
        source["S3KeyPrefix"] = json!(prefix);
    }
    json!({
        "S3BucketSource": source,
        "InputFormat": "DYNAMODB_JSON",
        "TableCreationParameters": {
            "TableName": table,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        },
    })
}

/// One row of the reporting table: what the request asks for, and what the readers must
/// then report.
struct Reported {
    case: &'static str,
    table: &'static str,
    bucket: &'static str,
    owner: Option<&'static str>,
    prefix: Option<&'static str>,
    /// How many items the source object under that prefix carries.
    items: usize,
    /// Whether the task could read its source at all, which decides the processed size:
    /// bytes for a source that was read, and the 0 of a task that read nothing.
    read: bool,
}

#[tokio::test]
async fn every_import_reader_reports_the_source_the_log_group_and_the_processed_size() {
    let router = emulator_with_s3_and_logs();
    create_bucket(&router, BUCKET).await;
    // The log-group ARN every import of this Region reports, built by CloudWatch Logs
    // itself for the group DynamoDB reports an import's errors in.
    let expected_log_group = service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_arn(
        REGION,
        state::IMPORT_LOG_GROUP_NAME,
        true,
    );
    // The account the requester's own buckets belong to, which is what an unnamed owner
    // resolves to.
    let own_account = engine::emulator_account_id();

    let rows = [
        Reported {
            case: "no owner named, one item",
            table: "Reported.unowned",
            bucket: BUCKET,
            owner: None,
            prefix: None,
            items: 1,
            read: true,
        },
        Reported {
            case: "the requester's own account named, several items",
            table: "Reported.owned",
            bucket: BUCKET,
            owner: Some(own_account),
            prefix: Some("owned/"),
            items: 4,
            read: true,
        },
        Reported {
            case: "a key prefix and no owner",
            table: "Reported.prefixed",
            bucket: BUCKET,
            owner: None,
            prefix: Some("nested/path/"),
            items: 2,
            read: true,
        },
        Reported {
            case: "a source nothing holds",
            table: "Reported.unreadable",
            bucket: MISSING_BUCKET,
            owner: None,
            prefix: None,
            items: 0,
            read: false,
        },
    ];

    for row in &rows {
        if row.read {
            put_object(
                &router,
                row.bucket,
                &format!("{}source.json", row.prefix.unwrap_or_default()),
                dynamodb_json_lines(row.items),
            )
            .await;
        }
        let started = ok(
            &router,
            "ImportTable",
            import_request(row.table, row.bucket, row.owner, row.prefix),
        )
        .await["ImportTableDescription"]
            .clone();
        let arn = started["ImportArn"].as_str().unwrap_or_default().to_string();

        // Rule 1 and rule 2 hold from the first answer on: the source carries the resolved
        // owner and the log group is named.
        assert_eq!(
            (
                started["S3BucketSource"]["S3Bucket"].clone(),
                started["S3BucketSource"]["S3BucketOwner"].clone(),
                started["S3BucketSource"]["S3KeyPrefix"].clone(),
                started["CloudWatchLogGroupArn"].clone(),
            ),
            (
                json!(row.bucket),
                json!(own_account),
                row.prefix.map_or(Value::Null, |prefix| json!(prefix)),
                json!(expected_log_group),
            ),
            "case {:?}: ImportTable reports the resolved source and the log group, got \
             {started}",
            row.case
        );

        settle().await;
        let described = ok(&router, "DescribeImport", json!({"ImportArn": arn.clone()})).await
            ["ImportTableDescription"]
            .clone();
        // Rule 3: a finished task reports the figure, the 0 of a task that read nothing
        // included.
        let processed = described["ProcessedSizeBytes"].as_i64();
        if row.read {
            assert!(
                processed.is_some_and(|bytes| bytes > 0),
                "case {:?}: a task that read its source reports the bytes it processed, got \
                 {described}",
                row.case
            );
        } else {
            assert_eq!(
                processed,
                Some(0),
                "case {:?}: a task that read nothing REPORTS the nothing it processed, got \
                 {described}",
                row.case
            );
        }
        // Rule 4: the two other readers answer from the same record.
        assert_eq!(
            (
                described["S3BucketSource"].clone(),
                described["CloudWatchLogGroupArn"].clone(),
            ),
            (
                started["S3BucketSource"].clone(),
                json!(expected_log_group),
            ),
            "case {:?}: DescribeImport reports the source and log group the start did, got \
             {described}",
            row.case
        );
        let listed = ok(&router, "ListImports", json!({})).await;
        let summary = listed["ImportSummaryList"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|summary| summary["ImportArn"] == json!(arn))
            .unwrap_or(Value::Null);
        assert_eq!(
            (
                summary["S3BucketSource"].clone(),
                summary["CloudWatchLogGroupArn"].clone(),
            ),
            (
                started["S3BucketSource"].clone(),
                json!(expected_log_group),
            ),
            "case {:?}: the ListImports summary reports the same source and log group, got \
             {listed}",
            row.case
        );
    }

    // Rule 2, the other half: the ARN every import reported names a log group CloudWatch
    // Logs really holds, so a caller that follows it finds the group.
    let groups = ok_logs(
        &router,
        "DescribeLogGroups",
        json!({"logGroupNamePrefix": state::IMPORT_LOG_GROUP_NAME}),
    )
    .await;
    let held: Vec<Value> = groups["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|group| group["logGroupName"] == json!(state::IMPORT_LOG_GROUP_NAME))
        .collect();
    assert_eq!(
        held.len(),
        1,
        "case the log group: the imports share ONE log group, held by CloudWatch Logs, got \
         {groups}"
    );
    assert_eq!(
        held[0]["arn"],
        json!(expected_log_group),
        "case the log group: the group CloudWatch Logs holds is the one the imports named, \
         got {groups}"
    );
}
