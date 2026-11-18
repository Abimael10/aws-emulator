// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the worker's own half of the scheduler:
//! `UpdateWorkerSchedule`, and the session it hands a worker, read back through
//! `GetSession`.
//!
//! The rules under test:
//!
//! * a job's steps and tasks are what its template declares — one task per point of a
//!   step's parameter space, carrying that point's parameters as the `TaskParameterValue`
//!   member its declared type names — and the schedule hands those tasks, one at a time, to
//!   a worker of a fleet the job's queue is associated with;
//! * a worker reporting a task action `SUCCEEDED` completes that task, and a job whose
//!   tasks have all succeeded is `SUCCEEDED`, with the per-status counts every reader of a
//!   job reports;
//! * a task a worker reports `FAILED` is run again while the job's `maxRetriesPerTask`
//!   allows it and then the job is `FAILED`; one the worker reports `INTERRUPTED` goes back
//!   to waiting without charging a retry;
//! * a session enters the environments the job runs under — the queue's, then the
//!   template's job environments, then the step's — before the task runs, and exits them in
//!   reverse once it has;
//! * nothing is handed to a worker whose fleet no scheduling association reaches, for a job
//!   that is not waiting to run, or for work this emulator did not read out of the template;
//! * a worker that has reported itself stopping is asked to stop and asked for its unfinished
//!   actions back;
//! * the schedule is reached only through the worker's own farm and fleet, and a worker
//!   reports only on the actions the service handed it, so each level it cannot find is a
//!   404 naming that level.
//!
//! This suite is a module of the `worker` suite rather than a second file under
//! `tests/all/`: this crate declares its suites by hand in `tests/all.rs`, and a file
//! deeper than `tests/all/` is declared by its own parent module (which is what the
//! crate's build-script layout check expects).
//!
//! The harness (`Emulator`) and the farm, fleet and worker helpers are the ones the other
//! suites drive the service with, so these tests go through routing, deserialization,
//! constraint validation, the handler and serialization, exactly as the router does in
//! production.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::farm_queue_job::{absent_id, error_code, is_resource_id, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::create_worker;

/// How long a test waits for a condition the clock owes before it gives up.
///
/// Generous on purpose: every wait here polls for the CONDITION and returns as soon as it
/// holds, so this bounds only a run in which the emulator never gets there.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

fn schedule_path(farm_id: &str, fleet_id: &str, worker_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/fleets/{fleet_id}/workers/{worker_id}/schedule")
}

fn jobs_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs")
}

fn job_path(farm_id: &str, queue_id: &str, job_id: &str) -> String {
    format!("{}/{job_id}", jobs_path(farm_id, queue_id))
}

fn session_path(farm_id: &str, queue_id: &str, job_id: &str, session_id: &str) -> String {
    format!(
        "{}/sessions/{session_id}",
        job_path(farm_id, queue_id, job_id)
    )
}

/// An Open Job Description job template carrying `steps`, written in JSON.
fn template(name: &str, steps: Value, job_environments: Option<Value>) -> String {
    let mut document = json!({
        "specificationVersion": "jobtemplate-2023-09",
        "name": name,
        "steps": steps,
    });
    if let (Some(document), Some(environments)) = (document.as_object_mut(), job_environments) {
        document.insert("jobEnvironments".to_string(), environments);
    }
    document.to_string()
}

/// One step with no parameter space: one task, with no parameters.
fn one_task_step(name: &str) -> Value {
    json!([{
        "name": name,
        "script": {"actions": {"onRun": {"command": "echo", "args": ["{{Task.File.Run}}"]}}},
    }])
}

/// One step whose parameter space declares `definitions`.
fn parameter_space_step(name: &str, definitions: Value) -> Value {
    json!([{
        "name": name,
        "parameterSpace": {"taskParameterDefinitions": definitions},
        "script": {"actions": {"onRun": {"command": "echo"}}},
    }])
}

/// One worker of one fleet, on a queue whose work that fleet is scheduling.
struct Scheduled {
    emulator: Emulator,
    farm_id: String,
    queue_id: String,
    fleet_id: String,
    worker_id: String,
}

impl Scheduled {
    /// Build the farm, queue, fleet, worker and association every scheduling test needs.
    async fn new(name: &str) -> Self {
        let emulator = Emulator::new();
        let farm_id = emulator.farm(name).await;
        let queue_id = emulator.queue(&farm_id, name).await;
        let fleet_id = create_fleet(&emulator, &farm_id, name).await;
        let (status, created) = create_worker(&emulator, &farm_id, &fleet_id, json!({}), None).await;
        assert_eq!(
            status, 200,
            "CreateWorker {name}: expected 200, got {status} with {created}"
        );
        let worker_id = string_at(&created, "workerId");
        let scheduled = Self {
            emulator,
            farm_id,
            queue_id,
            fleet_id,
            worker_id,
        };
        scheduled.associate(&scheduled.queue_id.clone()).await;
        scheduled
    }

    /// Put `queue_id`'s work on this fleet, the way `CreateQueueFleetAssociation` is bound.
    async fn associate(&self, queue_id: &str) {
        let (status, body) = self
            .emulator
            .call(
                "PUT",
                &format!("/2023-10-12/farms/{}/queue-fleet-associations", self.farm_id),
                Some(json!({"queueId": queue_id, "fleetId": self.fleet_id})),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueueFleetAssociation: expected 200, got {status} with {body}"
        );
    }

    /// Create a job from `body` under this queue and return its id.
    async fn job(&self, body: Value) -> String {
        let (status, created) = self
            .emulator
            .post(&jobs_path(&self.farm_id, &self.queue_id), body)
            .await;
        assert_eq!(
            status, 201,
            "CreateJob: expected 201, got {status} with {created}"
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
    /// The move a created job makes is owed by the clock, so this waits for the CONDITION
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

    /// One `UpdateWorkerSchedule` call carrying `reported`.
    async fn schedule(&self, reported: Map<String, Value>) -> (u16, Value) {
        let body = if reported.is_empty() {
            json!({})
        } else {
            json!({"updatedSessionActions": Value::Object(reported)})
        };
        self.emulator
            .call(
                "PATCH",
                &schedule_path(&self.farm_id, &self.fleet_id, &self.worker_id),
                Some(body),
                None,
            )
            .await
    }

    /// One `UpdateWorkerSchedule` call that must succeed.
    async fn schedule_ok(&self, case: &str, reported: Map<String, Value>) -> Value {
        let (status, response) = self.schedule(reported).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateWorkerSchedule to answer 200, got {status} with \
             {response}"
        );
        response
    }

    /// The sessions a schedule response assigned, as `(sessionId, session)` pairs sorted by
    /// id.
    fn assigned(case: &str, response: &Value) -> Vec<(String, Value)> {
        let sessions = response
            .get("assignedSessions")
            .and_then(Value::as_object)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected an assignedSessions map, got {response}")
            });
        let mut pairs: Vec<(String, Value)> = sessions
            .iter()
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        pairs.sort_by(|left, right| left.0.cmp(&right.0));
        pairs
    }
}

/// One task action a worker was handed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HandedTask {
    session_id: String,
    step_id: String,
    task_id: String,
    /// The task's parameters, as the response carried them.
    parameters: String,
}

/// Every action of every session a response assigned, in the order the session holds them.
fn actions(case: &str, response: &Value) -> Vec<(String, String, Value)> {
    let mut out = Vec::new();
    for (session_id, session) in Scheduled::assigned(case, response) {
        let list = session
            .get("sessionActions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected sessionActions in the session {session}")
            });
        for action in list {
            let action_id = string_at(&action, "sessionActionId");
            let definition = action
                .get("definition")
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: expected a definition in {action}"));
            out.push((session_id.clone(), action_id, definition));
        }
    }
    out
}

/// Run the worker until a poll hands it nothing, reporting every task action with
/// `task_outcome` and every environment action as succeeded.
///
/// Answers every task action the worker was handed, in the order it was handed them.
async fn run_worker(
    case: &str,
    scheduled: &Scheduled,
    task_outcome: &str,
    max_polls: usize,
) -> Vec<HandedTask> {
    let mut handed: Vec<HandedTask> = Vec::new();
    for poll in 0..max_polls {
        let response = scheduled.schedule_ok(case, Map::new()).await;
        let handed_now = actions(case, &response);
        if handed_now.is_empty() {
            return handed;
        }
        let mut reported = Map::new();
        for (session_id, action_id, definition) in handed_now {
            if let Some(task_run) = definition.get("taskRun") {
                handed.push(HandedTask {
                    session_id,
                    step_id: string_at(task_run, "stepId"),
                    task_id: string_at(task_run, "taskId"),
                    parameters: parameter_key(
                        &task_run
                            .get("parameters")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    ),
                });
                reported.insert(
                    action_id,
                    json!({"completedStatus": task_outcome, "processExitCode": 0}),
                );
            } else {
                reported.insert(action_id, json!({"completedStatus": "SUCCEEDED"}));
            }
        }
        let response = scheduled.schedule_ok(case, reported).await;
        assert_eq!(
            response.get("desiredWorkerStatus"),
            None,
            "case {case:?}: poll {poll} expected no desiredWorkerStatus for a running worker, \
             got {response}"
        );
    }
    panic!("case {case:?}: the worker was still being handed work after {max_polls} polls");
}

/// The parameters of a task as a value that does not depend on the order a map was
/// serialized in: one `name=<value>` entry per parameter, sorted.
///
/// A `TaskParameters` map has no modeled order, so comparing the raw JSON text would compare
/// the server's map iteration order rather than the task set.
fn parameter_key(parameters: &Value) -> String {
    let mut entries: Vec<String> = parameters
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries.join(",")
}

/// The parameters a task is expected to carry, in the same order-independent form.
fn parameters(values: &[(&str, &str, &str)]) -> String {
    let mut map = Map::new();
    for (name, member, value) in values {
        map.insert((*name).to_string(), json!({*member: *value}));
    }
    parameter_key(&Value::Object(map))
}

#[tokio::test]
async fn a_worker_is_handed_every_task_of_a_job_and_reporting_them_completes_it() {
    // The rule: a step's tasks are the points of its parameter space, each carrying that
    // point's parameters, and a worker is handed them one at a time until the job is done.
    // The rows differ along what the parameter space is — absent, a declared list of
    // values, an INT range expression, a product of two parameters, each declared type —
    // and along how many steps the template declares.
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a step with no parameter space",
            one_task_step("Render"),
            vec![parameters(&[])],
        ),
        (
            "an INT range declared as a list",
            parameter_space_step(
                "Frames",
                json!([{"name": "Frame", "type": "INT", "range": [1, 2, 3]}]),
            ),
            vec![
                parameters(&[("Frame", "int", "1")]),
                parameters(&[("Frame", "int", "2")]),
                parameters(&[("Frame", "int", "3")]),
            ],
        ),
        (
            "an INT range expression with a step",
            parameter_space_step(
                "Frames",
                json!([{"name": "Frame", "type": "INT", "range": "1-5:2"}]),
            ),
            vec![
                parameters(&[("Frame", "int", "1")]),
                parameters(&[("Frame", "int", "3")]),
                parameters(&[("Frame", "int", "5")]),
            ],
        ),
        (
            "a product of two parameters",
            parameter_space_step(
                "Tiles",
                json!([
                    {"name": "Frame", "type": "INT", "range": [7, 8]},
                    {"name": "Side", "type": "STRING", "range": ["left", "right"]},
                ]),
            ),
            vec![
                parameters(&[("Frame", "int", "7"), ("Side", "string", "left")]),
                parameters(&[("Frame", "int", "7"), ("Side", "string", "right")]),
                parameters(&[("Frame", "int", "8"), ("Side", "string", "left")]),
                parameters(&[("Frame", "int", "8"), ("Side", "string", "right")]),
            ],
        ),
        (
            "a PATH and a FLOAT parameter",
            parameter_space_step(
                "Scenes",
                json!([
                    {"name": "Scene", "type": "PATH", "range": ["/mnt/scene.blend"]},
                    {"name": "Scale", "type": "FLOAT", "range": ["0.5"]},
                ]),
            ),
            vec![parameters(&[
                ("Scale", "float", "0.5"),
                ("Scene", "path", "/mnt/scene.blend"),
            ])],
        ),
        (
            "two steps of one task each",
            json!([
                {"name": "First", "script": {"actions": {"onRun": {"command": "a"}}}},
                {"name": "Second", "script": {"actions": {"onRun": {"command": "b"}}}},
            ]),
            vec![parameters(&[]), parameters(&[])],
        ),
    ];

    for (case, steps, expected) in cases {
        let scheduled = Scheduled::new("handed").await;
        let job_id = scheduled
            .job(json!({
                "template": template(case, steps, None),
                "templateType": "JSON",
                "priority": 50,
            }))
            .await;
        scheduled
            .job_until(case, &job_id, "taskRunStatus", "READY")
            .await;

        let handed = run_worker(case, &scheduled, "SUCCEEDED", expected.len() + 2).await;

        assert_eq!(
            handed.len(),
            expected.len(),
            "case {case:?}: expected {} task(s) to be handed over, got {handed:?}",
            expected.len()
        );
        let mut seen: Vec<String> = handed
            .iter()
            .map(|task| task.parameters.clone())
            .collect();
        seen.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            seen, wanted,
            "case {case:?}: expected the parameter space's points, got {handed:?}"
        );
        let mut task_ids: Vec<String> = handed.iter().map(|task| task.task_id.clone()).collect();
        task_ids.sort();
        task_ids.dedup();
        assert_eq!(
            task_ids.len(),
            expected.len(),
            "case {case:?}: expected one task id per point, got {handed:?}"
        );
        for task in &handed {
            assert!(
                is_resource_id(&task.step_id, "step"),
                "case {case:?}: expected a minted ^step-[0-9a-f]{{32}}$ id, got {task:?}"
            );
            let hex = task.step_id.trim_start_matches("step-");
            assert!(
                task.task_id.starts_with(&format!("task-{hex}-")),
                "case {case:?}: expected a task id of its own step, got {task:?}"
            );
            assert!(
                is_resource_id(&task.session_id, "session"),
                "case {case:?}: expected a minted ^session-[0-9a-f]{{32}}$ id, got {task:?}"
            );
        }

        // Every task succeeded, so the job did: that is what every reader of it reports.
        let job = scheduled.read_job(&job_id).await;
        assert_eq!(
            job.get("taskRunStatus").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the job to have succeeded, got {job}"
        );
        assert_eq!(
            job.get("taskRunStatusCounts"),
            Some(&json!({"SUCCEEDED": expected.len()})),
            "case {case:?}: expected one SUCCEEDED count per task, got {job}"
        );
        assert!(
            job.get("startedAt").and_then(Value::as_str).is_some()
                && job.get("endedAt").and_then(Value::as_str).is_some(),
            "case {case:?}: expected the run's start and end to be recorded, got {job}"
        );
        assert_eq!(
            job.get("taskFailureRetryCount").and_then(Value::as_i64),
            Some(0),
            "case {case:?}: expected no retries for a run that succeeded, got {job}"
        );

        // Nothing is left for the worker: a further poll hands it no session at all.
        let response = scheduled.schedule_ok(case, Map::new()).await;
        assert_eq!(
            Scheduled::assigned(case, &response),
            Vec::new(),
            "case {case:?}: expected no further work, got {response}"
        );
        assert!(
            response
                .get("updateIntervalSeconds")
                .and_then(Value::as_i64)
                .is_some_and(|interval| interval > 0),
            "case {case:?}: expected the poll interval the schedule asks for, got {response}"
        );
        assert_eq!(
            response.get("cancelSessionActions"),
            Some(&json!({})),
            "case {case:?}: expected nothing to cancel, got {response}"
        );
    }
}

#[tokio::test]
async fn the_session_a_worker_is_handed_is_read_back_through_get_session() {
    let scheduled = Scheduled::new("read-back").await;
    let job_id = scheduled
        .job(json!({
            "template": template("read-back", one_task_step("Render"), None),
            "templateType": "JSON",
            "priority": 50,
        }))
        .await;
    scheduled
        .job_until("read back", &job_id, "taskRunStatus", "READY")
        .await;

    let response = scheduled.schedule_ok("read back", Map::new()).await;
    let assigned = Scheduled::assigned("read back", &response);
    assert_eq!(
        assigned.len(),
        1,
        "expected one assigned session, got {response}"
    );
    let (session_id, session) = assigned[0].clone();
    assert_eq!(
        (
            string_at(&session, "queueId"),
            string_at(&session, "jobId"),
        ),
        (scheduled.queue_id.clone(), job_id.clone()),
        "expected the session to name the job it runs, got {session}"
    );
    assert_eq!(
        session
            .get("logConfiguration")
            .and_then(|log| log.get("logDriver")),
        Some(&json!("awslogs")),
        "expected the session's log driver, got {session}"
    );

    let path = session_path(
        &scheduled.farm_id,
        &scheduled.queue_id,
        &job_id,
        &session_id,
    );

    let (status, read_back) = scheduled.emulator.get(&path).await;
    assert_eq!(
        status, 200,
        "GetSession: expected 200, got {status} with {read_back}"
    );
    assert_eq!(
        (
            string_at(&read_back, "sessionId"),
            string_at(&read_back, "workerId"),
            string_at(&read_back, "fleetId"),
            string_at(&read_back, "lifecycleStatus"),
        ),
        (
            session_id.clone(),
            scheduled.worker_id.clone(),
            scheduled.fleet_id.clone(),
            "STARTED".to_string(),
        ),
        "expected the session as the schedule assigned it, got {read_back}"
    );
    assert!(
        read_back.get("startedAt").and_then(Value::as_str).is_some()
            && read_back.get("endedAt").is_none(),
        "expected a started, unfinished session, got {read_back}"
    );
    assert_eq!(
        read_back
            .get("log")
            .and_then(|log| log.get("options"))
            .and_then(|options| options.get("logStreamName")),
        Some(&json!(session_id)),
        "expected the session's own log stream, got {read_back}"
    );

    // A read does not change what the next reader sees.
    let (_, again) = scheduled.emulator.get(&path).await;
    assert_eq!(
        again, read_back,
        "two reads of one session disagreed: {read_back} then {again}"
    );

    // Running the session to its end ends it, which the same read reports.
    run_worker("read back", &scheduled, "SUCCEEDED", 4).await;
    let (status, ended) = scheduled.emulator.get(&path).await;
    assert_eq!(
        (status, string_at(&ended, "lifecycleStatus")),
        (200, "ENDED".to_string()),
        "expected the finished session to have ended, got {status} with {ended}"
    );
    assert!(
        ended.get("endedAt").and_then(Value::as_str).is_some(),
        "expected the end of the session to be recorded, got {ended}"
    );

    // A session is reached only through its own farm, queue and job.
    let cases: Vec<(&str, String, &str, String)> = vec![
        (
            "an unheld farm",
            session_path(
                &absent_id("farm", "000000000000000000000000000000a1"),
                &scheduled.queue_id,
                &job_id,
                &session_id,
            ),
            "farm",
            absent_id("farm", "000000000000000000000000000000a1"),
        ),
        (
            "an unheld queue",
            session_path(
                &scheduled.farm_id,
                &absent_id("queue", "000000000000000000000000000000a2"),
                &job_id,
                &session_id,
            ),
            "queue",
            absent_id("queue", "000000000000000000000000000000a2"),
        ),
        (
            "an unheld job",
            session_path(
                &scheduled.farm_id,
                &scheduled.queue_id,
                &absent_id("job", "000000000000000000000000000000a3"),
                &session_id,
            ),
            "job",
            absent_id("job", "000000000000000000000000000000a3"),
        ),
        (
            "an unheld session",
            session_path(
                &scheduled.farm_id,
                &scheduled.queue_id,
                &job_id,
                &absent_id("session", "000000000000000000000000000000a4"),
            ),
            "session",
            absent_id("session", "000000000000000000000000000000a4"),
        ),
    ];
    for (case, path, resource_type, resource_id) in cases {
        let (status, body) = scheduled.emulator.get(&path).await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {case:?}: expected GetSession to answer a 404 naming what is missing, got \
             {status} with {body}"
        );
    }
}

#[tokio::test]
async fn a_session_enters_the_jobs_environments_before_its_task_and_exits_them_after() {
    let scheduled = Scheduled::new("environments").await;
    // A queue environment is entered before the template's own, and the queue orders its
    // environments by priority, 0 being the highest (activated first).
    for (priority, name) in [(5, "QueueSecond"), (0, "QueueFirst")] {
        let (status, created) = scheduled
            .emulator
            .call(
                "POST",
                &format!(
                    "/2023-10-12/farms/{}/queues/{}/environments",
                    scheduled.farm_id, scheduled.queue_id
                ),
                Some(json!({
                    "priority": priority,
                    "templateType": "JSON",
                    "template": json!({
                        "specificationVersion": "environment-2023-09",
                        "environment": {"name": name, "variables": {"SEAT": "1"}},
                    })
                    .to_string(),
                })),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueueEnvironment {name}: expected 200, got {status} with {created}"
        );
    }

    let steps = json!([{
        "name": "Render",
        "stepEnvironments": [{"name": "StepEnv", "script": {"actions": {"onEnter": {"command": "e"}}}}],
        "script": {"actions": {"onRun": {"command": "r"}}},
    }]);
    let job_id = scheduled
        .job(json!({
            "template": template(
                "environments",
                steps,
                Some(json!([{"name": "JobEnv", "script": {"actions": {"onEnter": {"command": "j"}}}}])),
            ),
            "templateType": "JSON",
            "priority": 50,
        }))
        .await;
    scheduled
        .job_until("environments", &job_id, "taskRunStatus", "READY")
        .await;

    // The rule: the session enters the queue's environments in priority order, then the
    // job template's, then the step's, and only then runs the task.
    let response = scheduled.schedule_ok("environments", Map::new()).await;
    let handed = actions("environments", &response);
    let shapes: Vec<String> = handed
        .iter()
        .map(|(_, _, definition)| describe(definition, &job_id))
        .collect();
    assert_eq!(
        shapes,
        vec![
            format!("envEnter JOB:{job_id}:QueueFirst"),
            format!("envEnter JOB:{job_id}:QueueSecond"),
            format!("envEnter JOB:{job_id}:JobEnv"),
            "envEnter STEP:StepEnv".to_string(),
            "taskRun".to_string(),
        ],
        "expected the environments entered before the task, got {response}"
    );

    let mut reported = Map::new();
    for (_, action_id, _) in &handed {
        reported.insert(action_id.clone(), json!({"completedStatus": "SUCCEEDED"}));
    }
    let response = scheduled.schedule_ok("environments", reported).await;

    // Everything it was handed is finished and the job holds no further task, so the
    // session exits what it entered, innermost first.
    let exits: Vec<String> = actions("environments", &response)
        .iter()
        .map(|(_, _, definition)| describe(definition, &job_id))
        .collect();
    assert_eq!(
        exits,
        vec![
            "envExit STEP:StepEnv".to_string(),
            format!("envExit JOB:{job_id}:JobEnv"),
            format!("envExit JOB:{job_id}:QueueSecond"),
            format!("envExit JOB:{job_id}:QueueFirst"),
        ],
        "expected the entered environments to be exited in reverse, got {response}"
    );

    let mut reported = Map::new();
    for (_, action_id, _) in actions("environments", &response) {
        reported.insert(action_id, json!({"completedStatus": "SUCCEEDED"}));
    }
    let response = scheduled.schedule_ok("environments", reported).await;
    assert_eq!(
        Scheduled::assigned("environments", &response),
        Vec::new(),
        "expected the session to be over, got {response}"
    );
    let job = scheduled.read_job(&job_id).await;
    assert_eq!(
        job.get("taskRunStatus").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "expected the job to have succeeded, got {job}"
    );

    // A job that declares no environment is handed its task and nothing else.
    let bare = Scheduled::new("no-environments").await;
    let bare_job = bare
        .job(json!({
            "template": template("bare", one_task_step("Render"), None),
            "templateType": "JSON",
            "priority": 50,
        }))
        .await;
    bare.job_until("no environments", &bare_job, "taskRunStatus", "READY")
        .await;
    let response = bare.schedule_ok("no environments", Map::new()).await;
    let shapes: Vec<String> = actions("no environments", &response)
        .iter()
        .map(|(_, _, definition)| describe(definition, &bare_job))
        .collect();
    assert_eq!(
        shapes,
        vec!["taskRun".to_string()],
        "expected only the task to be handed over, got {response}"
    );
}

/// One assigned action as a short shape, for comparing a whole session in order: the union
/// member it carries and, for an environment, the id it names (with a step id reduced to
/// `STEP:` plus the name, which the test does not know in advance).
fn describe(definition: &Value, job_id: &str) -> String {
    for member in ["envEnter", "envExit"] {
        if let Some(environment) = definition.get(member) {
            let id = string_at(environment, "environmentId");
            let shortened = match id.strip_prefix("STEP:") {
                Some(rest) => format!(
                    "STEP:{}",
                    rest.split_once(':').map_or(rest, |(_, name)| name)
                ),
                None => id,
            };
            assert!(
                shortened.starts_with("STEP:") || shortened.starts_with(&format!("JOB:{job_id}:")),
                "expected an environment id of this job or one of its steps, got {shortened}"
            );
            return format!("{member} {shortened}");
        }
    }
    for member in ["taskRun", "syncInputJobAttachments"] {
        if definition.get(member).is_some() {
            return member.to_string();
        }
    }
    panic!("expected a known session action definition, got {definition}")
}

#[tokio::test]
async fn a_failed_task_is_handed_out_again_while_the_job_allows_a_retry() {
    // The rule: a task a worker reports FAILED is run again while `maxRetriesPerTask`
    // allows it, and then the job has failed. The rows differ along how many retries the
    // job allows, including none at all.
    for retries in [0_i64, 1, 2] {
        let case = format!("maxRetriesPerTask {retries}");
        let scheduled = Scheduled::new("retries").await;
        let job_id = scheduled
            .job(json!({
                "template": template(&case, one_task_step("Render"), None),
                "templateType": "JSON",
                "priority": 50,
                "maxRetriesPerTask": retries,
            }))
            .await;
        scheduled
            .job_until(&case, &job_id, "taskRunStatus", "READY")
            .await;

        let handed = run_worker(&case, &scheduled, "FAILED", retries as usize + 4).await;
        assert_eq!(
            handed.len() as i64,
            retries + 1,
            "case {case:?}: expected the first run plus {retries} retries, got {handed:?}"
        );
        let task_ids: Vec<&String> = handed.iter().map(|task| &task.task_id).collect();
        assert!(
            task_ids.windows(2).all(|pair| pair[0] == pair[1]),
            "case {case:?}: expected every run to be of the one task, got {handed:?}"
        );

        let job = scheduled.read_job(&job_id).await;
        assert_eq!(
            (
                job.get("taskRunStatus").and_then(Value::as_str),
                job.get("taskRunStatusCounts").cloned(),
                job.get("taskFailureRetryCount").and_then(Value::as_i64),
            ),
            (
                Some("FAILED"),
                Some(json!({"FAILED": 1})),
                Some(retries),
            ),
            "case {case:?}: expected the job to have failed after its retries, got {job}"
        );
    }

    // A run the worker never finished is not a failure: the task goes back to waiting and
    // no retry is charged for it.
    let case = "interrupted";
    let scheduled = Scheduled::new("interrupted").await;
    let job_id = scheduled
        .job(json!({
            "template": template(case, one_task_step("Render"), None),
            "templateType": "JSON",
            "priority": 50,
            "maxRetriesPerTask": 0,
        }))
        .await;
    scheduled
        .job_until(case, &job_id, "taskRunStatus", "READY")
        .await;

    for outcome in ["INTERRUPTED", "NEVER_ATTEMPTED"] {
        let response = scheduled.schedule_ok(case, Map::new()).await;
        let handed = actions(case, &response);
        assert_eq!(
            handed.len(),
            1,
            "case {case:?}: expected the task to be handed over before reporting {outcome}, \
             got {response}"
        );
        let mut reported = Map::new();
        reported.insert(handed[0].1.clone(), json!({"completedStatus": outcome}));
        let response = scheduled.schedule_ok(case, reported).await;
        // The run did not fail, so no retry is charged and the task is waiting again —
        // which this same call hands straight back to the worker.
        let again = actions(case, &response);
        assert_eq!(
            again.len(),
            1,
            "case {case:?}: expected the task to be handed out again after {outcome}, got \
             {response}"
        );
        let job = scheduled.read_job(&job_id).await;
        assert_eq!(
            (
                job.get("taskFailureRetryCount").and_then(Value::as_i64),
                job.get("taskRunStatusCounts")
                    .and_then(|counts| counts.get("FAILED"))
                    .cloned(),
            ),
            (Some(0), None),
            "case {case:?}: expected {outcome} to charge no retry and fail no task, got {job}"
        );
    }

    // And it still runs: the same task succeeds on the next attempt.
    let handed = run_worker(case, &scheduled, "SUCCEEDED", 4).await;
    assert_eq!(
        handed.len(),
        1,
        "case {case:?}: expected one further run, got {handed:?}"
    );
    let job = scheduled.read_job(&job_id).await;
    assert_eq!(
        job.get("taskRunStatus").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the job to have succeeded, got {job}"
    );
}

#[tokio::test]
async fn no_work_is_handed_to_a_worker_a_scheduling_queue_does_not_reach() {
    // A whole template's work is bounded in bytes too, so a template whose steps together
    // cost more than one template may describe is work this emulator did not read: nine
    // steps of 4096 single-parameter tasks each pass that allowance.
    let oversized_steps: Vec<Value> = (0..9)
        .map(|index| {
            json!({
                "name": format!("Step{index}"),
                "parameterSpace": {
                    "taskParameterDefinitions": [
                        {"name": "Frame", "type": "INT", "range": "1-4096"},
                    ],
                },
                "script": {"actions": {"onRun": {"command": "r"}}},
            })
        })
        .collect();

    // The rule: a worker is handed work only from a queue its fleet is scheduling, only
    // from a job whose tasks are waiting to run, and only from work this emulator read out
    // of the template. The rows differ along which of those does not hold.
    let cases: Vec<(&str, bool, Value, &str)> = vec![
        (
            "no association between the queue and the fleet",
            false,
            json!({"template": template("lonely", one_task_step("Render"), None), "templateType": "JSON", "priority": 50}),
            "READY",
        ),
        (
            "a job submitted suspended",
            true,
            json!({
                "template": template("suspended", one_task_step("Render"), None),
                "templateType": "JSON",
                "priority": 50,
                "targetTaskRunStatus": "SUSPENDED",
            }),
            "SUSPENDED",
        ),
        (
            "a template that declares no step",
            true,
            json!({"template": template("stepless", json!([]), None), "templateType": "JSON", "priority": 50}),
            "READY",
        ),
        (
            // The parameter space of a step that declares a combination expression
            // associates its parameters in a way this emulator does not read, so its task
            // set is not the product and nothing may stand in for it.
            "a parameter space this emulator does not read",
            true,
            json!({
                "template": template(
                    "combination",
                    json!([{
                        "name": "Tiles",
                        "parameterSpace": {
                            "taskParameterDefinitions": [
                                {"name": "Frame", "type": "INT", "range": [1, 2]},
                                {"name": "Side", "type": "STRING", "range": ["a", "b"]},
                            ],
                            "combination": "(Frame,Side)",
                        },
                        "script": {"actions": {"onRun": {"command": "r"}}},
                    }]),
                    None,
                ),
                "templateType": "JSON",
                "priority": 50,
            }),
            "READY",
        ),
        (
            "more stored work than one template may describe",
            true,
            json!({
                "template": template("oversized", Value::Array(oversized_steps), None),
                "templateType": "JSON",
                "priority": 50,
            }),
            "READY",
        ),
        (
            // The parameter's name is stored with every task of its step, so a name past
            // the length one may carry is a space this emulator does not enumerate either.
            "a task parameter name longer than one may carry",
            true,
            json!({
                "template": template(
                    "long-name",
                    json!([{
                        "name": "Frames",
                        "parameterSpace": {
                            "taskParameterDefinitions": [
                                {"name": "F".repeat(2000), "type": "INT", "range": [1, 2]},
                            ],
                        },
                        "script": {"actions": {"onRun": {"command": "r"}}},
                    }]),
                    None,
                ),
                "templateType": "JSON",
                "priority": 50,
            }),
            "READY",
        ),
    ];

    for (case, associate, body, run_status) in cases {
        let emulator = Emulator::new();
        let farm_id = emulator.farm(case).await;
        let queue_id = emulator.queue(&farm_id, case).await;
        let fleet_id = create_fleet(&emulator, &farm_id, case).await;
        let (status, created) = create_worker(&emulator, &farm_id, &fleet_id, json!({}), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateWorker expected 200, got {status} with {created}"
        );
        let worker_id = string_at(&created, "workerId");
        let scheduled = Scheduled {
            emulator,
            farm_id,
            queue_id,
            fleet_id,
            worker_id,
        };
        if associate {
            scheduled.associate(&scheduled.queue_id.clone()).await;
        }
        let job_id = scheduled.job(body).await;
        // The job's creation completes either way; what differs is whether anything may be
        // handed out of it.
        scheduled
            .job_until(case, &job_id, "lifecycleStatus", "CREATE_COMPLETE")
            .await;

        let response = scheduled.schedule_ok(case, Map::new()).await;
        assert_eq!(
            Scheduled::assigned(case, &response),
            Vec::new(),
            "case {case:?}: expected no work to be handed over, got {response}"
        );
        let job = scheduled.read_job(&job_id).await;
        assert_eq!(
            job.get("taskRunStatus").and_then(Value::as_str),
            Some(run_status),
            "case {case:?}: expected the job's tasks to be untouched, got {job}"
        );
    }
}

#[tokio::test]
async fn a_worker_that_reported_itself_stopping_is_asked_to_stop() {
    let scheduled = Scheduled::new("stopping").await;
    let job_id = scheduled
        .job(json!({
            "template": template("stopping", one_task_step("Render"), None),
            "templateType": "JSON",
            "priority": 50,
        }))
        .await;
    scheduled
        .job_until("stopping", &job_id, "taskRunStatus", "READY")
        .await;
    let response = scheduled.schedule_ok("stopping", Map::new()).await;
    let handed = actions("stopping", &response);
    assert_eq!(
        handed.len(),
        1,
        "expected the task to be handed over first, got {response}"
    );

    // The agent reports its own status, and a worker on its way down is one the service
    // wants stopped: it is asked to stop and asked for the action it still holds back.
    let (status, updated) = scheduled
        .emulator
        .call(
            "PATCH",
            &format!(
                "/2023-10-12/farms/{}/fleets/{}/workers/{}",
                scheduled.farm_id, scheduled.fleet_id, scheduled.worker_id
            ),
            Some(json!({"status": "STOPPING"})),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "UpdateWorker STOPPING: expected 200, got {status} with {updated}"
    );

    let response = scheduled.schedule_ok("stopping", Map::new()).await;
    assert_eq!(
        response.get("desiredWorkerStatus").and_then(Value::as_str),
        Some("STOPPED"),
        "expected the service to ask the worker to stop, got {response}"
    );
    assert_eq!(
        Scheduled::assigned("stopping", &response),
        Vec::new(),
        "expected a stopping worker to be handed no session, got {response}"
    );
    let mut expected_cancellations = Map::new();
    expected_cancellations.insert(
        handed[0].0.clone(),
        json!([handed[0].1.clone()]),
    );
    assert_eq!(
        response.get("cancelSessionActions"),
        Some(&Value::Object(expected_cancellations)),
        "expected the unfinished action to be asked back, got {response}"
    );
}

#[tokio::test]
async fn the_schedule_names_the_level_it_cannot_find() {
    let scheduled = Scheduled::new("not-found").await;
    let other_farm = scheduled.emulator.farm("stranger").await;
    let other_fleet = create_fleet(&scheduled.emulator, &other_farm, "stranger").await;
    let no_farm = absent_id("farm", "000000000000000000000000000000b1");
    let no_fleet = absent_id("fleet", "000000000000000000000000000000b2");
    let no_worker = absent_id("worker", "000000000000000000000000000000b3");

    // The rule: a worker's schedule is reached only through the worker's own farm and
    // fleet, so each level this emulator does not hold at that place is reported as that
    // level. The rows differ along which level is wrong.
    let cases: Vec<(&str, String, String, String, &str, String)> = vec![
        (
            "an unheld farm",
            no_farm.clone(),
            scheduled.fleet_id.clone(),
            scheduled.worker_id.clone(),
            "farm",
            no_farm.clone(),
        ),
        (
            "an unheld fleet",
            scheduled.farm_id.clone(),
            no_fleet.clone(),
            scheduled.worker_id.clone(),
            "fleet",
            no_fleet.clone(),
        ),
        (
            "a fleet of another farm",
            scheduled.farm_id.clone(),
            other_fleet.clone(),
            scheduled.worker_id.clone(),
            "fleet",
            other_fleet.clone(),
        ),
        (
            "an unheld worker",
            scheduled.farm_id.clone(),
            scheduled.fleet_id.clone(),
            no_worker.clone(),
            "worker",
            no_worker.clone(),
        ),
    ];
    for (case, farm_id, fleet_id, worker_id, resource_type, resource_id) in cases {
        let (status, body) = scheduled
            .emulator
            .call(
                "PATCH",
                &schedule_path(&farm_id, &fleet_id, &worker_id),
                Some(json!({})),
                None,
            )
            .await;
        assert_eq!(
            (
                status,
                error_code(&body),
                string_at(&body, "resourceType"),
                string_at(&body, "resourceId"),
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                resource_type.to_string(),
                resource_id.clone(),
            ),
            "case {case:?}: expected a 404 naming what is missing, got {status} with {body}"
        );
    }

    // A worker reports only on the actions the service handed IT, so an id that belongs to
    // no session of this worker is a session action it cannot report on. The id satisfies
    // the modeled `^sessionaction-[0-9a-f]{32}-(0|([1-9][0-9]{0,9}))$` pattern, so the
    // handler is what decides the outcome.
    let unknown = format!("sessionaction-{}-0", "0".repeat(32));
    let mut reported = Map::new();
    reported.insert(unknown.clone(), json!({"completedStatus": "SUCCEEDED"}));
    let (status, body) = scheduled.schedule(reported).await;
    assert_eq!(
        (
            status,
            error_code(&body),
            string_at(&body, "resourceType"),
            string_at(&body, "resourceId"),
        ),
        (
            404,
            "ResourceNotFoundException".to_string(),
            "session-action".to_string(),
            unknown.clone(),
        ),
        "expected a 404 naming the session action this worker does not hold, got {status} with \
         {body}"
    );
}
