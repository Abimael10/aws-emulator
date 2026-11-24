// SPDX-License-Identifier: CC0-1.0

//! The instance tags a launch-template VERSION declares — the tags every launch made from
//! that version applies to the instance it creates.
//!
//! ## The rule
//!
//! A launch template version carries the tags to apply to the resources a launch from it
//! creates — "the tags to apply to the resources that are created during instance launch"
//! (`RequestLaunchTemplateData$TagSpecifications`) — and each entry tags the resources of
//! its own `ResourceType`, so the `instance` entries are the instance's own tags. Which
//! entries reach which resource is the one reading beside this
//! (`super::launch_resource_tags`), so the volume and the interfaces a launch from a version
//! creates take their tags from the same walk.
//!
//! Launch templates are this service's, so this is where that reading lives: a caller's own
//! `RunInstances` naming a template takes them through the overlay the launch applies to the
//! request it was given (`crate::ops::run_instances`), and the launch Amazon EC2 Auto Scaling
//! makes for a group's capacity reads them from here
//! (`service-com-amazonaws-autoscaling-autoscaling-2011-01-01`'s `state::ec2_instances`), so
//! one version's tags reach the instance the same way whoever made the launch. The two
//! reserved tags naming the template and the resolved version a launch read are
//! [`crate::state::LAUNCH_TEMPLATE_ID_TAG`] and its sibling, beside this.

use std::collections::BTreeMap;

use crate::ops::types::RequestLaunchTemplateData;
use crate::state::{launch_template_resource_tags, RESOURCE_TYPE_INSTANCE};

/// The instance tags one launch-template version declares: the tags of its
/// `TagSpecifications` entries whose `ResourceType` is `instance`, and no others.
///
/// An entry for another documented type names a resource that is not the instance (the
/// volume or the network interfaces a launch creates, a Spot Instance request), so its tags
/// must not land on the instance — each of those is read for its own type through
/// [`launch_template_resource_tags`] — and an entry naming no type names no resource at all.
pub fn launch_template_instance_tags(
    data: &RequestLaunchTemplateData,
) -> BTreeMap<String, String> {
    launch_template_resource_tags(data, RESOURCE_TYPE_INSTANCE)
}
