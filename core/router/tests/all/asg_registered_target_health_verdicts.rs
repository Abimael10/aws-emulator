// SPDX-License-Identifier: CC0-1.0

//! What an Auto Scaling group's own registration in a target group is REPORTED AS, and what
//! the load balancer in front of it then does with it — over the whole mounted service set,
//! because the instance is Amazon EC2's resource, the registration and its verdict are Elastic
//! Load Balancing v2's, and the instance health derived from that verdict is Auto Scaling's.
//!
//! The rule under test: **a registration is reported as the verdict its own health checks
//! reached, and the checks reach it against the group's EFFECTIVE health-check port.** A
//! registration is `initial` only while the checks that decide it are still owed — "the load
//! balancer is still sending the target the minimum number of health checks required to
//! determine its health status" — and then carries what they decided:
//! `healthy` with NO reason and NO description ("if the target state is healthy, a reason code
//! is not provided"), or `unhealthy` once `UnhealthyThresholdCount` consecutive checks found
//! nothing answering, worded as the family of load balancer performing them words that
//! (`Target.FailedHealthChecks` — "the load balancer received an error while establishing a
//! connection to the target or the target response was malformed" — for a Network Load
//! Balancer). A check is a connection to the target on the group's effective health-check port
//! — `HealthCheckPort` where it names one, the port the target receives traffic on where it
//! names the modelled default — so it arrives only while an inbound rule of a security group
//! governing that instance admits that port from the load balancer performing it, and the
//! verdict is DERIVED from those rules rather than latched: a rule admitted later returns the
//! same registration to `healthy`.
//!
//! Two things follow from that verdict, and both are asserted here rather than taken on trust:
//! a group whose `HealthCheckType` is `ELB` reports it as its instance's `HealthStatus`
//! ("healthy means that the instance is healthy and should remain in service", while the
//! group's own `EC2` checking reads no load balancer at all), and the load balancer forwards a
//! client's request to a registration it reports `healthy`, answering with the server that
//! instance's own user data started — so a 200 carrying that guest's body is only possible for
//! a target whose verdict is in.
//!
//! The rows of each half span what the rule generalises over: the port the traffic goes to, the
//! three forms the effective health-check port takes (none named, a port of its own, the
//! modelled `traffic-port` default), the protocol the checks run over, the program family that
//! leaves a server listening in the guest, and the family of load balancer whose words an
//! unanswered check carries. Every row's
//! guest leaves a server listening on BOTH the port the traffic goes to and the port the checks
//! go to — a check to a port nothing answers on is a check that fails — so what a row varies is
//! only WHICH port the checks go to and whether a rule admits it. The recorded configurations —
//! an Application Load Balancer in front of an `ELB`-health-checked group, and an internal
//! Network Load Balancer in front of instances whose security group admits nothing from any
//! address range until the subnet's block is admitted — are one row each.
//!
//! What this suite adds to the ones beside it, which each cover one leg of that loop:
//! `asg_target_group_lifecycle.rs` asserts WHICH registrations a group makes and the verdict
//! their checks reach on the group's own port, `elbv2_serves_an_instance_targets_own_response.rs`
//! asserts the answer a `forward` to an instance a CALLER registered produces, and
//! `ecs_service_target_group_health_check_port.rs` asserts the effective health-check port for a
//! task registration, whose scheduler declares the ports it serves. Here the whole loop runs at
//! once over a registration AUTO SCALING made — the verdict with its reason and description
//! absent, the instance health derived from it, and the body that same instance's guest serves
//! through the load balancer — over the three forms of the effective health-check port, and the
//! verdict is driven BACK from `unhealthy` to `healthy` inside one run by admitting exactly that
//! port, which is the half the client's whole diagnostic loop depends on.
//!
//! Every clock-owed move (the instance to `running`, the load balancer to `active`, the
//! registration's checks) is POLLED for until it holds, never slept for, and the whole router
//! is assembled on a sized thread exactly as `elbv2_network_resolution.rs` does — whose request
//! plumbing this suite reuses rather than restating.

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
/// consecutive checks `UnhealthyThresholdCount` may ask for ("the range is 2-10"), because a
/// verdict about a registration is that many checks at that spacing away and what this suite
/// asserts is WHICH verdict is reached, not how long a load balancer takes to reach it.
const CHECK_INTERVAL_SECONDS: &str = "5";
const CHECK_TIMEOUT_SECONDS: &str = "5";
const CHECK_THRESHOLD: &str = "2";

/// How long a leg waits for the clock-owed moves it depends on — the launch, the registration,
/// and the run of checks a verdict takes — before it gives up and reports what it last read.
/// Many multiples of the longest of them (the checks come [`CHECK_INTERVAL_SECONDS`] apart and
/// a verdict is [`CHECK_THRESHOLD`] of them away), because the deadline is only here so a
/// failure is a failure rather than a hang: the waits below look for their CONDITION, so
/// machine load — a whole-workspace run has thousands of other tests on the same host — decides
/// when a leg finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(180);

/// How often a wait looks again — and, because the emulator's own clock-owed work runs on the
/// runtime that carries these requests, a wait is spent INSIDE that runtime so the checks a
/// target group owes its registrations are taken while the test is waiting for them.
const POLL: Duration = Duration::from_millis(100);

/// Wait one [`POLL`] on `runtime`, which drives the emulator's own scheduled work.
fn waited(runtime: &tokio::runtime::Runtime) {
    runtime.block_on(async { tokio::time::sleep(POLL).await });
}

/// The `TargetHealthStateEnum` values this suite reads, from
/// `smithy_query(query="shape",shape="TargetHealthStateEnum")`:
/// `initial|healthy|unhealthy|unhealthy.draining|unused|draining|unavailable`.
const INITIAL: &str = "initial";
const HEALTHY: &str = "healthy";
const UNHEALTHY: &str = "unhealthy";

/// The `TargetHealthReasonEnum` value a Network Load Balancer words a check nothing answered
/// as: "`Target.FailedHealthChecks` - The load balancer received an error while establishing a
/// connection to the target or the target response was malformed" (`TargetHealth$Reason`).
const FAILED_HEALTH_CHECKS: &str = "Target.FailedHealthChecks";

/// The `LifecycleState` and `HealthStatus` a group reports for an instance that is in service
/// ( `smithy_query(query="shape",shape="Instance")`: `LifecycleState` includes `InService`,
/// and `HealthStatus` is "the last reported health status of the instance. `Healthy` means
/// that the instance is healthy and should remain in service").
const IN_SERVICE: &str = "InService";
const HEALTHY_INSTANCE: &str = "Healthy";

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

/// A current-thread runtime, which is what carries each request below and drives the
/// emulator's own clock-owed work while it does.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

/// One VPC with one subnet per entry of `blocks` (`(block, zone)`), created through Amazon EC2
/// itself so every id and every address range asserted against is one EC2 minted.
struct Network {
    vpc: String,
    /// `(subnet id, the block it was created with)`, in the order created.
    subnets: Vec<(String, String)>,
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
        subnets.push((element(&created, "subnetId"), (*block).to_string()));
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

/// Create one launch template that launches into `security_group` and runs `user_data` — the
/// program the guest RUNS at boot, which the API carries base64-encoded.
fn launch_template(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    security_group: &str,
    user_data: &str,
) {
    let encoded = aws_smithy_types::base64::encode(user_data);
    ec2_ok(
        router,
        runtime,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", name),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            // `RequestLaunchTemplateData$SecurityGroupIds` travels as its own `xmlName`,
            // `SecurityGroupId`, which is what an ec2Query client sends.
            ("LaunchTemplateData.SecurityGroupId.1", security_group),
            ("LaunchTemplateData.UserData", encoded.as_str()),
        ],
    );
}

/// Admit TCP `port` to `group` from the source the row needs: another security group, or an
/// address range.
fn authorize(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
    port: &str,
    source: Source<'_>,
) {
    let mut params: Vec<(&str, &str)> = vec![
        ("GroupId", group),
        ("IpPermissions.1.IpProtocol", "tcp"),
        ("IpPermissions.1.FromPort", port),
        ("IpPermissions.1.ToPort", port),
    ];
    match source {
        Source::Group(id) => params.push(("IpPermissions.1.Groups.1.GroupId", id)),
        Source::Range(block) => params.push(("IpPermissions.1.IpRanges.1.CidrIp", block)),
    }
    ec2_ok(router, runtime, "AuthorizeSecurityGroupIngress", &params);
}

/// Where one inbound rule admits its traffic from.
enum Source<'a> {
    /// A security group, which is how a rule names the group a load balancer is associated
    /// with.
    Group(&'a str),
    /// An address range, which is the only way a rule can name a load balancer that is
    /// associated with no security group at all.
    Range(&'a str),
}

/// One registration as `DescribeTargetHealth` reports it, read inside the member that carries
/// it so no field is paired with another registration's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    /// `TargetDescription$Id` and `$Port`: the target and the port it receives traffic on.
    id: String,
    port: Option<String>,
    /// `TargetHealth$State`, `$Reason` and `$Description`, each absent where the state is
    /// reported without it.
    state: String,
    reason: Option<String>,
    description: Option<String>,
    /// `TargetHealthDescription$HealthCheckPort`: "the port to use to connect with the
    /// target", which is the group's effective health-check port.
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
                port: opt_element(target, "Port"),
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

/// What the group `name` reports about its capacity: the instances it holds and the
/// `DesiredCapacity` it is running at.
fn group_instances(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> (Vec<Held>, Option<String>) {
    let body = asg_ok(
        router,
        runtime,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    );
    let instances = block_of(&body, "Instances").unwrap_or_default();
    let held = members_of(instances)
        .into_iter()
        .map(|member| Held {
            instance_id: opt_element(member, "InstanceId").unwrap_or_default(),
            lifecycle_state: opt_element(member, "LifecycleState").unwrap_or_default(),
            health_status: opt_element(member, "HealthStatus").unwrap_or_default(),
        })
        .collect();
    (held, opt_element(&body, "DesiredCapacity"))
}

/// One answered data-plane request: the status and the body, as the wire carries them.
#[derive(Debug, Clone)]
struct Answered {
    status: u16,
    body: String,
}

/// Send one request to the address the load balancer published, carrying no AWS credential —
/// exactly as a client handed that name sends it.
///
/// The same exchange over a connection DIALLED to that address is tabled in
/// `elbv2_serves_an_instance_targets_own_response.rs`; what this suite needs of the data plane
/// is only WHICH registration answers it, which either way of reaching it decides.
fn get(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    dns_name: &str,
    path: &str,
) -> Answered {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("http://{dns_name}{path}"))
        .header("host", dns_name)
        .body(bytes::Bytes::new())
        .expect("a well-formed data-plane request");
    let response = runtime.block_on(router.dispatch(request));
    Answered {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// One row of the serving table: a group that registers one instance into an Application Load
/// Balancer's target group, whose guest serves a body of its own.
struct ServingRow {
    case: &'static str,
    /// What every resource name of this row is suffixed with.
    slug: &'static str,
    /// The guest's own program, as a user-data script: a package's unit started by the init
    /// system, the interpreter's own server module, or both where the row's checks and its
    /// traffic go to different ports. EVERY row leaves a server listening on BOTH ports the
    /// row names, because an HTTP health check to a port whose guest answers nothing there is
    /// a check that fails: what this table is about is which port the checks go to, so
    /// something answers on each of them.
    user_data: &'static str,
    /// The body that program writes and its server then serves — a marker, so a body any
    /// other row's guest produced fails this row.
    marker: &'static str,
    /// `CreateTargetGroup$Port`, "the port on which the targets receive traffic", which is the
    /// port this row's guest listens on.
    traffic_port: &'static str,
    /// `CreateTargetGroup$HealthCheckPort` where the row names one at all.
    health_check_port: Option<&'static str>,
    /// The port the checks of that group therefore connect to, which is what
    /// `TargetHealthDescription$HealthCheckPort` reports.
    checked_port: &'static str,
}

/// The rows span the port the traffic goes to, the three forms the effective health-check port
/// takes, and the two program families that leave a server listening in a guest.
const SERVING_ROWS: [ServingRow; 3] = [
    ServingRow {
        case: "the recorded configuration: the checks go to the port the traffic goes to, and \
               a package's unit serves it",
        slug: "apache",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo ASG-APACHE-GUEST-BODY > /var/www/html/index.html\n\
                    systemctl enable --now httpd\n",
        marker: "ASG-APACHE-GUEST-BODY",
        traffic_port: "80",
        health_check_port: None,
        checked_port: "80",
    },
    ServingRow {
        case: "a health-check port of its own, served by a second program beside the one the \
               traffic goes to",
        slug: "module",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo ASG-MODULE-HEALTH-BODY > /var/www/html/index.html\n\
                    systemctl enable --now httpd\nmkdir -p /srv/app\n\
                    echo ASG-MODULE-GUEST-BODY > /srv/app/index.html\n\
                    python3 -m http.server 8080 --directory /srv/app\n",
        marker: "ASG-MODULE-GUEST-BODY",
        traffic_port: "8080",
        health_check_port: Some("80"),
        checked_port: "80",
    },
    ServingRow {
        case: "the modelled default names the port the traffic goes to",
        slug: "nginx",
        user_data: "#!/bin/bash\nmkdir -p /usr/share/nginx/html\nyum install -y nginx\n\
                    echo ASG-NGINX-GUEST-BODY > /usr/share/nginx/html/index.html\n\
                    systemctl start nginx\n",
        marker: "ASG-NGINX-GUEST-BODY",
        traffic_port: "80",
        health_check_port: Some("traffic-port"),
        checked_port: "80",
    },
];

/// RULE: an instance an Auto Scaling group registers into a target group whose checks a rule
/// admits is reported `healthy` with no reason and no description, an `ELB`-health-checked
/// group reports that verdict as its instance's health, and the load balancer answers a
/// client's request with that instance's own body.
#[test]
fn an_asg_registered_instance_reaches_healthy_and_serves_its_own_body() {
    std::thread::Builder::new()
        .name("asg-registered-target-serves".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = router();
            let runtime = runtime();
            // One network for every row: an Application Load Balancer is placed in two zones,
            // and the internet-facing scheme `CreateLoadBalancer` documents as its default
            // needs the VPC to have an internet gateway.
            let network = seed_network(
                &router,
                &runtime,
                "10.91.0.0/16",
                &[("10.91.1.0/24", "us-west-2a"), ("10.91.2.0/24", "us-west-2b")],
            );
            attach_internet_gateway(&router, &runtime, &network.vpc);
            let balancer_group = security_group(
                &router,
                &runtime,
                &network.vpc,
                "asgserve-balancer",
                "the load balancers",
            );
            let instances_group = security_group(
                &router,
                &runtime,
                &network.vpc,
                "asgserve-instances",
                "the groups instances",
            );
            // The instances admit the load balancers' own security group on every port a row
            // checks and every port a row's traffic goes to: a check arrives only where a rule
            // names both its port and the load balancer performing it. Each DISTINCT port is
            // admitted once — a rule a group already carries is refused as a duplicate.
            let ports: std::collections::BTreeSet<&str> = SERVING_ROWS
                .iter()
                .flat_map(|row| [row.traffic_port, row.checked_port])
                .collect();
            for port in ports {
                authorize(
                    &router,
                    &runtime,
                    &instances_group,
                    port,
                    Source::Group(&balancer_group),
                );
            }

            // Every row's launch template, target group, load balancer, listener and scaling
            // group are created FIRST, so the launches and the checks of all of them run on
            // the one clock instead of one row waiting out the next row's setup.
            struct Deployed<'a> {
                row: &'a ServingRow,
                target_group: String,
                dns_name: String,
                group_name: String,
            }
            let mut deployed: Vec<Deployed<'_>> = Vec::new();
            for row in &SERVING_ROWS {
                let template = format!("asgserve-{}-lt", row.slug);
                launch_template(
                    &router,
                    &runtime,
                    &template,
                    &instances_group,
                    row.user_data,
                );
                let group_label = format!("asgserve-{}-tg", row.slug);
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
                let balancer_name = format!("asgserve-{}-alb", row.slug);
                let created = elbv2_ok(
                    &router,
                    &runtime,
                    "CreateLoadBalancer",
                    &[
                        ("Name", balancer_name.as_str()),
                        ("Type", "application"),
                        ("Subnets.member.1", network.subnets[0].0.as_str()),
                        ("Subnets.member.2", network.subnets[1].0.as_str()),
                        ("SecurityGroups.member.1", balancer_group.as_str()),
                    ],
                );
                let balancer = element(&created, "LoadBalancerArn");
                let dns_name = element(&created, "DNSName");
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
                let group_name = format!("asgserve-{}-asg", row.slug);
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
                        ("VPCZoneIdentifier", network.subnets[0].0.as_str()),
                        ("TargetGroupARNs.member.1", target_group.as_str()),
                        // The load balancer's verdict IS this group's instance health.
                        ("HealthCheckType", "ELB"),
                    ],
                );
                deployed.push(Deployed {
                    row,
                    target_group,
                    dns_name,
                    group_name,
                });
            }

            for entry in &deployed {
                let row = entry.row;
                // The launch, the registration, the guest's boot and the checks are all
                // clock-owed: the row waits for the CONDITION — the verdict, the instance
                // health that follows it, and the answer the guest behind it serves — rather
                // than for a span, and reports what it last read.
                let deadline = Instant::now() + DEADLINE;
                let (reported, held, desired, answered) = loop {
                    let reported = target_health(&router, &runtime, &entry.target_group);
                    let (held, desired) = group_instances(&router, &runtime, &entry.group_name);
                    let answered = get(&router, &runtime, &entry.dns_name, "/");
                    let settled = reported.len() == 1
                        && reported[0].state == HEALTHY
                        && held.len() == 1
                        && held[0].lifecycle_state == IN_SERVICE
                        && held[0].health_status == HEALTHY_INSTANCE
                        && answered.status == 200
                        && answered.body.trim() == row.marker;
                    if settled || Instant::now() >= deadline {
                        break (reported, held, desired, answered);
                    }
                    waited(&runtime);
                };

                assert_eq!(
                    held.len(),
                    1,
                    "case {:?}: the group must hold exactly the one instance its capacity \
                     asks for, got {held:?}",
                    row.case
                );
                assert_eq!(
                    desired.as_deref(),
                    Some("1"),
                    "case {:?}: the group must report the capacity it was created with, got \
                     {desired:?}",
                    row.case
                );
                assert_eq!(
                    (
                        held[0].lifecycle_state.as_str(),
                        held[0].health_status.as_str()
                    ),
                    (IN_SERVICE, HEALTHY_INSTANCE),
                    "case {:?}: an ELB-health-checked group whose target is healthy must \
                     report its instance in service and healthy, got {held:?}",
                    row.case
                );
                assert_eq!(
                    reported,
                    vec![Reported {
                        id: held[0].instance_id.clone(),
                        port: Some(row.traffic_port.to_string()),
                        state: HEALTHY.to_string(),
                        // "If the target state is healthy, a reason code is not provided."
                        reason: None,
                        description: None,
                        health_check_port: Some(row.checked_port.to_string()),
                    }],
                    "case {:?}: expected the group's own instance reported healthy on the \
                     checks of port {}, got {reported:?}",
                    row.case,
                    row.checked_port
                );
                assert_eq!(
                    answered.status, 200,
                    "case {:?}: expected the load balancer to answer the guest's own 200, got \
                     {answered:?}",
                    row.case
                );
                assert_eq!(
                    answered.body.trim(),
                    row.marker,
                    "case {:?}: expected the body this row's guest serves, got {:?}",
                    row.case,
                    answered.body
                );
            }
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}

/// One row of the verdict table: a target group whose effective health-check port is reached
/// one of the three ways the member names it.
struct VerdictRow {
    case: &'static str,
    /// What every resource name of this row is suffixed with.
    slug: &'static str,
    /// `CreateTargetGroup$Port`, which is also this row's listener port.
    traffic_port: &'static str,
    /// `CreateTargetGroup$HealthCheckPort` where the row names one at all.
    health_check_port: Option<&'static str>,
    /// The port the checks of that group therefore connect to: the port whose admission — and
    /// nothing else's — is what lets them arrive.
    checked_port: &'static str,
    /// The guest's own program, as a user-data script. It leaves a server listening on BOTH
    /// ports the row names, and a document at the path its checks ask for, so the verdict
    /// below can only be the security group's doing: something answers on the port the checks
    /// go to, and on the port the traffic goes to.
    user_data: &'static str,
    /// `CreateTargetGroup$HealthCheckProtocol` and `$HealthCheckPath` where the row names
    /// them: a Network Load Balancer's target group may be checked over TCP — the default it
    /// takes from the group's own protocol — or over HTTP, which is the one that asks for a
    /// path ("the GENEVE, TLS, UDP, TCP_UDP, QUIC, and TCP_QUIC protocols are not supported
    /// for health checks").
    health_check_protocol: Option<&'static str>,
    health_check_path: Option<&'static str>,
}

/// The rows span the three forms the effective health-check port takes, whether it is the port
/// the traffic goes to, and the two protocols a Network Load Balancer's checks run over:
/// admitting one row's port admits no other row's checks.
const VERDICT_ROWS: [VerdictRow; 3] = [
    VerdictRow {
        case: "the recorded configuration: an HTTP check on the port the traffic goes to",
        slug: "same",
        traffic_port: "4000",
        health_check_port: None,
        checked_port: "4000",
        user_data: "#!/bin/bash\nmkdir -p /srv/app\necho served > /srv/app/index.html\n\
                    echo served > /srv/app/health-check\n\
                    python3 -m http.server 4000 --directory /srv/app\n",
        health_check_protocol: Some("HTTP"),
        health_check_path: Some("/health-check"),
    },
    VerdictRow {
        case: "a health-check port of its own, beside the port the traffic goes to",
        slug: "own",
        traffic_port: "80",
        health_check_port: Some("7000"),
        checked_port: "7000",
        user_data: "#!/bin/bash\nmkdir -p /var/www/html\nyum install -y httpd\n\
                    echo served > /var/www/html/index.html\nsystemctl enable --now httpd\n\
                    mkdir -p /srv/app\necho served > /srv/app/index.html\n\
                    python3 -m http.server 7000 --directory /srv/app\n",
        health_check_protocol: None,
        health_check_path: None,
    },
    VerdictRow {
        case: "the modelled default names the port the traffic goes to",
        slug: "traffic",
        traffic_port: "5000",
        health_check_port: Some("traffic-port"),
        checked_port: "5000",
        user_data: "#!/bin/bash\nmkdir -p /srv/app\necho served > /srv/app/index.html\n\
                    python3 -m http.server 5000 --directory /srv/app\n",
        health_check_protocol: None,
        health_check_path: None,
    },
];

/// RULE: a registration leaves `initial` for the verdict its checks reached — `unhealthy` with
/// the reason a Network Load Balancer words an unanswered check as, once the group's
/// `UnhealthyThresholdCount` consecutive checks to its effective health-check port could not
/// arrive — and that verdict is derived from the inbound rules rather than latched, so
/// admitting exactly that port from the load balancer's own subnet returns the registration to
/// `healthy` with no reason. A group that checks its instances Amazon EC2's way reads no load
/// balancer, so its instance stays `Healthy` throughout.
#[test]
fn an_nlb_target_reaches_its_verdict_on_the_effective_health_check_port() {
    std::thread::Builder::new()
        .name("asg-registered-target-verdict".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let router = router();
            let runtime = runtime();
            // An isolated network with one subnet, which is where the internal Network Load
            // Balancers below put their own nodes.
            let network = seed_network(
                &router,
                &runtime,
                "172.26.0.0/16",
                &[("172.26.0.0/23", "us-west-2a")],
            );
            let cluster = security_group(
                &router,
                &runtime,
                &network.vpc,
                "asgverdict-cluster",
                "the groups instances",
            );
            // The cluster group admits ITSELF and nothing else — the self-references a cluster
            // security group carries, named both as a port range and as every protocol — so no
            // rule of it names a load balancer outside the group or any address range at all.
            for port in ["988", "1018"] {
                authorize(&router, &runtime, &cluster, port, Source::Group(&cluster));
            }
            ec2_ok(
                &router,
                &runtime,
                "AuthorizeSecurityGroupIngress",
                &[
                    ("GroupId", cluster.as_str()),
                    ("IpPermissions.1.IpProtocol", "-1"),
                    ("IpPermissions.1.Groups.1.GroupId", cluster.as_str()),
                ],
            );

            struct Deployed<'a> {
                row: &'a VerdictRow,
                target_group: String,
                group_name: String,
            }
            let mut deployed: Vec<Deployed<'_>> = Vec::new();
            for row in &VERDICT_ROWS {
                let template = format!("asgverdict-{}-lt", row.slug);
                launch_template(&router, &runtime, &template, &cluster, row.user_data);
                let group_label = format!("asgverdict-{}-tg", row.slug);
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
                if let Some(protocol) = row.health_check_protocol {
                    group_params.push(("HealthCheckProtocol", protocol));
                }
                if let Some(path) = row.health_check_path {
                    group_params.push(("HealthCheckPath", path));
                }
                let target_group = element(
                    &elbv2_ok(&router, &runtime, "CreateTargetGroup", &group_params),
                    "TargetGroupArn",
                );
                let balancer_name = format!("asgverdict-{}-nlb", row.slug);
                let balancer = element(
                    &elbv2_ok(
                        &router,
                        &runtime,
                        "CreateLoadBalancer",
                        &[
                            ("Name", balancer_name.as_str()),
                            ("Type", "network"),
                            ("Scheme", "internal"),
                            ("Subnets.member.1", network.subnets[0].0.as_str()),
                        ],
                    ),
                    "LoadBalancerArn",
                );
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
                let group_name = format!("asgverdict-{}-asg", row.slug);
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
                        ("VPCZoneIdentifier", network.subnets[0].0.as_str()),
                        ("TargetGroupARNs.member.1", target_group.as_str()),
                        // The group checks its instances Amazon EC2's way, so the verdict
                        // asserted below is the load balancer's alone.
                        ("HealthCheckType", "EC2"),
                    ],
                );
                deployed.push(Deployed {
                    row,
                    target_group,
                    group_name,
                });
            }

            // The FIRST verdict each row reaches, so a wrong one fails here instead of being
            // waited out: the only states a registration may be reported in before one is
            // reached are the `initial` the model gives a registration whose checks have not
            // decided yet.
            let mut first: Vec<Option<Reported>> = vec![None; VERDICT_ROWS.len()];
            let mut last: Vec<Vec<Reported>> = vec![Vec::new(); VERDICT_ROWS.len()];
            let deadline = Instant::now() + DEADLINE;
            loop {
                for (index, entry) in deployed.iter().enumerate() {
                    let reported = target_health(&router, &runtime, &entry.target_group);
                    if first[index].is_none() {
                        if let Some(verdict) =
                            reported.iter().find(|report| report.state != INITIAL)
                        {
                            first[index] = Some(verdict.clone());
                        }
                    }
                    last[index] = reported;
                }
                if first.iter().all(Option::is_some) || Instant::now() >= deadline {
                    break;
                }
                waited(&runtime);
            }

            for (index, entry) in deployed.iter().enumerate() {
                let row = entry.row;
                let verdict = first[index].clone().unwrap_or_else(|| {
                    panic!(
                        "case {:?}: no verdict was ever reported for the group's registration; \
                         last read {:?}",
                        row.case, last[index]
                    )
                });
                assert_eq!(
                    (
                        verdict.state.as_str(),
                        verdict.reason.as_deref(),
                        verdict.health_check_port.as_deref()
                    ),
                    (UNHEALTHY, Some(FAILED_HEALTH_CHECKS), Some(row.checked_port)),
                    "case {:?}: expected the first verdict to be {UNHEALTHY}/\
                     {FAILED_HEALTH_CHECKS} on the checks of port {}, got {verdict:?}",
                    row.case,
                    row.checked_port
                );
                // The group checks its instances Amazon EC2's way, so the load balancer's
                // verdict is not its instance's health: it reports the instance healthy
                // while that same target is unhealthy.
                let (held, _) = group_instances(&router, &runtime, &entry.group_name);
                assert_eq!(
                    held.iter()
                        .map(|instance| {
                            (
                                instance.lifecycle_state.as_str(),
                                instance.health_status.as_str(),
                            )
                        })
                        .collect::<Vec<_>>(),
                    vec![(IN_SERVICE, HEALTHY_INSTANCE)],
                    "case {:?}: an EC2-health-checked group must report its one instance in \
                     service and healthy beside an unhealthy target, got {held:?}",
                    row.case
                );
            }

            // Now the remediation, one rule per row: each row's own effective health-check
            // port, admitted from the block the load balancer's nodes sit in — which is the
            // only way a rule can name a Network Load Balancer that is associated with no
            // security group.
            for row in &VERDICT_ROWS {
                authorize(
                    &router,
                    &runtime,
                    &cluster,
                    row.checked_port,
                    Source::Range(network.subnets[0].1.as_str()),
                );
            }
            let deadline = Instant::now() + DEADLINE;
            let recovered = loop {
                let recovered: Vec<Vec<Reported>> = deployed
                    .iter()
                    .map(|entry| target_health(&router, &runtime, &entry.target_group))
                    .collect();
                let settled = recovered
                    .iter()
                    .all(|reported| reported.len() == 1 && reported[0].state == HEALTHY);
                if settled || Instant::now() >= deadline {
                    break recovered;
                }
                waited(&runtime);
            };
            for (index, entry) in deployed.iter().enumerate() {
                let row = entry.row;
                let reported = &recovered[index];
                assert_eq!(
                    reported.len(),
                    1,
                    "case {:?}: the target group must hold exactly the group's instance, got \
                     {reported:?}",
                    row.case
                );
                assert_eq!(
                    (
                        reported[0].state.as_str(),
                        reported[0].reason.as_deref(),
                        reported[0].description.as_deref()
                    ),
                    (HEALTHY, None, None),
                    "case {:?}: a registration whose checks arrive once port {} is admitted \
                     must be reported {HEALTHY} with no reason and no description, got \
                     {reported:?}",
                    row.case,
                    row.checked_port
                );
            }
        })
        .expect("the sized thread")
        .join()
        .expect("the suite ran");
}
