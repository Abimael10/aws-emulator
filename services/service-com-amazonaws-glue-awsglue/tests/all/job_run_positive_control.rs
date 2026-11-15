// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the POSITIVE CONTROL of a Glue job run, and for what tells it apart
//! from a run that failed.
//!
//! The rule under test: a run of a job is ONE execution of the script its
//! `JobCommand$ScriptLocation` names, performed as the job's `Role`, and the run comes to rest
//! in the state that execution reached — so a submission with nothing wrong with it reaches
//! `SUCCEEDED` carrying NO `ErrorMessage`, with `CompletedOn` set and an `ExecutionTime` ("the
//! amount of time (in seconds) that the job run consumed resources", `JobRun$ExecutionTime`)
//! that spans the run, and a submission that is wrong in one way comes to rest carrying the
//! message of THAT failure class, which no other class states. A caller can therefore tell a
//! run that succeeded from one that failed, and two runs that failed for different reasons
//! from each other.
//!
//! What the job names BESIDE its script — the Glue connections it uses, the mode it runs in,
//! the command and the capacity form it is sized by — is not a reason for a run to fail: the
//! rows of the first table differ along exactly those members, and each one's run still
//! executes the script and succeeds. The connections the rows name are created through
//! `CreateConnection` and read back through `GetConnection`, so a row's "the connection
//! resolves" precondition is grounded in state the service really holds.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness; the scripts are
//! staged through Amazon S3's own `PutObject` and the IAM records the role resolution asks
//! about are seeded into the SAME shared engine the service reads them from. The due instants
//! the lifecycle turns on are moved THROUGH THE STORE rather than slept for in the tables, and
//! every run is read back until it HAS come to rest, so no outcome here depends on how fast
//! the host runs. The one test that leaves those instants where the service put them polls for
//! that same condition against a generous deadline and asserts nothing about elapsed time:
//! what it pins is that the service's own reported `ExecutionTime` is not zero for a run whose
//! lifecycle really spanned its execution.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{
    JOB_RUN_SCRIPT_UNREADABLE_LEAD, JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{glue_trust, seed_role, trust};
use crate::job_run_execution_role::{
    emulator_with_iam_and_s3, every_object, grant_role, statement, JOB_EXECUTOR,
    JOB_EXECUTOR_CODE, JOB_EXECUTOR_STATUS, SCRIPT_FILE_DOESNT_EXIST,
};
use crate::job_runs::{run_at_rest, run_at_rest_on_the_services_own_clock};
use crate::job_script_execution::{create_bucket, object_body, stage_script, BUCKET, SCRIPTS};
use crate::wire::{ok, ACCOUNT_ID};

/// The script a row stages: it writes a token object at the key its arguments name, carrying
/// the connection its arguments named — so a row can tell a run that EXECUTED, with its own
/// arguments on the script's command line, from one that never reached the interpreter.
const WRITES_A_TOKEN_FOR_ITS_CONNECTION: &str = r#"
import sys

import boto3

named = dict(zip(sys.argv[1::2], sys.argv[2::2]))
boto3.client("s3").put_object(
    Bucket=named["--bucket"],
    Key=named["--key"],
    Body=("ran for %s" % named["--connection"]).encode("utf-8"),
)
"#;

/// A script that raises, which is what a job whose own work failed does.
const RAISES: &str = r#"
raise ValueError("the input partition is malformed")
"#;

/// The whole reason a run of [`RAISES`] carries: the exception's own type and message.
const RAISED_REASON: &str = "ValueError: the input partition is malformed";

/// The grants a role needs to read the script and write what the script writes.
///
/// `s3:ListBucket` is among them because it is what decides WHICH refusal the object store
/// answers for an object it does not hold: "if you have the `s3:ListBucket` permission on the
/// bucket, Amazon S3 returns an HTTP status code 404 Not Found error. If you don't have the
/// `s3:ListBucket` permission, Amazon S3 returns an HTTP status code 403 Access Denied error\"
/// (`GetObject`'s own documentation) — so a role holding it hears about a script that is
/// absent, and a role without it hears only that the read was refused.
fn reads_the_script_and_writes_its_output() -> Vec<Value> {
    vec![
        statement(
            "Allow",
            &["s3:GetObject", "s3:PutObject"],
            &every_object(),
        ),
        statement("Allow", &["s3:ListBucket"], &format!("arn:aws:s3:::{BUCKET}")),
    ]
}

/// One `ConnectionInput` a row creates: its name, its `ConnectionType` and the property map
/// that type carries.
struct Connection {
    name: String,
    connection_type: &'static str,
    input: Value,
}

/// A JDBC connection, the form a job over a relational source names.
fn jdbc(name: String) -> Connection {
    Connection {
        input: json!({
            "Name": name,
            "ConnectionType": "JDBC",
            "ConnectionProperties": {
                "JDBC_CONNECTION_URL": "jdbc:postgresql://db.internal:5432/warehouse",
                "USERNAME": "etl",
            },
        }),
        connection_type: "JDBC",
        name,
    }
}

/// A Kafka connection, the form a job over a stream names.
fn kafka(name: String) -> Connection {
    Connection {
        input: json!({
            "Name": name,
            "ConnectionType": "KAFKA",
            "ConnectionProperties": { "KAFKA_BOOTSTRAP_SERVERS": "broker.internal:9094" },
        }),
        connection_type: "KAFKA",
        name,
    }
}

/// A connection carrying per-compute-environment property maps, which is the other schema
/// version a stored connection has.
fn salesforce(name: String) -> Connection {
    Connection {
        input: json!({
            "Name": name,
            "ConnectionType": "SALESFORCE",
            "ConnectionProperties": { "INSTANCE_URL": "https://example.my.salesforce.com" },
            "SparkProperties": { "INSTANCE_URL": "https://spark.my.salesforce.com" },
            "PythonProperties": { "API_VERSION": "60.0" },
        }),
        connection_type: "SALESFORCE",
        name,
    }
}

/// Create each of `connections` and read it back, so what a row calls a resolvable connection
/// is one the service really holds.
async fn create_connections(router: &MultiServiceRouter, case: &str, connections: &[Connection]) {
    for connection in connections {
        ok(
            router,
            "CreateConnection",
            &json!({ "ConnectionInput": connection.input }),
        )
        .await;
        let read = ok(router, "GetConnection", &json!({ "Name": connection.name })).await;
        assert_eq!(
            read["Connection"]["ConnectionType"].as_str(),
            Some(connection.connection_type),
            "case {case:?}: the connection {:?} must read back as a {} connection, got {read}",
            connection.name,
            connection.connection_type
        );
    }
}

/// The `CreateJob` body for a job that runs `location` as `role` using `connections`, with
/// `extra`'s members over it.
fn job_body(
    name: &str,
    role: &str,
    location: Option<&str>,
    connections: &[Connection],
    extra: &Value,
) -> Value {
    let mut command = json!({ "Name": "pythonshell", "PythonVersion": "3.9" });
    if let Some(location) = location {
        command["ScriptLocation"] = json!(location);
    }
    // A row naming another command states only what differs, so the script it staged stays on
    // the command it runs.
    if let Some(members) = extra.get("Command").and_then(Value::as_object) {
        for (member, value) in members {
            command[member] = value.clone();
        }
    }
    let mut body = json!({
        "Name": name,
        "Role": role,
        "Command": command,
        "MaxCapacity": 0.0625,
        "Timeout": 5,
    });
    if !connections.is_empty() {
        let names: Vec<&str> = connections
            .iter()
            .map(|connection| connection.name.as_str())
            .collect();
        body["Connections"] = json!({ "Connections": names });
    }
    for (member, value) in extra.as_object().into_iter().flatten() {
        if member == "Command" {
            continue;
        }
        if value.is_null() {
            // A row sized another way DROPS the shared member rather than contradicting it:
            // `MaxCapacity` and the worker members name the same capacity two ways.
            if let Some(members) = body.as_object_mut() {
                members.remove(member);
            }
        } else {
            body[member] = value.clone();
        }
    }
    body
}

/// The arguments a row's run passes its script: where to write its token, and the connection
/// the job names — `none` for a row that names no connection, so the script reads an argument
/// either way.
fn run_arguments(token_key: &str, connections: &[Connection]) -> Value {
    let connection = connections
        .first()
        .map_or("none", |connection| connection.name.as_str());
    json!({
        "--bucket": BUCKET,
        "--key": token_key,
        "--connection": connection,
    })
}

/// The number `run` reports for `member`, which every instant and span of a finished run has.
fn reported_number(case: &str, run: &Value, member: &str) -> f64 {
    run.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: a finished run reports {member}, got {run}"))
}

/// Assert that `run` reports the seconds it consumed as the span between the instants it
/// reports: `JobRun$ExecutionTime` is "the amount of time (in seconds) that the job run
/// consumed resources", so it is the whole seconds from `StartedOn` to `CompletedOn`.
///
/// The two instants reach the wire as fractional epoch seconds, so the whole-second count read
/// off them can fall either side of a second boundary by one; what is asserted is that the
/// reported figure is that span and not a figure of its own.
fn assert_execution_time_spans_the_run(case: &str, run: &Value) {
    let started = reported_number(case, run, "StartedOn");
    let completed = reported_number(case, run, "CompletedOn");
    assert!(
        completed >= started,
        "case {case:?}: a run cannot finish before it started, got {run}"
    );
    let elapsed = completed - started;
    let reported = reported_number(case, run, "ExecutionTime");
    assert!(
        (reported - elapsed).abs() <= 1.0,
        "case {case:?}: expected ExecutionTime to be the {elapsed} seconds between StartedOn \
         and CompletedOn, got {reported}"
    );
}

/// One row of the positive-control table: what the submission names beside its script.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The connections the job uses, which the row creates first.
    connections: Vec<Connection>,
    /// The members the row's `CreateJob` body carries over the shared ones.
    extra: Value,
}

/// The rows: every shape of submission whose script resolves and whose role Glue can assume.
///
/// They differ along what the rule generalises over — how many connections the job uses and of
/// which types, whether it names any at all, the `JobMode` it states, the command it runs and
/// the capacity form it is sized by — and the recorded case (a `SCRIPT`-mode job over one
/// connection, sized by `MaxCapacity`) is the first of them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a SCRIPT-mode job using one JDBC connection",
            connections: vec![jdbc("positive-control-jdbc".to_string())],
            extra: json!({ "JobMode": "SCRIPT" }),
        },
        Row {
            case: "a job using no connection at all",
            connections: Vec::new(),
            extra: json!({}),
        },
        Row {
            case: "a job using two connections of different types",
            connections: vec![
                jdbc("positive-control-jdbc-2".to_string()),
                kafka("positive-control-kafka".to_string()),
            ],
            extra: json!({}),
        },
        Row {
            case: "a job using a connection that carries per-environment properties",
            connections: vec![salesforce("positive-control-salesforce".to_string())],
            extra: json!({}),
        },
        Row {
            case: "a Spark ETL job over one connection, sized by workers",
            connections: vec![jdbc("positive-control-jdbc-3".to_string())],
            // `MaxCapacity` and the worker members name the same capacity two ways, so the
            // shared `MaxCapacity` is taken off this row rather than sent beside them.
            extra: json!({
                "Command": { "Name": "glueetl" },
                "MaxCapacity": Value::Null,
                "WorkerType": "G.1X",
                "NumberOfWorkers": 2,
            }),
        },
    ]
}

/// A submission whose role Glue can assume and whose script resolves executes that script and
/// comes to rest in `SUCCEEDED` carrying no `ErrorMessage`, whatever else it names.
#[tokio::test]
async fn a_submission_with_nothing_wrong_with_it_runs_and_succeeds() {
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let role_name = format!("positive-control-role-{index}");
        let role = seed_role(&engine, &role_name, Some(glue_trust()));
        grant_role(
            &engine,
            &role_name,
            &reads_the_script_and_writes_its_output(),
            &[],
        );
        create_connections(&router, case, &row.connections).await;

        let job_name = format!("positive_control_{index}");
        let script_key = format!("{SCRIPTS}{job_name}.py");
        let token_key = format!("tokens/{job_name}.txt");
        let location = stage_script(&router, &script_key, WRITES_A_TOKEN_FOR_ITS_CONNECTION).await;
        ok(
            &router,
            "CreateJob",
            &job_body(
                &job_name,
                &role,
                Some(&location),
                &row.connections,
                &row.extra,
            ),
        )
        .await;

        let started = ok(
            &router,
            "StartJobRun",
            &json!({
                "JobName": job_name,
                "Arguments": run_arguments(&token_key, &row.connections),
            }),
        )
        .await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
            .to_string();

        let run = run_at_rest(&router, &engine, case, &job_name, &run_id).await;
        assert_eq!(
            run["JobRunState"].as_str(),
            Some(JOB_RUN_STATE_SUCCEEDED),
            "case {case:?}: expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
        );
        assert!(
            run.get("ErrorMessage").is_none(),
            "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
        );
        assert_execution_time_spans_the_run(case, &run);

        // The script's own effect, carrying the arguments the run put on its command line: the
        // run executed, rather than being reported as having run.
        let expected = format!(
            "ran for {}",
            row.connections
                .first()
                .map_or("none", |connection| connection.name.as_str())
        );
        let token = object_body(&router, &token_key).await;
        assert_eq!(
            token.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the script's token object {expected:?}, got {token:?}"
        );
    }
}

/// What one row of the failure table does to its submission, and what the run must then carry.
enum Fault {
    /// Nothing: the submission is the positive control, so the run succeeds.
    None,
    /// The job's role is one Glue cannot assume.
    RoleGlueCannotAssume,
    /// The role may not read the object the job's script location names.
    ScriptUnreadableToTheRole,
    /// Nothing is staged at that location.
    ScriptAbsent,
    /// The script itself raises.
    ScriptRaises,
}

/// One started submission: what it was made of, and the run it started.
struct Submission {
    /// The job definition the run is a run of.
    job_name: String,
    /// The run `StartJobRun` minted.
    run_id: String,
    /// The ARN of the role the job carries, which the assume-role requirement names.
    role: String,
    /// The `s3://` location the job's `Command` points at.
    location: String,
    /// The key the submission's script writes its token under.
    token_key: String,
}

/// Stage one submission — its role, its grants, its connection, its script and its job — and
/// start a run of it.
///
/// `fault` is the ONE thing wrong with the submission, so every row differs from the positive
/// control in exactly one way.
async fn start_a_submission(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    index: usize,
    fault: &Fault,
) -> Submission {
    let role_name = format!("failure-class-role-{index}");
    let trust_document = match fault {
        // A role trusting another service is one IAM answers that Glue cannot assume.
        Fault::RoleGlueCannotAssume => trust("Allow", "ec2.amazonaws.com", "sts:AssumeRole"),
        _ => glue_trust(),
    };
    let role = seed_role(engine, &role_name, Some(trust_document));
    let grants = match fault {
        Fault::ScriptUnreadableToTheRole => Vec::new(),
        _ => reads_the_script_and_writes_its_output(),
    };
    grant_role(engine, &role_name, &grants, &[]);

    let connections = vec![jdbc(format!("failure-class-connection-{index}"))];
    create_connections(router, case, &connections).await;

    let job_name = format!("failure_class_{index}");
    let script_key = format!("{SCRIPTS}{job_name}.py");
    let token_key = format!("tokens/{job_name}.txt");
    let source = match fault {
        Fault::ScriptRaises => RAISES,
        _ => WRITES_A_TOKEN_FOR_ITS_CONNECTION,
    };
    let location = match fault {
        // Nothing is staged, so the location names an object the store does not serve.
        Fault::ScriptAbsent => format!("s3://{BUCKET}/{script_key}"),
        _ => stage_script(router, &script_key, source).await,
    };
    ok(
        router,
        "CreateJob",
        &job_body(
            &job_name,
            &role,
            Some(&location),
            &connections,
            &json!({ "JobMode": "SCRIPT" }),
        ),
    )
    .await;

    let started = ok(
        router,
        "StartJobRun",
        &json!({
            "JobName": job_name,
            "Arguments": run_arguments(&token_key, &connections),
        }),
    )
    .await;
    let run_id = started["JobRunId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
        .to_string();
    Submission {
        job_name,
        run_id,
        role,
        location,
        token_key,
    }
}

/// A run carries the message of the failure class it ran into, and no two classes state the
/// same one — so a caller reading a run can tell which of them it was, and tell all of them
/// from a run that succeeded.
#[tokio::test]
async fn runs_that_fail_for_different_reasons_carry_their_own_message() {
    // (case, the one thing wrong with the submission)
    let rows: Vec<(&str, Fault)> = vec![
        ("nothing wrong with the submission", Fault::None),
        ("a role Glue cannot assume", Fault::RoleGlueCannotAssume),
        (
            "a role that may not read the script",
            Fault::ScriptUnreadableToTheRole,
        ),
        (
            "a script the object store does not serve",
            Fault::ScriptAbsent,
        ),
        ("a script that raises", Fault::ScriptRaises),
    ];

    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;
    let mut reasons: Vec<(&str, String)> = Vec::new();

    for (index, (case, fault)) in rows.iter().enumerate() {
        let submission = start_a_submission(&router, &engine, case, index, fault).await;
        let run = run_at_rest(
            &router,
            &engine,
            case,
            &submission.job_name,
            &submission.run_id,
        )
        .await;
        let state = run["JobRunState"].as_str().unwrap_or_default();
        let message = run["ErrorMessage"].as_str().unwrap_or_default().to_string();
        let token = object_body(&router, &submission.token_key).await;

        if matches!(fault, Fault::None) {
            assert_eq!(
                state, JOB_RUN_STATE_SUCCEEDED,
                "case {case:?}: expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
            );
            assert!(
                run.get("ErrorMessage").is_none(),
                "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
            );
            assert!(
                token.is_some(),
                "case {case:?}: the script must have written its object, got {token:?}"
            );
            continue;
        }

        assert_eq!(
            state, JOB_RUN_STATE_FAILED,
            "case {case:?}: expected {JOB_RUN_STATE_FAILED}, got {run}"
        );
        match fault {
            Fault::RoleGlueCannotAssume => {
                // The job executor's own line for a run it could not execute: the run it is
                // about — the account, the job and the run id — then the reason that stopped
                // it, then the clause naming the component, status and code it was raised
                // with. The clause's request id is its one per-run field, so it is stripped
                // and asserted by shape; the rest is fixed text or read off the run.
                let sentence = crate::error_classification::without_diagnostic(
                    case,
                    &message,
                    JOB_EXECUTOR,
                    JOB_EXECUTOR_STATUS,
                    JOB_EXECUTOR_CODE,
                );
                let expected = format!(
                    "AccountId:{ACCOUNT_ID} and JobName:{} and JobRunId:{} failed to execute \
                     with exception Role {} should be given assume role permissions for Glue \
                     Service.\n",
                    submission.job_name, submission.run_id, submission.role
                );
                assert_eq!(
                    sentence, expected,
                    "case {case:?}: expected ErrorMessage {expected:?}, got {message:?}"
                );
            }
            Fault::ScriptUnreadableToTheRole => {
                let expected = format!("{SCRIPT_FILE_DOESNT_EXIST}: {}", submission.location);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected ErrorMessage {expected:?}, got {message:?}"
                );
            }
            Fault::ScriptAbsent => assert!(
                message.starts_with(JOB_RUN_SCRIPT_UNREADABLE_LEAD),
                "case {case:?}: expected an ErrorMessage beginning \
                 {JOB_RUN_SCRIPT_UNREADABLE_LEAD:?}, got {message:?}"
            ),
            Fault::ScriptRaises => assert_eq!(
                message, RAISED_REASON,
                "case {case:?}: expected ErrorMessage {RAISED_REASON:?}, got {message:?}"
            ),
            Fault::None => unreachable!("a submission with no fault was handled above"),
        }
        assert!(
            token.is_none(),
            "case {case:?}: nothing of the script may have run, but its object is {token:?}"
        );
        reasons.push((case, message));
    }

    // The messages are what tells the classes apart, so no two of them may be the same
    // sentence: a run that failed for one reason must not read like a run that failed for
    // another.
    for (first, (one_case, one)) in reasons.iter().enumerate() {
        for (other_case, other) in reasons.iter().skip(first + 1) {
            assert_ne!(
                one, other,
                "case {one_case:?} and case {other_case:?}: two failure classes must not state \
                 the same ErrorMessage, and both state {one:?}"
            );
        }
    }
}

/// A run whose lifecycle ran on the service's own clock reports the seconds it consumed: the
/// run passes through its states over the spans the service owes them, so `ExecutionTime` is
/// above zero and spans the run rather than reporting a run that finished the instant it
/// started.
#[tokio::test]
async fn a_run_reports_the_time_its_own_lifecycle_took() {
    let case = "a SCRIPT-mode job using one JDBC connection, on the service's own clock";
    let (router, engine) = emulator_with_iam_and_s3();
    create_bucket(&router).await;

    let submission = start_a_submission(&router, &engine, case, 0, &Fault::None).await;
    let run = run_at_rest_on_the_services_own_clock(
        &router,
        case,
        &submission.job_name,
        &submission.run_id,
    )
    .await;

    assert_eq!(
        run["JobRunState"].as_str(),
        Some(JOB_RUN_STATE_SUCCEEDED),
        "case {case:?}: expected {JOB_RUN_STATE_SUCCEEDED}, got {run}"
    );
    assert!(
        run.get("ErrorMessage").is_none(),
        "case {case:?}: a run that succeeded carries no ErrorMessage, got {run}"
    );
    assert_execution_time_spans_the_run(case, &run);
    let reported = reported_number(case, &run, "ExecutionTime");
    assert!(
        reported > 0.0,
        "case {case:?}: a run that passed through its lifecycle consumed time, got \
         ExecutionTime {reported} in {run}"
    );
    let token = object_body(&router, &submission.token_key).await;
    assert!(
        token.is_some(),
        "case {case:?}: the script must have written its object, got {token:?}"
    );
}
