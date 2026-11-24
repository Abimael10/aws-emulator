// SPDX-License-Identifier: CC0-1.0

//! The tags a LAUNCH asks for on the resources it creates, read per resource type.
//!
//! ## The rule
//!
//! "The tags to apply to the resources that are created during instance launch"
//! (`RunInstancesRequest$TagSpecifications`, and the same member on
//! `RequestLaunchTemplateData`): each specification tags the resources of ITS OWN
//! `ResourceType`, so one reading answers "what does this launch tag the resources of this
//! type with" for every type a launch creates a resource of — the instance, the EBS volume
//! and the network interfaces it creates. A launch therefore applies a specification's tags
//! to the resources it creates of that type and to nothing else: an `instance` entry never
//! reaches the volume, and a `volume` entry never reaches the instance.
//!
//! One walk for both shapes a launch carries its specifications in, so a launch made
//! directly, a launch made from a launch-template version and a launch another service makes
//! on the account's behalf cannot disagree about which tags land on which resource. A tag
//! with no key names nothing, so it is dropped; one with no value carries the empty value,
//! the way every tag of this service does; and a key given twice for one type keeps the last
//! value, there being no defined winner otherwise.

use std::collections::BTreeMap;

use crate::ops::types::{
    LaunchTemplateTagSpecificationRequest, RequestLaunchTemplateData, Tag, TagSpecification,
};

/// The tags `specifications` names for the resources of `resource_type`, as the stored tag
/// map, ordered by key.
///
/// Entries of every other type are left for the reading of THEIR type: a launch creates
/// resources of several kinds and each carries only the tags its own entries name.
pub fn launch_resource_tags(
    specifications: &[TagSpecification],
    resource_type: &str,
) -> BTreeMap<String, String> {
    tags_of_resource_type(
        specifications
            .iter()
            .map(|specification| (specification.resource_type.as_deref(), &specification.tags)),
        resource_type,
    )
}

/// The tags one launch-template VERSION names for the resources of `resource_type` a launch
/// from it creates — the same reading over that version's own specifications, so a version's
/// tags reach a resource exactly as a request's do.
pub fn launch_template_resource_tags(
    data: &RequestLaunchTemplateData,
    resource_type: &str,
) -> BTreeMap<String, String> {
    tags_of_resource_type(
        data.tag_specifications
            .iter()
            .flatten()
            .map(|specification: &LaunchTemplateTagSpecificationRequest| {
                (specification.resource_type.as_deref(), &specification.tags)
            }),
        resource_type,
    )
}

/// The ONE walk both readings above make: every `(ResourceType, Tags)` entry of a launch,
/// merged into the tag map of the resources of `resource_type`.
fn tags_of_resource_type<'spec>(
    entries: impl Iterator<Item = (Option<&'spec str>, &'spec Option<Vec<Tag>>)>,
    resource_type: &str,
) -> BTreeMap<String, String> {
    let mut tags = BTreeMap::new();
    for (named, entry_tags) in entries {
        if named != Some(resource_type) {
            continue;
        }
        for tag in entry_tags.iter().flatten() {
            let Some(key) = tag.key.clone() else { continue };
            tags.insert(key, tag.value.clone().unwrap_or_default());
        }
    }
    tags
}
