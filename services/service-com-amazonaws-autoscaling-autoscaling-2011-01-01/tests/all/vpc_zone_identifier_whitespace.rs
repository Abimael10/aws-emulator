// SPDX-License-Identifier: CC0-1.0

//! How a `VPCZoneIdentifier` value is split into the subnet ids it names.
//!
//! RULE under test: `VPCZoneIdentifier` is "a comma-separated list of subnet IDs"
//! (`smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="VPCZoneIdentifier")`),
//! so the ids are what the COMMAS separate and whitespace is not a separator: it is dropped
//! from the value, which leaves two ids a caller separated with whitespace alone run together
//! as ONE id — one that names no subnet, so the request is refused naming that one run-together
//! id. Whitespace around a comma, by the same rule, names exactly the subnets on either side of
//! it.
//!
//! The rows span what the rule generalises over: which whitespace character stands where a
//! comma belongs (a space, a tab, a newline), how many ids run together, and whitespace that
//! surrounds a comma or an id instead of replacing one — and the rule is checked on both
//! handlers that write a group's subnets, the create and the update, because the two resolve
//! the same value.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values, xml_unescape,
};

/// The launch source every group here is created from — the launch template the harness puts
/// in the account's own Amazon EC2 store, because no account can create a launch
/// configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The whole sentence a request naming a subnet the account does not hold is refused with,
/// written out here rather than taken from the service's own builder: a case that re-used the
/// implementation's literal would assert nothing about it.
///
/// SOURCE: Amazon EC2's `InvalidSubnetID.NotFound` sentence for an id it holds no subnet for,
/// which is the lookup this resolution is.
fn unknown_subnet_message(subnet_id: &str) -> String {
    format!("The subnet ID '{subnet_id}' does not exist")
}

/// One row: the `VPCZoneIdentifier` text, built from the ids the account holds and the
/// whitespace the caller wrote, and the ONE id the refusal must name.
struct RunTogether {
    case: &'static str,
    /// Rendered between the two subnet ids the account holds, in place of the comma.
    separator: &'static str,
    /// How many of the seeded subnets the value names, so a row can run three ids together.
    count: usize,
}

/// Whitespace where a comma belongs names ONE subnet id, the ids run together, and the request
/// is refused naming exactly that id — on the create and on the update alike.
#[tokio::test]
async fn whitespace_is_not_a_separator() {
    let rows = [
        RunTogether {
            case: "a space where the comma belongs",
            separator: " ",
            count: 2,
        },
        RunTogether {
            case: "a tab where the comma belongs",
            separator: "\t",
            count: 2,
        },
        RunTogether {
            case: "a newline where the comma belongs",
            separator: "\n",
            count: 2,
        },
        RunTogether {
            case: "several spaces where the comma belongs",
            separator: "   ",
            count: 2,
        },
        RunTogether {
            case: "three ids run together by whitespace",
            separator: " ",
            count: 3,
        },
    ];

    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 3);
    // The group the UPDATE rows move: it starts on one subnet the account holds, so a refused
    // update is seen to leave it exactly where it was.
    let standing = "whitespace-standing";
    let params = [
        p("AutoScalingGroupName", standing),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("VPCZoneIdentifier", subnets[0].id.clone()),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the standing group must be created, got {status}: {body}"
    );

    for (index, row) in rows.iter().enumerate() {
        let named: Vec<&str> = subnets
            .iter()
            .take(row.count)
            .map(|subnet| subnet.id.as_str())
            .collect();
        let identifier = named.join(row.separator);
        // Every whitespace character is dropped, so the ids the caller separated with it are
        // ONE id — the id the refusal names.
        let run_together = named.concat();

        // The create: refused, naming the run-together id, and leaving no group behind.
        let name = format!("whitespace-{index}");
        let params = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("VPCZoneIdentifier", identifier.clone()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {:?}: CreateAutoScalingGroup expected 400, got {status}: {body}",
            row.case
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            unknown_subnet_message(&run_together),
            "case {:?}: the refusal must name the one run-together id, got {body}",
            row.case
        );
        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        assert!(
            tag_values(&body, "AutoScalingGroupName").is_empty(),
            "case {:?}: a refused create must leave no group behind, got {body}",
            row.case
        );

        // The update: the same value, the same refusal, and the group's own subnets untouched.
        let params = [
            p("AutoScalingGroupName", standing),
            p("VPCZoneIdentifier", identifier.clone()),
        ];
        let (status, body) = query(&router, "UpdateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 400,
            "case {:?}: UpdateAutoScalingGroup expected 400, got {status}: {body}",
            row.case
        );
        assert_eq!(
            xml_unescape(&tag_value(&body, "Message")),
            unknown_subnet_message(&run_together),
            "case {:?}: the update's refusal must name the one run-together id, got {body}",
            row.case
        );
        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", standing)],
        )
        .await;
        assert_eq!(
            tag_value(&body, "VPCZoneIdentifier"),
            subnets[0].id,
            "case {:?}: a refused update must leave the group's subnets alone, got {body}",
            row.case
        );
    }
}

/// One row: the `VPCZoneIdentifier` text and the zones the created group must report — the
/// control the table above is read against, where the whitespace surrounds a comma or an id
/// instead of replacing one.
struct Separated {
    case: &'static str,
    /// Rendered between the two subnet ids, comma included.
    separator: &'static str,
    /// Written before the first id and after the last.
    around: &'static str,
}

/// Whitespace that surrounds a comma, or the whole value, separates nothing of its own: the
/// value names the subnets on either side of each comma and the group is created in their
/// zones.
#[tokio::test]
async fn whitespace_around_the_commas_names_the_subnets_it_surrounds() {
    let rows = [
        Separated {
            case: "no whitespace at all",
            separator: ",",
            around: "",
        },
        Separated {
            case: "spaces around the comma",
            separator: " , ",
            around: "",
        },
        Separated {
            case: "a newline after the comma",
            separator: ",\n",
            around: "",
        },
        Separated {
            case: "whitespace around the whole value",
            separator: ",",
            around: " ",
        },
    ];

    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 2);

    for (index, row) in rows.iter().enumerate() {
        let identifier = format!(
            "{}{}{}{}{}",
            row.around, subnets[0].id, row.separator, subnets[1].id, row.around
        );
        let name = format!("separated-{index}");
        let params = [
            p("AutoScalingGroupName", name.clone()),
            p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("VPCZoneIdentifier", identifier.clone()),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateAutoScalingGroup expected 200, got {status}: {body}",
            row.case
        );

        let (_, body) = query(
            &router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name.as_str())],
        )
        .await;
        let reported: Vec<String> = tag_values(&tag_value(&body, "AvailabilityZones"), "member");
        assert_eq!(
            reported,
            vec![subnets[0].zone.clone(), subnets[1].zone.clone()],
            "case {:?}: the group must be in the zones of the two subnets it named, got {body}",
            row.case
        );
    }
}
