// SPDX-License-Identifier: CC0-1.0

//! The `/responder-gateway/{gatewayId}/link/{linkId}/routing-rule` surface's state:
//! the routing rules a stored link carries.
//!
//! A rule belongs to one link, so it is keyed by (`linkId`, `ruleId`) and released
//! with that link ([`release_link`](super::ComAmazonawsRtbfabricRtbfabricState::release_link)):
//! no read can report a rule whose link is gone.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::ops::types::RuleCondition;

/// `RuleStatus` member: the rule is accepted, it is not routing yet.
pub const RULE_STATUS_CREATION_IN_PROGRESS: &str = "CREATION_IN_PROGRESS";
/// `RuleStatus` member: the rule routes traffic.
pub const RULE_STATUS_ACTIVE: &str = "ACTIVE";

/// What a `conditions` structure carrying no condition at all is refused with.
///
/// "At least one condition field must be set" is the modelled documentation of
/// `RuleCondition` (`smithy_query(query="shape", shape="RuleCondition")`), and no
/// single member is `@required`, so the schema layer cannot refuse an empty structure
/// and the rule is stated here.
pub const RULE_CONDITIONS_EMPTY_MESSAGE: &str =
    "conditions must set at least one condition for the rule to match on";

/// How many routing rules the store holds before `CreateLinkRoutingRule` refuses with
/// the modelled `ServiceQuotaExceededException` (HTTP 402).
///
/// The emulator's own bound, for the same reason as
/// [`LINK_QUOTA`](super::LINK_QUOTA): a rule is released with its link, so this bounds
/// what is live rather than what was ever created.
pub const ROUTING_RULE_QUOTA: usize = 5_000;

/// A routing rule on a link, as `CreateLinkRoutingRule` persisted it.
#[derive(Debug, Clone)]
pub struct LinkRoutingRule {
    pub rule_id: String,
    /// The link the rule routes for; the rule is addressable from either of that
    /// link's two ends, exactly as the link itself is.
    pub link_id: String,
    pub priority: i32,
    pub conditions: RuleCondition,
    pub status: String,
    /// The instant the clock makes `CREATION_IN_PROGRESS` -> `ACTIVE` due; `None`
    /// once the record has settled.
    pub settles_at: Option<SystemTime>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl LinkRoutingRule {
    /// Materialise the `CREATION_IN_PROGRESS` -> `ACTIVE` transition the clock has
    /// already made due, dated at the instant it was owed rather than now.
    pub fn settle(&mut self, now: SystemTime) {
        if let Some(due) = self.settles_at.filter(|due| *due <= now) {
            self.status = RULE_STATUS_ACTIVE.to_string();
            self.settles_at = None;
            self.updated_at = due;
        }
    }
}

/// Whether `conditions` states anything for a request to match on.
///
/// Every member of the modelled `RuleCondition` is optional, so an empty structure
/// deserialises happily and would match every request; the shape's own documentation
/// is that at least one of them is set.
pub fn conditions_stated(conditions: &RuleCondition) -> bool {
    conditions.host_header.is_some()
        || conditions.host_header_wildcard.is_some()
        || conditions.path_prefix.is_some()
        || conditions.path_exact.is_some()
        || conditions.query_string_equals.is_some()
        || conditions.query_string_exists.is_some()
}
