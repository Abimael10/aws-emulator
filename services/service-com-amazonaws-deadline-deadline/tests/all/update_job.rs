// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateJob`.
//!
//! The rules under test:
//!
//! * the operation is a PATCH of the job — only `farmId`, `queueId` and `jobId` are
//!   required, so every member the request carries replaces what the job held for it and
//!   every member it leaves out keeps the job's own value, which `GetJob` reads back;
//! * `targetTaskRunStatus` is the status to update the job's TASKS to, so the service works
//!   the job towards it: the job carries it as the target it is working to while the update
//!   is in progress, and the clock-owed move settles the job's run status on it;
//! * `lifecycleStatus` holds `ARCHIVED` alone, and archiving is the end of the job's life:
//!   the job stays `ARCHIVED` through every move the clock still owed it, and a later update
//!   of it is refused with the modeled 409;
//! * a job's `maxWorkerCount` of 0 is refused with the modeled 400 wherever it arrives, and
//!   nothing is stored for it;
//! * a job is reached only through its own queue and farm, so each missing level of the path
//!   is the modeled 404 naming that level and nothing is written;
//! * `clientToken` is `@idempotencyToken`, so a retried update is applied once.
//!
//! The harness (`Emulator`) is the one every other suite in this target drives the service
//! with, so these tests go through routing, deserialization, constraint validation, the
//! handler and serialization, exactly as the router does in production.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::fleet::create_fleet;

/// Every value of the modeled `JobTargetTaskRunStatus`, from `smithy_query(query="shape",
/// shape="JobTargetTaskRunStatus")`: the whole set of statuses `UpdateJob` can be asked to
/// take a job's tasks to.
const TARGET_TASK_RUN_STATUSES: [&str; 6] = [
    "READY",
    "FAILED",
    "SUCCEEDED",
    "CANCELED",
    "SUSPENDED",
    "PENDING",
];

/// How long a poll of a job waits for a clock-owed move of it to be materialised.
///
/// Generous on purpose: the spans this service owes a job are under a handful of seconds, so
/// a deadline this far out is reached only when the move never comes, never because the host
/// is loaded. Nothing here waits a fixed time — every wait polls for the CONDITION.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two reads of the job.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn job_path(farm_id: &str, queue_id: &str, job_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs/{job_id}")
}

/// A minimal OpenJD-shaped template, as `CreateJob` accepts it with `templateType=JSON`.
fn template(name: &str) -> String {
    json!({
        "specificationVersion": "jobtemplate-2023-09",
        "name": name,
        "steps": [],
    })
    .to_string()
}

/// Create a job under `farm_id`/`queue_id` from `extra` (the members beyond the template and
/// its type) and return its id.
async fn create_job(emulator: &Emulator, farm_id: &str, queue_id: &str, extra: &Value) -> String {
    let mut body = json!({
        "template": template("update-job"),
        "templateType": "JSON",
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in extra {
            body.insert(member.clone(), value.clone());
        }
    }
    let (status, created) = emulator
        .post(
            &format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs"),
            body,
        )
        .await;
    assert_eq!(
        status, 201,
        "CreateJob {extra}: expected 201, got {status} with {created}"
    );
    string_at(&created, "jobId")
}

/// `PATCH /2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}`, from
/// `smithy_query(query="operation", operation="UpdateJob")`.
async fn update_job(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    body: Value,
    client_token: Option<&str>,
) -> (u16, Value) {
    emulator
        .call(
            "PATCH",
            &job_path(farm_id, queue_id, job_id),
            Some(body),
            client_token,
        )
        .await
}

async fn get_job(emulator: &Emulator, farm_id: &str, queue_id: &str, job_id: &str) -> Value {
    let (status, body) = emulator.get(&job_path(farm_id, queue_id, job_id)).await;
    assert_eq!(status, 200, "GetJob: expected 200, got {status} with {body}");
    body
}

/// Read the job until `settled` holds for what `GetJob` reports, and return that report.
///
/// The wait is for the CONDITION, not for a span: a clock-owed move is materialised on read,
/// so the loop looks again until the move shows up or [`POLL_DEADLINE`] passes.
async fn job_until(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    case: &str,
    wanted: &str,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    let mut job = get_job(emulator, farm_id, queue_id, job_id).await;
    while !settled(&job) {
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job never reached {wanted}, last read {job}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
        job = get_job(emulator, farm_id, queue_id, job_id).await;
    }
    job
}

/// The job once the service has finished assembling it, which is the state every update in
/// this suite is applied on top of.
async fn created_job(
    emulator: &Emulator,
    farm_id: &str,
    queue_id: &str,
    job_id: &str,
    case: &str,
) -> Value {
    job_until(
        emulator,
        farm_id,
        queue_id,
        job_id,
        case,
        "CREATE_COMPLETE",
        |job| job.get("lifecycleStatus").and_then(Value::as_str) == Some("CREATE_COMPLETE"),
    )
    .await
}

/// Associate `fleet_id` with `queue_id`, the way `CreateQueueFleetAssociation` is bound:
/// `PUT /2023-10-12/farms/{farmId}/queue-fleet-associations` with the pair in the body.
///
/// A queue with a fleet associated to it is one the service does not reconcile its jobs away
/// from, so the only moves the clock owes those jobs are the ones these tests drive.
async fn associate(emulator: &Emulator, farm_id: &str, queue_id: &str, fleet_id: &str) {
    let (status, body) = emulator
        .call(
            "PUT",
            &format!("/2023-10-12/farms/{farm_id}/queue-fleet-associations"),
            Some(json!({"queueId": queue_id, "fleetId": fleet_id})),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "CreateQueueFleetAssociation: expected 200, got {status} with {body}"
    );
}

/// A farm, a queue with a fleet associated to it, and the emulator holding them.
async fn scheduling_queue(emulator: &Emulator, display_name: &str) -> (String, String) {
    let farm_id = emulator.farm(display_name).await;
    let queue_id = emulator.queue(&farm_id, display_name).await;
    let fleet_id = create_fleet(emulator, &farm_id, display_name).await;
    associate(emulator, &farm_id, &queue_id, &fleet_id).await;
    (farm_id, queue_id)
}

#[tokio::test]
async fn update_job_applies_the_members_it_carries_and_keeps_the_rest() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Patched job farm").await;

    // The rule: a member the request carries replaces the stored one; a member it omits is
    // untouched. The rows differ along which member moves and along the value it moves to —
    // both published ends of `priority`'s `range 0..100`, the two documented ends of
    // `maxWorkerCount`'s meaning (`-1` for no maximum and a real ceiling), an emptied
    // description, and a request that carries no member at all.
    let cases: Vec<(&str, Value)> = vec![
        ("no member at all", json!({})),
        ("name", json!({"name": "Renamed by the patch"})),
        ("description", json!({"description": "patched description"})),
        ("description emptied", json!({"description": ""})),
        ("priority at the bottom of the range", json!({"priority": 0})),
        ("priority mid range", json!({"priority": 73})),
        ("priority at the top of the range", json!({"priority": 100})),
        ("maxFailedTasksCount", json!({"maxFailedTasksCount": 0})),
        ("maxRetriesPerTask", json!({"maxRetriesPerTask": 11})),
        ("maxWorkerCount of no maximum", json!({"maxWorkerCount": -1})),
        ("maxWorkerCount of one worker", json!({"maxWorkerCount": 1})),
        (
            "maxWorkerCount at the top of the range",
            json!({"maxWorkerCount": 2147483647i64}),
        ),
        (
            "every member at once",
            json!({
                "name": "All at once",
                "description": "all at once",
                "priority": 42,
                "maxFailedTasksCount": 7,
                "maxRetriesPerTask": 3,
                "maxWorkerCount": 9,
            }),
        ),
    ];

    // What every row's job is created with, so what a row asserts about the members it did
    // NOT name is about this operation and not about an earlier row.
    let created_values = json!({
        "name": "Before the patch",
        "description": "before the patch",
        "priority": 50,
        "maxFailedTasksCount": 20,
        "maxRetriesPerTask": 5,
        "maxWorkerCount": -1,
    });

    for (case, patch) in &cases {
        let job_id = create_job(
            &emulator,
            &farm_id,
            &queue_id,
            &json!({
                "nameOverride": "Before the patch",
                "descriptionOverride": "before the patch",
                "priority": 50,
                "maxFailedTasksCount": 20,
                "maxRetriesPerTask": 5,
                "maxWorkerCount": -1,
            }),
        )
        .await;
        let before = created_job(&emulator, &farm_id, &queue_id, &job_id, case).await;

        let (status, body) =
            update_job(&emulator, &farm_id, &queue_id, &job_id, patch.clone(), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateJob to answer 200, got {status} with {body}"
        );

        let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        for member in [
            "name",
            "description",
            "priority",
            "maxFailedTasksCount",
            "maxRetriesPerTask",
            "maxWorkerCount",
        ] {
            // A description emptied by the patch is reported by nothing: the job holds none.
            let expected = patch
                .get(member)
                .or_else(|| created_values.get(member))
                .filter(|value| value.as_str() != Some(""));
            assert_eq!(
                job.get(member),
                expected,
                "case {case:?}: expected {member} to be {expected:?}, got {job}"
            );
        }
        // An update of the job's own values moves nothing about where the job is in its
        // life: the lifecycle it had settled to is what it still reports.
        assert_eq!(
            job.get("lifecycleStatus"),
            before.get("lifecycleStatus"),
            "case {case:?}: expected the settled lifecycleStatus to survive, got {job}"
        );
        assert_eq!(
            job.get("taskRunStatus"),
            before.get("taskRunStatus"),
            "case {case:?}: expected the settled taskRunStatus to survive, got {job}"
        );
        // Every update stamps who changed the job and when, and leaves the create's own
        // provenance alone.
        assert!(
            job.get("updatedAt").is_some() && !string_at(&job, "updatedBy").is_empty(),
            "case {case:?}: expected the update to stamp updatedAt/updatedBy, got {job}"
        );
        assert_eq!(
            (job.get("createdAt"), job.get("createdBy")),
            (before.get("createdAt"), before.get("createdBy")),
            "case {case:?}: expected the create's provenance to survive, got {job}"
        );
        // A read of the updated job does not change what the next reader sees.
        let again = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        assert_eq!(
            again, job,
            "case {case:?}: two reads of one updated job disagreed: {job} then {again}"
        );
    }
}

#[tokio::test]
async fn update_job_works_the_asked_for_task_run_status_through_the_job() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Retargeted job farm").await;

    // The rule generalises over the status asked for, so every value the model publishes for
    // `targetTaskRunStatus` is a row. Each row's job also carries a different priority, so
    // the rule is not read off one shape of job.
    let mut job_ids = Vec::new();
    for (index, target) in TARGET_TASK_RUN_STATUSES.iter().enumerate() {
        let job_id = create_job(
            &emulator,
            &farm_id,
            &queue_id,
            &json!({"priority": (index * 20) as i64, "nameOverride": format!("to {target}")}),
        )
        .await;
        created_job(&emulator, &farm_id, &queue_id, &job_id, target).await;
        job_ids.push(job_id);
    }

    // The update is answered before the service has worked the job's tasks to the status,
    // so the job reports the update as in progress and names the status it is working to.
    for (target, job_id) in TARGET_TASK_RUN_STATUSES.iter().zip(job_ids.iter()) {
        let (status, body) = update_job(
            &emulator,
            &farm_id,
            &queue_id,
            job_id,
            json!({"targetTaskRunStatus": target}),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {target:?}: expected UpdateJob to answer 200, got {status} with {body}"
        );
        let job = get_job(&emulator, &farm_id, &queue_id, job_id).await;
        assert_eq!(
            job.get("targetTaskRunStatus").and_then(Value::as_str),
            Some(*target),
            "case {target:?}: expected the asked-for target to be reported, got {job}"
        );
        assert_eq!(
            job.get("lifecycleStatus").and_then(Value::as_str),
            Some("UPDATE_IN_PROGRESS"),
            "case {target:?}: expected the update to be in progress, got {job}"
        );
    }

    // Once the service has worked it through, the job's run status carries the asked-for
    // status and the target is dropped: there is nothing left the job is working towards.
    for (target, job_id) in TARGET_TASK_RUN_STATUSES.iter().zip(job_ids.iter()) {
        let job = job_until(
            &emulator,
            &farm_id,
            &queue_id,
            job_id,
            target,
            "UPDATE_SUCCEEDED",
            |job| job.get("lifecycleStatus").and_then(Value::as_str) == Some("UPDATE_SUCCEEDED"),
        )
        .await;
        assert_eq!(
            job.get("taskRunStatus").and_then(Value::as_str),
            Some(*target),
            "case {target:?}: expected the job's run status to be the asked-for one, got {job}"
        );
        assert_eq!(
            job.get("targetTaskRunStatus"),
            None,
            "case {target:?}: expected the settled job to report no target, got {job}"
        );
    }
}

#[tokio::test]
async fn update_job_archives_the_job_and_refuses_every_later_update_of_it() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Archived job farm").await;

    // The rows differ along what the archiving request carries besides `lifecycleStatus` —
    // the member on its own, and together with each kind of member the operation patches —
    // and along the job it is addressed at.
    let cases: Vec<(&str, Value)> = vec![
        ("on its own", json!({"lifecycleStatus": "ARCHIVED"})),
        (
            "with a value to store",
            json!({"lifecycleStatus": "ARCHIVED", "priority": 11, "name": "Archived with a name"}),
        ),
        (
            "with a task run status to work to",
            json!({"lifecycleStatus": "ARCHIVED", "targetTaskRunStatus": "CANCELED"}),
        ),
    ];

    for (case, patch) in &cases {
        let job_id = create_job(&emulator, &farm_id, &queue_id, &json!({"priority": 50})).await;
        // A job archived while the service is still assembling it is archived all the same,
        // so one row does not wait for the create to settle.
        if *case != "on its own" {
            created_job(&emulator, &farm_id, &queue_id, &job_id, case).await;
        }

        let (status, body) =
            update_job(&emulator, &farm_id, &queue_id, &job_id, patch.clone(), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateJob to answer 200, got {status} with {body}"
        );

        let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        assert_eq!(
            job.get("lifecycleStatus").and_then(Value::as_str),
            Some("ARCHIVED"),
            "case {case:?}: expected the job to be archived, got {job}"
        );
        assert!(
            !string_at(&job, "lifecycleStatusMessage").is_empty(),
            "case {case:?}: expected a lifecycleStatusMessage, got {job}"
        );
        // An archived job is not working towards anything: its life has ended.
        assert_eq!(
            job.get("targetTaskRunStatus"),
            None,
            "case {case:?}: expected the archived job to report no target, got {job}"
        );
        // The values the same request carried are stored like any other patch.
        for member in ["priority", "name"] {
            if let Some(expected) = patch.get(member) {
                assert_eq!(
                    job.get(member),
                    Some(expected),
                    "case {case:?}: expected the patched {member}, got {job}"
                );
            }
        }

        // Archiving is the end of the job's life, so every later update of it is refused and
        // the job is left exactly as the archiving left it.
        for (refused, later) in [
            ("archived again", json!({"lifecycleStatus": "ARCHIVED"})),
            ("a value to store", json!({"priority": 1})),
            (
                "a task run status to work to",
                json!({"targetTaskRunStatus": "READY"}),
            ),
        ] {
            let (status, body) =
                update_job(&emulator, &farm_id, &queue_id, &job_id, later, None).await;
            assert_eq!(
                status, 409,
                "case {case:?}/{refused:?}: expected a 409, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ConflictException",
                "case {case:?}/{refused:?}: expected a ConflictException, got {body}"
            );
            let expected = "Job is in ARCHIVED status and cannot be updated.";
            assert_eq!(
                string_at(&body, "message"),
                expected,
                "case {case:?}/{refused:?}: expected the message {expected:?}, got {body}"
            );
            assert_eq!(
                (
                    string_at(&body, "reason"),
                    string_at(&body, "resourceType"),
                    string_at(&body, "resourceId"),
                ),
                (
                    "STATUS_CONFLICT".to_string(),
                    "job".to_string(),
                    job_id.clone(),
                ),
                "case {case:?}/{refused:?}: expected the conflict to name the job, got {body}"
            );
            let after = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
            assert_eq!(
                after, job,
                "case {case:?}/{refused:?}: the refused update changed the job: {job} then \
                 {after}"
            );
        }
    }

    // The archived job stays archived through the move the clock owed it: a job created
    // beside it reaching CREATE_COMPLETE is what says that move has come due.
    let control_id = create_job(&emulator, &farm_id, &queue_id, &json!({"priority": 50})).await;
    let archived_id = create_job(&emulator, &farm_id, &queue_id, &json!({"priority": 50})).await;
    let (status, body) = update_job(
        &emulator,
        &farm_id,
        &queue_id,
        &archived_id,
        json!({"lifecycleStatus": "ARCHIVED"}),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "archiving a job the clock still owed a move: expected 200, got {status} with {body}"
    );
    created_job(&emulator, &farm_id, &queue_id, &control_id, "the control job").await;
    let archived = get_job(&emulator, &farm_id, &queue_id, &archived_id).await;
    assert_eq!(
        archived.get("lifecycleStatus").and_then(Value::as_str),
        Some("ARCHIVED"),
        "expected the archived job to stay archived once its creation move came due, got \
         {archived}"
    );
}

#[tokio::test]
async fn a_max_worker_count_of_zero_is_refused_wherever_a_job_carries_it() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Worker count farm").await;
    let job_id = create_job(
        &emulator,
        &farm_id,
        &queue_id,
        &json!({"priority": 50, "maxWorkerCount": 4}),
    )
    .await;
    let before = created_job(&emulator, &farm_id, &queue_id, &job_id, "maxWorkerCount").await;

    // The rule: the documented value 0 is refused, and every other value of the modeled
    // `range -1..2147483647` is stored. The rows are the refused value, both published bounds
    // of the range, and a value inside it.
    let cases: Vec<(&str, i64, bool)> = vec![
        ("zero", 0, false),
        ("no maximum", -1, true),
        ("one worker", 1, true),
        ("the top of the range", 2147483647, true),
    ];

    for (case, max_worker_count, accepted) in &cases {
        let (status, body) = update_job(
            &emulator,
            &farm_id,
            &queue_id,
            &job_id,
            json!({"maxWorkerCount": max_worker_count}),
            None,
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected UpdateJob to answer 200, got {status} with {body}"
            );
            let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
            assert_eq!(
                job.get("maxWorkerCount").and_then(Value::as_i64),
                Some(*max_worker_count),
                "case {case:?}: expected the stored maxWorkerCount, got {job}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected a 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {body}"
        );
        assert!(
            string_at(&body, "message").contains("maxWorkerCount"),
            "case {case:?}: expected the message to name the member, got {body}"
        );
        assert_eq!(
            body.get("fieldList")
                .and_then(Value::as_array)
                .and_then(|fields| fields.first())
                .and_then(|field| field.get("name"))
                .and_then(Value::as_str),
            Some("maxWorkerCount"),
            "case {case:?}: expected the field list to name the member, got {body}"
        );
        let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        assert_eq!(
            job.get("maxWorkerCount"),
            before.get("maxWorkerCount"),
            "case {case:?}: the refused update changed the job, got {job}"
        );
    }

    // The same value is refused where a job is created with it, and nothing is stored: the
    // rule is about a job's `maxWorkerCount`, not about one operation.
    let (status, body) = emulator
        .post(
            &format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs"),
            json!({
                "template": template("zero-workers"),
                "templateType": "JSON",
                "priority": 50,
                "maxWorkerCount": 0,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "CreateJob with a maxWorkerCount of 0: expected a 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "CreateJob with a maxWorkerCount of 0: expected a ValidationException, got {body}"
    );
    let (status, listed) = emulator
        .get(&format!(
            "/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs"
        ))
        .await;
    assert_eq!(
        status, 200,
        "ListJobs: expected 200, got {status} with {listed}"
    );
    assert_eq!(
        listed.get("jobs").and_then(Value::as_array).map(Vec::len),
        Some(1),
        "expected the refused create to have stored no job, got {listed}"
    );
}

#[tokio::test]
async fn update_job_reports_each_missing_level_of_the_path_as_that_level() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Missing job farm").await;
    let job_id = create_job(&emulator, &farm_id, &queue_id, &json!({"priority": 50})).await;
    let other_queue_id = emulator.queue(&farm_id, "Another queue").await;

    let no_farm = absent_id("farm", "0000000000000000000000000000000d");
    let no_queue = absent_id("queue", "0000000000000000000000000000000e");
    let no_job = absent_id("job", "0000000000000000000000000000000f");

    // A job is reached only through its own queue of its own farm, so each level that is
    // missing is reported as that level — and a job held under ANOTHER queue of the same farm
    // is a job this path does not reach.
    let cases: Vec<(&str, String, String, String, &str, String)> = vec![
        (
            "an unheld farm",
            no_farm.clone(),
            queue_id.clone(),
            job_id.clone(),
            "farm",
            no_farm.clone(),
        ),
        (
            "an unheld queue",
            farm_id.clone(),
            no_queue.clone(),
            job_id.clone(),
            "queue",
            no_queue.clone(),
        ),
        (
            "an unheld job",
            farm_id.clone(),
            queue_id.clone(),
            no_job.clone(),
            "job",
            no_job.clone(),
        ),
        (
            "a job of another queue of the same farm",
            farm_id.clone(),
            other_queue_id.clone(),
            job_id.clone(),
            "job",
            job_id.clone(),
        ),
    ];

    for (case, farm, queue, job, resource_type, resource_id) in &cases {
        let (status, body) = update_job(
            &emulator,
            farm,
            queue,
            job,
            json!({"priority": 7}),
            None,
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected a 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected the missing-resource code, got {body}"
        );
        let expected =
            format!("Resource of type {resource_type} with id {resource_id} does not exist.");
        assert_eq!(
            string_at(&body, "message"),
            expected,
            "case {case:?}: expected the message {expected:?}, got {body}"
        );
    }

    // None of those refusals touched the job the path does hold.
    let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
    assert_eq!(
        job.get("priority").and_then(Value::as_i64),
        Some(50),
        "expected the refused updates to have changed nothing, got {job}"
    );
}

#[tokio::test]
async fn one_client_token_applies_an_update_once() {
    let emulator = Emulator::new();
    let (farm_id, queue_id) = scheduling_queue(&emulator, "Retried update farm").await;

    // The rows differ along what the retry carries: the very same patch, and a patch asking
    // for something else under the token already spent. Either way the token names the update
    // the first call made, so the second changes nothing.
    let cases: Vec<(&str, Value, Value)> = vec![
        (
            "the same patch",
            json!({"priority": 20}),
            json!({"priority": 20}),
        ),
        (
            "a different patch",
            json!({"priority": 30, "name": "First"}),
            json!({"priority": 90, "name": "Second"}),
        ),
    ];

    for (index, (case, first, second)) in cases.iter().enumerate() {
        let job_id = create_job(&emulator, &farm_id, &queue_id, &json!({"priority": 50})).await;
        created_job(&emulator, &farm_id, &queue_id, &job_id, case).await;
        let token = format!("retried-update-{index}");

        let (status, body) = update_job(
            &emulator,
            &farm_id,
            &queue_id,
            &job_id,
            first.clone(),
            Some(&token),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first UpdateJob to answer 200, got {status} with {body}"
        );
        let applied = get_job(&emulator, &farm_id, &queue_id, &job_id).await;

        let (status, body) = update_job(
            &emulator,
            &farm_id,
            &queue_id,
            &job_id,
            second.clone(),
            Some(&token),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the retry to answer 200, got {status} with {body}"
        );
        let after = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        assert_eq!(
            after, applied,
            "case {case:?}: the retry changed the job: {applied} then {after}"
        );

        // A DIFFERENT token is a different update, and it is applied.
        let (status, body) = update_job(
            &emulator,
            &farm_id,
            &queue_id,
            &job_id,
            second.clone(),
            Some(&format!("{token}-again")),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the second update to answer 200, got {status} with {body}"
        );
        let job = get_job(&emulator, &farm_id, &queue_id, &job_id).await;
        assert_eq!(
            job.get("priority"),
            second.get("priority"),
            "case {case:?}: expected the second update's priority, got {job}"
        );
    }
}
