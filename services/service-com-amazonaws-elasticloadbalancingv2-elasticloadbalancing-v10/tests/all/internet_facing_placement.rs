// SPDX-License-Identifier: CC0-1.0

//! An INTERNET-FACING load balancer needs the VPC it is placed in to have an internet
//! gateway attached.
//!
//! The rule asserted here: **"the nodes of an Internet-facing load balancer have public IP
//! addresses" (`CreateLoadBalancerInput$Scheme`), and a public address inside a VPC reaches
//! the internet only through an internet gateway ATTACHED to that VPC — so a request placing
//! one in a VPC EC2 reports no attached gateway for is refused `InvalidSubnet` (HTTP 400),
//! names the VPC it would have placed it in, and creates nothing, while the same subnets
//! carry an INTERNAL load balancer, whose nodes have only private addresses. The attachment
//! is read LIVE from EC2 on every call: attaching a gateway — with no route added — makes the
//! same refused request succeed, and detaching it again refuses it afresh. What each accepted
//! load balancer PUBLISHES is the address this emulator serves it at, whatever the scheme:
//! the two reads of a created load balancer — the create's own answer and a later describe —
//! report the one `DNSName` a client dials.**
//!
//! The rows differ along what the rule generalises over: the scheme (`internal`,
//! `internet-facing`), the load balancer TYPE (`application`, `network`, and `gateway`, the
//! family that carries no scheme at all), whether EC2 reports a gateway attached to the VPC,
//! and — for the rule's own reading — whether the placement resolved a VPC at all and whether
//! EC2 answered about it.
//!
//! Both halves this crate owns are driven: the RULE itself, tabled over what EC2 reports, and
//! the WIRE PATH with EC2 mounted beside this service on one engine, where the VPC, its
//! subnets, its security group and its internet gateway are the records EC2's own operations
//! wrote.
//!
//! The wire harness (`action`, `call`, `p`) comes from `provisioning_loop` and the EC2 front
//! door (`emulator_with_ec2`, `ec2_ok`, `reported`) from `target_security_group_health_check`;
//! nothing here restates them.

use smithy_server_runtime::MultiServiceRouter;

use crate::provisioning_loop::{action, call, p};
use crate::target_security_group_health_check::{ec2_ok, emulator_with_ec2, reported};
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    check_internet_gateway, network::InternetGatewayLookup, no_internet_gateway_message,
    LoadBalancerFamily, INTERNAL_SCHEME, INTERNET_FACING_SCHEME,
};

/// The network every wire row runs in: one VPC, and two subnets of it in different zones,
/// which is what an Application Load Balancer is placed across. The zones belong to the
/// Region every request of this crate's suites is signed for.
const VPC_CIDR: &str = "10.91.0.0/16";
const SUBNETS: [(&str, &str); 2] = [
    ("10.91.1.0/24", "us-west-2a"),
    ("10.91.2.0/24", "us-west-2b"),
];

/// One row of the rule table: the case name, the family and scheme of the load balancer, the
/// VPC the placement resolved (`None` for one that resolved none), what EC2 answered about
/// that VPC's internet gateway, and whether the placement must be refused.
struct RuleRow {
    case: &'static str,
    family: LoadBalancerFamily,
    scheme: &'static str,
    vpc_id: Option<&'static str>,
    gateway: InternetGatewayLookup,
    refused: bool,
}

#[test]
fn the_rule_reads_the_vpcs_gateway_for_a_public_placement_alone() {
    // The VPC a refusal names, which is the one the placement resolved.
    const VPC: &str = "vpc-0a1b2c3d4e5f60718";
    let rows: Vec<RuleRow> = vec![
        RuleRow {
            case: "an internet-facing Application Load Balancer in a gateway-less VPC",
            family: LoadBalancerFamily::Application,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Detached,
            refused: true,
        },
        RuleRow {
            case: "an internet-facing Application Load Balancer in a VPC with a gateway",
            family: LoadBalancerFamily::Application,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Attached,
            refused: false,
        },
        RuleRow {
            // Nothing is known about the VPC's gateway — there was no EC2 to ask — so
            // nothing is refused on it.
            case: "an internet-facing placement EC2 answered nothing about",
            family: LoadBalancerFamily::Application,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Unknown,
            refused: false,
        },
        RuleRow {
            // The nodes of an internal load balancer "have only private IP addresses", which
            // need no gateway.
            case: "an internal Application Load Balancer in a gateway-less VPC",
            family: LoadBalancerFamily::Application,
            scheme: INTERNAL_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Detached,
            refused: false,
        },
        RuleRow {
            case: "an internet-facing Network Load Balancer in a gateway-less VPC",
            family: LoadBalancerFamily::Network,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Detached,
            refused: true,
        },
        RuleRow {
            case: "an internal Network Load Balancer in a gateway-less VPC",
            family: LoadBalancerFamily::Network,
            scheme: INTERNAL_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Detached,
            refused: false,
        },
        RuleRow {
            // "You can't specify a scheme for a Gateway Load Balancer"
            // (`CreateLoadBalancerInput$Scheme`): that family's nodes carry no public
            // addresses whatever scheme a request spelled out, so its placement is judged on
            // no gateway.
            case: "a Gateway Load Balancer in a gateway-less VPC",
            family: LoadBalancerFamily::Gateway,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: Some(VPC),
            gateway: InternetGatewayLookup::Detached,
            refused: false,
        },
        RuleRow {
            // A placement whose subnets agree on no VPC resolves none, and a refusal needs
            // the VPC it is about.
            case: "an internet-facing placement that resolved no VPC",
            family: LoadBalancerFamily::Application,
            scheme: INTERNET_FACING_SCHEME,
            vpc_id: None,
            gateway: InternetGatewayLookup::Detached,
            refused: false,
        },
    ];
    for row in rows {
        let judged = check_internet_gateway(row.family, row.scheme, row.vpc_id, row.gateway);
        let want: Result<(), String> = if row.refused {
            Err(no_internet_gateway_message(
                row.vpc_id.expect("a refused row names the VPC"),
            ))
        } else {
            Ok(())
        };
        assert_eq!(
            judged, want,
            "case {:?}: expected {want:?}, got {judged:?}",
            row.case
        );
    }
}

/// The account's network for the wire half: the VPC, its two subnets and the security group a
/// load balancer of it is associated with.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    security_group: String,
}

/// Create that network through EC2's own API. No internet gateway is created with it: that is
/// what the rows below attach and detach.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let created = ec2_ok(router, "setup", "CreateVpc", &[p("CidrBlock", VPC_CIDR)]).await;
    let vpc = reported("setup", &created, "vpcId");
    let mut subnets: Vec<String> = Vec::new();
    for (cidr, zone) in SUBNETS {
        let created = ec2_ok(
            router,
            "setup",
            "CreateSubnet",
            &[
                p("VpcId", &vpc),
                p("CidrBlock", cidr),
                p("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(reported("setup", &created, "subnetId"));
    }
    let created = ec2_ok(
        router,
        "setup",
        "CreateSecurityGroup",
        &[
            p("VpcId", &vpc),
            p("GroupName", "igw-rule-group"),
            // `CreateSecurityGroupRequest$Description` travels under its own `xmlName`,
            // `GroupDescription`.
            p("GroupDescription", "a group of this suite"),
        ],
    )
    .await;
    let security_group = reported("setup", &created, "groupId");
    Network {
        vpc,
        subnets,
        security_group,
    }
}

/// One load balancer the wire half asks for: the row's case name, the `Type` and `Scheme` it
/// names (`None` where the request spells no scheme out, which is the documented
/// internet-facing default), and whether the create must be refused while the VPC has no
/// internet gateway attached.
struct WireRow {
    case: &'static str,
    name: &'static str,
    lb_type: &'static str,
    scheme: Option<&'static str>,
    refused_without_a_gateway: bool,
}

/// Ask for that load balancer in `network` and answer the reply.
async fn create(router: &MultiServiceRouter, row: &WireRow, network: &Network) -> crate::provisioning_loop::Reply {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", row.name));
    form.push(p("Type", row.lb_type));
    if let Some(scheme) = row.scheme {
        form.push(p("Scheme", scheme));
    }
    for (index, subnet) in network.subnets.iter().enumerate() {
        form.push(p(&format!("Subnets.member.{}", index + 1), subnet));
    }
    // A Gateway Load Balancer carries no security group at all, so only the two families
    // that do name one.
    if row.lb_type != "gateway" {
        form.push(p("SecurityGroups.member.1", &network.security_group));
    }
    call(router, &form).await
}

/// What `DescribeLoadBalancers` reports for the load balancer `name` names: the status and the
/// body, so a row can assert both a stored record and the absence of one.
async fn describe(router: &MultiServiceRouter, name: &str) -> crate::provisioning_loop::Reply {
    let mut form = action("DescribeLoadBalancers");
    form.push(p("Names.member.1", name));
    call(router, &form).await
}

/// Assert that the create of `row` was refused for the VPC's missing gateway, and that it
/// stored nothing.
async fn assert_refused(router: &MultiServiceRouter, row: &WireRow, network: &Network) {
    let reply = create(router, row, network).await;
    let case = row.case;
    assert_eq!(
        (reply.status, reply.error_code().as_str()),
        (400, "InvalidSubnet"),
        "case {case:?}: expected 400 InvalidSubnet, got {} body {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.text("Message").as_deref(),
        Some(no_internet_gateway_message(&network.vpc).as_str()),
        "case {case:?}: expected the refusal to name the VPC, got {}",
        reply.body
    );
    let read = describe(router, row.name).await;
    assert_eq!(
        (read.status, read.error_code().as_str()),
        (400, "LoadBalancerNotFound"),
        "case {case:?}: a refused create must store nothing, got {} body {}",
        read.status,
        read.body
    );
}

/// Assert that the create of `row` was served, and that the stored record carries the scheme
/// and the VPC of the placement.
async fn assert_served(router: &MultiServiceRouter, row: &WireRow, network: &Network) {
    let reply = create(router, row, network).await;
    let case = row.case;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected the create to be served, got {} body {}",
        reply.status, reply.body
    );
    let read = describe(router, row.name).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected the stored load balancer to be reported, got {} body {}",
        read.status, read.body
    );
    assert_eq!(
        read.text("VpcId").as_deref(),
        Some(network.vpc.as_str()),
        "case {case:?}: expected the placement's VPC to be reported, got {}",
        read.body
    );
    let expected_scheme = row.scheme.unwrap_or(INTERNET_FACING_SCHEME);
    assert_eq!(
        read.text("Scheme").as_deref(),
        Some(expected_scheme),
        "case {case:?}: expected the scheme {expected_scheme:?} to be reported, got {}",
        read.body
    );
    // What the load balancer publishes is the address this emulator serves it at, whichever
    // scheme it carries: `Scheme` decides which clients may reach it in a real network, not
    // whether the published value is the address it answers on. Both reads — the create's own
    // answer and the later describe — report that one value. The whole table of published
    // forms is `load_balancer_dns_name_form.rs`.
    let mut published_values: Vec<(&str, String)> = Vec::new();
    for (source, body) in [("the create", &reply), ("a later read", &read)] {
        let published = body
            .text("DNSName")
            .unwrap_or_else(|| panic!("case {case:?}: no DNSName in {}", body.body));
        if row.lb_type != "gateway" {
            assert!(
                published.parse::<std::net::SocketAddr>().is_ok(),
                "case {case:?}: {source} published {published:?}, which is no address a client \
                 can dial"
            );
        }
        published_values.push((source, published));
    }
    assert_eq!(
        published_values[0].1, published_values[1].1,
        "case {case:?}: the two reads must report one published address, got {published_values:?}"
    );
}

#[tokio::test]
async fn a_public_placement_needs_the_vpcs_internet_gateway_over_the_wire() {
    let router = emulator_with_ec2();
    let network = seed_network(&router).await;

    let rows: Vec<WireRow> = vec![
        WireRow {
            case: "an internet-facing Application Load Balancer",
            name: "igw-rule-alb-public",
            lb_type: "application",
            scheme: Some(INTERNET_FACING_SCHEME),
            refused_without_a_gateway: true,
        },
        WireRow {
            // The documented default is an internet-facing load balancer, so a request that
            // spells no scheme out asks for the same public placement.
            case: "an Application Load Balancer naming no scheme",
            name: "igw-rule-alb-default",
            lb_type: "application",
            scheme: None,
            refused_without_a_gateway: true,
        },
        WireRow {
            case: "an internal Application Load Balancer",
            name: "igw-rule-alb-private",
            lb_type: "application",
            scheme: Some(INTERNAL_SCHEME),
            refused_without_a_gateway: false,
        },
        WireRow {
            case: "an internet-facing Network Load Balancer",
            name: "igw-rule-nlb-public",
            lb_type: "network",
            scheme: Some(INTERNET_FACING_SCHEME),
            refused_without_a_gateway: true,
        },
        WireRow {
            case: "an internal Network Load Balancer",
            name: "igw-rule-nlb-private",
            lb_type: "network",
            scheme: Some(INTERNAL_SCHEME),
            refused_without_a_gateway: false,
        },
    ];

    // ---------------------------------------------------------------------
    // The VPC has no internet gateway: every public placement is refused and stores
    // nothing, and every private one is served.
    // ---------------------------------------------------------------------
    for row in &rows {
        if row.refused_without_a_gateway {
            assert_refused(&router, row, &network).await;
        } else {
            assert_served(&router, row, &network).await;
        }
    }

    // ---------------------------------------------------------------------
    // A gateway is attached to the VPC — and no route is added to any route table — so
    // every placement refused above is now served, under the very name it was refused
    // for: the refusal left that name free.
    // ---------------------------------------------------------------------
    let created = ec2_ok(&router, "attach", "CreateInternetGateway", &[]).await;
    let gateway = reported("attach", &created, "internetGatewayId");
    ec2_ok(
        &router,
        "attach",
        "AttachInternetGateway",
        &[p("InternetGatewayId", &gateway), p("VpcId", &network.vpc)],
    )
    .await;
    for row in rows.iter().filter(|row| row.refused_without_a_gateway) {
        assert_served(&router, row, &network).await;
    }

    // ---------------------------------------------------------------------
    // The attachment is read live on every call: once the public load balancers that
    // depend on the gateway are gone — AWS refuses to detach a gateway a public load
    // balancer still depends on — and the gateway is detached again, a public placement is
    // refused afresh.
    // ---------------------------------------------------------------------
    for row in rows.iter().filter(|row| row.refused_without_a_gateway) {
        delete(&router, row).await;
    }
    ec2_ok(
        &router,
        "detach",
        "DetachInternetGateway",
        &[p("InternetGatewayId", &gateway), p("VpcId", &network.vpc)],
    )
    .await;
    let after_detach = WireRow {
        case: "an internet-facing Application Load Balancer after the gateway is detached",
        name: "igw-rule-alb-detached",
        lb_type: "application",
        scheme: Some(INTERNET_FACING_SCHEME),
        refused_without_a_gateway: true,
    };
    assert_refused(&router, &after_detach, &network).await;
}

/// Delete the load balancer of `row`, through the operation that owns it, and assert the read
/// that found it no longer does.
async fn delete(router: &MultiServiceRouter, row: &WireRow) {
    let case = row.case;
    let read = describe(router, row.name).await;
    let arn = read
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no LoadBalancerArn in {}", read.body));
    let mut form = action("DeleteLoadBalancer");
    form.push(p("LoadBalancerArn", &arn));
    let deleted = call(router, &form).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected the delete to be served, got {} body {}",
        deleted.status, deleted.body
    );
    let after = describe(router, row.name).await;
    assert_eq!(
        (after.status, after.error_code().as_str()),
        (400, "LoadBalancerNotFound"),
        "case {case:?}: expected the deleted load balancer to be gone, got {} body {}",
        after.status,
        after.body
    );
}
