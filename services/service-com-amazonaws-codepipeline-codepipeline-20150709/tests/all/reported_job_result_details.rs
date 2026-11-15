// SPDX-License-Identifier: CC0-1.0

//! What the DETAILS a job worker reports alongside its outcome do to the action
//! execution, over the wire the service speaks (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: CodePipeline_20150709.<Operation>`).
//!
//! The rule under test: the details a result STATES are that action execution's own
//! reported result. `ExecutionDetails$summary` is read back as
//! `ActionExecutionResult$externalExecutionSummary` (and as `ActionExecution$summary`),
//! `ExecutionDetails$externalExecutionId` and `FailureDetails$externalExecutionId` as
//! `ActionExecutionResult$externalExecutionId`, `ExecutionDetails$percentComplete` as
//! `ActionExecution$percentComplete`, and a failure's `type`/`message` as the action's
//! `errorDetails` — so a job that failed is diagnosable from the action it failed. A
//! member the worker did NOT state asks for no change, so what the run already carries
//! stands; and a result that only states PROGRESS (one carrying a `continuationToken`)
//! reports its details while leaving the action `InProgress`.
//!
//! The rows differ along what the rule generalises over: which members a result states,
//! the outcome it reports, the `FailureType` it chose, and the length of the external id
//! (its modelled bounds included).
//!
//! The job here is a `Custom`-owned action's, which is the plane a worker reports on with
//! nothing else running: the same lifecycle an `Invoke`/`Lambda` action's dispatched job
//! is settled through (`tests/all/lambda_invoke_job_results.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService;

/// The `X-Amz-Target` prefix OPERATIONS.md records for this service.
const TARGET: &str = "CodePipeline_20150709";

/// A `roleArn` satisfying `RoleArn`'s modelled pattern. IAM is not mounted here, so
/// `CreatePipeline`'s role check has no answer and the declaration is accepted.
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/service-role/cp";

/// How long a test waits for a step the clock owes (each is due 300 ms after the one
/// before it), at which it reports what it last saw rather than a schedule anything is
/// timed against.
const WAIT: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const LOOK: Duration = Duration::from_millis(25);

/// The identity of the custom action type these pipelines run. Its category is `Source`
/// because the first stage of a pipeline must contain a source action.
const CATEGORY: &str = "Source";
/// See [`CATEGORY`].
const PROVIDER: &str = "DetailWorker";

/// The stage and the action the worker's job belongs to.
const STAGE: &str = "Work";
/// See [`STAGE`].
const ACTION: &str = "Run";

/// `ExecutionId`'s modelled maximum length — the shape both
/// `ExecutionDetails$externalExecutionId` and `FailureDetails$externalExecutionId` target
/// (`smithy_query query="shape" shape="ExecutionId"` -> `length {min: 1, max: 1500}`).
const MAX_EXTERNAL_ID: usize = 1500;

/// One emulator: the shared engine, the service mounted on it, and the router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let authorization = "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/codepipeline/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";
        let request = http::Request::builder()
            .method("POST")
            .uri("https://codepipeline.us-east-1.amazonaws.com/")
            .header("host", "codepipeline.us-east-1.amazonaws.com")
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET}.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or_else(|error| {
                panic!("{operation}: response body is not JSON ({error}): {body:?}")
            })
        };
        (status, parsed)
    }

    async fn ok(&self, operation: &str, body: &Value) -> Value {
        let (status, response) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {response}"
        );
        response
    }
}

/// The custom action type the worker polls for.
fn action_type() -> Value {
    json!({
        "category": CATEGORY,
        "owner": "Custom",
        "provider": PROVIDER,
        "version": "1",
    })
}

/// Register that action type and create the pipeline `name`, answering the id of the
/// execution its creation started.
///
/// Creating a pipeline runs it once, so the pipeline's first execution is the one whose
/// job the worker reports for. The `Gate` stage is the non-`Source` action a declaration
/// has to carry; nothing answers its approval here, which leaves the worker's own action
/// the only one whose details are read.
async fn start(emulator: &Emulator, name: &str) -> String {
    emulator
        .ok(
            "CreateCustomActionType",
            &json!({
                "category": CATEGORY,
                "provider": PROVIDER,
                "version": "1",
                "inputArtifactDetails": {"minimumCount": 0, "maximumCount": 1},
                "outputArtifactDetails": {"minimumCount": 0, "maximumCount": 1},
            }),
        )
        .await;
    emulator
        .ok(
            "CreatePipeline",
            &json!({"pipeline": {
                "name": name,
                "roleArn": ROLE_ARN,
                "artifactStore": {"type": "S3", "location": format!("{name}-artifacts")},
                "stages": [
                    {"name": STAGE, "actions": [{
                        "name": ACTION,
                        "actionTypeId": action_type(),
                        "configuration": {"ProjectName": name},
                    }]},
                    {"name": "Gate", "actions": [{
                        "name": "Approve",
                        "actionTypeId": {
                            "category": "Approval",
                            "owner": "AWS",
                            "provider": "Manual",
                            "version": "1",
                        },
                    }]},
                ],
            }}),
        )
        .await;
    let listed = emulator
        .ok("ListPipelineExecutions", &json!({"pipelineName": name}))
        .await;
    listed["pipelineExecutionSummaries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|summary| summary["trigger"]["triggerType"] == json!("CreatePipeline"))
        .and_then(|summary| summary["pipelineExecutionId"].as_str())
        .unwrap_or_else(|| panic!("creating {name} ran it once, got {listed}"))
        .to_string()
}

/// Poll until a job is served, answering its `(id, nonce)`.
async fn claimable_job(emulator: &Emulator, case: &str) -> (String, String) {
    let deadline = Instant::now() + WAIT;
    loop {
        let polled = emulator
            .ok(
                "PollForJobs",
                &json!({"actionTypeId": action_type(), "maxBatchSize": 10}),
            )
            .await;
        if let Some(job) = polled["jobs"].as_array().and_then(|jobs| jobs.first()) {
            return (
                job["id"].as_str().expect("a job carries its id").to_string(),
                job["nonce"]
                    .as_str()
                    .expect("a polled job carries the nonce that claims it")
                    .to_string(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no job was served, last {polled}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll until a job is served and claim it, answering its id.
async fn claimed_job(emulator: &Emulator, case: &str) -> String {
    let (job_id, nonce) = claimable_job(emulator, case).await;
    emulator
        .ok("AcknowledgeJob", &json!({"jobId": job_id, "nonce": nonce}))
        .await;
    job_id
}

/// What `GetPipelineState` reports as the worker action's latest execution.
async fn latest_execution(emulator: &Emulator, pipeline: &str) -> Value {
    let state = emulator
        .ok("GetPipelineState", &json!({"name": pipeline}))
        .await;
    state["stageStates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stage| stage["stageName"] == json!(STAGE))
        .filter_map(|stage| stage["actionStates"].as_array())
        .flatten()
        .find(|action| action["actionName"] == json!(ACTION))
        .map(|action| action["latestExecution"].clone())
        .unwrap_or_else(|| panic!("no action state for {STAGE}/{ACTION} in {state}"))
}

/// The worker action's execution detail, waited for until its status is terminal.
async fn terminal_detail(
    emulator: &Emulator,
    case: &str,
    pipeline: &str,
    execution: &str,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let listed = emulator
            .ok(
                "ListActionExecutions",
                &json!({
                    "pipelineName": pipeline,
                    "filter": {"pipelineExecutionId": execution},
                }),
            )
            .await;
        let detail = listed["actionExecutionDetails"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|detail| detail["actionName"] == json!(ACTION))
            .cloned();
        if let Some(detail) = &detail {
            if matches!(
                detail["status"].as_str().unwrap_or_default(),
                "Succeeded" | "Failed" | "Abandoned"
            ) {
                return detail.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the action never reached a terminal status, last {detail:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// What a row expects the action execution to report.
struct Expected {
    /// `ActionExecutionResult$externalExecutionSummary`.
    summary: Value,
    /// `ActionExecutionResult$externalExecutionId`.
    external_id: Value,
    /// `ActionExecution$percentComplete`.
    percent: Value,
    /// `ErrorDetails$code` and `$message`, when the row expects error details.
    error: Value,
}

/// RULE: the details a worker's result states are the action execution's own reported
/// result — the summary, the external id of the run that did the work, how far it got and,
/// for a failure, the type and message it failed with — and a member the result does not
/// state leaves what the run already carries.
#[tokio::test]
async fn a_reported_results_details_are_the_action_executions_own_result() {
    // The `FailureType` values a worker may choose, from the model
    // (`smithy_query query="shape" shape="FailureType"`): JobFailed, ConfigurationError,
    // PermissionError, RevisionOutOfSync, RevisionUnavailable, SystemUnavailable. The
    // rows below exercise one the issue's own function reports and one the issue does not
    // cite.
    let longest_id = "e".repeat(MAX_EXTERNAL_ID);
    // (case, operation, the result the worker reports, the status the action reaches,
    //  what the action execution then reports)
    let rows: Vec<(&str, &str, Value, &str, Expected)> = vec![
        (
            "a success stating every detail",
            "PutJobSuccessResult",
            json!({"executionDetails": {
                "summary": "the worker ran the deployment",
                "externalExecutionId": "worker-run-7",
                "percentComplete": 100,
            }}),
            "Succeeded",
            Expected {
                summary: json!("the worker ran the deployment"),
                external_id: json!("worker-run-7"),
                percent: json!(100),
                error: Value::Null,
            },
        ),
        (
            "a success stating only an external id, at the modelled minimum length of 1",
            "PutJobSuccessResult",
            json!({"executionDetails": {"externalExecutionId": "e"}}),
            "Succeeded",
            Expected {
                summary: Value::Null,
                external_id: json!("e"),
                percent: Value::Null,
                error: Value::Null,
            },
        ),
        (
            "a success stating an external id at the modelled maximum length of 1500",
            "PutJobSuccessResult",
            json!({"executionDetails": {"externalExecutionId": longest_id.clone()}}),
            "Succeeded",
            Expected {
                summary: Value::Null,
                external_id: json!(longest_id),
                percent: Value::Null,
                error: Value::Null,
            },
        ),
        (
            "a success stating nothing about the work",
            "PutJobSuccessResult",
            json!({}),
            "Succeeded",
            Expected {
                summary: Value::Null,
                external_id: Value::Null,
                percent: Value::Null,
                error: Value::Null,
            },
        ),
        (
            "a failure naming the run that failed",
            "PutJobFailureResult",
            json!({"failureDetails": {
                "type": "JobFailed",
                "message": "the Glue job run could not be started",
                "externalExecutionId": "e8c1a0d4-request",
            }}),
            "Failed",
            Expected {
                summary: json!("the Glue job run could not be started"),
                external_id: json!("e8c1a0d4-request"),
                percent: Value::Null,
                error: json!({
                    "code": "JobFailed",
                    "message": "the Glue job run could not be started",
                }),
            },
        ),
        (
            "a failure of another modelled type, naming no run",
            "PutJobFailureResult",
            json!({"failureDetails": {
                "type": "PermissionError",
                "message": "the action's role may not start that run",
            }}),
            "Failed",
            Expected {
                summary: json!("the action's role may not start that run"),
                external_id: Value::Null,
                percent: Value::Null,
                error: json!({
                    "code": "PermissionError",
                    "message": "the action's role may not start that run",
                }),
            },
        ),
    ];

    for (index, (case, operation, mut result, status, expected)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("reported-details-{index}");
        let execution = start(&emulator, &pipeline).await;
        let job_id = claimed_job(&emulator, case).await;
        result["jobId"] = json!(job_id);
        emulator.ok(operation, &result).await;

        let detail = terminal_detail(&emulator, case, &pipeline, &execution).await;
        assert_eq!(
            detail["status"],
            json!(status),
            "case {case:?}: the action takes the outcome the worker reported, got {detail}"
        );
        let reported = &detail["output"]["executionResult"];
        assert_eq!(
            reported["externalExecutionSummary"],
            expected.summary,
            "case {case:?}: expected the summary the result stated, got {detail}"
        );
        assert_eq!(
            reported["externalExecutionId"],
            expected.external_id,
            "case {case:?}: expected the external id the result stated, got {detail}"
        );
        let error = if expected.error.is_null() {
            reported["errorDetails"].clone()
        } else {
            json!({
                "code": reported["errorDetails"]["code"],
                "message": reported["errorDetails"]["message"],
            })
        };
        assert_eq!(
            error, expected.error,
            "case {case:?}: expected the error details the failure stated, got {detail}"
        );

        // The same facts the action's own state reports, where `percentComplete` is read
        // from: one statement about one run, whichever reader answers it.
        let latest = latest_execution(&emulator, &pipeline).await;
        assert_eq!(
            latest["percentComplete"],
            expected.percent,
            "case {case:?}: expected the figure the result stated, got {latest}"
        );
        assert_eq!(
            latest["summary"],
            expected.summary,
            "case {case:?}: the action's summary is the one the result stated, got {latest}"
        );
        assert_eq!(
            latest["externalExecutionId"],
            expected.external_id,
            "case {case:?}: the action's external id is the one the result stated, got \
             {latest}"
        );
    }
}

/// RULE: a success carrying a `continuationToken` states PROGRESS — the action stays
/// `InProgress` and reports the details that result stated — and the result that finishes
/// the action later states only what it states, so a detail it leaves out keeps the value
/// the progress report gave it.
#[tokio::test]
async fn a_progress_report_states_how_far_the_work_got_without_finishing_it() {
    // (case, the figure the progress report states) — the published bounds of
    // `Percentage` (`smithy_query query="shape" shape="Percentage"` -> range 0..100) and
    // a figure between them.
    let rows: Vec<(&str, i64)> = vec![
        ("no work done yet, the published minimum", 0),
        ("half the work done", 50),
        ("all the work done, the published maximum", 100),
    ];

    for (index, (case, percent)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let pipeline = format!("progress-{index}");
        let execution = start(&emulator, &pipeline).await;
        let job_id = claimed_job(&emulator, case).await;
        emulator
            .ok(
                "PutJobSuccessResult",
                &json!({
                    "jobId": job_id,
                    "continuationToken": "still-running",
                    "executionDetails": {
                        "summary": "the worker is still at it",
                        "externalExecutionId": "worker-run-99",
                        "percentComplete": percent,
                    },
                }),
            )
            .await;

        let running = latest_execution(&emulator, &pipeline).await;
        assert_eq!(
            running["status"],
            json!("InProgress"),
            "case {case:?}: a result carrying a continuation token has not finished the \
             action, got {running}"
        );
        assert_eq!(
            running["percentComplete"],
            json!(percent),
            "case {case:?}: expected the figure the progress report stated, got {running}"
        );
        assert_eq!(
            running["summary"],
            json!("the worker is still at it"),
            "case {case:?}: expected the summary the progress report stated, got {running}"
        );
        assert_eq!(
            running["externalExecutionId"],
            json!("worker-run-99"),
            "case {case:?}: expected the external id the progress report stated, got \
             {running}"
        );

        // The job is waiting again: the worker claims it and finishes it with a result
        // that states nothing about the work, which asks for no change to what it already
        // reported.
        let continued = claimed_job(&emulator, case).await;
        emulator
            .ok("PutJobSuccessResult", &json!({"jobId": continued}))
            .await;
        let detail = terminal_detail(&emulator, case, &pipeline, &execution).await;
        assert_eq!(
            detail["status"],
            json!("Succeeded"),
            "case {case:?}: a result with no continuation token finishes the action, got \
             {detail}"
        );
        let reported = &detail["output"]["executionResult"];
        assert_eq!(
            (
                reported["externalExecutionSummary"].clone(),
                reported["externalExecutionId"].clone(),
            ),
            (
                json!("the worker is still at it"),
                json!("worker-run-99"),
            ),
            "case {case:?}: a result stating no details does not erase the ones already \
             reported, got {detail}"
        );
        assert_eq!(
            latest_execution(&emulator, &pipeline).await["percentComplete"],
            json!(percent),
            "case {case:?}: nor the figure already reported"
        );
    }
}
