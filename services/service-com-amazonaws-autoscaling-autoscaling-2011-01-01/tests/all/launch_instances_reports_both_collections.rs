// SPDX-License-Identifier: CC0-1.0

//! Every `LaunchInstances` answer REPORTS both of the collections it is modelled to carry.
//!
//! "Returns instance IDs and other details if launch is successful or error details if launch
//! is unsuccessful" (`smithy_query(query="operation",operation="LaunchInstances")`): the
//! answer's `Instances` and `Errors` members are how a client finds out what a launch did, so
//! an answer reports BOTH of them — each holding what it holds. A collection with nothing in
//! it is an EMPTY list in the answer, never a member left out of it: "no error" and "no
//! answer about errors" are different things, and a client that reads the member to find the
//! units a launch could not place must find it on the happy path too.
//!
//! The table below spans what that rule generalises over: which of the two collections is the
//! empty one (a launch that placed everything, and one that placed nothing), how many units
//! the launch was asked for, how the group is placed (by subnet or by Availability Zone
//! alone), and whether the answer is the launch's own or the one a repeat of its client token
//! is given.

use crate::support::{
    borrowed, create_launch_template, element_body, emulator, p, query, seed_subnets, tag_values,
};

/// The launch template every group here launches from.
const TEMPLATE: &str = "lt-both-collections";

/// Whether an answer reports the member `tag` AT ALL, in either spelling an empty list may be
/// written with on the wire (`<Tag></Tag>` or `<Tag/>`) — both of which a client parses as an
/// empty list, where a missing element is no member at all.
fn reports_member(xml: &str, tag: &str) -> bool {
    let open = format!("<{tag}");
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        // What follows the NAME decides: `>` closes the open tag, `/` closes an empty element
        // and whitespace begins its attributes, while anything else means the match was a
        // longer tag name that merely begins with this one.
        if after.starts_with(['>', '/']) || after.starts_with(char::is_whitespace) {
            return true;
        }
        rest = after;
    }
    false
}

/// Every instance id one answer reports, across the collections it carries.
fn reported_instance_ids(xml: &str) -> Vec<String> {
    tag_values(xml, "InstanceIds")
        .iter()
        .flat_map(|ids| tag_values(ids, "member"))
        .collect()
}

/// The error codes one answer reports, read INSIDE its `Errors` member so an envelope's own
/// error never counts as one of them.
fn reported_error_codes(xml: &str) -> Vec<String> {
    element_body(xml, "Errors")
        .map(|errors| tag_values(errors, "ErrorCode"))
        .unwrap_or_default()
}

/// One row of the table: a group, what the launch asks of it, and the two collections its
/// answer reports.
struct Row {
    case: &'static str,
    /// Whether the group is placed by subnet (and the launch names that subnet), or by
    /// Availability Zone alone (and the launch names no placement, which a group with one
    /// zone allows).
    by_subnet: bool,
    /// Whether the group has suspended `Launch`, which is what makes a launch place nothing.
    suspend_launch: bool,
    capacity: i32,
    expected_instances: usize,
    /// How many error entries the answer reports — one per PLACEMENT nothing was placed in,
    /// as the collections the answer carries are grouped.
    expected_errors: usize,
}

#[tokio::test]
async fn every_launch_answer_reports_its_instances_and_its_errors() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    create_launch_template(&router, TEMPLATE).await;
    let zone = subnets[0].zone.clone();

    let rows = [
        Row {
            case: "a launch that placed the one unit it was asked for",
            by_subnet: true,
            suspend_launch: false,
            capacity: 1,
            expected_instances: 1,
            expected_errors: 0,
        },
        Row {
            case: "a launch that placed several units",
            by_subnet: true,
            suspend_launch: false,
            capacity: 3,
            expected_instances: 3,
            expected_errors: 0,
        },
        Row {
            case: "a launch into a group placed by Availability Zone alone",
            by_subnet: false,
            suspend_launch: false,
            capacity: 2,
            expected_instances: 2,
            expected_errors: 0,
        },
        Row {
            case: "a launch that placed nothing, because the group suspended Launch",
            by_subnet: true,
            suspend_launch: true,
            capacity: 2,
            expected_instances: 0,
            expected_errors: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let name = format!("asg-both-collections-{index}");
        let mut create = vec![
            p("AutoScalingGroupName", name.clone()),
            p("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            p("MinSize", "0"),
            p("MaxSize", "10"),
            p("DesiredCapacity", "0"),
        ];
        if row.by_subnet {
            create.push(p("VPCZoneIdentifier", subnets[0].id.clone()));
        } else {
            create.push(p("AvailabilityZones.member.1", zone.clone()));
        }
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&create)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        if row.suspend_launch {
            let (status, body) = query(
                &router,
                "SuspendProcesses",
                &[
                    ("AutoScalingGroupName", name.as_str()),
                    ("ScalingProcesses.member.1", "Launch"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: SuspendProcesses expected 200, got {status}: {body}",
                row.case
            );
        }

        let mut launch = vec![
            p("AutoScalingGroupName", name.clone()),
            p("RequestedCapacity", row.capacity.to_string()),
            p("ClientToken", format!("both-collections-{index}")),
        ];
        if row.by_subnet {
            launch.push(p("SubnetIds.member.1", subnets[0].id.clone()));
        }

        // The launch itself, and the answer a repeat of its own client token is given: ONE
        // launch, so both answers report the same two collections.
        let mut first: Option<Vec<String>> = None;
        for attempt in ["the launch", "a repeat of its client token"] {
            let (status, body) = query(&router, "LaunchInstances", &borrowed(&launch)).await;
            assert_eq!(
                status, 200,
                "case {:?} ({attempt}): expected 200, got {status}: {body}",
                row.case
            );
            assert!(
                reports_member(&body, "Instances"),
                "case {:?} ({attempt}): expected the Instances member to be reported, got {body}",
                row.case
            );
            assert!(
                reports_member(&body, "Errors"),
                "case {:?} ({attempt}): expected the Errors member to be reported, got {body}",
                row.case
            );

            let ids = reported_instance_ids(&body);
            assert_eq!(
                ids.len(),
                row.expected_instances,
                "case {:?} ({attempt}): expected {} launched instances, got {ids:?} in {body}",
                row.case,
                row.expected_instances
            );
            let codes = reported_error_codes(&body);
            assert_eq!(
                codes.len(),
                row.expected_errors,
                "case {:?} ({attempt}): expected {} launch errors, got {codes:?} in {body}",
                row.case,
                row.expected_errors
            );

            // A collection with nothing in it is reported as an EMPTY list: the member holds
            // no entry at all, so a client reads it as `[]` rather than as one blank entry.
            if row.expected_errors == 0 {
                let errors = element_body(&body, "Errors").unwrap_or_default();
                assert!(
                    errors.trim().is_empty(),
                    "case {:?} ({attempt}): an empty Errors list holds no entry, got \
                     {errors:?} in {body}",
                    row.case
                );
            }
            if row.expected_instances == 0 {
                let instances = element_body(&body, "Instances").unwrap_or_default();
                assert!(
                    instances.trim().is_empty(),
                    "case {:?} ({attempt}): an empty Instances list holds no entry, got \
                     {instances:?} in {body}",
                    row.case
                );
            }

            match &first {
                None => first = Some(ids),
                Some(launched) => assert_eq!(
                    &ids, launched,
                    "case {:?} ({attempt}): a repeat of one client token answers the launch it \
                     already made, got {body}",
                    row.case
                ),
            }
        }
    }
}
