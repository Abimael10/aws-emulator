// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeleteTableBucketEncryption`.
//!
//! RULE: a table bucket is always encrypted, so what the delete removes is the
//! configuration the bucket holds of its OWN — the one a create or a put wrote —
//! which reverts the bucket to the service default a read reports for a bucket
//! nothing was ever set on (S3-managed keys, no KMS key). A bucket holding no
//! configuration of its own has nothing for the delete to remove, so the call is
//! refused with the modelled `NotFoundException` (HTTP 404) and the one fixed
//! sentence this service states that class with, writing nothing; a delete
//! against an ARN the store holds no bucket for is the same class worded with
//! this service's bucket sentence. A served delete answers the modelled 204 with
//! an empty body.
//!
//! The rows span what the rule generalises over: how the bucket came to hold a
//! configuration (a create named one, a put set one, a put replaced one), which
//! algorithm it held and whether that algorithm carried a KMS key, how many
//! deletes are applied in a row, and the Region the bucket lives in.
//!
//! The KMS keys are KMS's OWN, created through its `CreateKey` over the same
//! engine, because a configuration naming `aws:kms` is only storable when KMS
//! resolves the key it names.

use serde_json::{json, Map, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::ComAmazonawsS3tablesS3tablebucketsState as S3TablesState;

use super::harness::{expected_bucket_arn, Emulator};
use super::table_bucket_encryption::reported;

/// The modelled error code of every refusal in this suite.
const NOT_FOUND: &str = "NotFoundException";

/// The sentence this service refuses a delete on a bucket holding no
/// configuration of its own with, spelled out here rather than read from the
/// crate so this suite pins the wording itself.
const NO_SUCH_CONFIGURATION: &str = "The specified encryption configuration does not exist.";

/// The sentence this service refuses a request addressing a bucket it does not
/// hold with.
const NO_SUCH_BUCKET: &str = "The specified bucket does not exist.";

/// An `encryptionConfiguration` naming `algorithm`, and `key` when the case
/// carries one.
fn configuration(algorithm: &str, key: Option<&str>) -> Value {
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(key));
    }
    Value::Object(members)
}

/// Whether the store holds an EXPLICIT configuration for `arn` — the record a
/// read reports from, as opposed to the default it reconstructs per request.
fn holds_configuration(emulator: &Emulator, arn: &str, case: &str) -> bool {
    let store = emulator.engine.state::<S3TablesState>();
    let store = store.lock();
    store
        .table_buckets
        .get(arn)
        .unwrap_or_else(|| panic!("case {case:?}: the bucket is resident"))
        .encryption
        .is_some()
}

/// Assert `reply` is the 404 `NotFoundException` worded with `expected`.
fn assert_not_found(reply: &super::harness::Reply, expected: &str, case: &str) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected status 404, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(NOT_FOUND),
        "case {case:?}: expected {NOT_FOUND}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        expected,
        "case {case:?}: expected the service's fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

/// Assert the bucket at `arn` reports the service default and holds no explicit
/// configuration.
async fn assert_default(emulator: &Emulator, region: &str, arn: &str, case: &str) {
    let read = emulator.get_table_bucket_encryption(region, arn).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    let (algorithm, key) = reported(&read.body, case);
    assert_eq!(
        (algorithm.as_str(), key.as_deref()),
        ("AES256", None),
        "case {case:?}: expected the service default, got {algorithm:?}/{key:?}"
    );
    assert!(
        !holds_configuration(emulator, arn, case),
        "case {case:?}: the bucket must hold no configuration of its own"
    );
}

/// One row: how the bucket comes to hold a configuration, and what it holds when
/// the delete runs.
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// The create's `encryptionConfiguration`, as `(sseAlgorithm, carries a KMS
    /// key)`, or `None` when the create names none.
    created_with: Option<(&'static str, bool)>,
    /// The configurations put on the bucket before the delete, in order.
    puts: &'static [(&'static str, bool)],
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a put of S3-managed keys",
            region: "us-east-1",
            bucket: "delete-encryption-put-aes",
            created_with: None,
            puts: &[("AES256", false)],
        },
        Case {
            label: "a put of KMS-managed keys with a key",
            region: "us-east-1",
            bucket: "delete-encryption-put-kms",
            created_with: None,
            puts: &[("aws:kms", true)],
        },
        Case {
            label: "a create that named S3-managed keys, no put",
            region: "us-east-1",
            bucket: "delete-encryption-created-aes",
            created_with: Some(("AES256", false)),
            puts: &[],
        },
        Case {
            label: "a create that named KMS-managed keys, no put",
            region: "us-east-1",
            bucket: "delete-encryption-created-kms",
            created_with: Some(("aws:kms", true)),
            puts: &[],
        },
        Case {
            label: "a put that replaced the configuration the create named",
            region: "us-east-1",
            bucket: "delete-encryption-replaced",
            created_with: Some(("aws:kms", true)),
            puts: &[("AES256", false)],
        },
        Case {
            label: "three puts in a row, ending on a KMS key",
            region: "eu-west-1",
            bucket: "delete-encryption-west-sequence",
            created_with: None,
            puts: &[("AES256", false), ("aws:kms", true), ("aws:kms", true)],
        },
    ]
}

#[tokio::test]
async fn a_delete_reverts_the_bucket_to_the_service_default() {
    let emulator = Emulator::new();

    for case in cases() {
        // Every case's keys are KMS's own, in the Region the case works in.
        let key = emulator.create_kms_key(case.region).await;
        let member = |(algorithm, carries_key): (&str, bool)| {
            configuration(algorithm, carries_key.then_some(key.as_str()))
        };

        let mut request = Map::new();
        request.insert("name".to_string(), json!(case.bucket));
        if let Some(created_with) = case.created_with {
            request.insert("encryptionConfiguration".to_string(), member(created_with));
        }
        let created = emulator
            .create_table_bucket(case.region, &Value::Object(request))
            .await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: the bucket create must succeed, got {status} {body}",
            label = case.label,
            status = created.status,
            body = created.body
        );
        let arn = created.text("arn");

        for configuration in case.puts {
            let put = emulator
                .put_table_bucket_encryption(case.region, &arn, &member(*configuration))
                .await;
            assert_eq!(
                put.status, 200,
                "case {label:?}: the put of {configuration:?} must succeed, got {status} {body}",
                label = case.label,
                status = put.status,
                body = put.body
            );
        }

        // The bucket holds a configuration of its own, so the delete is served.
        assert!(
            holds_configuration(&emulator, &arn, case.label),
            "case {label:?}: the fixture must hold a configuration to delete",
            label = case.label
        );
        let deleted = emulator
            .delete_table_bucket_encryption(case.region, &arn)
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {label:?}: the delete must answer the modelled 204, got {status} {body}",
            label = case.label,
            status = deleted.status,
            body = deleted.body
        );
        assert_eq!(
            deleted.body,
            Value::Null,
            "case {label:?}: the delete answers an empty body, got {body}",
            label = case.label,
            body = deleted.body
        );

        // The bucket is back to the service default, key and all.
        assert_default(&emulator, case.region, &arn, case.label).await;

        // And a SECOND delete finds nothing of the bucket's to remove: the
        // operation removes a configuration, it does not turn encryption off.
        let again = emulator
            .delete_table_bucket_encryption(case.region, &arn)
            .await;
        assert_not_found(&again, NO_SUCH_CONFIGURATION, case.label);
        assert_default(&emulator, case.region, &arn, case.label).await;
    }
}

#[tokio::test]
async fn a_bucket_holding_no_configuration_of_its_own_is_refused_and_unchanged() {
    let emulator = Emulator::new();
    let region = "us-east-1";

    // A bucket whose create named no configuration: it reports the service
    // default, which is not the bucket's to remove.
    let created = emulator
        .create_table_bucket(region, &json!({ "name": "delete-encryption-implicit" }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the implicit default\": the create must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let arn = created.text("arn");
    assert_default(&emulator, region, &arn, "the implicit default").await;

    // Twice, so the refusal is shown to leave the bucket in the state that
    // caused it rather than to settle anything.
    for attempt in ["the first delete", "the second delete"] {
        let refused = emulator.delete_table_bucket_encryption(region, &arn).await;
        assert_not_found(&refused, NO_SUCH_CONFIGURATION, attempt);
        assert_default(&emulator, region, &arn, attempt).await;
    }

    // The positive control: once a configuration IS put, the identical delete
    // is served and reverts the bucket.
    let put = emulator
        .put_table_bucket_encryption(region, &arn, &configuration("AES256", None))
        .await;
    assert_eq!(
        put.status, 200,
        "case \"the positive control\": the put must succeed, got {status} {body}",
        status = put.status,
        body = put.body
    );
    let deleted = emulator.delete_table_bucket_encryption(region, &arn).await;
    assert_eq!(
        deleted.status, 204,
        "case \"the positive control\": the delete must succeed, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );
    assert_default(&emulator, region, &arn, "the positive control").await;
}

#[tokio::test]
async fn a_delete_against_a_bucket_the_store_does_not_hold_is_refused() {
    let emulator = Emulator::new();

    // One resident bucket carrying an explicit configuration, so the refusals
    // below are about the ARN each names and can be shown to write nothing.
    let created = emulator
        .create_table_bucket("us-east-1", &json!({ "name": "delete-encryption-resident" }))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the resident bucket\": the create must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let resident = created.text("arn");
    let key = emulator.create_kms_key("us-east-1").await;
    let applied = emulator
        .put_table_bucket_encryption(
            "us-east-1",
            &resident,
            &configuration("aws:kms", Some(&key)),
        )
        .await;
    assert_eq!(
        applied.status, 200,
        "case \"the resident bucket\": the put must succeed, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    // A bucket that existed and was then deleted: the operation must stop
    // answering for it.
    let doomed = emulator
        .create_table_bucket("us-east-1", &json!({ "name": "delete-encryption-gone" }))
        .await
        .text("arn");
    let dropped = emulator.delete_table_bucket("us-east-1", &doomed).await;
    assert_eq!(
        dropped.status, 204,
        "case \"a deleted bucket\": the bucket delete must succeed, got {status} {body}",
        status = dropped.status,
        body = dropped.body
    );

    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a bucket that was never created",
            expected_bucket_arn("us-east-1", "delete-encryption-absent"),
            "us-east-1",
        ),
        ("a bucket that was deleted", doomed, "us-east-1"),
        (
            "the resident bucket's name in another region",
            expected_bucket_arn("eu-west-1", "delete-encryption-resident"),
            "eu-west-1",
        ),
        (
            "another account's bucket of the same name",
            resident.replace("000000000000", "123456789012"),
            "us-east-1",
        ),
    ];

    for (case, arn, region) in &rows {
        let refused = emulator.delete_table_bucket_encryption(region, arn).await;
        assert_not_found(&refused, NO_SUCH_BUCKET, case);
    }

    // The resident bucket is untouched by any of those refusals.
    let read = emulator
        .get_table_bucket_encryption("us-east-1", &resident)
        .await;
    let (algorithm, reported_key) = reported(&read.body, "the resident bucket");
    assert_eq!(
        (algorithm.as_str(), reported_key.as_deref()),
        ("aws:kms", Some(key.as_str())),
        "case \"the resident bucket\": a refused delete must change nothing, got \
         {algorithm:?}/{reported_key:?}"
    );
}
