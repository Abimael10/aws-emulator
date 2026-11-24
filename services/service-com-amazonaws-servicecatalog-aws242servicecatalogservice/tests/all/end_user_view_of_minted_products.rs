// SPDX-License-Identifier: CC0-1.0

//! `DescribeProduct` — the END-USER read of a product — over the wire as awsJson1_1
//! requests through the same `MultiServiceRouter` the production emulator mounts.
//!
//! The rule under test: an end-user read reaches a product only through a LAUNCH PATH, so
//! an identifier naming a product no portfolio carries is refused exactly as one naming
//! nothing stored is — HTTP 400, `ResourceNotFoundException`, and the sentence that read
//! words its not-found with: for `DescribeProduct` the identifier the request carried
//! followed by the fixed text `" does not exist or access was denied"`, for the sibling
//! product-view read the fixed text `"Product not found for ProductViewId: "` followed by
//! it. That holds for every product, WHATEVER
//! minted it — a create, a copy into a new product, a copy into a product that already
//! carried versions — and once a portfolio carries the product the same read reports the
//! product's OWN stored versions and the launch path it was reached through.
//!
//! `end_user_product_view_messages` drives that sentence across the identifier SHAPES the
//! model admits; this suite drives it across the ways a product comes into existence,
//! which is where a copy's freshly minted product and version ids make an end-user read
//! resolve something the request never handed the service.
//!
//! Every assertion is about STORED state, read back through `DescribeProductAsAdmin` and
//! `ListProvisioningArtifacts`, never a value this file hands the service and takes
//! straight out of the same response.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    seed_launch_path, call, create_portfolio, create_product, emulator, error_code,
    message,
};

/// The fixed text of the product read's sentence: everything but the identifier the
/// request carried.
const END_USER_SENTENCE: &str = " does not exist or access was denied";

/// The fixed text of the product VIEW read's sentence, which precedes the identifier:
/// each end-user read words its own not-found for the resource it addresses, so the two
/// templates are not shared.
const VIEW_NOT_FOUND_PREFIX: &str = "Product not found for ProductViewId: ";

/// The sentence `operation` words its refusal of `identifier` with.
fn expected_refusal(operation: &str, identifier: &str) -> String {
    match operation {
        "DescribeProductView" => format!("{VIEW_NOT_FOUND_PREFIX}{identifier}"),
        _ => format!("{identifier}{END_USER_SENTENCE}"),
    }
}

/// How long a poll keeps looking for a condition the emulator's clock owes before it
/// gives up. Generous: it bounds nothing this suite asserts, only how long a wait may
/// take on a loaded host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The whole stored record of `product_id` as the ADMIN view reports it — the view that
/// sees a product no portfolio carries.
async fn stored_detail(router: &MultiServiceRouter, product_id: &str) -> Value {
    let (status, described) =
        call(router, "DescribeProductAsAdmin", &json!({"Id": product_id})).await;
    assert_eq!(
        status, 200,
        "DescribeProductAsAdmin({product_id}): expected 200, got {status} with {described}"
    );
    described
}

/// The stored name of `product_id` — the value `Name` addresses that product by.
async fn stored_name(router: &MultiServiceRouter, product_id: &str) -> String {
    let detail = stored_detail(router, product_id).await;
    detail["ProductViewDetail"]["ProductViewSummary"]["Name"]
        .as_str()
        .unwrap_or_else(|| panic!("the stored product {product_id} has no Name: {detail}"))
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

/// The ids of every version `product_id` stores, as the ADMIN list reports them.
async fn stored_version_ids(router: &MultiServiceRouter, product_id: &str) -> BTreeSet<String> {
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
            detail["Id"]
                .as_str()
                .unwrap_or_else(|| panic!("a stored version with no Id: {detail}"))
                .to_string()
        })
        .collect()
}

/// The ids of the versions a `DescribeProduct` response reports.
fn reported_version_ids(case: &str, body: &Value) -> BTreeSet<String> {
    body["ProvisioningArtifacts"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ProvisioningArtifacts list in {body}"))
        .iter()
        .map(|artifact| {
            artifact["Id"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a reported version with no Id: {artifact}")
                })
                .to_string()
        })
        .collect()
}

/// Start one copy of `source_arn` and answer the `CopyProductToken` it was given.
async fn start_copy(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, copied) = call(router, "CopyProduct", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CopyProduct expected 200, got {status} with {copied}"
    );
    copied["CopyProductToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CopyProductToken in {copied}"))
        .to_string()
}

/// Look at the copy `token` names until it reports `SUCCEEDED`, and answer the product it
/// copied into. A copy's completion is owed by the clock, so this polls for the
/// CONDITION rather than waiting a fixed time.
async fn copy_succeeds(router: &MultiServiceRouter, case: &str, token: &str) -> String {
    let started = Instant::now();
    loop {
        let (status, reported) = call(
            router,
            "DescribeCopyProductStatus",
            &json!({"CopyProductToken": token}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeCopyProductStatus expected 200, got {status} with {reported}"
        );
        let copy_status = reported["CopyProductStatus"].as_str().unwrap_or_default();
        if copy_status == "SUCCEEDED" {
            return reported["TargetProductId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the finished copy names no target: {reported}")
                })
                .to_string();
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: the copy must reach SUCCEEDED, last reported {copy_status:?} with \
             {reported}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert one end-user read of `identifier` is refused with the sentence that read words
/// its not-found with.
async fn assert_end_user_refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    identifier: &str,
) {
    let (status, refused) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} expected 400, got {status} with {refused}"
    );
    assert_eq!(
        error_code(&refused),
        "ResourceNotFoundException",
        "case {case:?}: {operation} unexpected error in {refused}"
    );
    let reported = message(&refused);
    assert_eq!(
        reported,
        expected_refusal(operation, identifier),
        "case {case:?}: {operation} unexpected message, got {reported:?}"
    );
}

/// One product out of the end-user view, and how it came into existence.
struct Row {
    case: &'static str,
    product_id: String,
    product_name: String,
    /// The versions this product stores — its OWN ids, which for a copy are not the
    /// source's.
    version_ids: BTreeSet<String>,
}

/// A product no portfolio carries is refused by the end-user reads with the sentence each
/// words its not-found with, whatever minted it, and the grant that puts it on a launch
/// path is what brings it into view with its own versions.
///
/// Rows differ along the way the product came into existence — a create, a copy into a
/// freshly minted product, and a copy into a product that already carried a version of
/// its own ("A name for the target product. The default is the name of the source
/// product", `smithy_query query="operation" operation="CopyProduct"`, so each row's
/// product carries a name of its own that addresses it alone) — and each row is read by
/// both members `DescribeProduct` addresses a product by and through the sibling end-user
/// read `DescribeProductView`.
#[tokio::test]
async fn a_product_out_of_view_is_refused_whatever_minted_it() {
    let router = emulator();

    // The catalog an end user CAN see: one portfolio carrying one product. Every refusal
    // below is therefore about the identifier the request carried rather than about an
    // empty catalog, and the source a copy reads is itself in view.
    let portfolio_id = create_portfolio(&router, "minted-portfolio", "acme", &[], "tok-mp").await;
    let source_id = create_product(&router, "minted-source", "tok-mp-source").await;
    seed_launch_path(&router, &source_id, &portfolio_id).await;
    let source_versions = stored_version_ids(&router, &source_id).await;
    let source_arn = stored_arn(&router, &source_id).await;

    // A product the end user cannot see, minted three ways. Both copies are started
    // before either is awaited, so the suite waits about one copy's span in all.
    let created_id = create_product(&router, "minted-created", "tok-mp-created").await;
    let existing_target_id =
        create_product(&router, "minted-existing-target", "tok-mp-existing").await;
    let existing_own_versions = stored_version_ids(&router, &existing_target_id).await;
    let into_new = start_copy(
        &router,
        "a copy into a new product",
        &json!({
            "SourceProductArn": source_arn,
            "TargetProductName": "minted-copy",
            "IdempotencyToken": "tok-mp-copy-new",
        }),
    )
    .await;
    let into_existing = start_copy(
        &router,
        "a copy into a product that already has a version",
        &json!({
            "SourceProductArn": source_arn,
            "TargetProductId": existing_target_id,
            "IdempotencyToken": "tok-mp-copy-existing",
        }),
    )
    .await;
    let copied_id = copy_succeeds(&router, "a copy into a new product", &into_new).await;
    let copied_into_existing = copy_succeeds(
        &router,
        "a copy into a product that already has a version",
        &into_existing,
    )
    .await;
    assert_eq!(
        copied_into_existing, existing_target_id,
        "the copy must land in the product the request named, got {copied_into_existing}"
    );
    assert!(
        copied_id != source_id && copied_id != existing_target_id,
        "a copy that names no target mints its own product, got {copied_id}"
    );

    let mut rows = Vec::new();
    for (case, product_id) in [
        ("a created product", created_id.clone()),
        ("a copy into a new product", copied_id.clone()),
        (
            "a copy into a product that already has a version",
            existing_target_id.clone(),
        ),
    ] {
        let version_ids = stored_version_ids(&router, &product_id).await;
        rows.push(Row {
            case,
            product_name: stored_name(&router, &product_id).await,
            product_id,
            version_ids,
        });
    }

    // A copy's versions are the copy's own: the ids the end-user read reports once the
    // product is in view are ones no request in this file ever handed the service.
    let copied_versions = &rows[1].version_ids;
    assert_eq!(
        copied_versions.len(),
        source_versions.len(),
        "a copy carries one version per copied source version, got {copied_versions:?} against \
         {source_versions:?}"
    );
    assert!(
        copied_versions.is_disjoint(&source_versions),
        "a copied version is a version of the copy, so it carries its own id: \
         {copied_versions:?} against {source_versions:?}"
    );
    assert_eq!(
        rows[2].version_ids.len(),
        existing_own_versions.len() + source_versions.len(),
        "the copy must add its versions to the ones the target already carried, got {:?} against \
         {existing_own_versions:?}",
        rows[2].version_ids
    );

    // Out of view, every end-user read of every row is refused with the sentence that
    // read words its not-found with, whichever member addressed the product.
    for row in &rows {
        for (operation, member, identifier) in [
            ("DescribeProduct", "Id", row.product_id.clone()),
            ("DescribeProduct", "Name", row.product_name.clone()),
            ("DescribeProductView", "Id", row.product_id.clone()),
        ] {
            let case = format!("{}, by {member}", row.case);
            assert_end_user_refusal(
                &router,
                &case,
                operation,
                &json!({member: identifier.clone()}),
                &identifier,
            )
            .await;
        }
    }

    // The product that IS on a launch path stayed readable all along, so the refusals
    // above were the view's scoping and not a broken lookup.
    let (status, in_view) = call(&router, "DescribeProduct", &json!({"Id": source_id})).await;
    assert_eq!(
        status, 200,
        "the product on a launch path must stay in view, got {status} with {in_view}"
    );

    // The grant is what brings each row into view, reporting the product's own versions
    // and the launch path it was reached through.
    for row in &rows {
        seed_launch_path(&router, &row.product_id, &portfolio_id).await;
        for (member, identifier) in [
            ("Id", row.product_id.clone()),
            ("Name", row.product_name.clone()),
        ] {
            let case = format!("{}, by {member}", row.case);
            let (status, described) =
                call(&router, "DescribeProduct", &json!({member: identifier})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a product on a launch path is in view, got {status} with \
                 {described}"
            );
            assert_eq!(
                described["ProductViewSummary"]["ProductId"].as_str(),
                Some(row.product_id.as_str()),
                "case {case:?}: the read must report the stored product, got {described}"
            );
            assert_eq!(
                reported_version_ids(&case, &described),
                row.version_ids,
                "case {case:?}: the read must report the product's stored versions, got \
                 {described}"
            );
            let paths: BTreeSet<String> = described["LaunchPaths"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no LaunchPaths list in {described}"))
                .iter()
                .map(|path| {
                    path["Id"]
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: a path with no Id: {path}"))
                        .to_string()
                })
                .collect();
            assert_eq!(
                paths,
                BTreeSet::from([portfolio_id.clone()]),
                "case {case:?}: the read must report the launch path it was reached through, got \
                 {described}"
            );
        }
    }

    // Nothing above changed what the source reports.
    assert_eq!(
        stored_version_ids(&router, &source_id).await,
        source_versions,
        "the copies and the grants must leave the source's versions as they were"
    );
}
