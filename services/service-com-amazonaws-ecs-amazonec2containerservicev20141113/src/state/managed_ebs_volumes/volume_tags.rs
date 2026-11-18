// SPDX-License-Identifier: CC0-1.0

//! The tags the managed Amazon EBS volume of one task carries.
//!
//! Its own file because it is the one place the two tag sets meet: the ones the launch's
//! configuration asked for ([`super::configuration_tags`]) and the two Amazon ECS applies
//! itself, which the launch's own specification may not use
//! ([`super::RESERVED_VOLUME_TAG_KEYS`]).

use super::*;

/// The value of the reserved tag that marks the volume as one Amazon ECS manages, as every
/// other resource of an account Amazon ECS manages carries that marker.
const MANAGED_TAG_VALUE: &str = "true";

/// The tags one of these volumes carries: the ones the configuration's `tagSpecifications`
/// asked for, and the two reserved tags Amazon ECS applies itself
/// ([`RESERVED_VOLUME_TAG_KEYS`]: "Amazon ECS applies service-managed tags by default").
///
/// `AmazonECSCreated` names the resource whose launch created the volume — the task — and
/// `AmazonECSManaged` is the marker that the volume is Amazon ECS's to manage.
pub(super) fn volume_tags(wanted: &PendingManagedVolume) -> Vec<Ec2Tag> {
    let [created_key, managed_key] = RESERVED_VOLUME_TAG_KEYS;
    configuration_tags(&wanted.configuration)
        .into_iter()
        .map(|tag| (tag.key.unwrap_or_default(), tag.value.unwrap_or_default()))
        .chain([
            (created_key.to_string(), wanted.task_arn.clone()),
            (managed_key.to_string(), MANAGED_TAG_VALUE.to_string()),
        ])
        .map(|(key, value)| Ec2Tag {
            key: Some(key),
            value: Some(value),
        })
        .collect()
}
