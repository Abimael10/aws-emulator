// SPDX-License-Identifier: CC0-1.0

//! `DescribeProductView` over the wire as awsJson1_1 requests through the same
//! `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: the END-USER read of a product view reports the stored product
//! its `Id` addresses — by the identifier the view itself carries or by the id of the
//! product it shows, since this service keeps one view per product — together with every
//! provisioning artifact that product holds, and it reaches a product only through a
//! LAUNCH PATH, so an identifier that resolves no reachable product — one nothing stored
//! carries and a stored product this view cannot see alike — is refused with this
//! operation's own not-found sentence, which names the member the request addressed the
//! view by and echoes the value it carried.
//!
//! Every assertion is about STORED state: the summary and the versions a view reports are
//! checked against what an earlier create, copy or association persisted, read back
//! through `DescribeProductAsAdmin`, `ListProvisioningArtifacts` and `DescribeProduct`,
//! never against a value this file hands the service and takes straight out of the same
//! response.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    seed_launch_path, call, create_portfolio, create_product_of_type, emulator,
    error_code, is_catalog_id, message, ACCOUNT_ID, DEFAULT_REGION,
};

/// The fixed text of this operation's not-found sentence: everything but the identifier
/// the request carried, which follows it. `DescribeProduct` words its own refusal
/// differently (`" does not exist or access was denied"`, pinned in
/// `end_user_product_view_messages`), so the two templates are not shared and this one is
/// pinned here.
const VIEW_NOT_FOUND_PREFIX: &str = "Product not found for ProductViewId: ";

/// The fixed text of the sibling end-user read's sentence, which this operation must NOT
/// answer with.
const PRODUCT_SENTENCE: &str = " does not exist or access was denied";

/// The modelled upper bound on an error message: `ResourceNotFoundException$Message`
/// targets `ErrorMessage`, `@length 1..1024`
/// (`smithy_query query="shape" shape="ErrorMessage"`).
const MAX_ERROR_MESSAGE: usize = 1024;

/// How long a poll keeps looking for a condition the emulator's clock owes before it
/// gives up. Generous: it bounds nothing this suite asserts about timing, only how long
/// a wait may take on a loaded host.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// One `DescribeProductView` call.
async fn describe_view(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(router, "DescribeProductView", &json!({"Id": id})).await
}

/// The whole stored record of `product_id` as the ADMIN view reports it.
async fn stored_detail(router: &MultiServiceRouter, product_id: &str) -> Value {
    let (status, described) =
        call(router, "DescribeProductAsAdmin", &json!({"Id": product_id})).await;
    assert_eq!(
        status, 200,
        "DescribeProductAsAdmin({product_id}): expected 200, got {status} with {described}"
    );
    described
}

/// The stored `ProductViewSummary` of `product_id`.
async fn stored_summary(router: &MultiServiceRouter, product_id: &str) -> Value {
    stored_detail(router, product_id).await["ProductViewDetail"]["ProductViewSummary"].clone()
}

/// The identifier the stored product view of `product_id` carries — the value
/// `ProductViewSummary$Id` reports, which is not the product's own id.
async fn stored_view_id(router: &MultiServiceRouter, product_id: &str) -> String {
    let summary = stored_summary(router, product_id).await;
    summary["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("the stored product view of {product_id} has no Id: {summary}"))
        .to_string()
}

/// The stored ARN of `product_id` — the one identifier a copy addresses its source by.
async fn stored_arn(router: &MultiServiceRouter, product_id: &str) -> String {
    let detail = stored_detail(router, product_id).await;
    detail["ProductViewDetail"]["ProductARN"]
        .as_str()
        .unwrap_or_else(|| panic!("the stored product {product_id} has no ARN: {detail}"))
        .to_string()
}

/// The stored versions of `product_id` as the ADMIN list reports them: artifact id ->
/// version name.
async fn stored_versions(
    router: &MultiServiceRouter,
    product_id: &str,
) -> BTreeMap<String, String> {
    let (status, listed) = call(
        router,
        "ListProvisioningArtifacts",
        &json!({"ProductId": product_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListProvisioningArtifacts({product_id}): expected 200, got {status} with {listed}"
    );
    listed["ProvisioningArtifactDetails"]
        .as_array()
        .unwrap_or_else(|| panic!("ListProvisioningArtifacts({product_id}): no list in {listed}"))
        .iter()
        .map(|detail| {
            let id = detail["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("a stored version with no Id: {detail}"))
                .to_string();
            let name = detail["Name"].as_str().unwrap_or_default().to_string();
            (id, name)
        })
        .collect()
}

/// The versions a `DescribeProductView` response reports: artifact id -> version name.
fn reported_versions(case: &str, body: &Value) -> BTreeMap<String, String> {
    body["ProvisioningArtifacts"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ProvisioningArtifacts list in {body}"))
        .iter()
        .map(|artifact| {
            let id = artifact["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a reported version with no Id: {artifact}"))
                .to_string();
            let name = artifact["Name"].as_str().unwrap_or_default().to_string();
            (id, name)
        })
        .collect()
}

/// Copy `source_arn` into `target_product_id`, which gains the source's versions, and
/// answer the `CopyProductToken` the copy was given.
async fn copy_into(
    router: &MultiServiceRouter,
    source_arn: &str,
    target_product_id: &str,
    token: &str,
) -> String {
    let (status, copied) = call(
        router,
        "CopyProduct",
        &json!({
            "SourceProductArn": source_arn,
            "TargetProductId": target_product_id,
            "IdempotencyToken": token,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CopyProduct into {target_product_id}: expected 200, got {status} with {copied}"
    );
    copied["CopyProductToken"]
        .as_str()
        .unwrap_or_else(|| panic!("CopyProduct into {target_product_id}: no token in {copied}"))
        .to_string()
}

/// Look at the copy `token` names until it reports `SUCCEEDED`, and answer the product it
/// copied into. The copy's completion is owed by the clock, so this polls for the
/// CONDITION rather than waiting a fixed time.
async fn copy_succeeds(router: &MultiServiceRouter, token: &str) -> String {
    let started = std::time::Instant::now();
    loop {
        let (status, reported) = call(
            router,
            "DescribeCopyProductStatus",
            &json!({"CopyProductToken": token}),
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeCopyProductStatus({token}): expected 200, got {status} with {reported}"
        );
        let copy_status = reported["CopyProductStatus"].as_str().unwrap_or_default();
        if copy_status == "SUCCEEDED" {
            return reported["TargetProductId"]
                .as_str()
                .unwrap_or_else(|| panic!("the finished copy {token} names no target: {reported}"))
                .to_string();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "the copy {token} must reach SUCCEEDED, last reported {copy_status:?} with {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One product whose view is read by both of the identifiers that address it.
struct Row {
    case: &'static str,
    /// A `ProductType` the model admits, so the row's view reports a different `Type`.
    product_type: &'static str,
    /// How many portfolios carry the product — which is how many launch paths it has,
    /// and therefore whether its view reports a default path.
    portfolios: usize,
    /// How many versions beyond the one the create minted the product carries.
    extra_versions: usize,
}

/// A product view reports the stored product and every version it holds, by the view's
/// own identifier and by the product's, whatever the product's type, however many
/// portfolios carry it and however many versions it has.
///
/// Rows differ along the `ProductType` (values from
/// `smithy_query query="shape" shape="ProductType"`), the number of launch paths the
/// product has (one, which gives the view a default path, and several, which do not) and
/// the number of versions it carries.
#[tokio::test]
async fn a_product_view_reports_the_stored_product_and_its_versions() {
    let router = emulator();
    // The product every extra version is copied from, kept out of every row's portfolio
    // so it is never one of the products a row reads.
    let donor_id = create_product_of_type(
        &router,
        "view-version-donor",
        "CLOUD_FORMATION_TEMPLATE",
        "tok-view-donor",
    )
    .await;
    let donor_arn = stored_arn(&router, &donor_id).await;

    let rows = [
        Row {
            case: "one launch path, one version, a CloudFormation product",
            product_type: "CLOUD_FORMATION_TEMPLATE",
            portfolios: 1,
            extra_versions: 0,
        },
        Row {
            case: "two launch paths, one version, a Marketplace product",
            product_type: "MARKETPLACE",
            portfolios: 2,
            extra_versions: 0,
        },
        Row {
            case: "one launch path, two versions, a Terraform product",
            product_type: "TERRAFORM_OPEN_SOURCE",
            portfolios: 1,
            extra_versions: 1,
        },
        Row {
            case: "three launch paths, three versions, a CloudFormation product",
            product_type: "CLOUD_FORMATION_TEMPLATE",
            portfolios: 3,
            extra_versions: 2,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let product_id = create_product_of_type(
            &router,
            &format!("view-product-{index}"),
            row.product_type,
            &format!("tok-view-product-{index}"),
        )
        .await;
        for portfolio in 0..row.portfolios {
            let portfolio_id = create_portfolio(
                &router,
                &format!("view-portfolio-{index}-{portfolio}"),
                "acme",
                &[],
                &format!("tok-view-port-{index}-{portfolio}"),
            )
            .await;
            seed_launch_path(&router, &product_id, &portfolio_id).await;
        }
        for extra in 0..row.extra_versions {
            let token = copy_into(
                &router,
                &donor_arn,
                &product_id,
                &format!("tok-view-extra-{index}-{extra}"),
            )
            .await;
            let target = copy_succeeds(&router, &token).await;
            assert_eq!(
                target, product_id,
                "case {case:?}: the copy must have gone into this product, got {target}"
            );
        }

        let view_id = stored_view_id(&router, &product_id).await;
        assert!(
            is_catalog_id("prodview", &view_id),
            "case {case:?}: the view's own identifier must be a product view id, got {view_id:?}"
        );
        let summary = stored_summary(&router, &product_id).await;
        let versions = stored_versions(&router, &product_id).await;
        assert_eq!(
            versions.len(),
            1 + row.extra_versions,
            "case {case:?}: the product must carry {} stored version(s), got {versions:?}",
            1 + row.extra_versions
        );

        // Both identifiers address the same one view, and both report it identically.
        let mut bodies: Vec<(String, Value)> = Vec::new();
        for (addressed, identifier) in [("the view's Id", &view_id), ("the ProductId", &product_id)]
        {
            let case = format!("{case}: by {addressed}");
            let (status, body) = describe_view(&router, identifier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} with {body}"
            );
            assert_eq!(
                body["ProductViewSummary"], summary,
                "case {case:?}: the view must report the stored summary, got {body}"
            );
            assert_eq!(
                reported_versions(&case, &body),
                versions,
                "case {case:?}: the view must report the stored versions, got {body}"
            );
            assert_eq!(
                body["ProductViewSummary"]["Type"].as_str(),
                Some(row.product_type),
                "case {case:?}: the view reports the stored product type, got {body}"
            );
            assert_eq!(
                body["ProductViewSummary"]["HasDefaultPath"].as_bool(),
                Some(row.portfolios == 1),
                "case {case:?}: a product with {} launch path(s) has a default path only when it \
                 has exactly one, got {body}",
                row.portfolios
            );
            bodies.push((case, body));
        }
        assert_eq!(
            bodies[0].1, bodies[1].1,
            "case {case:?}: the view id and the product id must report the same view, got {} then \
             {}",
            bodies[0].1, bodies[1].1
        );

        // The end-user read of the same product reports the same summary and versions:
        // one product cannot read two ways.
        let (status, by_product) = call(&router, "DescribeProduct", &json!({"Id": product_id})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeProduct expected 200, got {status} with {by_product}"
        );
        assert_eq!(
            by_product["ProductViewSummary"], bodies[0].1["ProductViewSummary"],
            "case {case:?}: both end-user reads must report the same summary, got {by_product}"
        );
        assert_eq!(
            by_product["ProvisioningArtifacts"], bodies[0].1["ProvisioningArtifacts"],
            "case {case:?}: both end-user reads must report the same versions, got {by_product}"
        );

        // Reading a view changes nothing a later caller sees.
        let (status, again) = describe_view(&router, &view_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a second read expected 200, got {status} with {again}"
        );
        assert_eq!(
            again, bodies[0].1,
            "case {case:?}: a read must not change what the next one reports, got {again}"
        );
        assert_eq!(
            stored_versions(&router, &product_id).await,
            versions,
            "case {case:?}: a read must not change the stored versions"
        );
        assert_eq!(
            stored_summary(&router, &product_id).await,
            summary,
            "case {case:?}: a read must not change the stored product"
        );
    }
}

/// An identifier that resolves no reachable product view is refused with this
/// operation's own sentence, whatever shape it has and whether it names nothing at all or
/// a stored product this view cannot reach.
///
/// Rows span the identifier shapes the modelled `Id` admits (`@length 1..100`, pattern
/// `^[a-zA-Z0-9_\-]*$`, `smithy_query query="operation" operation="DescribeProductView"`):
/// the minimum and the maximum length, the prefixed form a product view carries, the one
/// a product carries, one of another resource kind, and one carrying each admitted
/// punctuation. Anything outside those constraints never reaches the handler.
#[tokio::test]
async fn an_unreachable_view_is_refused_with_the_product_view_sentence() {
    let router = emulator();
    // A product that IS in view, so every refusal below is about the identifier the
    // request carried rather than about an empty catalog.
    let portfolio_id =
        create_portfolio(&router, "refusal-portfolio", "acme", &[], "tok-refusal-port").await;
    let visible_id = create_product_of_type(
        &router,
        "refusal-visible",
        "CLOUD_FORMATION_TEMPLATE",
        "tok-refusal-visible",
    )
    .await;
    seed_launch_path(&router, &visible_id, &portfolio_id).await;
    let (status, in_view) = describe_view(&router, &stored_view_id(&router, &visible_id).await).await;
    assert_eq!(
        status, 200,
        "the seeded product must be in view, got {status} with {in_view}"
    );

    // A product that is stored but sits in no portfolio: out of this view entirely.
    let hidden_id = create_product_of_type(
        &router,
        "refusal-hidden",
        "CLOUD_FORMATION_TEMPLATE",
        "tok-refusal-hidden",
    )
    .await;
    let hidden_view_id = stored_view_id(&router, &hidden_id).await;

    let longest = format!("prodview-{}", "a".repeat(91));
    let rows: [(&str, String); 8] = [
        ("the minimum length", "q".to_string()),
        (
            "the prefixed form a product view carries",
            "prodview-zzzzzzzzzzzz".to_string(),
        ),
        (
            "the prefixed form a product carries",
            "prod-zzzzzzzzzzzz".to_string(),
        ),
        (
            "an id of another resource kind",
            "port-zzzzzzzzzzzz".to_string(),
        ),
        ("underscores and dashes", "prod_view-7_X".to_string()),
        ("the maximum length", longest.clone()),
        ("a stored product out of view, by its view id", hidden_view_id.clone()),
        ("a stored product out of view, by its product id", hidden_id.clone()),
    ];

    for (case, identifier) in rows {
        assert!(
            !identifier.is_empty() && identifier.len() <= 100,
            "case {case:?}: the identifier must satisfy the modelled length, got {} characters",
            identifier.len()
        );
        let (status, refused) = describe_view(&router, &identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: unexpected error in {refused}"
        );
        let reported = message(&refused);
        assert_eq!(
            reported,
            format!("{VIEW_NOT_FOUND_PREFIX}{identifier}"),
            "case {case:?}: unexpected message, got {reported:?}"
        );
        assert!(
            !reported.contains(PRODUCT_SENTENCE),
            "case {case:?}: this read must not answer the product read's sentence, got \
             {reported:?}"
        );
        assert!(
            !reported.is_empty() && reported.len() <= MAX_ERROR_MESSAGE,
            "case {case:?}: the message must satisfy the modelled length, got {} characters in \
             {reported:?}",
            reported.len()
        );
    }

    // The hidden product really was stored all along: the ADMIN view reported it, and
    // giving it a launch path is what brings it into this view — so the two refusals
    // above were the view's scoping and not a broken lookup.
    let hidden_summary = stored_summary(&router, &hidden_id).await;
    assert_eq!(
        hidden_summary["ProductId"].as_str(),
        Some(hidden_id.as_str()),
        "the hidden product must be stored, got {hidden_summary}"
    );
    seed_launch_path(&router, &hidden_id, &portfolio_id).await;
    for (case, identifier) in [
        ("by its view id", &hidden_view_id),
        ("by its product id", &hidden_id),
    ] {
        let (status, described) = describe_view(&router, identifier).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a product on a launch path is in view, got {status} with {described}"
        );
        assert_eq!(
            described["ProductViewSummary"]["ProductId"].as_str(),
            Some(hidden_id.as_str()),
            "case {case:?}: it is the stored product, got {described}"
        );
    }
}

/// The product a copy minted is out of this view until a portfolio carries it, and once
/// one does the view reports the copy's OWN versions — freshly minted ids, not the
/// source's — under the copy's own view identifier.
#[tokio::test]
async fn a_copied_product_comes_into_view_with_its_own_versions() {
    let router = emulator();
    let portfolio_id =
        create_portfolio(&router, "copy-portfolio", "acme", &[], "tok-copy-port").await;
    let source_id = create_product_of_type(
        &router,
        "copy-source",
        "CLOUD_FORMATION_TEMPLATE",
        "tok-copy-source",
    )
    .await;
    seed_launch_path(&router, &source_id, &portfolio_id).await;
    let source_view_id = stored_view_id(&router, &source_id).await;
    let source_versions = stored_versions(&router, &source_id).await;
    let source_arn = stored_arn(&router, &source_id).await;

    // The copy mints a new product, so the request names no target.
    let (status, copied) = call(
        &router,
        "CopyProduct",
        &json!({"SourceProductArn": source_arn, "IdempotencyToken": "tok-copy-run"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "CopyProduct: expected 200, got {status} with {copied}"
    );
    let token = copied["CopyProductToken"]
        .as_str()
        .unwrap_or_else(|| panic!("CopyProduct: no token in {copied}"))
        .to_string();
    let copy_id = copy_succeeds(&router, &token).await;
    assert_ne!(
        copy_id, source_id,
        "the copy must be its own product, got {copy_id}"
    );
    let copy_view_id = stored_view_id(&router, &copy_id).await;
    assert_ne!(
        copy_view_id, source_view_id,
        "the copy must carry its own product view, got {copy_view_id}"
    );

    // No portfolio carries the copy yet, so it is outside this view — by either
    // identifier — while the source it was copied from stays in view.
    for (case, identifier) in [
        ("the copy, by its view id", &copy_view_id),
        ("the copy, by its product id", &copy_id),
    ] {
        let (status, refused) = describe_view(&router, identifier).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} with {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundException",
            "case {case:?}: unexpected error in {refused}"
        );
        assert_eq!(
            message(&refused),
            format!("{VIEW_NOT_FOUND_PREFIX}{identifier}"),
            "case {case:?}: unexpected message, got {:?}",
            message(&refused)
        );
    }
    let (status, source_in_view) = describe_view(&router, &source_view_id).await;
    assert_eq!(
        status, 200,
        "the source must stay in view, got {status} with {source_in_view}"
    );

    // Giving the copy a launch path brings it into view with its own versions.
    seed_launch_path(&router, &copy_id, &portfolio_id).await;
    let (status, described) = describe_view(&router, &copy_view_id).await;
    assert_eq!(
        status, 200,
        "the copy must be in view once a portfolio carries it, got {status} with {described}"
    );
    assert_eq!(
        described["ProductViewSummary"]["ProductId"].as_str(),
        Some(copy_id.as_str()),
        "the view must report the copy's own product, got {described}"
    );
    let reported = reported_versions("the copy", &described);
    assert_eq!(
        reported,
        stored_versions(&router, &copy_id).await,
        "the view must report the copy's stored versions, got {described}"
    );
    assert_eq!(
        reported.len(),
        source_versions.len(),
        "the copy carries one version per copied source version, got {reported:?}"
    );
    let copied_ids: BTreeSet<&String> = reported.keys().collect();
    let source_ids: BTreeSet<&String> = source_versions.keys().collect();
    assert!(
        copied_ids.is_disjoint(&source_ids),
        "a copied version is a version of the copy, so it carries its own id: {copied_ids:?} \
         against {source_ids:?}"
    );
    assert_eq!(
        described["ProductViewSummary"]["Name"].as_str(),
        stored_summary(&router, &copy_id).await["Name"].as_str(),
        "the view must report the copy's stored name, got {described}"
    );

    // The source's own view is untouched by all of that, and still names the source.
    let (status, source_again) = describe_view(&router, &source_view_id).await;
    assert_eq!(
        status, 200,
        "the source must still be in view, got {status} with {source_again}"
    );
    assert_eq!(
        reported_versions("the source", &source_again),
        source_versions,
        "the source's versions must be unchanged, got {source_again}"
    );
    assert_eq!(
        source_again["ProductViewSummary"]["ProductId"].as_str(),
        Some(source_id.as_str()),
        "the source's view must still name the source, got {source_again}"
    );
    // The products the copy and the source are live in the one emulated account, whose
    // ARNs every product of this service carries.
    assert!(
        stored_arn(&router, &copy_id)
            .await
            .contains(&format!(":{ACCOUNT_ID}:")),
        "the copy must be a product of the emulated account"
    );
    assert!(
        stored_arn(&router, &copy_id).await.contains(DEFAULT_REGION),
        "the copy must be a product of the region the request was signed for"
    );
}
