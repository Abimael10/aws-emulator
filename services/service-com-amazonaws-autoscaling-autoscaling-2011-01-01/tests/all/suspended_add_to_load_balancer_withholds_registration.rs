// SPDX-License-Identifier: CC0-1.0

//! What a group with `AddToLoadBalancer` suspended does with the load balancing it is
//! attached to.
//!
//! RULE: `AddToLoadBalancer` is the process that REGISTERS an instance the group brings into
//! service with the load balancers and target groups the group is attached to, so while that
//! process is suspended the group still launches the instance and brings it `InService`
//! (`smithy_query(query="operation",operation="DescribeAutoScalingGroups")` reports it among
//! the group's `Instances`) and registers it with NONE of its attachments — the load
//! balancing service goes on reporting that instance as a target it does not hold
//! (`DescribeTargetHealth` answers `TargetHealth$State` `unused` with `TargetHealth$Reason`
//! `Target.NotRegistered` for it). The registration is owed by the instance coming into
//! service rather than by the attachment, so a resumption registers only the instances that
//! come into service AFTER it; an instance that came into service while the process was
//! suspended stays unregistered. Every other process a group suspends leaves the
//! registration exactly as it was: a suspension stops what the process it names does and
//! nothing else.
//!
//! The model states the suspension — `SuspendProcesses$ScalingProcesses` and
//! `ResumeProcesses$ScalingProcesses` carry the process names, `DescribeAutoScalingGroups`
//! reports `AutoScalingGroup$SuspendedProcesses` — and names `AddToLoadBalancer` in the
//! catalog `DescribeScalingProcessTypes` publishes, but states no registration side effect
//! for it, so WHAT the suspension withholds is recorded behaviour.
//!
//! The table below spans what the rule generalises over: which processes are suspended (the
//! one alone, it among several, none at all, and one that is not it), which form the
//! attachment takes (`TargetGroupARNs`, an `elbv2` traffic source, and a later
//! `AttachLoadBalancerTargetGroups`), how many target groups the group is attached to and on
//! what ports, how many instances come into service, and which request raises the capacity
//! they come into service for. Every row drives a SUBJECT group and a CONTROL group attached
//! to the same target groups through the same raise, so a row that asserts nothing was
//! registered also shows the target groups were registering for the group beside it.
//!
//! Both load balancer kinds a group attaches are asserted — an Elastic Load Balancing v2
//! target group and a Classic Load Balancer — as is the other way an instance comes into
//! service: one the group takes out of its warm pool. Every assertion is read where a client
//! reads it, over each owning service's own wire, with the services on ONE engine. Nothing
//! waits a fixed span: every wait looks again for the condition it needs until a generous
//! deadline passes.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, classic_elb_query, create_classic_load_balancer, create_launch_template,
    create_target_group, elbv2_query, emulator, emulator_with_classic_elb, list_members, p, query,
    seed_subnets, tag_value, tag_values, vpc_of, SeededSubnet, REGION,
};

/// The processes these cases name, as `ProcessType$ProcessName` spells them in the catalog
/// `DescribeScalingProcessTypes` publishes.
const PROCESS_ADD_TO_LOAD_BALANCER: &str = "AddToLoadBalancer";
const PROCESS_HEALTH_CHECK: &str = "HealthCheck";
const PROCESS_AZ_REBALANCE: &str = "AZRebalance";

/// The `LifecycleState` of an instance that has reached service, from the model's own
/// `LifecycleState` enum (`smithy_query(query="shape",shape="LifecycleState")`).
const IN_SERVICE: &str = "InService";

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The `TargetHealth$State` and `TargetHealth$Reason` Elastic Load Balancing v2 reports a
/// target group's own read with for an instance it holds no registration for — "`unused`"
/// and "`Target.NotRegistered` - The target is not registered to the target group"
/// (`smithy_query(query="doc",shape="TargetHealth",member="Reason")`).
const UNUSED_STATE: &str = "unused";
const NOT_REGISTERED_REASON: &str = "Target.NotRegistered";

/// How often a wait looks again for the condition it needs.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a wait keeps looking before it gives up: the lifecycle span this service owes a
/// transition and the one Amazon EC2 owes, with ample margin for a loaded test machine. Read
/// from the services' own spans rather than written out, so they cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION.max(ec2::TRANSITION_DELAY) + Duration::from_secs(60)
}

/// How a group names the target groups it is attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedAs {
    /// `CreateAutoScalingGroup$TargetGroupARNs`.
    TargetGroupArns,
    /// `CreateAutoScalingGroup$TrafficSources`, each entry an `elbv2` traffic source whose
    /// `Identifier` is the target group's ARN.
    Elbv2TrafficSources,
    /// A later `AttachLoadBalancerTargetGroups` rather than the create.
    LaterAttach,
}

/// Which request raises the capacity the instances come into service for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaiseWith {
    SetDesiredCapacity,
    UpdateAutoScalingGroup,
}

impl RaiseWith {
    /// The action a raise posts.
    fn action(self) -> &'static str {
        match self {
            Self::SetDesiredCapacity => "SetDesiredCapacity",
            Self::UpdateAutoScalingGroup => "UpdateAutoScalingGroup",
        }
    }
}

/// One row of the withholding table.
struct Row {
    case: &'static str,
    /// The processes the SUBJECT group suspends before the raise. Empty suspends nothing: the
    /// subject is then a second control.
    suspend: &'static [&'static str],
    attached_as: AttachedAs,
    /// The target groups both groups are attached to, one per port — the port a registration
    /// of an instance target is held at.
    ports: &'static [i32],
    /// The capacity the raise asks each group for, which is how many instances come into
    /// service in it.
    raise_to: usize,
    raise_with: RaiseWith,
    /// Whether the subject's instances are registered with every target group it is attached
    /// to: false only while the process that registers them is suspended.
    registers: bool,
}

/// Every row: a group that has suspended `AddToLoadBalancer` brings its instances into
/// service and registers none of them, while the group beside it registers all of its own.
#[tokio::test]
async fn a_suspended_add_to_load_balancer_withholds_every_registration() {
    let rows = [
        Row {
            case: "the process alone, one target group, raised with SetDesiredCapacity",
            suspend: &[PROCESS_ADD_TO_LOAD_BALANCER],
            attached_as: AttachedAs::TargetGroupArns,
            ports: &[80],
            raise_to: 1,
            raise_with: RaiseWith::SetDesiredCapacity,
            registers: false,
        },
        Row {
            case: "the process among several, two target groups, two instances, raised with \
                   UpdateAutoScalingGroup",
            suspend: &[PROCESS_HEALTH_CHECK, PROCESS_ADD_TO_LOAD_BALANCER],
            attached_as: AttachedAs::TargetGroupArns,
            ports: &[80, 8080],
            raise_to: 2,
            raise_with: RaiseWith::UpdateAutoScalingGroup,
            registers: false,
        },
        Row {
            case: "the process alone, the target group attached as an elbv2 traffic source",
            suspend: &[PROCESS_ADD_TO_LOAD_BALANCER],
            attached_as: AttachedAs::Elbv2TrafficSources,
            ports: &[443],
            raise_to: 1,
            raise_with: RaiseWith::SetDesiredCapacity,
            registers: false,
        },
        Row {
            case: "the process alone, the target group attached by a later attach",
            suspend: &[PROCESS_ADD_TO_LOAD_BALANCER],
            attached_as: AttachedAs::LaterAttach,
            ports: &[8081],
            raise_to: 1,
            raise_with: RaiseWith::SetDesiredCapacity,
            registers: false,
        },
        Row {
            case: "nothing suspended",
            suspend: &[],
            attached_as: AttachedAs::TargetGroupArns,
            ports: &[80],
            raise_to: 1,
            raise_with: RaiseWith::SetDesiredCapacity,
            registers: true,
        },
        Row {
            case: "another process suspended, which withholds nothing",
            suspend: &[PROCESS_AZ_REBALANCE],
            attached_as: AttachedAs::TargetGroupArns,
            ports: &[80, 8080],
            raise_to: 1,
            raise_with: RaiseWith::SetDesiredCapacity,
            registers: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: each row is its own account, so the ids of one row never
        // explain another's.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 1);
        let vpc = vpc_of(&router, &subnets[0]).await;
        let template = format!("lt-withheld-{index}");
        create_launch_template(&router, &template).await;
        let mut target_groups = Vec::new();
        for (slot, port) in row.ports.iter().enumerate() {
            target_groups.push(
                create_target_group(
                    &router,
                    &format!("tg-withheld-{index}-{slot}"),
                    *port,
                    TARGET_TYPE_INSTANCE,
                    &vpc,
                )
                .await,
            );
        }
        let subject = format!("asg-subject-{index}");
        let control = format!("asg-control-{index}");
        for name in [&subject, &control] {
            create_group(&router, name, &template, &subnets, &target_groups, row.attached_as).await;
        }

        let suspended: Vec<String> = row
            .suspend
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        if !suspended.is_empty() {
            let (status, body) = suspend(&router, &subject, &suspended).await;
            assert_eq!(
                status, 200,
                "case {:?}: SuspendProcesses for {subject:?} expected 200, got {status}: {body}",
                row.case
            );
        }
        assert_eq!(
            reported_suspensions(&router, &subject).await,
            sorted(&suspended),
            "case {:?}: expected {subject:?} to report exactly the suspended processes",
            row.case
        );
        assert!(
            reported_suspensions(&router, &control).await.is_empty(),
            "case {:?}: expected {control:?} to report no suspended process",
            row.case
        );

        for name in [&subject, &control] {
            raise(&router, row.case, name, row.raise_to, row.raise_with).await;
        }
        let subject_ids = settled_ids(&router, row.case, &subject, row.raise_to).await;
        let control_ids = settled_ids(&router, row.case, &control, row.raise_to).await;

        // What each target group must hold once the raise has settled: the control's
        // instances always, and the subject's only while the process that registers them is
        // running.
        let mut wanted = control_ids.clone();
        if row.registers {
            wanted.extend(subject_ids.iter().cloned());
        }
        let wanted = sorted(&wanted);
        for arn in &target_groups {
            let held = settled_registrations(&router, arn, &wanted).await;
            assert_eq!(
                held, wanted,
                "case {:?}: expected {arn:?} to hold exactly {wanted:?}, got {held:?}",
                row.case
            );
        }

        // And what the target group answers for each of the subject's instances by id: a
        // registration it holds, or the target it has none for.
        for arn in &target_groups {
            for instance_id in &subject_ids {
                let (state, reason) = target_health(&router, arn, instance_id).await;
                if row.registers {
                    assert_ne!(
                        reason, NOT_REGISTERED_REASON,
                        "case {:?}: expected {instance_id} to be registered with {arn:?}, got \
                         state {state:?} reason {reason:?}",
                        row.case
                    );
                } else {
                    assert_eq!(
                        (state.as_str(), reason.as_str()),
                        (UNUSED_STATE, NOT_REGISTERED_REASON),
                        "case {:?}: expected {arn:?} to report {instance_id} as \
                         {UNUSED_STATE}/{NOT_REGISTERED_REASON}",
                        row.case
                    );
                }
            }
        }
    }
}

/// A resumption registers what comes into service after it, and leaves the instance that came
/// into service while the process was suspended unregistered.
#[tokio::test]
async fn resuming_the_process_registers_only_what_comes_into_service_after_it() {
    let case = "a resumption after one withheld launch";
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc = vpc_of(&router, &subnets[0]).await;
    let template = "lt-resumed-registration";
    create_launch_template(&router, template).await;
    let arn = create_target_group(&router, "tg-resumed", 80, TARGET_TYPE_INSTANCE, &vpc).await;
    let group = "asg-resumed-registration";
    let arns = vec![arn.clone()];
    create_group(
        &router,
        group,
        template,
        &subnets,
        &arns,
        AttachedAs::TargetGroupArns,
    )
    .await;

    let suspended = vec![PROCESS_ADD_TO_LOAD_BALANCER.to_string()];
    let (status, body) = suspend(&router, group, &suspended).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses expected 200, got {status}: {body}"
    );
    raise(&router, case, group, 1, RaiseWith::SetDesiredCapacity).await;
    let withheld = settled_ids(&router, case, group, 1).await;
    let held = settled_registrations(&router, &arn, &[]).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected {arn:?} to hold no target while the process is suspended, got \
         {held:?}"
    );

    let (status, body) = resume(&router, group, &suspended).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ResumeProcesses expected 200, got {status}: {body}"
    );
    assert!(
        reported_suspensions(&router, group).await.is_empty(),
        "case {case:?}: expected the group to report no suspended process after the resumption"
    );

    // The resumption itself registers nothing: what proves it is the NEXT instance, which is
    // registered while the earlier one stays out.
    raise(&router, case, group, 2, RaiseWith::SetDesiredCapacity).await;
    let both = settled_ids(&router, case, group, 2).await;
    let after: Vec<String> = both
        .iter()
        .filter(|id| !withheld.contains(id))
        .cloned()
        .collect();
    assert_eq!(
        after.len(),
        1,
        "case {case:?}: expected exactly one instance to come into service after the \
         resumption, got {both:?} against {withheld:?}"
    );
    let held = settled_registrations(&router, &arn, &after).await;
    assert_eq!(
        held, after,
        "case {case:?}: expected {arn:?} to hold only the instance that came into service \
         after the resumption, got {held:?}"
    );
    for instance_id in &withheld {
        let (state, reason) = target_health(&router, &arn, instance_id).await;
        assert_eq!(
            (state.as_str(), reason.as_str()),
            (UNUSED_STATE, NOT_REGISTERED_REASON),
            "case {case:?}: expected {instance_id}, launched while the process was suspended, \
             to stay unregistered after the resumption"
        );
        assert_eq!(
            lifecycle_state(&router, group, instance_id).await,
            IN_SERVICE,
            "case {case:?}: expected {instance_id} to go on being reported {IN_SERVICE}"
        );
    }
}

/// The same rule through the other load balancer kind: a Classic Load Balancer the group is
/// attached to takes none of the instances it brings into service while the process is
/// suspended.
#[tokio::test]
async fn a_classic_load_balancer_attachment_is_withheld_the_same_way() {
    let case = "a Classic Load Balancer attachment";
    let (router, engine) = emulator_with_classic_elb();
    let subnets = seed_subnets(&engine, 1);
    let template = "lt-classic-withheld";
    create_launch_template(&router, template).await;
    let load_balancer = "elb-withheld";
    create_classic_load_balancer(&router, REGION, &subnets[0].zone, load_balancer).await;

    let subject = "asg-classic-subject";
    let control = "asg-classic-control";
    for name in [subject, control] {
        let params = vec![
            p("AutoScalingGroupName", name),
            p("LaunchTemplate.LaunchTemplateName", template),
            p("MinSize", "0"),
            p("MaxSize", "4"),
            p("DesiredCapacity", "0"),
            p("VPCZoneIdentifier", subnets[0].id.clone()),
            p("LoadBalancerNames.member.1", load_balancer),
        ];
        let (status, body) = query(&router, "CreateAutoScalingGroup", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating {name:?} expected 200, got {status}: {body}"
        );
    }

    let suspended = vec![PROCESS_ADD_TO_LOAD_BALANCER.to_string()];
    let (status, body) = suspend(&router, subject, &suspended).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses expected 200, got {status}: {body}"
    );
    for name in [subject, control] {
        raise(&router, case, name, 1, RaiseWith::SetDesiredCapacity).await;
    }
    let subject_ids = settled_ids(&router, case, subject, 1).await;
    let control_ids = settled_ids(&router, case, control, 1).await;

    let deadline = Instant::now() + poll_budget();
    let registered = loop {
        let registered = classic_registrations(&router, load_balancer).await;
        if registered == control_ids || Instant::now() >= deadline {
            break registered;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    assert_eq!(
        registered, control_ids,
        "case {case:?}: expected {load_balancer:?} to hold only the control's instance, got \
         {registered:?} against the subject's {subject_ids:?}"
    );
}

/// The other way an instance comes into service: taken out of the group's warm pool. The
/// registration is owed by that step, so it is withheld there too.
#[tokio::test]
async fn an_instance_taken_from_the_warm_pool_while_suspended_is_not_registered() {
    let case = "a warm pool consumed while the process is suspended";
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc = vpc_of(&router, &subnets[0]).await;
    let template = "lt-warm-withheld";
    create_launch_template(&router, template).await;
    let arn = create_target_group(&router, "tg-warm-withheld", 80, TARGET_TYPE_INSTANCE, &vpc).await;
    let group = "asg-warm-withheld";
    let arns = vec![arn.clone()];
    create_group(
        &router,
        group,
        template,
        &subnets,
        &arns,
        AttachedAs::TargetGroupArns,
    )
    .await;

    // A pool that maintains one prepared instance, which is the instance the raise below
    // takes into service rather than launching a fresh one. Its size is stated outright
    // ("specifies the total maximum number of instances that are allowed to be in the warm
    // pool or in any state except Terminated for the Auto Scaling group",
    // `PutWarmPoolType$MaxGroupPreparedCapacity`), so the pool prepares one whatever the
    // group's maximum is.
    let (status, body) = query(
        &router,
        "PutWarmPool",
        &[
            ("AutoScalingGroupName", group),
            ("PoolState", "Stopped"),
            ("MinSize", "1"),
            ("MaxGroupPreparedCapacity", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutWarmPool expected 200, got {status}: {body}"
    );
    let prepared = settled_pool(&router, case, group, 1, "Warmed:Stopped").await;
    assert_eq!(
        prepared.len(),
        1,
        "case {case:?}: expected the pool to prepare one instance, got {prepared:?}"
    );

    let suspended = vec![PROCESS_ADD_TO_LOAD_BALANCER.to_string()];
    let (status, body) = suspend(&router, group, &suspended).await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses expected 200, got {status}: {body}"
    );
    raise(&router, case, group, 1, RaiseWith::SetDesiredCapacity).await;
    let in_service = settled_ids(&router, case, group, 1).await;
    assert_eq!(
        in_service, prepared,
        "case {case:?}: expected the prepared instance to be the one in service, got \
         {in_service:?}"
    );
    let held = settled_registrations(&router, &arn, &[]).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected {arn:?} to hold no target for an instance taken out of the \
         warm pool while the process is suspended, got {held:?}"
    );
    let (state, reason) = target_health(&router, &arn, &in_service[0]).await;
    assert_eq!(
        (state.as_str(), reason.as_str()),
        (UNUSED_STATE, NOT_REGISTERED_REASON),
        "case {case:?}: expected {arn:?} to report {} as {UNUSED_STATE}/{NOT_REGISTERED_REASON}",
        in_service[0]
    );
}

/// Create one group over the awsQuery wire: placed by `VPCZoneIdentifier` over `subnets`, at
/// capacity 0, attached to `target_groups` the way `attached_as` says.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    target_groups: &[String],
    attached_as: AttachedAs,
) {
    let ids: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "4"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", ids.join(",")),
    ];
    for (index, arn) in target_groups.iter().enumerate() {
        match attached_as {
            AttachedAs::TargetGroupArns => params.push(p(
                &format!("TargetGroupARNs.member.{}", index + 1),
                arn.clone(),
            )),
            AttachedAs::Elbv2TrafficSources => {
                params.push(p(
                    &format!("TrafficSources.member.{}.Identifier", index + 1),
                    arn.clone(),
                ));
                params.push(p(
                    &format!("TrafficSources.member.{}.Type", index + 1),
                    "elbv2",
                ));
            }
            AttachedAs::LaterAttach => {}
        }
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} expected 200, got {status}: {body}"
    );
    if attached_as == AttachedAs::LaterAttach && !target_groups.is_empty() {
        let mut params = vec![p("AutoScalingGroupName", name)];
        for (index, arn) in target_groups.iter().enumerate() {
            params.push(p(
                &format!("TargetGroupARNs.member.{}", index + 1),
                arn.clone(),
            ));
        }
        let (status, body) =
            query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "attaching {} target group(s) to {name:?} expected 200, got {status}: {body}",
            target_groups.len()
        );
    }
}

/// `SuspendProcesses` for one group, naming the processes `processes` lists.
async fn suspend(
    router: &MultiServiceRouter,
    group: &str,
    processes: &[String],
) -> (u16, String) {
    process_call(router, "SuspendProcesses", group, processes).await
}

/// `ResumeProcesses` for one group, naming the processes `processes` lists.
async fn resume(router: &MultiServiceRouter, group: &str, processes: &[String]) -> (u16, String) {
    process_call(router, "ResumeProcesses", group, processes).await
}

/// One `SuspendProcesses`/`ResumeProcesses` call: the two take the same request shape.
async fn process_call(
    router: &MultiServiceRouter,
    action: &str,
    group: &str,
    processes: &[String],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            process.clone(),
        ));
    }
    query(router, action, &borrowed(&params)).await
}

/// The processes one group reports as suspended, sorted — what `DescribeAutoScalingGroups`
/// answers under `SuspendedProcesses`.
async fn reported_suspensions(router: &MultiServiceRouter, group: &str) -> Vec<String> {
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
    sorted(&tag_values(&body, "ProcessName"))
}

/// Raise one group's capacity to `desired`, through the request `raise_with` names.
async fn raise(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    desired: usize,
    raise_with: RaiseWith,
) {
    let (status, body) = query(
        router,
        raise_with.action(),
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", desired.to_string().as_str()),
        ],
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} for {group:?} expected 200, got {status}: {body}",
        raise_with.action()
    );
}

/// Read one group back until it holds `wanted` instances and every one of them is in
/// service, or a generous deadline passes — the CONDITION, never a fixed wait. Answers the
/// ids it last read, sorted.
async fn settled_ids(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let members = reported_members(router, group).await;
        let in_service: Vec<String> = members
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .map(|(id, _)| id.clone())
            .collect();
        let settled = members.len() == wanted && in_service.len() == wanted;
        if settled || Instant::now() >= deadline {
            assert_eq!(
                in_service.len(),
                wanted,
                "case {case:?}: expected {group:?} to hold {wanted} instance(s) {IN_SERVICE}, \
                 got {members:?}"
            );
            return sorted(&in_service);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every instance one group reports, as `(instance id, LifecycleState)`.
async fn reported_members(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
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

/// The `LifecycleState` one group reports for one of its instances, or `""` when it reports
/// no such instance.
async fn lifecycle_state(router: &MultiServiceRouter, group: &str, instance_id: &str) -> String {
    reported_members(router, group)
        .await
        .into_iter()
        .find(|(id, _)| id == instance_id)
        .map(|(_, state)| state)
        .unwrap_or_default()
}

/// The ids of the targets one target group holds, sorted — read through Elastic Load
/// Balancing v2's own `DescribeTargetHealth`.
async fn registered_ids(router: &MultiServiceRouter, arn: &str) -> Vec<String> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {arn:?} expected 200, got {status}: {body}"
    );
    let descriptions = list_members(&body, "TargetHealthDescriptions");
    sorted(
        &descriptions
            .into_iter()
            .map(|member| tag_value(member, "Id"))
            .collect::<Vec<String>>(),
    )
}

/// Read one target group until it holds exactly `wanted`, or a generous deadline passes.
/// Answers what it last read, so the caller asserts on it.
async fn settled_registrations(
    router: &MultiServiceRouter,
    arn: &str,
    wanted: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = registered_ids(router, arn).await;
        if held == wanted || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What one target group answers for ONE instance id: its `TargetHealth$State` and
/// `TargetHealth$Reason`, read through `DescribeTargetHealth` naming that target.
async fn target_health(
    router: &MultiServiceRouter,
    arn: &str,
    instance_id: &str,
) -> (String, String) {
    let (status, body) = elbv2_query(
        router,
        "DescribeTargetHealth",
        &[
            p("TargetGroupArn", arn),
            p("Targets.member.1.Id", instance_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeTargetHealth for {instance_id} in {arn:?} expected 200, got {status}: {body}"
    );
    (tag_value(&body, "State"), tag_value(&body, "Reason"))
}

/// The instance ids one Classic Load Balancer reports as registered with it, sorted — read
/// through Elastic Load Balancing's OWN `DescribeLoadBalancers`.
async fn classic_registrations(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let (status, body) = classic_elb_query(
        router,
        REGION,
        "DescribeLoadBalancers",
        &[p("LoadBalancerNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLoadBalancers for {name:?} expected 200, got {status}: {body}"
    );
    sorted(&tag_values(&body, "InstanceId"))
}

/// Read one group's warm pool until it holds `wanted` instances all reporting `state`, or a
/// generous deadline passes. Answers the ids it last read, sorted.
async fn settled_pool(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
    state: &str,
) -> Vec<String> {
    let deadline = Instant::now() + poll_budget();
    loop {
        let (status, body) = query(
            router,
            "DescribeWarmPool",
            &[("AutoScalingGroupName", group)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeWarmPool for {group:?} expected 200, got {status}: {body}"
        );
        let ids = tag_values(&body, "InstanceId");
        let states = tag_values(&body, "LifecycleState");
        let settled = ids.len() == wanted && states.iter().all(|held| held == state);
        if settled || Instant::now() >= deadline {
            return sorted(&ids);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One sorted copy of a list of ids or names, so an assertion compares SETS rather than the
/// order one service happens to report them in.
fn sorted(values: &[String]) -> Vec<String> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted
}
