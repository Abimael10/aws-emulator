// SPDX-License-Identifier: CC0-1.0

//! `AWS::EFS::FileSystem`, `AWS::EFS::AccessPoint` and `AWS::EFS::MountTarget`: the Amazon
//! EFS estate a stack declares, created in Amazon EFS itself.
//!
//! This is the per-type half of provisioning for three resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::athena_resources`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! All three are identified by an identifier the owning service MINTS, and by nothing else:
//! `CreateFileSystem` answers the `fs-` id every later call takes
//! (`DescribeFileSystems$FileSystemId`, `CreateMountTarget$FileSystemId`,
//! `CreateAccessPoint$FileSystemId`, each constrained to
//! `^(arn:aws[-a-z]*:elasticfilesystem:[0-9a-z-:]+:file-system/fs-[0-9a-f]{8,40}|fs-[0-9a-f]{8,40})$`),
//! `CreateAccessPoint` the `fsap-` id, and `CreateMountTarget` the `fsmt-` id. None of the
//! three types declares a NAME at all — a file system's name is a tag of its own tag set — so
//! a stack that reported one created while nothing created it publishes a handle no call of
//! that service even admits: feeding it to `DescribeFileSystems` is refused for the member's
//! modelled pattern, no mount target or access point can be made on it, and the stack's own
//! deletion has nothing to delete. That is what creating each resource where it belongs puts
//! right, and it is why
//! [`identity_is_service_assigned`](crate::state::template::identity_is_service_assigned)
//! names all three: the identity in force is the one Amazon EFS answered.
//!
//! ## The properties
//!
//! A file system's declaration is `CreateFileSystem`'s own members under the same names
//! ([`FILE_SYSTEM_CREATE_MEMBERS`]) plus the four that are CALLS OF THEIR OWN, which that
//! service takes only once the file system is `available`
//! ([`apply_available_settings`], reached from [`readiness`] exactly as
//! [`crate::state::provisioner::directory_service_directories`] applies the flags its own
//! create cannot carry): `BackupPolicy` is `PutBackupPolicy`, `FileSystemPolicy` (with
//! `BypassPolicyLockoutSafetyCheck`) is `PutFileSystemPolicy`, `LifecyclePolicies` is
//! `PutLifecycleConfiguration` and `FileSystemProtection` is `UpdateFileSystemProtection`.
//! `CreationToken` is required by the create and is no property of the type, so it is
//! derived from the resource's own computed identity ([`creation_token`]) — stable across
//! reads and across a re-deployment of the same declaration, which is what that member is
//! for ("the idempotency token").
//!
//! An access point's declaration is `CreateAccessPoint`'s own members under the same names,
//! and a mount target's is `CreateMountTarget`'s ([`ACCESS_POINT_CREATE_MEMBERS`],
//! [`MOUNT_TARGET_CREATE_MEMBERS`]). The tag properties the two taggable types publish are
//! `FileSystemTags` and `AccessPointTags` rather than `Tags` ([`tag_property`]), and each
//! travels as the create's own `Tags` list beside the tags CloudFormation gives the resource.
//! A mount target is not taggable at all: the type publishes no tag property, and the
//! `ResourceId` of that service's tagging calls admits only a file system and an access point.
//!
//! A declared property none of those calls carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::{
    applicable_tags, call_rest_json_named, key_value_tag_list, modelled_declaration, Created,
    ModelledScalar, Readiness, Refusal, Removal,
};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The resource types this module provisions. Visible to the rest of this service's state
/// because more than one module answers about them: the identity in force is the one the
/// owning service minted, the return-value catalogue publishes each type's attributes, and
/// the replacement rules name the properties a resource of one cannot take anew.
pub(in crate::state) const EFS_FILE_SYSTEM: &str = "AWS::EFS::FileSystem";
/// See [`EFS_FILE_SYSTEM`].
pub(in crate::state) const EFS_ACCESS_POINT: &str = "AWS::EFS::AccessPoint";
/// See [`EFS_FILE_SYSTEM`].
pub(in crate::state) const EFS_MOUNT_TARGET: &str = "AWS::EFS::MountTarget";

/// Amazon EFS's signing name, which is what routes an in-process call to its own handlers
/// (that model's `sigv4Name` and `awsService.endpointPrefix`, both
/// `elasticfilesystem`). It speaks `restJson1`, so each call below carries the method and
/// URI the called operation's own model declares.
pub(in crate::state) const EFS_SIGNING_NAME: &str = "elasticfilesystem";

/// The prefix of the URI every operation of that service is bound under: its API version
/// (`smithy_query(query="service_info")` -> `version: 2015-02-01`).
const EFS_URI_PREFIX: &str = "/2015-02-01";

/// The operations this module asks for, by name — stated rather than spelled at each call so
/// the name a refused resource reports and the URI the call was made on cannot drift apart
/// ([`call_rest_json_named`]).
const CREATE_FILE_SYSTEM: &str = "CreateFileSystem";
const DESCRIBE_FILE_SYSTEMS: &str = "DescribeFileSystems";
const UPDATE_FILE_SYSTEM: &str = "UpdateFileSystem";
const DELETE_FILE_SYSTEM: &str = "DeleteFileSystem";
const PUT_BACKUP_POLICY: &str = "PutBackupPolicy";
const PUT_FILE_SYSTEM_POLICY: &str = "PutFileSystemPolicy";
const DELETE_FILE_SYSTEM_POLICY: &str = "DeleteFileSystemPolicy";
const PUT_LIFECYCLE_CONFIGURATION: &str = "PutLifecycleConfiguration";
const UPDATE_FILE_SYSTEM_PROTECTION: &str = "UpdateFileSystemProtection";
const CREATE_ACCESS_POINT: &str = "CreateAccessPoint";
const DESCRIBE_ACCESS_POINTS: &str = "DescribeAccessPoints";
const DELETE_ACCESS_POINT: &str = "DeleteAccessPoint";
const CREATE_MOUNT_TARGET: &str = "CreateMountTarget";
const DESCRIBE_MOUNT_TARGETS: &str = "DescribeMountTargets";
const DELETE_MOUNT_TARGET: &str = "DeleteMountTarget";
const MODIFY_MOUNT_TARGET_SECURITY_GROUPS: &str = "ModifyMountTargetSecurityGroups";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error codes that service answers for a resource it does not hold — the modelled 404s
/// of its reads, its writes and its deletes — which is what says a delete has nothing left to
/// remove and a read nothing to report. Judged on the CODE, never on a message.
const FILE_SYSTEM_NOT_FOUND: &str = "FileSystemNotFound";
/// See [`FILE_SYSTEM_NOT_FOUND`].
const ACCESS_POINT_NOT_FOUND: &str = "AccessPointNotFound";
/// See [`FILE_SYSTEM_NOT_FOUND`].
const MOUNT_TARGET_NOT_FOUND: &str = "MountTargetNotFound";
/// The code that service answers for a policy it holds none of, which a file system that
/// never had one is in (`DescribeFileSystemPolicy`'s and `DeleteFileSystemPolicy`'s modelled
/// `PolicyNotFound`).
const POLICY_NOT_FOUND: &str = "PolicyNotFound";

/// The code the refusal of a MOUNT TARGET that would break the one-mount-target-per-zone rule
/// carries (`CreateMountTarget`'s modelled `MountTargetConflict`, HTTP 409): "there can be
/// only one mount target per Availability Zone".
///
/// Visible to the rest of this layer because the CLASS this type's provider reports it under
/// is not the class the code's own wording reads as ([`super::Owner::handler_error_class`]):
/// a zone that already has this file system's mount target is a resource that ALREADY EXISTS
/// where the declaration asks for one.
pub(in crate::state) const MOUNT_TARGET_CONFLICT: &str = "MountTargetConflict";

/// The lifecycle state Amazon EFS reports a resource of any of the three types ready in, and
/// the ones it reports one that is still coming up or going away in
/// (`smithy_query(query="shape",shape="LifeCycleState")` -> creating | available | updating |
/// deleting | deleted | error).
const LIFE_CYCLE_AVAILABLE: &str = "available";
/// See [`LIFE_CYCLE_AVAILABLE`].
const LIFE_CYCLE_COMING_UP: &[&str] = &["creating", "updating"];

/// The member every one of that service's descriptions carries its lifecycle state under.
const LIFE_CYCLE_STATE: &str = "LifeCycleState";

/// The members the three creates answer the identifier they minted under, which is each
/// type's physical resource id.
const FILE_SYSTEM_ID: &str = "FileSystemId";
/// See [`FILE_SYSTEM_ID`].
const ACCESS_POINT_ID: &str = "AccessPointId";
/// See [`FILE_SYSTEM_ID`].
const MOUNT_TARGET_ID: &str = "MountTargetId";

/// The members the creates answer the other published return values under: a file system's
/// ARN, an access point's ARN, and the address a mount target is reachable at.
const FILE_SYSTEM_ARN: &str = "FileSystemArn";
/// See [`FILE_SYSTEM_ARN`].
const ACCESS_POINT_ARN: &str = "AccessPointArn";
/// See [`FILE_SYSTEM_ARN`].
const IP_ADDRESS: &str = "IpAddress";

/// The `Fn::GetAtt` attributes the three types publish, as their own "Return values"
/// sections name them: a file system's `Arn` and `FileSystemId`, an access point's `Arn` and
/// `AccessPointId`, and a mount target's `Id` and `IpAddress`.
///
/// Carried here beside the arms that fill them so the catalogue that computes a stand-in for
/// each ([`crate::state::template::return_values`]) and the create that reports the real
/// value cannot name the same attribute differently.
pub(in crate::state) const ARN_ATTRIBUTE: &str = "Arn";
/// See [`ARN_ATTRIBUTE`].
pub(in crate::state) const FILE_SYSTEM_ID_ATTRIBUTE: &str = "FileSystemId";
/// See [`ARN_ATTRIBUTE`].
pub(in crate::state) const ACCESS_POINT_ID_ATTRIBUTE: &str = "AccessPointId";
/// See [`ARN_ATTRIBUTE`].
pub(in crate::state) const ID_ATTRIBUTE: &str = "Id";
/// See [`ARN_ATTRIBUTE`].
pub(in crate::state) const IP_ADDRESS_ATTRIBUTE: &str = "IpAddress";

/// The member the creates take a tag list under, and the two names that service's own `Tag`
/// spells a tag's key and value with (`smithy_query(query="shape",shape="Tag")`).
const TAGS_MEMBER: &str = "Tags";

/// The member `CreateFileSystem` requires as the create's idempotency token.
const CREATION_TOKEN: &str = "CreationToken";

/// How many characters of the resource's computed identity the creation token carries. The
/// member admits 1 to 64 (`CreationToken`, `@length` 1..64), and the computed identity is
/// already shorter than that for every template this service admits; the bound is applied so
/// a long stack and logical id cannot push the token past what the member takes.
const MAX_CREATION_TOKEN: usize = 64;

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one every other arm of this layer uses for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// Which of the three types one resource is.
///
/// Visible to the whole of this service's state module because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::EFS::FileSystem`.
    FileSystem,
    /// `AWS::EFS::AccessPoint`.
    AccessPoint,
    /// `AWS::EFS::MountTarget`.
    MountTarget,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        EFS_FILE_SYSTEM => Some(Kind::FileSystem),
        EFS_ACCESS_POINT => Some(Kind::AccessPoint),
        EFS_MOUNT_TARGET => Some(Kind::MountTarget),
        _ => None,
    }
}

/// Every property `AWS::EFS::FileSystem` publishes. A declared property this list does not
/// carry is one no call here applies, and is refused rather than dropped.
const FILE_SYSTEM_PROPERTIES: &[&str] = &[
    "AvailabilityZoneName",
    "BackupPolicy",
    "BypassPolicyLockoutSafetyCheck",
    "Encrypted",
    "FileSystemPolicy",
    "FileSystemProtection",
    "FileSystemTags",
    "KmsKeyId",
    "LifecyclePolicies",
    "PerformanceMode",
    "ProvisionedThroughputInMibps",
    "ThroughputMode",
];

/// The properties of a file system that are members of `CreateFileSystem` under the same
/// name (`smithy_query(query="operation",operation="CreateFileSystem")`). The rest of the
/// type's properties are calls of their own (see the module docs).
const FILE_SYSTEM_CREATE_MEMBERS: &[&str] = &[
    "AvailabilityZoneName",
    "Encrypted",
    "KmsKeyId",
    "PerformanceMode",
    "ProvisionedThroughputInMibps",
    "ThroughputMode",
];

/// The members `UpdateFileSystem` takes beside the file system's own id, which are the only
/// two values of a file system that service changes in place.
const FILE_SYSTEM_UPDATE_MEMBERS: &[&str] = &["ProvisionedThroughputInMibps", "ThroughputMode"];

/// Every property `AWS::EFS::AccessPoint` publishes, and the ones that are members of
/// `CreateAccessPoint` under the same name.
const ACCESS_POINT_PROPERTIES: &[&str] = &[
    "AccessPointTags",
    "ClientToken",
    "FileSystemId",
    "PosixUser",
    "RootDirectory",
];
/// See [`ACCESS_POINT_PROPERTIES`].
const ACCESS_POINT_CREATE_MEMBERS: &[&str] =
    &["ClientToken", "FileSystemId", "PosixUser", "RootDirectory"];

/// Every property `AWS::EFS::MountTarget` publishes, which are the members of
/// `CreateMountTarget` under the same name.
const MOUNT_TARGET_PROPERTIES: &[&str] =
    &["FileSystemId", "IpAddress", "SecurityGroups", "SubnetId"];
/// See [`MOUNT_TARGET_PROPERTIES`].
const MOUNT_TARGET_CREATE_MEMBERS: &[&str] = MOUNT_TARGET_PROPERTIES;

/// The member `ModifyMountTargetSecurityGroups` takes a replacement set under, which is the
/// one value of a mount target that service changes in place.
const SECURITY_GROUPS: &str = "SecurityGroups";

/// The four properties of a file system that are calls of their own, with the member each
/// one travels under.
const BACKUP_POLICY: &str = "BackupPolicy";
/// See [`BACKUP_POLICY`].
const FILE_SYSTEM_POLICY: &str = "FileSystemPolicy";
/// See [`BACKUP_POLICY`].
const BYPASS_POLICY_LOCKOUT: &str = "BypassPolicyLockoutSafetyCheck";
/// See [`BACKUP_POLICY`].
const POLICY_MEMBER: &str = "Policy";
/// See [`BACKUP_POLICY`].
const LIFECYCLE_POLICIES: &str = "LifecyclePolicies";
/// See [`BACKUP_POLICY`].
const FILE_SYSTEM_PROTECTION: &str = "FileSystemProtection";

/// How the owning model types the members of these declarations that are NOT text: the two
/// flags a file system carries, its provisioned throughput, and the POSIX identifiers of an
/// access point's user and of its root directory's creation info
/// (`smithy_query(query="shape",shape="PosixUser,CreationInfo")` -> `Uid`, `Gid`,
/// `SecondaryGids`, `OwnerUid` and `OwnerGid` are all `long`, while `Permissions` is a STRING
/// of octal digits — and the resource type publishes every one of them as a string, so the
/// reading has to go both ways).
///
/// Read through the one walk every provisioning arm whose owning model types a member as a
/// number or a boolean shares ([`modelled_declaration`]), so a template that quoted one — or
/// fed it through a `Ref` to a `String` parameter — reaches Amazon EFS as the type that model
/// declares.
fn modelled_as(member: &str) -> ModelledScalar {
    match member {
        "Encrypted" | BYPASS_POLICY_LOCKOUT => ModelledScalar::Boolean,
        "ProvisionedThroughputInMibps" | "Gid" | "Uid" | "SecondaryGids" | "OwnerGid"
        | "OwnerUid" => ModelledScalar::Number,
        // The octal mode bits, which that model types as text: a template that wrote them
        // unquoted states the number a `^[0-7]{3,4}$` member takes as its digits.
        "Permissions" => ModelledScalar::TextThroughout,
        _ => ModelledScalar::AsWritten,
    }
}

/// Create the resource one of the three declarations asks for, in Amazon EFS itself — so a
/// client that deployed the stack can pass `!Ref <resource>` straight to that service's own
/// API, and the id the stack publishes is the one it answered with.
///
/// The identifier Amazon EFS minted REPLACES the stand-in the naming rules computed: it is
/// what `Ref` and the type's identifier attribute resolve to, and what every call of that
/// service on the resource takes.
///
/// `Err` is that service's own refusal — a throughput mode without the provisioned amount it
/// requires, a subnet whose Availability Zone this file system already has a mount target in,
/// a file system it does not hold — which is what the resource fails with.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, &resource.resource_type, kind)?;
    match kind {
        Kind::FileSystem => create_file_system(ctx, region, resource, propagated).await,
        Kind::AccessPoint => create_access_point(ctx, region, resource, propagated).await,
        Kind::MountTarget => create_mount_target(ctx, region, resource).await,
    }
}

/// Create the file system an `AWS::EFS::FileSystem` declares.
///
/// The create carries its own members, the tags the declaration and CloudFormation give the
/// resource, and the idempotency token this resource's identity derives
/// ([`creation_token`]). The four properties that are calls of their own are applied once
/// that service reports the file system `available` ([`readiness`]), which is the only state
/// it takes them in.
async fn create_file_system(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = declared_members(&resource.properties, FILE_SYSTEM_CREATE_MEMBERS);
    request.insert(
        CREATION_TOKEN.to_string(),
        Value::String(creation_token(resource)),
    );
    let tags = declared_and_propagated_tags(Kind::FileSystem, &resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), key_value_tag_list(&tags));
    }
    let answered = call(
        ctx,
        region,
        CREATE_FILE_SYSTEM,
        "POST",
        &format!("{EFS_URI_PREFIX}/file-systems"),
        Some(&Value::Object(request)),
    )
    .await?;
    let file_system_id = answered_identifier(&answered, FILE_SYSTEM_ID, CREATE_FILE_SYSTEM)?;
    let mut attributes = BTreeMap::new();
    attributes.insert(
        FILE_SYSTEM_ID_ATTRIBUTE.to_string(),
        file_system_id.clone(),
    );
    if let Some(arn) = reported_text(&answered, FILE_SYSTEM_ARN) {
        attributes.insert(ARN_ATTRIBUTE.to_string(), arn);
    }
    Ok(Created {
        physical_resource_id: Some(file_system_id),
        attributes,
    })
}

/// Create the access point an `AWS::EFS::AccessPoint` declares.
///
/// `ClientToken` is required by the create and optional on the type, so a declaration stating
/// none has this resource's own derived token ([`creation_token`]): a re-deployment of the
/// same declaration is then the same create, which is what the member is for.
async fn create_access_point(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut request = declared_members(&resource.properties, ACCESS_POINT_CREATE_MEMBERS);
    request
        .entry("ClientToken".to_string())
        .or_insert_with(|| Value::String(creation_token(resource)));
    let tags = declared_and_propagated_tags(Kind::AccessPoint, &resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), key_value_tag_list(&tags));
    }
    let answered = call(
        ctx,
        region,
        CREATE_ACCESS_POINT,
        "POST",
        &format!("{EFS_URI_PREFIX}/access-points"),
        Some(&Value::Object(request)),
    )
    .await?;
    let access_point_id = answered_identifier(&answered, ACCESS_POINT_ID, CREATE_ACCESS_POINT)?;
    let mut attributes = BTreeMap::new();
    attributes.insert(
        ACCESS_POINT_ID_ATTRIBUTE.to_string(),
        access_point_id.clone(),
    );
    if let Some(arn) = reported_text(&answered, ACCESS_POINT_ARN) {
        attributes.insert(ARN_ATTRIBUTE.to_string(), arn);
    }
    Ok(Created {
        physical_resource_id: Some(access_point_id),
        attributes,
    })
}

/// Create the mount target an `AWS::EFS::MountTarget` declares.
///
/// The address is Amazon EFS's to assign — "Otherwise, Amazon EFS assigns a free address in
/// the subnet" — so the `IpAddress` the type publishes is the one the create answered, which
/// replaces the stand-in a declaration that states no address was resolved against.
async fn create_mount_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = declared_members(&resource.properties, MOUNT_TARGET_CREATE_MEMBERS);
    let answered = call(
        ctx,
        region,
        CREATE_MOUNT_TARGET,
        "POST",
        &format!("{EFS_URI_PREFIX}/mount-targets"),
        Some(&Value::Object(request)),
    )
    .await?;
    let mount_target_id = answered_identifier(&answered, MOUNT_TARGET_ID, CREATE_MOUNT_TARGET)?;
    let mut attributes = BTreeMap::new();
    attributes.insert(ID_ATTRIBUTE.to_string(), mount_target_id.clone());
    if let Some(address) = reported_text(&answered, IP_ADDRESS) {
        attributes.insert(IP_ADDRESS_ATTRIBUTE.to_string(), address);
    }
    Ok(Created {
        physical_resource_id: Some(mount_target_id),
        attributes,
    })
}

/// Whether Amazon EFS reports the created resource ready, and the settings a FILE SYSTEM's
/// declaration carries that only an available file system takes APPLIED.
///
/// All three creates answer the resource `creating`, in which that service serves no mount —
/// and refuses a mount target or an access point on a file system that is not `available` —
/// so this asks rather than assumes: `available` is ready, `creating` and `updating` are
/// states a new resource comes out of, and every other state is one no wait brings it out of
/// (a resource that failed, one being torn down out of this stack's reach, one that service no
/// longer holds at all).
///
/// A file system that IS available then takes the four settings its declaration asks for
/// ([`apply_available_settings`]): none of them is a member of `CreateFileSystem` and that
/// service takes each only once the file system is available, so they are applied here rather
/// than dropped — and one it REFUSES is the resource's own failure in that service's words.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some(described) = describe(ctx, region, kind, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon EFS holds no {} {physical_resource_id}",
            resource_noun(kind)
        )));
    };
    let state = reported_text(&described, LIFE_CYCLE_STATE).unwrap_or_default();
    if state == LIFE_CYCLE_AVAILABLE {
        if kind == Kind::FileSystem {
            if let Some(declared) = declared {
                if let Some(refusal) =
                    apply_available_settings(ctx, region, physical_resource_id, declared).await?
                {
                    return Ok(Readiness::Refused(refusal));
                }
            }
        }
        return Ok(Readiness::Ready);
    }
    if LIFE_CYCLE_COMING_UP.contains(&state.as_str()) {
        return Ok(Readiness::Pending(state));
    }
    Ok(Readiness::Failed(format!(
        "Amazon EFS reports the {} {physical_resource_id} in life cycle state {state}",
        resource_noun(kind)
    )))
}

/// The attributes Amazon EFS publishes for a resource it now reports available.
///
/// Nothing is read again: each create answers every published return value of its own type —
/// the file system's id and ARN, the access point's id and ARN, the mount target's id and the
/// address that service assigned it — and the resource already carries them.
pub(super) fn ready_attributes() -> BTreeMap<String, String> {
    BTreeMap::new()
}

/// Whether Amazon EFS has finished removing a deleted resource.
///
/// `DeleteFileSystem` and `DeleteMountTarget` each take the resource away as they answer
/// ("Upon return, the file system no longer exists"), so nothing waits for either. An ACCESS
/// POINT is the one of the three that service keeps describing while it removes it — the
/// modelled `LifeCycleState` carries `deleting` for that window — so its deletion is finished
/// once that service no longer holds it.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    if kind != Kind::AccessPoint {
        return Ok(Removal::Gone);
    }
    let Some(described) = describe(ctx, region, kind, physical_resource_id).await? else {
        return Ok(Removal::Gone);
    };
    // That service still holds it, in whatever state it reports the removal at.
    Ok(Removal::Pending(
        reported_text(&described, LIFE_CYCLE_STATE).unwrap_or_default(),
    ))
}

/// Apply a CHANGED declaration to the resource the stack already has, through the call Amazon
/// EFS publishes for changing it — so the resource keeps the identity that service minted and
/// nothing is replaced.
///
/// `Ok(None)` is a change no call of that service can make, which is reported as not applied
/// rather than as a success that dropped the caller's change: an ACCESS POINT is immutable
/// beside its tags (`CreateAccessPoint` is the only call that takes a POSIX user or a root
/// directory, and that service publishes no update for one), and a file system whose
/// declaration DROPS its backup policy or its protection setting is asking for a state neither
/// put can express — a put states what the file system is to end in, and there is no call that
/// takes the setting away.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource
    // carries the properties it was CREATED with, so a property the new declaration
    // introduces is only in `declared`.
    refuse_unapplied_properties(declared, &resource.resource_type, kind)?;
    match kind {
        Kind::FileSystem => update_file_system(ctx, region, resource, declared).await,
        // Every property of an access point beside its tags is create-only, and the tag set
        // one is to end up carrying is reconciled by its own arm ([`retag`]).
        Kind::AccessPoint => Ok(None),
        Kind::MountTarget => update_mount_target(ctx, region, resource, declared).await,
    }
}

/// Apply a changed file-system declaration: the throughput `UpdateFileSystem` takes, and the
/// four settings that are calls of their own.
async fn update_file_system(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    // A setting the new declaration drops cannot be taken away: a put states the state the
    // file system is to END in, and that service publishes no call that un-states either of
    // these two.
    for dropped in [BACKUP_POLICY, FILE_SYSTEM_PROTECTION] {
        if resource.properties.get(dropped).is_some() && declared.get(dropped).is_none() {
            return Ok(None);
        }
    }
    let file_system_id = resource.physical_resource_id.clone();
    let throughput = declared_members(declared, FILE_SYSTEM_UPDATE_MEMBERS);
    if !throughput.is_empty() {
        call(
            ctx,
            region,
            UPDATE_FILE_SYSTEM,
            "PUT",
            &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}"),
            Some(&Value::Object(throughput)),
        )
        .await?;
    }
    // A policy or a lifecycle configuration the new declaration DROPS is cleared, by the call
    // that service publishes for exactly that: the policy's own delete, and a lifecycle
    // configuration of no policies ("to disable lifecycle management, set LifecyclePolicies to
    // an empty array").
    if resource.properties.get(FILE_SYSTEM_POLICY).is_some()
        && declared.get(FILE_SYSTEM_POLICY).is_none()
    {
        delete_file_system_policy(ctx, region, &file_system_id).await?;
    }
    if resource.properties.get(LIFECYCLE_POLICIES).is_some()
        && declared.get(LIFECYCLE_POLICIES).is_none()
    {
        put_lifecycle_configuration(ctx, region, &file_system_id, &Value::Array(Vec::new()))
            .await?;
    }
    if let Some(refusal) =
        apply_available_settings(ctx, region, &file_system_id, declared).await?
    {
        return Err(refusal);
    }
    Ok(Some(Created::default()))
}

/// Apply a changed mount-target declaration: the security groups in effect, which is the one
/// value of a mount target Amazon EFS changes in place
/// (`ModifyMountTargetSecurityGroups` — "Modifies the set of security groups in effect for a
/// mount target"). Every other property is create-only and replaces the resource instead.
///
/// `Ok(None)` is a declaration that DROPS the set: the call takes the set the mount target is
/// to end up with, and a mount target made with no set carries the VPC's default security
/// group — which is not a set this call can ask for.
async fn update_mount_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let Some(groups) = declared.get(SECURITY_GROUPS).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let mount_target_id = resource.physical_resource_id.clone();
    let request = serde_json::json!({ SECURITY_GROUPS: groups.clone() });
    call(
        ctx,
        region,
        MODIFY_MOUNT_TARGET_SECURITY_GROUPS,
        "PUT",
        &format!("{EFS_URI_PREFIX}/mount-targets/{mount_target_id}/security-groups"),
        Some(&request),
    )
    .await?;
    Ok(Some(Created::default()))
}

/// Bring the tag set of a file system or an access point that ALREADY EXISTS into line with
/// what the deployment now asks for, through that service's own `TagResource` and
/// `UntagResource`.
///
/// The creates' `Tags` member applies to the resource that call creates and to no resource
/// that already exists, and no other call of that service carries a tag — so this is where the
/// set a resource is to end up carrying is reconciled. Read from that service's own
/// `ListTagsForResource` rather than from the stack's record, so the set the resource ends up
/// with is the declared one whatever else touched it.
///
/// A MOUNT TARGET is not taggable at all: the type publishes no tag property, and the
/// `ResourceId` of those calls admits only a file system and an access point.
pub(super) async fn retag(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    if kind == Kind::MountTarget {
        return Ok(());
    }
    let resource_id = resource.physical_resource_id.clone();
    let wanted = declared_and_propagated_tags(kind, declared, propagated);
    let held = match listed_tags(ctx, region, &resource_id).await? {
        Some(held) => held,
        // That service no longer holds the resource, so there is no tag set to reconcile.
        None => return Ok(()),
    };
    let removed: Vec<String> = held
        .keys()
        .filter(|key| !wanted.iter().any(|(wanted, _)| wanted == *key))
        .cloned()
        .collect();
    let added: Vec<(String, String)> = wanted
        .iter()
        .filter(|(key, value)| held.get(key) != Some(value))
        .cloned()
        .collect();
    if !removed.is_empty() {
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", super::percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call(
            ctx,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{EFS_URI_PREFIX}/resource-tags/{resource_id}?{keys}"),
            None,
        )
        .await?;
    }
    if !added.is_empty() {
        let request = serde_json::json!({ TAGS_MEMBER: key_value_tag_list(&added) });
        call(
            ctx,
            region,
            TAG_RESOURCE,
            "POST",
            &format!("{EFS_URI_PREFIX}/resource-tags/{resource_id}"),
            Some(&request),
        )
        .await?;
    }
    Ok(())
}

/// Delete the resource a stack created, addressed by the identifier Amazon EFS minted for it.
///
/// A resource that service no longer holds is already in the state the delete asks for, judged
/// on the CODE each of its deletes answers for one it does not hold and never on a message.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let identifier = resource.physical_resource_id.clone();
    let (operation, uri, gone) = match kind {
        Kind::FileSystem => (
            DELETE_FILE_SYSTEM,
            format!("{EFS_URI_PREFIX}/file-systems/{identifier}"),
            FILE_SYSTEM_NOT_FOUND,
        ),
        Kind::AccessPoint => (
            DELETE_ACCESS_POINT,
            format!("{EFS_URI_PREFIX}/access-points/{identifier}"),
            ACCESS_POINT_NOT_FOUND,
        ),
        Kind::MountTarget => (
            DELETE_MOUNT_TARGET,
            format!("{EFS_URI_PREFIX}/mount-targets/{identifier}"),
            MOUNT_TARGET_NOT_FOUND,
        ),
    };
    match call(ctx, region, operation, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == gone => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as Amazon EFS reports it, for drift detection.
///
/// Every description that service answers carries the create's own members under those same
/// names, so the answer maps onto the property vocabulary one for one — the tag properties
/// each type spells its own way included ([`tag_property`]). A resource that service no longer
/// holds was deleted, which is what `None` says ([`super::ActualConfiguration::read`]).
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let Some(described) = describe(ctx, region, kind, &resource.physical_resource_id).await?
    else {
        return Ok(None);
    };
    let mut configuration = Map::new();
    for property in reported_properties(kind) {
        if let Some(value) = described.get(*property) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    if let Some(tags) = described.get(TAGS_MEMBER) {
        configuration.insert(tag_property(kind).to_string(), tags.clone());
    }
    Ok(Some(Value::Object(configuration)))
}

/// The properties a description of one of the three resources reports back, which are the
/// members its create took.
fn reported_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::FileSystem => FILE_SYSTEM_CREATE_MEMBERS,
        Kind::AccessPoint => ACCESS_POINT_CREATE_MEMBERS,
        Kind::MountTarget => MOUNT_TARGET_CREATE_MEMBERS,
    }
}

/// Apply the settings of a file system's declaration that `CreateFileSystem` does not carry,
/// to a file system Amazon EFS already reports `available`.
///
/// Each is a call of its own and each is refused while the file system is in any other state
/// (their modelled `IncorrectFileSystemLifeCycleState`, HTTP 409), which is why they are made
/// here. `Ok(Some(refusal))` is that service refusing one of them: the setting was asked for
/// and not applied, so the resource fails with that refusal rather than being reported as
/// created with less than its declaration asked for.
async fn apply_available_settings(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    file_system_id: &str,
    declared: &Value,
) -> Result<Option<Refusal>, Refusal> {
    if let Some(policy) = declared.get(BACKUP_POLICY).filter(|value| !value.is_null()) {
        let request = serde_json::json!({ BACKUP_POLICY: policy.clone() });
        if let Err(refusal) = call(
            ctx,
            region,
            PUT_BACKUP_POLICY,
            "PUT",
            &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}/backup-policy"),
            Some(&request),
        )
        .await
        {
            return Ok(Some(refusal));
        }
    }
    if let Some(policy) = declared
        .get(FILE_SYSTEM_POLICY)
        .filter(|value| !value.is_null())
    {
        let mut request = Map::new();
        request.insert(POLICY_MEMBER.to_string(), Value::String(policy_text(policy)));
        if let Some(bypass) = declared.get(BYPASS_POLICY_LOCKOUT) {
            request.insert(
                BYPASS_POLICY_LOCKOUT.to_string(),
                modelled_declaration(BYPASS_POLICY_LOCKOUT, bypass, &modelled_as),
            );
        }
        if let Err(refusal) = call(
            ctx,
            region,
            PUT_FILE_SYSTEM_POLICY,
            "PUT",
            &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}/policy"),
            Some(&Value::Object(request)),
        )
        .await
        {
            return Ok(Some(refusal));
        }
    }
    if let Some(policies) = declared
        .get(LIFECYCLE_POLICIES)
        .filter(|value| !value.is_null())
    {
        if let Err(refusal) =
            put_lifecycle_configuration(ctx, region, file_system_id, policies).await
        {
            return Ok(Some(refusal));
        }
    }
    if let Some(protection) = declared
        .get(FILE_SYSTEM_PROTECTION)
        .filter(|value| !value.is_null())
    {
        if let Err(refusal) = call(
            ctx,
            region,
            UPDATE_FILE_SYSTEM_PROTECTION,
            "PUT",
            &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}/protection"),
            Some(protection),
        )
        .await
        {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

/// Put the lifecycle configuration `policies` states on `file_system_id`, which is the one
/// call that manages a file system's storage transitions.
async fn put_lifecycle_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    file_system_id: &str,
    policies: &Value,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ LIFECYCLE_POLICIES: policies.clone() });
    call(
        ctx,
        region,
        PUT_LIFECYCLE_CONFIGURATION,
        "PUT",
        &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}/lifecycle-configuration"),
        Some(&request),
    )
    .await
    .map(|_| ())
}

/// Take the file-system policy off `file_system_id`, which is what a declaration that no
/// longer states one asks for. A file system that holds none is already in that state.
async fn delete_file_system_policy(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    file_system_id: &str,
) -> Result<(), Refusal> {
    match call(
        ctx,
        region,
        DELETE_FILE_SYSTEM_POLICY,
        "DELETE",
        &format!("{EFS_URI_PREFIX}/file-systems/{file_system_id}/policy"),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == POLICY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The policy document as `PutFileSystemPolicy$Policy` takes it: a string, which is what that
/// member is ("The `FileSystemPolicy` that you're creating", `@length` 1..20000). A template
/// writes the policy as a document, so a document is rendered as its JSON text and a policy
/// already written as text travels as it stands.
fn policy_text(policy: &Value) -> String {
    match policy {
        Value::String(text) => text.clone(),
        document => document.to_string(),
    }
}

/// The resource as Amazon EFS's own read of it reports it, or `None` for one that service no
/// longer holds.
///
/// Each of the three reads is a LIST narrowed to one identifier, which is how that service
/// publishes a read of one resource (`DescribeFileSystems?FileSystemId=`,
/// `DescribeAccessPoints?AccessPointId=`, `DescribeMountTargets?MountTargetId=`), so the one
/// entry it answers is the resource asked about.
async fn describe(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    identifier: &str,
) -> Result<Option<Value>, Refusal> {
    let encoded = super::percent_encoded(identifier);
    let (operation, uri, items, gone) = match kind {
        Kind::FileSystem => (
            DESCRIBE_FILE_SYSTEMS,
            format!("{EFS_URI_PREFIX}/file-systems?FileSystemId={encoded}"),
            "FileSystems",
            FILE_SYSTEM_NOT_FOUND,
        ),
        Kind::AccessPoint => (
            DESCRIBE_ACCESS_POINTS,
            format!("{EFS_URI_PREFIX}/access-points?AccessPointId={encoded}"),
            "AccessPoints",
            ACCESS_POINT_NOT_FOUND,
        ),
        Kind::MountTarget => (
            DESCRIBE_MOUNT_TARGETS,
            format!("{EFS_URI_PREFIX}/mount-targets?MountTargetId={encoded}"),
            "MountTargets",
            MOUNT_TARGET_NOT_FOUND,
        ),
    };
    match call(ctx, region, operation, "GET", &uri, None).await {
        Ok(answered) => Ok(answered
            .get(items)
            .and_then(Value::as_array)
            .and_then(|entries| entries.first())
            .cloned()),
        Err(refusal) if refusal.code == gone => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The tag set Amazon EFS reports for `resource_id`, keyed by tag key, or `None` for a
/// resource that service no longer holds.
async fn listed_tags(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_id: &str,
) -> Result<Option<BTreeMap<String, String>>, Refusal> {
    let encoded = super::percent_encoded(resource_id);
    match call(
        ctx,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &format!("{EFS_URI_PREFIX}/resource-tags/{encoded}"),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(
            answered
                .get(TAGS_MEMBER)
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|tag| {
                    let key = tag.get("Key").and_then(Value::as_str)?;
                    let value = tag.get("Value").and_then(Value::as_str).unwrap_or_default();
                    Some((key.to_string(), value.to_string()))
                })
                .collect(),
        )),
        Err(refusal)
            if refusal.code == FILE_SYSTEM_NOT_FOUND || refusal.code == ACCESS_POINT_NOT_FOUND =>
        {
            Ok(None)
        }
        Err(refusal) => Err(refusal),
    }
}

/// One call to Amazon EFS on the `restJson1` wire its model declares, named as the operation
/// it is ([`call_rest_json_named`]) so a refusal a failed resource carries is the one that
/// service's own SDK would raise.
///
/// The refusal travels exactly as that service worded it: Amazon EFS's resource providers
/// report its own message ([`super::Owner::handler_message`]), so nothing is added around it
/// here either.
async fn call(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json_named(ctx, EFS_SIGNING_NAME, region, operation, method, uri, body).await
}

/// The members of `members` a declaration states, as the request carries them: the property
/// and the member are the same name, so every value goes through as the template wrote it —
/// read as the TYPES the owning model declares its members as ([`modelled_as`]). A property
/// declared as `null` states no value at all, so it is left out rather than sent as a null
/// member.
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    for member in members {
        let Some(value) = properties.get(*member).filter(|value| !value.is_null()) else {
            continue;
        };
        request.insert(
            (*member).to_string(),
            modelled_declaration(member, value, &modelled_as),
        );
    }
    request
}

/// The property a type carries its own tag list under: `FileSystemTags` for a file system and
/// `AccessPointTags` for an access point, neither of which is the `Tags` most types publish.
/// A MOUNT TARGET publishes none, and the empty name matches no property.
fn tag_property(kind: Kind) -> &'static str {
    match kind {
        Kind::FileSystem => "FileSystemTags",
        Kind::AccessPoint => "AccessPointTags",
        Kind::MountTarget => "",
    }
}

/// The tags one of these resources is created with: the ones its own declaration asks for
/// under the property its type spells them with, plus the ones CloudFormation propagates to
/// the resource — less the keys in AWS's reserved namespace, which no public call of that
/// service writes.
///
/// The merge itself is [`tags_of`], the one place that rule is written; it reads the `Tags`
/// property, so a type whose tags live under another name hands them to it under that one.
fn declared_and_propagated_tags(
    kind: Kind,
    properties: &Value,
    propagated: &[(String, String)],
) -> Vec<(String, String)> {
    let declared = properties.get(tag_property(kind)).cloned();
    applicable_tags(&tags_of(
        &serde_json::json!({ "Tags": declared }),
        propagated,
    ))
}

/// The idempotency token the create of a file system or an access point is made under:
/// the identity CloudFormation computed for this resource, which is a function of the stack
/// and the logical id — so the same declaration deployed again is the same create, and two
/// resources of one stack never share a token.
fn creation_token(resource: &StoredResource) -> String {
    let token: String = resource
        .physical_resource_id
        .chars()
        .take(MAX_CREATION_TOKEN)
        .collect();
    if token.is_empty() {
        return resource.logical_id.chars().take(MAX_CREATION_TOKEN).collect();
    }
    token
}

/// The identifier Amazon EFS minted for the resource it answered with.
///
/// An answer carrying none would leave no handle a client could call that service with and
/// nothing for the stack to publish, so it is a failed create rather than a created resource.
fn answered_identifier(
    answered: &Value,
    member: &str,
    operation: &str,
) -> Result<String, Refusal> {
    reported_text(answered, member).ok_or_else(|| Refusal {
        code: "InternalFailure".to_string(),
        message: format!("Amazon EFS answered {operation} without a {member}"),
    })
}

/// One text member of an answer, or `None` where that service reported none.
fn reported_text(answered: &Value, member: &str) -> Option<String> {
    answered
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// What one of the three resources is CALLED in a sentence about it, for the reports a
/// readiness answer words.
fn resource_noun(kind: Kind) -> &'static str {
    match kind {
        Kind::FileSystem => "file system",
        Kind::AccessPoint => "access point",
        Kind::MountTarget => "mount target",
    }
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a resource from being reported as created — or as updated —
/// with less than its declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    kind: Kind,
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    let published: &[&str] = match kind {
        Kind::FileSystem => FILE_SYSTEM_PROPERTIES,
        Kind::AccessPoint => ACCESS_POINT_PROPERTIES,
        Kind::MountTarget => MOUNT_TARGET_PROPERTIES,
    };
    let unapplied: BTreeSet<&str> = properties
        .keys()
        .map(String::as_str)
        .filter(|property| !published.contains(property))
        .collect();
    match unapplied.iter().next() {
        Some(property) => Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One deployed resource, which is all the readers below need.
    fn resource(kind: Kind, physical_resource_id: &str, properties: Value) -> StoredResource {
        let resource_type = match kind {
            Kind::FileSystem => EFS_FILE_SYSTEM,
            Kind::AccessPoint => EFS_ACCESS_POINT,
            Kind::MountTarget => EFS_MOUNT_TARGET,
        };
        StoredResource {
            logical_id: "ARes".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: physical_resource_id.to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: true,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Every type this module owns is resolved from the name a template spells, and nothing
    /// else is.
    #[test]
    fn the_three_types_are_the_ones_this_module_owns() {
        let rows: &[(&str, Option<Kind>)] = &[
            (EFS_FILE_SYSTEM, Some(Kind::FileSystem)),
            (EFS_ACCESS_POINT, Some(Kind::AccessPoint)),
            (EFS_MOUNT_TARGET, Some(Kind::MountTarget)),
            ("AWS::EFS::ReplicationConfiguration", None),
            ("AWS::FSx::FileSystem", None),
        ];
        for (resource_type, expected) in rows {
            let resolved = kind_of(resource_type);
            assert_eq!(
                resolved, *expected,
                "case {resource_type:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }

    /// The declared properties reach each create under that call's own member names, with a
    /// quoted flag and a quoted number carried as the types the owning model declares — at
    /// the depth an access point's POSIX user nests them.
    #[test]
    fn the_declared_properties_are_carried_as_the_model_types_them() {
        let declared = serde_json::json!({
            "Encrypted": "true",
            "PerformanceMode": "generalPurpose",
            "ThroughputMode": "provisioned",
            "ProvisionedThroughputInMibps": "128",
            "FileSystemTags": [{"Key": "Name", "Value": "shared"}],
            "BackupPolicy": {"Status": "ENABLED"}
        });
        let carried = Value::Object(declared_members(&declared, FILE_SYSTEM_CREATE_MEMBERS));
        let expected = serde_json::json!({
            "Encrypted": true,
            "PerformanceMode": "generalPurpose",
            "ThroughputMode": "provisioned",
            "ProvisionedThroughputInMibps": 128
        });
        assert_eq!(
            carried, expected,
            "case \"a full file-system declaration\": expected {expected}, got {carried}"
        );

        let declared = serde_json::json!({
            "FileSystemId": "fs-0123456789abcdef0",
            "PosixUser": {"Uid": "1001", "Gid": "1001", "SecondaryGids": ["2002"]},
            "RootDirectory": {
                "Path": "/app",
                "CreationInfo": {"OwnerUid": "1001", "OwnerGid": "1002", "Permissions": 755}
            }
        });
        let carried = Value::Object(declared_members(&declared, ACCESS_POINT_CREATE_MEMBERS));
        let expected = serde_json::json!({
            "FileSystemId": "fs-0123456789abcdef0",
            "PosixUser": {"Uid": 1001, "Gid": 1001, "SecondaryGids": [2002]},
            "RootDirectory": {
                "Path": "/app",
                "CreationInfo": {"OwnerUid": 1001, "OwnerGid": 1002, "Permissions": "755"}
            }
        });
        assert_eq!(
            carried, expected,
            "case \"an access point declaration\": expected {expected}, got {carried}"
        );
    }

    /// The tags of each taggable type are read from the property that type spells them with,
    /// and merged with the ones CloudFormation propagates — less the reserved namespace.
    #[test]
    fn each_types_own_tag_property_is_the_one_read() {
        let propagated = vec![
            ("aws:cloudformation:stack-name".to_string(), "a-stack".to_string()),
            ("Owner".to_string(), "platform".to_string()),
        ];
        let file_system = serde_json::json!({
            "FileSystemTags": [{"Key": "Name", "Value": "shared"}]
        });
        let read = declared_and_propagated_tags(Kind::FileSystem, &file_system, &propagated);
        assert_eq!(
            read,
            vec![
                ("Name".to_string(), "shared".to_string()),
                ("Owner".to_string(), "platform".to_string()),
            ],
            "case \"a file system's own tag property\": got {read:?}"
        );
        let access_point = serde_json::json!({
            "AccessPointTags": [{"Key": "Name", "Value": "app"}],
            "FileSystemTags": [{"Key": "Name", "Value": "not-this-one"}]
        });
        let read = declared_and_propagated_tags(Kind::AccessPoint, &access_point, &propagated);
        assert_eq!(
            read,
            vec![
                ("Name".to_string(), "app".to_string()),
                ("Owner".to_string(), "platform".to_string()),
            ],
            "case \"an access point's own tag property\": got {read:?}"
        );
        let mount_target = serde_json::json!({"SubnetId": "subnet-0123456789abcdef0"});
        let read = declared_and_propagated_tags(Kind::MountTarget, &mount_target, &propagated);
        assert_eq!(
            read,
            vec![("Owner".to_string(), "platform".to_string())],
            "case \"a mount target, which publishes no tag property\": got {read:?}"
        );
    }

    /// A property no call here applies is refused, naming it; every property each type
    /// publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        let rows: &[(Kind, &str, Value)] = &[
            (
                Kind::FileSystem,
                EFS_FILE_SYSTEM,
                serde_json::json!({
                    "AvailabilityZoneName": "us-east-1a",
                    "BackupPolicy": {"Status": "ENABLED"},
                    "BypassPolicyLockoutSafetyCheck": false,
                    "Encrypted": true,
                    "FileSystemPolicy": {},
                    "FileSystemProtection": {"ReplicationOverwriteProtection": "ENABLED"},
                    "FileSystemTags": [],
                    "KmsKeyId": "alias/aws/elasticfilesystem",
                    "LifecyclePolicies": [],
                    "PerformanceMode": "generalPurpose",
                    "ProvisionedThroughputInMibps": 1,
                    "ThroughputMode": "provisioned"
                }),
            ),
            (
                Kind::AccessPoint,
                EFS_ACCESS_POINT,
                serde_json::json!({
                    "AccessPointTags": [],
                    "ClientToken": "a-token",
                    "FileSystemId": "fs-0123456789abcdef0",
                    "PosixUser": {},
                    "RootDirectory": {}
                }),
            ),
            (
                Kind::MountTarget,
                EFS_MOUNT_TARGET,
                serde_json::json!({
                    "FileSystemId": "fs-0123456789abcdef0",
                    "IpAddress": "10.0.0.5",
                    "SecurityGroups": [],
                    "SubnetId": "subnet-0123456789abcdef0"
                }),
            ),
        ];
        for (kind, resource_type, declared) in rows {
            assert!(
                refuse_unapplied_properties(declared, resource_type, *kind).is_ok(),
                "case {resource_type:?}: expected every published property to be applied, got \
                 a refusal"
            );
            let unknown = serde_json::json!({"Unheard": 1});
            let refusal = refuse_unapplied_properties(&unknown, resource_type, *kind)
                .expect_err("expected a refusal of a property no call carries");
            assert!(
                refusal.message.contains("Unheard")
                    && refusal.message.contains(*resource_type),
                "case {resource_type:?}: expected the refusal to name the property and the \
                 type, got {:?}",
                refusal.message
            );
        }
    }

    /// The idempotency token is derived from the resource's own computed identity, inside the
    /// bound the member admits, and a resource with no identity yet falls back to its logical
    /// id rather than to an empty token the member would refuse.
    #[test]
    fn the_creation_token_is_derived_from_the_resources_identity() {
        let rows: &[(&str, &str, &str)] = &[
            (
                "a computed identity",
                "fs-0123456789abcdef0",
                "fs-0123456789abcdef0",
            ),
            ("no identity yet", "", "ARes"),
        ];
        for (case, identity, expected) in rows {
            let token = creation_token(&resource(Kind::FileSystem, identity, Value::Null));
            assert_eq!(
                token, *expected,
                "case {case:?}: expected {expected:?}, got {token:?}"
            );
            assert!(
                !token.is_empty() && token.chars().count() <= MAX_CREATION_TOKEN,
                "case {case:?}: expected a token inside the member's own bound, got {token:?}"
            );
        }
    }

    /// A policy document a template writes as a document reaches the put as its JSON text,
    /// and one already written as text travels as it stands.
    #[test]
    fn a_declared_policy_reaches_the_put_as_text() {
        let document = serde_json::json!({"Version": "2012-10-17", "Statement": []});
        let text = policy_text(&document);
        assert_eq!(
            text,
            document.to_string(),
            "case \"a declared document\": expected its JSON text, got {text:?}"
        );
        let written = Value::String("{\"Version\":\"2012-10-17\"}".to_string());
        let text = policy_text(&written);
        assert_eq!(
            text, "{\"Version\":\"2012-10-17\"}",
            "case \"a policy written as text\": expected it unchanged, got {text:?}"
        );
    }
}
