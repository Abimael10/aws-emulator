// SPDX-License-Identifier: CC0-1.0

//! The window a Glue job run is observable RUNNING in through Glue's own API, and what a
//! stop submitted while that window is open brings the run to rest in — driven across the
//! three services the question spans, mounted on ONE engine exactly as
//! `core/router/src/lib.rs` mounts them.
//!
//! RULE: a started Glue job run occupies every non-terminal state of its lifecycle for a
//! span of its own, so `GetJobRun` reports it `RUNNING` while the one execution of its
//! script is in flight — whoever started it, a client's own `StartJobRun` or the starting
//! call a Step Functions run-a-job (`.sync`) task makes — and a stop submitted for a run
//! still in flight wins over whatever that execution came to: the run passes through
//! `STOPPING` and comes to rest `STOPPED` carrying no reason, whether the stop was
//! submitted by Glue's own `BatchStopJobRun` or by `StopExecution` on the execution whose
//! task started the run.
//!
//! Three crates have to agree for that to hold — Step Functions starts the run, parks its
//! task at it and propagates the stop; Glue executes the script and keeps the run's
//! lifecycle; Amazon S3 serves the script the run executes — so the suite lives here rather
//! than in any of them.
//!
//! The rows differ along what the rule generalises over: who started the run (a `.sync`
//! task, the same pattern's second revision, a client's own start), what ends it (a stop of
//! the owning execution naming an error and a cause, one naming neither, a
//! `BatchStopJobRun` of the run itself, and nothing at all) and the command kind of the job
//! it is a run of. Every row's script exits cleanly, so a row whose run is stopped would
//! have SUCCEEDED and rests `STOPPED` instead, while the row nothing stops rests
//! `SUCCEEDED`: a stop is a real outcome of the run rather than a relabelled failure.
//!
//! Nothing here asserts a value this emulator invented: the run's id comes off the task's
//! own submission event or off the start's answer, every state is read through `GetJobRun`,
//! and the words an aborted execution reports are the ones the stop carried.
//!
//! Every wait is a poll for the CONDITION it waits on, and the span the clock owes the run
//! is held open and made due THROUGH THE STORE rather than slept out — the way the Glue
//! crate's own run-lifecycle suites do it — so what these rows observe does not depend on
//! how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    ComAmazonawsGlueAwsglueState, JOB_RUN_STATE_RUNNING, JOB_RUN_STATE_STARTING,
    JOB_RUN_STATE_STOPPED, JOB_RUN_STATE_STOPPING, JOB_RUN_STATE_SUCCEEDED,
    TERMINAL_JOB_RUN_STATES,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for, and the one the state machine's ARN names —
/// which is the Region a task's integration call is made in.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll waits for the condition it waits on before declaring that the work never
/// happened.
///
/// Generous rather than tight: a run's next state is owed a span after the call that started
/// it, a parked task is resolved a poll interval after the run it watches finished, and this
/// deadline is only the point at which the work is declared never to have happened — a host
/// running the whole suite at once must not be the thing that trips it.
const PATIENCE: Duration = Duration::from_secs(120);

/// How often a poll asks again — the gap between two reads of a client that polls, not a
/// span anything is expected to finish inside.
const POLL: Duration = Duration::from_millis(25);

/// The bucket every row's script is an object of.
const SCRIPT_BUCKET: &str = "xsvc-glue-sync-stop-scripts";

/// The script every row's job runs: it computes something and exits cleanly, so the state
/// the run would come to rest in on its own is the one a clean exit decides.
const EXITS_CLEANLY: &str = r#"
rows = [row for row in range(12) if row % 4 == 0]
assert sum(rows) == 12, rows
"#;

/// Step Functions, Glue and Amazon S3 on one engine, plus the engine itself — so a row can
/// read and move the due instants the run lifecycle turns on instead of waiting for them.
///
/// Request authentication is off, as in every other suite here; the in-process callback Step
/// Functions makes signs its request either way, and its credential scope is what routes it.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation of an `awsJson` service and answer `(status, body)`.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", format!("application/x-amz-json-{json_version}"))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Step Functions call (awsJson1_0, `AWSStepFunctions.<Operation>`), requiring a 200.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, "states", "AWSStepFunctions", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from Step Functions, got {status} with {parsed:?}"
    );
    parsed
}

/// One Glue call (awsJson1_1, `AWSGlue.<Operation>`), requiring a 200.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(router, "glue", "AWSGlue", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation}: expected 200 from Glue, got {status} with {parsed:?}"
    );
    parsed
}

/// The text a member of a response carries.
fn string(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {name:?} in {body:?}"))
        .to_string()
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

/// Create the bucket the scripts live in. A regional endpoint takes the location constraint
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

/// Create a Glue job of the row's command kind whose `Command` names `location` as its
/// script, admitting the runs a row keeps in flight at once.
async fn create_job(router: &MultiServiceRouter, name: &str, command: &str, location: &str) {
    let created = glue(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": format!("arn:aws:iam::{ACCOUNT}:role/GlueJobRole"),
            "Command": { "Name": command, "ScriptLocation": location },
            "ExecutionProperty": { "MaxConcurrentRuns": 2 },
        }),
    )
    .await;
    assert_eq!(
        created.get("Name").and_then(Value::as_str),
        Some(name),
        "setup: CreateJob must answer with the name it stored, got {created:?}"
    );
}

/// The definition of a single `Task` state asking for the run-a-job pattern over
/// `glue:startJobRun`: `suffix` is the revision it asks for, and the document the state
/// builds IS `StartJobRun`'s request.
fn definition(suffix: &str, job_name: &str) -> String {
    json!({
        "StartAt": "Job",
        "States": {
            "Job": {
                "Type": "Task",
                "Resource": format!("arn:aws:states:::glue:startJobRun{suffix}"),
                "Parameters": { "JobName": job_name },
                "End": true
            }
        }
    })
    .to_string()
}

/// Create a state machine over `definition` and answer its ARN.
async fn create_machine(router: &MultiServiceRouter, name: &str, definition: &str) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition,
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/StepFunctionsRole"),
        }),
    )
    .await;
    string(name, &created, "stateMachineArn")
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

/// The execution once it has left `RUNNING`.
async fn settled_execution(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let began = Instant::now();
    loop {
        let reported = described(router, execution_arn).await;
        if reported.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return reported;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the execution to leave RUNNING, got {reported:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The `JobRunId` the task's `TaskSubmitted` event reports, read off the history rather than
/// guessed at: the submission is "the response from a resource when a task has started", and
/// it is recorded by the call that parks the task, so it is polled for.
async fn submitted_job_run(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> String {
    let began = Instant::now();
    loop {
        let history = sfn(
            router,
            "GetExecutionHistory",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        let submitted = history
            .get("events")
            .and_then(Value::as_array)
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| {
                        event.get("type").and_then(Value::as_str) == Some("TaskSubmitted")
                    })
                    .cloned()
            });
        if let Some(submitted) = submitted {
            let details = submitted.get("taskSubmittedEventDetails").unwrap_or_else(|| {
                panic!("case {case:?}: expected taskSubmittedEventDetails in {submitted:?}")
            });
            let output = string(case, details, "output");
            let reported: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON submission, got {error} in {output:?}")
            });
            return string(case, &reported, "JobRunId");
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the task to submit its job run, got the history {history:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The run `run_id` of the job `job_name`, as `GetJobRun` reports it.
async fn job_run(router: &MultiServiceRouter, job_name: &str, run_id: &str) -> Value {
    let answered = glue(
        router,
        "GetJobRun",
        &json!({ "JobName": job_name, "RunId": run_id }),
    )
    .await;
    answered
        .get("JobRun")
        .cloned()
        .unwrap_or_else(|| panic!("expected a JobRun for {run_id} of {job_name}, got {answered:?}"))
}

/// The `JobRunState` a reported run is in.
fn state_of(run: &Value) -> &str {
    run.get("JobRunState")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// An instant far enough out that no owed transition falls due while a row is reading.
fn far_future() -> SystemTime {
    SystemTime::now() + Duration::from_secs(3600)
}

/// An instant already past, so the next call settles what the clock owed.
fn already_past() -> SystemTime {
    SystemTime::now() - Duration::from_secs(1)
}

/// Hold the state every unfinished run of `job_name` comes to REST in open, leaving the
/// state the clock owes it NEXT where it was.
///
/// What this fixes is only that a run may not come to rest yet: the transition into the
/// state it is in while its script runs still falls due on the clock, so a row can poll for
/// it, and the stop a row then submits lands on a run that is really still in flight.
fn hold_the_rest_open(engine: &Arc<engine::InMemoryEngine>, job_name: &str) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let Some(job) = store.job_mut(REGION, job_name) else {
        return;
    };
    for run in &mut job.runs {
        if run.rests_at.is_some() {
            run.rests_at = Some(far_future());
        } else if run.state == JOB_RUN_STATE_RUNNING && run.settles_at.is_some() {
            run.settles_at = Some(far_future());
        }
    }
}

/// Make everything the clock still owes every unfinished run of `job_name` due, so the run
/// a row left to its own work comes to rest without the row sleeping for it.
fn make_what_is_owed_due(engine: &Arc<engine::InMemoryEngine>, job_name: &str) {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let mut store = state.lock();
    let Some(job) = store.job_mut(REGION, job_name) else {
        return;
    };
    for run in &mut job.runs {
        if run.settles_at.is_some() {
            run.settles_at = Some(already_past());
        }
        if run.rests_at.is_some() {
            run.rests_at = Some(already_past());
        }
    }
}

/// The distinct states a client polling `GetJobRun` observes, in the order it observed them,
/// until the run reports the state it is in while its script runs — answered beside the run
/// as it then reads.
///
/// The state the run comes to REST in is held open on every turn, so the window the client
/// is polling for cannot close between two of its reads however loaded the host is; what
/// moves the run into that state is still the span the clock owes it.
async fn polled_until_running(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    job_name: &str,
    run_id: &str,
) -> (Vec<String>, Value) {
    let began = Instant::now();
    let mut observed: Vec<String> = Vec::new();
    loop {
        hold_the_rest_open(engine, job_name);
        let run = job_run(router, job_name, run_id).await;
        let state = state_of(&run).to_string();
        if observed.last() != Some(&state) {
            observed.push(state.clone());
        }
        if state == JOB_RUN_STATE_RUNNING {
            return (observed, run);
        }
        // A run that came to rest before the client ever saw it running is the failure this
        // poll is here to catch, so it is reported where it happened rather than waited out.
        assert!(
            !TERMINAL_JOB_RUN_STATES.contains(&state.as_str()),
            "case {case:?}: run {run_id:?} of {job_name:?} came to rest in {state} without \
             having been observed {JOB_RUN_STATE_RUNNING}, reporting {run:?} after {observed:?}"
        );
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: run {run_id:?} of {job_name:?} must reach \
             {JOB_RUN_STATE_RUNNING}, and it reports {run:?} after {observed:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The run `run_id` once it has come to rest, polled for on the SERVICE's own clock: nothing
/// is forced through the store from here, so the state it comes to rest in and when are the
/// lifecycle's own.
async fn run_at_rest(
    router: &MultiServiceRouter,
    case: &str,
    job_name: &str,
    run_id: &str,
) -> Value {
    let began = Instant::now();
    loop {
        let run = job_run(router, job_name, run_id).await;
        if TERMINAL_JOB_RUN_STATES.contains(&state_of(&run)) {
            return run;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: run {run_id:?} of {job_name:?} must come to rest, and it reports \
             {run:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// Who started the run under test.
#[derive(Debug, Clone, Copy)]
enum StartedBy {
    /// The starting call a run-a-job task makes, its `Resource` carrying this revision.
    ASyncTask(&'static str),
    /// A client's own `StartJobRun`.
    AClientsStart,
}

/// What ends the run under test.
#[derive(Debug, Clone, Copy)]
enum EndedBy {
    /// `StopExecution` on the execution whose task started the run, carrying these words.
    StoppingTheExecution(Option<&'static str>, Option<&'static str>),
    /// Glue's own `BatchStopJobRun` for the run itself.
    StoppingTheRun,
    /// Nothing: the run is left to the state its own execution decided.
    ItsOwnScript,
}

/// One row: who starts the run, what ends it, the command kind of the job it is a run of,
/// and the `JobRunState` it must come to rest in.
#[derive(Debug)]
struct Row {
    case: &'static str,
    /// `JobCommand$Name`.
    command: &'static str,
    started_by: StartedBy,
    ended_by: EndedBy,
    rests_in: &'static str,
}

/// The rows of the table.
fn rows() -> [Row; 4] {
    [
        Row {
            // The recorded case: a run reached through a `.sync` task, which a client polls
            // for `RUNNING` and then stops by stopping the owning execution.
            case: "a run a sync task started, stopped through its owning execution",
            command: "pythonshell",
            started_by: StartedBy::ASyncTask(".sync"),
            ended_by: EndedBy::StoppingTheExecution(
                Some("xsvc.StopWhileRunning"),
                Some("stopped while the script was running"),
            ),
            rests_in: JOB_RUN_STATE_STOPPED,
        },
        Row {
            case: "the same through the pattern's second revision, the stop naming no words",
            command: "glueetl",
            started_by: StartedBy::ASyncTask(".sync:2"),
            ended_by: EndedBy::StoppingTheExecution(None, None),
            rests_in: JOB_RUN_STATE_STOPPED,
        },
        Row {
            case: "a run a client started, stopped through Glue's own BatchStopJobRun",
            command: "pythonshell",
            started_by: StartedBy::AClientsStart,
            ended_by: EndedBy::StoppingTheRun,
            rests_in: JOB_RUN_STATE_STOPPED,
        },
        Row {
            case: "a run a sync task started and nothing stopped",
            command: "glueetl",
            started_by: StartedBy::ASyncTask(".sync"),
            ended_by: EndedBy::ItsOwnScript,
            rests_in: JOB_RUN_STATE_SUCCEEDED,
        },
    ]
}

/// Every row: the run is observed `RUNNING` through Glue's own API before anything ends it,
/// and the state it comes to rest in is the one its ending decided — `STOPPED` for a stop
/// submitted while it was in flight, whichever call submitted it, and what its own clean
/// script came to for the run nothing stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_is_observed_running_and_a_stop_while_it_runs_rests_it_stopped() {
    let (router, engine) = emulator();
    create_script_bucket(&router).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let job_name = format!("glue-sync-stop-{index}");
        let location = stage_script(&router, &format!("{job_name}.py"), EXITS_CLEANLY).await;
        create_job(&router, &job_name, row.command, &location).await;

        // The run is begun the row's way, and its id comes from whatever began it.
        let (run_id, execution_arn) = match row.started_by {
            StartedBy::ASyncTask(suffix) => {
                let machine = create_machine(
                    &router,
                    &format!("glue-sync-stop-machine-{index}"),
                    &definition(suffix, &job_name),
                )
                .await;
                let started = sfn(
                    &router,
                    "StartExecution",
                    &json!({ "stateMachineArn": machine, "name": format!("run-{index}") }),
                )
                .await;
                let execution_arn = string(case, &started, "executionArn");
                let run_id = submitted_job_run(&router, case, &execution_arn).await;
                (run_id, Some(execution_arn))
            }
            StartedBy::AClientsStart => {
                let started = glue(&router, "StartJobRun", &json!({ "JobName": job_name })).await;
                (string(case, &started, "JobRunId"), None)
            }
        };

        // A client polling the run observes it RUNNING, and observes nothing outside the
        // states a run that has not finished can be in on the way there.
        let (observed, running) =
            polled_until_running(&router, &engine, case, &job_name, &run_id).await;
        assert_eq!(
            observed.last().map(String::as_str),
            Some(JOB_RUN_STATE_RUNNING),
            "case {case:?}: expected the poller to reach {JOB_RUN_STATE_RUNNING}, got \
             {observed:?}"
        );
        for state in &observed {
            assert!(
                [JOB_RUN_STATE_STARTING, JOB_RUN_STATE_RUNNING].contains(&state.as_str()),
                "case {case:?}: a run in flight is only ever starting or running, got \
                 {observed:?}"
            );
        }
        // And while it is in flight it claims no outcome: neither an instant it finished at
        // nor a reason it failed with.
        assert!(
            running.get("CompletedOn").is_none(),
            "case {case:?}: a running run reports no CompletedOn, got {running:?}"
        );
        assert!(
            running.get("ErrorMessage").is_none(),
            "case {case:?}: a running run carries no ErrorMessage, got {running:?}"
        );
        assert_eq!(
            running.get("JobName").and_then(Value::as_str),
            Some(job_name.as_str()),
            "case {case:?}: expected a run of the job that was started, got {running:?}"
        );

        // Then the row's ending.
        match row.ended_by {
            EndedBy::StoppingTheExecution(error, cause) => {
                let stopped_arn = execution_arn.clone().unwrap_or_else(|| {
                    panic!("case {case:?}: a row stopped through its execution has one")
                });
                let mut request = json!({ "executionArn": stopped_arn });
                let members = request.as_object_mut().expect("the request is an object");
                if let Some(error) = error {
                    members.insert("error".to_string(), json!(error));
                }
                if let Some(cause) = cause {
                    members.insert("cause".to_string(), json!(cause));
                }
                let stopped = sfn(&router, "StopExecution", &request).await;
                assert!(
                    stopped.get("stopDate").is_some(),
                    "case {case:?}: expected a stopDate, got {stopped:?}"
                );
            }
            EndedBy::StoppingTheRun => {
                let submitted = glue(
                    &router,
                    "BatchStopJobRun",
                    &json!({ "JobName": job_name, "JobRunIds": [run_id.clone()] }),
                )
                .await;
                let names: Vec<&str> = submitted
                    .get("SuccessfulSubmissions")
                    .and_then(Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| entry.get("JobRunId").and_then(Value::as_str))
                            .collect()
                    })
                    .unwrap_or_default();
                assert_eq!(
                    names,
                    vec![run_id.as_str()],
                    "case {case:?}: expected a stop submitted for the run, got {submitted:?}"
                );
                assert_eq!(
                    submitted
                        .get("Errors")
                        .and_then(Value::as_array)
                        .map(Vec::len),
                    Some(0),
                    "case {case:?}: a stop of a run in flight fails at nothing, got {submitted:?}"
                );
            }
            // Nothing stops it: what the clock still owes it is made due, so the run comes
            // to rest in the state its own execution decided without the row sleeping.
            EndedBy::ItsOwnScript => make_what_is_owed_due(&engine, &job_name),
        }

        // A run a stop has been submitted for is stopping, or has already stopped, before it
        // comes to rest — it never jumps to the state its own work would have reached.
        if !matches!(row.ended_by, EndedBy::ItsOwnScript) {
            let stopping = job_run(&router, &job_name, &run_id).await;
            assert!(
                [JOB_RUN_STATE_STOPPING, JOB_RUN_STATE_STOPPED].contains(&state_of(&stopping)),
                "case {case:?}: a run a stop was submitted for is stopping or stopped, got \
                 {stopping:?}"
            );
        }

        let rest = run_at_rest(&router, case, &job_name, &run_id).await;
        assert_eq!(
            state_of(&rest),
            row.rests_in,
            "case {case:?}: expected the run to come to rest in {:?}, got {rest:?}",
            row.rests_in
        );
        assert!(
            rest.get("CompletedOn").is_some(),
            "case {case:?}: a run that came to rest reports when it finished, got {rest:?}"
        );
        // Neither ending is a failure: a stopped run and a run whose script exited cleanly
        // both failed at nothing, so neither carries a reason.
        assert!(
            rest.get("ErrorMessage").is_none(),
            "case {case:?}: a run that failed at nothing carries no ErrorMessage, got {rest:?}"
        );

        // The execution of a row whose run a task started ends as that run did: aborted with
        // the caller's own words for a stop, and succeeded for the run that ran to its end.
        if let Some(execution_arn) = execution_arn {
            let settled = settled_execution(&router, case, &execution_arn).await;
            match row.ended_by {
                EndedBy::StoppingTheExecution(error, cause) => {
                    assert_eq!(
                        (
                            settled.get("status").and_then(Value::as_str),
                            settled.get("error").and_then(Value::as_str),
                            settled.get("cause").and_then(Value::as_str),
                        ),
                        (Some("ABORTED"), error, cause),
                        "case {case:?}: expected the execution aborted with the words the stop \
                         carried, got {settled:?}"
                    );
                }
                _ => {
                    assert_eq!(
                        settled.get("status").and_then(Value::as_str),
                        Some("SUCCEEDED"),
                        "case {case:?}: the task is resolved by the run that succeeded, got \
                         {settled:?}"
                    );
                }
            }
        }
    }
}
