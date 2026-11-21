// SPDX-License-Identifier: CC0-1.0

//! The DATA leg of this front door as a CLIENT configures it: the advanced event selector a
//! client writes through CloudTrail's own `PutEventSelectors`, and what the synchronous run of
//! an Express state machine is delivered as once that selector is on the trail.
//!
//! RULE: a call on the CONTENTS of a resource is a CloudTrail DATA event of that resource —
//! `eventCategory` `Data` with `managementEvent` false, the operation's own read/write
//! semantics as `readOnly`, the resource the call acted on by TYPE and ARN, the redaction
//! literal in place of every value the model declares `@sensitive`, the id the CALLER was
//! answered under as `requestID`, and no `responseElements`, because what such a call answers
//! IS the resource's contents. It is delivered into the bucket of each trail one of whose
//! event selectors admits it, under that trail's own key prefix, and into no other: a selector
//! admits an event only when EVERY one of its field selectors does, a selector that states the
//! `Management` category admits no data event at all, and a data event reaches no event
//! history (a lookup serves management and Insights events).
//!
//! Three services are mounted on ONE engine, because the behaviour spans them: Step Functions
//! serves the run, CloudTrail holds the trail and decides the delivery, and the object store is
//! where the log files land. The trail is created, given its selectors, started and read
//! through CloudTrail's OWN API, so what a run is delivered as is decided by the selectors a
//! client really wrote; the bucket is created, policied, listed and read through Amazon S3's
//! own API over the same in-process callback the delivery writes through, so a delivered log
//! file counts only if the object store itself answers with it.
//!
//! The neighbouring `sync_execution_data_events` suite pins the same delivery over the trail
//! projection the delivery reads, and the record's own members; the rows here differ along
//! what the rule generalises over on the CLIENT's side — which fields the selector a client
//! installs pins (the category, the resource type, the resource ARN by equality and by prefix,
//! the event name, the read/write kind), which values it pins them to, which machine the run
//! was made on, and whether the call is the synchronous data plane at all.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::service::data_events::STATE_MACHINE_RESOURCE_TYPE;
use service_com_amazonaws_sfn_awsstepfunctions::service::management_events::EVENT_SOURCE;
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;
use service_common::management_events::HIDDEN_SENSITIVE_VALUE;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// Step Functions' `X-Amz-Target` prefix (this model declares the service shape
/// `AWSStepFunctions`), the `awsJson` version it speaks, and its `sigv4Name`, which is what
/// routes a request to it.
const SFN_TARGET_PREFIX: &str = "AWSStepFunctions";
const SFN_JSON_VERSION: &str = "1.0";
const SFN_SIGNING_NAME: &str = "states";

/// CloudTrail's `X-Amz-Target` prefix, the `awsJson` version it speaks, and its signing name.
const CLOUDTRAIL_TARGET_PREFIX: &str = "CloudTrail_20131101";
const CLOUDTRAIL_JSON_VERSION: &str = "1.1";
const CLOUDTRAIL_SIGNING_NAME: &str = "cloudtrail";

/// The Region every call below is signed for, which is the Region the trails are homed in and
/// so the Region their log files land under.
const REGION: &str = "us-east-1";

/// The account every ARN of this emulator carries, and so the account every record belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// A role ARN inside the modelled `Arn` length (1..256).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";

/// A definition that succeeds immediately with a static result.
const HELLO_DEFINITION: &str =
    r#"{"StartAt":"Hello","States":{"Hello":{"Type":"Pass","Result":{"msg":"hi"},"End":true}}}"#;

/// The execution input every synchronous run below carries: a value that appears nowhere else,
/// so finding it inside a delivered record would be unambiguous. The model declares
/// `StartSyncExecutionInput$input` `@sensitive`, so no record may carry it.
const SECRET_INPUT: &str = r#"{"card":"n0t-in-a-trail-log-file"}"#;

/// The `eventCategory` of a data event, and the one of a control-plane call — the two
/// categories a trail's advanced event selectors can state here.
const DATA_CATEGORY: &str = "Data";
const MANAGEMENT_CATEGORY: &str = "Management";

/// Another resource type a `resources.type` condition may name, for the selector that admits a
/// class of data events this account never serves.
const S3_OBJECT_TYPE: &str = "AWS::S3::Object";

/// This service's synchronous data plane, which is the `eventName` of the record under test,
/// and the asynchronous start, for the selector that names another operation.
const SYNC_START_EVENT_NAME: &str = "StartSyncExecution";
const ASYNC_START_EVENT_NAME: &str = "StartExecution";

/// The header a response names the request it answers in, lowercased as HTTP compares it.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The segments of a log file's key, and what its name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The member a log file's document holds its records in.
const RECORDS_MEMBER: &str = "Records";

/// How often, and how many times at most, a poll looks for a log file the delivery writes
/// while the call is served. One look is normally enough; the loop is what keeps a loaded host
/// waiting rather than failing, so the deadline is far above the work.
const LOOK: Duration = Duration::from_millis(25);
const LOOKS: usize = 400;

/// One answer as its caller reads it: the status, the parsed body, and the id the response
/// named the request by.
struct Answered {
    status: u16,
    body: Value,
    request_id: String,
}

/// Step Functions, CloudTrail and the object store over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so requests are unauthenticated; the credential
        // scope is still carried, because it is what names the Region and the service.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
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

    /// One `X-Amz-Target` RPC call: its status, parsed body and the id it was answered under.
    async fn rpc(
        &self,
        signing_name: &str,
        json_version: &str,
        target: &str,
        body: &Value,
    ) -> Answered {
        let authorization = format!(
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", target)
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Answered {
            status,
            body,
            request_id,
        }
    }

    /// One Step Functions call that has to be served.
    async fn step_functions(&self, case: &str, operation: &str, body: &Value) -> Answered {
        let answered = self
            .rpc(
                SFN_SIGNING_NAME,
                SFN_JSON_VERSION,
                &format!("{SFN_TARGET_PREFIX}.{operation}"),
                body,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: Step Functions {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One CloudTrail call that has to be served.
    async fn cloudtrail(&self, case: &str, operation: &str, body: &Value) -> Answered {
        let answered = self
            .rpc(
                CLOUDTRAIL_SIGNING_NAME,
                CLOUDTRAIL_JSON_VERSION,
                &format!("{CLOUDTRAIL_TARGET_PREFIX}.{operation}"),
                body,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: CloudTrail {operation} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// Create an EXPRESS state machine, which is the type a synchronous start serves, and
    /// answer its ARN.
    async fn express_machine(&self, name: &str) -> String {
        let created = self
            .step_functions(
                "setup",
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                }),
            )
            .await;
        created.body["stateMachineArn"]
            .as_str()
            .expect("CreateStateMachine answers a stateMachineArn")
            .to_string()
    }

    /// One synchronous run of `machine`, named `name`, carrying the secret input.
    async fn sync_run(&self, case: &str, machine: &str, name: &str) -> Answered {
        self.step_functions(
            case,
            SYNC_START_EVENT_NAME,
            &json!({"stateMachineArn": machine, "name": name, "input": SECRET_INPUT}),
        )
        .await
    }

    /// The Amazon S3 client, over the same in-process callback the delivery uses — built by
    /// the same shared constructor, so the test's transport is the delivery's by construction.
    fn s3(&self) -> aws_sdk_s3::Client {
        service_common::s3_client::s3_client(&self.callback, REGION)
    }

    /// Create `bucket` and attach the policy that lets CloudTrail deliver into it: the two
    /// calls a client makes before pointing a trail at a bucket.
    async fn log_bucket(&self, bucket: &str) {
        self.s3()
            .create_bucket()
            .bucket(bucket)
            .send()
            .await
            .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
        self.s3()
            .put_bucket_policy()
            .bucket(bucket)
            .policy(cloudtrail_bucket_policy(bucket))
            .send()
            .await
            .unwrap_or_else(|error| panic!("policying {bucket} must succeed, got {error:?}"));
    }

    /// Point a trail at `bucket` under `key_prefix`, configure it with `selectors` — the whole
    /// `PutEventSelectors` body but the trail's name — and start it logging, asserting on the
    /// way that the trail reports itself logging. Answers the ARN `CreateTrail` minted, which
    /// is one of the two forms every one of these operations addresses a trail by.
    async fn logging_trail(
        &self,
        case: &str,
        trail: &str,
        bucket: &str,
        key_prefix: &str,
        selectors: &Value,
    ) -> String {
        let created = self
            .cloudtrail(
                case,
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
        let trail_arn = created.body["TrailARN"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: CreateTrail answers a TrailARN, got {}",
                    created.body
                )
            })
            .to_string();
        let mut body = json!({"TrailName": trail_arn});
        for (member, value) in selectors
            .as_object()
            .expect("a row's selectors are a document")
        {
            body[member] = value.clone();
        }
        self.cloudtrail(case, "PutEventSelectors", &body).await;
        self.cloudtrail(case, "StartLogging", &json!({"Name": trail_arn}))
            .await;
        let status = self
            .cloudtrail(case, "GetTrailStatus", &json!({"Name": trail_arn}))
            .await;
        assert_eq!(
            status.body.get("IsLogging"),
            Some(&json!(true)),
            "case {case:?}: expected the trail to report itself logging, got {}",
            status.body
        );
        trail_arn
    }

    /// Stop and remove the trail a row installed, so what one row delivered is read apart from
    /// every other row's.
    async fn remove_trail(&self, case: &str, trail_arn: &str) {
        self.cloudtrail(case, "StopLogging", &json!({"Name": trail_arn}))
            .await;
        self.cloudtrail(case, "DeleteTrail", &json!({"Name": trail_arn}))
            .await;
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

    /// The records one delivered log file holds, read through `GetObject` and gunzipped.
    async fn records(&self, bucket: &str, key: &str) -> Vec<Value> {
        let object = self
            .s3()
            .get_object()
            .bucket(bucket)
            .key(key)
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
        parsed
            .get(RECORDS_MEMBER)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("{key} must hold {RECORDS_MEMBER}, got {parsed}"))
    }

    /// Every record delivered under `prefix`, with the key each one arrived in.
    async fn delivered(&self, bucket: &str, prefix: &str) -> Vec<(String, Value)> {
        let mut delivered = Vec::new();
        for key in self.keys_under(bucket, prefix).await {
            for record in self.records(bucket, &key).await {
                delivered.push((key.clone(), record));
            }
        }
        delivered
    }

    /// Every record delivered under `prefix` once the call answered as `request_id` is among
    /// them — looked for until it is there, so a host that writes the log file a moment later
    /// waits rather than fails.
    async fn delivered_including(
        &self,
        bucket: &str,
        prefix: &str,
        request_id: &str,
    ) -> Vec<(String, Value)> {
        let mut delivered = Vec::new();
        for _ in 0..LOOKS {
            delivered = self.delivered(bucket, prefix).await;
            if answered_as(&delivered, request_id) > 0 {
                return delivered;
            }
            tokio::time::sleep(LOOK).await;
        }
        panic!(
            "expected the call answered as {request_id:?} delivered under {prefix:?}, got {} \
             record(s): {:?}",
            delivered.len(),
            event_names(&delivered)
        );
    }
}

/// The bucket policy a client attaches so CloudTrail may deliver a trail's log files into
/// `bucket`: the ACL check on the bucket and the object write under it, granted to the
/// CloudTrail service principal and scoped by the canned ACL the delivery carries.
fn cloudtrail_bucket_policy(bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSCloudTrailAclCheck",
                "Effect": "Allow",
                "Principal": {"Service": "cloudtrail.amazonaws.com"},
                "Action": "s3:GetBucketAcl",
                "Resource": format!("arn:aws:s3:::{bucket}"),
            },
            {
                "Sid": "AWSCloudTrailWrite",
                "Effect": "Allow",
                "Principal": {"Service": "cloudtrail.amazonaws.com"},
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
                "Condition": {
                    "StringEquals": {"s3:x-amz-acl": "bucket-owner-full-control"},
                },
            },
        ],
    })
    .to_string()
}

/// One field selector stating `Equals` over `values` — the only operator the category, the
/// resource type and the read/write field take.
fn equals(field: &str, values: &[&str]) -> Value {
    json!({"Field": field, "Equals": values})
}

/// One field selector stating `StartsWith` over `values`, which `resources.ARN` takes.
fn starts_with(field: &str, values: &[&str]) -> Value {
    json!({"Field": field, "StartsWith": values})
}

/// One advanced event selector named `name`, as the field selectors it states.
fn selector(name: &str, fields: Vec<Value>) -> Value {
    json!({"Name": name, "FieldSelectors": fields})
}

/// The selector a client writes for the account's management events, which is what a trail
/// logs by default and what the data selector below is added BESIDE.
fn management_selector() -> Value {
    selector(
        "management-events",
        vec![equals("eventCategory", &[MANAGEMENT_CATEGORY])],
    )
}

/// The selector a client writes to audit who ran ONE state machine: the `Data` category, the
/// state-machine resource type and that machine's own ARN.
fn machine_data_selector(machine: &str) -> Value {
    selector(
        "state-machine-data",
        vec![
            equals("eventCategory", &[DATA_CATEGORY]),
            equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
            equals("resources.ARN", &[machine]),
        ],
    )
}

/// A `PutEventSelectors` body (but the trail's name) carrying `selectors` as the trail's
/// advanced event selectors.
fn advanced(selectors: Vec<Value>) -> Value {
    json!({"AdvancedEventSelectors": selectors})
}

/// The trail's whole delivery area under the key prefix it was created with, which is where a
/// consumer of the trail looks for its files.
fn delivery_prefix(key_prefix: &str) -> String {
    format!("{key_prefix}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/")
}

/// The text a record holds at `member`, or `""` for one that holds none.
fn text(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// How many of `delivered` are the record of the call answered as `request_id`.
fn answered_as(delivered: &[(String, Value)], request_id: &str) -> usize {
    delivered
        .iter()
        .filter(|(_, record)| text(record, "requestID") == request_id)
        .count()
}

/// The operations `delivered` holds records of, for a failure to report what did arrive.
fn event_names(delivered: &[(String, Value)]) -> Vec<String> {
    delivered
        .iter()
        .map(|(_, record)| text(record, "eventName"))
        .collect()
}

/// Assert `key` is laid out the way a CloudTrail log file of this trail is: the trail's own
/// delivery area, then the Region, the day, and a file named for the account, the service and
/// the Region.
fn assert_log_file_key(case: &str, key: &str, key_prefix: &str) {
    let prefix = format!("{}{REGION}/", delivery_prefix(key_prefix));
    assert!(
        key.starts_with(&prefix),
        "case {case:?}: expected a log file under {prefix:?}, got {key:?}"
    );
    let segments: Vec<&str> = key[prefix.len()..].split('/').collect();
    assert_eq!(
        segments.len(),
        4,
        "case {case:?}: expected <year>/<month>/<day>/<file> under {prefix:?}, got {key:?}"
    );
    for (index, width) in [(0_usize, 4_usize), (1, 2), (2, 2)] {
        let segment = segments[index];
        assert!(
            segment.len() == width && segment.chars().all(char::is_numeric),
            "case {case:?}: expected a {width}-digit date segment, got {segment:?} in {key:?}"
        );
    }
    let file = segments[3];
    let expected_start = format!("{ACCOUNT}_{LOG_FILE_SERVICE_SEGMENT}_{REGION}_");
    assert!(
        file.starts_with(&expected_start) && file.ends_with(LOG_FILE_SUFFIX),
        "case {case:?}: expected a file named {expected_start}<minute>_<unique>\
         {LOG_FILE_SUFFIX}, got {file:?}"
    );
}

/// RULE: a synchronous run is delivered as a DATA event of the state machine it ran — and only
/// once a selector of the trail admits it, which a selector stating the `Management` category
/// never does.
///
/// The whole client sequence: a trail logging management events only, a run nothing delivers,
/// the data selector added beside the management one, and the next run delivered as a data
/// event of the machine it ran — with the record's own members, and with a run of ANOTHER
/// machine the ARN-scoped selector does not admit.
#[tokio::test]
async fn a_state_machine_data_selector_makes_the_next_sync_run_a_delivered_data_event() {
    let emulator = Emulator::new();
    let bucket = "sfn-trail-data-selector-logs";
    let key_prefix = "obs/sfn";
    let area = delivery_prefix(key_prefix);
    emulator.log_bucket(bucket).await;
    let subject = emulator.express_machine("data-selector-subject").await;
    let bystander = emulator.express_machine("data-selector-bystander").await;

    // Before: the trail carries the management selector alone.
    let before_case = "before the data selector is added";
    let trail_arn = emulator
        .logging_trail(
            before_case,
            "sfn-data-selector-trail",
            bucket,
            key_prefix,
            &advanced(vec![management_selector()]),
        )
        .await;
    let before = emulator.sync_run(before_case, &subject, "before").await;
    // A management call made AFTER that run, so the absence asserted next is not a race: the
    // trail has demonstrably delivered a call served later than the run.
    let marker = emulator
        .step_functions(
            before_case,
            "DescribeStateMachine",
            &json!({"stateMachineArn": &subject}),
        )
        .await;
    let delivered = emulator
        .delivered_including(bucket, &area, &marker.request_id)
        .await;
    assert_eq!(
        answered_as(&delivered, &before.request_id),
        0,
        "case {before_case:?}: a management selector admits no data event, yet the run answered \
         as {:?} was delivered among {:?}",
        before.request_id,
        event_names(&delivered)
    );
    assert!(
        !event_names(&delivered).contains(&SYNC_START_EVENT_NAME.to_string()),
        "case {before_case:?}: expected no {SYNC_START_EVENT_NAME} record at all, got {:?}",
        event_names(&delivered)
    );

    // The client adds the data selector beside the management one, and reads it back.
    let case = "after the data selector is added";
    let selectors = advanced(vec![management_selector(), machine_data_selector(&subject)]);
    let mut put = json!({"TrailName": trail_arn});
    put["AdvancedEventSelectors"] = selectors["AdvancedEventSelectors"].clone();
    let applied = emulator.cloudtrail(case, "PutEventSelectors", &put).await;
    assert_eq!(
        applied.body.get("AdvancedEventSelectors"),
        selectors.get("AdvancedEventSelectors"),
        "case {case:?}: expected the selectors the client wrote, got {}",
        applied.body
    );
    let read_back = emulator
        .cloudtrail(case, "GetEventSelectors", &json!({"TrailName": trail_arn}))
        .await;
    assert_eq!(
        read_back.body.get("AdvancedEventSelectors"),
        selectors.get("AdvancedEventSelectors"),
        "case {case:?}: expected the selectors reported verbatim, got {}",
        read_back.body
    );

    // After: the run on the named machine, and one on a machine the selector does not name.
    let after = emulator.sync_run(case, &subject, "after").await;
    let other = emulator.sync_run(case, &bystander, "bystander").await;
    let delivered = emulator
        .delivered_including(bucket, &area, &after.request_id)
        .await;
    assert_eq!(
        answered_as(&delivered, &other.request_id),
        0,
        "case {case:?}: a selector scoped to one machine's ARN admits no run of another, yet \
         the run answered as {:?} was delivered among {:?}",
        other.request_id,
        event_names(&delivered)
    );
    let (key, record) = delivered
        .iter()
        .find(|(_, record)| text(record, "requestID") == after.request_id)
        .expect("the delivered record of the run just made");
    assert_log_file_key(case, key, key_prefix);
    for (member, expected) in [
        ("eventName", SYNC_START_EVENT_NAME.to_string()),
        ("eventSource", EVENT_SOURCE.to_string()),
        ("eventCategory", DATA_CATEGORY.to_string()),
        ("awsRegion", REGION.to_string()),
        ("recipientAccountId", ACCOUNT.to_string()),
        ("requestID", after.request_id.clone()),
    ] {
        let actual = text(record, member);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?}, got {actual:?} in {record}"
        );
    }
    assert_eq!(
        record.get("managementEvent"),
        Some(&json!(false)),
        "case {case:?}: a data event is no management event, got {record}"
    );
    assert_eq!(
        record.get("readOnly"),
        Some(&json!(false)),
        "case {case:?}: a run changes the machine's state, got {record}"
    );
    assert_eq!(
        record.get("responseElements"),
        Some(&Value::Null),
        "case {case:?}: a data event reports no responseElements, got {record}"
    );
    assert_eq!(
        record.get("resources"),
        Some(&json!([{
            "accountId": ACCOUNT,
            "type": STATE_MACHINE_RESOURCE_TYPE,
            "ARN": subject,
        }])),
        "case {case:?}: expected the machine named as the call's resource, got {record}"
    );
    assert_eq!(
        record.pointer("/requestParameters/stateMachineArn"),
        Some(&json!(subject)),
        "case {case:?}: expected the machine the request addressed, got {record}"
    );
    assert_eq!(
        record.pointer("/requestParameters/input"),
        Some(&json!(HIDDEN_SENSITIVE_VALUE)),
        "case {case:?}: the run's own input is withheld, got {record}"
    );
    assert!(
        !record.to_string().contains("n0t-in-a-trail-log-file"),
        "case {case:?}: no record may carry the run's own data, got {record}"
    );

    // A data event is not event history, while the account's management calls are.
    let looked_up = emulator.cloudtrail(case, "LookupEvents", &json!({})).await;
    let names: Vec<String> = looked_up.body["Events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["EventName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.contains(&SYNC_START_EVENT_NAME.to_string()),
        "case {case:?}: no data event may reach event history, got {names:?}"
    );
    assert!(
        names.contains(&"CreateTrail".to_string()),
        "case {case:?}: the account's management calls stay in event history, got {names:?}"
    );
}

/// One row of the delivery table: the selectors a client installs on the trail, the call it
/// then makes, and whether that trail delivers a record for it.
struct Delivery {
    case: &'static str,
    /// The trail's event selectors, as the `PutEventSelectors` body but the trail's name.
    selectors: Value,
    /// Whether the call is made on the SUBJECT machine; the other one is the bystander an
    /// ARN-scoped selector must not admit.
    subject: bool,
    /// Whether the call is the SYNCHRONOUS start — this service's data plane — or the
    /// asynchronous one, which is administration of the account's resources.
    synchronous: bool,
    /// How many records the trail must have delivered for that call.
    delivered: usize,
}

/// RULE: a selector admits a run only when EVERY one of its field selectors does, so a trail
/// delivers the record of a synchronous run exactly where its own selectors admit it.
///
/// The rows differ along which fields the client's selector pins, the values and the operators
/// it pins them with, the machine the run was made on, and whether the call is the synchronous
/// data plane at all.
#[tokio::test]
async fn a_sync_run_is_delivered_only_where_the_trails_own_selectors_admit_it() {
    let emulator = Emulator::new();
    let bucket = "sfn-selector-rows-logs";
    emulator.log_bucket(bucket).await;
    let subject = emulator.express_machine("selector-rows-subject").await;
    let bystander = emulator.express_machine("selector-rows-bystander").await;
    // Every state machine of this account and Region shares this ARN prefix, which is what a
    // selector matching by `StartsWith` names.
    let machine_prefix = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:");

    let rows: Vec<Delivery> = vec![
        Delivery {
            case: "the management selector alone",
            selectors: advanced(vec![management_selector()]),
            subject: true,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "the data selector for every state machine of the account",
            selectors: advanced(vec![selector(
                "every-machine",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "that same selector, for a run of another machine of the account",
            selectors: advanced(vec![selector(
                "every-machine",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
                ],
            )]),
            subject: false,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a selector scoped to the machine the run was made on",
            selectors: advanced(vec![machine_data_selector(&subject)]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a selector scoped to one machine, for a run of the other",
            selectors: advanced(vec![machine_data_selector(&subject)]),
            subject: false,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "the data selector beside the management one",
            selectors: advanced(vec![management_selector(), machine_data_selector(&subject)]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a selector matching the machine's ARN by prefix",
            selectors: advanced(vec![selector(
                "machines-by-prefix",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
                    starts_with("resources.ARN", &[&machine_prefix]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a selector whose ARN prefix no machine of this Region carries",
            selectors: advanced(vec![selector(
                "machines-of-another-region",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
                    starts_with(
                        "resources.ARN",
                        &[&format!("arn:aws:states:eu-west-1:{ACCOUNT}:stateMachine:")],
                    ),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "a data selector over another resource type",
            selectors: advanced(vec![selector(
                "objects",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[S3_OBJECT_TYPE]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "a data selector narrowed to reads, which a run is not",
            selectors: advanced(vec![selector(
                "reads",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("readOnly", &["true"]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "a data selector narrowed to writes, which a run is",
            selectors: advanced(vec![selector(
                "writes",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("readOnly", &["false"]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a data selector naming the synchronous start",
            selectors: advanced(vec![selector(
                "the-sync-start",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("eventName", &[SYNC_START_EVENT_NAME]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 1,
        },
        Delivery {
            case: "a data selector naming the asynchronous start",
            selectors: advanced(vec![selector(
                "the-async-start",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("eventName", &[ASYNC_START_EVENT_NAME]),
                ],
            )]),
            subject: true,
            synchronous: true,
            delivered: 0,
        },
        Delivery {
            case: "the asynchronous start, which is administration and no data event",
            selectors: advanced(vec![selector(
                "every-machine",
                vec![
                    equals("eventCategory", &[DATA_CATEGORY]),
                    equals("resources.type", &[STATE_MACHINE_RESOURCE_TYPE]),
                ],
            )]),
            subject: true,
            synchronous: false,
            delivered: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One trail per row, each under a key prefix of its own and removed once the row has
        // been read, so what one row delivered is never read as another's.
        let trail = format!("selector-row-{index}");
        let key_prefix = format!("rows/{index}");
        let trail_arn = emulator
            .logging_trail(row.case, &trail, bucket, &key_prefix, &row.selectors)
            .await;
        let machine = if row.subject { &subject } else { &bystander };
        let answered = if row.synchronous {
            emulator
                .sync_run(row.case, machine, &format!("row-{index}"))
                .await
        } else {
            emulator
                .step_functions(
                    row.case,
                    ASYNC_START_EVENT_NAME,
                    &json!({
                        "stateMachineArn": machine,
                        "name": format!("row-{index}"),
                        "input": SECRET_INPUT,
                    }),
                )
                .await
        };

        let delivered = if row.delivered > 0 {
            emulator
                .delivered_including(bucket, &delivery_prefix(&key_prefix), &answered.request_id)
                .await
        } else {
            emulator
                .delivered(bucket, &delivery_prefix(&key_prefix))
                .await
        };
        let actual = answered_as(&delivered, &answered.request_id);
        assert_eq!(
            actual, row.delivered,
            "case {:?}: expected {} delivered record(s) of the call answered as {:?}, got \
             {actual} among {:?}",
            row.case,
            row.delivered,
            answered.request_id,
            event_names(&delivered)
        );
        for (key, record) in &delivered {
            if text(record, "requestID") != answered.request_id {
                continue;
            }
            assert_log_file_key(row.case, key, &key_prefix);
            assert_eq!(
                text(record, "eventCategory"),
                DATA_CATEGORY,
                "case {:?}: a selector-matched run is a data event, got {record}",
                row.case
            );
        }
        emulator.remove_trail(row.case, &trail_arn).await;
    }
}
