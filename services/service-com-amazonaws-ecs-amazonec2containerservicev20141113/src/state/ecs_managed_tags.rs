// SPDX-License-Identifier: CC0-1.0

//! The tags Amazon ECS adds to a task OF ITS OWN, beside whatever the placement propagated
//! or the request carried.
//!
//! `enableECSManagedTags` is "Specifies whether to use Amazon ECS managed tags for the
//! task" — a second tag source, independent of `propagateTags`: with it on, Amazon ECS tags
//! the task it places with the resources that task belongs to, under the `aws:` prefix it
//! keeps for its own tags (which is why a caller may not send one,
//! [`reserved_tag_prefix_violation`](super::reserved_tag_prefix_violation)). Every task
//! belongs to a cluster, and a task a service placed belongs to that service as well, so
//! those are the two keys.
//!
//! One source for every placement, so a task carries the same managed tags whoever placed
//! it: a client's own `RunTask`/`StartTask`, a service's scheduler, or a daemon's.

use super::Tag;

/// The managed tag naming the CLUSTER a task runs in, whose value is the cluster's name.
pub const MANAGED_TAG_CLUSTER_NAME: &str = "aws:ecs:clusterName";

/// The managed tag naming the SERVICE a task belongs to, whose value is the service's name.
/// A task no service placed belongs to none, so it carries no such tag.
pub const MANAGED_TAG_SERVICE_NAME: &str = "aws:ecs:serviceName";

/// `tags` with the managed tags of a task in `cluster_name` — and of `service_name` when a
/// service placed it — added, or `tags` unchanged when the placement did not ask for them.
///
/// A managed tag is Amazon ECS's own, so it is added whatever the task already carries: a
/// caller cannot have written one ("you can't use the `aws:` prefix for either keys or
/// values"), and a key that somehow is on the task already is replaced rather than repeated
/// ("each tag key must be unique, and each tag key can have only one value").
pub fn with_ecs_managed_tags(
    tags: Vec<Tag>,
    enabled: bool,
    cluster_name: &str,
    service_name: Option<&str>,
) -> Vec<Tag> {
    if !enabled {
        return tags;
    }
    let managed = [
        Some((MANAGED_TAG_CLUSTER_NAME, cluster_name.to_string())),
        service_name.map(|name| (MANAGED_TAG_SERVICE_NAME, name.to_string())),
    ];
    let mut tagged: Vec<Tag> = tags
        .into_iter()
        .filter(|tag| {
            !managed
                .iter()
                .flatten()
                .any(|(key, _)| tag.key.as_deref() == Some(*key))
        })
        .collect();
    tagged.extend(managed.into_iter().flatten().map(|(key, value)| Tag {
        key: Some(key.to_string()),
        value: Some(value),
    }));
    tagged
}
