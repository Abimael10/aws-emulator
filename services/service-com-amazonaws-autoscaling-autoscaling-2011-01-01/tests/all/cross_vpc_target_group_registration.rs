// SPDX-License-Identifier: CC0-1.0

//! A registration an attached target group will NOT take, recorded where a client reads what
//! Amazon EC2 Auto Scaling did on a group's behalf.
//!
//! The rule under test: a target group a group is attached to is registered with the group's
//! instances, and where the load balancing service refuses one of those registrations the
//! group records a `Failed` scaling activity carrying that service's own refusal — "if an
//! attempt to launch instances failed, the StatusCode value is Failed or Cancelled and the
//! StatusMessage element in the response indicates the cause of the failure"
//! (`DescribeScalingActivities`) — rather than leaving the attachment silently inert. The
//! refusal in question is Elastic Load Balancing v2's own: it takes no target that is not in
//! the VPC its target group is in ("the specified target does not exist, is not in the same
//! VPC as the target group, or has an unsupported instance type",
//! `smithy_query(query="shape",shape="InvalidTargetException")` of that service's model), and
//! the attach itself is ANSWERED, so the activity log is the only place the failure appears.
//!
//! The tables below span what the rule generalises over: whether the target group is in the
//! group's own VPC or in another, how the attachment was made (the create's own
//! `TargetGroupARNs`, a later `AttachLoadBalancerTargetGroups`, a later
//! `AttachTrafficSources` of an `elbv2` traffic source), and how many instances the group
//! holds — one refused registration per instance. Every assertion is made where a client
//! sees it: Auto Scaling's own `DescribeScalingActivities`, and Elastic Load Balancing v2's
//! own `DescribeTargetHealth` for the targets, with the three services on ONE engine.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{
    borrowed, create_launch_template, create_target_group, elbv2_query, emulator, list_members, p,
    query, seed_subnets, tag_value, vpc_of, xml_unescape, SeededSubnet,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TrafficSourceIdentifier$Type` of a traffic source whose `Identifier` is an Elastic
/// Load Balancing v2 target group ARN.
const TRAFFIC_SOURCE_TYPE_ELBV2: &str = "elbv2";

/// The `LifecycleState` an instance that has come into service is reported in.
const LIFECYCLE_IN_SERVICE: &str = "InService";

/// The `ScalingActivityStatusCode` an activity whose step did not succeed is reported in.
const ACTIVITY_FAILED: &str = "Failed";

/// How often a poll looks again, and how long it keeps looking: the CONDITION is waited for,
/// never a fixed time, and the deadline is generous so a loaded machine answers the same.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Read from the service's own launch span rather than written out, so the budget and the
/// transition it waits for cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LAUNCH_TRANSITION + Duration::from_secs(20)
}

/// How the target group under test came to be attached to the group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedAs {
    /// Named by the group's own create, under `TargetGroupARNs`.
    CreateTargetGroupArns,
    /// Attached afterwards, by `AttachLoadBalancerTargetGroups`.
    LegacyAttach,
    /// Attached afterwards, as an `elbv2` traffic source.
    TrafficSourceAttach,
}

/// One case of the table: how the attachment was made, and how many instances the group
/// holds.
#[derive(Debug, Clone, Copy)]
struct RefusedRow {
    case: &'static str,
    attached_as: AttachedAs,
    instances: usize,
}

/// The rows: each of the three ways a client associates a target group with a group, and a
/// group holding more than one instance — the refusal is owed per INSTANCE, so the count is
/// what the rule generalises over beside the form of the attachment.
fn refused_rows() -> Vec<RefusedRow> {
    vec![
        RefusedRow {
            case: "attached by the create's TargetGroupARNs",
            attached_as: AttachedAs::CreateTargetGroupArns,
            instances: 1,
        },
        RefusedRow {
            case: "attached by AttachLoadBalancerTargetGroups",
            attached_as: AttachedAs::LegacyAttach,
            instances: 1,
        },
        RefusedRow {
            case: "attached as an elbv2 traffic source",
            attached_as: AttachedAs::TrafficSourceAttach,
            instances: 1,
        },
        RefusedRow {
            case: "two instances, attached by AttachLoadBalancerTargetGroups",
            attached_as: AttachedAs::LegacyAttach,
            instances: 2,
        },
    ]
}

/// One scaling activity as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedActivity {
    description: String,
    status_code: String,
    status_message: String,
}

/// Every scaling activity the group reports, through `DescribeScalingActivities`.
async fn scaling_activities(router: &MultiServiceRouter, name: &str) -> Vec<ReportedActivity> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeScalingActivities for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .map(|member| ReportedActivity {
            description: xml_unescape(&tag_value(member, "Description")),
            status_code: tag_value(member, "StatusCode"),
            status_message: xml_unescape(&tag_value(member, "StatusMessage")),
        })
        .collect()
}

/// The activities of the group that FAILED, in the order it reports them.
async fn failed_activities(router: &MultiServiceRouter, name: &str) -> Vec<ReportedActivity> {
    scaling_activities(router, name)
        .await
        .into_iter()
        .filter(|activity| activity.status_code == ACTIVITY_FAILED)
        .collect()
}

/// Read the group's failed activities until it reports `wanted` of them, or a generous
/// deadline passes. Answers what it last read.
async fn settled_failures(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<ReportedActivity> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let failed = failed_activities(router, name).await;
        if failed.len() >= wanted || Instant::now() >= deadline {
            assert_eq!(
                failed.len(),
                wanted,
                "case {case:?}: expected {wanted} failed activity(ies) of {name:?}, got {failed:?}"
            );
            return failed;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every instance the group reports, as `(instance id, LifecycleState)`.
async fn instance_states(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    list_members(&body, "Instances")
        .into_iter()
        .map(|member| {
            (
                tag_value(member, "InstanceId"),
                tag_value(member, "LifecycleState"),
            )
        })
        .collect()
}

/// Read the group until it holds `wanted` instances and every one of them is in service, or a
/// generous deadline passes. Answers their ids, sorted.
async fn settled_instance_ids(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = instance_states(router, name).await;
        let settled = held.len() == wanted
            && held
                .iter()
                .all(|(_, state)| state == LIFECYCLE_IN_SERVICE);
        if settled || Instant::now() >= deadline {
            assert_eq!(
                held.len(),
                wanted,
                "case {case:?}: expected {name:?} to hold {wanted} instance(s), got {held:?}"
            );
            for (instance_id, state) in &held {
                assert_eq!(
                    state, LIFECYCLE_IN_SERVICE,
                    "case {case:?}: expected {instance_id} of {name:?} to reach \
                     {LIFECYCLE_IN_SERVICE}, got {held:?}"
                );
            }
            let mut ids: Vec<String> = held.into_iter().map(|(id, _)| id).collect();
            ids.sort();
            return ids;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ids of the targets Elastic Load Balancing v2 reports in `arn`, sorted — read through
/// its own `DescribeTargetHealth`, the service that owns the registration.
async fn registered_ids(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    let mut ids: Vec<String> = list_members(&body, "TargetHealthDescriptions")
        .into_iter()
        .map(|member| tag_value(member, "Id"))
        .collect();
    ids.sort();
    ids
}

/// Read the target group until it registers exactly `wanted`, or a generous deadline passes.
async fn settled_registrations(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    wanted: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = registered_ids(router, arn).await;
        if held == wanted || Instant::now() >= deadline {
            assert_eq!(
                held, wanted,
                "case {case:?}: expected {arn:?} to hold {wanted:?}, got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one group over the awsQuery wire: placed by `VPCZoneIdentifier` over `subnets`,
/// holding `desired` instances, attached to `target_groups` under `TargetGroupARNs`.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    desired: usize,
    target_groups: &[String],
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "8"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", ids.join(",")),
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

/// Attach one target group through the API `attached_as` names, and answer the call's status.
async fn attach(
    router: &MultiServiceRouter,
    name: &str,
    arn: &str,
    attached_as: AttachedAs,
) -> (u16, String) {
    let (action, params) = match attached_as {
        AttachedAs::TrafficSourceAttach => (
            "AttachTrafficSources",
            vec![
                p("AutoScalingGroupName", name),
                p("TrafficSources.member.1.Identifier", arn),
                p("TrafficSources.member.1.Type", TRAFFIC_SOURCE_TYPE_ELBV2),
            ],
        ),
        _ => (
            "AttachLoadBalancerTargetGroups",
            vec![
                p("AutoScalingGroupName", name),
                p("TargetGroupARNs.member.1", arn),
            ],
        ),
    };
    let (status, body) = query(router, action, &borrowed(&params)).await;
    (status, body)
}

/// Detach one target group through `DetachLoadBalancerTargetGroups`.
async fn detach(router: &MultiServiceRouter, name: &str, arn: &str) -> u16 {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("TargetGroupARNs.member.1", arn),
    ];
    let (status, _) = query(
        router,
        "DetachLoadBalancerTargetGroups",
        &borrowed(&params),
    )
    .await;
    status
}

/// The whole `StatusMessage` the activity of a refused registration carries: Amazon EC2 Auto
/// Scaling's own fixed text for a target group it could not update, the target group it could
/// not update, Elastic Load Balancing v2's own refusal for the target — "the following
/// targets are not in the target group VPC", which is how that service words a registration
/// outside its target group's network — and the service, status and error code an AWS SDK
/// renders beside the message of a failed call to it (`InvalidTargetException`:
/// `awsQueryError` `InvalidTarget`, `httpError` 400).
///
/// Written out HERE rather than taken from the crate's own builder: a test that re-used the
/// implementation's literal would assert nothing.
fn refusal_message(target_group_arn: &str, vpc_id: &str, instance_id: &str) -> String {
    format!(
        "Failed to update target group {target_group_arn}: The following targets are not in \
         the target group VPC '{vpc_id}': '{instance_id}' (Service: ElasticLoadBalancingV2, \
         Status Code: 400, Error Code: InvalidTarget)."
    )
}

/// What that activity DESCRIBES: the step the group set out to make — the registration the
/// `AddToLoadBalancer` process makes, "adds instances to the attached load balancer or target
/// group" — and the reason it did not, named as the status reason the way every failed
/// activity of this service states it.
fn refusal_description(instance_id: &str, status_message: &str) -> String {
    format!(
        "Adding instance {instance_id} to the attached load balancer or target group. Status \
         Reason: {status_message}"
    )
}

/// A target group in a VPC the group's instances are not in takes none of them, and the group
/// records one FAILED activity per refused registration, carrying the refusing service's own
/// sentence; the target group in the group's own VPC keeps every one of them.
#[tokio::test]
async fn a_target_group_in_another_vpc_records_the_refusal_it_answered() {
    for (row, case) in refused_rows().into_iter().enumerate() {
        let (router, engine) = emulator();
        // Two networks of the account: the one the group places its instances in, and an
        // isolated one holding the target group under test.
        let home_subnets = seed_subnets(&engine, 2);
        let foreign_subnets = seed_subnets(&engine, 1);
        let home_vpc = vpc_of(&router, &home_subnets[0]).await;
        let foreign_vpc = vpc_of(&router, &foreign_subnets[0]).await;
        assert_ne!(
            home_vpc, foreign_vpc,
            "case {:?}: the two seeded networks must be different VPCs",
            case.case
        );
        let template = format!("lt-cross-vpc-{row}");
        create_launch_template(&router, &template).await;

        let home = create_target_group(
            &router,
            &format!("tg-cross-vpc-home-{row}"),
            80,
            TARGET_TYPE_INSTANCE,
            &home_vpc,
        )
        .await;
        let foreign = create_target_group(
            &router,
            &format!("tg-cross-vpc-foreign-{row}"),
            80,
            TARGET_TYPE_INSTANCE,
            &foreign_vpc,
        )
        .await;

        let name = format!("asg-cross-vpc-{row}");
        let attached_at_create: Vec<String> = match case.attached_as {
            AttachedAs::CreateTargetGroupArns => vec![home.clone(), foreign.clone()],
            _ => vec![home.clone()],
        };
        create_group(
            &router,
            &name,
            &template,
            &home_subnets,
            case.instances,
            &attached_at_create,
        )
        .await;
        let instances = settled_instance_ids(&router, case.case, &name, case.instances).await;

        if case.attached_as != AttachedAs::CreateTargetGroupArns {
            // The attach itself is ANSWERED: where the group's instances run is not read by
            // it, exactly as the real service answers one naming a target group of another
            // network.
            let (status, body) = attach(&router, &name, &foreign, case.attached_as).await;
            assert_eq!(
                status, 200,
                "case {:?}: attaching {foreign:?} must be answered 200, got {status}: {body}",
                case.case
            );
        }

        // One failed activity per instance the registration was owed for, each carrying the
        // refusal the load balancing service answered for that instance.
        let failed = settled_failures(&router, case.case, &name, case.instances).await;
        for instance_id in &instances {
            let message = refusal_message(&foreign, &foreign_vpc, instance_id);
            let description = refusal_description(instance_id, &message);
            let found = failed
                .iter()
                .find(|activity| activity.status_message == message);
            assert!(
                found.is_some(),
                "case {:?}: expected a failed activity reading {message:?}, got {failed:?}",
                case.case
            );
            let found = found.expect("the activity asserted above");
            assert_eq!(
                found.description, description,
                "case {:?}: expected the activity of {instance_id} to describe \
                 {description:?}, got {:?}",
                case.case, found.description
            );
        }

        // The target group of another network holds NO target, and the one in the group's own
        // network holds every instance.
        let registered = registered_ids(&router, &foreign).await;
        assert!(
            registered.is_empty(),
            "case {:?}: expected {foreign:?} to hold no target, got {registered:?}",
            case.case
        );
        settled_registrations(&router, case.case, &home, &instances).await;

        // The refusal is recorded ONCE however many passes the reconcile makes: a further
        // attach of the same target group is accepted (the attachment is a set) and adds no
        // second activity for the same registration.
        let (status, body) = attach(&router, &name, &foreign, case.attached_as).await;
        assert_eq!(
            status, 200,
            "case {:?}: re-attaching {foreign:?} must be answered 200, got {status}: {body}",
            case.case
        );
        let again = failed_activities(&router, &name).await;
        assert_eq!(
            again.len(),
            failed.len(),
            "case {:?}: expected the refusal to be recorded once per registration, got \
             {again:?}",
            case.case
        );
    }
}

/// A target group in the group's OWN VPC is registered normally and records no failed
/// activity: the refusal belongs to the network mismatch alone.
#[tokio::test]
async fn a_target_group_in_the_groups_own_vpc_records_no_failure() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-same-vpc").await;

    // The rows: the attachment made at the create, and one made by each of the two attach
    // APIs afterwards — every one of them in the group's own network.
    for (row, attached_as) in [
        AttachedAs::CreateTargetGroupArns,
        AttachedAs::LegacyAttach,
        AttachedAs::TrafficSourceAttach,
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("{attached_as:?} in the group's own VPC");
        let arn = create_target_group(
            &router,
            &format!("tg-same-vpc-{row}"),
            80,
            TARGET_TYPE_INSTANCE,
            &vpc_id,
        )
        .await;
        let name = format!("asg-same-vpc-{row}");
        let attached_at_create: Vec<String> = match attached_as {
            AttachedAs::CreateTargetGroupArns => vec![arn.clone()],
            _ => Vec::new(),
        };
        create_group(&router, &name, "lt-same-vpc", &subnets, 1, &attached_at_create).await;
        let instances = settled_instance_ids(&router, &case, &name, 1).await;
        if attached_as != AttachedAs::CreateTargetGroupArns {
            let (status, body) = attach(&router, &name, &arn, attached_as).await;
            assert_eq!(
                status, 200,
                "case {case:?}: attaching {arn:?} must be answered 200, got {status}: {body}"
            );
        }

        settled_registrations(&router, &case, &arn, &instances).await;
        let failed = failed_activities(&router, &name).await;
        assert!(
            failed.is_empty(),
            "case {case:?}: expected no failed activity, got {failed:?}"
        );
    }
}

/// The refusal is remembered only while it STANDS: a client that detaches the target group of
/// another network and attaches it again is told again.
#[tokio::test]
async fn a_reattached_target_group_records_its_refusal_again() {
    let (router, engine) = emulator();
    let home_subnets = seed_subnets(&engine, 1);
    let foreign_subnets = seed_subnets(&engine, 1);
    let home_vpc = vpc_of(&router, &home_subnets[0]).await;
    let foreign_vpc = vpc_of(&router, &foreign_subnets[0]).await;
    create_launch_template(&router, "lt-reattach-vpc").await;

    let home = create_target_group(&router, "tg-reattach-home", 80, TARGET_TYPE_INSTANCE, &home_vpc)
        .await;
    let foreign = create_target_group(
        &router,
        "tg-reattach-foreign",
        80,
        TARGET_TYPE_INSTANCE,
        &foreign_vpc,
    )
    .await;
    let name = "asg-reattach-vpc";
    create_group(
        &router,
        name,
        "lt-reattach-vpc",
        &home_subnets,
        1,
        std::slice::from_ref(&home),
    )
    .await;
    let instances = settled_instance_ids(&router, "re-attach", name, 1).await;
    let message = refusal_message(&foreign, &foreign_vpc, &instances[0]);

    let (status, body) = attach(&router, name, &foreign, AttachedAs::LegacyAttach).await;
    assert_eq!(
        status, 200,
        "attaching {foreign:?} must be answered 200, got {status}: {body}"
    );
    let first = settled_failures(&router, "re-attach", name, 1).await;
    assert_eq!(
        first[0].status_message, message,
        "expected the first refusal to read {message:?}, got {first:?}"
    );

    let status = detach(&router, name, &foreign).await;
    assert_eq!(status, 200, "detaching {foreign:?} must be answered 200");
    let (status, body) = attach(&router, name, &foreign, AttachedAs::LegacyAttach).await;
    assert_eq!(
        status, 200,
        "re-attaching {foreign:?} must be answered 200, got {status}: {body}"
    );
    let second = settled_failures(&router, "re-attach", name, 2).await;
    for activity in &second {
        assert_eq!(
            activity.status_message, message,
            "expected both refusals to read {message:?}, got {second:?}"
        );
    }
}
