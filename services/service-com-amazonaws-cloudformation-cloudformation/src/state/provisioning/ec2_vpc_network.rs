// SPDX-License-Identifier: CC0-1.0

//! The Amazon EC2 route table a stack declares, the subnet association that puts a subnet on
//! it, the VPC endpoint reached through it, the internet and virtual private gateways that
//! give the network a way out, the attachment that puts one of those on a VPC, and the
//! Elastic IP address the network reaches the internet by — each created in Amazon EC2
//! itself.
//!
//! ## The rule this module carries
//!
//! A declared resource of a type whose owning service this emulator serves is CREATED in that
//! service, so the physical id the stack publishes for it — the value a `Ref`, an
//! `Fn::GetAtt` and an `Outputs` entry resolve to — is the identifier that service minted,
//! and the resource is readable through that service's own API. A route table the stack only
//! RECORDED would publish an `rtb-…` id `DescribeRouteTables` reports nothing for, a subnet
//! would go on using its VPC's main table, an internet gateway would publish an `igw-…` id
//! `DescribeInternetGateways` reports nothing for and attach to nothing, and a gateway
//! endpoint's route would be in no table at all: the deployed network would report success
//! and route nothing.
//!
//! These types are the rest of the VPC network a deploy-then-operate template declares
//! beside the VPC, the subnet and the security group [`super::super::provisioner`] already
//! creates. They live in their own module beside the algorithm that drives them because that
//! file is far past the length a change to this repository may touch, so a type added now
//! brings its own module rather than growing one that can no longer be edited — the reason
//! [`super::lambda_function`] records. The seam that reaches them is [`super::Owner`].
//!
//! ## What each type's properties are
//!
//! * `AWS::EC2::RouteTable`'s are `CreateRouteTable`'s own `VpcId` plus the tag set, which
//!   reaches EC2 through the one call that tags a resource that exists ([`tag_resource`]).
//!   "Ref returns the route table ID", which EC2 assigns.
//! * `AWS::EC2::Route`'s are `CreateRoute`'s own members under the same names: the table the
//!   route goes in, the ONE destination it matches on and the ONE target it points at. EC2
//!   mints no identifier for a route — the delete addresses it by its table and its
//!   destination — and "Ref returns the CloudFormation-generated ID of the resource", so this
//!   resource keeps the identity CloudFormation computed for it.
//! * `AWS::EC2::NatGateway`'s are `CreateNatGateway`'s own members under the same names, with
//!   the tag set applied by the tagging call. "Ref returns the ID of the NAT gateway" — the
//!   `nat-…` id EC2 assigns — and the gateway comes up in the non-terminal state that
//!   service answers the create in, so the stack holds its creation open until
//!   `DescribeNatGateways` reports it available ([`readiness`]) and holds its deletion open
//!   until that service reports it deleted ([`removal`]).
//! * `AWS::EC2::SubnetRouteTableAssociation`'s are `AssociateRouteTable`'s own `SubnetId` and
//!   `RouteTableId`. The resource IS the association, and "Ref returns the ID of the subnet
//!   route table association" — the `rtbassoc-…` id that call answers with. It is not a
//!   taggable resource, so no tags are propagated to it.
//! * `AWS::EC2::VPCEndpoint`'s are `CreateVpcEndpoint`'s own members under the same names,
//!   with the tag set again applied by the tagging call, and `PolicyDocument` written as the
//!   JSON text that call takes (a template states it as an object).
//! * `AWS::EC2::InternetGateway`'s only property is its tag set: `CreateInternetGateway`
//!   takes nothing else, and "Ref returns the ID of the internet gateway" — the `igw-…` id
//!   EC2 assigns.
//! * `AWS::EC2::VPNGateway`'s are `CreateVpnGateway`'s own `Type` and `AmazonSideAsn` beside
//!   the tag set, and "Ref returns the VPN gateway ID" — the `vgw-…` id EC2 assigns.
//! * `AWS::EC2::VPCGatewayAttachment`'s are the VPC and the ONE gateway it puts on it, which
//!   is `AttachInternetGateway`'s pair for an internet gateway and `AttachVpnGateway`'s for a
//!   virtual private one. EC2 mints no identifier for the attachment — both detaches address
//!   it by the gateway and the VPC — and "Ref returns the name of the VPC gateway
//!   attachment", so this resource keeps the identity CloudFormation computed for it.
//! * `AWS::EC2::EIP`'s are `AllocateAddress`'s own members under the same names, with the tag
//!   set applied by the same tagging call. "Ref returns the Elastic IP address", which is the
//!   address EC2 allocated, and the allocation id it answers beside that address is the
//!   type's `AllocationId` attribute — the value every VPC use of the address is made with,
//!   and the one the release this module sends on the way out names.
//! * `AWS::EC2::SecurityGroupIngress`'s and `AWS::EC2::SecurityGroupEgress`'s are the group the
//!   rule is on plus the ONE permission it is, which is the same permission an inline rule of
//!   `AWS::EC2::SecurityGroup` declares — so each is one `AuthorizeSecurityGroupIngress` /
//!   `AuthorizeSecurityGroupEgress` call, and the `sgr-…` id EC2 mints for the rule it added is
//!   the resource's identity, which the revoke on the way out addresses the rule by.
//!
//! Each create carries only members of the call it is made with: a declared property this
//! module does not apply is REFUSED ([`refuse_unmapped_properties`]), so a resource is never
//! reported created with less than its declaration asked for.
//!
//! The ec2Query DECLARATION READERS below ([`scalar`], [`list`], [`required`],
//! [`refuse_unmapped_properties`]) and the one call that tags a resource that exists
//! ([`tag_resource`]) are about this SERVICE's wire rather than about these types, so the
//! sibling module that provisions `AWS::EC2::Instance` ([`super::ec2_instance`]) is built from
//! them too rather than from a second copy of them.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    call_query, xml_element, xml_elements, xml_scalar_children, Created, Readiness, Refusal,
    Removal,
};
// One inline rule of a security group is read into the `IpPermission` parameters an authorize
// call takes by the group's own arm, against the published members of that rule property type
// — and a rule declared as a RESOURCE of its own is the same permission under the same member
// names, so the arms below are built from that reading rather than from a second copy of it.
use crate::state::provisioner::{
    ec2_in_place::{
        applied_in_place, changed_properties, every_change_applied, retag_ec2_resource, UNAPPLIED,
    },
    security_group_rule_parameters, tags_of, EC2_SECURITY_GROUP_EGRESS_MEMBERS,
    EC2_SECURITY_GROUP_INGRESS_MEMBERS,
};
use crate::state::template::{EC2_SECURITY_GROUP_EGRESS, EC2_SECURITY_GROUP_INGRESS};
use crate::state::StoredResource;

/// The resource types this module provisions.
pub(super) const EC2_ROUTE_TABLE: &str = "AWS::EC2::RouteTable";
pub(super) const EC2_SUBNET_ROUTE_TABLE_ASSOCIATION: &str =
    "AWS::EC2::SubnetRouteTableAssociation";
pub(super) const EC2_VPC_ENDPOINT: &str = "AWS::EC2::VPCEndpoint";
pub(super) const EC2_INTERNET_GATEWAY: &str = "AWS::EC2::InternetGateway";
pub(super) const EC2_VPN_GATEWAY: &str = "AWS::EC2::VPNGateway";
pub(super) const EC2_VPC_GATEWAY_ATTACHMENT: &str = "AWS::EC2::VPCGatewayAttachment";
pub(super) const EC2_EIP: &str = "AWS::EC2::EIP";
pub(super) const EC2_ROUTE: &str = "AWS::EC2::Route";
pub(super) const EC2_NAT_GATEWAY: &str = "AWS::EC2::NatGateway";
pub(super) const EC2_NETWORK_INTERFACE: &str = "AWS::EC2::NetworkInterface";

/// Amazon EC2's signing name and the API version a request names beside the action, from its
/// model (`sigv4Name: ec2`, `version: 2016-11-15`) — the same pair the VPC and subnet arms of
/// [`super::super::provisioner`] call that service with.
pub(super) const EC2_SIGNING_NAME: &str = "ec2";
pub(super) const EC2_API_VERSION: &str = "2016-11-15";

/// The suffix of every Amazon EC2 error code that says the resource a request named is not
/// there (`InvalidRouteTableID.NotFound`, `InvalidAssociationID.NotFound`,
/// `InvalidVpcEndpointId.NotFound`, `InvalidInternetGatewayID.NotFound`,
/// `InvalidAllocationID.NotFound`, `InvalidAttachment.NotFound`): a delete asking for a state
/// that service is already in.
///
/// Judged on the CODE, never on a message, which carries the request's own name.
pub(super) const NOT_FOUND_SUFFIX: &str = ".NotFound";

/// Amazon EC2's code for a detach of a gateway and a VPC that are not attached to each other
/// (`Gateway.NotAttached`).
///
/// It says the same thing [`NOT_FOUND_SUFFIX`] says of a resource — the delete is asking for
/// the state that service is already in — so a detach refused with it is treated the same
/// way, and only a detach can ever answer it.
const GATEWAY_NOT_ATTACHED: &str = "Gateway.NotAttached";

/// Which of this module's types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`], which is: the algorithm resolves a type's owner once and passes it
/// back to the arms here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::EC2::RouteTable`.
    RouteTable,
    /// `AWS::EC2::SubnetRouteTableAssociation`.
    SubnetRouteTableAssociation,
    /// `AWS::EC2::VPCEndpoint`.
    VpcEndpoint,
    /// `AWS::EC2::InternetGateway`.
    InternetGateway,
    /// `AWS::EC2::VPNGateway`.
    VpnGateway,
    /// `AWS::EC2::VPCGatewayAttachment`.
    VpcGatewayAttachment,
    /// `AWS::EC2::EIP`.
    ElasticIp,
    /// `AWS::EC2::Route`.
    Route,
    /// `AWS::EC2::NatGateway`.
    NatGateway,
    /// `AWS::EC2::NetworkInterface`.
    NetworkInterface,
    /// `AWS::EC2::SecurityGroupIngress`.
    SecurityGroupIngress,
    /// `AWS::EC2::SecurityGroupEgress`.
    SecurityGroupEgress,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        EC2_ROUTE_TABLE => Some(Kind::RouteTable),
        EC2_SUBNET_ROUTE_TABLE_ASSOCIATION => Some(Kind::SubnetRouteTableAssociation),
        EC2_VPC_ENDPOINT => Some(Kind::VpcEndpoint),
        EC2_INTERNET_GATEWAY => Some(Kind::InternetGateway),
        EC2_VPN_GATEWAY => Some(Kind::VpnGateway),
        EC2_VPC_GATEWAY_ATTACHMENT => Some(Kind::VpcGatewayAttachment),
        EC2_EIP => Some(Kind::ElasticIp),
        EC2_ROUTE => Some(Kind::Route),
        EC2_NAT_GATEWAY => Some(Kind::NatGateway),
        EC2_NETWORK_INTERFACE => Some(Kind::NetworkInterface),
        EC2_SECURITY_GROUP_INGRESS => Some(Kind::SecurityGroupIngress),
        EC2_SECURITY_GROUP_EGRESS => Some(Kind::SecurityGroupEgress),
        _ => None,
    }
}

/// Every property `AWS::EC2::RouteTable` publishes, from AWS's CloudFormation resource
/// reference for the type.
const ROUTE_TABLE_PROPERTIES: &[&str] = &["Tags", "VpcId"];

/// Every property `AWS::EC2::SubnetRouteTableAssociation` publishes, from the same reference.
const ASSOCIATION_PROPERTIES: &[&str] = &["RouteTableId", "SubnetId"];

/// Every property `AWS::EC2::InternetGateway` publishes, from the same reference: a gateway
/// of this account is described by nothing but its tags.
const INTERNET_GATEWAY_PROPERTIES: &[&str] = &["Tags"];

/// Every property `AWS::EC2::VPNGateway` publishes, from the same reference.
///
/// `Type` and `AmazonSideAsn` are `CreateVpnGateway`'s own members under those names
/// (`smithy_query(query="operation",operation="CreateVpnGateway")`); the type's published
/// `AvailabilityZone` was withdrawn from the resource reference and is not one of them.
const VPN_GATEWAY_PROPERTIES: &[&str] = &["AmazonSideAsn", "Tags", "Type"];

/// The `AWS::EC2::VPNGateway` properties that are `CreateVpnGateway`'s own SCALAR members
/// under the same name, each travelling as that member's own `ec2Query` parameter.
const VPN_GATEWAY_MEMBERS: &[&str] = &["AmazonSideAsn", "Type"];

/// Every property `AWS::EC2::VPCGatewayAttachment` publishes, from the same reference: the
/// VPC, and the one gateway the attachment puts on it.
const GATEWAY_ATTACHMENT_PROPERTIES: &[&str] = &["InternetGatewayId", "VpcId", "VpnGatewayId"];

/// Every property `AWS::EC2::EIP` publishes, from the same reference read against
/// `AllocateAddress`'s member list
/// (`smithy_query(query="operation",operation="AllocateAddress")`).
///
/// `InstanceId` and `TransferAddress` are not among them HERE: associating an address with an
/// instance is `AssociateAddress` and accepting a transferred one is `AcceptAddressTransfer`,
/// neither of which this module makes, so a declaration carrying either is refused rather
/// than created without it.
const ELASTIC_IP_PROPERTIES: &[&str] = &[
    "Address",
    "Domain",
    "IpamPoolId",
    "NetworkBorderGroup",
    "PublicIpv4Pool",
    "Tags",
];

/// The `AWS::EC2::EIP` properties that are `AllocateAddress`'s own SCALAR members under the
/// same name, each travelling as that member's own `ec2Query` parameter.
const ELASTIC_IP_MEMBERS: &[&str] = &[
    "Address",
    "Domain",
    "IpamPoolId",
    "NetworkBorderGroup",
    "PublicIpv4Pool",
];

/// Every property `AWS::EC2::VPCEndpoint` publishes, from the same reference read against
/// `CreateVpcEndpoint`'s member list
/// (`smithy_query(query="operation",operation="CreateVpcEndpoint")`). `DnsOptions` is not one
/// of them HERE: it is a structure, and this module writes the flat `ec2Query` parameters a
/// scalar and a list take, so a declaration carrying it is refused rather than created
/// without it.
const VPC_ENDPOINT_PROPERTIES: &[&str] = &[
    "IpAddressType",
    "PolicyDocument",
    "PrivateDnsEnabled",
    "ResourceConfigurationArn",
    "RouteTableIds",
    "SecurityGroupIds",
    "ServiceName",
    "ServiceNetworkArn",
    "ServiceRegion",
    "SubnetIds",
    "Tags",
    "VpcEndpointType",
    "VpcId",
];

/// The `AWS::EC2::VPCEndpoint` properties that are `CreateVpcEndpoint`'s own SCALAR members
/// under the same name, each travelling as that member's own `ec2Query` parameter.
const VPC_ENDPOINT_SCALAR_MEMBERS: &[&str] = &[
    "IpAddressType",
    "PrivateDnsEnabled",
    "ResourceConfigurationArn",
    "ServiceName",
    "ServiceNetworkArn",
    "ServiceRegion",
    "VpcEndpointType",
    "VpcId",
];

/// The `AWS::EC2::VPCEndpoint` properties that are LIST members of that call, each against the
/// parameter name `ec2Query` flattens it under — the member's own `xmlName`, numbered from one
/// (`RouteTableId.1`), which is how that protocol spells a list.
const VPC_ENDPOINT_LIST_MEMBERS: &[(&str, &str)] = &[
    ("RouteTableIds", "RouteTableId"),
    ("SecurityGroupIds", "SecurityGroupId"),
    ("SubnetIds", "SubnetId"),
];

/// Every property `AWS::EC2::Route` publishes, from AWS's CloudFormation resource reference
/// for the type read against `CreateRoute`'s member list
/// (`smithy_query(query="operation",operation="CreateRoute")`): each one is that call's own
/// member under the same name, so the declaration IS the request.
const ROUTE_PROPERTIES: &[&str] = &[
    "CarrierGatewayId",
    "CoreNetworkArn",
    "DestinationCidrBlock",
    "DestinationIpv6CidrBlock",
    "DestinationPrefixListId",
    "EgressOnlyInternetGatewayId",
    "GatewayId",
    "InstanceId",
    "LocalGatewayId",
    "NatGatewayId",
    "NetworkInterfaceId",
    "RouteTableId",
    "TransitGatewayId",
    "VpcEndpointId",
    "VpcPeeringConnectionId",
];

/// The three `AWS::EC2::Route` properties that are DESTINATIONS: a route matches on exactly
/// one of them, and it is what identifies the route inside its table — which is how the
/// delete addresses it ([`delete_route`]), EC2 minting no identifier for a route.
const ROUTE_DESTINATIONS: &[&str] = &[
    "DestinationCidrBlock",
    "DestinationIpv6CidrBlock",
    "DestinationPrefixListId",
];

/// Every property `AWS::EC2::NatGateway` publishes that `CreateNatGateway` takes, from the
/// same reference read against that call's member list
/// (`smithy_query(query="operation",operation="CreateNatGateway")`).
///
/// `MaxDrainDurationSeconds` is not among them HERE: it is how long a DELETE drains the
/// gateway's connections, which `DeleteNatGateway` takes no member for in this model, so a
/// declaration carrying it is refused rather than created without it.
const NAT_GATEWAY_PROPERTIES: &[&str] = &[
    "AllocationId",
    "ConnectivityType",
    "PrivateIpAddress",
    "SecondaryAllocationIds",
    "SecondaryPrivateIpAddressCount",
    "SecondaryPrivateIpAddresses",
    "SubnetId",
    "Tags",
];

/// The `AWS::EC2::NatGateway` properties that are `CreateNatGateway`'s own SCALAR members
/// under the same name, each travelling as that member's own `ec2Query` parameter.
const NAT_GATEWAY_SCALAR_MEMBERS: &[&str] = &[
    "AllocationId",
    "ConnectivityType",
    "PrivateIpAddress",
    "SecondaryPrivateIpAddressCount",
    "SubnetId",
];

/// The `AWS::EC2::NatGateway` properties that are LIST members of that call, each against the
/// parameter name `ec2Query` flattens it under — the member's own `xmlName`, numbered from one
/// (`SecondaryAllocationId.1`).
const NAT_GATEWAY_LIST_MEMBERS: &[(&str, &str)] = &[
    ("SecondaryAllocationIds", "SecondaryAllocationId"),
    ("SecondaryPrivateIpAddresses", "SecondaryPrivateIpAddress"),
];

/// The states Amazon EC2 reports a NAT gateway in (`NatGatewayState`): the one it answers a
/// create in, the one a usable gateway is in, and the one a deletion has finished in. The
/// `deleting` state a deletion passes through is every other state a deleted gateway can be
/// reported in, so it is read as "not gone yet" rather than named.
const NAT_GATEWAY_PENDING: &str = "pending";
const NAT_GATEWAY_AVAILABLE: &str = "available";
const NAT_GATEWAY_DELETED: &str = "deleted";

/// Amazon EC2's error code for a NAT gateway this account does not hold
/// (`NatGatewayNotFound`), which does NOT carry [`NOT_FOUND_SUFFIX`] — so the calls that
/// treat "not there" as the state they asked for name it themselves.
const NAT_GATEWAY_NOT_FOUND: &str = "NatGatewayNotFound";

/// Create the resource one of these types declares, in Amazon EC2 itself — so a client that
/// deployed the stack and then calls `ec2 describe-route-tables` (or `describe-vpc-endpoints`)
/// with the id the stack published reads it back, and the subnet the stack deployed really
/// uses the routes the template declared.
///
/// `Err` is EC2's own refusal of the create, which is what the resource fails with: a VPC it
/// does not hold, a subnet of another VPC, a table already associated. The resource is never
/// passed off as created on a call that did not answer 2xx.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::RouteTable => create_route_table(ctx, region, resource, propagated).await,
        Kind::SubnetRouteTableAssociation => create_association(ctx, region, resource).await,
        Kind::VpcEndpoint => create_vpc_endpoint(ctx, region, resource, propagated).await,
        Kind::InternetGateway => {
            create_internet_gateway(ctx, region, resource, propagated).await
        }
        Kind::VpnGateway => create_vpn_gateway(ctx, region, resource, propagated).await,
        Kind::VpcGatewayAttachment => attach_gateway(ctx, region, resource).await,
        Kind::ElasticIp => allocate_address(ctx, region, resource, propagated).await,
        Kind::Route => create_route(ctx, region, resource).await,
        Kind::NatGateway => create_nat_gateway(ctx, region, resource, propagated).await,
        Kind::NetworkInterface => {
            create_network_interface(ctx, region, resource, propagated).await
        }
        // Neither rule type publishes a `Tags` property, so CloudFormation propagates no tag
        // to one: the rule is the permission its declaration states and nothing else.
        Kind::SecurityGroupIngress | Kind::SecurityGroupEgress => {
            authorize_security_group_rule(ctx, region, kind, resource).await
        }
    }
}

/// The properties of each of these types a resource that already exists can take a NEW VALUE
/// for, from AWS's CloudFormation resource reference for the type: the ones it does not publish
/// with "Update requires: `Replacement`", read against the call Amazon EC2 publishes for
/// changing one on the resource that exists.
///
/// * a ROUTE TABLE, an INTERNET GATEWAY, a VPN GATEWAY, an ELASTIC IP ADDRESS and a NAT GATEWAY
///   take only their TAG SET: every other property of each is a member of its create that no
///   call changes afterwards. (A NAT gateway's secondary addresses are updatable on AWS through
///   the assign and unassign calls for them, which this emulator does not serve yet, so a
///   change to one is reported as not applied rather than sent to a call that answers 501.)
/// * an ASSOCIATION takes NONE: both of its properties are the binding it IS, so restating
///   either asks for a different association.
/// * a VPC GATEWAY ATTACHMENT takes the ONE GATEWAY it puts on its VPC — swapping it is a
///   detach of the gateway it had and an attach of the one it now names
///   ([`update_gateway_attachment`]) — while the VPC is the half of its identity the physical
///   id carries.
/// * a ROUTE takes its TARGET, which `ReplaceRoute` "replaces an existing route within a route
///   table" with, while the table it is in and the destination it matches on are its identity.
/// * a VPC ENDPOINT takes the members `ModifyVpcEndpoint` carries beside its tags.
/// * a security group RULE takes its DESCRIPTION, which the
///   `UpdateSecurityGroupRuleDescriptions…` call for its direction writes; everything else
///   about a rule is the permission it IS.
fn updatable_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::RouteTable
        | Kind::InternetGateway
        | Kind::VpnGateway
        | Kind::ElasticIp
        | Kind::NatGateway => &["Tags"],
        // An elastic network INTERFACE takes the two attributes
        // `ModifyNetworkInterfaceAttribute` writes of the ones this module applies — its
        // description and the security groups it is in — beside its tag set; the subnet it is
        // in and the addresses the create gave it are fixed when Amazon EC2 makes it.
        Kind::NetworkInterface => &["Description", "GroupSet", "Tags"],
        Kind::SubnetRouteTableAssociation => &[],
        Kind::VpcGatewayAttachment => &["InternetGatewayId", "VpnGatewayId"],
        Kind::Route => ROUTE_TARGETS,
        Kind::VpcEndpoint => VPC_ENDPOINT_UPDATABLE_PROPERTIES,
        Kind::SecurityGroupIngress | Kind::SecurityGroupEgress => &["Description"],
    }
}

/// The `AWS::EC2::Route` properties that are TARGETS: `CreateRoute`'s target members, which
/// are `ReplaceRoute`'s own under the same names — every property of the type beside the table
/// it is in and the destination it matches on ([`ROUTE_DESTINATIONS`]).
const ROUTE_TARGETS: &[&str] = &[
    "CarrierGatewayId",
    "CoreNetworkArn",
    "EgressOnlyInternetGatewayId",
    "GatewayId",
    "InstanceId",
    "LocalGatewayId",
    "NatGatewayId",
    "NetworkInterfaceId",
    "TransitGatewayId",
    "VpcEndpointId",
    "VpcPeeringConnectionId",
];

/// The `AWS::EC2::VPCEndpoint` properties `ModifyVpcEndpoint` carries, beside the tag set:
/// "modifies attributes of a specified VPC endpoint"
/// (`smithy_query(query="operation",operation="ModifyVpcEndpoint")`) takes the policy, the
/// private-DNS switch, the address type and the three lists of resources the endpoint is
/// reached through — each as the ids to ADD and the ids to REMOVE
/// ([`modify_vpc_endpoint`]). Its service name, its type and its VPC are create-only.
const VPC_ENDPOINT_UPDATABLE_PROPERTIES: &[&str] = &[
    "IpAddressType",
    "PolicyDocument",
    "PrivateDnsEnabled",
    "RouteTableIds",
    "SecurityGroupIds",
    "SubnetIds",
    "Tags",
];

/// Apply a CHANGED declaration to the resource of one of these types the stack created, so the
/// resource keeps the identity it has and the logical resource reports the change as applied.
///
/// Which properties can be applied is the type's own ([`updatable_properties`]), and a change
/// to one OUTSIDE that set is reported as not applied — `Ok(None)`, which
/// [`super::reconcile`] turns into the resource's failure rather than a success that dropped
/// the caller's change. A property that is create-only never reaches here at all: a
/// declaration restating one asks for a DIFFERENT resource, which the deployment replaces
/// ([`crate::state::template::replacement_properties`]).
///
/// The TAG SET is applied by no arm here: it is reconciled after the configuration change, by
/// the one call pair that tags an Amazon EC2 resource that exists ([`retag`]).
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let changed = changed_properties(&resource.properties, declared);
    if !every_change_applied(&changed, updatable_properties(kind)) {
        return Ok(UNAPPLIED);
    }
    match kind {
        // Nothing but the tag set can have changed on any of these, and that is reconciled
        // after the configuration change: the change IS applied, so the resource reports it.
        Kind::RouteTable
        | Kind::InternetGateway
        | Kind::VpnGateway
        | Kind::ElasticIp
        | Kind::NatGateway
        | Kind::SubnetRouteTableAssociation => Ok(applied_in_place()),
        Kind::VpcGatewayAttachment => {
            update_gateway_attachment(ctx, region, resource, declared).await
        }
        Kind::Route => replace_route(ctx, region, resource, declared).await,
        Kind::VpcEndpoint => modify_vpc_endpoint(ctx, region, resource, declared).await,
        Kind::SecurityGroupIngress | Kind::SecurityGroupEgress => {
            update_rule_description(ctx, region, kind, resource, declared).await
        }
        Kind::NetworkInterface => {
            update_network_interface(ctx, region, resource, declared, &changed).await
        }
    }
}

/// Bring the tags of the resource a record names to the ones this deployment gives it, through
/// Amazon EC2's own `CreateTags` and `DeleteTags` ([`retag_ec2_resource`]).
///
/// Only a TAGGABLE resource has any: neither a route, a subnet route table association, a
/// gateway attachment nor a security group rule resource publishes a `Tags` property — the
/// first three are not taggable resources at all and a rule's tags are not CloudFormation's to
/// set — so nothing is asked of that service about one.
pub(super) async fn retag(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    match tagged_id(kind, resource) {
        Some(resource_id) => {
            retag_ec2_resource(ctx, region, &resource_id, &tags_of(declared, propagated)).await
        }
        None => Ok(()),
    }
}

/// The identifier Amazon EC2 holds a resource of this kind's TAGS under, or `None` for a kind
/// that carries none.
///
/// For every taggable kind here it is the resource's own physical id, which IS the identifier
/// that service assigned — except an ELASTIC IP ADDRESS: "Ref returns the Elastic IP address",
/// so that type's physical id is the address itself while the taggable resource is its
/// ALLOCATION, which is the id the create tagged ([`allocate_address`]) and the one the release
/// names.
///
/// `None` for the kinds that have no tags of their own: neither a route, a subnet route table
/// association nor a gateway attachment is a taggable resource at all — none of them publishes
/// a `Tags` property — and a security group RULE's tags are not CloudFormation's to set, the
/// rule types publishing no `Tags` property either.
fn tagged_id(kind: Kind, resource: &StoredResource) -> Option<String> {
    match kind {
        Kind::RouteTable
        | Kind::InternetGateway
        | Kind::VpnGateway
        | Kind::NatGateway
        | Kind::NetworkInterface
        | Kind::VpcEndpoint => Some(resource.physical_resource_id.clone()),
        Kind::ElasticIp => resource.attributes.get("AllocationId").cloned(),
        Kind::SubnetRouteTableAssociation
        | Kind::VpcGatewayAttachment
        | Kind::Route
        | Kind::SecurityGroupIngress
        | Kind::SecurityGroupEgress => None,
    }
}

/// Put the gateway an `AWS::EC2::VPCGatewayAttachment` now declares on its VPC, in place of the
/// one it had: the detach of the gateway in force and the attach of the declared one, the two
/// calls that are the only way a VPC's gateway changes.
///
/// The attachment keeps its identity — the pair it is, whose VPC half is create-only — so
/// everything that references the attachment goes on resolving, and the VPC is never left with
/// two gateways of one kind: the detach comes first, which is also what makes the attach the
/// one Amazon EC2 admits.
async fn update_gateway_attachment(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let vpc_id = required(resource, "VpcId")?;
    let (attach, declared_member, declared_gateway_id) =
        declared_gateway(declared, &resource.resource_type)?;
    detach_gateway(ctx, region, resource).await?;
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        attach,
        &[
            (declared_member.to_string(), declared_gateway_id),
            ("VpcId".to_string(), vpc_id),
        ],
    )
    .await?;
    Ok(applied_in_place())
}

/// Point the route an `AWS::EC2::Route` declares at the target it now names, through
/// `ReplaceRoute` — "replaces an existing route within a route table in a VPC".
///
/// The route is addressed the only way Amazon EC2 lets one be addressed: the table it is in and
/// the destination it matches on, which are its identity and so cannot have changed. The route
/// therefore keeps the `<RouteTableId>|<destination>` physical id the type publishes, and the
/// table goes on holding exactly one route for that destination.
async fn replace_route(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let route_table_id = required(resource, "RouteTableId")?;
    let (member, destination) =
        route_destination(resource).ok_or_else(|| missing_destination(resource))?;
    let mut parameters = vec![
        ("RouteTableId".to_string(), route_table_id),
        (member.to_string(), destination),
    ];
    for target in ROUTE_TARGETS {
        if let Some(value) = declared_scalar(declared, target) {
            parameters.push(((*target).to_string(), value));
        }
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "ReplaceRoute",
        &parameters,
    )
    .await?;
    Ok(applied_in_place())
}

/// Write the DESCRIPTION an `AWS::EC2::SecurityGroupIngress` or `AWS::EC2::SecurityGroupEgress`
/// now declares onto the rule it is, through the call that writes one for its direction:
/// "updates the description of an ingress (inbound) security group rule. You can replace an
/// existing description, or add a description to a rule that did not have one previously. You
/// can remove a description for a security group rule by omitting the description parameter".
///
/// The rule is addressed by the `sgr-…` id Amazon EC2 minted for it — this resource's physical
/// id — on the group its declaration names, so it keeps its identity and everything else that
/// makes it the rule it is.
async fn update_rule_description(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let outbound = kind == Kind::SecurityGroupEgress;
    let action = if outbound {
        "UpdateSecurityGroupRuleDescriptionsEgress"
    } else {
        "UpdateSecurityGroupRuleDescriptionsIngress"
    };
    let mut parameters = vec![
        named_group(resource, outbound)?,
        (
            "SecurityGroupRuleDescription.1.SecurityGroupRuleId".to_string(),
            resource.physical_resource_id.clone(),
        ),
    ];
    // A declaration that no longer states one asks for the rule WITHOUT a description, which
    // that call spells as the description member omitted.
    if let Some(description) = declared_scalar(declared, "Description") {
        parameters.push((
            "SecurityGroupRuleDescription.1.Description".to_string(),
            description,
        ));
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        action,
        &parameters,
    )
    .await?;
    Ok(applied_in_place())
}

/// Apply a changed `AWS::EC2::VPCEndpoint` declaration to the endpoint the stack created,
/// through `ModifyVpcEndpoint` — the one call that changes an endpoint that exists.
///
/// That call takes each list of resources the endpoint is reached through as the ids to ADD and
/// the ids to REMOVE, so each declared list is compared against the one in force and only the
/// difference travels; the policy, the private-DNS switch and the address type are its own
/// scalar members. The endpoint keeps the `vpce-…` id Amazon EC2 assigned it.
async fn modify_vpc_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let mut parameters = vec![(
        "VpcEndpointId".to_string(),
        resource.physical_resource_id.clone(),
    )];
    for (property, member) in [
        ("IpAddressType", "IpAddressType"),
        ("PrivateDnsEnabled", "PrivateDnsEnabled"),
        ("PolicyDocument", "PolicyDocument"),
    ] {
        if let Some(value) = declared_scalar(declared, property) {
            parameters.push((member.to_string(), value));
        }
    }
    for (property, element) in VPC_ENDPOINT_LIST_MEMBERS {
        let held = list(resource, property);
        let wanted = list_of(declared, property);
        let added: Vec<&String> = wanted.iter().filter(|id| !held.contains(id)).collect();
        let removed: Vec<&String> = held.iter().filter(|id| !wanted.contains(id)).collect();
        for (change, ids) in [("Add", added), ("Remove", removed)] {
            for (index, id) in ids.into_iter().enumerate() {
                parameters.push((format!("{change}{element}.{}", index + 1), id.clone()));
            }
        }
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "ModifyVpcEndpoint",
        &parameters,
    )
    .await?;
    Ok(applied_in_place())
}

/// Delete the resource a stack created, addressed by the identifier EC2 minted for it — this
/// resource's physical id, which is what `Ref` returns for the type.
///
/// A resource EC2 no longer holds is already in the state the delete asks for
/// ([`NOT_FOUND_SUFFIX`]).
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let id = resource.physical_resource_id.clone();
    match kind {
        Kind::RouteTable => {
            forgiving(
                call_query(
                    ctx,
                    EC2_SIGNING_NAME,
                    region,
                    EC2_API_VERSION,
                    "DeleteRouteTable",
                    &[("RouteTableId".to_string(), id)],
                )
                .await,
            )
            .map(|_| ())
        }
        Kind::SubnetRouteTableAssociation => {
            // "Disassociates a subnet or gateway from a route table" — addressed by the
            // association's own id, which is the resource.
            forgiving(
                call_query(
                    ctx,
                    EC2_SIGNING_NAME,
                    region,
                    EC2_API_VERSION,
                    "DisassociateRouteTable",
                    &[("AssociationId".to_string(), id)],
                )
                .await,
            )
            .map(|_| ())
        }
        Kind::VpcEndpoint => {
            // `DeleteVpcEndpoints` takes a LIST and answers 200 with the endpoints it could
            // not delete, so what says this one went is that answer rather than the status.
            let answered = forgiving(
                call_query(
                    ctx,
                    EC2_SIGNING_NAME,
                    region,
                    EC2_API_VERSION,
                    "DeleteVpcEndpoints",
                    &[("VpcEndpointId.1".to_string(), id)],
                )
                .await,
            )?;
            let Some(document) = answered else {
                return Ok(());
            };
            // "Information about the VPC endpoints that were not successfully deleted"
            // (`Unsuccessful`): each entry carries that endpoint's own error code, and an
            // endpoint EC2 no longer holds is the state this delete asks for.
            for code in xml_elements(&document, "code") {
                if !code.ends_with(NOT_FOUND_SUFFIX) {
                    return Err(Refusal {
                        message: xml_element(&document, "message").unwrap_or_else(|| {
                            format!("Amazon EC2 did not delete the VPC endpoint: {code}")
                        }),
                        code,
                    });
                }
            }
            Ok(())
        }
        Kind::InternetGateway => forgiving(
            call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                "DeleteInternetGateway",
                &[("InternetGatewayId".to_string(), id)],
            )
            .await,
        )
        .map(|_| ()),
        Kind::VpnGateway => forgiving(
            call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                "DeleteVpnGateway",
                &[("VpnGatewayId".to_string(), id)],
            )
            .await,
        )
        .map(|_| ()),
        // The attachment is addressed by the pair that was attached, not by an id: EC2 mints
        // none for it, so the detach reads the gateway and the VPC off the declaration this
        // resource was created from — which carries the identifiers their own services
        // minted, the deployment having re-pointed every reference at them.
        Kind::VpcGatewayAttachment => detach_gateway(ctx, region, resource).await,
        Kind::ElasticIp => {
            // The release names the address by the allocation id EC2 answered the create
            // with — "[EC2-VPC] The allocation ID" (`ReleaseAddressRequest$AllocationId`),
            // kept as this resource's `AllocationId` attribute — and falls back to the
            // address itself, which is this resource's physical id, for a record that
            // carries no such attribute.
            let named = match resource
                .attributes
                .get("AllocationId")
                .filter(|allocation_id| !allocation_id.is_empty())
            {
                Some(allocation_id) => {
                    ("AllocationId".to_string(), allocation_id.clone())
                }
                None => ("PublicIp".to_string(), id),
            };
            forgiving(
                call_query(
                    ctx,
                    EC2_SIGNING_NAME,
                    region,
                    EC2_API_VERSION,
                    "ReleaseAddress",
                    &[named],
                )
                .await,
            )
            .map(|_| ())
        }
        // A route is addressed by the table it is in and the destination it matches, not by
        // an id: EC2 mints none for it, so the delete reads both off the declaration this
        // resource was created from.
        Kind::Route => delete_route(ctx, region, resource).await,
        Kind::NatGateway => delete_nat_gateway(ctx, region, &id).await,
        // "You must detach the network interface before you can delete it"
        // (`DeleteNetworkInterface`), which is the order a teardown already walks a stack's
        // resources in: an instance that holds the interface goes first.
        Kind::NetworkInterface => forgiving(
            call_query(
                ctx,
                EC2_SIGNING_NAME,
                region,
                EC2_API_VERSION,
                "DeleteNetworkInterface",
                &[("NetworkInterfaceId".to_string(), id)],
            )
            .await,
        )
        .map(|_| ()),
        Kind::SecurityGroupIngress | Kind::SecurityGroupEgress => {
            revoke_security_group_rule(ctx, region, kind, resource).await
        }
    }
}

/// Every property `AWS::EC2::SecurityGroupIngress` publishes, from AWS's CloudFormation
/// resource reference for the type read against `AuthorizeSecurityGroupIngress`'s member list
/// (`smithy_query(query="operation",operation="AuthorizeSecurityGroupIngress")`).
///
/// `GroupId` and `GroupName` name the GROUP the rule is added to, which that call takes beside
/// the permission ([`named_group`]); every other property is a member of the ONE permission the
/// rule is — the same members an inline rule of `AWS::EC2::SecurityGroup` declares
/// ([`EC2_SECURITY_GROUP_INGRESS_MEMBERS`]), with `Description` carried on the source the rule
/// references.
const SECURITY_GROUP_INGRESS_PROPERTIES: &[&str] = &[
    "CidrIp",
    "CidrIpv6",
    "Description",
    "FromPort",
    "GroupId",
    "GroupName",
    "IpProtocol",
    "SourcePrefixListId",
    "SourceSecurityGroupId",
    "SourceSecurityGroupName",
    "SourceSecurityGroupOwnerId",
    "ToPort",
];

/// Every property `AWS::EC2::SecurityGroupEgress` publishes, read the same way against
/// `AuthorizeSecurityGroupEgress`: an outbound rule names a DESTINATION rather than a source,
/// and that call takes the group by id alone (`$GroupId` is its one required member), which is
/// what the type requires too.
const SECURITY_GROUP_EGRESS_PROPERTIES: &[&str] = &[
    "CidrIp",
    "CidrIpv6",
    "Description",
    "DestinationPrefixListId",
    "DestinationSecurityGroupId",
    "FromPort",
    "GroupId",
    "IpProtocol",
    "ToPort",
];

/// The properties of either rule type that name the GROUP rather than the permission: they
/// travel as the authorize call's own group member, not inside its `IpPermissions` entry.
const RULE_GROUP_PROPERTIES: &[&str] = &["GroupId", "GroupName"];

/// Add the ONE security-group rule an `AWS::EC2::SecurityGroupIngress` or an
/// `AWS::EC2::SecurityGroupEgress` declares to the group it names, in Amazon EC2 — so the group
/// a stack deployed really lets through what the template allowed, and
/// `DescribeSecurityGroups` reports the rule among that group's `IpPermissions`.
///
/// A rule declared as a resource of its own is ONE permission of the authorize call for its
/// direction, exactly as an inline rule of the group is: "each entry of either rule property is
/// one permission of `AuthorizeSecurityGroupIngress` / `AuthorizeSecurityGroupEgress`", and the
/// members are the same. Unlike an inline OUTBOUND declaration, a standalone egress rule leaves
/// the all-traffic rule Amazon EC2 gives a new group where it is: that rule is removed when the
/// GROUP's own declaration states outbound rules, which is a statement about the group's
/// `SecurityGroupEgress` property rather than about a rule resource added later.
///
/// "Ref returns the ID of the security group rule", which Amazon EC2 MINTS for the rule it
/// added (`SecurityGroupRule$SecurityGroupRuleId`) — so that id is what this answers as the
/// resource's physical id and as its one published attribute, the stack publishing an `sgr-…`
/// identifier that service really holds a rule for and that the revoke on the way out addresses
/// the rule by.
async fn authorize_security_group_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let outbound = kind == Kind::SecurityGroupEgress;
    let (published, members, action) = if outbound {
        (
            SECURITY_GROUP_EGRESS_PROPERTIES,
            EC2_SECURITY_GROUP_EGRESS_MEMBERS,
            "AuthorizeSecurityGroupEgress",
        )
    } else {
        (
            SECURITY_GROUP_INGRESS_PROPERTIES,
            EC2_SECURITY_GROUP_INGRESS_MEMBERS,
            "AuthorizeSecurityGroupIngress",
        )
    };
    refuse_unmapped_properties(resource, published)?;
    let mut parameters = vec![named_group(resource, outbound)?];
    parameters.extend(security_group_rule_parameters(
        &declared_permission(resource),
        members,
    )?);
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        action,
        &parameters,
    )
    .await?;
    // "Information about the inbound (ingress) security group rules that were added"
    // (`$SecurityGroupRules`): one rule per source the permission grants, and this permission
    // grants one — so the set carries the rule this resource IS.
    let rule_id = set_entry(&answered, "securityGroupRuleSet", "securityGroupRuleId")
        .ok_or_else(|| Refusal {
            code: "InvalidRequest".to_string(),
            message: format!("{action} answered no securityGroupRuleId"),
        })?;
    // The one attribute the type publishes: "Id — the ID of the security group rule", which IS
    // the id EC2 minted.
    let mut attributes = BTreeMap::new();
    attributes.insert("Id".to_string(), rule_id.clone());
    Ok(Created {
        physical_resource_id: Some(rule_id),
        attributes,
    })
}

/// Take the rule one of those resources added back off its group, addressed by the
/// `sgr-…` id Amazon EC2 minted for it — this resource's physical id — on the group its
/// declaration named.
///
/// A rule that group no longer holds, and a group EC2 no longer holds at all, are each already
/// in the state this delete asks for ([`NOT_FOUND_SUFFIX`] covers both
/// `InvalidPermission.NotFound` and `InvalidGroup.NotFound`).
async fn revoke_security_group_rule(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let outbound = kind == Kind::SecurityGroupEgress;
    let action = if outbound {
        "RevokeSecurityGroupEgress"
    } else {
        "RevokeSecurityGroupIngress"
    };
    let parameters = vec![
        named_group(resource, outbound)?,
        (
            "SecurityGroupRuleId.1".to_string(),
            resource.physical_resource_id.clone(),
        ),
    ];
    forgiving(
        call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            action,
            &parameters,
        )
        .await,
    )
    .map(|_| ())
}

/// The group a rule resource is on, as the parameter the authorize and the revoke for its
/// direction name that group by.
///
/// An OUTBOUND rule names it by id alone — `$GroupId` is the one member either outbound call
/// takes for it, and the type requires the property — while an inbound one may name it by
/// either, "[Default VPC] the name of the security group" being the other spelling
/// (`AuthorizeSecurityGroupIngress$GroupName`). A declaration naming the group nowhere is
/// REFUSED rather than sent without it: the rule would be added to no group at all.
fn named_group(
    resource: &StoredResource,
    outbound: bool,
) -> Result<(String, String), Refusal> {
    if let Some(group_id) = scalar(resource, "GroupId") {
        return Ok(("GroupId".to_string(), group_id));
    }
    if !outbound {
        if let Some(group_name) = scalar(resource, "GroupName") {
            return Ok(("GroupName".to_string(), group_name));
        }
    }
    // Amazon EC2's own code for a request that left out a member it cannot be served without
    // (`MissingParameter`), naming the property that was needed — both acceptable spellings
    // where the declaration had the choice.
    let needed = if outbound {
        "GroupId".to_string()
    } else {
        "GroupId or GroupName".to_string()
    };
    Err(Refusal {
        code: "MissingParameter".to_string(),
        message: format!(
            "{} requires the {needed} property",
            resource.resource_type
        ),
    })
}

/// The PERMISSION a rule resource declares: its properties but the ones that name the group,
/// read as the one inline rule the shared reader turns into `IpPermissions.1.…` parameters.
fn declared_permission(resource: &StoredResource) -> Value {
    let Value::Object(properties) = &resource.properties else {
        return Value::Object(serde_json::Map::new());
    };
    Value::Object(
        properties
            .iter()
            .filter(|(property, _)| !RULE_GROUP_PROPERTIES.contains(&property.as_str()))
            .map(|(property, value)| (property.clone(), value.clone()))
            .collect(),
    )
}

/// Create the route table an `AWS::EC2::RouteTable` declares, in Amazon EC2.
///
/// "Ref returns the route table ID" — an id EC2 ASSIGNS (`rtb-` and 17 hexadecimal
/// characters), which is what this answers as the resource's physical id, so the stack's
/// `Ref`, its outputs and its per-resource reads report an id EC2 really holds a table for.
/// The table comes up with the local route of its VPC, which is EC2's to add.
async fn create_route_table(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, ROUTE_TABLE_PROPERTIES)?;
    let vpc_id = required(resource, "VpcId")?;
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateRouteTable",
        &[("VpcId".to_string(), vpc_id)],
    )
    .await?;
    // The answer's `<routeTable>` structure, read for the id EC2 assigned: its own direct
    // children only, so the `<routeTableId>` of an association entry nested in it cannot be
    // read as the table's.
    let route_table_id = reported(&answered, "routeTable", "routeTableId")?;
    tag_resource(
        ctx,
        region,
        &route_table_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The one attribute the type publishes beside its `Ref`: "Fn::GetAtt RouteTableId — the
    // ID of the route table", which IS the id EC2 assigned.
    let mut attributes = BTreeMap::new();
    attributes.insert("RouteTableId".to_string(), route_table_id.clone());
    Ok(Created {
        physical_resource_id: Some(route_table_id),
        attributes,
    })
}

/// Associate the subnet an `AWS::EC2::SubnetRouteTableAssociation` declares with the route
/// table it declares, in Amazon EC2 — so that subnet really uses that table's routes rather
/// than its VPC's main table.
///
/// Both ids are the ones EC2 minted for the resources of the same template: a deployment
/// creates each resource after the ones it depends on and re-points a reference at the
/// identifier the owning service assigned ([`super::reconcile`]).
async fn create_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, ASSOCIATION_PROPERTIES)?;
    let parameters = vec![
        ("RouteTableId".to_string(), required(resource, "RouteTableId")?),
        ("SubnetId".to_string(), required(resource, "SubnetId")?),
    ];
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "AssociateRouteTable",
        &parameters,
    )
    .await?;
    // "The route table association ID" (`AssociateRouteTableResult$AssociationId`), which is
    // what this resource IS and what its removal addresses the association by.
    let association_id = xml_element(&answered, "associationId")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Refusal {
            code: "InvalidRequest".to_string(),
            message: "AssociateRouteTable answered no associationId".to_string(),
        })?;
    let mut attributes = BTreeMap::new();
    attributes.insert("Id".to_string(), association_id.clone());
    Ok(Created {
        physical_resource_id: Some(association_id),
        attributes,
    })
}

/// Create the VPC endpoint an `AWS::EC2::VPCEndpoint` declares, in Amazon EC2.
///
/// "Ref returns the ID of the VPC endpoint" — the `vpce-…` id EC2 assigns. A GATEWAY endpoint
/// declared with route tables is created WITH them, which is what puts the endpoint's own
/// route in each of those tables, so the subnets on them reach the service through the
/// endpoint.
async fn create_vpc_endpoint(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, VPC_ENDPOINT_PROPERTIES)?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in VPC_ENDPOINT_SCALAR_MEMBERS {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    // "A policy to attach to the endpoint that controls access to the service. The policy
    // must be in valid JSON format" (`PolicyDocument`), which a template states as the policy
    // OBJECT itself — so an object travels as its JSON text and a string as it was written.
    if let Some(document) = resource.properties.get("PolicyDocument") {
        if !document.is_null() {
            parameters.push(("PolicyDocument".to_string(), json_text(document)));
        }
    }
    for (property, member) in VPC_ENDPOINT_LIST_MEMBERS {
        for (index, entry) in list(resource, property).iter().enumerate() {
            parameters.push((format!("{member}.{}", index + 1), entry.clone()));
        }
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateVpcEndpoint",
        &parameters,
    )
    .await?;
    let endpoint_id = reported(&answered, "vpcEndpoint", "vpcEndpointId")?;
    tag_resource(
        ctx,
        region,
        &endpoint_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The type's published `Fn::GetAtt Id`, which IS the id EC2 assigned. The ones that are
    // LISTS (`DnsEntries`, `NetworkInterfaceIds`) are no single value an attribute carries,
    // so none is invented here.
    let mut attributes = BTreeMap::new();
    attributes.insert("Id".to_string(), endpoint_id.clone());
    Ok(Created {
        physical_resource_id: Some(endpoint_id),
        attributes,
    })
}

/// Create the internet gateway an `AWS::EC2::InternetGateway` declares, in Amazon EC2.
///
/// "Ref returns the ID of the internet gateway" — the `igw-…` id EC2 assigns, which is what
/// this answers as the resource's physical id, so a client that deployed the stack and then
/// calls `ec2 describe-internet-gateways` with the id the stack published reads it back.
/// `CreateInternetGateway` takes no member but the tag set
/// (`smithy_query(query="operation",operation="CreateInternetGateway")`), and the gateway
/// comes up detached: what puts it on a VPC is the attachment resource a template declares
/// beside it ([`attach_gateway`]).
async fn create_internet_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, INTERNET_GATEWAY_PROPERTIES)?;
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateInternetGateway",
        &[],
    )
    .await?;
    let internet_gateway_id = reported(&answered, "internetGateway", "internetGatewayId")?;
    tag_resource(
        ctx,
        region,
        &internet_gateway_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The one attribute the type publishes beside its `Ref`: "Fn::GetAtt InternetGatewayId —
    // the ID of the internet gateway", which IS the id EC2 assigned.
    let mut attributes = BTreeMap::new();
    attributes.insert(
        "InternetGatewayId".to_string(),
        internet_gateway_id.clone(),
    );
    Ok(Created {
        physical_resource_id: Some(internet_gateway_id),
        attributes,
    })
}

/// Create the virtual private gateway an `AWS::EC2::VPNGateway` declares, in Amazon EC2.
///
/// "Ref returns the VPN gateway ID" — the `vgw-…` id EC2 assigns. `Type` is required by both
/// sides (the type's own property and `CreateVpnGateway$Type`), so a declaration that states
/// none is refused rather than sent without it.
async fn create_vpn_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, VPN_GATEWAY_PROPERTIES)?;
    required(resource, "Type")?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in VPN_GATEWAY_MEMBERS {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateVpnGateway",
        &parameters,
    )
    .await?;
    let vpn_gateway_id = reported(&answered, "vpnGateway", "vpnGatewayId")?;
    tag_resource(
        ctx,
        region,
        &vpn_gateway_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // "Fn::GetAtt VPNGatewayId — the ID of the VPN gateway", which IS the id EC2 assigned.
    let mut attributes = BTreeMap::new();
    attributes.insert("VPNGatewayId".to_string(), vpn_gateway_id.clone());
    Ok(Created {
        physical_resource_id: Some(vpn_gateway_id),
        attributes,
    })
}

/// Attach the gateway an `AWS::EC2::VPCGatewayAttachment` declares to the VPC it declares, in
/// Amazon EC2 — so the VPC really has a way out and a route may point at that gateway.
///
/// One attachment attaches ONE gateway: the type publishes `InternetGatewayId` and
/// `VpnGatewayId` as alternatives, and the call each one is made with is its own
/// (`AttachInternetGateway`, `AttachVpnGateway`), so a declaration naming both or neither is
/// refused rather than guessed at.
///
/// EC2 assigns no identifier to the attachment — both detaches address it by the gateway and
/// the VPC — and "Ref returns the name of the VPC gateway attachment", so this answers no
/// physical id and the resource keeps the one CloudFormation computed.
async fn attach_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, GATEWAY_ATTACHMENT_PROPERTIES)?;
    let vpc_id = required(resource, "VpcId")?;
    let (action, gateway_member, gateway_id) = attached_gateway(resource)?;
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        action,
        &[
            (gateway_member.to_string(), gateway_id),
            ("VpcId".to_string(), vpc_id),
        ],
    )
    .await?;
    Ok(Created::default())
}

/// Take the attachment an `AWS::EC2::VPCGatewayAttachment` made back off its VPC.
///
/// The counterpart of [`attach_gateway`], addressed the same way and with the same pair; a
/// gateway and VPC that are not attached are already in the state this asks for
/// ([`GATEWAY_NOT_ATTACHED`]), as is a gateway or VPC EC2 no longer holds.
async fn detach_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let vpc_id = required(resource, "VpcId")?;
    let (attach, gateway_member, gateway_id) = attached_gateway(resource)?;
    let action = match attach {
        "AttachInternetGateway" => "DetachInternetGateway",
        _ => "DetachVpnGateway",
    };
    forgiving(
        call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            action,
            &[
                (gateway_member.to_string(), gateway_id),
                ("VpcId".to_string(), vpc_id),
            ],
        )
        .await,
    )
    .map(|_| ())
}

/// The gateway one `AWS::EC2::VPCGatewayAttachment` is for: the ATTACH action it is attached
/// with, the member that call names the gateway under, and the identifier the declaration
/// carries for it.
///
/// Read off the declaration rather than kept on the resource, so the attach and the detach
/// cannot disagree about which gateway the attachment is of.
fn attached_gateway(resource: &StoredResource) -> Result<(&'static str, &'static str, String), Refusal> {
    declared_gateway(&resource.properties, &resource.resource_type)
}

/// The same reading against a property DOCUMENT: the gateway one revision of an attachment's
/// declaration names, which is what a SWAP of the gateway detaches and attaches
/// ([`update_gateway_attachment`]).
fn declared_gateway(
    properties: &Value,
    resource_type: &str,
) -> Result<(&'static str, &'static str, String), Refusal> {
    let internet_gateway = declared_scalar(properties, "InternetGatewayId");
    let vpn_gateway = declared_scalar(properties, "VpnGatewayId");
    match (internet_gateway, vpn_gateway) {
        (Some(internet_gateway_id), None) => Ok((
            "AttachInternetGateway",
            "InternetGatewayId",
            internet_gateway_id,
        )),
        (None, Some(vpn_gateway_id)) => {
            Ok(("AttachVpnGateway", "VpnGatewayId", vpn_gateway_id))
        }
        // "You must specify either InternetGatewayId or VpnGatewayId, but not both"
        // (`AWS::EC2::VPCGatewayAttachment`): either way the declaration does not say which
        // gateway the attachment is of, so nothing is attached under a guess.
        _ => Err(Refusal {
            code: "InvalidRequest".to_string(),
            message: format!(
                "{resource_type} requires exactly one of the InternetGatewayId and \
                 VpnGatewayId properties"
            ),
        }),
    }
}

/// Allocate the Elastic IP address an `AWS::EC2::EIP` declares, in Amazon EC2.
///
/// "Ref returns the Elastic IP address" — the address EC2 allocated, which is what this
/// answers as the resource's physical id, and the allocation id it answers beside that
/// address is the type's `AllocationId` attribute: the value a NAT gateway or an association
/// of the same template is created with, and the one the release names on the way out.
async fn allocate_address(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, ELASTIC_IP_PROPERTIES)?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in ELASTIC_IP_MEMBERS {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "AllocateAddress",
        &parameters,
    )
    .await?;
    // `AllocateAddress` answers its members at the top of the document rather than inside a
    // structure of its own (`smithy_query(query="operation",operation="AllocateAddress")`:
    // `publicIp`, `allocationId`), so both are read as the answer's own elements.
    let public_ip = answered_element(&answered, "publicIp")?;
    let allocation_id = answered_element(&answered, "allocationId")?;
    tag_resource(
        ctx,
        region,
        &allocation_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The two attributes the type publishes: "AllocationId — the ID that represents the
    // allocation of the Elastic IP address" and "PublicIp — the Elastic IP address".
    let mut attributes = BTreeMap::new();
    attributes.insert("AllocationId".to_string(), allocation_id);
    attributes.insert("PublicIp".to_string(), public_ip.clone());
    Ok(Created {
        physical_resource_id: Some(public_ip),
        attributes,
    })
}

/// Add the route an `AWS::EC2::Route` declares to the route table it declares, in Amazon EC2 —
/// so the subnets on that table really reach the target the template declared rather than only
/// the VPC's own block.
///
/// Every property of the type is `CreateRoute`'s own member under the same name, so the
/// declaration IS the request: the table, the one destination the route matches on and the one
/// target it points at, each carrying the identifier the owning service minted for the resource
/// of the same template it names ([`super::reconcile`] re-points a reference at it).
///
/// EC2 assigns no identifier to a route — `CreateRoute` answers only whether it stored one, and
/// both the delete and the `DescribeRouteTables` entry address a route by its table and its
/// destination — so this answers no physical id and the resource keeps the one CloudFormation
/// computed.
async fn create_route(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, ROUTE_PROPERTIES)?;
    required(resource, "RouteTableId")?;
    // A route matches on ONE destination, and a request carrying none identifies no route: the
    // declaration is refused here naming the three it may state one in, rather than sent as a
    // request that cannot say where the route matches.
    if route_destination(resource).is_none() {
        return Err(missing_destination(resource));
    }
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in ROUTE_PROPERTIES {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateRoute",
        &parameters,
    )
    .await?;
    Ok(Created::default())
}

/// Take the route an `AWS::EC2::Route` added back out of its table.
///
/// The counterpart of [`create_route`], addressing the route the only way EC2 lets one be
/// addressed: the table it is in and the destination it matches, both read off the declaration.
/// A route EC2 no longer holds — or a table it no longer holds — is already in the state the
/// delete asks for ([`NOT_FOUND_SUFFIX`], which `InvalidRoute.NotFound` and
/// `InvalidRouteTableID.NotFound` both carry).
async fn delete_route(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let route_table_id = required(resource, "RouteTableId")?;
    let (member, destination) =
        route_destination(resource).ok_or_else(|| missing_destination(resource))?;
    forgiving(
        call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            "DeleteRoute",
            &[
                ("RouteTableId".to_string(), route_table_id),
                (member.to_string(), destination),
            ],
        )
        .await,
    )
    .map(|_| ())
}

/// The ONE destination an `AWS::EC2::Route` declaration matches on, as the member that carries
/// it and its value — what identifies the route inside its table.
///
/// Read off the declaration rather than kept on the resource, so the create and the delete
/// cannot disagree about which route the resource is.
fn route_destination(resource: &StoredResource) -> Option<(&'static str, String)> {
    ROUTE_DESTINATIONS
        .iter()
        .find_map(|member| scalar(resource, member).map(|value| (*member, value)))
}

/// The refusal for a route declaration that states no destination at all, naming the three
/// properties one of which identifies the route.
fn missing_destination(resource: &StoredResource) -> Refusal {
    Refusal {
        code: "InvalidRequest".to_string(),
        message: format!(
            "{} requires one of the {} properties",
            resource.resource_type,
            ROUTE_DESTINATIONS.join(", ")
        ),
    }
}

/// Create the NAT gateway an `AWS::EC2::NatGateway` declares, in Amazon EC2 — so the private
/// subnets whose route tables point at it really reach the internet through a gateway that
/// service holds.
///
/// "Ref returns the ID of the NAT gateway" — the `nat-…` id EC2 assigns, which is what this
/// answers as the resource's physical id, so a route of the same template is created with the
/// id EC2 knows the gateway by and `DescribeNatGateways` reports the id the stack published.
async fn create_nat_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, NAT_GATEWAY_PROPERTIES)?;
    required(resource, "SubnetId")?;
    let mut parameters: Vec<(String, String)> = Vec::new();
    for member in NAT_GATEWAY_SCALAR_MEMBERS {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    for (property, member) in NAT_GATEWAY_LIST_MEMBERS {
        for (index, entry) in list(resource, property).iter().enumerate() {
            parameters.push((format!("{member}.{}", index + 1), entry.clone()));
        }
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateNatGateway",
        &parameters,
    )
    .await?;
    let nat_gateway_id = reported(&answered, "natGateway", "natGatewayId")?;
    tag_resource(
        ctx,
        region,
        &nat_gateway_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The one attribute the type publishes beside its `Ref`: "Fn::GetAtt NatGatewayId — the ID
    // of the NAT gateway", which IS the id EC2 assigned.
    let mut attributes = BTreeMap::new();
    attributes.insert("NatGatewayId".to_string(), nat_gateway_id.clone());
    Ok(Created {
        physical_resource_id: Some(nat_gateway_id),
        attributes,
    })
}

/// Every property `AWS::EC2::NetworkInterface` publishes that this module applies, from AWS's
/// CloudFormation resource reference for the type read against `CreateNetworkInterface`'s
/// member list (`smithy_query(query="operation",operation="CreateNetworkInterface")`).
///
/// `GroupSet` is "the security group IDs associated with this network interface", which is that
/// call's own `Groups` member (`xmlName: SecurityGroupId`); every other entry is a member of it
/// under the same name. The properties this list LEAVES OUT are each refused rather than
/// dropped ([`refuse_unmapped_properties`]): the IPv4 and IPv6 prefix members and the
/// `PrivateIpAddresses` and `Ipv6Addresses` lists are lists of STRUCTURES, which this module
/// writes no parameters for, a `ConnectionTrackingSpecification` is a structure of its own, and
/// `SourceDestCheck` is a `ModifyNetworkInterfaceAttribute` the create cannot carry.
const NETWORK_INTERFACE_PROPERTIES: &[&str] = &[
    "Description",
    "EnablePrimaryIpv6",
    "GroupSet",
    "InterfaceType",
    "Ipv6AddressCount",
    "PrivateIpAddress",
    "SecondaryPrivateIpAddressCount",
    "SubnetId",
    "Tags",
];

/// The `AWS::EC2::NetworkInterface` properties that are `CreateNetworkInterface`'s own SCALAR
/// members under the same name.
const NETWORK_INTERFACE_SCALAR_MEMBERS: &[&str] = &[
    "Description",
    "EnablePrimaryIpv6",
    "InterfaceType",
    "Ipv6AddressCount",
    "PrivateIpAddress",
    "SecondaryPrivateIpAddressCount",
    "SubnetId",
];

/// Create the elastic network interface an `AWS::EC2::NetworkInterface` declares, in Amazon EC2
/// — so a route or an instance of the same template that points at it really reaches an
/// interface that service holds, rather than an `eni-…` id `DescribeNetworkInterfaces` reports
/// nothing for.
///
/// "Ref returns the ID of the network interface" — the `eni-…` id EC2 assigns, which is what
/// this answers as the resource's physical id.
async fn create_network_interface(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, NETWORK_INTERFACE_PROPERTIES)?;
    let mut parameters = vec![("SubnetId".to_string(), required(resource, "SubnetId")?)];
    for member in NETWORK_INTERFACE_SCALAR_MEMBERS
        .iter()
        .filter(|member| **member != "SubnetId")
    {
        if let Some(value) = scalar(resource, member) {
            parameters.push(((*member).to_string(), value));
        }
    }
    for (index, group_id) in list(resource, "GroupSet").iter().enumerate() {
        parameters.push((format!("SecurityGroupId.{}", index + 1), group_id.clone()));
    }
    let answered = call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateNetworkInterface",
        &parameters,
    )
    .await?;
    let network_interface_id = reported(&answered, "networkInterface", "networkInterfaceId")?;
    tag_resource(
        ctx,
        region,
        &network_interface_id,
        &tags_of(&resource.properties, propagated),
    )
    .await?;
    // The attribute the uniform convention reads off the physical id: "Id — the ID of the
    // network interface", which IS the id EC2 assigned.
    let mut attributes = BTreeMap::new();
    attributes.insert("Id".to_string(), network_interface_id.clone());
    Ok(Created {
        physical_resource_id: Some(network_interface_id),
        attributes,
    })
}

/// Apply a changed `AWS::EC2::NetworkInterface` declaration to the interface the stack created,
/// through `ModifyNetworkInterfaceAttribute` — "modifies the specified network interface
/// attribute. You can specify only one attribute at a time", so each changed attribute is its
/// own call.
///
/// The interface keeps the `eni-…` id Amazon EC2 assigned it, so an instance it is attached to
/// and a route pointing at it go on resolving.
async fn update_network_interface(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    changed: &[String],
) -> Result<Option<Created>, Refusal> {
    let network_interface_id = resource.physical_resource_id.clone();
    // "A description for the network interface" (`$Description`), which that call takes as an
    // `AttributeValue` — its value travelling as the member's own `Value`. A declaration that
    // no longer states one asks for the interface without a description, which is the empty
    // value of that attribute.
    if changed.iter().any(|property| property == "Description") {
        let description = declared_scalar(declared, "Description").unwrap_or_default();
        modify_network_interface(
            ctx,
            region,
            &network_interface_id,
            &[("Description.Value".to_string(), description)],
        )
        .await?;
    }
    // "Changes the security groups for the network interface" (`$Groups`), which that call
    // takes as the flattened list its own member name spells. A declaration that states none
    // states a set the call cannot carry — an interface is always in at least one group — so
    // the interface keeps the groups it is in and the change is reported as one this arm did
    // not apply.
    if changed.iter().any(|property| property == "GroupSet") {
        let groups: Vec<(String, String)> = list_of(declared, "GroupSet")
            .iter()
            .enumerate()
            .map(|(index, group_id)| {
                (format!("SecurityGroupId.{}", index + 1), group_id.clone())
            })
            .collect();
        if groups.is_empty() {
            return Ok(UNAPPLIED);
        }
        modify_network_interface(ctx, region, &network_interface_id, &groups).await?;
    }
    Ok(applied_in_place())
}

/// Write ONE attribute of an interface that exists: `ModifyNetworkInterfaceAttribute` carrying
/// the interface and that attribute's own parameters.
async fn modify_network_interface(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    network_interface_id: &str,
    attribute: &[(String, String)],
) -> Result<(), Refusal> {
    let mut parameters = vec![(
        "NetworkInterfaceId".to_string(),
        network_interface_id.to_string(),
    )];
    parameters.extend(attribute.iter().cloned());
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "ModifyNetworkInterfaceAttribute",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// Delete the NAT gateway a stack created, addressed by the id EC2 minted for it.
///
/// A gateway EC2 no longer holds is already in the state the delete asks for
/// ([`NAT_GATEWAY_NOT_FOUND`], which carries no [`NOT_FOUND_SUFFIX`]), and a gateway a delete
/// has already been asked for is answered by that service as the deletion already under way.
async fn delete_nat_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    nat_gateway_id: &str,
) -> Result<(), Refusal> {
    match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DeleteNatGateway",
        &[("NatGatewayId".to_string(), nat_gateway_id.to_string())],
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NAT_GATEWAY_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether the resource one of these types created is ready for a client to use.
///
/// Every one of them EXISTS from the moment its create answers — the table takes its routes,
/// the association holds, the gateway is there to be attached, the attachment is in force, the
/// address is allocated, the route is in its table, and the `pending` window Amazon EC2 reports
/// a new VPC ENDPOINT in is that service's own transition rather than a resource that has yet
/// to come into being.
///
/// A NAT GATEWAY is the one exception, for the reason the Amazon EBS volume arm of
/// [`super::super::provisioner`] states: `CreateNatGateway` answers the gateway in
/// [`NAT_GATEWAY_PENDING`], nothing routes through it in that state, and that service reports
/// when it reaches [`NAT_GATEWAY_AVAILABLE`] — so the stack holds the resource's creation open
/// until it does, and a client that reads the stack as complete finds a gateway it can route
/// through. A state no wait brings a NEW gateway out of (either state of a deletion, or a
/// gateway that service holds none of) fails the resource with what that service reports,
/// rather than waiting the window out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    if kind != Kind::NatGateway {
        return Ok(Readiness::Ready);
    }
    let Some(state) = nat_gateway_state(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "Amazon EC2 holds no NAT gateway for {physical_resource_id}"
        )));
    };
    match state.as_str() {
        NAT_GATEWAY_AVAILABLE => Ok(Readiness::Ready),
        NAT_GATEWAY_PENDING => Ok(Readiness::Pending(state)),
        other => Ok(Readiness::Failed(format!(
            "Amazon EC2 reports NAT gateway {physical_resource_id} in state {other}"
        ))),
    }
}

/// Whether the service that owns a deleted resource has finished removing it.
///
/// Each of these deletes takes the resource away as it answers — Amazon EC2 reports no state a
/// deleted route table, association, endpoint, gateway, attachment, address or route is still
/// being removed in — except the NAT gateway, which `DeleteNatGateway` moves into `deleting`
/// and that service reports [`NAT_GATEWAY_DELETED`] when it has finished with. So the stack
/// holds that resource's deletion open until it is gone there, and a client that reads the
/// deletion as complete finds nothing still being torn down.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    if kind != Kind::NatGateway {
        return Ok(Removal::Gone);
    }
    match nat_gateway_state(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(state) if state == NAT_GATEWAY_DELETED => Ok(Removal::Gone),
        Some(state) => Ok(Removal::Pending(state)),
    }
}

/// The state Amazon EC2 reports the NAT gateway `nat_gateway_id` in, or `None` for one it does
/// not hold ([`NAT_GATEWAY_NOT_FOUND`]).
///
/// The one read behind [`readiness`] and [`removal`], so the two ask that service the same
/// question and read its answer the same way.
async fn nat_gateway_state(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    nat_gateway_id: &str,
) -> Result<Option<String>, Refusal> {
    let answered = match call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "DescribeNatGateways",
        &[("NatGatewayId.1".to_string(), nat_gateway_id.to_string())],
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal)
            if refusal.code == NAT_GATEWAY_NOT_FOUND
                || refusal.code.ends_with(NOT_FOUND_SUFFIX) =>
        {
            return Ok(None)
        }
        Err(refusal) => return Err(refusal),
    };
    // The gateway's own `<state>`, read out of the answer's result set rather than as the first
    // element of that name anywhere in the document.
    Ok(set_entry(&answered, "natGatewaySet", "state"))
}

/// One member of the FIRST entry of an `ec2Query` answer's result set, as the non-empty text it
/// holds.
///
/// The set counterpart of [`reported`]: the read is scoped to the `<…Set>` the answer nests its
/// entries in and then to that set's first `<item>`'s own direct children, so a member of the
/// same name nested deeper cannot be mistaken for it. Every read through this names ONE id, so
/// the set holds one entry.
///
/// Visible to the rest of this service's state because what a describe REPORTED for one
/// identifier is also the question an AWS-specifically typed template parameter is resolved by
/// ([`crate::state::parameter_constraints::aws_specific_types`]), which reads an answer of the
/// same shape and must read it the same way.
pub(in crate::state) fn set_entry(document: &str, set: &str, member: &str) -> Option<String> {
    let entries = document
        .find(&format!("<{set}>"))
        .map(|start| &document[start..])?;
    xml_scalar_children(entries, "item")
        .get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// One element of an `ec2Query` answer's own members, as the non-empty text it holds.
///
/// The counterpart of [`reported`] for a call that answers its members at the top of the
/// document rather than inside a structure; an answer that reports none is a refusal rather
/// than an empty identifier.
fn answered_element(document: &str, member: &str) -> Result<String, Refusal> {
    xml_element(document, member)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Refusal {
            code: "InvalidRequest".to_string(),
            message: format!("Amazon EC2 answered no {member}"),
        })
}

/// Tag one EC2 resource with the set CloudFormation gives it.
///
/// Through `CreateTags` rather than a create's own `TagSpecifications`, for the reason the VPC
/// and subnet arms of [`super::super::provisioner`] use that call: the set carries the three
/// `aws:cloudformation:*` keys, and `CreateTags` is the request EC2 admits them from. Nothing
/// is called for a resource with no tags at all, and a type that is not taggable never
/// reaches here.
pub(super) async fn tag_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource_id: &str,
    tags: &[(String, String)],
) -> Result<(), Refusal> {
    if tags.is_empty() {
        return Ok(());
    }
    let mut parameters = vec![("ResourceId.1".to_string(), resource_id.to_string())];
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        parameters.push((format!("Tag.{position}.Key"), key.clone()));
        parameters.push((format!("Tag.{position}.Value"), value.clone()));
    }
    call_query(
        ctx,
        EC2_SIGNING_NAME,
        region,
        EC2_API_VERSION,
        "CreateTags",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// A call whose NOT-FOUND refusal is the state it asked for: `Ok(None)` for that case, and the
/// answer document for a call that went through.
///
/// A detach of a gateway and VPC EC2 does not hold attached to each other is the same case
/// ([`GATEWAY_NOT_ATTACHED`]): the state the call asks for is the one that service is in.
fn forgiving(answered: Result<String, Refusal>) -> Result<Option<String>, Refusal> {
    match answered {
        Ok(document) => Ok(Some(document)),
        Err(refusal)
            if refusal.code.ends_with(NOT_FOUND_SUFFIX)
                || refusal.code == GATEWAY_NOT_ATTACHED =>
        {
            Ok(None)
        }
        Err(refusal) => Err(refusal),
    }
}

/// One member of the `<parent>` structure an `ec2Query` answer carries, as the non-empty text
/// it holds — the id EC2 assigned.
///
/// Only that structure's own direct children are read ([`xml_scalar_children`]), so a member
/// of the same name nested inside it cannot be mistaken for it.
fn reported(document: &str, parent: &str, member: &str) -> Result<String, Refusal> {
    xml_scalar_children(document, parent)
        .get(member)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Refusal {
            code: "InvalidRequest".to_string(),
            message: format!("Amazon EC2 answered no {member}"),
        })
}

/// The value of the declared property `property`, as the text an `ec2Query` parameter carries:
/// a string as written, and a boolean or a number in its own JSON spelling (`true`, `3`),
/// which is what that protocol's parameters are.
pub(super) fn scalar(resource: &StoredResource, property: &str) -> Option<String> {
    declared_scalar(&resource.properties, property)
}

/// The same reading against a property DOCUMENT rather than a resource record: what an UPDATE
/// is handed is the document this deployment declares, which is not the one the record carries
/// ([`update`] compares the two).
pub(super) fn declared_scalar(properties: &Value, property: &str) -> Option<String> {
    let value = properties.get(property)?;
    if value.is_null() {
        return None;
    }
    match value.as_str() {
        Some("") => None,
        Some(text) => Some(text.to_string()),
        None => Some(json_text(value)),
    }
}

/// The entries of the declared LIST property `property`, each as that parameter's text. A
/// property that is not a list carries one entry, which is the one value it is.
pub(super) fn list(resource: &StoredResource, property: &str) -> Vec<String> {
    list_of(&resource.properties, property)
}

/// The same reading against a property DOCUMENT, for the reason [`declared_scalar`] records.
pub(super) fn list_of(properties: &Value, property: &str) -> Vec<String> {
    match properties.get(property) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(entries)) => entries
            .iter()
            .filter(|entry| !entry.is_null())
            .map(|entry| entry.as_str().map_or_else(|| json_text(entry), str::to_string))
            .collect(),
        Some(value) => vec![value.as_str().map_or_else(|| json_text(value), str::to_string)],
    }
}

/// One declared value as the text a parameter carries, for a value that is not a string: its
/// JSON spelling, which is what a policy document and a boolean are on the wire.
fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// The declared property `property`, which the create cannot be made without.
///
/// A declaration that states none is REFUSED rather than sent without it, so the resource is
/// never reported created somewhere the template did not ask for.
pub(super) fn required(resource: &StoredResource, property: &str) -> Result<String, Refusal> {
    scalar(resource, property).ok_or_else(|| Refusal {
        code: "InvalidRequest".to_string(),
        message: format!(
            "{} requires the {property} property",
            resource.resource_type
        ),
    })
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was.
pub(super) fn refuse_unmapped_properties(
    resource: &StoredResource,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = &resource.properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: "NotImplemented".to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource record of one of these types, carrying `properties` — what the arms above
    /// read, built once for every case below.
    fn declared(resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Declared".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: "rtb-0123456789abcdef0".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Every member an arm sends is a published property of the type it sends it for.
    #[test]
    fn every_sent_member_is_a_published_property() {
        for member in VPC_ENDPOINT_SCALAR_MEMBERS {
            assert!(
                VPC_ENDPOINT_PROPERTIES.contains(member),
                "case {member:?}: expected the sent member to be a published property of \
                 {EC2_VPC_ENDPOINT}, got none"
            );
        }
        for member in VPN_GATEWAY_MEMBERS {
            assert!(
                VPN_GATEWAY_PROPERTIES.contains(member),
                "case {member:?}: expected the sent member to be a published property of \
                 {EC2_VPN_GATEWAY}, got none"
            );
        }
        for member in ELASTIC_IP_MEMBERS {
            assert!(
                ELASTIC_IP_PROPERTIES.contains(member),
                "case {member:?}: expected the sent member to be a published property of \
                 {EC2_EIP}, got none"
            );
        }
        for (property, _) in VPC_ENDPOINT_LIST_MEMBERS {
            assert!(
                VPC_ENDPOINT_PROPERTIES.contains(property),
                "case {property:?}: expected the sent list to be a published property of \
                 {EC2_VPC_ENDPOINT}, got none"
            );
        }
        for member in NAT_GATEWAY_SCALAR_MEMBERS {
            assert!(
                NAT_GATEWAY_PROPERTIES.contains(member),
                "case {member:?}: expected the sent member to be a published property of \
                 {EC2_NAT_GATEWAY}, got none"
            );
        }
        for (property, _) in NAT_GATEWAY_LIST_MEMBERS {
            assert!(
                NAT_GATEWAY_PROPERTIES.contains(property),
                "case {property:?}: expected the sent list to be a published property of \
                 {EC2_NAT_GATEWAY}, got none"
            );
        }
        for member in ROUTE_DESTINATIONS {
            assert!(
                ROUTE_PROPERTIES.contains(member),
                "case {member:?}: expected the destination to be a published property of \
                 {EC2_ROUTE}, got none"
            );
        }
        // Every member of the permission a rule RESOURCE sends is a published property of that
        // rule type, and so is each of the two that name the group it is on.
        for (property, _) in EC2_SECURITY_GROUP_INGRESS_MEMBERS {
            assert!(
                SECURITY_GROUP_INGRESS_PROPERTIES.contains(property),
                "case {property:?}: expected the sent member to be a published property of \
                 {EC2_SECURITY_GROUP_INGRESS}, got none"
            );
        }
        for (property, _) in EC2_SECURITY_GROUP_EGRESS_MEMBERS {
            assert!(
                SECURITY_GROUP_EGRESS_PROPERTIES.contains(property),
                "case {property:?}: expected the sent member to be a published property of \
                 {EC2_SECURITY_GROUP_EGRESS}, got none"
            );
        }
        for property in RULE_GROUP_PROPERTIES {
            assert!(
                SECURITY_GROUP_INGRESS_PROPERTIES.contains(property),
                "case {property:?}: expected the group member to be a published property of \
                 {EC2_SECURITY_GROUP_INGRESS}, got none"
            );
        }
    }

    /// RULE: a rule resource is the group it names plus the ONE permission the rest of its
    /// properties are — so the group travels as the authorize call's own member and never
    /// inside the permission, an outbound rule names its group by id alone, and a declaration
    /// naming no group at all is refused rather than authorized onto nothing.
    #[test]
    fn a_rule_resource_names_its_group_and_one_permission() {
        let inbound = declared(
            EC2_SECURITY_GROUP_INGRESS,
            serde_json::json!({
                "GroupId": "sg-0123456789abcdef0",
                "IpProtocol": "tcp",
                "FromPort": 443,
                "ToPort": 443,
                "SourceSecurityGroupId": "sg-00112233445566778",
                "Description": "https from the peer",
            }),
        );
        let group = named_group(&inbound, false).expect("the group the rule is on");
        assert_eq!(
            group,
            ("GroupId".to_string(), "sg-0123456789abcdef0".to_string()),
            "case \"a rule on a group by id\": expected the group member, got {group:?}"
        );
        let permission = security_group_rule_parameters(
            &declared_permission(&inbound),
            EC2_SECURITY_GROUP_INGRESS_MEMBERS,
        )
        .expect("the permission the rule is");
        // In the order the declaration states its members, each against the `IpPermission`
        // member it is: the group's own property is not among them.
        let expected: Vec<(String, String)> = vec![
            ("IpPermissions.1.IpProtocol".to_string(), "tcp".to_string()),
            ("IpPermissions.1.FromPort".to_string(), "443".to_string()),
            ("IpPermissions.1.ToPort".to_string(), "443".to_string()),
            (
                "IpPermissions.1.Groups.1.GroupId".to_string(),
                "sg-00112233445566778".to_string(),
            ),
            (
                "IpPermissions.1.Groups.1.Description".to_string(),
                "https from the peer".to_string(),
            ),
        ];
        assert_eq!(
            permission, expected,
            "case \"a rule referencing another group\": expected {expected:?}, got \
             {permission:?}"
        );
        let by_name = declared(
            EC2_SECURITY_GROUP_INGRESS,
            serde_json::json!({ "GroupName": "default", "IpProtocol": "-1" }),
        );
        let named = named_group(&by_name, false).expect("the group the rule is on");
        assert_eq!(
            named,
            ("GroupName".to_string(), "default".to_string()),
            "case \"an inbound rule on a group by name\": expected the name member, got \
             {named:?}"
        );
        let outbound_by_name = named_group(&by_name, true);
        assert!(
            outbound_by_name.is_err(),
            "case \"an outbound rule on a group by name\": expected a refusal naming the \
             required property, got {outbound_by_name:?}"
        );
        let groupless = named_group(
            &declared(
                EC2_SECURITY_GROUP_EGRESS,
                serde_json::json!({ "IpProtocol": "-1", "CidrIp": "0.0.0.0/0" }),
            ),
            true,
        );
        assert!(
            groupless.is_err(),
            "case \"a rule naming no group\": expected a refusal naming the required \
             property, got {groupless:?}"
        );
    }

    /// Each of the three types is owned by this module, and nothing else is.
    #[test]
    fn each_type_names_its_kind() {
        let cases: &[(&str, Option<Kind>)] = &[
            (EC2_ROUTE_TABLE, Some(Kind::RouteTable)),
            (
                EC2_SUBNET_ROUTE_TABLE_ASSOCIATION,
                Some(Kind::SubnetRouteTableAssociation),
            ),
            (EC2_VPC_ENDPOINT, Some(Kind::VpcEndpoint)),
            (EC2_INTERNET_GATEWAY, Some(Kind::InternetGateway)),
            (EC2_VPN_GATEWAY, Some(Kind::VpnGateway)),
            (
                EC2_VPC_GATEWAY_ATTACHMENT,
                Some(Kind::VpcGatewayAttachment),
            ),
            (EC2_EIP, Some(Kind::ElasticIp)),
            (EC2_ROUTE, Some(Kind::Route)),
            (EC2_NAT_GATEWAY, Some(Kind::NatGateway)),
            (
                EC2_SECURITY_GROUP_INGRESS,
                Some(Kind::SecurityGroupIngress),
            ),
            (EC2_SECURITY_GROUP_EGRESS, Some(Kind::SecurityGroupEgress)),
            ("AWS::EC2::VPC", None),
            ("AWS::EC2::EIPAssociation", None),
        ];
        for (resource_type, expected) in cases {
            let answered = kind_of(resource_type);
            assert_eq!(
                answered, *expected,
                "case {resource_type:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// A declaration that states no required property is refused rather than sent without it,
    /// and a property this module does not apply is refused rather than dropped.
    #[test]
    fn an_incomplete_declaration_is_refused() {
        let cases: &[(&str, &str, Value)] = &[
            (
                "a route table with no VPC",
                EC2_ROUTE_TABLE,
                serde_json::json!({}),
            ),
            (
                "an association with no subnet",
                EC2_SUBNET_ROUTE_TABLE_ASSOCIATION,
                serde_json::json!({ "RouteTableId": "rtb-0123456789abcdef0" }),
            ),
        ];
        for (case, resource_type, properties) in cases {
            let resource = declared(resource_type, properties.clone());
            let refused = match kind_of(resource_type) {
                Some(Kind::RouteTable) => required(&resource, "VpcId"),
                _ => required(&resource, "SubnetId"),
            };
            assert!(
                refused.is_err(),
                "case {case:?}: expected a refusal naming the required property, got {refused:?}"
            );
        }
        let unmapped = refuse_unmapped_properties(
            &declared(
                EC2_VPC_ENDPOINT,
                serde_json::json!({ "DnsOptions": { "DnsRecordIpType": "ipv4" } }),
            ),
            VPC_ENDPOINT_PROPERTIES,
        );
        assert!(
            unmapped.is_err(),
            "case \"an endpoint with DnsOptions\": expected a refusal naming the property, got \
             {unmapped:?}"
        );
        let associated = refuse_unmapped_properties(
            &declared(EC2_EIP, serde_json::json!({ "InstanceId": "i-0123456789abcdef0" })),
            ELASTIC_IP_PROPERTIES,
        );
        assert!(
            associated.is_err(),
            "case \"an address declared onto an instance\": expected a refusal naming the \
             property, got {associated:?}"
        );
    }

    /// One attachment attaches ONE gateway: the call it is made with follows from which of
    /// the two alternatives the declaration names, and a declaration naming both or neither
    /// is refused rather than guessed at.
    #[test]
    fn an_attachment_names_one_gateway() {
        /// The attach one declaration resolves to: the action, the member it names the
        /// gateway under and that gateway's id, or `None` for a declaration that is refused.
        type Attach = Option<(&'static str, &'static str, &'static str)>;
        let cases: &[(&str, Value, Attach)] = &[
            (
                "an internet gateway",
                serde_json::json!({
                    "VpcId": "vpc-0123456789abcdef0",
                    "InternetGatewayId": "igw-0123456789abcdef0",
                }),
                Some((
                    "AttachInternetGateway",
                    "InternetGatewayId",
                    "igw-0123456789abcdef0",
                )),
            ),
            (
                "a virtual private gateway",
                serde_json::json!({
                    "VpcId": "vpc-0123456789abcdef0",
                    "VpnGatewayId": "vgw-0123456789abcdef0",
                }),
                Some((
                    "AttachVpnGateway",
                    "VpnGatewayId",
                    "vgw-0123456789abcdef0",
                )),
            ),
            (
                "both gateways at once",
                serde_json::json!({
                    "VpcId": "vpc-0123456789abcdef0",
                    "InternetGatewayId": "igw-0123456789abcdef0",
                    "VpnGatewayId": "vgw-0123456789abcdef0",
                }),
                None,
            ),
            (
                "no gateway at all",
                serde_json::json!({ "VpcId": "vpc-0123456789abcdef0" }),
                None,
            ),
        ];
        for (case, properties, expected) in cases {
            let resource = declared(EC2_VPC_GATEWAY_ATTACHMENT, properties.clone());
            let resolved = attached_gateway(&resource).ok();
            let answered = resolved
                .as_ref()
                .map(|(action, member, gateway_id)| (*action, *member, gateway_id.as_str()));
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// A declared value reaches a parameter as the text that protocol carries: a string as
    /// written, a boolean and a policy object in their JSON spelling, and a list one entry per
    /// parameter.
    #[test]
    fn a_declared_value_is_sent_as_its_parameter_text() {
        let resource = declared(
            EC2_VPC_ENDPOINT,
            serde_json::json!({
                "ServiceName": "com.amazonaws.us-east-1.s3",
                "PrivateDnsEnabled": false,
                "RouteTableIds": ["rtb-0123456789abcdef0", "rtb-00112233445566778"],
                "PolicyDocument": { "Statement": [] },
                "VpcEndpointType": "",
            }),
        );
        let cases: &[(&str, Option<String>)] = &[
            (
                "ServiceName",
                Some("com.amazonaws.us-east-1.s3".to_string()),
            ),
            ("PrivateDnsEnabled", Some("false".to_string())),
            // An empty string is no value: the member is left out rather than sent empty.
            ("VpcEndpointType", None),
            ("IpAddressType", None),
        ];
        for (property, expected) in cases {
            let answered = scalar(&resource, property);
            assert_eq!(
                answered, *expected,
                "case {property:?}: expected {expected:?}, got {answered:?}"
            );
        }
        let tables = list(&resource, "RouteTableIds");
        assert_eq!(
            tables,
            vec![
                "rtb-0123456789abcdef0".to_string(),
                "rtb-00112233445566778".to_string()
            ],
            "case \"RouteTableIds\": expected both declared tables, got {tables:?}"
        );
        let policy = json_text(resource.properties.get("PolicyDocument").unwrap());
        assert_eq!(
            policy, "{\"Statement\":[]}",
            "case \"PolicyDocument\": expected the declared object as JSON text, got {policy:?}"
        );
    }

    /// The id an answer reports is read off the structure's own members, and an answer that
    /// reports none is a refusal rather than an empty id.
    #[test]
    fn the_assigned_id_is_read_off_the_answer() {
        let cases: &[(&str, &str, Option<&str>)] = &[
            (
                "a route table",
                "<CreateRouteTableResponse><routeTable><routeTableId>rtb-0a1b2c3d4e5f60718\
                 </routeTableId><associationSet/></routeTable></CreateRouteTableResponse>",
                Some("rtb-0a1b2c3d4e5f60718"),
            ),
            (
                "an answer with no id",
                "<CreateRouteTableResponse><routeTable/></CreateRouteTableResponse>",
                None,
            ),
        ];
        for (case, document, expected) in cases {
            let answered = reported(document, "routeTable", "routeTableId").ok();
            assert_eq!(
                answered.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
