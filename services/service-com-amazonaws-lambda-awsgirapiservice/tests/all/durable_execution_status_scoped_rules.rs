// SPDX-License-Identifier: CC0-1.0

//! WHICH rule on the account's default EventBridge bus a durable execution's status change
//! reaches, when several rules on that bus are each pinned to ONE status — Lambda, Amazon
//! EventBridge and Amazon SQS on ONE engine, with every envelope read back off the queue its
//! rule targets, where a client reads it.
//!
//! The RULE pinned here: **a status-change event is routed by the status it REPORTS, so a rule
//! whose `EventPattern` pins `detail.status` to one `ExecutionStatus` value receives exactly
//! the durable-execution status changes into that status — one envelope per execution that
//! moved into it — and no envelope of any other status, whatever other rules watch the same
//! function and the same bus.** A rule pinned to a status none of the executions moved into
//! therefore receives nothing at all, and an execution that failed is delivered to the rule
//! that names `FAILED` and to no other.
//!
//! Sources: `smithy_query(query="shape",shape="ExecutionStatus")` for the five statuses an
//! execution takes (`RUNNING`, `SUCCEEDED`, `FAILED`, `TIMED_OUT`, `STOPPED`), which is the
//! catalog the rules below are written from — one rule per value, so no value is left
//! unasserted; and `smithy_query(query="shape",shape="DurableConfig")` for the
//! `ExecutionTimeout` (`@range(min: 1, max: 31622400)`) the timed-out row runs under. The
//! publication itself is in no model: it is recorded behaviour, and the `detail` member a rule
//! filters on is what this suite holds it to.
//!
//! The rows differ along what the rule generalises over: HOW the execution reaches the status
//! it is delivered for — a handler that returned, a handler that raised, an execution that
//! outlived its `DurableConfig.ExecutionTimeout` with nobody calling at all, and an execution
//! a caller stopped — so each of the four terminal statuses is produced a different way, and
//! each row's execution is every other row's control: its envelopes must be absent from the
//! four queues its statuses do not name.
//!
//! `durable_execution_status_events.rs` is the suite for WHAT one envelope carries and for a
//! status change reaching a rule on the bus at all; this one is the suite for the ROUTING
//! between rules, which that suite reads for one status only. Its harness — the router over
//! the three services, the queue-and-rule writer and the drain — is the one published there,
//! so both suites drive the same wire.

use std::collections::{BTreeMap, BTreeSet};
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

/// Every `ExecutionStatus` the model declares
/// (`smithy_query(query="shape",shape="ExecutionStatus")`), which is one rule each below: the
/// catalog is spelled out here rather than read from the service, so a value the service
/// publishes under another spelling fails this suite.
const STATUSES: [&str; 5] = ["RUNNING", "SUCCEEDED", "FAILED", "TIMED_OUT", "STOPPED"];

/// A handler that RAISES, which is how a row reaches `FAILED`.
const RAISES: &str = "def lambda_handler(event, context):\n    raise ValueError('step broke')\n";

/// How long this suite waits for an accepted invocation to run, for the status a row is about
/// to be reached and for its envelope to be delivered.
///
/// A ceiling on PATIENCE rather than a span anything counts on: every wait below looks again
/// until what it waits for is there or this deadline passes, so a loaded host delays a row
/// instead of failing it, and only something that never arrives fails the test.
const DELIVERY_CEILING: Duration = Duration::from_secs(180);

/// How long one look at a queue waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One row: how its execution reaches the terminal status it must be delivered for.
struct Case {
    /// What makes this row its own case, and names its function and its execution.
    case: &'static str,
    /// The handler's body.
    source: &'static str,
    /// The `DurableConfig$ExecutionTimeout` the function runs under.
    execution_timeout: i64,
    /// Whether the row STOPS the execution once it is running, rather than letting the run or
    /// the clock decide it.
    stopped: bool,
    /// The `ExecutionStatus` the execution ends in, which with `RUNNING` is the whole set of
    /// statuses its envelopes may appear under.
    terminal: &'static str,
}

/// What one row's execution became: the case it belongs to, the ARN it is addressed by and
/// the terminal status it reached.
struct Started {
    case: &'static str,
    arn: String,
    terminal: &'static str,
}

/// One rule's queue, and what has been taken off it so far, filed by the execution an
/// envelope is about and the status it reports (the shape [`drain_into`] files into).
struct Watcher {
    /// The `ExecutionStatus` this rule's `EventPattern` pins `detail.status` to.
    status: &'static str,
    /// The url of the queue the rule targets.
    url: String,
    /// Every envelope taken off that queue so far.
    seen: BTreeMap<(String, String), Vec<Value>>,
}

impl Watcher {
    /// Whether the envelope reporting `status` for the execution `arn` has arrived.
    fn holds(&self, arn: &str, status: &str) -> bool {
        self.seen
            .contains_key(&(arn.to_string(), status.to_string()))
    }
}

/// Take whatever each rule's queue is holding, so what a row reads next is what arrived
/// since.
async fn drain_every(router: &MultiServiceRouter, watchers: &mut [Watcher]) {
    for watcher in watchers.iter_mut() {
        drain_into(router, &watcher.url, &mut watcher.seen).await;
    }
}

/// Look again at every queue until the rule pinned to `status` holds the envelope reporting it
/// for `arn`, filing everything else that arrives meanwhile. Fails the case when nothing
/// delivers it inside [`DELIVERY_CEILING`].
async fn until_delivered(
    router: &MultiServiceRouter,
    watchers: &mut [Watcher],
    case: &str,
    arn: &str,
    status: &str,
) {
    let deadline = Instant::now() + DELIVERY_CEILING;
    loop {
        drain_every(router, watchers).await;
        if watchers
            .iter()
            .any(|watcher| watcher.status == status && watcher.holds(arn, status))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no {status} status-change event reached the rule pinned to \
             {status} for {arn} within {DELIVERY_CEILING:?}; the queues held {:?}",
            watchers
                .iter()
                .map(|watcher| (watcher.status, watcher.seen.keys().collect::<Vec<_>>()))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: each rule pinned to one `ExecutionStatus` receives exactly the status changes into
/// that status — one envelope per execution that moved into it — and nothing of any other
/// status, however the executions reached theirs.
#[tokio::test]
async fn a_status_scoped_rule_receives_only_its_own_status() {
    let rows = [
        Case {
            case: "a handler that returned",
            source: RETURNS,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "SUCCEEDED",
        },
        Case {
            case: "a handler that raised",
            source: RAISES,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "FAILED",
        },
        Case {
            case: "an execution that outlived its ExecutionTimeout",
            source: WAITS,
            execution_timeout: SHORT_EXECUTION_TIMEOUT,
            stopped: false,
            terminal: "TIMED_OUT",
        },
        Case {
            case: "an execution a caller stopped",
            source: WAITS,
            execution_timeout: LONG_EXECUTION_TIMEOUT,
            stopped: true,
            terminal: "STOPPED",
        },
    ];

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);

    // One rule per status the model declares, each on the DEFAULT bus and each with a queue of
    // its own, so which rule an envelope reached is read off which queue holds it.
    let mut watchers: Vec<Watcher> = Vec::new();
    for status in STATUSES {
        let name = format!("durable-{}-only", status.to_lowercase().replace('_', "-"));
        let url = control_queue(
            &router,
            &name,
            &name,
            &json!({
                "source": [EVENT_SOURCE],
                "detail-type": [DURABLE_EXECUTION_STATUS_CHANGE],
                "detail": { "status": [status] },
            }),
        )
        .await;
        watchers.push(Watcher {
            status,
            url,
            seen: BTreeMap::new(),
        });
    }

    let mut started: Vec<Started> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let name = format!("durable-scoped-{index}");
        let created = create_function(
            &router,
            &json!({
                "FunctionName": name,
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
        // The creation transition is brought forward rather than waited out: nothing this
        // suite asserts is about how long a function stays `Pending`.
        make_activation_due(&engine);

        // No RETRY for an errored handler: no row here is about how many delivery attempts an
        // asynchronous invocation makes, and Lambda waits minutes between the attempts it
        // makes, so a row left on the default would wait those out before the execution could
        // reach the status it IS about.
        let configured = json_call(
            &router,
            "PUT",
            &format!("/2019-09-25/functions/{name}/event-invoke-config"),
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

        let execution = format!("exec-{index}");
        let invoked = send(
            &router,
            "POST",
            &format!("{INVOCATIONS_URI}/{name}/invocations"),
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
            "case {:?}: expected 202 from Invoke, got {} {}",
            row.case,
            invoked.status,
            String::from_utf8_lossy(&invoked.body)
        );
        let arn = invoked
            .header(DURABLE_EXECUTION_ARN_HEADER)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the accepted invocation to answer an execution ARN",
                    row.case
                )
            });

        // The start owes a `RUNNING` change, and it is the rule pinned to `RUNNING` that must
        // receive it. It is waited FOR: the publication happens on the store's own clock,
        // after the call that started the execution was answered.
        until_delivered(&router, &mut watchers, row.case, &arn, "RUNNING").await;

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
        started.push(Started {
            case: row.case,
            arn,
            terminal: row.terminal,
        });
    }

    // Each execution's terminal change reaches the rule that names THAT status, whichever of
    // the four ways it was reached.
    for row in &started {
        until_delivered(&router, &mut watchers, row.case, &row.arn, row.terminal).await;
    }
    // Everything every rule has to show for the run, once every expected envelope is in: a
    // queue that received an envelope it may not have holds it after this drain too.
    drain_every(&router, &mut watchers).await;

    for watcher in &watchers {
        // The executions that moved into this rule's status: every one of them for `RUNNING`,
        // which each execution starts in, and the rows whose terminal status it is otherwise.
        let expected: BTreeSet<&str> = started
            .iter()
            .filter(|row| watcher.status == "RUNNING" || row.terminal == watcher.status)
            .map(|row| row.arn.as_str())
            .collect();
        let delivered: BTreeSet<&str> = watcher
            .seen
            .keys()
            .map(|(arn, _)| arn.as_str())
            .collect();
        assert_eq!(
            delivered, expected,
            "case {:?}: expected the rule pinned to {} to receive the executions that moved \
             into it, got {delivered:?}",
            watcher.status, watcher.status
        );
        for ((arn, status), envelopes) in &watcher.seen {
            // The cross-delivery this is about: an envelope reporting any other status reached
            // a rule whose pattern does not name it.
            assert_eq!(
                status, watcher.status,
                "case {:?}: the rule pinned to {} received a {status} envelope for {arn}: \
                 {envelopes:?}",
                watcher.status, watcher.status
            );
            assert_eq!(
                envelopes.len(),
                1,
                "case {:?}: expected exactly one {status} envelope for {arn}, got {} \
                 ({envelopes:?})",
                watcher.status,
                envelopes.len()
            );
            let envelope = &envelopes[0];
            assert_eq!(
                envelope["source"].as_str(),
                Some(EVENT_SOURCE),
                "case {:?}: expected the {status} envelope for {arn} to come from \
                 {EVENT_SOURCE}, got {envelope}",
                watcher.status
            );
            assert_eq!(
                envelope["detail-type"].as_str(),
                Some(DURABLE_EXECUTION_STATUS_CHANGE),
                "case {:?}: expected the {status} envelope for {arn} to carry the detail-type \
                 {DURABLE_EXECUTION_STATUS_CHANGE}, got {envelope}",
                watcher.status
            );
        }
    }
}
