// SPDX-License-Identifier: CC0-1.0

//! `DescribeLoadBalancerTargetGroups`: the Elastic Load Balancing target groups an Auto
//! Scaling group is attached to, and the STATE each of those attachments is in.
//!
//! The rule under test: a read of a group's load-balancer attachments answers exactly the
//! target groups the group holds an attachment to, in the order it holds them, each in the
//! state its REGISTRATIONS put it in — `Adding` while an instance of the group is not
//! registered with that target group yet, `Added` once every one of them is ("all Auto
//! Scaling instances are registered with the target group", which a group holding no
//! instance satisfies with none), and `InService` once Elastic Load Balancing health checks
//! are enabled for the group and one of those registrations has passed one ("at least one
//! Auto Scaling instance passed an `ELB` health check",
//! `smithy_query(query="doc",shape="LoadBalancerTargetGroupState",member="State")`). The
//! state is therefore derived from the two services' own records on every read, never stored
//! beside the attachment, and the read moves nothing: a second read of the same instant
//! answers the same thing.
//!
//! The tables below span what the rule generalises over: how many instances the group holds,
//! how many target groups it is attached to, whether a load balancer forwards to the target
//! group (so its health checks run at all), which `HealthCheckType` the group uses, whether
//! the target group still exists in the service that owns it, the page size asked for, and
//! the ways a request can be unusable.
//!
//! The registrations themselves are asserted where a client sees them — Elastic Load
//! Balancing v2's own `DescribeTargetHealth`, over its own wire — with all three services
//! (Auto Scaling, Amazon EC2 and Elastic Load Balancing v2) mounted on ONE engine, so the
//! state this read reports follows records the owning services return from their own reads.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10 as elbv2_service;

use crate::support::{
    borrowed, ec2_query, form_encode, p, query, seed_subnets, tag_value, tag_values, xml_unescape,
    SeededSubnet, REGION,
};

/// `DescribeTrafficSources`, in its own file beside this one: the reader of the OTHER member
/// that carries the same association — a client must not read two states for one attachment —
/// and this suite's sibling concern, driven through the same harness.
///
/// Declared here rather than in `tests/all.rs`, which declares the suites directly under
/// `tests/all/`: a file deeper than that is declared by its own parent module, which this
/// suite is. The `#[path]` is relative to this file's own directory, and is needed because
/// `tests/all.rs` pulled this file in with a `#[path]` of its own.
#[path = "describe_load_balancer_target_groups/traffic_sources.rs"]
mod traffic_sources;

/// The API version an Elastic Load Balancing v2 client puts in its awsQuery body, read from
/// that service's own statement of it rather than written out here.
const ELBV2_VERSION: &str = elbv2_service::service::shared_endpoint::API_VERSION;

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The three `LoadBalancerTargetGroupState$State` values an attachment this service holds can
/// be reported in, from the model's own catalog for the member
/// (`smithy_query(query="doc",shape="LoadBalancerTargetGroupState",member="State")`).
const STATE_ADDING: &str = "Adding";
const STATE_ADDED: &str = "Added";
const STATE_IN_SERVICE: &str = "InService";

/// The `TargetHealthStateEnum` value Elastic Load Balancing v2 reports for a registration a
/// health check has passed (`smithy_query(query="shape",shape="TargetHealthStateEnum")`).
const TARGET_HEALTHY: &str = "healthy";

/// The `HealthCheckType` values this suite creates groups with, from the catalog the service
/// states for the member (`autoscaling::state::HEALTH_CHECK_TYPES`): `EC2` is the default and
/// `ELB` is what makes the group's health checks Elastic Load Balancing's own.
const HEALTH_CHECK_EC2: &str = "EC2";
const HEALTH_CHECK_ELB: &str = "ELB";

/// One emulator with the three services on one engine.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(elbv2_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// POST one awsQuery action to Elastic Load Balancing v2, the way a client does, and answer
/// its status and body.
async fn elbv2_query(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={ELBV2_VERSION}");
    for (key, value) in params {
        body.push('&');
        body.push_str(key);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://elasticloadbalancingv2.{REGION}.amazonaws.com/"
        ))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}\
                 /elasticloadbalancingv2/aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// The VPC the seeded subnets are in, read back through Amazon EC2's own `DescribeSubnets`.
async fn vpc_of(router: &MultiServiceRouter, subnet: &SeededSubnet) -> String {
    let (status, body) = ec2_query(router, "DescribeSubnets", &[p("SubnetId.1", &subnet.id)]).await;
    assert_eq!(
        status, 200,
        "DescribeSubnets for {:?} expected 200, got {status}: {body}",
        subnet.id
    );
    let vpc_id = tag_value(&body, "vpcId");
    assert!(
        !vpc_id.is_empty(),
        "DescribeSubnets must report the subnet's VPC, got {body}"
    );
    vpc_id
}

/// Create one Elastic Load Balancing v2 target group over the wire and answer the ARN that
/// service minted for it.
async fn create_target_group(
    router: &MultiServiceRouter,
    name: &str,
    port: i32,
    vpc_id: &str,
) -> String {
    let (status, body) = elbv2_query(
        router,
        "CreateTargetGroup",
        &[
            p("Name", name),
            p("Protocol", "HTTP"),
            p("Port", port.to_string()),
            p("VpcId", vpc_id),
            p("TargetType", TARGET_TYPE_INSTANCE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateTargetGroup for {name:?} expected 200, got {status}: {body}"
    );
    let arn = tag_value(&body, "TargetGroupArn");
    assert!(
        !arn.is_empty(),
        "CreateTargetGroup must report the target group's ARN, got {body}"
    );
    arn
}

/// Delete one target group through Elastic Load Balancing v2's own wire — the service that
/// owns it is what takes it away, so the attachment the group still holds is left naming a
/// target group that no longer exists.
async fn delete_target_group(router: &MultiServiceRouter, arn: &str) {
    let (status, body) = elbv2_query(router, "DeleteTargetGroup", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DeleteTargetGroup for {arn:?} expected 200, got {status}: {body}"
    );
}

/// Put a load balancer in front of `target_group_arn`, so Elastic Load Balancing v2 is
/// forwarding traffic to it and therefore health-checking its targets: a target group nothing
/// forwards to has nothing checking its registrations at all.
async fn forward_to(
    router: &MultiServiceRouter,
    name: &str,
    subnets: &[SeededSubnet],
    target_group_arn: &str,
) {
    // INTERNAL, because the VPC these subnets are in has no internet gateway attached: an
    // internet-facing load balancer — the documented default — can only be placed in a VPC
    // that has one, and what this suite needs is a load balancer forwarding to the group.
    let mut params = vec![p("Name", name), p("Scheme", "internal")];
    for (index, subnet) in subnets.iter().enumerate() {
        params.push(p(
            format!("Subnets.member.{}", index + 1).as_str(),
            subnet.id.clone(),
        ));
    }
    let (status, body) = elbv2_query(router, "CreateLoadBalancer", &params).await;
    assert_eq!(
        status, 200,
        "CreateLoadBalancer for {name:?} expected 200, got {status}: {body}"
    );
    let load_balancer_arn = tag_value(&body, "LoadBalancerArn");
    assert!(
        !load_balancer_arn.is_empty(),
        "CreateLoadBalancer must report the load balancer's ARN, got {body}"
    );
    let (status, body) = elbv2_query(
        router,
        "CreateListener",
        &[
            p("LoadBalancerArn", &load_balancer_arn),
            p("Protocol", "HTTP"),
            p("Port", "80"),
            p("DefaultActions.member.1.Type", "forward"),
            p("DefaultActions.member.1.TargetGroupArn", target_group_arn),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateListener in front of {target_group_arn:?} expected 200, got {status}: {body}"
    );
}

/// Create one Amazon EC2 launch template over the wire, so the group launches from a template
/// the account actually holds.
async fn create_launch_template(router: &MultiServiceRouter, name: &str) {
    let (status, body) = ec2_query(
        router,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", name),
            p("LaunchTemplateData.ImageId", "ami-0123456789abcdef0"),
            p("LaunchTemplateData.InstanceType", "t3.micro"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateLaunchTemplate for {name:?} expected 200, got {status}: {body}"
    );
}

/// Create one group over the awsQuery wire: placed by `VPCZoneIdentifier` over `subnets`,
/// holding `desired` instances, created attached to `target_groups` and health-checked the
/// way `health_check_type` says.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    desired: usize,
    target_groups: &[String],
    health_check_type: &str,
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", ids.join(",")),
        p("HealthCheckType", health_check_type),
    ];
    for (index, arn) in target_groups.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Post one `AttachLoadBalancerTargetGroups` and answer its status.
async fn attach(router: &MultiServiceRouter, name: &str, arns: &[String]) -> u16 {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, _) = query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    status
}

/// Post one `DetachLoadBalancerTargetGroups` and answer its status.
async fn detach(router: &MultiServiceRouter, name: &str, arns: &[String]) -> u16 {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, _) = query(router, "DetachLoadBalancerTargetGroups", &borrowed(&params)).await;
    status
}

/// One attachment as a client reads it off the response: the target group and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAttachment {
    arn: String,
    state: String,
}

/// Post one `DescribeLoadBalancerTargetGroups` and answer its status and body.
async fn describe(
    router: &MultiServiceRouter,
    name: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    params.extend(extra.iter().cloned());
    query(
        router,
        "DescribeLoadBalancerTargetGroups",
        &borrowed(&params),
    )
    .await
}

/// The attachments the group reports, in the order reported, plus the `NextToken` the page
/// carries (empty when the response holds none).
async fn attachments_page(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &[(String, String)],
) -> (Vec<ReportedAttachment>, String) {
    let (status, body) = describe(router, name, extra).await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the attachments of {name:?} expected 200, got {status}: {body}"
    );
    let reported = tag_values(&body, "member")
        .into_iter()
        .map(|member| ReportedAttachment {
            arn: tag_value(&member, "LoadBalancerTargetGroupARN"),
            state: tag_value(&member, "State"),
        })
        .collect();
    (reported, tag_value(&body, "NextToken"))
}

/// The attachments of one unpaged read.
async fn attachments(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<ReportedAttachment> {
    attachments_page(router, case, name, &[]).await.0
}

/// How often a poll re-reads a group or a target group.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the lifecycle span this service owes a
/// transition, with ample margin for a loaded test machine. Read from the service's own span
/// rather than written out, so the two cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION + Duration::from_secs(20)
}

/// Read the group's attachments until every one of them is reported in `wanted`, or a
/// generous deadline passes — the CONDITION, never a fixed wait. Answers what it last read.
async fn settled_attachments(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Vec<ReportedAttachment> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = attachments(router, case, name).await;
        let settled = !held.is_empty() && held.iter().all(|entry| entry.state == wanted);
        if settled || Instant::now() >= deadline {
            for entry in &held {
                assert_eq!(
                    entry.state, wanted,
                    "case {case:?}: expected {:?} of {name:?} to reach the state {wanted:?}, \
                     got {held:?}",
                    entry.arn
                );
            }
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many of a target group's registrations Elastic Load Balancing v2 reports as healthy,
/// read through its own `DescribeTargetHealth`.
async fn healthy_targets(router: &MultiServiceRouter, arn: &str) -> usize {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "State")
        .into_iter()
        .filter(|state| state == TARGET_HEALTHY)
        .count()
}

/// Wait until Elastic Load Balancing v2 reports at least one healthy registration in `arn`,
/// or a generous deadline passes: the health check that decides `InService` is that service's
/// own, so the condition is read there.
async fn wait_for_a_healthy_target(router: &MultiServiceRouter, case: &str, arn: &str) {
    let deadline = Instant::now() + poll_budget();
    loop {
        let healthy = healthy_targets(router, arn).await;
        if healthy > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {arn:?} to report a healthy target, got {healthy}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read the group until it holds `wanted` instances, or a generous deadline passes.
async fn settled_instance_count(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read-back of {name:?} expected 200, got {status}: {body}"
        );
        let held = tag_values(&body, "InstanceId").len();
        if held == wanted || Instant::now() >= deadline {
            assert_eq!(
                held, wanted,
                "case {case:?}: expected {name:?} to hold {wanted} instance(s), got {body}"
            );
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the state table: how many instances the group holds, how many target groups it
/// is attached to, whether a load balancer forwards to them, the group's `HealthCheckType`,
/// whether the target groups are deleted out of band afterwards, and the state every
/// attachment must then be reported in.
struct StateRow {
    case: &'static str,
    desired: usize,
    target_groups: usize,
    forwarded: bool,
    health_check_type: &'static str,
    deleted: bool,
    expected: &'static str,
}

fn state_rows() -> Vec<StateRow> {
    vec![
        StateRow {
            case: "a group that holds no instance owes no registration",
            desired: 0,
            target_groups: 1,
            forwarded: false,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: false,
            expected: STATE_ADDED,
        },
        StateRow {
            case: "one instance registered with one target group",
            desired: 1,
            target_groups: 1,
            forwarded: false,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: false,
            expected: STATE_ADDED,
        },
        StateRow {
            case: "three instances registered with two target groups",
            desired: 3,
            target_groups: 2,
            forwarded: false,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: false,
            expected: STATE_ADDED,
        },
        StateRow {
            case: "a load balancer forwarding, with the group on EC2 health checks",
            desired: 1,
            target_groups: 1,
            forwarded: true,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: false,
            expected: STATE_ADDED,
        },
        StateRow {
            case: "a load balancer forwarding, with the group on ELB health checks",
            desired: 2,
            target_groups: 1,
            forwarded: true,
            health_check_type: HEALTH_CHECK_ELB,
            deleted: false,
            expected: STATE_IN_SERVICE,
        },
        StateRow {
            case: "an attachment whose target group has been deleted out of band",
            desired: 2,
            target_groups: 1,
            forwarded: false,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: true,
            expected: STATE_ADDING,
        },
        StateRow {
            case: "two deleted target groups of a group that holds no instance",
            desired: 0,
            target_groups: 2,
            forwarded: false,
            health_check_type: HEALTH_CHECK_EC2,
            deleted: true,
            expected: STATE_ADDED,
        },
    ]
}

/// Every row: each attachment is reported once, in attachment order, in the state its
/// registrations put it in — and reading it again answers the same thing, because the read
/// moves nothing.
#[tokio::test]
async fn every_attachment_is_reported_in_the_state_its_registrations_put_it_in() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-attachment-state").await;

    for (row, case) in state_rows().into_iter().enumerate() {
        let name = format!("asg-attachment-state-{row}");
        let mut arns = Vec::new();
        for index in 0..case.target_groups {
            arns.push(create_target_group(&router, &format!("tg-s{row}-{index}"), 80, &vpc_id).await);
        }
        create_group(
            &router,
            &name,
            "lt-attachment-state",
            &subnets,
            case.desired,
            &arns,
            case.health_check_type,
        )
        .await;
        settled_instance_count(&router, case.case, &name, case.desired).await;
        if case.forwarded {
            forward_to(&router, &format!("lb-s{row}"), &subnets, &arns[0]).await;
            // The health check that decides `InService` is Elastic Load Balancing v2's own,
            // so the row waits on THAT service reporting a healthy registration rather than
            // on any span of its own — and the row that expects `Added` waits on the same
            // condition, so it asserts the state of a group whose target IS healthy.
            wait_for_a_healthy_target(&router, case.case, &arns[0]).await;
        }
        if case.deleted {
            for arn in &arns {
                delete_target_group(&router, arn).await;
            }
        }

        let reported = settled_attachments(&router, case.case, &name, case.expected).await;
        let expected: Vec<ReportedAttachment> = arns
            .iter()
            .map(|arn| ReportedAttachment {
                arn: arn.clone(),
                state: case.expected.to_string(),
            })
            .collect();
        assert_eq!(
            reported, expected,
            "case {:?}: expected the attachments {expected:?}, got {reported:?}",
            case.case
        );
        // A read changes nothing a later caller sees: the same question asked twice answers
        // the same thing.
        let again = attachments(&router, case.case, &name).await;
        assert_eq!(
            again, reported,
            "case {:?}: a second read must answer the same attachments {reported:?}, got \
             {again:?}",
            case.case
        );
    }
}

/// The page sizes this suite walks the attachments with: absent (the operation's own default),
/// and every size from one item to more than the group holds.
fn page_sizes() -> Vec<(&'static str, Option<i32>)> {
    vec![
        ("no MaxRecords at all", None),
        ("one attachment per page", Some(1)),
        ("two attachments per page", Some(2)),
        ("exactly as many as the group holds", Some(3)),
        ("more than the group holds", Some(100)),
    ]
}

/// Every page size: walking the pages to the end yields every attachment exactly once, in
/// attachment order, and the last page carries no `NextToken`.
#[tokio::test]
async fn the_attachments_are_paged_by_max_records() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-attachment-pages").await;

    let name = "asg-attachment-pages";
    let mut arns = Vec::new();
    for index in 0..3 {
        arns.push(create_target_group(&router, &format!("tg-p{index}"), 80, &vpc_id).await);
    }
    create_group(
        &router,
        name,
        "lt-attachment-pages",
        &subnets,
        1,
        &arns[..1],
        HEALTH_CHECK_EC2,
    )
    .await;
    // The other two attachments arrive through the attach, so the order under test is the
    // order the two writers put them in rather than one write's own.
    assert_eq!(
        attach(&router, name, &arns[1..]).await,
        200,
        "attaching the remaining target groups to {name:?} must succeed"
    );

    for (case, max_records) in page_sizes() {
        let mut walked: Vec<ReportedAttachment> = Vec::new();
        let mut token = String::new();
        let mut pages = 0;
        loop {
            let mut extra = Vec::new();
            if let Some(max_records) = max_records {
                extra.push(p("MaxRecords", max_records.to_string()));
            }
            if !token.is_empty() {
                extra.push(p("NextToken", token.clone()));
            }
            let (page, next) = attachments_page(&router, case, name, &extra).await;
            pages += 1;
            assert!(
                pages <= arns.len() + 1,
                "case {case:?}: paging must terminate, walked {pages} page(s) of {walked:?}"
            );
            // The page this request asked for, held to the documented ceiling: "the default
            // value is 100 and the maximum value is 100"
            // (`DescribeLoadBalancerTargetGroupsRequest$MaxRecords`).
            let limit = usize::try_from(max_records.unwrap_or(100).min(100))
                .expect("a page size the request could carry");
            assert!(
                page.len() <= limit,
                "case {case:?}: a page must hold at most {limit} attachment(s), got {page:?}"
            );
            walked.extend(page);
            if next.is_empty() {
                break;
            }
            token = next;
        }
        let expected: Vec<ReportedAttachment> = arns
            .iter()
            .map(|arn| ReportedAttachment {
                arn: arn.clone(),
                state: STATE_ADDED.to_string(),
            })
            .collect();
        assert_eq!(
            walked, expected,
            "case {case:?}: walking the pages must yield {expected:?}, got {walked:?}"
        );
    }
}

/// One unusable request: the members it carries beyond the group name, and the wire code and
/// message it must be refused with.
struct RefusalRow {
    case: &'static str,
    group: &'static str,
    extra: Vec<(String, String)>,
    status: u16,
    code: &'static str,
    message: String,
}

fn refusal_rows(stored_group: &'static str) -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a group that does not exist",
            group: "asg-that-does-not-exist",
            extra: Vec::new(),
            status: 400,
            code: "ValidationError",
            message: "AutoScalingGroup name not found - Could not find Auto Scaling group \
                      named 'asg-that-does-not-exist'"
                .to_string(),
        },
        RefusalRow {
            case: "a page size of none at all",
            group: "stored",
            extra: vec![p("MaxRecords", "0")],
            status: 400,
            code: "ValidationError",
            message: "1 validation error detected: Value '0' at 'maxRecords' failed to satisfy \
                      constraint: Member must have value greater than or equal to 1"
                .to_string(),
        },
        RefusalRow {
            case: "a page size past the documented maximum",
            group: "stored",
            extra: vec![p("MaxRecords", "101")],
            status: 400,
            code: "ValidationError",
            message: "1 validation error detected: Value '101' at 'maxRecords' failed to \
                      satisfy constraint: Member must have value less than or equal to 100"
                .to_string(),
        },
        RefusalRow {
            case: "a token this service never issued",
            group: "stored",
            extra: vec![p("NextToken", "not-a-token")],
            status: 400,
            code: "ValidationError",
            message: "NextToken: not-a-token".to_string(),
        },
        RefusalRow {
            case: "a token naming a page past the attachments the group holds",
            group: "stored",
            extra: vec![p("NextToken", "next-9")],
            status: 400,
            code: "ValidationError",
            message: "NextToken: next-9".to_string(),
        },
    ]
    .into_iter()
    .map(|row| RefusalRow {
        group: if row.group == "stored" {
            stored_group
        } else {
            row.group
        },
        ..row
    })
    .collect()
}

/// Every row: the request is refused with its own status, wire code and whole message, and
/// nothing about the stored group changes.
#[tokio::test]
async fn an_unusable_request_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-attachment-refusal").await;

    let name = "asg-attachment-refusal";
    let arn = create_target_group(&router, "tg-refusal", 80, &vpc_id).await;
    create_group(
        &router,
        name,
        "lt-attachment-refusal",
        &subnets,
        1,
        std::slice::from_ref(&arn),
        HEALTH_CHECK_EC2,
    )
    .await;

    for row in refusal_rows(name) {
        let (status, body) = describe(&router, row.group, &row.extra).await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected HTTP {}, got {status}: {body}",
            row.case, row.status
        );
        let code = tag_value(&body, "Code");
        assert_eq!(
            code, row.code,
            "case {:?}: expected the {} code, got {code:?}: {body}",
            row.case, row.code
        );
        let message = xml_unescape(&tag_value(&body, "Message"));
        assert_eq!(
            message, row.message,
            "case {:?}: expected the message {:?}, got {message:?}",
            row.case, row.message
        );
    }

    // The refusals left the group's own attachment exactly where it was.
    let reported = attachments(&router, "after the refusals", name).await;
    assert_eq!(
        reported,
        vec![ReportedAttachment {
            arn: arn.clone(),
            state: STATE_ADDED.to_string(),
        }],
        "the refused requests must leave the attachment of {name:?} in place, got {reported:?}"
    );
}

/// An attachment is held ONCE however many calls attach it, and it is gone from this read
/// once it is detached — the attachments reported are the ones the group holds, not a log of
/// the calls that touched them.
#[tokio::test]
async fn an_attachment_is_reported_once_and_leaves_with_the_detach() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-attachment-identity").await;

    let name = "asg-attachment-identity";
    let created = create_target_group(&router, "tg-created-attached", 80, &vpc_id).await;
    let later = create_target_group(&router, "tg-later-attached", 8080, &vpc_id).await;
    create_group(
        &router,
        name,
        "lt-attachment-identity",
        &subnets,
        2,
        std::slice::from_ref(&created),
        HEALTH_CHECK_EC2,
    )
    .await;
    settled_instance_count(&router, "the create's own attachment", name, 2).await;

    // Attaching the target group the create already attached, and the second one, twice
    // each: the group holds each of them once, in the order it first took them.
    for round in 0..2 {
        assert_eq!(
            attach(&router, name, &[created.clone(), later.clone()]).await,
            200,
            "round {round}: re-attaching both target groups to {name:?} must succeed"
        );
    }

    let reported = settled_attachments(&router, "after two re-attaches", name, STATE_ADDED).await;
    let expected = vec![
        ReportedAttachment {
            arn: created.clone(),
            state: STATE_ADDED.to_string(),
        },
        ReportedAttachment {
            arn: later.clone(),
            state: STATE_ADDED.to_string(),
        },
    ];
    assert_eq!(
        reported, expected,
        "expected each target group once, as {expected:?}, got {reported:?}"
    );

    assert_eq!(
        detach(&router, name, std::slice::from_ref(&created)).await,
        200,
        "detaching {created:?} from {name:?} must succeed"
    );
    let reported = attachments(&router, "after the detach", name).await;
    assert_eq!(
        reported,
        vec![ReportedAttachment {
            arn: later.clone(),
            state: STATE_ADDED.to_string(),
        }],
        "a detached target group must be gone from the read, got {reported:?}"
    );

    assert_eq!(
        detach(&router, name, std::slice::from_ref(&later)).await,
        200,
        "detaching {later:?} from {name:?} must succeed"
    );
    let reported = attachments(&router, "after both detaches", name).await;
    assert!(
        reported.is_empty(),
        "a group attached to nothing must report no attachment, got {reported:?}"
    );
}
