// SPDX-License-Identifier: CC0-1.0

//! A rule that fires on its own SCHEDULE, end to end over the wire through the real
//! `MultiServiceRouter` with Step Functions mounted beside EventBridge — so the firing is
//! read back where a client reads it, in the executions the target state machine holds.
//!
//! RULE: "rules with `ScheduleExpressions` self-trigger based on the given schedule"
//! (`PutRule`) — a rule carrying one is triggered by nothing a client publishes, and
//! EventBridge invokes its targets on that cadence, handing each the scheduled-event
//! envelope it publishes itself: `aws.events` as the `source`, `Scheduled Event` as the
//! `detail-type`, an empty `detail`, the rule's own ARN as the one resource it names, and a
//! FRESH `id` on every firing. A target that declares `Input`, `InputPath` or an
//! `InputTransformer` is handed what that override states instead, exactly as it is for a
//! published event; one that declares none is handed the envelope. A `DISABLED` rule is
//! triggered by nothing at all.
//!
//! The rows differ along what the rule generalises over: the target's input configuration
//! (none, a constant `Input`, an `InputPath` selecting part of the envelope, an
//! `InputTransformer` rendering one) and whether the rule is enabled at all.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::target_dispatch::{
    emulator, events, execution_inputs, fired_within, given_state_machine, target_role, Emulator,
    REGION,
};

/// The cadence every row's rule declares: the shortest a `rate(...)` expression can state.
const SCHEDULE_EXPRESSION: &str = "rate(1 minute)";

/// The `source` EventBridge publishes a scheduled event under.
const SCHEDULED_SOURCE: &str = "aws.events";

/// The `detail-type` it publishes one under.
const SCHEDULED_DETAIL_TYPE: &str = "Scheduled Event";

/// How long a row waits for a firing it EXPECTS: the span this emulator holds a new schedule
/// rule for ([`service_com_amazonaws_eventbridge_awsevents::state::schedule::FIRST_FIRING_DELAY`])
/// and generous room for the clock's own tick, the delivery and however loaded the host running
/// every other suite beside this one is. A ceiling on patience, never a span anything here
/// counts on.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long the row that expects NO firing watches for one: several times over the span a new
/// rule's first firing falls after, so "nothing fired" is an observation rather than a race. Its
/// own budget, because a row waiting for something to happen and a row waiting to see nothing
/// stop for different reasons, and one of them must not wait out the other's.
const QUIET_WATCH: Duration = Duration::from_secs(20);

/// How long the wait for the SECOND firing is given: the rule's own cadence
/// ([`SCHEDULE_EXPRESSION`]) with the same generous room over it. The cadence is the rule's
/// figure; this is only the ceiling on how long the firing it owes is waited for.
const CADENCE_PATIENCE: Duration = Duration::from_secs(180);

/// The role every target names, which EventBridge acts as to start an execution, and the
/// readers of what a firing handed one, are the whole crate's own
/// ([`crate::target_dispatch`]): the two schedule suites read a firing the same way.
///
/// Write a rule that fires on [`SCHEDULE_EXPRESSION`] with `target` as its only target.
async fn given_schedule_rule(emulator: &Emulator, name: &str, state: &str, target: Value) {
    let (status, written) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "ScheduleExpression": SCHEDULE_EXPRESSION,
            "State": state,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: PutRule answered {status} {written}"
    );
    let (status, targeted) = events(
        emulator,
        "PutTargets",
        &json!({ "Rule": name, "Targets": [target] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: PutTargets answered {status} {targeted}"
    );
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {name}: PutTargets reported a failed entry: {targeted}"
    );
}

/// [`fired_within`] over the budget a firing this suite EXPECTS is waited for.
async fn fired(emulator: &Emulator, arn: &str) -> Vec<String> {
    fired_within(emulator, arn, PATIENCE).await
}

/// What a row expects the target to have been handed.
enum Expected {
    /// EventBridge's own scheduled-event envelope.
    Envelope,
    /// The exact text the target's own override states.
    Text(&'static str),
    /// Nothing: the rule fires for nobody.
    Nothing,
}

/// One row: how the rule's one target is configured, and what a firing hands it.
struct Row {
    case: &'static str,
    /// The rule's `State`.
    state: &'static str,
    /// The members the target declares beside its `Id`, `Arn` and `RoleArn`.
    overrides: fn() -> Value,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "a target declaring no input override is handed the envelope",
        state: "ENABLED",
        overrides: || json!({}),
        expected: Expected::Envelope,
    },
    Row {
        case: "a target declaring a constant Input is handed that text",
        state: "ENABLED",
        overrides: || json!({ "Input": r#"{"poll":"now"}"# }),
        expected: Expected::Text(r#"{"poll":"now"}"#),
    },
    Row {
        case: "a target declaring an InputPath is handed the part it selects",
        state: "ENABLED",
        overrides: || json!({ "InputPath": "$.source" }),
        expected: Expected::Text(r#""aws.events""#),
    },
    Row {
        case: "a target declaring an InputTransformer is handed its rendering",
        state: "ENABLED",
        overrides: || {
            json!({
                "InputTransformer": {
                    "InputPathsMap": { "kind": "$.detail-type" },
                    "InputTemplate": r#"{"was":<kind>}"#,
                },
            })
        },
        expected: Expected::Text(r#"{"was":"Scheduled Event"}"#),
    },
    Row {
        case: "a DISABLED rule fires for nobody",
        state: "DISABLED",
        overrides: || json!({}),
        expected: Expected::Nothing,
    },
];

/// Every row: the rule fires on its own schedule, with nothing published, and what its
/// target was handed is what the rule's configuration says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_schedule_rule_fires_its_targets_with_the_scheduled_event() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let machine = given_state_machine(&emulator, &format!("scheduled-{index}")).await;
        let rule = format!("schedule-{index}");
        let mut target = json!({ "Id": "t1", "Arn": machine, "RoleArn": target_role() });
        for (member, value) in (row.overrides)().as_object().cloned().unwrap_or_default() {
            target[member] = value;
        }
        given_schedule_rule(&emulator, &rule, row.state, target).await;

        // A row that expects a firing waits the budget for one; the row that expects none
        // watches for its own, shorter span rather than waiting out the other's.
        let inputs = match row.expected {
            Expected::Nothing => fired_within(&emulator, &machine, QUIET_WATCH).await,
            Expected::Text(_) | Expected::Envelope => fired(&emulator, &machine).await,
        };
        match row.expected {
            Expected::Nothing => {
                assert!(
                    inputs.is_empty(),
                    "case {case:?}: expected no execution at all, got {inputs:?}"
                );
                continue;
            }
            Expected::Text(expected) => {
                assert_eq!(
                    inputs.first().map(String::as_str),
                    Some(expected),
                    "case {case:?}: expected the target's own override, got {inputs:?}"
                );
            }
            Expected::Envelope => {
                let first = inputs
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: no execution was started"));
                let envelope: Value = serde_json::from_str(first).unwrap_or_else(|error| {
                    panic!("case {case:?}: the input is not JSON ({error}): {first:?}")
                });
                assert_eq!(
                    envelope["source"].as_str(),
                    Some(SCHEDULED_SOURCE),
                    "case {case:?}: expected EventBridge's own source, got {envelope}"
                );
                assert_eq!(
                    envelope["detail-type"].as_str(),
                    Some(SCHEDULED_DETAIL_TYPE),
                    "case {case:?}: expected the scheduled detail-type, got {envelope}"
                );
                assert_eq!(
                    envelope["detail"],
                    json!({}),
                    "case {case:?}: expected an empty detail, got {envelope}"
                );
                assert_eq!(
                    envelope["region"].as_str(),
                    Some(REGION),
                    "case {case:?}: expected the rule's own Region, got {envelope}"
                );
                assert_eq!(
                    envelope["account"].as_str(),
                    Some(engine::emulator_account_id()),
                    "case {case:?}: expected the account's own id, got {envelope}"
                );
                let expected_resource = format!(
                    "arn:aws:events:{REGION}:{}:rule/{rule}",
                    engine::emulator_account_id()
                );
                assert_eq!(
                    envelope["resources"],
                    json!([expected_resource]),
                    "case {case:?}: expected the rule's own ARN as the one resource, got \
                     {envelope}"
                );
                assert!(
                    envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
                    "case {case:?}: expected an event id, got {envelope}"
                );
                assert!(
                    envelope["time"].as_str().is_some_and(|time| time.ends_with('Z')),
                    "case {case:?}: expected the firing's own instant, got {envelope}"
                );
            }
        }
    }
}

/// Two firings of one rule are two events: each carries its own `id`, and each starts its
/// own execution — so a client watching the target sees the cadence rather than one event
/// re-delivered.
///
/// The second firing is waited out through the rule's own cadence being made the shortest a
/// `rate` can state, which is why this is one case rather than a row of the table above: the
/// wait is the rule's own figure, not a span this emulator chooses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_firing_of_a_schedule_rule_carries_a_fresh_event_id() {
    let case = "two firings of one rule";
    let emulator = emulator();
    let machine = given_state_machine(&emulator, "fresh-id").await;
    given_schedule_rule(
        &emulator,
        "fresh-id",
        "ENABLED",
        json!({ "Id": "t1", "Arn": machine, "RoleArn": target_role() }),
    )
    .await;

    let first = fired(&emulator, &machine).await;
    assert_eq!(
        first.len(),
        1,
        "case {case:?}: expected exactly one firing to have happened, got {first:?}"
    );

    // The next firing is one cadence on. Poll until a second execution appears rather than
    // sleeping the whole period, so the assertion is about the firings and not the clock.
    let began = Instant::now();
    let inputs = loop {
        let inputs = execution_inputs(&emulator, &machine).await;
        if inputs.len() >= 2 {
            break inputs;
        }
        assert!(
            began.elapsed() < CADENCE_PATIENCE,
            "case {case:?}: expected a second firing one cadence on, got {inputs:?}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let ids: ::std::collections::BTreeSet<String> = inputs
        .iter()
        .map(|input| event_id(case, input))
        .collect();
    assert_eq!(
        ids.len(),
        inputs.len(),
        "case {case:?}: expected a fresh event id per firing, got {inputs:?}"
    );
}

/// The `id` of the envelope `input` carries.
fn event_id(case: &str, input: &str) -> String {
    let envelope: Value = serde_json::from_str(input)
        .unwrap_or_else(|error| panic!("case {case:?}: the input is not JSON ({error})"));
    envelope["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no id in {envelope}"))
        .to_string()
}

/// A rule put BEFORE its targets — the order `PutRule` and `PutTargets` being two calls makes
/// ordinary — fires once it has them, rather than only on the firing after the one its targets
/// missed.
///
/// A rule with no target reaches nobody, so there is nothing of such a firing to observe; what
/// a client does observe is the first firing after the targets are in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rule_put_before_its_targets_fires_once_it_has_them() {
    let case = "targets put after the rule";
    let emulator = emulator();
    let machine = given_state_machine(&emulator, "late-targets").await;
    let rule = "late-targets";
    let (status, written) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": rule,
            "ScheduleExpression": SCHEDULE_EXPRESSION,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {written}"
    );

    // Past the span a new rule's first firing falls after, with no target to deliver to.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        execution_inputs(&emulator, &machine).await.is_empty(),
        "case {case:?}: expected no firing while the rule had no target"
    );

    let (status, targeted) = events(
        &emulator,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{ "Id": "t1", "Arn": machine, "RoleArn": target_role() }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {targeted}"
    );

    let inputs = fired(&emulator, &machine).await;
    assert_eq!(
        inputs.len(),
        1,
        "case {case:?}: expected the first firing after the targets arrived, got {inputs:?}"
    );
}
