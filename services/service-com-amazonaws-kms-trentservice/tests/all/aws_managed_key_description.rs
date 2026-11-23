// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `Description` an Amazon Web Services managed key carries,
//! driven over the wire through the shared [`super::harness`].
//!
//! The rule under test: the key behind a predefined `alias/aws/<service-name>` alias is
//! seeded with KMS's default-key description — the fixed sentence "Default key that
//! protects my <resources> when no other key is defined", whose only variable part names
//! the resources the service owning the alias protects with that key — and every reader of
//! `KeyMetadata` reports it unchanged, through every identifier form and at every Region's
//! endpoint. A key the caller creates keeps the description the caller gave it.
//!
//! The table's rows differ in the service-name segment of the alias, so they span both
//! kinds of noun phrase: a service whose managed key names a resource of its own, and one
//! whose managed key names the service's data. `KeyMetadata.Description` is typed
//! `DescriptionType` (`len 0..8192`) and documented only as "the description of the KMS
//! key" (`smithy_query(query="doc",shape="KeyMetadata",member="Description")`), so the
//! wording of each row is the default-key description KMS is observed to seed, not a value
//! the model states.

use serde_json::json;

use super::harness::{alias_arn, create_key, describe, describe_in, text, router, REGION};

/// The reserved prefix a predefined Amazon Web Services alias lives under: "the alias
/// name cannot begin with alias/aws/. The alias/aws/ prefix is reserved for Amazon Web
/// Services managed keys"
/// (`smithy_query(query="doc",shape="CreateAliasRequest",member="AliasName")`).
const RESERVED_PREFIX: &str = "alias/aws/";

/// The sentence every default-key description is, around the resources it names.
fn default_key_description(resources: &str) -> String {
    format!("Default key that protects my {resources} when no other key is defined")
}

/// One row of the table: the alias' service-name segment, and the resources that
/// service's managed key protects.
///
/// Both kinds of row are present: a service whose managed key names a resource of its
/// own (`dynamodb`, `ebs`, `sqs`, `lambda`, `s3`, `secretsmanager`), and one whose
/// managed key names the service's own data (`backup`, `glue`, `timestream`, and a
/// hyphenated segment, which `aws/<service-name>` admits).
fn rows() -> Vec<(&'static str, &'static str)> {
    vec![
        ("dynamodb", "DynamoDB tables"),
        ("ebs", "EBS volumes"),
        ("elasticfilesystem", "EFS filesystems"),
        ("lambda", "Lambda functions"),
        ("s3", "S3 objects"),
        ("secretsmanager", "Secrets Manager secrets"),
        ("sqs", "SQS queues"),
        ("backup", "Backup data"),
        ("glue", "Glue data"),
        ("timestream", "Timestream data"),
        ("kinesis-video", "Kinesis-video data"),
    ]
}

/// Every predefined alias' managed key reports its service's default-key description,
/// and reports the same one through every identifier form that resolves the key.
#[tokio::test]
async fn an_aws_managed_key_carries_its_service_default_key_description() {
    let router = router();

    for (service, resources) in rows() {
        let alias = format!("{RESERVED_PREFIX}{service}");
        let expected = default_key_description(resources);
        let metadata = describe(&router, &alias).await;
        let key_id = text(service, &metadata, "KeyId");
        assert_eq!(
            metadata["Description"],
            json!(expected),
            "case {service:?}: expected Description {expected:?}, got {metadata}"
        );
        assert_eq!(
            metadata["KeyManager"],
            json!("AWS"),
            "case {service:?}: the key of a predefined alias is Amazon Web Services \
             managed, got {metadata}"
        );

        // The description belongs to the KEY, so every form that resolves it reports the
        // same text: the alias name, the alias ARN, the key id and the key ARN.
        let key_arn = format!(
            "arn:aws:kms:{REGION}:{account}:key/{key_id}",
            account = engine::emulator_account_id()
        );
        for (form, identifier) in [
            ("alias name", alias.clone()),
            ("alias ARN", alias_arn(REGION, &alias)),
            ("key id", key_id.clone()),
            ("key ARN", key_arn),
        ] {
            let again = describe(&router, &identifier).await;
            assert_eq!(
                again["Description"],
                json!(expected),
                "case {service:?} read by {form}: expected Description {expected:?}, \
                 got {again}"
            );
        }
    }
}

/// The description is the SERVICE's, not the endpoint's: each Region's endpoint holds its
/// own managed key for a predefined alias, and each of those keys carries the same
/// default-key description.
#[tokio::test]
async fn the_default_key_description_is_the_same_at_every_region() {
    let router = router();

    for (service, resources) in rows() {
        let alias = format!("{RESERVED_PREFIX}{service}");
        let expected = default_key_description(resources);
        for region in ["us-east-1", "eu-west-1", "ap-south-1"] {
            let metadata = describe_in(&router, region, &alias).await;
            assert_eq!(
                metadata["Description"],
                json!(expected),
                "case {service:?} at {region}: expected Description {expected:?}, \
                 got {metadata}"
            );
        }
    }
}

/// A key the CALLER creates keeps the description the caller gave it — the default-key
/// wording is seeded only for the keys Amazon Web Services manages.
#[tokio::test]
async fn a_customer_managed_key_keeps_the_description_the_caller_gave_it() {
    let router = router();

    // Rows: what `CreateKey` was given for `Description`, spanning the modelled
    // `DescriptionType` (`len 0..8192`): omitted, the empty string, a word, and the
    // longest description the length allows.
    let longest = "d".repeat(8192);
    let rows: Vec<(&str, serde_json::Value, String)> = vec![
        ("omitted", json!({}), String::new()),
        ("empty", json!({"Description": ""}), String::new()),
        (
            "a word",
            json!({"Description": "ledger"}),
            "ledger".to_string(),
        ),
        (
            "longest the length allows",
            json!({"Description": longest.clone()}),
            longest.clone(),
        ),
    ];

    for (case, request, expected) in rows {
        let created = create_key(&router, &request).await;
        assert_eq!(
            created["Description"],
            json!(expected),
            "case {case:?}: expected Description {expected:?}, got {created}"
        );
        let read_back = describe(&router, &text(case, &created, "KeyId")).await;
        assert_eq!(
            read_back["Description"],
            json!(expected),
            "case {case:?}: DescribeKey must report the caller's description \
             {expected:?}, got {read_back}"
        );
    }
}
