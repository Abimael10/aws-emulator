// SPDX-License-Identifier: CC0-1.0

//! The Region scope of a named EC2 instance: the ONE reading of "is this instance one of the
//! Region this request was signed for".
//!
//! Split out of the [`crate::state`] module root so this one concern is a file of its own,
//! and re-exported from it, so a handler and the instance-id resolvers beside it reach it as
//! `crate::state::instance_record_in_region` like every other primitive there.

use super::{ComAmazonawsEc2Amazonec2State, InstanceRecord, instance_region};

/// The stored instance `instance_id` names IN `region`, or `None` when this store holds no
/// instance of that id there.
///
/// EC2 is a regional service reached through a regional endpoint, so "the instances" an
/// operation of it reads or acts on are the ones of the Region the request was signed for: an
/// instance that runs in another Region is invisible there, and an id naming one names no
/// instance at all — it is as absent as an id no instance ever carried, so a resolver answers
/// it with the sentence that operation words an ABSENT instance with. The Region an instance
/// runs in is read through the crate's one answer to that ([`instance_region`]), so an
/// operation that ACTS on an instance and one that REPORTS it cannot disagree about which
/// Region holds it.
///
/// The ONE place that scope is applied to a named instance: every resolver of an instance id
/// in [`super::instance_ids_and_instance_types`] goes through it, so no operation can resolve
/// an instance id without stating the Region it was asked in, and none re-spells the scope.
///
/// A LOOKUP and a comparison, and nothing else: the id is a key the map is asked for, never
/// indexed, sliced or unwrapped, so no request value can panic this reading.
pub fn instance_record_in_region<'store>(
    store: &'store ComAmazonawsEc2Amazonec2State,
    region: &str,
    instance_id: &str,
) -> Option<&'store InstanceRecord> {
    store
        .instances
        .get(instance_id)
        .filter(|instance| instance_region(instance) == region)
}
