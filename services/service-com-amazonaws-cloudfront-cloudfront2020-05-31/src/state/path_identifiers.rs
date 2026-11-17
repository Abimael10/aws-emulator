// SPDX-License-Identifier: CC0-1.0

//! What an identifier bound to the request PATH has to be before the operation it addresses
//! can resolve it at all.
//!
//! An identifier CloudFront issues names the kind of resource it was issued for: an
//! identifier of one kind is never an identifier of another, so an operation addressed by one
//! decides whether the value it was handed could be an identifier of ITS resource kind before
//! it goes near the store. The decision has two halves, and this module holds the half the
//! predicates in [`super::identity`] cannot make on their own:
//!
//! * the FORM — the namespace letter, the body alphabet and the body width CloudFront issues
//!   that kind's identifiers in — which is a property of the value alone
//!   ([`super::is_distribution_id`] and its neighbours);
//! * which KIND an identifier belongs to, for the family CloudFront draws from ONE form. A
//!   distribution, a streaming distribution, an origin access identity and an origin access
//!   control all carry an `E` followed by an identifier body of the same width and alphabet,
//!   so the form alone cannot tell one family member's identifier from another's. Real
//!   CloudFront reads the kind out of the identifier itself; this emulator's mint draws every
//!   one of them from the same shape, so the kind an identifier belongs to is read where this
//!   emulator records it — the STORE, whose map holding the identifier is what says which kind
//!   issued it.
//!
//! A value that fails either half is an identifier the service cannot READ, which is a
//! different answer from a resource that is absent ([`super::id_format_invalid_message`]), and
//! a per-operation one: whether an operation reads its identifier's kind before resolving it
//! is that operation's own recorded contract, so the refusals below are reached from the
//! handlers that record it rather than applied to every operation of a resource kind.
//!
//! The same split, for the kinds whose identifier form IS decided by the value alone, is made
//! by the free functions beside the refusals they answer with:
//! [`super::unresolved_key_group_id`] and [`super::unresolved_public_key_id`]. Those need no
//! store, which is the one reason the one below is a method.
//!
//! Nothing here echoes the submitted value: both sentences are fixed text.

use super::*;

/// The resource kinds whose identifiers CloudFront draws from ONE form (`E` followed by an
/// identifier body), and which are therefore told apart by the store rather than by the value.
///
/// Every variant is a kind this service MINTS an `E`-form identifier for
/// (`allocate_distribution_id`, `allocate_streaming_distribution_id`,
/// `allocate_origin_access_identity_id`, `allocate_origin_access_control_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Distribution,
    StreamingDistribution,
    OriginAccessIdentity,
    OriginAccessControl,
}

impl ComAmazonawsCloudfrontCloudfront20200531State {
    /// Which kind of resource this account holds `id` as, or `None` when no resource of the
    /// `E`-form family answers to it.
    ///
    /// The maps are what carry the answer: an identifier is in exactly one of them, because
    /// each mint refuses an identifier a store of its own kind already holds and the four
    /// kinds are minted from one counter.
    pub fn entity_kind(&self, id: &str) -> Option<EntityKind> {
        if self.distributions.contains_key(id) {
            Some(EntityKind::Distribution)
        } else if self.streaming_distributions.contains_key(id) {
            Some(EntityKind::StreamingDistribution)
        } else if self.origin_access_identities.contains_key(id) {
            Some(EntityKind::OriginAccessIdentity)
        } else if self.origin_access_controls.contains_key(id) {
            Some(EntityKind::OriginAccessControl)
        } else {
            None
        }
    }

    /// Whether `id` could ADDRESS a resource of `kind`: it is written the way CloudFront
    /// writes that family's identifiers, and this account holds no resource of ANOTHER kind of
    /// the family under it.
    ///
    /// An identifier of the right form that nothing answers to could address one — the
    /// resource it names is simply not there — so this answers `true` for it, which is what
    /// keeps the absent-resource sentence for the condition it describes.
    pub fn addresses(&self, kind: EntityKind, id: &str) -> bool {
        // Each kind's own form predicate, so a kind whose form ever stops being the family's
        // is read by its own rule here rather than by a shared alias of one of them.
        let in_form = match kind {
            EntityKind::Distribution => is_distribution_id(id),
            EntityKind::StreamingDistribution => is_streaming_distribution_id(id),
            EntityKind::OriginAccessIdentity => is_origin_access_identity_id(id),
            EntityKind::OriginAccessControl => is_origin_access_control_id(id),
        };
        in_form && self.entity_kind(id).is_none_or(|held| held == kind)
    }

    /// The modelled `NoSuchDistribution` an operation ADDRESSED by a distribution identifier
    /// answers an identifier it could not resolve, where `member` is the model's name for the
    /// input member the identifier arrived in (`Id` on the distribution reads,
    /// `DistributionId` on the operations addressed under one).
    ///
    /// An identifier that could address a distribution is the absent-distribution sentence;
    /// every other value is the identifier reported unreadable, naming that member.
    pub fn unresolved_distribution(&self, member: &str, id: &str) -> NoSuchDistribution {
        if self.addresses(EntityKind::Distribution, id) {
            no_such_distribution()
        } else {
            NoSuchDistribution {
                message: Some(id_format_invalid_message(member)),
            }
        }
    }
}

/// The model's name for the member every operation of the `E`-form family addresses its
/// resource by, except the ones that address a resource held UNDER a distribution
/// (`CreateInvalidation$DistributionId`).
pub const ADDRESSED_BY_ID: &str = "Id";

