// SPDX-License-Identifier: CC0-1.0

//! `DescribeLocations`: the Direct Connect location catalog, end to end over the
//! wire.
//!
//! Every assertion goes through `POST /` with
//! `X-Amz-Target: OvertureService.DescribeLocations` (awsJson1_1), the same path a
//! client takes, and every expectation is about what the SERVICE HOLDS: the catalog
//! seeded into this service's state.
//!
//! The rule under test: `DescribeLocations` answers 200 with one `Location` per
//! location the catalog publishes in the Region the request is signed for, and every
//! value AWS PUBLISHES for that location is answered as the catalog holds it, not
//! assembled out of another value —
//!
//! * `locationCode` is the published code, whether that is a building's own code with
//!   no partner suffix (`SNAP`) or a building's code suffixed with a colocation
//!   partner's (`EqSe2-WBE`);
//! * `locationName` is the published name, whether that is the building alone or the
//!   building and the partner's own site inside it, which is what
//!   `Location$locationName` means by "the name of the colocation partner and the
//!   physical site of the building";
//! * `availableProviders` is that location's OWN delivery partners, which may name a
//!   partner no other location in its Region names, published in the catalog's own
//!   (non-alphabetical) roster order and in the trade forms AWS prints;
//! * `availableBillingModes` opens with the usage-based mode, on the location's own
//!   port speeds and with no `includedRegions`, and then carries the flat-rate modes
//!   published at that location, each with its OWN `includedRegions` — any set of
//!   Regions, in the order published, the location's own Region among them where that
//!   mode's fee covers it;
//! * `availablePortSpeeds` is that location's own subset of the dedicated-connection
//!   speeds and `availableMacSecPortSpeeds` that same list narrowed to the speeds
//!   MACsec is offered on (or empty), so no two lists of one location disagree.
//!
//! The tables span the class the rule generalises over: every Region the catalog holds
//! (and one it holds none in), codes with and without a partner suffix, names with and
//! without a partner site, port-speed sets with and without 100G and 400G, locations
//! with and without MACsec, partner lists from a thirty-name carrier hotel down to a
//! handful at a metro site, and locations publishing no flat-rate mode, one, several,
//! and a port-pair tier.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_directconnect_overtureservice::ops::types::BILLING_MODE_VALUES;
use service_com_amazonaws_directconnect_overtureservice::state::{
    built_in_location_catalog, ComAmazonawsDirectconnectOvertureserviceState, LocationFlatRateMode,
    LocationRecord, DEDICATED_PORT_SPEEDS, DIRECT_CONNECT_PROVIDERS, FLAT_RATE_PORT_SPEEDS,
    MAC_SEC_PORT_SPEEDS, USAGE_BILLING_MODE,
};

use crate::connection_and_virtual_interface_lifecycle::Emulator;

/// The `locations` list `DescribeLocations` answers, with the response status.
async fn describe_locations(dx: &Emulator) -> (u16, Vec<Value>) {
    // The operation's input is `smithy.api#Unit`: an empty payload is the whole
    // request.
    let (status, response) = dx.call("DescribeLocations", json!({})).await;
    (
        status,
        response["locations"].as_array().cloned().unwrap_or_default(),
    )
}

/// The `availableBillingModes` a catalog record must publish, computed here from the
/// record's own fields: the usage-based mode on every speed the location offers, then
/// the flat-rate modes the record carries, in the record's order, each on the
/// location's own flat-rate ports and naming the Regions the record says that mode
/// includes.
fn expected_billing_modes(record: &LocationRecord) -> Vec<Value> {
    let port_speeds: Vec<&str> = record
        .available_port_speeds
        .iter()
        .map(String::as_str)
        .collect();
    let mut modes = vec![json!({
        "billingMode": USAGE_BILLING_MODE,
        "availablePortSpeeds": port_speeds,
    })];

    let flat_rate_speeds: Vec<&str> = FLAT_RATE_PORT_SPEEDS
        .iter()
        .copied()
        .filter(|speed| port_speeds.contains(speed))
        .collect();
    if flat_rate_speeds.is_empty() {
        return modes;
    }
    for mode in &record.flat_rate_modes {
        // A mode the modelled `BillingMode` enum does not name, or one whose fee
        // includes no Region's transfer, is not a mode a client could order.
        if !BILLING_MODE_VALUES.contains(&mode.billing_mode.as_str())
            || mode.billing_mode == USAGE_BILLING_MODE
            || mode.included_regions.is_empty()
        {
            continue;
        }
        modes.push(json!({
            "billingMode": mode.billing_mode,
            "availablePortSpeeds": flat_rate_speeds,
            "includedRegions": mode.included_regions,
        }));
    }
    modes
}

/// The `Location` JSON a catalog record must render to, computed here from the
/// record's own fields by the rules the handler has to follow, so the expectation is
/// derived independently of the service's renderer.
fn expected_location(record: &LocationRecord) -> Value {
    let port_speeds: Vec<&str> = record
        .available_port_speeds
        .iter()
        .map(String::as_str)
        .collect();
    let mac_sec_speeds: Vec<&str> = if record.offers_mac_sec {
        port_speeds
            .iter()
            .filter(|speed| MAC_SEC_PORT_SPEEDS.contains(*speed))
            .copied()
            .collect()
    } else {
        Vec::new()
    };

    json!({
        "locationCode": record.location_code,
        "locationName": record.location_name,
        "region": record.region,
        "availablePortSpeeds": port_speeds,
        "availableProviders": record.available_providers,
        "availableMacSecPortSpeeds": mac_sec_speeds,
        "availableBillingModes": expected_billing_modes(record),
    })
}

/// The answered `Location` whose `locationCode` is `code`.
fn location_of(locations: &[Value], code: &str) -> Value {
    locations
        .iter()
        .find(|location| location["locationCode"] == json!(code))
        .cloned()
        .unwrap_or_else(|| panic!("case {code:?}: the Region's catalog must publish this location, got {locations:?}"))
}

/// The `availableBillingModes` of the location `code` names.
fn billing_modes_of(locations: &[Value], code: &str) -> Vec<Value> {
    location_of(locations, code)["availableBillingModes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Whether `providers` is the delivery-partner roster NARROWED: every name is a
/// partner the catalog knows, and they come in the roster's own order.
///
/// Walking one iterator over the roster is what makes this an order check and not a
/// membership check: a list that names two partners in the opposite order to the
/// roster runs the iterator past the second one and fails.
fn in_published_order(providers: &[&str]) -> bool {
    let mut roster = DIRECT_CONNECT_PROVIDERS.iter();
    providers
        .iter()
        .all(|provider| roster.any(|known| known == provider))
}

#[tokio::test]
async fn every_region_answers_its_own_catalog_locations_in_full() {
    let catalog = built_in_location_catalog();
    let regions: BTreeSet<&str> = catalog
        .iter()
        .map(|location| location.region.as_str())
        .collect();
    assert!(
        regions.len() > 1,
        "the catalog must span more than one Region for this rule to be observable, got {regions:?}"
    );

    // Each Region the catalog holds a location in, plus one it holds none in: the
    // operation is scoped to "the current Amazon Web Services Region".
    let mut cases: Vec<&str> = regions.iter().copied().collect();
    cases.push("eu-north-1");

    for case in cases {
        let dx = Emulator::in_region(case);
        let (status, locations) = describe_locations(&dx).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {locations:?}"
        );

        let expected: Vec<Value> = catalog
            .iter()
            .filter(|location| location.region == case)
            .map(expected_location)
            .collect();
        assert_eq!(
            locations, expected,
            "case {case:?}: expected the Region's catalog locations in published order, got {locations:?}"
        );
        // A Region with no location is an empty list on 200, not an error.
        if case == "eu-north-1" {
            assert!(
                locations.is_empty(),
                "case {case:?}: a Region the catalog holds no location in answers an empty list, got {locations:?}"
            );
        } else {
            assert!(
                !locations.is_empty(),
                "case {case:?}: the catalog holds a location in this Region, got {locations:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_published_location_is_answered_with_its_own_code_name_partners_and_modes() {
    // Four `us-west-2` locations written out, spanning the class: a location published
    // for a whole building (an unsuffixed code, the building alone as the name, its own
    // partner roster and a five-tier flat-rate ladder) and three published for one
    // partner's demarcation in a shared building (a suffixed code, a two-part name),
    // with and without 400G and with and without MACsec.
    //
    // Source: the recorded `DescribeLocations` response for this Region for the Westin
    // Building and Las Vegas elements; the rest is the catalogued facility data in the
    // service's `published_locations` module.
    let dx = Emulator::in_region("us-west-2");
    let (status, locations) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {locations:?}");

    // The ORDER is published data too: the recorded response for this Region lists the
    // carrier hotel's two demarcations and then the Las Vegas building, which is not
    // the order their codes sort in.
    let answered_order: Vec<&str> = locations
        .iter()
        .map(|location| location["locationCode"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        answered_order.first().copied(),
        Some("EqSe2-EQ"),
        "expected the published order to open with the carrier hotel, got {answered_order:?}"
    );
    assert_eq!(
        answered_order.get(1).copied(),
        Some("EqSe2-WBE"),
        "expected its meet-me room second, got {answered_order:?}"
    );
    assert_eq!(
        answered_order.get(2).copied(),
        Some("SNAP"),
        "expected the Las Vegas building third, got {answered_order:?}"
    );

    let las_vegas = json!({
        "locationCode": "SNAP",
        "locationName": "Switch SUPERNAP 8, Las Vegas, NV",
        "region": "us-west-2",
        "availablePortSpeeds": ["100G", "1G", "10G"],
        "availableProviders": [
            "Verizon",
            "Megaport",
            "Zayo Group",
            "CenturyLink",
            "AT&T",
            "Crown Castle",
            "IX Reach",
            "Cox Business",
            "XO Communications",
        ],
        "availableMacSecPortSpeeds": ["100G", "10G"],
        "availableBillingModes": [
            {"billingMode": "PayAsYouGo", "availablePortSpeeds": ["100G", "1G", "10G"]},
            {
                "billingMode": "FlatRateTier1",
                "availablePortSpeeds": ["10G", "100G"],
                "includedRegions": ["us-west-1"],
            },
            {
                "billingMode": "FlatRateTier2",
                "availablePortSpeeds": ["10G", "100G"],
                "includedRegions": ["us-east-2", "eu-west-1", "us-east-1", "us-west-2"],
            },
            {
                "billingMode": "FlatRateTier3",
                "availablePortSpeeds": ["10G", "100G"],
                "includedRegions": [
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
            },
            {
                "billingMode": "FlatRateTier4",
                "availablePortSpeeds": ["10G", "100G"],
                "includedRegions": [
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
            },
            {
                "billingMode": "FlatRateTier5",
                "availablePortSpeeds": ["10G", "100G"],
                "includedRegions": [
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
            },
        ],
    });

    let carrier_hotel_partner = json!({
        "locationCode": "EqSe2-EQ",
        "locationName": "Westin Building Exchange, Seattle, WA | Equinix, SE2, Seattle, WA",
        "region": "us-west-2",
        "availablePortSpeeds": ["100G", "1G", "10G"],
        "availableProviders": [
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
        ],
        "availableMacSecPortSpeeds": ["100G", "10G"],
        "availableBillingModes": [
            {"billingMode": "PayAsYouGo", "availablePortSpeeds": ["100G", "1G", "10G"]},
        ],
    });

    let four_hundred_gig_meet_me_room = json!({
        "locationCode": "EqSe2-WBE",
        "locationName": "Westin Building Exchange, Seattle, WA | Westin Building Exchange - SEA10 - Main FMMR, Seattle, WA",
        "region": "us-west-2",
        "availablePortSpeeds": ["100G", "1G", "10G", "400G"],
        "availableMacSecPortSpeeds": ["100G", "10G", "400G"],
        "availableBillingModes": [
            {"billingMode": "PayAsYouGo", "availablePortSpeeds": ["100G", "1G", "10G", "400G"]},
        ],
    });

    let metro_site = json!({
        "locationCode": "TierPointSEA",
        "locationName": "TierPoint Seattle, Seattle, WA",
        "region": "us-west-2",
        "availablePortSpeeds": ["1G", "10G"],
        "availableProviders": ["Zayo Group", "CenturyLink", "AT&T", "Comcast"],
        // No MACsec here: the location lands no port MACsec is offered on.
        "availableMacSecPortSpeeds": [],
        // No flat-rate mode either: a 1G/10G site publishes the usage-based mode alone.
        "availableBillingModes": [
            {"billingMode": "PayAsYouGo", "availablePortSpeeds": ["1G", "10G"]},
        ],
    });

    for expected in [&las_vegas, &carrier_hotel_partner, &metro_site] {
        let case = expected["locationCode"].as_str().unwrap_or_default();
        let answered = location_of(&locations, case);
        assert_eq!(
            answered, *expected,
            "case {case:?}: expected the catalog's published location, got {answered:?}"
        );
    }

    // The 400G demarcation's own partner roster is the carrier hotel's, so only the
    // members this row is about are spelled out.
    let case = "EqSe2-WBE";
    let answered = location_of(&locations, case);
    for member in [
        "locationCode",
        "locationName",
        "region",
        "availablePortSpeeds",
        "availableMacSecPortSpeeds",
        "availableBillingModes",
    ] {
        assert_eq!(
            answered[member], four_hundred_gig_meet_me_room[member],
            "case {case:?}: expected {member} {:?}, got {:?}",
            four_hundred_gig_meet_me_room[member], answered[member]
        );
    }
    assert_eq!(
        answered["availableProviders"], carrier_hotel_partner["availableProviders"],
        "case {case:?}: both demarcations of one building publish its partners, got {:?}",
        answered["availableProviders"]
    );
}

#[tokio::test]
async fn a_location_publishes_the_flat_rate_modes_the_catalog_holds_for_it() {
    // One Region's locations, spanning the ladder: none, one tier, two, three, five,
    // and a port-pair tier in another Region — a location's modes are its own, not its
    // Region's.
    let dx = Emulator::in_region("us-west-2");
    let (status, locations) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {locations:?}");

    // (locationCode, the billing modes published there, in order)
    let rows: &[(&str, &[&str])] = &[
        ("EqSe2-EQ", &["PayAsYouGo"]),
        ("TierPointSEA", &["PayAsYouGo"]),
        ("EqSe3-CL", &["PayAsYouGo", "FlatRateTier1"]),
        (
            "EqSe3-EQ",
            &["PayAsYouGo", "FlatRateTier1", "FlatRateTier2"],
        ),
        (
            "T5PDX",
            &[
                "PayAsYouGo",
                "FlatRateTier1",
                "FlatRateTier2",
                "FlatRateTier3",
            ],
        ),
        (
            "SNAP",
            &[
                "PayAsYouGo",
                "FlatRateTier1",
                "FlatRateTier2",
                "FlatRateTier3",
                "FlatRateTier4",
                "FlatRateTier5",
            ],
        ),
    ];

    for (case, expected) in rows {
        let answered: Vec<String> = billing_modes_of(&locations, case)
            .iter()
            .map(|mode| mode["billingMode"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            answered, *expected,
            "case {case:?}: expected billing modes {expected:?}, got {answered:?}"
        );
    }

    // Two locations of ONE Region on the same tier publish the same `includedRegions`,
    // because that mapping is the catalog's, not each location's invention.
    let shared_tier = |code: &str| -> Value {
        billing_modes_of(&locations, code)
            .into_iter()
            .find(|mode| mode["billingMode"] == json!("FlatRateTier1"))
            .map(|mode| mode["includedRegions"].clone())
            .unwrap_or(Value::Null)
    };
    assert_eq!(
        shared_tier("SNAP"),
        shared_tier("EqSe3-CL"),
        "two locations of one Region publish one tier's Regions alike, got {:?} and {:?}",
        shared_tier("SNAP"),
        shared_tier("EqSe3-CL")
    );

    // A port-pair tier is a `BillingMode` too, and a location that publishes one is
    // answered with it.
    let dx = Emulator::in_region("eu-west-2");
    let (status, locations) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {locations:?}");
    let case = "EqLD5-EQ";
    let answered: Vec<String> = billing_modes_of(&locations, case)
        .iter()
        .map(|mode| mode["billingMode"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        answered,
        vec![
            "PayAsYouGo".to_string(),
            "FlatRateTier1".to_string(),
            "FlatRateTier2".to_string(),
            "PortPairFlatRateTier1".to_string(),
        ],
        "case {case:?}: a published port-pair tier is answered as published, got {answered:?}"
    );
}

/// One row of the round-trip table: the case, the record the catalog holds, and the
/// billing modes it must publish (`None` where only the usage-based mode is).
type RoundTripRow<'a> = (&'a str, LocationRecord, Vec<(&'a str, Vec<&'a str>)>);

/// A flat-rate mode a catalog record carries.
fn flat_rate(billing_mode: &str, included_regions: &[&str]) -> LocationFlatRateMode {
    LocationFlatRateMode {
        billing_mode: billing_mode.to_string(),
        included_regions: included_regions
            .iter()
            .map(|region| (*region).to_string())
            .collect(),
    }
}

/// A catalog record, written as the catalog holds one.
fn record(
    code: &str,
    name: &str,
    port_speeds: &[&str],
    offers_mac_sec: bool,
    providers: &[&str],
    flat_rate_modes: Vec<LocationFlatRateMode>,
) -> LocationRecord {
    LocationRecord {
        location_code: code.to_string(),
        location_name: name.to_string(),
        region: "us-west-2".to_string(),
        available_port_speeds: port_speeds
            .iter()
            .map(|speed| (*speed).to_string())
            .collect(),
        available_providers: providers
            .iter()
            .map(|provider| (*provider).to_string())
            .collect(),
        offers_mac_sec,
        flat_rate_modes,
    }
}

#[tokio::test]
async fn whatever_the_catalog_publishes_for_a_location_reaches_the_wire() {
    // The rule the catalog has to satisfy: a published value is ANSWERED, never
    // assembled. The rows below are the shapes a published location takes — an
    // unsuffixed code with the building alone as its name, a suffixed code with a
    // partner site, a one-partner roster, a tier whose Regions include the location's
    // own and span two continents in an order no geography walks, and a port-pair
    // tier — plus the two kinds of mode a client could not order and so is not shown.
    let rows: Vec<RoundTripRow<'_>> = vec![
        (
            "a building's own code and name, with no partner anywhere in them",
            record(
                "PROBE1",
                "Probe Exchange One, Nowhere, NV",
                &["1G", "10G", "100G"],
                true,
                &["Megaport"],
                vec![flat_rate("FlatRateTier4", &["us-west-2", "eu-west-1"])],
            ),
            vec![
                ("PayAsYouGo", vec!["1G", "10G", "100G"]),
                ("FlatRateTier4", vec!["10G", "100G"]),
            ],
        ),
        (
            "a partner demarcation: a suffixed code and a two-part name",
            record(
                "PROBE2-XYZ",
                "Probe Exchange Two, Nowhere, NV | XYZ Colo, Suite 400, Nowhere, NV",
                &["1G", "10G"],
                false,
                &["Megaport", "AT&T", "Crown Castle"],
                vec![flat_rate("FlatRateTier1", &["ap-south-1"])],
            ),
            vec![
                ("PayAsYouGo", vec!["1G", "10G"]),
                ("FlatRateTier1", vec!["10G"]),
            ],
        ),
        (
            "a port-pair tier beside a per-port tier",
            record(
                "PROBE3",
                "Probe Exchange Three, Nowhere, NV",
                &["10G", "100G"],
                true,
                &["Cox Business"],
                vec![
                    flat_rate("FlatRateTier2", &["sa-east-1", "us-west-2"]),
                    flat_rate("PortPairFlatRateTier5", &["eu-north-1"]),
                ],
            ),
            vec![
                ("PayAsYouGo", vec!["10G", "100G"]),
                ("FlatRateTier2", vec!["10G", "100G"]),
                ("PortPairFlatRateTier5", vec!["10G", "100G"]),
            ],
        ),
        (
            "a mode the modelled enum does not name, and one including no Region",
            record(
                "PROBE4",
                "Probe Exchange Four, Nowhere, NV",
                &["10G"],
                true,
                &["Telus"],
                vec![
                    flat_rate("FlatRateTier9", &["us-west-1"]),
                    flat_rate("FlatRateTier3", &[]),
                    flat_rate("FlatRateTier2", &["us-east-1"]),
                ],
            ),
            vec![
                ("PayAsYouGo", vec!["10G"]),
                ("FlatRateTier2", vec!["10G"]),
            ],
        ),
    ];

    for (case, held, expected_modes) in rows {
        let dx = Emulator::in_region("us-west-2");
        // No operation creates a location, so the record is put in the store the
        // handler reads — the catalog's own seam.
        {
            let state = dx
                .engine()
                .state::<ComAmazonawsDirectconnectOvertureserviceState>();
            let mut store = state.lock();
            store.locations.clear();
            store.location_order.clear();
            store.location_order.push(held.location_code.clone());
            store
                .locations
                .insert(held.location_code.clone(), held.clone());
        }

        let (status, locations) = describe_locations(&dx).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {locations:?}"
        );
        let answered = location_of(&locations, &held.location_code);

        assert_eq!(
            answered["locationCode"],
            json!(held.location_code),
            "case {case:?}: expected the code the catalog holds, got {:?}",
            answered["locationCode"]
        );
        assert_eq!(
            answered["locationName"],
            json!(held.location_name),
            "case {case:?}: expected the name the catalog holds, got {:?}",
            answered["locationName"]
        );
        assert_eq!(
            answered["availableProviders"],
            json!(held.available_providers),
            "case {case:?}: expected the partners the catalog holds, got {:?}",
            answered["availableProviders"]
        );

        let modes = answered["availableBillingModes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let names: Vec<String> = modes
            .iter()
            .map(|mode| mode["billingMode"].as_str().unwrap_or_default().to_string())
            .collect();
        let expected_names: Vec<String> = expected_modes
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        assert_eq!(
            names, expected_names,
            "case {case:?}: expected billing modes {expected_names:?}, got {modes:?}"
        );
        for (offset, (name, speeds)) in expected_modes.iter().enumerate() {
            assert_eq!(
                modes[offset]["availablePortSpeeds"],
                json!(speeds),
                "case {case:?}: expected {name} on {speeds:?}, got {:?}",
                modes[offset]["availablePortSpeeds"]
            );
        }
        // Every flat-rate mode's Regions are the ones the record holds, in that order.
        let held_regions: Vec<Vec<String>> = held
            .flat_rate_modes
            .iter()
            .filter(|mode| {
                BILLING_MODE_VALUES.contains(&mode.billing_mode.as_str())
                    && !mode.included_regions.is_empty()
            })
            .map(|mode| mode.included_regions.clone())
            .collect();
        let answered_regions: Vec<Vec<String>> = modes
            .iter()
            .skip(1)
            .map(|mode| {
                mode["includedRegions"]
                    .as_array()
                    .map(|regions| {
                        regions
                            .iter()
                            .map(|region| region.as_str().unwrap_or_default().to_string())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            answered_regions, held_regions,
            "case {case:?}: expected the Regions the catalog holds {held_regions:?}, got {answered_regions:?}"
        );
        // The usage-based mode includes no Region's transfer in a fixed fee.
        assert!(
            modes[0].get("includedRegions").is_none(),
            "case {case:?}: the usage-based mode publishes no includedRegions, got {:?}",
            modes[0]
        );
    }
}

#[tokio::test]
async fn every_published_location_keeps_its_own_lists_consistent() {
    let catalog = built_in_location_catalog();
    let regions: BTreeSet<&str> = catalog
        .iter()
        .map(|location| location.region.as_str())
        .collect();

    let mut unsuffixed_building_only = 0_usize;
    let mut suffixed_with_partner_site = 0_usize;
    let mut modes_per_location: BTreeMap<String, usize> = BTreeMap::new();

    for region in regions {
        let dx = Emulator::in_region(region);
        let (status, locations) = describe_locations(&dx).await;
        assert_eq!(
            status, 200,
            "case {region:?}: expected 200, got {status} with {locations:?}"
        );

        for location in &locations {
            let case = location["locationCode"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !case.is_empty(),
                "case {region:?}: every location publishes a code, got {location:?}"
            );
            let name = location["locationName"].as_str().unwrap_or_default();
            assert!(
                !name.is_empty(),
                "case {case:?}: every location publishes a name, got {location:?}"
            );
            assert_eq!(
                location["region"],
                json!(region),
                "case {case:?}: a location is answered in its own Region, got {:?}",
                location["region"]
            );

            // A code either carries a partner suffix, in which case the name names that
            // partner's site after the building, or it does not, in which case the name
            // is the building alone.
            if case.contains('-') {
                assert!(
                    name.contains(" | "),
                    "case {case:?}: a partner demarcation names the building and the partner's site, got {name:?}"
                );
                suffixed_with_partner_site += 1;
            } else {
                assert!(
                    !name.contains(" | "),
                    "case {case:?}: a location published for a whole building names the building alone, got {name:?}"
                );
                unsuffixed_building_only += 1;
            }

            let speeds: Vec<&str> = location["availablePortSpeeds"]
                .as_array()
                .map(|speeds| {
                    speeds
                        .iter()
                        .map(|speed| speed.as_str().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                !speeds.is_empty() && speeds.iter().all(|speed| DEDICATED_PORT_SPEEDS.contains(speed)),
                "case {case:?}: port speeds come from {DEDICATED_PORT_SPEEDS:?}, got {speeds:?}"
            );
            let mut known = DEDICATED_PORT_SPEEDS.iter();
            assert!(
                speeds.iter().all(|speed| known.any(|k| k == speed)),
                "case {case:?}: port speeds are published in {DEDICATED_PORT_SPEEDS:?} order without repeats, got {speeds:?}"
            );

            let mac_sec: Vec<&str> = location["availableMacSecPortSpeeds"]
                .as_array()
                .map(|speeds| {
                    speeds
                        .iter()
                        .map(|speed| speed.as_str().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            let narrowed: Vec<&str> = speeds
                .iter()
                .copied()
                .filter(|speed| MAC_SEC_PORT_SPEEDS.contains(speed))
                .collect();
            assert!(
                mac_sec.is_empty() || mac_sec == narrowed,
                "case {case:?}: MACsec speeds are this location's own speeds narrowed to {MAC_SEC_PORT_SPEEDS:?} ({narrowed:?}) or none, got {mac_sec:?}"
            );

            let providers: Vec<&str> = location["availableProviders"]
                .as_array()
                .map(|providers| {
                    providers
                        .iter()
                        .map(|provider| provider.as_str().unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                !providers.is_empty() && in_published_order(&providers),
                "case {case:?}: a location publishes its own partners in the catalog's order ({DIRECT_CONNECT_PROVIDERS:?}), got {providers:?}"
            );

            let modes = location["availableBillingModes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            assert_eq!(
                modes.first(),
                Some(&json!({
                    "billingMode": USAGE_BILLING_MODE,
                    "availablePortSpeeds": speeds,
                })),
                "case {case:?}: a location publishes the usage-based mode first, on its own port speeds and with no includedRegions, got {modes:?}"
            );
            let flat_rate_speeds: Vec<&str> = FLAT_RATE_PORT_SPEEDS
                .iter()
                .copied()
                .filter(|speed| speeds.contains(speed))
                .collect();
            for mode in modes.iter().skip(1) {
                let name = mode["billingMode"].as_str().unwrap_or_default();
                assert!(
                    BILLING_MODE_VALUES.contains(&name) && name != USAGE_BILLING_MODE,
                    "case {case:?}: a flat-rate mode names a modelled BillingMode other than the usage-based one, got {mode:?}"
                );
                assert_eq!(
                    mode["availablePortSpeeds"],
                    json!(flat_rate_speeds),
                    "case {case:?}: a flat-rate mode is offered on this location's own flat-rate ports {flat_rate_speeds:?}, got {:?}",
                    mode["availablePortSpeeds"]
                );
                let included = mode["includedRegions"].as_array().cloned().unwrap_or_default();
                assert!(
                    !included.is_empty(),
                    "case {case:?}: a flat-rate mode includes the Regions its fee covers, got {mode:?}"
                );
            }
            modes_per_location.insert(case, modes.len());
        }
    }

    // The catalog has to hold BOTH published shapes, or the rule above is vacuous.
    assert!(
        unsuffixed_building_only > 0 && suffixed_with_partner_site > 0,
        "the catalog publishes locations for whole buildings and for partner demarcations, got {unsuffixed_building_only} and {suffixed_with_partner_site}"
    );
    // Per location, not per Region: some locations offer flat-rate modes and some
    // offer the usage-based mode alone.
    let counts: BTreeSet<usize> = modes_per_location.values().copied().collect();
    assert!(
        counts.len() > 2 && counts.contains(&1),
        "flat-rate modes are per-location data, so the catalog must publish locations without one and locations with several, got {modes_per_location:?}"
    );
}

#[tokio::test]
async fn a_delivery_partner_is_published_only_where_it_delivers() {
    // `availableProviders` is the location's own roster: one location's partner need
    // not be another's, even in the same Region, and the roster admits a partner only
    // one location names.
    let dx = Emulator::in_region("us-west-2");
    let (status, locations) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {locations:?}");

    let providers_of = |code: &str| -> BTreeSet<String> {
        location_of(&locations, code)["availableProviders"]
            .as_array()
            .map(|providers| {
                providers
                    .iter()
                    .map(|provider| provider.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    // (case, the partner, the location publishing it, a location of the same Region
    // that does not)
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "a partner only the Las Vegas building names",
            "Crown Castle",
            "SNAP",
            "EqSe2-EQ",
        ),
        (
            "a second partner only that building names",
            "Cox Business",
            "SNAP",
            "TierPointSEA",
        ),
        (
            "a carrier-hotel partner the metro site does not reach",
            "Telstra",
            "EqSe2-EQ",
            "SNAP",
        ),
        (
            "a partner the carrier hotel reaches and the metro site does not",
            "Megaport",
            "EqSe3-EQ",
            "TierPointSEA",
        ),
    ];

    for (case, partner, publishing, not_publishing) in rows {
        assert!(
            providers_of(publishing).contains(*partner),
            "case {case:?}: {publishing} publishes {partner}, got {:?}",
            providers_of(publishing)
        );
        assert!(
            !providers_of(not_publishing).contains(*partner),
            "case {case:?}: {not_publishing} does not, got {:?}",
            providers_of(not_publishing)
        );
        assert!(
            DIRECT_CONNECT_PROVIDERS.contains(partner),
            "case {case:?}: the roster admits {partner}, got {DIRECT_CONNECT_PROVIDERS:?}"
        );
    }
}

#[tokio::test]
async fn describing_locations_changes_nothing_a_later_caller_sees() {
    let dx = Emulator::in_region("us-west-2");
    let (status, first) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {first:?}");

    // A create at one of those locations, then the same read again: the catalog is
    // location data, so neither the read nor the create touches it.
    let code = first[0]["locationCode"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (create_status, created) = dx
        .call(
            "CreateConnection",
            json!({"connectionName": "catalog-read", "location": &code, "bandwidth": "1Gbps"}),
        )
        .await;
    assert_eq!(
        create_status, 200,
        "setup: expected CreateConnection at {code:?} to succeed, got {create_status} with {created}"
    );

    let (status, again) = describe_locations(&dx).await;
    assert_eq!(status, 200, "expected 200, got {status} with {again:?}");
    assert_eq!(
        again, first,
        "case {code:?}: a second read must answer the same catalog, got {again:?}"
    );
}

#[tokio::test]
async fn an_empty_catalog_answers_200_with_an_empty_list() {
    let dx = Emulator::in_region("us-east-1");
    // No operation empties the catalog, so the store is emptied directly through the
    // shared engine — the same store the handler reads.
    dx.engine()
        .state::<ComAmazonawsDirectconnectOvertureserviceState>()
        .lock()
        .locations
        .clear();

    let (status, locations) = describe_locations(&dx).await;
    assert_eq!(
        status, 200,
        "an empty catalog is still a 200, got {status} with {locations:?}"
    );
    assert!(
        locations.is_empty(),
        "an empty catalog answers an empty list, got {locations:?}"
    );
}
