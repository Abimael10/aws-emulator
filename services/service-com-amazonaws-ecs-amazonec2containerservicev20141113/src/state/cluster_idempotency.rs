// SPDX-License-Identifier: CC0-1.0

//! Whether a repeated `CreateCluster` is the SAME create, or one that asks for a
//! different cluster under a name this account already holds.
//!
//! A cluster is identified by its name — `CreateCluster$clusterName` is the only thing a
//! later call names it by — so a create for a name that is already taken is not a second
//! cluster: it is the first create being made again. The repeat is answered with the
//! stored cluster only when it asks for the cluster that is stored; a repeat that carries
//! a DIFFERENT argument asks for a cluster this one is not, and neither creates a second
//! cluster nor rewrites the first — it is refused, with the stored record left exactly as
//! it stands ([`CLUSTER_ARGUMENTS_INCONSISTENT_MESSAGE`]).
//!
//! The comparison is over the arguments the repeat SUPPLIES: a member the request carries
//! nothing for asserts nothing about the cluster, which is why a bare re-create of a name
//! (`clusterName` and nothing else) is still the same create however the cluster was
//! configured. A list carries values only when it has entries, and the collections that
//! are KEYED — `tags` by [`Tag::key`] and `settings` by [`ClusterSetting::name`] — are
//! compared by those keys rather than by position, so the same arguments sent in another
//! order are still the same arguments.
//!
//! The same shape as the other creates in this crate whose name is the idempotency key:
//! [`capacity_provider_binding_fingerprint`](super::capacity_provider_binding_fingerprint)
//! decides it for a capacity provider, and `clientToken` for a service.

use super::{
    service_connect_defaults, CapacityProviderStrategyItem, ClusterConfiguration, ClusterRecord,
    ClusterSetting, Tag,
};

/// `InvalidParameterException$message` for a `CreateCluster` that repeats a name this
/// account holds while asking for something other than the cluster stored under it.
///
/// One fixed sentence: nothing from the request is interpolated, because the refusal is
/// about the request DISAGREEING with an earlier one rather than about any one value in
/// it. It tells the caller the repeat was not the same call, which is what a re-runnable
/// deploy branches on.
pub const CLUSTER_ARGUMENTS_INCONSISTENT_MESSAGE: &str =
    "Arguments on this idempotent request are inconsistent with arguments used in \
     previous request(s).";

/// The arguments of a `CreateCluster` request that configure the cluster — every input
/// member but `clusterName`, which is the identity the repeat is keyed on.
///
/// Borrowed straight off the deserialized request: an empty list and an absent one are
/// both "nothing supplied for this member", and `service_connect_namespace` is the
/// request's `serviceConnectDefaults.namespace` as it was sent.
#[derive(Debug, Default)]
pub struct RequestedClusterArguments<'a> {
    pub tags: &'a [Tag],
    pub settings: &'a [ClusterSetting],
    pub configuration: Option<&'a ClusterConfiguration>,
    pub capacity_providers: &'a [String],
    pub default_capacity_provider_strategy: &'a [CapacityProviderStrategyItem],
    pub service_connect_namespace: Option<&'a str>,
}

impl RequestedClusterArguments<'_> {
    /// The message the repeat is refused with when one of the arguments it supplies names
    /// something other than what `existing` carries, and `None` when every supplied
    /// argument is the one the stored cluster already has — the repeat that is the same
    /// create.
    pub fn inconsistency(&self, existing: &ClusterRecord) -> Option<&'static str> {
        let tags_differ =
            !self.tags.is_empty() && tag_pairs(self.tags) != tag_pairs(&existing.tags);
        // `settings` is keyed by name and carries only the settings a request speaks
        // about, so each one is weighed against the value stored under that NAME — the
        // value this cluster's create named or captured
        // ([`super::ComAmazonawsEcsAmazonec2containerservicev20141113State::captured_cluster_settings`]),
        // so a repeat that spells out the value the cluster carries is the same create and
        // one that asks for another value is not.
        let settings_differ = self.settings.iter().any(|setting| {
            let stored = setting
                .name
                .as_deref()
                .and_then(|name| existing.setting_value(name));
            stored.as_deref() != setting.value.as_deref()
        });
        let configuration_differs = self.configuration.is_some_and(|configuration| {
            // Both sides as `Option<&ClusterConfiguration>`, so the two renderings are of
            // the same type and differ only where the configurations do.
            format!("{:?}", Some(configuration)) != format!("{:?}", existing.configuration.as_ref())
        });
        // The association is a SET of provider names: which order they were sent in is
        // not part of what the cluster was created with.
        let providers_differ = !self.capacity_providers.is_empty()
            && sorted(self.capacity_providers.iter().map(String::as_str))
                != sorted(existing.capacity_providers.iter().map(String::as_str));
        let strategy_differs = !self.default_capacity_provider_strategy.is_empty()
            && sorted(
                self.default_capacity_provider_strategy
                    .iter()
                    .map(strategy_entry),
            ) != sorted(
                existing
                    .default_capacity_provider_strategy
                    .iter()
                    .map(strategy_entry),
            );
        // The namespace is compared as the cluster STORES it, so a repeat that asks for
        // the default to be cleared is weighed against a cluster that has none.
        let namespace_differs = self.service_connect_namespace.is_some_and(|namespace| {
            format!("{:?}", service_connect_defaults(namespace))
                != format!("{:?}", existing.service_connect_defaults)
        });

        (tags_differ
            || settings_differ
            || configuration_differs
            || providers_differ
            || strategy_differs
            || namespace_differs)
            .then_some(CLUSTER_ARGUMENTS_INCONSISTENT_MESSAGE)
    }
}

/// A tag set as what it means rather than as it was sent: its pairs ordered by key, so
/// two requests that carry the same tags compare equal whatever order they are in.
fn tag_pairs(tags: &[Tag]) -> Vec<(Option<&str>, Option<&str>)> {
    sorted(
        tags.iter()
            .map(|tag| (tag.key.as_deref(), tag.value.as_deref())),
    )
}

/// One `defaultCapacityProviderStrategy` entry as the three values that make it up — the
/// provider it names and the two numbers the scheduler weighs it by.
fn strategy_entry(item: &CapacityProviderStrategyItem) -> (&str, i32, i32) {
    (item.capacity_provider.as_str(), item.weight, item.base)
}

/// The values of an unordered collection in one canonical order, so a comparison of two
/// of them does not depend on the order either was written in.
fn sorted<T: Ord>(values: impl Iterator<Item = T>) -> Vec<T> {
    let mut sorted: Vec<T> = values.collect();
    sorted.sort();
    sorted
}
