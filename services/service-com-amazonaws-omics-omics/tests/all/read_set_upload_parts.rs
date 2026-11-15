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
//! * `ListReadSetUploadParts` reports exactly the parts stored for the one upload and
//!   source the request names, in ascending part-number order, each member read from the
//!   stored part, paging over them and narrowing them by creation instant;
//! * a request naming a sequence store or an upload this emulator does not hold — or
//!   carrying no content at all — is refused rather than stored.

use serde_json::{json, Value};
use service_com_amazonaws_omics_omics::state::{
    MULTIPART_READ_SET_UPLOAD_NOT_FOUND_MESSAGE, SEQUENCE_STORE_NOT_FOUND_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::multipart_read_set_uploads::{
    create_store, create_upload, created_upload_id, emulator_holding_its_engine, stored_upload,
};
use crate::private_workflow_runs::{
    assert_timestamp, call, error_code, expected_digest, REGION,
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
/// is the hex digest form the service reports.
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
    assert!(
        !checksum.is_empty()
            && checksum
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "case {case:?}: expected a lowercase hex checksum, got {checksum:?}"
    );
    checksum
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
            expected_digest(&part.payload),
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
                Some(expected_digest(&part.payload).as_str()),
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
            expected_digest(&part.payload),
            "case {:?}: the stored checksum must be the digest of the stored bytes",
            part.case
        );
    }
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
            expected_digest(first),
            "case {case:?}: expected the digest of the first content, got {body:?}"
        );
        let listed = listed_part(&router, &store, &upload, case, *part_number).await;
        let created = assert_timestamp(case, "creationTime", &listed);

        let (status, body) =
            upload_part(&router, &store, &upload, "SOURCE1", *part_number, second).await;
        let second_checksum = uploaded_checksum(case, status, &body);
        assert_eq!(
            second_checksum,
            expected_digest(second),
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
