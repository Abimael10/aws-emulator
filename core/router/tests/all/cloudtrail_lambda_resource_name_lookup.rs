// SPDX-License-Identifier: CC0-1.0

//! A Lambda control-plane call found through CloudTrail's lookup by RESOURCE, and what the
//! record a client then reads carries.
//!
//! RULE: a management record names the resources the call acted on, so
//! `LookupEvents` with a `ResourceName` attribute — "the name of the resource referenced by
//! the event", a user-created name — answers every management call made on that resource,
//! whichever of the documented reference forms each call addressed it in, and reports the
//! pair `(ResourceType, ResourceName)` on each event it serves. The record itself reports
//! the call: the `eventName` CloudTrail publishes the operation under, the `Management`
//! category, the Region and the account it was served for, `readOnly` from the operation's
//! own semantics, the members the request carried as `requestParameters` and — for a
//! mutating call — what it answered as `responseElements`, with the values the serving API
//! withholds (a deployment package, an environment variable) in neither. A call the service
//! refused carries the fault's code and the caller's own message, no `responseElements`, and
//! names no resource, because a call refused for want of the resource acted on none.
//!
//! Only a cross-service suite can pin this: the calls are served by AWS Lambda
//! (`restJson1`), the records are read back through `cloudtrail:LookupEvents`
//! (`awsJson1_1`), and the two meet in the one shared management-event log of the engine
//! `core/router/src/lib.rs` mounts them both on. The Lambda-side half — which resource each
//! operation's record names — is
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/management_event_resource_names.rs`.
//!
//! The rows span what the rule generalises over rather than the create that motivated it: a
//! write that makes the resource, a grant on it, a read addressing it by its full ARN rather
//! than by name, and a tagging write addressing it through a differently-named member — with
//! a read of a function the account does not hold as the refusal, and a listing that names no
//! resource as the control. Every page of every lookup is read, paging `NextToken` to
//! exhaustion, because a record a client has to find is one it finds however the service
//! pages.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, which is the Region the records are filed
/// in and the one the lookups are made in.
const REGION: &str = "us-east-1";

/// The account every emulated resource and every record belongs to.
const ACCOUNT: &str = "000000000000";

/// The event source of every record a Lambda call makes: the model's own
/// `awsService.cloudTrailEventSource`.
const LAMBDA_EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The `eventCategory` a lookup serves.
const MANAGEMENT_CATEGORY: &str = "Management";

/// The type CloudTrail names a Lambda function by, in a record's resource list and in a
/// trail's event selectors alike.
const FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";

/// The function every call below is made on, and the `ResourceName` its records are looked
/// up by.
const FUNCTION: &str = "ct-resource-subject";

/// A function name the account holds nothing under, which is what makes the read of it a
/// refusal.
const ABSENT_FUNCTION: &str = "ct-resource-never-made";

/// An execution role ARN satisfying the control plane's modelled `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime and handler the function is created with.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The environment variable the create declares, whose key and value a record must carry
/// nowhere — even though the create's own answer echoes both back to the caller.
const ENVIRONMENT_KEY: &str = "CT_REDACTION_PROBE";
const ENVIRONMENT_VALUE: &str = "ct-redaction-secret-value";

/// The `Code.ZipFile` the create uploads: the 22-byte empty archive, base64-encoded as
/// `restJson1` carries a blob. A record must carry neither the member nor this text, which
/// is the package's base64 form.
const PACKAGE_BASE64: &str = "UEsFBgAAAAAAAAAAAAAAAAAAAAAAAA==";

/// How often, and for how long at most, a poll looks for a condition the emulator's own
/// clock owes (a created function reaching `Active`). The deadline is far above the span the
/// service takes, so a loaded host waits longer rather than failing.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 400;

/// How many pages of one lookup are read at most: a bound on this test, not on the API —
/// the log holds a handful of records here, and a service that paged without end would fail
/// here rather than loop.
const MAX_PAGES: usize = 200;

/// One answer as its caller reads it.
struct Answered {
    status: u16,
    text: String,
    body: Value,
}

/// AWS Lambda and CloudTrail over one shared engine, exactly as `core/router/src/lib.rs`
/// mounts them. No trail is created: event history holds an account's management calls
/// "whether or not a trail exists".
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
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the request body serializes")
    };
    dispatch(
        router,
        method,
        "lambda",
        uri,
        "application/json",
        payload,
        &[],
    )
    .await
}

/// One CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must be
/// served.
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
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

/// Create the function and wait until Lambda reports it `Active` — the state the grant below
/// is made against. Waits for the CONDITION, never for a span of time.
async fn active_function(router: &MultiServiceRouter) -> Answered {
    let created = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Environment": { "Variables": { ENVIRONMENT_KEY: ENVIRONMENT_VALUE } },
            "Code": { "ZipFile": PACKAGE_BASE64 },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction({FUNCTION:?}) must succeed, got {} with {}",
        created.status, created.text
    );
    // The positive control for the redaction legs: the API itself echoes the environment
    // back, so what a record withholds is a property of the RECORD and not of the service
    // having dropped the value.
    assert_eq!(
        created.body.pointer(&format!("/Environment/Variables/{ENVIRONMENT_KEY}")),
        Some(&json!(ENVIRONMENT_VALUE)),
        "setup: the create's own answer echoes the environment variable, got {}",
        created.text
    );
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let read = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &Value::Null,
        )
        .await;
        if read.body["State"].as_str() == Some("Active") {
            return created;
        }
        last = read.body;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// One looked-up event: the summary members a lookup reports beside the record, and the
/// record itself.
struct Found {
    event_name: String,
    event_source: String,
    resources: Vec<(String, String)>,
    document: Value,
}

/// Every event of `attributes`, paging `NextToken` to exhaustion.
async fn looked_up(router: &MultiServiceRouter, case: &str, attributes: &Value) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut request = json!({ "LookupAttributes": [attributes], "MaxResults": 1 });
        if let Some(token) = &next {
            request["NextToken"] = json!(token);
        }
        let answered = cloudtrail(router, case, "LookupEvents", &request).await;
        for event in answered["Events"].as_array().unwrap_or(&Vec::new()) {
            let text = event["CloudTrailEvent"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: every event carries its CloudTrailEvent, got {event}")
            });
            let document: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: a CloudTrailEvent is JSON ({error}): {text}")
            });
            found.push(Found {
                event_name: event["EventName"].as_str().unwrap_or_default().to_string(),
                event_source: event["EventSource"].as_str().unwrap_or_default().to_string(),
                resources: event["Resources"]
                    .as_array()
                    .unwrap_or(&Vec::new())
                    .iter()
                    .map(|resource| {
                        (
                            resource["ResourceType"].as_str().unwrap_or_default().to_string(),
                            resource["ResourceName"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect(),
                document,
            });
        }
        next = answered["NextToken"].as_str().map(ToString::to_string);
        if next.is_none() {
            return found;
        }
    }
    panic!(
        "case {case:?}: the lookup must page to exhaustion, got {} events",
        found.len()
    );
}

/// The one event of `events` published under `event_name`.
fn one<'a>(case: &str, events: &'a [Found], event_name: &str) -> &'a Found {
    let matching: Vec<&Found> = events
        .iter()
        .filter(|found| found.event_name == event_name)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one {event_name} event, got {:?}",
        events
            .iter()
            .map(|found| found.event_name.as_str())
            .collect::<Vec<&str>>()
    );
    matching[0]
}

/// One call this suite makes on the function, and the `eventName` a lookup by the
/// function's name must then answer it under.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
    status: u16,
    event_name: &'static str,
    /// The operation's own semantics, as the record's `readOnly`.
    read_only: bool,
}

#[tokio::test]
async fn a_lambda_management_call_is_found_by_the_resource_it_acted_on() {
    let router = router();
    let created = active_function(&router).await;
    let function_arn = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{FUNCTION}");

    let rows = vec![
        Row {
            case: "a grant on the function's policy, addressing it by name",
            method: "POST",
            uri: format!("/2015-03-31/functions/{FUNCTION}/policy"),
            body: json!({
                "StatementId": "ct-resource-statement",
                "Action": "lambda:InvokeFunction",
                "Principal": "s3.amazonaws.com",
                "SourceAccount": ACCOUNT,
            }),
            status: 201,
            event_name: "AddPermission20150331v2",
            read_only: false,
        },
        Row {
            case: "a read of that policy addressing the function by its full ARN",
            method: "GET",
            uri: format!("/2015-03-31/functions/{function_arn}/policy"),
            body: Value::Null,
            status: 200,
            event_name: "GetPolicy20150331v2",
            read_only: true,
        },
        Row {
            case: "a tagging write, which addresses it through another member",
            method: "POST",
            uri: format!("/2017-03-31/tags/{function_arn}"),
            body: json!({ "Tags": { "owner": "audit" } }),
            status: 204,
            event_name: "TagResource20170331",
            read_only: false,
        },
    ];
    for row in &rows {
        let answered = lambda(&router, row.method, &row.uri, &row.body).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            row.case, row.status, answered.status, answered.text
        );
    }

    // A listing, which acts on no one function: the control for the lookup below.
    let listed = lambda(&router, "GET", "/2015-03-31/functions", &Value::Null).await;
    assert_eq!(
        listed.status, 200,
        "setup: ListFunctions must succeed, got {} with {}",
        listed.status, listed.text
    );

    // The refusal: a read of a function the account does not hold.
    let refused = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/functions/{ABSENT_FUNCTION}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        refused.status, 404,
        "setup: GetFunction({ABSENT_FUNCTION:?}) must be refused 404, got {} with {}",
        refused.status, refused.text
    );
    let refusal_message = refused.body["Message"]
        .as_str()
        .or_else(|| refused.body["message"].as_str())
        .unwrap_or_else(|| panic!("setup: the refusal states a message, got {}", refused.text))
        .to_string();

    // THE LOOKUP an audit client makes: by the name of the resource it is auditing.
    let by_resource = looked_up(
        &router,
        "a lookup by resource name",
        &json!({ "AttributeKey": "ResourceName", "AttributeValue": FUNCTION }),
    )
    .await;
    let names: Vec<&str> = by_resource
        .iter()
        .map(|found| found.event_name.as_str())
        .collect();

    // The create is one of the calls the lookup must answer, so it is a row here as well.
    let expected: Vec<(&str, bool)> = std::iter::once(("CreateFunction20150331", false))
        .chain(rows.iter().map(|row| (row.event_name, row.read_only)))
        .collect();
    for (event_name, read_only) in &expected {
        let found = one("a lookup by resource name", &by_resource, event_name);
        assert_eq!(
            found.event_source, LAMBDA_EVENT_SOURCE,
            "case {event_name:?}: the event names this service as its source, got {:?}",
            found.event_source
        );
        assert_eq!(
            found.resources,
            vec![(
                FUNCTION_RESOURCE_TYPE.to_string(),
                FUNCTION.to_string()
            )],
            "case {event_name:?}: the event reports the function it acted on, got {:?}",
            found.resources
        );
        assert_eq!(
            found.document.get("eventCategory"),
            Some(&json!(MANAGEMENT_CATEGORY)),
            "case {event_name:?}: a lookup serves management events, got {}",
            found.document
        );
        assert_eq!(
            found.document.get("eventSource"),
            Some(&json!(LAMBDA_EVENT_SOURCE)),
            "case {event_name:?}: the record names the event source, got {}",
            found.document
        );
        assert_eq!(
            found.document.get("awsRegion"),
            Some(&json!(REGION)),
            "case {event_name:?}: the record is filed in the Region the call was made in, \
             got {}",
            found.document
        );
        assert_eq!(
            found.document.get("recipientAccountId"),
            Some(&json!(ACCOUNT)),
            "case {event_name:?}: the record names the caller's account, got {}",
            found.document
        );
        assert_eq!(
            found.document.get("readOnly"),
            Some(&json!(read_only)),
            "case {event_name:?}: expected readOnly {read_only}, got {}",
            found.document
        );
        assert_eq!(
            found.document.get("errorCode"),
            None,
            "case {event_name:?}: a served call carries no errorCode, got {}",
            found.document
        );
    }

    // The listing acted on no function, so the lookup by that function does not answer it.
    assert!(
        !names.contains(&"ListFunctions20150331"),
        "case the control: a call that acted on no one function is not answered by a lookup \
         on that function, got {names:?}"
    );
    // Neither is the refused read: it named a function the account does not hold.
    assert!(
        !names.contains(&"GetFunction20150331v2"),
        "case the refusal: a call refused for want of the resource names none, got {names:?}"
    );
    // Every record is its own.
    let ids: BTreeSet<&str> = by_resource
        .iter()
        .filter_map(|found| found.document.get("eventID").and_then(Value::as_str))
        .collect();
    assert_eq!(
        ids.len(),
        by_resource.len(),
        "case every record: expected one eventID per record, got {ids:?} over {} records",
        by_resource.len()
    );

    // WHAT the create's record carries: what the call answered, and none of what the
    // serving API withholds.
    let create = one("a lookup by resource name", &by_resource, "CreateFunction20150331");
    assert_eq!(
        create.document.pointer("/requestParameters/code"),
        Some(&json!({})),
        "case the create: the deployment package is stripped from the parameters, got {}",
        create.document
    );
    assert_eq!(
        create.document.pointer("/responseElements/functionArn"),
        created.body.get("FunctionArn"),
        "case the create: the record reports the ARN the call answered, got {}",
        create.document
    );
    assert_eq!(
        create.document.pointer("/responseElements/codeSha256"),
        created.body.get("CodeSha256"),
        "case the create: the record reports the digest the call answered, got {}",
        create.document
    );
    let whole = create.document.to_string();
    for withheld in [
        ENVIRONMENT_KEY,
        ENVIRONMENT_VALUE,
        "zipFile",
        PACKAGE_BASE64.trim_end_matches('='),
    ] {
        assert!(
            !whole.contains(withheld),
            "case the create: expected the record to carry no {withheld:?}, got {whole}"
        );
    }

    // The grant's record reports the parameters the request carried.
    let grant = one("a lookup by resource name", &by_resource, "AddPermission20150331v2");
    for (member, expected) in [
        ("statementId", "ct-resource-statement"),
        ("principal", "s3.amazonaws.com"),
    ] {
        assert_eq!(
            grant.document.pointer(&format!("/requestParameters/{member}")),
            Some(&json!(expected)),
            "case the grant: expected requestParameters.{member} = {expected:?}, got {}",
            grant.document
        );
    }

    // THE OTHER LOOKUP: over the whole event source, which is what finds a call that named
    // no resource — the refused read among them.
    let by_source = looked_up(
        &router,
        "a lookup by event source",
        &json!({ "AttributeKey": "EventSource", "AttributeValue": LAMBDA_EVENT_SOURCE }),
    )
    .await;
    let refusal = one("a lookup by event source", &by_source, "GetFunction20150331v2");
    assert_eq!(
        refusal.document.get("readOnly"),
        Some(&json!(true)),
        "case the refusal: a read reports readOnly true however it ended, got {}",
        refusal.document
    );
    assert_eq!(
        refusal.document.get("errorCode"),
        Some(&json!("ResourceNotFoundException")),
        "case the refusal: the record names the fault the caller was answered with, got {}",
        refusal.document
    );
    assert_eq!(
        refusal.document.get("errorMessage"),
        Some(&json!(refusal_message)),
        "case the refusal: the record carries the message the caller received, got {}",
        refusal.document
    );
    assert_eq!(
        refusal.document.get("responseElements"),
        Some(&Value::Null),
        "case the refusal: a refused call answered nothing, got {}",
        refusal.document
    );
    assert!(
        refusal.resources.is_empty(),
        "case the refusal: a call refused for want of the resource names none, got {:?}",
        refusal.resources
    );
    // And the listing, which acted on no resource, is answered here and nowhere else.
    let _ = one("a lookup by event source", &by_source, "ListFunctions20150331");
}
