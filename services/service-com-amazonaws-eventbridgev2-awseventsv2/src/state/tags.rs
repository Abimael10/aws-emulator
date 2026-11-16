// SPDX-License-Identifier: CC0-1.0

//! Tags, across the resource types a `TaggableResourceArn` addresses.
//!
//! `TaggableResourceArn` documents "ARN for an EventBridge resource that
//! supports tagging: event buses, subscribers, and event sources", and each of
//! those three resources carries its own tag set in its own registry (minted by
//! the create that stored it). A tag-addressed operation therefore resolves ONE
//! ARN against all three registries, which is what this module does — once, so
//! no two of them disagree about which resource an ARN names or about how a
//! miss is worded.

use std::collections::BTreeMap;

use super::{
    event_source_not_found, resource_not_found, subscriber_not_found,
    ComAmazonawsEventbridgev2Awseventsv2State, ARN_RESOURCE_TYPE,
    ARN_RESOURCE_TYPE_EVENT_SOURCE, ARN_RESOURCE_TYPE_SUBSCRIBER,
};
use crate::ops::types::ResourceNotFoundException;

impl ComAmazonawsEventbridgev2Awseventsv2State {
    /// The tags stored on the one taggable resource this ARN addresses, or
    /// `None` when the store holds no such resource.
    ///
    /// Each registry is keyed by its resources' own ARNs and the resource-type
    /// segment differs per kind, so at most one of the three holds the resource
    /// a call addresses — the lookup is a miss in the other two.
    pub fn taggable_tags(&self, arn: &str) -> Option<&BTreeMap<String, String>> {
        if let Some(bus) = self.event_buses.get(arn) {
            return Some(&bus.tags);
        }
        if let Some(source) = self.event_sources.get(arn) {
            return Some(&source.tags);
        }
        self.subscribers
            .get(arn)
            .map(|subscriber| &subscriber.tags)
    }

    /// The `ResourceNotFoundException` a tag-addressed operation answers when
    /// the store holds nothing at the requested ARN.
    ///
    /// A `TaggableResourceArn` names which of the three resource types it
    /// addresses in its resource-type segment, so the refusal is worded by the
    /// one construction that resource's own registry owns: an operation reaching
    /// across the registries cannot word the miss differently from a read of
    /// that resource. A value naming no resource type of this service at all
    /// takes the fixed text of the class.
    pub fn taggable_resource_refusal(&self, arn: &str) -> ResourceNotFoundException {
        let resource_type = ::service_common::arn::resource_type(arn);
        if resource_type == Some(ARN_RESOURCE_TYPE) {
            self.bus_read_refusal(arn)
        } else if resource_type == Some(ARN_RESOURCE_TYPE_SUBSCRIBER) {
            subscriber_not_found(arn)
        } else if resource_type == Some(ARN_RESOURCE_TYPE_EVENT_SOURCE) {
            event_source_not_found(arn)
        } else {
            resource_not_found()
        }
    }
}
