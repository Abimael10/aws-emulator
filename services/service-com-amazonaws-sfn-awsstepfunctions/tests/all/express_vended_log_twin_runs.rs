// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RECORD an EXPRESS run leaves, driven over each service's own
//! wire protocol through the production `MultiServiceRouter` with Step Functions and
//! CloudWatch Logs mounted on ONE engine, so what is asserted is what a client polling
//! CloudWatch Logs observes.
//!
//! The rule under test: a state machine whose `loggingConfiguration` logs at a level that
//! selects every event delivers, for EVERY run and not only its first, that run's WHOLE
//! execution history to the configured group — the same `(id, type)` sequence, contiguous
//! from 1, that `GetExecutionHistory` reports for the same definition and input — and
//! `includeExecutionData` ("determines whether execution data is included in your log",
//! `LoggingConfiguration$includeExecutionData`) decides only whether the payload members
//! inside an event's `details` are delivered, never which events, ids or types are. For an
//! EXPRESS workflow that log is the ONLY record: each run is named by its own `express`
//! execution ARN, which the operations that report on an execution refuse.
//!
//! The rows differ along what the rule generalises over: the shape and length of the run's
//! history (a one-state run that succeeds, a run that fails at once, a chain of two
//! states), which run of the state machine it is (first, second, third), how the run is
//! named (two supplied names and one the service mints), and the arm
//! (`includeExecutionData` true and false) — with the sequence each arm must deliver taken
//! from a STANDARD twin's own history rather than written into this suite.
//!
//! The harness (`Observed`, `create`, `logging`) is the one `execution_observability`
//! builds for the same two services and the group reader is `vended_log_event_members`';
//! `tests/all.rs` is this binary's crate root, so both are reached through `crate::<suite>`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    observability, EXECUTION_SETTLE_DELAY, EXPRESS_EXECUTION_RESOURCE,
};

use crate::execution_observability::{create, logging, Observed, FAILING_DEFINITION};
use crate::express_execution_arn_identity::{resource_fields, resource_type_refusal};
use crate::state_machine_lifecycle::{error_code, string, HELLO_DEFINITION};
use crate::vended_log_event_members::PAYLOAD_MEMBERS;

/// How long this suite waits for a delivery, or for a run to reach its outcome, before it
/// fails.
///
/// The outcome of a run is owed to the clock one [`EXECUTION_SETTLE_DELAY`] after the
/// start and the delivery follows it, so the wait is that span with ample room around it —
/// far longer than it takes, and bounded so a run that never lands reports instead of
/// hanging.
const DELIVERY_DEADLINE: Duration = EXECUTION_SETTLE_DELAY.saturating_add(Duration::from_secs(30));

/// How often this suite looks again while it waits.
const POLL: Duration = Duration::from_millis(25);

/// The modelled refusal an operation answers an execution ARN it does not serve with.
const INVALID_ARN: &str = "InvalidArn";
const CLIENT_REFUSAL_STATUS: u16 = 400;

/// The operations of this service that REPORT on one execution, either of which could have
/// been the record of a run (`smithy_query(query="digest")`: `DescribeExecution`,
/// `GetExecutionHistory`).
const REPORTING_OPERATIONS: &[&str] = &["DescribeExecution", "GetExecutionHistory"];

/// A longer run: two `Pass` states in a chain, so a row spans a history of more than one
/// state's entry and exit.
const CHAINED_DEFINITION: &str = r#"{"StartAt":"One","States":{
    "One":{"Type":"Pass","Result":{"step":1},"Next":"Two"},
    "Two":{"Type":"Pass","Result":{"step":2},"End":true}}}"#;

/// One row of the table: the definition every state machine of the row runs.
struct Row {
    /// What the row is about, named in every assertion it makes.
    case: &'static str,
    definition: &'static str,
}

/// How each run of a row is named: a name the start supplies, or one the service mints.
const RUN_NAMES: &[Option<&str>] = &[Some("first"), Some("second"), None];

/// The `(id, type)` pairs of the events `messages` carries for the execution `arn`, in the
/// order the group reports them.
///
/// A vended event carries its id as TEXT, which is the form it is read back in here.
fn sequence(messages: &[Value], arn: &str, case: &str) -> Vec<(i64, String)> {
    messages
        .iter()
        .filter(|message| message.get("execution_arn").and_then(Value::as_str) == Some(arn))
        .map(|message| {
            let text = message
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected a delivered event to carry its id as text, got \
                         {message:?}"
                    )
                });
            let id = text.parse::<i64>().unwrap_or_else(|error| {
                panic!("case {case:?}: expected a whole id, got {text:?} ({error})")
            });
            let kind = message
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no type on {message:?}"))
                .to_string();
            (id, kind)
        })
        .collect()
}

/// The messages `group` holds once every run in `runs` has delivered at least `wanted`
/// events, polled exactly as a client watching the group polls it.
async fn delivered(
    observed: &Observed,
    group: &str,
    case: &str,
    runs: &[String],
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let count = runs.len();
    loop {
        let messages = crate::vended_log_event_members::delivered(observed, case, group).await;
        if runs
            .iter()
            .all(|arn| sequence(&messages, arn, case).len() >= wanted)
        {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} events for each of {count} runs in {group}, got \
             {messages:?} within {DELIVERY_DEADLINE:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The `(id, type)` pairs `GetExecutionHistory` reports for `arn`.
async fn history(observed: &Observed, arn: &str, case: &str) -> Vec<(i64, String)> {
    let answered = observed
        .ok(
            "GetExecutionHistory",
            &json!({ "executionArn": arn, "includeExecutionData": true }),
        )
        .await;
    let events = answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        !events.is_empty(),
        "case {case:?}: expected the STANDARD twin to have a history, got {answered:?}"
    );
    events
        .iter()
        .map(|event| {
            (
                event.get("id").and_then(Value::as_i64).unwrap_or_default(),
                event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The whole history of the run `arn`, waited for until the outcome the clock owed it has
/// come due — which is the event sequence the delivery of the same definition must carry.
async fn whole_history(observed: &Observed, arn: &str, case: &str) -> Vec<(i64, String)> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let reported = history(observed, arn, case).await;
        let settled = reported
            .iter()
            .any(|(_, kind)| matches!(kind.as_str(), "ExecutionSucceeded" | "ExecutionFailed"));
        if settled {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the STANDARD twin to reach its outcome, got {reported:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Start one run, answering the execution ARN it was given.
async fn start(observed: &Observed, machine: &str, name: Option<&str>, input: &str) -> String {
    let mut request = json!({ "stateMachineArn": machine, "input": input });
    if let Some(name) = name {
        request["name"] = json!(name);
    }
    let started = observed.ok("StartExecution", &request).await;
    assert!(
        started.get("status").is_none(),
        "an asynchronous start reports no status, got {started:?}"
    );
    string(&started, "executionArn")
}

/// Every run of an EXPRESS workflow delivers its whole history to the configured group,
/// with the sequence decided by the run and not by `includeExecutionData` — which decides
/// only the payload — while the run's own ARN is refused by the operations that report on
/// an execution, so the delivery is the only record there is.
#[tokio::test]
async fn every_express_run_delivers_the_whole_history_its_twin_arms_agree_on() {
    let observed = Observed::new();
    let refusal = resource_type_refusal(EXPRESS_EXECUTION_RESOURCE);

    let rows: Vec<Row> = vec![
        Row {
            case: "a run that succeeds through one Pass state",
            definition: HELLO_DEFINITION,
        },
        Row {
            case: "a run that fails at once",
            definition: FAILING_DEFINITION,
        },
        Row {
            case: "a run that chains two Pass states",
            definition: CHAINED_DEFINITION,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Row { case, definition } = row;
        // A distinct input per row, so the data a delivered event carries is this row's
        // own and not a value that happens to be there. `marker` is that document as a
        // delivered message carries it: escaped inside the message's own JSON string.
        let input = format!("{{\"row\":{index}}}");
        let marker = format!("\\\"row\\\":{index}");

        // The sequence the arms must deliver is the one the history API reports for the
        // same definition and input, read off a STANDARD twin — the workflow type whose
        // executions that API serves.
        let standard = create(
            &observed,
            &format!("twin-standard-{index}"),
            definition,
            "STANDARD",
            None,
        )
        .await;
        let reference = start(&observed, &standard, Some("reference"), &input).await;
        let expected = whole_history(&observed, &reference, case).await;

        // The twin EXPRESS arms: the same definition and level, differing only in whether
        // the configuration asks for the execution data.
        for (arm, include) in [("with its data", true), ("without its data", false)] {
            let case = format!("{case}, logged {arm}");
            let group = format!("/aws/vendedlogs/states/twin-{index}-{include}");
            observed
                .logs("CreateLogGroup", &json!({ "logGroupName": group }))
                .await;
            let machine_name = format!("twin-express-{index}-{include}");
            let machine = create(
                &observed,
                &machine_name,
                definition,
                "EXPRESS",
                Some(logging(observability::LOG_LEVEL_ALL, &group, include)),
            )
            .await;

            // Three runs of the one state machine, every one of them started before
            // anything is read: the delivery is owed for all of them, not for the first
            // alone.
            let mut runs = Vec::new();
            for name in RUN_NAMES {
                runs.push(start(&observed, &machine, *name, &input).await);
            }
            assert_eq!(
                runs.iter().collect::<BTreeSet<_>>().len(),
                runs.len(),
                "case {case:?}: each run of an EXPRESS workflow is named by its own ARN, got \
                 {runs:?}"
            );
            for arn in &runs {
                let fields = resource_fields(&case, arn);
                assert_eq!(
                    (
                        fields.first().map(String::as_str),
                        fields.get(1).map(String::as_str)
                    ),
                    (
                        Some(EXPRESS_EXECUTION_RESOURCE),
                        Some(machine_name.as_str())
                    ),
                    "case {case:?}: expected the express ARN of the state machine the run ran, \
                     got {arn:?}"
                );
            }

            // From here on the arm makes no Step Functions call about these runs: what it
            // reads is what a client watching the log group reads.
            let messages = delivered(&observed, &group, &case, &runs, expected.len()).await;
            for (run, arn) in runs.iter().enumerate() {
                let case = format!("{case}, run {run}");
                let delivered_sequence = sequence(&messages, arn, &case);
                assert_eq!(
                    delivered_sequence, expected,
                    "case {case:?}: expected the run's whole history, as the history API reports \
                     it for the same definition, got {delivered_sequence:?}"
                );
                let ids: Vec<i64> = delivered_sequence.iter().map(|(id, _)| *id).collect();
                let contiguous: Vec<i64> = (1..=ids.len() as i64).collect();
                assert_eq!(
                    ids, contiguous,
                    "case {case:?}: expected the contiguous ids 1..N in history order, got {ids:?}"
                );
            }

            // The payload is the only difference between the arms: a configuration that
            // excludes the execution data delivers none of those members at all, and one
            // that asks for it delivers the document the run was started with.
            let carried: Vec<&str> = messages
                .iter()
                .filter_map(|message| message.get("details"))
                .filter_map(Value::as_object)
                .flat_map(|details| {
                    PAYLOAD_MEMBERS
                        .iter()
                        .filter(|member| details.contains_key(**member))
                        .copied()
                })
                .collect();
            assert_eq!(
                !carried.is_empty(),
                include,
                "case {case:?}: expected the payload members to be delivered={include}, got \
                 {carried:?} in {messages:?}"
            );
            let carries_input = messages
                .iter()
                .any(|message| message.to_string().contains(&marker));
            assert_eq!(
                carries_input, include,
                "case {case:?}: expected the document the run was started with to be \
                 delivered={include}, got {messages:?}"
            );

            // One stream per execution, each naming the run it carries, so a reader can
            // tell the three runs apart from the stream alone.
            let described = observed
                .logs("DescribeLogStreams", &json!({ "logGroupName": group }))
                .await;
            let streams: Vec<String> = described
                .get("logStreams")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|stream| stream.get("logStreamName").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            assert_eq!(
                streams.len(),
                runs.len(),
                "case {case:?}: expected one stream per run, got {described:?}"
            );
            for arn in &runs {
                let name = resource_fields(&case, arn)
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| panic!("case {case:?}: no execution name in {arn:?}"));
                assert!(
                    streams.iter().any(|stream| stream.ends_with(&name)),
                    "case {case:?}: expected a stream naming the run {name:?}, got {streams:?}"
                );
            }

            // The delivery is the ONLY record: the operations that report on an execution
            // refuse the express ARN, so nothing else could have answered for the run.
            for arn in &runs {
                for operation in REPORTING_OPERATIONS {
                    let (status, answered) = observed
                        .call(operation, &json!({ "executionArn": arn }))
                        .await;
                    let code = error_code(&answered);
                    let message = answered
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    assert_eq!(
                        (status, code.as_str(), message),
                        (CLIENT_REFUSAL_STATUS, INVALID_ARN, refusal.as_str()),
                        "case {case:?}: expected {operation} to refuse the express ARN, got \
                         {status} {code} with {answered:?}"
                    );
                }
            }
        }
    }
}
