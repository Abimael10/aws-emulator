// SPDX-License-Identifier: CC0-1.0

//! What a group does about a member the account's Amazon EC2 store NO LONGER RUNS, when that
//! member was taken away by a caller of Amazon EC2's own API.
//!
//! RULE: a group's health check reads the Amazon EC2 state of each of its members, so a member
//! that service no longer runs is reported `Unhealthy`
//! (`smithy_query(query="operation",operation="DescribeAutoScalingGroups")` reports
//! `Instances[].HealthStatus` beside `Instances[].LifecycleState`) and — unless the group's
//! `ReplaceUnhealthy` process is suspended
//! (`smithy_query(query="shape",shape="ScalingProcessQuery")`) — taken out of the group and
//! replaced, so the group goes on holding the capacity it was ALREADY configured for: the
//! replacement restores `DesiredCapacity` without changing it, and `MinSize`, `DesiredCapacity`
//! and `MaxSize` all read back as they stood. The two moves are recorded as two activities,
//! each under the sentence for what it did: the check that condemned the member states why the
//! member went out of service, and the launch states that it replaced an unhealthy instance.
//! Only the members Amazon EC2 stopped running are condemned; the ones it still runs stay in
//! service.
//!
//! The tables below span what the rule generalises over: HOW MANY of a group's members Amazon
//! EC2 stopped running (the whole of a one-member group, and one of two), and whether the group
//! acts on the verdict (nothing suspended, `ReplaceUnhealthy` suspended, and that process
//! resumed afterwards) — the differential being measured on TWIN groups of one estate that one
//! `ec2:TerminateInstances` call takes the members of, which is how a client tells a correctly
//! suspended process from a broken replacement path.
//!
//! Every wait below looks again until the condition it needs holds, never for a fixed span, and
//! nothing here asserts on the wall clock.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    activity_causes, borrowed, create_launch_template, ec2_query, emulator, list_members, p, query,
    seed_subnets, tag_value, SeededSubnet,
};

/// The `LifecycleState` a member in service is reported in, and the two `HealthStatus` values a
/// member is reported with — "`Healthy`", "`Unhealthy`"
/// (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="HealthStatus")`).
const IN_SERVICE: &str = "InService";
const HEALTHY: &str = "Healthy";
const UNHEALTHY: &str = "Unhealthy";

/// The process that ACTS on an unhealthy verdict, as `ProcessType$ProcessName` names it.
const PROCESS_REPLACE_UNHEALTHY: &str = "ReplaceUnhealthy";

/// The sentence the activity taking a member out of service carries when the check that
/// condemned it read the member's Amazon EC2 state, and the sentence the activity launching its
/// replacement carries.
///
/// Written out here rather than read from the crate: the activity log is what a client reads
/// the reason off, so the assertion states the wording itself — an assertion re-using the
/// implementation's own literal would assert nothing about it.
const TAKEN_OUT_BY_EC2_CHECK: &str = "an instance was taken out of service in response to an \
                                      EC2 health check indicating it has been terminated or \
                                      stopped";
const LAUNCHED_TO_REPLACE_UNHEALTHY: &str =
    "an instance was launched in response to an unhealthy instance needing to be replaced";

/// How long a wait looks again for the condition it needs. Generous on purpose — many times the
/// spans this service moves an instance over — because what is asserted is that the condition is
/// reached at all, never how fast the host runs.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// How often it looks again while it waits.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One member of a group, as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    instance_id: String,
    lifecycle_state: String,
    health_status: String,
}

/// What one group reports: the three sizes it is configured with and the members it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    min_size: String,
    desired_capacity: String,
    max_size: String,
    members: Vec<Member>,
}

impl Reported {
    /// The three sizes, as the triple a case states them in.
    fn sizes(&self) -> (&str, &str, &str) {
        (
            self.min_size.as_str(),
            self.desired_capacity.as_str(),
            self.max_size.as_str(),
        )
    }

    /// The ids of the members reported in service, sorted so a comparison does not depend on
    /// the order the group lists them in.
    fn in_service(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .members
            .iter()
            .filter(|member| member.lifecycle_state == IN_SERVICE)
            .map(|member| member.instance_id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Read one group back over the awsQuery wire, where a client reads it.
async fn reported(router: &MultiServiceRouter, case: &str, name: &str) -> Reported {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups for {name:?} expected 200, got {status}: {body}"
    );
    Reported {
        min_size: tag_value(&body, "MinSize"),
        desired_capacity: tag_value(&body, "DesiredCapacity"),
        max_size: tag_value(&body, "MaxSize"),
        members: list_members(&body, "Instances")
            .into_iter()
            .map(|member| Member {
                instance_id: tag_value(member, "InstanceId"),
                lifecycle_state: tag_value(member, "LifecycleState"),
                health_status: tag_value(member, "HealthStatus"),
            })
            .collect(),
    }
}

/// Create one group over the awsQuery wire: `capacity` members placed over `subnets`, checked
/// against their Amazon EC2 state with no grace period holding a verdict off.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    subnets: &[SeededSubnet],
    capacity: i32,
    max_size: i32,
) {
    let placement: Vec<&str> = subnets.iter().map(|subnet| subnet.id.as_str()).collect();
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", capacity.to_string()),
        p("VPCZoneIdentifier", placement.join(",")),
        p("HealthCheckType", "EC2"),
        p("HealthCheckGracePeriod", "0"),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {name:?} expected 200, got {status}: {body}"
    );
}

/// Look again until the group reports exactly `capacity` members in service, and answer their
/// ids sorted: the steady state every case starts from.
async fn settled_members(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    capacity: usize,
) -> Vec<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        let settled = held.members.len() == capacity && held.in_service().len() == capacity;
        if settled || Instant::now() >= deadline {
            assert!(
                settled,
                "case {case:?}: expected {name:?} to settle on {capacity} member(s) in \
                 {IN_SERVICE}, got {held:?}"
            );
            return held.in_service();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Terminate `instance_ids` through Amazon EC2's OWN API — out of band, as far as the groups
/// holding them are concerned — in ONE call, so one caller takes the members of every group
/// named away at once.
async fn terminate_in_ec2(router: &MultiServiceRouter, case: &str, instance_ids: &[String]) {
    let params: Vec<(String, String)> = instance_ids
        .iter()
        .enumerate()
        .map(|(index, instance_id)| p(&format!("InstanceId.{}", index + 1), instance_id.clone()))
        .collect();
    let (status, body) = ec2_query(router, "TerminateInstances", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: TerminateInstances for {instance_ids:?} expected 200, got {status}: {body}"
    );
}

/// Suspend one process of a group.
async fn suspend(router: &MultiServiceRouter, case: &str, name: &str, process: &str) {
    let (status, body) = query(
        router,
        "SuspendProcesses",
        &[
            ("AutoScalingGroupName", name),
            ("ScalingProcesses.member.1", process),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SuspendProcesses({process}) for {name:?} expected 200, got {status}: \
         {body}"
    );
}

/// Resume one process of a group.
async fn resume(router: &MultiServiceRouter, case: &str, name: &str, process: &str) {
    let (status, body) = query(
        router,
        "ResumeProcesses",
        &[
            ("AutoScalingGroupName", name),
            ("ScalingProcesses.member.1", process),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ResumeProcesses({process}) for {name:?} expected 200, got {status}: {body}"
    );
}

/// Look again until the group reports `capacity` members in service, none of which is among
/// `replaced`, and answer their ids sorted.
async fn replacements_of(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    replaced: &[String],
    capacity: usize,
) -> Vec<String> {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        let serving = held.in_service();
        let fresh = held.members.len() == capacity
            && serving.len() == capacity
            && serving.iter().all(|id| !replaced.contains(id));
        if fresh || Instant::now() >= deadline {
            assert!(
                fresh,
                "case {case:?}: expected {name:?} to hold {capacity} member(s) in {IN_SERVICE}, \
                 none of {replaced:?}, got {held:?}"
            );
            return serving;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again until the group reports the member `instance_id` with the health status given,
/// and answer what it then reports.
async fn member_reported_as(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    instance_id: &str,
    health_status: &str,
) -> Reported {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let held = reported(router, case, name).await;
        let condemned = held.members.iter().any(|member| {
            member.instance_id == instance_id && member.health_status == health_status
        });
        if condemned || Instant::now() >= deadline {
            assert!(
                condemned,
                "case {case:?}: expected {name:?} to report {instance_id} as {health_status}, \
                 got {held:?}"
            );
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How many activities of the group state `sentence`.
async fn activities_stating(router: &MultiServiceRouter, name: &str, sentence: &str) -> usize {
    activity_causes(router, name)
        .await
        .iter()
        .filter(|cause| cause.contains(sentence))
        .count()
}

/// RULE: a group that has suspended nothing reports the member Amazon EC2 no longer runs out of
/// service, takes it out and launches a replacement at the capacity it was already configured
/// for — recording the check's sentence for the termination and the replacement's own for the
/// launch — while its TWIN, which has `ReplaceUnhealthy` suspended, launches nothing for the
/// member the same `ec2:TerminateInstances` call took away until that process is resumed.
#[tokio::test]
async fn an_ec2_terminated_member_is_replaced_unless_the_replacement_is_suspended() {
    let case = "twin groups, one ec2:TerminateInstances call, ReplaceUnhealthy suspended on one";
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = "lt-ec2-gone-twins";
    create_launch_template(&router, template).await;

    let control = "asg-ec2-gone-control";
    let subject = "asg-ec2-gone-subject";
    create_group(&router, case, control, template, &subnets, 1, 2).await;
    create_group(&router, case, subject, template, &subnets, 1, 2).await;
    let control_member = settled_members(&router, case, control, 1).await;
    let subject_member = settled_members(&router, case, subject, 1).await;

    // The differential: the subject stops acting on an unhealthy verdict, the control is left
    // exactly as it was created.
    suspend(&router, case, subject, PROCESS_REPLACE_UNHEALTHY).await;

    // ONE call of Amazon EC2's own API takes both twins' members away.
    let killed: Vec<String> = control_member
        .iter()
        .chain(subject_member.iter())
        .cloned()
        .collect();
    terminate_in_ec2(&router, case, &killed).await;

    // The control replaces: one member in service that is not the killed one, at the three
    // sizes the group was created with.
    let replacement = replacements_of(&router, case, control, &control_member, 1).await;
    assert_eq!(
        replacement.len(),
        1,
        "case {case:?}: expected {control:?} to hold one replacement, got {replacement:?}"
    );
    let held = reported(&router, case, control).await;
    assert_eq!(
        held.sizes(),
        ("0", "1", "2"),
        "case {case:?}: expected {control:?} to keep the sizes it was created with, got {held:?}"
    );
    assert!(
        held.members
            .iter()
            .all(|member| !control_member.contains(&member.instance_id)),
        "case {case:?}: expected {:?} to have left {control:?}, got {held:?}",
        control_member
    );
    // The replacement is in service and reported healthy: a member Amazon EC2 runs is not
    // condemned by the check that condemned the one it replaced.
    assert_eq!(
        held.members
            .iter()
            .filter(|member| member.lifecycle_state == IN_SERVICE
                && member.health_status == HEALTHY)
            .count(),
        1,
        "case {case:?}: expected {control:?} to report its replacement {IN_SERVICE} and \
         {HEALTHY}, got {held:?}"
    );

    // Two moves, two activities, each under the sentence for what it did.
    let taken_out = activities_stating(&router, control, TAKEN_OUT_BY_EC2_CHECK).await;
    let launched = activities_stating(&router, control, LAUNCHED_TO_REPLACE_UNHEALTHY).await;
    assert_eq!(
        (taken_out, launched),
        (1, 1),
        "case {case:?}: expected {control:?} to record the termination under \
         {TAKEN_OUT_BY_EC2_CHECK:?} and the launch under {LAUNCHED_TO_REPLACE_UNHEALTHY:?}, got \
         {:?}",
        activity_causes(&router, control).await
    );

    // The subject reached the same verdict — the check is not the suspended process — but acted
    // on none of it: the member is still listed, and nothing was launched for it.
    let held = member_reported_as(&router, case, subject, &subject_member[0], UNHEALTHY).await;
    assert_eq!(
        held.sizes(),
        ("0", "1", "2"),
        "case {case:?}: expected {subject:?} to keep the sizes it was created with, got {held:?}"
    );
    let launched = activities_stating(&router, subject, LAUNCHED_TO_REPLACE_UNHEALTHY).await;
    assert_eq!(
        launched, 0,
        "case {case:?}: expected {subject:?} to launch nothing while \
         {PROCESS_REPLACE_UNHEALTHY} is suspended, got {:?}",
        activity_causes(&router, subject).await
    );

    // Resuming the process performs the replacement that was owed, with no further call.
    resume(&router, case, subject, PROCESS_REPLACE_UNHEALTHY).await;
    let replacement = replacements_of(&router, case, subject, &subject_member, 1).await;
    assert_eq!(
        replacement.len(),
        1,
        "case {case:?}: expected {subject:?} to replace its member once \
         {PROCESS_REPLACE_UNHEALTHY} was resumed, got {replacement:?}"
    );
    let held = reported(&router, case, subject).await;
    assert_eq!(
        held.sizes(),
        ("0", "1", "2"),
        "case {case:?}: expected {subject:?} to keep the sizes it was created with, got {held:?}"
    );
    let launched = activities_stating(&router, subject, LAUNCHED_TO_REPLACE_UNHEALTHY).await;
    assert_eq!(
        launched, 1,
        "case {case:?}: expected {subject:?} to record one replacement launch, got {:?}",
        activity_causes(&router, subject).await
    );
}

/// The same rule where Amazon EC2 stopped running only SOME of a group's members: the ones it
/// still runs stay in service and only the shortfall is launched, at the unchanged capacity.
#[tokio::test]
async fn only_the_member_amazon_ec2_no_longer_runs_is_replaced() {
    let case = "one of two members taken away by ec2:TerminateInstances";
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 2);
    let template = "lt-ec2-gone-partial";
    create_launch_template(&router, template).await;

    let name = "asg-ec2-gone-partial";
    create_group(&router, case, name, template, &subnets, 2, 3).await;
    let original = settled_members(&router, case, name, 2).await;
    let killed = original[0].clone();
    let survivor = original[1].clone();

    terminate_in_ec2(&router, case, std::slice::from_ref(&killed)).await;

    let serving = replacements_of(&router, case, name, std::slice::from_ref(&killed), 2).await;
    assert!(
        serving.contains(&survivor),
        "case {case:?}: expected the member Amazon EC2 still runs ({survivor}) to stay in \
         {IN_SERVICE}, got {serving:?}"
    );
    let replacement: Vec<&String> = serving.iter().filter(|id| **id != survivor).collect();
    assert_eq!(
        replacement.len(),
        1,
        "case {case:?}: expected exactly one replacement for {killed}, got {serving:?}"
    );
    let held = reported(&router, case, name).await;
    assert_eq!(
        held.sizes(),
        ("0", "2", "3"),
        "case {case:?}: expected {name:?} to keep the sizes it was created with, got {held:?}"
    );
    let taken_out = activities_stating(&router, name, TAKEN_OUT_BY_EC2_CHECK).await;
    let launched = activities_stating(&router, name, LAUNCHED_TO_REPLACE_UNHEALTHY).await;
    assert_eq!(
        (taken_out, launched),
        (1, 1),
        "case {case:?}: expected one termination under {TAKEN_OUT_BY_EC2_CHECK:?} and one launch \
         under {LAUNCHED_TO_REPLACE_UNHEALTHY:?}, got {:?}",
        activity_causes(&router, name).await
    );
}
