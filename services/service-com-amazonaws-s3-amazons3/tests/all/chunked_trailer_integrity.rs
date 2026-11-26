// SPDX-License-Identifier: CC0-1.0

//! What a write whose body arrives in the `aws-chunked` transfer encoding with a TRAILING
//! checksum is held to.
//!
//! The rule under test: **a checksum value a write declares has to describe the bytes the
//! request carried, wherever the request declares it** — and for a framed body it is declared
//! in the trailing header `x-amz-trailer` names ("if you specify `x-amz-trailer` … the
//! checksum is sent in the trailing header"), which says exactly the same thing about the
//! object as an `x-amz-checksum-*` header sent up front. So:
//!
//! * a trailing value that is the digest of the payload is served, and the object is stored
//!   under that algorithm — every later read reports the trailer's own value;
//! * a trailing value that is NOT that digest is the `BadDigest` (400) every declared checksum
//!   of this service that does not describe its bytes earns, whose sentence names the
//!   algorithm the request declared, and nothing is stored;
//! * a trailing value that is not a digest at all — text that is not Base64, or Base64 of the
//!   wrong width — is the `InvalidRequest` (400) a malformed checksum value earns, naming the
//!   header it arrived on, and nothing is stored;
//! * trailing headers that are not the ones `x-amz-trailer` declared are trailing data that
//!   does not conform to the framing the request announced: `MalformedTrailerError` (400), and
//!   nothing is stored.
//!
//! The boundary of the rule is pinned too: the trailer is judged only for a body that really
//! is framed, so a request that declares `x-amz-trailer` and sends an unframed payload carries
//! no trailing data to be held to and is served; and the trailing line a SIGNED streaming
//! payload's framing appends after the declared trailers — the final signature of its chunk
//! chain, which `x-amz-trailer` never names — is not trailing data the request has to have
//! declared.
//!
//! The rows differ along what the rule generalises over: WHICH algorithm the trailer declares
//! (four of the ten the model publishes, over both digest widths), HOW the trailing data fails
//! (a value that is another payload's digest, a value that is not Base64, a value of the wrong
//! width, a name the request did not declare), and the SIZE of the payload the digest is over
//! (a few bytes, and several kilobytes in one chunk).

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, text};

/// The bucket every write in this suite is addressed to.
const BUCKET: &str = "chunked-trailer-integrity";

/// The `x-amz-content-sha256` marker an SDK streaming a payload with a trailing checksum
/// declares.
const STREAMING_TRAILER_MARKER: &str = "STREAMING-UNSIGNED-PAYLOAD-TRAILER";

/// The code S3 answers a digest that does not describe the bytes with, and the status it
/// carries (Amazon S3 *Error responses*: `BadDigest`, HTTP 400). Spelled out here rather than
/// imported, so a wording regression fails this suite.
const BAD_DIGEST: &str = "BadDigest";

/// The code a value that is not a digest at all earns from the same list.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code and sentence S3 answers trailing data that does not conform to the framing with.
const MALFORMED_TRAILER: &str = "MalformedTrailerError";
/// See [`MALFORMED_TRAILER`].
const MALFORMED_TRAILER_MESSAGE: &str = "The request contained trailing data that was not \
     well-formed or did not conform to our published schema.";

/// `payload` in the `aws-chunked` transfer encoding: one chunk of its own length, the
/// zero-length chunk that terminates the stream, and one trailing header `name: value`.
fn framed(payload: &[u8], name: &str, value: &str) -> Vec<u8> {
    let mut body = format!("{:x}\r\n", payload.len()).into_bytes();
    body.extend_from_slice(payload);
    body.extend_from_slice(b"\r\n0\r\n");
    body.extend_from_slice(format!("{name}:{value}\r\n\r\n").as_bytes());
    body
}

/// The trailing header a SIGNED streaming payload's framing appends after the trailers the
/// request declared: the final signature of its chunk chain, which SigV4 defines and
/// `x-amz-trailer` never names.
const TRAILER_SIGNATURE: &str = "x-amz-trailer-signature";

/// [`framed`] with that signature line after the declared trailer, which is the body a client
/// signing each chunk sends.
fn framed_and_signed(payload: &[u8], name: &str, value: &str) -> Vec<u8> {
    let mut body = framed(payload, name, value);
    body.truncate(body.len() - 2);
    body.extend_from_slice(
        format!("{TRAILER_SIGNATURE}:3f2a1c9b5d7e4f60a1b2c3d4e5f60718\r\n\r\n").as_bytes(),
    );
    body
}

/// The Base64 digest of `data` under the `ChecksumAlgorithm` value `algorithm`, computed by
/// the AWS SDK's own checksum crate rather than read back from the crate under test.
fn digest_of(algorithm: &str, data: &[u8]) -> String {
    let parsed: aws_smithy_checksums::ChecksumAlgorithm = algorithm
        .to_ascii_lowercase()
        .parse()
        .unwrap_or_else(|error| {
            panic!("{algorithm} must be a checksum this crate computes: {error}")
        });
    let mut checksum = parsed.into_impl();
    checksum.update(data);
    aws_smithy_types::base64::encode(checksum.finalize())
}

/// The trailing header one `ChecksumAlgorithm` value's digest arrives on: every `Checksum*`
/// member of this model binds `x-amz-checksum-` followed by its algorithm in lowercase
/// (`smithy_query(query="operation",operation="PutObject")` reports `ChecksumCRC32 ->
/// x-amz-checksum-crc32`, `ChecksumSHA256 -> x-amz-checksum-sha256`, and so on).
fn checksum_header(algorithm: &str) -> String {
    format!("x-amz-checksum-{}", algorithm.to_ascii_lowercase())
}

/// `len` bytes of payload, derived from `seed` so each case's digest is its own.
fn payload_of(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|index| seed.wrapping_add(index as u8)).collect()
}

/// One write as a case describes it: the key it addresses, the payload it carries, the trailer
/// name its HEAD announces, the name the trailing data really carries and the value under it,
/// and the two framing choices — whether the body is framed at all, and whether its framing
/// appends the signature line a signed streaming payload carries.
struct Write<'a> {
    key: &'a str,
    payload: &'a [u8],
    declared: &'a str,
    sent: &'a str,
    value: &'a str,
    framed_body: bool,
    signed: bool,
}

/// Drive one framed write and answer what it answered.
async fn framed_write(router: &MultiServiceRouter, write: &Write<'_>) -> Answer {
    let length = write.payload.len().to_string();
    let body = match (write.framed_body, write.signed) {
        (true, false) => framed(write.payload, write.sent, write.value),
        (true, true) => framed_and_signed(write.payload, write.sent, write.value),
        (false, _) => write.payload.to_vec(),
    };
    let mut headers: Vec<(&str, &str)> = vec![
        ("x-amz-content-sha256", STREAMING_TRAILER_MARKER),
        ("x-amz-trailer", write.declared),
    ];
    if write.framed_body {
        headers.push(("content-encoding", "aws-chunked"));
        headers.push(("x-amz-decoded-content-length", length.as_str()));
    }
    send_bytes(
        router,
        "PUT",
        &format!("/{BUCKET}/{}", write.key),
        &headers,
        &body,
    )
    .await
}

/// How a row's trailing data fails to hold, and what the refusal that earns says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// Nothing fails: the trailing value is the digest of the payload.
    None,
    /// The value is a digest, of other bytes.
    OtherBytes,
    /// The value is not Base64 at all.
    NotBase64,
    /// The value is Base64 of fewer bytes than the algorithm's digest is wide.
    TooShort,
    /// The trailing header name is not the one `x-amz-trailer` declared.
    OtherName,
}

/// One row of the table: the case, the algorithm the trailer declares, how the trailing data
/// fails, the payload size the digest is over, and whether the body carries the signature line
/// a signed streaming payload's framing appends.
struct TrailerRow {
    case: &'static str,
    algorithm: &'static str,
    fault: Fault,
    payload_len: usize,
    signed: bool,
}

/// The rows: four of the algorithms the model publishes, every way the trailing data can fail,
/// two payload sizes, and both framings — the plain one and the signed one, whose framing
/// appends the signature of its chunk chain after the declared trailer.
fn rows() -> Vec<TrailerRow> {
    vec![
        TrailerRow {
            case: "a CRC32 trailer that is the payload's digest",
            algorithm: "CRC32",
            fault: Fault::None,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a SHA256 trailer that is the payload's digest, over kilobytes",
            algorithm: "SHA256",
            fault: Fault::None,
            payload_len: 4_096,
            signed: false,
        },
        TrailerRow {
            case: "a signed framed body whose CRC32 trailer is the payload's digest",
            algorithm: "CRC32",
            fault: Fault::None,
            payload_len: 23,
            signed: true,
        },
        TrailerRow {
            case: "a CRC32 trailer carrying another payload's digest",
            algorithm: "CRC32",
            fault: Fault::OtherBytes,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a signed framed body whose CRC32 trailer is another payload's digest",
            algorithm: "CRC32",
            fault: Fault::OtherBytes,
            payload_len: 23,
            signed: true,
        },
        TrailerRow {
            case: "a CRC32C trailer carrying another payload's digest",
            algorithm: "CRC32C",
            fault: Fault::OtherBytes,
            payload_len: 64,
            signed: false,
        },
        TrailerRow {
            case: "a CRC64NVME trailer carrying another payload's digest, over kilobytes",
            algorithm: "CRC64NVME",
            fault: Fault::OtherBytes,
            payload_len: 4_096,
            signed: false,
        },
        TrailerRow {
            case: "a SHA256 trailer carrying another payload's digest",
            algorithm: "SHA256",
            fault: Fault::OtherBytes,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a CRC32 trailer whose value is not Base64",
            algorithm: "CRC32",
            fault: Fault::NotBase64,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a SHA256 trailer whose value is Base64 of too few bytes",
            algorithm: "SHA256",
            fault: Fault::TooShort,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a trailing header that is not the declared one",
            algorithm: "CRC32",
            fault: Fault::OtherName,
            payload_len: 23,
            signed: false,
        },
        TrailerRow {
            case: "a CRC64NVME trailer whose declared name is a SHA256 one",
            algorithm: "CRC64NVME",
            fault: Fault::OtherName,
            payload_len: 64,
            signed: false,
        },
    ]
}

/// The name the trailing data of `row` carries, given the name the request declared.
fn sent_name(row: &TrailerRow, declared: &str) -> String {
    match row.fault {
        // A checksum header of ANOTHER algorithm, which the request did not declare.
        Fault::OtherName => checksum_header(match row.algorithm {
            "SHA256" => "CRC32",
            _ => "SHA256",
        }),
        _ => declared.to_string(),
    }
}

/// The value the trailing data of `row` carries over `payload`.
fn sent_value(row: &TrailerRow, payload: &[u8]) -> String {
    let digest = digest_of(row.algorithm, payload);
    match row.fault {
        Fault::None | Fault::OtherName => digest,
        // The digest of other bytes entirely: a well-formed value of the right width that is
        // not this payload's.
        Fault::OtherBytes => digest_of(row.algorithm, b"bytes this write did not carry"),
        Fault::NotBase64 => String::from("not~base64~at~all"),
        Fault::TooShort => {
            let decoded = aws_smithy_types::base64::decode(&digest).expect("a Base64 digest");
            aws_smithy_types::base64::encode(&decoded[..decoded.len() - 1])
        }
    }
}

/// What `row` must be answered: the status, the code, and the message when the class fixes one
/// (the empty string where the row only requires a non-empty sentence).
fn expected(row: &TrailerRow) -> (u16, &'static str, String) {
    match row.fault {
        Fault::None => (200, "", String::new()),
        Fault::OtherBytes => (
            400,
            BAD_DIGEST,
            format!(
                "The {} you specified did not match the calculated checksum.",
                row.algorithm
            ),
        ),
        Fault::NotBase64 | Fault::TooShort => (
            400,
            INVALID_REQUEST,
            format!(
                "Value for {} header is invalid.",
                checksum_header(row.algorithm)
            ),
        ),
        Fault::OtherName => (400, MALFORMED_TRAILER, MALFORMED_TRAILER_MESSAGE.to_string()),
    }
}

#[tokio::test]
async fn a_framed_write_is_held_to_the_trailing_checksum_it_declared() {
    let router = router();
    bucket(&router, BUCKET).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let key = format!("trailer/{index}.bin");
        let payload = payload_of(index as u8 + 1, row.payload_len);
        let declared = checksum_header(row.algorithm);
        let sent = sent_name(row, &declared);
        let value = sent_value(row, &payload);
        let answered = framed_write(
            &router,
            &Write {
                key: &key,
                payload: &payload,
                declared: &declared,
                sent: &sent,
                value: &value,
                framed_body: true,
                signed: row.signed,
            },
        )
        .await;

        let (status, code, message) = expected(row);
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected HTTP {status}, got {} with body {:?}",
            answered.status,
            text(&answered)
        );
        let read = get_object(&router, BUCKET, &key, "", &[]).await;
        if status == 200 {
            // The object is there, holding exactly the payload the chunks decoded to, and the
            // checksum every later read reports is the trailer's own value.
            assert_eq!(
                read.body.as_ref(),
                payload.as_slice(),
                "case {case:?}: the stored object must hold the decoded payload, got {} bytes",
                read.body.len()
            );
            let reported = get_object(
                &router,
                BUCKET,
                &key,
                "",
                &[("x-amz-checksum-mode", "ENABLED")],
            )
            .await;
            assert_eq!(
                reported.header(&declared).as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected the read to report {declared} as {value:?}, got {:?}",
                reported.header(&declared)
            );
            continue;
        }
        assert_eq!(
            answered.error_code(),
            code,
            "case {case:?}: expected the wire code {code:?}, got {:?}",
            answered.error_code()
        );
        assert_eq!(
            answered.error_message(),
            message,
            "case {case:?}: expected the published sentence, got {:?}",
            answered.error_message()
        );
        // A refused write stores nothing: the key is not there at all.
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused write must store nothing, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.error_code(),
            "NoSuchKey",
            "case {case:?}: expected the key to be absent, got {:?}",
            read.error_code()
        );
    }
}

#[tokio::test]
async fn an_unframed_payload_carries_no_trailing_data_to_be_held_to() {
    let router = router();
    bucket(&router, BUCKET).await;

    // The boundary of the rule: the trailing data belongs to the FRAMING, so a request that
    // declares `x-amz-trailer` and sends its payload unframed has sent no trailer at all —
    // there is nothing to compare, and the write is served whatever value a framed body would
    // have had to carry.
    let payload = payload_of(99, 31);
    let declared = checksum_header("CRC32");
    let served = framed_write(
        &router,
        &Write {
            key: "unframed/payload.bin",
            payload: &payload,
            declared: &declared,
            sent: &declared,
            value: "not~base64~at~all",
            framed_body: false,
            signed: false,
        },
    )
    .await;
    assert_eq!(
        served.status,
        200,
        "an unframed payload declaring a trailer must be served, got {} with body {:?}",
        served.status,
        text(&served)
    );
    let read = get_object(&router, BUCKET, "unframed/payload.bin", "", &[]).await;
    assert_eq!(
        read.body.as_ref(),
        payload.as_slice(),
        "the unframed payload must be stored as sent, got {} bytes",
        read.body.len()
    );
}
