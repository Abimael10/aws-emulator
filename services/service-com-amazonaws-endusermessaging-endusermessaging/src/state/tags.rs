// SPDX-License-Identifier: CC0-1.0

//! The resource a `resourceArn` names, and the tags it carries.
//!
//! Every tagging operation of this service addresses its resource the same way: by
//! the full ARN of a resource of this service's own namespace, in the Region the
//! request was made in. Resolving that ARN lives here rather than in one handler, so
//! a tag read and a tag write cannot disagree about which resource an ARN names, or
//! about how a value that names none is refused.
//!
//! What a request may ASK to write lives here for the same reason: a `TagList` arrives
//! on a tag write and on every create that takes tags, and the rules a tag set has to
//! satisfy — the reserved key namespace, one value per key, the per-resource ceiling —
//! are the same wherever it arrived, so they are stated once ([`requested_tags`],
//! [`ComAmazonawsEndusermessagingEndusermessagingState::write_tags`]).
//!
//! The ARN itself is read by the workspace's one ARN parse,
//! [`::service_common::arn`], so this service does not decide for itself what an ARN
//! is: the fields, the resource type and the id of a resource of that type all come
//! from there. What belongs here is only what is this service's own — which vendor
//! its ARNs carry, which stored resources a tag call can reach, and how each refusal
//! is worded.

use super::{resource_not_found, ComAmazonawsEndusermessagingEndusermessagingState, ARN_NAMESPACE};

/// A stored resource that carries tags, as a `resourceArn` resolves to it.
///
/// The variants are this service's taggable resource types; each holds the resource's
/// own identifier, so the tags are read off the one stored record rather than off a
/// copy taken while the ARN was resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggedResource {
    /// A brand profile, by its `brandProfileId`.
    BrandProfile(String),
    /// A notify code configuration, by its `notifyCodeConfigurationId`.
    NotifyCodeConfiguration(String),
}

/// Why an identifier a request gave reaches no stored resource, as the refusal the
/// operation naming it answers with.
///
/// The distinction is the client-visible one: a value that cannot address a resource
/// of this request at all is a bad request, which no later state can make good, while
/// a well-formed identifier this state holds no record for is a missing resource.
#[derive(Debug)]
pub enum UnreachableResource {
    /// The value cannot address a resource of this request at all: it is not an
    /// identifier or ARN of this resource, it names another vendor, or it is scoped to
    /// another Region. `ValidationException`, HTTP 400.
    Invalid(crate::ops::types::ValidationException),
    /// A well-formed identifier of this resource that names no stored record.
    /// `ResourceNotFoundException`, HTTP 404.
    NotFound(crate::ops::types::ResourceNotFoundException),
}

/// `ValidationException` for a value that is not an ARN of a resource this service's
/// API can address: not a well-formed ARN at all, or one naming another vendor.
///
/// Both read the same way to a caller — the value cannot name a resource of this
/// namespace — so this service words them as one refusal, which is what lets the
/// shared parse answer both ([`::service_common::arn::parse`] requires the six
/// fields, the literal `arn`, and a non-empty partition, vendor and resource). The
/// fixed text is this refusal's; the only variable part is the value the request
/// gave, and `fieldList` points at the member it came in on.
fn invalid_resource_arn(resource_arn: &str) -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: format!(
            "Invalid ARN: {resource_arn} is not the ARN of an {ARN_NAMESPACE} resource"
        ),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: RESOURCE_ARN_POINTER.to_string(),
            message: format!(
                "The value must be an Amazon Resource Name of the form \
                 arn:<partition>:{ARN_NAMESPACE}:<region>:<account-id>:<resource>"
            ),
        }]),
    }
}

/// `ValidationException` for an ARN of this service that names a resource of a Region
/// other than the one the request was made in.
///
/// Worded as a sentence, the way this API words a refusal its model does not express
/// (an unissued pagination token, an update carrying nothing to update), rather than
/// in the boundary validator's constraint envelope, which states a modelled
/// constraint this value does not break. The fixed text is this refusal's; the
/// variable parts are the value the request gave and the Region it was made in.
fn cross_region_resource_arn(
    resource_arn: &str,
    request_region: &str,
) -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: format!(
            "Invalid ARN: {resource_arn} does not name a resource in Region \
             {request_region}, the Region this request was made in"
        ),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: RESOURCE_ARN_POINTER.to_string(),
            message: format!(
                "The resource ARN must name a resource in Region {request_region}, the Region \
                 this request was made in"
            ),
        }]),
    }
}

/// The JSON pointer naming the member a refused `resourceArn` arrived on, as
/// `ValidationExceptionField$path` documents it.
const RESOURCE_ARN_POINTER: &str = "/resourceArn";

/// The one failure that is not about the request: an ARN resolved to a resource this
/// state then had no record of, which the lock the resolution and the read share
/// makes impossible. Reported rather than passed off as a resource carrying no tags.
pub const RESOLVED_RESOURCE_VANISHED: &str =
    "the resource this request named was resolved but holds no record";

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// The stored resource whose tags `resource_arn` names, for a request made in
    /// `region`.
    ///
    /// An ARN addresses a resource of this service's own namespace, in the request's
    /// own Region: a value that is not such an ARN, or one naming another Region, can
    /// name no resource of this request and is refused as invalid input before any
    /// lookup; an ARN of this service and Region that this emulator never minted — a
    /// resource type it does not hold, an unknown identifier, a resource since
    /// deleted — names nothing and is refused as not found. The scan is over maps
    /// bounded by this state's own ceilings.
    pub fn resolve_tagged_resource(
        &self,
        resource_arn: &str,
        region: &str,
    ) -> Result<TaggedResource, UnreachableResource> {
        let Some(arn) = ::service_common::arn::parse_of(resource_arn, ARN_NAMESPACE) else {
            return Err(UnreachableResource::Invalid(invalid_resource_arn(
                resource_arn,
            )));
        };
        if arn.region != region {
            return Err(UnreachableResource::Invalid(cross_region_resource_arn(
                resource_arn,
                region,
            )));
        }
        if let Some(profile) = self
            .brand_profiles
            .values()
            .find(|profile| profile.arn == resource_arn)
        {
            return Ok(TaggedResource::BrandProfile(profile.id.clone()));
        }
        if let Some(record) = self
            .notify_code_configurations
            .values()
            .find(|record| record.arn == resource_arn)
        {
            return Ok(TaggedResource::NotifyCodeConfiguration(record.id.clone()));
        }
        let resource_type = arn.resource_type();
        Err(UnreachableResource::NotFound(resource_not_found(
            resource_type,
            // The identifier the ARN names, which is what an error about a resource
            // reports; an ARN whose resource field names no id at all is reported by
            // that field.
            ::service_common::arn::resource_id_of_type(resource_arn, resource_type)
                .unwrap_or(arn.resource),
        )))
    }

    /// The tags stored on `resource`, in the order they were written, or `None` for a
    /// resource this state no longer holds — which a caller that resolved `resource`
    /// under the same lock never sees.
    pub fn tags_of(&self, resource: &TaggedResource) -> Option<&[(String, String)]> {
        match resource {
            TaggedResource::BrandProfile(id) => self
                .brand_profiles
                .get(id)
                .map(|profile| profile.tags.as_slice()),
            TaggedResource::NotifyCodeConfiguration(id) => self
                .notify_code_configurations
                .get(id)
                .map(|record| record.tags.as_slice()),
        }
    }

    /// The tags stored on `resource`, to write, or `None` for a resource this state no
    /// longer holds.
    ///
    /// The one place a tag write reaches, so every stored tag list is the one its own
    /// resource record carries rather than a second copy beside it.
    fn tags_of_mut(&mut self, resource: &TaggedResource) -> Option<&mut Vec<(String, String)>> {
        match resource {
            TaggedResource::BrandProfile(id) => {
                self.brand_profiles.get_mut(id).map(|profile| &mut profile.tags)
            }
            TaggedResource::NotifyCodeConfiguration(id) => self
                .notify_code_configurations
                .get_mut(id)
                .map(|record| &mut record.tags),
        }
    }

    /// Write `requested` onto `resource`, overwriting the value of a key the resource
    /// already carries and leaving every other key alone.
    ///
    /// A tag key is unique per resource (`TagKey`'s own documentation), so a key the
    /// resource already carries is one pair whose value this write replaces — in the
    /// place it already occupies, since the list's order is the order its keys were
    /// first written — and a key it does not carry is appended. Refused, writing
    /// nothing, when the merge would leave the resource carrying more than
    /// [`MAX_TAGS_PER_RESOURCE`] tags.
    pub fn write_tags(
        &mut self,
        resource: &TaggedResource,
        requested: &[(String, String)],
    ) -> Result<(), TagWriteRefused> {
        let Some(stored) = self.tags_of_mut(resource) else {
            return Err(TagWriteRefused::Vanished);
        };
        // What the resource would carry once the merge is applied: its own keys, plus
        // the requested keys it does not carry yet.
        let would_hold = stored.len()
            + requested
                .iter()
                .filter(|(key, _)| !stored.iter().any(|(held, _)| held == key))
                .count();
        if would_hold > MAX_TAGS_PER_RESOURCE {
            return Err(TagWriteRefused::Quota { would_hold });
        }
        for (key, value) in requested {
            match stored.iter_mut().find(|(held, _)| held == key) {
                Some(pair) => pair.1 = value.clone(),
                None => stored.push((key.clone(), value.clone())),
            }
        }
        Ok(())
    }
}

/// Why a tag write a request asked for was not applied.
#[derive(Debug)]
pub enum TagWriteRefused {
    /// The resource was resolved but holds no record, which the lock the resolution
    /// and the write share makes impossible. `InternalServerException`, HTTP 500.
    Vanished,
    /// The merge would leave the resource carrying more than [`MAX_TAGS_PER_RESOURCE`]
    /// tags. `ServiceQuotaExceededException`, HTTP 400.
    Quota {
        /// How many tags the resource would have carried had the write been applied.
        would_hold: usize,
    },
}

/// The most tags one resource of this service carries.
///
/// AWS's per-resource tag quota is 50 keys, which the modelled `TagList`'s
/// `@length(max: 50)` states for ONE request — a tag write MERGES its keys into the
/// tags already stored on the resource, so the ceiling has to be on the stored list,
/// not on a request. A write that would take a resource past it is refused with
/// `ServiceQuotaExceededException`, the error `TagResource` models for a request that
/// would exceed a quota, rather than growing the list.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// `ServiceQuotaExceededException` for a tag write that would leave a resource
/// carrying more than [`MAX_TAGS_PER_RESOURCE`] tags.
///
/// The fixed text names the quota; the variable parts are the number of tags the
/// resource would have carried and the ceiling itself, so the caller is told how far
/// past it the request reached.
pub fn tag_quota_exceeded(would_hold: usize) -> crate::ops::types::ServiceQuotaExceededException {
    crate::ops::types::ServiceQuotaExceededException {
        message: format!(
            "The request would leave the resource carrying {would_hold} tags, more than the \
             maximum of {MAX_TAGS_PER_RESOURCE} tags a resource may carry."
        ),
    }
}

/// The tag-key namespace AWS keeps for the tags it creates itself.
///
/// A key inside it is not a caller's to assign: AWS's own tagging rules reserve this
/// prefix, so a request naming such a key is refused rather than written. The
/// namespace is AWS's however the prefix is cased, so it is matched
/// case-insensitively ([`is_reserved_tag_key`]).
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// Whether `key` lies inside the namespace [`RESERVED_TAG_KEY_PREFIX`] keeps, compared
/// without regard to case.
///
/// Only a key that BEGINS with the prefix is reserved: one carrying it anywhere else,
/// or carrying `aws` without the colon, is an ordinary key.
pub fn is_reserved_tag_key(key: &str) -> bool {
    key.get(..RESERVED_TAG_KEY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_TAG_KEY_PREFIX))
}

/// The key/value pairs a request's `tags` asks to write, as the store keeps them, or
/// the refusal of a tag set no request may carry.
///
/// Every operation of this service that takes a `TagList` reads it through here, so a
/// tag set is judged the same whichever member it arrived on: a key inside the
/// reserved namespace is not a caller's to assign, and a key repeated inside one list
/// asks for two values for one key, which `TagKey` — "case-sensitive and must be
/// unique per resource" — leaves no room for. The elements are judged in the order the
/// request gave them, so one request is always refused for the same element, and
/// nothing is written for a request that carries either.
pub fn requested_tags(
    tags: &[crate::ops::types::Tag],
) -> Result<Vec<(String, String)>, crate::ops::types::ValidationException> {
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(tags.len());
    for (index, tag) in tags.iter().enumerate() {
        if is_reserved_tag_key(&tag.key) {
            return Err(reserved_tag_key(index, &tag.key));
        }
        if pairs.iter().any(|(key, _)| key == &tag.key) {
            return Err(duplicate_tag_key(index, &tag.key));
        }
        pairs.push((tag.key.clone(), tag.value.clone()));
    }
    Ok(pairs)
}

/// The JSON pointer naming the `key` of the tag at `index` of a request's `tags`, as
/// `ValidationExceptionField$path` documents the member a refusal names.
fn tag_key_pointer(index: usize) -> String {
    format!("/tags/{index}/key")
}

/// `ValidationException` for a tag key inside the namespace
/// [`RESERVED_TAG_KEY_PREFIX`] keeps.
///
/// Worded as a sentence, the way this API words a refusal its model does not express
/// (an unissued pagination token, an update carrying nothing to update). The fixed
/// text is this refusal's and names the rule; the only variable parts are the prefix
/// itself and the key the request gave, which the sentence closes with.
fn reserved_tag_key(index: usize, key: &str) -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: format!("Tag key cannot start with '{RESERVED_TAG_KEY_PREFIX}': {key}"),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: tag_key_pointer(index),
            message: format!(
                "The tag key must not begin with the reserved prefix {RESERVED_TAG_KEY_PREFIX}"
            ),
        }]),
    }
}

/// `ValidationException` for a tag key a request names more than once.
///
/// The fixed text is this refusal's; the only variable part is the key the request
/// repeated. The element the `fieldList` names is the one that repeated it — the
/// second, since that is the element the request could drop.
fn duplicate_tag_key(index: usize, key: &str) -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: format!("Duplicate tag key: '{key}'"),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: tag_key_pointer(index),
            message: "Each tag key must be named at most once in one request; tag keys are \
                      case-sensitive"
                .to_string(),
        }]),
    }
}

/// A stored key/value pair as the modelled `Tag` the wire carries.
///
/// Every projection of a stored tag goes through here, so one stored pair has one
/// wire form whichever operation reports it.
pub fn tag_view(tag: &(String, String)) -> crate::ops::types::Tag {
    crate::ops::types::Tag {
        key: tag.0.clone(),
        value: tag.1.clone(),
    }
}
