// SPDX-License-Identifier: CC0-1.0

//! The ONE form of `If-None-Match` a write admits.
//!
//! The rule under test: **a write's `If-None-Match` admits only the wildcard — every member
//! bound to that header on a write says "expects the '*' (asterisk) character"
//! (`PutObjectRequest$IfNoneMatch`, `CopyObjectRequest$IfNoneMatch`,
//! `CompleteMultipartUploadRequest$IfNoneMatch`, and `RenameObjectRequest$DestinationIfNoneMatch`
//! as "expects the `*` character (asterisk)") — so a value naming a specific entity tag asks
//! for a write conditioned on WHICH representation the key holds, which is functionality the
//! header implies and the operation does not implement: HTTP 501 `NotImplemented` with the
//! fixed sentence that code publishes for a header, the `<Header>` naming the one the caller
//! must drop, and nothing written.**
//!
//! What the wildcard itself asks is unchanged and is pinned beside it on every row: the key
//! must hold NOTHING, so a write carrying `If-None-Match: *` onto a free key is served and
//! stores its object (what a key that already holds one earns — the documented 412 — is
//! [`super::object_conditional_writes`]'s table).
//!
//! The rows differ along what the rule generalises over: the FORM the value takes (the
//! wildcard, the wildcard with surrounding whitespace, the entity tag the destination would
//! hold, that tag unquoted, another object's tag, a tag list, the weak form of a tag, and the
//! wildcard inside quotes, which is a tag of its own and not the wildcard), and the WRITE that
//! carries it — the one-shot write, a copy's destination, the completion of a multipart upload
//! and a directory bucket's rename are judged by the one rule.

use service_com_amazonaws_s3_amazons3::state::{
    NOT_IMPLEMENTED_CODE, NOT_IMPLEMENTED_HEADER_MESSAGE, NOT_IMPLEMENTED_STATUS,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_partial_reads::seeded_payload;

/// The general purpose bucket the one-shot writes, the copies and the completions address.
const BUCKET: &str = "if-none-match-wildcard";

/// The directory bucket the renames address, and the Availability Zone it is placed in — the
/// name shape such a bucket carries (`<name>--<az>--x-s3`).
const DIRECTORY_BUCKET: &str = "ifnonematchrename--use1-az4--x-s3";
/// See [`DIRECTORY_BUCKET`].
const DIRECTORY_ZONE: &str = "use1-az4";

/// The header every one of these members is bound to, which is what the refusal's `<Header>`
/// element names.
const IF_NONE_MATCH: &str = "If-None-Match";

/// A well-formed entity tag that is no object of this suite's: a 32-character hexadecimal
/// digest inside the quotes HTTP wraps a tag in.
const OTHER_TAG: &str = "\"00000000000000000000000000000000\"";

/// The bytes a copy's source object and a rename's source object hold.
const SOURCE: &[u8] = b"the bytes a conditional write moves";

/// One row: the case, the value `If-None-Match` carries, and whether that value is the
/// wildcard the member expects (so the write is served onto a key holding nothing).
struct Row {
    case: &'static str,
    value: &'static str,
    wildcard: bool,
}

/// The value forms the rule spans.
fn rows() -> &'static [Row] {
    &[
        Row {
            case: "the wildcard the member expects",
            value: "*",
            wildcard: true,
        },
        Row {
            case: "the wildcard with surrounding whitespace",
            value: " * ",
            wildcard: true,
        },
        Row {
            case: "a specific entity tag",
            value: OTHER_TAG,
            wildcard: false,
        },
        Row {
            case: "that same tag unquoted",
            value: "00000000000000000000000000000000",
            wildcard: false,
        },
        Row {
            case: "a list of two entity tags",
            value: "\"11111111111111111111111111111111\", \
                    \"00000000000000000000000000000000\"",
            wildcard: false,
        },
        Row {
            case: "the weak form of an entity tag",
            value: "W/\"00000000000000000000000000000000\"",
            wildcard: false,
        },
        Row {
            case: "the asterisk inside quotes, which is a tag of its own",
            value: "\"*\"",
            wildcard: false,
        },
        Row {
            case: "an entity tag of another width entirely",
            value: "\"not-a-digest\"",
            wildcard: false,
        },
    ]
}

/// WHICH write a case drives, each carrying the member on a key that holds nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// `PUT /<bucket>/<key>` — `PutObject`.
    OneShot,
    /// `PUT /<bucket>/<key>` carrying `x-amz-copy-source` — `CopyObject`, whose own
    /// `If-None-Match` makes the DESTINATION write conditional.
    Copy,
    /// `POST /<bucket>/<key>?uploadId=…` — the completion of a multipart upload, which is
    /// where that upload's conditional members travel.
    Completion,
    /// `PUT /<bucket>/<key>?renameObject` — `RenameObject`, whose `DestinationIfNoneMatch`
    /// carries the condition.
    Rename,
}

/// Every write that carries the member.
const WRITES: [Write; 4] = [Write::OneShot, Write::Copy, Write::Completion, Write::Rename];

impl Write {
    /// The bucket this write is addressed to.
    fn bucket(self) -> &'static str {
        match self {
            // A rename "renames an existing object in a directory bucket"
            // (`RenameObject`'s own documentation), so only a directory bucket serves one.
            Self::Rename => DIRECTORY_BUCKET,
            _ => BUCKET,
        }
    }
}

/// The key the source object of a copy is held at, and the key a rename's source is written
/// at for one row (its own, so no row depends on what another left behind).
fn source_key(write: Write, index: usize) -> String {
    match write {
        Write::Rename => format!("rename-source-{index}.bin"),
        _ => String::from("copy-source.bin"),
    }
}

/// Drive `write` at `key` with `If-None-Match: value`, from a source written beforehand where
/// the write needs one. Answers what the write answered.
async fn conditional_write(
    router: &MultiServiceRouter,
    write: Write,
    key: &str,
    source: &str,
    value: &str,
) -> Answer {
    let bucket_name = write.bucket();
    match write {
        Write::OneShot => {
            put_object(
                router,
                bucket_name,
                key,
                SOURCE,
                &[(IF_NONE_MATCH, value)],
            )
            .await
        }
        Write::Copy => {
            send_bytes(
                router,
                "PUT",
                &format!("/{bucket_name}/{key}"),
                &[
                    ("x-amz-copy-source", &format!("{bucket_name}/{source}")),
                    (IF_NONE_MATCH, value),
                ],
                b"",
            )
            .await
        }
        Write::Completion => {
            let initiated = send(
                router,
                "POST",
                &format!("/{bucket_name}/{key}?uploads"),
                &[],
                "",
            )
            .await;
            assert_eq!(
                initiated.status,
                200,
                "the initiation of {bucket_name}/{key} must be served, got {} with body {:?}",
                initiated.status,
                text(&initiated)
            );
            let upload_id = initiated
                .element("UploadId")
                .expect("an initiation must report an UploadId");
            let part = seeded_payload(7, 64);
            let uploaded = send_bytes(
                router,
                "PUT",
                &format!("/{bucket_name}/{key}?partNumber=1&uploadId={upload_id}"),
                &[],
                &part,
            )
            .await;
            assert_eq!(
                uploaded.status,
                200,
                "the part upload must be served, got {} with body {:?}",
                uploaded.status,
                text(&uploaded)
            );
            let etag = uploaded
                .header("etag")
                .expect("a part upload must report an ETag");
            send(
                router,
                "POST",
                &format!("/{bucket_name}/{key}?uploadId={upload_id}"),
                &[(IF_NONE_MATCH, value)],
                &format!(
                    "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber>\
                     <ETag>{etag}</ETag></Part></CompleteMultipartUpload>"
                ),
            )
            .await
        }
        Write::Rename => {
            send(
                router,
                "PUT",
                &format!("/{bucket_name}/{key}?renameObject"),
                &[
                    ("x-amz-rename-source", source),
                    (IF_NONE_MATCH, value),
                ],
                "",
            )
            .await
        }
    }
}

#[tokio::test]
async fn a_write_whose_if_none_match_names_a_tag_is_refused_for_the_header() {
    let router = router();
    bucket(&router, BUCKET).await;
    directory_bucket(&router, DIRECTORY_BUCKET, DIRECTORY_ZONE).await;
    let staged = put_object(&router, BUCKET, "copy-source.bin", SOURCE, &[]).await;
    assert_eq!(
        staged.status,
        200,
        "the copy source must be stored first, got {} with body {:?}",
        staged.status,
        text(&staged)
    );

    for write in WRITES {
        for (index, row) in rows().iter().enumerate() {
            let Row {
                case,
                value,
                wildcard,
            } = row;
            let case = format!("{write:?} with {case}");
            let key = format!("conditional/{write:?}/{index}.bin");
            let source = source_key(write, index);
            if write == Write::Rename {
                let written =
                    put_object(&router, write.bucket(), &source, SOURCE, &[]).await;
                assert_eq!(
                    written.status,
                    200,
                    "case {case:?}: the rename source must be stored first, got {} with body \
                     {:?}",
                    written.status,
                    text(&written)
                );
            }

            let answered =
                conditional_write(&router, write, &key, &source, value).await;
            let read = get_object(&router, write.bucket(), &key, "", &[]).await;

            if *wildcard {
                // The wildcard asks that the key hold nothing, which it does, so the write
                // lands — the boundary that keeps the refusal about the VALUE rather than
                // about the member.
                assert_eq!(
                    answered.status,
                    200,
                    "case {case:?}: the wildcard onto a free key must be served, got {} with \
                     body {:?}",
                    answered.status,
                    text(&answered)
                );
                assert_eq!(
                    read.status,
                    200,
                    "case {case:?}: the served write must have stored its object, got {}",
                    read.status
                );
                continue;
            }

            assert_eq!(
                answered.status,
                NOT_IMPLEMENTED_STATUS,
                "case {case:?}: expected HTTP {NOT_IMPLEMENTED_STATUS}, got {} with body {:?}",
                answered.status,
                text(&answered)
            );
            assert_eq!(
                answered.error_code(),
                NOT_IMPLEMENTED_CODE,
                "case {case:?}: expected the {NOT_IMPLEMENTED_CODE} code, got {:?}",
                answered.error_code()
            );
            assert_eq!(
                answered.error_message(),
                NOT_IMPLEMENTED_HEADER_MESSAGE,
                "case {case:?}: expected the published sentence, got {:?}",
                answered.error_message()
            );
            assert_eq!(
                answered.element("Header").as_deref(),
                Some(IF_NONE_MATCH),
                "case {case:?}: the document must name the header that asked, got {:?}",
                answered.element("Header")
            );
            assert_eq!(
                read.status,
                404,
                "case {case:?}: a refused write must store nothing, got {} with body {:?}",
                read.status,
                text(&read)
            );
            if write == Write::Rename {
                // A refused rename leaves the object where it was, rather than half-moving it.
                let left = get_object(&router, write.bucket(), &source, "", &[]).await;
                assert_eq!(
                    left.status,
                    200,
                    "case {case:?}: a refused rename must leave its source in place, got {}",
                    left.status
                );
            }
        }
    }
}
