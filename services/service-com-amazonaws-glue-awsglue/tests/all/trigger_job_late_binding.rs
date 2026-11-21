// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the LATE BINDING of the job a Glue trigger's definition names.
//!
//! RULE: the job an `Action` runs, or a `Condition` waits on, is resolved when the trigger
//! FIRES and not when the trigger is written — `Action$JobName` is "the name of a job to be
//! run", and what runs it is the firing — so `CreateTrigger` and `UpdateTrigger` store the
//! name as given, `GetTrigger` reports it back unchanged with the state the write left the
//! trigger in, and a trigger may be written AHEAD of the job it will run. The firing then
//! resolves it: an action naming a job the Region holds begins a run of that job, and one
//! naming a job it does not hold begins none.
//!
//! The workflow a trigger belongs to and the crawler an action or a condition names are NOT
//! late-bound and keep their write-time resolution ([`crate::triggers`],
//! [`crate::trigger_updates`]).
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour over state the store really holds.
//!
//! The rows differ along what the rule generalises over: the trigger's `Type`, where the
//! dangling job name appears (one action, one of several actions, a predicate condition),
//! whether the write is a create or an update, and whether the job is created before or after
//! the trigger that names it.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{TRIGGER_STATE_CREATED, TRIGGER_TYPE_ON_DEMAND};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, ok};

/// A job name no create in this suite registers: the dangling reference under test.
const ABSENT_JOB: &str = "a-job-nothing-created";

/// A second one, so a row can leave more than one reference dangling.
const OTHER_ABSENT_JOB: &str = "another-job-nothing-created";

/// The job a row that mixes a held reference with a dangling one names.
const HELD_JOB: &str = "late-binding-held-job";

/// The crawler a row names where it needs an entity that IS resolved at write time.
const CRAWLER: &str = "late-binding-crawler";

/// A cron expression `Schedule` accepts, for the scheduled rows.
const SCHEDULE: &str = "cron(0 12 * * ? *)";

/// Register the job and the crawler the rows name when they need a HELD reference.
async fn register_held(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateJob",
        &json!({
            "Name": HELD_JOB,
            "Role": "arn:aws:iam::000000000000:role/GlueJobRole",
            "Command": { "Name": "glueetl" },
        }),
    )
    .await;
    ok(
        router,
        "CreateCrawler",
        &json!({
            "Name": CRAWLER,
            "Role": "arn:aws:iam::000000000000:role/GlueCrawlerRole",
            "DatabaseName": "late-binding-database",
            "Targets": { "S3Targets": [{ "Path": "s3://late-binding/data/" }] },
        }),
    )
    .await;
}

/// The `Trigger` a `GetTrigger` answer reports.
async fn get_trigger(router: &MultiServiceRouter, name: &str) -> Value {
    let read = ok(router, "GetTrigger", &json!({ "Name": name })).await;
    read.get("Trigger")
        .cloned()
        .unwrap_or_else(|| panic!("GetTrigger must report a Trigger, got {read}"))
}

/// Every job name the trigger's actions name, in action order.
fn action_job_names(trigger: &Value) -> Vec<String> {
    trigger
        .get("Actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("JobName").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// A create may name a job that does not exist yet
// ---------------------------------------------------------------------------

/// A `CreateTrigger` whose actions or conditions name a job this account does not hold is
/// SERVED: the trigger is stored with those names as sent, and `GetTrigger` reports them back
/// beside the state the create left it in.
#[tokio::test]
async fn a_create_may_name_a_job_that_does_not_exist_yet() {
    let router = emulator();
    register_held(&router).await;

    // (case, the definition the create sends, the JobNames its actions must report back)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "an on-demand trigger whose one action names an absent job",
            json!({ "Type": "ON_DEMAND", "Actions": [{ "JobName": ABSENT_JOB }] }),
            vec![ABSENT_JOB],
        ),
        (
            "a second action naming an absent job beside a held one",
            json!({
                "Type": "ON_DEMAND",
                "Actions": [{ "JobName": HELD_JOB }, { "JobName": ABSENT_JOB }],
            }),
            vec![HELD_JOB, ABSENT_JOB],
        ),
        (
            "two actions, both naming absent jobs",
            json!({
                "Type": "ON_DEMAND",
                "Actions": [{ "JobName": ABSENT_JOB }, { "JobName": OTHER_ABSENT_JOB }],
            }),
            vec![ABSENT_JOB, OTHER_ABSENT_JOB],
        ),
        (
            "a scheduled trigger whose action names an absent job",
            json!({
                "Type": "SCHEDULED",
                "Schedule": SCHEDULE,
                "Actions": [{ "JobName": ABSENT_JOB }],
            }),
            vec![ABSENT_JOB],
        ),
        (
            "an event trigger whose action names an absent job",
            json!({ "Type": "EVENT", "Actions": [{ "JobName": ABSENT_JOB }] }),
            vec![ABSENT_JOB],
        ),
        (
            "a conditional trigger whose condition waits on an absent job",
            json!({
                "Type": "CONDITIONAL",
                "Predicate": {
                    "Conditions": [{
                        "LogicalOperator": "EQUALS",
                        "JobName": ABSENT_JOB,
                        "State": "SUCCEEDED",
                    }],
                },
                "Actions": [{ "JobName": HELD_JOB }],
            }),
            vec![HELD_JOB],
        ),
        (
            "an action naming an absent job beside one naming a held crawler",
            json!({
                "Type": "ON_DEMAND",
                "Actions": [{ "JobName": ABSENT_JOB }, { "CrawlerName": CRAWLER }],
            }),
            vec![ABSENT_JOB],
        ),
    ];

    for (index, (case, definition, expected_jobs)) in cases.iter().enumerate() {
        let name = format!("late-bound-{index}");
        let mut request = definition.clone();
        request["Name"] = json!(name);

        let (status, body) = call(&router, "CreateTrigger", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: a trigger may be created ahead of the job it runs, got {status} \
             with {body}"
        );

        let stored = get_trigger(&router, &name).await;
        assert_eq!(
            action_job_names(&stored),
            *expected_jobs,
            "case {case}: expected the actions to report {expected_jobs:?}, got {stored}"
        );
        assert_eq!(
            stored.get("State").and_then(Value::as_str),
            Some(TRIGGER_STATE_CREATED),
            "case {case}: expected the state {TRIGGER_STATE_CREATED}, got {stored}"
        );
        assert_eq!(
            stored.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case}: expected the trigger to be read back under {name}, got {stored}"
        );
    }
}

/// An `UpdateTrigger` may leave the same dangling reference behind: the replacement
/// definition is stored as sent and read back unchanged.
#[tokio::test]
async fn an_update_may_name_a_job_that_does_not_exist_yet() {
    let router = emulator();
    register_held(&router).await;
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": "late-bound-update",
            "Type": TRIGGER_TYPE_ON_DEMAND,
            "Actions": [{ "JobName": HELD_JOB }],
        }),
    )
    .await;

    // (case, the TriggerUpdate, the JobNames the stored actions must report back)
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "an action moved onto a job nothing created",
            json!({ "Actions": [{ "JobName": ABSENT_JOB }] }),
            vec![ABSENT_JOB],
        ),
        (
            "one of two actions on a job nothing created",
            json!({ "Actions": [{ "JobName": HELD_JOB }, { "JobName": OTHER_ABSENT_JOB }] }),
            vec![HELD_JOB, OTHER_ABSENT_JOB],
        ),
        (
            "an action with the overrides a firing applies, on an absent job",
            json!({
                "Actions": [{
                    "JobName": ABSENT_JOB,
                    "Arguments": { "--mode": "delta" },
                    "Timeout": 45,
                }],
            }),
            vec![ABSENT_JOB],
        ),
    ];

    for (case, update, expected_jobs) in &cases {
        let (status, body) = call(
            &router,
            "UpdateTrigger",
            &json!({ "Name": "late-bound-update", "TriggerUpdate": update }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: an update may name a job the account does not hold, got {status} \
             with {body}"
        );
        let stored = get_trigger(&router, "late-bound-update").await;
        assert_eq!(
            action_job_names(&stored),
            *expected_jobs,
            "case {case}: expected the actions to report {expected_jobs:?}, got {stored}"
        );
    }
}

// ---------------------------------------------------------------------------
// The FIRING is what resolves the reference
// ---------------------------------------------------------------------------

/// A firing resolves the job reference: an on-demand trigger created ahead of its job runs
/// that job once the job exists, and a trigger whose action still names no job begins no run
/// and the start is served all the same.
#[tokio::test]
async fn the_firing_resolves_the_job_the_action_names() {
    let router = emulator();

    // The trigger is created FIRST, naming a job nothing has created yet — the ordering a
    // deployment uses.
    ok(
        &router,
        "CreateTrigger",
        &json!({
            "Name": "fires-a-late-bound-job",
            "Type": TRIGGER_TYPE_ON_DEMAND,
            "Actions": [{ "JobName": "late-arriving-job" }],
        }),
    )
    .await;
    // Starting it before the job exists initiates nothing, and the start is served.
    ok(
        &router,
        "StartTrigger",
        &json!({ "Name": "fires-a-late-bound-job" }),
    )
    .await;
    let (status, body) = call(
        &router,
        "GetJobRuns",
        &json!({ "JobName": "late-arriving-job" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case before the job exists: no run can be read of a job nothing created, got \
         {status} with {body}"
    );
    assert!(
        error_code(&body).contains("EntityNotFoundException"),
        "case before the job exists: expected EntityNotFoundException, got {body}"
    );

    // The job arrives, and the next firing of the SAME trigger runs it.
    ok(
        &router,
        "CreateJob",
        &json!({
            "Name": "late-arriving-job",
            "Role": "arn:aws:iam::000000000000:role/GlueJobRole",
            "Command": { "Name": "pythonshell" },
        }),
    )
    .await;
    ok(
        &router,
        "StartTrigger",
        &json!({ "Name": "fires-a-late-bound-job" }),
    )
    .await;
    let runs = ok(
        &router,
        "GetJobRuns",
        &json!({ "JobName": "late-arriving-job" }),
    )
    .await;
    let begun = runs
        .get("JobRuns")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    assert_eq!(
        begun, 1,
        "case after the job exists: the firing must begin exactly one run of it, got {runs}"
    );
    let trigger_name = runs
        .get("JobRuns")
        .and_then(Value::as_array)
        .and_then(|runs| runs.first())
        .and_then(|run| run.get("TriggerName"))
        .and_then(Value::as_str);
    assert_eq!(
        trigger_name,
        Some("fires-a-late-bound-job"),
        "case after the job exists: the run must name the trigger that began it, got {runs}"
    );
}
