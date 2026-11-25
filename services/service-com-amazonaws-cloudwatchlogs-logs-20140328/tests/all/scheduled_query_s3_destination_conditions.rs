// SPDX-License-Identifier: CC0-1.0

//! The CONDITIONS a scheduled query's Amazon S3 destination puts on the write that delivers a
//! run's results: the account the destination bucket is expected to belong to and the KMS key
//! the delivered object is encrypted under.
//!
//! CloudWatch Logs, Amazon S3 and KMS are mounted on ONE shared engine — the same assembly
//! `core/router` performs — so every bucket and key here is made with the owning service's own
//! operations and every object read back is one the object store itself holds, written by the
//! run over the in-process callback and not by anything in this suite.
//!
//! RULE: each member an `S3Configuration` carries beside the destination URI states a condition
//! of the delivery write, so the write states it and the service that owns the destination is
//! what judges it. `ownerAccountId` is "The Amazon Web Services accountId for the bucket owning
//! account", so a destination naming the bucket's owner is delivered to and one naming any
//! other account is not written at all: the execution still settles and its
//! `ScheduledQueryDestination` carries a terminal status with a populated `errorMessage` and no
//! `processedIdentifier`. `kmsKeyId` is "The Amazon Resource Name (ARN) of the KMS encryption
//! key", so the delivered object is stored under server-side encryption with that key; a
//! destination stating neither member leaves the write to state neither, and the object takes
//! the destination bucket's own default encryption configuration.
//!
//! The rows span what the rule generalises over: whether each of the two optional members is
//! stated at all, whether the stated owner is the bucket's own or another account, whether the
//! stated key is one KMS holds, whether a stated member is empty (`kmsKeyId` admits the empty
//! string, @length min 0, which states no key), and both members stated at once.
//!
//! Every row waits for the CONDITION — a run whose Amazon S3 destination has been processed —
//! and never for a span of the clock.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::now_ms;

/// The Region every request here is signed for.
const REGION: &str = "eu-west-2";

/// The host a path-style Amazon S3 request names.
const S3_HOST: &str = "s3.eu-west-2.amazonaws.com";

/// The XML namespace an Amazon S3 request payload carries.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The operations under test, and the read a delivery is proved through.
const CREATE: &str = "CreateScheduledQuery";
const HISTORY: &str = "GetScheduledQueryHistory";

/// The log group every row queries, and the stream its events are written to.
const GROUP: &str = "/aws/emulator/scheduled-query-destination-conditions";
const STREAM: &str = "runs";

/// The pipeline every row runs: the ERROR events of the window, as rows.
const QUERY_STRING: &str = "fields @timestamp, @message | filter @message like /ERROR/";

/// The messages seeded into the group — two of them ERROR, which is what the pipeline selects.
const MESSAGES: [&str; 3] = [
    "ERROR order 1 failed",
    "INFO order 2 accepted",
    "ERROR order 3 failed",
];

/// The IAM roles a scheduled query executes and delivers under, which is not what these rows
/// grade.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-execution";
const DELIVERY_ROLE: &str = "arn:aws:iam::000000000000:role/scheduled-query-delivery";

/// The key prefix every destination URI names.
const PREFIX: &str = "results";

/// The headers an object's server-side encryption is reported in: the `@httpHeader` members
/// `HeadObject` binds — `ServerSideEncryption` as `x-amz-server-side-encryption` and
/// `SSEKMSKeyId` as `x-amz-server-side-encryption-aws-kms-key-id`.
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The `ServerSideEncryption` value that is SSE-S3 — the base default a bucket no
/// `PutBucketEncryption` configured carries — and the one a KMS key belongs to ("This parameter
/// is allowed if and only if SSEAlgorithm is set to aws:kms or aws:kms:dsse",
/// `ApplyServerSideEncryptionByDefault$KMSMasterKeyID`).
const SSE_S3: &str = "AES256";
const SSE_KMS: &str = "aws:kms";

/// The statuses `GetScheduledQueryHistory` reports a destination's processing under: still owed,
/// and delivered.
const IN_PROGRESS: &str = "IN_PROGRESS";
const COMPLETE: &str = "COMPLETE";

/// An account id that owns no bucket of this emulator, which serves one account: a well-formed
/// `ownerAccountId` (@pattern `^\d{12}$`) naming somebody else.
const FOREIGN_ACCOUNT: &str = "210987654321";

/// A KMS key ARN of this Region that KMS holds no key under, for a destination whose stated key
/// cannot be used to encrypt anything.
const ABSENT_KEY: &str = "arn:aws:kms:eu-west-2:000000000000:key/4b2f1a60-1f2a-4f26-8a3a-1f4d9f0b61c2";

/// Milliseconds in one hour.
const HOUR: i64 = 60 * 60 * 1_000;

/// How long a row is given to see the run the clock has made due, and how long it waits between
/// two looks. Generous on purpose: the condition — a record whose Amazon S3 destination has been
/// processed — is what is waited for, never a fixed span.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

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
    async fn json(&self, service: &str, prefix: &str, operation: &str, body: &Value) -> (u16, Value) {
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

    /// A customer managed key of this account, answering its ARN.
    async fn created_key(&self, case: &str) -> String {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": {
                    "AWS": format!("arn:aws:iam::{}:root", engine::emulator_account_id()),
                },
                "Action": "kms:*",
                "Resource": "*",
            }],
        })
        .to_string();
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
            .uri(format!("https://{S3_HOST}{path}"))
            .header("host", S3_HOST)
            .header("authorization", Self::authorization("s3"))
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the request builds");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        (status, headers, response.into_body())
    }

    /// `PUT /<bucket>` — S3's `CreateBucket`, naming the Region every call here signs for.
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
    }

    /// Every key `bucket` holds, read through `ListObjectsV2`: the WHOLE listing, so an object
    /// written anywhere in the bucket shows up.
    async fn all_keys(&self, case: &str, bucket: &str) -> Vec<String> {
        let (status, _headers, body) = self
            .s3("GET", &format!("/{bucket}?list-type=2"), b"")
            .await;
        assert_eq!(
            status,
            200,
            "case {case:?}: list {bucket}: got {}",
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

    /// `GET /<bucket>/<key>` — the object's own headers and bytes.
    async fn object(&self, case: &str, bucket: &str, key: &str) -> (http::HeaderMap, Vec<u8>) {
        let (status, headers, body) = self.s3("GET", &format!("/{bucket}/{key}"), b"").await;
        assert_eq!(
            status,
            200,
            "case {case:?}: get {bucket}/{key}: got {}",
            String::from_utf8_lossy(&body)
        );
        (headers, body.to_vec())
    }
}

/// The value of `name` in a response's headers, or `None` when it carries none.
fn header(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Seed [`GROUP`] with [`MESSAGES`] an hour back, so every run's lookback window holds them.
async fn seed_events(emulator: &Emulator, case: &str) {
    emulator
        .logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    emulator
        .logs_ok(
            case,
            "CreateLogStream",
            &json!({"logGroupName": GROUP, "logStreamName": STREAM}),
        )
        .await;
    let first_ms = now_ms() - HOUR;
    let events: Vec<Value> = MESSAGES
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": first_ms + index as i64 * 1_000, "message": message})
        })
        .collect();
    emulator
        .logs_ok(
            case,
            "PutLogEvents",
            &json!({
                "logGroupName": GROUP,
                "logStreamName": STREAM,
                "logEvents": events,
            }),
        )
        .await;
}

/// What a row states as the destination's `ownerAccountId`.
#[derive(Debug, Clone, Copy)]
enum Owner {
    /// The member is not stated at all.
    Unstated,
    /// The account that really owns the destination bucket.
    Bucket,
    /// A well-formed account id that owns no bucket here.
    Foreign,
}

/// What a row states as the destination's `kmsKeyId`.
#[derive(Debug, Clone, Copy)]
enum Key {
    /// The member is not stated at all.
    Unstated,
    /// The empty string, which `kmsKeyId` admits (@length min 0) and which names no key.
    Empty,
    /// A customer managed key this account holds, created through KMS's own operation.
    Held,
    /// A key ARN KMS holds no key under.
    Absent,
}

/// One row: what the destination states, and what the delivery must then do.
struct Row {
    /// What the row is about, named in every assertion.
    case: &'static str,
    /// The name the scheduled query is created under, which also names its own bucket.
    name: &'static str,
    /// The `ownerAccountId` the destination states.
    owner: Owner,
    /// The `kmsKeyId` the destination states.
    key: Key,
    /// Whether the delivery happens at all.
    delivered: bool,
}

impl Row {
    /// The bucket this row's results are delivered to. One bucket per row, so the whole bucket
    /// listing is what the row's own delivery is judged by.
    fn bucket(&self) -> String {
        format!("sq-destination-conditions-{}", self.name)
    }

    /// The `destinationIdentifier` this row's `s3Configuration` names.
    fn destination_uri(&self) -> String {
        format!("s3://{}/{PREFIX}", self.bucket())
    }

    /// The key the run's object is written under, which the delivery reports as the URI of what
    /// it produced.
    fn expected_key(&self, query_id: &str) -> String {
        format!("{PREFIX}/{}/{query_id}.json", self.name)
    }
}

/// The table: every form each of the two optional members takes, and both at once.
const ROWS: [Row; 6] = [
    Row {
        case: "neither member stated",
        name: "plain",
        owner: Owner::Unstated,
        key: Key::Unstated,
        delivered: true,
    },
    Row {
        case: "the account that owns the destination bucket",
        name: "owner-matches",
        owner: Owner::Bucket,
        key: Key::Unstated,
        delivered: true,
    },
    Row {
        case: "an account that owns no bucket here",
        name: "owner-differs",
        owner: Owner::Foreign,
        key: Key::Unstated,
        delivered: false,
    },
    Row {
        case: "a KMS key this account holds",
        name: "key-held",
        owner: Owner::Unstated,
        key: Key::Held,
        delivered: true,
    },
    Row {
        case: "an empty kmsKeyId, which states no key",
        name: "key-empty",
        owner: Owner::Unstated,
        key: Key::Empty,
        delivered: true,
    },
    Row {
        case: "the bucket's owner and a KMS key together",
        name: "owner-and-key",
        owner: Owner::Bucket,
        key: Key::Held,
        delivered: true,
    },
];

/// The first record of `name`'s history whose Amazon S3 destination has been processed, polled
/// for because the run that processes it is owed by the clock.
async fn settled_record(emulator: &Emulator, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let page = emulator
            .logs_ok(
                case,
                HISTORY,
                &json!({
                    "identifier": name,
                    "startTime": 0,
                    "endTime": now_ms() + HOUR,
                }),
            )
            .await;
        let settled = page["triggerHistory"]
            .as_array()
            .and_then(|records| records.first())
            .filter(|record| {
                record["destinations"].as_array().is_some_and(|destinations| {
                    destinations.iter().any(|destination| {
                        destination["destinationType"].as_str() == Some("S3")
                            && destination["status"].as_str() != Some(IN_PROGRESS)
                    })
                })
            })
            .cloned();
        if let Some(settled) = settled {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a run of {name} with its Amazon S3 destination processed, \
             got {page}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The Amazon S3 destination one settled record reports.
fn s3_destination(case: &str, record: &Value) -> Value {
    record["destinations"]
        .as_array()
        .and_then(|destinations| {
            destinations
                .iter()
                .find(|destination| destination["destinationType"].as_str() == Some("S3"))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: the execution reports its Amazon S3 destination, got {record}")
        })
}

/// Every member the destination states travels with the delivery write: the owner the bucket is
/// expected to belong to and the key the delivered object is encrypted under.
#[tokio::test]
async fn a_destinations_stated_conditions_travel_with_the_delivery() {
    assert_ne!(
        FOREIGN_ACCOUNT,
        engine::emulator_account_id(),
        "the foreign account must not be the one that owns the buckets here"
    );

    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;

    for row in &ROWS {
        let case = row.case;
        let bucket = row.bucket();
        emulator.create_bucket(case, &bucket).await;

        let held_key = match row.key {
            Key::Held => Some(emulator.created_key(case).await),
            _ => None,
        };
        let mut s3_configuration = json!({
            "destinationIdentifier": row.destination_uri(),
            "roleArn": DELIVERY_ROLE,
        });
        match row.owner {
            Owner::Unstated => {}
            Owner::Bucket => {
                s3_configuration["ownerAccountId"] = json!(engine::emulator_account_id());
            }
            Owner::Foreign => s3_configuration["ownerAccountId"] = json!(FOREIGN_ACCOUNT),
        }
        match row.key {
            Key::Unstated => {}
            Key::Empty => s3_configuration["kmsKeyId"] = json!(""),
            Key::Held => s3_configuration["kmsKeyId"] = json!(held_key.clone().unwrap_or_default()),
            Key::Absent => s3_configuration["kmsKeyId"] = json!(ABSENT_KEY),
        }

        emulator
            .logs_ok(
                case,
                CREATE,
                &json!({
                    "name": row.name,
                    "queryLanguage": "CWLI",
                    "queryString": QUERY_STRING,
                    "scheduleExpression": "rate(1 day)",
                    "executionRoleArn": EXECUTION_ROLE,
                    "logGroupIdentifiers": [GROUP],
                    "startTimeOffset": 7_200,
                    "destinationConfiguration": {"s3Configuration": s3_configuration},
                }),
            )
            .await;

        let record = settled_record(&emulator, case, row.name).await;
        let query_id = record["queryId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: an execution reports the queryId it ran under, got {record}")
            })
            .to_string();
        let destination = s3_destination(case, &record);
        assert_eq!(
            destination["destinationIdentifier"].as_str(),
            Some(row.destination_uri().as_str()),
            "case {case:?}: the destination names the URI it delivers to, got {destination}"
        );

        if !row.delivered {
            // A write the destination's own conditions refuse happens at all: nothing is
            // written, the entry names no processed result, and it says why.
            assert_ne!(
                destination["status"].as_str(),
                Some(COMPLETE),
                "case {case:?}: a delivery that did not happen is not complete, got \
                 {destination}"
            );
            assert!(
                destination["errorMessage"]
                    .as_str()
                    .is_some_and(|message| !message.is_empty()),
                "case {case:?}: a delivery that did not happen says why, got {destination}"
            );
            assert!(
                destination.get("processedIdentifier").is_none(),
                "case {case:?}: a delivery that did not happen produced nothing, got \
                 {destination}"
            );
            let held = emulator.all_keys(case, &bucket).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected {bucket} to hold no object, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            destination["status"].as_str(),
            Some(COMPLETE),
            "case {case:?}: expected the delivery to the bucket the store holds to complete, got \
             {destination}"
        );
        let expected_key = row.expected_key(&query_id);
        let expected_uri = format!("s3://{bucket}/{expected_key}");
        assert_eq!(
            destination["processedIdentifier"].as_str(),
            Some(expected_uri.as_str()),
            "case {case:?}: expected the processed identifier {expected_uri:?}, got {destination}"
        );
        let held = emulator.all_keys(case, &bucket).await;
        assert_eq!(
            held,
            vec![expected_key.clone()],
            "case {case:?}: expected {bucket} to hold exactly {expected_key:?}, got {held:?}"
        );

        // The delivered object holds the rows the run produced over the seeded events, and
        // carries the encryption the destination stated: the stated key, or the bucket's own
        // default for a destination that stated none.
        let (headers, body) = emulator.object(case, &bucket, &expected_key).await;
        let rendered = String::from_utf8_lossy(&body).into_owned();
        for message in MESSAGES.iter().filter(|message| message.contains("ERROR")) {
            assert!(
                rendered.contains(message),
                "case {case:?}: expected the delivered rows to hold {message:?}, got {rendered:?}"
            );
        }
        assert!(
            !rendered.contains("INFO order 2 accepted"),
            "case {case:?}: the delivered rows are the ones the run produced, got {rendered:?}"
        );

        let algorithm = header(&headers, SSE_HEADER);
        let reported_key = header(&headers, SSE_KEY_HEADER);
        match &held_key {
            Some(key) => {
                assert_eq!(
                    algorithm.as_deref(),
                    Some(SSE_KMS),
                    "case {case:?}: expected the delivered object to be encrypted with a KMS \
                     key, got {algorithm:?}"
                );
                assert_eq!(
                    reported_key.as_deref(),
                    Some(key.as_str()),
                    "case {case:?}: expected the object to name {key:?} as its key, got \
                     {reported_key:?}"
                );
            }
            // A destination that states no key leaves the object to the destination bucket's
            // own default configuration, which for a bucket no `PutBucketEncryption`
            // configured is SSE-S3 and names no key at all.
            None => {
                assert_eq!(
                    algorithm.as_deref(),
                    Some(SSE_S3),
                    "case {case:?}: expected the bucket's own default encryption, got \
                     {algorithm:?}"
                );
                assert_eq!(
                    reported_key, None,
                    "case {case:?}: expected the object to name no KMS key, got {reported_key:?}"
                );
            }
        }
    }
}

/// A destination naming a KMS key KMS holds no key under delivers nothing, and the execution
/// settles reporting that refusal rather than an object nothing wrote.
#[tokio::test]
async fn a_destination_naming_a_key_kms_does_not_hold_delivers_nothing() {
    let row = Row {
        case: "a key ARN KMS holds no key under",
        name: "key-absent",
        owner: Owner::Unstated,
        key: Key::Absent,
        delivered: false,
    };
    let case = row.case;
    let emulator = Emulator::new();
    seed_events(&emulator, "seed").await;
    let bucket = row.bucket();
    emulator.create_bucket(case, &bucket).await;

    emulator
        .logs_ok(
            case,
            CREATE,
            &json!({
                "name": row.name,
                "queryLanguage": "CWLI",
                "queryString": QUERY_STRING,
                "scheduleExpression": "rate(1 day)",
                "executionRoleArn": EXECUTION_ROLE,
                "logGroupIdentifiers": [GROUP],
                "startTimeOffset": 7_200,
                "destinationConfiguration": {
                    "s3Configuration": {
                        "destinationIdentifier": row.destination_uri(),
                        "roleArn": DELIVERY_ROLE,
                        "kmsKeyId": ABSENT_KEY,
                    },
                },
            }),
        )
        .await;

    let record = settled_record(&emulator, case, row.name).await;
    let destination = s3_destination(case, &record);
    assert_ne!(
        destination["status"].as_str(),
        Some(COMPLETE),
        "case {case:?}: a delivery whose key cannot be used is not complete, got {destination}"
    );
    assert!(
        destination["errorMessage"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "case {case:?}: a delivery that did not happen says why, got {destination}"
    );
    assert!(
        destination.get("processedIdentifier").is_none(),
        "case {case:?}: a delivery that did not happen produced nothing, got {destination}"
    );
    let held = emulator.all_keys(case, &bucket).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected {bucket} to hold no object, got {held:?}"
    );
}
