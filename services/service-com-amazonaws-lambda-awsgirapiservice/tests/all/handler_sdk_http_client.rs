// SPDX-License-Identifier: CC0-1.0

//! The requests a RUNNING handler makes with the HTTP CLIENT its runtime supplies beside the
//! AWS SDK built on it, end to end over the wire: Lambda and AWS Systems Manager on ONE
//! engine, a `python3.x` function whose code does `import urllib3` and sends a request with
//! it, and everything that request wrote read back where a client reads it — through that
//! service's own `GetParameter`.
//!
//! The rule pinned here: a managed `python3.x` runtime carries the HTTP client its bundled
//! SDK is built on — `urllib3` — on the handler's import path, so a handler that imports it
//! and sends a request through it reaches this emulator over the same seam the standard
//! library's own client rides, and what it reads back is the addressed service's own answer:
//! the status, the reason, the headers and the body that service sent. It is the client AWS's
//! own documented custom-resource response code answers with, so a provider written that way
//! runs here; an address this emulator serves nothing at is reported as a request that was
//! NOT made, in the library's own error.
//!
//! The rows differ along what the rule generalises over: HOW the request is made (the pool
//! manager a caller holds, the module-level request that needs none, a connection pool made
//! from the URL itself, and the lower-level `urlopen` under all three), what the answer is
//! (a served call carrying an output document, a call that service REFUSES, which is an
//! answer and not an exception), and an address no service of this emulator answers at, which
//! is raised rather than answered. Each served row asserts both halves — what the handler read
//! back, which is what the invocation answers, and what Systems Manager itself holds
//! afterwards.
//!
//! That the SDK's OWN modules are on the import path is pinned beside this file
//! (`tests/all/python_runtime_bundled_sdk.rs`); what is new here is the HTTP client that SDK
//! is built on, which the same runtime carries and a handler sends its own requests with.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The one .zip writer this crate's suites package a handler with, reached rather than copied.
use crate::node_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function this suite runs every row through.
const FUNCTION: &str = "http-client-caller";

/// The parameter each row writes, one per way of making the request, and a name no parameter
/// answers to — which is what the refused row addresses.
const PARAMETER_PREFIX: &str = "/emulator/http-client/";
const ABSENT_PARAMETER: &str = "/emulator/http-client/absent";

/// An address of AWS's own naming that names no service this emulator serves, and one that is
/// no AWS endpoint at all: a request to either is one the runtime does not make.
const UNSERVED_ADDRESS: &str = "https://example.com/health";

/// Lambda and AWS Systems Manager over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them: the handler's own request is dispatched back into this same router, so the parameter
/// it writes is the parameter this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Systems Manager call (`awsJson1_1`, `AmazonSSM.<Operation>`), as its status and parsed
/// body — the way a client makes one.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ssm.{REGION}.amazonaws.com"))
        .header("authorization", authorization("ssm"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonSSM.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The handler every row runs: it sends the ONE request the event names, the way the event
/// names, and reports what the client answered — so the invocation's payload IS what the
/// runtime's HTTP client gave the guest.
const HANDLER_SOURCE: &str = r#"
import json

import urllib3

POOL = urllib3.PoolManager()


def _answer(answered):
    document = None
    try:
        document = answered.json()
    except Exception:
        document = None
    return {
        "ok": True,
        "class": type(answered).__name__,
        "status": answered.status,
        "reason": answered.reason,
        "content_type": answered.headers.get("content-type"),
        "data_length": len(answered.data),
        "document": document,
        "redirect": answered.get_redirect_location(),
        "version": urllib3.__version__,
    }


def lambda_handler(event, context):
    how = event["how"]
    url = event["url"]
    headers = dict(event.get("headers") or {})
    body = None
    if event.get("document") is not None:
        body = json.dumps(event["document"]).encode("utf-8")
    method = event.get("method") or "POST"
    try:
        if how == "pool_manager_request":
            answered = POOL.request(method, url, body=body, headers=headers)
        elif how == "module_request":
            answered = urllib3.request(method, url, body=body, headers=headers)
        elif how == "connection_pool_request":
            pool = urllib3.connection_from_url(url)
            answered = pool.request(method, url, body=body, headers=headers)
        elif how == "pool_manager_urlopen":
            answered = POOL.urlopen(method, url, body=body, headers=headers)
        else:
            return {"ok": False, "unknown_how": how}
    except urllib3.exceptions.MaxRetryError as error:
        return {
            "ok": False,
            "raised": type(error).__name__,
            "reason": str(error.reason),
            "url": error.url,
        }
    except urllib3.exceptions.HTTPError as error:
        return {"ok": False, "raised": type(error).__name__, "detail": str(error)}
    return _answer(answered)
"#;

/// How long this suite waits for the condition it needs — Lambda reporting the created function
/// `Active` — and how often it looks while waiting. The deadline is far longer than the span
/// this emulator gives that transition, so a loaded host cannot decide the outcome; what is
/// asserted is the CONDITION, never the time it took.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The state an invocation needs the function to be in.
const ACTIVE: &str = "Active";

/// Look at the function's own configuration until Lambda reports it [`ACTIVE`].
async fn awaited_active(router: &MultiServiceRouter) {
    let giving_up_at = Instant::now() + ACTIVE_DEADLINE;
    loop {
        let (status, _headers, reported) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if reported["State"] == json!(ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: expected Lambda to report {FUNCTION:?} {ACTIVE}, last read {status} \
             ({reported})"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One row: how the request is made, what it addresses, the document it carries, and what the
/// client must report back.
struct Row {
    case: &'static str,
    how: &'static str,
    method: &'static str,
    url: &'static str,
    target: Option<&'static str>,
    document: Value,
    /// The status the answer must carry, for a request the runtime makes.
    status: Option<u16>,
    /// The place in the answered document a served call must carry, with the value it must
    /// have there.
    answered: Vec<(&'static [&'static str], Value)>,
    /// The parameter Systems Manager must hold afterwards, with the value it must hold.
    held: Option<(String, &'static str)>,
    /// The error class the library must raise, for an address no request is made to.
    raised: Option<&'static str>,
}

/// The address every served row addresses: Systems Manager's own regional endpoint, which is
/// the host naming that service — and, for one row, that same endpoint carrying a query of its
/// own, which pins that a URL's query travels with the request.
fn endpoint() -> &'static str {
    "https://ssm.us-east-1.amazonaws.com/"
}

/// The parameter one row writes.
fn parameter(suffix: &str) -> String {
    format!("{PARAMETER_PREFIX}{suffix}")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a request through the pool manager the handler holds",
            how: "pool_manager_request",
            method: "POST",
            url: endpoint(),
            target: Some("PutParameter"),
            document: json!({
                "Name": parameter("pool-manager"),
                "Value": "written through the pool manager",
                "Type": "String",
            }),
            status: Some(200),
            answered: vec![(&["Version"], json!(1))],
            held: Some((parameter("pool-manager"), "written through the pool manager")),
            raised: None,
        },
        Row {
            case: "a request through the module-level request, which holds no pool itself",
            how: "module_request",
            method: "POST",
            url: endpoint(),
            target: Some("PutParameter"),
            document: json!({
                "Name": parameter("module"),
                "Value": "written through the module",
                "Type": "String",
            }),
            status: Some(200),
            answered: vec![(&["Version"], json!(1))],
            held: Some((parameter("module"), "written through the module")),
            raised: None,
        },
        Row {
            case: "a request through a connection pool made from the URL",
            how: "connection_pool_request",
            method: "POST",
            url: endpoint(),
            target: Some("PutParameter"),
            document: json!({
                "Name": parameter("connection-pool"),
                "Value": "written through a connection pool",
                "Type": "StringList",
            }),
            status: Some(200),
            answered: vec![(&["Version"], json!(1))],
            held: Some((
                parameter("connection-pool"),
                "written through a connection pool",
            )),
            raised: None,
        },
        Row {
            case: "a request through urlopen, the call the three above are built on",
            how: "pool_manager_urlopen",
            method: "POST",
            url: endpoint(),
            target: Some("PutParameter"),
            document: json!({
                "Name": parameter("urlopen"),
                "Value": "written through urlopen",
                "Type": "String",
            }),
            status: Some(200),
            answered: vec![(&["Version"], json!(1))],
            held: Some((parameter("urlopen"), "written through urlopen")),
            raised: None,
        },
        Row {
            case: "a request whose URL carries a query of its own",
            how: "pool_manager_request",
            method: "POST",
            url: "https://ssm.us-east-1.amazonaws.com/?probe=query",
            target: Some("PutParameter"),
            document: json!({
                "Name": parameter("queried"),
                "Value": "written through a queried address",
                "Type": "String",
            }),
            status: Some(200),
            answered: vec![(&["Version"], json!(1))],
            held: Some((parameter("queried"), "written through a queried address")),
            raised: None,
        },
        // A call the service REFUSES is an ANSWER: the client reports the status and the error
        // document the service sent, and raises nothing — which is what makes a handler read
        // the service's own refusal.
        Row {
            case: "a request the addressed service refuses, which is answered and not raised",
            how: "pool_manager_request",
            method: "POST",
            url: endpoint(),
            target: Some("GetParameter"),
            document: json!({ "Name": ABSENT_PARAMETER }),
            status: Some(400),
            answered: vec![(&["__type"], json!("ParameterNotFound"))],
            held: None,
            raised: None,
        },
        // An address this emulator serves nothing at: the request is not made, and the
        // library's own error says so rather than an answer being invented for it.
        Row {
            case: "an address no service of this emulator answers at",
            how: "pool_manager_request",
            method: "GET",
            url: UNSERVED_ADDRESS,
            target: None,
            document: Value::Null,
            status: None,
            answered: Vec::new(),
            held: None,
            raised: Some("MaxRetryError"),
        },
    ]
}

/// RULE: a managed `python3.x` runtime carries the HTTP client its bundled SDK is built on,
/// so a handler that sends a request with it reaches this emulator and reads back the
/// addressed service's own answer — and an address nothing is served at is reported as a
/// request that was not made.
#[tokio::test]
async fn a_running_handler_sends_requests_with_the_runtimes_own_http_client() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();

    let (status, created) = {
        let (status, _headers, created) = lambda(
            &router,
            "POST",
            FUNCTIONS_URI,
            &serde_json::to_vec(&json!({
                "FunctionName": FUNCTION,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
                },
            }))
            .expect("the create body serializes"),
        )
        .await;
        (status, created)
    };
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    // "During this time, you can't invoke or modify the function" (`CreateFunction`): the
    // created function leaves `Pending` by the clock, so every row is invoked once Lambda
    // reports it `Active` — which is looked for rather than waited out.
    awaited_active(&router).await;

    for row in rows() {
        let case = row.case;
        let mut headers = serde_json::Map::new();
        if let Some(target) = row.target {
            headers.insert(
                "content-type".to_string(),
                json!("application/x-amz-json-1.1"),
            );
            headers.insert("x-amz-target".to_string(), json!(format!("AmazonSSM.{target}")));
        }
        let event = json!({
            "how": row.how,
            "method": row.method,
            "url": row.url,
            "headers": Value::Object(headers),
            "document": row.document,
        });
        let (status, response_headers, payload) = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
        );
        let signal = response_headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok());
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
        );

        match row.raised {
            Some(class) => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the request not to be made, got {payload}"
                );
                assert_eq!(
                    payload["raised"],
                    json!(class),
                    "case {case:?}: expected {class} to be raised, got {payload}"
                );
            }
            None => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the client to answer the request, got {payload}"
                );
                assert_eq!(
                    payload["class"],
                    json!("HTTPResponse"),
                    "case {case:?}: expected the library's own response class, got {payload}"
                );
                let expected = row.status.expect("a served row states its status");
                assert_eq!(
                    payload["status"],
                    json!(expected),
                    "case {case:?}: expected the status the service answered ({expected}), got \
                     {payload}"
                );
                assert_eq!(
                    payload["redirect"],
                    json!(false),
                    "case {case:?}: an answer that is no redirect names no location, got {payload}"
                );
                assert!(
                    payload["data_length"].as_u64().unwrap_or_default() > 0,
                    "case {case:?}: expected the answer's own body to be read back, got {payload}"
                );
                assert_eq!(
                    payload["content_type"],
                    json!("application/x-amz-json-1.1"),
                    "case {case:?}: expected the media type the service answered in, got {payload}"
                );
                for (place, wanted) in &row.answered {
                    let mut read = &payload["document"];
                    for member in *place {
                        read = &read[member];
                    }
                    assert_eq!(
                        read, wanted,
                        "case {case:?}: expected the answered document's {place:?} to be \
                         {wanted}, got {payload}"
                    );
                }
            }
        }

        // The other half: what Systems Manager itself holds, read where a client reads it — so
        // a request that answered without doing the work fails here.
        if let Some((name, value)) = row.held {
            let (status, reported) = ssm(&router, "GetParameter", &json!({ "Name": name })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetParameter({name:?}), got {status} \
                 ({reported})"
            );
            assert_eq!(
                reported["Parameter"]["Value"],
                json!(value),
                "case {case:?}: expected the store to hold {value:?}, got {reported}"
            );
        }
    }

    // The refused row addressed a name no parameter answers to, and the refusal left none.
    let (status, reported) = ssm(
        &router,
        "GetParameter",
        &json!({ "Name": ABSENT_PARAMETER }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected the absent parameter to stay absent, got {status} ({reported})"
    );
}
