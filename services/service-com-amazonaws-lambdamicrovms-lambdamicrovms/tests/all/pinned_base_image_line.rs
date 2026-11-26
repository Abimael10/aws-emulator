// SPDX-License-Identifier: CC0-1.0

//! What a version record's `baseImageVersion` carries: the value a request PINNED, and —
//! for a request that pinned none — the release the named base image publishes.
//!
//! `baseImageVersion` is "the specific version of the base MicroVM image to use" and
//! targets `Version` (`@length 1..2048`, `@pattern ^[^\s]+$`). WHICH release an unpinned
//! build resolves to belongs to the base image: an AWS-managed one publishes its releases
//! through AWS's own catalogue, which republishes it as releases ship, so the concrete
//! label is a value of the moment rather than a contract. What IS the contract, and what
//! this suite pins, is the form of that value and the way every reader of the record
//! reports it:
//!
//! 1. a value the request PINNED is recorded verbatim — a generation the catalogue
//!    addresses a release by (what `ListManagedMicrovmImageVersions` publishes and what a
//!    client pins) as much as a `major.minor` release label — so a client that wants a
//!    fixed pedigree reads back exactly what it asked for;
//! 2. a RESOLVED pedigree is a `major.minor` release label, never one of those generation
//!    counters, and it is a release the named base image really publishes: the
//!    catalogue's for a managed base image, the version it serves for an image of this
//!    account;
//! 3. the value is STABLE: the create's answer, the version read, the listing row and a
//!    status publication all report the one stored value, and reading it never changes
//!    it.
//!
//! The rows differ along what the rule generalises over: the namespace the base image is
//! in (AWS-managed, and an image of this account), each generation the catalogue
//! publishes, a label of neither form, and the reader the value is read back through.
//!
//! Every call goes through the shared wire harness, the same front door a client uses.

use serde_json::{json, Value};

use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    latest_managed_base_image_version_label, managed_base_image_arn,
    published_managed_base_image_versions, FIRST_IMAGE_VERSION, MANAGED_BASE_IMAGE_IDS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::{emulator, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, image_body, REGION};

/// The bucket and key the harness's default `codeArtifact` names, so a build that names
/// it is SERVED.
const ARTIFACT_BUCKET: &str = "artifacts";
const ARTIFACT_KEY: &str = "app.zip";

/// A member's text, or the empty string when the member is absent.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// Create the image `name` with whatever `extra` adds to the request, asserting the 201.
async fn create(router: &MultiServiceRouter, name: &str, extra: Value) -> Value {
    let (status, body) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, extra)),
    )
    .await;
    assert_eq!(
        status, 201,
        "create {name:?}: expected 201, got {status} {body}"
    );
    body
}

/// Whether `value` is a `major.minor` release label: two runs of digits around one dot,
/// which is the form a base-image version is published in ([`FIRST_IMAGE_VERSION`]).
fn is_release_label(value: &str) -> bool {
    match value.split_once('.') {
        Some((major, minor)) => {
            !major.is_empty()
                && !minor.is_empty()
                && major.bytes().all(|byte| byte.is_ascii_digit())
                && minor.bytes().all(|byte| byte.is_ascii_digit())
        }
        None => false,
    }
}

/// One row of the version listing of `identifier`, for the version `image_version`.
async fn version_row(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    case: &str,
) -> Value {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions",
        as_uri_value(identifier)
    );
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the version listing, got {status} {body}"
    );
    body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|row| row["imageVersion"].as_str() == Some(image_version))
        .unwrap_or_else(|| panic!("case {case:?}: the listing must hold {image_version:?}"))
}

/// Publish `status_value` for one version of one image, asserting the 200.
async fn publish_status(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    status_value: &str,
    case: &str,
) -> Value {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{}",
        as_uri_value(identifier),
        as_uri_value(image_version)
    );
    let (status, body) = call(router, "PATCH", &uri, Some(json!({"status": status_value}))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the status publication, got {status} {body}"
    );
    body
}

#[tokio::test]
async fn a_pinned_base_image_version_is_recorded_verbatim() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    let managed_id = MANAGED_BASE_IMAGE_IDS[0];
    let managed_base = managed_base_image_arn(REGION, managed_id);
    // The rows span the forms a client pins: every generation counter the catalogue
    // publishes for that base image — read off the catalogue rather than written out — a
    // release label of the published form, and a value of neither form.
    let mut pinned: Vec<String> = published_managed_base_image_versions(managed_id)
        .into_iter()
        .map(ToString::to_string)
        .collect();
    assert!(
        !pinned.is_empty(),
        "the catalogue must publish a generation of {managed_id} for a row to pin"
    );
    pinned.push("3.1".to_string());
    pinned.push("edge".to_string());

    for (index, value) in pinned.iter().enumerate() {
        let case = format!("a create pinning {value:?}");
        let created = create(
            &router,
            &format!("pinned-base-version-{index}"),
            json!({"baseImageArn": managed_base.clone(), "baseImageVersion": value}),
        )
        .await;
        assert_eq!(
            text(&created, "baseImageVersion"),
            *value,
            "case {case:?}: a pinned pedigree is recorded verbatim, got {created}"
        );

        // The response is the stored record, so every reader of that version reports the
        // pinned value unchanged.
        let image_arn = text(&created, "imageArn");
        let image_version = text(&created, "imageVersion");
        assert_eq!(
            image_version, FIRST_IMAGE_VERSION,
            "case {case:?}: a new image is addressed by this service's first version, got \
             {image_version:?}"
        );
        let (status, read) = get_version(&router, &image_arn, &image_version).await;
        assert_eq!(
            (status, text(&read, "baseImageVersion")),
            (200, value.clone()),
            "case {case:?}: the version read must report the pinned pedigree, got {read}"
        );
        let row = version_row(&router, &image_arn, &image_version, &case).await;
        assert_eq!(
            text(&row, "baseImageVersion"),
            *value,
            "case {case:?}: the listing row must report the pinned pedigree, got {row}"
        );
    }
}

#[tokio::test]
async fn an_unpinned_pedigree_is_a_release_the_base_image_publishes_and_stays_put() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    let managed_id = MANAGED_BASE_IMAGE_IDS[0];
    let managed_base = managed_base_image_arn(REGION, managed_id);
    let generations = published_managed_base_image_versions(managed_id);
    let release = latest_managed_base_image_version_label(managed_id)
        .unwrap_or_else(|| panic!("the catalogue publishes a release of {managed_id}"))
        .to_string();

    // An image of this account to build on as well, so the rule is asserted over both
    // namespaces a base image can be in.
    let own = create(&router, "pedigree-shape-own-base", json!({})).await;
    let own_base = text(&own, "imageArn");

    for (index, (case, base_image_arn)) in [
        ("the AWS-managed base image", managed_base.clone()),
        ("an image of this account", own_base.clone()),
    ]
    .iter()
    .enumerate()
    {
        let created = create(
            &router,
            &format!("pedigree-shape-{index}"),
            json!({"baseImageArn": base_image_arn}),
        )
        .await;
        let resolved = text(&created, "baseImageVersion");
        assert!(
            is_release_label(&resolved),
            "case {case:?}: a resolved pedigree is a major.minor release label, got \
             {resolved:?}"
        );
        assert!(
            !generations.contains(&resolved.as_str()),
            "case {case:?}: a resolved pedigree is never the generation counter the \
             catalogue addresses a release by, got {resolved:?} among {generations:?}"
        );
        if *case == "the AWS-managed base image" {
            assert_eq!(
                resolved, release,
                "case {case:?}: expected the release the catalogue's newest generation \
                 ships, got {resolved:?}"
            );
        }

        // STABLE: every reader of the record reports the one stored value, and reading it
        // again changes nothing.
        let image_arn = text(&created, "imageArn");
        let image_version = text(&created, "imageVersion");
        let (status, read) = get_version(&router, &image_arn, &image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the version read, got {status} {read}"
        );
        assert_eq!(
            text(&read, "baseImageVersion"),
            resolved,
            "case {case:?}: the version read must report the stored pedigree, got {read}"
        );
        let row = version_row(&router, &image_arn, &image_version, case).await;
        assert_eq!(
            text(&row, "baseImageVersion"),
            resolved,
            "case {case:?}: the listing row must report the stored pedigree, got {row}"
        );
        let published = publish_status(&router, &image_arn, &image_version, "INACTIVE", case).await;
        assert_eq!(
            text(&published, "baseImageVersion"),
            resolved,
            "case {case:?}: a status publication changes no pedigree, got {published}"
        );
        let (status, again) = get_version(&router, &image_arn, &image_version).await;
        assert_eq!(
            (status, text(&again, "baseImageVersion")),
            (200, resolved.clone()),
            "case {case:?}: reading the pedigree must not change it, got {again}"
        );
    }
}
