// SPDX-License-Identifier: CC0-1.0

//! A SERVICE-placed elastic network interface does not outlive the resource it carries traffic
//! for — whichever service placed it — so a network teardown behind it converges.
//!
//! RULE: an interface a service creates in a subnet to serve one of its own resources is
//! reported in the account's EC2 inventory as that service's to manage
//! (`NetworkInterface$RequesterId` / `NetworkInterface$RequesterManaged`), and it holds the
//! subnet it sits in against `ec2:DeleteSubnet` for exactly as long as that resource does: the
//! delete that takes the resource away gives back every interface it placed, and no interface
//! of anything else, so `DeleteSubnet` — refused while an occupant remains with HTTP 400,
//! `DependencyViolation` and the sentence naming the subnet the REQUEST named — answers 200
//! once the resource is gone. A subnet of the same VPC the resource never placed an interface
//! in is held by nothing and deletes straight away.
//!
//! Why it matters: a client tears its network down by deleting what it put in the subnets,
//! polling `DescribeNetworkInterfaces` until the service's interfaces drain and then calling
//! `DeleteSubnet`. An interface left behind for a resource that is gone makes that poll run to
//! its own cap and every later `DeleteSubnet` keep being refused, so a retry-until-deleted
//! teardown never terminates and the account leaks subnets, interfaces and the VPC above them
//! between runs. The dependency has to clear in bounded time, not eventually.
//!
//! The rows span what the rule generalises over: WHICH SERVICE places the interfaces (Elastic
//! Load Balancing v2 behind a load balancer, Route 53 Resolver behind a Resolver endpoint —
//! each reported under its own requester alias and its own `NetworkInterfaceType`), the
//! resource's own configuration (the load balancer FAMILY, the endpoint DIRECTION), HOW MANY
//! interfaces one resource places, and how they are spread over the network's subnets (one
//! each, several in one subnet, and a subnet of the VPC that holds none at all) — so the
//! dependency set a delete has to clear is not simply "one interface per subnet". The single
//! configuration a report happens to mention is one row.
//!
//! `elbv2_teardown_frees_the_subnets_its_interfaces_held.rs` asserts this for the load
//! balancing service family by family, and `subnet_dependency_clears_with_its_last_holder.rs`
//! for several holders of ONE subnet; what this suite adds is that the rule belongs to the
//! inventory rather than to one service — a second service's own placement is released on the
//! same terms — and the exact sentence EC2 refuses a held subnet with. It takes the two-service
//! harness (`ec2`, `ec2_call`, `elbv2`, `elbv2_call`, `seed_network`, `describe_interface`,
//! `Reply` and the shared poll bounds) from the first of those rather than keeping a second
//! copy, and mounts Route 53 Resolver beside them. Nothing reaches into any service's state
//! struct: every assertion is a wire answer.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_teardown_frees_the_subnets_its_interfaces_held::{
    borrowed, describe_interface, ec2, ec2_call, elbv2, elbv2_call, seed_network, Network, Reply,
    DRAIN_DEADLINE, LOOK_AGAIN_AFTER, SUBNET_IN_USE_CODE,
};
use crate::query_wire::{blocks_of, element, elements};

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-east-1";

/// `NetworkInterface$RequesterId` of an interface Elastic Load Balancing created and manages.
const ELB_REQUESTER: &str = "amazon-elb";

/// `NetworkInterface$RequesterId` of an interface Route 53 Resolver created and manages.
const RESOLVER_REQUESTER: &str = "route53resolver.amazonaws.com";

/// The emulator the rows drive: the account's EC2 inventory and the two services that place
/// interfaces in it, over ONE shared engine, exactly as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_route53resolver_route53resolver::EmulatedService::new(engine),
    );
    router
}

/// One Route 53 Resolver call over its `awsJson1_1` wire, as its status and its parsed body.
async fn resolver(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("route53resolver.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/\
                 route53resolver/aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Route53Resolver.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Route 53 Resolver call that must be served, as its parsed body.
async fn resolver_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = resolver(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Route 53 Resolver {operation} must be served, got {status} with {answered}"
    );
    answered
}

/// The `Message` of a query error envelope, or `""` when the answer carries none.
///
/// The companion of `Reply::error_code`: what the refusal SAYS, which is what a client reads to
/// learn which of its resources it has to take away first. Read through `elements`, which
/// decodes the entities the XML serializer escaped — the sentence quotes an identifier, and a
/// quote travels the wire as `&apos;`.
fn refusal_message(reply: &Reply) -> String {
    blocks_of(&reply.body, "Error")
        .first()
        .and_then(|envelope| elements(envelope, "Message").into_iter().next())
        .unwrap_or_default()
}

/// Every interface EC2 reports in `vpc` that `requester` created and manages — the read a client
/// tearing its network down polls to see whether that service's presence has drained.
async fn service_interface_ids(
    router: &MultiServiceRouter,
    vpc: &str,
    requester: &str,
) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc),
            ("Filter.2.Name", "requester-id"),
            ("Filter.2.Value.1", requester),
        ],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// Look again until `requester` holds exactly `expected` interfaces in `vpc`, and answer their
/// ids.
///
/// Bounded by the shared [`DRAIN_DEADLINE`], which is generous on purpose: this waits for the
/// CONDITION rather than for a fixed time, so a slower or busier host only looks more times.
/// A deadline that passes fails naming the step and the ids last seen — the poll a client runs,
/// and the one the emulator has to let converge.
async fn interfaces_settle(
    router: &MultiServiceRouter,
    case: &str,
    step: &str,
    vpc: &str,
    requester: &str,
    expected: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    loop {
        let held = service_interface_ids(router, vpc, requester).await;
        if held.len() == expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: after {step} the VPC must hold {expected} interface(s) of \
             {requester}, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The resource a row places in its network, and so the service whose interfaces it holds.
#[derive(Clone, Copy)]
enum Resource {
    /// A load balancer of this family (`None` is the default, application family), attached to
    /// every subnet of the row's network — so it places one interface in each of them.
    LoadBalancer { lb_type: Option<&'static str> },
    /// A Resolver endpoint of this direction, with one IP address per entry placed in the
    /// subnet that entry names by POSITION in the row's network. "The subnet ID uniquely
    /// identifies a VPC" (`CreateResolverEndpoint$IpAddresses`), and each address is the address
    /// of an interface of that subnet, so repeating a position places two interfaces in one
    /// subnet and omitting one leaves that subnet untouched.
    ResolverEndpoint {
        direction: &'static str,
        addresses: &'static [usize],
    },
}

/// One row: the resource a client places, and what the inventory therefore reports.
struct Row {
    case: &'static str,
    /// Part of every name and request id this row mints, so no row is answered another's
    /// idempotent repeat.
    name: &'static str,
    /// The second octet of this row's own VPC block, so each row's network is its own.
    second_octet: u8,
    /// One subnet per zone, in creation order. Two zones at least: "[Application Load Balancers]
    /// You must specify subnets from at least two Availability Zones"
    /// (`CreateLoadBalancerInput$Subnets`).
    zones: &'static [&'static str],
    resource: Resource,
    /// `NetworkInterface$RequesterId` the placing service's interfaces carry.
    requester: &'static str,
    /// The `NetworkInterfaceType` those interfaces are of: the load balancing families from the
    /// modelled enum (`smithy_query(query="shape",shape="NetworkInterfaceType")`:
    /// `load_balancer`, `network_load_balancer`), and the plain `interface` an endpoint's
    /// address sits on.
    interface_type: &'static str,
}

/// The rows span the placing service, the resource's own configuration, how many interfaces it
/// places and how they are spread over the network's subnets.
const ROWS: &[Row] = &[
    Row {
        case: "an application load balancer, one interface per attached subnet",
        name: "placed-interfaces-alb",
        second_octet: 61,
        zones: &["us-east-1a", "us-east-1b"],
        resource: Resource::LoadBalancer { lb_type: None },
        requester: ELB_REQUESTER,
        interface_type: "load_balancer",
    },
    Row {
        case: "a network load balancer, whose interfaces are of the other modelled type",
        name: "placed-interfaces-nlb",
        second_octet: 62,
        zones: &["us-east-1a", "us-east-1b"],
        resource: Resource::LoadBalancer {
            lb_type: Some("network"),
        },
        requester: ELB_REQUESTER,
        interface_type: "network_load_balancer",
    },
    Row {
        case: "an outbound Resolver endpoint, one address in each of two subnets",
        name: "placed-interfaces-outbound",
        second_octet: 63,
        zones: &["us-east-1a", "us-east-1b"],
        resource: Resource::ResolverEndpoint {
            direction: "OUTBOUND",
            addresses: &[0, 1],
        },
        requester: RESOLVER_REQUESTER,
        interface_type: "interface",
    },
    Row {
        case: "an inbound Resolver endpoint with two addresses in one subnet, beside a subnet it \
               never used",
        name: "placed-interfaces-inbound",
        second_octet: 64,
        zones: &["us-east-1a", "us-east-1b", "us-east-1c"],
        resource: Resource::ResolverEndpoint {
            direction: "INBOUND",
            addresses: &[0, 0, 1],
        },
        requester: RESOLVER_REQUESTER,
        interface_type: "interface",
    },
];

/// How many interfaces this row's resource holds in each subnet of a network of `subnets`
/// subnets, keyed by the subnet's position — the dependency set its delete has to clear, and
/// nothing else.
fn occupancy(row: &Row, subnets: usize) -> BTreeMap<usize, usize> {
    let mut held: BTreeMap<usize, usize> = BTreeMap::new();
    match row.resource {
        Resource::LoadBalancer { .. } => {
            for position in 0..subnets {
                held.insert(position, 1);
            }
        }
        Resource::ResolverEndpoint { addresses, .. } => {
            for position in addresses {
                *held.entry(*position).or_default() += 1;
            }
        }
    }
    held
}

/// Create this row's resource through its OWN service, and answer the identifier its delete
/// names.
async fn place(router: &MultiServiceRouter, row: &Row, network: &Network) -> String {
    match row.resource {
        Resource::LoadBalancer { lb_type } => {
            let mut members: Vec<(String, String)> = vec![
                ("Name".to_string(), row.name.to_string()),
                // An internal placement, whose nodes hold private addresses only, so the
                // network needs no internet gateway for the create to be served.
                ("Scheme".to_string(), "internal".to_string()),
                (
                    "SecurityGroups.member.1".to_string(),
                    network.security_group.clone(),
                ),
            ];
            if let Some(lb_type) = lb_type {
                members.push(("Type".to_string(), lb_type.to_string()));
            }
            for (position, subnet) in network.subnets.iter().enumerate() {
                members.push((format!("Subnets.member.{}", position + 1), subnet.clone()));
            }
            let created = elbv2(router, "CreateLoadBalancer", &borrowed(&members)).await;
            element(&created, "LoadBalancerArn")
        }
        Resource::ResolverEndpoint {
            direction,
            addresses,
        } => {
            let ip_addresses: Vec<Value> = addresses
                .iter()
                .map(|position| json!({ "SubnetId": network.subnets[*position] }))
                .collect();
            let created = resolver_ok(
                router,
                row.case,
                "CreateResolverEndpoint",
                &json!({
                    "CreatorRequestId": row.name,
                    "Direction": direction,
                    "SecurityGroupIds": [network.security_group],
                    "IpAddresses": ip_addresses,
                }),
            )
            .await;
            created["ResolverEndpoint"]["Id"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the create must answer a ResolverEndpoint carrying an Id, got \
                         {created}",
                        row.case
                    )
                })
                .to_string()
        }
    }
}

/// Take this row's resource away again through its own service's delete, which must be served.
async fn take_away(router: &MultiServiceRouter, row: &Row, handle: &str) {
    match row.resource {
        Resource::LoadBalancer { .. } => {
            let deleted =
                elbv2_call(router, "DeleteLoadBalancer", &[("LoadBalancerArn", handle)]).await;
            assert_eq!(
                deleted.status,
                200,
                "case {:?}: the delete of the load balancer must succeed, got {} {}",
                row.case,
                deleted.status,
                deleted.body
            );
        }
        Resource::ResolverEndpoint { .. } => {
            resolver_ok(
                router,
                row.case,
                "DeleteResolverEndpoint",
                &json!({ "ResolverEndpointId": handle }),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn a_service_placed_interface_is_given_back_with_the_resource_it_serves() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let network = seed_network(&router, row.second_octet, row.zones, row.name, false).await;

        // Before the resource exists the VPC holds no interface of its service, so what the
        // teardown gives back below is exactly what the create placed.
        let before = service_interface_ids(&router, &network.vpc, row.requester).await;
        assert!(
            before.is_empty(),
            "case {case:?}: no interface of {} may exist before the create, got {before:?}",
            row.requester
        );

        let handle = place(&router, row, &network).await;
        let expected = occupancy(row, network.subnets.len());
        let total: usize = expected.values().sum();

        // One service-managed interface per address the resource placed, each of the type its
        // service's interfaces carry and each reported as that service's to manage — which is
        // how a client reading the inventory can tell whose interface holds its subnet.
        let held = interfaces_settle(
            &router,
            case,
            "the create",
            &network.vpc,
            row.requester,
            total,
        )
        .await;
        let mut counted: BTreeMap<usize, usize> = BTreeMap::new();
        for id in &held {
            let interface = describe_interface(&router, id).await;
            assert_eq!(
                (
                    interface.interface_type.as_str(),
                    interface.requester.as_str(),
                    interface.requester_managed.as_str(),
                ),
                (row.interface_type, row.requester, "true"),
                "case {case:?}: expected the placing service's own managed interface, got \
                 {interface:?}"
            );
            let position = network
                .subnets
                .iter()
                .position(|subnet| *subnet == interface.subnet)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: {id} must sit in a subnet of the network {:?}, got \
                         {interface:?}",
                        network.subnets
                    )
                });
            *counted.entry(position).or_default() += 1;
        }
        assert_eq!(
            counted, expected,
            "case {case:?}: expected the placement to hold {expected:?} interfaces by subnet \
             position, got {counted:?}"
        );

        // While the resource is there, each subnet one of its interfaces sits in has a
        // dependency and does not delete — and the refusal names the subnet THE REQUEST named.
        // A subnet of the same VPC the resource placed nothing in is held by nothing, so it
        // deletes right away.
        let mut still_held: Vec<String> = Vec::new();
        for (position, subnet) in network.subnets.iter().enumerate() {
            let reply = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
            if expected.contains_key(&position) {
                assert_eq!(
                    (reply.status, reply.error_code().as_str()),
                    (400, SUBNET_IN_USE_CODE),
                    "case {case:?}: {subnet} must not delete while an interface sits in it, got \
                     {} {}",
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    refusal_message(&reply),
                    format!("The subnet '{subnet}' has dependencies and cannot be deleted."),
                    "case {case:?}: the refusal of {subnet} must name the subnet the request \
                     named, got {}",
                    reply.body
                );
                still_held.push(subnet.clone());
            } else {
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: {subnet} holds no interface of the resource and must delete, \
                     got {} {}",
                    reply.status, reply.body
                );
            }
        }

        // The delete takes the resource, and with it every interface it placed, so the poll a
        // client runs converges to an empty answer instead of to its own cap …
        take_away(&router, row, &handle).await;
        interfaces_settle(
            &router,
            case,
            "the delete of the resource",
            &network.vpc,
            row.requester,
            0,
        )
        .await;

        // … which is what lets the teardown run to its end: the subnets its interfaces held,
        // the group guarding them and the VPC around them all delete.
        for subnet in &still_held {
            let removed = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
            assert_eq!(
                removed.status, 200,
                "case {case:?}: {subnet} must delete once no interface sits in it, got {} {}",
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
            "case {case:?}: the VPC must delete once nothing of the resource is left in it, got \
             {} {}",
            torn_down.status, torn_down.body
        );
    }
}
