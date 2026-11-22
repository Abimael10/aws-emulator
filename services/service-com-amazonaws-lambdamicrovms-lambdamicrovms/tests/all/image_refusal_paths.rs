// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the MicroVM-image plane answers when a call CANNOT be
//! served, driven over the same router the emulator boots.
//!
//! Three rules, each one level more general than the case that exposed it:
//!
//! * a not-found refusal of a build-addressed read carries AWS's own sentence for that
//!   error class, built from the ADDRESS the request carried — its `imageIdentifier`,
//!   `imageVersion` and `buildId` path labels;
//! * a not-found refusal of a VERSION-addressed call carries AWS's own sentence for that
//!   error class — the missing version named first, with the resolved image's own ARN and
//!   the `imageVersion` the request carried — whichever operation resolved the version;
//! * a paginated child enumeration of an image (`ListMicrovmImageVersions`,
//!   `ListMicrovmImageBuilds`) REPORTS what this region's store holds at the address the
//!   request named and refuses nothing past the authorization verdict: an address this
//!   account's own image namespace holds no children at gets a TERMINAL EMPTY PAGE, while
//!   an identifier of ANOTHER namespace is still denied (403) and a value that is no
//!   identifier of this API at all is still a bad request (400);
//! * `UpdateMicrovmImage` starts from the image's CURRENT state, so a record whose own
//!   lifecycle work has SETTLED admits an update, whether that work succeeded or failed —
//!   while an in-flight value and the values of a deletion are refused with the modelled
//!   `ValidationException` (HTTP 400) and leave the record exactly as it was.
//!
//! Each test states its rule and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The wire harness
//! (`emulator`, `call`, `image_body`, …) is the one `microvm_lifecycle` owns, the build
//! and version read paths are `image_build_read_path`'s and `image_version_listing`'s,
//! and the router with an object store beside this service — the one a build resolves its
//! code artifact against — is `image_build_outcome`'s.

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::ops::types::MICROVM_IMAGE_STATE_VALUES;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    image_not_updatable_message, invalid_arn_format_message,
    managed_base_image_arn, service_state, version_address_image_not_found_message,
    FIRST_IMAGE_VERSION, MANAGED_BASE_IMAGE_IDS, VERSION_STATUS_INACTIVE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::get_version;
use crate::image_build_read_path::{get_build, list_builds, page_build_ids};
use crate::image_version_deletion::{delete_version, image_with_versions, poll_until_version_gone};
use crate::image_version_listing::list_versions;
use crate::image_version_status_publication::publish_status;
use crate::microvm_image_and_instance_paths::{delete_image, get_image};
use crate::microvm_lifecycle::{
    as_uri_value, call, emulator, emulator_with_engine, expected_image_arn, image_body, poll_until,
    run_microvm, ACCOUNT, REGION,
};

/// A build role ARN: `RoleArn`, pattern
/// `^arn:aws[a-z\-]*:iam::[0-9]{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`.
const BUILD_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/microvm-refusals";

/// `PUT /2025-09-09/microvm-images/{imageIdentifier}` carrying the three members
/// `UpdateMicrovmImage` requires.
async fn update_image(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    call(
        router,
        "PUT",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier)),
        Some(json!({
            "baseImageArn": managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
            "buildRoleArn": BUILD_ROLE_ARN,
            "codeArtifact": {"uri": "s3://artifacts/app-next.zip"},
        })),
    )
    .await
}

/// Create the image `name`, asserting the create, and hand back the ARN it minted.
async fn create_image(router: &MultiServiceRouter, name: &str, extra: Value) -> String {
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
    body["imageArn"].as_str().unwrap_or_default().to_string()
}

/// AWS's sentence for a build-addressed not-found, spelled out here rather than read from
/// the crate so a change to the wording fails this suite: the fixed text `Build not found
/// for MicroVMImageID: `, `, VersionID: ` and `, BuildID: `, with the three variable parts
/// taken from the path labels the request carried.
fn expected_build_not_found(identifier: &str, image_version: &str, build_id: &str) -> String {
    format!(
        "Build not found for MicroVMImageID: {identifier}, VersionID: {image_version}, \
         BuildID: {build_id}"
    )
}

/// AWS's sentence for a version-addressed not-found, spelled out here rather than read
/// from the crate so a change to the wording fails this suite: the fixed text `MicroVM
/// image version not found: ` and `, version: `, with the resolved image's own ARN and the
/// `imageVersion` the request carried.
fn expected_version_not_found(image_arn: &str, image_version: &str) -> String {
    format!("MicroVM image version not found: {image_arn}, version: {image_version}")
}

/// A `buildId` in the form this service mints that no build of any version carries, so a
/// build-addressed row of the version-not-found table misses on its VERSION rather than
/// reaching the build lookup at all.
const UNHELD_BUILD_ID: &str = "build-ffffffffffffffff";

/// The state the store holds for the image `name`, as the image's own read reports it.
async fn image_state(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = get_image(router, name).await;
    assert_eq!(status, 200, "read {name:?}: expected 200, got {status} {body}");
    body["state"].as_str().unwrap_or_default().to_string()
}

/// Write `state` onto the stored image `name`, so one row can address an image in any
/// `MicrovmImageState` value — including the ones a deletion settles in, which this
/// service's own clock reaches by removing the record.
fn seed_image_state(engine: &engine::InMemoryEngine, name: &str, state: &str) {
    let store = service_state(engine).expect("the service's own store");
    let mut store = store.lock();
    let image = store
        .image_named_mut(REGION, name)
        .unwrap_or_else(|| panic!("the created image {name:?} is held"));
    image.state = state.to_string();
}

/// RULE: a build-addressed read that resolves its image and version and finds no build
/// under the `buildId` the request named answers the modelled `ResourceNotFoundException`
/// (HTTP 404) whose message is AWS's own sentence for the class, built from the address
/// the request carried.
#[tokio::test]
async fn a_build_the_version_does_not_hold_is_refused_in_awss_words() {
    let router = emulator();
    let image_arn = create_image(&router, "worded", json!({})).await;
    let (status, listed) = list_builds(&router, &image_arn, FIRST_IMAGE_VERSION, "").await;
    assert_eq!(status, 200, "listing the builds got {status} {listed}");
    let held_build = page_build_ids(&listed, "worded")
        .first()
        .cloned()
        .unwrap_or_default();

    // Rows differ along what the sentence's variable parts generalise over: WHICH form of
    // the identifier the request addressed the image by, and the FORM of the build id
    // that is missing — the id form this service mints, a caller's own token, one at the
    // published maximum length of `NonBlankString` (`@length(1..2048)`), and one carrying
    // a separator.
    let identifiers = [("the image name", "worded".to_string()), ("the image ARN", image_arn.clone())];
    let build_ids = [
        "build-ffffffffffffffff".to_string(),
        "no-such-build".to_string(),
        "b".repeat(2048),
        "build.2026-01_candidate".to_string(),
    ];

    for (form, identifier) in &identifiers {
        for build_id in &build_ids {
            let case = format!("{form} {identifier:?} build {build_id:?}");
            let (status, body) =
                get_build(&router, identifier, FIRST_IMAGE_VERSION, build_id).await;
            assert_eq!(
                status, 404,
                "case {case}: expected 404, got {status} {body}"
            );
            assert_eq!(
                body["__type"].as_str(),
                Some("ResourceNotFoundException"),
                "case {case}: expected ResourceNotFoundException, got {body}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(
                    expected_build_not_found(identifier, FIRST_IMAGE_VERSION, build_id).as_str()
                ),
                "case {case}: expected AWS's sentence built from the request's own address, \
                 got {:?}",
                body["message"]
            );
        }

        // The positive control: the build the version DOES hold is served through the
        // same path, so the rows above miss on the build id and not on a dead read.
        let (status, body) = get_build(&router, identifier, FIRST_IMAGE_VERSION, &held_build).await;
        assert_eq!(
            status, 200,
            "case {form}: the held build is readable, got {status} {body}"
        );
    }
}

/// RULE: a call addressed at an `imageVersion` the image it resolved does not hold is
/// refused with the modelled `ResourceNotFoundException` (HTTP 404) whose message is AWS's
/// own sentence for that class — the missing VERSION named first, then the resolved
/// image's own ARN and the `imageVersion` the request carried — so the one discriminator
/// the service offers tells a client which half of the address failed. The sentence is the
/// same whichever operation resolved the version and whichever form of
/// `MicrovmImageIdentifier` the request addressed the image by; a call whose IMAGE half
/// resolves to nothing has no ARN to name and reports the whole address it was given
/// instead.
#[tokio::test]
async fn a_version_the_image_does_not_hold_is_refused_in_awss_words() {
    let router = emulator();
    // Two settled versions, so the delete below is accepted (an image always holds at
    // least one version) and the version it takes is one this image HELD and no longer
    // does — the address a client racing a delete is left holding.
    let image_arn = image_with_versions(&router, "the versioned image", "versioned", 2).await;
    let (status, deleted) = delete_version(&router, &image_arn, FIRST_IMAGE_VERSION).await;
    assert_eq!(
        status, 200,
        "deleting {FIRST_IMAGE_VERSION} expected 200, got {status} {deleted}"
    );
    poll_until_version_gone(
        &router,
        "the deleted version",
        &image_arn,
        FIRST_IMAGE_VERSION,
    )
    .await;

    // Rows differ along what the sentence's version part generalises over: a version the
    // image held until a delete took it, one in this service's own `major.minor` form it
    // never held, one in no form this service mints at all, and one at the published
    // maximum length of `imageVersion` (`NonBlankString`, `@length(1..2048)`).
    let versions: [(&str, String); 4] = [
        (
            "a version a delete has taken",
            FIRST_IMAGE_VERSION.to_string(),
        ),
        ("a version this image never held", "9.9".to_string()),
        (
            "a version in no form this service mints",
            "latest".to_string(),
        ),
        (
            "a version at the published maximum length",
            "9".repeat(2048),
        ),
    ];
    let identifiers = [
        ("the image name", "versioned".to_string()),
        ("the image ARN", image_arn.clone()),
    ];

    for (version_form, image_version) in &versions {
        for (id_form, identifier) in &identifiers {
            // Every operation that resolves an `imageVersion` through the one address.
            // `DeleteMicrovmImageVersion` is the documented idempotent one, so a version
            // an earlier delete took is reported in its settled deletion state there and
            // only a version the image NEVER held is refused through it.
            let mut answers = vec![
                (
                    "GetMicrovmImageVersion",
                    get_version(&router, identifier, image_version).await,
                ),
                (
                    "UpdateMicrovmImageVersion",
                    publish_status(&router, identifier, image_version, VERSION_STATUS_INACTIVE)
                        .await,
                ),
                (
                    "GetMicrovmImageBuild",
                    get_build(&router, identifier, image_version, UNHELD_BUILD_ID).await,
                ),
                (
                    "RunMicrovm",
                    run_microvm(
                        &router,
                        json!({"imageIdentifier": identifier, "imageVersion": image_version}),
                    )
                    .await,
                ),
            ];
            if image_version != FIRST_IMAGE_VERSION {
                answers.push((
                    "DeleteMicrovmImageVersion",
                    delete_version(&router, identifier, image_version).await,
                ));
            }

            let expected = expected_version_not_found(&image_arn, image_version);
            for (operation, (status, body)) in answers {
                let case = format!("{version_form} addressed by {id_form} on {operation}");
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected 404, got {status} {body}"
                );
                assert_eq!(
                    body["__type"].as_str(),
                    Some("ResourceNotFoundException"),
                    "case {case:?}: expected ResourceNotFoundException, got {body}"
                );
                assert_eq!(
                    body["message"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected {expected:?}, got {:?}",
                    body["message"]
                );
            }
        }
    }

    // The positive control: the version the image DOES hold is served through the same
    // paths, so the rows above miss on the version they named and not on a dead read. The
    // surviving version is read out of the listing rather than written down here, so the
    // control does not depend on the form this service mints version labels in.
    let (status, surviving) = list_versions(&router, &image_arn, "").await;
    assert_eq!(
        status, 200,
        "listing the surviving versions got {status} {surviving}"
    );
    let held = surviving["items"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["imageVersion"].as_str())
        .unwrap_or_else(|| panic!("the image still holds a version, got {surviving}"))
        .to_string();
    assert_ne!(
        held, FIRST_IMAGE_VERSION,
        "the surviving version is not the one the delete took, got {surviving}"
    );
    for (id_form, identifier) in &identifiers {
        let (status, body) = get_version(&router, identifier, &held).await;
        assert_eq!(
            status, 200,
            "case {id_form:?}: the held version is readable, got {status} {body}"
        );
        assert_eq!(
            body["imageVersion"].as_str(),
            Some(held.as_str()),
            "case {id_form:?}: the read reports the version it resolved, got {body}"
        );
    }

    // The OTHER half of the address: with no image resolved there is no ARN to name a
    // version against, so a launch pinned to a version of an image this account holds no
    // record of reports the whole address it was given — the same sentence the
    // version-addressed reads answer for an unresolvable image
    // (`version_address_image_not_found_message`), not the version sentence above.
    let absent = expected_image_arn("never-created");
    for (version_form, image_version) in &versions {
        let (status, body) = run_microvm(
            &router,
            json!({"imageIdentifier": absent.clone(), "imageVersion": image_version}),
        )
        .await;
        let case = format!("{version_form} of an image this account does not hold");
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(version_address_image_not_found_message(&absent, image_version).as_str()),
            "case {case:?}: a launch with no image resolved names the address it was given, \
             got {:?}",
            body["message"]
        );
    }
}

/// RULE: past the authorization verdict a paginated child enumeration of an image reports
/// what this region's store holds at the address the request named — so an address of
/// this account's own image namespace holding no children, however it came to hold none,
/// is a terminal empty page rather than a refusal.
#[tokio::test]
async fn an_address_holding_no_children_gets_a_terminal_empty_page() {
    let (router, engine) = emulator_with_engine();
    let held_arn = create_image(&router, "held", json!({})).await;

    // An image this account holds whose deletion the clock has already completed: the
    // record is gone, so the identifier names nothing here.
    let removed_arn = create_image(&router, "removed", json!({})).await;
    let (status, deleted) = delete_image(&router, "removed").await;
    assert_eq!(status, 200, "the delete expected 200, got {status} {deleted}");
    // The removal is clock-owed: wait for the CONDITION — the record reported gone through
    // its own ARN — rather than for a span of time, so a loaded host only makes the test
    // look again.
    poll_until(
        "a record the clock has removed",
        "the removed image's record is gone",
        || async { get_image(&router, &removed_arn).await.0 == 404 },
    )
    .await;

    // Rows differ in HOW the address of this account's own namespace comes to hold no
    // children: a name nothing was created under, another region's ARN for a held name, a
    // record the clock has removed, and a held image addressed at a version it does not
    // hold (which only the build listing can name).
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "an ARN for a name never created",
            expected_image_arn("never-created"),
            FIRST_IMAGE_VERSION,
        ),
        (
            "another region's ARN for a held name",
            format!("arn:aws:lambda:eu-central-1:{ACCOUNT}:microvm-image:held"),
            FIRST_IMAGE_VERSION,
        ),
        (
            "a record the clock has removed",
            expected_image_arn("removed"),
            FIRST_IMAGE_VERSION,
        ),
        (
            "a held image's name, at a version it does not hold",
            "held".to_string(),
            "9.9",
        ),
        (
            "a held image's ARN, at a version in no form this service mints",
            held_arn.clone(),
            "latest",
        ),
    ];

    for (case, identifier, image_version) in &rows {
        let (status, body) = list_builds(&router, identifier, image_version, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing the builds of {identifier:?} at {image_version:?} expected \
             200, got {status} {body}"
        );
        assert_eq!(
            body["items"],
            json!([]),
            "case {case:?}: items is projected as an empty list, got {body}"
        );
        assert!(
            body["nextToken"].is_null(),
            "case {case:?}: an exhausted listing issues no nextToken, got {:?}",
            body["nextToken"]
        );

        // The version listing has no version label, so it is exercised with the rows
        // whose IMAGE holds nothing — the held image itself does hold versions.
        if identifier == "held" || identifier == &held_arn {
            continue;
        }
        let (status, body) = list_versions(&router, identifier, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing the versions of {identifier:?} expected 200, got {status} \
             {body}"
        );
        assert_eq!(
            body["items"],
            json!([]),
            "case {case:?}: items is projected as an empty list, got {body}"
        );
    }

    // An image this account holds whose version records are all gone: the identifier
    // resolves and the collection is empty, which is the same empty page.
    {
        let store = service_state(&*engine).expect("the service's own store");
        let mut store = store.lock();
        store
            .image_named_mut(REGION, "held")
            .expect("the created image is held")
            .versions
            .clear();
    }
    let (status, body) = list_versions(&router, &held_arn, "").await;
    assert_eq!(
        status, 200,
        "an emptied image's versions expected 200, got {status} {body}"
    );
    assert_eq!(
        body["items"],
        json!([]),
        "an emptied image holds no versions to enumerate, got {body}"
    );

    // The refusals the enumerations KEEP: an identifier of another namespace is denied
    // before anything is looked up, and a value that is no identifier of this API at all
    // is a request that can never succeed. A denied row carries no expected message of
    // its own because a denial's sentence is worded per OPERATION — it names the IAM
    // action being refused — so it is this service's own for that operation and
    // identifier.
    let managed = managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]);
    let other_account = format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:someone-elses");
    let refusals: Vec<(&str, String, u16, &str, Option<String>)> = vec![
        (
            "the AWS-owned namespace",
            managed.clone(),
            403,
            "AccessDeniedException",
            None,
        ),
        (
            "another account's namespace",
            other_account.clone(),
            403,
            "AccessDeniedException",
            None,
        ),
        (
            "a bare name no image of this account carries",
            "never-created".to_string(),
            400,
            "ValidationException",
            Some(invalid_arn_format_message("never-created")),
        ),
        (
            "no identifier of this API at all",
            "not-an-identifier".to_string(),
            400,
            "ValidationException",
            Some(invalid_arn_format_message("not-an-identifier")),
        ),
    ];

    for (case, identifier, expected_status, expected_type, expected_message) in refusals {
        for (operation, (status, body)) in [
            (
                "ListMicrovmImageVersions",
                list_versions(&router, &identifier, "").await,
            ),
            (
                "ListMicrovmImageBuilds",
                list_builds(&router, &identifier, FIRST_IMAGE_VERSION, "").await,
            ),
        ] {
            assert_eq!(
                status, expected_status,
                "case {case:?} on {operation}: expected {expected_status}, got {status} {body}"
            );
            assert_eq!(
                body["__type"].as_str(),
                Some(expected_type),
                "case {case:?} on {operation}: expected {expected_type}, got {body}"
            );
            let expected_message = expected_message.clone().unwrap_or_else(|| {
                crate::image_denial_message::expected_denial_sentence(operation, &identifier)
            });
            assert_eq!(
                body["message"].as_str(),
                Some(expected_message.as_str()),
                "case {case:?} on {operation}: expected {expected_message:?}, got {:?}",
                body["message"]
            );
        }
    }
}

/// RULE: `UpdateMicrovmImage` starts the next build from the image's CURRENT record, so
/// every state in which that record's own create-or-update lifecycle has SETTLED admits one
/// — `CREATED`, `CREATE_FAILED`, `UPDATED` and `UPDATE_FAILED` of the nine
/// `MicrovmImageState` values (`smithy_query(query="shape",shape="MicrovmImageState")`).
/// Every other value — an in-flight lifecycle, and the one-way values of a deletion — is
/// refused with HTTP 400 `ValidationException` naming the image's own ARN, and the refused
/// call leaves the record in the state it found it in.
#[tokio::test]
async fn an_update_is_refused_for_every_state_that_admits_none() {
    let (router, engine) = emulator_with_engine();

    // Rows span the WHOLE published enum, so the table is the state dimension itself
    // rather than a sample of it; the states an update may start from are the settled
    // values of the create and update lifecycles, whichever branch they settled on.
    let admits_update = ["CREATED", "CREATE_FAILED", "UPDATED", "UPDATE_FAILED"];
    assert_eq!(
        MICROVM_IMAGE_STATE_VALUES.len(),
        9,
        "the table must span every published MicrovmImageState value, got {:?}",
        MICROVM_IMAGE_STATE_VALUES
    );

    for (row, state) in MICROVM_IMAGE_STATE_VALUES.iter().enumerate() {
        // Each row gets its own image, so an accepted update cannot change what the next
        // row is answered.
        let name = format!("state-{row}");
        let image_arn = create_image(&router, &name, json!({})).await;
        // The create's build settles on the next call that reads the store, so the row
        // looks at the image until that has happened: what it then writes is the state of
        // a settled record rather than one the next call's clock is about to overwrite.
        poll_until(state, "the create's build has settled on CREATED", || async {
            image_state(&router, &name).await == "CREATED"
        })
        .await;
        seed_image_state(&engine, &name, state);

        let (status, body) = update_image(&router, &name).await;
        if admits_update.contains(state) {
            assert_eq!(
                status, 200,
                "case {state:?}: a settled, built image admits an update, got {status} {body}"
            );
            assert_eq!(
                image_state(&router, &name).await,
                "UPDATING",
                "case {state:?}: the accepted update puts the image's build in flight"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {state:?}: expected 400, got {status} {body}"
        );
        assert_eq!(
            body["__type"].as_str(),
            Some("ValidationException"),
            "case {state:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(image_not_updatable_message(&image_arn).as_str()),
            "case {state:?}: the refusal names the image's own ARN, got {:?}",
            body["message"]
        );
        assert_eq!(
            image_state(&router, &name).await,
            *state,
            "case {state:?}: a refused update leaves the record in the state it found"
        );
    }
}

// What an image whose own build has SETTLED — on either branch — is answered by a further
// `UpdateMicrovmImage` is the rule `image_configuration_update` owns, reached there through
// the build's own artifact resolution rather than through a seeded state.
