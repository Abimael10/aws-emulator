// SPDX-License-Identifier: CC0-1.0

//! What an `Invoke`/`Lambda` action's JOB RESULT does, end to end over the wire each
//! service speaks, with CodePipeline, S3 and Lambda mounted on one engine — so the
//! execution plane reaches the function the way the assembled emulator does and the
//! function reaches CodePipeline the way a real one does.
//!
//! The rules under test, each asserted over a SET of inputs rather than one case:
//!
//! * an `Invoke` action's work is a DISPATCHED invocation whose outcome the function
//!   reports itself: the action stays `InProgress` after the dispatch and is settled by
//!   the result reported for the job it was handed (`PutJobSuccessResult`), never by
//!   what the invocation answered;
//! * the `outputVariables` such a result carries are that action execution's own output
//!   — reported value for value and byte for byte, with no numeric coercion and no
//!   re-encoding, because a variable's value is an opaque string — whether or not the
//!   action declares a `namespace`, which only scopes the `#{namespace.key}` references
//!   the execution's LATER actions resolve;
//! * a function that reports its job FAILED fails the action with what it reported: the
//!   `failureDetails` it supplied are the action execution's `errorDetails`, its
//!   `externalExecutionSummary` and the external id of the run that failed, so the
//!   operator can see in the pipeline WHAT failed;
//! * so an execution parked on such an action holds one of its pipeline's
//!   concurrent-execution slots, which is what makes the documented ceiling of 50 active
//!   executions per pipeline reachable in `PARALLEL` mode: the start past it is refused
//!   `ConcurrentPipelineExecutionsLimitExceededException` and records no execution.

use std::io::Write as _;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE;
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    AsyncInvocationOutcome, ComAmazonawsLambdaAwsgirapiserviceState,
};

/// The `X-Amz-Target` prefix OPERATIONS.md records for CodePipeline.
const PIPELINE_TARGET: &str = "CodePipeline_20150709";

/// A `roleArn` satisfying `RoleArn`'s modelled pattern, for the pipeline and for the
/// function. IAM is not mounted here, so no role lookup has an answer and the
/// declarations are accepted (only a definitive "no such role" refuses one).
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/service-role/cp";

/// The module and attribute the deployment packages below carry their handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long a test waits for a condition the clock owes before giving up on it.
///
/// Each step of an execution becomes due `state::execution::ACTION_STEP` (300 ms) after
/// the one before it and a dispatched invocation is delivered on the Lambda service's own
/// queueing delay, so this is a generous ceiling at which a test reports what it last saw
/// rather than a schedule anything is timed against.
const WAIT: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const LOOK: Duration = Duration::from_millis(50);

/// The sentence the service states the concurrent-execution ceiling with, written out
/// here so a reword of the handler's wording fails this suite. It carries no variable
/// part.
const AT_THE_LIMIT: &str =
    "The limit for concurrent pipeline executions has been exceeded. Please try again later.";

/// The handler of the function the output-variable rows invoke: it reports the job it was
/// handed as successful, publishing the variables its action's `UserParameters` carry.
///
/// This is the shape AWS documents for a Lambda invoke action — read the job out of
/// `event["CodePipeline.job"]`, do the work, call `put_job_success_result` — so one
/// package serves every row: what the rows differ in is the map the action declares,
/// which is what the rule generalises over.
const REPORTING_HANDLER: &str = r#"
import json

import boto3

PIPELINE = boto3.client("codepipeline")


def lambda_handler(event, context):
    job = event["CodePipeline.job"]
    declared = job["data"]["actionConfiguration"]["configuration"]["UserParameters"]
    PIPELINE.put_job_success_result(
        jobId=job["id"], outputVariables=json.loads(declared)
    )
    return {"reported": job["id"]}
"#;

/// The handler of the function the failure rows invoke: its work raises, and the except
/// branch reports the job FAILED with the type its action's `UserParameters` name, the
/// message the raise carried and the run that failed — `context.aws_request_id`, the id of
/// the invocation that did the work.
///
/// This is the shape AWS documents for a launcher: do the work in a `try`, report success
/// for the job, and report `failureDetails` from the `except` branch so the operator can
/// see in the pipeline WHAT failed.
const FAILING_HANDLER: &str = r#"
import json

import boto3

PIPELINE = boto3.client("codepipeline")


def lambda_handler(event, context):
    job = event["CodePipeline.job"]
    declared = json.loads(
        job["data"]["actionConfiguration"]["configuration"]["UserParameters"]
    )
    try:
        raise RuntimeError(declared["message"])
    except Exception as error:
        PIPELINE.put_job_failure_result(
            jobId=job["id"],
            failureDetails={
                "type": declared["type"],
                "message": str(error),
                "externalExecutionId": context.aws_request_id,
            },
        )
        return {"reported": "failure"}
"#;

/// The handler of the function the ceiling rows invoke: it reports NOTHING, which is the
/// case a client runs into when its function returns without calling back — the action
/// then waits for a result, so its execution keeps its slot.
const SILENT_HANDLER: &str = r#"
def lambda_handler(event, context):
    return {"reported": False}
"#;

/// One emulator: the shared engine with CodePipeline, S3 and Lambda on it, and the router
/// that dispatches to all three. The execution plane invokes the function through the
/// framework's in-process callback and the function calls CodePipeline back through the
/// same emulator, so all three have to be there.
struct Emulator {
    router: MultiServiceRouter,
    /// The one engine every mounted service was built from, so a test can read the
    /// Lambda store a dispatched invocation settles in — which is how it waits for the
    /// function to have RUN rather than for a span of wall clock.
    engine: std::sync::Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: std::sync::Arc<engine::InMemoryEngine> =
            std::sync::Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_codepipeline_codepipeline_20150709::EmulatedService::new(
                std::sync::Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(
                std::sync::Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// Wait until an invocation of `function` has been DELIVERED — the function has run
    /// and its run has settled — so what a test asserts next is asserted about a service
    /// that has already seen whatever that run did.
    async fn function_has_run(&self, case: &str, function: &str) {
        let deadline = Instant::now() + WAIT;
        loop {
            let state = self
                .engine
                .state::<ComAmazonawsLambdaAwsgirapiserviceState>();
            let settled = {
                let store = state.lock();
                store
                    .functions
                    .get(function)
                    .map(|record| {
                        record.async_invocations.iter().any(|entry| {
                            !matches!(
                                entry.outcome,
                                AsyncInvocationOutcome::Queued
                                    | AsyncInvocationOutcome::Delivering
                            )
                        })
                    })
                    .unwrap_or(false)
            };
            if settled {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: no invocation of the function {function:?} ever ran"
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// One CodePipeline operation, as its status and parsed body.
    async fn cp(&self, operation: &str, body: &Value) -> (u16, Value) {
        let authorization = "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/codepipeline/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";
        let request = http::Request::builder()
            .method("POST")
            .uri("https://codepipeline.us-east-1.amazonaws.com/")
            .header("host", "codepipeline.us-east-1.amazonaws.com")
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{PIPELINE_TARGET}.{operation}"))
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

    /// One CodePipeline operation that must succeed.
    async fn cp_ok(&self, operation: &str, body: &Value) -> Value {
        let (status, response) = self.cp(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {response}"
        );
        response
    }

    /// One Lambda call: the service speaks restJson1, so each operation has its own
    /// method and URI.
    async fn lambda(&self, method: &str, uri: &str, body: &Value) -> (u16, Value) {
        let authorization = "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/lambda/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";
        let payload = if body.is_null() {
            bytes::Bytes::new()
        } else {
            bytes::Bytes::from(body.to_string())
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://lambda.us-east-1.amazonaws.com{uri}"))
            .header("host", "lambda.us-east-1.amazonaws.com")
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = response.into_body();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One S3 request, path style, the way the emulated S3 service routes it.
    async fn s3(&self, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let authorization = "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://s3.us-east-1.amazonaws.com{uri}"))
            .header("host", "s3.us-east-1.amazonaws.com")
            .header("authorization", authorization)
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        (parts.status.as_u16(), body.to_vec())
    }

    /// Create a bucket.
    async fn bucket(&self, name: &str) {
        let (status, body) = self.s3("PUT", &format!("/{name}"), b"").await;
        assert_eq!(
            status,
            200,
            "the bucket {name:?} must be created, got {status} with {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Put one object.
    async fn put_object(&self, bucket: &str, key: &str, body: &[u8]) {
        let (status, answered) = self.s3("PUT", &format!("/{bucket}/{key}"), body).await;
        assert_eq!(
            status,
            200,
            "the object {bucket}/{key} must be written, got {status} with {}",
            String::from_utf8_lossy(&answered)
        );
    }

    /// Create a `python3.12` function whose code is `source`, and wait for it to reach
    /// `Active` — the state an invocation needs.
    async fn function(&self, name: &str, source: &str) {
        let (status, created) = self
            .lambda(
                "POST",
                "/2015-03-31/functions",
                &json!({
                    "FunctionName": name,
                    "Role": ROLE_ARN,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": 30,
                    // The deployment package is a blob, carried base64-encoded.
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD
                            .encode(package(source)),
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "CreateFunction {name:?} must succeed, got {status} with {created}"
        );
        let uri = format!("/2015-03-31/functions/{name}/configuration");
        let deadline = Instant::now() + WAIT;
        loop {
            let (status, configuration) = self.lambda("GET", &uri, &Value::Null).await;
            assert_eq!(
                status, 200,
                "GetFunctionConfiguration {name:?} must succeed, got {configuration}"
            );
            if configuration["State"] == json!("Active") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the function {name:?} never became Active, last {configuration}"
            );
            tokio::time::sleep(LOOK).await;
        }
    }
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory
/// the way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// A pipeline declaration: an S3 source stage reading `bucket`/`source.zip`, and an
/// `Invoke` stage whose action invokes `function` with `parameters` as its
/// `UserParameters`, under the `namespace` it declares, if any.
fn declaration(
    name: &str,
    bucket: &str,
    function: &str,
    parameters: &str,
    namespace: Option<&str>,
    execution_mode: Option<&str>,
) -> Value {
    let mut invoke = json!({
        "name": "InvokeLambda",
        "actionTypeId": {
            "category": "Invoke", "owner": "AWS", "provider": "Lambda", "version": "1"
        },
        "configuration": {"FunctionName": function, "UserParameters": parameters},
        "inputArtifacts": [{"name": "SourceOutput"}],
    });
    if let Some(namespace) = namespace {
        invoke["namespace"] = json!(namespace);
    }
    let mut pipeline = json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "artifactStore": {"type": "S3", "location": bucket},
        "stages": [
            {
                "name": "Source",
                "actions": [{
                    "name": "SourceAction",
                    "actionTypeId": {
                        "category": "Source", "owner": "AWS", "provider": "S3", "version": "1"
                    },
                    "configuration": {
                        "S3Bucket": bucket,
                        "S3ObjectKey": "source.zip",
                        "PollForSourceChanges": "false",
                    },
                    "outputArtifacts": [{"name": "SourceOutput"}],
                }],
            },
            {"name": "InvokeStage", "actions": [invoke]},
        ],
    });
    if let Some(mode) = execution_mode {
        pipeline["pipelineType"] = json!("V2");
        pipeline["executionMode"] = json!(mode);
    }
    json!({"pipeline": pipeline})
}

/// The `pipelineExecutionId` the invoke stage's `latestExecution` reports, waited for.
async fn invoke_stage_execution(emulator: &Emulator, case: &str, pipeline: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let state = emulator
            .cp_ok("GetPipelineState", &json!({"name": pipeline}))
            .await;
        let reported = state["stageStates"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|stage| stage["stageName"] == json!("InvokeStage"))
            .and_then(|stage| stage["latestExecution"]["pipelineExecutionId"].as_str())
            .map(str::to_string);
        if let Some(id) = reported {
            return id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the invoke stage never reported an execution, last {state}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Every action execution `ListActionExecutions` reports for one execution of `pipeline`.
async fn action_executions(emulator: &Emulator, pipeline: &str, execution: &str) -> Vec<Value> {
    let listed = emulator
        .cp_ok(
            "ListActionExecutions",
            &json!({
                "pipelineName": pipeline,
                "filter": {"pipelineExecutionId": execution},
            }),
        )
        .await;
    listed["actionExecutionDetails"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The invoke action's execution detail, waited for until its status is `terminal`.
async fn terminal_invoke_action(
    emulator: &Emulator,
    case: &str,
    pipeline: &str,
    execution: &str,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let listed = action_executions(emulator, pipeline, execution).await;
        let detail = listed
            .iter()
            .find(|detail| detail["actionName"] == json!("InvokeLambda"))
            .cloned();
        if let Some(detail) = &detail {
            let status = detail["status"].as_str().unwrap_or_default();
            if matches!(status, "Succeeded" | "Failed" | "Abandoned") {
                return detail.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the invoke action never reached a terminal status, last {detail:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The summaries `ListPipelineExecutions` reports for `pipeline`, newest first.
async fn summaries(emulator: &Emulator, pipeline: &str) -> Vec<Value> {
    let listed = emulator
        .cp_ok("ListPipelineExecutions", &json!({"pipelineName": pipeline}))
        .await;
    listed["pipelineExecutionSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: a Lambda invoke action's outcome is the RESULT its function reports for the job
/// it was handed, and the `outputVariables` of that result are the action execution's own
/// output — value for value and byte for byte.
///
/// The rows differ along what the rule generalises over: what the values ARE (a long
/// integer literal, a long decimal literal, non-ASCII text, text carrying the syntax of
/// the document they travel in), how MANY of them there are, and whether the action
/// declares a namespace at all — which is what the issue's own case turns on, because an
/// action declaring none still publishes what it published.
#[tokio::test]
async fn a_reported_job_results_variables_are_the_action_executions_output() {
    // (case, namespace, variables)
    let rows: Vec<(&str, Option<&str>, Value)> = vec![
        (
            "a long integer literal and a long decimal literal, no namespace declared",
            None,
            json!({
                "BigInteger": "9007199254740993000000000000000001",
                "PreciseDecimal": "0.123456789012345678901234567890123456",
            }),
        ),
        (
            "non-ASCII text, under a declared namespace",
            Some("InvokeVariables"),
            json!({"UnicodeText": "ρυθμός 日本語 — Ünïcödé 🙂"}),
        ),
        (
            "one variable whose value carries the syntax of the document it travels in",
            Some("Published"),
            json!({"Quoted": "{\"not\": \"parsed\"} #{NotAReference.key} \\n"}),
        ),
        (
            "several variables at once",
            None,
            json!({
                "One": "1",
                "Two": "2",
                "Three": "0003",
                "Empty": " ",
                "Padded": "  spaces kept  ",
            }),
        ),
    ];

    let emulator = Emulator::new();
    let bucket = "reported-variables";
    emulator.bucket(bucket).await;
    emulator
        .put_object(bucket, "source.zip", b"the application bytes")
        .await;
    emulator.function("reporter", REPORTING_HANDLER).await;

    for (index, (case, namespace, variables)) in rows.into_iter().enumerate() {
        let pipeline = format!("reported-{index}");
        let parameters = variables.to_string();
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(
                    &pipeline,
                    bucket,
                    "reporter",
                    &parameters,
                    namespace,
                    None,
                ),
            )
            .await;

        let execution = invoke_stage_execution(&emulator, case, &pipeline).await;
        let detail = terminal_invoke_action(&emulator, case, &pipeline, &execution).await;
        assert_eq!(
            detail["status"],
            json!("Succeeded"),
            "case {case:?}: the action's outcome is the success its function reported, got \
             {detail}"
        );
        assert_eq!(
            detail["output"]["outputVariables"],
            variables,
            "case {case:?}: expected the variables the result reported, got {detail}"
        );
        assert_eq!(
            detail["pipelineExecutionId"],
            json!(execution),
            "case {case:?}: the action execution belongs to the execution asked for, got \
             {detail}"
        );
    }
}

/// RULE: an `Invoke` action is DISPATCHED and then waits for its function's result, so
/// the action is `InProgress` with no output while nothing has been reported for it —
/// and a function that reports nothing leaves it there rather than settling it from what
/// the invocation answered.
#[tokio::test]
async fn an_invoke_action_waits_for_the_result_its_function_reports() {
    let case = "a function that reports no result";
    let emulator = Emulator::new();
    let bucket = "silent-reporter";
    emulator.bucket(bucket).await;
    emulator
        .put_object(bucket, "source.zip", b"the application bytes")
        .await;
    emulator.function("silent", SILENT_HANDLER).await;
    emulator
        .cp_ok(
            "CreatePipeline",
            &declaration("silent-pipeline", bucket, "silent", "{}", None, None),
        )
        .await;

    let execution = invoke_stage_execution(&emulator, case, "silent-pipeline").await;
    // The function has RUN and answered — the invocation is settled — and it reported no
    // result, so there is nothing to settle the action with: it is still running, with no
    // output at all, and its execution has not finished either.
    emulator.function_has_run(case, "silent").await;
    let listed = action_executions(&emulator, "silent-pipeline", &execution).await;
    let detail = listed
        .iter()
        .find(|detail| detail["actionName"] == json!("InvokeLambda"))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the invoke action was reached, got {listed:?}"));
    assert_eq!(
        detail["status"],
        json!("InProgress"),
        "case {case:?}: an action nobody reported a result for is still running, got {detail}"
    );
    assert!(
        detail["output"].is_null(),
        "case {case:?}: an action that has reported nothing has no output, got {detail}"
    );
    assert_eq!(
        detail["input"]["actionTypeId"]["provider"],
        json!("Lambda"),
        "case {case:?}: the action waiting is the invoke action, got {detail}"
    );
    let statuses: Vec<String> = summaries(&emulator, "silent-pipeline")
        .await
        .iter()
        .map(|summary| summary["status"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        statuses,
        vec!["InProgress".to_string()],
        "case {case:?}: the execution is parked on the action, so it has not finished, got \
         {statuses:?}"
    );
}

/// RULE: an execution parked on a dispatched invoke action holds one of its pipeline's
/// concurrent-execution slots, so a `PARALLEL` pipeline whose every execution waits on
/// its function reaches the ceiling of 50 active executions — and the start past it is
/// refused `ConcurrentPipelineExecutionsLimitExceededException` with the fixed sentence,
/// having recorded nothing.
#[tokio::test]
async fn invoke_actions_fill_a_parallel_pipelines_execution_slots() {
    let case = "a PARALLEL pipeline whose invoke actions wait on their function";
    let emulator = Emulator::new();
    let bucket = "parallel-invokes";
    emulator.bucket(bucket).await;
    emulator
        .put_object(bucket, "source.zip", b"the application bytes")
        .await;
    emulator.function("waiter", SILENT_HANDLER).await;
    emulator
        .cp_ok(
            "CreatePipeline",
            &declaration(
                "parallel-pipeline",
                bucket,
                "waiter",
                "{}",
                None,
                Some("PARALLEL"),
            ),
        )
        .await;

    // One slot is already taken by the run the pipeline's creation started, so the starts
    // that fit inside the bound are one fewer than the bound. Each is accepted while
    // fewer than the bound are active, and each reports an id of its own.
    let mut accepted: Vec<String> = Vec::new();
    for index in 0..MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - 1 {
        let answered = emulator
            .cp_ok(
                "StartPipelineExecution",
                &json!({"name": "parallel-pipeline"}),
            )
            .await;
        let id = answered["pipelineExecutionId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: start {index} reports an execution id, got {answered}")
            })
            .to_string();
        assert!(
            !accepted.contains(&id),
            "case {case:?}: start {index} reports an id of its own, got {id} again"
        );
        accepted.push(id);
    }

    // Every slot is taken: the executions are parked on their invoke action, waiting for
    // a result their function never reports, so none of them gives its slot back.
    let deadline = Instant::now() + WAIT;
    let listed = loop {
        let listed = summaries(&emulator, "parallel-pipeline").await;
        let active = listed
            .iter()
            .filter(|summary| summary["status"] == json!("InProgress"))
            .count();
        if active == MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE} active \
             executions, got {active} of {}",
            listed.len()
        );
        tokio::time::sleep(LOOK).await;
    };
    for id in &accepted {
        assert!(
            listed
                .iter()
                .any(|summary| summary["pipelineExecutionId"] == json!(id)),
            "case {case:?}: every accepted execution is visible, {id} is not among {}",
            listed.len()
        );
    }

    // The start past the ceiling is refused, and the refusal states the condition with
    // its fixed sentence.
    let (status, refused) = emulator
        .cp(
            "StartPipelineExecution",
            &json!({"name": "parallel-pipeline"}),
        )
        .await;
    assert_eq!(
        (
            status,
            refused["__type"].as_str(),
            refused["message"].as_str()
        ),
        (
            400,
            Some("ConcurrentPipelineExecutionsLimitExceededException"),
            Some(AT_THE_LIMIT)
        ),
        "case {case:?}: a start past the ceiling is refused, got {status} with {refused}"
    );

    // And it recorded nothing: the pipeline still holds exactly its bound, and no
    // execution beyond the accepted ones and the creation run is reported.
    let retained = summaries(&emulator, "parallel-pipeline").await;
    assert_eq!(
        retained.len(),
        MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "case {case:?}: a refused start records no execution, got {} entries",
        retained.len()
    );
    let unaccounted = retained
        .iter()
        .filter(|summary| {
            summary["pipelineExecutionId"]
                .as_str()
                .is_none_or(|id| !accepted.iter().any(|accepted| accepted == id))
        })
        .count();
    assert_eq!(
        unaccounted, 1,
        "case {case:?}: exactly one execution beyond the accepted ones — the run the \
         pipeline's creation started — is reported, got {unaccounted}"
    );
}

/// RULE: a function that reports its job FAILED fails the action with what it reported —
/// the `failureDetails` it supplied are the action execution's `errorDetails`, its
/// `externalExecutionSummary` and the external id of the run that failed — and the
/// execution fails with it.
///
/// The rows differ along what the rule generalises over: the modelled `FailureType` the
/// function chose and the message its own work failed with.
#[tokio::test]
async fn a_reported_job_failure_is_the_actions_failure_and_says_what_failed() {
    // (case, the FailureType the function reports, the message its work failed with) —
    // the types are the model's (`smithy_query query="shape" shape="FailureType"` ->
    // JobFailed, ConfigurationError, PermissionError, RevisionOutOfSync,
    // RevisionUnavailable, SystemUnavailable).
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "the work the launcher does raised",
            "JobFailed",
            "the run of the job this action launches could not be started",
        ),
        (
            "the launcher found its own action misconfigured",
            "ConfigurationError",
            "UserParameters names no job for this action to launch",
        ),
        (
            "the launcher may not pass the role the run needs",
            "PermissionError",
            "this action's role may not pass the role the run needs",
        ),
    ];

    let emulator = Emulator::new();
    let bucket = "failing-reporter";
    emulator.bucket(bucket).await;
    emulator
        .put_object(bucket, "source.zip", b"the application bytes")
        .await;
    emulator.function("launcher", FAILING_HANDLER).await;

    for (index, (case, failure_type, message)) in rows.into_iter().enumerate() {
        let pipeline = format!("reported-failure-{index}");
        let parameters = json!({"type": failure_type, "message": message}).to_string();
        emulator
            .cp_ok(
                "CreatePipeline",
                &declaration(&pipeline, bucket, "launcher", &parameters, None, None),
            )
            .await;

        let execution = invoke_stage_execution(&emulator, case, &pipeline).await;
        let detail = terminal_invoke_action(&emulator, case, &pipeline, &execution).await;
        assert_eq!(
            detail["status"],
            json!("Failed"),
            "case {case:?}: the action takes the failure its function reported, got {detail}"
        );
        let reported = &detail["output"]["executionResult"];
        assert_eq!(
            (
                reported["errorDetails"]["code"].clone(),
                reported["errorDetails"]["message"].clone(),
                reported["externalExecutionSummary"].clone(),
            ),
            (json!(failure_type), json!(message), json!(message)),
            "case {case:?}: the action says what failed, got {detail}"
        );
        // The run that failed is the one the FUNCTION named — its own invocation, read
        // off its context — not the dispatch the engine recorded before it ran. A Lambda
        // request id is a uuid, so that is the shape the reported id takes.
        let external = reported["externalExecutionId"].as_str().unwrap_or_default();
        let uuid_shaped = external.len() == 36
            && external.char_indices().all(|(position, character)| {
                if matches!(position, 8 | 13 | 18 | 23) {
                    character == '-'
                } else {
                    character.is_ascii_hexdigit()
                }
            });
        assert!(
            uuid_shaped && external != "launcher",
            "case {case:?}: expected the run the function reported as having failed, got \
             {external:?} in {detail}"
        );

        let deadline = Instant::now() + WAIT;
        let settled = loop {
            let status = summaries(&emulator, &pipeline)
                .await
                .iter()
                .find(|summary| summary["pipelineExecutionId"] == json!(execution))
                .map(|summary| summary["status"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            if matches!(status.as_str(), "Failed" | "Succeeded" | "Stopped") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the execution never finished, last {status:?}"
            );
            tokio::time::sleep(LOOK).await;
        };
        assert_eq!(
            settled, "Failed",
            "case {case:?}: the execution fails with the action its function failed"
        );
    }
}
