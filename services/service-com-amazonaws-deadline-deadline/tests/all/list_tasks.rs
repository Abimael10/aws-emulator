// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListTasks`.
//!
//! The rules under test:
//!
//! * `ListTasks` reports one summary per task the step in its path holds — one per point of
//!   the parameter space that step's template declared, in the order the space enumerates
//!   them — each carrying that task's own id, the parameters it runs with, the run it is in
//!   and the instants that run started and ended;
//! * paging hands back every task exactly once in at most `maxResults` per page and stops
//!   handing back a token at the end; an unknown token is a 400 `ValidationException`;
//! * a task is reached only through its own step, job, queue and farm, so each level this
//!   emulator does not hold is a 404 `ResourceNotFoundException` naming that level, and a
//!   step's list reports no other step's tasks;
//! * listing is a read: a second call answers the same thing.
//!
//! The harness (`Emulator`) and the fleet and worker helpers are the ones the other suites
//! drive the service with, so these tests go through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::farm_queue_job::{absent_id, error_code, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::create_worker;

/// Modeled `@range` bounds of `MaxResults`, the target of `maxResults` (`range 1..100`,
/// default 100), from `smithy_query(query="operation", operation="ListTasks")`.
const MIN_MAX_RESULTS: i64 = 1;
const MAX_MAX_RESULTS: i64 = 100;

/// How long a test waits for a condition the clock owes before it gives up.
///
/// Generous on purpose: every wait here polls for the CONDITION and returns as soon as it
/// holds, so this bounds only a run in which the emulator never gets there.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// Collection path of the task resource, from
/// `smithy_query(query="operation", operation="ListTasks")`:
/// `GET /2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}/steps/{stepId}/tasks`.
fn tasks_path(farm_id: &str, queue_id: &str, job_id: &str, step_id: &str) -> String {
    format!(
        "/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs/{job_id}/steps/{step_id}/tasks"
    )
}

fn jobs_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs")
}

fn job_path(farm_id: &str, queue_id: &str, job_id: &str) -> String {
    format!("{}/{job_id}", jobs_path(farm_id, queue_id))
}

/// An Open Job Description job template declaring `steps`, written in JSON.
fn template(name: &str, steps: Value) -> String {
    json!({
        "specificationVersion": "jobtemplate-2023-09",
        "name": name,
        "steps": steps,
    })
    .to_string()
}

/// One step with no parameter space: one task, with no parameters.
fn bare_step(name: &str) -> Value {
    json!({
        "name": name,
        "script": {"actions": {"onRun": {"command": "echo"}}},
    })
}

/// One step whose parameter space declares `definitions`.
fn parameter_space_step(name: &str, definitions: Value) -> Value {
    json!({
        "name": name,
        "parameterSpace": {"taskParameterDefinitions": definitions},
        "script": {"actions": {"onRun": {"command": "echo"}}},
    })
}

/// Whether `id` satisfies the modeled `TaskId` pattern
/// `^task-[0-9a-f]{32}-(0|([1-9][0-9]{0,9}))$`, from
/// `smithy_query(query="shape", shape="TaskSummary")`, with the 32 hex characters of
/// `step_id` and the index `index`.
fn is_task_id_of(id: &str, step_id: &str, index: usize) -> bool {
    let hex = step_id.strip_prefix("step-").unwrap_or(step_id);
    id == format!("task-{hex}-{index}")
        && hex.len() == 32
        && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// A farm, a queue, and a fleet scheduling that queue's work: the arrangement in which a
/// created job settles on the run status it was submitted with instead of being reconciled
/// away as one no fleet can pick up.
struct Scheduled {
    emulator: Emulator,
    farm_id: String,
    queue_id: String,
    fleet_id: String,
}

impl Scheduled {
    async fn new(name: &str) -> Self {
        let emulator = Emulator::new();
        let farm_id = emulator.farm(name).await;
        let queue_id = emulator.queue(&farm_id, name).await;
        let fleet_id = create_fleet(&emulator, &farm_id, name).await;
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
            "CreateQueueFleetAssociation {name}: expected 200, got {status} with {body}"
        );
        Self {
            emulator,
            farm_id,
            queue_id,
            fleet_id,
        }
    }

    /// Create a job from the template declaring `steps` and return its id.
    async fn job(&self, name: &str, steps: Value) -> String {
        let (status, created) = self
            .emulator
            .post(
                &jobs_path(&self.farm_id, &self.queue_id),
                json!({
                    "template": template(name, steps),
                    "templateType": "JSON",
                    "priority": 50,
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "CreateJob {name}: expected 201, got {status} with {created}"
        );
        string_at(&created, "jobId")
    }

    /// `GetJob`, which must succeed.
    async fn read_job(&self, job_id: &str) -> Value {
        let (status, job) = self
            .emulator
            .get(&job_path(&self.farm_id, &self.queue_id, job_id))
            .await;
        assert_eq!(
            status, 200,
            "GetJob {job_id}: expected 200, got {status} with {job}"
        );
        job
    }

    /// Poll `GetJob` until the member `member` reads `wanted`, and answer the job then.
    ///
    /// The moves a created job makes are owed by the clock, so this waits for the CONDITION
    /// rather than for a span: it looks again until the job is there or the deadline passes.
    async fn job_until(&self, case: &str, job_id: &str, member: &str, wanted: &str) -> Value {
        let deadline = Instant::now() + CONDITION_DEADLINE;
        loop {
            let job = self.read_job(job_id).await;
            if job.get(member).and_then(Value::as_str) == Some(wanted) {
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {member} {wanted:?} within the deadline, last read {job}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// The step ids `ListSteps` reports for `job_id`, in declared order.
    async fn step_ids(&self, case: &str, job_id: &str) -> Vec<String> {
        let path = format!(
            "/2023-10-12/farms/{}/queues/{}/jobs/{job_id}/steps",
            self.farm_id, self.queue_id
        );
        let (status, body) = self.emulator.get(&path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListSteps to answer 200, got {status} with {body}"
        );
        body["steps"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a steps list in {body}"))
            .iter()
            .map(|summary| string_at(summary, "stepId"))
            .collect()
    }

    /// `ListTasks` for `step_id` with `query` appended, which must succeed.
    async fn list_tasks(&self, case: &str, job_id: &str, step_id: &str, query: &str) -> Value {
        let (status, body) = self
            .emulator
            .get(&format!(
                "{}{query}",
                tasks_path(&self.farm_id, &self.queue_id, job_id, step_id)
            ))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListTasks to answer 200, got {status} with {body}"
        );
        body
    }
}

/// The `tasks` array of a successful `ListTasks` answer.
fn tasks_of(case: &str, body: &Value) -> Vec<Value> {
    body["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a tasks list in {body}"))
        .clone()
}

/// A summary's `parameters`, in an order a comparison does not depend on: each name mapped
/// to the `TaskParameterValue` member it carries and that member's text.
fn parameters_of(summary: &Value) -> BTreeMap<String, (String, String)> {
    summary
        .get("parameters")
        .and_then(Value::as_object)
        .map(|parameters| {
            parameters
                .iter()
                .map(|(name, value)| {
                    let (member, text) = value
                        .as_object()
                        .and_then(|value| value.iter().next())
                        .map(|(member, text)| {
                            (
                                member.clone(),
                                text.as_str().unwrap_or_default().to_string(),
                            )
                        })
                        .unwrap_or_default();
                    (name.clone(), (member, text))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One expected point of a parameter space: each parameter's name, the
/// `TaskParameterValue` member its declared type maps to, and the text it carries.
fn point(entries: &[(&str, &str, &str)]) -> BTreeMap<String, (String, String)> {
    entries
        .iter()
        .map(|(name, member, text)| {
            (name.to_string(), (member.to_string(), text.to_string()))
        })
        .collect()
}

/// Walk every page of `ListTasks` at `max_results` and return the task ids in the order
/// they were handed back.
async fn page_all(
    scheduled: &Scheduled,
    case: &str,
    job_id: &str,
    step_id: &str,
    max_results: i64,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let query = match &token {
            Some(token) => format!("?maxResults={max_results}&nextToken={token}"),
            None => format!("?maxResults={max_results}"),
        };
        let body = scheduled.list_tasks(case, job_id, step_id, &query).await;
        let page = tasks_of(case, &body);
        assert!(
            page.len() as i64 <= max_results,
            "case {case:?}: expected at most {max_results} tasks on a page, got {}",
            page.len()
        );
        for summary in &page {
            seen.push(string_at(summary, "taskId"));
        }
        pages += 1;
        assert!(
            pages <= seen.len() + 2,
            "case {case:?}: paging did not terminate after {pages} pages"
        );
        match body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    seen
}

/// One row of the table below: the case, the single step its template declares, and the
/// parameters of each task that step's space enumerates, in enumerated order.
type EnumeratedCase = (
    &'static str,
    Value,
    Vec<BTreeMap<String, (String, String)>>,
);

#[tokio::test]
async fn list_tasks_reports_the_points_the_parameter_space_enumerated() {
    let scheduled = Scheduled::new("tasks").await;

    // The rule: a step's tasks are one per point of its declared parameter space, in the
    // order that space enumerates them (the last parameter varying fastest), and each
    // summary carries the parameters of its own point. The rows differ along what the rule
    // generalises over: whether a space is declared at all, how many parameters it
    // declares, how many values each declares, the form the range is written in, and the
    // declared type each value is carried as — `INT`/`FLOAT`/`STRING`/`PATH` map to the
    // `int`/`float`/`string`/`path` members of `TaskParameterValue`, from
    // `smithy_query(query="shape", shape="TaskParameterValue")`.
    let cases: Vec<EnumeratedCase> = vec![
        (
            "a step with no parameter space runs once with no parameters",
            bare_step("Solo"),
            vec![BTreeMap::new()],
        ),
        (
            "one INT parameter listed as a range array",
            parameter_space_step(
                "Frames",
                json!([{"name": "Frame", "type": "INT", "range": [1, 2, 3]}]),
            ),
            vec![
                point(&[("Frame", "int", "1")]),
                point(&[("Frame", "int", "2")]),
                point(&[("Frame", "int", "3")]),
            ],
        ),
        (
            "two parameters: the product, the last varying fastest",
            parameter_space_step(
                "Tiles",
                json!([
                    {"name": "Frame", "type": "INT", "range": [7, 8]},
                    {"name": "Side", "type": "STRING", "range": ["left", "right"]},
                ]),
            ),
            vec![
                point(&[("Frame", "int", "7"), ("Side", "string", "left")]),
                point(&[("Frame", "int", "7"), ("Side", "string", "right")]),
                point(&[("Frame", "int", "8"), ("Side", "string", "left")]),
                point(&[("Frame", "int", "8"), ("Side", "string", "right")]),
            ],
        ),
        (
            "an INT range expression declaring a stride",
            parameter_space_step(
                "Strided",
                json!([{"name": "Frame", "type": "INT", "range": "1-5:2"}]),
            ),
            vec![
                point(&[("Frame", "int", "1")]),
                point(&[("Frame", "int", "3")]),
                point(&[("Frame", "int", "5")]),
            ],
        ),
        (
            "a FLOAT and a PATH parameter, a type the issue did not cite",
            parameter_space_step(
                "Mixed",
                json!([
                    {"name": "Scale", "type": "FLOAT", "range": ["0.5", "1.25"]},
                    {"name": "Scene", "type": "PATH", "range": ["/assets/a.blend"]},
                ]),
            ),
            vec![
                point(&[
                    ("Scale", "float", "0.5"),
                    ("Scene", "path", "/assets/a.blend"),
                ]),
                point(&[
                    ("Scale", "float", "1.25"),
                    ("Scene", "path", "/assets/a.blend"),
                ]),
            ],
        ),
    ];

    for (case, step, expected) in &cases {
        let job_id = scheduled.job("enumerated", json!([step])).await;
        // Once the job's creation has converged its tasks carry the status it was submitted
        // with, which is the default `READY`: the one state every row can be read in.
        let job = scheduled
            .job_until(case, &job_id, "lifecycleStatus", "CREATE_COMPLETE")
            .await;
        let step_ids = scheduled.step_ids(case, &job_id).await;
        assert_eq!(
            step_ids.len(),
            1,
            "case {case:?}: expected the one declared step, got {step_ids:?}"
        );
        let step_id = &step_ids[0];

        let listed = scheduled.list_tasks(case, &job_id, step_id, "").await;
        let summaries = tasks_of(case, &listed);
        assert_eq!(
            summaries.len(),
            expected.len(),
            "case {case:?}: expected one summary per enumerated point, got {listed}"
        );
        assert!(
            listed.get("nextToken").is_none(),
            "case {case:?}: expected no nextToken on a complete page, got {listed}"
        );

        for (index, (summary, wanted)) in summaries.iter().zip(expected.iter()).enumerate() {
            let task_id = string_at(summary, "taskId");
            // A task id is its step's hex with the index of the task appended, which is
            // what the modeled `TaskId` pattern spells.
            assert!(
                is_task_id_of(&task_id, step_id, index),
                "case {case:?}: expected the id of task {index} of {step_id}, got {task_id:?}"
            );
            assert_eq!(
                parameters_of(summary),
                *wanted,
                "case {case:?}: expected the parameters of point {index}, got {summary}"
            );
            assert_eq!(
                summary.get("runStatus").and_then(Value::as_str),
                Some("READY"),
                "case {case:?}: expected task {index} to carry the status the job settled \
                 on, got {summary}"
            );
            assert_eq!(
                summary.get("failureRetryCount").and_then(Value::as_i64),
                Some(0),
                "case {case:?}: expected no retries on task {index} before anything ran, \
                 got {summary}"
            );
            // Nothing has run the task, so its run has neither started nor ended and no
            // session action has been assigned to it, and the job's lifecycle has settled,
            // so no target is being worked towards.
            assert!(
                summary.get("startedAt").is_none()
                    && summary.get("endedAt").is_none()
                    && summary.get("latestSessionActionId").is_none(),
                "case {case:?}: expected no run members on task {index} before anything \
                 ran, got {summary}"
            );
            assert!(
                summary.get("targetRunStatus").is_none(),
                "case {case:?}: expected no target once the lifecycle settled on task \
                 {index}, got {summary}"
            );
            assert!(
                summary.get("updatedAt").is_none() && summary.get("updatedBy").is_none(),
                "case {case:?}: expected no update members on task {index} before an \
                 update, got {summary}"
            );
            // The tasks are minted by the call that created the job, so that call is what
            // created them.
            assert_eq!(
                (
                    summary.get("createdAt").cloned(),
                    summary.get("createdBy").cloned(),
                ),
                (job.get("createdAt").cloned(), job.get("createdBy").cloned()),
                "case {case:?}: expected the creating call of the job on task {index}, got \
                 {summary} against {job}"
            );
        }

        let mut distinct: Vec<String> = summaries
            .iter()
            .map(|summary| string_at(summary, "taskId"))
            .collect();
        let minted = distinct.len();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            minted,
            "case {case:?}: expected a distinct id per task, got {distinct:?}"
        );

        // A read does not change what the next reader sees.
        let again = scheduled.list_tasks(case, &job_id, step_id, "").await;
        assert_eq!(
            again, listed,
            "case {case:?}: a second ListTasks answered differently: {listed} then {again}"
        );
    }
}

#[tokio::test]
async fn list_tasks_pages_over_every_task_exactly_once() {
    let scheduled = Scheduled::new("paging").await;
    let job_id = scheduled
        .job(
            "paging",
            json!([parameter_space_step(
                "Frames",
                json!([{"name": "Frame", "type": "INT", "range": "1-5"}]),
            )]),
        )
        .await;
    scheduled
        .job_until("paging", &job_id, "lifecycleStatus", "CREATE_COMPLETE")
        .await;
    let step_ids = scheduled.step_ids("paging", &job_id).await;
    let step_id = &step_ids[0];

    let unpaged = scheduled
        .list_tasks("the default page", &job_id, step_id, "")
        .await;
    let order: Vec<String> = tasks_of("the default page", &unpaged)
        .iter()
        .map(|summary| string_at(summary, "taskId"))
        .collect();
    assert_eq!(
        order.len(),
        5,
        "expected every enumerated task on the default page, got {unpaged}"
    );

    // The rule: paging hands back every task exactly once, in at most `maxResults` per
    // page, in the one order the unpaged list reports. The rows are page sizes that divide
    // the set evenly, unevenly and exceed it, plus both published bounds of `maxResults`.
    for max_results in [MIN_MAX_RESULTS, 2, 3, 5, MAX_MAX_RESULTS] {
        let case = format!("maxResults {max_results}");
        let seen = page_all(&scheduled, &case, &job_id, step_id, max_results).await;
        assert_eq!(
            seen, order,
            "case {case:?}: expected the unpaged order {order:?}, got {seen:?}"
        );
    }

    // An unknown token is a client error, not an empty page.
    let unknown = format!("task-{}-0", "0".repeat(32));
    let (status, body) = scheduled
        .emulator
        .get(&format!(
            "{}?nextToken={unknown}",
            tasks_path(&scheduled.farm_id, &scheduled.queue_id, &job_id, step_id)
        ))
        .await;
    assert_eq!(
        status, 400,
        "unknown nextToken: expected 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "unknown nextToken: expected a ValidationException, got {body}"
    );
    let expected = format!("The nextToken {unknown} isn't valid.");
    assert_eq!(
        string_at(&body, "message"),
        expected,
        "unknown nextToken: expected the message {expected:?}, got {body}"
    );
}

#[tokio::test]
async fn list_tasks_reports_only_the_tasks_of_the_step_in_its_path() {
    let scheduled = Scheduled::new("own-step").await;
    let other_queue_id = scheduled.emulator.queue(&scheduled.farm_id, "other").await;

    let job_id = scheduled
        .job(
            "own-step",
            json!([
                bare_step("Solo"),
                parameter_space_step(
                    "Frames",
                    json!([{"name": "Frame", "type": "INT", "range": [1, 2, 3]}]),
                ),
            ]),
        )
        .await;
    let other_job_id = scheduled.job("other", json!([bare_step("Other")])).await;
    for job in [&job_id, &other_job_id] {
        scheduled
            .job_until("own-step", job, "lifecycleStatus", "CREATE_COMPLETE")
            .await;
    }
    let step_ids = scheduled.step_ids("own-step", &job_id).await;
    let other_step_ids = scheduled.step_ids("own-step", &other_job_id).await;

    // The rule: a task hangs off its own step, so a list reports that step's tasks and no
    // other step's — the counts differ with the space each step declared.
    let cases: Vec<(&str, &String, &String, usize)> = vec![
        (
            "the step with no parameter space",
            &job_id,
            &step_ids[0],
            1,
        ),
        ("the step with three points", &job_id, &step_ids[1], 3),
        (
            "the step of the other job",
            &other_job_id,
            &other_step_ids[0],
            1,
        ),
    ];
    for (case, job, step_id, wanted) in &cases {
        let listed = scheduled.list_tasks(case, job, step_id, "").await;
        let summaries = tasks_of(case, &listed);
        assert_eq!(
            summaries.len(),
            *wanted,
            "case {case:?}: expected exactly this step's tasks, got {listed}"
        );
        for summary in &summaries {
            let task_id = string_at(summary, "taskId");
            let hex = step_id.strip_prefix("step-").unwrap_or(step_id);
            assert!(
                task_id.starts_with(&format!("task-{hex}-")),
                "case {case:?}: expected a task of {step_id}, got {task_id:?}"
            );
        }
    }

    // Every level of the path is reported as itself when this emulator does not hold it,
    // including a step of another job, which is not this job's step.
    let no_farm = absent_id("farm", "0000000000000000000000000000000a");
    let no_queue = absent_id("queue", "0000000000000000000000000000000b");
    let no_job = absent_id("job", "0000000000000000000000000000000c");
    let no_step = absent_id("step", "0000000000000000000000000000000d");
    let cases: Vec<(&str, String, &str, String)> = vec![
        (
            "an unheld farm",
            tasks_path(&no_farm, &scheduled.queue_id, &job_id, &step_ids[0]),
            "farm",
            no_farm.clone(),
        ),
        (
            "an unheld queue",
            tasks_path(&scheduled.farm_id, &no_queue, &job_id, &step_ids[0]),
            "queue",
            no_queue.clone(),
        ),
        (
            "an unheld job",
            tasks_path(&scheduled.farm_id, &scheduled.queue_id, &no_job, &step_ids[0]),
            "job",
            no_job.clone(),
        ),
        (
            "an unheld step",
            tasks_path(&scheduled.farm_id, &scheduled.queue_id, &job_id, &no_step),
            "step",
            no_step.clone(),
        ),
        (
            "a step of another job of the same queue",
            tasks_path(
                &scheduled.farm_id,
                &scheduled.queue_id,
                &job_id,
                &other_step_ids[0],
            ),
            "step",
            other_step_ids[0].clone(),
        ),
        (
            "a job of another queue of the same farm",
            tasks_path(
                &scheduled.farm_id,
                &other_queue_id,
                &job_id,
                &step_ids[0],
            ),
            "job",
            job_id.clone(),
        ),
    ];
    for (case, path, resource_type, resource_id) in &cases {
        let (status, body) = scheduled.emulator.get(path).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected ListTasks to answer 404, got {status} with {body}"
        );
        assert_eq!(
            (
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
                string_at(&body, "message"),
            ),
            (
                "ResourceNotFoundException".to_string(),
                resource_type.to_string(),
                resource_id.clone(),
                format!("Resource of type {resource_type} with id {resource_id} does not exist."),
            ),
            "case {case:?}: expected the 404 to name that level, got {body}"
        );
    }
}

/// Report every session action the worker is handed as succeeded until a poll hands it
/// nothing.
async fn run_every_task(case: &str, scheduled: &Scheduled, worker_id: &str, max_polls: usize) {
    let path = format!(
        "/2023-10-12/farms/{}/fleets/{}/workers/{worker_id}/schedule",
        scheduled.farm_id, scheduled.fleet_id
    );
    for poll in 0..max_polls {
        let (status, response) = scheduled
            .emulator
            .call("PATCH", &path, Some(json!({})), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: poll {poll} expected UpdateWorkerSchedule to answer 200, got \
             {status} with {response}"
        );
        let mut reported = Map::new();
        let sessions = response
            .get("assignedSessions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for session in sessions.values() {
            let actions = session
                .get("sessionActions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for action in actions {
                let action_id = string_at(&action, "sessionActionId");
                let runs_a_task = action
                    .get("definition")
                    .and_then(|definition| definition.get("taskRun"))
                    .is_some();
                let info = if runs_a_task {
                    json!({"completedStatus": "SUCCEEDED", "processExitCode": 0})
                } else {
                    json!({"completedStatus": "SUCCEEDED"})
                };
                reported.insert(action_id, info);
            }
        }
        if reported.is_empty() {
            return;
        }
        let (status, body) = scheduled
            .emulator
            .call(
                "PATCH",
                &path,
                Some(json!({"updatedSessionActions": Value::Object(reported)})),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: poll {poll} expected the report to answer 200, got {status} with \
             {body}"
        );
    }
    panic!("case {case:?}: the worker was still being handed work after {max_polls} polls");
}

#[tokio::test]
async fn a_tasks_reported_run_follows_what_the_worker_did() {
    let scheduled = Scheduled::new("ran").await;
    let (status, created) = create_worker(
        &scheduled.emulator,
        &scheduled.farm_id,
        &scheduled.fleet_id,
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateWorker: expected 200, got {status} with {created}"
    );
    let worker_id = string_at(&created, "workerId");

    // One step per task count, so the rule — a task reports the run IT is in — is read off
    // a step with one task and a step with several.
    let case = "every task of every step succeeded";
    let job_id = scheduled
        .job(
            "ran",
            json!([
                bare_step("Solo"),
                parameter_space_step(
                    "Frames",
                    json!([{"name": "Frame", "type": "INT", "range": [1, 2]}]),
                ),
            ]),
        )
        .await;
    scheduled
        .job_until(case, &job_id, "taskRunStatus", "READY")
        .await;
    let step_ids = scheduled.step_ids(case, &job_id).await;

    run_every_task(case, &scheduled, &worker_id, 32).await;
    scheduled
        .job_until(case, &job_id, "taskRunStatus", "SUCCEEDED")
        .await;

    for (step_id, tasks) in step_ids.iter().zip([1usize, 2usize]) {
        let listed = scheduled.list_tasks(case, &job_id, step_id, "").await;
        let summaries = tasks_of(case, &listed);
        assert_eq!(
            summaries.len(),
            tasks,
            "case {case:?}: expected one summary per enumerated point of {step_id}, got \
             {listed}"
        );
        for summary in &summaries {
            assert_eq!(
                summary.get("runStatus").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the reported run on {step_id}, got {summary}"
            );
            // The task's run started when the worker took it and ended when it reported
            // it, and nothing failed, so nothing was retried.
            assert!(
                summary.get("startedAt").and_then(Value::as_str).is_some()
                    && summary.get("endedAt").and_then(Value::as_str).is_some(),
                "case {case:?}: expected a started and an ended instant on {step_id}, got \
                 {summary}"
            );
            assert_eq!(
                summary.get("failureRetryCount").and_then(Value::as_i64),
                Some(0),
                "case {case:?}: expected no retries on {step_id}, got {summary}"
            );
            // The task ran in a session action, and the worker's report is what moved it,
            // so the task carries when it was last changed and no principal that changed
            // it.
            let action_id = string_at(summary, "latestSessionActionId");
            let hex = action_id
                .strip_prefix("sessionaction-")
                .and_then(|rest| rest.split_once('-'))
                .map(|(hex, _)| hex.to_string())
                .unwrap_or_default();
            assert!(
                hex.len() == 32 && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "case {case:?}: expected a ^sessionaction-[0-9a-f]{{32}}-[0-9]+$ id on \
                 {step_id}, got {action_id:?}"
            );
            assert!(
                summary.get("updatedAt").and_then(Value::as_str).is_some()
                    && summary.get("updatedBy").is_none(),
                "case {case:?}: expected a service-made update on {step_id}, got {summary}"
            );
        }
    }
}
