// SPDX-License-Identifier: CC0-1.0

//! Which refusal an `UpdateBillingView` earns for the view it identifies by its
//! own `arn`, end to end over the awsJson1_0 wire.
//!
//! One rule, over two tables: a request naming a resource by ARN that the
//! caller's own account holds nothing under is refused with the modelled
//! `ResourceNotFoundException` (404) wherever the operation declares one, in the
//! single form this crate answers for that member, while an eligibility refusal
//! — a `ValidationException` (400) — is reserved for a resource the account DOES
//! hold and may not act on. The rows vary the ARN's resource id, the update's
//! member set and the stored view's `BillingViewType`, so neither answer is
//! keyed to one recorded request.
//!
//! The harness (`call`, `emulator_with_engine`, `plant_view`, …) is the one
//! `billing_views.rs` owns.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::billing_views::{
    call, emulator_with_engine, error_code, plant_view, ACCOUNT_ID, PRIMARY_ARN,
};
use crate::rejection_fidelity::message;

/// The `resourceType` every billing-view not-found payload of this service
/// reports (`state::BILLING_VIEW_RESOURCE_TYPE`).
const BILLING_VIEW_RESOURCE_TYPE: &str = "billingview";

/// Resource ids of own-account ARNs no view is stored under, spanning the shape
/// of the modelled `BillingViewArn` resource id:
/// `[a-zA-Z0-9/:_\+=\.\-@]{0,75}[a-zA-Z0-9]`, as
/// `smithy_query(query="operation", operation="UpdateBillingView")` reports the
/// `arn` member's pattern. None of these is the id the issue cited.
fn absent_resource_ids() -> Vec<String> {
    vec![
        "a".to_string(),
        "custom-4242".to_string(),
        "nested/path:segment_1".to_string(),
        "every.allowed+punctuation=is-here_@ok".to_string(),
        // The longest id the pattern admits: 75 body characters plus the final
        // alphanumeric one.
        format!("{}z", "x".repeat(75)),
    ]
}

/// A well-formed `Expression` for the `dataFilterExpression` member: billing
/// views support `LINKED_ACCOUNT`, tags and cost categories.
fn filter_expression() -> Value {
    json!({
        "dimensions": { "key": "LINKED_ACCOUNT", "values": [ACCOUNT_ID] },
        "timeRange": { "beginDateInclusive": 1, "endDateInclusive": 2 },
    })
}

/// The member sets an update can carry beside the `arn`, including the request
/// that carries nothing else at all.
fn update_member_sets() -> Vec<(&'static str, Value)> {
    vec![
        ("the arn alone", json!({})),
        ("a name", json!({ "name": "renamed" })),
        ("a description", json!({ "description": "mine now" })),
        (
            "a data filter expression",
            json!({ "dataFilterExpression": filter_expression() }),
        ),
        (
            "every member at once",
            json!({
                "name": "renamed",
                "description": "mine now",
                "dataFilterExpression": filter_expression(),
            }),
        ),
    ]
}

/// Build an update body from the ARN and the members a row carries beside it.
fn update_body(arn: &str, members: &Value) -> Value {
    let mut body = members.clone();
    body["arn"] = json!(arn);
    body
}

/// The ARNs `ListBillingViews` reports, in the order it reports them — the
/// control that a refused update stored nothing.
async fn listed_arns(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, listed) = call(router, "ListBillingViews", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the control list must answer 200, got {status} {listed:?}"
    );
    listed["billingViews"]
        .as_array()
        .map(|views| {
            views
                .iter()
                .map(|view| view["arn"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Create one `CUSTOM` view through the API and hand back its ARN.
async fn create_custom_view(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateBillingView",
        &json!({ "name": name, "sourceViews": [PRIMARY_ARN], "clientToken": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: the harness create must answer 200, got {status} {created:?}"
    );
    created["arn"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn an_absent_own_account_arn_is_the_modeled_not_found_whatever_the_update_asks() {
    let (router, _engine) = emulator_with_engine();
    let before = listed_arns(&router, "before the refusals").await;

    for resource_id in absent_resource_ids() {
        let arn = format!("arn:aws:billing::{ACCOUNT_ID}:billingview/{resource_id}");
        // The read that resolves the same member is the reference answer: the
        // refusal has ONE form, so the writer's payload is the reader's.
        let (status, read) = call(&router, "GetBillingView", &json!({ "arn": arn })).await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (404, "ResourceNotFoundException"),
            "case {arn:?}: the reference read must be refused 404, got {status} {read:?}"
        );

        for (members_case, members) in update_member_sets() {
            let case = format!("{members_case} on {arn}");
            let body = update_body(&arn, &members);
            let (status, body) = call(&router, "UpdateBillingView", &body).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (404, "ResourceNotFoundException"),
                "case {case:?}: expected 404 ResourceNotFoundException, got {status} {body:?}"
            );
            assert_eq!(
                message(&body),
                format!("[{arn}] is an invalid billing view arn"),
                "case {case:?}: expected this service's ARN-validation sentence, got {:?}",
                message(&body)
            );
            assert_eq!(
                (body["resourceId"].as_str(), body["resourceType"].as_str()),
                (Some(arn.as_str()), Some(BILLING_VIEW_RESOURCE_TYPE)),
                "case {case:?}: the payload reports the unresolved ARN and its \
                 resource type, got {body:?}"
            );
            for member in ["message", "resourceId", "resourceType"] {
                assert_eq!(
                    body[member], read[member],
                    "case {case:?}: the writer's refusal is the reader's for {member}, \
                     expected {:?}, got {:?}",
                    read[member], body[member]
                );
            }
        }
    }

    let after = listed_arns(&router, "after the refusals").await;
    assert_eq!(
        after, before,
        "case \"after the refusals\": a refused update stores nothing"
    );
}

#[tokio::test]
async fn a_stored_view_the_account_may_not_edit_is_ineligible_rather_than_absent() {
    let (router, engine) = emulator_with_engine();

    // Every `BillingViewType` the model declares beside `CUSTOM` names a view the
    // service maintains. Source: `smithy_query(query="shape",
    // shape="BillingViewType")` — PRIMARY, BILLING_GROUP, CUSTOM,
    // BILLING_TRANSFER, BILLING_TRANSFER_SHOWBACK.
    let managed_types = [
        "BILLING_GROUP",
        "BILLING_TRANSFER",
        "BILLING_TRANSFER_SHOWBACK",
    ];
    let mut stored: Vec<(String, String)> = vec![("PRIMARY".to_string(), PRIMARY_ARN.to_string())];
    for view_type in managed_types {
        let arn = format!(
            "arn:aws:billing::{ACCOUNT_ID}:billingview/{}",
            view_type.to_lowercase().replace('_', "-")
        );
        plant_managed_view(&engine, &arn, view_type);
        stored.push((view_type.to_string(), arn));
    }

    for (view_type, arn) in &stored {
        // It EXISTS: the read resolves it, so the refusal below is about the kind
        // of view rather than about a missing one.
        let (status, read) = call(&router, "GetBillingView", &json!({ "arn": arn })).await;
        assert_eq!(
            (status, read["billingView"]["billingViewType"].as_str()),
            (200, Some(view_type.as_str())),
            "case {view_type:?}: the planted view must read back, got {status} {read:?}"
        );

        for (members_case, members) in update_member_sets() {
            let case = format!("{members_case} on a stored {view_type} view");
            let (status, body) =
                call(&router, "UpdateBillingView", &update_body(arn, &members)).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {body:?}"
            );
            assert!(
                message(&body).contains("Custom Billing View"),
                "case {case:?}: the refusal states the eligibility rule, got {:?}",
                message(&body)
            );
        }
    }

    // The positive control: a stored CUSTOM view takes the same request and the
    // change reads back, so neither refusal is swallowing a legal update.
    let custom_arn = create_custom_view(&router, "editable-view").await;
    let (status, updated) = call(
        &router,
        "UpdateBillingView",
        &update_body(&custom_arn, &json!({ "name": "edited" })),
    )
    .await;
    assert_eq!(
        (status, updated["arn"].as_str()),
        (200, Some(custom_arn.as_str())),
        "case \"update a CUSTOM view\": got {status} {updated:?}"
    );
    let (status, read) = call(&router, "GetBillingView", &json!({ "arn": custom_arn })).await;
    assert_eq!(
        (status, read["billingView"]["name"].as_str()),
        (200, Some("edited")),
        "case \"update a CUSTOM view\": the edit reads back, got {status} {read:?}"
    );
}

/// Plant one view of `view_type` under `arn`, as the service itself would hold a
/// view it manages: `CreateBillingView` mints `CUSTOM` views only.
fn plant_managed_view(engine: &Arc<engine::InMemoryEngine>, arn: &str, view_type: &str) {
    plant_view(engine, arn, |view| {
        view.billing_view_type = view_type.to_string();
        view.name = format!("{view_type} view");
        view.source_views.clear();
        view.tags.clear();
    });
}
