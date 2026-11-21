// SPDX-License-Identifier: CC0-1.0

//! Which resource of this service a tag-family `resourceArn` addresses, and the refusals
//! that reading settles.
//!
//! The tag APIs name the resource they act on by ARN rather than by id, so one member
//! addresses every kind of resource this service holds: the model declares `resourceArn` with
//! a `@references` trait naming the Access Source, Access Token, DNS View, Firewall Domain
//! List and Global Resolver resources
//! (`smithy_query(query="operation",operation="TagResource")`). Reading that one value is
//! therefore the step that decides WHICH collection a tag call reaches, and it is stated here
//! once so the three operations of the family cannot disagree about it.
//!
//! The reading is a pure function of the value: nothing here touches the store or the clock.

use super::{
    DNS_VIEW_KIND, DNS_VIEW_RESOURCE, FIREWALL_DOMAIN_LIST_KIND, FIREWALL_DOMAIN_LIST_RESOURCE,
    GLOBAL_RESOLVER_KIND, GLOBAL_RESOLVER_RESOURCE,
};
use crate::ops::types::{
    ServiceQuotaExceededException, ValidationException, ValidationExceptionField,
};
use std::collections::BTreeMap;

/// The most tags one resource carries at once.
///
/// The emulator's OWN ceiling, taken from the model's figure for the tag map a request may
/// carry: `Tags` is `@length(max: 50)` (`smithy_query(query="shape",shape="Tags")`), so a
/// resource holding more could not be reported by `ListTagsForResource` within its own output
/// shape. Nothing recorded shows this service's refusal for a resource past it, so the
/// sentence [`tag_quota_exceeded`] states is this emulator's own wording rather than AWS's,
/// in the form the sibling ceilings of this service state theirs ([`super::quota_exceeded`]);
/// what the refusal's CLASS and status are is the model's (`ServiceQuotaExceededException`,
/// `@httpError(402)`). The ceiling also keeps the store bounded under repeated tagging.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The member the tag family names the resource it acts on by, as the wire spells it — the
/// member a refusal of that value attributes itself to.
///
/// The shared spelling (`service_common::addressed_resource::RESOURCE_ARN_MEMBER`), since it
/// is the member name every AWS service's tagging operations use and the pre-dispatch gate
/// reads the value through it; re-exported here so this service's refusals name it without
/// reaching past their own module.
pub use ::service_common::addressed_resource::RESOURCE_ARN_MEMBER;

/// Which collection of this service a tag-family ARN addresses.
///
/// One variant per resource kind this emulator HOLDS. The kinds the model's `@references`
/// also names — an Access Source and an Access Token — have no collection yet, because the
/// operations that would mint one are scaffolds: an ARN naming one addresses nothing this
/// store can hold, so it is read as no resource of this service rather than as an empty
/// collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedResourceKind {
    /// A Global Resolver.
    GlobalResolver,
    /// A DNS View.
    DnsView,
    /// A DNS Firewall domain list.
    FirewallDomainList,
}

impl TaggedResourceKind {
    /// The ARN resource-type token this kind is written with, the one this service's own ARN
    /// builder puts in a minted ARN ([`super::resource_arn`]).
    pub fn arn_resource_type(self) -> &'static str {
        match self {
            Self::GlobalResolver => GLOBAL_RESOLVER_RESOURCE,
            Self::DnsView => DNS_VIEW_RESOURCE,
            Self::FirewallDomainList => FIREWALL_DOMAIN_LIST_RESOURCE,
        }
    }

    /// How this service's own refusal messages spell this kind — the API's spelling, not the
    /// ARN token ([`super::GLOBAL_RESOLVER_KIND`] and its siblings).
    pub fn message_kind(self) -> &'static str {
        match self {
            Self::GlobalResolver => GLOBAL_RESOLVER_KIND,
            Self::DnsView => DNS_VIEW_KIND,
            Self::FirewallDomainList => FIREWALL_DOMAIN_LIST_KIND,
        }
    }

    /// The kind an ARN's resource-type token names, or `None` when it names none of this
    /// service's collections.
    fn of_arn_resource_type(resource_type: &str) -> Option<Self> {
        [Self::GlobalResolver, Self::DnsView, Self::FirewallDomainList]
            .into_iter()
            .find(|kind| kind.arn_resource_type() == resource_type)
    }
}

/// The resource a tag-family `resourceArn` addresses: which collection, and the id within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaggedResource<'a> {
    /// The collection the ARN's resource type names.
    pub kind: TaggedResourceKind,
    /// The id the ARN's resource field carries, which is the key the collection holds it
    /// under.
    pub id: &'a str,
}

/// The resource of THIS service `resource_arn` addresses, or `None` when the value addresses
/// none — an ARN of another vendor, or one of this vendor whose resource field names no kind
/// this service holds or carries no id.
///
/// The ARN is read through the tree's one caller-supplied-ARN parse
/// (`service_common::arn`), so the vendor field — "the same value a service publishes as its
/// `arnNamespace`" — is what decides whose resource the value names. The Region and the
/// account are NOT part of the reading: a resource of this service is held under its id
/// alone, and this emulator serves one account, so an ARN differing only in those fields
/// addresses the same resource it names.
pub fn tagged_resource(resource_arn: &str) -> Option<TaggedResource<'_>> {
    let arn = ::service_common::arn::parse_of(resource_arn, super::ARN_SERVICE)?;
    let kind = TaggedResourceKind::of_arn_resource_type(arn.resource_type())?;
    let id = ::service_common::arn::resource_id_of_type(resource_arn, kind.arn_resource_type())?;
    (!id.is_empty()).then_some(TaggedResource { kind, id })
}

/// The modelled 400 for a `resourceArn` that is an ARN but addresses no resource of this
/// service — the refusal the tag family answers once the caller has been authorized against
/// the value ([`crate::service`]'s resource-ARN gate) and the value turns out to name another
/// vendor's resource, or none at all.
///
/// The sentence is the one the recorded refusal of such a value carries: the fixed text
/// `Invalid ARN format: ` followed by the value the REQUEST supplied, verbatim. Unlike this
/// service's not-found and list refusals it carries no bracketed `[GR-ERRnnnnn]` code, which
/// is how the recording states it. `reason` is `FIELD_VALIDATION_FAILED`, the one
/// `ValidationExceptionReason` that carries a `fieldList`
/// (`smithy_query(query="shape",shape="ValidationExceptionReason")`), and that list names the
/// member the value arrived on.
pub fn invalid_arn_format(resource_arn: &str) -> ValidationException {
    let message = format!("Invalid ARN format: {resource_arn}");
    ValidationException {
        message: message.clone(),
        reason: "FIELD_VALIDATION_FAILED".to_string(),
        field_list: Some(vec![ValidationExceptionField {
            name: RESOURCE_ARN_MEMBER.to_string(),
            message,
        }]),
    }
}

/// The modelled 402 for a `TagResource` that would push a resource past
/// [`MAX_TAGS_PER_RESOURCE`], naming the resource it would have tagged and the ceiling it
/// would pass. The wording is this emulator's own, for the reason stated on that ceiling.
pub fn tag_quota_exceeded(kind: TaggedResourceKind, id: &str) -> ServiceQuotaExceededException {
    ServiceQuotaExceededException {
        message: format!("this emulator holds at most {MAX_TAGS_PER_RESOURCE} tags per resource"),
        resource_id: Some(id.to_string()),
        resource_type: kind.message_kind().to_string(),
        service_code: Some(super::ARN_SERVICE.to_string()),
        quota_code: None,
    }
}

impl super::ComAmazonawsRoute53globalresolverEc2dnsglobalresolvercustomerapiState {
    /// The tags the resource `resource` addresses carries, or `None` when this store holds no
    /// such resource.
    ///
    /// The ONE place a tag-family read reaches a record's tags, so a list, a tag and an untag
    /// of the same ARN reach the same map whichever collection holds it.
    pub fn tags_of(&self, resource: TaggedResource<'_>) -> Option<&BTreeMap<String, String>> {
        match resource.kind {
            TaggedResourceKind::GlobalResolver => self
                .global_resolvers
                .get(resource.id)
                .map(|resolver| &resolver.tags),
            TaggedResourceKind::DnsView => self.dns_views.get(resource.id).map(|view| &view.tags),
            TaggedResourceKind::FirewallDomainList => self
                .firewall_domain_lists
                .get(resource.id)
                .map(|list| &list.tags),
        }
    }

    /// [`Self::tags_of`] for a call that WRITES them — the tag and untag paths.
    pub fn tags_of_mut(
        &mut self,
        resource: TaggedResource<'_>,
    ) -> Option<&mut BTreeMap<String, String>> {
        match resource.kind {
            TaggedResourceKind::GlobalResolver => self
                .global_resolvers
                .get_mut(resource.id)
                .map(|resolver| &mut resolver.tags),
            TaggedResourceKind::DnsView => self
                .dns_views
                .get_mut(resource.id)
                .map(|view| &mut view.tags),
            TaggedResourceKind::FirewallDomainList => self
                .firewall_domain_lists
                .get_mut(resource.id)
                .map(|list| &mut list.tags),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the reading table: the case, the ARN read, and the resource it addresses.
    ///
    /// Named rather than written inline so the row type stays readable as the table grows.
    type ArnRow = (
        &'static str,
        &'static str,
        Option<(TaggedResourceKind, &'static str)>,
    );

    /// Reading an ARN is decided by its vendor and its resource field, and by nothing else:
    /// the Region and the account vary without changing which resource is addressed, while a
    /// vendor, a resource type or an id this service does not hold addresses nothing.
    #[test]
    fn an_arn_addresses_a_resource_by_its_vendor_and_resource_field() {
        let rows: [ArnRow; 11] = [
            (
                "a Global Resolver of this account",
                "arn:aws:route53globalresolver:us-east-2:000000000000:global-resolver/gr-000000000001",
                Some((TaggedResourceKind::GlobalResolver, "gr-000000000001")),
            ),
            (
                "a DNS View",
                "arn:aws:route53globalresolver:us-east-2:000000000000:dns-view/dnsv-000000000002",
                Some((TaggedResourceKind::DnsView, "dnsv-000000000002")),
            ),
            (
                "a firewall domain list",
                "arn:aws:route53globalresolver:eu-west-1:000000000000:firewall-domain-list/fdl-3",
                Some((TaggedResourceKind::FirewallDomainList, "fdl-3")),
            ),
            (
                "another partition, another Region, another account",
                "arn:aws-us-gov:route53globalresolver:us-gov-west-1:111111111111:global-resolver/gr-4",
                Some((TaggedResourceKind::GlobalResolver, "gr-4")),
            ),
            (
                "no Region and no account, as AWS writes a global resource",
                "arn:aws:route53globalresolver:::global-resolver/gr-5",
                Some((TaggedResourceKind::GlobalResolver, "gr-5")),
            ),
            (
                "the colon form of the resource field",
                "arn:aws:route53globalresolver:us-east-2:000000000000:dns-view:dnsv-6",
                Some((TaggedResourceKind::DnsView, "dnsv-6")),
            ),
            (
                "another vendor's resource",
                "arn:aws:s3:::a-bucket",
                None,
            ),
            (
                "this vendor, a resource kind this service does not hold",
                "arn:aws:route53globalresolver:us-east-2:000000000000:firewall-rule/fr-7",
                None,
            ),
            (
                "this vendor, a known kind with no id",
                "arn:aws:route53globalresolver:us-east-2:000000000000:global-resolver/",
                None,
            ),
            (
                "this vendor, the kind alone",
                "arn:aws:route53globalresolver:us-east-2:000000000000:global-resolver",
                None,
            ),
            ("not an ARN at all", "not-an-arn", None),
        ];
        for (case, arn, expected) in rows {
            let actual = tagged_resource(arn).map(|resource| (resource.kind, resource.id));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The refusal of an ARN that addresses nothing states the value back, attributes itself
    /// to the member it arrived on, and reports the reason that carries a field list.
    #[test]
    fn the_invalid_arn_refusal_states_the_value_and_the_member() {
        for arn in [
            "arn:aws:s3:::a-bucket",
            "arn:aws:route53globalresolver:us-east-2:000000000000:firewall-rule/fr-1",
        ] {
            let refusal = invalid_arn_format(arn);
            assert_eq!(
                refusal.message,
                format!("Invalid ARN format: {arn}"),
                "case {arn:?}: expected the value stated back, got {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.reason, "FIELD_VALIDATION_FAILED",
                "case {arn:?}: expected the reason that carries a field list, got {:?}",
                refusal.reason
            );
            let named: Vec<String> = refusal
                .field_list
                .unwrap_or_default()
                .into_iter()
                .map(|field| field.name)
                .collect();
            assert_eq!(
                named,
                vec![RESOURCE_ARN_MEMBER.to_string()],
                "case {arn:?}: expected the field list to name the member, got {named:?}"
            );
        }
    }
}
