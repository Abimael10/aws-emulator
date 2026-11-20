// SPDX-License-Identifier: CC0-1.0

//! The Direct Connect locations this emulator publishes, written as AWS publishes
//! them.
//!
//! `DescribeLocations` takes no input and no caller identity narrows it, so what it
//! answers is the public location list for the signed Region: catalog data, not caller
//! data and not account data. Every value AWS publishes for a location is WRITTEN on
//! that location's row here — its `locationCode`, its `locationName`, the delivery
//! partners it names, and each flat-rate billing mode it offers with the Regions that
//! mode's fee includes — because none of them follows from another. AWS publishes a
//! location for a whole building under that building's own code and name (`SNAP`,
//! `Switch SUPERNAP 8, Las Vegas, NV`) and a location for one colocation partner's
//! demarcation inside a shared building under a code suffixed with that partner's and
//! a name carrying the building and the partner's site (`EqSe2-WBE`), and a row has to
//! be able to carry either, so it carries the published strings themselves.
//!
//! What this module does NOT write down is anything two published lists of ONE location
//! would then be free to contradict: the order a row's port speeds are published in
//! ([`catalog_port_speeds`]), its MACsec speeds ([`mac_sec_port_speeds`]), the port
//! speeds each billing mode is offered on and the order its partners are published in
//! ([`published_providers`]) are all rendered from the row's own speeds and partner
//! membership.
//!
//! WHICH rows are recorded: the `us-west-2` elements this emulator has a recorded
//! `DescribeLocations` response for — the Seattle carrier hotel's two demarcations and
//! the Las Vegas building, with their codes, names, speeds, partner rosters and (for
//! the Las Vegas building) the first two flat-rate tiers' `includedRegions`. Every
//! other row, and the wider tiers of every ladder, is CATALOGUED: written from the
//! public location and delivery-partner lists under the same published shape, so a
//! later fixer holding a recording for one of them should refine that row rather than
//! read it as a recorded fact.
//!
//! The rows deliberately span the class: locations published for a whole building and
//! for one partner's demarcation in it, port-speed sets with and without 100G and
//! 400G, locations with and without MACsec, partner rosters from a carrier hotel's
//! thirty names down to a handful at a metro site, a roster naming a delivery partner
//! no other row does, and locations publishing no flat-rate mode, one, and a full
//! five-tier ladder, in several Regions.

use super::*;

/// One flat-rate billing mode on a catalog row: the `BillingMode` it is published
/// under and the Regions that mode's fee includes, both as published.
struct CatalogFlatRateMode {
    /// A `BillingMode` value other than [`USAGE_BILLING_MODE`] — a flat-rate tier or
    /// a port-pair flat-rate tier.
    billing_mode: &'static str,
    /// The `includedRegions` the mode publishes, in published order.
    included_regions: &'static [&'static str],
}

/// One row of the catalog: one location AWS publishes a `Location` for.
struct CatalogLocation {
    /// The published `locationCode`, verbatim — a building's own code, or a building's
    /// code suffixed with a colocation partner's.
    code: &'static str,
    /// The published `locationName`, verbatim — the building alone, or the building
    /// and the partner's own site inside it.
    name: &'static str,
    /// The Region whose `DescribeLocations` answers this location.
    region: &'static str,
    /// The port speeds AWS lands here — a subset of [`DEDICATED_PORT_SPEEDS`], not all
    /// of it.
    port_speeds: &'static [&'static str],
    /// Whether MAC Security is offered here at all; which of [`Self::port_speeds`]
    /// carry it is [`mac_sec_port_speeds`].
    offers_mac_sec: bool,
    /// The delivery partners that deliver at this location.
    providers: ProviderPresence,
    /// The flat-rate billing modes published here, in published order; empty where
    /// only the usage-based mode is.
    flat_rate_modes: &'static [CatalogFlatRateMode],
}

/// The delivery partners the Seattle carrier hotel's two demarcations publish.
///
/// Recorded: both of its elements print these thirty names. Written out rather than
/// taken as "the whole roster", because the roster also admits partners that deliver
/// only elsewhere ([`DIRECT_CONNECT_PROVIDERS`]) and this building names none of them.
const WESTIN_BUILDING_PROVIDERS: ProviderPresence = &[
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

/// The delivery partners the Las Vegas building publishes.
///
/// Recorded, and the row that shows a location's roster is its OWN: it names delivery
/// partners (`Crown Castle`, `Cox Business`) no other row in its Region does.
const SUPERNAP_8_PROVIDERS: ProviderPresence = &[
    "Verizon",
    "Megaport",
    "Zayo Group",
    "CenturyLink",
    "AT&T",
    "Crown Castle",
    "IX Reach",
    "Cox Business",
    "XO Communications",
];

/// `us-west-2`, tier 1: recorded.
const US_WEST_2_TIER_1: CatalogFlatRateMode = CatalogFlatRateMode {
    billing_mode: "FlatRateTier1",
    included_regions: &["us-west-1"],
};

/// `us-west-2`, tier 2: recorded. A tier's Regions are published in the catalog's own
/// order and include the location's OWN Region as well as one across the Atlantic,
/// which is why a tier's list is written rather than walked out of a Region geography.
const US_WEST_2_TIER_2: CatalogFlatRateMode = CatalogFlatRateMode {
    billing_mode: "FlatRateTier2",
    included_regions: &["us-east-2", "eu-west-1", "us-east-1", "us-west-2"],
};

/// `us-west-2`, tier 3: catalogued — the tier-2 group with Canada and the rest of
/// Europe.
const US_WEST_2_TIER_3: CatalogFlatRateMode = CatalogFlatRateMode {
    billing_mode: "FlatRateTier3",
    included_regions: &[
        "us-east-2",
        "eu-west-1",
        "us-east-1",
        "us-west-2",
        "us-west-1",
        "ca-central-1",
        "eu-west-2",
        "eu-central-1",
        "eu-north-1",
    ],
};

/// `us-west-2`, tier 4: catalogued — the tier-3 group with Asia Pacific.
const US_WEST_2_TIER_4: CatalogFlatRateMode = CatalogFlatRateMode {
    billing_mode: "FlatRateTier4",
    included_regions: &[
        "us-east-2",
        "eu-west-1",
        "us-east-1",
        "us-west-2",
        "us-west-1",
        "ca-central-1",
        "eu-west-2",
        "eu-central-1",
        "eu-north-1",
        "ap-northeast-1",
        "ap-northeast-2",
        "ap-southeast-1",
        "ap-southeast-2",
        "ap-south-1",
    ],
};

/// `us-west-2`, tier 5: catalogued — every commercial Region the catalog knows.
const US_WEST_2_TIER_5: CatalogFlatRateMode = CatalogFlatRateMode {
    billing_mode: "FlatRateTier5",
    included_regions: &[
        "us-east-2",
        "eu-west-1",
        "us-east-1",
        "us-west-2",
        "us-west-1",
        "ca-central-1",
        "eu-west-2",
        "eu-central-1",
        "eu-north-1",
        "ap-northeast-1",
        "ap-northeast-2",
        "ap-northeast-3",
        "ap-southeast-1",
        "ap-southeast-2",
        "ap-south-1",
        "ap-east-1",
        "sa-east-1",
        "me-south-1",
        "af-south-1",
        "eu-south-1",
    ],
};

/// The whole `us-west-2` ladder, which the recorded Las Vegas building publishes.
const US_WEST_2_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    US_WEST_2_TIER_1,
    US_WEST_2_TIER_2,
    US_WEST_2_TIER_3,
    US_WEST_2_TIER_4,
    US_WEST_2_TIER_5,
];

/// The first tier of the `us-west-2` ladder, for a location offering that alone.
const US_WEST_2_FIRST_TIER: &[CatalogFlatRateMode] = &[US_WEST_2_TIER_1];

/// The first two tiers of the `us-west-2` ladder.
const US_WEST_2_FIRST_TWO_TIERS: &[CatalogFlatRateMode] = &[US_WEST_2_TIER_1, US_WEST_2_TIER_2];

/// The first three tiers of the `us-west-2` ladder.
const US_WEST_2_FIRST_THREE_TIERS: &[CatalogFlatRateMode] =
    &[US_WEST_2_TIER_1, US_WEST_2_TIER_2, US_WEST_2_TIER_3];

/// The `us-east-1` ladder: catalogued, one tier per step out from the location.
const US_EAST_1_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["us-east-2"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &["us-east-1", "us-west-2", "eu-west-1", "ca-central-1"],
    },
];

/// The first tier of the `us-east-1` ladder.
const US_EAST_1_FIRST_TIER: &[CatalogFlatRateMode] = &[CatalogFlatRateMode {
    billing_mode: "FlatRateTier1",
    included_regions: &["us-east-2"],
}];

/// The `us-east-2` ladder: catalogued.
const US_EAST_2_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["us-east-1"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &["us-east-2", "us-west-2", "eu-west-1", "ca-central-1"],
    },
];

/// The `us-west-1` ladder: catalogued.
const US_WEST_1_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["us-west-2"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &["us-west-1", "us-east-1", "us-east-2", "eu-west-1"],
    },
];

/// The `eu-west-2` ladder: catalogued, and the one that shows a location can publish
/// a port-pair flat-rate mode — `BillingMode` carries `PortPairFlatRateTier1` ..
/// `PortPairFlatRateTier5` beside the per-port tiers
/// (`smithy_query(query="shape",shape="BillingMode")`).
const EU_WEST_2_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["eu-west-1"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &["eu-west-2", "eu-central-1", "eu-north-1", "us-east-1"],
    },
    CatalogFlatRateMode {
        billing_mode: "PortPairFlatRateTier1",
        included_regions: &["eu-west-1"],
    },
];

/// The `eu-central-1` ladder: catalogued.
const EU_CENTRAL_1_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["eu-west-1"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &["eu-central-1", "eu-west-2", "eu-north-1", "us-east-1"],
    },
];

/// The `ap-southeast-1` ladder: catalogued.
const AP_SOUTHEAST_1_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[CatalogFlatRateMode {
    billing_mode: "FlatRateTier1",
    included_regions: &["ap-southeast-2"],
}];

/// The `ap-northeast-1` ladder: catalogued.
const AP_NORTHEAST_1_FLAT_RATE_LADDER: &[CatalogFlatRateMode] = &[
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier1",
        included_regions: &["ap-northeast-2"],
    },
    CatalogFlatRateMode {
        billing_mode: "FlatRateTier2",
        included_regions: &[
            "ap-northeast-1",
            "ap-southeast-1",
            "ap-southeast-2",
            "us-west-2",
        ],
    },
];

/// A location that publishes the usage-based mode alone.
const NO_FLAT_RATE_MODES: &[CatalogFlatRateMode] = &[];

/// The Direct Connect locations this emulator publishes, one row per published
/// `Location`.
const BUILT_IN_LOCATIONS: &[CatalogLocation] = &[
    CatalogLocation {
        code: "EqDC2-EQ",
        name: "Equinix DC2, Ashburn, VA | Equinix, DC2, Ashburn, VA",
        region: "us-east-1",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: US_EAST_1_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqDC2-DRT",
        name: "Equinix DC2, Ashburn, VA | Digital Realty, IAD38, Ashburn, VA",
        region: "us-east-1",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &[
            "Telstra",
            "Colt",
            "Verizon",
            "Megaport",
            "GTT",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Comcast",
            "PacketFabric",
            "Arelion",
        ],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        // A location published for a whole building: the operator's own code and the
        // building alone, with no partner suffix and no partner site.
        code: "CSDC1",
        name: "CoreSite DC1, Reston, VA",
        region: "us-east-1",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &[
            "Internet2",
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "PacketFabric",
            "Arelion",
        ],
        flat_rate_modes: US_EAST_1_FIRST_TIER,
    },
    CatalogLocation {
        code: "EqCh2-EQ",
        name: "Equinix CH2, Chicago, IL | Equinix, CH2, Chicago, IL",
        region: "us-east-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: US_EAST_2_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqCh2-CL",
        name: "Equinix CH2, Chicago, IL | Cologix, CHI1, Chicago, IL",
        region: "us-east-2",
        port_speeds: &["1G", "10G", "100G", "400G"],
        offers_mac_sec: true,
        providers: &[
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Comcast",
            "PacketFabric",
            "Telus",
            "Arelion",
        ],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        // Recorded: a demarcation in a shared building, published under the building's
        // code suffixed with the colocation partner's and a name carrying both.
        code: "EqSe2-EQ",
        name: "Westin Building Exchange, Seattle, WA | Equinix, SE2, Seattle, WA",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
        providers: WESTIN_BUILDING_PROVIDERS,
        // Recorded: this demarcation publishes the usage-based mode alone.
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        // Recorded: the building's own meet-me room, named by its site designation
        // rather than by the building again.
        code: "EqSe2-WBE",
        name: "Westin Building Exchange, Seattle, WA | Westin Building Exchange - SEA10 - Main FMMR, Seattle, WA",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G", "400G"],
        offers_mac_sec: true,
        providers: WESTIN_BUILDING_PROVIDERS,
        // Recorded: this demarcation publishes the usage-based mode alone.
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        // Recorded: a location published for a whole building — an unsuffixed code and
        // the building alone, a roster naming delivery partners no other row in this
        // Region does, and the full five-tier ladder. Recorded third in this Region's
        // list, right after the carrier hotel's two demarcations, which is why the
        // table's order is the published one.
        code: "SNAP",
        name: "Switch SUPERNAP 8, Las Vegas, NV",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
        providers: SUPERNAP_8_PROVIDERS,
        flat_rate_modes: US_WEST_2_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqSe3-EQ",
        name: "Equinix SE3, Seattle, WA | Equinix, SE3, Seattle, WA",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: US_WEST_2_FIRST_TWO_TIERS,
    },
    CatalogLocation {
        code: "EqSe3-CL",
        name: "Equinix SE3, Seattle, WA | Cologix, SEA1, Seattle, WA",
        region: "us-west-2",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &[
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Comcast",
            "PacketFabric",
            "Telus",
        ],
        flat_rate_modes: US_WEST_2_FIRST_TIER,
    },
    CatalogLocation {
        code: "FlexHB1",
        name: "Flexential Hillsboro, Hillsboro, OR",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G", "400G"],
        offers_mac_sec: true,
        providers: &[
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "PacketFabric",
            "Telus",
            "Arelion",
        ],
        flat_rate_modes: US_WEST_2_FIRST_TWO_TIERS,
    },
    CatalogLocation {
        code: "Pittock-PB",
        name: "Pittock Block, Portland, OR | Pittock Block - Main MMR, Portland, OR",
        region: "us-west-2",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
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
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        code: "Pittock-OPX",
        name: "Pittock Block, Portland, OR | Opus Interactive, Pittock Block, Portland, OR",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
        providers: &[
            "Colt",
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Comcast",
            "PacketFabric",
            "Telus",
        ],
        flat_rate_modes: US_WEST_2_FIRST_TIER,
    },
    CatalogLocation {
        code: "T5PDX",
        name: "T5@Portland, Hillsboro, OR",
        region: "us-west-2",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
        providers: &["Megaport", "Zayo Group", "CenturyLink", "PacketFabric"],
        flat_rate_modes: US_WEST_2_FIRST_THREE_TIERS,
    },
    CatalogLocation {
        code: "TierPointSEA",
        name: "TierPoint Seattle, Seattle, WA",
        region: "us-west-2",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &["Zayo Group", "CenturyLink", "AT&T", "Comcast"],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        code: "EqSV5",
        name: "Equinix SV5, San Jose, CA",
        region: "us-west-1",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: US_WEST_1_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqLD5-EQ",
        name: "Equinix LD5, Slough, UK | Equinix, LD5, Slough, UK",
        region: "eu-west-2",
        port_speeds: &["1G", "10G", "100G", "400G"],
        offers_mac_sec: true,
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
        flat_rate_modes: EU_WEST_2_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqLD5-THN",
        name: "Equinix LD5, Slough, UK | Telehouse, North Two, London, UK",
        region: "eu-west-2",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &[
            "Colt",
            "Verizon",
            "Megaport",
            "GTT",
            "CenturyLink",
            "IX Reach",
            "Arelion",
            "Epsilon",
        ],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        code: "EqFR5-EQ",
        name: "Equinix FR5, Frankfurt, DE | Equinix, FR5, Frankfurt, DE",
        region: "eu-central-1",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: EU_CENTRAL_1_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqFR5-ITX",
        name: "Equinix FR5, Frankfurt, DE | Interxion, FRA6, Frankfurt, DE",
        region: "eu-central-1",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
        providers: &[
            "Colt",
            "Verizon",
            "Megaport",
            "GTT",
            "CenturyLink",
            "IX Reach",
            "Arelion",
        ],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
    CatalogLocation {
        code: "EqSG2",
        name: "Equinix SG2, Singapore",
        region: "ap-southeast-1",
        port_speeds: &["1G", "10G"],
        offers_mac_sec: false,
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
        flat_rate_modes: AP_SOUTHEAST_1_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqTY2-EQ",
        name: "Equinix TY2, Tokyo, JP | Equinix, TY2, Tokyo, JP",
        region: "ap-northeast-1",
        port_speeds: &["1G", "10G", "100G"],
        offers_mac_sec: true,
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
        flat_rate_modes: AP_NORTHEAST_1_FLAT_RATE_LADDER,
    },
    CatalogLocation {
        code: "EqTY2-NTT",
        name: "Equinix TY2, Tokyo, JP | NTT Communications, Otemachi, Tokyo, JP",
        region: "ap-northeast-1",
        port_speeds: &["1G", "10G", "100G", "400G"],
        offers_mac_sec: true,
        providers: &[
            "Telstra",
            "Colt",
            "Console Connect",
            "Megaport",
            "IX Reach",
            "Arelion",
            "China Mobile International",
        ],
        flat_rate_modes: NO_FLAT_RATE_MODES,
    },
];

/// The catalog [`BUILT_IN_LOCATIONS`] describes, in the order it publishes them.
///
/// One record per published location, carrying that location's own published code and
/// name; its port speeds in [`catalog_port_speeds`] order; its partners narrowed to
/// the roster's published order ([`published_providers`]), so one order covers the
/// whole catalog; and its flat-rate modes with the Regions each one includes, as
/// published. The ORDER is the table's own, because the order AWS lists a Region's
/// locations in is published data too: the recorded `us-west-2` response lists the
/// carrier hotel's two demarcations and then the Las Vegas building, which is not the
/// order their codes sort in.
pub fn built_in_location_catalog() -> Vec<LocationRecord> {
    BUILT_IN_LOCATIONS
        .iter()
        .map(|location| LocationRecord {
            location_code: location.code.to_string(),
            location_name: location.name.to_string(),
            region: location.region.to_string(),
            available_port_speeds: catalog_port_speeds(location.port_speeds),
            available_providers: published_providers(location.providers),
            offers_mac_sec: location.offers_mac_sec,
            flat_rate_modes: location
                .flat_rate_modes
                .iter()
                .map(|mode| LocationFlatRateMode {
                    billing_mode: mode.billing_mode.to_string(),
                    included_regions: mode
                        .included_regions
                        .iter()
                        .map(|region| (*region).to_string())
                        .collect(),
                })
                .collect(),
        })
        .collect()
}
