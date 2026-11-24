// SPDX-License-Identifier: CC0-1.0

//! The two grants of the published AWS Backup service-role policy that the per-type table of
//! `tests/all/published_service_role_policy_grants.rs` does not carry, read through IAM's own
//! `SimulatePrincipalPolicy`.
//!
//! RULE under test: an entry of AWS's own managed-policy catalog carries the document AWS
//! publishes for that policy, so the permissions in force on a role it is attached to are the
//! ones that document grants — and the service-role policy AWS publishes for a backup role
//! grants, beside each protected-resource type's creating action, the ACCOUNT-WIDE tagging-API
//! read that resolves a tag-based backup selection, while naming no other action of that
//! tagging API.
//!
//! `tests/all/published_service_role_policy_grants.rs` owns the table over the resource types
//! and over the published PAIR (each half grants its own actions and not its sibling's); this
//! suite is its complement and deliberately repeats none of those rows. What it adds is the
//! grant that is not per-type at all — `tag:GetResources`, which AWS Backup makes under the
//! role to resolve a selection stated BY TAG rather than by ARN (the decision
//! `rejected_tag_selection_role` of
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/src/state.rs` puts to IAM)
//! — the one per-type creating action that table leaves out, and the neighbouring actions of
//! the SAME tagging API the document does not name, so a row reporting `allowed` cannot be
//! reporting it because everything is.
//!
//! SOURCE for the grants: the per-resource-type grants AWS publishes for the backup role (AWS
//! Backup, *Managed policies*, `AWSBackupServiceRolePolicyForBackup`).
//!
//! The awsQuery harness, the simulation request builder and the decision reader are the ones
//! `tests/all/user_lifecycle.rs` and `tests/all/policy_simulation.rs` own.

use smithy_server_runtime::MultiServiceRouter;

use crate::policy_simulation::{reported, simulate, ALLOWED, IMPLICIT_DENY};
use crate::user_lifecycle::{account_id, call, element, router};

/// The policy AWS publishes for the backup half of its service role.
const BACKUP_SERVICE_ROLE_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup";

/// The service principal such a role trusts, since AWS Backup itself assumes it. Nothing here
/// depends on it; a role is created with a trust document because `CreateRole` requires one.
const BACKUP_PRINCIPAL: &str = "backup.amazonaws.com";

/// The role every row simulates against.
const ROLE_NAME: &str = "aws-backup-tag-reading-role";

/// The Region a protected resource of the account lives in. Nothing here is Region-scoped —
/// IAM is global and the published document scopes its grants to `*`.
const REGION: &str = "us-west-2";

/// One row: the action asked about, the resource the request names, and the decision the
/// published document owes it.
struct Row {
    case: &'static str,
    action: &'static str,
    /// The resource the action would be performed on, as that service spells its ARN.
    resource: fn(&str) -> String,
    expected: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the account-wide tagging read that resolves a tag-based selection",
            action: "tag:GetResources",
            resource: |_account| "*".to_string(),
            expected: ALLOWED,
        },
        Row {
            case: "a storage gateway volume's snapshot",
            action: "storagegateway:CreateSnapshot",
            resource: |account| {
                format!(
                    "arn:aws:storagegateway:{REGION}:{account}:gateway/sgw-01234567/volume/\
                     vol-01234567"
                )
            },
            expected: ALLOWED,
        },
        Row {
            case: "a tagging-API read of the account's compliance the document does not name",
            action: "tag:GetComplianceSummary",
            resource: |_account| "*".to_string(),
            expected: IMPLICIT_DENY,
        },
        Row {
            case: "a tagging-API WRITE the document does not name",
            action: "tag:TagResources",
            resource: |account| format!("arn:aws:dynamodb:{REGION}:{account}:table/orders"),
            expected: IMPLICIT_DENY,
        },
    ]
}

/// The role a client provisions the way AWS documents: trusted by AWS Backup, carrying the
/// published service-role policy and NOTHING else, so every decision below is that document's.
async fn backup_service_role(router: &MultiServiceRouter) -> String {
    let trust = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": BACKUP_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = call(
        router,
        "CreateRole",
        &[
            ("Path", "/service-role/"),
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole must succeed, got {status}: {created}"
    );
    let arn = element(&created, "Arn");
    let (status, attached) = call(
        router,
        "AttachRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyArn", BACKUP_SERVICE_ROLE_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "AttachRolePolicy must succeed, got {status}: {attached}"
    );
    arn
}

/// A role carrying only the published backup service-role policy is allowed the tagging-API
/// read a tag-based selection needs and the gateway volume's creating action, and no other
/// action of that tagging API.
#[tokio::test]
async fn the_published_backup_policy_grants_the_tag_read_a_selection_needs() {
    let router = router();
    let role_arn = backup_service_role(&router).await;
    let account = account_id();
    for row in rows() {
        let resource = (row.resource)(account);
        let (status, body) = simulate(&router, &role_arn, &[row.action], &[&resource], &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: SimulatePrincipalPolicy must answer 200, got {status}: {body}",
            row.case
        );
        let decisions = reported(&body);
        assert_eq!(
            decisions,
            vec![(row.action.to_string(), row.expected.to_string())],
            "case {:?}: expected {} for {} on {resource}, got {decisions:?}",
            row.case,
            row.expected,
            row.action
        );
    }
}
