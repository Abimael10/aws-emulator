// SPDX-License-Identifier: CC0-1.0

//! The WORDING of six classes of ECR refusal, end to end over the wire.
//!
//! Each refusal below already carries the right HTTP status (400) and the right error
//! code; what these suites pin is the sentence. The rule under test: a refusal's message
//! is the service's OWN wording for that error class — its fixed text exactly, with only
//! the request's and the registry's values filled into the slots the class has — never
//! prose this emulator invented, and never a request value in a slot the class does not
//! have. One suite per class:
//!
//! 1. a refusal about ONE UPLOAD states the upload id, the repository it was opened
//!    against and the registry, then the verdict;
//! 2. a resource HELD INSIDE a repository that the registry does not hold is reported
//!    `within` that repository, whatever kind of resource it is — and for an IMAGE the
//!    `imageId` it states is the entry the request sent, both members spelled out and an
//!    absent one as `null`;
//! 3. a continuation token this listing never minted is stated in the
//!    parameter-and-constraint form, naming the parameter's API label, and does not echo
//!    the token;
//! 4. a repository policy that would lock out later policies states the warning and the
//!    remediation, with no value from the request;
//! 5. a repository filter outside its expression quotes the expression the service
//!    polices for that value class;
//! 6. a replication destination that IS the source registry states the constraint, with
//!    no value from the request.
//!
//! Class 2 is carried by three suites: the frame itself, the `imageId` echo in a repository
//! holding no image, and the same echo in one holding stored, tagged images, where an
//! identifier read off the store would differ from the request's entry.
//!
//! Each suite is a TABLE whose rows differ along what its rule generalises over — the
//! verdict and the operation, the kind of resource and how it was addressed, the shape of
//! the token and which listing was asked, the statement that locks the policy out, where
//! the filter sits and which member carries it, where the destination sits and which
//! Region the call was made against — and the instance each cause was reported against is
//! ONE row, marked as such. Every case goes through the router, so each exercises routing,
//! the boundary validator, the handler, the persisted state and serialization.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::image_storage_class_identification::given_tagged_image;
use crate::wire::{
    at, call, call_in_region, emulator, given_layer, layer_digest, ok, refused, REGION, REGISTRY_ID,
};

/// The error code ECR states a refused parameter with.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// An upload id of `UploadId`'s modelled shape
/// (`^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$`) that
/// this registry never issued, so it passes the boundary and resolves to nothing.
const UNISSUED_UPLOAD_ID: &str = "3b9f1c7a-6d42-4e18-8b05-7c2a9f4d6e13";

/// A well-formed `LayerDigest` nothing was ever pushed under.
const UNSTORED_DIGEST: &str =
    "sha256:1c7f3e5a9b2d48061f3a7c5e9d2b4860af13c7e592b4d80617a3c5e9d2b48061";

/// How a member of a rendered identifier reads when the request carried none.
const UNSET_MEMBER: &str = "null";

/// A second well-formed digest nothing was pushed under, so the digest a refusal states
/// cannot be one value this suite happens to use everywhere.
const OTHER_UNSTORED_DIGEST: &str =
    "sha256:9e4d2c6b8a0f13577e4d2c6b8a0f1357ce9b4d2a6f80135779e4d2c6b8a0f135";

// ---------------------------------------------------------------------------
// The sentences under test, written out literally — the point of these suites is that the
// service builds exactly these, so computing one from the implementation would assert
// nothing.
// ---------------------------------------------------------------------------

/// The frame every refusal about one upload is stated in, with the verdict of its class.
fn upload_sentence(upload_id: &str, repository: &str, verdict: &str) -> String {
    format!(
        "The upload with id '{upload_id}' in the repository with name '{repository}' in \
         registry with id '{REGISTRY_ID}' {verdict}"
    )
}

/// The frame a resource held inside a repository is reported missing in.
fn not_within_sentence(subject: &str, repository: &str) -> String {
    format!(
        "The {subject} does not exist within the repository with name '{repository}' in \
         the registry with id '{REGISTRY_ID}'"
    )
}

/// The parameter-and-constraint clause: the parameter at fault and the constraint it
/// failed.
fn clause(parameter: &str, constraint: &str) -> String {
    format!("Invalid parameter at '{parameter}' failed to satisfy constraint: '{constraint}'")
}

/// A repository, created through the operation that creates one.
async fn given_repository(router: &MultiServiceRouter, case: &str, name: &str) {
    ok(
        router,
        case,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
}

/// An upload opened against `repository`, answering the id the registry issued.
async fn given_upload(router: &MultiServiceRouter, case: &str, repository: &str) -> String {
    let opened = ok(
        router,
        case,
        "InitiateLayerUpload",
        &json!({"repositoryName": repository}),
    )
    .await;
    opened["uploadId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: InitiateLayerUpload must hand back an uploadId"))
        .to_string()
}

/// Assert the refusal `operation` answers `request` with: HTTP 400, `code`, and exactly
/// `expected` as its message.
async fn assert_refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    code: &str,
    expected: &str,
) {
    let body = refused(router, case, operation, request, code).await;
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?} from {operation}, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// 1. A refusal about one upload
// ---------------------------------------------------------------------------

/// Which upload a row addresses, and therefore which verdict it earns.
enum Addressed {
    /// An upload this registry issued that accepted no part.
    WithoutParts,
    /// An id of the modelled shape this registry never issued.
    Unissued,
    /// An id this registry issued against ANOTHER repository.
    OpenedElsewhere,
}

/// Every refusal about one upload names the upload id, the repository it was opened
/// against and the registry, and then states its verdict.
///
/// The rows differ along what the rule generalises over: the verdict (an upload with no
/// part, an id that resolves to no upload), how the id fails to resolve (never issued,
/// issued for another repository), the operation asked, and the shape of the repository
/// name (plain, and one carrying a namespace separator). The reported instance is the
/// first row.
#[tokio::test]
async fn an_upload_refusal_names_the_upload_the_repository_and_the_registry() {
    let router = emulator();

    // (case, repository, which upload the row addresses, operation, error code, verdict)
    let rows: Vec<(&str, &str, Addressed, &str, &str, &str)> = vec![
        (
            // The reported instance.
            "an upload that accepted no part",
            "upload-frame-plain",
            Addressed::WithoutParts,
            "CompleteLayerUpload",
            "EmptyUploadException",
            "cannot be completed because it does not have any parts",
        ),
        (
            "an upload that accepted no part, under a namespaced repository",
            "team/upload-frame",
            Addressed::WithoutParts,
            "CompleteLayerUpload",
            "EmptyUploadException",
            "cannot be completed because it does not have any parts",
        ),
        (
            "an id this registry never issued",
            "upload-frame-unissued",
            Addressed::Unissued,
            "CompleteLayerUpload",
            "UploadNotFoundException",
            "does not exist",
        ),
        (
            "an id opened against another repository",
            "upload-frame-elsewhere",
            Addressed::OpenedElsewhere,
            "CompleteLayerUpload",
            "UploadNotFoundException",
            "does not exist",
        ),
        (
            "an id this registry never issued, on the part upload",
            "upload-frame-part",
            Addressed::Unissued,
            "UploadLayerPart",
            "UploadNotFoundException",
            "does not exist",
        ),
    ];

    // One repository every `OpenedElsewhere` row borrows an upload from.
    given_repository(&router, "the other repository", "upload-frame-other").await;
    let elsewhere = given_upload(&router, "the other repository", "upload-frame-other").await;

    for (case, repository, addressed, operation, code, verdict) in rows {
        given_repository(&router, case, repository).await;
        let upload_id = match addressed {
            Addressed::WithoutParts => given_upload(&router, case, repository).await,
            Addressed::Unissued => UNISSUED_UPLOAD_ID.to_string(),
            Addressed::OpenedElsewhere => elsewhere.clone(),
        };
        let request = match operation {
            "UploadLayerPart" => json!({
                "repositoryName": repository,
                "uploadId": upload_id,
                "partFirstByte": 0,
                "partLastByte": 3,
                "layerPartBlob": aws_smithy_types::base64::encode(&b"abcd"[..]),
            }),
            _ => json!({
                "repositoryName": repository,
                "uploadId": upload_id,
                "layerDigests": [layer_digest(b"abcd")],
            }),
        };
        let expected = upload_sentence(&upload_id, repository, verdict);
        assert_refusal(&router, case, operation, &request, code, &expected).await;
    }
}

// ---------------------------------------------------------------------------
// 2. A resource held inside a repository
// ---------------------------------------------------------------------------

/// A resource the registry does not hold inside the repository the request addressed is
/// reported `within` that repository, whichever kind of resource it is.
///
/// The rows differ along what the rule generalises over: the kind of resource (a layer, an
/// image) and so the operation asked, how it was addressed (a digest, a tag, both), and
/// whether the resource exists elsewhere in the registry. The reported instance is the
/// first row.
///
/// What an IMAGE refusal names the identifier as is the next suite's rule: the entry the
/// request sent.
#[tokio::test]
async fn a_resource_missing_inside_a_repository_is_reported_within_it() {
    let router = emulator();
    given_repository(&router, "the addressed repository", "within-mine").await;
    given_repository(&router, "the other repository", "within-other").await;
    // A layer the OTHER repository holds, so a digest that exists in the registry but not
    // in the repository the request names is still reported as missing within it.
    let elsewhere = given_layer(&router, "the other repository", "within-other", b"held").await;

    // (case, operation, request, error code, the subject the sentence names)
    let rows: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            // The reported instance.
            "a layer digest nothing pushed",
            "GetDownloadUrlForLayer",
            json!({"repositoryName": "within-mine", "layerDigest": UNSTORED_DIGEST}),
            "LayersNotFoundException",
            format!("layer with digest '{UNSTORED_DIGEST}'"),
        ),
        (
            "a layer digest another repository holds",
            "GetDownloadUrlForLayer",
            json!({"repositoryName": "within-mine", "layerDigest": elsewhere.clone()}),
            "LayersNotFoundException",
            format!("layer with digest '{elsewhere}'"),
        ),
        (
            "an image addressed by a digest nothing pushed",
            "DescribeImages",
            json!({
                "repositoryName": "within-mine",
                "imageIds": [{"imageDigest": UNSTORED_DIGEST}],
            }),
            "ImageNotFoundException",
            format!("image with imageId {{imageDigest:'{UNSTORED_DIGEST}', imageTag:'null'}}"),
        ),
        (
            "an image addressed by a tag nothing carries",
            "DescribeImages",
            json!({"repositoryName": "within-mine", "imageIds": [{"imageTag": "absent"}]}),
            "ImageNotFoundException",
            format!("image with imageId {{imageDigest:'{UNSET_MEMBER}', imageTag:'absent'}}"),
        ),
        (
            "an image addressed by both",
            "DescribeImages",
            json!({
                "repositoryName": "within-mine",
                "imageIds": [{"imageDigest": UNSTORED_DIGEST, "imageTag": "absent"}],
            }),
            "ImageNotFoundException",
            format!("image with imageId {{imageDigest:'{UNSTORED_DIGEST}', imageTag:'absent'}}"),
        ),
    ];

    for (case, operation, request, code, subject) in rows {
        let expected = not_within_sentence(&subject, "within-mine");
        assert_refusal(&router, case, operation, &request, code, &expected).await;
    }
}

/// Every operation that refuses an identifier naming no stored image, with the request it
/// takes for the identifier `id`.
///
/// The parameter is `imageId` everywhere but `DescribeImages`, whose parameter is the LIST
/// `imageIds`, and `UpdateImageStorageClass` also carries the storage class it would move
/// the image to (`smithy_query(query="operation",
/// operation="DescribeImages,DescribeImageScanFindings,StartImageScan,DescribeImageReplicationStatus,DescribeImageSigningStatus,UpdateImageStorageClass")`).
fn image_lookups(repository: &str, id: &Value) -> Vec<(&'static str, Value)> {
    vec![
        (
            "DescribeImages",
            json!({"repositoryName": repository, "imageIds": [id.clone()]}),
        ),
        (
            "DescribeImageScanFindings",
            json!({"repositoryName": repository, "imageId": id.clone()}),
        ),
        (
            "DescribeImageReplicationStatus",
            json!({"repositoryName": repository, "imageId": id.clone()}),
        ),
        (
            "DescribeImageSigningStatus",
            json!({"repositoryName": repository, "imageId": id.clone()}),
        ),
        (
            "StartImageScan",
            json!({"repositoryName": repository, "imageId": id.clone()}),
        ),
        (
            "UpdateImageStorageClass",
            json!({
                "repositoryName": repository,
                "imageId": id.clone(),
                "targetStorageClass": "ARCHIVE",
            }),
        ),
    ]
}

/// The `imageId` an image-not-found refusal states is the entry the REQUEST sent, with
/// both members spelled out and a member the request did not carry rendered as `null`: a
/// refusal addresses the image the way the call named it, and a failed resolution has no
/// stored identifier to report instead.
///
/// The rows differ along what the rule generalises over: which members the identifier
/// carries (a tag alone, a digest alone, both), the shape of the tag (the shortest and the
/// longest `ImageTag`'s `@length(1..300)` admits, and one carrying separators), and the
/// digest's own value — crossed with every operation that resolves an identifier, because
/// the rule belongs to the resolution rather than to one operation. The reported instance
/// is a tag-only lookup on `DescribeImages`.
#[tokio::test]
async fn an_image_refusal_states_the_identifier_the_request_sent() {
    // `ImageTag` is `@length(1..300)` (`smithy_query(query="shape", shape="ImageTag")`),
    // so a tag of one character and one of 300 are both well formed and reach the lookup.
    let longest_tag = "t".repeat(300);

    // (case, the identifier sent, the digest the refusal must state, the tag it must state)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        // The reported instance: a tag-only lookup in a repository holding no image.
        (
            "a tag nothing carries",
            json!({"imageTag": "latest"}),
            UNSET_MEMBER,
            "latest",
        ),
        (
            "the shortest tag the model admits",
            json!({"imageTag": "a"}),
            UNSET_MEMBER,
            "a",
        ),
        (
            "the longest tag the model admits",
            json!({"imageTag": longest_tag.clone()}),
            UNSET_MEMBER,
            longest_tag.as_str(),
        ),
        (
            "a tag carrying separators",
            json!({"imageTag": "release-1.2_3"}),
            UNSET_MEMBER,
            "release-1.2_3",
        ),
        (
            "a digest nothing was pushed under",
            json!({"imageDigest": UNSTORED_DIGEST}),
            UNSTORED_DIGEST,
            UNSET_MEMBER,
        ),
        (
            "another digest nothing was pushed under",
            json!({"imageDigest": OTHER_UNSTORED_DIGEST}),
            OTHER_UNSTORED_DIGEST,
            UNSET_MEMBER,
        ),
        (
            "a digest and a tag, neither of which resolves",
            json!({"imageDigest": UNSTORED_DIGEST, "imageTag": "absent"}),
            UNSTORED_DIGEST,
            "absent",
        ),
    ];

    let router = emulator();
    given_repository(&router, "the empty repository", "resolution-empty").await;
    for (case, id, digest, tag) in rows {
        for (operation, request) in image_lookups("resolution-empty", &id) {
            let case = format!("{case} on {operation}");
            let expected = not_within_sentence(
                &format!("image with imageId {{imageDigest:'{digest}', imageTag:'{tag}'}}"),
                "resolution-empty",
            );
            assert_refusal(
                &router,
                &case,
                operation,
                &request,
                "ImageNotFoundException",
                &expected,
            )
            .await;
        }
    }
}

/// The same rule holds for an identifier that STOPPED resolving: a repository that did
/// hold the image refuses the identifier the request sends, so the echo is the request's
/// entry and not something the repository's history left behind.
///
/// Driven end to end through the operations that change what resolves: `PutImage` pushes a
/// manifest under two tags, `BatchDeleteImage` frees one tag and then the manifest itself,
/// and each lookup in between is crossed with every operation that resolves an identifier
/// ([`image_lookups`]), because the rule belongs to the resolution rather than to one
/// operation. The rows differ along how the identifier stopped resolving (a freed tag, a
/// deleted digest, a tag whose digest went away) and along which members the request
/// carries.
#[tokio::test]
async fn an_identifier_that_stopped_resolving_is_still_echoed_as_sent() {
    // A manifest this suite pushes; its digest is whatever the registry computed over
    // these bytes, read back from the push rather than written out here.
    const MANIFEST: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{},"layers":[]}"#;

    let router = emulator();
    let repository = "resolution-history";
    given_repository(&router, "the repository that held the image", repository).await;

    let pushed = ok(
        &router,
        "the first tag",
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": MANIFEST,
            "imageTag": "v1",
        }),
    )
    .await;
    let digest = pushed["image"]["imageId"]["imageDigest"]
        .as_str()
        .unwrap_or_else(|| panic!("PutImage must report the digest it stored, got {pushed:?}"))
        .to_string();
    ok(
        &router,
        "a second tag on the same manifest",
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": MANIFEST,
            "imageTag": "v2",
        }),
    )
    .await;

    // Both tags resolve while the manifest is stored.
    for tag in ["v1", "v2"] {
        let described = ok(
            &router,
            tag,
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [{"imageTag": tag}]}),
        )
        .await;
        assert_eq!(
            at(&described, "/imageDetails/0/imageDigest"),
            json!(digest),
            "case {tag:?}: the pushed tag must resolve to the stored manifest, got {described:?}"
        );
    }

    // Freeing ONE tag leaves the manifest, and the other tag, in place.
    ok(
        &router,
        "freeing the first tag",
        "BatchDeleteImage",
        &json!({"repositoryName": repository, "imageIds": [{"imageTag": "v1"}]}),
    )
    .await;

    // (case, the identifier the request sends, the digest echoed, the tag echoed)
    let freed: Vec<(&str, Value, &str, &str)> = vec![
        (
            "the tag the delete freed",
            json!({"imageTag": "v1"}),
            UNSET_MEMBER,
            "v1",
        ),
        (
            "the freed tag beside the digest that still exists",
            json!({"imageDigest": digest.clone(), "imageTag": "v1"}),
            digest.as_str(),
            "v1",
        ),
    ];
    for (case, id, echoed_digest, echoed_tag) in freed {
        assert_every_lookup_echoes(&router, case, repository, &id, echoed_digest, echoed_tag)
            .await;
    }

    // Deleting the manifest itself takes its remaining tag with it, and every way of
    // addressing it is echoed as the request wrote it.
    ok(
        &router,
        "deleting the manifest",
        "BatchDeleteImage",
        &json!({"repositoryName": repository, "imageIds": [{"imageDigest": digest.clone()}]}),
    )
    .await;

    let gone: Vec<(&str, Value, &str, &str)> = vec![
        (
            "the deleted digest",
            json!({"imageDigest": digest.clone()}),
            digest.as_str(),
            UNSET_MEMBER,
        ),
        (
            "the tag the deleted manifest carried",
            json!({"imageTag": "v2"}),
            UNSET_MEMBER,
            "v2",
        ),
        (
            "the deleted digest and its tag together",
            json!({"imageDigest": digest.clone(), "imageTag": "v2"}),
            digest.as_str(),
            "v2",
        ),
    ];
    for (case, id, echoed_digest, echoed_tag) in gone {
        assert_every_lookup_echoes(&router, case, repository, &id, echoed_digest, echoed_tag)
            .await;
    }
}

/// Assert that every operation resolving an identifier refuses `id` by echoing exactly
/// `echoed_digest` and `echoed_tag`, in the sentence a resource missing inside a repository
/// is reported in.
async fn assert_every_lookup_echoes(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    id: &Value,
    echoed_digest: &str,
    echoed_tag: &str,
) {
    let expected = not_within_sentence(
        &format!("image with imageId {{imageDigest:'{echoed_digest}', imageTag:'{echoed_tag}'}}"),
        repository,
    );
    for (operation, request) in image_lookups(repository, id) {
        let case = format!("{case} on {operation}");
        assert_refusal(
            router,
            &case,
            operation,
            &request,
            "ImageNotFoundException",
            &expected,
        )
        .await;
    }
}

/// The same rule in a repository that HOLDS stored, tagged images: the identifier a refusal
/// states is still the request's entry, even where half of it resolves — a stored digest
/// whose tag nothing carries, a stored digest carrying another image's tag, an unstored
/// digest beside a tag that does resolve. These are the cases where an identifier read off
/// the store would differ from the one the request sent.
///
/// The rows differ along which member resolves and which does not, and along which of the two
/// stored images each half names, crossed with every operation that resolves an identifier.
/// The admitted side closes the table: the identifiers the stored images really carry still
/// resolve, so each refusal is about the entry its row sent.
#[tokio::test]
async fn an_image_refusal_echoes_the_request_in_a_repository_holding_images() {
    let repository = "resolution-populated";
    let router = emulator();
    given_repository(&router, "the populated repository", repository).await;
    let live = given_tagged_image(&router, "the first stored image", repository, "live", "live").await;
    let other =
        given_tagged_image(&router, "the second stored image", repository, "other", "other").await;

    // (case, the identifier sent, the digest the refusal must state, the tag it must state)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "a stored digest with a tag nothing carries",
            json!({"imageDigest": live.clone(), "imageTag": "never-pushed"}),
            live.as_str(),
            "never-pushed",
        ),
        (
            "a stored digest with another stored image's tag",
            json!({"imageDigest": live.clone(), "imageTag": "other"}),
            live.as_str(),
            "other",
        ),
        (
            "an unstored digest with a stored tag",
            json!({"imageDigest": UNSTORED_DIGEST, "imageTag": "live"}),
            UNSTORED_DIGEST,
            "live",
        ),
        (
            "the second image's digest with the first one's tag",
            json!({"imageDigest": other.clone(), "imageTag": "live"}),
            other.as_str(),
            "live",
        ),
        (
            "a tag nothing carries, in a repository that holds images",
            json!({"imageTag": "never-pushed"}),
            UNSET_MEMBER,
            "never-pushed",
        ),
    ];

    for (case, id, digest, tag) in rows {
        for (operation, request) in image_lookups(repository, &id) {
            let case = format!("{case} on {operation}");
            let expected = not_within_sentence(
                &format!("image with imageId {{imageDigest:'{digest}', imageTag:'{tag}'}}"),
                repository,
            );
            assert_refusal(
                &router,
                &case,
                operation,
                &request,
                "ImageNotFoundException",
                &expected,
            )
            .await;
        }
    }

    // The admitted side: every identifier a stored image really carries resolves to it, read
    // back through the operation that reports an image's details.
    for (case, id, digest) in [
        ("the first image by tag", json!({"imageTag": "live"}), &live),
        (
            "the first image by digest",
            json!({"imageDigest": live.clone()}),
            &live,
        ),
        (
            "the second image by digest and tag",
            json!({"imageDigest": other.clone(), "imageTag": "other"}),
            &other,
        ),
    ] {
        let answer = ok(
            &router,
            case,
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [id]}),
        )
        .await;
        let reported = answer
            .pointer("/imageDetails/0/imageDigest")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported, digest,
            "case {case:?}: expected the stored image {digest:?} to resolve, got {answer:?}"
        );
    }
}

/// The identifier a refusal echoed is the one that RESOLVES once an image carries it: the
/// same request, unchanged, is refused while nothing carries the tag and reports the stored
/// image afterwards — while another tag the push did not attach is still refused, echoed as
/// it was sent.
///
/// The rows differ along the spelling of the tag the request asks by, which the rule does not
/// depend on.
#[tokio::test]
async fn the_echoed_identifier_resolves_once_an_image_carries_it() {
    for (index, tag) in ["pending", "v1.2_3-rc", "z"].iter().enumerate() {
        let repository = format!("resolution-round-trip-{index:02}");
        let router = emulator();
        given_repository(&router, tag, &repository).await;
        let request = json!({"repositoryName": repository, "imageIds": [{"imageTag": tag}]});

        assert_refusal(
            &router,
            &format!("before the push, by {tag:?}"),
            "DescribeImages",
            &request,
            "ImageNotFoundException",
            &not_within_sentence(
                &format!("image with imageId {{imageDigest:'{UNSET_MEMBER}', imageTag:'{tag}'}}"),
                &repository,
            ),
        )
        .await;

        let digest = given_tagged_image(&router, tag, &repository, tag, tag).await;
        let answer = ok(
            &router,
            &format!("after the push, by {tag:?}"),
            "DescribeImages",
            &request,
        )
        .await;
        let reported = answer
            .pointer("/imageDetails/0/imageDigest")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            reported, digest,
            "case {tag:?}: expected the echoed identifier to resolve after the push, got \
             {answer:?}"
        );

        let absent = format!("{tag}-absent");
        assert_refusal(
            &router,
            &format!("another tag after the push, by {absent:?}"),
            "DescribeImages",
            &json!({"repositoryName": repository, "imageIds": [{"imageTag": absent}]}),
            "ImageNotFoundException",
            &not_within_sentence(
                &format!(
                    "image with imageId {{imageDigest:'{UNSET_MEMBER}', imageTag:'{absent}'}}"
                ),
                &repository,
            ),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// 3. A continuation token no listing minted
// ---------------------------------------------------------------------------
/// The constraint a token this service never minted fails: one phrase for the class,
/// stating no detail of how a token is read.
const NEXT_TOKEN_CONSTRAINT: &str = "Invalid next token provided";

/// A continuation token this service never minted is refused in the parameter-and-
/// constraint form, naming the API label of the parameter that carried it — and the
/// refusal does not echo the token.
///
/// The rows differ along what the rule generalises over: the shape of the token (characters
/// no token carries, a truncated token, a token whose bytes are not text) crossed with the
/// listings that take one, which differ in what they page over and in what else the
/// request carries. The reported instance is the first token shape on
/// `ListPullTimeUpdateExclusions`.
#[tokio::test]
async fn a_token_this_listing_never_minted_names_the_parameter_and_the_constraint() {
    let router = emulator();
    given_repository(&router, "the paged repository", "token-repo").await;

    // (case, the token sent)
    let tokens: Vec<(&str, &str)> = vec![
        // The reported instance.
        (
            "characters no token carries",
            "not-a-real-continuation-token!!",
        ),
        ("a truncated token", "616"),
        ("a token of one character", "z"),
        // Bytes that are not text, so the token is well formed for the first step of the
        // decode and still names no page.
        ("a token whose bytes are not text", "ff"),
    ];

    // (the listing, what else its request carries)
    let listings: Vec<(&str, Value)> = vec![
        ("ListPullTimeUpdateExclusions", json!({})),
        ("DescribeRepositories", json!({})),
        ("DescribePullThroughCacheRules", json!({})),
        ("DescribeRepositoryCreationTemplates", json!({})),
        ("ListImages", json!({"repositoryName": "token-repo"})),
        ("DescribeImages", json!({"repositoryName": "token-repo"})),
    ];

    let expected = clause("NextToken", NEXT_TOKEN_CONSTRAINT);
    for (shape, token) in &tokens {
        for (operation, extra) in &listings {
            let case = format!("{shape} on {operation}");
            let mut request = extra.clone();
            request["nextToken"] = json!(token);
            let body = refused(&router, &case, operation, &request, INVALID_PARAMETER).await;
            let message = body["message"].as_str().unwrap_or_default();
            assert_eq!(
                message, expected,
                "case {case:?}: expected the clause {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains(token),
                "case {case:?}: the refusal must not echo the token {token:?}, got {message:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4. A repository policy that locks out later policies
// ---------------------------------------------------------------------------

/// What ECR answers a lock-out policy submitted without `force` with: the warning and the
/// remediation, carrying no value from the request.
const LOCK_OUT_MESSAGE: &str = "You are about to set a repository policy that will \
     prevent you from setting another one in the future. Use the force parameter to \
     override this exception.";

/// A policy that would prevent another policy from being set, submitted without `force`,
/// is refused with the remediation sentence — the same sentence whichever repository and
/// whichever statement it was, and with neither of them in it.
///
/// The rows differ along what the rule generalises over: how the statement covers the
/// action (the action itself, a wildcard prefix, every action, a `NotAction` that leaves
/// it in), the form the wildcard principal takes, and the repository name. The reported
/// instance is the first row.
#[tokio::test]
async fn a_lock_out_policy_without_force_states_the_remediation_and_no_request_value() {
    let router = emulator();

    // (case, repository, the policy document)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            // The reported instance.
            "deny every action to every principal",
            "lock-out-every-action",
            json!({"Statement": [{"Effect": "Deny", "Principal": "*", "Action": "ecr:*"}]}),
        ),
        (
            "deny the action itself to every principal",
            "lock-out-the-action",
            json!({"Statement": [
                {"Effect": "Deny", "Principal": "*", "Action": "ecr:SetRepositoryPolicy"},
            ]}),
        ),
        (
            "deny a wildcard prefix covering the action",
            "team/lock-out-prefix",
            json!({"Statement": [
                {"Effect": "Deny", "Principal": {"AWS": "*"}, "Action": ["ecr:Set*"]},
            ]}),
        ),
        (
            "deny everything except another action",
            "lock-out-notaction",
            json!({"Statement": [
                {"Effect": "Deny", "Principal": {"AWS": ["*"]}, "NotAction": "ecr:BatchGetImage"},
            ]}),
        ),
    ];

    for (case, repository, policy) in rows {
        given_repository(&router, case, repository).await;
        let request = json!({"repositoryName": repository, "policyText": policy.to_string()});
        assert_refusal(
            &router,
            case,
            "SetRepositoryPolicy",
            &request,
            INVALID_PARAMETER,
            LOCK_OUT_MESSAGE,
        )
        .await;
        // The sentence has no slot for the repository, so the refusal names none.
        let body = call(&router, "SetRepositoryPolicy", &request).await;
        assert!(
            !body.message().contains(repository),
            "case {case:?}: the refusal must not name the repository, got {:?}",
            body.message()
        );
        // And nothing was stored: the repository still carries no policy.
        refused(
            &router,
            case,
            "GetRepositoryPolicy",
            &json!({"repositoryName": repository}),
            "RepositoryPolicyNotFoundException",
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// 5. A repository filter outside its expression
// ---------------------------------------------------------------------------

/// The expression the service polices a repository-name GLOB against, written out here
/// rather than imported. `smithy_query(query="shape",
/// shape="SigningRepositoryFilterValue")` declares it; `ScanningRepositoryFilterValue`
/// writes the same language with a separator CLASS instead of a group, and the service
/// states this spelling for either member.
const REPOSITORY_GLOB_EXPRESSION: &str =
    "^(?:[a-z0-9*]+(?:[._-][a-z0-9*]+)*/)*[a-z0-9*]+(?:[._-][a-z0-9*]+)*$";

/// The fragment that only appears in the OTHER spelling of the same language, which the
/// refusal must not quote.
const SEPARATOR_CLASS_FRAGMENT: &str = r"[._\-/a-z0-9*]";

/// One scanning rule: `WILDCARD` is the only `ScanningRepositoryFilterType` the model
/// declares.
fn scanning_rule(filters: &[&str]) -> Value {
    json!({
        "scanFrequency": "SCAN_ON_PUSH",
        "repositoryFilters": filters
            .iter()
            .map(|filter| json!({"filter": filter, "filterType": "WILDCARD"}))
            .collect::<Vec<Value>>(),
    })
}

/// A repository filter outside the expression is refused quoting the expression the
/// service polices for that value class, under the path the validating walk descended.
///
/// The rows differ along what the rule generalises over: where the filter sits (the first
/// rule's first filter, a later filter of one rule, a later rule's filter), the shape of
/// the refused value, and which member carries it — the scanning configuration's filter
/// and the signing configuration's, the two members this model spells the one value class
/// on. The reported instance is the second row.
#[tokio::test]
async fn a_repository_filter_quotes_the_expression_the_service_polices() {
    let router = emulator();
    let profile = format!("arn:aws:signer:{REGION}:{REGISTRY_ID}:/signing-profiles/WellFormed");

    // (case, operation, request, the path the clause names)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "the first rule's first scanning filter",
            "PutRegistryScanningConfiguration",
            json!({"scanType": "BASIC", "rules": [scanning_rule(&["Prod"])]}),
            "rules.1.member.repositoryFilters.1.member.filter",
        ),
        (
            // The reported instance: a well-formed filter beside a refused one, so a
            // position cannot be mistaken for a name.
            "the second scanning filter of one rule",
            "PutRegistryScanningConfiguration",
            json!({"scanType": "BASIC", "rules": [scanning_rule(&["prod/*", "Team/App"])]}),
            "rules.1.member.repositoryFilters.2.member.filter",
        ),
        (
            "the second rule's scanning filter",
            "PutRegistryScanningConfiguration",
            json!({
                "scanType": "ENHANCED",
                "rules": [
                    scanning_rule(&["lib.core_v2-rc"]),
                    scanning_rule(&["UPPER"]),
                ],
            }),
            "rules.2.member.repositoryFilters.1.member.filter",
        ),
        (
            "the signing configuration's filter, the model's other spelling",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {"rules": [{
                "signingProfileArn": profile,
                "repositoryFilters": [{"filter": "Team/App", "filterType": "WILDCARD_MATCH"}],
            }]}}),
            "signingConfiguration.rules.1.member.repositoryFilters.1.member.filter",
        ),
    ];

    for (case, operation, request, path) in rows {
        let expected = clause(
            path,
            &format!("Member must satisfy regular expression pattern: {REPOSITORY_GLOB_EXPRESSION}"),
        );
        assert_refusal(
            &router,
            case,
            operation,
            &request,
            INVALID_PARAMETER,
            &expected,
        )
        .await;
        let answer = call(&router, operation, &request).await;
        assert!(
            !answer.message().contains(SEPARATOR_CLASS_FRAGMENT),
            "case {case:?}: the refusal must quote the expression the service polices, \
             got {:?}",
            answer.message()
        );
    }

    // What the expression admits is unchanged by how it is quoted: a filter inside it is
    // stored and reported back.
    for (case, filter) in [
        ("a wildcard tail", "prod/*"),
        ("a single character", "x"),
        ("every separator the expression allows", "lib.core_v2-rc/a"),
    ] {
        let stored = ok(
            &router,
            case,
            "PutRegistryScanningConfiguration",
            &json!({"scanType": "BASIC", "rules": [scanning_rule(&[filter])]}),
        )
        .await;
        let reported = ok(
            &router,
            case,
            "GetRegistryScanningConfiguration",
            &json!({}),
        )
        .await;
        assert_eq!(
            stored["registryScanningConfiguration"], reported["scanningConfiguration"],
            "case {case:?}: expected the put and the read to report one configuration, \
             got {stored:?} and {reported:?}"
        );
        assert_eq!(
            reported["scanningConfiguration"]["rules"],
            json!([scanning_rule(&[filter])]),
            "case {case:?}: expected the filter {filter:?} stored, got {:?}",
            reported["scanningConfiguration"]
        );
    }
}

// ---------------------------------------------------------------------------
// 6. A replication destination that is the source registry
// ---------------------------------------------------------------------------

/// The constraint a configuration fails when a destination IS the source registry.
const SOURCE_DESTINATION_CONSTRAINT: &str =
    "Replication destination cannot be the same as the source registry";

/// A destination naming this account's registry in the Region the call was made against
/// is refused with the parameter-and-constraint clause for the configuration, and the
/// clause carries no value from the request.
///
/// The rows differ along what the rule generalises over: where the offending destination
/// sits (the only one, a later destination of one rule, a later rule's destination, beside
/// a destination that is accepted), and the Region the call was made against — the Region
/// is what makes a destination "the source", so the same configuration is refused in one
/// Region and accepted in another. The reported instance is the first row.
#[tokio::test]
async fn a_destination_that_is_the_source_registry_states_the_constraint() {
    let router = emulator();
    let own = |region: &str| json!({"region": region, "registryId": REGISTRY_ID});
    let foreign = json!({"region": "us-east-2", "registryId": "111122223333"});

    // (case, the Region called, the rules sent)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            // The reported instance.
            "the only destination",
            REGION,
            json!([{"destinations": [own(REGION)]}]),
        ),
        (
            "the second destination of one rule",
            REGION,
            json!([{"destinations": [foreign.clone(), own(REGION)]}]),
        ),
        (
            "the second rule's destination",
            REGION,
            json!([
                {"destinations": [foreign.clone()]},
                {"destinations": [own(REGION)]},
            ]),
        ),
        (
            "the source registry in another Region the call was made against",
            "us-west-2",
            json!([{"destinations": [own("us-west-2")]}]),
        ),
    ];

    let expected = clause("replicationConfiguration", SOURCE_DESTINATION_CONSTRAINT);
    for (case, region, rules) in rows {
        let answer = call_in_region(
            &router,
            region,
            "PutReplicationConfiguration",
            &json!({"replicationConfiguration": {"rules": rules}}),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected the clause {expected:?}, got {:?}",
            answer.message()
        );
        // The clause has no slot for a registry, a Region or a rule position.
        for value in [REGISTRY_ID, region] {
            assert!(
                !answer.message().contains(value),
                "case {case:?}: the clause must not carry {value:?}, got {:?}",
                answer.message()
            );
        }
        // A refused put stores nothing.
        let described = ok(&router, case, "DescribeRegistry", &json!({})).await;
        assert_eq!(
            described["replicationConfiguration"],
            json!({"rules": []}),
            "case {case:?}: a refused put must store nothing, got {described:?}"
        );
    }

    // The same destination is a real destination from ANOTHER Region: the registry it
    // names is not the source there, so the put is accepted and reported back.
    let accepted = json!([{"destinations": [own(REGION)]}]);
    let answer = call_in_region(
        &router,
        "us-west-2",
        "PutReplicationConfiguration",
        &json!({"replicationConfiguration": {"rules": accepted}}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "a destination in another Region is a destination, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.body["replicationConfiguration"],
        json!({"rules": accepted}),
        "the accepted configuration is reported back, got {:?}",
        answer.body
    );
}
