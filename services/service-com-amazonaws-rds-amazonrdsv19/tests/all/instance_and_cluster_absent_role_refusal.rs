// SPDX-License-Identifier: CC0-1.0

//! What a role-association call answers for a `RoleArn` this account's IAM holds NO role
//! under.
//!
//! The rule under test: **a role integration Amazon RDS cannot perform is refused
//! `InvalidParameterValue` with ONE fixed sentence and records nothing — and a role the
//! service cannot act under because IAM holds no role under the name the ARN carries is that
//! same refusal as one whose trust policy admits another service principal.** The question is
//! asked of IAM's state at the time of the call, so a role that was created and then deleted
//! is refused exactly as one that was never created, while a role IAM does hold and whose trust
//! policy admits `rds.amazonaws.com` is associated and reported `ACTIVE`.
//!
//! Smithy (`smithy_query(query="operation",operation="AddRoleToDBInstance")`):
//! `AddRoleToDBInstance` takes `DBInstanceIdentifier`, `RoleArn` and `FeatureName`, all
//! required, and models `DBInstanceNotFoundFault` (404),
//! `DBInstanceRoleAlreadyExistsFault`, `DBInstanceRoleQuotaExceededFault` and
//! `InvalidDBInstanceStateFault` (400) — not one of which covers a role that does not exist,
//! so that condition is the query protocol's service-common 400 client refusal.
//!
//! Why it matters: a caller handed a DB instance whose S3 integration names a role nobody
//! created is handed a database the feature can never work on, and the ARN it got wrong is
//! never reported back to it. The sibling suite `iam_role_association_refusals.rs` holds the
//! trust-policy half of the same rule; this one holds the EXISTENCE half, which the one shared
//! reading of a caller-supplied role ARN answers for both operations.
//!
//! The rows span how the named role fails to be one IAM holds for this account — a name
//! nothing created, the same name under an IAM path, and the ARN of a role IAM held and holds
//! no longer — and each is driven against a DB instance and a DB cluster, whose two handlers
//! ask that one question. Every call goes over the wire against this service mounted beside
//! the emulated IAM the decision resolves against, and the sentence is spelled out here rather
//! than taken from the code under test, so this table pins the wording.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_error, expect_ok, text, REGION};
use crate::rds_s3_integration::{
    associated_roles, available_cluster, available_instance_of, emulator, iam, role,
};

/// The sentence a role Amazon RDS cannot act under is refused with. Entirely FIXED text: it
/// carries neither the role ARN the request named nor why the service cannot assume it.
const UNTRUSTED_ROLE: &str = "IAM role ARN value is invalid or does not include the \
                              required permissions for: AWS_ROLE_INTEGRATION";

/// The service principal a role Amazon RDS acts under for a resource's feature must trust.
const RDS_PRINCIPAL: &str = "rds.amazonaws.com";

/// The feature every association below names: one the engines these resources run publish in
/// their `SupportedFeatureNames`, so no row is refused for the feature instead of the role.
const FEATURE: &str = "s3Import";

/// The `Status` an association that holds reports.
const ACTIVE: &str = "ACTIVE";

/// The resources the calls address, and the roles IAM is asked about.
const INSTANCE: &str = "emu-absent-role-instance";
const CLUSTER: &str = "emu-absent-role-cluster";
const HELD_ROLE: &str = "emu-absent-role-held";
const WITHDRAWN_ROLE: &str = "emu-absent-role-withdrawn";

/// An IAM role ARN of the account this emulator presents, over the resource field a row
/// states — the form `RoleArn` is documented with
/// (`arn:aws:iam::123456789012:role/AccessRole`).
fn role_arn(resource: &str) -> String {
    format!("arn:aws:iam::{}:{resource}", engine::emulator_account_id())
}

/// The query parameters of one association call.
fn association(member: &str, resource: &str, role_arn: &str) -> Vec<(String, String)> {
    vec![
        (member.to_string(), resource.to_string()),
        ("RoleArn".to_string(), role_arn.to_string()),
        ("FeatureName".to_string(), FEATURE.to_string()),
    ]
}

/// The `(RoleArn, FeatureName, Status)` triples the DB instance reports.
async fn instance_roles(
    router: &MultiServiceRouter,
    case: &str,
) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", INSTANCE)],
        )
        .await,
    );
    associated_roles(&described)
}

/// The same triples the DB cluster reports.
async fn cluster_roles(router: &MultiServiceRouter, case: &str) -> Vec<(String, String, String)> {
    let described = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", CLUSTER)],
        )
        .await,
    );
    associated_roles(&described)
}

#[tokio::test]
async fn a_role_iam_holds_nothing_under_is_refused_like_one_rds_cannot_assume() {
    let (router, _engine) = emulator();
    available_instance_of(&router, INSTANCE, "postgres", "db.t3.micro", 20, None).await;
    available_cluster(&router, CLUSTER, "aurora-postgresql").await;

    // The role IAM DOES hold, trusting the principal Amazon RDS acts under: the positive
    // control, so what the rows below pin is the absence of a role rather than a surface that
    // refuses every ARN.
    let held = role(&router, HELD_ROLE, RDS_PRINCIPAL, None).await;
    // And a role of the same shape that IAM held and holds no longer, so the answer tracks
    // that service's state rather than the form of the value.
    let withdrawn = role(&router, WITHDRAWN_ROLE, RDS_PRINCIPAL, None).await;
    iam(&router)
        .delete_role()
        .role_name(WITHDRAWN_ROLE)
        .send()
        .await
        .unwrap_or_else(|error| panic!("IAM deletes the role {WITHDRAWN_ROLE}: {error}"));

    let rows: Vec<(&str, String)> = vec![
        (
            "a role name nothing created",
            role_arn("role/emu-absent-role-never-created"),
        ),
        (
            "a role name under an IAM path nothing created",
            role_arn("role/service-role/emu-absent-role-never-created"),
        ),
        (
            "the ARN of a role IAM held and holds no longer",
            withdrawn,
        ),
    ];
    for (row, absent) in &rows {
        for (action, member, resource) in [
            ("AddRoleToDBInstance", "DBInstanceIdentifier", INSTANCE),
            ("AddRoleToDBCluster", "DBClusterIdentifier", CLUSTER),
        ] {
            let case = format!("{action} in {REGION} naming {row}");
            let body = expect_error(
                &case,
                call(&router, action, &association(member, resource, absent)).await,
                400,
                "InvalidParameterValue",
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, UNTRUSTED_ROLE,
                "case {case:?}: expected the message {UNTRUSTED_ROLE:?}, got {message:?}"
            );
        }
    }

    // None of the refusals recorded anything on either resource.
    let reported = instance_roles(&router, "the instance after the refusals").await;
    assert!(
        reported.is_empty(),
        "expected no association on the DB instance, got {reported:?}"
    );
    let reported = cluster_roles(&router, "the cluster after the refusals").await;
    assert!(
        reported.is_empty(),
        "expected no association on the DB cluster, got {reported:?}"
    );

    // And the role IAM holds, under the trust Amazon RDS needs, is associated with both.
    for (action, member, resource) in [
        ("AddRoleToDBInstance", "DBInstanceIdentifier", INSTANCE),
        ("AddRoleToDBCluster", "DBClusterIdentifier", CLUSTER),
    ] {
        let case = format!("{action} naming the role IAM holds");
        expect_ok(
            &case,
            call(&router, action, &association(member, resource, &held)).await,
        );
    }
    let wanted = vec![(held.clone(), FEATURE.to_string(), ACTIVE.to_string())];
    let reported = instance_roles(&router, "the instance after the association").await;
    assert_eq!(
        reported, wanted,
        "expected the DB instance's associations {wanted:?}, got {reported:?}"
    );
    let reported = cluster_roles(&router, "the cluster after the association").await;
    assert_eq!(
        reported, wanted,
        "expected the DB cluster's associations {wanted:?}, got {reported:?}"
    );
}
