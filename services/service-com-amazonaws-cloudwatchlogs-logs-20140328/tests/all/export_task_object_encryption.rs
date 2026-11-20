// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SERVER-SIDE ENCRYPTION the objects an export task writes carry.
//!
//! CloudWatch Logs, Amazon S3 and KMS are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so every bucket, default encryption rule, KMS key and object here
//! is made with the owning service's own operations, and the objects a case reads back are the
//! ones the export task wrote over the in-process callback.
//!
//! The rule under test: an export task writes its objects to the destination bucket WITHOUT
//! naming any encryption of its own, so each object is stored under the destination bucket's
//! own default encryption configuration — the configuration in force on that bucket when the
//! object is written, which for a bucket no `PutBucketEncryption` has configured is the SSE-S3
//! base default ("Amazon S3 encrypts data with server-side encryption by using Amazon S3
//! managed keys (SSE-S3) by default. You can optionally tell Amazon S3 to encrypt data at rest
//! by using server-side encryption with other key options",
//! `PutObjectRequest$ServerSideEncryption`, the other key options being the ones
//! `PutBucketEncryption` configures). Nothing of the LOG GROUP's own encryption enters into it:
//! a group's `kmsKeyId` "is the ARN of the KMS key to use when encrypting log data" of that
//! group (`AssociateKmsKey`), not of an object another service stores. So a reader of an
//! exported object sees the destination bucket's own `x-amz-server-side-encryption` headers,
//! and the group reports the same `kmsKeyId` afterwards.
//!
//! The table spans what the rule generalises over: whether the destination bucket was
//! configured at all, which `SSEAlgorithm` its default names (SSE-S3, and both
//! `ServerSideEncryption` values a KMS key belongs to), whether that default names a KMS key
//! of the account, whether the S3 Bucket Key is on, off or unnamed, a rule that names no
//! algorithm at all, and whether the exported log group holds a customer managed key of its
//! own.
//!
//! Every case waits for the terminal status the way a client does — poll
//! `DescribeExportTasks` until it is terminal — and then reads the bucket, so nothing here
//! waits for a fixed span of the clock.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::export_task_objects::{WRITE_TEST_BODY, WRITE_TEST_OBJECT};
use crate::harness::now_ms;

/// The Region every request here is signed for, which is the Region the ARNs carry.
const REGION: &str = "eu-west-2";

/// The host a path-style S3 request names.
const HOST: &str = "s3.eu-west-2.amazonaws.com";

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// The XML namespace an S3 request payload carries.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The header an object's encryption algorithm is reported in, and the two that qualify it
/// — the `@httpHeader` members `PutObject`, `GetObject` and `HeadObject` bind
/// (`smithy_query(query="operation",operation="HeadObject")` reports `ServerSideEncryption:
/// {httpHeader: "x-amz-server-side-encryption"}`, `SSEKMSKeyId:
/// {httpHeader: "x-amz-server-side-encryption-aws-kms-key-id"}` and `BucketKeyEnabled:
/// {httpHeader: "x-amz-server-side-encryption-bucket-key-enabled"}`).
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";
const SSE_BUCKET_KEY_HEADER: &str = "x-amz-server-side-encryption-bucket-key-enabled";

/// The `ServerSideEncryption` value that is SSE-S3 — "server-side encryption with Amazon S3
/// managed keys", the base default every bucket carries until one is configured
/// (`smithy_query(query="shape",shape="ServerSideEncryption")` publishes it).
const SSE_S3: &str = "AES256";

/// The two `ServerSideEncryption` values a KMS key id belongs to: "this parameter is allowed if
/// and only if SSEAlgorithm is set to aws:kms or aws:kms:dsse"
/// (`ApplyServerSideEncryptionByDefault$KMSMasterKeyID`).
const SSE_KMS: &str = "aws:kms";
const SSE_DSSE_KMS: &str = "aws:kms:dsse";

/// How often, and how many times, a case polls for an export task's terminal status. The
/// product is far past the spans an export is under way for, so a settled export is never
/// missed and no case waits for a fixed duration instead of for the status itself.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_ATTEMPTS: usize = 600;

/// The statuses of the model's `ExportTaskStatusCode` enum an export task never leaves.
const TERMINAL_STATUSES: [&str; 3] = ["COMPLETED", "CANCELLED", "FAILED"];

/// CloudWatch Logs, Amazon S3 and KMS on one shared engine, reached over the wire.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope is still carried, because it is what names the Region.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// The authorization header naming `service`'s own credential scope.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        )
    }

    /// One awsJson1_1 call to the service signing as `service` under `prefix`.
    async fn json(
        &self,
        service: &str,
        prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{prefix}.{operation}"))
            .header("authorization", Self::authorization(service))
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.json("logs", "Logs_20140328", operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {body} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// A customer managed key carrying `policy`, answering its ARN.
    async fn created_key(&self, case: &str, policy: &str) -> String {
        let (status, parsed) = self
            .json(
                "kms",
                "TrentService",
                "CreateKey",
                &json!({"Policy": policy, "BypassPolicyLockoutSafetyCheck": true}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateKey must succeed, got {status} {parsed}"
        );
        parsed["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a created key carries an Arn, got {parsed}"))
            .to_string()
    }

    /// One path-style Amazon S3 request: its status, its response headers and its body bytes.
    async fn s3(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, bytes::Bytes) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{HOST}{path}"))
            .header("host", HOST)
            .header("authorization", Self::authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        (status, headers, response.into_body())
    }

    /// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region this suite signs for, followed
    /// by the policy an export destination has to carry
    /// ([`crate::export_task_objects::export_destination_policy`]): `CreateExportTask` checks
    /// that the destination's own policy lets CloudWatch Logs reach it before it mints a task.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let (status, _headers, body) = self
            .s3(
                "PUT",
                &format!("/{bucket}"),
                format!(
                    "<CreateBucketConfiguration xmlns=\"{XMLNS}\">\
                     <LocationConstraint>{REGION}</LocationConstraint>\
                     </CreateBucketConfiguration>"
                )
                .as_bytes(),
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: create bucket {bucket}: got {}",
            String::from_utf8_lossy(&body)
        );
        let (status, _headers, body) = self
            .s3(
                "PUT",
                &format!("/{bucket}?policy"),
                crate::export_task_objects::export_destination_policy(bucket, REGION).as_bytes(),
            )
            .await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: grant the export delivery on {bucket}: got {status} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// `PUT /<bucket>?encryption` — S3's `PutBucketEncryption`, carrying `payload`.
    async fn put_bucket_encryption(&self, case: &str, bucket: &str, payload: &str) {
        let (status, _headers, body) = self
            .s3("PUT", &format!("/{bucket}?encryption"), payload.as_bytes())
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: configure {bucket}'s default encryption with {payload}: got {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Every key under `prefix`, read through `ListObjectsV2`.
    async fn keys_under(&self, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
        let (status, _headers, body) = self
            .s3(
                "GET",
                &format!("/{bucket}?list-type=2&prefix={prefix}"),
                b"",
            )
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: list {bucket}/{prefix}: got {}",
            String::from_utf8_lossy(&body)
        );
        let document = String::from_utf8_lossy(&body).into_owned();
        document
            .split("<Key>")
            .skip(1)
            .filter_map(|tail| tail.split("</Key>").next())
            .map(str::to_string)
            .collect()
    }

    /// `HEAD /<bucket>/<key>` — the metadata S3 reports for one stored object, which is where
    /// its encryption headers are read from.
    async fn head_object(&self, case: &str, bucket: &str, key: &str) -> http::HeaderMap {
        let (status, headers, _body) = self.s3("HEAD", &format!("/{bucket}/{key}"), b"").await;
        assert_eq!(
            status, 200,
            "case {case:?}: head {bucket}/{key}: got {status} with {headers:?}"
        );
        headers
    }

    /// `GET /<bucket>/<key>` — the object's own headers and bytes.
    async fn get_object(&self, case: &str, bucket: &str, key: &str) -> (http::HeaderMap, Vec<u8>) {
        let (status, headers, body) = self.s3("GET", &format!("/{bucket}/{key}"), b"").await;
        assert_eq!(
            status,
            200,
            "case {case:?}: get {bucket}/{key}: got {}",
            String::from_utf8_lossy(&body)
        );
        (headers, body.to_vec())
    }

    /// The status code `DescribeExportTasks` reports for `task_id`.
    async fn task_status(&self, case: &str, task_id: &str) -> String {
        let page = self
            .logs_ok(case, "DescribeExportTasks", &json!({"taskId": task_id}))
            .await;
        page["exportTasks"][0]["status"]["code"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: an export task reports a status, got {page}"))
            .to_string()
    }

    /// Poll `DescribeExportTasks` until `task_id` reports a terminal status, answering it —
    /// the wait a client makes, on the CONDITION rather than on the clock.
    async fn awaited_terminal_status(&self, case: &str, task_id: &str) -> String {
        for _ in 0..POLL_ATTEMPTS {
            let status = self.task_status(case, task_id).await;
            if TERMINAL_STATUSES.contains(&status.as_str()) {
                return status;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        panic!("case {case:?}: the export task {task_id} must reach a terminal status")
    }

    /// The `kmsKeyId` CloudWatch Logs reports for `group`, or `None` when it reports none.
    async fn group_key(&self, case: &str, group: &str) -> Option<String> {
        let page = self
            .logs_ok(
                case,
                "DescribeLogGroups",
                &json!({"logGroupIdentifiers": [group]}),
            )
            .await;
        page["logGroups"][0]
            .get("kmsKeyId")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The value `headers` carries under `name`, or `None` when it carries none.
fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// A key policy granting CloudWatch Logs the encryption operations for every log group of the
/// account, beside the statement that keeps the key manageable by its account, as KMS's
/// default key policy words it. This is what lets a group's `kmsKeyId` be associated at all,
/// so a row whose group holds a key creates it with this.
fn logs_key_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
                "Action": "kms:*",
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Principal": {"Service": "logs.amazonaws.com"},
                "Action": [
                    "kms:Encrypt*",
                    "kms:Decrypt*",
                    "kms:ReEncrypt*",
                    "kms:GenerateDataKey*",
                    "kms:Describe*",
                ],
                "Resource": "*",
                "Condition": {"ArnLike": {"kms:EncryptionContext:aws:logs:arn": format!(
                    "arn:aws:logs:{REGION}:{}:log-group:*",
                    engine::emulator_account_id()
                )}},
            },
        ],
    })
    .to_string()
}

/// A key policy carrying the account statement alone — the one a key created with the default
/// policy carries, which is what lets this account's own callers store and read an object
/// under the key.
fn account_key_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "Enable IAM User Permissions",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id())},
            "Action": "kms:*",
            "Resource": "*",
        }],
    })
    .to_string()
}

/// The `ServerSideEncryptionConfiguration` payload of one rule, spelled the way a client's XML
/// writer spells it: `ServerSideEncryptionConfiguration$Rules` is `xmlFlattened` with `xmlName`
/// `Rule` (`smithy_query(query="shape",shape="ServerSideEncryptionConfiguration")`).
fn encryption_payload(
    algorithm: Option<&str>,
    key: Option<&str>,
    bucket_key: Option<bool>,
) -> String {
    let mut body = format!("<ServerSideEncryptionConfiguration xmlns=\"{XMLNS}\"><Rule>");
    if let Some(algorithm) = algorithm {
        body.push_str("<ApplyServerSideEncryptionByDefault>");
        body.push_str(&format!("<SSEAlgorithm>{algorithm}</SSEAlgorithm>"));
        if let Some(key) = key {
            body.push_str(&format!("<KMSMasterKeyID>{key}</KMSMasterKeyID>"));
        }
        body.push_str("</ApplyServerSideEncryptionByDefault>");
    }
    if let Some(enabled) = bucket_key {
        body.push_str(&format!("<BucketKeyEnabled>{enabled}</BucketKeyEnabled>"));
    }
    body.push_str("</Rule></ServerSideEncryptionConfiguration>");
    body
}

/// What the DESTINATION bucket's default encryption configuration is for one row.
#[derive(Debug, Clone, Copy)]
enum BucketDefault {
    /// No `PutBucketEncryption` at all, so the bucket stands on the SSE-S3 base default.
    Unconfigured,
    /// A rule naming an `SSEAlgorithm` that needs no key of the account.
    Algorithm(&'static str),
    /// A rule naming one of the KMS algorithms, the bucket's OWN customer managed key, and
    /// the Bucket Key setting when it names one.
    KmsKey(&'static str, Option<bool>),
    /// A rule naming only `BucketKeyEnabled` — `ServerSideEncryptionRule` makes the
    /// `ApplyServerSideEncryptionByDefault` container optional, so this configures a bucket
    /// that names no default algorithm at all.
    BucketKeyOnly(bool),
}

/// One row of the table.
struct Row {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The destination bucket's default encryption configuration.
    bucket_default: BucketDefault,
    /// Whether the exported LOG GROUP holds a customer managed key of its own, which must
    /// decide nothing about the objects.
    group_holds_key: bool,
    /// The `x-amz-server-side-encryption` every exported object must report.
    expected_algorithm: &'static str,
    /// Whether that object must report the BUCKET's own key as its
    /// `x-amz-server-side-encryption-aws-kms-key-id`; a row expecting none expects the header
    /// to be absent.
    expects_bucket_key_id: bool,
    /// Whether the bucket default turns the S3 Bucket Key ON, which is what the object must
    /// report under `x-amz-server-side-encryption-bucket-key-enabled`: "setting this header to
    /// true causes Amazon S3 to use an S3 Bucket Key for object encryption with SSE-KMS"
    /// (`PutObjectRequest$BucketKeyEnabled`), so a row whose default leaves it off must not
    /// report an object whose Bucket Key is on.
    expects_bucket_key: bool,
}

/// The table. Every row exports one event and reads the object back.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a destination bucket no PutBucketEncryption configured",
            bucket_default: BucketDefault::Unconfigured,
            group_holds_key: false,
            expected_algorithm: SSE_S3,
            expects_bucket_key_id: false,
            expects_bucket_key: false,
        },
        Row {
            case: "the bucket's own SSE-S3 default, over a log group holding its own CMK",
            bucket_default: BucketDefault::Algorithm(SSE_S3),
            group_holds_key: true,
            expected_algorithm: SSE_S3,
            expects_bucket_key_id: false,
            expects_bucket_key: false,
        },
        Row {
            case: "the bucket's own SSE-KMS default, over a log group holding another CMK",
            bucket_default: BucketDefault::KmsKey(SSE_KMS, None),
            group_holds_key: true,
            expected_algorithm: SSE_KMS,
            expects_bucket_key_id: true,
            expects_bucket_key: false,
        },
        Row {
            case: "the bucket's SSE-KMS default with the Bucket Key off",
            bucket_default: BucketDefault::KmsKey(SSE_KMS, Some(false)),
            group_holds_key: false,
            expected_algorithm: SSE_KMS,
            expects_bucket_key_id: true,
            expects_bucket_key: false,
        },
        Row {
            case: "the bucket's dual-layer SSE-KMS default with the Bucket Key on",
            bucket_default: BucketDefault::KmsKey(SSE_DSSE_KMS, Some(true)),
            group_holds_key: true,
            expected_algorithm: SSE_DSSE_KMS,
            expects_bucket_key_id: true,
            expects_bucket_key: true,
        },
        Row {
            case: "a rule naming only the Bucket Key setting, so the base default stands",
            bucket_default: BucketDefault::BucketKeyOnly(true),
            group_holds_key: false,
            expected_algorithm: SSE_S3,
            expects_bucket_key_id: false,
            expects_bucket_key: false,
        },
    ]
}

/// The message every row plants and must read back out of the exported object.
const MESSAGE: &str = "exported-event";

#[tokio::test]
async fn an_exported_object_carries_the_destination_buckets_own_encryption() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let bucket = "export-encryption-destination";
        let group = "export/encryption";
        let stream = "app/one";
        let prefix = "exported";

        emulator.create_bucket(case, bucket).await;
        // The bucket's own default encryption, configured through S3's own operation — and the
        // KMS key it names, when it names one, created through KMS's own.
        let bucket_key_arn = match row.bucket_default {
            BucketDefault::Unconfigured => None,
            BucketDefault::Algorithm(algorithm) => {
                emulator
                    .put_bucket_encryption(
                        case,
                        bucket,
                        &encryption_payload(Some(algorithm), None, None),
                    )
                    .await;
                None
            }
            BucketDefault::KmsKey(algorithm, bucket_key) => {
                let key = emulator.created_key(case, &account_key_policy()).await;
                emulator
                    .put_bucket_encryption(
                        case,
                        bucket,
                        &encryption_payload(Some(algorithm), Some(&key), bucket_key),
                    )
                    .await;
                Some(key)
            }
            BucketDefault::BucketKeyOnly(enabled) => {
                emulator
                    .put_bucket_encryption(
                        case,
                        bucket,
                        &encryption_payload(None, None, Some(enabled)),
                    )
                    .await;
                None
            }
        };

        // The log group, with a customer managed key of its own for the rows that hold one.
        let mut create_group = json!({"logGroupName": group});
        let group_key_arn = if row.group_holds_key {
            let key = emulator.created_key(case, &logs_key_policy()).await;
            create_group["kmsKeyId"] = json!(key);
            Some(key)
        } else {
            None
        };
        emulator.logs_ok(case, "CreateLogGroup", &create_group).await;
        assert_eq!(
            emulator.group_key(case, group).await,
            group_key_arn,
            "case {case:?}: the group must hold the key the row gave it"
        );
        emulator
            .logs_ok(
                case,
                "CreateLogStream",
                &json!({"logGroupName": group, "logStreamName": stream}),
            )
            .await;
        let clock = now_ms();
        emulator
            .logs_ok(
                case,
                "PutLogEvents",
                &json!({
                    "logGroupName": group,
                    "logStreamName": stream,
                    "logEvents": [{"timestamp": clock - HOUR, "message": MESSAGE}],
                }),
            )
            .await;

        let started = emulator
            .logs_ok(
                case,
                "CreateExportTask",
                &json!({
                    "logGroupName": group,
                    "from": clock - 3 * HOUR,
                    "to": clock,
                    "destination": bucket,
                    "destinationPrefix": prefix,
                }),
            )
            .await;
        let task_id = started["taskId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a started export reports a taskId"))
            .to_string();
        let status = emulator.awaited_terminal_status(case, &task_id).await;
        assert_eq!(
            status, "COMPLETED",
            "case {case:?}: the export must complete, got {status}"
        );

        let keys = emulator.keys_under(case, bucket, prefix).await;
        assert!(
            !keys.is_empty(),
            "case {case:?}: the export must write an object under {prefix}/, got {keys:?}"
        );
        for key in &keys {
            // What a reader sees on the metadata read, and on the read that serves the bytes:
            // both report the encryption the object is STORED under.
            for (read, headers) in [
                ("HeadObject", emulator.head_object(case, bucket, key).await),
                ("GetObject", emulator.get_object(case, bucket, key).await.0),
            ] {
                assert_eq!(
                    header(&headers, SSE_HEADER).as_deref(),
                    Some(row.expected_algorithm),
                    "case {case:?}: {read} of {key} must report the destination bucket's own \
                     encryption, got {:?}",
                    header(&headers, SSE_HEADER)
                );
                let reported_key = header(&headers, SSE_KEY_HEADER);
                let expected_key = row.expects_bucket_key_id.then(|| {
                    bucket_key_arn
                        .clone()
                        .expect("a row expecting a key id configured one")
                });
                assert_eq!(
                    reported_key, expected_key,
                    "case {case:?}: {read} of {key} must report the bucket default's own KMS \
                     key, got {reported_key:?}"
                );
                let reported_bucket_key = header(&headers, SSE_BUCKET_KEY_HEADER);
                assert_eq!(
                    reported_bucket_key.as_deref() == Some("true"),
                    row.expects_bucket_key,
                    "case {case:?}: {read} of {key} must report the bucket default's Bucket Key \
                     setting (on: {}), got {reported_bucket_key:?}",
                    row.expects_bucket_key
                );
                // The log group's own key encrypts that group's stored log data and nothing an
                // object store holds, so it names none of the object's encryption.
                if let Some(group_key) = &group_key_arn {
                    assert_ne!(
                        reported_key.as_deref(),
                        Some(group_key.as_str()),
                        "case {case:?}: {read} of {key} must not report the LOG GROUP's key"
                    );
                }
            }

            // And the bytes under those headers are the exported log data — in the objects
            // that hold it: the one object an export writes that is not exported log data is
            // the check that the destination takes its writes at all
            // ([`crate::export_task_objects::WRITE_TEST_OBJECT`]), which carries the same
            // encryption and plain text rather than a gzip frame.
            let (_headers, body) = emulator.get_object(case, bucket, key).await;
            if key.ends_with(WRITE_TEST_OBJECT) {
                assert_eq!(
                    String::from_utf8_lossy(&body),
                    WRITE_TEST_BODY,
                    "case {case:?}: the object at {key} is the export's write check"
                );
                continue;
            }
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(body.as_slice())
                .read_to_end(&mut decoded)
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the object at {key} must be gzip: {error}")
                });
            let text = String::from_utf8_lossy(&decoded).into_owned();
            assert!(
                text.contains(MESSAGE),
                "case {case:?}: the object at {key} must carry the exported event, got {text:?}"
            );
        }

        // The export moved nothing about the group's own encryption.
        assert_eq!(
            emulator.group_key(case, group).await,
            group_key_arn,
            "case {case:?}: the group must still report the key it held before the export"
        );
    }
}

/// One whole export of `group`'s window into `bucket` under `prefix`, awaited to `COMPLETED`.
async fn exported_into(
    emulator: &Emulator,
    case: &str,
    group: &str,
    bucket: &str,
    prefix: &str,
    clock: i64,
) {
    let started = emulator
        .logs_ok(
            case,
            "CreateExportTask",
            &json!({
                "logGroupName": group,
                "from": clock - 3 * HOUR,
                "to": clock,
                "destination": bucket,
                "destinationPrefix": prefix,
            }),
        )
        .await;
    let task_id = started["taskId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a started export reports a taskId"))
        .to_string();
    let status = emulator.awaited_terminal_status(case, &task_id).await;
    assert_eq!(
        status, "COMPLETED",
        "case {case:?}: the export into {prefix}/ must complete, got {status}"
    );
}

/// Each exported object carries the configuration in force on the destination bucket when IT
/// was written: a bucket reconfigured between two exports leaves the first export's object
/// exactly as it was stored and applies the new default to the second.
#[tokio::test]
async fn each_export_carries_the_bucket_default_in_force_when_it_was_written() {
    let case = "the bucket's default encryption changed between two exports";
    let emulator = Emulator::new();
    let bucket = "export-encryption-reconfigured";
    let group = "export/reconfigured";
    let stream = "app/one";

    emulator.create_bucket(case, bucket).await;
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": group}))
        .await;
    emulator
        .logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
    let clock = now_ms();
    emulator
        .logs_ok(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": [{"timestamp": clock - HOUR, "message": MESSAGE}],
            }),
        )
        .await;

    // One export under the bucket's base default, then — "each account can only have one
    // active (RUNNING or PENDING) export task at a time" — a second one once it has settled,
    // under the SSE-KMS default configured in between.
    exported_into(&emulator, case, group, bucket, "before", clock).await;
    let key_arn = emulator.created_key(case, &account_key_policy()).await;
    emulator
        .put_bucket_encryption(
            case,
            bucket,
            &encryption_payload(Some(SSE_KMS), Some(&key_arn), Some(true)),
        )
        .await;
    exported_into(&emulator, case, group, bucket, "after", clock).await;

    for (prefix, algorithm, key_id, bucket_key) in [
        ("before", SSE_S3, None, false),
        ("after", SSE_KMS, Some(key_arn.as_str()), true),
    ] {
        let keys = emulator.keys_under(case, bucket, prefix).await;
        assert!(
            !keys.is_empty(),
            "case {case:?}: the export into {prefix}/ must write an object, got {keys:?}"
        );
        for key in &keys {
            let headers = emulator.head_object(case, bucket, key).await;
            assert_eq!(
                (
                    header(&headers, SSE_HEADER).as_deref(),
                    header(&headers, SSE_KEY_HEADER).as_deref(),
                    header(&headers, SSE_BUCKET_KEY_HEADER).as_deref() == Some("true"),
                ),
                (Some(algorithm), key_id, bucket_key),
                "case {case:?}: the object at {key} must carry the default in force when it \
                 was written, got {headers:?}"
            );
        }
    }
}
