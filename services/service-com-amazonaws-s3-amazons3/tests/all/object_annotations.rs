// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an OBJECT ANNOTATION's lifecycle: `PutObjectAnnotation` attaches a
//! named payload to a stored object and `GetObjectAnnotation` reads it back.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one
//! the bucket-lifecycle and object-round-trip suites in this crate already own), so what is
//! pinned is the wire behaviour — status, bound headers, payload, error code and message —
//! rather than any handler's internals.
//!
//! The rules under test, with their sources:
//!
//! * An annotation is "a named payload of 1 byte to 1 MiB that you can associate with a
//!   specific object or object version" (`PutObjectAnnotation`'s own documentation), so a
//!   write records those bytes under that name on the addressed object version and a read
//!   of the same name answers exactly them, with the entity tag, size and last-modified
//!   time of the stored payload.
//! * The name is bounded: "Minimum length of 1. Maximum length of 512 bytes"
//!   (`PutObjectAnnotationRequest$AnnotationName`) — a longer one is
//!   `AnnotationNameTooLong` — and "cannot start with `aws` or `s3` (case-insensitive)"
//!   (`DeleteObjectAnnotationRequest$AnnotationName`), which with every other break of the
//!   naming rules is `InvalidAnnotation`.
//! * The payload is bounded and must be text: "Must be between 1 byte and 1 MiB in size,
//!   and must be valid UTF-8 encoded text. If the payload contains invalid UTF-8 bytes, the
//!   request fails with HTTP 415 (Unsupported Media Type)"
//!   (`PutObjectAnnotationRequest$AnnotationPayload`).
//! * "Each object can have up to 1,000 annotations", and a write past that is
//!   `AnnotationLimitExceeded` carrying the COUNTED sentence: how many annotations the write
//!   would have left the object version holding, against the ceiling in force.
//! * A write or a read is addressed by bucket, key and name, so each absent one answers its
//!   own failure: `NoSuchBucket`, `NoSuchKey`, `NoSuchAnnotation`, each with the `<Message>`
//!   S3's error document carries for that code. "If specified, the operation only succeeds
//!   if the object's ETag matches the provided value"
//!   (`PutObjectAnnotationRequest$ObjectIfMatch`).
//! * A copy carries the source's annotations over unless the request says otherwise: "if
//!   this header isn't specified, `COPY` is the default behavior"
//!   (`CopyObjectRequest$AnnotationDirective`).

use service_com_amazonaws_s3_amazons3::state::{
    annotation_limit_exceeded_message, url_encoded_key, MAX_ANNOTATIONS_PER_OBJECT,
    MAX_ANNOTATION_NAME_BYTES, MAX_ANNOTATION_PAYLOAD_BYTES, MIN_ANNOTATION_PAYLOAD_BYTES,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, expected_etag, put_object, text};

/// `PUT /<bucket>/<key>?annotation&annotationName=<name>` — `PutObjectAnnotation`. The
/// `?annotation` literal is what separates this route from `PutObject` on the same method
/// and path, and the name travels as the query parameter the model binds it to.
///
/// `pub(crate)`, like [`get_annotation`] and the two query-carrying variants beside them:
/// the annotation-removal suite in this same test crate writes and reads back the
/// annotations it deletes through these rather than standing up a second pair.
pub(crate) async fn put_annotation(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    name: &str,
    payload: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    put_annotation_at(router, bucket, key, name, "", payload, headers).await
}

/// [`put_annotation`] with whatever extra query parameters the case carries — a
/// `&versionId=<id>` addressing one stored version, say.
pub(crate) async fn put_annotation_at(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    name: &str,
    query: &str,
    payload: &[u8],
    headers: &[(&str, &str)],
) -> Answer {
    let uri = format!(
        "/{bucket}/{key}?annotation&annotationName={}{query}",
        url_encoded_key(name)
    );
    send_bytes(router, "PUT", &uri, headers, payload).await
}

/// `GET /<bucket>/<key>?annotation&annotationName=<name>&x-id=GetObjectAnnotation` —
/// `GetObjectAnnotation`. The `x-id` literal is what the model binds to separate this read
/// from `ListObjectAnnotations`, which is bound to the same method, path and `?annotation`.
pub(crate) async fn get_annotation(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    name: &str,
) -> Answer {
    get_annotation_at(router, bucket, key, name, "").await
}

/// [`get_annotation`] with whatever extra query parameters the case carries — a
/// `&versionId=<id>` addressing one stored version, say.
pub(crate) async fn get_annotation_at(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    name: &str,
    query: &str,
) -> Answer {
    let uri = format!(
        "/{bucket}/{key}?annotation&annotationName={}&x-id=GetObjectAnnotation{query}",
        url_encoded_key(name)
    );
    send_bytes(router, "GET", &uri, &[], b"").await
}

/// `PUT /<bucket>/<key>` carrying `x-amz-copy-source` — `CopyObject`, with whatever extra
/// headers the case names.
async fn copy_object(
    router: &MultiServiceRouter,
    destination_bucket: &str,
    destination_key: &str,
    copy_source: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("x-amz-copy-source", copy_source)];
    all.extend_from_slice(headers);
    send_bytes(
        router,
        "PUT",
        &format!("/{destination_bucket}/{destination_key}"),
        &all,
        b"",
    )
    .await
}

/// An object to hang annotations on, written the way the object-round-trip suite writes
/// one.
async fn object(router: &MultiServiceRouter, bucket_name: &str, key: &str, body: &[u8]) {
    let written = put_object(router, bucket_name, key, body, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object {key:?} must be stored first, got {} with body {:?}",
        written.status,
        text(&written)
    );
}

/// A name of exactly `bytes` ASCII bytes, for the rows that sit on the published bound.
fn name_of(bytes: usize) -> String {
    "a".repeat(bytes)
}

/// `length` bytes of valid UTF-8 text, cycling the printable ASCII range from a fixed
/// start, so a large payload is varied without anything random — and still text, which is
/// what an annotation payload has to be.
fn text_of(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| b'!' + u8::try_from(index % 90).unwrap_or(0))
        .collect()
}

/// One row of the round-trip table: the case name, the annotation name, and its payload.
type RoundTripRow<'a> = (&'a str, String, Vec<u8>);

#[tokio::test]
async fn annotations_round_trip_through_put_and_get() {
    let router = router();
    bucket(&router, "annotation-bucket").await;
    object(&router, "annotation-bucket", "reports/q1.csv", b"region,total\n").await;

    // The rows differ along what the rule generalises over: the NAME's size (one byte, a
    // plain word, the full 512-byte maximum), its form (a dotted key, a name that merely
    // CONTAINS a reserved prefix, UTF-8 text that is more bytes than characters) and the
    // PAYLOAD's size (the one-byte minimum, a sentence, the 1 MiB maximum) and content
    // (ASCII, multi-byte UTF-8).
    let rows: [RoundTripRow<'_>; 8] = [
        ("a one-byte name", "x".to_string(), b"1".to_vec()),
        (
            "a plain word",
            "reviewer".to_string(),
            b"approved by the data team".to_vec(),
        ),
        (
            "a dotted, namespaced name",
            "com.example.pipeline.stage".to_string(),
            b"ingest".to_vec(),
        ),
        (
            "a name that contains a reserved prefix without starting with it",
            "my-aws-notes".to_string(),
            b"not reserved".to_vec(),
        ),
        (
            "a name of UTF-8 text, more bytes than characters",
            "données-été".to_string(),
            "valeur — oui".as_bytes().to_vec(),
        ),
        (
            "a payload of multi-byte UTF-8 text",
            "résumé".to_string(),
            "✓ ✗ — ∑ 漢字".as_bytes().to_vec(),
        ),
        (
            "the longest name the API publishes",
            name_of(MAX_ANNOTATION_NAME_BYTES),
            b"on the bound".to_vec(),
        ),
        (
            "the largest payload the API publishes",
            "bulk".to_string(),
            text_of(MAX_ANNOTATION_PAYLOAD_BYTES),
        ),
    ];

    for (case, name, payload) in rows {
        let put = put_annotation(
            &router,
            "annotation-bucket",
            "reports/q1.csv",
            &name,
            &payload,
            &[],
        )
        .await;
        assert_eq!(
            put.status,
            200,
            "case {case:?}: expected HTTP 200 from PutObjectAnnotation, got {} with body {:?}",
            put.status,
            text(&put)
        );
        let etag = expected_etag(&payload);
        assert_eq!(
            put.header("etag").as_deref(),
            Some(etag.as_str()),
            "case {case:?}: expected the entity tag of the payload {etag:?}, got {:?}",
            put.header("etag")
        );

        let got = get_annotation(&router, "annotation-bucket", "reports/q1.csv", &name).await;
        assert_eq!(
            got.status,
            200,
            "case {case:?}: expected HTTP 200 from GetObjectAnnotation on the annotation \
             just written, got {} with body {:?}",
            got.status,
            text(&got)
        );
        assert_eq!(
            got.body.as_ref(),
            payload.as_slice(),
            "case {case:?}: expected the stored payload back ({} bytes), got {} bytes",
            payload.len(),
            got.body.len()
        );
        assert_eq!(
            got.header("etag").as_deref(),
            Some(etag.as_str()),
            "case {case:?}: expected the read to report the same entity tag {etag:?}, got {:?}",
            got.header("etag")
        );
        assert_eq!(
            got.header("content-length"),
            Some(payload.len().to_string()),
            "case {case:?}: expected Content-Length {}, got {:?}",
            payload.len(),
            got.header("content-length")
        );
        assert!(
            got.header("last-modified").is_some(),
            "case {case:?}: a stored annotation reports when it was written, got no \
             Last-Modified"
        );
        // The bucket has no versioning state, so its objects carry no version id of their
        // own and none is reported rather than one being invented.
        assert_eq!(
            got.header("x-amz-object-version-id"),
            None,
            "case {case:?}: an object with no version id reports none, got {:?}",
            got.header("x-amz-object-version-id")
        );
    }
}

/// One row of a refusal table: the case name, the request's annotation name, its payload,
/// the HTTP status it must answer and the wire error code it must carry.
type RefusalRow<'a> = (&'a str, String, Vec<u8>, u16, &'a str);

#[tokio::test]
async fn an_annotation_name_outside_its_published_rules_is_refused() {
    let router = router();
    bucket(&router, "name-rule-bucket").await;
    object(&router, "name-rule-bucket", "data.json", b"{}").await;

    // The rows walk the published bound (the 512-byte maximum, and one byte past it, in
    // ASCII and in multi-byte UTF-8 where the two units differ), the characters a name is
    // spelled out of (a symbol, a space and a colon, and the path separator the namespace IS
    // spelled with, at the front of a name and inside one) and the reserved prefixes, in both
    // cases, at and past the prefix itself. `AnnotationNameTooLong` is "Your annotation name
    // is too long." and every other break of the rules is `InvalidAnnotation`, so each row
    // names the one its own rule earns — and the READ of the same name earns the same
    // refusal, because the name is judged before the annotation it addresses is resolved.
    let too_long_in_bytes = "é".repeat(MAX_ANNOTATION_NAME_BYTES / 2 + 1);
    assert_eq!(
        too_long_in_bytes.len(),
        MAX_ANNOTATION_NAME_BYTES + 2,
        "the test's own multi-byte name must be past the bound in BYTES, got {} bytes",
        too_long_in_bytes.len()
    );
    let rows: [RefusalRow<'_>; 12] = [
        (
            "one byte past the published maximum",
            name_of(MAX_ANNOTATION_NAME_BYTES + 1),
            b"too long".to_vec(),
            400,
            "AnnotationNameTooLong",
        ),
        (
            "within the character count but past the byte count",
            too_long_in_bytes,
            b"too long".to_vec(),
            400,
            "AnnotationNameTooLong",
        ),
        (
            "the lowercase reserved prefix",
            "aws-owned".to_string(),
            b"reserved".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "that prefix in another case",
            "AwS".to_string(),
            b"reserved".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "the other reserved prefix",
            "s3-managed".to_string(),
            b"reserved".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "that prefix uppercased, with nothing after it",
            "S3".to_string(),
            b"reserved".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "a symbol inside the name",
            "bad!name".to_string(),
            b"not a name".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "a space inside the name",
            "two words".to_string(),
            b"not a name".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "a path separator inside the name, which the namespace is spelled with",
            "notes/2026".to_string(),
            b"a name".to_vec(),
            200,
            "",
        ),
        (
            "a leading path separator",
            "/notes".to_string(),
            b"not a name".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "a colon-namespaced name, which this namespace is not spelled with",
            "team:review".to_string(),
            b"not a name".to_vec(),
            400,
            "InvalidAnnotation",
        ),
        (
            "a name that only ends with a reserved prefix",
            "team-s3".to_string(),
            b"not reserved".to_vec(),
            200,
            "",
        ),
    ];

    for (case, name, payload, status, code) in rows {
        let put = put_annotation(
            &router,
            "name-rule-bucket",
            "data.json",
            &name,
            &payload,
            &[],
        )
        .await;
        assert_eq!(
            put.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            put.status,
            text(&put)
        );
        if status == 200 {
            continue;
        }
        assert_eq!(
            put.error_code(),
            code,
            "case {case:?}: expected the error code {code:?}, got {:?}",
            put.error_code()
        );
        assert!(
            !put.error_message().is_empty(),
            "case {case:?}: an S3 error document carries a message beside its code, got none"
        );
        // A name the write refuses for breaking the rules is not a name any object can be
        // carrying, so the READ of it answers the same refusal rather than reporting an
        // annotation that is simply absent.
        let got = get_annotation(&router, "name-rule-bucket", "data.json", &name).await;
        assert_eq!(
            got.status, status,
            "case {case:?}: expected the read to be refused HTTP {status} like the write, \
             got {} with body {:?}",
            got.status,
            text(&got)
        );
        assert_eq!(
            got.error_code(),
            code,
            "case {case:?}: expected the read to carry the write's code {code:?}, got {:?}",
            got.error_code()
        );
        assert_eq!(
            got.error_message(),
            put.error_message(),
            "case {case:?}: expected the read to carry the write's message {:?}, got {:?}",
            put.error_message(),
            got.error_message()
        );
    }
}

#[tokio::test]
async fn an_annotation_payload_outside_its_published_rules_is_refused() {
    let router = router();
    bucket(&router, "payload-rule-bucket").await;
    object(&router, "payload-rule-bucket", "data.json", b"{}").await;

    // The rows walk both published payload bounds — the one-byte minimum and the 1 MiB
    // maximum, each on the bound and one byte outside it — and the UTF-8 rule, with bytes
    // that are no encoding of any text (a lone 0xFF, a truncated multi-byte sequence).
    // "If the payload contains invalid UTF-8 bytes, the request fails with HTTP 415".
    let rows: [RefusalRow<'_>; 6] = [
        (
            "nothing at all, under the one-byte minimum",
            "empty".to_string(),
            Vec::new(),
            400,
            "InvalidRequest",
        ),
        (
            "exactly the minimum",
            "one-byte".to_string(),
            text_of(MIN_ANNOTATION_PAYLOAD_BYTES),
            200,
            "",
        ),
        (
            "exactly the maximum",
            "one-mebibyte".to_string(),
            text_of(MAX_ANNOTATION_PAYLOAD_BYTES),
            200,
            "",
        ),
        (
            "one byte past the maximum",
            "past-the-maximum".to_string(),
            text_of(MAX_ANNOTATION_PAYLOAD_BYTES + 1),
            400,
            "InvalidRequest",
        ),
        (
            "a byte that encodes no character",
            "lone-ff".to_string(),
            vec![0xFF],
            415,
            "UnsupportedMediaType",
        ),
        (
            "a truncated multi-byte sequence",
            "truncated".to_string(),
            vec![b'o', b'k', 0xE2, 0x9C],
            415,
            "UnsupportedMediaType",
        ),
    ];

    for (case, name, payload, status, code) in rows {
        let put = put_annotation(
            &router,
            "payload-rule-bucket",
            "data.json",
            &name,
            &payload,
            &[],
        )
        .await;
        assert_eq!(
            put.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            put.status,
            text(&put)
        );
        let got = get_annotation(&router, "payload-rule-bucket", "data.json", &name).await;
        if status == 200 {
            assert_eq!(
                got.status, 200,
                "case {case:?}: the payload on the bound is stored and read back, got {} \
                 with body {:?}",
                got.status,
                text(&got)
            );
            assert_eq!(
                got.body.len(),
                payload.len(),
                "case {case:?}: expected {} stored bytes, got {}",
                payload.len(),
                got.body.len()
            );
            continue;
        }
        assert_eq!(
            put.error_code(),
            code,
            "case {case:?}: expected the error code {code:?}, got {:?}",
            put.error_code()
        );
        assert_eq!(
            got.status, 404,
            "case {case:?}: a refused write records nothing, so the read must answer 404, \
             got {} with body {:?}",
            got.status,
            text(&got)
        );
    }
}

#[tokio::test]
async fn an_annotation_is_addressed_by_bucket_key_and_name() {
    let router = router();
    bucket(&router, "address-bucket").await;
    object(&router, "address-bucket", "present.txt", b"here").await;
    let present = put_annotation(
        &router,
        "address-bucket",
        "present.txt",
        "note",
        b"attached",
        &[],
    )
    .await;
    assert_eq!(
        present.status,
        200,
        "the annotation the address table reads must be written first, got {} with body {:?}",
        present.status,
        text(&present)
    );

    // (case, bucket, key, name, the status a write of that address answers, its code).
    let writes: [(&str, &str, &str, &str, u16, &str); 4] = [
        (
            "a bucket this emulator does not hold",
            "no-such-bucket",
            "present.txt",
            "note",
            404,
            "NoSuchBucket",
        ),
        (
            "a key the bucket does not hold",
            "address-bucket",
            "absent.txt",
            "note",
            404,
            "NoSuchKey",
        ),
        (
            "a key that is a prefix of the stored one",
            "address-bucket",
            "present",
            "note",
            404,
            "NoSuchKey",
        ),
        (
            "the address that is there",
            "address-bucket",
            "present.txt",
            "second-note",
            200,
            "",
        ),
    ];
    for (case, bucket_name, key, name, status, code) in writes {
        let put = put_annotation(&router, bucket_name, key, name, b"payload", &[]).await;
        assert_eq!(
            put.status, status,
            "case {case:?}: expected HTTP {status} from PutObjectAnnotation, got {} with \
             body {:?}",
            put.status,
            text(&put)
        );
        if status != 200 {
            assert_eq!(
                put.error_code(),
                code,
                "case {case:?}: expected the error code {code:?}, got {:?}",
                put.error_code()
            );
        }
    }

    // The same three addresses on the READ side, plus a name the object does not carry.
    // Each refusal row names the `<Message>` S3's error document carries beside the code,
    // which is what an SDK surfaces to a client.
    let reads: [(&str, &str, &str, &str, u16, &str, &str); 4] = [
        (
            "a bucket this emulator does not hold",
            "no-such-bucket",
            "present.txt",
            "note",
            404,
            "NoSuchBucket",
            "The specified bucket does not exist",
        ),
        (
            "a key the bucket does not hold",
            "address-bucket",
            "absent.txt",
            "note",
            404,
            "NoSuchKey",
            "The specified key does not exist.",
        ),
        (
            "a name the object does not carry",
            "address-bucket",
            "present.txt",
            "never-written",
            404,
            "NoSuchAnnotation",
            "The specified annotation does not exist.",
        ),
        (
            "the annotation that is there",
            "address-bucket",
            "present.txt",
            "note",
            200,
            "",
            "",
        ),
    ];
    for (case, bucket_name, key, name, status, code, message) in reads {
        let got = get_annotation(&router, bucket_name, key, name).await;
        assert_eq!(
            got.status, status,
            "case {case:?}: expected HTTP {status} from GetObjectAnnotation, got {} with \
             body {:?}",
            got.status,
            text(&got)
        );
        if status != 200 {
            assert_eq!(
                got.error_code(),
                code,
                "case {case:?}: expected the error code {code:?}, got {:?}",
                got.error_code()
            );
            assert_eq!(
                got.error_message(),
                message,
                "case {case:?}: expected the message {message:?}, got {:?}",
                got.error_message()
            );
        }
    }
}

#[tokio::test]
async fn a_conditional_write_only_succeeds_on_a_matching_object_etag() {
    let router = router();
    bucket(&router, "if-match-bucket").await;
    let body = b"the object this annotation hangs on";
    object(&router, "if-match-bucket", "conditional.txt", body).await;
    let etag = expected_etag(body);

    // (case, the `x-amz-object-if-match` value, the status it answers, its code).
    let rows: [(&str, String, u16, &str); 3] = [
        (
            "the object's own entity tag",
            etag.clone(),
            200,
            "",
        ),
        (
            "the entity tag of other bytes",
            expected_etag(b"something else"),
            412,
            "PreconditionFailed",
        ),
        (
            "a value that is no entity tag at all",
            "\"not-a-digest\"".to_string(),
            412,
            "PreconditionFailed",
        ),
    ];

    for (case, condition, status, code) in rows {
        let put = put_annotation(
            &router,
            "if-match-bucket",
            "conditional.txt",
            "conditional-note",
            b"only on a match",
            &[("x-amz-object-if-match", condition.as_str())],
        )
        .await;
        assert_eq!(
            put.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            put.status,
            text(&put)
        );
        if status != 200 {
            assert_eq!(
                put.error_code(),
                code,
                "case {case:?}: expected the error code {code:?}, got {:?}",
                put.error_code()
            );
        }
    }
}

#[tokio::test]
async fn one_name_is_replaced_without_touching_the_others() {
    let router = router();
    bucket(&router, "replacement-bucket").await;
    object(&router, "replacement-bucket", "many.txt", b"payload").await;

    // Three names written once each, then ONE of them written again: a second write under
    // a name is the whole new payload of that name, and the annotations the request did not
    // name are left exactly as they were.
    let first: [(&str, &[u8]); 3] = [
        ("first", b"one"),
        ("second", b"two"),
        ("third", b"three"),
    ];
    for (name, payload) in first {
        let put =
            put_annotation(&router, "replacement-bucket", "many.txt", name, payload, &[]).await;
        assert_eq!(
            put.status,
            200,
            "case {name:?}: expected HTTP 200 from the first write, got {} with body {:?}",
            put.status,
            text(&put)
        );
    }

    let replacement = b"two, revised and longer than before";
    let rewritten = put_annotation(
        &router,
        "replacement-bucket",
        "many.txt",
        "second",
        replacement,
        &[],
    )
    .await;
    assert_eq!(
        rewritten.status,
        200,
        "expected HTTP 200 from the second write of one name, got {} with body {:?}",
        rewritten.status,
        text(&rewritten)
    );

    for (name, payload) in [
        ("first", b"one".to_vec()),
        ("second", replacement.to_vec()),
        ("third", b"three".to_vec()),
    ] {
        let got = get_annotation(&router, "replacement-bucket", "many.txt", name).await;
        assert_eq!(
            got.status,
            200,
            "case {name:?}: expected HTTP 200 reading the annotation back, got {} with body {:?}",
            got.status,
            text(&got)
        );
        assert_eq!(
            got.body.as_ref(),
            payload.as_slice(),
            "case {name:?}: expected the payload {:?}, got {:?}",
            String::from_utf8_lossy(&payload),
            String::from_utf8_lossy(&got.body)
        );
    }
}

/// Attach annotations to the object version `query` addresses until it carries exactly the
/// ceiling, so the next write naming a NEW annotation is the first one past it.
async fn fill_to_the_ceiling(
    router: &MultiServiceRouter,
    bucket_name: &str,
    key: &str,
    query: &str,
) {
    for ordinal in 0..MAX_ANNOTATIONS_PER_OBJECT {
        let name = format!("note-{ordinal:04}");
        let put = put_annotation_at(router, bucket_name, key, &name, query, b"x", &[]).await;
        assert_eq!(
            put.status,
            200,
            "case {name:?}: an object under the ceiling takes the write, got {} with body {:?}",
            put.status,
            text(&put)
        );
    }
}

/// The sentence a write past the ceiling is refused with when the object version holds
/// exactly the ceiling: the count the write would have produced is what the version holds
/// plus the one the request names, and the ceiling is the one in force.
///
/// Computed from [`MAX_ANNOTATIONS_PER_OBJECT`] rather than written out, so what is pinned is
/// the template and not one ceiling's two numbers.
fn counted_ceiling_message() -> String {
    annotation_limit_exceeded_message(MAX_ANNOTATIONS_PER_OBJECT + 1, MAX_ANNOTATIONS_PER_OBJECT)
}

/// The `@documentation` of an error shape of this service's model, HTML stripped — read from
/// the model rather than retyped.
///
/// For [`LIMIT_EXCEEDED_CODE`] that sentence is the API reference's DESCRIPTION of the
/// condition, which is exactly what must NOT reach the wire in place of the counted message.
fn modeled_documentation(shape: &str) -> String {
    let model_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-s3-amazons3.json"
    );
    let model: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(model_path).expect("the service's Smithy model must be readable"),
    )
    .expect("the Smithy model must parse as JSON");
    model["shapes"][format!("com.amazonaws.s3#{shape}")]["traits"]["smithy.api#documentation"]
        .as_str()
        .unwrap_or_else(|| panic!("the model must document the shape {shape:?}"))
        .replace("<p>", "")
        .replace("</p>", "")
        .trim()
        .to_string()
}

/// The wire error code a write past the per-object annotation ceiling is refused with.
const LIMIT_EXCEEDED_CODE: &str = "AnnotationLimitExceeded";

#[tokio::test]
async fn an_object_holds_no_more_annotations_than_the_published_maximum() {
    let router = router();
    bucket(&router, "ceiling-bucket").await;
    object(&router, "ceiling-bucket", "full.txt", b"payload").await;

    // "Each object can have up to 1,000 annotations": the last write at the ceiling is
    // taken, the first one past it is refused, and a write that REPLACES a name the object
    // already carries is taken even then, because it adds no annotation.
    fill_to_the_ceiling(&router, "ceiling-bucket", "full.txt", "").await;

    let past = put_annotation(
        &router,
        "ceiling-bucket",
        "full.txt",
        "one-too-many",
        b"x",
        &[],
    )
    .await;
    assert_eq!(
        past.status,
        400,
        "case \"one past the ceiling\": expected HTTP 400, got {} with body {:?}",
        past.status,
        text(&past)
    );
    assert_eq!(
        past.error_code(),
        LIMIT_EXCEEDED_CODE,
        "case \"one past the ceiling\": expected AnnotationLimitExceeded, got {:?}",
        past.error_code()
    );
    let expected = counted_ceiling_message();
    assert_eq!(
        past.error_message(),
        expected,
        "case \"one past the ceiling\": expected the counted message {expected:?}, got {:?}",
        past.error_message()
    );

    let replaced = put_annotation(
        &router,
        "ceiling-bucket",
        "full.txt",
        "note-0000",
        b"y",
        &[],
    )
    .await;
    assert_eq!(
        replaced.status,
        200,
        "case \"replacing a name at the ceiling\": expected HTTP 200, got {} with body {:?}",
        replaced.status,
        text(&replaced)
    );
    let got = get_annotation(&router, "ceiling-bucket", "full.txt", "note-0000").await;
    assert_eq!(
        got.body.as_ref(),
        b"y",
        "case \"replacing a name at the ceiling\": expected the new payload, got {:?}",
        String::from_utf8_lossy(&got.body)
    );
}

/// One row of the one-too-many table: the case name, the annotation name the refused write
/// carries and its payload.
struct PastTheCeilingRow {
    case: &'static str,
    name: String,
    payload: Vec<u8>,
}

#[tokio::test]
async fn a_write_past_the_ceiling_is_refused_with_the_counted_message() {
    let router = router();
    bucket(&router, "counted-ceiling-bucket").await;
    object(&router, "counted-ceiling-bucket", "full.txt", b"payload").await;
    fill_to_the_ceiling(&router, "counted-ceiling-bucket", "full.txt", "").await;

    let expected = counted_ceiling_message();
    let prose = modeled_documentation(LIMIT_EXCEEDED_CODE);

    // The rows differ along what the rule generalises over: the annotation NAME's size and
    // form (a word, a dotted namespaced name, the published 512-byte maximum, UTF-8 text that
    // is more bytes than characters) and the PAYLOAD's size (the one-byte minimum, a word, a
    // kibibyte). None of that is in the sentence — it is filled from the object version's own
    // count and the ceiling in force — so every row is refused with the same one. And none of
    // them is written, so each later row is still the first past the ceiling.
    let rows = [
        PastTheCeilingRow {
            case: "a plain name, the one-byte minimum payload",
            name: "one-too-many".to_string(),
            payload: b"x".to_vec(),
        },
        PastTheCeilingRow {
            case: "a dotted, namespaced name",
            name: "com.example.pipeline.stage".to_string(),
            payload: b"ingest".to_vec(),
        },
        PastTheCeilingRow {
            case: "a name of the published maximum length",
            name: name_of(MAX_ANNOTATION_NAME_BYTES),
            payload: b"at the bound".to_vec(),
        },
        PastTheCeilingRow {
            case: "a name of UTF-8 text, more bytes than characters",
            name: "données-été".to_string(),
            payload: "valeur — oui".as_bytes().to_vec(),
        },
        PastTheCeilingRow {
            case: "a kibibyte of payload",
            name: "kibibyte".to_string(),
            payload: text_of(1024),
        },
    ];

    for row in &rows {
        let PastTheCeilingRow {
            case,
            name,
            payload,
        } = row;
        let past = put_annotation(
            &router,
            "counted-ceiling-bucket",
            "full.txt",
            name,
            payload,
            &[],
        )
        .await;
        assert_eq!(
            past.status,
            400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            past.status,
            text(&past)
        );
        assert_eq!(
            past.error_code(),
            LIMIT_EXCEEDED_CODE,
            "case {case:?}: expected error code {LIMIT_EXCEEDED_CODE:?}, got {:?}",
            past.error_code()
        );
        assert_eq!(
            past.error_message(),
            expected,
            "case {case:?}: expected the counted message {expected:?}, got {:?}",
            past.error_message()
        );
        assert_ne!(
            past.error_message(),
            prose,
            "case {case:?}: the error shape's documentation describes the condition for the \
             API reference and is not the message on the wire, got {:?}",
            past.error_message()
        );

        // Nothing was written, so the count the next row is refused against is the one this
        // row reported: the refused name is not an annotation of the object.
        let absent = get_annotation(&router, "counted-ceiling-bucket", "full.txt", name).await;
        assert_eq!(
            absent.status,
            404,
            "case {case:?}: the refused annotation must be absent, got {} with body {:?}",
            absent.status,
            text(&absent)
        );
    }
}

#[tokio::test]
async fn the_annotation_ceiling_is_counted_on_the_addressed_object_version_alone() {
    let router = router();
    bucket(&router, "counted-version-bucket").await;
    let versioning = send(
        &router,
        "PUT",
        "/counted-version-bucket?versioning",
        &[],
        "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Status>Enabled</Status></VersioningConfiguration>",
    )
    .await;
    assert_eq!(
        versioning.status,
        200,
        "versioning must be enabled first, got {} with body {:?}",
        versioning.status,
        text(&versioning)
    );

    // Two versions of one key: the FIRST is filled to the ceiling, the second — which is the
    // key's current object — carries no annotation at all.
    let first = put_object(&router, "counted-version-bucket", "versioned.txt", b"one", &[]).await;
    let second = put_object(&router, "counted-version-bucket", "versioned.txt", b"two", &[]).await;
    let first_version = first
        .header("x-amz-version-id")
        .expect("a write into a versioning-enabled bucket reports the version it stored");
    let second_version = second
        .header("x-amz-version-id")
        .expect("a write into a versioning-enabled bucket reports the version it stored");
    assert_ne!(
        first_version, second_version,
        "the two writes must store two versions, got {first_version:?} twice"
    );
    let filled = format!("&versionId={first_version}");
    fill_to_the_ceiling(&router, "counted-version-bucket", "versioned.txt", &filled).await;

    let past = put_annotation_at(
        &router,
        "counted-version-bucket",
        "versioned.txt",
        "one-too-many",
        &filled,
        b"x",
        &[],
    )
    .await;
    let expected = counted_ceiling_message();
    assert_eq!(
        past.status,
        400,
        "case \"the filled version\": expected HTTP 400, got {} with body {:?}",
        past.status,
        text(&past)
    );
    assert_eq!(
        past.error_message(),
        expected,
        "case \"the filled version\": expected the counted message {expected:?}, got {:?}",
        past.error_message()
    );

    // The identical write on the OTHER version of the same key is taken: the count the
    // sentence above stated is that one version's, read off the state of the object version
    // the request addressed.
    for (case, query) in [
        (
            "the other version, addressed by id",
            format!("&versionId={second_version}"),
        ),
        (
            "the other version, as the key's current object",
            String::new(),
        ),
    ] {
        let taken = put_annotation_at(
            &router,
            "counted-version-bucket",
            "versioned.txt",
            "one-too-many",
            &query,
            b"x",
            &[],
        )
        .await;
        assert_eq!(
            taken.status,
            200,
            "case {case:?}: a version under the ceiling takes the write, got {} with body {:?}",
            taken.status,
            text(&taken)
        );
    }
}

#[test]
fn the_counted_ceiling_sentence_states_the_two_counts_it_is_filled_from() {
    // The fixed fragments of the sentence, in the order it carries them: the requirement,
    // with the two counts spliced between them.
    const OPENS: &str = "The object annotation count (";
    const JOINS: &str = ") exceeds the limit (";
    const CLOSES: &str = ").";

    // The rows differ along what the template generalises over — the two counts — from the
    // smallest ceiling a write can be refused against up to one well past the ceiling this
    // service holds ([`MAX_ANNOTATIONS_PER_OBJECT`]).
    let rows: [(&str, usize, usize); 6] = [
        ("one past a ceiling of nothing", 1, 0),
        ("one past a ceiling of one", 2, 1),
        ("one past a ceiling of ten", 11, 10),
        (
            "one past the ceiling this service holds",
            MAX_ANNOTATIONS_PER_OBJECT + 1,
            MAX_ANNOTATIONS_PER_OBJECT,
        ),
        (
            "well past the ceiling this service holds",
            MAX_ANNOTATIONS_PER_OBJECT + 500,
            MAX_ANNOTATIONS_PER_OBJECT,
        ),
        ("one past a ceiling of ten thousand", 10_001, 10_000),
    ];

    for (case, count, limit) in rows {
        let message = annotation_limit_exceeded_message(count, limit);
        assert!(
            message.starts_with(OPENS),
            "case {case:?}: expected the sentence to open with {OPENS:?}, got {message:?}"
        );
        assert!(
            message.contains(JOINS),
            "case {case:?}: expected the sentence to join its two counts with {JOINS:?}, got \
             {message:?}"
        );
        assert!(
            message.ends_with(CLOSES),
            "case {case:?}: expected the sentence to close with {CLOSES:?}, got {message:?}"
        );

        // Both counts are read back OUT of the sentence, in the order it states them: the
        // count the refused write would have produced first, the ceiling second.
        let counts: Vec<usize> = message
            .split(['(', ')'])
            .filter_map(|piece| piece.parse::<usize>().ok())
            .collect();
        assert_eq!(
            counts,
            vec![count, limit],
            "case {case:?}: expected the sentence to state {count} then {limit}, got {counts:?} \
             from {message:?}"
        );
    }
}

#[tokio::test]
async fn a_copy_carries_the_sources_annotations_unless_the_request_excludes_them() {
    let router = router();
    bucket(&router, "copy-source-bucket").await;
    object(&router, "copy-source-bucket", "source.txt", b"copied bytes").await;
    for (name, payload) in [("owner", &b"data team"[..]), ("stage", &b"curated"[..])] {
        let put = put_annotation(
            &router,
            "copy-source-bucket",
            "source.txt",
            name,
            payload,
            &[],
        )
        .await;
        assert_eq!(
            put.status,
            200,
            "case {name:?}: the source's annotation must be written first, got {} with body {:?}",
            put.status,
            text(&put)
        );
    }

    // (case, the directive the copy carries, whether the destination must hold them).
    let rows: [(&str, Option<&str>, bool); 3] = [
        ("no directive at all, which is COPY", None, true),
        ("the directive COPY, stated", Some("COPY"), true),
        ("the directive EXCLUDE", Some("EXCLUDE"), false),
    ];

    for (case, directive, carried) in rows {
        let key = format!("copy-{}", directive.unwrap_or("default"));
        let headers: Vec<(&str, &str)> = directive
            .map(|value| vec![("x-amz-object-annotation-directive", value)])
            .unwrap_or_default();
        let copied = copy_object(
            &router,
            "copy-source-bucket",
            &key,
            "/copy-source-bucket/source.txt",
            &headers,
        )
        .await;
        assert_eq!(
            copied.status,
            200,
            "case {case:?}: expected HTTP 200 from CopyObject, got {} with body {:?}",
            copied.status,
            text(&copied)
        );

        for (name, payload) in [("owner", &b"data team"[..]), ("stage", &b"curated"[..])] {
            let got = get_annotation(&router, "copy-source-bucket", &key, name).await;
            if carried {
                assert_eq!(
                    got.status,
                    200,
                    "case {case:?}: the copy carries {name:?}, so the read must answer 200, \
                     got {} with body {:?}",
                    got.status,
                    text(&got)
                );
                assert_eq!(
                    got.body.as_ref(),
                    payload,
                    "case {case:?}: expected the source's payload for {name:?}, got {:?}",
                    String::from_utf8_lossy(&got.body)
                );
            } else {
                assert_eq!(
                    got.status, 404,
                    "case {case:?}: the copy excludes {name:?}, so the read must answer 404, \
                     got {} with body {:?}",
                    got.status,
                    text(&got)
                );
                assert_eq!(
                    got.error_code(),
                    "NoSuchAnnotation",
                    "case {case:?}: expected NoSuchAnnotation for {name:?}, got {:?}",
                    got.error_code()
                );
            }
        }
    }
}
