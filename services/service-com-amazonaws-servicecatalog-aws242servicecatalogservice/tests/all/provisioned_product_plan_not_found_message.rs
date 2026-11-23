// SPDX-License-Identifier: CC0-1.0

//! The refusal a read of a provisioned product plan answers a plan id that resolves to
//! no stored plan with, over the awsJson1_1 wire.
//!
//! The rule under test: a plan read addressed by a syntactically valid plan id that
//! names no stored plan is refused with HTTP 400 `ResourceNotFoundException` whose
//! message is "No plan with ID <PlanId> exists." — all fixed text but the one variable
//! part, the `PlanId` the request carried, taken from the request verbatim.
//!
//! The rows differ along what the rule generalises over: the id's length (both published
//! bounds of the modelled `@length`, and one past each), which of the characters the
//! modelled `@pattern` admits it uses, whether it carries this service's plan prefix at
//! all, and whether it misses a plan that DOES exist only by case or by one character.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    associate_product_with_portfolio, call, create_portfolio, create_product_with_info,
    create_template_bucket, emulator, emulator_with_template_services, error_code,
    first_provisioning_artifact_id, message, stage_template, staged_template_url,
};

/// The only `ProvisionedProductPlanType` the model declares
/// (`smithy_query query="shape" shape="ProvisionedProductPlanType"`).
const PLAN_TYPE: &str = "CLOUDFORMATION";

/// The modelled bounds of `DescribeProvisionedProductPlan`'s `PlanId`
/// (`smithy_query query="operation" operation="DescribeProvisionedProductPlan"`: target
/// `Id`, required, `@length` min 1 max 100, `@pattern ^[a-zA-Z0-9_\-]*$`). A value
/// outside them never reaches the lookup, so the rows that test the wording stay inside.
const PLAN_ID_MIN_LENGTH: usize = 1;
const PLAN_ID_MAX_LENGTH: usize = 100;

/// The sentence the rule requires, for the id a row sent.
fn expected_message(plan_id: &str) -> String {
    format!("No plan with ID {plan_id} exists.")
}

/// Every syntactically valid plan id that resolves to nothing is refused with AWS's
/// wording, whatever the id looks like.
#[tokio::test]
async fn an_unresolved_plan_id_is_refused_with_the_modelled_not_found_wording() {
    // Rows: each published length bound, the characters the pattern admits (letters of
    // both cases, digits, `_` and `-`), an id carrying this service's own plan prefix
    // and one carrying no prefix at all.
    let max_length_id = "p".repeat(PLAN_ID_MAX_LENGTH);
    let min_length_id = "a".repeat(PLAN_ID_MIN_LENGTH);
    let cases: [(&str, &str); 6] = [
        ("at the minimum modelled length", &min_length_id),
        ("at the maximum modelled length", &max_length_id),
        ("this service's own plan prefix", "plan-7f3c1b9d2e4a"),
        ("underscores, hyphens and both cases", "Plan_Id-7F3c_9-x"),
        ("digits only", "90210"),
        ("a single hyphen", "-"),
    ];

    let router = emulator();
    for (case, plan_id) in cases {
        let (status, body) = call(
            &router,
            "DescribeProvisionedProductPlan",
            &json!({"PlanId": plan_id}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the unresolved plan id {plan_id:?} to be refused, got \
             {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {plan_id:?}, got {body}"
        );
        assert_eq!(
            message(&body),
            expected_message(plan_id),
            "case {case:?}: expected the not-found wording for {plan_id:?}, got {body}"
        );
    }
}

/// A `PlanId` outside the modelled `@length` is a constraint violation, refused before
/// any lookup happens: it never carries the not-found wording.
#[tokio::test]
async fn a_plan_id_outside_the_modelled_length_is_refused_as_a_constraint_violation() {
    // One past each published bound: the empty value under the minimum, and one
    // character more than the maximum.
    let too_long = "p".repeat(PLAN_ID_MAX_LENGTH + 1);
    let cases: [(&str, &str); 2] = [
        ("one under the minimum modelled length", ""),
        ("one past the maximum modelled length", &too_long),
    ];

    let router = emulator();
    for (case, plan_id) in cases {
        let (status, body) = call(
            &router,
            "DescribeProvisionedProductPlan",
            &json!({"PlanId": plan_id}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a length of {} to be refused, got {status} with {body}",
            plan_id.len()
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected the constraint violation, got {body}"
        );
        assert_ne!(
            message(&body),
            expected_message(plan_id),
            "case {case:?}: a constraint violation must not read as a missing plan, got {body}"
        );
    }
}

/// A staged template declaring one resource: the simplest launch a plan can plan.
fn one_resource_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {"Data": {"Type": "AWS::S3::Bucket"}}
    })
    .to_string()
}

/// Stage a template, create a product whose only version names it, put that product in a
/// portfolio, plan a provisioning of it and answer the plan id the service minted.
async fn seed_plan(router: &MultiServiceRouter, slug: &str) -> String {
    let template_key = format!("{slug}/template.json");
    stage_template(router, &template_key, &one_resource_template()).await;
    let product_id = create_product_with_info(
        router,
        &format!("{slug}-product"),
        "CLOUD_FORMATION_TEMPLATE",
        // The accepted `Info` keys are the model's own:
        // `ProvisioningArtifactProperties$Info` documents
        // "Keys accepted: [ LoadTemplateFromURL, ImportFromPhysicalId ]".
        &json!({"LoadTemplateFromURL": staged_template_url(&template_key)}),
        slug,
    )
    .await;
    let artifact_id = first_provisioning_artifact_id(router, &product_id).await;
    let portfolio_id =
        create_portfolio(router, &format!("{slug}-portfolio"), "acme", &[], slug).await;
    associate_product_with_portfolio(router, &product_id, &portfolio_id).await;
    let (status, created) = call(
        router,
        "CreateProvisionedProductPlan",
        &json!({
            "PlanName": format!("{slug}-plan"),
            "PlanType": PLAN_TYPE,
            "ProductId": product_id,
            "ProvisionedProductName": format!("{slug}-pp"),
            "ProvisioningArtifactId": artifact_id,
            "IdempotencyToken": format!("{slug}-token"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the plan must be created first, got {status} with {created}"
    );
    created["PlanId"]
        .as_str()
        .unwrap_or_else(|| panic!("no PlanId in {created}"))
        .to_string()
}

/// The refusal is about RESOLUTION, not about the shape of the id: the plan that exists
/// is served, and an id that misses it by case or by one character takes the not-found
/// wording naming the id the request sent.
#[tokio::test]
async fn a_stored_plan_is_served_and_every_near_miss_of_its_id_is_refused() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let plan_id = seed_plan(&router, "wording").await;

    let (status, described) = call(
        &router,
        "DescribeProvisionedProductPlan",
        &json!({"PlanId": plan_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the plan that exists\": expected the stored plan to be served, got {status} with \
         {described}"
    );
    assert_eq!(
        described["ProvisionedProductPlanDetails"]["PlanId"]
            .as_str()
            .map(str::to_string),
        Some(plan_id.clone()),
        "case \"the plan that exists\": expected the stored plan back, got {described}"
    );

    // Rows derived from the id that DOES resolve, each differing from it along one
    // dimension the lookup is exact in: case, a trailing character, a missing one.
    let upper = plan_id.to_uppercase();
    let longer = format!("{plan_id}0");
    let shorter: String = {
        let mut dropped = plan_id.clone();
        dropped.pop();
        dropped
    };
    let cases: [(&str, &str); 3] = [
        ("the stored id in upper case", &upper),
        ("the stored id with one character more", &longer),
        ("the stored id with its last character dropped", &shorter),
    ];
    for (case, near_miss) in cases {
        let (status, body) = call(
            &router,
            "DescribeProvisionedProductPlan",
            &json!({"PlanId": near_miss}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {near_miss:?} to resolve to no plan, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException for {near_miss:?}, got {body}"
        );
        assert_eq!(
            message(&body),
            expected_message(near_miss),
            "case {case:?}: expected the refusal to name the id the request sent, got {body}"
        );
    }

    // Reading a plan that does not exist changed nothing a later caller sees: the stored
    // plan still reads back exactly as it did before the refusals.
    let (status, again) = call(
        &router,
        "DescribeProvisionedProductPlan",
        &json!({"PlanId": plan_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the plan after the refusals\": expected the stored plan to be served, got {status} \
         with {again}"
    );
    assert_eq!(
        again["ProvisionedProductPlanDetails"]["PlanId"]
            .as_str()
            .map(str::to_string),
        Some(plan_id.clone()),
        "case \"the plan after the refusals\": expected the same stored plan, got {again}"
    );
}
