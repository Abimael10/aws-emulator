// SPDX-License-Identifier: CC0-1.0

//! What the two budget-association refusals SAY, driven as awsJson1_1 requests through the
//! same `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: a budget association IS the (resource, budget) pair, so
//! `DisassociateBudgetFromResource` removes only a pair the store holds and refuses every
//! other pair with the modelled `ResourceNotFoundException` naming both halves of the pair
//! it could not find, while `AssociateBudgetWithResource` refuses a pair already stored
//! with the modelled `DuplicateResourceException` naming the ASSOCIATION on the resource
//! that holds it — whose only variable part is that resource's identifier.
//!
//! Both refusals are modelled with `httpError` 400 and a single `Message` member
//! (`smithy_query query="operation" operation="AssociateBudgetWithResource,DisassociateBudgetFromResource"`:
//! `DuplicateResourceException`, `InvalidParametersException`, `LimitExceededException` and
//! `ResourceNotFoundException` on the associate, `ResourceNotFoundException` alone on the
//! disassociate), so an HTTP status, an error code and a message are what each case asserts.
//!
//! Each row varies what the templates generalise over: the KIND of resource the pair hangs
//! off (`ResourceId` is "either a portfolio-id or a product-id", so each kind is a row), the
//! SHAPE of the budget name that fills the one variable part of the not-found template
//! (`smithy_query query="shape" shape="BudgetName"`: a bare string of length 1..100 with no
//! pattern, so the rows span the minimum length, the maximum length, spaces and
//! punctuation, and non-ASCII text), and WHY the pair is absent (never associated, already
//! disassociated, or associated with a different resource). The expected message of every
//! row is built from that row's own request and the ids the emulator minted, never from a
//! literal captured from one run.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::harness::{call, create_portfolio, create_product, emulator, error_code, message};
use smithy_server_runtime::MultiServiceRouter;

/// The kind of resource a row hangs its associations off. `ResourceId` is "the resource
/// identifier. Either a portfolio-id or a product-id", so both kinds are rows of the same
/// table.
#[derive(Debug, Clone, Copy)]
enum ResourceKind {
    Portfolio,
    Product,
}

/// The budget names the rows draw on, each paired with the case name that reports it.
///
/// `BudgetName` is a bare string of length 1..100 with no pattern, so all five are names
/// AWS Budgets' own naming admits and the refusal interpolates verbatim: the shortest a
/// name may be, a plain one, one carrying spaces and punctuation, one of the maximum
/// modelled length, and one of non-ASCII text.
fn budget_names() -> Vec<(&'static str, String)> {
    vec![
        ("shortest name", "a".to_string()),
        ("plain name", "monthly-cloud-spend".to_string()),
        (
            "spaces and punctuation",
            "Platform Team (prod) $1,000/mo".to_string(),
        ),
        ("longest name", format!("budget-{}", "x".repeat(93))),
        ("non-ascii name", "予算-プラットフォーム".to_string()),
    ]
}

/// Seed one resource of `kind` and answer the id the emulator minted for it.
async fn seed_resource(router: &MultiServiceRouter, kind: ResourceKind, token: &str) -> String {
    match kind {
        ResourceKind::Portfolio => {
            create_portfolio(router, &format!("portfolio-{token}"), "acme", &[], token).await
        }
        ResourceKind::Product => create_product(router, &format!("product-{token}"), token).await,
    }
}

/// Associate one (resource, budget) pair, which must succeed.
async fn associate(router: &MultiServiceRouter, case: &str, resource_id: &str, budget: &str) {
    let (status, body) = call(
        router,
        "AssociateBudgetWithResource",
        &json!({"ResourceId": resource_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: associating {budget:?} with {resource_id} must succeed, got {status} with \
         {body}"
    );
}

/// The budget names `ListBudgetsForResource` reports for `resource_id`.
async fn listed_names(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
) -> BTreeSet<String> {
    let (status, listed) = call(
        router,
        "ListBudgetsForResource",
        &json!({"ResourceId": resource_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListBudgetsForResource({resource_id}) must succeed, got {status} with \
         {listed}"
    );
    listed["Budgets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|budget: &Value| {
            budget["BudgetName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a BudgetDetail has no name: {budget}"))
                .to_string()
        })
        .collect()
}

/// Drive `DisassociateBudgetFromResource` for a pair the store does not hold and assert the
/// modelled refusal, with both variable parts taken from the request this row sent.
async fn assert_pair_not_found(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
    budget: &str,
) {
    let (status, refused) = call(
        router,
        "DisassociateBudgetFromResource",
        &json!({"ResourceId": resource_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a pair the store does not hold must be refused, got {status} with \
         {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {refused}"
    );
    assert_eq!(
        message(&refused),
        format!("Budget with name {budget} is not associated with resource {resource_id}"),
        "case {case:?}: the refusal must name the pair it could not find, got {refused}"
    );
}

#[tokio::test]
async fn a_pair_the_store_does_not_hold_is_refused_whatever_the_budget_name() {
    let router = emulator();

    for (kind, token) in [
        (ResourceKind::Portfolio, "msgp"),
        (ResourceKind::Product, "msgr"),
    ] {
        let resource_id = seed_resource(&router, kind, token).await;
        // A second resource of the same kind, so one row can hold a budget that the row
        // under test does NOT hold: the refusal is keyed on the PAIR, not on the budget
        // name existing somewhere.
        let elsewhere = seed_resource(&router, kind, &format!("{token}2")).await;

        for (row, budget) in budget_names() {
            let case = format!("{kind:?} / {row}");

            // Never associated anywhere.
            assert_pair_not_found(&router, &case, &resource_id, &budget).await;

            // Associated with a DIFFERENT resource of the same kind.
            associate(&router, &case, &elsewhere, &budget).await;
            assert_pair_not_found(&router, &case, &resource_id, &budget).await;

            // Associated with THIS resource: the identical call now succeeds with an
            // empty body, so only the never-associated leg is the refusal.
            associate(&router, &case, &resource_id, &budget).await;
            let (status, body) = call(
                &router,
                "DisassociateBudgetFromResource",
                &json!({"ResourceId": &resource_id, "BudgetName": &budget}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a pair the store holds must be removed, got {status} with {body}"
            );
            assert_eq!(
                body,
                json!({}),
                "case {case:?}: the modelled output carries no member, got {body}"
            );

            // And having been removed, the same pair is the same refusal again.
            assert_pair_not_found(&router, &case, &resource_id, &budget).await;
        }

        // Every refused disassociation left the OTHER resource's associations alone.
        assert_eq!(
            listed_names(&router, "untouched resource", &elsewhere).await,
            budget_names().into_iter().map(|(_, name)| name).collect(),
            "case {kind:?}: a refused disassociation must change nothing"
        );
        assert!(
            listed_names(&router, "emptied resource", &resource_id)
                .await
                .is_empty(),
            "case {kind:?}: every pair removed must leave no budget"
        );
    }
}

#[tokio::test]
async fn a_pair_already_stored_is_the_duplicate_named_on_its_resource() {
    let router = emulator();

    for (kind, token) in [
        (ResourceKind::Portfolio, "dupp"),
        (ResourceKind::Product, "dupr"),
    ] {
        let resource_id = seed_resource(&router, kind, token).await;
        let mut messages = BTreeSet::new();

        for (row, budget) in budget_names() {
            let case = format!("{kind:?} / {row}");
            associate(&router, &case, &resource_id, &budget).await;

            let (status, refused) = call(
                &router,
                "AssociateBudgetWithResource",
                &json!({"ResourceId": &resource_id, "BudgetName": &budget}),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: a pair already stored must be refused, got {status} with {refused}"
            );
            assert_eq!(
                error_code(&refused),
                "DuplicateResourceException",
                "case {case:?}: expected DuplicateResourceException, got {refused}"
            );
            assert_eq!(
                message(&refused),
                format!("BudgetAssociation already exists on resource {resource_id}"),
                "case {case:?}: the duplicate is the association, named on the resource that \
                 holds it, got {refused}"
            );
            messages.insert(message(&refused));
        }

        // The budget name is not a variable part of this template: every row above sent a
        // different one and the resource answered all of them with the one sentence.
        assert_eq!(
            messages.len(),
            1,
            "case {kind:?}: the duplicate refusal must not vary with the budget name, got \
             {messages:?}"
        );

        // The refusals changed nothing: each name is stored exactly once.
        assert_eq!(
            listed_names(&router, "after duplicates", &resource_id).await,
            budget_names().into_iter().map(|(_, name)| name).collect(),
            "case {kind:?}: a refused association must leave each pair stored once"
        );
    }
}

#[tokio::test]
async fn the_duplicate_refusal_names_the_resource_from_the_request() {
    let router = emulator();
    // One budget name reaching two resources: the same pair refused on each resource names
    // THAT resource, so the one variable part comes from the request.
    let budget = "monthly-cloud-spend";
    let portfolio_id = seed_resource(&router, ResourceKind::Portfolio, "twop").await;
    let product_id = seed_resource(&router, ResourceKind::Product, "twor").await;

    for (case, resource_id) in [
        ("shared budget on a portfolio", &portfolio_id),
        ("shared budget on a product", &product_id),
    ] {
        associate(&router, case, resource_id, budget).await;
        let (status, refused) = call(
            &router,
            "AssociateBudgetWithResource",
            &json!({"ResourceId": resource_id, "BudgetName": budget}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a pair already stored must be refused, got {status} with {refused}"
        );
        assert_eq!(
            message(&refused),
            format!("BudgetAssociation already exists on resource {resource_id}"),
            "case {case:?}: the refusal must name this request's resource, got {refused}"
        );
    }

    // The two resources hold independent associations, so disassociating from one leaves
    // the other's pair in place — and the removed pair is then the modelled not-found.
    let (status, body) = call(
        &router,
        "DisassociateBudgetFromResource",
        &json!({"ResourceId": &portfolio_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"shared budget on a portfolio\": the stored pair must be removed, got {status} \
         with {body}"
    );
    assert_pair_not_found(
        &router,
        "shared budget on a portfolio",
        &portfolio_id,
        budget,
    )
    .await;
    assert_eq!(
        listed_names(&router, "shared budget on a product", &product_id).await,
        BTreeSet::from([budget.to_string()]),
        "case \"shared budget on a product\": the other resource's pair must stand"
    );
}
