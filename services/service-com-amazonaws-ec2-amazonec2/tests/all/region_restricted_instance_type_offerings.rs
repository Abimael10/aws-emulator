// SPDX-License-Identifier: CC0-1.0

//! Which instance types a Region is OFFERED, driven over the wire this service speaks
//! (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML; README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the offering set of a Region is published per Region, so a type AWS has
//! not installed in a Region is reported as offered in NONE of that Region's locations —
//! whichever kind the request names — while the rest of the catalog is offered wherever its
//! locations are. A client picks an instance type out of this report and then launches it, so a
//! type reported as offered that the Region does not have is a launch that fails after the
//! choice was made.
//!
//! The rows span the class: the whole-host sizes of two families whose published availability
//! names no Region for them, the VIRTUALIZED sizes of those same families at the same size
//! multipliers — which are offered, so the restriction is per TYPE and not per family — and a
//! previous generation type every Region has had since it was built. Each row is asked of two
//! Regions and of all three location kinds a Region has (itself, its Availability Zones by name
//! and by id), because the restriction is a fact of the Region rather than of how a request
//! names its locations.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, filters};
use super::wire;

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The `(instanceType, locationType, location)` of every offering a filtered describe reports.
async fn offerings(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    location_type: &str,
    instance_type: &str,
) -> Vec<(String, String, String)> {
    let mut params = filters(&[("instance-type", &[instance_type])]);
    params.push(param("LocationType", location_type));
    let (status, body) = call(router, region, "DescribeInstanceTypeOfferings", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    let document = wire::document(case, &body);
    let root = document.root_element();
    wire::set_items(root, "instanceTypeOfferingSet")
        .into_iter()
        .map(|item| {
            (
                wire::child_text(item, "instanceType").unwrap_or_default(),
                wire::child_text(item, "locationType").unwrap_or_default(),
                wire::child_text(item, "location").unwrap_or_default(),
            )
        })
        .collect()
}

/// Rule: a Region offers exactly the types AWS publishes for it, in every kind of location.
#[tokio::test]
async fn a_region_offers_only_the_instance_types_it_is_published_with() {
    let (router, _engine) = super::ec2_query::emulator();

    // (the type, whether any location of a Region offers it)
    let rows: [(&str, bool); 7] = [
        ("c8gb.metal-24xl", false),
        ("c8gb.metal-48xl", false),
        ("c8gn.metal-24xl", false),
        ("c8gn.metal-48xl", false),
        ("c8gb.24xlarge", true),
        ("c8gn.16xlarge", true),
        ("c3.large", true),
    ];

    for region in ["us-west-2", "us-east-1"] {
        for location_type in ["region", "availability-zone", "availability-zone-id"] {
            for (instance_type, offered) in rows {
                let case = format!("{region}/{location_type}/{instance_type}");
                let reported =
                    offerings(&case, &router, region, location_type, instance_type).await;
                assert_eq!(
                    reported.is_empty(),
                    !offered,
                    "case {case:?}: expected the type to be offered: {offered}, got \
                     {reported:?}"
                );
                for (reported_type, reported_location_type, location) in &reported {
                    assert_eq!(
                        reported_type, instance_type,
                        "case {case:?}: the filtered report carries only the type asked for, \
                         got {reported_type:?}"
                    );
                    assert_eq!(
                        reported_location_type, location_type,
                        "case {case:?}: an offering is reported as the location kind asked \
                         for, got {reported_location_type:?}"
                    );
                    assert!(
                        !location.is_empty(),
                        "case {case:?}: an offering names the location that offers the type, \
                         got {location:?}"
                    );
                }
            }
        }
    }
}
