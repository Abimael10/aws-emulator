// SPDX-License-Identifier: CC0-1.0

//! What a COPY reports about the S3 Bucket Key that served IT, when a directory bucket is on
//! one side of the copy.
//!
//! RULE: a copy with a directory bucket on either side uses no S3 Bucket Key at all — "S3
//! Bucket Keys aren't supported, when you copy SSE-KMS encrypted objects from general purpose
//! buckets to directory buckets, from directory buckets to general purpose buckets, or between
//! directory buckets, through CopyObject, UploadPartCopy, the Copy operation in Batch
//! Operations, or the import jobs. In this case, Amazon S3 makes a call to KMS every time a
//! copy request is made for a KMS-encrypted object"
//! (`smithy_query(query="doc",shape="CopyObjectRequest",member="BucketKeyEnabled")`) — so the
//! copy's own answer states `x-amz-server-side-encryption-bucket-key-enabled: false`, whatever
//! bucket-key setting the object it WROTE is stored under, which every later reader of that
//! object goes on reporting ("S3 Bucket Keys are always enabled for GET and PUT operations in a
//! directory bucket and can't be disabled", `PutBucketEncryption`'s own documentation).
//!
//! The two sides of that rule are each pinned over a table:
//!
//! * a copy with NO directory bucket on either side is served BY the bucket key the object
//!   carries, so it reports that setting unchanged — the control rows, which is what keeps the
//!   rule from being "a copy never reports a bucket key";
//! * the member belongs to SSE-KMS alone ("for object encryption with server-side encryption
//!   using Key Management Service (KMS) keys", `PutObjectRequest$BucketKeyEnabled`), so a copy
//!   of an object under any other encryption reports nothing for it whichever buckets it
//!   crossed.
//!
//! Every assertion drives a real HTTP request through the router (the harness the object,
//! encryption and multipart suites in this crate own), so what is pinned is the wire answer.
//! Every key a row names is made with KMS's own `CreateKey` over the same engine: nothing here
//! plants a key in a store.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_encryption::{put_encryption, Rule};
use super::bucket_lifecycle::{send_bytes, Answer};
use super::directory_bucket_sessions_and_appends::directory_bucket;
use super::kms_key_references::{created_key, router_with_kms};
use super::multipart_part_checksums::initiate;
use super::multipart_part_copy::upload_part_copy;
use super::object_and_tagging_roundtrip::{bucket, put_object, text};
use super::object_encryption::head_object;

/// The bound header the S3 Bucket Key setting travels in, on a write, on a copy and on a read
/// (`BucketKeyEnabled: {httpHeader: "x-amz-server-side-encryption-bucket-key-enabled"}`).
const BUCKET_KEY_HEADER: &str = "x-amz-server-side-encryption-bucket-key-enabled";

/// The algorithm header beside it, and the one naming the KMS key.
const ALGORITHM_HEADER: &str = "x-amz-server-side-encryption";
const KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The `ServerSideEncryption` values an S3 Bucket Key and a KMS key belong to — the two values
/// `ApplyServerSideEncryptionByDefault$KMSMasterKeyID` is "allowed if and only if" the rule's
/// algorithm is one of (`smithy_query(query="shape",shape="ServerSideEncryption")` publishes
/// both beside `AES256`).
const KMS_ALGORITHMS: [&str; 2] = ["aws:kms", "aws:kms:dsse"];

/// The zone every directory bucket in this suite is placed in.
const ZONE_ID: &str = "use1-az4";

/// The bytes every source object of this suite carries, varied per row by the row's own index so
/// no assertion rides on one recorded body.
fn payload(index: usize) -> Vec<u8> {
    format!("bucket-key copy row {index}\n").into_bytes()
}

/// WHICH side of a copy is a directory bucket — the dimension the rule generalises over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Source and destination are ONE directory bucket, under two keys.
    WithinOneDirectoryBucket,
    /// Out of a directory bucket into a general purpose one.
    DirectoryToGeneralPurpose,
    /// Into a directory bucket out of a general purpose one.
    GeneralPurposeToDirectory,
    /// Neither side is a directory bucket: the control.
    BetweenGeneralPurposeBuckets,
}

/// The two buckets a row's copy crosses, created and asserted created, as
/// `(source, destination)` — one name twice for a copy inside a single bucket.
async fn buckets_for(router: &MultiServiceRouter, shape: Shape, index: usize) -> (String, String) {
    let directory = |role: &str| format!("bkcopy-{role}-{index}--{ZONE_ID}--x-s3");
    let general_purpose = |role: &str| format!("bkcopy-{role}-{index}");
    match shape {
        Shape::WithinOneDirectoryBucket => {
            let name = directory("one");
            directory_bucket(router, &name, ZONE_ID).await;
            (name.clone(), name)
        }
        Shape::DirectoryToGeneralPurpose => {
            let source = directory("src");
            let destination = general_purpose("dst");
            directory_bucket(router, &source, ZONE_ID).await;
            bucket(router, &destination).await;
            (source, destination)
        }
        Shape::GeneralPurposeToDirectory => {
            let source = general_purpose("src");
            let destination = directory("dst");
            bucket(router, &source).await;
            directory_bucket(router, &destination, ZONE_ID).await;
            (source, destination)
        }
        Shape::BetweenGeneralPurposeBuckets => {
            let source = general_purpose("src");
            let destination = general_purpose("dst");
            bucket(router, &source).await;
            bucket(router, &destination).await;
            (source, destination)
        }
    }
}

/// The default encryption rule a row configures on both of its buckets: the row's algorithm,
/// the key where that algorithm is a KMS one, and the bucket-key setting the row names.
fn rule_of(algorithm: &str, bucket_key: Option<bool>, key_arn: &str) -> Rule {
    let mut rule = Rule::by_default(algorithm);
    if KMS_ALGORITHMS.contains(&algorithm) {
        rule = rule.with_key(key_arn);
    }
    if let Some(enabled) = bucket_key {
        rule = rule.with_bucket_key(enabled);
    }
    rule
}

/// `PUT /<bucket>/<key>` carrying `x-amz-copy-source` — `CopyObject`. The header is what
/// separates this route from `PutObject` on the same method and path, so it is always sent.
async fn copy_object(
    router: &MultiServiceRouter,
    destination_bucket: &str,
    destination_key: &str,
    copy_source: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("x-amz-copy-source", copy_source)];
    all.extend_from_slice(headers);
    send_bytes(
        router,
        "PUT",
        &format!("/{destination_bucket}/{destination_key}"),
        &all,
        b"",
    )
    .await
}

/// One row of the copy table.
struct CopyRow {
    case: &'static str,
    shape: Shape,
    /// The `SSEAlgorithm` both buckets' default encryption names.
    algorithm: &'static str,
    /// The `BucketKeyEnabled` that rule names, `None` for a rule naming none.
    bucket_key_in_rule: Option<bool>,
    /// Whether the COPY REQUEST names the encryption itself — the same algorithm and key,
    /// with the Bucket Key switched on — rather than inheriting the destination's default.
    request_names_encryption: bool,
    /// What `x-amz-server-side-encryption-bucket-key-enabled` must be on the COPY's own answer.
    expected_on_the_copy: Option<&'static str>,
    /// ...and on a later read of the object that copy wrote.
    expected_on_the_object: Option<&'static str>,
}

#[tokio::test]
async fn a_copy_across_a_directory_bucket_reports_no_bucket_key_for_itself() {
    // The key both sides' defaults name is one KMS really holds: a write stored under a KMS key
    // resolves its reference against KMS before anything is stored, and every source object and
    // every copy below is such a write ([`super::kms_key_references`]).
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these buckets encrypt under").await;

    // The rows differ along what the rule generalises over: WHICH side of the copy is a
    // directory bucket (each of the three forms the sentence lists, plus the control with none),
    // WHICH algorithm the defaults name (single-layer SSE-KMS, dual-layer SSE-KMS, and SSE-S3,
    // which has no bucket key at all), whether the bucket-key setting is on, off or unnamed in
    // the rule, and whether the copy REQUEST named the encryption itself.
    let rows: [CopyRow; 8] = [
        CopyRow {
            case: "within one directory bucket whose default names a CMK and the Bucket Key",
            shape: Shape::WithinOneDirectoryBucket,
            algorithm: "aws:kms",
            bucket_key_in_rule: Some(true),
            request_names_encryption: false,
            expected_on_the_copy: Some("false"),
            expected_on_the_object: Some("true"),
        },
        CopyRow {
            case: "out of a directory bucket into a general purpose one, the Bucket Key on",
            shape: Shape::DirectoryToGeneralPurpose,
            algorithm: "aws:kms",
            bucket_key_in_rule: Some(true),
            request_names_encryption: false,
            expected_on_the_copy: Some("false"),
            expected_on_the_object: Some("true"),
        },
        CopyRow {
            case: "into a directory bucket out of a general purpose one, dual-layer SSE-KMS",
            shape: Shape::GeneralPurposeToDirectory,
            algorithm: "aws:kms:dsse",
            bucket_key_in_rule: Some(true),
            request_names_encryption: false,
            expected_on_the_copy: Some("false"),
            expected_on_the_object: Some("true"),
        },
        CopyRow {
            case: "out of a directory bucket into a default that switches the Bucket Key off",
            shape: Shape::DirectoryToGeneralPurpose,
            algorithm: "aws:kms",
            bucket_key_in_rule: Some(false),
            request_names_encryption: false,
            // The copy states that no Bucket Key served it; the object it wrote carries none,
            // so a read of that object reports the member not at all.
            expected_on_the_copy: Some("false"),
            expected_on_the_object: None,
        },
        CopyRow {
            case: "within one directory bucket, the copy request asking for the Bucket Key",
            shape: Shape::WithinOneDirectoryBucket,
            algorithm: "aws:kms",
            bucket_key_in_rule: None,
            request_names_encryption: true,
            expected_on_the_copy: Some("false"),
            expected_on_the_object: Some("true"),
        },
        CopyRow {
            case: "into a directory bucket whose default is SSE-S3, which has no Bucket Key",
            shape: Shape::GeneralPurposeToDirectory,
            algorithm: "AES256",
            bucket_key_in_rule: None,
            request_names_encryption: false,
            expected_on_the_copy: None,
            expected_on_the_object: None,
        },
        CopyRow {
            case: "between general purpose buckets with the Bucket Key on",
            shape: Shape::BetweenGeneralPurposeBuckets,
            algorithm: "aws:kms",
            bucket_key_in_rule: Some(true),
            request_names_encryption: false,
            // No directory bucket is involved, so the Bucket Key the object carries is the one
            // that served the copy, and the copy reports it.
            expected_on_the_copy: Some("true"),
            expected_on_the_object: Some("true"),
        },
        CopyRow {
            case: "between general purpose buckets with the Bucket Key off",
            shape: Shape::BetweenGeneralPurposeBuckets,
            algorithm: "aws:kms",
            bucket_key_in_rule: Some(false),
            request_names_encryption: false,
            expected_on_the_copy: None,
            expected_on_the_object: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (source_bucket, destination_bucket) = buckets_for(&router, row.shape, index).await;
        let rule = rule_of(row.algorithm, row.bucket_key_in_rule, &key_arn);
        let mut configured: Vec<&str> = vec![source_bucket.as_str()];
        if destination_bucket != source_bucket {
            configured.push(destination_bucket.as_str());
        }
        for name in configured {
            let answer = put_encryption(&router, name, ::std::slice::from_ref(&rule)).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: the bucket {name:?} must take its default encryption first, \
                 got {} with body {:?}",
                answer.status,
                text(&answer)
            );
        }

        let source_key = format!("sources/{index}.csv");
        let destination_key = format!("copies/{index}.csv");
        let bytes = payload(index);
        let written = put_object(&router, &source_bucket, &source_key, &bytes, &[]).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the source object must be written first, got {} with body {:?}",
            written.status,
            text(&written)
        );

        let request_headers: Vec<(&str, &str)> = if row.request_names_encryption {
            vec![
                (ALGORITHM_HEADER, row.algorithm),
                (KEY_ID_HEADER, key_arn.as_str()),
                (BUCKET_KEY_HEADER, "true"),
            ]
        } else {
            Vec::new()
        };
        let copied = copy_object(
            &router,
            &destination_bucket,
            &destination_key,
            &format!("{source_bucket}/{source_key}"),
            &request_headers,
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {case:?}: the copy must answer 200, got {} with body {:?}",
            copied.status,
            text(&copied)
        );

        // The copy IS an SSE-KMS copy where the row says so — the algorithm and the key the
        // answer reports are the destination's — which is what makes the bucket-key assertion
        // below a statement about the Bucket Key rather than about a copy that used no KMS key
        // at all.
        assert_eq!(
            copied.header(ALGORITHM_HEADER).as_deref(),
            Some(row.algorithm),
            "case {case:?}: the copy must report {:?} as its encryption, got {:?}",
            row.algorithm,
            copied.header(ALGORITHM_HEADER)
        );
        let expected_key = KMS_ALGORITHMS
            .contains(&row.algorithm)
            .then(|| key_arn.clone());
        assert_eq!(
            copied.header(KEY_ID_HEADER),
            expected_key,
            "case {case:?}: the copy must report {expected_key:?} as its key, got {:?}",
            copied.header(KEY_ID_HEADER)
        );
        assert_eq!(
            copied.header(BUCKET_KEY_HEADER).as_deref(),
            row.expected_on_the_copy,
            "case {case:?}: the copy must report {} as {:?}, got {:?}",
            BUCKET_KEY_HEADER,
            row.expected_on_the_copy,
            copied.header(BUCKET_KEY_HEADER)
        );

        // ...and the object the copy WROTE is read back under the setting it was stored with,
        // which a copy across a directory bucket does not change.
        let headed = head_object(&router, &destination_bucket, &destination_key).await;
        assert_eq!(
            headed.status, 200,
            "case {case:?}: the copy must read back, got {} with body {:?}",
            headed.status,
            text(&headed)
        );
        assert_eq!(
            headed.header(BUCKET_KEY_HEADER).as_deref(),
            row.expected_on_the_object,
            "case {case:?}: a read of the copy must report {} as {:?}, got {:?}",
            BUCKET_KEY_HEADER,
            row.expected_on_the_object,
            headed.header(BUCKET_KEY_HEADER)
        );
        assert_eq!(
            headed.header(ALGORITHM_HEADER).as_deref(),
            Some(row.algorithm),
            "case {case:?}: a read of the copy must report {:?} as its encryption, got {:?}",
            row.algorithm,
            headed.header(ALGORITHM_HEADER)
        );
    }
}

/// One row of the part-copy table: which side of the part copy is a directory bucket, and what
/// the copy's own answer must then report for the Bucket Key.
struct PartRow {
    case: &'static str,
    shape: Shape,
    expected_on_the_copy: Option<&'static str>,
}

#[tokio::test]
async fn a_part_copy_across_a_directory_bucket_reports_no_bucket_key_for_itself() {
    let router = router_with_kms();
    let (_, key_arn) = created_key(&router, "the key these uploads encrypt under").await;

    // The same three forms the sentence lists, plus the control with no directory bucket on
    // either side. Every bucket's default names the same CMK with the Bucket Key ON, so each row
    // differs only in WHICH side of the part copy is a directory bucket.
    let rows: [PartRow; 4] = [
        PartRow {
            case: "a part copied inside one directory bucket",
            shape: Shape::WithinOneDirectoryBucket,
            expected_on_the_copy: Some("false"),
        },
        PartRow {
            case: "a part copied out of a directory bucket into a general purpose upload",
            shape: Shape::DirectoryToGeneralPurpose,
            expected_on_the_copy: Some("false"),
        },
        PartRow {
            case: "a part copied into an upload of a directory bucket",
            shape: Shape::GeneralPurposeToDirectory,
            expected_on_the_copy: Some("false"),
        },
        PartRow {
            case: "a part copied between general purpose buckets",
            shape: Shape::BetweenGeneralPurposeBuckets,
            // No directory bucket is involved, so the upload's own Bucket Key served the copy.
            expected_on_the_copy: Some("true"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // The indexes continue past the copy table's so no two rows name one bucket.
        let (source_bucket, destination_bucket) =
            buckets_for(&router, row.shape, 100 + index).await;
        let rule = rule_of("aws:kms", Some(true), &key_arn);
        let mut configured: Vec<&str> = vec![source_bucket.as_str()];
        if destination_bucket != source_bucket {
            configured.push(destination_bucket.as_str());
        }
        for name in configured {
            let answer = put_encryption(&router, name, ::std::slice::from_ref(&rule)).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: the bucket {name:?} must take its default encryption first, \
                 got {} with body {:?}",
                answer.status,
                text(&answer)
            );
        }

        let source_key = format!("sources/part-{index}.bin");
        let bytes = payload(index);
        let written = put_object(&router, &source_bucket, &source_key, &bytes, &[]).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: the source object must be written first, got {} with body {:?}",
            written.status,
            text(&written)
        );

        // The upload the part is recorded into names no encryption of its own, so it is
        // initiated under the destination bucket's default — the Bucket Key included.
        let key = format!("uploads/part-{index}.bin");
        let upload_id = initiate(&router, &destination_bucket, &key, &[]).await;
        let copied = upload_part_copy(
            &router,
            &destination_bucket,
            &key,
            &upload_id,
            1,
            &format!("{source_bucket}/{source_key}"),
            &[],
        )
        .await;
        assert_eq!(
            copied.status, 200,
            "case {case:?}: the part copy must answer 200, got {} with body {:?}",
            copied.status,
            text(&copied)
        );
        assert_eq!(
            copied.header(ALGORITHM_HEADER).as_deref(),
            Some("aws:kms"),
            "case {case:?}: the part copy must report the upload's encryption, got {:?}",
            copied.header(ALGORITHM_HEADER)
        );
        assert_eq!(
            copied.header(BUCKET_KEY_HEADER).as_deref(),
            row.expected_on_the_copy,
            "case {case:?}: the part copy must report {} as {:?}, got {:?}",
            BUCKET_KEY_HEADER,
            row.expected_on_the_copy,
            copied.header(BUCKET_KEY_HEADER)
        );

        // A part UPLOADED into the same upload is no copy, so the Bucket Key of the upload
        // itself is what serves it and what it reports — whichever buckets the copy above
        // crossed.
        let uploaded = send_bytes(
            &router,
            "PUT",
            &format!("/{destination_bucket}/{key}?partNumber=2&uploadId={upload_id}&x-id=UploadPart"),
            &[],
            &bytes,
        )
        .await;
        assert_eq!(
            uploaded.status, 200,
            "case {case:?}: the uploaded part must answer 200, got {} with body {:?}",
            uploaded.status,
            text(&uploaded)
        );
        assert_eq!(
            uploaded.header(BUCKET_KEY_HEADER).as_deref(),
            Some("true"),
            "case {case:?}: an uploaded part reports the upload's own Bucket Key, got {:?}",
            uploaded.header(BUCKET_KEY_HEADER)
        );
    }
}
