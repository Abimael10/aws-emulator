// SPDX-License-Identifier: CC0-1.0

//! The concurrent-execution SLOTS a pipeline occupies, probed across the clock: which
//! executions hold one, and when it is given back.
//!
//! The rule under test: an execution occupies one of its pipeline's slots from the
//! moment it is recorded until it reaches a terminal `PipelineExecutionStatus`, so what
//! decides whether a further trigger is admitted is the set of executions that have not
//! completed — and nothing else. The rows span what that rule generalises over: whether
//! the execution's actions are work the engine performs (so the clock carries the
//! execution to a terminal status and the slot comes back) or work that waits for a
//! reporter (so the slot stays taken), and whether the trigger is a revision report or
//! a start.
//!
//! The suite exists because the two sides of that rule are what make the modelled
//! `ConcurrentPipelineExecutionsLimitExceededException` reachable at all: a pipeline
//! whose executions complete is never at its ceiling however many triggers it is sent,
//! while one whose executions wait reaches it at the bound and not before.
//!
//! The harness comes from `tests/all/execution_engine.rs`, so the engine reaches the
//! emulator's own S3 service the way the production emulator does, and every assertion
//! goes through the router.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_codepipeline_codepipeline_20150709::state::MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE;

use crate::execution_engine::{Emulator, WAIT};
use crate::pipeline_lifecycle::ROLE_ARN;

/// The sentence `StartPipelineExecution` states this condition with, written out here so
/// a reword of the handler's wording fails this suite: the limit was exceeded, and the
/// caller is told to try again later. It carries no variable part.
const AT_THE_LIMIT_STARTING: &str =
    "The limit for concurrent pipeline executions has been exceeded. Please try again later.";

/// The sentence `PutActionRevision` states the SAME condition with: the shared first
/// sentence, and a second one naming what the revision driver is to wait for. The two
/// calls' texts differ, so one shared constant cannot serve both.
const AT_THE_LIMIT_REPORTING: &str = "The limit for concurrent pipeline executions has been \
     exceeded. Wait for concurrent executions to complete, and then try again.";

/// How long a poll loop below waits between looks, while the steps the clock owes come
/// due (`state::execution::ACTION_STEP` is the span each step waits). The deadline every
/// loop gives up at is the harness's own `WAIT`.
const LOOK: Duration = Duration::from_millis(50);

/// The `PipelineExecutionStatus` values an execution can still leave — the ones that
/// hold a slot.
///
/// `pub(crate)` with the helpers below: the suite that pins how the ceiling is WORDED
/// (`concurrent_execution_limit_wording`) reads the slots the same way rather than
/// growing a second copy of this.
pub(crate) const NON_TERMINAL: [&str; 2] = ["InProgress", "Stopping"];

/// A pipeline whose source action is `(owner, provider)`, in `PARALLEL` mode so that
/// every admitted execution runs on its own rather than waiting for the one before it.
///
/// `pub(crate)` with the helpers below, for the same reason as [`NON_TERMINAL`].
pub(crate) fn pipeline(name: &str, owner: &str, provider: &str) -> Value {
    json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "pipelineType": "V2",
        "executionMode": "PARALLEL",
        "artifactStore": {"type": "S3", "location": "artifacts"},
        "stages": [
            {
                "name": "Source",
                "actions": [{
                    "name": "Src",
                    "actionTypeId": {
                        "category": "Source",
                        "owner": owner,
                        "provider": provider,
                        "version": "1",
                    },
                    "configuration": {"S3Bucket": "sources", "S3ObjectKey": "app.zip"},
                    "outputArtifacts": [{"name": "SourceOut"}],
                }],
            },
            {
                "name": "Approve",
                "actions": [{
                    "name": "Manual",
                    "actionTypeId": {
                        "category": "Approval",
                        "owner": "AWS",
                        "provider": "Manual",
                        "version": "1",
                    },
                }],
            },
        ],
    })
}

/// Report a new revision for the pipeline's source action.
pub(crate) async fn put_revision(
    emulator: &Emulator,
    pipeline: &str,
    revision: &str,
) -> (u16, Value) {
    emulator
        .cp(
            "PutActionRevision",
            &json!({
                "pipelineName": pipeline,
                "stageName": "Source",
                "actionName": "Src",
                "actionRevision": {
                    "revisionId": revision,
                    "revisionChangeId": format!("change-{revision}"),
                    "created": 1_760_000_000_i64,
                },
            }),
        )
        .await
}

/// The statuses the pipeline's retained executions carry, as
/// `ListPipelineExecutions` reports them.
pub(crate) async fn statuses(emulator: &Emulator, pipeline: &str) -> Vec<String> {
    let listed = emulator
        .ok("ListPipelineExecutions", &json!({"pipelineName": pipeline}))
        .await;
    listed["pipelineExecutionSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["status"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every action run `ListActionExecutions` reports for the pipeline, across all of its
/// pages: one page carries at most 100 details and a pipeline at its ceiling has more
/// runs than that.
async fn action_details(emulator: &Emulator, pipeline: &str) -> Vec<Value> {
    let mut details: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut request = json!({"pipelineName": pipeline, "maxResults": 100});
        if let Some(token) = &next {
            request["nextToken"] = json!(token);
        }
        let page = emulator.ok("ListActionExecutions", &request).await;
        details.extend(
            page["actionExecutionDetails"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        match page["nextToken"].as_str() {
            Some(token) => next = Some(token.to_string()),
            None => return details,
        }
    }
}

/// RULE: the slot comes back when the execution reaches a terminal status, so a
/// pipeline whose executions the clock carries to one is admitted a further trigger
/// however many it has already been sent.
#[tokio::test]
async fn a_completed_execution_gives_its_slot_back() {
    let emulator = Emulator::new();
    // An `AWS`-owned S3 source action: work the engine performs itself, so the clock
    // carries each execution to a terminal status (here `Failed`, because no source
    // object was ever put where the declaration points — nothing is reported as having
    // run when nothing ran).
    emulator
        .ok(
            "CreatePipeline",
            &json!({"pipeline": pipeline("served", "AWS", "S3")}),
        )
        .await;

    // More rounds than the pipeline has slots would be refused if a completed
    // execution kept its slot; each round waits for the steps the clock owes.
    for round in 0..3 {
        let (status, answered) = put_revision(&emulator, "served", &format!("rev-{round}")).await;
        assert_eq!(
            (status, answered["newRevision"].clone()),
            (200, json!(true)),
            "round {round}: a new revision of a pipeline with a free slot is admitted, \
             got {status} with {answered}"
        );
        // Look again until the clock has carried every execution to a terminal status,
        // rather than waiting a fixed span for it.
        let deadline = Instant::now() + WAIT;
        loop {
            let statuses = statuses(&emulator, "served").await;
            let in_flight = statuses
                .iter()
                .filter(|status| NON_TERMINAL.contains(&status.as_str()))
                .count();
            if in_flight == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "round {round}: every execution the clock has carried to a terminal status \
                 has given its slot back, got {statuses:?}"
            );
            tokio::time::sleep(LOOK).await;
        }
    }
}

/// RULE: an execution whose source action the engine CAN perform is not finished by
/// performing it — it carries on to the action that waits, so it keeps its slot — and
/// what the slots hold is what makes the ceiling reachable through a revision report:
/// the report past the bound is refused and records nothing.
///
/// The pipeline here is the one a revision report reaches in anger: an `AWS`-owned S3
/// source action whose object IS in the bucket, followed by a manual approval nobody
/// answers. The revisions reported for it carry ids no S3 object version matches, which
/// is what a caller reporting its own revision sends.
#[tokio::test]
async fn reported_revisions_fill_the_slots_of_a_pipeline_whose_source_is_readable() {
    let emulator = Emulator::new();
    // The source the declaration points at, and the artifact store it writes to, both
    // really there — so the source action's work is performed rather than failing.
    emulator.bucket("sources").await;
    emulator.bucket("artifacts").await;
    emulator
        .put_object("sources", "app.zip", b"the application bytes")
        .await;
    emulator
        .ok(
            "CreatePipeline",
            &json!({"pipeline": pipeline("holding", "AWS", "S3")}),
        )
        .await;

    // One slot is already taken by the run the pipeline's creation started, so the
    // reports that fit inside the bound are one fewer than the bound.
    for index in 0..MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - 1 {
        let (status, answered) = put_revision(&emulator, "holding", &format!("rev-{index}")).await;
        assert_eq!(
            (status, answered["newRevision"].clone()),
            (200, json!(true)),
            "revision {index} is inside the bound of {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE}, \
             got {status} with {answered}"
        );
    }

    // Look again until the clock has run every admitted execution's source action: each
    // execution is then held at the approval nobody answers, so every slot is still
    // taken.
    let deadline = Instant::now() + WAIT;
    let held = loop {
        let sources_run = action_details(&emulator, "holding")
            .await
            .iter()
            .filter(|detail| {
                detail["actionName"] == json!("Src") && detail["status"] == json!("Succeeded")
            })
            .count();
        let held = statuses(&emulator, "holding").await;
        if sources_run == MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE {
            break held;
        }
        assert!(
            Instant::now() < deadline,
            "expected every one of the {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE} admitted \
             executions to have run its source action, got {sources_run} with {held:?}"
        );
        tokio::time::sleep(LOOK).await;
    };
    let in_flight = held
        .iter()
        .filter(|status| NON_TERMINAL.contains(&status.as_str()))
        .count();
    assert_eq!(
        in_flight, MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "the reported revision names no object version, so no execution failed over one \
         and every one of them is held at its approval, got {held:?}"
    );

    let (status, response) = put_revision(&emulator, "holding", "one-too-many").await;
    assert_eq!(
        (
            status,
            response["__type"].as_str(),
            response["message"].as_str()
        ),
        (
            400,
            Some("ConcurrentPipelineExecutionsLimitExceededException"),
            Some(AT_THE_LIMIT_REPORTING)
        ),
        "a revision report past the bound is refused, got {status} with {response}"
    );
    let retained = statuses(&emulator, "holding").await;
    assert_eq!(
        retained.len(),
        MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "a refused report records no execution, got {retained:?}"
    );
    // And the refusal is not a one-off state: the pipeline is still at its ceiling, so
    // the next report is refused the same way.
    let (status, response) = put_revision(&emulator, "holding", "one-more").await;
    assert_eq!(
        (status, response["__type"].as_str()),
        (400, Some("ConcurrentPipelineExecutionsLimitExceededException")),
        "the pipeline is still at its ceiling, got {status} with {response}"
    );
}

#[tokio::test]
async fn executions_that_wait_fill_the_pipelines_slots() {
    let emulator = Emulator::new();
    // A `Custom`-owned source action: its work is a job this account's own worker
    // reports the outcome of, so the execution stays non-terminal until one does.
    emulator
        .ok(
            "CreatePipeline",
            &json!({"pipeline": pipeline("waiting", "Custom", "MySource")}),
        )
        .await;

    // One slot is already taken by the run the pipeline's creation started, so the
    // reports that fit inside the bound are one fewer than the bound.
    for index in 0..MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE - 1 {
        let (status, answered) = put_revision(&emulator, "waiting", &format!("rev-{index}")).await;
        assert_eq!(
            status, 200,
            "revision {index} is inside the bound of {MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE}, \
             got {status} with {answered}"
        );
    }
    // The clock changes nothing here: the slots are held by executions waiting for a
    // report their job worker never makes, not by steps that have not come due, so the
    // count below is the same at any moment.
    let waiting = statuses(&emulator, "waiting").await;
    let in_flight = waiting
        .iter()
        .filter(|status| NON_TERMINAL.contains(&status.as_str()))
        .count();
    assert_eq!(
        in_flight, MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "every execution waiting for its job worker holds a slot, got {waiting:?}"
    );

    // One past the bound, through either call that starts an execution: the same
    // accounting and the same modelled error answer both, each in that call's own words.
    let (status, response) = put_revision(&emulator, "waiting", "one-too-many").await;
    assert_eq!(
        (
            status,
            response["__type"].as_str(),
            response["message"].as_str()
        ),
        (
            400,
            Some("ConcurrentPipelineExecutionsLimitExceededException"),
            Some(AT_THE_LIMIT_REPORTING)
        ),
        "a revision report past the bound is refused, got {status} with {response}"
    );
    let (status, response) = emulator
        .cp("StartPipelineExecution", &json!({"name": "waiting"}))
        .await;
    assert_eq!(
        (
            status,
            response["__type"].as_str(),
            response["message"].as_str()
        ),
        (
            400,
            Some("ConcurrentPipelineExecutionsLimitExceededException"),
            Some(AT_THE_LIMIT_STARTING)
        ),
        "a start past the same bound is refused in its own words, got {status} with {response}"
    );

    // And the refusals started nothing: the pipeline still holds exactly its bound.
    let retained = statuses(&emulator, "waiting").await;
    assert_eq!(
        retained.len(),
        MAX_CONCURRENT_EXECUTIONS_PER_PIPELINE,
        "a refused trigger records no execution, got {retained:?}"
    );
}
