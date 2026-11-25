// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an invocation that arrives INSIDE a function's provisioning window:
//! the span a freshly created function reports `State=Pending` for.
//!
//! The rule under test: that window refuses the invocations that RUN the function for their
//! caller — "When you create a function, Lambda provisions an instance of the function and
//! its supporting resources. [...] During this time, you can't invoke or modify the function"
//! (`CreateFunction`) — and refuses nothing about an ASYNCHRONOUS invocation, which runs
//! nothing for its caller: "For asynchronous invocation, Lambda adds events to a queue before
//! sending them to your function" (`Invoke`), and the answer promises that queueing alone ("a
//! success response (202 Accepted) indicates that the request is queued for invocation",
//! `InvokeAsyncResponse`). So an event accepted inside the window is queued and SENT to the
//! function once it is invocable, while a synchronous invocation inside it is the modelled
//! `ResourceConflictException` (HTTP 409) naming the state the function is in.
//!
//! The rows differ along what the rule generalises over — the entry point an invocation
//! arrives through and the invocation type it names: `Invoke` defaulting to
//! `RequestResponse`, `Invoke` naming it, `Invoke` with `DryRun`,
//! `InvokeWithResponseStream`, `Invoke` with `Event`, and the deprecated `InvokeAsync`, whose
//! every invocation is asynchronous. Each row drives real HTTP requests through the router
//! against a function of its own, so one row's event cannot be read as another's.
//!
//! What an accepted event CAME TO is read out of the service's own state, the way the other
//! asynchronous-delivery suites read it, because no modelled Lambda operation reports an
//! accepted asynchronous invocation back out.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, AsyncInvocationRecord, ComAmazonawsLambdaAwsgirapiserviceState,
};
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The `State` a function being provisioned reports, from the model's `State` enum.
const STATE_PENDING: &str = "Pending";

/// The modelled error a refused invocation carries, and its documented status.
const RESOURCE_CONFLICT: &str = "ResourceConflictException";
/// See [`RESOURCE_CONFLICT`] (`smithy_query(query="operation",operation="Invoke")`).
const CONFLICT_STATUS: u16 = 409;

/// The status an accepted asynchronous invocation answers: "a success response (202
/// Accepted) indicates that the request is queued for invocation".
const ACCEPTED_STATUS: u16 = 202;

/// How long this suite waits for an accepted event to be delivered and settle. Generous: the
/// subject is THAT the event reaches the function once it is invocable, never how quickly
/// this host gets there.
const SETTLE_BUDGET: Duration = Duration::from_secs(120);

/// How often it re-reads the queue while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The handler the function runs: it echoes the event's own marker back, so the entry's
/// outcome is evidence the guest code ran with the event that was queued for it.
const HANDLER_SOURCE: &str = "def lambda_handler(event, context):\n\
                              \x20   return {'seen': event['case']}\n";

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The error code the answer carries, in either of the places this protocol puts it.
    fn error_code(&self) -> String {
        self.headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                self.json()
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.text())
    }

    fn message(&self) -> String {
        let document = self.json();
        document
            .get("Message")
            .or_else(|| document.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
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

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create one function from a .zip package and answer nothing: the caller invokes it at once,
/// INSIDE the provisioning window the create opens, which is the window under test.
async fn create_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 5,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
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
}

/// The `State` `GetFunctionConfiguration` reports for `name`.
async fn reported_state(router: &MultiServiceRouter, name: &str) -> String {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetFunctionConfiguration {name:?} must succeed, got {} {}",
        read.status,
        read.text()
    );
    read.json()
        .get("State")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The queue entries Lambda holds for `name`, in queue order, and the instant the clock owes
/// that function's move to `Active`: read out of the service's own state because no modelled
/// operation reports either.
fn queued(
    engine: &Arc<engine::InMemoryEngine>,
    name: &str,
) -> (Vec<AsyncInvocationRecord>, Option<SystemTime>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let store = state.lock();
    let Some(function) = store.functions.get(name) else {
        return (Vec::new(), None);
    };
    (
        function.async_invocations.iter().cloned().collect(),
        Some(function.active_due),
    )
}

/// How an invocation arrives: which entry point, and which invocation type it names.
#[derive(Clone, Copy)]
enum Arrival {
    /// `Invoke` (`POST /2015-03-31/functions/{FunctionName}/invocations`), naming
    /// `InvocationType` when `Some` and leaving the member — and so its documented default,
    /// `RequestResponse` — to the service when `None`.
    Invoke(Option<&'static str>),
    /// The deprecated `InvokeAsync` (`POST
    /// /2014-11-13/functions/{FunctionName}/invoke-async`), whose every invocation is
    /// asynchronous.
    InvokeAsync,
    /// `InvokeWithResponseStream` (`POST
    /// /2021-11-15/functions/{FunctionName}/response-streaming-invocations`).
    ResponseStream,
}

impl Arrival {
    async fn send(self, router: &MultiServiceRouter, function: &str, event: &str) -> Answer {
        match self {
            Self::Invoke(invocation_type) => {
                let headers = invocation_type
                    .map(|kind| vec![("x-amz-invocation-type", kind)])
                    .unwrap_or_default();
                call(
                    router,
                    "POST",
                    &format!("{FUNCTIONS_URI}/{function}/invocations"),
                    event.as_bytes(),
                    &headers,
                )
                .await
            }
            Self::InvokeAsync => {
                call(
                    router,
                    "POST",
                    &format!("/2014-11-13/functions/{function}/invoke-async"),
                    event.as_bytes(),
                    &[],
                )
                .await
            }
            Self::ResponseStream => {
                call(
                    router,
                    "POST",
                    &format!("/2021-11-15/functions/{function}/response-streaming-invocations"),
                    event.as_bytes(),
                    &[],
                )
                .await
            }
        }
    }
}

/// One row: how the invocation arrives, and whether the window refuses it.
struct Row {
    case: &'static str,
    function: &'static str,
    arrival: Arrival,
    /// Whether this invocation RUNS the function for its caller, which is what the
    /// provisioning window refuses.
    synchronous: bool,
}

/// The rows: every entry point an invocation reaches a function through, on both sides of the
/// rule.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "Invoke naming no invocation type (RequestResponse by default)",
            function: "window-default",
            arrival: Arrival::Invoke(None),
            synchronous: true,
        },
        Row {
            case: "Invoke with InvocationType=RequestResponse",
            function: "window-request-response",
            arrival: Arrival::Invoke(Some("RequestResponse")),
            synchronous: true,
        },
        Row {
            case: "Invoke with InvocationType=DryRun",
            function: "window-dry-run",
            arrival: Arrival::Invoke(Some("DryRun")),
            synchronous: true,
        },
        Row {
            case: "InvokeWithResponseStream",
            function: "window-response-stream",
            arrival: Arrival::ResponseStream,
            synchronous: true,
        },
        Row {
            case: "Invoke with InvocationType=Event",
            function: "window-event",
            arrival: Arrival::Invoke(Some("Event")),
            synchronous: false,
        },
        Row {
            case: "InvokeAsync",
            function: "window-invoke-async",
            arrival: Arrival::InvokeAsync,
            synchronous: false,
        },
    ]
}

/// The whole rule, row by row: a synchronous invocation inside the provisioning window is the
/// modelled 409 naming the state the function is in, and an asynchronous one is accepted,
/// queued, and delivered to the function once it is invocable.
#[tokio::test]
async fn the_provisioning_window_refuses_only_what_runs_the_function() {
    for row in rows() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);
        let case = row.case;
        let function = row.function;
        let event = format!(r#"{{"case":"{function}"}}"#);

        // Nothing is awaited between the create and the invocation but the read that pins the
        // window OPEN, so the invocation below arrives inside it.
        create_function(&router, function).await;
        let state = reported_state(&router, function).await;
        assert_eq!(
            state, STATE_PENDING,
            "case {case:?}: the invocation must arrive while the function is still being \
             provisioned, but it reported {state:?}"
        );

        let answered = row.arrival.send(&router, function, &event).await;
        if row.synchronous {
            assert_eq!(
                answered.status,
                CONFLICT_STATUS,
                "case {case:?}: expected {CONFLICT_STATUS} for an invocation that runs the \
                 function inside its provisioning window, got {} {}",
                answered.status,
                answered.text()
            );
            let code = answered.error_code();
            assert!(
                code.contains(RESOURCE_CONFLICT),
                "case {case:?}: expected {RESOURCE_CONFLICT}, got {code:?}"
            );
            assert_eq!(
                answered.message(),
                format!(
                    "The operation cannot be performed at this time. The function is \
                     currently in the following state: {state}"
                ),
                "case {case:?}: expected the refusal to name the state the function is in, \
                 got {:?}",
                answered.message()
            );
            let (held, _) = queued(&engine, function);
            assert!(
                held.is_empty(),
                "case {case:?}: a refused invocation must queue no event, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            answered.status,
            ACCEPTED_STATUS,
            "case {case:?}: expected {ACCEPTED_STATUS} for an asynchronous invocation \
             accepted inside the provisioning window, got {} {}",
            answered.status,
            answered.text()
        );

        // The event is on the queue, and Lambda sends it to the function once the function is
        // invocable: its first attempt is due no earlier than the instant the clock owes the
        // move out of the provisioning window.
        let (held, active_due) = queued(&engine, function);
        let [entry] = &held[..] else {
            panic!("case {case:?}: expected exactly one queued event, got {held:?}")
        };
        let active_due = active_due.expect("the created function is held");
        assert!(
            entry.attempt_due >= active_due,
            "case {case:?}: expected the event's first delivery attempt not before the \
             function is invocable ({active_due:?}), got {:?}",
            entry.attempt_due
        );
        assert!(
            entry.payload == event.as_bytes(),
            "case {case:?}: expected the entry to hold the event as sent, got {:?}",
            String::from_utf8_lossy(&entry.payload)
        );

        // And the function RUNS it: waited for by polling the queue, never slept through.
        let deadline = Instant::now() + SETTLE_BUDGET;
        let settled = loop {
            let (held, _) = queued(&engine, function);
            if let [entry] = &held[..] {
                if !matches!(
                    entry.outcome,
                    AsyncInvocationOutcome::Queued | AsyncInvocationOutcome::Delivering
                ) {
                    break entry.clone();
                }
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the accepted event must reach the function within \
                 {SETTLE_BUDGET:?}, got {held:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert!(
            matches!(
                settled.outcome,
                AsyncInvocationOutcome::Delivered {
                    function_error: None
                }
            ),
            "case {case:?}: expected the handler to have run with the event and returned, \
             got {:?}",
            settled.outcome
        );
    }
}
