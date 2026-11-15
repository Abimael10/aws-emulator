// SPDX-License-Identifier: CC0-1.0

//! Where a SERVICE JOB's work is DISPATCHED, end to end over the wire.
//!
//! Driven against Batch, SageMaker AI and Amazon S3 on ONE shared engine — what
//! `core/router` assembles — so every call travels the real wire and what is asserted is
//! what an SDK client would see.
//!
//! RULE: a service job is "a unit of work that you submit to Batch for execution on
//! SageMaker AI", its `serviceRequestPayload` is "the request, in JSON, for the service that
//! the `SubmitServiceJob` operation is queueing", and the only identifier an attempt of one
//! can report is a `ServiceResourceId` whose `name` has exactly one member, `TrainingJobArn`
//! (`smithy_query(query="shape", shape="ServiceResourceIdName")`). So a placed service job's
//! payload is HANDED ON to SageMaker AI as a `CreateTrainingJob` request, and from then on
//! the service job's `ServiceJobStatus` is the status of that training job in Batch's own
//! vocabulary: work still being done is `RUNNING`, work that finished is `SUCCEEDED`, and
//! work that failed, was stopped, or was never accepted at all is `FAILED` carrying what
//! that service said.
//!
//! The tables span what the rule generalises over: what the downstream service does with the
//! payload (accepts it and completes the work, accepts it and the work fails, refuses it
//! outright), and whether the payload names the training job itself or leaves the service job
//! to name it.
//!
//! Also asserted here, because they are the same fact read through other operations: the
//! `RUNNABLE` service job `GetJobQueueSnapshot` reports at the front of its queue, the
//! capacity a dispatched one is using, the `RUNNING` job `ListServiceJobs` selects when a
//! request names no status, and the stop a `TerminateServiceJob` asks of the dispatched work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every call is signed for and every ARN below is scoped to.
const REGION: &str = "us-east-1";

/// The account id the emulator mints its ARNs under.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The only `ServiceJobType`/`ServiceEnvironmentType` the model declares
/// (`smithy_query(query="shape", shape="ServiceJobType")`).
const SAGEMAKER_TRAINING: &str = "SAGEMAKER_TRAINING";

/// The bucket a dispatched training job writes its model artifact to.
const BUCKET: &str = "batch-service-job-output";

/// A bucket nothing creates, so the run that writes there fails its upload leg.
const ABSENT_BUCKET: &str = "batch-service-job-no-such-bucket";

/// An instance type a `ResourceConfig` can ask for: the example the `capacityUnit` doc
/// itself names.
const INSTANCE_TYPE: &str = "ml.m5.large";

/// How many instances every payload below asks for, so the capacity a dispatched job is
/// reported using can be weighed against what the request asked for.
const INSTANCE_COUNT: i64 = 2;

/// The same count as the `Double` a capacity-usage report measures it in
/// (`ServiceJobCapacityUsageDetail$quantity`).
const INSTANCE_QUANTITY: f64 = 2.0;

/// How long a poll below gives the two clocks: far longer than the spans Batch gives a
/// service job's moves and SageMaker AI gives a training run, so a loaded host still settles
/// well inside it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often a poll looks again. Only the loop's condition decides when it stops.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The terminal `ServiceJobStatus` members (`smithy_query(query="shape",
/// shape="ServiceJobStatus")`: SUBMITTED, PENDING, RUNNABLE, SCHEDULED, STARTING, RUNNING,
/// SUCCEEDED, FAILED — of which these two are the ones nothing leaves).
const TERMINAL: [&str; 2] = ["SUCCEEDED", "FAILED"];

/// A SigV4 credential scope naming `service`: it is what routes a request to that service.
/// The key id and signature are not credentials — this harness turns request authentication
/// off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20240101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Batch, SageMaker AI and Amazon S3 on ONE shared engine, the way the router mounts every
/// service.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        Self { router }
    }

    /// One Batch call, as a client makes it: `POST /v1/<operation>` with a JSON body.
    async fn batch(&self, uri: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
            .header("host", format!("batch.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("batch"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let decoded = decoded(response.into_body().as_ref());
        (status, decoded)
    }

    /// One Batch call that must be served.
    async fn batch_ok(&self, uri: &str, body: Value) -> Value {
        let (status, answered) = self.batch(uri, body).await;
        assert_eq!(
            status, 200,
            "case {uri}: expected HTTP 200, got {status} with {answered}"
        );
        answered
    }

    /// One SageMaker AI call, as a client makes it: `awsJson1_1` with `X-Amz-Target`.
    async fn sagemaker(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("api.sagemaker.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{operation}"))
            .header("authorization", authorization("sagemaker"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let decoded = decoded(response.into_body().as_ref());
        (status, decoded)
    }

    /// `PUT /<bucket>` against the emulator's own Amazon S3 front door — `CreateBucket`.
    async fn given_bucket(&self, bucket: &str) {
        let request = http::Request::builder()
            .method("PUT")
            .uri(format!("https://s3.{REGION}.amazonaws.com/{bucket}"))
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("authorization", authorization("s3"))
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        assert_eq!(
            status,
            200,
            "the bucket {bucket} must be created, got {status} {}",
            String::from_utf8_lossy(response.into_body().as_ref())
        );
    }
}

/// A response body as JSON, or `Null` when it carries none.
fn decoded(body: &[u8]) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()))
    }
}

/// A `SAGEMAKER_TRAINING` service environment and a queue over it; answers the queue's name.
async fn environment_and_queue(emulator: &Emulator, suffix: &str) -> String {
    let environment = format!("se-{suffix}");
    let queue = format!("jq-{suffix}");
    emulator
        .batch_ok(
            "/v1/createserviceenvironment",
            json!({
                "serviceEnvironmentName": &environment,
                "serviceEnvironmentType": SAGEMAKER_TRAINING,
                "capacityLimits": [{"maxCapacity": 64, "capacityUnit": "NUM_INSTANCES"}],
            }),
        )
        .await;
    emulator
        .batch_ok(
            "/v1/createjobqueue",
            json!({
                "jobQueueName": &queue,
                "priority": 1,
                "jobQueueType": SAGEMAKER_TRAINING,
                "serviceEnvironmentOrder": [{"order": 1, "serviceEnvironment": &environment}],
            }),
        )
        .await;
    queue
}

/// A `CreateTrainingJob` request document, as a `serviceRequestPayload` carries it.
///
/// `named` decides whether the payload names the training job itself; `output` is the bucket
/// its model artifact goes to; `complete` decides whether it carries the
/// `AlgorithmSpecification` that service requires to know what to run.
fn payload(named: Option<&str>, output: &str, complete: bool) -> String {
    let mut request = json!({
        "RoleArn": format!("arn:aws:iam::{ACCOUNT}:role/service-role/batch-training"),
        "OutputDataConfig": { "S3OutputPath": format!("s3://{output}/artifacts") },
        "ResourceConfig": {
            "InstanceType": INSTANCE_TYPE,
            "InstanceCount": INSTANCE_COUNT,
            "VolumeSizeInGB": 10,
        },
        "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
    });
    if complete {
        request["AlgorithmSpecification"] = json!({
            "TrainingImage": format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/trainer:1"),
            "TrainingInputMode": "File",
        });
    }
    if let Some(name) = named {
        request["TrainingJobName"] = json!(name);
    }
    request.to_string()
}

/// Submit one service job; answers its `jobId`.
async fn submit(emulator: &Emulator, queue: &str, job_name: &str, request: &str) -> String {
    let accepted = emulator
        .batch_ok(
            "/v1/submitservicejob",
            json!({
                "jobName": job_name,
                "jobQueue": queue,
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": request,
            }),
        )
        .await;
    accepted["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {job_name}: expected a jobId, got {accepted}"))
        .to_string()
}

/// `DescribeServiceJob` once.
async fn described(emulator: &Emulator, case: &str, job_id: &str) -> Value {
    let (status, body) = emulator
        .batch("/v1/describeservicejob", json!({ "jobId": job_id }))
        .await;
    assert_eq!(
        status, 200,
        "case {case}: expected DescribeServiceJob to answer 200, got {status} {body}"
    );
    body
}

/// The `status` a read reports.
fn status_of(detail: &Value) -> String {
    detail["status"].as_str().unwrap_or_default().to_string()
}

/// Look again until `job_id` reports `wanted`, answering the read that saw it.
async fn polled_until_status(emulator: &Emulator, case: &str, job_id: &str, wanted: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let detail = described(emulator, case, job_id).await;
        if status_of(&detail) == wanted {
            return detail;
        }
        assert!(
            !TERMINAL.contains(&status_of(&detail).as_str()),
            "case {case}: expected the status {wanted:?}, but the job ended at {detail}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case}: expected the status {wanted:?}, last read {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the dispatch table.
struct Dispatch {
    /// What the row varies.
    label: &'static str,
    /// The suffix its environment, queue and job names are built from.
    suffix: &'static str,
    /// Whether the payload names the training job itself.
    names_the_training_job: bool,
    /// The bucket the payload's output location points at.
    output: &'static str,
    /// Whether the payload carries everything the downstream service needs.
    complete: bool,
    /// The `ServiceJobStatus` the row must end in.
    ends: &'static str,
    /// Whether the downstream service accepted the work at all.
    accepted: bool,
}

/// The rows: what the downstream service does with the payload, and who names the training
/// job.
fn dispatches() -> Vec<Dispatch> {
    vec![
        Dispatch {
            label: "a payload the downstream service accepts and whose work completes",
            suffix: "done",
            names_the_training_job: true,
            output: BUCKET,
            complete: true,
            ends: "SUCCEEDED",
            accepted: true,
        },
        Dispatch {
            label: "the same payload, with the service job naming the training job",
            suffix: "unnamed",
            names_the_training_job: false,
            output: BUCKET,
            complete: true,
            ends: "SUCCEEDED",
            accepted: true,
        },
        Dispatch {
            label: "a payload whose work fails, because its output location holds no bucket",
            suffix: "runfails",
            names_the_training_job: true,
            output: ABSENT_BUCKET,
            complete: true,
            ends: "FAILED",
            accepted: true,
        },
        Dispatch {
            label: "a payload the downstream service refuses outright",
            suffix: "refused",
            names_the_training_job: true,
            output: BUCKET,
            complete: false,
            ends: "FAILED",
            accepted: false,
        },
    ]
}

/// RULE: a placed service job's payload is handed on to SageMaker AI as a
/// `CreateTrainingJob` request, and the service job's status from then on is the status of
/// that training job — `SUCCEEDED` for work that finished, `FAILED` carrying what that
/// service said for work that failed or was never accepted. The attempt reports the
/// `TrainingJobArn` that service answered, and the training job is one that service's own
/// `DescribeTrainingJob` holds.
#[tokio::test]
async fn a_service_jobs_status_is_the_status_of_the_work_it_was_dispatched_as() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;

    // Every row is submitted first, so the watch below sees them all progress together.
    let rows = dispatches();
    let mut submitted: Vec<(&Dispatch, String, String)> = Vec::new();
    for row in &rows {
        let queue = environment_and_queue(&emulator, row.suffix).await;
        let training_job_name = format!("t-{}", row.suffix);
        let named = row
            .names_the_training_job
            .then_some(training_job_name.as_str());
        let request = payload(named, row.output, row.complete);
        let job_name = format!("sj-{}", row.suffix);
        let job_id = submit(&emulator, &queue, &job_name, &request).await;
        let accepted = described(&emulator, row.label, &job_id).await;
        assert_eq!(
            status_of(&accepted),
            "SUBMITTED",
            "case {}: a submitted service job is handed back SUBMITTED, got {accepted}",
            row.label
        );
        // A payload that names no training job leaves the service job's own name as the
        // only name there is, so that is what the dispatch addresses.
        let addressed = if row.names_the_training_job {
            training_job_name
        } else {
            job_name
        };
        submitted.push((row, job_id, addressed));
    }

    // Watch every row at once, so each one's statuses are observed as they happen rather
    // than after the row before it has finished.
    let mut observed: Vec<Vec<String>> = vec![Vec::new(); submitted.len()];
    let mut finals: Vec<Option<Value>> = vec![None; submitted.len()];
    let deadline = Instant::now() + SETTLE_DEADLINE;
    while finals.iter().any(Option::is_none) {
        for (index, (row, job_id, _)) in submitted.iter().enumerate() {
            if finals[index].is_some() {
                continue;
            }
            let detail = described(&emulator, row.label, job_id).await;
            let status = status_of(&detail);
            if observed[index].last() != Some(&status) {
                observed[index].push(status.clone());
            }
            if TERMINAL.contains(&status.as_str()) {
                finals[index] = Some(detail);
            }
        }
        assert!(
            !finals.iter().any(Option::is_none) || Instant::now() < deadline,
            "case the watch: every service job must reach a terminal status, saw {observed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, (row, job_id, addressed)) in submitted.iter().enumerate() {
        let case = row.label;
        let detail = finals[index]
            .clone()
            .unwrap_or_else(|| panic!("case {case}: expected a terminal read"));
        let observed = &observed[index];
        assert_eq!(
            status_of(&detail),
            row.ends,
            "case {case}: expected the service job to end {}, got {detail} having been in \
             {observed:?}",
            row.ends
        );
        let _ = job_id;

        // The training job the dispatch created is one the OWNING service answers for.
        let (training_status, training) = emulator
            .sagemaker(
                "DescribeTrainingJob",
                json!({ "TrainingJobName": addressed }),
            )
            .await;
        if !row.accepted {
            assert_ne!(
                training_status, 200,
                "case {case}: a payload that service refused creates no training job, got \
                 {training}"
            );
            assert!(
                detail["statusReason"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "case {case}: the failure must carry what that service said, got {detail}"
            );
            assert_eq!(
                detail["latestAttempt"], Value::Null,
                "case {case}: nothing was dispatched, so there has been no attempt, got \
                 {detail}"
            );
            continue;
        }
        assert_eq!(
            training_status, 200,
            "case {case}: the training job {addressed} must be one that service holds, got \
             {training_status} {training}"
        );
        // Every status the service job reported is the one that service reports for the
        // work, in this service's own vocabulary.
        let reported = training["TrainingJobStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let expected = match reported.as_str() {
            "Completed" => "SUCCEEDED",
            "Failed" | "Stopped" => "FAILED",
            other => panic!("case {case}: unexpected downstream status {other:?} in {training}"),
        };
        assert_eq!(
            status_of(&detail),
            expected,
            "case {case}: the service job's status must be the work's status ({reported:?}), \
             got {detail}"
        );
        // The attempt names the resource that service holds the work as, and nothing else.
        let attempt_arn = training["TrainingJobArn"].as_str().unwrap_or_default();
        assert_eq!(
            detail["latestAttempt"]["serviceResourceId"],
            json!({"name": "TrainingJobArn", "value": attempt_arn}),
            "case {case}: expected the attempt to name the TrainingJobArn that service \
             answered, got {detail}"
        );
        assert_eq!(
            detail["attempts"][0]["serviceResourceId"]["value"].as_str(),
            Some(attempt_arn),
            "case {case}: the one attempt a dispatch started names the same resource, got \
             {detail}"
        );
        assert!(
            observed.contains(&"RUNNING".to_string()),
            "case {case}: work that service accepted is RUNNING while it is being done, saw \
             {observed:?}"
        );
        assert!(
            detail["startedAt"].as_i64().unwrap_or_default() > 0,
            "case {case}: a job that ran reports when it started, got {detail}"
        );
        assert!(
            detail["scheduledAt"].as_i64().unwrap_or_default() > 0,
            "case {case}: a job that was dispatched reports when it was scheduled, got \
             {detail}"
        );
        // Work that FAILED carries the reason that service reported for it, word for word.
        let failure = training["FailureReason"].as_str();
        if expected == "FAILED" {
            assert_eq!(
                detail["statusReason"].as_str(),
                failure,
                "case {case}: expected the reason that service reported, got {detail}"
            );
        } else {
            assert_eq!(
                detail["statusReason"], Value::Null,
                "case {case}: work that finished reports no failure reason, got {detail}"
            );
        }
        // A terminal service job holds no capacity any more.
        assert_eq!(
            detail["capacityUsage"], Value::Null,
            "case {case}: a job that has finished holds no capacity, got {detail}"
        );
    }
}

/// RULE: while a service job is `RUNNABLE` it is at the front of its queue
/// (`GetJobQueueSnapshot`), and once it has been dispatched it is `RUNNING` — which is what
/// `ListServiceJobs` selects when a request names no status ("If you don't specify a status,
/// only RUNNING jobs are returned") — and the capacity it is using is what its request asked
/// for, reported both on the job and against its queue.
#[tokio::test]
async fn a_queue_reports_its_waiting_and_its_dispatched_service_jobs() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let queue = environment_and_queue(&emulator, "snapshot").await;
    let request = payload(Some("t-snapshot"), BUCKET, true);
    let job_id = submit(&emulator, &queue, "sj-snapshot", &request).await;
    let job_arn = described(&emulator, "snapshot", &job_id).await["jobArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // RUNNABLE is where a service job waits, so that is where the snapshot reports it.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let snapshot = emulator
            .batch_ok("/v1/getjobqueuesnapshot", json!({ "jobQueue": &queue }))
            .await;
        let listed: Vec<&str> = snapshot["frontOfQueue"]["jobs"]
            .as_array()
            .map(|jobs| {
                jobs.iter()
                    .filter_map(|entry| entry["jobArn"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        if listed.contains(&job_arn.as_str()) {
            let entry = snapshot["frontOfQueue"]["jobs"][0].clone();
            assert!(
                entry["earliestTimeAtPosition"].as_i64().unwrap_or_default() > 0,
                "case the snapshot: an entry reports when it reached its position, got \
                 {snapshot}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case the snapshot: expected {job_arn} at the front of {queue}, got {snapshot}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Once the work has been handed on, the job is RUNNING and holding the capacity its
    // request asked for.
    let running = polled_until_status(&emulator, "the dispatched job", &job_id, "RUNNING").await;
    assert_eq!(
        running["capacityUsage"],
        json!([{"capacityUnit": INSTANCE_TYPE, "quantity": INSTANCE_QUANTITY}]),
        "case the dispatched job: expected the capacity the request asked for, got {running}"
    );
    let listed = emulator
        .batch_ok("/v1/listservicejobs", json!({ "jobQueue": &queue }))
        .await;
    assert_eq!(
        listed["jobSummaryList"][0]["jobId"].as_str(),
        Some(job_id.as_str()),
        "case the default listing: ListServiceJobs selects the RUNNING job, got {listed}"
    );
    assert_eq!(
        listed["jobSummaryList"][0]["capacityUsage"],
        json!([{"capacityUnit": INSTANCE_TYPE, "quantity": INSTANCE_QUANTITY}]),
        "case the default listing: the summary reports the same capacity, got {listed}"
    );
    let snapshot = emulator
        .batch_ok("/v1/getjobqueuesnapshot", json!({ "jobQueue": &queue }))
        .await;
    assert_eq!(
        snapshot["queueUtilization"]["totalCapacityUsage"],
        json!([{"capacityUnit": INSTANCE_TYPE, "quantity": INSTANCE_QUANTITY}]),
        "case the queue's utilization: expected the capacity its dispatched job is using, \
         got {snapshot}"
    );
    // A dispatched job is no longer waiting, so it has left the front of the queue.
    let waiting: Vec<&str> = snapshot["frontOfQueue"]["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .filter_map(|entry| entry["jobArn"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !waiting.contains(&job_arn.as_str()),
        "case the queue's front: a dispatched job is no longer waiting, got {snapshot}"
    );
}

/// RULE: a published capacity limit is a CONCURRENCY bound, enforced server side —
/// `CapacityLimit$maxCapacity`: "you can run up to 50 instances concurrently. If you run 5
/// SageMaker Training jobs that each use 10 instances, a subsequent job requiring 10
/// instances waits in the queue until capacity is available." So a quota share whose limit is
/// already fully allocated holds the next job of that share in `RUNNABLE`, and the job is
/// dispatched once the capacity is given back.
///
/// The rows span where the bound comes from: the quota share's own limit, and the service
/// environment's.
#[tokio::test]
async fn a_capacity_limit_already_allocated_holds_the_next_job_in_the_queue() {
    // (case, the share's limit, the environment's limit, which bound holds the second job)
    type Row<'a> = (&'a str, i32, i32, &'a str);
    let rows: &[Row<'_>] = &[
        (
            "the quota share's own limit, already fully allocated",
            INSTANCE_COUNT as i32,
            64,
            "share",
        ),
        (
            "the service environment's limit, already fully allocated",
            64,
            INSTANCE_COUNT as i32,
            "environment",
        ),
    ];
    for (index, (case, share_limit, environment_limit, bound)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        emulator.given_bucket(BUCKET).await;
        let suffix = format!("bound{index}");
        let environment = format!("se-{suffix}");
        let queue = format!("jq-{suffix}");
        let share = format!("qs-{suffix}");
        emulator
            .batch_ok(
                "/v1/createserviceenvironment",
                json!({
                    "serviceEnvironmentName": &environment,
                    "serviceEnvironmentType": SAGEMAKER_TRAINING,
                    "capacityLimits": [
                        {"maxCapacity": environment_limit, "capacityUnit": INSTANCE_TYPE},
                    ],
                }),
            )
            .await;
        // A quota-share scheduling policy, so the queue allocates by share.
        let policy = emulator
            .batch_ok(
                "/v1/createschedulingpolicy",
                json!({
                    "name": format!("sp-{suffix}"),
                    "quotaSharePolicy": {"idleResourceAssignmentStrategy": "FIFO"},
                }),
            )
            .await;
        emulator
            .batch_ok(
                "/v1/createjobqueue",
                json!({
                    "jobQueueName": &queue,
                    "priority": 1,
                    "jobQueueType": SAGEMAKER_TRAINING,
                    "serviceEnvironmentOrder": [
                        {"order": 1, "serviceEnvironment": &environment},
                    ],
                    "schedulingPolicyArn": policy["arn"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        emulator
            .batch_ok(
                "/v1/createquotashare",
                json!({
                    "quotaShareName": &share,
                    "jobQueue": &queue,
                    "capacityLimits": [
                        {"maxCapacity": share_limit, "capacityUnit": INSTANCE_TYPE},
                    ],
                    // RESERVE, so the share keeps its capacity to itself: a borrowing share
                    // publishes no bound of its own.
                    "resourceSharingConfiguration": {"strategy": "RESERVE"},
                    "preemptionConfiguration": {"inSharePreemption": "DISABLED"},
                }),
            )
            .await;

        // The first job takes the whole of whichever limit the row bounds it with.
        let first = submit_in_share(
            &emulator,
            &queue,
            &format!("sj-{suffix}-1"),
            &format!("t-{suffix}-1"),
            &share,
        )
        .await;
        polled_until_status(&emulator, case, &first, "RUNNING").await;

        // The second asks for the same capacity, so it waits.
        let second = submit_in_share(
            &emulator,
            &queue,
            &format!("sj-{suffix}-2"),
            &format!("t-{suffix}-2"),
            &share,
        )
        .await;
        let settled = Instant::now() + Duration::from_secs(6);
        while Instant::now() < settled {
            let holder = described(&emulator, case, &first).await;
            if status_of(&holder) != "RUNNING" {
                // The first job finished of its own accord, so the bound no longer holds the
                // second one back and there is nothing left to assert in this window.
                break;
            }
            let waiting = described(&emulator, case, &second).await;
            let status = status_of(&waiting);
            assert!(
                status == "SUBMITTED" || status == "RUNNABLE",
                "case {case}: the {bound} limit is fully allocated, so the next job of the \
                 share must not be dispatched, got {waiting}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }

        // Once the capacity is given back, the waiting job is dispatched.
        emulator
            .batch_ok(
                "/v1/terminateservicejob",
                json!({"jobId": &first, "reason": "make room"}),
            )
            .await;
        polled_until_status(&emulator, case, &second, "RUNNING").await;
    }
}

/// Submit one service job into the quota share `share`; answers its `jobId`.
async fn submit_in_share(
    emulator: &Emulator,
    queue: &str,
    job_name: &str,
    training_job_name: &str,
    share: &str,
) -> String {
    let accepted = emulator
        .batch_ok(
            "/v1/submitservicejob",
            json!({
                "jobName": job_name,
                "jobQueue": queue,
                "serviceJobType": SAGEMAKER_TRAINING,
                "serviceRequestPayload": payload(Some(training_job_name), BUCKET, true),
                "quotaShareName": share,
            }),
        )
        .await;
    accepted["jobId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {job_name}: expected a jobId, got {accepted}"))
        .to_string()
}

/// RULE: `TerminateServiceJob` ends the service job AND the work it was dispatched as —
/// "When it receives a `StopTrainingJob` request, SageMaker changes the status of the job to
/// `Stopping`" — so the training job the dispatch created is no longer being done either.
#[tokio::test]
async fn terminating_a_service_job_ends_the_work_it_was_dispatched_as() {
    let emulator = Emulator::new();
    emulator.given_bucket(BUCKET).await;
    let queue = environment_and_queue(&emulator, "terminate").await;
    let request = payload(Some("t-terminate"), BUCKET, true);
    let job_id = submit(&emulator, &queue, "sj-terminate", &request).await;
    polled_until_status(&emulator, "before the terminate", &job_id, "RUNNING").await;

    emulator
        .batch_ok(
            "/v1/terminateservicejob",
            json!({"jobId": &job_id, "reason": "the caller asked for it"}),
        )
        .await;
    let ended = described(&emulator, "after the terminate", &job_id).await;
    assert_eq!(
        status_of(&ended),
        "FAILED",
        "case after the terminate: a terminated service job ends FAILED, got {ended}"
    );
    assert_eq!(
        ended["statusReason"].as_str(),
        Some("the caller asked for it"),
        "case after the terminate: the caller's own reason is what the job carries, got \
         {ended}"
    );
    assert_eq!(
        ended["isTerminated"].as_bool(),
        Some(true),
        "case after the terminate: isTerminated is reported, got {ended}"
    );

    // The work itself is no longer being done: that service reports it stopping or stopped.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, training) = emulator
            .sagemaker(
                "DescribeTrainingJob",
                json!({"TrainingJobName": "t-terminate"}),
            )
            .await;
        assert_eq!(
            status, 200,
            "case the stopped work: expected the training job to be readable, got {status} \
             {training}"
        );
        let reported = training["TrainingJobStatus"].as_str().unwrap_or_default();
        if reported == "Stopping" || reported == "Stopped" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case the stopped work: expected the work to be stopping or stopped, got {training}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
