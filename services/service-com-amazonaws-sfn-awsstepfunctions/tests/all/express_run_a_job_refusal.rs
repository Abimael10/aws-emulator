// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the run-a-job (`.sync`) integration pattern inside an EXPRESS
//! workflow, driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: the run-a-job pattern is served for the STANDARD workflow type
//! only, so a definition whose `Task` state asks for it is REFUSED where it is
//! submitted as an EXPRESS state machine — `InvalidDefinition`, stating the service's
//! own refusal of the pattern at the `Resource` that asked for it — and no state
//! machine is created. The same definition created as a STANDARD workflow starts its
//! job and parks at it, which is the other side of the same rule.
//!
//! The rows differ along what the rule generalises over: the revision of the pattern
//! the `Resource` asks for (`.sync` and `.sync:2`), the INTEGRATION it asks it of (a
//! nested execution, whose calls this service serves itself, and an Athena query,
//! whose service is not even mounted here), and the name of the offending state.
//!
//! The harness (`Emulator`, `create_machine`, `string`) is the one
//! `state_machine_lifecycle` builds and the history reader is `execution_history`'s;
//! `tests/all/main.rs` picks this file up, so each is reached through `crate::<suite>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::EXPRESS_RUN_A_JOB_UNSUPPORTED;

use crate::execution_history::history;
use crate::state_machine_lifecycle::{create_machine, string, Emulator, ROLE_ARN};

/// One row of the refusal table: the case, the state name, and the `Resource` that
/// state's run-a-job `Task` names.
type RefusalRow = (&'static str, &'static str, &'static str);

/// A definition whose one `Task` state, named `state`, asks for the run-a-job pattern
/// through `resource`.
fn parent_definition(state: &str, resource: &str, child_arn: &str) -> String {
    json!({
        "StartAt": state,
        "States": {
            state: {
                "Type": "Task",
                "Resource": resource,
                "Parameters": { "StateMachineArn": child_arn, "Input": {} },
                "End": true,
            }
        }
    })
    .to_string()
}

/// A child definition that finishes at once, for the STANDARD control to really run.
const CHILD: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// How many executions `ListExecutions` reports for `machine_arn`.
async fn started_count(emulator: &Emulator, machine_arn: &str) -> usize {
    let listed = emulator
        .ok(
            "ListExecutions",
            &json!({ "stateMachineArn": machine_arn, "maxResults": 10 }),
        )
        .await;
    listed
        .get("executions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// Creating an EXPRESS state machine from a definition that asks for the run-a-job
/// pattern is refused, naming the pattern, and creates nothing.
#[tokio::test]
async fn an_express_create_refuses_a_run_a_job_task() {
    let rows: [RefusalRow; 4] = [
        (
            "a nested execution through the pattern's first revision",
            "CallChild",
            "arn:aws:states:::states:startExecution.sync",
        ),
        (
            "the same integration through its second revision",
            "CallChild",
            "arn:aws:states:::states:startExecution.sync:2",
        ),
        (
            "a state of another name, which the refusal must locate instead",
            "RunTheChildWorkflow",
            "arn:aws:states:::states:startExecution.sync",
        ),
        (
            // Athena is not mounted beside this service here, so the refusal cannot be
            // anything the integration itself answered: it is the workflow type's,
            // decided before anything is called.
            "another integration's run-a-job pattern entirely",
            "RunQuery",
            "arn:aws:states:::athena:startQueryExecution.sync",
        ),
    ];

    let emulator = Emulator::new();
    let child_arn = create_machine(&emulator, "express-sync-child", CHILD).await;
    for (index, (case, state, resource)) in rows.into_iter().enumerate() {
        let definition = parent_definition(state, resource, &child_arn);
        let name = format!("express-sync-{index}");
        let (status, body) = emulator
            .call(
                "CreateStateMachine",
                &json!({
                    "name": &name,
                    "definition": definition,
                    "roleArn": ROLE_ARN,
                    "type": "EXPRESS",
                }),
            )
            .await;
        assert_eq!(
            (status, body.get("__type").and_then(Value::as_str)),
            (400, Some("InvalidDefinition")),
            "case {case:?}: expected 400 InvalidDefinition, got {status} with {body:?}"
        );
        let message = string(&body, "message");
        assert!(
            message.contains(EXPRESS_RUN_A_JOB_UNSUPPORTED),
            "case {case:?}: expected the refusal to state {EXPRESS_RUN_A_JOB_UNSUPPORTED:?}, got \
             {message:?}"
        );
        assert!(
            message.contains(&format!("/States/{state}/Resource")),
            "case {case:?}: expected the refusal to locate the Resource that asked, got \
             {message:?}"
        );
        // Nothing was created: the name is free, which the STANDARD type takes.
        let (created, answered) = emulator
            .call(
                "CreateStateMachine",
                &json!({
                    "name": &name,
                    "definition": &definition,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        assert_eq!(
            created, 200,
            "case {case:?}: expected the refused create to have stored nothing, got {created} \
             with {answered:?}"
        );
    }

    // Nothing was started for the child either: no execution of a refused definition
    // exists to have called it.
    assert_eq!(
        started_count(&emulator, &child_arn).await,
        0,
        "case \"the child a refused task would have started\": expected no execution of it"
    );
}

/// The SAME definition run as a STANDARD workflow starts its job and parks at it: the
/// refusal above is the workflow type's and not the definition's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_standard_execution_of_the_same_definition_starts_its_job() {
    let emulator = Emulator::new();
    let child_arn = create_machine(&emulator, "standard-sync-child", CHILD).await;
    let definition = parent_definition(
        "CallChild",
        "arn:aws:states:::states:startExecution.sync",
        &child_arn,
    );
    let parent = create_machine(&emulator, "standard-sync-parent", &definition).await;
    let started = emulator
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": parent, "name": "parked" }),
        )
        .await;
    let execution_arn = string(&started, "executionArn");
    let case = "a STANDARD execution of the same definition";

    let described = emulator
        .ok(
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("RUNNING"),
        "case {case:?}: expected the parent to be parked at its job, got {described:?}"
    );
    assert_eq!(
        started_count(&emulator, &child_arn).await,
        1,
        "case {case:?}: expected the job to have been started as a child execution"
    );
    let submitted = history(&emulator, &execution_arn, json!({}))
        .await
        .into_iter()
        .any(|event| event.get("type").and_then(Value::as_str) == Some("TaskSubmitted"));
    assert!(
        submitted,
        "case {case:?}: expected the submission of the job in the parent's history"
    );
}
