// SPDX-License-Identifier: CC0-1.0

//! What `ModifyCluster` SAYS when it refuses a `StepConcurrencyLevel`, end to end over the
//! wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends; a cluster's level is only ever read back through
//! `DescribeCluster`.
//!
//! THE RULE: on the MODIFICATION plane the sentence a refused `StepConcurrencyLevel` is
//! answered with is decided by the CLASS the level falls into and by nothing else — not by
//! the level itself, not by what the cluster already holds, and not by the other members the
//! request carries. Both sentences are entirely fixed text: a level above the maximum the
//! member documents that is still no higher than the highest level a band is stated for is
//! told the band it may use, and a level outside every band is answered with the bare
//! sentence that states no band at all. Neither echoes the refused level, the cluster, or any
//! other request member, and a refusal leaves the level the cluster holds alone.
//!
//! The rows below hold the class fixed and VARY everything the rule says the sentence does
//! not depend on: the level inside each class (each published bound, one past it, and far
//! past both), the level the cluster was launched or last modified to, whether the cluster
//! is still coming up or has rested in its ready state, and whether `ExtendedSupport`
//! travels with the refused member.

use serde_json::{json, Value};

use crate::job_flow_lifecycle::{assert_refused, await_cluster_state, kept_alive_request, Emr};
use crate::refusal_messages::message;

/// The error identity `ModifyCluster` MODELS for a bad request, HTTP 400
/// (`smithy_query(query="operation", operation="ModifyCluster")`: errors =
/// `InvalidRequestException` (client, httpError 400), `InternalServerError` (server, 500)).
const MODELLED_CLIENT_ERROR: &str = "InvalidRequestException";

/// The band the member's own documentation publishes: "You can specify a minimum of 1 step
/// and a maximum of 256 steps." (`ModifyCluster$StepConcurrencyLevel` doc, Smithy model.)
const MIN_LEVEL: i64 = 1;
const MAX_LEVEL: i64 = 256;

/// The highest level the band-stating refusal is answered for — the ceiling the service
/// holds as `state::MAX_DISTRIBUTED_STEP_CONCURRENCY_LEVEL`, where this plane turns from the
/// band-stating sentence to the bare one.
const WIDEST_CEILING: i64 = 6400;

/// The sentence a level outside EVERY band is refused with: fixed text, stating no band.
const BARE: &str = "Invalid step concurrency level.";

/// The sentence a level just above the documented maximum is refused with: the bare sentence
/// extended with the clause that states the band the member may use. Fixed text as well —
/// both figures are the band the service enforces, not values from the request.
const WITH_BAND: &str = "Invalid step concurrency level. Allowed level is from 1 to 256";

/// Every refused level, with the sentence its class is answered with: each published bound of
/// both classes, one past each, and far past both.
fn refused_levels() -> Vec<(i64, &'static str)> {
    vec![
        (i64::from(i32::MIN), BARE),
        (-1, BARE),
        (0, BARE),
        (MIN_LEVEL - 1, BARE),
        (MAX_LEVEL + 1, WITH_BAND),
        (MAX_LEVEL + 2, WITH_BAND),
        (1_000, WITH_BAND),
        (WIDEST_CEILING - 1, WITH_BAND),
        (WIDEST_CEILING, WITH_BAND),
        (WIDEST_CEILING + 1, BARE),
        (10_000, BARE),
        (i64::from(i32::MAX), BARE),
    ]
}

/// What a cluster already HOLDS when a refusal reaches it, which the rule says the sentence
/// does not depend on: the level it was launched with, a level it was modified to afterwards,
/// and whether the case waits for it to rest in its ready state before refusing anything.
struct Holding {
    label: &'static str,
    launched_with: i64,
    then_modified_to: Option<i64>,
    wait_until_ready: bool,
}

/// Launch a kept-alive cluster holding `holding`, and answer its id with the level it ends up
/// holding.
async fn cluster_holding(emr: &Emr, holding: &Holding) -> (String, i64) {
    let mut request = kept_alive_request(holding.label);
    request["StepConcurrencyLevel"] = json!(holding.launched_with);
    let cluster_id = emr.run_job_flow(request).await;

    if holding.wait_until_ready {
        // A cluster kept alive with no steps rests in `WAITING`; the helper polls for that
        // condition rather than waiting out a span.
        await_cluster_state(emr, &cluster_id, holding.label, "WAITING").await;
    }

    let mut holds = holding.launched_with;
    if let Some(level) = holding.then_modified_to {
        emr.ok(
            "ModifyCluster",
            json!({ "ClusterId": &cluster_id, "StepConcurrencyLevel": level }),
        )
        .await;
        holds = level;
    }

    let described = emr.describe_cluster(&cluster_id).await;
    assert_eq!(
        described["StepConcurrencyLevel"].as_i64(),
        Some(holds),
        "case {:?}: the cluster must hold the level it was set to, got {described}",
        holding.label
    );
    (cluster_id, holds)
}

#[tokio::test]
async fn the_refusal_sentence_follows_the_class_and_nothing_about_the_cluster_or_the_request() {
    let emr = Emr::new();

    // Rows differ along what the cluster holds and how far through its lifecycle it is.
    let holdings = [
        Holding {
            label: "launched at the band's minimum",
            launched_with: MIN_LEVEL,
            then_modified_to: None,
            wait_until_ready: false,
        },
        Holding {
            label: "launched at the band's maximum",
            launched_with: MAX_LEVEL,
            then_modified_to: None,
            wait_until_ready: false,
        },
        Holding {
            label: "modified inside the band before being refused",
            launched_with: MIN_LEVEL,
            then_modified_to: Some(7),
            wait_until_ready: false,
        },
        Holding {
            label: "rested in its ready state",
            launched_with: 4,
            then_modified_to: None,
            wait_until_ready: true,
        },
    ];

    for holding in &holdings {
        let (cluster_id, holds) = cluster_holding(&emr, holding).await;

        for (row, (level, says)) in refused_levels().into_iter().enumerate() {
            // The other member the operation accepts travels with the refused one on every
            // other row, so the sentence is read with and without it.
            let carries_extended_support = row % 2 == 0;
            let mut request = json!({
                "ClusterId": &cluster_id,
                "StepConcurrencyLevel": level,
            });
            if carries_extended_support {
                request["ExtendedSupport"] = json!(true);
            }
            let case = format!(
                "ModifyCluster StepConcurrencyLevel {level} on a cluster {} \
                 (holding {holds}, ExtendedSupport carried: {carries_extended_support})",
                holding.label
            );

            let (status, payload) = emr.call("ModifyCluster", request).await;
            assert_refused(&case, status, &payload, MODELLED_CLIENT_ERROR);
            let said = message(&case, &payload);
            assert_eq!(
                said, says,
                "case {case:?}: expected {says:?}, got {said:?}"
            );
            // Fixed text: nothing of the request reaches the sentence — neither the level it
            // asked for nor the cluster it named.
            assert!(
                !said.contains(&level.to_string()) && !said.contains(cluster_id.as_str()),
                "case {case:?}: the sentence is fixed text, so neither {level} nor \
                 {cluster_id} may appear in {said:?}"
            );

            // A refused modification changes nothing: neither member is applied.
            let described = emr.describe_cluster(&cluster_id).await;
            assert_eq!(
                described["StepConcurrencyLevel"].as_i64(),
                Some(holds),
                "case {case:?}: a refused modification must leave the level alone, got \
                 {described}"
            );
            assert_ne!(
                described["ExtendedSupport"].as_bool(),
                Some(true),
                "case {case:?}: a refused modification must apply no member of the request, \
                 got {described}"
            );
        }
    }

    // One sentence per class across every row above, and the band-stating one is the bare one
    // extended: the two are not interchangeable and neither is per-request.
    assert!(
        WITH_BAND.starts_with(BARE),
        "the band-stating sentence must extend the bare one, got {WITH_BAND:?} against \
         {BARE:?}"
    );
    assert!(
        WITH_BAND.contains(&MIN_LEVEL.to_string()) && WITH_BAND.contains(&MAX_LEVEL.to_string()),
        "the band a caller is told must be the band that is enforced ({MIN_LEVEL}..\
         {MAX_LEVEL}), got {WITH_BAND:?}"
    );
}

/// The positive control for the same member: a level the documented band takes is APPLIED,
/// echoed by the response, and read back by `DescribeCluster`, so the refusals above are
/// about the class the level falls into rather than about the member. Rows walk both
/// published bounds, one inside each, and a point between them, each carried with and without
/// `ExtendedSupport`.
#[tokio::test]
async fn a_level_the_band_takes_is_applied_echoed_and_read_back() {
    let emr = Emr::new();
    let cluster_id = emr
        .run_job_flow(kept_alive_request("accepted levels"))
        .await;

    for (row, level) in [
        MIN_LEVEL,
        MIN_LEVEL + 1,
        (MIN_LEVEL + MAX_LEVEL) / 2,
        MAX_LEVEL - 1,
        MAX_LEVEL,
    ]
    .into_iter()
    .enumerate()
    {
        let extended_support = row % 2 == 0;
        let case = format!(
            "ModifyCluster StepConcurrencyLevel {level} (ExtendedSupport \
             {extended_support})"
        );
        let answered: Value = emr
            .ok(
                "ModifyCluster",
                json!({
                    "ClusterId": &cluster_id,
                    "StepConcurrencyLevel": level,
                    "ExtendedSupport": extended_support,
                }),
            )
            .await;
        assert_eq!(
            answered["StepConcurrencyLevel"].as_i64(),
            Some(level),
            "case {case:?}: the response must echo the applied level, got {answered}"
        );
        assert_eq!(
            answered["ExtendedSupport"].as_bool(),
            Some(extended_support),
            "case {case:?}: the response must echo the applied setting, got {answered}"
        );

        let described = emr.describe_cluster(&cluster_id).await;
        assert_eq!(
            described["StepConcurrencyLevel"].as_i64(),
            Some(level),
            "case {case:?}: the applied level must read back, got {described}"
        );
    }
}
