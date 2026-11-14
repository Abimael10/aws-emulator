// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a MicroVM image version reports a `versionStateTimeBucket`
//! and for the bucket half of that value, driven over the same router the emulator boots
//! with Amazon S3 mounted beside this service on one engine (a build's outcome is the
//! object store's answer, so a FAILED version can only be reached through the owner of
//! objects).
//!
//! RULE: the member is a projection of the stored record and not of the last write, so
//! every answer reporting one version of a MicroVM image — the single-version read, a row
//! of the version listing, the status publication — carries exactly the value that
//! version's own state says it has: the state, a `#` and the compact calendar hour the
//! state settled in once it HAS settled, and nothing at all while the build it is waiting
//! on is still in flight. The bucket half is the hour the settlement fell in, so
//! settlements of one hour share it and the values sort as the instants do.
//!
//! What a SETTLED version's answers carry, and that the three answers agree, is
//! `image_version_pedigree`'s RULE 3; this suite covers the other half of the rule — the
//! member's presence being decided by the record's own state, and the bucket's behaviour
//! at the boundaries of an hour, a day and a year.
//!
//! Each test exercises the rule over a SET of inputs: the rows differ in which branch the
//! build settles on, in whether the version's build has settled yet, in which build minted
//! the version, in which answer reports it, and in where in an hour, a day and a year the
//! settlement fell. The wire harness (`call`, `PAST_THE_SPAN`, …) is the one
//! `microvm_lifecycle` owns, and the Amazon S3 harness is `image_build_outcome`'s.

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    state_time_bucket, FIRST_IMAGE_VERSION, VERSION_STATE_FAILED, VERSION_STATE_IN_PROGRESS,
    VERSION_STATE_SUCCESSFUL, VERSION_STATUS_ACTIVE, VERSION_STATUS_INACTIVE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::{create_image, emulator, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, image_body};

/// How long a poll for the clock-owed build may take before the test gives up: generous,
/// so a loaded host converges rather than fails.
const SETTLE_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(30);

/// How long one such poll waits before looking again.
const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// The bucket this suite stages a fetchable artifact in, so a build of it succeeds.
const SERVED_BUCKET: &str = "mvm-bucket-artifacts";

/// The object key of that artifact.
const SERVED_KEY: &str = "app.zip";

/// A `codeArtifact$uri` whose bucket the object store does not serve, so a build of it
/// settles on the failure branch.
const UNFETCHABLE_URI: &str = "s3://mvm-bucket-no-such-bucket/app.zip";

/// The `versionStateTimeBucket` a version in `state` that settled at `at` must be reported
/// under: the composite of the state and the bucket, over the service's own projection of
/// the bucket half.
fn expected_composite(state: &str, at: DateTime) -> Option<String> {
    state_time_bucket(at).map(|bucket| format!("{state}#{bucket}"))
}

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

/// The row of `ListMicrovmImageVersions` reporting `image_version` of `identifier`.
async fn listed_version(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
) -> Value {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions",
        as_uri_value(identifier)
    );
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(status, 200, "list versions of {identifier}: got {body}");
    body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|row| row["imageVersion"].as_str() == Some(image_version))
        .unwrap_or_else(|| panic!("the listing carries version {image_version}: got {body}"))
}

/// A member's text, or the empty string when the member is absent.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The `versionStateTimeBucket` an answer reports, or `None` when it reports none.
fn bucket(answer: &Value) -> Option<String> {
    answer["versionStateTimeBucket"]
        .as_str()
        .map(str::to_string)
}

/// Assert that `answer`'s bucket agrees with `answer`'s OWN state: nothing at all while
/// the version's build is in flight, and otherwise that state, a `#` and a run of digits.
/// Each answer is judged against the state it itself reports, so the assertion holds
/// however fast the host runs through a build's span.
fn assert_bucket_agrees_with_its_own_state(answer: &Value, case: &str) {
    let state = text(answer, "state");
    let actual = bucket(answer);
    if state == VERSION_STATE_IN_PROGRESS {
        assert!(
            actual.is_none(),
            "case {case:?}: a version whose build is in flight has no settled state to \
             bucket, got {actual:?}"
        );
        return;
    }
    let value = actual.unwrap_or_else(|| {
        panic!("case {case:?}: a version in state {state} reports a bucket, got {answer}")
    });
    let prefix = format!("{state}#");
    let digits = value.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected a bucket starting {prefix:?}, got {value:?}")
    });
    assert!(
        !digits.is_empty() && digits.chars().all(|character| character.is_ascii_digit()),
        "case {case:?}: expected the bucket half of {value:?} to be digits, got {digits:?}"
    );
}

/// Every answer about `image_version` of `identifier` — the read, the listing row and a
/// status publication — judged against its own state, and asserted to carry ONE value.
async fn assert_every_answer_agrees(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    status_asked: &str,
    case: &str,
) {
    let (status, read) = get_version(router, identifier, image_version).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for the version read, got {status} {read}"
    );
    assert_bucket_agrees_with_its_own_state(&read, case);

    let row = listed_version(router, identifier, image_version).await;
    assert_bucket_agrees_with_its_own_state(&row, case);
    assert_eq!(
        bucket(&row),
        bucket(&read),
        "case {case:?}: the listing row and the read report one bucket, got {:?} and {:?}",
        bucket(&row),
        bucket(&read)
    );

    let (status, published) = publish_status(router, identifier, image_version, status_asked).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the publication, got {status} {published}"
    );
    assert_bucket_agrees_with_its_own_state(&published, case);
    assert_eq!(
        bucket(&published),
        bucket(&read),
        "case {case:?}: publishing a status writes availability and not state, so the \
         answer carries the read's bucket, got {:?} and {:?}",
        bucket(&published),
        bucket(&read)
    );
}

/// Wait for the CONDITION the build owes — the version's state leaving `IN_PROGRESS` — by
/// looking again until a generous deadline passes, rather than for any span of time.
async fn await_settled(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    case: &str,
) {
    let deadline = ::std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, version) = get_version(router, identifier, image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for version {image_version}, got {status} {version}"
        );
        if text(&version, "state") != VERSION_STATE_IN_PROGRESS {
            return;
        }
        assert!(
            ::std::time::Instant::now() < deadline,
            "case {case:?}: version {image_version} of {identifier} never settled, \
             last read {version}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: the member's presence is decided by the version's own state — absent while the
/// build it is waiting on is in flight, present once that state has settled, on whichever
/// branch — and every answer about the version agrees, whichever build minted it.
#[tokio::test]
async fn a_version_carries_the_bucket_its_own_state_says_it_has() {
    let router = emulator();
    stage_object(&router, SERVED_BUCKET, SERVED_KEY).await;
    let served = format!("s3://{SERVED_BUCKET}/{SERVED_KEY}");

    // Rows: which branch the version's build settles on, because the member must appear on
    // both and carry each branch's own state.
    let rows: Vec<(&str, &str, String)> = vec![
        ("the artifact is served", "bucket-served", served.clone()),
        (
            "the artifact's bucket does not exist",
            "bucket-absent",
            UNFETCHABLE_URI.to_string(),
        ),
    ];

    for (case, name, uri) in rows {
        let image_arn = create_image(&router, name, &uri).await;

        // Straight after the create the build has not settled, so the answers carry no
        // bucket; once the clock has made the build due they carry the settled state's.
        assert_every_answer_agrees(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_INACTIVE,
            &format!("{case}, before the build settles"),
        )
        .await;
        await_settled(&router, &image_arn, FIRST_IMAGE_VERSION, case).await;
        assert_every_answer_agrees(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_ACTIVE,
            &format!("{case}, once the build has settled"),
        )
        .await;

        // A SECOND version, minted by a configuration update rather than by the create:
        // which build minted a version does not change what it reports.
        let (status, updated) = call(
            &router,
            "PUT",
            &format!("/2025-09-09/microvm-images/{}", as_uri_value(&image_arn)),
            Some(image_body(
                "ignored-by-the-update",
                json!({"codeArtifact": {"uri": served}}),
            )),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second version must be minted, got {status} {updated}"
        );
        let second = text(&updated, "imageVersion");
        assert_every_answer_agrees(
            &router,
            &image_arn,
            &second,
            VERSION_STATUS_INACTIVE,
            &format!("{case}, an update's version before its build settles"),
        )
        .await;
        await_settled(&router, &image_arn, &second, case).await;
        assert_every_answer_agrees(
            &router,
            &image_arn,
            &second,
            VERSION_STATUS_ACTIVE,
            &format!("{case}, an update's version once its build has settled"),
        )
        .await;
    }
}

/// RULE: the bucket half is the compact calendar fields of the HOUR the settlement fell
/// in, wherever in an hour, a day or a year the instant falls: instants of one hour share a
/// bucket, instants of different hours do not, and the values sort as the instants do.
#[test]
fn the_bucket_is_the_hour_the_settlement_fell_in() {
    // Rows: where in the hour, the day and the year the settlement fell, the instant, and
    // the calendar fields of the hour it falls in — read off the instant beside it.
    let instants: &[(&str, &str, &str)] = &[
        ("the first second of an hour", "2026-11-05T09:00:00Z", "26110509"),
        ("a later second of that hour", "2026-11-05T09:17:43Z", "26110509"),
        ("the last second of that hour", "2026-11-05T09:59:59Z", "26110509"),
        ("the hour after it", "2026-11-05T10:00:00Z", "26110510"),
        ("the first hour of the next day", "2026-11-06T00:00:00Z", "26110600"),
        ("the first hour of the next year", "2027-01-01T00:00:00Z", "27010100"),
        ("the last hour of a leap day", "2028-02-29T23:30:00Z", "28022923"),
    ];

    let mut previous: Option<(&str, String)> = None;
    for (case, rendered, expected_digits) in instants {
        let at = DateTime::from_str(rendered, Format::DateTime).unwrap_or_else(|error| {
            panic!("case {case:?}: {rendered} parses as an instant, got {error}")
        });
        let actual = state_time_bucket(at);
        let expected = Some((*expected_digits).to_string());
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );

        // The value is a sortable key: later instants never sort before earlier ones.
        let value = actual.unwrap_or_default();
        if let Some((earlier_case, earlier)) = previous {
            assert!(
                earlier <= value,
                "case {case:?}: {value:?} must not sort before {earlier:?} of case \
                 {earlier_case:?}"
            );
        }
        previous = Some((case, value));
    }

    // The whole member is a key for state AND time: two states settling in one hour are two
    // different keys, so a client grouping by it groups by outcome as well as by hour.
    let at = DateTime::from_str("2026-11-05T09:00:00Z", Format::DateTime)
        .unwrap_or_else(|error| panic!("the instant parses, got {error}"));
    let left = expected_composite(VERSION_STATE_SUCCESSFUL, at);
    let right = expected_composite(VERSION_STATE_FAILED, at);
    assert_ne!(
        left, right,
        "two states settling in one hour have different keys, got {left:?} and {right:?}"
    );
}
