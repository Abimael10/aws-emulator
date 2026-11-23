// SPDX-License-Identifier: CC0-1.0

//! WHICH plane refused a `StepConcurrencyLevel` decides HOW Amazon EMR words the refusal,
//! end to end over the wire.
//!
//! THE RULE: a level outside the band the member documents falls into one of two classes —
//! above the documented maximum but still a level a step execution mode runs, or outside
//! what any mode runs — and the sentence a caller is told is a property of the OPERATION
//! that judged the member as well as of the class. The LAUNCH (`RunJobFlow`) answers the
//! class a mode could still run by NAMING that mode and the level it was asked for, and the
//! class no mode could run by stating the band it enforces. The MODIFICATION
//! (`ModifyCluster`) answers the first class by stating that same band, and the second with
//! a BARE sentence that states no band at all. So the two planes never word one class the
//! same way, and neither plane words its two classes the same way.
//!
//! The suite reads what each plane says for every level of the partition and asserts the
//! invariants the rule makes — each plane answers exactly two distinct sentences, the band
//! the member is told is the band that is enforced, the band-stating sentence is the bare
//! one extended, and only the launch's mode-naming sentence carries the level the request
//! asked for. Rows walk both sides of the band, each published bound, one past it, and far
//! past both; a level the band takes is the positive control, launched or applied and read
//! back through `DescribeCluster`.

use std::collections::BTreeSet;

use serde_json::json;

use crate::job_flow_lifecycle::{assert_refused, kept_alive_request, minimal_request, Emr};
use crate::notebook_executions::REQUEST_VALIDATION;
use crate::refusal_messages::message;

/// The error identity `ModifyCluster` MODELS for a bad request
/// (`smithy_query(query="operation", operation="ModifyCluster")`).
const MODELLED_CLIENT_ERROR: &str = "InvalidRequestException";

/// The band the member's own documentation publishes: "The default value is 1. The maximum
/// value is 256." (`RunJobFlow$StepConcurrencyLevel` doc) / "You can specify a minimum of 1
/// step and a maximum of 256 steps." (`ModifyCluster$StepConcurrencyLevel` doc.)
const MIN_LEVEL: i64 = 1;
const MAX_LEVEL: i64 = 256;

/// The most concurrent steps any step execution mode runs, which the service holds as
/// `state::MAX_DISTRIBUTED_STEP_CONCURRENCY_LEVEL`: the ceiling the launch's own refusal
/// publishes, and the class boundary both planes turn at.
const MAX_MODE_SERVED_LEVEL: i64 = 6400;

/// The sentence that states no band at all.
const BARE: &str = "Invalid step concurrency level.";

/// The sentence that states the band the member may use.
const WITH_BAND: &str = "Invalid step concurrency level. Allowed level is from 1 to 256";

/// Every level of the partition OUTSIDE the band, each with whether a step execution mode
/// could still run it: below the minimum, just above the maximum, each end of the range a
/// mode runs, and far past both ends.
fn out_of_band_levels() -> Vec<(i64, bool)> {
    vec![
        (i64::from(i32::MIN), false),
        (-1, false),
        (MIN_LEVEL - 1, false),
        (MAX_LEVEL + 1, true),
        (MAX_LEVEL + 2, true),
        (MAX_MODE_SERVED_LEVEL - 1, true),
        (MAX_MODE_SERVED_LEVEL, true),
        (MAX_MODE_SERVED_LEVEL + 1, false),
        (10_000, false),
        (i64::from(i32::MAX), false),
    ]
}

/// The levels the band takes, as the positive control: each published bound and one
/// between them.
fn accepted_levels() -> [i64; 3] {
    [MIN_LEVEL, (MIN_LEVEL + MAX_LEVEL) / 2, MAX_LEVEL]
}

/// The sentence the LAUNCH plane refuses `level` with, read off the wire.
async fn launch_refusal(emr: &Emr, level: i64) -> String {
    let case = format!("RunJobFlow with StepConcurrencyLevel {level}");
    let mut request = minimal_request("refusal planes");
    request["StepConcurrencyLevel"] = json!(level);
    let (status, payload) = emr.call("RunJobFlow", request).await;
    // The member carries no range in the model, so a launch is refused through the
    // request-validation channel rather than a modelled error.
    assert_refused(&case, status, &payload, REQUEST_VALIDATION);
    message(&case, &payload)
}

/// The sentence the MODIFICATION plane refuses `level` with, read off the wire.
async fn modification_refusal(emr: &Emr, cluster: &str, level: i64) -> String {
    let case = format!("ModifyCluster with StepConcurrencyLevel {level}");
    let (status, payload) = emr
        .call(
            "ModifyCluster",
            json!({ "ClusterId": cluster, "StepConcurrencyLevel": level }),
        )
        .await;
    assert_refused(&case, status, &payload, MODELLED_CLIENT_ERROR);
    let said = message(&case, &payload);
    assert!(
        !said.contains(&level.to_string()),
        "case {case:?}: this plane's sentences are fixed text, so {level} must not appear in \
         {said:?}"
    );
    said
}

#[tokio::test]
async fn the_launch_plane_names_the_mode_for_a_level_a_mode_runs_and_the_band_for_any_other() {
    let emr = Emr::new();

    let mut said: BTreeSet<String> = BTreeSet::new();
    for (level, a_mode_runs_it) in out_of_band_levels() {
        let case = format!("RunJobFlow with StepConcurrencyLevel {level}");
        let sentence = launch_refusal(&emr, level).await;
        said.insert(sentence.clone());

        if a_mode_runs_it {
            // The actionable class: the caller is told the mode to set and the ceiling that
            // mode runs, around the level it asked for.
            assert!(
                sentence.contains(&level.to_string()),
                "case {case:?}: the level the caller is being told how to get must appear in \
                 {sentence:?}"
            );
            assert!(
                sentence.contains(&MAX_LEVEL.to_string())
                    && sentence.contains(&MAX_MODE_SERVED_LEVEL.to_string()),
                "case {case:?}: the sentence must publish the maximum {MAX_LEVEL} and the \
                 ceiling {MAX_MODE_SERVED_LEVEL}, got {sentence:?}"
            );
            assert!(
                !sentence.starts_with(BARE),
                "case {case:?}: this class is not the band-stating refusal, got {sentence:?}"
            );
        } else {
            // No mode runs it, so there is nothing to switch to: the caller is told the
            // band, in fixed text that carries nothing of the request.
            assert_eq!(
                sentence, WITH_BAND,
                "case {case:?}: expected {WITH_BAND:?}, got {sentence:?}"
            );
            assert!(
                !sentence.contains(&level.to_string()),
                "case {case:?}: the band-stating sentence is fixed text, so {level} must not \
                 appear in {sentence:?}"
            );
        }
    }

    // One sentence per class, and the band-stating one is shared by every level of its
    // class: the mode-naming class carries a level each, so the distinct count is its rows
    // plus the one band-stating sentence.
    let mode_naming_rows = out_of_band_levels()
        .into_iter()
        .filter(|(_, a_mode_runs_it)| *a_mode_runs_it)
        .count();
    assert_eq!(
        said.len(),
        mode_naming_rows + 1,
        "the launch answers one band-stating sentence for its whole class and one \
         mode-naming sentence per level, got {said:?}"
    );

    // The positive control: a level the band takes launches a cluster that reports it.
    for level in accepted_levels() {
        let case = format!("RunJobFlow with StepConcurrencyLevel {level}");
        let mut request = minimal_request("refusal planes");
        request["StepConcurrencyLevel"] = json!(level);
        let (status, payload) = emr.call("RunJobFlow", request).await;
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
            "case {case:?}: the launched cluster must report its level, got {described}"
        );
    }
}

#[tokio::test]
async fn neither_plane_words_a_class_the_way_the_other_does() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("refusal planes")).await;

    let mut modification_sentences: BTreeSet<String> = BTreeSet::new();
    for (level, a_mode_runs_it) in out_of_band_levels() {
        let case = format!("StepConcurrencyLevel {level}");
        let by_launch = launch_refusal(&emr, level).await;
        let by_modification = modification_refusal(&emr, &cluster, level).await;
        modification_sentences.insert(by_modification.clone());

        assert_ne!(
            by_modification, by_launch,
            "case {case:?}: the two planes word this class differently, both said \
             {by_launch:?}"
        );
        if a_mode_runs_it {
            // The modification states the band here; the launch names the mode instead.
            assert_eq!(
                by_modification, WITH_BAND,
                "case {case:?}: expected {WITH_BAND:?}, got {by_modification:?}"
            );
        } else {
            // The modification answers bare here, and the launch's band-stating sentence is
            // that same sentence extended.
            assert_eq!(
                by_modification, BARE,
                "case {case:?}: expected {BARE:?}, got {by_modification:?}"
            );
            assert!(
                by_launch.starts_with(&by_modification),
                "case {case:?}: the band-stating sentence must extend the bare one, got \
                 {by_launch:?} against {by_modification:?}"
            );
        }

        // The level the cluster holds is the one its launch left it on: a refused
        // modification changes nothing.
        let described = emr.describe_cluster(&cluster).await;
        assert_eq!(
            described["StepConcurrencyLevel"].as_i64(),
            Some(MIN_LEVEL),
            "case {case:?}: a refused modification must leave the level alone, got \
             {described}"
        );
    }

    let answered: Vec<&String> = modification_sentences.iter().collect();
    assert_eq!(
        answered.len(),
        2,
        "the modification plane answers two sentences across the partition, got {answered:?}"
    );
    assert!(
        modification_sentences.contains(BARE) && modification_sentences.contains(WITH_BAND),
        "the two sentences are {BARE:?} and {WITH_BAND:?}, got {answered:?}"
    );

    // The positive control on this plane too: a level the band takes is applied and read
    // back, so the refusals above are about the class rather than about the member.
    for level in accepted_levels() {
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
