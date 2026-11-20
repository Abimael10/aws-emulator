// SPDX-License-Identifier: CC0-1.0

//! Tags addressed by the ARN of the resource that carries them.
//!
//! One concern, so one file: Connect's tagging plane takes a resource ARN rather than
//! the instance-scoped path labels the rest of the API uses, so the lookup from "an ARN"
//! to "the tag map stored on whatever resource that ARN names" lives here once, beside
//! the records it walks, instead of being written again in each tagging operation.
//!
//! Every kind of resource this store holds that carries a `TagMap` is reachable through
//! it: the instance itself, the queues, flows and hours of operation inside it, and the
//! phone numbers claimed to the account. A record's ARN is minted when the record is
//! stored, so this resolves exactly the resources that exist — a resource a delete
//! retired, or one of an account or Region this store never minted for, resolves to
//! nothing.

use std::collections::BTreeMap;

use super::ComAmazonawsConnectAmazonconnectserviceState;

impl ComAmazonawsConnectAmazonconnectserviceState {
    /// The tags stored on the resource `resource_arn` names, or `None` when no stored
    /// resource carries that ARN.
    ///
    /// An untagged resource that DOES exist answers an empty map, which is a different
    /// answer from `None`: the first is a resource with no tags, the second is no
    /// resource at all.
    pub fn tags_of_resource(&self, resource_arn: &str) -> Option<&BTreeMap<String, String>> {
        if let Some(number) = self
            .phone_numbers
            .values()
            .find(|number| number.arn == resource_arn)
        {
            return Some(&number.tags);
        }
        for instance in self.instances.values() {
            if instance.arn == resource_arn {
                return Some(&instance.tags);
            }
            // A resource inside an instance carries an ARN built from its instance's,
            // so only the instance whose ARN prefixes this one can hold it — but the
            // scan does not depend on that, which keeps one rule for every kind.
            if let Some(queue) = instance
                .queues
                .values()
                .find(|queue| queue.arn == resource_arn)
            {
                return Some(&queue.tags);
            }
            if let Some(flow) = instance
                .contact_flows
                .values()
                .find(|flow| flow.arn == resource_arn)
            {
                return Some(&flow.tags);
            }
            if let Some(hours) = instance
                .hours_of_operations
                .values()
                .find(|hours| hours.arn == resource_arn)
            {
                return Some(&hours.tags);
            }
        }
        None
    }
}
