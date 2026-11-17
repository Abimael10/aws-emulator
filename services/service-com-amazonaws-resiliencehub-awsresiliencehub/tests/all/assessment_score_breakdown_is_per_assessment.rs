// SPDX-License-Identifier: CC0-1.0

//! Where an assessment's scoring-component breakdown comes FROM, end to end over
//! HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a
//! restJson1 request — and asserts the response BODY, not just the status.
//!
//! The rule under test: a finished assessment's `resiliencyScore.componentScore`
//! breaks that assessment's own score down over the scoring components the model's
//! `ResiliencyScoreType` enum names — the shares add up to the whole of a score,
//! what the components earn adds up to the score itself, and the compliance
//! component counts the Application Components of the SNAPSHOT THAT ASSESSMENT
//! GRADED that breached the policy. So the breakdown is computed from the assessed
//! version's state at the call and frozen with the rest of the verdict: assessing
//! successive snapshots of one application yields successive breakdowns, a later
//! edit to the application cannot move the breakdown an earlier assessment reports,
//! and re-reading an assessment reports the same breakdown again.
//!
//! The rule itself, and the wire harness that drives it, are the sibling
//! [`super::assessment_score_breakdown`]'s: that suite reads the breakdown of one
//! assessment per application over a table of applications, this one reads the
//! breakdowns of successive assessments of ONE application, so both call the same
//! assertion rather than writing it twice.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehub_awsresiliencehub::state::RELEASE_APP_VERSION;
use smithy_server_runtime::MultiServiceRouter;

use super::assessment_score_breakdown::{
    add_a_component, add_a_resource, assert_the_score_breaks_down, create_a_policy,
    create_an_app, emulator, finish_the_assessment, post, publish, SCORING_COMPONENTS,
    WHOLE_SCORE,
};

/// One scoring component's name beside its four numbers as text, so an entry can be
/// compared without depending on how a double renders.
type ComponentNumbers = (String, String, String, String, String);

/// A whole breakdown as comparable values, in a fixed component order.
type BreakdownNumbers = Vec<ComponentNumbers>;

/// One breakdown as a comparable value: each scoring component's four numbers, in the
/// fixed order the shared component list puts them in. Two breakdowns are the same one
/// exactly when these agree, whatever order the members of the serialized map came out
/// in.
fn breakdown_numbers(breakdown: &Value) -> BreakdownNumbers {
    SCORING_COMPONENTS
        .iter()
        .map(|component| {
            let entry = &breakdown[*component];
            (
                (*component).to_string(),
                entry["score"].to_string(),
                entry["possibleScore"].to_string(),
                entry["outstandingCount"].to_string(),
                entry["excludedCount"].to_string(),
            )
        })
        .collect()
}

/// Start an assessment of the application's release and report it finished; answers
/// the assessment's ARN.
async fn start_and_finish(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    app_arn: &str,
    assessment_name: &str,
) -> String {
    let started = post(
        router,
        "/start-app-assessment",
        &json!({
            "appArn": app_arn,
            "appVersion": RELEASE_APP_VERSION,
            "assessmentName": assessment_name,
        }),
    )
    .await;
    let assessment_arn = started.ok(assessment_name)["assessment"]["assessmentArn"]
        .as_str()
        .expect("a minted assessmentArn")
        .to_string();
    finish_the_assessment(engine, &assessment_arn);
    assessment_arn
}

/// One assessment's `resiliencyScore` as `DescribeAppAssessment` reports it.
async fn read_the_score(router: &MultiServiceRouter, assessment_arn: &str, case: &str) -> Value {
    let read = post(
        router,
        "/describe-app-assessment",
        &json!({"assessmentArn": assessment_arn}),
    )
    .await;
    read.ok(case)["assessment"]["resiliencyScore"].clone()
}

/// A round of the table: what the application's draft gains before this round's
/// publish, and what the assessment of the snapshot that publish freezes therefore
/// reports.
struct Round {
    label: &'static str,
    /// Component ids added to the draft in this round with a resource mapped into
    /// each, so each has something to recover it.
    added_mapped: &'static [&'static str],
    /// Component ids added to the draft in this round with nothing mapped to them —
    /// each one a breach of every objective the policy states.
    added_unmapped: &'static [&'static str],
    /// Component ids already in the draft that this round maps a resource into, so
    /// they stop breaching.
    newly_mapped: &'static [&'static str],
    /// How much of the policy the snapshot this round publishes meets.
    score: f64,
    /// How many of its Application Components breach the policy.
    outstanding: i64,
}

/// The rounds, applied in order to ONE application. They differ along what the rule
/// generalises over: how many Application Components of the assessed snapshot have
/// nothing to recover them (1, then 0, then 2) and therefore what the compliance
/// component has outstanding and earns — so no single breakdown satisfies more than
/// its own round.
fn rounds() -> Vec<Round> {
    vec![
        Round {
            label: "one component mapped, one with nothing mapped to it",
            added_mapped: &["compute-tier"],
            added_unmapped: &["storage-tier"],
            newly_mapped: &[],
            score: 0.0,
            outstanding: 1,
        },
        Round {
            label: "the empty component gets a resource, so every component recovers",
            added_mapped: &[],
            added_unmapped: &[],
            newly_mapped: &["storage-tier"],
            score: WHOLE_SCORE,
            outstanding: 0,
        },
        Round {
            label: "two further components with nothing mapped to them",
            added_mapped: &[],
            added_unmapped: &["queue-tier", "log-tier"],
            newly_mapped: &[],
            score: 0.0,
            outstanding: 2,
        },
    ]
}

/// Successive snapshots of one application yield successive breakdowns, each
/// assessment keeps the breakdown of the snapshot IT graded however the application
/// moves on afterwards, and reading an assessment again reports the same breakdown.
#[tokio::test]
async fn each_assessment_breaks_down_the_snapshot_it_graded() {
    let (router, engine) = emulator();
    // The four objectives of a tightly tiered policy, so every disruption type the
    // model names is judged.
    let policy_arn = create_a_policy(
        &router,
        "scored-policy",
        &json!({
            "Software": {"rtoInSecs": 60, "rpoInSecs": 30},
            "Hardware": {"rtoInSecs": 300, "rpoInSecs": 150},
            "AZ": {"rtoInSecs": 3600, "rpoInSecs": 1800},
            "Region": {"rtoInSecs": 86400, "rpoInSecs": 3600},
        }),
    )
    .await;
    let app_arn = create_an_app(&router, "scored-app", Some(&policy_arn)).await;

    // Every assessment finished so far, with the breakdown it reported when it
    // finished.
    let mut graded: Vec<(String, &'static str, f64, i64, Value)> = Vec::new();

    for (index, round) in rounds().into_iter().enumerate() {
        let case = round.label;
        for component in round.added_mapped {
            add_a_component(&router, &app_arn, component).await;
            add_a_resource(&router, &app_arn, component).await;
        }
        for component in round.added_unmapped {
            add_a_component(&router, &app_arn, component).await;
        }
        for component in round.newly_mapped {
            add_a_resource(&router, &app_arn, component).await;
        }
        publish(&router, &app_arn).await;

        let assessment_arn =
            start_and_finish(&router, &engine, &app_arn, &format!("assessment-{index}")).await;
        let score = read_the_score(&router, &assessment_arn, case).await;
        let breakdown = assert_the_score_breaks_down(case, &score, round.score, round.outstanding);
        graded.push((
            assessment_arn,
            case,
            round.score,
            round.outstanding,
            breakdown,
        ));

        // Every assessment, this round's included, still reports the breakdown of
        // the snapshot it graded: a later publish of a different snapshot does not
        // reach back into a finished verdict, and reading one does not change what
        // the next reader sees.
        for (arn, graded_case, score, outstanding, breakdown) in &graded {
            let re_read = read_the_score(&router, arn, graded_case).await;
            let again = assert_the_score_breaks_down(graded_case, &re_read, *score, *outstanding);
            assert_eq!(
                breakdown_numbers(&again),
                breakdown_numbers(breakdown),
                "case {graded_case:?}: after {case:?} the assessment must still report the \
                 breakdown it finished with {breakdown:?}, got {again:?}"
            );
        }
    }

    // The rounds grade three different snapshots, so the breakdowns they report are
    // three different ones — the breakdown is of the assessed snapshot, not a
    // constant.
    let distinct: BTreeSet<BreakdownNumbers> = graded
        .iter()
        .map(|(_, _, _, _, breakdown)| breakdown_numbers(breakdown))
        .collect();
    assert_eq!(
        distinct.len(),
        graded.len(),
        "expected one breakdown per graded snapshot, got {distinct:?}"
    );
}
