// SPDX-License-Identifier: CC0-1.0

//! How an AMI is DEREGISTERED: where the image itself goes, and what that does to the
//! snapshots its block device mappings restore from.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.
//!
//! "Deregisters the specified AMI. A deregistered AMI can't be used to launch new instances.
//! If a deregistered EBS-backed AMI matches a Recycle Bin retention rule, it moves to the
//! Recycle Bin for the specified retention period … If the deregistered AMI doesn't match a
//! retention rule, it is permanently deleted immediately" (`DeregisterImage`): a
//! deregistration therefore has two endings, and the snapshots behind the image are a question
//! of their own — "when deregistering an EBS-backed AMI, you can optionally delete its
//! associated snapshots at the same time. However, if a snapshot is associated with multiple
//! AMIs, it won't be deleted even if specified for deletion, although the AMI will still be
//! deregistered".
//!
//! [`ComAmazonawsEc2Amazonec2State::deregister_image`] is the ONE writer of all of that, so an
//! image that has been deregistered is gone from the images the account can see whichever
//! ending it took, and no snapshot is deleted that another AMI still restores a device from.
//!
//! A snapshot this deletes goes the same way one `DeleteSnapshot` deletes does: a snapshot a
//! Recycle Bin retention rule covers MOVES into the bin for that rule's retention period
//! rather than being permanently deleted, through the same writer
//! ([`ComAmazonawsEc2Amazonec2State::move_snapshot_to_recycle_bin`]) that operation uses — the
//! retention question is the rules' answer rather than this store's, so the caller asks it (it
//! is the `rbin` service's, read through [`recycle_bin_retention_period`]) and hands the answer
//! in per snapshot.

use super::*;

/// The `success` member of [`crate::ops::types::SNAPSHOT_RETURN_CODES_VALUES`]:
/// "`success` - The snapshot was successfully deleted"
/// (`DeleteSnapshotReturnCode$ReturnCode`).
pub const SNAPSHOT_RETURN_CODE_SUCCESS: &str = "success";

/// The `skipped` member of [`crate::ops::types::SNAPSHOT_RETURN_CODES_VALUES`]:
/// "`skipped` - The snapshot was not deleted because it's associated with other AMIs"
/// (`DeleteSnapshotReturnCode$ReturnCode`).
pub const SNAPSHOT_RETURN_CODE_SKIPPED: &str = "skipped";

/// The `internal-error` member of [`crate::ops::types::SNAPSHOT_RETURN_CODES_VALUES`]:
/// "`internal-error` - The snapshot was not deleted due to a server error"
/// (`DeleteSnapshotReturnCode$ReturnCode`) — which is what a snapshot a retention rule covers
/// that this emulator has no room to retain is reported as, the server-side ceiling being this
/// emulator's own.
pub const SNAPSHOT_RETURN_CODE_INTERNAL_ERROR: &str = "internal-error";

/// One snapshot a deregistration was asked to delete, with the retention period a Recycle Bin
/// rule of the account covers it for.
///
/// The pair [`ComAmazonawsEc2Amazonec2State::deregister_image`] takes per snapshot: the id is
/// one of [`ComAmazonawsEc2Amazonec2State::image_associated_snapshot_ids`], and the period is
/// what [`recycle_bin_retention_period`] answered about that snapshot's own tags — `None` being
/// "no rule retains it", which the delete reads as "delete this permanently", exactly as
/// `DeleteSnapshot` does.
#[derive(Debug, Clone)]
pub struct AssociatedSnapshot {
    /// The `SnapshotId` of a snapshot one of the AMI's Amazon EBS devices restores from.
    pub snapshot_id: String,
    /// For how long a retention rule of the account retains it, or `None` when none does.
    pub retention_period: Option<Duration>,
}

/// What one deregistration did, as `DeregisterImage` reports it.
#[derive(Debug, Clone, Default)]
pub struct ImageDeregistration {
    /// "The deletion result for each snapshot associated with the AMI, including the snapshot
    /// ID and its success or error code"
    /// (`DeregisterImageResult$DeleteSnapshotResults`) — empty for a deregistration that was
    /// not asked to delete them.
    pub delete_snapshot_results: Vec<crate::ops::types::DeleteSnapshotReturnCode>,
}

impl ComAmazonawsEc2Amazonec2State {
    /// Deregister the AMI `image_id` names, and answer what that did — `None` when this store
    /// holds no image under that id, in which case NOTHING was written.
    ///
    /// `retention_period` is how long a Recycle Bin retention rule of the account retains a
    /// deregistered AMI, as the rules stood when this call read them
    /// ([`recycle_bin_retention_period`]), and `None` is "no rule retains it": the image
    /// moves into the bin ([`Self::recycle_image`]) for that period, or is permanently
    /// deleted at once. Either way it leaves [`Self::images`] — "a deregistered AMI can't be
    /// used to launch new instances" — so nothing can see or launch from it any more.
    ///
    /// `associated_snapshots` are the snapshots the request asked to be deleted with the AMI,
    /// each with the retention period a rule covers it for ([`AssociatedSnapshot`]) — EMPTY for
    /// a request that asked for none: "specifies whether to delete the snapshots associated
    /// with the AMI during deregistration. If a snapshot is associated with multiple AMIs, it
    /// is not deleted, regardless of this setting"
    /// (`DeregisterImageRequest$DeleteAssociatedSnapshots`). Each is deleted the way
    /// `DeleteSnapshot` deletes one — moved into the Recycle Bin for the period a rule retains
    /// it, else removed through the one remover of a snapshot ([`Self::remove_snapshot`]) —
    /// unless a REGISTERED AMI still restores a device from it ([`Self::image_using_snapshot`],
    /// which an image in the Recycle Bin is not one of), and each is reported with the code
    /// that says which of those happened. `now` is the instant this call runs at, the one the
    /// bin reads the entry's two instants off.
    ///
    /// Instances already launched from the AMI are untouched: "deregistering an AMI does not
    /// delete … instances already launched from the AMI".
    pub fn deregister_image(
        &mut self,
        image_id: &str,
        retention_period: Option<Duration>,
        associated_snapshots: &[AssociatedSnapshot],
        now: SystemTime,
    ) -> Option<ImageDeregistration> {
        if !self.images.contains_key(image_id) {
            return None;
        }
        let moved = match retention_period {
            Some(retention_period) => self.recycle_image(image_id, retention_period),
            None => self.permanently_delete_image(image_id),
        };
        if !moved {
            return None;
        }

        let mut delete_snapshot_results = Vec::new();
        for associated in associated_snapshots {
            let snapshot_id = associated.snapshot_id.as_str();
            let return_code = if self.image_using_snapshot(snapshot_id).is_some() {
                SNAPSHOT_RETURN_CODE_SKIPPED
            } else {
                match associated.retention_period {
                    // A rule of the account retains it, so the delete MOVES it into the bin
                    // for that period rather than deleting it permanently, and reports the
                    // deletion the caller asked for: the snapshot stops being one of the
                    // account's either way. A bin with no room for it leaves the snapshot
                    // where it is rather than permanently deleting one a rule says to keep.
                    Some(retention) => {
                        if self
                            .move_snapshot_to_recycle_bin(snapshot_id, now, retention)
                            .is_some()
                        {
                            SNAPSHOT_RETURN_CODE_SUCCESS
                        } else {
                            SNAPSHOT_RETURN_CODE_INTERNAL_ERROR
                        }
                    }
                    None => {
                        if self.remove_snapshot(snapshot_id).is_some() {
                            SNAPSHOT_RETURN_CODE_SUCCESS
                        } else {
                            // The caller read this id off the image's own mappings a moment
                            // ago, so the only way the removal reports nothing is a snapshot
                            // something else took out from under it — which is still a
                            // snapshot this call did not delete.
                            SNAPSHOT_RETURN_CODE_SKIPPED
                        }
                    }
                }
            };
            delete_snapshot_results.push(crate::ops::types::DeleteSnapshotReturnCode {
                snapshot_id: Some(associated.snapshot_id.clone()),
                return_code: Some(return_code.to_string()),
            });
        }
        Some(ImageDeregistration {
            delete_snapshot_results,
        })
    }

    /// The snapshots the AMI `image_id` restores its Amazon EBS devices from, in mapping order
    /// and each named once — the snapshots a deregistration can be asked to delete with it.
    ///
    /// Only the ones this account HOLDS: a mapping naming a snapshot the account has none of
    /// names nothing for a deletion to report on. Read before the deregistration writes
    /// anything, which is what lets its caller ask the retention rules about each one's tags
    /// first — and what makes an image whose own mapping is the only thing naming a snapshot
    /// still able to have that snapshot deleted.
    pub fn image_associated_snapshot_ids(&self, image_id: &str) -> Vec<String> {
        let mut snapshot_ids: Vec<String> = Vec::new();
        let Some(image) = self.images.get(image_id) else {
            return snapshot_ids;
        };
        for snapshot_id in image
            .block_device_mappings
            .iter()
            .flatten()
            .filter_map(|mapping| mapping.ebs.as_ref())
            .filter_map(|ebs| ebs.snapshot_id.as_deref())
        {
            if self.snapshots.contains_key(snapshot_id)
                && !snapshot_ids.iter().any(|held| held == snapshot_id)
            {
                snapshot_ids.push(snapshot_id.to_string());
            }
        }
        snapshot_ids
    }

    /// Permanently delete the AMI `image_id` names, and answer whether there was one to
    /// delete.
    ///
    /// "If the deregistered AMI doesn't match a retention rule, it is permanently deleted
    /// immediately" (`DeregisterImage`): the image leaves [`Self::images`] and the UEFI
    /// variable store held beside it under the same id goes with it, nothing being able to
    /// restore the image any more.
    ///
    /// The TAG bytes the image held are given back, and only those — for the reason
    /// [`Self::purge_expired_recycled_images`] gives for the entry it purges: this store
    /// keeps the modelled [`Image`] and not the `ImageSpec` the charge was computed from, so
    /// releasing a figure recomputed from the stored image would be a different number, and
    /// keeping the rest counted can only refuse a later write earlier rather than admit one
    /// the budget forbids.
    fn permanently_delete_image(&mut self, image_id: &str) -> bool {
        let Some(image) = self.images.remove(image_id) else {
            return false;
        };
        self.release_stored_bytes(tag_list_bytes(image.tags.as_deref().unwrap_or_default()));
        self.image_uefi_data.remove(image_id);
        true
    }
}
