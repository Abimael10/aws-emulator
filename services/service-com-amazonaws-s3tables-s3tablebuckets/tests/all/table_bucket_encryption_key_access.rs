// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the KMS KEY a `PutTableBucketEncryption` names.
//!
//! RULE: a configuration naming `aws:kms` names the key S3 Tables would encrypt
//! the bucket's tables under — the model requires the service be granted access
//! to it ("if you choose SSE-KMS encryption you must grant the S3 Tables
//! maintenance principal access to your KMS key",
//! `smithy_query(query="doc", shape="PutTableBucketEncryption")`) — so the put
//! is served only when KMS resolves that key ARN for the Region the request was
//! signed for, as `kms:DescribeKey` resolves one. A key ARN KMS answers the
//! caller nothing about is refused with the modelled `ForbiddenException`
//! (HTTP 403) carrying KMS's own sentence for an authorization failure it can
//! report nothing about the resource with, whose only variable part is the
//! principal the request speaks for — what `sts:GetCallerIdentity` reports for
//! the caller — and the refusal writes nothing: the bucket keeps the
//! configuration it already carried.
//!
//! The rows span what the rule generalises over: a key of this account in this
//! Region (served), a second such key (served, so the stored value is not a
//! constant), a key ARN naming another account, a key of this account in ANOTHER
//! Region, and a well-formed key ARN of this account and Region that names no
//! key at all — plus the same key ARN served by the Region that does hold it,
//! since a KMS key is a regional resource, and a configuration naming no key at
//! all, which the gate asks nothing about.
//!
//! Every key here is KMS's OWN, created through KMS's own `CreateKey` over the
//! same engine, so what this service resolves is what that service wrote.

use serde_json::{json, Map, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::ComAmazonawsS3tablesS3tablebucketsState as S3TablesState;

use super::harness::Emulator;
use super::table_bucket_encryption::reported;

/// The Region the buckets of this suite live in.
const REGION: &str = "us-east-1";

/// Another Region, for the rows about a key the request's Region does not serve.
const OTHER_REGION: &str = "eu-west-1";

/// The modelled error code of the refusal under test.
const FORBIDDEN: &str = "ForbiddenException";

/// An account id that is not this emulator's, in the form the modelled
/// `kmsKeyArn` pattern requires (`[0-9]{12}`).
const FOREIGN_ACCOUNT: &str = "111122223333";

/// The sentence KMS refuses an authorization failure it can report nothing about
/// the resource with, spelled out here rather than read from a crate so this
/// suite pins the wording itself. `{principal}` is the only variable part.
fn refusal_sentence(principal: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: kms:DescribeKey on this resource \
         because the resource does not exist in this Region, no resource-based policies allow \
         access, or a resource-based policy explicitly denies access"
    )
}

/// An `encryptionConfiguration` naming `algorithm`, and `key` when the row
/// carries one.
fn configuration(algorithm: &str, key: Option<&str>) -> Value {
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(key));
    }
    Value::Object(members)
}

/// The configuration the store holds for `arn`, as `(sseAlgorithm, kmsKeyArn)`,
/// or `None` when the bucket holds no configuration of its own.
fn persisted(emulator: &Emulator, arn: &str, case: &str) -> Option<(String, Option<String>)> {
    let store = emulator.engine.state::<S3TablesState>();
    let store = store.lock();
    let bucket = store
        .table_buckets
        .get(arn)
        .unwrap_or_else(|| panic!("case {case:?}: the bucket is resident"));
    bucket.encryption.as_ref().map(|encryption| {
        (
            encryption.sse_algorithm.as_wire().to_string(),
            encryption.kms_key_arn.clone(),
        )
    })
}

/// A table bucket in `region`, by its ARN.
async fn bucket(emulator: &Emulator, region: &str, name: &str) -> String {
    let created = emulator
        .create_table_bucket(region, &json!({ "name": name }))
        .await;
    assert_eq!(
        created.status, 200,
        "the bucket {name:?} must be created, got {status} {body}",
        status = created.status,
        body = created.body
    );
    created.text("arn")
}

#[tokio::test]
async fn a_put_is_served_only_for_a_key_kms_resolves_in_the_requests_region() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, REGION, "key-access-subject").await;
    let principal = emulator.caller_identity_arn(REGION).await;

    // The keys every row names, each created through KMS's own `CreateKey`.
    let resident = emulator.create_kms_key(REGION).await;
    let second_resident = emulator.create_kms_key(REGION).await;
    let elsewhere = emulator.create_kms_key(OTHER_REGION).await;
    // A key of this account and Region whose id KMS holds no key under: the
    // resident key's ARN with its key id replaced by one nothing minted.
    let absent = resident
        .rsplit_once('/')
        .map(|(prefix, _)| format!("{prefix}/00000000-0000-0000-0000-000000000000"))
        .expect("a key ARN carries a key id");
    // The resident key's id in an account that is not this one.
    let foreign = resident.replace("000000000000", FOREIGN_ACCOUNT);

    // The bucket carries an accepted configuration BEFORE the refusals, so a
    // refused put is shown to leave the previous one rather than an absence.
    let applied = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", Some(&resident)))
        .await;
    assert_eq!(
        applied.status, 200,
        "case \"the resident key\": the put must be served, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    // (case, the key ARN the put names, whether the put must be served)
    let rows: Vec<(&str, String, bool)> = vec![
        ("a key of this account in this Region", second_resident, true),
        ("a key ARN naming another account", foreign, false),
        (
            "a key of this account in another Region",
            elsewhere.clone(),
            false,
        ),
        (
            "a well-formed key ARN naming no key at all",
            absent.clone(),
            false,
        ),
    ];

    for (case, key, served) in &rows {
        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", Some(key)))
            .await;
        if *served {
            assert_eq!(
                put.status, 200,
                "case {case:?}: the put must be served, got {status} {body}",
                status = put.status,
                body = put.body
            );
            let held = persisted(&emulator, &arn, case);
            assert_eq!(
                held,
                Some(("aws:kms".to_string(), Some(key.clone()))),
                "case {case:?}: the store must hold the key the put named, got {held:?}"
            );
            continue;
        }
        assert_eq!(
            put.status, 403,
            "case {case:?}: expected status 403, got {status} {body}",
            status = put.status,
            body = put.body
        );
        assert_eq!(
            put.error_code.as_deref(),
            Some(FORBIDDEN),
            "case {case:?}: expected {FORBIDDEN}, got {actual:?}",
            actual = put.error_code
        );
        assert_eq!(
            put.message(),
            refusal_sentence(&principal),
            "case {case:?}: expected KMS's sentence about the caller's own principal, got \
             {actual:?}",
            actual = put.message()
        );

        // The refusal is effect-free: the configuration the last served put
        // named is still the bucket's, and the read still reports it.
        let held = persisted(&emulator, &arn, case);
        assert_eq!(
            held,
            Some(("aws:kms".to_string(), Some(rows[0].1.clone()))),
            "case {case:?}: a refused put must change nothing, got {held:?}"
        );
    }

    // The same key ARNs the refusals named are served by the Region that DOES
    // hold the key: the gate is about the Region the request was signed for, not
    // about the value.
    let west = bucket(&emulator, OTHER_REGION, "key-access-west").await;
    let served = emulator
        .put_table_bucket_encryption(
            OTHER_REGION,
            &west,
            &configuration("aws:kms", Some(&elsewhere)),
        )
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the other Region's own key\": the put must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
    let read = emulator
        .get_table_bucket_encryption(OTHER_REGION, &west)
        .await;
    let (algorithm, key) = reported(&read.body, "the other Region's own key");
    assert_eq!(
        (algorithm.as_str(), key.as_deref()),
        ("aws:kms", Some(elsewhere.as_str())),
        "case \"the other Region's own key\": the read must report it, got {algorithm:?}/{key:?}"
    );
}

#[tokio::test]
async fn a_put_naming_no_key_is_served_whatever_kms_holds() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, REGION, "key-access-no-key").await;

    // S3-managed keys name no KMS key, so there is nothing for KMS to answer
    // about: the put is served on an engine whose KMS store holds no key at
    // all, and the refusal above is about the key a configuration names rather
    // than about SSE-KMS.
    let put = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("AES256", None))
        .await;
    assert_eq!(
        put.status, 200,
        "case \"S3-managed keys\": the put must be served, got {status} {body}",
        status = put.status,
        body = put.body
    );
    let held = persisted(&emulator, &arn, "S3-managed keys");
    assert_eq!(
        held,
        Some(("AES256".to_string(), None)),
        "case \"S3-managed keys\": the store must hold the configuration, got {held:?}"
    );
}

#[tokio::test]
async fn a_refused_put_leaves_the_bucket_on_the_service_default() {
    let emulator = Emulator::new();
    let arn = bucket(&emulator, REGION, "key-access-untouched").await;
    let principal = emulator.caller_identity_arn(REGION).await;

    // A bucket that carries NO configuration of its own: a refused put must
    // leave it reporting the service default and holding nothing.
    let refused = emulator
        .put_table_bucket_encryption(
            REGION,
            &arn,
            &configuration(
                "aws:kms",
                Some(&format!(
                    "arn:aws:kms:{REGION}:{FOREIGN_ACCOUNT}:key/\
                     11111111-2222-3333-4444-555555555555"
                )),
            ),
        )
        .await;
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (403, Some(FORBIDDEN)),
        "case \"a foreign account's key\": expected a 403 {FORBIDDEN}, got {status} {body}",
        status = refused.status,
        body = refused.body
    );
    assert_eq!(
        refused.message(),
        refusal_sentence(&principal),
        "case \"a foreign account's key\": expected KMS's sentence, got {actual:?}",
        actual = refused.message()
    );
    assert_eq!(
        persisted(&emulator, &arn, "a foreign account's key"),
        None,
        "case \"a foreign account's key\": the bucket must hold no configuration"
    );
    let read = emulator.get_table_bucket_encryption(REGION, &arn).await;
    let (algorithm, key) = reported(&read.body, "a foreign account's key");
    assert_eq!(
        (algorithm.as_str(), key.as_deref()),
        ("AES256", None),
        "case \"a foreign account's key\": the read must report the service default, got \
         {algorithm:?}/{key:?}"
    );

    // And the positive control the recorded refusal carries: the identical put
    // naming S3-managed keys is served afterwards.
    let served = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("AES256", None))
        .await;
    assert_eq!(
        served.status, 200,
        "case \"the positive control\": the put must be served, got {status} {body}",
        status = served.status,
        body = served.body
    );
}
