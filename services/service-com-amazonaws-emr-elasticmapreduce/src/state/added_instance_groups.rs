// SPDX-License-Identifier: CC0-1.0

//! Which instance groups a cluster that is ALREADY RUNNING may be given on top of the ones
//! it was launched with.
//!
//! One concern of this service's state, reached from the [`crate::state`] root like every
//! other primitive there, and the sibling of the readings a LAUNCH's groups are held to
//! ([`super::launch_shape_refusals`]): "adds one or more instance groups to a running
//! cluster" (`AddInstanceGroups` doc) states the same capacity in the same
//! `InstanceGroupConfig` shape, so the bounds on one configuration
//! ([`super::validate_instance_group_config`]) and the classifications its configuration
//! tree may carry ([`super::launch_classification_refusal`]) are the ones already in place.
//! What is only true of an ADDITION is here: the groups the cluster already carries decide
//! which ROLES it can still be given one of, and the release it is already running decides
//! which Amazon EC2 instance types those groups may be provisioned from.

use super::*;

/// How Amazon EMR refuses an addition that names no instance group to add.
///
/// `InstanceGroups` is "instance groups to add" and is `@required`
/// (`AddInstanceGroupsInput$InstanceGroups`, Smithy model); the model bounds its length
/// nowhere, so a list that is present and EMPTY reaches the service and names nothing to
/// add. The sentence is fixed and states the member, the form every empty REQUIRED
/// collection of this service is refused by ([`empty_tag_list_message`]).
pub fn empty_instance_group_list_message() -> String {
    "Instance groups cannot be null or empty.".to_string()
}

/// The instance group roles a cluster carries at most ONE group of.
///
/// "A cluster can have one master instance group, one core instance group, and up to 48
/// task instance groups" ("Amazon EMR Management Guide", instance group limits — the same
/// published limit [`MAX_INSTANCE_GROUPS_PER_CLUSTER`] is shaped after): the primary node
/// and the file system each live in a single group, so only the TASK role is one a cluster
/// holds several groups of. "Valid values are MASTER, CORE or TASK"
/// (`InstanceGroupConfig$InstanceRole` doc) is the whole set the roles are drawn from.
pub const SINGLETON_INSTANCE_GROUP_ROLES: &[&str] =
    &[instance_group_type::MASTER, instance_group_type::CORE];

/// How Amazon EMR refuses an addition that would leave a cluster carrying a second
/// instance group of the role `role`.
///
/// This emulator's own sentence, like the other published-limit refusals this service
/// states ([`instance_group_count_violation`]): its one variable part is the role the
/// request named, taken from the request, so a caller reads which group of its list to
/// drop.
pub fn singleton_instance_group_role_message(role: &str) -> String {
    format!("A cluster carries one {role} instance group, which this cluster already holds.")
}

/// The refusal an addition earns for the ROLES it names, or `None` when every group it
/// names is one the cluster can still be given.
///
/// `held` is what the cluster already carries and `added` what the request asks for, so a
/// list that names two groups of a singleton role is refused by the same reading as one
/// that names a second of a role the cluster already holds: what is bounded is what the
/// cluster would be left carrying.
pub fn added_instance_group_role_refusal(
    held: &[InstanceGroupRecord],
    added: &[InstanceGroupConfig],
) -> Option<String> {
    for role in SINGLETON_INSTANCE_GROUP_ROLES {
        let held_groups = held
            .iter()
            .filter(|group| group.config.instance_role == *role)
            .count();
        let added_groups = added
            .iter()
            .filter(|config| config.instance_role == *role)
            .count();
        if held_groups.saturating_add(added_groups) > 1 {
            return Some(singleton_instance_group_role_message(role));
        }
    }
    None
}

/// The first instance type an addition names that the release `release` does not offer, if
/// it names one — the value [`unsupported_instance_type_message`] refuses.
///
/// The release decides which Amazon EC2 instance types a cluster can be provisioned from —
/// "the list of instance types that the release specified … supports"
/// (`ListSupportedInstanceTypesOutput$SupportedInstanceTypes` doc) — and an added group is
/// provisioned on the cluster that is already running ONE release, so its
/// "Amazon EC2 instance type for all instances in the instance group"
/// (`InstanceGroupConfig$InstanceType` doc) is resolved against that release rather than
/// against anything in the request. The types are judged in the order the request states
/// them, the way a launch's are ([`unsupported_launch_instance_type`]), so an addition that
/// gets several wrong is refused for the first.
pub fn unsupported_added_instance_type(
    release: (u32, u32, u32),
    added: &[InstanceGroupConfig],
) -> Option<String> {
    added
        .iter()
        .map(|config| config.instance_type.clone())
        .find(|instance_type| !release_offers_instance_type(release, instance_type))
}
