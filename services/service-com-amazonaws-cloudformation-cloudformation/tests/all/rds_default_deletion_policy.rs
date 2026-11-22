// SPDX-License-Identifier: CC0-1.0

//! The DEFAULT a declaration that states no `DeletionPolicy` gets, per resource TYPE, end to
//! end over the awsQuery wire.
//!
//! RULE under test: **a resource whose declaration states no `DeletionPolicy` is torn down
//! under the default its own TYPE documents. That default is `Delete` for almost every type,
//! and the exceptions are the ones AWS documents another default for: "for
//! `AWS::RDS::DBCluster` resources, the default policy is Snapshot. For `AWS::RDS::DBInstance`
//! resources that don't specify the `DBClusterIdentifier` property, the default policy is
//! Snapshot."** A policy the declaration DOES state wins over the default either way, which is
//! the half [`crate::resource_deletion_policies`] tables.
//!
//! Why it matters: a client that deploys a database by template and never names a policy is
//! relying on the type's own default for whether its data survives the stack. Taking `Delete`
//! for every type throws away the final snapshot such a stack is owed and makes the
//! default-policy contract unobservable; taking `Snapshot` for every type would leave a
//! snapshot behind for resources AWS deletes outright.
//!
//! The rows differ along what the rule generalises over: the TYPE declared (a DB instance, a DB
//! cluster, and an `AWS::SNS::Topic` as a type AWS documents no other default for), the
//! PROPERTY that decides a DB instance's default (whether it names the cluster it is a member
//! of), and whether the declaration states a policy at all — so the row that states `Delete`
//! shows the stated policy still winning over a type whose default is `Snapshot`.
//!
//! Every case drives the real request path for both services through one router, so what is
//! asserted is what a client that deploys by template and then calls `DescribeDBSnapshots`,
//! `DescribeDBClusterSnapshots`, `DescribeDBInstances`, `DescribeDBClusters` or
//! `GetTopicAttributes` observes.

use smithy_server_runtime::MultiServiceRouter;

use crate::resource_deletion_policies::manual_snapshots_of;
use crate::stack_lifecycle::{
    call, create_stack, member, rds, router, settled_stack, sns, xml_blocks, xml_text,
};
use crate::stack_resources::sns_arn;
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// One stack torn down with no policy stated on the resource under test (or with `Delete`
/// stated, for the row that shows a stated policy winning).
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The DB instance the row declares, if any, and whether its teardown must leave a manual
    /// DB snapshot of it behind.
    instance: Option<(&'static str, bool)>,
    /// The DB cluster the row declares, if any, and whether its teardown must leave a manual
    /// DB CLUSTER snapshot of it behind.
    cluster: Option<(&'static str, bool)>,
    /// The SNS topic the row declares, if any. A topic's owning service takes no snapshot on
    /// the way out at all, so there is nothing to look for beside its removal.
    topic: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a DB instance that is no cluster member, stating no policy",
        stack: "default-policy-standalone",
        template: concat!(
            "Resources:\n",
            "  Db:\n",
            "    Type: AWS::RDS::DBInstance\n",
            "    Properties:\n",
            "      DBInstanceIdentifier: default-policy-standalone-db\n",
            "      Engine: mysql\n",
            "      DBInstanceClass: db.t3.micro\n",
            "      AllocatedStorage: 20\n",
        ),
        instance: Some(("default-policy-standalone-db", true)),
        cluster: None,
        topic: None,
    },
    Row {
        case: "a DB instance that NAMES the cluster it is a member of, stating no policy",
        stack: "default-policy-member",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::RDS::DBCluster\n",
            // Stated outright, so this row is about the MEMBER's default alone — and it shows
            // a stated policy winning over this type's own Snapshot default.
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      DBClusterIdentifier: default-policy-member-cluster\n",
            "      Engine: aurora-postgresql\n",
            "      MasterUsername: dbadmin\n",
            "      MasterUserPassword: an-example-password\n",
            "  Db:\n",
            "    Type: AWS::RDS::DBInstance\n",
            "    Properties:\n",
            "      DBInstanceIdentifier: default-policy-member-db\n",
            "      Engine: aurora-postgresql\n",
            "      DBInstanceClass: db.r5.large\n",
            "      DBClusterIdentifier: !Ref Cluster\n",
        ),
        instance: Some(("default-policy-member-db", false)),
        cluster: Some(("default-policy-member-cluster", false)),
        topic: None,
    },
    Row {
        case: "a DB cluster stating no policy",
        stack: "default-policy-cluster",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::RDS::DBCluster\n",
            "    Properties:\n",
            "      DBClusterIdentifier: default-policy-cluster-db\n",
            "      Engine: aurora-mysql\n",
            "      MasterUsername: dbadmin\n",
            "      MasterUserPassword: an-example-password\n",
        ),
        instance: None,
        cluster: Some(("default-policy-cluster-db", true)),
        topic: None,
    },
    Row {
        case: "a type AWS documents no other default for, stating no policy",
        stack: "default-policy-topic",
        template: concat!(
            "Resources:\n",
            "  Topic:\n",
            "    Type: AWS::SNS::Topic\n",
            "    Properties:\n",
            "      TopicName: default-policy-topic-name\n",
        ),
        instance: None,
        cluster: None,
        topic: Some("default-policy-topic-name"),
    },
    Row {
        case: "a DB instance stating Delete, against this type's Snapshot default",
        stack: "default-policy-stated",
        template: concat!(
            "Resources:\n",
            "  Db:\n",
            "    Type: AWS::RDS::DBInstance\n",
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      DBInstanceIdentifier: default-policy-stated-db\n",
            "      Engine: mysql\n",
            "      DBInstanceClass: db.t3.micro\n",
            "      AllocatedStorage: 20\n",
        ),
        instance: Some(("default-policy-stated-db", false)),
        cluster: None,
        topic: None,
    },
];

/// Whether Amazon RDS still holds the DB instance `identifier`, with the body it answered.
async fn instance_is_there(router: &MultiServiceRouter, identifier: &str) -> (bool, String) {
    let (code, body) = rds(
        router,
        "DescribeDBInstances",
        &[("DBInstanceIdentifier", identifier)],
    )
    .await;
    (code == 200, body)
}

/// Whether Amazon RDS still holds the DB cluster `identifier`, with the body it answered.
async fn cluster_is_there(router: &MultiServiceRouter, identifier: &str) -> (bool, String) {
    let (code, body) = rds(
        router,
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await;
    (code == 200, body)
}

/// The `(identifier, status)` of every MANUAL cluster snapshot Amazon RDS holds of `cluster` —
/// the cluster-scoped counterpart of [`manual_snapshots_of`], since a cluster's data is the
/// cluster volume's and a snapshot of it is a DB CLUSTER snapshot.
async fn manual_cluster_snapshots_of(
    router: &MultiServiceRouter,
    cluster: &str,
) -> Vec<(String, String)> {
    let (code, body) = rds(
        router,
        "DescribeDBClusterSnapshots",
        &[
            ("DBClusterIdentifier", cluster),
            ("SnapshotType", "manual"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "DescribeDBClusterSnapshots({cluster}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "DBClusterSnapshot")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "DBClusterSnapshotIdentifier").unwrap_or_default(),
                xml_text(entry, "Status").unwrap_or_default(),
            )
        })
        .collect()
}

/// The status a snapshot the owning service has finished taking reports
/// (`DBSnapshot$Status` / `DBClusterSnapshot$Status`).
const AVAILABLE: &str = "available";

#[tokio::test]
async fn a_declaration_stating_no_deletion_policy_takes_the_default_its_type_documents() {
    for row in ROWS {
        let router = router();
        let (code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must answer 200, got {code} {created}",
            row.case
        );
        let stack_id = xml_text(&created, "StackId")
            .unwrap_or_else(|| panic!("case {:?}: expected a StackId, got {created}", row.case));
        let described = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {:?}: the stack must be created before the delete, got {described}",
            row.case
        );

        // Each subject really exists before the teardown: a row that asserts what became of
        // one has to start from one that was there.
        if let Some((identifier, _)) = row.instance {
            let (there, body) = instance_is_there(&router, identifier).await;
            assert!(
                there,
                "case {:?}: expected {identifier} to exist before the delete, got {body}",
                row.case
            );
        }
        if let Some((identifier, _)) = row.cluster {
            let (there, body) = cluster_is_there(&router, identifier).await;
            assert!(
                there,
                "case {:?}: expected {identifier} to exist before the delete, got {body}",
                row.case
            );
        }

        let (code, deleted) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: DeleteStack must answer 200, got {code} {deleted}",
            row.case
        );
        // Described by its ID from here on, which is how a completed deletion is still
        // readable.
        let described = settled_stack(&router, row.case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {:?}: expected the deletion to complete, got {described}",
            row.case
        );

        // Whatever the default says about the snapshot, the resource itself is deleted: none of
        // these defaults is `Retain`.
        if let Some((identifier, snapshotted)) = row.instance {
            let (there, body) = instance_is_there(&router, identifier).await;
            assert!(
                !there,
                "case {:?}: expected {identifier} to have been deleted, got {body}",
                row.case
            );
            let snapshots = manual_snapshots_of(&router, identifier).await;
            assert_eq!(
                snapshots.len(),
                usize::from(snapshotted),
                "case {:?}: expected a final DB snapshot of {identifier} = {snapshotted}, got \
                 {snapshots:?}",
                row.case
            );
            if let Some((_, status)) = snapshots.first() {
                assert_eq!(
                    status, AVAILABLE,
                    "case {:?}: expected the final snapshot to be available, got {snapshots:?}",
                    row.case
                );
            }
        }
        if let Some((identifier, snapshotted)) = row.cluster {
            let (there, body) = cluster_is_there(&router, identifier).await;
            assert!(
                !there,
                "case {:?}: expected {identifier} to have been deleted, got {body}",
                row.case
            );
            let snapshots = manual_cluster_snapshots_of(&router, identifier).await;
            assert_eq!(
                snapshots.len(),
                usize::from(snapshotted),
                "case {:?}: expected a final cluster snapshot of {identifier} = {snapshotted}, \
                 got {snapshots:?}",
                row.case
            );
            if let Some((_, status)) = snapshots.first() {
                assert_eq!(
                    status, AVAILABLE,
                    "case {:?}: expected the final cluster snapshot to be available, got \
                     {snapshots:?}",
                    row.case
                );
            }
        }
        if let Some(name) = row.topic {
            let (code, body) = sns(&router, "GetTopicAttributes", &[("TopicArn", &sns_arn(name))]).await;
            assert_ne!(
                code, 200,
                "case {:?}: expected {name} to have been deleted, got {code} {body}",
                row.case
            );
        }
    }
}
