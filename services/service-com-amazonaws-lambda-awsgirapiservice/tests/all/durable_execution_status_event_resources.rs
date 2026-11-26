// SPDX-License-Identifier: CC0-1.0

//! WHICH resource a durable execution's status-change event names in its envelope's
//! `resources` — Lambda, Amazon EventBridge and Amazon SQS on ONE engine, with every envelope
//! read back off the queue its rule targets, where a client reads it.
//!
//! The RULE pinned here: **an event's `resources` names the resource the event is ABOUT at the
//! granularity the publishing service uses, which for a durable-execution status change is the
//! FUNCTION the execution runs on — one element, the ARN of the function version the execution
//! runs (`$LATEST` included), the same string the envelope's `detail.functionArn` carries — so
//! a rule whose `EventPattern` keys `resources` on that ARN receives every status change of
//! that function's executions; the EXECUTION's own identity is carried in the `detail`
//! (`durableExecutionArn`, `durableExecutionName`) and never in `resources`.** Every status the
//! execution moves into carries that same single-element array, so a consumer reading
//! `resources[0]` as a function ARN gets one on the first event and on the terminal one alike.
//!
//! Sources: `smithy_query(query="shape",shape="ExecutionStatus")` for the statuses an execution
//! takes (`RUNNING`, `SUCCEEDED`, `FAILED`, `TIMED_OUT`, `STOPPED`) and
//! `smithy_query(query="shape",shape="DurableConfig")` for the `ExecutionTimeout`
//! (`@range(min: 1, max: 31622400)`) the timed-out row runs under. The envelope itself is in no
//! model: it is recorded behaviour, and the member a rule filters on is what this suite holds
//! it to.
//!
//! The rows differ along what the rule generalises over: WHICH version the execution runs on
//! (the unpublished `$LATEST` and a published numbered version, which is the qualifier the
//! resource ARN must carry) and HOW the execution reaches its terminal status — a handler that
//! returned, a handler that raised, an execution that outlived its
//! `DurableConfig.ExecutionTimeout` with nobody calling at all, and an execution a caller
//! stopped.
//!
//! `durable_execution_status_events.rs` is the suite for the rest of the envelope and for a
//! status change reaching a rule at all, and `durable_execution_status_scoped_rules.rs` the one
//! for the routing between status-scoped rules; the harness all three drive — the router over
//! the three services, the queue-and-rule writer and the drain — is the one published there, so
//! they cannot disagree about the wire.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::durable_execution_status_events::{
    base64_of, control_queue, drain_into, emulator, DURABLE_EXECUTION_ARN_HEADER,
    DURABLE_EXECUTION_NAME_HEADER, DURABLE_EXECUTION_STATUS_CHANGE, EVENT_SOURCE, FUNCTION_TIMEOUT,
    HANDLER_WAIT_SECONDS, INVOCATIONS_URI, LONG_EXECUTION_TIMEOUT, RETURNS, SHORT_EXECUTION_TIMEOUT,
    WAITS,
};
use crate::function_lifecycle::{create_function, json_call, make_activation_due, send, ROLE};
use crate::synchronous_invoke_run::package;

/// The qualifier of the version an invocation that names none runs.
const VERSION_LATEST: &str = "$LATEST";

/// A handler that RAISES, which is how a row reaches `FAILED`.
const RAISES: &str = "def lambda_handler(event, context):\n    raise ValueError('step broke')\n";

/// How long this suite waits for a status to be reached and its envelope delivered.
///
/// A ceiling on PATIENCE rather than a span anything counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host delays a row
/// instead of failing it, and only something that never arrives fails the test.
const DELIVERY_CEILING: Duration = Duration::from_secs(180);

/// How long one look at a queue waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: the version the execution runs on, and how it reaches its terminal status.
struct Case {
    /// What makes this row its own case, and names its function and its execution.
    case: &'static str,
    /// The handler's body.
    source: &'static str,
    /// Whether the invocation runs a PUBLISHED version rather than the unpublished one, which
    /// is the qualifier the resource ARN must carry.
    published: bool,
    /// The `DurableConfig$ExecutionTimeout` the function runs under.
    execution_timeout: i64,
    /// Whether the row STOPS the execution once it is running, rather than letting the run or
    /// the clock decide it.
    stopped: bool,
    /// The `ExecutionStatus` the execution ends in, which with `RUNNING` is the whole set of
    /// statuses whose envelopes the row reads.
    terminal: &'static str,
}

/// RULE: every durable-execution status-change envelope names the FUNCTION the execution runs
/// on in `resources` — one element, the ARN of the version it runs, the same string the
/// `detail` names the function by — so a rule keyed on `resources` receives those events.
#[tokio::test]
async fn a_status_change_envelope_names_the_function_version_in_resources() {
    let rows = [
        Case {
            case: "an execution of the unpublished version whose handler returned",
            source: RETURNS,
            published: false,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "SUCCEEDED",
        },
        Case {
            case: "an execution of a published version whose handler returned",
            source: RETURNS,
            published: true,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "SUCCEEDED",
        },
        Case {
            case: "an execution of the unpublished version whose handler raised",
            source: RAISES,
            published: false,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "FAILED",
        },
        Case {
            case: "an execution that outlived its ExecutionTimeout",
            source: WAITS,
            published: false,
            execution_timeout: SHORT_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "TIMED_OUT",
        },
        Case {
            case: "an execution a caller stopped",
            source: WAITS,
            published: false,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: true,
            terminal: "STOPPED",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One store per row, so each row reads its own queue from nothing and the rule keyed on
        // this row's function ARN is the only one watching.
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);
        let function = format!("durable-resources-{index}");
        let created = create_function(
            &router,
            &json!({
                "FunctionName": function,
                "Role": ROLE,
                "Runtime": "python3.12",
                "Handler": "lambda_function.lambda_handler",
                "Timeout": FUNCTION_TIMEOUT,
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
        let function_arn = created.string("FunctionArn").unwrap_or_else(|| {
            panic!(
                "case {:?}: CreateFunction must answer a FunctionArn",
                row.case
            )
        });
        // The creation transition is brought forward rather than waited out: nothing here is
        // about how long a function stays `Pending`.
        make_activation_due(&engine);

        // The version the execution will run on, and the ARN of THAT version — which is what
        // the envelope's `resources` must name.
        let (qualifier, version) = if row.published {
            let published = json_call(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{function}/versions"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                published.status,
                201,
                "case {:?}: expected 201 from PublishVersion, got {} {}",
                row.case,
                published.status,
                String::from_utf8_lossy(&published.body)
            );
            let version = published.string("Version").unwrap_or_else(|| {
                panic!(
                    "case {:?}: PublishVersion must answer the version it made",
                    row.case
                )
            });
            (format!("?Qualifier={version}"), version)
        } else {
            (String::new(), VERSION_LATEST.to_string())
        };
        let version_arn = format!("{function_arn}:{version}");

        // No RETRY for an errored handler: no row here is about how many delivery attempts an
        // asynchronous invocation makes, and Lambda waits minutes between the attempts it
        // makes, so a row left on the default would wait those out before the execution could
        // reach the status it IS about.
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

        // The consumer: a queue behind a rule whose pattern keys `resources` on the qualified
        // ARN of the version the execution runs on — the normal way to subscribe to one
        // function's durable-execution status changes — so a delivery proves that key matches.
        let queue = control_queue(
            &router,
            &format!("durable-resources-{index}"),
            &format!("durable-resources-rule-{index}"),
            &json!({
                "source": [EVENT_SOURCE],
                "detail-type": [DURABLE_EXECUTION_STATUS_CHANGE],
                "resources": [&version_arn],
            }),
        )
        .await;

        let execution = format!("resources-{index}");
        let invoked = send(
            &router,
            "POST",
            &format!("{INVOCATIONS_URI}/{function}/invocations{qualifier}"),
            "application/json",
            format!("{{\"wait_seconds\":{HANDLER_WAIT_SECONDS}}}").into_bytes(),
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

        // The start owes a `RUNNING` change, which is waited FOR: the publication happens on
        // the store's own clock rather than inside the call that was answered.
        let mut delivered: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
        until_delivered(&router, &queue, &mut delivered, row.case, &arn, "RUNNING").await;

        if row.stopped {
            let stop = json_call(
                &router,
                "POST",
                &format!(
                    "/2025-12-01/durable-executions/{}/stop",
                    arn.replace('/', "%2F")
                ),
                &json!({ "ErrorType": "OperatorStopped", "ErrorMessage": "stopped by the test" }),
            )
            .await;
            assert_eq!(
                stop.status,
                200,
                "case {:?}: expected 200 from StopDurableExecution, got {} {}",
                row.case,
                stop.status,
                String::from_utf8_lossy(&stop.body)
            );
        }
        until_delivered(
            &router,
            &queue,
            &mut delivered,
            row.case,
            &arn,
            row.terminal,
        )
        .await;

        for status in ["RUNNING", row.terminal] {
            let events = delivered
                .get(&(arn.clone(), status.to_string()))
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                events.len(),
                1,
                "case {:?}: expected exactly one {status} envelope for {arn}, got {} \
                 ({events:?})",
                row.case,
                events.len()
            );
            let envelope = &events[0];
            let resources = envelope["resources"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                resources,
                vec![Value::String(version_arn.clone())],
                "case {:?}: expected the {status} envelope's resources to name the function \
                 version the execution runs on ([{version_arn:?}]), got {:?} in {envelope}",
                row.case,
                envelope["resources"]
            );
            // The resource named in the envelope and the function named in the detail are ONE
            // string: the execution's own identity is what the detail carries beside it.
            assert_eq!(
                resources.first().and_then(Value::as_str),
                envelope["detail"]["functionArn"].as_str(),
                "case {:?}: expected the {status} envelope's resources and its \
                 detail.functionArn to be the same function ARN, got {:?} and {:?}",
                row.case,
                envelope["resources"],
                envelope["detail"]["functionArn"]
            );
            assert_eq!(
                envelope["detail"]["durableExecutionArn"].as_str(),
                Some(arn.as_str()),
                "case {:?}: expected the {status} envelope's detail to name the execution \
                 {arn}, got {envelope}",
                row.case
            );
        }
    }
}

/// Look again at the queue at `url` until the envelope reporting `status` for `arn` has
/// arrived, filing everything that arrives meanwhile. Fails the case when nothing delivers it
/// inside [`DELIVERY_CEILING`].
async fn until_delivered(
    router: &MultiServiceRouter,
    url: &str,
    into: &mut BTreeMap<(String, String), Vec<Value>>,
    case: &str,
    arn: &str,
    status: &str,
) {
    let deadline = Instant::now() + DELIVERY_CEILING;
    loop {
        drain_into(router, url, into).await;
        if into.contains_key(&(arn.to_string(), status.to_string())) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no {status} status-change envelope was delivered for {arn} within \
             {DELIVERY_CEILING:?}; the queue held {:?}",
            into.keys().collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
