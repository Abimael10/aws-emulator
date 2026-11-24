// SPDX-License-Identifier: CC0-1.0

//! What an asynchronous invocation's ANSWER reports about the run it accepted, and where that
//! run becomes observable instead.
//!
//! RULE: an invocation Lambda answers BEFORE the run happens — `InvocationType=Event`, where
//! "the API response only includes a status code" and "a success response (202 Accepted)
//! indicates that the request is queued for invocation"
//! (`smithy_query(query="doc",shape="Invoke",member="InvocationType")`) — reports that
//! QUEUEING and nothing of the run: the accepted status, an empty payload, and none of the
//! members a completed run reports (`FunctionError`, `LogResult`, `ExecutedVersion`), whatever
//! the delivered run comes to. What it came to is observable where every invocation's log is
//! delivered: the function's log group — "the name of the Amazon CloudWatch log group the
//! function sends logs to", and "by default, Lambda functions send logs to a default log group
//! named `/aws/lambda/<function name>`"
//! (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`) — which the first run
//! that needs it brings into being and which carries the handler's own records and its
//! runtime's error report inside that run's `START`/`END`/`REPORT` framing. So a failure the
//! asynchronous answer withholds is still reported in full by the SYNCHRONOUS answer for the
//! same handler and the same event, which is what tells "withheld from this answer" apart from
//! "never observed at all".
//!
//! The rows differ along what the rule generalises over: what the delivered run comes to (a
//! handler that returns, a handler that raises), which runtime ran it (so both engines' own
//! error reports are read), which group the log goes to (the default one, and one the
//! function's `LoggingConfig` names, in and out of the default prefix), and whether the
//! accepting call asked for the log tail at all — "applies to synchronously invoked functions
//! only" (`smithy_query(query="doc",shape="Invoke",member="LogType")`), so asking changes
//! nothing about what the asynchronous answer carries.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] builds, reused rather than copied — so what is asserted
//! is what a client reads: the `Invoke` answer's own status, body and headers, and
//! `DescribeLogGroups`, `DescribeLogStreams` and `GetLogEvents` on the group.
//!
//! Every wait here is for a CONDITION — the function reporting `Active`, the queued run's log
//! having reached its group — looked at again until it holds or a generous deadline passes, so
//! how fast the host runs decides nothing a row asserts.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::FUNCTION_ERROR_UNHANDLED;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::call;
use crate::invocation_log_delivery::{
    create_function, delivered, emulator, groups, invocation_request_carrying, streams, Created,
    FUNCTION_ERROR_HEADER, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// A well-formed execution role ARN no IAM record is written for: a role the store holds
/// nothing about is evidence of nothing, so no log write is withheld from these runs. What a
/// role short of a logs grant leaves behind is the subject of
/// [`crate::invocation_log_group_lazy_creation`].
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The header `Invoke$InvocationType` is bound to.
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// The invocation type whose answer precedes its run.
const EVENT_TYPE: &str = "Event";

/// The header `Invoke$LogType` is bound to, and the value that asks for the tail.
const LOG_TYPE_HEADER: &str = "x-amz-log-type";
/// See [`LOG_TYPE_HEADER`].
const TAIL: &str = "Tail";

/// The header `Invoke$LogResult` is bound to — "the last 4 KB of the execution log", which no
/// answer given before the run can carry.
const LOG_RESULT_HEADER: &str = "x-amz-log-result";

/// The header `Invoke$ExecutedVersion` is bound to — "the version of the function that
/// executed", which no answer given before the run can carry either.
const EXECUTED_VERSION_HEADER: &str = "x-amz-executed-version";

/// The status the model documents for an accepted asynchronous invocation ("202 Accepted").
const ACCEPTED_STATUS: u16 = 202;

/// The status a synchronous invocation is answered with, a handler that raised included: "an
/// unhandled error is still HTTP 200".
const COMPLETED_STATUS: u16 = 200;

/// How long a row waits for a queued run's log to reach its group before giving up, and how
/// often it looks. The delivery is owed by the CLOCK shortly after the accepting call and is
/// polled for rather than slept out, so the deadline is far above what the work takes and only
/// a delivery nothing is making fails a row.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
/// See [`DELIVERY_DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The python handler every python row runs: it writes the marker its event carries and then
/// either raises or answers it, so a row's outcome is the event's own doing.
const PYTHON_HANDLER: &str = "def lambda_handler(event, context):\n    \
                              print('handled ' + event['marker'])\n    \
                              if event['fail']:\n        \
                              raise ValueError('the queued handler gave up')\n    \
                              return {'handled': event['marker']}\n";

/// The node handler every node row runs, which does the same.
const NODE_HANDLER: &str = "exports.handler = async (event) => {\n  \
                            console.log('handled ' + event.marker);\n  \
                            if (event.fail) {\n    \
                            throw new Error('the queued handler gave up');\n  }\n  \
                            return { handled: event.marker };\n};\n";

/// One row of the table.
struct Row {
    case: &'static str,
    /// The marker the event carries, which the handler writes: it tells this row's records
    /// apart from any other's.
    marker: &'static str,
    /// The runtime that runs the handler, which decides whose error report the log carries.
    runtime: &'static str,
    /// The handler's source.
    source: &'static str,
    /// The group the function's `LoggingConfig` names, or `None` to take the default.
    log_group: Option<&'static str>,
    /// Whether the event asks the handler to raise, which is what the answer must say nothing
    /// about.
    raises: bool,
    /// Whether the accepting call asks for the log tail.
    asks_for_tail: bool,
    /// The text the delivered log has to carry beside the framing: the handler's own record,
    /// and for a run that raised the runtime's own report of the failure.
    carried: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a python handler that returns, logging to its default group",
            marker: "async-python-returned",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_HANDLER,
            log_group: None,
            raises: false,
            asks_for_tail: false,
            carried: &["handled async-python-returned"],
        },
        Row {
            case: "a python handler that raises, logging to its default group, asked for a tail",
            marker: "async-python-raised",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_HANDLER,
            log_group: None,
            raises: true,
            asks_for_tail: true,
            carried: &[
                "handled async-python-raised",
                "[ERROR] ValueError: the queued handler gave up",
                "Traceback (most recent call last):",
            ],
        },
        Row {
            case: "a node handler that raises, logging to a group its configuration names \
                   outside the default prefix",
            marker: "async-node-raised",
            runtime: NODE_RUNTIME,
            source: NODE_HANDLER,
            log_group: Some("/aws/vendedlogs/async-withheld-suite"),
            raises: true,
            asks_for_tail: false,
            carried: &[
                "handled async-node-raised",
                "Invoke Error",
                "the queued handler gave up",
            ],
        },
        Row {
            case: "a node handler that returns, logging to a group its configuration names \
                   under that prefix, asked for a tail",
            marker: "async-node-returned",
            runtime: NODE_RUNTIME,
            source: NODE_HANDLER,
            log_group: Some("/aws/lambda/async-withheld-elsewhere"),
            raises: false,
            asks_for_tail: true,
            carried: &["handled async-node-returned"],
        },
    ]
}

/// What ONE `Invoke` answered, as a client reads it: the status, the body, and the three
/// headers the members a completed run reports are bound to.
struct Answered {
    status: u16,
    body: bytes::Bytes,
    function_error: Option<String>,
    log_result: Option<String>,
    executed_version: Option<String>,
}

/// `Invoke` the function `name` with `event`, carrying `headers` on top of the ones every
/// invocation of this suite carries, and answer what came back untouched — no assertion of its
/// own, because what the answer may and may not carry is the rule under test.
async fn invoked(
    router: &MultiServiceRouter,
    name: &str,
    headers: &[(&str, &str)],
    event: &Value,
) -> Answered {
    let body = event.to_string();
    let response = router
        .dispatch(invocation_request_carrying(name, headers, body.as_bytes()))
        .await;
    let (parts, body) = response.into_parts();
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    Answered {
        status: parts.status.as_u16(),
        function_error: header(FUNCTION_ERROR_HEADER),
        log_result: header(LOG_RESULT_HEADER),
        executed_version: header(EXECUTED_VERSION_HEADER),
        body,
    }
}

/// Settle an errored event on its FIRST delivery attempt.
///
/// No row here is about how many attempts an event gets — that is
/// [`crate::async_retry_budget_attempts`]'s subject — so each function states "the maximum
/// number of times to retry when the function returns an error"
/// (`FunctionEventInvokeConfig$MaximumRetryAttempts`, whose floor is 0) as none, and the log
/// the group then holds is the log of one run.
async fn settle_on_one_attempt(router: &MultiServiceRouter, case: &str, name: &str) {
    let configured = call(
        router,
        "PUT",
        &format!("/2019-09-25/functions/{name}/event-invoke-config"),
        &json!({ "MaximumRetryAttempts": 0 }),
    )
    .await;
    assert_eq!(
        configured.status,
        200,
        "case {case:?}: PutFunctionEventInvokeConfig on {name:?} must succeed, got {} {}",
        configured.status,
        configured.text()
    );
}

/// The streams of `group` and the lines the first of them holds, once the delivery made off the
/// response path has put them there — looked for again until [`DELIVERY_DEADLINE`] passes.
///
/// The delivery opens the stream before it writes to it, so this reads until the EVENTS are
/// there rather than until the group or the stream is: the read a client polling for a queued
/// run's output makes.
async fn delivery_to(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        if !groups(router, case, group).await.is_empty() {
            let held = streams(router, case, group).await;
            if let Some(stream) = held.first() {
                let (lines, _) = delivered(router, case, group, stream).await;
                if !lines.is_empty() {
                    return (held, lines);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the queued invocation's log never reached {group:?} within \
             {DELIVERY_DEADLINE:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The request id the framing of a run's log names, asserting the three frames agree on it.
///
/// The log of the execution environment the run BUILT opens on the record that states its
/// initialization, so the invocation's own opening frame is the first `START` line of the
/// stream.
fn framed_request_id(case: &str, lines: &[String]) -> String {
    let opening = lines
        .iter()
        .find_map(|line| line.strip_prefix("START RequestId: "))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the run's START line in the group, got {lines:?}")
        });
    let id = opening.split(' ').next().unwrap_or_default().to_string();
    for frame in ["END RequestId: ", "REPORT RequestId: "] {
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&format!("{frame}{id}"))),
            "case {case:?}: expected the {frame:?} frame of the run {id:?} in the group, got \
             {lines:?}"
        );
    }
    id
}

/// An accepted asynchronous invocation reports its queueing and nothing of the run, while the
/// run's whole log — the handler's records and its runtime's error report among the framing —
/// is delivered to the function's log group, and the same failure IS reported by the
/// synchronous answer for the same handler and event.
#[tokio::test]
async fn an_accepted_event_reports_only_its_queueing_and_logs_what_the_run_came_to() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("async-withheld-{index}");
        let default_group = format!("/aws/lambda/{name}");
        let group = row.log_group.unwrap_or(&default_group).to_string();
        create_function(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: row.runtime,
                role: ROLE,
                log_group: row.log_group,
            },
        )
        .await;
        settle_on_one_attempt(&router, case, &name).await;
        let event = json!({ "marker": row.marker, "fail": row.raises });

        // The accepting call: the queueing receipt, and no member of a run that has not
        // happened when it is given.
        let mut headers = vec![(INVOCATION_TYPE_HEADER, EVENT_TYPE)];
        if row.asks_for_tail {
            headers.push((LOG_TYPE_HEADER, TAIL));
        }
        let accepted = invoked(&router, &name, &headers, &event).await;
        assert_eq!(
            accepted.status, ACCEPTED_STATUS,
            "case {case:?}: expected the documented {ACCEPTED_STATUS} for an accepted event, got \
             {} {}",
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        assert!(
            accepted.body.is_empty(),
            "case {case:?}: expected an empty payload from the accepting call, got {:?}",
            String::from_utf8_lossy(&accepted.body)
        );
        for (header, carried) in [
            (FUNCTION_ERROR_HEADER, &accepted.function_error),
            (LOG_RESULT_HEADER, &accepted.log_result),
            (EXECUTED_VERSION_HEADER, &accepted.executed_version),
        ] {
            assert!(
                carried.is_none(),
                "case {case:?}: an answer given before the run reports nothing of it, so \
                 {header:?} must be absent, got {carried:?}"
            );
        }

        // And the run itself, where a client reads it: the group the configuration named, which
        // this first run brought into being, holding the one run's whole log.
        let (held, lines) = delivery_to(&router, case, &group).await;
        let found = groups(&router, case, &group).await;
        assert_eq!(
            found,
            vec![group.clone()],
            "case {case:?}: expected the delivered run to create exactly {group:?}, got {found:?}"
        );
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for the one delivered run, got {held:?}"
        );
        framed_request_id(case, &lines);
        for carried in row.carried {
            assert!(
                lines.iter().any(|line| line.contains(carried)),
                "case {case:?}: expected the log in {group:?} to carry {carried:?}, got {lines:?}"
            );
        }
        // A function that logs elsewhere leaves its default group uncreated: the delivery goes
        // to the one destination the configuration named, not to both.
        if row.log_group.is_some() {
            let untouched = groups(&router, case, &default_group).await;
            assert!(
                untouched.is_empty(),
                "case {case:?}: expected {default_group:?} to stay uncreated, got {untouched:?}"
            );
        }

        // The failure the asynchronous answer withheld is reported in full by the SYNCHRONOUS
        // answer for the same handler and the same event — "the status code in the API response
        // doesn't reflect function errors", so it is HTTP 200 carrying the `FunctionError`
        // signal and the error document.
        if row.raises {
            let completed = invoked(&router, &name, &[], &event).await;
            assert_eq!(
                completed.status, COMPLETED_STATUS,
                "case {case:?}: expected HTTP {COMPLETED_STATUS} from the synchronous invoke, got \
                 {} {}",
                completed.status,
                String::from_utf8_lossy(&completed.body)
            );
            assert_eq!(
                completed.function_error.as_deref(),
                Some(FUNCTION_ERROR_UNHANDLED),
                "case {case:?}: expected the synchronous answer to report the handler's failure, \
                 got {:?} with {}",
                completed.function_error,
                String::from_utf8_lossy(&completed.body)
            );
            let document: Value =
                serde_json::from_slice(&completed.body).unwrap_or(Value::Null);
            assert_eq!(
                document["errorMessage"],
                json!("the queued handler gave up"),
                "case {case:?}: expected the error document of the run the asynchronous answer \
                 withheld, got {document}"
            );
        }
    }
}
