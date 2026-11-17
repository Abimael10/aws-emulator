// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SCOPE of the image-build read plane, driven over the same
//! router the emulator boots.
//!
//! RULE: `ListMicrovmImageBuilds` and `GetMicrovmImageBuild` report the builds held by
//! exactly the image version the request names, so a build record never crosses between
//! sibling images: every build a listing serves carries the named image's ARN and
//! version, whatever filter, page size or identifier form (`MicrovmImageIdentifier` is
//! "the unique identifier (ARN or ID)") the request used, and a `buildId` another image
//! holds is `ResourceNotFoundException` (HTTP 404) under this one.
//!
//! Each test states that rule and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The wire harness
//! (`emulator`, `call`, `image_body`, …) is the one `microvm_lifecycle` owns, and the
//! build-plane helpers are the ones `image_build_read_path` owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    image_build_not_found_message, FIRST_IMAGE_VERSION,
};

use crate::image_build_read_path::{
    arm64_targets, create_image, get_build, list_builds, page_build_ids, page_items,
};
use crate::microvm_lifecycle::{as_uri_value, call, emulator};

/// One image under test: the case that describes it, the name it was created under, the
/// ARN its create minted, and the `buildId`s its own first version holds.
struct ScopedImage {
    case: &'static str,
    name: &'static str,
    image_arn: String,
    build_ids: Vec<String>,
}

/// RULE: the build plane is scoped to the image version named, so each image's listing
/// serves only the builds its own version holds — identically whether the image is
/// addressed by name or by ARN — and the build ids of two images are disjoint.
#[tokio::test]
async fn each_image_listing_serves_only_its_own_builds() {
    let router = emulator();
    let images = scoped_images(&router).await;

    // Rows: every query this listing publishes, over every image — no query at all, a
    // page smaller than the build set, the architecture filter, and each chipset
    // generation the service builds (`state::BUILD_TARGETS`, via `arm64_targets`).
    let mut queries: Vec<String> = vec![
        String::new(),
        "maxResults=1".to_string(),
        "architecture=ARM_64".to_string(),
    ];
    queries.extend(
        arm64_targets()
            .iter()
            .map(|(_, generation)| format!("chipsetGeneration={generation}")),
    );

    for image in &images {
        for query in &queries {
            let case = format!("{:?} with {query:?}", image.case);
            let (status, body) =
                list_builds(&router, &image.image_arn, FIRST_IMAGE_VERSION, query).await;
            assert_eq!(status, 200, "case {case}: expected 200, got {status} {body}");

            for item in page_items(&body, &case) {
                assert_eq!(
                    item["imageArn"].as_str(),
                    Some(image.image_arn.as_str()),
                    "case {case}: every build served names the image asked for, got {:?}",
                    item["imageArn"]
                );
                assert_eq!(
                    item["imageVersion"].as_str(),
                    Some(FIRST_IMAGE_VERSION),
                    "case {case}: every build served names the version asked for, got {:?}",
                    item["imageVersion"]
                );
                let build_id = item["buildId"].as_str().unwrap_or_default().to_string();
                assert!(
                    image.build_ids.contains(&build_id),
                    "case {case}: expected a build of {:?} ({:?}), got {build_id}",
                    image.name,
                    image.build_ids
                );
            }
        }
    }

    // The build sets of two images are disjoint, so the scope assertions above can tell
    // one image's builds from another's at all.
    for image in &images {
        for sibling in &images {
            if sibling.name == image.name {
                continue;
            }
            for build_id in &image.build_ids {
                assert!(
                    !sibling.build_ids.contains(build_id),
                    "case {:?} vs {:?}: two images hold distinct builds, got {build_id} in both",
                    image.case,
                    sibling.case
                );
            }
        }
    }
}

/// RULE: a `buildId` the named image version does not hold is `ResourceNotFoundException`
/// (HTTP 404) naming what was missing — including a build id a SIBLING image of the same
/// account holds, which this image's listing therefore never serves either.
#[tokio::test]
async fn a_sibling_images_build_is_not_found_under_this_one() {
    let router = emulator();
    let images = scoped_images(&router).await;

    for image in &images {
        let (status, body) = list_builds(&router, &image.image_arn, FIRST_IMAGE_VERSION, "").await;
        assert_eq!(
            status, 200,
            "case {:?}: listing got {status} {body}",
            image.case
        );
        let served = page_build_ids(&body, image.case);

        for sibling in &images {
            if sibling.name == image.name {
                continue;
            }
            for build_id in &sibling.build_ids {
                let case = format!("{:?} reading {:?}'s {build_id}", image.case, sibling.case);
                assert!(
                    !served.contains(build_id),
                    "case {case}: a listing serves no sibling's build, got {served:?}"
                );

                let (status, body) =
                    get_build(&router, &image.image_arn, FIRST_IMAGE_VERSION, build_id).await;
                assert_eq!(status, 404, "case {case}: expected 404, got {status} {body}");
                assert_eq!(
                    body["__type"].as_str(),
                    Some("ResourceNotFoundException"),
                    "case {case}: expected ResourceNotFoundException, got {body}"
                );
                let expected =
                    image_build_not_found_message(&image.image_arn, FIRST_IMAGE_VERSION, build_id);
                assert_eq!(
                    body["message"].as_str(),
                    Some(expected.as_str()),
                    "case {case}: expected the refusal naming what was missing, got {:?}",
                    body["message"]
                );
            }
        }
    }
}

/// RULE: a build is addressed BY its `buildId`, so the single-build read answers the very
/// identifier the request named — the same characters, on every read, and whichever form of
/// the image identifier carried it — and that is the identifier the listing reports for the
/// row, because `GetMicrovmImageBuildOutput$buildId` and `MicrovmImageBuildSummary$buildId`
/// target ONE shape (`NonBlankString`, `@length` 1..2048, pattern `^[^\s]+$`) holding one
/// stored value. One id names one build, so the handle a client keeps round-trips: an
/// equality check against the id it asked for holds, and a map keyed on it keeps one entry
/// per build.
#[tokio::test]
async fn a_build_read_echoes_the_identifier_it_was_addressed_by() {
    let router = emulator();
    let images = scoped_images(&router).await;

    // Rows span both forms `MicrovmImageIdentifier` accepts for the image a build belongs
    // to ("the unique identifier (ARN or ID)"), over every image `scoped_images` created —
    // each from a different create input, so the ids read back here are minted at several
    // points of the store's own allocation rather than at its first one only.
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for image in &images {
        rows.push((
            format!("{:?} addressed by its ARN", image.case),
            image.image_arn.clone(),
            image.image_arn.clone(),
        ));
        rows.push((
            format!("{:?} addressed by its name", image.case),
            image.name.to_string(),
            image.image_arn.clone(),
        ));
    }

    // Which build each id was minted for, so an id two builds of the store shared would
    // fail here: the id is the only handle a client has on a build.
    let mut minted_for: BTreeMap<String, String> = BTreeMap::new();

    for (case, identifier, image_arn) in &rows {
        let (status, body) = list_builds(&router, identifier, FIRST_IMAGE_VERSION, "").await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200 from the listing, got {status} {body}"
        );
        let listed = page_build_ids(&body, case);
        assert!(
            !listed.is_empty(),
            "case {case}: the version holds at least one build, got {body}"
        );

        for build_id in &listed {
            // A value the member may hold at all: 1..2048 characters, and the pattern
            // `^[^\s]+$` forbids whitespace ANYWHERE in it — leading and trailing included.
            let length = build_id.chars().count();
            assert!(
                (1..=2048).contains(&length) && !build_id.contains(char::is_whitespace),
                "case {case}: a build id holds 1..2048 characters and no whitespace, got \
                 {build_id:?} ({length} characters)"
            );

            let owner = format!("{image_arn} at {FIRST_IMAGE_VERSION}");
            match minted_for.get(build_id) {
                Some(first) => assert_eq!(
                    first, &owner,
                    "case {case}: {build_id:?} is the id of ONE build, got it for {first} and \
                     for {owner}"
                ),
                None => {
                    minted_for.insert(build_id.clone(), owner);
                }
            }

            // The read is an ECHO of the key the URI carried, and a second read answers the
            // same characters — a report, not a mint.
            for read in ["the first read", "a second read"] {
                let (status, body) =
                    get_build(&router, identifier, FIRST_IMAGE_VERSION, build_id).await;
                assert_eq!(
                    status, 200,
                    "case {case}, {read} of {build_id:?}: expected 200, got {status} {body}"
                );
                assert_eq!(
                    body["buildId"].as_str(),
                    Some(build_id.as_str()),
                    "case {case}, {read}: the read answers the identifier it was addressed by \
                     ({build_id:?}), got {:?}",
                    body["buildId"]
                );
            }
        }
    }

    assert_eq!(
        minted_for.len(),
        images
            .iter()
            .map(|image| image.build_ids.len())
            .sum::<usize>(),
        "every build of the store carries its own id, got {minted_for:?}"
    );
}

/// RULE: `buildId` targets `NonBlankString`, whose pattern `^[^\s]+$` admits no whitespace
/// anywhere, so a value carrying any is not a value that member may hold — a request that
/// addresses a build by one can never succeed and is refused with `ValidationException`
/// (HTTP 400) rather than served or reported missing. This is what makes the echo above
/// load-bearing: an identifier the read padded or spaced would not be addressable again.
#[tokio::test]
async fn a_build_id_carrying_whitespace_addresses_no_build() {
    let router = emulator();
    let images = scoped_images(&router).await;
    let image = &images[0];
    let held = image
        .build_ids
        .first()
        .expect("the created version holds a build")
        .clone();

    // Rows span where the whitespace sits in the value — before it, after it, inside it —
    // and the value that is whitespace alone, each as the percent-encoded path label a
    // client would have to send to carry it at all. The split is taken on a CHARACTER
    // boundary, so the row holds whatever characters the minted id is made of.
    let middle = held
        .char_indices()
        .nth(held.chars().count() / 2)
        .map(|(index, _)| index)
        .unwrap_or_default();
    let (head, tail) = held.split_at(middle);
    assert!(
        !head.is_empty() && !tail.is_empty(),
        "the embedded-whitespace row needs a split INSIDE the id, so it stays a different \
         case from the leading one, got {held:?}"
    );
    let rows: Vec<(&str, String)> = vec![
        ("a leading space", format!("%20{held}")),
        ("a trailing space", format!("{held}%20")),
        ("a trailing newline", format!("{held}%0A")),
        ("an embedded space", format!("{head}%20{tail}")),
        ("whitespace alone", "%20".to_string()),
    ];

    for (case, label) in rows {
        // Only the build label is hand-encoded: the image identifier and the version go
        // through the harness's own path-value guard, which refuses a value that would
        // need escaping.
        let uri = format!(
            "/2025-09-09/microvm-images/{}/versions/{}/builds/{}",
            as_uri_value(&image.image_arn),
            as_uri_value(FIRST_IMAGE_VERSION),
            label
        );
        let (status, body) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a buildId carrying whitespace is no value this member may hold, \
             got {status} {body}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some("ValidationException"),
            "case {case:?}: expected ValidationException, got {body}"
        );
    }
}

/// The images every test here runs over. Rows differ in the build inputs the create
/// carried — what the fan-out generalises over — so the images hold different build sets
/// rather than copies of one, and each row's listing is read back through BOTH identifier
/// forms to prove they address the same version.
async fn scoped_images(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<ScopedImage> {
    let rows: Vec<(&'static str, &'static str, Value)> = vec![
        (
            "the configuration the service supplies",
            "scoped-defaulted",
            json!({}),
        ),
        (
            "an explicit architecture and memory floor",
            "scoped-explicit",
            json!({
                "cpuConfigurations": [{"architecture": "ARM_64"}],
                "resources": [{"minimumMemoryInMiB": 512}],
            }),
        ),
        (
            "the same architecture named twice",
            "scoped-repeated",
            json!({"cpuConfigurations": [
                {"architecture": "ARM_64"},
                {"architecture": "ARM_64"},
            ]}),
        ),
    ];

    let mut images: Vec<ScopedImage> = Vec::new();
    for (case, name, extra) in rows {
        let image_arn = create_image(router, name, extra).await;

        let (status, by_arn) = list_builds(router, &image_arn, FIRST_IMAGE_VERSION, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing by ARN got {status} {by_arn}"
        );
        let (status, by_name) = list_builds(router, name, FIRST_IMAGE_VERSION, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing by name got {status} {by_name}"
        );
        assert_eq!(
            by_name, by_arn,
            "case {case:?}: both identifier forms address one version, got {by_name} and {by_arn}"
        );

        let build_ids = page_build_ids(&by_arn, case);
        assert!(
            !build_ids.is_empty(),
            "case {case:?}: one create fans out into at least one build, got {by_arn}"
        );
        images.push(ScopedImage {
            case,
            name,
            image_arn,
            build_ids,
        });
    }
    images
}
