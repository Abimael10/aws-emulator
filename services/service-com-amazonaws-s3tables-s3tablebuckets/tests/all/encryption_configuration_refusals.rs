// SPDX-License-Identifier: CC0-1.0

//! How S3 Tables refuses an `encryptionConfiguration` it will not encrypt
//! under.
//!
//! The rule under test: the two members of that structure are ones the service
//! validates ITSELF, so each is refused with HTTP 400, this model's
//! `BadRequestException` and ONE fixed sentence naming the class of the failure —
//! an `sseAlgorithm` outside the modelled `SSEAlgorithm` set states that the
//! algorithm is invalid, a `kmsKeyArn` outside the modelled form states the one
//! key-ARN form the service takes — and nothing of the request appears in
//! either. The structure is carried by every operation that configures
//! encryption, so the same two sentences answer all of them, and a refused
//! request leaves the encryption that was already there.
//!
//! Each table spans the class the rule generalises over rather than one recorded
//! request: which operation carries the structure (a table bucket create, a
//! table create, a put on a bucket that exists), which spelling missed the
//! enumerated set, and which part of a key ARN is wrong (the scheme, the
//! service, the region, the account id, the resource type, the key id). The
//! members and their constraints are the model's own
//! (`smithy_query(query="shape", shape="EncryptionConfiguration,SSEAlgorithm")`:
//! `sseAlgorithm` targets `SSEAlgorithm` (`AES256`, `aws:kms`) and is required,
//! `kmsKeyArn` is `@length(1, 2048)` `@pattern`).
//!
//! The accepted rows keep the refusals honest: every pairing the model does
//! allow is stored and read back.

use serde_json::{json, Map, Value};

use crate::harness::Emulator;

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The one client-input error this service's model declares.
const BAD_REQUEST: &str = "BadRequestException";

/// The fixed sentences, spelled out here rather than read from the crate, so
/// this suite pins the wording itself.
const INVALID_SSE_ALGORITHM: &str = "The specified SSE algorithm is invalid.";
const INVALID_KMS_KEY_ARN: &str = "The specified KMS key ARN is invalid. Only key ARN format \
                                   'arn:<partition>:kms:<region>:<account-id>:key/<key-id>' is \
                                   supported.";

/// A KMS key ARN of the form the modelled `kmsKeyArn` pattern accepts, for the
/// rows that refuse something else.
const KEY_ARN: &str = "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

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

/// The spellings that miss the modelled `SSEAlgorithm` value set: the two
/// modelled values re-cased, the recorded nonsense algorithm, the name of a
/// cipher the service has no keys for, an empty value, and a pair joined with a
/// comma.
fn algorithms_outside_the_enum() -> Vec<&'static str> {
    vec![
        "aes256",
        "AWS:KMS",
        "aws-kms",
        "des-not-a-real-algorithm",
        "RC4",
        "",
        "AES256,aws:kms",
    ]
}

/// The values that cannot be a KMS key ARN: nothing ARN-shaped at all, an
/// account id one digit short of the modelled twelve, one naming another
/// service, one naming another resource type, one with no key id after the
/// resource type, and the ARN of a table bucket.
fn malformed_key_arns() -> Vec<String> {
    vec![
        "not-a-valid-kms-key-arn".to_string(),
        "arn:aws:kms:us-east-1:00000000000:key/1234abcd".to_string(),
        "arn:aws:s3tables:us-east-1:000000000000:key/1234abcd".to_string(),
        "arn:aws:kms:us-east-1:000000000000:alias/an-alias".to_string(),
        "arn:aws:kms:us-east-1:000000000000:key/".to_string(),
        format!("arn:aws:s3tables:{REGION}:000000000000:bucket/encryption-refusals"),
    ]
}

/// Assert `reply` is the 400 `BadRequestException` this service words with
/// `expected`, and that nothing of `sent` appears in it.
fn assert_refusal(reply: &crate::harness::Reply, expected: &str, sent: &str, case: &str) {
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
        expected,
        "case {case:?}: expected the service's fixed sentence, got {actual:?}",
        actual = reply.message()
    );
    if sent.chars().count() >= 3 {
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
async fn an_algorithm_outside_the_modelled_set_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let arn = bucket_with_namespace(&emulator, "sse-algorithm-refusals").await;

    for (index, algorithm) in algorithms_outside_the_enum().into_iter().enumerate() {
        // The put on a bucket that exists.
        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration(algorithm, None))
            .await;
        assert_refusal(
            &put,
            INVALID_SSE_ALGORITHM,
            algorithm,
            &format!("PutTableBucketEncryption with {algorithm:?}"),
        );

        // The table create, which configures the table's own encryption.
        let table = format!("refused_{index}");
        let created = emulator
            .create_table(
                REGION,
                &arn,
                "resident",
                &json!({
                    "name": table,
                    "format": "ICEBERG",
                    "encryptionConfiguration": configuration(algorithm, None),
                }),
            )
            .await;
        assert_refusal(
            &created,
            INVALID_SSE_ALGORITHM,
            algorithm,
            &format!("CreateTable with {algorithm:?}"),
        );
        // A refused create creates nothing.
        let read = emulator.get_table(REGION, &arn, "resident", &table).await;
        assert_eq!(
            read.status, 404,
            "case {algorithm:?}: a refused create must leave no table, got {status} {body}",
            status = read.status,
            body = read.body
        );

        // And the bucket create, which configures the bucket's default.
        let bucket = emulator
            .create_table_bucket(
                REGION,
                &json!({
                    "name": format!("sse-refused-{index}"),
                    "encryptionConfiguration": configuration(algorithm, None),
                }),
            )
            .await;
        assert_refusal(
            &bucket,
            INVALID_SSE_ALGORITHM,
            algorithm,
            &format!("CreateTableBucket with {algorithm:?}"),
        );
    }

    // Every refusal above left the bucket's own encryption alone: it still
    // reports the service default, since no put was ever accepted on it.
    let (algorithm, key) = reported(&emulator, &arn).await;
    assert_eq!(
        (algorithm.as_str(), key.as_deref()),
        ("AES256", None),
        "the refused puts must leave the bucket's encryption as it was"
    );
}

#[tokio::test]
async fn a_key_outside_the_modelled_form_is_refused_with_the_services_sentence() {
    let emulator = Emulator::new();
    let arn = bucket_with_namespace(&emulator, "kms-key-refusals").await;

    // The bucket carries an accepted configuration first, so a refused put is
    // shown to leave the PREVIOUS one rather than an absence.
    let applied = emulator
        .put_table_bucket_encryption(REGION, &arn, &configuration("aws:kms", Some(KEY_ARN)))
        .await;
    assert_eq!(
        applied.status, 200,
        "the first put must be served, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    for (index, malformed) in malformed_key_arns().into_iter().enumerate() {
        let put = emulator
            .put_table_bucket_encryption(
                REGION,
                &arn,
                &configuration("aws:kms", Some(&malformed)),
            )
            .await;
        assert_refusal(
            &put,
            INVALID_KMS_KEY_ARN,
            &malformed,
            &format!("PutTableBucketEncryption with {malformed:?}"),
        );

        let table = format!("key_refused_{index}");
        let created = emulator
            .create_table(
                REGION,
                &arn,
                "resident",
                &json!({
                    "name": table,
                    "format": "ICEBERG",
                    "encryptionConfiguration": configuration("aws:kms", Some(&malformed)),
                }),
            )
            .await;
        assert_refusal(
            &created,
            INVALID_KMS_KEY_ARN,
            &malformed,
            &format!("CreateTable with {malformed:?}"),
        );
        let read = emulator.get_table(REGION, &arn, "resident", &table).await;
        assert_eq!(
            read.status, 404,
            "case {malformed:?}: a refused create must leave no table, got {status} {body}",
            status = read.status,
            body = read.body
        );

        let bucket = emulator
            .create_table_bucket(
                REGION,
                &json!({
                    "name": format!("key-refused-{index}"),
                    "encryptionConfiguration": configuration("aws:kms", Some(&malformed)),
                }),
            )
            .await;
        assert_refusal(
            &bucket,
            INVALID_KMS_KEY_ARN,
            &malformed,
            &format!("CreateTableBucket with {malformed:?}"),
        );

        // The configuration the accepted put named is still the bucket's.
        let (algorithm, key) = reported(&emulator, &arn).await;
        assert_eq!(
            (algorithm.as_str(), key.as_deref()),
            ("aws:kms", Some(KEY_ARN)),
            "case {malformed:?}: a refused put must change nothing"
        );
    }
}

#[tokio::test]
async fn every_pairing_the_model_allows_is_stored_and_read_back() {
    let emulator = Emulator::new();
    let arn = bucket_with_namespace(&emulator, "encryption-accepted").await;

    // The modelled values, each with the key the model documents for it:
    // `kmsKeyArn` is "required only when `sseAlgorithm` is set to `aws:kms`".
    let accepted: [(&str, Option<&str>); 3] = [
        ("AES256", None),
        ("aws:kms", Some(KEY_ARN)),
        (
            "aws:kms",
            Some("arn:aws-us-gov:kms:us-gov-west-1:000000000000:key/another-key"),
        ),
    ];

    for (algorithm, key) in accepted {
        let put = emulator
            .put_table_bucket_encryption(REGION, &arn, &configuration(algorithm, key))
            .await;
        assert_eq!(
            put.status, 200,
            "case {algorithm:?}/{key:?}: the put must be served, got {status} {body}",
            status = put.status,
            body = put.body
        );
        let (reported_algorithm, reported_key) = reported(&emulator, &arn).await;
        assert_eq!(
            (reported_algorithm.as_str(), reported_key.as_deref()),
            (algorithm, key),
            "case {algorithm:?}/{key:?}: the read must report what the put stored"
        );
    }
}
