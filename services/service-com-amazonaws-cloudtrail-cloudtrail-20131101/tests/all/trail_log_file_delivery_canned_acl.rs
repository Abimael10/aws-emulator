// SPDX-License-Identifier: CC0-1.0

//! WHAT a trail's log-file write ASKS FOR on the object it delivers — the canned ACL that
//! leaves the bucket's owner owning the file.
//!
//! RULE: CloudTrail delivers each log file as an object write naming the
//! `bucket-owner-full-control` canned ACL — the very ACL the destination policy CloudTrail
//! documents conditions its `s3:PutObject` grant on — so a destination whose policy admits
//! only writes that name it, and one that DENIES every write that does not, both receive the
//! trail's log files. The ACL is a fact of the write, not of the grant: CloudTrail asks a
//! destination to permit a write carrying it before accepting a trail, so the write it then
//! makes carries it.
//!
//! Two services are mounted on ONE engine, because the behaviour spans them: CloudTrail
//! serves the calls and delivers the log files, and the object store decides the write and
//! holds what landed. CloudTrail is driven over the wire, the way a client drives it; the
//! bucket is created, policied and listed through Amazon S3's OWN API over the same in-process
//! callback a handler uses — so a delivered log file counts only if the object store itself
//! answers with it.
//!
//! The rows span the destination policies a client writes over that ACL: the documented policy
//! alone, the documented policy hardened with a deny of every write that does not name the
//! ACL, the documented policy hardened with a deny of every write not made over TLS (the other
//! clause such a hardened destination carries), and — as the control that the deny clauses are
//! really decided against the delivery's own write — a destination that denies exactly the
//! write CloudTrail would make, which is a destination the trail is never accepted against.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::emulator::TARGET_PREFIX;

use service_com_amazonaws_cloudtrail_cloudtrail_20131101::state::referenced_resources;

/// The Region every call below is signed for, which is the Region the trails are homed in and
/// the Region their log files are delivered under.
const REGION: &str = "us-east-1";

/// What a log file's name ends with, so an object a suite put beside the deliveries is not
/// read as one.
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The canned ACL a CloudTrail log-file write names. Spelled out here rather than imported, so
/// a wrong constant in the implementation cannot make this suite pass.
const DELIVERY_CANNED_ACL: &str = "bucket-owner-full-control";

/// The error CloudTrail refuses a destination it may not deliver into with.
const INSUFFICIENT_POLICY_CODE: &str = "InsufficientS3BucketPolicyException";

/// CloudTrail and the object store over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_s3_amazons3::EmulatedService::new(Arc::clone(&engine)),
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// One CloudTrail call (`awsJson1_1`), answering the HTTP status and the parsed body.
    async fn cloudtrail(&self, operation: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/cloudtrail/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("cloudtrail.{REGION}.amazonaws.com"))
            .header("x-amz-target", format!("{TARGET_PREFIX}{operation}"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("authorization", authorization)
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One CloudTrail call that has to be served.
    async fn served(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.cloudtrail(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// The Amazon S3 client, over the same in-process callback the delivery uses — built by
    /// the same shared constructor, so the suite's transport is the delivery's by
    /// construction.
    fn s3(&self) -> aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket` and attach `policy` — the `CreateBucket` and `PutBucketPolicy` a client
    /// makes before pointing a trail at it.
    async fn log_bucket(&self, bucket: &str, policy: &str) {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
        self.s3()
            .put_bucket_policy()
            .bucket(bucket)
            .policy(policy)
            .send()
            .await
            .unwrap_or_else(|error| panic!("policying {bucket} must succeed, got {error:?}"));
    }

    /// The log files `bucket` now holds, as Amazon S3 lists them.
    async fn log_files(&self, bucket: &str) -> Vec<String> {
        let listed = self
            .s3()
            .list_objects_v2()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("listing {bucket} must succeed, got {error:?}"));
        let mut keys: Vec<String> = listed
            .contents()
            .iter()
            .filter_map(|object| object.key().map(str::to_string))
            .filter(|key| key.ends_with(LOG_FILE_SUFFIX))
            .collect();
        keys.sort();
        keys
    }
}

/// This account's id, as the policies below name it in their resource ARNs.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The two statements CloudTrail's own documentation gives a destination bucket: the ACL check
/// on the bucket, and the object write under `AWSLogs/<account-id>/` granted to the CloudTrail
/// service principal and scoped by the canned ACL the delivery names.
fn documented_statements(bucket: &str) -> Vec<Value> {
    vec![
        json!({
            "Sid": "AWSCloudTrailAclCheck",
            "Effect": "Allow",
            "Principal": {"Service": "cloudtrail.amazonaws.com"},
            "Action": "s3:GetBucketAcl",
            "Resource": format!("arn:aws:s3:::{bucket}"),
        }),
        json!({
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": {"Service": "cloudtrail.amazonaws.com"},
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/AWSLogs/{}/*", account()),
            "Condition": {"StringEquals": {"s3:x-amz-acl": DELIVERY_CANNED_ACL}},
        }),
    ]
}

/// One destination policy this suite points a trail at, and what the trail then does.
struct Row {
    case: &'static str,
    /// The statements beside the documented pair.
    hardening: fn(&str) -> Vec<Value>,
    /// Whether CloudTrail accepts a trail delivering into it at all.
    accepted: bool,
}

/// No clause beside the documented pair.
fn no_hardening(_bucket: &str) -> Vec<Value> {
    Vec::new()
}

/// The clause a hardened destination carries about the ACL: every write that does NOT leave
/// the bucket's owner owning the object is refused, whoever makes it.
fn deny_writes_that_are_not_owner_granting(bucket: &str) -> Vec<Value> {
    vec![json!({
        "Sid": "DenyWritesThatAreNotOwnerGranting",
        "Effect": "Deny",
        "Principal": "*",
        "Action": "s3:PutObject",
        "Resource": format!("arn:aws:s3:::{bucket}/*"),
        "Condition": {"StringNotEquals": {"s3:x-amz-acl": DELIVERY_CANNED_ACL}},
    })]
}

/// The other clause such a destination carries: every write not made over TLS is refused.
fn deny_writes_outside_tls(bucket: &str) -> Vec<Value> {
    vec![json!({
        "Sid": "DenyWritesOutsideTls",
        "Effect": "Deny",
        "Principal": "*",
        "Action": "s3:PutObject",
        "Resource": format!("arn:aws:s3:::{bucket}/*"),
        "Condition": {"Bool": {"aws:SecureTransport": "false"}},
    })]
}

/// The control: a destination that denies exactly the write CloudTrail would make, which is
/// what says the clauses above are decided against that write and not ignored.
fn deny_owner_granting_writes(bucket: &str) -> Vec<Value> {
    vec![json!({
        "Sid": "DenyTheDeliveryItself",
        "Effect": "Deny",
        "Principal": "*",
        "Action": "s3:PutObject",
        "Resource": format!("arn:aws:s3:::{bucket}/*"),
        "Condition": {"StringEquals": {"s3:x-amz-acl": DELIVERY_CANNED_ACL}},
    })]
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the documented destination policy",
            hardening: no_hardening,
            accepted: true,
        },
        Row {
            case: "hardened against writes that do not leave the owner owning the object",
            hardening: deny_writes_that_are_not_owner_granting,
            accepted: true,
        },
        Row {
            case: "hardened against writes made outside TLS",
            hardening: deny_writes_outside_tls,
            accepted: true,
        },
        Row {
            case: "a destination denying exactly the write the delivery makes",
            hardening: deny_owner_granting_writes,
            accepted: false,
        },
    ]
}

/// RULE: the log-file write names the `bucket-owner-full-control` canned ACL, so a destination
/// that admits only such writes receives the trail's log files — and one that refuses them is
/// a destination the trail is not accepted against at all.
#[tokio::test]
async fn a_trails_log_file_write_names_the_owner_granting_canned_acl() {
    for (index, row) in rows().into_iter().enumerate() {
        let emulator = Emulator::new();
        let bucket = format!("canned-acl-logs-{index}");
        let trail = format!("canned-acl-trail-{index}");
        let case = row.case;

        let mut statements = documented_statements(&bucket);
        statements.extend((row.hardening)(&bucket));
        let policy = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
        emulator.log_bucket(&bucket, &policy).await;

        let create = json!({"Name": &trail, "S3BucketName": &bucket});
        let (status, answered) = emulator.cloudtrail("CreateTrail", &create).await;
        if !row.accepted {
            assert_eq!(
                status,
                referenced_resources::INSUFFICIENT_S3_BUCKET_POLICY_STATUS,
                "case {case:?}: expected the destination to be refused, got {status} with \
                 {answered}"
            );
            let reported = answered
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                reported.ends_with(INSUFFICIENT_POLICY_CODE),
                "case {case:?}: expected {INSUFFICIENT_POLICY_CODE}, got {answered}"
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: expected the destination to be accepted, got {status} with {answered}"
        );

        // Nothing is logged yet, so nothing is delivered for the create itself.
        let before = emulator.log_files(&bucket).await;
        assert!(
            before.is_empty(),
            "case {case:?}: a trail that is not logging delivers nothing, got {before:?}"
        );

        // Once it logs, the call that started it is delivered — which is the write the row's
        // destination policy decides.
        emulator
            .served(case, "StartLogging", &json!({"Name": &trail}))
            .await;
        let delivered = emulator.log_files(&bucket).await;
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected the logging trail's log file in {bucket}, got none"
        );
        for key in &delivered {
            assert!(
                key.starts_with(&format!("AWSLogs/{}/CloudTrail/{REGION}/", account())),
                "case {case:?}: expected a log file under the documented prefix, got {key:?}"
            );
        }
    }
}
