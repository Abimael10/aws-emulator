// SPDX-License-Identifier: CC0-1.0

//! The EFFECTIVE form of a routing rule's actions: the request's actions with every
//! optional setting the caller left out resolved to the value the rule is served under.
//!
//! A module of its own under [`crate::state`], because this is the one reading both
//! writers of the resource (`CreateRoutingRule` and `PutRoutingRule`) share, and because
//! what a rule's action is served UNDER is state rather than a projection: the effective
//! setting is stored with the rule, so every reader of it — the write's own response,
//! `GetRoutingRule`, `ListRoutingRules` — reports the same value, and a
//! read-modify-write that sends an action back unchanged keeps the setting it read.
//! [`crate::state`] re-exports every public item here.

use super::wire;

/// The setting an `invokeApi` action is served under when its request named none.
///
/// `RoutingRuleActionInvokeApi$StripBasePath` ("The strip base path setting") is an
/// optional boolean on the input, and a request that names no value for a boolean setting
/// asks for the service's own: API Gateway invokes the target api with the matched base
/// path still on the request path unless the caller asked for it to be stripped, so the
/// setting a rule written without one is served under is OFF.
pub const DEFAULT_STRIP_BASE_PATH: bool = false;

/// Resolve the actions of a routing-rule write into the actions to STORE.
///
/// Every member a request named is kept exactly as it wrote it — an action's `ApiId` and
/// `Stage` are the caller's reference to an api stage, carried verbatim — and every
/// optional setting it did not name takes the value the rule is served under
/// ([`DEFAULT_STRIP_BASE_PATH`]), so the stored rule describes one evaluable behaviour
/// rather than leaving a reader to guess at an absent setting. The list's order is the
/// caller's: `Actions` is a modelled list, not a map.
pub fn resolve_routing_rule_actions(
    requested: Vec<wire::RoutingRuleAction>,
) -> Vec<wire::RoutingRuleAction> {
    requested
        .into_iter()
        .map(|mut action| {
            action.invoke_api.strip_base_path.get_or_insert(DEFAULT_STRIP_BASE_PATH);
            action
        })
        .collect()
}
