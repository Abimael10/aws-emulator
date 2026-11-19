// SPDX-License-Identifier: CC0-1.0

//! What an `ELB`-health-checked Auto Scaling group reports for its instance when the target
//! group's health check connects to a port NOTHING behind that instance is listening on — the
//! whole chain from the check to the replacement trigger, over the three services that own its
//! links: the instance is Amazon EC2's resource, the registration and the verdict its checks
//! reach are Elastic Load Balancing v2's, and the instance health derived from that verdict is
//! Auto Scaling's.
//!
//! The rule under test: **a health check is a connection to the target on the group's
//! EFFECTIVE health-check port, so it is answered only on a port something behind the
//! registration is LISTENING on.** The ports behind a registration naming an INSTANCE of the
//! account are the ones this emulator is told about: the port the registration itself names —
//! "the port on which the target is listening" (`TargetDescription$Port`), which is the target
//! group's own port where the registration takes it — and the ports the GUEST of that instance
//! is listening on, the server its own user data installed and started, which is the very fact
//! a `forward` of a client's request to such a target is served from. A check pointed at any
//! other port is answered by nothing, so it fails, and the registration reaches the
//! `unhealthy` its group's `UnhealthyThresholdCount` consecutive failures take.
//!
//! Two things follow from that verdict for a group whose `HealthCheckType` is `ELB` — "use
//! Elastic Load Balancing health checks" — and both are asserted here rather than taken on
//! trust: the group reports that verdict as its instance's own `HealthStatus` ("`Unhealthy`
//! means that the instance is unhealthy and that Amazon EC2 Auto Scaling should terminate and
//! replace it"), and it then terminates and replaces that instance.
//!
//! The rows span what the rule generalises over: the three forms the effective health-check
//! port takes (none named, the modelled `traffic-port` default, a port of its own), which port
//! the checks therefore go to against the port the registration names, and whether a server the
//! instance's guest runs is listening there. The recorded configuration — a group created with
//! `HealthCheckType=ELB` whose target group points its checks at a port nothing listens on — is
//! one row.
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

use crate::elbv2_network_resolution::{
    attach_internet_gateway, ec2_ok, elbv2, form, send, Answer, BOOT_STACK_SIZE,
};
use crate::query_wire::{block_of, element, members_of, opt_element};

/// The API version an awsQuery request naming the Auto Scaling model carries.
const ASG_VERSION: &str = "2011-01-01";

/// The AMI and instance type every launch template here names; this suite asserts nothing
/// about either.
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// The health-check schedule every target group below is created with: the shortest cadence
/// `HealthCheckIntervalSeconds` may be written at ("the range is 5-300") and the fewest
/// consecutive checks `UnhealthyThresholdCount` may ask for ("the range is 2-10"), because what
/// this suite asserts is WHICH verdict the checks reach, not how long a load balancer takes to
/// reach it.
const CHECK_INTERVAL_SECONDS: &str = "5";
const CHECK_TIMEOUT_SECONDS: &str = "5";
const CHECK_THRESHOLD: &str = "2";

/// The `HealthCheckGracePeriod` every group below is created with: "the amount of time, in
/// seconds, that Amazon EC2 Auto Scaling waits before checking the health status of an EC2
/// instance that has come into service". The window itself is not what this suite is about —
/// it asks only WHICH health the group derives — so every row waits the shortest window the
/// member admits.
const GRACE_PERIOD: &str = "0";

/// How long a leg waits for the clock-owed moves it depends on before it gives up and reports
/// what it last read. Many multiples of the longest of them (the checks come
/// [`CHECK_INTERVAL_SECONDS`] apart and a verdict is [`CHECK_THRESHOLD`] of them away), because
/// the deadline is only here so a failure is a failure rather than a hang: the waits below look
/// for their CONDITION, so machine load decides when a leg finishes and never whether it
/// passes.
const DEADLINE: Duration = Duration::from_secs(180);

/// How often a wait looks again — and, because the emulator's own clock-owed work runs on the
/// runtime that carries these requests, a wait is spent INSIDE that runtime so the checks a
/// target group owes its registrations are taken while the test is waiting for them.
const POLL: Duration = Duration::from_millis(100);

/// The `TargetHealthStateEnum` values this suite reads, from
/// `smithy_query(query="shape",shape="TargetHealthStateEnum")`:
/// `initial|healthy|unhealthy|unhealthy.draining|unused|draining|unavailable`.
const HEALTHY: &str = "healthy";
const UNHEALTHY: &str = "unhealthy";

/// The `TargetHealthReasonEnum` values `TargetHealth$Reason` documents for a check nothing
/// answered: "`Target.Timeout` - The health check requests timed out" and
/// "`Target.FailedHealthChecks` - The load balancer received an error while establishing a
/// connection to the target or the target response was malformed" — which of the two is the
/// family of load balancer's own wording, tabled in the ELBv2 crate's own suites.
const UNANSWERED_REASONS: [&str; 2] = ["Target.Timeout", "Target.FailedHealthChecks"];

/// The `LifecycleState` and the two `HealthStatus` values a group reports for its instances
/// (`smithy_query(query="shape",shape="Instance")`: "`Healthy` means that the instance is
/// healthy and should remain in service. `Unhealthy` means that the instance is unhealthy and
/// that Amazon EC2 Auto Scaling should terminate and replace it").
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

/// One VPC with one subnet per entry of `blocks` (`(block, zone)`), created through Amazon EC2
/// itself so every id asserted against is one EC2 minted.
struct Network {
    vpc: String,
    subnets: Vec<String>,
}

fn seed_network(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc_block: &str,
    blocks: &[(&str, &str)],
) -> Network {
    let vpc = element(
        &ec2_ok(router, runtime, "CreateVpc", &[("CidrBlock", vpc_block)]),
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (block, zone) in blocks {
        let created = ec2_ok(
            router,
            runtime,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", block),
                ("AvailabilityZone", zone),
            ],
        );
        subnets.push(element(&created, "subnetId"));
    }
    Network { vpc, subnets }
}

/// Create one security group in `vpc` and answer its id.
fn security_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc: &str,
    name: &str,
    description: &str,
) -> String {
    element(
        &ec2_ok(
            router,
            runtime,
            "CreateSecurityGroup",
            &[
                ("GroupName", name),
                ("GroupDescription", description),
                ("VpcId", vpc),
            ],
        ),
        "groupId",
    )
}

/// Admit TCP `port` to `group` from the security group `source`, which is how an inbound rule
/// names the group a load balancer is associated with.
fn authorize(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
    port: &str,
    source: &str,
) {
    ec2_ok(
        router,
        runtime,
        "AuthorizeSecurityGroupIngress",
        &[
            ("GroupId", group),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", port),
            ("IpPermissions.1.ToPort", port),
            ("IpPermissions.1.Groups.1.GroupId", source),
        ],
    );
}

/// Create one launch template that launches into `security_group`, running `user_data` — the
/// program the guest RUNS at boot, which the API carries base64-encoded — or nothing at all for
/// a row whose template names none.
fn launch_template(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    security_group: &str,
    user_data: &str,
) {
    let encoded = aws_smithy_types::base64::encode(user_data);
    let mut params: Vec<(&str, &str)> = vec![
        ("LaunchTemplateName", name),
        ("LaunchTemplateData.ImageId", IMAGE_ID),
        ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        // `RequestLaunchTemplateData$SecurityGroupIds` travels as its own `xmlName`,
        // `SecurityGroupId`, which is what an ec2Query client sends.
        ("LaunchTemplateData.SecurityGroupId.1", security_group),
    ];
    if !user_data.is_empty() {
        params.push(("LaunchTemplateData.UserData", encoded.as_str()));
    }
    ec2_ok(router, runtime, "CreateLaunchTemplate", &params);
}

/// One registration as `DescribeTargetHealth` reports it, read inside the member that carries
/// it so no field is paired with another registration's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    /// `TargetDescription$Id`: the target the description is about.
    id: String,
    /// `TargetHealth$State`, `$Reason` and `$Description`, each absent where the state is
    /// reported without it.
    state: String,
    reason: Option<String>,
    description: Option<String>,
    /// `TargetHealthDescription$HealthCheckPort`: "the port to use to connect with the target",
    /// which is the group's effective health-check port — the port this suite is about.
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

/// Read what the two services report until `settled` holds for the pair, or [`DEADLINE`]
/// passes; answer the last reading either way, so a failure reports what was really read.
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
        runtime.block_on(async { tokio::time::sleep(POLL).await });
    }
}

/// A guest that installs a package's web server and leaves it listening on port 80 — the
/// server Amazon EC2's guest runtime starts for that unit.
const SERVER_ON_80: &str = "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                            echo ASG-HEALTH-PORT-GUEST > /var/www/html/index.html\n\
                            systemctl enable --now httpd\n";

/// One row: a group whose single instance is registered into a target group whose checks go to
/// the port the row names, and the verdict that port implies.
struct Row {
    case: &'static str,
    /// What every resource name of this row is suffixed with.
    slug: &'static str,
    /// The guest's own program, as a user-data script — empty for a launch template that starts
    /// nothing at all, which is an instance listening on no port.
    user_data: &'static str,
    /// `CreateTargetGroup$Port`, "the port on which the targets receive traffic", which is the
    /// port the registration names the target as listening on.
    traffic_port: &'static str,
    /// `CreateTargetGroup$HealthCheckPort` where the row names one at all.
    health_check_port: Option<&'static str>,
    /// The port the checks of that group therefore connect to, which is what
    /// `TargetHealthDescription$HealthCheckPort` reports.
    checked_port: &'static str,
    /// Whether anything behind the instance is listening on that port — which is the whole
    /// question a check of it asks.
    listening: bool,
}

/// The rows span the three forms the effective health-check port takes, the port it reaches
/// against the one the registration names, and whether a server of the guest is listening
/// there.
const ROWS: [Row; 5] = [
    Row {
        case: "the recorded configuration: the checks go to a port of their own that nothing \
               behind the instance listens on",
        slug: "deadport",
        user_data: "",
        traffic_port: "22",
        health_check_port: Some("55555"),
        checked_port: "55555",
        listening: false,
    },
    Row {
        case: "the member names no health-check port, so the checks go to the port the \
               registration names",
        slug: "default",
        user_data: "",
        traffic_port: "22",
        health_check_port: None,
        checked_port: "22",
        listening: true,
    },
    Row {
        case: "the modelled traffic-port default names the port the registration names",
        slug: "trafficport",
        user_data: "",
        traffic_port: "8080",
        health_check_port: Some("traffic-port"),
        checked_port: "8080",
        listening: true,
    },
    Row {
        case: "a health-check port of its own that the instance's guest is listening on",
        slug: "guestport",
        user_data: SERVER_ON_80,
        traffic_port: "8080",
        health_check_port: Some("80"),
        checked_port: "80",
        listening: true,
    },
    Row {
        case: "a health-check port of its own that the instance's guest runs no server on",
        slug: "otherport",
        user_data: SERVER_ON_80,
        traffic_port: "80",
        health_check_port: Some("8080"),
        checked_port: "8080",
        listening: false,
    },
];

/// Everything one row needs standing up, so every row's launch and checks run on the one clock
/// instead of one row waiting out the next row's setup.
struct Deployed<'a> {
    row: &'a Row,
    target_group: String,
    group_name: String,
}

/// RULE: an Auto Scaling group's instance is reported `unhealthy` by the load balancer, and
/// `Unhealthy` by the group itself, exactly when the checks of its registration reach a port
/// nothing behind that instance is listening on — and the group then replaces it.
#[test]
fn an_elb_health_check_reaching_no_listening_port_turns_the_asg_instance_unhealthy() {
    std::thread::Builder::new()
        .name("asg-elb-health-check-port".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = router();
            let runtime = runtime();
            // An Application Load Balancer is placed in two zones, and the internet-facing
            // scheme `CreateLoadBalancer` documents as its default needs the VPC to have an
            // internet gateway.
            let network = seed_network(
                &router,
                &runtime,
                "10.93.0.0/16",
                &[("10.93.1.0/24", "us-west-2a"), ("10.93.2.0/24", "us-west-2b")],
            );
            attach_internet_gateway(&router, &runtime, &network.vpc);
            let balancer_group = security_group(
                &router,
                &runtime,
                &network.vpc,
                "asghcport-balancer",
                "the load balancers",
            );
            let instances_group = security_group(
                &router,
                &runtime,
                &network.vpc,
                "asghcport-instances",
                "the groups instances",
            );
            // The instances admit the load balancers' own security group on EVERY port any row
            // checks and every port any row's traffic goes to, so what a check of a row finds
            // is decided by the port it connects to and never by a rule missing for it. Each
            // distinct port is admitted once — a rule a group already carries is refused as a
            // duplicate.
            let ports: BTreeSet<&str> = ROWS
                .iter()
                .flat_map(|row| [row.traffic_port, row.checked_port])
                .collect();
            for port in ports {
                authorize(
                    &router,
                    &runtime,
                    &instances_group,
                    port,
                    &balancer_group,
                );
            }

            let mut deployed: Vec<Deployed<'_>> = Vec::new();
            for row in &ROWS {
                let template = format!("asghcport-{}-lt", row.slug);
                launch_template(
                    &router,
                    &runtime,
                    &template,
                    &instances_group,
                    row.user_data,
                );
                let group_label = format!("asghcport-{}-tg", row.slug);
                let mut group_params: Vec<(&str, &str)> = vec![
                    ("Name", group_label.as_str()),
                    ("Protocol", "HTTP"),
                    ("Port", row.traffic_port),
                    ("VpcId", network.vpc.as_str()),
                    ("TargetType", "instance"),
                    ("HealthCheckProtocol", "HTTP"),
                    ("HealthCheckPath", "/"),
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
                let balancer_name = format!("asghcport-{}-alb", row.slug);
                let balancer = element(
                    &elbv2_ok(
                        &router,
                        &runtime,
                        "CreateLoadBalancer",
                        &[
                            ("Name", balancer_name.as_str()),
                            ("Type", "application"),
                            ("Subnets.member.1", network.subnets[0].as_str()),
                            ("Subnets.member.2", network.subnets[1].as_str()),
                            ("SecurityGroups.member.1", balancer_group.as_str()),
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
                        ("Protocol", "HTTP"),
                        ("Port", "80"),
                        ("DefaultActions.member.1.Type", "forward"),
                        ("DefaultActions.member.1.TargetGroupArn", target_group.as_str()),
                    ],
                );
                let group_name = format!("asghcport-{}-asg", row.slug);
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
                        ("VPCZoneIdentifier", network.subnets[0].as_str()),
                        ("TargetGroupARNs.member.1", target_group.as_str()),
                        // The load balancer's verdict IS this group's instance health.
                        ("HealthCheckType", "ELB"),
                        ("HealthCheckGracePeriod", GRACE_PERIOD),
                    ],
                );
                deployed.push(Deployed {
                    row,
                    target_group,
                    group_name,
                });
            }

            for entry in &deployed {
                let row = entry.row;
                let wanted_state = if row.listening { HEALTHY } else { UNHEALTHY };
                let wanted_health = if row.listening {
                    HEALTHY_INSTANCE
                } else {
                    UNHEALTHY_INSTANCE
                };
                // The launch, the registration, the guest's boot and the run of checks a
                // verdict takes are all clock-owed: the row waits for the CONDITION — the
                // verdict its port implies, and the instance health the group derives from it.
                let (reported, held) = read_until(
                    &router,
                    &runtime,
                    &entry.target_group,
                    &entry.group_name,
                    |reported, held| {
                        reported
                            .iter()
                            .any(|description| description.state == wanted_state)
                            && held
                                .iter()
                                .any(|instance| instance.health_status == wanted_health)
                    },
                );

                let verdict = reported
                    .iter()
                    .find(|description| description.state == wanted_state);
                assert!(
                    verdict.is_some(),
                    "case {:?}: expected a registration reported {wanted_state} on the checks \
                     of port {}, got {reported:?}",
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
                if row.listening {
                    // "If the target state is healthy, a reason code is not provided."
                    assert_eq!(
                        (verdict.reason.as_deref(), verdict.description.as_deref()),
                        (None, None),
                        "case {:?}: a healthy registration carries no reason and no \
                         description, got {verdict:?}",
                        row.case
                    );
                } else {
                    let reason = verdict.reason.clone().unwrap_or_default();
                    assert!(
                        UNANSWERED_REASONS.contains(&reason.as_str()),
                        "case {:?}: a check nothing answered must be reported as one of \
                         {UNANSWERED_REASONS:?}, got {verdict:?}",
                        row.case
                    );
                    assert!(
                        verdict
                            .description
                            .as_deref()
                            .is_some_and(|text| !text.is_empty()),
                        "case {:?}: a state reported with a reason carries the detail that \
                         reason is described by, got {verdict:?}",
                        row.case
                    );
                }

                let condemned: Vec<&Held> = held
                    .iter()
                    .filter(|instance| instance.health_status == wanted_health)
                    .collect();
                assert!(
                    !condemned.is_empty(),
                    "case {:?}: an ELB-health-checked group must report the verdict of its \
                     instance's own registration as that instance's {wanted_health} health, \
                     got {held:?}",
                    row.case
                );
                if row.listening {
                    assert_eq!(
                        condemned
                            .iter()
                            .map(|instance| instance.lifecycle_state.as_str())
                            .collect::<Vec<&str>>(),
                        vec![IN_SERVICE],
                        "case {:?}: a healthy instance stays in service, got {held:?}",
                        row.case
                    );
                    continue;
                }

                // "Unhealthy means that the instance is unhealthy and that Amazon EC2 Auto
                // Scaling should terminate and replace it": the group goes on to do exactly
                // that, so an instance it never held before comes to stand in for the
                // condemned one.
                let known: BTreeSet<String> = held
                    .iter()
                    .map(|instance| instance.instance_id.clone())
                    .collect();
                let (_, replaced) = read_until(
                    &router,
                    &runtime,
                    &entry.target_group,
                    &entry.group_name,
                    |_, held| {
                        held.iter()
                            .any(|instance| !known.contains(&instance.instance_id))
                    },
                );
                assert!(
                    replaced
                        .iter()
                        .any(|instance| !known.contains(&instance.instance_id)),
                    "case {:?}: expected the group to replace the instance its ELB health \
                     check condemned (it held {known:?}), got {replaced:?}",
                    row.case
                );
            }
        })
        .expect("the suite's own thread")
        .join()
        .expect("the suite's own thread finishes");
}
