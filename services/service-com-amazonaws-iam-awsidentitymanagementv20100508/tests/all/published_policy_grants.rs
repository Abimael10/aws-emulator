// SPDX-License-Identifier: CC0-1.0

//! What a role carrying an AWS-MANAGED policy is allowed, reported by the simulation of
//! its policies, end to end over the awsQuery wire.
//!
//! The RULE under test: a policy AWS owns is served with the grants AWS publishes for
//! it, so the decision the simulation reaches for a role the policy is attached to is
//! the decision AWS reaches — `allowed` for every action the published policy grants
//! and `implicitDeny` for an action no statement of it names. The grants of the service
//! -role policies AWS publishes for a service to act through are stated per resource
//! type, and each type's grants include the action that performs the work the role
//! exists for, so a role carrying one of those policies may do that work for EVERY type
//! the policy covers — not only for the one a caller happens to ask about first.
//!
//! The table spans that rule along the policy the role carries (the backup half and the
//! restore half of the AWS Backup service-role family, and a service-role policy of
//! another service as a row that is not of that family), the SERVICE NAMESPACE of the
//! action asked about, the resource type the action creates, and whether the published
//! policy grants the action at all.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other
//! suites in this crate share, and the decision is read through the simulation harness
//! `tests/all/policy_simulation.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{reported, simulate, ALLOWED, IMPLICIT_DENY};
use crate::user_lifecycle::{account_id, call, element, router};

/// A trust policy with a single statement, which every role needs to exist at all.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"backup.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The Region every resource ARN of this suite names; IAM itself is global, so this is
/// the Region of the resource the simulated call would be made in.
const REGION: &str = "us-west-2";

/// The ARNs of the published policies the rows attach, as the AWS Managed Policy
/// Reference spells them: the two halves of the AWS Backup service-role family, and one
/// service-role policy of another service.
const FOR_BACKUP: &str = "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup";
const FOR_RESTORES: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForRestores";
const FOR_SQS_ESM: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole";

/// One row: the policy the role carries, the action asked about, the resource it is
/// asked about, and the decision the published grants reach.
struct Case {
    /// What this row is about, named in every assertion of it.
    name: &'static str,
    /// The IAM role name the row creates, unique so each row's attachment is its own.
    role_name: &'static str,
    /// The published policy attached to that role.
    policy_arn: &'static str,
    /// The action the simulation is asked about.
    action: &'static str,
    /// The resource ARN the action is asked about.
    resource_arn: String,
    /// The decision the published policy reaches for that pair.
    expected: &'static str,
}

/// An ARN of this account in [`REGION`].
fn resource(namespace: &str, resource: &str) -> String {
    format!("arn:aws:{namespace}:{REGION}:{}:{resource}", account_id())
}

/// The table.
///
/// The actions of the first two blocks are the ones AWS publishes for the AWS Backup
/// service role (AWS Backup, *Managed policies*): per protected resource type, the
/// action that creates that type's recovery point for the backup policy, and the action
/// that creates the resource back from a recovery point for the restore policy. The
/// rows that expect `implicitDeny` are actions of the OTHER half of the family, which
/// is what keeps the two documents apart rather than making either grant everything.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "the backup policy and a table's creating action",
            role_name: "backup-table",
            policy_arn: FOR_BACKUP,
            action: "dynamodb:CreateBackup",
            resource_arn: resource("dynamodb", "table/orders"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a volume's creating action",
            role_name: "backup-volume",
            policy_arn: FOR_BACKUP,
            action: "ec2:CreateSnapshot",
            resource_arn: resource("ec2", "volume/vol-0123456789abcdef0"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and an instance's creating action",
            role_name: "backup-instance",
            policy_arn: FOR_BACKUP,
            action: "ec2:CreateImage",
            resource_arn: resource("ec2", "instance/i-0123456789abcdef0"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a file system's creating action",
            role_name: "backup-filesystem",
            policy_arn: FOR_BACKUP,
            action: "elasticfilesystem:Backup",
            resource_arn: resource("elasticfilesystem", "file-system/fs-0123456789abcdef0"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a database instance's creating action",
            role_name: "backup-database",
            policy_arn: FOR_BACKUP,
            action: "rds:CreateDBSnapshot",
            resource_arn: resource("rds", "db:orders-instance"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a cluster's creating action",
            role_name: "backup-cluster",
            policy_arn: FOR_BACKUP,
            action: "rds:CreateDBClusterSnapshot",
            resource_arn: resource("rds", "cluster:orders-cluster"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and an Amazon FSx file system's creating action",
            role_name: "backup-fsx",
            policy_arn: FOR_BACKUP,
            action: "fsx:CreateBackup",
            resource_arn: resource("fsx", "file-system/fs-0123456789abcdef1"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a Redshift cluster's creating action",
            role_name: "backup-redshift",
            policy_arn: FOR_BACKUP,
            action: "redshift:CreateClusterSnapshot",
            resource_arn: resource("redshift", "cluster:orders-warehouse"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a Timestream table's creating action",
            role_name: "backup-timestream",
            policy_arn: FOR_BACKUP,
            action: "timestream:StartAwsBackupJob",
            resource_arn: resource("timestream", "database/metrics/table/readings"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and the vault read a backup makes",
            role_name: "backup-vault-read",
            policy_arn: FOR_BACKUP,
            action: "backup:DescribeBackupVault",
            resource_arn: resource("backup", "backup-vault:orders-vault"),
            expected: ALLOWED,
        },
        Case {
            name: "the backup policy and a restore action it does not grant",
            role_name: "backup-restore-action",
            policy_arn: FOR_BACKUP,
            action: "dynamodb:RestoreTableFromBackup",
            resource_arn: resource("dynamodb", "table/orders"),
            expected: IMPLICIT_DENY,
        },
        Case {
            name: "the restore policy and a table's restoring action",
            role_name: "restore-table",
            policy_arn: FOR_RESTORES,
            action: "dynamodb:RestoreTableFromBackup",
            resource_arn: resource("dynamodb", "table/orders"),
            expected: ALLOWED,
        },
        Case {
            name: "the restore policy and a volume's restoring action",
            role_name: "restore-volume",
            policy_arn: FOR_RESTORES,
            action: "ec2:CreateVolume",
            resource_arn: resource("ec2", "volume/vol-0123456789abcdef0"),
            expected: ALLOWED,
        },
        Case {
            name: "the restore policy and a database instance's restoring action",
            role_name: "restore-database",
            policy_arn: FOR_RESTORES,
            action: "rds:RestoreDBInstanceFromDBSnapshot",
            resource_arn: resource("rds", "db:orders-instance"),
            expected: ALLOWED,
        },
        Case {
            name: "the restore policy and a cluster's restoring action",
            role_name: "restore-cluster",
            policy_arn: FOR_RESTORES,
            action: "rds:RestoreDBClusterFromSnapshot",
            resource_arn: resource("rds", "cluster:orders-cluster"),
            expected: ALLOWED,
        },
        Case {
            name: "the restore policy and a backup action it does not grant",
            role_name: "restore-backup-action",
            policy_arn: FOR_RESTORES,
            action: "dynamodb:CreateBackup",
            resource_arn: resource("dynamodb", "table/orders"),
            expected: IMPLICIT_DENY,
        },
        Case {
            name: "a service-role policy of another service and the read it grants",
            role_name: "sqs-esm",
            policy_arn: FOR_SQS_ESM,
            action: "sqs:ReceiveMessage",
            resource_arn: resource("sqs", "orders-queue"),
            expected: ALLOWED,
        },
        Case {
            name: "a service-role policy of another service and an action it does not grant",
            role_name: "sqs-esm-other",
            policy_arn: FOR_SQS_ESM,
            action: "dynamodb:CreateBackup",
            resource_arn: resource("dynamodb", "table/orders"),
            expected: IMPLICIT_DENY,
        },
    ]
}

/// Create the role `role_name` and attach `policy_arn` to it, answering the ARN IAM
/// minted for the role — read off the response rather than constructed, so the
/// simulation is addressed to the principal IAM actually holds.
async fn role_carrying(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy_arn: &str,
) -> String {
    let (status, body) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("Path", "/service-role/"),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole must succeed, got {status}: {body}"
    );
    let role_arn = element(&body, "Arn");
    let (status, attached) = call(
        router,
        "AttachRolePolicy",
        &[("RoleName", role_name), ("PolicyArn", policy_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AttachRolePolicy {policy_arn} must succeed, got {status}: {attached}"
    );
    role_arn
}

/// RULE: the grants a published AWS-managed policy carries are the grants AWS publishes
/// for it, so the simulation of a role carrying one allows every action that policy
/// grants and allows no action it does not.
#[tokio::test]
async fn a_published_policy_grants_what_aws_publishes_it_as_granting() {
    let router = router();
    for case in cases() {
        let role_arn = role_carrying(&router, case.name, case.role_name, case.policy_arn).await;
        let (status, body) = simulate(
            &router,
            &role_arn,
            &[case.action],
            &[case.resource_arn.as_str()],
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected SimulatePrincipalPolicy 200, got {status}: {body}",
            case.name
        );
        assert_eq!(
            reported(&body),
            vec![(case.action.to_string(), case.expected.to_string())],
            "case {:?}: expected {} for {} on {}, got {body}",
            case.name,
            case.expected,
            case.action,
            case.resource_arn
        );
    }
}
