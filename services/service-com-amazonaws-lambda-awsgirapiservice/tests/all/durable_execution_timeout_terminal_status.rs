// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TERMINAL STATUS of a durable execution whose run spends more time
//! than the execution is allowed — Lambda, Amazon EventBridge and Amazon SQS on ONE engine,
//! with the status-change event read back where a client reads it: off the queue the matching
//! rule targets.
//!
//! The RULE pinned here: `DurableConfig$ExecutionTimeout` is "the maximum time (in seconds)
//! that a durable execution can run before timing out. This timeout applies to the entire
//! durable execution, not individual function invocations"
//! (`smithy_query(query="shape", shape="DurableConfig")`), so it is the ONE span that ends an
//! execution: an execution still working when it falls due ends `TIMED_OUT` — the distinct
//! `ExecutionStatus` of an execution the service itself ended, beside `RUNNING | SUCCEEDED |
//! FAILED | STOPPED` (`smithy_query(query="shape", shape="ExecutionStatus")`) — with an
//! `EndTimestamp` no earlier than its `StartTimestamp` plus that timeout, while an execution
//! that finishes inside it ends on what its handler produced, however long one invocation of
//! that handler is allowed. `FAILED` is the status of an execution whose handler RAISED, so a
//! consumer branching on `TIMED_OUT` can tell a timed-out workflow from a crashed one.
//!
//! The rows span the class rather than one case: they differ along the `ExecutionTimeout`
//! (including the model's own floor, `@range(min: 1, max: 31622400)`), along whether that
//! timeout is SHORTER or LONGER than the function `Timeout` one invocation of the handler is
//! allowed, along HOW the handler spends the span (the durable SDK's own `context.wait`, and
//! the standard library's `sleep`) and along what the execution comes to (an execution that
//! outlived its timeout, one whose handler returned after waiting longer than one invocation
//! is allowed, and one whose handler raised).
//!
//! Every status is read TWICE — off `GetDurableExecution` and off the status-change event the
//! account's default bus delivered — so a status reported to a poller and one published to a
//! rule cannot disagree. The timed-out rows are read a third time, off
//! `GetDurableExecutionHistory`: the history of an execution the timeout ended closes with
//! the model's `ExecutionTimedOut` event and not with `ExecutionFailed`.
//!
//! The three-service harness (the router, the queue-and-rule writer and the drain that files
//! what a queue received) and the package writer are the ones
//! `durable_execution_status_events.rs` and `synchronous_invoke_run.rs` publish, so this suite
//! drives the same wire rather than rebuilding one that could disagree with it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::durable_execution_status_events::{
    base64_of, control_queue, drain_into, emulator, DURABLE_EXECUTION_ARN_HEADER,
    DURABLE_EXECUTION_NAME_HEADER, DURABLE_EXECUTION_STATUS_CHANGE, EVENT_SOURCE,
    INVOCATIONS_URI,
};
use crate::function_lifecycle::{create_function, json_call, make_activation_due, send, ROLE};
use crate::synchronous_invoke_run::package;

/// The rule that watches every durable-execution status change of this suite's functions, and
/// the queue it delivers to.
const RULE: &str = "durable-timeout-statuses";
const QUEUE: &str = "durable-timeout-statuses";

/// How long this suite waits for an execution to reach its terminal status and for both of
/// its events to be delivered. A ceiling on PATIENCE rather than a span any row takes: every
/// wait below is a POLL for the condition, so a row that reaches its status at once does not
/// spend this.
const PATIENCE: Duration = Duration::from_secs(120);

/// How long one poll waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: how the execution's span is configured, how its handler spends it, and what the
/// execution must come to.
struct Case {
    /// What makes this row its own case, and names its function and execution.
    case: &'static str,
    /// The handler's body.
    source: &'static str,
    /// The span the handler asks to wait for, which its event carries.
    wait_seconds: u64,
    /// The `DurableConfig$ExecutionTimeout` the function runs under, in seconds.
    execution_timeout: i64,
    /// The function `Timeout` ONE invocation of the handler is allowed, in seconds.
    function_timeout: i64,
    /// The `ExecutionStatus` the execution must end in.
    terminal: &'static str,
}

/// A durable handler that waits through the durable SDK's own `context.wait`, which is how a
/// durable execution spends a span of its own (`durable_execution_sdk_runtime.rs` is the suite
/// for that SDK itself).
const DURABLE_WAIT: &str = r#"
from aws_durable_execution_sdk_python import Duration, durable_execution


@durable_execution
def lambda_handler(event, context):
    context.wait(Duration.from_seconds(event["wait_seconds"]))
    return {"waited": event["wait_seconds"]}
"#;

/// A handler that spends the span in the standard library's own sleep instead, so the rule is
/// not about which call the handler waits in.
const PLAIN_SLEEP: &str = "import time\n\n\ndef lambda_handler(event, context):\n    \
                           time.sleep(event['wait_seconds'])\n    return {'done': True}\n";

/// A handler that RAISES, which is what `FAILED` is the status of.
const RAISES: &str = "def lambda_handler(event, context):\n    \
                      raise ValueError('the handler broke')\n";

/// RULE: an execution that outlives its `ExecutionTimeout` ends `TIMED_OUT`, ending no
/// earlier than that timeout after it started, and the status-change event its rule receives
/// reports that same status; an execution that finishes inside its `ExecutionTimeout` ends on
/// what its handler produced, whatever one invocation of that handler was allowed.
#[tokio::test]
async fn an_execution_is_ended_by_its_execution_timeout_and_never_by_one_invocations_span() {
    let rows = [
        Case {
            case: "a durable wait past a timeout shorter than one invocation's span",
            source: DURABLE_WAIT,
            wait_seconds: 60,
            // The model's own floor for `ExecutionTimeout`.
            execution_timeout: 1,
            function_timeout: 10,
            terminal: "TIMED_OUT",
        },
        Case {
            case: "a durable wait past a timeout longer than one invocation's span",
            source: DURABLE_WAIT,
            wait_seconds: 120,
            execution_timeout: 4,
            function_timeout: 1,
            terminal: "TIMED_OUT",
        },
        Case {
            case: "a plain sleep past a timeout longer than one invocation's span",
            source: PLAIN_SLEEP,
            wait_seconds: 90,
            execution_timeout: 2,
            function_timeout: 1,
            terminal: "TIMED_OUT",
        },
        Case {
            case: "a wait longer than one invocation's span but inside the execution timeout",
            source: DURABLE_WAIT,
            wait_seconds: 3,
            execution_timeout: 600,
            function_timeout: 1,
            terminal: "SUCCEEDED",
        },
        Case {
            case: "a handler that raised inside its execution timeout",
            source: RAISES,
            wait_seconds: 0,
            execution_timeout: 600,
            function_timeout: 10,
            terminal: "FAILED",
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);
    let queue = control_queue(
        &router,
        QUEUE,
        RULE,
        &json!({ "source": [EVENT_SOURCE], "detail-type": [DURABLE_EXECUTION_STATUS_CHANGE] }),
    )
    .await;

    // Every event this suite's executions published, keyed by the execution it is about and
    // the status it reports, so a row reads its own events out of the one queue the rule
    // delivers to.
    let mut delivered: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();

    for (index, row) in rows.iter().enumerate() {
        let function = format!("durable-timeout-{index}");
        let created = create_function(
            &router,
            &json!({
                "FunctionName": function,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "lambda_function.lambda_handler",
                "Timeout": row.function_timeout,
                "DurableConfig": { "ExecutionTimeout": row.execution_timeout },
                "Code": { "ZipFile": base64_of(&package(row.source)) },
            }),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {:?}: expected 201 from CreateFunction, got {} {}",
            row.case,
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        // The provisioning window the create documents is brought forward rather than waited
        // out, which is what every suite of this target does with a clock-driven transition.
        make_activation_due(&engine);

        // No RETRY for a run that does not succeed: no row here is about how many delivery
        // attempts an event has, and Lambda waits minutes between the attempts it makes.
        let configured = json_call(
            &router,
            "PUT",
            &format!("/2019-09-25/functions/{function}/event-invoke-config"),
            &json!({ "MaximumRetryAttempts": 0 }),
        )
        .await;
        assert_eq!(
            configured.status,
            200,
            "case {:?}: expected 200 from PutFunctionEventInvokeConfig, got {} {}",
            row.case,
            configured.status,
            String::from_utf8_lossy(&configured.body)
        );

        let execution = format!("timeout-{index}");
        let invoked = send(
            &router,
            "POST",
            &format!("{INVOCATIONS_URI}/{function}/invocations"),
            "application/json",
            format!("{{\"wait_seconds\":{}}}", row.wait_seconds).into_bytes(),
            &[
                ("x-amz-invocation-type", "Event".to_string()),
                (DURABLE_EXECUTION_NAME_HEADER, execution.clone()),
            ],
        )
        .await;
        assert_eq!(
            invoked.status,
            202,
            "case {:?}: expected 202 from an asynchronous Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the accepted invocation must answer an execution ARN",
                    row.case
                )
            });

        // What a POLLER reads: the execution's own report, looked at again until it reports a
        // terminal status.
        let read = settled(&router, row.case, &arn).await;
        let status = read["Status"].as_str().unwrap_or_default();
        assert_eq!(
            status, row.terminal,
            "case {:?}: expected GetDurableExecution to report {}, got {status} in {read}",
            row.case, row.terminal
        );

        // An execution the TIMEOUT ended ended when that timeout fell due, so it ran for at
        // least the span the configuration allowed it.
        if row.terminal == "TIMED_OUT" {
            let started = seconds_of(&read["StartTimestamp"]);
            let ended = seconds_of(&read["EndTimestamp"]);
            let ran = match (started, ended) {
                (Some(started), Some(ended)) => ended - started,
                _ => panic!(
                    "case {:?}: expected both instants on a timed-out execution, got {read}",
                    row.case
                ),
            };
            assert!(
                ran >= row.execution_timeout as f64,
                "case {:?}: expected an execution that outlived a {}s timeout to end at least \
                 that long after it started, got {ran}s in {read}",
                row.case,
                row.execution_timeout
            );

            // And its HISTORY closes with the event that timeout is, rather than with the
            // failure of a handler.
            let events = history(&router, row.case, &arn).await;
            let last = events.last().map(String::as_str).unwrap_or_default();
            assert_eq!(
                last, "ExecutionTimedOut",
                "case {:?}: expected the history to close with ExecutionTimedOut, got \
                 {events:?}",
                row.case
            );
        }

        // What a RULE's target reads: the statuses the execution published, which must be the
        // move into `RUNNING` and the terminal one it just reported — and nothing else.
        collect_until(
            &router,
            &queue,
            &mut delivered,
            row.case,
            &arn,
            row.terminal,
        )
        .await;
        drain_into(&router, &queue, &mut delivered).await;
        let published: BTreeSet<String> = delivered
            .keys()
            .filter(|(published_arn, _)| *published_arn == arn)
            .map(|(_, status)| status.clone())
            .collect();
        let expected: BTreeSet<String> =
            ["RUNNING", row.terminal].iter().map(|status| (*status).to_string()).collect();
        assert_eq!(
            published, expected,
            "case {:?}: expected the delivered statuses to be {expected:?}, got {published:?}",
            row.case
        );
        for status in &expected {
            let events = delivered
                .get(&(arn.clone(), status.clone()))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                events.len(),
                1,
                "case {:?}: expected exactly one {status} event for {arn}, got {events:?}",
                row.case
            );
        }
    }
}

/// `GetDurableExecution` on `arn`, looked at again until it reports a status that is not
/// `RUNNING`; answers that report. Fails the case when nothing settles it inside [`PATIENCE`].
async fn settled(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
) -> Value {
    let deadline = Instant::now() + PATIENCE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let read = json_call(
            router,
            "GET",
            &format!(
                "/2025-12-01/durable-executions/{}",
                arn.replace('/', "%2F")
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected 200 from GetDurableExecution, got {} {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        last = read.json();
        if last["Status"].as_str().is_some_and(|status| status != "RUNNING") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: {arn} was still RUNNING after {PATIENCE:?}: {last}");
}

/// The `EventType` of every event `GetDurableExecutionHistory` reports for `arn`, in order.
async fn history(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
) -> Vec<String> {
    let read = json_call(
        router,
        "GET",
        &format!(
            "/2025-12-01/durable-executions/{}/history",
            arn.replace('/', "%2F")
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected 200 from GetDurableExecutionHistory, got {} {}",
        read.status,
        String::from_utf8_lossy(&read.body)
    );
    read.json()["Events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event["EventType"].as_str().map(str::to_string))
        .collect()
}

/// Poll the queue at `url` until the event reporting `status` for `arn` has arrived, filing
/// everything that arrives meanwhile. Fails the case when nothing delivers it in time.
async fn collect_until(
    router: &smithy_server_runtime::MultiServiceRouter,
    url: &str,
    into: &mut BTreeMap<(String, String), Vec<Value>>,
    case: &str,
    arn: &str,
    status: &str,
) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if into.contains_key(&(arn.to_string(), status.to_string())) {
            return;
        }
        drain_into(router, url, into).await;
        if into.contains_key(&(arn.to_string(), status.to_string())) {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "case {case:?}: no {status} status-change event was delivered for {arn} within \
         {PATIENCE:?}; the queue held {:?}",
        into.keys().collect::<Vec<_>>()
    );
}

/// The epoch seconds a modelled `restJson1` timestamp member carries: such a member is epoch
/// seconds with a fraction.
fn seconds_of(value: &Value) -> Option<f64> {
    value.as_f64()
}
