// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `GetMicrovmImageVersion` reports for a version whose code
//! artifact cannot be fetched, driven over the same router the emulator boots with Amazon
//! S3 mounted beside this service on one engine (a build's outcome is the object store's
//! answer, so an unfetchable artifact can only be driven through the owner of objects).
//!
//! RULE: a MicroVM image version's asynchronous build is TERMINAL, and a build whose code
//! artifact the object store ANSWERS that it does not serve settles the version it was of
//! on the failure branch — `state` FAILED, `status` INACTIVE, `stateReason` the reason for
//! that answer, and `versionStateTimeBucket` the state and the hour it settled in — while
//! a build whose artifact IS served settles SUCCESSFUL/ACTIVE with no reason. That holds
//! whichever call minted the version and whichever half of the object address is not
//! served, so a client polling this read leaves the in-flight states in finite time and is
//! never told that a version with no usable code is launchable; and because the read only
//! REPORTS, every later read of the same version answers the same settlement.
//!
//! Each test states that rule and exercises it over a SET of inputs: the rows differ in
//! which half of the object address the store does not serve and in the form of the key,
//! and the two tests differ in which call minted the version — the create, or a
//! configuration update of an image that already serves a version. The recorded case, a
//! `codeArtifact$uri` naming a bucket that does not exist, is one row of the class.
//!
//! The wire harness (`call`, `image_body`, `poll_until`, …) is the one `microvm_lifecycle`
//! owns, and the Amazon S3 router and the object staging are `image_build_outcome`'s. What
//! a build's settlement writes on the IMAGE record is that suite's subject; this one is the
//! single-version read a client polls.

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::code_artifact::{
    BUCKET_ABSENT_REASON, OBJECT_ABSENT_REASON,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    state_time_bucket, FIRST_IMAGE_VERSION, VERSION_STATE_FAILED, VERSION_STATE_SUCCESSFUL,
    VERSION_STATUS_ACTIVE, VERSION_STATUS_INACTIVE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::{create_image, emulator, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, image_body, poll_until};

/// A member's text, or the empty string when the member is absent.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// One row: what a failing assertion calls it, the image name, the `codeArtifact$uri`, the
/// object to stage before the build (if any) and the `stateReason` the build must settle
/// with — `None` for a build that must settle successfully.
type ArtifactRow = (
    &'static str,
    &'static str,
    &'static str,
    Option<(&'static str, &'static str)>,
    Option<&'static str>,
);

/// The rows both tests span: whether the object store serves the artifact, WHICH half of
/// the object address it does not serve, and the form of the key.
fn artifact_rows() -> Vec<ArtifactRow> {
    vec![
        (
            "the artifact is served",
            "read-served",
            "s3://mvm-read-artifacts/app.zip",
            Some(("mvm-read-artifacts", "app.zip")),
            None,
        ),
        (
            "the artifact is served under a nested prefix",
            "read-served-nested",
            "s3://mvm-read-nested/team/build/app.zip",
            Some(("mvm-read-nested", "team/build/app.zip")),
            None,
        ),
        (
            "the bucket does not exist",
            "read-absent-bucket",
            "s3://mvm-read-no-such-bucket/app.zip",
            None,
            Some(BUCKET_ABSENT_REASON),
        ),
        (
            "the bucket exists and the object does not",
            "read-absent-object",
            "s3://mvm-read-empty/app.zip",
            Some(("mvm-read-empty", "another.zip")),
            Some(OBJECT_ABSENT_REASON),
        ),
        (
            "the object is absent under a nested prefix",
            "read-absent-nested-object",
            "s3://mvm-read-partial/team/build/app.zip",
            Some(("mvm-read-partial", "team/build/other.zip")),
            Some(OBJECT_ABSENT_REASON),
        ),
    ]
}

/// The row of `ListMicrovmImageVersions` reporting `image_version` of `identifier` — the
/// other projection of the same stored record, which the read cannot disagree with.
async fn listed_version(
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
        "case {case:?}: the version listing expected 200, got {status} {body}"
    );
    body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|row| row["imageVersion"].as_str() == Some(image_version))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing carries version {image_version}, got {body}")
        })
}

/// Whether `state` is one the asynchronous build SETTLES in, as against one it is still in
/// flight in: a terminal state is what a poll of the version read is waiting for.
fn is_settled(state: &str) -> bool {
    state == VERSION_STATE_SUCCESSFUL || state == VERSION_STATE_FAILED
}

/// Poll `GetMicrovmImageVersion` until the version's build has settled, and assert the
/// whole settlement the row expects.
///
/// The poll waits for the CONDITION the clock owes — a terminal `state` — through the poll
/// loop this crate's control-plane suites share, so a loaded host only looks again and no
/// case measures how long the build took. The answer the poll settled on is handed back.
async fn assert_settled_version(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    expected_reason: Option<&str>,
    case: &str,
) -> Value {
    poll_until(
        case,
        &format!("version {image_version} of {identifier} reports a settled state"),
        || async {
            let (status, version) = get_version(router, identifier, image_version).await;
            status == 200 && is_settled(&text(&version, "state"))
        },
    )
    .await;

    let (status, version) = get_version(router, identifier, image_version).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for version {image_version}, got {status} {version}"
    );

    // A build that could not fetch its code artifact produced no image, so the version it
    // was of is FAILED and blocked from launching new MicroVMs; one whose artifact was
    // served is the built, launchable version.
    let (expected_state, expected_status) = match expected_reason {
        None => (VERSION_STATE_SUCCESSFUL, VERSION_STATUS_ACTIVE),
        Some(_) => (VERSION_STATE_FAILED, VERSION_STATUS_INACTIVE),
    };
    let actual_state = text(&version, "state");
    assert_eq!(
        actual_state, expected_state,
        "case {case:?}: expected state {expected_state}, got {actual_state:?} on {version}"
    );
    let actual_status = text(&version, "status");
    assert_eq!(
        actual_status, expected_status,
        "case {case:?}: expected status {expected_status}, got {actual_status:?} on {version}"
    );

    // The reason is the whole explanation of the object store's answer, and a build that
    // produced an image has nothing to explain.
    let actual_reason = version["stateReason"].as_str().map(str::to_string);
    assert_eq!(
        actual_reason.as_deref(),
        expected_reason,
        "case {case:?}: expected stateReason {expected_reason:?}, got {actual_reason:?}"
    );

    // The bucket a settled version carries is the state it settled in and the hour of the
    // instant it settled at — the `updatedAt` the same answer reports — so it is checked
    // against the answer rather than merely for presence.
    let settled_at = version["updatedAt"].as_f64().unwrap_or_else(|| {
        panic!("case {case:?}: a settled version reports updatedAt, got {version}")
    });
    let expected_bucket = state_time_bucket(aws_smithy_types::DateTime::from_secs_f64(settled_at))
        .map(|bucket| format!("{actual_state}#{bucket}"));
    let actual_bucket = version["versionStateTimeBucket"]
        .as_str()
        .map(str::to_string);
    assert_eq!(
        actual_bucket, expected_bucket,
        "case {case:?}: expected versionStateTimeBucket {expected_bucket:?}, \
         got {actual_bucket:?}"
    );

    // The read REPORTS the settlement: it is terminal, so looking again answers exactly
    // the same record and no later caller sees anything the poll moved.
    let (status, again) = get_version(router, identifier, image_version).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the re-read, got {status} {again}"
    );
    assert_eq!(
        again, version,
        "case {case:?}: a settled version's read changes nothing a later caller sees, \
         got {again} then {version}"
    );

    // The listing is the same projection of the same record, so its row cannot disagree
    // about the settlement.
    let row = listed_version(router, identifier, image_version, case).await;
    for member in [
        "state",
        "status",
        "stateReason",
        "versionStateTimeBucket",
        "codeArtifact",
    ] {
        assert_eq!(
            row[member], version[member],
            "case {case:?}: the listing row's {member} must match the version read, \
             got {} and {}",
            row[member], version[member]
        );
    }

    version
}

/// RULE: the version a CREATE minted settles on the branch the object store answered for
/// its code artifact — FAILED/INACTIVE with the reason for that answer when the store does
/// not serve it, SUCCESSFUL/ACTIVE with no reason when it does — and the single-version
/// read a client polls reports that settlement, with the bucket of the hour it settled in.
#[tokio::test]
async fn the_version_a_create_mints_settles_on_its_artifacts_branch() {
    let router = emulator();

    for (case, name, uri, staged, reason) in artifact_rows() {
        if let Some((bucket, key)) = staged {
            stage_object(&router, bucket, key).await;
        }
        let image_arn = create_image(&router, name, uri).await;

        let version = assert_settled_version(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            reason,
            case,
        )
        .await;

        // The version the read answers for is the one the create minted, of the image the
        // create minted, built from the artifact the request named.
        assert_eq!(
            text(&version, "imageVersion"),
            FIRST_IMAGE_VERSION,
            "case {case:?}: expected the created version {FIRST_IMAGE_VERSION}, got {version}"
        );
        assert_eq!(
            text(&version, "imageArn"),
            image_arn,
            "case {case:?}: expected the image {image_arn}, got {version}"
        );
        assert_eq!(
            version["codeArtifact"]["uri"].as_str(),
            Some(uri),
            "case {case:?}: expected the artifact {uri:?} the build resolved, got {version}"
        );
    }
}

/// RULE: the same resolution settles the version a configuration UPDATE mints, so an
/// update whose artifact the store does not serve leaves that version FAILED/INACTIVE with
/// the reason for the store's answer — while the version the image was already serving
/// keeps its own settlement, each version's read reporting its own.
#[tokio::test]
async fn the_version_an_update_mints_settles_on_its_own_artifacts_branch() {
    let router = emulator();

    // Every image's FIRST version is built from one served artifact, so the image has a
    // settled version to carry the configuration forward from and the rows differ only in
    // the UPDATE's own artifact.
    let served_bucket = "mvm-read-update-artifacts";
    let served_key = "app.zip";
    stage_object(&router, served_bucket, served_key).await;
    let served_uri = format!("s3://{served_bucket}/{served_key}");

    for (case, name, uri, staged, reason) in artifact_rows() {
        if let Some((bucket, key)) = staged {
            stage_object(&router, bucket, key).await;
        }
        let image_arn = create_image(&router, name, &served_uri).await;
        assert_settled_version(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            None,
            &format!("{case}, the version the create built"),
        )
        .await;

        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/2025-09-09/microvm-images/{}", as_uri_value(&image_arn)),
            Some(image_body(name, json!({"codeArtifact": {"uri": uri}}))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the update, got {status} {updated}"
        );
        let minted = text(&updated, "imageVersion");
        assert_ne!(
            minted, FIRST_IMAGE_VERSION,
            "case {case:?}: an update mints a further version, got {updated}"
        );

        let version = assert_settled_version(
            &router,
            &image_arn,
            &minted,
            reason,
            &format!("{case}, the version the update built"),
        )
        .await;
        assert_eq!(
            version["codeArtifact"]["uri"].as_str(),
            Some(uri),
            "case {case:?}: expected the update's artifact {uri:?}, got {version}"
        );

        // Each version is settled by its OWN build: the version the create built stays
        // SUCCESSFUL with nothing to explain, whatever the update's artifact resolved to.
        // Its availability is the one thing the update's own settlement moves — exactly one
        // version of an image is ACTIVE, so it keeps serving the image only while the
        // update's build produced no image to replace it with.
        let (status, first) = get_version(&router, &image_arn, FIRST_IMAGE_VERSION).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for the created version, got {status} {first}"
        );
        assert_eq!(
            text(&first, "state"),
            VERSION_STATE_SUCCESSFUL,
            "case {case:?}: the created version keeps its own settlement, got {first}"
        );
        assert!(
            first["stateReason"].is_null(),
            "case {case:?}: a version whose build produced an image explains nothing, \
             got {:?}",
            first["stateReason"]
        );
        let expected_status = match reason {
            None => VERSION_STATUS_INACTIVE,
            Some(_) => VERSION_STATUS_ACTIVE,
        };
        assert_eq!(
            text(&first, "status"),
            expected_status,
            "case {case:?}: expected the created version to be {expected_status}, got {first}"
        );
    }
}
