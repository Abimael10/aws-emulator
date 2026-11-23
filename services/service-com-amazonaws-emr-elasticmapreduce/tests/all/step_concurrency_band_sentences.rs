// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a `StepConcurrencyLevel` outside the band Amazon EMR allows is refused
//! with, end to end over the wire.
//!
//! THE RULE: a level outside the band falls into one of two classes — above the documented
//! maximum but still a level a step execution mode runs, or outside what any mode runs — and
//! each PLANE words those classes its own way: the LAUNCH names the mode that runs the level
//! it was asked for for the first and states the band it enforces for the second, while the
//! MODIFICATION states that band for the first and answers the bare sentence, which states
//! no band at all, for the second. Only the launch's mode-naming sentence carries anything of
//! the request; neither of the others echoes the level it refused, and none is a
//! range-checker's prose.
//!
//! The table spans both sides of the band, each published bound and one past it, numbers
//! far past both, and both operations that take the member — `RunJobFlow`, which judges a
//! cluster being launched, and `ModifyCluster`, which judges one already running — so each
//! sentence is a property of the operation and the class rather than of one request.

use serde_json::json;

use crate::job_flow_lifecycle::{assert_refused, kept_alive_request, minimal_request, Emr};
use crate::notebook_executions::REQUEST_VALIDATION;
use crate::refusal_messages::message;

/// The service's own client error, which `ModifyCluster` models for a bad request.
const SERVICE_ERROR: &str = "InvalidRequestException";

/// The band the documentation publishes: "The default value is 1. The maximum value is
/// 256." (`RunJobFlow$StepConcurrencyLevel`) / "a minimum of 1 step and a maximum of 256
/// steps" (`ModifyCluster$StepConcurrencyLevel`).
const BAND: (i64, i64) = (1, 256);

/// The most concurrent steps any step execution mode runs, which the launch's own refusal
/// names; a level past it is one no mode runs.
const WIDEST_CEILING: i64 = 6400;

/// The sentence a modification of a level no mode runs earns: fixed text that states no
/// band and nothing of the request.
const BARE_SENTENCE: &str = "Invalid step concurrency level.";

/// The sentence that STATES the band the member may use: the bare sentence extended with
/// the enforced band. A launch earns it for a level no mode runs; a modification for a level
/// above the documented maximum that a mode does run.
const BAND_SENTENCE: &str = "Invalid step concurrency level. Allowed level is from 1 to 256";

/// The sentence a LAUNCH earns for a level above the documented maximum that a mode still
/// runs: the mode to set, and the level the request asked for.
fn mode_sentence(level: i64) -> String {
    let (_, high) = BAND;
    format!(
        "StepConcurrencyLevel {level} exceeds the maximum ({high}) for StepExecutionMode \
         STANDARD. Set StepExecutionMode to DISTRIBUTED for up to {WIDEST_CEILING} concurrent \
         steps."
    )
}

#[tokio::test]
async fn the_refusal_of_a_level_outside_the_band_states_the_band_it_enforces() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("band-sentences")).await;
    let (low, high) = BAND;

    // Each row: the level, the whole sentence a LAUNCH earns, and the whole sentence a
    // MODIFICATION earns.
    let rows: Vec<(i64, String, &str)> = vec![
        (low - 1, BAND_SENTENCE.to_string(), BARE_SENTENCE),
        (low - 2, BAND_SENTENCE.to_string(), BARE_SENTENCE),
        (-7, BAND_SENTENCE.to_string(), BARE_SENTENCE),
        (
            i64::from(i32::MIN),
            BAND_SENTENCE.to_string(),
            BARE_SENTENCE,
        ),
        (high + 1, mode_sentence(high + 1), BAND_SENTENCE),
        (high + 44, mode_sentence(high + 44), BAND_SENTENCE),
        (
            WIDEST_CEILING,
            mode_sentence(WIDEST_CEILING),
            BAND_SENTENCE,
        ),
        (
            WIDEST_CEILING + 1,
            BAND_SENTENCE.to_string(),
            BARE_SENTENCE,
        ),
        (99_999, BAND_SENTENCE.to_string(), BARE_SENTENCE),
        (
            i64::from(i32::MAX),
            BAND_SENTENCE.to_string(),
            BARE_SENTENCE,
        ),
    ];

    for (level, launch_says, modification_says) in &rows {
        let level = *level;
        let case = format!("RunJobFlow with StepConcurrencyLevel {level}");
        let mut request = minimal_request("band-sentences");
        request["StepConcurrencyLevel"] = json!(level);
        let (status, payload) = emr.call("RunJobFlow", request).await;
        assert_refused(&case, status, &payload, REQUEST_VALIDATION);
        assert_eq!(
            &message(&case, &payload),
            launch_says,
            "case {case:?}: expected {launch_says:?}, got {payload}"
        );

        let case = format!("ModifyCluster with StepConcurrencyLevel {level}");
        let (status, payload) = emr
            .call(
                "ModifyCluster",
                json!({ "ClusterId": &cluster, "StepConcurrencyLevel": level }),
            )
            .await;
        assert_refused(&case, status, &payload, SERVICE_ERROR);
        assert_eq!(
            message(&case, &payload),
            *modification_says,
            "case {case:?}: expected {modification_says:?}, got {payload}"
        );
    }

    // The control: every level the band names IS taken, so the sentences above are about
    // the band rather than about the member — and the one the cluster ends up holding is
    // the last level it accepted.
    for level in [low, low + 1, (low + high) / 2, high] {
        let case = format!("ModifyCluster with StepConcurrencyLevel {level}");
        emr.ok(
            "ModifyCluster",
            json!({ "ClusterId": &cluster, "StepConcurrencyLevel": level }),
        )
        .await;
        let described = emr.describe_cluster(&cluster).await;
        assert_eq!(
            described["StepConcurrencyLevel"].as_i64(),
            Some(level),
            "case {case:?}: an accepted level must read back, got {described}"
        );
    }
}
