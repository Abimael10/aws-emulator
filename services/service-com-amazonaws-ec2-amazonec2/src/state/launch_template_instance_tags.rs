// SPDX-License-Identifier: CC0-1.0

//! The instance tags a launch-template VERSION declares — the tags every launch made from
//! that version applies to the instance it creates.
//!
//! ## The rule
//!
//! A launch template version carries the tags to apply to the resources a launch from it
//! creates — "the tags to apply to the resources that are created during instance launch"
//! (`RequestLaunchTemplateData$TagSpecifications`) — and each entry tags the resources of
//! its own `ResourceType`, so the `instance` entries are the instance's own tags.
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
use crate::state::RESOURCE_TYPE_INSTANCE;

/// The instance tags one launch-template version declares: the tags of its
/// `TagSpecifications` entries whose `ResourceType` is `instance`, and no others.
///
/// An entry for another documented type names a resource that is not the instance (the
/// volume or the network interfaces a launch creates, a Spot Instance request), so its tags
/// must not land on the instance, and an entry naming no type names no resource at all. A
/// tag with no key names nothing; one with no value carries the empty value, the way every
/// tag of this service does.
pub fn launch_template_instance_tags(
    data: &RequestLaunchTemplateData,
) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    for specification in data.tag_specifications.iter().flatten() {
        if specification.resource_type.as_deref() != Some(RESOURCE_TYPE_INSTANCE) {
            continue;
        }
        for tag in specification.tags.iter().flatten() {
            let Some(key) = tag.key.clone() else { continue };
            tags.insert(key, tag.value.clone().unwrap_or_default());
        }
    }
    tags
}
