// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a `StepConcurrencyLevel` outside the band Amazon EMR allows is refused
//! with, end to end over the wire.
//!
//! THE RULE: a level outside the band is refused with the entirely FIXED sentence of the
//! class it falls into — the bare sentence, which states no band at all, for a level
//! outside every band the service serves, and that same sentence EXTENDED with the clause
//! stating the enforced band for a level just above the documented maximum. Neither
//! sentence is the other, neither echoes the level it refused, and neither is a
//! range-checker's prose.
//!
//! The table spans both sides of the band, each published bound and one past it, numbers
//! far past both, and both operations that take the member — `RunJobFlow`, which judges a
//! cluster being launched, and `ModifyCluster`, which judges one already running — so the
//! sentence is a property of the member rather than of one request.

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

/// The highest level the band-stating refusal is answered for; a level past it is outside
/// every band the service serves.
const WIDEST_CEILING: i64 = 6400;

/// The sentence a level outside every band the service serves earns: fixed text that states
/// no band and nothing of the request.
const BARE_SENTENCE: &str = "Invalid step concurrency level.";

/// The sentence a level above the documented maximum, but no higher than the highest level
/// a band is stated for, earns: the bare sentence extended with the enforced band.
const BAND_SENTENCE: &str = "Invalid step concurrency level. Allowed level is from 1 to 256";

#[tokio::test]
async fn the_refusal_of_a_level_outside_the_band_states_the_band_it_enforces() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("band-sentences")).await;
    let (low, high) = BAND;

    // Each row: the level, and the whole sentence it earns.
    let rows: Vec<(i64, &str)> = vec![
        (low - 1, BARE_SENTENCE),
        (low - 2, BARE_SENTENCE),
        (-7, BARE_SENTENCE),
        (i64::from(i32::MIN), BARE_SENTENCE),
        (high + 1, BAND_SENTENCE),
        (high + 44, BAND_SENTENCE),
        (WIDEST_CEILING, BAND_SENTENCE),
        (WIDEST_CEILING + 1, BARE_SENTENCE),
        (99_999, BARE_SENTENCE),
        (i64::from(i32::MAX), BARE_SENTENCE),
    ];

    for &(level, sentence) in &rows {
        let case = format!("RunJobFlow with StepConcurrencyLevel {level}");
        let mut request = minimal_request("band-sentences");
        request["StepConcurrencyLevel"] = json!(level);
        let (status, payload) = emr.call("RunJobFlow", request).await;
        assert_refused(&case, status, &payload, REQUEST_VALIDATION);
        assert_eq!(
            message(&case, &payload),
            sentence,
            "case {case:?}: expected {sentence:?}, got {payload}"
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
            sentence,
            "case {case:?}: expected {sentence:?}, got {payload}"
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
