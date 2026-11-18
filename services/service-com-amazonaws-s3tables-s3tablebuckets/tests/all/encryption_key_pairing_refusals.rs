// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables refuses an `encryptionConfiguration` whose algorithm and KMS
//! key disagree.
//!
//! The rule under test: a configuration that names no KMS key ARN of the one
//! form this service encrypts under is refused with HTTP 400, this model's
//! `BadRequestException` and the service's single fixed invalid-key-ARN
//! sentence — whatever way the key missed that form, and whichever operation
//! carried the structure — and the refusal changes nothing.
//!
//! The model is what says when a key is part of the form: `kmsKeyArn` "is
//! required only when `sseAlgorithm` is set to `aws:kms`", and is
//! `@length(1, 2048)` with a key-ARN `@pattern`, while `sseAlgorithm` is
//! `@required` and targets `SSEAlgorithm` (`AES256`, `aws:kms`)
//! (`smithy_query(query="shape", shape="EncryptionConfiguration,SSEAlgorithm")`).
//! So `aws:kms` with no key names no key at all, and a key under `AES256` names
//! one for an algorithm that takes none: both are the same class as a key
//! outside the modelled form, and the service states the class one way.
//!
//! The table below spans that class rather than one recorded request: which
//! side of the pairing is wrong (an absent key where one is required, a key
//! where none is), how it is wrong (absent, empty, well-formed-but-unused,
//! malformed), and which operation carries the structure (a put on a bucket
//! that exists, a table create, a table bucket create). A refused put leaves
//! the configuration the bucket already carried, and a configuration the model
//! does allow still applies to the same bucket afterwards.

use serde_json::{json, Map, Value};

use crate::harness::{Emulator, ACCOUNT};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The one client-input error this service's model declares.
const BAD_REQUEST: &str = "BadRequestException";

/// The fixed sentence, spelled out here rather than read from the crate, so
/// this suite pins the wording itself.
const INVALID_KMS_KEY_ARN: &str = "The specified KMS key ARN is invalid. Only key ARN format \
                                   'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is \
                                   supported.";

/// One pairing of an algorithm and a key that names no usable key.
struct Pairing {
    /// What makes this row its own case, for the assertion messages.
    label: &'static str,
    algorithm: &'static str,
    key: Option<String>,
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

/// The pairings that name no key ARN of the supported form: `aws:kms` with no
/// key member at all and with an empty one, and `AES256` carrying a key — one
/// KMS itself holds (`held`), and one that is not ARN-shaped.
fn disagreeing_pairings(held: &str) -> Vec<Pairing> {
    vec![
        Pairing {
            label: "aws:kms with no kmsKeyArn member",
            algorithm: "aws:kms",
            key: None,
        },
        Pairing {
            label: "aws:kms with an empty kmsKeyArn",
            algorithm: "aws:kms",
            key: Some(String::new()),
        },
        Pairing {
            label: "AES256 with a key ARN KMS holds",
            algorithm: "AES256",
            key: Some(held.to_string()),
        },
        Pairing {
            label: "AES256 with a key ARN outside the modelled form",
            algorithm: "AES256",
            key: Some("not-a-kms-key-arn".to_string()),
        },
    ]
}

/// Assert `reply` is the 400 `BadRequestException` this service words with the
/// fixed invalid-key-ARN sentence, and that nothing of `sent` appears in it.
fn assert_refusal(reply: &crate::harness::Reply, sent: Option<&str>, case: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(BAD_REQUEST),
        "case {case:?}: expected error type {BAD_REQUEST}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        INVALID_KMS_KEY_ARN,
        "case {case:?}: expected the service's fixed invalid-key-ARN sentence, got {actual:?}",
        actual = reply.message()
    );
    if let Some(sent) = sent.filter(|value| value.chars().count() >= 3) {
        assert!(
            !reply.message().contains(sent),
            "case {case:?}: the refusal must not echo the request's {sent:?}, got {actual:?}",
            actual = reply.message()
        );
    }
}

/// A table bucket with one namespace in it, and the bucket's ARN.
async fn bucket_with_namespace(emulator: &Emulator, bucket: &str) -> String {
    let created = emulator
        .create_table_bucket(REGION, &json!({ "name": bucket }))
        .await;
    assert_eq!(
        created.status, 200,
        "the bucket {bucket:?} must be created, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let arn = created.text("arn");
    let namespace = emulator
        .create_namespace(REGION, &arn, &json!({ "namespace": ["resident"] }))
        .await;
    assert_eq!(
        namespace.status, 200,
        "the namespace must be created, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );
    arn
}

/// The `sseAlgorithm` and `kmsKeyArn` the store reports for `arn`.
async fn reported(emulator: &Emulator, arn: &str) -> (String, Option<String>) {
    let read = emulator.get_table_bucket_encryption(REGION, arn).await;
    assert_eq!(
        read.status, 200,
        "the encryption read must be served, got {status} {body}",
        status = read.status,
        body = read.body
    );
    let configuration = read
        .body
        .get("encryptionConfiguration")
        .unwrap_or_else(|| panic!("the read carries no configuration: {body}", body = read.body));
    let algorithm = configuration
        .get("sseAlgorithm")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the configuration carries no algorithm: {configuration}"))
        .to_string();
    let key = configuration
        .get("kmsKeyArn")
        .and_then(Value::as_str)
        .map(str::to_string);
    (algorithm, key)
}

#[tokio::test]
async fn a_configuration_whose_algorithm_and_key_disagree_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let arn = bucket_with_namespace(&emulator, "encryption-pairings").await;
    // A key KMS itself holds, both for the accepted configuration the refusals
    // are shown to leave alone and for the row that pairs it with `AES256`.
    let held = emulator.create_kms_key(REGION).await;

    // The bucket carries an accepted configuration first, so a refused call is
    // shown to leave the PREVIOUS one rather than an absence.
    let applied = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", Some(&held)))
        .await;
    assert_eq!(
        applied.status, 200,
        "the first put must be served, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    for (index, pairing) in disagreeing_pairings(&held).into_iter().enumerate() {
        let sent = pairing.key.clone();
        let configuration = configuration(pairing.algorithm, pairing.key.as_deref());

        // The put on a bucket that exists.
        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration)
            .await;
        assert_refusal(
            &put,
            sent.as_deref(),
            &format!("PutTableBucketEncryption with {label}", label = pairing.label),
        );

        // The table create, which configures the table's own encryption.
        let table = format!("pairing_refused_{index}");
        let created = emulator
            .create_table(
                REGION,
                &arn,
                "resident",
                &json!({
                    "name": table,
                    "format": "ICEBERG",
                    "encryptionConfiguration": configuration,
                }),
            )
            .await;
        assert_refusal(
            &created,
            sent.as_deref(),
            &format!("CreateTable with {label}", label = pairing.label),
        );
        // A refused create creates nothing.
        let read = emulator.get_table(REGION, &arn, "resident", &table).await;
        assert_eq!(
            read.status,
            404,
            "case {label:?}: a refused create must leave no table, got {status} {body}",
            label = pairing.label,
            status = read.status,
            body = read.body
        );

        // And the bucket create, which configures the bucket's default.
        let bucket_name = format!("pairing-refused-{index}");
        let bucket = emulator
            .create_table_bucket(
                REGION,
                &json!({
                    "name": bucket_name,
                    "encryptionConfiguration": configuration,
                }),
            )
            .await;
        assert_refusal(
            &bucket,
            sent.as_deref(),
            &format!("CreateTableBucket with {label}", label = pairing.label),
        );
        // A refused create creates nothing: the bucket the call named has no
        // encryption to read because it has no bucket.
        let refused_arn = format!("arn:aws:s3tables:{REGION}:{ACCOUNT}:bucket/{bucket_name}");
        let absent = emulator.get_table_bucket(REGION, &refused_arn).await;
        assert_eq!(
            absent.status,
            404,
            "case {label:?}: a refused create must leave no bucket, got {status} {body}",
            label = pairing.label,
            status = absent.status,
            body = absent.body
        );

        // The configuration the accepted put named is still the bucket's.
        let (algorithm, key) = reported(&emulator, &arn).await;
        assert_eq!(
            (algorithm.as_str(), key.as_deref()),
            ("aws:kms", Some(held.as_str())),
            "case {label:?}: a refused put must change nothing",
            label = pairing.label
        );
    }
}

#[tokio::test]
async fn a_pairing_the_model_allows_still_applies_after_a_refused_one() {
    let emulator = Emulator::new();
    let arn = bucket_with_namespace(&emulator, "encryption-after-refusal").await;
    let held = emulator.create_kms_key(REGION).await;

    // Each refused pairing, followed by a configuration the model does allow on
    // the SAME bucket: the refusal is about the request, so it leaves the bucket
    // no worse off for configuring encryption.
    let accepted: [(&str, Option<&str>); 2] = [("AES256", None), ("aws:kms", Some(held.as_str()))];
    for (pairing, (algorithm, key)) in disagreeing_pairings(&held)
        .into_iter()
        .zip(accepted.into_iter().cycle())
    {
        let refused = emulator
            .put_table_bucket_encryption(
                REGION,
                &arn,
                &configuration(pairing.algorithm, pairing.key.as_deref()),
            )
            .await;
        assert_refusal(&refused, pairing.key.as_deref(), pairing.label);

        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration(algorithm, key))
            .await;
        assert_eq!(
            put.status,
            200,
            "case {label:?}: the following {algorithm:?} put must be served, got {status} {body}",
            label = pairing.label,
            status = put.status,
            body = put.body
        );
        let (reported_algorithm, reported_key) = reported(&emulator, &arn).await;
        assert_eq!(
            (reported_algorithm.as_str(), reported_key.as_deref()),
            (algorithm, key),
            "case {label:?}: the read must report what the accepted put stored",
            label = pairing.label
        );
    }
}
