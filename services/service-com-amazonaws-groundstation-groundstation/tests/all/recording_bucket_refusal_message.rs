// SPDX-License-Identifier: CC0-1.0

//! WHICH answer Ground Station gives when an `s3RecordingConfig`'s bucket cannot be
//! resolved.
//!
//! The RULE under test: Ground Station reads the recording bucket's location in its OWN
//! name, so every condition that leaves it without that location — S3 reporting no such
//! bucket, S3 refusing the read, and a `bucketArn` that names no bucket at all, which is
//! the same thing settled without asking — is refused with ONE answer: the modelled
//! `InvalidParameterException` (HTTP 431), `parameterName`
//! `configData.s3RecordingConfig.bucketArn`, and the single sentence that says the
//! location could not be obtained and names the permission such a read needs. The caller
//! is never told which of the conditions it was. The sentence is FIXED text: it
//! interpolates no bucket name, ARN, account or region, so every row — differing along the
//! form of the ARN, the kind of ARN, the bucket name's length and characters, whether a
//! trailing key is carried, the partition, and whether S3 holds the bucket at all — gets a
//! byte-identical sentence.
//!
//! The sentence is written out HERE as well as exported from the crate, so a drift in the
//! crate's constant fails this suite rather than passing it unnoticed, and the rows run
//! through both writers (`CreateConfig` and `UpdateConfig`) of the one resolution path.

use serde_json::json;
use service_com_amazonaws_groundstation_groundstation::state::RECORDING_BUCKET_UNRESOLVABLE;

use crate::s3_recording_buckets::{
    create, create_bucket, harness_with_object_storage, put_bucket_policy, recording_config,
};
use crate::scheduling_loop::send;

/// The refusal text, as the service words it for a recording bucket it cannot locate.
/// Spelled out in full rather than composed, because the whole point of the rule is that
/// it is FIXED text.
const UNRESOLVABLE_BUCKET_MESSAGE: &str = "Unable to create new S3 endpoint config - \
     unable to get location of bucket. Ensure that Ground Station has s3:GetBucketLocation \
     permissions on the bucket.";

/// The member of the request the refusal blames.
const BLAMED_PARAMETER: &str = "configData.s3RecordingConfig.bucketArn";

/// The one bucket the account in these tests holds and whose read is allowed; every other
/// name below either resolves to nothing or has its read denied.
const HELD_BUCKET: &str = "aws-groundstation-downlink";

/// The bucket whose read S3 refuses: a bucket the account HOLDS, so S3 answers about it
/// without handing over where it is.
const DENIED_BUCKET: &str = "aws-groundstation-denied";

/// Recording buckets Ground Station cannot locate, differing along what the rule
/// generalises over.
///
/// Buckets that are NOT THERE: an S3 bucket ARN whose bucket S3 does not hold (by itself,
/// as a near miss of the held name, in another case, with a trailing key, in another
/// partition, with a three-character and with a 63-character name, with dots), an ARN of
/// another service, an S3 resource that is not a bucket, an ARN with no bucket in its
/// resource part, an ARN naming no service, and a string that is no ARN at all. And
/// buckets S3 ANSWERS about while withholding the location: the held-but-denied bucket
/// alone, a key under it, and the same bucket named in another partition — all of which
/// name that one bucket (`state::bucket_name_of`).
fn unresolvable_bucket_rows() -> Vec<(&'static str, String)> {
    vec![
        (
            "a bucket name nothing holds",
            "arn:aws:s3:::absent-recording-bucket".to_string(),
        ),
        (
            "a near miss of the held bucket's name",
            format!("arn:aws:s3:::{HELD_BUCKET}s"),
        ),
        (
            "the held bucket's name in another case",
            format!("arn:aws:s3:::{}", HELD_BUCKET.to_uppercase()),
        ),
        (
            "a key under a bucket nothing holds",
            "arn:aws:s3:::absent-recording-bucket/downlink/2026/01/01".to_string(),
        ),
        (
            "an absent bucket in another partition",
            "arn:aws-us-gov:s3:::absent-recording-bucket".to_string(),
        ),
        (
            "a three-character absent bucket name",
            "arn:aws:s3:::abc".to_string(),
        ),
        (
            // 63 characters, the longest name S3 takes for a new bucket.
            "a 63-character absent bucket name",
            format!("arn:aws:s3:::{}", "a".repeat(63)),
        ),
        (
            "a dotted absent bucket name",
            "arn:aws:s3:::my.absent.recording-bucket.01".to_string(),
        ),
        (
            "an IAM role ARN, which names no bucket",
            "arn:aws:iam::000000000000:role/GroundStationS3Role".to_string(),
        ),
        (
            "an S3 access point ARN, which is not a bucket ARN",
            "arn:aws:s3:us-east-1:000000000000:accesspoint/gs".to_string(),
        ),
        (
            "an S3 ARN with an empty resource part",
            "arn:aws:s3:::".to_string(),
        ),
        (
            "an ARN of no service at all",
            "arn:aws:::::absent-recording-bucket".to_string(),
        ),
        (
            "a string that is not an ARN",
            "absent-recording-bucket".to_string(),
        ),
        (
            "a bucket whose read S3 refuses",
            format!("arn:aws:s3:::{DENIED_BUCKET}"),
        ),
        (
            "a key under a bucket whose read S3 refuses",
            format!("arn:aws:s3:::{DENIED_BUCKET}/downlink/2026/01/01"),
        ),
        (
            "a denied bucket named in another partition",
            format!("arn:aws-us-gov:s3:::{DENIED_BUCKET}"),
        ),
    ]
}

/// A bucket policy denying the read of `bucket` to every principal — the state that makes
/// S3 refuse the question Ground Station asks of the bucket. An explicit denial refuses
/// whatever any grant says, so this holds for the caller Ground Station asks as.
fn deny_bucket_read(bucket: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Deny\",\
         \"Principal\":\"*\",\"Action\":\"s3:ListBucket\",\
         \"Resource\":\"arn:aws:s3:::{bucket}\"}}]}}"
    )
}

/// A router with Ground Station and S3 over one engine, holding the bucket this account
/// may read and the bucket whose read is denied.
async fn harness_with_held_and_denied_buckets() -> smithy_server_runtime::MultiServiceRouter {
    let router = harness_with_object_storage();
    assert_eq!(
        create_bucket(&router, HELD_BUCKET).await,
        200,
        "case setup: the one bucket this account holds must be created"
    );
    assert_eq!(
        create_bucket(&router, DENIED_BUCKET).await,
        200,
        "case setup: the bucket whose read is denied must be created"
    );
    let attached = put_bucket_policy(&router, DENIED_BUCKET, &deny_bucket_read(DENIED_BUCKET)).await;
    assert!(
        (200..300).contains(&attached),
        "case setup: PutBucketPolicy must attach the denial, got {attached}"
    );
    router
}

/// The crate's exported constant and the sentence this suite pins are the same text: the
/// other suites assert a refusal through that constant, so pinning it once here is what
/// ties them to AWS's wording.
#[test]
fn the_exported_refusal_text_is_the_sentence_aws_answers() {
    assert_eq!(
        RECORDING_BUCKET_UNRESOLVABLE, UNRESOLVABLE_BUCKET_MESSAGE,
        "case exported-constant: expected the unobtainable-location sentence, got \
         {RECORDING_BUCKET_UNRESOLVABLE:?}"
    );
    assert!(
        RECORDING_BUCKET_UNRESOLVABLE.contains("s3:GetBucketLocation"),
        "case exported-constant: the refusal must name the permission the read needs, got \
         {RECORDING_BUCKET_UNRESOLVABLE:?}"
    );
}

#[tokio::test]
async fn every_recording_bucket_that_cannot_be_located_is_refused_with_the_one_sentence() {
    let router = harness_with_held_and_denied_buckets().await;

    for (case, bucket_arn) in unresolvable_bucket_rows() {
        let reply = create(&router, "recording-refused", &recording_config(&bucket_arn)).await;
        assert_eq!(
            reply.status, 431,
            "case {case:?}: expected status 431, got {} {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {}",
            reply.body
        );
        assert_eq!(
            reply.string("message"),
            UNRESOLVABLE_BUCKET_MESSAGE,
            "case {case:?}: expected the unobtainable-location sentence, got {}",
            reply.body
        );
        assert_eq!(
            reply.string("parameterName"),
            BLAMED_PARAMETER,
            "case {case:?}: expected the bucketArn member to be blamed, got {}",
            reply.body
        );
        // Fixed text: nothing of the request leaks into it, so neither the ARN nor the
        // bucket name it carries can appear in what the caller is told.
        assert!(
            !reply.string("message").contains(bucket_arn.as_str()),
            "case {case:?}: the refusal text must interpolate nothing from the request, got {}",
            reply.body
        );
    }

    // And nothing was stored for any of them: the listing is the store.
    let listed = send(&router, "GET", "/config", None).await;
    assert_eq!(
        listed.field("configList"),
        &json!([]),
        "case refused-creates-store-nothing: expected an empty configList, got {}",
        listed.body
    );

    // The bucket the account DOES hold and may read is accepted, so the rows above are
    // refused for the location Ground Station could not obtain and not because every
    // recording config is.
    let held = create(
        &router,
        "recording-held",
        &recording_config(&format!("arn:aws:s3:::{HELD_BUCKET}")),
    )
    .await;
    assert_eq!(
        held.status, 200,
        "case a-bucket-that-resolves: expected status 200, got {} {}",
        held.status, held.body
    );
}

#[tokio::test]
async fn an_update_answers_the_same_sentence_as_a_create() {
    let router = harness_with_held_and_denied_buckets().await;

    // A bucket that DOES resolve is accepted — so the sentence below is the answer to a
    // bucket that cannot be located, not a blanket refusal of every recording config.
    let held_data = recording_config(&format!("arn:aws:s3:::{HELD_BUCKET}"));
    let created = create(&router, "recording-held", &held_data).await;
    assert_eq!(
        created.status, 200,
        "case a-bucket-that-resolves: expected status 200, got {} {}",
        created.status, created.body
    );
    let path = format!("/config/s3-recording/{}", created.string("configId"));

    for (case, bucket_arn) in unresolvable_bucket_rows() {
        let refused = send(
            &router,
            "PUT",
            &path,
            Some(json!({
                "name": "recording-held",
                "configData": recording_config(&bucket_arn),
            })),
        )
        .await;
        assert_eq!(
            refused.status, 431,
            "case update to {case:?}: expected status 431, got {} {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "InvalidParameterException",
            "case update to {case:?}: expected InvalidParameterException, got {}",
            refused.body
        );
        assert_eq!(
            refused.string("message"),
            UNRESOLVABLE_BUCKET_MESSAGE,
            "case update to {case:?}: expected {UNRESOLVABLE_BUCKET_MESSAGE:?}, got {}",
            refused.body
        );
        assert_eq!(
            refused.string("parameterName"),
            BLAMED_PARAMETER,
            "case update to {case:?}: expected the bucketArn member to be blamed, got {}",
            refused.body
        );
    }

    // Every one of those updates was refused, so the stored config still records to the
    // bucket it was created for.
    let read_back = send(&router, "GET", &path, None).await;
    assert_eq!(
        read_back.field("configData"),
        &held_data,
        "case refused-updates-change-nothing: expected the created configData, got {}",
        read_back.field("configData")
    );
}
