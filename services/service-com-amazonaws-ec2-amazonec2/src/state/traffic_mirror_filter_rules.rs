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

use super::{
    cidr_block_is_well_formed, invalid_parameter_value, missing_parameter, tag_list_bytes,
    ComAmazonawsEc2Amazonec2State, MAX_PORT,
};
use crate::ops::types::{
    TrafficMirrorFilter, TrafficMirrorFilterRule, TrafficMirrorPortRange,
    TrafficMirrorPortRangeRequest,
};
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

/// The lowest `RuleNumber` a Traffic Mirror filter rule may carry.
///
/// "The number of the Traffic Mirror rule… The rules are processed in ascending order by rule
/// number" (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`): the rules of a filter are
/// NUMBERED, and a numbered rule set starts at one, so nothing below it names a rule. The
/// model puts no `@range` on the member, so a handler is the only place that floor can be
/// enforced — and it is the same floor whichever call writes the number, which is why it is
/// declared here rather than in one operation.
pub const MIN_TRAFFIC_MIRROR_FILTER_RULE_NUMBER: i32 = 1;

/// The `TrafficMirrorFilterRuleField` naming a rule's destination port range, as the modelled
/// enum spells it (`crate::ops::types::TRAFFIC_MIRROR_FILTER_RULE_FIELD_VALUES`).
pub const TRAFFIC_MIRROR_FILTER_RULE_FIELD_DESTINATION_PORT_RANGE: &str = "destination-port-range";

/// The `TrafficMirrorFilterRuleField` naming a rule's source port range, from the same
/// modelled enum as [`TRAFFIC_MIRROR_FILTER_RULE_FIELD_DESTINATION_PORT_RANGE`].
pub const TRAFFIC_MIRROR_FILTER_RULE_FIELD_SOURCE_PORT_RANGE: &str = "source-port-range";

/// The `TrafficMirrorFilterRuleField` naming a rule's protocol, from the same modelled enum.
pub const TRAFFIC_MIRROR_FILTER_RULE_FIELD_PROTOCOL: &str = "protocol";

/// The `TrafficMirrorFilterRuleField` naming a rule's description, from the same modelled
/// enum.
pub const TRAFFIC_MIRROR_FILTER_RULE_FIELD_DESCRIPTION: &str = "description";

/// EC2's code for a Traffic Mirror filter rule id that names no rule of the account.
///
/// EC2's model declares no error shape at all (see [`crate::scaffolded::ServiceError`]), and
/// the service answers `Invalid<Resource>Id.NotFound` for an id of a resource it cannot
/// resolve — the code the wire corpus records for an unresolvable Traffic Mirror filter rule
/// id, named the way its FILTER's own code is
/// ([`super::INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND`]).
pub const INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND: &str =
    "InvalidTrafficMirrorFilterRuleId.NotFound";

/// Refuse a request that named a Traffic Mirror filter rule id this account holds no rule
/// for: HTTP 400 [`INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND`].
///
/// `TrafficMirrorFilterRuleIdWithResolver` publishes no pattern
/// (`smithy_query(query="shape",shape="TrafficMirrorFilterRuleIdWithResolver")`), so there is
/// no spelling to call an id malformed by: one code covers an id of any shape, because an id
/// this service never minted names no rule of the account whether or not it reads like one.
/// The sentence is the one this resource family words that refusal with
/// ([`super::invalid_traffic_mirror_filter_id_not_found`]), built from the id the REQUEST gave
/// so a client reading the text sees which id it was.
pub fn invalid_traffic_mirror_filter_rule_id_not_found<E>(
    traffic_mirror_filter_rule_id: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND,
        400,
        format!("{traffic_mirror_filter_rule_id} does not exist"),
    )
}

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

/// The rules `filter` holds in the direction `traffic_direction` names, to be written, or
/// `None` when that is no direction the modelled `TrafficDirection` enum carries.
///
/// The writing counterpart of [`traffic_mirror_filter_rules_in_direction`], so the two sides
/// of that choice cannot disagree about which member a direction names: the list is created
/// empty for a direction the filter holds no rule in yet, so a caller can push into it.
fn traffic_mirror_filter_rules_in_direction_mut<'a>(
    filter: &'a mut TrafficMirrorFilter,
    traffic_direction: &str,
) -> Option<&'a mut Vec<TrafficMirrorFilterRule>> {
    let rules = match traffic_direction {
        TRAFFIC_DIRECTION_INGRESS => &mut filter.ingress_filter_rules,
        TRAFFIC_DIRECTION_EGRESS => &mut filter.egress_filter_rules,
        _ => return None,
    };
    Some(rules.get_or_insert_with(Vec::new))
}

/// `requested` read for its FORM as an address prefix in CIDR notation, or EC2's
/// `InvalidParameterValue` for a value that is no address prefix of either family.
///
/// "The destination CIDR block to assign to the Traffic Mirror rule"
/// (`CreateTrafficMirrorFilterRuleRequest$DestinationCidrBlock`) and the source block are
/// stored as the REQUEST spelled them — the modelled members say nothing about rewriting one —
/// so a block is read for its form only. The ONE reader of that form for a Traffic Mirror
/// rule's blocks, so the create and the modify cannot admit a different value or word the same
/// refusal differently; `parameter` is the member as EC2 names it in a message.
pub fn require_traffic_mirror_cidr_block<E>(
    requested: &str,
    parameter: &str,
) -> Result<(), Scaffolded<E>> {
    if cidr_block_is_well_formed(requested) {
        return Ok(());
    }
    Err(invalid_parameter_value(format!(
        "Value ({requested}) for parameter {parameter} is invalid. This is not a valid CIDR \
         block."
    )))
}

/// `requested` as the port range a Traffic Mirror rule stores, or the refusal the request
/// earns: `None` when the request named no range at all, which is a rule over every port.
///
/// A range is BOTH of its ports, so a request that named only one of them has not named a
/// range: what is missing from such a request is a LEAF of the member, and that leaf is what
/// the refusal names. A range that is given has to be a range of ports — each within the port
/// space, the first not above the last. The ONE reader of such a range, so the create and the
/// modify cannot admit a different range or word the same refusal differently; `parameter` is
/// the member as EC2 names it in a message.
pub fn requested_traffic_mirror_port_range<E>(
    requested: Option<&TrafficMirrorPortRangeRequest>,
    parameter: &str,
) -> Result<Option<TrafficMirrorPortRange>, Scaffolded<E>> {
    let Some(range) = requested.filter(|range| range.from_port.is_some() || range.to_port.is_some())
    else {
        return Ok(None);
    };
    let (Some(from_port), Some(to_port)) = (range.from_port, range.to_port) else {
        return Err(missing_parameter(format!(
            "The request must contain the parameter {parameter}.{half}",
            half = if range.from_port.is_none() {
                "fromPort"
            } else {
                "toPort"
            },
        )));
    };
    for (member, port) in [("fromPort", from_port), ("toPort", to_port)] {
        if !(0..=MAX_PORT).contains(&port) {
            return Err(invalid_parameter_value(format!(
                "Value ({port}) for parameter {parameter}.{member} is invalid. A port is \
                 between 0 and {max}",
                max = MAX_PORT,
            )));
        }
    }
    if from_port > to_port {
        return Err(invalid_parameter_value(format!(
            "Value ({from_port}, {to_port}) for parameter {parameter} is invalid. The first \
             port of the range must not be above the last"
        )));
    }
    Ok(Some(TrafficMirrorPortRange {
        from_port: Some(from_port),
        to_port: Some(to_port),
    }))
}

/// What one `ModifyTrafficMirrorFilterRule` asks to be changed about a stored Traffic Mirror
/// filter rule.
///
/// A member left `None` here is one the REQUEST did not carry, which leaves the stored value
/// exactly as it was rather than returning it to the default a create would have given it —
/// the same contract [`super::IpamPoolModification`] has for an IPAM pool. The removed fields
/// are the one half of the request that asks for a property to be taken AWAY: "The properties
/// that you want to remove from the Traffic Mirror filter rule. When you remove a property
/// from a Traffic Mirror filter rule, the property is set to the default"
/// (`ModifyTrafficMirrorFilterRuleRequest$RemoveFields`), and the fields that can be named are
/// the modelled `TrafficMirrorFilterRuleField` enum's own members.
#[derive(Debug, Clone, Default)]
pub struct TrafficMirrorFilterRuleModification {
    /// "The ID of the Traffic Mirror rule"
    /// (`ModifyTrafficMirrorFilterRuleRequest$TrafficMirrorFilterRuleId`).
    pub traffic_mirror_filter_rule_id: String,
    /// "The type of traffic to assign to the rule" (`$TrafficDirection`), or `None` to leave
    /// the rule in the direction it already filters.
    pub traffic_direction: Option<String>,
    /// "The number of the Traffic Mirror rule" (`$RuleNumber`), or `None` to leave it at the
    /// number it is processed under.
    pub rule_number: Option<i32>,
    /// "The action to assign to the rule" (`$RuleAction`), or `None` to leave it as it is.
    pub rule_action: Option<String>,
    /// "The destination ports that are associated with the Traffic Mirror rule"
    /// (`$DestinationPortRange`), already read by [`requested_traffic_mirror_port_range`].
    pub destination_port_range: Option<TrafficMirrorPortRange>,
    /// "The port range to assign to the Traffic Mirror rule" (`$SourcePortRange`), likewise.
    pub source_port_range: Option<TrafficMirrorPortRange>,
    /// "The protocol, for example TCP, to assign to the Traffic Mirror rule" (`$Protocol`).
    pub protocol: Option<i32>,
    /// "The destination CIDR block to assign to the Traffic Mirror rule"
    /// (`$DestinationCidrBlock`).
    pub destination_cidr_block: Option<String>,
    /// "The source CIDR block to assign to the Traffic Mirror rule" (`$SourceCidrBlock`).
    pub source_cidr_block: Option<String>,
    /// "The description to assign to the Traffic Mirror rule" (`$Description`).
    pub description: Option<String>,
    /// "The properties that you want to remove from the Traffic Mirror filter rule"
    /// (`$RemoveFields`), each a member of the modelled `TrafficMirrorFilterRuleField` enum.
    pub removed_fields: Vec<String>,
}

impl TrafficMirrorFilterRuleModification {
    /// Whether this modification removes the rule property `field` names.
    ///
    /// The ONE reading of the removal list, so the handler's refusal of a field a request both
    /// sets and removes and the write itself cannot disagree about which fields are removed.
    pub fn removes(&self, field: &str) -> bool {
        self.removed_fields.iter().any(|removed| removed == field)
    }

    /// The Traffic Mirror filter rule this modification leaves behind, from the one it is
    /// applied to.
    ///
    /// The ONE definition of what a modification DOES to a rule, so what a handler checks and
    /// charges to the store's byte budget is exactly what the write stores. A property named
    /// in `RemoveFields` "is set to the default" (`$RemoveFields`) — the absent member a
    /// create that named none leaves — whatever else the request carries; every member the
    /// request neither set nor removed keeps the stored value; and the ids the service owns,
    /// the filter the rule lives on and the tags it was created with are carried over
    /// untouched, since none of them follows from this request.
    pub fn applied_to(&self, rule: &TrafficMirrorFilterRule) -> TrafficMirrorFilterRule {
        let mut applied = rule.clone();
        if self.traffic_direction.is_some() {
            applied.traffic_direction = self.traffic_direction.clone();
        }
        if self.rule_number.is_some() {
            applied.rule_number = self.rule_number;
        }
        if self.rule_action.is_some() {
            applied.rule_action = self.rule_action.clone();
        }
        if self.destination_cidr_block.is_some() {
            applied.destination_cidr_block = self.destination_cidr_block.clone();
        }
        if self.source_cidr_block.is_some() {
            applied.source_cidr_block = self.source_cidr_block.clone();
        }
        if self.removes(TRAFFIC_MIRROR_FILTER_RULE_FIELD_PROTOCOL) {
            applied.protocol = None;
        } else if self.protocol.is_some() {
            applied.protocol = self.protocol;
        }
        if self.removes(TRAFFIC_MIRROR_FILTER_RULE_FIELD_DESTINATION_PORT_RANGE) {
            applied.destination_port_range = None;
        } else if self.destination_port_range.is_some() {
            applied.destination_port_range = self.destination_port_range.clone();
        }
        if self.removes(TRAFFIC_MIRROR_FILTER_RULE_FIELD_SOURCE_PORT_RANGE) {
            applied.source_port_range = None;
        } else if self.source_port_range.is_some() {
            applied.source_port_range = self.source_port_range.clone();
        }
        if self.removes(TRAFFIC_MIRROR_FILTER_RULE_FIELD_DESCRIPTION) {
            applied.description = None;
        } else if self.description.is_some() {
            applied.description = self.description.clone();
        }
        applied
    }
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
        let rules = traffic_mirror_filter_rules_in_direction_mut(filter, &traffic_direction)?;
        rules.push(rule);
        rules.sort_by_key(|stored| stored.rule_number.unwrap_or_default());
        self.add_stored_bytes(payload_bytes);
        if let Some(token) = client_token {
            self.traffic_mirror_filter_rule_client_tokens
                .insert(token.to_string(), traffic_mirror_filter_rule_id.clone());
        }
        Some(traffic_mirror_filter_rule_id)
    }

    /// Apply `modification` to the Traffic Mirror filter rule it names and answer the rule as
    /// the store then holds it — `None` when the account holds no such rule, or when the
    /// direction it would be moved to is no direction the modelled enum carries.
    ///
    /// The ONE writer of a modified rule, the counterpart of
    /// [`Self::insert_traffic_mirror_filter_rule`] for a rule that already exists: what the
    /// record becomes is [`TrafficMirrorFilterRuleModification::applied_to`]'s answer, so a
    /// handler cannot check one rule and store another. A rule lives in its direction's own
    /// list on the filter, so a modification that moves it between directions takes it out of
    /// the list it was in and puts it in the other; either way the list it lands in stays in
    /// ASCENDING RULE NUMBER order — "the rules are processed in ascending order by rule
    /// number" (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`) — so a renumbered rule is
    /// reported where its new number puts it.
    ///
    /// The store's byte budget gives back what the replaced record held and charges what the
    /// new one holds ([`traffic_mirror_filter_rule_payload_bytes`]), so a caller cannot grow
    /// the store past [`super::MAX_STORED_BYTES`] by modifying one rule repeatedly; a handler
    /// asks [`Self::would_exceed_stored_bytes_after_refund`] about that net change first. The
    /// client token the rule was created under is untouched: it still names this rule.
    pub fn modify_traffic_mirror_filter_rule(
        &mut self,
        modification: &TrafficMirrorFilterRuleModification,
    ) -> Option<TrafficMirrorFilterRule> {
        let rule_id = modification.traffic_mirror_filter_rule_id.as_str();
        let current = self.traffic_mirror_filter_rule(rule_id)?.clone();
        let traffic_mirror_filter_id = current.traffic_mirror_filter_id.clone()?;
        let held_direction = current.traffic_direction.clone()?;
        let applied = modification.applied_to(&current);
        let direction = applied.traffic_direction.clone()?;
        if !matches!(
            direction.as_str(),
            TRAFFIC_DIRECTION_INGRESS | TRAFFIC_DIRECTION_EGRESS
        ) {
            return None;
        }
        let refund = traffic_mirror_filter_rule_payload_bytes(&current, None);
        let charge = traffic_mirror_filter_rule_payload_bytes(&applied, None);
        let filter = self
            .traffic_mirror_filters
            .get_mut(&traffic_mirror_filter_id)?;
        if let Some(held) = traffic_mirror_filter_rules_in_direction_mut(filter, &held_direction) {
            held.retain(|stored| {
                stored.traffic_mirror_filter_rule_id.as_deref() != Some(rule_id)
            });
        }
        let rules = traffic_mirror_filter_rules_in_direction_mut(filter, &direction)?;
        rules.push(applied.clone());
        rules.sort_by_key(|stored| stored.rule_number.unwrap_or_default());
        self.release_stored_bytes(refund);
        self.add_stored_bytes(charge);
        Some(applied)
    }

    /// Remove the Traffic Mirror filter rule `traffic_mirror_filter_rule_id` names from the
    /// filter it lives on and answer it as it was last reported, or `None` when the account
    /// holds no such rule.
    ///
    /// The ONE way a single rule leaves the store, the counterpart of
    /// [`Self::remove_traffic_mirror_filter`] for one rule of a filter: the answer is the
    /// record the removal took OUT of the store, so what a delete reports is what the readers
    /// reported. [`TrafficMirrorFilterRule`] carries no state member
    /// (`smithy_query(query="shape",shape="TrafficMirrorFilterRule")`), so there is no terminal
    /// state for a deleted rule to be reported in and the id simply stops resolving — for the
    /// filter's own rule set as well, since a rule lives in that set.
    ///
    /// The bytes the record held go back to the store's budget on exactly the terms
    /// [`Self::insert_traffic_mirror_filter_rule`] took them on
    /// ([`traffic_mirror_filter_rule_payload_bytes`]), and the client token that named this
    /// rule stops naming it, so a later create carrying it mints a new rule rather than
    /// reporting one nothing holds.
    pub fn remove_traffic_mirror_filter_rule(
        &mut self,
        traffic_mirror_filter_rule_id: &str,
    ) -> Option<TrafficMirrorFilterRule> {
        let rule = self
            .traffic_mirror_filter_rule(traffic_mirror_filter_rule_id)?
            .clone();
        let traffic_mirror_filter_id = rule.traffic_mirror_filter_id.clone()?;
        let traffic_direction = rule.traffic_direction.clone()?;
        let filter = self
            .traffic_mirror_filters
            .get_mut(&traffic_mirror_filter_id)?;
        let rules = traffic_mirror_filter_rules_in_direction_mut(filter, &traffic_direction)?;
        rules.retain(|stored| {
            stored.traffic_mirror_filter_rule_id.as_deref() != Some(traffic_mirror_filter_rule_id)
        });
        let client_token = self
            .traffic_mirror_filter_rule_client_tokens
            .iter()
            .find(|(_, named)| named.as_str() == traffic_mirror_filter_rule_id)
            .map(|(token, _)| token.clone());
        if let Some(token) = client_token.as_deref() {
            self.traffic_mirror_filter_rule_client_tokens.remove(token);
        }
        self.release_stored_bytes(traffic_mirror_filter_rule_payload_bytes(
            &rule,
            client_token.as_deref(),
        ));
        Some(rule)
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
