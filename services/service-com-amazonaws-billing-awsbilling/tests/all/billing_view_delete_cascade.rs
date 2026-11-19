// SPDX-License-Identifier: CC0-1.0

//! What a `DeleteBillingView` leaves behind, end to end over the awsJson1_0
//! wire.
//!
//! One rule: a deleted billing view is referenced nowhere in the store
//! afterwards — not by its own record, not by any record the store keys by that
//! view's ARN, and not by the source-view set of any view that drew from it. So
//! no read reports an association that names a view the store no longer holds
//! (the state `AssociateSourceViews` refuses to write), every other source a
//! view held stays where it was, the per-view source slot the deleted view
//! occupied is free for another association, and a create/delete cycle hands
//! back every bounded slot it took — an ARN is minted once and never again, so
//! a record left under a deleted one could never be observed or freed.
//!
//! The rows span what the rule generalises over: how many source views the
//! drawing view holds (1 through 10, the `BillingViewSourceViewsList` `@length`
//! maximum), where in that stored order the deleted view sits (only, first,
//! middle, last), how many views draw from the deleted one, and — for the
//! per-view records the store keys by an ARN, the billing-view segment timeline
//! — how many segments that timeline holds, how they are bounded, which
//! `BillingDomain` each carries and whether one is hidden.
//!
//! The harness (`call`, `error_code`, …) is the one `billing_views.rs` owns and
//! the view creation is `source_view_disassociation.rs`'s.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::{
    BillingViewSegment, BILLING_DOMAIN_BILLABLE, BILLING_DOMAIN_PRO_FORMA,
};
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{call, emulator_with_engine, error_code, ABSENT_ARN, PRIMARY_ARN};
use crate::source_view_disassociation::create_view;

/// One row: a view created holding `held` real source views, `derived` views
/// drawing from all of them, and the stored position of the source that is then
/// deleted.
struct Row {
    case: &'static str,
    held: usize,
    delete_index: usize,
    derived: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the only source view",
            held: 1,
            delete_index: 0,
            derived: 1,
        },
        Row {
            case: "the first of three",
            held: 3,
            delete_index: 0,
            derived: 1,
        },
        Row {
            case: "the middle of three, two views drawing from it",
            held: 3,
            delete_index: 1,
            derived: 2,
        },
        Row {
            case: "the last of three",
            held: 3,
            delete_index: 2,
            derived: 1,
        },
        Row {
            case: "one of ten, the modeled maximum, three views drawing from it",
            held: 10,
            delete_index: 4,
            derived: 3,
        },
        Row {
            case: "the first of two",
            held: 2,
            delete_index: 0,
            derived: 2,
        },
    ]
}

/// The source views the STORE holds for `arn`, in order, read back through the
/// list API. No `maxResults` is sent and a view holds at most ten sources, well
/// under the page size this read serves by default, so the whole set is one
/// page.
async fn listed_sources(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let (status, listed) =
        call(router, "ListSourceViewsForBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the source-view list to answer 200, got {status} {listed:?}"
    );
    listed
        .get("sourceViews")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The persisted projection of `arn`, as `GetBillingView` reports it.
async fn view_of(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetBillingView to answer 200 for {arn}, got {status} {got:?}"
    );
    got["billingView"].clone()
}

/// Whether `view`'s projection carries no `updatedAt` — a view no write has
/// touched since it was created.
fn never_updated(view: &Value) -> bool {
    view.get("updatedAt").is_none_or(Value::is_null)
        && view
            .get("viewDefinitionLastUpdatedAt")
            .is_none_or(Value::is_null)
}

#[tokio::test]
async fn a_forced_delete_leaves_no_view_drawing_from_the_deleted_one() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        // Every source is a view this store really holds, in the order the
        // drawing views are created with.
        let mut sources = Vec::new();
        for slot in 0..row.held {
            sources.push(
                create_view(
                    &router,
                    &engine,
                    case,
                    &format!("casc-{index}-src-{slot}"),
                    &[PRIMARY_ARN.to_string()],
                )
                .await,
            );
        }
        let mut drawing = Vec::new();
        for nth in 0..row.derived {
            drawing.push(
                create_view(
                    &router,
                    &engine,
                    case,
                    &format!("casc-{index}-derived-{nth}"),
                    &sources,
                )
                .await,
            );
        }

        let target = sources[row.delete_index].clone();
        let expected: Vec<String> = sources
            .iter()
            .filter(|source| **source != target)
            .cloned()
            .collect();

        // Before the delete: every drawing view holds the target, and none of
        // them has been written since it was created.
        for arn in &drawing {
            let listed = listed_sources(&router, case, arn).await;
            assert_eq!(
                listed, sources,
                "case {case:?}: expected the view to be created holding every source, got {listed:?}"
            );
            let view = view_of(&router, case, arn).await;
            assert!(
                never_updated(&view),
                "case {case:?}: a freshly created view carries no update instant, got {view:?}"
            );
        }

        let (status, forced) = call(
            &router,
            "DeleteBillingView",
            &json!({ "arn": target, "force": true }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the forced delete to answer 200, got {status} {forced:?}"
        );

        let (status, gone) = call(&router, "GetBillingView", &json!({ "arn": target })).await;
        assert_eq!(
            (status, error_code(&gone).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: the deleted view must be gone, got {status} {gone:?}"
        );

        for arn in &drawing {
            let listed = listed_sources(&router, case, arn).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected the sources it held minus the deleted one, got {listed:?}"
            );
            // Every ARN a read reports resolves to a view the store holds: one
            // read of this service cannot contradict another.
            for source in &listed {
                let (status, resolved) =
                    call(&router, "GetBillingView", &json!({ "arn": source })).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the reported source {source} must resolve, got {status} {resolved:?}"
                );
            }
            let view = view_of(&router, case, arn).await;
            assert_eq!(
                view["sourceViewCount"].as_u64(),
                Some(expected.len() as u64),
                "case {case:?}: sourceViewCount counts only the sources that remain, got {:?}",
                view["sourceViewCount"]
            );
            // The source set is part of the view definition, and it changed, so
            // both instants a definition change moves are now reported.
            assert!(
                view["updatedAt"].is_number() && view["viewDefinitionLastUpdatedAt"].is_number(),
                "case {case:?}: a view whose source set changed reports both instants, got {view:?}"
            );
        }

        // The views the delete did not name keep their own definition, their
        // derived count and their instants.
        for kept in &expected {
            let view = view_of(&router, case, kept).await;
            assert_eq!(
                view["derivedViewCount"].as_u64(),
                Some(row.derived as u64),
                "case {case:?}: {kept} is still drawn from by every drawing view, got {:?}",
                view["derivedViewCount"]
            );
            assert!(
                never_updated(&view),
                "case {case:?}: a view the delete did not name moves no instant, got {view:?}"
            );
        }

        // The per-view source slot the deleted view occupied is free again: one
        // more real source fits, taking each drawing view back up to the count
        // it was created with — even at the modeled maximum of ten.
        let replacement = create_view(
            &router,
            &engine,
            case,
            &format!("casc-{index}-replacement"),
            &[PRIMARY_ARN.to_string()],
        )
        .await;
        for arn in &drawing {
            let (status, associated) = call(
                &router,
                "AssociateSourceViews",
                &json!({ "arn": arn, "sourceViews": [replacement] }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the slot the deleted view held is free again, got {status} {associated:?}"
            );
            let view = view_of(&router, case, arn).await;
            assert_eq!(
                view["sourceViewCount"].as_u64(),
                Some(row.held as u64),
                "case {case:?}: expected the view back at {} sources, got {:?}",
                row.held,
                view["sourceViewCount"]
            );
        }
    }
}

#[tokio::test]
async fn a_forced_delete_in_a_chain_drops_only_the_deleted_link() {
    let (router, engine) = emulator_with_engine();
    // A three-link chain: `head` draws from `middle`, `middle` draws from
    // `tail`, `tail` draws from the account's PRIMARY view.
    let case = "chain";
    let tail = create_view(&router, &engine, case, "chain-tail", &[PRIMARY_ARN.to_string()]).await;
    let middle =
        create_view(&router, &engine, case, "chain-middle", std::slice::from_ref(&tail)).await;
    let head =
        create_view(&router, &engine, case, "chain-head", std::slice::from_ref(&middle)).await;

    let (status, forced) = call(
        &router,
        "DeleteBillingView",
        &json!({ "arn": middle, "force": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the forced delete of the middle link to answer 200, \
         got {status} {forced:?}"
    );

    let listed = listed_sources(&router, case, &head).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: the head drew only from the deleted link, got {listed:?}"
    );
    assert_eq!(
        view_of(&router, case, &head).await["sourceViewCount"].as_u64(),
        Some(0),
        "case {case:?}: the head holds no source view now"
    );

    // The link BELOW the deleted one is untouched: it keeps its own source and
    // simply has nothing drawing from it any more.
    let listed = listed_sources(&router, case, &tail).await;
    assert_eq!(
        listed,
        vec![PRIMARY_ARN.to_string()],
        "case {case:?}: the tail keeps the source it was created with, got {listed:?}"
    );
    let tail_view = view_of(&router, case, &tail).await;
    assert_eq!(
        tail_view["derivedViewCount"].as_u64(),
        Some(0),
        "case {case:?}: the only view that drew from the tail is gone, got {:?}",
        tail_view["derivedViewCount"]
    );
    assert!(
        never_updated(&tail_view),
        "case {case:?}: the tail's own definition did not change, got {tail_view:?}"
    );
}

#[tokio::test]
async fn only_the_sets_that_named_the_deleted_view_are_rewritten() {
    let (router, engine) = emulator_with_engine();
    let case = "untouched";
    let source = create_view(&router, &engine, case, "ctl-source", &[PRIMARY_ARN.to_string()]).await;
    let drawing =
        create_view(&router, &engine, case, "ctl-drawing", std::slice::from_ref(&source)).await;
    let lone = create_view(&router, &engine, case, "ctl-lone", &[PRIMARY_ARN.to_string()]).await;

    // A view nothing draws from needs no `force`, and deleting it rewrites
    // nobody else's set.
    let (status, deleted) = call(&router, "DeleteBillingView", &json!({ "arn": lone })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to answer 200, got {status} {deleted:?}"
    );

    // And a delete the service REFUSED writes nothing at all: the same view,
    // without `force`, is a conflict.
    let (status, refused) = call(&router, "DeleteBillingView", &json!({ "arn": source })).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (409, "ConflictException"),
        "case {case:?}: a view another view draws from needs force, got {status} {refused:?}"
    );

    let listed = listed_sources(&router, case, &drawing).await;
    assert_eq!(
        listed,
        vec![source.clone()],
        "case {case:?}: an unrelated delete and a refused one leave the set alone, got {listed:?}"
    );
    let view = view_of(&router, case, &drawing).await;
    assert!(
        never_updated(&view),
        "case {case:?}: no instant moves on a view no delete touched, got {view:?}"
    );
    let source_view = view_of(&router, case, &source).await;
    assert_eq!(
        source_view["derivedViewCount"].as_u64(),
        Some(1),
        "case {case:?}: the refused delete left the association in place, got {:?}",
        source_view["derivedViewCount"]
    );
}

// ---------------------------------------------------------------------------
// The per-view records the store keys BY a billing view's ARN: its segment
// timeline, the one `ListBillingViewSegments` reports.
// ---------------------------------------------------------------------------

/// Instants the timelines below are built from: four UTC month boundaries, far
/// enough in the past that the clock cannot move them.
const JANUARY: i64 = 1_735_689_600; // 2025-01-01T00:00:00Z
const FEBRUARY: i64 = 1_738_368_000; // 2025-02-01T00:00:00Z
const MARCH: i64 = 1_740_787_200; // 2025-03-01T00:00:00Z
const APRIL: i64 = 1_743_465_600; // 2025-04-01T00:00:00Z

/// A day, the step the longest timeline below moves its segments by.
const DAY: i64 = 86_400;

/// One segment of a timeline a row records: its own bounds in epoch seconds
/// (`None` for an unbounded side), the `BillingDomain` that applied over it, and
/// whether it is hidden from the caller.
type Segment = (Option<i64>, Option<i64>, &'static str, bool);

/// One row of the timeline table: the shape of the deleted view's timeline, and
/// whether another view draws from it (so the delete needs `force`).
struct TimelineRow {
    case: &'static str,
    segments: Vec<Segment>,
    derived: bool,
}

/// The timeline shapes a delete must reclaim, spanning what the rule
/// generalises over: how many segments the view's timeline holds (none at all
/// through ten), whether a segment is bounded on neither, one or both sides,
/// each published `BillingDomain` value
/// (`smithy_query(query="shape",shape="BillingDomain")` = BILLABLE, PRO_FORMA),
/// whether a segment is hidden from the caller, and whether the delete needed
/// `force` to go through at all.
fn timeline_rows() -> Vec<TimelineRow> {
    vec![
        TimelineRow {
            case: "a view whose timeline was never recorded",
            segments: vec![],
            derived: false,
        },
        TimelineRow {
            case: "one segment in effect, bounded on neither side",
            segments: vec![(None, None, BILLING_DOMAIN_BILLABLE, false)],
            derived: false,
        },
        TimelineRow {
            case: "two segments meeting at a change",
            segments: vec![
                (None, Some(FEBRUARY), BILLING_DOMAIN_BILLABLE, false),
                (Some(FEBRUARY), None, BILLING_DOMAIN_PRO_FORMA, false),
            ],
            derived: false,
        },
        TimelineRow {
            case: "three bounded segments, the middle one hidden, a view drawing from it",
            segments: vec![
                (
                    Some(JANUARY),
                    Some(FEBRUARY),
                    BILLING_DOMAIN_BILLABLE,
                    false,
                ),
                (Some(FEBRUARY), Some(MARCH), BILLING_DOMAIN_BILLABLE, true),
                (Some(MARCH), Some(APRIL), BILLING_DOMAIN_PRO_FORMA, false),
            ],
            derived: true,
        },
        TimelineRow {
            case: "a timeline whose only segment is hidden",
            segments: vec![(Some(JANUARY), None, BILLING_DOMAIN_PRO_FORMA, true)],
            derived: true,
        },
        TimelineRow {
            case: "ten consecutive daily segments",
            segments: (0..10)
                .map(|nth| {
                    (
                        Some(JANUARY + nth * DAY),
                        Some(JANUARY + (nth + 1) * DAY),
                        BILLING_DOMAIN_BILLABLE,
                        false,
                    )
                })
                .collect(),
            derived: false,
        },
    ]
}

/// The timeline of a view no delete below names — what the reclamation must
/// leave exactly where it is.
fn bystander_timeline() -> Vec<Segment> {
    vec![
        (Some(JANUARY), Some(MARCH), BILLING_DOMAIN_BILLABLE, false),
        (Some(MARCH), None, BILLING_DOMAIN_PRO_FORMA, false),
    ]
}

/// Epoch seconds as the instant the store holds.
fn instant(secs: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).expect("a post-epoch instant"))
}

/// Record `segments` as the timeline of the billing view `arn`.
///
/// A segment is "a time range during which the billing domain and account
/// relationships for a billing view remained unchanged", and no modelled
/// operation moves an account into an organization or a billing group, so a
/// timeline is written straight into the shared engine's store — the way the
/// `ListBillingViewSegments` suite writes the one it reads back.
fn record_timeline(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    arn: &str,
    segments: &[Segment],
) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    for (begin, end, domain, hidden) in segments {
        assert!(
            state.record_billing_view_segment(BillingViewSegment {
                begin_date_inclusive: begin.map(instant),
                end_date_exclusive: end.map(instant),
                hidden: *hidden,
                ..BillingViewSegment::in_effect(arn, domain)
            }),
            "case {case:?}: the recorded segment {begin:?}..{end:?} must fit in the store"
        );
    }
}

/// Every segment the store holds, as the key it is stored under beside the
/// billing view its record names.
fn stored_segments(engine: &Arc<engine::InMemoryEngine>) -> Vec<(String, String)> {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let state = state.lock();
    state
        .billing_view_segments
        .iter()
        .map(|(key, segment)| (key.clone(), segment.billing_view_arn.clone()))
        .collect()
}

/// Every stored segment that names a billing view the store no longer holds —
/// the records the rule says cannot exist.
fn segments_naming_no_stored_view(engine: &Arc<engine::InMemoryEngine>) -> Vec<(String, String)> {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let state = state.lock();
    state
        .billing_view_segments
        .iter()
        .filter(|(_, segment)| !state.billing_views.contains_key(&segment.billing_view_arn))
        .map(|(key, segment)| (key.clone(), segment.billing_view_arn.clone()))
        .collect()
}

/// How many segments the store holds for the billing view `arn`.
fn segment_count(engine: &Arc<engine::InMemoryEngine>, arn: &str) -> usize {
    stored_segments(engine)
        .iter()
        .filter(|(_, named)| named == arn)
        .count()
}

#[tokio::test]
async fn a_delete_reclaims_the_deleted_views_whole_segment_timeline() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in timeline_rows().iter().enumerate() {
        let case = row.case;
        let target = create_view(
            &router,
            &engine,
            case,
            &format!("seg-{index}"),
            &[PRIMARY_ARN.to_string()],
        )
        .await;
        // A view no delete names, with a timeline of its own, so what the
        // reclamation must leave alone is more than the account's PRIMARY view.
        let bystander = create_view(
            &router,
            &engine,
            case,
            &format!("seg-{index}-bystander"),
            &[PRIMARY_ARN.to_string()],
        )
        .await;
        record_timeline(&engine, case, &bystander, &bystander_timeline());
        if row.derived {
            create_view(
                &router,
                &engine,
                case,
                &format!("seg-{index}-drawing"),
                std::slice::from_ref(&target),
            )
            .await;
        }

        // The store as it stands before this view has a timeline at all: what
        // the delete must hand back.
        let before = stored_segments(&engine);
        record_timeline(&engine, case, &target, &row.segments);
        let recorded = stored_segments(&engine);
        assert_eq!(
            recorded.len(),
            before.len() + row.segments.len(),
            "case {case:?}: the timeline must be recorded before the delete, got {recorded:?}"
        );

        let (status, deleted) = call(
            &router,
            "DeleteBillingView",
            &json!({ "arn": target, "force": row.derived }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer 200, got {status} {deleted:?}"
        );

        let after = stored_segments(&engine);
        assert_eq!(
            after, before,
            "case {case:?}: the delete reclaims exactly the deleted view's timeline, got {after:?}"
        );
        assert_eq!(
            segment_count(&engine, &target),
            0,
            "case {case:?}: no segment may name the deleted view {target}, got {after:?}"
        );
        assert_eq!(
            segment_count(&engine, &bystander),
            bystander_timeline().len(),
            "case {case:?}: the timeline of a view the delete did not name is untouched, \
             got {after:?}"
        );
        let orphaned = segments_naming_no_stored_view(&engine);
        assert!(
            orphaned.is_empty(),
            "case {case:?}: no stored segment may name a view the store no longer holds, \
             got {orphaned:?}"
        );
    }
}

#[tokio::test]
async fn a_delete_reclaims_only_the_timeline_of_the_arn_it_names() {
    let (router, engine) = emulator_with_engine();

    // Ten views, so the minted resource ids run `custom-1` through `custom-10`
    // and one view's ARN is a strict prefix of another's: what a delete
    // reclaims is the timeline of the ARN the request names, not of every ARN
    // that merely starts with it.
    let case = "an ARN another view's ARN extends";
    let mut created = Vec::new();
    for nth in 0..10 {
        created.push(
            create_view(
                &router,
                &engine,
                case,
                &format!("prefix-{nth}"),
                &[PRIMARY_ARN.to_string()],
            )
            .await,
        );
    }
    for arn in &created {
        record_timeline(&engine, case, arn, &bystander_timeline());
    }

    let target = created[0].clone();
    let extending: Vec<String> = created
        .iter()
        .filter(|arn| **arn != target && arn.starts_with(target.as_str()))
        .cloned()
        .collect();
    assert!(
        !extending.is_empty(),
        "case {case:?}: expected a minted ARN extending {target}, got {created:?}"
    );

    let (status, deleted) = call(&router, "DeleteBillingView", &json!({ "arn": target })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to answer 200, got {status} {deleted:?}"
    );

    assert_eq!(
        segment_count(&engine, &target),
        0,
        "case {case:?}: the named view's timeline is reclaimed, got {:?}",
        stored_segments(&engine)
    );
    for arn in created.iter().skip(1) {
        assert_eq!(
            segment_count(&engine, arn),
            bystander_timeline().len(),
            "case {case:?}: {arn} keeps its own timeline, got {:?}",
            stored_segments(&engine)
        );
    }
}

#[tokio::test]
async fn a_create_record_delete_cycle_frees_the_segment_budget_it_took() {
    let (router, engine) = emulator_with_engine();
    // A billing-view ARN is minted once and never again, so a segment left
    // under a deleted view's ARN would hold its place in the bounded store for
    // good: the resident count after a cycle is what it was before it.
    let case = "a create/record/delete cycle";
    let baseline = stored_segments(&engine).len();

    for cycle in 0..8 {
        let arn = create_view(
            &router,
            &engine,
            case,
            &format!("cycle-{cycle}"),
            &[PRIMARY_ARN.to_string()],
        )
        .await;
        record_timeline(&engine, case, &arn, &timeline_rows()[5].segments);
        let (status, deleted) = call(&router, "DeleteBillingView", &json!({ "arn": arn })).await;
        assert_eq!(
            status, 200,
            "case {case:?} {cycle}: expected the delete to answer 200, got {status} {deleted:?}"
        );
        assert_eq!(
            stored_segments(&engine).len(),
            baseline,
            "case {case:?} {cycle}: the cycle must hand back every segment slot it took, got {:?}",
            stored_segments(&engine)
        );
    }
}

#[tokio::test]
async fn a_refused_delete_reclaims_no_timeline() {
    let (router, engine) = emulator_with_engine();
    let case = "a refused delete";
    let source = create_view(
        &router,
        &engine,
        case,
        "refused-source",
        &[PRIMARY_ARN.to_string()],
    )
    .await;
    create_view(
        &router,
        &engine,
        case,
        "refused-drawing",
        std::slice::from_ref(&source),
    )
    .await;
    record_timeline(&engine, case, &source, &bystander_timeline());
    let before = stored_segments(&engine);

    // Every refusal this operation models for an ARN: a view another view draws
    // from without `force` (409), the account's PRIMARY view and an ARN no view
    // is stored under (400).
    let rows: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a view another view draws from, without force",
            json!({ "arn": source }),
            409,
            "ConflictException",
        ),
        (
            "the account's PRIMARY view",
            json!({ "arn": PRIMARY_ARN }),
            400,
            "ValidationException",
        ),
        (
            "an arn no view is stored under",
            json!({ "arn": ABSENT_ARN }),
            400,
            "ValidationException",
        ),
    ];
    for (row, request, want_status, want_code) in rows {
        let (status, refused) = call(&router, "DeleteBillingView", &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (want_status, want_code),
            "case {row:?}: expected {want_status} {want_code}, got {status} {refused:?}"
        );
        let after = stored_segments(&engine);
        assert_eq!(
            after, before,
            "case {row:?}: a refused delete reclaims nothing, got {after:?}"
        );
    }

    // And the PRIMARY view's own timeline still reads back over the wire, which
    // is the one timeline an API caller can observe.
    let (status, listed) = call(
        &router,
        "ListBillingViewSegments",
        &json!({ "arn": PRIMARY_ARN }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the segment read to answer 200, got {status} {listed:?}"
    );
    assert!(
        listed["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "case {case:?}: the PRIMARY view still reports its own segments, got {listed:?}"
    );
}
