// SPDX-License-Identifier: CC0-1.0

//! What `DeleteTableBucketEncryption` REACHES, and what it must leave alone.
//!
//! RULE: the delete of a table bucket's encryption configuration removes exactly
//! the configuration the ADDRESSED bucket holds of its own, which reverts that
//! bucket — and only that bucket — to the service default a read reports for a
//! bucket holding none (S3-managed keys, no KMS key). Nothing else moves: every
//! other bucket keeps reporting the configuration it was given, every other
//! sub-resource of the addressed bucket (its record, its storage class, its
//! maintenance, metrics and policy configurations, its tags, its namespaces and
//! its tables) reads exactly as it did, and a table that already exists keeps the
//! encryption its create settled for it, because a bucket's configuration is "the
//! default encryption settings that will be applied to all tables created in this
//! bucket unless overridden at the table level"
//! (`smithy_query(query="doc", shape="CreateTableBucketRequest",
//! member="encryptionConfiguration")`) — so removing it is the default for the
//! tables created afterwards and re-encrypts none that is already there.
//!
//! `table_bucket_encryption_delete.rs` pins the delete's OUTCOME on one bucket at
//! a time (served 204, reverted read, the modelled not-found for a bucket holding
//! no configuration of its own or for an ARN the store holds no bucket at). This
//! suite pins its REACH, which that round trip cannot observe: the rows below span
//! the buckets a store holds beside the addressed one (different Regions, both
//! modelled algorithms, keys that differ per bucket), the other sub-resources of
//! the addressed bucket, and the tables of a bucket that is still in active use
//! when the delete arrives.
//!
//! Every KMS key a configuration names is KMS's OWN, minted through its
//! `CreateKey` over the same engine, because a configuration naming `aws:kms` is
//! only storable when KMS resolves the key it names.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::harness::{listed_tags, served, Emulator};
use super::table_bucket_encryption::reported;

/// The configuration a bucket holding none of its own is protected by: S3-managed
/// keys, no key — what a read reports for a bucket nothing was ever set on, and
/// for one a delete reverted.
const SERVICE_DEFAULT: (&str, Option<&str>) = ("AES256", None);

/// The namespace the tables of this suite are created in (`^[0-9a-z_]*$`).
const NAMESPACE: &str = "delete_reach_ns";

/// A reported `encryptionConfiguration`, as `(sseAlgorithm, kmsKeyArn)` — the
/// form [`reported`] hands back and the form a row states what it expects in.
type Reported = (String, Option<String>);

/// An `encryptionConfiguration` member naming `algorithm`, with `key` when the
/// case carries one.
fn configuration(algorithm: &str, key: Option<&str>) -> Value {
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(key));
    }
    Value::Object(members)
}

/// A `CreateTable` request body for `name`, with `encryptionConfiguration` when
/// the case names one.
fn create_table_request(name: &str, encryption: Option<Value>) -> Value {
    let mut members = Map::new();
    members.insert("name".to_string(), json!(name));
    members.insert("format".to_string(), json!("ICEBERG"));
    if let Some(encryption) = encryption {
        members.insert("encryptionConfiguration".to_string(), encryption);
    }
    Value::Object(members)
}

/// The configuration `GetTableBucketEncryption` reports for `arn`.
async fn bucket_reports(emulator: &Emulator, region: &str, arn: &str, case: &str) -> Reported {
    let read = served(
        emulator.get_table_bucket_encryption(region, arn).await,
        "GetTableBucketEncryption",
        case,
    );
    reported(&read.body, case)
}

/// The configuration `GetTableEncryption` reports for one table.
async fn table_reports(
    emulator: &Emulator,
    region: &str,
    bucket_arn: &str,
    table: &str,
    case: &str,
) -> Reported {
    let read = served(
        emulator
            .get_table_encryption(region, bucket_arn, NAMESPACE, table)
            .await,
        "GetTableEncryption",
        case,
    );
    reported(&read.body, case)
}

/// Assert `reply` is the modelled 204 with no body.
fn assert_empty_204(reply: &super::harness::Reply, case: &str) {
    assert_eq!(
        reply.status, 204,
        "case {case:?}: expected the modelled 204, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.body,
        Value::Null,
        "case {case:?}: expected an empty body, got {body}",
        body = reply.body
    );
}

/// One bucket the reach table holds at once: the Region it lives in, the name it
/// carries and the configuration it is given before any delete runs.
struct Bucket {
    label: &'static str,
    region: &'static str,
    name: &'static str,
    algorithm: &'static str,
    /// Whether the configuration carries a KMS key, which only `aws:kms` takes.
    carries_key: bool,
    /// Whether the configuration is given by the create or by a later put, so a
    /// row's reach is asserted however the bucket came to hold one.
    by_put: bool,
}

/// One of the reach table's buckets as it stands in the store: the row it came
/// from, the ARN its create minted and what a read of it must report until its
/// own delete runs.
struct Configured<'a> {
    row: &'a Bucket,
    arn: String,
    expected: Reported,
}

/// The rows: both modelled algorithms, with and without a key, written by a
/// create and by a put, over four Regions, with one name repeated across two
/// Regions so an ARN cannot be confused for a name.
fn buckets() -> Vec<Bucket> {
    vec![
        Bucket {
            label: "S3-managed keys, named at create",
            region: "us-east-1",
            name: "delete-reach-east",
            algorithm: "AES256",
            carries_key: false,
            by_put: false,
        },
        Bucket {
            label: "KMS-managed keys with a key of its own, put after the create",
            region: "us-east-1",
            name: "delete-reach-east-kms",
            algorithm: "aws:kms",
            carries_key: true,
            by_put: true,
        },
        Bucket {
            label: "KMS-managed keys in another region",
            region: "eu-west-1",
            name: "delete-reach-west",
            algorithm: "aws:kms",
            carries_key: true,
            by_put: false,
        },
        Bucket {
            label: "the first row's name, in another region",
            region: "ap-south-1",
            name: "delete-reach-east",
            algorithm: "AES256",
            carries_key: false,
            by_put: true,
        },
    ]
}

#[tokio::test]
async fn a_delete_reverts_only_the_addressed_bucket() {
    let emulator = Emulator::new();

    // Every row's bucket is created and configured FIRST, so each delete below
    // runs against a store holding several configurations at once.
    let rows = buckets();
    let mut resident: Vec<Configured<'_>> = Vec::new();
    for row in &rows {
        let key = emulator.create_kms_key(row.region).await;
        let member = configuration(row.algorithm, row.carries_key.then_some(key.as_str()));

        let mut request = Map::new();
        request.insert("name".to_string(), json!(row.name));
        if !row.by_put {
            request.insert("encryptionConfiguration".to_string(), member.clone());
        }
        let created = served(
            emulator
                .create_table_bucket(row.region, &Value::Object(request))
                .await,
            "CreateTableBucket",
            row.label,
        );
        let arn = created.text("arn");
        if row.by_put {
            served(
                emulator
                    .put_table_bucket_encryption(row.region, &arn, &member)
                    .await,
                "PutTableBucketEncryption",
                row.label,
            );
        }

        // What this bucket reports before any delete: the configuration it was
        // given, which is what every row whose turn has not come must keep
        // reporting.
        let expected = (
            row.algorithm.to_string(),
            row.carries_key.then(|| key.clone()),
        );
        let actual = bucket_reports(&emulator, row.region, &arn, row.label).await;
        assert_eq!(
            actual, expected,
            "case {label:?}: the configured bucket must report {expected:?}, got {actual:?}",
            label = row.label
        );
        resident.push(Configured { row, arn, expected });
    }

    while !resident.is_empty() {
        let Configured { row, arn, .. } = resident.remove(0);

        let deleted = emulator
            .delete_table_bucket_encryption(row.region, &arn)
            .await;
        assert_empty_204(&deleted, row.label);

        // The addressed bucket is back to the service default.
        let actual = bucket_reports(&emulator, row.region, &arn, row.label).await;
        assert_eq!(
            (actual.0.as_str(), actual.1.as_deref()),
            SERVICE_DEFAULT,
            "case {label:?}: expected the service default after the delete, got {actual:?}",
            label = row.label
        );

        // And nothing else moved: every bucket whose turn has not come still
        // reports exactly the configuration its own write persisted.
        for other in &resident {
            let still =
                bucket_reports(&emulator, other.row.region, &other.arn, other.row.label).await;
            assert_eq!(
                still, other.expected,
                "case {label:?}: deleting {arn:?} must leave {other_arn:?} reporting \
                 {expected:?}, got {still:?}",
                label = other.row.label,
                other_arn = other.arn,
                expected = other.expected
            );
        }
    }
}

#[tokio::test]
async fn a_delete_leaves_the_buckets_other_sub_resources_alone() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let case = "the fully configured bucket";
    let key = emulator.create_kms_key(region).await;

    let created = served(
        emulator
            .create_table_bucket(
                region,
                &json!({
                    "name": "delete-reach-neighbours",
                    "encryptionConfiguration": configuration("aws:kms", Some(&key)),
                    "storageClassConfiguration": { "storageClass": "INTELLIGENT_TIERING" },
                    "tags": { "team": "data", "owner": "analytics" }
                }),
            )
            .await,
        "CreateTableBucket",
        case,
    );
    let arn = created.text("arn");

    served(
        emulator
            .create_namespace(region, &arn, &json!({ "namespace": [NAMESPACE] }))
            .await,
        "CreateNamespace",
        case,
    );
    served(
        emulator
            .create_table(region, &arn, NAMESPACE, &create_table_request("neighbour", None))
            .await,
        "CreateTable",
        case,
    );
    let maintenance = emulator
        .put_table_bucket_maintenance_configuration(
            region,
            &arn,
            "icebergUnreferencedFileRemoval",
            &json!({
                "status": "enabled",
                "settings": {
                    "icebergUnreferencedFileRemoval": {
                        "unreferencedDays": 7,
                        "nonCurrentDays": 14
                    }
                }
            }),
        )
        .await;
    assert_empty_204(&maintenance, "the maintenance put");
    let metrics = emulator
        .put_table_bucket_metrics_configuration(region, &arn)
        .await;
    assert_empty_204(&metrics, "the metrics put");
    served(
        emulator
            .put_table_bucket_policy(
                region,
                &arn,
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": "*",
                        "Action": "s3tables:GetTable",
                        "Resource": "*",
                    }],
                })
                .to_string(),
            )
            .await,
        "PutTableBucketPolicy",
        case,
    );

    // Every neighbouring read of the SAME bucket, taken before and after the
    // encryption delete: the delete reaches none of them.
    let neighbours: &[&str] = &[
        "GetTableBucket",
        "GetTableBucketStorageClass",
        "GetTableBucketMaintenanceConfiguration",
        "GetTableBucketMetricsConfiguration",
        "GetTableBucketPolicy",
        "ListNamespaces",
        "ListTables",
    ];
    async fn read_neighbour(emulator: &Emulator, region: &str, arn: &str, which: &str) -> Value {
        let reply = match which {
            "GetTableBucket" => emulator.get_table_bucket(region, arn).await,
            "GetTableBucketStorageClass" => {
                emulator.get_table_bucket_storage_class(region, arn).await
            }
            "GetTableBucketMaintenanceConfiguration" => {
                emulator
                    .get_table_bucket_maintenance_configuration(region, arn)
                    .await
            }
            "GetTableBucketMetricsConfiguration" => {
                emulator
                    .get_table_bucket_metrics_configuration(region, arn)
                    .await
            }
            "GetTableBucketPolicy" => emulator.get_table_bucket_policy(region, arn).await,
            "ListNamespaces" => emulator.list_namespaces(region, arn, &[]).await,
            "ListTables" => emulator.list_tables(region, arn, &[]).await,
            other => panic!("case {other:?}: the table names no such read"),
        };
        served(reply, which, which).body
    }

    let mut before: Vec<(&str, Value)> = Vec::new();
    for which in neighbours {
        before.push((which, read_neighbour(&emulator, region, &arn, which).await));
    }
    let tags_before = listed_tags(&emulator, region, &arn, case).await;

    let deleted = emulator.delete_table_bucket_encryption(region, &arn).await;
    assert_empty_204(&deleted, "the encryption delete");

    for (which, expected) in &before {
        let actual = read_neighbour(&emulator, region, &arn, which).await;
        assert_eq!(
            &actual, expected,
            "case {which:?}: the encryption delete must leave this read reporting {expected}, \
             got {actual}"
        );
    }
    let tags_after: BTreeMap<String, String> = listed_tags(&emulator, region, &arn, case).await;
    assert_eq!(
        tags_after, tags_before,
        "case {case:?}: the encryption delete must leave the tags {tags_before:?}, got \
         {tags_after:?}"
    );

    // Only the encryption configuration went, and the bucket can be given one
    // again afterwards.
    let actual = bucket_reports(&emulator, region, &arn, case).await;
    assert_eq!(
        (actual.0.as_str(), actual.1.as_deref()),
        SERVICE_DEFAULT,
        "case {case:?}: expected the service default after the delete, got {actual:?}"
    );
    served(
        emulator
            .put_table_bucket_encryption(region, &arn, &configuration("aws:kms", Some(&key)))
            .await,
        "PutTableBucketEncryption",
        "the put after the delete",
    );
    let again = bucket_reports(&emulator, region, &arn, "the put after the delete").await;
    assert_eq!(
        (again.0.as_str(), again.1.as_deref()),
        ("aws:kms", Some(key.as_str())),
        "case \"the put after the delete\": expected the configuration just put, got {again:?}"
    );
}

/// One table that already exists when the delete arrives: how its create named
/// its encryption, and what a read of it must report both before and after.
struct Existing {
    name: &'static str,
    created_with: Option<Value>,
    expected: Reported,
}

#[tokio::test]
async fn a_delete_under_active_tables_changes_none_of_them_and_only_the_default() {
    let emulator = Emulator::new();
    let region = "eu-west-1";
    let case = "a bucket in active use";
    let bucket_key = emulator.create_kms_key(region).await;
    let table_key = emulator.create_kms_key(region).await;

    let created = served(
        emulator
            .create_table_bucket(
                region,
                &json!({
                    "name": "delete-reach-active-tables",
                    "encryptionConfiguration": configuration("aws:kms", Some(&bucket_key))
                }),
            )
            .await,
        "CreateTableBucket",
        case,
    );
    let arn = created.text("arn");
    served(
        emulator
            .create_namespace(region, &arn, &json!({ "namespace": [NAMESPACE] }))
            .await,
        "CreateNamespace",
        case,
    );

    // The tables that exist when the delete arrives, spanning how each came by
    // its encryption: inherited from the bucket's default, overridden with
    // S3-managed keys, and overridden with a key of the table's own.
    let existing: Vec<Existing> = vec![
        Existing {
            name: "inherits_the_default",
            created_with: None,
            expected: ("aws:kms".to_string(), Some(bucket_key.clone())),
        },
        Existing {
            name: "overrides_with_aes",
            created_with: Some(configuration("AES256", None)),
            expected: (SERVICE_DEFAULT.0.to_string(), None),
        },
        Existing {
            name: "overrides_with_its_own_key",
            created_with: Some(configuration("aws:kms", Some(&table_key))),
            expected: ("aws:kms".to_string(), Some(table_key.clone())),
        },
    ];
    for table in &existing {
        served(
            emulator
                .create_table(
                    region,
                    &arn,
                    NAMESPACE,
                    &create_table_request(table.name, table.created_with.clone()),
                )
                .await,
            "CreateTable",
            table.name,
        );
        let actual = table_reports(&emulator, region, &arn, table.name, table.name).await;
        assert_eq!(
            actual, table.expected,
            "case {name:?}: the created table must report {expected:?}, got {actual:?}",
            name = table.name,
            expected = table.expected
        );
    }

    // The bucket holds live namespaces and tables: the delete is still served.
    let deleted = emulator.delete_table_bucket_encryption(region, &arn).await;
    assert_empty_204(&deleted, case);

    // Every table that already existed keeps the encryption its create settled.
    for table in &existing {
        let actual = table_reports(&emulator, region, &arn, table.name, table.name).await;
        assert_eq!(
            actual, table.expected,
            "case {name:?}: the delete must leave this table reporting {expected:?}, got \
             {actual:?}",
            name = table.name,
            expected = table.expected
        );
    }

    // What changed is the DEFAULT: a table created after the delete is encrypted
    // the way a bucket holding no configuration of its own encrypts.
    served(
        emulator
            .create_table(
                region,
                &arn,
                NAMESPACE,
                &create_table_request("created_after_the_delete", None),
            )
            .await,
        "CreateTable",
        "created after the delete",
    );
    let actual = table_reports(
        &emulator,
        region,
        &arn,
        "created_after_the_delete",
        "created after the delete",
    )
    .await;
    assert_eq!(
        (actual.0.as_str(), actual.1.as_deref()),
        SERVICE_DEFAULT,
        "case \"created after the delete\": expected the service default, got {actual:?}"
    );

    // And the bucket's namespaces and tables are all still there.
    let listed = served(
        emulator.list_tables(region, &arn, &[]).await,
        "ListTables",
        case,
    );
    let rows = listed.list("tables");
    assert_eq!(
        rows.len(),
        existing.len() + 1,
        "case {case:?}: expected {expected} tables after the delete, got {actual}: {rows:?}",
        expected = existing.len() + 1,
        actual = rows.len()
    );
}
