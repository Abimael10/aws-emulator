// SPDX-License-Identifier: CC0-1.0

//! What `StartExportTask` answers for an `IamRoleArn` whose role the export's KMS GRANT
//! cannot be created for.
//!
//! The rule under test: **the grant that encrypts an export is created FOR the role the
//! request names, so an `IamRoleArn` this account's IAM resolves no role for is a grant that
//! cannot be created — the modelled `KMSKeyNotAccessibleFault` (HTTP 400, `Sender`), worded
//! as the generic "specified KMS key" sentence naming the KEY the request's reference
//! resolves to, and naming neither the role nor the action.** It is the role,
//! not the key, that decided the refusal: the key the rows below pass is one KMS holds and
//! reports enabled, and the sentence a key that is itself unusable carries is a different one
//! (`cluster_role_and_export_key_refusals` pins that one).
//!
//! Two conditions leave the role unresolved, and both are the same answer: IAM reports no
//! role under the name, and the value names a role of ANOTHER account, whose roles this
//! account's IAM cannot resolve at all. The rows span both of them, the key reference forms
//! the request may carry (bare key id and key ARN, which name ONE key and so leave one
//! sentence), and a role name under a path — with the accepted export as the control, so the
//! table cannot pass against a handler that refuses every request.
//!
//! Every call goes over the wire as a real awsQuery form POST, with IAM, KMS and Amazon S3
//! mounted beside this service on ONE engine exactly as the assembled router mounts them, and
//! the snapshot the export reads is waited for through this target's own poll loop.

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    available_instance, create_bucket, emulator, export_role_policy, kms, role,
};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An account that is NOT this emulator's, for the rows whose `IamRoleArn` names a role of
/// another account.
const OTHER_ACCOUNT: &str = "123456789012";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The sentence this refusal carries, spelled out here so the table pins the wording rather
/// than the code under test: the three reasons the one fault covers, naming the key the
/// request's reference resolved to.
fn key_not_accessible(key: &str) -> String {
    format!(
        "The specified KMS key [{key}] does not exist, is not enabled or you do not have \
         permissions to access it."
    )
}

/// Which spelling of the encryption key a row passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyReference {
    /// The bare key id KMS minted.
    Id,
    /// The key's ARN, which is the other spelling `KmsKeyId` accepts.
    Arn,
}

/// Which value a row passes as `IamRoleArn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleReference {
    /// A role ARN of THIS account that IAM holds no role under.
    AbsentHere,
    /// A role ARN of this account whose name carries a path, likewise held by nothing.
    AbsentUnderAPath,
    /// A well-formed role ARN of ANOTHER account.
    OfAnotherAccount,
    /// The export role this fixture really created: the control.
    TheExportRole,
}

/// One row: the role the export is asked to run under, and the spelling of the key.
struct Row {
    case: &'static str,
    identifier: &'static str,
    role: RoleReference,
    key: KeyReference,
}

/// The rows span what the rule generalises over: both conditions that leave the role
/// unresolved, the form the role name takes, and both spellings of the key the sentence
/// echoes — with the accepted export last.
const ROWS: &[Row] = &[
    Row {
        case: "a role of this account IAM holds nothing under, named by key id",
        identifier: "emu-grant-absent-id",
        role: RoleReference::AbsentHere,
        key: KeyReference::Id,
    },
    Row {
        case: "a role of this account IAM holds nothing under, named by key ARN",
        identifier: "emu-grant-absent-arn",
        role: RoleReference::AbsentHere,
        key: KeyReference::Arn,
    },
    Row {
        case: "a role under a path IAM holds nothing under",
        identifier: "emu-grant-absent-path",
        role: RoleReference::AbsentUnderAPath,
        key: KeyReference::Arn,
    },
    Row {
        case: "a well-formed role ARN of another account",
        identifier: "emu-grant-other-account",
        role: RoleReference::OfAnotherAccount,
        key: KeyReference::Arn,
    },
    Row {
        case: "the control: the export role this account really holds",
        identifier: "emu-grant-accepted",
        role: RoleReference::TheExportRole,
        key: KeyReference::Arn,
    },
];

/// Everything these rows need: the snapshot the export reads, the destination bucket, the
/// encryption key in both of its spellings, and the one role IAM really holds.
struct Fixture {
    router: MultiServiceRouter,
    bucket: String,
    snapshot_arn: String,
    key_id: String,
    key_arn: String,
    export_role_arn: String,
}

impl Fixture {
    /// The `KmsKeyId` a row passes.
    fn key(&self, reference: KeyReference) -> &str {
        match reference {
            KeyReference::Id => &self.key_id,
            KeyReference::Arn => &self.key_arn,
        }
    }

    /// The `IamRoleArn` a row passes.
    fn role(&self, reference: RoleReference) -> String {
        match reference {
            RoleReference::AbsentHere => {
                format!("arn:aws:iam::{ACCOUNT}:role/emu-grant-no-such-role")
            }
            RoleReference::AbsentUnderAPath => {
                format!("arn:aws:iam::{ACCOUNT}:role/service-role/emu-grant-no-such-role")
            }
            RoleReference::OfAnotherAccount => {
                format!("arn:aws:iam::{OTHER_ACCOUNT}:role/emu-grant-export-role")
            }
            RoleReference::TheExportRole => self.export_role_arn.clone(),
        }
    }
}

/// Stand the fixture up: an available DB instance, a snapshot of it, the bucket, the key and
/// the export role that trusts the export principal and may write the destination.
async fn fixture() -> Fixture {
    let (router, _engine) = emulator();
    let instance = "emu-grant-source";
    available_instance(&router, instance, "mysql").await;

    let snapshot = "emu-grant-snapshot";
    expect_ok(
        "snapshot the source",
        call(
            &router,
            "CreateDBSnapshot",
            &params![
                "DBInstanceIdentifier" => instance,
                "DBSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    // The snapshot is `creating` when the call answers, and an export reads one that is
    // available: the suite waits for that CONDITION rather than for a span.
    let described = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        "snapshot the source",
        "the DB snapshot",
        || call(&router, "DescribeDBSnapshots", &described),
        |status, body| status == 200 && body.contains("<Status>available</Status>"),
    )
    .await;

    let bucket = "emu-grant-destination";
    create_bucket(&router, bucket, REGION).await;
    let created = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key");
    let metadata = created
        .key_metadata()
        .expect("the created key reports its metadata");
    let export_role_arn = role(
        &router,
        "emu-grant-export-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(bucket)),
    )
    .await;

    Fixture {
        bucket: bucket.to_string(),
        snapshot_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}"),
        key_id: metadata.key_id().to_string(),
        key_arn: metadata
            .arn()
            .expect("the created key reports its ARN")
            .to_string(),
        export_role_arn,
        router,
    }
}

/// Every row: a role the grant cannot be created for is the modelled key fault with the
/// generic sentence naming the key the request passed, and the control export is accepted.
#[tokio::test]
async fn a_role_the_export_grant_cannot_be_created_for_is_the_key_fault() {
    let fixture = fixture().await;

    for row in ROWS {
        let case = row.case;
        let key = fixture.key(row.key).to_string();
        let request = params![
            "ExportTaskIdentifier" => row.identifier,
            "SourceArn" => fixture.snapshot_arn,
            "S3BucketName" => fixture.bucket,
            "IamRoleArn" => fixture.role(row.role),
            "KmsKeyId" => key,
        ];
        let answered = call(&fixture.router, "StartExportTask", &request).await;

        if row.role == RoleReference::TheExportRole {
            let body = expect_ok(case, answered);
            assert_eq!(
                text(&body, "ExportTaskIdentifier"),
                row.identifier,
                "case {case:?}: the accepted export reports the identifier it was started \
                 under, got {body}"
            );
            continue;
        }

        let body = expect_error(case, answered, 400, "KMSKeyNotAccessibleFault");
        let message = text(&body, "Message");
        // Whichever spelling the row passed, the key it names is the ONE key KMS holds, so
        // the sentence names that key's ARN.
        let expected = key_not_accessible(&fixture.key_arn);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?} in {body}"
        );
        let kind = text(&body, "Type");
        assert_eq!(
            kind, "Sender",
            "case {case:?}: expected a Sender fault, got {kind:?} in {body}"
        );
        // Nothing is recorded for a refused export: the region holds no task under the
        // identifier it named.
        let reported = expect_ok(
            case,
            call(
                &fixture.router,
                "DescribeExportTasks",
                &params!["ExportTaskIdentifier" => row.identifier],
            )
            .await,
        );
        assert!(
            crate::db_instances::texts(&reported, "ExportTaskIdentifier").is_empty(),
            "case {case:?}: a refused export records no task, got {reported}"
        );
    }
}
