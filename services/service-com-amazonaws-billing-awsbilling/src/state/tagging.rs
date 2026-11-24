// SPDX-License-Identifier: CC0-1.0

//! The decision every operation of this service's tagging plane takes about the
//! resource an `ResourceArn` names: whether its tags are the customer's to read
//! or change, and if not, which refusal answers the call.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` all identify their
//! target the same way and draw the same two lines on it — a view AWS manages is
//! not taggable ([`super::BillingView::is_managed`]) and an ARN naming no stored
//! resource does not exist — so the decision lives here once
//! ([`ComAmazonawsBillingAwsbillingState::tagging_target`] and its `_mut`
//! sibling) instead of beside each of the three handlers, which cannot then
//! drift apart on which refusal applies or on what it reports.
//!
//! The DECISION is shared, the exception is not: each handler turns a
//! [`TaggingRefusal`] into the modeled error its own operation declares, the way
//! the Enterprise Support reads lift [`super::enterprise_support_read_refusal`]
//! and the charge reports lift [`super::BillingMonthRefusal`].

use super::{
    BillingView, ComAmazonawsBillingAwsbillingState, BILLING_VIEW_RESOURCE_TYPE,
    MANAGED_VIEW_NOT_TAGGABLE, RESOURCE_NOT_FOUND_MESSAGE, VALIDATION_REASON_FIELD,
};
use crate::ops::types::{
    ResourceNotFoundException, ValidationException, ValidationExceptionField,
};

/// The request member every tagging operation identifies its resource by.
pub const TAGGING_RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// Why the resource an `resourceArn` names is not one whose tags the caller may
/// read or change — carrying the payload the refusal reports, so the three
/// handlers report one ARN's refusal identically whichever of them was asked.
#[derive(Debug, Clone)]
pub enum TaggingRefusal {
    /// The ARN names a view AWS manages on the account's behalf, whose tags are
    /// the service's own: a request to refuse as invalid (HTTP 400) over the
    /// member that carried the ARN.
    Managed(ValidationException),
    /// The ARN names no stored resource (HTTP 404).
    Absent(ResourceNotFoundException),
}

/// The modeled `ValidationException` payload (HTTP 400) that refuses a tagging
/// call against a view AWS manages, over the member that carried the ARN.
fn managed_view_not_taggable() -> ValidationException {
    ValidationException {
        message: MANAGED_VIEW_NOT_TAGGABLE.to_string(),
        reason: VALIDATION_REASON_FIELD.to_string(),
        field_list: Some(vec![ValidationExceptionField {
            name: TAGGING_RESOURCE_ARN_MEMBER.to_string(),
            message: MANAGED_VIEW_NOT_TAGGABLE.to_string(),
        }]),
    }
}

/// The modeled `ResourceNotFoundException` payload (HTTP 404) for a tagging
/// call against an ARN no stored resource answers to.
///
/// The tagging APIs answer the FIXED sentence [`RESOURCE_NOT_FOUND_MESSAGE`]
/// here: the ARN the request carried is reported in `resourceId`, the member the
/// model has for it, and not echoed into the message.
fn tagging_target_not_found(resource_arn: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: RESOURCE_NOT_FOUND_MESSAGE.to_string(),
        resource_id: resource_arn.to_string(),
        resource_type: BILLING_VIEW_RESOURCE_TYPE.to_string(),
    }
}

/// The whole decision of the tagging plane, over whatever borrow of the stored
/// record the caller looked the ARN up with: a stored view AWS manages is
/// refused as invalid, an ARN under which nothing is stored as absent, and
/// anything else — a stored CUSTOM view — is the caller's to tag, untag and
/// read.
///
/// Generic over the borrow so the decision is stated ONCE for both the read
/// accessor and the write one, each of which takes its own single lookup:
/// `Borrow<BillingView>` is satisfied by `&BillingView` and by
/// `&mut BillingView` alike.
fn resolve_tagging_target<View: ::std::borrow::Borrow<BillingView>>(
    found: Option<View>,
    resource_arn: &str,
) -> Result<View, TaggingRefusal> {
    match found {
        Some(view) if view.borrow().is_managed() => {
            Err(TaggingRefusal::Managed(managed_view_not_taggable()))
        }
        Some(view) => Ok(view),
        None => Err(TaggingRefusal::Absent(tagging_target_not_found(
            resource_arn,
        ))),
    }
}

impl ComAmazonawsBillingAwsbillingState {
    /// The stored view a tagging READ may report the tags of, or the refusal
    /// that read answers instead.
    pub fn tagging_target(&self, resource_arn: &str) -> Result<&BillingView, TaggingRefusal> {
        resolve_tagging_target(self.billing_views.get(resource_arn), resource_arn)
    }

    /// The stored view a tagging WRITE may change the tags of, or the refusal
    /// that write answers instead — the same decision [`Self::tagging_target`]
    /// takes, so the direction of the call does not change which resources are
    /// taggable.
    pub fn tagging_target_mut(
        &mut self,
        resource_arn: &str,
    ) -> Result<&mut BillingView, TaggingRefusal> {
        resolve_tagging_target(self.billing_views.get_mut(resource_arn), resource_arn)
    }
}
