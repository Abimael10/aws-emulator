// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `PutTableBucketEncryption`.
//!
//! RULE: a put of a table bucket's encryption configuration makes that
//! configuration the bucket's own — the algorithm it names, together with the
//! KMS key exactly when the algorithm takes one ("required only when
//! `sseAlgorithm` is set to `aws:kms`",
//! `smithy_query(query="doc", shape="EncryptionConfiguration",
//! member="kmsKeyArn")`) — so every later read of that bucket's encryption
//! reports exactly it, and every table created in the bucket after the put is
//! created under it ("the default encryption settings that will be applied to
//! all tables created in this bucket unless overridden at the table level",
//! `smithy_query(query="doc", shape="CreateTableBucketRequest",
//! member="encryptionConfiguration")`), until another put replaces it. The
//! operation is `@idempotent`, so a repeated put of the same configuration
//! reports the same thing; a put naming a pairing this service cannot store is
//! the modeled `BadRequestException` and a put against a bucket the store does
//! not hold is the modeled `NotFoundException`, each leaving the bucket's
//! configuration exactly as it was.
//!
//! The table below spans what the rule generalises over: which algorithm the
//! put names (both modeled values, checked against the model's own value list),
//! whether it carries a KMS key and which one, what the bucket carried before
//! the put (nothing, S3-managed keys, another KMS key), how many puts are
//! applied in a row, and the region the bucket lives in.
//!
//! Every KMS key a row names is KMS's OWN, created through its `CreateKey` over
//! the same engine in the region the row works in, because a configuration
//! naming `aws:kms` is only storable when KMS resolves the key it names
//! (`table_bucket_encryption_key_access.rs` is the suite of that rule). A row
//! therefore names a key SLOT and the ARN is resolved from the keys this run
//! created.

use serde_json::{json, Map, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::ops::types::S_S_E_ALGORITHM_VALUES;
use s3tables::state::{ComAmazonawsS3tablesS3tablebucketsState as S3TablesState, SseAlgorithm};

use super::harness::{expected_bucket_arn, Emulator};
// The one reading of a reported `encryptionConfiguration`, shared with the
// `GetTableBucketEncryption` suite rather than copied.
use super::table_bucket_encryption::reported;

/// The KMS keys this suite's rows name: two in the region most rows work in and
/// two in the other one, so a row can move a bucket from one key to another
/// without leaving the region that holds them.
///
/// Shared with the suites that read a bucket's encryption beside its other reads,
/// so the key fixture is declared once for this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    First,
    Second,
    West,
    WestSecond,
}

/// The ARNs KMS minted for those keys in this run.
pub(crate) struct Keys {
    first: String,
    second: String,
    west: String,
    west_second: String,
}

impl Keys {
    /// The Region [`Key::First`] and [`Key::Second`] are created in.
    pub(crate) const REGION: &'static str = "us-east-1";

    /// The Region [`Key::West`] and [`Key::WestSecond`] are created in.
    pub(crate) const OTHER_REGION: &'static str = "eu-west-1";

    /// Create every key, each in the region the rows that name it work in.
    pub(crate) async fn create(emulator: &Emulator) -> Self {
        Self {
            first: emulator.create_kms_key(Self::REGION).await,
            second: emulator.create_kms_key(Self::REGION).await,
            west: emulator.create_kms_key(Self::OTHER_REGION).await,
            west_second: emulator.create_kms_key(Self::OTHER_REGION).await,
        }
    }

    /// The ARN of one slot.
    pub(crate) fn arn(&self, key: Key) -> &str {
        match key {
            Key::First => &self.first,
            Key::Second => &self.second,
            Key::West => &self.west,
            Key::WestSecond => &self.west_second,
        }
    }
}

/// The namespace the inheritance case creates its tables in.
const NAMESPACE: &str = "put_encryption_ns";

/// An `encryptionConfiguration`, as `(sseAlgorithm, the key slot)`.
pub(crate) type Configuration = (&'static str, Option<Key>);

/// A configuration as a row ASSERTS it: the algorithm, and the key ARN resolved
/// from the slot the row named.
type SettledConfiguration = (&'static str, Option<String>);

/// One row: a table bucket created the way the row says, then the puts the row
/// applies in order, and the configuration a read must report afterwards.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// The bucket create's `encryptionConfiguration`, or `None` for none.
    created_with: Option<Configuration>,
    /// The configurations put on the bucket, in order; the last one is the one
    /// a read must report.
    puts: &'static [Configuration],
    expected_algorithm: &'static str,
    expected_kms_key: Option<Key>,
}

/// The body member a put (or a create) carries for `configuration`.
pub(crate) fn configuration_member(configuration: Configuration, keys: &Keys) -> Value {
    let (algorithm, key) = configuration;
    let mut members = Map::new();
    members.insert("sseAlgorithm".to_string(), json!(algorithm));
    if let Some(key) = key {
        members.insert("kmsKeyArn".to_string(), json!(keys.arn(key)));
    }
    Value::Object(members)
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a bucket that named no configuration takes S3-managed keys",
            region: "us-east-1",
            bucket: "put-encryption-fresh-aes",
            created_with: None,
            puts: &[("AES256", None)],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        Case {
            label: "a bucket that named no configuration takes KMS-managed keys with a key",
            region: "us-east-1",
            bucket: "put-encryption-fresh-kms",
            created_with: None,
            puts: &[("aws:kms", Some(Key::First))],
            expected_algorithm: "aws:kms",
            expected_kms_key: Some(Key::First),
        },
        Case {
            label: "the put replaces the configuration the create named",
            region: "us-east-1",
            bucket: "put-encryption-replaces-create",
            created_with: Some(("AES256", None)),
            puts: &[("aws:kms", Some(Key::Second))],
            expected_algorithm: "aws:kms",
            expected_kms_key: Some(Key::Second),
        },
        Case {
            label: "a second put moves the bucket to another key",
            region: "us-east-1",
            bucket: "put-encryption-another-key",
            created_with: Some(("aws:kms", Some(Key::First))),
            puts: &[
                ("aws:kms", Some(Key::Second)),
                ("aws:kms", Some(Key::First)),
            ],
            expected_algorithm: "aws:kms",
            expected_kms_key: Some(Key::First),
        },
        Case {
            label: "a put back to S3-managed keys drops the key the bucket carried",
            region: "us-east-1",
            bucket: "put-encryption-back-to-aes",
            created_with: Some(("aws:kms", Some(Key::First))),
            puts: &[("AES256", None)],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        Case {
            label: "the same configuration put twice: the operation is idempotent",
            region: "us-east-1",
            bucket: "put-encryption-idempotent",
            created_with: None,
            puts: &[
                ("aws:kms", Some(Key::First)),
                ("aws:kms", Some(Key::First)),
            ],
            expected_algorithm: "aws:kms",
            expected_kms_key: Some(Key::First),
        },
        Case {
            label: "a bucket in another region, put a key of that region",
            region: "eu-west-1",
            bucket: "put-encryption-west",
            created_with: None,
            puts: &[("aws:kms", Some(Key::West))],
            expected_algorithm: "aws:kms",
            expected_kms_key: Some(Key::West),
        },
        Case {
            label: "three puts in a row, ending on S3-managed keys",
            region: "eu-west-1",
            bucket: "put-encryption-west-sequence",
            created_with: Some(("AES256", None)),
            puts: &[
                ("aws:kms", Some(Key::West)),
                ("aws:kms", Some(Key::WestSecond)),
                ("AES256", None),
            ],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
    ]
}

/// The configuration the store holds for `arn`, as `(sseAlgorithm, kmsKeyArn)`,
/// or `None` when the bucket carries no explicit configuration of its own.
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

/// Create `case`'s table bucket and return its ARN.
async fn create_bucket(emulator: &Emulator, case: &Case, keys: &Keys) -> String {
    let mut request = Map::new();
    request.insert("name".to_string(), json!(case.bucket));
    if let Some(configuration) = case.created_with {
        request.insert(
            "encryptionConfiguration".to_string(),
            configuration_member(configuration, keys),
        );
    }
    let created = emulator
        .create_table_bucket(case.region, &Value::Object(request))
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
    arn
}

#[tokio::test]
async fn a_put_makes_its_configuration_the_one_every_later_read_reports() {
    let emulator = Emulator::new();
    let keys = Keys::create(&emulator).await;

    for case in cases() {
        let arn = create_bucket(&emulator, &case, &keys).await;
        let expected_key = case.expected_kms_key.map(|key| keys.arn(key).to_string());

        for (ordinal, configuration) in case.puts.iter().enumerate() {
            let put = emulator
                .put_table_bucket_encryption(
                    case.region,
                    &arn,
                    &configuration_member(*configuration, &keys),
                )
                .await;
            assert_eq!(
                put.status,
                200,
                "case {label:?}: put #{ordinal} ({configuration:?}) must succeed, got \
                 {status} {body}",
                label = case.label,
                status = put.status,
                body = put.body
            );
            assert_eq!(
                put.error_code, None,
                "case {label:?}: put #{ordinal} must carry no error code, got {actual:?}",
                label = case.label,
                actual = put.error_code
            );

            // Every put is reported in full by the read, so a sequence of puts
            // is not just its last step landing.
            let read = emulator.get_table_bucket_encryption(case.region, &arn).await;
            let (algorithm, key) = reported(&read.body, case.label);
            let (expected_algorithm, expected_slot) = *configuration;
            let expected_put_key = expected_slot.map(|slot| keys.arn(slot));
            assert_eq!(
                (algorithm.as_str(), key.as_deref()),
                (expected_algorithm, expected_put_key),
                "case {label:?}: after put #{ordinal} expected \
                 {expected_algorithm:?}/{expected_put_key:?}, got {algorithm:?}/{key:?}",
                label = case.label
            );
        }

        let read = emulator.get_table_bucket_encryption(case.region, &arn).await;
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
            expected_key.as_deref(),
            "case {label:?}: expected kmsKeyArn {expected_key:?}, got {key:?}",
            label = case.label
        );

        // The put persisted an EXPLICIT configuration — the record a read
        // reports from, not a default reconstructed per request — and the key
        // is carried only by the algorithm that takes one.
        let held = persisted(&emulator, &arn, case.label);
        assert_eq!(
            held,
            Some((case.expected_algorithm.to_string(), expected_key.clone())),
            "case {label:?}: the store must hold the configuration the put named, got {held:?}",
            label = case.label
        );

        // Two reads answer the same, so nothing the put wrote depends on being
        // read once.
        let again = emulator.get_table_bucket_encryption(case.region, &arn).await;
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
    // model fails this until the table above puts it and reports it.
    let rows = cases();
    for modeled in S_S_E_ALGORITHM_VALUES {
        assert!(
            rows.iter().any(|case| case.expected_algorithm == *modeled),
            "case {modeled:?}: no row reports this modeled algorithm after its puts"
        );
        assert!(
            rows.iter().any(|case| case
                .puts
                .iter()
                .any(|(algorithm, _)| *algorithm == *modeled)),
            "case {modeled:?}: no row puts this modeled algorithm"
        );
        assert!(
            SseAlgorithm::from_wire(modeled).is_some(),
            "case {modeled:?}: the stored form must parse every modeled algorithm"
        );
    }
}

#[tokio::test]
async fn a_put_is_the_default_for_the_tables_created_after_it() {
    let emulator = Emulator::new();
    let keys = Keys::create(&emulator).await;
    let region = "us-east-1";

    let arn = create_bucket(
        &emulator,
        &Case {
            label: "the inheritance fixture",
            region,
            bucket: "put-encryption-inherited",
            created_with: None,
            puts: &[],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        &keys,
    )
    .await;
    let namespace = emulator
        .create_namespace(region, &arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    assert_eq!(
        namespace.status, 200,
        "case \"the inheritance fixture\": the namespace create must succeed, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );

    // (case, the configuration put before the table is created, the table's name)
    let rows: &[(&str, Option<Configuration>, &str)] = &[
        ("before any put: the service default", None, "before_any_put"),
        (
            "after a put of KMS-managed keys",
            Some(("aws:kms", Some(Key::First))),
            "after_kms_put",
        ),
        (
            "after a put moving the bucket to another key",
            Some(("aws:kms", Some(Key::Second))),
            "after_second_key_put",
        ),
        (
            "after a put back to S3-managed keys",
            Some(("AES256", None)),
            "after_aes_put",
        ),
    ];

    // Each row's table, and the configuration it must keep for the rest of the
    // suite: a put is the default for the tables created AFTER it and
    // re-encrypts none that already exists.
    let mut settled: Vec<(&str, &str, SettledConfiguration)> = Vec::new();
    for (case, put, table) in rows {
        if let Some(configuration) = *put {
            let applied = emulator
                .put_table_bucket_encryption(
                    region,
                    &arn,
                    &configuration_member(configuration, &keys),
                )
                .await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: the put must succeed, got {status} {body}",
                status = applied.status,
                body = applied.body
            );
        }
        let created = emulator
            .create_table(
                region,
                &arn,
                NAMESPACE,
                &json!({ "name": table, "format": "ICEBERG" }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the table create must succeed, got {status} {body}",
            status = created.status,
            body = created.body
        );
        let (expected_algorithm, expected_slot) = put.unwrap_or(("AES256", None));
        let expected: SettledConfiguration = (
            expected_algorithm,
            expected_slot.map(|slot| keys.arn(slot).to_string()),
        );
        settled.push((case, table, expected.clone()));

        let read = emulator
            .get_table_encryption(region, &arn, NAMESPACE, table)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the table encryption read must succeed, got {status} {body}",
            status = read.status,
            body = read.body
        );
        let (algorithm, key) = reported(&read.body, case);
        assert_eq!(
            (algorithm.as_str(), key.as_deref()),
            (expected.0, expected.1.as_deref()),
            "case {case:?}: a table created under the bucket's configuration must report \
             {expected:?}, got {algorithm:?}/{key:?}"
        );
    }

    // Every table still reports what was settled for it, so a later put moved
    // none of them.
    for (case, table, expected) in &settled {
        let read = emulator
            .get_table_encryption(region, &arn, NAMESPACE, table)
            .await;
        let (algorithm, key) = reported(&read.body, case);
        assert_eq!(
            (algorithm.as_str(), key.as_deref()),
            (expected.0, expected.1.as_deref()),
            "case {case:?}: a later put must leave this table at {expected:?}, got \
             {algorithm:?}/{key:?}"
        );
    }
}

#[tokio::test]
async fn a_put_against_a_bucket_the_store_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    let keys = Keys::create(&emulator).await;

    // One resident bucket carrying an explicit configuration, so the refusals
    // below are about the ARN each names and can be shown to write nothing.
    let resident = create_bucket(
        &emulator,
        &Case {
            label: "the resident bucket",
            region: "us-east-1",
            bucket: "put-encryption-resident",
            created_with: None,
            puts: &[],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        &keys,
    )
    .await;
    let applied = emulator
        .put_table_bucket_encryption(
            "us-east-1",
            &resident,
            &configuration_member(("aws:kms", Some(Key::First)), &keys),
        )
        .await;
    assert_eq!(
        applied.status, 200,
        "case \"the resident bucket\": the put must succeed, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    // A bucket that was created and then deleted: the put must stop answering
    // for it.
    let doomed = create_bucket(
        &emulator,
        &Case {
            label: "a deleted bucket",
            region: "us-east-1",
            bucket: "put-encryption-deleted",
            created_with: None,
            puts: &[],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        &keys,
    )
    .await;
    let deleted = emulator.delete_table_bucket("us-east-1", &doomed).await;
    assert_eq!(
        deleted.status, 204,
        "case \"a deleted bucket\": the delete must succeed, got {status} {body}",
        status = deleted.status,
        body = deleted.body
    );

    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a bucket that was never created",
            expected_bucket_arn("us-east-1", "put-encryption-never-created"),
            "us-east-1",
        ),
        ("a bucket that was deleted", doomed, "us-east-1"),
        (
            "the resident bucket's name in another region",
            expected_bucket_arn("eu-west-1", "put-encryption-resident"),
            "eu-west-1",
        ),
        (
            "another account's bucket of the same name",
            resident.replace("000000000000", "123456789012"),
            "us-east-1",
        ),
    ];

    for (case, arn, region) in &rows {
        let put = emulator
            .put_table_bucket_encryption(
                region,
                arn,
                &configuration_member(("AES256", None), &keys),
            )
            .await;
        assert_eq!(
            put.status, 404,
            "case {case:?}: expected 404, got {status} {body}",
            status = put.status,
            body = put.body
        );
        assert_eq!(
            put.error_code.as_deref(),
            Some("NotFoundException"),
            "case {case:?}: expected NotFoundException, got {actual:?}",
            actual = put.error_code
        );
        assert_eq!(
            put.message(),
            "The specified bucket does not exist.",
            "case {case:?}: expected the service's fixed not-found sentence, got {actual:?}",
            actual = put.message()
        );
    }

    // The resident bucket is untouched by any of those refusals.
    let held = persisted(&emulator, &resident, "the resident bucket");
    assert_eq!(
        held,
        Some(("aws:kms".to_string(), Some(keys.first.clone()))),
        "case \"the resident bucket\": a refused put must change nothing, got {held:?}"
    );
}

#[tokio::test]
async fn a_configuration_this_service_cannot_store_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let keys = Keys::create(&emulator).await;
    let region = "us-east-1";

    let arn = create_bucket(
        &emulator,
        &Case {
            label: "the refusal fixture",
            region,
            bucket: "put-encryption-refusals",
            created_with: None,
            puts: &[],
            expected_algorithm: "AES256",
            expected_kms_key: None,
        },
        &keys,
    )
    .await;
    // The bucket carries an explicit configuration before the refusals, so a
    // refused put is shown to leave the PREVIOUS one rather than an absence.
    let applied = emulator
        .put_table_bucket_encryption(
            region,
            &arn,
            &configuration_member(("aws:kms", Some(Key::Second)), &keys),
        )
        .await;
    assert_eq!(
        applied.status, 200,
        "case \"the refusal fixture\": the put must succeed, got {status} {body}",
        status = applied.status,
        body = applied.body
    );

    // Each row is a pairing of `sseAlgorithm` and `kmsKeyArn` the model's own
    // documentation rules out — a key is carried by `aws:kms` and by nothing
    // else — with both members otherwise satisfying their modeled constraints.
    let rows: &[(&str, Value)] = &[
        (
            "KMS-managed keys with no key",
            json!({ "sseAlgorithm": "aws:kms" }),
        ),
        (
            "S3-managed keys carrying a key",
            json!({ "sseAlgorithm": "AES256", "kmsKeyArn": keys.first }),
        ),
        (
            "S3-managed keys carrying a key of another region",
            json!({ "sseAlgorithm": "AES256", "kmsKeyArn": keys.west }),
        ),
    ];

    for (case, configuration) in rows {
        let put = emulator
            .put_table_bucket_encryption(region, &arn, configuration)
            .await;
        assert_eq!(
            put.status, 400,
            "case {case:?}: expected 400, got {status} {body}",
            status = put.status,
            body = put.body
        );
        assert_eq!(
            put.error_code.as_deref(),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {actual:?}",
            actual = put.error_code
        );

        // The refusal is effect-free: the configuration the last accepted put
        // named is still the bucket's, and the read still reports it.
        let held = persisted(&emulator, &arn, case);
        assert_eq!(
            held,
            Some(("aws:kms".to_string(), Some(keys.second.clone()))),
            "case {case:?}: a refused put must change nothing, got {held:?}"
        );
        let read = emulator.get_table_bucket_encryption(region, &arn).await;
        let (algorithm, key) = reported(&read.body, case);
        assert_eq!(
            (algorithm.as_str(), key.as_deref()),
            ("aws:kms", Some(keys.second.as_str())),
            "case {case:?}: the read must still report the accepted configuration, got \
             {algorithm:?}/{key:?}"
        );
    }
}
