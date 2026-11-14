// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RUNTIME API this emulator serves a running handler at the
//! address `AWS_LAMBDA_RUNTIME_API` names.
//!
//! RULE: a request a running handler makes to that address reaches the runtime API, so a
//! runtime, an extension or a handler that probes one of the paths this emulator answers
//! reads that answer — `GET /2018-06-01/runtime/restore/next` is `404` while no snapshot is
//! being restored, which every invocation here is — and the probing invocation itself
//! succeeds, rather than failing inside the sandbox on an address it could not resolve.
//!
//! The rows differ along what the rule generalises over: how the handler addresses the
//! interface (the address the environment names, and the same address written out), and the
//! runtime the probe runs on.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name. Nothing in it is verified:
/// the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(200);

/// The path a SnapStart-aware runtime waits for its restore on.
const RESTORE_NEXT_PATH: &str = "/2018-06-01/runtime/restore/next";

/// The handler every row runs: it probes the path the event names, on the address the event
/// names, and reports the status it read — so what is asserted is what the runtime API
/// answered the guest.
const HANDLER_SOURCE: &str = r#"
import os
import urllib.error
import urllib.request


def lambda_handler(event, context):
    address = event.get("address") or os.environ["AWS_LAMBDA_RUNTIME_API"]
    url = "http://" + address + event["path"]
    sent = urllib.request.Request(url, method=event.get("method", "GET"))
    try:
        with urllib.request.urlopen(sent) as answer:
            return {"status": answer.status}
    except urllib.error.HTTPError as error:
        return {"status": error.code}
"#;

/// One dispatched request's answer.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one control-plane request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// A .zip deployment package holding the handler above.
fn package() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file::<_, ()>("lambda_function.py", zip::write::SimpleFileOptions::default())
        .expect("the entry must open");
    archive
        .write_all(HANDLER_SOURCE.as_bytes())
        .expect("the entry must be written");
    archive
        .finish()
        .expect("the archive must finish")
        .into_inner()
}

/// Create the probing function and wait out the provisioning window.
async fn given_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 20,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }))
        .expect("the request body must serialize"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// RULE: the restore endpoint of the runtime API answers `404` while no snapshot is being
/// restored, and the probing invocation itself succeeds.
#[tokio::test]
async fn a_handler_probing_the_restore_endpoint_reads_its_answer() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "runtime-api-probe").await;

    // (case, the address the handler probes, the status it must read)
    let rows: &[(&str, Value, u16)] = &[
        (
            "the address the environment names",
            json!({ "path": RESTORE_NEXT_PATH }),
            404,
        ),
        (
            "the same address written out",
            json!({ "path": RESTORE_NEXT_PATH, "address": "127.0.0.1:9001" }),
            404,
        ),
        (
            "the same path carrying a query",
            json!({ "path": format!("{RESTORE_NEXT_PATH}?probe=1") }),
            404,
        ),
        // Every other path of the interface is one it does not route: the invocation half of
        // it is served by nothing here, so nothing is answered for it.
        (
            "another path of the interface",
            json!({ "path": "/2018-06-01/runtime/invocation/next" }),
            404,
        ),
        (
            "another method on the restore endpoint",
            json!({ "path": RESTORE_NEXT_PATH, "method": "POST" }),
            404,
        ),
    ];
    for (case, event, status) in rows {
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/runtime-api-probe/invocations"),
            &serde_json::to_vec(event).expect("the event must serialize"),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected the probing invocation to succeed, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the probe itself must not fail, got {}",
            answered.text()
        );
        assert_eq!(
            answered.json(),
            json!({ "status": status }),
            "case {case:?}: expected the interface's own answer, got {}",
            answered.text()
        );
    }
}
