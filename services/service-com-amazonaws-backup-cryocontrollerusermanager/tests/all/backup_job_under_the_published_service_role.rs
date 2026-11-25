// SPDX-License-Identifier: CC0-1.0

//! What a backup job COMES TO when it runs under the role AWS publishes for AWS Backup —
//! the managed policy a client attaches rather than a document it wrote itself.
//!
//! RULE under test: the backup is performed under the `IamRoleArn` the request named, so the
//! job's terminal state is decided by what the documents in force on that role allow; a role
//! carrying the AWS-managed policy AWS publishes for the BACKUP half of its service role is
//! allowed the action that creates the recovery point of EVERY protected-resource type AWS
//! Backup supports, so a job started under it completes and lands its recovery point,
//! whichever type it was started for. The per-type grants are what decide it: the same role
//! carrying only the RESTORES half of the pair, or no permissions policy at all, is allowed no
//! creating action, so its job fails with the denial naming the action that type's backup
//! needs.
//!
//! Source for the grants: the per-resource-type grants AWS publishes for the backup role (AWS
//! Backup, *Managed policies*, `AWSBackupServiceRolePolicyForBackup` and
//! `AWSBackupServiceRolePolicyForRestores`) — of each type's grants, the one action that
//! CREATES the recovery point, which is the call a role without it is denied.
//!
//! The rows span what the rule generalises over: the protected-resource TYPE the job is
//! started for — a DynamoDB table, an EBS volume, an EC2 instance, an EFS file system, an FSx
//! file system, an Amazon RDS instance, an Aurora cluster, a Redshift cluster and a Timestream
//! table, each resolved to its Backup resource type from its own ARN — and, for two of those
//! types, the document in force on the role (the published BACKUP policy, the published
//! RESTORES policy, and nothing). An Amazon DocumentDB and an Amazon Neptune cluster are the
//! Aurora row: all three are addressed in the Amazon RDS namespace and their recovery point is
//! created by the one cluster-snapshot action, whichever engine fronts the cluster.
//!
//! The decision is put to IAM over the emulator's own in-process callback, so IAM is mounted
//! beside Backup on one engine and every role and attachment below is made through IAM's OWN
//! wire: what decides each job is what IAM persisted and what its catalog publishes for the
//! policy, not a copy of either.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::plan_and_selection_refusals::{create_role, form_value, router_with_iam};
use crate::vault_and_backup_job::{
    call, create_vault, settled_backup_job, start_job, string_at, ACCOUNT, REGION,
};

/// The vault every job of this suite is started into.
const VAULT: &str = "published-role-vault";

/// The service principal a backup role admits, so every row's call is accepted and only the
/// role's PERMISSIONS decide the outcome. Source: AWS Backup's service principal, as the
/// trust policy of its service role carries it.
const SERVICE_PRINCIPAL: &str = "backup.amazonaws.com";

/// The two terminal `BackupJobState` values a started job reaches here, spelled out rather
/// than imported so a wrong constant in the implementation cannot make this suite pass (model
/// enum `BackupJobState`).
const COMPLETED: &str = "COMPLETED";
const FAILED: &str = "FAILED";

/// The `MessageCategory` each of those two outcomes reports (the example strings the model's
/// own documentation of `DescribeBackupJobOutput$MessageCategory` gives).
const SUCCESS_CATEGORY: &str = "SUCCESS";
const ACCESS_DENIED_CATEGORY: &str = "AccessDenied";

/// The ARNs of the two halves of the service role AWS publishes for AWS Backup, as the AWS
/// Managed Policy Reference spells them.
///
/// The backup half is `pub(crate)` for the sibling suite covering which FAILURE CLASS a job
/// settles in (`backup_job_failure_class.rs`): the role AWS's own instructions provision is
/// named by this one ARN wherever a suite needs it.
pub(crate) const BACKUP_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup";
const RESTORES_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForRestores";

/// What is in force on a row's role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InForce {
    /// The published BACKUP half attached to the role.
    PublishedBackup,
    /// The published RESTORES half attached to the role — the sibling policy, which grants
    /// each type's RESTORING action and none of the creating ones.
    PublishedRestores,
    /// No permissions policy at all: the role carries only its trust document.
    Nothing,
}

/// One row: the protected resource a job is started for, the action its backup performs under
/// the role, what is in force on that role, and what the job must come to.
struct Case {
    /// What this row is about, named in every assertion of it.
    name: String,
    /// The IAM role name the row creates, unique so each row's documents are its own.
    role_name: String,
    /// The protected resource the job backs up, unique per row so the vault's recovery points
    /// can be filtered down to this row's.
    resource_arn: String,
    /// The action the backup of that resource performs under the role, which a denial has to
    /// name.
    creating_action: &'static str,
    in_force: InForce,
    expected_state: &'static str,
}

/// An ARN of this emulator's account in this suite's Region.
fn resource(namespace: &str, resource: &str) -> String {
    format!("arn:aws:{namespace}:{REGION}:{ACCOUNT}:{resource}")
}

/// (what the row is about, the protected resource, the action its backup performs) — one
/// entry per protected-resource type the published backup policy has to cover.
fn protected_resources() -> Vec<(&'static str, String, &'static str)> {
    vec![
        (
            "a DynamoDB table",
            resource("dynamodb", "table/orders"),
            "dynamodb:CreateBackup",
        ),
        (
            "an EBS volume",
            resource("ec2", "volume/vol-0123456789abcdef0"),
            "ec2:CreateSnapshot",
        ),
        (
            "an EC2 instance",
            resource("ec2", "instance/i-0123456789abcdef0"),
            "ec2:CreateImage",
        ),
        (
            "an EFS file system",
            resource("elasticfilesystem", "file-system/fs-0123456789abcdef0"),
            "elasticfilesystem:Backup",
        ),
        (
            "an FSx file system",
            resource("fsx", "file-system/fs-0123456789abcdef1"),
            "fsx:CreateBackup",
        ),
        (
            "an Amazon RDS instance",
            resource("rds", "db:orders-instance"),
            "rds:CreateDBSnapshot",
        ),
        (
            "an Aurora cluster",
            resource("rds", "cluster:orders-cluster"),
            "rds:CreateDBClusterSnapshot",
        ),
        (
            "a Redshift cluster",
            resource("redshift", "cluster:orders-warehouse"),
            "redshift:CreateClusterSnapshot",
        ),
        (
            "a Timestream table",
            resource("timestream", "database/orders/table/events"),
            "timestream:StartAwsBackupJob",
        ),
    ]
}

/// The rows: every protected-resource type under the published BACKUP policy, plus the two
/// controls — the sibling RESTORES policy and no policy at all — on two of those types, so a
/// row that completes cannot be completing because nothing is decided.
fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (index, (what, resource_arn, action)) in protected_resources().into_iter().enumerate() {
        cases.push(Case {
            name: format!("{what} under the published backup policy"),
            role_name: format!("published-backup-{index}"),
            resource_arn,
            creating_action: action,
            in_force: InForce::PublishedBackup,
            expected_state: COMPLETED,
        });
    }
    for (index, (what, namespace, resource_part, action)) in [
        (
            "a DynamoDB table",
            "dynamodb",
            "table/controls",
            "dynamodb:CreateBackup",
        ),
        (
            "an Aurora cluster",
            "rds",
            "cluster:controls",
            "rds:CreateDBClusterSnapshot",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        for (label, in_force) in [
            ("the published restores policy", InForce::PublishedRestores),
            ("no permissions policy", InForce::Nothing),
        ] {
            cases.push(Case {
                name: format!("{what} under {label}"),
                role_name: format!("published-control-{index}-{}", cases.len()),
                resource_arn: resource(namespace, &format!("{resource_part}-{}", cases.len())),
                creating_action: action,
                in_force,
                expected_state: FAILED,
            });
        }
    }
    cases
}

/// A trust document admitting [`SERVICE_PRINCIPAL`], so every row's role is one AWS Backup
/// can assume and only its permissions decide the job's outcome.
fn backup_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": SERVICE_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Attach the managed policy `policy_arn` to the role `role_name` through IAM's OWN awsQuery
/// wire, so what the decision reads is the attachment IAM recorded.
///
/// `pub(crate)` for the sibling suite covering which FAILURE CLASS a job settles in
/// (`backup_job_failure_class.rs`): a role whose permissions come from an ATTACHED policy is
/// provisioned by exactly this call, so both suites make it one way rather than two.
pub(crate) async fn attach_role_policy(
    router: &MultiServiceRouter,
    role_name: &str,
    policy_arn: &str,
) {
    let body = format!(
        "Action=AttachRolePolicy&Version=2010-05-08&RoleName={}&PolicyArn={}",
        form_value(role_name),
        form_value(policy_arn)
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let xml = String::from_utf8(response.into_body().to_vec()).expect("a UTF-8 response body");
    assert_eq!(
        status, 200,
        "setup {role_name:?}: expected AttachRolePolicy 200 for {policy_arn:?}, got \
         {status} {xml}"
    );
}

/// The `RecoveryPointArn`s the vault holds for `resource_arn`.
async fn points_of(router: &MultiServiceRouter, case: &str, resource_arn: &str) -> Vec<String> {
    let path = format!("/backup-vaults/{VAULT}/recovery-points?resourceArn={resource_arn}");
    let (status, listed) = call(router, "GET", &path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListRecoveryPointsByBackupVault 200, got {status} {listed:?}"
    );
    listed["RecoveryPoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| string_at(point, "RecoveryPointArn"))
        .collect()
}

/// RULE: a role carrying the managed policy AWS publishes for the backup half of its service
/// role may create the recovery point of every protected-resource type AWS Backup supports,
/// so a job started under it COMPLETES and lands its point for each of those types; a role
/// carrying only the restore half, or nothing, may create none, so its job FAILS with the
/// denial naming that type's creating action and the vault keeps no point for it.
#[tokio::test]
async fn a_job_under_the_published_backup_policy_completes_for_every_supported_type() {
    let router = router_with_iam();
    let (status, created) = create_vault(&router, VAULT, "{}").await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBackupVault 200, got {status} {created:?}"
    );

    let cases = cases();
    // Every job is started FIRST and read afterwards, so the suite waits out the settle span
    // once rather than once per row.
    let mut started: Vec<(usize, String, String)> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let role_arn = create_role(&router, "/", &case.role_name, &backup_trust()).await;
        match case.in_force {
            InForce::PublishedBackup => {
                attach_role_policy(&router, &case.role_name, BACKUP_POLICY_ARN).await;
            }
            InForce::PublishedRestores => {
                attach_role_policy(&router, &case.role_name, RESTORES_POLICY_ARN).await;
            }
            InForce::Nothing => {}
        }
        let body = json!({
            "BackupVaultName": VAULT,
            "ResourceArn": case.resource_arn,
            "IamRoleArn": role_arn,
        })
        .to_string();
        let (status, job) = start_job(&router, &body).await;
        // The call is ACCEPTED whatever the role allows: a backup that cannot be performed
        // fails later, and this response is not where that shows.
        assert_eq!(
            status, 200,
            "case {:?}: expected StartBackupJob 200, got {status} {job:?}",
            case.name
        );
        started.push((index, string_at(&job, "BackupJobId"), role_arn));
    }

    for (index, job_id, role_arn) in started {
        let case = &cases[index];
        let settled = settled_backup_job(&router, &case.name, &job_id).await;
        let state = string_at(&settled, "State");
        assert_eq!(
            state, case.expected_state,
            "case {:?}: expected the job to settle {:?}, got {state:?} in {settled:?}",
            case.name, case.expected_state
        );
        let category = string_at(&settled, "MessageCategory");
        let message = string_at(&settled, "StatusMessage");
        let points = points_of(&router, &case.name, &case.resource_arn).await;
        if case.expected_state == COMPLETED {
            assert_eq!(
                category, SUCCESS_CATEGORY,
                "case {:?}: a completed job reports the success category, got {category:?} in \
                 {settled:?}",
                case.name
            );
            assert!(
                message.is_empty(),
                "case {:?}: a completed job carries no status message, got {message:?}",
                case.name
            );
            assert_eq!(
                points.len(),
                1,
                "case {:?}: a completed job lands exactly one recovery point, got {points:?}",
                case.name
            );
            continue;
        }
        assert_eq!(
            category, ACCESS_DENIED_CATEGORY,
            "case {:?}: a job its role may not perform reports the denial category, got \
             {category:?} in {settled:?}",
            case.name
        );
        // The denial is about the action THIS type's backup performs, under the role the
        // request named: both are what a client repairing the role has to be told.
        assert!(
            message.contains(case.creating_action) && message.contains(&role_arn),
            "case {:?}: the denial must name {:?} and {role_arn:?}, got {message:?}",
            case.name,
            case.creating_action
        );
        assert!(
            points.is_empty(),
            "case {:?}: a job that captured nothing leaves no recovery point, got {points:?}",
            case.name
        );
    }
}
