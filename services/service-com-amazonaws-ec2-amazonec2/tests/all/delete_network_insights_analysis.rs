// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteNetworkInsightsAnalysis`, driven over the wire the service
//! actually speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. the delete TAKES THE ANALYSIS OUT OF THE STORE and answers the id it removed, so the
//!    store holds it no more, the client token that named it stops naming it and the bytes it
//!    held go back to the store's budget — while every other analysis the account holds is
//!    untouched;
//! 2. a read of an analysis the store does not hold is a REFUSAL, not an absent row:
//!    `DescribeNetworkInsightsAnalyses` naming it answers 400
//!    `InvalidNetworkInsightsAnalysisId.NotFound` with the id the request carried followed by
//!    the fixed text " does not exist" — the wording every read of this surface answers an id
//!    it cannot resolve with, whatever the form of that id;
//! 3. the delete, which ACTS on the one analysis it is given, refuses an id it cannot resolve
//!    with EC2's `InvalidParameterValue` naming the member and the value, and removes nothing;
//!    a request carrying no id at all is `MissingParameter`;
//! 4. a `DryRun` reports `DryRunOperation` and removes nothing.
//!
//! Each table row is a distinct input along what the rules generalise over — how many analyses
//! one path carries and which of them is deleted, and which form of unresolvable id is read
//! (one this service minted and then removed, a well-formed id it never minted, a short-form
//! id, the all-zero placeholder a client reaches for) — so no one call is the specification.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, NetworkInsightsPathSpec, DRY_RUN_OPERATION,
    INVALID_NETWORK_INSIGHTS_ANALYSIS_ID_NOT_FOUND, INVALID_PARAMETER_VALUE, MISSING_PARAMETER,
    NETWORK_INSIGHTS_ANALYSIS_ID_PREFIX,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{child_text, document, is_long_id, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because anything was wrong with it.
const DRY_RUN_STATUS: u16 = 412;

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn api(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = api(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// A Reachability Analyzer path of the account, as its id, written through the store's one
/// path-insertion path — what a create stores for the path itself is that operation's own
/// cover.
fn seed_path(engine: &engine::InMemoryEngine) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_network_insights_path(NetworkInsightsPathSpec {
            source: Some("eni-0123456789abcdef0".to_string()),
            destination: Some("eni-0fedcba9876543210".to_string()),
            protocol: Some("tcp".to_string()),
            destination_port: Some(443),
            region: REGION.to_string(),
            ..NetworkInsightsPathSpec::default()
        })
        .expect("the store holds room for the suite's path")
}

/// One analysis started on `path_id` under `client_token`, as the id the start answered.
async fn start(
    case: &str,
    router: &MultiServiceRouter,
    path_id: &str,
    client_token: &str,
) -> String {
    let body = ok(
        case,
        router,
        "StartNetworkInsightsAnalysis",
        &[
            param("NetworkInsightsPathId", path_id),
            param("ClientToken", client_token),
        ],
    )
    .await;
    let started = analysis_ids(case, &body);
    assert_eq!(
        started.len(),
        1,
        "case {case:?}: one start answers one analysis, got {started:?}"
    );
    started.into_iter().next().expect("one analysis")
}

/// Every `networkInsightsAnalysisId` a body reports, in document order.
fn analysis_ids(case: &str, body: &str) -> Vec<String> {
    let doc = document(case, body);
    doc.descendants()
        .filter(|node| node.is_element())
        .filter_map(|node| child_text(node, "networkInsightsAnalysisId"))
        .collect()
}

/// How many analyses the store holds.
fn stored_count(engine: &engine::InMemoryEngine) -> usize {
    engine
        .state::<Ec2State>()
        .lock()
        .network_insights_analyses
        .len()
}

/// The caller-supplied bytes the store currently holds against its budget.
fn stored_bytes(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().stored_bytes()
}

/// One `DeleteNetworkInsightsAnalysis` call.
async fn delete(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    api(router, "DeleteNetworkInsightsAnalysis", params).await
}

/// What `DescribeNetworkInsightsAnalyses` answers for the one id `analysis_id` names.
async fn describe_one(router: &MultiServiceRouter, analysis_id: &str) -> (u16, String) {
    api(
        router,
        "DescribeNetworkInsightsAnalyses",
        &[param("NetworkInsightsAnalysisId.1", analysis_id)],
    )
    .await
}

/// RULE 1 and RULE 2: the delete removes the analysis it names and nothing else, answers the
/// id it removed, and the describe of that id afterwards is a refusal — while the other
/// analyses of the same path are still reported.
#[tokio::test]
async fn a_deleted_analysis_stops_resolving_and_leaves_the_others() {
    // One row per shape of the store the delete acts on: which of the path's analyses is
    // deleted, out of how many.
    let rows: [(&str, usize, usize); 4] = [
        ("the only analysis of the path", 1, 0),
        ("the first of two", 2, 0),
        ("the last of two", 2, 1),
        ("the middle of three", 3, 1),
    ];

    for (case, count, deleted_index) in rows {
        let (router, engine) = super::ec2_query::emulator();
        let path_id = seed_path(&engine);
        let before = stored_bytes(&engine);

        let mut analyses = Vec::new();
        for index in 0..count {
            analyses
                .push(start(case, &router, &path_id, &format!("delete-token-{index}")).await);
        }
        let target = analyses[deleted_index].clone();

        let body = ok(
            case,
            &router,
            "DeleteNetworkInsightsAnalysis",
            &[param("NetworkInsightsAnalysisId", &target)],
        )
        .await;
        assert_eq!(
            analysis_ids(case, &body),
            vec![target.clone()],
            "case {case:?}: the delete must answer the id it removed, got body {body}"
        );
        assert!(
            is_long_id(NETWORK_INSIGHTS_ANALYSIS_ID_PREFIX, &target),
            "case {case:?}: the deleted id must be a long-form analysis id, got {target:?}"
        );
        assert_eq!(
            stored_count(&engine),
            count - 1,
            "case {case:?}: exactly one analysis must have left the store"
        );

        // The deleted id resolves to nothing: a read of it is this resource's own refusal,
        // worded with the id the request carried.
        let (status, body) = describe_one(&router, &target).await;
        assert_eq!(
            status, 400,
            "case {case:?}: reading a deleted analysis must be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_NETWORK_INSIGHTS_ANALYSIS_ID_NOT_FOUND,
            "case {case:?}: expected the analysis's own not-found code, got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("{target} does not exist"),
            "case {case:?}: expected the id followed by the fixed text, got {body}"
        );

        // Every other analysis of the path is still reported, and only those.
        let remaining: Vec<String> = analyses
            .iter()
            .filter(|id| **id != target)
            .cloned()
            .collect();
        let body = ok(
            case,
            &router,
            "DescribeNetworkInsightsAnalyses",
            &[param("NetworkInsightsPathId", &path_id)],
        )
        .await;
        let mut reported = analysis_ids(case, &body);
        reported.sort();
        let mut expected = remaining;
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the analyses that were not deleted, got {reported:?}"
        );

        // With every analysis deleted the store is back where it started: the bytes each
        // record held were given back with it.
        for id in analyses.iter().filter(|id| **id != target) {
            ok(
                case,
                &router,
                "DeleteNetworkInsightsAnalysis",
                &[param("NetworkInsightsAnalysisId", id)],
            )
            .await;
        }
        assert_eq!(
            stored_count(&engine),
            0,
            "case {case:?}: deleting every analysis must leave none in the store"
        );
        assert_eq!(
            stored_bytes(&engine),
            before,
            "case {case:?}: the bytes the analyses held must go back to the store's budget"
        );
    }
}

/// RULE 1: the `ClientToken` that named a deleted analysis stops naming it, so a start
/// carrying that token again begins a NEW analysis rather than reporting one nothing holds.
#[tokio::test]
async fn the_token_of_a_deleted_analysis_starts_a_new_one() {
    let case = "a token reused after the analysis it named was deleted";
    let (router, engine) = super::ec2_query::emulator();
    let path_id = seed_path(&engine);
    let token = "reused-after-delete";

    let first = start(case, &router, &path_id, token).await;
    ok(
        case,
        &router,
        "DeleteNetworkInsightsAnalysis",
        &[param("NetworkInsightsAnalysisId", &first)],
    )
    .await;

    let second = start(case, &router, &path_id, token).await;
    assert_ne!(
        second, first,
        "case {case:?}: the token must name a new analysis, got {second:?} again"
    );
    assert_eq!(
        stored_count(&engine),
        1,
        "case {case:?}: the store holds the new analysis and not the deleted one"
    );
}

/// RULE 2: every id a read cannot resolve is refused the same way, whatever its form — the
/// code and the `<id> does not exist` wording are facts of the condition, not of the id.
#[tokio::test]
async fn every_unresolvable_analysis_id_is_refused_the_same_way() {
    let (router, engine) = super::ec2_query::emulator();
    let path_id = seed_path(&engine);
    let started = start("a seed analysis", &router, &path_id, "refusal-token").await;
    ok(
        "a seed analysis",
        &router,
        "DeleteNetworkInsightsAnalysis",
        &[param("NetworkInsightsAnalysisId", &started)],
    )
    .await;

    // One row per form of id a client may carry: one this service minted and then removed, a
    // well-formed long id it never minted, a short-form id, and the all-zero placeholder.
    let rows: Vec<(&str, String)> = vec![
        ("an analysis this service removed", started),
        (
            "a long-form id never minted",
            "nia-0ffffffffffffffff".to_string(),
        ),
        ("a short-form id", "nia-0fffffff".to_string()),
        (
            "the all-zero placeholder",
            "nia-00000000000000000".to_string(),
        ),
    ];

    for (case, analysis_id) in rows {
        let (status, body) = describe_one(&router, &analysis_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a refusal for {analysis_id:?}, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_NETWORK_INSIGHTS_ANALYSIS_ID_NOT_FOUND,
            "case {case:?}: expected the analysis's own not-found code for {analysis_id:?}, \
             got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("{analysis_id} does not exist"),
            "case {case:?}: expected the id followed by the fixed text, got {body}"
        );
    }
}

/// RULE 3: the delete refuses an id it cannot resolve — whatever its form — with EC2's
/// `InvalidParameterValue` naming the member and the value, and removes nothing; a request
/// carrying no id is `MissingParameter`.
#[tokio::test]
async fn the_delete_refuses_an_id_it_cannot_resolve_and_removes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let path_id = seed_path(&engine);
    let kept = start("a kept analysis", &router, &path_id, "kept-token").await;

    let rows: [(&str, &str); 3] = [
        ("a long-form id never minted", "nia-0ffffffffffffffff"),
        ("a short-form id", "nia-0fffffff"),
        ("the all-zero placeholder", "nia-00000000000000000"),
    ];
    for (case, analysis_id) in rows {
        let (status, body) = delete(&router, &[param("NetworkInsightsAnalysisId", analysis_id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: deleting {analysis_id:?} must be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the acting operation's refusal for {analysis_id:?}, \
             got {body}"
        );
        assert_eq!(
            error_message(&body),
            format!("NetworkInsightsAnalysisId ({analysis_id}) is not valid"),
            "case {case:?}: expected the member and the value, got {body}"
        );
    }

    // A request carrying no id at all names no analysis to delete.
    let (status, body) = delete(&router, &[]).await;
    assert_eq!(
        status, 400,
        "a delete naming no analysis must be refused, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        MISSING_PARAMETER,
        "a delete naming no analysis must be MissingParameter, got {body}"
    );

    assert_eq!(
        stored_count(&engine),
        1,
        "no refusal may remove an analysis the account holds"
    );
    let (status, body) = describe_one(&router, &kept).await;
    assert_eq!(
        (status, analysis_ids("the kept analysis", &body)),
        (200, vec![kept.clone()]),
        "the analysis the account holds must still be reported, got {body}"
    );
}

/// RULE 4: a dry run of the delete reports `DryRunOperation` and removes nothing.
#[tokio::test]
async fn a_dry_run_delete_removes_nothing() {
    let case = "a dry run of the delete";
    let (router, engine) = super::ec2_query::emulator();
    let path_id = seed_path(&engine);
    let analysis_id = start(case, &router, &path_id, "dry-run-token").await;

    let (status, body) = delete(
        &router,
        &[
            param("NetworkInsightsAnalysisId", &analysis_id),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected the dry-run status, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert_eq!(
        stored_count(&engine),
        1,
        "case {case:?}: a dry run must remove nothing"
    );
    let (status, body) = describe_one(&router, &analysis_id).await;
    assert_eq!(
        (status, analysis_ids(case, &body)),
        (200, vec![analysis_id.clone()]),
        "case {case:?}: the analysis must still be reported, got {body}"
    );
}
