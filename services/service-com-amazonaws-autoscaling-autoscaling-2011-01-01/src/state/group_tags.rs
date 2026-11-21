// SPDX-License-Identifier: CC0-1.0

//! The tags an Auto Scaling group carries: how one is stored when a request creates it,
//! and the ONE projection every read of it reports.
//!
//! A tag of a group is a CHILD of that group ([`StoredGroup::tags`]), and three of its
//! members are the SERVICE's decision rather than the caller's: which resource the tag is
//! on (`ResourceId`, "the name of the Auto Scaling group"), what kind of resource that is
//! (`ResourceType`, "the only supported value is `auto-scaling-group`") and whether the
//! tag is "added to new instances as they are launched in the group"
//! (`PropagateAtLaunch`). Each is decided when the tag is stored, so each is present on
//! every read of the tag — a tag created without `PropagateAtLaunch` takes the service's
//! own default for the member ([`DEFAULT_PROPAGATE_AT_LAUNCH`]) and stores it, rather than
//! storing nothing at all.
//!
//! That makes [`stored_tag`] the one place a requested tag becomes a stored one and
//! [`tag_description`] the one place a stored tag becomes what the wire reports, so the
//! group's own read, the account-wide tag read and the propagation that happens at launch
//! all see one tag the same way. Every WRITE of a group's tags goes the same way:
//! [`tagged_group_name`] resolves which group one requested tag is on, and
//! [`StoredGroup::put_tag`] / [`StoredGroup::remove_tag`] are what change it, so the create
//! that carries tags and the two tag-writing operations cannot drift apart.

use std::collections::{BTreeMap, BTreeSet};

use crate::ops::types::{Tag, TagDescription};

use super::StoredGroup;

/// The `ResourceType` a tag on an Auto Scaling group is reported under.
///
/// From the model's own documentation for `Tag$ResourceType`: "The only supported value is
/// `auto-scaling-group`."
pub const GROUP_RESOURCE_TYPE: &str = "auto-scaling-group";

/// Whether a tag created without `PropagateAtLaunch` is added to the instances the group
/// launches.
///
/// The flag "determines whether the tag is added to new instances as they are launched in
/// the group" (`Tag$PropagateAtLaunch`), and the model states no default for it
/// (`smithy_query(query="shape",shape="Tag")` carries no `@default` on the member), so the
/// default is the service's own. Observed AWS behaviour decides which it is: a tag passed
/// to `CreateAutoScalingGroup` with no `PropagateAtLaunch` reads back PROPAGATING on every
/// later read of the group and of the account's tags, so a `propagate-at-launch=false`
/// filter then selects only a tag whose creation stated `false`. (CloudFormation's own
/// `Tag` property for the type requires the member, so a declaration always states one;
/// this default is what an API caller that omits it gets.)
pub const DEFAULT_PROPAGATE_AT_LAUNCH: bool = true;

/// The most tags one Auto Scaling group may carry.
///
/// A group's tags are its own records, and the account's quota for them is what keeps one
/// request from growing the store without end: Amazon Web Services publishes a quota of 50
/// tags per resource, and a write that would take a group past it is refused with the
/// modelled `LimitExceededFault` ("You have already reached a limit for your Amazon EC2
/// Auto Scaling resources"), which is the error class the model declares for every writer of
/// a group's tags.
pub const MAX_TAGS_PER_GROUP: usize = 50;

/// Whether the tag is added to the instances the group launches.
///
/// The stored decision, read through one rule: a tag that states nothing takes the
/// service's default ([`DEFAULT_PROPAGATE_AT_LAUNCH`]).
pub fn propagates_at_launch(tag: &Tag) -> bool {
    tag.propagate_at_launch
        .unwrap_or(DEFAULT_PROPAGATE_AT_LAUNCH)
}

/// The tag a group stores for one tag a request carries.
///
/// The caller's own members — the key and the value — are kept as written; the three the
/// service decides are filled in: the tag is reported against THIS group whatever resource
/// the request named, under the one supported resource type, and with the propagation
/// decision the request stated or [`DEFAULT_PROPAGATE_AT_LAUNCH`] when it stated none.
pub fn stored_tag(group_name: &str, tag: Tag) -> Tag {
    Tag {
        resource_id: Some(group_name.to_string()),
        resource_type: Some(GROUP_RESOURCE_TYPE.to_string()),
        key: tag.key,
        value: tag.value,
        propagate_at_launch: Some(
            tag.propagate_at_launch
                .unwrap_or(DEFAULT_PROPAGATE_AT_LAUNCH),
        ),
    }
}

/// The tags a group stores for the tags ONE request carries: each tag as [`stored_tag`]
/// makes it, and ONE entry per key — the last statement of a key wins, exactly as a later
/// write of that key overwrites it ([`StoredGroup::put_tag`]).
///
/// So a group's tag collection is as large as the KEYS a request names rather than as large
/// as the request, and the collection one create leaves behind is the one a sequence of
/// writes of the same tags would leave behind.
pub fn stored_tags(group_name: &str, tags: Vec<Tag>) -> Vec<Tag> {
    let mut stored: BTreeMap<String, Tag> = BTreeMap::new();
    for tag in tags {
        let tag = stored_tag(group_name, tag);
        stored.insert(tag.key.clone(), tag);
    }
    stored.into_values().collect()
}

/// The `TagDescription` the service reports for a stored tag — THE projection of a group
/// tag onto the wire, so the group's own read and the account-wide tag read report one tag
/// identically.
pub fn tag_description(tag: &Tag) -> TagDescription {
    TagDescription {
        resource_id: tag.resource_id.clone(),
        resource_type: tag.resource_type.clone(),
        key: Some(tag.key.clone()),
        value: tag.value.clone(),
        // A tag stored before this member was a stored decision still reports one: the
        // projection answers the default rather than an absent member, because the flag
        // decides inheritance for every tag of a group.
        propagate_at_launch: Some(
            tag.propagate_at_launch
                .unwrap_or(DEFAULT_PROPAGATE_AT_LAUNCH),
        ),
    }
}

impl StoredGroup {
    /// Every tag of this group as the wire reports it, in the order the group holds them.
    pub fn tag_descriptions(&self) -> Vec<TagDescription> {
        self.tags.iter().map(tag_description).collect()
    }

    /// Store one tag on this group, as [`stored_tag`] makes it: a key the group already
    /// carries is OVERWRITTEN in place ("when you specify a tag with a key that already
    /// exists, the operation overwrites the previous tag definition, and you do not get an
    /// error message"), and a key it does not carry is added.
    ///
    /// A tag of a group is identified by its key alone, so the stored value and the
    /// propagation decision are the ones the latest write of that key stated.
    pub fn put_tag(&mut self, tag: Tag) {
        let tag = stored_tag(&self.name, tag);
        match self.tags.iter_mut().find(|existing| existing.key == tag.key) {
            Some(existing) => *existing = tag,
            None => self.tags.push(tag),
        }
    }

    /// Remove the tag `key` names from this group, answering whether the group was carrying
    /// one.
    ///
    /// A key the group does not carry leaves it exactly as it was: a tag is removed by its
    /// key, so removing one twice removes it once.
    pub fn remove_tag(&mut self, key: &str) -> bool {
        let before = self.tags.len();
        self.tags.retain(|tag| tag.key != key);
        self.tags.len() != before
    }

    /// How many tags this group would carry once the keys `keys` were written to it.
    ///
    /// A key the group already carries is overwritten rather than added, so writing it again
    /// does not grow the group: the count is what the quota ([`MAX_TAGS_PER_GROUP`]) is
    /// checked against, for the whole request at once rather than one tag at a time.
    ///
    /// The keys the group holds are read into a SET once and the keys the request adds are
    /// collected into another, so the count costs one pass over each rather than a scan per
    /// pair, and the pass STOPS as soon as the quota is crossed — what the caller gets back
    /// is the same refusal whether it asked for one tag too many or a thousand.
    pub fn tag_count_after<'a>(&self, keys: impl Iterator<Item = &'a str>) -> usize {
        let held: BTreeSet<&str> = self.tags.iter().map(|tag| tag.key.as_str()).collect();
        let mut added: BTreeSet<&str> = BTreeSet::new();
        for key in keys {
            if !held.contains(key) {
                added.insert(key);
                if held.len().saturating_add(added.len()) > MAX_TAGS_PER_GROUP {
                    break;
                }
            }
        }
        held.len().saturating_add(added.len())
    }
}

/// Why a request that writes a group's tags cannot be applied. Each variant states what
/// about the requested tag could not be resolved; [`TagWriteRefusal::message`] words it.
#[derive(Debug, Clone)]
pub enum TagWriteRefusal {
    /// The tag names no resource at all, so there is no group to write it to.
    NoResourceId,
    /// The tag names a kind of resource this service does not tag.
    UnsupportedResourceType(String),
    /// The tag names a group the account does not hold.
    AbsentGroup(String),
}

impl TagWriteRefusal {
    /// The sentence this refusal is reported with.
    pub fn message(&self) -> String {
        match self {
            // A tag is written to the group its `ResourceId` names, so a tag carrying none
            // names no group: the absent-group refusal of this service with its detail
            // field unfilled, because there is no name to state in it.
            Self::NoResourceId => super::ABSENT_GROUP_MESSAGE_WITHOUT_DETAIL.to_string(),
            // The refusal class this service states for a request naming something an
            // operation does not accept: what was named, and what is accepted.
            Self::UnsupportedResourceType(resource_type) => format!(
                "Resource type {resource_type} is not correct. Allowed Resource types are: \
                 {GROUP_RESOURCE_TYPE}"
            ),
            // A group the account does not hold carries no tags, so the write is refused
            // with this service's absent-group refusal in the wording BOTH writers of a
            // group's tags state it in: the opening is about the group that was not found,
            // and the detail names the resource as two words and then the name asked for
            // ([`super::absent_group_message_as_subject_naming_group`]).
            Self::AbsentGroup(name) => super::absent_group_message_as_subject_naming_group(name),
        }
    }
}

/// The most tags ONE call that writes a group's tags may carry.
///
/// How many tags one request may state is a fact of the REQUEST's shape, which is why this
/// service's other batch writes carry the same kind of per-call bound
/// (`MAX_INSTANCE_IDS_PER_CALL`, `MAX_TARGET_GROUPS_PER_CALL`) and refuse past it with the
/// service-common `ValidationError` before the batch is looked at at all. A group carries
/// at most [`MAX_TAGS_PER_GROUP`] tags, so a call stating more than that many is past every
/// quota it could be applied against.
pub const MAX_TAGS_PER_CALL: usize = 50;

/// The message a call naming more tags than [`MAX_TAGS_PER_CALL`] is refused with, which
/// quotes the bound the caller has to come back under — the per-call sentence class this
/// service's other batch writes state.
pub fn too_many_tags_in_one_call_message() -> String {
    format!("Trying to update too many tags at once. The limit is {MAX_TAGS_PER_CALL}")
}

/// The message a write that would take a group past the tag quota is refused with: the
/// group it would have grown and the quota it would have crossed, which is what the caller
/// has to come back under.
pub fn too_many_tags_message(group_name: &str) -> String {
    format!(
        "You have already reached the limit of {MAX_TAGS_PER_GROUP} tags for Auto Scaling \
         group {group_name}"
    )
}

/// The keys a request would write to each group it names, in ONE pass over the request.
///
/// What the per-group quota is checked against: the request is read once, so the work is
/// the number of tags it carries rather than that number times the groups it names.
pub fn requested_keys_by_group(tags: &[Tag]) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut by_group: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for tag in tags {
        let Some(resource_id) = tag.resource_id.as_deref().filter(|id| !id.is_empty()) else {
            continue;
        };
        by_group
            .entry(resource_id)
            .or_default()
            .insert(tag.key.as_str());
    }
    by_group
}

/// The groups a tag-writing request names, each once.
///
/// A request writes tags onto the groups its tags' `ResourceId`s name, so those are the
/// resources the action is judged against. A SET, built in one pass: a tag naming no
/// resource names no group here (the write itself refuses it, [`tagged_group_name`]), and a
/// group named by many tags is one resource.
pub fn named_tag_resources(tags: &[Tag]) -> BTreeSet<&str> {
    tags.iter()
        .filter_map(|tag| tag.resource_id.as_deref())
        .filter(|resource_id| !resource_id.is_empty())
        .collect()
}

/// The group one tag of a tag-writing request applies to, or why it applies to none.
///
/// THE ONE reading of a requested tag's target for both writers of a group's tags: a tag
/// names the group it is on (`ResourceId`, "the name of the Auto Scaling group") and the
/// kind of resource that is (`ResourceType`, "the only supported value is
/// `auto-scaling-group`"), and it is written only to a group the account holds — so a
/// request naming several groups is taken against every one of them before any of them is
/// changed.
pub fn tagged_group_name(
    groups: &BTreeMap<String, StoredGroup>,
    tag: &Tag,
) -> Result<String, TagWriteRefusal> {
    if let Some(resource_type) = tag.resource_type.as_deref() {
        if resource_type != GROUP_RESOURCE_TYPE {
            return Err(TagWriteRefusal::UnsupportedResourceType(
                resource_type.to_string(),
            ));
        }
    }
    let name = tag
        .resource_id
        .as_deref()
        .filter(|resource_id| !resource_id.is_empty())
        .ok_or(TagWriteRefusal::NoResourceId)?;
    if !groups.contains_key(name) {
        return Err(TagWriteRefusal::AbsentGroup(name.to_string()));
    }
    Ok(name.to_string())
}
