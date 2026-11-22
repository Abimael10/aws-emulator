// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WHOLE trigger lifecycle a deployment drives in one sequence:
//! `CreateTrigger`, the `GetTrigger` that reads the registered definition back,
//! `StartTrigger`, the `GetTrigger` that reports the state the start left behind,
//! `UpdateTrigger`, and `StopTrigger`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour over state the store really holds.
//!
//! The rule under test: a trigger's definition is registered as written and the entity each
//! `Action$JobName` names is resolved only when the trigger FIRES ("the name of a job to be
//! run"), so a create that names a job this account does not hold yet is SERVED — the
//! ordering a deployment uses, declaring its triggers before the jobs they run — and every
//! later call of the lifecycle is served against the trigger it stored: the read reports the
//! stored definition in the `TriggerState` a create leaves a trigger in (`CREATED`), a start
//! answers the name it started and leaves a trigger that waits on something outside the
//! caller in the one armed state (`ACTIVATED`), an update overwrites the definition and
//! answers the resulting `Trigger` without moving that state, and a stop answers the name it
//! stopped and leaves the trigger in the one stopped state (`DEACTIVATED`).
//!
//! The rows differ along what the rule generalises over: the trigger's `Type` (each of the
//! four modelled `TriggerType` values), the member its type waits on (a cron `Schedule`, a
//! `Predicate`, an `EventBatchingCondition`, nothing at all), the form of the cron expression
//! (an explicit instant, wildcard fields, a step and a range), how many actions the
//! definition names, whether the trigger belongs to a workflow, and which definition members
//! the update replaces.
//!
//! Nothing here registers the job an action names, which is the point: the whole sequence is
//! driven against a dangling job reference.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    TRIGGER_STATE_ACTIVATED, TRIGGER_STATE_CREATED, TRIGGER_STATE_DEACTIVATED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{assert_echoed, call, emulator, ok};

/// A job name no call in this suite registers: the dangling reference the whole lifecycle is
/// driven against.
const ABSENT_JOB: &str = "a-job-this-account-does-not-hold";

/// A second one, so a row can name another target in its update.
const OTHER_ABSENT_JOB: &str = "another-job-this-account-does-not-hold";

/// One row: the definition the create registers, the replacement the update writes, and the
/// state a start leaves a trigger of this type in.
struct LifecycleCase {
    /// What this row varies, named in every assertion it drives.
    case: &'static str,
    /// The `CreateTrigger` body, minus `Name` and `WorkflowName`. Every member of it is also
    /// a `Trigger` member, so the read must report it back unchanged.
    create: Value,
    /// Whether the trigger belongs to a workflow: the lifecycle holds either way.
    in_workflow: bool,
    /// The `TriggerUpdate` the update carries.
    update: Value,
    /// The `TriggerState` a read reports once the start has been served: `ACTIVATED` for a
    /// trigger that waits on something outside the caller, and the state it waits in for one
    /// that waits on nothing but a caller, since a start FIRES that one.
    state_after_start: &'static str,
}

/// The rows.
fn lifecycle_cases() -> Vec<LifecycleCase> {
    vec![
        LifecycleCase {
            // "This field is required when the trigger type is SCHEDULED": a cron expression
            // naming one explicit instant, the whole sequence driven against a job nothing
            // created.
            case: "scheduled on an explicit instant, one action, outside any workflow",
            create: json!({
                "Type": "SCHEDULED",
                "Schedule": "cron(0 0 1 1 ? 2099)",
                "Description": "the description the create registered",
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            in_workflow: false,
            update: json!({
                "Schedule": "cron(0 0 2 1 ? 2099)",
                "Description": "the description the update wrote",
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            state_after_start: TRIGGER_STATE_ACTIVATED,
        },
        LifecycleCase {
            // The same type, with the wildcard form the member's own documentation gives
            // ("to run something every day at 12:15 UTC, you would specify:
            // cron(15 12 * * ? *)"), inside a workflow, and an update that moves the action
            // onto another job that does not exist either.
            case: "scheduled on a wildcard expression, inside a workflow",
            create: json!({
                "Type": "SCHEDULED",
                "Schedule": "cron(15 12 * * ? 2099)",
                "Actions": [{ "JobName": ABSENT_JOB, "Timeout": 60 }],
            }),
            in_workflow: true,
            update: json!({
                "Schedule": "cron(45 23 * * ? 2099)",
                "Actions": [
                    { "JobName": OTHER_ABSENT_JOB },
                    { "JobName": ABSENT_JOB, "Arguments": { "--mode": "delta" } },
                ],
            }),
            state_after_start: TRIGGER_STATE_ACTIVATED,
        },
        LifecycleCase {
            // A step and a range in the expression's fields, and two actions from the start.
            case: "scheduled on a step and a range, two actions",
            create: json!({
                "Type": "SCHEDULED",
                "Schedule": "cron(0 */6 1-7 1,7 ? 2099)",
                "Description": "every six hours in the first week",
                "Actions": [
                    { "JobName": ABSENT_JOB },
                    { "JobName": OTHER_ABSENT_JOB },
                ],
            }),
            in_workflow: false,
            update: json!({
                "Schedule": "cron(30 1-5 ? 1 MON 2099)",
                "Description": "weekday mornings",
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            state_after_start: TRIGGER_STATE_ACTIVATED,
        },
        LifecycleCase {
            // "This field is required when the trigger type is CONDITIONAL": the predicate
            // waits on a job nothing created, which is late-bound exactly as an action's job
            // is.
            case: "conditional on a predicate naming an absent job",
            create: json!({
                "Type": "CONDITIONAL",
                "Description": "waits on a job that does not exist yet",
                "Predicate": {
                    "Conditions": [{
                        "LogicalOperator": "EQUALS",
                        "JobName": OTHER_ABSENT_JOB,
                        "State": "SUCCEEDED",
                    }],
                },
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            in_workflow: true,
            update: json!({
                "Predicate": {
                    "Logical": "ANY",
                    "Conditions": [
                        {
                            "LogicalOperator": "EQUALS",
                            "JobName": OTHER_ABSENT_JOB,
                            "State": "FAILED",
                        },
                        {
                            "LogicalOperator": "EQUALS",
                            "JobName": OTHER_ABSENT_JOB,
                            "State": "TIMEOUT",
                        },
                    ],
                },
                "Description": "waits on either outcome",
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            state_after_start: TRIGGER_STATE_ACTIVATED,
        },
        LifecycleCase {
            // "Batch condition that must be met (specified number of events received or batch
            // time window expired) before EventBridge event trigger fires."
            case: "event batching a window of events",
            create: json!({
                "Type": "EVENT",
                "EventBatchingCondition": { "BatchSize": 3, "BatchWindow": 900 },
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            in_workflow: false,
            update: json!({
                "EventBatchingCondition": { "BatchSize": 1 },
                "Description": "one event is enough",
                "Actions": [{ "JobName": OTHER_ABSENT_JOB }],
            }),
            state_after_start: TRIGGER_STATE_ACTIVATED,
        },
        LifecycleCase {
            // "True is not supported for ON_DEMAND triggers": such a trigger waits on nothing
            // but a caller, so the start FIRES it and leaves it in the state it waits in. The
            // firing resolves the job its action names and finds none, so it begins no run
            // and the start is served all the same.
            case: "on-demand, waiting on nothing but a caller",
            create: json!({
                "Type": "ON_DEMAND",
                "Description": "fired by its caller",
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            in_workflow: false,
            update: json!({
                "Description": "fired by its caller, on another job",
                "Actions": [{ "JobName": OTHER_ABSENT_JOB }],
            }),
            state_after_start: TRIGGER_STATE_CREATED,
        },
    ]
}

/// The `Trigger` `GetTrigger` reports for `name`.
async fn get_trigger(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let (status, body) = call(router, "GetTrigger", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetTrigger must answer 200, got {status} with {body}"
    );
    body.get("Trigger")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetTrigger must report a Trigger, got {body}"))
}

/// Make one call of the lifecycle that must be served and answer its body, naming the case
/// and the call in the failure.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answer) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answer}"
    );
    answer
}

// ---------------------------------------------------------------------------
// The whole sequence, driven against a job the account does not hold
// ---------------------------------------------------------------------------

/// Create, read, start, read, update and stop, in one sequence per row: every call is served
/// and each one leaves the trigger in the state the next call reads.
#[tokio::test]
async fn the_whole_lifecycle_is_served_for_a_trigger_created_ahead_of_its_job() {
    let router = emulator();

    for (index, row) in lifecycle_cases().iter().enumerate() {
        let case = row.case;
        let name = format!("waiting-lifecycle-{index}");

        let mut create = row.create.clone();
        create["Name"] = json!(name);
        if row.in_workflow {
            let workflow = format!("waiting-lifecycle-workflow-{index}");
            ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
            create["WorkflowName"] = json!(workflow);
        }

        // 1. The create is served even though no action's job exists.
        let created = served(&router, case, "CreateTrigger", &create).await;
        assert_eq!(
            created.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: the create answers the name of the trigger it registered, got \
             {created}"
        );

        // 2. The read reports the stored definition, in the state a create leaves behind.
        let registered = get_trigger(&router, case, &name).await;
        assert_echoed(case, &create, &registered);
        assert_eq!(
            registered.get("State").and_then(Value::as_str),
            Some(TRIGGER_STATE_CREATED),
            "case {case:?}: a trigger the create did not start is registered in \
             {TRIGGER_STATE_CREATED}, got {registered}"
        );

        // 3. The start answers the name it started and leaves the state its type is left in.
        let started = served(&router, case, "StartTrigger", &json!({ "Name": name })).await;
        assert_eq!(
            started.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: the start answers the name of the trigger it started, got {started}"
        );
        let armed = get_trigger(&router, case, &name).await;
        assert_eq!(
            armed.get("State").and_then(Value::as_str),
            Some(row.state_after_start),
            "case {case:?}: expected State {} after the start, got {armed}",
            row.state_after_start
        );

        // 4. The update answers the resulting definition and moves no state.
        let answered = served(
            &router,
            case,
            "UpdateTrigger",
            &json!({ "Name": name, "TriggerUpdate": row.update }),
        )
        .await;
        let updated = answered.get("Trigger").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: the update must answer the resulting Trigger, got {answered}")
        });
        assert_echoed(case, &row.update, &updated);
        assert_eq!(
            updated.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: the updated trigger keeps the name it was created under, got \
             {updated}"
        );
        assert_eq!(
            updated.get("Type"),
            row.create.get("Type"),
            "case {case:?}: the update carries no Type, so the create's stands, got {updated}"
        );
        assert_eq!(
            updated.get("State").and_then(Value::as_str),
            Some(row.state_after_start),
            "case {case:?}: an update is neither a start nor a stop, so State stays {}, got \
             {updated}",
            row.state_after_start
        );
        // What the update answered is what the store holds.
        let read_back = get_trigger(&router, case, &name).await;
        assert_eq!(
            read_back, updated,
            "case {case:?}: the read must report the definition the update answered, got \
             {read_back} for {updated}"
        );

        // 5. The stop answers the name it stopped and deactivates the trigger.
        let stopped = served(&router, case, "StopTrigger", &json!({ "Name": name })).await;
        assert_eq!(
            stopped.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: the stop answers the name of the trigger it stopped, got {stopped}"
        );
        let deactivated = get_trigger(&router, case, &name).await;
        assert_eq!(
            deactivated.get("State").and_then(Value::as_str),
            Some(TRIGGER_STATE_DEACTIVATED),
            "case {case:?}: a stopped trigger is in {TRIGGER_STATE_DEACTIVATED}, got \
             {deactivated}"
        );
        // Only the state moved: the definition the update left is still the stored one.
        for member in ["Name", "Type", "Description", "Schedule", "Actions", "Predicate"] {
            assert_eq!(
                deactivated.get(member),
                updated.get(member),
                "case {case:?}: the stop must change only the state, and {member} differs: \
                 {deactivated} for {updated}"
            );
        }
    }
}

/// Reading the trigger at each stage of the lifecycle reports the same answer however often
/// it is read: a read neither starts, stops nor fires the trigger.
#[tokio::test]
async fn reading_the_trigger_at_each_stage_moves_nothing() {
    let router = emulator();

    for (index, row) in lifecycle_cases().iter().enumerate() {
        let case = row.case;
        let name = format!("repeated-read-{index}");
        let mut create = row.create.clone();
        create["Name"] = json!(name);
        // The workflow axis is covered by the sequence above; here every row is created
        // outside any workflow, so no read can be answered from a run a firing began.
        served(&router, case, "CreateTrigger", &create).await;

        // (the stage, the call that reaches it, the state every read of it must report)
        let stages: [(&str, Option<&str>, &str); 3] = [
            ("after the create", None, TRIGGER_STATE_CREATED),
            ("after the start", Some("StartTrigger"), row.state_after_start),
            ("after the stop", Some("StopTrigger"), TRIGGER_STATE_DEACTIVATED),
        ];
        for (stage, reached_by, expected) in stages {
            if let Some(operation) = reached_by {
                served(&router, case, operation, &json!({ "Name": name })).await;
            }

            let first = get_trigger(&router, case, &name).await;
            assert_eq!(
                first.get("State").and_then(Value::as_str),
                Some(expected),
                "case {case:?}: {stage} the trigger must report {expected}, got {first}"
            );
            for read in 2..=4 {
                let again = get_trigger(&router, case, &name).await;
                assert_eq!(
                    again, first,
                    "case {case:?}: {stage} read {read} must report what the first read \
                     reported, got {again} for {first}"
                );
            }
        }
    }
}
