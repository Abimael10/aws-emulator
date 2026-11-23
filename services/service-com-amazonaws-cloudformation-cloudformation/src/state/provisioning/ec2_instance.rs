// SPDX-License-Identifier: CC0-1.0

//! The Amazon EC2 instance a stack declares, LAUNCHED in Amazon EC2 itself.
//!
//! ## The rule this module carries
//!
//! A declared resource of a type whose owning service this emulator serves is created by that
//! service's own create call, so the call's REFUSAL is the resource's failure and the
//! identifier the call answers with is what the stack publishes. `AWS::EC2::Instance` is
//! created by `RunInstances` — "Ref returns the ID of the instance", the `i-…` identifier that
//! service assigns — so a declaration Amazon EC2 will not launch (an `ImageId` the account
//! cannot see, a subnet it does not hold, an `InstanceType` the service does not publish)
//! fails THAT resource with Amazon EC2's own refusal as its `ResourceStatusReason`, the
//! resources declared after it are never asked for, and the stack rolls back with no instance
//! left behind. A resource only RECORDED would publish an identifier
//! `DescribeInstances` reports nothing for, and a template that cannot work in Amazon EC2
//! would deploy as though it could.
//!
//! This type lives in its own module beside the algorithm that drives it for the reason
//! [`super::ec2_vpc_network`] records: the per-type catalogue of
//! [`super::super::provisioner`] is far past the length a change to this repository may
//! touch, so a type added now brings its own module. The ec2Query DECLARATION READERS it is
//! built from ([`scalar`], [`list`], [`required`], [`refuse_unmapped_properties`]) and the one
//! call that tags an Amazon EC2 resource ([`tag_resource`]) are that module's, shared rather
//! than copied: both modules write the same wire for the same service. The seam that reaches
//! this one is [`super::Owner`].
//!
//! ## What the type's properties are
//!
//! Source: AWS's CloudFormation resource reference for `AWS::EC2::Instance` read against
//! `RunInstances`'s own member list
//! (`smithy_query(query="shape",shape="RunInstancesRequest")`). The properties below are that
//! call's own members — most of them under the same name, the placement properties inside its
//! `Placement` structure, the monitoring switch inside its `Monitoring` one, the instance
//! profile inside its `IamInstanceProfile` one and the launch template inside its
//! `LaunchTemplate` one — and `MinCount`/`MaxCount` are
//! CloudFormation's to decide, a declaration of this type being exactly ONE instance.
//!
//! A declaration whose launch parameters come from a LAUNCH TEMPLATE states the template
//! rather than restating them, and the launch carries that template: "any additional
//! parameters that you specify for the new instance overwrite the corresponding parameters
//! included in the launch template" (`RunInstancesRequest$LaunchTemplate`), so Amazon EC2 is
//! the one that merges the two and the instance inherits the template's image, instance type,
//! instance profile, security groups and metadata options ([`launch_template_parameters`]).
//!
//! The tag set reaches Amazon EC2 through the one call that tags a resource that exists, for
//! the reason [`tag_resource`] records: it carries the three `aws:cloudformation:*` keys, and
//! `CreateTags` is the request that service admits them from.
//!
//! Every property of this type that is a member of that call travels to it, whatever its
//! SHAPE: a scalar as that member, a STRUCTURE as that member's own members
//! ([`INSTANCE_STRUCTURE_PROPERTIES`] — the CPU, credit, enclave, hibernation, metadata and
//! private-DNS options) and a LIST OF STRUCTURES as the parameter name `ec2Query` flattens that
//! list under ([`INSTANCE_STRUCTURE_LIST_PROPERTIES`] — the block device mappings, the IPv6
//! addresses, the license specifications and the two accelerator lists). So a declaration that
//! asks for an encrypted root volume or for IMDSv2 is launched with them rather than failed.
//! Each such property is READ against the members the type publishes ([`published_structure`])
//! and WRITTEN by the one ec2Query writer this crate has for this service
//! ([`push_ec2_member`]), which the launch-template arm writes its own nested members with.
//!
//! A declared property this module does not apply is REFUSED
//! ([`refuse_unmapped_properties`]), so an instance is never reported created with less than
//! its declaration asked for. What is left is exactly the properties `RunInstances` has no
//! member for, each of which is another call: `SourceDestCheck` is a
//! `ModifyInstanceAttribute`, an attached `Volumes` entry an `AttachVolume`, an
//! `SsmAssociations` entry a Systems Manager association, and
//! `PropagateTagsToVolumeOnCreation` the tagging of the volumes a launch creates.
//!
//! ## What the stack waits for
//!
//! `RunInstances` answers the instance `pending` — nothing runs on it in that state — and
//! Amazon EC2 reports when it reaches `running`, so the stack holds the resource's creation
//! open until it does ([`readiness`]) and holds its deletion open until that service reports
//! it `terminated` ([`removal`]). The addresses and DNS names the type publishes are not
//! assigned until the instance is up, which is why they are read again once it is
//! ([`ready_attributes`]); until Amazon EC2 reports one, the attribute stays the EMPTY value
//! the naming rules gave it, which is what AWS answers for an address an instance has none of.

use std::collections::BTreeMap;

use serde_json::Value;

use super::ec2_vpc_network::{
    list, refuse_unmapped_properties, required, scalar, tag_resource, EC2_API_VERSION,
    EC2_SIGNING_NAME, NOT_FOUND_SUFFIX,
};
use super::{call_query, xml_scalar_children, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::compute_fleet::{ec2_list_element, push_ec2_member};
use crate::state::provisioner::ec2_in_place::{retag_ec2_resource, update_ec2_instance};
use crate::state::provisioner::tags_of;
use crate::state::StoredResource;

/// The resource type this module provisions.
pub(super) const EC2_INSTANCE: &str = "AWS::EC2::Instance";

/// Every property `AWS::EC2::Instance` publishes that this module applies, from AWS's
/// CloudFormation resource reference for the type read against `RunInstances`'s member list.
///
/// A property outside this list is refused rather than dropped
/// ([`refuse_unmapped_properties`]).
const INSTANCE_PROPERTIES: &[&str] = &[
    "AdditionalInfo",
    "Affinity",
    "AvailabilityZone",
    "BlockDeviceMappings",
    "CpuOptions",
    "CreditSpecification",
    "DisableApiTermination",
    "EbsOptimized",
    "ElasticGpuSpecifications",
    "ElasticInferenceAccelerators",
    "EnclaveOptions",
    "HibernationOptions",
    "HostId",
    "HostResourceGroupArn",
    "IamInstanceProfile",
    "ImageId",
    "InstanceInitiatedShutdownBehavior",
    "InstanceType",
    "Ipv6AddressCount",
    "Ipv6Addresses",
    "KernelId",
    "KeyName",
    "LaunchTemplate",
    "LicenseSpecifications",
    "MetadataOptions",
    "Monitoring",
    "NetworkInterfaces",
    "PlacementGroupName",
    "PrivateDnsNameOptions",
    "PrivateIpAddress",
    "RamdiskId",
    "SecurityGroupIds",
    "SecurityGroups",
    "SubnetId",
    "Tags",
    "Tenancy",
    "UserData",
];

/// The properties that are `RunInstances`'s own SCALAR members under the SAME name, each
/// travelling as that member's own ec2Query parameter.
const INSTANCE_SCALAR_MEMBERS: &[&str] = &[
    "AdditionalInfo",
    "DisableApiTermination",
    "EbsOptimized",
    "ImageId",
    "InstanceInitiatedShutdownBehavior",
    "InstanceType",
    "Ipv6AddressCount",
    "KernelId",
    "KeyName",
    "PrivateIpAddress",
    "RamdiskId",
    "SubnetId",
    "UserData",
];

/// The properties that are members of `RunInstances`'s `Placement` structure, each against the
/// member of that shape it becomes (`smithy_query(query="shape",shape="Placement")`): a
/// placement member travels as `Placement.<Member>`.
///
/// `PlacementGroupName` is the one property renamed on the way: the shape's member for the
/// group an instance is in is `GroupName`.
const INSTANCE_PLACEMENT_MEMBERS: &[(&str, &str)] = &[
    ("Affinity", "Affinity"),
    ("AvailabilityZone", "AvailabilityZone"),
    ("HostId", "HostId"),
    ("HostResourceGroupArn", "HostResourceGroupArn"),
    ("PlacementGroupName", "GroupName"),
    ("Tenancy", "Tenancy"),
];

/// The properties that are LIST members of that call, each against the parameter name
/// ec2Query flattens it under — the member's own `xmlName`, numbered from one
/// (`SecurityGroupId.1`), which is how that protocol spells a list.
const INSTANCE_LIST_MEMBERS: &[(&str, &str)] = &[
    ("SecurityGroupIds", "SecurityGroupId"),
    ("SecurityGroups", "SecurityGroup"),
];

/// One member of a declared STRUCTURE property: the member of the call's own shape it travels
/// as, and — where that member is itself a structure — the members of IT that are written.
///
/// The three constructors below are the three cases the catalogues need: a member spelled the
/// same on the type and on the call, a member the call takes under another name, and a member
/// that is a nested structure.
#[derive(Debug)]
struct StructureMember {
    /// The member as the resource type publishes it.
    property: &'static str,
    /// The member as the call takes it, which is also its `ec2QueryName`.
    sent: &'static str,
    /// The members of the nested structure this member is, empty for a scalar one.
    nested: &'static [StructureMember],
    /// The text the member travels as whatever the declaration states it, for the one member
    /// whose published FORM is not the value the call takes: `BlockDeviceMapping$NoDevice` is
    /// "specify an empty string" on the call, where the type publishes it as an empty
    /// structure.
    fixed: Option<&'static str>,
}

/// A member spelled identically on the resource type and on the call.
const fn sent_as_itself(property: &'static str) -> StructureMember {
    StructureMember {
        property,
        sent: property,
        nested: &[],
        fixed: None,
    }
}

/// A member the call takes under a DIFFERENT name than the type publishes it under.
const fn sent_as(property: &'static str, sent: &'static str) -> StructureMember {
    StructureMember {
        property,
        sent,
        nested: &[],
        fixed: None,
    }
}

/// A member that is itself a structure, whose own members are `nested`.
const fn sent_as_structure(
    property: &'static str,
    nested: &'static [StructureMember],
) -> StructureMember {
    StructureMember {
        property,
        sent: property,
        nested,
        fixed: None,
    }
}

/// The published members of an `Ebs` entry of a `BlockDeviceMappings` entry, each a member of
/// `RunInstances`'s own `EbsBlockDevice` under the same name
/// (`smithy_query(query="shape",shape="EbsBlockDevice")`).
const EBS_BLOCK_DEVICE_MEMBERS: &[StructureMember] = &[
    sent_as_itself("DeleteOnTermination"),
    sent_as_itself("Encrypted"),
    sent_as_itself("Iops"),
    sent_as_itself("KmsKeyId"),
    sent_as_itself("SnapshotId"),
    sent_as_itself("Throughput"),
    sent_as_itself("VolumeSize"),
    sent_as_itself("VolumeType"),
];

/// The published members of a `BlockDeviceMappings` entry, each a member of that call's own
/// `BlockDeviceMapping` (`smithy_query(query="shape",shape="BlockDeviceMapping")`).
///
/// `NoDevice` is the one member whose form differs: the type publishes it as an empty
/// structure and the call takes "an empty string", so it travels as that string.
const BLOCK_DEVICE_MAPPING_MEMBERS: &[StructureMember] = &[
    sent_as_itself("DeviceName"),
    sent_as_structure("Ebs", EBS_BLOCK_DEVICE_MEMBERS),
    StructureMember {
        property: "NoDevice",
        sent: "NoDevice",
        nested: &[],
        fixed: Some(""),
    },
    sent_as_itself("VirtualName"),
];

/// The published members of `CpuOptions`, each a member of that call's own `CpuOptionsRequest`
/// under the same name.
const CPU_OPTIONS_MEMBERS: &[StructureMember] = &[
    sent_as_itself("CoreCount"),
    sent_as_itself("ThreadsPerCore"),
];

/// The published member of `CreditSpecification`: the type spells the credit option
/// `CPUCredits` and the call's own `CreditSpecificationRequest` spells it `CpuCredits`, which
/// is the one renaming on the way.
const CREDIT_SPECIFICATION_MEMBERS: &[StructureMember] =
    &[sent_as("CPUCredits", "CpuCredits")];

/// The published member of `EnclaveOptions` (`EnclaveOptionsRequest$Enabled`).
const ENCLAVE_OPTIONS_MEMBERS: &[StructureMember] = &[sent_as_itself("Enabled")];

/// The published member of `HibernationOptions`
/// (`HibernationOptionsRequest$Configured`).
const HIBERNATION_OPTIONS_MEMBERS: &[StructureMember] = &[sent_as_itself("Configured")];

/// The published members of `MetadataOptions`, each a member of that call's own
/// `InstanceMetadataOptionsRequest` under the same name — which is how a declaration asking
/// for IMDSv2 (`HttpTokens: required`) reaches the launch.
const METADATA_OPTIONS_MEMBERS: &[StructureMember] = &[
    sent_as_itself("HttpEndpoint"),
    sent_as_itself("HttpProtocolIpv6"),
    sent_as_itself("HttpPutResponseHopLimit"),
    sent_as_itself("HttpTokens"),
    sent_as_itself("InstanceMetadataTags"),
];

/// The published members of `PrivateDnsNameOptions`, each a member of that call's own
/// `PrivateDnsNameOptionsRequest` under the same name.
const PRIVATE_DNS_NAME_OPTIONS_MEMBERS: &[StructureMember] = &[
    sent_as_itself("EnableResourceNameDnsAAAARecord"),
    sent_as_itself("EnableResourceNameDnsARecord"),
    sent_as_itself("HostnameType"),
];

/// The published member of an `Ipv6Addresses` entry
/// (`InstanceIpv6Address$Ipv6Address`).
const IPV6_ADDRESS_MEMBERS: &[StructureMember] = &[sent_as_itself("Ipv6Address")];

/// The published member of a `LicenseSpecifications` entry
/// (`LicenseConfigurationRequest$LicenseConfigurationArn`).
const LICENSE_SPECIFICATION_MEMBERS: &[StructureMember] =
    &[sent_as_itself("LicenseConfigurationArn")];

/// The published member of an `ElasticGpuSpecifications` entry
/// (`ElasticGpuSpecification$Type`).
const ELASTIC_GPU_MEMBERS: &[StructureMember] = &[sent_as_itself("Type")];

/// The published members of an `ElasticInferenceAccelerators` entry
/// (`ElasticInferenceAccelerator$Type` and `$Count`).
const ELASTIC_INFERENCE_MEMBERS: &[StructureMember] =
    &[sent_as_itself("Count"), sent_as_itself("Type")];

/// The properties that are STRUCTURE members of `RunInstances` under the SAME member name,
/// each with the members of that shape the type publishes: a declared member travels as
/// `<Member>.<Inner>`, which is how ec2Query spells a structure.
const INSTANCE_STRUCTURE_PROPERTIES: &[(&str, &[StructureMember])] = &[
    ("CpuOptions", CPU_OPTIONS_MEMBERS),
    ("CreditSpecification", CREDIT_SPECIFICATION_MEMBERS),
    ("EnclaveOptions", ENCLAVE_OPTIONS_MEMBERS),
    ("HibernationOptions", HIBERNATION_OPTIONS_MEMBERS),
    ("MetadataOptions", METADATA_OPTIONS_MEMBERS),
    ("PrivateDnsNameOptions", PRIVATE_DNS_NAME_OPTIONS_MEMBERS),
];

/// The properties that are LISTS OF STRUCTURES on that call, each with the members of the
/// entry shape the type publishes. The element name `ec2Query` flattens the list under is the
/// shared reading of that question ([`list_element`]).
const INSTANCE_STRUCTURE_LIST_PROPERTIES: &[(&str, &[StructureMember])] = &[
    ("BlockDeviceMappings", BLOCK_DEVICE_MAPPING_MEMBERS),
    ("ElasticGpuSpecifications", ELASTIC_GPU_MEMBERS),
    ("ElasticInferenceAccelerators", ELASTIC_INFERENCE_MEMBERS),
    ("Ipv6Addresses", IPV6_ADDRESS_MEMBERS),
    ("LicenseSpecifications", LICENSE_SPECIFICATION_MEMBERS),
];

/// The element name `ec2Query` flattens one of this type's LIST properties under: the
/// `xmlName` `RunInstances` declares for that member, and the member's own name where it
/// declares none.
///
/// The sibling writer's table ([`ec2_list_element`]) answers that question for every list both
/// calls declare — the block device mappings, the two accelerator lists and the license
/// specifications are flattened identically on each — so it is read rather than re-stated, and
/// only the ONE member the two shapes spell differently is named here:
/// `RunInstancesRequest$Ipv6Addresses` carries `xmlName: Ipv6Address`
/// (`smithy_query(query="operation",operation="RunInstances")`), where the launch template's
/// same-named list carries none.
fn list_element(property: &str) -> &str {
    match property {
        "Ipv6Addresses" => "Ipv6Address",
        other => ec2_list_element(other),
    }
}

/// The published members of the type's `LaunchTemplate` property, each the member of
/// `RunInstances`'s own `LaunchTemplateSpecification` it travels as.
///
/// Source: that shape's member list
/// (`smithy_query(query="shape",shape="LaunchTemplateSpecification")`) read against the
/// type's `LaunchTemplateSpecification` property type — the template's id, its name and the
/// version are the three members of both, each under the same name, so a declared member
/// travels as [`LAUNCH_TEMPLATE`]`.<Member>`.
const LAUNCH_TEMPLATE_MEMBERS: &[&str] = &["LaunchTemplateId", "LaunchTemplateName", "Version"];

/// The property that names the launch template an instance is launched from, which is also the
/// member `RunInstances` carries it under.
const LAUNCH_TEMPLATE: &str = "LaunchTemplate";

/// How many instances a declaration of this type is: ONE. "Ref returns the ID of the
/// instance", singular, so the launch asks for exactly one and the resource IS that instance.
const ONE_INSTANCE: &str = "1";

/// The states Amazon EC2 reports an instance in (`InstanceStateName`): the one it answers a
/// launch in, the one a usable instance is in, and the one a termination has finished in. The
/// `shutting-down` state a termination passes through is every other state a terminated
/// instance can be reported in, so it is read as "not gone yet" rather than named.
const INSTANCE_STATE_PENDING: &str = "pending";
const INSTANCE_STATE_RUNNING: &str = "running";
const INSTANCE_STATE_TERMINATED: &str = "terminated";

/// The `InstanceAttributeName` that names an instance's boot configuration — "the user
/// data", the attribute `DescribeInstanceAttribute` reports it under
/// ([`boot_configuration`]).
const USER_DATA_ATTRIBUTE: &str = "userData";

/// Each `Fn::GetAtt` attribute of this type that Amazon EC2 reports as a DIRECT member of the
/// instance, as the element of an ec2Query answer that carries it.
///
/// Source: the type's "Return values" section read against the `Instance` shape's own wire
/// names (`smithy_query(query="shape",shape="Instance")`) — a public IPv4 address travels as
/// `ipAddress` and a public DNS name as `dnsName`, neither spelled as the property is, and the
/// VPC the instance runs in is its own `vpcId` member. The `AvailabilityZone` the type also
/// publishes is NOT one of them: Amazon EC2 reports it inside the instance's `placement`,
/// which [`placement_zone`] reads.
const INSTANCE_ATTRIBUTE_ELEMENTS: &[(&str, &str)] = &[
    ("PrivateDnsName", "privateDnsName"),
    ("PrivateIp", "privateIpAddress"),
    ("PublicDnsName", "dnsName"),
    ("PublicIp", "ipAddress"),
    ("VpcId", "vpcId"),
];

/// Launch the instance an `AWS::EC2::Instance` declares, in Amazon EC2 — so a client that
/// deployed the stack and then calls `ec2 describe-instances` with the id the stack published
/// reads the instance back, and a template Amazon EC2 would refuse fails the resource instead
/// of being reported deployed.
///
/// `Err` is that service's own refusal of the launch, which is what the resource fails with.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, INSTANCE_PROPERTIES)?;
    // "An AMI ID is required to launch an instance and must be specified here or in a launch
    // template" (`RunInstancesRequest$ImageId`): a declaration that names a launch template
    // takes its image from THAT template — Amazon EC2 reads it, and refuses a launch left with
    // no image either way — so the property is required only of a declaration that names none.
    if declared_launch_template(resource).is_none() {
        required(resource, "ImageId")?;
    }
    let mut parameters: Vec<(String, String)> = vec![
        ("MinCount".to_string(), ONE_INSTANCE.to_string()),
        ("MaxCount".to_string(), ONE_INSTANCE.to_string()),
    ];
    for member in INSTANCE_SCALAR_MEMBERS {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    for (property, member) in INSTANCE_PLACEMENT_MEMBERS {
        if let Some(value) = scalar(resource, property) {
            parameters.push((format!("Placement.{member}"), value));
        }
    }
    for (property, member) in INSTANCE_LIST_MEMBERS {
        for (index, entry) in list(resource, property).iter().enumerate() {
            parameters.push((format!("{member}.{}", index + 1), entry.clone()));
        }
    }
    // "Specifies whether detailed monitoring is enabled for the instance" is a STRUCTURE on
    // that call (`RunInstancesMonitoringEnabled$Enabled`) where the type states it as the one
    // boolean it is, so the declared value travels as that structure's own member.
    if let Some(enabled) = scalar(resource, "Monitoring") {
        parameters.push(("Monitoring.Enabled".to_string(), enabled));
    }
    // "The name of an IAM instance profile" is this type's property, and that call takes the
    // profile as a structure naming it either way (`IamInstanceProfileSpecification$Name`), so
    // the declared name travels as that member.
    if let Some(profile) = scalar(resource, "IamInstanceProfile") {
        parameters.push(("IamInstanceProfile.Name".to_string(), profile));
    }
    // "The launch template to use to launch the instances"
    // (`RunInstancesRequest$LaunchTemplate`): a declaration that names one is launched FROM
    // that template, so the launch carries it ([`launch_template_parameters`]) and Amazon EC2
    // merges the template's parameters with the ones the declaration states itself.
    parameters.extend(launch_template_parameters(resource)?);
    // The properties that are STRUCTURES of that call — the metadata, CPU, credit, enclave,
    // hibernation and private-DNS options — each read against the members the type publishes
    // and written by the shared ec2Query writer.
    for (property, members) in INSTANCE_STRUCTURE_PROPERTIES {
        let Some(declared) = declared_value(resource, property) else {
            continue;
        };
        let sent = published_structure(resource, property, declared, members)?;
        push_ec2_member(&mut parameters, "", property, &sent, 0)?;
    }
    // The properties that are LISTS OF STRUCTURES on that call — the block device mappings,
    // the IPv6 addresses, the license specifications and the two accelerator lists — each
    // entry read the same way and the list flattened under the element name that call gives
    // it ([`list_element`]).
    for (property, members) in INSTANCE_STRUCTURE_LIST_PROPERTIES {
        let declared = declared_list_entries(resource, property);
        if declared.is_empty() {
            continue;
        }
        let mut sent = Vec::with_capacity(declared.len());
        for entry in &declared {
            sent.push(published_structure(resource, property, entry, members)?);
        }
        push_ec2_member(
            &mut parameters,
            "",
            list_element(property),
            &Value::Array(sent),
            0,
        )?;
    }
    // "The network interfaces to associate with the instance": each entry is one interface of
    // `RunInstances`'s own `NetworkInterfaces` list ([`interface_parameters`]), which is how a
    // template puts an instance on a chosen subnet with chosen security groups and a public
    // address.
    parameters.extend(interface_parameters(resource)?);
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "RunInstances",
        &parameters,
    )
    .await?;
    // The launch answers a RESERVATION holding the instances it placed, so the instance is the
    // first entry of that answer's own `instancesSet` — this launch asked for one.
    let reported = instance_entry(&answered);
    let instance_id = reported
        .get("instanceId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: "InvalidRequest".to_string(),
            message: "RunInstances answered no instanceId".to_string(),
        })?
        .to_string();
    tag_resource(
        ctx,
        region,
        &instance_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    Ok(Created {
        physical_resource_id: Some(instance_id),
        attributes: published_attributes(&answered),
    })
}

/// Apply a CHANGED `AWS::EC2::Instance` declaration to the instance the stack launched, so it
/// keeps the `i-…` id Amazon EC2 assigned it ([`update_ec2_instance`]).
///
/// Which properties can be applied is read there; a change to one outside that set is reported
/// as not applied, and a create-only property replaces the instance instead
/// ([`crate::state::template::replacement_properties`]).
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    update_ec2_instance(ctx, region, resource, declared).await
}

/// Bring the instance's tags to the ones this deployment gives it, through Amazon EC2's own
/// `CreateTags` and `DeleteTags` ([`retag_ec2_resource`]): `RunInstances` tagged the instance it
/// launched, and no call that changes a live instance carries a tag, so this is where the set an
/// instance is to end up carrying is reconciled.
pub(super) async fn retag(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    retag_ec2_resource(
        ctx,
        region,
        &resource.physical_resource_id,
        &tags_of(declared, propagated),
    )
    .await
}

/// Terminate the instance a stack launched, addressed by the id Amazon EC2 minted for it —
/// this resource's physical id, which is what `Ref` returns for the type.
///
/// An instance that service no longer holds is already in the state the delete asks for
/// ([`NOT_FOUND_SUFFIX`], which `InvalidInstanceID.NotFound` carries), and one a termination
/// has already been asked for is answered by that service as the termination already under
/// way.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "TerminateInstances",
        &[(
            "InstanceId.1".to_string(),
            resource.physical_resource_id.clone(),
        )],
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code.ends_with(NOT_FOUND_SUFFIX) => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether Amazon EC2 reports the launched instance ready for a client to use.
///
/// `RunInstances` answers the instance [`INSTANCE_STATE_PENDING`], in which nothing runs on
/// it, and that service reports when it reaches [`INSTANCE_STATE_RUNNING`] — so the stack holds
/// the resource's creation open until it does, and a client that reads the stack as complete
/// finds an instance it can really use. A state no wait brings a NEW instance out of (either
/// state of a termination, a stopped instance, or an instance that service holds none of)
/// fails the resource with what that service reports rather than waiting the window out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(described) = describe(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon EC2 holds no instance for {physical_resource_id}"
        )));
    };
    match instance_state(&described).as_deref() {
        Some(INSTANCE_STATE_RUNNING) => Ok(Readiness::Ready),
        Some(INSTANCE_STATE_PENDING) => Ok(Readiness::Pending(INSTANCE_STATE_PENDING.to_string())),
        Some(other) => Ok(Readiness::Failed(format!(
            "Amazon EC2 reports instance {physical_resource_id} in state {other}"
        ))),
        None => Ok(Readiness::Failed(format!(
            "Amazon EC2 reports no state for instance {physical_resource_id}"
        ))),
    }
}

/// The attributes Amazon EC2 publishes for an instance it NOW reports running: its addresses
/// and DNS names, which "[are] not available until the instance enters the `running` state"
/// (`Instance$PrivateDnsName`, `$PublicDnsName`), and the zone it was placed in.
///
/// Read once the wait above comes through, so an `Fn::GetAtt` on one of them resolves to the
/// value that service really assigned rather than to the stand-in a reference to a resource
/// that did not exist yet was resolved against.
pub(super) async fn ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    match describe(ctx, region, physical_resource_id).await? {
        Some(described) => Ok(published_attributes(&described)),
        None => Ok(BTreeMap::new()),
    }
}

/// Whether Amazon EC2 has finished terminating a deleted instance.
///
/// `TerminateInstances` moves the instance into `shutting-down` and that service reports
/// [`INSTANCE_STATE_TERMINATED`] when it has finished with it, so the stack holds the
/// resource's deletion open until it is gone there — and a client that reads the deletion as
/// complete finds nothing still being torn down. An instance that service no longer reports at
/// all is gone.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let Some(described) = describe(ctx, region, physical_resource_id).await? else {
        return Ok(Removal::Gone);
    };
    match instance_state(&described) {
        None => Ok(Removal::Gone),
        Some(state) if state == INSTANCE_STATE_TERMINATED => Ok(Removal::Gone),
        Some(state) => Ok(Removal::Pending(state)),
    }
}

/// What Amazon EC2 reports about the instance `instance_id`, or `None` for an instance it does
/// not hold (`InvalidInstanceID.NotFound`, which carries [`NOT_FOUND_SUFFIX`]).
///
/// The ONE read behind [`readiness`], [`ready_attributes`] and [`removal`], so all three ask
/// that service the same question and read its answer the same way.
async fn describe(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_id: &str,
) -> Result<Option<String>, Refusal> {
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeInstances",
        &[("InstanceId.1".to_string(), instance_id.to_string())],
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code.ends_with(NOT_FOUND_SUFFIX) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The LAUNCH TEMPLATE a declaration names, or `None` for one that names none.
///
/// A property stated as nothing (`null`, or an empty structure) names no template, so such a
/// declaration is read as the declaration that states none at all: it is launched from the
/// parameters it spells out itself.
fn declared_launch_template(resource: &StoredResource) -> Option<&::serde_json::Map<String, Value>>
{
    resource
        .properties
        .get(LAUNCH_TEMPLATE)?
        .as_object()
        .filter(|members| !members.is_empty())
}

/// The launch template a declaration names, as the parameters `RunInstances` takes it under —
/// and no parameters at all for a declaration that names none.
///
/// The template reaches the launch rather than being read here: "any additional parameters
/// that you specify for the new instance overwrite the corresponding parameters included in
/// the launch template" (`RunInstancesRequest$LaunchTemplate`), so Amazon EC2 resolves the
/// version the declaration asks for and merges its parameters with the ones the declaration
/// states itself — which is what makes the instance inherit the template's image, instance
/// type, instance profile, security groups and metadata options, and what makes a template or
/// version that service holds none of the RESOURCE's failure with that service's own refusal.
///
/// A declared member outside the property type's own three ([`LAUNCH_TEMPLATE_MEMBERS`]) is
/// refused rather than dropped, for the reason [`refuse_unmapped_properties`] records: an
/// instance launched from less than its declaration named is not the instance the template
/// declared.
fn launch_template_parameters(
    resource: &StoredResource,
) -> Result<Vec<(String, String)>, Refusal> {
    let mut parameters: Vec<(String, String)> = Vec::new();
    // A property stated as nothing (absent, `null`) names no template, so the instance is
    // launched from the parameters the declaration spells out itself; one stated as something
    // other than a structure is not a template specification at all, which is a declaration
    // this launch cannot be made from.
    let members = match resource.properties.get(LAUNCH_TEMPLATE) {
        None | Some(Value::Null) => return Ok(parameters),
        Some(Value::Object(members)) => members,
        Some(_) => {
            return Err(Refusal {
                code: "InvalidRequest".to_string(),
                message: format!(
                    "the {LAUNCH_TEMPLATE} property of {} is a structure of the members its \
                     property type publishes",
                    resource.resource_type
                ),
            })
        }
    };
    for (member, value) in members {
        let Some(published) = LAUNCH_TEMPLATE_MEMBERS
            .iter()
            .find(|published| *published == member)
        else {
            return Err(Refusal {
                code: "NotImplemented".to_string(),
                message: format!(
                    "this emulator does not apply the {member} member of the \
                     {LAUNCH_TEMPLATE} property of {}",
                    resource.resource_type
                ),
            });
        };
        let text = member_text(value);
        // A member stated as the empty string states nothing: the launch carries the members
        // the declaration really named, and Amazon EC2 resolves the rest ("Default: The
        // default version of the launch template", `LaunchTemplateSpecification$Version`).
        if text.is_empty() {
            continue;
        }
        parameters.push((format!("{LAUNCH_TEMPLATE}.{published}"), text));
    }
    Ok(parameters)
}

/// The published members of an `AWS::EC2::Instance` `NetworkInterface` entry that are SCALAR
/// members of `RunInstances`'s own `InstanceNetworkInterfaceSpecification`, each against the
/// parameter name that member travels under.
///
/// Source: the type's `NetworkInterface` property type in AWS's resource and property type
/// reference against
/// `smithy_query(query="shape",shape="InstanceNetworkInterfaceSpecification")` — every one is
/// that shape's own member under the same name, which is also its `ec2QueryName` where it
/// declares one.
const INTERFACE_SCALAR_MEMBERS: &[&str] = &[
    "AssociateCarrierIpAddress",
    "AssociatePublicIpAddress",
    "DeleteOnTermination",
    "Description",
    "DeviceIndex",
    "NetworkInterfaceId",
    "PrivateIpAddress",
    "SecondaryPrivateIpAddressCount",
    "SubnetId",
];

/// The published member of such an entry that is a LIST of that shape, against the parameter
/// name `ec2Query` flattens it under — the member's own `xmlName`, numbered from one.
///
/// `GroupSet` is "the IDs of the security groups for the network interface", which is that
/// shape's `Groups` member (`xmlName: SecurityGroupId`).
const INTERFACE_LIST_MEMBERS: &[(&str, &str)] = &[("GroupSet", "SecurityGroupId")];

/// Every member of an `AWS::EC2::Instance` `NetworkInterface` entry this module applies: the
/// two catalogues above. A member outside them is REFUSED rather than dropped, for the reason
/// [`refuse_unmapped_properties`] records — an interface created without the addresses its
/// declaration asked for is not the interface the template declared, and both lists this
/// leaves out (`Ipv6Addresses` and `PrivateIpAddresses`) are lists of STRUCTURES, which this
/// module writes no parameters for.
fn interface_parameters(resource: &StoredResource) -> Result<Vec<(String, String)>, Refusal> {
    /// The parameter name `RunInstances` takes its interface list under: that member's own
    /// `ec2QueryName` (`NetworkInterface`), numbered from one.
    const INTERFACE: &str = "NetworkInterface";

    let mut parameters: Vec<(String, String)> = Vec::new();
    let declared = match resource.properties.get("NetworkInterfaces") {
        None | Some(Value::Null) => return Ok(parameters),
        Some(Value::Array(interfaces)) => interfaces.clone(),
        // A declaration that writes a single interface without the sequence syntax is the
        // one-entry list of itself, which is how CloudFormation reads a list property.
        Some(single) => vec![single.clone()],
    };
    for (index, interface) in declared.iter().enumerate() {
        let position = index + 1;
        let Some(members) = interface.as_object() else {
            return Err(Refusal {
                code: "InvalidRequest".to_string(),
                message: format!(
                    "a NetworkInterfaces entry of {} is a structure of the members its \
                     property type publishes",
                    resource.resource_type
                ),
            });
        };
        for (member, value) in members {
            if let Some(scalar) = INTERFACE_SCALAR_MEMBERS
                .iter()
                .find(|published| *published == member)
            {
                parameters.push((
                    format!("{INTERFACE}.{position}.{scalar}"),
                    member_text(value),
                ));
                continue;
            }
            if let Some((_, element)) = INTERFACE_LIST_MEMBERS
                .iter()
                .find(|(published, _)| *published == member)
            {
                let entries = match value {
                    Value::Array(entries) => entries.clone(),
                    single => vec![single.clone()],
                };
                for (entry_index, entry) in entries.iter().enumerate() {
                    parameters.push((
                        format!("{INTERFACE}.{position}.{element}.{}", entry_index + 1),
                        member_text(entry),
                    ));
                }
                continue;
            }
            return Err(Refusal {
                code: "NotImplemented".to_string(),
                message: format!(
                    "this emulator does not apply the {member} member of a NetworkInterfaces \
                     entry of {}",
                    resource.resource_type
                ),
            });
        }
    }
    Ok(parameters)
}

/// The declared value of `property`, or `None` where the declaration states none — a property
/// left out, and one stated as `null`, are the same thing to CloudFormation.
fn declared_value<'a>(resource: &'a StoredResource, property: &str) -> Option<&'a Value> {
    resource
        .properties
        .get(property)
        .filter(|value| !value.is_null())
}

/// The entries of a declared LIST property, in the order the declaration states them.
///
/// A declaration that writes a single entry without the sequence syntax is the one-entry list
/// of itself, which is how CloudFormation reads a list property
/// ([`interface_parameters`] reads its own list the same way).
fn declared_list_entries(resource: &StoredResource, property: &str) -> Vec<Value> {
    match declared_value(resource, property) {
        None => Vec::new(),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter(|entry| !entry.is_null())
            .cloned()
            .collect(),
        Some(single) => vec![single.clone()],
    }
}

/// One declared STRUCTURE read against the members the resource type publishes for it: the
/// same structure under the names the CALL takes those members by, which the shared ec2Query
/// writer then flattens ([`push_ec2_member`]).
///
/// The walk runs over the CATALOGUE rather than over the declaration, so its depth is the
/// depth of the shape the call publishes however deeply a declaration nests; the writer bounds
/// the declaration's own depth.
///
/// A member outside `members` is REFUSED naming it, for the reason
/// [`refuse_unmapped_properties`] records: an instance launched without what one of its
/// declared options asked for is not the instance the template declared. `property` names the
/// property the structure belongs to, so the refusal says which declaration it came from.
fn published_structure(
    resource: &StoredResource,
    property: &str,
    declared: &Value,
    members: &[StructureMember],
) -> Result<Value, Refusal> {
    let Some(stated) = declared.as_object() else {
        return Err(Refusal {
            code: "InvalidRequest".to_string(),
            message: format!(
                "the {property} property of {} is a structure of the members its property \
                 type publishes",
                resource.resource_type
            ),
        });
    };
    let mut sent = ::serde_json::Map::new();
    for (stated_member, value) in stated {
        if value.is_null() {
            continue;
        }
        let Some(published) = members
            .iter()
            .find(|published| published.property == stated_member)
        else {
            return Err(Refusal {
                code: "NotImplemented".to_string(),
                message: format!(
                    "this emulator does not apply the {stated_member} member of the \
                     {property} property of {}",
                    resource.resource_type
                ),
            });
        };
        let read = match published.fixed {
            Some(text) => Value::String(text.to_string()),
            None if published.nested.is_empty() => value.clone(),
            None => published_structure(resource, property, value, published.nested)?,
        };
        sent.insert(published.sent.to_string(), read);
    }
    Ok(Value::Object(sent))
}

/// One member of a declared STRUCTURE — an interface entry, a launch template specification —
/// as the text an `ec2Query` parameter carries: a string as written, and a boolean or a number
/// in its own JSON spelling, which is the reading every other declared value of this module
/// goes through ([`scalar`]).
fn member_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

/// The instance an answer carries, as the scalar members Amazon EC2 reports for it.
///
/// Read out of the `instancesSet` the answer nests its instances in, so an element of the same
/// name elsewhere in the document cannot be mistaken for one of this instance's members. Every
/// call through here names ONE instance, so that set holds one entry.
fn instance_entry(document: &str) -> ::serde_json::Map<String, Value> {
    let entries = document
        .find("<instancesSet>")
        .map_or(document, |start| &document[start..]);
    xml_scalar_children(entries, "item")
}

/// The BOOT CONFIGURATION Amazon EC2 holds for one launched instance — "the user data"
/// (`InstanceAttribute$UserData`), as that service stored it — or `None` for an instance
/// launched without any, and for one Amazon EC2 holds nothing for.
///
/// `DescribeInstanceAttribute` reports the attribute asked about as
/// `<userData><value>…</value></userData>`, carrying no value for an instance that has none;
/// the text is the base64 the launch supplied, which is the form the API takes it in.
///
/// This is what says what the instance was told to do when it came up, which the signals a
/// resource's `CreationPolicy` waits for are read against
/// ([`super::instance_signals`]): the instance that sends such a signal on AWS is one whose
/// own boot configuration asks it to.
pub(super) async fn boot_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_id: &str,
) -> Result<Option<String>, Refusal> {
    let answered = match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeInstanceAttribute",
        &[
            ("InstanceId".to_string(), instance_id.to_string()),
            ("Attribute".to_string(), USER_DATA_ATTRIBUTE.to_string()),
        ],
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code.ends_with(NOT_FOUND_SUFFIX) => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(xml_scalar_children(&answered, USER_DATA_ATTRIBUTE)
        .get("value")
        .and_then(Value::as_str)
        .filter(|user_data| !user_data.is_empty())
        .map(str::to_string))
}

/// The state Amazon EC2 reports the described instance in, from the instance's own
/// `instanceState` structure (`Instance$State`, whose `name` is the `InstanceStateName`).
fn instance_state(document: &str) -> Option<String> {
    xml_scalar_children(document, "instanceState")
        .get("name")
        .and_then(Value::as_str)
        .filter(|state| !state.is_empty())
        .map(str::to_string)
}

/// The zone the described instance was placed in, from its own `placement` structure
/// (`Instance$Placement`, whose `availabilityZone` is the zone).
fn placement_zone(document: &str) -> Option<String> {
    xml_scalar_children(document, "placement")
        .get("availabilityZone")
        .and_then(Value::as_str)
        .filter(|zone| !zone.is_empty())
        .map(str::to_string)
}

/// The `Fn::GetAtt` attributes this type publishes, as the answer reports them.
///
/// A value that service has not assigned yet is reported as no attribute rather than as an
/// empty one: an instance that is still coming up has no public address, and the attribute is
/// read again once it is up ([`ready_attributes`]).
fn published_attributes(document: &str) -> BTreeMap<String, String> {
    let reported = instance_entry(document);
    let mut attributes = BTreeMap::new();
    for (attribute, element) in INSTANCE_ATTRIBUTE_ELEMENTS {
        if let Some(value) = reported
            .get(*element)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert((*attribute).to_string(), value.to_string());
        }
    }
    if let Some(zone) = placement_zone(document) {
        attributes.insert("AvailabilityZone".to_string(), zone);
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer document carrying one instance, as Amazon EC2's ec2Query wire spells it.
    fn answer(state: &str, public_ip: &str) -> String {
        format!(
            "<RunInstancesResponse><reservationId>r-1</reservationId><instancesSet><item>\
             <instanceId>i-0123456789abcdef0</instanceId>\
             <instanceState><code>16</code><name>{state}</name></instanceState>\
             <privateIpAddress>10.0.0.5</privateIpAddress>\
             <privateDnsName>ip-10-0-0-5.ec2.internal</privateDnsName>\
             <ipAddress>{public_ip}</ipAddress>\
             <placement><availabilityZone>us-east-1a</availabilityZone></placement>\
             </item></instancesSet></RunInstancesResponse>"
        )
    }

    /// One row of the table: what the case is, the state Amazon EC2 reports the instance in,
    /// the public address it has assigned (empty for one it has not), and the attributes the
    /// type must then publish.
    type Row<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

    /// The instance's identity, its state and the attributes the type publishes are each read
    /// from the member of the answer Amazon EC2 reports them in — including the two whose wire
    /// name is not the property's (`ipAddress`, `dnsName`) — and a value that service has not
    /// assigned is reported as no attribute at all.
    #[test]
    fn the_published_attributes_are_the_members_amazon_ec2_reports() {
        let rows: &[Row<'_>] = &[
            (
                "an instance that is up, with a public address",
                "running",
                "203.0.113.7",
                &[
                    ("AvailabilityZone", "us-east-1a"),
                    ("PrivateDnsName", "ip-10-0-0-5.ec2.internal"),
                    ("PrivateIp", "10.0.0.5"),
                    ("PublicIp", "203.0.113.7"),
                ],
            ),
            (
                "an instance with no public address assigned",
                "pending",
                "",
                &[
                    ("AvailabilityZone", "us-east-1a"),
                    ("PrivateDnsName", "ip-10-0-0-5.ec2.internal"),
                    ("PrivateIp", "10.0.0.5"),
                ],
            ),
        ];
        for (case, state, public_ip, expected) in rows {
            let document = answer(state, public_ip);
            let read = instance_entry(&document)
                .get("instanceId")
                .and_then(Value::as_str)
                .map(str::to_string);
            assert_eq!(
                read.as_deref(),
                Some("i-0123456789abcdef0"),
                "case {case:?}: expected the answer's own instanceId, got {read:?}"
            );
            let reported_state = instance_state(&document);
            assert_eq!(
                reported_state.as_deref(),
                Some(*state),
                "case {case:?}: expected state {state:?}, got {reported_state:?}"
            );
            let attributes = published_attributes(&document);
            let expected: BTreeMap<String, String> = expected
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                attributes, expected,
                "case {case:?}: expected {expected:?}, got {attributes:?}"
            );
        }
    }
}
