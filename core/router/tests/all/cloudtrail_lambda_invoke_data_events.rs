// SPDX-License-Identifier: CC0-1.0

//! What a LOGGING trail delivers for the account's Lambda INVOCATIONS: the calls that run a
//! function, as against the control-plane calls that administer the account's functions.
//!
//! Three services are mounted on ONE engine, because the behaviour spans them: AWS Lambda
//! serves the invocation, CloudTrail holds the trail and decides the delivery, and Amazon S3
//! is where the log files land. Every call goes over the wire the way a client makes it, and
//! the log bucket is created, policied and listed through Amazon S3's own API over the same
//! router, so a delivered log file counts only if the object store itself answers with it.
//!
//! RULE: a call on the CONTENTS of a resource is a CloudTrail DATA event of that resource —
//! recorded in no event history (a lookup serves management and Insights events) and
//! delivered as a log file into the bucket of each trail whose event selectors admit it,
//! under that trail's own key prefix and the documented key layout. For AWS Lambda the call
//! on a function's contents is the INVOCATION, whose record carries the `Data` category, the
//! operation as its `eventName`, the invocation's write class, and the function it ran as its
//! one resource — by the `AWS::Lambda::Function` type and that function's ARN, which is what
//! a selector selects it by. A trail with no selector admitting it receives nothing.
//!
//! The trail's own SELECTOR is what each row asserts the record through: a selector admits an
//! event only when every one of its field selectors does, so a file arriving under a trail
//! that pins the category, the resource type, the resource ARN, the event name and the
//! read/write class is that record carrying those very values — and a near miss on any one of
//! them arriving nowhere is the same statement from the other side. The rows therefore span
//! what the rule generalises over: which fields the trail's selector pins and which values it
//! pins them to, whether the selector is an ADVANCED one or a BASIC `DataResource`, whether
//! the call was made on the function an ARN-scoped selector names or on a sibling, which
//! INVOCATION TYPE the client asked for, and whether the call is an invocation at all.
//!
//! That an invocation is a DATA event rather than a management one is the second test: its
//! record reaches the trail's bucket and never the event history a lookup reports, while the
//! control-plane calls of the same account stay in that history.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the trails are homed in
/// and so the Region their log files land under.
const REGION: &str = "us-east-1";

/// The account every emulated resource and every delivered record belongs to.
const ACCOUNT: &str = "000000000000";

/// The `eventCategory` of a data event.
const DATA_CATEGORY: &str = "Data";

/// The resource type a function is named by in a delivered record's `resources` and in a
/// selector's `resources.type` field — one of the three types a basic `DataResource$Type`
/// lists.
const FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";

/// Another data-event resource type, for the selector that admits a class this account never
/// serves here.
const OBJECT_RESOURCE_TYPE: &str = "AWS::S3::Object";

/// The operation an invocation is recorded as, and a control-plane operation of the same
/// function for the selector that names another one.
const INVOKE_EVENT_NAME: &str = "Invoke";
const CONTROL_PLANE_EVENT_NAME: &str = "GetFunction";

/// The segments of a log file's key, and what its name ends with.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";
const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The bucket every trail below delivers into.
const LOG_BUCKET: &str = "lambda-invoke-data-event-logs";

/// An execution role ARN satisfying the control plane's modelled `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// A `Code.ZipFile` payload: `restJson1` carries a blob base64-encoded in the JSON body. This
/// is the 22-byte empty archive, which is a readable `.zip` holding no entry — enough for
/// every invocation type below, whose ANSWER is the API's and not the handler's.
const ZIP_FILE: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// The runtime the functions below are created with, and the handler their configuration
/// names.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How often, and for how long at most, a poll looks for a condition the emulator's own clock
/// owes (a created function reaching `Active`). The deadline is far above the span the service
/// takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

/// One answer as its caller reads it: the status, the raw body and the body parsed as JSON.
struct Answered {
    status: u16,
    text: String,
    body: Value,
}

/// AWS Lambda, CloudTrail and Amazon S3 over one shared engine, exactly as
/// `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region and the caller.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Dispatch one request and read back its status and body.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answered { status, text, body }
}

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be served.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answered = dispatch(
        router,
        "POST",
        "cloudtrail",
        "/",
        "application/x-amz-json-1.1",
        body.to_string().into_bytes(),
        &[(
            "x-amz-target",
            &format!("CloudTrail_20131101.{operation}"),
        )],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
        answered.status, answered.text
    );
    answered.body
}

/// One Lambda call (`restJson1`), answered as it stands — a row asserts the status itself.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answered {
    dispatch(
        router,
        method,
        "lambda",
        uri,
        "application/json",
        body,
        headers,
    )
    .await
}

/// Create the bucket the trails deliver into, carrying the policy that lets CloudTrail write
/// its log files there — exactly the two calls a client makes.
async fn log_bucket(router: &MultiServiceRouter) {
    let created = dispatch(
        router,
        "PUT",
        "s3",
        &format!("/{LOG_BUCKET}"),
        "application/xml",
        Vec::new(),
        &[],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateBucket({LOG_BUCKET:?}) must succeed, got {} with {}",
        created.status, created.text
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{LOG_BUCKET}/*"),
            "Condition": {
                "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
            },
        }],
    })
    .to_string();
    let policied = dispatch(
        router,
        "PUT",
        "s3",
        &format!("/{LOG_BUCKET}?policy"),
        "application/json",
        policy.into_bytes(),
        &[],
    )
    .await;
    assert!(
        (200..300).contains(&policied.status),
        "setup: PutBucketPolicy({LOG_BUCKET:?}) must succeed, got {} with {}",
        policied.status,
        policied.text
    );
}

/// Create one function and wait until Lambda reports it `Active`, which is the state an
/// invocation needs, and answer its ARN.
async fn function(router: &MultiServiceRouter, name: &str) -> String {
    let created = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Code": { "ZipFile": ZIP_FILE },
        }))
        .expect("the create request serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction({name:?}) must succeed, got {} with {}",
        created.status, created.text
    );
    let arn = created.body["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateFunction({name:?}) answers a FunctionArn"))
        .to_string();
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            Vec::new(),
            &[],
        )
        .await;
        if read.body["State"].as_str() == Some("Active") {
            return arn;
        }
        last = read.body;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Point a trail at the log bucket under `key_prefix` — `None` for a trail that carries
/// none, whose files land at the bucket's root — configure it with `selectors` and start it
/// logging: the calls a client makes, each of which must be served.
///
/// `selectors` is the whole `PutEventSelectors` body but the trail's name, so a row can
/// configure the trail with ADVANCED selectors or with BASIC ones. The trail is addressed by
/// the ARN `CreateTrail` answered from then on, which is one of the two forms every one of
/// these operations takes.
async fn logging_trail(
    router: &MultiServiceRouter,
    case: &str,
    trail: &str,
    key_prefix: Option<&str>,
    selectors: &Value,
) -> String {
    let mut created = json!({
        "Name": trail,
        "S3BucketName": LOG_BUCKET,
        "IsMultiRegionTrail": false,
        "IncludeGlobalServiceEvents": false,
    });
    if let Some(key_prefix) = key_prefix {
        created["S3KeyPrefix"] = json!(key_prefix);
    }
    let created = cloudtrail(router, case, "CreateTrail", &created).await;
    let trail_arn = created["TrailARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateTrail answers a TrailARN, got {created}"))
        .to_string();
    let mut body = json!({ "TrailName": trail_arn });
    for (member, value) in selectors
        .as_object()
        .expect("a row's selectors are a document")
    {
        body[member] = value.clone();
    }
    cloudtrail(router, case, "PutEventSelectors", &body).await;
    cloudtrail(router, case, "StartLogging", &json!({ "Name": trail_arn })).await;
    let status = cloudtrail(router, case, "GetTrailStatus", &json!({ "Name": trail_arn })).await;
    assert_eq!(
        status.get("IsLogging"),
        Some(&json!(true)),
        "case {case:?}: expected the trail to report itself logging, got {status}"
    );
    trail_arn
}

/// Stop and remove the trail a row installed, so what one row delivered is read apart from
/// every other row's.
async fn remove_trail(router: &MultiServiceRouter, case: &str, trail_arn: &str) {
    cloudtrail(router, case, "StopLogging", &json!({ "Name": trail_arn })).await;
    cloudtrail(router, case, "DeleteTrail", &json!({ "Name": trail_arn })).await;
}

/// The trail's whole delivery area under the key prefix it was created with, which is where a
/// consumer of the trail looks for its files — the bucket's own root for a trail that carries
/// no prefix.
fn delivery_prefix(key_prefix: Option<&str>) -> String {
    let prefix = key_prefix.map(|prefix| format!("{prefix}/")).unwrap_or_default();
    format!("{prefix}{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/")
}

/// Every object key the log bucket holds under `prefix`, as Amazon S3's own `ListObjectsV2`
/// reports them.
async fn keys_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let listed = dispatch(
        router,
        "GET",
        "s3",
        &format!("/{LOG_BUCKET}?list-type=2&prefix={prefix}"),
        "application/xml",
        Vec::new(),
        &[],
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: listing {LOG_BUCKET}/{prefix} must succeed, got {} with {}",
        listed.status, listed.text
    );
    let mut keys: Vec<String> = Vec::new();
    let mut rest = listed.text.as_str();
    while let Some(start) = rest.find("<Key>") {
        let after = &rest[start + "<Key>".len()..];
        let Some(end) = after.find("</Key>") else { break };
        keys.push(after[..end].to_string());
        rest = &after[end..];
    }
    keys.sort();
    keys
}

/// Assert `key` is laid out the way a CloudTrail log file of this trail is: the trail's own
/// delivery area, then the Region, the day, and a file named for the account, the service and
/// the Region.
fn assert_log_file_key(case: &str, key: &str, key_prefix: Option<&str>) {
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

/// One advanced event selector, as the field selectors it states.
fn advanced(fields: Vec<Value>) -> Value {
    json!({ "AdvancedEventSelectors": [{ "Name": "the row's selector", "FieldSelectors": fields }] })
}

/// One field selector stating `Equals` over `values`, which is the only operator the category,
/// the resource type and the read/write field take.
fn equals(field: &str, values: &[&str]) -> Value {
    json!({ "Field": field, "Equals": values })
}

/// The selector a client installs to log the invocation data events of every function of the
/// account: the `Data` category and the function resource type.
fn published_function_selector() -> Value {
    advanced(vec![
        equals("eventCategory", &[DATA_CATEGORY]),
        equals("resources.type", &[FUNCTION_RESOURCE_TYPE]),
    ])
}

/// Which call a row makes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    /// An invocation of the stated `InvocationType`, which is what the client asks for in the
    /// `X-Amz-Invocation-Type` header.
    Invoke(&'static str),
    /// A control-plane read of the same function, which is a management call.
    Read,
}

/// One row of the delivery table: the trail the account carries, the call the client makes,
/// and whether that trail delivers a record for it.
struct Delivery {
    case: &'static str,
    /// The trail's event selectors, as the `PutEventSelectors` body but the trail's name.
    selectors: Value,
    /// Whether the call is made on the SUBJECT function; the other is the bystander an
    /// ARN-scoped selector must not admit.
    subject: bool,
    /// The call the client makes.
    call: Call,
    /// How many records the trail must have delivered for it.
    delivered: usize,
}

/// RULE: an invocation is delivered as a DATA event of the function it ran, into the bucket of
/// each trail whose event selectors admit it and of no other.
#[tokio::test]
async fn an_invocation_is_delivered_only_to_a_trail_whose_selector_admits_it() {
    let router = router();
    log_bucket(&router).await;
    let subject = function(&router, "invoke-data-event-subject").await;
    let bystander = function(&router, "invoke-data-event-bystander").await;

    let rows: Vec<Delivery> = vec![
        Delivery {
            case: "the published function data selector",
            selectors: published_function_selector(),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a basic data resource naming the function type and the function's ARN",
            selectors: json!({ "EventSelectors": [{
                "ReadWriteType": "All",
                "IncludeManagementEvents": false,
                "DataResources": [{
                    "Type": FUNCTION_RESOURCE_TYPE,
                    "Values": [subject.clone()],
                }],
            }] }),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a basic data resource naming a sibling function's ARN",
            selectors: json!({ "EventSelectors": [{
                "ReadWriteType": "All",
                "IncludeManagementEvents": false,
                "DataResources": [{
                    "Type": FUNCTION_RESOURCE_TYPE,
                    "Values": [bystander.clone()],
                }],
            }] }),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 0,
        },
        Delivery {
            case: "a selector scoped to the function the call invoked",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("resources.type", &[FUNCTION_RESOURCE_TYPE]),
                equals("resources.ARN", &[&subject]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a selector scoped to a sibling function, called on the sibling",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("resources.ARN", &[&bystander]),
            ]),
            subject: false,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a selector scoped to a sibling function, called on the subject",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("resources.ARN", &[&bystander]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 0,
        },
        Delivery {
            case: "a data selector over another resource type",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("resources.type", &[OBJECT_RESOURCE_TYPE]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 0,
        },
        Delivery {
            case: "a data selector narrowed to reads, which an invocation is not",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("readOnly", &["true"]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 0,
        },
        Delivery {
            case: "a data selector narrowed to writes, which an invocation is",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("readOnly", &["false"]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a data selector naming the invocation",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("eventName", &[INVOKE_EVENT_NAME]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 1,
        },
        Delivery {
            case: "a data selector naming a control-plane operation",
            selectors: advanced(vec![
                equals("eventCategory", &[DATA_CATEGORY]),
                equals("eventName", &[CONTROL_PLANE_EVENT_NAME]),
            ]),
            subject: true,
            call: Call::Invoke("DryRun"),
            delivered: 0,
        },
        Delivery {
            case: "an asynchronous invocation",
            selectors: published_function_selector(),
            subject: true,
            call: Call::Invoke("Event"),
            delivered: 1,
        },
        Delivery {
            case: "a synchronous invocation",
            selectors: published_function_selector(),
            subject: true,
            call: Call::Invoke("RequestResponse"),
            delivered: 1,
        },
        Delivery {
            case: "a control-plane read of the same function",
            selectors: published_function_selector(),
            subject: true,
            call: Call::Read,
            delivered: 0,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One trail per row, each with a key prefix of its own, removed once the row has been
        // read — so what one row delivered is never read as another's.
        let trail = format!("row-{index}-trail");
        let key_prefix = format!("row-{index}");
        let trail_arn =
            logging_trail(&router, row.case, &trail, Some(&key_prefix), &row.selectors).await;
        let invoked = if row.subject { &subject } else { &bystander };
        match row.call {
            Call::Invoke(invocation_type) => {
                let answered = lambda(
                    &router,
                    "POST",
                    &format!("/2015-03-31/functions/{invoked}/invocations"),
                    br#"{"probe":true}"#.to_vec(),
                    &[("x-amz-invocation-type", invocation_type)],
                )
                .await;
                // What a run REPORTS is the handler's and not the API's: "the status code in
                // the API response doesn't reflect function errors", and the record of the
                // call is delivered whatever the run answered. So the row asserts only that
                // the invocation was SERVED.
                assert!(
                    (200..300).contains(&answered.status),
                    "case {:?}: the {invocation_type} invocation must be served, got {} with {}",
                    row.case,
                    answered.status,
                    answered.text
                );
            }
            Call::Read => {
                let answered = lambda(
                    &router,
                    "GET",
                    &format!("/2015-03-31/functions/{invoked}"),
                    Vec::new(),
                    &[],
                )
                .await;
                assert_eq!(
                    answered.status, 200,
                    "case {:?}: the control-plane read must be served, got {} with {}",
                    row.case, answered.status, answered.text
                );
            }
        }

        let delivered = keys_under(&router, row.case, &delivery_prefix(Some(&key_prefix))).await;
        assert_eq!(
            delivered.len(),
            row.delivered,
            "case {:?}: expected {} delivered log file(s), got {delivered:?}",
            row.case,
            row.delivered
        );
        for key in &delivered {
            assert_log_file_key(row.case, key, Some(&key_prefix));
        }
        remove_trail(&router, row.case, &trail_arn).await;
    }
}

/// RULE: a data event reaches the trail's bucket and NO event history — "data events are not
/// logged by default when you create a trail", and a lookup serves management and Insights
/// events — while the control-plane calls of the same account stay in that history.
///
/// Served on a MULTI-THREADED runtime, as the emulator's own entrypoint runs: the record of a
/// call is handed from the operation that served it to the layer that delivers it, so a
/// hand-over that only holds while one thread drives everything would pass the suite above and
/// fail the moment a second worker exists. The trail here carries NO key prefix either, so its
/// files land at the bucket's root under the documented layout, and the invocation is the
/// SYNCHRONOUS one a client makes by default.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invocation_is_delivered_as_a_data_event_and_never_into_event_history() {
    let router = router();
    log_bucket(&router).await;
    let subject = function(&router, "event-history-subject").await;
    let case = "a trail logging every function data event";
    logging_trail(
        &router,
        case,
        "event-history-trail",
        None,
        &advanced(vec![
            equals("eventCategory", &[DATA_CATEGORY]),
            equals("resources.type", &[FUNCTION_RESOURCE_TYPE]),
            equals("resources.ARN", &[&subject]),
        ]),
    )
    .await;

    let answered = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{subject}/invocations"),
        br#"{"probe":true}"#.to_vec(),
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert!(
        (200..300).contains(&answered.status),
        "case {case:?}: the invocation must be served, got {} with {}",
        answered.status,
        answered.text
    );

    // Looked for until it is there: the delivery is made while the call is served, so one look
    // is normally enough — the loop is what keeps a loaded host waiting rather than failing.
    let mut delivered: Vec<String> = Vec::new();
    for _ in 0..LOOKS {
        delivered = keys_under(&router, case, &delivery_prefix(None)).await;
        if !delivered.is_empty() {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        delivered.len(),
        1,
        "case {case:?}: expected the invocation's one log file, got {delivered:?}"
    );
    assert_log_file_key(case, &delivered[0], None);

    let looked_up = cloudtrail(&router, case, "LookupEvents", &json!({})).await;
    let names: Vec<String> = looked_up["Events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["EventName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.contains(&INVOKE_EVENT_NAME.to_string()),
        "case {case:?}: no data event may reach event history, got {names:?}"
    );
    assert!(
        names.contains(&"CreateTrail".to_string()),
        "case {case:?}: the account's management calls stay in event history, got {names:?}"
    );
}
