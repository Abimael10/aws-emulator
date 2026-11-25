// SPDX-License-Identifier: CC0-1.0

//! How a delivered invocation record NAMES the call it is the audit record of: the function it
//! ran, the version that run addressed, and the account and Region the call belongs to.
//!
//! Three services are mounted on ONE engine, because the behaviour spans them: AWS Lambda
//! serves the invocation, CloudTrail holds the trail whose advanced event selector admits it,
//! and Amazon S3 is where the gzipped log file lands. Every call goes over the wire the way a
//! client makes it, and the log bucket is created, policied, listed and READ through Amazon
//! S3's own API, so a record counts only if the object store itself answers with the file
//! holding it.
//!
//! RULE: the record of a call on a resource's contents names that ONE resource by the type and
//! the ARN a trail's selector selects it by, reports the call's own parameters with the
//! addressed resource resolved to that same ARN — unqualified, with the version the call
//! addressed reported beside the parameters — and attributes the call to the account that owns
//! the resource and to the Region the request was made in. A client may address a Lambda
//! function in any of the documented `FunctionName` forms
//! (`smithy_query(query="operation",operation="Invoke")` ->
//! `InvocationRequest$FunctionName`: "Function name", "Function ARN", "Partial ARN", each
//! optionally carrying a version or alias, with `InvocationRequest$Qualifier` as the other
//! place a version may be named), and they all name one resource — so the delivered record
//! reads the same whichever form the request happened to use.
//!
//! The rows therefore span what the rule generalises over: WHICH documented form the request
//! addressed the function in, and WHERE the version it addressed was named — in the reference
//! itself or in the `Qualifier` parameter, or nowhere at all. What a trail delivers for an
//! invocation and to which trail is the subject of
//! `core/router/tests/all/cloudtrail_lambda_invoke_data_events.rs`, and the join between a
//! response's own request id and the record's `requestID` is the subject of
//! `invoke_record_names_its_caller`; neither is restated here beyond the id this suite finds
//! each row's record by.
//!
//! The second test is the SCOPING of such a selector from both sides at once: with a trail
//! whose `resources.ARN` names one function, the invocation of that function is delivered and
//! the invocation of a sibling in the same window is withheld — so an empty answer is read as
//! withholding only where the same trail demonstrably delivered something.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the trail is homed in and
/// the Region a delivered record attributes its call to.
const REGION: &str = "us-east-1";

/// The account every emulated resource and every delivered record belongs to.
const ACCOUNT: &str = "000000000000";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function here is created with, and the handler its configuration names.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// A `Code.ZipFile` payload: `restJson1` carries a blob base64-encoded in the JSON body. This
/// is the 22-byte empty archive — a readable `.zip` holding no entry, which is enough for the
/// invocations below, whose answer is the API's and not a handler's.
const ZIP_FILE: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// The bucket the trail delivers into and the key prefix its files land under.
const LOG_BUCKET: &str = "lambda-invoke-record-attribution-logs";
const KEY_PREFIX: &str = "invoke-attribution";

/// The header a response names the request it answered in, lowercased as HTTP compares header
/// names, and spelled out here rather than read from the crate under test.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The member a delivered log file holds its records in.
const RECORDS_MEMBER: &str = "Records";

/// The first segments of a log file's key, under the trail's own key prefix.
const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";
const LOG_FILE_SERVICE_SEGMENT: &str = "CloudTrail";

/// The `eventCategory` of a data event, and the `eventSource` CloudTrail stamps on a record of
/// this service (`smithy_query(query="service_info")` on the Lambda model -> `endpointPrefix:
/// lambda`).
const DATA_CATEGORY: &str = "Data";
const EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The operation an invocation is recorded as, and the type a function is named by in a
/// record's `resources` and in a selector's `resources.type` field.
const INVOKE_EVENT_NAME: &str = "Invoke";
const FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";

/// The members a record carries the function and the version beside it in.
const FUNCTION_NAME_MEMBER: &str = "functionName";
const FUNCTION_VERSION_MEMBER: &str = "functionVersion";

/// The version an invocation that named no published version ran: "the unpublished version of
/// the function" (`Qualifier`), which is what the record reports the call addressed.
const VERSION_LATEST: &str = "$LATEST";

/// The invocation type every row asks for: it runs no handler at all
/// (`InvocationRequest$InvocationType`, `DryRun`: "validate parameter values and verify that
/// the user or role has permission to invoke the function"), which is all these rows need —
/// the record is of the CALL, not of the run.
const DRY_RUN: &str = "DryRun";

/// How often, and for how long at most, a poll looks for a condition the emulator owes (a
/// created function reporting itself `Active`, a log file reaching the bucket). The deadline is
/// far above the span either takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

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

    /// The id this answer names its request by, which is the `requestID` of the record of that
    /// same call.
    fn request_id(&self) -> String {
        self.headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
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

/// Dispatch one request and read back what it answered.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    signing_name: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> Answer {
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
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answer {
        status,
        headers,
        body: response.into_body(),
    }
}

/// One Lambda call (`restJson1`), answered as it stands.
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

/// One Amazon S3 call, answered as it stands.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: Vec<u8>) -> Answer {
    dispatch(router, method, "s3", uri, "application/xml", body, &[]).await
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
        "setup: CloudTrail {operation} must be served, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.json()
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
        policied.text()
    );
}

/// Create one function and wait until Lambda reports it `Active`, which is the state an
/// invocation needs, and answer its ARN.
async fn function(router: &MultiServiceRouter, name: &str) -> String {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
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
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            Vec::new(),
            &[],
        )
        .await;
        let configuration = read.json();
        if configuration["State"].as_str() == Some("Active") {
            return arn;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Point a trail at the log bucket under this suite's key prefix, give it `selectors` and start
/// it logging: the calls a client makes, each of which must be served.
async fn logging_trail(router: &MultiServiceRouter, trail: &str, selectors: &Value) {
    let created = cloudtrail(
        router,
        "CreateTrail",
        &json!({
            "Name": trail,
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
    let echoed = cloudtrail(
        router,
        "PutEventSelectors",
        &json!({
            "TrailName": trail_arn,
            "AdvancedEventSelectors": selectors,
        }),
    )
    .await;
    assert_eq!(
        echoed.get("AdvancedEventSelectors"),
        Some(selectors),
        "setup: the trail must store the selectors it was given, got {echoed}"
    );
    cloudtrail(router, "StartLogging", &json!({ "Name": trail_arn })).await;
    let status = cloudtrail(router, "GetTrailStatus", &json!({ "Name": trail_arn })).await;
    assert_eq!(
        status.get("IsLogging"),
        Some(&json!(true)),
        "setup: the trail must report itself logging, got {status}"
    );
}

/// The whole delivery area of this suite's trail, which is where a consumer of it looks for its
/// files.
fn delivery_prefix() -> String {
    format!("{KEY_PREFIX}/{LOG_FILE_ROOT_SEGMENT}/{ACCOUNT}/{LOG_FILE_SERVICE_SEGMENT}/")
}

/// Every object key the log bucket holds under the trail's delivery area, as Amazon S3's own
/// `ListObjectsV2` reports them.
async fn delivered_keys(router: &MultiServiceRouter) -> Vec<String> {
    let prefix = delivery_prefix();
    let listed = s3(
        router,
        "GET",
        &format!("/{LOG_BUCKET}?list-type=2&prefix={prefix}"),
        Vec::new(),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "listing {LOG_BUCKET}/{prefix} must succeed, got {} with {}",
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

/// One advanced event selector, as the field selectors it states.
fn advanced(name: &str, fields: Vec<Value>) -> Value {
    json!([{ "Name": name, "FieldSelectors": fields }])
}

/// One field selector stating `Equals` over `values`, which is the only operator the category,
/// the resource type and the read/write field take.
fn equals(field: &str, values: &[&str]) -> Value {
    json!({ "Field": field, "Equals": values })
}

/// The selector AWS publishes for the invocations of ONE function: the `Data` category, the
/// function resource type and that function's ARN.
fn one_functions_selector(name: &str, arn: &str) -> Value {
    advanced(
        name,
        vec![
            equals("eventCategory", &[DATA_CATEGORY]),
            equals("resources.type", &[FUNCTION_RESOURCE_TYPE]),
            equals("resources.ARN", &[arn]),
        ],
    )
}

/// How a row addresses the function — the dimension this table spans. Each is a documented
/// `FunctionName` form, and the last two are the other places the version a call addresses may
/// be named.
#[derive(Clone, Copy)]
enum Address {
    /// The bare function name, naming no version.
    Name,
    /// The name with the unpublished version as its qualifier.
    NameWithVersion,
    /// The bare name, with the version named in the `Qualifier` parameter instead.
    NameAndQualifierParameter,
    /// The partial ARN: the account and the function, without the `arn:` prefix.
    PartialArn,
    /// The function's own ARN.
    FullArn,
    /// The function's ARN with the unpublished version as its qualifier.
    FullArnWithVersion,
}

impl Address {
    /// The path segment the request addresses the function in, and the query it carries.
    fn addressed(self, name: &str, arn: &str) -> (String, String) {
        match self {
            Self::Name => (name.to_string(), String::new()),
            Self::NameWithVersion => (format!("{name}:{VERSION_LATEST}"), String::new()),
            Self::NameAndQualifierParameter => {
                (name.to_string(), format!("?Qualifier={VERSION_LATEST}"))
            }
            Self::PartialArn => (format!("{ACCOUNT}:function:{name}"), String::new()),
            Self::FullArn => (arn.to_string(), String::new()),
            Self::FullArnWithVersion => (format!("{arn}:{VERSION_LATEST}"), String::new()),
        }
    }
}

/// `Invoke` the function the row's `address` names, running no handler.
async fn invoke(router: &MultiServiceRouter, address: Address, name: &str, arn: &str) -> Answer {
    let (reference, query) = address.addressed(name, arn);
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{reference}/invocations{query}"),
        br#"{"probe":true}"#.to_vec(),
        &[("x-amz-invocation-type", DRY_RUN)],
    )
    .await
}

/// The members of `record` the rule is about, read as one document so a failure reports every
/// one of them at once.
fn attribution(record: &Value, expected: &Value) -> Value {
    expected
        .as_object()
        .expect("the expectation is a document")
        .keys()
        .map(|member| {
            (
                member.clone(),
                record.get(member).cloned().unwrap_or(Value::Null),
            )
        })
        .collect::<serde_json::Map<String, Value>>()
        .into()
}

/// RULE: a delivered invocation record names the function the call ran by the resource type and
/// the unqualified ARN a trail's selector selects it by, reports that same ARN as the
/// invocation's own `functionName` parameter with the version the call addressed beside it, and
/// attributes the call to the owning account and the request's Region — whichever documented
/// `FunctionName` form the request used and wherever it named the version.
#[tokio::test]
async fn an_invocation_record_names_the_function_and_the_account_it_belongs_to() {
    let router = router();
    log_bucket(&router).await;
    let name = "invoke-attribution-subject";
    let arn = function(&router, name).await;
    logging_trail(
        &router,
        "invoke-attribution-trail",
        &one_functions_selector("the subject function's invocations", &arn),
    )
    .await;

    let rows: &[(&str, Address)] = &[
        ("the bare function name", Address::Name),
        (
            "the name with the unpublished version as its qualifier",
            Address::NameWithVersion,
        ),
        (
            "the bare name, with the version in the Qualifier parameter",
            Address::NameAndQualifierParameter,
        ),
        ("the partial ARN", Address::PartialArn),
        ("the function's own ARN", Address::FullArn),
        (
            "the function's ARN with the unpublished version as its qualifier",
            Address::FullArnWithVersion,
        ),
    ];

    let mut named: Vec<String> = Vec::new();
    for (case, address) in rows {
        let answered = invoke(&router, *address, name, &arn).await;
        assert!(
            (200..300).contains(&answered.status),
            "case {case:?}: the invocation must be served, got {} with {}",
            answered.status,
            answered.text()
        );
        let stated = answered.request_id();
        assert!(
            !stated.is_empty(),
            "case {case:?}: the answer must name its request in {REQUEST_ID_HEADER}"
        );
        assert!(
            !named.contains(&stated),
            "case {case:?}: every call is answered under an id of its own, got {stated:?} twice"
        );
        let record = record_naming(&router, case, &stated).await;
        let expected = json!({
            "eventSource": EVENT_SOURCE,
            "eventName": INVOKE_EVENT_NAME,
            "eventCategory": DATA_CATEGORY,
            "managementEvent": false,
            "readOnly": false,
            "awsRegion": REGION,
            "recipientAccountId": ACCOUNT,
            "requestParameters": { FUNCTION_NAME_MEMBER: arn },
            "additionalEventData": {
                FUNCTION_VERSION_MEMBER: format!("{arn}:{VERSION_LATEST}"),
            },
            "resources": [{
                "accountId": ACCOUNT,
                "type": FUNCTION_RESOURCE_TYPE,
                "ARN": arn,
            }],
        });
        let actual = attribution(&record, &expected);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the record to name the function and the account it \
             belongs to as {expected}, got {actual} in {record}"
        );
        named.push(stated);
    }
}

/// RULE: a selector that pins `resources.ARN` admits the data events of THAT resource and of no
/// other, so the invocations of one function reach the trail's bucket while a sibling's
/// invocations in the same window reach nothing.
///
/// Both sides are read off the SAME trail and the same delivery area: the subject's record
/// being there is what makes the sibling's absence a withholding rather than a delivery that
/// never happened.
#[tokio::test]
async fn a_selector_scoped_to_one_function_withholds_a_siblings_invocations() {
    let router = router();
    log_bucket(&router).await;
    let subject = "invoke-attribution-selected";
    let sibling = "invoke-attribution-withheld";
    let subject_arn = function(&router, subject).await;
    let sibling_arn = function(&router, sibling).await;
    logging_trail(
        &router,
        "invoke-attribution-scoped-trail",
        &one_functions_selector("only the selected function", &subject_arn),
    )
    .await;

    // Each function is invoked in every documented form, so the scoping is read over the whole
    // class of references rather than over the one form a client happened to write.
    let forms: &[(&str, Address)] = &[
        ("the bare function name", Address::Name),
        ("the partial ARN", Address::PartialArn),
        ("the function's own ARN", Address::FullArn),
    ];
    let mut selected: Vec<String> = Vec::new();
    for (form, address) in forms {
        for (case, name, arn, admitted) in [
            (
                format!("the selected function addressed by {form}"),
                subject,
                &subject_arn,
                true,
            ),
            (
                format!("the sibling function addressed by {form}"),
                sibling,
                &sibling_arn,
                false,
            ),
        ] {
            let answered = invoke(&router, *address, name, arn).await;
            assert!(
                (200..300).contains(&answered.status),
                "case {case:?}: the invocation must be served, got {} with {}",
                answered.status,
                answered.text()
            );
            if admitted {
                selected.push(answered.request_id());
            }
        }
    }

    // The selected function's records first, which is what proves this trail delivers at all.
    for request_id in &selected {
        let record = record_naming(&router, "the selected function", request_id).await;
        assert_eq!(
            record.get("requestParameters"),
            Some(&json!({ FUNCTION_NAME_MEMBER: subject_arn })),
            "case \"the selected function\": expected the record of an invocation of \
             {subject_arn:?}, got {record}"
        );
    }
    let withheld: Vec<&Value> = Vec::new();
    let delivered = delivered_records(&router).await;
    let naming_the_sibling: Vec<&Value> = delivered
        .iter()
        .filter(|record| {
            serde_json::to_string(record)
                .unwrap_or_default()
                .contains(&sibling_arn)
        })
        .collect();
    assert_eq!(
        naming_the_sibling, withheld,
        "case \"the sibling function\": a selector pinned to {subject_arn:?} admits no record \
         naming {sibling_arn:?}, got {naming_the_sibling:?} among {} delivered record(s)",
        delivered.len()
    );
}
