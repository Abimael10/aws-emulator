// SPDX-License-Identifier: CC0-1.0

//! What a READ of a transferred destination object reports about the object itself: the
//! media type it is stored under, the user metadata stored with it, and the whole-object
//! checksum computed over its bytes.
//!
//! A transfer between two Amazon S3 locations moves "the data (including metadata)"
//! (`Options$TransferMode`), so each destination object is a stored object carrying the
//! source object's content type and `x-amz-meta-*` entries, plus the integrity record the
//! object store computes for the write that materialised it — and a later read binds each
//! of those to its own response member (`ContentType`, `Metadata`, `ChecksumCRC64NVME`).
//! Every value asserted here is read back out of the object store through its own API, so
//! a member served from anything other than the stored object cannot pass.

use std::collections::BTreeMap;

use serde_json::json;

use super::s3_transfer_execution::Aws;

/// The user-metadata entry a transfer into an object store carries whatever the task's
/// `ObjectMetadata` says: "even if you specify NONE, metadata for the last modified time
/// is still preserved" (`Options$ObjectMetadata`).
const LAST_MODIFIED_ENTRY: &str = "last-modified";

/// The checksum algorithm an object store applies to a write that names none, and so the
/// one every destination object a transfer writes carries: "if it was uploaded without a
/// checksum (and Amazon S3 added the default checksum, `CRC64NVME`, to the uploaded
/// object)" (`PutObjectOutput$ChecksumCRC64NVME`).
const DEFAULT_CHECKSUM_ALGORITHM: &str = "CRC64NVME";

/// The checksum type a single write records, there being no parts to compose one from:
/// "for `PutObject` uploads, the checksum type is always `FULL_OBJECT`"
/// (`PutObjectRequest$ChecksumType`).
const FULL_OBJECT_CHECKSUM_TYPE: &str = "FULL_OBJECT";

/// The input the CRC-64/NVME specification publishes a check value for.
const CHECK_INPUT: &[u8] = b"123456789";

/// That published check value: the CRC-64/NVME digest of [`CHECK_INPUT`] is
/// `0xAE8B14860A799888`, which anchors the reported checksum to the algorithm the model
/// names rather than to whatever this emulator happens to compute.
const CHECK_VALUE: u64 = 0xAE8B_1486_0A79_9888;

/// One object staged on the source side, and the record a read of the destination copy
/// has to report for it.
struct ObjectRow {
    case: &'static str,
    key: &'static str,
    body: &'static [u8],
    /// The media type the source object's own write declared.
    content_type: &'static str,
    /// The `x-amz-meta-*` entries it carries.
    metadata: &'static [(&'static str, &'static str)],
}

/// The table: objects differing in media type (a text type, a structured type, a type
/// carrying a parameter and the generic binary type), in how many user-metadata entries
/// they carry (none, one, two, and one that is the preserved-time entry itself), in size
/// (empty, a few bytes, tens of bytes) and in key shape (bare, one prefix, two prefixes).
const OBJECT_ROWS: &[ObjectRow] = &[
    ObjectRow {
        case: "a text object carrying no user metadata of its own",
        key: "notes.txt",
        body: b"a line of text, and then another one\n",
        content_type: "text/plain",
        metadata: &[],
    },
    ObjectRow {
        case: "a structured object under two prefixes carrying two entries",
        key: "exports/2026/report.json",
        body: b"{\"rows\":2,\"complete\":true}",
        content_type: "application/json",
        metadata: &[("origin", "staging"), ("revision", "7")],
    },
    ObjectRow {
        case: "an object whose media type carries a parameter",
        key: "exports/rows.csv",
        body: b"left,right\n1,2\n",
        content_type: "text/csv; charset=utf-8",
        metadata: &[("stage", "raw")],
    },
    ObjectRow {
        case: "an object carrying a preserved-time entry of its own",
        key: "carried/last-modified.txt",
        body: b"the object whose own preserved time travels with it\n",
        content_type: "text/plain",
        metadata: &[(LAST_MODIFIED_ENTRY, "1700000000")],
    },
    ObjectRow {
        case: "a zero-length object stored under the generic binary type",
        key: "empty.dat",
        body: b"",
        content_type: "application/octet-stream",
        metadata: &[("origin", "empty")],
    },
    ObjectRow {
        case: "the object whose bytes have a published checksum",
        key: "check/value.txt",
        body: CHECK_INPUT,
        content_type: "text/plain",
        metadata: &[("origin", "check")],
    },
];

/// RULE: a read of a stored object reports the record the write that stored it left —
/// the media type that write declared on `ContentType`, every user-metadata entry it
/// stored on `Metadata`, and the whole-object checksum of the stored bytes on the member
/// for the algorithm the object carries — and a transfer between two Amazon S3 locations
/// stores each destination object with the source object's media type and user metadata,
/// so a read of a transferred object reports those and a checksum over the transferred
/// bytes rather than a server-chosen fallback or nothing at all.
///
/// The rows span four media types, four user-metadata shapes, three sizes including an
/// empty object and three key shapes; one row's bytes are the input the checksum
/// algorithm publishes a check value for, so the reported value is pinned to the
/// algorithm itself and not only to this emulator's own computation.
#[tokio::test]
async fn a_read_of_a_transferred_object_reports_its_stored_metadata_and_checksum() {
    let aws = Aws::new();
    let case = "a transfer of the whole table";
    let (source_bucket, destination_bucket) = ("headers-source", "headers-destination");
    aws.seed_bucket(source_bucket, &[], case).await;
    aws.seed_bucket(destination_bucket, &[], case).await;
    for row in OBJECT_ROWS {
        stage(&aws, source_bucket, row).await;
    }

    let role_arn = aws.create_role(&[source_bucket, destination_bucket]).await;
    let source = aws
        .create_location(source_bucket, None, &role_arn, case)
        .await;
    let destination = aws
        .create_location(destination_bucket, None, &role_arn, case)
        .await;
    let created = aws
        .ok(
            "CreateTask",
            json!({
                "SourceLocationArn": source,
                "DestinationLocationArn": destination,
                "Name": "destination-headers-task",
            }),
            case,
        )
        .await;
    let task_arn = created["TaskArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a TaskArn, got {created}"))
        .to_string();
    let execution_arn = aws.start_execution(&task_arn, case).await;
    let finished = aws.await_terminal(&execution_arn, case).await;
    assert_eq!(
        finished["Status"].as_str(),
        Some("SUCCESS"),
        "case {case:?}: expected the execution to end SUCCESS, got {finished}"
    );

    for row in OBJECT_ROWS {
        let case = row.case;
        let key = row.key;
        // The source object's own modification time: what the destination's preserved-time
        // entry has to state where the source carries no entry of that name itself.
        let source_modified = aws.object_modified_at(source_bucket, key, case).await;

        let read = aws
            .s3()
            .get_object()
            .bucket(destination_bucket)
            .key(key)
            // Integrity asked for, so the checksum the object carries is reported.
            .checksum_mode(::aws_sdk_s3::types::ChecksumMode::Enabled)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the destination must serve {key:?} after the transfer, \
                     got {error:?}"
                )
            });
        let content_type = read.content_type().map(ToString::to_string);
        let checksum = read.checksum_crc64_nvme().map(ToString::to_string);
        let checksum_type = read.checksum_type().map(|value| value.as_str().to_string());
        let metadata: BTreeMap<String, String> = read
            .metadata()
            .map(|metadata| {
                metadata
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let body = read
            .body
            .collect()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the object {key:?} must arrive in full, got {error:?}")
            })
            .into_bytes()
            .to_vec();

        assert_eq!(
            body.as_slice(),
            row.body,
            "case {case:?}: wrong bytes at {key:?}, got {actual:?}",
            actual = String::from_utf8_lossy(&body)
        );
        assert_eq!(
            content_type.as_deref(),
            Some(row.content_type),
            "case {case:?}: expected {key:?} to report the media type its source was \
             stored under, got {content_type:?}"
        );

        // Every entry the source carries, and a preserved-time entry either way: the
        // source's own where it has one, else the time the source side reports for it.
        let mut expected: BTreeMap<String, String> = row
            .metadata
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        expected
            .entry(LAST_MODIFIED_ENTRY.to_string())
            .or_insert(source_modified.clone());
        assert_eq!(
            metadata, expected,
            "case {case:?}: expected {key:?} to report the user metadata {expected:?}, got \
             {actual:?}",
            actual = metadata
        );

        // The checksum is the digest of the bytes the destination holds, under the
        // algorithm an object store applies to a write that names none.
        let expected_checksum = ::service_com_amazonaws_s3_amazons3::state::full_object_checksum(
            DEFAULT_CHECKSUM_ALGORITHM,
            &body,
        );
        assert!(
            expected_checksum.is_some(),
            "case {case:?}: {DEFAULT_CHECKSUM_ALGORITHM:?} must be a computable algorithm"
        );
        assert_eq!(
            checksum, expected_checksum,
            "case {case:?}: expected {key:?} to report the \
             {DEFAULT_CHECKSUM_ALGORITHM:?} checksum {expected_checksum:?} of its bytes, got \
             {checksum:?}"
        );
        assert_eq!(
            checksum_type.as_deref(),
            Some(FULL_OBJECT_CHECKSUM_TYPE),
            "case {case:?}: expected {key:?} to report a whole-object checksum type, got \
             {checksum_type:?}"
        );
        if row.body == CHECK_INPUT {
            let published = ::aws_smithy_types::base64::encode(CHECK_VALUE.to_be_bytes());
            assert_eq!(
                checksum.as_deref(),
                Some(published.as_str()),
                "case {case:?}: expected {key:?} to report the published check value \
                 {published:?}, got {checksum:?}"
            );
        }
    }
}

/// Put one source object, declaring the media type and the user metadata its row names.
async fn stage(aws: &Aws, bucket: &str, row: &ObjectRow) {
    let case = row.case;
    let mut write = aws
        .s3()
        .put_object()
        .bucket(bucket)
        .key(row.key)
        .content_type(row.content_type)
        .body(::aws_sdk_s3::primitives::ByteStream::from(row.body.to_vec()));
    for (name, value) in row.metadata {
        write = write.metadata(*name, *value);
    }
    let stored = write.send().await;
    assert!(
        stored.is_ok(),
        "case {case:?}: Amazon S3 must store the source object {key:?}, got {stored:?}",
        key = row.key
    );
}
