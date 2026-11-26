// SPDX-License-Identifier: CC0-1.0

//! What the budget-association operations SAY when their `ResourceId` resolves to no
//! stored resource, driven as awsJson1_1 requests through the same `MultiServiceRouter`
//! the production emulator mounts.
//!
//! The rule under test: an operation that addresses a resource by a `ResourceId` which may
//! name either kind ("either a portfolio-id or a product-id") refuses an identifier no
//! stored portfolio and no stored product answers to with the modelled
//! `ResourceNotFoundException` — HTTP 400, a single `Message` member — whose sentence is
//! the kind that identifier names, the identifier echoed from the request, and nothing
//! else: `"<Kind> <ResourceId> not found."`. The kind is read from the identifier itself —
//! a portfolio id carries the `port-` prefix and every other identifier names a product —
//! never from the identifier happening to be stored somewhere else, so the sentence is the
//! same across every operation that takes the member.
//!
//! The three operations that take it, and the errors each models, are the model's own
//! (`smithy_query query="operation"
//! operation="AssociateBudgetWithResource,DisassociateBudgetFromResource,ListBudgetsForResource"`:
//! `ResourceId` targets `Id`, required, length 1..100, pattern `^[a-zA-Z0-9_\-]*$`, and
//! `ResourceNotFoundException` is modelled on all three with `httpError` 400).
//!
//! Each row varies what the rule generalises over: the LENGTH of the identifier (both
//! modelled bounds), which characters of the modelled pattern it is made of, whether it
//! carries the portfolio prefix — in the exact case the prefix has, in another case, or
//! without its separator — WHY no resource answers to it (never minted, minted and then
//! deleted, or a stored record of a kind that carries no budget), and WHICH of the three
//! operations asks. Every expected sentence is built from the row's own request and the
//! ids the emulator minted, never from a literal captured from one run.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_portfolio, create_product, emulator, error_code, message};

/// The prefix a portfolio id carries, which is what tells a member taking "either a
/// portfolio-id or a product-id" which of the two kinds it was handed.
const PORTFOLIO_PREFIX: &str = "port-";

/// The two kind names a refusal reports, which are the two `ResourceType` values the model
/// documents for the member that narrows a TagOption's resources
/// (`smithy_query query="doc" shape="ListResourcesForTagOptionInput" member="ResourceType"`).
const PORTFOLIO_KIND: &str = "Portfolio";
const PRODUCT_KIND: &str = "Product";

/// The kind the refusal must name for `resource_id`: the prefix is all the request carries
/// to say which of the two kinds the identifier addresses.
fn expected_kind(resource_id: &str) -> &'static str {
    if resource_id.starts_with(PORTFOLIO_PREFIX) {
        PORTFOLIO_KIND
    } else {
        PRODUCT_KIND
    }
}

/// The three operations whose input takes `ResourceId`, each with the other members it
/// requires: the rule is about the member, so every row is asked of all three.
fn resource_id_operations() -> [(&'static str, Value); 3] {
    [
        (
            "AssociateBudgetWithResource",
            json!({"BudgetName": "monthly-cloud-spend"}),
        ),
        (
            "DisassociateBudgetFromResource",
            json!({"BudgetName": "monthly-cloud-spend"}),
        ),
        ("ListBudgetsForResource", json!({})),
    ]
}

/// Assert every operation taking a `ResourceId` refuses `resource_id` with the modelled
/// not-found and the one sentence the rule fixes.
async fn assert_resource_not_found(router: &MultiServiceRouter, row: &str, resource_id: &str) {
    let expected = format!("{} {resource_id} not found.", expected_kind(resource_id));
    for (operation, extra) in resource_id_operations() {
        let mut request = extra;
        request["ResourceId"] = json!(resource_id);
        let case = format!("{row} / {operation}");

        let (status, refused) = call(router, operation, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled 400 refusal, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {refused}"
        );
        assert_eq!(
            message(&refused),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?} ({refused})",
            message(&refused)
        );
    }
}

/// Associate one budget with `resource_id`, which must succeed, and read it back through
/// `ListBudgetsForResource`: the positive control that the refusals below are keyed on
/// stored state and not on the shape of the identifier.
async fn associate_and_read_back(router: &MultiServiceRouter, case: &str, resource_id: &str) {
    let budget = "monthly-cloud-spend";
    let (status, body) = call(
        router,
        "AssociateBudgetWithResource",
        &json!({"ResourceId": resource_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a stored resource must take the association, got {status} with {body}"
    );

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
    assert_eq!(
        listed["Budgets"],
        json!([{"BudgetName": budget}]),
        "case {case:?}: expected the one association just made, got {listed}"
    );
}

#[tokio::test]
async fn an_identifier_nothing_answers_to_is_refused_by_the_kind_it_names() {
    let router = emulator();
    // Seeded so the store is not empty: a refusal must come from the identifier resolving
    // to nothing, not from there being nothing at all.
    let portfolio_id = create_portfolio(&router, "nfm-portfolio", "acme", &[], "nfm-p").await;
    let product_id = create_product(&router, "nfm-product", "nfm-r").await;
    associate_and_read_back(&router, "seeded portfolio", &portfolio_id).await;
    associate_and_read_back(&router, "seeded product", &product_id).await;

    // Identifiers the modelled `Id` admits (length 1..100, pattern `^[a-zA-Z0-9_\-]*$`)
    // that no stored resource answers to: both length bounds, each character class of the
    // pattern, and the portfolio prefix present, mis-cased, or without its separator.
    let rows: Vec<(&str, String)> = vec![
        ("the shortest identifier", "a".to_string()),
        ("the longest identifier", "z".repeat(100)),
        (
            "the longest portfolio-prefixed identifier",
            format!("{PORTFOLIO_PREFIX}{}", "z".repeat(95)),
        ),
        ("underscores only", "___".to_string()),
        ("hyphens only", "---".to_string()),
        ("digits only", "1234567890".to_string()),
        ("an unminted portfolio id", "port-aaaaaaaaaaaa".to_string()),
        ("an unminted product id", "prod-aaaaaaaaaaaa".to_string()),
        (
            "an unminted provisioned product id",
            "pp-aaaaaaaaaaaa".to_string(),
        ),
        ("the portfolio prefix alone", PORTFOLIO_PREFIX.to_string()),
        (
            "the portfolio prefix without its separator",
            "port1234".to_string(),
        ),
        (
            "the portfolio prefix in another case",
            "PORT-AAAAAAAAAAAA".to_string(),
        ),
        (
            "an identifier of neither prefix",
            "no_such-Resource-42".to_string(),
        ),
    ];

    for (row, resource_id) in &rows {
        assert!(
            !resource_id.is_empty() && resource_id.len() <= 100,
            "case {row:?}: the row must stay inside the modelled length 1..100, got {} \
             characters",
            resource_id.len()
        );
        assert_resource_not_found(&router, row, resource_id).await;
    }

    // Not one of those refusals touched the resources that do exist: each still reports the
    // single association the positive control made.
    for (case, resource_id) in [
        ("seeded portfolio after the refusals", &portfolio_id),
        ("seeded product after the refusals", &product_id),
    ] {
        let (status, listed) = call(
            &router,
            "ListBudgetsForResource",
            &json!({"ResourceId": resource_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the stored resource must still read, got {status} with {listed}"
        );
        assert_eq!(
            listed["Budgets"],
            json!([{"BudgetName": "monthly-cloud-spend"}]),
            "case {case:?}: a refused call must change nothing, got {listed}"
        );
    }
}

#[tokio::test]
async fn an_identifier_whose_resource_was_deleted_is_refused_by_its_own_kind() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "gone-portfolio", "acme", &[], "gone-p").await;
    let product_id = create_product(&router, "gone-product", "gone-r").await;

    // Both take the association while they exist — so what changes below is the stored
    // record, not the identifier.
    associate_and_read_back(&router, "portfolio before deletion", &portfolio_id).await;
    associate_and_read_back(&router, "product before deletion", &product_id).await;

    for (case, operation, member, id) in [
        (
            "deleted portfolio",
            "DeletePortfolio",
            "Id",
            portfolio_id.clone(),
        ),
        ("deleted product", "DeleteProduct", "Id", product_id.clone()),
    ] {
        let (status, body) = call(&router, operation, &json!({member: &id})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation}({id}) must succeed, got {status} with {body}"
        );
    }

    // A minted portfolio id carries the portfolio prefix and a minted product id does not,
    // so each deleted resource's own identifier is refused by its own kind.
    assert_eq!(
        expected_kind(&portfolio_id),
        PORTFOLIO_KIND,
        "case \"deleted portfolio\": a minted portfolio id must carry the portfolio prefix, got \
         {portfolio_id:?}"
    );
    assert_eq!(
        expected_kind(&product_id),
        PRODUCT_KIND,
        "case \"deleted product\": a minted product id must not carry the portfolio prefix, got \
         {product_id:?}"
    );
    assert_resource_not_found(&router, "deleted portfolio", &portfolio_id).await;
    assert_resource_not_found(&router, "deleted product", &product_id).await;
}

#[tokio::test]
async fn an_identifier_of_a_record_that_carries_no_budget_is_refused_the_same_way() {
    let router = emulator();

    // A TagOption is a stored record addressed by an id of the same modelled `Id` shape,
    // and it is not a resource a budget hangs off: the lookup is of portfolios and
    // products, so a TagOption's own id names no budget-carrying resource.
    let (status, created) = call(
        &router,
        "CreateTagOption",
        &json!({"Key": "cost-center", "Value": "platform"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a stored TagOption\": CreateTagOption must succeed, got {status} with {created}"
    );
    let tag_option_id = created["TagOptionDetail"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"a stored TagOption\": no Id in {created}"))
        .to_string();

    assert_resource_not_found(&router, "a stored TagOption id", &tag_option_id).await;
}
