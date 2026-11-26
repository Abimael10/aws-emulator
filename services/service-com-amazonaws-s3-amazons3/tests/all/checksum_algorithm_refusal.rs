// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for a request naming a CHECKSUM ALGORITHM S3 does not implement.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (status, error code, message, stored state) rather than any handler's
//! internals.
//!
//! The rule under test, asserted over a SET of inputs rather than one request: the algorithm a
//! request asks its payload to be checksummed under is bound to one modelled enum
//! (`ChecksumAlgorithm`, carried by `x-amz-checksum-algorithm` on the copy and the multipart
//! initiation and by `x-amz-sdk-checksum-algorithm` on every other write), and S3 answers a
//! value outside it with its OWN refusal — `InvalidRequest` naming every algorithm it does
//! implement — rather than with the generic enum-constraint error, which carries a different
//! code, a different sentence and no set a caller could choose from. Nothing is stored.
//!
//! The rows differ along what the rule generalises over: the OPERATION (and so the header the
//! value travels in and the method), and the VALUE (a digest family S3 has no implementation
//! of, a wrong SPELLING of one it does implement, a list where one value belongs, and a
//! near-miss of a published name) — the recorded case is ONE row.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, text};

/// The code S3 answers a request it cannot carry out as given with, at HTTP 400 (Amazon S3
/// *Error responses*, list of error codes).
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code the generic enum-constraint refusal carries, which this refusal must NOT: a
/// client branching on the two mis-classifies the failure.
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The algorithms S3 implements, in the order the model declares them
/// (`smithy_query(query="shape",shape="com.amazonaws.s3#ChecksumAlgorithm")` reports `[CRC32,
/// CRC32C, CRC64NVME, MD5, SHA1, SHA256, SHA512, XXHASH128, XXHASH3, XXHASH64]`) — the set the
/// refusal's sentence lists. Written out here rather than imported, so a change to either the
/// set or its order fails this suite.
const PUBLISHED_ALGORITHMS: &str =
    "CRC32, CRC32C, CRC64NVME, MD5, SHA1, SHA256, SHA512, XXHASH128, XXHASH3, XXHASH64";

/// The sentence S3 refuses an unsupported checksum algorithm with: fixed text around that set.
fn unsupported_algorithm_message() -> String {
    format!(
        "Checksum algorithm provided is unsupported. Please try again with any of the valid \
         types: [{PUBLISHED_ALGORITHMS}]"
    )
}

/// The values no `ChecksumAlgorithm` names: a digest family S3 has no implementation of, a
/// list where one value belongs, a near-miss of a published name, and a name of another hash
/// entirely.
///
/// A value that differs from a published one only in CASE is deliberately not here: AWS's own
/// clients send `crc32` for the `x-amz-sdk-checksum-algorithm` member and S3 serves them, so
/// the set is matched there without regard to ASCII case and such a value is not outside it.
const UNSUPPORTED: [&str; 5] = ["SHA3-256", "CRC32,CRC32C", "SHA-256", "BLAKE3", "CRC33"];

/// The bytes the object this suite copies from holds.
const SOURCE_BODY: &[u8] = b"the source object a copy reads";

/// One operation that binds the algorithm, as the model binds it: the method, the address, the
/// header the value travels in and the body the call carries.
struct Call {
    operation: &'static str,
    method: &'static str,
    /// The address, with `{bucket}` standing for the bucket this suite created.
    uri: &'static str,
    header: &'static str,
    /// The headers the call needs beside the checksum one for the request to be the call at
    /// all (the copy's source reference).
    extra_headers: &'static [(&'static str, &'static str)],
    body: &'static str,
}

/// The source the copy rows read, held under the suite's own bucket.
const SOURCE_KEY: &str = "source/original.txt";

/// The calls: one per spelling of the bound header, over three different methods, so what is
/// pinned is the MEMBER rather than one operation.
fn calls() -> Vec<Call> {
    vec![
        Call {
            operation: "CopyObject",
            method: "PUT",
            uri: "/{bucket}/copied/target.txt?x-id=CopyObject",
            header: "x-amz-checksum-algorithm",
            extra_headers: &[(
                "x-amz-copy-source",
                "checksum-algorithm-bucket/source/original.txt",
            )],
            body: "",
        },
        Call {
            operation: "PutObject",
            method: "PUT",
            uri: "/{bucket}/written/target.txt?x-id=PutObject",
            header: "x-amz-sdk-checksum-algorithm",
            extra_headers: &[],
            body: "the body a write carries",
        },
        Call {
            operation: "CreateMultipartUpload",
            method: "POST",
            uri: "/{bucket}/initiated/target.txt?uploads",
            header: "x-amz-checksum-algorithm",
            extra_headers: &[],
            body: "",
        },
        Call {
            operation: "PutBucketTagging",
            method: "PUT",
            uri: "/{bucket}?tagging",
            header: "x-amz-sdk-checksum-algorithm",
            extra_headers: &[],
            body: "<Tagging><TagSet><Tag><Key>owner</Key><Value>ledger</Value></Tag></TagSet>\
                   </Tagging>",
        },
    ]
}

/// The bucket every row of this suite addresses.
const BUCKET: &str = "checksum-algorithm-bucket";

/// `call` sent against [`BUCKET`], naming `algorithm` in the header the call binds (or naming
/// none when `algorithm` is `None`).
async fn send_call(
    router: &MultiServiceRouter,
    call: &Call,
    algorithm: Option<&str>,
) -> Answer {
    let uri = call.uri.replace("{bucket}", BUCKET);
    let mut headers: Vec<(&str, &str)> = call.extra_headers.to_vec();
    if let Some(algorithm) = algorithm {
        headers.push((call.header, algorithm));
    }
    send_bytes(router, call.method, &uri, &headers, call.body.as_bytes()).await
}

/// RULE: a checksum algorithm outside the modelled set is refused with S3's own
/// `InvalidRequest` naming every algorithm it does implement — on every operation that binds
/// the member, and for every value outside it.
#[tokio::test]
async fn an_unsupported_checksum_algorithm_is_refused_with_the_published_set() {
    let router = router();
    bucket(&router, BUCKET).await;
    let stored = send_bytes(
        &router,
        "PUT",
        &format!("/{BUCKET}/{SOURCE_KEY}"),
        &[],
        SOURCE_BODY,
    )
    .await;
    assert_eq!(
        stored.status,
        200,
        "the copy source must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    for call in calls() {
        for algorithm in UNSUPPORTED {
            let case = format!("{} naming {algorithm:?}", call.operation);
            let answered = send_call(&router, &call, Some(algorithm)).await;
            let body = text(&answered);
            assert_eq!(
                answered.status, 400,
                "case {case}: expected HTTP 400, got {} with body {body:?}",
                answered.status
            );
            assert_eq!(
                answered.error_code(),
                INVALID_REQUEST,
                "case {case}: expected the code {INVALID_REQUEST} rather than \
                 {INVALID_ARGUMENT}, got {:?} in body {body:?}",
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                unsupported_algorithm_message(),
                "case {case}: expected the sentence that lists the supported algorithms, got \
                 {:?}",
                answered.error_message()
            );
        }
    }

    // Nothing any refused row asked for is there afterwards: no copied object, no written
    // object, no upload and no tag set.
    for (case, method, uri) in [
        ("the copy's target", "HEAD", format!("/{BUCKET}/copied/target.txt")),
        ("the write's key", "HEAD", format!("/{BUCKET}/written/target.txt")),
    ] {
        let read = send(&router, method, &uri, &[], "").await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused call must store nothing, got {} for the read",
            read.status
        );
    }
    let uploads = send(&router, "GET", &format!("/{BUCKET}?uploads"), &[], "").await;
    assert!(
        !text(&uploads).contains("initiated/target.txt"),
        "a refused initiation must record no upload, got {:?}",
        text(&uploads)
    );
    let tagging = send(&router, "GET", &format!("/{BUCKET}?tagging"), &[], "").await;
    assert_ne!(
        tagging.status, 200,
        "a refused tag write must leave the bucket with no tag set, got body {:?}",
        text(&tagging)
    );
}

/// RULE (the other side of it): every algorithm the model declares IS accepted, so the
/// refusals above are about the value being outside the set rather than about the header.
#[tokio::test]
async fn every_published_checksum_algorithm_is_accepted() {
    let router = router();
    bucket(&router, BUCKET).await;
    let stored = send_bytes(
        &router,
        "PUT",
        &format!("/{BUCKET}/{SOURCE_KEY}"),
        &[],
        SOURCE_BODY,
    )
    .await;
    assert_eq!(
        stored.status,
        200,
        "the copy source must be stored first, got {} with body {:?}",
        stored.status,
        text(&stored)
    );

    for algorithm in PUBLISHED_ALGORITHMS.split(", ") {
        for call in calls() {
            let case = format!("{} naming {algorithm:?}", call.operation);
            let answered = send_call(&router, &call, Some(algorithm)).await;
            // Each call has its own modelled success status, so what is asserted is that the
            // request was SERVED rather than refused.
            assert!(
                (200..300).contains(&answered.status),
                "case {case}: expected the call to be served, got {} with body {:?}",
                answered.status,
                text(&answered)
            );
        }
    }
}
