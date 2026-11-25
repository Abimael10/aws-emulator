// SPDX-License-Identifier: CC0-1.0

//! What an identifier bound to the request PATH has to be before the operation it addresses
//! can resolve it at all.
//!
//! An identifier CloudFront issues names the kind of resource it was issued for, so an
//! operation addressed by one can report a value it cannot resolve either as the resource that
//! is ABSENT or as an identifier it cannot READ ([`super::id_format_invalid_message`]) — the
//! same not-found code and status under two different sentences. HOW MUCH of the submitted
//! value a route reads before it goes near the store is that resource kind's own recorded
//! contract, and the members that carry one are plain `string` `@httpLabel`s in the model (no
//! `@pattern`, no `@length`), so no route may invent a format constraint of its own:
//!
//! * the DISTRIBUTION reads the ALPHABET and nothing more
//!   ([`super::unresolved_distribution_id`]). Every operation addressed by a distribution
//!   identifier — the distribution's own reads and mutations, the invalidations held under one,
//!   the monitoring subscription — carries a value written in the alphabet an entity
//!   identifier is written in ([`super::is_entity_id_form`]) straight through to the store and
//!   reports the distribution ABSENT ([`super::no_such_distribution`]) for every one it holds
//!   no distribution under, because the width this emulator MINTS a distribution identifier at
//!   is one width CloudFront writes one in, never the only width, and WHICH identifier of that
//!   alphabet was ever issued is a property of the STORE alone — while a value written outside
//!   that alphabet is the identifier the route reports it cannot READ;
//! * the field-level encryption CONFIGURATION DOCUMENT routes read the WHOLE form their
//!   record's ids are ISSUED in — the kind's namespace letter over an identifier body of the
//!   issued width — of a value that is written as an identifier of this mint at all, and read
//!   no form of a value that is not ([`super::unresolved_field_level_encryption_config`],
//!   [`super::unresolved_field_level_encryption_profile`]), while that configuration's
//!   resource read reads no form at all and reports every id it cannot resolve as the
//!   configuration that is absent ([`super::no_such_field_level_encryption_config`]);
//! * the ORIGIN ACCESS CONTROL reads the ALPHABET and nothing more
//!   ([`ComAmazonawsCloudfrontCloudfront20200531State::unresolved_origin_access_control`]).
//!
//! The same split, for the kinds whose identifier form IS decided by the value alone, is made
//! by the free functions beside the refusals they answer with:
//! [`super::unresolved_distribution_id`], [`super::unresolved_key_group_id`] and the two
//! field-level encryption records'. Those need
//! no store, which is the one reason the one below is a method. A kind whose recorded contract
//! makes NO such split reads nothing of the value at all and reports the absent resource for
//! every identifier it cannot resolve, which is what the public key and the
//! field-level encryption configuration's RESOURCE read do
//! ([`super::no_such_public_key`], [`super::no_such_field_level_encryption_config`]).
//!
//! Nothing here echoes the submitted value: both sentences are fixed text.

use super::*;

impl ComAmazonawsCloudfrontCloudfront20200531State {
    /// The modelled `NoSuchOriginAccessControl` an operation ADDRESSED by an origin access
    /// control `Id` answers an identifier it could not resolve.
    ///
    /// `Id` is a plain `string` `@httpLabel` with no `@pattern` and no `@length`
    /// (`smithy_query(query="operation",operation="GetOriginAccessControl")`), so what this
    /// route reads of the value is the ALPHABET an identifier is written in and nothing more
    /// ([`is_entity_id_form`]) — this route's own recorded contract, which the clusters of
    /// `GetOriginAccessControl` own rather than the field-level encryption surface's
    /// ([`super::unresolved_field_level_encryption_config`]). A control's
    /// identifier comes from the namespace its four kinds SHARE ([`ENTITY_ID_PREFIX`]), so
    /// neither the letter a value opens with nor the width of its body says which kind issued
    /// it, and WHICH identifier of that alphabet was ever issued is a property of the STORE: a
    /// value of the alphabet the account holds no control under is the control reported
    /// ABSENT.
    ///
    /// A value written outside that alphabet — in lower case, around a separator, as a UUID,
    /// as a word or as a domain name — was never an identifier CloudFront issued at all, so it
    /// is the identifier this route reports it cannot READ ([`id_format_invalid_message`]).
    /// Both sentences are fixed text, so nothing from the request reaches the wire.
    pub fn unresolved_origin_access_control(&self, id: &str) -> NoSuchOriginAccessControl {
        if is_entity_id_form(id) {
            return no_such_origin_access_control();
        }
        NoSuchOriginAccessControl {
            message: Some(id_format_invalid_message(ADDRESSED_BY_ID)),
        }
    }
}

/// The model's name for the member every operation of the `E`-form family addresses its
/// resource by, except the ones that address a resource held UNDER a distribution
/// (`CreateInvalidation$DistributionId`).
pub const ADDRESSED_BY_ID: &str = "Id";

/// The model's name for the member the operations addressed by the distribution a resource is
/// held under carry that distribution's identifier in — the invalidations of a distribution
/// and its monitoring subscription (`CreateInvalidation$DistributionId`,
/// `GetMonitoringSubscription$DistributionId`), which is the member such a route names when
/// it reports an identifier it cannot read ([`super::unresolved_distribution_id`]).
pub const ADDRESSED_BY_DISTRIBUTION_ID: &str = "DistributionId";
