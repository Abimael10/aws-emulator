// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the read set upload PART slice of this service.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. The HTTP harness is the one
//! `private_workflow_runs` owns and the upload helpers are `multipart_read_set_uploads`';
//! only the part helpers live here.
//!
//! The rules under test:
//!
//! * a part uploaded into a multipart read set upload is stored once inside that upload,
//!   identified by the pair that names it there — its source file and its part number —
//!   and the response reports the digest of the bytes the request carried, so the same
//!   content always answers with the same checksum and different content with a different
//!   one;
//!   * uploading a source and part number the upload already holds REPLACES that part:
//!     its size and checksum become the new content's, the instant it was first uploaded
//!     stands, and the upload holds no second part for that pair;
//!   * the checksum is the SHA-256 digest of the part's bytes rendered as standard,
//!     `=`-padded base64 — the encoding an AWS content checksum travels in — and never the
//!     hexadecimal rendering of the same digest, so one value travels every path that
//!     carries it: the upload response, the listing entry, and the checksum a completion
//!     has to list for that part;
//! * `ListReadSetUploadParts` reports exactly the parts stored for the one upload and
//!   source the request names, in ascending part-number order, each member read from the
//!   stored part, paging over them and narrowing them by creation instant;
//! * `ListReadSets` reports exactly the read sets the one sequence store the request names
//!   still holds, each member read from that read set's own stored record, in ascending
//!   `readSetId` order, paging over them and narrowing them by every member of its filter;
//! * a request naming a sequence store or an upload this emulator does not hold — or
//!   carrying no content at all — is refused rather than stored.

use serde_json::{json, Value};
use service_com_amazonaws_omics_omics::state::{
    service_state, CREATION_TYPE_UPLOAD, FIRST_SEQUENCE_STORE_ID,
    MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE, MULTIPART_READ_SET_UPLOAD_PAGE_TOKEN_TAG,
    READ_SET_ID_OFFSET, READ_SET_NOT_FOUND_MESSAGE, READ_SET_PAGE_TOKEN_TAG,
    READ_SET_STATUS_ACTIVE, READ_SET_STATUS_DELETED, READ_SET_STATUS_PROCESSING_UPLOAD,
    RESOURCE_HAS_DEPENDENTS_MESSAGE, SEQUENCE_STORE_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::multipart_read_set_uploads::{
    create_store, create_upload, created_upload_id, emulator_holding_its_engine, stored_upload,
    REFERENCE_ARN,
};
use crate::private_workflow_runs::{
    assert_absent, assert_arn, assert_members, assert_timestamp, call, error_code, POLL_INTERVAL,
    REGION, SETTLE_DEADLINE,
};

/// Every value the modelled `ReadSetPartSource` enum admits.
/// Source: `smithy_query(query="shape",shape="ReadSetPartSource")` -> SOURCE1 | SOURCE2.
const PART_SOURCES: &[&str] = &["SOURCE1", "SOURCE2"];

/// The published bounds of the modelled `partNumber` range (1..10000).
/// Source: the `partNumber` member of `UploadReadSetPartRequest`.
const PART_NUMBER_RANGE: (i32, i32) = (1, 10_000);

/// The access key id every call below presents, matching the harness's own default.
const AKID: &str = "AKIDOMICSTEST";

/// `PUT /sequencestore/{store}/upload/{upload}/part?partSource=..&partNumber=..` with
/// `payload` as the RAW body.
///
/// That is the one wire form this operation binds: `payload` is an `@httpPayload`
/// streaming blob, and `partSource`/`partNumber` are `@httpQuery`, so the body carries
/// bytes rather than JSON and the harness's JSON `call` cannot express it.
async fn upload_part(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    upload_id: &str,
    part_source: &str,
    part_number: i32,
    payload: &[u8],
) -> (u16, Value) {
    let uri = format!(
        "/sequencestore/{sequence_store_id}/upload/{upload_id}/part\
         ?partSource={part_source}&partNumber={part_number}"
    );
    let request = http::Request::builder()
        .method("PUT")
        .uri(uri)
        .header("host", format!("omics.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={AKID}/20260101/{REGION}/omics/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        // The modelled blob declares `@requiresLength`, so the part is sent as a body of
        // a known length and not as a chunked stream.
        .header("content-type", "application/octet-stream")
        .header("content-length", payload.len().to_string())
        .body(bytes::Bytes::copy_from_slice(payload))
        .expect("a well-formed test request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    // restJson1 carries the error shape in `__type`, and the framework also sets the
    // `x-amzn-errortype` header; fold the header in so `error_code` reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// `POST /sequencestore/{store}/upload/{upload}/parts{query}` with `body`.
async fn list_parts(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    upload_id: &str,
    query: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/sequencestore/{sequence_store_id}/upload/{upload_id}/parts{query}"),
        Some(body),
    )
    .await
}

/// The `parts` array of a 200 listing, after asserting the status.
fn parts_of(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListReadSetUploadParts, got {body:?}"
    );
    body.get("parts")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a parts array, got {body:?}")
        })
        .clone()
}

/// The `checksum` of a 200 upload response, after asserting the status and that the value
/// is the base64 SHA-256 form the service reports.
fn uploaded_checksum(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from UploadReadSetPart, got {body:?}"
    );
    let checksum = body
        .get("checksum")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: response carried no checksum, got {body:?}"))
        .to_string();
    assert_base64_sha256(case, &checksum);
    checksum
}

/// The width of a SHA-256 digest in bytes, and the number of characters standard base64
/// renders it in: 44, the last of them padding.
/// Source: SHA-256 (FIPS 180-4) is a 256-bit digest, and standard base64 (RFC 4648 section
/// 4) codes every 3 bytes as 4 characters, padding the final group.
const SHA256_DIGEST_BYTES: usize = 32;
const BASE64_SHA256_WIDTH: usize = 44;

/// The checksum this service reports for a part carrying `bytes`: the base64 of their
/// SHA-256 digest.
fn expected_part_checksum(bytes: &[u8]) -> String {
    use sha2::Digest;
    ::aws_smithy_types::base64::encode(sha2::Sha256::digest(bytes).as_slice())
}

/// The lowercase hexadecimal rendering of the same digest — used only to prove it is NOT
/// what the service answers with, and is not accepted in its place.
fn hex_part_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// Assert `value` is a SHA-256 digest in standard, padded base64: 44 characters of the
/// base64 alphabet that decode to 32 bytes.
fn assert_base64_sha256(case: &str, value: &str) {
    assert_eq!(
        value.len(),
        BASE64_SHA256_WIDTH,
        "case {case:?}: expected a {BASE64_SHA256_WIDTH}-character base64 checksum, got \
         {value:?} of {} characters",
        value.len()
    );
    assert!(
        value.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || byte == b'+'
            || byte == b'/'
            || byte == b'='),
        "case {case:?}: expected only standard base64 characters, got {value:?}"
    );
    assert!(
        value.ends_with('='),
        "case {case:?}: expected the padding a 32-byte digest's base64 ends in, got {value:?}"
    );
    let decoded = ::aws_smithy_types::base64::decode(value)
        .unwrap_or_else(|error| panic!("case {case:?}: {value:?} is not base64: {error}"));
    assert_eq!(
        decoded.len(),
        SHA256_DIGEST_BYTES,
        "case {case:?}: expected {SHA256_DIGEST_BYTES} decoded digest bytes, got {} from \
         {value:?}",
        decoded.len()
    );
}

/// The request body every upload below is initiated with.
fn upload_request(name: &str) -> Value {
    json!({
        "name": name,
        "sourceFileType": "FASTQ",
        "subjectId": "subject-parts",
        "sampleId": "sample-parts",
    })
}

/// An initiated upload inside a fresh sequence store, with the engine behind them.
async fn store_and_upload(
    name: &str,
) -> (
    MultiServiceRouter,
    std::sync::Arc<engine::InMemoryEngine>,
    String,
    String,
) {
    let (router, engine) = emulator_holding_its_engine();
    let store = create_store(&router, name).await;
    let (status, body) = create_upload(&router, &store, &upload_request(name)).await;
    let upload = created_upload_id(name, status, &body);
    (router, engine, store, upload)
}

/// A row of the part table: where the part belongs and what it carries.
struct PartCase {
    /// How the row is reported in a failure.
    case: &'static str,
    part_source: &'static str,
    part_number: i32,
    payload: Vec<u8>,
}

/// The table: one row per way a part can differ, so the rule is asserted over the class
/// and not over one request. The rows vary along the source file, the part number
/// (including both published bounds of its range), the size of the content (one byte to
/// kilobytes) and its bytes (text, a byte ramp, and bytes no text encoding admits).
fn part_cases() -> Vec<PartCase> {
    let (first_number, last_number) = PART_NUMBER_RANGE;
    vec![
        PartCase {
            case: "the first part of the first source",
            part_source: "SOURCE1",
            part_number: first_number,
            payload: b"@read1\nACGTACGTACGT\n+\n!!!!!!!!!!!!\n".to_vec(),
        },
        PartCase {
            case: "a later part of the first source",
            part_source: "SOURCE1",
            part_number: 2,
            payload: (0..4096u32).map(|n| (n % 251) as u8).collect(),
        },
        PartCase {
            case: "the highest part number the model admits",
            part_source: "SOURCE1",
            part_number: last_number,
            payload: vec![0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe],
        },
        PartCase {
            case: "a single byte from the paired source",
            part_source: "SOURCE2",
            part_number: first_number,
            payload: vec![b'N'],
        },
        PartCase {
            case: "the same content as the first row, under the paired source",
            part_source: "SOURCE2",
            part_number: 2,
            payload: b"@read1\nACGTACGTACGT\n+\n!!!!!!!!!!!!\n".to_vec(),
        },
        PartCase {
            case: "a part in the middle of the paired source",
            part_source: "SOURCE2",
            part_number: 573,
            payload: (0..1000u32).map(|n| (n % 7) as u8 + 1).collect(),
        },
    ]
}

#[tokio::test]
async fn an_uploaded_part_is_stored_once_and_reported_from_that_record() {
    let (router, engine, store, upload) = store_and_upload("the part destination").await;
    let cases = part_cases();

    // Upload them all first, so each assertion below also proves the upload keeps the
    // rows apart rather than answering with the most recent one.
    let mut checksums: Vec<String> = Vec::new();
    for part in &cases {
        let (status, body) = upload_part(
            &router,
            &store,
            &upload,
            part.part_source,
            part.part_number,
            &part.payload,
        )
        .await;
        let checksum = uploaded_checksum(part.case, status, &body);
        // The checksum is read off the content: a client that digests the bytes it sent
        // gets exactly this value back.
        assert_eq!(
            checksum,
            expected_part_checksum(&part.payload),
            "case {:?}: expected the digest of the uploaded bytes, got {checksum:?}",
            part.case
        );
        checksums.push(checksum);
    }

    // Same content, same checksum; different content, a different one.
    for (left, left_case) in cases.iter().enumerate().map(|(n, c)| (n, c.case)) {
        for right in (left + 1)..cases.len() {
            let same_content = cases[left].payload == cases[right].payload;
            assert_eq!(
                checksums[left] == checksums[right],
                same_content,
                "case {left_case:?} against {:?}: same content must mean the same \
                 checksum and different content a different one, got {:?} and {:?}",
                cases[right].case,
                checksums[left],
                checksums[right]
            );
        }
    }

    // Read back through the operation that reports an upload's parts: one source at a
    // time, every stored part of it, in ascending part-number order.
    for source in PART_SOURCES {
        let mut expected: Vec<&PartCase> = cases
            .iter()
            .filter(|part| part.part_source == *source)
            .collect();
        expected.sort_by_key(|part| part.part_number);
        let (status, body) = list_parts(
            &router,
            &store,
            &upload,
            "",
            &json!({ "partSource": source }),
        )
        .await;
        let listed = parts_of(source, status, &body);
        assert_eq!(
            listed.len(),
            expected.len(),
            "case {source:?}: expected {} listed parts, got {body:?}",
            expected.len()
        );
        for (part, item) in expected.iter().zip(&listed) {
            let case = part.case;
            assert_eq!(
                item.get("partNumber").and_then(Value::as_i64),
                Some(i64::from(part.part_number)),
                "case {case:?}: expected partNumber {}, got {item:?}",
                part.part_number
            );
            assert_eq!(
                item.get("partSize").and_then(Value::as_i64),
                Some(part.payload.len() as i64),
                "case {case:?}: expected partSize {}, got {item:?}",
                part.payload.len()
            );
            assert_eq!(
                item.get("partSource").and_then(Value::as_str),
                Some(*source),
                "case {case:?}: expected partSource {source:?}, got {item:?}"
            );
            assert_eq!(
                item.get("checksum").and_then(Value::as_str),
                Some(expected_part_checksum(&part.payload).as_str()),
                "case {case:?}: expected the content digest as the listed checksum, \
                 got {item:?}"
            );
            let created = assert_timestamp(case, "creationTime", item);
            let updated = assert_timestamp(case, "lastUpdatedTime", item);
            assert!(
                updated >= created,
                "case {case:?}: expected lastUpdatedTime at or after creationTime, \
                 got {updated} and {created}"
            );
        }
    }

    // And the bytes really are in the service's state: the part a later complete reads is
    // the content the caller sent, not its length alone.
    let stored = stored_upload(&engine, &store, &upload);
    assert_eq!(
        stored.parts.len(),
        cases.len(),
        "expected {} stored parts, got {:?}",
        cases.len(),
        stored.parts.keys().collect::<Vec<_>>()
    );
    for part in &cases {
        let held = stored
            .parts
            .get(&(part.part_source.to_string(), part.part_number))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: no part is stored for {} {}",
                    part.case, part.part_source, part.part_number
                )
            });
        assert_eq!(
            held.payload, part.payload,
            "case {:?}: the stored part must carry the bytes the request sent",
            part.case
        );
        assert_eq!(
            held.checksum,
            expected_part_checksum(&part.payload),
            "case {:?}: the stored checksum must be the digest of the stored bytes",
            part.case
        );
    }
}

#[tokio::test]
async fn a_part_checksum_is_the_base64_sha256_of_its_bytes_on_every_path() {
    let (router, _engine, store, upload) = store_and_upload("the checksum destination").await;
    // The same table of contents: the encoding rule holds for every size and byte pattern
    // a part can carry, not for one payload.
    let cases = part_cases();
    let mut answered: Vec<(&'static str, i32, String)> = Vec::new();

    for part in &cases {
        let (status, body) = upload_part(
            &router,
            &store,
            &upload,
            part.part_source,
            part.part_number,
            &part.payload,
        )
        .await;
        // `uploaded_checksum` asserts the base64 shape: 44 characters that decode to the
        // 32 bytes of a SHA-256 digest.
        let checksum = uploaded_checksum(part.case, status, &body);
        assert_eq!(
            checksum,
            expected_part_checksum(&part.payload),
            "case {:?}: expected the base64 SHA-256 of the uploaded bytes, got {checksum:?}",
            part.case
        );
        // The same digest rendered the other way is a different string, and the response
        // carries the base64 one.
        let hex = hex_part_digest(&part.payload);
        assert_ne!(
            checksum, hex,
            "case {:?}: expected the base64 rendering of the digest, got the hexadecimal one",
            part.case
        );
        assert_eq!(
            ::aws_smithy_types::base64::decode(&checksum)
                .unwrap_or_else(|error| panic!("case {:?}: {checksum:?} is not base64: {error}", part.case)),
            hex::decode(&hex)
                .unwrap_or_else(|error| panic!("case {:?}: {hex:?} is not hex: {error}", part.case)),
            "case {:?}: both renderings must carry the same digest bytes",
            part.case
        );
        answered.push((part.part_source, part.part_number, checksum));
    }

    // The listing reports, for each part, the very value the upload answered with.
    for source in PART_SOURCES {
        let (status, body) = list_parts(
            &router,
            &store,
            &upload,
            "",
            &json!({ "partSource": source }),
        )
        .await;
        for item in &parts_of(source, status, &body) {
            let number = item
                .get("partNumber")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("case {source:?}: a listed part carried no partNumber"));
            let expected = answered
                .iter()
                .find(|(part_source, part_number, _)| {
                    part_source == source && i64::from(*part_number) == number
                })
                .map(|(_, _, checksum)| checksum.clone())
                .unwrap_or_else(|| {
                    panic!("case {source:?}: part {number} was never uploaded, got {item:?}")
                });
            let reported = item
                .get("checksum")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {source:?}: part {number} listed no checksum"));
            assert_base64_sha256(&format!("{source} part {number}"), reported);
            assert_eq!(
                reported, expected,
                "case {source:?}: part {number} must be listed with the checksum the upload \
                 answered, got {reported:?}"
            );
        }
    }

    // A completion listing the hexadecimal rendering of each digest is naming a checksum
    // no part of this upload carries, so it is refused and forms nothing...
    let hex_listed: Vec<(&str, i32, String)> = cases
        .iter()
        .map(|part| {
            (
                part.part_source,
                part.part_number,
                hex_part_digest(&part.payload),
            )
        })
        .collect();
    let (status, body) = complete_upload(&router, &store, &upload, &parts_list(&hex_listed)).await;
    assert_eq!(
        status, 400,
        "case \"hexadecimal checksums\": expected 400 from the completion, got {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case \"hexadecimal checksums\": expected ValidationException, got {body:?}"
    );

    // ...while the values the service answered with are exactly what it accepts for them.
    let base64_listed: Vec<(&str, i32, String)> = answered
        .iter()
        .map(|(part_source, part_number, checksum)| {
            (*part_source, *part_number, checksum.clone())
        })
        .collect();
    let (status, body) =
        complete_upload(&router, &store, &upload, &parts_list(&base64_listed)).await;
    let read_set_id = completed_read_set_id("the answered checksums", status, &body);
    assert!(
        !read_set_id.is_empty(),
        "case \"the answered checksums\": expected a readSetId, got {body:?}"
    );
}

#[tokio::test]
async fn re_uploading_a_part_number_replaces_that_part() {
    let (router, engine, store, upload) = store_and_upload("the replacement destination").await;

    // Each row replaces one part number with content that differs from the first upload
    // in length, in bytes, or in neither.
    let rows: Vec<(&str, i32, Vec<u8>, Vec<u8>)> = vec![
        (
            "replaced with longer content",
            1,
            b"the first attempt".to_vec(),
            b"the second attempt, which is longer".to_vec(),
        ),
        (
            "replaced with shorter content",
            2,
            vec![7u8; 512],
            vec![9u8],
        ),
        (
            "replaced with the same content",
            3,
            b"identical bytes".to_vec(),
            b"identical bytes".to_vec(),
        ),
    ];

    for (case, part_number, first, second) in &rows {
        let (status, body) =
            upload_part(&router, &store, &upload, "SOURCE1", *part_number, first).await;
        let first_checksum = uploaded_checksum(case, status, &body);
        assert_eq!(
            first_checksum,
            expected_part_checksum(first),
            "case {case:?}: expected the digest of the first content, got {body:?}"
        );
        let listed = listed_part(&router, &store, &upload, case, *part_number).await;
        let created = assert_timestamp(case, "creationTime", &listed);

        let (status, body) =
            upload_part(&router, &store, &upload, "SOURCE1", *part_number, second).await;
        let second_checksum = uploaded_checksum(case, status, &body);
        assert_eq!(
            second_checksum,
            expected_part_checksum(second),
            "case {case:?}: expected the digest of the replacing content, got {body:?}"
        );

        let listed = listed_part(&router, &store, &upload, case, *part_number).await;
        assert_eq!(
            listed.get("partSize").and_then(Value::as_i64),
            Some(second.len() as i64),
            "case {case:?}: expected the replacing content's size, got {listed:?}"
        );
        assert_eq!(
            listed.get("checksum").and_then(Value::as_str),
            Some(second_checksum.as_str()),
            "case {case:?}: expected the replacing content's checksum, got {listed:?}"
        );
        // A replacement is not a second create: the part number keeps the instant it was
        // first uploaded, and only its last-updated instant can have moved.
        let created_again = assert_timestamp(case, "creationTime", &listed);
        let updated = assert_timestamp(case, "lastUpdatedTime", &listed);
        assert_eq!(
            created_again, created,
            "case {case:?}: expected the creationTime of the first upload to stand, \
             got {created_again} after {created}"
        );
        assert!(
            updated >= created_again,
            "case {case:?}: expected lastUpdatedTime at or after creationTime, got \
             {updated} and {created_again}"
        );
    }

    // One part per number, however often a number was uploaded.
    let stored = stored_upload(&engine, &store, &upload);
    assert_eq!(
        stored.parts.len(),
        rows.len(),
        "expected one stored part per uploaded part number, got {:?}",
        stored.parts.keys().collect::<Vec<_>>()
    );
    for (case, part_number, _, second) in &rows {
        let held = stored
            .parts
            .get(&("SOURCE1".to_string(), *part_number))
            .unwrap_or_else(|| panic!("case {case:?}: no part is stored for {part_number}"));
        assert_eq!(
            &held.payload, second,
            "case {case:?}: the stored part must carry the replacing bytes"
        );
    }
}

/// The one listed item for `part_number` of `SOURCE1`, after asserting there is exactly
/// one.
async fn listed_part(
    router: &MultiServiceRouter,
    store: &str,
    upload: &str,
    case: &str,
    part_number: i32,
) -> Value {
    let (status, body) = list_parts(
        router,
        store,
        upload,
        "",
        &json!({ "partSource": "SOURCE1" }),
    )
    .await;
    let listed = parts_of(case, status, &body);
    let matching: Vec<&Value> = listed
        .iter()
        .filter(|item| item.get("partNumber").and_then(Value::as_i64) == Some(i64::from(part_number)))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one listed part numbered {part_number}, got {listed:?}"
    );
    matching[0].clone()
}

/// A row of the part-refusal table: the request, and the refusal it must answer with.
struct RefusalCase {
    /// How the row is reported in a failure.
    case: &'static str,
    sequence_store_id: String,
    upload_id: String,
    part_source: &'static str,
    part_number: i32,
    payload: Vec<u8>,
    status: u16,
    code: &'static str,
    /// The message the refusal must carry verbatim, for the refusals this service words
    /// itself; a modelled-constraint refusal is worded by the centralized walk, so those
    /// rows name none.
    message: Option<&'static str>,
}

/// A row of the listing-refusal table: the ids and query the listing is refused for.
struct ListingRefusalCase {
    case: &'static str,
    sequence_store_id: String,
    upload_id: String,
    query: &'static str,
    status: u16,
    code: &'static str,
    message: Option<&'static str>,
}

#[tokio::test]
async fn a_part_this_emulator_cannot_place_is_refused() {
    let (router, engine, store, upload) = store_and_upload("the refusal destination").await;
    // A second store, so an upload id can be presented under a store it does not belong
    // to.
    let other_store = create_store(&router, "the other refusal destination").await;

    // An id no store or upload holds, spelled inside the modelled 10..36 digits.
    let absent = "9999999999";

    let rows: Vec<RefusalCase> = vec![
        RefusalCase {
            case: "a sequence store nothing holds",
            sequence_store_id: absent.to_string(),
            upload_id: upload.clone(),
            part_source: "SOURCE1",
            part_number: 1,
            payload: b"orphan".to_vec(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(SEQUENCE_STORE_NOT_FOUND_MESSAGE),
        },
        RefusalCase {
            case: "an upload nothing holds",
            sequence_store_id: store.clone(),
            upload_id: absent.to_string(),
            part_source: "SOURCE1",
            part_number: 1,
            payload: b"orphan".to_vec(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE),
        },
        RefusalCase {
            case: "an upload presented under another store",
            sequence_store_id: other_store.clone(),
            upload_id: upload.clone(),
            part_source: "SOURCE1",
            part_number: 1,
            payload: b"orphan".to_vec(),
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE),
        },
        RefusalCase {
            case: "a part carrying no content",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            part_source: "SOURCE1",
            part_number: 1,
            payload: Vec::new(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a part number below the published range",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            part_source: "SOURCE1",
            part_number: PART_NUMBER_RANGE.0 - 1,
            payload: b"out of range".to_vec(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a part number above the published range",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            part_source: "SOURCE1",
            part_number: PART_NUMBER_RANGE.1 + 1,
            payload: b"out of range".to_vec(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a source the enum does not admit",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            part_source: "SOURCE3",
            part_number: 1,
            payload: b"unknown source".to_vec(),
            status: 400,
            code: "ValidationException",
            message: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (got, body) = upload_part(
            &router,
            &row.sequence_store_id,
            &row.upload_id,
            row.part_source,
            row.part_number,
            &row.payload,
        )
        .await;
        assert_eq!(
            got, row.status,
            "case {case:?}: expected HTTP {}, got {got} with {body:?}",
            row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected {}, got {body:?}",
            row.code
        );
        if let Some(message) = row.message {
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(message),
                "case {case:?}: expected the service's own wording, got {body:?}"
            );
        }
    }

    // Nothing was stored by any refusal, under either store.
    let stored = stored_upload(&engine, &store, &upload);
    assert!(
        stored.parts.is_empty(),
        "expected a refused upload to store nothing, got {:?}",
        stored.parts.keys().collect::<Vec<_>>()
    );

    // The same two ids refuse a LISTING as well, and a token no listing of parts issued
    // is refused rather than read as a resume point.
    let listing_rows: Vec<ListingRefusalCase> = vec![
        ListingRefusalCase {
            case: "listing a sequence store nothing holds",
            sequence_store_id: absent.to_string(),
            upload_id: upload.clone(),
            query: "",
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(SEQUENCE_STORE_NOT_FOUND_MESSAGE),
        },
        ListingRefusalCase {
            case: "listing an upload nothing holds",
            sequence_store_id: store.clone(),
            upload_id: absent.to_string(),
            query: "",
            status: 404,
            code: "ResourceNotFoundException",
            message: Some(MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE),
        },
        ListingRefusalCase {
            case: "a token from another listing",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            query: "?nextToken=rg_31303030303030",
            status: 400,
            code: "ValidationException",
            message: None,
        },
        ListingRefusalCase {
            case: "a token this service never issued",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            query: "?nextToken=not-a-token",
            status: 400,
            code: "ValidationException",
            message: None,
        },
    ];

    for row in &listing_rows {
        let case = row.case;
        let (got, body) = list_parts(
            &router,
            &row.sequence_store_id,
            &row.upload_id,
            row.query,
            &json!({ "partSource": "SOURCE1" }),
        )
        .await;
        assert_eq!(
            got, row.status,
            "case {case:?}: expected HTTP {}, got {got} with {body:?}",
            row.status
        );
        assert_eq!(
            error_code(&body),
            row.code,
            "case {case:?}: expected {}, got {body:?}",
            row.code
        );
        if let Some(message) = row.message {
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(message),
                "case {case:?}: expected the service's own wording, got {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn listing_an_uploads_parts_pages_and_narrows_over_the_stored_parts() {
    let (router, _engine, store, upload) = store_and_upload("the paging destination").await;

    // Five parts of one source, and two of the other, so a page is asserted to stop at
    // the source it was asked for as well as at its size.
    let numbered: Vec<i32> = vec![1, 2, 3, 4, 5];
    for number in &numbered {
        let (status, body) = upload_part(
            &router,
            &store,
            &upload,
            "SOURCE1",
            *number,
            format!("part {number} of source1").as_bytes(),
        )
        .await;
        uploaded_checksum(&format!("source1 part {number}"), status, &body);
    }
    for number in &[1, 2] {
        let (status, body) = upload_part(
            &router,
            &store,
            &upload,
            "SOURCE2",
            *number,
            format!("part {number} of source2").as_bytes(),
        )
        .await;
        uploaded_checksum(&format!("source2 part {number}"), status, &body);
    }

    // A page size at and below the number of stored parts, so the walk is asserted both
    // with and without a token.
    for page_size in &[1usize, 2, 5, 7] {
        let case = format!("maxResults={page_size}");
        let mut seen: Vec<i64> = Vec::new();
        let mut token: Option<String> = None;
        // One iteration per part at worst, plus the page that ends the walk.
        for _ in 0..=numbered.len() {
            let query = match &token {
                Some(token) => format!("?maxResults={page_size}&nextToken={token}"),
                None => format!("?maxResults={page_size}"),
            };
            let (status, body) = list_parts(
                &router,
                &store,
                &upload,
                &query,
                &json!({ "partSource": "SOURCE1" }),
            )
            .await;
            let page = parts_of(&case, status, &body);
            assert!(
                page.len() <= *page_size,
                "case {case:?}: a page must hold at most {page_size} parts, got {body:?}"
            );
            seen.extend(
                page.iter()
                    .filter_map(|item| item.get("partNumber").and_then(Value::as_i64)),
            );
            token = body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the walk must end without a nextToken, got {token:?}"
        );
        let expected: Vec<i64> = numbered.iter().map(|n| i64::from(*n)).collect();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected every part of the source exactly once in ascending \
             order, got {seen:?}"
        );
    }

    // The listing narrows to the source the request names.
    let (status, body) = list_parts(
        &router,
        &store,
        &upload,
        "",
        &json!({ "partSource": "SOURCE2" }),
    )
    .await;
    let listed = parts_of("the paired source", status, &body);
    let numbers: Vec<i64> = listed
        .iter()
        .filter_map(|item| item.get("partNumber").and_then(Value::as_i64))
        .collect();
    assert_eq!(
        numbers,
        vec![1, 2],
        "case \"the paired source\": expected only that source's parts, got {body:?}"
    );

    // And it narrows by creation instant: each bound is inclusive, and a window the
    // parts fall outside of selects none of them.
    let windows: Vec<(&str, Value, usize)> = vec![
        (
            "created after an instant long past",
            json!({ "createdAfter": "2000-01-01T00:00:00Z" }),
            numbered.len(),
        ),
        (
            "created before an instant far ahead",
            json!({ "createdBefore": "2999-01-01T00:00:00Z" }),
            numbered.len(),
        ),
        (
            "inside a window spanning both",
            json!({
                "createdAfter": "2000-01-01T00:00:00Z",
                "createdBefore": "2999-01-01T00:00:00Z",
            }),
            numbered.len(),
        ),
        (
            "created after an instant far ahead",
            json!({ "createdAfter": "2999-01-01T00:00:00Z" }),
            0,
        ),
        (
            "created before an instant long past",
            json!({ "createdBefore": "2000-01-01T00:00:00Z" }),
            0,
        ),
    ];

    for (case, filter, expected) in &windows {
        let (status, body) = list_parts(
            &router,
            &store,
            &upload,
            "",
            &json!({ "partSource": "SOURCE1", "filter": filter }),
        )
        .await;
        let listed = parts_of(case, status, &body);
        assert_eq!(
            listed.len(),
            *expected,
            "case {case:?}: expected {expected} parts, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// CompleteMultipartReadSetUpload -> GetReadSetMetadata
// ---------------------------------------------------------------------------
//
// The rules the cases below assert:
//
// * completing a multipart read set upload forms ONE read set inside the sequence store
//   the request names, out of the parts the request lists: each file of the read set is
//   the content of that source's parts in ascending part-number order, however the
//   request happened to list them, and every descriptive member is the one the upload
//   carried;
// * the upload is spent by the completion: nothing can be added to, listed in or aborted
//   on it afterwards;
// * a completion naming a part the upload does not hold, a checksum that is not the
//   stored part's, the same part twice, or no part at all is refused and forms nothing;
// * a sequence store that holds a read set is not empty, so deleting it is refused until
//   its read sets are gone.

/// The published bounds of the modelled `ReadSetId` length (10..36).
/// Source: the `readSetId` member of `CompleteMultipartReadSetUploadResponse`.
const READ_SET_ID_WIDTH: (usize, usize) = (10, 36);

/// `POST /sequencestore/{store}/upload/{upload}/complete` with `parts` as the list.
pub(crate) async fn complete_upload(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    upload_id: &str,
    parts: &Value,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &format!("/sequencestore/{sequence_store_id}/upload/{upload_id}/complete"),
        Some(&json!({ "parts": parts })),
    )
    .await
}

/// `GET /sequencestore/{store}/readset/{id}/metadata`.
async fn read_set_metadata(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    read_set_id: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/sequencestore/{sequence_store_id}/readset/{read_set_id}/metadata"),
        None,
    )
    .await
}

/// `DELETE /sequencestore/{id}`.
async fn delete_sequence_store(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/sequencestore/{sequence_store_id}"),
        None,
    )
    .await
}

/// The `readSetId` of a 200 completion response, after asserting the status and its shape.
fn completed_read_set_id(case: &str, status: u16, body: &Value) -> String {
    assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
    let id = body
        .get("readSetId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: response carried no readSetId, got {body:?}"))
        .to_string();
    let (min, max) = READ_SET_ID_WIDTH;
    assert!(
        id.len() >= min && id.len() <= max && id.bytes().all(|b| b.is_ascii_digit()),
        "case {case:?}: expected a {min}..{max} digit decimal readSetId, got {id:?}"
    );
    let value: u64 = id
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: readSetId {id:?} does not parse as a number"));
    let floor = FIRST_SEQUENCE_STORE_ID + READ_SET_ID_OFFSET;
    assert!(
        value >= floor,
        "case {case:?}: expected a readSetId at or above {floor}, got {value}"
    );
    id
}

/// The parts list a completion carries: one entry per uploaded part, with the checksum
/// `UploadReadSetPart` answered with.
pub(crate) fn parts_list(parts: &[(&str, i32, String)]) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|(part_source, part_number, checksum)| {
                json!({
                    "partSource": part_source,
                    "partNumber": part_number,
                    "checksum": checksum,
                })
            })
            .collect(),
    )
}

/// Upload every part of `parts` and answer the completion list entry for each, in the
/// order the parts were uploaded.
pub(crate) async fn upload_all(
    router: &MultiServiceRouter,
    case: &str,
    sequence_store_id: &str,
    upload_id: &str,
    parts: &[(&'static str, i32, Vec<u8>)],
) -> Vec<(&'static str, i32, String)> {
    let mut listed = Vec::new();
    for (part_source, part_number, payload) in parts {
        let (status, body) = upload_part(
            router,
            sequence_store_id,
            upload_id,
            part_source,
            *part_number,
            payload,
        )
        .await;
        let checksum = uploaded_checksum(case, status, &body);
        assert_eq!(
            checksum,
            expected_part_checksum(payload),
            "case {case:?}: part {part_number} of {part_source} must report the digest of \
             its own content"
        );
        listed.push((*part_source, *part_number, checksum));
    }
    listed
}

/// Read the read set until it reports `expected`, and answer that response.
///
/// Polling rather than sleeping a fixed span: the status is owed to the clock, so what is
/// asserted is that it arrives, not when a single read happened to land.
async fn await_read_set_status(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    read_set_id: &str,
    expected: &str,
) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = read_set_metadata(router, sequence_store_id, read_set_id).await;
        assert_eq!(
            status, 200,
            "read set {read_set_id}: expected 200, got {body:?}"
        );
        if body.get("status").and_then(Value::as_str) == Some(expected) {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "read set {read_set_id}: expected status {expected} within {SETTLE_DEADLINE:?}, \
             last read {body:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A row of the completion table: what the upload carried, which parts were uploaded into
/// it, and the descriptive members the read set must report.
struct CompleteCase {
    /// How the row is reported in a failure, and the upload's `name`.
    case: &'static str,
    /// The `sourceFileType` the row uploads.
    source_file_type: &'static str,
    /// The members the upload is initiated with around the required ones.
    upload: Value,
    /// The parts uploaded into it: source file, part number, content.
    parts: Vec<(&'static str, i32, Vec<u8>)>,
    /// Every member `GetReadSetMetadata` must report with exactly this value.
    expected: Value,
    /// Members it must NOT carry: nothing is invented for what the upload left out, and
    /// nothing is reported for what this emulator does not compute.
    absent: Vec<&'static str>,
}

/// The table: one row per way a completed upload can differ, so the rule is asserted over
/// the class and not over one request. The rows vary along the source format, the number
/// of source files (one and a paired pair), the number of parts per source, the part
/// numbers (including the top of the modelled range), the size and bytes of the content,
/// and which optional members the upload carried.
fn complete_cases() -> Vec<CompleteCase> {
    let (_, last_number) = PART_NUMBER_RANGE;
    vec![
        CompleteCase {
            case: "one part from one source",
            source_file_type: "FASTQ",
            upload: json!({}),
            parts: vec![(
                "SOURCE1",
                1,
                b"@read1\nACGTACGTACGT\n+\n!!!!!!!!!!!!\n".to_vec(),
            )],
            expected: json!({ "fileType": "FASTQ" }),
            absent: vec![
                "description",
                "referenceArn",
                "sequenceInformation",
                "etag",
                "statusMessage",
            ],
        },
        CompleteCase {
            case: "several parts from one source",
            source_file_type: "FASTQ",
            upload: json!({ "description": "reads for the autumn cohort" }),
            parts: vec![
                ("SOURCE1", 1, vec![b'A'; 64]),
                ("SOURCE1", 2, vec![b'C'; 64]),
                ("SOURCE1", 3, vec![b'G'; 17]),
            ],
            expected: json!({
                "fileType": "FASTQ",
                "description": "reads for the autumn cohort",
            }),
            absent: vec!["referenceArn", "sequenceInformation", "etag"],
        },
        CompleteCase {
            case: "a paired-end upload",
            source_file_type: "FASTQ",
            upload: json!({ "generatedFrom": "1000 Genomes" }),
            parts: vec![
                ("SOURCE1", 1, (0..4096u32).map(|n| (n % 251) as u8).collect()),
                ("SOURCE1", 2, vec![0x1f, 0x8b, 0x08, 0x00, 0xff, 0xfe]),
                ("SOURCE2", 1, vec![b'N']),
            ],
            expected: json!({
                "fileType": "FASTQ",
                "sequenceInformation": { "generatedFrom": "1000 Genomes" },
            }),
            absent: vec!["description", "referenceArn", "etag"],
        },
        CompleteCase {
            case: "an aligned upload with a reference",
            source_file_type: "CRAM",
            upload: json!({
                "referenceArn": REFERENCE_ARN,
                "description": "aligned reads",
            }),
            parts: vec![
                ("SOURCE1", 7, vec![b'T'; 10]),
                ("SOURCE1", 9, vec![b'G'; 3]),
            ],
            expected: json!({
                "fileType": "CRAM",
                "referenceArn": REFERENCE_ARN,
                "description": "aligned reads",
            }),
            absent: vec!["sequenceInformation", "etag"],
        },
        CompleteCase {
            case: "the highest part number the model admits",
            source_file_type: "UBAM",
            upload: json!({}),
            parts: vec![
                ("SOURCE1", 1, vec![0x42; 128]),
                ("SOURCE1", last_number, vec![0x43; 5]),
            ],
            expected: json!({ "fileType": "UBAM" }),
            absent: vec!["description", "referenceArn", "sequenceInformation", "etag"],
        },
    ]
}

/// The `FileInformation` a source's parts must be reported as: how many parts formed the
/// file, the size of its first part, and the total content length.
fn expected_file(parts: &[(&'static str, i32, Vec<u8>)], part_source: &str) -> Option<Value> {
    let mut of_source: Vec<&(&'static str, i32, Vec<u8>)> = parts
        .iter()
        .filter(|(source, _, _)| *source == part_source)
        .collect();
    of_source.sort_by_key(|(_, number, _)| *number);
    let first = of_source.first()?;
    let content_length: usize = of_source.iter().map(|(_, _, payload)| payload.len()).sum();
    Some(json!({
        "totalParts": of_source.len(),
        "partSize": first.2.len(),
        "contentLength": content_length,
    }))
}

/// The content a source's file must hold: its parts concatenated in ascending part-number
/// order, whatever order they were uploaded or listed in.
fn expected_content(parts: &[(&'static str, i32, Vec<u8>)], part_source: &str) -> Vec<u8> {
    let mut of_source: Vec<&(&'static str, i32, Vec<u8>)> = parts
        .iter()
        .filter(|(source, _, _)| *source == part_source)
        .collect();
    of_source.sort_by_key(|(_, number, _)| *number);
    of_source
        .iter()
        .flat_map(|(_, _, payload)| payload.clone())
        .collect()
}

#[tokio::test]
async fn completing_an_upload_forms_a_read_set_from_the_parts_it_lists() {
    let (router, engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the completion destination").await;
    let mut read_set_ids: Vec<String> = Vec::new();

    for row in complete_cases() {
        let case = row.case;
        let mut upload_body = row.upload.as_object().cloned().unwrap_or_default();
        upload_body.insert("name".to_string(), Value::String(case.to_string()));
        upload_body.insert(
            "sourceFileType".to_string(),
            Value::String(row.source_file_type.to_string()),
        );
        upload_body.insert(
            "subjectId".to_string(),
            Value::String(format!("subject for {case}")),
        );
        upload_body.insert(
            "sampleId".to_string(),
            Value::String(format!("sample for {case}")),
        );
        let (status, body) = create_upload(&router, &store, &Value::Object(upload_body)).await;
        let upload = created_upload_id(case, status, &body);

        let mut listed = upload_all(&router, case, &store, &upload, &row.parts).await;
        // Listed in the REVERSE of the order they were uploaded: the read set's files are
        // assembled by part number, not by the order the request happened to name them.
        listed.reverse();
        let (status, body) = complete_upload(&router, &store, &upload, &parts_list(&listed)).await;
        let read_set_id = completed_read_set_id(case, status, &body);
        assert!(
            !read_set_ids.contains(&read_set_id),
            "case {case:?}: readSetId {read_set_id} was already minted for another read set"
        );
        read_set_ids.push(read_set_id.clone());

        // The read set is readable back through its own Get, every member from the one
        // stored record. The status is the transient one the completion leaves behind
        // until the clock moves it, so both values are admitted here and the move itself
        // is awaited below.
        let (status, metadata) = read_set_metadata(&router, &store, &read_set_id).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {metadata:?}");
        let reported_status = metadata.get("status").and_then(Value::as_str);
        assert!(
            reported_status == Some(READ_SET_STATUS_PROCESSING_UPLOAD)
                || reported_status == Some(READ_SET_STATUS_ACTIVE),
            "case {case:?}: expected a read set still being processed or already active, \
             got {metadata:?}"
        );
        assert_members(
            case,
            "GetReadSetMetadata",
            &json!({
                "id": read_set_id,
                "sequenceStoreId": store,
                "subjectId": format!("subject for {case}"),
                "sampleId": format!("sample for {case}"),
                "name": case,
                "creationType": CREATION_TYPE_UPLOAD,
                // The upload whose completion formed it: the one job this service ran.
                "creationJobId": upload,
            }),
            &metadata,
        );
        assert_members(case, "GetReadSetMetadata", &row.expected, &metadata);
        assert_absent(case, &row.absent, &metadata);
        assert_timestamp(case, "creationTime", &metadata);
        let arn = metadata
            .get("arn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: response carried no arn, got {metadata:?}"));
        assert_arn(
            case,
            arn,
            &format!("sequenceStore/{store}/readSet"),
            &read_set_id,
        );

        // One `FileInformation` per source the parts came from, read off those parts.
        let files = metadata
            .get("files")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: response carried no files, got {metadata:?}"));
        for part_source in PART_SOURCES {
            let member = match *part_source {
                "SOURCE1" => "source1",
                _ => "source2",
            };
            match expected_file(&row.parts, part_source) {
                Some(expected) => assert_eq!(
                    files.get(member),
                    Some(&expected),
                    "case {case:?}: expected {member} = {expected}, got {files:?}"
                ),
                None => assert!(
                    files.get(member).is_none(),
                    "case {case:?}: expected no {member}, got {files:?}"
                ),
            }
        }
        // No index file is reported: nothing here builds one.
        assert_absent(case, &["index"], &files);

        // And the read set really holds the uploaded bytes, in part-number order.
        {
            let state = service_state(&*engine).expect("the omics service state");
            let registry = state.lock();
            let stored = registry
                .read_set(&store, &read_set_id)
                .unwrap_or_else(|| panic!("case {case:?}: no read set {read_set_id} is stored"));
            for part_source in PART_SOURCES {
                let expected = expected_content(&row.parts, part_source);
                match stored.sources.get(*part_source) {
                    Some(file) => assert_eq!(
                        file.content, expected,
                        "case {case:?}: {part_source} must hold its parts in part-number \
                         order, got {} bytes",
                        file.content.len()
                    ),
                    None => assert!(
                        expected.is_empty(),
                        "case {case:?}: expected a stored {part_source} file of {} bytes",
                        expected.len()
                    ),
                }
            }
            // The upload is spent: the completion consumed it.
            assert!(
                registry
                    .multipart_read_set_upload(&store, &upload)
                    .is_none(),
                "case {case:?}: upload {upload} must not survive its completion"
            );
        }

        // Which is also what the wire says: nothing can be listed on the spent upload.
        let (status, body) = list_parts(
            &router,
            &store,
            &upload,
            "",
            &json!({ "partSource": "SOURCE1" }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from ListReadSetUploadParts on a completed \
             upload, got {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE),
            "case {case:?}: expected the upload-not-found message, got {body:?}"
        );

        // The status the clock owes the read set arrives, and its creation instant does
        // not move with it.
        let settled = await_read_set_status(&router, &store, &read_set_id, READ_SET_STATUS_ACTIVE)
            .await;
        assert_eq!(
            settled.get("creationTime"),
            metadata.get("creationTime"),
            "case {case:?}: the creation instant must not move, got {settled:?}"
        );
    }
}

#[tokio::test]
async fn a_completion_this_emulator_cannot_place_is_refused() {
    let (router, engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the refusal destination").await;
    let (status, body) = create_upload(&router, &store, &upload_request("a refused completion"))
        .await;
    let upload = created_upload_id("the fixture upload", status, &body);
    let uploaded = upload_all(
        &router,
        "the fixture upload",
        &store,
        &upload,
        &[
            ("SOURCE1", 1, vec![b'A'; 32]),
            ("SOURCE2", 1, vec![b'C'; 16]),
        ],
    )
    .await;
    let good = parts_list(&uploaded);
    let checksum = uploaded[0].2.clone();
    let other_checksum = uploaded[1].2.clone();

    // Each row is a way a completion can fail to name the parts the upload holds, plus
    // the two ids it resolves; every one is refused and forms no read set.
    struct RefusalCase {
        /// How the row is reported in a failure.
        case: &'static str,
        sequence_store_id: String,
        upload_id: String,
        parts: Value,
        expected_status: u16,
        expected_code: &'static str,
        /// The modelled message, where the refusal is one this service words for the
        /// whole resource class.
        message: Option<&'static str>,
    }
    let rows = vec![
        RefusalCase {
            case: "no part at all",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            parts: Value::Array(Vec::new()),
            expected_status: 400,
            expected_code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a part number the upload does not hold",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            parts: parts_list(&[("SOURCE1", 2, checksum.clone())]),
            expected_status: 400,
            expected_code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a source the part was not uploaded from",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            parts: parts_list(&[("SOURCE2", 2, other_checksum.clone())]),
            expected_status: 400,
            expected_code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "a checksum that is not the stored part's",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            parts: parts_list(&[("SOURCE1", 1, other_checksum.clone())]),
            expected_status: 400,
            expected_code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "the same part listed twice",
            sequence_store_id: store.clone(),
            upload_id: upload.clone(),
            parts: parts_list(&[
                ("SOURCE1", 1, checksum.clone()),
                ("SOURCE1", 1, checksum.clone()),
            ]),
            expected_status: 400,
            expected_code: "ValidationException",
            message: None,
        },
        RefusalCase {
            case: "an upload id this emulator does not hold",
            sequence_store_id: store.clone(),
            upload_id: "9999999999".to_string(),
            parts: good.clone(),
            expected_status: 404,
            expected_code: "ResourceNotFoundException",
            message: Some(MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE),
        },
        RefusalCase {
            case: "a sequence store this emulator does not hold",
            sequence_store_id: "9999999999".to_string(),
            upload_id: upload.clone(),
            parts: good.clone(),
            expected_status: 404,
            expected_code: "ResourceNotFoundException",
            message: Some(SEQUENCE_STORE_NOT_FOUND_MESSAGE),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, body) = complete_upload(
            &router,
            &row.sequence_store_id,
            &row.upload_id,
            &row.parts,
        )
        .await;
        assert_eq!(
            status, row.expected_status,
            "case {case:?}: expected {}, got {body:?}",
            row.expected_status
        );
        assert_eq!(
            error_code(&body),
            row.expected_code,
            "case {case:?}: expected {}, got {body:?}",
            row.expected_code
        );
        if let Some(message) = row.message {
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(message),
                "case {case:?}: expected the modelled message, got {body:?}"
            );
        }
        // Nothing was formed and nothing was spent: the upload is still there with its
        // parts, and the store holds no read set.
        let stored = stored_upload(&engine, &store, &upload);
        assert_eq!(
            stored.parts.len(),
            uploaded.len(),
            "case {case:?}: the refused completion must leave the upload's parts alone, \
             got {stored:?}"
        );
        let state = service_state(&*engine).expect("the omics service state");
        let registry = state.lock();
        assert_eq!(
            registry.read_sets_for(&store).count(),
            0,
            "case {case:?}: a refused completion must form no read set"
        );
    }

    // A read set no store holds is not found, and so is one addressed through another
    // store: a read set is resolved inside the store the request names.
    let elsewhere = create_store(&router, "another store").await;
    let listed = parts_list(&uploaded);
    let (status, body) = complete_upload(&router, &store, &upload, &listed).await;
    let read_set_id = completed_read_set_id("the accepted completion", status, &body);
    for (case, sequence_store_id, id) in [
        ("a read set id nothing holds", store.as_str(), "9999999999"),
        (
            "a read set addressed through another store",
            elsewhere.as_str(),
            read_set_id.as_str(),
        ),
    ] {
        let (status, body) = read_set_metadata(&router, sequence_store_id, id).await;
        assert_eq!(status, 404, "case {case:?}: expected 404, got {body:?}");
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(READ_SET_NOT_FOUND_MESSAGE),
            "case {case:?}: expected the read-set-not-found message, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_sequence_store_holding_a_read_set_cannot_be_deleted() {
    let (router, engine) = emulator_holding_its_engine();
    let empty = create_store(&router, "an empty store").await;
    let filled = create_store(&router, "a store with a read set").await;
    let (status, body) = create_upload(&router, &filled, &upload_request("the held read set"))
        .await;
    let upload = created_upload_id("the held read set", status, &body);
    let uploaded = upload_all(
        &router,
        "the held read set",
        &filled,
        &upload,
        &[("SOURCE1", 1, vec![b'A'; 48])],
    )
    .await;
    let (status, body) = complete_upload(&router, &filled, &upload, &parts_list(&uploaded)).await;
    let read_set_id = completed_read_set_id("the held read set", status, &body);

    // The store is not empty, so the delete is refused and the store stays.
    let (status, body) = delete_sequence_store(&router, &filled).await;
    assert_eq!(
        status, 409,
        "case \"a store holding a read set\": expected 409, got {body:?}"
    );
    assert_eq!(
        error_code(&body),
        "ConflictException",
        "case \"a store holding a read set\": expected ConflictException, got {body:?}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(RESOURCE_HAS_DEPENDENTS_MESSAGE),
        "case \"a store holding a read set\": expected the modelled message, got {body:?}"
    );
    let (status, body) = read_set_metadata(&router, &filled, &read_set_id).await;
    assert_eq!(
        status, 200,
        "case \"a store holding a read set\": the read set must survive the refused \
         delete, got {body:?}"
    );

    // A store holding none deletes as before, so what the refusal is about is the read
    // set and not the delete itself.
    let (status, body) = delete_sequence_store(&router, &empty).await;
    assert_eq!(
        status, 200,
        "case \"an empty store\": expected 200, got {body:?}"
    );
    let state = service_state(&*engine).expect("the omics service state");
    let registry = state.lock();
    assert!(
        registry.sequence_store(&filled).is_some(),
        "case \"a store holding a read set\": the store must survive its refused delete"
    );
    assert!(
        registry.sequence_store(&empty).is_none(),
        "case \"an empty store\": the store must be gone"
    );
}

// ---------------------------------------------------------------------------
// ListReadSets
// ---------------------------------------------------------------------------
//
// The rules the cases below assert:
//
// * `ListReadSets` reports exactly the read sets the one sequence store the request names
//   still holds, in ascending `readSetId` order — which is creation order — each member
//   read from that read set's own stored record, so a listed read set reports what its own
//   `GetReadSetMetadata` reports and nothing else;
// * the listing pages over them by `maxResults`/`nextToken`, and a token it did not issue
//   is refused;
// * every member of the filter narrows the listing on its own, a member the filter leaves
//   out narrows nothing, and both instant bounds are inclusive;
// * a read set the store no longer holds leaves the listing;
// * a sequence store id no store holds is the modelled 404.

/// The published bounds of the modelled `maxResults` range (1..100).
/// Source: the `maxResults` member of `ListReadSetsRequest`.
const MAX_RESULTS_RANGE: (i64, i64) = (1, 100);

/// Every member the modelled `ReadSetListItem` carries.
/// Source: `smithy_query(query="shape",shape="ReadSetListItem")`.
const READ_SET_LIST_ITEM_MEMBERS: &[&str] = &[
    "id",
    "arn",
    "sequenceStoreId",
    "subjectId",
    "sampleId",
    "status",
    "name",
    "description",
    "referenceArn",
    "fileType",
    "sequenceInformation",
    "creationTime",
    "statusMessage",
    "creationType",
    "etag",
];

/// The members of `ReadSetListItem` the model makes required, which every listed read set
/// therefore has to carry.
/// Source: the same shape — `id`, `arn`, `sequenceStoreId`, `status`, `fileType` and
/// `creationTime` are `@required`.
const READ_SET_LIST_ITEM_REQUIRED: &[&str] = &[
    "id",
    "arn",
    "sequenceStoreId",
    "status",
    "fileType",
    "creationTime",
];

/// The members `GetReadSetMetadata` reports that the listing's item does NOT model, so a
/// listed read set carries neither.
/// Source: `ReadSetListItem` has no `files` and no `creationJobId` member.
const NOT_IN_READ_SET_LIST_ITEM: &[&str] = &["files", "creationJobId"];

/// The modelled `CreationType` an uploaded read set does NOT carry.
/// Source: `smithy_query(query="shape",shape="CreationType")` -> IMPORT | UPLOAD.
const CREATION_TYPE_IMPORT: &str = "IMPORT";

/// A modelled `ReadSetStatus` no read set this emulator forms ever carries.
/// Source: `smithy_query(query="shape",shape="ReadSetStatus")` -> ARCHIVED | ACTIVATING |
/// ACTIVE | DELETING | DELETED | PROCESSING_UPLOAD | UPLOAD_FAILED.
const READ_SET_STATUS_ARCHIVED: &str = "ARCHIVED";

/// `POST /sequencestore/{store}/readsets{query}`, carrying `filter` when the case names
/// one: `maxResults` and `nextToken` are `@httpQuery`, the filter is the request body.
async fn list_read_sets(
    router: &MultiServiceRouter,
    sequence_store_id: &str,
    query: &str,
    filter: Option<&Value>,
) -> (u16, Value) {
    let body = match filter {
        Some(filter) => json!({ "filter": filter }),
        None => json!({}),
    };
    call(
        router,
        "POST",
        &format!("/sequencestore/{sequence_store_id}/readsets{query}"),
        Some(&body),
    )
    .await
}

/// The `readSets` of a 200 listing response, after asserting the status.
fn listed_read_sets(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(status, 200, "case {case:?}: expected 200, got {body:?}");
    body.get("readSets")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: response carried no readSets, got {body:?}"))
        .clone()
}

/// The `id` of each listed read set, in the order the listing reported them.
fn listed_ids(case: &str, items: &[Value]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a listed read set carried no id, got {item:?}")
                })
                .to_string()
        })
        .collect()
}

/// A row of the read set table: how the row is reported in a failure, and the members the
/// upload that forms it carries.
struct ListedReadSet {
    case: &'static str,
    upload: Value,
}

/// The read sets every listing case below is asserted over, so each rule is checked over a
/// class of read sets and not over one. The rows differ along the source format, the
/// subject and the sample they came from, where the sequence was generated, whether they
/// name a reference genome and whether they carry a description — so every filter member
/// has rows on both sides of it, sharing values across rows where a filter must select
/// several.
fn listed_read_set_table() -> Vec<ListedReadSet> {
    vec![
        ListedReadSet {
            case: "a single-source FASTQ",
            upload: json!({
                "name": "a single-source FASTQ",
                "sourceFileType": "FASTQ",
                "subjectId": "subject-alpha",
                "sampleId": "sample-one",
            }),
        },
        ListedReadSet {
            case: "a described FASTQ",
            upload: json!({
                "name": "a described FASTQ",
                "sourceFileType": "FASTQ",
                "subjectId": "subject-alpha",
                "sampleId": "sample-two",
                "description": "the second cohort",
            }),
        },
        ListedReadSet {
            case: "a FASTQ with a named origin",
            upload: json!({
                "name": "a FASTQ with a named origin",
                "sourceFileType": "FASTQ",
                "subjectId": "subject-beta",
                "sampleId": "sample-one",
                "generatedFrom": "1000 Genomes",
            }),
        },
        ListedReadSet {
            case: "an aligned CRAM",
            upload: json!({
                "name": "an aligned CRAM",
                "sourceFileType": "CRAM",
                "subjectId": "subject-beta",
                "sampleId": "sample-three",
                "referenceArn": REFERENCE_ARN,
                "description": "aligned reads",
            }),
        },
        ListedReadSet {
            case: "a UBAM",
            upload: json!({
                "name": "a UBAM",
                "sourceFileType": "UBAM",
                "subjectId": "subject-gamma",
                "sampleId": "sample-one",
            }),
        },
    ]
}

/// Form one read set per row of [`listed_read_set_table`] inside `store`, each out of one
/// uploaded part, and answer their ids in the order they were formed.
async fn form_listed_read_sets(router: &MultiServiceRouter, store: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for row in listed_read_set_table() {
        let (status, body) = create_upload(router, store, &row.upload).await;
        let upload = created_upload_id(row.case, status, &body);
        let uploaded = upload_all(
            router,
            row.case,
            store,
            &upload,
            &[("SOURCE1", 1, row.case.as_bytes().to_vec())],
        )
        .await;
        let (status, body) = complete_upload(router, store, &upload, &parts_list(&uploaded)).await;
        ids.push(completed_read_set_id(row.case, status, &body));
    }
    ids
}

/// Wait until every read set of `ids` carries the status the clock owes it, so a case that
/// reads or filters on the status sees a settled store rather than a race.
async fn await_all_active(router: &MultiServiceRouter, store: &str, ids: &[String]) {
    for id in ids {
        await_read_set_status(router, store, id, READ_SET_STATUS_ACTIVE).await;
    }
}

/// The read set ids the rows named by `cases` were formed with, in table order.
fn ids_of(cases: &[&str], ids: &[String]) -> Vec<String> {
    listed_read_set_table()
        .iter()
        .zip(ids)
        .filter(|(row, _)| cases.contains(&row.case))
        .map(|(_, id)| id.clone())
        .collect()
}

#[tokio::test]
async fn list_read_sets_reports_the_read_sets_its_store_holds() {
    let (router, _engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the listed store").await;
    let elsewhere = create_store(&router, "another listed store").await;
    let ids = form_listed_read_sets(&router, &store).await;
    let elsewhere_ids = form_listed_read_sets(&router, &elsewhere).await;
    await_all_active(&router, &store, &ids).await;

    let case = "every read set the store holds";
    let (status, body) = list_read_sets(&router, &store, "", None).await;
    let items = listed_read_sets(case, status, &body);
    assert_eq!(
        listed_ids(case, &items),
        ids,
        "case {case:?}: expected the store's read sets in creation order, got {body:?}"
    );
    // The last page carries no token: everything the store holds fitted in it.
    assert_absent(case, &["nextToken"], &body);
    // Ascending `readSetId`, which is what lets a page token name a resume point.
    let keys: Vec<u64> = ids
        .iter()
        .map(|id| id.parse().expect("a numeric readSetId"))
        .collect();
    assert!(
        keys.windows(2).all(|pair| pair[0] < pair[1]),
        "case {case:?}: expected ascending read set ids, got {keys:?}"
    );

    // Every member of a listed read set is the stored record's, which is exactly what that
    // read set's own `GetReadSetMetadata` reports.
    for (row, (id, item)) in listed_read_set_table()
        .iter()
        .zip(ids.iter().zip(items.iter()))
    {
        let case = row.case;
        let (status, metadata) = read_set_metadata(&router, &store, id).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {metadata:?}");
        for member in READ_SET_LIST_ITEM_MEMBERS {
            assert_eq!(
                item.get(member),
                metadata.get(member),
                "case {case:?}: the listed {member} must be the one GetReadSetMetadata \
                 reports, got {item:?} against {metadata:?}"
            );
        }
        for member in READ_SET_LIST_ITEM_REQUIRED {
            assert!(
                item.get(member).is_some(),
                "case {case:?}: a listed read set must carry {member}, got {item:?}"
            );
        }
        assert_absent(case, NOT_IN_READ_SET_LIST_ITEM, item);
        assert_members(
            case,
            "ListReadSets",
            &json!({
                "id": id,
                "sequenceStoreId": store,
                "name": case,
                "status": READ_SET_STATUS_ACTIVE,
                "creationType": CREATION_TYPE_UPLOAD,
            }),
            item,
        );
        assert_timestamp(case, "creationTime", item);
    }

    // A read set belongs to the store that holds it: the other store's read sets are
    // neither in this listing nor mixed into its own.
    for id in &elsewhere_ids {
        assert!(
            !ids.contains(id),
            "case {case:?}: read set {id} was minted for another store"
        );
    }
    let case = "the other store's own listing";
    let (status, body) = list_read_sets(&router, &elsewhere, "", None).await;
    let items = listed_read_sets(case, status, &body);
    assert_eq!(
        listed_ids(case, &items),
        elsewhere_ids,
        "case {case:?}: expected only the other store's read sets, got {body:?}"
    );
}

#[tokio::test]
async fn list_read_sets_narrows_on_every_filter_member() {
    let (router, _engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the filtered store").await;
    let ids = form_listed_read_sets(&router, &store).await;
    await_all_active(&router, &store, &ids).await;

    /// A row of the filter table: the filter a case sends and the rows it must select.
    struct FilterCase {
        case: &'static str,
        filter: Value,
        selects: &'static [&'static str],
    }
    let every_row: &[&str] = &[
        "a single-source FASTQ",
        "a described FASTQ",
        "a FASTQ with a named origin",
        "an aligned CRAM",
        "a UBAM",
    ];
    let rows = vec![
        FilterCase {
            case: "a filter naming no member at all",
            filter: json!({}),
            selects: every_row,
        },
        FilterCase {
            case: "a name one read set carries",
            filter: json!({ "name": "a described FASTQ" }),
            selects: &["a described FASTQ"],
        },
        FilterCase {
            case: "a name nothing carries",
            filter: json!({ "name": "no read set is called this" }),
            selects: &[],
        },
        FilterCase {
            case: "the status every settled read set carries",
            filter: json!({ "status": READ_SET_STATUS_ACTIVE }),
            selects: every_row,
        },
        FilterCase {
            case: "a modelled status none carries",
            filter: json!({ "status": READ_SET_STATUS_ARCHIVED }),
            selects: &[],
        },
        FilterCase {
            case: "a sample three read sets share",
            filter: json!({ "sampleId": "sample-one" }),
            selects: &[
                "a single-source FASTQ",
                "a FASTQ with a named origin",
                "a UBAM",
            ],
        },
        FilterCase {
            case: "a subject two read sets share",
            filter: json!({ "subjectId": "subject-beta" }),
            selects: &["a FASTQ with a named origin", "an aligned CRAM"],
        },
        FilterCase {
            case: "where the sequence was generated",
            filter: json!({ "generatedFrom": "1000 Genomes" }),
            selects: &["a FASTQ with a named origin"],
        },
        FilterCase {
            case: "the creation type an uploaded read set carries",
            filter: json!({ "creationType": CREATION_TYPE_UPLOAD }),
            selects: every_row,
        },
        FilterCase {
            case: "the other modelled creation type",
            filter: json!({ "creationType": CREATION_TYPE_IMPORT }),
            selects: &[],
        },
        FilterCase {
            case: "a reference genome one read set is aligned against",
            filter: json!({ "referenceArn": REFERENCE_ARN }),
            selects: &["an aligned CRAM"],
        },
        FilterCase {
            case: "a reference genome nothing is aligned against",
            filter: json!({
                "referenceArn":
                    "arn:aws:omics:us-west-2:000000000000:referenceStore/1000000000/reference/9",
            }),
            selects: &[],
        },
        FilterCase {
            // The modelled `ReferenceArnFilter` pattern is `^$|^arn:.+$` with length
            // 0..127, so the empty string is a value the member admits; it names no
            // reference genome, so it narrows nothing.
            case: "the empty reference ARN the model admits",
            filter: json!({ "referenceArn": "" }),
            selects: every_row,
        },
        FilterCase {
            case: "two members that agree on one read set",
            filter: json!({ "name": "a UBAM", "sampleId": "sample-one" }),
            selects: &["a UBAM"],
        },
        FilterCase {
            case: "two members that agree on none",
            filter: json!({ "name": "a UBAM", "sampleId": "sample-two" }),
            selects: &[],
        },
        FilterCase {
            case: "a subject and a sample that meet in one read set",
            filter: json!({ "subjectId": "subject-alpha", "sampleId": "sample-one" }),
            selects: &["a single-source FASTQ"],
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, body) = list_read_sets(&router, &store, "", Some(&row.filter)).await;
        let items = listed_read_sets(case, status, &body);
        assert_eq!(
            listed_ids(case, &items),
            ids_of(row.selects, &ids),
            "case {case:?}: expected the read sets {:?}, got {body:?}",
            row.selects
        );
    }
}

#[tokio::test]
async fn list_read_sets_bounds_the_listing_by_creation_instant() {
    let (router, _engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the time-filtered store").await;
    let ids = form_listed_read_sets(&router, &store).await;
    await_all_active(&router, &store, &ids).await;

    // What each read set reports as its own creation instant: the wire value a filter
    // bound carries, and the instant the expectation is computed from — so the rule is
    // asserted against the store's own instants rather than against an assumed ordering.
    let mut instants: Vec<(String, String, f64)> = Vec::new();
    for id in &ids {
        let (status, metadata) = read_set_metadata(&router, &store, id).await;
        assert_eq!(status, 200, "read set {id}: expected 200, got {metadata:?}");
        let text = metadata
            .get("creationTime")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("read set {id}: no creationTime, got {metadata:?}"))
            .to_string();
        let seconds = assert_timestamp(id, "creationTime", &metadata);
        instants.push((id.clone(), text, seconds));
    }

    for (id, text, seconds) in &instants {
        // Both bounds are INCLUSIVE, so a read set's own instant selects it.
        let case = format!("createdAfter the instant of read set {id}");
        let (status, body) =
            list_read_sets(&router, &store, "", Some(&json!({ "createdAfter": text }))).await;
        let items = listed_read_sets(&case, status, &body);
        let expected: Vec<String> = instants
            .iter()
            .filter(|(_, _, other)| other >= seconds)
            .map(|(other, _, _)| other.clone())
            .collect();
        assert_eq!(
            listed_ids(&case, &items),
            expected,
            "case {case:?}: expected every read set created at or after {text}, got {body:?}"
        );

        let case = format!("createdBefore the instant of read set {id}");
        let (status, body) =
            list_read_sets(&router, &store, "", Some(&json!({ "createdBefore": text }))).await;
        let items = listed_read_sets(&case, status, &body);
        let expected: Vec<String> = instants
            .iter()
            .filter(|(_, _, other)| other <= seconds)
            .map(|(other, _, _)| other.clone())
            .collect();
        assert_eq!(
            listed_ids(&case, &items),
            expected,
            "case {case:?}: expected every read set created at or before {text}, got {body:?}"
        );

        // The two bounds together are a window, and the narrowest one still holds the read
        // set whose instant it was taken from.
        let case = format!("the one-instant window of read set {id}");
        let (status, body) = list_read_sets(
            &router,
            &store,
            "",
            Some(&json!({ "createdAfter": text, "createdBefore": text })),
        )
        .await;
        let items = listed_read_sets(&case, status, &body);
        assert!(
            listed_ids(&case, &items).contains(id),
            "case {case:?}: expected the window to hold read set {id}, got {body:?}"
        );
    }

    // A window that closes before the store held anything selects nothing, and one that
    // opens after it selects nothing either.
    let (earliest, _, _) = instants.first().expect("at least one read set");
    let (status, metadata) = read_set_metadata(&router, &store, earliest).await;
    assert_eq!(status, 200, "expected 200, got {metadata:?}");
    for (case, filter) in [
        (
            "a window closing before the store held anything",
            json!({ "createdBefore": "2001-01-01T00:00:00Z" }),
        ),
        (
            "a window opening long after it",
            json!({ "createdAfter": "2999-01-01T00:00:00Z" }),
        ),
    ] {
        let (status, body) = list_read_sets(&router, &store, "", Some(&filter)).await;
        let items = listed_read_sets(case, status, &body);
        assert!(
            items.is_empty(),
            "case {case:?}: expected no read set, got {body:?}"
        );
    }
}

#[tokio::test]
async fn list_read_sets_pages_over_its_store_in_creation_order() {
    let (router, _engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the paged store").await;
    let ids = form_listed_read_sets(&router, &store).await;
    await_all_active(&router, &store, &ids).await;

    let (smallest, largest) = MAX_RESULTS_RANGE;
    for page_size in [smallest, 2, 3, ids.len() as i64, largest] {
        let case = format!("maxResults={page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let query = match &token {
                Some(token) => format!("?maxResults={page_size}&nextToken={token}"),
                None => format!("?maxResults={page_size}"),
            };
            let (status, body) = list_read_sets(&router, &store, &query, None).await;
            let items = listed_read_sets(&case, status, &body);
            assert!(
                items.len() as i64 <= page_size,
                "case {case:?}: expected at most {page_size} read sets in a page, got {body:?}"
            );
            seen.extend(listed_ids(&case, &items));
            assert!(
                seen.len() <= ids.len(),
                "case {case:?}: the pages reported more read sets than the store holds, \
                 got {seen:?}"
            );
            match body.get("nextToken").and_then(Value::as_str) {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, ids,
            "case {case:?}: paging must report every read set exactly once, in creation order"
        );
    }

    // A page size outside the modelled range is refused by the constraint walk, so the
    // page a request can ask for is bounded on both sides.
    for page_size in [smallest - 1, largest + 1] {
        let case = format!("maxResults={page_size}");
        let (status, body) =
            list_read_sets(&router, &store, &format!("?maxResults={page_size}"), None).await;
        assert_eq!(status, 400, "case {case:?}: expected 400, got {body:?}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // A `nextToken` this listing did not issue resumes nothing, so it is refused rather
    // than read as a position in the store.
    for (case, token) in [
        ("a token this service never issued", "notatoken".to_string()),
        (
            "another listing's token",
            format!(
                "{MULTIPART_READ_SET_UPLOAD_PAGE_TOKEN_TAG}{}",
                hex::encode(&ids[0])
            ),
        ),
        (
            "a read set token naming no read set",
            READ_SET_PAGE_TOKEN_TAG.to_string(),
        ),
    ] {
        let (status, body) =
            list_read_sets(&router, &store, &format!("?nextToken={token}"), None).await;
        assert_eq!(status, 400, "case {case:?}: expected 400, got {body:?}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body:?}"
        );
    }

    // And the token this listing DOES issue resumes it: the page after the first read set
    // is the rest of the store.
    let case = "the token the first page issued";
    let (status, body) = list_read_sets(&router, &store, "?maxResults=1", None).await;
    let items = listed_read_sets(case, status, &body);
    assert_eq!(
        listed_ids(case, &items),
        ids[..1].to_vec(),
        "case {case:?}: expected the first read set, got {body:?}"
    );
    let token = body
        .get("nextToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a nextToken, got {body:?}"));
    let (status, body) = list_read_sets(
        &router,
        &store,
        &format!("?nextToken={token}&maxResults={largest}"),
        None,
    )
    .await;
    let items = listed_read_sets(case, status, &body);
    assert_eq!(
        listed_ids(case, &items),
        ids[1..].to_vec(),
        "case {case:?}: expected the rest of the store, got {body:?}"
    );
}

#[tokio::test]
async fn a_read_set_its_store_no_longer_holds_leaves_the_listing() {
    let (router, engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the emptied store").await;
    let ids = form_listed_read_sets(&router, &store).await;
    await_all_active(&router, &store, &ids).await;

    // A read set reaches the terminal `DELETED` status through `BatchDeleteReadSet`, which
    // is still a 501 in this crate, so the status is set on the stored record through the
    // service's own state — the one record every read set operation reads — rather than
    // through a call this emulator cannot answer yet.
    let released = ids[1].clone();
    {
        let state = service_state(&*engine).expect("the omics service state");
        let mut registry = state.lock();
        registry
            .read_set_mut(&store, &released)
            .unwrap_or_else(|| panic!("no read set {released} is stored"))
            .status = READ_SET_STATUS_DELETED.to_string();
    }

    let case = "a read set the store no longer holds";
    let (status, body) = list_read_sets(&router, &store, "", None).await;
    let items = listed_read_sets(case, status, &body);
    let expected: Vec<String> = ids.iter().filter(|id| **id != released).cloned().collect();
    assert_eq!(
        listed_ids(case, &items),
        expected,
        "case {case:?}: expected the released read set to leave the listing, got {body:?}"
    );

    // Including when the filter asks for its status by name: it is no longer among the
    // store's read sets at all.
    let (status, body) = list_read_sets(
        &router,
        &store,
        "",
        Some(&json!({ "status": READ_SET_STATUS_DELETED })),
    )
    .await;
    let items = listed_read_sets(case, status, &body);
    assert!(
        items.is_empty(),
        "case {case:?}: expected no read set, got {body:?}"
    );
}

#[tokio::test]
async fn list_read_sets_refuses_a_sequence_store_nothing_holds() {
    let (router, _engine) = emulator_holding_its_engine();
    let store = create_store(&router, "the only store").await;

    // However the id is shaped inside the modelled `SequenceStoreId` (len 10..36, decimal):
    // one no store holds is the modelled 404, and so is one wider than any id this
    // emulator mints.
    for (case, sequence_store_id) in [
        ("an id no store holds", "9999999999".to_string()),
        ("an id at the top of the modelled length", "9".repeat(36)),
        (
            "an id one digit past the ids this emulator mints",
            format!("{}0", FIRST_SEQUENCE_STORE_ID),
        ),
    ] {
        let (status, body) = list_read_sets(&router, &sequence_store_id, "", None).await;
        assert_eq!(status, 404, "case {case:?}: expected 404, got {body:?}");
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(SEQUENCE_STORE_NOT_FOUND_MESSAGE),
            "case {case:?}: expected the modelled message, got {body:?}"
        );
    }

    // A store that holds no read set yet is an EMPTY listing, not a refusal.
    let case = "a store holding no read set";
    let (status, body) = list_read_sets(&router, &store, "", None).await;
    let items = listed_read_sets(case, status, &body);
    assert!(
        items.is_empty(),
        "case {case:?}: expected an empty listing, got {body:?}"
    );
    assert_absent(case, &["nextToken"], &body);
}
