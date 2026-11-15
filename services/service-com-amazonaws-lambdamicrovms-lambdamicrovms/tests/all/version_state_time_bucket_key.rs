// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `versionStateTimeBucket` key every answer that reports one
//! VERSION of a MicroVM image carries, driven over the same router the emulator boots
//! with Amazon S3 mounted beside this service on one engine (a build's outcome is the
//! object store's answer, so a FAILED version can only be reached through the owner of
//! objects).
//!
//! RULE: the key is a composite of the version's own STATE and the time bucket that state
//! settled in — the state token, a literal `#`, then the bucket's compact digits — so
//! every response reporting one settled version (the status publication, the version's own
//! read and the listing row for it) carries the same key, and a version whose build has
//! not settled carries none.
//!
//! Each test states that rule and exercises it over a SET of inputs: the rows differ in
//! the state the version's build settled on and in the status published over it. The wire
//! harness (`call`, `PAST_THE_SPAN`, …) is `microvm_lifecycle`'s and the Amazon S3 harness
//! is `image_build_outcome`'s.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    FIRST_IMAGE_VERSION, VERSION_STATE_FAILED, VERSION_STATE_SUCCESSFUL, VERSION_STATUS_ACTIVE,
    VERSION_STATUS_INACTIVE,
};

use crate::image_build_outcome::{create_image, emulator, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, poll_until};

/// How many digits the bucket half of the key carries: the calendar fields of the
/// bucket's start down to its own width, without their separators — the compact form the
/// recorded key carries (`<STATE>#26110509`), rather than an instant.
const BUCKET_DIGITS: usize = 8;

/// The bucket of a fetchable artifact, so a build of it succeeds.
const SERVED_BUCKET: &str = "mvm-bucket-key-artifacts";

/// The object key of that artifact.
const SERVED_KEY: &str = "app.zip";

/// A `codeArtifact$uri` whose bucket the object store does not serve, so a build of it
/// settles on the failure branch.
const UNFETCHABLE_URI: &str = "s3://mvm-bucket-key-no-such-bucket/app.zip";

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

/// The versions of one image, as `ListMicrovmImageVersions` reports them.
async fn list_versions(router: &MultiServiceRouter, identifier: &str) -> Vec<Value> {
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions",
        as_uri_value(identifier)
    );
    let (status, body) = call(router, "GET", &uri, None).await;
    assert_eq!(status, 200, "list versions of {identifier}: got {body}");
    body["items"].as_array().cloned().unwrap_or_default()
}

/// The key `answer` reports, with the composite form asserted against the `state` that
/// same answer reports.
fn asserted_key(answer: &Value, case: &str) -> String {
    let key = answer["versionStateTimeBucket"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a settled version reports versionStateTimeBucket: {answer}")
        })
        .to_string();
    let state = answer["state"].as_str().unwrap_or_default();
    let Some((reported_state, bucket)) = key.split_once('#') else {
        panic!("case {case:?}: expected a '<state>#<bucket>' key, got {key:?}");
    };
    assert_eq!(
        reported_state, state,
        "case {case:?}: expected the version's own state {state:?} in the key, got {key:?}"
    );
    assert!(
        bucket.len() == BUCKET_DIGITS && bucket.chars().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected {BUCKET_DIGITS} digits of time bucket, got {key:?}"
    );
    key
}

/// Look at the version `1.0` of the image `identifier` until its build has SETTLED —
/// whichever branch it settled on, which is the condition the key reports the bucket of.
async fn poll_until_settled(router: &MultiServiceRouter, case: &str, identifier: &str) {
    poll_until(
        case,
        &format!("the build of {identifier} version {FIRST_IMAGE_VERSION} has settled"),
        || async {
            let (status, read) = get_version(router, identifier, FIRST_IMAGE_VERSION).await;
            status == 200
                && matches!(
                    read["state"].as_str(),
                    Some(VERSION_STATE_SUCCESSFUL) | Some(VERSION_STATE_FAILED)
                )
        },
    )
    .await;
}

/// One row: what a failing assertion calls it, the image name, its `codeArtifact` URI, and
/// the status to publish over the version that build settles.
type KeyRow = (&'static str, &'static str, String, &'static str);

/// The rows the key assertions span: the state the build settles on — which the object
/// store's answer for the artifact decides — and the status published over it, the two
/// members every answer carrying the key reports beside it.
fn key_rows() -> Vec<KeyRow> {
    let served = format!("s3://{SERVED_BUCKET}/{SERVED_KEY}");
    vec![
        (
            "a successful build blocked from launching",
            "bucket-key-successful-inactive",
            served.clone(),
            VERSION_STATUS_INACTIVE,
        ),
        (
            "a successful build published again",
            "bucket-key-successful-active",
            served,
            VERSION_STATUS_ACTIVE,
        ),
        (
            "a failed build blocked",
            "bucket-key-failed-inactive",
            UNFETCHABLE_URI.to_string(),
            VERSION_STATUS_INACTIVE,
        ),
        (
            "a failed build published ACTIVE",
            "bucket-key-failed-active",
            UNFETCHABLE_URI.to_string(),
            VERSION_STATUS_ACTIVE,
        ),
    ]
}

/// RULE: every answer that reports one settled version carries the SAME
/// `versionStateTimeBucket` — the version's state, a `#`, and the compact digits of the
/// bucket its state settled in — so the status publication, the version's own read and the
/// listing row for it cannot disagree about it.
#[tokio::test]
async fn every_answer_reporting_a_settled_version_carries_one_state_and_time_key() {
    let router = emulator();
    stage_object(&router, SERVED_BUCKET, SERVED_KEY).await;

    // Every image is created first, so the one wait below covers every build.
    let mut created: Vec<(&str, String, &str)> = Vec::new();
    for (case, name, uri, status_asked) in key_rows() {
        let image_arn = create_image(&router, name, &uri).await;
        created.push((case, image_arn, status_asked));
    }

    for (case, image_arn, status_asked) in created {
        poll_until_settled(&router, case, &image_arn).await;
        let (status, published) =
            publish_status(&router, &image_arn, FIRST_IMAGE_VERSION, status_asked).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {published}"
        );
        let key = asserted_key(&published, case);

        // The answer is the PERSISTED record: the version's own read reports the same key.
        let (status, read) = get_version(&router, &image_arn, FIRST_IMAGE_VERSION).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the version reads back, got {status} {read}"
        );
        let read_key = asserted_key(&read, case);
        assert_eq!(
            read_key, key,
            "case {case:?}: the read's key must match the publication's, got {read_key:?} \
             and {key:?}"
        );

        // So does the listing row, which is the same projection of the same record.
        let listed = list_versions(&router, &image_arn)
            .await
            .into_iter()
            .find(|row| row["imageVersion"].as_str() == Some(FIRST_IMAGE_VERSION))
            .unwrap_or_else(|| panic!("case {case:?}: the listing carries the version"));
        let listed_key = asserted_key(&listed, case);
        assert_eq!(
            listed_key, key,
            "case {case:?}: the listing row's key must match the publication's, got \
             {listed_key:?} and {key:?}"
        );
    }
}

/// RULE: the key reports the bucket a version's state SETTLED in, so a version whose build
/// is still in flight has no settled state and reports no key — on the status publication
/// and on the read alike.
#[tokio::test]
async fn a_version_whose_build_is_in_flight_reports_no_key() {
    let router = emulator();
    stage_object(&router, SERVED_BUCKET, SERVED_KEY).await;

    // Rows: a build that will succeed and one that will fail, read while each is still in
    // flight — neither has settled, so neither reports a key.
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a build that will succeed",
            "bucket-key-in-flight-served",
            format!("s3://{SERVED_BUCKET}/{SERVED_KEY}"),
        ),
        (
            "a build that will fail",
            "bucket-key-in-flight-absent",
            UNFETCHABLE_URI.to_string(),
        ),
    ];

    for (case, name, uri) in rows {
        let image_arn = create_image(&router, name, &uri).await;

        let (status, read) = get_version(&router, &image_arn, FIRST_IMAGE_VERSION).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the version reads back, got {status} {read}"
        );
        assert!(
            read["versionStateTimeBucket"].is_null(),
            "case {case:?}: a build in flight has settled nothing, got {:?}",
            read["versionStateTimeBucket"]
        );

        let (status, published) = publish_status(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_INACTIVE,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {published}"
        );
        assert!(
            published["versionStateTimeBucket"].is_null(),
            "case {case:?}: a build in flight has settled nothing, got {:?}",
            published["versionStateTimeBucket"]
        );

        // Once the build settles, the same two answers carry the key.
        poll_until_settled(&router, case, &image_arn).await;
        let (status, settled) = get_version(&router, &image_arn, FIRST_IMAGE_VERSION).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the version reads back, got {status} {settled}"
        );
        asserted_key(&settled, case);
    }
}
