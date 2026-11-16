// SPDX-License-Identifier: CC0-1.0

//! Every handler of this crate documents the item its operation-struct description
//! describes.
//!
//! RULE: a doc comment documents the item it sits on, so the description of an operation
//! STRUCT — the paragraph that says the item owns the shared engine, implements
//! [`smithy_server_runtime::Operation`] and is generic over the engine type — is stated once
//! per handler and sits directly above that handler's `pub struct <Op><Eng>`, with no other
//! item (a constant, an error constructor) between the two.
//!
//! The crate states that description four times, once per handler, and a hand-written helper
//! inserted above a struct silently moves it onto the helper: the first test below reads each
//! handler's own source and walks from the description to the item it actually documents, so
//! every copy is held to the one rule and the next helper added above a struct fails here
//! rather than shipping a constant documented as "the operation".
//!
//! The description is documentation only — no response, no status and no stored state reads
//! it — so the second test drives the lifecycle through an operation from each handler the
//! description had drifted in (`CompleteRollout`, `UpdateReward` and
//! `SampleWithResponseStream`, plus `Sample`, the handler it had stayed put in) and pins the
//! answers and the stored state the handlers served before: moving a comment changes none of
//! them.

use serde_json::json;
use service_com_amazonaws_sagemakerjobruntime_agenticrftruntimeservice::state;
use service_com_amazonaws_sagemakerjobruntime_agenticrftruntimeservice::ComAmazonawsSagemakerjobruntimeAgenticrftruntimeserviceState as ServiceState;
use smithy_server_runtime::MultiServiceRouter;

// The shared harness: the wire-answer reduction and the job-directory setup every suite
// of this crate drives (`tests/all/harness.rs`).
use crate::harness::{
    create_job, minted_job_arn, mounted_service, send, served_completion, Reply,
    COMPLETE_ROLLOUT_PATH, JOB_ARN_HEADER, SAMPLE_PATH, SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER,
    UPDATE_REWARD_PATH,
};

/// Every handler this crate holds, with the operation it serves and its own source text.
///
/// The four operations the model binds, one handler each
/// (`smithy_query(query="list_operations")`: `Sample`, `SampleWithResponseStream`,
/// `CompleteRollout`, `UpdateReward`), so a handler added for a fifth is declared here and
/// held to the same rule.
const HANDLERS: [(&str, &str, &str); 4] = [
    (
        "Sample",
        "src/ops/sample.rs",
        include_str!("../../src/ops/sample.rs"),
    ),
    (
        "SampleWithResponseStream",
        "src/ops/sample_with_response_stream.rs",
        include_str!("../../src/ops/sample_with_response_stream.rs"),
    ),
    (
        "CompleteRollout",
        "src/ops/complete_rollout.rs",
        include_str!("../../src/ops/complete_rollout.rs"),
    ),
    (
        "UpdateReward",
        "src/ops/update_reward.rs",
        include_str!("../../src/ops/update_reward.rs"),
    ),
];

/// The sentence that makes the operation-struct description recognisable in a handler's
/// source wherever it sits: what the described item OWNS, which is true of the struct and of
/// nothing else in the file.
const DESCRIPTION_MARKER: &str = "Owns the shared engine (the constructor seam)";

/// Whether a source line is part of the doc comment or the outer attributes attached to the
/// item below it, which is what a walk from a doc block to its item steps over.
///
/// An inner attribute (`#![..]`) documents the enclosing module rather than the item below it,
/// so it is NOT stepped over: one left between a description and a struct is a line the walk
/// stops on, and the rule reports it.
fn attached_to_the_item_below(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("///") || line.starts_with("#[")
}

/// RULE: in every handler, the operation-struct description is stated once and the item
/// directly below it is that handler's operation struct.
#[test]
fn every_handler_documents_its_operation_struct() {
    for (operation, path, source) in HANDLERS {
        let case = format!("{path} ({operation})");
        let lines: Vec<&str> = source.lines().collect();

        // Stated once per handler: a second copy would be a second item documented as the
        // operation.
        let carrying: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(DESCRIPTION_MARKER))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            carrying.len(),
            1,
            "case {case:?}: expected the operation-struct description once, got {} \
             occurrence(s) at line(s) {:?}",
            carrying.len(),
            carrying.iter().map(|index| index + 1).collect::<Vec<_>>()
        );
        let marker_line = carrying[0];

        // The block it belongs to opens by naming the operation, which is the first line of
        // the description.
        let mut block_start = marker_line;
        while block_start > 0 && lines[block_start - 1].trim_start().starts_with("///") {
            block_start -= 1;
        }
        let opening = format!("/// The `{operation}` operation.");
        assert_eq!(
            lines[block_start].trim_end(),
            opening,
            "case {case:?}: expected the description to open with {opening:?} at line {}, got \
             {:?}",
            block_start + 1,
            lines[block_start]
        );

        // The item the block documents: the first line below it that is neither doc comment
        // nor attribute.
        let mut item = marker_line + 1;
        while item < lines.len() && attached_to_the_item_below(lines[item]) {
            item += 1;
        }
        // Matched as a PREFIX of the declaration, so how the struct's body is spaced or
        // wrapped below the name is the formatter's business and not this rule's.
        let declaration = format!("pub struct {operation}<Eng>");
        assert!(
            lines
                .get(item)
                .is_some_and(|line| line.trim_start().starts_with(declaration.as_str())),
            "case {case:?}: expected the operation-struct description to document \
             {declaration:?}, got {:?} at line {}",
            lines.get(item),
            item + 1
        );

        // And that declaration is the handler's one operation struct, so the line matched
        // above is the item the description names.
        let declarations = lines
            .iter()
            .filter(|line| line.trim_start().starts_with(declaration.as_str()))
            .count();
        assert_eq!(
            declarations, 1,
            "case {case:?}: expected exactly one {declaration:?}, got {declarations}"
        );
    }
}

/// One sampling call on `path`, which captures a turn on the trajectory.
async fn sample(router: &MultiServiceRouter, path: &str, arn: &str, trajectory_id: &str) -> Reply {
    send(
        router,
        path,
        &[
            (JOB_ARN_HEADER, arn),
            (TRAJECTORY_ID_HEADER, trajectory_id),
            ("content-type", "application/json"),
        ],
        serde_json::to_vec(&json!({ "messages": [{ "role": "user", "content": "go" }] }))
            .expect("a serializable payload"),
    )
    .await
}

/// One `CompleteRollout` call, carrying `status` only when the row states one — an absent
/// `Status` is what the handler's documented default answers.
async fn complete_rollout(
    router: &MultiServiceRouter,
    arn: &str,
    trajectory_id: &str,
    status: Option<&str>,
) -> Reply {
    let mut body = json!({ "TrajectoryId": trajectory_id, "ClientToken": "tok-complete" });
    if let Some(status) = status {
        body["Status"] = json!(status);
    }
    send(
        router,
        COMPLETE_ROLLOUT_PATH,
        &[(JOB_ARN_HEADER, arn), ("content-type", "application/json")],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// One `UpdateReward` call, carrying one reward value per captured turn.
async fn update_reward(
    router: &MultiServiceRouter,
    arn: &str,
    trajectory_id: &str,
    rewards: &[f64],
) -> Reply {
    let body = json!({
        "TrajectoryId": trajectory_id,
        "Rewards": rewards,
        "ClientToken": "tok-reward",
    });
    send(
        router,
        UPDATE_REWARD_PATH,
        &[(JOB_ARN_HEADER, arn), ("content-type", "application/json")],
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// RULE (the behaviour the move leaves alone): the rollout lifecycle answers exactly what it
/// answered before — both sampling operations capture a turn, `CompleteRollout` seals the
/// trajectory with the status the request carried or, absent one, with the default its
/// handler documents, and `UpdateReward` records one score per captured turn on a trajectory
/// sealed eligible for reward submission and conflicts with one sealed terminal.
///
/// The rows differ along the terminal status the rollout is sealed with — absent, each of the
/// two values the model declares — which is what decides whether the reward submission that
/// follows is served or refused.
#[tokio::test]
async fn the_rollout_lifecycle_answers_what_it_answered_before() {
    // One score per turn captured below: the two sampling calls each capture one.
    let rewards = [0.25, 0.75];

    for (case, requested, sealed_with, reward_status) in [
        ("an absent Status", None, "ready", 200),
        ("Status ready", Some("ready"), "ready", 200),
        ("Status failed", Some("failed"), "failed", 409),
    ] {
        let (engine, router) = mounted_service();
        let job_name = "doc-placement-run";
        create_job(&engine, "AgentRFT", job_name);
        let arn = minted_job_arn("AgentRFT", job_name);
        let trajectory_id = "traj-doc-placement";

        // Both sampling handlers serve a turn on the same trajectory.
        for (operation, path) in [
            ("Sample", SAMPLE_PATH),
            ("SampleWithResponseStream", SAMPLE_STREAM_PATH),
        ] {
            let served = sample(&router, path, &arn, trajectory_id).await;
            assert_eq!(
                served.status, 200,
                "case {case:?}/{operation}: expected 200 for a job the control plane created, \
                 got {} {:?} / {}",
                served.status,
                served.error_type,
                served.text()
            );
            let completion = served_completion(&served);
            assert!(
                completion.get("choices").is_some(),
                "case {case:?}/{operation}: expected a completion document in the answer, got \
                 {completion:?}"
            );
        }

        let sealing = complete_rollout(&router, &arn, trajectory_id, requested).await;
        assert_eq!(
            sealing.status, 200,
            "case {case:?}/CompleteRollout: expected 200 for the first sealing call, got {} \
             {:?} / {}",
            sealing.status,
            sealing.error_type,
            sealing.text()
        );

        // Read back through the store the way the next request would.
        {
            let store = engine.state::<ServiceState>();
            let guard = store.lock();
            let trajectory = guard
                .trajectory(&arn, trajectory_id)
                .unwrap_or_else(|| panic!("case {case:?}: expected the sampled trajectory"));
            assert_eq!(
                (
                    trajectory.turns.len(),
                    trajectory.status.as_str(),
                    trajectory.completion_status.map(state::CompletionStatus::as_str),
                ),
                (2, "complete", Some(sealed_with)),
                "case {case:?}: expected two captured turns on a trajectory sealed \
                 {sealed_with:?}, got {trajectory:?}"
            );
        }

        let rewarding = update_reward(&router, &arn, trajectory_id, &rewards).await;
        assert_eq!(
            rewarding.status, reward_status,
            "case {case:?}/UpdateReward: expected {reward_status} for a trajectory sealed \
             {sealed_with:?}, got {} {:?} / {}",
            rewarding.status,
            rewarding.error_type,
            rewarding.text()
        );

        let store = engine.state::<ServiceState>();
        let guard = store.lock();
        let trajectory = guard
            .trajectory(&arn, trajectory_id)
            .unwrap_or_else(|| panic!("case {case:?}: expected the sealed trajectory"));
        if reward_status == 200 {
            assert_eq!(
                (trajectory.status.as_str(), trajectory.rewards.clone()),
                ("reward-received", Some(rewards.to_vec())),
                "case {case:?}: expected the submitted scores recorded, got {trajectory:?}"
            );
        } else {
            assert_eq!(
                rewarding.error_code(),
                "ConflictException",
                "case {case:?}: expected the modelled conflict for a terminal trajectory, got \
                 {:?} / {}",
                rewarding.error_type,
                rewarding.text()
            );
            assert_eq!(
                (trajectory.status.as_str(), trajectory.rewards.clone()),
                ("complete", None),
                "case {case:?}: expected a refused submission to record nothing, got \
                 {trajectory:?}"
            );
        }
    }
}
