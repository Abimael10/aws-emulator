// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that fans a VPC out over the Availability Zones Amazon EC2 reports
//! for the Region SETTLES, and the estate it settles on is the one it declared.
//!
//! The rule under test: **a stack whose declared resources fan out over the Availability Zones
//! `DescribeAvailabilityZones` reports for the Region reaches a TERMINAL stack status — one the
//! caller can act on rather than an `…_IN_PROGRESS` that never ends — and every resource of it
//! is created in the service that owns it, so the deployed VPC carries two subnets per zone
//! spanned, one explicit route table per subnet beside its implicit main table, a default route
//! out of each public subnet through the internet gateway attached to the VPC, and a default
//! route out of each private subnet through the NAT gateway serving it; a declaration a service
//! REFUSES fails the stack to a terminal status instead of holding it open.**
//!
//! The rows differ along the COUNT the rule generalises over: how many of the Region's zones
//! the VPC spans — one, two, and every zone `DescribeAvailabilityZones` reports, which is the
//! widest fan-out a client can ask for — and how many NAT gateways carry the private subnets'
//! egress, so one gateway serving every zone is exercised beside one per zone. The zone names
//! are read from that operation rather than written here, so the widest row spans exactly what
//! the Region offers.
//!
//! A sibling suite (`cloudformation_ec2_cdk_vpc_estate`) pins the route tables, associations
//! and NAT gateways of a stack-shaped VPC, and `stack_created_vpc_internet_gateway_attachment`
//! pins the gateway attachment on its own; what is pinned HERE is that the whole multi-zone
//! fan-out CONVERGES, and that what it converged on spans the zones the Region reports.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{
    block_of, elements, form_encode, items_of, members_of, opt_element, outputs_of,
};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The VPC block every row is declared with, and the destination every default route is
/// declared for.
const VPC_CIDR: &str = "10.0.0.0/16";
const ALL_IPV4: &str = "0.0.0.0/0";

/// How long a poll waits between reads, and how many times it reads: a deadline generous
/// enough for every clock-owed span of a multi-zone deploy to come due on a loaded host, with
/// each loop stopping as soon as the condition it waits for holds.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_ATTEMPTS: usize = 240;

/// The state a NAT gateway settles in, which is what makes a private subnet's egress usable.
const NAT_AVAILABLE: &str = "available";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation makes the Amazon EC2 calls a stack's
/// resources are created with, and Amazon EC2 serves them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from. The signature is never checked:
/// this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One form-encoded call on a query wire, as its HTTP status and body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// Call one Amazon EC2 operation over its ec2Query wire.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "ec2", "2016-11-15", action, members).await
}

/// The names of the Availability Zones Amazon EC2 reports for the Region, in the order it
/// reports them — what a client clamping a zone count to the Region's reads, and what the
/// widest row of the table below spans.
async fn reported_zone_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = ec2(router, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeAvailabilityZones, got {status} with body {body}"
    );
    let zones = elements(&body, "zoneName");
    assert!(
        !zones.is_empty(),
        "expected Amazon EC2 to report at least one Availability Zone for {REGION}, got {body}"
    );
    zones
}

/// What the stack's own events say about every resource that failed, for a deploy that did not
/// reach the status it was asserted to.
async fn failures(router: &MultiServiceRouter, stack_name: &str) -> String {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    members_of(&events)
        .into_iter()
        .filter(|member| member.contains("_FAILED"))
        .map(|member| {
            format!(
                "{:?} {:?} {:?}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatus").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Read a stack until it reports a status that is no longer in progress, and answer that
/// read's body — the terminal answer the rule asks a deploy to reach.
async fn poll_to_terminal(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let mut described = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        described = body;
        if !opt_element(&described, "StackStatus")
            .unwrap_or_default()
            .ends_with("_IN_PROGRESS")
        {
            return described;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    described
}

/// Create a stack and read it to a terminal status, answering the `DescribeStacks` body that
/// reported it.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &Value,
) -> String {
    let template = template.to_string();
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    poll_to_terminal(router, case, stack_name).await
}

/// Deploy a stack and assert it settled at `CREATE_COMPLETE`, quoting what failed when it did
/// not: a status still in progress at the deadline is the stall the rule forbids, and reads
/// here as the status it was last reported in.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &Value,
) -> String {
    let described = create_and_settle(router, case, stack_name, template).await;
    let reported = opt_element(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to settle at CREATE_COMPLETE, got {reported:?}; its \
         failed resources: {}",
        failures(router, stack_name).await
    );
    described
}

/// How many of the Region's Availability Zones one row's VPC spans.
enum Span {
    /// A fixed number of zones, taken from the front of what the Region reports.
    Zones(usize),
    /// Every zone `DescribeAvailabilityZones` reports — the widest fan-out a client clamping
    /// its own zone count to the Region's can ask for.
    EveryZoneTheRegionReports,
}

/// One row of the table: how many zones the VPC spans, and how many NAT gateways carry its
/// private subnets' egress.
struct Row {
    case: &'static str,
    /// The stack name, which is also what keeps each row's resources its own.
    stack: &'static str,
    span: Span,
    /// How many NAT gateways the template declares: one for every private subnet to share, or
    /// one per zone.
    nat_gateways: NatGateways,
}

/// How many NAT gateways a row declares, stated the way a client asks for them rather than as
/// a bare number: the count is what the rule generalises over.
enum NatGateways {
    /// One gateway, in the first zone, which every private subnet egresses through.
    One,
    /// One gateway per zone spanned, each serving the private subnet of its own zone.
    OnePerZone,
}

const ROWS: &[Row] = &[
    Row {
        case: "one zone, one NAT gateway",
        stack: "az-span-one",
        span: Span::Zones(1),
        nat_gateways: NatGateways::One,
    },
    Row {
        case: "two zones, one NAT gateway per zone",
        stack: "az-span-two-per-zone",
        span: Span::Zones(2),
        nat_gateways: NatGateways::OnePerZone,
    },
    Row {
        case: "every zone the Region reports, one NAT gateway for all of them",
        stack: "az-span-every-zone",
        span: Span::EveryZoneTheRegionReports,
        nat_gateways: NatGateways::One,
    },
];

/// The zones one row spans, out of the ones the Region reports.
fn spanned<'a>(row: &Row, reported: &'a [String]) -> &'a [String] {
    match row.span {
        Span::Zones(count) => &reported[..count.min(reported.len())],
        Span::EveryZoneTheRegionReports => reported,
    }
}

/// How many NAT gateways one row declares over `zones` zones.
fn nat_gateway_count(row: &Row, zones: usize) -> usize {
    match row.nat_gateways {
        NatGateways::One => 1,
        NatGateways::OnePerZone => zones,
    }
}

/// The logical ids a row's template uses, so the template and the assertions name the same
/// resources. `group` is `Public` or `Private` and `index` counts zones from 1.
fn subnet_id(group: &str, index: usize) -> String {
    format!("{group}Subnet{index}")
}
fn table_id(group: &str, index: usize) -> String {
    format!("{group}Subnet{index}RouteTable")
}
fn nat_id(index: usize) -> String {
    format!("PublicSubnet{index}NATGateway")
}

/// The template one row deploys: the VPC, its internet gateway and the attachment that puts
/// the one on the other, then per zone spanned a public and a private subnet, each with its
/// OWN route table, the association that puts the subnet on it and the default route out of
/// it — the public one through the internet gateway, the private one through the NAT gateway
/// serving its zone, which is itself declared in a public subnet with the Elastic IP it
/// consumes. Every id leaves the stack through an output, the way a client learns what a
/// deploy created.
fn vpc_template(zones: &[String], nat_gateways: usize) -> Value {
    let mut resources = Map::new();
    let mut outputs = Map::new();
    resources.insert(
        "Vpc".to_string(),
        json!({
            "Type": "AWS::EC2::VPC",
            "Properties": {
                "CidrBlock": VPC_CIDR,
                "EnableDnsHostnames": true,
                "EnableDnsSupport": true,
            },
        }),
    );
    resources.insert(
        "VpcIGW".to_string(),
        json!({ "Type": "AWS::EC2::InternetGateway" }),
    );
    resources.insert(
        "VpcVPCGW".to_string(),
        json!({
            "Type": "AWS::EC2::VPCGatewayAttachment",
            "Properties": {
                "VpcId": { "Ref": "Vpc" },
                "InternetGatewayId": { "Ref": "VpcIGW" },
            },
        }),
    );
    outputs.insert("VpcId".to_string(), json!({ "Value": { "Ref": "Vpc" } }));
    outputs.insert("IgwId".to_string(), json!({ "Value": { "Ref": "VpcIGW" } }));

    for (zone, zone_name) in zones.iter().enumerate() {
        let index = zone + 1;
        for (group, public) in [("Public", true), ("Private", false)] {
            let subnet = subnet_id(group, index);
            let table = table_id(group, index);
            // One /20 per subnet out of the VPC's /16, so no two subnets of a row overlap
            // however many zones it spans.
            let block = format!("10.0.{}.0/20", 16 * (zone * 2 + usize::from(!public)));
            let mut properties = json!({
                "VpcId": { "Ref": "Vpc" },
                "AvailabilityZone": zone_name,
                "CidrBlock": block,
            });
            if public {
                if let Some(members) = properties.as_object_mut() {
                    members.insert("MapPublicIpOnLaunch".to_string(), json!(true));
                }
            }
            resources.insert(
                subnet.clone(),
                json!({ "Type": "AWS::EC2::Subnet", "Properties": properties }),
            );
            resources.insert(
                table.clone(),
                json!({
                    "Type": "AWS::EC2::RouteTable",
                    "Properties": { "VpcId": { "Ref": "Vpc" } },
                }),
            );
            let association = format!("{subnet}RouteTableAssociation");
            resources.insert(
                association.clone(),
                json!({
                    "Type": "AWS::EC2::SubnetRouteTableAssociation",
                    "Properties": {
                        "RouteTableId": { "Ref": table },
                        "SubnetId": { "Ref": subnet },
                    },
                }),
            );
            outputs.insert(
                format!("{subnet}Id"),
                json!({ "Value": { "Ref": subnet } }),
            );
            outputs.insert(format!("{table}Id"), json!({ "Value": { "Ref": table } }));

            let route = format!("{subnet}DefaultRoute");
            if public {
                resources.insert(
                    route,
                    json!({
                        "Type": "AWS::EC2::Route",
                        "DependsOn": ["VpcVPCGW"],
                        "Properties": {
                            "RouteTableId": { "Ref": table },
                            "DestinationCidrBlock": ALL_IPV4,
                            "GatewayId": { "Ref": "VpcIGW" },
                        },
                    }),
                );
                // The Elastic IP and the NAT gateway of this zone, where the row declares one
                // for it: a NAT gateway lives in a public subnet.
                if zone < nat_gateways {
                    let eip = format!("{subnet}EIP");
                    let nat = nat_id(index);
                    resources.insert(
                        eip.clone(),
                        json!({
                            "Type": "AWS::EC2::EIP",
                            "Properties": { "Domain": "vpc" },
                        }),
                    );
                    resources.insert(
                        nat.clone(),
                        json!({
                            "Type": "AWS::EC2::NatGateway",
                            "DependsOn": [association],
                            "Properties": {
                                "SubnetId": { "Ref": subnet },
                                "AllocationId": { "Fn::GetAtt": [eip, "AllocationId"] },
                            },
                        }),
                    );
                    outputs.insert(format!("{nat}Id"), json!({ "Value": { "Ref": nat } }));
                }
            } else {
                // The private subnet egresses through the NAT gateway of its own zone where
                // the row declares one per zone, and through the single one otherwise.
                let serving = zone.min(nat_gateways - 1) + 1;
                resources.insert(
                    route,
                    json!({
                        "Type": "AWS::EC2::Route",
                        "Properties": {
                            "RouteTableId": { "Ref": table },
                            "DestinationCidrBlock": ALL_IPV4,
                            "NatGatewayId": { "Ref": nat_id(serving) },
                        },
                    }),
                );
            }
        }
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
}

/// The one route of `table_id` for `0.0.0.0/0`, as the element text of its `target` member.
async fn default_route_target(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    target: &str,
) -> String {
    let (status, answered) = ec2(router, "DescribeRouteTables", &[("RouteTableId.1", table)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected Amazon EC2 to hold the route table {table}, got HTTP {status} \
         with body {answered}"
    );
    let routes = block_of(&answered, "routeSet").unwrap_or_default();
    let default: Vec<&str> = items_of(routes)
        .into_iter()
        .filter(|route| opt_element(route, "destinationCidrBlock").as_deref() == Some(ALL_IPV4))
        .collect();
    assert_eq!(
        default.len(),
        1,
        "case {case:?}: expected the table {table} to carry exactly one {ALL_IPV4} route, got \
         {default:?} from {answered}"
    );
    opt_element(default[0], target).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the {ALL_IPV4} route of {table} to name a <{target}>, got \
             {:?}",
            default[0]
        )
    })
}

/// Read a NAT gateway until Amazon EC2 reports it available, and answer the state it was last
/// reported in.
async fn nat_gateway_state(router: &MultiServiceRouter, case: &str, nat: &str) -> String {
    let mut state = String::new();
    for _ in 0..POLL_ATTEMPTS {
        let (status, answered) = ec2(router, "DescribeNatGateways", &[("NatGatewayId.1", nat)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon EC2 to hold the NAT gateway {nat}, got HTTP {status} \
             with body {answered}"
        );
        state = opt_element(&answered, "state").unwrap_or_default();
        if state == NAT_AVAILABLE {
            return state;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    state
}

/// A stack that fans a VPC out over the Availability Zones the Region reports settles, and the
/// estate it settles on is the one it declared: two subnets per zone spanned, each in the zone
/// it was declared in, one route table per subnet beside the VPC's implicit main table, the
/// public subnets routing `0.0.0.0/0` to the ONE internet gateway attached to the VPC and the
/// private ones to the NAT gateway serving them, which settles available.
#[tokio::test]
async fn a_vpc_spanning_every_reported_zone_settles_with_the_estate_it_declared() {
    let router = build_router();
    let reported = reported_zone_names(&router).await;

    for row in ROWS {
        let case = row.case;
        let zones = spanned(row, &reported);
        let nat_gateways = nat_gateway_count(row, zones.len());
        let described = deploy(
            &router,
            case,
            row.stack,
            &vpc_template(zones, nat_gateways),
        )
        .await;
        let outputs = outputs_of(&described);
        let published = |key: &str| -> String {
            outputs.get(key).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish the output {key}: {outputs:?}")
            })
        };
        let vpc = published("VpcId");

        // Two subnets per zone spanned, and each one is in the zone it was declared in — so
        // the fan-out really reached every zone the Region reports rather than piling up in
        // one.
        let (status, subnets) = ec2(
            &router,
            "DescribeSubnets",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeSubnets, got {status} with body \
             {subnets}"
        );
        let reported_subnets: BTreeSet<String> =
            elements(&subnets, "subnetId").into_iter().collect();
        let declared_subnets: BTreeSet<String> = (1..=zones.len())
            .flat_map(|index| ["Public", "Private"].map(|group| subnet_id(group, index)))
            .map(|logical| published(&format!("{logical}Id")))
            .collect();
        assert_eq!(
            reported_subnets, declared_subnets,
            "case {case:?}: expected the VPC {vpc} to hold exactly the {} subnets the stack \
             declared, got {reported_subnets:?} from {subnets}",
            declared_subnets.len()
        );
        for (zone, zone_name) in zones.iter().enumerate() {
            for group in ["Public", "Private"] {
                let subnet = published(&format!("{}Id", subnet_id(group, zone + 1)));
                let (status, answered) =
                    ec2(&router, "DescribeSubnets", &[("SubnetId.1", &subnet)]).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected Amazon EC2 to hold the subnet {subnet}, got HTTP \
                     {status} with body {answered}"
                );
                let placed = opt_element(&answered, "availabilityZone").unwrap_or_default();
                assert_eq!(
                    &placed, zone_name,
                    "case {case:?}: expected the {group} subnet {subnet} in {zone_name}, got \
                     {placed:?} from {answered}"
                );
            }
        }

        // One explicit route table per subnet PLUS the implicit main table the VPC carries.
        let (status, tables) = ec2(
            &router,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeRouteTables, got {status} with body \
             {tables}"
        );
        let reported_tables: BTreeSet<String> =
            elements(&tables, "routeTableId").into_iter().collect();
        let expected_tables = 2 * zones.len() + 1;
        assert_eq!(
            reported_tables.len(),
            expected_tables,
            "case {case:?}: expected the VPC {vpc} to carry {expected_tables} route tables (one \
             per subnet plus its implicit main table), got {reported_tables:?}"
        );

        // Exactly one internet gateway is attached to the VPC, and it is the one the stack
        // published: the filter a client holding only the VPC id has to use resolves it.
        let (status, gateways) = ec2(
            &router,
            "DescribeInternetGateways",
            &[
                ("Filter.1.Name", "attachment.vpc-id"),
                ("Filter.1.Value.1", &vpc),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeInternetGateways, got {status} with \
             body {gateways}"
        );
        assert_eq!(
            elements(&gateways, "internetGatewayId"),
            vec![published("IgwId")],
            "case {case:?}: expected exactly the gateway the stack published to be attached to \
             {vpc}, got {gateways}"
        );

        // Each public subnet's table routes out through that gateway, and each private one
        // through the NAT gateway serving its zone.
        let gateway = published("IgwId");
        for index in 1..=zones.len() {
            let table = published(&format!("{}Id", table_id("Public", index)));
            let target = default_route_target(&router, case, &table, "gatewayId").await;
            assert_eq!(
                target, gateway,
                "case {case:?}: expected public subnet {index}'s default route to target the \
                 VPC's internet gateway {gateway}, got {target:?}"
            );
        }
        let mut private_targets: BTreeSet<String> = BTreeSet::new();
        for index in 1..=zones.len() {
            let table = published(&format!("{}Id", table_id("Private", index)));
            let target = default_route_target(&router, case, &table, "natGatewayId").await;
            let serving = (index - 1).min(nat_gateways - 1) + 1;
            let declared = published(&format!("{}Id", nat_id(serving)));
            assert_eq!(
                target, declared,
                "case {case:?}: expected private subnet {index}'s default route to target the \
                 NAT gateway {declared}, got {target:?}"
            );
            private_targets.insert(target);
        }
        assert_eq!(
            private_targets.len(),
            nat_gateways,
            "case {case:?}: expected the private subnets to egress through {nat_gateways} \
             distinct NAT gateway(s), got {private_targets:?}"
        );

        // Every NAT gateway the stack created settles in a state a client can act on, which is
        // what the stack held its own completion open for.
        for nat in &private_targets {
            let state = nat_gateway_state(&router, case, nat).await;
            assert_eq!(
                state, NAT_AVAILABLE,
                "case {case:?}: expected the NAT gateway {nat} to settle {NAT_AVAILABLE}, got \
                 {state:?}"
            );
        }
    }
}

/// The other half of the convergence rule: a declaration the owning service REFUSES fails the
/// stack to a terminal status instead of holding it open for ever.
///
/// The refused declaration is a subnet in a zone name the Region does not report — read off
/// `DescribeAvailabilityZones` rather than assumed — which is exactly what a client that did
/// NOT clamp its zone count to the Region's would ask for.
#[tokio::test]
async fn a_subnet_in_a_zone_the_region_does_not_report_fails_the_stack() {
    let case = "a subnet in a zone the Region does not report";
    let router = build_router();
    let reported = reported_zone_names(&router).await;
    // A zone name of this Region that is NOT among the reported ones: the letters run from
    // `a`, so one past the last reported zone's names none of them.
    let beyond = format!(
        "{REGION}{}",
        char::from(b'a' + u8::try_from(reported.len()).unwrap_or(0))
    );
    assert!(
        !reported.contains(&beyond),
        "case {case:?}: expected {beyond} not to be a zone of {REGION}, got {reported:?}"
    );

    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vpc": {
                "Type": "AWS::EC2::VPC",
                "Properties": { "CidrBlock": VPC_CIDR },
            },
            "Subnet": {
                "Type": "AWS::EC2::Subnet",
                "Properties": {
                    "VpcId": { "Ref": "Vpc" },
                    "AvailabilityZone": beyond,
                    "CidrBlock": "10.0.0.0/20",
                },
            },
        },
    });
    let described = create_and_settle(&router, case, "az-span-beyond", &template).await;
    let reported_status = opt_element(&described, "StackStatus").unwrap_or_default();
    assert!(
        !reported_status.is_empty() && !reported_status.ends_with("_IN_PROGRESS"),
        "case {case:?}: expected the stack to reach a terminal status rather than stall, got \
         {reported_status:?} in {described}"
    );
    assert_ne!(
        reported_status, "CREATE_COMPLETE",
        "case {case:?}: expected the refused subnet to fail the stack, got \
         {reported_status:?}; its failed resources: {}",
        failures(&router, "az-span-beyond").await
    );
    let failed = failures(&router, "az-span-beyond").await;
    assert!(
        failed.contains("Subnet"),
        "case {case:?}: expected the stack's events to name the refused subnet, got {failed:?}"
    );
}
