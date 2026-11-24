// SPDX-License-Identifier: CC0-1.0

//! What a role is ALLOWED once one of the service-role policies AWS publishes is attached to
//! it.
//!
//! RULE under test: an entry of AWS's own catalog of managed policies carries the document AWS
//! publishes for that policy, so the permissions in force on a role it is attached to are the
//! ones that document grants — and a service-role policy AWS publishes for a service to act
//! through grants, for EVERY resource type that service supports, the action that carries out
//! the service's work on that type. The simulation of the role's policies is what reports it
//! ("determine the policies' effective permissions"), so an action the published document
//! grants is reported `allowed` and one it does not is reported `implicitDeny`, whichever
//! resource of that type the request names.
//!
//! The policies driven here are the two halves of the service role AWS publishes for AWS
//! Backup, because they are a published PAIR split along exactly that line: the backup half
//! grants, per protected-resource type, the action that CREATES that type's recovery point,
//! and the restore half grants each type's RESTORING action — so each half is asserted to
//! grant its own actions and NOT its sibling's, which is what makes the rows about the
//! documents rather than about attachment.
//!
//! SOURCE for the per-type actions: the per-resource-type grants AWS publishes for the backup
//! role (AWS Backup, *Managed policies*, `AWSBackupServiceRolePolicyForBackup` and
//! `AWSBackupServiceRolePolicyForRestores`).
//!
//! The rows span what the rule generalises over: WHICH published policy is attached, which
//! resource TYPE the action belongs to (a table, a volume, an instance, two kinds of file
//! system, a database instance, a cluster, a warehouse, a time-series table), whether the
//! request names the wildcard resource or a resource of that type, and whether the action is
//! one the document grants at all.
//!
//! The awsQuery harness, the simulation request builder and the decision reader are the ones
//! `tests/all/user_lifecycle.rs` and `tests/all/policy_simulation.rs` own, so a simulation is
//! built and read exactly one way in this crate.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{reported, simulate, ALLOWED, IMPLICIT_DENY};
use crate::user_lifecycle::{account_id, call, element, router};

/// The ARNs of the two halves of the service role AWS publishes for AWS Backup, as the AWS
/// Managed Policy Reference spells them.
const BACKUP_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup";
const RESTORES_POLICY_ARN: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForRestores";

/// A trust document admitting the service the published role is for, which is what such a role
/// carries; nothing here depends on it, and a role is created with one because `CreateRole`
/// requires it.
const BACKUP_TRUST: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"backup.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The resource every action is also simulated against, which is the resource element a policy
/// scoped to every resource carries.
const ANY_RESOURCE: &str = "*";

/// One action the backup half of the published pair grants, with a resource of the type it is
/// performed on: the action that CREATES that protected-resource type's recovery point.
fn creating_actions() -> Vec<(&'static str, &'static str, String)> {
    let account = account_id();
    vec![
        (
            "a DynamoDB table",
            "dynamodb:CreateBackup",
            format!("arn:aws:dynamodb:us-east-1:{account}:table/orders"),
        ),
        (
            "an EBS volume",
            "ec2:CreateSnapshot",
            format!("arn:aws:ec2:us-east-1:{account}:volume/vol-0123456789abcdef0"),
        ),
        (
            "an EC2 instance",
            "ec2:CreateImage",
            format!("arn:aws:ec2:us-east-1:{account}:instance/i-0123456789abcdef0"),
        ),
        (
            "an EFS file system",
            "elasticfilesystem:Backup",
            format!("arn:aws:elasticfilesystem:us-east-1:{account}:file-system/fs-0123456789abcdef0"),
        ),
        (
            "an FSx file system",
            "fsx:CreateBackup",
            format!("arn:aws:fsx:us-east-1:{account}:file-system/fs-0123456789abcdef1"),
        ),
        (
            "a database instance",
            "rds:CreateDBSnapshot",
            format!("arn:aws:rds:us-east-1:{account}:db:orders-instance"),
        ),
        (
            "a database cluster",
            "rds:CreateDBClusterSnapshot",
            format!("arn:aws:rds:us-east-1:{account}:cluster:orders-cluster"),
        ),
        (
            "a Redshift cluster",
            "redshift:CreateClusterSnapshot",
            format!("arn:aws:redshift:us-east-1:{account}:cluster:orders-warehouse"),
        ),
        (
            "a Timestream table",
            "timestream:StartAwsBackupJob",
            format!("arn:aws:timestream:us-east-1:{account}:database/orders/table/events"),
        ),
    ]
}

/// One action the RESTORE half grants, per resource type it restores — the sibling of each
/// creating action above, which is what makes "each half grants its own" assertable.
const RESTORING_ACTIONS: &[(&str, &str)] = &[
    ("a DynamoDB table", "dynamodb:RestoreTableFromBackup"),
    ("an EBS volume", "ec2:CreateVolume"),
    ("an EFS file system", "elasticfilesystem:CreateFilesystem"),
    (
        "a database instance",
        "rds:RestoreDBInstanceFromDBSnapshot",
    ),
    (
        "a storage gateway volume",
        "storagegateway:CreateStorediSCSIVolume",
    ),
];

/// Actions NEITHER published half grants, so a row that reports `allowed` cannot be reporting
/// it because everything is: a destructive call on each of the services the pair does name.
const UNGRANTED_ACTIONS: &[&str] = &[
    "dynamodb:DeleteTable",
    "ec2:TerminateInstances",
    "rds:DeleteDBInstance",
];

/// Create the role `name` with [`BACKUP_TRUST`] and attach the managed policy `policy_arn` to
/// it, answering the ARN the account holds the role under — both through IAM's own awsQuery
/// wire, so what the simulation reads is what those calls persisted.
async fn role_with(router: &MultiServiceRouter, name: &str, policy_arn: &str) -> String {
    let (status, created) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", BACKUP_TRUST),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: CreateRole must succeed, got {status}: {created}"
    );
    let arn = element(&created, "Arn");
    let (status, attached) = call(
        router,
        "AttachRolePolicy",
        &[("RoleName", name), ("PolicyArn", policy_arn)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: AttachRolePolicy {policy_arn:?} must succeed, got {status}: {attached}"
    );
    arn
}

/// The decision the simulation reports for `action` on `resource` under `role_arn`.
async fn decision(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> String {
    let (status, body) = simulate(router, role_arn, &[action], &[resource], &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SimulatePrincipalPolicy must answer 200, got {status}: {body}"
    );
    let results = reported(&body);
    assert_eq!(
        results.len(),
        1,
        "case {case:?}: one action asked about is one result, got {results:?}"
    );
    results[0].1.clone()
}

/// RULE: the role carrying the published BACKUP half is allowed the action that creates each
/// supported type's recovery point — on the wildcard resource and on a resource of that type
/// alike — and is allowed neither the restore half's actions nor an action neither half
/// grants.
#[tokio::test]
async fn the_published_backup_policy_allows_every_types_creating_action() {
    let router = router();
    let role_arn = role_with(&router, "published-backup-role", BACKUP_POLICY_ARN).await;

    for (what, action, resource) in creating_actions() {
        for (where_asked, resource) in [
            ("every resource", ANY_RESOURCE.to_string()),
            ("a resource of that type", resource),
        ] {
            let case = format!("{what}: {action} on {where_asked}");
            let decided = decision(&router, &case, &role_arn, action, &resource).await;
            assert_eq!(
                decided, ALLOWED,
                "case {case:?}: the published backup policy grants {action:?}, so the \
                 simulation must report {ALLOWED:?}, got {decided:?}"
            );
        }
    }

    for (what, action) in RESTORING_ACTIONS {
        let case = format!("{what}: the restore half's {action} under the backup half");
        let decided = decision(&router, &case, &role_arn, action, ANY_RESOURCE).await;
        assert_eq!(
            decided, IMPLICIT_DENY,
            "case {case:?}: the backup half grants no restoring action, so the simulation \
             must report {IMPLICIT_DENY:?}, got {decided:?}"
        );
    }

    for action in UNGRANTED_ACTIONS {
        let case = format!("an action neither half grants: {action}");
        let decided = decision(&router, &case, &role_arn, action, ANY_RESOURCE).await;
        assert_eq!(
            decided, IMPLICIT_DENY,
            "case {case:?}: nothing grants it, so the simulation must report \
             {IMPLICIT_DENY:?}, got {decided:?}"
        );
    }
}

/// RULE, the other half of the published pair: the role carrying the RESTORES half is allowed
/// each type's restoring action and none of the creating ones, so the two documents are each
/// reported as what AWS publishes rather than as one grant of everything.
#[tokio::test]
async fn the_published_restores_policy_allows_every_types_restoring_action() {
    let router = router();
    let role_arn = role_with(&router, "published-restores-role", RESTORES_POLICY_ARN).await;

    for (what, action) in RESTORING_ACTIONS {
        let case = format!("{what}: {action} under the restore half");
        let decided = decision(&router, &case, &role_arn, action, ANY_RESOURCE).await;
        assert_eq!(
            decided, ALLOWED,
            "case {case:?}: the published restores policy grants {action:?}, so the \
             simulation must report {ALLOWED:?}, got {decided:?}"
        );
    }

    for (what, action, _resource) in creating_actions() {
        let case = format!("{what}: the backup half's {action} under the restore half");
        let decided = decision(&router, &case, &role_arn, action, ANY_RESOURCE).await;
        assert_eq!(
            decided, IMPLICIT_DENY,
            "case {case:?}: the restore half grants no creating action, so the simulation \
             must report {IMPLICIT_DENY:?}, got {decided:?}"
        );
    }
}
