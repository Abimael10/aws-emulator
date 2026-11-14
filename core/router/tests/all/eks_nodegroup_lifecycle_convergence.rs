// SPDX-License-Identifier: CC0-1.0

//! A managed node group's post-create lifecycle CONVERGES, in the assembled emulator the
//! production binary mounts.
//!
//! RULE under test: every status a managed node group's lifecycle passes through is
//! TRANSIENT — the clock owes the terminal one, and a client that keeps reading reaches it.
//! `CreateNodegroup` leaves the node group `CREATING` and it becomes `ACTIVE`;
//! `UpdateNodegroupConfig` returns an `Update`, leaves the node group `UPDATING` and the
//! update `InProgress`, and the node group returns to `ACTIVE` carrying the NEW
//! `scalingConfig` while the update reports `Successful`; `DeleteNodegroup` leaves it
//! `DELETING` and `DescribeNodegroup` is then refused `ResourceNotFoundException` (HTTP
//! 404), after which the cluster that held it can itself be deleted and stops being
//! readable. Nothing sleeps for a due instant inside the emulator, so each step is POLLED
//! until the condition holds, with a generous deadline.
//!
//! This suite lives here rather than in the EKS crate because the lifecycle it drives
//! crosses services: the node group's capacity is created in Amazon EC2 Auto Scaling and
//! Amazon EC2 and released there again, which only the assembled router mounts — the one
//! difference between this flow and the EKS crate's own in-process cover of the same rule.
//! The scale step is the one a client's `eks wait nodegroup-active` blocks on, and the
//! delete step the one a teardown blocks on, so both are driven here end to end.
//!
//! The harness is the test crate's own (`eks_wire`).

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};

use crate::eks_wire::{autoscaling, cluster_body, ec2, eks, role_arn, Answer};
use crate::query_wire::{elements, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How often a clock-owed read is retried, and for how long in all: nothing inside the
/// emulator sleeps for a due instant, so a status is polled rather than waited for, with a
/// deadline far above every span this service owes (the longest is its deletion window).
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 400 polls at 50ms is a 20-second deadline.
const POLLS: usize = 400;

/// Poll `read` until `holds` answers true, and answer the last read; panic naming `what`
/// when the deadline passes first.
fn read_until(
    what: &str,
    mut read: impl FnMut() -> Answer,
    holds: impl Fn(&Answer) -> bool,
) -> Answer {
    let mut last = read();
    for _ in 0..POLLS {
        if holds(&last) {
            return last;
        }
        std::thread::sleep(POLL_INTERVAL);
        last = read();
    }
    panic!("expected {what}; the last read answered {last:?}");
}

#[test]
fn a_node_group_scales_and_is_torn_down_to_a_not_found() {
    std::thread::Builder::new()
        .name("eks-nodegroup-lifecycle".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            // The account's own default subnets, read from EC2 rather than invented.
            let defaults = ec2(
                &router,
                &runtime,
                "DescribeSubnets",
                &[
                    ("Filter.1.Name", "default-for-az"),
                    ("Filter.1.Value.1", "true"),
                ],
            );
            assert_eq!(
                defaults.status, 200,
                "setup: DescribeSubnets must succeed, got {defaults:?}"
            );
            let available = elements(&defaults.text, "subnetId");
            assert!(
                available.len() >= 2,
                "setup: the account must have default subnets, got {available:?}"
            );
            let subnets = vec![available[0].clone(), available[1].clone()];

            let cluster = "nodegroup-lifecycle-host";
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &subnets)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster must be created, got {created:?} ({})",
                created.message()
            );
            read_until(
                "the cluster to reach ACTIVE",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| answer.body["cluster"]["status"] == serde_json::json!("ACTIVE"),
            );

            let nodegroup = "ng-lifecycle";
            let path = format!("/clusters/{cluster}/node-groups/{nodegroup}");
            let answer = eks(
                &router,
                &runtime,
                "POST",
                &format!("/clusters/{cluster}/node-groups"),
                Some(serde_json::json!({
                    "nodegroupName": nodegroup,
                    "nodeRole": role_arn("eksNodeRole"),
                    "subnets": subnets,
                    "scalingConfig": { "minSize": 1, "maxSize": 2, "desiredSize": 1 },
                })),
            );
            assert_eq!(
                answer.status, 200,
                "the node group must be created, got {answer:?} ({})",
                answer.message()
            );
            assert_eq!(
                answer.body["nodegroup"]["status"],
                serde_json::json!("CREATING"),
                "a create leaves the node group CREATING, got {}",
                answer.text
            );

            // 1. CREATING -> ACTIVE.
            let active = read_until(
                "the node group to reach ACTIVE",
                || eks(&router, &runtime, "GET", &path, None),
                |answer| answer.body["nodegroup"]["status"] == serde_json::json!("ACTIVE"),
            );
            assert_eq!(
                active.body["nodegroup"]["scalingConfig"]["desiredSize"],
                serde_json::json!(1),
                "the node group must carry the sizes it was created with, got {}",
                active.text
            );
            let group_name = active.body["nodegroup"]["resources"]["autoScalingGroups"][0]["name"]
                .as_str()
                .expect("the Auto Scaling group backing the node group")
                .to_string();

            // 2. UPDATING -> ACTIVE, carrying the NEW scaling configuration, with the
            // update's own record reaching `Successful`.
            let scaled = eks(
                &router,
                &runtime,
                "POST",
                &format!("{path}/update-config"),
                Some(serde_json::json!({
                    "scalingConfig": { "minSize": 2, "maxSize": 4, "desiredSize": 3 },
                })),
            );
            assert_eq!(
                scaled.status, 200,
                "the scale must be accepted, got {scaled:?} ({})",
                scaled.message()
            );
            let update_id = scaled.body["update"]["id"]
                .as_str()
                .expect("the update's own id")
                .to_string();
            let settled = read_until(
                "the node group to return to ACTIVE carrying the new scaling configuration",
                || eks(&router, &runtime, "GET", &path, None),
                |answer| {
                    answer.body["nodegroup"]["status"] == serde_json::json!("ACTIVE")
                        && answer.body["nodegroup"]["scalingConfig"]["desiredSize"]
                            == serde_json::json!(3)
                },
            );
            assert_eq!(
                settled.body["nodegroup"]["scalingConfig"]["maxSize"],
                serde_json::json!(4),
                "the settled node group must carry every size the update named, got {}",
                settled.text
            );
            let update = read_until(
                "the update to report Successful",
                || {
                    eks(
                        &router,
                        &runtime,
                        "GET",
                        &format!("/clusters/{cluster}/updates/{update_id}?nodegroupName={nodegroup}"),
                        None,
                    )
                },
                |answer| answer.body["update"]["status"] == serde_json::json!("Successful"),
            );
            assert_eq!(
                update.body["update"]["type"],
                serde_json::json!("ConfigUpdate"),
                "the update must report what it changed, got {}",
                update.text
            );

            // 3. DELETING -> not found, and the capacity goes with it.
            let deleted = eks(&router, &runtime, "DELETE", &path, None);
            assert_eq!(
                deleted.status, 200,
                "the delete must be accepted, got {deleted:?} ({})",
                deleted.message()
            );
            assert_eq!(
                deleted.body["nodegroup"]["status"],
                serde_json::json!("DELETING"),
                "a delete leaves the node group DELETING, got {}",
                deleted.text
            );
            let gone = read_until(
                "the node group to be reported absent once its removal has finished",
                || eks(&router, &runtime, "GET", &path, None),
                |answer| answer.status == 404,
            );
            assert_eq!(
                gone.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "the removed node group must be refused ResourceNotFoundException, got {gone:?}"
            );
            let released = autoscaling(
                &router,
                &runtime,
                "DescribeAutoScalingGroups",
                &[("AutoScalingGroupNames.member.1", group_name.as_str())],
            );
            assert_eq!(
                opt_element(&released.text, "AutoScalingGroupName"),
                None,
                "the removed node group's Auto Scaling group must be released, got {}",
                released.text
            );

            // 4. And the cluster that held it can now be deleted: the node group no longer
            // holds it, and the cluster stops being readable.
            let cluster_deleted = eks(
                &router,
                &runtime,
                "DELETE",
                &format!("/clusters/{cluster}"),
                None,
            );
            assert_eq!(
                cluster_deleted.status, 200,
                "the cluster must be deleted once its node group is gone, got \
                 {cluster_deleted:?} ({})",
                cluster_deleted.message()
            );
            let cluster_gone = read_until(
                "the deleted cluster to be reported absent",
                || eks(&router, &runtime, "GET", &format!("/clusters/{cluster}"), None),
                |answer| answer.status == 404,
            );
            assert_eq!(
                cluster_gone.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "the deleted cluster must be refused ResourceNotFoundException, got \
                 {cluster_gone:?}"
            );
        })
        .expect("spawn the eks nodegroup-lifecycle thread")
        .join()
        .expect("the eks nodegroup-lifecycle thread");
}
