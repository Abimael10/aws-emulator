// SPDX-License-Identifier: CC0-1.0

//! Whose ACCOUNT the resources a rule's targets name may belong to.
//!
//! EventBridge invokes the resources of the account the rule lives in: "to be able
//! to make API calls against the resources that you own, Amazon EventBridge needs
//! the appropriate permissions" (`PutTargets`), and the ONE class of target that
//! operation documents in another account is the event BUS of that account — "if
//! another Amazon Web Services account is in the same region and has granted you
//! permission (using `PutPermission`), you can send events to that account. Set
//! that account's event bus as a target of the rules in your account". A target of
//! any other class whose ARN names another account therefore asks for a delivery
//! EventBridge does not make.
//!
//! That is an AUTHORIZATION boundary rather than a malformed value — the ARN is
//! well formed and names a real class of resource, in an account this caller has
//! nothing in — so the refusal is `AccessDeniedException` and it names the
//! resource access was refused to. It is decided where the target is CONFIGURED,
//! before anything is stored, so a client that mistyped an account id reads it at
//! once instead of owning a rule that silently never delivers.
//!
//! The account a request speaks for is this emulator's own ([`super::account_id`]),
//! the account every resource it mints belongs to.

use super::{account_id, ARN_NAMESPACE};

/// The target classes a rule may name in ANOTHER account: the ARN namespace and
/// the resource type of an event bus, which is the one cross-account destination
/// `PutTargets` documents.
///
/// The Region is deliberately not part of it: a target in another REGION of the
/// caller's own account is a resource the account owns, and this rule is about
/// whose resource it is.
const CROSS_ACCOUNT_TARGET_CLASSES: [(&str, &str); 1] = [(ARN_NAMESPACE, "event-bus")];

/// What the service states when a target names a resource of another account: the
/// resource access was denied to, and the reason it was.
fn cross_account_refusal(arn: &str) -> String {
    format!(
        "Access to the resource {arn} is denied. Reason: Adding cross-account target is not \
         permitted."
    )
}

/// The `AccessDeniedException` message a target whose ARN names a resource of
/// another account earns, or `None` when the resource is the caller's own or is of
/// the one class deliverable across accounts.
///
/// An ARN carrying no account at all names a partition- or Region-global resource,
/// which is nobody's account to be outside of, so it is left to the checks that
/// read what it names. Text that is no ARN is not judged here either: it is refused
/// as the malformed ARN it is, before this ([`super::target_arn_violation`]).
#[must_use]
pub fn cross_account_target_denial(arn: &str) -> Option<String> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.account.is_empty() || parsed.account == account_id() {
        return None;
    }
    (!names_a_cross_account_class(arn)).then(|| cross_account_refusal(arn))
}

/// Whether `arn` names a resource of one of [`CROSS_ACCOUNT_TARGET_CLASSES`].
fn names_a_cross_account_class(arn: &str) -> bool {
    let Some(parsed) = ::service_common::arn::parse(arn) else {
        return false;
    };
    CROSS_ACCOUNT_TARGET_CLASSES
        .iter()
        .any(|(vendor, resource_type)| {
            parsed.vendor == *vendor && parsed.resource_type() == *resource_type
        })
}

/// Whether `arn` names an EVENT BUS — the one class above, read here so the
/// class is held in one place.
///
/// Asked by the role requirement of a bus target
/// ([`super::request_validation`]), which is about the CLASS a target names
/// rather than about whether that class may be named across accounts at all.
#[must_use]
pub fn names_an_event_bus(arn: &str) -> bool {
    names_a_cross_account_class(arn)
}
