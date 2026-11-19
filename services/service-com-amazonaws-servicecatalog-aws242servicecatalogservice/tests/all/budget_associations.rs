// SPDX-License-Identifier: CC0-1.0

//! The budgets a Service Catalog resource carries, driven as awsJson1_1 requests through
//! the same `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: associating a budget with a resource stores the (resource, budget)
//! pair, so every later read of that resource — `ListBudgetsForResource` and the
//! `Budgets` member of each reader of a portfolio or a product — reports exactly the
//! stored set and nothing else, a disassociation removes exactly that pair and a pair the
//! store does not hold is `ResourceNotFoundException` naming it, a pair already
//! stored is the modelled `DuplicateResourceException`, a resource identifier naming
//! neither a portfolio nor a product is `ResourceNotFoundException` naming the kind that
//! identifier carries, and a resource past the per-resource quota is
//! `LimitExceededException`.
//!
//! Each row varies what the rule generalises over: the KIND of resource, how many budgets
//! one resource carries, how many resources one budget reaches, the SHAPE of the budget
//! name (`smithy_query query="shape" shape="BudgetName"`: a string of length 1..100 with
//! no pattern, so the rows span the minimum length, the maximum length, spaces and
//! punctuation, and non-ASCII text), and the page size a walk asks for. Every assertion is
//! about STORED state: what a reader reports is compared against the set of associations
//! the case made, never against the body of the request that made one.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_servicecatalog_aws242servicecatalogservice::state::MAX_BUDGETS_PER_RESOURCE;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    associate_product_with_portfolio, call, create_portfolio, create_product, emulator, error_code,
    message,
};

/// `case` as an `IdempotencyToken`: the modelled pattern is
/// `^[a-zA-Z0-9][a-zA-Z0-9_-]*$`, which a prose case name is not.
fn token(case: &str) -> String {
    case.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The kind of resource a row associates its budgets with. `ResourceId` is "the resource
/// identifier. Either a portfolio-id or a product-id", so both kinds are rows of the same
/// table, each read back through its own admin reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    Portfolio,
    Product,
}

impl ResourceKind {
    /// The admin reader that reports this kind of resource's `Budgets`
    /// (`smithy_query query="operation" operation="DescribePortfolio,DescribeProductAsAdmin"`:
    /// both carry a `Budgets` member targeting the `Budgets` list).
    fn reader(self) -> &'static str {
        match self {
            ResourceKind::Portfolio => "DescribePortfolio",
            ResourceKind::Product => "DescribeProductAsAdmin",
        }
    }
}

/// A budget name of the maximum modelled length (100), distinct from every other row.
fn longest_budget_name() -> String {
    format!("budget-{}", "x".repeat(93))
}

/// The budget names the rows draw on: the shortest a `BudgetName` may be, a plain one, one
/// carrying spaces and punctuation, one of the maximum length, and one of non-ASCII text —
/// the member is a bare string of length 1..100, so all five are names AWS Budgets' own
/// naming admits and the association stores verbatim.
fn budget_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "monthly-cloud-spend".to_string(),
        "Platform Team (prod) $1,000/mo".to_string(),
        longest_budget_name(),
        "予算-プラットフォーム".to_string(),
    ]
}

/// One association row: the kind of resource, and which budget names it carries by index
/// into [`budget_names`].
struct AssociationCase {
    case: &'static str,
    kind: ResourceKind,
    budgets: &'static [usize],
}

/// The rows: both resource kinds, from no association through a single one to every seeded
/// name, with index 0 shared by several rows so one budget reaches several resources.
const ASSOCIATION_CASES: &[AssociationCase] = &[
    AssociationCase {
        case: "portfolio with no budget",
        kind: ResourceKind::Portfolio,
        budgets: &[],
    },
    AssociationCase {
        case: "portfolio with one budget",
        kind: ResourceKind::Portfolio,
        budgets: &[0],
    },
    AssociationCase {
        case: "portfolio with a punctuated and a longest name",
        kind: ResourceKind::Portfolio,
        budgets: &[2, 3],
    },
    AssociationCase {
        case: "product with no budget",
        kind: ResourceKind::Product,
        budgets: &[],
    },
    AssociationCase {
        case: "product with one budget, shared with a portfolio",
        kind: ResourceKind::Product,
        budgets: &[0],
    },
    AssociationCase {
        case: "product with every seeded budget",
        kind: ResourceKind::Product,
        budgets: &[0, 1, 2, 3, 4],
    },
];

/// Seed one resource of `kind` and answer its id.
async fn seed_resource(router: &MultiServiceRouter, kind: ResourceKind, name: &str) -> String {
    match kind {
        ResourceKind::Portfolio => create_portfolio(router, name, "acme", &[], &token(name)).await,
        ResourceKind::Product => create_product(router, name, &token(name)).await,
    }
}

/// Associate one budget with one resource, expecting the call to be served.
async fn associate(router: &MultiServiceRouter, case: &str, resource_id: &str, budget: &str) {
    let (status, body) = call(
        router,
        "AssociateBudgetWithResource",
        &json!({"ResourceId": resource_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssociateBudgetWithResource({resource_id}, {budget:?}): expected 200, got \
         {status} with {body}"
    );
}

/// Disassociate one budget from one resource, expecting the call to be served.
async fn disassociate(router: &MultiServiceRouter, case: &str, resource_id: &str, budget: &str) {
    let (status, body) = call(
        router,
        "DisassociateBudgetFromResource",
        &json!({"ResourceId": resource_id, "BudgetName": budget}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DisassociateBudgetFromResource({resource_id}, {budget:?}): expected 200, \
         got {status} with {body}"
    );
}

/// The names in a reported `Budgets` list, with each entry checked to carry the one member
/// the modelled `BudgetDetail` has.
fn reported_names(case: &str, reported: &[Value]) -> BTreeSet<String> {
    reported
        .iter()
        .map(|detail| {
            let name = detail["BudgetName"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: a reported budget has no BudgetName: {detail}")
            });
            name.to_string()
        })
        .collect()
}

/// The `Budgets` `ListBudgetsForResource` reports for `resource_id` on one unbounded page.
async fn listed_budgets(router: &MultiServiceRouter, case: &str, resource_id: &str) -> Vec<Value> {
    let (status, body) = call(
        router,
        "ListBudgetsForResource",
        &json!({"ResourceId": resource_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListBudgetsForResource({resource_id}), got {status} with \
         {body}"
    );
    assert!(
        body["NextPageToken"].is_null(),
        "case {case:?}: an unbounded page reports every budget, so it carries no NextPageToken: \
         {body}"
    );
    body["Budgets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: ListBudgetsForResource reported no list: {body}"))
        .clone()
}

/// The `Budgets` the reader `operation` reports for the resource `request` names.
async fn described_budgets(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Vec<Value> {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}({request}), got {status} with {body}"
    );
    body["Budgets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: {operation} reported no Budgets list: {body}"))
        .clone()
}

#[tokio::test]
async fn a_resources_reported_budgets_are_its_stored_associations() {
    let router = emulator();
    let names = budget_names();

    for case in ASSOCIATION_CASES {
        let resource_id = seed_resource(&router, case.kind, case.case).await;

        // Before any association, both readers report no budget at all: a reader that
        // answered one here would not be reading stored state.
        let before = listed_budgets(&router, case.case, &resource_id).await;
        assert!(
            before.is_empty(),
            "case {:?}: a resource with no association must list no budget, got {before:?}",
            case.case
        );
        let described = described_budgets(
            &router,
            case.case,
            case.kind.reader(),
            &json!({"Id": resource_id}),
        )
        .await;
        assert!(
            described.is_empty(),
            "case {:?}: a resource with no association must describe no budget, got {described:?}",
            case.case
        );

        for index in case.budgets {
            associate(&router, case.case, &resource_id, &names[*index]).await;
        }

        let expected: BTreeSet<String> = case
            .budgets
            .iter()
            .map(|index| names[*index].clone())
            .collect();
        let listed = listed_budgets(&router, case.case, &resource_id).await;
        assert_eq!(
            reported_names(case.case, &listed),
            expected,
            "case {:?}: expected exactly the associated budgets {expected:?}, got {listed:?}",
            case.case
        );

        // The resource's own reader reports the SAME set: one stored association cannot be
        // reported two ways.
        let described = described_budgets(
            &router,
            case.case,
            case.kind.reader(),
            &json!({"Id": resource_id}),
        )
        .await;
        assert_eq!(
            described, listed,
            "case {:?}: {} must report the budgets ListBudgetsForResource does ({listed:?}), got \
             {described:?}",
            case.case,
            case.kind.reader()
        );
    }
}

#[tokio::test]
async fn one_budget_reaches_every_resource_it_is_associated_with() {
    let router = emulator();
    let budget = "shared-monthly-budget";
    let portfolio_id = create_portfolio(&router, "shared-portfolio", "acme", &[], "shared-p").await;
    let product_id = create_product(&router, "shared-product", "shared-r").await;
    // The END-USER view of a product reaches it only through a launch path, so the product
    // goes into the portfolio before that reader is asked for its budgets.
    associate_product_with_portfolio(&router, &product_id, &portfolio_id).await;

    // A budget is not consumed by an association: associating it with a second resource
    // leaves the first association standing.
    associate(&router, "shared: portfolio", &portfolio_id, budget).await;
    associate(&router, "shared: product", &product_id, budget).await;

    let only = BTreeSet::from([budget.to_string()]);
    for (case, resource_id) in [
        ("shared: portfolio", &portfolio_id),
        ("shared: product", &product_id),
    ] {
        let listed = listed_budgets(&router, case, resource_id).await;
        assert_eq!(
            reported_names(case, &listed),
            only,
            "case {case:?}: expected the shared budget {budget:?}, got {listed:?}"
        );
    }

    // Every reader of either kind of resource reports that one association: the admin
    // readers of both kinds and the end-user view of the product.
    for (case, operation, request) in [
        (
            "portfolio describe",
            "DescribePortfolio",
            json!({"Id": portfolio_id}),
        ),
        (
            "product admin describe",
            "DescribeProductAsAdmin",
            json!({"Id": product_id}),
        ),
        (
            "product end-user describe",
            "DescribeProduct",
            json!({"Id": product_id}),
        ),
    ] {
        let described = described_budgets(&router, case, operation, &request).await;
        assert_eq!(
            reported_names(case, &described),
            only,
            "case {case:?}: {operation} must report the stored budget {budget:?}, got {described:?}"
        );
    }
}

#[tokio::test]
async fn associating_a_stored_pair_again_is_the_duplicate() {
    let router = emulator();
    let budget = "monthly-cloud-spend";
    let other = "quarterly-cloud-spend";
    let portfolio_id = create_portfolio(&router, "dup-portfolio", "acme", &[], "dup-p").await;
    let product_id = create_product(&router, "dup-product", "dup-r").await;

    for (case, resource_id) in [
        ("duplicate on a portfolio", &portfolio_id),
        ("duplicate on a product", &product_id),
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
            error_code(&refused),
            "DuplicateResourceException",
            "case {case:?}: expected DuplicateResourceException, got {refused}"
        );
        // The duplicate is the ASSOCIATION, and the refusal names it on the resource
        // that already holds it — the one variable part is that resource's identifier,
        // taken from the request.
        assert_eq!(
            message(&refused),
            format!("BudgetAssociation already exists on resource {resource_id}"),
            "case {case:?}: expected the duplicate association's own wording, got {refused}"
        );

        // The refusal changed nothing, and another budget still associates.
        associate(&router, case, resource_id, other).await;
        let listed = listed_budgets(&router, case, resource_id).await;
        assert_eq!(
            reported_names(case, &listed),
            BTreeSet::from([budget.to_string(), other.to_string()]),
            "case {case:?}: expected both budgets once each, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_resource_identifier_nothing_stored_carries_is_not_found() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "nf-portfolio", "acme", &[], "nf-p").await;
    let product_id = create_product(&router, "nf-product", "nf-r").await;

    // Rows: an id of each resource prefix nothing minted, a well-formed id of a kind
    // that carries no budget (a provisioned product), and an id carrying neither
    // resource prefix, so the lookup cannot be reading one map for another. Each row is
    // driven through all three operations that take "the resource identifier", and each
    // carries the KIND its identifier names: a `port-` id is a portfolio and every other
    // id a product, which is what the refusal reports.
    let unknown: [(&str, &str, &str); 4] = [
        ("unminted portfolio id", "port-zzzzzzzzzzzz", "Portfolio"),
        ("unminted product id", "prod-zzzzzzzzzzzz", "Product"),
        ("a provisioned product id", "pp-zzzzzzzzzzzz", "Product"),
        ("an id of neither kind", "no-such-resource", "Product"),
    ];
    let operations: [(&str, Value); 3] = [
        (
            "AssociateBudgetWithResource",
            json!({"BudgetName": "monthly-cloud-spend"}),
        ),
        (
            "DisassociateBudgetFromResource",
            json!({"BudgetName": "monthly-cloud-spend"}),
        ),
        ("ListBudgetsForResource", json!({})),
    ];

    for (row, resource_id, kind) in unknown {
        for (operation, extra) in &operations {
            let mut request = extra.clone();
            request["ResourceId"] = json!(resource_id);
            let case = format!("{operation} with an {row}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected a 400 refusal, got {status} with {refused}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {refused}"
            );
            assert_eq!(
                message(&refused),
                format!("{kind} {resource_id} not found."),
                "case {case:?}: the refusal must name the kind the identifier carries and the \
                 identifier itself, got {refused}"
            );
        }
    }

    // None of the refusals above stored anything: both seeded resources still carry no
    // budget.
    for (case, resource_id) in [
        ("nothing stored on the portfolio", &portfolio_id),
        ("nothing stored on the product", &product_id),
    ] {
        let listed = listed_budgets(&router, case, resource_id).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused call must store nothing, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_resource_takes_budgets_up_to_the_quota_and_no_more() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "quota-portfolio", "acme", &[], "quota-p").await;

    // Every association up to the quota the emulator enforces is stored, and the first one
    // past it is refused: the bound itself and the first value outside it.
    let mut associated = BTreeSet::new();
    for index in 0..MAX_BUDGETS_PER_RESOURCE {
        let budget = format!("budget-{index}");
        associate(&router, "within the quota", &portfolio_id, &budget).await;
        associated.insert(budget);
    }

    let (status, refused) = call(
        &router,
        "AssociateBudgetWithResource",
        &json!({"ResourceId": portfolio_id, "BudgetName": "one-too-many"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"past the quota\": expected a 400 refusal, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "LimitExceededException",
        "case \"past the quota\": expected LimitExceededException, got {refused}"
    );

    let listed = listed_budgets(&router, "past the quota", &portfolio_id).await;
    assert_eq!(
        reported_names("past the quota", &listed),
        associated,
        "case \"past the quota\": the refused association must store nothing, got {listed:?}"
    );
}

/// Walk `ListBudgetsForResource` at `page_size`, following `NextPageToken` to the end, and
/// answer the names in the order the pages reported them.
async fn walk_pages(
    router: &MultiServiceRouter,
    case: &str,
    resource_id: &str,
    page_size: i32,
) -> Vec<String> {
    let mut walked = Vec::new();
    let mut page_token: Option<String> = None;
    // One more iteration than there can be pages, so a cursor that failed to advance is
    // caught here rather than looping forever.
    for _ in 0..=MAX_BUDGETS_PER_RESOURCE + 1 {
        let mut request = json!({"ResourceId": resource_id, "PageSize": page_size});
        if let Some(token) = &page_token {
            request["PageToken"] = json!(token);
        }
        let (status, body) = call(router, "ListBudgetsForResource", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ListBudgetsForResource({request}), got {status} with \
             {body}"
        );
        let page = body["Budgets"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: a page reported no Budgets list: {body}"))
            .clone();
        if page_size > 0 {
            assert!(
                page.len() <= page_size as usize,
                "case {case:?}: PageSize {page_size} is a bound, got {} entries in {body}",
                page.len()
            );
        }
        walked.extend(
            page.iter()
                .map(|detail| detail["BudgetName"].as_str().unwrap_or_default().to_string()),
        );
        match body["NextPageToken"].as_str() {
            Some(token) => page_token = Some(token.to_string()),
            None => return walked,
        }
    }
    panic!("case {case:?}: the walk never reached a page without a NextPageToken");
}

#[tokio::test]
async fn a_paged_walk_reports_every_budget_exactly_once() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "page-portfolio", "acme", &[], "page-p").await;
    let names = budget_names();
    for budget in &names {
        associate(&router, "paged walk", &portfolio_id, budget).await;
    }

    // The one unbounded page is the whole stored set, and every page size walks exactly
    // that set once: one budget per page up to the modelled `PageSize` maximum of 20, plus
    // the default 0, which asks for no caller-imposed bound.
    let whole: Vec<String> = listed_budgets(&router, "paged walk", &portfolio_id)
        .await
        .iter()
        .map(|detail| detail["BudgetName"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        whole.iter().cloned().collect::<BTreeSet<String>>(),
        names.iter().cloned().collect::<BTreeSet<String>>(),
        "case \"paged walk\": the unbounded page must report every associated budget, got {whole:?}"
    );

    for page_size in [0, 1, 2, 3, 5, 20] {
        let case = format!("walk at PageSize={page_size}");
        let walked = walk_pages(&router, &case, &portfolio_id, page_size).await;
        assert_eq!(
            walked, whole,
            "case {case:?}: a paged walk must report every budget exactly once, in one order: \
             expected {whole:?}, got {walked:?}"
        );
    }

    // `PageSize` is modelled `range 0..20`: the bounds are served above, and the first
    // value past the maximum is refused before the handler runs.
    let (status, refused) = call(
        &router,
        "ListBudgetsForResource",
        &json!({"ResourceId": portfolio_id, "PageSize": 21}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"PageSize past the maximum\": expected a 400 refusal, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ValidationException",
        "case \"PageSize past the maximum\": expected ValidationException, got {refused}"
    );

    // A token this service did not mint identifies no page: that is a bad parameter, not
    // an empty page.
    for (case, bad_token) in [
        ("a resource id as the token", portfolio_id.as_str()),
        ("a token that is not hex", "not-a-page-token"),
    ] {
        let (status, refused) = call(
            &router,
            "ListBudgetsForResource",
            &json!({"ResourceId": portfolio_id, "PageToken": bad_token}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a foreign page token must be refused, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidParametersException",
            "case {case:?}: expected InvalidParametersException, got {refused}"
        );
    }
}

#[tokio::test]
async fn disassociating_removes_exactly_that_pair() {
    let router = emulator();
    let kept = "monthly-cloud-spend";
    let dropped = "quarterly-cloud-spend";
    let unknown = "never-associated-budget";
    let portfolio_id = create_portfolio(&router, "dis-portfolio", "acme", &[], "dis-p").await;
    let product_id = create_product(&router, "dis-product", "dis-r").await;

    for (case, kind, resource_id) in [
        (
            "disassociate on a portfolio",
            ResourceKind::Portfolio,
            &portfolio_id,
        ),
        (
            "disassociate on a product",
            ResourceKind::Product,
            &product_id,
        ),
    ] {
        associate(&router, case, resource_id, kept).await;
        associate(&router, case, resource_id, dropped).await;
        disassociate(&router, case, resource_id, dropped).await;

        let listed = listed_budgets(&router, case, resource_id).await;
        assert_eq!(
            reported_names(case, &listed),
            BTreeSet::from([kept.to_string()]),
            "case {case:?}: only the disassociated pair is removed, got {listed:?}"
        );

        // What this call removes is the PAIR, so a pair the store does not hold is the
        // modelled not-found: the one just removed and a budget never associated with
        // this resource are the same refusal, named by the pair it could not find.
        for (row, budget) in [("already disassociated", dropped), ("never associated", unknown)] {
            let (status, refused) = call(
                &router,
                "DisassociateBudgetFromResource",
                &json!({"ResourceId": resource_id, "BudgetName": budget}),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?} ({row}): a pair the store does not hold must be refused, got \
                 {status} with {refused}"
            );
            assert_eq!(
                error_code(&refused),
                "ResourceNotFoundException",
                "case {case:?} ({row}): expected ResourceNotFoundException, got {refused}"
            );
            assert_eq!(
                message(&refused),
                format!("Budget with name {budget} is not associated with resource {resource_id}"),
                "case {case:?} ({row}): the refusal must name the pair it could not find, got \
                 {refused}"
            );
        }
        let listed = listed_budgets(&router, case, resource_id).await;
        assert_eq!(
            reported_names(case, &listed),
            BTreeSet::from([kept.to_string()]),
            "case {case:?}: a refused disassociation changes nothing, got {listed:?}"
        );

        // The last association removed leaves both readers reporting nothing.
        disassociate(&router, case, resource_id, kept).await;
        let listed = listed_budgets(&router, case, resource_id).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: every pair removed must leave no budget, got {listed:?}"
        );
        let described =
            described_budgets(&router, case, kind.reader(), &json!({"Id": resource_id})).await;
        assert!(
            described.is_empty(),
            "case {case:?}: every pair removed must leave {} reporting none, got {described:?}",
            kind.reader()
        );
    }
}

#[tokio::test]
async fn reading_a_resources_budgets_does_not_change_them() {
    let router = emulator();
    let portfolio_id = create_portfolio(&router, "read-portfolio", "acme", &[], "read-p").await;
    associate(&router, "repeated read", &portfolio_id, "monthly-cloud-spend").await;

    let first = listed_budgets(&router, "repeated read", &portfolio_id).await;
    let second = listed_budgets(&router, "repeated read", &portfolio_id).await;
    assert_eq!(
        first, second,
        "case \"repeated read\": a read must not change what the next caller sees: {first:?} then \
         {second:?}"
    );
    let described = described_budgets(
        &router,
        "repeated read",
        "DescribePortfolio",
        &json!({"Id": portfolio_id}),
    )
    .await;
    assert_eq!(
        described, first,
        "case \"repeated read\": the describe after the reads reports the same budgets {first:?}, \
         got {described:?}"
    );
}
