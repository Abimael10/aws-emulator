// SPDX-License-Identifier: CC0-1.0

//! What container properties a job definition that runs on FARGATE may state.
//!
//! The EC2-only members are REFUSED. `ContainerProperties$vcpus` and
//! `ContainerProperties$memory` are "deprecated, use `resourceRequirements`" and "not
//! supported for jobs running on Fargate resources"; `ContainerProperties$privileged`
//! ("isn't applicable to jobs that are running on Fargate resources and shouldn't be
//! provided, or specified as false"), `ContainerProperties$ulimits` and
//! `ContainerProperties$instanceType` ("isn't applicable to … jobs that run on Fargate
//! resources, and shouldn't be provided"), `EcsTaskProperties$ipcMode` ("not supported for
//! jobs that run on Fargate resources") and `TaskContainerProperties$privileged` ("not
//! supported for Windows containers or tasks run on Fargate") say the same of themselves. A
//! definition that runs on Fargate and states one of them describes a container Fargate
//! cannot run, so the registration is refused and nothing is stored.
//!
//! The rule binds to EVERY container-properties object a job definition carries, the way the
//! execution role and the Fargate reservation do ([`super::execution_roles_of`],
//! [`super::resource_requirements_of`]). The other thing the model documents about a
//! container that runs on Fargate rather than on Amazon EC2 — the platform version it runs
//! on when the request names none — is applied where the revision is written
//! ([`super::apply_fargate_platform_defaults`]).

use crate::ops::types::{ContainerProperties, EcsProperties, NodeProperties};

/// How AWS Batch words the refusal of a member Fargate does not support, naming the member
/// the request stated.
///
/// The model states no message for this rule, so the detail is the sentence this service is
/// recorded to answer with: the member, that Fargate does not take it, and — for the two
/// deprecated reservation members, whose own documentation names the replacement ("this
/// parameter is deprecated, use `resourceRequirements` to specify the vCPU requirements")
/// — what to state instead.
fn unsupported_member_detail(member: &str, resource_requirements_instead: bool) -> String {
    if resource_requirements_instead {
        return format!(
            "{member} is not applicable for Fargate. Use resource requirements instead."
        );
    }
    format!("{member} is not applicable for Fargate.")
}

/// The reason a container-properties object a job definition carries cannot run on Fargate,
/// or `None` when every member it states is one Fargate supports.
///
/// The members are examined in the order the model declares them, so one request draws one
/// deterministic refusal however many unsupported members it states.
fn unsupported_container_member(container: &ContainerProperties) -> Option<String> {
    if container.vcpus.is_some() {
        return Some(unsupported_member_detail("vcpus", true));
    }
    if container.memory.is_some() {
        return Some(unsupported_member_detail("memory", true));
    }
    // "shouldn't be provided, OR SPECIFIED AS FALSE": elevated privileges are what Fargate
    // does not give a container, so `false` states nothing Fargate cannot honour.
    if container.privileged == Some(true) {
        return Some(unsupported_member_detail("privileged", false));
    }
    if container
        .ulimits
        .as_ref()
        .is_some_and(|ulimits| !ulimits.is_empty())
    {
        return Some(unsupported_member_detail("ulimits", false));
    }
    if container
        .instance_type
        .as_deref()
        .is_some_and(|instance_type| !instance_type.is_empty())
    {
        return Some(unsupported_member_detail("instanceType", false));
    }
    None
}

/// The reason an `ecsProperties` object a job definition carries cannot run on Fargate, or
/// `None` when every member its tasks and their containers state is one Fargate supports.
fn unsupported_ecs_member(ecs: &EcsProperties) -> Option<String> {
    for task in &ecs.task_properties {
        if task
            .ipc_mode
            .as_deref()
            .is_some_and(|ipc_mode| !ipc_mode.is_empty())
        {
            return Some(unsupported_member_detail("ipcMode", false));
        }
        if task
            .containers
            .iter()
            .any(|container| container.privileged == Some(true))
        {
            return Some(unsupported_member_detail("privileged", false));
        }
    }
    None
}

/// The reason a job definition that runs on Fargate cannot be registered as stated, or
/// `None` when it states no member Fargate does not support.
///
/// One traversal of every container-properties object the definition carries — the
/// top-level `containerProperties`, each node range's `container` and `ecsProperties`, and
/// its own `ecsProperties` — so the rule reaches every place such a member can be stated
/// rather than the top-level member alone.
pub fn fargate_unsupported_member_refusal(
    container_properties: Option<&ContainerProperties>,
    node_properties: Option<&NodeProperties>,
    ecs_properties: Option<&EcsProperties>,
) -> Option<String> {
    if let Some(container) = container_properties {
        if let Some(detail) = unsupported_container_member(container) {
            return Some(detail);
        }
    }
    if let Some(node) = node_properties {
        for range in &node.node_range_properties {
            if let Some(detail) = range
                .container
                .as_ref()
                .and_then(unsupported_container_member)
            {
                return Some(detail);
            }
            if let Some(detail) = range.ecs_properties.as_ref().and_then(unsupported_ecs_member) {
                return Some(detail);
            }
        }
    }
    ecs_properties.and_then(unsupported_ecs_member)
}
