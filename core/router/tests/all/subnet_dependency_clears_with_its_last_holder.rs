// SPDX-License-Identifier: CC0-1.0

//! WHEN a subnet of the account becomes deletable again, over a subnet several resources sit
//! in at once.
//!
//! RULE: `ec2:DeleteSubnet` is refused while some resource still holds a place in the subnet,
//! and that refusal is TRANSIENT because every holder's own delete gives back exactly the
//! places THAT holder took and no others — the service-managed elastic network interfaces a
//! load balancer holds (one per attached subnet, under the load balancing service's requester
//! alias) go with that load balancer, and with no other resource sitting in the same subnet —
//! so the subnet's delete answers 200 as soon as its LAST holder is gone, and the VPC around
//! it follows.
//!
//! Why it matters: a client tearing a network down deletes what it put in a subnet, polls the
//! inventory until the subnet's holders drain and then deletes the subnet. If a delete gives
//! back none of the places its resource took, or gives back another resource's, the poll runs
//! to its own cap, every later `DeleteSubnet` keeps being refused, and the teardown never
//! terminates — the client's whole destroy run stalls and the account's subnets and VPCs leak
//! between runs. One load balancer alone is not what settles this: the question is whether the
//! refusal is keyed to the holders that are actually still there.
//!
//! The rows span what the rule generalises over: HOW MANY holders share the subnets (two,
//! three), WHAT KIND each holder is (load balancers of different families, each with its own
//! modelled interface type, and an interface the caller created itself), and WHICH subnets of
//! the network each holder occupies (all of them, or only one) — so the drain is asserted over
//! a dependency set that empties one holder at a time rather than all at once.
//!
//! `elbv2_teardown_frees_the_subnets_its_interfaces_held.rs` asserts the single-holder
//! teardown of one load balancer per family, and this suite takes its two-service harness —
//! `emulator`, `Reply`, the `ec2`/`elbv2` callers, `seed_network`, `service_interface_ids` and
//! the `interfaces_until` poll — from there rather than keeping a second copy of it. What this
//! suite adds is the SHARED subnet, where the refusal has to survive some holders going and
//! stop exactly when the last one does. The fact spans two services — Elastic Load Balancing
//! places and releases the interfaces, EC2 holds the inventory and decides whether a subnet
//! still has dependencies — so the suite lives beside the router. Nothing reaches into either
//! service's state struct: every assertion is a wire answer.

use std::collections::BTreeSet;
use std::time::Instant;

use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_teardown_frees_the_subnets_its_interfaces_held::{
    ec2, ec2_call, elbv2, elbv2_call, emulator, seed_network, service_interface_ids, Network,
    DRAIN_DEADLINE, LOOK_AGAIN_AFTER, SUBNET_IN_USE_CODE,
};
use crate::query_wire::{element, elements};

/// Every interface EC2 reports in `vpc`, whoever created it — the read that answers whether
/// anything at all still sits in the network, which is what this suite's holders have to be
/// read against: an interface of the CALLER is in the VPC too, and no filter naming the load
/// balancing service selects it.
async fn interface_ids(router: &MultiServiceRouter, vpc: &str) -> BTreeSet<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc)],
    )
    .await;
    elements(&body, "networkInterfaceId").into_iter().collect()
}

/// Look again until the interfaces of `vpc` are exactly `expected`, and fail naming what was
/// last seen once the shared deadline has passed.
///
/// The EXACT-SET counterpart of the imported `interfaces_until`, which waits on a predicate
/// over the service's own interfaces: here what has to hold after each delete is that the
/// inventory holds the remaining holders' interfaces and nothing else. It waits for the
/// CONDITION, so a slower host only looks more times.
async fn interfaces_settle(
    router: &MultiServiceRouter,
    case: &str,
    step: &str,
    vpc: &str,
    expected: &BTreeSet<String>,
) {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    loop {
        let held = interface_ids(router, vpc).await;
        if held == *expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: after {step} the VPC must hold exactly {expected:?}, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// What one holder of a row is, and so what takes it away again.
enum Holder {
    /// A load balancer of this family (`None` is the default, application family), attached to
    /// EVERY subnet of the row's network — so it holds one interface in each of them.
    LoadBalancer(Option<&'static str>),
    /// An interface the CALLER created in the row's first subnet: a holder that belongs to no
    /// service, so no load balancer's delete may give it back.
    CallerInterface,
}

/// One row: the holders that share a network's subnets, in the order they are created and then
/// deleted.
struct Row {
    case: &'static str,
    /// Part of every name this row mints, so no row is answered another's idempotent repeat.
    prefix: &'static str,
    /// One subnet per zone. Two zones at least: "[Application Load Balancers] You must specify
    /// subnets from at least two Availability Zones" (`CreateLoadBalancerInput$Subnets`).
    zones: &'static [&'static str],
    holders: &'static [Holder],
}

/// The rows span how many holders share the subnets, what kind each is, and how much of the
/// network each one occupies.
const ROWS: &[Row] = &[
    Row {
        case: "two application load balancers over the same two subnets",
        prefix: "shared-two-alb",
        zones: &["us-east-1a", "us-east-1b"],
        holders: &[Holder::LoadBalancer(None), Holder::LoadBalancer(None)],
    },
    Row {
        case: "an application and a network load balancer over the same two subnets",
        prefix: "shared-mixed",
        zones: &["us-east-1a", "us-east-1b"],
        holders: &[
            Holder::LoadBalancer(None),
            Holder::LoadBalancer(Some("network")),
        ],
    },
    Row {
        case: "three load balancers of three families over the same two subnets",
        prefix: "shared-three",
        zones: &["us-east-1a", "us-east-1b"],
        holders: &[
            Holder::LoadBalancer(None),
            Holder::LoadBalancer(Some("network")),
            Holder::LoadBalancer(Some("gateway")),
        ],
    },
    Row {
        case: "a load balancer sharing its first subnet with an interface of the caller",
        prefix: "shared-caller",
        zones: &["us-east-1a", "us-east-1b"],
        holders: &[Holder::LoadBalancer(None), Holder::CallerInterface],
    },
    Row {
        case: "two application load balancers over three subnets in three zones",
        prefix: "shared-wide",
        zones: &["us-east-1a", "us-east-1b", "us-east-1c"],
        holders: &[Holder::LoadBalancer(None), Holder::LoadBalancer(None)],
    },
];

/// A holder as it stands once created: how it is taken away, which interfaces of the VPC are
/// its own, and which of the row's subnets it sits in.
struct Placed {
    label: String,
    /// The delete that takes this holder away, as the action and its one member.
    delete: (&'static str, (&'static str, String)),
    interfaces: BTreeSet<String>,
    subnets: BTreeSet<String>,
}

/// Create one holder and read back, off the wire, exactly which interfaces of the VPC it added.
async fn place(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    network: &Network,
    index: usize,
    holder: &Holder,
) -> Placed {
    let before = interface_ids(router, &network.vpc).await;
    let (label, delete, subnets) = match holder {
        Holder::LoadBalancer(lb_type) => {
            let name = format!("{}-{index}", row.prefix);
            let mut members: Vec<(String, String)> = vec![("Name".to_string(), name.clone())];
            match lb_type {
                // "You can't specify a scheme for a Gateway Load Balancer"
                // (`CreateLoadBalancerInput$Scheme`), and that family carries no security
                // groups either (`CreateLoadBalancerInput$SecurityGroups`).
                Some(family @ "gateway") => {
                    members.push(("Type".to_string(), (*family).to_string()));
                }
                // An internal placement for every other family: "the nodes of an internal load
                // balancer have only private IP addresses"
                // (`CreateLoadBalancerInput$Scheme`), so the row needs no internet gateway for
                // the placement to be accepted.
                other => {
                    members.push(("Scheme".to_string(), "internal".to_string()));
                    if let Some(family) = other {
                        members.push(("Type".to_string(), (*family).to_string()));
                    }
                    members.push((
                        "SecurityGroups.member.1".to_string(),
                        network.security_group.clone(),
                    ));
                }
            }
            for (position, subnet) in network.subnets.iter().enumerate() {
                members.push((format!("Subnets.member.{}", position + 1), subnet.clone()));
            }
            let borrowed: Vec<(&str, &str)> = members
                .iter()
                .map(|(member, value)| (member.as_str(), value.as_str()))
                .collect();
            let created = elbv2(router, "CreateLoadBalancer", &borrowed).await;
            let arn = element(&created, "LoadBalancerArn");
            (
                format!("load balancer {name}"),
                ("DeleteLoadBalancer", ("LoadBalancerArn", arn)),
                network
                    .subnets
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<String>>(),
            )
        }
        Holder::CallerInterface => {
            let created = ec2(
                router,
                "CreateNetworkInterface",
                &[
                    ("SubnetId", network.subnets[0].as_str()),
                    ("SecurityGroupId.1", network.security_group.as_str()),
                    ("Description", "a workload of the account"),
                ],
            )
            .await;
            let id = element(&created, "networkInterfaceId");
            (
                format!("caller interface {id}"),
                ("DeleteNetworkInterface", ("NetworkInterfaceId", id)),
                BTreeSet::from([network.subnets[0].clone()]),
            )
        }
    };
    let after = interface_ids(router, &network.vpc).await;
    let interfaces: BTreeSet<String> = after.difference(&before).cloned().collect();
    assert!(
        !interfaces.is_empty(),
        "case {case:?}: creating {label} must put at least one interface in the VPC, the \
         inventory still holds {before:?}"
    );
    Placed {
        label,
        delete,
        interfaces,
        subnets,
    }
}

/// Which of `subnets` are still occupied by some member of `holders`.
fn still_held<'a>(holders: &[&Placed], subnets: &'a [String]) -> Vec<&'a String> {
    subnets
        .iter()
        .filter(|subnet| {
            holders
                .iter()
                .any(|holder| holder.subnets.contains(*subnet))
        })
        .collect()
}

#[tokio::test]
async fn a_subnet_deletes_once_its_last_holder_is_gone() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        // One network per row, each at its own CIDR so no row can be handed another's address
        // space; a row index that did not fit the octet would silently collide, so it fails
        // here instead.
        let octet = 60 + u8::try_from(index).expect("the row index fits an octet");
        let network = seed_network(&router, octet, row.zones, row.prefix, false).await;

        // Every holder of the row, created in order, each one's interfaces read off the
        // inventory rather than assumed.
        let mut placed: Vec<Placed> = Vec::with_capacity(row.holders.len());
        for (position, holder) in row.holders.iter().enumerate() {
            placed.push(place(&router, case, row, &network, position, holder).await);
        }

        // No two holders may claim the same interface: a place one holder took is not a place
        // another one took, which is what makes "a delete gives back its own" meaningful.
        let total: usize = placed.iter().map(|holder| holder.interfaces.len()).sum();
        let distinct: BTreeSet<&String> = placed
            .iter()
            .flat_map(|holder| holder.interfaces.iter())
            .collect();
        assert_eq!(
            distinct.len(),
            total,
            "case {case:?}: each holder must hold interfaces of its own, got {:?}",
            placed
                .iter()
                .map(|holder| (holder.label.as_str(), &holder.interfaces))
                .collect::<Vec<_>>()
        );

        // Each holder goes in turn. After each delete the inventory holds exactly the
        // interfaces of the holders that are STILL there, and every subnet one of them still
        // sits in keeps refusing its delete — the refusal follows the holders rather than
        // being permanent, and it is not lifted early by a delete that did not free that
        // subnet.
        for position in 0..placed.len() {
            let (action, (member, value)) = &placed[position].delete;
            let reply = match *action {
                "DeleteLoadBalancer" => {
                    elbv2_call(&router, action, &[(member, value.as_str())]).await
                }
                _ => ec2_call(&router, action, &[(member, value.as_str())]).await,
            };
            assert_eq!(
                reply.status,
                200,
                "case {case:?}: {action} of {} must succeed, got {} {}",
                placed[position].label,
                reply.status,
                reply.body
            );

            let remaining: Vec<&Placed> = placed[position + 1..].iter().collect();
            let expected: BTreeSet<String> = remaining
                .iter()
                .flat_map(|holder| holder.interfaces.iter().cloned())
                .collect();
            interfaces_settle(
                &router,
                case,
                &format!("the delete of {}", placed[position].label),
                &network.vpc,
                &expected,
            )
            .await;

            for subnet in still_held(&remaining, &network.subnets) {
                let refused =
                    ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
                assert_eq!(
                    (refused.status, refused.error_code().as_str()),
                    (400, SUBNET_IN_USE_CODE),
                    "case {case:?}: {subnet} must stay held while {} is in it, got {} {}",
                    remaining
                        .iter()
                        .filter(|holder| holder.subnets.contains(subnet))
                        .map(|holder| holder.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    refused.status,
                    refused.body
                );
            }
        }

        // The last holder is gone, so the inventory holds nothing of this network …
        let drained = service_interface_ids(&router, &network.vpc).await;
        assert!(
            drained.is_empty(),
            "case {case:?}: no interface of the load balancing service may outlive its load \
             balancer, got {drained:?}"
        );
        // … and the teardown the client is in the middle of runs to its end.
        for subnet in &network.subnets {
            let removed = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: {subnet} must delete once its last holder is gone, got {} {}",
                removed.status, removed.body
            );
        }
        let ungoverned = ec2_call(
            &router,
            "DeleteSecurityGroup",
            &[("GroupId", network.security_group.as_str())],
        )
        .await;
        assert_eq!(
            ungoverned.status, 200,
            "case {case:?}: the group must delete once no interface carries it, got {} {}",
            ungoverned.status, ungoverned.body
        );
        let torn_down = ec2_call(&router, "DeleteVpc", &[("VpcId", network.vpc.as_str())]).await;
        assert_eq!(
            torn_down.status, 200,
            "case {case:?}: the VPC must delete once nothing is left in it, got {} {}",
            torn_down.status, torn_down.body
        );
    }
}

#[tokio::test]
async fn a_subnet_a_load_balancer_has_left_deletes_before_it_does() {
    let case = "an application load balancer moved onto another subnet of its VPC";
    let router = emulator();
    let network = seed_network(
        &router,
        70,
        &["us-east-1a", "us-east-1b", "us-east-1c"],
        "left-subnet",
        false,
    )
    .await;

    let created = elbv2(
        &router,
        "CreateLoadBalancer",
        &[
            ("Name", "left-subnet-alb"),
            ("Scheme", "internal"),
            ("SecurityGroups.member.1", network.security_group.as_str()),
            ("Subnets.member.1", network.subnets[0].as_str()),
            ("Subnets.member.2", network.subnets[1].as_str()),
        ],
    )
    .await;
    let arn = element(&created, "LoadBalancerArn");
    let placed = service_interface_ids(&router, &network.vpc).await;
    assert_eq!(
        placed.len(),
        2,
        "case {case:?}: the placement holds one interface per attached subnet, got {placed:?}"
    );

    // The load balancer is moved off its first subnet onto the third. The interfaces it holds
    // follow the subnets it is attached to NOW, so the subnet it has LEFT has no holder left —
    // and that subnet deletes while the load balancer itself is still there, which is the same
    // rule the delete of the whole load balancer obeys for all of them at once.
    elbv2(
        &router,
        "SetSubnets",
        &[
            ("LoadBalancerArn", arn.as_str()),
            ("Subnets.member.1", network.subnets[1].as_str()),
            ("Subnets.member.2", network.subnets[2].as_str()),
        ],
    )
    .await;
    let vacated = ec2_call(
        &router,
        "DeleteSubnet",
        &[("SubnetId", network.subnets[0].as_str())],
    )
    .await;
    assert_eq!(
        vacated.status, 200,
        "case {case:?}: the subnet the load balancer has left must delete, got {} {}",
        vacated.status, vacated.body
    );
    for still_attached in &network.subnets[1..] {
        let refused = ec2_call(
            &router,
            "DeleteSubnet",
            &[("SubnetId", still_attached.as_str())],
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, SUBNET_IN_USE_CODE),
            "case {case:?}: {still_attached} must stay held while the load balancer sits in it, \
             got {} {}",
            refused.status,
            refused.body
        );
    }

    // The delete frees the rest. Repeating it changes nothing: "if the load balancer does not
    // exist or has already been deleted, the call succeeds" (`DeleteLoadBalancer`), and a
    // repeat releases no interface of anything else, so the subnets stay deletable.
    for repeat in 0..2 {
        let deleted = elbv2_call(
            &router,
            "DeleteLoadBalancer",
            &[("LoadBalancerArn", arn.as_str())],
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: delete attempt {repeat} must succeed, got {} {}",
            deleted.status, deleted.body
        );
    }
    interfaces_settle(
        &router,
        case,
        "the delete of the load balancer",
        &network.vpc,
        &BTreeSet::new(),
    )
    .await;
    for subnet in &network.subnets[1..] {
        let removed = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
        assert_eq!(
            removed.status, 200,
            "case {case:?}: {subnet} must delete once the load balancer is gone, got {} {}",
            removed.status, removed.body
        );
    }
}
