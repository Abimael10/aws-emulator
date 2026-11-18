// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the firewall endpoint a firewall instantiates in each of its
//! subnets, and for routing traffic through it.
//!
//! RULE under test: the firewall endpoint an attachment reports
//! (`FirewallStatus.SyncStates.<az>.Attachment.EndpointId`) is a Gateway Load Balancer VPC
//! endpoint that Amazon EC2 HOLDS, created in the firewall's VPC and in that attachment's
//! subnet — so EC2's own `DescribeVpcEndpoints` reports it and a route table of that VPC can
//! name it as its `VpcEndpointId` target — and it exists exactly as long as the attachment
//! carrying it does.
//!
//! Every test mounts Amazon EC2 beside Network Firewall on one engine, because the VPC, the
//! subnets, the route table and the endpoint itself are all ITS resources: the networks are
//! created through EC2's own API and the ids asserted on are the ids EC2 minted. Network
//! Firewall is driven the way a client drives it, over the wire through the same router the
//! emulator boots.
//!
//! The tests are tables: each row is one input in the class the rule covers, and every
//! assertion names its row and the actual value.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::harness::{error_code, error_message, string_at, Harness};

/// The `VpcEndpointType` a firewall endpoint is reported with
/// (`smithy_query(query="shape",shape="VpcEndpointType")` on the EC2 model: one of `Gateway`,
/// `GatewayLoadBalancer`, `Interface`, `Resource`, `ServiceNetwork`, `Tunnel`).
const GATEWAY_LOAD_BALANCER: &str = "GatewayLoadBalancer";

/// The `State` an endpoint that is in service is REPORTED in: "`vpc-endpoint-state` - The
/// state of the endpoint (`pendingAcceptance` | `pending` | `available` | `deleting` |
/// `deleted` | `rejected` | `failed`)" (`DescribeVpcEndpointsRequest$Filters` of the EC2
/// model), the filter whose values are the values `VpcEndpoint$State` carries.
const ENDPOINT_STATE_AVAILABLE: &str = "available";

/// EC2's own answer for a VPC endpoint id it holds no endpoint for, which is what a describe
/// naming one is REFUSED with rather than given an absent row.
const ENDPOINT_NOT_FOUND: &str = "InvalidVpcEndpointId.NotFound";

/// A VPC endpoint id of the form EC2 mints (`vpce-` and 17 hexadecimal digits) that nothing
/// in this emulator ever minted.
const NEVER_MINTED: &str = "vpce-00000000000000000";

/// The destination of a route that steers every address through a firewall endpoint, which is
/// what naming the endpoint in a route table is for.
const EVERY_DESTINATION: &str = "0.0.0.0/0";

/// How long a test waits for a CONDITION the clock owes before giving up. Far above the
/// firewall's own provisioning span, because what each wait below loops on is the state the
/// emulator reports, never an elapsed time.
const CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One VPC with one subnet per entry of `subnet_cidrs` and one route table, created through
/// Amazon EC2's own API; every id is the one EC2 MINTED.
struct Network {
    vpc_id: String,
    subnet_ids: Vec<String>,
    route_table_id: String,
}

/// Create [`Network`] in `ec2`: the VPC `vpc_cidr`, a subnet per entry of `subnet_cidrs`, and
/// one route table for the VPC.
async fn create_network(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    vpc_cidr: &str,
    subnet_cidrs: &[&str],
) -> Network {
    let created = ec2
        .create_vpc()
        .cidr_block(vpc_cidr)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: EC2 must create {vpc_cidr}: {error:?}"));
    let vpc_id = created
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must report the VPC it minted"))
        .to_string();

    let mut subnet_ids = Vec::with_capacity(subnet_cidrs.len());
    for cidr in subnet_cidrs {
        let created = ec2
            .create_subnet()
            .vpc_id(&vpc_id)
            .cidr_block(*cidr)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: EC2 must create the subnet {cidr}: {error:?}")
            });
        subnet_ids.push(
            created
                .subnet()
                .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: CreateSubnet must report the subnet it minted")
                })
                .to_string(),
        );
    }

    let created = ec2
        .create_route_table()
        .vpc_id(&vpc_id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must create a route table in {vpc_id}: {error:?}")
        });
    let route_table_id = created
        .route_table()
        .and_then(::aws_sdk_ec2::types::RouteTable::route_table_id)
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateRouteTable must report the table it minted")
        })
        .to_string();

    Network {
        vpc_id,
        subnet_ids,
        route_table_id,
    }
}

/// Create a firewall named `name` on `subnet_ids` of `vpc_id`, through a policy created for
/// it, and answer the raw `CreateFirewall` response (status and body).
async fn create_firewall(
    harness: &Harness,
    name: &str,
    vpc_id: &str,
    subnet_ids: &[String],
) -> (u16, Value) {
    let policy_arn = harness.shared_policy(&format!("{name}-policy")).await;
    let mappings: Vec<Value> = subnet_ids
        .iter()
        .map(|subnet_id| json!({ "SubnetId": subnet_id }))
        .collect();
    harness
        .call(
            "CreateFirewall",
            &json!({
                "FirewallName": name,
                "FirewallPolicyArn": policy_arn,
                "VpcId": vpc_id,
                "SubnetMappings": mappings,
            }),
        )
        .await
}

/// Poll `DescribeFirewall` until the firewall reports `READY`, and answer that response.
///
/// Waits for the CONDITION rather than for a duration: the clock owes the firewall its
/// `PROVISIONING` -> `READY` move, so a loaded host only makes this loop more times.
async fn ready_firewall(harness: &Harness, case: &str, name: &str) -> Value {
    let deadline = Instant::now() + CONDITION_DEADLINE;
    loop {
        let described = harness
            .ok("DescribeFirewall", &json!({ "FirewallName": name }))
            .await;
        if described["FirewallStatus"]["Status"] == json!("READY") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the firewall {name} to reach READY, last saw {described:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The endpoint id each attachment of a `DescribeFirewall` response reports, keyed by the
/// subnet that attachment is in.
fn attachments(described: &Value) -> Vec<(String, String)> {
    described["FirewallStatus"]["SyncStates"]
        .as_object()
        .unwrap_or_else(|| panic!("expected SyncStates in {described:?}"))
        .values()
        .map(|sync_state| {
            (
                string_at(sync_state, &["Attachment", "SubnetId"]).to_string(),
                string_at(sync_state, &["Attachment", "EndpointId"]).to_string(),
            )
        })
        .collect()
}

/// The VPC endpoints Amazon EC2 holds in `vpc_id`, as `(endpoint id, type, subnet ids)`.
async fn endpoints_in_vpc(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    vpc_id: &str,
) -> Vec<(String, String, Vec<String>)> {
    let by_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name("vpc-id")
        .values(vpc_id.to_string())
        .build();
    let described = ec2
        .describe_vpc_endpoints()
        .filters(by_vpc)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the endpoints of {vpc_id}: {error:?}")
        });
    described
        .vpc_endpoints()
        .iter()
        .map(|endpoint| {
            (
                endpoint.vpc_endpoint_id().unwrap_or_default().to_string(),
                endpoint.vpc_endpoint_type().map_or_else(
                    || "<no type>".to_string(),
                    |endpoint_type| endpoint_type.as_str().to_string(),
                ),
                endpoint.subnet_ids().to_vec(),
            )
        })
        .collect()
}

/// The endpoint ids Amazon EC2 holds in `vpc_id`.
async fn endpoint_ids_in_vpc(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    vpc_id: &str,
) -> BTreeSet<String> {
    endpoints_in_vpc(ec2, case, vpc_id)
        .await
        .into_iter()
        .map(|(endpoint_id, _, _)| endpoint_id)
        .collect()
}

/// The target `DescribeRouteTables` reports for the route to `destination` in the table
/// `route_table_id`, or `None` when the table routes that destination nowhere.
///
/// An endpoint target is reported in `Route$GatewayId`, the modelled `Route` carrying no
/// member of its own for a VPC endpoint.
async fn route_target(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    route_table_id: &str,
    destination: &str,
) -> Option<String> {
    let described = ec2
        .describe_route_tables()
        .route_table_ids(route_table_id)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the table {route_table_id}: {error:?}")
        });
    described
        .route_tables()
        .iter()
        .flat_map(::aws_sdk_ec2::types::RouteTable::routes)
        .find(|route| route.destination_cidr_block() == Some(destination))
        .and_then(|route| route.gateway_id().map(str::to_string))
}

/// RULE: every attachment of a READY firewall reports an endpoint Amazon EC2 holds — a
/// `GatewayLoadBalancer` endpoint in the firewall's VPC and in that attachment's subnet — and
/// a route table of that VPC accepts it as a `VpcEndpointId` target.
///
/// The table spans the number of subnets a firewall is placed in, which is what the endpoint
/// count generalises over ("Network Firewall creates a firewall endpoint in each subnet",
/// `CreateFirewallRequest$SubnetMappings`), each row in its own VPC with its own address
/// ranges.
#[tokio::test]
async fn a_firewall_endpoint_is_an_ec2_endpoint_a_route_can_target() {
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidrs: &'static [&'static str],
        /// One destination per endpoint, each more specific than the VPC's own range, as a
        /// route steering workload traffic through a firewall endpoint is.
        destinations: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "one-subnet",
            vpc_cidr: "10.21.0.0/16",
            subnet_cidrs: &["10.21.1.0/24"],
            destinations: &["10.21.8.0/24"],
        },
        Case {
            name: "two-subnets",
            vpc_cidr: "10.22.0.0/16",
            subnet_cidrs: &["10.22.1.0/24", "10.22.2.0/24"],
            destinations: &["10.22.8.0/24", "10.22.9.0/24"],
        },
        Case {
            name: "three-subnets",
            vpc_cidr: "10.23.0.0/16",
            subnet_cidrs: &["10.23.1.0/24", "10.23.2.0/24", "10.23.3.0/24"],
            destinations: &["10.23.8.0/24", "10.23.9.0/24", "10.23.10.0/24"],
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let network = create_network(&ec2, case.name, case.vpc_cidr, case.subnet_cidrs).await;

        let (status, created) =
            create_firewall(&harness, case.name, &network.vpc_id, &network.subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the firewall to be created, got HTTP {status} with \
             {created:?}",
            name = case.name
        );

        let described = ready_firewall(&harness, case.name, case.name).await;
        let reported = attachments(&described);
        assert_eq!(
            reported.len(),
            case.subnet_cidrs.len(),
            "case {name:?}: expected one attachment per subnet, got {reported:?}",
            name = case.name
        );

        // Every endpoint the firewall reports is one EC2 holds, of the routable type, in the
        // firewall's VPC and in the attachment's own subnet.
        let held = endpoints_in_vpc(&ec2, case.name, &network.vpc_id).await;
        for (subnet_id, endpoint_id) in &reported {
            let found = held
                .iter()
                .find(|(held_id, _, _)| held_id == endpoint_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {name:?}: expected Amazon EC2 to hold the firewall endpoint \
                         {endpoint_id}, it holds {held:?}",
                        name = case.name
                    )
                });
            assert_eq!(
                found.1, GATEWAY_LOAD_BALANCER,
                "case {name:?}: expected {endpoint_id} to be a {GATEWAY_LOAD_BALANCER} \
                 endpoint, got {found:?}",
                name = case.name
            );
            assert_eq!(
                found.2,
                vec![subnet_id.clone()],
                "case {name:?}: expected {endpoint_id} to be in the attachment's subnet, got \
                 {found:?}",
                name = case.name
            );
        }

        // And a route table of that VPC takes each of them as a target: 200, `return` true,
        // and the stored route reports the endpoint it points at.
        for ((_, endpoint_id), destination) in reported.iter().zip(case.destinations) {
            let routed = ec2
                .create_route()
                .route_table_id(&network.route_table_id)
                .destination_cidr_block(*destination)
                .vpc_endpoint_id(endpoint_id)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "case {name:?}: EC2 must route {destination} through {endpoint_id}: \
                         {error:?}",
                        name = case.name
                    )
                });
            assert_eq!(
                routed.r#return(),
                Some(true),
                "case {name:?}: expected CreateRoute to answer return=true for \
                 {endpoint_id}, got {routed:?}",
                name = case.name
            );
            let target = route_target(&ec2, case.name, &network.route_table_id, destination).await;
            assert_eq!(
                target.as_deref(),
                Some(endpoint_id.as_str()),
                "case {name:?}: expected the route to {destination} to target {endpoint_id}, \
                 got {target:?}",
                name = case.name
            );
        }
    }
}

/// RULE: an attachment reports an endpoint Amazon EC2 holds exactly where EC2 created one —
/// so a subnet this account reaches but EC2 can place no endpoint of this firewall in yields
/// no endpoint at all, and the id such an attachment reports is refused as a route target by
/// EC2 itself.
///
/// The table spans the placements that are the account's own — so the create is served — and
/// that EC2 still instantiates no endpoint in: a subnet of ANOTHER VPC than the one the
/// firewall names, and a list that mixes the firewall's own subnet with such a foreign one,
/// which shows the two outcomes side by side on one firewall. A placement naming a VPC or a
/// subnet this account does not hold at all is refused before any of this (see
/// `placement_reachability`).
#[tokio::test]
async fn an_attachment_reports_an_endpoint_only_where_ec2_created_one() {
    /// Which subnets of the account the row asks the firewall to be placed in.
    enum Placement {
        /// A subnet that exists, but in another VPC than the one the firewall names.
        ForeignSubnet,
        /// The firewall's own subnet first, then one of another VPC.
        OwnThenForeign,
    }
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidr: &'static str,
        placement: Placement,
    }
    let cases = [
        Case {
            name: "foreign-subnet",
            vpc_cidr: "10.32.0.0/16",
            subnet_cidr: "10.32.1.0/24",
            placement: Placement::ForeignSubnet,
        },
        Case {
            name: "own-then-foreign",
            vpc_cidr: "10.34.0.0/16",
            subnet_cidr: "10.34.1.0/24",
            placement: Placement::OwnThenForeign,
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let network = create_network(&ec2, case.name, case.vpc_cidr, &[case.subnet_cidr]).await;
        let elsewhere =
            create_network(&ec2, case.name, "10.99.0.0/16", &["10.99.1.0/24"]).await;

        // What the request names, and which of those subnets EC2 can create an endpoint in:
        // one of the firewall's OWN VPC, which is none of them unless the row says so.
        let (vpc_id, subnet_ids, resolving): (String, Vec<String>, Vec<String>) =
            match case.placement {
                Placement::ForeignSubnet => (
                    network.vpc_id.clone(),
                    elsewhere.subnet_ids.clone(),
                    Vec::new(),
                ),
                Placement::OwnThenForeign => (
                    network.vpc_id.clone(),
                    vec![
                        network.subnet_ids[0].clone(),
                        elsewhere.subnet_ids[0].clone(),
                    ],
                    vec![network.subnet_ids[0].clone()],
                ),
            };

        let (status, created) = create_firewall(&harness, case.name, &vpc_id, &subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the create to be served, got HTTP {status} with {created:?}",
            name = case.name
        );
        let described = ready_firewall(&harness, case.name, case.name).await;
        let reported = attachments(&described);

        // EC2 holds one endpoint for each subnet of the firewall's VPC and none for the rest.
        let held = endpoint_ids_in_vpc(&ec2, case.name, &vpc_id).await;
        assert_eq!(
            held.len(),
            resolving.len(),
            "case {name:?}: expected {wanted} endpoint(s) in {vpc_id}, EC2 holds {held:?}",
            name = case.name,
            wanted = resolving.len(),
        );
        for (subnet_id, endpoint_id) in &reported {
            let backed = resolving.contains(subnet_id);
            assert_eq!(
                held.contains(endpoint_id),
                backed,
                "case {name:?}: expected EC2 to hold the endpoint of {subnet_id} ({backed}), \
                 it holds {held:?} and the attachment reports {endpoint_id}",
                name = case.name,
            );
            if backed {
                continue;
            }
            // An attachment EC2 created no endpoint for reports an id that resolves nowhere,
            // and EC2 refuses it as a route target — the very refusal a firewall endpoint EC2
            // DID create does not get (see the routing table above).
            let refused = ec2
                .create_route()
                .route_table_id(&network.route_table_id)
                .destination_cidr_block("10.80.0.0/24")
                .vpc_endpoint_id(endpoint_id)
                .send()
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "case {name:?}: expected EC2 to refuse a route to {endpoint_id}, which \
                         it holds no endpoint for",
                        name = case.name
                    )
                });
            let code = ::aws_sdk_ec2::error::ProvideErrorMetadata::code(&refused)
                .unwrap_or("<no code>")
                .to_string();
            // A route target is a RESOURCE, so an id EC2 holds no endpoint for is refused with
            // the ENDPOINT's own not-found code and sentence rather than the gateway's — the
            // wording the recorded wire corpus has for a route named at an unheld
            // `VpcEndpointId` (`ec2-nfwr-traj-route-target-to-firewall-endpoint-mistakes`).
            assert_eq!(
                code, "InvalidVpcEndpointId.NotFound",
                "case {name:?}: expected EC2's own refusal for an endpoint it does not hold, \
                 got {code:?}",
                name = case.name
            );
            let message = ::aws_sdk_ec2::error::ProvideErrorMetadata::message(&refused)
                .unwrap_or("<no message>")
                .to_string();
            assert_eq!(
                message,
                format!("The vpcEndpoint ID '{endpoint_id}' does not exist"),
                "case {name:?}: expected the refusal to name the endpoint it holds none of, \
                 got {message:?}",
                name = case.name
            );
        }
    }
}

/// RULE: an attachment's endpoint exists in Amazon EC2 for exactly as long as the attachment
/// does — an association instantiates one that a route can target, a disassociation deletes
/// that one and leaves the rest, and deleting the firewall takes every one of them with it.
#[tokio::test]
async fn a_firewall_endpoint_lives_as_long_as_its_attachment() {
    let case = "endpoint-lifecycle";
    let harness = Harness::with_ec2();
    let ec2 = harness.ec2();
    let network = create_network(
        &ec2,
        case,
        "10.41.0.0/16",
        &["10.41.1.0/24", "10.41.2.0/24"],
    )
    .await;

    let (status, created) = create_firewall(
        &harness,
        "lifecycle-fw",
        &network.vpc_id,
        &network.subnet_ids[..1],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the firewall to be created, got HTTP {status} with {created:?}"
    );
    let first = ready_firewall(&harness, case, "lifecycle-fw").await;
    let first_endpoint = attachments(&first)
        .first()
        .map(|(_, endpoint_id)| endpoint_id.clone())
        .unwrap_or_else(|| panic!("case {case:?}: expected one attachment in {first:?}"));
    assert_eq!(
        endpoint_ids_in_vpc(&ec2, case, &network.vpc_id).await,
        BTreeSet::from([first_endpoint.clone()]),
        "case {case:?}: expected EC2 to hold exactly the firewall's own endpoint"
    );

    // An association instantiates the second subnet's endpoint, and it is routable too.
    harness
        .ok(
            "AssociateSubnets",
            &json!({
                "FirewallName": "lifecycle-fw",
                "SubnetMappings": [{ "SubnetId": network.subnet_ids[1] }],
            }),
        )
        .await;
    let grown = ready_firewall(&harness, case, "lifecycle-fw").await;
    let grown_attachments = attachments(&grown);
    assert_eq!(
        grown_attachments.len(),
        2,
        "case {case:?}: expected both subnets to carry an attachment, got {grown_attachments:?}"
    );
    let second_endpoint = grown_attachments
        .iter()
        .find(|(subnet_id, _)| subnet_id == &network.subnet_ids[1])
        .map(|(_, endpoint_id)| endpoint_id.clone())
        .unwrap_or_else(|| panic!("case {case:?}: expected an attachment in the new subnet"));
    assert_eq!(
        endpoint_ids_in_vpc(&ec2, case, &network.vpc_id).await,
        BTreeSet::from([first_endpoint.clone(), second_endpoint.clone()]),
        "case {case:?}: expected EC2 to hold both of the firewall's endpoints"
    );
    let routed = ec2
        .create_route()
        .route_table_id(&network.route_table_id)
        .destination_cidr_block("10.41.8.0/24")
        .vpc_endpoint_id(&second_endpoint)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must route through {second_endpoint}: {error:?}")
        });
    assert_eq!(
        routed.r#return(),
        Some(true),
        "case {case:?}: expected CreateRoute to answer return=true, got {routed:?}"
    );

    // A disassociation deletes that endpoint and leaves the other.
    harness
        .ok(
            "DisassociateSubnets",
            &json!({
                "FirewallName": "lifecycle-fw",
                "SubnetIds": [network.subnet_ids[1]],
            }),
        )
        .await;
    assert_eq!(
        endpoint_ids_in_vpc(&ec2, case, &network.vpc_id).await,
        BTreeSet::from([first_endpoint.clone()]),
        "case {case:?}: expected the disassociated subnet's endpoint {second_endpoint} to be \
         deleted and the other kept"
    );

    // And deleting the firewall takes the endpoints it still carried with it.
    harness
        .ok(
            "DeleteFirewall",
            &json!({ "FirewallName": "lifecycle-fw" }),
        )
        .await;
    let held = endpoint_ids_in_vpc(&ec2, case, &network.vpc_id).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected the deleted firewall's endpoints to be gone, EC2 holds \
         {held:?}"
    );
}

/// What Amazon EC2 reports for the endpoints `endpoint_ids` NAMES, as
/// `(type, vpc id, state, subnet ids)` keyed by endpoint id.
///
/// The endpoints are NAMED rather than selected with a filter, because that is the
/// cross-service read a client does with the ids it took off `SyncStates`: an id EC2 holds no
/// endpoint for is a refusal of the whole call and not an absent row, so this read is what
/// proves every published id resolves.
async fn endpoints_by_id(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    endpoint_ids: &[String],
) -> BTreeMap<String, (String, String, String, Vec<String>)> {
    let described = ec2
        .describe_vpc_endpoints()
        .set_vpc_endpoint_ids(Some(endpoint_ids.to_vec()))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: EC2 must report the endpoints {endpoint_ids:?}: {error:?}")
        });
    described
        .vpc_endpoints()
        .iter()
        .map(|endpoint| {
            (
                endpoint.vpc_endpoint_id().unwrap_or_default().to_string(),
                (
                    endpoint.vpc_endpoint_type().map_or_else(
                        || "<no type>".to_string(),
                        |endpoint_type| endpoint_type.as_str().to_string(),
                    ),
                    endpoint.vpc_id().unwrap_or_default().to_string(),
                    endpoint.state().map_or_else(
                        || "<no state>".to_string(),
                        |state| state.as_str().to_string(),
                    ),
                    endpoint.subnet_ids().to_vec(),
                ),
            )
        })
        .collect()
}

/// The error code EC2 answers a describe NAMING `endpoint_id` with, or `None` when it reported
/// the endpoint instead.
async fn refusal_naming(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    endpoint_id: &str,
) -> Option<String> {
    let answer = ec2
        .describe_vpc_endpoints()
        .vpc_endpoint_ids(endpoint_id.to_string())
        .send()
        .await;
    match answer {
        Ok(reported) => {
            assert!(
                !reported.vpc_endpoints().is_empty(),
                "case {case:?}: a describe naming {endpoint_id} answered no endpoint and no \
                 refusal: {reported:?}"
            );
            None
        }
        Err(error) => Some(
            ::aws_sdk_ec2::error::ProvideErrorMetadata::code(&error)
                .unwrap_or("<no code>")
                .to_string(),
        ),
    }
}

/// The route tables whose routes target `endpoint_id`, as the `route.gateway-id` filter selects
/// them ("`route.gateway-id` - The ID of a gateway specified in a route in the table",
/// `DescribeRouteTablesRequest$Filters`) — the read that finds what routes through a firewall
/// endpoint.
async fn tables_routing_to(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    endpoint_id: &str,
) -> Vec<String> {
    let by_target = ::aws_sdk_ec2::types::Filter::builder()
        .name("route.gateway-id")
        .values(endpoint_id.to_string())
        .build();
    let described = ec2
        .describe_route_tables()
        .filters(by_target)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: EC2 must report the tables routing to {endpoint_id}: {error:?}"
            )
        });
    described
        .route_tables()
        .iter()
        .filter_map(::aws_sdk_ec2::types::RouteTable::route_table_id)
        .map(str::to_string)
        .collect()
}

/// RULE: every endpoint id a READY firewall publishes in
/// `FirewallStatus.SyncStates[*].Attachment.EndpointId` resolves in Amazon EC2 when NAMED —
/// `DescribeVpcEndpoints(VpcEndpointIds=…)` answers one `GatewayLoadBalancer` endpoint per id,
/// in the firewall's own VPC and `available` — and that id is the handle the VPC's route tables
/// name the endpoint by, so `route.gateway-id` selects no table before a route names it and
/// exactly the table that does afterwards.
///
/// The table spans the number of subnets a firewall is placed in, which is what the endpoint
/// count generalises over ("Network Firewall creates a firewall endpoint in each subnet",
/// `CreateFirewallRequest$SubnetMappings`), each row in its own VPC with its own ranges. This
/// is the read the cross-service client does, which the suite above makes with a `vpc-id`
/// FILTER instead: a filter answers an absent row for an id nothing holds, naming the id
/// refuses the call.
#[tokio::test]
async fn a_ready_firewalls_published_endpoint_ids_resolve_in_ec2_when_named() {
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidrs: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "one-zone",
            vpc_cidr: "10.51.0.0/16",
            subnet_cidrs: &["10.51.1.0/24"],
        },
        Case {
            name: "two-zones",
            vpc_cidr: "10.52.0.0/16",
            subnet_cidrs: &["10.52.1.0/24", "10.52.2.0/24"],
        },
        Case {
            name: "three-zones",
            vpc_cidr: "10.53.0.0/16",
            subnet_cidrs: &["10.53.1.0/24", "10.53.2.0/24", "10.53.3.0/24"],
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let network = create_network(&ec2, case.name, case.vpc_cidr, case.subnet_cidrs).await;

        let (status, created) =
            create_firewall(&harness, case.name, &network.vpc_id, &network.subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the firewall to be created, got HTTP {status} with \
             {created:?}",
            name = case.name
        );
        let described = ready_firewall(&harness, case.name, case.name).await;
        let reported = attachments(&described);
        assert_eq!(
            reported.len(),
            case.subnet_cidrs.len(),
            "case {name:?}: expected one attachment per subnet, got {reported:?}",
            name = case.name
        );
        let endpoint_ids: Vec<String> = reported
            .iter()
            .map(|(_, endpoint_id)| endpoint_id.clone())
            .collect();
        let distinct: BTreeSet<&String> = endpoint_ids.iter().collect();
        assert_eq!(
            distinct.len(),
            endpoint_ids.len(),
            "case {name:?}: expected one distinct endpoint per subnet, got {endpoint_ids:?}",
            name = case.name
        );

        // Naming every published id answers one endpoint per id, each of the routable type, in
        // the firewall's VPC, in service, and in its own attachment's subnet.
        let resolved = endpoints_by_id(&ec2, case.name, &endpoint_ids).await;
        assert_eq!(
            resolved.len(),
            endpoint_ids.len(),
            "case {name:?}: expected EC2 to report every one of {endpoint_ids:?}, got \
             {resolved:?}",
            name = case.name
        );
        for (subnet_id, endpoint_id) in &reported {
            let found = resolved.get(endpoint_id).unwrap_or_else(|| {
                panic!(
                    "case {name:?}: expected EC2 to report {endpoint_id}, it reported \
                     {resolved:?}",
                    name = case.name
                )
            });
            assert_eq!(
                (found.0.as_str(), found.1.as_str(), found.2.as_str()),
                (
                    GATEWAY_LOAD_BALANCER,
                    network.vpc_id.as_str(),
                    ENDPOINT_STATE_AVAILABLE
                ),
                "case {name:?}: expected {endpoint_id} to be a {GATEWAY_LOAD_BALANCER} \
                 endpoint in {vpc} and {ENDPOINT_STATE_AVAILABLE}, got {found:?}",
                name = case.name,
                vpc = network.vpc_id
            );
            assert_eq!(
                found.3,
                vec![subnet_id.clone()],
                "case {name:?}: expected {endpoint_id} to be in {subnet_id}, got {found:?}",
                name = case.name
            );
        }

        // Nothing routes to any of them yet, which is what makes the filter's answer below
        // evidence of the route and not of the endpoint merely existing.
        for endpoint_id in &endpoint_ids {
            let tables = tables_routing_to(&ec2, case.name, endpoint_id).await;
            assert!(
                tables.is_empty(),
                "case {name:?}: expected no table to route to {endpoint_id} before any route \
                 names it, got {tables:?}",
                name = case.name
            );
        }

        // One route names the FIRST endpoint, and the filter then selects exactly the table
        // that names it — and still nothing for the firewall's other endpoints.
        let named = &endpoint_ids[0];
        let routed = ec2
            .create_route()
            .route_table_id(&network.route_table_id)
            .destination_cidr_block(EVERY_DESTINATION)
            .vpc_endpoint_id(named)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {name:?}: EC2 must route {EVERY_DESTINATION} through {named}: \
                     {error:?}",
                    name = case.name
                )
            });
        assert_eq!(
            routed.r#return(),
            Some(true),
            "case {name:?}: expected CreateRoute to answer return=true for {named}, got \
             {routed:?}",
            name = case.name
        );
        let target = route_target(&ec2, case.name, &network.route_table_id, EVERY_DESTINATION).await;
        assert_eq!(
            target.as_deref(),
            Some(named.as_str()),
            "case {name:?}: expected the route to {EVERY_DESTINATION} to target {named}, got \
             {target:?}",
            name = case.name
        );
        let routing = tables_routing_to(&ec2, case.name, named).await;
        assert_eq!(
            routing,
            vec![network.route_table_id.clone()],
            "case {name:?}: expected the route.gateway-id filter to select exactly the table \
             that names {named}, got {routing:?}",
            name = case.name
        );
        for endpoint_id in &endpoint_ids[1..] {
            let tables = tables_routing_to(&ec2, case.name, endpoint_id).await;
            assert!(
                tables.is_empty(),
                "case {name:?}: expected no table to route to {endpoint_id}, which no route \
                 names, got {tables:?}",
                name = case.name
            );
        }
    }
}

/// RULE (the other side of the one above): a published id resolves because EC2 HOLDS that
/// endpoint, so an id EC2 holds none for is refused by the same named read — a firewall
/// publishing an id is not what makes a describe answer.
///
/// The table spans the ways EC2 comes to hold no endpoint for a `vpce-` id: one nothing ever
/// minted, one whose subnet was disassociated, and one whose firewall was deleted. Each row
/// also names an id that must STILL resolve wherever the firewall still carries that
/// attachment, so a row cannot pass by EC2 having forgotten everything.
#[tokio::test]
async fn an_endpoint_id_ec2_holds_no_endpoint_for_is_refused_by_the_same_read() {
    /// Why EC2 holds no endpoint for the id the row probes.
    enum Unheld {
        /// A form-valid id nothing in this emulator minted.
        NeverMinted,
        /// The endpoint of a subnet the firewall was disassociated from.
        Disassociated,
        /// The endpoints of a firewall that was deleted.
        FirewallDeleted,
    }
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidrs: &'static [&'static str],
        unheld: Unheld,
    }
    let cases = [
        Case {
            name: "never-minted",
            vpc_cidr: "10.61.0.0/16",
            subnet_cidrs: &["10.61.1.0/24", "10.61.2.0/24"],
            unheld: Unheld::NeverMinted,
        },
        Case {
            name: "disassociated",
            vpc_cidr: "10.62.0.0/16",
            subnet_cidrs: &["10.62.1.0/24", "10.62.2.0/24"],
            unheld: Unheld::Disassociated,
        },
        Case {
            name: "firewall-deleted",
            vpc_cidr: "10.63.0.0/16",
            subnet_cidrs: &["10.63.1.0/24", "10.63.2.0/24"],
            unheld: Unheld::FirewallDeleted,
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let network = create_network(&ec2, case.name, case.vpc_cidr, case.subnet_cidrs).await;
        let firewall = format!("{}-fw", case.name);

        let (status, created) =
            create_firewall(&harness, &firewall, &network.vpc_id, &network.subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the firewall to be created, got HTTP {status} with \
             {created:?}",
            name = case.name
        );
        let described = ready_firewall(&harness, case.name, &firewall).await;
        let published = attachments(&described);
        let endpoint_of = |subnet_id: &str| -> String {
            published
                .iter()
                .find(|(subnet, _)| subnet == subnet_id)
                .map(|(_, endpoint_id)| endpoint_id.clone())
                .unwrap_or_else(|| {
                    panic!(
                        "case {name:?}: expected an attachment in {subnet_id}, got {published:?}",
                        name = case.name
                    )
                })
        };

        // The id this row probes, and the one that must still resolve beside it.
        let (probed, still_held): (String, Option<String>) = match case.unheld {
            Unheld::NeverMinted => (
                NEVER_MINTED.to_string(),
                Some(endpoint_of(&network.subnet_ids[0])),
            ),
            Unheld::Disassociated => {
                let going = endpoint_of(&network.subnet_ids[1]);
                harness
                    .ok(
                        "DisassociateSubnets",
                        &json!({
                            "FirewallName": firewall,
                            "SubnetIds": [network.subnet_ids[1]],
                        }),
                    )
                    .await;
                (going, Some(endpoint_of(&network.subnet_ids[0])))
            }
            Unheld::FirewallDeleted => {
                let going = endpoint_of(&network.subnet_ids[0]);
                harness
                    .ok("DeleteFirewall", &json!({ "FirewallName": firewall }))
                    .await;
                (going, None)
            }
        };

        let refused = refusal_naming(&ec2, case.name, &probed).await;
        assert_eq!(
            refused.as_deref(),
            Some(ENDPOINT_NOT_FOUND),
            "case {name:?}: expected EC2 to refuse a describe naming {probed} with \
             {ENDPOINT_NOT_FOUND}, got {refused:?}",
            name = case.name
        );
        if let Some(held) = still_held {
            let resolved = endpoints_by_id(&ec2, case.name, std::slice::from_ref(&held)).await;
            let found = resolved.get(&held).unwrap_or_else(|| {
                panic!(
                    "case {name:?}: expected EC2 to still report {held}, it reported \
                     {resolved:?}",
                    name = case.name
                )
            });
            assert_eq!(
                (found.0.as_str(), found.1.as_str(), found.2.as_str()),
                (
                    GATEWAY_LOAD_BALANCER,
                    network.vpc_id.as_str(),
                    ENDPOINT_STATE_AVAILABLE
                ),
                "case {name:?}: expected {held} to still be a {GATEWAY_LOAD_BALANCER} endpoint \
                 in {vpc} and {ENDPOINT_STATE_AVAILABLE}, got {found:?}",
                name = case.name,
                vpc = network.vpc_id
            );
        }
    }
}

/// The FIXED text of the refusal a delete of a firewall whose endpoints are still routed
/// carries; the ids the call found still routed are what fills the bracket after it.
const STILL_ROUTED_PREFIX: &str = "Unable to fulfill request because the following related \
                                   VPC endpoint(s) still exist in route table(s): [";

/// What a route the row creates matches on, which the rule generalises over: a route names its
/// target whichever destination family it matches on
/// (`CreateRouteRequest$DestinationCidrBlock` / `$DestinationIpv6CidrBlock`).
#[derive(Debug, Clone, Copy)]
enum Destination {
    /// An IPv4 block.
    Ipv4(&'static str),
    /// An IPv6 block.
    Ipv6(&'static str),
}

/// One route a row asks for: the attachment whose endpoint it targets (its index in the row's
/// subnet list), which of the row's route tables it goes in, and what it matches on.
#[derive(Debug, Clone, Copy)]
struct Routed {
    endpoint: usize,
    table: usize,
    destination: Destination,
}

/// Add the route `routed` to `tables`, pointing it at `endpoint_ids[routed.endpoint]`, and
/// assert EC2 accepted it.
async fn add_route(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    tables: &[String],
    endpoint_ids: &[String],
    routed: Routed,
) {
    let table = &tables[routed.table];
    let endpoint_id = &endpoint_ids[routed.endpoint];
    let request = ec2
        .create_route()
        .route_table_id(table)
        .vpc_endpoint_id(endpoint_id);
    let request = match routed.destination {
        Destination::Ipv4(block) => request.destination_cidr_block(block),
        Destination::Ipv6(block) => request.destination_ipv6_cidr_block(block),
    };
    let created = request.send().await.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: EC2 must route {destination:?} through {endpoint_id} in {table}: \
             {error:?}",
            destination = routed.destination
        )
    });
    assert_eq!(
        created.r#return(),
        Some(true),
        "case {case:?}: expected CreateRoute for {destination:?} -> {endpoint_id} to answer \
         return=true, got {created:?}",
        destination = routed.destination
    );
}

/// Take the route `routed` back out of `tables`, so nothing routes to that endpoint any more.
async fn drop_route(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    tables: &[String],
    routed: Routed,
) {
    let table = &tables[routed.table];
    let request = ec2.delete_route().route_table_id(table);
    let request = match routed.destination {
        Destination::Ipv4(block) => request.destination_cidr_block(block),
        Destination::Ipv6(block) => request.destination_ipv6_cidr_block(block),
    };
    request.send().await.unwrap_or_else(|error| {
        panic!(
            "case {case:?}: EC2 must take the route {destination:?} back out of {table}: \
             {error:?}",
            destination = routed.destination
        )
    });
}

/// RULE: a firewall endpoint a VPC route table still sends traffic to is IN USE, so a
/// `DeleteFirewall` that would take it away is refused — HTTP 400 `InvalidRequestException`
/// naming exactly those of the firewall's endpoints a route still targets — and the firewall,
/// its attachments and its endpoints all survive the refusal; once no route names them, the
/// same delete is accepted and the endpoints go with it.
///
/// "You can check whether a firewall is in use by reviewing the route tables for the
/// Availability Zones where you have firewall subnet mappings … When the route tables no longer
/// use the firewall endpoints, you can remove the firewall safely" (`DeleteFirewall`).
///
/// The table spans what the rule generalises over: HOW MANY of the firewall's endpoints a route
/// names (none, one, two of three, all of them), WHICH attachment's endpoint is named (the
/// first subnet's, the last subnet's), what the route matches on (every address, a narrower
/// IPv4 block, an IPv6 block), and how many route tables of the VPC carry those routes (one
/// shared table, one table each).
#[tokio::test]
async fn a_firewall_whose_endpoints_are_still_routed_cannot_be_deleted() {
    struct Case {
        name: &'static str,
        vpc_cidr: &'static str,
        subnet_cidrs: &'static [&'static str],
        /// How many route tables the row creates in the VPC.
        tables: usize,
        routes: &'static [Routed],
    }
    let cases = [
        Case {
            name: "no-route-names-the-endpoints",
            vpc_cidr: "10.71.0.0/16",
            subnet_cidrs: &["10.71.1.0/24", "10.71.2.0/24"],
            tables: 1,
            routes: &[],
        },
        Case {
            name: "the-first-subnets-endpoint-takes-every-address",
            vpc_cidr: "10.72.0.0/16",
            subnet_cidrs: &["10.72.1.0/24", "10.72.2.0/24"],
            tables: 1,
            routes: &[Routed {
                endpoint: 0,
                table: 0,
                destination: Destination::Ipv4(EVERY_DESTINATION),
            }],
        },
        Case {
            name: "the-last-subnets-endpoint-takes-a-narrower-block",
            vpc_cidr: "10.73.0.0/16",
            subnet_cidrs: &["10.73.1.0/24", "10.73.2.0/24"],
            tables: 1,
            routes: &[Routed {
                endpoint: 1,
                table: 0,
                destination: Destination::Ipv4("10.73.9.0/24"),
            }],
        },
        Case {
            name: "both-endpoints-from-one-table-one-over-ipv6",
            vpc_cidr: "10.74.0.0/16",
            subnet_cidrs: &["10.74.1.0/24", "10.74.2.0/24"],
            tables: 1,
            routes: &[
                Routed {
                    endpoint: 0,
                    table: 0,
                    destination: Destination::Ipv4(EVERY_DESTINATION),
                },
                Routed {
                    endpoint: 1,
                    table: 0,
                    destination: Destination::Ipv6("2001:db8::/32"),
                },
            ],
        },
        Case {
            name: "two-of-three-endpoints-from-a-table-each",
            vpc_cidr: "10.75.0.0/16",
            subnet_cidrs: &["10.75.1.0/24", "10.75.2.0/24", "10.75.3.0/24"],
            tables: 2,
            routes: &[
                Routed {
                    endpoint: 0,
                    table: 0,
                    destination: Destination::Ipv4(EVERY_DESTINATION),
                },
                Routed {
                    endpoint: 2,
                    table: 1,
                    destination: Destination::Ipv4("10.75.9.0/24"),
                },
            ],
        },
    ];

    for case in cases {
        let harness = Harness::with_ec2();
        let ec2 = harness.ec2();
        let network = create_network(&ec2, case.name, case.vpc_cidr, case.subnet_cidrs).await;
        let mut tables = vec![network.route_table_id.clone()];
        while tables.len() < case.tables {
            let created = ec2
                .create_route_table()
                .vpc_id(&network.vpc_id)
                .send()
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "case {name:?}: EC2 must create a second route table: {error:?}",
                        name = case.name
                    )
                });
            tables.push(
                created
                    .route_table()
                    .and_then(::aws_sdk_ec2::types::RouteTable::route_table_id)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {name:?}: CreateRouteTable must report the table it minted",
                            name = case.name
                        )
                    })
                    .to_string(),
            );
        }

        let firewall = format!("{}-fw", case.name);
        let (status, created) =
            create_firewall(&harness, &firewall, &network.vpc_id, &network.subnet_ids).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the firewall to be created, got HTTP {status} with \
             {created:?}",
            name = case.name
        );
        let described = ready_firewall(&harness, case.name, &firewall).await;
        let published = attachments(&described);
        // The endpoints in the row's own subnet order, so a row names an attachment by where
        // its subnet sits rather than by an id it cannot know.
        let endpoint_ids: Vec<String> = case
            .subnet_cidrs
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let subnet_id = &network.subnet_ids[index];
                published
                    .iter()
                    .find(|(subnet, _)| subnet == subnet_id)
                    .map(|(_, endpoint_id)| endpoint_id.clone())
                    .unwrap_or_else(|| {
                        panic!(
                            "case {name:?}: expected an attachment in {subnet_id}, got \
                             {published:?}",
                            name = case.name
                        )
                    })
            })
            .collect();

        for routed in case.routes {
            add_route(&ec2, case.name, &tables, &endpoint_ids, *routed).await;
        }
        let mut still_routed: Vec<String> = case
            .routes
            .iter()
            .map(|routed| endpoint_ids[routed.endpoint].clone())
            .collect();
        still_routed.sort();
        still_routed.dedup();

        let (status, body) = harness
            .call("DeleteFirewall", &json!({ "FirewallName": firewall }))
            .await;
        if still_routed.is_empty() {
            // The negative control: nothing routes to this firewall, so the delete is the
            // plain one and takes every endpoint with it.
            assert_eq!(
                status, 200,
                "case {name:?}: expected a firewall nothing routes to to be deleted, got HTTP \
                 {status} with {body:?}",
                name = case.name
            );
            let held = endpoint_ids_in_vpc(&ec2, case.name, &network.vpc_id).await;
            assert!(
                held.is_empty(),
                "case {name:?}: expected the deleted firewall's endpoints to be gone, EC2 holds \
                 {held:?}",
                name = case.name
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidRequestException"),
            "case {name:?}: expected 400 InvalidRequestException for a firewall still routed \
             to, got HTTP {status} with {body:?}",
            name = case.name
        );
        let message = error_message(&body);
        assert!(
            message.starts_with(STILL_ROUTED_PREFIX) && message.ends_with(']'),
            "case {name:?}: expected the refusal to read {STILL_ROUTED_PREFIX}…], got \
             {message:?}",
            name = case.name
        );
        for endpoint_id in &still_routed {
            assert!(
                message.contains(endpoint_id.as_str()),
                "case {name:?}: expected the refusal to name the still-routed {endpoint_id}, \
                 got {message:?}",
                name = case.name
            );
        }
        for endpoint_id in &endpoint_ids {
            if still_routed.contains(endpoint_id) {
                continue;
            }
            assert!(
                !message.contains(endpoint_id.as_str()),
                "case {name:?}: expected the refusal NOT to name {endpoint_id}, which no route \
                 targets, got {message:?}",
                name = case.name
            );
        }

        // The refused delete left the firewall and every one of its endpoints exactly where
        // they were, so the caller can remove the routes and try again.
        let after = harness
            .ok("DescribeFirewall", &json!({ "FirewallName": firewall }))
            .await;
        let mut surviving = attachments(&after);
        surviving.sort();
        let mut expected = published.clone();
        expected.sort();
        assert_eq!(
            surviving, expected,
            "case {name:?}: expected the refused delete to leave the attachments alone",
            name = case.name
        );
        let held = endpoint_ids_in_vpc(&ec2, case.name, &network.vpc_id).await;
        assert_eq!(
            held,
            endpoint_ids.iter().cloned().collect::<BTreeSet<String>>(),
            "case {name:?}: expected EC2 to still hold every endpoint of the firewall whose \
             delete was refused",
            name = case.name
        );

        // And once no route names them, the very same delete is accepted.
        for routed in case.routes {
            drop_route(&ec2, case.name, &tables, *routed).await;
        }
        for endpoint_id in &endpoint_ids {
            let routing = tables_routing_to(&ec2, case.name, endpoint_id).await;
            assert!(
                routing.is_empty(),
                "case {name:?}: expected no table to route to {endpoint_id} once the routes \
                 were removed, got {routing:?}",
                name = case.name
            );
        }
        let (status, body) = harness
            .call("DeleteFirewall", &json!({ "FirewallName": firewall }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the delete to be accepted once nothing routed to the \
             firewall, got HTTP {status} with {body:?}",
            name = case.name
        );
        let held = endpoint_ids_in_vpc(&ec2, case.name, &network.vpc_id).await;
        assert!(
            held.is_empty(),
            "case {name:?}: expected the deleted firewall's endpoints to be gone, EC2 holds \
             {held:?}",
            name = case.name
        );
    }
}
