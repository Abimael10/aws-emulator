// SPDX-License-Identifier: CC0-1.0

//! The two reads of a table bucket a client performs TOGETHER — what the bucket is
//! encrypted with, and what tables it holds — over the bucket's whole encryption
//! trajectory.
//!
//! RULE 1 — a read of a table bucket's encryption reports the configuration IN FORCE for
//! that bucket: the algorithm the bucket's create or its latest put named, together with
//! the KMS key exactly when that algorithm takes one ("required only when `sseAlgorithm`
//! is set to `aws:kms`", `smithy_query(query="doc", shape="EncryptionConfiguration",
//! member="kmsKeyArn")`), and the service default — S3-managed keys, no key — for a bucket
//! holding no configuration of its own, whether it never held one or a delete removed it.
//! It is therefore never a constant: two buckets of one account report what each was
//! given.
//!
//! RULE 2 — a `ListTables` row reports the LISTED table's stored record: its two instants
//! as RFC 3339 date-time strings (`smithy_query(query="shape", shape="TableSummary")`
//! reports `"timestampFormat": "date-time"` on `createdAt` and `modifiedAt`, both
//! `@required`) and, in `tableARN`, the id this service minted for the table — a canonical
//! hyphenated UUID, asserted through the one [`assert_canonical_uuid`] every suite shares.
//!
//! What this suite adds to the suites that state each rule on its own
//! (`table_bucket_encryption.rs`, `table_bucket_encryption_put.rs` and
//! `table_bucket_encryption_delete.rs` for RULE 1; `wire_forms.rs`,
//! `list_tables_summary_members.rs` and `list_tables_row_wire_forms.rs` for RULE 2) is the
//! dimension that CROSSES them, which is the shape a client reading a bucket works in: the
//! bucket's encryption configuration is written and rewritten under a listing that must not
//! move. So every row below carries both a trajectory of encryption writes (a
//! configuration named at create, puts that replace it, a delete that reverts it) and
//! tables whose listing is read before and after those writes — and the rows' text must be
//! the same text both times, because a write to the bucket's encryption re-mints no table
//! id and modifies no table.
//!
//! The KMS keys, the key slots a row names them by and the request member a configuration
//! travels in are the ones `table_bucket_encryption_put.rs` declares, so a key a
//! configuration names is one KMS itself holds and no second copy of that fixture exists.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::harness::{served, timestamp_seconds, Emulator};
// The one reading of a reported `encryptionConfiguration`, the key fixture and the request
// member a configuration travels in, and the one assertion of the minted-id form: each
// shared with the suite that owns its rule rather than copied.
use crate::table_bucket_encryption::reported;
use crate::table_bucket_encryption_put::{configuration_member, Configuration, Key, Keys};
use crate::wire_forms::{assert_canonical_uuid, create_table_request, summary_of, table_id_of};

/// The configuration a bucket holding none of its own is protected by: S3-managed keys,
/// no key. What a read reports for a bucket nothing was ever set on, and for one a delete
/// reverted.
const SERVICE_DEFAULT: Configuration = ("AES256", None);

/// A write a row applies to its bucket's encryption, in order.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `PutTableBucketEncryption` of this configuration.
    Put(Configuration),
    /// `DeleteTableBucketEncryption`, which reverts the bucket to [`SERVICE_DEFAULT`].
    Delete,
}

/// One row: a table bucket, the tables it holds, the encryption writes applied under the
/// listing of those tables, and the configuration a read must report afterwards.
#[derive(Debug)]
struct Case {
    label: &'static str,
    region: &'static str,
    bucket: &'static str,
    /// The namespace the row's tables are created in (`^[0-9a-z_]*$`).
    namespace: &'static str,
    /// The tables created in it, in request order.
    tables: &'static [&'static str],
    /// Whether the creates describe the tables' metadata.
    metadata: bool,
    /// The bucket create's `encryptionConfiguration`, or `None` when it named none.
    created_with: Option<Configuration>,
    /// The writes applied to the bucket's encryption, in order, after the first listing.
    writes: &'static [Write],
    /// What a read must report once those writes have landed.
    expected: Configuration,
}

/// The configuration a row ASSERTS: the algorithm, and the key ARN resolved from the slot
/// the row named.
fn expected_read(configuration: Configuration, keys: &Keys) -> (&'static str, Option<String>) {
    let (algorithm, key) = configuration;
    (algorithm, key.map(|key| keys.arn(key).to_string()))
}

/// The rows: the trajectories a bucket's encryption configuration can take (none, named at
/// create, put onto a bucket that had none, replaced by a put, moved from one key to
/// another, dropped back to S3-managed keys, removed by a delete) crossed with the shapes a
/// listing can have — one, two and three tables in a namespace, name lengths from the
/// modelled one-character minimum to a long name, with and without table metadata — over
/// the two Regions the shared key fixture holds keys in, each row naming keys of its own
/// Region.
const CASES: &[Case] = &[
    Case {
        label: "no configuration named, and none written: the service default",
        region: Keys::REGION,
        bucket: "reads-encryption-default",
        namespace: "n",
        tables: &["t"],
        metadata: false,
        created_with: None,
        writes: &[],
        expected: SERVICE_DEFAULT,
    },
    Case {
        label: "KMS-managed keys named at create, left alone",
        region: Keys::REGION,
        bucket: "reads-encryption-created-kms",
        namespace: "analytics_ns",
        tables: &["tbl_one", "tbl_two"],
        metadata: true,
        created_with: Some(("aws:kms", Some(Key::First))),
        writes: &[],
        expected: ("aws:kms", Some(Key::First)),
    },
    Case {
        label: "a put gives KMS-managed keys to a bucket that named none",
        region: Keys::REGION,
        bucket: "reads-encryption-put-kms",
        namespace: "ns_2026_q1",
        tables: &["t0", "sales_2026", "a_very_long_table_name_0123456789"],
        metadata: false,
        created_with: None,
        writes: &[Write::Put(("aws:kms", Some(Key::Second)))],
        expected: ("aws:kms", Some(Key::Second)),
    },
    Case {
        label: "a put replaces the configuration the create named, then moves to the \
                other key",
        region: Keys::OTHER_REGION,
        bucket: "reads-encryption-replaced",
        namespace: "maint_ns",
        tables: &["tbl_3", "tbl_4"],
        metadata: true,
        created_with: Some(("AES256", None)),
        writes: &[
            Write::Put(("aws:kms", Some(Key::West))),
            Write::Put(("aws:kms", Some(Key::WestSecond))),
        ],
        expected: ("aws:kms", Some(Key::WestSecond)),
    },
    Case {
        label: "a put back to S3-managed keys drops the key the bucket carried",
        region: Keys::OTHER_REGION,
        bucket: "reads-encryption-back-to-aes",
        namespace: "svc_ns",
        tables: &["customer_tbl"],
        metadata: false,
        created_with: Some(("aws:kms", Some(Key::West))),
        writes: &[Write::Put(("AES256", None))],
        expected: ("AES256", None),
    },
    Case {
        label: "a delete reverts the bucket to the service default",
        region: Keys::REGION,
        bucket: "reads-encryption-deleted",
        namespace: "events_ns",
        tables: &["events_tbl", "events_tbl_2"],
        metadata: false,
        created_with: Some(("aws:kms", Some(Key::Second))),
        writes: &[Write::Delete],
        expected: SERVICE_DEFAULT,
    },
    Case {
        label: "a put after a delete gives the bucket a configuration again",
        region: Keys::REGION,
        bucket: "reads-encryption-put-after-delete",
        namespace: "ns_a",
        tables: &["a0", "a_1", "a_very_long_table_name_0123456789"],
        metadata: true,
        created_with: Some(("AES256", None)),
        writes: &[Write::Delete, Write::Put(("aws:kms", Some(Key::First)))],
        expected: ("aws:kms", Some(Key::First)),
    },
];

/// What one `ListTables` row says about the table it stands for: the TEXT of its two
/// instants and its ARN, which is what must be the same before and after a write to the
/// bucket's encryption.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    created_at: String,
    modified_at: String,
    table_arn: String,
}

/// The TEXT of a row's timestamp member, with RULE 1's wire form asserted on it: the member
/// is a JSON STRING, and `timestamp_seconds` — the harness's ONE reading of a timestamp
/// member — accepts the modelled date-time form and refuses every other. The instant it
/// decodes to travels back beside the text so the two can be ordered.
fn instant(row: &Value, member: &str, table: &str, case: &str) -> (String, f64) {
    let actual = row.get(member);
    assert!(
        actual.is_some_and(Value::is_string),
        "case {case:?}: table {table:?}: expected {member} to be an RFC 3339 date-time \
         string, got {actual:?}"
    );
    let text = actual
        .and_then(Value::as_str)
        .expect("the member was just read as a string")
        .to_string();
    (text, timestamp_seconds(row, member, case))
}

/// The id segment of a row's `tableARN`, with RULE 2 asserted on it.
fn row_arn(row: &Value, table: &str, case: &str) -> String {
    let arn = row
        .get("tableARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: table {table:?}: the row carries no tableARN: {row}")
        });
    assert_canonical_uuid(
        &table_id_of(arn, case),
        "the id segment of a listing row's tableARN",
        case,
    );
    arn.to_string()
}

/// Every row of the bucket's listing, by table name, with both rules asserted on each.
///
/// `view` names which reading of the listing this is, so a failure says whether it was the
/// one before the encryption writes or the one after.
async fn listing(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    view: &str,
) -> BTreeMap<String, Row> {
    let label = format!("{} / {view}", case.label);
    let listed = served(
        emulator.list_tables(case.region, bucket_arn, &[]).await,
        "ListTables",
        &label,
    );
    let rows = listed.list("tables");
    assert_eq!(
        rows.len(),
        case.tables.len(),
        "case {label:?}: expected {expected} listing rows, got {actual}: {rows:?}",
        expected = case.tables.len(),
        actual = rows.len()
    );

    let mut collected = BTreeMap::new();
    for table in case.tables {
        let row = summary_of(&rows, table, &label);
        let (created_at, created_seconds) = instant(row, "createdAt", table, &label);
        let (modified_at, modified_seconds) = instant(row, "modifiedAt", table, &label);
        assert!(
            modified_seconds >= created_seconds,
            "case {label:?}: table {table:?}: the row was last modified at \
             {modified_seconds}, before it was created at {created_seconds}"
        );
        collected.insert(
            (*table).to_string(),
            Row {
                created_at,
                modified_at,
                table_arn: row_arn(row, table, &label),
            },
        );
    }
    collected
}

/// Create `case`'s table bucket, its namespace and its tables, and answer the bucket's ARN
/// beside the `tableARN` each create minted.
async fn set_up(
    emulator: &Emulator,
    case: &Case,
    keys: &Keys,
) -> (String, BTreeMap<String, String>) {
    let label = case.label;
    let mut request = Map::new();
    request.insert("name".to_string(), json!(case.bucket));
    if let Some(configuration) = case.created_with {
        request.insert(
            "encryptionConfiguration".to_string(),
            configuration_member(configuration, keys),
        );
    }
    let bucket = served(
        emulator
            .create_table_bucket(case.region, &Value::Object(request))
            .await,
        "CreateTableBucket",
        label,
    );
    let bucket_arn = bucket.text("arn");
    served(
        emulator
            .create_namespace(
                case.region,
                &bucket_arn,
                &json!({ "namespace": [case.namespace] }),
            )
            .await,
        "CreateNamespace",
        label,
    );

    let mut minted = BTreeMap::new();
    for table in case.tables {
        let created = served(
            emulator
                .create_table(
                    case.region,
                    &bucket_arn,
                    case.namespace,
                    &create_table_request(table, case.metadata),
                )
                .await,
            "CreateTable",
            label,
        );
        minted.insert((*table).to_string(), created.text("tableARN"));
    }
    (bucket_arn, minted)
}

/// Apply one of `case`'s writes and answer the configuration a read must report after it.
async fn apply(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    write: Write,
    keys: &Keys,
    step: &str,
) -> Configuration {
    match write {
        Write::Put(configuration) => {
            let put = served(
                emulator
                    .put_table_bucket_encryption(
                        case.region,
                        bucket_arn,
                        &configuration_member(configuration, keys),
                    )
                    .await,
                "PutTableBucketEncryption",
                step,
            );
            assert_eq!(
                put.error_code, None,
                "case {step:?}: a served put carries no error code, got {actual:?}",
                actual = put.error_code
            );
            configuration
        }
        Write::Delete => {
            let deleted = emulator
                .delete_table_bucket_encryption(case.region, bucket_arn)
                .await;
            assert_eq!(
                deleted.status, 204,
                "case {step:?}: expected the modelled 204, got {status} {body}",
                status = deleted.status,
                body = deleted.body
            );
            // The delete removes the bucket's own configuration, which reverts it to the
            // configuration a bucket holding none is protected by.
            SERVICE_DEFAULT
        }
    }
}

/// The configuration `GetTableBucketEncryption` reports for `bucket_arn`, asserted to be
/// `expected`.
async fn assert_reports(
    emulator: &Emulator,
    case: &Case,
    bucket_arn: &str,
    expected: Configuration,
    keys: &Keys,
    step: &str,
) -> (String, Option<String>) {
    let expected = expected_read(expected, keys);
    let read = served(
        emulator
            .get_table_bucket_encryption(case.region, bucket_arn)
            .await,
        "GetTableBucketEncryption",
        step,
    );
    let (algorithm, key) = reported(&read.body, step);
    assert_eq!(
        (algorithm.as_str(), key.as_deref()),
        (expected.0, expected.1.as_deref()),
        "case {step:?}: expected {expected:?}, got {algorithm:?}/{key:?}"
    );
    assert_eq!(
        key.is_some(),
        algorithm == "aws:kms",
        "case {step:?}: a key is carried exactly by the algorithm that takes one, got \
         {algorithm:?}/{key:?}"
    );
    (algorithm, key)
}

/// RULE 1 and RULE 2 together: the bucket's encryption is written under its listing, and
/// each read reports its own subject — the configuration in force, and the stored rows,
/// unmoved.
#[tokio::test]
async fn a_bucket_encryption_write_changes_the_configuration_a_read_reports_and_no_listing_row() {
    for case in CASES {
        let label = case.label;
        let emulator = Emulator::new();
        let keys = Keys::create(&emulator).await;

        let (bucket_arn, minted) = set_up(&emulator, case, &keys).await;

        // The listing as it stands before any encryption write, and the configuration the
        // create left the bucket with.
        let before = listing(&emulator, case, &bucket_arn, "before the encryption writes").await;
        for (table, arn) in &minted {
            assert_eq!(
                before[table].table_arn, *arn,
                "case {label:?}: table {table:?}: expected the row to carry the ARN the \
                 create minted ({arn:?}), got {actual:?}",
                actual = before[table].table_arn
            );
        }
        assert_reports(
            &emulator,
            case,
            &bucket_arn,
            case.created_with.unwrap_or(SERVICE_DEFAULT),
            &keys,
            &format!("{label} / before any write"),
        )
        .await;

        // Every write, and the configuration a read reports straight after it: a
        // trajectory is each of its steps landing, not only its last.
        for (ordinal, write) in case.writes.iter().enumerate() {
            let step = format!("{label} / write #{ordinal} ({write:?})");
            let settled = apply(&emulator, case, &bucket_arn, *write, &keys, &step).await;
            assert_reports(&emulator, case, &bucket_arn, settled, &keys, &step).await;
        }

        // The configuration the row says is in force once the trajectory has run.
        let (algorithm, key) =
            assert_reports(&emulator, case, &bucket_arn, case.expected, &keys, label).await;

        // And the listing is the one it was: a write to the bucket's encryption re-mints
        // no table id and modifies no table, so every row's text is the text it had.
        let after = listing(&emulator, case, &bucket_arn, "after the encryption writes").await;
        assert_eq!(
            after, before,
            "case {label:?}: the encryption writes moved the listing: {after:?} instead of \
             {before:?}"
        );

        // The reads report, they do not write: after the listing, the configuration is
        // still the same one.
        let again = served(
            emulator
                .get_table_bucket_encryption(case.region, &bucket_arn)
                .await,
            "GetTableBucketEncryption",
            label,
        );
        assert_eq!(
            reported(&again.body, label),
            (algorithm, key),
            "case {label:?}: a second read, after the listing, reports a different \
             configuration"
        );

        // Each table still reads back at its own coordinates, with the row's text: the
        // round trip the listing stands for.
        for (table, arn) in &minted {
            let described = served(
                emulator
                    .get_table(case.region, &bucket_arn, case.namespace, table)
                    .await,
                "GetTable",
                label,
            );
            assert_eq!(
                described.text("tableARN"),
                *arn,
                "case {label:?}: table {table:?}: the read reports a different tableARN \
                 from the create"
            );
            for (member, row_text) in [
                ("createdAt", &after[table].created_at),
                ("modifiedAt", &after[table].modified_at),
            ] {
                assert_eq!(
                    described.text(member),
                    *row_text,
                    "case {label:?}: table {table:?}: the read reports {member} as \
                     {actual:?}, not the {row_text:?} the listing row carries",
                    actual = described.text(member)
                );
            }
        }
    }
}

/// The rows of the per-bucket test below: one account's buckets, each given a different
/// configuration, in one Region and under one namespace name, so the only thing that
/// differs between them is what each was given and what each holds.
const SHARED_ACCOUNT_CASES: &[Case] = &[
    Case {
        label: "the bucket that named no configuration",
        region: Keys::REGION,
        bucket: "reads-encryption-many-default",
        namespace: "shared_ns",
        tables: &["only_tbl"],
        metadata: false,
        created_with: None,
        writes: &[],
        expected: SERVICE_DEFAULT,
    },
    Case {
        label: "the bucket created with KMS-managed keys",
        region: Keys::REGION,
        bucket: "reads-encryption-many-kms",
        namespace: "shared_ns",
        tables: &["tbl_one", "tbl_two"],
        metadata: true,
        created_with: Some(("aws:kms", Some(Key::First))),
        writes: &[],
        expected: ("aws:kms", Some(Key::First)),
    },
    Case {
        label: "the bucket a put moved to the other key",
        region: Keys::REGION,
        bucket: "reads-encryption-many-put",
        namespace: "shared_ns",
        tables: &["t0"],
        metadata: false,
        created_with: Some(("AES256", None)),
        writes: &[Write::Put(("aws:kms", Some(Key::Second)))],
        expected: ("aws:kms", Some(Key::Second)),
    },
    Case {
        label: "the bucket a delete reverted",
        region: Keys::REGION,
        bucket: "reads-encryption-many-deleted",
        namespace: "shared_ns",
        tables: &["events_tbl", "events_tbl_2", "events_tbl_3"],
        metadata: false,
        created_with: Some(("aws:kms", Some(Key::Second))),
        writes: &[Write::Delete],
        expected: SERVICE_DEFAULT,
    },
];

/// RULE 1 is per BUCKET: several buckets of one account, each given a different
/// configuration, each reporting its own — and each listing carrying only its own tables,
/// with ids no other bucket's table shares.
#[tokio::test]
async fn each_bucket_reports_its_own_configuration_and_its_own_tables() {
    let emulator = Emulator::new();
    let keys = Keys::create(&emulator).await;

    let mut buckets: Vec<(&Case, String)> = Vec::new();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for case in SHARED_ACCOUNT_CASES {
        let label = case.label;
        let (bucket_arn, minted) = set_up(&emulator, case, &keys).await;
        for (table, arn) in &minted {
            let id = table_id_of(arn, label);
            assert!(
                ids.insert(id.clone()),
                "case {label:?}: table {table:?}: the id {id:?} was already minted for \
                 another table"
            );
        }
        for (ordinal, write) in case.writes.iter().enumerate() {
            let step = format!("{label} / write #{ordinal} ({write:?})");
            apply(&emulator, case, &bucket_arn, *write, &keys, &step).await;
        }
        buckets.push((case, bucket_arn));
    }

    // Every bucket is read AFTER every bucket was written, and in the reverse order they
    // were created in, so what a read reports cannot be the last configuration written.
    for (case, bucket_arn) in buckets.iter().rev() {
        let label = case.label;
        assert_reports(&emulator, case, bucket_arn, case.expected, &keys, label).await;

        let listed = served(
            emulator.list_tables(case.region, bucket_arn, &[]).await,
            "ListTables",
            label,
        );
        let listed = listed.list("tables");
        let names: BTreeSet<String> = listed
            .iter()
            .filter_map(|row| row.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let expected_names: BTreeSet<String> =
            case.tables.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            names, expected_names,
            "case {label:?}: expected the listing to carry exactly {expected_names:?}, got \
             {names:?}"
        );
        for table in case.tables {
            let row = summary_of(&listed, table, label);
            instant(row, "createdAt", table, label);
            instant(row, "modifiedAt", table, label);
            let arn = row_arn(row, table, label);
            assert!(
                arn.starts_with(&format!("{bucket_arn}/table/")),
                "case {label:?}: table {table:?}: expected the row's ARN to name its own \
                 bucket ({bucket_arn:?}), got {arn:?}"
            );
        }
    }
}
