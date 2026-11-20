// SPDX-License-Identifier: CC0-1.0

//! What an `ELB`-health-checked Auto Scaling group does while the security group of its own
//! instances admits NOTHING, and what happens once the block the load balancer's nodes sit in
//! is admitted — over the three services that own the links of that loop: the instance and its
//! security group are Amazon EC2's, the registration and the verdict its checks reach are
//! Elastic Load Balancing v2's, and the instance health derived from that verdict (and the
//! replacement it calls for) is Auto Scaling's.
//!
//! The rule under test: **a health check arrives only while an inbound rule of a security
//! group governing the instance admits the checked port from the load balancer performing it,
//! and the verdict is DERIVED from those rules rather than latched** — so, for an internal
//! Network Load Balancer associated with no security group, a group of instances whose rules
//! name no address range at all answers no check, every registration reaches `unhealthy`, an
//! `ELB`-health-checked group reports that as its instance's `Unhealthy` health and
//! "terminate[s] and replace[s] the unhealthy instance" — again and again, because each
//! replacement is as unreachable as the instance it stands in for — and admitting the block
//! the load balancer's nodes sit in returns the group's CURRENT registration to `healthy`,
//! which is the one state no replacement follows.
//!
//! That is the loop a client runs to diagnose an unreachable target: the two recorded
//! configurations it was written for — a cluster security group that admits only itself, and an
//! ingressless one — are one row each of the table below, which spans what the rule
//! generalises over: whether the group's rules name a source at all before the remediation,
//! which port the checks go to (the port the registration names, and a health-check port of its
//! own), and whether the guest leaves a server listening on it.
//!
//! What this adds to the suites beside it: `asg_registered_target_health_verdicts.rs` asserts
//! the same firewall rule for a group that checks its instances Amazon EC2's way, so nothing
//! there is ever replaced, and `asg_elb_health_check_port_drives_instance_health.rs` asserts
//! the replacement an `ELB`-health-checked group makes when the checked PORT has no listener.
//! Here the two halves meet: the checks fail because of the RULES, the group replaces its
//! instances for as long as they do, and the remediation is a rule — so a client's whole
//! diagnostic loop is driven end to end.
//!
//! Every clock-owed move (the launch, the registration, the guest's boot, the run of checks a
//! verdict takes, the replacement) is POLLED for until it holds, never slept for, and the
//! router is assembled on a sized thread exactly as `elbv2_network_resolution.rs` does — whose
//! request plumbing this suite reuses rather than restating.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{ec2_ok, elbv2, form, send, Answer, BOOT_STACK_SIZE};
use crate::query_wire::{block_of, element, members_of, opt_element};

/// The API version an awsQuery request naming the Auto Scaling model carries.
const ASG_VERSION: &str = "2011-01-01";

/// The AMI and instance type every launch template here names; this suite asserts nothing
/// about either.
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// The health-check schedule every target group below is created with: the shortest cadence
/// `HealthCheckIntervalSeconds` may be written at ("the range is 5-300") and the fewest
/// consecutive checks `UnhealthyThresholdCount`/`HealthyThresholdCount` may ask for ("the
/// range is 2-10"), because what this suite asserts is WHICH verdict the checks reach and not
/// how long a load balancer takes to reach it.
const CHECK_INTERVAL_SECONDS: &str = "5";
const CHECK_TIMEOUT_SECONDS: &str = "2";
const CHECK_THRESHOLD: &str = "2";

/// The `HealthCheckGracePeriod` every group below is created with: the shortest window the
/// member admits, because the window is not what this suite is about.
const GRACE_PERIOD: &str = "0";

/// How long a leg waits for the clock-owed moves it depends on before it gives up and reports
/// what it last read. Many multiples of the longest of them (the checks come
/// [`CHECK_INTERVAL_SECONDS`] apart and a verdict is [`CHECK_THRESHOLD`] of them away, and a
/// replacement is a launch after that), because the deadline is only here so a failure is a
/// failure rather than a hang: every wait below looks for its CONDITION, so machine load
/// decides when a leg finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(240);

/// How often a wait looks again — and, because the emulator's own clock-owed work runs on the
/// runtime that carries these requests, a wait is spent INSIDE that runtime so the checks a
/// target group owes its registrations are taken while the test is waiting for them.
const POLL: Duration = Duration::from_millis(100);

/// The `TargetHealthStateEnum` values this suite reads.
const HEALTHY: &str = "healthy";
const UNHEALTHY: &str = "unhealthy";

/// The `LifecycleState` and the two `HealthStatus` values a group reports for its instances.
const IN_SERVICE: &str = "InService";
const HEALTHY_INSTANCE: &str = "Healthy";
const UNHEALTHY_INSTANCE: &str = "Unhealthy";

/// The body of one control-plane answer that must have been served.
fn served(action: &str, answer: Answer) -> String {
    assert_eq!(
        answer.status, 200,
        "setup: {action} must succeed, got {answer:?}"
    );
    answer.text
}

/// Drive one Elastic Load Balancing v2 action that must succeed, and answer its body.
fn elbv2_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    served(action, elbv2(router, runtime, action, params))
}

/// Drive one Auto Scaling action that must succeed, and answer its body.
fn asg_ok(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    served(
        action,
        send(
            router,
            runtime,
            "autoscaling",
            form(action, ASG_VERSION, params),
        ),
    )
}

/// The whole emulator over one engine, exactly as the binary assembles it, with signature
/// verification left to the front door.
fn router() -> MultiServiceRouter {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A current-thread runtime, which carries each request below and drives the emulator's own
/// clock-owed work while it does.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

/// Spend one polling interval inside the runtime that carries the requests.
fn waited(runtime: &tokio::runtime::Runtime) {
    runtime.block_on(async { tokio::time::sleep(POLL).await });
}

/// One VPC and the one subnet the load balancer's nodes and the group's instances both sit in,
/// created through Amazon EC2 itself so every id below is one EC2 minted.
struct Network {
    vpc: String,
    subnet: String,
    block: String,
}

fn seed_network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc_block: &str,
    subnet_block: &str,
) -> Network {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", vpc_block)]),
        "vpcId",
    );
    let subnet = element(
        &ec2_ok(
            router,
            runtime,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", subnet_block),
                ("AvailabilityZone", "us-west-2a"),
            ],
        ),
        "subnetId",
    );
    Network {
        vpc,
        subnet,
        block: subnet_block.to_string(),
    }
}

/// Create one security group in `vpc` and answer its id.
fn security_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc: &str,
    name: &str,
) -> String {
    element(
        &ec2_ok(
            router,
            runtime,
            "CreateSecurityGroup",
            &[
                ("GroupName", name),
                ("GroupDescription", "the groups instances"),
                ("VpcId", vpc),
            ],
        ),
        "groupId",
    )
}

/// Create one launch template that launches into `group`, running `user_data` — the program
/// the guest RUNS at boot, which the API carries base64-encoded — or nothing at all.
fn launch_template(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    group: &str,
    user_data: &str,
) {
    let encoded = aws_smithy_types::base64::encode(user_data);
    let mut params: Vec<(&str, &str)> = vec![
        ("LaunchTemplateName", name),
        ("LaunchTemplateData.ImageId", IMAGE_ID),
        ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        // `RequestLaunchTemplateData$SecurityGroupIds` travels as its own `xmlName`,
        // `SecurityGroupId`, which is what an ec2Query client sends.
        ("LaunchTemplateData.SecurityGroupId.1", group),
    ];
    if !user_data.is_empty() {
        params.push(("LaunchTemplateData.UserData", encoded.as_str()));
    }
    ec2_ok(router, runtime, "CreateLaunchTemplate", &params);
}

/// One registration as `DescribeTargetHealth` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    id: String,
    state: String,
    reason: Option<String>,
    description: Option<String>,
    health_check_port: Option<String>,
}

/// Every registration the target group `arn` holds right now, as a client reads it.
fn target_health(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    arn: &str,
) -> Vec<Reported> {
    let body = elbv2_ok(
        router,
        runtime,
        "DescribeTargetHealth",
        &[("TargetGroupArn", arn)],
    );
    let descriptions = block_of(&body, "TargetHealthDescriptions").unwrap_or_default();
    members_of(descriptions)
        .into_iter()
        .map(|member| {
            let target = block_of(member, "Target").unwrap_or_default();
            let health = block_of(member, "TargetHealth").unwrap_or_default();
            Reported {
                id: opt_element(target, "Id").unwrap_or_default(),
                state: opt_element(health, "State").unwrap_or_default(),
                reason: opt_element(health, "Reason"),
                description: opt_element(health, "Description"),
                health_check_port: opt_element(member, "HealthCheckPort"),
            }
        })
        .collect()
}

/// One instance of an Auto Scaling group, as `DescribeAutoScalingGroups` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    instance_id: String,
    lifecycle_state: String,
    health_status: String,
}

/// The instances the group `name` reports right now.
fn group_instances(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> Vec<Held> {
    let body = asg_ok(
        router,
        runtime,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    );
    let instances = block_of(&body, "Instances").unwrap_or_default();
    members_of(instances)
        .into_iter()
        .map(|member| Held {
            instance_id: opt_element(member, "InstanceId").unwrap_or_default(),
            lifecycle_state: opt_element(member, "LifecycleState").unwrap_or_default(),
            health_status: opt_element(member, "HealthStatus").unwrap_or_default(),
        })
        .collect()
}

/// Read both services until `settled` holds for the pair, or [`DEADLINE`] passes; answer the
/// last reading either way, so a failure reports what was really read.
fn read_until(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    target_group: &str,
    group_name: &str,
    settled: impl Fn(&[Reported], &[Held]) -> bool,
) -> (Vec<Reported>, Vec<Held>) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = target_health(router, runtime, target_group);
        let held = group_instances(router, runtime, group_name);
        if settled(&reported, &held) || Instant::now() >= deadline {
            return (reported, held);
        }
        waited(runtime);
    }
}

/// A guest that leaves a server listening on port 80 — the server Amazon EC2's guest runtime
/// starts for that unit.
const SERVER_ON_80: &str = "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                            echo ASG-FIREWALL-GUEST > /var/www/html/index.html\n\
                            systemctl enable --now httpd\n";

/// One row: how the instances' security group is written before the remediation, which port
/// the checks go to, and whether the guest listens there.
struct Row {
    case: &'static str,
    /// What every resource name of this row is suffixed with.
    slug: &'static str,
    /// Whether the group admits ITSELF before the remediation — the self-references a cluster
    /// security group carries — or carries no rule at all.
    admits_itself: bool,
    /// The guest's own program, as a user-data script.
    user_data: &'static str,
    /// `CreateTargetGroup$Port`, the port the registration names the target as listening on.
    traffic_port: &'static str,
    /// `CreateTargetGroup$HealthCheckPort` where the row names one at all.
    health_check_port: Option<&'static str>,
    /// The port the checks therefore connect to, which is what the remediation admits.
    checked_port: &'static str,
}

/// The rows: the two recorded configurations of the instances' group, crossed with the two
/// forms the effective health-check port takes.
const ROWS: [Row; 2] = [
    Row {
        case: "a cluster security group that admits only itself, checked on the port the \
               registration names",
        slug: "cluster",
        admits_itself: true,
        user_data: "",
        traffic_port: "22",
        health_check_port: None,
        checked_port: "22",
    },
    Row {
        case: "an ingressless security group, checked on a health-check port of its own the \
               guest listens on",
        slug: "ingressless",
        admits_itself: false,
        user_data: SERVER_ON_80,
        traffic_port: "8080",
        health_check_port: Some("80"),
        checked_port: "80",
    },
];

/// Everything one row needs standing up, so every row's launch and checks run on the one clock
/// instead of one row waiting out the next row's setup.
struct Deployed<'a> {
    row: &'a Row,
    target_group: String,
    group_name: String,
}

#[test]
fn an_unreachable_asg_instance_is_replaced_until_its_subnet_block_is_admitted() {
    std::thread::Builder::new()
        .name("asg-firewall-replacement".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = router();
            let runtime = runtime();
            // One subnet, which is where the internal Network Load Balancers below put their
            // own nodes and where the groups' instances are launched.
            let network = seed_network(&router, &runtime, "172.27.0.0/16", "172.27.0.0/23");

            let mut deployed: Vec<Deployed<'_>> = Vec::new();
            let mut groups: Vec<String> = Vec::new();
            for row in &ROWS {
                let instances_group = security_group(
                    &router,
                    &runtime,
                    &network.vpc,
                    &format!("asgfw-{}-sg", row.slug),
                );
                if row.admits_itself {
                    // The self-references a cluster security group carries: no rule of it
                    // names a load balancer outside the group or any address range at all.
                    ec2_ok(
                        &router,
                        &runtime,
                        "AuthorizeSecurityGroupIngress",
                        &[
                            ("GroupId", instances_group.as_str()),
                            ("IpPermissions.1.IpProtocol", "-1"),
                            ("IpPermissions.1.Groups.1.GroupId", instances_group.as_str()),
                        ],
                    );
                }
                let template = format!("asgfw-{}-lt", row.slug);
                launch_template(
                    &router,
                    &runtime,
                    &template,
                    &instances_group,
                    row.user_data,
                );
                let group_label = format!("asgfw-{}-tg", row.slug);
                let mut group_params: Vec<(&str, &str)> = vec![
                    ("Name", group_label.as_str()),
                    ("Protocol", "TCP"),
                    ("Port", row.traffic_port),
                    ("VpcId", network.vpc.as_str()),
                    ("TargetType", "instance"),
                    ("HealthCheckIntervalSeconds", CHECK_INTERVAL_SECONDS),
                    ("HealthCheckTimeoutSeconds", CHECK_TIMEOUT_SECONDS),
                    ("HealthyThresholdCount", CHECK_THRESHOLD),
                    ("UnhealthyThresholdCount", CHECK_THRESHOLD),
                ];
                if let Some(port) = row.health_check_port {
                    group_params.push(("HealthCheckPort", port));
                }
                let target_group = element(
                    &elbv2_ok(&router, &runtime, "CreateTargetGroup", &group_params),
                    "TargetGroupArn",
                );
                // An INTERNAL Network Load Balancer, associated with no security group of its
                // own, so the only rule that can name it is one covering the block its nodes
                // sit in.
                let balancer_name = format!("asgfw-{}-nlb", row.slug);
                let balancer = element(
                    &elbv2_ok(
                        &router,
                        &runtime,
                        "CreateLoadBalancer",
                        &[
                            ("Name", balancer_name.as_str()),
                            ("Type", "network"),
                            ("Scheme", "internal"),
                            ("Subnets.member.1", network.subnet.as_str()),
                        ],
                    ),
                    "LoadBalancerArn",
                );
                // A target group nothing forwards to is checked by no load balancer at all, so
                // the listener is what starts the checks this row is about.
                elbv2_ok(
                    &router,
                    &runtime,
                    "CreateListener",
                    &[
                        ("LoadBalancerArn", balancer.as_str()),
                        ("Protocol", "TCP"),
                        ("Port", row.traffic_port),
                        ("DefaultActions.member.1.Type", "forward"),
                        ("DefaultActions.member.1.TargetGroupArn", target_group.as_str()),
                    ],
                );
                let group_name = format!("asgfw-{}-asg", row.slug);
                asg_ok(
                    &router,
                    &runtime,
                    "CreateAutoScalingGroup",
                    &[
                        ("AutoScalingGroupName", group_name.as_str()),
                        ("MinSize", "1"),
                        ("MaxSize", "1"),
                        ("DesiredCapacity", "1"),
                        ("LaunchTemplate.LaunchTemplateName", template.as_str()),
                        ("VPCZoneIdentifier", network.subnet.as_str()),
                        ("TargetGroupARNs.member.1", target_group.as_str()),
                        // The load balancer's verdict IS this group's instance health, which
                        // is what makes an unreachable instance one the group replaces.
                        ("HealthCheckType", "ELB"),
                        ("HealthCheckGracePeriod", GRACE_PERIOD),
                    ],
                );
                groups.push(instances_group);
                deployed.push(Deployed {
                    row,
                    target_group,
                    group_name,
                });
            }

            // LEG 1: no rule of the instances' group names the load balancer, so its checks
            // arrive nowhere: the registration reaches `unhealthy` and the group reports its
            // instance `Unhealthy`.
            for entry in &deployed {
                let row = entry.row;
                let (reported, held) = read_until(
                    &router,
                    &runtime,
                    &entry.target_group,
                    &entry.group_name,
                    |reported, held| {
                        reported.iter().any(|report| report.state == UNHEALTHY)
                            && held
                                .iter()
                                .any(|instance| instance.health_status == UNHEALTHY_INSTANCE)
                    },
                );
                let verdict = reported.iter().find(|report| report.state == UNHEALTHY);
                assert!(
                    verdict.is_some(),
                    "case {:?}: expected the registration to reach {UNHEALTHY} while no rule \
                     admits the checks of port {}, got {reported:?}",
                    row.case,
                    row.checked_port
                );
                let verdict = verdict.expect("the verdict asserted above");
                assert_eq!(
                    verdict.health_check_port.as_deref(),
                    Some(row.checked_port),
                    "case {:?}: expected the checks to connect to port {}, got {verdict:?}",
                    row.case,
                    row.checked_port
                );
                assert!(
                    verdict
                        .reason
                        .as_deref()
                        .is_some_and(|reason| !reason.is_empty())
                        && verdict
                            .description
                            .as_deref()
                            .is_some_and(|text| !text.is_empty()),
                    "case {:?}: an unhealthy registration carries the reason and the detail \
                     its state is described by, got {verdict:?}",
                    row.case
                );
                assert!(
                    held.iter()
                        .any(|instance| instance.health_status == UNHEALTHY_INSTANCE),
                    "case {:?}: an ELB-health-checked group must report its instance's own \
                     verdict as that instance's health, got {held:?}",
                    row.case
                );
            }

            // LEG 2: "Amazon EC2 Auto Scaling terminates and replaces the unhealthy instance"
            // — and the replacement is as unreachable as the instance it stands in for, so the
            // group goes on replacing: a SECOND instance the group never held before comes
            // along after the first replacement.
            for entry in &deployed {
                let row = entry.row;
                let mut seen: BTreeSet<String> = BTreeSet::new();
                let deadline = Instant::now() + DEADLINE;
                loop {
                    for instance in group_instances(&router, &runtime, &entry.group_name) {
                        if !instance.instance_id.is_empty() {
                            seen.insert(instance.instance_id);
                        }
                    }
                    if seen.len() >= 3 || Instant::now() >= deadline {
                        break;
                    }
                    waited(&runtime);
                }
                assert!(
                    seen.len() >= 3,
                    "case {:?}: expected the group to keep replacing instances no check can \
                     reach, got only {seen:?}",
                    row.case
                );
            }

            // LEG 3: the remediation — each row's own effective health-check port, admitted
            // from the block the load balancer's nodes sit in, which is the only way a rule can
            // name a Network Load Balancer associated with no security group. The verdict is
            // derived rather than latched, so the group's CURRENT registration returns to
            // `healthy` and its instance to `Healthy`, in service.
            for (entry, group) in deployed.iter().zip(groups.iter()) {
                ec2_ok(
                    &router,
                    &runtime,
                    "AuthorizeSecurityGroupIngress",
                    &[
                        ("GroupId", group.as_str()),
                        ("IpPermissions.1.IpProtocol", "tcp"),
                        ("IpPermissions.1.FromPort", entry.row.checked_port),
                        ("IpPermissions.1.ToPort", entry.row.checked_port),
                        ("IpPermissions.1.IpRanges.1.CidrIp", network.block.as_str()),
                    ],
                );
            }
            for entry in &deployed {
                let row = entry.row;
                let (reported, held) = read_until(
                    &router,
                    &runtime,
                    &entry.target_group,
                    &entry.group_name,
                    |reported, held| {
                        reported.iter().any(|report| report.state == HEALTHY)
                            && held.iter().any(|instance| {
                                instance.health_status == HEALTHY_INSTANCE
                                    && instance.lifecycle_state == IN_SERVICE
                            })
                    },
                );
                let settled = reported.iter().find(|report| report.state == HEALTHY);
                assert!(
                    settled.is_some(),
                    "case {:?}: expected the registration to return to {HEALTHY} once port {} \
                     is admitted from {}, got {reported:?}",
                    row.case,
                    row.checked_port,
                    network.block
                );
                let settled = settled.expect("the verdict asserted above");
                // "If the target state is healthy, a reason code is not provided."
                assert_eq!(
                    (settled.reason.as_deref(), settled.description.as_deref()),
                    (None, None),
                    "case {:?}: a healthy registration carries no reason and no description, \
                     got {settled:?}",
                    row.case
                );
                let in_service: Vec<&Held> = held
                    .iter()
                    .filter(|instance| {
                        instance.health_status == HEALTHY_INSTANCE
                            && instance.lifecycle_state == IN_SERVICE
                    })
                    .collect();
                assert!(
                    !in_service.is_empty(),
                    "case {:?}: a group whose checks now arrive must report its instance \
                     {HEALTHY_INSTANCE} and {IN_SERVICE}, got {held:?}",
                    row.case
                );
                // ...and the instance the healthy registration is about is the one the group
                // holds, so the verdict that stops the replacements is the current instance's.
                assert!(
                    in_service
                        .iter()
                        .any(|instance| instance.instance_id == settled.id),
                    "case {:?}: expected the healthy registration {settled:?} to be the \
                     group's own instance, got {held:?}",
                    row.case
                );
            }
        })
        .expect("the suite's own thread")
        .join()
        .expect("the suite's own thread finishes");
}
