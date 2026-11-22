// SPDX-License-Identifier: CC0-1.0

//! The identity a stack publishes for an Amazon RDS resource whose name the TEMPLATE
//! declared, read back in Amazon RDS.
//!
//! RULE: the physical id a stack publishes for a resource it created — what the resource's
//! `Ref` resolves to — is the identifier the service that OWNS the resource answered it
//! created it under, and the ARN its `Fn::GetAtt` answers is the ARN that service reports for
//! it, so a service that NORMALISES the identifier it stores hands the stack the stored
//! spelling and not the one the declaration wrote. Every Amazon RDS resource an
//! `AWS::RDS::*` type creates is named under a member whose own documentation says "This
//! value is stored as a lowercase string" (`CreateDBInstance.DBInstanceIdentifier`,
//! `CreateDBSubnetGroup.DBSubnetGroupName`, `CreateDBParameterGroup.DBParameterGroupName`,
//! `CreateDBClusterParameterGroup.DBClusterParameterGroupName` —
//! `smithy_query(query="doc", shape="CreateDBSubnetGroupMessage", member="DBSubnetGroupName",
//! model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`
//! and its siblings), so a template that names one of these resources in mixed case gets the
//! lower-cased identifier back.
//!
//! Why it matters: every downstream consumer of such a `Ref` — a DB instance's
//! `DBSubnetGroupName`, a stack output a script feeds back into an RDS call — is handed that
//! identifier. One that differs from the stored spelling names a resource that service does
//! not hold, so the next call 404s on a resource the stack has just created.
//!
//! `rds_published_name_identity.rs` is the sibling suite that asserts the same rule for a name
//! CloudFormation GENERATED; this one is about a name the TEMPLATE supplied, which no naming
//! rule of CloudFormation's ever touched. The rows differ along what the rule generalises
//! over: the resource TYPE (each read back through its own describe) and the CASE of the
//! declared name — mixed case, all upper case, and an already-lower-case control row that
//! needs no folding at all.

use smithy_server_runtime::MultiServiceRouter;

use crate::rds_generated_resource_names::{deploy, published_name};
use crate::stack_lifecycle::{describe_stack, output_value, rds, router, xml_blocks, xml_text};

/// One resource the template NAMES, with the handles its identity is read through.
struct Declared {
    /// The CloudFormation resource type.
    resource_type: &'static str,
    /// The logical id it is declared under.
    logical_id: &'static str,
    /// The name the template declares for it, exactly as written.
    declared: &'static str,
    /// The `OutputKey` whose `Value` is `!Ref <logical id>`.
    output_key: &'static str,
    /// The `OutputKey` whose `Value` is `!GetAtt <logical id>.<arn_member>`.
    arn_output_key: &'static str,
    /// The member the owning service's read reports the resource's ARN as, which is also the
    /// attribute name the type publishes it under.
    arn_member: &'static str,
    /// The Amazon RDS read that reports the resource.
    describe: &'static str,
    /// The element that read reports one resource as.
    record: &'static str,
    /// The member that read addresses the resource by and reports its name as — the same
    /// member the create takes, and so what this type's `Ref` returns.
    name_member: &'static str,
}

/// Every row of this suite's template: one Amazon RDS type per declared-name case.
const DECLARED: &[Declared] = &[
    Declared {
        resource_type: "AWS::RDS::DBSubnetGroup",
        logical_id: "Subnets",
        // Mixed case, as a template written by a human or a CDK app spells a name.
        declared: "Sim-Declared-Subnets-MixedCase",
        output_key: "SubnetGroupNameOut",
        arn_output_key: "SubnetGroupArnOut",
        arn_member: "DBSubnetGroupArn",
        describe: "DescribeDBSubnetGroups",
        record: "DBSubnetGroup",
        name_member: "DBSubnetGroupName",
    },
    Declared {
        resource_type: "AWS::RDS::DBParameterGroup",
        logical_id: "Params",
        declared: "Sim-Declared-Params",
        output_key: "ParameterGroupNameOut",
        arn_output_key: "ParameterGroupArnOut",
        arn_member: "DBParameterGroupArn",
        describe: "DescribeDBParameterGroups",
        record: "DBParameterGroup",
        name_member: "DBParameterGroupName",
    },
    Declared {
        resource_type: "AWS::RDS::DBClusterParameterGroup",
        // All upper case: the whole name is folded, not just a character of it.
        logical_id: "ClusterParams",
        declared: "SIM-DECLARED-CLUSTER-PARAMS",
        output_key: "ClusterParameterGroupNameOut",
        arn_output_key: "ClusterParameterGroupArnOut",
        arn_member: "DBClusterParameterGroupArn",
        describe: "DescribeDBClusterParameterGroups",
        record: "DBClusterParameterGroup",
        name_member: "DBClusterParameterGroupName",
    },
    Declared {
        resource_type: "AWS::RDS::DBInstance",
        logical_id: "Database",
        declared: "Sim-Declared-Db",
        output_key: "InstanceIdentifierOut",
        arn_output_key: "InstanceArnOut",
        arn_member: "DBInstanceArn",
        describe: "DescribeDBInstances",
        record: "DBInstance",
        name_member: "DBInstanceIdentifier",
    },
    Declared {
        // The CONTROL row: a declared name that is already of the form Amazon RDS stores, so
        // the published identifier is the declared name unchanged.
        resource_type: "AWS::RDS::DBParameterGroup",
        logical_id: "LowerParams",
        declared: "sim-declared-lower-params",
        output_key: "LowerParameterGroupNameOut",
        arn_output_key: "LowerParameterGroupArnOut",
        arn_member: "DBParameterGroupArn",
        describe: "DescribeDBParameterGroups",
        record: "DBParameterGroup",
        name_member: "DBParameterGroupName",
    },
];

/// The template declaring every row of [`DECLARED`] under the name it carries, with one `Ref`
/// output per resource — so the identifier the stack publishes is read both as the resource's
/// `PhysicalResourceId` and as the value its `Ref` resolved to.
///
/// The subnet ids are the stack's own subnets, since Amazon RDS resolves every entry of
/// `SubnetIds` against the subnets the account holds, and the DB instance is placed in the
/// subnet group and the parameter group the same stack declares — so the names those `Ref`s
/// resolve to have to be names Amazon RDS accepts.
const TEMPLATE: &str = concat!(
    "Resources:\n",
    "  Vpc:\n",
    "    Type: AWS::EC2::VPC\n",
    "    Properties:\n",
    "      CidrBlock: 10.11.0.0/16\n",
    "  SubnetOne:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.11.1.0/24\n",
    "      AvailabilityZone: !Select [0, !GetAZs \"\"]\n",
    "  SubnetTwo:\n",
    "    Type: AWS::EC2::Subnet\n",
    "    Properties:\n",
    "      VpcId: !Ref Vpc\n",
    "      CidrBlock: 10.11.2.0/24\n",
    "      AvailabilityZone: !Select [1, !GetAZs \"\"]\n",
    "  Subnets:\n",
    "    Type: AWS::RDS::DBSubnetGroup\n",
    "    Properties:\n",
    "      DBSubnetGroupName: Sim-Declared-Subnets-MixedCase\n",
    "      DBSubnetGroupDescription: the declared-name stack's subnets\n",
    "      SubnetIds:\n",
    "        - !Ref SubnetOne\n",
    "        - !Ref SubnetTwo\n",
    "  Params:\n",
    "    Type: AWS::RDS::DBParameterGroup\n",
    "    Properties:\n",
    "      DBParameterGroupName: Sim-Declared-Params\n",
    "      Description: the declared-name stack's parameters\n",
    "      Family: mysql8.0\n",
    "      Parameters:\n",
    "        max_connections: '77'\n",
    "  ClusterParams:\n",
    "    Type: AWS::RDS::DBClusterParameterGroup\n",
    "    Properties:\n",
    "      DBClusterParameterGroupName: SIM-DECLARED-CLUSTER-PARAMS\n",
    "      Description: the declared-name stack's cluster parameters\n",
    "      Family: aurora-mysql8.0\n",
    "  LowerParams:\n",
    "    Type: AWS::RDS::DBParameterGroup\n",
    "    Properties:\n",
    "      DBParameterGroupName: sim-declared-lower-params\n",
    "      Description: the declared-name stack's already-folded parameters\n",
    "      Family: mysql8.0\n",
    "  Database:\n",
    "    Type: AWS::RDS::DBInstance\n",
    "    Properties:\n",
    "      DBInstanceIdentifier: Sim-Declared-Db\n",
    "      Engine: mysql\n",
    "      DBInstanceClass: db.t3.micro\n",
    "      AllocatedStorage: 20\n",
    "      DBSubnetGroupName: !Ref Subnets\n",
    "      DBParameterGroupName: !Ref Params\n",
    "Outputs:\n",
    "  SubnetGroupNameOut:\n",
    "    Value: !Ref Subnets\n",
    "  SubnetGroupArnOut:\n",
    "    Value: !GetAtt Subnets.DBSubnetGroupArn\n",
    "  ParameterGroupNameOut:\n",
    "    Value: !Ref Params\n",
    "  ParameterGroupArnOut:\n",
    "    Value: !GetAtt Params.DBParameterGroupArn\n",
    "  ClusterParameterGroupNameOut:\n",
    "    Value: !Ref ClusterParams\n",
    "  ClusterParameterGroupArnOut:\n",
    "    Value: !GetAtt ClusterParams.DBClusterParameterGroupArn\n",
    "  LowerParameterGroupNameOut:\n",
    "    Value: !Ref LowerParams\n",
    "  LowerParameterGroupArnOut:\n",
    "    Value: !GetAtt LowerParams.DBParameterGroupArn\n",
    "  InstanceIdentifierOut:\n",
    "    Value: !Ref Database\n",
    "  InstanceArnOut:\n",
    "    Value: !GetAtt Database.DBInstanceArn\n",
);

/// The ONE record Amazon RDS reports for the resource asked for under `asked`, left exactly as
/// that read answered it.
async fn reported_record(
    case: &str,
    router: &MultiServiceRouter,
    row: &Declared,
    asked: &str,
) -> String {
    let (status_code, body) = rds(router, row.describe, &[(row.name_member, asked)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: {}({asked}) must answer 200, got {status_code} {body}",
        row.describe
    );
    let records = xml_blocks(&body, row.record);
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected {}({asked}) to report exactly one {}, got {body}",
        row.describe,
        row.record
    );
    records.into_iter().next().unwrap_or_default()
}

#[tokio::test]
async fn a_declared_rds_name_is_published_as_the_name_amazon_rds_stored() {
    let case = "a stack naming every Amazon RDS resource it declares";
    let stack = "rds-declared-names";
    let router = router();
    deploy(case, &router, stack, TEMPLATE).await;
    let (status_code, described) = describe_stack(&router, stack).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {status_code} {described}"
    );

    for row in DECLARED {
        let row_case = format!("{case}/{}/{}", row.resource_type, row.logical_id);
        // The rule's own statement: the stored spelling of the declared name.
        let stored = row.declared.to_ascii_lowercase();

        let published = published_name(&router, stack, row.logical_id).await;
        assert_eq!(
            published, stored,
            "case {row_case:?}: expected the stack to publish the identifier Amazon RDS \
             stored for the declared {:?}, got {published:?}",
            row.declared
        );
        assert_eq!(
            output_value(&described, row.output_key).as_deref(),
            Some(stored.as_str()),
            "case {row_case:?}: expected the Ref output {} to be {stored:?}, got {described}",
            row.output_key
        );

        // And the owning service holds the resource under exactly that identifier, reporting
        // it under that spelling — so the stack's view of the resource's identity and the
        // owning service's view cannot disagree.
        let record = reported_record(&row_case, &router, row, &published).await;
        assert_eq!(
            xml_text(&record, row.name_member).as_deref(),
            Some(stored.as_str()),
            "case {row_case:?}: expected Amazon RDS to report {} {stored:?}, got {record}",
            row.name_member
        );

        // The resource's ARN is that service's too: the stack publishes the one Amazon RDS
        // reports for the resource, not one built from the spelling the template wrote.
        let reported_arn = xml_text(&record, row.arn_member).unwrap_or_default();
        assert!(
            reported_arn.ends_with(&stored),
            "case {row_case:?}: expected Amazon RDS to report an ARN of the resource it \
             stored, got {reported_arn:?}"
        );
        assert_eq!(
            output_value(&described, row.arn_output_key).as_deref(),
            Some(reported_arn.as_str()),
            "case {row_case:?}: expected the GetAtt output {} to be the ARN Amazon RDS \
             reports ({reported_arn:?}), got {described}",
            row.arn_output_key
        );
    }
}
