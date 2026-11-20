// SPDX-License-Identifier: CC0-1.0

//! Whether a load balancer can FORWARD a client's request to a registered target is decided by
//! the security groups of the resource that registration names — the same firewall its health
//! checks are admitted by.
//!
//! The rule asserted here: **a `forward` reaches the target over a connection the load balancer
//! opens from its own network interfaces to the port the registration receives traffic on, so
//! it is made only where an inbound rule of a security group governing that target covers that
//! port and names the load balancer as its source. Where the account's records show that no
//! rule does, the load balancer cannot establish the connection and answers its OWN `502` —
//! never the target's own response — even when that registration is the only target its group
//! holds and the load balancer is therefore sending requests across its unhealthy ones.**
//!
//! A security group "controls the traffic that is allowed to reach and leave the resources that
//! it is associated with", and its inbound rules "control the incoming traffic that's allowed to
//! reach the instance" (Amazon VPC User Guide, "Security groups"); `TargetDescription$Port` is
//! "the port on which the target is listening", which is the port that connection goes to. The
//! load balancer's own `502` is the status it answers for a target it cannot connect to, which
//! `all/alb_instance_target.rs` pins for the sibling condition (a registration with no server
//! behind it at all).
//!
//! The rows differ along what the rule generalises over: how the registration names the target
//! (its instance id, an address an interface of it holds) and what the governing group's inbound
//! rules say (the load balancer's own group on the traffic port, that group on another port, the
//! traffic port from an unrelated group, every protocol, an address range holding the load
//! balancer's own addresses, a range holding none of them, and no inbound rule at all). Each row
//! reads the health its group reaches first, so the answer it then asserts is the one the load
//! balancer gives with that verdict in — a 200 from a target it may send requests to, and its
//! own 502 for one it cannot reach.
//!
//! Everything is driven through the two services' own APIs: EC2 is mounted beside this one on
//! the suite's own engine, so the VPC, the subnets, the security groups, their rules, the
//! instances and the web server each instance's user data installed are EC2's own records, and
//! the load balancers, the listeners and the registrations are written through this service's
//! operations. The data-plane requests carry no AWS credential, exactly as a client of the
//! published `DNSName` sends them.

use std::time::{Duration, Instant};

use base64::Engine as _;
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::send;
use crate::provisioning_loop::{action, call, first_text, members, p};
use crate::target_security_group_health_check::{ec2_ok, emulator_with_ec2, reported};

/// How long a step waits for a clock-owed move — an instance's to `running`, a load balancer's
/// to `active`, the verdict a group's own health-check cadence reaches — and how often it looks.
/// Generous on purpose, many times the cadence below: the deadline is only here so a failure is
/// a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The health-check cadence and thresholds every target group below is created with: a verdict
/// takes a threshold of consecutive checks, each one interval after the last, so this suite asks
/// for the shortest interval `HealthCheckIntervalSeconds` admits ("the range is 5-300") and the
/// fewest checks a threshold may ask for ("the range is 2-10").
const INTERVAL_SECONDS: &str = "5";
const THRESHOLD: &str = "2";

/// An AMI id the launches name; nothing here asserts anything about the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The network every row runs in: one VPC and two subnets of it in different zones, which is
/// what an Application Load Balancer is placed across. The zones belong to the Region every
/// request of this crate's suites is signed for.
const VPC_CIDR: &str = "10.81.0.0/16";
const SUBNETS: [(&str, &str); 2] = [
    ("10.81.1.0/24", "us-west-2a"),
    ("10.81.2.0/24", "us-west-2b"),
];

/// The port every target group receives traffic on, which is also the port its health check
/// connects to: no row names a `HealthCheckPort` of its own.
const TRAFFIC_PORT: i32 = 80;

/// A port no target group of this suite forwards to, for a rule that admits the load balancer
/// somewhere other than the port the request goes to.
const OTHER_PORT: i32 = 8080;

/// An address range that holds no address of this network, so no address a load balancer of it
/// holds is inside it.
const FOREIGN_RANGE: &str = "192.168.0.0/16";

/// The marker each instance's own user-data program writes into the document it serves, so a
/// body anything else produced fails an assertion.
const MARKER: &str = "from-the-target-7f2a";

/// The `TargetHealth$State` a row's registration settles on before its request is sent.
const HEALTHY: &str = "healthy";
const UNHEALTHY: &str = "unhealthy";

/// The statuses a row is answered with: the target's own for a connection the firewall admits,
/// and the load balancer's own for one it cannot make.
const SERVED_STATUS: u16 = 200;
const UNREACHABLE_STATUS: u16 = 502;

/// The user data of every instance below: it installs the Apache package of the Amazon Linux
/// family, starts its unit and writes the document the rows ask for into the directory that
/// package serves.
fn serving_user_data() -> String {
    format!(
        "#!/bin/bash\n\
         yum update -y\n\
         yum install -y httpd\n\
         systemctl enable httpd\n\
         systemctl start httpd\n\
         echo \"{MARKER}\" > /var/www/html/index.html\n"
    )
}

/// Where one inbound rule grants access FROM, as `AuthorizeSecurityGroupIngress` names a source:
/// "a CIDR range, a security group, or a prefix list".
#[derive(Clone, Copy)]
enum Source {
    /// The load balancer's own security group — the source a rule for load-balancer traffic
    /// names.
    LoadBalancer,
    /// A security group of the account no load balancer is associated with.
    Unrelated,
    /// An address range.
    Cidr(&'static str),
}

/// One inbound rule of the group governing a target: its protocol, its port range (absent for a
/// rule that names none) and the one source it grants.
#[derive(Clone, Copy)]
struct InboundRule {
    protocol: &'static str,
    ports: Option<(i32, i32)>,
    source: Source,
}

/// One row: how the registration names the target, the inbound rule the group governing it
/// carries, and what both imply — the health the group settles on and the answer the client gets.
struct Row {
    case: &'static str,
    /// The row's own name, which names its security group, its target group and its load
    /// balancer.
    name: &'static str,
    /// `instance` to register the target by its instance id, `ip` by its address.
    target_type: &'static str,
    /// The private address the row's instance is launched at, which an `ip` row registers.
    address: &'static str,
    /// The one inbound rule the target's security group carries, or `None` for a group with no
    /// inbound rule at all.
    rule: Option<InboundRule>,
    /// The `TargetHealth$State` every registration of the row's group settles on.
    health: &'static str,
    /// The status the client is answered with, and the body — the document the target's own
    /// server holds, or nothing for a status the load balancer answered itself.
    status: u16,
    serves_its_document: bool,
}

/// The rows, in creation order.
const ROWS: [Row; 9] = [
    Row {
        case: "a group admitting the load balancer on the port the request goes to",
        name: "fwdsg-admitted",
        target_type: "instance",
        address: "10.81.1.101",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((TRAFFIC_PORT, TRAFFIC_PORT)),
            source: Source::LoadBalancer,
        }),
        health: HEALTHY,
        status: SERVED_STATUS,
        serves_its_document: true,
    },
    Row {
        case: "the recorded configuration: a group with no inbound rule at all",
        name: "fwdsg-norule",
        target_type: "instance",
        address: "10.81.1.102",
        rule: None,
        health: UNHEALTHY,
        status: UNREACHABLE_STATUS,
        serves_its_document: false,
    },
    Row {
        case: "a group admitting the load balancer on another port",
        name: "fwdsg-otherport",
        target_type: "instance",
        address: "10.81.1.103",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((OTHER_PORT, OTHER_PORT)),
            source: Source::LoadBalancer,
        }),
        health: UNHEALTHY,
        status: UNREACHABLE_STATUS,
        serves_its_document: false,
    },
    Row {
        case: "a group admitting the traffic port from an unrelated group",
        name: "fwdsg-othergroup",
        target_type: "instance",
        address: "10.81.1.104",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((TRAFFIC_PORT, TRAFFIC_PORT)),
            source: Source::Unrelated,
        }),
        health: UNHEALTHY,
        status: UNREACHABLE_STATUS,
        serves_its_document: false,
    },
    Row {
        case: "a group admitting every protocol from the load balancer",
        name: "fwdsg-allprotocols",
        target_type: "instance",
        address: "10.81.1.105",
        rule: Some(InboundRule {
            protocol: "-1",
            ports: None,
            source: Source::LoadBalancer,
        }),
        health: HEALTHY,
        status: SERVED_STATUS,
        serves_its_document: true,
    },
    Row {
        case: "a group admitting the traffic port from the VPC's own range",
        name: "fwdsg-cidr",
        target_type: "instance",
        address: "10.81.1.106",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((TRAFFIC_PORT, TRAFFIC_PORT)),
            source: Source::Cidr(VPC_CIDR),
        }),
        health: HEALTHY,
        status: SERVED_STATUS,
        serves_its_document: true,
    },
    Row {
        case: "a group admitting the traffic port from a range holding no address of the load \
               balancer",
        name: "fwdsg-foreigncidr",
        target_type: "instance",
        address: "10.81.1.107",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((TRAFFIC_PORT, TRAFFIC_PORT)),
            source: Source::Cidr(FOREIGN_RANGE),
        }),
        health: UNHEALTHY,
        status: UNREACHABLE_STATUS,
        serves_its_document: false,
    },
    Row {
        case: "the same workload registered by its address, admitted on the traffic port",
        name: "fwdsg-addr-admitted",
        target_type: "ip",
        address: "10.81.1.108",
        rule: Some(InboundRule {
            protocol: "tcp",
            ports: Some((TRAFFIC_PORT, TRAFFIC_PORT)),
            source: Source::LoadBalancer,
        }),
        health: HEALTHY,
        status: SERVED_STATUS,
        serves_its_document: true,
    },
    Row {
        case: "the same workload registered by its address, with no inbound rule at all",
        name: "fwdsg-addr-norule",
        target_type: "ip",
        address: "10.81.1.109",
        rule: None,
        health: UNHEALTHY,
        status: UNREACHABLE_STATUS,
        serves_its_document: false,
    },
];

/// The account's network, as the two services read it.
struct Network {
    vpc: String,
    subnets: Vec<String>,
    /// The group every load balancer below is associated with, which is the source a target's
    /// own inbound rule names.
    load_balancer_group: String,
    /// A group of the account no load balancer is associated with.
    unrelated_group: String,
}

/// Create one security group of `vpc` and answer its `GroupId`.
async fn security_group(router: &MultiServiceRouter, vpc: &str, name: &str) -> String {
    let created = ec2_ok(
        router,
        "setup",
        "CreateSecurityGroup",
        &[
            p("VpcId", vpc),
            p("GroupName", name),
            p("GroupDescription", "a group of this suite"),
        ],
    )
    .await;
    reported("setup", &created, "groupId")
}

/// Create the network through EC2's own API: the VPC with the internet gateway an
/// internet-facing load balancer needs, its two subnets, and the two groups every row reads
/// against.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let created = ec2_ok(router, "setup", "CreateVpc", &[p("CidrBlock", VPC_CIDR)]).await;
    let vpc = reported("setup", &created, "vpcId");
    let gateway = ec2_ok(router, "setup", "CreateInternetGateway", &[]).await;
    let gateway = reported("setup", &gateway, "internetGatewayId");
    ec2_ok(
        router,
        "setup",
        "AttachInternetGateway",
        &[p("InternetGatewayId", &gateway), p("VpcId", &vpc)],
    )
    .await;
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
    let load_balancer_group = security_group(router, &vpc, "fwdsg-alb").await;
    let unrelated_group = security_group(router, &vpc, "fwdsg-unrelated").await;
    Network {
        vpc,
        subnets,
        load_balancer_group,
        unrelated_group,
    }
}

/// Authorize `rule` on `group`, as the one inbound rule the row's target group carries.
async fn authorize(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    rule: &InboundRule,
    network: &Network,
) {
    let mut fields = vec![
        p("GroupId", group),
        p("IpPermissions.1.IpProtocol", rule.protocol),
    ];
    if let Some((first, last)) = rule.ports {
        fields.push(p("IpPermissions.1.FromPort", &first.to_string()));
        fields.push(p("IpPermissions.1.ToPort", &last.to_string()));
    }
    match rule.source {
        Source::LoadBalancer => fields.push(p(
            "IpPermissions.1.Groups.1.GroupId",
            &network.load_balancer_group,
        )),
        Source::Unrelated => {
            fields.push(p("IpPermissions.1.Groups.1.GroupId", &network.unrelated_group));
        }
        Source::Cidr(cidr) => fields.push(p("IpPermissions.1.IpRanges.1.CidrIp", cidr)),
    }
    ec2_ok(router, case, "AuthorizeSecurityGroupIngress", &fields).await;
}

/// Launch the row's instance: in its own security group, at its own address, running the server
/// its user data installs. Answers its instance id.
async fn run_instance(
    router: &MultiServiceRouter,
    case: &str,
    network: &Network,
    group: &str,
    address: &str,
) -> String {
    let launched = ec2_ok(
        router,
        case,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", &network.subnets[0]),
            p("SecurityGroupId.1", group),
            p("PrivateIpAddress", address),
            p(
                "UserData",
                &base64::engine::general_purpose::STANDARD.encode(serving_user_data()),
            ),
        ],
    )
    .await;
    reported(case, &launched, "instanceId")
}

/// Look at the instance until EC2 reports it `running` — the move the launch owes, which is also
/// when its guest comes up and runs its user data.
async fn running(router: &MultiServiceRouter, case: &str, instance: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ec2_ok(
            router,
            case,
            "DescribeInstances",
            &[p("InstanceId.1", instance)],
        )
        .await;
        let is_running = described.contains("<name>running</name>");
        if is_running || Instant::now() >= deadline {
            assert!(
                is_running,
                "case {case:?}: expected EC2 to report {instance} running, got {described}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Create the row's target group — of its own target type, in the network's VPC, health-checking
/// the document its instance serves — and answer its ARN.
async fn target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target_type: &str,
    network: &Network,
) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", &TRAFFIC_PORT.to_string()));
    form.push(p("TargetType", target_type));
    form.push(p("VpcId", &network.vpc));
    form.push(p("HealthCheckProtocol", "HTTP"));
    form.push(p("HealthCheckPath", "/"));
    form.push(p("HealthCheckIntervalSeconds", INTERVAL_SECONDS));
    form.push(p("HealthyThresholdCount", THRESHOLD));
    form.push(p("UnhealthyThresholdCount", THRESHOLD));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateTargetGroup, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn in {}", reply.body))
}

/// Register `id` with the target group `arn` names.
async fn register(router: &MultiServiceRouter, case: &str, arn: &str, id: &str) {
    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", arn));
    form.push(p("Targets.member.1.Id", id));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from RegisterTargets, got {} body {}",
        reply.status, reply.body
    );
}

/// Create the row's Application Load Balancer across the network's subnets, in the network's own
/// security group, with an HTTP listener on the traffic port forwarding to `tg_arn`. Answers its
/// ARN and the name it published.
async fn load_balancer(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    network: &Network,
    tg_arn: &str,
) -> (String, String) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    for (index, subnet) in network.subnets.iter().enumerate() {
        form.push(p(&format!("Subnets.member.{}", index + 1), subnet));
    }
    form.push(p("SecurityGroups.member.1", &network.load_balancer_group));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateLoadBalancer, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no LoadBalancerArn in {}", reply.body));
    let dns_name = reply
        .text("DNSName")
        .unwrap_or_else(|| panic!("case {case:?}: no DNSName in {}", reply.body));
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", &TRAFFIC_PORT.to_string()));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", tg_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from CreateListener, got {} body {}",
        reply.status, reply.body
    );
    (arn, dns_name)
}

/// Look at the load balancer until it reports `State.Code` `active`.
async fn active(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let mut form = action("DescribeLoadBalancers");
        form.push(p("LoadBalancerArns.member.1", arn));
        let reply = call(router, &form).await;
        let code = first_text(&reply.body, "Code").unwrap_or_default();
        if code == "active" || Instant::now() >= deadline {
            assert_eq!(
                code, "active",
                "case {case:?}: expected the load balancer to reach active, got {code:?} in {}",
                reply.body
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Look at the target group until every registration it holds reports `wanted`, and answer every
/// state it reported on the way there — so a row can assert what it must never have reported.
async fn health_reaching(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let mut form = action("DescribeTargetHealth");
        form.push(p("TargetGroupArn", arn));
        let reply = call(router, &form).await;
        let states: Vec<String> = members(&reply.body, "TargetHealthDescriptions")
            .iter()
            .map(|description| first_text(description, "State").unwrap_or_default())
            .collect();
        for state in &states {
            if seen.last() != Some(state) {
                seen.push(state.clone());
            }
        }
        let settled = !states.is_empty() && states.iter().all(|state| state == wanted);
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected every registration to report {wanted}, got {states:?} \
                 (states seen: {seen:?})"
            );
            return seen;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// What one row is answered out of: its target group and the name its load balancer published.
struct Deployed {
    target_group_arn: String,
    dns_name: String,
}

/// RULE: a `forward` is made only over a connection a security group governing the target
/// admits from the load balancer on the port the registration receives traffic on; where none
/// does, the load balancer answers its own 502 rather than the target's response.
#[tokio::test]
async fn a_forward_is_made_only_where_the_targets_security_groups_admit_the_load_balancer() {
    let router = emulator_with_ec2();
    let network = seed_network(&router).await;

    // Every row's own security group, its one inbound rule, and the instance that group
    // governs — launched BEFORE any load balancer is placed, so each instance holds the address
    // its row registers rather than one a load balancer's own interface took.
    let mut instances: Vec<String> = Vec::new();
    for row in &ROWS {
        let group = security_group(&router, &network.vpc, row.name).await;
        if let Some(rule) = row.rule.as_ref() {
            authorize(&router, row.case, &group, rule, &network).await;
        }
        instances.push(run_instance(&router, row.case, &network, &group, row.address).await);
    }
    // The rest of every row is written before anything waits, so the spans the launches and the
    // load balancers owe run at the same time and the health-check cadences overlap.
    let mut deployed: Vec<Deployed> = Vec::new();
    let mut load_balancers: Vec<String> = Vec::new();
    for (row, instance) in ROWS.iter().zip(instances.iter()) {
        let target_group_arn =
            target_group(&router, row.case, row.name, row.target_type, &network).await;
        // "if the target type is instance, specify an instance ID" and "if the target type is
        // ip, specify an IP address" (`TargetDescription$Id`): one workload, named the way the
        // row's own group names it.
        let target_id = if row.target_type == "ip" {
            row.address.to_string()
        } else {
            instance.clone()
        };
        register(&router, row.case, &target_group_arn, &target_id).await;
        let (arn, dns_name) = load_balancer(
            &router,
            row.case,
            &format!("{}-lb", row.name),
            &network,
            &target_group_arn,
        )
        .await;
        load_balancers.push(arn);
        deployed.push(Deployed {
            target_group_arn,
            dns_name,
        });
    }
    for (row, instance) in ROWS.iter().zip(instances.iter()) {
        running(&router, row.case, instance).await;
    }
    for (row, arn) in ROWS.iter().zip(load_balancers.iter()) {
        active(&router, row.case, arn).await;
    }

    for (row, Deployed { target_group_arn, dns_name }) in ROWS.iter().zip(deployed.iter()) {
        // The verdict the row's own checks reach, which is the state the load balancer makes
        // its forwarding decision under.
        let seen = health_reaching(&router, row.case, target_group_arn, row.health).await;
        if row.health == UNHEALTHY {
            assert!(
                !seen.iter().any(|state| state == HEALTHY),
                "case {:?}: a target no security group admits the load balancer to must never \
                 be reported {HEALTHY}, got the states {seen:?}",
                row.case
            );
        }
        let answered = send(&router, "GET", dns_name, "/", &[]).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected the status {}, got {} body {:?}",
            row.case, row.status, answered.status, answered.body
        );
        let body = answered.body.trim_end().to_string();
        if row.serves_its_document {
            assert_eq!(
                body, MARKER,
                "case {:?}: expected the target's own document {MARKER:?}, got {body:?}",
                row.case
            );
        } else {
            assert!(
                body.is_empty(),
                "case {:?}: a status the load balancer answered itself carries no body, got \
                 {body:?}",
                row.case
            );
        }
    }
}
