// SPDX-License-Identifier: CC0-1.0

//! Tags, across the resource types a `TaggableResourceArn` addresses.
//!
//! `TaggableResourceArn` documents "ARN for an EventBridge resource that
//! supports tagging: event buses, subscribers, and event sources", and each of
//! those three resources carries its own tag set in its own registry (minted by
//! the create that stored it). A tag-addressed operation therefore resolves ONE
//! ARN against all three registries, which is what this module does — once, so
//! no two of them disagree about which resource an ARN names or about how a
//! miss is worded. A tag WRITE resolves it the same way and through the same
//! lookup, so what it changes is the set of the one resource a read of that ARN
//! reports and nothing else.

use std::collections::BTreeMap;

use super::{
    event_source_not_found, resource_not_found, subscriber_not_found,
    ComAmazonawsEventbridgev2Awseventsv2State, ARN_RESOURCE_TYPE,
    ARN_RESOURCE_TYPE_EVENT_SOURCE, ARN_RESOURCE_TYPE_SUBSCRIBER,
};
use crate::ops::types::ResourceNotFoundException;
use crate::ops::update_event_source::REVOKED_EVENT_SOURCE_MESSAGE;
use crate::ops::update_subscriber::REVOKED_SUBSCRIBER_MESSAGE;

/// The most tags one taggable resource holds at once.
///
/// `TagMap` is modelled `@length(max: 200)` and a tag read reports a resource's
/// WHOLE set as one such map, so a resource holding more than that is a resource
/// no read of it could report. A tag write that would leave it holding more is
/// therefore the quota case `LimitExceededException` — "The request would exceed
/// a service quota for the account" — is the modelled answer for.
pub const MAX_TAGS_PER_RESOURCE: usize = 200;

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

    /// The tags stored on the one taggable resource this ARN addresses, as the
    /// handle a tag WRITE changes them through, or `None` when the store holds
    /// no such resource.
    ///
    /// The one resolution [`Self::taggable_tags`] reads through, so a write and
    /// a read of one ARN can never reach different resources — and it hands back
    /// the set stored on THAT resource alone, which is what keeps a change to
    /// one resource's tags off every other resource, its bus and its
    /// subscribers included.
    pub fn taggable_tags_mut(&mut self, arn: &str) -> Option<&mut BTreeMap<String, String>> {
        if let Some(bus) = self.event_buses.get_mut(arn) {
            return Some(&mut bus.tags);
        }
        if let Some(source) = self.event_sources.get_mut(arn) {
            return Some(&mut source.tags);
        }
        self.subscribers
            .get_mut(arn)
            .map(|subscriber| &mut subscriber.tags)
    }

    /// Why the taggable resource this ARN addresses is in no state to take a
    /// change to it, as the `Message` of an `InvalidStateException`; `None` when
    /// it can take one (and for an ARN naming no stored resource, which is a
    /// not-found rather than a state).
    ///
    /// `Revoked` documents the one state that refuses a change: "Revocation is
    /// terminal: it never returns to false. Mutating operations on a revoked
    /// subscriber fail with InvalidStateException, except DeleteSubscriber", and
    /// its EventSource sibling says the same — so a withdrawn resource takes no
    /// further change, a tag write included. Nothing documents a bus's
    /// PROVISIONING state as refusing one, and a create stores the tags it
    /// carries while the bus it mints is still `CREATING`, so a stored bus takes
    /// a tag change whatever lifecycle state it is in.
    pub fn taggable_change_fault(&self, arn: &str) -> Option<String> {
        if self.event_buses.contains_key(arn) {
            return None;
        }
        if let Some(source) = self.event_sources.get(arn) {
            return source
                .revoked
                .then(|| REVOKED_EVENT_SOURCE_MESSAGE.to_string());
        }
        self.subscribers
            .get(arn)?
            .revoked
            .then(|| REVOKED_SUBSCRIBER_MESSAGE.to_string())
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
