// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the REFERENCES a Glue job definition carries: the name of the
//! command that runs it, the script that command points at, and the IAM role it runs as.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a job definition's references are STORED AS SUBMITTED and resolved
//! only when a run of the job is started, so a write of a definition refuses neither the
//! command name it carries nor the paths it points at. `JobCommand$Name` is an OPTIONAL
//! `GenericString` with no enum, length or pattern (`smithy_query shape=JobCommand`), so
//! every string — and no string at all — is a command name a definition may carry;
//! `JobCommand$ScriptLocation` is a path ("Specifies the Amazon Simple Storage Service
//! (Amazon S3) path to a script that runs a job") and `Role` is "the name or Amazon Resource
//! Name (ARN) of the IAM role associated with this job", neither of which has to resolve for
//! the definition to be stored. The names `JobCommand$Name` documents per job kind ("For an
//! Apache Spark ETL job, this must be `glueetl`. For a Python shell job, it must be
//! `pythonshell` … `gluestreaming` … `glueray`") tell a caller which name to use; they only
//! pick the defaults the service fills in, they do not close the set a write admits. What a
//! NAMED command does have to carry is a script for it to run, which is
//! [`crate::job_definition_member_refusals`]'s rule rather than this one's.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the command
//! name (one of the documented four, another casing of one, a vendor name outside them, one
//! carrying spaces and punctuation, and none at all), the script the command points at,
//! whether the role is an ARN or a bare name, and which write submitted the definition
//! (`CreateJob` or the `UpdateJob` that overwrites it).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, ok};

/// A role ARN naming a role no IAM in this emulator holds: a reference a job definition
/// stores without resolving it.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-job-role";

/// The same reference in the other form the member documents ("the name or Amazon Resource
/// Name (ARN) of the IAM role"): a bare role name.
const ROLE_NAME: &str = "sim2real-glue-job-role";

/// A script location pointing at an object no bucket in this emulator holds, which a create
/// stores without reading.
const SCRIPT_LOCATION: &str = "s3://sim2real-glue-scripts/jobs/probe.py";

/// One row: a `Command` structure a write may submit, under the axis it varies.
struct CommandRow {
    case: &'static str,
    command: Value,
}

/// The command structures the rule spans. The documented names are one row each; the rest
/// are names the documentation does not list, which the member's type admits all the same.
fn command_rows() -> Vec<CommandRow> {
    vec![
        CommandRow {
            // The recorded case: a name outside the documented four.
            case: "a command named for the engine it runs on",
            command: json!({ "Name": "spark", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "the documented Apache Spark ETL command",
            command: json!({ "Name": "glueetl", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "the documented Python shell command",
            command: json!({ "Name": "pythonshell", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "the documented Apache Spark streaming command",
            command: json!({ "Name": "gluestreaming", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "the documented Ray command, with the runtime it names its versions by",
            command: json!({
                "Name": "glueray",
                "ScriptLocation": SCRIPT_LOCATION,
                "Runtime": "Ray2.4",
            }),
        },
        CommandRow {
            // `JobCommand$Name` carries no documented case fold, so another casing is
            // another name — and it is stored as sent rather than folded or refused.
            case: "a documented name in another casing",
            command: json!({ "Name": "GlueETL", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "a vendor command name the documentation does not list",
            command: json!({ "Name": "sparkmagic", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            case: "a command name carrying spaces and punctuation",
            command: json!({ "Name": "spark 3.5 (preview)", "ScriptLocation": SCRIPT_LOCATION }),
        },
        CommandRow {
            // The member is optional, so a command may name no kind at all.
            case: "a command with no name at all",
            command: json!({ "ScriptLocation": SCRIPT_LOCATION }),
        },
    ]
}

/// The `Job` `GetJob` reports for `name`.
async fn get_job(router: &MultiServiceRouter, name: &str) -> Value {
    let body = ok(router, "GetJob", &json!({ "JobName": name })).await;
    body.get("Job")
        .cloned()
        .unwrap_or_else(|| panic!("GetJob must report a Job, got {body}"))
}

/// Assert that the definition stored under `name` carries exactly the references
/// `command` and `role` submitted.
async fn assert_references_stored(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    command: &Value,
    role: &str,
) {
    let job = get_job(router, name).await;
    let stored = job
        .get("Command")
        .unwrap_or_else(|| panic!("case {case:?}: a read of a job reports its Command, got {job}"));
    assert_eq!(
        stored.get("Name"),
        command.get("Name"),
        "case {case:?}: expected the command name {:?} the write submitted, got {stored}",
        command.get("Name")
    );
    assert_eq!(
        stored.get("ScriptLocation"),
        command.get("ScriptLocation"),
        "case {case:?}: expected the script location {:?} the write submitted, got {stored}",
        command.get("ScriptLocation")
    );
    assert_eq!(
        stored.get("Runtime"),
        command.get("Runtime"),
        "case {case:?}: expected the runtime {:?} the write submitted, got {stored}",
        command.get("Runtime")
    );
    assert_eq!(
        job.get("Role").and_then(Value::as_str),
        Some(role),
        "case {case:?}: expected the role {role:?} the write submitted, got {job}"
    );
}

/// `CreateJob` serves a definition whatever its command is named, and `GetJob` reports the
/// command and the role back exactly as they were submitted.
#[tokio::test]
async fn a_create_stores_the_command_it_was_given() {
    let router = emulator();

    for (index, row) in command_rows().into_iter().enumerate() {
        // The role form alternates across the rows, so neither form is the only one tested.
        let role = if index % 2 == 0 { ROLE_ARN } else { ROLE_NAME };
        let name = format!("command-row-{index}");
        let body = json!({ "Name": name, "Role": role, "Command": row.command });

        let (status, answer) = call(&router, "CreateJob", &body).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status} with {answer}",
            row.case
        );
        assert_eq!(
            answer.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {:?}: expected the create to report the job's name, got {answer}",
            row.case
        );

        assert_references_stored(&router, row.case, &name, &row.command, role).await;
    }
}

/// The same rule for the write that OVERWRITES a definition: `UpdateJob` stores the command
/// it was given, and the read reports it.
#[tokio::test]
async fn an_update_stores_the_command_it_was_given() {
    let router = emulator();
    let name = "updated-command";
    ok(
        &router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": ROLE_ARN,
            "Command": { "Name": "glueetl", "ScriptLocation": SCRIPT_LOCATION },
        }),
    )
    .await;

    for row in command_rows() {
        let update = json!({ "Role": ROLE_NAME, "Command": row.command });
        let (status, answer) = call(
            &router,
            "UpdateJob",
            &json!({ "JobName": name, "JobUpdate": update }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200, got {status} with {answer}",
            row.case
        );
        assert_eq!(
            answer.get("JobName").and_then(Value::as_str),
            Some(name),
            "case {:?}: expected the update to report the job's name, got {answer}",
            row.case
        );

        assert_references_stored(&router, row.case, name, &row.command, ROLE_NAME).await;
    }
}

/// A definition whose command this service has no documented default capacity for is still
/// sized by the worker configuration the request named: the capacity members a read reports
/// follow from `WorkerType` and `NumberOfWorkers`, not from the command's name.
///
/// The rows differ along the worker type and count, each with the DPUs
/// `CreateJob$WorkerType` documents for one worker of that type ("For the `G.1X` worker
/// type, each worker maps to 1 DPU", "… `G.2X` … 2 DPU", "… `G.025X` … 0.25 DPU"); the
/// recorded case is two `G.1X` workers.
#[tokio::test]
async fn a_worker_configuration_sizes_a_job_whatever_its_command_is_named() {
    let router = emulator();

    // (case, command name, worker type, worker count, the DPUs that pair amounts to). Every
    // count is at or above the floor a definition's worker configuration has
    // (`crate::job_capacity_refusal`), which is not what this rule is about.
    let rows: Vec<(&str, &str, &str, i64, f64)> = vec![
        ("the recorded two one-DPU workers", "spark", "G.1X", 2, 2.0),
        ("four two-DPU workers", "spark", "G.2X", 4, 8.0),
        ("two quarter-DPU workers", "sparkmagic", "G.025X", 2, 0.5),
        ("ten one-DPU workers on a documented command", "glueetl", "G.1X", 10, 10.0),
    ];

    for (index, (case, command_name, worker_type, workers, dpus)) in rows.into_iter().enumerate() {
        let name = format!("sized-{index}");
        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": name,
                "Role": ROLE_ARN,
                "Command": { "Name": command_name, "ScriptLocation": SCRIPT_LOCATION },
                "GlueVersion": "4.0",
                "JobMode": "SCRIPT",
                "WorkerType": worker_type,
                "NumberOfWorkers": workers,
            }),
        )
        .await;

        let job = get_job(&router, &name).await;
        assert_eq!(
            job.get("WorkerType").and_then(Value::as_str),
            Some(worker_type),
            "case {case:?}: expected the worker type the create named, got {job}"
        );
        assert_eq!(
            job.get("NumberOfWorkers").and_then(Value::as_i64),
            Some(workers),
            "case {case:?}: expected the worker count the create named, got {job}"
        );
        assert_eq!(
            job.get("MaxCapacity").and_then(Value::as_f64),
            Some(dpus),
            "case {case:?}: expected {dpus} DPUs for {workers} {worker_type} workers, got {job}"
        );
        // "This field is deprecated. Use MaxCapacity instead": the same quantity in whole
        // DPUs.
        assert_eq!(
            job.get("AllocatedCapacity").and_then(Value::as_i64),
            Some(dpus as i64),
            "case {case:?}: expected the deprecated capacity to report the same DPUs, got {job}"
        );
        assert_eq!(
            job.get("GlueVersion").and_then(Value::as_str),
            Some("4.0"),
            "case {case:?}: expected the Glue version the create named, got {job}"
        );
    }
}

/// A run of a job whose command and script this service cannot resolve is still RECORDED and
/// read back: the references are resolved when the run starts, not when the definition is
/// written, so the job surface stays reachable.
#[tokio::test]
async fn a_run_of_such_a_job_is_recorded_and_read_back() {
    let router = emulator();
    let name = "started-command";
    ok(
        &router,
        "CreateJob",
        &json!({
            "Name": name,
            "Role": ROLE_ARN,
            "Command": { "Name": "spark", "ScriptLocation": SCRIPT_LOCATION },
            "GlueVersion": "4.0",
            "WorkerType": "G.1X",
            "NumberOfWorkers": 2,
        }),
    )
    .await;

    let started = ok(&router, "StartJobRun", &json!({ "JobName": name, "Timeout": 5 })).await;
    let run_id = started
        .get("JobRunId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("StartJobRun must report the run it recorded, got {started}"))
        .to_string();

    let answer = ok(
        &router,
        "GetJobRun",
        &json!({ "JobName": name, "RunId": run_id }),
    )
    .await;
    let run = answer
        .get("JobRun")
        .unwrap_or_else(|| panic!("GetJobRun must report a JobRun, got {answer}"));
    assert_eq!(
        run.get("Id").and_then(Value::as_str),
        Some(run_id.as_str()),
        "expected the run that was started, got {run}"
    );
    assert_eq!(
        run.get("JobName").and_then(Value::as_str),
        Some(name),
        "expected the run to name the job it is a run of, got {run}"
    );
    // The run carries the job's own worker configuration, and the timeout the start
    // overrode ("This value overrides the timeout value set in the parent job").
    assert_eq!(
        run.get("WorkerType").and_then(Value::as_str),
        Some("G.1X"),
        "expected the run to carry the job's worker type, got {run}"
    );
    assert_eq!(
        run.get("Timeout").and_then(Value::as_i64),
        Some(5),
        "expected the run to carry the timeout the start named, got {run}"
    );
}
