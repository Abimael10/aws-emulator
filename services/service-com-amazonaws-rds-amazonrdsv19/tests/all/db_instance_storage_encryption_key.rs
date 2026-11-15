// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KMS key a DB INSTANCE whose storage is encrypted reports.
//!
//! The rule under test: a DB instance created with storage encryption turned on is encrypted
//! under a key of this account whichever way the request did or did not name one — the key it
//! named, or, when it named none, the account's AWS managed key for this service in the
//! instance's Region ("If `StorageEncrypted` is enabled, and you do not specify a value for
//! the `KmsKeyId` parameter, then Amazon RDS uses your default KMS key. There is a default KMS
//! key for your Amazon Web Services account. Your Amazon Web Services account has a different
//! default KMS key for each Amazon Web Services Region",
//! `smithy_query(query="doc",shape="CreateDBInstanceMessage",member="KmsKeyId")`) — so that
//! key's identifier is reported by every envelope that projects the instance, and an instance
//! whose storage is NOT encrypted reports no key, because none encrypts it.
//!
//! Asserted over a TABLE spanning what the rule generalises over: whether the request turns
//! encryption on (named `true`, named `false`, left out), whether it names a key and in which
//! of the forms the member accepts (a bare key id, a key ARN), and the engine family the
//! instance runs — crossed with the two envelopes that project the stored instance
//! (`CreateDBInstance`'s own answer and the `DescribeDBInstances` read-back).
//!
//! The key an encrypted instance that named none reports is then checked against the account's
//! own `aws/rds` key AS KMS REPORTS IT: the suite asks KMS for that alias itself and asserts
//! the instance carries that key's ARN and that KMS manages the key (`KeyManager` `AWS`), so
//! what the instance reports is a key this account really holds rather than text derived from
//! the request. The key's own id is minted per run, so no identifier is restated here.
//!
//! A second test follows that key THROUGH the resources an encrypted instance begets — the
//! snapshot taken of it and the read replica created from it, neither of which names a key —
//! since a client audits encryption, grants the key or copies the snapshot by that identifier.
//!
//! Every call goes over the wire as a real awsQuery form POST, and KMS is mounted beside this
//! service on ONE engine exactly as the assembled router mounts them, so the key is resolved
//! against the service that OWNS it.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, expect_ok, optional_text, subtrees, text, the_instance, REGION,
};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{emulator, kms};

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The alias AWS publishes for the AWS managed key that encrypts Amazon RDS storage for an
/// account (`aws/rds`, the alias the KMS console and the RDS User Guide's "Encrypting Amazon
/// RDS resources" name that key by). The suite resolves it through KMS rather than assuming
/// anything about the key behind it.
const RDS_DEFAULT_KEY_ALIAS: &str = "alias/aws/rds";

/// The instance class and storage every row below orders, which every engine family in the
/// table is offered with.
const CLASS: &str = "db.t3.medium";

/// What the `KmsKeyId` of one created instance must be.
#[derive(Debug)]
enum ExpectedKey {
    /// The account's AWS managed key for this service: the request named none.
    AccountDefault,
    /// Exactly the key the request named, in the form it named it.
    Named(String),
    /// No key at all: nothing encrypts this instance's storage.
    NoKey,
}

/// One row of the table: a create, and what the instance it makes must report.
struct Row {
    case: String,
    identifier: String,
    engine: &'static str,
    /// The members beyond the identifier, engine and class that this row's request carries.
    extra: Vec<(&'static str, String)>,
    /// `StorageEncrypted` as the instance must report it.
    encrypted: bool,
    expected: ExpectedKey,
}

/// The rows: every combination of "is encryption on" and "is a key named, in which form",
/// over three engine families.
fn rows(key_id: &str, key_arn: &str) -> Vec<Row> {
    vec![
        Row {
            case: "encryption on, no key named, mysql".to_string(),
            identifier: "emu-storage-key-mysql-default".to_string(),
            engine: "mysql",
            extra: vec![("StorageEncrypted", "true".to_string())],
            encrypted: true,
            expected: ExpectedKey::AccountDefault,
        },
        Row {
            case: "encryption on, no key named, postgres".to_string(),
            identifier: "emu-storage-key-postgres-default".to_string(),
            engine: "postgres",
            extra: vec![("StorageEncrypted", "true".to_string())],
            encrypted: true,
            expected: ExpectedKey::AccountDefault,
        },
        Row {
            case: "encryption on, no key named, mariadb".to_string(),
            identifier: "emu-storage-key-mariadb-default".to_string(),
            engine: "mariadb",
            extra: vec![("StorageEncrypted", "true".to_string())],
            encrypted: true,
            expected: ExpectedKey::AccountDefault,
        },
        Row {
            case: "encryption on, a key named by its key id".to_string(),
            identifier: "emu-storage-key-named-id".to_string(),
            engine: "mysql",
            extra: vec![
                ("StorageEncrypted", "true".to_string()),
                ("KmsKeyId", key_id.to_string()),
            ],
            encrypted: true,
            expected: ExpectedKey::Named(key_id.to_string()),
        },
        Row {
            case: "encryption on, the same key named by its ARN".to_string(),
            identifier: "emu-storage-key-named-arn".to_string(),
            engine: "postgres",
            extra: vec![
                ("StorageEncrypted", "true".to_string()),
                ("KmsKeyId", key_arn.to_string()),
            ],
            encrypted: true,
            expected: ExpectedKey::Named(key_arn.to_string()),
        },
        Row {
            case: "StorageEncrypted left out of the request".to_string(),
            identifier: "emu-storage-key-unencrypted-default".to_string(),
            engine: "mysql",
            extra: Vec::new(),
            encrypted: false,
            expected: ExpectedKey::NoKey,
        },
        Row {
            case: "StorageEncrypted named false".to_string(),
            identifier: "emu-storage-key-unencrypted-named".to_string(),
            engine: "mariadb",
            extra: vec![("StorageEncrypted", "false".to_string())],
            encrypted: false,
            expected: ExpectedKey::NoKey,
        },
    ]
}

/// The one `<DBSnapshot>` subtree of a response.
///
/// The sibling reader of a snapshot envelope (`db_cluster_snapshot_envelope_members`) reads
/// the `<DBClusterSnapshot>` element, which is a different member of a different response, so
/// the instance snapshot this suite takes is read here.
fn the_snapshot(case: &str, xml: &str) -> String {
    let found = subtrees(xml, "DBSnapshot");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one DBSnapshot, got {found:?} in {xml}"
    );
    found.into_iter().next().expect("checked just above")
}

/// A key this emulator's own KMS holds for the account, as the two forms that name it: its
/// key id and its ARN, both as KMS itself reports them.
async fn customer_key(router: &MultiServiceRouter) -> (String, String) {
    let created = kms(router)
        .create_key()
        .description("the key these rows name themselves")
        .send()
        .await
        .expect("the emulated KMS creates the key");
    let metadata = created
        .key_metadata()
        .expect("the created key reports its metadata");
    (
        metadata.key_id().to_string(),
        metadata
            .arn()
            .expect("the created key reports its ARN")
            .to_string(),
    )
}

/// The ARN of the account's AWS managed RDS key, as KMS reports it, having also asserted that
/// KMS is the manager of that key — which is what makes it the ACCOUNT's default key for this
/// service rather than one a caller created.
async fn account_rds_key_arn(router: &MultiServiceRouter) -> String {
    let described = kms(router)
        .describe_key()
        .key_id(RDS_DEFAULT_KEY_ALIAS)
        .send()
        .await
        .expect("KMS resolves the account's aws/rds key");
    let metadata = described
        .key_metadata()
        .expect("the described key reports its metadata");
    let manager = metadata.key_manager().map(|manager| manager.as_str());
    assert_eq!(
        manager,
        Some("AWS"),
        "the key behind {RDS_DEFAULT_KEY_ALIAS} is managed by Amazon Web Services, got \
         {manager:?}"
    );
    metadata
        .arn()
        .expect("the described key reports its ARN")
        .to_string()
}

/// Assert the encryption members of one envelope: whether the storage is encrypted, and the
/// key it is encrypted under.
fn assert_storage_key(
    case: &str,
    envelope: &str,
    instance: &str,
    encrypted: bool,
    expected: &ExpectedKey,
    account_default: &str,
) {
    let reported = optional_text(instance, "StorageEncrypted");
    assert_eq!(
        reported.as_deref(),
        Some(encrypted.to_string().as_str()),
        "case {case:?}: the {envelope} envelope must report StorageEncrypted {encrypted}, got \
         {reported:?} in {instance}"
    );
    let key = optional_text(instance, "KmsKeyId");
    match expected {
        ExpectedKey::AccountDefault => {
            assert_eq!(
                key.as_deref(),
                Some(account_default),
                "case {case:?}: the {envelope} envelope must report the account's AWS managed \
                 RDS key {account_default:?}, got {key:?} in {instance}"
            );
        }
        ExpectedKey::Named(named) => {
            assert_eq!(
                key.as_deref(),
                Some(named.as_str()),
                "case {case:?}: the {envelope} envelope must report the key the request named \
                 {named:?}, got {key:?} in {instance}"
            );
        }
        ExpectedKey::NoKey => {
            assert_eq!(
                key, None,
                "case {case:?}: an instance whose storage is not encrypted is encrypted under \
                 no key, got {key:?} in {instance}"
            );
        }
    }
}

/// Create one instance and answer the `<DBInstance>` the create envelope carries.
async fn create(router: &MultiServiceRouter, row: &Row) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", row.identifier.clone()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", CLASS.to_string()),
    ];
    params.extend(row.extra.iter().cloned());
    let body = expect_ok(&row.case, call(router, "CreateDBInstance", &params).await);
    the_instance(&row.case, &body)
}

/// Poll `DescribeDBInstances` until `identifier` reports `available`, and answer the
/// `<DBInstance>` it settled on: a created instance is answered in a status the clock owes
/// `available`, and this suite waits for that CONDITION rather than for a span.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let params = [("DBInstanceIdentifier", identifier.to_string())];
    let read = || call(router, "DescribeDBInstances", &params);
    let (body, _polls) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        read,
        |status, body| {
            status == 200
                && subtrees(body, "DBInstance").first().is_some_and(|instance| {
                    optional_text(instance, "DBInstanceStatus").as_deref() == Some("available")
                })
        },
    )
    .await;
    the_instance(case, &body)
}

#[tokio::test]
async fn an_encrypted_db_instance_reports_the_key_its_storage_is_encrypted_under() {
    let (router, _engine) = emulator();
    let (key_id, key_arn) = customer_key(&router).await;
    let account_default = account_rds_key_arn(&router).await;
    // The account's default key is a key of this Region, named as a KMS key is named: the
    // member carries "the key ARN, key ID, alias ARN, or alias name for the KMS key", and the
    // identity a resolved key reports is its ARN.
    let prefix = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/");
    assert!(
        account_default.starts_with(&prefix),
        "the account's AWS managed RDS key is a key of this Region: expected an ARN under \
         {prefix:?}, got {account_default:?}"
    );

    for row in rows(&key_id, &key_arn) {
        let created = create(&router, &row).await;
        assert_storage_key(
            &row.case,
            "CreateDBInstance",
            &created,
            row.encrypted,
            &row.expected,
            &account_default,
        );

        // Read back through the describe: the key is a fact of the STORED instance, not of
        // the answer the create happened to build.
        let described = available_instance(&router, &row.case, &row.identifier).await;
        assert_storage_key(
            &row.case,
            "DescribeDBInstances",
            &described,
            row.encrypted,
            &row.expected,
            &account_default,
        );
    }
}

#[tokio::test]
async fn the_default_key_an_encrypted_instance_carries_follows_it_into_what_it_begets() {
    let (router, _engine) = emulator();
    let account_default = account_rds_key_arn(&router).await;

    let source = "emu-storage-key-source";
    let row = Row {
        case: "an encrypted instance created without naming a key".to_string(),
        identifier: source.to_string(),
        engine: "mysql",
        extra: vec![("StorageEncrypted", "true".to_string())],
        encrypted: true,
        expected: ExpectedKey::AccountDefault,
    };
    create(&router, &row).await;
    available_instance(&router, &row.case, source).await;

    // The snapshot of that instance is encrypted under the instance's own key: a client that
    // copies or restores it names the key by this identifier.
    let snapshot = "emu-storage-key-source-snap";
    let case = "a snapshot of an encrypted instance";
    let taken = the_snapshot(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "CreateDBSnapshot",
                &[
                    ("DBInstanceIdentifier", source),
                    ("DBSnapshotIdentifier", snapshot),
                ],
            )
            .await,
        ),
    );
    assert_eq!(
        text(&taken, "Encrypted"),
        "true",
        "case {case:?}: a snapshot of an encrypted instance is encrypted: {taken}"
    );
    assert_eq!(
        optional_text(&taken, "KmsKeyId").as_deref(),
        Some(account_default.as_str()),
        "case {case:?}: the snapshot reports the key the instance is encrypted under: {taken}"
    );
    let read_back = the_snapshot(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "DescribeDBSnapshots",
                &[("DBSnapshotIdentifier", snapshot)],
            )
            .await,
        ),
    );
    assert_eq!(
        optional_text(&read_back, "KmsKeyId").as_deref(),
        Some(account_default.as_str()),
        "case {case:?}: the stored snapshot reports that key too: {read_back}"
    );

    // And the read replica of that instance, which names no key either: "the read replica is
    // encrypted with the same KMS key as the source DB instance".
    let replica = "emu-storage-key-replica";
    let case = "a read replica of an encrypted instance";
    let created = the_instance(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "CreateDBInstanceReadReplica",
                &[
                    ("DBInstanceIdentifier", replica),
                    ("SourceDBInstanceIdentifier", source),
                ],
            )
            .await,
        ),
    );
    assert_storage_key(
        case,
        "CreateDBInstanceReadReplica",
        &created,
        true,
        &ExpectedKey::Named(account_default.clone()),
        &account_default,
    );
    let described = available_instance(&router, case, replica).await;
    assert_storage_key(
        case,
        "DescribeDBInstances",
        &described,
        true,
        &ExpectedKey::Named(account_default.clone()),
        &account_default,
    );
}
