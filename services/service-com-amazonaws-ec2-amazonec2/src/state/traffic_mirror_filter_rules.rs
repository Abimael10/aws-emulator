// SPDX-License-Identifier: CC0-1.0

//! The Traffic Mirror filter RULE store: the id prefix, the direction words, the bounds, the
//! byte charge and the error channel the Traffic Mirror filter rule operations share, with
//! the one write a rule enters the store by.
//!
//! A rule has nowhere to live but the filter it belongs to: "The ID of the filter that this
//! rule is associated with" (`CreateTrafficMirrorFilterRuleRequest$TrafficMirrorFilterId`),
//! and a filter reports its rules as its own members ("The Traffic Mirror rules associated
//! with the Traffic Mirror filter", `TrafficMirrorFilter$IngressFilterRules` and
//! `$EgressFilterRules`). So a stored rule lives on the filter record
//! ([`ComAmazonawsEc2Amazonec2State::traffic_mirror_filters`]) in the direction's own list,
//! as the MODELLED [`TrafficMirrorFilterRule`] shape — one copy of each rule, which is what
//! every reader of a filter already reports — and only the primitives over that store live
//! here, the way [`super::network_acls`] keeps a network ACL's numbered entries on the ACL.

use super::{tag_list_bytes, ComAmazonawsEc2Amazonec2State};
use crate::ops::types::{TrafficMirrorFilter, TrafficMirrorFilterRule};
use crate::scaffolded::Scaffolded;

/// The id prefix a Traffic Mirror filter rule is minted under.
///
/// EC2 names the resource's ids `tmfr-<hex>`, the same `<prefix>-<hex>` form every other EC2
/// resource id takes, so [`ComAmazonawsEc2Amazonec2State::next_id`] mints them.
pub const TRAFFIC_MIRROR_FILTER_RULE_ID_PREFIX: &str = "tmfr";

/// The one `ResourceType` a `CreateTrafficMirrorFilterRule` tag specification may name:
/// "Traffic Mirroring tags specifications"
/// (`CreateTrafficMirrorFilterRuleRequest$TagSpecifications`) for the rule the call creates,
/// spelled as the modelled `ResourceType` enum spells it
/// (`smithy_query(query="shape",shape="ResourceType")`).
pub const TRAFFIC_MIRROR_FILTER_RULE_TAGGABLE_RESOURCE: &str = "traffic-mirror-filter-rule";

/// The `TrafficDirection` of a rule that filters traffic INTO the mirrored source, as the
/// modelled `TrafficDirection` enum spells it
/// (`smithy_query(query="shape",shape="TrafficDirection")`).
pub const TRAFFIC_DIRECTION_INGRESS: &str = "ingress";

/// The `TrafficDirection` of a rule that filters traffic OUT of the mirrored source, the
/// other member of the same modelled enum as [`TRAFFIC_DIRECTION_INGRESS`].
pub const TRAFFIC_DIRECTION_EGRESS: &str = "egress";

/// How many Traffic Mirror filter rules this emulator stores before
/// `CreateTrafficMirrorFilterRule` is refused.
///
/// CHOSEN, like [`super::MAX_TRAFFIC_MIRROR_FILTERS`] and for the same reason: nothing in
/// this emulator reclaims a stored rule, so the store needs a ceiling that is not the
/// process's memory. A count ceiling is not a memory bound on its own — see
/// [`super::MAX_STORED_BYTES`] — so the caller-chosen bytes of each record are charged
/// against that budget too ([`traffic_mirror_filter_rule_payload_bytes`]).
pub const MAX_TRAFFIC_MIRROR_FILTER_RULES: usize = 1_000;

/// EC2's quota code for one Traffic Mirror filter rule too many, named the way every other
/// per-resource EC2 quota code is ([`super::TRAFFIC_MIRROR_FILTER_LIMIT_EXCEEDED`],
/// `VolumeLimitExceeded`).
pub const TRAFFIC_MIRROR_FILTER_RULE_LIMIT_EXCEEDED: &str = "TrafficMirrorFilterRuleLimitExceeded";

/// EC2's code for a rule the filter already holds at that rule number in that direction (EC2
/// API "Error codes": `TrafficMirrorFilterRuleAlreadyExists`) — "This number must be unique
/// for each Traffic Mirror rule in a given direction"
/// (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`), so a create never overwrites one.
pub const TRAFFIC_MIRROR_FILTER_RULE_ALREADY_EXISTS: &str = "TrafficMirrorFilterRuleAlreadyExists";

/// Refuse a create at a rule number the filter already holds a rule at in that direction:
/// HTTP 400 [`TRAFFIC_MIRROR_FILTER_RULE_ALREADY_EXISTS`], naming the rule number the request
/// sent — the sentence this error class is worded with for every other numbered rule of this
/// service ([`super::network_acl_entry_already_exists`]).
pub fn traffic_mirror_filter_rule_already_exists<E>(rule_number: i32) -> Scaffolded<E> {
    Scaffolded::service_error(
        TRAFFIC_MIRROR_FILTER_RULE_ALREADY_EXISTS,
        400,
        format!("The traffic mirror filter rule identified by {rule_number} already exists."),
    )
}

/// Refuse a create that would take the store past [`MAX_TRAFFIC_MIRROR_FILTER_RULES`] or
/// past the store's byte budget: HTTP 400 [`TRAFFIC_MIRROR_FILTER_RULE_LIMIT_EXCEEDED`].
pub fn traffic_mirror_filter_rule_limit_exceeded<E>(message: impl Into<String>) -> Scaffolded<E> {
    Scaffolded::service_error(TRAFFIC_MIRROR_FILTER_RULE_LIMIT_EXCEEDED, 400, message)
}

/// The caller-chosen bytes one stored Traffic Mirror filter rule holds for the process's
/// lifetime: its tags, its description, the two address ranges it matches and the client
/// token the token map keeps for it.
///
/// Everything else on the record — the ids, the rule number, the ports and the protocol, the
/// direction and the action — is derived by this crate, drawn from a modelled enum or a
/// bounded number, so it is bounded already and not counted here. The ONE definition of that
/// charge, so a writer cannot store bytes the store's budget never saw, and a filter's
/// removal can give back exactly what its rules took
/// ([`ComAmazonawsEc2Amazonec2State::release_traffic_mirror_filter_rules`]).
pub fn traffic_mirror_filter_rule_payload_bytes(
    rule: &TrafficMirrorFilterRule,
    client_token: Option<&str>,
) -> usize {
    tag_list_bytes(rule.tags.as_deref().unwrap_or_default())
        .saturating_add(rule.description.as_ref().map_or(0, String::len))
        .saturating_add(rule.destination_cidr_block.as_ref().map_or(0, String::len))
        .saturating_add(rule.source_cidr_block.as_ref().map_or(0, String::len))
        .saturating_add(client_token.map_or(0, str::len))
}

/// The rules `filter` holds in the direction `traffic_direction` names, or `None` when that
/// is no direction the modelled `TrafficDirection` enum carries.
///
/// "Each Traffic Mirror filter has a set of ingress rules and a separate set of egress
/// rules" is what the two members of [`TrafficMirrorFilter`] are
/// (`$IngressFilterRules`/`$EgressFilterRules`), so a direction selects one of them and
/// nothing else — the one place that choice is made, for the readers and the writer alike.
pub fn traffic_mirror_filter_rules_in_direction<'a>(
    filter: &'a TrafficMirrorFilter,
    traffic_direction: &str,
) -> Option<&'a [TrafficMirrorFilterRule]> {
    let rules = match traffic_direction {
        TRAFFIC_DIRECTION_INGRESS => filter.ingress_filter_rules.as_deref(),
        TRAFFIC_DIRECTION_EGRESS => filter.egress_filter_rules.as_deref(),
        _ => return None,
    };
    Some(rules.unwrap_or_default())
}

impl ComAmazonawsEc2Amazonec2State {
    /// Every Traffic Mirror filter rule the account holds, in filter order and within a
    /// filter in the order the filter reports them.
    ///
    /// The filter records ARE the rule store, so this is a view over them rather than a
    /// second copy that could drift from what a filter reports.
    pub fn traffic_mirror_filter_rules(&self) -> impl Iterator<Item = &TrafficMirrorFilterRule> {
        self.traffic_mirror_filters.values().flat_map(|filter| {
            filter
                .ingress_filter_rules
                .iter()
                .chain(filter.egress_filter_rules.iter())
                .flatten()
        })
    }

    /// The Traffic Mirror filter rule `traffic_mirror_filter_rule_id` names, wherever it
    /// lives, or `None` when the account holds no such rule.
    ///
    /// "The ID of the Traffic Mirror rule" (`TrafficMirrorFilterRule$TrafficMirrorFilterRuleId`)
    /// is unique across this store, so an id resolves to at most one rule of one filter.
    pub fn traffic_mirror_filter_rule(
        &self,
        traffic_mirror_filter_rule_id: &str,
    ) -> Option<&TrafficMirrorFilterRule> {
        self.traffic_mirror_filter_rules().find(|rule| {
            rule.traffic_mirror_filter_rule_id.as_deref() == Some(traffic_mirror_filter_rule_id)
        })
    }

    /// The rule the filter `traffic_mirror_filter_id` holds at `rule_number` in the direction
    /// `traffic_direction`, or `None` when it holds none there.
    ///
    /// "This number must be unique for each Traffic Mirror rule in a given direction"
    /// (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`), so one number in one direction of
    /// one filter names at most one rule: this is what a create asks before it writes.
    pub fn traffic_mirror_filter_rule_at(
        &self,
        traffic_mirror_filter_id: &str,
        traffic_direction: &str,
        rule_number: i32,
    ) -> Option<&TrafficMirrorFilterRule> {
        let filter = self.traffic_mirror_filters.get(traffic_mirror_filter_id)?;
        traffic_mirror_filter_rules_in_direction(filter, traffic_direction)?
            .iter()
            .find(|rule| rule.rule_number == Some(rule_number))
    }

    /// Whether another Traffic Mirror filter rule fits inside the store's ceiling
    /// ([`MAX_TRAFFIC_MIRROR_FILTER_RULES`]) and its byte budget.
    ///
    /// The same contract as [`Self::has_traffic_mirror_filter_capacity`]: a create asks
    /// first and refuses with [`traffic_mirror_filter_rule_limit_exceeded`] when it answers
    /// `false`, and [`Self::insert_traffic_mirror_filter_rule`] asks it again for itself so a
    /// writer that forgets cannot overshoot either bound.
    pub fn has_traffic_mirror_filter_rule_capacity(
        &self,
        rule: &TrafficMirrorFilterRule,
        client_token: Option<&str>,
    ) -> bool {
        self.traffic_mirror_filter_rules().count() < MAX_TRAFFIC_MIRROR_FILTER_RULES
            && !self.would_exceed_stored_bytes(traffic_mirror_filter_rule_payload_bytes(
                rule,
                client_token,
            ))
    }

    /// Store `rule` on the filter its `TrafficMirrorFilterId` names, under a freshly minted
    /// `TrafficMirrorFilterRuleId`, charge the caller-chosen bytes it and `client_token`
    /// retain against the store's byte budget, and answer the id it was given.
    ///
    /// `None` when the rule names no filter this account holds, no direction the modelled
    /// enum carries, a rule number the filter already holds a rule at in that direction, or
    /// when the store has no room for it — every one of those is enforced HERE as well as
    /// asked by the writer ([`Self::has_traffic_mirror_filter_rule_capacity`],
    /// [`Self::traffic_mirror_filter_rule_at`]), the way
    /// [`Self::insert_traffic_mirror_filter`] enforces its own, so a writer that forgets one
    /// cannot store a rule this store could not report.
    ///
    /// The id is minted here rather than by the writer, so a rule never shares an id with
    /// another resource of this store, and the record is written with its id and its filter's
    /// id on it so no reader has to learn either twice. The rule lands in its direction's
    /// list in ASCENDING RULE NUMBER order: "The rules are processed in ascending order by
    /// rule number" (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`), so that is the order
    /// a filter's rules are stored and reported in, whatever order they were created in.
    pub fn insert_traffic_mirror_filter_rule(
        &mut self,
        mut rule: TrafficMirrorFilterRule,
        client_token: Option<&str>,
    ) -> Option<String> {
        let traffic_mirror_filter_id = rule.traffic_mirror_filter_id.clone()?;
        let traffic_direction = rule.traffic_direction.clone()?;
        if !matches!(
            traffic_direction.as_str(),
            TRAFFIC_DIRECTION_INGRESS | TRAFFIC_DIRECTION_EGRESS
        ) {
            return None;
        }
        if !self
            .traffic_mirror_filters
            .contains_key(&traffic_mirror_filter_id)
            || !self.has_traffic_mirror_filter_rule_capacity(&rule, client_token)
        {
            return None;
        }
        // One number in one direction of one filter names one rule, so the store itself never
        // holds a second rule at a number it already has one at.
        if self
            .traffic_mirror_filter_rule_at(
                &traffic_mirror_filter_id,
                &traffic_direction,
                rule.rule_number.unwrap_or_default(),
            )
            .is_some()
        {
            return None;
        }
        let traffic_mirror_filter_rule_id = self.next_id(TRAFFIC_MIRROR_FILTER_RULE_ID_PREFIX);
        rule.traffic_mirror_filter_rule_id = Some(traffic_mirror_filter_rule_id.clone());
        // What the store will be charged for this record, read off the rule BEFORE it moves
        // onto the filter: the write below is all-or-nothing, so the charge and the token
        // entry happen only once the record really is on the filter.
        let payload_bytes = traffic_mirror_filter_rule_payload_bytes(&rule, client_token);
        let filter = self
            .traffic_mirror_filters
            .get_mut(&traffic_mirror_filter_id)?;
        let rules = if traffic_direction == TRAFFIC_DIRECTION_EGRESS {
            &mut filter.egress_filter_rules
        } else {
            &mut filter.ingress_filter_rules
        }
        .get_or_insert_with(Vec::new);
        rules.push(rule);
        rules.sort_by_key(|stored| stored.rule_number.unwrap_or_default());
        self.add_stored_bytes(payload_bytes);
        if let Some(token) = client_token {
            self.traffic_mirror_filter_rule_client_tokens
                .insert(token.to_string(), traffic_mirror_filter_rule_id.clone());
        }
        Some(traffic_mirror_filter_rule_id)
    }

    /// Give the store's byte budget back the bytes the rules of `filter` held, and stop their
    /// client tokens naming them.
    ///
    /// A rule lives on its filter, so a filter's removal takes its rules with it: the bytes go
    /// back on exactly the terms [`Self::insert_traffic_mirror_filter_rule`] took them on
    /// ([`traffic_mirror_filter_rule_payload_bytes`]), and a token that named one of those
    /// rules mints a new rule next time rather than reporting one nothing holds — the same
    /// contract [`Self::remove_traffic_mirror_filter`] keeps for the filter's own token.
    pub(super) fn release_traffic_mirror_filter_rules(&mut self, filter: &TrafficMirrorFilter) {
        let rules: Vec<&TrafficMirrorFilterRule> = filter
            .ingress_filter_rules
            .iter()
            .chain(filter.egress_filter_rules.iter())
            .flatten()
            .collect();
        for rule in rules {
            let client_token = rule
                .traffic_mirror_filter_rule_id
                .as_deref()
                .and_then(|rule_id| {
                    self.traffic_mirror_filter_rule_client_tokens
                        .iter()
                        .find(|(_, named)| named.as_str() == rule_id)
                        .map(|(token, _)| token.clone())
                });
            if let Some(token) = client_token.as_deref() {
                self.traffic_mirror_filter_rule_client_tokens.remove(token);
            }
            self.release_stored_bytes(traffic_mirror_filter_rule_payload_bytes(
                rule,
                client_token.as_deref(),
            ));
        }
    }
}
