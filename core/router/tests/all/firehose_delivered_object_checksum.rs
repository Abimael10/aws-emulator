// SPDX-License-Identifier: CC0-1.0

//! The checksum an Amazon Data Firehose delivery leaves on the Amazon S3 object it writes.
//!
//! The rule under test: a delivery uploads its object through the same `s3:PutObject` call any
//! other client makes, and that call names a checksum algorithm of its own — `CRC32` — so the
//! delivered object is STORED with a `CRC32` checksum of its own stored bytes, computed over
//! the whole object because a delivery writes the object in one request ("for `PutObject`
//! uploads, the checksum type is always `FULL_OBJECT`", `PutObjectRequest$ChecksumType`).
//! Every S3 read path then reports that one checksum and nothing else: `GetObject` and
//! `HeadObject` answer `x-amz-checksum-crc32` beside `x-amz-checksum-type: FULL_OBJECT` to a
//! read that enabled checksum retrieval ("to retrieve the checksum, this mode must be
//! enabled", `GetObjectRequest$ChecksumMode`) and no checksum header at all to one that did
//! not, and a listing reports `ChecksumAlgorithm` `CRC32` with `ChecksumType` `FULL_OBJECT`
//! for the same object. No other `x-amz-checksum-*` header is reported, because an object
//! carries the checksum of ONE algorithm.
//!
//! Only a cross-service suite can pin that: the write belongs to Firehose and every reading of
//! it to Amazon S3, both over ONE engine exactly as `core/router/src/lib.rs` mounts them.
//! Nothing here writes into a store to arrange what it asserts — the bucket comes from
//! `CreateBucket`, the stream from `CreateDeliveryStream`, the records from `PutRecordBatch`,
//! and every reading is taken through `ListObjectsV2`, `GetObject` and `HeadObject`.
//!
//! The rows differ along what the rule generalises over: the bytes the object ends up holding
//! (one short record, a batch of several, a long one), whether the destination compresses them
//! (so the stored bytes are not the records' own), and which destination member configures the
//! location. One row's digest is checked against a PUBLISHED value — the CRC-32 check value —
//! and every row's against what S3 itself computes for the same bytes under `CRC32`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, element, opt_element};

/// The Region every call below is signed for, and the one the bucket is created in.
const REGION: &str = "us-west-2";

/// The bucket every row delivers into.
const BUCKET: &str = "firehose-delivered-checksum";

/// The `ChecksumAlgorithm` a delivered object is stored under, and the response header its
/// value is bound to (`smithy_query(query="operation",operation="GetObject")` reports
/// `ChecksumCRC32 -> x-amz-checksum-crc32`).
const CRC32: &str = "CRC32";
const CRC32_HEADER: &str = "x-amz-checksum-crc32";

/// `ChecksumType` -> `x-amz-checksum-type`, and the one of its two values
/// (`smithy_query(query="shape",shape="ChecksumType")`: `COMPOSITE | FULL_OBJECT`) a
/// single-request upload carries.
const TYPE_HEADER: &str = "x-amz-checksum-type";
const FULL_OBJECT: &str = "FULL_OBJECT";

/// `ChecksumMode` -> `x-amz-checksum-mode`, and its only value
/// (`smithy_query(query="shape",shape="ChecksumMode")`: `ENABLED`).
const MODE_HEADER: &str = "x-amz-checksum-mode";
const ENABLED: &str = "ENABLED";

/// Every `x-amz-checksum-*` header `GetObjectOutput` binds a `Checksum*` member to, from the
/// model itself (`smithy_query(query="operation",operation="GetObject")` reports
/// `ChecksumCRC32 -> x-amz-checksum-crc32`, `ChecksumCRC32C -> x-amz-checksum-crc32c`,
/// `ChecksumCRC64NVME`, `ChecksumSHA1`, `ChecksumSHA256`, `ChecksumSHA512`, `ChecksumMD5`,
/// `ChecksumXXHASH64`, `ChecksumXXHASH3`, `ChecksumXXHASH128`) — the ten a read of a delivered
/// object must report exactly one of.
const CHECKSUM_HEADERS: [&str; 10] = [
    "x-amz-checksum-crc32",
    "x-amz-checksum-crc32c",
    "x-amz-checksum-crc64nvme",
    "x-amz-checksum-sha1",
    "x-amz-checksum-sha256",
    "x-amz-checksum-sha512",
    "x-amz-checksum-md5",
    "x-amz-checksum-xxhash64",
    "x-amz-checksum-xxhash3",
    "x-amz-checksum-xxhash128",
];

/// `PutObject`'s own `ChecksumAlgorithm` header
/// (`smithy_query(query="operation",operation="PutObject")` reports
/// `ChecksumAlgorithm -> x-amz-sdk-checksum-algorithm`), which is how the oracle below asks
/// Amazon S3 for the `CRC32` of the very bytes the delivery stored.
const ALGORITHM_HEADER: &str = "x-amz-sdk-checksum-algorithm";

/// The CRC-32 of the nine ASCII digits `123456789`, Base64 encoded as a checksum value is.
///
/// A PUBLISHED constant, not a value read out of this emulator: `0xCBF43926` is the check
/// value of CRC-32 (the `CRC-32/ISO-HDLC` algorithm `CRC32` names) over that string, and
/// `y/Q5Jg==` is the Base64 of its four bytes most-significant first. One row delivers exactly
/// those nine bytes, so the digest its object reports is known in advance.
const CHECK_VECTOR_RECORD: &[u8] = b"123456789";
const CHECK_VECTOR_CRC32: &str = "y/Q5Jg==";

/// How long a phase waits for the condition it is about — the stream reporting itself `ACTIVE`,
/// the delivery landing in the destination.
///
/// Generous on purpose: nothing here asserts how long anything took, so this is only the point
/// at which waiting is called a failure.
const PHASE_DEADLINE: Duration = Duration::from_secs(120);

/// How long each look waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    /// The value of the response header `name`, or `None` when the answer carries none.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The answer's body as text, for a message naming what came back.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the routing key
/// the router claims a request by. Nothing here is a credential: this router runs with request
/// authentication off, so no signature is verified.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request to the service `signing_name` serves.
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let (parts, body) = router.dispatch(request).await.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body: body.to_vec(),
    }
}

/// One Firehose operation over its `awsJson1_1` wire.
async fn firehose(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    send(
        router,
        "firehose",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Firehose_20150804.{operation}")),
        ],
        body.to_string().into_bytes(),
    )
    .await
}

/// One path-style Amazon S3 request.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    send(router, "s3", method, path, headers, body).await
}

/// The emulator the suite drives: the two services the flow spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// The role every destination below delivers under.
fn delivery_role() -> String {
    format!(
        "arn:aws:iam::{}:role/firehose-delivered-checksum",
        engine::emulator_account_id()
    )
}

/// One row: what is delivered, how the destination is configured, and the digest the stored
/// object must report where that digest is known in advance.
struct Row {
    case: &'static str,
    /// Names this row's stream and its destination prefix, so rows cannot collide.
    name: &'static str,
    /// The destination member the location is configured through.
    member: &'static str,
    /// The `CompressionFormat` the destination declares, `None` to leave the member out.
    compression: Option<&'static str>,
    /// The records the batch puts.
    records: Vec<Vec<u8>>,
    /// The Base64 `CRC32` the delivered object must report, where it is a published value.
    published_crc32: Option<&'static str>,
}

/// The rows: one per form the delivered bytes and the destination take.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one short record, the destination naming no compression",
            name: "check-vector",
            member: "ExtendedS3DestinationConfiguration",
            compression: None,
            records: vec![CHECK_VECTOR_RECORD.to_vec()],
            published_crc32: Some(CHECK_VECTOR_CRC32),
        },
        Row {
            case: "a batch of several records, uncompressed",
            name: "batch",
            member: "ExtendedS3DestinationConfiguration",
            compression: Some("UNCOMPRESSED"),
            records: (0..4)
                .map(|n| format!("{{\"event\":{n},\"marker\":\"batched-{n}\"}}\n").into_bytes())
                .collect(),
            published_crc32: None,
        },
        Row {
            case: "a long record the destination compresses, so the stored bytes are not the \
                   record's own",
            name: "compressed",
            member: "ExtendedS3DestinationConfiguration",
            compression: Some("GZIP"),
            records: vec!["compressible-".repeat(512).into_bytes()],
            published_crc32: None,
        },
        Row {
            case: "the legacy destination member",
            name: "legacy",
            member: "S3DestinationConfiguration",
            compression: None,
            records: vec![b"{\"event\":\"legacy-destination\"}".to_vec()],
            published_crc32: None,
        },
    ]
}

/// The destination prefix one row delivers under.
fn prefix(row: &Row) -> String {
    format!("{}/", row.name)
}

/// Create `BUCKET`, naming the Region it is created in.
async fn given_bucket(router: &MultiServiceRouter) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{BUCKET}"),
        &[],
        format!(
            "<CreateBucketConfiguration \
             xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <LocationConstraint>{REGION}</LocationConstraint>\
             </CreateBucketConfiguration>"
        )
        .into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "setup: the destination bucket must be created, got {} {}",
        answer.status,
        answer.text()
    );
}

/// Create one row's stream, with its own prefix in `BUCKET`.
async fn given_stream(router: &MultiServiceRouter, row: &Row) {
    let mut config = json!({
        "RoleARN": delivery_role(),
        "BucketARN": format!("arn:aws:s3:::{BUCKET}"),
        "Prefix": prefix(row),
        // The shortest wait `IntervalInSeconds` admits: these rows are about what the stored
        // object reports, not about how long a buffer is held.
        "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
    });
    if let Some(compression) = row.compression {
        config["CompressionFormat"] = json!(compression);
    }
    let mut body = json!({
        "DeliveryStreamName": row.name,
        "DeliveryStreamType": "DirectPut",
    });
    body[row.member] = config;
    let created = firehose(router, "CreateDeliveryStream", body).await;
    assert_eq!(
        created.status,
        200,
        "case {:?}: CreateDeliveryStream must succeed, got {} {}",
        row.case,
        created.status,
        created.text()
    );
}

/// Wait for `row`'s stream to report itself `ACTIVE` — a put is accepted only then.
async fn active(router: &MultiServiceRouter, row: &Row) {
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let described = firehose(
            router,
            "DescribeDeliveryStream",
            json!({ "DeliveryStreamName": row.name }),
        )
        .await;
        let text = described.text();
        let status = serde_json::from_str::<Value>(&text)
            .unwrap_or(Value::Null)
            .pointer("/DeliveryStreamDescription/DeliveryStreamStatus")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the stream must report itself ACTIVE, last read {text}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The listing of `BUCKET` under `prefix`, read through `ListObjectsV2`.
async fn listing(router: &MultiServiceRouter, case: &str, prefix: &str) -> String {
    let answer = s3(
        router,
        "GET",
        &format!("/{BUCKET}?list-type=2&prefix={prefix}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: listing {prefix:?} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
    answer.text()
}

/// The one object delivered under `row`'s prefix, as (its key, the listing that found it).
async fn delivered(router: &MultiServiceRouter, row: &Row) -> (String, String) {
    let under = prefix(row);
    let deadline = Instant::now() + PHASE_DEADLINE;
    loop {
        let body = listing(router, row.case, &under).await;
        let keys: Vec<String> = blocks_of(&body, "Contents")
            .into_iter()
            .filter_map(|entry| opt_element(entry, "Key"))
            .collect();
        if let Some(key) = keys.first() {
            return (key.clone(), body);
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected a delivered object under {under:?}, listing holds {keys:?}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What Amazon S3 itself computes for `body` under `CRC32`: the oracle every row's digest is
/// compared against, taken by uploading those same bytes through `PutObject` naming that
/// algorithm and reading the checksum the write answers with.
async fn s3_crc32_of(router: &MultiServiceRouter, case: &str, name: &str, body: &[u8]) -> String {
    let written = s3(
        router,
        "PUT",
        &format!("/{BUCKET}/oracle/{name}"),
        &[(ALGORITHM_HEADER, CRC32)],
        body.to_vec(),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: the oracle upload must succeed, got {} {}",
        written.status,
        written.text()
    );
    let reported_type = written.header(TYPE_HEADER);
    assert_eq!(
        reported_type.as_deref(),
        Some(FULL_OBJECT),
        "case {case:?}: a single-request upload under {CRC32} is stored {FULL_OBJECT}, got \
         {reported_type:?}"
    );
    written.header(CRC32_HEADER).unwrap_or_else(|| {
        panic!("case {case:?}: the oracle upload must report {CRC32_HEADER}, got none")
    })
}

#[tokio::test]
async fn a_delivered_object_is_stored_with_a_full_object_crc32_of_its_own_bytes() {
    let router = emulator();
    given_bucket(&router).await;

    // Every stream is created before any of them is awaited, so the suite spends ONE
    // clock-owed CREATING -> ACTIVE span rather than one per row.
    let rows = rows();
    for row in &rows {
        given_stream(&router, row).await;
    }
    for row in &rows {
        active(&router, row).await;
    }
    for row in &rows {
        let records: Vec<Value> = row
            .records
            .iter()
            .map(|data| json!({ "Data": aws_smithy_types::base64::encode(data.as_slice()) }))
            .collect();
        let put = firehose(
            &router,
            "PutRecordBatch",
            json!({ "DeliveryStreamName": row.name, "Records": records }),
        )
        .await;
        let text = put.text();
        let answered = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        assert_eq!(
            (put.status, answered["FailedPutCount"].as_i64()),
            (200, Some(0)),
            "case {:?}: every record of the batch must be accepted, got {} {text}",
            row.case,
            put.status
        );
    }

    // Every row's digest, so one object's checksum cannot be another's.
    let mut digests: BTreeSet<String> = BTreeSet::new();

    for row in &rows {
        let case = row.case;
        let (key, found) = delivered(&router, row).await;

        // A read that did NOT enable checksum retrieval is told nothing about the checksum.
        let plain = s3(&router, "GET", &format!("/{BUCKET}/{key}"), &[], Vec::new()).await;
        assert_eq!(
            plain.status,
            200,
            "case {case:?}: GET {key} must succeed, got {} {}",
            plain.status,
            plain.text()
        );
        for header in CHECKSUM_HEADERS {
            let reported = plain.header(header);
            assert!(
                reported.is_none(),
                "case {case:?}: a read that did not enable the checksum mode reports no \
                 {header}, got {reported:?}"
            );
        }
        let reported_type = plain.header(TYPE_HEADER);
        assert!(
            reported_type.is_none(),
            "case {case:?}: a read that did not enable the checksum mode reports no \
             {TYPE_HEADER}, got {reported_type:?}"
        );
        let stored = plain.body.clone();

        // What Amazon S3 computes for exactly those stored bytes under CRC32.
        let oracle = s3_crc32_of(&router, case, row.name, &stored).await;
        if let Some(published) = row.published_crc32 {
            assert_eq!(
                oracle, published,
                "case {case:?}: the {CRC32} of the delivered bytes is the published value, got \
                 {oracle:?}"
            );
        }

        // And both reads that report an object's metadata report that checksum, as a
        // full-object one, and report no other algorithm's.
        for method in ["GET", "HEAD"] {
            let answer = s3(
                &router,
                method,
                &format!("/{BUCKET}/{key}"),
                &[(MODE_HEADER, ENABLED)],
                Vec::new(),
            )
            .await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: {method} {key} must succeed, got {} {}",
                answer.status,
                answer.text()
            );
            let digest = answer.header(CRC32_HEADER);
            assert_eq!(
                digest.as_deref(),
                Some(oracle.as_str()),
                "case {case:?}: {method} must report the stored object's {CRC32}, got {digest:?}"
            );
            let reported_type = answer.header(TYPE_HEADER);
            assert_eq!(
                reported_type.as_deref(),
                Some(FULL_OBJECT),
                "case {case:?}: {method} must report {TYPE_HEADER} {FULL_OBJECT}, got \
                 {reported_type:?}"
            );
            let others: Vec<&str> = CHECKSUM_HEADERS
                .into_iter()
                .filter(|header| *header != CRC32_HEADER)
                .filter(|header| answer.header(header).is_some())
                .collect();
            assert!(
                others.is_empty(),
                "case {case:?}: {method} must report only {CRC32_HEADER}, also got {others:?}"
            );
        }

        // The listing that found the object agrees with those reads about both members.
        let entry = blocks_of(&found, "Contents")
            .into_iter()
            .find(|entry| opt_element(entry, "Key").as_deref() == Some(key.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing must hold {key:?}, got {found}")
            });
        let listed_algorithm = element(entry, "ChecksumAlgorithm");
        assert_eq!(
            listed_algorithm, CRC32,
            "case {case:?}: the listing must report ChecksumAlgorithm {CRC32}, got \
             {listed_algorithm:?}"
        );
        let listed_type = element(entry, "ChecksumType");
        assert_eq!(
            listed_type, FULL_OBJECT,
            "case {case:?}: the listing must report ChecksumType {FULL_OBJECT}, got \
             {listed_type:?}"
        );

        // Reading it again reports the same value: a checksum is of the stored bytes, so it
        // does not change under the reader.
        let again = s3(
            &router,
            "GET",
            &format!("/{BUCKET}/{key}"),
            &[(MODE_HEADER, ENABLED)],
            Vec::new(),
        )
        .await;
        let repeated = again.header(CRC32_HEADER);
        assert_eq!(
            repeated.as_deref(),
            Some(oracle.as_str()),
            "case {case:?}: a second read must report the same {CRC32_HEADER}, got {repeated:?}"
        );
        digests.insert(oracle);
    }

    // The rows deliver different bytes, so each object's checksum is its own: a value copied
    // from a constant, or from another object, would collapse them.
    assert_eq!(
        digests.len(),
        rows.len(),
        "the rows' objects hold different bytes, so each reports its own digest, got {digests:?}"
    );
}
