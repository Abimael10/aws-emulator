// SPDX-License-Identifier: CC0-1.0

//! A capacity change the group SERVES but cannot place, retried: what the group holds while
//! the obstacle stands, and what it reports for the launch it did not make.
//!
//! THE RULE: registering the instance is part of bringing it into service, so a group one of
//! whose attached target groups Elastic Load Balancing v2 no longer holds places nothing — the
//! capacity change is served, the group goes on holding exactly the instances it already had,
//! and the unit it could not place is recorded as a `Failed` scaling activity whose
//! `StatusMessage` names the target group it could not register into. That is the answer to
//! EVERY attempt while the obstacle stands, so a client that asks again — as a client watching
//! for capacity does — gets the same answer again and reads the group at the same size, and the
//! capacity stays owed until the client repairs the attachment, at which point the group
//! launches it and registers it where it is now attached.
//!
//! The rows span what the rule generalises over: the target group's own protocol and port, how
//! many instances the group already holds, how far the blocked change reaches, how many target
//! groups are attached and how many of them are gone, and how many times the client retries the
//! blocked change.
//!
//! The sibling suite `owed_capacity_convergence` owns the obstacle CLASSES (a deleted target
//! group, a launch source the account cannot satisfy) and the ways a client lifts them;
//! `load_balancer_target_registration` owns the refusal's wording. What is under test here is
//! the group a RETRIED blocked change leaves behind.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, elbv2_query, emulator, p, query, seed_subnets, tag_value,
    tag_values, vpc_of, xml_unescape,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// How long a row waits for the clock-owed moves it depends on, and how often it looks: far
/// above the spans this service owes them, because the deadline is only here so a failure is a
/// failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// How long a row watches a group that must launch NOTHING: long enough to cover both instants
/// this service owes a group of its own accord — the span a lifecycle move takes to settle
/// ([`autoscaling::state::LIFECYCLE_TRANSITION`], several times over) and the instant at which
/// a group that is owed capacity attempts the launch again
/// ([`autoscaling::state::WITHDRAWN_LAUNCH_RETRY`]) — so a launch made on either falls inside
/// the window. The row looks again until it has passed, so machine load decides how many reads
/// it makes rather than whether it passes.
fn watch_window() -> Duration {
    autoscaling::state::WITHDRAWN_LAUNCH_RETRY + autoscaling::state::LIFECYCLE_TRANSITION * 4
}

/// One row of the blocked-change table.
struct Row {
    case: &'static str,
    /// The `Protocol` and `Port` the row's target groups are created with: a target group's
    /// traffic is its own, and an Auto Scaling group registers its instances into it whichever
    /// protocol it carries.
    protocol: &'static str,
    port: i32,
    /// How many target groups the group is attached to, and how many of them are deleted out
    /// from under it.
    attached: usize,
    gone: usize,
    /// How many instances it holds before the blocked change, and the capacity that change
    /// asks for.
    held: i32,
    asked: i32,
    /// How many times the client asks for that capacity while the obstacle stands.
    retries: usize,
}

/// ROW 1: the group's only attached target group — a TCP one — is gone, and the client asks
/// twice for one more instance.
#[tokio::test]
async fn a_blocked_change_retried_keeps_the_group_at_the_capacity_it_holds() {
    blocked_change_holds_the_group(
        1,
        Row {
            case: "the only attached target group, a TCP one, is gone and the change is asked \
                   for twice",
            protocol: "TCP",
            port: 22,
            attached: 1,
            gone: 1,
            held: 1,
            asked: 2,
            retries: 2,
        },
    )
    .await;
}

/// ROW 2: one of TWO attached target groups is gone — the live one is no help — and the group
/// already holds two instances.
#[tokio::test]
async fn a_blocked_change_is_blocked_by_one_gone_attachment_of_two() {
    blocked_change_holds_the_group(
        2,
        Row {
            case: "one of two attached target groups is gone and the group holds two instances",
            protocol: "HTTP",
            port: 80,
            attached: 2,
            gone: 1,
            held: 2,
            asked: 3,
            retries: 3,
        },
    )
    .await;
}

/// Drive one row: the group settles on what it was configured for and registers there, the
/// obstacle is put in its way, every attempt at the capacity change is served and places
/// nothing, and the repair lets the group launch what it was owed.
async fn blocked_change_holds_the_group(index: usize, row: Row) {
    let case = row.case;
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc = vpc_of(&router, &subnets[0]).await;
    let template = format!("lt-blocked-{index}");
    create_launch_template(&router, &template).await;

    let mut attached: Vec<String> = Vec::new();
    for position in 0..row.attached {
        attached.push(
            target_group(
                &router,
                case,
                &format!("tg-blocked-{index}-{position}"),
                row.protocol,
                row.port,
                &vpc,
            )
            .await,
        );
    }

    let group = format!("asg-blocked-{index}");
    let mut params = vec![
        p("AutoScalingGroupName", &group),
        p("LaunchTemplate.LaunchTemplateName", &template),
        p("MinSize", "0"),
        p("MaxSize", "6"),
        p("DesiredCapacity", row.held.to_string()),
        p("VPCZoneIdentifier", subnets[0].id.clone()),
        p("HealthCheckType", "EC2"),
    ];
    for (position, arn) in attached.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", position + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be created, got {status} with {body:?}"
    );
    let before = settled(&router, case, &group, row.held).await;
    let mut registered_before = before.clone();
    registered_before.sort();
    for arn in &attached {
        let registered = registered_ids(&router, case, arn).await;
        assert_eq!(
            registered, registered_before,
            "case {case:?}: the target group {arn:?} must hold the group's instances before \
             anything is deleted, got {registered:?}"
        );
    }

    // The obstacle: the first `gone` attachments are deleted in the service that owns them,
    // out from under the group, which goes on reporting them.
    let gone: Vec<String> = attached.iter().take(row.gone).cloned().collect();
    for arn in &gone {
        let (status, body) =
            elbv2_query(&router, "DeleteTargetGroup", &[p("TargetGroupArn", arn)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: deleting the target group {arn:?} must succeed, got {status} with \
             {body:?}"
        );
    }

    // Every attempt at the capacity change is SERVED, places nothing, and says why.
    for attempt in 1..=row.retries {
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("DesiredCapacity", row.asked.to_string().as_str()),
                ("HonorCooldown", "false"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: attempt {attempt} at the blocked change must be served, got {status} \
             with {body:?}"
        );
        let (held, states, capacity) = group_state(&router, case, &group).await;
        assert_eq!(
            held, before,
            "case {case:?}: attempt {attempt} must leave the group holding the instances it had \
             ({before:?}), got {held:?}"
        );
        assert!(
            states.iter().all(|state| state == "InService"),
            "case {case:?}: attempt {attempt} must leave every instance in service, got {states:?}"
        );
        assert_eq!(
            capacity,
            row.asked.to_string(),
            "case {case:?}: the group records the capacity it was asked for ({}), got \
             {capacity:?}",
            row.asked
        );

        // The failure a client reads off the group's own scaling history, naming every target
        // group the launch could not be registered into.
        let reported = activities(&router, case, &group).await;
        let failed: Vec<&(String, String)> = reported
            .iter()
            .filter(|(status_code, _)| status_code == "Failed")
            .collect();
        assert!(
            !failed.is_empty(),
            "case {case:?}: attempt {attempt} must record a Failed activity, got {reported:?}"
        );
        for arn in &gone {
            assert!(
                failed
                    .iter()
                    .any(|(_, message)| message.contains(arn.as_str())),
                "case {case:?}: a Failed activity must name the target group {arn:?} it could \
                 not register into, got {failed:?}"
            );
        }
    }

    // Nothing is launched while the obstacle stands, asserted over a run of reads rather than
    // one: a launch that arrives late still fails the row.
    let until = Instant::now() + watch_window();
    while Instant::now() < until {
        let (held, _, _) = group_state(&router, case, &group).await;
        assert_eq!(
            held, before,
            "case {case:?}: the group must launch nothing while the obstacle stands, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    // The client's own repair: the attachments that are gone come off and a live one goes on.
    let fresh = target_group(
        &router,
        case,
        &format!("tg-blocked-fresh-{index}"),
        row.protocol,
        row.port,
        &vpc,
    )
    .await;
    attach(
        &router,
        case,
        &group,
        "AttachLoadBalancerTargetGroups",
        std::slice::from_ref(&fresh),
    )
    .await;
    attach(
        &router,
        case,
        &group,
        "DetachLoadBalancerTargetGroups",
        &gone,
    )
    .await;

    // The capacity the blocked change was owed is launched, with no further change asked for,
    // and every instance is a target of every target group the group is still attached to.
    let after = settled(&router, case, &group, row.asked).await;
    let mut expected = after.clone();
    expected.sort();
    let live = attached
        .iter()
        .filter(|arn| !gone.contains(arn))
        .chain(std::iter::once(&fresh));
    for arn in live {
        let registered = registered_ids(&router, case, arn).await;
        assert_eq!(
            registered, expected,
            "case {case:?}: the target group {arn:?} must hold every instance of the group, got \
             {registered:?}"
        );
    }
}

/// Create one target group over Elastic Load Balancing v2's own wire, with the row's own
/// protocol and port.
///
/// The shared helper in `support` creates HTTP target groups only, and the protocol is one of
/// the things these rows differ along, so the create is written here with both members taken
/// from the row.
async fn target_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    protocol: &str,
    port: i32,
    vpc_id: &str,
) -> String {
    let (status, body) = elbv2_query(
        router,
        "CreateTargetGroup",
        &[
            p("Name", name),
            p("TargetType", TARGET_TYPE_INSTANCE),
            p("Protocol", protocol),
            p("Port", port.to_string()),
            p("VpcId", vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTargetGroup {name:?} ({protocol}:{port}) must succeed, got \
         {status} with {body:?}"
    );
    let arn = tag_value(&body, "TargetGroupArn");
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateTargetGroup must report the target group's ARN, got {body:?}"
    );
    arn
}

/// Attach or detach `arns` through `action`, which must be served.
async fn attach(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    action: &str,
    arns: &[String],
) {
    if arns.is_empty() {
        return;
    }
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (position, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", position + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} of {arns:?} must be served, got {status} with {body:?}"
    );
}

/// The instances the group reports, their lifecycle states, and the `DesiredCapacity` it
/// reports.
async fn group_state(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> (Vec<String>, Vec<String>, String) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group {group:?} must be readable, got {status} with {body:?}"
    );
    (
        tag_values(&body, "InstanceId"),
        tag_values(&body, "LifecycleState"),
        tag_value(&body, "DesiredCapacity"),
    )
}

/// Every activity the group reports, as `(StatusCode, StatusMessage)`.
async fn activities(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the activities of {group:?} must be readable, got {status} with {body:?}"
    );
    tag_values(&body, "StatusCode")
        .into_iter()
        .zip(
            tag_values(&body, "StatusMessage")
                .iter()
                .map(|message| xml_unescape(message))
                .collect::<Vec<String>>(),
        )
        .collect()
}

/// The ids the target group `arn` holds as targets, as a client reads them off Elastic Load
/// Balancing v2's own `DescribeTargetHealth`.
async fn registered_ids(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTargetHealth for {arn:?} must succeed, got {status} with {body:?}"
    );
    let mut ids = tag_values(&body, "Id");
    ids.sort();
    ids
}

/// Look at the group until it holds `count` instances, every one of them `InService`, or
/// [`DEADLINE`] passes; answer their ids in the order it reports them.
async fn settled(router: &MultiServiceRouter, case: &str, group: &str, count: i32) -> Vec<String> {
    let wanted = usize::try_from(count).expect("a configured capacity is not negative");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (ids, states, _) = group_state(router, case, group).await;
        if ids.len() == wanted && states.iter().all(|state| state == "InService") {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must hold {wanted} instance(s) in service, got {ids:?} in \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}
