// SPDX-License-Identifier: CC0-1.0

//! The SSE-KMS ENCRYPTION CONTEXT an object was stored under, read back.
//!
//! The rule under test: **the encryption context an object's data key is bound to is reported
//! on `x-amz-server-side-encryption-context` by every answer ABOUT that object, and the value
//! is the whole context — the pairs the write supplied plus the entry S3 binds naming what was
//! encrypted (`aws:s3:arn`), Base64-encoded JSON** (Amazon S3 User Guide, *Using server-side
//! encryption with KMS keys*: S3 "uses the object or bucket Amazon Resource Name (ARN) as the
//! encryption context pair"). The WRITES answer it through a modelled member
//! (`PutObjectOutput$SSEKMSEncryptionContext`), while `GetObjectOutput` and `HeadObjectOutput`
//! declare no member for it (`smithy_query(query="operation",operation="GetObject,HeadObject")`)
//! — so what this suite pins is that the two READS report the same value the write did: a
//! caller that must verify which context its data was wrapped under, or reproduce it to
//! decrypt out of band, can only do so if a read carries it.
//!
//! Its other half is the value the header may CARRY: "a Base64-encoded string holding a UTF-8
//! encoded JSON of additional encryption context key-value pairs"
//! (`PutObjectRequest$SSEKMSEncryptionContext`), so a value that is not an encoded
//! string-string map is one KMS could never be handed — refused `InvalidArgument` (400) with
//! the sentence that refusal publishes, the `<ArgumentName>` naming the header and the
//! `<ArgumentValue>` echoing what was sent, and nothing stored. Every write that accepts the
//! header is judged by it.
//!
//! The rows differ along what the rules generalise over: how many pairs the write supplied
//! (one, several, none), the KEY the object is addressed by (a flat name and one with `/`
//! separators, each changing the ARN entry), whether an S3 Bucket Key binds the data key to
//! the BUCKET instead of the object, which READ reports it (a GET and a HEAD), and — for the
//! refusal — HOW the value fails to be an encoded map, over each write that accepts it.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{send, send_bytes, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::kms_key_references::{copy_object, create_multipart_upload, created_key, router_with_kms};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_encryption::{decoded_pairs, ARN_CONTEXT_KEY};

/// The bucket every object of this suite is stored in.
const BUCKET: &str = "sse-kms-context-read-back";

/// The DIRECTORY bucket a session is created on, and the Availability Zone it is placed in: a
/// session's own encryption settings carry the same header, and "Amazon S3 will use the
/// encryption settings values from the CreateSession request to protect new objects in the
/// directory bucket" (`CreateSession`'s own documentation).
const SESSION_BUCKET: &str = "ssekmscontext--use1-az4--x-s3";
/// See [`SESSION_BUCKET`].
const SESSION_ZONE: &str = "use1-az4";

/// The header the context travels on, on a write's answer and on a read's alike.
const CONTEXT_HEADER: &str = "x-amz-server-side-encryption-context";

/// The bytes every object here holds: the context is about the data key, not about the bytes,
/// so one payload serves every row.
const BODY: &[u8] = b"sse-kms context read-back payload";

/// The code and status S3 answers a value the header may not carry with (Amazon S3 *Error
/// responses*: `InvalidArgument`, HTTP 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The sentence that refusal carries, written out here rather than imported so a wording
/// regression fails this suite.
const CONTEXT_MESSAGE: &str = "The header 'x-amz-server-side-encryption-context' shall be \
     Base64-encoded UTF-8 string holding JSON which represents a string-string map";

/// `HEAD /<bucket>/<key>` — `HeadObject`.
async fn head_object(router: &MultiServiceRouter, key: &str) -> Answer {
    send_bytes(router, "HEAD", &format!("/{BUCKET}/{key}"), &[], b"").await
}

/// One row of the read-back table: the case, the key written, the pairs the write supplies,
/// whether an S3 Bucket Key is in force, and the ARN the bound entry must name.
struct ContextRow {
    case: &'static str,
    key: &'static str,
    supplied: &'static [(&'static str, &'static str)],
    bucket_key: bool,
}

/// The rows: how many pairs are supplied, the shape of the key, and whether a Bucket Key binds
/// the context to the bucket.
fn rows() -> &'static [ContextRow] {
    &[
        ContextRow {
            case: "one supplied pair",
            key: "context/one.txt",
            supplied: &[("team", "sim2real")],
            bucket_key: false,
        },
        ContextRow {
            case: "several supplied pairs",
            key: "context/many.txt",
            supplied: &[("team", "sim2real"), ("zone", "a"), ("tier", "gold")],
            bucket_key: false,
        },
        ContextRow {
            case: "a key carrying / separators",
            key: "context/nested/dir/one.txt",
            supplied: &[("team", "sim2real")],
            bucket_key: false,
        },
        ContextRow {
            case: "an S3 Bucket Key, which binds the data key to the bucket",
            key: "context/bucket-key.txt",
            supplied: &[("team", "sim2real")],
            bucket_key: true,
        },
        ContextRow {
            case: "no context supplied at all",
            key: "context/none.txt",
            supplied: &[],
            bucket_key: false,
        },
    ]
}

/// The Base64 of the JSON object `pairs` spells, which is what the header carries.
fn encoded(pairs: &[(&str, &str)]) -> String {
    let document = serde_json::to_vec(&pairs.iter().copied().collect::<
        std::collections::BTreeMap<&str, &str>,
    >())
    .expect("the pairs must serialize");
    aws_smithy_types::base64::encode(&document)
}

/// The ARN the bound entry names for `row`: the BUCKET's for a write using an S3 Bucket Key —
/// one data key per bucket rather than per object — and the OBJECT's otherwise.
fn bound_arn(row: &ContextRow) -> String {
    match row.bucket_key {
        true => format!("arn:aws:s3:::{BUCKET}"),
        false => format!("arn:aws:s3:::{BUCKET}/{}", row.key),
    }
}

/// What the context of `row` must decode to: the pairs it supplied plus the bound entry.
fn expected_pairs(row: &ContextRow) -> std::collections::BTreeMap<String, String> {
    let mut expected: std::collections::BTreeMap<String, String> = row
        .supplied
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    expected.insert(ARN_CONTEXT_KEY.to_string(), bound_arn(row));
    expected
}

#[tokio::test]
async fn every_read_of_an_sse_kms_object_reports_the_context_its_data_key_is_bound_to() {
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these writes name").await;
    bucket(&router, BUCKET).await;

    for row in rows() {
        let case = row.case;
        let supplied = encoded(row.supplied);
        let mut headers: Vec<(&str, &str)> = vec![
            ("x-amz-server-side-encryption", "aws:kms"),
            ("x-amz-server-side-encryption-aws-kms-key-id", &key_arn),
        ];
        if !row.supplied.is_empty() {
            headers.push((CONTEXT_HEADER, supplied.as_str()));
        }
        if row.bucket_key {
            headers.push(("x-amz-server-side-encryption-bucket-key-enabled", "true"));
        }
        let written = put_object(&router, BUCKET, row.key, BODY, &headers).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the write must be served, got {} with body {:?}",
            written.status,
            text(&written)
        );

        // The write's own answer and each of the two reads report the SAME value: a modelled
        // member on the write, a header no member binds on the reads.
        let read = get_object(&router, BUCKET, row.key, "", &[]).await;
        let head = head_object(&router, row.key).await;
        for (answer, operation) in [
            (&written, "PutObject"),
            (&read, "GetObject"),
            (&head, "HeadObject"),
        ] {
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: {operation} must be served, got {} with body {:?}",
                answer.status,
                text(answer)
            );
            let reported = answer.header(CONTEXT_HEADER);
            if row.supplied.is_empty() {
                // "The encryption context as key-value pairs" a REQUEST named: a write that
                // supplied none is reported none at all rather than the bound entry alone.
                assert_eq!(
                    reported, None,
                    "case {case:?}: {operation} must report no context, got {reported:?}"
                );
                continue;
            }
            let reported =
                reported.unwrap_or_else(|| panic!("case {case:?}: {operation} reported no context"));
            assert_eq!(
                decoded_pairs(&reported),
                expected_pairs(row),
                "case {case:?}: {operation} must report the whole bound context, got {:?}",
                decoded_pairs(&reported)
            );
            // And the key the data was wrapped under travels beside it, as the same answer's
            // own `x-amz-server-side-encryption-aws-kms-key-id`.
            assert_eq!(
                answer
                    .header("x-amz-server-side-encryption-aws-kms-key-id")
                    .as_deref(),
                Some(key_arn.as_str()),
                "case {case:?}: {operation} must name the key, got {:?}",
                answer.header("x-amz-server-side-encryption-aws-kms-key-id")
            );
        }
    }
}

/// HOW a refused value fails to be an encoded string-string map, and the value it sends.
fn malformed_values() -> Vec<(&'static str, String)> {
    vec![
        ("text that is not Base64 at all", String::from("not base64 at all!")),
        (
            "Base64 of text that is not JSON",
            aws_smithy_types::base64::encode(b"team=sim2real"),
        ),
        (
            "Base64 of a JSON array rather than an object",
            aws_smithy_types::base64::encode(br#"["team"]"#),
        ),
        (
            "Base64 of a JSON object whose value is not a string",
            aws_smithy_types::base64::encode(br#"{"team":7}"#),
        ),
        (
            "Base64 of a JSON scalar",
            aws_smithy_types::base64::encode(br#""team""#),
        ),
        (
            "Base64 of bytes that are not UTF-8",
            aws_smithy_types::base64::encode([0xff_u8, 0xfe, 0xfd]),
        ),
    ]
}

/// WHICH write carries the header in a case of the refusal table.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `PutObject`.
    OneShot,
    /// `CopyObject`, whose destination object is stored under the context it names.
    Copy,
    /// `CreateMultipartUpload`, which settles the encryption every part of the upload is
    /// stored under.
    Initiation,
    /// `CreateSession`, whose settings protect every object written in the session.
    Session,
}

#[tokio::test]
async fn a_context_that_is_not_an_encoded_string_string_map_is_refused_by_every_write() {
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these writes name").await;
    bucket(&router, BUCKET).await;
    directory_bucket(&router, SESSION_BUCKET, SESSION_ZONE).await;
    let source = put_object(&router, BUCKET, "refusal-source.bin", BODY, &[]).await;
    assert_eq!(
        source.status, 200,
        "the copy source must be stored first, got {} with body {:?}",
        source.status,
        text(&source)
    );

    for write in [Write::OneShot, Write::Copy, Write::Initiation, Write::Session] {
        for (index, (fault, value)) in malformed_values().into_iter().enumerate() {
            let case = format!("{write:?} with {fault}");
            let key = format!("refused/{write:?}/{index}.bin");
            let headers: Vec<(&str, &str)> = vec![
                ("x-amz-server-side-encryption", "aws:kms"),
                ("x-amz-server-side-encryption-aws-kms-key-id", &key_arn),
                (CONTEXT_HEADER, value.as_str()),
            ];
            let refused = match write {
                Write::OneShot => put_object(&router, BUCKET, &key, BODY, &headers).await,
                Write::Copy => {
                    copy_object(
                        &router,
                        BUCKET,
                        &key,
                        &format!("{BUCKET}/refusal-source.bin"),
                        &headers,
                    )
                    .await
                }
                Write::Initiation => {
                    create_multipart_upload(&router, BUCKET, &key, &headers).await
                }
                Write::Session => {
                    send(
                        &router,
                        "GET",
                        &format!("/{SESSION_BUCKET}?session"),
                        &headers,
                        "",
                    )
                    .await
                }
            };

            assert_eq!(
                refused.status, 400,
                "case {case:?}: expected HTTP 400, got {} with body {:?}",
                refused.status,
                text(&refused)
            );
            assert_eq!(
                refused.error_code(),
                INVALID_ARGUMENT,
                "case {case:?}: expected the {INVALID_ARGUMENT} code, got {:?}",
                refused.error_code()
            );
            assert_eq!(
                refused.error_message(),
                CONTEXT_MESSAGE,
                "case {case:?}: expected the published sentence, got {:?}",
                refused.error_message()
            );
            assert_eq!(
                refused.element("ArgumentName").as_deref(),
                Some(CONTEXT_HEADER),
                "case {case:?}: the document must name the header, got {:?}",
                refused.element("ArgumentName")
            );
            assert_eq!(
                refused.element("ArgumentValue").as_deref(),
                Some(value.as_str()),
                "case {case:?}: the document must echo the value as sent, got {:?}",
                refused.element("ArgumentValue")
            );
            // Nothing is stored, and no upload is left in progress to store parts under a
            // context nothing could decode.
            let read = get_object(&router, BUCKET, &key, "", &[]).await;
            assert_eq!(
                read.status, 404,
                "case {case:?}: a refused write must store nothing, got {} with body {:?}",
                read.status,
                text(&read)
            );
        }
    }
}
