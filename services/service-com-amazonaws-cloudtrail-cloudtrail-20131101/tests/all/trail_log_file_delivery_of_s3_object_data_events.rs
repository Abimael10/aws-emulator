// SPDX-License-Identifier: CC0-1.0

//! What a LOGGING trail delivers for the account's S3 OBJECT data events: the calls on the
//! CONTENTS of an object, as against the bucket-level calls that administer the account's
//! resources.
//!
//! Two services are mounted on ONE engine, because the behaviour spans them: CloudTrail holds
//! the trail and decides the delivery, and Amazon S3 both serves the object calls and is where
//! the log files land. Every object call goes over the wire the way a client makes it, and the
//! log bucket is created, policied, listed and read through Amazon S3's OWN API over the same
//! in-process callback a handler uses — so a delivered log file counts only if the object
//! store itself answers with it.
//!
//! The rule under test: while a trail is logging, every call on the CONTENTS of one object
//! that one of the trail's event selectors admits is delivered into that trail's bucket as a
//! gzipped log-file record, under the documented key layout — carrying `s3.amazonaws.com` as
//! its `eventSource`, the operation as its `eventName`, `eventCategory` `Data` with
//! `managementEvent` false, the object the call acted on as its one resource (by the
//! `AWS::S3::Object` type and the object's ARN), the call's own read/write class as
//! `readOnly`, the code and message a REFUSED call was answered with, and the id the CALLER
//! was answered under as its `requestID`. A call no selector admits is delivered nowhere, and
//! no object data event reaches event history.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! which object operation served the call (a read, a write, a delete, a multi-object delete),
//! whether the call was served or refused, which bucket and which key prefix it addressed, and
//! — over the second and third tables — which KIND of selector the trail carries (an advanced
//! one narrowing by ARN prefix, a basic `DataResource` narrowing by `ReadWriteType`) and
//! whether that selector admits the trail's OWN log bucket.

use std::io::Read;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::emulator::{cloudtrail_bucket_policy, TARGET_PREFIX};

/// The Region every call below is signed for, which is the Region the trail is homed in and
/// so the Region its log files land under.
const REGION: &str = "us-east-1";

/// The `eventSource` CloudTrail stamps on an Amazon S3 record
/// (`smithy_query(query="service_info")` on that service ->
/// `awsService.cloudTrailEventSource: s3.amazonaws.com`).
const S3_EVENT_SOURCE: &str = "s3.amazonaws.com";

/// The `eventCategory` of a data event, and of a control-plane one.
const DATA_CATEGORY: &str = "Data";
const MANAGEMENT_CATEGORY: &str = "Management";

/// The resource type an object is named by in a record and in a selector's `resources.type`
/// field — the one `DataResource$Type` lists for this service's data events.
const OBJECT_RESOURCE_TYPE: &str = "AWS::S3::Object";

/// The header an S3 response names the request it answers in ("`x-amz-request-id`: A value
/// created by Amazon S3 that uniquely identifies the request", Amazon S3 API Reference,
/// *Common Response Headers*), lowercased as HTTP compares it.
const REQUEST_ID_HEADER: &str = "x-amz-request-id";

/// The same for CloudTrail's own `awsJson` responses.
const CLOUDTRAIL_REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// How many characters an S3 request id carries, and the alphabet it is written in — the
/// shape every answer of that service names its request by.
const REQUEST_ID_CHARS: usize = 16;
const REQUEST_ID_ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The member a log file's document holds its records in.
const RECORDS_MEMBER: &str = "Records";

/// The first segment of a log-file key under the trail's own prefix, the segment naming the
/// service whose files it holds, and what a log file's name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// What one dispatched request answered: its status, and the id it named its request by.
struct Answered {
    status: u16,
    request_id: String,
    body: Value,
}

/// CloudTrail and Amazon S3 over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; a credential
        // scope is still carried, because it is what names the Region and the caller.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_s3_amazons3::EmulatedService::new(Arc::clone(&engine)),
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// One CloudTrail call that has to be served.
    async fn cloudtrail(&self, case: &str, operation: &str, body: &Value) -> Answered {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("x-amz-target", format!("{TARGET_PREFIX}{operation}"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("host", format!("cloudtrail.{REGION}.amazonaws.com"))
            .header("authorization", authorization("cloudtrail"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let answered = self
            .dispatch(request, CLOUDTRAIL_REQUEST_ID_HEADER, true)
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One call on an OBJECT, made the way a client makes it: a path-style request to the
    /// Regional endpoint, signed for `s3`.
    async fn object_call(&self, method: &str, uri: &str, body: &str) -> Answered {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("content-type", "text/plain")
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        self.dispatch(request, REQUEST_ID_HEADER, false).await
    }

    /// Dispatch one request and read the id it named its request by out of `header`.
    async fn dispatch(
        &self,
        request: http::Request<bytes::Bytes>,
        header: &str,
        json_body: bool,
    ) -> Answered {
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get(header)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if json_body {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        } else {
            Value::String(String::from_utf8_lossy(response.body()).into_owned())
        };
        Answered {
            status,
            request_id,
            body,
        }
    }

    /// The Amazon S3 client, over the same in-process callback the delivery uses — built by
    /// the same shared constructor, so the test's transport is the delivery's by
    /// construction.
    fn s3(&self) -> aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket`.
    async fn bucket(&self, bucket: &str) {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
    }

    /// Create `bucket` and attach the policy that lets CloudTrail deliver into it.
    async fn log_bucket(&self, bucket: &str) {
        self.bucket(bucket).await;
        self.s3()
            .put_bucket_policy()
            .bucket(bucket)
            .policy(cloudtrail_bucket_policy(bucket))
            .send()
            .await
            .unwrap_or_else(|error| panic!("policying {bucket} must succeed, got {error:?}"));
    }

    /// Every object key the bucket holds under `prefix`, as Amazon S3 lists them.
    async fn keys_under(&self, bucket: &str, prefix: &str) -> Vec<String> {
        let listed = self
            .s3()
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .send()
            .await
            .unwrap_or_else(|error| panic!("listing {bucket}/{prefix} must succeed, got {error:?}"));
        let mut keys: Vec<String> = listed
            .contents()
            .iter()
            .filter_map(|object| object.key().map(str::to_string))
            .collect();
        keys.sort();
        keys
    }

    /// Every record the trail delivered under `prefix`, with the key each one arrived in —
    /// each file read through `GetObject` and gunzipped.
    ///
    /// The keys are listed FIRST and then read, so the files this reading itself may cause
    /// (a read of an object is a data event too, for a trail whose selectors admit the log
    /// bucket) are not part of what it reports.
    async fn delivered(&self, bucket: &str, prefix: &str) -> Vec<(String, Value)> {
        let mut delivered = Vec::new();
        for key in self.keys_under(bucket, prefix).await {
            let object = self
                .s3()
                .get_object()
                .bucket(bucket)
                .key(&key)
                .send()
                .await
                .unwrap_or_else(|error| panic!("reading {key} must succeed, got {error:?}"));
            let body = object
                .body
                .collect()
                .await
                .unwrap_or_else(|error| panic!("reading {key}'s body must succeed, got {error:?}"))
                .into_bytes();
            let mut document = String::new();
            flate2::read::GzDecoder::new(body.as_ref())
                .read_to_string(&mut document)
                .unwrap_or_else(|error| panic!("{key} must be a gzip stream, got {error}"));
            let parsed: Value = serde_json::from_str(&document)
                .unwrap_or_else(|error| panic!("{key} must gunzip to JSON, got {error}"));
            let records = parsed
                .get(RECORDS_MEMBER)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| panic!("{key} must hold {RECORDS_MEMBER}, got {parsed}"));
            for record in records {
                delivered.push((key.clone(), record));
            }
        }
        delivered
    }

    /// Every DATA record of Amazon S3 the trail delivered under `prefix`.
    async fn delivered_object_records(&self, bucket: &str, prefix: &str) -> Vec<(String, Value)> {
        self.delivered(bucket, prefix)
            .await
            .into_iter()
            .filter(|(_, record)| {
                text(record, "eventSource") == S3_EVENT_SOURCE
                    && text(record, "eventCategory") == DATA_CATEGORY
            })
            .collect()
    }
}

/// The `Authorization` header a request signed for `signing_name` in [`REGION`] carries.
/// Neither the key id nor the signature is verified — the harness turns request
/// authentication off — but the credential SCOPE is what names the service and the Region.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The account every trail, bucket and record below belongs to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The trail's whole delivery area under the key prefix it was created with, which is where a
/// consumer of the trail looks for its files.
fn delivery_prefix(key_prefix: &str) -> String {
    format!(
        "{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{}/{LOG_FILE_SERVICE_SEGMENT}/",
        account()
    )
}

/// The ARN of the object `key` in `bucket`, which is how a record names the resource the call
/// acted on: `arn:aws:s3:::<bucket>/<key>` carries no Region and no account, because a bucket
/// name is globally unique.
fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// The text a record holds at `member`, or `""` for one that holds none.
fn text(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Point a trail at `bucket` under `key_prefix`, configure it with `selectors`, and start it
/// logging — the four calls a client makes, each of which must be served.
///
/// `selectors` is the whole `PutEventSelectors` body but the trail's name, so a row can
/// configure the trail with ADVANCED selectors or with BASIC ones.
async fn logging_trail(
    emulator: &Emulator,
    trail: &str,
    bucket: &str,
    key_prefix: &str,
    selectors: Value,
) {
    emulator
        .cloudtrail(
            "setup",
            "CreateTrail",
            &json!({
                "Name": trail,
                "S3BucketName": bucket,
                "S3KeyPrefix": key_prefix,
                "IsMultiRegionTrail": false,
                "IncludeGlobalServiceEvents": false,
            }),
        )
        .await;
    let mut body = json!({"TrailName": trail});
    for (member, value) in selectors
        .as_object()
        .expect("a row's selectors are a document")
    {
        body[member] = value.clone();
    }
    emulator
        .cloudtrail("setup", "PutEventSelectors", &body)
        .await;
    emulator
        .cloudtrail("setup", "StartLogging", &json!({"Name": trail}))
        .await;
    let status = emulator
        .cloudtrail("setup", "GetTrailStatus", &json!({"Name": trail}))
        .await;
    assert_eq!(
        status.body.get("IsLogging"),
        Some(&json!(true)),
        "setup: expected the trail to report itself logging, got {}",
        status.body
    );
}

/// The advanced event selector a client installs to log the object data events of ONE bucket
/// while EXCLUDING one key prefix of it: the `Data` category, the object resource type, and
/// the two narrowings of the resource ARN.
fn arn_scoped_object_selector(bucket: &str, excluded_prefix: &str) -> Value {
    json!({"AdvancedEventSelectors": [{
        "Name": "s3-object-data",
        "FieldSelectors": [
            {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
            {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
            {"Field": "resources.ARN", "StartsWith": [format!("arn:aws:s3:::{bucket}/")]},
            {
                "Field": "resources.ARN",
                "NotStartsWith": [format!("arn:aws:s3:::{bucket}/{excluded_prefix}")],
            },
        ],
    }]})
}

/// The BASIC event selector a client installs to log the object data events of ONE bucket in
/// the read/write class `read_write_type` names: a `DataResource` of the object type whose
/// values are the bucket's own ARN prefix.
fn basic_object_selector(bucket: &str, read_write_type: &str) -> Value {
    json!({"EventSelectors": [{
        "ReadWriteType": read_write_type,
        "IncludeManagementEvents": false,
        "DataResources": [{
            "Type": OBJECT_RESOURCE_TYPE,
            "Values": [format!("arn:aws:s3:::{bucket}/")],
        }],
    }]})
}

/// One object call of the round, and what the record delivered for it has to say.
struct Served {
    case: &'static str,
    /// The operation the call reaches, which is also the `eventName` its record carries.
    operation: &'static str,
    /// The object the call acted on, as the record names it — `None` for a call that acted
    /// on no object at all.
    resource: Option<String>,
    /// Whether the call only REPORTED state, which the record carries as `readOnly`.
    read_only: bool,
    /// The wire error code the call was answered with, for a call that failed.
    error_code: Option<&'static str>,
    /// The message that went with it, which is the one the CALLER received.
    error_message: Option<&'static str>,
    /// Whether the trail's selectors admit the call's record.
    delivered: bool,
    /// The id the caller was answered under, which joins the record to this call.
    request_id: String,
}

/// RULE: a logging trail whose advanced selector names the `Data` category, the object
/// resource type and a bucket's ARN prefix delivers exactly one record per object call inside
/// that prefix — a refused call included — and delivers nothing for a call the selector's
/// exclusion or its bucket scope leaves out, nor for a call that administers a bucket.
#[tokio::test]
async fn an_arn_scoped_selector_delivers_the_object_calls_it_admits_and_withholds_the_rest() {
    let emulator = Emulator::new();
    let (log_bucket, data_bucket, other_bucket) = (
        "s3-object-data-trail-logs",
        "s3-object-data-subject",
        "s3-object-data-bystander",
    );
    let (trail, key_prefix) = ("s3-object-data-trail", "obs/s3-data");
    let prefix = delivery_prefix(key_prefix);
    emulator.log_bucket(log_bucket).await;
    emulator.bucket(data_bucket).await;
    emulator.bucket(other_bucket).await;
    logging_trail(
        &emulator,
        trail,
        log_bucket,
        key_prefix,
        arn_scoped_object_selector(data_bucket, "private/"),
    )
    .await;

    // The rows differ along the operation, the key prefix, the bucket, the class of the call
    // and whether the call was served or refused.
    let wrote_included = emulator
        .object_call("PUT", &format!("/{data_bucket}/open/report.csv"), "ledger")
        .await;
    let read_included = emulator
        .object_call("GET", &format!("/{data_bucket}/open/report.csv"), "")
        .await;
    let read_missing = emulator
        .object_call("GET", &format!("/{data_bucket}/open/absent.csv"), "")
        .await;
    let wrote_excluded = emulator
        .object_call("PUT", &format!("/{data_bucket}/private/secret.csv"), "hush")
        .await;
    let read_excluded = emulator
        .object_call("GET", &format!("/{data_bucket}/private/secret.csv"), "")
        .await;
    let wrote_elsewhere = emulator
        .object_call("PUT", &format!("/{other_bucket}/open/report.csv"), "ledger")
        .await;
    let deleted_included = emulator
        .object_call("DELETE", &format!("/{data_bucket}/open/report.csv"), "")
        .await;
    let listed = emulator
        .object_call("GET", &format!("/{data_bucket}?list-type=2"), "")
        .await;

    let served: Vec<Served> = vec![
        Served {
            case: "a write inside the selector's prefix",
            operation: "PutObject",
            resource: Some(object_arn(data_bucket, "open/report.csv")),
            read_only: false,
            error_code: None,
            error_message: None,
            delivered: true,
            request_id: wrote_included.request_id,
        },
        Served {
            case: "a read inside the selector's prefix",
            operation: "GetObject",
            resource: Some(object_arn(data_bucket, "open/report.csv")),
            read_only: true,
            error_code: None,
            error_message: None,
            delivered: true,
            request_id: read_included.request_id,
        },
        Served {
            case: "a read of a key the bucket does not hold",
            operation: "GetObject",
            resource: Some(object_arn(data_bucket, "open/absent.csv")),
            read_only: true,
            error_code: Some("NoSuchKey"),
            error_message: Some("The specified key does not exist."),
            delivered: true,
            request_id: read_missing.request_id,
        },
        Served {
            case: "a write under the EXCLUDED key prefix",
            operation: "PutObject",
            resource: Some(object_arn(data_bucket, "private/secret.csv")),
            read_only: false,
            error_code: None,
            error_message: None,
            delivered: false,
            request_id: wrote_excluded.request_id,
        },
        Served {
            case: "a read under the EXCLUDED key prefix",
            operation: "GetObject",
            resource: Some(object_arn(data_bucket, "private/secret.csv")),
            read_only: true,
            error_code: None,
            error_message: None,
            delivered: false,
            request_id: read_excluded.request_id,
        },
        Served {
            case: "a write in ANOTHER bucket",
            operation: "PutObject",
            resource: Some(object_arn(other_bucket, "open/report.csv")),
            read_only: false,
            error_code: None,
            error_message: None,
            delivered: false,
            request_id: wrote_elsewhere.request_id,
        },
        Served {
            case: "a delete inside the selector's prefix",
            operation: "DeleteObject",
            resource: Some(object_arn(data_bucket, "open/report.csv")),
            read_only: false,
            error_code: None,
            error_message: None,
            delivered: true,
            request_id: deleted_included.request_id,
        },
        Served {
            case: "a listing of what the bucket holds, which is no object call",
            operation: "ListObjectsV2",
            resource: None,
            read_only: true,
            error_code: None,
            error_message: None,
            delivered: false,
            request_id: listed.request_id,
        },
    ];

    for (case, answered, expected) in [
        ("a write inside the prefix", &wrote_included.status, 200),
        ("a read inside the prefix", &read_included.status, 200),
        ("a read of an absent key", &read_missing.status, 404),
        ("a write under the excluded prefix", &wrote_excluded.status, 200),
        ("a delete inside the prefix", &deleted_included.status, 204),
        ("the listing", &listed.status, 200),
    ] {
        assert_eq!(
            *answered, expected,
            "case {case:?}: expected HTTP {expected} from the object call, got {answered}"
        );
    }

    let delivered = emulator.delivered_object_records(log_bucket, &prefix).await;
    for call in &served {
        assert_eq!(
            call.request_id.len(),
            REQUEST_ID_CHARS,
            "case {:?}: the answer must name the request it answers, got {:?}",
            call.case,
            call.request_id
        );
        let matching: Vec<&(String, Value)> = delivered
            .iter()
            .filter(|(_, record)| text(record, "requestID") == call.request_id)
            .collect();
        assert_eq!(
            matching.len(),
            usize::from(call.delivered),
            "case {:?}: expected {} delivered record(s) of the call answered as {:?}, got {} \
             among records of {:?}",
            call.case,
            usize::from(call.delivered),
            call.request_id,
            matching.len(),
            delivered
                .iter()
                .map(|(_, record)| (text(record, "eventName"), text(record, "requestID")))
                .collect::<Vec<(String, String)>>()
        );
        if !call.delivered {
            continue;
        }
        let (key, record) = matching[0];
        assert_eq!(
            (
                text(record, "eventName").as_str(),
                text(record, "eventSource").as_str(),
                text(record, "eventCategory").as_str(),
            ),
            (call.operation, S3_EVENT_SOURCE, DATA_CATEGORY),
            "case {:?}: expected the data record of {} by {S3_EVENT_SOURCE}, got {record}",
            call.case,
            call.operation
        );
        assert_eq!(
            record.get("managementEvent"),
            Some(&json!(false)),
            "case {:?}: a data event is no management event, got {record}",
            call.case
        );
        assert_eq!(
            (
                record.pointer("/resources/0/type"),
                record.pointer("/resources/0/ARN"),
            ),
            (
                Some(&json!(OBJECT_RESOURCE_TYPE)),
                call.resource.as_ref().map(|arn| json!(arn)).as_ref(),
            ),
            "case {:?}: expected the object as the record's resource, got {record}",
            call.case
        );
        assert_eq!(
            record.get("readOnly"),
            Some(&json!(call.read_only)),
            "case {:?}: expected readOnly == {}, got {record}",
            call.case,
            call.read_only
        );
        assert_eq!(
            record.get("errorCode").and_then(Value::as_str),
            call.error_code,
            "case {:?}: expected the code the caller was answered with, got {record}",
            call.case
        );
        assert_eq!(
            record.get("errorMessage").and_then(Value::as_str),
            call.error_message,
            "case {:?}: expected the message the caller was answered with, got {record}",
            call.case
        );
        assert_eq!(
            record
                .get("requestParameters")
                .and_then(|parameters| parameters.get("bucketName"))
                .and_then(Value::as_str),
            Some(data_bucket),
            "case {:?}: a dispatched call records the bucket it addressed, got {record}",
            call.case
        );
        assert_eq!(
            record
                .get("requestParameters")
                .and_then(|parameters| parameters.get("key"))
                .and_then(Value::as_str)
                .map(|key| object_arn(data_bucket, key)),
            call.resource.clone(),
            "case {:?}: a dispatched call records the key it addressed, got {record}",
            call.case
        );
        assert!(
            text(record, "requestID")
                .chars()
                .all(|character| REQUEST_ID_ALPHABET.contains(character)),
            "case {:?}: expected the id S3 answered the caller under, got {record}",
            call.case
        );
        assert!(
            key.starts_with(&prefix) && key.ends_with(LOG_FILE_SUFFIX),
            "case {:?}: expected the record under the trail's prefix in a {LOG_FILE_SUFFIX} \
             file, got {key:?}",
            call.case
        );
    }

    // ...and an object data event is not event history: a lookup over Amazon S3's events
    // reports none of these calls.
    let looked_up = emulator
        .cloudtrail(
            "the lookup",
            "LookupEvents",
            &json!({
                "LookupAttributes": [{
                    "AttributeKey": "EventSource",
                    "AttributeValue": S3_EVENT_SOURCE,
                }],
            }),
        )
        .await;
    let names: Vec<String> = looked_up
        .body
        .get("Events")
        .and_then(Value::as_array)
        .map(|events| events.iter().map(|event| text(event, "EventName")).collect())
        .unwrap_or_default();
    for call in &served {
        if call.resource.is_none() {
            // The complement of the same rule: a BUCKET-level call is no data event, and it
            // IS a management event of the account — so event history holds it, which is what
            // tells "no data event reaches a lookup" from "nothing is recorded at all".
            assert!(
                names.contains(&call.operation.to_string()),
                "case {:?}: a bucket-level call is a management event, so a lookup reports it, \
                 got {names:?}",
                call.case
            );
            continue;
        }
        assert!(
            !names.contains(&call.operation.to_string()),
            "case {:?}: a data event reaches no event history, got {names:?}",
            call.case
        );
    }
}

/// RULE's second side: a BASIC event selector states the same thing a `DataResource` of the
/// object type plus a `ReadWriteType` — so the class it names decides which object calls the
/// trail delivers, and a trail that includes management events alone delivers no object call
/// at all.
#[tokio::test]
async fn a_basic_data_resource_selector_delivers_the_read_write_class_it_names() {
    // (case, the trail's selectors, whether the write is delivered, whether the read is)
    let rows: Vec<(&str, Value, bool, bool)> = vec![
        (
            "a data resource logging every class",
            basic_object_selector("basic-selector-subject", "All"),
            true,
            true,
        ),
        (
            "a data resource narrowed to WriteOnly",
            basic_object_selector("basic-selector-subject", "WriteOnly"),
            true,
            false,
        ),
        (
            "a data resource narrowed to ReadOnly",
            basic_object_selector("basic-selector-subject", "ReadOnly"),
            false,
            true,
        ),
        (
            "a data resource naming ANOTHER bucket",
            basic_object_selector("basic-selector-bystander", "All"),
            false,
            false,
        ),
        (
            "management events alone, which is a trail's default",
            json!({"EventSelectors": [{
                "ReadWriteType": "All",
                "IncludeManagementEvents": true,
            }]}),
            false,
            false,
        ),
    ];

    for (case, selectors, delivers_write, delivers_read) in rows {
        let emulator = Emulator::new();
        let (log_bucket, data_bucket) =
            ("basic-selector-trail-logs", "basic-selector-subject");
        let (trail, key_prefix) = ("basic-selector-trail", "obs/basic");
        let prefix = delivery_prefix(key_prefix);
        emulator.log_bucket(log_bucket).await;
        emulator.bucket(data_bucket).await;
        logging_trail(&emulator, trail, log_bucket, key_prefix, selectors).await;

        let wrote = emulator
            .object_call("PUT", &format!("/{data_bucket}/open/report.csv"), "ledger")
            .await;
        let read = emulator
            .object_call("GET", &format!("/{data_bucket}/open/report.csv"), "")
            .await;
        let delivered = emulator.delivered_object_records(log_bucket, &prefix).await;
        for (operation, request_id, expected) in [
            ("PutObject", &wrote.request_id, delivers_write),
            ("GetObject", &read.request_id, delivers_read),
        ] {
            let found = delivered
                .iter()
                .any(|(_, record)| &text(record, "requestID") == request_id);
            assert_eq!(
                found, expected,
                "case {case:?}: expected the {operation} answered as {request_id:?} delivered \
                 == {expected}, got records of {:?}",
                delivered
                    .iter()
                    .map(|(_, record)| (
                        text(record, "eventName"),
                        text(record, "requestID")
                    ))
                    .collect::<Vec<(String, String)>>()
            );
        }
    }
}

/// RULE's third side: the write CloudTrail itself makes to DELIVER a log file is not one of
/// the account's data events — so a trail whose data resource admits every object of the
/// account, its own log bucket included, delivers one record per CLIENT call and no record of
/// its own deliveries.
#[tokio::test]
async fn a_trail_delivers_no_record_of_its_own_log_file_writes() {
    let emulator = Emulator::new();
    let (log_bucket, data_bucket) = ("every-object-trail-logs", "every-object-subject");
    let (trail, key_prefix) = ("every-object-trail", "obs/every");
    let prefix = delivery_prefix(key_prefix);
    emulator.log_bucket(log_bucket).await;
    emulator.bucket(data_bucket).await;
    logging_trail(
        &emulator,
        trail,
        log_bucket,
        key_prefix,
        // "To log data events for all objects in all S3 buckets in your Amazon Web Services
        // account, specify the prefix as `arn:aws:s3`" (`DataResource$Values`).
        json!({"EventSelectors": [{
            "ReadWriteType": "All",
            "IncludeManagementEvents": false,
            "DataResources": [{"Type": OBJECT_RESOURCE_TYPE, "Values": ["arn:aws:s3"]}],
        }]}),
    )
    .await;

    let wrote = emulator
        .object_call("PUT", &format!("/{data_bucket}/open/report.csv"), "ledger")
        .await;
    let delivered = emulator.delivered_object_records(log_bucket, &prefix).await;
    let named: Vec<(String, String)> = delivered
        .iter()
        .map(|(_, record)| {
            (
                text(record, "eventName"),
                record
                    .pointer("/resources/0/ARN")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        named,
        vec![(
            "PutObject".to_string(),
            object_arn(data_bucket, "open/report.csv"),
        )],
        "case \"a selector over every object of the account\": expected the one CLIENT write \
         answered as {:?} and no record of the delivery's own write, got {named:?}",
        wrote.request_id
    );
}

/// RULE's fourth side: the delivery is the TRAIL's, so a trail that has STOPPED logging
/// delivers nothing for a later object call, while the same call before the stop was
/// delivered.
#[tokio::test]
async fn a_stopped_trail_delivers_no_later_object_data_event() {
    let emulator = Emulator::new();
    let (log_bucket, data_bucket) = ("stopped-object-trail-logs", "stopped-object-subject");
    let (trail, key_prefix) = ("stopped-object-trail", "obs/stopped");
    let prefix = delivery_prefix(key_prefix);
    emulator.log_bucket(log_bucket).await;
    emulator.bucket(data_bucket).await;
    logging_trail(
        &emulator,
        trail,
        log_bucket,
        key_prefix,
        arn_scoped_object_selector(data_bucket, "private/"),
    )
    .await;

    let while_logging = emulator
        .object_call("PUT", &format!("/{data_bucket}/open/first.csv"), "one")
        .await;
    let delivered = emulator.delivered_object_records(log_bucket, &prefix).await;
    assert!(
        delivered
            .iter()
            .any(|(_, record)| text(record, "requestID") == while_logging.request_id),
        "case \"while logging\": expected the object call answered as {:?} delivered, got {} \
         record(s)",
        while_logging.request_id,
        delivered.len()
    );

    emulator
        .cloudtrail("stopping", "StopLogging", &json!({"Name": trail}))
        .await;
    let before = emulator.keys_under(log_bucket, &prefix).await;
    let after_stop = emulator
        .object_call("PUT", &format!("/{data_bucket}/open/second.csv"), "two")
        .await;
    let after = emulator.keys_under(log_bucket, &prefix).await;
    assert_eq!(
        after, before,
        "case \"after the stop\": expected no further log file for the object call answered \
         as {:?}, got {after:?}",
        after_stop.request_id
    );
}

/// RULE's fifth side: an advanced selector that pins the `Management` category admits no
/// object call, and one that pins the object type admits the object calls of every bucket
/// when it narrows no ARN — the two ends of what a selector's scope decides.
#[tokio::test]
async fn an_object_selector_admits_what_its_own_fields_say_it_does() {
    // (case, the trail's selectors, whether the object write is delivered)
    let rows: Vec<(&str, Value, bool)> = vec![
        (
            "a management-events selector",
            json!({"AdvancedEventSelectors": [{
                "Name": "management",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": [MANAGEMENT_CATEGORY]},
                ],
            }]}),
            false,
        ),
        (
            "a data selector over ANOTHER resource type",
            json!({"AdvancedEventSelectors": [{
                "Name": "table-data",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
                    {"Field": "resources.type", "Equals": ["AWS::DynamoDB::Table"]},
                ],
            }]}),
            false,
        ),
        (
            "a data selector narrowing no ARN at all",
            json!({"AdvancedEventSelectors": [{
                "Name": "every-object",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
                    {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
                ],
            }]}),
            true,
        ),
        (
            "a data selector that also pins the operation",
            json!({"AdvancedEventSelectors": [{
                "Name": "writes-only",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
                    {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
                    {"Field": "eventName", "Equals": ["PutObject"]},
                ],
            }]}),
            true,
        ),
        (
            "a data selector that pins ANOTHER operation",
            json!({"AdvancedEventSelectors": [{
                "Name": "reads-only",
                "FieldSelectors": [
                    {"Field": "eventCategory", "Equals": [DATA_CATEGORY]},
                    {"Field": "resources.type", "Equals": [OBJECT_RESOURCE_TYPE]},
                    {"Field": "eventName", "Equals": ["GetObject"]},
                ],
            }]}),
            false,
        ),
    ];

    for (case, selectors, delivers) in rows {
        let emulator = Emulator::new();
        let (log_bucket, data_bucket) = ("selector-scope-trail-logs", "selector-scope-subject");
        let (trail, key_prefix) = ("selector-scope-trail", "obs/scope");
        let prefix = delivery_prefix(key_prefix);
        emulator.log_bucket(log_bucket).await;
        emulator.bucket(data_bucket).await;
        logging_trail(&emulator, trail, log_bucket, key_prefix, selectors).await;

        let wrote = emulator
            .object_call("PUT", &format!("/{data_bucket}/open/report.csv"), "ledger")
            .await;
        let delivered = emulator.delivered_object_records(log_bucket, &prefix).await;
        let found = delivered
            .iter()
            .any(|(_, record)| text(record, "requestID") == wrote.request_id);
        assert_eq!(
            found, delivers,
            "case {case:?}: expected the object write answered as {:?} delivered == \
             {delivers}, got records of {:?}",
            wrote.request_id,
            delivered
                .iter()
                .map(|(_, record)| (text(record, "eventName"), text(record, "requestID")))
                .collect::<Vec<(String, String)>>()
        );
    }
}
