// SPDX-License-Identifier: CC0-1.0

//! The IDENTITY of a manual approval, over the wire the service speaks (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: CodePipeline_20150709.<operation>`).
//!
//! The rule under test: the system-generated token a pending manual approval publishes
//! (`ActionExecution$token`, "the system-generated token used to identify a unique
//! approval request", handed out by `GetPipelineState` and consumed by
//! `PutApprovalResult`) identifies the action execution that approval request IS — the
//! provider of a manual approval is the service's own approval record — so that ONE
//! value is what the state reports as the action's `latestExecution.actionExecutionId`,
//! what `ListActionExecutions` reports as the record's `actionExecutionId`, and, once the
//! request has been answered, what the provider's
//! `output.executionResult.externalExecutionId` carries. Each open request has its own
//! handle: two approvals never share one, no other action's run carries it, and a later
//! ATTEMPT of the same action is an action execution of its own, with a token of its own.
//!
//! So a client that answered an approval can find the run its decision produced by the
//! handle it answered with, which is the only handle it holds.
//!
//! The rows differ along what the rule generalises over: how many approvals one stage
//! holds, which decision each is answered with, and whether the run is the action's first
//! attempt or the one a retry started.
//!
//! The harness is `tests/all/execution_engine.rs`'s, so the pipeline's source action runs
//! through the emulated S3 service and every assertion goes through the router.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::execution_engine::{
    artifact_bucket, creation_run, declaration, latest_execution, source_stage, wait_for_terminal,
    Emulator, WAIT,
};

/// The bucket the source object lives in.
const SOURCE_BUCKET: &str = "approval-identity-source";

/// The source object's key.
const SOURCE_KEY: &str = "source.zip";

/// The stage the approval actions wait in.
const GATE_STAGE: &str = "Gate";

/// The name of the source action the harness's source stage declares.
const SOURCE_ACTION: &str = "Src";

/// One manual-approval action: it holds its stage until an approver answers the token it
/// published.
fn approval_action(name: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Approval",
            "owner": "AWS",
            "provider": "Manual",
            "version": "1",
        },
        "configuration": {"CustomData": format!("approve {name}")},
    })
}

/// A declaration with an S3 source stage and one gate stage holding `actions`, every one
/// of them a manual approval at the default run order — so they are all reached together.
fn gate_declaration(pipeline: &str, actions: &[&str]) -> Value {
    let approvals: Vec<Value> = actions.iter().map(|name| approval_action(name)).collect();
    declaration(
        pipeline,
        json!([
            source_stage(SOURCE_BUCKET, SOURCE_KEY, "SourceArtifact"),
            {"name": GATE_STAGE, "actions": approvals},
        ]),
        None,
    )
}

/// The buckets the pipeline reads and writes, the object its source action fetches and the
/// pipeline itself — answering the id of the execution its creation started, which is the
/// run that reaches the approvals.
async fn create_pipeline(emulator: &Emulator, pipeline: &str, actions: &[&str]) -> String {
    emulator.bucket(&artifact_bucket(pipeline)).await;
    emulator.bucket(SOURCE_BUCKET).await;
    emulator
        .put_object(SOURCE_BUCKET, SOURCE_KEY, b"approval identity")
        .await;
    emulator
        .ok("CreatePipeline", &gate_declaration(pipeline, actions))
        .await;
    creation_run(emulator, pipeline).await
}

/// Every action-execution record of one execution, as `ListActionExecutions` reports them.
async fn details(emulator: &Emulator, pipeline: &str, execution: &str) -> Vec<Value> {
    let page = emulator
        .ok(
            "ListActionExecutions",
            &json!({"pipelineName": pipeline, "filter": {"pipelineExecutionId": execution}}),
        )
        .await;
    page["actionExecutionDetails"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing reports actionExecutionDetails, got {page}"))
}

/// The record `history` holds under `id`, which is how a client that holds a handle finds
/// the run it names.
fn record_under(history: &[Value], id: &str, case: &str) -> Value {
    history
        .iter()
        .find(|detail| detail["actionExecutionId"] == json!(id))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no action execution is reported under the handle {id:?}, \
                 got {history:?}"
            )
        })
}

/// Answer one approval.
async fn answer(
    emulator: &Emulator,
    pipeline: &str,
    action: &str,
    token: &str,
    status: &str,
) -> Value {
    emulator
        .ok(
            "PutApprovalResult",
            &json!({
                "pipelineName": pipeline,
                "stageName": GATE_STAGE,
                "actionName": action,
                "result": {"summary": format!("{action}: {status}"), "status": status},
                "token": token,
            }),
        )
        .await
}

/// Poll the state until the approval of `action` publishes a token other than `held` —
/// `None` when any published token will do.
///
/// Waiting for the CONDITION: the token appears when the clock makes the step that
/// reaches the action due, and the attempt a retry displaced published `held`.
async fn published_token(
    emulator: &Emulator,
    pipeline: &str,
    action: &str,
    held: Option<&str>,
    case: &str,
) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let latest = latest_execution(emulator, pipeline, action).await;
        if let Some(token) = latest["token"].as_str() {
            if held != Some(token) {
                return token.to_string();
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {action} never published an approval token other than \
             {held:?} within {WAIT:?}, last {latest}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One row of the table below: the approvals one gate stage holds, the decision each is
/// answered with, and what those decisions make of the run.
struct Row {
    /// What the row is, named in every assertion it drives.
    case: &'static str,
    /// The gate stage's approval actions, in declaration order.
    actions: Vec<&'static str>,
    /// The `ApprovalStatus` each of them is answered with.
    answers: Vec<&'static str>,
    /// The `ActionExecutionStatus` each of them then carries.
    action_statuses: Vec<&'static str>,
    /// The `PipelineExecutionStatus` the execution then reaches.
    execution_status: &'static str,
}

/// RULE: the token a pending manual approval publishes IS the id of the action execution
/// that approval request is, so the state, the history and the provider's external id all
/// report that one value — and every open request has a handle of its own.
#[tokio::test]
async fn the_token_a_pending_approval_publishes_is_the_id_of_its_action_execution() {
    let rows: Vec<Row> = vec![
        Row {
            case: "one approval, approved",
            actions: vec!["Approve"],
            answers: vec!["Approved"],
            action_statuses: vec!["Succeeded"],
            execution_status: "Succeeded",
        },
        Row {
            case: "one approval, rejected",
            actions: vec!["ReleaseApproval"],
            answers: vec!["Rejected"],
            action_statuses: vec!["Failed"],
            execution_status: "Failed",
        },
        Row {
            case: "two approvals of one stage, both approved",
            actions: vec!["SecuritySignOff", "ReleaseApproval"],
            answers: vec!["Approved", "Approved"],
            action_statuses: vec!["Succeeded", "Succeeded"],
            execution_status: "Succeeded",
        },
        Row {
            case: "two approvals of one stage, the second rejected",
            actions: vec!["SecuritySignOff", "ReleaseApproval"],
            answers: vec!["Approved", "Rejected"],
            action_statuses: vec!["Succeeded", "Failed"],
            execution_status: "Failed",
        },
    ];

    for Row {
        case,
        actions,
        answers,
        action_statuses,
        execution_status,
    } in rows
    {
        let emulator = Emulator::new();
        let pipeline = "approval-identity";
        let execution = create_pipeline(&emulator, pipeline, &actions).await;

        // Each approval, while its request is still OPEN: the handle it published is
        // already the id of the run it will settle, and the history records that run
        // under it.
        let mut tokens: Vec<String> = Vec::new();
        for action in &actions {
            let row = format!("{case}, {action} pending");
            let token = published_token(&emulator, pipeline, action, None, &row).await;
            let pending = latest_execution(&emulator, pipeline, action).await;
            assert_eq!(
                pending["actionExecutionId"],
                json!(token),
                "case {row:?}: the state must report the published token {token:?} as the \
                 action execution's id, got {pending}"
            );
            let history = details(&emulator, pipeline, &execution).await;
            let record = record_under(&history, &token, &row);
            assert_eq!(
                record["actionName"],
                json!(*action),
                "case {row:?}: the run reported under {token:?} must be this action's, \
                 got {record}"
            );
            tokens.push(token);
        }

        // One handle per open request: no two approvals share one, and no other action's
        // run carries one.
        let distinct: BTreeSet<&String> = tokens.iter().collect();
        assert_eq!(
            distinct.len(),
            tokens.len(),
            "case {case:?}: each open approval request must have its own handle, \
             got {tokens:?}"
        );
        let history = details(&emulator, pipeline, &execution).await;
        let source = history
            .iter()
            .find(|detail| detail["actionName"] == json!(SOURCE_ACTION))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the source run is reported, got {history:?}"));
        for token in &tokens {
            assert_ne!(
                source["actionExecutionId"],
                json!(token),
                "case {case:?}: an approval's handle must name that approval's run alone, \
                 got {source}"
            );
        }

        for (index, action) in actions.iter().enumerate() {
            answer(&emulator, pipeline, action, &tokens[index], answers[index]).await;
        }

        let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
        assert_eq!(
            settled["pipelineExecution"]["status"],
            json!(execution_status),
            "case {case:?}: unexpected execution status, got {settled}"
        );

        // Each answered approval, read back through both readers: the handle the decision
        // was answered with is the run's id, and the provider's external id for that run.
        let history = details(&emulator, pipeline, &execution).await;
        for (index, action) in actions.iter().enumerate() {
            let row = format!("{case}, {action} answered");
            let token = tokens[index].as_str();
            let answered = latest_execution(&emulator, pipeline, action).await;
            assert_eq!(
                answered["status"],
                json!(action_statuses[index]),
                "case {row:?}: unexpected action status, got {answered}"
            );
            assert!(
                answered["token"].is_null(),
                "case {row:?}: an answered request is no longer open, so no token is \
                 published for it, got {answered}"
            );
            assert_eq!(
                answered["actionExecutionId"],
                json!(token),
                "case {row:?}: the state must report {token:?} as the action execution's \
                 id, got {answered}"
            );
            assert_eq!(
                answered["externalExecutionId"],
                json!(token),
                "case {row:?}: the state must report {token:?} as the provider's external \
                 id, got {answered}"
            );

            let record = record_under(&history, token, &row);
            assert_eq!(
                record["actionName"],
                json!(*action),
                "case {row:?}: the run reported under {token:?} must be this action's, \
                 got {record}"
            );
            assert_eq!(
                record["status"],
                json!(action_statuses[index]),
                "case {row:?}: unexpected reported status, got {record}"
            );
            assert_eq!(
                record["output"]["executionResult"]["externalExecutionId"],
                json!(token),
                "case {row:?}: the history must report {token:?} as the provider's \
                 external id for the run, got {record}"
            );
        }
    }
}

/// RULE, on the other input the rule generalises over: every ATTEMPT of an approval action
/// is an action execution of its own, so a retry publishes a NEW token that is the new
/// run's id, the handle of the attempt it displaced is no longer in force, and the history
/// reports each attempt under its own handle.
#[tokio::test]
async fn a_retried_approval_publishes_a_new_token_that_is_its_new_runs_id() {
    let emulator = Emulator::new();
    let pipeline = "approval-identity-retry";
    let action = "ReleaseApproval";
    let execution = create_pipeline(&emulator, pipeline, &[action]).await;

    // The first attempt, rejected: the stage fails with it.
    let first = published_token(&emulator, pipeline, action, None, "the first attempt").await;
    answer(&emulator, pipeline, action, &first, "Rejected").await;
    let failed = wait_for_terminal(&emulator, pipeline, &execution).await;
    assert_eq!(
        failed["pipelineExecution"]["status"],
        json!("Failed"),
        "case \"the first attempt\": a rejected approval fails the execution, got {failed}"
    );

    emulator
        .ok(
            "RetryStageExecution",
            &json!({
                "pipelineName": pipeline,
                "stageName": GATE_STAGE,
                "pipelineExecutionId": execution,
                "retryMode": "FAILED_ACTIONS",
            }),
        )
        .await;

    // The retried attempt publishes a request of its own, identified by the id of the new
    // run rather than by the displaced attempt's.
    let second = published_token(
        &emulator,
        pipeline,
        action,
        Some(&first),
        "the retried attempt",
    )
    .await;
    assert_ne!(
        second, first,
        "case \"the retried attempt\": a new attempt is a new action execution, so its \
         request has a handle of its own"
    );
    let pending = latest_execution(&emulator, pipeline, action).await;
    assert_eq!(
        pending["actionExecutionId"],
        json!(second),
        "case \"the retried attempt\": the state must report the published token \
         {second:?} as the action execution's id, got {pending}"
    );

    // The displaced attempt's handle names no open request any more.
    let (status, code) = emulator
        .error_code(
            "PutApprovalResult",
            &json!({
                "pipelineName": pipeline,
                "stageName": GATE_STAGE,
                "actionName": action,
                "result": {"summary": "answered with the displaced handle", "status": "Approved"},
                "token": first,
            }),
        )
        .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidApprovalTokenException"),
        "case \"the displaced handle\": expected a 400 InvalidApprovalTokenException, \
         got {status} {code:?}"
    );

    answer(&emulator, pipeline, action, &second, "Approved").await;
    let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
    assert_eq!(
        settled["pipelineExecution"]["status"],
        json!("Succeeded"),
        "case \"the retried attempt\": an approved retry runs the execution on to the \
         end, got {settled}"
    );

    // One record per attempt, each under the handle its own request was identified by.
    let history = details(&emulator, pipeline, &execution).await;
    let attempts: Vec<Value> = history
        .iter()
        .filter(|detail| detail["actionName"] == json!(action))
        .cloned()
        .collect();
    assert_eq!(
        attempts.len(),
        2,
        "case \"both attempts\": the history reports one record per attempt, \
         got {attempts:?}"
    );
    for (row, token, status) in [
        ("the displaced attempt", first.as_str(), "Failed"),
        ("the retried attempt", second.as_str(), "Succeeded"),
    ] {
        let record = record_under(&attempts, token, row);
        assert_eq!(
            record["status"],
            json!(status),
            "case {row:?}: unexpected reported status, got {record}"
        );
        assert_eq!(
            record["output"]["executionResult"]["externalExecutionId"],
            json!(token),
            "case {row:?}: the history must report {token:?} as the provider's external \
             id for the run, got {record}"
        );
    }
}
