// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteMicrovmImageVersion`, driven over the same router the
//! emulator boots.
//!
//! RULE: a MicroVM image always holds at least one version, so deleting a version is
//! accepted only while the image still holds another one a delete has not taken: the
//! accepted call records that version `DELETING` and the removal the clock then owes
//! takes it, so the version's own read, the version listing and a launch pinned to it all
//! miss afterwards and the image's remaining versions are untouched, while a request to
//! delete the ONE version an image still holds is refused with HTTP 400
//! `ValidationException` and changes nothing.
//!
//! Each test states that rule and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The wire harness (`emulator`,
//! `call`, `image_body`, `poll_until`, …) is the one `microvm_lifecycle` owns, the version
//! read is `image_build_outcome`'s and the version listing is `image_version_listing`'s.

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    image_version_not_found_message, invalid_arn_format_message, managed_base_image_arn,
    no_active_version_message, version_address_image_not_found_message,
    FIRST_IMAGE_VERSION, IMAGE_STATE_UPDATED, IMAGE_STATE_UPDATE_FAILED,
    LAST_IMAGE_VERSION_REFUSAL, MANAGED_BASE_IMAGE_IDS, VERSION_STATE_DELETED,
    VERSION_STATE_DELETING, VERSION_STATE_SUCCESSFUL,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::get_version;
use crate::image_denial_message::expected_denial_sentence;
use crate::image_version_listing::list_versions;
use crate::microvm_lifecycle::{
    as_uri_value, call, emulator, expected_image_arn, image_body, poll_until, run_microvm, ACCOUNT,
    REGION,
};

/// A build role ARN: `RoleArn`, pattern
/// `^arn:aws[a-z\-]*:iam::[0-9]{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`.
const BUILD_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/microvm-version-delete";

/// The code artifact every build here points at: `CodeArtifact$uri` is "The URI of the
/// code artifact in Amazon S3", so it is an S3 object URI.
const CODE_ARTIFACT_URI: &str = "s3://artifacts/app.zip";

/// `DELETE /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}`.
pub(crate) async fn delete_version(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
) -> (u16, Value) {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{}",
        as_uri_value(identifier),
        as_uri_value(image_version)
    );
    call(router, "DELETE", &uri, None).await
}

/// `GET /2025-09-09/microvm-images/{imageIdentifier}`.
async fn get_image(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let uri = format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier));
    call(router, "GET", &uri, None).await
}

/// `PUT /2025-09-09/microvm-images/{imageIdentifier}` — the call that mints a FURTHER
/// version of an image, so a test can hold an image of several versions over the wire.
async fn update_image(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let body = json!({
        "baseImageArn": managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
        "buildRoleArn": BUILD_ROLE_ARN,
        "codeArtifact": {"uri": CODE_ARTIFACT_URI},
    });
    call(
        router,
        "PUT",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier)),
        Some(body),
    )
    .await
}

/// Look at the version `image_version` of the image `identifier` until its build has
/// SETTLED — the condition the emulator reaches on its own clock, never a duration this
/// test measures.
async fn poll_until_version_settled(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    image_version: &str,
) {
    poll_until(
        case,
        &format!("version {image_version} of {identifier} has settled"),
        || async {
            let (status, body) = get_version(router, identifier, image_version).await;
            status == 200 && body["state"] == json!(VERSION_STATE_SUCCESSFUL)
        },
    )
    .await;
}

/// Look at the version `image_version` of the image `identifier` until the deletion the
/// clock owes has taken it, so the read misses.
pub(crate) async fn poll_until_version_gone(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    image_version: &str,
) {
    poll_until(
        case,
        &format!("version {image_version} of {identifier} is gone"),
        || async {
            let (status, _) = get_version(router, identifier, image_version).await;
            status == 404
        },
    )
    .await;
}

/// Create the image `name` and give it `generations` settled versions — the create mints
/// the first and an update mints each further one — and hand back its ARN.
pub(crate) async fn image_with_versions(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    generations: usize,
) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, json!({}))),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: creating {name:?} expected 201, got {status} {created}"
    );
    let arn = created["imageArn"].as_str().unwrap_or_default().to_string();
    let mut version = created["imageVersion"]
        .as_str()
        .unwrap_or(FIRST_IMAGE_VERSION)
        .to_string();
    poll_until_version_settled(router, case, &arn, &version).await;

    for _ in 1..generations {
        let (status, updated) = update_image(router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: updating {name:?} expected 200, got {status} {updated}"
        );
        version = updated["imageVersion"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        poll_until_version_settled(router, case, &arn, &version).await;
    }
    arn
}

/// The versions the image `identifier` holds, as the listing reports them.
async fn listed_versions(router: &MultiServiceRouter, case: &str, identifier: &str) -> Vec<String> {
    let (status, body) = list_versions(router, identifier, "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: listing the versions of {identifier} got {status} {body}"
    );
    let mut versions: Vec<String> = body["items"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: items is a list, got {body}"))
        .iter()
        .map(|item| item["imageVersion"].as_str().unwrap_or_default().to_string())
        .collect();
    versions.sort();
    versions
}

/// One case of [`deleting_a_version_takes_that_version_and_leaves_the_others`]: an image
/// of `generations` settled versions, the one the delete addresses, the form the request
/// addresses the image by, and the versions that must remain.
struct DeleteCase {
    case: &'static str,
    name: &'static str,
    generations: usize,
    target: &'static str,
    by_arn: bool,
    remaining: &'static [&'static str],
}

/// One case of [`the_only_version_an_image_still_holds_cannot_be_deleted`]: an image of
/// `generations` settled versions, a version an earlier delete takes first (if any), the
/// version the refused delete addresses, and the form it addresses the image by.
struct LastVersionCase {
    case: &'static str,
    name: &'static str,
    generations: usize,
    already_deleted: Option<&'static str>,
    target: &'static str,
    by_arn: bool,
}

/// RULE: deleting a version of an image that still holds another one is accepted, reports
/// that version `DELETING`, and the removal the clock then owes takes it — the version's
/// own read and the version listing miss afterwards and a repeat of the delete is the
/// documented idempotent success, while every other version of the image is left exactly
/// as it was.
#[tokio::test]
async fn deleting_a_version_takes_that_version_and_leaves_the_others() {
    let router = emulator();

    // Rows differ in how many versions the image holds, WHICH of them the delete
    // addresses (the oldest, the newest — which is also the active one — or one in the
    // middle) and which form `MicrovmImageIdentifier` the request carries: none of that
    // changes the verdict, only whether another version remains does.
    let rows = vec![
        DeleteCase {
            case: "the oldest of two, by ARN",
            name: "del-two-oldest",
            generations: 2,
            target: "1.0",
            by_arn: true,
            remaining: &["2.0"],
        },
        DeleteCase {
            case: "the newest of two, which is the active one, by name",
            name: "del-two-newest",
            generations: 2,
            target: "2.0",
            by_arn: false,
            remaining: &["1.0"],
        },
        DeleteCase {
            case: "the middle of three, by ARN",
            name: "del-three-middle",
            generations: 3,
            target: "2.0",
            by_arn: true,
            remaining: &["1.0", "3.0"],
        },
    ];

    for DeleteCase {
        case,
        name,
        generations,
        target,
        by_arn,
        remaining,
    } in rows
    {
        let arn = image_with_versions(&router, case, name, generations).await;
        let identifier = match by_arn {
            true => arn.clone(),
            false => name.to_string(),
        };

        let (status, deleted) = delete_version(&router, &identifier, target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: deleting {target} expected 200, got {status} {deleted}"
        );
        assert_eq!(
            deleted["imageIdentifier"].as_str(),
            Some(identifier.as_str()),
            "case {case:?}: the answer names the identifier the request carried, got {:?}",
            deleted["imageIdentifier"]
        );
        assert_eq!(
            deleted["imageVersion"].as_str(),
            Some(target),
            "case {case:?}: the answer names the version the request carried, got {:?}",
            deleted["imageVersion"]
        );
        assert_eq!(
            deleted["state"].as_str(),
            Some(VERSION_STATE_DELETING),
            "case {case:?}: the deletion is asynchronous, got {:?}",
            deleted["state"]
        );

        // The removal the clock owes takes the record, so the read misses.
        poll_until_version_gone(&router, case, &arn, target).await;

        assert_eq!(
            listed_versions(&router, case, &arn).await,
            remaining.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            "case {case:?}: only the deleted version goes"
        );
        for version in remaining {
            let (status, read) = get_version(&router, &arn, version).await;
            assert_eq!(
                status, 200,
                "case {case:?}: version {version} is untouched, got {status} {read}"
            );
            assert_eq!(
                read["state"].as_str(),
                Some(VERSION_STATE_SUCCESSFUL),
                "case {case:?}: version {version} keeps the state its build settled on, got {:?}",
                read["state"]
            );
        }

        // "This operation is idempotent; deleting a version that has already been deleted
        // succeeds without error": the repeat reports the state the deletion settled in,
        // and resurrects nothing.
        let (status, again) = delete_version(&router, &identifier, target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a repeat of the delete succeeds, got {status} {again}"
        );
        assert_eq!(
            again["imageVersion"].as_str(),
            Some(target),
            "case {case:?}: the repeat names the version the request carried, got {:?}",
            again["imageVersion"]
        );
        assert_eq!(
            again["state"].as_str(),
            Some(VERSION_STATE_DELETED),
            "case {case:?}: the repeat reports the settled deletion, got {:?}",
            again["state"]
        );
        let (status, read) = get_version(&router, &arn, target).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the repeat brings the version back for nobody, got {status} {read}"
        );
        assert_eq!(
            listed_versions(&router, case, &arn).await,
            remaining.iter().map(|v| v.to_string()).collect::<Vec<_>>(),
            "case {case:?}: the repeat leaves the image's versions as they were"
        );

        // The IMAGE is untouched: a version delete is not an image delete.
        let (status, image) = get_image(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the image is still there, got {status} {image}"
        );
    }
}

/// RULE: an image always holds at least one version, so the delete of the ONE version it
/// still holds is refused with HTTP 400 `ValidationException` carrying the refusal's whole
/// fixed sentence — and changes nothing: that version stays readable, stays listed and
/// keeps the state its build settled on.
#[tokio::test]
async fn the_only_version_an_image_still_holds_cannot_be_deleted() {
    let router = emulator();

    // Rows differ in how the image came to hold exactly one version — created with its
    // first version, or left with one after an earlier delete took the other — and in the
    // identifier form the request carries.
    let rows = vec![
        LastVersionCase {
            case: "a freshly created image, by ARN",
            name: "last-created",
            generations: 1,
            already_deleted: None,
            target: "1.0",
            by_arn: true,
        },
        LastVersionCase {
            case: "a freshly created image, by name",
            name: "last-by-name",
            generations: 1,
            already_deleted: None,
            target: "1.0",
            by_arn: false,
        },
        LastVersionCase {
            case: "an image whose other version an earlier delete took",
            name: "last-after-delete",
            generations: 2,
            already_deleted: Some("1.0"),
            target: "2.0",
            by_arn: true,
        },
    ];

    for LastVersionCase {
        case,
        name,
        generations,
        already_deleted,
        target,
        by_arn,
    } in rows
    {
        let arn = image_with_versions(&router, case, name, generations).await;
        let identifier = match by_arn {
            true => arn.clone(),
            false => name.to_string(),
        };
        if let Some(earlier) = already_deleted {
            let (status, body) = delete_version(&router, &identifier, earlier).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the earlier delete expected 200, got {status} {body}"
            );
            poll_until_version_gone(&router, case, &arn, earlier).await;
        }

        let (status, refused) = delete_version(&router, &identifier, target).await;
        assert_eq!(
            status, 400,
            "case {case:?}: deleting the only version is refused, got {status} {refused}"
        );
        assert_eq!(
            refused["__type"].as_str(),
            Some("ValidationException"),
            "case {case:?}: expected ValidationException, got {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(LAST_IMAGE_VERSION_REFUSAL),
            "case {case:?}: expected the refusal's own sentence, got {:?}",
            refused["message"]
        );

        // The refusal has NO effect.
        let (status, read) = get_version(&router, &arn, target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refused version is untouched, got {status} {read}"
        );
        assert_eq!(
            read["state"].as_str(),
            Some(VERSION_STATE_SUCCESSFUL),
            "case {case:?}: the refused version keeps its settled state, got {:?}",
            read["state"]
        );
        assert_eq!(
            listed_versions(&router, case, &arn).await,
            vec![target.to_string()],
            "case {case:?}: the refused version is still the image's one version"
        );
    }
}

/// RULE: a version a delete has taken can launch nothing — the image stops serving it, so
/// an image whose ACTIVE version was deleted reports none and a launch that pins no
/// version is refused, while deleting a version the image was NOT serving leaves its
/// active version (and the launch that resolves it) exactly as it was. A launch pinned to
/// the deleted version is refused once the record is gone.
#[tokio::test]
async fn a_deleted_version_can_no_longer_be_launched() {
    let router = emulator();

    // Rows differ in whether the deleted version was the one the image was serving.
    let rows: Vec<(&str, &str, &str, Option<&str>)> = vec![
        (
            "the active version is deleted",
            "launch-active-deleted",
            "2.0",
            None,
        ),
        (
            "an inactive version is deleted",
            "launch-inactive-deleted",
            "1.0",
            Some("2.0"),
        ),
    ];

    for (case, name, target, active_after) in rows {
        let arn = image_with_versions(&router, case, name, 2).await;

        let (status, deleted) = delete_version(&router, &arn, target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: deleting {target} expected 200, got {status} {deleted}"
        );
        poll_until_version_gone(&router, case, &arn, target).await;

        let (status, image) = get_image(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the image got {status} {image}"
        );
        assert_eq!(
            image["latestActiveImageVersion"].as_str(),
            active_after,
            "case {case:?}: the image serves {active_after:?}, got {:?}",
            image["latestActiveImageVersion"]
        );

        // A launch pinned to the deleted version is refused: the address resolves to
        // nothing.
        let (status, pinned) = run_microvm(
            &router,
            json!({"imageIdentifier": arn.clone(), "imageVersion": target}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: launching the deleted version is refused, got {status} {pinned}"
        );
        assert_eq!(
            pinned["message"].as_str(),
            Some(image_version_not_found_message(&arn, target).as_str()),
            "case {case:?}: expected the version-not-found refusal, got {:?}",
            pinned["message"]
        );

        // A launch that pins NO version resolves the image's active one, so it is refused
        // exactly when the delete took the version the image was serving.
        let (status, unpinned) = run_microvm(&router, json!({"imageIdentifier": arn.clone()})).await;
        match active_after {
            None => {
                assert_eq!(
                    status, 404,
                    "case {case:?}: the image serves no version, so the launch is refused, got \
                     {status} {unpinned}"
                );
                assert_eq!(
                    unpinned["message"].as_str(),
                    Some(no_active_version_message(&arn).as_str()),
                    "case {case:?}: expected the active-version refusal, got {:?}",
                    unpinned["message"]
                );
            }
            Some(version) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the image still serves {version}, so the launch succeeds, got \
                     {status} {unpinned}"
                );
                assert_eq!(
                    unpinned["imageVersion"].as_str(),
                    Some(version),
                    "case {case:?}: the launch resolves the image's active version, got {:?}",
                    unpinned["imageVersion"]
                );
            }
        }
    }
}

/// RULE: the verdict on the ADDRESS comes before anything else — an identifier of the
/// AWS-owned namespace is denied before any lookup (HTTP 403), a value that is no
/// identifier of this API at all is a bad request (HTTP 400), and an identifier of this
/// account's own namespace naming no record here, or a version the resolved image never
/// held, is the modelled not-found (HTTP 404).
#[tokio::test]
async fn a_version_delete_addressed_at_nothing_is_refused_however_the_address_fails() {
    let router = emulator();

    // An image this account DOES hold, so the refused rows are refused for the address
    // they name rather than for an empty store.
    let held = image_with_versions(&router, "the held image", "held", 2).await;

    let rows: Vec<(&str, String, &str, u16, &str, String)> = vec![
        (
            "an ARN nobody minted",
            expected_image_arn("never-minted"),
            "1.0",
            404,
            "ResourceNotFoundException",
            version_address_image_not_found_message(&expected_image_arn("never-minted"), "1.0"),
        ),
        (
            "another account's ARN, outside this account's own namespace",
            format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:theirs"),
            "1.0",
            403,
            "AccessDeniedException",
            expected_denial_sentence(
                "DeleteMicrovmImageVersion",
                &format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:theirs"),
            ),
        ),
        (
            "another region's ARN for a held name",
            format!("arn:aws:lambda:eu-central-1:{ACCOUNT}:microvm-image:held"),
            "1.0",
            404,
            "ResourceNotFoundException",
            version_address_image_not_found_message(
                &format!("arn:aws:lambda:eu-central-1:{ACCOUNT}:microvm-image:held"),
                "1.0",
            ),
        ),
        (
            "an AWS-owned managed base image",
            managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
            "1.0",
            403,
            "AccessDeniedException",
            expected_denial_sentence(
                "DeleteMicrovmImageVersion",
                &managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
            ),
        ),
        (
            "a name nothing was created under",
            "never-created".to_string(),
            "1.0",
            400,
            "ValidationException",
            invalid_arn_format_message("never-created"),
        ),
        (
            "nothing ARN-shaped at all",
            "not.an.arn".to_string(),
            "1.0",
            400,
            "ValidationException",
            invalid_arn_format_message("not.an.arn"),
        ),
        (
            "a version the held image never held",
            held.clone(),
            "9.9",
            404,
            "ResourceNotFoundException",
            image_version_not_found_message(&held, "9.9"),
        ),
    ];

    for (case, identifier, version, expected_status, expected_type, expected_message) in rows {
        let (status, body) = delete_version(&router, &identifier, version).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: {identifier:?}/{version} expected {expected_status}, got {status} \
             {body}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some(expected_type),
            "case {case:?}: expected {expected_type}, got {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal built from the address the request carried, got \
             {:?}",
            body["message"]
        );
    }

    // The positive control: the held image's older version still deletes through the same
    // path, so the rows above are refused by the address and not by a dead path.
    let (status, body) = delete_version(&router, &held, "1.0").await;
    assert_eq!(
        status, 200,
        "a version the account holds still deletes, got {status} {body}"
    );
}

/// RULE: a delete that takes a version whose build is still in flight abandons that build,
/// so the lifecycle of the IMAGE that build was the work of ENDS — the image leaves the
/// progressive state nothing is writing any more and settles on a value a further update
/// can start from, rather than being stuck reporting work in flight for a version that is
/// gone.
#[tokio::test]
async fn deleting_a_version_whose_build_is_in_flight_settles_the_image() {
    let router = emulator();
    let case = "a delete landing on the version an update is building";

    // One settled version, so the image holds another version and the delete below is not
    // the refused last-version one.
    let arn = image_with_versions(&router, case, "abandoned-build", 1).await;

    // The update mints the next version and starts its build: the image is progressive
    // (`UPDATING`) from here until that build settles or the delete below abandons it.
    let (status, updated) = update_image(&router, &arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update expected 200, got {status} {updated}"
    );
    let building = updated["imageVersion"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    let (status, deleted) = delete_version(&router, &arn, &building).await;
    assert_eq!(
        status, 200,
        "case {case:?}: deleting {building} expected 200, got {status} {deleted}"
    );
    assert_eq!(
        deleted["state"].as_str(),
        Some(VERSION_STATE_DELETING),
        "case {case:?}: the deletion is asynchronous, got {:?}",
        deleted["state"]
    );
    poll_until_version_gone(&router, case, &arn, &building).await;

    // The image is no longer writing anything, so it reports no progressive state: the
    // settled value of the update whose build was abandoned, or — when the clock settled
    // that build before the delete landed — the value that build reached.
    let (status, image) = get_image(&router, &arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the image got {status} {image}"
    );
    let settled = image["state"].as_str().unwrap_or_default().to_string();
    assert!(
        settled == IMAGE_STATE_UPDATE_FAILED || settled == IMAGE_STATE_UPDATED,
        "case {case:?}: expected the update's own settled state, got {settled:?}"
    );

    // Which is what keeps the record usable: an image in a progressive state admits no
    // update, so an image left in one by the delete could never be built from again.
    let (status, again) = update_image(&router, &arn).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the settled image still accepts an update, got {status} {again}"
    );
    // And the version that survived the delete is still readable.
    let (status, read) = get_version(&router, &arn, FIRST_IMAGE_VERSION).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the version the delete did not address is untouched, got {status} \
         {read}"
    );
}
