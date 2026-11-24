// SPDX-License-Identifier: CC0-1.0

//! What a function's `Timeout` bounds: "the amount of time (in seconds) that Lambda allows a
//! function to run before stopping it".
//!
//! The rule pinned here: that span bounds the HANDLER's own run, and a handler whose whole work
//! is an SDK call gets through it inside every `Timeout` the model allows — down to the
//! smallest one, and so inside the default a create that states none takes — while a handler
//! that asks to WAIT longer than its span is stopped on its deadline and reports it. So what a
//! function is configured for decides whether an invocation is stopped, and nothing else does:
//! a client that configures neither `Timeout` nor `MemorySize` still gets its work done.
//!
//! It is the configuration the suites over an event source do NOT drive — each of them gives
//! its function a `Timeout` as long as its queue's own `VisibilityTimeout` — so the defaults a
//! client gets when it asks for none are pinned here, where one invocation is the whole of what
//! is under test.
//!
//! The rows differ along what the rule generalises over: the `Timeout` the function is
//! configured with (the smallest the model allows, the default a create with no `Timeout`
//! takes, and a generous one) and what the handler does inside it (one SDK call, several, and
//! a wait past its own span).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{call, call_sqs, create_queue, create_running_function, router_with_queues};

/// The smallest `Timeout` the model allows (`smithy_query(query="shape",shape="Timeout")`:
/// `@range(min: 1)`), and the one a create that states no `Timeout` takes — AWS's documented
/// default of 3 seconds.
const SMALLEST_TIMEOUT: i64 = 1;
/// See [`SMALLEST_TIMEOUT`].
const DEFAULT_TIMEOUT: i64 = 3;

/// A span well past what one of these handlers needs of its own time.
const GENEROUS_TIMEOUT: i64 = 30;

/// How long a row waits for a message the handler's own send put on the sink queue.
const BUDGET: Duration = Duration::from_secs(60);

/// How long between two looks while waiting for one.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// A handler whose whole work is `sends` calls of one in-process SDK operation, the way a
/// relay written for a managed runtime does it: the client is built at module scope and the
/// handler sends the event on.
fn relay_handler(sends: usize) -> String {
    format!(
        "import json\n\
         import os\n\
         \n\
         import boto3\n\
         \n\
         QUEUES = boto3.client(\"sqs\")\n\
         SINK_URL = os.environ[\"SINK_URL\"]\n\
         \n\
         \n\
         def lambda_handler(event, context):\n    \
             for index in range({sends}):\n        \
                 QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(event))\n    \
             return {{\"sent\": {sends}}}\n"
    )
}

/// `Invoke` of `function` with `event`.
async fn invoke(router: &MultiServiceRouter, function: &str, event: &Value) -> (u16, String, Value) {
    let answered = call(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event,
    )
    .await;
    (
        answered.status,
        answered.header("x-amz-function-error").unwrap_or_default(),
        answered.json(),
    )
}

/// How many messages the queue `url` addresses has handed over by the time `wanted` of them
/// have arrived, or everything it handed over inside [`BUDGET`].
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + BUDGET;
    let mut bodies: Vec<String> = Vec::new();
    while std::time::Instant::now() < deadline && bodies.len() < wanted {
        let (status, answered) = call_sqs(
            router,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ReceiveMessage to answer 200, got {status} {answered}"
        );
        if let Some(messages) = answered["Messages"].as_array() {
            for message in messages {
                bodies.push(message["Body"].as_str().unwrap_or_default().to_string());
            }
        }
        if bodies.len() < wanted {
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }
    bodies
}

#[tokio::test]
async fn one_sdk_call_is_work_the_smallest_timeout_has_room_for() {
    // (case, the function's `Timeout`, how many sends its handler makes)
    let rows: &[(&str, i64, usize)] = &[
        ("the smallest Timeout the model allows", SMALLEST_TIMEOUT, 1),
        ("the default Timeout a create takes", DEFAULT_TIMEOUT, 1),
        ("several sends inside the default Timeout", DEFAULT_TIMEOUT, 3),
        ("a generous Timeout", GENEROUS_TIMEOUT, 1),
    ];
    for (index, (case, timeout, sends)) in rows.iter().enumerate() {
        let router = router_with_queues();
        let (sink_url, _) = create_queue(&router, case, &format!("span-sink-{index}"), &json!({}))
            .await;
        let function = format!("span-relay-{index}");
        create_running_function(
            &router,
            case,
            &function,
            &relay_handler(*sends),
            *timeout,
            &json!({ "SINK_URL": sink_url }),
        )
        .await;

        let (status, function_error, answered) =
            invoke(&router, &function, &json!({ "marker": case })).await;
        assert_eq!(
            (status, function_error.as_str()),
            (200, ""),
            "case {case:?}: a handler whose work is {sends} SDK call(s) must answer inside a \
             Timeout of {timeout}s, got {status} {function_error:?} {answered}"
        );
        assert_eq!(
            answered,
            json!({ "sent": sends }),
            "case {case:?}: expected the handler's own value, got {answered}"
        );

        // And the sends really happened: the sink holds one message per send.
        let bodies = drained(&router, case, &sink_url, *sends).await;
        assert_eq!(
            bodies.len(),
            *sends,
            "case {case:?}: expected {sends} message(s) on the sink queue, got {bodies:?}"
        );
        for body in &bodies {
            let relayed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            assert_eq!(
                relayed,
                json!({ "marker": case }),
                "case {case:?}: expected the event the handler was given, got {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_handler_that_waits_past_its_span_is_still_stopped_on_its_deadline() {
    // The other half of the rule: the span still bounds the guest's OWN time, so a handler
    // that asks to wait longer than its `Timeout` is stopped and says so.
    let rows: &[(&str, i64)] = &[
        ("the smallest Timeout the model allows", SMALLEST_TIMEOUT),
        ("the default Timeout a create takes", DEFAULT_TIMEOUT),
    ];
    for (index, (case, timeout)) in rows.iter().enumerate() {
        let router = router_with_queues();
        let function = format!("span-sleeper-{index}");
        create_running_function(
            &router,
            case,
            &function,
            "import time\n\
             \n\
             \n\
             def lambda_handler(event, context):\n    \
                 time.sleep(600)\n    \
                 return {\"slept\": True}\n",
            *timeout,
            &json!({}),
        )
        .await;
        let (status, function_error, answered) = invoke(&router, &function, &json!({})).await;
        assert_eq!(
            (status, function_error.as_str()),
            (200, "Unhandled"),
            "case {case:?}: a handler that waits past its own span must be stopped, got \
             {status} {function_error:?} {answered}"
        );
        let stated = format!("Task timed out after {:.2} seconds", *timeout as f64);
        let message = answered["errorMessage"].as_str().unwrap_or_default();
        assert!(
            message.contains(&stated),
            "case {case:?}: expected the stop to state {stated:?}, got {answered}"
        );
    }
}
