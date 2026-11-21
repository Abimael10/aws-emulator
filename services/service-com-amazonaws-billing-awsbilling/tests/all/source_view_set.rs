// SPDX-License-Identifier: CC0-1.0

//! A billing view's source views are a SET, end to end over the awsJson1_0 wire.
//!
//! One rule: a view draws from each distinct billing view ONCE, however many
//! times a request names it — `BillingViewElement$sourceViewCount` is "the
//! number of source views associated with this billing view", and naming one
//! view twice associates it once. So every writer of a view's source list (the
//! create that mints a view over its sources and the association that adds one
//! later) stores each distinct ARN once, in the order it was first named, and
//! each read derived from that list — the count, the paged source listing, the
//! per-view source bound — reports one association per distinct source.
//!
//! Sources (the model, not a recording):
//! `smithy_query(query="shape", shape="BillingViewSourceViewsList")` — the shape
//! BOTH writers' `sourceViews` targets is `@length(min:1,max:10)` with NO
//! `@uniqueItems`, so a repeated entry reaches the handler and the rule is the
//! service's; `smithy_query(query="shape", shape="BillingViewElement")` —
//! `sourceViewCount` is "The number of source views associated with this billing
//! view."
//!
//! The rows differ along everything the rule generalises over: how many distinct
//! sources a request names (one, two, three), the multiplicity of a repeat (two,
//! three, ten copies), where the repeats sit (adjacent, interleaved, first,
//! last), which view is repeated (one a suite planted, the account's `PRIMARY`
//! view) and which writer of the field was asked.
//!
//! The harness (`call`, `emulator_with_engine`, `plant_source_views`, …) is the
//! one `billing_views.rs` owns.

use serde_json::{json, Value};

use crate::billing_views::{
    call, emulator_with_engine, error_code, plant_source_views, ACCOUNT_ID, PRIMARY_ARN,
};
use crate::rejection_fidelity::message;

/// A pattern-valid `BillingViewArn` of this account for the resource id
/// `view_id`.
fn view_arn(view_id: &str) -> String {
    format!("arn:aws:billing::{ACCOUNT_ID}:billingview/{view_id}")
}

/// The distinct source views every row draws from, which the store really
/// holds: one more than [`MAX_SOURCE_VIEWS_PER_VIEW`] of them, so a case can
/// fill a view's whole bound with distinct sources and still have one left over.
fn held_sources() -> Vec<String> {
    (0..11)
        .map(|index| view_arn(&format!("set-source-{index}")))
        .collect()
}

/// One row: the `sourceViews` a request names, and the sources the view draws
/// from afterwards — each distinct entry once, in the order it was first named.
struct Row {
    case: &'static str,
    requested: Vec<String>,
    associated: Vec<String>,
}

fn rows() -> Vec<Row> {
    let held = held_sources();
    let (a, b, c) = (held[0].clone(), held[1].clone(), held[2].clone());

    vec![
        Row {
            case: "one source, named once (the control)",
            requested: vec![a.clone()],
            associated: vec![a.clone()],
        },
        Row {
            case: "one source, named twice",
            requested: vec![a.clone(), a.clone()],
            associated: vec![a.clone()],
        },
        Row {
            case: "one source, named three times",
            requested: vec![a.clone(), a.clone(), a.clone()],
            associated: vec![a.clone()],
        },
        Row {
            case: "one source, named ten times — a whole modelled list of copies",
            requested: vec![a.clone(); 10],
            associated: vec![a.clone()],
        },
        Row {
            case: "the account's PRIMARY view, named twice",
            requested: vec![PRIMARY_ARN.to_string(), PRIMARY_ARN.to_string()],
            associated: vec![PRIMARY_ARN.to_string()],
        },
        Row {
            case: "two distinct sources, the first repeated adjacently",
            requested: vec![a.clone(), a.clone(), b.clone()],
            associated: vec![a.clone(), b.clone()],
        },
        Row {
            case: "two distinct sources, the last repeated",
            requested: vec![a.clone(), b.clone(), b.clone()],
            associated: vec![a.clone(), b.clone()],
        },
        Row {
            case: "two distinct sources, interleaved",
            requested: vec![a.clone(), b.clone(), a.clone(), b.clone(), a.clone()],
            associated: vec![a.clone(), b.clone()],
        },
        Row {
            case: "three distinct sources, each named twice",
            requested: vec![
                a.clone(),
                a.clone(),
                b.clone(),
                b.clone(),
                c.clone(),
                c.clone(),
            ],
            associated: vec![a.clone(), b.clone(), c.clone()],
        },
        Row {
            case: "three distinct sources, kept in the order they are FIRST named",
            requested: vec![
                c.clone(),
                b.clone(),
                a.clone(),
                c.clone(),
                b.clone(),
                a.clone(),
            ],
            associated: vec![c.clone(), b.clone(), a.clone()],
        },
        Row {
            case: "three distinct sources, no repeat at all (the control)",
            requested: vec![a.clone(), b.clone(), c.clone()],
            associated: vec![a, b, c],
        },
    ]
}

/// A world holding every source the rows draw from, so a request may name those
/// exact ARNs.
fn world() -> smithy_server_runtime::MultiServiceRouter {
    let (router, engine) = emulator_with_engine();
    plant_source_views(&engine, &held_sources());
    router
}

/// The `sourceViews` an answer carries; an omitted list and `[]` mean the same.
fn source_views(body: &Value) -> Vec<String> {
    body.get("sourceViews")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Every source view the view at `arn` draws from, as
/// `ListSourceViewsForBillingView` reports them, in stored order.
async fn listed_sources(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
    case: &str,
) -> Vec<String> {
    let (status, listed) =
        call(router, "ListSourceViewsForBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the sources to be listed, got {status} {listed:?}"
    );
    source_views(&listed)
}

/// The `sourceViewCount` `GetBillingView` derives for the view at `arn`.
async fn source_view_count(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
    case: &str,
) -> u64 {
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetBillingView to answer 200, got {status} {got:?}"
    );
    got["billingView"]["sourceViewCount"]
        .as_u64()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: sourceViewCount must be a number, got {:?}",
                got["billingView"]["sourceViewCount"]
            )
        })
}

/// Create a view over `sources` and hand back the ARN it was minted under.
async fn create_over(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    sources: &[String],
    case: &str,
) -> String {
    let (status, created) = call(
        router,
        "CreateBillingView",
        &json!({ "name": name, "sourceViews": sources, "clientToken": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateBillingView to answer 200, got {status} {created:?}"
    );
    created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a minted arn, got {created:?}"))
        .to_string()
}

#[tokio::test]
async fn a_create_associates_each_distinct_source_once() {
    let router = world();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let arn = create_over(
            &router,
            &format!("created-set-{index}"),
            &row.requested,
            case,
        )
        .await;

        let listed = listed_sources(&router, &arn, case).await;
        assert_eq!(
            listed, row.associated,
            "case {case:?}: expected each distinct source once, in first-named order, from {:?}, \
             got {listed:?}",
            row.requested
        );

        let count = source_view_count(&router, &arn, case).await;
        assert_eq!(
            count,
            row.associated.len() as u64,
            "case {case:?}: sourceViewCount counts the associated views, expected {}, got {count}",
            row.associated.len()
        );
    }
}

#[tokio::test]
async fn an_association_adds_each_distinct_source_once() {
    // The other writer of the same stored field answers the same request the
    // same way: the rule belongs to the field, not to one operation.
    let router = world();
    let held = held_sources();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        // A target that draws from a source NONE of the rows names, so what the
        // association adds is exactly what the row asked for.
        let target = create_over(
            &router,
            &format!("association-target-{index}"),
            &[held[10].clone()],
            case,
        )
        .await;

        let (status, associated) = call(
            &router,
            "AssociateSourceViews",
            &json!({ "arn": target, "sourceViews": row.requested }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AssociateSourceViews to answer 200, got {status} \
             {associated:?}"
        );

        let mut expected = vec![held[10].clone()];
        expected.extend(row.associated.iter().cloned());
        let listed = listed_sources(&router, &target, case).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected each distinct source added once from {:?}, got {listed:?}",
            row.requested
        );
        assert_eq!(
            source_view_count(&router, &target, case).await,
            expected.len() as u64,
            "case {case:?}: sourceViewCount counts the associated views, expected {}",
            expected.len()
        );
    }
}

#[tokio::test]
async fn associating_a_source_a_created_view_already_names_adds_nothing() {
    // The state a create persists is one its sibling writer agrees with: every
    // source the created view reports is already associated, so associating it
    // again — once, or as many copies as the modelled list takes — changes
    // nothing a later read sees.
    let router = world();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let arn = create_over(
            &router,
            &format!("idempotent-set-{index}"),
            &row.requested,
            case,
        )
        .await;
        let before = listed_sources(&router, &arn, case).await;

        let (status, body) = call(
            &router,
            "AssociateSourceViews",
            &json!({ "arn": arn, "sourceViews": row.requested }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: re-associating the same sources is served, got {status} {body:?}"
        );

        let after = listed_sources(&router, &arn, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a source already associated adds nothing, got {after:?}"
        );
        assert_eq!(
            source_view_count(&router, &arn, case).await,
            before.len() as u64,
            "case {case:?}: sourceViewCount must not grow, expected {}",
            before.len()
        );
    }
}

#[tokio::test]
async fn one_source_named_any_number_of_times_is_one_association() {
    // The invariant over the whole multiplicity the modelled list allows: the
    // same source named 1..=10 times is one association, whichever writer was
    // asked, so neither the stored list nor the count depends on how many copies
    // arrived.
    let router = world();
    let held = held_sources();

    for copies in 1..=10 {
        let case = format!("{copies} copies of one source");
        let requested = vec![held[0].clone(); copies];

        let created = create_over(&router, &format!("copies-{copies}"), &requested, &case).await;
        let listed = listed_sources(&router, &created, &case).await;
        assert_eq!(
            listed,
            vec![held[0].clone()],
            "case {case:?}: expected the one source once from the create, got {listed:?}"
        );
        assert_eq!(
            source_view_count(&router, &created, &case).await,
            1,
            "case {case:?}: one association, whatever the multiplicity"
        );

        let target =
            create_over(&router, &format!("copies-target-{copies}"), &[held[1].clone()], &case)
                .await;
        let (status, body) = call(
            &router,
            "AssociateSourceViews",
            &json!({ "arn": target, "sourceViews": requested }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the association to be served, got {status} {body:?}"
        );
        let listed = listed_sources(&router, &target, &case).await;
        assert_eq!(
            listed,
            vec![held[1].clone(), held[0].clone()],
            "case {case:?}: expected the one source added once, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_per_view_source_bound_counts_distinct_sources() {
    // The bound is on the views a view DRAWS FROM, so copies of one source
    // consume one place in it: a view created from ten copies of one ARN still
    // takes nine more distinct sources, and only the eleventh distinct one is
    // refused with the modeled quota error.
    let router = world();
    let held = held_sources();
    let case = "ten copies of one source, then nine more distinct";

    let arn = create_over(&router, "bounded-by-distinct", &vec![held[0].clone(); 10], case).await;
    assert_eq!(
        source_view_count(&router, &arn, case).await,
        1,
        "case {case:?}: the ten copies are one association"
    );

    let (status, body) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": arn, "sourceViews": held[1..10].to_vec() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: nine more distinct sources fit under the bound, got {status} {body:?}"
    );
    let listed = listed_sources(&router, &arn, case).await;
    assert_eq!(
        listed,
        held[..10].to_vec(),
        "case {case:?}: expected the ten distinct sources, got {listed:?}"
    );

    // One past the bound: the view already draws from ten distinct views.
    let case = "an eleventh distinct source";
    let (status, refused) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": arn, "sourceViews": [held[10].clone()] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (402, "ServiceQuotaExceededException"),
        "case {case:?}: expected the modeled quota refusal, got {status} {refused:?}"
    );
    assert_eq!(
        message(&refused),
        "A billing view can draw from at most 10 source views",
        "case {case:?}: unexpected refusal wording, got {:?}",
        message(&refused)
    );
    let after = listed_sources(&router, &arn, case).await;
    assert_eq!(
        after,
        held[..10].to_vec(),
        "case {case:?}: a refused association changes nothing, got {after:?}"
    );
}
