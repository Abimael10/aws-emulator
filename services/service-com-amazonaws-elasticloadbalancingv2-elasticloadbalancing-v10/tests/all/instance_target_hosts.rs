// SPDX-License-Identifier: CC0-1.0

//! What the HOST behind an `instance` registration decides about that registration, driven
//! over the wire against the EC2 records the host is held in.
//!
//! Two rules, both about the same thing — an `instance` registration names a host Amazon EC2
//! owns ("if the target type of the target group is `instance`, specify an instance ID",
//! `TargetDescription$Id`), so what that host IS and WHERE it lives is read there:
//!
//! 1. **A registration may only name a host the target group's own VPC holds. One naming a
//!    host EC2 holds in another VPC is refused `InvalidTarget` (HTTP 400), whose message names
//!    the target group's VPC and every target of the request that VPC does not hold — as a
//!    list, in the order the request named them — and registers none of them.** The message
//!    says nothing about where those targets DO live: the verdict is that the group's network
//!    does not hold them.
//! 2. **A load balancer keeps no registration of a host whose lifecycle has ENDED: once EC2
//!    reports the instance behind an `instance` registration terminating, every registration
//!    of it leaves the target group, whatever port it was registered on. A host that can still
//!    come back (one EC2 reports stopped) and one that is running keep their registrations, and
//!    so does a registration that names an ADDRESS rather than an instance.**
//!
//! Everything is driven through the two services' own APIs — EC2 is mounted beside this one on
//! the suite's own engine ([`emulator_with_ec2`]), so the VPCs, the subnets, the security group
//! and the instances are the records EC2's own readers answer with, the lifecycle moves are
//! EC2's own `TerminateInstances` and `StopInstances`, and what the target groups hold is read
//! back through `DescribeTargetHealth`. Nothing sleeps for a transition: each wait looks again
//! until the condition holds.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::provisioning_loop::{action, call, first_text, members, p, Reply};
use crate::target_security_group_health_check::{ec2_ok, emulator_with_ec2, reported};

/// How long a wait gives a clock-owed move — an instance's launch, a registration leaving a
/// target group — and how often it looks. Generous on purpose: the deadline is only here so a
/// failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The health-check cadence the routed target group is created with: the shortest
/// `HealthCheckIntervalSeconds` admits ("the range is 5-300").
const INTERVAL_SECONDS: &str = "5";

/// An AMI id a launch names; nothing here asserts anything about the image.
const AMI: &str = "ami-0123456789abcdef0";

/// The port the target groups receive traffic on, and a second port one host is registered on
/// as well — "you can register each EC2 instance or IP address with the same target group
/// multiple times using different ports".
const TRAFFIC_PORT: i32 = 80;
const OTHER_PORT: i32 = 8081;

/// One network of the account: a VPC and one subnet of it.
struct Network {
    vpc: String,
    subnet: String,
}

/// Create a VPC with one subnet in `zone` and the internet gateway an internet-facing load
/// balancer needs, through EC2's own API.
async fn network(router: &MultiServiceRouter, cidr: &str, subnet_cidr: &str, zone: &str) -> Network {
    let created = ec2_ok(router, "setup", "CreateVpc", &[p("CidrBlock", cidr)]).await;
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
    let created = ec2_ok(
        router,
        "setup",
        "CreateSubnet",
        &[
            p("VpcId", &vpc),
            p("CidrBlock", subnet_cidr),
            p("AvailabilityZone", zone),
        ],
    )
    .await;
    let subnet = reported("setup", &created, "subnetId");
    Network { vpc, subnet }
}

/// Create one security group of `vpc` admitting every protocol from the whole network, so a
/// health check of a target in it is one the account's rules admit.
async fn open_security_group(router: &MultiServiceRouter, vpc: &str, name: &str) -> String {
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
    let group = reported("setup", &created, "groupId");
    ec2_ok(
        router,
        "setup",
        "AuthorizeSecurityGroupIngress",
        &[
            p("GroupId", &group),
            p("IpPermissions.1.IpProtocol", "-1"),
            p("IpPermissions.1.IpRanges.1.CidrIp", "0.0.0.0/0"),
        ],
    )
    .await;
    group
}

/// Launch one instance in `subnet` at `address`, in `group`, answering its id.
async fn run_instance(
    router: &MultiServiceRouter,
    case: &str,
    subnet: &str,
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
            p("SubnetId", subnet),
            p("SecurityGroupId.1", group),
            p("PrivateIpAddress", address),
        ],
    )
    .await;
    reported(case, &launched, "instanceId")
}

/// Look at the instance until EC2 reports it in `state` — the move its last call owes it.
async fn instance_state(router: &MultiServiceRouter, case: &str, instance: &str, state: &str) {
    let deadline = Instant::now() + DEADLINE;
    let wanted = format!("<name>{state}</name>");
    loop {
        let described = ec2_ok(
            router,
            case,
            "DescribeInstances",
            &[p("InstanceId.1", instance)],
        )
        .await;
        let reached = described.contains(&wanted);
        if reached || Instant::now() >= deadline {
            assert!(
                reached,
                "case {case:?}: expected EC2 to report {instance} {state}, got {described}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// Create a target group of `target_type` in `vpc` and answer its ARN.
async fn target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    target_type: &str,
    vpc: &str,
) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", &TRAFFIC_PORT.to_string()));
    form.push(p("TargetType", target_type));
    form.push(p("VpcId", vpc));
    form.push(p("HealthCheckIntervalSeconds", INTERVAL_SECONDS));
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

/// Create an Application Load Balancer across `subnets` whose HTTP listener forwards to
/// `tg_arn` — which is what makes that target group one a load balancer checks.
async fn load_balancer_forwarding_to(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[&str],
    group: &str,
    tg_arn: &str,
) {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    for (index, subnet) in subnets.iter().enumerate() {
        form.push(p(&format!("Subnets.member.{}", index + 1), subnet));
    }
    form.push(p("SecurityGroups.member.1", group));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup {name}: expected 200 from CreateLoadBalancer, got {} body {}",
        reply.status, reply.body
    );
    let arn = reply
        .text("LoadBalancerArn")
        .unwrap_or_else(|| panic!("setup {name}: no LoadBalancerArn in {}", reply.body));
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", &TRAFFIC_PORT.to_string()));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", tg_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup {name}: expected 200 from CreateListener, got {} body {}",
        reply.status, reply.body
    );
}

/// Register `targets` — each an id and the port it is registered on — with the target group
/// `arn` names, as the answer that call gave.
async fn register(
    router: &MultiServiceRouter,
    arn: &str,
    targets: &[(&str, Option<i32>)],
) -> Reply {
    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", arn));
    for (index, (id, port)) in targets.iter().enumerate() {
        let at = index + 1;
        form.push(p(&format!("Targets.member.{at}.Id"), id));
        if let Some(port) = port {
            form.push(p(&format!("Targets.member.{at}.Port"), &port.to_string()));
        }
    }
    call(router, &form).await
}

/// Register `targets` with `arn`, which must be accepted.
async fn register_ok(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    targets: &[(&str, Option<i32>)],
) {
    let reply = register(router, arn, targets).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from RegisterTargets, got {} body {}",
        reply.status, reply.body
    );
}

/// Every registration the target group `arn` holds, as `(target id, port)`, sorted — what the
/// group holds, read the way a client reads it.
async fn registrations(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<(String, String)> {
    let mut form = action("DescribeTargetHealth");
    form.push(p("TargetGroupArn", arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from DescribeTargetHealth, got {} body {}",
        reply.status, reply.body
    );
    let mut held: Vec<(String, String)> = members(&reply.body, "TargetHealthDescriptions")
        .iter()
        .map(|description| {
            // The `Target` of the description, so the port read is the one the registration
            // receives traffic on rather than the health-check port beside it.
            let target = description
                .split("</Target>")
                .next()
                .unwrap_or(description)
                .to_string();
            (
                first_text(&target, "Id").unwrap_or_default(),
                first_text(&target, "Port").unwrap_or_default(),
            )
        })
        .collect();
    held.sort();
    held
}

/// One registration key, as [`registrations`] reports it.
fn key(id: &str, port: i32) -> (String, String) {
    (id.to_string(), port.to_string())
}

/// The `Message` of an awsQuery error envelope, as a client that PARSES the XML reads it: the
/// serializer escapes the quoting an error message carries, and the parse undoes that, so an
/// assertion on the sentence compares the sentence rather than its encoding.
fn message_of(body: &str) -> String {
    first_text(body, "Message")
        .unwrap_or_default()
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Look at `arn` until the registrations it holds are exactly `wanted`.
async fn registrations_reaching(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &[(String, String)],
) {
    let deadline = Instant::now() + DEADLINE;
    let mut expected = wanted.to_vec();
    expected.sort();
    loop {
        let held = registrations(router, case, arn).await;
        if held == expected || Instant::now() >= deadline {
            assert_eq!(
                held, expected,
                "case {case:?}: expected the target group to hold {expected:?}, got {held:?}"
            );
            return;
        }
        ::tokio::time::sleep(POLL).await;
    }
}

/// RULE: a load balancer keeps no registration of a host whose lifecycle has ended — once EC2
/// reports the instance behind an `instance` registration terminating, every registration of it
/// leaves the target group — while a stopped host, a running one and a registration naming an
/// address keep theirs.
#[tokio::test]
async fn every_registration_of_a_terminated_instance_leaves_its_target_group() {
    let router = emulator_with_ec2();
    let home = network(&router, "10.70.0.0/16", "10.70.1.0/24", "us-west-2a").await;
    // A second subnet, in another zone, because that is what an Application Load Balancer is
    // placed across.
    let second = ec2_ok(
        &router,
        "setup",
        "CreateSubnet",
        &[
            p("VpcId", &home.vpc),
            p("CidrBlock", "10.70.2.0/24"),
            p("AvailabilityZone", "us-west-2b"),
        ],
    )
    .await;
    let second = reported("setup", &second, "subnetId");
    let group = open_security_group(&router, &home.vpc, "hostlc-group").await;

    const TERMINATED_ADDRESS: &str = "10.70.1.10";
    let terminated = run_instance(
        &router,
        "the instance that is terminated",
        &home.subnet,
        &group,
        TERMINATED_ADDRESS,
    )
    .await;
    let stopped = run_instance(
        &router,
        "the instance that is stopped",
        &home.subnet,
        &group,
        "10.70.1.11",
    )
    .await;
    let kept = run_instance(
        &router,
        "the instance that keeps running",
        &home.subnet,
        &group,
        "10.70.1.12",
    )
    .await;
    for (case, instance) in [
        ("the instance that is terminated", &terminated),
        ("the instance that is stopped", &stopped),
        ("the instance that keeps running", &kept),
    ] {
        instance_state(&router, case, instance, "running").await;
    }

    let hosts = target_group(&router, "instance targets", "hostlc-inst", "instance", &home.vpc).await;
    // The group is one a load balancer forwards to, which is what has its registrations
    // checked — and what the one clock of this service runs for.
    load_balancer_forwarding_to(
        &router,
        "hostlc-lb",
        &[home.subnet.as_str(), second.as_str()],
        &group,
        &hosts,
    )
    .await;
    // The terminated host is registered TWICE, on two ports, so the rule is read over every
    // registration of one host rather than over one key.
    register_ok(
        &router,
        "instance targets",
        &hosts,
        &[
            (terminated.as_str(), Some(TRAFFIC_PORT)),
            (terminated.as_str(), Some(OTHER_PORT)),
            (stopped.as_str(), Some(TRAFFIC_PORT)),
            (kept.as_str(), Some(TRAFFIC_PORT)),
        ],
    )
    .await;
    // And the same host's ADDRESS is registered with an `ip` group: such a registration names
    // the address, not the host, so no instance's lifecycle is a statement about it.
    let addresses = target_group(&router, "address targets", "hostlc-ip", "ip", &home.vpc).await;
    register_ok(
        &router,
        "address targets",
        &addresses,
        &[(TERMINATED_ADDRESS, Some(TRAFFIC_PORT))],
    )
    .await;

    let before = registrations(&router, "before the lifecycle moves", &hosts).await;
    let mut all = vec![
        key(&terminated, TRAFFIC_PORT),
        key(&terminated, OTHER_PORT),
        key(&stopped, TRAFFIC_PORT),
        key(&kept, TRAFFIC_PORT),
    ];
    all.sort();
    assert_eq!(
        before, all,
        "case \"before the lifecycle moves\": expected the group to hold {all:?}, got {before:?}"
    );

    ec2_ok(
        &router,
        "the instance that is terminated",
        "TerminateInstances",
        &[p("InstanceId.1", &terminated)],
    )
    .await;
    ec2_ok(
        &router,
        "the instance that is stopped",
        "StopInstances",
        &[p("InstanceId.1", &stopped)],
    )
    .await;
    instance_state(
        &router,
        "the instance that is stopped",
        &stopped,
        "stopped",
    )
    .await;

    // Both registrations of the terminated host leave the group; the stopped host's and the
    // running host's stay. The drain each leaving registration passes through is short, so
    // nothing here asserts on catching it — what the rule is about is that they LEAVE.
    registrations_reaching(
        &router,
        "a terminated host's registrations",
        &hosts,
        &[key(&stopped, TRAFFIC_PORT), key(&kept, TRAFFIC_PORT)],
    )
    .await;
    // The address registration of the very same host is untouched.
    let held = registrations(&router, "address targets", &addresses).await;
    assert_eq!(
        held,
        vec![key(TERMINATED_ADDRESS, TRAFFIC_PORT)],
        "case \"address targets\": expected the `ip` registration of the terminated host's \
         address to stay, got {held:?}"
    );
}

/// One row of the cross-VPC refusal table: the targets one `RegisterTargets` carries, and the
/// ids its message must name, in order.
struct Row {
    case: &'static str,
    /// Which of the suite's instances the request names, each with the port it asks for.
    targets: Vec<(String, Option<i32>)>,
    /// The ids the refusal names, in the order the message lists them — empty for a request
    /// that is accepted.
    named: Vec<String>,
}

/// RULE: a registration may only name a host the target group's own VPC holds; one naming a
/// host EC2 holds in another VPC is refused `InvalidTarget` (HTTP 400), whose message names the
/// group's VPC and every target of the request that VPC does not hold, and registers none of
/// them.
#[tokio::test]
async fn registering_a_host_of_another_vpc_is_refused_naming_the_target_group_vpc() {
    let router = emulator_with_ec2();
    let home = network(&router, "10.71.0.0/16", "10.71.1.0/24", "us-west-2a").await;
    let away = network(&router, "10.72.0.0/16", "10.72.1.0/24", "us-west-2a").await;
    let home_group = open_security_group(&router, &home.vpc, "hostvpc-home").await;
    let away_group = open_security_group(&router, &away.vpc, "hostvpc-away").await;

    let inside = run_instance(
        &router,
        "a host of the group's own VPC",
        &home.subnet,
        &home_group,
        "10.71.1.10",
    )
    .await;
    let outside_one = run_instance(
        &router,
        "a host of another VPC",
        &away.subnet,
        &away_group,
        "10.72.1.10",
    )
    .await;
    let outside_two = run_instance(
        &router,
        "a second host of another VPC",
        &away.subnet,
        &away_group,
        "10.72.1.11",
    )
    .await;
    for (case, instance) in [
        ("a host of the group's own VPC", &inside),
        ("a host of another VPC", &outside_one),
        ("a second host of another VPC", &outside_two),
    ] {
        instance_state(&router, case, instance, "running").await;
    }

    let hosts = target_group(&router, "the home group", "hostvpc-tg", "instance", &home.vpc).await;
    let rows = vec![
        Row {
            case: "one host of another VPC",
            targets: vec![(outside_one.clone(), None)],
            named: vec![outside_one.clone()],
        },
        Row {
            case: "two hosts of another VPC in one request",
            targets: vec![(outside_one.clone(), None), (outside_two.clone(), None)],
            named: vec![outside_one.clone(), outside_two.clone()],
        },
        Row {
            case: "the same two hosts in the other order",
            targets: vec![(outside_two.clone(), None), (outside_one.clone(), None)],
            named: vec![outside_two.clone(), outside_one.clone()],
        },
        Row {
            case: "a host of the group's VPC beside one of another",
            targets: vec![(inside.clone(), None), (outside_two.clone(), None)],
            named: vec![outside_two.clone()],
        },
        Row {
            case: "one host of another VPC named twice, on two ports",
            targets: vec![
                (outside_one.clone(), Some(TRAFFIC_PORT)),
                (outside_one.clone(), Some(OTHER_PORT)),
            ],
            named: vec![outside_one.clone()],
        },
    ];
    for Row {
        case,
        targets,
        named,
    } in &rows
    {
        let asked: Vec<(&str, Option<i32>)> = targets
            .iter()
            .map(|(id, port)| (id.as_str(), *port))
            .collect();
        let reply = register(&router, &hosts, &asked).await;
        let quoted: Vec<String> = named.iter().map(|id| format!("'{id}'")).collect();
        let expected = format!(
            "The following targets are not in the target group VPC '{}': {}",
            home.vpc,
            quoted.join(", ")
        );
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "InvalidTarget".to_string()),
            "case {case:?}: expected 400 InvalidTarget, got {} body {}",
            reply.status,
            reply.body
        );
        let message = message_of(&reply.body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // A host the group's own VPC holds is accepted, and after every refusal above the group
    // holds exactly that one registration: a refused request registers none of its targets.
    register_ok(
        &router,
        "a host of the group's own VPC",
        &hosts,
        &[(inside.as_str(), None)],
    )
    .await;
    let held = registrations(&router, "the home group", &hosts).await;
    assert_eq!(
        held,
        vec![key(&inside, TRAFFIC_PORT)],
        "case \"the home group\": expected only the host of the group's own VPC to be \
         registered, got {held:?}"
    );
}
