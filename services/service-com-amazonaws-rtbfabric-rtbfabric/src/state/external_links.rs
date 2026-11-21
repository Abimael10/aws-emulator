// SPDX-License-Identifier: CC0-1.0

//! The two external-link surfaces' state: the links a stored gateway holds with one
//! end OUTSIDE the fabric —
//! `/requester-gateway/{gatewayId}/outbound-external-link`, whose traffic leaves the
//! fabric for a public endpoint, and
//! `/responder-gateway/{gatewayId}/inbound-external-link`, whose traffic arrives from
//! outside it on a name this service assigns.
//!
//! An external link is not a link between two gateways: it has one end in the fabric
//! and one end at a public address, so it carries no `peerGatewayId`, nothing accepts
//! or rejects it, and it is addressed only through the gateway it was opened on.
//! That is why it is a record of its own rather than a
//! [`Link`](super::Link) with members left empty — every operation on
//! `super::Link` reports a peer, and this resource has none.
//!
//! Each surface's create and read are two operations on one surface, so the record
//! they share lives here once: the write and the read then answer about exactly the
//! same set of links.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{LinkAttributes, LinkLogSettings, ModuleConfiguration};

/// `ConnectivityType` member: the traffic leaves the fabric for a public endpoint,
/// which is what an OUTBOUND external link carries
/// (`smithy_query(query="shape", shape="ConnectivityType")`).
pub const CONNECTIVITY_TYPE_PUBLIC_EGRESS: &str = "PUBLIC_EGRESS";

/// `ConnectivityType` member: the traffic arrives from outside the fabric, which is
/// what an INBOUND EXTERNAL link carries — the member of that enum named for exactly
/// this resource (`smithy_query(query="shape", shape="ConnectivityType")`).
pub const CONNECTIVITY_TYPE_EXTERNAL_INBOUND: &str = "EXTERNAL_INBOUND";

/// The DNS name this service assigns ONE inbound external link: the link's own id as
/// the leftmost label under the inbound name of the gateway it was created on
/// ([`external_inbound_endpoint`](super::external_inbound_endpoint)).
///
/// Per link rather than per gateway, because the name is what an external caller
/// sends this link's traffic to and one gateway takes many inbound external links;
/// and assigned by the service rather than taken from the request, because the
/// create carries no name and the response member is `@required` — a gateway reached
/// through a managed endpoint has no caller-stated domain name to report at all.
pub fn inbound_external_link_domain_name(
    gateway_id: &str,
    link_id: &str,
    region: &str,
) -> String {
    format!(
        "{link_id}.{}",
        super::external_inbound_endpoint(gateway_id, region)
    )
}

/// Materialise the `PENDING_CREATION` -> `ACTIVE` transition the clock has already
/// made due on an external link of either direction, dated at the instant it was
/// owed rather than now, so the record does not depend on when it was next read.
///
/// One statement of the transition for both records: nothing accepts or rejects a
/// link with one end outside the fabric, so an external link of either direction
/// starts at `PENDING_CREATION` and is owed `ACTIVE`.
fn settle_pending_creation(
    status: &mut String,
    settles_at: &mut Option<SystemTime>,
    updated_at: &mut SystemTime,
    now: SystemTime,
) {
    if let Some(due) = settles_at.filter(|due| *due <= now) {
        if status == super::LINK_STATUS_PENDING_CREATION {
            *status = super::LINK_STATUS_ACTIVE.to_string();
            *updated_at = due;
        }
        *settles_at = None;
    }
}

/// One inbound external link, as `CreateInboundExternalLink` persisted it.
///
/// Keyed in the store by (`gatewayId`, `linkId`): the link belongs to the responder
/// gateway it was created on and is addressed only through it.
///
/// Holds the members `CreateInboundExternalLink` takes but its response shape does
/// not carry (`attributes`, `logSettings`, `tags`) and the `domainName` this service
/// assigned the link: `GetInboundExternalLink` reads them back from here, which is
/// the whole point of storing the request rather than echoing it.
#[derive(Debug, Clone)]
pub struct InboundExternalLink {
    pub link_id: String,
    pub gateway_id: String,
    pub status: String,
    /// The instant the clock makes `PENDING_CREATION` -> `ACTIVE` due; `None` once
    /// the record has settled.
    pub settles_at: Option<SystemTime>,
    /// The name callers outside the fabric reach this link on, as the create
    /// assigned it ([`inbound_external_link_domain_name`]).
    pub domain_name: String,
    pub connectivity_type: String,
    pub attributes: Option<LinkAttributes>,
    pub log_settings: LinkLogSettings,
    pub flow_modules: Vec<ModuleConfiguration>,
    pub pending_flow_modules: Vec<ModuleConfiguration>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl InboundExternalLink {
    /// See [`settle_pending_creation`].
    pub fn settle(&mut self, now: SystemTime) {
        settle_pending_creation(
            &mut self.status,
            &mut self.settles_at,
            &mut self.updated_at,
            now,
        );
    }
}

/// One outbound external link, as `CreateOutboundExternalLink` persisted it.
///
/// Keyed in the store by (`gatewayId`, `linkId`): the link belongs to the requester
/// gateway it was opened from and is addressed only through it.
///
/// Holds the members `CreateOutboundExternalLink` takes but its response shape does
/// not carry (`publicEndpoint`, `attributes`, `logSettings`, `tags`):
/// `GetOutboundExternalLink` reads them back from here, which is the whole point of
/// storing the request rather than echoing it.
#[derive(Debug, Clone)]
pub struct OutboundExternalLink {
    pub link_id: String,
    pub gateway_id: String,
    pub status: String,
    /// The instant the clock makes `PENDING_CREATION` -> `ACTIVE` due; `None` once
    /// the record has settled.
    pub settles_at: Option<SystemTime>,
    /// The address outside the fabric this link carries traffic to, as the caller
    /// stated it.
    pub public_endpoint: String,
    pub connectivity_type: String,
    pub attributes: Option<LinkAttributes>,
    pub log_settings: LinkLogSettings,
    pub flow_modules: Vec<ModuleConfiguration>,
    pub pending_flow_modules: Vec<ModuleConfiguration>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl OutboundExternalLink {
    /// See [`settle_pending_creation`].
    pub fn settle(&mut self, now: SystemTime) {
        settle_pending_creation(
            &mut self.status,
            &mut self.settles_at,
            &mut self.updated_at,
            now,
        );
    }
}
