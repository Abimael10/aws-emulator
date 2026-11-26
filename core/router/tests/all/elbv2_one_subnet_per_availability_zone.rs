// SPDX-License-Identifier: CC0-1.0

//! A load balancer is attached to at most ONE subnet per Availability Zone.
//!
//! "You can specify only one subnet per Availability Zone" (`Subnets`, `SubnetMappings`): a
//! load balancer carries one node per zone it is placed in, so a placement naming two subnets
//! of one zone is a configuration no load balancer can hold — `CreateLoadBalancer` refuses it
//! with the modelled `InvalidConfigurationRequest` (HTTP 400) and creates nothing, and
//! `SetSubnets` refuses it and the load balancer keeps the subnets it had.
//!
//! Which zone a subnet is in is EC2's to say, so the rule is driven with both services on one
//! engine, here beside `elbv2_network_resolution.rs` (whose request plumbing this suite
//! reuses) rather than in the ELBv2 crate, where the rule itself is tabled over what EC2
//! reports (`tests/all/subnet_availability_zones.rs`).
//!
//! The table varies what the rule generalises over: how many subnets the request names, which
//! two of them share a zone and in which positions, whether the placement is spelled as
//! `Subnets` or as `SubnetMappings`, and whether the call is the create or the replacement.
//! Every subnet below is in the SAME VPC, so a shared zone is the only rule a refused row
//! breaks.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{ec2_ok, elbv2, Answer, BOOT_STACK_SIZE};
use crate::query_wire::{element, elements};

/// The awsQuery code of `InvalidConfigurationRequestException`, which the model binds to
/// HTTP 400 (`smithy_query(query="operation",operation="SetSubnets")`).
const INVALID_CONFIGURATION_REQUEST: &str = "InvalidConfigurationRequest";

/// The refusal Elastic Load Balancing words a placement naming two subnets of one zone with.
const ONE_SUBNET_PER_ZONE: &str =
    "A load balancer cannot be attached to multiple subnets in the same Availability Zone";

/// How a row spells the placement it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `Subnets.member.N`.
    Subnets,
    /// `SubnetMappings.member.N.SubnetId` — the other list the operation takes.
    Mappings,
}

/// The parameters one placement is spelled as, beside `LoadBalancerArn` or `Name`.
fn placement(spelling: Spelling, subnets: &[String]) -> Vec<(String, String)> {
    subnets
        .iter()
        .enumerate()
        .map(|(index, subnet)| {
            let position = index + 1;
            let key = match spelling {
                Spelling::Subnets => format!("Subnets.member.{position}"),
                Spelling::Mappings => format!("SubnetMappings.member.{position}.SubnetId"),
            };
            (key, subnet.clone())
        })
        .collect()
}

/// Drive one request whose parameters are owned strings.
fn call(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(String, String)],
) -> Answer {
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    elbv2(router, runtime, action, &borrowed)
}

/// The subnet ids `DescribeLoadBalancers` reports for the load balancer `arn` names.
fn placed_subnets(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    arn: &str,
) -> Vec<String> {
    let read = elbv2(
        router,
        runtime,
        "DescribeLoadBalancers",
        &[("LoadBalancerArns.member.1", arn)],
    );
    assert_eq!(
        read.status, 200,
        "the load balancer must be readable back, got {read:?}"
    );
    let mut subnets = elements(&read.text, "SubnetId");
    subnets.sort();
    subnets
}

/// The zone names `DescribeLoadBalancers` reports for the load balancer `arn` names.
fn placed_zones(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    arn: &str,
) -> Vec<String> {
    let read = elbv2(
        router,
        runtime,
        "DescribeLoadBalancers",
        &[("LoadBalancerArns.member.1", arn)],
    );
    let mut zones = elements(&read.text, "ZoneName");
    zones.sort();
    zones
}

/// One row of the replacement table: the case, which subnets the request names — as indices
/// into the suite's own subnets, one per zone plus a second one in the first zone — and
/// whether the placement is accepted.
struct Row {
    case: &'static str,
    named: &'static [usize],
    spelling: Spelling,
    accepted: bool,
}

/// Index 0, 1 and 2 are one subnet each in zones a, b and c; index 3 is a SECOND subnet in
/// zone a, so any row naming both 0 and 3 names two subnets of one zone.
const ROWS: &[Row] = &[
    Row {
        case: "two subnets in two zones",
        named: &[0, 1],
        spelling: Spelling::Subnets,
        accepted: true,
    },
    Row {
        case: "two subnets in one zone",
        named: &[0, 3],
        spelling: Spelling::Subnets,
        accepted: false,
    },
    Row {
        case: "the mirror: the second subnet of that zone named first",
        named: &[3, 0],
        spelling: Spelling::Subnets,
        accepted: false,
    },
    Row {
        case: "three subnets in three zones",
        named: &[0, 1, 2],
        spelling: Spelling::Subnets,
        accepted: true,
    },
    Row {
        case: "three subnets, two of them in one zone",
        named: &[0, 1, 3],
        spelling: Spelling::Subnets,
        accepted: false,
    },
    Row {
        case: "mappings naming two subnets of one zone",
        named: &[0, 3],
        spelling: Spelling::Mappings,
        accepted: false,
    },
    Row {
        case: "mappings naming one subnet per zone",
        named: &[1, 2],
        spelling: Spelling::Mappings,
        accepted: true,
    },
];

#[test]
fn a_placement_naming_two_subnets_of_one_zone_is_refused() {
    std::thread::Builder::new()
        .name("elbv2-one-subnet-per-zone".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers, as the sibling suites enable them: a service this
                // router mounts may open a loopback listener or wait on the clock on a
                // background task of the request it served.
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // One VPC, with one subnet in each of three zones and a SECOND subnet in the
            // first of them — created through EC2 itself, so every zone below is one EC2
            // reported.
            let vpc = element(
                &ec2_ok(
                    &router,
                    &runtime,
                    "CreateVpc",
                    &[("CidrBlock", "10.106.0.0/16")],
                ),
                "vpcId",
            );
            let zones = ["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2a"];
            let subnets: Vec<String> = zones
                .iter()
                .enumerate()
                .map(|(index, zone)| {
                    element(
                        &ec2_ok(
                            &router,
                            &runtime,
                            "CreateSubnet",
                            &[
                                ("VpcId", vpc.as_str()),
                                ("CidrBlock", &format!("10.106.{}.0/24", index + 1)),
                                ("AvailabilityZone", zone),
                            ],
                        ),
                        "subnetId",
                    )
                })
                .collect();

            // -----------------------------------------------------------------
            // A create naming two subnets of one zone places nothing.
            // -----------------------------------------------------------------
            // (case, the subnets the create names, whether it is accepted)
            let create_rows: Vec<(&str, Vec<String>, bool)> = vec![
                (
                    "a create in two subnets of two zones",
                    vec![subnets[0].clone(), subnets[1].clone()],
                    true,
                ),
                (
                    "a create in two subnets of one zone",
                    vec![subnets[0].clone(), subnets[3].clone()],
                    false,
                ),
                (
                    "a create in three subnets, two of them in one zone",
                    vec![subnets[3].clone(), subnets[1].clone(), subnets[0].clone()],
                    false,
                ),
            ];
            for (index, (case, named, accepted)) in create_rows.iter().enumerate() {
                let name = format!("one-subnet-per-zone-{index}");
                let mut params = vec![
                    ("Name".to_string(), name.clone()),
                    ("Scheme".to_string(), "internal".to_string()),
                ];
                params.extend(placement(Spelling::Subnets, named));
                let answer = call(&router, &runtime, "CreateLoadBalancer", &params);
                if *accepted {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: expected the create to be accepted, got {answer:?}"
                    );
                    continue;
                }
                assert_eq!(
                    (answer.status, answer.code(), answer.message()),
                    (
                        400,
                        INVALID_CONFIGURATION_REQUEST.to_string(),
                        ONE_SUBNET_PER_ZONE.to_string()
                    ),
                    "case {case:?}: expected 400 {INVALID_CONFIGURATION_REQUEST} \
                     {ONE_SUBNET_PER_ZONE:?}, got {answer:?}"
                );
                // A refused create stores nothing: the name is still free.
                let read = elbv2(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[("Names.member.1", name.as_str())],
                );
                assert_eq!(
                    (read.status, read.code()),
                    (400, "LoadBalancerNotFound".to_string()),
                    "case {case:?}: a refused create must store no load balancer, got {read:?}"
                );
            }

            // -----------------------------------------------------------------
            // Nor does a replacement attach one to two subnets of one zone.
            // -----------------------------------------------------------------
            let arn = element(
                &call(
                    &router,
                    &runtime,
                    "DescribeLoadBalancers",
                    &[(
                        "Names.member.1".to_string(),
                        "one-subnet-per-zone-0".to_string(),
                    )],
                )
                .text,
                "LoadBalancerArn",
            );
            for row in ROWS {
                let named: Vec<String> =
                    row.named.iter().map(|index| subnets[*index].clone()).collect();
                let before = placed_subnets(&router, &runtime, &arn);
                let mut params = vec![("LoadBalancerArn".to_string(), arn.clone())];
                params.extend(placement(row.spelling, &named));
                let answer = call(&router, &runtime, "SetSubnets", &params);
                let mut expected = named.clone();
                expected.sort();
                if row.accepted {
                    assert_eq!(
                        answer.status, 200,
                        "case {:?}: expected the replacement to be accepted, got {answer:?}",
                        row.case
                    );
                } else {
                    assert_eq!(
                        (answer.status, answer.code(), answer.message()),
                        (
                            400,
                            INVALID_CONFIGURATION_REQUEST.to_string(),
                            ONE_SUBNET_PER_ZONE.to_string()
                        ),
                        "case {:?}: expected 400 {INVALID_CONFIGURATION_REQUEST} \
                         {ONE_SUBNET_PER_ZONE:?}, got {answer:?}",
                        row.case
                    );
                    // A refused replacement leaves the load balancer where it was.
                    expected = before.clone();
                }
                let after = placed_subnets(&router, &runtime, &arn);
                assert_eq!(
                    after, expected,
                    "case {:?}: expected the load balancer to be placed in {expected:?}, got \
                     {after:?}",
                    row.case
                );
                // And every zone it reports is a zone of its own, whatever the row asked for.
                let reported = placed_zones(&router, &runtime, &arn);
                let mut distinct = reported.clone();
                distinct.dedup();
                assert_eq!(
                    reported, distinct,
                    "case {:?}: expected one zone per subnet, got {reported:?}",
                    row.case
                );
            }
        })
        .expect("the suite thread starts")
        .join()
        .expect("the suite thread completes");
}
