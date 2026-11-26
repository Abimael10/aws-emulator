// SPDX-License-Identifier: CC0-1.0

//! The identifier a manual approval is known by, over the wire the service speaks.
//!
//! The rule under test: a manual-approval action's provider is this service's own
//! approval record, and that record IS the action's run — so ONE identifier is minted for
//! that run and every reader reports it: the token `GetPipelineState` publishes for the
//! open request, the `actionExecutionId` of the execution answering it produced, and the
//! `externalExecutionId` the history reports as the provider's external id for it. A
//! client that answered request T can therefore join its decision to the run that
//! recorded it. `PutApprovalResult$token` states the same identity for the execution mode
//! that publishes no token — "use the externalExecutionId in the response output from the
//! ListActionExecutions action as the token in the approval request" — and distinct
//! approvals keep distinct identifiers.
//!
//! The harness comes from `tests/all/execution_engine.rs`, so each row drives a real
//! execution whose source action runs through the emulated S3 service, and every
//! assertion goes through the router.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::execution_engine::{
    action_detail, artifact_bucket, creation_run, declaration, latest_execution, source_stage,
    wait_for_terminal, Emulator, WAIT,
};

/// The bucket the source object of every row lives in.
const SOURCE_BUCKET: &str = "approval-id-source";

/// The source object's key.
const SOURCE_KEY: &str = "source.zip";

/// One manual-approval action: it starts when its stage is entered and then holds the
/// stage until an approver answers the request it published.
fn approval_action(name: &str) -> Value {
    json!({
        "name": name,
        "actionTypeId": {
            "category": "Approval",
            "owner": "AWS",
            "provider": "Manual",
            "version": "1",
        },
    })
}

/// A declaration whose first stage is the S3 source stage and whose later stages hold the
/// named manual-approval actions, all at the default `runOrder` — so every approval of
/// one stage waits concurrently, each with its own request.
fn approval_declaration(pipeline: &str, stages: &[(&str, Vec<&str>)]) -> Value {
    let mut declared = vec![source_stage(SOURCE_BUCKET, SOURCE_KEY, "SourceArtifact")];
    for (stage, actions) in stages {
        declared.push(json!({
            "name": stage,
            "actions": actions.iter().map(|name| approval_action(name)).collect::<Vec<Value>>(),
        }));
    }
    declaration(pipeline, json!(declared), None)
}

/// The buckets the pipeline reads and writes, the object its source action fetches, and
/// the pipeline itself — whose creation runs it once.
async fn create_pipeline(emulator: &Emulator, pipeline: &str, stages: &[(&str, Vec<&str>)]) {
    emulator.bucket(&artifact_bucket(pipeline)).await;
    emulator.bucket(SOURCE_BUCKET).await;
    emulator
        .put_object(SOURCE_BUCKET, SOURCE_KEY, b"the reviewed bytes")
        .await;
    emulator
        .ok("CreatePipeline", &approval_declaration(pipeline, stages))
        .await;
}

/// Whether `candidate` has the shape `ApprovalToken` is modelled with
/// (`smithy_query query="shape" shape="ApprovalToken"` ->
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`).
fn is_approval_token(candidate: &str) -> bool {
    let groups: Vec<&str> = candidate.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && candidate.chars().all(|character| {
            character == '-'
                || (character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        })
}

/// Poll until the named approval action has published a token other than `answered`, and
/// answer with the `latestExecution` reporting it.
///
/// `answered` is the token a previous request of the same action was answered with, so a
/// retried approval is only read once the NEW request is in force.
async fn pending_approval(
    emulator: &Emulator,
    pipeline: &str,
    action: &str,
    answered: Option<&str>,
    case: &str,
) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let latest = latest_execution(emulator, pipeline, action).await;
        if let Some(token) = latest["token"].as_str() {
            if answered != Some(token) {
                return latest;
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {action} never published an open approval request, last {latest}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Answer one approval request.
async fn answer(
    emulator: &Emulator,
    pipeline: &str,
    stage: &str,
    action: &str,
    token: &str,
    status: &str,
) {
    emulator
        .ok(
            "PutApprovalResult",
            &json!({
                "pipelineName": pipeline,
                "stageName": stage,
                "actionName": action,
                "token": token,
                "result": {"summary": "the reviewer's note", "status": status},
            }),
        )
        .await;
}

/// Every record `ListActionExecutions` reports for one action of one execution, newest
/// first — one per attempt.
async fn approval_records(
    emulator: &Emulator,
    pipeline: &str,
    execution: &str,
    action: &str,
) -> Vec<Value> {
    let page = emulator
        .ok(
            "ListActionExecutions",
            &json!({"pipelineName": pipeline, "filter": {"pipelineExecutionId": execution}}),
        )
        .await;
    page["actionExecutionDetails"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|detail| detail["actionName"] == json!(action))
        .cloned()
        .collect()
}

/// One row of the table below: the decision an approval is answered with, what it
/// settles, and the approval stages of the declaration it is answered in — each stage as
/// its name and the manual-approval actions it declares.
struct Row {
    case: &'static str,
    answer_status: &'static str,
    action_status: &'static str,
    execution_status: &'static str,
    stages: Vec<(&'static str, Vec<&'static str>)>,
}

/// RULE: the token a manual approval publishes, the `actionExecutionId` of the run that
/// answering it settles, and the provider's `externalExecutionId` for that run are ONE
/// identifier, read from the emulator's own state; distinct approvals carry distinct
/// identifiers.
///
/// The rows differ along what the rule generalises over: the decision the answer carries,
/// how many approvals one stage holds (so how many requests are open at once), and how
/// many stages of the execution hold one.
#[tokio::test]
async fn an_approval_request_and_the_run_it_settles_share_one_identifier() {
    let rows: Vec<Row> = vec![
        Row {
            case: "one approval, approved",
            answer_status: "Approved",
            action_status: "Succeeded",
            execution_status: "Succeeded",
            stages: vec![("Gate", vec!["Approve"])],
        },
        Row {
            case: "one approval, rejected",
            answer_status: "Rejected",
            action_status: "Failed",
            execution_status: "Failed",
            stages: vec![("Gate", vec!["Approve"])],
        },
        Row {
            case: "two approvals in one stage",
            answer_status: "Approved",
            action_status: "Succeeded",
            execution_status: "Succeeded",
            stages: vec![("Gate", vec!["Approve-one", "Approve-two"])],
        },
        Row {
            case: "an approval in each of two stages",
            answer_status: "Approved",
            action_status: "Succeeded",
            execution_status: "Succeeded",
            stages: vec![("Gate", vec!["Approve"]), ("Ship", vec!["Ship-it"])],
        },
    ];

    for Row {
        case,
        answer_status,
        action_status,
        execution_status,
        stages,
    } in rows
    {
        let emulator = Emulator::new();
        let pipeline = "approval-id";
        create_pipeline(&emulator, pipeline, &stages).await;
        let execution = creation_run(&emulator, pipeline).await;

        // Every identifier this run published, so no two approvals can share one.
        let mut identifiers: Vec<String> = Vec::new();

        for (stage, actions) in &stages {
            // The open requests of ONE stage: read before any of them is answered, so
            // the concurrent approvals are each read while still waiting.
            let mut open: Vec<(&str, String)> = Vec::new();
            for action in actions {
                let case = format!("{case}, {stage}/{action}");
                let latest = pending_approval(&emulator, pipeline, action, None, &case).await;
                let token = latest["token"].as_str().unwrap_or_default().to_string();
                assert!(
                    is_approval_token(&token),
                    "case {case:?}: the published token must have ApprovalToken's shape, \
                     got {latest}"
                );
                assert_eq!(
                    latest["actionExecutionId"], json!(token),
                    "case {case:?}: the open request's token is the id of the action \
                     execution it identifies, got {latest}"
                );
                identifiers.push(token.clone());
                open.push((action, token));
            }

            for (action, token) in &open {
                answer(&emulator, pipeline, stage, action, token, answer_status).await;
                let case = format!("{case}, {stage}/{action}");

                // The state of the answered action: the id it reports is the one the
                // answered request was identified by, and the request itself is no
                // longer open, so it publishes no token.
                let settled = latest_execution(&emulator, pipeline, action).await;
                assert_eq!(
                    (
                        settled["actionExecutionId"].clone(),
                        settled["status"].clone(),
                        settled["token"].clone(),
                    ),
                    (json!(token), json!(action_status), Value::Null),
                    "case {case:?}: the answered approval must report the identifier of \
                     the request it answered and no open token, got {settled}"
                );

                // The same identifier through the history, as the run's own id and as
                // the approval provider's external id for it.
                let detail = action_detail(&emulator, pipeline, &execution, action).await;
                assert_eq!(
                    (
                        detail["actionExecutionId"].clone(),
                        detail["output"]["executionResult"]["externalExecutionId"].clone(),
                    ),
                    (json!(token), json!(token)),
                    "case {case:?}: the history must report the answered request's \
                     identifier as the run's id and as its external id, got {detail}"
                );
            }
        }

        let mut distinct = identifiers.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            identifiers.len(),
            "case {case:?}: every approval carries its own identifier, got {identifiers:?}"
        );

        let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
        assert_eq!(
            settled["pipelineExecution"]["status"], json!(execution_status),
            "case {case:?}: unexpected execution status, got {settled}"
        );
    }
}

/// RULE, on the input the rule above generalises over once more — a SECOND run of the
/// same approval action: each run of the action is an approval request of its own, so a
/// retry mints a new identifier that the new request and the new run share, while the
/// attempt it displaced goes on reporting its own.
#[tokio::test]
async fn retrying_an_approval_mints_an_identifier_the_displaced_attempt_keeps() {
    // The rows differ along the retryMode the retry asks for
    // (`smithy_query query="shape" shape="StageRetryMode"` -> `FAILED_ACTIONS |
    // ALL_ACTIONS`), both of which re-run the failed approval.
    for retry_mode in ["FAILED_ACTIONS", "ALL_ACTIONS"] {
        let case = format!("retried with {retry_mode}");
        let emulator = Emulator::new();
        let pipeline = "approval-retry";
        let stages = vec![("Gate", vec!["Approve"])];
        create_pipeline(&emulator, pipeline, &stages).await;
        let execution = creation_run(&emulator, pipeline).await;

        let first = pending_approval(&emulator, pipeline, "Approve", None, &case).await;
        let first_token = first["token"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            first["actionExecutionId"], json!(first_token),
            "case {case:?}: the first request's token is its run's id, got {first}"
        );
        answer(
            &emulator,
            pipeline,
            "Gate",
            "Approve",
            &first_token,
            "Rejected",
        )
        .await;
        wait_for_terminal(&emulator, pipeline, &execution).await;

        emulator
            .ok(
                "RetryStageExecution",
                &json!({
                    "pipelineName": pipeline,
                    "stageName": "Gate",
                    "pipelineExecutionId": execution,
                    "retryMode": retry_mode,
                }),
            )
            .await;

        let second = pending_approval(
            &emulator,
            pipeline,
            "Approve",
            Some(&first_token),
            &case,
        )
        .await;
        let second_token = second["token"].as_str().unwrap_or_default().to_string();
        assert!(
            is_approval_token(&second_token) && second_token != first_token,
            "case {case:?}: the retried approval opens a request of its own, got {second}"
        );
        assert_eq!(
            second["actionExecutionId"], json!(second_token),
            "case {case:?}: the new request's token is the new run's id, got {second}"
        );

        answer(
            &emulator,
            pipeline,
            "Gate",
            "Approve",
            &second_token,
            "Approved",
        )
        .await;
        let settled = latest_execution(&emulator, pipeline, "Approve").await;
        assert_eq!(
            settled["actionExecutionId"], json!(second_token),
            "case {case:?}: the answered retry reports the identifier it was answered \
             with, got {settled}"
        );

        // Both attempts are in the history, each under the identifier its own request
        // was answered with.
        let records = approval_records(&emulator, pipeline, &execution, "Approve").await;
        let reported: Vec<(Value, Value, Value)> = records
            .iter()
            .map(|detail| {
                (
                    detail["actionExecutionId"].clone(),
                    detail["output"]["executionResult"]["externalExecutionId"].clone(),
                    detail["status"].clone(),
                )
            })
            .collect();
        assert!(
            reported.contains(&(json!(first_token), json!(first_token), json!("Failed")))
                && reported.contains(&(
                    json!(second_token),
                    json!(second_token),
                    json!("Succeeded"),
                )),
            "case {case:?}: each attempt reports its own identifier, got {reported:?}"
        );

        let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
        assert_eq!(
            settled["pipelineExecution"]["status"], json!("Succeeded"),
            "case {case:?}: the retried execution succeeds once the approval is \
             approved, got {settled}"
        );
    }
}
