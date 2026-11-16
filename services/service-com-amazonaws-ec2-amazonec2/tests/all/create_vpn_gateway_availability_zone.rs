// SPDX-License-Identifier: CC0-1.0

//! The `AvailabilityZone` a virtual private gateway is created in — driven over the wire the
//! service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: `CreateVpnGateway` creates the gateway in the zone the request names
//! when the Region has it, reports NO zone for a request that names none — "this field may be
//! empty or not returned" (`VpnGateway$AvailabilityZone`), which an EMPTY value names just as an
//! absent member does — and refuses a value that is no zone of the Region with EC2's own
//! sentence for that class: the fixed text, with the value the request sent and the zones the
//! Region offers as its variable parts, so a caller can recover by picking one of them.
//!
//! The rows differ along what the rule generalises over: every zone the Region has (by name),
//! an absent and an empty member, a zone of ANOTHER Region, a zone name of the Region with a
//! letter it does not have, and a value that is no zone name at all — driven in two Regions, so
//! nothing about the refusal is tied to one Region's zone set.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{param, Param};

/// The emulator the tests drive, reached only over the wire.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `CreateVpnGateway` call in `region` carrying `params`, as its status and body.
async fn create(
    router: &MultiServiceRouter,
    region: &str,
    params: &[Param],
) -> (u16, String) {
    let mut all = vec![param("Type", "ipsec.1")];
    all.extend(params.iter().cloned());
    super::ec2_query::call(router, region, "CreateVpnGateway", &all).await
}

/// The zone NAMES `DescribeAvailabilityZones` reports for `region` — read through the API
/// rather than written out here, so the suite compares the refusal against the one derivation
/// this emulator has of a Region's zones.
async fn zone_names(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, body) =
        super::ec2_query::call(router, region, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeAvailabilityZones for {region} must succeed, got {status}: {body}"
    );
    elements(&body, "zoneName")
}

/// The Regions the table is driven in: two Regions of different naming, so the zone set a
/// refusal publishes is the signed Region's own.
const REGIONS: [&str; 2] = ["us-west-2", "ap-southeast-2"];

/// A gateway is created in each zone the Region HAS, and reports that zone back.
#[tokio::test]
async fn a_gateway_is_created_in_every_zone_the_region_has() {
    let router = emulator();

    for region in REGIONS {
        for zone in zone_names(&router, region).await {
            let case = format!("{region}: the zone {zone}");
            let (status, body) =
                create(&router, region, &[param("AvailabilityZone", &zone)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            assert_eq!(
                element(&body, "availabilityZone").as_deref(),
                Some(zone.as_str()),
                "case {case:?}: expected the gateway to report the zone it was created in, \
                 got {body}"
            );
        }
    }
}

/// A request that names NO zone — the member absent, or carrying an empty value — is served,
/// and the gateway reports no zone at all.
#[tokio::test]
async fn a_request_that_names_no_zone_creates_a_gateway_without_one() {
    let router = emulator();

    for (case, params) in [
        ("the member absent", Vec::new()),
        ("the member empty", vec![param("AvailabilityZone", "")]),
    ] {
        let (status, body) = create(&router, REGIONS[0], &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "availabilityZone"),
            None,
            "case {case:?}: a gateway created without a zone reports none, got {body}"
        );
        let id = element(&body, "vpnGatewayId")
            .unwrap_or_else(|| panic!("case {case:?}: no vpnGatewayId in {body}"));
        let (status, read) = super::ec2_query::call(
            &router,
            REGIONS[0],
            "DescribeVpnGateways",
            &[param("VpnGatewayId.1", &id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeVpnGateways, got {status}: {read}"
        );
        assert_eq!(
            element(&read, "availabilityZone"),
            None,
            "case {case:?}: the stored gateway reports no zone either, got {read}"
        );
    }
}

/// A value that is no zone of the signed Region is refused with the sentence that publishes
/// the zones one CAN be created in, and nothing is created for it.
#[tokio::test]
async fn a_zone_the_region_does_not_have_is_refused_with_the_zones_it_offers() {
    let router = emulator();

    for region in REGIONS {
        let offered = zone_names(&router, region).await;
        // The rows: a zone of another Region, a zone name of this Region with a letter it does
        // not have, and a value that is no zone name at all.
        let other_region_zone = REGIONS
            .iter()
            .find(|named| **named != region)
            .map(|named| format!("{named}a"))
            .unwrap_or_default();
        let rows = [
            ("a zone of another Region", other_region_zone),
            ("a letter this Region has no zone for", format!("{region}z")),
            ("no zone name at all", format!("{region}-nozone")),
        ];
        for (row, value) in rows {
            let case = format!("{region}: {row}");
            let (status, body) =
                create(&router, region, &[param("AvailabilityZone", &value)]).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_PARAMETER_VALUE,
                "case {case:?}: expected InvalidParameterValue, got {body}"
            );
            assert_eq!(
                error_message(&body),
                format!(
                    "Value ({value}) for parameter availabilityZone is invalid. VpnConnection \
                     gateways can currently only be created in the following availability \
                     zones: {zones}.",
                    zones = offered.join(","),
                ),
                "case {case:?}: expected the sentence naming the value and the zones the \
                 Region offers, got {body}"
            );
        }

        // Nothing was created by any of those rows: the Region reports no gateway at all.
        let (status, read) =
            super::ec2_query::call(&router, region, "DescribeVpnGateways", &[]).await;
        assert_eq!(
            status, 200,
            "{region}: expected HTTP 200 from DescribeVpnGateways, got {status}: {read}"
        );
        assert!(
            elements(&read, "vpnGatewayId").is_empty(),
            "{region}: a refused create must store no gateway, got {read}"
        );
    }
}
