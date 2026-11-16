// SPDX-License-Identifier: CC0-1.0

//! What a transfer's DESTINATION object is, as a later read of it reports: a WRITE of the
//! bytes the transfer read off the source, not a copy of the source's own record.
//!
//! The suite drives DataSync through the same front door a client uses, against a router
//! that mounts DataSync beside this emulator's own Amazon S3 and IAM on ONE engine
//! ([`super::s3_transfer_execution::Aws`]), so every object asserted on here is a real
//! object of the object store, staged and read back through its own API.

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

/// The checksum type a single write records, there being no parts to compose one from.
const FULL_OBJECT_CHECKSUM_TYPE: &str = "FULL_OBJECT";

/// One source object a transfer is asked to move, and the integrity record and metadata
/// its own write stored it under.
struct SourceRow {
    case: &'static str,
    key: &'static str,
    body: &'static [u8],
    /// The `ChecksumAlgorithm` the source object's own write DECLARED, or `None` for a
    /// write that declared none. The values come from the model's `ChecksumAlgorithm`
    /// enum (`smithy_query(query="shape",shape="ChecksumAlgorithm")` on the Amazon S3
    /// model lists `CRC32`, `CRC32C`, `CRC64NVME`, `MD5`, `SHA1`, `SHA256`, `SHA512`,
    /// `XXHASH128`, `XXHASH3`, `XXHASH64`).
    source_algorithm: Option<&'static str>,
    /// The user metadata the source object carries.
    metadata: &'static [(&'static str, &'static str)],
    /// The tag set it carries, as the pairs a later `GetObjectTagging` of either side
    /// reports.
    tags: &'static [(&'static str, &'static str)],
}

/// The table: objects differing in size, in the algorithm their own write was stored
/// under — none, a 32-bit CRC, a different 32-bit CRC, the store's own default, and two
/// cryptographic digests — and in the user metadata and tag set they carry, including one
/// that carries a [`LAST_MODIFIED_ENTRY`] of its own and one whose tag halves carry every
/// character the `x-amz-tagging` grammar reserves.
const SOURCE_ROWS: &[SourceRow] = &[
    SourceRow {
        case: "a source stored under no checksum algorithm at all",
        key: "plain.txt",
        body: b"the object whose write named no algorithm\n",
        source_algorithm: None,
        metadata: &[],
        tags: &[],
    },
    SourceRow {
        case: "a source stored under a 32-bit CRC",
        key: "nested/crc32.txt",
        body: b"a longer object, stored under CRC32, under a prefix\n",
        source_algorithm: Some("CRC32"),
        metadata: &[("origin", "staging")],
        tags: &[("tier", "gold")],
    },
    SourceRow {
        case: "a source stored under the other 32-bit CRC",
        key: "nested/crc32c.bin",
        body: b"short",
        source_algorithm: Some("CRC32C"),
        metadata: &[],
        tags: &[],
    },
    SourceRow {
        case: "a source stored under the store's own default algorithm",
        key: "crc64nvme.txt",
        body: b"an object already stored under the default algorithm\n",
        source_algorithm: Some(DEFAULT_CHECKSUM_ALGORITHM),
        metadata: &[("origin", "hub"), ("revision", "11")],
        tags: &[("tier", "silver"), ("owner", "platform")],
    },
    SourceRow {
        case: "a source stored under a SHA-1 digest",
        key: "digests/sha1.txt",
        body: b"an object stored under SHA1\n",
        source_algorithm: Some("SHA1"),
        metadata: &[],
        tags: &[],
    },
    SourceRow {
        case: "a source stored under a SHA-256 digest",
        key: "digests/sha256.txt",
        body: b"an object stored under SHA256, the longest body of the table by a margin\n",
        source_algorithm: Some("SHA256"),
        metadata: &[("stage", "raw")],
        // The characters a tag may carry that the header's own grammar also uses: `=`
        // separates a pair's halves, `+` reads as a space, and a space itself must survive
        // — so a tag that arrives as the tag it IS proves the halves travel URL-encoded.
        tags: &[("report group", "a=b c+d/e:f@g")],
    },
    SourceRow {
        case: "a source carrying a last-modified entry of its own",
        key: "carried/last-modified.txt",
        body: b"an object whose own last-modified entry travels with it\n",
        source_algorithm: Some("CRC32"),
        metadata: &[(LAST_MODIFIED_ENTRY, "1700000000")],
        tags: &[("tier", "copper")],
    },
];

/// RULE: a task execution materialises each destination object as a WRITE of the bytes it
/// read off the source, so a read of the destination reports that write's own record: the
/// full-object checksum the object store computes for a write that names no algorithm
/// (`CRC64NVME`), whatever algorithm the SOURCE object was stored under, and a
/// `x-amz-meta-last-modified` entry stating the source object's modification time in the
/// unit a modification time is kept in — seconds since the epoch — except where the source
/// object carries an entry of that name itself, which travels verbatim with the rest of its
/// user metadata and with its tag set.
///
/// The rows span a source stored under no algorithm, under each of the two 32-bit CRCs,
/// under the store's own default and under two cryptographic digests, with and without user
/// metadata and tags of its own, over three object sizes and three key shapes.
#[tokio::test]
async fn a_transferred_object_reports_the_write_that_materialised_it() {
    let aws = Aws::new();
    let case = "a transfer of the whole table";
    let (source_bucket, destination_bucket) = ("integrity-source", "integrity-destination");
    aws.seed_bucket(source_bucket, &[], case).await;
    aws.seed_bucket(destination_bucket, &[], case).await;
    for row in SOURCE_ROWS {
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
                "Name": "integrity-task",
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

    for row in SOURCE_ROWS {
        let case = row.case;
        let key = row.key;

        // What the SOURCE object itself reports, so each row's input is shown to have
        // taken effect: the algorithm its own write declared, and when it was modified.
        let source_read = read(&aws, source_bucket, key, case).await;
        let expected_source_algorithm =
            row.source_algorithm.unwrap_or(DEFAULT_CHECKSUM_ALGORITHM);
        assert_eq!(
            source_read.algorithms, vec![expected_source_algorithm],
            "case {case:?}: the source object {key:?} must be stored under \
             {expected_source_algorithm:?}, got {actual:?}",
            actual = source_read.algorithms
        );
        let source_modified = source_read.last_modified_seconds.unwrap_or_else(|| {
            panic!("case {case:?}: Amazon S3 must report when the source {key:?} was modified")
        });

        let destination_read = read(&aws, destination_bucket, key, case).await;
        assert_eq!(
            destination_read.body.as_slice(),
            row.body,
            "case {case:?}: wrong bytes at {key:?}, got {actual:?}",
            actual = String::from_utf8_lossy(&destination_read.body)
        );
        // The destination's integrity record is the one the object store computed for the
        // write that materialised it, and the source's algorithm does not travel.
        assert_eq!(
            destination_read.algorithms,
            vec![DEFAULT_CHECKSUM_ALGORITHM],
            "case {case:?}: the destination object {key:?} must report only the \
             {DEFAULT_CHECKSUM_ALGORITHM:?} checksum the store computes for a write that \
             names no algorithm, got {actual:?}",
            actual = destination_read.algorithms
        );
        assert_eq!(
            destination_read.checksum_type.as_deref(),
            Some(FULL_OBJECT_CHECKSUM_TYPE),
            "case {case:?}: expected {key:?} to report a whole-object checksum type, got \
             {actual:?}",
            actual = destination_read.checksum_type
        );

        // The metadata that travelled: the source's own entries verbatim, and a
        // last-modified entry stating the source's modification time in seconds.
        let mut expected: BTreeMap<String, String> = row
            .metadata
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        expected
            .entry(LAST_MODIFIED_ENTRY.to_string())
            .or_insert(source_modified.to_string());
        assert_eq!(
            destination_read.metadata, expected,
            "case {case:?}: expected {key:?} to report the user metadata {expected:?}, got \
             {actual:?}",
            actual = destination_read.metadata
        );

        // And the tag set, every half of which travels as the text it is however the
        // header that delivers it reserves a character.
        let mut expected_tags: Vec<(String, String)> = row
            .tags
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        expected_tags.sort();
        let actual_tags = tags_of(&aws, destination_bucket, key, case).await;
        assert_eq!(
            actual_tags, expected_tags,
            "case {case:?}: expected {key:?} to report the tag set {expected_tags:?}, got \
             {actual_tags:?}"
        );
    }
}

/// What a read of one object reports.
#[derive(Debug)]
struct StoredObject {
    body: Vec<u8>,
    /// The checksum algorithms the read advertises a value for, in name order.
    algorithms: Vec<&'static str>,
    checksum_type: Option<String>,
    metadata: BTreeMap<String, String>,
    /// The object's modification time in whole seconds since the epoch.
    last_modified_seconds: Option<i64>,
}

/// Read `key` of `bucket` with checksum validation asked for, so every checksum header
/// the object carries is reported.
async fn read(aws: &Aws, bucket: &str, key: &str, case: &str) -> StoredObject {
    let read = aws
        .s3()
        .get_object()
        .bucket(bucket)
        .key(key)
        .checksum_mode(::aws_sdk_s3::types::ChecksumMode::Enabled)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: Amazon S3 must serve {key:?} of {bucket:?}, got {error:?}")
        });
    let algorithms = [
        ("CRC32", read.checksum_crc32()),
        ("CRC32C", read.checksum_crc32_c()),
        ("CRC64NVME", read.checksum_crc64_nvme()),
        ("SHA1", read.checksum_sha1()),
        ("SHA256", read.checksum_sha256()),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_some_and(|value| !value.is_empty()))
    .map(|(algorithm, _)| algorithm)
    .collect();
    let checksum_type = read.checksum_type().map(|value| value.as_str().to_string());
    let metadata = read
        .metadata()
        .map(|metadata| {
            metadata
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    let last_modified_seconds = read.last_modified().map(::aws_smithy_types::DateTime::secs);
    let body = read
        .body
        .collect()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the object {key:?} must arrive in full, got {error:?}")
        })
        .into_bytes()
        .to_vec();
    StoredObject {
        body,
        algorithms,
        checksum_type,
        metadata,
        last_modified_seconds,
    }
}

/// The tag set `key` of `bucket` carries, in key order.
async fn tags_of(aws: &Aws, bucket: &str, key: &str, case: &str) -> Vec<(String, String)> {
    let tagged = aws
        .s3()
        .get_object_tagging()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: Amazon S3 must report the tag set of {key:?} of {bucket:?}, \
                 got {error:?}"
            )
        });
    let mut tags: Vec<(String, String)> = tagged
        .tag_set()
        .iter()
        .map(|tag| (tag.key().to_string(), tag.value().to_string()))
        .collect();
    tags.sort();
    tags
}

/// Put one source object, declaring the checksum algorithm, the user metadata and the tag
/// set its row names.
async fn stage(aws: &Aws, bucket: &str, row: &SourceRow) {
    let case = row.case;
    let mut write = aws
        .s3()
        .put_object()
        .bucket(bucket)
        .key(row.key)
        .content_type("text/plain")
        .body(::aws_sdk_s3::primitives::ByteStream::from(row.body.to_vec()));
    if let Some(algorithm) = row.source_algorithm {
        write = write.checksum_algorithm(::aws_sdk_s3::types::ChecksumAlgorithm::from(algorithm));
    }
    for (name, value) in row.metadata {
        write = write.metadata(*name, *value);
    }
    let stored = write.send().await;
    assert!(
        stored.is_ok(),
        "case {case:?}: Amazon S3 must store the source object {key:?}, got {stored:?}",
        key = row.key
    );
    if row.tags.is_empty() {
        return;
    }
    // The tag set is associated through the operation that takes it as a document, so a
    // half carrying a character the inline header reserves is staged as the text it is.
    let tag_set: Vec<::aws_sdk_s3::types::Tag> = row
        .tags
        .iter()
        .map(|(name, value)| {
            ::aws_sdk_s3::types::Tag::builder()
                .key(*name)
                .value(*value)
                .build()
                .expect("a tag with both halves set")
        })
        .collect();
    let tagging = ::aws_sdk_s3::types::Tagging::builder()
        .set_tag_set(Some(tag_set))
        .build()
        .expect("a tagging document with its tag set");
    let tagged = aws
        .s3()
        .put_object_tagging()
        .bucket(bucket)
        .key(row.key)
        .tagging(tagging)
        .send()
        .await;
    assert!(
        tagged.is_ok(),
        "case {case:?}: Amazon S3 must store the tag set of {key:?}, got {tagged:?}",
        key = row.key
    );
}
