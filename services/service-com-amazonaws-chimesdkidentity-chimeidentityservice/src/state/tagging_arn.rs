// SPDX-License-Identifier: CC0-1.0

//! The ONE rule that decides whether a tagging call may address a `ResourceARN`.
//!
//! `TagResource`, `UntagResource` and `ListTagsForResource` serve the family's three generic
//! tagging routes, and the question they each have to answer first is the same one: may a
//! tagging call address this ARN, and if not, which error class and which sentence does the
//! refusal carry. What those handlers cannot share is the error ENUM — each operation's
//! generated enum is its own type — so the decision is stated here once and each handler
//! only wraps it in the variant of its own enum. A change to the rule is then made in one
//! place and the three answers cannot drift apart.
//!
//! Two stores answer it, read in the lock order [`super::service_state`] documents: this
//! service's own store first and released again, then the family's shared tag directory.
//!
//! * This service's store knows whether an ARN of its own still names a live resource, and
//!   whether the `AppInstance` behind it has been deleted
//!   ([`super::UnresolvedArn`]).
//! * The shared directory carries the class and the wording the member that OWNS a
//!   withdrawn resource serves for a tagging call naming it, because this service serves the
//!   tagging routes for every member's resource and cannot read a sibling's store.

use ::service_common::chime_tags::{ChimeTagDirectory, WithdrawnClass};

use super::{
    ComAmazonawsChimesdkidentityChimeidentityserviceState, UnresolvedArn,
    FORBIDDEN_REQUEST_MESSAGE,
};

/// The `Code` a refusal of an ARN that names no resource this service serves tagging for
/// carries: no tagging operation of this model declares a 404 shape, so the refusal is the
/// modelled `BadRequestException` identified by this code.
const NOT_FOUND_CODE: &str = "NotFound";

/// The `Code` of the modelled permanent refusal (`ForbiddenException`).
const FORBIDDEN_CODE: &str = "Forbidden";

/// The `Code` a refusal carries when the member owning the resource states a tagging call
/// naming it as an invalid request.
const BAD_REQUEST_CODE: &str = "BadRequest";

/// Which of the two client-error shapes every tagging operation of this model declares the
/// refusal belongs to, with the `Code` and `Message` it carries.
///
/// The CLASS rather than an error value: the handler that serves the call wraps this in the
/// matching variant of its own generated error enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggingArnRefusal {
    /// The modelled `BadRequestException` (HTTP 400).
    BadRequest {
        /// The refusal's `Code` member, which is what a client branches on.
        code: &'static str,
        /// The refusal's `Message` member.
        message: String,
    },
    /// The modelled `ForbiddenException` (HTTP 403).
    Forbidden {
        /// The refusal's `Code` member.
        code: &'static str,
        /// The refusal's `Message` member.
        message: String,
    },
}

/// Why a tagging call may not address `resource_arn`, or `None` when it may.
///
/// Reads only: whichever way it answers, neither store is changed.
pub fn tagging_arn_refusal(
    state: &::engine::ServiceState<ComAmazonawsChimesdkidentityChimeidentityserviceState>,
    directory: &::engine::ServiceState<ChimeTagDirectory>,
    resource_arn: &str,
) -> Option<TaggingArnRefusal> {
    // `None` when the ARN names a resource a tagging call may address; otherwise WHY
    // it does not, which is what decides the refusal.
    let unresolved = {
        let store = state.lock();
        (!store.taggable(resource_arn)).then(|| store.unresolved_arn(resource_arn))
    };
    if let Some(unresolved) = unresolved {
        return Some(match unresolved {
            // The AppInstance this ARN belongs to has been deleted, so it can no longer
            // serve a read or a write of its own tags or of a cascaded-away child's: the
            // answer is the modelled permanent refusal.
            UnresolvedArn::DeletedAppInstance => TaggingArnRefusal::Forbidden {
                code: FORBIDDEN_CODE,
                message: FORBIDDEN_REQUEST_MESSAGE.to_string(),
            },
            // These operations model no 404 shape, so an ARN that names no resource this
            // service serves tagging for is refused with the modelled `BadRequestException`
            // carrying the `NotFound` error code.
            UnresolvedArn::NeverExisted => TaggingArnRefusal::BadRequest {
                code: NOT_FOUND_CODE,
                message: format!("resource {resource_arn} does not exist"),
            },
        });
    }

    // An ARN whose OWNING member has deleted the resource it named is refused with that
    // member's own class and wording: this service serves the family's shared tagging routes
    // for every member's resource and cannot read a sibling's store, so the fact that the
    // resource is gone is read from the shared directory its owner recorded it in, and each
    // class is mapped onto the shape THIS model declares.
    let withdrawn = directory
        .lock()
        .withdrawn_refusal(resource_arn)
        .map(|refusal| (refusal.class, refusal.message.to_string()));
    withdrawn.map(|(class, message)| match class {
        WithdrawnClass::InvalidRequest => TaggingArnRefusal::BadRequest {
            code: BAD_REQUEST_CODE,
            message,
        },
        WithdrawnClass::Forbidden => TaggingArnRefusal::Forbidden {
            code: FORBIDDEN_CODE,
            message,
        },
    })
}
