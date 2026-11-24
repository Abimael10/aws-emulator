// SPDX-License-Identifier: CC0-1.0

//! What a load balancer's TEARDOWN gives back to the account's VPC, and so whether the
//! network it sat in can be torn down after it.
//!
//! RULE: a load balancer's own elastic network interfaces — one service-managed interface per
//! subnet it is attached to, reported in the account's EC2 inventory under the load balancing
//! service's requester alias — carry its traffic, so they do not outlive it: the delete that
//! takes the load balancer releases every one of them, and nothing the service created holds a
//! place in those subnets afterwards. While it is there, each of its interfaces holds its
//! subnet against `ec2:DeleteSubnet` exactly as any other interface in the subnet does; once
//! it is gone, the same subnets — and the VPC around them — delete. A create the service
//! REFUSES places no interface at all, so a subnet named by a request that was turned down is
//! never held either.
//!
//! Why it matters: an interface left behind for a load balancer that is gone makes the subnet,
//! and transitively the VPC, permanently undeletable. A client that deletes its load balancer
//! and then tears its network down polls `DescribeNetworkInterfaces` until the service's
//! interfaces drain and then calls `DeleteSubnet`; if they never drain the poll runs to its
//! own cap and every `DeleteSubnet` keeps being refused, so the teardown never converges and
//! the account leaks subnets and VPCs between runs.
//!
//! The rows span what the rule generalises over: the load balancer FAMILY (application,
//! network, gateway — each with its own modelled interface type), the SCHEME (an internal
//! placement, whose nodes hold private addresses only, and an internet-facing one, whose nodes
//! hold public addresses beside them), and HOW MANY subnets of how many zones the placement
//! spans, hence how many interfaces the teardown has to give back. The configuration a report
//! happens to mention is a single row.
//!
//! The fact spans two services — ELBv2 places and releases the interfaces, EC2 holds the
//! inventory, answers the describes and decides whether a subnet still has dependencies — so
//! the suite lives beside the router. `elbv2_own_interfaces_under_ec2_filters.rs` asserts
//! WHICH interfaces each EC2 filter selects, and `elbv2_host_counts_and_interfaces.rs` asserts
//! WHAT one of them carries; this suite asserts what the teardown makes DELETABLE. Nothing
//! reaches into either service's state struct: every assertion is a wire answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the resources belong to.
const REGION: &str = "us-east-1";

/// The API version an ec2Query client sends beside `Action`.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an awsQuery client naming the ELBv2 model sends beside `Action`.
const ELB_VERSION: &str = "2015-12-01";

/// `NetworkInterface$RequesterId` of an interface Elastic Load Balancing created and manages,
/// which is what a client filters its VPC's interfaces by to find the network presence of its
/// load balancers.
const SERVICE_REQUESTER: &str = "amazon-elb";

/// The error code EC2 refuses the delete of a subnet something still sits in with.
const SUBNET_IN_USE_CODE: &str = "DependencyViolation";

/// The error code Elastic Load Balancing answers for an ARN naming no load balancer, which is
/// what the deleted one's ARN names afterwards.
const LOAD_BALANCER_GONE_CODE: &str = "LoadBalancerNotFound";

/// How long this suite keeps looking for a condition it waits on, and how often it looks
/// again. Generous on purpose: it waits for the CONDITION rather than for a fixed time, so
/// what it asserts does not depend on how fast or how loaded the host is.
const DRAIN_DEADLINE: Duration = Duration::from_secs(60);
/// See [`DRAIN_DEADLINE`].
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// One emulator carrying Amazon EC2 and Elastic Load Balancing v2 over ONE shared engine,
/// exactly as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One query-protocol answer, reduced to what a client branches on.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// The query error envelope's `Code`, or `""` when the answer carries no envelope.
    fn error_code(&self) -> String {
        crate::query_wire::blocks_of(&self.body, "Error")
            .first()
            .and_then(|envelope| opt_element(envelope, "Code"))
            .unwrap_or_default()
    }
}

/// Call one query-protocol operation of `signing_name`, at `version`, with these form members.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Reply {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// Call one Amazon EC2 action.
async fn ec2_call(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Reply {
    query(router, "ec2", EC2_API_VERSION, action, members).await
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let reply = ec2_call(router, action, members).await;
    assert_eq!(
        reply.status, 200,
        "setup: EC2 {action} must succeed, got {} with {}",
        reply.status, reply.body
    );
    reply.body
}

/// Call one Elastic Load Balancing v2 operation over its awsQuery wire.
async fn elbv2_call(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Reply {
    query(
        router,
        "elasticloadbalancingv2",
        ELB_VERSION,
        action,
        members,
    )
    .await
}

/// One ELBv2 operation that must succeed, answered with its XML body.
async fn elbv2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let reply = elbv2_call(router, action, members).await;
    assert_eq!(
        reply.status, 200,
        "setup: ELBv2 {action} must succeed, got {} with {}",
        reply.status, reply.body
    );
    reply.body
}

/// The network a row places its load balancer in: the VPC, one subnet per zone in the order
/// created, a security group of it, and the internet gateway attached to it when the row's
/// placement needs one.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    security_group: String,
    gateway: Option<String>,
}

/// Create that network through EC2's OWN operations, so every identifier handed to Elastic
/// Load Balancing is one the owner of subnets handed out.
async fn seed_network(
    router: &MultiServiceRouter,
    second_octet: u8,
    zones: &[&str],
    name: &str,
    needs_gateway: bool,
) -> Network {
    let cidr = format!("10.{second_octet}.0.0/16");
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", cidr.as_str())]).await,
        "vpcId",
    );
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        let created = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", subnet_cidr.as_str()),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(element(&created, "subnetId"));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", name),
                ("GroupDescription", "a group guarding a load balancer"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    // "The nodes of an Internet-facing load balancer have public IP addresses"
    // (`CreateLoadBalancerInput$Scheme`), and a public address inside a VPC routes to the
    // internet only through an internet gateway attached to it — so the row whose placement
    // carries public addresses gets one, through EC2's own operations.
    let gateway = if needs_gateway {
        let gateway = element(
            &ec2(router, "CreateInternetGateway", &[]).await,
            "internetGatewayId",
        );
        ec2(
            router,
            "AttachInternetGateway",
            &[
                ("InternetGatewayId", gateway.as_str()),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await;
        Some(gateway)
    } else {
        None
    };
    Network {
        vpc,
        subnets,
        security_group,
        gateway,
    }
}

/// Every interface EC2 reports in `vpc` that Elastic Load Balancing created and manages — the
/// read a client does to find the network presence of its load balancers.
async fn service_interface_ids(router: &MultiServiceRouter, vpc: &str) -> Vec<String> {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc),
            ("Filter.2.Name", "requester-id"),
            ("Filter.2.Value.1", SERVICE_REQUESTER),
        ],
    )
    .await;
    elements(&body, "networkInterfaceId")
}

/// Look again until `holds` is true of the service's interfaces in `vpc`, and answer the ids it
/// held then.
///
/// Bounded by [`DRAIN_DEADLINE`]; a deadline that passes fails naming the ids last seen — the
/// poll a client tearing its network down runs, and the one the emulator has to let converge.
async fn interfaces_until(
    router: &MultiServiceRouter,
    case: &str,
    description: &str,
    vpc: &str,
    holds: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + DRAIN_DEADLINE;
    loop {
        let held = service_interface_ids(router, vpc).await;
        if holds(&held) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {description} never came about; the VPC still holds {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// One interface of the account, reduced to what the rows assert over.
#[derive(Debug)]
struct Interface {
    subnet: String,
    interface_type: String,
    requester: String,
    requester_managed: String,
    description: String,
}

/// One interface as EC2 describes it when asked for exactly it, which is how a client reads an
/// interface off an id a filter answered with.
async fn describe_interface(router: &MultiServiceRouter, id: &str) -> Interface {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", id)],
    )
    .await;
    Interface {
        subnet: opt_element(&body, "subnetId").unwrap_or_default(),
        interface_type: opt_element(&body, "interfaceType").unwrap_or_default(),
        requester: opt_element(&body, "requesterId").unwrap_or_default(),
        requester_managed: opt_element(&body, "requesterManaged").unwrap_or_default(),
        description: opt_element(&body, "description").unwrap_or_default(),
    }
}

/// One row: the load balancer a client places, and the interfaces its placement therefore
/// holds in the account's inventory.
struct Row {
    case: &'static str,
    /// The load balancer's name, unique per row so no row is answered the idempotent repeat of
    /// another.
    name: &'static str,
    /// `Type`, or `None` for the default (application) family.
    lb_type: Option<&'static str>,
    /// `Scheme`, or `None` where the family carries none — "You can't specify a scheme for a
    /// Gateway Load Balancer" (`CreateLoadBalancerInput$Scheme`).
    scheme: Option<&'static str>,
    /// Whether the request names the VPC's security group: documented for the Application and
    /// Network families (`CreateLoadBalancerInput$SecurityGroups`).
    security_groups: bool,
    /// One subnet per zone, so also how many interfaces the placement holds.
    zones: &'static [&'static str],
    /// The `NetworkInterfaceType` this family's own interfaces carry, from the modelled enum
    /// (`smithy_query(query="shape",shape="NetworkInterfaceType")`: `load_balancer`,
    /// `network_load_balancer`, `gateway_load_balancer`).
    interface_type: &'static str,
}

/// The rows span the family, the scheme and the placement's width.
const ROWS: &[Row] = &[
    Row {
        case: "an internal application load balancer over two subnets in two zones",
        name: "elbv2-teardown-internal-alb",
        lb_type: None,
        scheme: Some("internal"),
        security_groups: true,
        zones: &["us-east-1a", "us-east-1b"],
        interface_type: "load_balancer",
    },
    Row {
        case: "an internet-facing application load balancer, whose nodes carry public addresses",
        name: "elbv2-teardown-public-alb",
        lb_type: None,
        scheme: Some("internet-facing"),
        security_groups: true,
        zones: &["us-east-1a", "us-east-1b"],
        interface_type: "load_balancer",
    },
    Row {
        case: "an internal network load balancer in a single subnet",
        name: "elbv2-teardown-nlb",
        lb_type: Some("network"),
        scheme: Some("internal"),
        security_groups: true,
        zones: &["us-east-1c"],
        interface_type: "network_load_balancer",
    },
    Row {
        case: "a gateway load balancer, which carries no scheme, over two subnets",
        name: "elbv2-teardown-gwlb",
        lb_type: Some("gateway"),
        scheme: None,
        security_groups: false,
        zones: &["us-east-1a", "us-east-1b"],
        interface_type: "gateway_load_balancer",
    },
];

#[tokio::test]
async fn deleting_a_load_balancer_frees_the_subnets_its_interfaces_held() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let network = seed_network(
            &router,
            40 + u8::try_from(index).unwrap_or(0),
            row.zones,
            row.name,
            row.scheme == Some("internet-facing"),
        )
        .await;

        // Before the load balancer exists the VPC holds no interface of this service, so what
        // the teardown gives back below is exactly what the create placed.
        let before = service_interface_ids(&router, &network.vpc).await;
        assert!(
            before.is_empty(),
            "case {case:?}: no interface of the service must exist before the create, got \
             {before:?}"
        );

        let mut members: Vec<(String, String)> = vec![("Name".to_string(), row.name.to_string())];
        if let Some(lb_type) = row.lb_type {
            members.push(("Type".to_string(), lb_type.to_string()));
        }
        if let Some(scheme) = row.scheme {
            members.push(("Scheme".to_string(), scheme.to_string()));
        }
        if row.security_groups {
            members.push((
                "SecurityGroups.member.1".to_string(),
                network.security_group.clone(),
            ));
        }
        for (position, subnet) in network.subnets.iter().enumerate() {
            members.push((format!("Subnets.member.{}", position + 1), subnet.clone()));
        }
        let created = elbv2(&router, "CreateLoadBalancer", &borrowed(&members)).await;
        let arn = element(&created, "LoadBalancerArn");

        // One service-managed interface per subnet the load balancer is attached to, each one
        // of the family's modelled type and each naming the load balancer it belongs to, so a
        // client reading the inventory can tell whose interface it is.
        let held = interfaces_until(
            &router,
            case,
            &format!("one interface per attached subnet ({})", row.zones.len()),
            &network.vpc,
            |held| held.len() == row.zones.len(),
        )
        .await;
        assert_eq!(
            held.len(),
            row.zones.len(),
            "case {case:?}: expected one interface per attached subnet, got {held:?}"
        );
        // `ELB <family>/<name>/<id>`, which is the load balancer's own ARN from the family code
        // onwards — read off the ARN the create answered rather than written out here.
        let expected_description = format!(
            "ELB {}",
            arn.split_once("loadbalancer/")
                .map(|(_, suffix)| suffix)
                .unwrap_or_default()
        );
        for id in &held {
            let interface = describe_interface(&router, id).await;
            assert_eq!(
                (
                    interface.interface_type.as_str(),
                    interface.requester.as_str(),
                    interface.requester_managed.as_str(),
                    interface.description.as_str(),
                ),
                (
                    row.interface_type,
                    SERVICE_REQUESTER,
                    "true",
                    expected_description.as_str(),
                ),
                "case {case:?}: expected the family's service-managed interface naming its load \
                 balancer, got {interface:?}"
            );
            assert!(
                network.subnets.contains(&interface.subnet),
                "case {case:?}: an interface sits in a subnet the load balancer is attached to, \
                 got {interface:?} beside {:?}",
                network.subnets
            );
        }

        // While the load balancer is there its interface holds the subnet exactly as any other
        // interface in it does: the subnet has a dependency and does not delete.
        let refused = ec2_call(
            &router,
            "DeleteSubnet",
            &[("SubnetId", network.subnets[0].as_str())],
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, SUBNET_IN_USE_CODE),
            "case {case:?}: a subnet the load balancer still sits in must not delete, got {} {}",
            refused.status,
            refused.body
        );

        // The delete takes the load balancer …
        let deleted = elbv2_call(
            &router,
            "DeleteLoadBalancer",
            &[("LoadBalancerArn", arn.as_str())],
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must succeed, got {} {}",
            deleted.status, deleted.body
        );
        let gone = elbv2_call(
            &router,
            "DescribeLoadBalancers",
            &[("LoadBalancerArns.member.1", arn.as_str())],
        )
        .await;
        assert_eq!(
            (gone.status, gone.error_code().as_str()),
            (400, LOAD_BALANCER_GONE_CODE),
            "case {case:?}: the deleted load balancer must name nothing, got {} {}",
            gone.status,
            gone.body
        );

        // … and with it every interface it held, so the poll a client runs converges to an
        // empty answer instead of to its own cap.
        let drained = interfaces_until(
            &router,
            case,
            "every interface of the load balancer being given back",
            &network.vpc,
            <[String]>::is_empty,
        )
        .await;
        assert!(
            drained.is_empty(),
            "case {case:?}: the delete must release every interface it placed, got {drained:?}"
        );

        // Which is what lets the client finish: each subnet it was attached to, the group
        // guarding it, the gateway the placement needed and the VPC around them all delete.
        for subnet in &network.subnets {
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
        if let Some(gateway) = &network.gateway {
            for (action, members) in [
                (
                    "DetachInternetGateway",
                    vec![
                        ("InternetGatewayId", gateway.as_str()),
                        ("VpcId", network.vpc.as_str()),
                    ],
                ),
                (
                    "DeleteInternetGateway",
                    vec![("InternetGatewayId", gateway.as_str())],
                ),
            ] {
                let reply = ec2_call(&router, action, &members).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case:?}: {action} must succeed on teardown, got {} {}",
                    reply.status, reply.body
                );
            }
        }
        let torn_down = ec2_call(&router, "DeleteVpc", &[("VpcId", network.vpc.as_str())]).await;
        assert_eq!(
            torn_down.status, 200,
            "case {case:?}: the VPC must delete once nothing of the load balancer is left in it, \
             got {} {}",
            torn_down.status, torn_down.body
        );
    }
}

/// One row of the counter-case: a create the service turns down, and so a placement it never
/// performs.
struct RefusedRow {
    case: &'static str,
    /// The name the request carries.
    name: &'static str,
    /// How many of the VPC's own subnets the request names.
    subnets: usize,
    /// A subnet id the account holds none of, named beside them when the row has one.
    absent_subnet: Option<&'static str>,
}

/// The rows span the kinds of request this operation refuses: a `Name` outside its documented
/// form ("must not begin with \"internal-\""), a placement with fewer subnets than the family
/// requires ("[Application Load Balancers] You must specify subnets from at least two
/// Availability Zones"), and a subnet EC2 — which owns subnets — holds none of.
const REFUSED_ROWS: &[RefusedRow] = &[
    RefusedRow {
        case: "a name beginning with the prefix the model reserves",
        name: "internal-elbv2-teardown-refused",
        subnets: 2,
        absent_subnet: None,
    },
    RefusedRow {
        case: "an application load balancer placed in a single subnet",
        name: "elbv2-teardown-one-subnet",
        subnets: 1,
        absent_subnet: None,
    },
    RefusedRow {
        case: "a subnet the account holds none of, beside one it does",
        name: "elbv2-teardown-absent-subnet",
        subnets: 1,
        absent_subnet: Some("subnet-0f1e2d3c4b5a69780"),
    },
];

#[tokio::test]
async fn a_refused_create_places_no_interface_and_holds_no_subnet() {
    let router = emulator();
    let network = seed_network(
        &router,
        50,
        &["us-east-1a", "us-east-1b"],
        "elbv2-teardown-refused",
        false,
    )
    .await;

    for row in REFUSED_ROWS {
        let case = row.case;
        let mut members: Vec<(String, String)> = vec![
            ("Name".to_string(), row.name.to_string()),
            ("Scheme".to_string(), "internal".to_string()),
            (
                "SecurityGroups.member.1".to_string(),
                network.security_group.clone(),
            ),
        ];
        let mut named = 0;
        for subnet in network.subnets.iter().take(row.subnets) {
            named += 1;
            members.push((format!("Subnets.member.{named}"), subnet.clone()));
        }
        if let Some(absent) = row.absent_subnet {
            named += 1;
            members.push((format!("Subnets.member.{named}"), absent.to_string()));
        }
        let refused = elbv2_call(&router, "CreateLoadBalancer", &borrowed(&members)).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: the create must be refused, got {} {}",
            refused.status, refused.body
        );
        let placed = service_interface_ids(&router, &network.vpc).await;
        assert!(
            placed.is_empty(),
            "case {case:?}: a refused create must place no interface, got {placed:?}"
        );
    }

    // So the network named by requests that were all turned down tears down with nothing left
    // holding it.
    for subnet in &network.subnets {
        let removed = ec2_call(&router, "DeleteSubnet", &[("SubnetId", subnet.as_str())]).await;
        assert_eq!(
            removed.status, 200,
            "case \"a subnet only refused creates named\": {subnet} must delete, got {} {}",
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
        "case \"a group only refused creates named\": it must delete, got {} {}",
        ungoverned.status, ungoverned.body
    );
    let torn_down = ec2_call(&router, "DeleteVpc", &[("VpcId", network.vpc.as_str())]).await;
    assert_eq!(
        torn_down.status, 200,
        "case \"a VPC only refused creates named\": it must delete, got {} {}",
        torn_down.status, torn_down.body
    );
}

/// The owned form members as the borrowed pairs [`query`] takes.
fn borrowed(members: &[(String, String)]) -> Vec<(&str, &str)> {
    members
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}
