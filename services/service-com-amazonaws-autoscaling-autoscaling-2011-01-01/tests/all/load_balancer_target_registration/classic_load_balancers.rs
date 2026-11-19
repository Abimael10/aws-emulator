// SPDX-License-Identifier: CC0-1.0

//! The Classic Load Balancers an Auto Scaling group is ATTACHED to, and the registrations
//! its instances hold in them.
//!
//! The rule under test: a group's Classic Load Balancer attachments are the set its own
//! `LoadBalancerNames` holds — `AttachLoadBalancers` adds to it ("this operation is additive
//! and does not detach existing Classic Load Balancers or target groups from the Auto Scaling
//! group"), `DetachLoadBalancers` takes entries out of it, each name is held once and in the
//! order the group came to hold it, and `DescribeLoadBalancers` reports exactly that set with
//! the state each attachment is in. An attachment is what puts the group's instances behind
//! the load balancer — "Amazon EC2 Auto Scaling registers the running instances with these
//! Classic Load Balancers" — so every instance the group holds is a registered instance of
//! every load balancer it is attached to, an instance that leaves the group is deregistered,
//! and a detach takes that load balancer's registrations with it ("it enters the Removing
//! state while deregistering the instances in the group … the instances remain running").
//!
//! The registrations are asserted where a client sees them: Elastic Load Balancing's own
//! `DescribeLoadBalancers` and `DescribeInstanceHealth`, over its own wire, with all three
//! services (Auto Scaling, Amazon EC2 and Elastic Load Balancing) mounted on ONE engine — so
//! a registration asserted here is one the owning service returns from its own read, and the
//! instance ids are the account's own Amazon EC2 instances.
//!
//! The tables below span what the rule generalises over: how many load balancers a group is
//! attached to, how it came to hold them (the create or a later attach), how many times a
//! name is given, how many instances the group holds, which of them a detach or a scale-in
//! takes out, the documented per-call bound, a name no load balancer answers to, and whether
//! Elastic Load Balancing health checks are enabled for the group.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7 as elb_service;

use crate::support::{
    borrowed, form_encode, p, query, seed_launch_templates, tag_value, tag_values, REGION,
};

/// The launch template every group in this suite launches from, seeded into the account's own
/// Amazon EC2 store by the harness.
const LAUNCH_TEMPLATE: &str = "lt-classic-elb";

/// The Availability Zone every group places its instances in and every load balancer in this
/// suite is enabled for.
const ZONE: &str = "us-west-2a";

/// The API version an Elastic Load Balancing (Classic) client puts in its awsQuery body.
const ELB_API_VERSION: &str = "2012-06-01";

/// The most load balancers ONE attach or detach may name, from the model's own documentation
/// for `AttachLoadBalancersType$LoadBalancerNames`: "You can specify up to 10 load
/// balancers."
const MAX_LOAD_BALANCERS_PER_CALL: usize = 10;

/// The `LoadBalancerState$State` values this suite asserts, from that member's own catalog
/// (`smithy_query(query="shape",shape="LoadBalancerState")`).
const ADDED: &str = "Added";
const IN_SERVICE: &str = "InService";

/// The `InstanceState$State` of a registration Elastic Load Balancing routes traffic to,
/// from that member's own documented values.
const HEALTH_IN_SERVICE: &str = "InService";

/// How often a poll re-reads the surface it is waiting on.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the spans the three services owe a
/// transition — this service's lifecycle span, Amazon EC2's, and the cadence of an Elastic
/// Load Balancing health check times the threshold the load balancers here are configured
/// with — with ample margin for a loaded test machine. Read from the services' own spans
/// rather than written out, so they cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION
        + ::service_com_amazonaws_ec2_amazonec2::state::TRANSITION_DELAY
        + elb_service::state::HEALTH_CHECK_SPAN * (HEALTHY_THRESHOLD as u32 + 2)
        + Duration::from_secs(30)
}

/// The `HealthCheck$HealthyThreshold` every load balancer in this suite is configured with:
/// the smallest the member admits, so a registration reaches `InService` in as few checks as
/// Elastic Load Balancing will take.
const HEALTHY_THRESHOLD: i32 = 2;

/// One emulator with the three services on ONE engine: a group's instances are the account's
/// own Amazon EC2 instances, and the load balancers it is attached to and the registrations
/// they hold are Elastic Load Balancing's own records, so one engine is what makes the three
/// views one account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(elb_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// POST one awsQuery action to Elastic Load Balancing, the way a client does, and answer its
/// status and body.
async fn elb_query(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={ELB_API_VERSION}");
    for (key, value) in params {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://elasticloadbalancing.{REGION}.amazonaws.com/"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}\
                 /elasticloadbalancing/aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// Create one Classic Load Balancer over Elastic Load Balancing's own wire, with one HTTP
/// listener and a health check that reaches a verdict in [`HEALTHY_THRESHOLD`] checks.
async fn create_load_balancer(router: &MultiServiceRouter, name: &str) {
    let (status, body) = elb_query(
        router,
        "CreateLoadBalancer",
        &[
            p("LoadBalancerName", name),
            p("AvailabilityZones.member.1", ZONE),
            p("Listeners.member.1.Protocol", "HTTP"),
            p("Listeners.member.1.LoadBalancerPort", "80"),
            p("Listeners.member.1.InstanceProtocol", "HTTP"),
            p("Listeners.member.1.InstancePort", "80"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLoadBalancer for {name:?} expected 200, got {status}: {body}"
    );
    let (status, body) = elb_query(
        router,
        "ConfigureHealthCheck",
        &[
            p("LoadBalancerName", name),
            p("HealthCheck.Target", "HTTP:80/"),
            p("HealthCheck.Interval", "5"),
            p("HealthCheck.Timeout", "2"),
            p("HealthCheck.HealthyThreshold", HEALTHY_THRESHOLD.to_string()),
            p("HealthCheck.UnhealthyThreshold", "2"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "ConfigureHealthCheck for {name:?} expected 200, got {status}: {body}"
    );
}

/// The instance ids one load balancer reports as registered with it, through Elastic Load
/// Balancing's OWN read.
async fn registered_instances(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = elb_query(
        router,
        "DescribeLoadBalancers",
        &[p("LoadBalancerNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "Elastic Load Balancing DescribeLoadBalancers for {name:?} expected 200, got \
         {status}: {body}"
    );
    tag_values(&body, "InstanceId")
}

/// The health Elastic Load Balancing reports for `instance_id` with respect to `name`.
async fn reported_health(
    router: &MultiServiceRouter,
    name: &str,
    instance_id: &str,
) -> String {
    let (status, body) = elb_query(
        router,
        "DescribeInstanceHealth",
        &[
            p("LoadBalancerName", name),
            p("Instances.member.1.InstanceId", instance_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeInstanceHealth for {name:?} expected 200, got {status}: {body}"
    );
    tag_value(&body, "State")
}

/// Create one group, with the members `extra` carries beyond its launch source and its
/// placement, and assert the create succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str, extra: &[(String, String)]) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("AvailabilityZones.member.1", ZONE),
        p("MinSize", "0"),
        p("MaxSize", "4"),
    ];
    params.extend(extra.iter().cloned());
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// One `AttachLoadBalancers` or `DetachLoadBalancers` call naming `names`, answering the
/// status, the wire error code and the message.
async fn load_balancer_call(
    router: &MultiServiceRouter,
    action: &str,
    group: &str,
    names: &[&str],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, name) in names.iter().enumerate() {
        params.push(p(&format!("LoadBalancerNames.member.{}", index + 1), *name));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        tag_value(&body, "Message"),
    )
}

/// The `(name, state)` pairs `DescribeLoadBalancers` reports for one group, in the order
/// reported.
async fn described_attachments(
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeLoadBalancers",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLoadBalancers for {group:?} expected 200, got {status}: {body}"
    );
    let names = tag_values(&body, "LoadBalancerName");
    let states = tag_values(&body, "State");
    names.into_iter().zip(states).collect()
}

/// The names `DescribeLoadBalancers` reports for one group, in the order reported.
async fn attached_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    described_attachments(router, group)
        .await
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The group's own `LoadBalancerNames`, as `DescribeAutoScalingGroups` reports them: the
/// member the attach writes, read back through the group's own description.
async fn group_load_balancer_names(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    let inner = body
        .split_once("<LoadBalancerNames>")
        .and_then(|(_, after)| after.split_once("</LoadBalancerNames>"))
        .map(|(inner, _)| inner.to_string())
        .unwrap_or_default();
    tag_values(&inner, "member")
}

/// The instance ids the group holds, as `DescribeAutoScalingGroups` reports them.
async fn group_instances(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {group:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
}

/// Read the group until it holds `wanted` instance ids, or a generous deadline passes — the
/// CONDITION, never a fixed wait.
async fn settled_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = group_instances(router, group).await;
        if held.len() == wanted || Instant::now() >= deadline {
            assert_eq!(
                held.len(),
                wanted,
                "case {case:?}: expected the group to hold {wanted} instance(s), got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read one load balancer's registrations until they are exactly `wanted` (in any order), or
/// a generous deadline passes.
async fn settled_registrations(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &[String],
) {
    let deadline = Instant::now() + poll_budget();
    let mut expected: Vec<String> = wanted.to_vec();
    expected.sort();
    loop {
        let mut held = registered_instances(router, name).await;
        held.sort();
        if held == expected || Instant::now() >= deadline {
            assert_eq!(
                held, expected,
                "case {case:?}: expected {name:?} to hold the registrations {expected:?}, got \
                 {held:?}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the group's attachments until `name` is reported in the state `wanted`, or a generous
/// deadline passes.
async fn settled_attachment_state(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    name: &str,
    wanted: &str,
) {
    let deadline = Instant::now() + poll_budget();
    loop {
        let reported = described_attachments(router, group).await;
        let state = reported
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, state)| state.clone())
            .unwrap_or_default();
        if state == wanted || Instant::now() >= deadline {
            assert_eq!(
                state, wanted,
                "case {case:?}: expected {name:?} to be reported {wanted:?}, got {reported:?}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One attachment case: how the group came to hold its load balancers, and what it must then
/// report.
struct AttachCase {
    case: &'static str,
    /// The names the CREATE carried, if any.
    created_with: Vec<&'static str>,
    /// One entry per `AttachLoadBalancers` call, in the order they are made.
    attaches: Vec<Vec<&'static str>>,
    /// The names the group must then hold, in order.
    held: Vec<&'static str>,
}

/// Every load balancer name any row of the table below names, created once per emulator so a
/// name a row attaches resolves to a load balancer of the account.
fn suite_load_balancers() -> Vec<String> {
    let mut names: Vec<String> = vec!["clb-a".to_string(), "clb-b".to_string(), "clb-c".to_string()];
    names.extend((1..=MAX_LOAD_BALANCERS_PER_CALL).map(|index| format!("clb-bulk-{index}")));
    names
}

/// The attachment of Classic Load Balancers to a group is ADDITIVE and holds each name ONCE,
/// in the order the group came to hold it — and what the group holds is what both reads of it
/// report.
#[tokio::test]
async fn classic_load_balancer_attachments_are_additive_and_held_once() {
    let bulk: Vec<&'static str> = vec![
        "clb-bulk-1",
        "clb-bulk-2",
        "clb-bulk-3",
        "clb-bulk-4",
        "clb-bulk-5",
        "clb-bulk-6",
        "clb-bulk-7",
        "clb-bulk-8",
        "clb-bulk-9",
        "clb-bulk-10",
    ];
    assert_eq!(
        bulk.len(),
        MAX_LOAD_BALANCERS_PER_CALL,
        "the bulk row must name exactly the documented per-call bound, got {}",
        bulk.len()
    );
    let cases = [
        AttachCase {
            case: "one attach of one load balancer",
            created_with: vec![],
            attaches: vec![vec!["clb-a"]],
            held: vec!["clb-a"],
        },
        AttachCase {
            case: "additive across two calls",
            created_with: vec![],
            attaches: vec![vec!["clb-a"], vec!["clb-b"]],
            held: vec!["clb-a", "clb-b"],
        },
        AttachCase {
            case: "a name the group already holds keeps its position",
            created_with: vec![],
            attaches: vec![vec!["clb-a", "clb-b"], vec!["clb-a"]],
            held: vec!["clb-a", "clb-b"],
        },
        AttachCase {
            case: "a name repeated inside one call is held once",
            created_with: vec![],
            attaches: vec![vec!["clb-c", "clb-c"]],
            held: vec!["clb-c"],
        },
        AttachCase {
            case: "the attach joins what the create named",
            created_with: vec!["clb-a"],
            attaches: vec![vec!["clb-b", "clb-c"]],
            held: vec!["clb-a", "clb-b", "clb-c"],
        },
        AttachCase {
            case: "the documented per-call bound in one call",
            created_with: vec![],
            attaches: vec![bulk.clone()],
            held: bulk.clone(),
        },
    ];

    let (router, _engine) = emulator();
    for name in suite_load_balancers() {
        create_load_balancer(&router, &name).await;
    }

    for (index, case) in cases.iter().enumerate() {
        let group = format!("asg-attach-{index}");
        let mut extra: Vec<(String, String)> = vec![p("DesiredCapacity", "0")];
        for (position, name) in case.created_with.iter().enumerate() {
            extra.push(p(
                &format!("LoadBalancerNames.member.{}", position + 1),
                *name,
            ));
        }
        create_group(&router, &group, &extra).await;
        for attach in &case.attaches {
            let (status, code, message) =
                load_balancer_call(&router, "AttachLoadBalancers", &group, attach).await;
            assert_eq!(
                status, 200,
                "case {:?}: AttachLoadBalancers({attach:?}) expected 200, got {status} \
                 {code:?} {message:?}",
                case.case
            );
        }

        let reported = described_attachments(&router, &group).await;
        let names: Vec<String> = reported.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names, case.held,
            "case {:?}: expected the attachments {:?}, got {reported:?}",
            case.case, case.held
        );
        // A group holding no instance has every instance of it registered with none: "Added -
        // all Auto Scaling instances are registered with the load balancer".
        for (name, state) in &reported {
            assert_eq!(
                state, ADDED,
                "case {:?}: expected {name:?} to be reported {ADDED:?} for a group holding no \
                 instance, got {state:?}",
                case.case
            );
        }
        // The same set, read back through the group's own description: one collection, two
        // reads of it.
        let on_the_group = group_load_balancer_names(&router, &group).await;
        assert_eq!(
            on_the_group, case.held,
            "case {:?}: expected DescribeAutoScalingGroups to report {:?}, got {on_the_group:?}",
            case.case, case.held
        );
    }
}

/// One detach case: what the group holds, what the detach names, and what it must leave.
struct DetachCase {
    case: &'static str,
    attached: Vec<&'static str>,
    detach: Vec<&'static str>,
    /// The names the group must hold afterwards, in order.
    remaining: Vec<&'static str>,
    /// The message the call is refused with, or `None` when it must succeed.
    refusal: Option<String>,
}

/// A detach removes exactly the load balancers it names, and one name the group does not hold
/// refuses the whole call.
#[tokio::test]
async fn a_detach_removes_only_the_load_balancers_it_names() {
    let cases = [
        DetachCase {
            case: "one of two",
            attached: vec!["clb-a", "clb-b"],
            detach: vec!["clb-a"],
            remaining: vec!["clb-b"],
            refusal: None,
        },
        DetachCase {
            case: "all of them",
            attached: vec!["clb-a", "clb-b", "clb-c"],
            detach: vec!["clb-a", "clb-b", "clb-c"],
            remaining: vec![],
            refusal: None,
        },
        DetachCase {
            case: "a name the group does not hold",
            attached: vec!["clb-a"],
            detach: vec!["clb-b"],
            remaining: vec!["clb-a"],
            refusal: Some(
                "Trying to remove Load Balancers that are not part of the group: clb-b"
                    .to_string(),
            ),
        },
        DetachCase {
            case: "a mix of attached and unattached names",
            attached: vec!["clb-a", "clb-b"],
            detach: vec!["clb-a", "clb-c"],
            remaining: vec!["clb-a", "clb-b"],
            refusal: Some(
                "Trying to remove Load Balancers that are not part of the group: clb-c"
                    .to_string(),
            ),
        },
    ];

    let (router, _engine) = emulator();
    for name in suite_load_balancers() {
        create_load_balancer(&router, &name).await;
    }

    for (index, case) in cases.iter().enumerate() {
        let group = format!("asg-detach-{index}");
        create_group(&router, &group, &[p("DesiredCapacity", "0")]).await;
        let (status, code, message) =
            load_balancer_call(&router, "AttachLoadBalancers", &group, &case.attached).await;
        assert_eq!(
            status, 200,
            "case {:?}: the attach expected 200, got {status} {code:?} {message:?}",
            case.case
        );

        let (status, code, message) =
            load_balancer_call(&router, "DetachLoadBalancers", &group, &case.detach).await;
        match &case.refusal {
            None => assert_eq!(
                status, 200,
                "case {:?}: DetachLoadBalancers expected 200, got {status} {code:?} {message:?}",
                case.case
            ),
            Some(expected) => {
                assert_eq!(
                    status, 400,
                    "case {:?}: DetachLoadBalancers expected 400, got {status}: {message:?}",
                    case.case
                );
                assert_eq!(
                    code, "ValidationError",
                    "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
                    case.case
                );
                assert_eq!(
                    &message, expected,
                    "case {:?}: expected the message {expected:?}, got {message:?}",
                    case.case
                );
            }
        }

        let held = attached_names(&router, &group).await;
        assert_eq!(
            held, case.remaining,
            "case {:?}: expected the attachments {:?}, got {held:?}",
            case.case, case.remaining
        );
    }

    // A detach addressed at a group the account does not hold is refused with the wording
    // this service's group-scoped mutations answer with — the same one the attach answers.
    let (status, code, message) =
        load_balancer_call(&router, "DetachLoadBalancers", "asg-never-created", &["clb-a"]).await;
    assert_eq!(
        status, 400,
        "case \"a group the account does not hold\": expected 400, got {status} {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "case \"a group the account does not hold\": expected the ValidationError code, got \
         {code:?} {message:?}"
    );
    assert_eq!(
        message, "AutoScalingGroup name not found - null",
        "case \"a group the account does not hold\": expected the detail-less refusal, got \
         {message:?}"
    );
}

/// The group a refusal row addresses whose DELETE has already been taken.
const DELETED_GROUP: &str = "asg-deleted";

/// One refusal case: what the attach names, and the message it is refused with.
struct RefusalCase {
    case: &'static str,
    /// The group the call addresses, and `None` for the one this suite creates.
    group: Option<&'static str>,
    names: Vec<String>,
    message: String,
}

/// An attach is refused WHOLE: nothing is attached, and the group is left exactly as it was.
#[tokio::test]
async fn an_attach_outside_the_rules_attaches_nothing() {
    let too_many: Vec<String> = (1..=MAX_LOAD_BALANCERS_PER_CALL + 1)
        .map(|index| format!("clb-bulk-{index}"))
        .collect();
    let cases = vec![
        RefusalCase {
            case: "one more than the documented per-call bound",
            group: None,
            names: too_many,
            message: format!(
                "Trying to update too many Load Balancers at once. The limit is \
                 {MAX_LOAD_BALANCERS_PER_CALL}"
            ),
        },
        RefusalCase {
            case: "a name no load balancer of the account answers to",
            group: None,
            names: vec!["clb-absent".to_string()],
            message: "Provided Load Balancers may not be valid. Please ensure they exist and \
                      try again."
                .to_string(),
        },
        RefusalCase {
            case: "a mix of one that exists and one that does not",
            group: None,
            names: vec!["clb-a".to_string(), "clb-absent".to_string()],
            message: "Provided Load Balancers may not be valid. Please ensure they exist and \
                      try again."
                .to_string(),
        },
        RefusalCase {
            case: "a group the account does not hold",
            group: Some("asg-never-created"),
            names: vec!["clb-a".to_string()],
            message: "AutoScalingGroup name not found - null".to_string(),
        },
        RefusalCase {
            case: "a group this account has deleted",
            group: Some(DELETED_GROUP),
            names: vec!["clb-a".to_string()],
            message: "AutoScalingGroup name not found - null".to_string(),
        },
    ];

    let (router, _engine) = emulator();
    for name in suite_load_balancers() {
        create_load_balancer(&router, &name).await;
    }
    let group = "asg-refusals";
    create_group(&router, group, &[p("DesiredCapacity", "0")]).await;
    // The group one row addresses is one whose DELETE has been taken: the name answers to no
    // group from that moment on, which is what the refusal reports.
    create_group(&router, DELETED_GROUP, &[p("DesiredCapacity", "0")]).await;
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", DELETED_GROUP)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DeleteAutoScalingGroup expected 200, got {status}: {body}"
    );

    for case in &cases {
        let addressed = case.group.unwrap_or(group);
        let names: Vec<&str> = case.names.iter().map(String::as_str).collect();
        let (status, code, message) =
            load_balancer_call(&router, "AttachLoadBalancers", addressed, &names).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status} {code:?} {message:?}",
            case.case
        );
        assert_eq!(
            code, "ValidationError",
            "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
            case.case
        );
        assert_eq!(
            message, case.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            case.case, case.message
        );
        let held = attached_names(&router, group).await;
        assert!(
            held.is_empty(),
            "case {:?}: a refused attach must leave the group attached to nothing, got {held:?}",
            case.case
        );
    }
}

/// The instances of an attached group ARE registered instances of every load balancer it is
/// attached to, in Elastic Load Balancing's own store — and a scale-in, and a detach, take
/// those registrations out again while a registration a CALLER made is left alone.
#[tokio::test]
async fn the_group_instances_are_registered_with_every_attached_load_balancer() {
    let (router, _engine) = emulator();
    for name in ["clb-a", "clb-b"] {
        create_load_balancer(&router, name).await;
    }
    // A registration a CALLER of Elastic Load Balancing made itself, which no reconcile of
    // this service's own registrations may touch.
    let caller_instance = "i-0caller0caller000";
    let (status, body) = elb_query(
        &router,
        "RegisterInstancesWithLoadBalancer",
        &[
            p("LoadBalancerName", "clb-a"),
            p("Instances.member.1.InstanceId", caller_instance),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "RegisterInstancesWithLoadBalancer expected 200, got {status}: {body}"
    );

    let group = "asg-registered";
    create_group(&router, group, &[p("DesiredCapacity", "2")]).await;
    let instances = settled_instances(&router, "two instances", group, 2).await;

    let (status, code, message) =
        load_balancer_call(&router, "AttachLoadBalancers", group, &["clb-a", "clb-b"]).await;
    assert_eq!(
        status, 200,
        "the attach expected 200, got {status} {code:?} {message:?}"
    );

    // Every instance of the group is registered with BOTH load balancers, beside the
    // caller's own registration.
    let mut with_caller = instances.clone();
    with_caller.push(caller_instance.to_string());
    settled_registrations(&router, "both attached", "clb-a", &with_caller).await;
    settled_registrations(&router, "both attached", "clb-b", &instances).await;
    settled_attachment_state(&router, "both attached", group, "clb-a", ADDED).await;

    // A scale-in takes the instances it terminates out of the load balancers they were
    // registered with, and leaves the caller's registration where it is.
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "0")],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetDesiredCapacity expected 200, got {status}: {body}"
    );
    settled_instances(&router, "scaled in", group, 0).await;
    settled_registrations(
        &router,
        "scaled in",
        "clb-a",
        &[caller_instance.to_string()],
    )
    .await;
    settled_registrations(&router, "scaled in", "clb-b", &[]).await;

    // A detach takes the load balancer off the group, so the group no longer reports it.
    let (status, code, message) =
        load_balancer_call(&router, "DetachLoadBalancers", group, &["clb-a", "clb-b"]).await;
    assert_eq!(
        status, 200,
        "the detach expected 200, got {status} {code:?} {message:?}"
    );
    let held = attached_names(&router, group).await;
    assert!(
        held.is_empty(),
        "a detach of every attachment must leave the group attached to nothing, got {held:?}"
    );
    settled_registrations(
        &router,
        "detached",
        "clb-a",
        &[caller_instance.to_string()],
    )
    .await;
}

/// The state an attachment is reported in follows the registrations it owes and the health
/// checks the load balancer runs: `InService` only where Elastic Load Balancing health checks
/// are enabled for the group and one of its registrations has passed one.
#[tokio::test]
async fn the_attachment_state_follows_the_health_check_type() {
    // One row per `HealthCheckType` the group may carry, and the state its attachment must
    // then settle in: the member admits `EC2`, `ELB` and `ELB,EC2`
    // (`smithy_query(query="doc",shape="CreateAutoScalingGroupType",member="HealthCheckType")`).
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        ("elb health checks", "ELB", "clb-elb", IN_SERVICE),
        ("both health check types", "ELB,EC2", "clb-both", IN_SERVICE),
        ("ec2 health checks only", "EC2", "clb-ec2", ADDED),
    ];

    let (router, _engine) = emulator();
    for (_, _, name, _) in &cases {
        create_load_balancer(&router, name).await;
    }

    for (index, (case, health_check_type, name, expected)) in cases.iter().enumerate() {
        let group = format!("asg-state-{index}");
        create_group(
            &router,
            &group,
            &[
                p("DesiredCapacity", "1"),
                p("HealthCheckType", *health_check_type),
                p("HealthCheckGracePeriod", "0"),
            ],
        )
        .await;
        let instances = settled_instances(&router, case, &group, 1).await;
        let (status, code, message) =
            load_balancer_call(&router, "AttachLoadBalancers", &group, &[name]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the attach expected 200, got {status} {code:?} {message:?}"
        );
        settled_registrations(&router, case, name, &instances).await;

        if *expected == ADDED {
            // The condition this row turns on is Elastic Load Balancing's own verdict, so the
            // wait is for THAT — never for a span of time: once the registration has passed
            // its health checks, a group whose health checks are not Elastic Load Balancing's
            // still reports the attachment as `Added`.
            let deadline = Instant::now() + poll_budget();
            loop {
                let health = reported_health(&router, name, &instances[0]).await;
                if health == HEALTH_IN_SERVICE || Instant::now() >= deadline {
                    assert_eq!(
                        health, HEALTH_IN_SERVICE,
                        "case {case:?}: expected Elastic Load Balancing to report \
                         {HEALTH_IN_SERVICE:?} for {:?}, got {health:?}",
                        instances[0]
                    );
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        settled_attachment_state(&router, case, &group, name, expected).await;
    }
}
