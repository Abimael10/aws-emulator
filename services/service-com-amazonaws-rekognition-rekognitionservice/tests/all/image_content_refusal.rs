// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a request whose `Image` identifies NO content, end to end over
//! the wire (`awsJson1_1`, `POST /` with `X-Amz-Target: RekognitionService.<Operation>`),
//! dispatched through the same [`MultiServiceRouter`] the production emulator mounts.
//!
//! THE RULE UNDER TEST: an image operation whose `Image` structure does not supply EXACTLY
//! ONE of its two alternative content members (`Bytes`, `S3Object` —
//! `smithy_query(query="shape",shape="Image")`, which marks both optional, so the request
//! reaches the handler, and whose own documentation provides the image "either as bytes or an
//! S3 object") is refused with HTTP 400 `InvalidParameterException`, named in the
//! envelope's `__type` and in the body's own `Code`, carrying the fixed wording the service
//! words that class with — one wording for the image that names NEITHER and one for the
//! image that names BOTH. Each interpolates nothing — no request value and no member name —
//! so it is byte-for-byte the same message whichever operation was called and whatever else
//! the request carried.
//!
//! The table spans what the rule generalises over: which image operation is called
//! (`DetectFaces`, `DetectLabels`, `IndexFaces`, `RecognizeCelebrities` — the operations of
//! this service that take ONE `Image` and are implemented; `CompareFaces`, which takes two,
//! is covered the same way in the comparison suite) and what else the request carries beside
//! the image (nothing, and each operation's own optional members at several values). A
//! contrast table pins the other side of the rule: an image that DOES identify exactly one
//! source is never answered with either refusal, whether that content turns out to be
//! readable, to violate the model's own constraint on it, to be undecodable, or to be an S3
//! object the store does not serve.

use std::io::Cursor;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The router harness, the dispatch helper and the reduced reply all come from the
// collection-lifecycle suite rather than being built a second time here.
use crate::face_collections::{bytes_image, call, harness, Reply};

/// The wording AWS answers an `Image` with no content source with. Fixed text for the
/// class: no part of it comes from the request or from this emulator's state.
const MISSING_CONTENT_MESSAGE: &str = "Requested image should either contain bytes or s3 object.";

/// The wording AWS answers an `Image` naming BOTH content sources with — the other half of
/// the same rule, and fixed text for its class in the same way.
const CONFLICTING_CONTENT_MESSAGE: &str = "Cannot provide both S3 Location and Image Bytes.";

/// The collection the `IndexFaces` rows name, so the image is what each of them gets
/// refused for rather than the collection being absent.
const COLLECTION: &str = "image-content-refusal";

/// An `Image` carrying no content source: neither of the two members the shape models.
fn contentless_image() -> Value {
    json!({})
}

/// Inline content, the way an SDK passes `Image.Bytes` on a JSON protocol: base64 of the
/// bytes.
fn inline_bytes(content: &[u8]) -> Value {
    Value::String(aws_smithy_types::base64::encode(content))
}

/// A small, real PNG: content that IS identified and readable.
fn readable_png() -> Vec<u8> {
    let flat = image::GrayImage::from_pixel(16, 16, image::Luma([200u8]));
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(flat)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode the control image");
    encoded.into_inner()
}

/// What a contentless image must be answered with, wherever it was submitted.
fn assert_missing_content(case: &str, reply: &Reply) {
    assert_invalid_parameter(case, reply, MISSING_CONTENT_MESSAGE);
}

/// What an `Image` that does not identify exactly one source must be answered with.
fn assert_invalid_parameter(case: &str, reply: &Reply, message: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        "InvalidParameterException",
        "case {case:?}: expected __type InvalidParameterException, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Code").as_deref(),
        Some("InvalidParameterException"),
        "case {case:?}: expected Code InvalidParameterException, got {:?}",
        reply.body
    );
    assert_eq!(
        reply.string("Message").as_deref(),
        Some(message),
        "case {case:?}: expected Message {message:?}, got {:?}",
        reply.body
    );
}

/// A router holding the collection the `IndexFaces` rows index into.
async fn seeded() -> MultiServiceRouter {
    let router = harness();
    let created = call(
        &router,
        "CreateCollection",
        json!({ "CollectionId": COLLECTION }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the seeded collection must be created, got {:?}",
        created.body
    );
    router
}

/// Every image operation refuses an `Image` with no content source the same way, whichever
/// operation was called and whatever else the request carried.
#[tokio::test]
async fn an_image_with_no_content_source_is_refused_with_the_fixed_wording() {
    let router = seeded().await;

    // What else the request carries beside the image, per operation: the member set is the
    // operation's own optional input (`smithy_query(query="operation",
    // operation="DetectFaces,IndexFaces")`), at several values each.
    let extras: Vec<(&str, &str, Value)> = vec![
        ("DetectFaces", "nothing else", json!({})),
        ("DetectLabels", "nothing else", json!({})),
        (
            "DetectLabels",
            "a cap and a confidence floor",
            json!({ "MaxLabels": 5, "MinConfidence": 70.5 }),
        ),
        (
            "DetectLabels",
            "both feature types",
            json!({ "Features": ["GENERAL_LABELS", "IMAGE_PROPERTIES"] }),
        ),
        (
            "DetectFaces",
            "the default attribute set",
            json!({ "Attributes": ["DEFAULT"] }),
        ),
        (
            "DetectFaces",
            "every attribute",
            json!({ "Attributes": ["ALL"] }),
        ),
        (
            "DetectFaces",
            "named attributes",
            json!({ "Attributes": ["AGE_RANGE", "FACE_OCCLUDED"] }),
        ),
        (
            "IndexFaces",
            "nothing else",
            json!({ "CollectionId": COLLECTION }),
        ),
        (
            "IndexFaces",
            "an external image id and a face cap",
            json!({
                "CollectionId": COLLECTION,
                "ExternalImageId": "portrait-42",
                "MaxFaces": 1,
            }),
        ),
        (
            "IndexFaces",
            "a quality filter and detection attributes",
            json!({
                "CollectionId": COLLECTION,
                "QualityFilter": "AUTO",
                "DetectionAttributes": ["ALL"],
            }),
        ),
        // `RecognizeCelebrities` models no input member beside the image
        // (`smithy_query(query="operation",operation="RecognizeCelebrities")`), so the
        // image is all one of its requests can carry.
        ("RecognizeCelebrities", "nothing else", json!({})),
    ];

    for (operation, extra_case, extra) in extras {
        let mut body = extra.clone();
        body.as_object_mut()
            .expect("the extra members are an object")
            .insert("Image".to_string(), contentless_image());
        let case = format!("{operation} with {extra_case}");
        let reply = call(&router, operation, body).await;
        assert_missing_content(&case, &reply);
    }
}

/// The other half of the rule: an `Image` naming BOTH content sources identifies no single
/// image to analyse, so it is refused the same way and with its own fixed wording, whichever
/// operation was called and whatever else the request carried — never analysed as if one of
/// the two had been chosen.
#[tokio::test]
async fn an_image_naming_both_content_sources_is_refused_with_the_fixed_wording() {
    let router = seeded().await;
    let png = readable_png();
    // What the two sources are, each pairing inline bytes with an S3 address: the address
    // the store SERVES nothing for and one it would, so the refusal cannot be the store's
    // answer about the object.
    let sources: Vec<(&str, Value)> = vec![
        (
            "an S3 address the store does not serve",
            json!({
                "Bytes": inline_bytes(&png),
                "S3Object": { "Bucket": "no-such-bucket", "Name": "no-such-key" },
            }),
        ),
        (
            "an S3 address naming no key",
            json!({
                "Bytes": inline_bytes(&png),
                "S3Object": { "Bucket": "staged-images" },
            }),
        ),
        (
            "an S3 address naming nothing at all",
            json!({ "Bytes": inline_bytes(&png), "S3Object": {} }),
        ),
        (
            // Bytes that are no image at all, so the refusal cannot be the decoder's
            // answer about them either.
            "inline bytes that are no image, beside an S3 address",
            json!({
                "Bytes": inline_bytes(b"not-an-image-at-all"),
                "S3Object": { "Bucket": "staged-images", "Name": "portrait.png" },
            }),
        ),
    ];

    let operations: Vec<(&str, Value)> = vec![
        ("DetectFaces", json!({})),
        ("DetectLabels", json!({})),
        (
            "DetectLabels",
            json!({ "MaxLabels": 3, "Features": ["GENERAL_LABELS"] }),
        ),
        ("IndexFaces", json!({ "CollectionId": COLLECTION })),
        ("RecognizeCelebrities", json!({})),
    ];

    for (source_case, image) in &sources {
        for (operation, extra) in &operations {
            let mut body = extra.clone();
            body.as_object_mut()
                .expect("the extra members are an object")
                .insert("Image".to_string(), image.clone());
            let case = format!("{operation} with inline bytes beside {source_case}");
            let reply = call(&router, operation, body).await;
            assert_invalid_parameter(&case, &reply, CONFLICTING_CONTENT_MESSAGE);
        }
    }
}

/// The other side of the rule: an image that DOES identify content is answered on its own
/// merits — never with the no-content refusal — so the two failures stay distinguishable.
#[tokio::test]
async fn an_image_that_identifies_content_is_never_the_no_content_refusal() {
    let router = seeded().await;
    let png = readable_png();
    let cases: Vec<(&str, &str, Value, u16, &str)> = vec![
        (
            "DetectFaces on readable inline bytes",
            "DetectFaces",
            json!({ "Image": bytes_image(&png) }),
            200,
            "",
        ),
        (
            // The model's own `ImageBlob` length constraint (`length 1..5242880`) refuses
            // inline content with no bytes in it at the boundary: content WAS identified,
            // it just violates the constraint on it.
            "DetectFaces on inline content carrying no bytes",
            "DetectFaces",
            json!({ "Image": { "Bytes": "" } }),
            400,
            "ValidationException",
        ),
        (
            "IndexFaces on inline content carrying no bytes",
            "IndexFaces",
            json!({ "CollectionId": COLLECTION, "Image": { "Bytes": "" } }),
            400,
            "ValidationException",
        ),
        (
            "DetectFaces on inline bytes that are no image",
            "DetectFaces",
            json!({ "Image": bytes_image(b"not-an-image-at-all") }),
            400,
            "InvalidImageFormatException",
        ),
        (
            "DetectFaces on an S3 object the store does not serve",
            "DetectFaces",
            json!({ "Image": { "S3Object": { "Bucket": "no-such-bucket", "Name": "no-such-key" } } }),
            400,
            "InvalidS3ObjectException",
        ),
        (
            "IndexFaces on readable inline bytes",
            "IndexFaces",
            json!({ "CollectionId": COLLECTION, "Image": bytes_image(&png) }),
            200,
            "",
        ),
        (
            "RecognizeCelebrities on readable inline bytes",
            "RecognizeCelebrities",
            json!({ "Image": bytes_image(&png) }),
            200,
            "",
        ),
        (
            "RecognizeCelebrities on inline bytes that are no image",
            "RecognizeCelebrities",
            json!({ "Image": bytes_image(b"not-an-image-at-all") }),
            400,
            "InvalidImageFormatException",
        ),
        (
            "RecognizeCelebrities on an S3 object the store does not serve",
            "RecognizeCelebrities",
            json!({ "Image": { "S3Object": { "Bucket": "no-such-bucket", "Name": "no-such-key" } } }),
            400,
            "InvalidS3ObjectException",
        ),
        (
            // An S3 address identifies content as much as inline bytes do, so this is
            // content: it is refused for the object the store does not serve, never for
            // an image that named nothing.
            "IndexFaces on an S3 object the store does not serve",
            "IndexFaces",
            json!({
                "CollectionId": COLLECTION,
                "Image": { "S3Object": { "Bucket": "indexed-bucket", "Name": "portrait.png" } },
            }),
            400,
            "InvalidS3ObjectException",
        ),
    ];

    for (case, operation, body, expected_status, expected_code) in cases {
        let reply = call(&router, operation, body).await;
        assert_eq!(
            reply.status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {} with {:?}",
            reply.status, reply.body
        );
        if !expected_code.is_empty() {
            assert_eq!(
                reply.error_code(),
                expected_code,
                "case {case:?}: expected __type {expected_code}, got {:?}",
                reply.body
            );
        }
        assert_ne!(
            reply.string("Message").as_deref(),
            Some(MISSING_CONTENT_MESSAGE),
            "case {case:?}: an image that identifies content is not the no-content refusal, \
             got {:?}",
            reply.body
        );
        assert_ne!(
            reply.string("Message").as_deref(),
            Some(CONFLICTING_CONTENT_MESSAGE),
            "case {case:?}: an image that identifies one source is not the two-source \
             refusal, got {:?}",
            reply.body
        );
    }
}
