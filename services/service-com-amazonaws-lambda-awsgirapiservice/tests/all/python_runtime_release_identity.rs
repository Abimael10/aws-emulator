// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RELEASE of Python a function's run happens on: `CreateFunction`
//! then `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a managed runtime IS a release of its language, so the version of the
//! interpreter a handler observes is the one the function's `Runtime` identifier names — a
//! function created with one identifier runs on that release and a function created with
//! another runs on another's, on either architecture, and every reading of the version the
//! interpreter answers (the version's own fields, the version TEXT the `platform` module reads
//! the running version out of, and the single number a version is packed into) states the same
//! release.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: WHICH `Runtime` identifier the function carries (five members of the
//! model's own enum, including two the issue behind this suite never cited), which
//! `Architectures` entry it carries beside it (both values of that enum), and whether the
//! version is read while the handler's module is being IMPORTED or from the handler itself.

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

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
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

/// Create a function carrying `runtime` and `architecture`, whose package holds the probe as
/// `index.py`, and wait out the provisioning window the create documents.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    runtime: &str,
    architecture: &str,
) {
    let body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": runtime,
        "Architectures": [architecture],
        "Handler": "index.handler",
        "Timeout": FUNCTION_TIMEOUT_SECONDS,
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(package(&[("index.py", PROBE_SOURCE)])),
        },
    });
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
        "CreateFunction {name:?} with {runtime} must succeed, got {} {}",
        created.status,
        created.text()
    );
    // The runtime the create was answered with is the one it was asked for, which is what the
    // run below is the run of.
    let echoed = created.json();
    assert_eq!(
        echoed.get("Runtime").and_then(Value::as_str),
        Some(runtime),
        "CreateFunction {name:?} must report the runtime it was given, got {echoed}"
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole
/// test binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &[u8]) -> Answer {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, event).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &[u8]) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await
}

/// What one row's handler answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the row asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    event: &[u8],
    case: &str,
) -> Option<Value> {
    let answer = invoke(router, name, event).await;
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
        "case {case}: the handler's module imports cleanly and reads its own version, so the \
         invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// One row of the table: the `Runtime` the function is created with, the `Architectures` entry
/// beside it, and the release that runtime names.
struct Row {
    /// The case this row is named by in every assertion.
    case: &'static str,
    /// The `Runtime` identifier the create carries.
    runtime: &'static str,
    /// The `Architectures` entry the create carries.
    architecture: &'static str,
    /// The major and minor version a handler of that function must observe.
    release: (u64, u64),
}

/// The whole table: five `python` members of the model's `Runtime` enum
/// (`smithy_query(query="shape", shape="Runtime")`), each paired with a value of the
/// `Architectures` enum (`smithy_query(query="shape", shape="Architecture")`).
///
/// `python3.10` and `python3.15` are members the issue behind this suite never cited, so the
/// table reads the correspondence rather than the four pairings one run reported.
const ROWS: &[Row] = &[
    Row {
        case: "a python3.11 function on arm64",
        runtime: "python3.11",
        architecture: "arm64",
        release: (3, 11),
    },
    Row {
        case: "a python3.12 function on x86_64",
        runtime: "python3.12",
        architecture: "x86_64",
        release: (3, 12),
    },
    Row {
        case: "a python3.13 function on arm64",
        runtime: "python3.13",
        architecture: "arm64",
        release: (3, 13),
    },
    Row {
        case: "a python3.14 function on x86_64",
        runtime: "python3.14",
        architecture: "x86_64",
        release: (3, 14),
    },
    Row {
        case: "a python3.10 function on x86_64",
        runtime: "python3.10",
        architecture: "x86_64",
        release: (3, 10),
    },
    Row {
        case: "a python3.15 function on arm64",
        runtime: "python3.15",
        architecture: "arm64",
        release: (3, 15),
    },
];

/// The whole rule, driven END TO END as one test: every row below is a real `CreateFunction`
/// and a real `Invoke` over the router.
///
/// One test and one stream of invocations, the way the sibling runtime suites are built: the
/// memory bound a run is stopped on is read from the WHOLE process
/// (`state::execution::python`) and this binary runs its suites in parallel, so fewer
/// interpreters in flight at once is fewer runs stopped for memory belonging to another suite.
/// A row whose run was stopped by one of those shared bounds says nothing about the rule and
/// is left out of the reading — but not every one can be: a test that read none of them at all
/// is a test that asserted nothing, so it fails rather than passing silently.
#[tokio::test]
async fn a_python_run_happens_on_the_release_its_runtime_names() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let mut read = 0_usize;
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("probe-runtime-release-{index}");
        create_function(&router, &name, row.runtime, row.architecture).await;
        let case = row.case;
        let (major, minor) = row.release;
        let expected = json!({
            // The two fields the version is read by, positionally and by name, from the
            // handler itself.
            "version": [major, minor],
            "major": major,
            "minor": minor,
            // The version TEXT, which is what the `platform` module reads the running
            // version out of: the release the runtime names, patch level and all.
            "python_version": format!("{major}.{minor}.0"),
            "python_version_tuple": [major.to_string(), minor.to_string(), "0".to_string()],
            // The same release, packed into the one number a Python program reads it from.
            "hexversion_release": [major, minor],
            // The module body read the version WHILE BEING IMPORTED, which is where a
            // vendored client library decides which language features it has.
            "imported_version": [major, minor],
            // A version-conditional comparison in the handler, against the release the
            // EVENT names rather than against the version it just read: the interpreter is
            // at least that release and below the next one, which holds for exactly one
            // release.
            "at_least_the_release_asked_for": true,
            "below_the_release_after_it": true,
        });
        // What the invocation asks the handler to compare the interpreter against: the row's
        // own release, so the comparison is a reading of the installed release and not of
        // itself.
        let event = serde_json::to_vec(&json!({"release": [major, minor]}))
            .expect("the event serializes");
        // Twice: the invocation an execution environment is BUILT for, and one served in the
        // environment that is then standing — the release belongs to the environment, so a
        // warm invocation reads the same one.
        for served_in in [
            "the environment this invocation built",
            "the environment already standing",
        ] {
            let Some(document) = answered(&router, &name, &event, case).await else {
                continue;
            };
            read += 1;
            assert_eq!(
                document, expected,
                "case {case} ({served_in}): expected the release {major}.{minor} the runtime \
                 names, got {document}"
            );
        }
    }
    assert!(
        read > 0,
        "expected at least one row of the table above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the \
         rule was read"
    );
}

/// The probe every row drives: its module body reads the interpreter's version — the shape a
/// vendored client library's own module body has — and its handler answers every reading of
/// that version the interpreter carries, plus the version-conditional comparison a library
/// makes against a release its CALLER names (the event's `release`).
const PROBE_SOURCE: &str = r#"
import platform
import sys

# Read WHILE THIS MODULE IS BEING IMPORTED, which is where a client library decides which
# language features it has.
_IMPORTED = list(sys.version_info[:2])


def handler(event, context):
    asked = tuple(event["release"])
    after = (asked[0], asked[1] + 1)
    return {
        "version": list(sys.version_info[:2]),
        "major": sys.version_info.major,
        "minor": sys.version_info.minor,
        "python_version": platform.python_version(),
        "python_version_tuple": list(platform.python_version_tuple()),
        "hexversion_release": [sys.hexversion >> 24, (sys.hexversion >> 16) & 0xFF],
        "imported_version": _IMPORTED,
        "at_least_the_release_asked_for": sys.version_info >= asked,
        "below_the_release_after_it": sys.version_info < after,
    }
"#;
