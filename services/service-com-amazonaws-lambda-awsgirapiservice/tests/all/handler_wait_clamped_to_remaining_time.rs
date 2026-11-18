// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a WAIT a handler asks for: `CreateFunction` then `Invoke`
//! (`POST /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The RULE asserted here, in one sentence: a wait a handler asks for is spent out of what
//! the RUN has left and never past it — the function's `Timeout` is "the amount of time (in
//! seconds) that Lambda allows a function to run before stopping it"
//! (`smithy_query(query="shape", shape="Timeout")`, `@range(min: 1, max: 5400)`) — so a wait
//! inside that span lets the handler go on and return its own answer, a wait past it is ended
//! by the run's own `Timeout` rather than by the span the guest named, and a value that is not
//! a finite, positive number of seconds is no wait at all.
//!
//! So what a wait costs is decided by the span the RUN is allowed and not by how long the
//! handler asked for: the rows differ along what the rule generalises over — the span asked for
//! (none, a fraction of the run's span, a negative span, a span that is not a number, an
//! infinite span, a span well past the run's own, and spans at and beyond the longest one call
//! may ask for) and the `Timeout` the run is given, on both sides of the span this emulator
//! really spends on a handler's wait.
//!
//! What the TIMEOUT DOCUMENT itself looks like — the class, the signal and the message, for
//! each runtime — belongs to `invoke_function_error_document`; what is read here is which SPAN
//! bounded the wait, which is why the rows that time out assert the message states the
//! function's own `Timeout` rather than the span the guest named.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::make_activation_due;
use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `Runtime` the handler below runs on.
const RUNTIME: &str = "python3.12";

/// The handler the package below carries.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The error class a run the sandbox ended for outlasting its `Timeout` reports.
const TIMED_OUT: &str = "Sandbox.Timedout";

/// A handler that asks to wait for as long as the event names and then answers.
///
/// The span arrives as TEXT, so a row can ask for one that JSON itself cannot carry: a value
/// that is not a number, and an infinite one.
const ASKS_TO_WAIT: &str = "import time\n\
     \n\
     def lambda_handler(event, context):\n    \
         time.sleep(float(event['asked']))\n    \
         return {'asked': event['asked'], 'returned': True}\n";

/// How many times a row re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run of the handler.
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

    fn member(&self, name: &str) -> Option<String> {
        self.json()
            .get(name)
            .and_then(Value::as_str)
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

/// Create the function `name` with `timeout` seconds to run in, and bring it to `Active`.
///
/// The activation window is brought FORWARD rather than waited out ([`make_activation_due`]):
/// the transition is one the clock owes, so this suite waits for no span of its own. The only
/// waiting here is the RUN's, inside the emulator: a row that times out is a handler really
/// asking to wait, bounded by the `Timeout` its own row names.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
    timeout: i64,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": timeout,
            "Code": { "ZipFile": base64_zip(ASKS_TO_WAIT) },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    make_activation_due(engine);
}

/// The deployment package holding the handler above, base64 the way restJson1 carries a blob.
fn base64_zip(source: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(package(source))
}

/// `Invoke` the function `name`, asking its handler to wait for `asked`, re-driven while the
/// answer reports one of this process's own host bounds instead of a run ([`is_host_bound`]).
async fn invoke(router: &MultiServiceRouter, name: &str, asked: &str) -> Answer {
    let mut answered = invoke_once(router, name, asked).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, asked).await;
    }
    answered
}

/// One `Invoke` request.
async fn invoke_once(router: &MultiServiceRouter, name: &str, asked: &str) -> Answer {
    let event = serde_json::to_vec(&json!({ "asked": asked })).expect("the event serializes");
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &event,
    )
    .await
}

/// What a row expects of the run its wait was asked inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Expected {
    /// The wait is spent inside what the run has left, so the handler goes on and answers.
    Answered,
    /// The wait is past what the run has left, so the run is ended by its own `Timeout`.
    TimedOut,
}

/// One row: what it is about, the span the handler asks to wait for, the `Timeout` the run it
/// is asked inside is given, and what that run must come to.
struct Row {
    case: &'static str,
    asked: &'static str,
    timeout: i64,
    expected: Expected,
}

/// The `Timeout` the rows that ANSWER a wait of their own are given: far above the span any of
/// them asks to wait for, so a row that answers does so because its wait fitted and not because
/// this host happened to be quick.
const GENEROUS_TIMEOUT: i64 = 10;

/// The `Timeout` the rows that TIME OUT are given: the smallest the model allows
/// (`@range(min: 1)`), which is the span those rows are about, so no row shares one budget
/// with a row expecting the other outcome.
const TIGHT_TIMEOUT: i64 = 1;

/// A `Timeout` well above the span this emulator really spends on a handler's wait, for the
/// row whose handler asks to wait for MINUTES: "the amount of time that Lambda allows a
/// function to run before stopping it" is the span the wait is spent out of, so a wait of
/// minutes inside it is one the handler goes on past.
const MINUTES_TIMEOUT: i64 = 120;

/// A `Timeout` above that same span, for the row whose wait is past it: what such a run is
/// reported to have run past is this span, and not the one the host really waited out.
const ABOVE_THE_HOST_SPAN_TIMEOUT: i64 = 45;

/// The whole rule, driven END TO END: every row is a real `CreateFunction` followed by a real
/// `Invoke` over the router.
///
/// One test, and one stream of invocations, for the reason the sibling suites that run
/// handlers are ([`one_invocation_stream_at_a_time`]): this emulator serves a bounded number
/// of runs at once for the whole process, and this target runs its suites in parallel.
#[tokio::test]
async fn a_wait_a_handler_asks_for_is_spent_out_of_what_the_run_has_left() {
    let _stream = one_invocation_stream_at_a_time().await;
    // (case, the span asked for, what the run must come to). The spans that are no wait at all
    // are the ones `state::execution`'s clamp reads as no span: a value that is not finite, and
    // one that is not above zero.
    let rows: &[Row] = &[
        Row {
            case: "no wait asked for at all",
            asked: "0",
            timeout: GENEROUS_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "a fraction of the span the run has",
            asked: "0.25",
            timeout: GENEROUS_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "a negative span, which is no wait",
            asked: "-1",
            timeout: GENEROUS_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "a span that is not a number, which is no wait",
            asked: "nan",
            timeout: GENEROUS_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "an infinite span, which is no wait either",
            asked: "inf",
            timeout: GENEROUS_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "a wait of minutes, inside a Timeout that allows it",
            asked: "90",
            timeout: MINUTES_TIMEOUT,
            expected: Expected::Answered,
        },
        Row {
            case: "a span well past the one the run has",
            asked: "30",
            timeout: TIGHT_TIMEOUT,
            expected: Expected::TimedOut,
        },
        Row {
            case: "a span past a Timeout of its own above the span a wait really takes",
            asked: "300",
            timeout: ABOVE_THE_HOST_SPAN_TIMEOUT,
            expected: Expected::TimedOut,
        },
        Row {
            case: "the longest span one call may ask for",
            asked: "3600",
            timeout: TIGHT_TIMEOUT,
            expected: Expected::TimedOut,
        },
        Row {
            case: "a span past even that",
            asked: "100000",
            timeout: TIGHT_TIMEOUT,
            expected: Expected::TimedOut,
        },
    ];
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    // One function per `Timeout` a row names, so a row's wait is spent out of the span its own
    // row is about.
    let mut given: Vec<i64> = Vec::new();
    for row in rows {
        if given.contains(&row.timeout) {
            continue;
        }
        given.push(row.timeout);
        given_function(&router, &engine, &probe_name(row.timeout), row.timeout).await;
    }
    // How many rows of each outcome a run really answered: a row this process's own host
    // bounds ended says nothing about the rule, and the rule is only observed once BOTH sides
    // of it have been — a wait that fitted and a wait that did not.
    let mut answered_rows = 0_usize;
    let mut timed_out_rows = 0_usize;
    for row in rows {
        let answered = invoke(&router, &probe_name(row.timeout), row.asked).await;
        if is_host_bound(answered.status, &answered.text()) {
            continue;
        }
        match row.expected {
            Expected::Answered => answered_rows += 1,
            Expected::TimedOut => timed_out_rows += 1,
        }
        assert_eq!(
            answered.status,
            200,
            "case {}: a run of the handler is answered with a 200 whatever it came to, got {} {}",
            row.case,
            answered.status,
            answered.text()
        );
        match row.expected {
            Expected::Answered => assert_answered(row, &answered),
            Expected::TimedOut => assert_timed_out(row, row.timeout, &answered),
        }
    }
    assert!(
        answered_rows > 0 && timed_out_rows > 0,
        "both sides of the rule have to be observed by a run: {answered_rows} row(s) whose wait \
         fitted and {timed_out_rows} row(s) whose wait was past the run's span were answered by \
         one, the rest having been ended by this process's own host bounds"
    );
}

/// The name of the function a row whose `Timeout` is `timeout` is invoked on.
fn probe_name(timeout: i64) -> String {
    format!("probe-wait-{timeout}")
}

/// The rule read off a row whose wait fitted: the handler went on past its wait and its own
/// value is the answer.
fn assert_answered(row: &Row, answered: &Answer) {
    let failure = answered.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {}: the handler asked to wait for {:?} and goes on to return, so the invocation \
         carries no function error, got {failure:?} with {}",
        row.case,
        row.asked,
        answered.text()
    );
    assert_eq!(
        answered.json(),
        json!({ "asked": row.asked, "returned": true }),
        "case {}: expected the handler's own answer after its wait, got {}",
        row.case,
        answered.text()
    );
}

/// The rule read off a row whose wait was past what the run had left: the run is ended by the
/// function's own `Timeout`, and the message states that span rather than the one the guest
/// asked for.
fn assert_timed_out(row: &Row, timeout: i64, answered: &Answer) {
    assert_eq!(
        answered.header("x-amz-function-error").as_deref(),
        Some("Unhandled"),
        "case {}: expected the Unhandled signal, got {:?} with {}",
        row.case,
        answered.header("x-amz-function-error"),
        answered.text()
    );
    assert_eq!(
        answered.member("errorType").as_deref(),
        Some(TIMED_OUT),
        "case {}: expected the sandbox timeout class, got {:?} with {}",
        row.case,
        answered.member("errorType"),
        answered.text()
    );
    let message = answered.member("errorMessage").unwrap_or_default();
    let stated = format!("Task timed out after {:.2} seconds", timeout as f64);
    assert!(
        message.ends_with(&stated),
        "case {}: the wait is clamped to the run's own span, so the message must state {stated:?} \
         and not the {:?} seconds asked for, got {message:?}",
        row.case,
        row.asked
    );
}
