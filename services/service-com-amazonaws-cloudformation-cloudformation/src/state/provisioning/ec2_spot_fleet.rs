// SPDX-License-Identifier: CC0-1.0

//! `AWS::EC2::SpotFleet`: the Spot Fleet request a stack declares, REQUESTED in Amazon EC2
//! itself.
//!
//! This is the per-type half of provisioning for one resource type, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::ec2_instance`] for the reason that module records — the
//! catalogue file is far past the length a change to this repository may touch — and is reached
//! through the same seam ([`super::Owner`]).
//!
//! ## Why the fleet is really requested
//!
//! A declared resource of a type whose owning service this emulator serves is created by that
//! service's own create call, so the identifier the call answers with is what the stack
//! publishes and the call's REFUSAL is the resource's failure. `AWS::EC2::SpotFleet` is created
//! by `RequestSpotFleet` — "Ref returns the Spot Fleet request ID", the `sfr-…` identifier that
//! service assigns — and that id is the handle every later call about the fleet takes:
//! `DescribeSpotFleetRequests`, `DescribeSpotFleetInstances`, `ModifySpotFleetRequest` and
//! `CancelSpotFleetRequests` are each addressed by it. A resource only RECORDED would publish
//! an id `DescribeSpotFleetRequests` reports nothing for, so a client that reconciles its
//! fleets by the role they act under ([`IAM_FLEET_ROLE`]) would find none of the fleets its
//! stacks deployed, and a declaration naming a role that cannot act as a fleet would deploy as
//! though it could.
//!
//! ## The properties
//!
//! The type publishes ONE property, the fleet's configuration
//! ([`SPOT_FLEET_REQUEST_CONFIG_DATA`]), which `RequestSpotFleet` takes as its own
//! `SpotFleetRequestConfig` member ([`SPOT_FLEET_REQUEST_CONFIG`]): every member below it is a
//! member of that call's `SpotFleetRequestConfigData` under the same name, so the declaration
//! travels member for member ([`SPOT_FLEET_REQUEST_CONFIG_MEMBERS`]) and is written by the one
//! `ec2Query` writer this crate has for this service ([`push_ec2_member`]). A LIST member
//! travels under the element name that protocol flattens it under, which is the member's own
//! `ec2QueryName` where it declares one and its `xmlName` otherwise — the `sent` name each
//! catalogue row carries.
//!
//! A declared property or member this module does not apply is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`], [`sent_structure`]): a fleet requested with less than its
//! declaration asked for is the shallow success this arm exists to avoid.
//!
//! ## What the stack waits for
//!
//! `RequestSpotFleet` answers the request [`BATCH_STATE_SUBMITTED`] — Amazon EC2 is still
//! placing its capacity — and that service reports when it reaches [`BATCH_STATE_ACTIVE`], so
//! the stack holds the resource's creation open until it does ([`readiness`]) and holds its
//! deletion open until that service reports the request cancelled ([`removal`]).

use std::collections::BTreeMap;

use serde_json::Value;

use super::ec2_instance::{
    sent_as_itself, sent_as_structure, StructureMember, BLOCK_DEVICE_MAPPING_MEMBERS,
};
use super::ec2_vpc_network::{
    declared_scalar, refuse_unmapped_properties, EC2_API_VERSION, EC2_SIGNING_NAME,
    NOT_FOUND_SUFFIX,
};
use super::{call_query, xml_element, xml_scalar_children, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::compute_fleet::push_ec2_member;
use crate::state::StoredResource;

/// The resource type this module provisions, named where every other type's name that the
/// return-value rules carry is named ([`crate::state::template::EC2_SPOT_FLEET`]).
pub(super) use crate::state::template::EC2_SPOT_FLEET;

/// The code a refusal of this module's own carries for a declaration it cannot read at all.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code a refusal of this module's own carries for a declaration it cannot act on.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The one property `AWS::EC2::SpotFleet` publishes: "the configuration for the Spot Fleet
/// request", which that type publishes "Required: Yes".
pub(super) const SPOT_FLEET_REQUEST_CONFIG_DATA: &str = "SpotFleetRequestConfigData";

/// Every property of the type, which is that one. A declared property outside this list is one
/// no call here applies, and is refused rather than dropped.
const SPOT_FLEET_PROPERTIES: &[&str] = &[SPOT_FLEET_REQUEST_CONFIG_DATA];

/// The member `RequestSpotFleet` takes the configuration as
/// (`RequestSpotFleetRequest$SpotFleetRequestConfig`), which is the type's own property under
/// the name that call gives it.
const SPOT_FLEET_REQUEST_CONFIG: &str = "SpotFleetRequestConfig";

/// "The Amazon Resource Name (ARN) of an Identity and Access Management (IAM) role that grants
/// the Spot Fleet the permission to request, launch, terminate, and tag instances on your
/// behalf" (`SpotFleetRequestConfigData$IamFleetRole`): the member a client enumerating the
/// fleets one role owns reads, and the member Amazon EC2 refuses a request it cannot act under
/// on.
const IAM_FLEET_ROLE: &str = "IamFleetRole";

/// The attribute `AWS::EC2::SpotFleet` publishes through `Fn::GetAtt` ("Id: the ID of the Spot
/// Fleet"), which is also what its `Ref` returns.
pub(super) const ID_ATTRIBUTE: &str = "Id";

/// The member Amazon EC2 reports a Spot Fleet request's id under
/// (`RequestSpotFleetResponse$SpotFleetRequestId`, `xmlName: spotFleetRequestId`).
const SPOT_FLEET_REQUEST_ID_MEMBER: &str = "spotFleetRequestId";

/// The member `ModifySpotFleetRequest` addresses the fleet it changes by.
const SPOT_FLEET_REQUEST_ID: &str = "SpotFleetRequestId";

/// A member that is a LIST whose entries are structures, carried under the element name
/// `ec2Query` flattens that list under (`sent`) with the entry members `nested`.
///
/// The three constructors this shares with the instance arm cover a scalar, a renamed scalar
/// and a nested structure; the configuration below also nests LISTS of structures, several of
/// them under an element name that is not the property's own — the security groups of a launch
/// specification travel as `GroupSet` — which is this case.
const fn sent_as_entries(
    property: &'static str,
    sent: &'static str,
    nested: &'static [StructureMember],
) -> StructureMember {
    StructureMember {
        property,
        sent,
        nested,
        fixed: None,
    }
}

/// The parameter one Spot Fleet request id travels to a describe or a cancel under: the first
/// entry of that call's own `SpotFleetRequestId` list.
const SPOT_FLEET_REQUEST_ID_PARAMETER: &str = "SpotFleetRequestId.1";

/// The member Amazon EC2 reports a Spot Fleet request's state under
/// (`SpotFleetRequestConfig$SpotFleetRequestState`, `xmlName: spotFleetRequestState`).
const SPOT_FLEET_REQUEST_STATE_MEMBER: &str = "spotFleetRequestState";

/// The element the describe nests its fleets in
/// (`DescribeSpotFleetRequestsResponse$SpotFleetRequestConfigs`, `xmlName:
/// spotFleetRequestConfigSet`), so an element of the same name elsewhere in the document cannot
/// be mistaken for one of this fleet's members.
const SPOT_FLEET_REQUEST_CONFIG_SET: &str = "<spotFleetRequestConfigSet>";

/// The `submitted` member of the `BatchState` catalog: "the Spot Fleet request is being
/// evaluated and Amazon EC2 is preparing to launch the target number of instances", which is
/// the state `RequestSpotFleet` answers a fresh request in.
const BATCH_STATE_SUBMITTED: &str = "submitted";

/// The `active` member of that catalog: the request has been validated and Amazon EC2 is
/// attempting to maintain the target capacity, which is the state a fleet is ready in.
const BATCH_STATE_ACTIVE: &str = "active";

/// The `modifying` member of that catalog: a request whose target capacity is being changed,
/// which is a state a fleet comes out of on its own.
const BATCH_STATE_MODIFYING: &str = "modifying";

/// The prefix every CANCELLED state of that catalog carries (`cancelled`,
/// `cancelled_running`, `cancelled_terminating`): the cancellation the delete asked for has
/// been carried out, so the resource is gone as far as the stack is concerned, whatever the
/// instances it left running are still doing.
const BATCH_STATE_CANCELLED_PREFIX: &str = "cancelled";

/// The per-fleet error code `CancelSpotFleetRequests` reports for an id this account holds no
/// Spot Fleet request for (`CancelBatchErrorCode$fleetRequestIdDoesNotExist`), which is a
/// delete asking for a state Amazon EC2 is already in.
const FLEET_REQUEST_ID_DOES_NOT_EXIST: &str = "fleetRequestIdDoesNotExist";

/// Whether the instances a cancelled fleet launched are terminated with it: a stack's Spot
/// Fleet OWNS the capacity it placed, so taking the resource down takes that capacity down
/// with it rather than leaving instances running that nothing manages any more. The member is
/// required by the call (`CancelSpotFleetRequestsRequest$TerminateInstances`).
const TERMINATE_INSTANCES_ON_CANCEL: &str = "true";

/// The members `ModifySpotFleetRequest` takes for a fleet that already exists, each the member
/// of the configuration a declaration states it as.
///
/// Source: that call's own member list (`smithy_query(query="operation",
/// operation="ModifySpotFleetRequest")`) read against the configuration's members — the target
/// capacities it maintains, the policy for the capacity it no longer needs, and the reserved
/// context — so a declaration that states one of them differently is applied to the fleet the
/// stack has rather than replacing it.
const MODIFIABLE_MEMBERS: &[&str] = &[
    "Context",
    "ExcessCapacityTerminationPolicy",
    "OnDemandTargetCapacity",
    "TargetCapacity",
];

/// The published members of a `Tags` entry of a tag specification
/// (`Tag$Key`/`Tag$Value`).
const TAG_MEMBERS: &[StructureMember] = &[sent_as_itself("Key"), sent_as_itself("Value")];

/// The element name `ec2Query` flattens a tag list under (`Tag$…` entries of a
/// `TagSpecification`): the list member's own `xmlName`.
const TAG_ELEMENT: &str = "Tag";

/// The published members of a `TagSpecifications` entry, each a member of that call's own
/// `SpotFleetTagSpecification` (`smithy_query(query="shape",
/// shape="SpotFleetTagSpecification")`): the resource type the tags are for, and the tags
/// themselves, which that shape flattens under `Tag`.
const TAG_SPECIFICATION_MEMBERS: &[StructureMember] = &[
    sent_as_itself("ResourceType"),
    sent_as_entries("Tags", TAG_ELEMENT, TAG_MEMBERS),
];

/// The published member of a `SecurityGroups` entry: the type publishes the group's id, and
/// that call's own `GroupIdentifier` takes it under the same name.
const GROUP_IDENTIFIER_MEMBERS: &[StructureMember] = &[sent_as_itself("GroupId")];

/// The published member of a launch specification's `IamInstanceProfile`
/// (`IamInstanceProfileSpecification$Arn`), which is the member that property type publishes.
const IAM_INSTANCE_PROFILE_MEMBERS: &[StructureMember] = &[sent_as_itself("Arn")];

/// The published member of a launch specification's `Monitoring`
/// (`SpotFleetMonitoring$Enabled`).
const MONITORING_MEMBERS: &[StructureMember] = &[sent_as_itself("Enabled")];

/// The published members of a launch specification's `Placement`, each a member of that call's
/// own `SpotPlacement` under the same name (`smithy_query(query="shape",
/// shape="SpotPlacement")`).
const PLACEMENT_MEMBERS: &[StructureMember] = &[
    sent_as_itself("AvailabilityZone"),
    sent_as_itself("GroupName"),
    sent_as_itself("Tenancy"),
];

/// The published members of a `LaunchSpecifications` entry, each a member of that call's own
/// `SpotFleetLaunchSpecification` (`smithy_query(query="shape",
/// shape="SpotFleetLaunchSpecification")`) — under the same name for a scalar, and under the
/// element name `ec2Query` flattens it as for a list: the security groups travel as `GroupSet`
/// and the launch's own tags as `TagSpecificationSet`, which are those members' `ec2QueryName`s.
///
/// `NetworkInterfaces` and `InstanceRequirements` are the two published members NOT here: each
/// is a shape of its own whose members this arm does not carry, so a declaration stating one is
/// refused naming it rather than launching capacity that is not what the template described.
const LAUNCH_SPECIFICATION_MEMBERS: &[StructureMember] = &[
    sent_as_entries(
        "BlockDeviceMappings",
        "BlockDeviceMapping",
        BLOCK_DEVICE_MAPPING_MEMBERS,
    ),
    sent_as_itself("EbsOptimized"),
    sent_as_structure("IamInstanceProfile", IAM_INSTANCE_PROFILE_MEMBERS),
    sent_as_itself("ImageId"),
    sent_as_itself("InstanceType"),
    sent_as_itself("KernelId"),
    sent_as_itself("KeyName"),
    sent_as_structure("Monitoring", MONITORING_MEMBERS),
    sent_as_structure("Placement", PLACEMENT_MEMBERS),
    sent_as_itself("RamdiskId"),
    sent_as_entries("SecurityGroups", "GroupSet", GROUP_IDENTIFIER_MEMBERS),
    sent_as_itself("SpotPrice"),
    sent_as_itself("SubnetId"),
    sent_as_entries(
        "TagSpecifications",
        "TagSpecificationSet",
        TAG_SPECIFICATION_MEMBERS,
    ),
    sent_as_itself("UserData"),
    sent_as_itself("WeightedCapacity"),
];

/// The published members of the launch template a `LaunchTemplateConfigs` entry names, each a
/// member of that call's own `FleetLaunchTemplateSpecification` under the same name.
const LAUNCH_TEMPLATE_SPECIFICATION_MEMBERS: &[StructureMember] = &[
    sent_as_itself("LaunchTemplateId"),
    sent_as_itself("LaunchTemplateName"),
    sent_as_itself("Version"),
];

/// The published members of an `Overrides` entry of such a configuration, each a member of
/// that call's own `LaunchTemplateOverrides` under the same name
/// (`smithy_query(query="shape",shape="LaunchTemplateOverrides")`).
///
/// `InstanceRequirements` is the one published member NOT here, for the reason
/// [`LAUNCH_SPECIFICATION_MEMBERS`] records.
const LAUNCH_TEMPLATE_OVERRIDE_MEMBERS: &[StructureMember] = &[
    sent_as_itself("AvailabilityZone"),
    sent_as_itself("InstanceType"),
    sent_as_itself("Priority"),
    sent_as_itself("SpotPrice"),
    sent_as_itself("SubnetId"),
    sent_as_itself("WeightedCapacity"),
];

/// The published members of a `LaunchTemplateConfigs` entry, each a member of that call's own
/// `LaunchTemplateConfig` under the same name: the launch template it launches from and the
/// overrides it varies.
const LAUNCH_TEMPLATE_CONFIG_MEMBERS: &[StructureMember] = &[
    sent_as_structure(
        "LaunchTemplateSpecification",
        LAUNCH_TEMPLATE_SPECIFICATION_MEMBERS,
    ),
    sent_as_structure("Overrides", LAUNCH_TEMPLATE_OVERRIDE_MEMBERS),
];

/// The published member of a `ClassicLoadBalancersConfig`
/// (`ClassicLoadBalancersConfig$ClassicLoadBalancers`), whose entries carry a load balancer's
/// name (`ClassicLoadBalancer$Name`).
const CLASSIC_LOAD_BALANCER_MEMBERS: &[StructureMember] = &[sent_as_itself("Name")];
const CLASSIC_LOAD_BALANCERS_CONFIG_MEMBERS: &[StructureMember] = &[sent_as_structure(
    "ClassicLoadBalancers",
    CLASSIC_LOAD_BALANCER_MEMBERS,
)];

/// The published member of a `TargetGroupsConfig` (`TargetGroupsConfig$TargetGroups`), whose
/// entries carry a target group's ARN (`TargetGroup$Arn`).
const TARGET_GROUP_MEMBERS: &[StructureMember] = &[sent_as_itself("Arn")];
const TARGET_GROUPS_CONFIG_MEMBERS: &[StructureMember] =
    &[sent_as_structure("TargetGroups", TARGET_GROUP_MEMBERS)];

/// The published members of the configuration's `LoadBalancersConfig`, each a member of that
/// call's own `LoadBalancersConfig` under the same name.
const LOAD_BALANCERS_CONFIG_MEMBERS: &[StructureMember] = &[
    sent_as_structure(
        "ClassicLoadBalancersConfig",
        CLASSIC_LOAD_BALANCERS_CONFIG_MEMBERS,
    ),
    sent_as_structure("TargetGroupsConfig", TARGET_GROUPS_CONFIG_MEMBERS),
];

/// The published members of the configuration's `SpotMaintenanceStrategies`, each a member of
/// that call's own shape of the same name, down to the capacity-rebalance strategy's own two
/// (`SpotCapacityRebalance$ReplacementStrategy`/`$TerminationDelay`).
const CAPACITY_REBALANCE_MEMBERS: &[StructureMember] = &[
    sent_as_itself("ReplacementStrategy"),
    sent_as_itself("TerminationDelay"),
];
const SPOT_MAINTENANCE_STRATEGIES_MEMBERS: &[StructureMember] =
    &[sent_as_structure("CapacityRebalance", CAPACITY_REBALANCE_MEMBERS)];

/// Every member of `SpotFleetRequestConfigData` this arm carries, each the member
/// `RequestSpotFleet` takes it as.
///
/// Source: AWS's CloudFormation resource reference for the `AWS::EC2::SpotFleet`
/// `SpotFleetRequestConfigData` property type, read against that call's own member list
/// (`smithy_query(query="shape",shape="SpotFleetRequestConfigData")`): every one is that
/// shape's own member under the same name, and the one member whose element name differs is
/// the fleet's own `TagSpecifications`, which that shape flattens under `TagSpecification`
/// (its `xmlName`, the member declaring no `ec2QueryName`).
///
/// `FulfilledCapacity` and `OnDemandFulfilledCapacity` are members of the shape that the type
/// does not publish and a caller cannot set, and `ClientToken` is not published either, so none
/// of them is here: a declaration stating one is refused naming it.
const SPOT_FLEET_REQUEST_CONFIG_MEMBERS: &[StructureMember] = &[
    sent_as_itself("AllocationStrategy"),
    sent_as_itself("Context"),
    sent_as_itself("ExcessCapacityTerminationPolicy"),
    sent_as_itself(IAM_FLEET_ROLE),
    sent_as_itself("InstanceInterruptionBehavior"),
    sent_as_itself("InstancePoolsToUseCount"),
    sent_as_structure("LaunchSpecifications", LAUNCH_SPECIFICATION_MEMBERS),
    sent_as_structure("LaunchTemplateConfigs", LAUNCH_TEMPLATE_CONFIG_MEMBERS),
    sent_as_structure("LoadBalancersConfig", LOAD_BALANCERS_CONFIG_MEMBERS),
    sent_as_itself("OnDemandAllocationStrategy"),
    sent_as_itself("OnDemandMaxTotalPrice"),
    sent_as_itself("OnDemandTargetCapacity"),
    sent_as_itself("ReplaceUnhealthyInstances"),
    sent_as_structure(
        "SpotMaintenanceStrategies",
        SPOT_MAINTENANCE_STRATEGIES_MEMBERS,
    ),
    sent_as_itself("SpotMaxTotalPrice"),
    sent_as_itself("SpotPrice"),
    sent_as_entries(
        "TagSpecifications",
        "TagSpecification",
        TAG_SPECIFICATION_MEMBERS,
    ),
    sent_as_itself("TargetCapacity"),
    sent_as_itself("TargetCapacityUnitType"),
    sent_as_itself("TerminateInstancesWithExpiration"),
    sent_as_itself("Type"),
    sent_as_itself("ValidFrom"),
    sent_as_itself("ValidUntil"),
];

/// Request the Spot Fleet an `AWS::EC2::SpotFleet` declares, in Amazon EC2 itself — so a client
/// that deployed the stack can pass `!Ref <fleet>` straight to `DescribeSpotFleetRequests`, and
/// a fleet the stack reports created is one that service reports against the fleet role it acts
/// under.
///
/// What this answers as the resource's identity is the `sfr-…` id Amazon EC2 assigned: that
/// service mints it, so the stand-in CloudFormation's naming rules computed is replaced by the
/// identifier really in force ([`crate::state::template::identity_is_service_assigned`] names
/// the type).
///
/// `Err` is that service's own refusal of the request — a fleet role this account's IAM does not
/// hold or that the Spot Fleet service cannot assume, an AMI the account cannot see, a target
/// capacity it will not accept — which is what the resource fails with, because a request that
/// did not answer 2xx requested nothing.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, SPOT_FLEET_PROPERTIES)?;
    let declared = declared_configuration(resource)?;
    let sent = sent_structure(
        resource,
        SPOT_FLEET_REQUEST_CONFIG_DATA,
        declared,
        SPOT_FLEET_REQUEST_CONFIG_MEMBERS,
    )?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    push_ec2_member(&mut parameters, "", SPOT_FLEET_REQUEST_CONFIG, &sent, 0)?;
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "RequestSpotFleet",
        &parameters,
    )
    .await?;
    let fleet_id = xml_element(&answered, SPOT_FLEET_REQUEST_ID_MEMBER)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: INVALID_REQUEST.to_string(),
            message: format!("RequestSpotFleet answered no {SPOT_FLEET_REQUEST_ID_MEMBER}"),
        })?;
    // The type publishes no `Tags` property of its own — a fleet's tags are stated inside its
    // configuration, as the `TagSpecifications` the request carried — so CloudFormation
    // propagates no tag to one and there is no tagging call to make here.
    let mut attributes = BTreeMap::new();
    attributes.insert(ID_ATTRIBUTE.to_string(), fleet_id.clone());
    Ok(Created {
        physical_resource_id: Some(fleet_id),
        attributes,
    })
}

/// Whether Amazon EC2 reports the requested fleet ready for a client to use.
///
/// `RequestSpotFleet` answers the request [`BATCH_STATE_SUBMITTED`], in which the fleet is
/// still being evaluated and has placed nothing, and that service reports when it reaches
/// [`BATCH_STATE_ACTIVE`] — so the stack holds the resource's creation open until it does, and
/// a client that reads the stack as complete finds a fleet that is really holding capacity. A
/// state no wait brings a NEW request out of (a cancellation, a failed request, or a request
/// that service holds none of) fails the resource with what that service reports rather than
/// waiting the window out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(state) = reported_state(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon EC2 holds no Spot Fleet request for {physical_resource_id}"
        )));
    };
    match state.as_str() {
        BATCH_STATE_ACTIVE => Ok(Readiness::Ready),
        BATCH_STATE_SUBMITTED | BATCH_STATE_MODIFYING => Ok(Readiness::Pending(state)),
        other => Ok(Readiness::Failed(format!(
            "Amazon EC2 reports Spot Fleet request {physical_resource_id} in state {other}"
        ))),
    }
}

/// Whether Amazon EC2 has finished removing a deleted fleet.
///
/// `CancelSpotFleetRequests` moves the request into one of the cancelled states
/// ([`BATCH_STATE_CANCELLED_PREFIX`]) and that service keeps the cancelled request on record,
/// so what the stack waits for is the CANCELLATION being in force rather than the record
/// disappearing — a fleet still `active` or `modifying` when the delete's own answer has come
/// back is one that service has not finished with.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    let Some(state) = reported_state(ctx, region, physical_resource_id).await? else {
        return Ok(Removal::Gone);
    };
    if state.starts_with(BATCH_STATE_CANCELLED_PREFIX) || state.is_empty() {
        return Ok(Removal::Gone);
    }
    Ok(Removal::Pending(state))
}

/// Apply a CHANGED `AWS::EC2::SpotFleet` declaration to the fleet the stack requested, so it
/// keeps the `sfr-…` id Amazon EC2 assigned it.
///
/// `ModifySpotFleetRequest` is the one call that changes a Spot Fleet request that exists, and
/// what it takes is [`MODIFIABLE_MEMBERS`] — the capacities the fleet maintains, the policy for
/// capacity it no longer needs, and the reserved context. A declaration that states one of them
/// differently is applied through that call; a change to any other member of the configuration
/// is one no call of that service carries, so it is REFUSED naming the member rather than
/// reported as applied.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let stated = declared_configuration_of(declared, &resource.resource_type)?;
    let held = declared_configuration(resource)?;
    // The declaration is still held to what this arm APPLIES, so a member it does not carry
    // fails the resource here exactly as it does on the way in.
    sent_structure(
        resource,
        SPOT_FLEET_REQUEST_CONFIG_DATA,
        stated,
        SPOT_FLEET_REQUEST_CONFIG_MEMBERS,
    )?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in changed_members(held, stated) {
        if !MODIFIABLE_MEMBERS.contains(&member.as_str()) {
            return Err(Refusal {
                code: NOT_IMPLEMENTED.to_string(),
                message: format!(
                    "this emulator does not change the {member} member of an existing \
                     {EC2_SPOT_FLEET}'s {SPOT_FLEET_REQUEST_CONFIG_DATA}"
                ),
            });
        }
        if let Some(value) = declared_scalar(stated, &member) {
            parameters.push((member, value));
        }
    }
    if parameters.is_empty() {
        return Ok(Some(Created::default()));
    }
    parameters.push((
        SPOT_FLEET_REQUEST_ID.to_string(),
        resource.physical_resource_id.clone(),
    ));
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "ModifySpotFleetRequest",
        &parameters,
    )
    .await?;
    Ok(Some(Created::default()))
}

/// Cancel the Spot Fleet request a stack made, addressed by the id Amazon EC2 minted for it —
/// this resource's physical id, which is what `Ref` returns for the type — and terminate the
/// capacity it placed ([`TERMINATE_INSTANCES_ON_CANCEL`]).
///
/// `CancelSpotFleetRequests` reports its outcome PER FLEET rather than raising, so the
/// cancellation is read off the answer: a fleet that service holds none of
/// ([`FLEET_REQUEST_ID_DOES_NOT_EXIST`]) is already in the state the delete asks for, and any
/// other per-fleet error is the delete's own failure with that service's code and sentence.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let answered = match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CancelSpotFleetRequests",
        &[
            (
                SPOT_FLEET_REQUEST_ID_PARAMETER.to_string(),
                resource.physical_resource_id.clone(),
            ),
            (
                "TerminateInstances".to_string(),
                TERMINATE_INSTANCES_ON_CANCEL.to_string(),
            ),
        ],
    )
    .await
    {
        Ok(answered) => answered,
        // A request that is not one this account holds at all is answered by that service's
        // own not-found code, which is the state the delete asks for too.
        Err(refusal) if refusal.code.ends_with(NOT_FOUND_SUFFIX) => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // One call, one fleet: the answer carries an error element only for a fleet it could not
    // cancel, so a `<code>` anywhere in it is THIS fleet's.
    let Some(code) = xml_element(&answered, "code").filter(|code| !code.is_empty()) else {
        return Ok(());
    };
    if code == FLEET_REQUEST_ID_DOES_NOT_EXIST {
        return Ok(());
    }
    Err(Refusal {
        code,
        message: xml_element(&answered, "message").unwrap_or_default(),
    })
}

/// The declared configuration of a resource record, which the request cannot be made without.
fn declared_configuration(resource: &StoredResource) -> Result<&Value, Refusal> {
    declared_configuration_of(&resource.properties, &resource.resource_type)
}

/// The same reading against a property DOCUMENT: what an UPDATE is handed is the document this
/// deployment declares, which is not the one the record carries.
fn declared_configuration_of<'a>(
    properties: &'a Value,
    resource_type: &str,
) -> Result<&'a Value, Refusal> {
    properties
        .get(SPOT_FLEET_REQUEST_CONFIG_DATA)
        .filter(|value| !value.is_null())
        .ok_or_else(|| Refusal {
            code: INVALID_REQUEST.to_string(),
            message: format!("{resource_type} requires the {SPOT_FLEET_REQUEST_CONFIG_DATA} property"),
        })
}

/// The members of the configuration the two documents state differently, in the order the
/// catalogue publishes them — one entry per member whose declared value changed, including one
/// a deployment added and one it dropped.
fn changed_members(held: &Value, stated: &Value) -> Vec<String> {
    SPOT_FLEET_REQUEST_CONFIG_MEMBERS
        .iter()
        .filter(|member| held.get(member.property) != stated.get(member.property))
        .map(|member| member.property.to_string())
        .collect()
}

/// One declared STRUCTURE read against the members the resource type publishes for it: the same
/// document under the names the CALL takes those members by, which the shared ec2Query writer
/// then flattens ([`push_ec2_member`]).
///
/// The walk runs over the CATALOGUE rather than over the declaration, so its depth is the depth
/// of the shape the call publishes however deeply a declaration nests; the writer bounds the
/// declaration's own depth. A LIST of structures is walked entry by entry, which is what the
/// configuration's own shape nests and the instance arm's does not — the entries then travel
/// under the element name the row's `sent` carries, indexed from one.
///
/// A member outside `members` is REFUSED naming it: a fleet requested without what one of its
/// declared members asked for is not the fleet the template declared.
fn sent_structure(
    resource: &StoredResource,
    property: &str,
    declared: &Value,
    members: &'static [StructureMember],
) -> Result<Value, Refusal> {
    if let Value::Array(entries) = declared {
        let mut sent = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.is_null() {
                continue;
            }
            sent.push(sent_structure(resource, property, entry, members)?);
        }
        return Ok(Value::Array(sent));
    }
    let Some(stated) = declared.as_object() else {
        return Err(Refusal {
            code: INVALID_REQUEST.to_string(),
            message: format!(
                "the {property} property of {} is a structure of the members its property type \
                 publishes",
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
                code: NOT_IMPLEMENTED.to_string(),
                message: format!(
                    "this emulator does not apply the {stated_member} member of the {property} \
                     property of {}",
                    resource.resource_type
                ),
            });
        };
        let read = match published.fixed {
            Some(text) => Value::String(text.to_string()),
            None if published.nested.is_empty() => value.clone(),
            None => sent_structure(resource, property, value, published.nested)?,
        };
        sent.insert(published.sent.to_string(), read);
    }
    Ok(Value::Object(sent))
}

/// The `BatchState` Amazon EC2 reports the Spot Fleet request `fleet_id` in, or `None` for a
/// request it holds none of (`InvalidSpotFleetRequestId.NotFound`, which carries
/// [`NOT_FOUND_SUFFIX`]).
///
/// The ONE read behind [`readiness`] and [`removal`], so both ask that service the same question
/// and read its answer the same way.
async fn reported_state(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    fleet_id: &str,
) -> Result<Option<String>, Refusal> {
    let answered = match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeSpotFleetRequests",
        &[(
            SPOT_FLEET_REQUEST_ID_PARAMETER.to_string(),
            fleet_id.to_string(),
        )],
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code.ends_with(NOT_FOUND_SUFFIX) => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let reported = fleet_entry(&answered);
    Ok(reported
        .get(SPOT_FLEET_REQUEST_STATE_MEMBER)
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// The fleet an answer carries, as the scalar members Amazon EC2 reports for it.
///
/// Read out of the set the describe nests its fleets in
/// ([`SPOT_FLEET_REQUEST_CONFIG_SET`]), so an element of the same name elsewhere in the
/// document cannot be mistaken for one of this fleet's members. Every call through here names
/// ONE fleet, so that set holds one entry.
fn fleet_entry(document: &str) -> ::serde_json::Map<String, Value> {
    let entries = document
        .find(SPOT_FLEET_REQUEST_CONFIG_SET)
        .map_or(document, |start| &document[start..]);
    xml_scalar_children(entries, "item")
}
