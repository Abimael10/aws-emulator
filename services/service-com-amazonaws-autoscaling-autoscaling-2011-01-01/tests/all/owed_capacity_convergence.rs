// SPDX-License-Identifier: CC0-1.0

//! The capacity an Auto Scaling group was left OWING, and the launch it makes as soon as
//! nothing stands in the way of placing it.
//!
//! THE RULE: a group's `DesiredCapacity` is the capacity it MAINTAINS rather than an
//! instruction one call carries out — "Amazon EC2 Auto Scaling launches instances until the
//! desired capacity is totally fulfilled"
//! (`smithy_query(query="doc",shape="LaunchTemplateOverrides",member="WeightedCapacity")`) —
//! so the units a launch COULD NOT PLACE stay owed: the group launches them as soon as
//! whatever stopped the launch is gone, with no further capacity change from the client, and
//! each instance it then launches is registered with what the group is attached to exactly as
//! every other instance of it is. While the obstacle stands the group launches nothing, and a
//! group whose members already provide its capacity launches nothing either.
//!
//! The sibling suite `load_balancer_target_registration.rs` owns the REFUSAL itself — the
//! `Failed` activity whose `StatusMessage` names the target groups a launch could not be
//! registered into, and the launch that does not happen while that attachment stands. What is
//! under test here is what happens AFTERWARDS: the capacity the group is still owed.
//!
//! The rows span what the rule generalises over:
//!
//! * WHAT stopped the launch — an attachment Elastic Load Balancing v2 can take no
//!   registration into (a target group deleted since it was attached, which stops the launch
//!   because registering the instance is part of bringing it into service) and a launch
//!   source the account cannot satisfy (a launch-template version naming an IAM instance
//!   profile the account does not hold);
//! * HOW the client lifts it — detaching the target group that is gone ("when you detach a
//!   target group, it enters the Removing state while deregistering the instances in the
//!   group", `DetachLoadBalancerTargetGroups`), detaching it and attaching a fresh one in its
//!   place, or creating the IAM resource the launch source names;
//! * how large the shortfall is (one unit and two), how many target groups the group is
//!   attached to and how many of them are gone, and how many instances it already holds.
//!
//! A group that has SUSPENDED the process which adds instances is the negative of the same
//! rule, and has its own test below.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_target_group, ec2_query, elbv2_query, emulator_with_iam, iam_query, p, query,
    seed_subnets, tag_values, vpc_of, xml_unescape,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port every target group of this suite is created on, and the image and instance type
/// its launch templates name: fixed, because what the rows differ along is what stopped the
/// launch and what lifted it.
const TARGET_GROUP_PORT: i32 = 80;
const IMAGE_ID: &str = "ami-0123456789abcdef0";
const INSTANCE_TYPE: &str = "t3.micro";

/// How long a row waits for the launch the group owes, and how often it looks: FAR above the
/// span this service owes a lifecycle transition, because the wait is only here so a failure
/// is a failure rather than a hang — the waits below look for their condition, so machine load
/// decides when a row finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// How long a row watches a group that must launch NOTHING: several times over the span a
/// launch this service makes takes to settle, so a launch it could still have made falls
/// inside the window.
fn held_window() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION * 4
}

/// What stopped the row's launch, and what the client does to lift it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Obstacle {
    /// The target groups the group is attached to, the first `gone` of them DELETED in
    /// Elastic Load Balancing v2 since they were attached: a launch cannot be registered
    /// there, so the group launches nothing. Lifted by detaching the ones that are gone,
    /// and — when `attach_fresh` — attaching a target group that exists in their place.
    DeletedTargetGroups {
        attached: usize,
        gone: usize,
        attach_fresh: bool,
    },
    /// A launch-template version naming an IAM instance profile the account does not hold:
    /// the group can launch nothing from it. Lifted by creating that instance profile.
    AbsentInstanceProfile,
}

/// One row of the owed-capacity table.
struct Row {
    case: &'static str,
    obstacle: Obstacle,
    /// How many instances the group holds before the blocked capacity change.
    held: usize,
    /// The capacity it is then asked for, whose difference from `held` it ends up owing.
    desired: usize,
}

/// ROW 1: the only attached target group is gone, and the client detaches it — one unit owed.
#[tokio::test]
async fn one_unit_is_owed_until_the_target_group_that_is_gone_is_detached() {
    owed_capacity_row(
        1,
        Row {
            case: "the only attached target group is gone, and is detached",
            obstacle: Obstacle::DeletedTargetGroups {
                attached: 1,
                gone: 1,
                attach_fresh: false,
            },
            held: 1,
            desired: 2,
        },
    )
    .await;
}

/// ROW 2: the target group that is gone is detached and a FRESH one attached in its place —
/// the remediation a client performs — and the owed unit registers there.
#[tokio::test]
async fn an_owed_unit_registers_in_the_fresh_target_group_that_replaced_the_one_that_is_gone() {
    owed_capacity_row(
        2,
        Row {
            case: "the only attached target group is gone, and is replaced by a fresh one",
            obstacle: Obstacle::DeletedTargetGroups {
                attached: 1,
                gone: 1,
                attach_fresh: true,
            },
            held: 2,
            desired: 3,
        },
    )
    .await;
}

/// ROW 3: ONE of two attached target groups is gone, and TWO units are owed: both are launched
/// and both register in the attachment that survived.
#[tokio::test]
async fn two_owed_units_are_launched_when_one_of_two_attachments_is_detached() {
    owed_capacity_row(
        3,
        Row {
            case: "one of two attached target groups is gone, with two units owed",
            obstacle: Obstacle::DeletedTargetGroups {
                attached: 2,
                gone: 1,
                attach_fresh: false,
            },
            held: 1,
            desired: 3,
        },
    )
    .await;
}

/// ROW 4: what stopped the launch is the LAUNCH SOURCE rather than an attachment — a
/// launch-template version naming an IAM instance profile the account does not hold — and
/// creating that instance profile is what lets the owed unit be placed.
#[tokio::test]
async fn a_unit_owed_by_an_unsatisfiable_launch_source_is_launched_once_it_resolves() {
    owed_capacity_row(
        4,
        Row {
            case: "the launch source names an instance profile the account does not hold",
            obstacle: Obstacle::AbsentInstanceProfile,
            held: 0,
            desired: 1,
        },
    )
    .await;
}

/// EVERY ROW: the capacity a launch could not place stays owed, nothing is launched while the
/// obstacle stands, and the group reaches that capacity — registered in every target group it
/// is still attached to — once the client has lifted it, without being asked for the capacity
/// again.
///
/// One row per test above, each on its own emulator, so one row's group, target groups and IAM
/// records cannot decide another's and no row waits on another's clock.
async fn owed_capacity_row(index: usize, row: Row) {
    {
        let (router, engine) = emulator_with_iam();
        let case = row.case;
        let group = format!("asg-owed-{index}");
        let template = format!("lt-owed-{index}");
        let subnets = seed_subnets(&engine, 2);
        let vpc = vpc_of(&router, &subnets[0]).await;
        let profile = format!("asg-owed-profile-{index}");
        let absent_profile = format!("asg-owed-absent-profile-{index}");

        // The launch source the group launches from, floating on `$Latest` so a later version
        // is what its next launch reads. Its first version names an instance profile the
        // account HOLDS, so every row starts from a source that can produce an instance.
        create_instance_profile(&router, case, &profile).await;
        create_template(
            &router,
            case,
            &template,
            &[p("LaunchTemplateData.IamInstanceProfile.Name", &profile)],
        )
        .await;

        // The target groups the group is attached to, in attachment order.
        let mut attached: Vec<String> = Vec::new();
        if let Obstacle::DeletedTargetGroups { attached: count, .. } = row.obstacle {
            for position in 0..count {
                attached.push(
                    create_target_group(
                        &router,
                        &format!("tg-owed-{index}-{position}"),
                        TARGET_GROUP_PORT,
                        TARGET_TYPE_INSTANCE,
                        &vpc,
                    )
                    .await,
                );
            }
        }

        let subnet_ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
        let mut params = vec![
            p("AutoScalingGroupName", &group),
            p("LaunchTemplate.LaunchTemplateName", &template),
            p("LaunchTemplate.Version", "$Latest"),
            p("MinSize", "0"),
            p("MaxSize", "8"),
            p("DesiredCapacity", row.held.to_string()),
            p("VPCZoneIdentifier", subnet_ids.join(",")),
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

        // The obstacle: the attachments deleted in the load-balancing service, or the
        // launch-template version the group now floats on naming a profile the account does
        // not hold.
        let mut gone: Vec<String> = Vec::new();
        match row.obstacle {
            Obstacle::DeletedTargetGroups { gone: count, .. } => {
                gone = attached.iter().take(count).cloned().collect();
                for arn in &gone {
                    delete_target_group(&router, case, arn).await;
                }
            }
            Obstacle::AbsentInstanceProfile => {
                create_template_version(
                    &router,
                    case,
                    &template,
                    &[p(
                        "LaunchTemplateData.IamInstanceProfile.Name",
                        &absent_profile,
                    )],
                )
                .await;
            }
        }

        // The capacity change the obstacle stops: it is SERVED, and it places nothing.
        let (status, body) = query(
            &router,
            "SetDesiredCapacity",
            &[
                ("AutoScalingGroupName", group.as_str()),
                ("DesiredCapacity", row.desired.to_string().as_str()),
                ("HonorCooldown", "false"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the blocked capacity change must be served, got {status} with \
             {body:?}"
        );
        // Nothing is launched while the obstacle stands — asserted over a RUN of reads, so a
        // launch that arrives late still fails the row.
        let until = Instant::now() + held_window();
        while Instant::now() < until {
            let held = instances(&router, case, &group).await;
            assert_eq!(
                held, before,
                "case {case:?}: the group must launch nothing while the obstacle stands, got \
                 {held:?}"
            );
            tokio::time::sleep(POLL).await;
        }

        // The client's own remediation, and the whole of what it does: it asks for no capacity
        // again.
        match row.obstacle {
            Obstacle::DeletedTargetGroups { attach_fresh, .. } => {
                detach(&router, case, &group, &gone).await;
                if attach_fresh {
                    let fresh = create_target_group(
                        &router,
                        &format!("tg-owed-fresh-{index}"),
                        TARGET_GROUP_PORT,
                        TARGET_TYPE_INSTANCE,
                        &vpc,
                    )
                    .await;
                    attach(&router, case, &group, std::slice::from_ref(&fresh)).await;
                    attached.push(fresh);
                }
            }
            Obstacle::AbsentInstanceProfile => {
                create_instance_profile(&router, case, &absent_profile).await;
            }
        }

        // The capacity the group was owed is launched, and the group then holds exactly the
        // capacity it was configured for: `settled` answers only for that count, so a unit
        // launched twice fails the row rather than passing it.
        let after = settled(&router, case, &group, row.desired).await;

        // And every instance it now holds is a target of every target group it is still
        // attached to: the registration is part of the launch, whichever launch made it.
        let mut expected = after.clone();
        expected.sort();
        for arn in attached.iter().filter(|arn| !gone.contains(arn)) {
            let registered = registered_ids(&router, case, arn).await;
            assert_eq!(
                registered, expected,
                "case {case:?}: the target group {arn:?} must hold every instance of the group, \
                 got {registered:?}"
            );
        }

        // The launch the group made for ITSELF is on the activity log a client reads, as a
        // launch that SUCCEEDED, carrying the whole sentence Amazon EC2 Auto Scaling words a
        // launch answering a shortfall with — the capacities built from THIS row's own numbers:
        // the capacity the group held when it converged, and the capacity it was configured
        // for.
        let wanted = shortfall_cause(row.held, row.desired);
        let reported = activities(&router, case, &group).await;
        let converged: Vec<&(String, String)> = reported
            .iter()
            .filter(|(status_code, cause)| status_code == "Successful" && *cause == wanted)
            .collect();
        assert!(
            !converged.is_empty(),
            "case {case:?}: the launch that met the group's capacity must record a successful \
             activity whose cause reads {wanted:?}, got {reported:?}"
        );
    }
}

/// A group whose `Launch` process is SUSPENDED converges on nothing — that process is what
/// adds instances to a group ("`Launch`: Adds instances to the group",
/// `smithy_query(query="doc",shape="SuspendProcessesType",member="ScalingProcesses")`) — and
/// launches the capacity it is owed once the process is resumed.
#[tokio::test]
async fn a_group_with_launch_suspended_launches_nothing_it_is_owed_until_it_is_resumed() {
    let case = "launch suspended while the group is owed a unit of capacity";
    let (router, engine) = emulator_with_iam();
    let subnets = seed_subnets(&engine, 1);
    let vpc = vpc_of(&router, &subnets[0]).await;
    create_template(&router, case, "lt-owed-suspended", &[]).await;
    let gone = create_target_group(
        &router,
        "tg-owed-suspended",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc,
    )
    .await;

    let group = "asg-owed-suspended";
    let params = vec![
        p("AutoScalingGroupName", group),
        p("LaunchTemplate.LaunchTemplateName", "lt-owed-suspended"),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "1"),
        p("VPCZoneIdentifier", subnets[0].id.clone()),
        p("TargetGroupARNs.member.1", gone.clone()),
    ];
    let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the group must be created, got {status} with {body:?}"
    );
    let before = settled(&router, case, group, 1).await;

    // The group is left owing a unit of capacity by a launch the deleted target group stopped.
    delete_target_group(&router, case, &gone).await;
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", "2"),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the blocked capacity change must be served, got {status} with {body:?}"
    );
    let (status, body) = query(
        &router,
        "SuspendProcesses",
        &[
            ("AutoScalingGroupName", group),
            ("ScalingProcesses.member.1", "Launch"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses must be served, got {status} with {body:?}"
    );

    // The obstacle is gone, but the process that would add the instance is suspended.
    detach(&router, case, group, std::slice::from_ref(&gone)).await;
    let until = Instant::now() + held_window();
    while Instant::now() < until {
        let held = instances(&router, case, group).await;
        assert_eq!(
            held, before,
            "case {case:?}: a group whose Launch process is suspended must launch nothing it is \
             owed, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    let (status, body) = query(
        &router,
        "ResumeProcesses",
        &[
            ("AutoScalingGroupName", group),
            ("ScalingProcesses.member.1", "Launch"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ResumeProcesses must be served, got {status} with {body:?}"
    );
    let after = settled(&router, case, group, 2).await;
    assert_eq!(
        after.len(),
        2,
        "case {case:?}: the resumed group must launch the capacity it was owed, got {after:?}"
    );
}

/// The WHOLE cause a launch that answered a shortfall records, as Amazon EC2 Auto Scaling
/// words that class of activity: the difference it answered, then the capacities it moved the
/// group between — the capacity its members provided when it launched, and the capacity it is
/// configured for. Written out here rather than read off the crate, so a wrong sentence in the
/// implementation cannot make a row pass; the two numbers come from the row's own inputs.
fn shortfall_cause(from: usize, to: usize) -> String {
    format!(
        "an instance was launched in response to a difference between desired and actual \
         capacity, increasing the capacity from {from} to {to}."
    )
}

/// Create one Amazon EC2 launch template over that service's own wire, naming `extra` beside
/// the image and instance type every template here carries.
async fn create_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &[(String, String)],
) {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    params.extend(extra.iter().cloned());
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Add one version to an existing launch template, naming `extra` beside the image and
/// instance type — the version a group floating on `$Latest` launches from next.
async fn create_template_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    extra: &[(String, String)],
) {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    params.extend(extra.iter().cloned());
    let (status, body) = ec2_query(router, "CreateLaunchTemplateVersion", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplateVersion {name:?} must succeed, got {status} with \
         {body:?}"
    );
}

/// Create one IAM instance profile over IAM's own wire, so a launch source naming it resolves.
async fn create_instance_profile(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = iam_query(
        router,
        "CreateInstanceProfile",
        &[p("InstanceProfileName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInstanceProfile {name:?} must succeed, got {status} with {body:?}"
    );
}

/// Delete one target group over the load-balancing service's own wire: the out-of-band
/// deletion that leaves the group attached to a target group that no longer exists.
async fn delete_target_group(router: &MultiServiceRouter, case: &str, arn: &str) {
    let (status, body) =
        elbv2_query(router, "DeleteTargetGroup", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteTargetGroup {arn:?} must succeed, got {status} with {body:?}"
    );
}

/// Detach `arns` from the group, which must be served: the group reports the attachment, so
/// the client that put it there can take it off.
async fn detach(router: &MultiServiceRouter, case: &str, group: &str, arns: &[String]) {
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
    let (status, body) = query(router, "DetachLoadBalancerTargetGroups", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: detaching {arns:?} must be served, got {status} with {body:?}"
    );
}

/// Attach `arns` to the group, which must be served.
async fn attach(router: &MultiServiceRouter, case: &str, group: &str, arns: &[String]) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (position, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", position + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: attaching {arns:?} must be served, got {status} with {body:?}"
    );
}

/// The ids of the instances the group holds, in the order it reports them.
async fn instances(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
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
    tag_values(&body, "InstanceId")
}

/// Look at the group until it holds `count` instances, every one of them `InService`, or
/// [`DEADLINE`] passes; answer their ids.
async fn settled(router: &MultiServiceRouter, case: &str, group: &str, count: usize) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
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
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        if ids.len() == count && states.iter().all(|state| state == "InService") {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must hold {count} instance(s) in service, got {ids:?} in \
             {states:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The ids the target group `arn` holds as targets, in order, as a client reads them off the
/// load-balancing service's own `DescribeTargetHealth`.
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

/// Every activity the group reports, as `(StatusCode, Cause)`.
async fn activities(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities for {group:?} must succeed, got {status} with \
         {body:?}"
    );
    tag_values(&body, "StatusCode")
        .into_iter()
        .zip(
            tag_values(&body, "Cause")
                .iter()
                .map(|cause| xml_unescape(cause))
                .collect::<Vec<String>>(),
        )
        .collect()
}
