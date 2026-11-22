// SPDX-License-Identifier: CC0-1.0

//! The response elements EC2 renders that this checkout's Smithy model does not declare.
//!
//! # The gap this closes
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and the live EC2
//! wire has moved past it in places: a reported shape carries an element the shape's
//! modelled member list does not mention. A recorded `DescribeVpcs` answers every VPC with
//! `<dhcpOptionsSource>`, every operation that reports a Verified Access instance
//! answers it with `<verifiedAccessInstanceTransitGatewayAttachmentSet>` — present and
//! empty for an instance with no transit-gateway attachment — a recorded
//! `CreateInternetGateway` answers its gateway with `<association>`, present and empty for a
//! gateway no association has been made for, a recorded `DescribeNetworkInterfaces` answers
//! the association of an Elastic IP address with `<natEnabled>`, and a recorded
//! `DescribeInstanceTypes` answers every instance type with `<cpuBurstModel>` and every
//! `networkInfo` with `<trafficMirroringSupport>`, and a recorded `DescribeSubnets` answers
//! every subnet with the `<dhcpOptionsId>` it inherits from its VPC and the
//! `<dhcpOptionsSource>` naming where that set came from. The generated serializer walks
//! the model's members and only those, so those elements are missing from this emulator's
//! responses: a client that reads `dhcpOptionsSource` to decide whether a VPC's DHCP
//! options are its own or the account's reads nothing, and a strict deserializer that
//! iterates the attachment set is handed a null collection instead of an empty one.
//!
//! An element the model does not declare cannot be a field of a generated shape, so it
//! cannot be written from a handler. It is written HERE instead, at the one seam where this
//! service renders a success body ([`super::Ec2QueryEnvelope::serialize_output`]), the same
//! way the envelope's own `<return>` flag is: a member schema built by hand, written after
//! the shape's modelled members. The rule is the PROTOCOL's — "an element the live wire
//! renders on this shape is written whichever operation reports the shape" — so it reaches
//! every operation that answers with a VPC, an internet gateway or a Verified Access
//! instance (a create, a modify, a describe, a delete) rather than being restated per
//! handler.
//!
//! Nothing here invents a value: each element is derived from the record being reported —
//! the VPC's own DHCP option set association ([`WrittenMembers::dhcp_options_id`]), the
//! transit-gateway attachments a Verified Access instance of this store holds, of which
//! there are none, no operation of this service attaching one, the association an internet
//! gateway of this store is under, of which there is likewise none, the Availability Zone a
//! reported volume's attachment sits in, which is the zone of the VOLUME the attachment was
//! written under ([`EnclosingRecord`]) — a volume and the instance it is attached to are always
//! in one zone, which is what "the volume must be in the same Availability Zone as the
//! instance" (`AttachVolume`) means — and, for an instance type, the burstable performance
//! and the network adapter the type itself reports.
//!
//! # What it does NOT derive here
//!
//! An element whose value is NOT a function of the reported record — a subnet's inherited
//! DHCP option set id, which has to be resolved from the subnet's VPC, or the public hostname
//! a network interface holds, which the instance's projection of that interface reads off the
//! interface's own record — needs the store, and a protocol has none. The PROJECTION that
//! owns the value resolves it and hands it here ([`hold_subnet_dhcp_options`],
//! [`hold_instance_interface_public_hostname`]), because writing an element the model does
//! not declare is still only possible at this seam.
//!
//! # The other difference this seam closes
//!
//! The same walk the envelope wraps is also where a value's RENDERING is corrected: EC2
//! writes a `double`-typed member as a double, fraction and all, which
//! [`service_common::query_decimal_floats`] does for every floating-point member of a body
//! this service answers with ([`super::Ec2QueryEnvelope::serialize_output`] wraps this walk
//! in its `DecimalFloatsRef`). Nothing here restates that rule.

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType};
use ::aws_smithy_types::{BigDecimal, BigInteger, DateTime, Document};
use ::std::cell::RefCell;
use ::std::collections::BTreeMap;

/// The namespace every shape of this service's model lives in, so an element is only ever
/// written onto a shape of EC2's own.
const EC2_NAMESPACE: &str = "com.amazonaws.ec2";

/// The scope a reported VPC's DHCP option set comes from, as `dhcpOptionsSource` names it:
/// the VPC itself, which is where every DHCP option set this store associates is associated
/// ([`crate::state::ComAmazonawsEc2Amazonec2State::insert_vpc`] gives the VPC the Region's
/// default set as its OWN association) — and so also the scope a SUBNET of that VPC names
/// for the set it inherits from it. The other scope the element reports — an account-level
/// default set, which no operation of this service creates — is why the element exists at
/// all, and why it is written from the association that was found rather than as a fact of
/// every VPC.
const DHCP_OPTIONS_SOURCE_VPC: &str = "vpc";

/// The Smithy member name a VPC's DHCP option set id is written under, which is what says
/// the VPC has an association for `dhcpOptionsSource` to name the scope of.
const DHCP_OPTIONS_ID_MEMBER: &str = "DhcpOptionsId";

/// The Smithy member name a subnet's own id is written under, which is what says the
/// structure being walked IS a reported subnet — and the key the option set in force for it
/// was held under.
const SUBNET_ID_MEMBER: &str = "SubnetId";

thread_local! {
    /// The DHCP option set in force for each subnet THIS response reports, keyed by
    /// `SubnetId`.
    ///
    /// A subnet inherits the option set its VPC is associated with, which is a fact of the
    /// STORE rather than of the subnet record being written, so the projection that reports
    /// the subnet resolves it ([`hold_subnet_dhcp_options`]) and this seam — the only place
    /// an element the model does not declare can be written — writes it.
    ///
    /// # The invariant this rests on
    ///
    /// The same one `super::management_events` holds its outcome under, from the other side:
    /// a handler fills this slot while building its output, and the response that output is
    /// rendered into is serialized on this thread with nothing awaited in between
    /// (`smithy_server_runtime::run_operation_http`). The run about to serve a call clears
    /// the slot first ([`forget_any_held_subnet_options`]), and an empty slot means the
    /// element is simply not written — never a value carried over from another call.
    static SUBNET_DHCP_OPTIONS: RefCell<BTreeMap<String, String>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Hold the DHCP option set in force for each subnet the response being built reports, as
/// `(SubnetId, DhcpOptionsId)` pairs read out of the store by the projection that reports
/// them ([`crate::state::ComAmazonawsEc2Amazonec2State::dhcp_options_in_force_for_subnet`]).
///
/// Replaces whatever was held, so one call's answer never leaks into another's. A subnet
/// left out of `entries` is reported without the elements below, there being no option set
/// of this account's to name.
pub fn hold_subnet_dhcp_options(entries: impl IntoIterator<Item = (String, String)>) {
    let held: BTreeMap<String, String> = entries.into_iter().collect();
    SUBNET_DHCP_OPTIONS.with(|slot| *slot.borrow_mut() = held);
}

/// Drop whatever subnet option sets this thread may still hold: they belong to no call the
/// run about to start serves.
pub fn forget_any_held_subnet_options() {
    SUBNET_DHCP_OPTIONS.with(|slot| slot.borrow_mut().clear());
}

/// The DHCP option set held for the subnet `subnet_id`, or `None` when the projection held
/// none for it.
fn held_subnet_dhcp_options(subnet_id: &str) -> Option<String> {
    SUBNET_DHCP_OPTIONS.with(|slot| slot.borrow().get(subnet_id).cloned())
}

thread_local! {
    /// The public hostname each network interface THIS response reports under the instance
    /// holding it holds, keyed by `NetworkInterfaceId`.
    ///
    /// Held for the same reason as [`SUBNET_DHCP_OPTIONS`], and on the same invariant: a
    /// public hostname is a member of the interface's OWN shape
    /// (`NetworkInterface$PublicDnsName`) and `InstanceNetworkInterface` declares none, so the
    /// projection that reports an interface under its instance
    /// ([`crate::state::instance_network_interfaces`]) hands the hostname here and this seam
    /// writes it. An interface left out of the slot is reported without the elements below.
    static INSTANCE_INTERFACE_PUBLIC_HOSTNAMES: RefCell<BTreeMap<String, String>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Hold the public hostname the network interface `network_interface_id` holds, as the
/// instance's projection of that interface read it off the interface's own record.
///
/// Keyed by interface id and added to rather than replacing, because one response reports the
/// interfaces of every instance it answers with, projected one instance at a time. The run
/// about to serve a call clears the slot ([`forget_any_held_instance_interface_hostnames`]),
/// so nothing another call held can be written onto this one's interfaces.
pub fn hold_instance_interface_public_hostname(network_interface_id: &str, public_dns_name: &str) {
    INSTANCE_INTERFACE_PUBLIC_HOSTNAMES.with(|slot| {
        slot.borrow_mut().insert(
            network_interface_id.to_string(),
            public_dns_name.to_string(),
        )
    });
}

/// Drop whatever interface hostnames this thread may still hold: they belong to no call the
/// run about to start serves.
pub fn forget_any_held_instance_interface_hostnames() {
    INSTANCE_INTERFACE_PUBLIC_HOSTNAMES.with(|slot| slot.borrow_mut().clear());
}

/// The public hostname held for the interface `network_interface_id`, or `None` when the
/// projection held none for it.
fn held_instance_interface_public_hostname(network_interface_id: &str) -> Option<String> {
    INSTANCE_INTERFACE_PUBLIC_HOSTNAMES
        .with(|slot| slot.borrow().get(network_interface_id).cloned())
}

/// The Smithy member name a Verified Access instance's own id is written under, which is
/// what says the structure being walked IS such an instance.
const VERIFIED_ACCESS_INSTANCE_ID_MEMBER: &str = "VerifiedAccessInstanceId";

/// The Smithy member name an internet gateway's own id is written under, which is what says
/// the structure being walked IS a reported internet gateway rather than a shape whose
/// member merely shares the name (`Explanation$InternetGateway` targets an analysis
/// component).
const INTERNET_GATEWAY_ID_MEMBER: &str = "InternetGatewayId";

/// `InstanceMetadataProtocolState`'s `enabled`, the state of the IPv4 metadata endpoint on
/// every instance: IMDS has always served IPv4 and no operation of this service turns it
/// off — `HttpProtocolIpv6` is the protocol a launch switches on — so the element the live
/// wire renders beside that one reports the endpoint as enabled.
const METADATA_PROTOCOL_IPV4_ENABLED: &str = "enabled";

/// The Smithy member name an instance's IMDSv2 requirement is written under, which is what
/// says the structure being walked IS a reported metadata option set.
const HTTP_TOKENS_MEMBER: &str = "HttpTokens";

/// The Smithy member name a volume's own id is written under, which is what says the
/// structure being walked carries one — the identity check the volume attachment's elements
/// below are gated on.
const VOLUME_ID_MEMBER: &str = "VolumeId";

/// The Smithy member name an Availability Zone's NAME is written under, on every shape that
/// reports one.
const AVAILABILITY_ZONE_MEMBER: &str = "AvailabilityZone";

/// The Smithy member name an Availability Zone's ID is written under.
const AVAILABILITY_ZONE_ID_MEMBER: &str = "AvailabilityZoneId";

/// The Smithy member name an instance type's own name is written under, on the reported
/// instance type and on every other shape that names one.
const INSTANCE_TYPE_MEMBER: &str = "InstanceType";

/// The Smithy member name an instance type's burstable performance is written under, which
/// is what says the structure being walked IS a reported instance type — and what the burst
/// model below is read off.
const BURSTABLE_PERFORMANCE_MEMBER: &str = "BurstablePerformanceSupported";

/// The Smithy member name an instance type's Elastic Network Adapter support is written
/// under, which is what says the structure being walked IS a reported `networkInfo`.
const ENA_SUPPORT_MEMBER: &str = "EnaSupport";

/// The Smithy member name an instance type's bare metal flag is written under, which is
/// what says a type runs the caller's software on the host with no hypervisor under it.
const BARE_METAL_MEMBER: &str = "BareMetal";

/// The Smithy member name a network interface's own id is written under, which is what says
/// WHICH interface a structure named after one is, and the key its public hostname was held
/// under.
const NETWORK_INTERFACE_ID_MEMBER: &str = "NetworkInterfaceId";

/// The Smithy member name a public hostname is written under, on every shape that reports
/// one — on a reported instance, the `dnsName` the public-hostname options below name.
const PUBLIC_DNS_NAME_MEMBER: &str = "PublicDnsName";

/// The Smithy member name an allocation id is written under, which on a reported association
/// is what says the public address bound there is an Elastic IP address the account
/// ALLOCATED rather than one AWS assigned itself.
const ALLOCATION_ID_MEMBER: &str = "AllocationId";

/// The Smithy member name a reported association is written under. Every shape that reports
/// one names the member after the association rather than after the shape it targets, so the
/// DECLARING shape is what says WHICH association it is ([`recorded_writer_for`]).
const ASSOCIATION_MEMBER: &str = "Association";

/// The shapes whose `Association` member is the association of a public address with a
/// NETWORK INTERFACE (`com.amazonaws.ec2#NetworkInterfaceAssociation`): the interface itself,
/// and the private address of it the public one is bound to — the two places one such
/// association is reported.
const NETWORK_INTERFACE_ASSOCIATION_HOLDERS: &[&str] =
    &["NetworkInterface", "NetworkInterfacePrivateIpAddress"];

/// `cpuBurstModel` of a burstable performance type: its vCPUs run at a baseline rate and
/// spend the CPU credits they earn to run above it.
const CPU_BURST_MODEL_CREDIT: &str = "credit";

/// `cpuBurstModel` of a type whose vCPUs run at their full rate, with no burst above a
/// baseline and so no burst model at all.
const CPU_BURST_MODEL_NONE: &str = "none";

/// `trafficMirroringSupport` of a type that supports Traffic Mirroring, and of one that does
/// not: copying a session's packets is done under the guest rather than by it, which a bare
/// metal instance — running on the host with no hypervisor under it — has nothing to do.
const TRAFFIC_MIRRORING_SUPPORTED: &str = "supported";

/// `trafficMirroringSupport` of a type whose packets nothing under the guest can copy.
const TRAFFIC_MIRRORING_UNSUPPORTED: &str = "unsupported";

/// `<cpuBurstModel>` on a reported instance type — a member schema for the same reason as
/// [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#InstanceTypeInfo` declares no such
/// member.
static INSTANCE_TYPE_CPU_BURST_MODEL: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#InstanceTypeInfo$cpuBurstModel",
        EC2_NAMESPACE,
        "cpuBurstModel",
    ),
    ShapeType::String,
    "cpuBurstModel",
    0,
);

/// `<trafficMirroringSupport>` on a reported instance type's network settings — a member
/// schema for the same reason as [`VPC_DHCP_OPTIONS_SOURCE`]:
/// `com.amazonaws.ec2#NetworkInfo` declares no such member.
static NETWORK_INFO_TRAFFIC_MIRRORING_SUPPORT: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#NetworkInfo$trafficMirroringSupport",
        EC2_NAMESPACE,
        "trafficMirroringSupport",
    ),
    ShapeType::String,
    "trafficMirroringSupport",
    0,
);

/// `<dhcpOptionsSource>` on a reported VPC.
///
/// A member schema rather than a modelled one: `com.amazonaws.ec2#Vpc` declares no such
/// member, so the element has no shape of its own to borrow. The XML codec names an element
/// from the member name, which is what makes this write `<dhcpOptionsSource>`.
static VPC_DHCP_OPTIONS_SOURCE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#Vpc$dhcpOptionsSource",
        EC2_NAMESPACE,
        "dhcpOptionsSource",
    ),
    ShapeType::String,
    "dhcpOptionsSource",
    0,
);

/// `<dhcpOptionsId>` on a reported SUBNET — a member schema for the same reason as
/// [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#Subnet` declares neither this member nor
/// the source below, where the VPC's own shape declares the id.
static SUBNET_DHCP_OPTIONS_ID: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#Subnet$dhcpOptionsId",
        EC2_NAMESPACE,
        "dhcpOptionsId",
    ),
    ShapeType::String,
    "dhcpOptionsId",
    0,
);

/// `<dhcpOptionsSource>` on a reported SUBNET, naming where the set above came from.
static SUBNET_DHCP_OPTIONS_SOURCE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#Subnet$dhcpOptionsSource",
        EC2_NAMESPACE,
        "dhcpOptionsSource",
    ),
    ShapeType::String,
    "dhcpOptionsSource",
    0,
);

/// `<publicIpDnsNameOptions>` on a reported INSTANCE — a member schema for the same reason
/// as [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#Instance` declares the public hostname
/// (`dnsName`) and no options naming it, where the network interface's own shape declares
/// both.
///
/// The element is a STRUCTURE, so the modelled `PublicIpDnsNameOptions` shape is what is
/// written into it ([`crate::state::public_ip_dns_name_options`]) rather than a second
/// hand-built rendering of the same members.
static INSTANCE_PUBLIC_IP_DNS_NAME_OPTIONS: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#Instance$publicIpDnsNameOptions",
        EC2_NAMESPACE,
        "publicIpDnsNameOptions",
    ),
    ShapeType::Structure,
    "publicIpDnsNameOptions",
    0,
);

/// `<publicDnsName>` on a network interface reported under the instance holding it — a member
/// schema for the same reason as [`VPC_DHCP_OPTIONS_SOURCE`]:
/// `com.amazonaws.ec2#InstanceNetworkInterface` declares neither this member nor the options
/// below, where `com.amazonaws.ec2#NetworkInterface` declares both.
static INSTANCE_NETWORK_INTERFACE_PUBLIC_DNS_NAME: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#InstanceNetworkInterface$publicDnsName",
        EC2_NAMESPACE,
        "publicDnsName",
    ),
    ShapeType::String,
    "publicDnsName",
    0,
);

/// `<publicIpDnsNameOptions>` on a network interface reported under the instance holding it,
/// naming the hostname written above.
static INSTANCE_NETWORK_INTERFACE_PUBLIC_IP_DNS_NAME_OPTIONS: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#InstanceNetworkInterface$publicIpDnsNameOptions",
        EC2_NAMESPACE,
        "publicIpDnsNameOptions",
    ),
    ShapeType::Structure,
    "publicIpDnsNameOptions",
    0,
);

/// `<availabilityZone>` on a reported volume attachment — a member schema for the same
/// reason as [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#VolumeAttachment` declares no
/// Availability Zone member.
static VOLUME_ATTACHMENT_AVAILABILITY_ZONE: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#VolumeAttachment$availabilityZone",
        EC2_NAMESPACE,
        "availabilityZone",
    ),
    ShapeType::String,
    "availabilityZone",
    0,
);

/// `<availabilityZoneId>` on a reported volume attachment — the zone's id beside the name
/// above.
static VOLUME_ATTACHMENT_AVAILABILITY_ZONE_ID: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#VolumeAttachment$availabilityZoneId",
        EC2_NAMESPACE,
        "availabilityZoneId",
    ),
    ShapeType::String,
    "availabilityZoneId",
    0,
);

/// `<httpProtocolIpv4>` on a reported instance metadata option set — a member schema for
/// the same reason as [`VPC_DHCP_OPTIONS_SOURCE`].
static METADATA_OPTIONS_HTTP_PROTOCOL_IPV4: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#InstanceMetadataOptionsResponse$httpProtocolIpv4",
        EC2_NAMESPACE,
        "httpProtocolIpv4",
    ),
    ShapeType::String,
    "httpProtocolIpv4",
    0,
);

/// `<verifiedAccessInstanceTransitGatewayAttachmentSet>` on a reported Verified Access
/// instance — a member schema for the same reason as [`VPC_DHCP_OPTIONS_SOURCE`].
static VERIFIED_ACCESS_INSTANCE_TRANSIT_GATEWAY_ATTACHMENT_SET: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#VerifiedAccessInstance$verifiedAccessInstanceTransitGatewayAttachmentSet",
        EC2_NAMESPACE,
        "verifiedAccessInstanceTransitGatewayAttachmentSet",
    ),
    ShapeType::List,
    "verifiedAccessInstanceTransitGatewayAttachmentSet",
    0,
);

/// `<association>` on a reported internet gateway — a member schema for the same reason as
/// [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#InternetGateway` declares only
/// `attachmentSet`, `internetGatewayId`, `ownerId` and `tagSet`.
///
/// The element the live wire renders is a CONTAINER naming the association rather than a
/// value of its own, so it is written as a structure ([`NoAssociation`]) — the same reading
/// the always-empty attachment set above takes of its collection, rather than an element
/// that happens to render the same bytes while claiming to be text.
static INTERNET_GATEWAY_ASSOCIATION: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#InternetGateway$association",
        EC2_NAMESPACE,
        "association",
    ),
    ShapeType::Structure,
    "association",
    0,
);

/// `<natEnabled>` on a reported network-interface association — a member schema for the same
/// reason as [`VPC_DHCP_OPTIONS_SOURCE`]: `com.amazonaws.ec2#NetworkInterfaceAssociation`
/// declares the addressing members of the association and no flag for the translation that
/// carries traffic to the address.
static NETWORK_INTERFACE_ASSOCIATION_NAT_ENABLED: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.ec2#NetworkInterfaceAssociation$natEnabled",
        EC2_NAMESPACE,
        "natEnabled",
    ),
    ShapeType::Boolean,
    "natEnabled",
    0,
);

/// The content of the `association` element of a gateway no association has been made for:
/// no member at all, which is what makes the element present and empty.
struct NoAssociation;

impl SerializableStruct for NoAssociation {
    fn serialize_members(&self, _s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        Ok(())
    }
}

/// One BORROWED structure serialized with the recorded elements of every shape under it.
///
/// The entry point: a service that renders its own protocol envelope reaches its output as a
/// `&dyn SerializableStruct` and wraps it here. Every structure NESTED under that output is
/// re-entered, because the codec walks a nested structure with its own serializer, so the
/// rule reaches the reported shape wherever it sits — a list item, a member of its own.
pub struct RecordedMembersRef<'a>(pub &'a dyn SerializableStruct);

impl SerializableStruct for RecordedMembersRef<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.0.serialize_members(&mut RecordedMemberSerializer {
            inner: s,
            written: None,
            zone: EnclosingRecord::default(),
        })
    }
}

/// One nested structure of no recorded elements of its own, serialized with the zone of the
/// record it was written under carried on into it — so a shape that DOES render the zone
/// still gets it however deep under that record it sits.
struct RecordedMembersUnder<'a>(&'a dyn SerializableStruct, EnclosingRecord);

impl SerializableStruct for RecordedMembersUnder<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        self.0.serialize_members(&mut RecordedMemberSerializer {
            inner: s,
            written: None,
            zone: self.1.clone(),
        })
    }
}

/// The CONTEXT the structure a nested one is written UNDER reported, as that nested
/// structure's own recorded elements need it.
///
/// What this walk carries DOWN: a volume attachment renders the zone it
/// is in and the `VolumeAttachment` structure has no member for it, so the zone is taken
/// from the volume the attachment was reported under, whose own `AvailabilityZone` and
/// `AvailabilityZoneId` members are written before its `attachmentSet`; an instance type's
/// network settings render whether the type supports Traffic Mirroring and the `NetworkInfo`
/// structure has no member saying which type it belongs to or whether that type is bare
/// metal, so those too are taken from the instance type the settings were reported under.
/// Empty for a
/// structure written under one that reported neither, in which case nothing is rendered.
#[derive(Debug, Default, Clone)]
struct EnclosingRecord {
    /// `Volume$AvailabilityZone` as the enclosing structure wrote it.
    availability_zone: Option<String>,
    /// `Volume$AvailabilityZoneId` as the enclosing structure wrote it.
    availability_zone_id: Option<String>,
    /// `InstanceTypeInfo$BareMetal` as the enclosing structure wrote it.
    bare_metal: Option<bool>,
    /// `InstanceTypeInfo$InstanceType` as the enclosing structure wrote it, which is what
    /// says WHICH type's settings a nested `NetworkInfo` belongs to.
    instance_type: Option<String>,
}

impl EnclosingRecord {
    /// What a structure written under the one being walked is handed: whatever that structure
    /// reported for ITSELF, and otherwise what it was handed in turn — so a record passes on
    /// the context it was given rather than hiding it.
    fn inherited_from(&self, outer: &Self) -> Self {
        Self {
            availability_zone: self
                .availability_zone
                .clone()
                .or_else(|| outer.availability_zone.clone()),
            availability_zone_id: self
                .availability_zone_id
                .clone()
                .or_else(|| outer.availability_zone_id.clone()),
            bare_metal: self.bare_metal.or(outer.bare_metal),
            instance_type: self
                .instance_type
                .clone()
                .or_else(|| outer.instance_type.clone()),
        }
    }
}

/// What the structure being walked wrote of the members a recorded element is derived from.
///
/// Collected while the shape's MODELLED members are written, so the element below is decided
/// by the record the response carries rather than by the shape it carries it in. It is also
/// what IDENTIFIES the shape: see [`recorded_writer_for`] for why a name alone does not.
#[derive(Debug, Default, Clone)]
struct WrittenMembers {
    /// Whether the structure wrote a DHCP option set id of its own — the set associated
    /// WITH IT, which is the scope `dhcpOptionsSource` reports.
    dhcp_options_id: bool,
    /// The subnet id the structure wrote for itself, which is what says it IS a reported
    /// subnet and which subnet's inherited option set is written on it.
    subnet_id: Option<String>,
    /// Whether the structure wrote a Verified Access instance id of its own, which is the
    /// id the attachment set below belongs to.
    verified_access_instance_id: bool,
    /// Whether the structure wrote an internet gateway id of its own, which is what makes a
    /// structure named after an internet gateway one.
    internet_gateway_id: bool,
    /// Whether the structure wrote a volume id of its own, which is what makes a structure
    /// named after a volume attachment one.
    volume_id: bool,
    /// Whether the structure wrote an IMDSv2 requirement of its own, which is what makes a
    /// structure named after a reported metadata option set one.
    http_tokens: bool,
    /// Whether the structure wrote an instance type name of its own.
    instance_type: bool,
    /// The burstable performance the structure reported for itself, which is what says it IS
    /// a reported instance type and what its burst model follows from.
    burstable_performance_supported: Option<bool>,
    /// The Elastic Network Adapter support the structure reported for itself, which is what
    /// says it IS a reported `networkInfo`.
    ena_support: Option<String>,
    /// The network interface id the structure wrote for itself, which is what says WHICH
    /// interface it is and whose public hostname is written on it.
    network_interface_id: Option<String>,
    /// Whether the structure wrote an allocation id of its own, which on an association is
    /// what says the public address bound there is an Elastic IP address the account
    /// allocated.
    allocation_id: bool,
    /// The public hostname the structure wrote for itself, which an instance's
    /// public-hostname options NAME — and whose emptiness says the instance holds no public
    /// address for them to name.
    public_dns_name: Option<String>,
    /// The context the structure reported for ITSELF — the zone it is in, and whether it is
    /// a bare metal instance type — which is what the structures nested under it are handed.
    zone: EnclosingRecord,
}

/// How one shape's recorded elements are written, once its modelled members have been:
/// from what the structure itself wrote and the zone of the record it was written under.
type RecordedWriter =
    fn(&mut dyn ShapeSerializer, &WrittenMembers, &EnclosingRecord) -> Result<(), SerdeError>;

/// How `schema`'s own shape renders the elements the model does not declare for it, or
/// `None` for a shape the live wire renders exactly as the model declares it.
///
/// `schema` is what the codec was handed for the structure being written, which is the
/// structure's own schema when it is a list item and the MEMBER's schema when it is a member
/// of another structure. Both carry the same shape NAME in EC2's model — every member that
/// targets one of these shapes is named after it (`CreateVpcResult$Vpc`,
/// `ModifyVerifiedAccessInstanceResult$VerifiedAccessInstance`) — so one name answers for a
/// shape in either position.
///
/// A name is NOT on its own enough to identify the shape: a member may be named after one
/// shape and target another (`Explanation$Vpc` and `PathComponent$Vpc` are members named
/// `Vpc` that target `AnalysisComponent`). So the name only selects the writer, and each
/// writer then asks [`WrittenMembers`] for the member that only its own shape carries — the
/// identity of the structure and the derivation of its element being the same question.
fn recorded_writer_for(schema: &Schema) -> Option<RecordedWriter> {
    if schema.shape_id().namespace() != EC2_NAMESPACE {
        return None;
    }
    match schema.shape_id().shape_name() {
        "Vpc" => Some(write_vpc_recorded_members),
        "Subnet" => Some(write_subnet_recorded_members),
        "VerifiedAccessInstance" => Some(write_verified_access_instance_recorded_members),
        "InternetGateway" => Some(write_internet_gateway_recorded_members),
        "VolumeAttachment" => Some(write_volume_attachment_recorded_members),
        "Instance" => Some(write_instance_recorded_members),
        "InstanceNetworkInterface" => Some(write_instance_network_interface_recorded_members),
        "InstanceTypeInfo" => Some(write_instance_type_recorded_members),
        "NetworkInfo" => Some(write_network_info_recorded_members),
        // An INSTANCE's metadata option set (`InstanceMetadataOptionsResponse`), which this
        // model reports under exactly two members: `Instance$MetadataOptions` and
        // `ModifyInstanceMetadataOptionsResult$InstanceMetadataOptions`. A LAUNCH TEMPLATE's
        // option set is a DIFFERENT shape under a member of the same name
        // (`ResponseLaunchTemplateData$MetadataOptions`, targeting
        // `LaunchTemplateInstanceMetadataOptions`) and carries the same member names, so the
        // name alone cannot tell them apart and the DECLARING shape decides.
        "InstanceMetadataOptions" => Some(write_metadata_options_recorded_members),
        "MetadataOptions" if declaring_shape(schema).as_deref() == Some("Instance") => {
            Some(write_metadata_options_recorded_members)
        }
        // The association of a PUBLIC ADDRESS with a network interface
        // (`NetworkInterfaceAssociation`), reported under a member named after the
        // association rather than after that shape. Other shapes declare a member of the
        // same name targeting something else — an internet gateway's association, written
        // below, and the instance report's own projection of this one
        // (`InstanceNetworkInterfaceAssociation`) — so the DECLARING shape decides here too.
        ASSOCIATION_MEMBER
            if declaring_shape(schema)
                .is_some_and(|shape| NETWORK_INTERFACE_ASSOCIATION_HOLDERS.contains(&shape.as_str())) =>
        {
            Some(write_network_interface_association_recorded_members)
        }
        // A reported volume renders every element the model declares for it, and is walked
        // for its own shape all the same: the zone it reports is the zone the attachments
        // nested under it are in ([`EnclosingRecord`]).
        "Volume" => Some(write_no_recorded_members),
        _ => None,
    }
}

/// The name of the shape that DECLARES `schema`, when `schema` is a member schema, or
/// `None` for anything else — a member's shape id is `<namespace>#<Shape>$<member>`, so the
/// shape it belongs to is the part between the two separators.
///
/// What tells two members of the same NAME apart when they target different shapes, which
/// is the one question [`recorded_writer_for`]'s name match cannot answer on its own.
fn declaring_shape(schema: &Schema) -> Option<String> {
    schema.member_name()?;
    let id = schema.shape_id().to_string();
    let (_, qualified) = id.split_once('#')?;
    let (shape, _) = qualified.split_once('$')?;
    Some(shape.to_string())
}

/// A shape that renders no element of its own, walked only so the structures NESTED under
/// it are handed what it reported ([`EnclosingRecord`]).
fn write_no_recorded_members(
    _s: &mut dyn ShapeSerializer,
    _written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    Ok(())
}

/// A reported metadata option set's recorded elements: the state of the IPv4 metadata
/// endpoint ([`METADATA_PROTOCOL_IPV4_ENABLED`]), which the live wire renders beside the
/// IPv6 one the model declares.
///
/// Written only for a structure that reported an IMDSv2 requirement of its own, which is
/// what makes it a reported option set rather than a shape whose member shares the name.
fn write_metadata_options_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.http_tokens {
        return Ok(());
    }
    s.write_string(
        &METADATA_OPTIONS_HTTP_PROTOCOL_IPV4,
        METADATA_PROTOCOL_IPV4_ENABLED,
    )
}

/// A reported volume attachment's recorded elements: the Availability Zone it is in, by
/// name and by id.
///
/// "The volume must be in the same Availability Zone as the instance" (`AttachVolume`), so
/// the zone of an attachment IS the zone of the volume it belongs to — taken from the volume
/// this attachment was reported under rather than looked up, which is the only reading a
/// protocol has and the one that cannot disagree with the volume's own report.
///
/// Written only for a structure that reported a volume id of its own, and only under a
/// record that reported a zone: an attachment reported outside a volume has no zone to name,
/// so the elements are left out rather than claiming one.
fn write_volume_attachment_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.volume_id {
        return Ok(());
    }
    if let Some(availability_zone) = zone.availability_zone.as_deref() {
        s.write_string(&VOLUME_ATTACHMENT_AVAILABILITY_ZONE, availability_zone)?;
    }
    if let Some(availability_zone_id) = zone.availability_zone_id.as_deref() {
        s.write_string(&VOLUME_ATTACHMENT_AVAILABILITY_ZONE_ID, availability_zone_id)?;
    }
    Ok(())
}

/// A reported INSTANCE's recorded elements: the public-hostname options naming the public
/// hostname it reported.
///
/// "Public hostname type options" (`PublicIpDnsNameOptions`) name the public hostname a
/// resource holds, so they are derived from the hostname the instance itself wrote
/// (`Instance$PublicDnsName`, the `dnsName` element) through the one derivation this crate
/// has ([`crate::state::public_ip_dns_name_options`]) — never resolved a second time, so the
/// options and the hostname they name cannot disagree.
///
/// Written only for a structure that reported a public hostname of its own, and only when
/// that hostname is not the EMPTY one an instance holding no public address reports: there is
/// then no hostname for the options to name.
fn write_instance_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    let Some(public_dns_name) = written.public_dns_name.as_deref() else {
        return Ok(());
    };
    let Some(options) = crate::state::public_ip_dns_name_options(public_dns_name) else {
        return Ok(());
    };
    s.write_struct(&INSTANCE_PUBLIC_IP_DNS_NAME_OPTIONS, &options)
}

/// A network interface reported UNDER its instance: the public hostname it holds, and the
/// public-hostname options naming it.
///
/// The same two facts `DescribeNetworkInterfaces` reports for that interface off its own
/// shape, read off the one record both reports project — handed here by that projection
/// ([`hold_instance_interface_public_hostname`]), a protocol having no store to resolve an
/// interface in. So an interface read through the instance holding it and the same interface
/// read on its own cannot disagree about the hostname it answers to.
///
/// The hostname is written whenever one was held, EMPTY included: an interface holding no
/// public address reports the empty hostname, which is the member AWS renders for one rather
/// than no member at all. The options are written only when there is a hostname for them to
/// name.
fn write_instance_network_interface_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    let Some(network_interface_id) = written.network_interface_id.as_deref() else {
        return Ok(());
    };
    let Some(public_dns_name) = held_instance_interface_public_hostname(network_interface_id)
    else {
        return Ok(());
    };
    s.write_string(
        &INSTANCE_NETWORK_INTERFACE_PUBLIC_DNS_NAME,
        &public_dns_name,
    )?;
    let Some(options) = crate::state::public_ip_dns_name_options(&public_dns_name) else {
        return Ok(());
    };
    s.write_struct(
        &INSTANCE_NETWORK_INTERFACE_PUBLIC_IP_DNS_NAME_OPTIONS,
        &options,
    )
}

/// A reported instance type's recorded elements: the burst model of its vCPUs.
///
/// "Burstable performance instances ... can burst above the baseline" (EC2 User Guide): a
/// type whose vCPUs run at a baseline spends CPU credits to run above it, and a type that
/// runs at its full rate has no burst to model. So the element is read off the burstable
/// performance the type itself reported, which is the fact the model DOES declare for it.
///
/// Written only for a structure that reported an instance type and a burstable performance
/// of its own, which is what makes it a reported instance type rather than a shape whose
/// member shares the name.
fn write_instance_type_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    let Some(burstable) = written.burstable_performance_supported else {
        return Ok(());
    };
    if !written.instance_type {
        return Ok(());
    }
    let burst_model = if burstable {
        CPU_BURST_MODEL_CREDIT
    } else {
        CPU_BURST_MODEL_NONE
    };
    s.write_string(&INSTANCE_TYPE_CPU_BURST_MODEL, burst_model)
}

/// A reported instance type's network settings' recorded elements: whether the type supports
/// Traffic Mirroring.
///
/// Whether a type's packets can be copied is a fact the catalog publishes per type
/// ([`crate::state::published_instance_type_facts`]), read off the instance type these
/// settings were reported under ([`EnclosingRecord`]), the `NetworkInfo` structure having no
/// member of its own for it. For a type the catalog holds no row for, the answer follows from
/// the hypervisor: copying the packets of a session is done UNDER the guest rather than by it,
/// so a bare metal type — which runs the caller's software on the host with no hypervisor
/// under it, and reports no hypervisor at all — does not support it while every other type
/// does.
///
/// Written only for a structure that reported an adapter support of its own, which is what
/// makes it a reported `networkInfo`.
fn write_network_info_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if written.ena_support.is_none() {
        return Ok(());
    }
    let published = zone
        .instance_type
        .as_deref()
        .and_then(crate::state::published_instance_type_facts)
        .and_then(|facts| facts.traffic_mirroring_supported);
    let supported = published.unwrap_or(zone.bare_metal != Some(true));
    let mirroring = if supported {
        TRAFFIC_MIRRORING_SUPPORTED
    } else {
        TRAFFIC_MIRRORING_UNSUPPORTED
    };
    s.write_string(&NETWORK_INFO_TRAFFIC_MIRRORING_SUPPORT, mirroring)
}

/// A reported network-interface association's recorded elements: that the public address
/// bound there is reached by network address translation.
///
/// An Elastic IP address is a PUBLIC address held by the network's internet gateway, which
/// translates between it and the private address it is bound to — so the association of one
/// is a translated binding, which is what the element says. It is derived from the ALLOCATION
/// the association itself reported (`NetworkInterfaceAssociation$AllocationId`): an
/// allocation is what the account holds an Elastic IP address as, so an association that
/// reports one binds an Elastic IP address. An association that reports no allocation — a
/// public address AWS assigned the interface itself, which this store records with the owner
/// `amazon` and no allocation at all — has none of this to report, so the element is left out
/// rather than claiming a translation for an address the store knows nothing of the kind
/// about.
///
/// The rule is the shape's, so it reaches the association wherever one is reported: on the
/// interface, and on the private address the public one is bound to, which is where
/// `DescribeNetworkInterfaces` reports each.
fn write_network_interface_association_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.allocation_id {
        return Ok(());
    }
    s.write_boolean(&NETWORK_INTERFACE_ASSOCIATION_NAT_ENABLED, true)
}

/// A reported VPC's recorded elements: the scope its DHCP option set comes from.
///
/// Written only for a VPC that reported an option set association of its own: a VPC with no
/// association has no scope to name, so the element is left out rather than claiming one.
fn write_vpc_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.dhcp_options_id {
        return Ok(());
    }
    s.write_string(&VPC_DHCP_OPTIONS_SOURCE, DHCP_OPTIONS_SOURCE_VPC)
}

/// A reported SUBNET's recorded elements: the DHCP option set in force for it and the scope
/// that set came from.
///
/// A subnet holds no option set of its own — the one in force for it is the one its VPC is
/// associated with — so the value is the one the projection reporting this subnet read out of
/// the store for it ([`hold_subnet_dhcp_options`]). Written only for a structure that wrote a
/// subnet id of its own, and only when an option set was held for that id: a subnet whose VPC
/// is associated with none has no set to report and no scope to name.
fn write_subnet_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    let Some(subnet_id) = written.subnet_id.as_deref() else {
        return Ok(());
    };
    let Some(dhcp_options_id) = held_subnet_dhcp_options(subnet_id) else {
        return Ok(());
    };
    s.write_string(&SUBNET_DHCP_OPTIONS_ID, &dhcp_options_id)?;
    s.write_string(&SUBNET_DHCP_OPTIONS_SOURCE, DHCP_OPTIONS_SOURCE_VPC)
}

/// A reported Verified Access instance's recorded elements: the transit-gateway attachments
/// it holds.
///
/// Always EMPTY, and present: no operation of this service attaches a transit gateway to a
/// Verified Access instance, so an instance of this store holds none — and an empty
/// collection is still a collection, which is the whole difference this closes for a client
/// that iterates it.
///
/// Written only for a structure that reported an instance id of its own, which is what makes
/// it one of these instances rather than a shape whose member merely shares the name.
fn write_verified_access_instance_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.verified_access_instance_id {
        return Ok(());
    }
    s.write_list(
        &VERIFIED_ACCESS_INSTANCE_TRANSIT_GATEWAY_ATTACHMENT_SET,
        &|_items| Ok(()),
    )
}

/// A reported INTERNET GATEWAY's recorded elements: the association it is under.
///
/// Always EMPTY, and present: an association is not a thing any operation of this service
/// makes — what a caller does with a new gateway is "attach it to a VPC using
/// AttachInternetGateway" (`CreateInternetGateway`), which is the record's own
/// `attachmentSet` member — so a gateway of this store is under none, and the element says so
/// by being there and carrying nothing. That is the whole difference it closes for a client
/// that reads it to decide whether a gateway is already associated: absent reads as "this
/// answer does not say", present and empty as "nothing".
///
/// Written only for a structure that reported an internet gateway id of its own, which is
/// what makes it one of these gateways rather than a shape whose member shares the name — so
/// it reaches the create's `internetGateway` and every item of a describe's
/// `internetGatewaySet` alike, the rule being the protocol's rather than one handler's.
///
/// The element is empty because the STORE holds no association for any gateway, not because
/// the element is a constant: the day an operation that associates one is implemented, the
/// association it writes belongs on the gateway's record in
/// [`crate::state::ComAmazonawsEc2Amazonec2State::internet_gateways`], and this writer reads
/// it from there the way [`write_subnet_recorded_members`] reads what its projection
/// resolved.
fn write_internet_gateway_recorded_members(
    s: &mut dyn ShapeSerializer,
    written: &WrittenMembers,
    _zone: &EnclosingRecord,
) -> Result<(), SerdeError> {
    if !written.internet_gateway_id {
        return Ok(());
    }
    s.write_struct(&INTERNET_GATEWAY_ASSOCIATION, &NoAssociation)
}

/// One structure, serialized as its modelled members followed by the recorded elements its
/// shape renders on the live wire.
struct WithRecordedMembers<'a> {
    /// The structure as the generated serializer writes it.
    inner: &'a dyn SerializableStruct,
    /// How this shape's recorded elements are written.
    writer: RecordedWriter,
    /// The zone of the record THIS structure was written under, for a shape whose elements
    /// are read off it.
    zone: EnclosingRecord,
}

impl SerializableStruct for WithRecordedMembers<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        let mut written = WrittenMembers::default();
        {
            // The modelled members first, so the recorded elements sit after them and the
            // order the model gave this shape is untouched. The walk notes what it wrote,
            // which is what the recorded elements are derived from.
            let mut walk = RecordedMemberSerializer {
                inner: s,
                written: Some(&mut written),
                zone: self.zone.clone(),
            };
            self.inner.serialize_members(&mut walk)?;
        }
        (self.writer)(s, &written, &self.zone)
    }
}

/// The serializer a wrapped output is walked with: every call is forwarded to the codec's own
/// serializer unchanged, except a STRUCTURE, which is re-entered so the rule reaches every
/// depth and so a shape with recorded elements gets them.
struct RecordedMemberSerializer<'a> {
    /// The codec's own serializer, or the next wrapper out.
    inner: &'a mut dyn ShapeSerializer,
    /// The members of the structure being walked, when this walk is collecting them for that
    /// structure's recorded elements. `None` while walking anything else, so a nested
    /// structure's members are never read as the enclosing one's.
    written: Option<&'a mut WrittenMembers>,
    /// The zone handed DOWN to the structures written from here: the one the structure being
    /// walked has reported so far, or the one it was itself written under.
    zone: EnclosingRecord,
}

impl RecordedMemberSerializer<'_> {
    /// The context the structures written from here sit in: what the structure being walked
    /// reported for ITSELF, and otherwise what it was written under — so a volume hands
    /// its own zone to its attachments, an instance type hands down whether it is bare
    /// metal, and a structure that reports neither passes on what it was given rather than
    /// hiding it.
    fn nested_zone(&self) -> EnclosingRecord {
        match self.written.as_deref() {
            Some(written) => written.zone.inherited_from(&self.zone),
            None => self.zone.clone(),
        }
    }
}

impl ShapeSerializer for RecordedMemberSerializer<'_> {
    fn write_struct(
        &mut self,
        schema: &Schema,
        value: &dyn SerializableStruct,
    ) -> Result<(), SerdeError> {
        let zone = self.nested_zone();
        match recorded_writer_for(schema) {
            Some(writer) => self.inner.write_struct(
                schema,
                &WithRecordedMembers {
                    inner: value,
                    writer,
                    zone,
                },
            ),
            None => self
                .inner
                .write_struct(schema, &RecordedMembersUnder(value, zone)),
        }
    }

    fn write_list(
        &mut self,
        schema: &Schema,
        write_elements: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let zone = self.nested_zone();
        self.inner.write_list(schema, &|inner| {
            write_elements(&mut RecordedMemberSerializer {
                inner,
                written: None,
                zone: zone.clone(),
            })
        })
    }

    fn write_map(
        &mut self,
        schema: &Schema,
        write_entries: &dyn Fn(&mut dyn ShapeSerializer) -> Result<(), SerdeError>,
    ) -> Result<(), SerdeError> {
        let zone = self.nested_zone();
        self.inner.write_map(schema, &|inner| {
            write_entries(&mut RecordedMemberSerializer {
                inner,
                written: None,
                zone: zone.clone(),
            })
        })
    }

    fn write_boolean(&mut self, schema: &Schema, value: bool) -> Result<(), SerdeError> {
        if let Some(written) = self.written.as_deref_mut() {
            if schema.shape_id().namespace() == EC2_NAMESPACE {
                match schema.shape_id().shape_name() {
                    BURSTABLE_PERFORMANCE_MEMBER => {
                        written.burstable_performance_supported = Some(value);
                    }
                    BARE_METAL_MEMBER => written.zone.bare_metal = Some(value),
                    _ => {}
                }
            }
        }
        self.inner.write_boolean(schema, value)
    }

    fn write_byte(&mut self, schema: &Schema, value: i8) -> Result<(), SerdeError> {
        self.inner.write_byte(schema, value)
    }

    fn write_short(&mut self, schema: &Schema, value: i16) -> Result<(), SerdeError> {
        self.inner.write_short(schema, value)
    }

    fn write_integer(&mut self, schema: &Schema, value: i32) -> Result<(), SerdeError> {
        self.inner.write_integer(schema, value)
    }

    fn write_long(&mut self, schema: &Schema, value: i64) -> Result<(), SerdeError> {
        self.inner.write_long(schema, value)
    }

    fn write_float(&mut self, schema: &Schema, value: f32) -> Result<(), SerdeError> {
        self.inner.write_float(schema, value)
    }

    fn write_double(&mut self, schema: &Schema, value: f64) -> Result<(), SerdeError> {
        self.inner.write_double(schema, value)
    }

    fn write_big_integer(&mut self, schema: &Schema, value: &BigInteger) -> Result<(), SerdeError> {
        self.inner.write_big_integer(schema, value)
    }

    fn write_big_decimal(&mut self, schema: &Schema, value: &BigDecimal) -> Result<(), SerdeError> {
        self.inner.write_big_decimal(schema, value)
    }

    /// The leaves this notes as it forwards: the DHCP option set id a VPC reports, which is
    /// what [`write_vpc_recorded_members`] names the scope of, the id a Verified Access
    /// instance reports, which is what identifies it as one, the id an internet gateway
    /// reports, which identifies it the same way, the volume id that identifies a
    /// volume attachment, the zone a record reports for itself, which is the zone the
    /// records nested under it are in
    /// ([`write_volume_attachment_recorded_members`]), the instance type name and
    /// adapter support a reported instance type and its network settings carry, and the
    /// allocation id that makes a reported association an Elastic IP address's
    /// ([`write_network_interface_association_recorded_members`]).
    fn write_string(&mut self, schema: &Schema, value: &str) -> Result<(), SerdeError> {
        if let Some(written) = self.written.as_deref_mut() {
            if schema.shape_id().namespace() == EC2_NAMESPACE {
                match schema.shape_id().shape_name() {
                    DHCP_OPTIONS_ID_MEMBER => written.dhcp_options_id = true,
                    SUBNET_ID_MEMBER => written.subnet_id = Some(value.to_string()),
                    VERIFIED_ACCESS_INSTANCE_ID_MEMBER => {
                        written.verified_access_instance_id = true;
                    }
                    INTERNET_GATEWAY_ID_MEMBER => written.internet_gateway_id = true,
                    VOLUME_ID_MEMBER => written.volume_id = true,
                    HTTP_TOKENS_MEMBER => written.http_tokens = true,
                    AVAILABILITY_ZONE_MEMBER => {
                        written.zone.availability_zone = Some(value.to_string());
                    }
                    AVAILABILITY_ZONE_ID_MEMBER => {
                        written.zone.availability_zone_id = Some(value.to_string());
                    }
                    INSTANCE_TYPE_MEMBER => {
                        written.instance_type = true;
                        written.zone.instance_type = Some(value.to_string());
                    }
                    ENA_SUPPORT_MEMBER => written.ena_support = Some(value.to_string()),
                    NETWORK_INTERFACE_ID_MEMBER => {
                        written.network_interface_id = Some(value.to_string());
                    }
                    PUBLIC_DNS_NAME_MEMBER => written.public_dns_name = Some(value.to_string()),
                    ALLOCATION_ID_MEMBER => written.allocation_id = true,
                    _ => {}
                }
            }
        }
        self.inner.write_string(schema, value)
    }

    fn write_blob(&mut self, schema: &Schema, value: &[u8]) -> Result<(), SerdeError> {
        self.inner.write_blob(schema, value)
    }

    fn write_timestamp(&mut self, schema: &Schema, value: &DateTime) -> Result<(), SerdeError> {
        self.inner.write_timestamp(schema, value)
    }

    fn write_document(&mut self, schema: &Schema, value: &Document) -> Result<(), SerdeError> {
        self.inner.write_document(schema, value)
    }

    fn write_null(&mut self, schema: &Schema) -> Result<(), SerdeError> {
        self.inner.write_null(schema)
    }
}
