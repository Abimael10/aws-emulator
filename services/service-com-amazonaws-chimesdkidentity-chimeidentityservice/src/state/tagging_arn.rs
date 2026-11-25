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
//!
//! The wording of the other refusal a tag WRITE earns — the pairs would take the resource
//! past the tag count the family's `TagList` bounds it by, so the shared directory stores
//! none of them ([`too_many_tags_message`]) — is stated here for the same reason: `TagResource`
//! and every create that carries `Tags` answer that one refusal, so they cannot word it
//! differently either.
//!
//! The same rule read the other way round is what this service's own deletes PUBLISH:
//! [`withdrawal_for`] answers, for an ARN this service has just stopped holding, the class
//! and the wording a tagging call naming it earns here, and a delete records that pair in
//! the shared directory. Both directions are then one statement of the rule, so the refusal
//! a sibling serves for one of this service's deleted ARNs is the refusal this service
//! serves for it.

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

/// The `Code` the modelled `ResourceLimitExceededException` (HTTP 400) carries.
pub const RESOURCE_LIMIT_EXCEEDED_CODE: &str = "ResourceLimitExceeded";

/// The sentence a refusal of a tag WRITE carries when the pairs it asks for would take the
/// resource past the tag count the family's `TagList` bounds it by.
///
/// One function because the shared directory refuses such a write the same way wherever it
/// is made — it stores nothing — and every route of this service that writes tags has to
/// report that refusal: the tagging call itself and each create that carries `Tags`. The
/// count is the one the refused merge would have reached, which is what tells a caller how
/// far past the maximum its request was.
pub fn too_many_tags_message(requested: usize, resource_arn: &str) -> String {
    format!(
        "{requested} tags were requested for {resource_arn}, which is more than the maximum \
         of {} tags per resource",
        ::service_common::chime_tags::MAX_TAGS_PER_RESOURCE
    )
}

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

/// The sentence a refusal of an ARN this service holds no resource for carries.
///
/// One function because it is answered twice: to the caller of a tagging call this service
/// serves ([`tagging_arn_refusal`]), and to the shared directory when a delete withdraws the
/// ARN ([`withdrawal_for`]).
fn absent_resource_message(resource_arn: &str) -> String {
    format!("resource {resource_arn} does not exist")
}

/// What a delete of `resource_arn` records in the family's shared directory: the class and
/// the wording THIS service answers a tagging call naming that ARN with, now that it holds
/// no resource under it.
///
/// A delete withdraws an ARN rather than only dropping its tags because the member that
/// serves the next tagging call naming it may be a sibling, which cannot read this store:
/// without the record it would take the ARN at its word and report a deleted resource as a
/// live, untagged one. The pair comes from the same [`super::UnresolvedArn`] reading the
/// own-store half of [`tagging_arn_refusal`] answers with, so what a sibling serves and what
/// this service serves for one of its own deleted ARNs cannot drift apart.
///
/// Call it with the store AFTER the removal, so the ARN already reads as one this service
/// does not hold.
pub fn withdrawal_for(
    store: &ComAmazonawsChimesdkidentityChimeidentityserviceState,
    resource_arn: &str,
) -> (WithdrawnClass, String) {
    match store.unresolved_arn(resource_arn) {
        // The `AppInstance` this ARN belongs to is gone, so a tagging call naming it — or
        // naming one of the children the delete cascaded away — is a request this caller may
        // no longer make.
        UnresolvedArn::DeletedAppInstance => (
            WithdrawnClass::Forbidden,
            FORBIDDEN_REQUEST_MESSAGE.to_string(),
        ),
        // A child deleted on its own, under an `AppInstance` that is still live: nothing
        // this service holds carries the ARN any more, which is an invalid request rather
        // than a revoked one.
        UnresolvedArn::NeverExisted => (
            WithdrawnClass::InvalidRequest,
            absent_resource_message(resource_arn),
        ),
    }
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
                message: absent_resource_message(resource_arn),
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
