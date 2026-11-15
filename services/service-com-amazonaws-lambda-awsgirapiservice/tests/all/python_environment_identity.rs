// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a `python3.x` function's run reports about its OWN EXECUTION
//! ENVIRONMENT: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: the Python a function runs on answers a call about the execution
//! environment from the facts of THAT environment — the operating system a managed runtime
//! runs on, the environment's own name, and the instruction set the function's
//! `Architectures` names — so a module whose body asks which platform it is on imports
//! cleanly and reads the function's own answer, and nothing of the machine this emulator's
//! process happens to be on is reported. Only a call needing a capability the sandbox does
//! not have is refused, and never an import.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what
//! the rule generalises over: which `Architectures` entry the function carries (both values
//! of the model's enum, and a function that names none), whether the surface is read through
//! `os` or through the `platform` module built on it, and whether the read happens while the
//! handler's module is being IMPORTED or from the handler itself.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by
/// the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Sized the way the
/// sibling suites that RUN handlers size theirs: it has to cover this test binary's own
/// scheduling, not the handlers' work, which is immediate.
const FUNCTION_TIMEOUT_SECONDS: i32 = 25;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// What a dispatched request answered.
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
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
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

/// Dispatch one request.
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

/// Create a `python3.12` function whose package holds `source` as `index.py`, carrying the
/// `Architectures` list `architectures` names (nothing at all when it is `None`), and wait
/// out the provisioning window the create documents.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    source: &str,
    architectures: Option<&str>,
) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Timeout": FUNCTION_TIMEOUT_SECONDS,
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&[("index.py", source)])),
        },
    });
    if let Some(architecture) = architectures {
        body["Architectures"] = json!([architecture]);
    }
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&body).expect("the create body serializes"),
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

/// `Invoke` on `name`, re-driven while the answer reports a bound this whole test binary
/// shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str) -> Answer {
    let mut answered = invoke_once(router, name).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        b"{}",
    )
    .await
}

/// What one row's handler answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the row asked for.
async fn answered(router: &MultiServiceRouter, name: &str, case: &str) -> Option<Value> {
    let answer = invoke(router, name).await;
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler's module imports cleanly and reads its own environment, so \
         the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// One row of the table: the `Architectures` entry the function carries, and the machine name
/// a POSIX `uname` gives that instruction set.
struct Row {
    /// The case this row is named by in every assertion.
    case: &'static str,
    /// The `Architectures` entry the create carries, or `None` for a create that names none.
    architecture: Option<&'static str>,
    /// What the run must report as its own machine.
    machine: &'static str,
}

/// The whole table: both values of the model's `Architectures` enum
/// (`smithy_query(query="shape", shape="Architecture")` — `arm64` and `x86_64`), and a
/// function that names none, which the model's own member documentation gives `x86_64`.
/// The machine name of each is the name the Linux kernel's `uname` gives that instruction
/// set.
const ROWS: &[Row] = &[
    Row {
        case: "a function that names no architecture at all",
        architecture: None,
        machine: "x86_64",
    },
    Row {
        case: "a function configured for x86_64",
        architecture: Some("x86_64"),
        machine: "x86_64",
    },
    Row {
        case: "a function configured for arm64",
        architecture: Some("arm64"),
        machine: "aarch64",
    },
];

/// The whole rule, driven END TO END as one test: every row below is a real `CreateFunction`
/// and a real `Invoke` over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another
/// suite. A row whose run was stopped by one of those shared bounds says nothing about the
/// rule and is left out of the reading — but not every one can be: a test that read none of
/// them at all is a test that asserted nothing, so it fails rather than passing silently.
#[tokio::test]
async fn a_python_run_reports_the_execution_environment_it_is_in() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = 0_usize;
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("probe-environment-identity-{index}");
        create_function(&router, &name, PROBE_SOURCE, row.architecture).await;
        let Some(document) = answered(&router, &name, row.case).await else {
            continue;
        };
        read += 1;
        let case = row.case;
        let mut document = document;
        // The environment's own NAME is minted per execution environment, so it is read out
        // of the answer rather than written here: what the rule states about it is that it
        // is the name this environment reports its log under, and that it is a name at all.
        let nodename = document
            .get("nodename")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            !nodename.is_empty(),
            "case {case}: the environment's own name must be the one it reports its log \
             under, got {nodename:?}"
        );
        let fields = document
            .as_object_mut()
            .and_then(|document| document.remove("fields"))
            .unwrap_or(Value::Null);
        assert_eq!(
            fields,
            json!(["Linux", nodename, "", "", row.machine]),
            "case {case}: expected the five values of the call to be the environment's own, \
             got {fields}"
        );
        // Everything else the rule fixes, read through the call itself and through the
        // `platform` module built on it, from the handler and from the module body that was
        // imported before it.
        let expected = json!({
            "sysname": "Linux",
            "nodename": nodename,
            "release": "",
            "version": "",
            "machine": row.machine,
            "length": 5,
            "type": "uname_result",
            // The name is the execution environment's own, which is the log stream this
            // invocation's own context names.
            "named_for_the_environment": true,
            // And the `platform` module built on the call reads exactly the same.
            "platform_system": "Linux",
            "platform_machine": row.machine,
            "platform_release": "",
            "platform_version": "",
            "platform_node_is_the_nodename": true,
            "platform_uname_system": "Linux",
            "platform_uname_machine": row.machine,
            "platform_is_windows": false,
            // The module body read the platform WHILE BEING IMPORTED, which is what a
            // vendored client library does, and the import was clean.
            "imported_system": "Linux",
            "imported_windows": false,
        });
        assert_eq!(
            document, expected,
            "case {case}: expected the environment the function is configured for, got \
             {document}"
        );
    }
    assert!(
        read > 0,
        "expected at least one row of the table above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// The probe every row drives: its module body asks which platform it is on — the shape a
/// vendored client library's own module body has — and its handler answers what the run
/// reports about the environment it is in.
const PROBE_SOURCE: &str = r#"
import os
import platform

# Read WHILE THIS MODULE IS BEING IMPORTED, which is where a client library decides whether
# it is on Windows.
_SYSTEM = platform.system()
_WINDOWS = _SYSTEM == "Windows"


def handler(event, context):
    identity = os.uname()
    through_platform = platform.uname()
    return {
        "fields": list(identity),
        "sysname": identity.sysname,
        "nodename": identity.nodename,
        "release": identity.release,
        "version": identity.version,
        "machine": identity.machine,
        "length": len(identity),
        "type": type(identity).__name__,
        "named_for_the_environment": identity.nodename == context.log_stream_name,
        "platform_system": platform.system(),
        "platform_machine": platform.machine(),
        "platform_release": platform.release(),
        "platform_version": platform.version(),
        "platform_node_is_the_nodename": platform.node() == identity.nodename,
        "platform_uname_system": through_platform.system,
        "platform_uname_machine": through_platform.machine,
        "platform_is_windows": platform.system() == "Windows",
        "imported_system": _SYSTEM,
        "imported_windows": _WINDOWS,
    }
"#;
