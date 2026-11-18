// SPDX-License-Identifier: CC0-1.0

//! What NAMES one domain name access association: the PAIR it is.
//!
//! A `DomainNameAccessAssociation` "represents a domain name access association between an
//! access association source and a private custom domain name", so the record IS that pair —
//! which is why the account holds at most one of them for one pair, and why the ARN that
//! addresses one spells BOTH halves: the private custom domain name it is for, and the source
//! that may invoke it ([`access_association_path`]).
//!
//! Reading such an ARN and spelling one are inverses ([`super::parse_tagged_resource`] and
//! [`super::tagged_resource_path`]), and what an ARN of this collection addresses is decided
//! by LOOKUP rather than by its spelling: any ARN of this service whose resource portion is
//! under the access-association collection names one of these records, and the record it names
//! is the one whose own pair spells that same portion
//! ([`ComAmazonawsApigatewayBackplanecontrolserviceState::access_association_at`]). A portion
//! no stored record spells therefore names an association that is not there — the 404 every
//! reader of one of these ARNs answers — while a value that is no ARN of this collection at
//! all is a request to fix. That order is what the delete, the rejection and the tag
//! operations share, so one ARN resolves to one record whichever of them took it.
//!
//! The sentences a member TYPED as one of these ARNs, or as the ARN of the private custom
//! domain name an association is for, is refused with live here beside that grammar: the
//! refusal and the form it is about are one concern.

use super::*;

/// The segment of an access association's ARN that introduces the PRIVATE CUSTOM DOMAIN NAME
/// half of the pair, before the key of that domain name.
pub const ACCESS_ASSOCIATION_DOMAIN_NAME_SEGMENT: &str = "domainname";

/// The segment that introduces the ACCESS ASSOCIATION SOURCE half, before the source itself.
///
/// It names the source's TYPE — the one value `AccessAssociationSourceType` declares is
/// `VPCE` (`smithy_query(query="shape", shape="AccessAssociationSourceType")`), whose source
/// "is the VPC endpoint ID" — so an ARN says which kind of source the identifier that follows
/// is, exactly as the member pair `accessAssociationSourceType`/`accessAssociationSource`
/// does.
pub const ACCESS_ASSOCIATION_SOURCE_SEGMENT: &str = "vpcesource";

/// The collection segment every access association's ARN carries, which is also the first
/// path segment of the operation that reads one (`smithy_query(query="operation",
/// operation="DeleteDomainNameAccessAssociation")`:
/// `/domainnameaccessassociations/{domainNameAccessAssociationArn}`).
pub const ACCESS_ASSOCIATION_KIND: &str = "domainnameaccessassociations";

/// How the pair `(private custom domain name, access association source)` is spelled INSIDE
/// the access-association collection: each half introduced by the segment that says which
/// half it is.
///
/// The ONE spelling of it, so the ARN this service mints, the ARN it reports and the portion
/// it resolves a record by cannot drift apart.
#[must_use]
pub fn access_association_path(domain_name_key: &str, access_association_source: &str) -> String {
    format!(
        "{ACCESS_ASSOCIATION_DOMAIN_NAME_SEGMENT}/{domain_name_key}/\
         {ACCESS_ASSOCIATION_SOURCE_SEGMENT}/{access_association_source}"
    )
}

/// The resource PORTION of the ARN of the access association `access_association_path`
/// addresses: the collection segment and then that portion.
#[must_use]
pub fn access_association_resource_path(access_association_path: &str) -> String {
    format!("/{ACCESS_ASSOCIATION_KIND}/{access_association_path}")
}

/// The `BadRequestException` message for a value carried on a member typed as the ARN OF A
/// DOMAIN NAME ACCESS ASSOCIATION that this service could not READ as one — a value that is
/// no ARN at all, an ARN of another vendor, or an API Gateway ARN whose resource portion is
/// not under the access-association collection.
///
/// API Gateway words this condition as a sentence of its own rather than as the generic
/// unreadable-ARN sentence ([`INVALID_ARN`]) a `resourceArn` earns, and the sentence is
/// ENTIRELY FIXED TEXT: it names the member's kind of ARN and quotes back neither the value
/// nor the member that carried it, as the recorded refusal of an unreadable
/// `domainNameAccessAssociationArn` shows. It is answered BEFORE any record is looked for,
/// because a value this service cannot read addresses nothing: a request to fix is a
/// different answer from an ARN of this collection that names no stored association
/// ([`NO_SUCH_ACCESS_ASSOCIATION`]).
pub const INVALID_ACCESS_ASSOCIATION_ARN: &str =
    "Invalid domain name access association ARN specified.";

/// The `BadRequestException` message for a `domainNameArn` that is not the ARN of a PRIVATE
/// custom domain name, on the member that takes one.
///
/// `CreateDomainNameAccessAssociation` creates "a domain name access association resource
/// between an access association source and a PRIVATE custom domain name", so its
/// `domainNameArn` is typed as the ARN of a private custom domain name — and a `domainNameId`
/// is "supported only for private custom domain names", which is what the resource portion of
/// such an ARN carries beside the name ([`DOMAIN_NAME_ID_SEPARATOR`]). A value that reads as
/// a domain name ARN without one, or one whose record is a domain name of another endpoint
/// type, names no private custom domain name at all.
///
/// ENTIRELY FIXED TEXT: it states which kind of ARN the member takes and quotes back neither
/// the value the request carried nor the record it resolved. A DIFFERENT sentence from
/// [`DOMAIN_NAME_NOT_FOUND`], which is what an ARN that IS one of these and answers to no
/// stored record earns: that is a resource the caller can go and look for, this is a member
/// the caller filled with the wrong kind of ARN.
pub const INVALID_PRIVATE_DOMAIN_NAME_ARN: &str = "Invalid private custom domain name arn.";

/// What a private custom domain name's key — and so the resource portion of its ARN — joins
/// its two identifiers with: several private records may carry one name, so the `domainNameId`
/// that identifies one is spelled after the name, as [`domain_name_key`] joins them.
pub const DOMAIN_NAME_ID_SEPARATOR: char = '+';

/// Whether `domain_name_key` is the key of a PRIVATE custom domain name: it carries a
/// `domainNameId`, the member "supported only for private custom domain names".
///
/// The ONE reading of that form, so every member typed as the ARN of a private custom domain
/// name holds its value to the same shape.
#[must_use]
pub fn is_private_domain_name_key(domain_name_key: &str) -> bool {
    match domain_name_key.split_once(DOMAIN_NAME_ID_SEPARATOR) {
        Some((domain_name, domain_name_id)) => {
            !domain_name.is_empty() && !domain_name_id.is_empty()
        }
        None => false,
    }
}

/// Read an access association's resource portion as the PAIR it spells: the private custom
/// domain name the association is for and the source that may invoke it, each introduced by
/// the segment that says which half it is.
///
/// `None` for a portion that spells no such pair, which is what a member held to the FORM of
/// one of these ARNs refuses ([`INVALID_ACCESS_ASSOCIATION_ARN`]).
#[must_use]
pub fn access_association_pair(access_association_path: &str) -> Option<(&str, &str)> {
    match *access_association_path.split('/').collect::<Vec<&str>>().as_slice() {
        [ACCESS_ASSOCIATION_DOMAIN_NAME_SEGMENT, domain_name_key, ACCESS_ASSOCIATION_SOURCE_SEGMENT, access_association_source]
            if !domain_name_key.is_empty() && !access_association_source.is_empty() =>
        {
            Some((domain_name_key, access_association_source))
        }
        _ => None,
    }
}

impl DomainNameAccessAssociationEntry {
    /// The resource portion this record is ADDRESSED by inside the access-association
    /// collection: its own pair, spelled the one way ([`access_association_path`]).
    #[must_use]
    pub fn access_association_path(&self) -> String {
        access_association_path(&self.domain_name_key, &self.access_association_source)
    }
}

impl ComAmazonawsApigatewayBackplanecontrolserviceState {
    /// The stored access association an ARN whose resource portion is
    /// `access_association_path` names, or `None` when the account holds none that portion
    /// addresses.
    ///
    /// The pair is what identifies one of these records, so the record an ARN names is the one
    /// whose own pair spells the portion that ARN carried — the lookup behind
    /// [`Self::tags_of`], and so behind every operation that addresses an association by its
    /// ARN.
    #[must_use]
    pub fn access_association_at(
        &self,
        access_association_path: &str,
    ) -> Option<&DomainNameAccessAssociationEntry> {
        self.domain_name_access_associations
            .values()
            .find(|association| association.access_association_path() == access_association_path)
    }

    /// The same record for WRITING, so `TagResource` and `UntagResource` edit the very record
    /// [`Self::access_association_at`] reports.
    #[must_use]
    pub fn access_association_at_mut(
        &mut self,
        access_association_path: &str,
    ) -> Option<&mut DomainNameAccessAssociationEntry> {
        self.domain_name_access_associations
            .values_mut()
            .find(|association| association.access_association_path() == access_association_path)
    }

    /// The KEY the record that portion addresses is stored under, which is what an operation
    /// that REMOVES it (`DeleteDomainNameAccessAssociation`,
    /// `RejectDomainNameAccessAssociation`) takes it out by — resolved through the same lookup
    /// every reader of an ARN of this collection uses, so a remove cannot address a record a
    /// read would not have found.
    #[must_use]
    pub fn access_association_key_at(&self, access_association_path: &str) -> Option<String> {
        self.access_association_at(access_association_path)
            .map(|association| association.id.clone())
    }
}
