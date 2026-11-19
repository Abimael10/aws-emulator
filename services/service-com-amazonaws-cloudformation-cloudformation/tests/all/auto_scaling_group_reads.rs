// SPDX-License-Identifier: CC0-1.0

//! The reads this target takes of an Auto Scaling group a stack created, shared by every
//! suite that takes them.
//!
//! An `AWS::AutoScaling::AutoScalingGroup` a template declares is created in Amazon EC2 Auto
//! Scaling, so more than one suite here reads a group back through that service's own wire:
//! which groups a filtered `DescribeAutoScalingGroups` matches, and which of a group's
//! instances it reports in service. Both reads live here once, so no two suites can disagree
//! about how a group is asked for or how long a launch is waited for.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{autoscaling, xml_texts};

/// The longest a caller waits for the instances a group was sized for to come into service: a
/// group's launches settle on the clock at an instant Auto Scaling records, and nothing sleeps
/// for it, so what is waited for is the CONDITION. Generous, because what a case is about is
/// that the instances arrive at all, not how quickly the host gets there.
pub const IN_SERVICE_DEADLINE: Duration = Duration::from_secs(120);

/// How often the wait below looks again.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One `DescribeAutoScalingGroups` read, filtered to `filtered_to` (every group when that is
/// empty): the XML body, and the group names the answer reports in wire order.
///
/// The names come back rather than being asserted here, because a case that asks for a name
/// no deployment declared is asserting that the answer carries NONE.
pub async fn describe_groups(
    case: &str,
    router: &MultiServiceRouter,
    filtered_to: &[&str],
) -> (String, Vec<String>) {
    let params: Vec<(String, String)> = filtered_to
        .iter()
        .enumerate()
        .map(|(index, name)| {
            (
                format!("AutoScalingGroupNames.member.{}", index + 1),
                (*name).to_string(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (status, body) = autoscaling(router, "DescribeAutoScalingGroups", &borrowed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups({filtered_to:?}) must answer 200, got \
         {status} {body}"
    );
    let names = xml_texts(&body, "AutoScalingGroupName");
    (body, names)
}

/// Amazon EC2 Auto Scaling's own answer for the group `name`, asserted to report exactly that
/// one group — the read a client takes with the name a deployment just handed it.
pub async fn described_group(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let (body, names) = describe_groups(case, router, &[name]).await;
    assert_eq!(
        names,
        vec![name.to_string()],
        "case {case:?}: expected Amazon EC2 Auto Scaling to report exactly the group {name:?} \
         the stack reports created, got {body}"
    );
    body
}

/// The ids of the instances Auto Scaling reports IN SERVICE in the group, once it reports
/// `wanted` of them — polled, because a launch settles at an instant that service records.
///
/// The two members are read as PARALLEL lists and zipped, which is sound exactly because the
/// answering service reports both on every instance it carries.
pub async fn in_service_instances(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + IN_SERVICE_DEADLINE;
    loop {
        let body = described_group(case, router, name).await;
        let in_service: Vec<String> = xml_texts(&body, "InstanceId")
            .into_iter()
            .zip(xml_texts(&body, "LifecycleState"))
            .filter(|(_, state)| state == "InService")
            .map(|(instance_id, _)| instance_id)
            .collect();
        if in_service.len() == wanted {
            return in_service;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group {name:?} to hold {wanted} instances in service, \
             got {in_service:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
