// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EVENT LOOP a `python3.x` function's run can drive, and the
//! in-process connected pair it is built on: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a `python3.x` runtime presents the standard library of the runtime it
//! advertises, so a handler that runs a coroutine through `asyncio.run` gets its answer — the
//! event loop opens its own wakeup pair inside this process, the waiting interface reports
//! readiness on the descriptors of that pair, and the loop runs its callbacks and its timers.
//! Nothing of the host is involved: a connected pair is two byte queues in this process.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: how many coroutines the loop is given, whether they await a timer or
//! another coroutine's result, whether the awaited work finishes inside the wait it was given,
//! how many times one invocation drives a loop, and which call is made on the pair itself.

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

/// The `Timeout` every function this suite creates carries, in seconds. Nothing here is about a
/// deadline of the FUNCTION, so it is given generous room.
const FUNCTION_TIMEOUT_SECONDS: i32 = 600;

/// How many times a case re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

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

/// Create a `python3.12` function whose package holds `source` as `lambda_function.py`, and
/// wait out the provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", source)])),
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

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole test
/// binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
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
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// What one case of the probe answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
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
        "case {case}: the handler's module imports cleanly and the handler catches each refusal \
         itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END: ONE real `CreateFunction` whose handler module BODY
/// imports `asyncio` and real `Invoke` requests over the router, which the tables below read.
#[tokio::test]
async fn the_python_runtime_runs_a_handlers_coroutines_on_its_own_event_loop() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-asyncio-event-loop";
    create_function(&router, probe, PROBE_SOURCE).await;
    let mut read = a_coroutine_run_on_the_loop_answers_what_it_computed(&router, probe).await;
    read += the_loop_runs_its_timers_and_its_tasks(&router, probe).await;
    read += a_connected_pair_carries_bytes_between_its_two_ends(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the rule \
         was read"
    );
}

/// What `asyncio.run` answers, over a table of coroutine programs: the recorded symptom is a
/// handler whose `asyncio.run` never gets as far as running anything, and these are what the
/// loop computes once it does.
///
/// The rows differ along how many coroutines the loop is given and how they are driven: one
/// coroutine, several awaited one after another, several gathered at once, one awaiting
/// another's result, and two loops driven one after the other in a single invocation.
///
/// Answers how many cases a run answered.
async fn a_coroutine_run_on_the_loop_answers_what_it_computed(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (the case, the program the event names, what the loop computes for it)
    let cases: &[(&str, &str, Value)] = &[
        ("one coroutine", "one", json!(4)),
        ("coroutines awaited one after another", "sequence", json!([0, 1, 4, 9])),
        ("coroutines gathered at once", "gather", json!([0, 1, 4, 9, 16])),
        ("a coroutine awaiting another's result", "nested", json!(30)),
        ("two loops in one invocation", "twice", json!([4, 4])),
    ];
    let mut read = 0;
    for (case, program, expected) in cases {
        let event = json!({"case": "coroutines", "program": program});
        let Some(document) = answered(router, probe, case, &event).await else {
            continue;
        };
        read += 1;
        let ran = document.get("ran").cloned().unwrap_or(Value::Null);
        assert_eq!(
            ran, *expected,
            "case {case}: expected the loop to compute {expected}, got {ran} in {document}"
        );
    }
    read
}

/// The loop's own machinery: a timer it has to wait out, a task it has to schedule, a queue two
/// coroutines hand values through, and the refusal a wait that does not finish in time carries.
///
/// Answers how many cases a run answered.
async fn the_loop_runs_its_timers_and_its_tasks(router: &MultiServiceRouter, probe: &str) -> usize {
    let case = "the loop's timers and tasks";
    let Some(document) = answered(router, probe, case, &json!({"case": "machinery"})).await else {
        return 0;
    };
    for (member, expected) in [
        // A timer the loop waited out, after which the coroutine continued.
        ("slept", json!(true)),
        // A task the loop scheduled, which ran and answered.
        ("task", json!(81)),
        // The values two coroutines handed through a queue, in the order they were put in.
        ("queued", json!([1, 2, 3])),
        // A wait that did not finish inside the span it was given, which is the standard
        // library's own `TimeoutError`.
        ("timed_out", json!("TimeoutError")),
        // ... and one that did.
        ("in_time", json!("done")),
        // The loop is closed afterwards, and a second one can be opened.
        ("closed", json!(true)),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    1
}

/// The CONNECTED PAIR the loop is built on, driven directly: the bytes one end writes are the
/// bytes the other reads, the waiting interface reports each end's readiness, and a read with
/// nothing to read answers the way a non-blocking read does.
///
/// Source of each expected value: the socket interface's own contract — `socketpair` answers
/// two connected stream sockets, `recv` on a non-blocking socket with nothing to read raises
/// `BlockingIOError`, a closed peer leaves the end-of-file an empty read reports, and
/// `select` answers the objects it was given.
///
/// Answers how many cases a run answered.
async fn a_connected_pair_carries_bytes_between_its_two_ends(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "a connected pair in this process";
    let Some(document) = answered(router, probe, case, &json!({"case": "pair"})).await else {
        return 0;
    };
    for (member, expected) in [
        // Each end holds a descriptor of its own, past the three streams an environment has.
        ("descriptors", json!(true)),
        // The bytes one end writes are read off the other, in order, and a read takes at most
        // what it asked for.
        ("carried", json!("hello")),
        ("in_order", json!(["one", "two"])),
        ("partial", json!(["he", "llo"])),
        // Readiness is reported on the descriptors the run really holds.
        ("ready_before", json!(false)),
        ("ready_after", json!(true)),
        ("writable", json!(true)),
        // A read with nothing standing is a read nothing can satisfy — an invocation runs on
        // one thread, so the only code that could write to that end is the code reading it —
        // and it answers at once rather than holding the thread: a non-blocking end the way a
        // non-blocking read does, and a blocking one as the wait it cannot end.
        ("empty", json!("BlockingIOError")),
        ("blocked", json!("TimeoutError")),
        // ... and a peer that closed leaves the end-of-file a read reports as no bytes.
        ("eof", json!("")),
        // A descriptor this runtime does not hold is answered the way one that is not open is.
        ("unheld", json!("OSError")),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    1
}

/// The probe every table drives: its module BODY imports `asyncio`, `select` and `socket` —
/// the recorded symptom is a handler that cannot get past one of those — and its handler
/// answers the surface the event names.
const PROBE_SOURCE: &str = r#"
import asyncio
import select
import socket

# The modules above were imported by this module's BODY, so a handler that runs at all is a
# clean import of each.
_IMPORTED = True

# How long a wait that is EXPECTED to be cut off is given, in seconds, and how long the work it
# waits on takes. The work is far longer than the wait, so which of the two ends the wait does
# not depend on how fast this host runs.
_SHORT_WAIT = 1.0
_LONG_WORK = 600.0


async def _squared(value):
    await asyncio.sleep(0)
    return value * value


async def _total(values):
    gathered = await asyncio.gather(*[_squared(value) for value in values])
    return sum(gathered)


def _coroutines(event):
    program = event.get("program")
    if program == "one":
        return {"ran": asyncio.run(_squared(2))}
    if program == "sequence":

        async def main():
            answers = []
            for value in range(4):
                answers.append(await _squared(value))
            return answers

        return {"ran": asyncio.run(main())}
    if program == "gather":

        async def main():
            return await asyncio.gather(*[_squared(value) for value in range(5)])

        return {"ran": asyncio.run(main())}
    if program == "nested":
        return {"ran": asyncio.run(_total([1, 2, 3, 4]))}
    if program == "twice":
        return {"ran": [asyncio.run(_squared(2)), asyncio.run(_squared(2))]}
    raise AssertionError("the event named no program: %r" % (program,))


def _machinery(event):
    async def main():
        answers = {}
        await asyncio.sleep(0.01)
        answers["slept"] = True
        task = asyncio.ensure_future(_squared(9))
        answers["task"] = await task
        queue = asyncio.Queue()

        async def produce():
            for value in (1, 2, 3):
                await queue.put(value)

        async def consume():
            taken = []
            for _ in range(3):
                taken.append(await queue.get())
            return taken

        producer = asyncio.ensure_future(produce())
        answers["queued"] = await consume()
        await producer
        try:
            await asyncio.wait_for(asyncio.sleep(_LONG_WORK), timeout=_SHORT_WAIT)
            answers["timed_out"] = ""
        except asyncio.TimeoutError as error:
            answers["timed_out"] = type(error).__name__
        answers["in_time"] = await asyncio.wait_for(_done(), timeout=_LONG_WORK)
        return answers

    async def _done():
        await asyncio.sleep(0)
        return "done"

    loop = asyncio.new_event_loop()
    try:
        answers = loop.run_until_complete(main())
    finally:
        loop.close()
    answers["closed"] = loop.is_closed()
    # A second loop opens after the first one closed, which is what every invocation of a
    # handler that calls `asyncio.run` relies on.
    answers["ran_again"] = asyncio.run(_squared(3))
    return answers


def _pair(event):
    answers = {}
    first, second = socket.socketpair()
    try:
        answers["descriptors"] = first.fileno() > 2 and second.fileno() > 2
        first.sendall(b"hello")
        answers["carried"] = second.recv(16).decode("utf-8")
        first.sendall(b"one")
        first.sendall(b"two")
        answers["in_order"] = [
            second.recv(3).decode("utf-8"),
            second.recv(3).decode("utf-8"),
        ]
        first.sendall(b"hello")
        answers["partial"] = [
            second.recv(2).decode("utf-8"),
            second.recv(8).decode("utf-8"),
        ]
        answers["ready_before"] = bool(select.select([second], [], [], 0)[0])
        first.sendall(b"ready")
        answers["ready_after"] = select.select([second], [], [], 0)[0] == [second]
        answers["writable"] = select.select([], [first], [], 0)[1] == [first]
        second.recv(16)
        # A BLOCKING end with nothing standing: nothing can write to it while this thread is
        # the one reading it, so the read answers rather than holding the invocation.
        try:
            second.recv(16)
            answers["blocked"] = ""
        except TimeoutError as error:
            answers["blocked"] = type(error).__name__
        second.setblocking(False)
        try:
            second.recv(16)
            answers["empty"] = ""
        except BlockingIOError as error:
            answers["empty"] = type(error).__name__
        first.close()
        answers["eof"] = second.recv(16).decode("utf-8")
    finally:
        first.close()
        second.close()
    # A descriptor this runtime does not hold is answered the way a descriptor that is not
    # open is, which is what every other descriptor of a function is. The number is far above
    # any this environment's own table has handed out.
    try:
        select.select([4096], [], [], 0)
        answers["unheld"] = ""
    except OSError as error:
        answers["unheld"] = type(error).__name__
    return answers


_CASES = {
    "coroutines": _coroutines,
    "machinery": _machinery,
    "pair": _pair,
}


def handler(event, context):
    case = (event or {}).get("case")
    answer = _CASES.get(case)
    if answer is None:
        raise AssertionError("the event named no case: %r" % (case,))
    return answer(event or {})
"#;
