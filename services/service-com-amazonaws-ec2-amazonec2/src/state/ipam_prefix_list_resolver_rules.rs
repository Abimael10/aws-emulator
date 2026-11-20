// SPDX-License-Identifier: CC0-1.0

//! The CIDR selection rules an IPAM prefix list resolver selects CIDRs by: how the rule set a
//! request names is READ, which IPAM scopes those rules may select from, and what a
//! modification does to a stored resolver.
//!
//! "CIDR selection rules define the business logic for selecting CIDRs from IPAM. If a CIDR
//! matches any of the rules, it will be included. If a rule has multiple conditions, the CIDR
//! has to match every condition of that rule" (`IpamPrefixListResolverRuleRequest`) — so a rule
//! is stored as the request gave it once its own values are values, which is one reading both
//! the create that first accepts a rule set and the modify that REPLACES it go through.
//!
//! "You can update the description and CIDR selection rules" and "these rules replace the
//! existing rules entirely" (`ModifyIpamPrefixListResolver`,
//! `ModifyIpamPrefixListResolverRequest$Rules`): a modification carries the members it changes
//! and leaves every other one as it stands, exactly as [`IpamModification`] does for an IPAM,
//! and the resolver it leaves behind is in the non-terminal `modify-in-progress` state the
//! clock then owes `modify-complete`.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;
use crate::ops::types::{IpamPrefixListResolverRuleCondition, IpamPrefixListResolverRuleRequest};

/// Read the rule set a request names, as the resolver will report it — or refuse the request.
///
/// The ONE reading of a requested rule set, which `CreateIpamPrefixListResolver` accepts its
/// `Rules` by and `ModifyIpamPrefixListResolver` replaces a resolver's with, so the two cannot
/// disagree about which rules an IPAM prefix list resolver may hold:
///
/// * the COUNT is bounded first ([`MAX_IPAM_PREFIX_LIST_RESOLVER_RULES`]), before any work
///   proportional to it, because the rules a request names are kept for the resolver's
///   lifetime;
/// * every CIDR a rule carries is read by the one CIDR reader this crate keeps
///   ([`cidr_block_is_well_formed`]) — a `StaticCidr` is "a fixed list of CIDRs"
///   (`IpamPrefixListResolverRuleRequest$StaticCidr`) and a condition's `Cidr` is an address
///   prefix too;
/// * the rule type, the resource type and a condition's operation are modelled enums, so a
///   spelling outside them was refused before any handler ran, and the IPAM scope a rule names
///   is resolved against the store by [`ComAmazonawsEc2Amazonec2State::require_ipam_prefix_list_resolver_rule_scopes`].
pub fn read_ipam_prefix_list_resolver_rules<E>(
    requested: &[IpamPrefixListResolverRuleRequest],
) -> Result<Vec<IpamPrefixListResolverRule>, Scaffolded<E>> {
    if requested.len() > MAX_IPAM_PREFIX_LIST_RESOLVER_RULES {
        return Err(invalid_parameter_value(format!(
            "The request carries {count} CIDR selection rules, above the maximum of {max}",
            count = requested.len(),
            max = MAX_IPAM_PREFIX_LIST_RESOLVER_RULES,
        )));
    }
    requested.iter().map(read_rule).collect()
}

/// One requested CIDR selection rule as the resolver reports it, with every value the request
/// may not name checked first. See [`read_ipam_prefix_list_resolver_rules`], its one caller.
fn read_rule<E>(
    requested: &IpamPrefixListResolverRuleRequest,
) -> Result<IpamPrefixListResolverRule, Scaffolded<E>> {
    if let Some(cidr) = requested
        .static_cidr
        .as_deref()
        .filter(|cidr| !cidr_block_is_well_formed(cidr))
    {
        return Err(invalid_parameter_value(format!(
            "Invalid value '{cidr}' for StaticCidr. It must be an address prefix in CIDR \
             notation"
        )));
    }
    let mut conditions: Vec<IpamPrefixListResolverRuleCondition> = Vec::new();
    for condition in requested.conditions.iter().flatten() {
        if let Some(cidr) = condition
            .cidr
            .as_deref()
            .filter(|cidr| !cidr_block_is_well_formed(cidr))
        {
            return Err(invalid_parameter_value(format!(
                "Invalid value '{cidr}' for Cidr. It must be an address prefix in CIDR notation"
            )));
        }
        conditions.push(IpamPrefixListResolverRuleCondition {
            operation: Some(condition.operation.clone()),
            ipam_pool_id: condition.ipam_pool_id.clone(),
            resource_id: condition.resource_id.clone(),
            resource_owner: condition.resource_owner.clone(),
            resource_region: condition.resource_region.clone(),
            resource_tag: condition.resource_tag.as_ref().map(|tag| IpamResourceTag {
                key: tag.key.clone(),
                value: tag.value.clone(),
            }),
            cidr: condition.cidr.clone(),
        });
    }
    Ok(IpamPrefixListResolverRule {
        rule_type: Some(requested.rule_type.clone()),
        static_cidr: requested.static_cidr.clone(),
        ipam_scope_id: requested.ipam_scope_id.clone(),
        resource_type: requested.resource_type.clone(),
        conditions: Some(conditions),
    })
}

/// The caller-chosen bytes of a stored resolver that a modification can change: its description
/// and the CIDR selection rules it holds.
///
/// The ONE definition of that charge, so what
/// [`ComAmazonawsEc2Amazonec2State::modify_ipam_prefix_list_resolver`] gives back for the record
/// it replaces is the same number a later modification of the replacement gives back for it —
/// the same contract [`ipam_modifiable_bytes`] has for an IPAM. The tags, the client token and
/// the fixed per-record overhead are deliberately NOT counted: no modification changes them, so
/// counting them would charge and refund the same figure twice.
pub fn ipam_prefix_list_resolver_modifiable_bytes(
    resolver: &IpamPrefixListResolver,
    rules: &[IpamPrefixListResolverRule],
) -> usize {
    resolver
        .description
        .as_deref()
        .unwrap_or_default()
        .len()
        .saturating_add(ipam_prefix_list_resolver_rule_bytes(rules))
}

/// What one `ModifyIpamPrefixListResolver` asks to be changed about a stored resolver.
///
/// A member left `None` here is one the REQUEST did not carry, which leaves the stored value
/// exactly as it was rather than returning it to the default a create would have given it — the
/// same contract [`IpamModification`] has for an IPAM. The rules are one whole SET rather than
/// additions and removals, because "these rules replace the existing rules entirely"
/// (`ModifyIpamPrefixListResolverRequest$Rules`).
#[derive(Debug, Clone)]
pub struct IpamPrefixListResolverModification {
    /// "The ID of the IPAM prefix list resolver to modify"
    /// (`ModifyIpamPrefixListResolverRequest$IpamPrefixListResolverId`).
    pub ipam_prefix_list_resolver_id: String,
    /// "A new description for the IPAM prefix list resolver"
    /// (`ModifyIpamPrefixListResolverRequest$Description`), or `None` when the request named
    /// none.
    pub description: Option<String>,
    /// "The updated CIDR selection rules for the resolver"
    /// (`ModifyIpamPrefixListResolverRequest$Rules`), already read by
    /// [`read_ipam_prefix_list_resolver_rules`] — or `None` when the request named no rule set,
    /// which leaves the one the resolver holds.
    pub rules: Option<Vec<IpamPrefixListResolverRule>>,
}

impl IpamPrefixListResolverModification {
    /// The resolver this modification leaves behind, from the one it is applied to.
    ///
    /// The ONE definition of what a modification DOES to a resolver, so the figure a handler
    /// charges to the store's byte budget is exactly the one the write stores. Every member the
    /// request did not carry is copied across unchanged, and every member the resolver's own
    /// creation derived — its id, its ARN, its owner, the IPAM it selects CIDRs from, its
    /// address family and its version creation status — is carried over untouched, since none of
    /// them follows from this request. The state is left as it stands here;
    /// [`ComAmazonawsEc2Amazonec2State::modify_ipam_prefix_list_resolver`] is what moves it, so
    /// no path can store a state the clock knows nothing about.
    pub fn applied_to(&self, resolver: &IpamPrefixListResolver) -> IpamPrefixListResolver {
        let mut applied = resolver.clone();
        if self.description.is_some() {
            applied.description = self.description.clone();
        }
        applied
    }

    /// The CIDR selection rules the resolver holds once this modification is applied: the set
    /// the request named, or the ones it already held when the request named none.
    pub fn rules_after(
        &self,
        held: &[IpamPrefixListResolverRule],
    ) -> Vec<IpamPrefixListResolverRule> {
        self.rules.clone().unwrap_or_else(|| held.to_vec())
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// Refuse a rule set that selects from an IPAM scope this account holds none of.
    ///
    /// "The ID of the IPAM scope from which to select CIDRs"
    /// (`IpamPrefixListResolverRuleRequest$IpamScopeId`): a rule selecting from a scope the
    /// account holds none of selects from nothing, so it is refused with the scope's own
    /// `NotFound` code rather than stored — the one reading of that question, asked by the create
    /// that first accepts a rule set and by the modify that replaces it.
    pub fn require_ipam_prefix_list_resolver_rule_scopes<E>(
        &self,
        rules: &[IpamPrefixListResolverRule],
    ) -> Result<(), Scaffolded<E>> {
        match rules
            .iter()
            .filter_map(|rule| rule.ipam_scope_id.as_deref())
            .find(|scope_id| !self.ipam_scopes.contains_key(*scope_id))
        {
            Some(unknown) => Err(invalid_ipam_scope_id_not_found(unknown)),
            None => Ok(()),
        }
    }

    /// The CIDR selection rules the resolver `resolver_id` names holds right now.
    ///
    /// The modelled [`IpamPrefixListResolver`] shape carries no member for them, so they live
    /// beside the resolver ([`Self::ipam_prefix_list_resolver_rules`]); this is the one reading
    /// of "the rules this resolver holds", which a modification's refund is computed from.
    pub fn ipam_prefix_list_resolver_rules_of(
        &self,
        resolver_id: &str,
    ) -> Vec<IpamPrefixListResolverRule> {
        self.ipam_prefix_list_resolver_rules
            .get(resolver_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Apply one `ModifyIpamPrefixListResolver` to the resolver it names and answer the record it
    /// leaves behind — or `None` when this store holds no resolver of that id and nothing was
    /// written.
    ///
    /// The ONE writer of a resolver modification, so what `ModifyIpamPrefixListResolver` answers
    /// is the stored record a later `DescribeIpamPrefixListResolvers` of the same resolver
    /// reports. What the modification CHANGES is
    /// [`IpamPrefixListResolverModification::applied_to`] and its rule set
    /// [`IpamPrefixListResolverModification::rules_after`]; what this adds is everything the
    /// store owns:
    ///
    /// * the resolver is left in the NON-TERMINAL [`IPAM_STATE_MODIFY_IN_PROGRESS`] state with
    ///   the move to [`IPAM_STATE_MODIFY_COMPLETE`] recorded in
    ///   [`Self::ipam_prefix_list_resolver_modify_complete_at`] one [`TRANSITION_DELAY`] out and
    ///   applied by [`Self::materialize_due_transitions`], so a caller acting straight after the
    ///   call still sees the transient state and nothing is slept for;
    /// * any move the clock still owed this resolver's CREATION is superseded: a resolver this
    ///   call has moved is not going to land in the state its creation was moving it to;
    /// * the store's byte budget gives back what the replaced record held and is charged what the
    ///   new one holds ([`ipam_prefix_list_resolver_modifiable_bytes`]), so the budget tracks
    ///   exactly the caller-chosen text that is stored.
    ///
    /// A caller must have asked [`Self::would_exceed_stored_bytes_after_refund`] first: this
    /// method is the single write path, and it is not where a request is refused.
    pub fn modify_ipam_prefix_list_resolver(
        &mut self,
        modification: &IpamPrefixListResolverModification,
    ) -> Option<IpamPrefixListResolver> {
        let resolver_id = modification.ipam_prefix_list_resolver_id.clone();
        let current = self.ipam_prefix_list_resolvers.get(&resolver_id)?.clone();
        let held_rules = self.ipam_prefix_list_resolver_rules_of(&resolver_id);
        let rules = modification.rules_after(&held_rules);
        let mut applied = modification.applied_to(&current);
        applied.state = Some(IPAM_STATE_MODIFY_IN_PROGRESS.to_string());

        self.release_stored_bytes(ipam_prefix_list_resolver_modifiable_bytes(
            &current,
            &held_rules,
        ));
        self.add_stored_bytes(ipam_prefix_list_resolver_modifiable_bytes(&applied, &rules));

        self.ipam_prefix_list_resolver_rules
            .insert(resolver_id.clone(), rules);
        self.ipam_prefix_list_resolvers
            .insert(resolver_id.clone(), applied.clone());
        self.ipam_prefix_list_resolver_create_complete_at
            .remove(&resolver_id);
        self.ipam_prefix_list_resolver_modify_complete_at
            .insert(resolver_id, Instant::now() + TRANSITION_DELAY);
        Some(applied)
    }

    /// Apply the move the clock owes a modified resolver, as of `now`.
    ///
    /// A resolver a `ModifyIpamPrefixListResolver` left [`IPAM_STATE_MODIFY_IN_PROGRESS`] was
    /// [`IPAM_STATE_MODIFY_COMPLETE`] at the instant that call recorded for it, whether or not
    /// anybody called again — so the move is applied from [`Self::materialize_due_transitions`],
    /// its one caller, before anything reads the store.
    pub(super) fn materialize_due_ipam_prefix_list_resolver_modifications(
        &mut self,
        now: Instant,
    ) {
        let resolvers = &mut self.ipam_prefix_list_resolvers;
        self.ipam_prefix_list_resolver_modify_complete_at
            .retain(|resolver_id, due_at| {
                if now < *due_at {
                    return true;
                }
                if let Some(resolver) = resolvers.get_mut(resolver_id) {
                    if resolver.state.as_deref() == Some(IPAM_STATE_MODIFY_IN_PROGRESS) {
                        resolver.state = Some(IPAM_STATE_MODIFY_COMPLETE.to_string());
                    }
                }
                // The transition is applied once; nothing is owed on this resolver any more.
                false
            });
    }
}
