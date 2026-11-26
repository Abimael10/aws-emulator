// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `BatchGetJobEntity`.
//!
//! The rules under test:
//!
//! * a worker reads the entities of a job it holds an ASSIGNMENT on — a session of that job
//!   that has not ended — and every one of the four entity kinds is built out of stored
//!   state: the job's own parameters and attachments, the queue's role, attachment settings
//!   and run-as user, and the Open Job Description declarations the job's template and the
//!   queue's environment templates carry;
//! * a job the worker holds no assignment on is not there for it to read: the identifier
//!   that named it is refused as the missing job it is, and never with a code that would say
//!   whether such a job exists for anybody else — a job of the farm the worker was never
//!   given, one another worker holds, one whose session has ended, and an id no job carries
//!   all answer the same way;
//! * an identifier naming a PART of an assigned job that the job does not hold — a step that
//!   is not one of its own, an environment no template of it declares, attachments a job
//!   created without any has none of — is the missing resource it names;
//! * each identifier is answered on its own: the call is a 200 carrying one entity per
//!   identifier it could read, in the order asked for, beside one `errors` entry per
//!   identifier it could not, so a refused identifier never hides the rest of the batch;
//! * the worker is reached only through its own farm and fleet, so each level this emulator
//!   does not hold is a 404 naming that level.
//!
//! The harness (`Emulator`) and the fleet and worker helpers are the ones the other suites
//! drive the service with, so these tests go through routing, deserialization, constraint
//! validation, the handler and serialization, exactly as the router does in production.

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::farm_queue_job::{absent_id, error_code, id_of, string_at, Emulator};
use crate::fleet::create_fleet;
use crate::worker::create_worker;

/// How long a test waits for a condition the clock owes before it gives up.
///
/// Generous on purpose: every wait here polls for the CONDITION and returns as soon as it
/// holds, so this bounds only a run in which the emulator never gets there.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// How many schedule polls one worker's run of a job is given before the test gives up.
const MAX_POLLS: usize = 32;

/// The Open Job Description version the job templates below declare, which is what every
/// entity read out of one reports as its `schemaVersion`.
const JOB_TEMPLATE_VERSION: &str = "jobtemplate-2023-09";

/// The version an environment template of its own declares (`environment-2023-09`), which is
/// what an entity read out of a QUEUE environment reports.
const ENVIRONMENT_TEMPLATE_VERSION: &str = "environment-2023-09";

/// The IAM role the queue below carries: what a worker runs that queue's jobs under.
const QUEUE_ROLE_ARN: &str = "arn:aws:iam::111122223333:role/DeadlineQueue";

/// `POST /2023-10-12/farms/{farmId}/fleets/{fleetId}/workers/{workerId}/batchGetJobEntity`,
/// from `smithy_query(query="operation", operation="BatchGetJobEntity")`.
fn batch_path(farm_id: &str, fleet_id: &str, worker_id: &str) -> String {
    format!(
        "/2023-10-12/farms/{farm_id}/fleets/{fleet_id}/workers/{worker_id}/batchGetJobEntity"
    )
}

fn schedule_path(farm_id: &str, fleet_id: &str, worker_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/fleets/{fleet_id}/workers/{worker_id}/schedule")
}

fn jobs_path(farm_id: &str, queue_id: &str) -> String {
    format!("/2023-10-12/farms/{farm_id}/queues/{queue_id}/jobs")
}

fn steps_path(farm_id: &str, queue_id: &str, job_id: &str) -> String {
    format!("{}/{job_id}/steps", jobs_path(farm_id, queue_id))
}

/// The attachments a job below is created with: one manifest naming where its inputs live.
fn attachments() -> Value {
    json!({
        "manifests": [{
            "rootPath": "/mnt/shared/scene",
            "rootPathFormat": "posix",
            "inputManifestPath": "manifests/scene.json",
            "inputManifestHash": "0123456789abcdef",
            "outputRelativeDirectories": ["output"],
        }],
        "fileSystem": "COPIED",
    })
}

/// A job template declaring two steps — the second depending on the first — a job-wide
/// environment and a step-local one: the declarations every entity kind is read out of.
fn template(name: &str) -> String {
    json!({
        "specificationVersion": JOB_TEMPLATE_VERSION,
        "name": name,
        "jobEnvironments": [{
            "name": "JobWide",
            "variables": {"LICENSE": "on"},
        }],
        "steps": [
            {
                "name": "Render",
                "stepEnvironments": [{"name": "StepLocal", "variables": {"TILE": "1"}}],
                "script": {"actions": {"onRun": {"command": "echo", "args": ["render"]}}},
            },
            {
                "name": "Composite",
                "dependencies": [{"dependsOn": "Render"}],
                "script": {"actions": {"onRun": {"command": "echo", "args": ["composite"]}}},
            },
        ],
    })
    .to_string()
}

/// A template declaring one step and nothing else: the smallest job a worker is handed a
/// session for.
fn one_step_template(name: &str) -> String {
    json!({
        "specificationVersion": JOB_TEMPLATE_VERSION,
        "name": name,
        "steps": [{
            "name": "Render",
            "script": {"actions": {"onRun": {"command": "echo"}}},
        }],
    })
    .to_string()
}

/// An environment template of its own, as a queue environment carries it.
fn environment_template(name: &str) -> String {
    json!({
        "specificationVersion": ENVIRONMENT_TEMPLATE_VERSION,
        "environment": {"name": name, "variables": {"SEAT": "1"}},
    })
    .to_string()
}

/// One farm, one fleet, one worker of it, and a queue whose work that fleet schedules.
struct Fixture {
    emulator: Emulator,
    farm_id: String,
    fleet_id: String,
    worker_id: String,
    queue_id: String,
}

impl Fixture {
    /// Build the farm, fleet, worker, queue and association every test below needs.
    ///
    /// The queue carries the role, the attachment settings and the run-as user a worker runs
    /// its jobs under, which is where a `jobDetails` entity reports them from.
    async fn new(name: &str) -> Self {
        let emulator = Emulator::new();
        let farm_id = emulator.farm(name).await;
        let fleet_id = create_fleet(&emulator, &farm_id, name).await;
        let worker_id = Self::worker(&emulator, &farm_id, &fleet_id, name).await;
        let (status, created) = emulator
            .post(
                &format!("/2023-10-12/farms/{farm_id}/queues"),
                json!({
                    "displayName": name,
                    "roleArn": QUEUE_ROLE_ARN,
                    "jobAttachmentSettings": {
                        "s3BucketName": "render-inputs",
                        "rootPrefix": "deadline/attachments",
                    },
                    "jobRunAsUser": {"runAs": "WORKER_AGENT_USER"},
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueue {name}: expected 200, got {status} with {created}"
        );
        let queue_id = id_of(&created, "queueId");
        let fixture = Self {
            emulator,
            farm_id,
            fleet_id,
            worker_id,
            queue_id,
        };
        fixture.associate(&fixture.queue_id.clone()).await;
        fixture
    }

    /// Register one worker with `fleet_id`.
    async fn worker(emulator: &Emulator, farm_id: &str, fleet_id: &str, name: &str) -> String {
        let (status, created) = create_worker(emulator, farm_id, fleet_id, json!({}), None).await;
        assert_eq!(
            status, 200,
            "CreateWorker {name}: expected 200, got {status} with {created}"
        );
        string_at(&created, "workerId")
    }

    /// Create a second queue of the same farm, optionally putting its work on this fleet.
    async fn queue(&self, name: &str, scheduled: bool) -> String {
        let (status, created) = self
            .emulator
            .post(
                &format!("/2023-10-12/farms/{}/queues", self.farm_id),
                json!({"displayName": name}),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueue {name}: expected 200, got {status} with {created}"
        );
        let queue_id = id_of(&created, "queueId");
        if scheduled {
            self.associate(&queue_id).await;
        }
        queue_id
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
            "CreateQueueFleetAssociation {queue_id}: expected 200, got {status} with {body}"
        );
    }

    /// Create one queue environment under this fixture's queue.
    async fn queue_environment(&self, name: &str) {
        let (status, created) = self
            .emulator
            .post(
                &format!(
                    "/2023-10-12/farms/{}/queues/{}/environments",
                    self.farm_id, self.queue_id
                ),
                json!({
                    "priority": 1,
                    "templateType": "JSON",
                    "template": environment_template(name),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateQueueEnvironment {name}: expected 200, got {status} with {created}"
        );
    }

    /// Create a job under `queue_id` from `body` and return its id.
    async fn job(&self, queue_id: &str, body: Value) -> String {
        let (status, created) = self
            .emulator
            .post(&jobs_path(&self.farm_id, queue_id), body)
            .await;
        assert_eq!(
            status, 201,
            "CreateJob: expected 201, got {status} with {created}"
        );
        id_of(&created, "jobId")
    }

    /// The step ids of `job_id`, in the order its template declared them.
    async fn step_ids(&self, case: &str, queue_id: &str, job_id: &str) -> Vec<String> {
        let (status, body) = self
            .emulator
            .get(&steps_path(&self.farm_id, queue_id, job_id))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListSteps to answer 200, got {status} with {body}"
        );
        body.get("steps")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: expected a steps list in {body}"))
            .iter()
            .map(|summary| string_at(summary, "stepId"))
            .collect()
    }

    /// One `UpdateWorkerSchedule` poll for `worker_id` carrying `reported`, which must
    /// succeed.
    async fn schedule(&self, case: &str, worker_id: &str, reported: Map<String, Value>) -> Value {
        let body = if reported.is_empty() {
            json!({})
        } else {
            json!({"updatedSessionActions": Value::Object(reported)})
        };
        let (status, response) = self
            .emulator
            .call(
                "PATCH",
                &schedule_path(&self.farm_id, &self.fleet_id, worker_id),
                Some(body),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateWorkerSchedule to answer 200, got {status} with \
             {response}"
        );
        response
    }

    /// Poll `worker_id`'s schedule until it holds a session, and answer the job that session
    /// is of.
    ///
    /// A created job reaches the status that lets it be scheduled on the clock, so this waits
    /// for the CONDITION — a session in the schedule — rather than for a span.
    async fn assigned_job(&self, case: &str, worker_id: &str) -> String {
        let deadline = Instant::now() + CONDITION_DEADLINE;
        loop {
            let response = self.schedule(case, worker_id, Map::new()).await;
            if let Some((_, session)) = assigned_sessions(&response).first() {
                return string_at(session, "jobId");
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {worker_id} to be handed a session within the \
                 deadline, last schedule {response}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// Report every action `worker_id` holds as succeeded until its schedule hands it
    /// nothing, which is what ends the session it held.
    async fn finish_work(&self, case: &str, worker_id: &str) {
        for _ in 0..MAX_POLLS {
            let response = self.schedule(case, worker_id, Map::new()).await;
            let mut reported = Map::new();
            for (_, session) in assigned_sessions(&response) {
                for action in session
                    .get("sessionActions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                {
                    reported.insert(
                        string_at(&action, "sessionActionId"),
                        json!({"completedStatus": "SUCCEEDED", "processExitCode": 0}),
                    );
                }
            }
            if reported.is_empty() {
                return;
            }
            self.schedule(case, worker_id, reported).await;
        }
        panic!("case {case:?}: {worker_id} was still holding work after {MAX_POLLS} polls");
    }

    /// One `BatchGetJobEntity` call for this fixture's worker, which must answer 200.
    async fn entities(&self, case: &str, identifiers: Vec<Value>) -> Value {
        self.entities_for(case, &self.worker_id.clone(), identifiers)
            .await
    }

    /// [`Fixture::entities`] for a chosen worker.
    async fn entities_for(&self, case: &str, worker_id: &str, identifiers: Vec<Value>) -> Value {
        let (status, body) = self
            .emulator
            .post(
                &batch_path(&self.farm_id, &self.fleet_id, worker_id),
                json!({"identifiers": identifiers}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetJobEntity to answer 200, got {status} with {body}"
        );
        body
    }
}

/// The sessions one schedule response assigned, as `(sessionId, session)` pairs.
fn assigned_sessions(response: &Value) -> Vec<(String, Value)> {
    response
        .get("assignedSessions")
        .and_then(Value::as_object)
        .map(|sessions| {
            sessions
                .iter()
                .map(|(id, session)| (id.clone(), session.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The `entities` list of an answer.
fn entities_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("entities")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an entities list in {body}"))
        .clone()
}

/// The `errors` list of an answer.
fn errors_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("errors")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an errors list in {body}"))
        .clone()
}

/// The member of a union entry, failing the test when the entry is of another kind.
fn member<'a>(case: &str, entry: &'a Value, kind: &str) -> &'a Value {
    entry
        .get(kind)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {kind} entry, got {entry}"))
}

/// The one sentence this service answers a missing resource with
/// (`crate::state::not_found`), which every read of it shares.
fn not_found_message(resource_type: &str, resource_id: &str) -> String {
    format!("Resource of type {resource_type} with id {resource_id} does not exist.")
}

/// Hold one `errors` entry to carrying `code` and `message` for the entity kind `kind`.
fn assert_error(case: &str, entry: &Value, kind: &str, code: &str, message: &str) {
    let reported = member(case, entry, kind);
    assert_eq!(
        reported.get("code").and_then(Value::as_str),
        Some(code),
        "case {case:?}: expected the {kind} error to carry code {code:?}, got {reported}"
    );
    assert_eq!(
        reported.get("message").and_then(Value::as_str),
        Some(message),
        "case {case:?}: expected the {kind} error to read {message:?}, got {reported}"
    );
}

#[tokio::test]
async fn a_worker_reads_every_entity_kind_of_the_job_it_was_assigned() {
    // The rule: a worker holding an assignment on a job reads that job's entities out of
    // stored state — the job, its queue and the templates that declare its work. The
    // identifiers of the one batch below differ along the entity KIND they ask for and, for
    // the two that name a part of the job, along WHICH part: each step of the template, and
    // an environment of each scope an `EnvironmentId` spells (a queue's, the job template's,
    // and one step's).
    let case = "every entity kind";
    let fixture = Fixture::new("entities").await;
    fixture.queue_environment("QueueWide").await;
    let job_id = fixture
        .job(
            &fixture.queue_id.clone(),
            json!({
                "template": template("entities"),
                "templateType": "JSON",
                "priority": 50,
                "parameters": {"Quality": {"string": "high"}},
                "attachments": attachments(),
            }),
        )
        .await;
    let steps = fixture
        .step_ids(case, &fixture.queue_id.clone(), &job_id)
        .await;
    assert_eq!(
        steps.len(),
        2,
        "case {case:?}: expected the two declared steps, got {steps:?}"
    );
    let assigned = fixture.assigned_job(case, &fixture.worker_id.clone()).await;
    assert_eq!(
        assigned, job_id,
        "case {case:?}: expected the worker to be handed the one job, got {assigned}"
    );

    let identifiers = vec![
        json!({"jobDetails": {"jobId": job_id}}),
        json!({"jobAttachmentDetails": {"jobId": job_id}}),
        json!({"stepDetails": {"jobId": job_id, "stepId": steps[0]}}),
        json!({"stepDetails": {"jobId": job_id, "stepId": steps[1]}}),
        json!({"environmentDetails": {
            "jobId": job_id,
            "environmentId": format!("JOB:{job_id}:QueueWide"),
        }}),
        json!({"environmentDetails": {
            "jobId": job_id,
            "environmentId": format!("JOB:{job_id}:JobWide"),
        }}),
        json!({"environmentDetails": {
            "jobId": job_id,
            "environmentId": format!("STEP:{}:StepLocal", steps[1 - 1]),
        }}),
    ];
    let body = fixture.entities(case, identifiers.clone()).await;
    let entities = entities_of(case, &body);
    assert!(
        errors_of(case, &body).is_empty(),
        "case {case:?}: expected no errors for an assigned job, got {body}"
    );
    assert_eq!(
        entities.len(),
        identifiers.len(),
        "case {case:?}: expected one entity per identifier, got {body}"
    );

    // One entity per identifier, in the order asked for.
    let details = member(case, &entities[0], "jobDetails");
    assert_eq!(
        string_at(details, "jobId"),
        job_id,
        "case {case:?}: jobDetails named another job, got {details}"
    );
    assert_eq!(
        string_at(details, "schemaVersion"),
        JOB_TEMPLATE_VERSION,
        "case {case:?}: expected the template's own version, got {details}"
    );
    assert_eq!(
        string_at(details, "logGroupName"),
        format!("/aws/deadline/{}/{}", fixture.farm_id, fixture.queue_id),
        "case {case:?}: expected the queue's own log group, got {details}"
    );
    assert_eq!(
        string_at(details, "queueRoleArn"),
        QUEUE_ROLE_ARN,
        "case {case:?}: expected the queue's role, got {details}"
    );
    assert_eq!(
        details.get("jobAttachmentSettings"),
        Some(&json!({
            "s3BucketName": "render-inputs",
            "rootPrefix": "deadline/attachments",
        })),
        "case {case:?}: expected the queue's attachment settings, got {details}"
    );
    assert_eq!(
        details.get("jobRunAsUser"),
        Some(&json!({"runAs": "WORKER_AGENT_USER"})),
        "case {case:?}: expected the queue's run-as user, got {details}"
    );
    assert_eq!(
        details.get("parameters"),
        Some(&json!({"Quality": {"string": "high"}})),
        "case {case:?}: expected the job's own parameters, got {details}"
    );

    let attached = member(case, &entities[1], "jobAttachmentDetails");
    assert_eq!(
        string_at(attached, "jobId"),
        job_id,
        "case {case:?}: jobAttachmentDetails named another job, got {attached}"
    );
    assert_eq!(
        attached.get("attachments"),
        Some(&attachments()),
        "case {case:?}: expected the attachments the job was created with, got {attached}"
    );

    let render = member(case, &entities[2], "stepDetails");
    assert_eq!(
        (
            string_at(render, "stepId"),
            string_at(render, "schemaVersion")
        ),
        (steps[0].clone(), JOB_TEMPLATE_VERSION.to_string()),
        "case {case:?}: expected the first step under the template's version, got {render}"
    );
    assert_eq!(
        render.get("template").and_then(|step| step.get("name")),
        Some(&json!("Render")),
        "case {case:?}: expected the step as its template declares it, got {render}"
    );
    assert_eq!(
        render.get("dependencies"),
        Some(&json!([])),
        "case {case:?}: expected no dependencies for the first step, got {render}"
    );

    let composite = member(case, &entities[3], "stepDetails");
    assert_eq!(
        composite.get("dependencies"),
        Some(&json!([steps[0]])),
        "case {case:?}: expected the declared dependency as a step id, got {composite}"
    );
    assert_eq!(
        composite
            .get("template")
            .and_then(|step| step.get("dependencies")),
        Some(&json!([{"dependsOn": "Render"}])),
        "case {case:?}: expected the step's own declaration, got {composite}"
    );

    // An environment of each scope: the queue's comes from the queue environment's own
    // template, so it reports that template's version; the job's and the step's are declared
    // inside the job template, so they report the job template's.
    let expected_environments = [
        (
            format!("JOB:{job_id}:QueueWide"),
            ENVIRONMENT_TEMPLATE_VERSION,
            json!({"name": "QueueWide", "variables": {"SEAT": "1"}}),
        ),
        (
            format!("JOB:{job_id}:JobWide"),
            JOB_TEMPLATE_VERSION,
            json!({"name": "JobWide", "variables": {"LICENSE": "on"}}),
        ),
        (
            format!("STEP:{}:StepLocal", steps[0]),
            JOB_TEMPLATE_VERSION,
            json!({"name": "StepLocal", "variables": {"TILE": "1"}}),
        ),
    ];
    for (at, (environment_id, version, declared)) in expected_environments.iter().enumerate() {
        let entity = member(case, &entities[4 + at], "environmentDetails");
        assert_eq!(
            (
                string_at(entity, "environmentId"),
                string_at(entity, "schemaVersion")
            ),
            (environment_id.clone(), (*version).to_string()),
            "case {case:?}: expected {environment_id} under {version}, got {entity}"
        );
        assert_eq!(
            entity.get("template"),
            Some(declared),
            "case {case:?}: expected {environment_id} as its template declares it, got {entity}"
        );
    }

    // Reading the entities changes nothing: the same batch answers the same thing, and the
    // worker still holds the session it was reading for.
    let again = fixture.entities(case, identifiers).await;
    assert_eq!(
        again, body,
        "case {case:?}: expected a second read to answer the same, got {again}"
    );
}

#[tokio::test]
async fn a_worker_is_refused_the_entities_of_a_job_it_holds_no_assignment_on() {
    // The rule: a worker reads entities only of a job it holds an assignment on, and a job
    // it holds none on is not there for it to read at all. The rows differ along WHY the
    // worker holds no assignment — a job of a queue nothing schedules, a job of its own queue
    // it was never given, a job another worker of the fleet holds, and an id no job carries —
    // and along the entity kind each one asks for, so every error entry the response can
    // carry is exercised.
    let fixture = Fixture::new("unassigned").await;
    let assigned_job = fixture
        .job(
            &fixture.queue_id.clone(),
            json!({
                "template": one_step_template("assigned"),
                "templateType": "JSON",
                "priority": 60,
            }),
        )
        .await;
    let held = fixture.assigned_job("setup", &fixture.worker_id.clone()).await;
    assert_eq!(
        held, assigned_job,
        "setup: expected the worker to hold the first job, got {held}"
    );

    // A job of a queue no association schedules can never be handed to this worker.
    let other_queue = fixture.queue("unscheduled", false).await;
    let unscheduled_job = fixture
        .job(
            &other_queue,
            json!({
                "template": one_step_template("unscheduled"),
                "templateType": "JSON",
                "priority": 50,
            }),
        )
        .await;
    // A second job of the worker's OWN queue: the worker holds a session of the first, so
    // nothing hands it this one.
    let waiting_job = fixture
        .job(
            &fixture.queue_id.clone(),
            json!({
                "template": one_step_template("waiting"),
                "templateType": "JSON",
                "priority": 10,
            }),
        )
        .await;
    // A job ANOTHER worker of the same fleet holds.
    let second_worker =
        Fixture::worker(&fixture.emulator, &fixture.farm_id, &fixture.fleet_id, "second").await;
    let other_workers_job = fixture.assigned_job("setup", &second_worker).await;
    assert_eq!(
        other_workers_job, waiting_job,
        "setup: expected the second worker to be handed the waiting job, got \
         {other_workers_job}"
    );
    let absent_job = absent_id("job", &"a".repeat(32));

    let cases: Vec<(&str, String, Value, &str)> = vec![
        (
            "a job of a queue nothing schedules",
            unscheduled_job.clone(),
            json!({"jobDetails": {"jobId": unscheduled_job}}),
            "jobDetails",
        ),
        (
            "a job another worker of the fleet holds",
            other_workers_job.clone(),
            json!({"stepDetails": {
                "jobId": other_workers_job,
                "stepId": absent_id("step", &"b".repeat(32)),
            }}),
            "stepDetails",
        ),
        (
            "an id no job of the farm carries",
            absent_job.clone(),
            json!({"jobAttachmentDetails": {"jobId": absent_job}}),
            "jobAttachmentDetails",
        ),
        (
            "an environment of a job it holds no assignment on",
            unscheduled_job.clone(),
            json!({"environmentDetails": {
                "jobId": unscheduled_job,
                "environmentId": format!("JOB:{unscheduled_job}:JobWide"),
            }}),
            "environmentDetails",
        ),
    ];

    for (case, job_id, identifier, kind) in cases {
        // The batch also asks for an entity the worker MAY read, so the answer shows a
        // refused identifier beside a served one.
        let body = fixture
            .entities(
                case,
                vec![
                    json!({"jobDetails": {"jobId": assigned_job}}),
                    identifier,
                ],
            )
            .await;
        let entities = entities_of(case, &body);
        assert_eq!(
            entities.len(),
            1,
            "case {case:?}: expected the assigned job's entity to be served, got {body}"
        );
        assert_eq!(
            string_at(member(case, &entities[0], "jobDetails"), "jobId"),
            assigned_job,
            "case {case:?}: expected the served entity to be the assigned job's, got {body}"
        );
        let errors = errors_of(case, &body);
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: expected one error entry, got {body}"
        );
        assert_error(
            case,
            &errors[0],
            kind,
            "ResourceNotFoundException",
            &not_found_message("job", &job_id),
        );
        assert_eq!(
            member(case, &errors[0], kind)
                .get("jobId")
                .and_then(Value::as_str),
            Some(job_id.as_str()),
            "case {case:?}: expected the entry to name the job asked for, got {body}"
        );
    }

    // The assignment is the session: once it has ended, the worker is refused the very job
    // it was reading a moment ago.
    fixture
        .finish_work("after the session ended", &fixture.worker_id.clone())
        .await;
    let body = fixture
        .entities(
            "after the session ended",
            vec![json!({"jobDetails": {"jobId": assigned_job}})],
        )
        .await;
    assert!(
        entities_of("after the session ended", &body).is_empty(),
        "case {:?}: expected no entity once the session ended, got {body}",
        "after the session ended"
    );
    let errors = errors_of("after the session ended", &body);
    assert_eq!(
        errors.len(),
        1,
        "case {:?}: expected one error entry, got {body}",
        "after the session ended"
    );
    assert_error(
        "after the session ended",
        &errors[0],
        "jobDetails",
        "ResourceNotFoundException",
        &not_found_message("job", &assigned_job),
    );
}

#[tokio::test]
async fn a_part_an_assigned_job_does_not_hold_is_reported_as_the_missing_resource() {
    // The rule: an identifier naming a part of an assigned job that the job does not hold is
    // the missing resource it names. The rows differ along WHICH part is missing — a step
    // that is no step of this job, an environment name no template of it declares, an
    // environment id naming another job, and an environment of a step that declares none —
    // and are worded with the one missing-resource sentence every read of this service
    // answers with.
    let fixture = Fixture::new("missing").await;
    let job_id = fixture
        .job(
            &fixture.queue_id.clone(),
            json!({
                "template": one_step_template("missing"),
                "templateType": "JSON",
                "priority": 50,
            }),
        )
        .await;
    let steps = fixture
        .step_ids("setup", &fixture.queue_id.clone(), &job_id)
        .await;
    let step_id = steps
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("setup: expected the declared step, got {steps:?}"));
    fixture.assigned_job("setup", &fixture.worker_id.clone()).await;

    let absent_step = absent_id("step", &"c".repeat(32));
    let absent_job = absent_id("job", &"d".repeat(32));
    let unknown_job_environment = format!("JOB:{job_id}:NoSuchEnvironment");
    let unknown_step_environment = format!("STEP:{step_id}:NoSuchEnvironment");
    let another_jobs_environment = format!("JOB:{absent_job}:JobWide");
    let cases: Vec<(&str, Value, &str, String, String)> = vec![
        (
            "a step id of no step of this job",
            json!({"stepDetails": {"jobId": job_id, "stepId": absent_step}}),
            "stepDetails",
            not_found_message("step", &absent_step),
            absent_step.clone(),
        ),
        (
            "an environment name the job template does not declare",
            json!({"environmentDetails": {
                "jobId": job_id,
                "environmentId": unknown_job_environment,
            }}),
            "environmentDetails",
            not_found_message("environment", &unknown_job_environment),
            unknown_job_environment.clone(),
        ),
        (
            "an environment of a step that declares none",
            json!({"environmentDetails": {
                "jobId": job_id,
                "environmentId": unknown_step_environment,
            }}),
            "environmentDetails",
            not_found_message("environment", &unknown_step_environment),
            unknown_step_environment.clone(),
        ),
        (
            "an environment id scoped to another job",
            json!({"environmentDetails": {
                "jobId": job_id,
                "environmentId": another_jobs_environment,
            }}),
            "environmentDetails",
            not_found_message("environment", &another_jobs_environment),
            another_jobs_environment.clone(),
        ),
        (
            "the attachments of a job created without any",
            json!({"jobAttachmentDetails": {"jobId": job_id}}),
            "jobAttachmentDetails",
            format!("Job {job_id} has no job attachments."),
            job_id.clone(),
        ),
    ];

    for (case, identifier, kind, message, named) in cases {
        let body = fixture.entities(case, vec![identifier]).await;
        assert!(
            entities_of(case, &body).is_empty(),
            "case {case:?}: expected no entity for a part the job does not hold, got {body}"
        );
        let errors = errors_of(case, &body);
        assert_eq!(
            errors.len(),
            1,
            "case {case:?}: expected one error entry, got {body}"
        );
        assert_error(case, &errors[0], kind, "ResourceNotFoundException", &message);
        let reported = member(case, &errors[0], kind);
        assert!(
            reported
                .as_object()
                .is_some_and(|entry| entry.values().any(|value| value == &json!(named))),
            "case {case:?}: expected the entry to name {named}, got {reported}"
        );
    }
}

#[tokio::test]
async fn a_level_this_emulator_does_not_hold_is_a_404_naming_that_level() {
    // The rule: the batch is the worker's, and a worker is reached only through its own
    // fleet and farm, so each level the path names and this emulator does not hold is a 404
    // naming that level. The rows differ along WHICH level is absent.
    let fixture = Fixture::new("absent").await;
    let job_id = fixture
        .job(
            &fixture.queue_id.clone(),
            json!({
                "template": one_step_template("absent"),
                "templateType": "JSON",
                "priority": 50,
            }),
        )
        .await;
    let cases = [
        (
            "an absent farm",
            absent_id("farm", &"a".repeat(32)),
            fixture.fleet_id.clone(),
            fixture.worker_id.clone(),
            "farm",
        ),
        (
            "an absent fleet",
            fixture.farm_id.clone(),
            absent_id("fleet", &"b".repeat(32)),
            fixture.worker_id.clone(),
            "fleet",
        ),
        (
            "an absent worker",
            fixture.farm_id.clone(),
            fixture.fleet_id.clone(),
            absent_id("worker", &"c".repeat(32)),
            "worker",
        ),
    ];

    for (case, farm_id, fleet_id, worker_id, level) in cases {
        let (status, body) = fixture
            .emulator
            .post(
                &batch_path(&farm_id, &fleet_id, &worker_id),
                json!({"identifiers": [{"jobDetails": {"jobId": job_id}}]}),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected a 404 naming the absent level, got {status} with {body}"
        );
        assert_eq!(
            body.get("resourceType").and_then(Value::as_str),
            Some(level),
            "case {case:?}: expected the refusal to name the {level} level, got {body}"
        );
        let named = match level {
            "farm" => &farm_id,
            "fleet" => &fleet_id,
            _ => &worker_id,
        };
        assert_eq!(
            body.get("resourceId").and_then(Value::as_str),
            Some(named.as_str()),
            "case {case:?}: expected the refusal to name {named}, got {body}"
        );
    }
}
