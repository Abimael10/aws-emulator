// SPDX-License-Identifier: CC0-1.0

//! The Availability Zone a REGISTRATION is stored under, and therefore the one every read of
//! it reports.
//!
//! RULE: `TargetDescription$AvailabilityZone` is "an Availability Zone or `all`. This
//! determines whether the target receives traffic from the load balancer nodes in the
//! specified Availability Zone or from all enabled Availability Zones for the load balancer …
//! if the target type is `ip` and the IP address is in a subnet of the VPC for the target
//! group, the Availability Zone is automatically detected … if the target type is `lambda`,
//! this parameter is optional and the only supported value is `all`"
//! (`smithy_query(query="doc",shape="TargetDescription",member="AvailabilityZone")`). So the
//! zone a registration carries is the one the group's network resolved for it, else the one
//! the request named — except for a target type whose targets sit in no one zone of that
//! network, where the member has a single supported value and the registration carries it
//! whether or not the request named it.
//!
//! The rows span what the rule generalises over: the group's target type, and whether the
//! request named a zone at all.

use crate::provisioning_loop::{account, action, call, emulator, first_text, members, p, REGION};
use crate::target_id_forms_and_reported_ports::target_group_on;

/// The one value the member supports for a target of a `lambda` target group.
const ALL_ZONES: &str = "all";

/// One row: the case, the group's `TargetType`, the target id, the zone the request names,
/// and the zone every later read of the registration must report.
struct ZoneRow {
    case: &'static str,
    target_type: &'static str,
    id: String,
    names_zone: Option<&'static str>,
    reports_zone: Option<&'static str>,
}

/// The `AvailabilityZone` of every registration the group at `arn` holds, in the order the
/// health read reports them.
async fn reported_zones(router: &smithy_server_runtime::MultiServiceRouter, arn: &str) -> Vec<Option<String>> {
    let mut form = action("DescribeTargetHealth");
    form.push(p("TargetGroupArn", arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "a health read must be served, got {} {}",
        reply.status, reply.body
    );
    members(&reply.body, "TargetHealthDescriptions")
        .iter()
        .map(|description| {
            let target = first_text(description, "Target").unwrap_or_default();
            first_text(&target, "AvailabilityZone")
        })
        .collect()
}

#[tokio::test]
async fn a_registration_reports_the_zone_its_target_type_and_request_decide() {
    let (router, _engine) = emulator();
    // A Lambda function ARN of this account, which is what a target of a `lambda` group is
    // written as.
    let lambda_target = format!("arn:aws:lambda:{REGION}:{}:function:greeter", account());

    let rows: [ZoneRow; 5] = [
        ZoneRow {
            case: "a lambda target with no zone named",
            target_type: "lambda",
            id: lambda_target.clone(),
            names_zone: None,
            // No subnet holds a Lambda function, so the registration carries the one value
            // the member supports for this target type rather than nothing.
            reports_zone: Some(ALL_ZONES),
        },
        ZoneRow {
            case: "a lambda target naming the one zone value it supports",
            target_type: "lambda",
            id: lambda_target.clone(),
            names_zone: Some(ALL_ZONES),
            reports_zone: Some(ALL_ZONES),
        },
        ZoneRow {
            case: "an address target with no zone named, in a network nothing reports",
            target_type: "ip",
            id: "10.0.0.7".to_string(),
            names_zone: None,
            reports_zone: None,
        },
        ZoneRow {
            case: "an address target naming its own zone",
            target_type: "ip",
            id: "10.0.0.8".to_string(),
            names_zone: Some("us-west-2a"),
            reports_zone: Some("us-west-2a"),
        },
        ZoneRow {
            case: "an instance target with no zone named",
            target_type: "instance",
            id: "i-0123456789abcdef0".to_string(),
            names_zone: None,
            reports_zone: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let arn = target_group_on(
            &router,
            row.case,
            &format!("tg-zone-{index}"),
            row.target_type,
            // A target type whose targets are invoked rather than connected to names
            // neither a protocol nor a port of its own.
            (row.target_type != "lambda").then_some("80"),
            None,
        )
        .await;

        let mut form = action("RegisterTargets");
        form.push(p("TargetGroupArn", &arn));
        form.push(p("Targets.member.1.Id", &row.id));
        if let Some(zone) = row.names_zone {
            form.push(p("Targets.member.1.AvailabilityZone", zone));
        }
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: the registration must be served, got {} {}",
            row.case, reply.status, reply.body
        );

        let reported = reported_zones(&router, &arn).await;
        assert_eq!(
            reported,
            vec![row.reports_zone.map(str::to_string)],
            "case {:?}: expected the stored zone {:?}, got {reported:?}",
            row.case,
            row.reports_zone
        );
    }
}
