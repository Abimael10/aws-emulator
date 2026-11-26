// SPDX-License-Identifier: CC0-1.0

//! The LOWER bound on a billing view's source-view association, end to end over
//! the awsJson1_0 wire.
//!
//! One rule: a view's stored source-view set has to stay inside the modelled
//! `@length` of `BillingViewSourceViewsList` (1..10 — the shape
//! `CreateBillingView$sourceViews`, `AssociateSourceViews$sourceViews`,
//! `DisassociateSourceViews$sourceViews` and the `@required`
//! `ListSourceViewsForBillingView$sourceViews` output all target), so a
//! disassociation that would leave the view drawing from nothing is refused with
//! the modeled `ValidationException` and writes nothing — the same bound
//! `AssociateSourceViews` enforces at its maximum end.
//!
//! Source: `smithy_query(query="shape",
//! shape="BillingViewSourceViewsList,ListSourceViewsForBillingViewResponse")` —
//! `{"type": "list", "length": {"min": 1, "max": 10}}`, and the response member
//! is `required`.
//!
//! The rows span what the rule generalises over: how many sources the view holds
//! (one, two, the modelled maximum of ten), how many of them the request names,
//! the order it names them in, whether it also names a source the view does not
//! hold or names the same one twice, and — on the accepted side — a removal that
//! stops exactly at the minimum and one that removes nothing at all.
//!
//! The one path that still leaves a view drawing from nothing is a FORCED delete
//! of a view another one draws from, which the model documents as breaking the
//! derived resources; that state, and that a request which removes nothing from
//! it is still answered, is the second test here.
//!
//! The harness (`call`, `error_code`, …) is the one `billing_views.rs` owns and
//! the view creation is `source_view_disassociation.rs`'s.

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::MIN_SOURCE_VIEWS_PER_VIEW;
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{call, emulator_with_engine, error_code, PRIMARY_ARN};
use crate::rejection_fidelity::message;
use crate::source_view_disassociation::{create_view, source_arn};

/// The sentence the refusal carries: fixed text for this error class, with the
/// modelled minimum of `BillingViewSourceViewsList` as its only variable part.
fn refusal_sentence() -> String {
    format!("A billing view must draw from at least {MIN_SOURCE_VIEWS_PER_VIEW} source view")
}

/// One row of the table: what the view holds, what the request names, and what
/// the view must hold afterwards — `None` when the request must be refused.
struct Row {
    case: &'static str,
    held: Vec<String>,
    remove: Vec<String>,
    kept: Option<Vec<String>>,
}

fn rows() -> Vec<Row> {
    let ten: Vec<String> = (0..10).map(|i| source_arn(&format!("min-{i}"))).collect();
    vec![
        Row {
            case: "the only source view it holds",
            held: vec![source_arn("single")],
            remove: vec![source_arn("single")],
            kept: None,
        },
        Row {
            case: "both of the two it holds",
            held: vec![source_arn("pair-a"), source_arn("pair-b")],
            remove: vec![source_arn("pair-a"), source_arn("pair-b")],
            kept: None,
        },
        Row {
            case: "both of two, named in the other order",
            held: vec![source_arn("order-a"), source_arn("order-b")],
            remove: vec![source_arn("order-b"), source_arn("order-a")],
            kept: None,
        },
        Row {
            case: "all ten, the modelled maximum a view may hold",
            held: ten.clone(),
            remove: ten.clone(),
            kept: None,
        },
        Row {
            case: "the last one beside a source it does not hold",
            held: vec![source_arn("beside")],
            remove: vec![source_arn("beside"), source_arn("never-associated")],
            kept: None,
        },
        Row {
            case: "the only one, named twice in one request",
            held: vec![source_arn("twice")],
            remove: vec![source_arn("twice"), source_arn("twice")],
            kept: None,
        },
        Row {
            case: "the account's PRIMARY view as the only source",
            held: vec![PRIMARY_ARN.to_string()],
            remove: vec![PRIMARY_ARN.to_string()],
            kept: None,
        },
        Row {
            case: "one of two, stopping at the modelled minimum",
            held: vec![source_arn("keep-a"), source_arn("keep-b")],
            remove: vec![source_arn("keep-a")],
            kept: Some(vec![source_arn("keep-b")]),
        },
        Row {
            case: "nine of ten, stopping at the modelled minimum",
            held: ten.clone(),
            remove: ten[..9].to_vec(),
            kept: Some(vec![ten[9].clone()]),
        },
        Row {
            case: "a source it does not hold removes nothing from a one-source view",
            held: vec![source_arn("untouched")],
            remove: vec![source_arn("never-associated")],
            kept: Some(vec![source_arn("untouched")]),
        },
    ]
}

/// The source views `ListSourceViewsForBillingView` reports for `arn`, the
/// `sourceViewCount` `GetBillingView` derives for the same view, and the two
/// instants a definition change moves.
async fn stored(router: &MultiServiceRouter, case: &str, arn: &str) -> (Vec<String>, u64, Value) {
    let (status, listed) =
        call(router, "ListSourceViewsForBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the list to answer 200, got {status} {listed:?}"
    );
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the get to answer 200, got {status} {got:?}"
    );
    let sources: Vec<String> = listed["sourceViews"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    let view = &got["billingView"];
    (
        sources,
        view["sourceViewCount"].as_u64().unwrap_or(0),
        json!([view["updatedAt"].clone(), view["viewDefinitionLastUpdatedAt"].clone()]),
    )
}

#[tokio::test]
async fn a_disassociation_may_not_take_a_view_below_the_modelled_minimum() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let arn = create_view(&router, &engine, case, &format!("min-{index}"), &row.held).await;
        let before = stored(&router, case, &arn).await;

        let (status, body) = call(
            &router,
            "DisassociateSourceViews",
            &json!({ "arn": arn, "sourceViews": row.remove }),
        )
        .await;

        match &row.kept {
            None => {
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
                );
                assert_eq!(
                    message(&body),
                    refusal_sentence(),
                    "case {case:?}: expected the bound's own sentence, got {:?}",
                    message(&body)
                );
                assert_eq!(
                    (
                        body["reason"].as_str(),
                        body["fieldList"][0]["name"].as_str(),
                        body["fieldList"][0]["message"].as_str(),
                    ),
                    (
                        Some("fieldValidationFailed"),
                        Some("sourceViews"),
                        Some(refusal_sentence().as_str()),
                    ),
                    "case {case:?}: expected the refusal to name sourceViews, got {body:?}"
                );
                let after = stored(&router, case, &arn).await;
                assert_eq!(
                    (after.0.clone(), after.1, after.2.clone()),
                    (
                        row.held.clone(),
                        row.held.len() as u64,
                        before.2.clone()
                    ),
                    "case {case:?}: a refused disassociation writes nothing, got {after:?}"
                );
            }
            Some(kept) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} {body:?}"
                );
                let after = stored(&router, case, &arn).await;
                assert_eq!(
                    (after.0.clone(), after.1),
                    (kept.clone(), kept.len() as u64),
                    "case {case:?}: expected the remaining source views, got {after:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn the_refused_source_view_can_be_removed_once_another_is_associated() {
    // The bound is about what the view is LEFT with, not about which ARN the
    // request names: the very removal that was refused goes through once the
    // view draws from something else.
    let (router, engine) = emulator_with_engine();
    let case = "removed after another is associated";
    let first = source_arn("swap-first");
    let second = source_arn("swap-second");
    let arn = create_view(
        &router,
        &engine,
        case,
        "swap-view",
        std::slice::from_ref(&first),
    )
    .await;

    let (status, body) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": arn, "sourceViews": [first.clone()] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected the last source to be held, got {status} {body:?}"
    );

    // `AssociateSourceViews` resolves every ARN it is given, so the view that
    // joins the set is created first.
    let joined = create_view(
        &router,
        &engine,
        case,
        "swap-joined",
        std::slice::from_ref(&second),
    )
    .await;
    let (status, body) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": arn, "sourceViews": [joined.clone()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association to answer 200, got {status} {body:?}"
    );

    let (status, body) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": arn, "sourceViews": [first.clone()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the same removal to go through now, got {status} {body:?}"
    );
    let (sources, count, _) = stored(&router, case, &arn).await;
    assert_eq!(
        (sources.clone(), count),
        (vec![joined], 1),
        "case {case:?}: expected the newly associated source alone, got {sources:?}"
    );
}

#[tokio::test]
async fn a_forced_delete_still_takes_the_last_source_away_and_a_no_op_removal_is_answered() {
    // "If set to true, forces deletion of the billing view even if it has
    // derived resources": the view that drew from the deleted one loses that
    // source, and what it is left with is nothing. A later disassociation
    // against that view removes nothing, so it is answered rather than refused —
    // the caller of it is not the one that took the set down.
    let (router, engine) = emulator_with_engine();
    let case = "a forced delete of the only source";
    let source = create_view(
        &router,
        &engine,
        case,
        "forced-source",
        &[PRIMARY_ARN.to_string()],
    )
    .await;
    let drawing = create_view(
        &router,
        &engine,
        case,
        "forced-drawing",
        std::slice::from_ref(&source),
    )
    .await;

    let (status, body) = call(
        &router,
        "DeleteBillingView",
        &json!({ "arn": source, "force": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the forced delete to answer 200, got {status} {body:?}"
    );
    let (sources, count, _) = stored(&router, case, &drawing).await;
    assert_eq!(
        (sources.clone(), count),
        (Vec::<String>::new(), 0),
        "case {case:?}: the cascade left the drawing view without its source, got {sources:?}"
    );

    let (status, body) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": drawing, "sourceViews": [source.clone()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a removal that removes nothing is answered, got {status} {body:?}"
    );
    let (sources, count, _) = stored(&router, case, &drawing).await;
    assert_eq!(
        (sources.clone(), count),
        (Vec::<String>::new(), 0),
        "case {case:?}: that call changed nothing, got {sources:?}"
    );

    // And the view is usable again as soon as it draws from something: the bound
    // guards what a CALLER's removal may leave behind, not what the view holds
    // now.
    let (status, body) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": drawing, "sourceViews": [PRIMARY_ARN] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association to answer 200, got {status} {body:?}"
    );
    let (sources, count, _) = stored(&router, case, &drawing).await;
    assert_eq!(
        (sources.clone(), count),
        (vec![PRIMARY_ARN.to_string()], 1),
        "case {case:?}: expected the re-associated source, got {sources:?}"
    );
}
