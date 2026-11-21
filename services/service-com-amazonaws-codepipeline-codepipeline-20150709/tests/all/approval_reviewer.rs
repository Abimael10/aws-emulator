// SPDX-License-Identifier: CC0-1.0

//! WHO answered a manual approval and WHEN, as the readers of an action's run report it.
//!
//! The first rule under test: `ActionExecution$lastUpdatedBy` and
//! `ActionExecutionDetail$updatedBy` are "the ARN of the user who changed the pipeline
//! execution details", so an action a CALLER's own request moved reports that caller's
//! identity — the principal `sts:GetCallerIdentity` answers the same request with — and
//! an action only the execution plane moved reports none. The identity is the same
//! whichever way the approval was answered, and whichever reader is asked.
//!
//! The second: the instant the answer RECORDED is serialized as an epoch-seconds number
//! at millisecond resolution — the millisecond the stored instant fell in — whether the
//! call that recorded it answers with it (`PutApprovalResult$approvedAt`) or a later read
//! projects it (`ActionExecution$lastStatusChange`,
//! `ActionExecutionDetail$lastUpdateTime`), so one instant is ONE number wherever it is
//! reported.
//!
//! The harness comes from `tests/all/execution_engine.rs`, so the pipeline runs its real
//! actions through the emulated S3 service and every assertion goes through the router.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::ComAmazonawsCodepipelineCodepipeline20150709State as ServiceState;

use crate::execution_engine::{
    artifact_bucket, creation_run, declaration, source_stage, wait_for_terminal, Emulator, WAIT,
};
// The two readings of a serialized instant this test target shares: the epoch-seconds
// number a response carries, and the millisecond a stored instant fell in.
use crate::pipeline_declaration_wire_form::{epoch_millis, millis_of};

/// The bucket the source object lives in.
const SOURCE_BUCKET: &str = "reviewer-source";

/// The source object's key.
const SOURCE_KEY: &str = "source.zip";

/// The stage the approval action waits in, and the action's name.
const GATE_STAGE: &str = "Gate";
/// See [`GATE_STAGE`].
const GATE_ACTION: &str = "Approve";

/// The ARN of the principal every request of this suite is signed for: the IAM user
/// whose long-term key this emulator publishes, which is the identity
/// `sts:GetCallerIdentity` answers with (`service_common::caller_principal`).
const CALLER_ARN: &str = "arn:aws:iam::000000000000:user/emulator";

/// The approval action's `latestExecution`, as `GetPipelineState` reports it.
async fn approval_state(emulator: &Emulator, pipeline: &str) -> Value {
    let state = emulator
        .ok("GetPipelineState", &json!({"name": pipeline}))
        .await;
    state["stageStates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|stage| stage["stageName"] == json!(GATE_STAGE))
        .filter_map(|stage| stage["actionStates"].as_array())
        .flatten()
        .find(|action| action["actionName"] == json!(GATE_ACTION))
        .map(|action| action["latestExecution"].clone())
        .unwrap_or(Value::Null)
}

/// The approval action's run, as `ListActionExecutions` reports it.
async fn approval_detail(emulator: &Emulator, pipeline: &str, execution: &str) -> Value {
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
        .find(|detail| detail["actionName"] == json!(GATE_ACTION))
        .cloned()
        .unwrap_or_else(|| panic!("the approval action's run is reported, got {page}"))
}

/// The declaration this suite drives: an S3 source stage, and the manual-approval stage
/// whose answer the rules above are about.
fn approval_declaration(pipeline: &str) -> Value {
    declaration(
        pipeline,
        json!([
            source_stage(SOURCE_BUCKET, SOURCE_KEY, "SourceArtifact"),
            {
                "name": GATE_STAGE,
                "actions": [{
                    "name": GATE_ACTION,
                    "actionTypeId": {
                        "category": "Approval",
                        "owner": "AWS",
                        "provider": "Manual",
                        "version": "1",
                    },
                }],
            },
        ]),
        None,
    )
}

/// The buckets the pipeline reads and writes, the object its source action fetches, and
/// the pipeline itself — whose creation runs it once, so the run reaching the approval is
/// already in flight when this returns.
async fn create_pipeline(emulator: &Emulator, pipeline: &str) {
    emulator.bucket(&artifact_bucket(pipeline)).await;
    emulator.bucket(SOURCE_BUCKET).await;
    emulator
        .put_object(SOURCE_BUCKET, SOURCE_KEY, b"reviewed bytes")
        .await;
    emulator
        .ok("CreatePipeline", &approval_declaration(pipeline))
        .await;
}

/// Poll until the approval action publishes the token it waits on.
async fn approval_token(emulator: &Emulator, pipeline: &str, case: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let latest = approval_state(emulator, pipeline).await;
        if let Some(token) = latest["token"].as_str() {
            return token.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the approval action never published a token, last {latest}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// RULE: the identity that answered an approval is reported for it, whichever
/// `ApprovalStatus` the answer carried and whichever reader is asked — while an action
/// the execution plane moved on its own reports none.
#[tokio::test]
async fn an_approval_records_the_identity_that_answered_it() {
    // (case, the ApprovalStatus answered with, the status the action then carries, the
    //  status the execution then reaches)
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        ("approved", "Approved", "Succeeded", "Succeeded"),
        ("rejected", "Rejected", "Failed", "Failed"),
    ];

    for (case, answer_status, action_status, execution_status) in rows {
        let emulator = Emulator::new();
        let pipeline = "reviewer-pipe";
        create_pipeline(&emulator, pipeline).await;
        let execution = creation_run(&emulator, pipeline).await;
        let token = approval_token(&emulator, pipeline, case).await;

        // Before anyone answers it, the waiting approval names nobody: the execution
        // plane reached the action, and that is not a caller changing it.
        let waiting = approval_state(&emulator, pipeline).await;
        assert!(
            waiting["lastUpdatedBy"].is_null(),
            "case {case:?}: an approval nobody has answered names nobody, got {waiting}"
        );

        emulator
            .ok(
                "PutApprovalResult",
                &json!({
                    "pipelineName": pipeline,
                    "stageName": GATE_STAGE,
                    "actionName": GATE_ACTION,
                    "result": {"summary": "the reviewer's note", "status": answer_status},
                    "token": token,
                }),
            )
            .await;

        let answered = approval_state(&emulator, pipeline).await;
        assert_eq!(
            answered["lastUpdatedBy"], json!(CALLER_ARN),
            "case {case:?}: the approval names the identity that answered it, \
             got {answered}"
        );
        assert_eq!(
            answered["status"], json!(action_status),
            "case {case:?}: unexpected action status, got {answered}"
        );

        // The same fact through the other reader, which reports it as `updatedBy`.
        let detail = approval_detail(&emulator, pipeline, &execution).await;
        assert_eq!(
            detail["updatedBy"], json!(CALLER_ARN),
            "case {case:?}: the action execution names the identity that changed it, \
             got {detail}"
        );

        // An action of the same execution that no caller touched still names nobody.
        let page = emulator
            .ok(
                "ListActionExecutions",
                &json!({"pipelineName": pipeline, "filter": {"pipelineExecutionId": execution}}),
            )
            .await;
        let source = page["actionExecutionDetails"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|detail| detail["actionName"] == json!("Src"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the source action is reported, got {page}"));
        assert!(
            source["updatedBy"].is_null(),
            "case {case:?}: an action only the execution plane moved names nobody, \
             got {source}"
        );

        let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
        assert_eq!(
            settled["pipelineExecution"]["status"], json!(execution_status),
            "case {case:?}: unexpected execution status, got {settled}"
        );
    }
}

/// The instant the approval's answer recorded, as the ONE store every projection of it
/// reads holds it.
fn recorded_instant(emulator: &Emulator, execution: &str, case: &str) -> SystemTime {
    let state = emulator.engine().state::<ServiceState>();
    let state = state.lock();
    state
        .executions
        .get(execution)
        .into_iter()
        .flat_map(|record| record.stages.iter())
        .filter(|stage| stage.name == GATE_STAGE)
        .flat_map(|stage| stage.actions.iter())
        .find(|action| action.name == GATE_ACTION)
        .map(|action| action.last_status_change)
        .unwrap_or_else(|| panic!("case {case:?}: the answered approval must be in the store"))
}

/// Put `instant` on the approval's run, so a read of a stored instant with a known
/// sub-second part can be asked for.
fn set_recorded_instant(emulator: &Emulator, execution: &str, instant: SystemTime, case: &str) {
    let state = emulator.engine().state::<ServiceState>();
    let mut state = state.lock();
    let action = state
        .executions
        .get_mut(execution)
        .into_iter()
        .flat_map(|record| record.stages.iter_mut())
        .filter(|stage| stage.name == GATE_STAGE)
        .flat_map(|stage| stage.actions.iter_mut())
        .find(|action| action.name == GATE_ACTION)
        .unwrap_or_else(|| panic!("case {case:?}: the answered approval must be in the store"));
    action.last_status_change = instant;
}

/// RULE: the instant an answered approval recorded is serialized at MILLISECOND
/// resolution, and the call that recorded it and the two reads that project it report the
/// same number for it.
///
/// Two phases, because a number the service writes has a value as well as a shape. First
/// the instant the answer itself recorded: `approvedAt` must be the millisecond the stored
/// instant fell in, which is exact without the test knowing what the host's clock read,
/// and both readers must report that same number. Then the sub-second part the store may
/// hold, which no call can be made to produce: the recorded instant is replaced by one
/// with a known sub-second part and read back, with the rows differing along what the rule
/// generalises over — a whole second, both sides of a single millisecond, a millisecond
/// trailed by finer digits, and the last nanosecond below a second.
///
/// The outer rows are the two decisions an answer can carry, so the rule is asserted on an
/// approval that settled its action at `Succeeded` and on one that settled it at `Failed`.
#[tokio::test]
async fn the_instant_an_answered_approval_recorded_is_reported_at_millisecond_resolution() {
    // The second count the crafted instants below sit on, kept well away from the host's
    // own clock so a projection reporting the wrong instant cannot pass.
    const CRAFTED_SECS: u64 = 1_700_000_000;

    // (case, the sub-second NANOseconds stored, the sub-second MILLIseconds reported)
    let crafted: [(&str, u32, i64); 6] = [
        ("a whole second", 0, 0),
        ("one nanosecond past the second", 1, 0),
        ("the last nanosecond below a millisecond", 999_999, 0),
        ("exactly one millisecond", 1_000_000, 1),
        ("a millisecond trailed by finer digits", 272_499_999, 272),
        ("the last nanosecond below a second", 999_999_999, 999),
    ];

    for (decision, answer_status) in [("approved", "Approved"), ("rejected", "Rejected")] {
        let emulator = Emulator::new();
        let pipeline = "reviewer-instant";
        create_pipeline(&emulator, pipeline).await;
        let execution = creation_run(&emulator, pipeline).await;
        let token = approval_token(&emulator, pipeline, decision).await;

        let answered = emulator
            .ok(
                "PutApprovalResult",
                &json!({
                    "pipelineName": pipeline,
                    "stageName": GATE_STAGE,
                    "actionName": GATE_ACTION,
                    "result": {"summary": "the reviewer's note", "status": answer_status},
                    "token": token,
                }),
            )
            .await;

        let approved_at = epoch_millis(
            decision,
            "PutApprovalResult.approvedAt",
            &answered["approvedAt"],
        );
        assert_eq!(
            approved_at,
            millis_of(recorded_instant(&emulator, &execution, decision)),
            "case {decision:?}: approvedAt must be the millisecond the recorded instant \
             fell in, got {answered}"
        );

        // The same instant through the two reads that project it.
        let latest = approval_state(&emulator, pipeline).await;
        assert_eq!(
            epoch_millis(
                decision,
                "GetPipelineState.latestExecution.lastStatusChange",
                &latest["lastStatusChange"],
            ),
            approved_at,
            "case {decision:?}: the state must report the instant the answer recorded, \
             got {latest}"
        );
        let detail = approval_detail(&emulator, pipeline, &execution).await;
        assert_eq!(
            epoch_millis(decision, "ListActionExecutions.lastUpdateTime", &detail["lastUpdateTime"]),
            approved_at,
            "case {decision:?}: the action execution must report the instant the answer \
             recorded, got {detail}"
        );

        // The run is over before a crafted instant is written, so nothing the execution
        // plane still owes can move the instant under the reads below.
        wait_for_terminal(&emulator, pipeline, &execution).await;
        for (case, nanos, sub_second_millis) in crafted {
            let case = format!("{decision}, {case}");
            set_recorded_instant(
                &emulator,
                &execution,
                SystemTime::UNIX_EPOCH + Duration::new(CRAFTED_SECS, nanos),
                &case,
            );
            let expected = CRAFTED_SECS as i64 * 1_000 + sub_second_millis;

            let latest = approval_state(&emulator, pipeline).await;
            assert_eq!(
                epoch_millis(&case, "GetPipelineState.lastStatusChange", &latest["lastStatusChange"]),
                expected,
                "case {case:?}: the state must report the millisecond the stored instant \
                 fell in, got {latest}"
            );
            let detail = approval_detail(&emulator, pipeline, &execution).await;
            assert_eq!(
                epoch_millis(&case, "ListActionExecutions.lastUpdateTime", &detail["lastUpdateTime"]),
                expected,
                "case {case:?}: the action execution must report the millisecond the \
                 stored instant fell in, got {detail}"
            );
        }
    }
}
