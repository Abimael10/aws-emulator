// SPDX-License-Identifier: CC0-1.0

//! WHICH step-concurrency refusal a `StepConcurrencyLevel` earns, end to end over the
//! wire.
//!
//! THE RULE: a level outside the band the member documents falls into one of two classes —
//! above the maximum but still a level a step execution mode runs, or outside what any mode
//! runs — and each PLANE words those two classes its own way. The LAUNCH names the mode
//! that runs the level it was asked for for the first class, and states the band it enforces
//! for the second; the MODIFICATION states that band for the first class, and answers the
//! BARE sentence, which states no band at all, for the second. Only the launch's
//! mode-naming sentence carries anything of the request — the level the caller is being told
//! how to get — and a refusal leaves the level the cluster already holds alone.
//!
//! The table walks the class partition by its boundaries — one below the minimum and far
//! below it, each end of the accepted band, one above the maximum, each end of the
//! mode-served class, and one above it and far above it — on both operations that take the
//! member: `RunJobFlow`, which judges a cluster being launched, and `ModifyCluster`, which
//! judges one already running. Nothing reaches into the service's state: an accepted level
//! is only ever read back through `DescribeCluster`.

use serde_json::json;

use crate::job_flow_lifecycle::{assert_refused, kept_alive_request, minimal_request, Emr};
use crate::notebook_executions::REQUEST_VALIDATION;
use crate::refusal_messages::message;

/// The error identity `ModifyCluster` MODELS for a bad request
/// (`smithy_query(query="operation", operation="ModifyCluster")`).
const MODELLED_CLIENT_ERROR: &str = "InvalidRequestException";

/// The band the member's own documentation publishes: "You can specify a minimum of 1 step
/// and a maximum of 256 steps." (`ModifyCluster$StepConcurrencyLevel` doc) / "The default
/// value is 1. The maximum value is 256." (`RunJobFlow$StepConcurrencyLevel` doc.)
const MIN_LEVEL: i64 = 1;
const MAX_LEVEL: i64 = 256;

/// The most concurrent steps any step execution mode runs, which the service holds as
/// `state::MAX_DISTRIBUTED_STEP_CONCURRENCY_LEVEL` and the launch's own refusal names.
const MAX_MODE_SERVED_LEVEL: i64 = 6400;

/// The sentences, each entirely fixed text.
const BARE: &str = "Invalid step concurrency level.";
const WITH_BAND: &str = "Invalid step concurrency level. Allowed level is from 1 to 256";

/// The launch's sentence for a level a mode could still run: fixed text around the level
/// the request asked for.
fn names_the_mode(level: i64) -> String {
    format!(
        "StepConcurrencyLevel {level} exceeds the maximum ({MAX_LEVEL}) for StepExecutionMode \
         STANDARD. Set StepExecutionMode to DISTRIBUTED for up to {MAX_MODE_SERVED_LEVEL} \
         concurrent steps."
    )
}

/// What a level earns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Inside the documented band: stored, echoed and read back.
    Accepted,
    /// Outside what any step execution mode runs — below the documented minimum, or above
    /// the ceiling the mode-naming sentence publishes.
    BeyondEveryMode,
    /// Above the documented maximum but still a level a mode runs.
    AboveTheBand,
}

impl Class {
    /// The sentence a MODIFICATION of this class is refused with, or `None` when it is not
    /// a refusal.
    fn modification_sentence(self) -> Option<String> {
        match self {
            Class::Accepted => None,
            Class::BeyondEveryMode => Some(BARE.to_string()),
            Class::AboveTheBand => Some(WITH_BAND.to_string()),
        }
    }

    /// The sentence a LAUNCH of this class is refused with, or `None` when it is not a
    /// refusal.
    fn launch_sentence(self, level: i64) -> Option<String> {
        match self {
            Class::Accepted => None,
            Class::BeyondEveryMode => Some(WITH_BAND.to_string()),
            Class::AboveTheBand => Some(names_the_mode(level)),
        }
    }
}

/// The boundaries of the partition, each with the class it falls into.
fn rows() -> Vec<(i64, Class)> {
    vec![
        (i64::from(i32::MIN), Class::BeyondEveryMode),
        (-1, Class::BeyondEveryMode),
        (MIN_LEVEL - 1, Class::BeyondEveryMode),
        (MIN_LEVEL, Class::Accepted),
        (MIN_LEVEL + 1, Class::Accepted),
        ((MIN_LEVEL + MAX_LEVEL) / 2, Class::Accepted),
        (MAX_LEVEL, Class::Accepted),
        (MAX_LEVEL + 1, Class::AboveTheBand),
        (MAX_LEVEL + 2, Class::AboveTheBand),
        (MAX_MODE_SERVED_LEVEL - 1, Class::AboveTheBand),
        (MAX_MODE_SERVED_LEVEL, Class::AboveTheBand),
        (MAX_MODE_SERVED_LEVEL + 1, Class::BeyondEveryMode),
        (10_000, Class::BeyondEveryMode),
        (i64::from(i32::MAX), Class::BeyondEveryMode),
    ]
}

/// `ModifyCluster` answers the sentence of the class the level falls into, stores a level
/// the band takes, and changes nothing when it refuses.
#[tokio::test]
async fn modify_cluster_refuses_each_step_concurrency_class_with_its_own_sentence() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("concurrency classes")).await;

    // The level the cluster holds, which only an accepted row moves: the launch asked for
    // none, so it starts on the documented default.
    let mut held = MIN_LEVEL;

    for (level, class) in rows() {
        let case = format!("ModifyCluster with StepConcurrencyLevel {level} ({class:?})");
        let (status, payload) = emr
            .call(
                "ModifyCluster",
                json!({ "ClusterId": &cluster, "StepConcurrencyLevel": level }),
            )
            .await;

        match class.modification_sentence() {
            Some(sentence) => {
                assert_refused(&case, status, &payload, MODELLED_CLIENT_ERROR);
                let said = message(&case, &payload);
                assert_eq!(
                    said, sentence,
                    "case {case:?}: expected {sentence:?}, got {said:?}"
                );
                assert!(
                    !said.contains(&level.to_string()),
                    "case {case:?}: this plane's sentences are fixed text, so {level} must \
                     not appear in {said:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a level the band takes must be accepted, got {payload}"
                );
                assert_eq!(
                    payload["StepConcurrencyLevel"].as_i64(),
                    Some(level),
                    "case {case:?}: the accepted level must be echoed, got {payload}"
                );
                held = level;
            }
        }

        // A refusal leaves the level the cluster already holds alone; an accepted level is
        // the one it now holds. Either way `DescribeCluster` reports it.
        let described = emr.describe_cluster(&cluster).await;
        assert_eq!(
            described["StepConcurrencyLevel"].as_i64(),
            Some(held),
            "case {case:?}: the cluster must report level {held}, got {described}"
        );
    }
}

/// `RunJobFlow` judges the member on its own plane: each class earns the launch's own
/// sentence for it, and a level the band takes launches a cluster that reports it.
#[tokio::test]
async fn run_job_flow_refuses_each_step_concurrency_class_with_its_own_sentence() {
    let emr = Emr::new();

    for (level, class) in rows() {
        let case = format!("RunJobFlow with StepConcurrencyLevel {level} ({class:?})");
        let mut request = minimal_request("concurrency classes");
        request["StepConcurrencyLevel"] = json!(level);
        let (status, payload) = emr.call("RunJobFlow", request).await;

        match class.launch_sentence(level) {
            Some(sentence) => {
                // The member carries no range in the model, so a launch is refused through
                // the request-validation channel rather than a modelled error.
                assert_refused(&case, status, &payload, REQUEST_VALIDATION);
                let said = message(&case, &payload);
                assert_eq!(
                    said, sentence,
                    "case {case:?}: expected {sentence:?}, got {said:?}"
                );
                if class == Class::BeyondEveryMode {
                    // The band-stating sentence is fixed text: nothing of the request
                    // reaches it, the refused level included.
                    assert!(
                        !said.contains(&level.to_string()),
                        "case {case:?}: the band-stating sentence is fixed text, so {level} \
                         must not appear in {said:?}"
                    );
                }
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a level the band takes must launch, got {payload}"
                );
                let id = payload["JobFlowId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a launch answers an id, got {payload}"));
                let described = emr.describe_cluster(id).await;
                assert_eq!(
                    described["StepConcurrencyLevel"].as_i64(),
                    Some(level),
                    "case {case:?}: the launched cluster must report its level, got \
                     {described}"
                );
            }
        }
    }
}

/// The sentence `ModifyCluster` refuses `level` with, read off the wire.
async fn refusal_of(emr: &Emr, cluster: &str, level: i64) -> String {
    let case = format!("ModifyCluster with StepConcurrencyLevel {level}");
    let (status, payload) = emr
        .call(
            "ModifyCluster",
            json!({ "ClusterId": cluster, "StepConcurrencyLevel": level }),
        )
        .await;
    assert_refused(&case, status, &payload, MODELLED_CLIENT_ERROR);
    message(&case, &payload)
}

/// The two sentences the MODIFICATION plane answers are the SAME sentence, one of them
/// extended: the band-stating refusal begins with the bare refusal's own text, and only the
/// band clause separates them. Both texts come from the service's own answers rather than
/// from a constant in this file.
#[tokio::test]
async fn the_band_stating_sentence_extends_the_bare_one() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("two sentences")).await;

    let bare = refusal_of(&emr, &cluster, MIN_LEVEL - 1).await;
    let with_band = refusal_of(&emr, &cluster, MAX_LEVEL + 1).await;

    assert!(
        with_band.starts_with(&bare),
        "case {:?}: the band-stating sentence must extend the bare one, got {with_band:?} \
         against {bare:?}",
        "the two step-concurrency refusals"
    );
    assert_ne!(
        with_band, bare,
        "case {:?}: the two classes must not answer one sentence, both said {bare:?}",
        "the two step-concurrency refusals"
    );
}
