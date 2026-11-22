// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon Redshift Serverless' own control plane: the
//! namespace an `AWS::RedshiftServerless::Namespace` declares, the compute of one an
//! `AWS::RedshiftServerless::Workgroup` declares and the backup of one an
//! `AWS::RedshiftServerless::Snapshot` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! Each identity is the NAME the declaration states: `CreateNamespace` takes a
//! `namespaceName`, `CreateWorkgroup` a `workgroupName` and `CreateSnapshot` a
//! `snapshotName`, and that name is the handle every later call of that service on the
//! resource takes — `GetNamespace`, `GetWorkgroup`, `GetSnapshot` and the three deletes each
//! address one by it. So each type's `Ref` returns the name the conventional
//! `<last segment>Name` reading of [`super::template`]'s `physical_name` already finds, and
//! the resource is created in the owning service under exactly it.
//!
//! What each type PUBLISHES beyond that name is the owning service's own: the ids and ARNs it
//! mints, the endpoint it serves a workgroup on, the instants it records and the status it
//! reports the resource in. Every one of them is read off that service's own answer rather
//! than recomputed here ([`reported_namespace_attributes`],
//! [`reported_workgroup_attributes`], [`reported_snapshot_attributes`]), which is why the
//! stand-ins [`super::template`]'s return values compute for them are re-pointed by the
//! create.
//!
//! Two of the three lifecycle changes are not finished when their call answers: a created
//! workgroup is `CREATING`, in which it serves no connection and publishes no reachable
//! endpoint, and a created snapshot is `CREATING`, in which there is nothing to restore from
//! — so each resource's creation is held open until that service reports it `AVAILABLE`
//! ([`workgroup_readiness`], [`redshift_serverless_snapshot_readiness`]). A namespace is
//! `AVAILABLE` as soon as its create answers, so it needs no wait.

use super::*;

/// Amazon Redshift Serverless' signing name and `X-Amz-Target` prefix, from its model
/// (`smithy_query(query="service_info")`: `sigv4Name: redshift-serverless`, service shape
/// `RedshiftServerless`, protocol `awsJson1_1`) — the wire these calls ride on
/// ([`call_json_rpc`]).
pub(crate) const REDSHIFT_SERVERLESS_SIGNING_NAME: &str = "redshift-serverless";
pub(crate) const REDSHIFT_SERVERLESS_TARGET_PREFIX: &str = "RedshiftServerless";

/// The `AWS::<cloudFormationName>::<Resource>` types of the three Amazon Redshift Serverless
/// resources a template declares. The middle segment is the one the resource-type namespace
/// publishes (`RedshiftServerless`), which is that model's own `awsService.sdkId`
/// (`Redshift Serverless`) written as one word.
pub(crate) const REDSHIFT_SERVERLESS_NAMESPACE: &str = "AWS::RedshiftServerless::Namespace";
/// See [`REDSHIFT_SERVERLESS_NAMESPACE`].
pub(crate) const REDSHIFT_SERVERLESS_WORKGROUP: &str = "AWS::RedshiftServerless::Workgroup";
/// See [`REDSHIFT_SERVERLESS_NAMESPACE`].
pub(crate) const REDSHIFT_SERVERLESS_SNAPSHOT: &str = "AWS::RedshiftServerless::Snapshot";

/// The operations this module calls, as Amazon Redshift Serverless' own model names them.
const CREATE_NAMESPACE: &str = "CreateNamespace";
const GET_NAMESPACE: &str = "GetNamespace";
const UPDATE_NAMESPACE: &str = "UpdateNamespace";
const DELETE_NAMESPACE: &str = "DeleteNamespace";
const CREATE_WORKGROUP: &str = "CreateWorkgroup";
const GET_WORKGROUP: &str = "GetWorkgroup";
const UPDATE_WORKGROUP: &str = "UpdateWorkgroup";
const DELETE_WORKGROUP: &str = "DeleteWorkgroup";
const CREATE_SNAPSHOT: &str = "CreateSnapshot";
const GET_SNAPSHOT: &str = "GetSnapshot";
const DELETE_SNAPSHOT: &str = "DeleteSnapshot";

/// The error code Amazon Redshift Serverless answers for a resource it does not hold — the
/// modelled 404 of every read, update and delete of all three resources
/// (`smithy_query(query="operation", operation="GetWorkgroup,GetNamespace,GetSnapshot")`).
/// Judged on the CODE and never on a message, which carries the request's own name.
const REDSHIFT_SERVERLESS_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member each read answers its resource under
/// (`GetNamespaceResponse$namespace`, `GetWorkgroupResponse$workgroup`,
/// `GetSnapshotResponse$snapshot`), and the one the two creates take their tag list in.
const NAMESPACE_MEMBER: &str = "namespace";
const WORKGROUP_MEMBER: &str = "workgroup";
const SNAPSHOT_MEMBER: &str = "snapshot";
const TAGS_MEMBER: &str = "tags";

/// The member every call carries each resource's NAME in, which is the resource's physical id.
const NAMESPACE_NAME_MEMBER: &str = "namespaceName";
const WORKGROUP_NAME_MEMBER: &str = "workgroupName";
const SNAPSHOT_NAME_MEMBER: &str = "snapshotName";

/// The member each resource's lifecycle status is reported in.
const STATUS_MEMBER: &str = "status";

/// The status all three resources are usable in, from `NamespaceStatus`, `WorkgroupStatus` and
/// `SnapshotStatus` (`smithy_query(query="shape",
/// shape="NamespaceStatus,WorkgroupStatus,SnapshotStatus")`).
const REDSHIFT_SERVERLESS_AVAILABLE: &str = "AVAILABLE";

/// The `WorkgroupStatus` values a workgroup is still SETTLING in: a created one is `CREATING`
/// and a reconfigured one `MODIFYING`, and in neither has the change the resource waits on
/// landed. `DELETING` is not one: a teardown is what [`workgroup_removal`] waits on.
const WORKGROUP_SETTLING: &[&str] = &["CREATING", "MODIFYING"];

/// The `SnapshotStatus` values a snapshot is still being taken in: a created one is
/// `CREATING`, and one copied from another Region `COPYING`; in neither is there anything to
/// restore from yet.
const SNAPSHOT_SETTLING: &[&str] = &["CREATING", "COPYING"];

/// The status a resource being removed is reported in, which is what a teardown that has not
/// finished reports.
const REDSHIFT_SERVERLESS_DELETING: &str = "DELETING";

/// Every `Fn::GetAtt` attribute an [`REDSHIFT_SERVERLESS_NAMESPACE`] publishes, with the member
/// of that service's own `Namespace` each is read off
/// (`smithy_query(query="shape", shape="Namespace")`).
///
/// Source: the type's own "Return values" section, which publishes each value of the `Namespace`
/// the create answered under a `Namespace.` prefix. Every one of them is that service's to
/// produce — the id and the ARN it mints, the secret ARN it stores managed credentials under,
/// the instant it recorded and the status it reports — so each is a STAND-IN until
/// [`create_redshift_serverless_namespace`] has reported what `CreateNamespace` really answered.
/// The two members the shape reports as LISTS (`iamRoles`, `logExports`) are not carried: the
/// attribute map is string-valued, which is what [`super::template::return_values`] records.
const REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTES: &[(&str, &str)] = &[
    ("Namespace.AdminPasswordSecretArn", "adminPasswordSecretArn"),
    ("Namespace.AdminUsername", "adminUsername"),
    ("Namespace.CreationDate", "creationDate"),
    ("Namespace.DbName", "dbName"),
    ("Namespace.DefaultIamRoleArn", "defaultIamRoleArn"),
    ("Namespace.KmsKeyId", "kmsKeyId"),
    ("Namespace.NamespaceArn", "namespaceArn"),
    ("Namespace.NamespaceId", "namespaceId"),
    ("Namespace.NamespaceName", "namespaceName"),
    ("Namespace.Status", "status"),
];

/// Every `Fn::GetAtt` attribute an [`REDSHIFT_SERVERLESS_WORKGROUP`] publishes off the
/// `Workgroup` itself, read the same way (`smithy_query(query="shape", shape="Workgroup")`).
///
/// The members that shape reports as LISTS (`configParameters`, `securityGroupIds`,
/// `subnetIds`) are not carried, for the reason the namespace's are not; the two scalars of its
/// ENDPOINT live one level down ([`REDSHIFT_SERVERLESS_ENDPOINT_ATTRIBUTES`]).
const REDSHIFT_SERVERLESS_WORKGROUP_ATTRIBUTES: &[(&str, &str)] = &[
    ("Workgroup.BaseCapacity", "baseCapacity"),
    ("Workgroup.CreationDate", "creationDate"),
    ("Workgroup.EnhancedVpcRouting", "enhancedVpcRouting"),
    ("Workgroup.MaxCapacity", "maxCapacity"),
    ("Workgroup.NamespaceName", "namespaceName"),
    ("Workgroup.Port", "port"),
    ("Workgroup.PubliclyAccessible", "publiclyAccessible"),
    ("Workgroup.Status", "status"),
    ("Workgroup.TrackName", "trackName"),
    ("Workgroup.WorkgroupArn", "workgroupArn"),
    ("Workgroup.WorkgroupId", "workgroupId"),
    ("Workgroup.WorkgroupName", "workgroupName"),
];

/// The member a workgroup's endpoint is reported under, and the two attributes the type
/// publishes off it: the address a client connects to and the port it listens on, which are the
/// scalar members of that service's own `Endpoint` shape (its `vpcEndpoints` is a list, so no
/// attribute carries it).
const ENDPOINT_MEMBER: &str = "endpoint";
const REDSHIFT_SERVERLESS_ENDPOINT_ATTRIBUTES: &[(&str, &str)] = &[
    ("Workgroup.Endpoint.Address", "address"),
    ("Workgroup.Endpoint.Port", "port"),
];

/// Every `Fn::GetAtt` attribute an [`REDSHIFT_SERVERLESS_SNAPSHOT`] publishes, read the same way
/// off that service's own `Snapshot` (`smithy_query(query="shape", shape="Snapshot")`).
///
/// The two account lists that shape reports (`accountsWithRestoreAccess`,
/// `accountsWithProvisionedRestoreAccess`) are not carried, for the reason the namespace's lists
/// are not.
const REDSHIFT_SERVERLESS_SNAPSHOT_ATTRIBUTES: &[(&str, &str)] = &[
    (
        "Snapshot.ActualIncrementalBackupSizeInMegaBytes",
        "actualIncrementalBackupSizeInMegaBytes",
    ),
    ("Snapshot.AdminPasswordSecretArn", "adminPasswordSecretArn"),
    (
        "Snapshot.AdminPasswordSecretKmsKeyId",
        "adminPasswordSecretKmsKeyId",
    ),
    ("Snapshot.AdminUsername", "adminUsername"),
    (
        "Snapshot.BackupProgressInMegaBytes",
        "backupProgressInMegaBytes",
    ),
    (
        "Snapshot.CurrentBackupRateInMegaBytesPerSecond",
        "currentBackupRateInMegaBytesPerSecond",
    ),
    ("Snapshot.ElapsedTimeInSeconds", "elapsedTimeInSeconds"),
    (
        "Snapshot.EstimatedSecondsToCompletion",
        "estimatedSecondsToCompletion",
    ),
    ("Snapshot.KmsKeyId", "kmsKeyId"),
    ("Snapshot.NamespaceArn", "namespaceArn"),
    ("Snapshot.NamespaceName", "namespaceName"),
    ("Snapshot.OwnerAccount", "ownerAccount"),
    ("Snapshot.SnapshotArn", "snapshotArn"),
    ("Snapshot.SnapshotCreateTime", "snapshotCreateTime"),
    ("Snapshot.SnapshotName", "snapshotName"),
    ("Snapshot.SnapshotRemainingDays", "snapshotRemainingDays"),
    ("Snapshot.SnapshotRetentionPeriod", "snapshotRetentionPeriod"),
    (
        "Snapshot.SnapshotRetentionStartTime",
        "snapshotRetentionStartTime",
    ),
    ("Snapshot.Status", "status"),
    (
        "Snapshot.TotalBackupSizeInMegaBytes",
        "totalBackupSizeInMegaBytes",
    ),
];

/// The attribute NAMES each of the three types publishes, which is the other half of the
/// catalogue above — what the type's registry schema and the stand-in table both read
/// ([`super::template::published_attribute_names`]).
///
/// Stated in the SORTED order a resource's own attribute map holds them in, because that is the
/// order the stand-in table computes them in; a workgroup's carries the two endpoint attributes
/// in their sorted place among its own. Held to the catalogues above by
/// `the_published_names_are_the_catalogued_attributes`, so the two halves cannot drift apart.
pub(crate) const REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTE_NAMES: &[&str] = &[
    "Namespace.AdminPasswordSecretArn",
    "Namespace.AdminUsername",
    "Namespace.CreationDate",
    "Namespace.DbName",
    "Namespace.DefaultIamRoleArn",
    "Namespace.KmsKeyId",
    "Namespace.NamespaceArn",
    "Namespace.NamespaceId",
    "Namespace.NamespaceName",
    "Namespace.Status",
];

/// See [`REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTE_NAMES`].
pub(crate) const REDSHIFT_SERVERLESS_WORKGROUP_ATTRIBUTE_NAMES: &[&str] = &[
    "Workgroup.BaseCapacity",
    "Workgroup.CreationDate",
    "Workgroup.Endpoint.Address",
    "Workgroup.Endpoint.Port",
    "Workgroup.EnhancedVpcRouting",
    "Workgroup.MaxCapacity",
    "Workgroup.NamespaceName",
    "Workgroup.Port",
    "Workgroup.PubliclyAccessible",
    "Workgroup.Status",
    "Workgroup.TrackName",
    "Workgroup.WorkgroupArn",
    "Workgroup.WorkgroupId",
    "Workgroup.WorkgroupName",
];

/// See [`REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTE_NAMES`].
pub(crate) const REDSHIFT_SERVERLESS_SNAPSHOT_ATTRIBUTE_NAMES: &[&str] = &[
    "Snapshot.ActualIncrementalBackupSizeInMegaBytes",
    "Snapshot.AdminPasswordSecretArn",
    "Snapshot.AdminPasswordSecretKmsKeyId",
    "Snapshot.AdminUsername",
    "Snapshot.BackupProgressInMegaBytes",
    "Snapshot.CurrentBackupRateInMegaBytesPerSecond",
    "Snapshot.ElapsedTimeInSeconds",
    "Snapshot.EstimatedSecondsToCompletion",
    "Snapshot.KmsKeyId",
    "Snapshot.NamespaceArn",
    "Snapshot.NamespaceName",
    "Snapshot.OwnerAccount",
    "Snapshot.SnapshotArn",
    "Snapshot.SnapshotCreateTime",
    "Snapshot.SnapshotName",
    "Snapshot.SnapshotRemainingDays",
    "Snapshot.SnapshotRetentionPeriod",
    "Snapshot.SnapshotRetentionStartTime",
    "Snapshot.Status",
    "Snapshot.TotalBackupSizeInMegaBytes",
];

/// The properties of [`REDSHIFT_SERVERLESS_NAMESPACE`] this arm carries onto `CreateNamespace`,
/// with the member each travels as — the type's property names being its members' own, spelled
/// with an upper-case initial.
const NAMESPACE_TEXT_MEMBERS: &[(&str, &str)] = &[
    ("AdminUserPassword", "adminUserPassword"),
    ("AdminUsername", "adminUsername"),
    (
        "AdminPasswordSecretKmsKeyId",
        "adminPasswordSecretKmsKeyId",
    ),
    ("DbName", "dbName"),
    ("DefaultIamRoleArn", "defaultIamRoleArn"),
    ("KmsKeyId", "kmsKeyId"),
    ("RedshiftIdcApplicationArn", "redshiftIdcApplicationArn"),
];

/// The LIST properties of that type, carried as they came: each is a list of strings on both
/// sides.
const NAMESPACE_LIST_MEMBERS: &[(&str, &str)] =
    &[("IamRoles", "iamRoles"), ("LogExports", "logExports")];

/// Its one BOOLEAN property, read in either spelling a template gives one.
const NAMESPACE_FLAG_MEMBERS: &[(&str, &str)] = &[("ManageAdminPassword", "manageAdminPassword")];

/// Every property of that type this arm carries, so a declaration stating one outside the set
/// is REFUSED rather than reported as created without it ([`refuse_unmapped_properties`]).
///
/// `FinalSnapshotName` and `FinalSnapshotRetentionPeriod` are in the set because they are
/// `DeleteNamespace`'s own members rather than the create's: they are what the namespace is
/// taken away WITH ([`delete_redshift_serverless_namespace`]). `SnapshotCopyConfigurations` is
/// NOT: that configuration is a call of its own in the owning service, which this emulator
/// does not serve, so a declaration asking for one is refused rather than deployed without it.
const NAMESPACE_MAPPED_PROPERTIES: &[&str] = &[
    "AdminPasswordSecretKmsKeyId",
    "AdminUserPassword",
    "AdminUsername",
    "DbName",
    "DefaultIamRoleArn",
    "FinalSnapshotName",
    "FinalSnapshotRetentionPeriod",
    "IamRoles",
    "KmsKeyId",
    "LogExports",
    "ManageAdminPassword",
    "NamespaceName",
    "RedshiftIdcApplicationArn",
    "Tags",
];

/// The members `UpdateNamespace` takes of those — that call carries the admin credentials, the
/// encryption keys, the IAM roles and the log exports
/// (`smithy_query(query="operation", operation="UpdateNamespace")`). `DbName` and
/// `RedshiftIdcApplicationArn` are members of the CREATE alone, which is why a declaration
/// restating one replaces the resource ([`super::template::replacement_properties`]) instead of
/// reaching here.
const NAMESPACE_UPDATABLE_TEXT_MEMBERS: &[(&str, &str)] = &[
    ("AdminUserPassword", "adminUserPassword"),
    ("AdminUsername", "adminUsername"),
    (
        "AdminPasswordSecretKmsKeyId",
        "adminPasswordSecretKmsKeyId",
    ),
    ("DefaultIamRoleArn", "defaultIamRoleArn"),
    ("KmsKeyId", "kmsKeyId"),
];

/// The two members `DeleteNamespace` takes beside the name: the snapshot it captures on the way
/// out and how long that snapshot is kept, which are the type's own two `FinalSnapshot…`
/// properties.
const FINAL_SNAPSHOT_NAME_PROPERTY: &str = "FinalSnapshotName";
const FINAL_SNAPSHOT_NAME_MEMBER: &str = "finalSnapshotName";
const FINAL_SNAPSHOT_RETENTION_PROPERTY: &str = "FinalSnapshotRetentionPeriod";
const FINAL_SNAPSHOT_RETENTION_MEMBER: &str = "finalSnapshotRetentionPeriod";

/// The TEXT properties of [`REDSHIFT_SERVERLESS_WORKGROUP`] and the members they travel as.
const WORKGROUP_TEXT_MEMBERS: &[(&str, &str)] = &[
    ("NamespaceName", "namespaceName"),
    ("TrackName", "trackName"),
];

/// Its NUMERIC properties: the compute it is provisioned with, the ceiling it may scale to and
/// the port it serves on. A template states a number either in JSON's own spelling or as the
/// TEXT a parameter reference resolves to, so each is read in the one spelling the member takes
/// ([`declared_number`]).
const WORKGROUP_NUMBER_MEMBERS: &[(&str, &str)] = &[
    ("BaseCapacity", "baseCapacity"),
    ("MaxCapacity", "maxCapacity"),
    ("Port", "port"),
];

/// Its BOOLEAN properties: whether traffic is forced through the VPC and whether the workgroup
/// is reachable from a public network.
const WORKGROUP_FLAG_MEMBERS: &[(&str, &str)] = &[
    ("EnhancedVpcRouting", "enhancedVpcRouting"),
    ("PubliclyAccessible", "publiclyAccessible"),
];

/// Its LIST properties: the subnets the workgroup's endpoint is placed in and the security
/// groups that endpoint is governed by, each a list of the identifiers Amazon EC2 minted for
/// the resources the same stack declares.
const WORKGROUP_LIST_MEMBERS: &[(&str, &str)] = &[
    ("SecurityGroupIds", "securityGroupIds"),
    ("SubnetIds", "subnetIds"),
];

/// Its STRUCTURED properties: the database settings it is created with and the
/// price-performance target it is tuned to. Both are the owning service's own shapes spelled
/// with upper-case initials (`ConfigParameter$ParameterKey` against that model's
/// `parameterKey`, `PerformanceTarget$Level` against its `level`), so each is carried through
/// [`camel_cased`].
const WORKGROUP_STRUCTURED_MEMBERS: &[(&str, &str)] = &[
    ("ConfigParameters", "configParameters"),
    ("PricePerformanceTarget", "pricePerformanceTarget"),
];

/// Every property of the workgroup type this arm carries.
///
/// The four properties that restore a workgroup FROM a backup (`RecoveryPointId`,
/// `SnapshotArn`, `SnapshotName`, `SnapshotOwnerAccount`) are not in the set: each asks for a
/// restore call of its own in the owning service rather than for a member of `CreateWorkgroup`,
/// so a declaration stating one is refused rather than deployed as a workgroup with none of
/// the data it asked for.
const WORKGROUP_MAPPED_PROPERTIES: &[&str] = &[
    "BaseCapacity",
    "ConfigParameters",
    "EnhancedVpcRouting",
    "MaxCapacity",
    "NamespaceName",
    "Port",
    "PricePerformanceTarget",
    "PubliclyAccessible",
    "SecurityGroupIds",
    "SubnetIds",
    "Tags",
    "TrackName",
    "WorkgroupName",
];

/// The properties of [`REDSHIFT_SERVERLESS_SNAPSHOT`] this arm carries, with the member of
/// `CreateSnapshot` each travels as: the namespace the snapshot is taken of and how long it is
/// kept. Its own NAME is this resource's physical id, which the arm pushes.
const SNAPSHOT_TEXT_MEMBERS: &[(&str, &str)] = &[("NamespaceName", "namespaceName")];
const SNAPSHOT_NUMBER_MEMBERS: &[(&str, &str)] = &[("RetentionPeriod", "retentionPeriod")];

/// Every property of the snapshot type this arm carries.
const SNAPSHOT_MAPPED_PROPERTIES: &[&str] =
    &["NamespaceName", "RetentionPeriod", "SnapshotName", "Tags"];

/// Create the namespace an `AWS::RedshiftServerless::Namespace` declares, in Amazon Redshift
/// Serverless — so a client that deployed the stack reads it back through that service's own
/// `GetNamespace` and `ListNamespaces` under the name the stack published.
///
/// The declared properties ARE `CreateNamespace`'s members, each spelled with the initial its
/// own member takes, with the name the naming rules settled pushed as `namespaceName` — this
/// resource's physical id, which is what the type's `Ref` returns — and the declared tags
/// beside the ones CloudFormation gives the resource ([`tags_of`]) as the `TagList` that call
/// takes.
///
/// What the create ANSWERS is the namespace itself, whose id, ARN and recorded instant are
/// values only that service produces: they become the attributes the type publishes, replacing
/// the stand-ins the naming rules computed.
pub(crate) async fn create_redshift_serverless_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, NAMESPACE_MAPPED_PROPERTIES)?;
    let properties = declared_properties(&resource.properties);
    let mut request = Map::new();
    copy_members(&properties, NAMESPACE_TEXT_MEMBERS, &mut request);
    copy_members(&properties, NAMESPACE_LIST_MEMBERS, &mut request);
    copy_flag_members(&properties, NAMESPACE_FLAG_MEMBERS, &mut request);
    request.insert(
        NAMESPACE_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    push_tag_list(&mut request, &tags_of(&resource.properties, propagated));
    let answered = call_redshift_serverless(
        ctx,
        region,
        CREATE_NAMESPACE,
        &Value::Object(request),
    )
    .await?;
    Ok(Created::attributes(reported_namespace_attributes(
        answered.get(NAMESPACE_MEMBER),
    )))
}

/// Apply a CHANGED declaration of the namespace type to the namespace the stack already has,
/// through `UpdateNamespace` — the one call that reconfigures a namespace that exists.
///
/// It takes the admin credentials, the encryption keys, the IAM roles and the log exports,
/// addressed by the name the namespace was created under; every other property is fixed when
/// the namespace is made, so a declaration restating one replaces the resource rather than
/// reaching here.
pub(crate) async fn update_redshift_serverless_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared_properties(declared);
    let mut request = Map::new();
    copy_members(&properties, NAMESPACE_UPDATABLE_TEXT_MEMBERS, &mut request);
    copy_members(&properties, NAMESPACE_LIST_MEMBERS, &mut request);
    copy_flag_members(&properties, NAMESPACE_FLAG_MEMBERS, &mut request);
    request.insert(
        NAMESPACE_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let answered = call_redshift_serverless(
        ctx,
        region,
        UPDATE_NAMESPACE,
        &Value::Object(request),
    )
    .await?;
    Ok(Created::attributes(reported_namespace_attributes(
        answered.get(NAMESPACE_MEMBER),
    )))
}

/// Delete the namespace a stack created, by the name it was created under — this resource's
/// physical id.
///
/// `DeleteNamespace` takes the snapshot the namespace is captured into on its way out and how
/// long that snapshot is kept, which are the type's own two `FinalSnapshot…` properties: a
/// declaration that states them asks for that capture, so they travel with the delete. One
/// that service no longer holds is already in the state the delete asks for.
pub(crate) async fn delete_redshift_serverless_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let properties = declared_properties(&resource.properties);
    let mut request = Map::new();
    request.insert(
        NAMESPACE_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    if let Some(name) = properties.get(FINAL_SNAPSHOT_NAME_PROPERTY) {
        request.insert(FINAL_SNAPSHOT_NAME_MEMBER.to_string(), name.clone());
        if let Some(retention) = properties.get(FINAL_SNAPSHOT_RETENTION_PROPERTY) {
            request.insert(
                FINAL_SNAPSHOT_RETENTION_MEMBER.to_string(),
                declared_number(retention).unwrap_or_else(|| retention.clone()),
            );
        }
    }
    gone_or_refused(
        call_redshift_serverless(ctx, region, DELETE_NAMESPACE, &Value::Object(request)).await,
    )
}

/// The namespace's configuration as Amazon Redshift Serverless reports it, for drift detection:
/// its own read of the namespace, by the name the stack published. One it no longer holds was
/// deleted.
pub(crate) async fn describe_redshift_serverless_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_namespace(ctx, region, physical_resource_id)
        .await?
        .map(|namespace| {
            reported_as_properties(
                &namespace,
                &[
                    ("NamespaceName", NAMESPACE_NAME_MEMBER),
                    ("AdminUsername", "adminUsername"),
                    ("DbName", "dbName"),
                    ("DefaultIamRoleArn", "defaultIamRoleArn"),
                    ("KmsKeyId", "kmsKeyId"),
                ],
            )
        }))
}

/// Create the workgroup an `AWS::RedshiftServerless::Workgroup` declares, in Amazon Redshift
/// Serverless — the compute the namespace the declaration names serves its databases through.
///
/// The declared properties ARE `CreateWorkgroup`'s members, each read in the one spelling its
/// own member takes ([`WORKGROUP_NUMBER_MEMBERS`], [`WORKGROUP_FLAG_MEMBERS`],
/// [`WORKGROUP_STRUCTURED_MEMBERS`]), with the name the naming rules settled pushed as
/// `workgroupName`. The subnets and security groups a declaration names are the identifiers
/// Amazon EC2 minted for the resources of the same stack, which the template's own `Ref`
/// already resolved to by the time this arm runs — so the workgroup's endpoint is placed in
/// the stack's own network.
///
/// What the create ANSWERS is the workgroup itself: the id and ARN that service minted, the
/// endpoint it will serve the workgroup on and the status it reports it in, which become the
/// attributes the type publishes. The create leaves the workgroup `CREATING`, so those
/// attributes are read again once that service reports it up
/// ([`workgroup_ready_attributes`]).
pub(crate) async fn create_redshift_serverless_workgroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, WORKGROUP_MAPPED_PROPERTIES)?;
    let properties = declared_properties(&resource.properties);
    let mut request = workgroup_members(&properties, WORKGROUP_TEXT_MEMBERS)?;
    request.insert(
        WORKGROUP_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    push_tag_list(&mut request, &tags_of(&resource.properties, propagated));
    let answered = call_redshift_serverless(
        ctx,
        region,
        CREATE_WORKGROUP,
        &Value::Object(request),
    )
    .await?;
    Ok(Created::attributes(reported_workgroup_attributes(
        answered.get(WORKGROUP_MEMBER),
    )))
}

/// Apply a CHANGED declaration of the workgroup type to the workgroup the stack already has,
/// through `UpdateWorkgroup` — the one call that reconfigures a workgroup that exists.
///
/// It takes the compute, the ceiling, the port, the two flags, the subnets, the security
/// groups, the database settings and the price-performance target, addressed by the name the
/// workgroup was created under; the NAMESPACE it serves is a member of the create alone, so a
/// declaration naming another namespace replaces the resource rather than reaching here.
pub(crate) async fn update_redshift_serverless_workgroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    let properties = declared_properties(declared);
    // The namespace is not a member of this call, so only the workgroup's own configuration
    // travels: `TrackName` is the one text member the update takes.
    let mut request = workgroup_members(&properties, &[("TrackName", "trackName")])?;
    request.insert(
        WORKGROUP_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let answered = call_redshift_serverless(
        ctx,
        region,
        UPDATE_WORKGROUP,
        &Value::Object(request),
    )
    .await?;
    Ok(Created::attributes(reported_workgroup_attributes(
        answered.get(WORKGROUP_MEMBER),
    )))
}

/// Whether Amazon Redshift Serverless has finished bringing up — or reconfiguring — the
/// workgroup a stack created.
///
/// A created workgroup is `CREATING`, in which it serves no connection, and a reconfigured one
/// `MODIFYING`; `AVAILABLE` is the state either change is finished in, and every other status
/// is one no wait will bring a workgroup out of. One that service no longer holds is in the
/// same position, nothing being left to create it.
pub(crate) async fn workgroup_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(workgroup) = read_workgroup(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon Redshift Serverless holds no workgroup {physical_resource_id}"
        )));
    };
    Ok(settled(
        &workgroup,
        WORKGROUP_SETTLING,
        &format!("workgroup {physical_resource_id}"),
    ))
}

/// The attributes Amazon Redshift Serverless publishes for a workgroup it now reports up: the
/// status it reports and the endpoint it serves the workgroup on are not the ones the create
/// answered, so the type's attributes are read off the workgroup again.
pub(crate) async fn workgroup_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(reported_workgroup_attributes(
        read_workgroup(ctx, region, physical_resource_id)
            .await?
            .as_ref(),
    ))
}

/// Whether Amazon Redshift Serverless has finished removing the workgroup a stack deleted —
/// which is what keeps a stack from deleting the subnets the workgroup's endpoint network
/// interfaces are still placed in. The deletion is finished once that service no longer holds
/// it.
pub(crate) async fn workgroup_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    Ok(removal_of(
        read_workgroup(ctx, region, physical_resource_id).await?,
    ))
}

/// Delete the workgroup a stack created, by the name it was created under. One that service no
/// longer holds is already in the state the delete asks for.
pub(crate) async fn delete_redshift_serverless_workgroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    gone_or_refused(
        call_redshift_serverless(
            ctx,
            region,
            DELETE_WORKGROUP,
            &serde_json::json!({ WORKGROUP_NAME_MEMBER: physical_resource_id }),
        )
        .await,
    )
}

/// The workgroup's configuration as Amazon Redshift Serverless reports it, for drift detection.
pub(crate) async fn describe_redshift_serverless_workgroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_workgroup(ctx, region, physical_resource_id)
        .await?
        .map(|workgroup| {
            reported_as_properties(
                &workgroup,
                &[
                    ("WorkgroupName", WORKGROUP_NAME_MEMBER),
                    ("NamespaceName", NAMESPACE_NAME_MEMBER),
                    ("BaseCapacity", "baseCapacity"),
                    ("MaxCapacity", "maxCapacity"),
                    ("Port", "port"),
                    ("EnhancedVpcRouting", "enhancedVpcRouting"),
                    ("PubliclyAccessible", "publiclyAccessible"),
                    ("TrackName", "trackName"),
                ],
            )
        }))
}

/// Create the snapshot an `AWS::RedshiftServerless::Snapshot` declares, in Amazon Redshift
/// Serverless — the backup of the namespace the declaration names, which a client reads back
/// through that service's own `GetSnapshot` and `ListSnapshots` under the name the stack
/// published.
///
/// The create leaves the snapshot `CREATING`, in which there is nothing to restore from, so the
/// resource's creation is held open until that service reports it `AVAILABLE`
/// ([`redshift_serverless_snapshot_readiness`]).
pub(crate) async fn create_redshift_serverless_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SNAPSHOT_MAPPED_PROPERTIES)?;
    let properties = declared_properties(&resource.properties);
    let mut request = Map::new();
    copy_members(&properties, SNAPSHOT_TEXT_MEMBERS, &mut request);
    copy_numeric_members(&properties, SNAPSHOT_NUMBER_MEMBERS, &mut request);
    request.insert(
        SNAPSHOT_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    push_tag_list(&mut request, &tags_of(&resource.properties, propagated));
    let answered =
        call_redshift_serverless(ctx, region, CREATE_SNAPSHOT, &Value::Object(request)).await?;
    Ok(Created::attributes(reported_snapshot_attributes(
        answered.get(SNAPSHOT_MEMBER),
    )))
}

/// Whether Amazon Redshift Serverless has finished taking the snapshot a stack created:
/// `AVAILABLE` is the state the capture is finished in, `CREATING` and `COPYING` are states to
/// wait out, and anything else is one no wait will bring it out of.
pub(crate) async fn redshift_serverless_snapshot_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(snapshot) = read_snapshot(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon Redshift Serverless holds no snapshot {physical_resource_id}"
        )));
    };
    Ok(settled(
        &snapshot,
        SNAPSHOT_SETTLING,
        &format!("snapshot {physical_resource_id}"),
    ))
}

/// The attributes that service publishes for a snapshot it now reports taken: the sizes it
/// measured, the instants it recorded and the status it reports are not the ones the create
/// answered, so the type's attributes are read off the snapshot again.
pub(crate) async fn redshift_serverless_snapshot_ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    Ok(reported_snapshot_attributes(
        read_snapshot(ctx, region, physical_resource_id).await?.as_ref(),
    ))
}

/// Whether that service has finished removing the snapshot a stack deleted.
pub(crate) async fn redshift_serverless_snapshot_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    Ok(removal_of(
        read_snapshot(ctx, region, physical_resource_id).await?,
    ))
}

/// Delete the snapshot a stack created, by the name it was taken under. One that service no
/// longer holds is already in the state the delete asks for.
pub(crate) async fn delete_redshift_serverless_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    gone_or_refused(
        call_redshift_serverless(
            ctx,
            region,
            DELETE_SNAPSHOT,
            &serde_json::json!({ SNAPSHOT_NAME_MEMBER: physical_resource_id }),
        )
        .await,
    )
}

/// The snapshot's configuration as Amazon Redshift Serverless reports it, for drift detection.
pub(crate) async fn describe_redshift_serverless_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_snapshot(ctx, region, physical_resource_id)
        .await?
        .map(|snapshot| {
            reported_as_properties(
                &snapshot,
                &[
                    ("SnapshotName", SNAPSHOT_NAME_MEMBER),
                    ("NamespaceName", NAMESPACE_NAME_MEMBER),
                    ("RetentionPeriod", "snapshotRetentionPeriod"),
                ],
            )
        }))
}

/// The request `CreateWorkgroup` and `UpdateWorkgroup` share: the declaration's numbers, flags,
/// lists and structures, each in the one spelling its own member takes, plus the text members
/// the calling arm says that call takes.
fn workgroup_members(
    properties: &Map<String, Value>,
    text: &[(&str, &str)],
) -> Result<Map<String, Value>, Refusal> {
    let mut request = Map::new();
    copy_members(properties, text, &mut request);
    copy_numeric_members(properties, WORKGROUP_NUMBER_MEMBERS, &mut request);
    copy_flag_members(properties, WORKGROUP_FLAG_MEMBERS, &mut request);
    copy_members(properties, WORKGROUP_LIST_MEMBERS, &mut request);
    for (property, member) in WORKGROUP_STRUCTURED_MEMBERS {
        if let Some(declared) = properties.get(*property) {
            request.insert((*member).to_string(), camel_cased(declared, 0)?);
        }
    }
    Ok(request)
}

/// The namespace record that service holds under `physical_resource_id`, or `None` when it
/// holds none.
async fn read_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_NAMESPACE,
        NAMESPACE_NAME_MEMBER,
        NAMESPACE_MEMBER,
        physical_resource_id,
    )
    .await
}

/// The workgroup record that service holds under `physical_resource_id`, read the same way.
async fn read_workgroup(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_WORKGROUP,
        WORKGROUP_NAME_MEMBER,
        WORKGROUP_MEMBER,
        physical_resource_id,
    )
    .await
}

/// The snapshot record that service holds under `physical_resource_id`, read the same way.
async fn read_snapshot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    read_record(
        ctx,
        region,
        GET_SNAPSHOT,
        SNAPSHOT_NAME_MEMBER,
        SNAPSHOT_MEMBER,
        physical_resource_id,
    )
    .await
}

/// The ONE read all three reads are: that service's own `Get…` of the record by its name, with
/// the not-found it models read as the record being gone rather than as a refused read.
async fn read_record(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    name_member: &str,
    record_member: &str,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ name_member: name });
    match call_redshift_serverless(ctx, region, operation, &request).await {
        Ok(answered) => Ok(answered.get(record_member).cloned()),
        Err(refusal) if refusal.code == REDSHIFT_SERVERLESS_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// What one delete answered: a resource that service no longer holds
/// ([`REDSHIFT_SERVERLESS_NOT_FOUND`], judged on the CODE) is already in the state the delete
/// asks for, and every other refusal is the delete's.
fn gone_or_refused(answered: Result<Value, Refusal>) -> Result<(), Refusal> {
    match answered {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == REDSHIFT_SERVERLESS_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// What one reported record says about the lifecycle change its resource is waiting on:
/// [`REDSHIFT_SERVERLESS_AVAILABLE`] is finished, a status in `settling` is still running, and
/// anything else is a state no wait will bring the resource out of. `subject` names the
/// resource in the reason a failed wait reports.
fn settled(record: &Value, settling: &[&str], subject: &str) -> Readiness {
    let status = reported_text(record, STATUS_MEMBER).unwrap_or_default();
    if status == REDSHIFT_SERVERLESS_AVAILABLE {
        return Readiness::Ready;
    }
    if settling.contains(&status.as_str()) {
        return Readiness::Pending(status);
    }
    Readiness::Failed(format!(
        "Amazon Redshift Serverless reports {subject} as {status}"
    ))
}

/// What one read of a resource a stack DELETED says about the teardown: a record that service no
/// longer answers is gone, and one it still answers is still being removed, in the status it
/// reports.
fn removal_of(record: Option<Value>) -> Removal {
    match record {
        None => Removal::Gone,
        Some(record) => Removal::Pending(
            reported_text(&record, STATUS_MEMBER)
                .unwrap_or_else(|| REDSHIFT_SERVERLESS_DELETING.to_string()),
        ),
    }
}

/// The attributes one reported namespace gives the resource, read the one way every arm reads a
/// reported attribute ([`reported_attributes`]). A read that answered no namespace gives none,
/// so the resource keeps what the naming rules computed.
fn reported_namespace_attributes(namespace: Option<&Value>) -> BTreeMap<String, String> {
    namespace
        .map(|namespace| reported_attributes(namespace, REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTES))
        .unwrap_or_default()
}

/// The attributes one reported workgroup gives the resource: its own members, and the two
/// scalars of the ENDPOINT that service publishes for it, which live one level down.
fn reported_workgroup_attributes(workgroup: Option<&Value>) -> BTreeMap<String, String> {
    let Some(workgroup) = workgroup else {
        return BTreeMap::new();
    };
    let mut attributes = reported_attributes(workgroup, REDSHIFT_SERVERLESS_WORKGROUP_ATTRIBUTES);
    if let Some(endpoint) = workgroup.get(ENDPOINT_MEMBER) {
        attributes.extend(reported_attributes(
            endpoint,
            REDSHIFT_SERVERLESS_ENDPOINT_ATTRIBUTES,
        ));
    }
    attributes
}

/// The attributes one reported snapshot gives the resource, read the same way.
fn reported_snapshot_attributes(snapshot: Option<&Value>) -> BTreeMap<String, String> {
    snapshot
        .map(|snapshot| reported_attributes(snapshot, REDSHIFT_SERVERLESS_SNAPSHOT_ATTRIBUTES))
        .unwrap_or_default()
}

/// Add the tag set to a create's request as the `TagList` of `Tag` structures that member takes,
/// whose members are spelled `key`/`value` ([`LOWER_CASED_TAG`], which is how that service's own
/// `Tag` shape spells them: `smithy_query(query="shape", shape="Tag")`); a resource with no tags
/// at all carries no `tags` member.
fn push_tag_list(request: &mut Map<String, Value>, tags: &[(String, String)]) {
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(tags, LOWER_CASED_TAG),
        );
    }
}

/// One declaration's property document as the map every arm here walks; an empty map for a
/// declaration that states no properties at all.
fn declared_properties(properties: &Value) -> Map<String, Value> {
    properties.as_object().cloned().unwrap_or_else(Map::new)
}

/// Call one Amazon Redshift Serverless operation in process, on the awsJson wire that service
/// publishes.
async fn call_redshift_serverless(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        REDSHIFT_SERVERLESS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{REDSHIFT_SERVERLESS_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of [`the_published_names_are_the_catalogued_attributes`]: a type, the attribute
    /// names it publishes and the catalogues the arms here read its reported record off.
    struct Row {
        case: &'static str,
        names: &'static [&'static str],
        catalogues: &'static [&'static [(&'static str, &'static str)]],
    }

    /// The published attribute NAMES of each of the three types are exactly the attributes the
    /// catalogues above read off the owning service's own records, in the SORTED order the
    /// stand-in table computes them in.
    ///
    /// The two halves are read by different callers — the names by the registry's resource
    /// schema and by [`super::super::template`]'s stand-in table, the pairs by the arms here —
    /// so a name missing from one half or invented in the other would make what a stack
    /// publishes for a resource disagree with what the type's schema says it publishes.
    #[test]
    fn the_published_names_are_the_catalogued_attributes() {
        let rows = [
            Row {
                case: REDSHIFT_SERVERLESS_NAMESPACE,
                names: REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTE_NAMES,
                catalogues: &[REDSHIFT_SERVERLESS_NAMESPACE_ATTRIBUTES],
            },
            Row {
                case: REDSHIFT_SERVERLESS_WORKGROUP,
                names: REDSHIFT_SERVERLESS_WORKGROUP_ATTRIBUTE_NAMES,
                catalogues: &[
                    REDSHIFT_SERVERLESS_WORKGROUP_ATTRIBUTES,
                    REDSHIFT_SERVERLESS_ENDPOINT_ATTRIBUTES,
                ],
            },
            Row {
                case: REDSHIFT_SERVERLESS_SNAPSHOT,
                names: REDSHIFT_SERVERLESS_SNAPSHOT_ATTRIBUTE_NAMES,
                catalogues: &[REDSHIFT_SERVERLESS_SNAPSHOT_ATTRIBUTES],
            },
        ];
        for Row {
            case,
            names,
            catalogues,
        } in rows
        {
            let catalogued: ::std::collections::BTreeSet<&str> = catalogues
                .iter()
                .flat_map(|catalogue| catalogue.iter().map(|(attribute, _)| *attribute))
                .collect();
            let expected: Vec<&str> = catalogued.into_iter().collect();
            assert_eq!(
                names.to_vec(),
                expected,
                "case {case:?}: expected the published names to be the catalogued attributes \
                 in sorted order, got {names:?}"
            );
        }
    }
}
