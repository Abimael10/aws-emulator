// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `ListSteps`.
//!
//! The rules under test:
//!
//! * `ListSteps` reports one summary per step the job in its path holds — the steps its
//!   template declared, in declared order — each carrying that step's own name, lifecycle
//!   status, and the run its own tasks are in: the per-status counts of those tasks, their
//!   retries, and the instants the step's run started and ended;
//! * paging hands back every step exactly once in at most `maxResults` per page and stops
//!   handing back a token at the end; an unknown token is a 400 `ValidationException`;
//! * a step is reached only through its own job, queue and farm, so each level this
//!   emulator does not hold is a 404 `ResourceNotFoundException` naming that level, and a
//!   job's list reports no other job's steps;
//! * listing is a read: a second call answers the same thing.
//!
//! The harness (`Emulator`) and the fleet and worker helpers are the ones the other suites
//! drive the service with, so these tests go through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::farm_queue_job::{absent_id, error_code, is_resource_id, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::create_worker;

/// Modeled `@range` bounds of `MaxResults`, the target of `maxResults` (`range 1..100`,
/// default 100), from `smithy_query(query="operation", operation="ListSteps")`.
const MIN_MAX_RESULTS: i64 = 1;
const MAX_MAX_RESULTS: i64 = 100;

/// The longest `name` a step summary carries: `StepName` is `len 1..512`, from
/// `smithy_query(query="shape", shape="StepSummary")`.
const MAX_STEP_NAME: usize = 512;

/// How long a test waits for a condition the clock owes before it gives up.
///
/// Generous on purpose: every wait here polls for the CONDITION and returns as soon as it
/// holds, so this bounds only a run in which the emulator never gets there.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// Collection path of the step resource, from
/// `smithy_query(query="operation", operation="ListSteps")`:
/// `GET /2023-10-12/farms/{farmId}/queues/{queueId}/jobs/{jobId}/steps`.
fn steps_path(farm_id: &str, queue_id: &str, job_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs/{job_id}/steps")
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

    /// `ListSteps` for `job_id` with `query` appended, which must succeed.
    async fn list_steps(&self, case: &str, job_id: &str, query: &str) -> Value {
        let (status, body) = self
            .emulator
            .get(&format!(
                "{}{query}",
                steps_path(&self.farm_id, &self.queue_id, job_id)
            ))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListSteps to answer 200, got {status} with {body}"
        );
        body
    }
}

/// The `steps` array of a successful `ListSteps` answer.
fn steps_of(case: &str, body: &Value) -> Vec<Value> {
    body["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a steps list in {body}"))
        .clone()
}

/// A summary's `taskRunStatusCounts`, in an order a comparison does not depend on.
fn counts_of(case: &str, summary: &Value) -> BTreeMap<String, i64> {
    summary
        .get("taskRunStatusCounts")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: expected taskRunStatusCounts in {summary}"))
        .iter()
        .map(|(status, count)| (status.clone(), count.as_i64().unwrap_or_default()))
        .collect()
}

/// The counts a step of `tasks` tasks all carrying `status` reports.
fn counts(status: &str, tasks: i64) -> BTreeMap<String, i64> {
    let mut wanted = BTreeMap::new();
    if tasks > 0 {
        wanted.insert(status.to_string(), tasks);
    }
    wanted
}

/// Walk every page of `ListSteps` at `max_results` and return the step ids in the order
/// they were handed back.
async fn page_all(
    scheduled: &Scheduled,
    case: &str,
    job_id: &str,
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
        let body = scheduled.list_steps(case, job_id, &query).await;
        let page = steps_of(case, &body);
        assert!(
            page.len() as i64 <= max_results,
            "case {case:?}: expected at most {max_results} steps on a page, got {}",
            page.len()
        );
        for summary in &page {
            seen.push(string_at(summary, "stepId"));
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

/// One row of the table below: the case, the `steps` its template declares, and the name
/// and task count of each step that template declares, in declared order.
type DeclaredCase = (&'static str, Value, Vec<(String, i64)>);

#[tokio::test]
async fn list_steps_reports_the_steps_the_template_declared() {
    let scheduled = Scheduled::new("steps").await;

    // The rule: a job's steps are the ones its template declared, and each summary carries
    // that step's own name and the run its own tasks are in. The rows differ along what the
    // rule generalises over: how many steps the template declares, how many tasks each step
    // has (the points of its parameter space), the form that space is written in, and the
    // length of a step's name — at both published bounds of `StepName`.
    let longest_name = "n".repeat(MAX_STEP_NAME);
    let cases: Vec<DeclaredCase> = vec![
        ("a template declaring no step", json!([]), Vec::new()),
        (
            "one step with no parameter space",
            json!([bare_step("Render")]),
            vec![("Render".to_string(), 1)],
        ),
        (
            "step names at both published bounds of the name length",
            json!([bare_step("a"), bare_step(&longest_name)]),
            vec![("a".to_string(), 1), (longest_name.clone(), 1)],
        ),
        (
            "three steps whose parameter spaces differ",
            json!([
                bare_step("Solo"),
                parameter_space_step(
                    "Frames",
                    json!([{"name": "Frame", "type": "INT", "range": [1, 2, 3]}]),
                ),
                parameter_space_step(
                    "Tiles",
                    json!([
                        {"name": "Frame", "type": "INT", "range": [7, 8]},
                        {"name": "Side", "type": "STRING", "range": ["left", "right"]},
                    ]),
                ),
            ]),
            vec![
                ("Solo".to_string(), 1),
                ("Frames".to_string(), 3),
                ("Tiles".to_string(), 4),
            ],
        ),
        (
            "a step whose INT range expression declares a stride",
            json!([parameter_space_step(
                "Strided",
                json!([{"name": "Frame", "type": "INT", "range": "1-5:2"}]),
            )]),
            vec![("Strided".to_string(), 3)],
        ),
    ];

    for (case, steps, expected) in &cases {
        let job_id = scheduled.job("declared", steps.clone()).await;
        // Once the job's creation has converged its tasks carry the status it was submitted
        // with, which is the default `READY`: the one state every row can be read in.
        let job = scheduled
            .job_until(case, &job_id, "lifecycleStatus", "CREATE_COMPLETE")
            .await;

        let listed = scheduled.list_steps(case, &job_id, "").await;
        let summaries = steps_of(case, &listed);
        assert_eq!(
            summaries.len(),
            expected.len(),
            "case {case:?}: expected one summary per declared step, got {listed}"
        );
        assert!(
            listed.get("nextToken").is_none(),
            "case {case:?}: expected no nextToken on a complete page, got {listed}"
        );

        let names: Vec<String> = summaries
            .iter()
            .map(|summary| string_at(summary, "name"))
            .collect();
        let declared: Vec<String> = expected.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names, declared,
            "case {case:?}: expected the declared steps in declared order, got {names:?}"
        );

        let mut ids: Vec<String> = Vec::new();
        for (summary, (name, tasks)) in summaries.iter().zip(expected.iter()) {
            let step_id = string_at(summary, "stepId");
            assert!(
                is_resource_id(&step_id, "step"),
                "case {case:?}: expected a ^step-[0-9a-f]{{32}}$ id for {name:?}, got \
                 {step_id:?}"
            );
            ids.push(step_id);
            // `StepLifecycleStatus` is `CREATE_COMPLETE | UPDATE_IN_PROGRESS |
            // UPDATE_FAILED | UPDATE_SUCCEEDED`, from
            // `smithy_query(query="shape", shape="StepLifecycleStatus")`: a step created
            // with its job and never updated is complete.
            assert_eq!(
                string_at(summary, "lifecycleStatus"),
                "CREATE_COMPLETE",
                "case {case:?}: expected a created step for {name:?}, got {summary}"
            );
            // The step's tasks are the points of its parameter space, and every one of them
            // carries the status the job settled on.
            assert_eq!(
                counts_of(case, summary),
                counts("READY", *tasks),
                "case {case:?}: expected {tasks} READY tasks for {name:?}, got {summary}"
            );
            assert_eq!(
                summary.get("taskRunStatus").and_then(Value::as_str),
                Some("READY"),
                "case {case:?}: expected the step {name:?} to report its tasks' run status, \
                 got {summary}"
            );
            assert_eq!(
                summary.get("taskFailureRetryCount").and_then(Value::as_i64),
                Some(0),
                "case {case:?}: expected no retries for {name:?} before anything ran, got \
                 {summary}"
            );
            // Nothing has run the step, so its run has neither started nor ended, and the
            // lifecycle has settled, so no target is being worked towards.
            assert!(
                summary.get("startedAt").is_none() && summary.get("endedAt").is_none(),
                "case {case:?}: expected no run instants for {name:?} before anything ran, \
                 got {summary}"
            );
            assert!(
                summary.get("targetTaskRunStatus").is_none(),
                "case {case:?}: expected no target once the lifecycle settled for {name:?}, \
                 got {summary}"
            );
            assert!(
                summary.get("updatedAt").is_none() && summary.get("updatedBy").is_none(),
                "case {case:?}: expected no update members for {name:?} before an update, \
                 got {summary}"
            );
            // The steps are minted by the call that created the job, so that call is what
            // created them.
            assert_eq!(
                (
                    summary.get("createdAt").cloned(),
                    summary.get("createdBy").cloned(),
                ),
                (job.get("createdAt").cloned(), job.get("createdBy").cloned()),
                "case {case:?}: expected the creating call of the job on {name:?}, got \
                 {summary} against {job}"
            );
        }

        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            ids.len(),
            "case {case:?}: expected a distinct id per step, got {ids:?}"
        );

        // A read does not change what the next reader sees.
        let again = scheduled.list_steps(case, &job_id, "").await;
        assert_eq!(
            again, listed,
            "case {case:?}: a second ListSteps answered differently: {listed} then {again}"
        );
    }
}

#[tokio::test]
async fn list_steps_pages_over_every_step_exactly_once() {
    let scheduled = Scheduled::new("paging").await;
    let declared: Vec<Value> = (0..5).map(|index| bare_step(&format!("step-{index}"))).collect();
    let job_id = scheduled.job("paging", Value::Array(declared)).await;
    scheduled
        .job_until("paging", &job_id, "lifecycleStatus", "CREATE_COMPLETE")
        .await;

    let unpaged = scheduled.list_steps("the default page", &job_id, "").await;
    let order: Vec<String> = steps_of("the default page", &unpaged)
        .iter()
        .map(|summary| string_at(summary, "stepId"))
        .collect();
    assert_eq!(
        order.len(),
        5,
        "expected every declared step on the default page, got {unpaged}"
    );

    // The rule: paging hands back every step exactly once, in at most `maxResults` per
    // page, in the one order the unpaged list reports. The rows are page sizes that divide
    // the set evenly, unevenly and exceed it, plus both published bounds of `maxResults`.
    for max_results in [MIN_MAX_RESULTS, 2, 3, 5, MAX_MAX_RESULTS] {
        let case = format!("maxResults {max_results}");
        let seen = page_all(&scheduled, &case, &job_id, max_results).await;
        assert_eq!(
            seen, order,
            "case {case:?}: expected the unpaged order {order:?}, got {seen:?}"
        );
    }

    // An unknown token is a client error, not an empty page.
    let unknown = absent_id("step", "00000000000000000000000000000009");
    let (status, body) = scheduled
        .emulator
        .get(&format!(
            "{}?nextToken={unknown}",
            steps_path(&scheduled.farm_id, &scheduled.queue_id, &job_id)
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
async fn list_steps_reports_only_the_steps_of_the_job_in_its_path() {
    let scheduled = Scheduled::new("own-job").await;
    let other_queue_id = scheduled.emulator.queue(&scheduled.farm_id, "other").await;

    let left = scheduled
        .job("left", json!([bare_step("Left"), bare_step("LeftToo")]))
        .await;
    let right = scheduled.job("right", json!([bare_step("Right")])).await;
    let empty = scheduled.job("empty", json!([])).await;
    for job_id in [&left, &right, &empty] {
        scheduled
            .job_until("own-job", job_id, "lifecycleStatus", "CREATE_COMPLETE")
            .await;
    }

    // The rule: a step hangs off its own job, so a list reports that job's steps and no
    // other job's — a job whose template declared none lists none.
    let cases: Vec<(&str, &String, Vec<&str>)> = vec![
        ("the job declaring two steps", &left, vec!["Left", "LeftToo"]),
        ("the job declaring one step", &right, vec!["Right"]),
        ("a job declaring no step", &empty, Vec::new()),
    ];
    for (case, job_id, wanted) in &cases {
        let listed = scheduled.list_steps(case, job_id, "").await;
        let names: Vec<String> = steps_of(case, &listed)
            .iter()
            .map(|summary| string_at(summary, "name"))
            .collect();
        assert_eq!(
            names,
            wanted.iter().map(|name| name.to_string()).collect::<Vec<String>>(),
            "case {case:?}: expected exactly this job's steps, got {names:?}"
        );
    }

    // Every level of the path is reported as itself when this emulator does not hold it,
    // including a job of another queue of the same farm, which is not this queue's job.
    let no_farm = absent_id("farm", "0000000000000000000000000000000a");
    let no_queue = absent_id("queue", "0000000000000000000000000000000b");
    let no_job = absent_id("job", "0000000000000000000000000000000c");
    let cases: Vec<(&str, String, &str, String)> = vec![
        (
            "an unheld farm",
            steps_path(&no_farm, &scheduled.queue_id, &left),
            "farm",
            no_farm.clone(),
        ),
        (
            "an unheld queue",
            steps_path(&scheduled.farm_id, &no_queue, &left),
            "queue",
            no_queue.clone(),
        ),
        (
            "an unheld job",
            steps_path(&scheduled.farm_id, &scheduled.queue_id, &no_job),
            "job",
            no_job.clone(),
        ),
        (
            "a job of another queue of the same farm",
            steps_path(&scheduled.farm_id, &other_queue_id, &left),
            "job",
            left.clone(),
        ),
    ];
    for (case, path, resource_type, resource_id) in &cases {
        let (status, body) = scheduled.emulator.get(path).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected ListSteps to answer 404, got {status} with {body}"
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
                format!(
                    "Resource of type {resource_type} with id {resource_id} does not exist."
                ),
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
async fn a_steps_reported_run_follows_what_its_tasks_did() {
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

    // One step per task count, so the rule — a step reports the run ITS OWN tasks are in —
    // is read off a step with one task and a step with several.
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

    run_every_task(case, &scheduled, &worker_id, 32).await;
    let job = scheduled
        .job_until(case, &job_id, "taskRunStatus", "SUCCEEDED")
        .await;

    let listed = scheduled.list_steps(case, &job_id, "").await;
    let summaries = steps_of(case, &listed);
    let expected: Vec<(&str, i64)> = vec![("Solo", 1), ("Frames", 2)];
    assert_eq!(
        summaries.len(),
        expected.len(),
        "case {case:?}: expected one summary per declared step, got {listed}"
    );
    for (summary, (name, tasks)) in summaries.iter().zip(expected.iter()) {
        assert_eq!(
            string_at(summary, "name"),
            *name,
            "case {case:?}: expected the declared steps in declared order, got {listed}"
        );
        assert_eq!(
            counts_of(case, summary),
            counts("SUCCEEDED", *tasks),
            "case {case:?}: expected {tasks} succeeded tasks for {name:?}, got {summary}"
        );
        assert_eq!(
            summary.get("taskRunStatus").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected {name:?} to report its tasks' run, got {summary}"
        );
        // The step's run started when its first task did and ended when its last one did,
        // and nothing failed, so nothing was retried.
        assert!(
            summary.get("startedAt").and_then(Value::as_str).is_some()
                && summary.get("endedAt").and_then(Value::as_str).is_some(),
            "case {case:?}: expected a started and an ended instant for {name:?}, got \
             {summary}"
        );
        assert_eq!(
            summary.get("taskFailureRetryCount").and_then(Value::as_i64),
            Some(0),
            "case {case:?}: expected no retries for {name:?}, got {summary}"
        );
        // A worker's report is what moved the step's tasks, so the step carries when it was
        // last changed and no principal that changed it.
        assert!(
            summary.get("updatedAt").and_then(Value::as_str).is_some()
                && summary.get("updatedBy").is_none(),
            "case {case:?}: expected a service-made update on {name:?}, got {summary}"
        );
        // Every step of the job ran, so the job reports the same run its steps do.
        assert_eq!(
            summary.get("taskRunStatus"),
            job.get("taskRunStatus"),
            "case {case:?}: expected {name:?} to agree with the job, got {summary} against \
             {job}"
        );
    }
}
