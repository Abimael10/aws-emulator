// SPDX-License-Identifier: CC0-1.0

//! How many LOAD BALANCERS one target group may be forwarded to by.
//!
//! A target group belongs to one load balancer: `TargetGroup$LoadBalancerArns`
//! (`smithy_query(query="shape",shape="TargetGroup")`) is "[t]he Amazon Resource Names
//! (ARN) of the load balancers that route traffic to this target group", and every
//! operation that ATTACHES an action to a listener or a rule models
//! `TargetGroupAssociationLimitException` — "You've reached the limit on the number of load
//! balancers per target group" — for the request that would attach a group a second one
//! already routes to.
//!
//! So the limit is read wherever a forward action is SET: a listener's default actions and
//! a rule's actions, on the create that first stores them and on the modification that
//! replaces them. A group ALREADY routed to by the load balancer the request names is not
//! at the limit — adding a second listener or rule of the same load balancer forwarding
//! there changes nothing about how many load balancers route to it — and a group nothing
//! routes to any more (its listener deleted, its actions pointed elsewhere) is free again,
//! because the association a target group reports is derived from the listeners that exist
//! rather than accumulated (`super::ElbState::refresh_target_group_associations`).

use super::{
    action_target_group_arns, Action,
    ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State as ElbState,
};

/// The message the refusal states: the target groups of the REQUEST that are already
/// routed to by another load balancer, quoted in the order the actions named them.
///
/// Recorded behaviour, and worded about the TARGET GROUPS rather than about the load
/// balancer: the ARNs are what the caller has to change, and the load balancer that holds
/// each one is not named.
pub fn target_group_association_limit_message<'a>(
    target_group_arns: impl IntoIterator<Item = &'a str>,
) -> String {
    let named: Vec<&str> = target_group_arns.into_iter().collect();
    format!(
        "The following target groups cannot be associated with more than one load \
         balancer: {}",
        named.join(", ")
    )
}

/// Every target group `actions` forwards to that a load balancer OTHER than
/// `load_balancer_arn` already routes to, in the order the actions name them.
///
/// Empty for a request that would take no target group past the limit, which is what makes
/// it the condition each operation refuses on. A target group this store does not hold is
/// passed over: that one is the operation's own `TargetGroupNotFoundException`.
pub fn target_groups_routed_elsewhere(
    store: &ElbState,
    load_balancer_arn: &str,
    actions: &[Action],
) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for target_group_arn in action_target_group_arns(actions) {
        let Some(group) = store.target_groups.get(&target_group_arn) else {
            continue;
        };
        let elsewhere = group
            .load_balancer_arns
            .iter()
            .any(|routing| routing != load_balancer_arn);
        if elsewhere && !taken.contains(&target_group_arn) {
            taken.push(target_group_arn);
        }
    }
    taken
}
