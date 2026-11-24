// SPDX-License-Identifier: CC0-1.0

//! The refusals the provisioned-product OUTPUTS read and the product VIEW read answer an
//! identifier that resolves to nothing with, over the awsJson1_1 wire.
//!
//! The rules under test:
//!
//! * `GetProvisionedProductOutputs` answers what the launch's CloudFormation stack holds,
//!   so an identifier resolving no stack is refused as the STACK LOOKUP failed: HTTP 400
//!   `ResourceNotFoundException` whose message is `"No stack named <identifier> exists."` —
//!   all fixed text but the identifier the request carried, taken from it verbatim —
//!   whichever of the two members addressed the provisioned product;
//! * `DescribeProductView` words its own not-found by naming the member the request
//!   addressed the view by: `"Product not found for ProductViewId: <Id>"`, with the `Id`
//!   the request carried as its only variable part — a different template from the one the
//!   sibling product read answers with, so neither read may answer the other's.
//!
//! The rows differ along what the rules generalise over: which member addressed the
//! resource, the identifier's length (each published bound of the modelled `@length`, and
//! one past each, which never reaches a lookup), which of the characters the modelled
//! `@pattern` admits it uses, whether it carries the prefix this service mints, and
//! whether it misses a resource that DOES exist only by case or by one character.
//!
//! Each refusal is driven against a store that really holds the resource kind being
//! looked up — a settled provisioned product, a product on a launch path — read back
//! through the implemented reads, so a refusal is about the identifier the request
//! carried and not about an empty store.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    associate_product_with_portfolio, call, create_portfolio, create_product_of_type,
    create_product_with_info, create_template_bucket, emulator, emulator_with_template_services,
    error_code, first_provisioning_artifact_id, message, stage_template, staged_template_url,
};

/// The modelled bounds of `GetProvisionedProductOutputs`'s two addressing members and of
/// `DescribeProductView`'s `Id` (`smithy_query query="operation"
/// operation="GetProvisionedProductOutputs,DescribeProductView"`): `Id` `@length 1..100`
/// `@pattern ^[a-zA-Z0-9_\-]*$`, `ProvisionedProductName` `@length 1..128`
/// `@pattern ^[a-zA-Z0-9][a-zA-Z0-9._-]*$`.
const ID_MIN_LENGTH: usize = 1;
const ID_MAX_LENGTH: usize = 100;
const NAME_MAX_LENGTH: usize = 128;

/// The modelled upper bound on an error message: `ResourceNotFoundException$Message`
/// targets `ErrorMessage`, `@length 1..1024`
/// (`smithy_query query="shape" shape="ErrorMessage"`).
const MAX_ERROR_MESSAGE: usize = 1024;

/// The `ProductType` whose launch stands a CloudFormation stack up
/// (`smithy_query query="shape" shape="ProductType"`).
const CFN_PRODUCT: &str = "CLOUD_FORMATION_TEMPLATE";

/// `ProvisionedProductStatus`'s settled value
/// (`smithy_query query="shape" shape="ProvisionedProductStatus"`).
const AVAILABLE: &str = "AVAILABLE";

/// How long a poll keeps looking for a condition the emulator's clock owes before it
/// gives up. Generous: it bounds nothing these cases assert, only how long a wait may
/// take on a loaded host.
const POLL_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The sentence the outputs read's rule requires for the identifier a row sent.
fn expected_stack_message(identifier: &str) -> String {
    format!("No stack named {identifier} exists.")
}

/// The sentence the product-view read's rule requires for the `Id` a row sent.
fn expected_view_message(product_view_id: &str) -> String {
    format!("Product not found for ProductViewId: {product_view_id}")
}

/// One `GetProvisionedProductOutputs` call.
async fn get_outputs(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "GetProvisionedProductOutputs", request).await
}

/// Assert the refusal `body` is the modelled not-found carrying `expected`.
fn assert_not_found(case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the unresolved identifier to be refused, got {status} with {body}"
    );
    assert_eq!(
        error_code(body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {body}"
    );
    let reported = message(body);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected {expected:?}, got {reported:?}"
    );
    assert!(
        !reported.is_empty() && reported.len() <= MAX_ERROR_MESSAGE,
        "case {case:?}: the message must satisfy the modelled length, got {} characters in \
         {reported:?}",
        reported.len()
    );
}

/// Every syntactically valid provisioned-product identifier that resolves to nothing is
/// refused with the stack lookup's wording, whichever member carried it and whatever it
/// looks like.
#[tokio::test]
async fn an_unresolved_provisioned_product_is_refused_with_the_stack_wording() {
    let router = emulator();

    let longest_id = "p".repeat(ID_MAX_LENGTH);
    let shortest_id = "q".repeat(ID_MIN_LENGTH);
    let longest_name = format!("s{}", "x".repeat(NAME_MAX_LENGTH - 1));
    // Rows: both addressing members, each at its published length bounds, the prefixed
    // form this service mints, the punctuation each member's pattern admits, and digits
    // only.
    let rows: [(&str, &str, &str); 9] = [
        (
            "ProvisionedProductId",
            "at the minimum modelled length",
            &shortest_id,
        ),
        (
            "ProvisionedProductId",
            "at the maximum modelled length",
            &longest_id,
        ),
        (
            "ProvisionedProductId",
            "the prefixed form this service mints",
            "pp-7f3c1b9d2e4aq",
        ),
        (
            "ProvisionedProductId",
            "underscores, hyphens and both cases",
            "PP_Stack-7F3c_9-x",
        ),
        ("ProvisionedProductId", "digits only", "90210"),
        (
            "ProvisionedProductName",
            "at the minimum modelled length",
            "q",
        ),
        (
            "ProvisionedProductName",
            "at the maximum modelled length",
            &longest_name,
        ),
        (
            "ProvisionedProductName",
            "the punctuation a provisioned product name admits",
            "stack.name_7-x",
        ),
        ("ProvisionedProductName", "digits only", "90210"),
    ];

    for (member, shape, identifier) in rows {
        let case = format!("{member}: {shape}");
        let (status, body) = get_outputs(&router, &json!({member: identifier})).await;
        assert_not_found(&case, status, &body, &expected_stack_message(identifier));
    }
}

/// An identifier outside the modelled `@length` is a constraint violation, refused before
/// any lookup happens: it never carries the stack lookup's wording.
#[tokio::test]
async fn an_identifier_outside_the_modelled_length_is_a_constraint_violation() {
    let router = emulator();

    // One past each published bound of each member: the empty value under the minimum,
    // and one character more than the maximum.
    let id_too_long = "p".repeat(ID_MAX_LENGTH + 1);
    let name_too_long = format!("s{}", "x".repeat(NAME_MAX_LENGTH));
    let rows: [(&str, &str, &str); 4] = [
        (
            "ProvisionedProductId",
            "one under the minimum modelled length",
            "",
        ),
        (
            "ProvisionedProductId",
            "one past the maximum modelled length",
            &id_too_long,
        ),
        (
            "ProvisionedProductName",
            "one under the minimum modelled length",
            "",
        ),
        (
            "ProvisionedProductName",
            "one past the maximum modelled length",
            &name_too_long,
        ),
    ];

    for (member, shape, identifier) in rows {
        let case = format!("{member}: {shape}");
        let (status, body) = get_outputs(&router, &json!({member: identifier})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a length of {} to be refused, got {status} with {body}",
            identifier.len()
        );
        assert_ne!(
            message(&body),
            expected_stack_message(identifier),
            "case {case:?}: a request that never reached a lookup must not read as a missing \
             stack, got {body}"
        );
    }
}

/// A template declaring one bucket and one output over it: the simplest launch whose
/// outputs can be read back.
fn one_output_template(bucket: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "A staged product template",
        "Resources": {
            "Data": {"Type": "AWS::S3::Bucket", "Properties": {"BucketName": bucket}}
        },
        "Outputs": {
            "DataBucket": {
                "Description": "The bucket the launch stood up",
                "Value": {"Ref": "Data"}
            }
        }
    })
    .to_string()
}

/// Stage a template, create the product and the portfolio that gives it a launch path,
/// provision it and wait for the launch to settle. Answers the provisioned product's id
/// and its name.
async fn launch(router: &MultiServiceRouter, slug: &str) -> (String, String) {
    let key = format!("{slug}.template.json");
    stage_template(router, &key, &one_output_template(&format!("{slug}-data"))).await;
    let product_id = create_product_with_info(
        router,
        &format!("{slug}-product"),
        CFN_PRODUCT,
        // The accepted `Info` keys are the model's own:
        // `ProvisioningArtifactProperties$Info` documents
        // "Keys accepted: [ LoadTemplateFromURL, ImportFromPhysicalId ]".
        &json!({"LoadTemplateFromURL": staged_template_url(&key)}),
        slug,
    )
    .await;
    let version_id = first_provisioning_artifact_id(router, &product_id).await;
    let portfolio_id =
        create_portfolio(router, &format!("{slug}-portfolio"), "acme", &[], slug).await;
    associate_product_with_portfolio(router, &product_id, &portfolio_id).await;

    let name = format!("{slug}-pp");
    let (status, provisioned) = call(
        router,
        "ProvisionProduct",
        &json!({
            "ProductId": product_id,
            "ProvisioningArtifactId": version_id,
            "ProvisionedProductName": name,
            "ProvisionToken": format!("{slug}-provision"),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the launch must be served first, got {status} with {provisioned}"
    );
    let provisioned_product_id = provisioned["RecordDetail"]["ProvisionedProductId"]
        .as_str()
        .unwrap_or_else(|| panic!("no ProvisionedProductId in {provisioned}"))
        .to_string();

    // The launch's completion is owed by the clock, so this polls for the CONDITION
    // rather than waiting a fixed time.
    let started = std::time::Instant::now();
    loop {
        let (status, described) =
            call(router, "DescribeProvisionedProduct", &json!({"Name": name})).await;
        assert_eq!(
            status, 200,
            "the provisioned product must be described, got {status} with {described}"
        );
        let reported = described["ProvisionedProductDetail"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == AVAILABLE {
            return (provisioned_product_id, name);
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "the launch must settle to {AVAILABLE}, last reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The refusal is about RESOLUTION, not about the shape of the identifier: the
/// provisioned product that exists has its outputs served by either member, and an
/// identifier that misses it by case or by one character takes the stack lookup's wording
/// naming what the request sent.
#[tokio::test]
async fn a_settled_launch_is_served_and_every_near_miss_is_refused() {
    let router = emulator_with_template_services();
    create_template_bucket(&router).await;
    let (provisioned_product_id, name) = launch(&router, "wording").await;

    for (case, request) in [
        (
            "by ProvisionedProductId",
            json!({"ProvisionedProductId": provisioned_product_id}),
        ),
        (
            "by ProvisionedProductName",
            json!({"ProvisionedProductName": name}),
        ),
    ] {
        let (status, served) = get_outputs(&router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a settled launch's outputs must be served, got {status} with {served}"
        );
        let keys: Vec<String> = served["Outputs"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected an Outputs list, got {served}"))
            .iter()
            .map(|output| {
                output["OutputKey"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an output carries no OutputKey: {output}")
                    })
                    .to_string()
            })
            .collect();
        assert!(
            keys.iter().any(|key| key == "DataBucket"),
            "case {case:?}: the output the staged template declares must be reported, got {keys:?}"
        );
    }

    // Rows derived from the identifiers that DO resolve, each differing along one
    // dimension the lookup is exact in: case, a trailing character, a missing one.
    let upper_id = provisioned_product_id.to_uppercase();
    let longer_id = format!("{provisioned_product_id}0");
    let shorter_id: String = {
        let mut dropped = provisioned_product_id.clone();
        dropped.pop();
        dropped
    };
    let upper_name = name.to_uppercase();
    let longer_name = format!("{name}0");
    let rows: [(&str, &str, &str); 6] = [
        ("ProvisionedProductId", "in upper case", &upper_id),
        ("ProvisionedProductId", "with one character more", &longer_id),
        (
            "ProvisionedProductId",
            "with its last character dropped",
            &shorter_id,
        ),
        ("ProvisionedProductName", "in upper case", &upper_name),
        (
            "ProvisionedProductName",
            "with one character more",
            &longer_name,
        ),
        (
            "ProvisionedProductName",
            "a name only like the stored one",
            "wording-pp-2",
        ),
    ];
    for (member, shape, near_miss) in rows {
        let case = format!("{member}: the stored identifier {shape}");
        let (status, body) = get_outputs(&router, &json!({member: near_miss})).await;
        assert_not_found(&case, status, &body, &expected_stack_message(near_miss));
    }

    // The refusals changed nothing a later caller sees: the settled launch's outputs read
    // back exactly as they did before them.
    let (status, again) = get_outputs(
        &router,
        &json!({"ProvisionedProductId": provisioned_product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "a read after the refusals must still be served, got {status} with {again}"
    );
}

/// The product-view read's refusal is about RESOLUTION, not about the shape of the `Id`:
/// the view that exists is served, and an id at either published length bound or missing
/// the stored one by case or by one character takes this operation's own wording naming
/// what the request sent.
///
/// The identifier SHAPES the model admits, and the stored-product-out-of-view case, are
/// driven against this same template by `product_views`; the rows here are the ones that
/// suite does not carry: each published bound of the modelled `@length`, and near misses
/// of an id that DOES resolve.
#[tokio::test]
async fn a_product_view_is_served_and_every_near_miss_of_its_id_is_refused() {
    let router = emulator();
    // A product that IS in view, so every refusal below is about the identifier the
    // request carried rather than about an empty catalog.
    let portfolio_id = create_portfolio(&router, "view-portfolio", "acme", &[], "tok-vp").await;
    let visible_id =
        create_product_of_type(&router, "view-visible", CFN_PRODUCT, "tok-view-visible").await;
    associate_product_with_portfolio(&router, &visible_id, &portfolio_id).await;
    let (status, in_view) = call(&router, "DescribeProductView", &json!({"Id": visible_id})).await;
    assert_eq!(
        status, 200,
        "the seeded product must be in view, got {status} with {in_view}"
    );
    assert_eq!(
        in_view["ProductViewSummary"]["ProductId"].as_str(),
        Some(visible_id.as_str()),
        "the view must report the stored product, got {in_view}"
    );

    let longest_id = "p".repeat(ID_MAX_LENGTH);
    let shortest_id = "q".repeat(ID_MIN_LENGTH);
    let longer = format!("{visible_id}0");
    let upper = visible_id.to_uppercase();
    let shorter: String = {
        let mut dropped = visible_id.clone();
        dropped.pop();
        dropped
    };
    let rows: [(&str, &str); 5] = [
        ("at the minimum modelled length", &shortest_id),
        ("at the maximum modelled length", &longest_id),
        ("the stored id in upper case", &upper),
        ("the stored id with one character more", &longer),
        ("the stored id with its last character dropped", &shorter),
    ];

    for (case, identifier) in rows {
        let (status, body) = call(&router, "DescribeProductView", &json!({"Id": identifier})).await;
        assert_not_found(case, status, &body, &expected_view_message(identifier));
    }

    // The refusals changed nothing a later caller sees: the view that resolves still
    // reports the same stored product.
    let (status, again) = call(&router, "DescribeProductView", &json!({"Id": visible_id})).await;
    assert_eq!(
        status, 200,
        "a read after the refusals must still be served, got {status} with {again}"
    );
    assert_eq!(
        again, in_view,
        "a refusal must not change what the next read reports, got {again}"
    );
}

/// The product-view read and the sibling product read word their refusals of the very
/// same unresolvable identifier differently: each names the resource it addresses, so
/// neither template may be answered by the other operation.
#[tokio::test]
async fn the_two_end_user_reads_word_their_refusals_differently() {
    let router = emulator();
    let unknown = "prod-7f3c1b9d2e4a";

    let (status, refused) = call(&router, "DescribeProductView", &json!({"Id": unknown})).await;
    assert_not_found(
        "the product view read",
        status,
        &refused,
        &expected_view_message(unknown),
    );
    let view = message(&refused);

    let (status, refused) = call(&router, "DescribeProduct", &json!({"Id": unknown})).await;
    assert_eq!(
        status, 400,
        "the product read: expected 400, got {status} with {refused}"
    );
    let product = message(&refused);

    assert_ne!(
        view, product,
        "the two reads word their refusals differently, got {view:?} for both"
    );
    assert!(
        !view.starts_with(unknown),
        "the product view read's sentence leads with its fixed text, got {view:?}"
    );
}

/// An `Id` outside the modelled `@length` is a constraint violation on the product-view
/// read too: it never carries the not-found wording.
#[tokio::test]
async fn a_product_view_id_outside_the_modelled_length_is_a_constraint_violation() {
    let router = emulator();
    // One past each published bound: the empty value under the minimum, and one character
    // more than the maximum.
    let too_long = "p".repeat(ID_MAX_LENGTH + 1);
    for (case, identifier) in [
        ("one under the minimum modelled length", ""),
        ("one past the maximum modelled length", too_long.as_str()),
    ] {
        let (status, body) = call(&router, "DescribeProductView", &json!({"Id": identifier})).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a length of {} to be refused, got {status} with {body}",
            identifier.len()
        );
        assert_ne!(
            message(&body),
            expected_view_message(identifier),
            "case {case:?}: a constraint violation must not read as a missing product view, got \
             {body}"
        );
    }
}
