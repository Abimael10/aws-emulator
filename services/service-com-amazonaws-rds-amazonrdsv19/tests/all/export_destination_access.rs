// SPDX-License-Identifier: CC0-1.0

//! What `StartExportTask` decides about the EXPORT ROLE and the destination bucket's own
//! policy, before any task exists.
//!
//! Two rules, each asserted over a table spanning what it generalises over:
//!
//! 1. The KMS grant that encrypts an export is created FOR the principal `IamRoleArn` names
//!    and ON the key `KmsKeyId` names, so a value this account's IAM resolves NO role for —
//!    whatever shape it has — is a grant that cannot be created: the modelled
//!    `KMSKeyNotAccessibleFault` (HTTP 400, `Sender`) worded as the generic "specified KMS
//!    key" sentence naming the key the grant would have been created on — the ARN the
//!    request's key reference RESOLVES to — and naming neither the role nor the action. The
//!    rows span the shapes a value that names no role of this account takes — a role ARN IAM
//!    holds nothing under, a role ARN of another account, a bare role name, an IAM ARN of
//!    another entity type, an ARN of another service, an IAM role ARN carrying a Region, and
//!    a role ARN with no name — crossed with the three spellings of the key a request may
//!    carry (bare key id, key ARN and an ALIAS, which names the key only through KMS's own
//!    resolution), which the one sentence names identically, with the accepted export as the
//!    control. `export_role_grant_refusals` beside this suite pins the two shapes it already
//!    covered; the rest are this suite's.
//! 2. The destination's EFFECTIVE access decides the export: Amazon RDS probes the calls it
//!    makes on the bucket before accepting the task, and an explicit deny in the bucket's own
//!    policy refuses it — "an explicit deny in any policy overrides any allow" — with the
//!    modelled `IamRoleMissingPermissionsFault` naming the call that failed. The rows span
//!    the spellings a destination policy states its rule about the export's identity in (the
//!    role's own ARN, the account root, the bare account id, and the Amazon RDS export
//!    service principal), the call each deny covers (the bucket-location read the export
//!    makes first, and the object write), and the resource it is scoped to (every object of
//!    the bucket, and only the keys under the export's own prefix) — with two controls that
//!    must still be ACCEPTED: a deny about another principal, and a deny scoped to keys this
//!    export does not write.
//!
//! Every call goes over the wire as a real awsQuery form POST, with Amazon S3, IAM and KMS
//! mounted beside this service on ONE engine exactly as the assembled router mounts them, so
//! the bucket policy the export reads is one Amazon S3 really holds and the role is one IAM
//! really holds.

use crate::db_instances::{call, expect_error, expect_ok, text, texts, REGION};
use crate::lifecycle_poll_loops::poll_until;
use crate::rds_s3_integration::{
    available_instance, create_bucket, emulator, export_role_policy, kms, role, s3,
};
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// An account that is NOT this emulator's, for the row whose `IamRoleArn` names a role of
/// another account.
const OTHER_ACCOUNT: &str = "123456789012";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The generic sentence the export's key fault carries where the GRANT could not be created,
/// spelled out here so the table pins the wording rather than the code under test. It names
/// the key the grant would have been created on, which is the key's own ARN.
fn key_not_accessible(key_arn: &str) -> String {
    format!(
        "The specified KMS key [{key_arn}] does not exist, is not enabled or you do not have \
         permissions to access it."
    )
}

/// The sentence the missing-permissions fault carries: the role, the call that failed and the
/// destination, likewise spelled out here.
fn missing_permissions(role_arn: &str, action: &str, bucket: &str) -> String {
    format!("The IAM role {role_arn} isn't authorized to call {action} on the S3 bucket {bucket}.")
}

/// Everything both tables need: the snapshot an export reads, the encryption key in the
/// spellings a request may name it by, and the router the calls go over.
struct Fixture {
    router: MultiServiceRouter,
    snapshot_arn: String,
    key_id: String,
    key_arn: String,
    /// An alias of that same key — the third spelling `KmsKeyId` accepts — which names no key
    /// at all until KMS resolves it.
    key_alias: String,
}

/// Stand up a source snapshot and an enabled key under `prefix`, so two tests in one target
/// never share a name.
async fn fixture(prefix: &str) -> Fixture {
    let (router, _engine) = emulator();
    let instance = format!("{prefix}-source");
    available_instance(&router, &instance, "mysql").await;

    let snapshot = format!("{prefix}-snapshot");
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
    // The snapshot is `creating` when the call answers and an export reads one that is
    // available: wait for that CONDITION rather than for a span.
    let described = params!["DBSnapshotIdentifier" => snapshot];
    poll_until(
        "snapshot the source",
        "the DB snapshot",
        || call(&router, "DescribeDBSnapshots", &described),
        |status, body| status == 200 && body.contains("<Status>available</Status>"),
    )
    .await;

    let created = kms(&router)
        .create_key()
        .description("the export's encryption key")
        .send()
        .await
        .expect("the emulated KMS creates the export key");
    let metadata = created
        .key_metadata()
        .expect("the created key reports its metadata")
        .clone();
    let key_alias = format!("alias/{prefix}-export-key");
    kms(&router)
        .create_alias()
        .alias_name(&key_alias)
        .target_key_id(metadata.key_id())
        .send()
        .await
        .expect("the emulated KMS holds the alias of the export key");

    Fixture {
        snapshot_arn: format!("arn:aws:rds:{REGION}:{ACCOUNT}:snapshot:{snapshot}"),
        key_id: metadata.key_id().to_string(),
        key_arn: metadata
            .arn()
            .expect("the created key reports its ARN")
            .to_string(),
        key_alias,
        router,
    }
}

/// Which spelling of the encryption key a row of the first table passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyReference {
    /// The bare key id KMS minted.
    Id,
    /// The key's ARN, the second spelling `KmsKeyId` accepts.
    Arn,
    /// An alias of the key, the third: it names the key only through KMS's own resolution.
    Alias,
}

/// One row of the unresolvable-role table: the value passed as `IamRoleArn`, and the spelling
/// of the key the request carries — whose own ARN the refusal's sentence must name.
struct RoleRow {
    case: &'static str,
    identifier: &'static str,
    /// `None` for the control row, which passes the role IAM really holds.
    role: Option<&'static str>,
    key: KeyReference,
}

/// The shapes a value that names no IAM role of THIS account takes. Each one leaves the
/// export's grant with no principal to be created for, so each one is the same answer.
fn role_rows() -> Vec<RoleRow> {
    vec![
        RoleRow {
            case: "a role ARN of this account IAM holds nothing under",
            identifier: "emu-dest-absent-role",
            role: Some("arn:aws:iam::000000000000:role/emu-dest-no-such-role"),
            key: KeyReference::Arn,
        },
        RoleRow {
            case: "a role ARN of another account, with the key named by its id",
            identifier: "emu-dest-other-account",
            role: Some("arn:aws:iam::123456789012:role/emu-dest-export-role"),
            key: KeyReference::Id,
        },
        RoleRow {
            case: "a bare role name, which is no ARN at all, with the key named by alias",
            identifier: "emu-dest-bare-name",
            role: Some("emu-dest-export-role"),
            key: KeyReference::Alias,
        },
        RoleRow {
            case: "an IAM ARN of another entity type",
            identifier: "emu-dest-user-arn",
            role: Some("arn:aws:iam::000000000000:user/emu-dest-export-user"),
            key: KeyReference::Id,
        },
        RoleRow {
            case: "an ARN of another service, with the key named by alias",
            identifier: "emu-dest-foreign-arn",
            role: Some("arn:aws:sts::000000000000:assumed-role/emu-dest-export-role/session"),
            key: KeyReference::Alias,
        },
        RoleRow {
            case: "an IAM role ARN carrying a Region, which no IAM ARN does",
            identifier: "emu-dest-regional-arn",
            role: Some("arn:aws:iam:us-west-2:000000000000:role/emu-dest-export-role"),
            key: KeyReference::Arn,
        },
        RoleRow {
            case: "a role ARN whose role name is empty",
            identifier: "emu-dest-empty-name",
            role: Some("arn:aws:iam::000000000000:role/"),
            key: KeyReference::Id,
        },
        RoleRow {
            case: "the control: the export role this account really holds",
            identifier: "emu-dest-accepted",
            role: None,
            key: KeyReference::Arn,
        },
    ]
}

/// Rule 1: every `IamRoleArn` this account's IAM resolves no role for is the export's key
/// fault in its generic wording, naming the key's own ARN whichever spelling the request
/// carried, and the role IAM holds is accepted.
#[tokio::test]
async fn an_unresolvable_export_role_is_the_key_grant_fault() {
    let fixture = fixture("emu-dest-grant").await;
    let bucket = "emu-dest-grant-destination";
    create_bucket(&fixture.router, bucket, REGION).await;
    let held_role = role(
        &fixture.router,
        "emu-dest-grant-role",
        st::EXPORT_SERVICE_PRINCIPAL,
        Some(export_role_policy(bucket)),
    )
    .await;
    // The other-account row's ARN is written out above; this keeps the account it names and
    // the one this emulator presents visibly apart.
    assert_ne!(
        ACCOUNT, OTHER_ACCOUNT,
        "the other account must not be this emulator's own"
    );

    for row in role_rows() {
        let case = row.case;
        let key = match row.key {
            KeyReference::Id => fixture.key_id.clone(),
            KeyReference::Arn => fixture.key_arn.clone(),
            KeyReference::Alias => fixture.key_alias.clone(),
        };
        let role_arn = row.role.map_or_else(|| held_role.clone(), str::to_string);
        let answered = call(
            &fixture.router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => row.identifier,
                "SourceArn" => fixture.snapshot_arn,
                "S3BucketName" => bucket,
                "IamRoleArn" => role_arn,
                "KmsKeyId" => key,
            ],
        )
        .await;

        if row.role.is_none() {
            let body = expect_ok(case, answered);
            assert_eq!(
                text(&body, "ExportTaskIdentifier"),
                row.identifier,
                "case {case:?}: the accepted export reports its own identifier, got {body}"
            );
            continue;
        }

        let body = expect_error(case, answered, 400, "KMSKeyNotAccessibleFault");
        let message = text(&body, "Message");
        // Whichever spelling this row passed, the sentence names the key itself.
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
        // A refused export records nothing: the region holds no task under the identifier.
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
            texts(&reported, "ExportTaskIdentifier").is_empty(),
            "case {case:?}: a refused export records no task, got {reported}"
        );
    }
}

/// What a destination row's bucket policy denies, and to whom.
struct DenyRow {
    case: &'static str,
    /// The bucket this row's policy is put on, and the export writes into.
    bucket: &'static str,
    /// The export task this row starts.
    identifier: &'static str,
    /// The `S3Prefix` the export is given, so a prefix-scoped policy can be written about the
    /// keys it really writes.
    s3_prefix: &'static str,
    /// Which principal spelling the deny names, built once the role and account are known.
    principal: Principal,
    /// The `Action` element of the deny.
    action: &'static str,
    /// The `Resource` element of the deny, with `{bucket}` and `{prefix}` substituted.
    resource: &'static str,
    /// The S3 action the refusal must name, or `None` where the export must be ACCEPTED
    /// because the deny is about something else.
    refused_action: Option<&'static str>,
}

/// Which identity a row's deny names — every spelling a destination policy may state its
/// rule about the export's own identity in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Principal {
    /// The export role's own ARN.
    Role,
    /// The account root, which denies each of the account's principals.
    AccountRoot,
    /// The bare account id, the other spelling of the same account.
    AccountId,
    /// The Amazon RDS export service principal the role is assumed by.
    ExportService,
    /// A role of this account the export is NOT run under: a control.
    AnotherRole,
}

/// The rows: every principal spelling, both calls the export makes on the destination, and a
/// deny scoped to the export's own prefix, with the two accepted controls last.
fn deny_rows() -> Vec<DenyRow> {
    vec![
        DenyRow {
            case: "the role's own ARN denied the object write on every object",
            bucket: "emu-dest-deny-role",
            identifier: "emu-dest-deny-role-task",
            s3_prefix: "exports",
            principal: Principal::Role,
            action: "s3:PutObject",
            resource: "arn:aws:s3:::{bucket}/*",
            refused_action: Some("s3:PutObject"),
        },
        DenyRow {
            case: "the role denied the object write on the keys of this export's prefix only",
            bucket: "emu-dest-deny-prefix",
            identifier: "emu-dest-deny-prefix-task",
            s3_prefix: "nightly",
            principal: Principal::Role,
            action: "s3:PutObject",
            resource: "arn:aws:s3:::{bucket}/{prefix}/*",
            refused_action: Some("s3:PutObject"),
        },
        DenyRow {
            case: "the account root denied every S3 call on the bucket itself",
            bucket: "emu-dest-deny-root",
            identifier: "emu-dest-deny-root-task",
            s3_prefix: "exports",
            principal: Principal::AccountRoot,
            action: "s3:*",
            resource: "arn:aws:s3:::{bucket}",
            // Nothing is written before the export finds out where the bucket is, so the
            // bucket-level call is the one that fails.
            refused_action: Some("s3:GetBucketLocation"),
        },
        DenyRow {
            case: "the bare account id denied the bucket-location read",
            bucket: "emu-dest-deny-account",
            identifier: "emu-dest-deny-account-task",
            s3_prefix: "exports",
            principal: Principal::AccountId,
            action: "s3:GetBucketLocation",
            resource: "arn:aws:s3:::{bucket}",
            refused_action: Some("s3:GetBucketLocation"),
        },
        DenyRow {
            case: "the Amazon RDS export service principal denied the object write",
            bucket: "emu-dest-deny-service",
            identifier: "emu-dest-deny-service-task",
            s3_prefix: "exports",
            principal: Principal::ExportService,
            action: "s3:PutObject",
            resource: "arn:aws:s3:::{bucket}/*",
            refused_action: Some("s3:PutObject"),
        },
        DenyRow {
            case: "a control: the deny is about another role of this account",
            bucket: "emu-dest-deny-other",
            identifier: "emu-dest-deny-other-task",
            s3_prefix: "exports",
            principal: Principal::AnotherRole,
            action: "s3:*",
            resource: "arn:aws:s3:::{bucket}/*",
            refused_action: None,
        },
        DenyRow {
            case: "a control: the deny is scoped to keys this export does not write",
            bucket: "emu-dest-deny-elsewhere",
            identifier: "emu-dest-deny-elsewhere-task",
            s3_prefix: "exports",
            principal: Principal::Role,
            action: "s3:PutObject",
            resource: "arn:aws:s3:::{bucket}/somebody-else/*",
            refused_action: None,
        },
    ]
}

/// The deny document one row puts on its bucket.
fn deny_document(row: &DenyRow, role_arn: &str, another_role_arn: &str) -> String {
    let principal = match row.principal {
        Principal::Role => format!(r#"{{"AWS":"{role_arn}"}}"#),
        Principal::AccountRoot => format!(r#"{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}}"#),
        Principal::AccountId => format!(r#"{{"AWS":"{ACCOUNT}"}}"#),
        Principal::ExportService => {
            format!(r#"{{"Service":"{}"}}"#, st::EXPORT_SERVICE_PRINCIPAL)
        }
        Principal::AnotherRole => format!(r#"{{"AWS":"{another_role_arn}"}}"#),
    };
    // The export's objects live under `<S3Prefix>/<ExportTaskIdentifier>/`, which is what a
    // prefix-scoped row states its rule about.
    let prefix = format!("{}/{}", row.s3_prefix, row.identifier);
    let resource = row
        .resource
        .replace("{bucket}", row.bucket)
        .replace("{prefix}", &prefix);
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Deny","Principal":{principal},"Action":"{}","Resource":"{resource}"}}]}}"#,
        row.action
    )
}

/// Rule 2: the destination bucket's own policy is part of the export's authorization, read
/// over every spelling of the export's identity and against the keys the export writes.
#[tokio::test]
async fn a_denying_destination_policy_refuses_the_export_it_covers() {
    let fixture = fixture("emu-dest-deny").await;
    // A role of this account no row runs its export under, for the control whose deny is
    // about somebody else. It is really created, because Amazon S3 takes no bucket policy
    // naming a principal this account does not hold.
    let another_role_arn = role(
        &fixture.router,
        "emu-dest-deny-bystander",
        st::EXPORT_SERVICE_PRINCIPAL,
        None,
    )
    .await;

    for row in deny_rows() {
        let case = row.case;
        create_bucket(&fixture.router, row.bucket, REGION).await;
        // One role per row, granted the export's own calls on that row's bucket: the identity
        // half ALLOWS every row, so what decides each one is the bucket's own policy.
        let role_arn = role(
            &fixture.router,
            &format!("{}-role", row.bucket),
            st::EXPORT_SERVICE_PRINCIPAL,
            Some(export_role_policy(row.bucket)),
        )
        .await;
        s3(&fixture.router)
            .put_bucket_policy()
            .bucket(row.bucket)
            .policy(deny_document(&row, &role_arn, &another_role_arn))
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: Amazon S3 takes the policy: {error}"));

        let answered = call(
            &fixture.router,
            "StartExportTask",
            &params![
                "ExportTaskIdentifier" => row.identifier,
                "SourceArn" => fixture.snapshot_arn,
                "S3BucketName" => row.bucket,
                "S3Prefix" => row.s3_prefix,
                "IamRoleArn" => role_arn,
                "KmsKeyId" => fixture.key_arn,
            ],
        )
        .await;

        match row.refused_action {
            Some(action) => {
                let body = expect_error(case, answered, 400, "IamRoleMissingPermissions");
                let message = text(&body, "Message");
                let expected = missing_permissions(&role_arn, action, row.bucket);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected the message {expected:?}, got {message:?} in {body}"
                );
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
                    texts(&reported, "ExportTaskIdentifier").is_empty(),
                    "case {case:?}: a refused export records no task, got {reported}"
                );
            }
            None => {
                let body = expect_ok(case, answered);
                assert_eq!(
                    text(&body, "ExportTaskIdentifier"),
                    row.identifier,
                    "case {case:?}: the accepted export reports its own identifier, got {body}"
                );
            }
        }
    }
}
