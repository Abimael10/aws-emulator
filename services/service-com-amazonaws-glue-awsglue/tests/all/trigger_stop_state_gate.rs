// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the state gate `StopTrigger` answers from: the lifecycle a
//! deployment drives (create, read, stop, start, stop) and the refusal a stop earns when
//! the trigger it addresses is in a state no stop is defined from.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a stop is the transition OUT of the one `TriggerState` of a started
//! trigger (`ACTIVATED`), so `StopTrigger` is served only for a trigger IN that state; for a
//! trigger in any other state the call is refused with HTTP 400 and
//! `InvalidInputException`, whose `Message` is fixed text followed by the `TriggerState`
//! the addressed trigger is in AT THE TIME OF THE CALL — read off the stored record, so the
//! same request answers a different token as the trigger moves — and the refusal writes
//! nothing at all.
//!
//! The rows differ along what the rule generalises over: the trigger's `Type`, the member
//! its type waits on, the state the stop arrives at, and — over the published
//! `TriggerState` catalog — which state the message's variable part names.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::ops::types::TRIGGER_STATE_VALUES;
use service_com_amazonaws_glue_awsglue::state::{
    messages, ComAmazonawsGlueAwsglueState, TRIGGER_STATE_ACTIVATED, TRIGGER_STATE_CREATED,
    TRIGGER_STATE_DEACTIVATED,
};

use crate::error_messages::assert_refusal;
use crate::job_runs::emulator_with_engine;
use crate::trigger_starts::{register_targets, trigger_state, JOB, OTHER_JOB};
use crate::wire::{ok, REGION};

/// One row of the lifecycle table: the definition the create registers and the state a
/// start leaves a trigger of that type in.
struct GateCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `CreateTrigger` body, minus `Name`.
    trigger: Value,
}

/// The rows: the trigger types that HAVE an armed state to be stopped out of — a schedule
/// in two forms, a predicate, and an event batch — so each row's stop is refused before the
/// start and served after it.
fn gate_cases() -> Vec<GateCase> {
    vec![
        GateCase {
            // The recorded shape: "This field is required when the trigger type is
            // SCHEDULED."
            case: "scheduled on a daily cron expression",
            trigger: json!({
                "Type": "SCHEDULED",
                "Schedule": "cron(0 12 * * ? *)",
                "Actions": [{ "JobName": JOB }],
            }),
        },
        GateCase {
            case: "scheduled on a step expression",
            trigger: json!({
                "Type": "SCHEDULED",
                "Schedule": "cron(0 */6 1-7 1,7 ? *)",
                "Description": "every six hours in the first week",
                "Actions": [{ "JobName": JOB }],
            }),
        },
        GateCase {
            // "This field is required when the trigger type is CONDITIONAL." The predicate
            // waits on a job no action of this trigger runs: a conditional trigger may not
            // name one job in both places.
            case: "conditional on a job state",
            trigger: json!({
                "Type": "CONDITIONAL",
                "Predicate": {
                    "Conditions": [{
                        "LogicalOperator": "EQUALS",
                        "JobName": OTHER_JOB,
                        "State": "SUCCEEDED",
                    }],
                },
                "Actions": [{ "JobName": JOB }],
            }),
        },
        GateCase {
            // "Batch condition that must be met … before EventBridge event trigger fires."
            case: "event batching a window of events",
            trigger: json!({
                "Type": "EVENT",
                "EventBatchingCondition": { "BatchSize": 2, "BatchWindow": 900 },
                "Actions": [{ "JobName": JOB }],
            }),
        },
    ]
}

/// The lifecycle the recorded check drives, for every row: a trigger nothing has started is
/// refused the stop, naming the state it is registered in; the start then arms it; the stop
/// of the armed trigger is served and settles it in the stopped state; and the stop of the
/// stopped trigger is refused in turn, naming THAT state.
#[tokio::test]
async fn a_stop_is_served_only_from_the_started_state() {
    let (router, _engine) = emulator_with_engine();
    register_targets(&router).await;

    for (index, row) in gate_cases().iter().enumerate() {
        let case = row.case;
        let name = format!("stop-gate-{index}");
        let mut create = row.trigger.clone();
        create["Name"] = json!(name);
        ok(&router, "CreateTrigger", &create).await;

        // A create that did not ask for the trigger to be started registers it in the state
        // it waits in, which is no state a stop is defined from.
        let registered = trigger_state(&router, &name).await;
        assert_eq!(
            registered, TRIGGER_STATE_CREATED,
            "case {case:?}: the create registers the trigger in {TRIGGER_STATE_CREATED}, got \
             {registered:?}"
        );
        assert_refusal(
            &router,
            case,
            "StopTrigger",
            &json!({ "Name": name }),
            "InvalidInputException",
            &messages::trigger_state_not_allowed(TRIGGER_STATE_CREATED),
        )
        .await;
        let after_refusal = trigger_state(&router, &name).await;
        assert_eq!(
            after_refusal, TRIGGER_STATE_CREATED,
            "case {case:?}: the refused stop must leave the trigger in \
             {TRIGGER_STATE_CREATED}, got {after_refusal:?}"
        );

        // Started, the trigger is in the one state a stop IS defined from.
        ok(&router, "StartTrigger", &json!({ "Name": name })).await;
        let armed = trigger_state(&router, &name).await;
        assert_eq!(
            armed, TRIGGER_STATE_ACTIVATED,
            "case {case:?}: the start arms the trigger in {TRIGGER_STATE_ACTIVATED}, got \
             {armed:?}"
        );
        let stopped = ok(&router, "StopTrigger", &json!({ "Name": name })).await;
        assert_eq!(
            stopped.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: the stop answers the name of the trigger it stopped, got {stopped}"
        );
        let deactivated = trigger_state(&router, &name).await;
        assert_eq!(
            deactivated, TRIGGER_STATE_DEACTIVATED,
            "case {case:?}: the stop settles the trigger in {TRIGGER_STATE_DEACTIVATED}, got \
             {deactivated:?}"
        );

        // The same request now names the state the stop left behind: the token is read off
        // the record, not fixed to one state.
        assert_refusal(
            &router,
            case,
            "StopTrigger",
            &json!({ "Name": name }),
            "InvalidInputException",
            &messages::trigger_state_not_allowed(TRIGGER_STATE_DEACTIVATED),
        )
        .await;
        let still = trigger_state(&router, &name).await;
        assert_eq!(
            still, TRIGGER_STATE_DEACTIVATED,
            "case {case:?}: the refused stop must leave the trigger in \
             {TRIGGER_STATE_DEACTIVATED}, got {still:?}"
        );
    }
}

/// The refusal's variable part is the state the STORE holds, over every value the published
/// `TriggerState` catalog carries (source:
/// `service_com_amazonaws_glue_awsglue::ops::types::TRIGGER_STATE_VALUES`, generated from
/// the model's `TriggerState` enum): the one started state is served, and every other value
/// — the transient ones a client never asked for included — is refused with that value in
/// the message.
#[tokio::test]
async fn the_refusal_names_the_state_the_store_holds() {
    let (router, engine) = emulator_with_engine();
    register_targets(&router).await;
    let name = "stop-gate-catalog";
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": name,
            "Type": "SCHEDULED",
            "Schedule": "cron(0 12 * * ? *)",
            "Actions": [{ "JobName": JOB }],
        }),
    )
    .await;

    for state in TRIGGER_STATE_VALUES {
        // Put the stored record in the state this row is about, the way the service's own
        // transitions write it: the gate reads the record and nothing else.
        {
            let held = engine.state::<ComAmazonawsGlueAwsglueState>();
            let mut store = held.lock();
            let trigger = store
                .trigger_mut(REGION, name)
                .unwrap_or_else(|| panic!("case {state:?}: the store must hold {name:?}"));
            trigger.state = (*state).to_string();
        }

        if *state == TRIGGER_STATE_ACTIVATED {
            let stopped = ok(&router, "StopTrigger", &json!({ "Name": name })).await;
            assert_eq!(
                stopped.get("Name").and_then(Value::as_str),
                Some(name),
                "case {state:?}: the stop of a started trigger answers its name, got {stopped}"
            );
            let after = trigger_state(&router, name).await;
            assert_eq!(
                after, TRIGGER_STATE_DEACTIVATED,
                "case {state:?}: the served stop settles the trigger in \
                 {TRIGGER_STATE_DEACTIVATED}, got {after:?}"
            );
            continue;
        }

        assert_refusal(
            &router,
            state,
            "StopTrigger",
            &json!({ "Name": name }),
            "InvalidInputException",
            &messages::trigger_state_not_allowed(state),
        )
        .await;
        let after = trigger_state(&router, name).await;
        assert_eq!(
            after, *state,
            "case {state:?}: the refused stop must leave the state it found, got {after:?}"
        );
    }
}
