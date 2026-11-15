// SPDX-License-Identifier: CC0-1.0

//! What a tracker's consumer associations are addressed by: the geofence collection a
//! `ConsumerArn` names, and the cursor a page of one tracker's consumers is read with.
//!
//! Kept beside [`super`] rather than in one operation's file because
//! `AssociateTrackerConsumer`, `DisassociateTrackerConsumer` and `ListTrackerConsumers`
//! all reach the SAME per-tracker set ([`super::Tracker::consumer_arns`]) and must read
//! a `ConsumerArn` the same way: an association a write makes is what the read reports,
//! and an ARN one of them resolves to a collection the other must resolve identically.

use super::{TaggedKind, ARN_VENDOR};

/// The member a refusal about the addressed geofence collection names, as the model
/// spells it on both association operations.
pub const CONSUMER_ARN_MEMBER: &str = "ConsumerArn";

/// The cursor tag for `ListTrackerConsumers`, whose page key is the INCLUSIVE ARN of
/// the next page's first consumer: a tracker holds each consumer ARN once, so the ARN
/// is a total order over the set.
pub const TRACKER_CONSUMERS_CURSOR: &str = "tracker-consumers";

/// The sentence a `ConsumerArn` that is not a geofence collection ARN of this service
/// is refused with.
///
/// The modelled `Arn` pattern admits any well-formed ARN, so what KIND of resource the
/// value names is this service's own check. It follows the form this service states a
/// member requirement in: the member name as the model spells it, quoted, then what the
/// member has to satisfy. It carries no value from the request, so every such refusal
/// states one sentence.
pub fn not_a_geofence_collection_arn() -> String {
    format!("'{CONSUMER_ARN_MEMBER}' must be the ARN of an Amazon Location geofence collection.")
}

/// The geofence collection name `arn` names, or the sentence the refusal states.
///
/// The ARN is read against this service's vendor and the geofence-collection resource
/// type with the workspace's one caller-supplied-ARN parse (`service_common::arn`), so
/// a value that is no ARN, one of another vendor and one naming another resource type
/// are all refused on the input. The partition, Region and account fields take no part
/// in the lookup: this service stores a collection under its name alone, which is also
/// what every name-addressed read of the crate resolves, so an ARN written in another
/// scope still names the collection it holds.
pub fn consumer_collection_name(arn: &str) -> Result<String, String> {
    let resource_type = TaggedKind::GeofenceCollection.arn_resource_type();
    let parsed =
        ::service_common::arn::parse_of(arn, ARN_VENDOR).ok_or_else(not_a_geofence_collection_arn)?;
    if parsed.resource_type() != resource_type {
        return Err(not_a_geofence_collection_arn());
    }
    ::service_common::arn::resource_id_of_type(arn, resource_type)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(not_a_geofence_collection_arn)
}

impl super::Tracker {
    /// Remove this tracker's association with the geofence collection named
    /// `collection_name`, reporting whether it held one.
    ///
    /// A tracker holds a collection's OWN ARN, so an association is removed by the
    /// collection it names rather than by the text one request addressed it with: a
    /// caller that writes the ARN in another scope still removes the association it
    /// made. An association the tracker does not hold is removed by doing nothing, so
    /// the removal is idempotent and the caller is handed `false` rather than a
    /// half-applied write.
    pub fn disassociate_consumer(&mut self, collection_name: &str) -> bool {
        let held = self.consumer_arns.len();
        self.consumer_arns
            .retain(|arn| !consumer_collection_name(arn).is_ok_and(|name| name == collection_name));
        held != self.consumer_arns.len()
    }
}
