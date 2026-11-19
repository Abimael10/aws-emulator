// SPDX-License-Identifier: CC0-1.0

//! The `HealthStatus` an Auto Scaling group whose `HealthCheckType` names `ELB` reports for
//! its instances when the load balancer checking them is a CLASSIC Load Balancer — through
//! the WHOLE mounted service set, the one `core/router/src/lib.rs` assembles for the binary.
//!
//! RULE: with `HealthCheckType` naming `ELB`, a group takes the verdict of the load balancers
//! it is attached to as the health of its instances — `CreateAutoScalingGroup$HealthCheckType`
//! is "the service you want the health status from" and `ELB` means "use Elastic Load
//! Balancing health checks" — so an instance whose registration a load balancer the group is
//! attached to reports as having failed that load balancer's health checks is reported
//! `HealthStatus=Unhealthy` by `DescribeAutoScalingInstances` and `DescribeAutoScalingGroups`,
//! and `Healthy` while no such load balancer reports it failing. A member reported unhealthy
//! then owes what any failed health check of a group owes — "Amazon EC2 Auto Scaling
//! terminates and replaces the unhealthy instance" (`SetInstanceHealth$HealthStatus`) — so the
//! verdict is one the group acts on and not only one it reports.
//!
//! The verdict belongs to the service that performs the checks: a Classic Load Balancer
//! reports a registration whose checks it has run `InService` while they are answered, and
//! `OutOfService` with `ReasonCode=Instance` once `UnhealthyThreshold` consecutive checks were
//! not — "information about the cause of `OutOfService` instances. Specifically, whether the
//! cause is Elastic Load Balancing or the instance" (`InstanceState$ReasonCode`). A
//! registration whose own checks are still running is out of service for Elastic Load
//! Balancing's own reason instead, which is no failed check and no reason to call the instance
//! unhealthy.
//!
//! The rows span what the rule generalises over: how the group came to hold the attachment
//! (its own `LoadBalancerNames`, or an `AttachLoadBalancers` after the create), the form its
//! `HealthCheckType` takes (`ELB` alone, and the comma-separated list the member documents),
//! and — in the second test — the `HealthCheckGracePeriod` the derivation waits out before it
//! marks an instance on a failed check at all.

use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{elements, form_encode, members_of};

/// The Region every request below is signed for, and the Availability Zone every group places
/// its instance in and every load balancer is enabled for.
const REGION: &str = "us-east-1";
const ZONE: &str = "us-east-1a";

/// The API versions an ec2Query and each awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const CLASSIC_ELB_VERSION: &str = "2012-06-01";
const ASG_VERSION: &str = "2011-01-01";

/// The AMI and instance type every launch template here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The thresholds every load balancer of this suite is configured with: the fewest
/// consecutive checks a verdict can take either way, because a verdict about a registration is
/// that many checks away. What this suite asserts is WHICH verdict the group reads, not how
/// long Elastic Load Balancing takes to reach one.
const HEALTHY_THRESHOLD: &str = "2";
const UNHEALTHY_THRESHOLD: &str = "2";

/// How long a leg waits for the clock-owed moves it depends on — the instance's launch, its
/// registration, and the health checks of it, which come one cadence apart so a verdict is a
/// threshold of them away — and how often it looks. Generous, because the deadline is only
/// here so a failure is a failure rather than a hang: every wait below looks for its
/// CONDITION, so machine load decides when a leg finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// How long a leg keeps re-reading a status that must NOT change, and how often: the absence
/// of a change is asserted over a run of reads rather than over one.
const HELD_READS: usize = 20;

/// The `HealthStatus` values Auto Scaling reports for an instance
/// (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="HealthStatus")`).
const HEALTHY: &str = "Healthy";
const UNHEALTHY: &str = "Unhealthy";

/// The `LifecycleState` of an instance the group has brought into service.
const IN_SERVICE: &str = "InService";

/// The `InstanceState$State` and `$ReasonCode` a Classic Load Balancer reports for a
/// registration it routes traffic to, and for one that has failed its checks.
const ELB_IN_SERVICE: &str = "InService";
const ELB_OUT_OF_SERVICE: &str = "OutOfService";
const ELB_REASON_INSTANCE: &str = "Instance";

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must succeed.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Classic Elastic Load Balancing action that must succeed.
async fn elb(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancing",
        form(action, CLASSIC_ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Elastic Load Balancing {action} must succeed, got {status} with body \
         {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Create the launch template a group launches from: an AMI and an instance type, and no user
/// data at all, so the instance runs whatever its image runs and nothing else.
async fn launch_template(router: &MultiServiceRouter, case: &str, name: &str) {
    ec2(
        router,
        case,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", name),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
}

/// Create one Classic Load Balancer with an HTTP listener and a health check that reaches a
/// verdict in as few checks as the two thresholds allow.
async fn load_balancer(router: &MultiServiceRouter, case: &str, name: &str) {
    elb(
        router,
        case,
        "CreateLoadBalancer",
        &[
            ("LoadBalancerName", name),
            ("AvailabilityZones.member.1", ZONE),
            ("Listeners.member.1.Protocol", "HTTP"),
            ("Listeners.member.1.LoadBalancerPort", "80"),
            ("Listeners.member.1.InstanceProtocol", "HTTP"),
            ("Listeners.member.1.InstancePort", "80"),
        ],
    )
    .await;
    elb(
        router,
        case,
        "ConfigureHealthCheck",
        &[
            ("LoadBalancerName", name),
            ("HealthCheck.Target", "HTTP:80/"),
            ("HealthCheck.Interval", "5"),
            ("HealthCheck.Timeout", "2"),
            ("HealthCheck.HealthyThreshold", HEALTHY_THRESHOLD),
            ("HealthCheck.UnhealthyThreshold", UNHEALTHY_THRESHOLD),
        ],
    )
    .await;
}

/// The `(InstanceId, HealthStatus)` pairs `DescribeAutoScalingInstances` reports for the
/// instances of the group `group`, each read from the member that carries both.
async fn reported_health(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
    let body = asg(router, case, "DescribeAutoScalingInstances", &[]).await;
    members_of(&body)
        .into_iter()
        .filter(|member| elements(member, "AutoScalingGroupName") == vec![group.to_string()])
        .filter_map(|member| {
            let id = elements(member, "InstanceId").into_iter().next()?;
            let status = elements(member, "HealthStatus").into_iter().next()?;
            Some((id, status))
        })
        .collect()
}

/// The `(LifecycleState, InstanceId)` pairs the group `group` reports for its own members,
/// through the reader whose subject the group is.
async fn group_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<(String, String)> {
    let body = asg(
        router,
        case,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let state = elements(member, "LifecycleState").into_iter().next()?;
            let id = elements(member, "InstanceId").into_iter().next()?;
            Some((state, id))
        })
        .collect()
}

/// The `(State, ReasonCode)` the load balancer `name` reports for the registration of
/// `instance_id`, through Elastic Load Balancing's OWN read.
async fn registration_health(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) -> (String, String) {
    let body = elb(
        router,
        case,
        "DescribeInstanceHealth",
        &[
            ("LoadBalancerName", name),
            ("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
    let state = elements(&body, "State").into_iter().next().unwrap_or_default();
    let reason = elements(&body, "ReasonCode")
        .into_iter()
        .next()
        .unwrap_or_default();
    (state, reason)
}

/// The one instance the group `group` holds, once the group has brought it into service and
/// the load balancer `name` reports its registration `InService` — the state a client waits
/// for before it sends the load balancer traffic.
///
/// Both moves are owed to the clock (the launch, the registration, and the threshold of
/// answered checks behind the verdict), so this waits for the CONDITION rather than for a
/// span.
async fn serving_instance(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router, case, group).await;
        let in_service: Vec<String> = held
            .iter()
            .filter(|(state, _)| state == IN_SERVICE)
            .map(|(_, id)| id.clone())
            .collect();
        let reported = match in_service.first() {
            Some(id) => registration_health(router, case, name, id).await,
            None => (String::new(), String::new()),
        };
        if reported.0 == ELB_IN_SERVICE {
            return in_service[0].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group's instance to be in service and its \
             registration with {name} to be reported {ELB_IN_SERVICE}, got instances {held:?} \
             and registration {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until the load balancer `name` reports the registration of `instance_id` as having
/// FAILED its checks — out of service for the instance's own doing, which is the verdict the
/// group reads.
async fn failing_registration(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = registration_health(router, case, name, instance_id).await;
        if reported == (ELB_OUT_OF_SERVICE.to_string(), ELB_REASON_INSTANCE.to_string()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {name} to report {instance_id} \
             {ELB_OUT_OF_SERVICE}/{ELB_REASON_INSTANCE}, got {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until the group `group` reports `wanted` as the `HealthStatus` of `instance_id`.
async fn health_reaching(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = reported_health(router, case, group).await;
        let status = reported
            .iter()
            .find(|(id, _)| id == instance_id)
            .map(|(_, status)| status.clone())
            .unwrap_or_default();
        if status == wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to report {instance_id} {wanted}, got {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait until the group `group` has taken `condemned` out and holds another instance in
/// service instead — the one path a failed health check takes a member of a group out by:
/// "Amazon EC2 Auto Scaling terminates and replaces the unhealthy instance"
/// (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="HealthStatus")`), so the
/// verdict the group read is one it ACTS on rather than only reports.
async fn replaced_instance(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    condemned: &str,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router, case, group).await;
        let gone = !held.iter().any(|(_, id)| id == condemned);
        let serving = held
            .iter()
            .any(|(state, id)| state == IN_SERVICE && id != condemned);
        if gone && serving {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to take {condemned} out and hold a replacement in \
             service, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Assert the group `group` goes on reporting `wanted` for `instance_id` over a run of reads.
async fn health_held(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    instance_id: &str,
    wanted: &str,
) {
    for read in 1..=HELD_READS {
        let reported = reported_health(router, case, group).await;
        let status = reported
            .iter()
            .find(|(id, _)| id == instance_id)
            .map(|(_, status)| status.clone())
            .unwrap_or_default();
        assert_eq!(
            status, wanted,
            "case {case:?}: read {read} of {HELD_READS}: expected {group} to go on reporting \
             {instance_id} {wanted}, got {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: the configuration of the group whose instance the load balancer then stops
/// answering for.
struct Row {
    case: &'static str,
    group: &'static str,
    load_balancer: &'static str,
    template: &'static str,
    /// The `HealthCheckType` the group is created with.
    health_check_type: &'static str,
    /// Whether the group comes to hold the attachment through `AttachLoadBalancers` after the
    /// create, rather than through its own `LoadBalancerNames`.
    attached_after_create: bool,
}

/// The rows span how the group came to hold the attachment and the form its `HealthCheckType`
/// takes — "a comma-separated value string of one or more health check types"
/// (`CreateAutoScalingGroup$HealthCheckType`), so a group naming `ELB` beside another type
/// takes the load balancer's verdict exactly as one naming `ELB` alone does.
const ROWS: [Row; 3] = [
    Row {
        case: "a group attached by its own LoadBalancerNames",
        group: "asgclb-own",
        load_balancer: "asgclb-own-elb",
        template: "asgclb-own-lt",
        health_check_type: "ELB",
        attached_after_create: false,
    },
    Row {
        case: "a group attached by AttachLoadBalancers after the create",
        group: "asgclb-attached",
        load_balancer: "asgclb-attached-elb",
        template: "asgclb-attached-lt",
        health_check_type: "ELB",
        attached_after_create: true,
    },
    Row {
        case: "a group naming ELB beside the other health check type",
        group: "asgclb-both",
        load_balancer: "asgclb-both-elb",
        template: "asgclb-both-lt",
        health_check_type: "ELB,EC2",
        attached_after_create: false,
    },
];

/// Create the group of `row` with `grace` as its `HealthCheckGracePeriod`, attached to its
/// Classic Load Balancer however that row holds the attachment.
async fn seed_group(router: &MultiServiceRouter, row: &Row, grace: &str) {
    launch_template(router, row.case, row.template).await;
    load_balancer(router, row.case, row.load_balancer).await;
    let mut params: Vec<(&str, &str)> = vec![
        ("AutoScalingGroupName", row.group),
        ("LaunchTemplate.LaunchTemplateName", row.template),
        ("AvailabilityZones.member.1", ZONE),
        ("MinSize", "1"),
        ("MaxSize", "1"),
        ("DesiredCapacity", "1"),
        ("HealthCheckType", row.health_check_type),
        ("HealthCheckGracePeriod", grace),
    ];
    if !row.attached_after_create {
        params.push(("LoadBalancerNames.member.1", row.load_balancer));
    }
    asg(router, row.case, "CreateAutoScalingGroup", &params).await;
    if row.attached_after_create {
        asg(
            router,
            row.case,
            "AttachLoadBalancers",
            &[
                ("AutoScalingGroupName", row.group),
                ("LoadBalancerNames.member.1", row.load_balancer),
            ],
        )
        .await;
    }
}

/// RULE: a group whose `HealthCheckType` names `ELB` reports the instance its Classic Load
/// Balancer has failed as `Unhealthy`, and then acts on that check the way it acts on any
/// other failed one — the member is taken out of service and replaced.
#[tokio::test]
async fn an_elb_health_checked_group_reports_the_verdict_of_its_classic_load_balancer() {
    for row in &ROWS {
        // Each row runs on its own emulator, so one row's load balancer cannot decide
        // another's instance.
        let router = router();
        seed_group(&router, row, "0").await;

        // The instance is in service and its registration is one the load balancer routes
        // traffic to, so the group reports it healthy: a verdict that found nothing wrong is
        // not a failed check.
        let instance = serving_instance(&router, row.case, row.group, row.load_balancer).await;
        health_held(&router, row.case, row.group, &instance, HEALTHY).await;

        // Nothing on the instance answers the load balancer's checks any more. Stopping it
        // through Amazon EC2's own API is what a client does to a machine whose application
        // has gone away: the instance stays a member of the group, and the load balancer's
        // own checks are what notice.
        ec2(
            &router,
            row.case,
            "StopInstances",
            &[("InstanceId.1", instance.as_str())],
        )
        .await;
        failing_registration(&router, row.case, row.load_balancer, &instance).await;
        health_reaching(&router, row.case, row.group, &instance, UNHEALTHY).await;

        // And the group ACTS on the check it read: the member it reports unhealthy is taken
        // out of service and the capacity it was providing is launched again.
        replaced_instance(&router, row.case, row.group, &instance).await;
    }
}

/// RULE: `HealthCheckGracePeriod` is "the amount of time, in seconds, that Amazon EC2 Auto
/// Scaling waits before checking the health status of an EC2 instance that has come into
/// service and marking it unhealthy due to a failed health check", so an instance inside that
/// window keeps `Healthy` however its registration is reported.
#[tokio::test]
async fn an_instance_inside_the_grace_period_is_not_marked_on_a_failed_check() {
    let row = &Row {
        case: "an instance still inside its group's health-check grace period",
        group: "asgclb-grace",
        load_balancer: "asgclb-grace-elb",
        template: "asgclb-grace-lt",
        health_check_type: "ELB",
        attached_after_create: false,
    };
    let router = router();
    // An hour of grace, which no leg of this test waits out: the window is what the
    // derivation must respect, not a span anything here sleeps for.
    seed_group(&router, row, "3600").await;

    let instance = serving_instance(&router, row.case, row.group, row.load_balancer).await;
    ec2(
        &router,
        row.case,
        "StopInstances",
        &[("InstanceId.1", instance.as_str())],
    )
    .await;
    // The load balancer has reached the failing verdict — so what the group reports from here
    // is the derivation's own reading of the grace period rather than a verdict that has not
    // arrived yet.
    failing_registration(&router, row.case, row.load_balancer, &instance).await;
    health_held(&router, row.case, row.group, &instance, HEALTHY).await;

    // And the consequence a failed check would have had is held off with the report: the
    // member is still the one the group holds, in service, rather than one it has replaced.
    let held = group_instances(&router, row.case, row.group).await;
    assert_eq!(
        held,
        vec![(IN_SERVICE.to_string(), instance.clone())],
        "case {:?}: expected {} to still hold {instance} in service, got {held:?}",
        row.case,
        row.group
    );
}
