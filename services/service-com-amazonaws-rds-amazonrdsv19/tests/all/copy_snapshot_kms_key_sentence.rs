// SPDX-License-Identifier: CC0-1.0

//! The `KMSKeyNotAccessibleFault` a snapshot COPY answers, end to end: its sentence, the
//! fact that it leaves no copy behind, and the fact that it lifts the moment the key KMS
//! holds becomes usable again.
//!
//! Both operations that copy a snapshot into an encrypted copy under a caller-named key —
//! `CopyDBSnapshot` and `CopyDBClusterSnapshot` — take "the key ARN, key ID, alias ARN, or
//! alias name for the KMS key" in `KmsKeyId` and model the one
//! `KMSKeyNotAccessibleFault` (HTTP 400, `Error/Type` `Sender`) for a key that cannot
//! encrypt the copy. Each copy path words that fault its OWN way, and the opener is what
//! tells them apart: `CopyDBSnapshot` states it about the key the copy is TARGETED at ("The
//! target KMS key [") and `CopyDBClusterSnapshot` about the key the request SPECIFIED
//! ("Specified KMS key ["), while the creates open "The specified KMS key [" (pinned by
//! `encryption_key_accessibility.rs`). All three carry the `KmsKeyId` exactly as the request
//! spelled it and close with the three reasons the single fault covers.
//!
//! Asserted over a TABLE crossing
//!
//! * both snapshot copies, each with the source it copies and the describe that would
//!   report the copy;
//! * with every way a `KmsKeyId` names no key that can encrypt anything — a key ARN and a
//!   bare key id KMS holds nothing under, an alias this account holds no key for, and a key
//!   KMS really HOLDS but has disabled, named both by its key id and by its ARN.
//!
//! Each row asserts the full sentence, and then that the copy the row asked for is reported
//! by NOTHING: the describe that would hold it answers its own modelled "not found" fault
//! for that identifier.
//!
//! A second test closes the loop the issue's own sequence makes: the identical call that was
//! refused while the key was disabled succeeds once KMS re-enables it, and the copy is
//! readable back through its describe, encrypted under that key.
//!
//! Every call goes over the wire as a real awsQuery form POST, and KMS is mounted beside
//! this service on ONE engine exactly as the assembled router mounts them, so the key state
//! each row depends on is owned by the service that answers for it.

use crate::db_instances::{
    call, expect_error, expect_ok, optional_text, text, wait_for_settle, REGION,
};
use crate::rds_s3_integration::{available_cluster, available_instance, emulator, kms};
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The DB instance the copied DB snapshot is taken from.
const SOURCE_INSTANCE: &str = "emu-copy-key-source-db";

/// The DB snapshot every `CopyDBSnapshot` row copies.
const SOURCE_SNAPSHOT: &str = "emu-copy-key-source-snap";

/// The DB cluster the copied cluster snapshot is taken from.
const SOURCE_CLUSTER: &str = "emu-copy-key-source-cluster";

/// The DB cluster snapshot every `CopyDBClusterSnapshot` row copies.
const SOURCE_CLUSTER_SNAPSHOT: &str = "emu-copy-key-source-cluster-snap";

/// The sentence `CopyDBClusterSnapshot` answers `KMSKeyNotAccessibleFault` with, spelled out
/// here so the wording is pinned by the test rather than by the code under test.
fn specified_key_not_accessible(key_id: &str) -> String {
    format!(
        "Specified KMS key [{key_id}] does not exist, is not enabled or you do not have \
         permissions to access it."
    )
}

/// The sentence `CopyDBSnapshot` answers the same fault with: the same three reasons behind
/// its own opener, which states the fault about the key the copy is targeted at.
fn target_key_not_accessible(key_id: &str) -> String {
    format!(
        "The target KMS key [{key_id}] does not exist, is not enabled or you do not have \
         permissions to access it."
    )
}

/// The ARN form of a key of this account.
fn key_arn(key_id: &str) -> String {
    format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/{key_id}")
}

/// The fixture both tests run against: the router, the sources the copies read, and the one
/// key KMS really holds, created ENABLED and left to each test to disable.
struct Fixture {
    router: MultiServiceRouter,
    key_id: String,
}

/// Stand the fixture up: the two sources every copy resolves, so no refusal below can come
/// from a source that is missing or not yet available, and the key KMS holds.
async fn fixture() -> Fixture {
    let (router, _engine) = emulator();
    available_instance(&router, SOURCE_INSTANCE, "mysql").await;
    expect_ok(
        "snapshot the source instance",
        call(
            &router,
            "CreateDBSnapshot",
            &[
                ("DBInstanceIdentifier", SOURCE_INSTANCE),
                ("DBSnapshotIdentifier", SOURCE_SNAPSHOT),
            ],
        )
        .await,
    );
    available_cluster(&router, SOURCE_CLUSTER, "aurora-postgresql").await;
    expect_ok(
        "snapshot the source cluster",
        call(
            &router,
            "CreateDBClusterSnapshot",
            &[
                ("DBClusterIdentifier", SOURCE_CLUSTER),
                ("DBClusterSnapshotIdentifier", SOURCE_CLUSTER_SNAPSHOT),
            ],
        )
        .await,
    );
    // Both snapshots are answered in a status the clock owes `available`, and a copy reads a
    // source that holds data: the fixture waits that span out once, for both.
    wait_for_settle();

    let key_id = kms(&router)
        .create_key()
        .description("the key the copies below name")
        .send()
        .await
        .expect("the emulated KMS creates the key")
        .key_metadata()
        .map(|metadata| metadata.key_id().to_string())
        .expect("the created key reports its id");
    Fixture { router, key_id }
}

/// Turn the fixture's key off, so KMS performs no cryptographic operation with it.
async fn disable(fixture: &Fixture) {
    kms(&fixture.router)
        .disable_key()
        .key_id(&fixture.key_id)
        .send()
        .await
        .expect("the emulated KMS disables the key");
}

/// Turn it back on, so the identical call can be served.
async fn enable(fixture: &Fixture) {
    kms(&fixture.router)
        .enable_key()
        .key_id(&fixture.key_id)
        .send()
        .await
        .expect("the emulated KMS enables the key");
}

/// One snapshot copy of the table: the call, the source it copies, the member that names the
/// copy, and the describe that would report one.
struct CopyRow {
    case: &'static str,
    action: &'static str,
    source_member: &'static str,
    source: &'static str,
    target_member: &'static str,
    describe: &'static str,
    /// The member the describe selects one snapshot by, and names the copy under.
    identifier_member: &'static str,
    /// The fault the describe answers for an identifier it holds no snapshot under.
    not_found_code: &'static str,
    /// The member the describe reports the copy's encryption under: a DB snapshot publishes
    /// `Encrypted`, a DB cluster snapshot `StorageEncrypted`.
    encrypted_member: &'static str,
    /// The whole `KMSKeyNotAccessibleFault` sentence THIS copy words, around the `KmsKeyId`
    /// the request carried.
    sentence: fn(&str) -> String,
}

/// Both operations of this service that copy a snapshot under a caller-named key.
fn copy_rows() -> Vec<CopyRow> {
    vec![
        CopyRow {
            case: "CopyDBSnapshot",
            action: "CopyDBSnapshot",
            source_member: "SourceDBSnapshotIdentifier",
            source: SOURCE_SNAPSHOT,
            target_member: "TargetDBSnapshotIdentifier",
            describe: "DescribeDBSnapshots",
            identifier_member: "DBSnapshotIdentifier",
            not_found_code: "DBSnapshotNotFound",
            encrypted_member: "Encrypted",
            sentence: target_key_not_accessible,
        },
        CopyRow {
            case: "CopyDBClusterSnapshot",
            action: "CopyDBClusterSnapshot",
            source_member: "SourceDBClusterSnapshotIdentifier",
            source: SOURCE_CLUSTER_SNAPSHOT,
            target_member: "TargetDBClusterSnapshotIdentifier",
            describe: "DescribeDBClusterSnapshots",
            identifier_member: "DBClusterSnapshotIdentifier",
            not_found_code: "DBClusterSnapshotNotFoundFault",
            encrypted_member: "StorageEncrypted",
            sentence: specified_key_not_accessible,
        },
    ]
}

/// One `KmsKeyId` a row names: how the request spells it, and the value itself.
struct KeyRow {
    case: String,
    key_id: String,
}

/// Every `KmsKeyId` no copy can be encrypted under: the three forms the member accepts
/// naming no key of this account, and the key KMS HOLDS disabled, in both forms that name
/// one key.
fn inaccessible_keys(fixture: &Fixture) -> Vec<KeyRow> {
    vec![
        KeyRow {
            case: "a key ARN KMS holds nothing under".to_string(),
            key_id: key_arn("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
        },
        KeyRow {
            case: "a bare key id KMS holds nothing under".to_string(),
            key_id: "bbbbbbbb-cccc-dddd-eeee-ffffffffffff".to_string(),
        },
        KeyRow {
            case: "an alias this account holds no key under".to_string(),
            key_id: "alias/emu-copy-absent-key".to_string(),
        },
        KeyRow {
            case: "the key KMS holds disabled, by its key id".to_string(),
            key_id: fixture.key_id.clone(),
        },
        KeyRow {
            case: "the same disabled key, by its ARN".to_string(),
            key_id: key_arn(&fixture.key_id),
        },
    ]
}

/// Assert one refusal in full: the status, the wire `<Code>`, the exact `<Message>` and the
/// fault class.
fn expect_refusal(case: &str, response: (u16, String), message: &str) {
    let body = expect_error(case, response, 400, "KMSKeyNotAccessibleFault");
    let answered = text(&body, "Message");
    assert_eq!(
        answered, message,
        "case {case:?}: expected the message {message:?}, got {answered:?} in {body}"
    );
    let kind = text(&body, "Type");
    assert_eq!(
        kind, "Sender",
        "case {case:?}: expected a Sender fault, got {kind:?} in {body}"
    );
}

#[tokio::test]
async fn a_snapshot_copy_under_an_unusable_key_is_refused_with_the_copy_sentence() {
    let fixture = fixture().await;
    disable(&fixture).await;

    for (key_index, key_row) in inaccessible_keys(&fixture).into_iter().enumerate() {
        for row in copy_rows() {
            let case = format!("{} with {}", row.case, key_row.case);
            let target = format!("emu-copy-key-{}-r{key_index}", row.case.to_lowercase());
            expect_refusal(
                &case,
                call(
                    &fixture.router,
                    row.action,
                    &[
                        (row.source_member, row.source.to_string()),
                        (row.target_member, target.clone()),
                        ("KmsKeyId", key_row.key_id.clone()),
                    ],
                )
                .await,
                &(row.sentence)(&key_row.key_id),
            );

            // The refusal is a decision about the request, answered before the copy is
            // minted: the describe that would report the copy holds no snapshot under the
            // identifier the row asked for, and says so with its own modelled fault.
            let (status, body) = call(
                &fixture.router,
                row.describe,
                &[(row.identifier_member, target.as_str())],
            )
            .await;
            assert_eq!(
                status, 404,
                "case {case:?}: a refused copy records nothing, so {} answers 404, got \
                 {status} with {body}",
                row.describe
            );
            let code = text(&body, "Code");
            assert_eq!(
                code, row.not_found_code,
                "case {case:?}: expected {:?}, got {code:?} in {body}",
                row.not_found_code
            );
        }
    }
}

#[tokio::test]
async fn the_same_copy_succeeds_once_kms_re_enables_the_key() {
    let fixture = fixture().await;
    disable(&fixture).await;
    let named = key_arn(&fixture.key_id);

    for row in copy_rows() {
        let target = format!("emu-copy-key-lifted-{}", row.case.to_lowercase());
        let params = [
            (row.source_member, row.source.to_string()),
            (row.target_member, target.clone()),
            ("KmsKeyId", named.clone()),
        ];
        expect_refusal(
            row.case,
            call(&fixture.router, row.action, &params).await,
            &(row.sentence)(&named),
        );

        // The identical call, once the key performs cryptographic operations again.
        enable(&fixture).await;
        let copied = expect_ok(row.case, call(&fixture.router, row.action, &params).await);
        assert_eq!(
            text(&copied, row.encrypted_member),
            "true",
            "case {:?}: a copy under a named key is encrypted: {copied}",
            row.case
        );
        assert_eq!(
            optional_text(&copied, "KmsKeyId").as_deref(),
            Some(named.as_str()),
            "case {:?}: the copy reports the key it was encrypted under: {copied}",
            row.case
        );

        // And the copy is a fact of state, not of the response the call happened to build.
        let described = expect_ok(
            row.describe,
            call(
                &fixture.router,
                row.describe,
                &[(row.identifier_member, target.as_str())],
            )
            .await,
        );
        assert_eq!(
            text(&described, row.identifier_member),
            target,
            "case {:?}: the stored copy is reported under the identifier asked for: \
             {described}",
            row.case
        );
        assert_eq!(
            optional_text(&described, "KmsKeyId").as_deref(),
            Some(named.as_str()),
            "case {:?}: the stored copy reports its key: {described}",
            row.case
        );

        // Leave the key off again for the next row, so each row starts from the refusal.
        disable(&fixture).await;
    }
}
