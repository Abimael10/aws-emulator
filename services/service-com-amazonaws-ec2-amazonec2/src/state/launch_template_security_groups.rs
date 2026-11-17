// SPDX-License-Identifier: CC0-1.0

//! The security groups a launch-template VERSION declares — the groups every launch made
//! from that version applies to the instance it creates.
//!
//! ## The rule
//!
//! A launch names the security groups of its instance in ONE of two places, never both: the
//! version's own `SecurityGroupIds` / `SecurityGroups`, or the `Groups` of the network
//! interfaces the version declares — "if you specify a network interface, you must specify
//! any security groups as part of the network interface instead of using this parameter"
//! (`RequestLaunchTemplateData$SecurityGroupIds`). An instance's groups are the ones applied
//! to its PRIMARY network interface, which is the entry at device index 0 — "the primary
//! network interface has a device index of 0"
//! (`LaunchTemplateInstanceNetworkInterfaceSpecificationRequest$DeviceIndex`), the index an
//! entry that names none takes.
//!
//! So a version that declares its interfaces declares its groups THERE, and reading only the
//! version's own two members leaves such a launch naming no group at all — which gives the
//! instance its VPC's default group instead of the declared ones.
//!
//! Launch templates are this service's, so this is where that reading lives, beside the
//! instance tags one version declares (`super::launch_template_instance_tags`): a caller's
//! own `RunInstances` naming a template reaches the groups through the interfaces the overlay
//! reads them into (`crate::ops::run_instances`), and the launch Amazon EC2 Auto Scaling
//! makes for a group's capacity reads them from here
//! (`service-com-amazonaws-autoscaling-autoscaling-2011-01-01`'s `state::ec2_instances`), so
//! one version's groups reach the instance whoever made the launch.

use crate::ops::types::{
    LaunchTemplateInstanceNetworkInterfaceSpecificationRequest, RequestLaunchTemplateData,
};
use crate::state::PRIMARY_NETWORK_INTERFACE_DEVICE_INDEX;

/// The security groups one launch-template version declares for the instance a launch from it
/// creates, as the `(group id, group name)` pairs a launch carries — the form a group may be
/// named in, exactly as a launch names it.
///
/// A version that declares network interfaces declares its groups on its PRIMARY interface
/// ([`primary_launch_template_interface`]), whose `Groups` are ids ("the IDs of one or more security groups",
/// `LaunchTemplateInstanceNetworkInterfaceSpecificationRequest$Groups`); one that declares
/// none declares them on its own members, where `SecurityGroupIds` names ids and
/// `SecurityGroups` names names.
///
/// EMPTY where the version names no group in the place that applies — a launch that names
/// none is given its VPC's default group, which is what
/// [`crate::state::ComAmazonawsEc2Amazonec2State::default_security_group_of_vpc`] gives it. A
/// reference with no text names nothing, so it is not carried as a group.
pub fn launch_template_security_groups(
    data: &RequestLaunchTemplateData,
) -> Vec<(Option<String>, Option<String>)> {
    let declared = data.network_interfaces.as_deref().unwrap_or_default();
    if !declared.is_empty() {
        return primary_launch_template_interface(declared)
            .map(|interface| {
                interface
                    .groups
                    .iter()
                    .flatten()
                    .filter(|group_id| !group_id.is_empty())
                    .map(|group_id| (Some(group_id.clone()), None))
                    .collect()
            })
            .unwrap_or_default();
    }
    data.security_group_ids
        .iter()
        .flatten()
        .filter(|group_id| !group_id.is_empty())
        .map(|group_id| (Some(group_id.clone()), None))
        .chain(
            data.security_groups
                .iter()
                .flatten()
                .filter(|group_name| !group_name.is_empty())
                .map(|group_name| (None, Some(group_name.clone()))),
        )
        .collect()
}

/// The PRIMARY network interface among the ones a version declares: the entry at device index
/// 0, which is the index an entry that names none takes.
///
/// `None` for a list that declares no primary interface at all — "if you create a launch
/// template that includes secondary network interfaces but not a primary network interface,
/// then you must add a primary network interface as a launch parameter when you launch an
/// instance from the template" (`$DeviceIndex`), so such a version says nothing about the
/// instance's own interface and nothing is read off one of its secondary entries.
pub fn primary_launch_template_interface(
    interfaces: &[LaunchTemplateInstanceNetworkInterfaceSpecificationRequest],
) -> Option<&LaunchTemplateInstanceNetworkInterfaceSpecificationRequest> {
    interfaces.iter().find(|interface| {
        interface
            .device_index
            .unwrap_or(PRIMARY_NETWORK_INTERFACE_DEVICE_INDEX)
            == PRIMARY_NETWORK_INTERFACE_DEVICE_INDEX
    })
}
