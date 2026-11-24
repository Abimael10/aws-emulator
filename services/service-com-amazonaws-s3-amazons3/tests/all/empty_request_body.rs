// SPDX-License-Identifier: CC0-1.0

//! An empty request body earns the same refusal whatever FORM the request arrives in.
//!
//! The rule under test is [`super::missing_request_body`]'s — a request that carries no body
//! where the operation's model requires a document sent no document, which Amazon S3 refuses
//! with `MissingRequestBodyError` and HTTP 400 — and what this suite spans is the half that
//! suite does not: the rule is a property of the BODY, so nothing else about how the request
//! was addressed or framed may change the answer.
//!
//! The table's rows differ along:
//!
//! * the ADDRESS FORM — path style (the bucket in the path) and virtual-hosted style (the
//!   bucket in the first label of the host), which this service resolves differently before
//!   any member is bound;
//! * how the empty body was FRAMED — an explicit `content-length: 0`, no `content-type` at
//!   all, and the `Content-MD5` of the empty string, which a client that digests its payload
//!   sends;
//! * whether the request carried the headers the operation also binds
//!   (`x-amz-expected-bucket-owner` naming the bucket's own owner), because a document that
//!   never arrived is not a question about any other member.
//!
//! Why it matters: a client that retries, or an SDK that addresses the same bucket in the
//! other style, must not see two different refusals for one request — the code is what it
//! branches on.

use super::bucket_lifecycle::{create_bucket, router, send_bytes, send_bytes_to_host, Answer};
use super::object_and_tagging_roundtrip::text;

/// The subresource every row writes: `PutBucketVersioning`, whose `VersioningConfiguration`
/// the model marks `@required`
/// (`smithy_query(query="operation",operation="PutBucketVersioning")`).
const SUBRESOURCE: &str = "versioning";

/// The bucket every row addresses.
const BUCKET: &str = "empty-request-body";

/// The virtual-hosted-style host that bucket is reached at.
const HOST_STYLE_HOST: &str = "empty-request-body.s3.us-east-1.amazonaws.com";

/// The code Amazon S3 answers a request that carried no body where the operation requires a
/// document with (Amazon S3 API Reference, *Error responses*).
const MISSING_REQUEST_BODY_CODE: &str = "MissingRequestBodyError";

/// The `<Message>` that refusal carries.
const MISSING_REQUEST_BODY_MESSAGE: &str = "Request Body is empty";

/// The status it is rendered with.
const REFUSAL_STATUS: u16 = 400;

/// The `Content-MD5` of the empty string, base64 of its MD5 digest — what a client that
/// digests the payload it is about to send puts on a request carrying none.
const EMPTY_BODY_CONTENT_MD5: &str = "1B2M2Y8AsgTpgAmY7PhCfg==";

/// Assert `answer` is the refusal a request that sent no document earns.
fn assert_refused(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.error_code(),
        MISSING_REQUEST_BODY_CODE,
        "case {case:?}: expected the code {MISSING_REQUEST_BODY_CODE:?}, got {:?} in {:?}",
        answer.error_code(),
        text(answer)
    );
    assert_eq!(
        answer.error_message(),
        MISSING_REQUEST_BODY_MESSAGE,
        "case {case:?}: expected the message {MISSING_REQUEST_BODY_MESSAGE:?}, got {:?}",
        answer.error_message()
    );
}

#[tokio::test]
async fn an_empty_body_is_refused_whatever_form_the_request_arrives_in() {
    let router = router();
    let created = create_bucket(&router, BUCKET, None, None).await;
    assert_eq!(
        created.status, 200,
        "the bucket must be created first, got {} with body {:?}",
        created.status,
        text(&created)
    );
    // The account every bucket of this emulator belongs to, which is what the
    // confused-deputy guard admits as an owner.
    let owner = engine::emulator_account_id();

    let path_style = format!("/{BUCKET}?{SUBRESOURCE}");
    // (case, the headers the request carries). Each row sends the SAME empty body to the
    // same operation, path style.
    let framings: [(&str, Vec<(&str, &str)>); 5] = [
        ("no headers beyond the signed ones", vec![]),
        (
            "the XML content type a client sends with a document",
            vec![("content-type", "application/xml")],
        ),
        ("an explicit zero content length", vec![("content-length", "0")]),
        (
            "the Content-MD5 of the empty body",
            vec![("content-md5", EMPTY_BODY_CONTENT_MD5)],
        ),
        (
            "the expected bucket owner the request also binds",
            vec![("x-amz-expected-bucket-owner", owner)],
        ),
    ];
    for (case, headers) in framings {
        let answer = send_bytes(&router, "PUT", &path_style, &headers, b"").await;
        assert_refused(&answer, case);
    }

    // And the same request addressed VIRTUAL-HOSTED style: the bucket is the first label of
    // the host and the path carries the subresource alone.
    let host_style = send_bytes_to_host(
        &router,
        "PUT",
        HOST_STYLE_HOST,
        &format!("/?{SUBRESOURCE}"),
        &[("content-type", "application/xml")],
        b"",
    )
    .await;
    assert_refused(&host_style, "the virtual-hosted-style address");
}
