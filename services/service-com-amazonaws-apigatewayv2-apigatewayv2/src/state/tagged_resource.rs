// SPDX-License-Identifier: CC0-1.0

//! The resource a caller-supplied `ResourceArn` of this service names, and the tag
//! collection stored on it.
//!
//! `GetTags`, `TagResource` and `UntagResource` address their target by ARN rather than by
//! the path labels every other operation of this service takes
//! (`smithy_query(query="operation", operation="UntagResource")`: `ResourceArn` is the one
//! `@httpLabel` of `/v2/tags/{ResourceArn}`), so reading an ARN into the record it names
//! lives here, in one place all three share: the same string has to resolve to the same
//! record, and be refused the same way, whichever of them took it.
//!
//! WHICH resources are taggable comes from the model: a resource of this service is
//! taggable exactly when the shape declared for it carries a `Tags` member
//! (`smithy_query(query="shape", shape="Api,Stage,VpcLink,DomainName,Portal,\
//! PortalProduct")` — all six do), and the resource portion of such a resource's ARN is the
//! URI of the operation that reads it without the version prefix
//! (`smithy_query(query="operation", operation="GetApi,GetStage,GetVpcLink,GetDomainName,\
//! GetPortal,GetPortalProduct")`: `/v2/apis/{ApiId}`, `/v2/apis/{ApiId}/stages/{StageName}`,
//! `/v2/vpclinks/{VpcLinkId}`, and so on) — the form the ARN minters of this store already
//! spell ([`super::domain_name_arn`], [`super::portal_arn`],
//! [`super::portal_product_arn`]).
//!
//! A child module of [`super`] rather than more of that file: the ARN reading and the tag
//! lookup are one concern, and keeping them together leaves the state module itself to the
//! records of this service.

use std::collections::BTreeMap;

use super::{ARN_SERVICE, ComAmazonawsApigatewayv2Apigatewayv2State};

/// The sentence this API answers a tag operation's `ResourceArn` that resolves to no
/// resource the account holds with.
///
/// A tag operation addresses its target by ARN rather than by the typed identifier every
/// other operation takes, so the refusal is about the RESOURCE the ARN named and not about
/// an identifier of a particular kind: one sentence for every taggable kind, carrying
/// neither the ARN the caller already holds nor the kind it named — unlike the per-kind
/// sentences the typed operations answer ([`super::invalid_api_identifier_message`],
/// [`super::invalid_identifier_message`]). Held here once, so the reader, the writer and
/// the removal of one resource's tags cannot word the same miss three ways.
pub const TAGGED_RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The resource one `ResourceArn` names: the stored record a tag collection hangs off, as
/// [`parse_tagged_resource`] read it out of the ARN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaggedResource {
    /// `/apis/{ApiId}` — an [`super::Api`]'s tags.
    Api { api_id: String },
    /// `/apis/{ApiId}/stages/{StageName}` — a [`super::Stage`]'s tags.
    Stage { api_id: String, stage_name: String },
    /// `/vpclinks/{VpcLinkId}` — a [`super::VpcLink`]'s tags.
    VpcLink { vpc_link_id: String },
    /// `/domainnames/{DomainName}` — a [`super::DomainName`]'s tags, whose path segment IS
    /// the record's key.
    DomainName { domain_name: String },
    /// `/portals/{PortalId}` — a [`super::Portal`]'s tags.
    Portal { portal_id: String },
    /// `/portalproducts/{PortalProductId}` — a [`super::PortalProduct`]'s tags.
    PortalProduct { portal_product_id: String },
}

/// Read `resource_arn` as the ARN of a taggable resource of this service.
///
/// The six ARN fields come from [`::service_common::arn::parse_of`], the ONE parse of a
/// caller-supplied ARN, narrowed to this service's vendor namespace — so what counts as an
/// ARN, and whose resource it names, is what every other service of this emulator says.
/// Only the resource PORTION is read here, because its shape is this service's own.
///
/// `None` for anything that is not the ARN of a taggable resource of this service: a string
/// that is not an ARN, an ARN of another vendor, or an API Gateway ARN whose resource
/// portion names something this service does not tag (a route, an integration, a deployment,
/// a REST API of the other API Gateway control plane) — which the caller is told is a bad
/// request rather than a missing resource.
///
/// The region and account fields are not matched further: this emulator serves one account,
/// and nothing this store holds is keyed by partition or Region, so what selects a record is
/// the resource portion. API Gateway's own api, stage, VPC-link and domain-name ARNs carry
/// an EMPTY account field while a portal's and a portal product's carry the owning account,
/// and both forms read back here as the records they name.
#[must_use]
pub fn parse_tagged_resource(resource_arn: &str) -> Option<TaggedResource> {
    let arn = ::service_common::arn::parse_of(resource_arn, ARN_SERVICE)?;
    let path: Vec<&str> = arn.resource.strip_prefix('/')?.split('/').collect();
    if path.iter().any(|segment| segment.is_empty()) {
        return None;
    }
    match path.as_slice() {
        ["apis", api_id] => Some(TaggedResource::Api {
            api_id: (*api_id).to_string(),
        }),
        ["apis", api_id, "stages", stage_name] => Some(TaggedResource::Stage {
            api_id: (*api_id).to_string(),
            stage_name: (*stage_name).to_string(),
        }),
        ["vpclinks", vpc_link_id] => Some(TaggedResource::VpcLink {
            vpc_link_id: (*vpc_link_id).to_string(),
        }),
        ["domainnames", domain_name] => Some(TaggedResource::DomainName {
            domain_name: (*domain_name).to_string(),
        }),
        ["portals", portal_id] => Some(TaggedResource::Portal {
            portal_id: (*portal_id).to_string(),
        }),
        ["portalproducts", portal_product_id] => Some(TaggedResource::PortalProduct {
            portal_product_id: (*portal_product_id).to_string(),
        }),
        _ => None,
    }
}

impl ComAmazonawsApigatewayv2Apigatewayv2State {
    /// The tag collection stored on the resource a `ResourceArn` named, as
    /// [`parse_tagged_resource`] read it, for a READER.
    ///
    /// One lookup for every tag read, so what a tag reader reports is the very collection
    /// the resource's own reader reports. `None` for an ARN that is well formed and that no
    /// record this account holds answers to — including a resource a delete has already
    /// taken away, and a VPC link whose deprovisioning the clock has already finished,
    /// which no reader of this account reports any more — told to the caller with
    /// [`TAGGED_RESOURCE_NOT_FOUND_MESSAGE`].
    pub fn tags_of(&self, resource: &TaggedResource) -> Option<&BTreeMap<String, String>> {
        match resource {
            TaggedResource::Api { api_id } => self.apis.get(api_id).map(|api| &api.tags),
            TaggedResource::Stage { api_id, stage_name } => self
                .apis
                .get(api_id)?
                .stages
                .get(stage_name)
                .map(|stage| &stage.tags),
            TaggedResource::VpcLink { vpc_link_id } => self
                .vpc_links
                .get(vpc_link_id)
                .filter(|link| !link.is_deprovisioned())
                .map(|link| &link.tags),
            TaggedResource::DomainName { domain_name } => self
                .domain_names
                .get(domain_name)
                .map(|record| &record.tags),
            TaggedResource::Portal { portal_id } => {
                self.live_portal(portal_id).map(|portal| &portal.tags)
            }
            TaggedResource::PortalProduct { portal_product_id } => self
                .portal_products
                .get(portal_product_id)
                .map(|product| &product.tags),
        }
    }

    /// The tag collection stored on the resource a `ResourceArn` named, as
    /// [`parse_tagged_resource`] read it, for a WRITER.
    ///
    /// The same collection [`Self::tags_of`] reports, so a tag write edits the very record a
    /// tag read serves; see there for the failure.
    pub fn tags_of_mut(
        &mut self,
        resource: &TaggedResource,
    ) -> Option<&mut BTreeMap<String, String>> {
        match resource {
            TaggedResource::Api { api_id } => self.apis.get_mut(api_id).map(|api| &mut api.tags),
            TaggedResource::Stage { api_id, stage_name } => self
                .apis
                .get_mut(api_id)?
                .stages
                .get_mut(stage_name)
                .map(|stage| &mut stage.tags),
            TaggedResource::VpcLink { vpc_link_id } => {
                // A link whose deprovisioning the clock already finished is no longer held
                // by the account, so it is dropped before the lookup and a tag operation
                // resolves the id against exactly what a reader at the same instant does.
                self.purge_deprovisioned_vpc_links();
                self.vpc_links
                    .get_mut(vpc_link_id)
                    .map(|link| &mut link.tags)
            }
            TaggedResource::DomainName { domain_name } => self
                .domain_names
                .get_mut(domain_name)
                .map(|record| &mut record.tags),
            // A portal a `DeletePortal` has taken away is not one the account holds, so it
            // is resolved through the live-portal lookup every other portal writer uses.
            TaggedResource::Portal { portal_id } => self
                .live_portal_mut(portal_id)
                .map(|portal| &mut portal.tags),
            TaggedResource::PortalProduct { portal_product_id } => self
                .portal_products
                .get_mut(portal_product_id)
                .map(|product| &mut product.tags),
        }
    }
}
