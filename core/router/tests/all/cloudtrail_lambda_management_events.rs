// SPDX-License-Identifier: CC0-1.0

//! A Lambda control-plane call read back out of CloudTrail event history, through the
//! operation a client reads it with.
//!
//! RULE: CloudTrail's `LookupEvents` "looks up management events … captured by CloudTrail"
//! for the Region the request is made in, and every management call on a service is
//! captured in event history whether or not a trail exists. So after a successful
//! control-plane call of AWS Lambda — whose model declares
//! `awsService.cloudTrailEventSource: lambda.amazonaws.com` — a lookup filtered on that
//! event source answers an event whose `CloudTrailEvent` carries `eventCategory`
//! `Management`, that event source, the `eventName` CloudTrail publishes for the operation
//! (its own name followed by the API date of its REST binding, with the `v2` marker on the
//! operations AWS revised in place), `readOnly` from the operation's own semantics, no
//! `errorCode`, the Region the request was made in, the caller's account as
//! `recipientAccountId`, an `eventID` of its own, and the members the request carried as
//! `requestParameters`. The other half of the same contract: a lookup serves MANAGEMENT
//! events only, so the `Invoke` calls of the same window — data events, delivered to a
//! trail's bucket and nowhere else — are in none of its pages.
//!
//! Only a cross-service suite can pin this: the calls are served by AWS Lambda
//! (`restJson1`), the records are read back through `cloudtrail:LookupEvents`
//! (`awsJson1_1`), and the two meet in the one shared management-event log of the engine
//! `core/router/src/lib.rs` mounts them both on. The Lambda-side half of the rule — which
//! operations are recorded, with which class, and what a record withholds — is
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/management_event_recording.rs`.
//!
//! The rows span what the rule generalises over rather than the one read that motivated it:
//! a revised read and an unrevised one, a read of one resource and a list over a
//! collection, a read of the account itself, and a write — over three of the API versions
//! Lambda binds — with the invocations as the control. Every page of the lookup is read,
//! paging `NextToken` to exhaustion, because a record a client has to find is a record it
//! finds however the service pages.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the records are filed
/// in and the one the lookup is made in.
const REGION: &str = "us-east-1";

/// The account every emulated resource and every record belongs to.
const ACCOUNT: &str = "000000000000";

/// The event source of every record a Lambda call makes: the model's own
/// `awsService.cloudTrailEventSource`.
const LAMBDA_EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The `eventCategory` a lookup serves.
const MANAGEMENT_CATEGORY: &str = "Management";

/// The function the calls below are made on.
const FUNCTION: &str = "ct-management-subject";

/// An execution role ARN satisfying the control plane's modelled `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime and handler the function is created with.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// A `Code.ZipFile` payload: the 22-byte empty archive, base64-encoded as `restJson1`
/// carries a blob. It is a readable `.zip` holding no entry, which is enough for the
/// invocations below — what a run answers is the handler's business, not this suite's.
const ZIP_FILE: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// How often, and for how long at most, a poll looks for a condition the emulator's own
/// clock owes (a created function reaching `Active`). The deadline is far above the span the
/// service takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

/// How many pages of one lookup are read at most: a bound on this test, not on the API —
/// the log holds a handful of records here, and a service that paged without end would fail
/// here rather than loop.
const MAX_PAGES: usize = 50;

/// One answer as its caller reads it.
struct Answered {
    status: u16,
    text: String,
    body: Value,
}

/// AWS Lambda and CloudTrail over one shared engine, exactly as `core/router/src/lib.rs`
/// mounts them. No trail is created: event history holds an account's management calls
/// "whether or not a trail exists", which is the fact this suite pins.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential
    // scope is still carried, because it is what names the Region and the caller.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request. The signature is never checked here.
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

/// One Lambda call (`restJson1`), answered as it stands.
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

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be
/// served.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
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
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {} with {}",
        answered.status, answered.text
    );
    answered.body
}

/// Create the function and wait until Lambda reports it `Active` — the state an invocation
/// needs. Waits for the CONDITION, never for a span of time.
async fn active_function(router: &MultiServiceRouter) {
    let created = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        serde_json::to_vec(&json!({
            "FunctionName": FUNCTION,
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
        "setup: CreateFunction({FUNCTION:?}) must succeed, got {} with {}",
        created.status, created.text
    );
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            Vec::new(),
            &[],
        )
        .await;
        if read.body["State"].as_str() == Some("Active") {
            return;
        }
        last = read.body;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// Every event of `attributes`, paging `NextToken` to exhaustion, as the parsed
/// `CloudTrailEvent` document of each one beside the `EventName` the event itself reports.
async fn looked_up(
    router: &MultiServiceRouter,
    case: &str,
    attributes: &Value,
) -> Vec<(String, Value)> {
    let mut found: Vec<(String, Value)> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = json!({ "LookupAttributes": [attributes], "MaxResults": 1 });
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let answered = cloudtrail(router, case, "LookupEvents", &request).await;
        for event in answered["Events"].as_array().unwrap_or(&Vec::new()) {
            let document = event["CloudTrailEvent"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every event carries its CloudTrailEvent, got {event}")
            });
            let parsed: Value = serde_json::from_str(document).unwrap_or_else(|error| {
                panic!("case {case:?}: a CloudTrailEvent is JSON ({error}): {document}")
            });
            found.push((
                event["EventName"].as_str().unwrap_or_default().to_string(),
                parsed,
            ));
        }
        next = answered["NextToken"].as_str().map(ToString::to_string);
        if next.is_none() {
            return found;
        }
    }
    panic!("case {case:?}: the lookup must page to exhaustion, got {} events", found.len())
}

/// One call this suite makes and reads back, and what the record of it must say.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
    status: u16,
    /// The `eventName` CloudTrail publishes the call under: the name a client filters a
    /// lookup by.
    event_name: &'static str,
    /// The operation's own semantics, as the record's `readOnly`.
    read_only: bool,
    /// A `(json pointer, value)` the record's `requestParameters` must carry.
    parameter: Option<(&'static str, Value)>,
}

#[tokio::test]
async fn a_lambda_management_call_is_served_back_by_lookup_events() {
    let router = router();
    active_function(&router).await;

    let rows = vec![
        Row {
            case: "the read of one function, a revised operation",
            method: "GET",
            uri: format!("/2015-03-31/functions/{FUNCTION}"),
            body: Value::Null,
            status: 200,
            event_name: "GetFunction20150331v2",
            read_only: true,
            parameter: Some(("/functionName", json!(FUNCTION))),
        },
        Row {
            case: "a list over the account's functions, an unrevised read",
            method: "GET",
            uri: "/2015-03-31/functions".to_string(),
            body: Value::Null,
            status: 200,
            event_name: "ListFunctions20150331",
            read_only: true,
            parameter: None,
        },
        Row {
            case: "a read of the account itself, under an API date of its own",
            method: "GET",
            uri: "/2016-08-19/account-settings".to_string(),
            body: Value::Null,
            status: 200,
            event_name: "GetAccountSettings20160819",
            read_only: true,
            parameter: None,
        },
        Row {
            case: "a tagging write, under the tagging API date",
            method: "POST",
            uri: format!("/2017-03-31/tags/arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}"),
            body: json!({ "Tags": { "owner": "audit" } }),
            status: 204,
            event_name: "TagResource20170331",
            read_only: false,
            parameter: Some((
                "/resource",
                json!(format!(
                    "arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}"
                )),
            )),
        },
    ];

    for row in &rows {
        let body = if row.body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&row.body).expect("the request body serializes")
        };
        let answered = lambda(&router, row.method, &row.uri, body, &[]).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            row.case, row.status, answered.status, answered.text
        );
    }

    // The invocations of the same window, which are DATA events: delivered to a trail's
    // bucket and never to event history. Made LAST, so a record of one that did reach the
    // log would already be there when the pages below are read.
    for invocation_type in ["RequestResponse", "Event"] {
        let answered = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
            br#"{"probe":true}"#.to_vec(),
            &[("x-amz-invocation-type", invocation_type)],
        )
        .await;
        assert!(
            (200..300).contains(&answered.status),
            "case {invocation_type:?}: the invocation must be served, got {} with {}",
            answered.status,
            answered.text
        );
    }

    // ONE lookup over the whole event source, as an audit client makes it.
    let by_source = looked_up(
        &router,
        "a lookup by event source",
        &json!({ "AttributeKey": "EventSource", "AttributeValue": LAMBDA_EVENT_SOURCE }),
    )
    .await;
    let names: Vec<&str> = by_source
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<&str>>();

    for row in &rows {
        let found: Vec<&Value> = by_source
            .iter()
            .filter(|(name, _)| name == row.event_name)
            .map(|(_, document)| document)
            .collect();
        assert!(
            !found.is_empty(),
            "case {:?}: expected a record of {} in event history, got {names:?}",
            row.case,
            row.event_name
        );
        let document = found[0];
        assert_eq!(
            document.get("eventSource"),
            Some(&json!(LAMBDA_EVENT_SOURCE)),
            "case {:?}: the record names the service's event source, got {document}",
            row.case
        );
        assert_eq!(
            document.get("eventCategory"),
            Some(&json!(MANAGEMENT_CATEGORY)),
            "case {:?}: a lookup serves management events, got {document}",
            row.case
        );
        assert_eq!(
            document.get("eventName"),
            Some(&json!(row.event_name)),
            "case {:?}: the document names the published operation, got {document}",
            row.case
        );
        assert_eq!(
            document.get("readOnly"),
            Some(&json!(row.read_only)),
            "case {:?}: expected readOnly {}, got {document}",
            row.case,
            row.read_only
        );
        assert_eq!(
            document.get("errorCode"),
            None,
            "case {:?}: a served call carries no errorCode, got {document}",
            row.case
        );
        assert_eq!(
            document.get("awsRegion"),
            Some(&json!(REGION)),
            "case {:?}: the record is filed in the Region the call was made in, got {document}",
            row.case
        );
        assert_eq!(
            document.get("recipientAccountId"),
            Some(&json!(ACCOUNT)),
            "case {:?}: the record names the caller's account, got {document}",
            row.case
        );
        assert!(
            document
                .get("eventID")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty()),
            "case {:?}: every record carries an eventID, got {document}",
            row.case
        );
        if let Some((pointer, expected)) = &row.parameter {
            assert_eq!(
                document.pointer(&format!("/requestParameters{pointer}")),
                Some(expected),
                "case {:?}: expected requestParameters{pointer} = {expected}, got {document}",
                row.case
            );
        }
    }

    // Every record is its own: no two of them share an `eventID`.
    let ids: BTreeSet<&str> = by_source
        .iter()
        .filter_map(|(_, document)| document.get("eventID").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids.len(),
        by_source.len(),
        "case every record: expected one eventID per record, got {ids:?} over {} records",
        by_source.len()
    );

    // The other half of the contract: no invocation of the window is in event history,
    // under the lookup over the whole source or under a lookup naming the invocation.
    assert!(
        !names.iter().any(|name| name.starts_with("Invoke")),
        "case the data events: a lookup serves management events only, got {names:?}"
    );
    let by_invoke = looked_up(
        &router,
        "a lookup naming the invocation",
        &json!({ "AttributeKey": "EventName", "AttributeValue": "Invoke" }),
    )
    .await;
    assert!(
        by_invoke.is_empty(),
        "case a lookup naming the invocation: expected no event, got {:?}",
        by_invoke
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<&str>>()
    );

    // And the lookup a client filters by the published NAME answers the same record, which
    // is how an audit workflow finds one call rather than reading the whole source.
    let by_name = looked_up(
        &router,
        "a lookup by event name",
        &json!({ "AttributeKey": "EventName", "AttributeValue": "GetFunction20150331v2" }),
    )
    .await;
    assert!(
        !by_name.is_empty(),
        "case a lookup by event name: expected the read's record, got none while the source \
         holds {names:?}"
    );
    for (name, document) in &by_name {
        assert_eq!(
            name, "GetFunction20150331v2",
            "case a lookup by event name: every event answers the name asked for, got \
             {document}"
        );
    }
}
