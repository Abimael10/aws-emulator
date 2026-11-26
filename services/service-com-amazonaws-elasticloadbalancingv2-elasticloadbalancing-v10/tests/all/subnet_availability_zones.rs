// SPDX-License-Identifier: CC0-1.0

//! Every subnet a load balancer is placed in is in an Availability Zone of its OWN.
//!
//! "You can specify only one subnet per Availability Zone" (`Subnets`, `SubnetMappings`): a
//! load balancer carries one node per zone it is placed in, so two subnets of one zone ask
//! for a placement it cannot hold — `CreateLoadBalancer` and `SetSubnets` refuse it with the
//! modelled `InvalidConfigurationRequest`, and nothing is written.
//!
//! Which zone a subnet is in is EC2's to say, so this suite drives the two halves this crate
//! owns:
//!
//! * THE RULE ITSELF, tabled directly over what EC2 reports — how many subnets the request
//!   names, which two of them share a zone and in which positions, and the cases that are
//!   awkward to stand a zone up for (no EC2 to ask, a subnet EC2 reports no zone for);
//! * THE WIRE PATH with no EC2 mounted, where nothing is known about the subnets and so
//!   nothing is refused on them: the placement is served, and the zones are stood in for one
//!   per subnet, so a read never reports one zone twice.
//!
//! The whole rule end to end — subnets created through EC2 in one zone, and the create and
//! the replacement naming them both refused — needs both services on one engine and is
//! tabled in `core/router/tests/all/elbv2_one_subnet_per_availability_zone.rs`.

use crate::load_balancer_attributes::create_load_balancer;
use crate::provisioning_loop::{action, call, emulator, first_text, members, p};
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    check_subnet_zones, network::SubnetFacts, Refusal, ONE_SUBNET_PER_ZONE,
};

/// One subnet as EC2 reports it, reduced to what this rule reads off it: its id and the zone
/// it is in (`None` for a subnet EC2 reported no zone for).
fn subnet(subnet_id: &str, zone: Option<&str>) -> SubnetFacts {
    SubnetFacts {
        subnet_id: subnet_id.to_string(),
        vpc_id: Some("vpc-a".to_string()),
        availability_zone: zone.map(str::to_string),
        cidrs: Vec::new(),
    }
}

#[test]
fn a_placement_names_one_subnet_per_availability_zone() {
    // Each row: the case, the subnets EC2 reported for the ids the request named — in request
    // order — and whether the placement is accepted. The rows span what the rule generalises
    // over: how many subnets the request names, which two share a zone and in which
    // positions, and a subnet EC2 reports no zone for.
    let rows: Vec<(&str, Vec<SubnetFacts>, bool)> = vec![
        (
            "two subnets in two zones",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2b")),
            ],
            true,
        ),
        (
            "two subnets in one zone",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2a")),
            ],
            false,
        ),
        (
            "one subnet, which can share a zone with nothing",
            vec![subnet("subnet-1", Some("us-west-2a"))],
            true,
        ),
        (
            "three subnets in three zones",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2b")),
                subnet("subnet-3", Some("us-west-2c")),
            ],
            true,
        ),
        (
            "three subnets, the first two sharing a zone",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2a")),
                subnet("subnet-3", Some("us-west-2c")),
            ],
            false,
        ),
        (
            "three subnets, the outer two sharing a zone",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2b")),
                subnet("subnet-3", Some("us-west-2a")),
            ],
            false,
        ),
        (
            "three subnets, all in one zone",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", Some("us-west-2a")),
                subnet("subnet-3", Some("us-west-2a")),
            ],
            false,
        ),
        (
            // Nothing is known about a subnet EC2 reported no zone for, so nothing is
            // refused on it.
            "a subnet EC2 reports no zone for, beside one it does",
            vec![
                subnet("subnet-1", Some("us-west-2a")),
                subnet("subnet-2", None),
            ],
            true,
        ),
        (
            "every named subnet reported without a zone",
            vec![subnet("subnet-1", None), subnet("subnet-2", None)],
            true,
        ),
        ("no subnet at all", Vec::new(), true),
    ];
    for (case, subnets, accepted) in rows {
        let judged = check_subnet_zones(Some(&subnets));
        let want: Result<(), Refusal> = if accepted {
            Ok(())
        } else {
            Err(Refusal::Configuration(ONE_SUBNET_PER_ZONE.to_string()))
        };
        assert_eq!(
            judged, want,
            "case {case:?}: expected {want:?}, got {judged:?}"
        );
    }
}

#[test]
fn with_no_ec2_to_ask_a_placement_is_refused_on_nothing() {
    // No EC2 answered, so the subnets resolve to no facts at all and the rule has nothing to
    // compare: the placement is served, whichever zones its subnets may really be in.
    let judged = check_subnet_zones(None);
    assert_eq!(
        judged,
        Ok(()),
        "case no EC2 to ask: expected the placement to be served, got {judged:?}"
    );
}

#[tokio::test]
async fn with_no_ec2_a_created_load_balancer_reports_one_zone_per_subnet() {
    let (router, _engine) = emulator();
    // The rows differ in how many subnets the load balancer is placed in: with no EC2 to ask,
    // every id is one nothing is known about, so each is stood in for in a zone of its own
    // and a read never reports one zone twice.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("two subnets", vec!["subnet-zone-1", "subnet-zone-2"]),
        (
            "three subnets",
            vec!["subnet-zone-3", "subnet-zone-4", "subnet-zone-5"],
        ),
    ];
    for (index, (case, subnets)) in rows.iter().enumerate() {
        let mut fields: Vec<(&str, &str)> = vec![("Scheme", "internal")];
        let keys: Vec<String> = (1..=subnets.len())
            .map(|position| format!("Subnets.member.{position}"))
            .collect();
        for (key, subnet_id) in keys.iter().zip(subnets.iter()) {
            fields.push((key.as_str(), subnet_id));
        }
        let arn = create_load_balancer(&router, &format!("zone-standin-{index}"), &fields).await;

        let mut read = action("DescribeLoadBalancers");
        read.push(p("LoadBalancerArns.member.1", &arn));
        let described = call(&router, &read).await;
        let zones: Vec<String> = members(&described.body, "AvailabilityZones")
            .iter()
            .filter_map(|zone| first_text(zone, "ZoneName"))
            .collect();
        let mut distinct = zones.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            (zones.len(), distinct.len()),
            (subnets.len(), subnets.len()),
            "case {case:?}: expected one zone per subnet and no zone twice, got {zones:?}"
        );
    }
}
