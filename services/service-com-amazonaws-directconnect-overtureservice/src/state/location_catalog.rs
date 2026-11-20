// SPDX-License-Identifier: CC0-1.0

//! The Direct Connect location catalog's record and the rendering of it.
//!
//! `DescribeLocations` publishes one `Location` per location AWS lists in the signed
//! Region, and every published value of it — its `locationCode`, its `locationName`,
//! the delivery partners it names and the billing modes it offers with the Regions
//! each mode includes — is CATALOG DATA, written per location in
//! [`super::published_locations`] and stored in a [`LocationRecord`]. Nothing here
//! synthesises one published value out of another: the only things derived are the
//! ORDER a list is published in and the narrowing of a list to the location's own
//! ports, so two lists of one location cannot disagree.

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
/// ([`super::CatalogLocation::port_speeds`]), because AWS offers 400G — and even 100G
/// — at some facilities only.
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

/// One flat-rate billing mode a location publishes, as the catalog records it: the
/// `BillingMode` it is offered under and the Regions that mode's fixed fee includes.
///
/// `AvailableBillingMode$includedRegions` is "the Amazon Web Services Regions included
/// with the billing mode" and `billingMode` is a `BillingMode` (via
/// `smithy_query(query="shape",shape="AvailableBillingMode")`); the model constrains
/// neither which modes a location offers nor which Regions one of them includes, so
/// BOTH are catalog data and both are written per location rather than computed. A
/// mode's Region list is published exactly as it is recorded — in its own order,
/// naming whichever Regions that fee covers, the location's own Region among them
/// where it is covered — because no rule over Region geography reproduces it.
#[derive(Debug, Clone)]
pub struct LocationFlatRateMode {
    /// The `BillingMode` this mode is published under, one of the values the modelled
    /// enum carries ([`crate::ops::types::BILLING_MODE_VALUES`]) other than
    /// [`USAGE_BILLING_MODE`] — a flat-rate tier or a port-pair flat-rate tier.
    pub billing_mode: String,
    /// The `includedRegions` this mode publishes, in the order the catalog records.
    pub included_regions: Vec<String>,
}

/// The `availableBillingModes` a location publishes: the usage-based mode on every
/// port speed it offers, then the flat-rate modes the catalog records for that
/// location, in the order it records them.
///
/// Which modes a location offers and which Regions each one includes are read off the
/// stored record ([`LocationRecord::flat_rate_modes`]), never computed, so the catalog
/// can publish any recorded tier-to-Region mapping. What is still derived is only the
/// narrowing that keeps two lists of ONE location consistent: the usage-based mode is
/// published on the location's own speeds with no `includedRegions`, a flat-rate mode
/// on those speeds narrowed to the flat-rate ports ([`flat_rate_port_speeds`]), and a
/// mode is left out where the modelled enum does not carry its name, where the
/// location lands no flat-rate port, or where its fee includes no Region's transfer —
/// none of which is a mode a client could order.
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
    for mode in &location.flat_rate_modes {
        let modelled = crate::ops::types::BILLING_MODE_VALUES.contains(&mode.billing_mode.as_str());
        if !modelled || mode.billing_mode == USAGE_BILLING_MODE || mode.included_regions.is_empty()
        {
            continue;
        }
        modes.push(crate::ops::types::AvailableBillingMode {
            billing_mode: Some(mode.billing_mode.clone()),
            available_port_speeds: Some(flat_rate_speeds.clone()),
            included_regions: Some(mode.included_regions.clone()),
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

/// A Direct Connect location in this service's catalog — the `Location` shape's
/// data, owned by this service rather than rebuilt per response.
///
/// Catalogued location data, not caller data: no operation creates or deletes a
/// location, so the catalog is seeded once (see [`built_in_location_catalog`]) and
/// only read. Every value AWS PUBLISHES for a location is stored here as published —
/// its code, its name, its partners, its flat-rate modes with the Regions each
/// includes — because none of them follows from another. Only the two lists that must
/// agree with [`Self::available_port_speeds`] are derived from it:
/// `availableMacSecPortSpeeds` ([`mac_sec_port_speeds`]) and each billing mode's port
/// speeds ([`location_billing_modes`]).
#[derive(Debug, Clone)]
pub struct LocationRecord {
    /// `Location$locationCode`: "the code for the location", as AWS publishes it for
    /// THIS location — a facility code on its own (`SNAP`) where the location is the
    /// whole building, or a facility code suffixed with a colocation partner's
    /// (`EqSe2-WBE`) where AWS publishes one location per demarcation in a shared
    /// building. It is the value a client then passes as `CreateConnection`'s
    /// `location`, so it is stored verbatim rather than assembled from parts.
    pub location_code: String,
    /// `Location$locationName`: "the name of the location. This includes the name of
    /// the colocation partner and the physical site of the building" — the building
    /// alone where the location IS the building and its operator, and the building
    /// and the partner's own site where AWS publishes a partner's demarcation in it.
    /// Stored as published, for the same reason as [`Self::location_code`].
    pub location_name: String,
    /// `Location$region`: "the Amazon Web Services Region for the location."
    pub region: String,
    /// `Location$availablePortSpeeds`: this location's own subset of
    /// [`DEDICATED_PORT_SPEEDS`], in [`catalog_port_speeds`] order.
    pub available_port_speeds: Vec<String>,
    /// `Location$availableProviders`: the service providers present at the location,
    /// in [`published_providers`] order.
    pub available_providers: Vec<String>,
    /// Whether the location offers MAC Security at all; which of its port speeds
    /// carry it is [`mac_sec_port_speeds`].
    pub offers_mac_sec: bool,
    /// The flat-rate billing modes published at this location, in published order;
    /// empty where only the usage-based mode is. Each one carries its own
    /// `includedRegions` ([`LocationFlatRateMode`]); the port speeds a mode is
    /// offered on are derived ([`location_billing_modes`]).
    pub flat_rate_modes: Vec<LocationFlatRateMode>,
}

/// Which Direct Connect delivery partners deliver at one location: MEMBERSHIP only,
/// because the order a location publishes them in is not a property of the place.
///
/// A list PER LOCATION rather than one catalog-wide list: `availableProviders` is
/// "the name of the service provider for the location", and which partners deliver
/// there is a property of that location — a carrier hotel's meet-me room reaches
/// nearly every partner, a single-tenant data centre a handful, and a site may name a
/// delivery partner no other site in its Region does. The ORDER comes from
/// [`DIRECT_CONNECT_PROVIDERS`] through [`published_providers`], so no location can
/// publish two partners in an order another location contradicts.
pub(crate) type ProviderPresence = &'static [&'static str];

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
/// Source: the recorded `DescribeLocations` responses for `us-west-2`, whose
/// carrier-hotel elements print the names in exactly this order — that is where the
/// roster's spellings and its order come from. The roster holds every partner ANY
/// location in the catalog names, which is more than the partners of one building:
/// a recorded single-building element names delivery partners (`Crown Castle`,
/// `Cox Business`) the carrier hotel's own list does not, and the roster has to admit
/// them for that location to publish them.
/// `Location$availableProviders` ("the name of the service provider for the
/// location", `ProviderList`, via `smithy_query(query="shape",shape="Location")`)
/// constrains neither the names nor their order, so both are catalog data.
pub const DIRECT_CONNECT_PROVIDERS: [&str; 32] = [
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
    "Crown Castle",
    "IX Reach",
    "Comcast",
    "Sprint",
    "Cox Business",
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
