// SPDX-License-Identifier: CC0-1.0

//! The nodes of a managed node group boot an image the service that OWNS images holds.
//!
//! RULE: a managed node group runs on an Amazon EC2 Auto Scaling group that launches from a
//! launch template Amazon EKS creates for it, and that template states the AMI its nodes boot
//! — which is why a node group reports a `releaseVersion` ("the AMI version of the Amazon EKS
//! optimized AMI to use with your node group", `CreateNodegroupRequest$releaseVersion`) and why
//! an Auto Scaling group, whose launch parameters are read when the group is configured, can be
//! created from it at all. So the `ImageId` the version carries is an image Amazon EC2 itself
//! reports, of the architecture the node group's `amiType` names ("`AL2023_ARM_64_STANDARD`"
//! and its siblings carry the architecture in their own names,
//! `smithy_query(query="shape",shape="AMITypes")`) — never a value EKS wrote for itself.
//!
//! The rows span what the rule generalises over: the AMI type the node group is created with,
//! including the one a request that names none is given, and both architectures the published
//! types cover.
//!
//! EKS creates the template in Amazon EC2 and the group in Auto Scaling, and this suite reads
//! both back through their own APIs, which is why it lives here rather than in the EKS crate.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};

use crate::eks_wire::{autoscaling, cluster_body, ec2, eks, role_arn, Answer};
use crate::query_wire::{element, elements, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// How often a clock-owed read is retried, and for how long in all: nothing inside the
/// emulator sleeps for a due instant, so a status is polled rather than waited for, with a
/// generous deadline.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 200;

/// Read a cluster until it is `ACTIVE`, which is the precondition a node group has.
fn wait_for_active_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    cluster: &str,
) {
    for _ in 0..POLLS {
        let read = eks(router, runtime, "GET", &format!("/clusters/{cluster}"), None);
        if read.body["cluster"]["status"] == serde_json::json!("ACTIVE") {
            return;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("setup: cluster {cluster:?} never reached ACTIVE");
}

/// The name of the Auto Scaling group the node group reports, once the capacity it runs on
/// exists.
fn reported_group_name(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    cluster: &str,
    nodegroup: &str,
) -> String {
    for _ in 0..POLLS {
        let read = eks(
            router,
            runtime,
            "GET",
            &format!("/clusters/{cluster}/node-groups/{nodegroup}"),
            None,
        );
        if let Some(name) = read.body["nodegroup"]["resources"]["autoScalingGroups"][0]["name"]
            .as_str()
        {
            return name.to_string();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("case {case:?}: the node group {nodegroup:?} never reported an Auto Scaling group");
}

/// The `(LaunchTemplateId, Version)` the Auto Scaling group `name` launches from, as Auto
/// Scaling itself reports it. The version is empty where the group's specification names
/// none, which is the default version of the template
/// (`LaunchTemplateSpecification$Version`).
fn group_launch_template(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    name: &str,
) -> (String, String) {
    let described: Answer = autoscaling(
        router,
        runtime,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    );
    assert_eq!(
        described.status, 200,
        "case {case:?}: DescribeAutoScalingGroups must answer 200, got {described:?}"
    );
    (
        element(&described.text, "LaunchTemplateId"),
        opt_element(&described.text, "Version").unwrap_or_default(),
    )
}

/// One row: the `amiType` the node group is created with (`None` for a request that names
/// none) and the processor architecture Amazon EC2 must report for the image its nodes boot.
struct Row {
    case: &'static str,
    nodegroup: &'static str,
    ami_type: Option<&'static str>,
    /// SOURCE: `AMITypes` — a type naming `ARM` runs 64-bit Arm nodes, every other published
    /// type 64-bit x86. The default a request naming no type is given is an x86 one.
    architecture: &'static str,
}

#[test]
fn a_node_groups_launch_template_states_an_image_amazon_ec2_holds() {
    std::thread::Builder::new()
        .name("eks-nodegroup-node-image".to_string())
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

            let cluster = "nodegroup-image-host";
            let created = eks(
                &router,
                &runtime,
                "POST",
                "/clusters",
                Some(cluster_body(cluster, &subnets)),
            );
            assert_eq!(
                created.status, 200,
                "setup: the cluster must be created, got {created:?}"
            );
            wait_for_active_cluster(&router, &runtime, cluster);

            let rows = [
                Row {
                    case: "a node group that names no AMI type at all",
                    nodegroup: "ng-image-default",
                    ami_type: None,
                    architecture: "x86_64",
                },
                Row {
                    case: "an Amazon Linux 2023 node group of 64-bit x86",
                    nodegroup: "ng-image-al2023",
                    ami_type: Some("AL2023_x86_64_STANDARD"),
                    architecture: "x86_64",
                },
                Row {
                    case: "an Amazon Linux 2023 node group of 64-bit Arm",
                    nodegroup: "ng-image-arm",
                    ami_type: Some("AL2023_ARM_64_STANDARD"),
                    architecture: "arm64",
                },
            ];

            for row in &rows {
                let case = row.case;
                let mut body = serde_json::json!({
                    "nodegroupName": row.nodegroup,
                    "nodeRole": role_arn("eksNodeRole"),
                    "subnets": subnets,
                });
                if let Some(ami_type) = row.ami_type {
                    body["amiType"] = serde_json::json!(ami_type);
                }
                let answer = eks(
                    &router,
                    &runtime,
                    "POST",
                    &format!("/clusters/{cluster}/node-groups"),
                    Some(body),
                );
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: the node group must be created, got {answer:?} ({})",
                    answer.message()
                );

                // The group EKS created for it, and the launch-template version that group
                // launches from — both read through the APIs of the services that own them.
                let group_name =
                    reported_group_name(&router, &runtime, case, cluster, row.nodegroup);
                let (template_id, version) =
                    group_launch_template(&router, &runtime, case, &group_name);
                assert!(
                    !template_id.is_empty(),
                    "case {case:?}: the group {group_name} must launch from a launch template, \
                     got {template_id:?}"
                );

                let mut params: Vec<(&str, &str)> =
                    vec![("LaunchTemplateId", template_id.as_str())];
                if !version.is_empty() {
                    params.push(("Versions.1", version.as_str()));
                }
                let described = ec2(&router, &runtime, "DescribeLaunchTemplateVersions", &params);
                assert_eq!(
                    described.status, 200,
                    "case {case:?}: DescribeLaunchTemplateVersions must answer 200, got \
                     {described:?}"
                );
                let image_id = element(&described.text, "imageId");
                assert!(
                    !image_id.is_empty(),
                    "case {case:?}: the version the group launches from must state the image \
                     its nodes boot, got {}",
                    described.text
                );

                // And that image is one Amazon EC2 really holds, of the architecture the AMI
                // type names.
                let images = ec2(
                    &router,
                    &runtime,
                    "DescribeImages",
                    &[("ImageId.1", image_id.as_str())],
                );
                assert_eq!(
                    images.status, 200,
                    "case {case:?}: EC2 must hold {image_id}, got {images:?}"
                );
                assert_eq!(
                    element(&images.text, "imageId"),
                    image_id,
                    "case {case:?}: EC2 must report the image the template names, got {}",
                    images.text
                );
                assert_eq!(
                    element(&images.text, "architecture"),
                    row.architecture,
                    "case {case:?}: the node group's nodes must boot an image of its own \
                     architecture, got {}",
                    images.text
                );
            }
        })
        .expect("spawn the eks nodegroup-node-image thread")
        .join()
        .expect("the eks nodegroup-node-image thread");
}
