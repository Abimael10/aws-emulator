// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the terminal outcome a started job settles in: the job
//! error it reports is the one its OWN work failed on.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `dataexchange` signing name — against a router
//! that mounts the S3 service BESIDE DataExchange on ONE shared engine, which is what
//! the assembled emulator does. Every bucket, object and bucket policy a case reads is
//! created through S3's own front door, so what a job reports is grounded in what S3
//! holds rather than in anything this suite reached into.
//!
//! The rules under test:
//!
//! 1. A job that publishes an Amazon S3 data access works on data that stays in a
//!    bucket the CALLER owns, which AWS Data Exchange reads in its own name on behalf
//!    of the subscribers the data access entitles — so its outcome turns on the grant
//!    that bucket's own policy carries for the AWS Data Exchange service principal. A
//!    bucket whose policy does not carry it settles the job in `ERROR` with a job error
//!    whose `Code` is the modelled `VALIDATION_EXCEPTION` and whose `Message` names
//!    that bucket.
//! 2. The not-found `Code` is reported only for a source that is NOT there: a bucket
//!    this emulator holds none of, keys the bucket does not hold, and the sources this
//!    emulator holds nothing of for the other job types.
//! 3. A bucket whose policy carries the grant and which holds the data the job names
//!    settles the job in `COMPLETED`, and the Amazon S3 data access it published is
//!    the asset the revision then holds: `ListRevisionAssets` and `GetAsset` report it
//!    with `AssetDetails.S3DataAccessAsset` naming the bucket, keys and key prefixes
//!    the job asked for, and no access point, because this emulator mints none.
//! 4. An import from Amazon S3 whose source objects the storage HOLDS carries its work
//!    out: it completes with no `Errors`, and the revision its details name holds one
//!    asset per source object — `Name` that object's own Amazon S3 key ("When importing
//!    from Amazon S3, the Amazon S3 object key is used as the asset name",
//!    `AssetEntry.Name`'s own documentation), the `AssetType` of its data set, and
//!    `AssetDetails.S3SnapshotAsset.Size` the number of bytes that object holds.
//! 5. A source the storage answers it does not hold — a bucket it holds none of, or a
//!    key a bucket does not hold — settles the import in `ERROR` with the not-found
//!    `Code`, naming that bucket and key in the `Details` member the model gives this
//!    job type's errors, and the revision receives nothing.
//! 6. An export to Amazon S3 writes the content of the assets its details name into the
//!    destination bucket — under the `Key` or key pattern the destination names, and
//!    under the asset's own `Name` when it names none — so a client reads the same
//!    bytes, declaring the media type the source declared, back out of the storage; a
//!    destination bucket the storage does not hold settles the job in `ERROR` and leaves
//!    no object behind.
//!
//! The rows span what rule 1 generalises over: a bucket with no policy at all, a grant
//! to another principal, a grant covering one of the two permissions, a grant over
//! another bucket, an explicit deny beside the grant, a grant conditioned on another
//! account and one conditioned on the secure transport this read arrives over, and a
//! source naming keys, key prefixes, both and neither. The import and export tables
//! state what they span where they are declared.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::data_sets::{call, create_data_set, harness_with_object_storage, Answer, ACCOUNT, REGION};
use crate::job_lifecycle::{get_job, settle_past_the_execution_span, start_job};
use crate::jobs::create_job;

/// A SigV4 `Authorization` header in the `s3` scope: the credential scope is how the
/// router picks the service a request is for, so a request to the object storage front
/// door carries one naming `s3`. Nothing here is verified — the harness turns request
/// authentication off, exactly as an unauthenticated emulator runs.
fn signed_for_s3() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=TESTKEY/20240101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// The job `Type` that publishes an Amazon S3 data access, and the `Details` member
/// the model pairs it with (`smithy_query(query="shape", shape="Type,RequestDetails")`).
const DATA_ACCESS_TYPE: &str = "CREATE_S3_DATA_ACCESS_FROM_S3_BUCKET";
const DATA_ACCESS_MEMBER: &str = "CreateS3DataAccessFromS3Bucket";

/// The `AssetType` of a data set whose revisions hold Amazon S3 data accesses, from
/// the model's `AssetType` enum (`smithy_query(query="shape", shape="AssetType")`).
const DATA_ACCESS_ASSET_TYPE: &str = "S3_DATA_ACCESS";

/// The service principal a data access source bucket's policy has to grant the read
/// to, and the two Amazon S3 actions that grant covers — this suite's own statement of
/// them, so the crate's constants and the policies here cannot agree by construction.
const DATA_EXCHANGE: &str = "dataexchange.amazonaws.com";
const GET_OBJECT: &str = "s3:GetObject";
const LIST_BUCKET: &str = "s3:ListBucket";

/// The modelled `Code` values a job error carries here, from the model's `Code` enum
/// (`smithy_query(query="shape", shape="Code")`: `ACCESS_DENIED_EXCEPTION`,
/// `INTERNAL_SERVER_EXCEPTION`, `MALWARE_DETECTED`, `RESOURCE_NOT_FOUND_EXCEPTION`,
/// `SERVICE_QUOTA_EXCEEDED_EXCEPTION`, `VALIDATION_EXCEPTION`,
/// `MALWARE_SCAN_ENCRYPTED_FILE`).
const VALIDATION_EXCEPTION: &str = "VALIDATION_EXCEPTION";
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "RESOURCE_NOT_FOUND_EXCEPTION";

/// The `Message` a job error of a source that is not there carries, stated here
/// independently of the crate that composes it.
const SOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found. Unable to find source Amazon S3 object.";

/// One request to the object storage front door, path-style (the emulator serves one
/// endpoint per region, so the bucket travels in the path). Answers the HTTP status.
/// One answer from the object storage front door, reduced to what these cases read.
struct ObjectAnswer {
    status: u16,
    /// The `Content-Type` the answer declares, where it declares one.
    content_type: Option<String>,
    body: Vec<u8>,
}

/// One request to the object storage front door, path-style (the emulator serves one
/// endpoint per region, so the bucket travels in the path). Answers the WHOLE response,
/// for the cases that read an exported object back out of it.
async fn s3_answer(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> ObjectAnswer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", signed_for_s3())
        .header("content-type", content_type)
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed S3 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    ObjectAnswer {
        status,
        content_type,
        body: response.into_body().to_vec(),
    }
}

/// The media type a request declares when the case under test is not about one.
const DEFAULT_MEDIA_TYPE: &str = "application/octet-stream";

/// The same request as [`s3_answer`], reduced to the HTTP status.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> u16 {
    s3_answer(router, method, path, DEFAULT_MEDIA_TYPE, body)
        .await
        .status
}

/// `PUT /<bucket>` — `CreateBucket`, naming the Region these requests are signed for
/// as its location constraint, which a regional endpoint is what serves.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let status = s3(router, "PUT", &format!("/{bucket}"), configuration.as_bytes()).await;
    assert_eq!(
        status, 200,
        "bucket {bucket:?}: expected CreateBucket 200, got {status}"
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`.
async fn put_object(router: &MultiServiceRouter, bucket: &str, key: &str, body: &[u8]) {
    let status = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        status, 200,
        "object s3://{bucket}/{key}: expected PutObject 200, got {status}"
    );
}

/// `PUT /<bucket>/<key>` — `PutObject`, declaring `media_type` for the content, which
/// is what a later read of that object answers.
async fn put_typed_object(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    media_type: &str,
    body: &[u8],
) {
    let answer = s3_answer(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        media_type,
        body,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "object s3://{bucket}/{key}: expected PutObject 200, got {}",
        answer.status
    );
}

/// `GET /<bucket>/<key>` — `GetObject`, the call a client makes to read an export.
async fn get_object(router: &MultiServiceRouter, bucket: &str, key: &str) -> ObjectAnswer {
    s3_answer(
        router,
        "GET",
        &format!("/{bucket}/{key}"),
        DEFAULT_MEDIA_TYPE,
        &[],
    )
    .await
}

/// `PUT /<bucket>?policy` — `PutBucketPolicy`.
async fn put_bucket_policy(router: &MultiServiceRouter, bucket: &str, policy: &Value) {
    let document = serde_json::to_vec(policy).expect("a policy document serializes");
    let status = s3(router, "PUT", &format!("/{bucket}?policy"), &document).await;
    assert_eq!(
        status, 204,
        "bucket {bucket:?}: expected PutBucketPolicy 204, got {status}"
    );
}

/// One statement of a bucket policy, with the principal, the actions, the resources
/// and the optional condition the row varies.
fn statement(effect: &str, principal: &str, actions: Value, resources: Value) -> Value {
    json!({
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": actions,
        "Resource": resources,
    })
}

/// A whole bucket policy carrying `statements`.
fn policy(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The grant a provider of an Amazon S3 data access puts on the bucket: AWS Data
/// Exchange may read the objects being shared and list the bucket they are found in.
fn full_grant(bucket: &str) -> Value {
    policy(vec![statement(
        "Allow",
        DATA_EXCHANGE,
        json!([GET_OBJECT, LIST_BUCKET]),
        json!([
            format!("arn:aws:s3:::{bucket}"),
            format!("arn:aws:s3:::{bucket}/*"),
        ]),
    )])
}

/// What a job's execution must settle in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// `COMPLETED`, with the data access published as the revision's asset.
    Published,
    /// `ERROR`, with the validation job error naming the bucket.
    NotGranted,
    /// `ERROR`, with the not-found job error for a source that is not there.
    SourceAbsent,
}

/// One row: the case, the bucket to build, what it holds, the policy it carries, the
/// `AssetSource` members the job names, and the outcome the job must settle in.
struct Row {
    case: &'static str,
    /// Whether the bucket is created at all — a bucket this emulator does not hold is
    /// a source that is not there.
    bucket_exists: bool,
    /// The objects put into the bucket before the job runs.
    objects: &'static [&'static str],
    /// The policy the bucket carries, built from its own name; `None` for a bucket
    /// with no policy at all.
    policy: Option<fn(&str) -> Value>,
    /// The `Keys` the data access shares.
    keys: &'static [&'static str],
    /// The `KeyPrefixes` the data access shares.
    key_prefixes: &'static [&'static str],
    expected: Expected,
}

/// Every row of the table, in the order they run. Each gets its own bucket, so one
/// row's policy is never another's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket with no policy at all",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: None,
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            case: "a grant to another service principal",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                policy(vec![statement(
                    "Allow",
                    "amplify.amazonaws.com",
                    json!([GET_OBJECT, LIST_BUCKET]),
                    json!([
                        format!("arn:aws:s3:::{bucket}"),
                        format!("arn:aws:s3:::{bucket}/*"),
                    ]),
                )])
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            // The grant a provider most easily gets wrong: the object read without the
            // bucket listing the keys and prefixes of a data access are resolved by.
            case: "a grant covering the object read but not the bucket listing",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                policy(vec![statement(
                    "Allow",
                    DATA_EXCHANGE,
                    json!([GET_OBJECT, "s3:GetBucketLocation"]),
                    json!([
                        format!("arn:aws:s3:::{bucket}"),
                        format!("arn:aws:s3:::{bucket}/*"),
                    ]),
                )])
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            case: "a grant covering the bucket listing but not the object read",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                policy(vec![statement(
                    "Allow",
                    DATA_EXCHANGE,
                    json!(LIST_BUCKET),
                    json!(format!("arn:aws:s3:::{bucket}")),
                )])
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            case: "a grant over another bucket",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|_| {
                policy(vec![statement(
                    "Allow",
                    DATA_EXCHANGE,
                    json!([GET_OBJECT, LIST_BUCKET]),
                    json!(["arn:aws:s3:::another-provider-bucket", "arn:aws:s3:::another-provider-bucket/*"]),
                )])
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            case: "a deny beside the grant",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                policy(vec![
                    statement(
                        "Allow",
                        DATA_EXCHANGE,
                        json!([GET_OBJECT, LIST_BUCKET]),
                        json!([
                            format!("arn:aws:s3:::{bucket}"),
                            format!("arn:aws:s3:::{bucket}/*"),
                        ]),
                    ),
                    statement(
                        "Deny",
                        DATA_EXCHANGE,
                        json!(GET_OBJECT),
                        json!(format!("arn:aws:s3:::{bucket}/*")),
                    ),
                ])
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            // `aws:SourceAccount` is an IAM global condition key; a grant scoped to
            // another account is not a grant to this read, which is made on behalf of
            // the one account this emulator serves.
            case: "a grant conditioned on another account",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                let mut document = full_grant(bucket);
                document["Statement"][0]["Condition"] =
                    json!({ "StringEquals": { "aws:SourceAccount": "210987654321" } });
                document
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::NotGranted,
        },
        Row {
            case: "the grant, over the keys the job names",
            bucket_exists: true,
            objects: &["reports/q1.csv", "reports/q2.csv"],
            policy: Some(full_grant),
            keys: &["reports/q1.csv", "reports/q2.csv"],
            key_prefixes: &[],
            expected: Expected::Published,
        },
        Row {
            case: "the grant, over the key prefixes the job names",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(full_grant),
            keys: &[],
            key_prefixes: &["reports/"],
            expected: Expected::Published,
        },
        Row {
            case: "the grant, over keys and key prefixes together",
            bucket_exists: true,
            objects: &["reports/q1.csv", "tables/orders.parquet"],
            policy: Some(full_grant),
            keys: &["tables/orders.parquet"],
            key_prefixes: &["reports/"],
            expected: Expected::Published,
        },
        Row {
            // A source naming neither keys nor prefixes shares the bucket's whole
            // object space.
            case: "the grant, over the whole bucket",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(full_grant),
            keys: &[],
            key_prefixes: &[],
            expected: Expected::Published,
        },
        Row {
            // The usual "refuse everything that did not arrive over a secure
            // transport" clause: this read arrives over one, so it refuses nothing.
            case: "the grant, conditioned on the secure transport this read arrives over",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(|bucket| {
                let mut document = full_grant(bucket);
                document["Statement"][0]["Condition"] =
                    json!({ "Bool": { "aws:SecureTransport": "true" } });
                document
            }),
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::Published,
        },
        Row {
            case: "the grant, over a key the bucket does not hold",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(full_grant),
            keys: &["reports/q4.csv"],
            key_prefixes: &[],
            expected: Expected::SourceAbsent,
        },
        Row {
            case: "the grant, over a key prefix nothing is stored under",
            bucket_exists: true,
            objects: &["reports/q1.csv"],
            policy: Some(full_grant),
            keys: &[],
            key_prefixes: &["archive/"],
            expected: Expected::SourceAbsent,
        },
        Row {
            case: "a bucket this emulator does not hold",
            bucket_exists: false,
            objects: &[],
            policy: None,
            keys: &["reports/q1.csv"],
            key_prefixes: &[],
            expected: Expected::SourceAbsent,
        },
    ]
}

/// The `AssetSource` of `row`, over the bucket named for it.
fn asset_source(row: &Row, bucket: &str) -> Value {
    let mut source = json!({ "Bucket": bucket });
    if !row.keys.is_empty() {
        source["Keys"] = json!(row.keys);
    }
    if !row.key_prefixes.is_empty() {
        source["KeyPrefixes"] = json!(row.key_prefixes);
    }
    source
}

/// A data set holding Amazon S3 data accesses, with one revision on it.
async fn data_access_data_set(router: &MultiServiceRouter, name: &str) -> (String, String) {
    data_set_with_revision(router, name, DATA_ACCESS_ASSET_TYPE).await
}

/// A data set of `asset_type` with one revision on it — a fresh pair per case, so one
/// case's assets are never another's.
async fn data_set_with_revision(
    router: &MultiServiceRouter,
    name: &str,
    asset_type: &str,
) -> (String, String) {
    let data_set = create_data_set(router, name, "for data access jobs", asset_type, None).await;
    assert_eq!(
        data_set.status, 201,
        "case {name}: expected 201 from CreateDataSet, got {} body {:?}",
        data_set.status, data_set.body
    );
    let data_set_id = data_set.string("Id");
    let revision = call(
        router,
        "POST",
        &format!("/v1/data-sets/{data_set_id}/revisions"),
        Some(json!({ "Comment": "for data access jobs" })),
    )
    .await;
    assert_eq!(
        revision.status, 201,
        "case {name}: expected 201 from CreateRevision, got {} body {:?}",
        revision.status, revision.body
    );
    (data_set_id, revision.string("Id"))
}

/// Create one data access job, start it, and read it back once the clock owes it its
/// terminal state.
async fn settled_job(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    details: Value,
) -> Answer {
    settled_job_of_type(router, engine, case, DATA_ACCESS_TYPE, details).await
}

/// The same, for a job of any `Type`: create it, start it, and read it back once the
/// clock owes it its terminal state.
async fn settled_job_of_type(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    job_type: &str,
    details: Value,
) -> Answer {
    let created = create_job(router, job_type, details, None).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected 201 from CreateJob, got {} body {:?}",
        created.status, created.body
    );
    let job_id = created.string("Id");
    let started = start_job(router, &job_id).await;
    assert_eq!(
        started.status, 202,
        "case {case}: expected 202 from StartJob, got {} body {:?}",
        started.status, started.body
    );
    settle_past_the_execution_span(engine);
    let settled = get_job(router, &job_id).await;
    assert_eq!(
        settled.status, 200,
        "case {case}: expected 200 from GetJob, got {} body {:?}",
        settled.status, settled.body
    );
    settled
}

/// The one job error a settled job reports.
fn only_error(case: &str, settled: &Answer) -> Value {
    let errors = settled
        .member("Errors")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        errors.len(),
        1,
        "case {case}: expected one job error, got {:?}",
        settled.member("Errors")
    );
    errors[0].clone()
}

/// RULES 1-3: the outcome of a data access job is decided from what this emulator's
/// own object storage answers about the bucket it would share — the grant for AWS Data
/// Exchange in that bucket's policy, then whether the data it names is there.
#[tokio::test]
async fn a_data_access_job_settles_on_what_the_source_bucket_answers() {
    let (router, engine) = harness_with_object_storage();
    let (data_set_id, revision_id) = data_access_data_set(&router, "data-access-outcomes").await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let bucket = format!("provider-bucket-{index}");
        if row.bucket_exists {
            create_bucket(&router, &bucket).await;
            for key in row.objects {
                put_object(&router, &bucket, key, b"id,amount\n1,10\n").await;
            }
            if let Some(build) = row.policy {
                put_bucket_policy(&router, &bucket, &build(&bucket)).await;
            }
        }

        let settled = settled_job(
            &router,
            &engine,
            case,
            json!({ DATA_ACCESS_MEMBER: {
                "AssetSource": asset_source(row, &bucket),
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            }}),
        )
        .await;
        let state = settled.string("State");

        match row.expected {
            Expected::Published => {
                assert_eq!(
                    state, "COMPLETED",
                    "case {case}: expected the published data access to complete, got \
                     {state:?} with {:?}",
                    settled.member("Errors")
                );
                assert!(
                    settled.member("Errors").is_null(),
                    "case {case}: expected no Errors on a job that was carried out, got {:?}",
                    settled.member("Errors")
                );
                assert_published_asset(&router, &data_set_id, &revision_id, row, &bucket).await;
            }
            Expected::NotGranted => {
                assert_eq!(
                    state, "ERROR",
                    "case {case}: expected a job the bucket policy stops to end in ERROR, \
                     got {state:?}"
                );
                let error = only_error(case, &settled);
                assert_eq!(
                    error.get("Code").and_then(Value::as_str),
                    Some(VALIDATION_EXCEPTION),
                    "case {case}: expected the modelled validation Code, got {error:?}"
                );
                let message = error
                    .get("Message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    message.contains(&bucket),
                    "case {case}: expected the message to name the bucket {bucket:?}, got \
                     {message:?}"
                );
                assert!(
                    message.contains(GET_OBJECT),
                    "case {case}: expected the message to name the {GET_OBJECT} prerequisite, \
                     got {message:?}"
                );
                // The identity of the job travels in the job's own record, and the
                // model's `JobErrorResourceTypes` (`REVISION | ASSET | DATA_SET`) has
                // no value for a bucket, so neither member is invented.
                for absent in ["ResourceId", "ResourceType", "Details", "LimitName"] {
                    assert!(
                        error.get(absent).is_none(),
                        "case {case}: expected no {absent} on this job error, got {error:?}"
                    );
                }
            }
            Expected::SourceAbsent => {
                assert_eq!(
                    state, "ERROR",
                    "case {case}: expected a job whose source is absent to end in ERROR, got \
                     {state:?}"
                );
                let error = only_error(case, &settled);
                assert_eq!(
                    error.get("Code").and_then(Value::as_str),
                    Some(RESOURCE_NOT_FOUND_EXCEPTION),
                    "case {case}: expected the modelled not-found Code, got {error:?}"
                );
                assert_eq!(
                    error.get("Message").and_then(Value::as_str),
                    Some(SOURCE_NOT_FOUND_MESSAGE),
                    "case {case}: expected the message of a source that cannot be found, got \
                     {error:?}"
                );
            }
        }
    }
}

/// Read the asset a published data access left in the revision back through the
/// operations that report it, and assert it IS that data access.
async fn assert_published_asset(
    router: &MultiServiceRouter,
    data_set_id: &str,
    revision_id: &str,
    row: &Row,
    bucket: &str,
) {
    let case = row.case;
    let listed = call(
        router,
        "GET",
        &format!("/v1/data-sets/{data_set_id}/revisions/{revision_id}/assets"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected 200 from ListRevisionAssets, got {} body {:?}",
        listed.status, listed.body
    );
    let entry = listed
        .member("Assets")
        .as_array()
        .and_then(|assets| {
            assets
                .iter()
                .find(|asset| asset["Name"].as_str() == Some(bucket))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected the revision to hold the data access of {bucket:?}, got \
                 {:?}",
                listed.member("Assets")
            )
        });
    assert_eq!(
        entry["AssetType"].as_str(),
        Some(DATA_ACCESS_ASSET_TYPE),
        "case {case}: expected the asset to carry the data set's AssetType, got {entry:?}"
    );

    let details = &entry["AssetDetails"];
    let access = &details["S3DataAccessAsset"];
    assert_eq!(
        access["Bucket"].as_str(),
        Some(bucket),
        "case {case}: expected the data access to name the bucket, got {details:?}"
    );
    assert_eq!(
        access["Keys"],
        expected_list(row.keys),
        "case {case}: expected the keys the job named, got {details:?}"
    );
    assert_eq!(
        access["KeyPrefixes"],
        expected_list(row.key_prefixes),
        "case {case}: expected the key prefixes the job named, got {details:?}"
    );
    // This emulator mints no Amazon S3 access point, so there is no alias or ARN of
    // one to report rather than an invented one.
    for minted in ["S3AccessPointAlias", "S3AccessPointArn"] {
        assert!(
            access.get(minted).is_none(),
            "case {case}: expected no {minted}, got {details:?}"
        );
    }
}

/// How a list of keys or prefixes the job named is reported: absent when the job named
/// none, because the request carried the member not at all.
fn expected_list(values: &[&str]) -> Value {
    if values.is_empty() {
        Value::Null
    } else {
        json!(values)
    }
}

/// RULE 2: the not-found `Code` stays the answer for a job whose work is to read a
/// source this emulator holds nothing of — the validation `Code` is reported for the
/// grant a data access turns on, not for every job that fails.
#[tokio::test]
async fn a_job_whose_source_this_emulator_holds_nothing_of_still_reports_not_found() {
    let (router, engine) = harness_with_object_storage();
    let (data_set_id, revision_id) = data_access_data_set(&router, "other-job-types").await;

    // Two job types whose sources are resources outside this emulator altogether: an
    // export's signed-url copy of an asset, and a Redshift datashare.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "EXPORT_ASSET_TO_SIGNED_URL",
            "ExportAssetToSignedUrl",
            json!({
                "AssetId": "a1b2c3d4e5f60718293a4b5c6d7e8f90",
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            }),
        ),
        (
            "IMPORT_ASSETS_FROM_REDSHIFT_DATA_SHARES",
            "ImportAssetsFromRedshiftDataShares",
            json!({
                "AssetSources": [{
                    "DataShareArn":
                        format!("arn:aws:redshift:{REGION}:{ACCOUNT}:datashare:cluster/share"),
                }],
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            }),
        ),
    ];

    for (job_type, member, detail) in rows {
        let created = create_job(&router, job_type, json!({ member: detail }), None).await;
        assert_eq!(
            created.status, 201,
            "case {job_type}: expected 201 from CreateJob, got {} body {:?}",
            created.status, created.body
        );
        let job_id = created.string("Id");
        let started = start_job(&router, &job_id).await;
        assert_eq!(
            started.status, 202,
            "case {job_type}: expected 202 from StartJob, got {} body {:?}",
            started.status, started.body
        );
        settle_past_the_execution_span(&engine);
        let settled = get_job(&router, &job_id).await;
        assert_eq!(
            settled.string("State"),
            "ERROR",
            "case {job_type}: expected the execution to settle in ERROR, got {:?}",
            settled.member("State")
        );
        let error = only_error(job_type, &settled);
        assert_eq!(
            error.get("Code").and_then(Value::as_str),
            Some(RESOURCE_NOT_FOUND_EXCEPTION),
            "case {job_type}: expected the modelled not-found Code, got {error:?}"
        );
    }
}

/// The job `Type`s that read objects out of, and write objects into, the object storage,
/// each with the `Details` member the model pairs it with
/// (`smithy_query(query="shape", shape="Type,RequestDetails")`).
const IMPORT_FROM_S3_TYPE: &str = "IMPORT_ASSETS_FROM_S3";
const EXPORT_ASSETS_TYPE: &str = "EXPORT_ASSETS_TO_S3";
const EXPORT_REVISIONS_TYPE: &str = "EXPORT_REVISIONS_TO_S3";

/// The `AssetType` of a data set whose revisions hold Amazon S3 objects, from the
/// model's `AssetType` enum (`smithy_query(query="shape", shape="AssetType")`).
const SNAPSHOT_ASSET_TYPE: &str = "S3_SNAPSHOT";

/// The media type the source objects of these cases declare, so what an exported copy
/// declares can be told apart from whatever a destination would default to.
const SOURCE_MEDIA_TYPE: &str = "text/csv";

/// A bucket name no create ever makes, so the object storage answers it holds none.
const ABSENT_BUCKET: &str = "dataexchange-absent-bucket";

/// One source object of an import: the key it is stored under and the bytes it holds.
struct Source {
    key: &'static str,
    content: &'static str,
}

/// One row of the import table: what the row's bucket is given, what the job names, and
/// whether the job names that bucket at all.
struct ImportRow {
    case: &'static str,
    /// The objects put into the row's own bucket before the job runs.
    stored: &'static [Source],
    /// The keys the job's `AssetSources` name, in that order.
    named: &'static [&'static str],
    /// Whether the job names the row's own bucket; `false` names [`ABSENT_BUCKET`].
    own_bucket: bool,
}

/// The rows of the import table, spanning what rules 4 and 5 generalise over: how many
/// sources one job names, how large the object behind each is (including none at all),
/// the shape of the key, and which source of a job is the missing one.
fn import_rows() -> Vec<ImportRow> {
    vec![
        ImportRow {
            case: "one nested source object the bucket holds",
            stored: &[Source {
                key: "source/orders.csv",
                content: "id,total\n1,10\n",
            }],
            named: &["source/orders.csv"],
            own_bucket: true,
        },
        ImportRow {
            case: "two source objects of different sizes in one job",
            stored: &[
                Source {
                    key: "flat.csv",
                    content: "a",
                },
                Source {
                    key: "deep/nested/feed.json",
                    content: "{\"rows\":[1,2,3,4,5,6,7,8]}",
                },
            ],
            named: &["flat.csv", "deep/nested/feed.json"],
            own_bucket: true,
        },
        ImportRow {
            case: "a source object holding no bytes at all",
            stored: &[Source {
                key: "empty.csv",
                content: "",
            }],
            named: &["empty.csv"],
            own_bucket: true,
        },
        ImportRow {
            case: "a key the bucket does not hold",
            stored: &[Source {
                key: "present.csv",
                content: "present",
            }],
            named: &["absent.csv"],
            own_bucket: true,
        },
        ImportRow {
            case: "one source the bucket holds beside one it does not",
            stored: &[Source {
                key: "present.csv",
                content: "present",
            }],
            named: &["present.csv", "absent.csv"],
            own_bucket: true,
        },
        ImportRow {
            case: "a bucket the object storage does not hold",
            stored: &[],
            named: &["source/orders.csv"],
            own_bucket: false,
        },
    ]
}

/// RULES 4 and 5: an import from Amazon S3 is decided from what the object storage
/// HOLDS. Every source object it answers with becomes one asset of the revision the
/// job's details name — named by that object's own key, carrying the data set's
/// `AssetType`, and sized by the bytes that object holds — and the job completes with no
/// `Errors`. A source the storage answers it does not hold settles the job in `ERROR`
/// with the modelled not-found `Code`, naming that bucket and key in the `Details`
/// member the model gives this job type's errors, and the revision receives nothing.
#[tokio::test]
async fn an_import_from_s3_settles_on_what_the_object_storage_holds() {
    let (router, engine) = harness_with_object_storage();

    for (index, row) in import_rows().iter().enumerate() {
        let case = row.case;
        let bucket = format!("import-source-{index}");
        create_bucket(&router, &bucket).await;
        for source in row.stored {
            put_typed_object(
                &router,
                &bucket,
                source.key,
                SOURCE_MEDIA_TYPE,
                source.content.as_bytes(),
            )
            .await;
        }
        let named_bucket = if row.own_bucket {
            bucket.clone()
        } else {
            ABSENT_BUCKET.to_string()
        };
        let (data_set_id, revision_id) =
            data_set_with_revision(&router, &format!("import-row-{index}"), SNAPSHOT_ASSET_TYPE)
                .await;

        let sources: Vec<Value> = row
            .named
            .iter()
            .map(|key| json!({ "Bucket": named_bucket, "Key": key }))
            .collect();
        let settled = settled_job_of_type(
            &router,
            &engine,
            case,
            IMPORT_FROM_S3_TYPE,
            json!({ "ImportAssetsFromS3": {
                "AssetSources": sources,
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            }}),
        )
        .await;

        // Which of the sources the job named the storage actually holds decides the
        // outcome.
        let held: Vec<&Source> = row
            .named
            .iter()
            .filter_map(|key| {
                row.stored
                    .iter()
                    .find(|source| source.key == *key && row.own_bucket)
            })
            .collect();
        let assets = listed_assets(&router, case, &data_set_id, &revision_id).await;

        if held.len() != row.named.len() {
            assert_eq!(
                settled.string("State"),
                "ERROR",
                "case {case}: expected an import whose source is absent to settle in ERROR, \
                 got {:?}",
                settled.member("State")
            );
            let error = only_error(case, &settled);
            assert_eq!(
                error.get("Code").and_then(Value::as_str),
                Some(RESOURCE_NOT_FOUND_EXCEPTION),
                "case {case}: expected the modelled not-found Code, got {error:?}"
            );
            assert_eq!(
                error.get("Message").and_then(Value::as_str),
                Some(SOURCE_NOT_FOUND_MESSAGE),
                "case {case}: expected the message of a source that cannot be found, got \
                 {error:?}"
            );
            let named_sources = error
                .get("Details")
                .and_then(|details| details.get("ImportAssetsFromS3JobErrorDetails"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                named_sources
                    .iter()
                    .any(|source| source.get("Bucket") == Some(&json!(named_bucket))),
                "case {case}: expected the error to name the source it could not read, got \
                 {error:?}"
            );
            assert!(
                assets.is_empty(),
                "case {case}: expected a failed import to leave the revision empty, got \
                 {assets:?}"
            );
            continue;
        }

        assert_eq!(
            settled.string("State"),
            "COMPLETED",
            "case {case}: expected an import whose sources are all there to COMPLETE, got \
             {:?} with Errors {:?}",
            settled.member("State"),
            settled.member("Errors")
        );
        assert!(
            settled.member("Errors").is_null(),
            "case {case}: expected no Errors on a completed import, got {:?}",
            settled.member("Errors")
        );
        assert_eq!(
            assets.len(),
            row.named.len(),
            "case {case}: expected one asset per source object, got {assets:?}"
        );
        for source in held {
            let entry = assets
                .iter()
                .find(|asset| asset.get("Name") == Some(&json!(source.key)))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected an asset named by the object key {:?}, got \
                         {assets:?}",
                        source.key
                    )
                });
            assert_eq!(
                entry.get("AssetType").and_then(Value::as_str),
                Some(SNAPSHOT_ASSET_TYPE),
                "case {case}: expected the asset to carry its data set's AssetType, got \
                 {entry:?}"
            );
            assert_eq!(
                entry.get("DataSetId").and_then(Value::as_str),
                Some(data_set_id.as_str()),
                "case {case}: expected the asset to belong to the import's data set, got \
                 {entry:?}"
            );
            assert_eq!(
                entry.get("RevisionId").and_then(Value::as_str),
                Some(revision_id.as_str()),
                "case {case}: expected the asset to belong to the import's revision, got \
                 {entry:?}"
            );
            assert_eq!(
                entry
                    .get("AssetDetails")
                    .and_then(|details| details.get("S3SnapshotAsset"))
                    .and_then(|snapshot| snapshot.get("Size"))
                    .and_then(Value::as_f64),
                Some(source.content.len() as f64),
                "case {case}: expected the asset's Size to be the source object's byte \
                 length, got {entry:?}"
            );
        }
    }
}

/// The `Assets` a revision holds, as `ListRevisionAssets` reports them.
async fn listed_assets(
    router: &MultiServiceRouter,
    case: &str,
    data_set_id: &str,
    revision_id: &str,
) -> Vec<Value> {
    let listed = call(
        router,
        "GET",
        &format!("/v1/data-sets/{data_set_id}/revisions/{revision_id}/assets"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected 200 from ListRevisionAssets, got {} body {:?}",
        listed.status, listed.body
    );
    listed
        .member("Assets")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Which export job type a row drives, and the key its destination names.
enum Destination {
    /// `EXPORT_ASSETS_TO_S3`, whose destination names one asset and may name the object
    /// key to write it under.
    Asset { key: Option<&'static str> },
    /// `EXPORT_REVISIONS_TO_S3`, whose destination names the whole revision and may name
    /// the key pattern its assets are written under.
    Revision { key_pattern: Option<&'static str> },
}

/// One row of the export table.
struct ExportRow {
    case: &'static str,
    destination: Destination,
    /// Whether the destination is the bucket this row created; `false` names
    /// [`ABSENT_BUCKET`].
    own_bucket: bool,
}

/// The rows of the export table, spanning what rule 6 generalises over: both export job
/// types, each with and without the key its destination may name, and a destination the
/// object storage does not hold.
fn export_rows() -> Vec<ExportRow> {
    vec![
        ExportRow {
            case: "an asset destination naming its own object key",
            destination: Destination::Asset {
                key: Some("exports/out.csv"),
            },
            own_bucket: true,
        },
        ExportRow {
            case: "an asset destination naming no key",
            destination: Destination::Asset { key: None },
            own_bucket: true,
        },
        ExportRow {
            case: "a revision destination naming a key pattern",
            destination: Destination::Revision {
                key_pattern: Some("${Revision.Id}/${Asset.Name}"),
            },
            own_bucket: true,
        },
        ExportRow {
            case: "a revision destination naming no key pattern",
            destination: Destination::Revision { key_pattern: None },
            own_bucket: true,
        },
        ExportRow {
            case: "an asset destination in a bucket the object storage does not hold",
            destination: Destination::Asset {
                key: Some("exports/out.csv"),
            },
            own_bucket: false,
        },
    ]
}

/// The key every export row's asset is imported from, and the bytes behind it — one
/// object, so each row is about WHERE the export lands rather than how many there are.
const EXPORTED_KEY: &str = "source/orders.csv";
const EXPORTED_CONTENT: &str = "id,total\n1,10\n2,20\n";

/// RULE 6: an export writes the content of the assets its details name into the
/// destination bucket — under the `Key` or key pattern the destination names, and under
/// the asset's own `Name` when it names none — so a client reads the same bytes, with
/// the same media type the source declared, back out of the object storage. A
/// destination bucket the storage does not hold settles the job in `ERROR` naming it,
/// and leaves no object behind.
#[tokio::test]
async fn an_export_to_s3_writes_the_imported_content_back_into_the_bucket() {
    let (router, engine) = harness_with_object_storage();
    let source_bucket = "export-source-bucket";
    create_bucket(&router, source_bucket).await;
    put_typed_object(
        &router,
        source_bucket,
        EXPORTED_KEY,
        SOURCE_MEDIA_TYPE,
        EXPORTED_CONTENT.as_bytes(),
    )
    .await;

    for (index, row) in export_rows().iter().enumerate() {
        let case = row.case;
        let bucket = format!("export-destination-{index}");
        create_bucket(&router, &bucket).await;
        let (data_set_id, revision_id) =
            data_set_with_revision(&router, &format!("export-row-{index}"), SNAPSHOT_ASSET_TYPE)
                .await;

        // The asset an export writes out is one an import created, which is the only way
        // a revision comes to hold one.
        let imported = settled_job_of_type(
            &router,
            &engine,
            case,
            IMPORT_FROM_S3_TYPE,
            json!({ "ImportAssetsFromS3": {
                "AssetSources": [{ "Bucket": source_bucket, "Key": EXPORTED_KEY }],
                "DataSetId": data_set_id,
                "RevisionId": revision_id,
            }}),
        )
        .await;
        assert_eq!(
            imported.string("State"),
            "COMPLETED",
            "case {case}: the import the export reads from must COMPLETE, got {:?} with \
             Errors {:?}",
            imported.member("State"),
            imported.member("Errors")
        );
        let assets = listed_assets(&router, case, &data_set_id, &revision_id).await;
        let asset = assets
            .first()
            .unwrap_or_else(|| panic!("case {case}: the import must leave one asset"));
        let asset_id = asset
            .get("Id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let named_bucket = if row.own_bucket {
            bucket.clone()
        } else {
            ABSENT_BUCKET.to_string()
        };
        let (job_type, details, expected_key) = match &row.destination {
            Destination::Asset { key } => {
                let mut destination = json!({ "AssetId": asset_id, "Bucket": named_bucket });
                if let Some(key) = key {
                    destination["Key"] = json!(key);
                }
                (
                    EXPORT_ASSETS_TYPE,
                    json!({ "ExportAssetsToS3": {
                        "AssetDestinations": [destination],
                        "DataSetId": data_set_id,
                        "RevisionId": revision_id,
                    }}),
                    // A destination naming no key writes the asset under its own name,
                    // which is the key it was imported from.
                    key.unwrap_or(EXPORTED_KEY).to_string(),
                )
            }
            Destination::Revision { key_pattern } => {
                let mut destination = json!({ "Bucket": named_bucket, "RevisionId": revision_id });
                if let Some(pattern) = key_pattern {
                    destination["KeyPattern"] = json!(pattern);
                }
                (
                    EXPORT_REVISIONS_TYPE,
                    json!({ "ExportRevisionsToS3": {
                        "DataSetId": data_set_id,
                        "RevisionDestinations": [destination],
                    }}),
                    // A destination naming no pattern writes each asset under its own
                    // name; one naming a pattern writes it under what that pattern's
                    // variables stand for.
                    key_pattern.map_or(EXPORTED_KEY.to_string(), |pattern| {
                        pattern
                            .replace("${Revision.Id}", &revision_id)
                            .replace("${Asset.Name}", EXPORTED_KEY)
                    }),
                )
            }
        };

        let settled = settled_job_of_type(&router, &engine, case, job_type, details).await;

        if !row.own_bucket {
            assert_eq!(
                settled.string("State"),
                "ERROR",
                "case {case}: expected an export into a bucket that is not there to settle \
                 in ERROR, got {:?}",
                settled.member("State")
            );
            let error = only_error(case, &settled);
            assert_eq!(
                error.get("Code").and_then(Value::as_str),
                Some(RESOURCE_NOT_FOUND_EXCEPTION),
                "case {case}: expected the modelled not-found Code, got {error:?}"
            );
            assert!(
                error
                    .get("Message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(ABSENT_BUCKET)),
                "case {case}: expected the error to name the destination bucket, got \
                 {error:?}"
            );
            // And nothing of it reached the bucket that does exist.
            let unwritten = get_object(&router, &bucket, &expected_key).await;
            assert_eq!(
                unwritten.status, 404,
                "case {case}: expected a failed export to write no object, got {} with {:?}",
                unwritten.status,
                String::from_utf8_lossy(&unwritten.body)
            );
            continue;
        }

        assert_eq!(
            settled.string("State"),
            "COMPLETED",
            "case {case}: expected an export whose destination is there to COMPLETE, got \
             {:?} with Errors {:?}",
            settled.member("State"),
            settled.member("Errors")
        );
        let exported = get_object(&router, &bucket, &expected_key).await;
        assert_eq!(
            exported.status, 200,
            "case {case}: expected the exported object at {expected_key:?} to be readable, \
             got {} with {:?}",
            exported.status,
            String::from_utf8_lossy(&exported.body)
        );
        assert_eq!(
            String::from_utf8_lossy(&exported.body),
            EXPORTED_CONTENT,
            "case {case}: expected the exported object to carry the asset's content, got \
             {:?}",
            String::from_utf8_lossy(&exported.body)
        );
        assert_eq!(
            exported.content_type.as_deref(),
            Some(SOURCE_MEDIA_TYPE),
            "case {case}: expected the exported copy to declare the media type the source \
             declared, got {:?}",
            exported.content_type
        );
    }
}
