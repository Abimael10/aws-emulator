// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-directconnect-overtureservice service.
//!
//! The connection + private virtual interface + Direct Connect gateway slice of
//! Direct Connect lives here: a
//! handler mints a record, stores it, and every later response is rendered FROM the
//! stored record (see the [`connection_output!`] / [`virtual_interface_output!`]
//! macros), so a create followed by a describe answers the same bytes.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::ops::types::{BGPPeer, RouteFilterPrefix, Tag};

/// The gateways the ACCOUNT holds, asked of the service that owns them.
pub mod ec2_network;
/// The associations between a Direct Connect gateway and those gateways.
pub mod gateway_associations;

/// The Region a request names in its SigV4 credential scope when it carries no
/// `Authorization` header at all. Matches the Region the emulator's own generated
/// shared config names (`emulator_server`'s `[default] region = us-east-1`).
pub const DEFAULT_REGION: &str = "us-east-1";

/// How long after the call that starts it a state change this service owes the
/// clock falls due.
///
/// CHOSEN, not documented: AWS takes minutes to tear a connection or a virtual
/// interface down, which no test can wait for. It is deliberately non-zero so a
/// caller that reads straight back still observes the transient `deleting` /
/// `pending` state the model documents, and the instant is stored rather than
/// slept on, so no handler blocks.
pub const TRANSITION_DELAY: Duration = Duration::from_millis(500);

/// The MTUs `NewPrivateVirtualInterface$mtu` documents: "The supported values are
/// 1500 and 8500. The default value is 1500."
pub const SUPPORTED_VIRTUAL_INTERFACE_MTUS: &[i32] = &[1500, 8500];

/// The default MTU `NewPrivateVirtualInterface$mtu` documents.
pub const DEFAULT_VIRTUAL_INTERFACE_MTU: i32 = 1500;

/// The largest `asn` `NewPrivateVirtualInterface$asn` documents ("The valid values
/// are 1-2147483646").
pub const MAX_ASN: i64 = 2_147_483_646;

/// The largest `asnLong` `NewPrivateVirtualInterface$asnLong` documents ("The valid
/// range is from 1 to 4294967294").
pub const MAX_ASN_LONG: i64 = 4_294_967_294;

/// The largest value the legacy 2/4-byte `asn` member can carry;
/// `NewPrivateVirtualInterface$asnLong` documents that a larger `asnLong` is
/// reported back as `0` in `asn` "since 4-byte ASN values exceed the maximum
/// supported value of 2,147,483,647".
pub const MAX_LEGACY_ASN: i64 = 2_147_483_647;

/// The `amazonSideAsn` a `CreateDirectConnectGateway` request that names none
/// gets: `CreateDirectConnectGatewayRequest$amazonSideAsn` documents "The default
/// is 64512."
pub const DEFAULT_AMAZON_SIDE_ASN: i64 = 64_512;

/// The ASN ranges `CreateDirectConnectGatewayRequest$amazonSideAsn` documents:
/// "The ASN must be in the private range of 64,512 to 65,534 or 4,200,000,000 to
/// 4,294,967,294." Each entry is an inclusive `(low, high)` pair.
pub const PRIVATE_AMAZON_SIDE_ASN_RANGES: [(i64, i64); 2] =
    [(64_512, 65_534), (4_200_000_000, 4_294_967_294)];

/// Whether `asn` lies in one of the private ranges
/// [`PRIVATE_AMAZON_SIDE_ASN_RANGES`] holds.
pub fn amazon_side_asn_is_private(asn: i64) -> bool {
    PRIVATE_AMAZON_SIDE_ASN_RANGES
        .iter()
        .any(|(low, high)| (*low..=*high).contains(&asn))
}

/// How this service STATES a refusal of a listing of associations that selects by nothing.
///
/// `DescribeDirectConnectGatewayAssociations` publishes its listing in forms that each name at
/// least one of the Direct Connect gateway, the gateway associated with it, or the association
/// itself ("You must specify one of the following: A Direct Connect gateway … A virtual private
/// gateway … A transit gateway …"), so a request carrying none of those members asks for no
/// listing at all. FIXED text: nothing of the request varies it. Stated here beside this
/// service's other refusal wording.
pub const ASSOCIATION_SELECTOR_REQUIRED_MESSAGE: &str =
    "Either Direct Connect Gateway ID or Associated Gateway ID or Association ID must be set";

/// How this service STATES a refusal of an Amazon-side ASN outside the ranges
/// [`PRIVATE_AMAZON_SIDE_ASN_RANGES`] holds.
///
/// `amazonSideAsn` carries no `@range` in the model
/// (`smithy_query(query="operation",operation="CreateDirectConnectGateway")`), so the
/// bound is the documented one and the sentence reporting it is this service's own: the
/// VALUE the request submitted, then fixed text. The member is not named and the ranges
/// are not restated — a client branches on this sentence to tell a bad ASN from any
/// other client fault, since every one of them shares the one error shape and the one
/// 400 status. Stated here once, so every caller that refuses such a value words it
/// alike.
pub fn amazon_side_asn_not_valid_message(asn: i64) -> String {
    format!("{asn} is not a valid value for Amazon Side ASN")
}

/// The largest page a `maxResults` of `MaxResultSetSize` can ask for: "If
/// MaxResults is given a value larger than 100, only 100 results are returned."
pub const MAX_RESULT_SET_SIZE: i32 = 100;

/// The longest `virtualInterfaceName`
/// `NewPrivateVirtualInterface$virtualInterfaceName` documents: "The name has a
/// maximum of 100 characters."
pub const MAX_VIRTUAL_INTERFACE_NAME_LEN: usize = 100;

/// Whether `name` satisfies what
/// `NewPrivateVirtualInterface$virtualInterfaceName` documents: at most
/// [`MAX_VIRTUAL_INTERFACE_NAME_LEN`] characters, and "the following are valid
/// characters: a-z, 0-9 and a hyphen (-)".
pub fn virtual_interface_name_is_valid(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_VIRTUAL_INTERFACE_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The dedicated port speeds a Direct Connect location can offer, in the `PortSpeed`
/// form `Location$availablePortSpeeds` reports them in.
///
/// Source: `CreateLagRequest$connectionsBandwidth` — "The bandwidth of the individual
/// physical dedicated connections bundled by the LAG. The possible values are 1Gbps,
/// 10Gbps, 100Gbps, and 400Gbps." A `PortSpeed` names the same speeds in the short
/// form (`1Gbps` -> `1G`).
///
/// This is the set a speed must come FROM, never the list a location publishes: which
/// of these a location offers is that location's own data
/// ([`CatalogLocationRow::port_speeds`]), because AWS offers 400G — and even 100G — at
/// some facilities only.
///
/// ORDER: `PortSpeed` is a plain string and neither the model nor the operation's
/// docs fix an order, so the order these are listed in here is the one the recorded
/// `DescribeLocations` response publishes — `100G`, `1G`, `10G`, `400G`, which is
/// neither ascending by bandwidth nor lexicographic. Fixed in this ONE place so a
/// location's `availablePortSpeeds`, its `availableMacSecPortSpeeds` and its billing
/// mode's port speeds are always in the same order.
pub const DEDICATED_PORT_SPEEDS: [&str; 4] = ["100G", "1G", "10G", "400G"];

/// `known` narrowed to the members `present` names, in `known`'s OWN order.
///
/// The one narrowing every catalog-wide list is published through
/// ([`catalog_port_speeds`], [`published_providers`]): the order comes from the
/// catalog-wide list rather than from the per-location one, a member repeated in
/// `present` is published once, and a name `known` does not carry is dropped rather
/// than published.
fn narrowed_in_order(known: &[&str], present: &[&str]) -> Vec<String> {
    known
        .iter()
        .filter(|member| present.contains(*member))
        .map(|member| (*member).to_string())
        .collect()
}

/// `available` narrowed to the speeds a dedicated connection can be ordered at, in
/// [`DEDICATED_PORT_SPEEDS`] order.
///
/// Derived here rather than trusted off a catalog row, so every location publishes
/// its speeds in that one order, without repeats, and a speed
/// [`DEDICATED_PORT_SPEEDS`] does not name is dropped rather than published.
pub fn catalog_port_speeds(available: &[&str]) -> Vec<String> {
    narrowed_in_order(&DEDICATED_PORT_SPEEDS, available)
}

/// The port speeds MAC Security (MACsec) is offered on, at a location that offers
/// MACsec at all.
///
/// Source: `CreateConnectionRequest$requestMACSec` ("MAC Security (MACsec) is
/// unavailable on hosted connections … see MAC Security in Direct Connect"), which
/// offers MACsec on the dedicated 10 Gbps, 100 Gbps and 400 Gbps ports — the
/// sub-10G ports carry none. `availableMacSecPortSpeeds` is therefore never an
/// independent list: the recorded response shows it is the location's own
/// `availablePortSpeeds` narrowed to these, which is why it is computed by
/// [`mac_sec_port_speeds`] rather than stored per location.
pub const MAC_SEC_PORT_SPEEDS: [&str; 3] = ["10G", "100G", "400G"];

/// The usage-based billing mode EVERY Direct Connect location publishes, one of the
/// values `BillingMode` accepts on the wire
/// ([`crate::ops::types::BILLING_MODE_VALUES`]).
///
/// It is published on every port speed the location offers and with no
/// `includedRegions`: usage-based data transfer is metered per gigabyte wherever it
/// leaves, so the mode includes no Region's traffic in a fixed fee.
pub const USAGE_BILLING_MODE: &str = "PayAsYouGo";

/// The port speeds a flat-rate billing mode is offered on, in the order a recorded
/// `DescribeLocations` response prints them inside such a mode (ascending by
/// bandwidth, which is NOT [`DEDICATED_PORT_SPEEDS`]' order — the recorded element
/// carries `["100G","1G","10G"]` on its usage-based mode and `["10G","100G"]` on each
/// of its flat-rate modes).
///
/// Flat-rate ports are the high-capacity DEDICATED ports: a location publishes a
/// flat-rate mode on its own `availablePortSpeeds` narrowed to these
/// ([`flat_rate_port_speeds`]), so a facility that lands nothing faster than 1G
/// publishes no flat-rate mode at all.
pub const FLAT_RATE_PORT_SPEEDS: [&str; 2] = ["10G", "100G"];

/// The highest flat-rate tier a location can publish: `BillingMode`
/// (via `smithy_query(query="shape",shape="BillingMode")`) names `FlatRateTier1`
/// through `FlatRateTier5`, so a row offering more tiers than that publishes five.
pub const MAX_FLAT_RATE_TIERS: usize = 5;

/// The `billingMode` of flat-rate tier `tier`, as `BillingMode` spells it
/// (`FlatRateTier1` .. `FlatRateTier5`).
pub fn flat_rate_billing_mode(tier: usize) -> String {
    format!("FlatRateTier{tier}")
}

/// A location's flat-rate port speeds: its own `available` speeds narrowed to
/// [`FLAT_RATE_PORT_SPEEDS`], in that constant's order.
///
/// Derived from the location's own speeds rather than written per row, so a location
/// can never advertise a flat-rate mode on a port it does not land.
pub fn flat_rate_port_speeds(available: &[String]) -> Vec<String> {
    FLAT_RATE_PORT_SPEEDS
        .iter()
        .filter(|speed| available.iter().any(|offered| offered == *speed))
        .map(|speed| (*speed).to_string())
        .collect()
}

/// The geographic areas a flat-rate tier's `includedRegions` is drawn from, in the
/// order the catalog walks them outward from a location's own area.
///
/// `AvailableBillingMode$includedRegions` is "the Amazon Web Services Regions included
/// with the billing mode" (via `smithy_query(query="shape",shape="AvailableBillingMode")`),
/// i.e. the Regions whose data transfer that tier's flat fee covers; the model
/// constrains the list no further, so WHICH Regions a tier covers is catalog data. The
/// catalog groups the Regions a location can reach into areas and gives each
/// successive tier the next area out, which is what a recorded `us-west-2` element
/// shows: its `FlatRateTier1` includes `us-west-1` (the other Region of the location's
/// own area) and its `FlatRateTier2` the US East Regions.
const FLAT_RATE_REGION_AREAS: [&[&str]; 6] = [
    // US West
    &["us-west-1", "us-west-2"],
    // US East
    &["us-east-1", "us-east-2"],
    // Canada
    &["ca-central-1"],
    // Europe
    &["eu-west-1", "eu-west-2", "eu-central-1", "eu-north-1"],
    // Asia Pacific
    &[
        "ap-northeast-1",
        "ap-northeast-2",
        "ap-southeast-1",
        "ap-southeast-2",
        "ap-south-1",
    ],
    // South America
    &["sa-east-1"],
];

/// The areas a location in `region` reaches, nearest first: its own area without its
/// own Region, then every other area of [`FLAT_RATE_REGION_AREAS`] in that constant's
/// order, starting after its own and wrapping. An area that contributes no Region
/// (the location's own area when it holds only that Region) is left out, so tier
/// numbering never skips.
fn flat_rate_region_rings(region: &str) -> Vec<Vec<String>> {
    let own = FLAT_RATE_REGION_AREAS
        .iter()
        .position(|area| area.contains(&region));
    let area_regions = |index: usize, skip_own: bool| -> Vec<String> {
        FLAT_RATE_REGION_AREAS[index]
            .iter()
            .filter(|member| !skip_own || **member != region)
            .map(|member| (*member).to_string())
            .collect()
    };

    let mut rings: Vec<Vec<String>> = Vec::new();
    if let Some(own) = own {
        rings.push(area_regions(own, true));
    }
    let start = own.map_or(0, |own| own + 1);
    for offset in 0..FLAT_RATE_REGION_AREAS.len() {
        let index = (start + offset) % FLAT_RATE_REGION_AREAS.len();
        if Some(index) == own {
            continue;
        }
        rings.push(area_regions(index, false));
    }
    rings.retain(|ring| !ring.is_empty());
    rings
}

/// The `includedRegions` flat-rate tier `tier` (1-based) publishes at a location in
/// `region`: the `tier`-th ring of [`flat_rate_region_rings`], empty where the catalog
/// knows no ring that far out.
pub fn flat_rate_included_regions(region: &str, tier: usize) -> Vec<String> {
    if tier == 0 {
        return Vec::new();
    }
    flat_rate_region_rings(region)
        .into_iter()
        .nth(tier - 1)
        .unwrap_or_default()
}

/// The `availableBillingModes` a location publishes: the usage-based mode on every
/// port speed it offers, then one flat-rate mode per tier offered there, in tier
/// order.
///
/// Derived from the stored record rather than stored beside it, so a mode can name
/// neither a port speed the location does not land nor a tier beyond
/// [`MAX_FLAT_RATE_TIERS`]; a tier the catalog knows no Region ring for is not
/// published, because a flat-rate fee that includes no Region's transfer is not a
/// mode a client can order.
pub fn location_billing_modes(
    location: &LocationRecord,
) -> Vec<crate::ops::types::AvailableBillingMode> {
    let mut modes = vec![crate::ops::types::AvailableBillingMode {
        billing_mode: Some(USAGE_BILLING_MODE.to_string()),
        available_port_speeds: Some(location.available_port_speeds.clone()),
        included_regions: None,
    }];

    let flat_rate_speeds = flat_rate_port_speeds(&location.available_port_speeds);
    if flat_rate_speeds.is_empty() {
        return modes;
    }
    for tier in 1..=location.flat_rate_tiers.min(MAX_FLAT_RATE_TIERS) {
        let included = flat_rate_included_regions(&location.region, tier);
        if included.is_empty() {
            break;
        }
        modes.push(crate::ops::types::AvailableBillingMode {
            billing_mode: Some(flat_rate_billing_mode(tier)),
            available_port_speeds: Some(flat_rate_speeds.clone()),
            included_regions: Some(included),
        });
    }
    modes
}

/// The MACsec port speeds a location offering `available` port speeds publishes.
///
/// The location's own speeds narrowed to [`MAC_SEC_PORT_SPEEDS`], in the location's
/// order, so the two lists can never disagree; empty at a location that offers no
/// MACsec at all.
pub fn mac_sec_port_speeds(available: &[String], offers_mac_sec: bool) -> Vec<String> {
    if !offers_mac_sec {
        return Vec::new();
    }
    available
        .iter()
        .filter(|speed| MAC_SEC_PORT_SPEEDS.contains(&speed.as_str()))
        .cloned()
        .collect()
}

/// The number of records this emulator keeps in each caller-driven store.
///
/// An EMULATOR memory ceiling, not an AWS quota: every successful create inserts
/// a record holding caller-supplied strings, so without a ceiling one caller
/// could grow the process without bound. At the ceiling a create first reclaims
/// the records the clock has already torn down
/// ([`ComAmazonawsDirectconnectOvertureserviceState::reclaim_deleted`]) and, if
/// that frees nothing, is refused with a modelled error naming the ceiling.
pub const MAX_RECORDS_PER_STORE: usize = 1_000;

/// The number of tags AWS allows on one Direct Connect resource (the "Tags per
/// Direct Connect resource" service quota). A request carrying more is refused
/// with the modelled `TooManyTagsException`.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The first tag key that appears more than once in `tags`, which the modelled
/// `DuplicateTagKeysException` ("A tag key was specified more than once") refuses.
///
/// `BTreeSet` membership rather than a linear scan, so the cost is O(n log n) in
/// the caller-chosen tag count whatever order a caller's checks run in.
pub fn first_duplicate_tag_key(tags: &[Tag]) -> Option<&str> {
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for tag in tags {
        if !seen.insert(tag.key.as_str()) {
            return Some(tag.key.as_str());
        }
    }
    None
}

/// A state change the clock owes a resource: at [`DueTransition::at`] the resource
/// moves to [`DueTransition::to`].
///
/// A reporting operation materialises it through
/// [`ComAmazonawsDirectconnectOvertureserviceState::settle_due_transitions`] — the
/// change was owed at that instant whether or not anybody called.
#[derive(Debug, Clone)]
pub struct DueTransition {
    /// The instant the move falls due.
    pub at: Instant,
    /// The state the resource moves to.
    pub to: String,
}

/// A stored Direct Connect connection — the `Connection` shape's data, owned by
/// this service rather than rebuilt per response.
#[derive(Debug, Clone)]
pub struct Connection {
    pub connection_id: String,
    pub connection_name: String,
    pub connection_state: String,
    pub location: String,
    pub bandwidth: String,
    pub region: String,
    pub owner_account: String,
    /// `0` for a port: a dedicated connection carries no VLAN tag of its own, and
    /// `Connection$vlan` is a required member with a `default` of `0`.
    pub vlan: i32,
    pub lag_id: Option<String>,
    pub provider_name: Option<String>,
    pub billing_mode: Option<String>,
    pub mac_sec_capable: bool,
    pub jumbo_frame_capable: bool,
    pub has_logical_redundancy: String,
    pub aws_device: String,
    pub aws_device_v2: String,
    pub aws_logical_device_id: String,
    /// Set by a call to `DescribeLoa`/`DescribeConnectionLoa`, which
    /// `Connection$loaIssueTime` defines as "the time of the most recent call to
    /// DescribeLoa for this connection". Both are still scaffolded, so no stored
    /// connection has one yet.
    pub loa_issue_time: Option<::aws_smithy_types::DateTime>,
    pub tags: Vec<Tag>,
    pub due: Option<DueTransition>,
}

/// A stored Direct Connect gateway - the `DirectConnectGateway` shape's data.
///
/// A Direct Connect gateway "is global and visible in any Amazon Web Services
/// Region after it is created", so unlike a [`Connection`] it holds no Region of
/// its own.
#[derive(Debug, Clone)]
pub struct DirectConnectGateway {
    pub direct_connect_gateway_id: String,
    pub direct_connect_gateway_name: String,
    pub amazon_side_asn: i64,
    pub owner_account: String,
    pub direct_connect_gateway_state: String,
    /// `DirectConnectGateway$stateChangeError`: "the error message if the state
    /// of an object failed to advance". `None` while no advance has failed.
    pub state_change_error: Option<String>,
    pub tags: Vec<Tag>,
    pub due: Option<DueTransition>,
}

/// A stored private virtual interface — the `VirtualInterface` shape's data.
///
/// Membership of a connection is held ONCE, here, as [`Self::connection_id`]:
/// `DescribeVirtualInterfaces` filters on it rather than on a second list kept
/// beside the connection, so the two cannot disagree.
#[derive(Debug, Clone)]
pub struct VirtualInterface {
    pub virtual_interface_id: String,
    pub connection_id: String,
    pub owner_account: String,
    pub virtual_interface_name: String,
    pub virtual_interface_type: String,
    pub virtual_interface_state: String,
    pub vlan: i32,
    pub asn: i32,
    pub asn_long: Option<i64>,
    pub amazon_side_asn: Option<i64>,
    pub auth_key: Option<String>,
    pub amazon_address: Option<String>,
    pub customer_address: Option<String>,
    pub address_family: String,
    pub mtu: i32,
    pub jumbo_frame_capable: bool,
    pub virtual_gateway_id: Option<String>,
    pub direct_connect_gateway_id: Option<String>,
    pub route_filter_prefixes: Vec<RouteFilterPrefix>,
    pub bgp_peers: Vec<BGPPeer>,
    pub location: String,
    pub region: String,
    pub aws_device_v2: String,
    pub aws_logical_device_id: String,
    pub site_link_enabled: bool,
    pub tags: Vec<Tag>,
    pub due: Option<DueTransition>,
}

/// A Direct Connect location in this service's catalog — the `Location` shape's
/// data, owned by this service rather than rebuilt per response.
///
/// Facility data, not caller data: no operation creates or deletes a location, so
/// the catalog is seeded once (see [`built_in_location_catalog`]) and only read.
/// `availableMacSecPortSpeeds` and `availableBillingModes` are NOT stored: they are
/// derived from the fields below ([`mac_sec_port_speeds`],
/// [`ComAmazonawsDirectconnectOvertureserviceState::location_output`]), so a
/// location's port speeds and the speeds its MACsec and billing modes name cannot
/// drift apart.
#[derive(Debug, Clone)]
pub struct LocationRecord {
    /// `Location$locationCode`: "the code for the location" — the facility's code
    /// suffixed with the colocation partner's (`EqSe2-WBE`), which is the value a
    /// client then passes as `CreateConnection`'s `location`.
    pub location_code: String,
    /// `Location$locationName`: "the name of the location. This includes the name of
    /// the colocation partner and the physical site of the building" — so BOTH, the
    /// building and then the partner's site, joined by `" | "`.
    pub location_name: String,
    /// `Location$region`: "the Amazon Web Services Region for the location."
    pub region: String,
    /// `Location$availablePortSpeeds`: this location's own subset of
    /// [`DEDICATED_PORT_SPEEDS`], in [`catalog_port_speeds`] order.
    pub available_port_speeds: Vec<String>,
    /// `Location$availableProviders`: the service providers present at the location.
    pub available_providers: Vec<String>,
    /// Whether the location offers MAC Security at all; which of its port speeds
    /// carry it is [`mac_sec_port_speeds`].
    pub offers_mac_sec: bool,
    /// How many flat-rate billing tiers are offered at this location; `0` where only
    /// the usage-based mode is. Which Regions each tier includes and which port
    /// speeds it covers are derived ([`location_billing_modes`]).
    pub flat_rate_tiers: usize,
}

/// Per-service state for com-amazonaws-directconnect-overtureservice. A handler reaches it through [`service_state`].
#[derive(Debug)]
pub struct ComAmazonawsDirectconnectOvertureserviceState {
    /// Stored connections, keyed by `connectionId`.
    ///
    /// A `BTreeMap` rather than a `HashMap`: minted ids are zero-padded and
    /// monotonic, so key order IS creation order and `DescribeConnections` answers
    /// a stable list instead of a per-process-random one.
    pub connections: BTreeMap<String, Connection>,
    /// Stored virtual interfaces, keyed by `virtualInterfaceId`. Ordered for the
    /// same reason as [`Self::connections`].
    pub virtual_interfaces: BTreeMap<String, VirtualInterface>,
    /// Stored Direct Connect gateways, keyed by `directConnectGatewayId`.
    ///
    /// A gateway id is a UUID rather than a monotonic suffix, so key order is NOT
    /// creation order here: [`Self::gateway_order`] carries that, and
    /// `DescribeDirectConnectGateways` answers through it.
    pub gateways: BTreeMap<String, DirectConnectGateway>,
    /// The keys of [`Self::gateways`] in creation order.
    pub gateway_order: Vec<String>,
    /// Monotonic source of the `dxcon-`/`dxvif-` suffixes, so every minted id is
    /// unique and reproducible within a run.
    pub id_counter: u64,
    /// The Direct Connect location catalog, keyed by `locationCode`.
    ///
    /// Seeded from [`built_in_location_catalog`] when this store is created, so
    /// `DescribeLocations` is a pure read: no call creates, changes or removes a
    /// location. A `BTreeMap`, so the catalog is answered in a stable order
    /// (by location code) rather than a per-process-random one.
    pub locations: BTreeMap<String, LocationRecord>,
    /// Stored associations between a Direct Connect gateway and a virtual private
    /// gateway or transit gateway, keyed by `associationId`.
    ///
    /// An association id is a UUID rather than a monotonic suffix, so key order is
    /// NOT creation order here: [`Self::association_order`] carries that, and
    /// `DescribeDirectConnectGatewayAssociations` answers through it.
    pub gateway_associations: BTreeMap<String, gateway_associations::GatewayAssociation>,
    /// The keys of [`Self::gateway_associations`] in creation order.
    pub association_order: Vec<String>,
}

impl Default for ComAmazonawsDirectconnectOvertureserviceState {
    /// A fresh store: no caller-created records, and the location catalog already
    /// seeded. Seeded HERE rather than on first read so a reporting operation never
    /// has to write: every caller sees the same catalog from the first request on.
    fn default() -> Self {
        Self {
            connections: BTreeMap::new(),
            virtual_interfaces: BTreeMap::new(),
            gateways: BTreeMap::new(),
            gateway_order: Vec::new(),
            id_counter: 0,
            locations: built_in_location_catalog(),
            gateway_associations: BTreeMap::new(),
            association_order: Vec::new(),
        }
    }
}

impl ComAmazonawsDirectconnectOvertureserviceState {
    /// Mint the next id for `prefix` (`"dxcon"`, `"dxvif"`), in AWS's
    /// `<prefix>-xxxxxxxx` 8-character form.
    pub fn next_id(&mut self, prefix: &str) -> String {
        self.id_counter += 1;
        format!("{prefix}-{:08x}", self.id_counter)
    }

    /// Mint the next `directConnectGatewayId`.
    ///
    /// A Direct Connect gateway id is not one of the `dxcon-`/`dxvif-` prefixed
    /// ids: AWS hands out a version-4 UUID (`DirectConnectGatewayId` is an opaque
    /// string), so the minted id has that shape.
    pub fn next_gateway_id(&mut self) -> String {
        self.next_uuid()
    }

    /// Mint the next opaque version-4 UUID this service hands a resource out under.
    ///
    /// The low 12 hex digits ARE the monotonic counter, so every minted id is unique
    /// within a run, and the rest is derived from it, so it is reproducible. ONE
    /// minter, so a gateway id and an association id can never collide.
    pub fn next_uuid(&mut self) -> String {
        self.id_counter += 1;
        let seed = fnv1a64(&self.id_counter.to_be_bytes());
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            u32::try_from(seed >> 32).unwrap_or(0),
            (seed >> 16) & 0xffff,
            seed & 0x0fff,
            0x8000 | ((seed >> 4) & 0x3fff),
            self.id_counter & 0xffff_ffff_ffff,
        )
    }

    /// Render a stored [`DirectConnectGateway`] into the generated
    /// `DirectConnectGateway` output shape: the ONE rendering of a stored
    /// gateway, so every operation answers the same field set.
    ///
    /// A method rather than a free function because
    /// `DirectConnectGateway$totalPrefixPoolAllocations` ("the total number of
    /// inbound route prefixes allocated to the attachments on the Direct Connect
    /// gateway") is a property of the attachments, so it is counted from the
    /// stored virtual interfaces rather than kept as a second copy on the gateway.
    pub fn gateway_output(
        &self,
        gateway: &DirectConnectGateway,
    ) -> crate::ops::types::DirectConnectGateway {
        crate::ops::types::DirectConnectGateway {
            direct_connect_gateway_id: Some(gateway.direct_connect_gateway_id.clone()),
            direct_connect_gateway_name: Some(gateway.direct_connect_gateway_name.clone()),
            amazon_side_asn: Some(gateway.amazon_side_asn),
            owner_account: Some(gateway.owner_account.clone()),
            direct_connect_gateway_state: Some(gateway.direct_connect_gateway_state.clone()),
            state_change_error: gateway.state_change_error.clone(),
            total_prefix_pool_allocations: Some(
                self.gateway_prefix_allocations(&gateway.direct_connect_gateway_id),
            ),
            tags: (!gateway.tags.is_empty()).then(|| gateway.tags.clone()),
        }
    }

    /// The inbound route prefixes allocated to the attachments on `gateway_id`,
    /// combining both address families.
    fn gateway_prefix_allocations(&self, gateway_id: &str) -> i32 {
        let allocated: usize = self
            .virtual_interfaces
            .values()
            .filter(|interface| {
                interface.direct_connect_gateway_id.as_deref() == Some(gateway_id)
                    && interface.virtual_interface_state != "deleted"
            })
            .map(|interface| interface.route_filter_prefixes.len())
            .sum();
        i32::try_from(allocated).unwrap_or(i32::MAX)
    }

    /// The stored gateways in creation order, newest last.
    pub fn gateways_in_order(&self) -> Vec<&DirectConnectGateway> {
        self.gateway_order
            .iter()
            .filter_map(|id| self.gateways.get(id))
            .collect()
    }

    /// Apply every state change the clock has already made due.
    ///
    /// This is the ONE write a reporting operation is allowed: each move was owed
    /// at [`DueTransition::at`] whether or not anybody called, so materialising it
    /// on read cannot change what a later caller sees.
    pub fn settle_due_transitions(&mut self) {
        let now = Instant::now();
        for connection in self.connections.values_mut() {
            if let Some(due) = &connection.due {
                if now >= due.at {
                    connection.connection_state = due.to.clone();
                    connection.due = None;
                }
            }
        }
        for interface in self.virtual_interfaces.values_mut() {
            if let Some(due) = &interface.due {
                if now >= due.at {
                    interface.virtual_interface_state = due.to.clone();
                    interface.due = None;
                }
            }
        }
        for gateway in self.gateways.values_mut() {
            if let Some(due) = &gateway.due {
                if now >= due.at {
                    gateway.direct_connect_gateway_state = due.to.clone();
                    gateway.due = None;
                }
            }
        }
        self.settle_due_associations();
    }

    /// Reclaim the records the clock has already torn down.
    ///
    /// A resource in the terminal `deleted` state can no longer forward traffic
    /// and no call can bring it back, so dropping it frees memory without
    /// changing what any later caller can do. Called only when a store is at
    /// [`MAX_RECORDS_PER_STORE`], so a `deleted` record stays readable until the
    /// store actually needs the room.
    pub fn reclaim_deleted(&mut self) {
        self.connections
            .retain(|_, connection| connection.connection_state != "deleted");
        self.virtual_interfaces
            .retain(|_, interface| interface.virtual_interface_state != "deleted");
        self.gateways
            .retain(|_, gateway| gateway.direct_connect_gateway_state != "deleted");
        self.gateway_order
            .retain(|id| self.gateways.contains_key(id));
    }

    /// Whether `len` leaves room for one more record under
    /// [`MAX_RECORDS_PER_STORE`].
    pub fn has_room(len: usize) -> bool {
        len < MAX_RECORDS_PER_STORE
    }

    /// The virtual interfaces attached to `connection_id`, in creation order.
    pub fn interfaces_of(&self, connection_id: &str) -> Vec<&VirtualInterface> {
        self.virtual_interfaces
            .values()
            .filter(|interface| interface.connection_id == connection_id)
            .collect()
    }

    /// The catalog locations in `region`, in location-code order.
    ///
    /// `DescribeLocations` "lists the Direct Connect locations in the current Amazon
    /// Web Services Region", and `Location$region` is "the Amazon Web Services Region
    /// for the location", so the Region the request is signed for is what selects
    /// them. A Region the catalog holds no location in selects none, which is an
    /// empty list, not an error.
    pub fn locations_in_region(&self, region: &str) -> Vec<&LocationRecord> {
        self.locations
            .values()
            .filter(|location| location.region == region)
            .collect()
    }

    /// Render a stored [`LocationRecord`] into the generated `Location` output shape:
    /// the ONE rendering of a catalog location, so every operation answers the same
    /// field set.
    ///
    /// A method rather than a free function so a renderer that has to reach the rest
    /// of the catalog keeps ONE call site; `availableMacSecPortSpeeds`
    /// ([`mac_sec_port_speeds`]) and `availableBillingModes`
    /// ([`location_billing_modes`]) are derived from the location's own stored speeds,
    /// its Region and its tier count, so no second copy of them can drift.
    pub fn location_output(&self, location: &LocationRecord) -> crate::ops::types::Location {
        // The usage-based mode on the location's own speeds, then the flat-rate tiers
        // offered there with the Regions each one includes.
        let billing_modes = location_billing_modes(location);

        crate::ops::types::Location {
            location_code: Some(location.location_code.clone()),
            location_name: Some(location.location_name.clone()),
            region: Some(location.region.clone()),
            available_port_speeds: Some(location.available_port_speeds.clone()),
            available_providers: Some(location.available_providers.clone()),
            available_mac_sec_port_speeds: Some(mac_sec_port_speeds(
                &location.available_port_speeds,
                location.offers_mac_sec,
            )),
            available_billing_modes: Some(billing_modes),
        }
    }
}

/// Which Direct Connect delivery partners deliver at one place: MEMBERSHIP only,
/// because the order a location publishes them in is not a property of the place.
///
/// A list PER LOCATION rather than one catalog-wide list: `availableProviders` is
/// "the name of the service provider for the location", and which partners deliver
/// there is a property of that demarcation — a carrier hotel's meet-me room reaches
/// nearly every partner, a single-tenant data centre a handful. The ORDER comes from
/// [`DIRECT_CONNECT_PROVIDERS`] through [`published_providers`], so no location can
/// publish two partners in an order another location contradicts.
type ProviderPresence = &'static [&'static str];

/// The Direct Connect delivery partners this catalog draws `availableProviders` from,
/// in the order AWS publishes them.
///
/// Two things the roster fixes, both of them observable in `availableProviders`:
///
/// * the SPELLING. The names are the trade forms AWS prints — `Colt`, `GTT`,
///   `Comcast`, `Console Connect`, `Equinix, Inc.` — not the expanded company names
///   (`Colt Technology Services`, `GTT Communications`, `Comcast Business`,
///   `Console Connect by PCCW Global`), because the string a client reads here is the
///   one it carries forward as `CreateConnection$providerName`.
/// * the ORDER. A partner list comes back in the catalog's own order, which is
///   neither alphabetical nor lexicographic, so the roster is written in that order
///   and every location's list is rendered by narrowing it ([`published_providers`])
///   rather than by sorting.
///
/// Source: the recorded `DescribeLocations` responses for the two carrier-hotel
/// demarcations in `us-west-2`, which both print exactly these thirty names in exactly
/// this order — that is where the roster's spellings and its order come from. A name
/// no recorded location publishes is NOT carried here: a location may only publish a
/// partner the catalog records as delivering somewhere, so an unrecorded delivery
/// partner would have every location that draws the whole roster publish it.
/// `Location$availableProviders` ("the name of the service provider for the
/// location", `ProviderList`, via `smithy_query(query="shape",shape="Location")`)
/// constrains neither the names nor their order, so both are catalog data.
pub const DIRECT_CONNECT_PROVIDERS: [&str; 30] = [
    "Pureport",
    "Beanfield Metroconnect",
    "ePLDT",
    "Datapipe",
    "Telstra",
    "Colt",
    "Verizon",
    "Console Connect",
    "Internet2",
    "Transtelco",
    "Megaport",
    "GTT",
    "Layer42",
    "Allstream",
    "Equinix, Inc.",
    "Zayo Group",
    "Masergy",
    "CenturyLink",
    "AT&T",
    "IX Reach",
    "Comcast",
    "Sprint",
    "XO Communications",
    "HopOne",
    "RCN",
    "PacketFabric",
    "Telus",
    "Arelion",
    "China Mobile International",
    "Epsilon",
];

/// A location's `availableProviders`: [`DIRECT_CONNECT_PROVIDERS`] narrowed to the
/// partners `present` where it is published, in the roster's order.
///
/// Narrowing the roster rather than copying `present` is what gives every location ONE
/// published order (the catalog's, not alphabetical) and drops any name that is not a
/// delivery partner of this catalog, so a location can neither publish a provider the
/// catalog does not know nor publish two partners in an order another location
/// contradicts. A name repeated in `present` is published once, because a partner
/// delivers at a place or does not.
pub fn published_providers(present: &[&str]) -> Vec<String> {
    narrowed_in_order(&DIRECT_CONNECT_PROVIDERS, present)
}

/// What the Seattle carrier hotel's demarcations publish: every partner on the roster,
/// because a cross connect in that building reaches all of them.
///
/// Source: the recorded `DescribeLocations` responses for its two demarcations in
/// `us-west-2`, which print the whole roster — the same thirty names in the same order
/// — for both the colocation partner's cage and the building's own meet-me room.
const WESTIN_BUILDING_PROVIDERS: ProviderPresence = &DIRECT_CONNECT_PROVIDERS;

/// One row of the catalog: the demarcation ONE colocation partner offers at a
/// facility, which is what AWS publishes one `Location` for.
///
/// AWS does not publish one location per building: it publishes one per
/// (facility, partner) pair, because the cross connect a customer orders is to that
/// partner's patch panel, and the ports AWS lands there differ by partner — which is
/// why [`Self::port_speeds`] and [`Self::offers_mac_sec`] sit on the row rather than
/// on the facility.
struct CatalogLocationRow {
    /// The partner's code, which suffixes the facility's code to make
    /// `locationCode`.
    partner_code: &'static str,
    /// The partner's own PHYSICAL SITE inside the building — its cage, suite or
    /// meet-me room designation — which is the second half of `locationName`.
    ///
    /// Never a bare repeat of [`CatalogFacility::building`]: `Location$locationName`
    /// "includes the name of the colocation partner AND the physical site of the
    /// building", so the second half has to say WHERE in the building the cross
    /// connect lands (`Westin Building Exchange - SEA10 - Main FMMR, Seattle, WA`,
    /// not `Westin Building Exchange, Seattle, WA` again), which is also what makes
    /// one facility's demarcations distinguishable by name.
    partner_site: &'static str,
    /// The port speeds AWS offers on this demarcation — a subset of
    /// [`DEDICATED_PORT_SPEEDS`], not all of it.
    port_speeds: &'static [&'static str],
    /// Whether MAC Security is offered here at all; which of [`Self::port_speeds`]
    /// carry it is [`mac_sec_port_speeds`].
    offers_mac_sec: bool,
    /// How many flat-rate billing tiers AWS offers at this demarcation, `0` where it
    /// offers the usage-based mode alone.
    ///
    /// On the row rather than on the facility because a flat-rate port is a property
    /// of the ports AWS lands at that partner's panel; the speeds and the
    /// `includedRegions` each tier publishes are derived
    /// ([`location_billing_modes`]).
    flat_rate_tiers: usize,
}

/// One colocation facility, and every partner demarcation AWS publishes a location
/// for at it.
struct CatalogFacility {
    /// The facility's code, which every `locationCode` at it begins with.
    code: &'static str,
    /// The building's name and city, which is the first half of `locationName`.
    building: &'static str,
    /// The Region whose `DescribeLocations` answers these locations.
    region: &'static str,
    /// The delivery partners with presence at this facility, which every demarcation
    /// at it publishes. Published in [`DIRECT_CONNECT_PROVIDERS`] order by
    /// [`published_providers`].
    ///
    /// Per FACILITY, not per demarcation: the recorded response prints the same
    /// partner list for both demarcations of the one building it records, because a
    /// cross connect inside a building reaches the partners present in it.
    providers: ProviderPresence,
    /// The partner demarcations at this facility.
    locations: &'static [CatalogLocationRow],
}

/// The colocation facilities this emulator publishes Direct Connect locations at.
///
/// FACILITY DATA, not caller data and not account data: `DescribeLocations` takes no
/// input and no caller identity narrows it, so what it answers is the public facility
/// list for the signed Region. The buildings, partner codes and partner sites below
/// are the Direct Connect facilities AWS publishes, as the recorded
/// `DescribeLocations` response for `us-west-2` spells them — a per-partner
/// `locationCode` (`EqSe2-EQ`, `EqSe2-WBE`) and a `locationName` that names the
/// building and then the partner's site.
///
/// Each facility's [`CatalogFacility::providers`] names the delivery partners that
/// deliver there, from the roster [`DIRECT_CONNECT_PROVIDERS`] holds: a carrier hotel
/// reaches nearly the whole roster, a metro or single-operator site the handful of
/// carriers present in it.
///
/// WHICH of these lists is recorded: the Seattle carrier hotel's
/// ([`WESTIN_BUILDING_PROVIDERS`]) and that alone. Every other location's partner
/// membership is CATALOGUED — written from the public facility and delivery-partner
/// lists under the rule above — so a later fixer holding a recording for one of these
/// facilities should refine that row's membership rather than read it as a recorded
/// fact. The spellings and the published ORDER are recorded for all of them, because
/// both come from the one roster. The same holds for
/// [`CatalogLocationRow::flat_rate_tiers`]: that the Seattle demarcations offer none
/// is recorded, and every other row's tier count is catalogued.
///
/// What is derived rather than written down: a row's `locationCode`, its
/// `locationName`, the order of its port speeds ([`catalog_port_speeds`]), its MACsec
/// speeds ([`mac_sec_port_speeds`]), its billing modes with their port speeds and
/// `includedRegions` ([`location_billing_modes`]) and the order of its providers
/// ([`published_providers`]) — so no two of those can disagree with a row's own
/// speeds, and no facility publishes partners in an order another contradicts.
///
/// The rows deliberately span the class: facilities with one partner and with
/// several, port-speed sets with and without 100G and 400G, demarcations with and
/// without MACsec, carrier hotels reaching the whole partner roster and metro sites
/// reaching a handful, demarcations with no flat-rate tier and with several, in
/// several Regions.
const BUILT_IN_FACILITIES: &[CatalogFacility] = &[
    CatalogFacility {
        code: "EqDC2",
        building: "Equinix DC2, Ashburn, VA",
        region: "us-east-1",
        providers: &[
            "Pureport",
            "Datapipe",
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Internet2",
            "Megaport",
            "GTT",
            "Allstream",
            "Equinix, Inc.",
            "Zayo Group",
            "Masergy",
            "CenturyLink",
            "AT&T",
            "IX Reach",
            "Comcast",
            "Sprint",
            "XO Communications",
            "HopOne",
            "RCN",
            "PacketFabric",
            "Arelion",
            "China Mobile International",
            "Epsilon",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, DC2, Ashburn, VA",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 2,
            },
            CatalogLocationRow {
                partner_code: "DRT",
                partner_site: "Digital Realty, IAD38, Ashburn, VA",
                port_speeds: &["1G", "10G"],
                offers_mac_sec: false,
                flat_rate_tiers: 0,
            },
        ],
    },
    CatalogFacility {
        code: "CSDC1",
        building: "CoreSite DC1, Reston, VA",
        region: "us-east-1",
        providers: &[
            "Internet2",
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "PacketFabric",
            "Arelion",
        ],
        locations: &[CatalogLocationRow {
            partner_code: "CS",
            partner_site: "CoreSite, DC1, Reston, VA",
            port_speeds: &["1G", "10G"],
            offers_mac_sec: false,
            flat_rate_tiers: 1,
        }],
    },
    CatalogFacility {
        code: "EqCh2",
        building: "Equinix CH2, Chicago, IL",
        region: "us-east-2",
        providers: &[
            "Datapipe",
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Internet2",
            "Megaport",
            "GTT",
            "Allstream",
            "Equinix, Inc.",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "IX Reach",
            "Comcast",
            "Sprint",
            "PacketFabric",
            "Telus",
            "Arelion",
            "Epsilon",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, CH2, Chicago, IL",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 2,
            },
            CatalogLocationRow {
                partner_code: "CL",
                partner_site: "Cologix, CHI1, Chicago, IL",
                port_speeds: &["1G", "10G", "100G", "400G"],
                offers_mac_sec: true,
                flat_rate_tiers: 0,
            },
        ],
    },
    CatalogFacility {
        code: "EqSe2",
        building: "Westin Building Exchange, Seattle, WA",
        region: "us-west-2",
        providers: WESTIN_BUILDING_PROVIDERS,
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, SE2, Seattle, WA",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                // Recorded: this demarcation publishes the usage-based mode alone.
                flat_rate_tiers: 0,
            },
            CatalogLocationRow {
                partner_code: "WBE",
                // The building's own meet-me room, named by its site designation
                // rather than by the building again.
                partner_site: "Westin Building Exchange - SEA10 - Main FMMR, Seattle, WA",
                port_speeds: &["1G", "10G", "100G", "400G"],
                offers_mac_sec: true,
                // Recorded: this demarcation publishes the usage-based mode alone.
                flat_rate_tiers: 0,
            },
        ],
    },
    CatalogFacility {
        code: "EqSe3",
        building: "Equinix SE3, Seattle, WA",
        region: "us-west-2",
        providers: &[
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Internet2",
            "Megaport",
            "GTT",
            "Allstream",
            "Equinix, Inc.",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Comcast",
            "PacketFabric",
            "Telus",
            "Arelion",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, SE3, Seattle, WA",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 2,
            },
            CatalogLocationRow {
                partner_code: "CL",
                partner_site: "Cologix, SEA1, Seattle, WA",
                port_speeds: &["1G", "10G"],
                offers_mac_sec: false,
                flat_rate_tiers: 1,
            },
        ],
    },
    CatalogFacility {
        code: "FlexHB1",
        building: "Flexential Hillsboro, Hillsboro, OR",
        region: "us-west-2",
        providers: &[
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "PacketFabric",
            "Telus",
            "Arelion",
        ],
        locations: &[CatalogLocationRow {
            partner_code: "FLX",
            partner_site: "Flexential, HIL1, Hillsboro, OR",
            port_speeds: &["1G", "10G", "100G", "400G"],
            offers_mac_sec: true,
            flat_rate_tiers: 2,
        }],
    },
    CatalogFacility {
        code: "Pittock",
        building: "Pittock Block, Portland, OR",
        region: "us-west-2",
        providers: &[
            "Colt",
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "IX Reach",
            "Comcast",
            "PacketFabric",
            "Telus",
            "Arelion",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "PB",
                // The building operator's own meet-me room, named by its site
                // designation rather than by the building again.
                partner_site: "Pittock Block - Main MMR, Portland, OR",
                port_speeds: &["1G", "10G"],
                offers_mac_sec: false,
                flat_rate_tiers: 0,
            },
            CatalogLocationRow {
                partner_code: "OPX",
                partner_site: "Opus Interactive, Pittock Block, Portland, OR",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 1,
            },
        ],
    },
    CatalogFacility {
        code: "SUPERNAP8",
        building: "Switch SUPERNAP 8, Las Vegas, NV",
        region: "us-west-2",
        providers: &[
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "PacketFabric",
        ],
        locations: &[CatalogLocationRow {
            partner_code: "SW",
            partner_site: "Switch, SUPERNAP 8, Las Vegas, NV",
            port_speeds: &["1G", "10G", "100G"],
            offers_mac_sec: true,
            flat_rate_tiers: 0,
        }],
    },
    CatalogFacility {
        code: "T5PDX",
        building: "T5@Portland, Hillsboro, OR",
        region: "us-west-2",
        providers: &["Megaport", "Zayo Group", "CenturyLink", "PacketFabric"],
        locations: &[CatalogLocationRow {
            partner_code: "T5",
            partner_site: "T5 Data Centers, Hillsboro, OR",
            port_speeds: &["1G", "10G", "100G"],
            offers_mac_sec: true,
            flat_rate_tiers: 3,
        }],
    },
    CatalogFacility {
        code: "TierPointSEA",
        building: "TierPoint Seattle, Seattle, WA",
        region: "us-west-2",
        providers: &["Zayo Group", "CenturyLink", "AT&T", "Comcast"],
        locations: &[CatalogLocationRow {
            partner_code: "TP",
            partner_site: "TierPoint, Seattle, WA",
            port_speeds: &["1G", "10G"],
            offers_mac_sec: false,
            flat_rate_tiers: 0,
        }],
    },
    CatalogFacility {
        code: "EqSV5",
        building: "Equinix SV5, San Jose, CA",
        region: "us-west-1",
        providers: &[
            "Pureport",
            "ePLDT",
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Internet2",
            "Transtelco",
            "Megaport",
            "GTT",
            "Layer42",
            "Equinix, Inc.",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "IX Reach",
            "Comcast",
            "PacketFabric",
            "Arelion",
            "China Mobile International",
            "Epsilon",
        ],
        locations: &[CatalogLocationRow {
            partner_code: "EQ",
            partner_site: "Equinix, SV5, San Jose, CA",
            port_speeds: &["1G", "10G", "100G"],
            offers_mac_sec: true,
            flat_rate_tiers: 2,
        }],
    },
    CatalogFacility {
        code: "EqLD5",
        building: "Equinix LD5, Slough, UK",
        region: "eu-west-2",
        providers: &[
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Megaport",
            "GTT",
            "Equinix, Inc.",
            "Zayo Group",
            "Masergy",
            "CenturyLink",
            "IX Reach",
            "Arelion",
            "China Mobile International",
            "Epsilon",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, LD5, Slough, UK",
                port_speeds: &["1G", "10G", "100G", "400G"],
                offers_mac_sec: true,
                flat_rate_tiers: 3,
            },
            CatalogLocationRow {
                partner_code: "THN",
                partner_site: "Telehouse, North Two, London, UK",
                port_speeds: &["1G", "10G"],
                offers_mac_sec: false,
                flat_rate_tiers: 0,
            },
        ],
    },
    CatalogFacility {
        code: "EqFR5",
        building: "Equinix FR5, Frankfurt, DE",
        region: "eu-central-1",
        providers: &[
            "Telstra",
            "Colt",
            "Verizon",
            "Console Connect",
            "Megaport",
            "GTT",
            "Equinix, Inc.",
            "Zayo Group",
            "Masergy",
            "CenturyLink",
            "IX Reach",
            "Arelion",
            "Epsilon",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, FR5, Frankfurt, DE",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 2,
            },
            CatalogLocationRow {
                partner_code: "ITX",
                partner_site: "Interxion, FRA6, Frankfurt, DE",
                port_speeds: &["1G", "10G"],
                offers_mac_sec: false,
                flat_rate_tiers: 0,
            },
        ],
    },
    CatalogFacility {
        code: "EqSG2",
        building: "Equinix SG2, Singapore",
        region: "ap-southeast-1",
        providers: &[
            "ePLDT",
            "Telstra",
            "Colt",
            "Console Connect",
            "Megaport",
            "Equinix, Inc.",
            "IX Reach",
            "Arelion",
            "China Mobile International",
            "Epsilon",
        ],
        locations: &[CatalogLocationRow {
            partner_code: "EQ",
            partner_site: "Equinix, SG2, Singapore",
            port_speeds: &["1G", "10G"],
            offers_mac_sec: false,
            flat_rate_tiers: 1,
        }],
    },
    CatalogFacility {
        code: "EqTY2",
        building: "Equinix TY2, Tokyo, JP",
        region: "ap-northeast-1",
        providers: &[
            "ePLDT",
            "Telstra",
            "Colt",
            "Console Connect",
            "Megaport",
            "Equinix, Inc.",
            "Masergy",
            "IX Reach",
            "Arelion",
            "China Mobile International",
            "Epsilon",
        ],
        locations: &[
            CatalogLocationRow {
                partner_code: "EQ",
                partner_site: "Equinix, TY2, Tokyo, JP",
                port_speeds: &["1G", "10G", "100G"],
                offers_mac_sec: true,
                flat_rate_tiers: 2,
            },
            CatalogLocationRow {
                partner_code: "NTT",
                partner_site: "NTT Communications, Otemachi, Tokyo, JP",
                port_speeds: &["1G", "10G", "100G", "400G"],
                offers_mac_sec: true,
                flat_rate_tiers: 0,
            },
        ],
    },
];

/// The catalog [`BUILT_IN_FACILITIES`] describes, keyed by `locationCode`.
///
/// One entry per (facility, partner) pair, because that is what AWS publishes a
/// `Location` for: its `locationCode` is the facility's code suffixed with the
/// partner's (`EqSe2` + `WBE` -> `EqSe2-WBE`), and its `locationName` is the building
/// joined to that partner's own site by `" | "`, which is what
/// `Location$locationName` means by "the name of the colocation partner and the
/// physical site of the building".
pub fn built_in_location_catalog() -> BTreeMap<String, LocationRecord> {
    let mut catalog = BTreeMap::new();
    for facility in BUILT_IN_FACILITIES {
        for row in facility.locations {
            let location_code = format!("{}-{}", facility.code, row.partner_code);
            catalog.insert(
                location_code.clone(),
                LocationRecord {
                    location_code,
                    location_name: format!("{} | {}", facility.building, row.partner_site),
                    region: facility.region.to_string(),
                    available_port_speeds: catalog_port_speeds(row.port_speeds),
                    // The facility's partner list, narrowed to the roster, so one
                    // published order covers the whole catalog.
                    available_providers: published_providers(facility.providers),
                    offers_mac_sec: row.offers_mac_sec,
                    flat_rate_tiers: row.flat_rate_tiers,
                },
            );
        }
    }
    catalog
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsDirectconnectOvertureserviceState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsDirectconnectOvertureserviceState>)
}

/// The Region the request is signed for, from its SigV4 credential scope
/// (`Credential=<akid>/<date>/<region>/<name>/aws4_request`), else
/// [`DEFAULT_REGION`].
///
/// Direct Connect is a regional service, so `Connection$region` is a property of
/// the request's endpoint, not of the caller's body.
pub fn request_region(headers: &::http::HeaderMap) -> String {
    ::emulator_server::sigv4_credential_scope(headers)
        .map(|scope| scope.region)
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// Whether a connection of `bandwidth` supports jumbo frames.
///
/// Jumbo frames (the 8500-byte MTU `NewPrivateVirtualInterface$mtu` documents) are
/// a property of a DEDICATED port, whose bandwidths AWS quotes in Gbps; a hosted
/// sub-gigabit connection is quoted in Mbps and carries the 1500-byte MTU only.
/// Keyed on the unit of the stored bandwidth string, so it holds for every
/// bandwidth AWS offers rather than for an enumerated few.
pub fn bandwidth_supports_jumbo_frames(bandwidth: &str) -> bool {
    bandwidth.trim().to_ascii_lowercase().ends_with("gbps")
}

/// The Direct Connect endpoint device id for a resource at `location`.
///
/// AWS reports these as `<LocationCode>-<opaque suffix>`. The suffix is derived
/// from `seed` so it is stable for the life of the resource (two reads of one
/// connection report the same device) and distinct between resources.
pub fn device_id(location: &str, seed: &str) -> String {
    format!("{location}-{}", base36(fnv1a64(seed.as_bytes()), 13))
}

/// The link-local `/30` AWS assigns to the n-th virtual interface when the caller
/// supplies no addresses: the peering addresses come from `169.254.0.0/16`, two
/// usable hosts per `/30`, the Amazon side on the first and the customer side on
/// the second.
///
/// Returns `(amazonAddress, customerAddress)`.
pub fn link_local_peering_addresses(index: u64) -> (String, String) {
    // 169.254.0.0/16 holds 16384 /30 blocks; wrap so a long-lived run keeps
    // producing a well-formed address rather than overflowing out of the range.
    let block = index % 16_384;
    let third = block / 64;
    let fourth = (block % 64) * 4;
    (
        format!("169.254.{third}.{}/30", fourth + 1),
        format!("169.254.{third}.{}/30", fourth + 2),
    )
}

/// One page of a list response, plus the token that fetches the next one.
#[derive(Debug)]
pub struct Page<T> {
    /// The items on this page.
    pub items: Vec<T>,
    /// The token the caller passes as `nextToken` for the following page; `None`
    /// when this page is the last one, which is what
    /// `Connections$nextToken` documents ("This value is null when there are no
    /// more results to return").
    pub next_token: Option<String>,
}

/// Window an already-ordered result set by the `maxResults`/`nextToken` pair both
/// list operations model.
///
/// The token is the KEY of the last item on the previous page, so paging stays
/// stable when items are added or removed between calls. `None` means the token
/// names no item in this result set — a client error for the caller to answer.
pub fn paginate<T>(
    items: Vec<T>,
    max_results: Option<i32>,
    next_token: Option<&str>,
    key: impl Fn(&T) -> String,
) -> Option<Page<T>> {
    let mut items = items;
    if let Some(token) = next_token {
        let position = items.iter().position(|item| key(item) == token)?;
        items.drain(..=position);
    }
    let limit = match max_results {
        // "If MaxResults is given a value larger than 100, only 100 results are
        // returned."
        Some(max) if max > 0 => {
            usize::try_from(max.min(MAX_RESULT_SET_SIZE)).unwrap_or(usize::MAX)
        }
        // `maxResults` absent, zero or negative asks for no window at all.
        _ => items.len(),
    };
    let next_token = if items.len() > limit {
        items.truncate(limit);
        items.last().map(key)
    } else {
        None
    };
    Some(Page { items, next_token })
}

/// FNV-1a over `bytes`. A stable, dependency-free mixer for deriving an opaque but
/// REPRODUCIBLE device suffix from a resource id; it is not used for anything
/// security-relevant.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// `value` in lowercase base36, left-padded to `len` digits — the alphabet AWS's
/// own device suffixes use.
fn base36(mut value: u64, len: usize) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut digits = vec![b'0'; len];
    for slot in digits.iter_mut().rev() {
        *slot = ALPHABET[usize::try_from(value % 36).unwrap_or(0)];
        value /= 36;
    }
    String::from_utf8(digits).expect("the base36 alphabet is ASCII")
}

/// Render a stored [`Connection`] into one of the generated `Connection`-shaped
/// output structs (`CreateConnectionOutput`, `DeleteConnectionOutput`, the list
/// element `AllocateConnectionOnInterconnectOutput`).
///
/// A macro rather than a function because the generated types are distinct Rust
/// structs for one Smithy shape; this keeps ONE rendering of a stored connection,
/// so every operation answers the same field set.
#[macro_export]
macro_rules! connection_output {
    ($ty:ident, $conn:expr) => {{
        let conn: &$crate::state::Connection = $conn;
        $ty {
            owner_account: Some(conn.owner_account.clone()),
            connection_id: Some(conn.connection_id.clone()),
            connection_name: Some(conn.connection_name.clone()),
            connection_state: Some(conn.connection_state.clone()),
            region: Some(conn.region.clone()),
            location: Some(conn.location.clone()),
            bandwidth: Some(conn.bandwidth.clone()),
            vlan: conn.vlan,
            loa_issue_time: conn.loa_issue_time,
            lag_id: conn.lag_id.clone(),
            aws_device: Some(conn.aws_device.clone()),
            aws_device_v2: Some(conn.aws_device_v2.clone()),
            aws_logical_device_id: Some(conn.aws_logical_device_id.clone()),
            jumbo_frame_capable: Some(conn.jumbo_frame_capable),
            has_logical_redundancy: Some(conn.has_logical_redundancy.clone()),
            tags: (!conn.tags.is_empty()).then(|| conn.tags.clone()),
            provider_name: conn.provider_name.clone(),
            mac_sec_capable: Some(conn.mac_sec_capable),
            // MACsec encryption is reported only for a connection that supports
            // it, and a connection with no MAC Security key associated has its
            // port encryption down — `Connection$portEncryptionStatus` names
            // "Encryption Up, which means that there is an active Connection Key
            // Name, or Encryption Down".
            port_encryption_status: conn
                .mac_sec_capable
                .then(|| "Encryption Down".to_string()),
            encryption_mode: conn.mac_sec_capable.then(|| "no_encrypt".to_string()),
            billing_mode: conn.billing_mode.clone(),
            ..Default::default()
        }
    }};
}

/// Render a stored [`VirtualInterface`] into one of the generated
/// `VirtualInterface`-shaped output structs
/// (`CreatePrivateVirtualInterfaceOutput`, the list element
/// `AllocatePrivateVirtualInterfaceOutput`). See [`connection_output!`] for why
/// this is a macro.
#[macro_export]
macro_rules! virtual_interface_output {
    ($ty:ident, $vif:expr) => {{
        let vif: &$crate::state::VirtualInterface = $vif;
        $ty {
            owner_account: Some(vif.owner_account.clone()),
            virtual_interface_id: Some(vif.virtual_interface_id.clone()),
            location: Some(vif.location.clone()),
            connection_id: Some(vif.connection_id.clone()),
            virtual_interface_type: Some(vif.virtual_interface_type.clone()),
            virtual_interface_name: Some(vif.virtual_interface_name.clone()),
            vlan: vif.vlan,
            asn: vif.asn,
            asn_long: vif.asn_long,
            amazon_side_asn: vif.amazon_side_asn,
            auth_key: vif.auth_key.clone(),
            amazon_address: vif.amazon_address.clone(),
            customer_address: vif.customer_address.clone(),
            address_family: Some(vif.address_family.clone()),
            virtual_interface_state: Some(vif.virtual_interface_state.clone()),
            mtu: Some(vif.mtu),
            jumbo_frame_capable: Some(vif.jumbo_frame_capable),
            virtual_gateway_id: vif.virtual_gateway_id.clone(),
            direct_connect_gateway_id: vif.direct_connect_gateway_id.clone(),
            route_filter_prefixes: (!vif.route_filter_prefixes.is_empty())
                .then(|| vif.route_filter_prefixes.clone()),
            bgp_peers: (!vif.bgp_peers.is_empty()).then(|| vif.bgp_peers.clone()),
            region: Some(vif.region.clone()),
            aws_device_v2: Some(vif.aws_device_v2.clone()),
            aws_logical_device_id: Some(vif.aws_logical_device_id.clone()),
            tags: (!vif.tags.is_empty()).then(|| vif.tags.clone()),
            site_link_enabled: Some(vif.site_link_enabled),
            ..Default::default()
        }
    }};
}
