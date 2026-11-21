// SPDX-License-Identifier: CC0-1.0

//! WHEN a Step Functions run-a-job (`.sync`) task asks the invoked service what the job
//! it submitted did — driven across the three services the question spans, mounted on
//! ONE engine exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULE: the pattern's reporting call is made only once the invoked service could have
//! moved the job it accepted — one of that service's own settle spans after the
//! submission — so neither the job's report nor a REFUSAL of that report resolves the
//! task while the job is still exactly as the submission left it. A client that reads
//! the job in the service that owns it, as soon as the workflow waiting on it has
//! ended, therefore finds the job where the job's own clock has taken it: a submitted
//! Athena query has completed, and carries its completion instant and its result rows,
//! whether or not the role that submitted it was allowed to observe it.
//!
//! Four crates have to agree for that to hold (Step Functions starts and reports the
//! job, IAM holds the documents the pattern's calls and the result write are judged
//! against, Athena serves both calls and runs the query, and Amazon S3 holds the
//! workgroup's results location), so the suite lives here rather than in any of them.
//!
//! The rows differ along what the rule generalises over: which integration's job it is
//! (an Athena query execution and a nested Step Functions execution, whose services
//! hold a submitted job for spans of their own), what the execution role's policies say
//! about the reporting call (granted, not mentioned, denied over a grant) — so the task
//! is resolved by the job's report in some rows and by a refusal of it in others — and
//! the statement each query projects. A second test drives two workflows started before
//! either is waited for, which is the other order a client runs them in.
//!
//! Nothing here asserts a value this emulator invented: the job's identifier comes off
//! the submission event, the rows come back from the query's own results, and the
//! refusal's own sentence is read out of the `TaskFailed` event.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness the execution-role suites build: the role with its trust document, the
// state machine, the awsJson caller for Step Functions and the reader of a failed
// task's details. `tests/all/main.rs` is one crate root, so this suite borrows them
// rather than carrying a second copy.
use crate::sfn_task_target_authorization::{
    allow, logs_only, machine, role, sfn, string, task_failure, ACCOUNT, REGION,
};
// The caller for Athena and the reader of a query's result rows, from the suite that
// asks the other half of this question of the same service, for the same reason.
use crate::athena_query_progress_without_an_observer::{athena, query_rows};

/// How long a row waits for an execution, or a job, to leave its non-terminal state,
/// and how often it looks while waiting. The budget is generous next to the spans a
/// submission settles after, so a loaded machine has room.
const PATIENCE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(25);

/// The ASL error name a task whose own work failed reports.
const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// The fragment the cause of an access-denied refusal carries: the code the answer came
/// back under.
const REFUSAL_CODE: &str = "Error Code: AccessDeniedException";

/// The history events of a run-a-job task
/// (`smithy_query(query="shape",shape="HistoryEventType")`).
const TASK_SUBMITTED: &str = "TaskSubmitted";
const TASK_SUCCEEDED: &str = "TaskSucceeded";

/// The `Resource` an Athena row's `Task` names, and the IAM actions its two calls are
/// taken under: the operation names of Athena's own `StartQueryExecution` and
/// `GetQueryExecution` under that service's action namespace.
const START_QUERY_SYNC: &str = "arn:aws:states:::athena:startQueryExecution.sync";
const START_QUERY_ACTION: &str = "athena:StartQueryExecution";
const REPORT_QUERY_ACTION: &str = "athena:GetQueryExecution";

/// The `Resource` a nested-execution row's `Task` names, and the IAM actions its two
/// calls are taken under: this service's own `StartExecution` and `DescribeExecution`.
const START_EXECUTION_SYNC: &str = "arn:aws:states:::states:startExecution.sync";
const START_EXECUTION_ACTION: &str = "states:StartExecution";
const REPORT_EXECUTION_ACTION: &str = "states:DescribeExecution";

/// The bucket the workgroup's results are written into, and the prefix inside it.
/// Nothing here reads the objects: the location is what lets a query run at all ("if
/// none of them is set, Athena issues an error that no output location is provided"),
/// and Athena verifies the bucket is there before it accepts a submission.
const BUCKET: &str = "sfn-run-a-job-clock-results";
const PREFIX: &str = "results";

/// The workgroup every query here runs in.
const WORK_GROUP: &str = "sfn-run-a-job-clock";

/// The four services this question spans, over ONE shared engine.
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
    router.register_service(
        service_com_amazonaws_athena_amazonathena::EmulatedService::new(engine),
    );
    router
}

/// The ARN a policy names every object of the results bucket by, which is what a result
/// object is written under: the query's results are saved with the submitting caller's
/// own permissions, so a role that may submit a query carries that object write.
fn results_objects_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}/*")
}

/// The IAM action an object write is taken under, as Amazon S3 publishes it.
const OBJECT_WRITE_ACTION: &str = "s3:PutObject";

/// The ARN a policy names the workgroup `name` by, which is the resource Athena takes a
/// query's actions on.
fn work_group_arn(name: &str) -> String {
    format!("arn:aws:athena:{REGION}:{ACCOUNT}:workgroup/{name}")
}

/// The results bucket, and the workgroup whose queries write into it.
async fn given_the_results_location(router: &MultiServiceRouter) {
    let configuration = format!(
        "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <LocationConstraint>{REGION}</LocationConstraint></CreateBucketConfiguration>"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{BUCKET}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(configuration))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status, 200,
        "the results bucket must be created, got {status} with {:?}",
        String::from_utf8_lossy(response.body())
    );
    athena(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "ResultConfiguration": {
                    "OutputLocation": format!("s3://{BUCKET}/{PREFIX}/"),
                },
                "EnforceWorkGroupConfiguration": true,
            },
        }),
    )
    .await;
}

/// A state machine whose one `Pass` state ends it, which is the job a nested-execution
/// row submits; answers its ARN.
async fn given_child_machine(router: &MultiServiceRouter, name: &str) -> String {
    let created = sfn(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": json!({
                "StartAt": "Done",
                "States": { "Done": { "Type": "Pass", "End": true } },
            })
            .to_string(),
            "roleArn": format!("arn:aws:iam::{ACCOUNT}:role/unused-by-a-pass"),
        }),
    )
    .await;
    string(&created, "stateMachineArn")
}

/// The events of one execution's history.
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

/// How many events of `events` are of the type `event_type`.
fn count_of(events: &[Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .count()
}

/// The job identifier the one `TaskSubmitted` event of this history reports under
/// `member`, which is what the starting call answered.
fn submitted_job_id(events: &[Value], member: &str, case: &str) -> String {
    assert_eq!(
        count_of(events, TASK_SUBMITTED),
        1,
        "case {case:?}: expected exactly one {TASK_SUBMITTED}, got {events:?}"
    );
    let submitted = events
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(TASK_SUBMITTED))
        .unwrap_or_else(|| panic!("case {case:?}: expected a {TASK_SUBMITTED} event"));
    let output = submitted
        .get("taskSubmittedEventDetails")
        .and_then(|details| details.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the submission's output, got {submitted:?}")
        });
    let answered: Value = serde_json::from_str(output)
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON submission, got {output:?}"));
    answered
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the submission to name the {member}, got {answered:?}")
        })
        .to_string()
}

/// Start one execution of `machine_arn` under `name`, and answer its ARN.
async fn started(router: &MultiServiceRouter, machine_arn: &str, name: &str) -> String {
    let answered = sfn(
        router,
        "StartExecution",
        &json!({ "stateMachineArn": machine_arn, "name": name }),
    )
    .await;
    string(&answered, "executionArn")
}

/// The execution `execution_arn`, once it has left `RUNNING`.
async fn terminal(router: &MultiServiceRouter, execution_arn: &str, case: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = sfn(
            router,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal execution within {PATIENCE:?}, got {described:?}"
        );
        ::tokio::time::sleep(LOOK).await;
    }
}

/// Which job a row's `Task` submits, and so which service owns it.
#[derive(Debug, Clone, Copy)]
enum Job {
    /// An Athena query execution: the statement projects `literal` under the column
    /// name `alias`, so a successful query's results are the header row and one data
    /// row of the caller's own literal.
    AthenaQuery {
        alias: &'static str,
        literal: &'static str,
    },
    /// A nested execution of a state machine, through this service's own API.
    NestedExecution,
}

impl Job {
    /// The `Resource` the `Task` state names to submit this job.
    fn resource(self) -> &'static str {
        match self {
            Job::AthenaQuery { .. } => START_QUERY_SYNC,
            Job::NestedExecution => START_EXECUTION_SYNC,
        }
    }

    /// The IAM action the call that SUBMITS this job is taken under, and the action the
    /// call that REPORTS it is taken under.
    fn actions(self) -> (&'static str, &'static str) {
        match self {
            Job::AthenaQuery { .. } => (START_QUERY_ACTION, REPORT_QUERY_ACTION),
            Job::NestedExecution => (START_EXECUTION_ACTION, REPORT_EXECUTION_ACTION),
        }
    }

    /// The member of the submission's own answer that identifies the job.
    fn id_member(self) -> &'static str {
        match self {
            Job::AthenaQuery { .. } => "QueryExecutionId",
            Job::NestedExecution => "ExecutionArn",
        }
    }
}

/// What a row's execution role says about the pattern's REPORTING call.
#[derive(Debug, Clone, Copy)]
enum Report {
    /// The role is granted it.
    Granted,
    /// The role carries no statement about it.
    Unmentioned,
    /// The role is granted it and then explicitly denied it.
    Denied,
}

impl Report {
    /// Whether the policies let the pattern report the job.
    fn allowed(self) -> bool {
        matches!(self, Report::Granted)
    }
}

/// One row: the case, the job its `Task` submits, and what its role says about the
/// reporting call.
struct Row {
    case: &'static str,
    job: Job,
    report: Report,
}

/// A run-a-job task resolves no sooner than the job it submitted could have moved, so
/// the job's own service has carried the job to its outcome by the time the workflow
/// waiting on it ends — whether the task was resolved by the job's report or by a
/// refusal of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jobs_own_service_has_moved_it_on_by_the_time_its_workflow_ends() {
    let router = emulator();
    given_the_results_location(&router).await;
    let child_machine = given_child_machine(&router, "run-a-job-clock-child").await;

    let rows = vec![
        Row {
            case: "an Athena query whose reporting call is granted",
            job: Job::AthenaQuery {
                alias: "marker",
                literal: "north-1",
            },
            report: Report::Granted,
        },
        Row {
            case: "an Athena query with no statement about the reporting call",
            job: Job::AthenaQuery {
                alias: "label",
                literal: "south 2",
            },
            report: Report::Unmentioned,
        },
        Row {
            case: "an Athena query whose reporting call is denied over a grant",
            job: Job::AthenaQuery {
                alias: "region",
                literal: "east-3",
            },
            report: Report::Denied,
        },
        Row {
            case: "a nested execution whose reporting call is granted",
            job: Job::NestedExecution,
            report: Report::Granted,
        },
        Row {
            case: "a nested execution with no statement about the reporting call",
            job: Job::NestedExecution,
            report: Report::Unmentioned,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let (start_action, report_action) = row.job.actions();
        // What the pattern's calls are taken ON: a query's workgroup, and the child
        // execution the nested start minted — which no policy can name in advance, so
        // the grant for it is over every resource.
        let (start_resource, report_resource) = match row.job {
            Job::AthenaQuery { .. } => (work_group_arn(WORK_GROUP), work_group_arn(WORK_GROUP)),
            Job::NestedExecution => (child_machine.clone(), "*".to_string()),
        };
        let mut statements = vec![logs_only(), allow(start_action, &start_resource)];
        match row.report {
            Report::Granted => statements.push(allow(report_action, &report_resource)),
            Report::Unmentioned => {}
            Report::Denied => {
                statements.push(allow(report_action, &report_resource));
                statements.push(json!({
                    "Effect": "Deny",
                    "Action": report_action,
                    "Resource": &report_resource,
                }));
            }
        }
        // A job this emulator stops when nobody waits for it any more is stopped as the
        // same role, so every row carries that grant whatever it says about reporting;
        // and a query's results are saved with the submitting caller's own permissions,
        // so the object write goes with the grant to submit one.
        statements.push(allow("states:StopExecution", "*"));
        statements.push(allow("athena:StopQueryExecution", &work_group_arn(WORK_GROUP)));
        statements.push(allow(OBJECT_WRITE_ACTION, &results_objects_arn()));
        let role_arn = role(
            &router,
            &format!("RunAJobClockRole{index}"),
            &json!({ "Version": "2012-10-17", "Statement": statements }),
        )
        .await;

        let parameters = match row.job {
            Job::AthenaQuery { alias, literal } => json!({
                "QueryString": format!("SELECT '{literal}' AS {alias}"),
                "WorkGroup": WORK_GROUP,
            }),
            Job::NestedExecution => json!({ "StateMachineArn": child_machine }),
        };
        let machine_arn = machine(
            &router,
            &format!("run-a-job-clock-{index}"),
            row.job.resource(),
            &parameters,
            &role_arn,
        )
        .await;

        let execution_arn = started(&router, &machine_arn, "only").await;
        let described = terminal(&router, &execution_arn, case).await;
        let events = history(&router, &execution_arn).await;
        // Whatever the policies say about reporting the job, the SUBMITTING call was
        // granted, so the job was really submitted and the task really parked.
        let job_id = submitted_job_id(&events, row.job.id_member(), case);

        // The job, read ONCE in the service that owns it, now that the workflow waiting
        // on it has ended: its own clock has carried it to its outcome.
        match row.job {
            Job::AthenaQuery { alias, literal } => {
                let reported = athena(
                    &router,
                    "GetQueryExecution",
                    &json!({ "QueryExecutionId": &job_id }),
                )
                .await;
                let status = &reported["QueryExecution"]["Status"];
                assert_eq!(
                    status["State"].as_str(),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the submitted query to have completed, got \
                     {reported:?}"
                );
                assert!(
                    status["CompletionDateTime"].is_number(),
                    "case {case:?}: expected a completed query to carry its completion \
                     instant, got {status:?}"
                );
                let rows = query_rows(&router, &job_id).await;
                assert_eq!(
                    rows,
                    vec![vec![alias.to_string()], vec![literal.to_string()]],
                    "case {case:?}: expected the header row and the one row the statement \
                     projects, got {rows:?}"
                );
            }
            Job::NestedExecution => {
                let child = sfn(
                    &router,
                    "DescribeExecution",
                    &json!({ "executionArn": &job_id }),
                )
                .await;
                assert_eq!(
                    child.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the submitted execution to have ended, got \
                     {child:?}"
                );
                assert!(
                    child.get("stopDate").is_some_and(|stopped| !stopped.is_null()),
                    "case {case:?}: expected an ended execution to carry the instant it \
                     stopped, got {child:?}"
                );
            }
        }

        // What the TASK did, which is the other half of the rule: a report the role may
        // make resolves the task with the job's own document, and one it may not leaves
        // the task failing with the refusal of that report.
        if row.report.allowed() {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the workflow to carry the job's report, got \
                 {described:?}"
            );
            assert_eq!(
                count_of(&events, TASK_SUCCEEDED),
                1,
                "case {case:?}: expected exactly one {TASK_SUCCEEDED}, got {events:?}"
            );
            continue;
        }
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(TASK_FAILED_ERROR)),
            "case {case:?}: expected the refused report to fail the task, got {described:?}"
        );
        let details = task_failure(&router, &execution_arn, case).await;
        let cause = string(&details, "cause");
        assert!(
            cause.contains(REFUSAL_CODE) && cause.contains(report_action),
            "case {case:?}: expected the cause to carry {REFUSAL_CODE:?} and name \
             {report_action}, got {cause:?}"
        );
        assert_eq!(
            count_of(&events, TASK_SUCCEEDED),
            0,
            "case {case:?}: nothing reported the job, so no {TASK_SUCCEEDED} may appear, got \
             {events:?}"
        );
    }
}

/// The same rule when two workflows are started before either is waited for: each
/// submitted query is completed, with its own rows, by the time its workflow has ended.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workflows_started_before_either_is_waited_for() {
    let router = emulator();
    given_the_results_location(&router).await;
    let group_arn = work_group_arn(WORK_GROUP);

    // (case, the literal the query projects, whether the role may report the query)
    let rows = [
        ("the workflow whose role may report its query", "west-4", true),
        ("the workflow whose role may not", "west-5", false),
    ];

    let mut running = Vec::new();
    for (index, (case, literal, reports)) in rows.into_iter().enumerate() {
        let mut statements = vec![
            logs_only(),
            allow(START_QUERY_ACTION, &group_arn),
            allow(OBJECT_WRITE_ACTION, &results_objects_arn()),
        ];
        if reports {
            statements.push(allow(REPORT_QUERY_ACTION, &group_arn));
        }
        let role_arn = role(
            &router,
            &format!("RunAJobClockPairRole{index}"),
            &json!({ "Version": "2012-10-17", "Statement": statements }),
        )
        .await;
        let machine_arn = machine(
            &router,
            &format!("run-a-job-clock-pair-{index}"),
            START_QUERY_SYNC,
            &json!({
                "QueryString": format!("SELECT '{literal}' AS marker"),
                "WorkGroup": WORK_GROUP,
            }),
            &role_arn,
        )
        .await;
        running.push((case, literal, started(&router, &machine_arn, "only").await));
    }

    for (case, literal, execution_arn) in running {
        terminal(&router, &execution_arn, case).await;
        let events = history(&router, &execution_arn).await;
        let query_id = submitted_job_id(&events, "QueryExecutionId", case);
        let reported = athena(
            &router,
            "GetQueryExecution",
            &json!({ "QueryExecutionId": &query_id }),
        )
        .await;
        assert_eq!(
            reported["QueryExecution"]["Status"]["State"].as_str(),
            Some("SUCCEEDED"),
            "case {case:?}: expected the submitted query to have completed, got {reported:?}"
        );
        let rows = query_rows(&router, &query_id).await;
        assert_eq!(
            rows,
            vec![vec!["marker".to_string()], vec![literal.to_string()]],
            "case {case:?}: expected the header row and the one row the statement projects, \
             got {rows:?}"
        );
    }
}
