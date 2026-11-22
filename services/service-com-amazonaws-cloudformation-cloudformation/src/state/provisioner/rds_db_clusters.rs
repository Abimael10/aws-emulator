// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of an Amazon RDS DB CLUSTER: the cluster an
//! `AWS::RDS::DBCluster` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`.
//!
//! The type's properties are `CreateDBCluster`'s own members
//! ([`CREATE_DB_CLUSTER_MEMBERS`]) — `Engine`, `EngineVersion`, `MasterUsername`,
//! `DBSubnetGroupName`, `VpcSecurityGroupIds` and the rest — with the handful CloudFormation
//! spells differently renamed ([`DB_CLUSTER_RENAMED_PROPERTIES`]) and the ones that belong
//! to another RDS call left out of the request ([`DB_CLUSTER_PROPERTIES_NOT_SENT`]), exactly
//! as an `AWS::RDS::DBInstance`'s declaration is walked
//! ([`super::application_services::databases_and_networks`]).
//!
//! The cluster is created in the service that OWNS it, which is what lets the rest of a
//! stack work: a member `AWS::RDS::DBInstance` resolves its `DBClusterIdentifier` against
//! Amazon RDS's own clusters, so a stack declaring a cluster and an instance in it can only
//! reach `CREATE_COMPLETE` if the cluster really exists there.
//!
//! `CreateDBCluster` answers the cluster `creating`, in which it serves no connection, so
//! the resource's creation is held open until Amazon RDS reports it `available`
//! ([`db_cluster_readiness`]) and the endpoints the type publishes are read off it then
//! ([`super::rds_published_endpoints::db_cluster_ready_attributes`]).

use super::*;

/// `AWS::RDS::DBCluster`: "Ref returns the DBClusterIdentifier", which is the identifier
/// `DescribeDBClusters`, `ModifyDBCluster` and `DeleteDBCluster` each address one by — so
/// this resource's physical id needs no replacing by what the create answered.
pub(crate) const RDS_DB_CLUSTER: &str = "AWS::RDS::DBCluster";

/// The error code Amazon RDS answers for a DB cluster it does not hold:
/// `DBClusterNotFoundFault`'s own `awsQueryError` code
/// (`smithy_query(query="operation", operation="CreateDBCluster")`), which is what an
/// awsQuery client branches on.
pub(crate) const DB_CLUSTER_NOT_FOUND: &str = "DBClusterNotFoundFault";

/// How a DB cluster's associations are made and read: `AddRoleToDBCluster` makes one, and
/// `DescribeDBClusters` reports the ones in place under the element
/// `DBCluster$AssociatedRoles`' own member fixes (`smithy_query(query="shape",
/// shape="DBClusterRoles")`).
///
/// The cluster's CREATE carries the list too ([`CREATE_DB_CLUSTER_MEMBERS`]), so these calls
/// add the associations a CHANGED declaration asks for — `ModifyDBCluster` takes no such
/// member. The DB instance's record sits beside its own walk
/// ([`super::rds_db_instance_roles::DB_INSTANCE_ROLE_CALLS`]).
pub(crate) const DB_CLUSTER_ROLE_CALLS: super::rds_role_associations::RoleAssociationCalls =
    super::rds_role_associations::RoleAssociationCalls {
        identifier_member: "DBClusterIdentifier",
        associate: "AddRoleToDBCluster",
        describe: "DescribeDBClusters",
        entry_element: "DBClusterRole",
        not_found: DB_CLUSTER_NOT_FOUND,
    };

/// The `DBCluster$Status` a cluster SERVES in, and the ones a new cluster is still coming up
/// out of (Amazon Aurora User Guide, "Viewing Amazon Aurora DB cluster status": `available`,
/// `backing-up`, `creating`, `deleting`, `failing-over`, `maintenance`, `migrating`,
/// `modifying`, `promoting`, `rebooting`, `renaming`, `resetting-master-credentials`,
/// `starting`, `stopped`, `stopping`, `update-iam-db-auth`, `upgrading`).
///
/// A cluster in any other status is in one no wait brings a NEW cluster out of, which is a
/// resource that failed to come up rather than one still coming.
const DB_CLUSTER_STATUS_AVAILABLE: &str = "available";
const DB_CLUSTER_STATUSES_COMING_UP: &[&str] =
    &["creating", "backing-up", "maintenance", "modifying", "starting", "upgrading"];

/// The members `CreateDBCluster` takes, from Amazon RDS's own model
/// (`smithy_query(query="operation", operation="CreateDBCluster")`), with each list's
/// element name from its list shape's member `@xmlName`
/// (`smithy_query(query="shape", shape="VpcSecurityGroupIdList,AvailabilityZones,
/// LogTypeList,TagList")`).
///
/// This is the catalogue the declaration of an `AWS::RDS::DBCluster` is walked against: the
/// type's properties ARE these members, so a property whose name is one of them (or renames
/// to one, [`DB_CLUSTER_RENAMED_PROPERTIES`]) is passed through under it, and one the
/// catalogue does not carry is refused rather than dropped
/// ([`super::application_services::databases_and_networks::query_members`]).
pub(crate) const CREATE_DB_CLUSTER_MEMBERS: &[(&str, QueryMember)] = &[
    ("AllocatedStorage", QueryMember::Scalar),
    (
        // The list's element is the `@xmlName` its member fixes
        // (`smithy_query(query="shape", shape="DBClusterAssociatedRoles")`), which is the
        // spelling the called operation reads its entries under.
        "AssociatedRoles",
        QueryMember::List("DBClusterAssociatedRole"),
    ),
    ("AutoMinorVersionUpgrade", QueryMember::Scalar),
    ("AvailabilityZones", QueryMember::List("AvailabilityZone")),
    ("BacktrackWindow", QueryMember::Scalar),
    ("BackupRetentionPeriod", QueryMember::Scalar),
    ("CACertificateIdentifier", QueryMember::Scalar),
    ("CharacterSetName", QueryMember::Scalar),
    ("ClusterScalabilityType", QueryMember::Scalar),
    ("CopyTagsToSnapshot", QueryMember::Scalar),
    ("DBClusterIdentifier", QueryMember::Scalar),
    ("DBClusterInstanceClass", QueryMember::Scalar),
    ("DBClusterParameterGroupName", QueryMember::Scalar),
    ("DBSubnetGroupName", QueryMember::Scalar),
    ("DBSystemId", QueryMember::Scalar),
    ("DatabaseInsightsMode", QueryMember::Scalar),
    ("DatabaseName", QueryMember::Scalar),
    ("DeletionProtection", QueryMember::Scalar),
    ("Domain", QueryMember::Scalar),
    ("DomainIAMRoleName", QueryMember::Scalar),
    ("EnableCloudwatchLogsExports", QueryMember::List("member")),
    ("EnableGlobalWriteForwarding", QueryMember::Scalar),
    ("EnableHttpEndpoint", QueryMember::Scalar),
    ("EnableIAMDatabaseAuthentication", QueryMember::Scalar),
    ("EnableLimitlessDatabase", QueryMember::Scalar),
    ("EnableLocalWriteForwarding", QueryMember::Scalar),
    ("EnablePerformanceInsights", QueryMember::Scalar),
    ("Engine", QueryMember::Scalar),
    ("EngineLifecycleSupport", QueryMember::Scalar),
    ("EngineMode", QueryMember::Scalar),
    ("EngineVersion", QueryMember::Scalar),
    ("GlobalClusterIdentifier", QueryMember::Scalar),
    ("Iops", QueryMember::Scalar),
    ("KmsKeyId", QueryMember::Scalar),
    ("ManageMasterUserPassword", QueryMember::Scalar),
    ("MasterUserAuthenticationType", QueryMember::Scalar),
    ("MasterUserPassword", QueryMember::Scalar),
    ("MasterUserSecretKmsKeyId", QueryMember::Scalar),
    ("MasterUsername", QueryMember::Scalar),
    ("MonitoringInterval", QueryMember::Scalar),
    ("MonitoringRoleArn", QueryMember::Scalar),
    ("NetworkType", QueryMember::Scalar),
    ("OptionGroupName", QueryMember::Scalar),
    ("PerformanceInsightsKMSKeyId", QueryMember::Scalar),
    ("PerformanceInsightsRetentionPeriod", QueryMember::Scalar),
    ("Port", QueryMember::Scalar),
    ("PreferredBackupWindow", QueryMember::Scalar),
    ("PreferredMaintenanceWindow", QueryMember::Scalar),
    ("PubliclyAccessible", QueryMember::Scalar),
    ("ReplicationSourceIdentifier", QueryMember::Scalar),
    ("ScalingConfiguration", QueryMember::Structure),
    ("ServerlessV2ScalingConfiguration", QueryMember::Structure),
    ("StorageEncrypted", QueryMember::Scalar),
    ("StorageType", QueryMember::Scalar),
    ("Tags", QueryMember::List("Tag")),
    ("VpcSecurityGroupIds", QueryMember::List("VpcSecurityGroupId")),
];

/// The properties of `AWS::RDS::DBCluster` whose `CreateDBCluster` member is spelled
/// differently: CloudFormation publishes the two Performance Insights settings under the
/// names the DB CLUSTER resource type uses, where the API spells one as the switch that
/// enables the collection and the other with an upper-case `KMS`.
pub(crate) const DB_CLUSTER_RENAMED_PROPERTIES: &[(&str, &str)] = &[
    ("PerformanceInsightsEnabled", "EnablePerformanceInsights"),
    ("PerformanceInsightsKmsKeyId", "PerformanceInsightsKMSKeyId"),
];

/// The properties of `AWS::RDS::DBCluster` the walk over the declaration does NOT turn into
/// a `CreateDBCluster` member, each for a reason of its own — the same split the DB
/// instance's catalogue is in
/// ([`super::application_services::databases_and_networks::DB_INSTANCE_PROPERTIES_NOT_SENT`]).
pub(crate) const DB_CLUSTER_PROPERTIES_NOT_SENT: &[&str] = &[
    // The identifier CloudFormation decides, which is this resource's physical id ("Ref
    // returns the DBClusterIdentifier"): the arm sets the member from that id, so the walk
    // must not also push the declared spelling of it.
    "DBClusterIdentifier",
    // The tag list carries the declared tags AND the ones CloudFormation propagates to the
    // resource, so the arm builds it out of both.
    "Tags",
    // Members of a DIFFERENT call, which say nothing about the cluster a create makes:
    // `ModifyDBCluster` takes `ApplyImmediately`, `AllowMajorVersionUpgrade` and
    // `DBInstanceParameterGroupName` (each governs how a LATER modification is applied),
    // and the restore calls take where a cluster is rebuilt FROM —
    // `RestoreDBClusterFromSnapshot`'s `SnapshotIdentifier` and
    // `RestoreDBClusterToPointInTime`'s `SourceDBClusterIdentifier`, `RestoreType`,
    // `RestoreToTime`, `UseLatestRestorableTime` and `SourceRegion`.
    "AllowMajorVersionUpgrade",
    "ApplyImmediately",
    "DBInstanceParameterGroupName",
    "RestoreToTime",
    "RestoreType",
    "SnapshotIdentifier",
    "SourceDBClusterIdentifier",
    "SourceDbClusterIdentifier",
    "SourceRegion",
    "UseLatestRestorableTime",
    // Read-only members CloudFormation publishes as this type's RETURN values: a create
    // neither takes them nor could be told them.
    "Endpoint",
    "MasterUserSecret",
    "ReadEndpoint",
];

/// Create the DB cluster an `AWS::RDS::DBCluster` declares.
///
/// The evaluated declaration IS the request ([`CREATE_DB_CLUSTER_MEMBERS`]), with the one
/// value CloudFormation decides — the `DBClusterIdentifier`, which is this resource's
/// physical id — set on the way out, and the tag list built from the declared tags plus the
/// ones CloudFormation propagates.
///
/// What the create ANSWERS replaces the identifiers CloudFormation's naming rules had
/// computed for the resource: the ARN and the cluster's resource id are Amazon RDS's to
/// assign, the identifier is the one that service answered it stored the cluster under
/// ([`stored_identity`]), and the whole point of provisioning the cluster is that the handles
/// the stack publishes address the cluster RDS really holds. The ENDPOINTS are not read here — a
/// cluster that is still `creating` publishes none it serves on — so they are read once RDS
/// reports it up ([`super::rds_published_endpoints::db_cluster_ready_attributes`]).
pub(crate) async fn create_db_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_DB_CLUSTER_MEMBERS,
        DB_CLUSTER_RENAMED_PROPERTIES,
        DB_CLUSTER_PROPERTIES_NOT_SENT,
        RDS_DB_CLUSTER,
    )?;
    parameters.push((
        "DBClusterIdentifier".to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        "Tags",
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    let answered = call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "CreateDBCluster",
        &parameters,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "DBClusterArn").filter(|arn| !arn.is_empty()) {
        // The type publishes one ARN, so every name a resource's ARN is published under is
        // that same value ([`super::template::arn_attributes`]): all of them are re-pointed at
        // the ARN the cluster really carries.
        attributes.extend(super::template::arn_attributes(RDS_DB_CLUSTER, &arn));
    }
    if let Some(resource_id) =
        xml_element(&answered, "DbClusterResourceId").filter(|id| !id.is_empty())
    {
        attributes.insert("DBClusterResourceId".to_string(), resource_id);
    }
    push_master_user_secret_attribute(&answered, &mut attributes);
    Ok(Created {
        physical_resource_id: stored_identity(&answered, "DBClusterIdentifier"),
        attributes,
    })
}

/// Publish the managed master-user secret of the DB resource the owning service answered
/// about, where it reports one.
///
/// `AWS::RDS::DBCluster` and `AWS::RDS::DBInstance` both publish
/// [`MASTER_USER_SECRET_ARN_ATTRIBUTE`] as a return value: a declaration that turns
/// `ManageMasterUserPassword` on has Amazon RDS generate the master user password and manage
/// it in an Amazon Web Services Secrets Manager secret, and the resource reports that secret
/// as its read-only `MasterUserSecret`. Only that service can produce the ARN — it is the
/// secret Secrets Manager allocated — so it is read off its own answer rather than computed,
/// which is what makes a template that hands the secret to another resource (a function that
/// reads the credentials) resolve to the secret that really exists.
///
/// The member is a STRUCTURE (`<MasterUserSecret><SecretArn/><SecretStatus/>…`), read through
/// the same nesting the published members of `Endpoint` are. A resource whose declaration
/// manages no password has no such secret and reports no member, so nothing is re-pointed for
/// it.
pub(crate) fn push_master_user_secret_attribute(
    answered: &str,
    attributes: &mut BTreeMap<String, String>,
) {
    let reported = xml_scalar_children(answered, "MasterUserSecret");
    if let Some(secret_arn) = reported
        .get("SecretArn")
        .and_then(scalar_text)
        .filter(|arn| !arn.is_empty())
    {
        attributes.insert(MASTER_USER_SECRET_ARN_ATTRIBUTE.to_string(), secret_arn);
    }
}

/// Whether Amazon RDS reports the DB cluster a stack created UP, and the role associations a
/// CHANGED declaration asks for MADE.
///
/// `CreateDBCluster` answers the cluster `creating`, in which it serves no connection, so
/// the resource's creation is held open until that service reports it `available`. A status
/// no new cluster comes out of, and a cluster Amazon RDS no longer holds, is a failure,
/// because nothing is going to bring it up.
///
/// A cluster that IS available then takes the `AssociatedRoles` its declaration carries that
/// it is not already acting under ([`super::rds_role_associations`]): the create carries the
/// list, so a cluster is created with the roles its first declaration asked for and nothing is
/// added then — but `ModifyDBCluster` takes no such member, so an association a CHANGED
/// declaration adds is made here rather than dropped, and one Amazon RDS refuses is the
/// resource's own failure in that service's words.
pub(crate) async fn db_cluster_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_db_cluster(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon RDS holds no DB cluster named {physical_resource_id}"
        )));
    };
    let status = reported
        .get("Status")
        .and_then(scalar_text)
        .unwrap_or_default();
    if status == DB_CLUSTER_STATUS_AVAILABLE {
        if let Some(refusal) = super::rds_role_associations::associate_declared_roles(
            ctx,
            region,
            physical_resource_id,
            declared,
            DB_CLUSTER_ROLE_CALLS,
        )
        .await?
        {
            return Ok(Readiness::Refused(refusal));
        }
        return Ok(Readiness::Ready);
    }
    if DB_CLUSTER_STATUSES_COMING_UP.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Amazon RDS reports the DB cluster {physical_resource_id} in status {status}"
    )))
}

// The endpoints `AWS::RDS::DBCluster` publishes are read off the CLUSTER once Amazon RDS
// reports it available ([`super::rds_published_endpoints::db_cluster_ready_attributes`]).
//
// The type publishes the cluster endpoint (`Endpoint.Address`, `Endpoint.Port`) and the
// reader endpoint (`ReadEndpoint.Address`), and a client that reads one out of a stack
// output CONNECTS to it — so what the stack publishes is the address the service that owns
// the cluster really serves it on rather than a handle computed for the resource, which
// would be a name nothing resolves and no database answers on. A cluster that is still
// `creating` has no endpoint at all, which is why the create's answer carries none of them.
//
// What the create does read off the cluster is its IDENTITY — the ARN and the resource id
// Amazon RDS assigned — which `create_db_cluster` re-points above.

/// Apply a CHANGED declaration to the DB cluster a stack already created, through
/// `ModifyDBCluster` — the call Amazon RDS changes a cluster's own configuration with.
///
/// Only the members the modification TAKES are sent: a create-only member is not something
/// a modification can change, so a declaration that changed one is applied as far as the
/// modification reaches and the rest is left to the replacement CloudFormation decides
/// elsewhere. `ApplyImmediately` is sent as the declaration asks, and defaults to applying
/// the change at once, because a stack reports a resource updated when the update is done
/// rather than when the cluster's next maintenance window comes round.
pub(crate) async fn update_db_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let mut parameters = query_members(
        declared,
        MODIFY_DB_CLUSTER_MEMBERS,
        DB_CLUSTER_RENAMED_PROPERTIES,
        DB_CLUSTER_UPDATE_PROPERTIES_NOT_SENT,
        RDS_DB_CLUSTER,
    )?;
    parameters.push((
        "DBClusterIdentifier".to_string(),
        resource.physical_resource_id.clone(),
    ));
    if !parameters
        .iter()
        .any(|(member, _)| member == "ApplyImmediately")
    {
        parameters.push(("ApplyImmediately".to_string(), "true".to_string()));
    }
    let answered = call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "ModifyDBCluster",
        &parameters,
    )
    .await?;
    // The cluster keeps the identity it was created with — the ARN, the resource id and the
    // endpoints all follow the identifier — so the one return value a modification can change
    // is the managed master-user secret, which a modification that turns
    // `ManageMasterUserPassword` on has Amazon RDS mint.
    let mut attributes = BTreeMap::new();
    push_master_user_secret_attribute(&answered, &mut attributes);
    Ok(Some(Created::attributes(attributes)))
}

/// The properties the UPDATE does not send: the create's own list, plus every property that
/// is not a member of `ModifyDBCluster` at all.
///
/// A property outside the modification's catalogue is not something that call can change,
/// and it is not an unknown property either — the create accepted it — so it is left out of
/// the modification rather than refused: what a modification cannot change is what the
/// replacement CloudFormation decides elsewhere is for.
const DB_CLUSTER_UPDATE_PROPERTIES_NOT_SENT: &[&str] = &[
    "DBClusterIdentifier",
    "Tags",
    "AvailabilityZones",
    "CharacterSetName",
    "ClusterScalabilityType",
    "DBSubnetGroupName",
    "DBSystemId",
    "DatabaseName",
    "EnableCloudwatchLogsExports",
    "EnableLimitlessDatabase",
    "Endpoint",
    "Engine",
    "EngineLifecycleSupport",
    "EngineMode",
    "GlobalClusterIdentifier",
    "KmsKeyId",
    "MasterUserAuthenticationType",
    "MasterUserSecret",
    "MasterUsername",
    "PubliclyAccessible",
    "ReadEndpoint",
    "ReplicationSourceIdentifier",
    "RestoreToTime",
    "RestoreType",
    "SnapshotIdentifier",
    "SourceDBClusterIdentifier",
    "SourceDbClusterIdentifier",
    "SourceRegion",
    "StorageEncrypted",
    "UseLatestRestorableTime",
    // The role associations, which `ModifyDBCluster` takes no member for: an association a
    // changed declaration adds is made by the call that DOES associate one, where the
    // readiness arm makes it ([`super::rds_role_associations`]).
    "AssociatedRoles",
];

/// The members `ModifyDBCluster` takes that an `AWS::RDS::DBCluster` declaration can carry,
/// from Amazon RDS's own model (`smithy_query(query="operation",
/// operation="ModifyDBCluster")`): the create's catalogue without the members that can only
/// be set when the cluster is created — where it is placed, how its storage is encrypted,
/// what it was created from and the engine mode it runs.
const MODIFY_DB_CLUSTER_MEMBERS: &[(&str, QueryMember)] = &[
    ("AllocatedStorage", QueryMember::Scalar),
    ("ApplyImmediately", QueryMember::Scalar),
    ("AutoMinorVersionUpgrade", QueryMember::Scalar),
    ("BacktrackWindow", QueryMember::Scalar),
    ("BackupRetentionPeriod", QueryMember::Scalar),
    ("CACertificateIdentifier", QueryMember::Scalar),
    ("CopyTagsToSnapshot", QueryMember::Scalar),
    ("DBClusterInstanceClass", QueryMember::Scalar),
    ("DBClusterParameterGroupName", QueryMember::Scalar),
    ("DBInstanceParameterGroupName", QueryMember::Scalar),
    ("DatabaseInsightsMode", QueryMember::Scalar),
    ("DeletionProtection", QueryMember::Scalar),
    ("Domain", QueryMember::Scalar),
    ("DomainIAMRoleName", QueryMember::Scalar),
    ("EnableGlobalWriteForwarding", QueryMember::Scalar),
    ("EnableHttpEndpoint", QueryMember::Scalar),
    ("EnableIAMDatabaseAuthentication", QueryMember::Scalar),
    ("EnableLocalWriteForwarding", QueryMember::Scalar),
    ("EnablePerformanceInsights", QueryMember::Scalar),
    ("EngineVersion", QueryMember::Scalar),
    ("Iops", QueryMember::Scalar),
    ("ManageMasterUserPassword", QueryMember::Scalar),
    ("MasterUserPassword", QueryMember::Scalar),
    ("MasterUserSecretKmsKeyId", QueryMember::Scalar),
    ("MonitoringInterval", QueryMember::Scalar),
    ("MonitoringRoleArn", QueryMember::Scalar),
    ("NetworkType", QueryMember::Scalar),
    ("OptionGroupName", QueryMember::Scalar),
    ("PerformanceInsightsKMSKeyId", QueryMember::Scalar),
    ("PerformanceInsightsRetentionPeriod", QueryMember::Scalar),
    ("Port", QueryMember::Scalar),
    ("PreferredBackupWindow", QueryMember::Scalar),
    ("PreferredMaintenanceWindow", QueryMember::Scalar),
    ("ScalingConfiguration", QueryMember::Structure),
    ("ServerlessV2ScalingConfiguration", QueryMember::Structure),
    ("StorageType", QueryMember::Scalar),
    ("VpcSecurityGroupIds", QueryMember::List("VpcSecurityGroupId")),
];

/// Delete the DB cluster a stack created, addressed by its identifier — this resource's
/// physical id. A cluster Amazon RDS no longer has (`DBClusterNotFoundFault`) is already in
/// the state the delete asks for.
///
/// `DeleteDBCluster` requires the caller to say what becomes of the data, exactly as the DB
/// instance's delete does: the `FinalDBSnapshotIdentifier` a `Snapshot` deletion policy's
/// snapshot is taken under, and without one the form CloudFormation makes for the `Delete`
/// policy, which skips the final snapshot.
pub(crate) async fn delete_db_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    final_snapshot: Option<&str>,
) -> Result<(), Refusal> {
    let mut parameters = vec![(
        "DBClusterIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    match final_snapshot {
        Some(snapshot_identifier) => parameters.push((
            "FinalDBSnapshotIdentifier".to_string(),
            snapshot_identifier.to_string(),
        )),
        None => parameters.push(("SkipFinalSnapshot".to_string(), "true".to_string())),
    }
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DeleteDBCluster",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == DB_CLUSTER_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon RDS has finished removing the DB cluster a stack deleted.
///
/// `DeleteDBCluster` leaves the cluster `deleting` until the deletion completes, so the
/// cluster is still there when the delete answers: the deletion is finished once that
/// service no longer holds it, which is what keeps a stack from deleting the DB subnet
/// group the cluster still sits in.
pub(crate) async fn db_cluster_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_db_cluster(ctx, region, physical_resource_id).await? {
        Some(reported) => Ok(Removal::Pending(
            reported
                .get("Status")
                .and_then(scalar_text)
                .unwrap_or_default(),
        )),
        None => Ok(Removal::Gone),
    }
}

/// The cluster's configuration as Amazon RDS reports it, for the readiness and removal reads
/// above and for drift detection. A cluster it no longer holds (`DBClusterNotFoundFault`)
/// was deleted.
///
/// A DB cluster is reported as a STRUCTURE whose scalar members are named exactly as this
/// type's properties are (`Engine`, `EngineVersion`, `DatabaseName`, `Port`,
/// `BackupRetentionPeriod`), so those members are the actual configuration the declared one
/// is compared against. Its composite members are not: `VpcSecurityGroups` and
/// `DBClusterMembers` are reported as MEMBERSHIP structures carrying a status, where the
/// properties are plain id lists, so comparing either would need a mapping this module would
/// have to invent ([`super::drift`]).
pub(crate) async fn read_db_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "DBClusterIdentifier".to_string(),
        physical_resource_id.to_string(),
    )];
    let answered = match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeDBClusters",
        &parameters,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == DB_CLUSTER_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let reported = xml_scalar_children(&answered, "DBCluster");
    if reported.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(reported)))
}
