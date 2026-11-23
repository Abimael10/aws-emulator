// SPDX-License-Identifier: CC0-1.0

//! What `OverrideStageCondition` answers for a stage an entry condition SKIPPED, and what
//! it leaves behind — end to end over the wire the service speaks.
//!
//! The rules under test:
//!
//! * a `BEFORE_ENTRY` condition whose met result is `SKIP` settles the stage `Skipped`,
//!   which is not a gate an override acts on: there is no failed or in-progress rule left
//!   to decide, so the request is refused `ConditionNotOverridableException` (HTTP 400)
//!   whichever condition type it names;
//! * that refusal states ONE fixed sentence whose three substitutions are the REQUEST's
//!   own values — the `conditionType` and `stageName` it named, and the PIPELINE it named
//!   them on in the single-quoted "the execution '…'" slot — so no per-run execution id
//!   reaches the wording;
//! * and the refusal changes nothing: the stage stays `Skipped` and its actions stay
//!   un-run.
//!
//! The rows differ along the condition type the request names — the site the skip was
//! declared at, and the site the stage declares nothing at — and along the stage the
//! request names, which is what the sentence interpolates. Two of them are request classes
//! `tests/all/stage_condition_overrides.rs` also drives against a gate that FAILED a stage;
//! they are repeated here deliberately, because what they are read against is a stage a SKIP
//! settled, where no gate was recorded at all.
//!
//! The harness comes from `tests/all/execution_engine.rs` and the gate builders from
//! `tests/all/stage_condition_overrides.rs`, so this pipeline runs its real actions
//! through the emulated S3 service and every assertion goes through the router.

use std::time::Instant;

use serde_json::{json, Value};

use crate::execution_engine::{status_of, wait_for_terminal, Emulator, WAIT};
use crate::stage_condition_overrides::{
    gated_pipeline, not_overridable_sentence, site, stage_state, variable_check, DEPLOY_BUCKET,
    DEPLOY_KEY,
};

/// The `ConditionType` values the model declares
/// (`smithy_query query="shape" shape="ConditionType"` -> `BEFORE_ENTRY`, `ON_SUCCESS`).
const BEFORE_ENTRY: &str = "BEFORE_ENTRY";
const ON_SUCCESS: &str = "ON_SUCCESS";

/// The refusal status `ConditionNotOverridableException` carries (HTTP 400).
const REFUSAL_STATUS: u16 = 400;

/// The `StageExecutionStatus` of a stage an entry condition's `SKIP` result kept the
/// execution out of.
const SKIPPED: &str = "Skipped";

/// How often a wait looks again.
const LOOK: std::time::Duration = std::time::Duration::from_millis(25);

/// Poll `GetPipelineState` until `stage` reports the status `Skipped`, and answer that
/// stage's state.
async fn wait_for_skipped(emulator: &Emulator, pipeline: &str, stage: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let reported = stage_state(emulator, pipeline, stage).await;
        if reported["latestExecution"]["status"] == json!(SKIPPED) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "the stage {stage} of {pipeline} never reported {SKIPPED}, last {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// RULE: a stage an entry condition skipped holds no overridable gate, so every override
/// of it is refused the one not-overridable sentence — the pipeline in the quoted slot —
/// and the stage stays skipped with nothing performed.
#[tokio::test]
async fn an_override_of_a_skipped_stage_is_refused_and_leaves_it_skipped() {
    let emulator = Emulator::new();
    let pipeline = "cond-skip-override";
    // A comparison that does not hold, so the condition is MET and its SKIP result keeps
    // the execution out of the stage rather than failing it.
    let execution = gated_pipeline(
        &emulator,
        pipeline,
        "beforeEntry",
        site(
            "SKIP",
            variable_check("MismatchCheck", "definitely-mismatched-value-xyz", "EQ"),
        ),
    )
    .await;
    let settled = wait_for_terminal(&emulator, pipeline, &execution).await;
    let before = settled["pipelineExecution"]["status"].clone();
    wait_for_skipped(&emulator, pipeline, "Gated").await;
    let (status, _) = emulator.get_object(DEPLOY_BUCKET, DEPLOY_KEY).await;
    assert_eq!(
        status, 404,
        "a stage the skip kept the execution out of delivers nothing, got {status}"
    );

    // (case, the stage the request names, the condition type it names)
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "the site the skip was declared at",
            "Gated",
            BEFORE_ENTRY,
        ),
        (
            "the site the stage declares no condition at",
            "Gated",
            ON_SUCCESS,
        ),
        (
            "a stage the skip did not act on, at the entry site",
            "Source",
            BEFORE_ENTRY,
        ),
    ];

    for (case, stage_name, condition_type) in rows {
        let (status, response) = emulator
            .cp(
                "OverrideStageCondition",
                &json!({
                    "pipelineName": pipeline,
                    "stageName": stage_name,
                    "pipelineExecutionId": execution,
                    "conditionType": condition_type,
                }),
            )
            .await;
        let code = response["__type"].as_str().unwrap_or_default();
        let message = response["message"]
            .as_str()
            .or_else(|| response["Message"].as_str())
            .unwrap_or_default();
        assert_eq!(
            (status, code),
            (REFUSAL_STATUS, "ConditionNotOverridableException"),
            "case {case:?}: unexpected refusal, got {status} {code} {message:?}"
        );
        let expected = not_overridable_sentence(condition_type, stage_name, pipeline);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the refusal's one sentence, got {message:?}"
        );
        assert!(
            !message.contains(execution.as_str()),
            "case {case:?}: the per-run execution id is not part of the sentence, got \
             {message:?}"
        );

        // The refusal changed nothing: the stage is still the one the skip settled, the
        // execution still carries the status it settled with, and its action never ran.
        let reported = stage_state(&emulator, pipeline, "Gated").await;
        assert_eq!(
            reported["latestExecution"]["status"],
            json!(SKIPPED),
            "case {case:?}: the skipped stage stays skipped, got {reported}"
        );
        let after = status_of(&emulator, pipeline, &execution).await;
        assert_eq!(
            json!(after),
            before,
            "case {case:?}: the execution's own status is unchanged, got {after:?}"
        );
        let (status, _) = emulator.get_object(DEPLOY_BUCKET, DEPLOY_KEY).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the refused override performs no work, got {status}"
        );
    }
}
