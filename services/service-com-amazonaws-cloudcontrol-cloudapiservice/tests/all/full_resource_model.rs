// SPDX-License-Identifier: CC0-1.0

//! The FULL current model `GetResource` reports, and what a patch of it may not touch.
//!
//! Two halves of one rule, both about the schema's classification of a type's properties:
//!
//! * **`ResourceDescription.Properties` is the resource's full current model as the OWNING
//!   service holds it — the schema's `readOnlyProperties`, which that service computes, and
//!   the properties it applies without any client input included — rather than only what a
//!   desired state set.** A client reads a read-only value out of this model to wire the
//!   resource into policies and references, and a server-enforced default that is invisible
//!   makes the API's view of the resource materially wrong.
//! * **a patch that names one of those `readOnlyProperties` is refused, because the value is
//!   the owning service's to compute**: `ValidationException` "Invalid patch update:
//!   readOnlyProperties [`<pointers>`] cannot be updated".
//!
//! The rows differ along what the rules generalise over: which owning service holds the
//! resource (Amazon SQS, Amazon S3, CloudWatch Logs), whether the read-only value is an ARN
//! or a URL the service derives, which configuration the service applies without client
//! input (Block Public Access, default encryption), whether the desired state named it at
//! all, and whether the patch names a read-only property, a writable one, or both at once.

use serde_json::{json, Value};

use super::emulator::{code, message, Emulator};

/// A type whose schema declares the read-only `Arn` beside the identity.
const QUEUE: &str = "AWS::SQS::Queue";

/// A type whose owning service applies a configuration to every new resource without any
/// client input.
const BUCKET: &str = "AWS::S3::Bucket";

/// A type whose schema declares one read-only property, its ARN.
const LOG_GROUP: &str = "AWS::Logs::LogGroup";

/// The `PublicAccessBlockConfiguration` Amazon S3 applies to every new bucket: "Amazon S3
/// automatically enables S3 Block Public Access … for new buckets", so all four settings are
/// on.
fn blocked_public_access() -> Value {
    json!({
        "BlockPublicAcls": true,
        "BlockPublicPolicy": true,
        "IgnorePublicAcls": true,
        "RestrictPublicBuckets": true,
    })
}

#[tokio::test]
async fn a_read_reports_the_read_only_properties_the_owning_service_computed() {
    let emulator = Emulator::with_sqs();
    let identifier = emulator
        .provision(QUEUE, &json!({ "QueueName": "read-only-queue" }))
        .await;

    // The queue's own ARN, as the owning service holds it: read off ITS api rather than
    // spelled here, so the case pins the model to the resource rather than to a string.
    let attributes = emulator
        .sqs_ok(
            "GetQueueAttributes",
            json!({ "QueueUrl": identifier, "AttributeNames": ["QueueArn"] }),
        )
        .await;
    let native_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !native_arn.is_empty(),
        "the owning service must report the queue's ARN, got {attributes:?}"
    );

    let properties = emulator.properties(QUEUE, &identifier).await;
    assert_eq!(
        properties["Arn"].as_str(),
        Some(native_arn.as_str()),
        "expected the model to carry the read-only Arn the owning service computed, got \
         {properties:?}"
    );
    // The identity is the schema's own primary identifier, which for this type is the URL
    // that service assigned, so the model carries it too.
    assert_eq!(
        properties["QueueUrl"].as_str(),
        Some(identifier.as_str()),
        "expected the model to carry the queue's url, got {properties:?}"
    );
}

#[tokio::test]
async fn a_read_reports_a_configuration_the_owning_service_applied_without_client_input() {
    let emulator = Emulator::with_s3();

    // (case, desired state, the configuration the model must report)
    let rows: [(&str, Value, Value); 3] = [
        (
            "a desired state that names no configuration at all",
            json!({ "BucketName": "default-blocked-bucket" }),
            blocked_public_access(),
        ),
        (
            "a desired state that turns every setting off",
            json!({
                "BucketName": "unblocked-bucket",
                "PublicAccessBlockConfiguration": {
                    "BlockPublicAcls": false,
                    "BlockPublicPolicy": false,
                    "IgnorePublicAcls": false,
                    "RestrictPublicBuckets": false,
                },
            }),
            json!({
                "BlockPublicAcls": false,
                "BlockPublicPolicy": false,
                "IgnorePublicAcls": false,
                "RestrictPublicBuckets": false,
            }),
        ),
        (
            "a desired state that names some settings, so the rest are off",
            json!({
                "BucketName": "partly-blocked-bucket",
                "PublicAccessBlockConfiguration": { "BlockPublicPolicy": true },
            }),
            json!({
                "BlockPublicAcls": false,
                "BlockPublicPolicy": true,
                "IgnorePublicAcls": false,
                "RestrictPublicBuckets": false,
            }),
        ),
    ];

    let mut provisioned = Vec::new();
    for (_, desired, _) in &rows {
        provisioned.push(emulator.provision(BUCKET, desired).await);
    }
    for ((case, _, expected), identifier) in rows.iter().zip(&provisioned) {
        let properties = emulator.properties(BUCKET, identifier).await;
        assert_eq!(
            &properties["PublicAccessBlockConfiguration"],
            expected,
            "case {case:?}: expected the model to report {expected:?}, got {properties:?}"
        );
        // And it is the configuration the OWNING service holds: its own subresource reader
        // answers with the same four settings.
        let (status, document) = emulator
            .s3_bucket_call("GET", &format!("/{identifier}?publicAccessBlock"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: s3:GetPublicAccessBlock must answer, got {status} with \
             {document:?}"
        );
        for (member, on) in expected.as_object().expect("the four settings") {
            let fragment = format!("<{member}>{}</{member}>", on.as_bool().unwrap_or_default());
            assert!(
                document.contains(&fragment),
                "case {case:?}: expected s3:GetPublicAccessBlock to report {fragment:?}, \
                 got {document:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_read_reports_the_default_encryption_the_owning_service_applied() {
    let emulator = Emulator::with_s3();

    // (case, desired state, the rule the model must report, the algorithm the owning
    // service's own subresource reader must report)
    let rows: [(&str, Value, Value, &str); 2] = [
        (
            "a desired state that names no encryption at all",
            json!({ "BucketName": "default-encrypted-bucket" }),
            json!({
                "ServerSideEncryptionConfiguration": [{
                    "ServerSideEncryptionByDefault": { "SSEAlgorithm": "AES256" },
                    "BucketKeyEnabled": false,
                }],
            }),
            "AES256",
        ),
        (
            "a desired state that configures a KMS key",
            json!({
                "BucketName": "kms-encrypted-bucket",
                "BucketEncryption": {
                    "ServerSideEncryptionConfiguration": [{
                        "ServerSideEncryptionByDefault": {
                            "SSEAlgorithm": "aws:kms",
                            "KMSMasterKeyID": "alias/aws/s3",
                        },
                        "BucketKeyEnabled": true,
                    }],
                },
            }),
            json!({
                "ServerSideEncryptionConfiguration": [{
                    "ServerSideEncryptionByDefault": {
                        "SSEAlgorithm": "aws:kms",
                        "KMSMasterKeyID": "alias/aws/s3",
                    },
                    "BucketKeyEnabled": true,
                }],
            }),
            "aws:kms",
        ),
    ];

    let mut provisioned = Vec::new();
    for (_, desired, _, _) in &rows {
        provisioned.push(emulator.provision(BUCKET, desired).await);
    }
    for ((case, _, expected, algorithm), identifier) in rows.iter().zip(&provisioned) {
        let properties = emulator.properties(BUCKET, identifier).await;
        assert_eq!(
            &properties["BucketEncryption"], expected,
            "case {case:?}: expected the model to report {expected:?}, got {properties:?}"
        );
        let (status, document) = emulator
            .s3_bucket_call("GET", &format!("/{identifier}?encryption"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: s3:GetBucketEncryption must answer, got {status} with \
             {document:?}"
        );
        let fragment = format!("<SSEAlgorithm>{algorithm}</SSEAlgorithm>");
        assert!(
            document.contains(&fragment),
            "case {case:?}: expected s3:GetBucketEncryption to report {fragment:?}, got \
             {document:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_of_a_read_only_property_is_refused() {
    // (case, type name, the emulator its owning service is mounted on, desired state, the
    // patch, the pointers the refusal names)
    let rows: [(&str, &str, Emulator, Value, Value, &str); 4] = [
        (
            "the ARN of a queue",
            QUEUE,
            Emulator::with_sqs(),
            json!({ "QueueName": "arn-patched-queue" }),
            json!([{ "op": "replace", "path": "/Arn", "value": "arn:aws:sqs:::bogus" }]),
            "/properties/Arn",
        ),
        (
            "the ARN of a log group",
            LOG_GROUP,
            Emulator::with_cloudwatch_logs(),
            json!({ "LogGroupName": "arn-patched-group" }),
            json!([{ "op": "replace", "path": "/Arn", "value": "arn:aws:logs:::bogus" }]),
            "/properties/Arn",
        ),
        (
            "a URL the owning service derives for a bucket",
            BUCKET,
            Emulator::with_s3(),
            json!({ "BucketName": "url-patched-bucket" }),
            json!([{ "op": "add", "path": "/WebsiteURL", "value": "http://example.test" }]),
            "/properties/WebsiteURL",
        ),
        (
            "two read-only properties at once, listed as the schema declares them",
            BUCKET,
            Emulator::with_s3(),
            json!({ "BucketName": "both-patched-bucket" }),
            json!([
                { "op": "add", "path": "/DomainName", "value": "example.test" },
                { "op": "add", "path": "/Arn", "value": "arn:aws:s3:::bogus" },
            ]),
            "/properties/Arn, /properties/DomainName",
        ),
    ];

    for (case, type_name, emulator, desired, patch, pointers) in rows {
        let identifier = emulator.provision(type_name, &desired).await;
        let before = emulator.properties(type_name, &identifier).await;
        let (status, body) = emulator
            .update(type_name, &identifier, &patch, None)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert_eq!(
            code(&body),
            "ValidationException",
            "case {case:?}: unexpected error code in {body:?}"
        );
        assert_eq!(
            message(&body),
            format!("Invalid patch update: readOnlyProperties [{pointers}] cannot be updated"),
            "case {case:?}: unexpected wording"
        );
        // A refused patch writes nothing: the model is the one the resource already held.
        let after = emulator.properties(type_name, &identifier).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused patch to leave the model alone, got \
             {after:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_of_a_writable_property_still_applies() {
    let emulator = Emulator::with_cloudwatch_logs();
    let identifier = emulator
        .provision(LOG_GROUP, &json!({ "LogGroupName": "writable-group" }))
        .await;

    // The control for the refusal above: a property the schema does NOT declare read-only
    // is the caller's to set, so the same shape of patch applies and the read reports it.
    let patch = json!([{ "op": "add", "path": "/RetentionInDays", "value": 7 }]);
    emulator
        .update_and_settle(LOG_GROUP, &identifier, &patch)
        .await;
    let properties = emulator.properties(LOG_GROUP, &identifier).await;
    assert_eq!(
        properties["RetentionInDays"].as_i64(),
        Some(7),
        "expected the patch of a writable property to apply, got {properties:?}"
    );
    // And the read-only property is still the one the owning service computed.
    assert!(
        properties["Arn"]
            .as_str()
            .is_some_and(|arn| arn.contains(&identifier)),
        "expected the model to still carry the group's own ARN, got {properties:?}"
    );
}
