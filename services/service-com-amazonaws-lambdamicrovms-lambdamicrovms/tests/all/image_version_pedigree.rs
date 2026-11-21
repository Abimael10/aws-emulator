// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PEDIGREE a MicroVM image version record carries and for the
//! bookkeeping its settlement writes on the image, driven over the same router the
//! emulator boots with Amazon S3 mounted beside this service on one engine.
//!
//! RULE 1: a version's `baseImageVersion` is the base image version that version is built
//! from — the version the request pinned when it pinned one, and otherwise the version the
//! base image the request named currently publishes: the version an account's own image is
//! serving when the base image is one of its own, the newest generation the AWS-managed
//! catalogue publishes when it is a managed one, and the `major.minor` initial version
//! when the base image it named publishes nothing at all.
//!
//! RULE 2: `latestFailedImageVersion` names the latest version of the image that settled
//! on the failed branch, so a configuration update's own 200 names the version that was
//! already broken when it was called, and reports it identically to the image's own read.
//!
//! RULE 3: every response that reports a SETTLED version carries that version's
//! `versionStateTimeBucket` — the state it settled in, a `#`, and the compact hour bucket
//! of the instant it settled at — with the version read, the version listing row and the
//! status publication all reporting the one value.
//!
//! Each test states its rule and exercises it over a SET of inputs: which namespace the
//! base image is in and whether the request pinned a version, which version of the image
//! had already failed, and which branch the build settled on. The wire harness (`call`,
//! `image_body`, …) is the one `microvm_lifecycle` owns, and the Amazon S3 router and the
//! object staging are the ones `image_build_outcome` owns, because a settled FAILURE can
//! only be driven with the owner of objects on the same engine.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::code_artifact::BUCKET_ABSENT_REASON;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    managed_base_image_arn, published_managed_base_image_versions, FIRST_IMAGE_VERSION,
    MANAGED_BASE_IMAGE_IDS, VERSION_STATE_FAILED, VERSION_STATE_SUCCESSFUL,
    VERSION_STATUS_INACTIVE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_build_outcome::{emulator, get_image, get_version, stage_object};
use crate::microvm_lifecycle::{as_uri_value, call, image_body, REGION};

/// The bucket and key the staged code artifact lives at, which is the address the
/// harness's default `codeArtifact` names, so a build that names it is SERVED.
const ARTIFACT_BUCKET: &str = "artifacts";
const ARTIFACT_KEY: &str = "app.zip";

/// A `codeArtifact$uri` whose BUCKET the object store does not serve, so the build of the
/// version carrying it settles on the failure branch.
const ABSENT_BUCKET_URI: &str = "s3://mvm-pedigree-no-such-bucket/app.zip";

/// How long a settlement poll waits for the clock-owed build before giving up: generous,
/// because the only thing under test is the state the build settles IN, never how quickly
/// it gets there.
const SETTLEMENT_DEADLINE: Duration = Duration::from_secs(60);

/// How often that poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A member's text, or the empty string when the member is absent.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The compact bucket digits the instant `at` (epoch seconds, as a JSON timestamp of
/// this service carries it) falls in, derived INDEPENDENTLY of the projection under test:
/// the ISO-8601 rendering of the same instant, with the last two digits of its year, its
/// month, its day and its hour read off it.
fn expected_bucket_digits(at: f64, case: &str) -> String {
    let rendered = aws_smithy_types::DateTime::from_secs_f64(at)
        .fmt(aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the settlement instant renders as an instant: {error}")
        });
    // `2026-11-02T07:29:53Z` -> `20261102072953` -> `26110207`.
    let digits: String = rendered.chars().filter(char::is_ascii_digit).collect();
    digits
        .get(2..10)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the rendered instant {rendered:?} carries its calendar")
        })
        .to_string()
}

/// Create the image `name` with whatever `extra` adds to the request, asserting the 201,
/// and hand back the response.
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

/// Update the image `identifier` with whatever `extra` adds, asserting the 200, and hand
/// back the response.
async fn update(
    router: &MultiServiceRouter,
    identifier: &str,
    name: &str,
    extra: Value,
    case: &str,
) -> Value {
    let uri = format!("/2025-09-09/microvm-images/{}", as_uri_value(identifier));
    let (status, body) = call(router, "PUT", &uri, Some(image_body(name, extra))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the update, got {status} {body}"
    );
    body
}

/// Publish `status` for one version of one image, asserting the 200, and hand back the
/// response.
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
        "case {case:?}: the version listing got {status} {body}"
    );
    body["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|row| row["imageVersion"].as_str() == Some(image_version))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the listing carries the version {image_version}: got {body}")
        })
}

/// The version `image_version` of `identifier` once its asynchronous build has SETTLED.
///
/// The build is owed by the clock, so this waits for the CONDITION — a terminal state —
/// by looking again until a generous deadline passes, rather than for any span of time.
async fn settled_version(
    router: &MultiServiceRouter,
    identifier: &str,
    image_version: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + SETTLEMENT_DEADLINE;
    loop {
        let (status, version) = get_version(router, identifier, image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 for version {image_version}, got {status} {version}"
        );
        let state = text(&version, "state");
        if state == VERSION_STATE_SUCCESSFUL || state == VERSION_STATE_FAILED {
            return version;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: version {image_version} of {identifier} never settled, \
             last read {version}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a version's `baseImageVersion` is the base image version it is built from — the
/// one the request pinned, and otherwise the version the base image it named publishes
/// (the version an own image is serving, the newest generation a managed base image's
/// catalogue entry publishes, and the `major.minor` initial version for a base image
/// publishing none) — reported identically by the create, the version read and the
/// listing row.
#[tokio::test]
async fn a_version_records_the_base_image_version_it_is_built_from() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    // The AWS-managed base image the harness builds on, and the generations its catalogue
    // entry publishes (`state::published_managed_base_image_versions`): a client may PIN
    // any of them, and the NEWEST one is what an unpinned build on that base image is
    // resolved to.
    let managed_id = MANAGED_BASE_IMAGE_IDS[0];
    let managed_base = managed_base_image_arn(REGION, managed_id);
    let published = published_managed_base_image_versions(managed_id);
    let newest_generation = published
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the catalogue publishes a generation of {managed_id}"))
        .to_string();
    let oldest_generation = published
        .last()
        .copied()
        .unwrap_or_else(|| panic!("the catalogue publishes a generation of {managed_id}"))
        .to_string();

    // A base image of the OTHER namespace: an image of this account, serving the first
    // version it built.
    let serving_first = create(&router, "pedigree-base-first", json!({})).await;
    let serving_first_arn = text(&serving_first, "imageArn");
    settled_version(
        &router,
        &serving_first_arn,
        FIRST_IMAGE_VERSION,
        "the own base image serving its first version",
    )
    .await;

    // The same, one update later, so the version it serves is no longer the first.
    let serving_later = create(&router, "pedigree-base-later", json!({})).await;
    let serving_later_arn = text(&serving_later, "imageArn");
    settled_version(
        &router,
        &serving_later_arn,
        FIRST_IMAGE_VERSION,
        "the own base image before its update",
    )
    .await;
    let updated = update(
        &router,
        &serving_later_arn,
        "pedigree-base-later",
        json!({}),
        "the own base image's update",
    )
    .await;
    let promoted = text(&updated, "imageVersion");
    settled_version(
        &router,
        &serving_later_arn,
        &promoted,
        "the own base image serving a later version",
    )
    .await;

    // An image of this account that serves NOTHING: its only build settled on the failure
    // branch, so the base image holds no version of its own to resolve.
    let unserved = create(
        &router,
        "pedigree-base-unserved",
        json!({"codeArtifact": {"uri": ABSENT_BUCKET_URI}}),
    )
    .await;
    let unserved_arn = text(&unserved, "imageArn");
    settled_version(
        &router,
        &unserved_arn,
        FIRST_IMAGE_VERSION,
        "the own base image serving nothing",
    )
    .await;

    // Rows: whether the request pins a version and, when it does not, which record holds
    // the current version of the base image it named.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "pinned in this service's own major.minor form",
            "pedigree-pinned-major",
            json!({"baseImageVersion": "3.1"}),
            "3.1".to_string(),
        ),
        (
            "pinned to the oldest generation the catalogue publishes",
            "pedigree-pinned-generation",
            json!({"baseImageVersion": oldest_generation.clone()}),
            oldest_generation.clone(),
        ),
        (
            "unpinned, on the AWS-managed base image",
            "pedigree-managed",
            json!({"baseImageArn": managed_base.clone()}),
            newest_generation.clone(),
        ),
        (
            "unpinned, on an own image serving its first version",
            "pedigree-own-first",
            json!({"baseImageArn": serving_first_arn.clone()}),
            FIRST_IMAGE_VERSION.to_string(),
        ),
        (
            "unpinned, on an own image serving a later version",
            "pedigree-own-later",
            json!({"baseImageArn": serving_later_arn.clone()}),
            promoted.clone(),
        ),
        (
            "unpinned, on an own image serving nothing",
            "pedigree-own-unserved",
            json!({"baseImageArn": unserved_arn.clone()}),
            FIRST_IMAGE_VERSION.to_string(),
        ),
    ];

    for (case, name, extra, expected) in rows {
        let created = create(&router, name, extra).await;
        let actual = created["baseImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            actual.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected baseImageVersion {expected:?}, got {actual:?}"
        );

        // The response is the stored record, so every read of that version reports the
        // same pedigree.
        let image_arn = text(&created, "imageArn");
        let image_version = text(&created, "imageVersion");
        let (status, read) = get_version(&router, &image_arn, &image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the version read, got {status} {read}"
        );
        let read_version = read["baseImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            read_version.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the version read to report {expected:?}, \
             got {read_version:?}"
        );
        let row = version_row(&router, &image_arn, &image_version, case).await;
        let row_version = row["baseImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            row_version.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the listing row to report {expected:?}, \
             got {row_version:?}"
        );
    }
}

/// RULE: `latestFailedImageVersion` names the latest version of the image that settled on
/// the failed branch, so each configuration update's own 200 names whichever version was
/// already broken when it was called — none while nothing has failed — and the image's own
/// read reports the same value, while the version the image serves is left alone.
#[tokio::test]
async fn an_update_names_the_images_latest_failed_version() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    let created = create(&router, "pedigree-failed-bookkeeping", json!({})).await;
    let image_arn = text(&created, "imageArn");
    assert!(
        created["latestFailedImageVersion"].is_null(),
        "a create reports no failed version: got {created}"
    );
    settled_version(&router, &image_arn, FIRST_IMAGE_VERSION, "the first build").await;

    // Rows: successive updates whose artifact the store does not serve. Each one's 200
    // must name the version the PREVIOUS round left failed, so the member moves on with
    // the record instead of naming one version forever.
    let mut expected_failed: Option<String> = None;
    for round in 1..=3 {
        let case = format!("update {round} of 3");
        let updated = update(
            &router,
            &image_arn,
            "pedigree-failed-bookkeeping",
            json!({"codeArtifact": {"uri": ABSENT_BUCKET_URI}}),
            &case,
        )
        .await;
        let answered = updated["latestFailedImageVersion"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            answered, expected_failed,
            "case {case:?}: expected latestFailedImageVersion {expected_failed:?}, \
             got {answered:?}"
        );

        // One record, one projection: the image's own read answers the same value.
        let (status, image) = get_image(&router, &image_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the image read, got {status} {image}"
        );
        let read = image["latestFailedImageVersion"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            read, expected_failed,
            "case {case:?}: expected the image read to report {expected_failed:?}, got {read:?}"
        );

        // The version this update minted settles on the failed branch with the reason the
        // object store answered, and is the version the NEXT round must be told about.
        let minted = text(&updated, "imageVersion");
        let settled = settled_version(&router, &image_arn, &minted, &case).await;
        let state = text(&settled, "state");
        assert_eq!(
            state, VERSION_STATE_FAILED,
            "case {case:?}: expected version {minted} to settle {VERSION_STATE_FAILED}, \
             got {state:?}"
        );
        let reason = settled["stateReason"].as_str().map(str::to_string);
        assert_eq!(
            reason.as_deref(),
            Some(BUCKET_ABSENT_REASON),
            "case {case:?}: expected stateReason {BUCKET_ABSENT_REASON:?}, got {reason:?}"
        );

        // A build that produced nothing cannot replace the version the image serves.
        let (status, image) = get_image(&router, &image_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from the image re-read, got {status} {image}"
        );
        let active = image["latestActiveImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            active.as_deref(),
            Some(FIRST_IMAGE_VERSION),
            "case {case:?}: expected the served version {FIRST_IMAGE_VERSION:?} to stay \
             active, got {active:?}"
        );
        let failed_now = image["latestFailedImageVersion"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            failed_now.as_deref(),
            Some(minted.as_str()),
            "case {case:?}: expected latestFailedImageVersion {minted:?} once the build \
             settled, got {failed_now:?}"
        );
        expected_failed = Some(minted);
    }
}

/// RULE: a settled version's `versionStateTimeBucket` is the state it settled in, a `#`
/// and the compact hour bucket of the instant it settled at — the one value the version
/// read, the version listing row and a status publication all report.
#[tokio::test]
async fn every_response_reporting_a_settled_version_carries_its_state_bucket() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    // Rows: the two branches a build settles on, because the bucket carries the state it
    // settled in and must therefore differ between them.
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "the artifact is served",
            "pedigree-bucket-served",
            format!("s3://{ARTIFACT_BUCKET}/{ARTIFACT_KEY}"),
            VERSION_STATE_SUCCESSFUL,
        ),
        (
            "the bucket does not exist",
            "pedigree-bucket-absent",
            ABSENT_BUCKET_URI.to_string(),
            VERSION_STATE_FAILED,
        ),
    ];

    for (case, name, uri, expected_state) in rows {
        let created = create(&router, name, json!({"codeArtifact": {"uri": uri}})).await;
        let image_arn = text(&created, "imageArn");
        let version = settled_version(&router, &image_arn, FIRST_IMAGE_VERSION, case).await;

        let state = text(&version, "state");
        assert_eq!(
            state, expected_state,
            "case {case:?}: expected version state {expected_state}, got {state:?}"
        );

        let bucket = version["versionStateTimeBucket"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a settled version carries a bucket: got {version}")
            })
            .to_string();
        let (reported_state, digits) = bucket.split_once('#').unwrap_or_else(|| {
            panic!("case {case:?}: expected a '<state>#<bucket>' value, got {bucket:?}")
        });
        assert_eq!(
            reported_state, expected_state,
            "case {case:?}: expected the bucket to name the state {expected_state}, \
             got {bucket:?}"
        );
        assert!(
            digits.len() == 8 && digits.chars().all(|digit| digit.is_ascii_digit()),
            "case {case:?}: expected eight digits of year, month, day and hour, \
             got {digits:?}"
        );

        // The bucket is a projection of the instant the state settled at — the version's
        // own `updatedAt` at that point — so it must agree with it.
        let settled_at = version["updatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case:?}: a settled version reports updatedAt: got {version}")
        });
        let expected_digits = expected_bucket_digits(settled_at, case);
        assert_eq!(
            digits, expected_digits,
            "case {case:?}: expected the bucket of the settlement instant \
             {expected_digits:?}, got {digits:?}"
        );

        // Every other answer about the version carries the same value.
        let row = version_row(&router, &image_arn, FIRST_IMAGE_VERSION, case).await;
        let listed = row["versionStateTimeBucket"].as_str().map(str::to_string);
        assert_eq!(
            listed.as_deref(),
            Some(bucket.as_str()),
            "case {case:?}: expected the listing row to carry {bucket:?}, got {listed:?}"
        );
        let published = publish_status(
            &router,
            &image_arn,
            FIRST_IMAGE_VERSION,
            VERSION_STATUS_INACTIVE,
            case,
        )
        .await;
        let answered = published["versionStateTimeBucket"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            answered.as_deref(),
            Some(bucket.as_str()),
            "case {case:?}: expected the status publication to carry {bucket:?}, \
             got {answered:?}"
        );
    }
}

/// RULE: every ROW of a version listing reports the base image version THAT row's own
/// build resolved — the page is an image's whole pedigree, so rows whose builds named
/// different base images, or pinned a version where another pinned none, each carry their
/// own resolution rather than one value for the image — and every row reports what the
/// single-version read of the same version reports.
#[tokio::test]
async fn every_row_of_a_version_listing_reports_its_own_resolved_base_image_version() {
    let router = emulator();
    stage_object(&router, ARTIFACT_BUCKET, ARTIFACT_KEY).await;

    // The AWS-managed base image and the generations its catalogue entry publishes: the
    // newest is what an unpinned build on it resolves to, any of them may be pinned.
    let managed_id = MANAGED_BASE_IMAGE_IDS[0];
    let managed_base = managed_base_image_arn(REGION, managed_id);
    let published = published_managed_base_image_versions(managed_id);
    let newest_generation = published
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the catalogue publishes a generation of {managed_id}"))
        .to_string();
    let oldest_generation = published
        .last()
        .copied()
        .unwrap_or_else(|| panic!("the catalogue publishes a generation of {managed_id}"))
        .to_string();

    // A base image of the other namespace: an image of this account, serving the version
    // its own build produced — which is what a build naming it resolves to.
    let own_base = create(&router, "listing-pedigree-base", json!({})).await;
    let own_base_arn = text(&own_base, "imageArn");
    settled_version(
        &router,
        &own_base_arn,
        FIRST_IMAGE_VERSION,
        "the own base image",
    )
    .await;
    let (status, own_base_read) = get_image(&router, &own_base_arn).await;
    assert_eq!(
        status, 200,
        "the own base image reads back: got {status} {own_base_read}"
    );
    let own_base_serving = text(&own_base_read, "latestActiveImageVersion");
    assert!(
        !own_base_serving.is_empty(),
        "the own base image serves a version to resolve: got {own_base_read}"
    );

    // One image, whose FIRST version is built unpinned on the managed base image.
    let name = "listing-pedigree";
    let created = create(
        &router,
        name,
        json!({"baseImageArn": managed_base.clone()}),
    )
    .await;
    let image_arn = text(&created, "imageArn");
    let mut expected: Vec<(String, String, String)> = vec![(
        text(&created, "imageVersion"),
        newest_generation.clone(),
        "unpinned, on the AWS-managed base image".to_string(),
    )];

    // Each further row is an UPDATE, which mints another version of the same image: the
    // rows differ in which namespace the base image is in and in whether the request
    // pinned a version, so no two rows of the page may resolve alike.
    let updates: Vec<(&str, Value, String)> = vec![
        (
            "pinned to the oldest generation the catalogue publishes",
            json!({
                "baseImageArn": managed_base.clone(),
                "baseImageVersion": oldest_generation.clone(),
            }),
            oldest_generation.clone(),
        ),
        (
            "unpinned, on an own image serving a version",
            json!({"baseImageArn": own_base_arn.clone()}),
            own_base_serving.clone(),
        ),
        (
            "pinned in this service's own major.minor form",
            json!({"baseImageArn": managed_base.clone(), "baseImageVersion": "5.4"}),
            "5.4".to_string(),
        ),
    ];
    for (case, extra, resolved) in updates {
        // An image admits another update once the version it is building has settled, so
        // each round waits for the CONDITION the previous one started.
        let previous = expected
            .last()
            .map(|(version, ..)| version.clone())
            .unwrap_or_default();
        settled_version(&router, &image_arn, &previous, case).await;
        let updated = update(&router, &image_arn, name, extra, case).await;
        expected.push((
            text(&updated, "imageVersion"),
            resolved,
            case.to_string(),
        ));
    }
    let last = expected
        .last()
        .map(|(version, ..)| version.clone())
        .unwrap_or_default();
    settled_version(&router, &image_arn, &last, "the last version").await;

    // The page: one row per version, each reporting its own build's resolution.
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions",
        as_uri_value(&image_arn)
    );
    let (status, page) = call(&router, "GET", &uri, None).await;
    assert_eq!(status, 200, "the version listing got {status} {page}");
    let rows = page["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        rows.len(),
        expected.len(),
        "expected one row per version built ({}), got {page}",
        expected.len()
    );

    for (image_version, resolved, case) in &expected {
        let row = rows
            .iter()
            .find(|row| row["imageVersion"].as_str() == Some(image_version.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the page carries the version {image_version}: got {page}")
            });
        let listed = row["baseImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            listed.as_deref(),
            Some(resolved.as_str()),
            "case {case:?}: expected the row for version {image_version} to report \
             baseImageVersion {resolved:?}, got {listed:?}"
        );

        // One record, one pedigree: the single-version read answers the same value.
        let (status, read) = get_version(&router, &image_arn, image_version).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the version read got {status} {read}"
        );
        let served = read["baseImageVersion"].as_str().map(str::to_string);
        assert_eq!(
            served.as_deref(),
            Some(resolved.as_str()),
            "case {case:?}: expected the version read of {image_version} to report \
             {resolved:?}, got {served:?}"
        );
    }
}
