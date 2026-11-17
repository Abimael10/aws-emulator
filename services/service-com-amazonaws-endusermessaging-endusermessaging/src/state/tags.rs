// SPDX-License-Identifier: CC0-1.0

//! The resource a `resourceArn` names, and the tags it carries.
//!
//! Every tagging operation of this service addresses its resource the same way: by
//! the full ARN of a resource of this service's own namespace, in the Region the
//! request was made in. Resolving that ARN lives here rather than in one handler, so
//! a tag read and a tag write cannot disagree about which resource an ARN names, or
//! about how a value that names none is refused.
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
