// SPDX-License-Identifier: CC0-1.0

//! GuardDuty resolves a publishing destination's resource against Amazon S3 before it
//! records the destination.
//!
//! The rule under test: a create whose input names a resource of ANOTHER service
//! resolves that resource against the owning service before it persists anything, and
//! refuses the request when that owner reports it absent. The requirement is
//! `CreatePublishingDestination`'s own documentation — "before you start exporting the
//! findings, the destination resource must exist" — and
//! `DestinationProperties$DestinationArn` is "the ARN of the resource to publish to",
//! written `arn:aws:s3:::DOC-EXAMPLE-BUCKET/myFolder/` for a bucket folder, so the
//! resource whose existence decides the request is the BUCKET the ARN opens with. A
//! destination recorded for a bucket that is not there would publish nowhere and the
//! failure would surface much later, or never. The refusal is this service's HTTP 400
//! under the `InvalidInputException` class, in AWS's own words for that parameter.
//!
//! Only EXISTENCE is resolved: that is the half the object store can answer for.
//!
//! This suite needs TWO services on one engine — GuardDuty reaches S3 through the
//! in-process callback, so a single-service router has no S3 to ask and the bucket a
//! row names has to be creatable — which is why it lives here rather than in the
//! GuardDuty crate, exactly as `logs_export_task_destination_bucket.rs` does for
//! CloudWatch Logs. The GuardDuty crate's own suites pin the other half: with no S3 to
//! ask, the create is admitted rather than refused on an assumption.
//!
//! The rows span what the rule generalises over: whether the bucket was ever created,
//! the FOLDER the ARN names under it (none, one level, nested, with and without the
//! trailing separator), the bucket name's length (S3 documents 3..=63 characters) and
//! form (hyphens, dots, digits), a name that merely HAS an existing bucket's name as a
//! prefix — so the resolution is exact-name, not prefix, matching — and whether the
//! request also carries `expectedBucketOwner` and tags. The UPDATE path, which can
//! point an existing destination at another resource, is held to the same rule.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The signing names the router routes these requests on.
const GUARDDUTY_SIGNING_NAME: &str = "guardduty";
const S3_SIGNING_NAME: &str = "s3";

/// The only member of the `DestinationType` enum
/// (`smithy_query(shape="DestinationType")`).
const S3_DESTINATION: &str = "S3";

/// A usable KMS key ARN, the form `DestinationProperties.KmsKeyArn` carries.
const KEY_ARN: &str = "arn:aws:kms:us-east-1:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// The wording the refusal carries, written out here rather than read from the
/// service, so a change to the implementation's own constant makes this fail.
const RESOURCE_ABSENT_MESSAGE: &str =
    "The request failed because the resource specified in the destinationArn parameter does not exist.";

/// The error CLASS the refusal carries in the body's `__type` and `type`.
const INVALID_INPUT: &str = "InvalidInputException";

/// The emulator with the two services this suite exercises, mounted on ONE engine
/// exactly as `core/router/src/lib.rs` mounts them.
fn build_emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential
    // scope is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_guardduty_guarddutyapiservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// The SigV4 header a request carries, naming the signing name the router routes on.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// One GuardDuty restJson1 call: its HTTP status and parsed body.
async fn guardduty(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the request serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header(
            "host",
            format!("{GUARDDUTY_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization(GUARDDUTY_SIGNING_NAME))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `PUT /<bucket>` — S3's `CreateBucket`. This suite signs for us-east-1, whose
/// location constraint is the unspecified one, so the create carries no body.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("{S3_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization(S3_SIGNING_NAME))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// `DELETE /<bucket>` — S3's `DeleteBucket`, for the row whose bucket stops existing.
async fn delete_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("DELETE")
        .uri(format!("/{bucket}"))
        .header("host", format!("{S3_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(S3_SIGNING_NAME))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert!(
        status == 204 || status == 200,
        "setup: DeleteBucket({bucket:?}) must succeed, got {status} with {:?}",
        String::from_utf8_lossy(response.body())
    );
}

/// Enable GuardDuty and return the detector's id.
async fn detector(router: &MultiServiceRouter) -> String {
    let (status, created) = guardduty(router, "POST", "/detector", Some(json!({"enable": true})))
        .await;
    assert_eq!(
        status, 200,
        "setup: CreateDetector must succeed, got {status} with {created}"
    );
    created["detectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateDetector answers a detectorId, got {created}"))
        .to_string()
}

/// The destination ids the detector holds, as `ListPublishingDestinations` reports
/// them (`GET /detector/{DetectorId}/publishingDestination` —
/// `smithy_query(operation="ListPublishingDestinations")`).
async fn listed_destinations(router: &MultiServiceRouter, detector_id: &str) -> Vec<String> {
    let (status, page) = guardduty(
        router,
        "GET",
        &format!("/detector/{detector_id}/publishingDestination"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "ListPublishingDestinations must succeed, got {status} with {page}"
    );
    page["destinations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|destination| destination["destinationId"].as_str().map(str::to_string))
        .collect()
}

/// The `destinationArn` a destination reports back.
async fn described_arn(router: &MultiServiceRouter, detector_id: &str, id: &str) -> String {
    let (status, described) = guardduty(
        router,
        "GET",
        &format!("/detector/{detector_id}/publishingDestination/{id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribePublishingDestination({id}) must succeed, got {status} with {described}"
    );
    described["destinationProperties"]["destinationArn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One row: the bucket it names, whether that bucket is created first, the ARN the
/// request carries, the optional members beside it, and whether the create must be
/// admitted.
struct Row {
    case: &'static str,
    create: bool,
    bucket: String,
    /// The ARN's resource field: the bucket, plus any folder under it.
    resource: String,
    expected_bucket_owner: bool,
    tagged: bool,
    accepted: bool,
}

/// A 63-character bucket name — the longest S3 documents ("Bucket names must be
/// between 3 (min) and 63 (max) characters long", S3 bucket naming rules).
fn longest_bucket_name() -> String {
    let name = format!("guardduty-publish-gate-{}", "a".repeat(40));
    assert_eq!(name.len(), 63, "the longest-name row must be 63 characters");
    name
}

fn rows() -> Vec<Row> {
    let existing = "guardduty-publish-gate-plain";
    let longest = longest_bucket_name();
    vec![
        Row {
            case: "a bucket that exists",
            create: true,
            bucket: existing.to_string(),
            resource: existing.to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: true,
        },
        Row {
            case: "a bucket that was never created",
            create: false,
            bucket: "guardduty-publish-gate-ghost".to_string(),
            resource: "guardduty-publish-gate-ghost".to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: false,
        },
        Row {
            // The documented folder spelling, on a bucket that exists: the folder is
            // not a resource of its own, so what is resolved is still the bucket.
            case: "a folder under a bucket that exists",
            create: true,
            bucket: "guardduty-publish-gate-folder".to_string(),
            resource: "guardduty-publish-gate-folder/myFolder/".to_string(),
            expected_bucket_owner: true,
            tagged: false,
            accepted: true,
        },
        Row {
            case: "a nested folder with no trailing separator, on a bucket that exists",
            create: true,
            bucket: "guardduty-publish-gate-nested".to_string(),
            resource: "guardduty-publish-gate-nested/one/two".to_string(),
            expected_bucket_owner: false,
            tagged: true,
            accepted: true,
        },
        Row {
            case: "a folder under a bucket that was never created",
            create: false,
            bucket: "guardduty-publish-gate-folder-ghost".to_string(),
            resource: "guardduty-publish-gate-folder-ghost/myFolder/".to_string(),
            expected_bucket_owner: true,
            tagged: true,
            accepted: false,
        },
        Row {
            // The shortest name S3's naming rules allow.
            case: "the shortest bucket name, created",
            create: true,
            bucket: "gpc".to_string(),
            resource: "gpc".to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: true,
        },
        Row {
            case: "the shortest bucket name, never created",
            create: false,
            bucket: "gpd".to_string(),
            resource: "gpd".to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: false,
        },
        Row {
            case: "the longest bucket name, created",
            create: true,
            bucket: longest.clone(),
            resource: format!("{longest}/exported/"),
            expected_bucket_owner: false,
            tagged: false,
            accepted: true,
        },
        Row {
            case: "a dotted, digit-bearing bucket name, created",
            create: true,
            bucket: "guardduty.publish.7.dots".to_string(),
            resource: "guardduty.publish.7.dots".to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: true,
        },
        Row {
            case: "a dotted bucket name that was never created",
            create: false,
            bucket: "guardduty.publish.9.dots".to_string(),
            resource: "guardduty.publish.9.dots".to_string(),
            expected_bucket_owner: false,
            tagged: false,
            accepted: false,
        },
        Row {
            // Resolution is exact-name: a name that merely STARTS WITH an existing
            // bucket's name is a different bucket.
            case: "a name that only has an existing bucket's name as its prefix",
            create: false,
            bucket: format!("{existing}-extra"),
            resource: format!("{existing}-extra"),
            expected_bucket_owner: false,
            tagged: false,
            accepted: false,
        },
    ]
}

#[tokio::test]
async fn a_publishing_destination_is_recorded_only_for_a_resource_s3_holds() {
    let router = build_emulator();
    let detector_id = detector(&router).await;
    let account_id = engine::emulator_account_id().to_string();

    let mut recorded: Vec<String> = Vec::new();
    for row in rows() {
        if row.create {
            create_bucket(&router, &row.bucket).await;
        }
        let arn = format!("arn:aws:s3:::{}", row.resource);
        let mut properties = json!({"destinationArn": arn, "kmsKeyArn": KEY_ARN});
        if row.expected_bucket_owner {
            properties["expectedBucketOwner"] = json!(account_id);
        }
        let mut request = json!({
            "destinationType": S3_DESTINATION,
            "destinationProperties": properties,
        });
        if row.tagged {
            request["tags"] = json!({"team": "secops"});
        }

        let (status, body) = guardduty(
            &router,
            "POST",
            &format!("/detector/{detector_id}/publishingDestination"),
            Some(request),
        )
        .await;
        if row.accepted {
            assert_eq!(
                status, 200,
                "case {:?}: a resource S3 holds must be accepted, got {status} with {body}",
                row.case
            );
            let id = body["destinationId"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: an accepted create answers a destinationId, got {body}",
                        row.case
                    )
                })
                .to_string();
            // Round trip: the destination reads back through the describe, carrying the
            // ARN it was created for, and the listing names it.
            assert_eq!(
                described_arn(&router, &detector_id, &id).await,
                arn,
                "case {:?}: the describe must report the ARN the create accepted",
                row.case
            );
            recorded.push(id);
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, INVALID_INPUT),
                "case {:?}: a resource S3 does not hold must be refused 400 {INVALID_INPUT}, \
                 got {status} with {body}",
                row.case
            );
            assert_eq!(
                body["message"].as_str().unwrap_or_default(),
                RESOURCE_ABSENT_MESSAGE,
                "case {:?}: expected the absent-resource refusal, got {body}",
                row.case
            );
        }

        // Whatever the row, the store holds exactly the destinations the accepted rows
        // recorded: a refused create records nothing.
        let mut listed = listed_destinations(&router, &detector_id).await;
        listed.sort();
        let mut expected = recorded.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {:?}: the detector must hold exactly the accepted destinations",
            row.case
        );
    }
}

#[tokio::test]
async fn the_same_request_is_decided_by_the_buckets_existence_alone() {
    let router = build_emulator();
    let detector_id = detector(&router).await;

    // The identical request, sent twice around one bucket's creation: refused first,
    // accepted after — so what decides is the resource's existence and nothing about
    // the request itself.
    let bucket = "guardduty-publish-gate-created-later";
    let request = json!({
        "destinationType": S3_DESTINATION,
        "destinationProperties": {
            "destinationArn": format!("arn:aws:s3:::{bucket}/findings/"),
            "kmsKeyArn": KEY_ARN,
        },
    });
    let path = format!("/detector/{detector_id}/publishingDestination");

    let (status, body) = guardduty(&router, "POST", &path, Some(request.clone())).await;
    assert_eq!(
        (status, body["message"].as_str().unwrap_or_default()),
        (400, RESOURCE_ABSENT_MESSAGE),
        "before the bucket exists the create must be refused, got {status} with {body}"
    );

    create_bucket(&router, bucket).await;

    let (status, body) = guardduty(&router, "POST", &path, Some(request.clone())).await;
    assert_eq!(
        status, 200,
        "once the bucket exists the identical create must succeed, got {status} with {body}"
    );
    let id = body["destinationId"].as_str().unwrap_or_default().to_string();
    assert!(
        listed_destinations(&router, &detector_id).await.contains(&id),
        "the accepted create must be reported by the listing"
    );

    // A bucket that STOPS existing is in the same position as one that never did: the
    // next create naming it is refused, and the destination already recorded for it
    // keeps reporting the ARN it was created with (nothing re-resolves it).
    delete_bucket(&router, bucket).await;
    let (status, body) = guardduty(&router, "POST", &path, Some(request)).await;
    assert_eq!(
        (status, body["message"].as_str().unwrap_or_default()),
        (400, RESOURCE_ABSENT_MESSAGE),
        "once the bucket is gone the identical create must be refused again, got {status} \
         with {body}"
    );
    assert_eq!(
        described_arn(&router, &detector_id, &id).await,
        format!("arn:aws:s3:::{bucket}/findings/"),
        "the destination already recorded keeps the ARN its own create accepted"
    );
}

#[tokio::test]
async fn an_update_is_held_to_the_same_rule_as_the_create() {
    let router = build_emulator();
    let detector_id = detector(&router).await;
    let home = "guardduty-publish-gate-update-home";
    let moved_to = "guardduty-publish-gate-update-target";
    create_bucket(&router, home).await;
    create_bucket(&router, moved_to).await;

    let (status, created) = guardduty(
        &router,
        "POST",
        &format!("/detector/{detector_id}/publishingDestination"),
        Some(json!({
            "destinationType": S3_DESTINATION,
            "destinationProperties": {
                "destinationArn": format!("arn:aws:s3:::{home}"),
                "kmsKeyArn": KEY_ARN,
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the create must succeed, got {status} with {created}"
    );
    let id = created["destinationId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a destinationId, got {created}"))
        .to_string();
    let path = format!("/detector/{detector_id}/publishingDestination/{id}");

    // An update that points the destination at a bucket nothing created is refused and
    // leaves the stored ARN where it was.
    let (status, body) = guardduty(
        &router,
        "POST",
        &path,
        Some(json!({"destinationProperties": {
            "destinationArn": "arn:aws:s3:::guardduty-publish-gate-update-ghost"
        }})),
    )
    .await;
    assert_eq!(
        (status, body["message"].as_str().unwrap_or_default()),
        (400, RESOURCE_ABSENT_MESSAGE),
        "an update onto a resource S3 does not hold must be refused, got {status} with {body}"
    );
    assert_eq!(
        described_arn(&router, &detector_id, &id).await,
        format!("arn:aws:s3:::{home}"),
        "a refused update must leave the stored ARN alone"
    );

    // The same update onto a bucket S3 holds is written.
    let (status, body) = guardduty(
        &router,
        "POST",
        &path,
        Some(json!({"destinationProperties": {
            "destinationArn": format!("arn:aws:s3:::{moved_to}/exported/")
        }})),
    )
    .await;
    assert_eq!(
        status, 200,
        "an update onto a resource S3 holds must succeed, got {status} with {body}"
    );
    assert_eq!(
        described_arn(&router, &detector_id, &id).await,
        format!("arn:aws:s3:::{moved_to}/exported/"),
        "the accepted update must be what the describe reports"
    );
}

/// The error CLASS a refusal body carries (`__type`, which may carry a namespace).
fn error_code(body: &Value) -> String {
    let raw = body["__type"].as_str().unwrap_or_default();
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}
