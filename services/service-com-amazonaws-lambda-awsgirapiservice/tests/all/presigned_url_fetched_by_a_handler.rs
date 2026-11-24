// SPDX-License-Identifier: CC0-1.0

//! A PRESIGNED URL fetched from INSIDE a running function, end to end over the wire: Lambda,
//! IAM, the token service and Amazon S3 on one engine, a `nodejs*` function whose handler
//! fetches the URL it is handed with the runtime's own HTTP client, and the object's bytes
//! read back where a client reads them.
//!
//! RULE: a presigned URL states the service it was signed for in its own query string — the
//! SigV4 form in its credential scope, the legacy form by being that form — so the seam that
//! carries a handler's outbound HTTP into this emulator attributes such a URL on the
//! emulator's own endpoint to that service whatever its path addressing, and sends it as the
//! URL states itself, with no credential of the handler's over it: a presigned URL is
//! authorized by the credential it was signed with. Fetching one is how object access is
//! handed to code that holds no credential of its own, so the function below holds no object
//! permission at all — the URL is what grants the read.
//!
//! The rows differ along what the rule generalises over: WHICH query form carries the
//! credential (the legacy one and the SigV4 one), which host form the endpoint is addressed
//! by (a loopback literal with a port, the loopback name), how many segments the object's key
//! has, whether the object store SERVES or REFUSES the read, the same object addressed with
//! no presigned credential — which is the handler's own role asking, and is refused — a URL
//! on the same endpoint that carries no presigned credential, and a presigned URL on a host
//! that names a service of its own: the last two name no address this seam makes a request
//! to, and are reported as requests that were not made.
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

/// The bucket the presigned URLs address, the objects it is seeded with and their bytes.
const BUCKET: &str = "presigned-fetch-objects";
const FETCHED: &str = "fetched.bin";
const FETCHED_CONTENT: &str = "the bytes a presigned GET answers";
const NESTED: &str = "reports/2026/11/nested.json";
const NESTED_CONTENT: &str = "{\"report\": \"nested\"}";
const ABSENT: &str = "never-stored.bin";

/// The `Runtime` the function carries: a managed node runtime, whose `https` module is the
/// ordinary HTTP client a presigned URL is fetched with.
const RUNTIME: &str = "nodejs22.x";

/// The endpoint the presigned URLs below are minted against: this emulator's own, which names
/// no service — addressed by the loopback literal with a port, and by the loopback name.
const LOOPBACK_ENDPOINT: &str = "https://127.0.0.1:9297";
const NAMED_ENDPOINT: &str = "https://localhost:9297";

/// The instant the legacy form's `Expires` names: a fixed point far in the future (1 January
/// 2100, in the epoch seconds that parameter carries), so the URL a row states is the same
/// one on every run and nothing here reads a clock.
const FAR_FUTURE_EPOCH_SECONDS: u64 = 4_102_444_800;

/// The handler: it fetches `event.url` with the runtime's own `https.get` and reports what
/// came back — the status and the body — or the failure the request reported, naming its
/// code.
///
/// Nothing here synthesises an answer: every field reported is read off the message the
/// client handed the handler.
const HANDLER_SOURCE: &str = r#"
'use strict';

const https = require('https');

exports.handler = async (event) => new Promise((resolve) => {
    const sending = https.get(event.url, (response) => {
        let text = '';
        response.setEncoding('utf8');
        response.on('data', (chunk) => { text += chunk; });
        response.on('end', () => resolve({
            status: response.statusCode,
            complete: response.complete,
            body: text,
        }));
    });
    sending.on('error', (error) => resolve({ failed: error.message, code: error.code }));
});
"#;

/// Lambda, IAM, the token service and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own request is dispatched back into
/// this same router, so the bytes it reads are the bytes this suite stored.
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

/// The policy the function's own role carries: it may write its logs and NOTHING on the
/// object store, which is the position a function handed a presigned URL is in — the URL is
/// what grants the object access, not the role.
fn logs_only_access() -> Value {
    json!([{ "Effect": "Allow", "Action": "logs:*", "Resource": "*" }])
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

/// The LEGACY presigned query a client hands out for an object: the access key id, the
/// signature and the expiry, which are the three parameters that form carries (AWS General
/// Reference, *Authenticating requests: using query parameters*).
fn legacy_presigned_query() -> String {
    format!(
        "AWSAccessKeyId=AKIDEXAMPLEPRESIGNED&Signature=ZGVhZGJlZWZzaWduYXR1cmU%3D&\
         Expires={FAR_FUTURE_EPOCH_SECONDS}"
    )
}

/// The SigV4 presigned query for the same object: the algorithm, the credential scope — whose
/// fourth field is the signing name — the instant it was signed at, how long it is valid, the
/// headers it covers and the signature.
fn sigv4_presigned_query() -> String {
    format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEXAMPLEPRESIGNED%2F20260101\
         %2F{REGION}%2Fs3%2Faws4_request&X-Amz-Date=20260101T000000Z&X-Amz-Expires=3600&\
         X-Amz-SignedHeaders=host&X-Amz-Signature=deadbeefsignature"
    )
}

/// One row: the URL the handler is handed, and what the addressed service is expected to
/// answer it with.
struct Row {
    case: &'static str,
    /// The URL the handler fetches.
    url: String,
    /// The status the object store answers, or `None` for a row whose request is not made at
    /// all.
    status: Option<u64>,
    /// Text the answered body must carry, for a row that asserts on it.
    carries: Option<&'static str>,
    /// Text the reported FAILURE must carry, for a row whose request is not made and whose
    /// refusal says which address named no service.
    failure_carries: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a path-style legacy presigned GET on the loopback endpoint",
            url: format!(
                "{LOOPBACK_ENDPOINT}/{BUCKET}/{FETCHED}?{}",
                legacy_presigned_query()
            ),
            status: Some(200),
            carries: Some(FETCHED_CONTENT),
            failure_carries: None,
        },
        Row {
            case: "the SigV4 query form, whose scope names the service",
            url: format!(
                "{LOOPBACK_ENDPOINT}/{BUCKET}/{FETCHED}?{}",
                sigv4_presigned_query()
            ),
            status: Some(200),
            carries: Some(FETCHED_CONTENT),
            failure_carries: None,
        },
        Row {
            case: "a key of several segments, on the loopback name",
            url: format!(
                "{NAMED_ENDPOINT}/{BUCKET}/{NESTED}?{}",
                legacy_presigned_query()
            ),
            status: Some(200),
            carries: Some(NESTED_CONTENT),
            failure_carries: None,
        },
        Row {
            case: "a presigned read the object store REFUSES, whose own answer is the handler's",
            url: format!(
                "{LOOPBACK_ENDPOINT}/{BUCKET}/{ABSENT}?{}",
                legacy_presigned_query()
            ),
            status: Some(404),
            carries: Some("NoSuchKey"),
            failure_carries: None,
        },
        Row {
            case: "the same endpoint and path carrying no presigned credential",
            url: format!("{LOOPBACK_ENDPOINT}/{BUCKET}/{FETCHED}"),
            status: None,
            carries: None,
            failure_carries: None,
        },
        Row {
            case: "the same object addressed with no presigned credential at all, which is \
                   the handler's own role asking",
            url: format!("https://s3.{REGION}.amazonaws.com/{BUCKET}/{FETCHED}"),
            status: Some(403),
            carries: Some("AccessDenied"),
            failure_carries: None,
        },
        Row {
            case: "a presigned URL on a host that names a service of its own",
            url: format!(
                "https://example.test/{BUCKET}/{FETCHED}?{}",
                legacy_presigned_query()
            ),
            status: None,
            carries: None,
            failure_carries: Some("addresses no service"),
        },
    ]
}

/// Invoke `name` with the URL `row` states, and answer the document the handler returned.
async fn invoked(router: &MultiServiceRouter, row: &Row, name: &str) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &json!({ "url": row.url }),
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

/// A presigned URL on this emulator's own endpoint is fetched from inside a function and
/// answers the object store's own status and bytes, whichever query form carries its
/// credential and however its path addresses the object.
#[tokio::test]
async fn a_handler_fetches_a_presigned_url_on_the_emulators_own_endpoint() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a handler fetching a presigned URL";
    let router = emulator();
    let (created, body) = s3(&router, "PUT", &format!("/{BUCKET}"), b"").await;
    assert_eq!(
        created, 200,
        "case {case:?}: expected CreateBucket to answer 200, got {created}: {body}"
    );
    for (key, content) in [(FETCHED, FETCHED_CONTENT), (NESTED, NESTED_CONTENT)] {
        let (stored, body) = s3(
            &router,
            "PUT",
            &format!("/{BUCKET}/{key}"),
            content.as_bytes(),
        )
        .await;
        assert_eq!(
            stored, 200,
            "case {key:?}: expected PutObject to answer 200, got {stored}: {body}"
        );
    }

    let role = given_role(&router, case, "presigned-fetch-role", Some(logs_only_access())).await;
    let function = "presigned-url-fetcher";
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
                    !failed.is_empty(),
                    "case {case:?}: a URL naming no service must be reported as a request that \
                     was not made, got {answered}"
                );
                assert_eq!(
                    answered.get("status"),
                    None,
                    "case {case:?}: a request nothing carried answers no status, got {answered}"
                );
                if let Some(carries) = row.failure_carries {
                    assert!(
                        failed.contains(carries),
                        "case {case:?}: expected the reported failure to carry {carries:?}, got \
                         {answered}"
                    );
                }
            }
            Some(status) => {
                assert_eq!(
                    answered.get("status").and_then(Value::as_u64),
                    Some(status),
                    "case {case:?}: expected the object store's own status, got {answered}"
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

    // The bytes the handler read are the object store's own: the same read through that
    // service's API answers the same bytes, so nothing above was synthesised for the fetch.
    for (key, content) in [(FETCHED, FETCHED_CONTENT), (NESTED, NESTED_CONTENT)] {
        let (status, body) = s3(&router, "GET", &format!("/{BUCKET}/{key}"), b"").await;
        assert_eq!(
            status, 200,
            "case {key:?}: expected the stored object to be readable, got {status}: {body}"
        );
        assert_eq!(
            body, content,
            "case {key:?}: expected the stored bytes, got {body:?}"
        );
    }
}
