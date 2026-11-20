// SPDX-License-Identifier: CC0-1.0

//! The `/requester-gateway/{gatewayId}/outbound-external-link` surface's state: the
//! links a stored requester gateway holds to an endpoint OUTSIDE the fabric.
//!
//! An external link is not a link between two gateways: it has one end in the fabric
//! and one end at a public address, so it carries no `peerGatewayId`, nothing accepts
//! or rejects it, and it is addressed only through the gateway it was opened from.
//! That is why it is a record of its own rather than a
//! [`Link`](super::Link) with members left empty — every operation on
//! `super::Link` reports a peer, and this resource has none.
//!
//! `CreateOutboundExternalLink` and `GetOutboundExternalLink` are two operations on
//! one surface, so the record they share lives here once: the write and the read then
//! answer about exactly the same set of links.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::{LinkAttributes, LinkLogSettings, ModuleConfiguration};

/// `ConnectivityType` member: the traffic leaves the fabric for a public endpoint,
/// which is what an OUTBOUND external link carries
/// (`smithy_query(query="shape", shape="ConnectivityType")`).
pub const CONNECTIVITY_TYPE_PUBLIC_EGRESS: &str = "PUBLIC_EGRESS";

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
    /// Materialise the `PENDING_CREATION` -> `ACTIVE` transition the clock has
    /// already made due, dated at the instant it was owed rather than now, so the
    /// record does not depend on when it was next read.
    pub fn settle(&mut self, now: SystemTime) {
        if let Some(due) = self.settles_at.filter(|due| *due <= now) {
            if self.status == super::LINK_STATUS_PENDING_CREATION {
                self.status = super::LINK_STATUS_ACTIVE.to_string();
                self.updated_at = due;
            }
            self.settles_at = None;
        }
    }
}
