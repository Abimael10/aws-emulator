// SPDX-License-Identifier: CC0-1.0

//! The ATTRIBUTES of one EBS snapshot of the account-visible inventory: which snapshot an id
//! names, whether every account may create a volume from it, and the Marketplace product codes
//! it carries.
//!
//! "Describes the specified attribute of the specified snapshot" (`DescribeSnapshotAttribute`)
//! — so an attribute read first has to RESOLVE the snapshot, and the snapshot it resolves is
//! one of the same inventory `DescribeSnapshots` reports: "the snapshots available to you
//! include public snapshots, private snapshots that you own, and private snapshots owned by
//! other Amazon Web Services accounts for which you have explicit create volume permissions".
//! That inventory has three sources in this emulator — the account's own store
//! ([`ComAmazonawsEc2Amazonec2State::snapshots`]), the snapshots the EBS direct APIs hold
//! ([`super::direct_api_snapshots`]) and the public ones the AMI catalogue is published with
//! ([`super::published_snapshots`]) — so [`visible_snapshot`] reads all three, and an id a
//! report resolves is an id an attribute read resolves.
//!
//! NOTHING here writes: every value is read off the stores or derived from them, so two
//! attribute reads of one snapshot answer the same thing and neither changes what a later
//! caller sees.

use std::time::{Instant, SystemTime};

use super::*;

/// The `SnapshotAttributeName` member that names a snapshot's create volume permissions
/// (`smithy_query(query="shape",shape="SnapshotAttributeName")`, whose generated catalog is
/// [`crate::ops::types::SNAPSHOT_ATTRIBUTE_NAME_VALUES`]): "the users and groups that have
/// the permissions for creating volumes from the snapshot".
pub const SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION: &str = "createVolumePermission";

/// The other `SnapshotAttributeName` member: "the product codes" a snapshot carries.
pub const SNAPSHOT_ATTRIBUTE_PRODUCT_CODES: &str = "productCodes";

/// Is `snapshot` one EVERY account may create a volume from — a PUBLIC snapshot?
///
/// "`all` for public snapshots" and "public: … All Amazon Web Services accounts have create
/// volume permissions for these snapshots" (`DescribeSnapshots`, `RestorableByUserIds`). The
/// public snapshots of this inventory are the ones the calling account does NOT own: a
/// snapshot of another account is visible to it only because every account may create a
/// volume from a public one, and the snapshots this emulator holds of another account are
/// exactly the ones the AMI catalogue is published with
/// ([`super::published_snapshots`]). Public and explicit permissions ON a snapshot of the
/// account are granted through `ModifySnapshotAttribute`, which is still a scaffold, so no
/// snapshot of the account carries one.
///
/// The ONE definition of that test, so the inventory's `RestorableByUserIds` narrowing and the
/// permissions an attribute read reports cannot disagree about which snapshots are public.
pub fn snapshot_is_public(snapshot: &SnapshotView, account: &str) -> bool {
    snapshot.owner_id.as_deref() != Some(account)
}

/// The one snapshot `snapshot_id` names in the account-visible inventory as of `now` (the
/// monotonic clock the account's own records derive their state from) and `observed` (the
/// wall-clock instant the direct-API side compares its own timeouts against), or `None` when
/// no side of that inventory holds one.
///
/// The three sources are read in the order a snapshot is cheapest to resolve in: the
/// account's own store, the EBS direct APIs' store, then the catalogue the AMI records derive.
/// A snapshot of the account's own store SHADOWS a catalogue projection of the same id, which
/// is the precedence [`ComAmazonawsEc2Amazonec2State::image_backing_snapshots`] already
/// applies, so one snapshot has one record whichever reader resolves it.
///
/// LOCK ORDER: called with this service's store already locked, and the direct-API read takes
/// the EBS store second — the order every operation of this service keeps (see
/// [`super::direct_api_snapshots`]).
pub fn visible_snapshot<Eng: Send + Sync + 'static>(
    store: &ComAmazonawsEc2Amazonec2State,
    engine: &Eng,
    snapshot_id: &str,
    now: Instant,
    observed: SystemTime,
) -> Option<SnapshotView> {
    if let Some(record) = store.snapshots.get(snapshot_id) {
        return Some(record.view_at(now));
    }
    if let Some(snapshot) = direct_api_snapshot(engine, snapshot_id, observed) {
        return Some(snapshot);
    }
    store.image_backing_snapshots().remove(snapshot_id)
}

impl ComAmazonawsEc2Amazonec2State {
    /// "The product codes" of `snapshot` (`DescribeSnapshotAttributeResult$ProductCodes`): the
    /// Amazon Web Services Marketplace codes the snapshot is billed through.
    ///
    /// A Marketplace product code travels WITH the data: "any Amazon Web Services Marketplace
    /// product codes from the snapshot are propagated to the volume" (`CreateVolume`) and a
    /// snapshot of that volume carries them on again. So a snapshot's codes are read off
    /// whichever resource it was made from:
    ///
    /// * a PUBLIC snapshot ([`snapshot_is_public`]) is one an AMI of the catalogue was
    ///   published with, so its codes are that image's own ([`Self::image_using_snapshot`]
    ///   resolves the image, `Image$ProductCodes` carries them) — an image nobody sells
    ///   carries none, and so does the snapshot it restores from;
    /// * a snapshot of the ACCOUNT was taken of one of its volumes, so its codes are that
    ///   volume's, through the one derivation of a volume's codes this crate has
    ///   ([`VolumeRecord::product_codes`]) — which is also what `DescribeVolumeAttribute`
    ///   reports, so a volume and a snapshot of it cannot be billed two ways.
    ///
    /// Derived rather than stored, for the reason the snapshot store gives for the members it
    /// derives: two writers cannot then record one snapshot's codes differently.
    pub fn snapshot_product_codes(
        &self,
        snapshot: &SnapshotView,
        account: &str,
    ) -> Vec<ProductCode> {
        if snapshot_is_public(snapshot, account) {
            return self.published_snapshot_product_codes(snapshot);
        }
        snapshot
            .volume_id
            .as_deref()
            .and_then(|volume_id| self.volumes.get(volume_id))
            .map(VolumeRecord::product_codes)
            .unwrap_or_default()
    }

    /// The product codes of a PUBLIC snapshot: the ones of the AMI it was published with, or
    /// none when no image of the catalogue restores a device from it.
    fn published_snapshot_product_codes(&self, snapshot: &SnapshotView) -> Vec<ProductCode> {
        snapshot
            .snapshot_id
            .as_deref()
            .and_then(|snapshot_id| self.image_using_snapshot(snapshot_id))
            .and_then(|image_id| self.images.get(&image_id))
            .and_then(|image| image.product_codes.clone())
            .unwrap_or_default()
    }
}
