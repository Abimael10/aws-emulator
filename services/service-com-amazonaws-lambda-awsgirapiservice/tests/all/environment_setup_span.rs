// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the span a run's own bounds do NOT include: `CreateFunction` then
//! `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router, with the
//! log tail the invocation reports.
//!
//! The rule under test: the two spans a run is judged against measure the RUN's own doing —
//! the function's `Timeout` is "the amount of time that Lambda allows a function to run before
//! stopping it", which is the handler's own phase, and the initialization that precedes it has
//! a span of its own — so the work of STANDING THE EXECUTION ENVIRONMENT UP is charged to
//! neither. An invocation that found no environment standing has one built inside it: the
//! interpreter or the engine is created, its native surface is narrowed and this emulator's own
//! runtime is evaluated, all before a line of the function's code is read. None of that is the
//! function's doing, so an invocation whose handler returns at once is answered with the
//! handler's own value and the handler is told it has its whole `Timeout` — on a host of any
//! speed, and for every `Timeout` a function may carry, the smallest included.
//!
//! The rows differ along what the rule generalises over: the `Timeout` configured (the smallest
//! the model allows and the largest, `Timeout`: "between 1 second and 900 seconds"), the engine
//! whose environment is stood up (both), and whether the handler's own module does work while
//! it is being read — which is the part of the initialization that IS the function's.
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `crate::invocation_init_phase` pins that the `Timeout` bounds the handler's phase and not the
//! import of the package's own module, and that a handler which outlasts its `Timeout` is still
//! stopped; `crate::invocation_log_phases_and_level_filters` pins which records belong to the
//! initialization. What this suite reads is the REPORT of the run the environment was stood up
//! inside: it states that setup as a phase of its own, and the run it happened in was served.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

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

/// The `Runtime` each engine is reached by.
const PYTHON: &str = "python3.12";
/// See [`PYTHON`].
const NODE: &str = "nodejs22.x";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// How much of its `Timeout` a handler may already have spent by the time it reads what it has
/// left, in milliseconds.
///
/// Not an allowance for the environment's setup — that is what the rule is about — but for the
/// few statements of the runtime between the instant the invoke phase begins and the handler's
/// own reading: the context object is built and the handler is called. Generous enough that a
/// loaded host cannot turn those statements into a failure, and far smaller than the setup it
/// must not cover.
const ENTRY_SLACK_MS: i64 = 500;

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 4;

/// The field the REPORT of a run that had an environment stood up inside it states that phase's
/// span in.
const INIT_DURATION_FIELD: &str = "Init Duration:";

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

    /// The log tail this invocation was answered with, decoded.
    fn log_tail(&self) -> String {
        self.header("x-amz-log-result")
            .and_then(|tail| base64::engine::general_purpose::STANDARD.decode(tail).ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
            .unwrap_or_default()
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request, with the headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
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

/// One row: the case, the engine's `Runtime`, the package entries, the handler they name and the
/// `Timeout` the function carries.
struct Row {
    case: &'static str,
    runtime: &'static str,
    entries: &'static [(&'static str, &'static str)],
    handler: &'static str,
    timeout: i64,
}

/// Create the function `row` describes and wait out the provisioning window the create
/// documents.
async fn create_function(router: &MultiServiceRouter, name: &str, row: &Row) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": row.runtime,
            "Handler": row.handler,
            "Timeout": row.timeout,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(row.entries)),
            },
        }))
        .expect("the create body serializes"),
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {:?}: CreateFunction must succeed, got {} {}",
        row.case,
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// Remove the function `name`, so the next row has an environment stood up for its own.
async fn delete_function(router: &MultiServiceRouter, case: &str, name: &str) {
    let deleted = call(router, "DELETE", &format!("{FUNCTIONS_URI}/{name}"), &[], &[]).await;
    assert_eq!(
        deleted.status, 204,
        "case {case:?}: the function must be removable between rows, got {} {}",
        deleted.status,
        deleted.text()
    );
}

/// One `Invoke` request on `name`, asking for the log tail.
async fn invoke_once(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        b"{}",
        &[("x-amz-log-type", "Tail")],
    )
    .await
}

/// `Invoke` on `name`, re-driven while the answer reports one of this process's own HOST BOUNDS
/// instead of a run of the handler ([`is_host_bound`]): the run slots every suite of this target
/// shares, and the memory bound read from the whole process. Neither says anything about the
/// rule.
async fn invoke(router: &MultiServiceRouter, name: &str) -> Answer {
    let mut answered = invoke_once(router, name).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(SLACK).await;
        answered = invoke_once(router, name).await;
    }
    answered
}

/// A python handler that returns at once, reporting what the runtime tells it it has left.
const PYTHON_RETURNS_AT_ONCE: &str =
    "def lambda_handler(event, context):\n    \
         return {'left': context.get_remaining_time_in_millis()}\n";

/// The same handler behind a module that does its own work while it is being read, which is the
/// part of the initialization that belongs to the function.
const PYTHON_WORKS_WHILE_IT_IMPORTS: &str = "import hashlib\n\
     import json\n\
     \n\
     READY = hashlib.sha256(json.dumps({'ready': True}).encode()).hexdigest()\n\
     \n\
     def lambda_handler(event, context):\n    \
         return {'left': context.get_remaining_time_in_millis(),\n            \
                 'ready': bool(READY)}\n";

/// The node handler that reports the same.
const NODE_RETURNS_AT_ONCE: &str = "exports.handler = async (event, context) => ({\n    \
         left: context.getRemainingTimeInMillis(),\n\
     });\n";

/// RULE: the span this emulator spends standing an execution environment up is charged to
/// neither of a run's own spans, so an invocation that had one built inside it is answered with
/// its handler's value and its handler is told it has the whole `Timeout`.
///
/// One test and one stream of invocations, the way the sibling suites that RUN handlers are
/// built: this emulator serves a bounded number of runs at once for the whole process. A row
/// whose answer reports a bound this binary shares says nothing about the rule and is left out
/// of the reading, and a test that read no row at all fails rather than passing silently.
#[tokio::test]
async fn the_setup_of_an_environment_answers_no_invocation_served_in_it() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let rows: &[Row] = &[
        // The smallest `Timeout` the model allows (`@range(min: 1)`): the row an environment's
        // setup charged to the `Timeout` leaves nothing of.
        Row {
            case: "a python handler with the smallest Timeout the model allows",
            runtime: PYTHON,
            entries: &[("lambda_function.py", PYTHON_RETURNS_AT_ONCE)],
            handler: "lambda_function.lambda_handler",
            timeout: 1,
        },
        // The same engine with the longest `Timeout` the model allows, and a module that works
        // while it is read: the setup before the handler is the longest this table has.
        Row {
            case: "a python handler behind a module that works while it is read",
            runtime: PYTHON,
            entries: &[("lambda_function.py", PYTHON_WORKS_WHILE_IT_IMPORTS)],
            handler: "lambda_function.lambda_handler",
            timeout: 900,
        },
        // The other engine, whose environment this emulator stands up its own way.
        Row {
            case: "a node handler with the smallest Timeout the model allows",
            runtime: NODE,
            entries: &[("index.js", NODE_RETURNS_AT_ONCE)],
            handler: "index.handler",
            timeout: 1,
        },
        Row {
            case: "a node handler with the longest Timeout the model allows",
            runtime: NODE,
            entries: &[("index.js", NODE_RETURNS_AT_ONCE)],
            handler: "index.handler",
            timeout: 900,
        },
    ];
    let name = "probe-environment-setup";
    let mut read = 0_usize;
    for row in rows {
        let case = row.case;
        create_function(&router, name, row).await;
        let answered = invoke(&router, name).await;
        if is_host_bound(answered.status, &answered.text()) {
            delete_function(&router, case, name).await;
            continue;
        }
        read += 1;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a handler that returns at once must be answered, got {} {}",
            answered.status,
            answered.text()
        );
        let failure = answered.header("x-amz-function-error");
        assert_eq!(
            failure, None,
            "case {case:?}: the handler returns at once and the setup of its environment is \
             charged to neither of the run's spans, so the invocation carries no function \
             error, got {failure:?} with {}",
            answered.text()
        );
        // The environment really was stood up inside THIS invocation: its own report states
        // that phase, which is the span the two assertions above say answers nothing.
        let tail = answered.log_tail();
        assert!(
            tail.contains(INIT_DURATION_FIELD),
            "case {case:?}: expected the report of a run that had its environment stood up \
             inside it to state {INIT_DURATION_FIELD:?}, got {tail:?}"
        );
        // And the handler was given its whole `Timeout`: what it is told it has left is the
        // span its own phase was given and never one the setup had spent part of.
        let allowed_ms = row.timeout.saturating_mul(1_000);
        let document = answered.json();
        let left = document.get("left").and_then(Value::as_i64);
        assert!(
            left.is_some_and(|left| left > allowed_ms - ENTRY_SLACK_MS && left <= allowed_ms),
            "case {case:?}: expected the handler to be told it has between {} and {allowed_ms} \
             ms of its own run left, got {left:?} in {document}",
            allowed_ms - ENTRY_SLACK_MS
        );
        delete_function(&router, case, name).await;
    }
    assert!(
        read > 0,
        "no row of the table was answered by a run at all: every one was ended by this \
         process's own host bounds, so the rule was never observed"
    );
}
