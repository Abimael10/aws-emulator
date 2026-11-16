// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT `UpdateMicrovmImageVersion` writes, driven over the same
//! router the emulator boots with Amazon S3 mounted beside this service on one engine (a
//! build's outcome is the object store's answer, so a FAILED version can only be reached
//! through the owner of objects).
//!
//! RULE: an operation that "updates the status of a specific MicroVM image version" writes
//! exactly the availability it was asked for and the stamp that records that write — the
//! addressed version's `status` and `updatedAt`, plus the step-down of the version that
//! held the image's active pointer — so every other member of every version record of
//! that image, the settlement members among them (`state`, `stateReason` and the
//! state-settlement projection `versionStateTimeBucket`, which records WHEN the version's
//! state settled and not when the record was last written), is left exactly as it was,
//! a version the call did not address is not written at all, the answer IS the stored
//! record, and a later read reports that record unchanged however often it is read.
//!
//! Each test states that rule and exercises it over a SET of inputs: the rows differ in
//! the state the version's build settled on, in which of an image's versions is
//! addressed, in whether the status asked for differs from the one the record already
//! carries, and in whether another version has to step down. The recorded toggle of one
//! version between ACTIVE and INACTIVE is two rows of that class. The wire harness
//! (`call`, `poll_until`, …) is the one `microvm_lifecycle` owns, and the Amazon S3
//! harness is `image_build_outcome`'s.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    next_image_version, FIRST_IMAGE_VERSION, VERSION_STATE_FAILED, VERSION_STATE_SUCCESSFUL,
    VERSION_STATUS_ACTIVE, VERSION_STATUS_INACTIVE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::{create_image, emulator, get_image, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, image_body, poll_until};

/// The bucket this suite stages a fetchable artifact in, so a build of it succeeds.
const SERVED_BUCKET: &str = "mvm-write-scope-artifacts";

/// The object key of that artifact.
const SERVED_KEY: &str = "app.zip";

/// A `codeArtifact$uri` whose bucket the object store does not serve, so a build of it
/// settles on the failure branch.
const UNFETCHABLE_URI: &str = "s3://mvm-write-scope-no-such-bucket/app.zip";

/// The two members a status publication is allowed to write on a version record.
const WRITTEN_MEMBERS: &[&str] = &["status", "updatedAt"];

/// One row of the table below: what a failing assertion calls it, the image addressed,
/// every version that image holds, the version the publication addresses, the status it
/// asks for, and the state that version's build settled on.
type WriteScopeRow<'a> = (&'a str, &'a str, Vec<&'a str>, &'a str, &'a str, &'a str);

/// `PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}`.
async fn publish_status(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    status: &str,
) -> (u16, Value) {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{}",
        as_uri_value(identifier),
        as_uri_value(image_version)
    );
    call(router, "PATCH", &uri, Some(json!({"status": status}))).await
}

/// Look again until the version's state has SETTLED — the state-settlement projection is
/// what reports that it has — so no row races the build that is still in flight.
async fn settled(router: &MultiServiceRouter, case: &str, identifier: &str, image_version: &str) {
    poll_until(
        case,
        &format!("the version {image_version} of {identifier} has settled"),
        || async {
            let (status, read) = get_version(router, identifier, image_version).await;
            status == 200 && !read["versionStateTimeBucket"].is_null()
        },
    )
    .await;
}

/// The record of every version of `identifier`, keyed by version, read one by one through
/// the version's own read.
async fn records_of(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    versions: &[&str],
) -> BTreeMap<String, Value> {
    let mut records = BTreeMap::new();
    for image_version in versions {
        let (status, read) = get_version(router, identifier, image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the version {image_version} reads back, got {status} {read}"
        );
        records.insert((*image_version).to_string(), read);
    }
    records
}

/// Every member either answer carries, so a member one of them is missing is compared too.
fn members(left: &Value, right: &Value) -> Vec<String> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for value in [left, right] {
        if let Some(map) = value.as_object() {
            keys.extend(map.keys().cloned());
        }
    }
    keys.into_iter().collect()
}

/// The epoch seconds of a wire timestamp member, or `None` when the member is absent.
fn instant(record: &Value, member: &str) -> Option<f64> {
    record[member].as_f64()
}

/// One image whose first build SUCCEEDED, with the ARN the create minted.
async fn image_with_successful_build(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
    stage_object(router, SERVED_BUCKET, SERVED_KEY).await;
    let arn = create_image(router, name, &format!("s3://{SERVED_BUCKET}/{SERVED_KEY}")).await;
    settled(router, case, &arn, FIRST_IMAGE_VERSION).await;
    arn
}

/// One image whose first build FAILED, with the ARN the create minted.
async fn image_with_failed_build(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let arn = create_image(router, name, UNFETCHABLE_URI).await;
    settled(router, case, &arn, FIRST_IMAGE_VERSION).await;
    arn
}

/// Mint a SECOND version of `identifier` through `UpdateMicrovmImage` and return it, so a
/// row can span which of two versions the publication addresses.
async fn second_version(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let (status, updated) = call(
        router,
        "PUT",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier)),
        Some(image_body(
            "ignored-by-the-update",
            json!({"codeArtifact": {"uri": format!("s3://{SERVED_BUCKET}/{SERVED_KEY}")}}),
        )),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a second version must be minted, got {status} {updated}"
    );
    let second = next_image_version([FIRST_IMAGE_VERSION]);
    settled(router, case, identifier, &second).await;
    second
}

/// RULE: a status publication writes the addressed version's `status` and `updatedAt` and
/// nothing else about that record — the settlement members `state`, `stateReason` and
/// `versionStateTimeBucket` among the rest, because the bucket records when the version's
/// STATE settled rather than when its record was last written — and the answer it returns
/// is the stored record, which a later read reports unchanged however often it is read.
#[tokio::test]
async fn a_status_publication_writes_only_the_availability_and_its_stamp() {
    let case_setup = "setting up the images";
    let router = emulator();

    let built = image_with_successful_build(&router, case_setup, "write-scope-built").await;
    let second = second_version(&router, case_setup, &built).await;
    let failed = image_with_failed_build(&router, case_setup, "write-scope-failed").await;

    // Rows span the build state the version settled on, which of an image's versions is
    // addressed, whether the status asked for differs from the one on the record, and
    // whether another version has to step down.
    let rows: Vec<WriteScopeRow<'_>> = vec![
        (
            "the first version of a built image is blocked",
            built.as_str(),
            vec![FIRST_IMAGE_VERSION, second.as_str()],
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_INACTIVE,
            VERSION_STATE_SUCCESSFUL,
        ),
        (
            "the first version of a built image is published again",
            built.as_str(),
            vec![FIRST_IMAGE_VERSION, second.as_str()],
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_ACTIVE,
            VERSION_STATE_SUCCESSFUL,
        ),
        (
            "the status already on the record is published once more",
            built.as_str(),
            vec![FIRST_IMAGE_VERSION, second.as_str()],
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_ACTIVE,
            VERSION_STATE_SUCCESSFUL,
        ),
        (
            "the other version of the same image takes the pointer",
            built.as_str(),
            vec![FIRST_IMAGE_VERSION, second.as_str()],
            second.as_str(),
            VERSION_STATUS_ACTIVE,
            VERSION_STATE_SUCCESSFUL,
        ),
        (
            "a version whose build failed is published",
            failed.as_str(),
            vec![FIRST_IMAGE_VERSION],
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_ACTIVE,
            VERSION_STATE_FAILED,
        ),
        (
            "a version whose build failed is blocked",
            failed.as_str(),
            vec![FIRST_IMAGE_VERSION],
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_INACTIVE,
            VERSION_STATE_FAILED,
        ),
    ];

    for (case, image_arn, versions, addressed, asked, expected_state) in rows {
        let before = records_of(&router, case, image_arn, &versions).await;
        let (status, image) = get_image(&router, image_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the image reads back, got {status} {image}"
        );
        let held_pointer = image["latestActiveImageVersion"]
            .as_str()
            .map(str::to_string);

        let (status, published) = publish_status(&router, image_arn, addressed, asked).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {published}"
        );
        assert_eq!(
            published["status"].as_str(),
            Some(asked),
            "case {case:?}: expected status {asked}, got {published}"
        );
        assert_eq!(
            published["state"].as_str(),
            Some(expected_state),
            "case {case:?}: expected the build's state {expected_state}, got {published}"
        );

        // The addressed record: only the availability and its stamp may differ from what
        // the record carried before the call.
        let addressed_before = &before[addressed];
        for member in members(addressed_before, &published) {
            if WRITTEN_MEMBERS.contains(&member.as_str()) {
                continue;
            }
            assert_eq!(
                published[&member], addressed_before[&member],
                "case {case:?}: publishing a status must leave {member} as it was, got {} after \
                 {}",
                published[&member], addressed_before[&member]
            );
        }
        let stamped = instant(&published, "updatedAt").unwrap_or_else(|| {
            panic!("case {case:?}: the publication stamps updatedAt, got {published}")
        });
        for (member, earlier) in [
            ("createdAt", instant(addressed_before, "createdAt")),
            ("updatedAt", instant(addressed_before, "updatedAt")),
        ] {
            if let Some(earlier) = earlier {
                assert!(
                    stamped >= earlier,
                    "case {case:?}: the stamp cannot precede the record's {member}, got \
                     {stamped} before {earlier}"
                );
            }
        }

        // The answer IS the stored record, and reading it moves nothing: two further
        // reads report byte-for-byte what the publication answered.
        for look in 1..=2 {
            let (status, read) = get_version(&router, image_arn, addressed).await;
            assert_eq!(
                status, 200,
                "case {case:?}: look {look}: the version reads back, got {status} {read}"
            );
            assert_eq!(
                read, published,
                "case {case:?}: look {look}: the read must report exactly the published \
                 record, got {read} after {published}"
            );
        }

        // Every OTHER version of the image: the one that held the pointer steps down —
        // which is a write of its availability and its stamp, and of nothing else — and a
        // version the call neither addressed nor stepped down is not written at all.
        let steps_down = asked == VERSION_STATUS_ACTIVE
            && held_pointer.as_deref().is_some_and(|held| held != addressed);
        for (image_version, record) in &before {
            if image_version == addressed {
                continue;
            }
            let (status, now) = get_version(&router, image_arn, image_version).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the version {image_version} reads back, got {status} {now}"
            );
            if steps_down && held_pointer.as_deref() == Some(image_version.as_str()) {
                assert_eq!(
                    now["status"].as_str(),
                    Some(VERSION_STATUS_INACTIVE),
                    "case {case:?}: the version that held the pointer steps down, got {now}"
                );
                for member in members(record, &now) {
                    if WRITTEN_MEMBERS.contains(&member.as_str()) {
                        continue;
                    }
                    assert_eq!(
                        now[&member], record[&member],
                        "case {case:?}: stepping {image_version} down must leave {member} as it \
                         was, got {} after {}",
                        now[&member], record[&member]
                    );
                }
            } else {
                assert_eq!(
                    &now, record,
                    "case {case:?}: the version {image_version} was neither addressed nor \
                     stepped down, so nothing of it may be written, got {now} after {record}"
                );
            }
        }

        // The write is STAMPED, not merely stampable: a publication must move the
        // addressed record's `updatedAt` past what it carried. Publishing the same
        // availability again is the same write, so the stamp a later publication reports
        // advances as the clock does — and the one thing that keeps it where it was is a
        // handler that does not stamp the write at all. Looked at again until it has
        // advanced, so what the case waits for is that CONDITION rather than any duration:
        // two publications inside one tick of the wire timestamp's resolution report one
        // instant, and the next look reports the later one.
        let earlier = instant(addressed_before, "updatedAt");
        poll_until(
            case,
            &format!(
                "publishing {asked} on {addressed} stamps updatedAt past {earlier:?}"
            ),
            || async {
                let (status, again) = publish_status(&router, image_arn, addressed, asked).await;
                status == 200
                    && instant(&again, "updatedAt")
                        .is_some_and(|now| earlier.is_none_or(|earlier| now > earlier))
            },
        )
        .await;
    }
}
