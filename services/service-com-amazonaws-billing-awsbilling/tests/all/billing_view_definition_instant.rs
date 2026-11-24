// SPDX-License-Identifier: CC0-1.0

//! When `UpdateBillingView` moves `viewDefinitionLastUpdatedAt`, end to end over
//! the awsJson1_0 wire.
//!
//! One rule: an update moves the instant that records a change to the view
//! definition only when it CHANGES that definition — a request carrying the
//! value the stored view already holds, or carrying no definition member at all,
//! leaves that instant exactly where it stood. So a read-modify-write that
//! resubmits the filter it just read, and a retry of an update that already
//! landed, both report the instant of the call that really changed the
//! definition.
//!
//! The rows span what the rule generalises over: which kind of filter the
//! expression carries (the three the model's `dataFilterExpression` doc says a
//! billing view supports — `LINKED_ACCOUNT`, `Tags` and `CostCategories`),
//! whether the view was created with a filter at all, and how the filter changes
//! (a key, a value, the size of a value list, the kind of filter, and a return to
//! a filter the view held earlier). Every row walks the same sequence: each
//! expression is sent, then sent AGAIN unchanged, then followed by an update
//! carrying no expression — so each row asserts both directions of the rule.
//!
//! The harness (`call`, `emulator`, …) is the one `billing_views.rs` owns.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{call, emulator, ACCOUNT_ID, PRIMARY_ARN};

/// A `LINKED_ACCOUNT` dimension filter over `accounts` — the one `Dimension`
/// value this surface accepts (`ops::types::DIMENSION_VALUES`).
fn dimension_filter(accounts: &[&str]) -> Value {
    json!({ "dimensions": { "key": "LINKED_ACCOUNT", "values": accounts } })
}

/// A tag filter on `key` over `values`.
fn tag_filter(key: &str, values: &[&str]) -> Value {
    json!({ "tags": { "key": key, "values": values } })
}

/// A cost-category filter on `key` over `values`.
fn cost_category_filter(key: &str, values: &[&str]) -> Value {
    json!({ "costCategories": { "key": key, "values": values } })
}

/// One row: the expression the view is created with (`None` for a view created
/// without a filter), then the expressions successive updates send.
struct Row {
    case: &'static str,
    created: Option<Value>,
    updates: Vec<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "created without a filter, then dimension filters",
            created: None,
            updates: vec![
                dimension_filter(&[ACCOUNT_ID]),
                dimension_filter(&[ACCOUNT_ID, "111122223333"]),
                dimension_filter(&[ACCOUNT_ID]),
            ],
        },
        Row {
            case: "created with a dimension filter, then tag filters",
            created: Some(dimension_filter(&[ACCOUNT_ID])),
            updates: vec![
                // The expression the create already stored: a no-op from the
                // very first update.
                dimension_filter(&[ACCOUNT_ID]),
                tag_filter("costCenter", &["a"]),
                tag_filter("costCenter", &["a", "b"]),
                tag_filter("team", &["a", "b"]),
            ],
        },
        Row {
            case: "created with a tag filter, then a cost-category filter and back",
            created: Some(tag_filter("team", &["finance"])),
            updates: vec![
                cost_category_filter("Project", &["alpha", "beta"]),
                cost_category_filter("Project", &["alpha"]),
                tag_filter("team", &["finance"]),
            ],
        },
        Row {
            case: "created with a cost-category filter, values shrink then grow",
            created: Some(cost_category_filter("Project", &["alpha", "beta"])),
            updates: vec![
                cost_category_filter("Project", &["alpha", "beta"]),
                cost_category_filter("Project", &["alpha"]),
                cost_category_filter("Project", &["alpha", "beta", "gamma"]),
            ],
        },
    ]
}

/// The projection `GetBillingView` reports for `arn`.
async fn view_of(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, got) = call(router, "GetBillingView", &json!({ "arn": arn })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetBillingView to answer 200 for {arn}, got {status} {got:?}"
    );
    got["billingView"].clone()
}

/// Send one `UpdateBillingView` carrying `members` beside the ARN and hand back
/// the view as a later reader sees it.
async fn update(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    mut members: Value,
) -> (Value, Value) {
    members["arn"] = json!(arn);
    let (status, updated) = call(router, "UpdateBillingView", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateBillingView to answer 200, got {status} {updated:?}"
    );
    let stored = view_of(router, case, arn).await;
    (updated, stored)
}

#[tokio::test]
async fn an_update_moves_the_definition_instant_only_when_the_definition_changes() {
    let router = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let mut create = json!({
            "name": "definition-instant",
            "sourceViews": [PRIMARY_ARN],
            "clientToken": format!("di-{index}"),
        });
        if let Some(expression) = &row.created {
            create["dataFilterExpression"] = expression.clone();
        }
        let (status, created) = call(&router, "CreateBillingView", &create).await;
        assert_eq!(status, 200, "case {case:?}: create failed: {created:?}");
        let arn = created["arn"].as_str().unwrap_or_default().to_string();

        // A create records no definition instant, so every assertion below
        // starts from an absent one.
        let view = view_of(&router, case, &arn).await;
        assert!(
            view["viewDefinitionLastUpdatedAt"].is_null(),
            "case {case:?}: a created view carries no viewDefinitionLastUpdatedAt, got {:?}",
            view["viewDefinitionLastUpdatedAt"]
        );
        let mut held = row.created.clone();
        let mut expected_instant = Value::Null;

        for (step, expression) in row.updates.iter().enumerate() {
            // Each expression three ways: as sent, re-sent unchanged, and
            // followed by an update that carries no definition member.
            for attempt in ["sent", "re-sent unchanged"] {
                let step_case = format!("{case} / step {step} {attempt}");
                let changes = held.as_ref() != Some(expression);
                let (_, stored) = update(
                    &router,
                    &step_case,
                    &arn,
                    json!({ "dataFilterExpression": expression.clone() }),
                )
                .await;
                let instant = stored["viewDefinitionLastUpdatedAt"].clone();
                if changes {
                    assert!(
                        instant.is_number(),
                        "case {step_case:?}: a changed definition stamps the instant, got {instant:?}"
                    );
                    assert_ne!(
                        instant, expected_instant,
                        "case {step_case:?}: a changed definition moves the instant off {expected_instant:?}, got {instant:?}"
                    );
                    expected_instant = instant.clone();
                } else {
                    assert_eq!(
                        instant, expected_instant,
                        "case {step_case:?}: an unchanged definition leaves the instant at {expected_instant:?}, got {instant:?}"
                    );
                }
                assert_eq!(
                    &stored["dataFilterExpression"],
                    expression,
                    "case {step_case:?}: expected the stored filter to be {expression:?}, got {:?}",
                    stored["dataFilterExpression"]
                );
                assert!(
                    stored["updatedAt"].is_number(),
                    "case {step_case:?}: every update stamps updatedAt, got {:?}",
                    stored["updatedAt"]
                );
                held = Some(expression.clone());
            }

            // An update carrying no definition member at all touches neither the
            // filter nor the instant that records a change to it.
            let step_case = format!("{case} / step {step} name only");
            let (_, stored) = update(
                &router,
                &step_case,
                &arn,
                json!({ "name": format!("renamed-{step}") }),
            )
            .await;
            assert_eq!(
                stored["viewDefinitionLastUpdatedAt"], expected_instant,
                "case {step_case:?}: an update with no definition member leaves the instant at {expected_instant:?}, got {:?}",
                stored["viewDefinitionLastUpdatedAt"]
            );
            assert_eq!(
                &stored["dataFilterExpression"],
                expression,
                "case {step_case:?}: expected the stored filter to stay {expression:?}, got {:?}",
                stored["dataFilterExpression"]
            );
        }
    }
}
