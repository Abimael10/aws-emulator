// SPDX-License-Identifier: CC0-1.0

//! What an ACCEPTED Amazon S3 log-export destination grants: the log delivery service's
//! access on that destination bucket's own policy.
//!
//! The rule under test: when a user pool accepts an `S3Configuration` destination, the
//! records are written into that bucket by the log delivery service, so the access that
//! lets it write is granted where Amazon S3 keeps it — the destination bucket's own policy
//! carries two statements for the `delivery.logs.amazonaws.com` principal, one `s3:PutObject`
//! over the keys the delivery writes the exported logs under for the pool's account
//! (`<bucket>/AWSLogs/<account>/*`) and one `s3:GetBucketAcl` over the bucket itself, each
//! scoped to that account's own logging. The grants are MERGED into whatever document the
//! bucket already carries, and a bucket that is granted again is left granting once.
//!
//! The rows span what the rule generalises over rather than one recorded request: the form
//! of the destination bucket's name, how many entries the collection carries and whether
//! another destination type sits beside the Amazon S3 one, whether the bucket already
//! carries a policy of its own, how often the same bucket is configured and by how many
//! pools, and — the other side of the rule — which requests grant NOTHING: a collection
//! naming no Amazon S3 destination, and a request that is refused.
//!
//! Every service is mounted on ONE shared engine, exactly as the assembled emulator mounts
//! them, so the policy read back is the one Amazon S3 itself holds for that bucket.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::log_delivery_configuration::{error_code, pool, read_back, user_notification};
use crate::log_delivery_destinations::{
    account, bucket_arn, given_bucket, given_delivery_stream, given_log_group, log_group_arn,
    router, set, stream_arn, user_auth_events, HELD_LOG_GROUP, HELD_STREAM, REGION,
    STREAM_SINK_BUCKET,
};

/// The identity a destination bucket's policy grants: the service that writes the exported
/// records into it.
const DELIVERY_PRINCIPAL: &str = "delivery.logs.amazonaws.com";

/// The two actions the delivery is granted: the write of the delivered objects, and the read
/// of the destination bucket's ACL it makes before writing.
const WRITE_ACTION: &str = "s3:PutObject";
const ACL_CHECK_ACTION: &str = "s3:GetBucketAcl";

/// The key segment the delivery writes every exported log object under.
const DELIVERED_LOGS_SEGMENT: &str = "AWSLogs";

/// The canned ACL the delivered objects have to be written with, so what lands in the bucket
/// belongs to the bucket's owner.
const BUCKET_OWNER_FULL_CONTROL: &str = "bucket-owner-full-control";

/// What Amazon S3 answers a read of the policy of a bucket that carries none.
const NO_SUCH_BUCKET_POLICY: &str = "NoSuchBucketPolicy";

/// The version an AWS resource policy declares.
const POLICY_VERSION: &str = "2012-10-17";

/// The name the log delivery's own grant document carries.
const DELIVERY_DOCUMENT_ID: &str = "AWSLogDeliveryWrite20150319";

/// Drive one Amazon S3 request through its own (REST) wire, path style, and return its status
/// and body text.
async fn s3_request(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// `GET /<bucket>?policy` — the policy Amazon S3 holds for `bucket`, as the status and the
/// body it answers with.
async fn bucket_policy(router: &MultiServiceRouter, bucket: &str) -> (u16, String) {
    s3_request(router, "GET", &format!("/{bucket}?policy"), "").await
}

/// The policy document Amazon S3 holds for `bucket`, requiring that it holds one.
async fn held_policy(router: &MultiServiceRouter, case: &str, bucket: &str) -> Value {
    let (status, body) = bucket_policy(router, bucket).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon S3 must hold a policy for {bucket:?}, got {status} with {body:?}"
    );
    serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("case {case:?}: the policy must be JSON ({error}): {body}"))
}

/// Assert that `bucket` carries no policy at all, which is what Amazon S3 answers for a
/// bucket nothing has been granted on.
async fn carries_no_policy(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (status, body) = bucket_policy(router, bucket).await;
    assert_eq!(
        status, 404,
        "case {case:?}: {bucket:?} must carry no policy, got {status} with {body:?}"
    );
    assert!(
        body.contains(NO_SUCH_BUCKET_POLICY),
        "case {case:?}: {bucket:?} must be reported {NO_SUCH_BUCKET_POLICY}, got {body:?}"
    );
}

/// `PUT /<bucket>?policy` — attach `document` to `bucket` as its owner would.
async fn given_bucket_policy(router: &MultiServiceRouter, bucket: &str, document: &Value) {
    let (status, body) = s3_request(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &document.to_string(),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "PutBucketPolicy({bucket}) must succeed, got {status} with {body:?}"
    );
}

/// The statements a bucket's policy carries for the delivery principal, in the order the
/// document carries them.
fn delivery_statements(document: &Value) -> Vec<Value> {
    document
        .get("Statement")
        .and_then(Value::as_array)
        .map(|statements| {
            statements
                .iter()
                .filter(|statement| {
                    statement["Principal"]["Service"].as_str() == Some(DELIVERY_PRINCIPAL)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The grants the log delivery is given on the destination bucket `bucket`, written out from
/// the rule rather than read from the service: the ACL check of the bucket and the write of
/// the keys the exported logs land under for the configuring account, each scoped to that
/// account's own logging in this Region.
fn expected_delivery_grants(bucket: &str) -> Vec<Value> {
    let account = account();
    let source_arn = format!("arn:aws:logs:{REGION}:{account}:*");
    vec![
        json!({
            "Sid": "AWSLogDeliveryAclCheck",
            "Effect": "Allow",
            "Principal": { "Service": DELIVERY_PRINCIPAL },
            "Action": ACL_CHECK_ACTION,
            "Resource": bucket_arn(bucket),
            "Condition": {
                "StringEquals": { "aws:SourceAccount": account },
                "ArnLike": { "aws:SourceArn": source_arn },
            },
        }),
        json!({
            "Sid": "AWSLogDeliveryWrite",
            "Effect": "Allow",
            "Principal": { "Service": DELIVERY_PRINCIPAL },
            "Action": WRITE_ACTION,
            "Resource": format!(
                "{}/{DELIVERED_LOGS_SEGMENT}/{account}/*",
                bucket_arn(bucket)
            ),
            "Condition": {
                "StringEquals": {
                    "s3:x-amz-acl": BUCKET_OWNER_FULL_CONTROL,
                    "aws:SourceAccount": account,
                },
                "ArnLike": { "aws:SourceArn": source_arn },
            },
        }),
    ]
}

/// A `userAuthEvents` entry exporting to the Amazon S3 bucket `bucket`.
fn s3_export(bucket: &str) -> Value {
    user_auth_events(json!({ "S3Configuration": { "BucketArn": bucket_arn(bucket) } }))
}

#[tokio::test]
async fn an_accepted_s3_destination_grants_the_delivery_on_that_bucket() {
    let router = router();
    given_log_group(&router, HELD_LOG_GROUP).await;
    // A Plus pool, so the plan gate includes `userAuthEvents` and what each row turns on is
    // the destination.
    let user_pool_id = pool(&router, "a Plus pool", Some("PLUS")).await;

    // (case, the destination bucket, the collection set on the pool)
    //
    // The rows differ along the FORM of the bucket's name (Amazon S3 general purpose bucket
    // naming admits labels of lowercase letters, digits and hyphens separated by dots) and
    // along the collection the destination sits in: alone, or beside an entry exporting to a
    // destination of another service.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "an S3 destination alone",
            "cognito-pool-logs",
            json!([s3_export("cognito-pool-logs")]),
        ),
        (
            "a bucket whose name carries dots",
            "cognito.pool.logs",
            json!([s3_export("cognito.pool.logs")]),
        ),
        (
            "a bucket whose name carries digits",
            "cognito-pool-logs-2026",
            json!([s3_export("cognito-pool-logs-2026")]),
        ),
        (
            "an S3 destination beside a CloudWatch Logs one",
            "cognito-pool-logs-beside",
            json!([
                user_notification(&log_group_arn(HELD_LOG_GROUP)),
                s3_export("cognito-pool-logs-beside"),
            ]),
        ),
    ];

    let mut granted: Vec<&str> = Vec::new();
    for (case, bucket, log_configurations) in &rows {
        given_bucket(&router, bucket).await;
        // Before the set, the bucket Amazon S3 holds carries no policy at all.
        carries_no_policy(&router, case, bucket).await;

        let (status, payload) = set(&router, &user_pool_id, log_configurations).await;
        assert_eq!(
            status, 200,
            "case {case:?}: every destination here is held by its own service, got {status} \
             with {payload}"
        );
        let configuration = read_back(&router, case, &user_pool_id).await;
        assert_eq!(
            configuration,
            json!({ "UserPoolId": user_pool_id, "LogConfigurations": log_configurations }),
            "case {case:?}: the pool must report the collection it accepted, got {configuration}"
        );

        let document = held_policy(&router, case, bucket).await;
        let expected = expected_delivery_grants(bucket);
        let statements = document
            .get("Statement")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            statements, expected,
            "case {case:?}: the destination bucket must carry exactly the two delivery \
             grants, got {statements:?}"
        );
        assert_eq!(
            document["Version"].as_str(),
            Some(POLICY_VERSION),
            "case {case:?}: expected the policy version {POLICY_VERSION:?}, got {document}"
        );
        assert_eq!(
            document["Id"].as_str(),
            Some(DELIVERY_DOCUMENT_ID),
            "case {case:?}: expected the delivery's own document id {DELIVERY_DOCUMENT_ID:?}, \
             got {document}"
        );
        granted.push(bucket);
    }

    // A set REPLACES the collection, so by now the pool exports to the last row's bucket
    // alone — and every bucket granted along the way still carries its grant: the policy is
    // the bucket owner's document, which this service only ever adds the delivery's
    // statements to.
    for bucket in granted {
        let case = format!("{bucket:?} after the pool was re-pointed");
        let document = held_policy(&router, &case, bucket).await;
        let statements = delivery_statements(&document);
        assert_eq!(
            statements,
            expected_delivery_grants(bucket),
            "case {case:?}: a grant already written is not retracted, got {statements:?}"
        );
    }
}

#[tokio::test]
async fn the_grants_are_merged_into_the_policy_the_destination_bucket_carries() {
    let router = router();
    let bucket = "cognito-owned-logs";
    given_bucket(&router, bucket).await;

    // The bucket's owner wrote a policy of their own first: a refusal of insecure transport,
    // which nothing about a log export may touch.
    let owners_statement = json!({
        "Sid": "DenyInsecureTransport",
        "Effect": "Deny",
        "Principal": "*",
        "Action": "s3:*",
        "Resource": [bucket_arn(bucket), format!("{}/*", bucket_arn(bucket))],
        "Condition": { "Bool": { "aws:SecureTransport": "false" } },
    });
    given_bucket_policy(
        &router,
        bucket,
        &json!({
            "Version": POLICY_VERSION,
            "Id": "TheOwnersDocument",
            "Statement": [owners_statement.clone()],
        }),
    )
    .await;

    // Three configurations of the same bucket: the same pool twice, and a second pool of the
    // same account. Each is the same grant, because the statements are of the bucket and of
    // the account rather than of the pool.
    let first = pool(&router, "the first pool", Some("PLUS")).await;
    let second = pool(&router, "the second pool", Some("PLUS")).await;
    let configurations = json!([s3_export(bucket)]);
    for (case, user_pool_id) in [
        ("the first configuration", &first),
        ("the same pool configured again", &first),
        ("a second pool of the same account", &second),
    ] {
        let (status, payload) = set(&router, user_pool_id, &configurations).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the bucket is held by Amazon S3, got {status} with {payload}"
        );

        let document = held_policy(&router, case, bucket).await;
        let statements = document
            .get("Statement")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut expected = vec![owners_statement.clone()];
        expected.extend(expected_delivery_grants(bucket));
        assert_eq!(
            statements, expected,
            "case {case:?}: the owner's statement must stay and the delivery must be granted \
             exactly once, got {statements:?}"
        );
        // The owner named their document, and the grants do not rename it.
        assert_eq!(
            document["Id"].as_str(),
            Some("TheOwnersDocument"),
            "case {case:?}: the owner's own document id must be kept, got {document}"
        );
    }
}

#[tokio::test]
async fn a_request_that_grants_nothing_leaves_the_bucket_without_a_policy() {
    let router = router();
    given_log_group(&router, HELD_LOG_GROUP).await;
    given_bucket(&router, STREAM_SINK_BUCKET).await;
    given_delivery_stream(&router, HELD_STREAM).await;

    // One bucket Amazon S3 holds, which no row below exports to.
    let untouched = "cognito-untouched-logs";
    given_bucket(&router, untouched).await;

    let plus = pool(&router, "a Plus pool", Some("PLUS")).await;
    let lite = pool(&router, "a Lite pool", Some("LITE")).await;

    // (case, the pool, the collection, the status and error code it is answered with)
    //
    // Two kinds of request grant nothing: one that is ACCEPTED but names no Amazon S3
    // destination, and one that is REFUSED — whether the refusal is the pool's feature plan
    // or the request's own shape.
    type GrantsNothingRow<'a> = (&'a str, &'a String, Value, (u16, &'a str));
    let rows: Vec<GrantsNothingRow<'_>> = vec![
        (
            "the empty collection, which clears the pool's logging",
            &plus,
            json!([]),
            (200, ""),
        ),
        (
            "a CloudWatch Logs destination",
            &plus,
            json!([user_notification(&log_group_arn(HELD_LOG_GROUP))]),
            (200, ""),
        ),
        (
            "a Firehose destination, whose own sink bucket is not the pool's",
            &plus,
            json!([user_auth_events(
                json!({ "FirehoseConfiguration": { "StreamArn": stream_arn(HELD_STREAM) } })
            )]),
            (200, ""),
        ),
        (
            "an S3 destination the pool's feature plan excludes",
            &lite,
            json!([s3_export(untouched)]),
            // The status this service answers a feature-plan refusal with, which the
            // crate's feature-plan suite pins (`feature_plan_gate`).
            (400, "FeatureUnavailableInTierException"),
        ),
        (
            "an entry naming two destinations, one of them the bucket",
            &plus,
            json!([user_auth_events(json!({
                "CloudWatchLogsConfiguration": { "LogGroupArn": log_group_arn(HELD_LOG_GROUP) },
                "S3Configuration": { "BucketArn": bucket_arn(untouched) },
            }))]),
            (400, "InvalidParameterException"),
        ),
        (
            "the same event source twice, both naming the bucket",
            &plus,
            json!([s3_export(untouched), s3_export(untouched)]),
            (400, "InvalidParameterException"),
        ),
    ];

    for (case, user_pool_id, log_configurations, (expected_status, expected_code)) in &rows {
        let (status, payload) = set(&router, user_pool_id, log_configurations).await;
        if *expected_status == 200 {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the set to be accepted, got {status} with {payload}"
            );
        } else {
            assert_eq!(
                (status, error_code(&payload)),
                (*expected_status, *expected_code),
                "case {case:?}: expected a {expected_status} {expected_code}, got {status} \
                 with {payload}"
            );
        }
        // Neither the bucket no accepted collection named, nor the sink bucket of the
        // Firehose stream the pool exports THROUGH, is a bucket the pool's own log delivery
        // writes into.
        carries_no_policy(&router, case, untouched).await;
        carries_no_policy(&router, case, STREAM_SINK_BUCKET).await;
    }

    // And the check can fail: the very same bucket IS granted once a pool that may export to
    // it is pointed at it.
    let case = "the Plus pool exporting to the bucket";
    let (status, payload) = set(&router, &plus, &json!([s3_export(untouched)])).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the set to be accepted, got {status} with {payload}"
    );
    let document = held_policy(&router, case, untouched).await;
    let statements = delivery_statements(&document);
    assert_eq!(
        statements,
        expected_delivery_grants(untouched),
        "case {case:?}: the destination bucket must carry the two delivery grants, \
         got {statements:?}"
    );
}
