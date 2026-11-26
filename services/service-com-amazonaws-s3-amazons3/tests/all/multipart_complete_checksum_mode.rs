// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the checksum MODE a `CompleteMultipartUpload` may state: the mode the
//! upload was created with, and nothing else.
//!
//! Every assertion drives a real HTTP request through the router, so what is pinned is the
//! wire behaviour (status, error code, message, stored object) rather than any handler's
//! internals.
//!
//! The rule under test, asserted over a SET of inputs rather than one request: a completion's
//! `ChecksumType` "determines how part-level checksums are combined to create an object-level
//! checksum for multipart objects" (`CompleteMultipartUploadRequest$ChecksumType`), and which
//! composition that is was settled by the INITIATION — so a completion stating the OTHER mode
//! is refused 400 `InvalidRequest` naming the mode the upload carries, and stores no object,
//! while one that repeats the upload's mode (or states none at all) completes. The refusal is
//! about the MODE whatever digest the request also declares: a full-object digest offered to a
//! composite upload is the wrong mode, not a wrong value, so the answer must not point at a
//! digest mismatch and send the caller back with the same mode.
//!
//! The rows differ along what the rule generalises over: the ALGORITHM the upload was created
//! under (one that settles a composition, one that admits a full-object checksum, the one that
//! is always a full-object checksum, and none at all), HOW the upload's mode was settled (from
//! the algorithm or asked for explicitly), WHICH mode the completion states (each of the two
//! values the enum publishes, and none), and whether the completion also declares an
//! object-level digest.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::multipart_part_checksums::digest;
use super::object_and_tagging_roundtrip::{bucket, text};

/// The two values `ChecksumType` publishes
/// (`smithy_query(query="shape",shape="com.amazonaws.s3#ChecksumType")` reports `[COMPOSITE,
/// FULL_OBJECT]`) — the whole space a completion may state.
const COMPOSITE: &str = "COMPOSITE";
/// See [`COMPOSITE`].
const FULL_OBJECT: &str = "FULL_OBJECT";

/// The code S3 answers "a parameter or header in your request isn't valid" with, at HTTP 400
/// (Amazon S3 *Error responses*, list of error codes).
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code S3 answers a declared digest that does not describe what it received with — the
/// answer this refusal must NOT be, because nothing is wrong with the value.
const BAD_DIGEST: &str = "BadDigest";

/// The bytes every upload of this suite is completed from — one part, which the published
/// minimum part size does not apply to ("There is no minimum size limit on the last part of
/// your multipart upload", Amazon S3 User Guide, *Amazon S3 multipart upload limits*).
const PART: &[u8] = b"one part, completed under a checksum mode";

/// The sentence S3 refuses a completion stating the wrong mode with: fixed text around the
/// mode the UPLOAD was created with. Written out here from the published wording rather than
/// read off the handler, so a wording regression fails this suite.
fn mismatched_mode_message(upload_mode: &str) -> String {
    format!(
        "The upload was created using the {upload_mode} checksum mode. The complete request \
         must use the same checksum mode."
    )
}

/// `POST /<bucket>/<key>?uploads` — `CreateMultipartUpload`, carrying the checksum headers the
/// row names (each absent when the row names none).
async fn initiate(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    algorithm: Option<&str>,
    requested_type: Option<&str>,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(algorithm) = algorithm {
        headers.push(("x-amz-checksum-algorithm", algorithm));
    }
    if let Some(requested) = requested_type {
        headers.push(("x-amz-checksum-type", requested));
    }
    send(router, "POST", &format!("/{name}/{key}?uploads"), &headers, "").await
}

/// `PUT /<bucket>/<key>?partNumber=1&uploadId=<id>` — `UploadPart`, answering the entity tag
/// the store recorded for [`PART`].
async fn upload_part(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    upload_id: &str,
) -> String {
    let uploaded = send_bytes(
        router,
        "PUT",
        &format!("/{name}/{key}?partNumber=1&uploadId={upload_id}&x-id=UploadPart"),
        &[],
        PART,
    )
    .await;
    assert_eq!(
        uploaded.status,
        200,
        "uploading the part must answer HTTP 200, got {} with body {:?}",
        uploaded.status,
        text(&uploaded)
    );
    uploaded
        .header("etag")
        .expect("a part upload must report an ETag header")
}

/// What a completion STATES about the checksum, beside the upload it addresses: the mode, the
/// per-part digest its manifest carries and the object-level digest it declares (each absent
/// where the row states none).
struct Stated<'a> {
    /// The `x-amz-checksum-type` the completion states.
    mode: Option<&'a str>,
    /// The algorithm each named part carries its own digest under in the manifest.
    part_checksum_algorithm: Option<&'a str>,
    /// The algorithm the completion declares the WHOLE object's digest under.
    object_checksum_algorithm: Option<&'a str>,
}

/// `POST /<bucket>/<key>?uploadId=<id>` — `CompleteMultipartUpload` stating what `stated`
/// describes and naming the one part by `etag`.
async fn complete(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    upload_id: &str,
    etag: &str,
    stated: &Stated<'_>,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(mode) = stated.mode {
        headers.push(("x-amz-checksum-type", mode));
    }
    // The object-level digest a client declares as "a data integrity check to verify that the
    // data received is the same data that was originally sent"
    // (`CompleteMultipartUploadRequest$ChecksumCRC64NVME` and its siblings): the digest of the
    // whole object, which is what a client asking for a full-object checksum sends.
    let object_header = stated.object_checksum_algorithm.map(|algorithm| {
        (
            format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase()),
            digest(algorithm, PART),
        )
    });
    if let Some((header, value)) = &object_header {
        headers.push((header.as_str(), value.as_str()));
    }
    let part_checksum = stated
        .part_checksum_algorithm
        .map(|algorithm| {
            format!(
                "<Checksum{algorithm}>{}</Checksum{algorithm}>",
                digest(algorithm, PART)
            )
        })
        .unwrap_or_default();
    let payload = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{etag}</ETag>\
         {part_checksum}</Part></CompleteMultipartUpload>"
    );
    send(
        router,
        "POST",
        &format!("/{name}/{key}?uploadId={upload_id}"),
        &headers,
        &payload,
    )
    .await
}

/// One row: what the upload was created with, what the completion states, and the mode the
/// upload therefore carries.
struct Row {
    case: &'static str,
    /// The `x-amz-checksum-algorithm` the initiation named, if any.
    created_algorithm: Option<&'static str>,
    /// The `x-amz-checksum-type` the initiation asked for, if any.
    created_type: Option<&'static str>,
    /// The `x-amz-checksum-type` the completion states, if any.
    stated_type: Option<&'static str>,
    /// Whether the completion also declares the OBJECT's own digest, under the algorithm the
    /// upload was created with.
    declares_object_digest: bool,
    /// The mode the upload carries — what a served completion reports and what a refused one
    /// names. For an upload created under no algorithm at all it is the mode of the algorithm
    /// every object S3 stores is protected by, whose checksum "is always a full object
    /// checksum" (`ChecksumCRC64NVME`).
    upload_mode: &'static str,
}

/// The table: one row per (created, stated) pair the rule distinguishes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a composite upload completed as a full-object one",
            created_algorithm: Some("CRC32"),
            created_type: None,
            stated_type: Some(FULL_OBJECT),
            declares_object_digest: false,
            upload_mode: COMPOSITE,
        },
        Row {
            case: "a composite upload completed as a full-object one, digest and all",
            created_algorithm: Some("CRC32"),
            created_type: None,
            stated_type: Some(FULL_OBJECT),
            declares_object_digest: true,
            upload_mode: COMPOSITE,
        },
        Row {
            case: "a composite upload under a hash algorithm completed as a full-object one",
            created_algorithm: Some("SHA256"),
            created_type: Some(COMPOSITE),
            stated_type: Some(FULL_OBJECT),
            declares_object_digest: false,
            upload_mode: COMPOSITE,
        },
        Row {
            case: "a full-object upload completed as a composite one",
            created_algorithm: Some("CRC32C"),
            created_type: Some(FULL_OBJECT),
            stated_type: Some(COMPOSITE),
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
        Row {
            case: "the always-full-object algorithm completed as a composite one",
            created_algorithm: Some("CRC64NVME"),
            created_type: None,
            stated_type: Some(COMPOSITE),
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
        Row {
            case: "an upload created under no algorithm completed as a composite one",
            created_algorithm: None,
            created_type: None,
            stated_type: Some(COMPOSITE),
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
        Row {
            case: "a composite upload completed as the composite one it is",
            created_algorithm: Some("CRC32"),
            created_type: Some(COMPOSITE),
            stated_type: Some(COMPOSITE),
            declares_object_digest: false,
            upload_mode: COMPOSITE,
        },
        Row {
            case: "a full-object upload completed as the full-object one it is",
            created_algorithm: Some("CRC32C"),
            created_type: Some(FULL_OBJECT),
            stated_type: Some(FULL_OBJECT),
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
        Row {
            case: "a composite upload completed stating no mode at all",
            created_algorithm: Some("SHA1"),
            created_type: None,
            stated_type: None,
            declares_object_digest: false,
            upload_mode: COMPOSITE,
        },
        Row {
            case: "an upload created under no algorithm completed stating no mode at all",
            created_algorithm: None,
            created_type: None,
            stated_type: None,
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
        Row {
            case: "an upload created under no algorithm completed as a full-object one",
            created_algorithm: None,
            created_type: None,
            stated_type: Some(FULL_OBJECT),
            declares_object_digest: false,
            upload_mode: FULL_OBJECT,
        },
    ]
}

/// RULE: a completion may state only the checksum mode its upload was created with; any other
/// mode is refused 400 `InvalidRequest` naming the upload's mode, and stores no object.
#[tokio::test]
async fn a_completion_may_only_state_the_mode_its_upload_was_created_with() {
    let router = router();
    let name = "multipart-complete-mode-bucket";
    bucket(&router, name).await;

    for (index, row) in rows().iter().enumerate() {
        let key = format!("completed/{index}.bin");
        let initiated = initiate(
            &router,
            name,
            &key,
            row.created_algorithm,
            row.created_type,
        )
        .await;
        assert_eq!(
            initiated.status,
            200,
            "case {:?}: the initiation must answer HTTP 200, got {} with body {:?}",
            row.case,
            initiated.status,
            text(&initiated)
        );
        let upload_id = initiated.element("UploadId").unwrap_or_else(|| {
            panic!("case {:?}: an initiation must report an UploadId", row.case)
        });
        let etag = upload_part(&router, name, &key, &upload_id).await;

        // A completion of a COMPOSITE upload names each part's own digest, which is what its
        // object-level checksum is composed of; one of an upload under no algorithm, or of a
        // full-object upload, is asked for none.
        let part_checksum_algorithm = row
            .created_algorithm
            .filter(|_| row.upload_mode == COMPOSITE);
        let object_checksum_algorithm = row
            .created_algorithm
            .filter(|_| row.declares_object_digest);
        let answered = complete(
            &router,
            name,
            &key,
            &upload_id,
            &etag,
            &Stated {
                mode: row.stated_type,
                part_checksum_algorithm,
                object_checksum_algorithm,
            },
        )
        .await;
        let body = text(&answered);
        let mismatched = row
            .stated_type
            .is_some_and(|stated| stated != row.upload_mode);

        if mismatched {
            assert_eq!(
                answered.status, 400,
                "case {:?}: expected the completion to be refused 400, got {} with body {body:?}",
                row.case, answered.status
            );
            assert_eq!(
                answered.error_code(),
                INVALID_REQUEST,
                "case {:?}: expected the code {INVALID_REQUEST} rather than {BAD_DIGEST}, got \
                 {:?} in body {body:?}",
                row.case,
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                mismatched_mode_message(row.upload_mode),
                "case {:?}: expected the sentence naming the upload's mode, got {:?}",
                row.case,
                answered.error_message()
            );
            // Nothing was stored: the key the completion addressed holds no object at all,
            // and the upload is still there to be completed with the right mode.
            let read = send(&router, "HEAD", &format!("/{name}/{key}"), &[], "").await;
            assert_eq!(
                read.status, 404,
                "case {:?}: a refused completion must store no object, got {} for the read",
                row.case, read.status
            );
            let parts = send(
                &router,
                "GET",
                &format!("/{name}/{key}?uploadId={upload_id}&x-id=ListParts"),
                &[],
                "",
            )
            .await;
            assert_eq!(
                parts.status, 200,
                "case {:?}: a refused completion must leave the upload in progress, got {} with \
                 body {:?}",
                row.case,
                parts.status,
                text(&parts)
            );
            continue;
        }

        assert_eq!(
            answered.status, 200,
            "case {:?}: expected the completion to answer HTTP 200, got {} with body {body:?}",
            row.case, answered.status
        );
        assert_eq!(
            answered.element("ChecksumType").as_deref(),
            Some(row.upload_mode),
            "case {:?}: expected the completion to report the upload's mode, got {:?}",
            row.case,
            answered.element("ChecksumType")
        );
        // And the object really is there afterwards, which is what makes the refusals above
        // the only thing that stopped the other rows.
        let read = send(&router, "HEAD", &format!("/{name}/{key}"), &[], "").await;
        assert_eq!(
            read.status, 200,
            "case {:?}: a served completion must store the object, got {} for the read",
            row.case, read.status
        );
    }
}
