// SPDX-License-Identifier: CC0-1.0

//! The PARAMETER SUBSTITUTION a job's container is run with, end to end over the wire.
//!
//! RULE under test: `RegisterJobDefinition$parameters` is "default parameter substitution
//! placeholders to set in the job definition" and `SubmitJob$parameters` is "additional
//! parameters passed to the job that replace parameter substitution placeholders that are set
//! in the job definition … Parameters in a `SubmitJob` request override any corresponding
//! parameter defaults from the job definition"
//! (`smithy_query(query="doc", shape="SubmitJobRequest", member="parameters")`). So a
//! placeholder — `Ref::` followed by the parameter's name — standing in the words of the
//! container's `command` or in the values of its `environment` is replaced, where the job is
//! RUN, by that job's effective parameter: the submit's value over the definition's default.
//! What the job REPORTS is unchanged by that (`JobDetail$parameters`, `container.command` and
//! `container.environment` are replayed as the requests stated them), and what the container
//! then does is the program with those values in it — which its
//! `ContainerDetail$exitCode` ("the exit code returned upon completion") and so the job's own
//! terminal `JobStatus` follow from.
//!
//! The rows differ along what the rule generalises over: WHERE the placeholder stands (a word
//! of the command, part of a longer word, the value of an environment variable, several
//! placeholders in one program), WHICH side decides the value (the definition's default, the
//! submit's override, a definition default with no override), and whether the container's
//! command came from the definition or from `containerOverrides` — "the command to send to the
//! container that overrides the default command from the Docker image or the job definition"
//! (`smithy_query(query="shape", shape="ContainerOverrides")`).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::state;

use crate::array_jobs_and_placement::managed_compute_environment;
use crate::job_lifecycle::{describe_job, Batch, job_queue};

/// `JobStatus` members this suite names. Source:
/// `smithy_query(query="shape", shape="JobStatus")`.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";

/// How long one job is given to settle, polled for the CONDITION rather than waited out.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How long one look waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One row: the definition's parameter defaults and command, what the submit overrides, and
/// the exit status the substituted program must then end with.
struct Row {
    case: &'static str,
    /// `RegisterJobDefinition$parameters`.
    defaults: Value,
    /// The `sh -c` text the definition's container declares.
    program: &'static str,
    /// The definition's container `environment`.
    environment: Value,
    /// `SubmitJob$parameters`, or `null` for a submit that states none.
    overrides: Value,
    /// `SubmitJob$containerOverrides`, or `null` for a submit that states none.
    container_overrides: Value,
    /// The status the substituted program exits with.
    exit: i64,
}

fn rows() -> Vec<Row> {
    vec![
    Row {
        case: "a definition default fills the placeholder the command carries",
        defaults: json!({"code": "0"}),
        program: "exit Ref::code",
        environment: json!([]),
        overrides: json!(null),
        container_overrides: json!(null),
        exit: 0,
    },
    Row {
        case: "the submit's parameter overrides that default",
        defaults: json!({"code": "0"}),
        program: "exit Ref::code",
        environment: json!([]),
        overrides: json!({"code": "6"}),
        container_overrides: json!(null),
        exit: 6,
    },
    Row {
        case: "a placeholder inside a longer word, and one the program writes",
        defaults: json!({"code": "0", "message": "greeting"}),
        program: "echo msg=Ref::message; exit Ref::code",
        environment: json!([]),
        overrides: json!(null),
        container_overrides: json!(null),
        exit: 0,
    },
    Row {
        case: "a placeholder in the value of an environment variable",
        defaults: json!({"code": "4"}),
        program: "exit $DECLARED_CODE",
        environment: json!([{"name": "DECLARED_CODE", "value": "Ref::code"}]),
        overrides: json!(null),
        container_overrides: json!(null),
        exit: 4,
    },
    Row {
        case: "the submit's parameter reaches an environment placeholder too",
        defaults: json!({"code": "4"}),
        program: "exit $DECLARED_CODE",
        environment: json!([{"name": "DECLARED_CODE", "value": "Ref::code"}]),
        overrides: json!({"code": "9"}),
        container_overrides: json!(null),
        exit: 9,
    },
    Row {
        case: "several placeholders decide one program",
        defaults: json!({"first": "1", "second": "2", "code": "3"}),
        program: "echo Ref::first Ref::second; exit Ref::code",
        environment: json!([]),
        overrides: json!({"code": "5"}),
        container_overrides: json!(null),
        exit: 5,
    },
    Row {
        case: "the command containerOverrides sent is the one substituted into",
        defaults: json!({"code": "7"}),
        program: "exit 1",
        environment: json!([]),
        overrides: json!(null),
        container_overrides: json!({"command": ["/bin/sh", "-c", "exit Ref::code"]}),
        exit: 7,
    },
    Row {
        case: "a placeholder no parameter names is left as the definition wrote it",
        defaults: json!({"code": "0"}),
        program: "echo Ref::absent; exit Ref::code",
        environment: json!([]),
        overrides: json!(null),
        container_overrides: json!(null),
        exit: 0,
    },
    ]
}

/// Look again until `job_id` reports a terminal `JobStatus`, and answer its detail.
async fn until_terminal(batch: &Batch, case: &str, job_id: &str) -> Value {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let detail = describe_job(batch, job_id).await;
        let status = detail["status"].as_str().unwrap_or_default();
        if status == SUCCEEDED || status == FAILED {
            return detail;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: expected a terminal status inside {SETTLES_WITHIN:?}, last {detail}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the container a job RUNS is its declared container with the job's effective
/// parameters substituted into the words of its command and the values of its environment —
/// so the exit status the job reports is the one that substituted program ends with — while
/// the job replays the command, the environment and the `parameters` as the requests stated
/// them.
#[tokio::test]
async fn a_job_runs_the_program_its_effective_parameters_fill() {
    let batch = Batch::new();
    let environment =
        managed_compute_environment(&batch, "ce-parameters", state::STATE_ENABLED).await;
    let queue_arn = job_queue(&batch, "q-parameters", &environment).await;

    let rows = rows();
    let mut submitted: Vec<(&Row, String)> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let name = format!("jd-parameters-{index}");
        batch
            .ok(
                "/v1/registerjobdefinition",
                json!({
                    "jobDefinitionName": &name,
                    "type": "container",
                    "parameters": row.defaults,
                    "containerProperties": {
                        "image": "busybox",
                        "command": ["/bin/sh", "-c", row.program],
                        "environment": row.environment,
                        "resourceRequirements": [
                            {"type": "VCPU", "value": "1"},
                            {"type": "MEMORY", "value": "512"},
                        ],
                    },
                }),
            )
            .await;
        let mut request = json!({
            "jobName": format!("parameters-{index}"),
            "jobQueue": &queue_arn,
            "jobDefinition": &name,
        });
        if !row.overrides.is_null() {
            request["parameters"] = row.overrides.clone();
        }
        if !row.container_overrides.is_null() {
            request["containerOverrides"] = row.container_overrides.clone();
        }
        let answered = batch.ok("/v1/submitjob", request).await;
        let job_id = answered["jobId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {}: expected a jobId in {answered}", row.case))
            .to_string();
        submitted.push((row, job_id));
    }

    for (row, job_id) in &submitted {
        let case = row.case;
        let detail = until_terminal(&batch, case, job_id).await;
        let wanted = if row.exit == 0 { SUCCEEDED } else { FAILED };
        assert_eq!(
            detail["container"]["exitCode"].as_i64(),
            Some(row.exit),
            "case {case}: expected the substituted program to exit {}, got {detail}",
            row.exit
        );
        assert_eq!(
            detail["status"].as_str(),
            Some(wanted),
            "case {case}: expected the job to end {wanted} on that exit status, got {detail}"
        );
        // The RECORD is unchanged by the substitution: a client reads the placeholders it
        // wrote and the parameters that fill them.
        let expected_program = row
            .container_overrides
            .get("command")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                vec![json!("/bin/sh"), json!("-c"), json!(row.program)]
            });
        assert_eq!(
            detail["container"]["command"],
            Value::Array(expected_program),
            "case {case}: expected the command to be replayed as it was stated, got {detail}"
        );
        let mut expected_parameters = row.defaults.as_object().cloned().unwrap_or_default();
        for (key, value) in row.overrides.as_object().cloned().unwrap_or_default() {
            expected_parameters.insert(key, value);
        }
        assert_eq!(
            detail["parameters"],
            Value::Object(expected_parameters),
            "case {case}: expected the effective parameters to be reported, got {detail}"
        );
    }
}
