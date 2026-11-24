// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `GetTableEncryption`.
//!
//! RULE: every table is encrypted, so a read of one's encryption configuration
//! reports the algorithm — and, for SSE-KMS, the key — the table was CREATED
//! with: the configuration the create named, else the default of the table
//! bucket that holds it at that moment ("the default encryption settings that
//! will be applied to all tables created in this bucket unless overridden at the
//! table level", `smithy_query(query="doc", shape="CreateTableBucketRequest",
//! member="encryptionConfiguration")`), else the service default, which is
//! S3-managed keys and no KMS key. A table a configuration was settled for keeps
//! it, so a bucket default set after the table exists is the default for the
//! tables created after it and changes no table that already exists. A read of a
//! bucket or a table the store does not hold is refused with the modeled
//! not-found, and a read changes nothing a later caller sees.
//!
//! The table below spans what the rule generalises over: which algorithm the
//! BUCKET carries (both modeled values, checked against the model's own value
//! list), which the TABLE carries, whether either named a configuration at all,
//! whether a KMS key is carried and which one, and the region the resources live
//! in.

use serde_json::{json, Map, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::ops::types::S_S_E_ALGORITHM_VALUES;
use s3tables::state::{
    BucketEncryption, ComAmazonawsS3tablesS3tablebucketsState as S3TablesState, SseAlgorithm,
};

use super::harness::{expected_bucket_arn, Emulator};

/// Three KMS key ARNs of the form the modeled `kmsKeyArn` pattern accepts
/// (`^(arn:aws[-a-z0-9]*:kms:[-a-z0-9]*:[0-9]{12}:key/.+)$`), differing in
/// region and key id so a reported key cannot be a constant.
const BUCKET_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";
const TABLE_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210";
const OTHER_KEY_ARN: &str =
    "arn:aws:kms:eu-west-1:000000000000:key/fedcba98-7654-3210-fedc-ba9876543210";

/// The namespace every table in this suite is created in.
const NAMESPACE: &str = "encryption_ns";

/// An `encryptionConfiguration`, as `(sseAlgorithm, kmsKeyArn)`.
type Configuration = (&'static str, Option<&'static str>);

/// One row: a table bucket and a table created the way the row says, and the
/// encryption configuration a read of the TABLE must report.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    table: &'static str,
    /// The bucket create's `encryptionConfiguration`, or `None` for none.
    bucket_created_with: Option<Configuration>,
    /// The table create's `encryptionConfiguration`, or `None` for none.
    table_created_with: Option<Configuration>,
    expected_algorithm: &'static str,
    expected_kms_key_arn: Option<&'static str>,
}

/// The `encryptionConfiguration` member of a create request, when the row names
/// one.
fn configuration_member(configuration: Option<Configuration>) -> Option<Value> {
    let (algorithm, key) = configuration?;
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(key));
    }
    Some(Value::Object(members))
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "neither the bucket nor the table named one: the service default",
            region: "us-east-1",
            bucket: "table-encryption-default",
            table: "plain",
            bucket_created_with: None,
            table_created_with: None,
            expected_algorithm: "AES256",
            expected_kms_key_arn: None,
        },
        Case {
            label: "the bucket named S3-managed keys, the table none",
            region: "us-east-1",
            bucket: "table-encryption-bucket-aes",
            table: "inherits_aes",
            bucket_created_with: Some(("AES256", None)),
            table_created_with: None,
            expected_algorithm: "AES256",
            expected_kms_key_arn: None,
        },
        Case {
            label: "the bucket named KMS-managed keys, the table none: inherited with the key",
            region: "us-east-1",
            bucket: "table-encryption-bucket-kms",
            table: "inherits_kms",
            bucket_created_with: Some(("aws:kms", Some(BUCKET_KEY_ARN))),
            table_created_with: None,
            expected_algorithm: "aws:kms",
            expected_kms_key_arn: Some(BUCKET_KEY_ARN),
        },
        Case {
            label: "the table overrode a KMS bucket default with S3-managed keys",
            region: "us-east-1",
            bucket: "table-encryption-override-aes",
            table: "overrides_with_aes",
            bucket_created_with: Some(("aws:kms", Some(BUCKET_KEY_ARN))),
            table_created_with: Some(("AES256", None)),
            expected_algorithm: "AES256",
            expected_kms_key_arn: None,
        },
        Case {
            label: "the table named a key of its own, the bucket nothing",
            region: "us-east-1",
            bucket: "table-encryption-table-kms",
            table: "own_key",
            bucket_created_with: None,
            table_created_with: Some(("aws:kms", Some(TABLE_KEY_ARN))),
            expected_algorithm: "aws:kms",
            expected_kms_key_arn: Some(TABLE_KEY_ARN),
        },
        Case {
            label: "the table named another key than its bucket, in another region",
            region: "eu-west-1",
            bucket: "table-encryption-two-keys",
            table: "other_key",
            bucket_created_with: Some(("aws:kms", Some(BUCKET_KEY_ARN))),
            table_created_with: Some(("aws:kms", Some(OTHER_KEY_ARN))),
            expected_algorithm: "aws:kms",
            expected_kms_key_arn: Some(OTHER_KEY_ARN),
        },
    ]
}

/// The reported `encryptionConfiguration`, as `(sseAlgorithm, kmsKeyArn)`.
fn reported(body: &Value, case: &str) -> (String, Option<String>) {
    let configuration = body.get("encryptionConfiguration").unwrap_or_else(|| {
        panic!("case {case:?}: the response carries no encryptionConfiguration: {body}")
    });
    let algorithm = configuration
        .get("sseAlgorithm")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the configuration carries no sseAlgorithm: {configuration}")
        })
        .to_string();
    let key = configuration
        .get("kmsKeyArn")
        .and_then(Value::as_str)
        .map(str::to_string);
    (algorithm, key)
}

/// Create the bucket, the namespace and the table of `case`, answering the
/// bucket's ARN.
async fn fixture(emulator: &Emulator, case: &Case) -> String {
    let mut create_bucket = Map::new();
    create_bucket.insert("name".to_string(), json!(case.bucket));
    if let Some(configuration) = configuration_member(case.bucket_created_with) {
        create_bucket.insert("encryptionConfiguration".to_string(), configuration);
    }
    let created = emulator
        .create_table_bucket(case.region, &Value::Object(create_bucket))
        .await;
    assert_eq!(
        created.status,
        200,
        "case {label:?}: the bucket create must succeed, got {status} {body}",
        label = case.label,
        status = created.status,
        body = created.body
    );
    let arn = created.text("arn");
    assert_eq!(
        arn,
        expected_bucket_arn(case.region, case.bucket),
        "case {label:?}: the create must mint the modeled ARN, got {arn:?}",
        label = case.label
    );

    let namespace = emulator
        .create_namespace(case.region, &arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    assert_eq!(
        namespace.status,
        200,
        "case {label:?}: the namespace create must succeed, got {status} {body}",
        label = case.label,
        status = namespace.status,
        body = namespace.body
    );

    let mut create_table = Map::new();
    create_table.insert("name".to_string(), json!(case.table));
    create_table.insert("format".to_string(), json!("ICEBERG"));
    if let Some(configuration) = configuration_member(case.table_created_with) {
        create_table.insert("encryptionConfiguration".to_string(), configuration);
    }
    let table = emulator
        .create_table(
            case.region,
            &arn,
            NAMESPACE,
            &Value::Object(create_table),
        )
        .await;
    assert_eq!(
        table.status,
        200,
        "case {label:?}: the table create must succeed, got {status} {body}",
        label = case.label,
        status = table.status,
        body = table.body
    );
    arn
}

#[tokio::test]
async fn a_read_reports_the_configuration_the_table_was_created_with() {
    let emulator = Emulator::new();

    for case in cases() {
        let arn = fixture(&emulator, &case).await;

        let read = emulator
            .get_table_encryption(case.region, &arn, NAMESPACE, case.table)
            .await;
        assert_eq!(
            read.status,
            200,
            "case {label:?}: the read must succeed, got {status} {body}",
            label = case.label,
            status = read.status,
            body = read.body
        );
        let (algorithm, key) = reported(&read.body, case.label);
        assert_eq!(
            algorithm, case.expected_algorithm,
            "case {label:?}: expected sseAlgorithm {expected:?}, got {algorithm:?}",
            label = case.label,
            expected = case.expected_algorithm
        );
        assert_eq!(
            key.as_deref(),
            case.expected_kms_key_arn,
            "case {label:?}: expected kmsKeyArn {expected:?}, got {key:?}",
            label = case.label,
            expected = case.expected_kms_key_arn
        );

        // A read reports, it does not write: the record the create settled is
        // the one the next caller sees, and a second read answers the same.
        {
            let store = emulator.engine.state::<S3TablesState>();
            let store = store.lock();
            let table = store
                .table_buckets
                .get(&arn)
                .and_then(|bucket| bucket.namespaces.get(NAMESPACE))
                .and_then(|namespace| namespace.tables.get(case.table))
                .unwrap_or_else(|| {
                    panic!("case {label:?}: the table is resident", label = case.label)
                });
            let persisted = table
                .encryption
                .as_ref()
                .map(|encryption| encryption.sse_algorithm.as_wire().to_string());
            assert_eq!(
                persisted.as_deref(),
                Some(case.expected_algorithm),
                "case {label:?}: the read must leave the settled record, got {persisted:?}",
                label = case.label
            );
        }
        let again = emulator
            .get_table_encryption(case.region, &arn, NAMESPACE, case.table)
            .await;
        assert_eq!(
            reported(&again.body, case.label),
            (algorithm, key),
            "case {label:?}: two reads must report the same configuration",
            label = case.label
        );
    }
}

#[tokio::test]
async fn the_table_spans_every_modeled_algorithm() {
    // The catalog is the model's own value list for `SSEAlgorithm`
    // (`s3tables::ops::types::S_S_E_ALGORITHM_VALUES`), so a value added to the
    // model fails this until the table above covers it — on both sides of the
    // rule, the bucket's default and the table's own override.
    for modeled in S_S_E_ALGORITHM_VALUES {
        assert!(
            cases()
                .iter()
                .any(|case| case.expected_algorithm == *modeled),
            "case {modeled:?}: no row reports this modeled algorithm"
        );
        assert!(
            cases().iter().any(|case| case
                .bucket_created_with
                .is_some_and(|(algorithm, _)| algorithm == *modeled)),
            "case {modeled:?}: no row has a bucket default naming this modeled algorithm"
        );
        assert!(
            cases().iter().any(|case| case
                .table_created_with
                .is_some_and(|(algorithm, _)| algorithm == *modeled)),
            "case {modeled:?}: no row has a table override naming this modeled algorithm"
        );
    }
}

#[tokio::test]
async fn a_bucket_default_changed_later_applies_to_the_tables_created_after_it() {
    let emulator = Emulator::new();
    let region = "us-west-2";

    let arn = emulator
        .create_table_bucket(region, &json!({ "name": "table-encryption-timeline" }))
        .await
        .text("arn");
    emulator
        .create_namespace(region, &arn, &json!({ "namespace": [NAMESPACE] }))
        .await;

    // A table created while the bucket carried no configuration of its own: the
    // service default was settled for it.
    let created_before = emulator
        .create_table(
            region,
            &arn,
            NAMESPACE,
            &json!({ "name": "before", "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        created_before.status, 200,
        "case \"before\": the table create must succeed, got {status} {body}",
        status = created_before.status,
        body = created_before.body
    );

    // The bucket's default changes AFTER that table exists. The operation that
    // changes it over the wire, `PutTableBucketEncryption`, is still a scaffold
    // stub with an issue of its own, so the new default is set where that put
    // would persist it — on the bucket's record — and read back through the
    // bucket's own implemented reader below.
    {
        let store = emulator.engine.state::<S3TablesState>();
        let mut store = store.lock();
        let bucket = store
            .table_buckets
            .get_mut(&arn)
            .unwrap_or_else(|| panic!("the bucket {arn:?} is resident"));
        bucket.encryption = Some(BucketEncryption {
            sse_algorithm: SseAlgorithm::AwsKms,
            kms_key_arn: Some(BUCKET_KEY_ARN.to_string()),
        });
    }
    let bucket = emulator.get_table_bucket_encryption(region, &arn).await;
    assert_eq!(
        reported(&bucket.body, "the bucket default"),
        ("aws:kms".to_string(), Some(BUCKET_KEY_ARN.to_string())),
        "case \"the bucket default\": the bucket must read back the new default, got {body}",
        body = bucket.body
    );

    let created_after = emulator
        .create_table(
            region,
            &arn,
            NAMESPACE,
            &json!({ "name": "after", "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        created_after.status, 200,
        "case \"after\": the table create must succeed, got {status} {body}",
        status = created_after.status,
        body = created_after.body
    );

    // (case, table, expected algorithm, expected key)
    let rows: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a table created before the bucket default was set",
            "before",
            "AES256",
            None,
        ),
        (
            "a table created after the bucket default was set",
            "after",
            "aws:kms",
            Some(BUCKET_KEY_ARN),
        ),
    ];
    for (case, table, algorithm, key) in rows {
        let read = emulator
            .get_table_encryption(region, &arn, NAMESPACE, table)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the read must succeed, got {status} {body}",
            status = read.status,
            body = read.body
        );
        let actual = reported(&read.body, case);
        assert_eq!(
            (actual.0.as_str(), actual.1.as_deref()),
            (*algorithm, *key),
            "case {case:?}: expected {algorithm:?} / {key:?}, got {actual:?}"
        );
    }
}

#[tokio::test]
async fn a_read_of_a_table_the_store_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    let region = "us-east-1";

    let arn = emulator
        .create_table_bucket(region, &json!({ "name": "table-encryption-refusals" }))
        .await
        .text("arn");
    emulator
        .create_namespace(region, &arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    let resident = emulator
        .create_table(
            region,
            &arn,
            NAMESPACE,
            &json!({ "name": "resident", "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        resident.status, 200,
        "case \"the resident table\": the create must succeed, got {status} {body}",
        status = resident.status,
        body = resident.body
    );

    // A table that was created and then deleted: the read must stop answering
    // for it.
    emulator
        .create_table(
            region,
            &arn,
            NAMESPACE,
            &json!({ "name": "doomed", "format": "ICEBERG" }),
        )
        .await;
    let deleted = emulator
        .delete_table(region, &arn, NAMESPACE, "doomed", &[])
        .await;
    assert_eq!(
        deleted.status, 204,
        "case \"a deleted table\": the delete must succeed, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );

    // The two fixed sentences this service states each class with, restated
    // here rather than read out of the crate.
    let no_bucket = "The specified bucket does not exist.";
    let no_table = "The specified table does not exist.";
    // (case, the Region the request is made in, bucket ARN, namespace, table,
    // expected sentence)
    let rows: Vec<(&str, &str, String, &str, &str, &str)> = vec![
        (
            "a bucket that was never created",
            region,
            expected_bucket_arn(region, "table-encryption-absent"),
            NAMESPACE,
            "resident",
            no_bucket,
        ),
        // The same name in another Region is a bucket of that Region, so the row
        // is read for that Region: an ARN of a Region other than the endpoint's
        // is an addressing mistake rather than a resource that is not there,
        // which `region_addressing.rs` is where that class is covered.
        (
            "the resident bucket's name in another region, read in that region",
            "eu-west-1",
            expected_bucket_arn("eu-west-1", "table-encryption-refusals"),
            NAMESPACE,
            "resident",
            no_bucket,
        ),
        (
            "another account's bucket of the same name",
            region,
            arn.replace("000000000000", "123456789012"),
            NAMESPACE,
            "resident",
            no_bucket,
        ),
        (
            "a namespace the bucket does not hold",
            region,
            arn.clone(),
            "absent_ns",
            "resident",
            no_table,
        ),
        (
            "a table the namespace does not hold",
            region,
            arn.clone(),
            NAMESPACE,
            "absent",
            no_table,
        ),
        (
            "a table that was deleted",
            region,
            arn.clone(),
            NAMESPACE,
            "doomed",
            no_table,
        ),
    ];

    for (case, case_region, bucket_arn, namespace, table, sentence) in &rows {
        let read = emulator
            .get_table_encryption(case_region, bucket_arn, namespace, table)
            .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected 404, got {status} {body}",
            status = read.status,
            body = read.body
        );
        assert_eq!(
            read.error_code.as_deref(),
            Some("NotFoundException"),
            "case {case:?}: expected NotFoundException, got {actual:?}",
            actual = read.error_code
        );
        assert_eq!(
            read.message(),
            *sentence,
            "case {case:?}: expected {sentence:?}, got {actual:?}",
            actual = read.message()
        );
        assert!(
            read.body.get("encryptionConfiguration").is_none(),
            "case {case:?}: a refusal must report no configuration, got {body}",
            body = read.body
        );
    }

    // The resident table still reads, so the refusals above are about what each
    // row named.
    let read = emulator
        .get_table_encryption(region, &arn, NAMESPACE, "resident")
        .await;
    assert_eq!(
        read.status, 200,
        "case \"the resident table\": the read must still succeed, got {status} {body}",
        status = read.status,
        body = read.body
    );
}
