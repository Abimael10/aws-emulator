// SPDX-License-Identifier: CC0-1.0

//! The EBS volumes a TERMINATION takes off the instance that held them.
//!
//! The reverse of the root volume a launch mints (`super::launched_root_volumes`), and the
//! storage counterpart of the interfaces a termination deletes
//! (`super::launched_primary_interfaces`): "indicates whether the volume is deleted on
//! instance termination" (`EbsBlockDevice$DeleteOnTermination`) is answered at the instant
//! the termination completes, so a volume marked for it is GONE from the account's volume
//! store and a volume marked otherwise is no longer attached to anything.
//!
//! Both are applied to the one volume store, so the block device mappings an instance
//! reports
//! ([`ComAmazonawsEc2Amazonec2State::instance_block_device_mappings`](super::ComAmazonawsEc2Amazonec2State::instance_block_device_mappings))
//! and what `DescribeVolumes` reports for the same volume cannot disagree about whether a
//! terminated instance still holds it.

use super::{ComAmazonawsEc2Amazonec2State, InstanceStateName, VolumeRecord};

impl ComAmazonawsEc2Amazonec2State {
    /// Take the EBS volumes of every instance that has reached `terminated` off it: delete
    /// the ones its attachments are marked to delete with it, and detach the rest.
    ///
    /// Owed by the CLOCK, at the instant the termination completed
    /// ([`Self::materialize_due_transitions`], the one caller) — "indicates whether the
    /// volume is deleted on instance termination" (`EbsBlockDevice$DeleteOnTermination`) is
    /// a question the termination answers, whether or not anybody called again. A volume
    /// whose attachment says `false` outlives the instance, which is what that value means,
    /// but it is not attached to it any more: the instance it was attached to no longer
    /// exists, so the volume holds no attachment of it and reports `available`
    /// ([`VolumeRecord::state_at`]).
    ///
    /// This sweep sees every instance that has EVER reached `terminated`, so it is
    /// idempotent: a volume already removed is not there to remove again, and an attachment
    /// already dropped is not there to drop again.
    pub(super) fn release_volumes_of_terminated_instances(&mut self) {
        let terminated: ::std::collections::BTreeSet<String> = self
            .instances
            .values()
            .filter(|instance| instance.state == InstanceStateName::Terminated)
            .map(|instance| instance.instance_id.clone())
            .collect();
        if terminated.is_empty() {
            return;
        }
        // Whether this volume holds an attachment to one of those instances at all, asked
        // before anything is cloned so a volume attached elsewhere costs one comparison.
        let attached_to_terminated = |volume: &VolumeRecord, deleted_with_it: bool| -> bool {
            volume.view.attachments.iter().flatten().any(|attachment| {
                (!deleted_with_it || attachment.delete_on_termination == Some(true))
                    && attachment
                        .instance_id
                        .as_deref()
                        .is_some_and(|instance_id| terminated.contains(instance_id))
            })
        };
        let doomed: Vec<String> = self
            .volumes
            .iter()
            .filter(|(_, volume)| attached_to_terminated(volume, true))
            .map(|(volume_id, _)| volume_id.clone())
            .collect();
        for volume_id in doomed {
            // Through the one removal path, so the bytes the record held go back to the
            // store's budget and nothing goes on referring to the volume.
            self.remove_volume(&volume_id);
        }
        // Whatever is left attached to a terminated instance is detached: the attachment is
        // dropped outright rather than taken through `detaching`, because the instance half
        // of it is already gone.
        let detached: Vec<String> = self
            .volumes
            .iter()
            .filter(|(_, volume)| attached_to_terminated(volume, false))
            .map(|(volume_id, _)| volume_id.clone())
            .collect();
        for volume_id in detached {
            let Some(volume) = self.volumes.get_mut(&volume_id) else {
                continue;
            };
            if let Some(attachments) = volume.view.attachments.as_mut() {
                attachments.retain(|attachment| {
                    attachment
                        .instance_id
                        .as_deref()
                        .is_none_or(|instance_id| !terminated.contains(instance_id))
                });
            }
            // The instants the clock owed those attachments go with them — both maps are
            // keyed by (instance, device), so nothing is owed on an attachment the volume no
            // longer holds.
            volume
                .detaching_at
                .retain(|(instance_id, _), _| !terminated.contains(instance_id));
            volume
                .attaching_at
                .retain(|(instance_id, _), _| !terminated.contains(instance_id));
        }
    }
}
