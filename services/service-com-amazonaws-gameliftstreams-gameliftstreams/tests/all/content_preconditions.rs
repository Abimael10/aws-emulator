// SPDX-License-Identifier: CC0-1.0

//! What `CreateApplication` requires of the Amazon S3 location it is pointed at, and
//! how it reports that location back.
//!
//! "Before you create an application, upload your application content files to an
//! Amazon Simple Storage Service (Amazon S3) bucket", and "creating an application is
//! the only time Amazon GameLift Streams accesses your Amazon S3 bucket"
//! (`CreateApplication`'s own documentation). So the create READS the caller's bucket,
//! and three rules follow from that one read — each pinned below over a table of
//! locations rather than one recorded call:
//!
//! * the location must hold content: "Amazon GameLift Streams copies everything under
//!   the specified location" (`ApplicationSourceUri`), so a location holding no object
//!   is refused with the modelled `ValidationException` (400) and nothing is allocated;
//! * the content must be readable: the copy lists the bucket and gets each object it
//!   holds, so a location whose object store refuses either action is refused with the
//!   modelled `AccessDeniedException` (403) — and a location the same policy leaves
//!   alone is created as usual;
//! * the value names a PREFIX ("the location can have a multi-level prefix structure"),
//!   so it is reported back with exactly one trailing separator, by the create and by
//!   every later read of the application.
//!
//! Every case goes through the same front door a client uses — a `restJson1` request
//! against a router that mounts this service beside this emulator's own Amazon S3 on ONE
//! engine — so the objects and the bucket policies these rules are judged against are
//! real ones, staged through Amazon S3's own API.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::application_read_projection::{emulator_with_object_store, s3};
use super::stream_workflow::{call, text, REGION};

/// The bucket every case below stages its content in.
const BUCKET: &str = "gameliftstreams-precondition-content";

/// A second bucket, created and left EMPTY: a bucket that exists and holds nothing is
/// no content location either.
const EMPTY_BUCKET: &str = "gameliftstreams-precondition-empty";

/// A bucket name nothing ever creates.
const ABSENT_BUCKET: &str = "gameliftstreams-precondition-absent";

/// The objects staged in [`BUCKET`], as `(key, bytes)`.
///
/// Three layouts the rules below range over: one object directly under a prefix, one
/// nested two levels under another, and one at the root of the bucket.
const CONTENT: &[(&str, usize)] = &[
    ("build/start.sh", 9),
    ("nested/deep/pak0.bin", 21),
    ("root.bin", 3),
];

/// The message AWS refuses a content location holding no object with, without the
/// per-request internal id this service appends to every handler-side message.
const NO_OBJECTS: &str = "No S3 objects were found at the specified source URI";

/// The message AWS refuses a content location it may not read with, likewise.
const ACCESS_DENIED: &str =
    "Access was denied for s3:ListBucket and s3:GetObject actions on the specified source URI";

/// The wire error code of an error body (`__type`), or the empty string.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error message of an error body. A modelled error renders the member's `Message`.
fn error_message(body: &Value) -> String {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One handler-side message split into the text it asserts and the per-request
/// correlation id this service appends to every one of them — a per-run value, so it is
/// only ever checked for its form.
fn without_internal_id(message: &str, case: &str) -> String {
    let (head, suffix) = message
        .rsplit_once(" (Internal ID: ")
        .unwrap_or_else(|| panic!("case {case:?}: no internal id in {message:?}"));
    let id = suffix
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("case {case:?}: unterminated internal id in {message:?}"));
    assert!(
        !id.is_empty(),
        "case {case:?}: expected a correlation id in {message:?}"
    );
    head.to_string()
}

/// Create a bucket in [`REGION`].
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = s3(router)
        .create_bucket()
        .bucket(bucket)
        .create_bucket_configuration(
            ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(REGION))
                .build(),
        )
        .send()
        .await;
    assert!(
        created.is_ok(),
        "Amazon S3 must create {bucket}, got {created:?}"
    );
}

/// A router with this service beside Amazon S3, [`BUCKET`] holding [`CONTENT`] and
/// [`EMPTY_BUCKET`] holding nothing.
async fn emulator_with_content() -> MultiServiceRouter {
    let router = emulator_with_object_store();
    create_bucket(&router, BUCKET).await;
    create_bucket(&router, EMPTY_BUCKET).await;
    for (key, size) in CONTENT {
        let stored = s3(&router)
            .put_object()
            .bucket(BUCKET)
            .key(*key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(vec![
                b'c';
                *size
            ]))
            .send()
            .await;
        assert!(
            stored.is_ok(),
            "Amazon S3 must store {key} in {BUCKET}, got {stored:?}"
        );
    }
    router
}

/// `POST /applications` with `source` as the content location: the status and the body.
async fn create_application(
    router: &MultiServiceRouter,
    description: &str,
    source: &str,
) -> (u16, Value) {
    call(
        router,
        "POST",
        "/applications",
        Some(json!({
            "Description": description,
            "RuntimeEnvironment": {"Type": "UBUNTU", "Version": "22_04_LTS"},
            "ExecutablePath": "start.sh",
            "ApplicationSourceUri": source,
        })),
    )
    .await
}

/// Every application `ListApplications` reports, as `(description, arn)` pairs.
async fn listed_applications(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let (status, listed) = call(router, "GET", "/applications", None).await;
    assert_eq!(
        status, 200,
        "expected ListApplications 200, got {status} {listed}"
    );
    listed["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    (
                        item["Description"].as_str().unwrap_or_default().to_string(),
                        item["Arn"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that `description` names no application the store holds: a refused create
/// allocates nothing.
async fn assert_nothing_was_allocated(
    router: &MultiServiceRouter,
    description: &str,
    case: &str,
    before: usize,
) {
    let listed = listed_applications(router).await;
    assert_eq!(
        listed.len(),
        before,
        "case {case:?}: a refused create must allocate nothing, got {listed:?}"
    );
    assert!(
        !listed.iter().any(|(named, _)| named == description),
        "case {case:?}: the refused application must not be listed, got {listed:?}"
    );
}

/// A bucket policy document denying `actions` on `resources` to every principal.
fn deny(actions: &[&str], resources: &[String]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": "*",
            "Action": actions,
            "Resource": resources,
        }],
    })
    .to_string()
}

/// The ARN of [`BUCKET`] itself — the resource `s3:ListBucket` is judged over.
fn bucket_arn() -> String {
    format!("arn:aws:s3:::{BUCKET}")
}

/// The ARN of the objects under `prefix` of [`BUCKET`] — the resource `s3:GetObject` is
/// judged over.
fn objects_arn(prefix: &str) -> String {
    format!("arn:aws:s3:::{BUCKET}/{prefix}*")
}

/// Attach `document` to [`BUCKET`], or remove its policy when `document` is `None`.
async fn set_bucket_policy(router: &MultiServiceRouter, case: &str, document: Option<&str>) {
    match document {
        Some(document) => {
            let attached = s3(router)
                .put_bucket_policy()
                .bucket(BUCKET)
                .policy(document)
                .send()
                .await;
            assert!(
                attached.is_ok(),
                "case {case:?}: Amazon S3 must attach the bucket policy, got {attached:?}"
            );
        }
        None => {
            let removed = s3(router)
                .delete_bucket_policy()
                .bucket(BUCKET)
                .send()
                .await;
            assert!(
                removed.is_ok(),
                "case {case:?}: Amazon S3 must remove the bucket policy, got {removed:?}"
            );
        }
    }
}

/// RULE: a content location the object store holds no object at is refused with the
/// modelled `ValidationException` (400) and allocates nothing, and a location it holds
/// content at is created.
///
/// The rows differ along what the rule generalises over: whether the bucket exists at
/// all, whether it is empty, whether the prefix names objects, how deep they lie,
/// whether the prefix carries its separator, whether the value names the whole bucket,
/// and whether it names one object directly or a key nothing is stored at or under.
#[tokio::test]
async fn a_content_location_holding_no_object_is_refused() {
    let router = emulator_with_content().await;

    let accepted: Vec<(&str, String)> = vec![
        (
            "a prefix holding one object",
            format!("s3://{BUCKET}/build/"),
        ),
        (
            "the same prefix without its separator",
            format!("s3://{BUCKET}/build"),
        ),
        (
            "a prefix whose object lies two levels under it",
            format!("s3://{BUCKET}/nested/"),
        ),
        (
            "a partial prefix of a stored key, which is a byte prefix of it",
            format!("s3://{BUCKET}/buil"),
        ),
        (
            "an object named directly",
            format!("s3://{BUCKET}/build/start.sh"),
        ),
        ("the whole bucket", format!("s3://{BUCKET}/")),
        ("the bucket with no key at all", format!("s3://{BUCKET}")),
    ];
    let refused: Vec<(&str, String)> = vec![
        (
            "a bucket that was never created",
            format!("s3://{ABSENT_BUCKET}/build/"),
        ),
        (
            "a bucket that exists and holds nothing",
            format!("s3://{EMPTY_BUCKET}/"),
        ),
        (
            "a prefix the bucket holds nothing under",
            format!("s3://{BUCKET}/absent/"),
        ),
        (
            "a prefix differing from a stored one in case",
            format!("s3://{BUCKET}/Build/"),
        ),
        (
            "a key the bucket holds no object at or under",
            format!("s3://{BUCKET}/build/absent.sh"),
        ),
    ];

    for (index, (case, source)) in accepted.iter().enumerate() {
        let description = format!("precondition-accepted-{index}");
        let (status, created) = create_application(&router, &description, source).await;
        assert_eq!(
            status, 201,
            "case {case:?} ({source}): expected 201, got {status} {created}"
        );
        // The create allocated the resource: the id and ARN are in the body, and the
        // store reports the application under them.
        let arn = text(&created, "/Arn", case);
        let listed = listed_applications(&router).await;
        assert!(
            listed
                .iter()
                .any(|(named, listed_arn)| named == &description && listed_arn == &arn),
            "case {case:?}: expected {arn} among the listed applications, got {listed:?}"
        );
    }

    for (index, (case, source)) in refused.iter().enumerate() {
        let description = format!("precondition-refused-{index}");
        let before = listed_applications(&router).await.len();
        let (status, got) = create_application(&router, &description, source).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({source}): expected 400, got {status} {got}"
        );
        assert_eq!(
            error_code(&got),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {got}"
        );
        assert_eq!(
            without_internal_id(&error_message(&got), case),
            NO_OBJECTS,
            "case {case:?}: unexpected refusal message, got {got}"
        );
        assert!(
            got.get("Arn").is_none() && got.get("Id").is_none(),
            "case {case:?}: a refused create allocates no Arn and no Id, got {got}"
        );
        assert_nothing_was_allocated(&router, &description, case, before).await;
    }
}

/// RULE: a content location whose object store refuses either of the actions the copy
/// performs — listing the bucket, getting the objects it holds — is refused with the
/// modelled `AccessDeniedException` (403) and allocates nothing; a location the same
/// policy leaves readable is created as usual.
///
/// The rows differ along what the rule generalises over: WHICH action is denied,
/// WHICH resource the denial covers (the bucket, every object, the objects of one
/// prefix) and whether the location the request names falls under it.
#[tokio::test]
async fn a_content_location_this_service_may_not_read_is_refused() {
    let router = emulator_with_content().await;
    let build = format!("s3://{BUCKET}/build/");
    let nested = format!("s3://{BUCKET}/nested/");

    // (case, the policy in force, the location the request names, the status expected)
    let rows: Vec<(&str, Option<String>, &String, u16)> = vec![
        ("no policy at all", None, &build, 201),
        (
            "both actions denied over the bucket and its objects",
            Some(deny(
                &["s3:GetObject", "s3:ListBucket"],
                &[bucket_arn(), objects_arn("")],
            )),
            &build,
            403,
        ),
        (
            "the listing alone denied",
            Some(deny(&["s3:ListBucket"], &[bucket_arn()])),
            &build,
            403,
        ),
        (
            "the object read alone denied, over every object",
            Some(deny(&["s3:GetObject"], &[objects_arn("")])),
            &build,
            403,
        ),
        (
            "the object read denied over one prefix, naming that prefix",
            Some(deny(&["s3:GetObject"], &[objects_arn("build/")])),
            &build,
            403,
        ),
        (
            "the same denial, naming a prefix it does not cover",
            Some(deny(&["s3:GetObject"], &[objects_arn("build/")])),
            &nested,
            201,
        ),
        (
            "a denial of an action the copy does not perform",
            Some(deny(&["s3:DeleteObject"], &[objects_arn("")])),
            &build,
            201,
        ),
        ("the policy removed again", None, &build, 201),
    ];

    for (index, (case, document, source, expected)) in rows.iter().enumerate() {
        set_bucket_policy(&router, case, document.as_deref()).await;
        let description = format!("access-case-{index}");
        let before = listed_applications(&router).await.len();
        let (status, got) = create_application(&router, &description, source).await;
        assert_eq!(
            status, *expected,
            "case {case:?} ({source}): expected {expected}, got {status} {got}"
        );
        if *expected == 403 {
            assert_eq!(
                error_code(&got),
                "AccessDeniedException",
                "case {case:?}: expected AccessDeniedException, got {got}"
            );
            assert_eq!(
                without_internal_id(&error_message(&got), case),
                ACCESS_DENIED,
                "case {case:?}: unexpected refusal message, got {got}"
            );
            assert!(
                got.get("Arn").is_none() && got.get("Id").is_none(),
                "case {case:?}: a refused create allocates no Arn and no Id, got {got}"
            );
            assert_nothing_was_allocated(&router, &description, case, before).await;
        } else {
            let arn = text(&got, "/Arn", case);
            let listed = listed_applications(&router).await;
            assert!(
                listed
                    .iter()
                    .any(|(named, listed_arn)| named == &description && listed_arn == &arn),
                "case {case:?}: expected {arn} among the listed applications, got {listed:?}"
            );
        }
    }
}

/// RULE: the content location is reported as the prefix it names — the submitted value
/// with exactly one trailing separator — by the create and by every later read.
///
/// The rows differ along what the rule generalises over: whether the submitted value
/// already carries the separator, how many levels the prefix has, and whether it names
/// the whole bucket.
#[tokio::test]
async fn a_content_location_is_reported_as_one_prefix() {
    let router = emulator_with_content().await;

    // (case, submitted, the value every answer must report)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a prefix without its separator",
            format!("s3://{BUCKET}/build"),
            format!("s3://{BUCKET}/build/"),
        ),
        (
            "the same prefix with its separator",
            format!("s3://{BUCKET}/build/"),
            format!("s3://{BUCKET}/build/"),
        ),
        (
            "a two-level prefix without its separator",
            format!("s3://{BUCKET}/nested/deep"),
            format!("s3://{BUCKET}/nested/deep/"),
        ),
        (
            "the bucket with no key at all",
            format!("s3://{BUCKET}"),
            format!("s3://{BUCKET}/"),
        ),
        (
            "the bucket with a bare separator",
            format!("s3://{BUCKET}/"),
            format!("s3://{BUCKET}/"),
        ),
        (
            "an object named directly, reported as the location it is read as",
            format!("s3://{BUCKET}/build/start.sh"),
            format!("s3://{BUCKET}/build/start.sh/"),
        ),
    ];

    for (index, (case, submitted, expected)) in rows.iter().enumerate() {
        let description = format!("normalized-case-{index}");
        let (status, created) = create_application(&router, &description, submitted).await;
        assert_eq!(
            status, 201,
            "case {case:?} ({submitted}): expected 201, got {status} {created}"
        );
        assert_eq!(
            created["ApplicationSourceUri"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected CreateApplication to report {expected}, got {created}"
        );

        // Round trip: the stored record is what a later read reports.
        let id = text(&created, "/Id", case);
        let (status, got) = call(&router, "GET", &format!("/applications/{id}"), None).await;
        assert_eq!(
            (status, got["ApplicationSourceUri"].as_str()),
            (200, Some(expected.as_str())),
            "case {case:?}: expected GetApplication to report {expected}, got {status} {got}"
        );
    }
}
