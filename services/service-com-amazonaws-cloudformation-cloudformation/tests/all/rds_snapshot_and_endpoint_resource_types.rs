// SPDX-License-Identifier: CC0-1.0

//! The Amazon RDS SNAPSHOT and CUSTOM CLUSTER ENDPOINT resource types, end to end over the
//! awsQuery wire.
//!
//! RULE under test: **a template may declare every resource type CloudFormation knows, and
//! each declared resource is created in the service that OWNS it — so a stack declaring one of
//! the Amazon RDS types whose backing call exists (`CreateDBSnapshot`,
//! `CreateDBClusterSnapshot`, `CreateDBClusterEndpoint`, `CreateGlobalCluster`) is accepted,
//! reaches `CREATE_COMPLETE` once that service reports the resource ready, publishes the
//! identifiers that service minted, and leaves nothing behind when the stack is deleted.**
//! "Template format error: Unrecognized resource types" is reserved for a type CloudFormation
//! does not know at all.
//!
//! The rows differ along what the rule generalises over: WHICH type is declared (a snapshot of
//! a DB instance, a snapshot of a DB cluster, a custom endpoint of a DB cluster, an Aurora
//! global database), what the resource is taken OF or hangs off, and which identifier its `Ref`
//! publishes — the ARN
//! Amazon RDS minted for a snapshot, the identifier the declaration asked for for an endpoint.
//! Each row drives the real request path for BOTH services through one router — the
//! CloudFormation calls over the awsQuery wire and the Amazon RDS calls as a client's SDK makes
//! them — so what is asserted is what a client that deploys by template and then calls
//! `DescribeDBSnapshots`, `DescribeDBClusterSnapshots`, `DescribeDBClusterEndpoints` or
//! `DescribeGlobalClusters` observes.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, member, rds, resource_event_reason, router, settled_stack, xml_blocks,
    xml_text, xml_texts,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The Aurora cluster the cluster-scoped rows hang their resource off is declared inline in
/// each template below: `concat!` takes literals only, so the shared fragment is written out
/// per row. The engine is one `CreateDBClusterEndpoint` serves ("this action applies only to
/// Aurora DB clusters"), with the master credentials that create requires.
///
/// How the service that owns the row's resource is asked whether it still holds it, and which
/// of its members carries the ARN the stack must publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subject {
    /// A snapshot of a DB instance, reported by `DescribeDBSnapshots`.
    DbSnapshot,
    /// A snapshot of a DB cluster, reported by `DescribeDBClusterSnapshots`.
    ClusterSnapshot,
    /// A custom endpoint of a DB cluster, reported by `DescribeDBClusterEndpoints`.
    ClusterEndpoint,
    /// An Aurora global database, reported by `DescribeGlobalClusters`.
    GlobalCluster,
}

impl Subject {
    /// The RDS action that reports one of these, the member it is addressed by, the element
    /// one is reported as, and the member its ARN is reported under.
    fn read(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::DbSnapshot => (
                "DescribeDBSnapshots",
                "DBSnapshotIdentifier",
                "DBSnapshot",
                "DBSnapshotArn",
            ),
            Self::ClusterSnapshot => (
                "DescribeDBClusterSnapshots",
                "DBClusterSnapshotIdentifier",
                "DBClusterSnapshot",
                "DBClusterSnapshotArn",
            ),
            Self::ClusterEndpoint => (
                "DescribeDBClusterEndpoints",
                "DBClusterEndpointIdentifier",
                // The entry element of `DBClusterEndpointMessage$DBClusterEndpoints`:
                // `DBClusterEndpointList`'s member `@xmlName` (`smithy_query(query="shape",
                // shape="DBClusterEndpointList")`), which is the list's own naming rather
                // than the protocol's default `member`.
                "DBClusterEndpointList",
                "DBClusterEndpointArn",
            ),
            Self::GlobalCluster => (
                "DescribeGlobalClusters",
                "GlobalClusterIdentifier",
                // The entry element of `GlobalClustersMessage$GlobalClusters`:
                // `GlobalClusterList`'s member `@xmlName` (`smithy_query(query="shape",
                // shape="GlobalClusterList")`).
                "GlobalClusterMember",
                "GlobalClusterArn",
            ),
        }
    }

    /// The status member a created resource of this kind reports, and the value it carries
    /// once that service has finished creating it.
    fn ready_status(self) -> &'static str {
        match self {
            // `DBSnapshot$Status` / `DBClusterSnapshot$Status` / `DBClusterEndpoint$Status` /
            // `GlobalCluster$Status` all report `available` for a resource that is up.
            Self::DbSnapshot
            | Self::ClusterSnapshot
            | Self::ClusterEndpoint
            | Self::GlobalCluster => "available",
        }
    }
}

/// One stack declaring one of the types under test.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: &'static str,
    /// The logical id of the declared resource.
    logical_id: &'static str,
    /// The identifier the declaration asks the owning service for.
    identifier: &'static str,
    subject: Subject,
    /// Whether this type's `Ref` publishes the ARN Amazon RDS minted rather than the
    /// identifier the declaration asked for.
    refs_the_arn: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a DB snapshot of an instance of the same stack",
        stack: "rds-types-db-snapshot",
        template: concat!(
            "Resources:\n",
            "  Database:\n",
            "    Type: AWS::RDS::DBInstance\n",
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      Engine: mysql\n",
            "      DBInstanceClass: db.t3.micro\n",
            "      AllocatedStorage: 20\n",
            "  Snapshot:\n",
            "    Type: AWS::RDS::DBSnapshot\n",
            "    Properties:\n",
            "      DBInstanceIdentifier: !Ref Database\n",
            "      DBSnapshotIdentifier: rds-types-db-snapshot-taken\n",
            "Outputs:\n",
            "  SnapshotRef:\n",
            "    Value: !Ref Snapshot\n",
            "  SnapshotArn:\n",
            "    Value: !GetAtt Snapshot.DBSnapshotArn\n",
        ),
        logical_id: "Snapshot",
        identifier: "rds-types-db-snapshot-taken",
        subject: Subject::DbSnapshot,
        refs_the_arn: true,
    },
    Row {
        case: "a cluster snapshot of an Aurora cluster of the same stack",
        stack: "rds-types-cluster-snapshot",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::RDS::DBCluster\n",
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      Engine: aurora-mysql\n",
            "      MasterUsername: dbadmin\n",
            "      MasterUserPassword: an-example-password\n",
            "  Snapshot:\n",
            "    Type: AWS::RDS::ClusterSnapshot\n",
            "    Properties:\n",
            "      DBClusterIdentifier: !Ref Cluster\n",
            "      DBClusterSnapshotIdentifier: rds-types-cluster-snapshot-taken\n",
            "Outputs:\n",
            "  SnapshotRef:\n",
            "    Value: !Ref Snapshot\n",
            "  SnapshotArn:\n",
            "    Value: !GetAtt Snapshot.DBClusterSnapshotArn\n",
        ),
        logical_id: "Snapshot",
        identifier: "rds-types-cluster-snapshot-taken",
        subject: Subject::ClusterSnapshot,
        refs_the_arn: true,
    },
    Row {
        case: "a custom reader endpoint on an Aurora cluster of the same stack",
        stack: "rds-types-cluster-endpoint",
        template: concat!(
            "Resources:\n",
            "  Cluster:\n",
            "    Type: AWS::RDS::DBCluster\n",
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      Engine: aurora-mysql\n",
            "      MasterUsername: dbadmin\n",
            "      MasterUserPassword: an-example-password\n",
            "  Endpoint:\n",
            "    Type: AWS::RDS::DBClusterEndpoint\n",
            "    Properties:\n",
            "      DBClusterIdentifier: !Ref Cluster\n",
            "      DBClusterEndpointIdentifier: rds-types-reader-endpoint\n",
            "      EndpointType: READER\n",
            "Outputs:\n",
            "  SnapshotRef:\n",
            "    Value: !Ref Endpoint\n",
            "  SnapshotArn:\n",
            "    Value: !GetAtt Endpoint.DBClusterEndpointArn\n",
            "  Served:\n",
            "    Value: !GetAtt Endpoint.Endpoint\n",
        ),
        logical_id: "Endpoint",
        identifier: "rds-types-reader-endpoint",
        subject: Subject::ClusterEndpoint,
        refs_the_arn: false,
    },
    Row {
        case: "a custom endpoint on a cluster that sits in the stack's own DB subnet group",
        stack: "rds-types-endpoint-in-a-subnet-group",
        template: concat!(
            "Resources:\n",
            "  Vpc:\n",
            "    Type: AWS::EC2::VPC\n",
            "    Properties:\n",
            "      CidrBlock: 10.4.0.0/16\n",
            "  SubA:\n",
            "    Type: AWS::EC2::Subnet\n",
            "    Properties:\n",
            "      VpcId: !Ref Vpc\n",
            "      CidrBlock: 10.4.1.0/24\n",
            "      AvailabilityZone: !Select [0, !GetAZs \"\"]\n",
            "  SubB:\n",
            "    Type: AWS::EC2::Subnet\n",
            "    Properties:\n",
            "      VpcId: !Ref Vpc\n",
            "      CidrBlock: 10.4.2.0/24\n",
            "      AvailabilityZone: !Select [1, !GetAZs \"\"]\n",
            "  SubnetGroup:\n",
            "    Type: AWS::RDS::DBSubnetGroup\n",
            "    Properties:\n",
            "      DBSubnetGroupDescription: the stack's own subnets\n",
            "      SubnetIds:\n",
            "        - !Ref SubA\n",
            "        - !Ref SubB\n",
            "  Cluster:\n",
            "    Type: AWS::RDS::DBCluster\n",
            "    DeletionPolicy: Delete\n",
            "    Properties:\n",
            "      Engine: aurora-postgresql\n",
            "      EngineMode: provisioned\n",
            "      MasterUsername: dbadmin\n",
            "      MasterUserPassword: an-example-password\n",
            "      DBSubnetGroupName: !Ref SubnetGroup\n",
            "      ServerlessV2ScalingConfiguration:\n",
            "        MinCapacity: 0.5\n",
            "        MaxCapacity: 1\n",
            "  Endpoint:\n",
            "    Type: AWS::RDS::DBClusterEndpoint\n",
            "    Properties:\n",
            "      DBClusterIdentifier: !Ref Cluster\n",
            "      DBClusterEndpointIdentifier: rds-types-subnet-group-endpoint\n",
            "      EndpointType: ANY\n",
            "      StaticMembers: []\n",
            "      ExcludedMembers: []\n",
            "Outputs:\n",
            "  SnapshotRef:\n",
            "    Value: !Ref Endpoint\n",
            "  SnapshotArn:\n",
            "    Value: !GetAtt Endpoint.DBClusterEndpointArn\n",
            "  Served:\n",
            "    Value: !GetAtt Endpoint.Endpoint\n",
        ),
        logical_id: "Endpoint",
        identifier: "rds-types-subnet-group-endpoint",
        subject: Subject::ClusterEndpoint,
        refs_the_arn: false,
    },
    Row {
        case: "an Aurora global database declared on its own",
        stack: "rds-types-global-cluster",
        template: concat!(
            "Resources:\n",
            "  Global:\n",
            "    Type: AWS::RDS::GlobalCluster\n",
            "    Properties:\n",
            "      GlobalClusterIdentifier: rds-types-global-database\n",
            "      Engine: aurora-postgresql\n",
            "      DatabaseName: ledger\n",
            "      StorageEncrypted: true\n",
            "Outputs:\n",
            "  SnapshotRef:\n",
            "    Value: !Ref Global\n",
            "  SnapshotArn:\n",
            "    Value: !GetAtt Global.GlobalClusterArn\n",
        ),
        logical_id: "Global",
        identifier: "rds-types-global-database",
        subject: Subject::GlobalCluster,
        refs_the_arn: false,
    },
];

/// The stack's outputs, keyed by `OutputKey`: the answer carries one `OutputValue` per key, so
/// the two parallel lists zip.
fn outputs_of(case: &str, described: &str) -> BTreeMap<String, String> {
    let keys = xml_texts(described, "OutputKey");
    let values = xml_texts(described, "OutputValue");
    assert_eq!(
        keys.len(),
        values.len(),
        "case {case:?}: expected one OutputValue per OutputKey {keys:?}, got {values:?} in \
         {described}"
    );
    keys.into_iter().zip(values).collect()
}

/// The record Amazon RDS reports for the row's resource, or `None` where it holds none.
async fn reported(router: &MultiServiceRouter, row: &Row) -> Option<String> {
    let (action, member_name, element, _) = row.subject.read();
    let (code, body) = rds(router, action, &[(member_name, row.identifier)]).await;
    if code != 200 {
        return None;
    }
    xml_blocks(&body, element).into_iter().next()
}

#[tokio::test]
async fn each_declared_rds_snapshot_or_endpoint_is_created_in_the_service_that_owns_it() {
    for row in ROWS {
        let router = router();
        let (code, created) = create_stack(&router, row.stack, row.template, &[]).await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must answer 200, got {code} {created}",
            row.case
        );
        let described = settled_stack(&router, row.case, row.stack).await;
        if xml_text(&described, "StackStatus").as_deref() != Some(status::CREATE_COMPLETE) {
            let reason = resource_event_reason(
                &router,
                row.stack,
                row.logical_id,
                resource_status::CREATE_FAILED,
            )
            .await;
            panic!(
                "case {:?}: expected the stack to reach {}, got {described} with the resource's \
                 own reason {reason:?}",
                row.case,
                status::CREATE_COMPLETE
            );
        }

        // The resource really exists in the service that owns it, in the status that service
        // reports a finished one in.
        let record = reported(&router, row).await.unwrap_or_else(|| {
            panic!(
                "case {:?}: expected Amazon RDS to report {}",
                row.case, row.identifier
            )
        });
        assert_eq!(
            xml_text(&record, "Status").as_deref(),
            Some(row.subject.ready_status()),
            "case {:?}: expected the resource Amazon RDS holds to be {}, got {record}",
            row.case,
            row.subject.ready_status()
        );
        let (_, _, _, arn_member) = row.subject.read();
        let minted = xml_text(&record, arn_member).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected Amazon RDS to report {arn_member}, got {record}",
                row.case
            )
        });

        // What the stack publishes is what that service minted: the ARN under the member the
        // type publishes it as, and the `Ref` of whichever identity this type is addressed by.
        let outputs = outputs_of(row.case, &described);
        assert_eq!(
            outputs.get("SnapshotArn"),
            Some(&minted),
            "case {:?}: expected the stack to publish the ARN Amazon RDS minted, got {outputs:?}",
            row.case
        );
        let expected_ref = if row.refs_the_arn {
            minted.clone()
        } else {
            row.identifier.to_string()
        };
        assert_eq!(
            outputs.get("SnapshotRef"),
            Some(&expected_ref),
            "case {:?}: expected Ref to resolve to {expected_ref}, got {outputs:?}",
            row.case
        );
        if row.subject == Subject::ClusterEndpoint {
            // The ADDRESS a custom endpoint serves on is Amazon RDS's to assign, so the stack
            // publishes the host that service reports rather than one built here.
            assert_eq!(
                outputs.get("Served").map(String::as_str),
                xml_text(&record, "Endpoint").as_deref(),
                "case {:?}: expected the stack to publish the endpoint Amazon RDS reports, got \
                 {outputs:?} against {record}",
                row.case
            );
        }

        // And the deletion really removes it: nothing of the stack is left in that service.
        // The stack is described by its ID from here on, which is how a completed deletion is
        // still readable.
        let stack_id = xml_text(&created, "StackId")
            .unwrap_or_else(|| panic!("case {:?}: expected a StackId, got {created}", row.case));
        let (code, deleted) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: DeleteStack must answer 200, got {code} {deleted}",
            row.case
        );
        let described = settled_stack(&router, row.case, &stack_id).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {:?}: expected the deletion to complete, got {described}",
            row.case
        );
        assert!(
            reported(&router, row).await.is_none(),
            "case {:?}: expected Amazon RDS to hold no {} once the stack was deleted",
            row.case,
            row.identifier
        );
    }
}

#[tokio::test]
async fn a_declaration_that_names_no_identifier_takes_a_generated_one_amazon_rds_accepts() {
    // The other half of the identifier rule: a declaration that names the resource nowhere
    // takes the name CloudFormation generates, which has to be one the owning service's own
    // identifier rule admits — Amazon RDS bounds a cluster endpoint's identifier at 63
    // characters and folds it to lower case, where a generated name is built from the stack
    // name, the logical id and a unique suffix. The declaration also carries the two
    // properties the row above leaves out (a membership list and the tag list), so a stack
    // declaring every property of the type still deploys.
    let case = "a custom endpoint declared with no identifier, a membership list and tags";
    let router = router();
    let template = concat!(
        "Resources:\n",
        "  Cluster:\n",
        "    Type: AWS::RDS::DBCluster\n",
        "    DeletionPolicy: Delete\n",
        "    Properties:\n",
        "      Engine: aurora-postgresql\n",
        "      MasterUsername: dbadmin\n",
        "      MasterUserPassword: an-example-password\n",
        "  CustomReaderEndpointOfTheCluster:\n",
        "    Type: AWS::RDS::DBClusterEndpoint\n",
        "    Properties:\n",
        "      DBClusterIdentifier: !Ref Cluster\n",
        "      EndpointType: ANY\n",
        "      ExcludedMembers: []\n",
        "      Tags:\n",
        "        - Key: owner\n",
        "          Value: the-stack\n",
    );
    let stack = "rds-types-generated-endpoint-name-for-a-long-enough-stack-name";
    let (code, created) = create_stack(&router, stack, template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described = settled_stack(&router, case, stack).await;
    if xml_text(&described, "StackStatus").as_deref() != Some(status::CREATE_COMPLETE) {
        let reason = resource_event_reason(
            &router,
            stack,
            "CustomReaderEndpointOfTheCluster",
            resource_status::CREATE_FAILED,
        )
        .await;
        panic!(
            "case {case:?}: expected the stack to reach {}, got {described} with the \
             resource's own reason {reason:?}",
            status::CREATE_COMPLETE
        );
    }

    // The generated identifier is the one Amazon RDS really holds the endpoint under, which
    // is what the stack reports as this resource's physical id.
    let (code, body) = call(
        &router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources must answer 200, got {code} {body}"
    );
    let endpoint_id = xml_blocks(&body, "member")
        .into_iter()
        .find(|entry| {
            xml_text(entry, "LogicalResourceId").as_deref()
                == Some("CustomReaderEndpointOfTheCluster")
        })
        .and_then(|entry| xml_text(&entry, "PhysicalResourceId"))
        .unwrap_or_else(|| panic!("case {case:?}: expected the endpoint's physical id, got {body}"));
    assert!(
        endpoint_id.len() <= 63
            && endpoint_id == endpoint_id.to_lowercase()
            && endpoint_id.starts_with(|first: char| first.is_ascii_alphabetic()),
        "case {case:?}: expected a generated identifier Amazon RDS admits, got {endpoint_id:?}"
    );
    let (code, reported) = rds(
        &router,
        "DescribeDBClusterEndpoints",
        &[("DBClusterEndpointIdentifier", &endpoint_id)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected Amazon RDS to hold the generated endpoint, got {code} \
         {reported}"
    );
    assert_eq!(
        xml_text(&reported, "CustomEndpointType").as_deref(),
        Some("ANY"),
        "case {case:?}: expected the declared endpoint type, got {reported}"
    );
}

#[tokio::test]
async fn a_type_cloudformation_does_not_know_is_still_refused() {
    // The control: the refusal these three types used to get is reserved for a type that is
    // none of the ones CloudFormation publishes, so registering them must not have made the
    // check permissive.
    let router = router();
    let template = concat!(
        "Resources:\n",
        "  Snapshot:\n",
        "    Type: AWS::RDS::DBSnapshotOfNothing\n",
        "    Properties:\n",
        "      DBSnapshotIdentifier: rds-types-unknown\n",
    );
    let (code, refused) = create_stack(&router, "rds-types-unknown", template, &[]).await;
    assert_eq!(
        code, 400,
        "case {:?}: expected the unknown type to be refused, got {code} {refused}",
        "a type CloudFormation publishes no such resource for"
    );
    assert!(
        refused.contains("Unrecognized resource types: [AWS::RDS::DBSnapshotOfNothing]"),
        "expected the unrecognized-type refusal to name the type, got {refused}"
    );
}
