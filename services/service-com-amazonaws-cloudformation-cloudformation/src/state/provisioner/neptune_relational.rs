// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon Neptune control plane's RELATIONAL types: the
//! DB cluster parameter group an `AWS::Neptune::DBClusterParameterGroup` declares, the DB
//! parameter group an `AWS::Neptune::DBParameterGroup` declares, the subnet group an
//! `AWS::Neptune::DBSubnetGroup` declares and the global database an
//! `AWS::Neptune::GlobalCluster` declares. One file of [`super`]'s per-type arms, dispatched
//! to by its `Provisioner`.
//!
//! ## Why the arms are Amazon RDS's
//!
//! Amazon Neptune's control plane is served on the relational front door: its model carries
//! `sigv4Name: rds` and `endpointPrefix: rds`, and the actions a group or a global database is
//! created, written, read and deleted by are spelled identically on both control planes
//! (`CreateDBClusterParameterGroup`, `ModifyDBClusterParameterGroup`,
//! `CreateDBSubnetGroup`, `CreateGlobalCluster`, …) against the same API version. So the call
//! a Neptune parameter group's provider makes IS the call the Amazon RDS one makes, and the
//! group it creates is the group a client's own `DescribeDBClusterParameterGroups` reads back
//! — which is what provisioning a declared resource means: it exists in the service that owns
//! it, with the configuration the template declared, under the name the stack publishes.
//!
//! That is why this file holds no call of its own: each type is registered against the arm the
//! Amazon RDS type of the same document already has
//! ([`super::create_parameter_group`], [`super::create_db_subnet_group`],
//! [`super::rds_global_clusters::create_global_cluster`]), stating only what is the TYPE's
//! rather than the wire's — the type name a property outside the create's catalogue is refused
//! by, and the SDK name a refusal this type's provider reports carries
//! ([`NEPTUNE_SDK_SERVICE`]).
//!
//! The engine's own validation comes with the call: the front door publishes this engine's
//! parameter catalogue, so a declared value outside a parameter's published range is refused
//! by it exactly as it refuses one of a relational family's, and the stack rolls the resource
//! back carrying that refusal.
//!
//! ## Which Neptune types are NOT here
//!
//! `AWS::Neptune::DBCluster` and `AWS::Neptune::DBInstance`, which stay declared-only: their
//! properties are NOT their create's own members (the cluster states its port as `DBPort`, its
//! IAM authentication as `IamAuthEnabled` and its serverless range as
//! `ServerlessScalingConfiguration`, and it carries a `DBInstanceParameterGroupName` that is a
//! member of no create at all), so each needs an arm of its own rather than a registration
//! against one of these.

use super::*;

/// `AWS::Neptune::DBClusterParameterGroup`: "Ref returns the resource name", which is the DB
/// cluster parameter group name every call on one takes — so this resource's physical id is
/// that name.
pub(crate) const NEPTUNE_DB_CLUSTER_PARAMETER_GROUP: &str =
    "AWS::Neptune::DBClusterParameterGroup";

/// `AWS::Neptune::DBParameterGroup`: the instance-scoped sibling, whose `Ref` is the DB
/// parameter group name a DB instance's `DBParameterGroupName` names.
pub(crate) const NEPTUNE_DB_PARAMETER_GROUP: &str = "AWS::Neptune::DBParameterGroup";

/// `AWS::Neptune::DBSubnetGroup`: the subnet group a Neptune DB cluster is launched into,
/// whose `Ref` is the group name — the same document `AWS::RDS::DBSubnetGroup` declares
/// (`DBSubnetGroupName`, `DBSubnetGroupDescription`, `SubnetIds`, `Tags`), which is why one
/// arm serves both.
pub(crate) const NEPTUNE_DB_SUBNET_GROUP: &str = "AWS::Neptune::DBSubnetGroup";

/// `AWS::Neptune::GlobalCluster`: a Neptune global database, whose `Ref` returns the
/// `GlobalClusterIdentifier` `DescribeGlobalClusters` and `DeleteGlobalCluster` each address
/// one by — so this resource's physical id needs no replacing by what the create answered.
pub(crate) const NEPTUNE_GLOBAL_CLUSTER: &str = "AWS::Neptune::GlobalCluster";

/// The name a provider of one of these types reports the called service under: `Neptune`, this
/// control plane's own `sdkId` (`smithy_query(query="service_info")` on
/// `services/service-com-amazonaws-neptune-amazonrdsv19/smithy-model/\
/// com-amazonaws-neptune-amazonrdsv19.json`), which is the name of the SDK client a handler of
/// an `AWS::Neptune::*` type holds.
///
/// It is NOT the signing name's service ([`super::RDS_SDK_SERVICE`]): the two control planes
/// share the name, the API version, the one path an awsQuery request is bound to and the action
/// names, so nothing on the wire says which of them was called and the provider states it
/// ([`call_query_as`]).
pub(crate) const NEPTUNE_SDK_SERVICE: &str = "Neptune";

/// The property either Amazon Neptune parameter-group type states the group's NAME in: plain
/// `Name`, which is the one property of those types that is not its create member's own name
/// — the type publishes `Description`, `Family`, `Name`, `Parameters` and `Tags`, and
/// "Ref returns the resource name".
const NEPTUNE_GROUP_NAME_PROPERTY: &str = "Name";

/// `AWS::Neptune::DBClusterParameterGroup`'s calls: the cluster-level parameter-group
/// operations of this front door, under this type's own name and SDK service.
///
/// The type states the group's name in a plain `Name` property — "Ref returns the resource
/// name" — where the Amazon RDS type of the same document spells it as the API member, so
/// that is the property the walk leaves to the arm ([`ParameterGroupCalls::name_property`]).
pub(crate) const NEPTUNE_DB_CLUSTER_PARAMETER_GROUP_CALLS: ParameterGroupCalls =
    ParameterGroupCalls {
        resource_type: NEPTUNE_DB_CLUSTER_PARAMETER_GROUP,
        sdk_service: NEPTUNE_SDK_SERVICE,
        name_member: "DBClusterParameterGroupName",
        name_property: NEPTUNE_GROUP_NAME_PROPERTY,
        // The group's own name is NOT one of them: this type publishes no property spelling
        // the API member, and the arm pushes that member itself from the resource's physical
        // id — so a declaration stating `DBClusterParameterGroupName` names a property the type
        // does not publish and is refused rather than passed through ([`query_members`]).
        create_members: &[
            ("DBParameterGroupFamily", QueryMember::Scalar),
            ("Description", QueryMember::Scalar),
            ("Tags", QueryMember::List("Tag")),
        ],
        create: "CreateDBClusterParameterGroup",
        modify: "ModifyDBClusterParameterGroup",
        delete: "DeleteDBClusterParameterGroup",
        describe: "DescribeDBClusterParameterGroups",
        record: "DBClusterParameterGroup",
    };

/// `AWS::Neptune::DBParameterGroup`'s calls: the instance-level sibling of the record above.
pub(crate) const NEPTUNE_DB_PARAMETER_GROUP_CALLS: ParameterGroupCalls = ParameterGroupCalls {
    resource_type: NEPTUNE_DB_PARAMETER_GROUP,
    sdk_service: NEPTUNE_SDK_SERVICE,
    name_member: "DBParameterGroupName",
    name_property: NEPTUNE_GROUP_NAME_PROPERTY,
    // Without the name member, for the reason the cluster-level record above gives.
    create_members: &[
        ("DBParameterGroupFamily", QueryMember::Scalar),
        ("Description", QueryMember::Scalar),
        ("Tags", QueryMember::List("Tag")),
    ],
    create: "CreateDBParameterGroup",
    modify: "ModifyDBParameterGroup",
    delete: "DeleteDBParameterGroup",
    describe: "DescribeDBParameterGroups",
    record: "DBParameterGroup",
};
