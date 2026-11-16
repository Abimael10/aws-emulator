// SPDX-License-Identifier: CC0-1.0

//! What a BURST of requests addressing two stages of ONE deployment at the same time is
//! answered with, and what a grant added for a stage that was refused does to that stage's
//! next request.
//!
//! The rule under test, one level above the recorded case:
//!
//! > API Gateway decides EVERY request to a proxy integration on its own, out of the
//! > function's resource policy as it stands when that request arrives, judged against the
//! > ARN of the method that request reached — which names the stage it came through. So
//! > requests travelling through different stages of one deployment at the same time are
//! > decided independently and each is ANSWERED: a stage the policy admits is served by the
//! > function's own run, and a stage it does not is answered through the api's
//! > `API_CONFIGURATION_ERROR` gateway response (`500` with
//! > `{"message":"Internal server error"}`) with the function never invoked and the refusal
//! > recorded in that stage's execution log. Nothing about one request's decision is kept for
//! > the next: a grant added for a stage that was refused admits that stage's next request,
//! > with the rest of the wiring untouched.
//!
//! The suite beside this one ([`crate::stage_scoped_lambda_permission`]) drives those stages
//! ONE REQUEST AT A TIME, with every grant in place before the request it decides. This one
//! drives the two legs CONCURRENTLY, as a client comparing two stages of one deployment sends
//! them, and then changes the policy BETWEEN two bursts — the two things that reading one
//! sequential leg at a time cannot show: that a leg of a burst is answered at all rather than
//! left waiting, and that a refusal is a decision about one request rather than a state the
//! stage is left in.
//!
//! The rows differ along what the rule generalises over: WHICH of the two stages the function's
//! policy admits when the burst arrives — neither, each one alone, and both.
//!
//! The wiring, the grants, the invoke and the per-leg check are that suite's, so the two
//! cannot disagree about either half of the differential. Only the run accounting is this
//! suite's own: the legs of a burst are in flight together, so the runs they log are counted
//! over the burst as a whole.

use std::sync::Arc;
use std::time::Duration;

use smithy_server_runtime::MultiServiceRouter;

use crate::stage_scoped_lambda_permission::{
    emulator, grant, invoke, judged_leg, revoke, runs_logged, wired, Answered, FIRST_STAGE, REGION,
    SECOND_STAGE,
};

/// The verb the method is put on: `ANY`, the catch-all that serves the `GET` each leg below
/// carries.
const VERB: &str = "ANY";

/// How long ONE leg of a burst is given to be answered before this suite calls it unanswered.
///
/// It bounds nothing the rule is about — an answer is computed in process, with no clock-owed
/// transition to wait for — so it is far longer than a loaded machine needs and tight for
/// nothing; what it pins is that a leg of a burst RETURNS a terminal answer rather than leaving
/// its caller waiting, which is the one outcome a client comparing two stages cannot act on.
const ANSWERED_WITHIN: Duration = Duration::from_secs(30);

/// How often a reading this suite waits for is looked at again (the runs a burst logged).
const LOOK: Duration = Duration::from_millis(100);

/// One row: which of the two stages the function's policy admits when the burst arrives.
struct Row {
    case: &'static str,
    /// Whether the function is granted the invocation for [`FIRST_STAGE`]'s method.
    grant_first: bool,
    /// Whether the function is granted the invocation for [`SECOND_STAGE`]'s method.
    grant_second: bool,
}

/// Every way the two stages of one deployment can be covered by the grants on their function.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a burst through two stages the function grants nothing for",
            grant_first: false,
            grant_second: false,
        },
        Row {
            case: "a burst through two stages, the first of them granted",
            grant_first: true,
            grant_second: false,
        },
        Row {
            case: "a burst through two stages, the second of them granted",
            grant_first: false,
            grant_second: true,
        },
        Row {
            case: "a burst through two stages, each of them granted",
            grant_first: true,
            grant_second: true,
        },
    ]
}

/// The statement id a grant for `stage` is written under, so a row can take back exactly the
/// grants it added.
fn statement(stage: &str) -> String {
    format!("apigateway-invoke-{stage}")
}

/// Grant the function the invocation of `stage`'s method and nothing else: a `SourceArn`
/// naming that stage of this api, over every verb and path under it.
async fn grant_stage(router: &MultiServiceRouter, case: &str, api: &str, stage: &str) {
    let source_arn = format!(
        "arn:aws:execute-api:{REGION}:{account}:{api}/{stage}/*/*",
        account = engine::emulator_account_id(),
    );
    grant(router, case, &statement(stage), &source_arn).await;
}

/// Both legs of one burst, issued CONCURRENTLY: one request through each stage of the
/// deployment, each carrying its own marker.
///
/// Each leg is a task of its own, so neither waits for the other to be answered first, and
/// each is given [`ANSWERED_WITHIN`] to answer at all.
async fn burst(
    router: &Arc<MultiServiceRouter>,
    case: &str,
    api: &str,
    markers: (&str, &str),
) -> (Answered, Answered) {
    let legs = [(FIRST_STAGE, markers.0), (SECOND_STAGE, markers.1)].map(|(stage, marker)| {
        let router = Arc::clone(router);
        let api = api.to_string();
        let marker = marker.to_string();
        (
            stage,
            tokio::spawn(async move { invoke(&router, &api, stage, &marker).await }),
        )
    });
    let mut answered = Vec::with_capacity(legs.len());
    for (stage, leg) in legs {
        let answer = tokio::time::timeout(ANSWERED_WITHIN, leg)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "case {case:?}: the leg through {stage:?} was not answered within \
                     {ANSWERED_WITHIN:?}"
                )
            })
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the leg through {stage:?} did not run: {error}")
            });
        answered.push(answer);
    }
    let mut legs = answered.into_iter();
    let first = legs.next().expect("the leg through the first stage");
    let second = legs.next().expect("the leg through the second stage");
    (first, second)
}

/// What the function's own group owes a burst whose legs were admitted as `invoked` says: one
/// run per admitted leg, and that leg's marker among the lines.
fn expected_runs(invoked: (bool, bool)) -> (usize, bool, bool) {
    (
        usize::from(invoked.0) + usize::from(invoked.1),
        invoked.0,
        invoked.1,
    )
}

/// What the function's group holds about one burst — the runs it logged beyond `runs_before`,
/// and whether each leg's marker is among the lines — looked at again until it is `expected`.
///
/// The legs of a burst are in flight together, so this LOOKS for the reading the rule owes
/// rather than taking one the instant both answers arrive, and answers the last reading it took
/// once the deadline has passed, for the assertion to report.
async fn runs_of_the_burst(
    router: &MultiServiceRouter,
    markers: &(String, String),
    runs_before: usize,
    expected: (usize, bool, bool),
) -> (usize, bool, bool) {
    let looks = ANSWERED_WITHIN.as_millis() / LOOK.as_millis().max(1);
    let mut last = (0, false, false);
    for _ in 0..looks.max(1) {
        let (runs_after, first_marked) = runs_logged(router, &markers.0).await;
        let (_, second_marked) = runs_logged(router, &markers.1).await;
        last = (runs_after - runs_before, first_marked, second_marked);
        if last == expected {
            return last;
        }
        tokio::time::sleep(LOOK).await;
    }
    last
}

/// Drive ONE burst and assert the whole outcome the rule requires of it: each leg answered on
/// its own stage's decision, the function run once per admitted leg and not at all for the
/// others, and each stage's execution log holding the records for the request it answered.
///
/// `attempt` separates one burst's markers from the markers of the bursts before it, so each
/// leg's own run is identifiable among the runs the function's group already holds.
async fn one_burst(
    router: &Arc<MultiServiceRouter>,
    api: &str,
    case: &str,
    attempt: usize,
    invoked: (bool, bool),
) {
    let markers = (
        format!("b{attempt}-{FIRST_STAGE}"),
        format!("b{attempt}-{SECOND_STAGE}"),
    );
    // The runs the function's group already holds, which is what the runs of THIS burst are
    // counted against. Both legs are in flight together, so the count is taken over the burst.
    let (runs_before, _) = runs_logged(router, &markers.0).await;

    let (first, second) = burst(router, case, api, (&markers.0, &markers.1)).await;

    assert_ne!(
        first.request_id, second.request_id,
        "case {case:?}: each leg of a burst is its own request, so no two legs may be reported \
         under one request id, got {:?} and {:?}",
        first.request_id, second.request_id
    );

    let (runs_after, first_marked, second_marked) =
        runs_of_the_burst(router, &markers, runs_before, expected_runs(invoked)).await;
    assert_eq!(
        (runs_after, first_marked, second_marked),
        expected_runs(invoked),
        "case {case:?}: expected {} run(s) of the function logged by this burst and the markers \
         of its admitted legs present ({}, {}), got {runs_after} run(s) and ({first_marked}, \
         {second_marked})",
        expected_runs(invoked).0,
        invoked.0,
        invoked.1
    );

    judged_leg(router, api, case, FIRST_STAGE, &markers.0, &first, invoked.0).await;
    judged_leg(router, api, case, SECOND_STAGE, &markers.1, &second, invoked.1).await;
}

/// Two stages of one deployment addressed at the same time are each answered on the decision
/// the function's own resource policy gives for the method THAT stage's request reached, and
/// each leg of the burst is answered rather than left waiting.
///
/// The runtime is given a worker per live task — one per leg of the burst and one for the test
/// body that waits on them — so a leg that stopped answering by BLOCKING its worker rather than
/// by parking leaves the deadline in [`burst`] a worker to be polled on, and the suite fails
/// instead of hanging on the very symptom it is here to catch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_through_two_stages_answers_each_leg_on_its_own_stages_grant() {
    let router = Arc::new(emulator());
    let api = wired(&router, VERB).await;

    for (attempt, row) in rows().into_iter().enumerate() {
        let case = row.case;
        for (stage, granted) in [
            (FIRST_STAGE, row.grant_first),
            (SECOND_STAGE, row.grant_second),
        ] {
            if granted {
                grant_stage(&router, case, &api, stage).await;
            }
        }

        one_burst(
            &router,
            &api,
            case,
            attempt,
            (row.grant_first, row.grant_second),
        )
        .await;

        for (stage, granted) in [
            (FIRST_STAGE, row.grant_first),
            (SECOND_STAGE, row.grant_second),
        ] {
            if granted {
                revoke(&router, case, &statement(stage)).await;
            }
        }
    }
}

/// A refusal is a decision about ONE request, not a state the stage is left in: adding the
/// grant the refused stage needed admits its NEXT request, and the stage that was already
/// admitted keeps being served.
///
/// The runtime is sized as the test above's is, and for the same reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grant_added_for_a_refused_stage_admits_that_stages_next_request() {
    let router = Arc::new(emulator());
    let api = wired(&router, VERB).await;

    // One grant, naming the first stage: the first leg of the burst is served, the second is
    // refused.
    let scoped = "a grant naming the first stage only";
    grant_stage(&router, scoped, &api, FIRST_STAGE).await;
    one_burst(&router, &api, scoped, 0, (true, false)).await;

    // The grant the refused stage needed, added with nothing else touched.
    let widened = "a grant added for the stage that was refused";
    grant_stage(&router, widened, &api, SECOND_STAGE).await;
    one_burst(&router, &api, widened, 1, (true, true)).await;
}
