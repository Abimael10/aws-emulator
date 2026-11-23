// SPDX-License-Identifier: CC0-1.0

//! What a STOP of an execution parked in a run-a-job (`.sync`) wait does to the job that
//! wait is over — driven across every service the pattern's own catalogue reaches, mounted
//! on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: an execution parked at a submitted job is waiting on work of ANOTHER service, so
//! stopping it cancels that work through the stopping call its integration names — the
//! invoked service then reports the job in the terminal state a submitted stop brings it to
//! rather than the one it would have reached on its own — and the task that submitted the
//! job is ended without an outcome of its own: the history carries the submission and then
//! the abort, with neither a success nor a failure of the task, and the execution is
//! `ABORTED` carrying exactly the `error` and `cause` the stop named and no output.
//!
//! Four crates have to agree for that to hold for every row — Step Functions submits the
//! job, parks the task at it and propagates the stop; Glue keeps a job run's lifecycle and
//! runs its script; Athena keeps a query execution's; Amazon S3 holds the script a run
//! executes and the location a query's results go to — so the suite lives here rather than
//! in any of them.
//!
//! The rows differ along what the rule generalises over: which integration submitted the
//! job (a Glue job run, an Athena query execution and a nested Step Functions execution,
//! which are the three the catalogue holds, each stopped through a different operation of a
//! different service) and what the stop carries (an `error` and a `cause`, an `error`
//! alone, and neither — both members are optional). Every row's job would have ENDED
//! WELL — the Glue run's script exits cleanly, the query projects a literal, the child
//! execution's only state is a wait it would have come out of — so a stop is a real
//! cancellation here and not a relabelling of what the job was going to do anyway.
//!
//! Nothing here asserts a value this emulator invented: every job's identifier is read off
//! the task's own `TaskSubmitted` event, every job's state is read through the invoked
//! service's own reporting call, and the words the aborted execution reports are the ones
//! the stop carried.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The Step Functions caller, the member reader and the identities every suite here signs
// for: `tests/all/main.rs` is one crate root, so this suite borrows them rather than
// carrying a second copy.
use crate::sfn_task_target_authorization::{sfn, string, ACCOUNT, REGION};
// The Athena caller, from the suite that asks a different question of the same service,
// for the same reason.
use crate::athena_query_progress_without_an_observer::athena;

/// How long a poll waits for the condition it waits on before declaring that the work
/// never happened.
///
/// Generous rather than tight: a run a stop was submitted for comes to rest a span of the
/// invoked service's own later, and this deadline is only the point at which that is
/// declared never to have happened — a host running the whole suite at once must not be the
/// thing that trips it.
const PATIENCE: Duration = Duration::from_secs(120);

/// How often a poll asks again — the gap between two reads of a client that polls, not a
/// span anything is expected to finish inside.
const POLL: Duration = Duration::from_millis(25);

/// The history event types this suite reads
/// (`smithy_query(query="shape",shape="HistoryEventType",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
const TASK_SUBMITTED: &str = "TaskSubmitted";
/// See [`TASK_SUBMITTED`].
const TASK_SUCCEEDED: &str = "TaskSucceeded";
/// See [`TASK_SUBMITTED`].
const TASK_FAILED: &str = "TaskFailed";
/// See [`TASK_SUBMITTED`].
const EXECUTION_ABORTED: &str = "ExecutionAborted";

/// The `ExecutionStatus` a stopped execution reports, and the one it reports while it is
/// parked
/// (`smithy_query(query="shape",shape="ExecutionStatus",model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`).
const ABORTED: &str = "ABORTED";
/// See [`ABORTED`].
const RUNNING: &str = "RUNNING";

/// The `JobRunState` a Glue run a submitted stop brought to rest is in, and the ones a run
/// has not finished in
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`
/// lists every value; `STARTING`, `RUNNING`, `STOPPING` and `WAITING` each still owe a
/// further state).
const JOB_RUN_STOPPED: &str = "STOPPED";
/// See [`JOB_RUN_STOPPED`].
const JOB_RUN_IN_FLIGHT: [&str; 4] = ["STARTING", "RUNNING", "STOPPING", "WAITING"];

/// The `QueryExecutionState` an Athena query a stop interrupted is in, and the ones a query
/// has not finished in
/// (`smithy_query(query="shape",shape="QueryExecutionState",model_path="services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json")`).
const QUERY_CANCELLED: &str = "CANCELLED";
/// See [`QUERY_CANCELLED`].
const QUERY_IN_FLIGHT: [&str; 2] = ["QUEUED", "RUNNING"];

/// The bucket the Glue row's script is an object of.
const SCRIPT_BUCKET: &str = "sfn-parked-stop-scripts";

/// The script the Glue row's job runs: it computes something and exits cleanly, so the
/// state the run would come to rest in on its own is the one a clean exit decides and not
/// the one its stop brings it to.
const EXITS_CLEANLY: &str = r#"
rows = [row for row in range(12) if row % 4 == 0]
assert sum(rows) == 12, rows
"#;

/// Where the workgroup every row's query runs in writes its results. Nothing here reads
/// those objects: the location is what lets a query run at all, and Athena verifies the
/// bucket it names before it accepts a submission.
const RESULTS_LOCATION: &str = "s3://sfn-parked-stop-results/results/";

/// The workgroup every row's query runs in.
const WORK_GROUP: &str = "sfn-parked-stop";

/// The statement a row's query runs, which needs no table.
const QUERY: &str = "SELECT 'marker' AS marker";

/// How long the child execution of a nested-execution row holds, so the stop of its parent
/// reaches a child that is really still running.
///
/// Spent by nothing: the child is aborted while it waits, and no row waits for this span to
/// pass.
const CHILD_HOLD_SECONDS: u32 = 600;

/// Step Functions, Glue, Athena and Amazon S3 over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
///
/// Request authentication is off, as in every other suite here; the in-process callback
/// Step Functions makes signs its own requests either way, and their credential scope is
/// what routes them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    // Athena verifies the resolved output location before it accepts a submission, so the
    // bucket this suite's results location names is held before its first query runs.
    crate::athena_results_bucket::hold_results_bucket(&engine, RESULTS_LOCATION);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_athena_amazonathena::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`], which is what routes
/// the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// Make one Amazon S3 request (`restXml`, path-style) and answer `(status, body)`.
async fn s3_request(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Bytes,
) -> (u16, Bytes) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", authorization("s3"))
        .body(body)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().clone())
}

/// Create the bucket the script lives in. A regional endpoint takes the location constraint
/// of its own Region, which is what `CreateBucket` refuses a request without.
async fn create_script_bucket(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration><LocationConstraint>{REGION}</LocationConstraint>\
         </CreateBucketConfiguration>"
    );
    let (status, body) = s3_request(
        router,
        "PUT",
        &format!("/{SCRIPT_BUCKET}"),
        Bytes::from(configuration.into_bytes()),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "setup: the bucket must be created, got {status} with {body:?}"
    );
}

/// Store `source` as the object `key` and answer the `s3://` location that names it.
async fn stage_script(router: &MultiServiceRouter, key: &str, source: &str) -> String {
    let (status, body) = s3_request(
        router,
        "PUT",
        &format!("/{SCRIPT_BUCKET}/{key}"),
        Bytes::from(source.as_bytes().to_vec()),
    )
    .await;
    assert!(
        (200..300).contains(&status),
        "setup: the script {key} must be stored, got {status} with {body:?}"
    );
    format!("s3://{SCRIPT_BUCKET}/{key}")
}

/// One `awsJson1_1` call to Glue, requiring a 200, under the target prefix its model's
/// `service_info` names (`sigv4Name` `glue`).
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header("authorization", authorization("glue"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from Glue, got {status} with {parsed:?}"
    );
    parsed
}

/// Which job a row's `Task` submits, and so which service owns the work the stop has to
/// reach.
#[derive(Debug, Clone, Copy)]
enum Job {
    /// A Glue job run.
    GlueJobRun,
    /// An Athena query execution.
    AthenaQuery,
    /// A nested execution of a state machine, through Step Functions' own API.
    NestedExecution,
}

impl Job {
    /// The `Resource` a `Task` names to submit this job, as the optimized integration
    /// spells it.
    fn resource(self) -> String {
        match self {
            Job::GlueJobRun => "arn:aws:states:::glue:startJobRun.sync".to_string(),
            Job::AthenaQuery => "arn:aws:states:::athena:startQueryExecution.sync".to_string(),
            Job::NestedExecution => "arn:aws:states:::states:startExecution.sync".to_string(),
        }
    }

    /// The member of the submission's own answer that identifies the job, which is what
    /// the reporting and stopping calls of this integration name it by.
    fn id_member(self) -> &'static str {
        match self {
            Job::GlueJobRun => "JobRunId",
            Job::AthenaQuery => "QueryExecutionId",
            Job::NestedExecution => "ExecutionArn",
        }
    }
}

/// One row: what the task submits, and what the stop of its execution carries.
struct Row {
    case: &'static str,
    job: Job,
    /// The `error` and `cause` the stop names, each of which the aborted execution must
    /// report back exactly — and report none of where the stop named none.
    error: Option<&'static str>,
    cause: Option<&'static str>,
}

/// The rows of the table: one per integration the catalogue holds, each varying what the
/// stop carries.
fn rows() -> [Row; 3] {
    [
        Row {
            case: "a Glue job run, stopped with an error and a cause",
            job: Job::GlueJobRun,
            error: Some("sim2real.Stop"),
            cause: Some("stop while parked"),
        },
        Row {
            case: "an Athena query execution, stopped with an error alone",
            job: Job::AthenaQuery,
            error: Some("Halt"),
            cause: None,
        },
        Row {
            case: "a nested execution, stopped with neither",
            job: Job::NestedExecution,
            error: None,
            cause: None,
        },
    ]
}

/// Create the Glue job `name`, whose `Command` names `location` as the script its runs
/// execute.
async fn create_job(router: &MultiServiceRouter, name: &str, location: &str) {
    let created = glue(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": format!("arn:aws:iam::{ACCOUNT}:role/GlueJobRole"),
            "Command": { "Name": "pythonshell", "ScriptLocation": location },
        }),
    )
    .await;
    assert_eq!(
        created.get("Name").and_then(Value::as_str),
        Some(name),
        "setup: CreateJob must answer with the name it stored, got {created:?}"
    );
}

/// Register the workgroup every row's query runs in.
async fn create_work_group(router: &MultiServiceRouter) {
    athena(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": RESULTS_LOCATION },
            },
        }),
    )
    .await;
}

/// Create the state machine a nested-execution row submits an execution of, and answer its
/// ARN.
///
/// Its one `Wait` state holds, so the child is still running when its parent is stopped and
/// what ends it is the stop rather than its own work.
async fn create_child_machine(router: &MultiServiceRouter, name: &str) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": json!({
                "StartAt": "Hold",
                "States": {
                    "Hold": { "Type": "Wait", "Seconds": CHILD_HOLD_SECONDS, "Next": "Done" },
                    "Done": { "Type": "Pass", "End": true },
                },
            })
            .to_string(),
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/StepFunctionsRole"),
        }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// Create the state machine of a row — one `Task` state asking for the run-a-job pattern
/// over `resource`, building `parameters` as the starting call's request — and answer its
/// ARN.
async fn create_machine(
    router: &MultiServiceRouter,
    name: &str,
    resource: &str,
    parameters: &Value,
) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": json!({
                "StartAt": "Job",
                "States": {
                    "Job": {
                        "Type": "Task",
                        "Resource": resource,
                        "Parameters": parameters,
                        "End": true,
                    },
                },
            })
            .to_string(),
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/StepFunctionsRole"),
        }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// `DescribeExecution`, as the API answers it.
async fn described(router: &MultiServiceRouter, execution_arn: &str) -> Value {
    sfn(
        router,
        "DescribeExecution",
        &json!({ "executionArn": execution_arn }),
    )
    .await
}

/// The events of one execution's history, in order.
async fn history(router: &MultiServiceRouter, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn, "maxResults": 100 }),
    )
    .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The types of those events, in order.
fn types_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The identifier the task's own `TaskSubmitted` event reports under `member`, polled for:
/// the submission is "the response from a resource when a task has started", recorded by the
/// call that parks the task, so it is read off the history rather than guessed at.
async fn submitted_job_id(
    router: &MultiServiceRouter,
    case: &str,
    execution_arn: &str,
    member: &str,
) -> String {
    let began = Instant::now();
    loop {
        let events = history(router, execution_arn).await;
        let submitted = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_SUBMITTED));
        if let Some(submitted) = submitted {
            let output = submitted
                .get("taskSubmittedEventDetails")
                .and_then(|details| details.get("output"))
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the submission's output, got {submitted:?}")
                });
            let answered: Value = serde_json::from_str(output).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON submission, got {error} in {output:?}")
            });
            return answered
                .get(member)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the submission to name the {member}, got \
                         {answered:?}"
                    )
                })
                .to_string();
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the task to submit its job, got the history \
             {:?}",
            types_of(&events)
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The state the job of `row` is in, read through the reporting call of the invoked
/// service's own API, together with the document that call answered.
async fn job_state(
    router: &MultiServiceRouter,
    job: Job,
    job_name: &str,
    job_id: &str,
) -> (String, Value) {
    match job {
        Job::GlueJobRun => {
            let answered = glue(
                router,
                "GetJobRun",
                &json!({ "JobName": job_name, "RunId": job_id }),
            )
            .await;
            let run = answered.get("JobRun").cloned().unwrap_or(Value::Null);
            let state = run
                .get("JobRunState")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (state, run)
        }
        Job::AthenaQuery => {
            let answered = athena(
                router,
                "GetQueryExecution",
                &json!({ "QueryExecutionId": job_id }),
            )
            .await;
            let state = answered["QueryExecution"]["Status"]["State"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            (state, answered)
        }
        Job::NestedExecution => {
            let answered = described(router, job_id).await;
            let state = answered
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (state, answered)
        }
    }
}

/// Which states mean the job of `job` has NOT finished, as the invoked service's own model
/// names them.
fn in_flight_states(job: Job) -> &'static [&'static str] {
    match job {
        Job::GlueJobRun => &JOB_RUN_IN_FLIGHT,
        Job::AthenaQuery => &QUERY_IN_FLIGHT,
        Job::NestedExecution => &[RUNNING],
    }
}

/// The job of `row`, once the invoked service reports it in a state it does not leave
/// again.
async fn job_at_rest(
    router: &MultiServiceRouter,
    case: &str,
    job: Job,
    job_name: &str,
    job_id: &str,
) -> (String, Value) {
    let began = Instant::now();
    loop {
        let (state, reported) = job_state(router, job, job_name, job_id).await;
        if !in_flight_states(job).contains(&state.as_str()) {
            return (state, reported);
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the submitted job {job_id:?} to reach a terminal state, \
             and it reports {state} in {reported:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The terminal state a submitted stop brings the job of `job` to rest in, as that
/// service's own model names it.
fn stopped_state(job: Job) -> &'static str {
    match job {
        Job::GlueJobRun => JOB_RUN_STOPPED,
        Job::AthenaQuery => QUERY_CANCELLED,
        Job::NestedExecution => ABORTED,
    }
}

/// Stopping an execution parked in a run-a-job wait cancels the job it submitted, whichever
/// integration submitted it, and claims no outcome for the task that submitted it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_while_parked_cancels_the_submitted_job_and_claims_no_task_outcome() {
    let router = emulator();
    create_script_bucket(&router).await;
    let script = stage_script(&router, "exits-cleanly.py", EXITS_CLEANLY).await;
    create_work_group(&router).await;
    let child_machine = create_child_machine(&router, "parked-stop-child").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // What the task submits, and the name the reporting and stopping calls of a Glue
        // run carry beside the run's own id.
        let job_name = format!("parked-stop-{index}");
        let parameters = match row.job {
            Job::GlueJobRun => {
                create_job(&router, &job_name, &script).await;
                json!({ "JobName": job_name })
            }
            Job::AthenaQuery => json!({ "QueryString": QUERY, "WorkGroup": WORK_GROUP }),
            Job::NestedExecution => json!({ "StateMachineArn": child_machine }),
        };
        let machine = create_machine(
            &router,
            &format!("parked-stopper-{index}"),
            &row.job.resource(),
            &parameters,
        )
        .await;
        let started = sfn(
            &router,
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": format!("stop-{index}") }),
        )
        .await;
        let execution_arn = string(&started, "executionArn");

        // The job was really submitted, and its identifier is the one the integration's own
        // starting call answered.
        let job_id = submitted_job_id(&router, case, &execution_arn, row.job.id_member()).await;
        assert!(
            !job_id.is_empty(),
            "case {case:?}: expected the submission to identify the job, got {job_id:?}"
        );
        // While the task is parked the execution has no outcome: it is still running.
        let parked = described(&router, &execution_arn).await;
        assert_eq!(
            parked.get("status").and_then(Value::as_str),
            Some(RUNNING),
            "case {case:?}: expected the parked execution to be {RUNNING}, got {parked:?}"
        );

        let mut request = json!({ "executionArn": execution_arn });
        let members = request.as_object_mut().expect("the request is an object");
        if let Some(error) = row.error {
            members.insert("error".to_string(), json!(error));
        }
        if let Some(cause) = row.cause {
            members.insert("cause".to_string(), json!(cause));
        }
        let stopped = sfn(&router, "StopExecution", &request).await;
        assert!(
            stopped.get("stopDate").is_some(),
            "case {case:?}: expected the stop to report when it stopped the execution, got \
             {stopped:?}"
        );

        // (a) the cancellation crossed the service boundary: the job the task submitted is
        //     at rest in the state a submitted stop brings it to, read through the invoked
        //     service's own reporting call, and not in the one it would have reached on its
        //     own.
        let expected = stopped_state(row.job);
        let (state, reported) = job_at_rest(&router, case, row.job, &job_name, &job_id).await;
        assert_eq!(
            state, expected,
            "case {case:?}: expected the submitted job {job_id:?} to come to rest {expected}, \
             got {reported:?}"
        );

        // (b) the execution reports the words the stop carried, and nothing it did not: an
        //     aborted execution has no output.
        let after = described(&router, &execution_arn).await;
        assert_eq!(
            (
                after.get("status").and_then(Value::as_str),
                after.get("error").and_then(Value::as_str),
                after.get("cause").and_then(Value::as_str),
            ),
            (Some(ABORTED), row.error, row.cause),
            "case {case:?}: expected {ABORTED} carrying exactly the stop's words, got {after:?}"
        );
        assert!(
            after.get("output").is_none_or(Value::is_null),
            "case {case:?}: a stopped execution carries no output, got {after:?}"
        );

        // (c) and nothing claims an outcome for the task: the history holds its submission
        //     and ends on the abort, with neither a success nor a failure of the task — one
        //     stopped mid-wait neither succeeded nor failed.
        let types = types_of(&history(&router, &execution_arn).await);
        assert_eq!(
            types.last().map(String::as_str),
            Some(EXECUTION_ABORTED),
            "case {case:?}: expected the history to end on {EXECUTION_ABORTED}, got {types:?}"
        );
        assert!(
            types.iter().any(|event| event == TASK_SUBMITTED),
            "case {case:?}: expected the submission of the job in {types:?}"
        );
        assert!(
            !types
                .iter()
                .any(|event| event == TASK_SUCCEEDED || event == TASK_FAILED),
            "case {case:?}: a task stopped mid-wait has neither succeeded nor failed, got \
             {types:?}"
        );
    }
}
