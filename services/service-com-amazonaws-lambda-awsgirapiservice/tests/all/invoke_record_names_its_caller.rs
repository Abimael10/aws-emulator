// SPDX-License-Identifier: CC0-1.0

//! The JOIN between an invocation's answer and the CloudTrail record of that same call.
//!
//! The rule under test: a response names the request it answered in the `x-amzn-requestid`
//! response header — which is what an AWS SDK surfaces as `ResponseMetadata.RequestId` — and
//! the CloudTrail record of that very call carries that same id as its `requestID`. So a
//! client holding an invocation's response can find the audit record of the invocation it
//! made: the id is one id, read off the answer and written into the record, on an answer that
//! ran a handler as on one that ran nothing.
//!
//! Three services are mounted on ONE engine, because the join spans them: AWS Lambda serves
//! the invocation, CloudTrail holds the trail that decides the delivery, and Amazon S3 is
//! where its log files land. Every call goes over the wire the way a client makes it, and the
//! log bucket is created, policied, listed and read through Amazon S3's own API, so a record
//! counts only if the object store itself answers with the file holding it.
//!
//! An invocation is a DATA event of the function it ran, so the trail carries an advanced
//! selector naming that category and the function resource type; what such a trail delivers,
//! and to which trail, is the subject of
//! `core/router/tests/all/cloudtrail_lambda_invoke_data_events.rs` and is not restated here.
//! What this suite states is the ID on the record, and its rows differ along what the rule
//! generalises over: the invocation type the client asked for (`RequestResponse`, `Event`,
//! `DryRun` — so a call whose handler RAN, one accepted for later and one that runs nothing),
//! the qualifier the call named (the unpublished version, a published version, an alias) and
//! how the run ended (a returned value, a raised exception).

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, is_request_id, one_invocation_stream_at_a_time};
use crate::synchronous_invoke_run::package;

/// The Region every call below is signed for, which is the Region the trail is homed in and
/// so the Region its log files land under.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function here is created with, and the handler its configuration names
/// for the package [`package`] writes.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "lambda_function.lambda_handler";

/// The bucket the trail delivers into, the trail itself, and the key prefix its files land
/// under.
const LOG_BUCKET: &str = "lambda-invoke-record-join-logs";
const TRAIL: &str = "lambda-invoke-record-join-trail";
const KEY_PREFIX: &str = "invoke-join";

/// The header a response names the request it answered in, lowercased as HTTP compares header
/// names, and spelled out here rather than read from the crate under test so the assertion
/// does not merely compare the implementation with itself.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The member a delivered log file holds its records in.
const RECORDS_MEMBER: &str = "Records";

/// The `eventCategory` of a data event, and the `eventSource` CloudTrail stamps on a record of
/// this service (`smithy_query(query="service_info")` on the Lambda model -> `endpointPrefix:
/// lambda`).
const DATA_CATEGORY: &str = "Data";
const EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The operation an invocation is recorded as, and the type a function is named by in a
/// record's `resources` and in a selector's `resources.type` field — one of the types a basic
/// `DataResource$Type` lists.
const INVOKE_EVENT_NAME: &str = "Invoke";
const FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";

/// The version an invocation that named no qualifier ran, as the record reports it beside the
/// parameters (`Qualifier`: "Specify a version or alias to invoke a published version of the
/// function"; the unpublished version is `$LATEST`).
const VERSION_LATEST: &str = "$LATEST";

/// How often, and for how long at most, a poll looks for a condition the emulator owes (a
/// created function reporting itself `Active`, a log file reaching the bucket). The deadline is
/// far above the span either takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds: it bounds the WALL
/// CLOCK one run may spend, and the handlers here return at once, so what it has to cover is
/// the host's own scheduling while this target's suites run in parallel — not the handler's
/// work.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while this process's own host bounds answer
/// instead of a run.
const HOST_STOP_ATTEMPTS: usize = 6;

/// What one request came back with.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The id this answer names its request by.
    fn request_id(&self) -> String {
        self.header(REQUEST_ID_HEADER).unwrap_or_default()
    }
}

/// AWS Lambda, CloudTrail and Amazon S3 over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope is
    // still carried, because it is what names the Region and routes the request.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
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
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request and read back everything its caller sees.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization(signing_name))
        .header("content-type", content_type);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One AWS Lambda call (`restJson1`), answered as it stands — a row asserts the status itself.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answer {
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

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be served.
async fn cloudtrail(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let answered = dispatch(
        router,
        "POST",
        "cloudtrail",
        "/",
        "application/x-amz-json-1.1",
        body.to_string().into_bytes(),
        &[("x-amz-target", &format!("CloudTrail_20131101.{operation}"))],
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup: CloudTrail {operation} must succeed, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One Amazon S3 request, path style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: Vec<u8>) -> Answer {
    dispatch(router, method, "s3", path, "application/xml", body, &[]).await
}

/// Create the bucket the trail delivers into, carrying the policy that lets CloudTrail write
/// its log files there — exactly the two calls a client makes.
async fn log_bucket(router: &MultiServiceRouter) {
    let created = s3(router, "PUT", &format!("/{LOG_BUCKET}"), Vec::new()).await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateBucket({LOG_BUCKET:?}) must succeed, got {} with {}",
        created.status,
        created.text()
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
    let policied = s3(
        router,
        "PUT",
        &format!("/{LOG_BUCKET}?policy"),
        policy.into_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&policied.status),
        "setup: PutBucketPolicy({LOG_BUCKET:?}) must succeed, got {} with {}",
        policied.status,
        policied.text()
    );
}

/// Install the trail that logs this account's function data events and start it logging: the
/// calls a client makes, each of which must be served.
async fn logging_trail(router: &MultiServiceRouter) {
    let created = cloudtrail(
        router,
        "CreateTrail",
        &json!({
            "Name": TRAIL,
            "S3BucketName": LOG_BUCKET,
            "S3KeyPrefix": KEY_PREFIX,
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": false,
        }),
    )
    .await;
    let trail_arn = created["TrailARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateTrail answers a TrailARN, got {created}"))
        .to_string();
    cloudtrail(
        router,
        "PutEventSelectors",
        &json!({
            "TrailName": trail_arn,
            "AdvancedEventSelectors": [{
                "Name": "every function data event of the account",
                "FieldSelectors": [
                    { "Field": "eventCategory", "Equals": [DATA_CATEGORY] },
                    { "Field": "resources.type", "Equals": [FUNCTION_RESOURCE_TYPE] },
                ],
            }],
        }),
    )
    .await;
    cloudtrail(router, "StartLogging", &json!({ "Name": trail_arn })).await;
    let status = cloudtrail(router, "GetTrailStatus", &json!({ "Name": trail_arn })).await;
    assert_eq!(
        status.get("IsLogging"),
        Some(&json!(true)),
        "setup: expected the trail to report itself logging, got {status}"
    );
}

/// Create the function `name` from `source`, publish version 1, point the alias `live` at it,
/// wait out the activation window the create documents, and answer the ARN the create reported.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Publish": true,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateFunction({name:?}) must succeed, got {} with {}",
        created.status,
        created.text()
    );
    let arn = created.json()["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateFunction({name:?}) answers a FunctionArn"))
        .to_string();
    let alias = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/aliases"),
        serde_json::to_vec(&json!({ "Name": "live", "FunctionVersion": "1" }))
            .expect("the alias body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        alias.status,
        201,
        "setup: CreateAlias on {name:?} must succeed, got {} with {}",
        alias.status,
        alias.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
    arn
}

/// Every object key the log bucket holds under the trail's prefix, as Amazon S3's own
/// `ListObjectsV2` reports them.
async fn delivered_keys(router: &MultiServiceRouter) -> Vec<String> {
    let listed = s3(
        router,
        "GET",
        &format!("/{LOG_BUCKET}?list-type=2&prefix={KEY_PREFIX}"),
        Vec::new(),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "listing {LOG_BUCKET}/{KEY_PREFIX} must succeed, got {} with {}",
        listed.status,
        listed.text()
    );
    let text = listed.text();
    let mut keys: Vec<String> = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<Key>") {
        let after = &rest[start + "<Key>".len()..];
        let Some(end) = after.find("</Key>") else {
            break;
        };
        keys.push(after[..end].to_string());
        rest = &after[end..];
    }
    keys
}

/// Every record the trail has delivered so far, read out of the log files the object store
/// holds: a delivered file is a gzip stream holding one `Records` document.
async fn delivered_records(router: &MultiServiceRouter) -> Vec<Value> {
    let mut records: Vec<Value> = Vec::new();
    for key in delivered_keys(router).await {
        let object = s3(router, "GET", &format!("/{LOG_BUCKET}/{key}"), Vec::new()).await;
        assert_eq!(
            object.status,
            200,
            "reading the log file {key:?} must succeed, got {} with {}",
            object.status,
            object.text()
        );
        let mut document = String::new();
        flate2::read::GzDecoder::new(object.body.as_ref())
            .read_to_string(&mut document)
            .unwrap_or_else(|error| panic!("the log file {key:?} is a gzip stream: {error}"));
        let parsed: Value = serde_json::from_str(&document)
            .unwrap_or_else(|error| panic!("the log file {key:?} holds a document: {error}"));
        if let Some(held) = parsed.get(RECORDS_MEMBER).and_then(Value::as_array) {
            records.extend(held.iter().cloned());
        }
    }
    records
}

/// The one delivered record naming `request_id`, looked for until it is there.
///
/// The delivery is made once the response that names the request exists, so a record is
/// normally there on the first look; the loop is what keeps a loaded host waiting rather than
/// failing. Exactly one record may name the id — a second would be a second audit record of one
/// call.
async fn record_naming(router: &MultiServiceRouter, case: &str, request_id: &str) -> Value {
    let mut named: Vec<Value> = Vec::new();
    for _ in 0..LOOKS {
        named = delivered_records(router)
            .await
            .into_iter()
            .filter(|record| record.get("requestID") == Some(&json!(request_id)))
            .collect();
        if !named.is_empty() {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        named.len(),
        1,
        "case {case:?}: expected exactly one delivered record naming the request \
         {request_id:?} the caller was answered under, got {named:?}"
    );
    named.remove(0)
}

/// The text a record holds at `member`.
fn text(record: &Value, member: &str) -> String {
    record
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row: the call a client makes, and the version the record must report it ran.
struct Row {
    case: &'static str,
    /// Which of the two functions this suite created the call addresses.
    function: Function,
    /// The qualifier the call names, or `None` for the unpublished version.
    qualifier: Option<&'static str>,
    /// The `InvocationType` the client asks for in the `X-Amz-Invocation-Type` header.
    invocation_type: &'static str,
    /// The status that invocation type answers with.
    status: u16,
    /// Whether the run the call made FAILED, which the answer signals in
    /// `X-Amz-Function-Error`.
    failed: bool,
}

/// Which handler the addressed function runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Function {
    /// Returns the id the runtime gave the invocation.
    Returns,
    /// Raises, so the answer is the run's error document.
    Raises,
}

/// The handler that reports its own invocation, and the one that raises.
const REPORTS_ITS_REQUEST_ID: &str =
    "def lambda_handler(event, context):\n    return {'request': context.aws_request_id}\n";
const RAISES: &str = "def lambda_handler(event, context):\n    raise ValueError('boom')\n";

/// RULE: the id a response names the request it answered by is the id the CloudTrail record of
/// that call carries as its `requestID`, so a client holding an invocation's answer finds the
/// record of that very invocation.
#[tokio::test]
async fn an_invocations_record_names_the_request_its_caller_was_answered_under() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = router();
    log_bucket(&router).await;
    logging_trail(&router).await;
    let returning = "invoke-join-returns";
    let raising = "invoke-join-raises";
    let returning_arn = create_function(&router, returning, REPORTS_ITS_REQUEST_ID).await;
    let raising_arn = create_function(&router, raising, RAISES).await;

    let rows: &[Row] = &[
        Row {
            case: "a synchronous invocation of the unpublished version",
            function: Function::Returns,
            qualifier: None,
            invocation_type: "RequestResponse",
            status: 200,
            failed: false,
        },
        Row {
            case: "a synchronous invocation of a published version",
            function: Function::Returns,
            qualifier: Some("1"),
            invocation_type: "RequestResponse",
            status: 200,
            failed: false,
        },
        Row {
            case: "a synchronous invocation through an alias",
            function: Function::Returns,
            qualifier: Some("live"),
            invocation_type: "RequestResponse",
            status: 200,
            failed: false,
        },
        Row {
            case: "a synchronous invocation whose handler raised",
            function: Function::Raises,
            qualifier: None,
            invocation_type: "RequestResponse",
            status: 200,
            failed: true,
        },
        Row {
            case: "an asynchronous invocation, accepted for later",
            function: Function::Returns,
            qualifier: None,
            invocation_type: "Event",
            status: 202,
            failed: false,
        },
        Row {
            case: "a dry run, which runs no handler at all",
            function: Function::Returns,
            qualifier: None,
            invocation_type: "DryRun",
            status: 204,
            failed: false,
        },
    ];

    let mut named: Vec<String> = Vec::new();
    let mut read = 0_usize;
    for row in rows {
        let (name, arn) = match row.function {
            Function::Returns => (returning, &returning_arn),
            Function::Raises => (raising, &raising_arn),
        };
        let reference = match row.qualifier {
            Some(qualifier) => format!("{name}:{qualifier}"),
            None => name.to_string(),
        };
        let mut answered = invoke(&router, &reference, row.invocation_type).await;
        for _ in 1..HOST_STOP_ATTEMPTS {
            if !is_host_bound(answered.status, &answered.text()) {
                break;
            }
            tokio::time::sleep(SLACK).await;
            answered = invoke(&router, &reference, row.invocation_type).await;
        }
        if is_host_bound(answered.status, &answered.text()) {
            // This process's own run slots or memory bound, held by another suite of this
            // target: no answer to what this row asked, so the row is not read.
            continue;
        }
        read += 1;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            row.case,
            row.status,
            answered.status,
            answered.text()
        );
        let failure = answered.header("x-amz-function-error");
        assert_eq!(
            failure.is_some(),
            row.failed,
            "case {:?}: expected the answer to {} report a failed run, got {failure:?} with {}",
            row.case,
            if row.failed { "" } else { "not" },
            answered.text()
        );
        let stated = answered.request_id();
        assert!(
            is_request_id(&stated),
            "case {:?}: expected the answer to name its request in {REQUEST_ID_HEADER}, got \
             {stated:?}",
            row.case
        );
        assert!(
            !named.contains(&stated),
            "case {:?}: every call is answered under an id of its own, got {stated:?} twice",
            row.case
        );
        // A run that reported its own invocation reported the id the answer names: the answer
        // of an invocation names the invocation it ran, not a second id minted for the reply.
        if row.function == Function::Returns && row.invocation_type == "RequestResponse" {
            let reported = answered.json()["request"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reported,
                stated,
                "case {:?}: the run reports its invocation as {reported:?}, so the answer must \
                 name that invocation, got {stated:?}",
                row.case
            );
        }

        let record = record_naming(&router, row.case, &stated).await;
        assert_eq!(
            (
                text(&record, "eventName").as_str(),
                text(&record, "eventCategory").as_str(),
                text(&record, "eventSource").as_str(),
                record.get("managementEvent"),
            ),
            (
                INVOKE_EVENT_NAME,
                DATA_CATEGORY,
                EVENT_SOURCE,
                Some(&json!(false))
            ),
            "case {:?}: expected the record of the invocation the caller was answered under, \
             got {record}",
            row.case
        );
        // The one resource the call acted on, named by its type and its ARN — the two fields a
        // trail's selector selects a data event by. Read as those two fields rather than as the
        // whole entry, because what else CloudTrail identifies a resource by (the account that
        // owns it) is not what this rule is about.
        let resources = record
            .get("resources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let named_resources: Vec<(String, String)> = resources
            .iter()
            .map(|resource| (text(resource, "type"), text(resource, "ARN")))
            .collect();
        assert_eq!(
            named_resources,
            vec![(FUNCTION_RESOURCE_TYPE.to_string(), arn.to_string())],
            "case {:?}: expected the record to name the function it ran, got {record}",
            row.case
        );
        let version = row.qualifier.unwrap_or(VERSION_LATEST);
        assert_eq!(
            record.pointer("/additionalEventData/functionVersion"),
            Some(&json!(format!("{arn}:{version}"))),
            "case {:?}: expected the record to report the version the call addressed, got \
             {record}",
            row.case
        );
        named.push(stated);
    }
    assert!(
        read > 0,
        "no row of this table was served at all: every one was ended by this process's own \
         host bounds, so the rule was never observed"
    );
}

/// `Invoke` the function `reference` names, with the invocation type `invocation_type`.
async fn invoke(router: &MultiServiceRouter, reference: &str, invocation_type: &str) -> Answer {
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations"),
        br#"{"probe":true}"#.to_vec(),
        &[("x-amz-invocation-type", invocation_type)],
    )
    .await
}
