// SPDX-License-Identifier: CC0-1.0

//! What a Step Functions run-a-job (`.sync`) task is left with when the execution role
//! may not make the pattern's COMPLETION POLL — the call that asks the invoked service
//! what the job it submitted did — driven across the services the question spans,
//! mounted on ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: an authorization refusal of a `.sync` pattern's completion poll is not an
//! answer ABOUT the job — it says the role may not ask, and nothing about what the job
//! did — so what the waiting task is held to is the invoked integration's own
//! behaviour: the integration whose poll takes a refused report as NO observation keeps
//! the task submitted, so the time the state allowed its task is what ends the
//! execution (a `TaskTimedOut` event, and `FAILED` under the language's own
//! `States.Timeout`, with neither a succeeded nor a failed task reported), while the
//! integration whose poll surfaces the refusal fails the task with it
//! (`States.TaskFailed`). Either way the JOB is untouched: the denial is on observing
//! the run, not on running it, so the run reaches its own terminal state in the service
//! that owns it.
//!
//! Three crates have to agree for that to hold (Step Functions starts and polls the
//! job, IAM holds the documents each poll is judged against, and Glue serves the
//! starting call and runs the job), so the suite lives here rather than in any of them.
//!
//! The rows differ along what the rule generalises over: what the execution role's
//! policies say about the completion poll (no statement about it, an explicit `Deny`
//! over a grant, and a grant — the control, which must still be resolved by the job's
//! own report), the `TimeoutSeconds` the `Task` state declares, and — in the second
//! test — WHICH integration's job it is, over the catalogue entry this suite's first
//! test does not cover: a nested execution, whose poll surfaces the refusal instead.
//!
//! Nothing here asserts a value this emulator invented: the job run id comes off the
//! `TaskSubmitted` event, what the run ended as is read through Glue's own `GetJobRun`,
//! the marker its script wrote is read back out of Amazon S3, and the terminal error
//! comes back from `DescribeExecution`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness the execution-role suites build: the role with its trust document, the
// awsJson caller for Step Functions, and the statement helpers. `tests/all/main.rs` is
// one crate root, so this suite borrows them rather than carrying a second copy.
use crate::sfn_task_target_authorization::{
    allow, iam, logs_only, role, sfn, string, ACCOUNT, REGION,
};

/// How long a wait for a status looks for it before declaring it never arrived, and how
/// often it looks. Generous next to every bound a row declares, so a loaded host is
/// never the thing that trips it.
const PATIENCE: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(25);

/// The ASL error names the two readings report
/// (`smithy_query(query="shape",shape="HistoryEventType")` names the events each is
/// recorded under).
const TIMEOUT_ERROR: &str = "States.Timeout";
const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// The history events this suite reads
/// (`smithy_query(query="shape",shape="HistoryEventType")`).
const TASK_SUBMITTED: &str = "TaskSubmitted";
const TASK_SUCCEEDED: &str = "TaskSucceeded";
const TASK_FAILED: &str = "TaskFailed";
const TASK_TIMED_OUT: &str = "TaskTimedOut";

/// The IAM actions the Glue integration's two calls are taken under: the operation names
/// of Glue's own `StartJobRun` and `GetJobRun` under that service's action namespace.
const START_RUN_ACTION: &str = "glue:StartJobRun";
const REPORT_RUN_ACTION: &str = "glue:GetJobRun";

/// The action the stop of a job nobody waits for any more is taken under, which every
/// row carries whatever it says about reporting.
const STOP_RUN_ACTION: &str = "glue:BatchStopJobRun";

/// The `Resource` a Glue row's `Task` names.
const START_JOB_RUN_SYNC: &str = "arn:aws:states:::glue:startJobRun.sync";

/// The `JobRunState` a Glue run that did its work ends on, and every state a run never
/// moves out of again (`smithy_query(query="shape",shape="JobRunState",
/// model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/
/// com-amazonaws-glue-awsglue.json")` lists every value; `STARTING`, `RUNNING`,
/// `STOPPING` and `WAITING` each still owe a further state).
const RUN_SUCCEEDED: &str = "SUCCEEDED";
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// The bucket the job's script, and the marker that script writes, live in.
const BUCKET: &str = "sfn-sync-poll-refused";

/// The object the job's script is staged as.
const SCRIPT_KEY: &str = "scripts/marker.py";

/// The script every job here runs: it writes the token its `--token` argument carries
/// under the key its own arguments name, so the run's effect is readable from the object
/// store rather than from the run record alone.
const SCRIPT: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"],
    Key=named["--key"],
    Body=named["--token"].encode("utf-8"),
)
"#;

/// Step Functions, IAM, Glue and Amazon S3 over ONE shared engine. Amazon S3 is mounted
/// because a job run READS its script from it and this script WRITES through it, both
/// over the emulator's own in-process callback.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        engine,
    ));
    router
}

/// One Amazon S3 request against the object `key` of [`BUCKET`], answered as
/// `(status, body)`.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.unwrap_or_default().to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The bucket, with the job's script staged in it.
async fn given_the_script(router: &MultiServiceRouter) -> String {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let (status, body) = s3(router, "PUT", &format!("/{BUCKET}"), Some(&configuration)).await;
    assert_eq!(
        status, 200,
        "setup: the bucket {BUCKET} must be created, got {status} with {body}"
    );
    let (status, body) = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/{SCRIPT_KEY}"),
        Some(SCRIPT),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the script must be staged, got {status} with {body}"
    );
    format!("s3://{BUCKET}/{SCRIPT_KEY}")
}

/// The body of the object `key`, once the object store serves it: the marker a run's
/// script wrote. Polled for, because the script runs inside the job rather than inside
/// any call this suite makes.
async fn marker(router: &MultiServiceRouter, key: &str, case: &str) -> String {
    let began = Instant::now();
    loop {
        let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), None).await;
        if status == 200 {
            return body;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the run's marker {key} within {PATIENCE:?}, got {status} \
             with {body}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// Call one Glue operation (awsJson1_1, `AWSGlue.<Operation>`), requiring a 200. The
/// signature is never checked: this router runs with request authentication off, and the
/// credential scope is what routes the request.
async fn glue(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("glue.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSGlue.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/glue/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
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

/// The ARN a policy names the Glue job `name` by, which is the resource Glue takes a
/// run's actions on.
fn job_arn(name: &str) -> String {
    format!("arn:aws:glue:{REGION}:{ACCOUNT}:job/{name}")
}

/// The name of the role the jobs here run AS, and the trust document that lets Glue
/// assume it — the one statement a service role for Glue carries.
const JOB_ROLE: &str = "SyncPollRefusedGlueJobRole";

/// The role a Glue job runs as, created once: a job whose role Glue cannot assume is a
/// run that fails, and a run executes its script AS that role, so the role carries the
/// object reads and writes the script makes.
async fn given_job_role(router: &MultiServiceRouter) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", JOB_ROLE.to_string()),
            (
                "AssumeRolePolicyDocument",
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "glue.amazonaws.com" },
                        "Action": "sts:AssumeRole",
                    }],
                })
                .to_string(),
            ),
        ],
    )
    .await;
    assert!(
        created.contains(JOB_ROLE),
        "setup: CreateRole must answer with the role it stored, got {created}"
    );
    let objects = format!("arn:aws:s3:::{BUCKET}/*");
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", JOB_ROLE.to_string()),
            ("PolicyName", "inline".to_string()),
            (
                "PolicyDocument",
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        allow("s3:GetObject", &objects),
                        allow("s3:PutObject", &objects),
                    ],
                })
                .to_string(),
            ),
        ],
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{JOB_ROLE}")
}

/// A Python-shell Glue job under `name`, running the staged script
/// ([`given_the_script`]), which is the job every row's task runs.
async fn given_job(
    router: &MultiServiceRouter,
    name: &str,
    job_role_arn: &str,
    script_location: &str,
) {
    let created = glue(
        router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": job_role_arn,
            "Command": {
                "Name": "pythonshell",
                "ScriptLocation": script_location,
            },
        }),
    )
    .await;
    assert_eq!(
        created.get("Name").and_then(Value::as_str),
        Some(name),
        "setup: CreateJob must answer with the name it stored, got {created:?}"
    );
}

/// A state machine whose one `Task` state asks for `resource` with `parameters` and
/// declares its own `TimeoutSeconds`; answers its ARN.
///
/// The bound is the STATE's: "if the task runs longer than the specified seconds, this
/// state fails with a States.Timeout error name".
async fn given_machine(
    router: &MultiServiceRouter,
    name: &str,
    resource: &str,
    parameters: &Value,
    timeout_seconds: u64,
    role_arn: &str,
) -> String {
    let definition = json!({
        "StartAt": "Work",
        "States": {
            "Work": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": parameters,
                "TimeoutSeconds": timeout_seconds,
                "End": true,
            }
        }
    });
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": role_arn,
        }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// Start one execution of `machine_arn` and answer its ARN.
async fn started(router: &MultiServiceRouter, machine_arn: &str) -> String {
    let answered = sfn(
        router,
        "StartExecution",
        &json!({ "stateMachineArn": machine_arn, "name": "only" }),
    )
    .await;
    string(&answered, "executionArn")
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

/// The events of one execution's history.
async fn history(router: &MultiServiceRouter, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn, "maxResults": 1000 }),
    )
    .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How many events of `events` are of the type `event_type`.
fn count_of(events: &[Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .count()
}

/// The document the one `TaskSubmitted` event of `execution_arn` reports as its output,
/// parsed — which for an optimized integration is the literal response of the call the
/// task made. Polled for, because the submission is recorded by the call that parks the
/// job.
async fn submission(router: &MultiServiceRouter, execution_arn: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let events = history(router, execution_arn).await;
        let submitted = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_SUBMITTED))
            .cloned();
        if let Some(submitted) = submitted {
            let details = submitted.get("taskSubmittedEventDetails").unwrap_or_else(|| {
                panic!("case {case:?}: expected taskSubmittedEventDetails in {submitted:?}")
            });
            let output = string(details, "output");
            return serde_json::from_str(&output).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON submission, got {error} in {output:?}")
            });
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the task to submit its job within {PATIENCE:?}, got the \
             history {events:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// The run `run_id` of `job_name`, once Glue reports it in a state it never moves out of
/// again.
async fn settled_run(
    router: &MultiServiceRouter,
    job_name: &str,
    run_id: &str,
    case: &str,
) -> Value {
    let began = Instant::now();
    loop {
        let answered = glue(
            router,
            "GetJobRun",
            &json!({ "JobName": job_name, "RunId": run_id }),
        )
        .await;
        let run = answered.get("JobRun").cloned().unwrap_or(Value::Null);
        let state = run
            .get("JobRunState")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if TERMINAL_RUN_STATES.contains(&state.as_str()) {
            return run;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected the job run to come to rest within {PATIENCE:?}, got \
             {answered:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// The execution `execution_arn`, once it has left `RUNNING`.
async fn settled_execution(router: &MultiServiceRouter, execution_arn: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let answered = described(router, execution_arn).await;
        if answered.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return answered;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal execution within {PATIENCE:?}, got {answered:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// What a row's execution role says about the pattern's completion poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Poll {
    /// The role is granted it, so the job's own report resolves the task.
    Granted,
    /// The role carries no statement about it.
    Unmentioned,
    /// The role is granted it and then explicitly denied it.
    DeniedOverAGrant,
}

/// One row: the case, what the role says about the completion poll, the
/// `TimeoutSeconds` the `Task` state declares, and the token its job's script writes.
struct Row {
    case: &'static str,
    poll: Poll,
    timeout_seconds: u64,
    token: &'static str,
}

/// A Glue `.sync` task whose completion poll the role may not make keeps its submission
/// and is ended by the time the state allowed it, while the run itself succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_glue_completion_poll_leaves_the_task_to_the_states_own_bound() {
    let router = emulator();
    let script_location = given_the_script(&router).await;
    let job_role_arn = given_job_role(&router).await;
    let rows = [
        Row {
            case: "no statement about the completion poll",
            poll: Poll::Unmentioned,
            timeout_seconds: 30,
            token: "unmentioned",
        },
        Row {
            case: "the completion poll denied over a grant",
            poll: Poll::DeniedOverAGrant,
            timeout_seconds: 40,
            token: "denied over a grant",
        },
        Row {
            case: "the completion poll granted",
            poll: Poll::Granted,
            timeout_seconds: 30,
            token: "granted",
        },
    ];

    // Every row's work is started before any of it is waited for: the bounds the rows
    // declare are the states' own, so they run side by side rather than one after
    // another.
    let mut running = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let job_name = format!("sfn-sync-poll-{index}");
        given_job(&router, &job_name, &job_role_arn, &script_location).await;
        let named = job_arn(&job_name);
        let mut statements = vec![
            logs_only(),
            allow(START_RUN_ACTION, &named),
            allow(STOP_RUN_ACTION, &named),
        ];
        match row.poll {
            Poll::Granted => statements.push(allow(REPORT_RUN_ACTION, &named)),
            Poll::Unmentioned => {}
            Poll::DeniedOverAGrant => {
                statements.push(allow(REPORT_RUN_ACTION, &named));
                statements.push(json!({
                    "Effect": "Deny",
                    "Action": REPORT_RUN_ACTION,
                    "Resource": &named,
                }));
            }
        }
        let role_arn = role(
            &router,
            &format!("SyncPollRefusedRole{index}"),
            &json!({ "Version": "2012-10-17", "Statement": statements }),
        )
        .await;
        let marker_key = format!("markers/{job_name}");
        let machine_arn = given_machine(
            &router,
            &format!("sfn-sync-poll-{index}"),
            START_JOB_RUN_SYNC,
            &json!({
                "JobName": &job_name,
                "Arguments": {
                    "--bucket": BUCKET,
                    "--key": &marker_key,
                    "--token": row.token,
                },
            }),
            row.timeout_seconds,
            &role_arn,
        )
        .await;
        running.push((
            row,
            job_name,
            marker_key,
            started(&router, &machine_arn).await,
        ));
    }

    for (row, job_name, marker_key, execution_arn) in running {
        let case = row.case;
        // Whatever the policies say about the poll, the STARTING call was granted, so a
        // run was really submitted and the task really parked.
        let submitted = submission(&router, &execution_arn, case).await;
        let run_id = string(&submitted, "JobRunId");

        // The run, in the service that owns it: the denial is on observing it, so it
        // does its work — the marker its script wrote is there to read — and comes to
        // rest having succeeded.
        let run = settled_run(&router, &job_name, &run_id, case).await;
        assert_eq!(
            run.get("JobRunState").and_then(Value::as_str),
            Some(RUN_SUCCEEDED),
            "case {case:?}: expected the submitted run to have succeeded, got {run:?}"
        );
        let wrote = marker(&router, &marker_key, case).await;
        assert_eq!(
            wrote, row.token,
            "case {case:?}: expected the run's script to have written its token, got {wrote:?}"
        );

        if row.poll != Poll::Granted {
            // The run has ended and nothing told the workflow so: the refusal resolved
            // nothing, so the task is still submitted and its execution still running.
            let waiting = described(&router, &execution_arn).await;
            assert_eq!(
                waiting.get("status").and_then(Value::as_str),
                Some("RUNNING"),
                "case {case:?}: a refused poll must leave the task submitted, got {waiting:?}"
            );
        }

        let answered = settled_execution(&router, &execution_arn, case).await;
        let events = history(&router, &execution_arn).await;
        if row.poll == Poll::Granted {
            assert_eq!(
                answered.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the job's own report to resolve the task, got \
                 {answered:?}"
            );
            assert_eq!(
                (
                    count_of(&events, TASK_SUCCEEDED),
                    count_of(&events, TASK_TIMED_OUT),
                ),
                (1, 0),
                "case {case:?}: expected exactly one {TASK_SUCCEEDED} and no {TASK_TIMED_OUT}, \
                 got {events:?}"
            );
            continue;
        }
        assert_eq!(
            (
                answered.get("status").and_then(Value::as_str),
                answered.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(TIMEOUT_ERROR)),
            "case {case:?}: expected the state's own bound to end the execution, got {answered:?}"
        );
        // A bound that simply ran out is reported by its error name alone: nobody
        // answered, so there is no cause to carry.
        assert!(
            answered.get("cause").is_none_or(Value::is_null),
            "case {case:?}: expected no cause beside {TIMEOUT_ERROR}, got {answered:?}"
        );
        assert_eq!(
            (
                count_of(&events, TASK_SUBMITTED),
                count_of(&events, TASK_TIMED_OUT),
                count_of(&events, TASK_SUCCEEDED),
                count_of(&events, TASK_FAILED),
            ),
            (1, 1, 0, 0),
            "case {case:?}: expected the submission and one {TASK_TIMED_OUT}, with neither \
             {TASK_SUCCEEDED} nor {TASK_FAILED}, got {events:?}"
        );
        let timed_out = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_TIMED_OUT))
            .unwrap_or_else(|| panic!("case {case:?}: expected a {TASK_TIMED_OUT} event"));
        let details = timed_out
            .get("taskTimedOutEventDetails")
            .unwrap_or_else(|| panic!("case {case:?}: expected taskTimedOutEventDetails"));
        assert_eq!(
            details.get("error").and_then(Value::as_str),
            Some(TIMEOUT_ERROR),
            "case {case:?}: expected the event to report {TIMEOUT_ERROR}, got {timed_out:?}"
        );
        // The event names the WORK the state asked for — required on its details — and it is
        // the work the task's own submission named, rather than a second reading of it.
        let submitted = events
            .iter()
            .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_SUBMITTED))
            .and_then(|event| event.get("taskSubmittedEventDetails"))
            .unwrap_or_else(|| panic!("case {case:?}: expected taskSubmittedEventDetails"));
        for member in ["resourceType", "resource"] {
            let named = submitted.get(member).and_then(Value::as_str);
            assert!(
                named.is_some(),
                "case {case:?}: expected the submission to name the {member}, got {submitted:?}"
            );
            assert_eq!(
                details.get(member).and_then(Value::as_str),
                named,
                "case {case:?}: expected the {TASK_TIMED_OUT} event to name the {member} the \
                 submission named ({named:?}), got {details:?}"
            );
        }
    }
}

/// The `Resource` the other entry's `Task` names, and the actions its two calls are
/// taken under: this service's own `StartExecution` and `DescribeExecution`.
const START_EXECUTION_SYNC: &str = "arn:aws:states:::states:startExecution.sync";
const START_EXECUTION_ACTION: &str = "states:StartExecution";
const REPORT_EXECUTION_ACTION: &str = "states:DescribeExecution";

/// The OTHER reading of the same refusal, over an entry of the catalogue the test above
/// does not cover: a nested execution's poll surfaces the refusal, so the task fails
/// with it rather than waiting for the state's bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_nested_execution_poll_fails_the_task_instead() {
    let router = emulator();
    let child = sfn(
        &router,
        "CreateStateMachine",
        &json!({
            "name": "sync-poll-refused-child",
            "definition": json!({
                "StartAt": "Done",
                "States": { "Done": { "Type": "Pass", "End": true } },
            })
            .to_string(),
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/unused-by-a-pass"),
        }),
    )
    .await;
    let child_arn = string(&child, "stateMachineArn");

    let case = "a nested execution with no statement about the completion poll";
    let role_arn = role(
        &router,
        "SyncPollRefusedNestedRole",
        &json!({
            "Version": "2012-10-17",
            "Statement": [
                logs_only(),
                allow(START_EXECUTION_ACTION, &child_arn),
                // The child the start mints is named by no policy in advance, so the
                // stop a job nobody waits for any more owes is granted over every
                // resource; the REPORT is the one call left unmentioned.
                allow("states:StopExecution", "*"),
            ],
        }),
    )
    .await;
    let machine_arn = given_machine(
        &router,
        "sfn-sync-poll-nested",
        START_EXECUTION_SYNC,
        &json!({ "StateMachineArn": &child_arn }),
        // The same bound the Glue rows declare, so what ends this execution cannot be
        // the absence of one.
        30,
        &role_arn,
    )
    .await;

    let execution_arn = started(&router, &machine_arn).await;
    let answered = settled_execution(&router, &execution_arn, case).await;
    assert_eq!(
        (
            answered.get("status").and_then(Value::as_str),
            answered.get("error").and_then(Value::as_str),
        ),
        (Some("FAILED"), Some(TASK_FAILED_ERROR)),
        "case {case:?}: expected the refused poll to fail the task, got {answered:?}"
    );
    let events = history(&router, &execution_arn).await;
    assert_eq!(
        (
            count_of(&events, TASK_SUBMITTED),
            count_of(&events, TASK_FAILED),
            count_of(&events, TASK_TIMED_OUT),
            count_of(&events, TASK_SUCCEEDED),
        ),
        (1, 1, 0, 0),
        "case {case:?}: expected the submission and one {TASK_FAILED}, with neither \
         {TASK_TIMED_OUT} nor {TASK_SUCCEEDED}, got {events:?}"
    );
    // The refusal the task carries is the one the call that reports the child answered.
    let failed = events
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_FAILED))
        .unwrap_or_else(|| panic!("case {case:?}: expected a {TASK_FAILED} event"));
    let cause = failed
        .get("taskFailedEventDetails")
        .map(|details| string(details, "cause"))
        .unwrap_or_else(|| panic!("case {case:?}: expected taskFailedEventDetails"));
    assert!(
        cause.contains(REPORT_EXECUTION_ACTION),
        "case {case:?}: expected the cause to name {REPORT_EXECUTION_ACTION}, got {cause:?}"
    );
}
