// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHOSE memory ends an invocation: `Invoke` of a handler that allocates
//! nothing while another handler is holding all the memory its own function is configured for.
//!
//! The rule under test: the memory an invocation is answerable for is the memory IT holds.
//! "If your function runs out of memory" is a property of that function's own execution
//! environment — `MemorySize` is "the amount of memory available to the function at runtime" —
//! so a run is ended, and reported as the platform's out-of-memory failure, only when the run
//! itself holds more than its function configures. Memory something ELSE holds — another
//! invocation, an execution environment standing by for a later call, the rest of the emulator
//! — is never charged to it: an invocation that allocated none of it is answered by its
//! handler, because ending it would answer a caller that allocated nothing and would free
//! nothing either.
//!
//! The rows differ along what the rule generalises over: how many memory-hungry runs are in
//! flight beside the invocations that allocate nothing, how many of those invocations there
//! are, and how much memory their own function is configured with. Each row reads BOTH sides,
//! because the rule has two halves: the run that passed its OWN ceiling is stopped, and every
//! run that did not is answered by its handler.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::one_invocation_stream_at_a_time;
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

/// The module the handler of every package below lives in.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The header `Invoke$FunctionError` is bound to.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function here carries, in seconds.
///
/// Generous: the rows are about which run a memory bound belongs to, never about how fast this
/// host runs, and the suites of this target share the emulator's run slots — so a run can sit
/// mid-flight while another suite holds the CPU without any row being answered by a deadline.
const FUNCTION_TIMEOUT_SECONDS: u32 = 25;

/// The smallest `MemorySize` the model allows (`smithy_query(query="shape",
/// shape="MemorySize")`: `@range(min: 128, max: 32768)`), which the hungry function is given so
/// it reaches its own ceiling on a modest allocation.
const SMALLEST_MEMORY_SIZE_MB: u32 = 128;

/// A `MemorySize` well above it, so a row can vary the ceiling of the runs that allocate
/// nothing.
const LARGER_MEMORY_SIZE_MB: u32 = 512;

/// The `errorType` the platform reports for a run it ended at its function's memory ceiling.
const RUNTIME_OUT_OF_MEMORY: &str = "Runtime.OutOfMemory";

/// The `FunctionError` an invocation that failed reports (`Invoke$FunctionError`).
const FUNCTION_ERROR_UNHANDLED: &str = "Unhandled";

/// A handler that holds memory until the platform ends it: it allocates a megabyte at a time
/// and keeps every block, so what stops it is its own function's `MemorySize` and nothing
/// else.
const HOLDS_UNTIL_KILLED: &str = "def lambda_handler(event, context):\n    \
     kept = []\n    \
     while True:\n        \
         kept.append(bytearray(b'x' * (1024 * 1024)))\n";

/// A handler that allocates nothing and answers the caller that sent the event, so each answer
/// is identifiable as that invocation's own run.
const ALLOCATES_NOTHING: &str = "def lambda_handler(event, context):\n    \
     return {'caller': event['caller']}\n";

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

/// Create a function whose code is `source` and whose `MemorySize` is `memory_size` MB, and
/// wait out the provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str, memory_size: u32) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "MemorySize": memory_size,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
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
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `Invoke` on `name` with the event `event`.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// One row: the case, how many runs hold memory until the platform ends them, how many
/// invocations that allocate nothing run beside them, and the `MemorySize` those are
/// configured with.
struct Row {
    case: &'static str,
    hungry: usize,
    calm: usize,
    calm_memory_mb: u32,
}

/// An invocation that allocates nothing is answered by its handler while another invocation is
/// being ended at its own memory ceiling: the memory one run holds is that run's alone.
///
/// Both halves are read on every row, because together they are the rule: the run that passed
/// the ceiling its OWN function configures is stopped and reported as the platform's
/// out-of-memory failure, and every run that passed no ceiling of its own is answered with the
/// value its handler returned — not with a failure of a sandbox that something else filled.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_invocation_is_ended_only_by_the_memory_its_own_run_holds() {
    let _stream = one_invocation_stream_at_a_time().await;
    let rows: &[Row] = &[
        Row {
            case: "one hungry run beside invocations at the smallest MemorySize",
            hungry: 1,
            calm: 3,
            calm_memory_mb: SMALLEST_MEMORY_SIZE_MB,
        },
        Row {
            case: "two hungry runs beside invocations at a larger MemorySize",
            hungry: 2,
            calm: 3,
            calm_memory_mb: LARGER_MEMORY_SIZE_MB,
        },
        Row {
            case: "two hungry runs beside a single invocation",
            hungry: 2,
            calm: 1,
            calm_memory_mb: SMALLEST_MEMORY_SIZE_MB,
        },
    ];
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let hungry_name = "probe-memory-hungry";
    create_function(
        &router,
        hungry_name,
        HOLDS_UNTIL_KILLED,
        SMALLEST_MEMORY_SIZE_MB,
    )
    .await;
    let mut created_calm: Vec<u32> = Vec::new();
    for row in rows {
        let calm_name = calm_function(row.calm_memory_mb);
        if !created_calm.contains(&row.calm_memory_mb) {
            create_function(&router, &calm_name, ALLOCATES_NOTHING, row.calm_memory_mb).await;
            created_calm.push(row.calm_memory_mb);
        }
        // Every invocation of the row is fired at once, each on a clone of the router sharing
        // the one engine and the one service instance, so the runs really do overlap.
        let mut callers = tokio::task::JoinSet::new();
        for index in 0..row.hungry {
            let router = router.clone();
            let name = hungry_name.to_string();
            callers.spawn(async move {
                let answered = invoke(&router, &name, &json!({ "caller": index })).await;
                (true, index, answered)
            });
        }
        for index in 0..row.calm {
            let router = router.clone();
            let name = calm_name.clone();
            callers.spawn(async move {
                let answered = invoke(&router, &name, &json!({ "caller": index })).await;
                (false, index, answered)
            });
        }
        let mut answers: Vec<(bool, usize, Answer)> = Vec::new();
        while let Some(joined) = callers.join_next().await {
            answers.push(joined.expect("an invoking task must not panic"));
        }
        for (hungry, index, answered) in &answers {
            let side = if *hungry { "hungry" } else { "calm" };
            assert_eq!(
                answered.status,
                200,
                "case {}, the {side} invocation {index}: an invocation Lambda admits is \
                 answered with what the run came to, got {} {}",
                row.case,
                answered.status,
                answered.text()
            );
            let failure = answered.header(FUNCTION_ERROR_HEADER);
            let document = answered.text();
            if *hungry {
                assert_eq!(
                    failure.as_deref(),
                    Some(FUNCTION_ERROR_UNHANDLED),
                    "case {}, the {side} invocation {index}: a run that passed its own \
                     MemorySize is reported as a failed invocation, got {failure:?} with \
                     {document}",
                    row.case
                );
                assert!(
                    document.contains(RUNTIME_OUT_OF_MEMORY),
                    "case {}, the {side} invocation {index}: expected the platform's \
                     {RUNTIME_OUT_OF_MEMORY} class, got {document}",
                    row.case
                );
            } else {
                assert_eq!(
                    failure, None,
                    "case {}, the {side} invocation {index}: this run allocated nothing, so no \
                     bound of its own was breached, got {failure:?} with {document}",
                    row.case
                );
                assert_eq!(
                    answered.json(),
                    json!({ "caller": index }),
                    "case {}, the {side} invocation {index}: expected this invocation's own \
                     run, got {document}",
                    row.case
                );
            }
        }
    }
}

/// The function that allocates nothing, at `memory_size` MB: one per `MemorySize` a row needs,
/// because the ceiling a run is judged against is the one its own function configures.
fn calm_function(memory_size: u32) -> String {
    format!("probe-memory-calm-{memory_size}")
}
