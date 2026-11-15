// SPDX-License-Identifier: CC0-1.0

//! Resolving a tagging request's `ResourceArn` to the tag set of the resource it names.
//!
//! One resolver for the whole tagging surface — `ListTagsForResource`, `TagResource`
//! and `UntagResource` — so a resource type is reachable through all three or through
//! none of them.

use super::*;

impl ComAmazonawsConfigserviceStarlingdoveserviceState {
    /// The tags of the resource `arn` identifies, or `None` when this store holds no
    /// resource with that ARN.
    ///
    /// This is the ONE place an ARN is resolved to a tag set, so the tagging
    /// operations agree with each other and with the resource's own read path: the
    /// tags live ON the resource, which is also why deleting the resource deletes its
    /// tags (`TagResource`: "When a resource is deleted, the tags associated with that
    /// resource are deleted as well").
    ///
    /// EVERY taggable Config resource type this emulator persists is reached here —
    /// the types `ListTagsForResourceRequest$ResourceArn` documents (configuration
    /// recorder, Config rule, organization Config rule, conformance pack, organization
    /// conformance pack, configuration aggregator, aggregation authorization, stored
    /// query, connector), minus the ones whose write operation is still scaffolded and
    /// which therefore cannot be in the store at all. An ARN of a type nothing
    /// persists resolves to `None` exactly as the ARN of a deleted resource does, so
    /// the not-found refusal means what it says: no such resource is held here now.
    pub fn tags_of(&self, arn: &str) -> Option<&TagSet> {
        if let Some(recorder) = self.recorders.values().find(|entry| entry.arn == arn) {
            return Some(&recorder.tags);
        }
        if let Some(rule) = self.config_rules.values().find(|entry| entry.arn == arn) {
            return Some(&rule.tags);
        }
        if let Some(aggregator) = self
            .configuration_aggregators
            .values()
            .find(|entry| entry.arn == arn)
        {
            return Some(&aggregator.tags);
        }
        if let Some(authorization) = self
            .aggregation_authorizations
            .values()
            .find(|entry| entry.arn == arn)
        {
            return Some(&authorization.tags);
        }
        if let Some(stored_query) = self.stored_queries.values().find(|entry| entry.arn == arn) {
            return Some(&stored_query.tags);
        }
        if let Some(pack) = self.conformance_packs.values().find(|entry| entry.arn == arn) {
            return Some(&pack.tags);
        }
        if let Some(connector) = self.connectors.values().find(|entry| entry.arn == arn) {
            return Some(&connector.tags);
        }
        None
    }

    /// The tags of the resource `arn` identifies, for a request that changes them.
    /// See [`Self::tags_of`].
    pub fn tags_of_mut(&mut self, arn: &str) -> Option<&mut TagSet> {
        if let Some(recorder) = self.recorders.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut recorder.tags);
        }
        if let Some(rule) = self.config_rules.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut rule.tags);
        }
        if let Some(aggregator) = self
            .configuration_aggregators
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut aggregator.tags);
        }
        if let Some(authorization) = self
            .aggregation_authorizations
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut authorization.tags);
        }
        if let Some(stored_query) = self
            .stored_queries
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut stored_query.tags);
        }
        if let Some(pack) = self
            .conformance_packs
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut pack.tags);
        }
        if let Some(connector) = self.connectors.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut connector.tags);
        }
        None
    }
}
