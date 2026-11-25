// SPDX-License-Identifier: CC0-1.0

//! How a billing view's health follows the state it depends on, end to end over
//! the awsJson1_0 wire.
//!
//! One rule: `BillingViewHealthStatus` is not a property a view carries from the
//! moment it is created — it is derived, on every read, from the state the view
//! depends on, so an event that invalidates any of that state moves the view out
//! of `HEALTHY` with the modelled `BillingViewStatusReason` that explains it, the
//! writers the model declares `BillingViewHealthStatusException` for refuse the
//! view while it is there, and the view returns to `HEALTHY` as soon as the
//! condition clears. The state a view depends on is the standing of the account
//! that owns it and the billing views it draws from, so each of those is one
//! class of event:
//!
//! * a change to a source view's DEFINITION, which has to reach the views that
//!   aggregate it (`SOURCE_VIEW_UPDATING`, `UPDATING`);
//! * a source view that is itself not healthy (`SOURCE_VIEW_UNHEALTHY`,
//!   `UNHEALTHY`, and `SOURCE_VIEW_UPDATING` for one that is still settling);
//! * the owning account no longer being the management account of the
//!   organization it belongs to — the one identity `CreateBillingView` admits
//!   (`VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT`, `UNHEALTHY`).
//!
//! The rows span what the rule generalises over: which writer changed the source
//! view's definition, how far down a chain of views the event is read, which
//! status the source carries, and how the organization record stands. Every
//! status and reason value named here is the modelled one —
//! `smithy_query(query="shape",shape="BillingViewStatus,BillingViewStatusReason")`
//! = HEALTHY, UNHEALTHY, CREATING, UPDATING and AGGREGATE_SOURCE,
//! CYCLIC_DEPENDENCY, SOURCE_VIEW_ACCESS_DENIED, SOURCE_VIEW_DEPTH_EXCEEDED,
//! SOURCE_VIEW_NOT_FOUND, SOURCE_VIEW_UNHEALTHY, SOURCE_VIEW_UPDATING,
//! VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT.
//!
//! The harness (`call`, `error_code`, …) is the one `billing_views.rs` owns and
//! the view creation and the store-side health record are
//! `source_view_disassociation.rs`'s.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::SOURCE_VIEW_PROPAGATION;
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{
    call, emulator_with_engine, error_code, set_organization_management_account, ACCOUNT_ID,
    PRIMARY_ARN,
};
use crate::rejection_fidelity::message;
use crate::source_view_disassociation::{create_view, set_health_status, source_arn};

/// The `BillingViewStatus` a view reports while a change it depends on is still
/// being applied.
const UPDATING: &str = "UPDATING";

/// The `BillingViewStatus` a view reports when it cannot serve its data.
const UNHEALTHY: &str = "UNHEALTHY";

/// The `BillingViewStatus` of a view that depends on nothing unsound.
const HEALTHY: &str = "HEALTHY";

/// The `BillingViewStatusReason` for a source view whose definition change has
/// not arrived yet.
const SOURCE_VIEW_UPDATING: &str = "SOURCE_VIEW_UPDATING";

/// The `BillingViewStatusReason` for a source view that is itself unhealthy.
const SOURCE_VIEW_UNHEALTHY: &str = "SOURCE_VIEW_UNHEALTHY";

/// The `BillingViewStatusReason` for a view whose owner is not the management
/// account of the organization it belongs to.
const VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT: &str = "VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT";

/// An account id, in the modelled `^[0-9]{12}$` form, that is not the emulated
/// account — for the worlds in which some OTHER account manages the
/// organization.
const OTHER_ACCOUNT: &str = "111111111111";

/// A second one, so the rows are not about one particular other account.
const THIRD_ACCOUNT: &str = "999999999999";

/// The health a read reports for `arn`: its status code and its reason list.
async fn health_of(router: &MultiServiceRouter, case: &str, arn: &str) -> (String, Vec<String>) {
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetBillingView to answer 200 for {arn}, got {status} {got:?}"
    );
    read_health(&got["billingView"])
}

/// The same health, as the `ListBillingViews` entry for `arn` reports it: the
/// two reads project one view's health from the same derivation, so they must
/// agree.
async fn listed_health_of(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> (String, Vec<String>) {
    let (status, got) = call(router, "ListBillingViews", &json!({ "maxResults": 100 })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListBillingViews to answer 200, got {status} {got:?}"
    );
    let entry = got["billingViews"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: ListBillingViews answered no list: {got:?}"))
        .iter()
        .find(|view| view["arn"].as_str() == Some(arn))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: {arn} is not in the list: {got:?}"));
    read_health(&entry)
}

/// The `healthStatus` of one projected view, as a status code and reason list.
fn read_health(view: &Value) -> (String, Vec<String>) {
    let health = &view["healthStatus"];
    (
        health["statusCode"].as_str().unwrap_or_default().to_string(),
        health["statusReasons"]
            .as_array()
            .map(|reasons| {
                reasons
                    .iter()
                    .map(|reason| reason.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Put the stored instant at which `arn`'s definition last changed `ago` before
/// this reading of the clock.
///
/// It is the very field every writer of a view's definition moves — that those
/// writers move it is `billing_view_definition_instant.rs`'s own rule — so a
/// read measures the propagation span from state a later call would see.
/// [`Duration::ZERO`] re-stamps it as a write just did, which is how an
/// assertion about the span is kept off the race between the clock and however
/// long a loaded host takes over a round trip; a span's worth of it puts the
/// change in the past, so the span has elapsed without the test waiting for the
/// clock.
fn stamp_definition_change(engine: &Arc<engine::InMemoryEngine>, arn: &str, ago: Duration) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    let view = state
        .billing_views
        .get_mut(arn)
        .expect("the harness created this view");
    view.view_definition_last_updated_at = Some(
        SystemTime::now()
            .checked_sub(ago)
            .expect("the clock is well past the span"),
    );
}

/// Long enough ago that the propagation span measured from it has elapsed.
fn well_past_the_span() -> Duration {
    SOURCE_VIEW_PROPAGATION * 4
}

/// One row of the definition-change table: a request that changes the source
/// view's definition, or leaves it alone.
struct Change {
    case: &'static str,
    operation: &'static str,
    /// The request members beside `arn`, built from the source view's own source
    /// views.
    members: fn(&[String]) -> Value,
    /// Whether the request changes the source view's DEFINITION — the set of
    /// views it draws from, or its data filter expression.
    changes_definition: bool,
}

fn changes() -> Vec<Change> {
    vec![
        Change {
            case: "a source view gains a source of its own",
            operation: "AssociateSourceViews",
            members: |_| json!({ "sourceViews": [PRIMARY_ARN] }),
            changes_definition: true,
        },
        Change {
            case: "a source view loses a source of its own",
            operation: "DisassociateSourceViews",
            members: |held| json!({ "sourceViews": [held[0].clone()] }),
            changes_definition: true,
        },
        Change {
            case: "a source view's data filter expression changes",
            operation: "UpdateBillingView",
            members: |_| {
                json!({
                    "dataFilterExpression": {
                        "dimensions": { "key": "LINKED_ACCOUNT", "values": [ACCOUNT_ID] },
                    },
                })
            },
            changes_definition: true,
        },
        Change {
            case: "a source view is renamed, which is no part of its definition",
            operation: "UpdateBillingView",
            members: |_| json!({ "name": "renamed-source" }),
            changes_definition: false,
        },
    ]
}

#[tokio::test]
async fn a_source_view_definition_change_reaches_every_view_down_the_chain() {
    let (router, engine) = emulator_with_engine();

    for (index, change) in changes().iter().enumerate() {
        let case = change.case;
        // A three-link chain: `source` draws from two views of its own, `drawing`
        // draws from `source`, and `far` draws from `drawing` — so the event is
        // read one and two links below where it happened.
        let held = vec![
            source_arn(&format!("prop-{index}-a")),
            source_arn(&format!("prop-{index}-b")),
        ];
        let source =
            create_view(&router, &engine, case, &format!("prop-source-{index}"), &held).await;
        let drawing = create_view(
            &router,
            &engine,
            case,
            &format!("prop-drawing-{index}"),
            std::slice::from_ref(&source),
        )
        .await;
        let far = create_view(
            &router,
            &engine,
            case,
            &format!("prop-far-{index}"),
            std::slice::from_ref(&drawing),
        )
        .await;

        // Before the change every view in the chain depends on nothing unsound.
        for arn in [&source, &drawing, &far] {
            let got = health_of(&router, case, arn).await;
            assert_eq!(
                got,
                (HEALTHY.to_string(), Vec::new()),
                "case {case:?}: expected {arn} to be healthy before the change, got {got:?}"
            );
        }

        let mut request = (change.members)(&held);
        request["arn"] = json!(source);
        let (status, answer) = call(&router, change.operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the change to the source view to answer 200, \
             got {status} {answer:?}"
        );

        let expected = if change.changes_definition {
            // The write stored the instant its change propagates from; it is
            // re-stamped to this reading of the clock so that every read below
            // falls inside the span whatever the host's pace, rather than
            // racing it.
            stamp_definition_change(&engine, &source, Duration::ZERO);
            (UPDATING.to_string(), vec![SOURCE_VIEW_UPDATING.to_string()])
        } else {
            (HEALTHY.to_string(), Vec::new())
        };
        for (what, arn) in [("the drawing view", &drawing), ("the far view", &far)] {
            let got = health_of(&router, case, arn).await;
            assert_eq!(
                got, expected,
                "case {case:?}: expected {what} to report {expected:?}, got {got:?}"
            );
            let listed = listed_health_of(&router, case, arn).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected the list to report {what} as {expected:?}, got {listed:?}"
            );
        }
        // The view the request named answers from its own definition straight
        // away: the write it reported is applied, and it is the views that
        // AGGREGATE it that wait for the change.
        let got = health_of(&router, case, &source).await;
        assert_eq!(
            got,
            (HEALTHY.to_string(), Vec::new()),
            "case {case:?}: the view the write named stays healthy, got {got:?}"
        );

        // Once the span measured from the stored instant has passed, the whole
        // chain is healthy again — nothing had to be written for it.
        stamp_definition_change(&engine, &source, well_past_the_span());
        for (what, arn) in [("the drawing view", &drawing), ("the far view", &far)] {
            let got = health_of(&router, case, arn).await;
            assert_eq!(
                got,
                (HEALTHY.to_string(), Vec::new()),
                "case {case:?}: expected {what} to settle once the span passed, got {got:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_propagation_clears_on_the_clock_alone() {
    // The span is owed by the clock, not by a later call: nothing is written to
    // end it and no request is needed to collect it, so a caller that only ever
    // reads watches the view settle.
    let (router, engine) = emulator_with_engine();
    let case = "the clock alone";
    let source =
        create_view(&router, &engine, case, "clock-source", &[PRIMARY_ARN.to_string()]).await;
    let drawing = create_view(
        &router,
        &engine,
        case,
        "clock-drawing",
        std::slice::from_ref(&source),
    )
    .await;

    let (status, answer) = call(
        &router,
        "DisassociateSourceViews",
        &json!({ "arn": source, "sourceViews": [PRIMARY_ARN] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the definition change to answer 200, got {status} {answer:?}"
    );
    let got = health_of(&router, case, &drawing).await;
    assert_eq!(
        got,
        (UPDATING.to_string(), vec![SOURCE_VIEW_UPDATING.to_string()]),
        "case {case:?}: a caller acting straight after the change sees the transient \
         status, got {got:?}"
    );

    // Wait for the CONDITION, with room for a loaded host: the span is short and
    // the deadline is not.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let (code, reasons) = health_of(&router, case, &drawing).await;
        if code == HEALTHY {
            assert!(
                reasons.is_empty(),
                "case {case:?}: a settled view explains nothing, got {reasons:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the view never settled, last status {code:?} {reasons:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One row of the gate table: an operation the model declares
/// `BillingViewHealthStatusException` for, and the request it refuses while the
/// view it would act on is not healthy.
///
/// The three writers of a stored view are here; `ListBillingViewSegments`, the
/// fourth operation that consults the same status, is not, because its own
/// `arn` "must reference a primary billing view" and the account's `PRIMARY`
/// view depends on nothing that can go unsound — it draws from no view and the
/// account owns it whatever organization it belongs to.
struct Gated {
    case: &'static str,
    operation: &'static str,
    /// The request members beside `arn`, built from the view's one source.
    members: fn(&str) -> Value,
}

fn gated() -> Vec<Gated> {
    vec![
        Gated {
            case: "UpdateBillingView",
            operation: "UpdateBillingView",
            members: |_| json!({ "name": "renamed-while-updating" }),
        },
        Gated {
            case: "AssociateSourceViews",
            operation: "AssociateSourceViews",
            members: |_| json!({ "sourceViews": [PRIMARY_ARN] }),
        },
        Gated {
            case: "DisassociateSourceViews",
            operation: "DisassociateSourceViews",
            members: |source| json!({ "sourceViews": [source] }),
        },
    ]
}

#[tokio::test]
async fn every_gate_fires_while_a_source_view_is_still_propagating() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in gated().iter().enumerate() {
        let case = row.case;
        let source = create_view(
            &router,
            &engine,
            case,
            &format!("gate-source-{index}"),
            &[source_arn(&format!("gate-{index}-held"))],
        )
        .await;
        let drawing = create_view(
            &router,
            &engine,
            case,
            &format!("gate-drawing-{index}"),
            std::slice::from_ref(&source),
        )
        .await;

        // The event: the source view's definition changes, so the view drawing
        // from it is UPDATING until the change arrives.
        let (status, answer) = call(
            &router,
            "AssociateSourceViews",
            &json!({ "arn": source, "sourceViews": [PRIMARY_ARN] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the source-view change to answer 200, got {status} {answer:?}"
        );

        let mut request = (row.members)(&source);
        request["arn"] = json!(drawing);
        // The instant the change propagates from is re-stamped to this reading
        // of the clock, so the refusal below is asserted inside the span
        // whatever the host's pace.
        stamp_definition_change(&engine, &source, Duration::ZERO);
        let (status, refused) = call(&router, row.operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "BillingViewHealthStatusException"),
            "case {case:?}: expected 400 BillingViewHealthStatusException, got {status} {refused:?}"
        );
        let stated = message(&refused);
        assert!(
            stated.contains(UPDATING) && stated.contains(HEALTHY) && stated.contains(&drawing),
            "case {case:?}: the refusal names the view and the status that blocked it, \
             got {stated:?}"
        );

        // The control: the very same request once the change has arrived.
        stamp_definition_change(&engine, &source, well_past_the_span());
        let (status, served) = call(&router, row.operation, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the same request to go through once the view \
             settled, got {status} {served:?}"
        );
    }
}

#[tokio::test]
async fn a_source_view_that_is_not_healthy_propagates_its_condition() {
    // A view is only as sound as what it aggregates: whatever puts a source view
    // out of HEALTHY, the views drawing from it report it — UNHEALTHY for a
    // source that cannot serve its data, UPDATING for one that is still
    // settling.
    let (router, engine) = emulator_with_engine();

    // Every `BillingViewStatus` a source view can carry besides HEALTHY, against
    // the status and reason the views drawing from it must report.
    let rows: Vec<(&str, &str, Vec<String>)> = vec![
        (
            UNHEALTHY,
            UNHEALTHY,
            vec![SOURCE_VIEW_UNHEALTHY.to_string()],
        ),
        ("CREATING", UPDATING, vec![SOURCE_VIEW_UPDATING.to_string()]),
        (UPDATING, UPDATING, vec![SOURCE_VIEW_UPDATING.to_string()]),
    ];

    for (index, (source_status, expected_code, expected_reasons)) in rows.iter().enumerate() {
        let case = format!("a source view at {source_status}");
        let source = create_view(
            &router,
            &engine,
            &case,
            &format!("cond-source-{index}"),
            &[PRIMARY_ARN.to_string()],
        )
        .await;
        let drawing = create_view(
            &router,
            &engine,
            &case,
            &format!("cond-drawing-{index}"),
            std::slice::from_ref(&source),
        )
        .await;
        let far = create_view(
            &router,
            &engine,
            &case,
            &format!("cond-far-{index}"),
            std::slice::from_ref(&drawing),
        )
        .await;

        set_health_status(&engine, &source, source_status);
        let expected = (expected_code.to_string(), expected_reasons.clone());
        for (what, arn) in [("the drawing view", &drawing), ("the far view", &far)] {
            let got = health_of(&router, &case, arn).await;
            assert_eq!(
                got, expected,
                "case {case:?}: expected {what} to report {expected:?}, got {got:?}"
            );
        }
        // And the gate on the drawing view follows the status it reports.
        let (status, refused) = call(
            &router,
            "UpdateBillingView",
            &json!({ "arn": drawing, "name": "renamed-by-propagation" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "BillingViewHealthStatusException"),
            "case {case:?}: expected the write to the drawing view to be refused, \
             got {status} {refused:?}"
        );

        // The control: the same views once the source is healthy again.
        set_health_status(&engine, &source, HEALTHY);
        for (what, arn) in [("the drawing view", &drawing), ("the far view", &far)] {
            let got = health_of(&router, &case, arn).await;
            assert_eq!(
                got,
                (HEALTHY.to_string(), Vec::new()),
                "case {case:?}: expected {what} to recover with its source, got {got:?}"
            );
        }
        let (status, served) = call(
            &router,
            "UpdateBillingView",
            &json!({ "arn": drawing, "name": "renamed-by-propagation" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the write to go through once the source recovered, \
             got {status} {served:?}"
        );
    }
}

#[tokio::test]
async fn a_view_whose_owner_does_not_manage_the_organization_is_unhealthy() {
    // `CreateBillingView` admits one identity — the management account of the
    // organization the account belongs to — so a `CUSTOM` view whose owner no
    // longer holds that standing is a view whose definition nobody may make.
    // The rows span how the Organizations record can stand, and the account's
    // own `PRIMARY` view rides along as the control: a view the service manages
    // for the account depends on no organization at all.
    let (router, engine) = emulator_with_engine();
    let case = "the organization record";
    let source =
        create_view(&router, &engine, case, "owner-source", &[PRIMARY_ARN.to_string()]).await;
    let drawing = create_view(
        &router,
        &engine,
        case,
        "owner-drawing",
        std::slice::from_ref(&source),
    )
    .await;

    let worlds: Vec<(&str, Option<&str>, bool)> = vec![
        ("an organization the owner manages", Some(ACCOUNT_ID), true),
        (
            "an organization another account manages",
            Some(OTHER_ACCOUNT),
            false,
        ),
        ("the owner manages it again", Some(ACCOUNT_ID), true),
        (
            "an organization a third account manages",
            Some(THIRD_ACCOUNT),
            false,
        ),
        // An account in no organization owns its own views: it is nobody's
        // member account, so the reason does not apply to it.
        ("no organization at all", None, true),
    ];

    for (world, management_account, sound) in worlds {
        set_organization_management_account(&engine, management_account);

        let expected_source = if sound {
            (HEALTHY.to_string(), Vec::new())
        } else {
            (
                UNHEALTHY.to_string(),
                vec![VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT.to_string()],
            )
        };
        let got = health_of(&router, world, &source).await;
        assert_eq!(
            got, expected_source,
            "case {world:?}: expected the owned view to report {expected_source:?}, got {got:?}"
        );
        let listed = listed_health_of(&router, world, &source).await;
        assert_eq!(
            listed, expected_source,
            "case {world:?}: expected the list to agree, got {listed:?}"
        );

        // The view drawing from it carries its own condition AND its source's.
        let expected_drawing = if sound {
            (HEALTHY.to_string(), Vec::new())
        } else {
            (
                UNHEALTHY.to_string(),
                vec![
                    SOURCE_VIEW_UNHEALTHY.to_string(),
                    VIEW_OWNER_NOT_MANAGEMENT_ACCOUNT.to_string(),
                ],
            )
        };
        let got = health_of(&router, world, &drawing).await;
        assert_eq!(
            got, expected_drawing,
            "case {world:?}: expected the drawing view to report {expected_drawing:?}, got {got:?}"
        );

        // The account's `PRIMARY` view is one the service manages for it, so no
        // organization record moves it.
        let got = health_of(&router, world, PRIMARY_ARN).await;
        assert_eq!(
            got,
            (HEALTHY.to_string(), Vec::new()),
            "case {world:?}: the default view depends on no organization, got {got:?}"
        );

        // And the writers follow the status: refused while the standing is
        // missing, served once it is back.
        let (status, answer) = call(
            &router,
            "UpdateBillingView",
            &json!({ "arn": source, "name": format!("owner-{}", sound) }),
        )
        .await;
        if sound {
            assert_eq!(
                status, 200,
                "case {world:?}: expected the write to be served, got {status} {answer:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (400, "BillingViewHealthStatusException"),
                "case {world:?}: expected 400 BillingViewHealthStatusException, \
                 got {status} {answer:?}"
            );
            let stated = message(&answer);
            assert!(
                stated.contains(UNHEALTHY) && stated.contains(&source),
                "case {world:?}: the refusal names the view and its status, got {stated:?}"
            );
        }
    }
}
