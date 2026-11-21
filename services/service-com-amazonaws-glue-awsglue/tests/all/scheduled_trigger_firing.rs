// SPDX-License-Identifier: CC0-1.0

//! What a SCHEDULED trigger DOES: the instants its own cron expression names, and the runs a
//! firing at one of them begins.
//!
//! The rule under test: a `SCHEDULED` trigger that is ARMED fires at the instants its own
//! `Schedule` names and at no others — "a `cron` expression used to specify the schedule […]
//! to run something every day at 12:15 UTC, you would specify: `cron(15 12 * * ? *)`"
//! (`Trigger$Schedule`) — and a trigger that is not armed fires at none of them, "set to true
//! to start SCHEDULED and CONDITIONAL triggers when created"
//! (`CreateTriggerRequest$StartOnCreation`) and `StartTrigger`/`StopTrigger` being what arm
//! and disarm one. Each firing initiates "the actions initiated by this trigger"
//! (`Trigger$Actions`): every action naming a job begins ONE run of that job, which
//! `GetJobRuns` then reports with the firing trigger as its `JobRun$TriggerName`, and a
//! trigger that belongs to a workflow also begins one run of it.
//!
//! The firing is owed by the CLOCK rather than by a call, at an instant the caller's own
//! expression names — minutes or hours ahead for every schedule a client would write — so the
//! suite splits the rule in two:
//!
//! * WHICH instant a schedule names is asserted over a TABLE of expressions, against the one
//!   reading of them the service fires on
//!   ([`service_com_amazonaws_glue_awsglue::state::next_scheduled_firing`]), with rows
//!   differing along the form of the expression and the instant it is read at;
//! * WHAT a firing at that instant does is asserted end to end over the wire, with the armed
//!   instant brought forward in the store rather than waited for — the same thing the suites
//!   that drive a stack's clock do — so no case depends on how fast the host runs or on which
//!   minute it happens to run in.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_glue_awsglue::state::{
    next_scheduled_firing, service_state, TRIGGER_STATE_ACTIVATED, TRIGGER_STATE_CREATED,
    TRIGGER_STATE_DEACTIVATED,
};

use crate::wire::{call, emulator_on, error_code, ok, REGION};

/// The job every trigger below acts on, so a firing has a run to begin.
const JOB: &str = "scheduled-firing-job";

/// A schedule whose instants are a DAY apart, so exactly one of them can fall due inside a
/// case: the example `Trigger$Schedule` itself gives ("to run something every day at 12:15
/// UTC, you would specify: `cron(15 12 * * ? *)`").
const DAILY_SCHEDULE: &str = "cron(15 12 * * ? *)";

/// How long a case waits for a firing the clock owes to show up, looking again until it has.
///
/// Generous: the condition is polled for, so this bounds only a firing that never happens.
const FIRING_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One emulator and the engine it holds.
fn given_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    (router, engine)
}

/// Register the job the trigger actions name.
async fn given_job(router: &MultiServiceRouter, name: &str) {
    ok(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": "arn:aws:iam::000000000000:role/glue-job-role",
            "Command": { "Name": "pythonshell", "ScriptLocation": "s3://scripts/noop.py" },
        }),
    )
    .await;
}

/// The `Trigger` document `GetTrigger` reports for `name`.
async fn trigger_of(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let read = ok(router, "GetTrigger", &json!({ "Name": name })).await;
    read.get("Trigger")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetTrigger reports a Trigger, got {read}"))
}

/// The runs `GetJobRuns` reports for `job`, as a client reads them.
async fn runs_of(router: &MultiServiceRouter, case: &str, job: &str) -> Vec<Value> {
    let (status, body) = call(router, "GetJobRuns", &json!({ "JobName": job })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetJobRuns must answer 200, got {status} with {body}"
    );
    body["JobRuns"].as_array().cloned().unwrap_or_default()
}

/// Bring the instant `trigger`'s arming put ahead of now forward to NOW, and answer whether
/// the trigger was armed at all.
///
/// This is how a case reaches a firing without waiting for the instant the caller's own
/// expression names: it moves the DUE instant and nothing else, so what the case then
/// observes is the service's own firing.
fn bring_the_firing_forward(
    engine: &Arc<engine::InMemoryEngine>,
    trigger: &str,
) -> Option<SystemTime> {
    let state = service_state(&**engine).expect("the Glue store is reachable");
    let mut held = state.lock();
    let entry = held
        .trigger_mut(REGION, trigger)
        .expect("the trigger was created");
    let armed = entry.scheduled_firing_due_at;
    if armed.is_some() {
        entry.scheduled_firing_due_at = Some(SystemTime::now());
    }
    armed
}

/// Poll `job`'s run history until it holds `expected` runs, or the deadline passes.
async fn runs_settle_to(
    router: &MultiServiceRouter,
    case: &str,
    job: &str,
    expected: usize,
) -> Vec<Value> {
    let deadline = SystemTime::now() + FIRING_DEADLINE;
    loop {
        let runs = runs_of(router, case, job).await;
        if runs.len() == expected {
            return runs;
        }
        assert!(
            SystemTime::now() < deadline,
            "case {case:?}: expected {expected} run(s) of {job:?}, got {} ({runs:?})",
            runs.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// WHICH instants a schedule names, over a TABLE of expressions.
///
/// Every row is read through the one reading the service fires on, and the expected instant is
/// the first one the expression names strictly after the instant it is read at. The rows differ
/// along the form of the expression (a wildcard minute, a step, a fixed daily time, a range of
/// weekdays, a named year) and along where in the period the reading instant falls — including
/// one exactly ON an instant the expression names, which the NEXT one follows.
///
/// The forms come from the six-field layout `Trigger$Schedule` documents (`[Minutes] [Hours]
/// [Day_of_Month] [Month] [Day_of_Week] [Year]`), whose own example is the daily row.
#[test]
fn a_schedule_names_the_instants_it_fires_at() {
    // (case, the expression, the instant it is read at, the instant it names next)
    let rows: [(&str, &str, &str, Option<&str>); 8] = [
        (
            "every minute, read inside a minute",
            "cron(* * * * ? *)",
            "2026-01-02T03:04:05Z",
            Some("2026-01-02T03:05:00Z"),
        ),
        (
            "every minute, read exactly on an instant it names",
            "cron(* * * * ? *)",
            "2026-01-02T03:04:00Z",
            Some("2026-01-02T03:05:00Z"),
        ),
        (
            "every five minutes from zero",
            "cron(0/5 * * * ? *)",
            "2026-01-02T03:04:05Z",
            Some("2026-01-02T03:05:00Z"),
        ),
        (
            "every five minutes, read on one of them",
            "cron(0/5 * * * ? *)",
            "2026-01-02T03:05:00Z",
            Some("2026-01-02T03:10:00Z"),
        ),
        (
            // The example the member's own documentation gives.
            "every day at 12:15 UTC, read after it",
            "cron(15 12 * * ? *)",
            "2026-01-02T13:00:00Z",
            Some("2026-01-03T12:15:00Z"),
        ),
        (
            // A Saturday, so the next weekday instant is the Monday after it.
            "weekday mornings, read on a Saturday",
            "cron(30 9 ? * MON-FRI *)",
            "2026-01-03T00:00:00Z",
            Some("2026-01-05T09:30:00Z"),
        ),
        (
            // A year already past names no instant ahead at all, so the trigger fires no
            // more.
            "a year already past",
            "cron(0 0 1 1 ? 2020)",
            "2026-01-02T03:04:05Z",
            None,
        ),
        (
            // Not the `cron(...)` form the member documents, so it names nothing: such a
            // value is refused by the write that would have stored it.
            "a rate expression, which this member is not written in",
            "rate(5 minutes)",
            "2026-01-02T03:04:05Z",
            None,
        ),
    ];

    for (case, expression, read_at, expected) in rows {
        let read_at = instant(read_at);
        let named = next_scheduled_firing(expression, read_at);
        match expected {
            Some(expected) => {
                let expected = instant(expected);
                assert_eq!(
                    named,
                    Some(expected),
                    "case {case:?}: expected {expression:?} to name {}, got {}",
                    rendered(Some(expected)),
                    rendered(named)
                );
                assert!(
                    named.is_some_and(|named| named > read_at),
                    "case {case:?}: an instant a schedule names next is strictly ahead of the \
                     instant it is read at, got {}",
                    rendered(named)
                );
            }
            None => assert_eq!(
                named,
                None,
                "case {case:?}: expected {expression:?} to name no instant ahead, got {}",
                rendered(named)
            ),
        }
    }
}

/// An RFC 3339 instant as the clock holds it.
fn instant(text: &str) -> SystemTime {
    chrono::DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|error| panic!("{text:?} is an instant: {error}"))
        .with_timezone(&chrono::Utc)
        .into()
}

/// An instant as this suite names one, for an assertion's message.
fn rendered(instant: Option<SystemTime>) -> String {
    instant.map_or_else(
        || "no instant".to_string(),
        |instant| {
            chrono::DateTime::<chrono::Utc>::from(instant)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        },
    )
}

/// One row of the firing table: how the trigger comes to be armed (or not), and what the
/// firing its schedule owes must then have begun.
struct Row {
    case: &'static str,
    /// Whether the create starts the trigger.
    start_on_creation: bool,
    /// Whether the case stops the trigger after creating it.
    stop_after_create: bool,
    /// Whether the case starts it through `StartTrigger` after creating it.
    start_after_create: bool,
    /// The `TriggerState` every read must then report.
    expected_state: &'static str,
    /// Whether the trigger is on its schedule, so the clock owes it a firing at all.
    armed: bool,
}

/// The rows: the four ways a scheduled trigger comes to be armed or not.
fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: a trigger the create started.
            case: "started on creation",
            start_on_creation: true,
            stop_after_create: false,
            start_after_create: false,
            expected_state: TRIGGER_STATE_ACTIVATED,
            armed: true,
        },
        Row {
            case: "created idle and started afterwards",
            start_on_creation: false,
            stop_after_create: false,
            start_after_create: true,
            expected_state: TRIGGER_STATE_ACTIVATED,
            armed: true,
        },
        Row {
            case: "created idle and left alone",
            start_on_creation: false,
            stop_after_create: false,
            start_after_create: false,
            expected_state: TRIGGER_STATE_CREATED,
            armed: false,
        },
        Row {
            case: "started on creation and then stopped",
            start_on_creation: true,
            stop_after_create: true,
            start_after_create: false,
            expected_state: TRIGGER_STATE_DEACTIVATED,
            armed: false,
        },
        Row {
            case: "stopped and then started again",
            start_on_creation: true,
            stop_after_create: true,
            start_after_create: true,
            expected_state: TRIGGER_STATE_ACTIVATED,
            armed: true,
        },
    ]
}

/// An ARMED scheduled trigger is owed a firing at an instant its own expression names, and a
/// trigger that is not armed is owed none — over the table of ways one comes to be armed.
#[tokio::test]
async fn only_an_armed_scheduled_trigger_is_owed_a_firing() {
    for row in rows() {
        let (router, engine) = given_emulator();
        given_job(&router, JOB).await;
        let trigger = format!("sch-{}", row.case.replace(' ', "-"));
        let created_at = SystemTime::now();
        ok(
            &router,
            "CreateTrigger",
            &json!({
                "Name": trigger,
                "Type": "SCHEDULED",
                "Schedule": DAILY_SCHEDULE,
                "StartOnCreation": row.start_on_creation,
                "Actions": [{ "JobName": JOB }],
            }),
        )
        .await;
        if row.stop_after_create {
            ok(&router, "StopTrigger", &json!({ "Name": trigger })).await;
        }
        if row.start_after_create {
            ok(&router, "StartTrigger", &json!({ "Name": trigger })).await;
        }

        let read = trigger_of(&router, row.case, &trigger).await;
        assert_eq!(
            read.get("State").and_then(Value::as_str),
            Some(row.expected_state),
            "case {}: expected State {:?}, got {read}",
            row.case,
            row.expected_state
        );

        // Whether the clock owes this trigger a firing, and WHEN: the instant is the first one
        // its own expression names after it was armed, which is the reading the table above
        // pins.
        let armed = {
            let state = service_state(&*engine).expect("the Glue store is reachable");
            let held = state.lock();
            held.trigger(REGION, &trigger)
                .expect("the trigger was created")
                .scheduled_firing_due_at
        };
        if row.armed {
            let earliest = next_scheduled_firing(DAILY_SCHEDULE, created_at)
                .expect("the daily schedule names an instant ahead");
            assert!(
                armed.is_some_and(|armed| armed >= earliest),
                "case {}: expected a firing owed at an instant the schedule names (no earlier \
                 than {}), got {}",
                row.case,
                rendered(Some(earliest)),
                rendered(armed)
            );
        } else {
            assert_eq!(
                armed,
                None,
                "case {}: a trigger that is not armed is owed no firing, got {}",
                row.case,
                rendered(armed)
            );
        }

        // And nothing has fired yet either way: the instant the expression names is a day
        // ahead.
        let runs = runs_of(&router, row.case, JOB).await;
        assert!(
            runs.is_empty(),
            "case {}: no instant of the schedule has come due, so no run has begun, got \
             {runs:?}",
            row.case
        );
    }
}

/// The firing itself: at the instant its schedule names, an armed trigger begins ONE run of
/// each job its actions name, each reporting the trigger that began it — and a trigger that is
/// not armed begins none even once that instant has passed.
#[tokio::test]
async fn a_firing_begins_one_run_of_each_job_the_trigger_acts_on() {
    for row in rows() {
        let (router, engine) = given_emulator();
        let second_job = format!("{JOB}-two");
        given_job(&router, JOB).await;
        given_job(&router, &second_job).await;
        let trigger = format!("fire-{}", row.case.replace(' ', "-"));
        ok(
            &router,
            "CreateTrigger",
            &json!({
                "Name": trigger,
                "Type": "SCHEDULED",
                "Schedule": DAILY_SCHEDULE,
                "StartOnCreation": row.start_on_creation,
                // Two actions, so a firing is shown to begin one run of EACH job it names.
                "Actions": [{ "JobName": JOB }, { "JobName": second_job }],
            }),
        )
        .await;
        if row.stop_after_create {
            ok(&router, "StopTrigger", &json!({ "Name": trigger })).await;
        }
        if row.start_after_create {
            ok(&router, "StartTrigger", &json!({ "Name": trigger })).await;
        }

        let armed = bring_the_firing_forward(&engine, &trigger);
        assert_eq!(
            armed.is_some(),
            row.armed,
            "case {}: expected armed={}, got {}",
            row.case,
            row.armed,
            rendered(armed)
        );
        if !row.armed {
            // Nothing is owed, so nothing fires however long the case looks.
            for job in [JOB, second_job.as_str()] {
                let runs = runs_of(&router, row.case, job).await;
                assert!(
                    runs.is_empty(),
                    "case {}: a trigger that is not armed begins no run of {job:?}, got \
                     {runs:?}",
                    row.case
                );
            }
            continue;
        }

        for job in [JOB, second_job.as_str()] {
            let runs = runs_settle_to(&router, row.case, job, 1).await;
            let run = &runs[0];
            assert_eq!(
                run.get("TriggerName").and_then(Value::as_str),
                Some(trigger.as_str()),
                "case {}: the run must name the trigger that began it, got {run}",
                row.case
            );
            assert_eq!(
                run.get("JobName").and_then(Value::as_str),
                Some(job),
                "case {}: the run must be a run of the job the action names, got {run}",
                row.case
            );
            assert!(
                run.get("Id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()),
                "case {}: the run carries a JobRunId, got {run}",
                row.case
            );
        }

        // ONE instant fires ONCE: the firing armed the NEXT instant the expression names, a
        // day ahead, so reading the history again reports no second run.
        for job in [JOB, second_job.as_str()] {
            let runs = runs_of(&router, row.case, job).await;
            assert_eq!(
                runs.len(),
                1,
                "case {}: one instant of the schedule begins one run of {job:?} and no more, \
                 got {runs:?}",
                row.case
            );
        }
        let rearmed = {
            let state = service_state(&*engine).expect("the Glue store is reachable");
            let held = state.lock();
            held.trigger(REGION, &trigger)
                .expect("the trigger was created")
                .scheduled_firing_due_at
        };
        assert!(
            rearmed.is_some_and(|rearmed| rearmed > SystemTime::now()),
            "case {}: a firing arms the next instant the expression names, got {}",
            row.case,
            rendered(rearmed)
        );
    }
}

/// A trigger that belongs to a WORKFLOW begins one run of it when it fires, beside the job
/// runs its actions begin — the run `GetWorkflowRuns` then reports.
#[tokio::test]
async fn a_firing_of_a_workflows_trigger_begins_a_run_of_that_workflow() {
    let case = "a scheduled entry point of a workflow";
    let (router, engine) = given_emulator();
    given_job(&router, JOB).await;
    let workflow = "scheduled-firing-workflow";
    ok(&router, "CreateWorkflow", &json!({ "Name": workflow })).await;
    let trigger = "scheduled-firing-entry";
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": trigger,
            "Type": "SCHEDULED",
            "Schedule": DAILY_SCHEDULE,
            "StartOnCreation": true,
            "WorkflowName": workflow,
            "Actions": [{ "JobName": JOB }],
        }),
    )
    .await;

    // Before the instant falls due the workflow has no run at all, which `GetWorkflowRuns`
    // answers as the modelled miss.
    let (status, body) = call(&router, "GetWorkflowRuns", &json!({ "Name": workflow })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: a workflow nothing has run has no run history, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "EntityNotFoundException",
        "case {case:?}: an empty run history is the modelled miss, got {body}"
    );

    assert!(
        bring_the_firing_forward(&engine, trigger).is_some(),
        "case {case:?}: the create must have armed the trigger's schedule"
    );
    let runs = runs_settle_to(&router, case, JOB, 1).await;
    assert_eq!(
        runs[0].get("TriggerName").and_then(Value::as_str),
        Some(trigger),
        "case {case:?}: the job run must name the trigger that began it, got {:?}",
        runs[0]
    );

    let read = ok(&router, "GetWorkflowRuns", &json!({ "Name": workflow })).await;
    let workflow_runs = read["Runs"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: GetWorkflowRuns reports a Runs list, got {read}"));
    assert_eq!(
        workflow_runs.len(),
        1,
        "case {case:?}: the firing begins ONE run of the workflow it belongs to, got \
         {workflow_runs:?}"
    );
    assert_eq!(
        workflow_runs[0].get("Name").and_then(Value::as_str),
        Some(workflow),
        "case {case:?}: the run names the workflow it belongs to, got {:?}",
        workflow_runs[0]
    );
    assert!(
        workflow_runs[0]
            .get("WorkflowRunId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "case {case:?}: the run carries a WorkflowRunId, got {:?}",
        workflow_runs[0]
    );
}

/// An update overwrites the schedule a trigger fires on: "this object updates the previous
/// trigger definition by overwriting it completely" (`TriggerUpdate`), so the firing the clock
/// owes is the first instant the schedule NOW in force names.
#[tokio::test]
async fn an_update_moves_the_firing_onto_the_schedule_it_states() {
    let case = "a redeclared schedule";
    let (router, engine) = given_emulator();
    given_job(&router, JOB).await;
    let trigger = "scheduled-firing-updated";
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": trigger,
            "Type": "SCHEDULED",
            "Schedule": DAILY_SCHEDULE,
            "StartOnCreation": true,
            "Actions": [{ "JobName": JOB }],
        }),
    )
    .await;

    // A schedule whose instants are a year apart, so the firing the update owes is far from
    // the one the create owed.
    let replacement = "cron(0 0 1 1 ? *)";
    let updated_at = SystemTime::now();
    ok(
        &router,
        "UpdateTrigger",
        &json!({
            "Name": trigger,
            "TriggerUpdate": {
                "Schedule": replacement,
                "Actions": [{ "JobName": JOB }],
            },
        }),
    )
    .await;

    let read = trigger_of(&router, case, trigger).await;
    assert_eq!(
        read.get("Schedule").and_then(Value::as_str),
        Some(replacement),
        "case {case:?}: the stored schedule is the one the update states, got {read}"
    );
    let armed = {
        let state = service_state(&*engine).expect("the Glue store is reachable");
        let held = state.lock();
        held.trigger(REGION, trigger)
            .expect("the trigger was created")
            .scheduled_firing_due_at
    };
    let expected = next_scheduled_firing(replacement, updated_at)
        .expect("the replacement schedule names an instant ahead");
    assert!(
        armed.is_some_and(|armed| armed >= expected),
        "case {case:?}: expected the firing to be owed at an instant the NEW schedule names \
         (no earlier than {}), got {}",
        rendered(Some(expected)),
        rendered(armed)
    );
}
