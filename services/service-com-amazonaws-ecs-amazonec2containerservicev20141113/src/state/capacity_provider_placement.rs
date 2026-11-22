// SPDX-License-Identifier: CC0-1.0

//! WHICH capacity provider of a strategy places each task, and the launch type the task it
//! places reports.
//!
//! A `capacityProviderStrategy` is not one provider but a distribution over several, and
//! the `CapacityProviderStrategyItem` members state it: `base` "designates how many tasks,
//! at a minimum, to run on the specified capacity provider" and is satisfied first ("Base
//! requirements are satisfied first before weight distribution"), and `weight` "designates
//! the relative percentage of the total number of tasks launched that should use the
//! specified capacity provider", "taken into consideration after the `base` value, if
//! defined, is satisfied". So a placement of several tasks walks the strategy task by task:
//! each task goes to the first provider still short of its `base`, and once every `base` is
//! met to the provider whose share of the remainder is furthest behind its `weight` — "any
//! capacity providers with a weight of `0` can't be used to place tasks" beyond their base.
//!
//! The LAUNCH TYPE is the infrastructure the chosen provider binds onto: `Task$launchType`
//! is "The infrastructure where your task runs on", and the Fargate providers every account
//! holds ([`MANAGED_CAPACITY_PROVIDERS`]) run tasks on Fargate whichever of the two placed
//! them, so a task placed through `FARGATE_SPOT` reports the Fargate launch type beside the
//! provider's own name in `capacityProviderName`. A provider over a caller's Auto Scaling
//! group runs them on the Amazon EC2 instances that group registers, and a managed-instances
//! provider on the capacity Amazon ECS runs for the cluster — the remaining two values of
//! `LaunchType` (`smithy_query(query="shape", shape="LaunchType")`).
//!
//! One chooser and one projection for every placement that routes tasks through a strategy:
//! `RunTask`'s own batch, and the task a service's or a task set's scheduler places.

use super::{
    capacity_provider_name_from_reference, CapacityProviderStrategyItem,
    ComAmazonawsEcsAmazonec2containerservicev20141113State, CAPACITY_PROVIDER_TYPE_EC2_AUTOSCALING,
    CAPACITY_PROVIDER_TYPE_MANAGED_INSTANCES, DEFAULT_LAUNCH_TYPE, LAUNCH_TYPE_FARGATE,
    LAUNCH_TYPE_MANAGED_INSTANCES, MANAGED_CAPACITY_PROVIDERS,
};

/// Which entry of `strategy` the NEXT task is placed through, given how many tasks each of
/// its entries already holds (`placed`, one count per entry in the strategy's own order),
/// or `None` for a placement that names no strategy at all.
///
/// The two documented steps, in order: a provider short of its `base` takes the task, the
/// entries being weighed in the order the strategy states them; otherwise the task goes to
/// the entry whose tasks BEYOND its base are furthest behind its weight, so two entries of
/// equal weight end up with equal shares and one with four times the weight of another ends
/// up with four times its tasks.
///
/// A strategy whose bases are all met and whose entries all carry weight `0` can place no
/// further task under the rule above, and Amazon ECS refuses such a request outright ("If
/// you specify multiple capacity providers in a strategy that all have a weight of `0`, any
/// `RunTask` or `CreateService` actions using the capacity provider strategy will fail").
/// That refusal is not stated here, because this crate holds no wording for it and a refusal
/// worded differently is its own defect; the task is placed through the strategy's first
/// entry instead, so it is still attributed to a provider the strategy names. This is the
/// ONE case the chooser does not decide from the two members.
pub fn next_strategy_placement(
    strategy: &[CapacityProviderStrategyItem],
    placed: &[usize],
) -> Option<usize> {
    if strategy.is_empty() {
        return None;
    }
    let held = |index: usize| placed.get(index).copied().unwrap_or(0);
    let base = |index: usize| usize::try_from(strategy[index].base).unwrap_or(0);
    if let Some(index) = (0..strategy.len()).find(|&index| held(index) < base(index)) {
        return Some(index);
    }
    // The share each entry has taken of the tasks placed beyond the bases, compared by
    // cross-multiplying rather than by dividing, so no rounding puts two entries on one
    // share: entry `left` is further behind than `right` when
    // `(beyond(left) + 1) / weight(left)` is the smaller of the two. Of two entries equally
    // far behind, the one the strategy states first takes the task.
    let beyond = |index: usize| u128::try_from(held(index).saturating_sub(base(index))).unwrap_or(0);
    let weight = |index: usize| u128::try_from(strategy[index].weight).unwrap_or(0);
    let behind = (0..strategy.len())
        .filter(|&index| strategy[index].weight > 0)
        .min_by(|&left, &right| {
            ((beyond(left) + 1) * weight(right)).cmp(&((beyond(right) + 1) * weight(left)))
        });
    behind.or(Some(0))
}

/// How many of `placed` — the capacity provider each task already placed was placed
/// through — belong to each entry of `strategy`, in the strategy's own order.
///
/// Both sides are read as the provider they REFER to
/// ([`capacity_provider_name_from_reference`]), so a strategy that names a provider by ARN
/// still counts the tasks placed through it.
pub fn strategy_distribution<'a>(
    strategy: &[CapacityProviderStrategyItem],
    placed: impl Iterator<Item = &'a str>,
) -> Vec<usize> {
    let names: Vec<String> = strategy
        .iter()
        .map(|item| capacity_provider_name_from_reference(&item.capacity_provider))
        .collect();
    let mut counts = vec![0_usize; strategy.len()];
    for provider in placed {
        let provider = capacity_provider_name_from_reference(provider);
        if let Some(index) = names.iter().position(|name| *name == provider) {
            counts[index] += 1;
        }
    }
    counts
}

/// The two `CapacityProviderType` values that bind onto FARGATE capacity. The enum spells
/// each Fargate class with the predefined provider's own name
/// (`smithy_query(query="shape", shape="CapacityProviderType")` -> EC2_AUTOSCALING, FARGATE,
/// FARGATE_SPOT, MANAGED_INSTANCES), so these two spellings coincide with
/// [`MANAGED_CAPACITY_PROVIDERS`] — but this list is read against a provider's `type` and
/// that one against a provider's NAME, which are different questions.
const FARGATE_CAPACITY_PROVIDER_TYPES: [&str; 2] = ["FARGATE", "FARGATE_SPOT"];

/// The launch type a task placed through a capacity provider of `provider_type` runs on, or
/// `None` for a type that names no infrastructure this API reports.
///
/// `CapacityProvider$type` is the class of capacity the provider binds onto, and that class
/// is what `Task$launchType` reports: the Fargate classes run tasks on Fargate, a provider
/// over a caller's Auto Scaling group on the Amazon EC2 instances it registers, and a
/// managed-instances provider on the capacity Amazon ECS runs.
pub fn launch_type_through_capacity_provider(provider_type: Option<&str>) -> Option<&'static str> {
    let provider_type = provider_type?;
    if FARGATE_CAPACITY_PROVIDER_TYPES.contains(&provider_type) {
        return Some(LAUNCH_TYPE_FARGATE);
    }
    if provider_type == CAPACITY_PROVIDER_TYPE_MANAGED_INSTANCES {
        return Some(LAUNCH_TYPE_MANAGED_INSTANCES);
    }
    (provider_type == CAPACITY_PROVIDER_TYPE_EC2_AUTOSCALING).then_some(DEFAULT_LAUNCH_TYPE)
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The launch type a task placed through the capacity provider `reference` names runs
    /// on, resolved against this store: the Fargate providers every account holds are
    /// recognised by name ([`MANAGED_CAPACITY_PROVIDERS`], which nothing creates), and every
    /// other name through the `type` of the provider stored under it.
    pub fn capacity_provider_launch_type(&self, reference: &str) -> Option<&'static str> {
        let name = capacity_provider_name_from_reference(reference);
        if MANAGED_CAPACITY_PROVIDERS.contains(&name.as_str()) {
            return Some(LAUNCH_TYPE_FARGATE);
        }
        launch_type_through_capacity_provider(
            self.capacity_providers
                .get(&name)
                .and_then(|provider| provider.provider_type.as_deref()),
        )
    }
}
