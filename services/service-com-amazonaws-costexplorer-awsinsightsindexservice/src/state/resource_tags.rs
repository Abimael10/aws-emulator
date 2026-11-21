// SPDX-License-Identifier: CC0-1.0

//! The tagging surface's shared rules: where one resource's tag set lives, how the tags a
//! request carries are normalised before they are stored, and the refusals a tagging
//! operation answers an ARN it cannot resolve with.
//!
//! One tag set per resource, held on the resource's own record in whichever registry owns
//! it, so `TagResource` writes and `ListTagsForResource` reads the SAME set the create
//! that minted the resource seeded. The resolution and the merge live here rather than in
//! a handler because both operations — and any further tagging operation — have to agree
//! on what a resource's tag set is; duplicating that across handlers is how they would
//! drift apart.

use crate::ops::types::{ResourceNotFoundException, ResourceTag, TooManyTagsException};

use super::ComAmazonawsCostexplorerAwsinsightsindexserviceState;

/// How many tags a caller may hold on one resource.
///
/// From `TagResourceRequest$ResourceTags`: "Although the maximum number of array members
/// is 200, you can assign a maximum of 50 user-tags to one resource. The remaining are
/// reserved for Amazon Web Services use". `TooManyTagsException` is the modelled failure
/// for passing it ("a number of tags for a resource greater than the maximum 50 user tags
/// per resource"), so the 200 the list's `@length` admits is the wire bound and this is
/// the per-resource one.
pub const MAX_USER_TAGS: usize = 50;

/// The tag-key prefix reserved for Amazon Web Services' own tags.
///
/// From `TagResourceRequest$ResourceTags`: "Don't use `aws:` as a prefix for your keys.
/// This prefix is reserved for Amazon Web Services use". The prefix satisfies the
/// modelled `ResourceTagKey` pattern, so the boundary validator admits such a key and the
/// rule is enforced here.
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// The sentence a caller-supplied tag in the reserved key namespace is refused with.
///
/// Wholly FIXED text for this refusal class — it names neither the key, nor the prefix,
/// nor the resource — and the wording Amazon Web Services' tagging surface answers a
/// request that tries to write a system tag with.
const SYSTEM_TAG_REFUSAL: &str = "System tags cannot be added/updated by requester";

/// The sentence a tagging operation refuses an unresolvable `ResourceArn` with.
///
/// Wholly FIXED text for this refusal class — it names neither the ARN, nor the resource
/// type, nor the account — and the wording Amazon Web Services' Cost Explorer tagging
/// surface answers an ARN it resolves to nothing with. It is deliberately NOT
/// `ResourceNotFoundException`'s documentation sentence ("The specified ARN in the request
/// doesn't exist."): a shape's doc string describes the error to a reader of the model, it
/// is not the message the service puts on the wire.
const RESOURCE_NOT_FOUND: &str = "Resource not found";

/// The sentence a tag set larger than [`MAX_USER_TAGS`] is refused with. The limit is the
/// rule's own published number, read off the constant rather than written out.
fn user_tag_maximum_refusal() -> String {
    format!("You can assign a maximum of {MAX_USER_TAGS} user-tags to one resource")
}

/// Normalise the tags a tagging or create request carries into the set to store.
///
/// Keys and values are TRIMMED of leading and trailing whitespace, and a key that repeats
/// within one request keeps the LAST value given for it, so the result holds each key
/// once — both rules are `TagResourceRequest$ResourceTags`': "Keys and values are trimmed
/// for any leading or trailing whitespaces" and "each key must be unique for the
/// resource".
///
/// `Err` carries the refusal message for a request this service does not admit: a key in
/// the reserved namespace, or one that is nothing but whitespace (trimming it leaves a key
/// shorter than the one character `ResourceTagKey`'s `@length` requires).
pub fn normalize_resource_tags(tags: &[ResourceTag]) -> Result<Vec<ResourceTag>, String> {
    let mut normalized: Vec<ResourceTag> = Vec::with_capacity(tags.len());
    for tag in tags {
        let key = tag.key.trim();
        let value = tag.value.trim();
        // Matched exactly as the member documents the prefix: tag keys are case sensitive
        // ("Keys and values are case sensitive"), so `aws:` is the reserved namespace and a
        // key differing from it in case is an ordinary user key.
        if key.starts_with(RESERVED_TAG_KEY_PREFIX) {
            return Err(SYSTEM_TAG_REFUSAL.to_string());
        }
        if key.is_empty() {
            return Err(
                "ResourceTags: a tag key must hold at least one character once its leading \
                 and trailing whitespace is trimmed"
                    .to_string(),
            );
        }
        let tag = ResourceTag {
            key: key.to_string(),
            value: value.to_string(),
        };
        match normalized.iter_mut().find(|held| held.key == tag.key) {
            Some(held) => *held = tag,
            None => normalized.push(tag),
        }
    }
    Ok(normalized)
}

/// Normalise the tags a CREATE seeds a resource's tag set with: the same rule as
/// [`normalize_resource_tags`], and the per-resource maximum as well.
///
/// A create reaches that maximum in ONE request, because `ResourceTagList`'s `@length`
/// admits 200 members while a resource holds at most [`MAX_USER_TAGS`] of them — so
/// without this a resource could be created already holding a tag set `TagResource` says
/// cannot exist. `TooManyTagsException` is modelled by the tagging operation alone, so a
/// create past the maximum is refused through the validation channel instead, carrying the
/// same sentence.
pub fn normalize_created_resource_tags(tags: &[ResourceTag]) -> Result<Vec<ResourceTag>, String> {
    let normalized = normalize_resource_tags(tags)?;
    if normalized.len() > MAX_USER_TAGS {
        return Err(user_tag_maximum_refusal());
    }
    Ok(normalized)
}

/// The tag set a resource carries after `incoming` is added to the `stored` one.
///
/// From `TagResource`'s own documentation: "If you specify a new tag key for the resource,
/// this tag is appended to the list of tags associated with the resource. If you specify a
/// tag key that is already associated with the resource, the new tag value you specify
/// replaces the previous value for that tag." So the merge keeps the stored order, writes
/// over the value of a key already held, and appends the keys that are new — it never
/// drops a key the request does not name.
pub fn merged_tags(stored: &[ResourceTag], incoming: &[ResourceTag]) -> Vec<ResourceTag> {
    let mut merged = stored.to_vec();
    for tag in incoming {
        match merged.iter_mut().find(|held| held.key == tag.key) {
            Some(held) => held.value = tag.value.clone(),
            None => merged.push(tag.clone()),
        }
    }
    merged
}

/// The tag set a resource carries once the keys `removed` names are dropped from the
/// `stored` one.
///
/// From `UntagResource`'s `ResourceTagKeys`: "If you specify a tag key that doesn't exist,
/// it's ignored" — so a key the resource does not hold removes nothing and the call is
/// still served. A key is matched after the same trimming a write applies, so the key a
/// caller removes is the key a caller can set.
pub fn tags_without_keys(stored: &[ResourceTag], removed: &[String]) -> Vec<ResourceTag> {
    stored
        .iter()
        .filter(|held| !removed.iter().any(|key| key.trim() == held.key))
        .cloned()
        .collect()
}

/// The refusal a tagging operation answers an ARN this account holds no resource under.
///
/// `ResourceName` is the modelled member for the ARN the request named, so the ARN is
/// reported there rather than interpolated into the sentence, which is fixed.
pub fn unknown_tagged_resource(resource_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(RESOURCE_NOT_FOUND.to_string()),
        resource_name: Some(resource_arn.to_string()),
    }
}

/// The refusal `TagResource` answers a request that would take a resource past
/// [`MAX_USER_TAGS`] tags with. The limit is the rule's own published number; the resource
/// the request named is reported in `ResourceName`.
pub fn too_many_tags(resource_arn: &str) -> TooManyTagsException {
    TooManyTagsException {
        message: Some(user_tag_maximum_refusal()),
        resource_name: Some(resource_arn.to_string()),
    }
}

impl ComAmazonawsCostexplorerAwsinsightsindexserviceState {
    /// The tag set stored on the resource `resource_arn` names, or `None` when this
    /// account holds no such resource.
    ///
    /// Resolved across EVERY registry the tagging surface covers, which `ResourceTag`
    /// documents as "Tagging is supported only for the following Cost Explorer resource
    /// types: `AnomalyMonitor`, `AnomalySubscription`, `CostCategory`" — so an ARN naming
    /// anything else resolves to nothing, exactly as one naming a resource that was never
    /// created does. An ENDED cost category definition (one the registry only retains as a
    /// tombstone so a read at a date inside its lifetime can still be answered) is not a
    /// resource the account holds either.
    pub fn resource_tags(&self, resource_arn: &str) -> Option<&Vec<ResourceTag>> {
        if let Some(monitor) = self.anomaly_monitors.get(resource_arn) {
            return Some(&monitor.resource_tags);
        }
        if let Some(subscription) = self.anomaly_subscriptions.get(resource_arn) {
            return Some(&subscription.resource_tags);
        }
        self.cost_categories
            .get(resource_arn)
            .filter(|definition| definition.effective_end.is_none())
            .map(|definition| &definition.resource_tags)
    }

    /// The same tag set, for a tagging operation that writes it. Resolves exactly as
    /// [`resource_tags`](Self::resource_tags) does, so a write and a read of one ARN can
    /// never reach different resources.
    pub fn resource_tags_mut(&mut self, resource_arn: &str) -> Option<&mut Vec<ResourceTag>> {
        if let Some(monitor) = self.anomaly_monitors.get_mut(resource_arn) {
            return Some(&mut monitor.resource_tags);
        }
        if let Some(subscription) = self.anomaly_subscriptions.get_mut(resource_arn) {
            return Some(&mut subscription.resource_tags);
        }
        self.cost_categories
            .get_mut(resource_arn)
            .filter(|definition| definition.effective_end.is_none())
            .map(|definition| &mut definition.resource_tags)
    }
}
