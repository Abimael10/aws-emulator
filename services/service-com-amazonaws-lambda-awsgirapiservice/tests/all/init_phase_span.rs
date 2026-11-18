// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the span the INITIALIZATION of an execution environment is allowed.
//!
//! Every case drives real HTTP requests through the router into the mounted service, so what
//! is asserted is what a client of `Invoke` sees. The emulator, the request dispatcher and
//! the deployment-archive writer are this target's own
//! (`crate::asynchronous_invoke_run::{router_on, call}`,
//! `crate::synchronous_invoke_run::package`), reused rather than stood up again.
//!
//! The rule under test: an invocation happens in two phases, and the function's `Timeout` —
//! "the amount of time that Lambda allows a function to run before stopping it" — bounds the
//! second of them. The INIT phase, which runs the code OUTSIDE the handler, has a span of its
//! own, which AWS states is limited to 10 seconds
//! (`…::state::execution::INIT_PHASE_LIMIT`): module-level code that stays inside that span
//! is initialized and its handler then answers, while an initialization that spends the whole
//! span is given up on, so the caller is answered the platform's own timeout class and the
//! `Unhandled` function-error signal rather than a result the initialization never reached.
//!
//! The rows differ along what the rule generalises over — how long the module-level code
//! waits: not at all, well inside the span, and past it — and each asserts the whole answer a
//! client reads (the status, the function-error signal, and the payload or the error class),
//! never a span of this host's time.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::INIT_PHASE_LIMIT;
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use smithy_server_runtime::MultiServiceRouter;

use crate::asynchronous_invoke_run::{call, router_on, PYTHON_HANDLER};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past the activation the create owes this suite calls. The transition is owed by
/// the CLOCK, so nothing here waits on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(100);

/// The HANDLER's own span, stated SHORTER than the init phase's: a row that initializes at
/// all can only have paid its wait outside this, so the two spans are not one budget.
const HANDLER_TIMEOUT_SECONDS: u32 = 3;

/// The `errorType` the platform reports for a run IT ended on a span, rather than one the
/// handler's own exception carries.
const SANDBOX_TIMEDOUT: &str = "Sandbox.Timedout";

/// The package of a function whose MODULE-LEVEL code waits `wait_seconds` before the handler
/// is ever found: the wait is taken outside the handler, so it is the INIT phase's.
fn module_waiting(wait_seconds: u64) -> Vec<u8> {
    package(&format!(
        "import time\n\
         time.sleep({wait_seconds})\n\
         def lambda_handler(event, context):\n\
         \x20   return {{'ok': True}}\n"
    ))
}

/// Create the function `name` from `code`, and wait out the activation the create owes.
async fn create_function(router: &MultiServiceRouter, name: &str, code: Vec<u8>) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": PYTHON_HANDLER,
            "Timeout": HANDLER_TIMEOUT_SECONDS,
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(code) },
        }))
        .expect("the create body serializes"),
        &[],
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

/// `Invoke` the function `name`, as the status, the `X-Amz-Function-Error` signal and the
/// answered document — the three things a client of a synchronous invocation reads.
async fn invoke(router: &MultiServiceRouter, name: &str) -> (u16, Option<String>, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "http://lambda.us-east-1.amazonaws.com{FUNCTIONS_URI}/{name}/invocations"
        ))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/us-east-1/lambda/\
             aws4_request, SignedHeaders=host, Signature=not-verified",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from_static(b"{}"))
        .expect("the test request must build");
    let (parts, body) = router.dispatch(request).await.into_parts();
    let document = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "expected a JSON body, got {error}: {}",
                String::from_utf8_lossy(&body)
            )
        })
    };
    (
        parts.status.as_u16(),
        parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        document,
    )
}

/// The rows: a module that waits not at all, one that waits well inside the phase's span, and
/// one that waits past it — the spans read off the phase's own limit rather than written out,
/// so the table follows the published span. The third column says whether the row spends the
/// whole span.
fn rows() -> Vec<(&'static str, u64, bool)> {
    vec![
        ("a module that waits not at all", 0, false),
        ("a module that waits well inside the phase's span", 1, false),
        (
            "a module that waits past the phase's span",
            INIT_PHASE_LIMIT.as_secs() + 2,
            true,
        ),
    ]
}

#[tokio::test]
async fn a_module_that_waits_past_the_init_span_is_answered_the_platforms_timeout() {
    for (case, module_wait, past_the_limit) in rows() {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let name = format!("init-span-{module_wait}");
        create_function(&router, &name, module_waiting(module_wait)).await;

        let (status, function_error, document) = invoke(&router, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an invocation that RAN is a 200 whatever the run came to, got \
             {status} with {document:?}"
        );
        if past_the_limit {
            assert_eq!(
                function_error.as_deref(),
                Some("Unhandled"),
                "case {case:?}: an initialization that spends the whole span the phase is \
                 allowed is given up on, so the caller is signalled a failure, got \
                 {function_error:?} with {document:?}"
            );
            let error_type = document
                .get("errorType")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                error_type, SANDBOX_TIMEDOUT,
                "case {case:?}: the failure is the platform's own span rather than the \
                 handler's, got {error_type:?} in {document:?}"
            );
        } else {
            assert_eq!(
                function_error, None,
                "case {case:?}: an initialization inside the phase's span is followed by the \
                 handler's own run, got {function_error:?} with {document:?}"
            );
            assert_eq!(
                document,
                json!({ "ok": true }),
                "case {case:?}: the payload is what the handler returned, got {document:?}"
            );
        }
    }
}
