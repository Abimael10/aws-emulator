// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of ONE rule about an optional echo member:
//!
//! > A job's optional `outputSourceConfig` reports the output-source override the START
//! > of that job carried, so it is emitted only when the start supplied one and omitted
//! > from the body entirely otherwise; it is never derived from the configuration of the
//! > workflow the job runs.
//!
//! `StartMatchingJob` takes `workflowName` and nothing else
//! (`smithy_query(query="operation",operation="StartMatchingJob")` — its input shape
//! carries no `outputSourceConfig`), so no matching job can carry an override and
//! `GetMatchingJob` never reports the member. Where a run writes stays a property of the
//! matching WORKFLOW, which is why every row here also reads the workflow back through
//! `GetMatchingWorkflow` and finds its own `outputSourceConfig` intact.
//!
//! The rows differ along exactly what the old derivation keyed on — how the workflow's
//! output source is configured: an S3 path alone, a path with a `KMSArn`, a deep path, a
//! bucket-only path, a path with hashed output attributes and normalization, and no path
//! at all (`OutputSource$outputS3Path` targets `OptionalS3Path`, whose default is the
//! empty string). The recorded request is ONE row of these. A final case updates the
//! workflow's output configuration after the job has settled, which under the rule
//! changes nothing about the job.
//!
//! Every read drives the real wire path through the harness the
//! `schema_mapping_workflow_lifecycle` suite owns, so what is asserted is what an AWS
//! SDK client would see.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::schema_mapping_workflow_lifecycle::{
    glue_table_arn, role_arn, string_member, Emulator, Reply, REGION,
};

/// The terminal `JobStatus` values (`smithy_query(query="shape",shape="JobStatus")`
/// gives `RUNNING | SUCCEEDED | FAILED | QUEUED`; these two are the ones nothing
/// further is owed from).
const TERMINAL: &[&str] = &["SUCCEEDED", "FAILED"];

/// How long the job lifecycle may take before the test gives up. Generous on purpose:
/// the assertion is about the BODY, never about how fast the host runs.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the job is polled while waiting for it to settle.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The schema mapping every row's `inputSourceConfig` references.
const SCHEMA_NAME: &str = "osc-schema";

/// A KMS ARN, as `OutputSource$KMSArn` models it (`^arn:aws:kms:.*:[0-9]+:.*$`).
fn kms_arn() -> String {
    format!("arn:aws:kms:{REGION}:000000000000:key/11111111-2222-3333-4444-555555555555")
}

/// One matching workflow, named by how its OUTPUT source is configured.
struct OutputCase {
    case: &'static str,
    workflow_name: &'static str,
    /// The workflow's `outputSourceConfig`, as the create request carries it. Itself
    /// `length 1..1` (`smithy_query(query="shape",shape="OutputSourceConfig")`).
    output_source_config: Value,
}

fn output_cases() -> Vec<OutputCase> {
    vec![
        OutputCase {
            case: "an S3 path and nothing else",
            workflow_name: "osc-plain",
            output_source_config: json!([{
                "outputS3Path": "s3://er-osc/plain",
                "output": [{ "name": "id" }],
            }]),
        },
        OutputCase {
            case: "an S3 path with a KMS key",
            workflow_name: "osc_kms_1",
            output_source_config: json!([{
                "outputS3Path": "s3://er-osc/kms",
                "KMSArn": kms_arn(),
                "output": [{ "name": "id" }, { "name": "email" }],
            }]),
        },
        OutputCase {
            case: "a deep S3 path",
            workflow_name: "OSCDEEP",
            output_source_config: json!([{
                "outputS3Path": "s3://er-osc-2/matches/2026/01/02",
                "output": [{ "name": "id" }],
            }]),
        },
        OutputCase {
            case: "a bucket-only S3 path",
            workflow_name: "osc3",
            output_source_config: json!([{
                "outputS3Path": "s3://er-osc-bucket-only",
                "output": [{ "name": "id" }],
            }]),
        },
        OutputCase {
            case: "hashed output attributes and normalization, with a KMS key",
            workflow_name: "osc-hashed-normalized",
            output_source_config: json!([{
                "outputS3Path": "s3://er-osc/hashed",
                "KMSArn": kms_arn(),
                "output": [{ "name": "id" }, { "name": "email", "hashed": true }],
                "applyNormalization": true,
            }]),
        },
        OutputCase {
            case: "no output S3 path at all",
            workflow_name: "osc_none",
            output_source_config: json!([{ "output": [{ "name": "id" }] }]),
        },
    ]
}

/// The `CreateMatchingWorkflow` request for one row.
fn workflow_request(case: &OutputCase) -> Value {
    json!({
        "workflowName": case.workflow_name,
        "inputSourceConfig": [
            { "inputSourceARN": glue_table_arn("customers"), "schemaName": SCHEMA_NAME },
        ],
        "outputSourceConfig": case.output_source_config,
        "resolutionTechniques": {
            "resolutionType": "RULE_MATCHING",
            "ruleBasedProperties": {
                "rules": [{ "ruleName": "by email", "matchingKeys": ["email"] }],
                "attributeMatchingModel": "ONE_TO_ONE",
            },
        },
        "roleArn": role_arn(),
    })
}

/// `POST /matchingworkflows/{workflowName}/jobs` — `StartMatchingJob`, which models no
/// body members at all.
async fn start_matching_job(emulator: &Emulator, workflow_name: &str) -> Reply {
    emulator
        .call(
            "POST",
            &format!("/matchingworkflows/{workflow_name}/jobs"),
            None,
        )
        .await
}

/// `GET /matchingworkflows/{workflowName}/jobs/{jobId}` — `GetMatchingJob`.
async fn get_matching_job(emulator: &Emulator, workflow_name: &str, job_id: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/matchingworkflows/{workflow_name}/jobs/{job_id}"),
            None,
        )
        .await
}

/// `outputSourceConfig[0].outputS3Path` of a matching-workflow body, as the empty
/// string when the member is absent — `OutputSource$outputS3Path` targets
/// `OptionalS3Path`, whose default IS the empty string.
fn workflow_output_s3_path(body: &Value) -> String {
    body.get("outputSourceConfig")
        .and_then(Value::as_array)
        .and_then(|sources| sources.first())
        .and_then(|source| source.get("outputS3Path"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Assert the one rule on one `GetMatchingJob` body: the member is ABSENT — not `null`,
/// and not an empty list, which the modelled `length 1..1` forbids.
fn assert_no_output_source_config(case: &str, when: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?} ({when}): expected HTTP 200 from GetMatchingJob, got {reply:?}"
    );
    assert!(
        reply.body.get("outputSourceConfig").is_none(),
        "case {case:?} ({when}): a job started with no output-source override must omit \
         outputSourceConfig, got {:?}",
        reply.body
    );
}

/// Poll `GetMatchingJob` until the job reports a terminal status, asserting the rule on
/// every intermediate body too, and answer the settled reply.
async fn settle(case: &str, emulator: &Emulator, workflow_name: &str, job_id: &str) -> Reply {
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        let reply = get_matching_job(emulator, workflow_name, job_id).await;
        assert_no_output_source_config(case, "while polling", &reply);
        let status = string_member(&reply.body, "status");
        if TERMINAL.contains(&status.as_str()) {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the job did not reach a terminal status, last status {status:?} \
             in {:?}",
            reply.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the schema mapping and the workflows of every row.
async fn seed(emulator: &Emulator) {
    emulator.seed_schema_mapping(SCHEMA_NAME).await;
    for case in output_cases() {
        let reply = emulator
            .create_matching_workflow(&workflow_request(&case))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: seeding the matching workflow failed, got {reply:?}",
            case.case
        );
    }
}

#[tokio::test]
async fn get_matching_job_omits_output_source_config_however_the_workflow_writes() {
    let emulator = Emulator::new();
    seed(&emulator).await;

    for case in output_cases() {
        let label = case.case;
        let name = case.workflow_name;

        let started = start_matching_job(&emulator, name).await;
        assert_eq!(
            started.status, 200,
            "case {label:?}: expected HTTP 200 from StartMatchingJob, got {started:?}"
        );
        let job_id = string_member(&started.body, "jobId");

        // Read straight after the start, while the job is still non-terminal...
        let read = get_matching_job(&emulator, name, &job_id).await;
        assert_no_output_source_config(label, "just started", &read);
        assert_eq!(
            string_member(&read.body, "jobId"),
            job_id,
            "case {label:?}: GetMatchingJob must answer the job that was started, got {:?}",
            read.body
        );

        // ...and once its run is over, which is where the destination would have been
        // echoed if it had been taken from the workflow.
        let settled = settle(label, &emulator, name, &job_id).await;
        assert_no_output_source_config(label, "settled", &settled);

        // The destination itself is still readable where it lives: on the WORKFLOW.
        let workflow = emulator.get_matching_workflow(name).await;
        assert_eq!(
            workflow.status, 200,
            "case {label:?}: expected HTTP 200 from GetMatchingWorkflow, got {workflow:?}"
        );
        assert_eq!(
            workflow_output_s3_path(&workflow.body),
            workflow_output_s3_path(&json!({ "outputSourceConfig": case.output_source_config })),
            "case {label:?}: the workflow must still report its own output path, got {:?}",
            workflow.body
        );
    }
}

#[tokio::test]
async fn changing_the_workflows_output_configuration_adds_nothing_to_a_settled_job() {
    let emulator = Emulator::new();
    seed(&emulator).await;

    for case in output_cases() {
        let label = case.case;
        let name = case.workflow_name;

        let started = start_matching_job(&emulator, name).await;
        assert_eq!(
            started.status, 200,
            "case {label:?}: expected HTTP 200 from StartMatchingJob, got {started:?}"
        );
        let job_id = string_member(&started.body, "jobId");
        let settled = settle(label, &emulator, name, &job_id).await;

        // Point the workflow at a different destination, with a KMS key the row may not
        // have had: the job echoed no override before and still has none to echo.
        let mut request = workflow_request(&case);
        request["outputSourceConfig"] = json!([{
            "outputS3Path": "s3://er-osc-updated/matches",
            "KMSArn": kms_arn(),
            "output": [{ "name": "id" }],
        }]);
        request
            .as_object_mut()
            .expect("the update request is a JSON object")
            .remove("workflowName");
        let updated = emulator.update_matching_workflow(name, &request).await;
        assert_eq!(
            updated.status, 200,
            "case {label:?}: expected HTTP 200 from UpdateMatchingWorkflow, got {updated:?}"
        );

        let after = get_matching_job(&emulator, name, &job_id).await;
        assert_no_output_source_config(label, "after the workflow was updated", &after);
        assert_eq!(
            after.body, settled.body,
            "case {label:?}: updating the workflow must not change what the job reports"
        );
    }
}
