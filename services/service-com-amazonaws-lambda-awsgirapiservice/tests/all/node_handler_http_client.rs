// SPDX-License-Identifier: CC0-1.0

//! The HTTP CLIENT a managed node runtime presents to a handler, end to end over the wire:
//! Lambda, IAM, the token service and Amazon S3 on one engine, a `nodejs*` function whose
//! code builds its own requests with `https`, and the objects it wrote read back where a
//! client reads them.
//!
//! RULE: a managed node runtime supplies the `http` and `https` core modules, and a request
//! a handler builds with them is carried into this emulator and answered with the addressed
//! service's OWN status, headers and body — as the function's own execution role — while a
//! request to an address this emulator serves nothing at is reported to the handler as a
//! request that was not made. That is what lets a node handler answer a callback URL it was
//! handed (a CloudFormation custom resource provider's `ResponseURL`, which the CDK's own
//! providers answer exactly this way) and reach a data plane addressed by its own endpoint.
//!
//! The rows differ along what the rule generalises over: HOW the request is stated (an
//! options object parsed out of an address — the form a provider answering a callback URL
//! uses — an address with an options object over it, and the convenience `get`), WHICH
//! method it sends, whether the addressed service SERVES or REFUSES it, and whether the
//! address names a service of this emulator at all. The last assertions read the writes back
//! through the object store's OWN API, so a request that answered without doing the work
//! fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::given_role;
use crate::event_source_mappings::{await_function_active, call, router_on};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The bucket the handler reads and writes, and the object this suite seeds it with.
const BUCKET: &str = "node-http-client-objects";
const SEEDED: &str = "reports/seeded.json";
const CONTENT: &str = "{\"report\": \"seeded\"}";

/// The `Runtime` the function carries: a managed node runtime, which is what supplies the
/// two modules under test.
const RUNTIME: &str = "nodejs20.x";

/// The handler: it states ONE request the way the row asks for, and reports what came back
/// — the status, the reason, one header and the body — or the failure the request reported.
///
/// Nothing here synthesises an answer: every field reported is read off the message the
/// client handed the handler.
const HANDLER_SOURCE: &str = r#"
'use strict';

const https = require('https');
const url = require('url');

function collected(response, resolve) {
    let text = '';
    response.setEncoding('utf8');
    response.on('data', (chunk) => { text += chunk; });
    response.on('end', () => resolve({
        status: response.statusCode,
        reason: response.statusMessage,
        content_type: response.headers['content-type'] || '',
        complete: response.complete,
        body: text,
    }));
}

exports.handler = async (event) => new Promise((resolve) => {
    const parsed = url.parse(event.url);
    let sending;
    if (event.form === 'parsed-options') {
        // The shape a custom resource provider answers its callback URL in: the address is
        // parsed into options, the document is written, and then the request is ended.
        sending = https.request({
            hostname: parsed.hostname,
            port: parsed.port,
            path: parsed.path,
            method: event.method,
            headers: {
                'content-type': 'text/plain',
                'content-length': Buffer.byteLength(event.body || ''),
            },
        }, (answer) => collected(answer, resolve));
        sending.write(event.body);
        sending.end();
    } else if (event.form === 'url-with-options') {
        sending = https.request(
            event.url,
            { method: event.method, headers: { 'content-type': 'text/plain' } },
            (answer) => collected(answer, resolve)
        );
        sending.end(event.body);
    } else {
        sending = https.get(event.url, (answer) => collected(answer, resolve));
    }
    sending.on('error', (error) => resolve({ failed: error.message, code: error.code }));
});
"#;

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own requests are dispatched back into
/// this same router, so the objects it writes are the objects this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One request to Amazon S3, made as a CLIENT makes one: its protocol is `restXml`, so the
/// request is a method and a path and the answer is a document.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The policy a role that may read and write the bucket carries.
fn object_access() -> Value {
    json!([{ "Effect": "Allow", "Action": "s3:*", "Resource": "*" }])
}

/// Create the function `name`, running as `role_arn`, out of [`HANDLER_SOURCE`].
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": "index.handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(crate::node_invoke_run::package(&[("index.js", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, case, name).await;
}

/// One row: how the request is stated, what it sends, and what the addressed service is
/// expected to answer.
struct Row {
    case: &'static str,
    form: &'static str,
    method: &'static str,
    path: &'static str,
    body: &'static str,
    /// The status the addressed service answers, or `None` for a row whose request is not
    /// made at all.
    status: Option<u64>,
    /// Text the answered body must carry, for a row that asserts on it.
    carries: Option<&'static str>,
}

/// The address a row's request is sent to: this emulator's own S3 endpoint, or — for the row
/// that names no service of it — an address off it.
fn address(path: &str) -> String {
    if path.starts_with("https://") {
        return path.to_string();
    }
    format!("https://s3.{REGION}.amazonaws.com{path}")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an options object parsed out of an address, with the body written",
            form: "parsed-options",
            method: "PUT",
            path: "/node-http-client-objects/reports/written-by-options.txt",
            body: "written through the parsed options",
            status: Some(200),
            carries: None,
        },
        Row {
            case: "an address with an options object over it, ended with the body",
            form: "url-with-options",
            method: "PUT",
            path: "/node-http-client-objects/reports/written-by-url.txt",
            body: "written through the address",
            status: Some(200),
            carries: None,
        },
        Row {
            case: "the convenience get, reading the seeded object back",
            form: "get",
            method: "GET",
            path: "/node-http-client-objects/reports/seeded.json",
            body: "",
            status: Some(200),
            carries: Some(CONTENT),
        },
        Row {
            case: "the convenience get, reading one of the handler's own writes back",
            form: "get",
            method: "GET",
            path: "/node-http-client-objects/reports/written-by-options.txt",
            body: "",
            status: Some(200),
            carries: Some("written through the parsed options"),
        },
        Row {
            case: "a read the object store REFUSES, whose own answer is the handler's",
            form: "get",
            method: "GET",
            path: "/node-http-client-objects/reports/absent.json",
            body: "",
            status: Some(404),
            carries: Some("NoSuchKey"),
        },
        Row {
            case: "an address this emulator serves nothing at",
            form: "get",
            method: "GET",
            path: "https://example.test/anything",
            body: "",
            status: None,
            carries: None,
        },
    ]
}

/// Invoke `name` with the request `row` states, and answer the document the handler
/// returned.
async fn invoked(router: &MultiServiceRouter, row: &Row, name: &str) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({
            "form": row.form,
            "method": row.method,
            "url": address(row.path),
            "body": row.body,
        }),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {:?}: expected HTTP 200 from Invoke, got {} {}",
        row.case,
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {:?}: the handler must not fault, got {}",
        row.case,
        answered.text()
    );
    answered.json()
}

/// Every form of a request a node handler builds itself reaches the addressed service of
/// this emulator and reports that service's own answer — and the writes are really there.
#[tokio::test]
async fn a_node_handler_sends_its_own_requests_with_the_runtimes_http_client() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a node handler building its own requests";
    let router = emulator();
    let (created, body) = s3(&router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        created, 200,
        "case {case:?}: expected CreateBucket to answer 200, got {created}: {body}"
    );
    let (stored, body) = s3(
        &router,
        "PUT",
        &format!("/{BUCKET}/{SEEDED}"),
        CONTENT.as_bytes(),
    )
    .await;
    assert_eq!(
        stored, 200,
        "case {case:?}: expected PutObject to answer 200, got {stored}: {body}"
    );

    let role = given_role(&router, case, "node-http-client-role", Some(object_access())).await;
    let function = "node-http-client-sender";
    given_function(&router, case, function, &role).await;

    for row in rows() {
        let answered = invoked(&router, &row, function).await;
        let case = row.case;
        match row.status {
            None => {
                let failed = answered
                    .get("failed")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    failed.contains("addresses no service"),
                    "case {case:?}: an address this emulator serves nothing at must be reported \
                     as a request that was not made, got {answered}"
                );
                assert_eq!(
                    answered.get("status"),
                    None,
                    "case {case:?}: a request nothing carried answers no status, got {answered}"
                );
            }
            Some(status) => {
                assert_eq!(
                    answered.get("status").and_then(Value::as_u64),
                    Some(status),
                    "case {case:?}: expected the addressed service's own status, got {answered}"
                );
                assert_eq!(
                    answered.get("complete").and_then(Value::as_bool),
                    Some(true),
                    "case {case:?}: expected the whole answer to have been delivered, got \
                     {answered}"
                );
                if let Some(carries) = row.carries {
                    let body = answered
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    assert!(
                        body.contains(carries),
                        "case {case:?}: expected the answered body to carry {carries:?}, got \
                         {answered}"
                    );
                }
            }
        }
    }

    // The writes are backed by the object store itself: what the handler sent is what a
    // client reads there, through that service's own API.
    for (key, written) in [
        (
            "reports/written-by-options.txt",
            "written through the parsed options",
        ),
        ("reports/written-by-url.txt", "written through the address"),
    ] {
        let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
        assert_eq!(
            status, 200,
            "case {key:?}: expected the handler's write to be readable, got {status}: {body}"
        );
        assert_eq!(
            body, written,
            "case {key:?}: expected the bytes the handler sent, got {body:?}"
        );
    }
}
