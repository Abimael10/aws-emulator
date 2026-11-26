// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the Local Zone groups a Region publishes and the opt-in status this
//! account holds for them — `DescribeAvailabilityZones` and `ModifyAvailabilityZoneGroup` —
//! driven over the wire the service actually speaks: `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. a Region that publishes Local Zones reports them as `local-zone` zones of the group they
//!    belong to, each hanging off a standard zone of the Region (`ParentZoneName` /
//!    `ParentZoneId`) and advertising from the group's own network border group, while a
//!    standard Availability Zone keeps its `opt-in-not-required` status and no parent at all;
//! 2. a zone whose group the account has NOT opted in to is withheld from a describe that did
//!    not ask for every zone, and reported by one that did — "if you do not use this parameter,
//!    the results include only the zones for the Regions where you have chosen the option to opt
//!    in" (`AllAvailabilityZones`);
//! 3. `ModifyAvailabilityZoneGroup` moves that status for the GROUP, so every zone of the group
//!    reports it and the zones then appear in the default describe; asking for the status the
//!    account already holds changes nothing and still succeeds; and a group name this Region has
//!    none under is refused with EC2's own code and sentence for it.
//!
//! The rows differ along what the rules generalise over: which Region (one with two published
//! groups, one with one, one with none), which group, how many zones the group publishes, and
//! which opt-in status is asked for — so no row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::call;
use super::wire::{child_text, document, param, Param};

/// One zone as the describe reports it.
#[derive(Debug, Clone)]
struct Zone {
    /// `zoneName`.
    name: String,
    /// `groupName`.
    group: String,
    /// `networkBorderGroup`.
    border_group: String,
    /// `zoneType`.
    zone_type: String,
    /// `optInStatus`.
    opt_in_status: String,
    /// `parentZoneName`, for a zone that has a parent.
    parent_zone_name: Option<String>,
    /// `parentZoneId`, for the same.
    parent_zone_id: Option<String>,
}

/// The zones a `DescribeAvailabilityZones` body reports.
fn zones(case: &str, body: &str) -> Vec<Zone> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .filter_map(|node| {
            Some(Zone {
                name: child_text(node, "zoneName")?,
                group: child_text(node, "groupName").unwrap_or_default(),
                border_group: child_text(node, "networkBorderGroup").unwrap_or_default(),
                zone_type: child_text(node, "zoneType").unwrap_or_default(),
                opt_in_status: child_text(node, "optInStatus").unwrap_or_default(),
                parent_zone_name: child_text(node, "parentZoneName"),
                parent_zone_id: child_text(node, "parentZoneId"),
            })
        })
        .collect()
}

/// The zones `region` reports, with `params` as extra form fields.
async fn describe(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    params: &[Param],
) -> Vec<Zone> {
    let (status, body) = call(router, region, "DescribeAvailabilityZones", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAvailabilityZones, got {status} with \
         body {body}"
    );
    zones(case, &body)
}

/// The form field asking for every zone whatever its opt-in status.
fn all_zones() -> Vec<Param> {
    vec![param("AllAvailabilityZones", "true")]
}

/// RULE 1 and RULE 2: the Local Zones a Region publishes are reported only to a request that
/// asks for every zone while their group is not opted in, and they carry the members only such
/// a zone has.
#[tokio::test]
async fn local_zones_are_reported_to_a_request_that_asks_for_every_zone() {
    // (case, the Region, how many Local Zone groups it publishes)
    let cases: [(&str, &str, usize); 3] = [
        ("a Region publishing two groups", "us-west-2", 2),
        ("a Region publishing one group", "eu-central-1", 1),
        ("a Region publishing none", "ap-southeast-2", 0),
    ];

    for (case, region, group_count) in cases {
        let (router, _engine) = super::ec2_query::emulator();
        let groups = ec2::state::local_zone_group_names(region);
        assert_eq!(
            groups.len(),
            group_count,
            "case {case:?}: expected {group_count} published Local Zone group(s), got {groups:?}"
        );

        // The default describe reports the Region's standard zones alone: every Local Zone
        // belongs to a group this account has not opted in to.
        let default = describe(case, &router, region, &[]).await;
        assert!(
            default
                .iter()
                .all(|zone| zone.zone_type == "availability-zone"),
            "case {case:?}: a describe that did not ask for every zone reports only the \
             Region's Availability Zones, got {default:?}"
        );
        for zone in &default {
            assert_eq!(
                zone.opt_in_status, "opt-in-not-required",
                "case {case:?}: an Availability Zone needs no opt-in, got {zone:?}"
            );
            assert!(
                zone.parent_zone_name.is_none() && zone.parent_zone_id.is_none(),
                "case {case:?}: an Availability Zone has no parent zone, got {zone:?}"
            );
        }

        // Asking for every zone reports the Local Zones too, each in its group.
        let every = describe(case, &router, region, &all_zones()).await;
        let local: Vec<&Zone> = every
            .iter()
            .filter(|zone| zone.zone_type == "local-zone")
            .collect();
        assert_eq!(
            local.len(),
            every.len() - default.len(),
            "case {case:?}: every zone the default withheld is a Local Zone, got {every:?}"
        );
        for group in &groups {
            let of_group: Vec<&&Zone> = local.iter().filter(|zone| &zone.group == group).collect();
            assert!(
                !of_group.is_empty(),
                "case {case:?}: the published group {group} must report at least one zone, got \
                 {local:?}"
            );
            for zone in of_group {
                assert!(
                    zone.name.starts_with(group.as_str()),
                    "case {case:?}: a zone of {group} is named for its group, got {zone:?}"
                );
                assert_eq!(
                    zone.opt_in_status, "not-opted-in",
                    "case {case:?}: a group nothing opted in to reports not-opted-in, got \
                     {zone:?}"
                );
                assert_eq!(
                    &zone.border_group, group,
                    "case {case:?}: a Local Zone advertises from its own group, got {zone:?}"
                );
                let parent_name = zone.parent_zone_name.clone().unwrap_or_default();
                assert!(
                    parent_name.starts_with(region) && parent_name.len() > region.len(),
                    "case {case:?}: a Local Zone hangs off a standard zone of its Region, got \
                     {zone:?}"
                );
                assert!(
                    zone.parent_zone_id.is_some(),
                    "case {case:?}: a Local Zone reports its parent zone's id too, got {zone:?}"
                );
            }
        }
    }
}

/// RULE 3: opting a group in moves the status of every zone of that group and brings those
/// zones into the default describe; opting it out again takes them back out.
#[tokio::test]
async fn opting_a_zone_group_in_moves_every_zone_of_that_group() {
    let case = "us-west-2's Los Angeles group";
    let region = "us-west-2";
    let (router, _engine) = super::ec2_query::emulator();
    let groups = ec2::state::local_zone_group_names(region);
    let group = groups
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the Region publishes a Local Zone group"));
    let other = groups
        .get(1)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the Region publishes a second group"));

    // (case, the status asked for, is the group opted in afterwards?)
    let steps: [(&str, &str, bool); 4] = [
        ("opting in", "opted-in", true),
        ("opting in again, which changes nothing", "opted-in", true),
        ("opting out", "not-opted-in", false),
        ("opting out again", "not-opted-in", false),
    ];
    for (step, status_asked, opted_in) in steps {
        let (status, body) = call(
            &router,
            region,
            "ModifyAvailabilityZoneGroup",
            &[
                param("GroupName", &group),
                param("OptInStatus", status_asked),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?} / {step}: expected HTTP 200, got {status} with body {body}"
        );
        assert!(
            body.contains("<return>true</return>"),
            "case {case:?} / {step}: the modify reports the request succeeded, got {body}"
        );

        let expected_status = if opted_in { "opted-in" } else { "not-opted-in" };
        let every = describe(case, &router, region, &all_zones()).await;
        for zone in every.iter().filter(|zone| zone.group == group) {
            assert_eq!(
                zone.opt_in_status, expected_status,
                "case {case:?} / {step}: every zone of the group reports the status the \
                 account holds, got {zone:?}"
            );
        }
        // The OTHER group is untouched: the status is a fact of one group.
        for zone in every.iter().filter(|zone| zone.group == other) {
            assert_eq!(
                zone.opt_in_status, "not-opted-in",
                "case {case:?} / {step}: another group keeps its own status, got {zone:?}"
            );
        }
        // And the default describe reports the group's zones only while it is opted in.
        let default = describe(case, &router, region, &[]).await;
        let reported = default.iter().any(|zone| zone.group == group);
        assert_eq!(
            reported, opted_in,
            "case {case:?} / {step}: expected the group's zones in the default describe: \
             {opted_in}, got {reported}"
        );
    }
}

/// RULE 3: a group name this Region has no opt-in zone group under is refused with EC2's own
/// code and sentence for it, and nothing is opted in. The rows are the forms such a value can
/// take, including a group of ANOTHER Region and the Region's own standard zone group, whose
/// zones need no opt-in at all.
#[tokio::test]
async fn a_group_this_region_does_not_publish_is_refused() {
    let region = "us-west-2";
    let (router, _engine) = super::ec2_query::emulator();
    let elsewhere = ec2::state::local_zone_group_names("eu-central-1")
        .first()
        .cloned()
        .unwrap_or_default();
    let cases: [(&str, String); 4] = [
        ("free text", "not-a-zone-group".to_string()),
        ("a group of another Region", elsewhere),
        (
            "the Region's own standard zone group",
            service_common::aws_availability_zones::zone_group(region),
        ),
        ("a zone name rather than a group", format!("{region}a")),
    ];
    for (case, group_name) in cases {
        let (status, body) = call(
            &router,
            region,
            "ModifyAvailabilityZoneGroup",
            &[
                param("GroupName", &group_name),
                param("OptInStatus", "opted-in"),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            super::ec2_query::error_code(&body),
            ec2::state::INVALID_AZ_GROUP_NOT_FOUND,
            "case {case:?}: expected {}, got {body}",
            ec2::state::INVALID_AZ_GROUP_NOT_FOUND
        );
        assert_eq!(
            super::ec2_query::error_message(&body),
            ec2::state::INVALID_AZ_GROUP_MESSAGE,
            "case {case:?}: unexpected sentence, got {body}"
        );
        // Nothing was opted in: every zone of the Region still reports what it did.
        let every = describe(case, &router, region, &all_zones()).await;
        assert!(
            every
                .iter()
                .filter(|zone| zone.zone_type == "local-zone")
                .all(|zone| zone.opt_in_status == "not-opted-in"),
            "case {case:?}: a refused modify must opt nothing in, got {every:?}"
        );
    }
}
