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
//! caller sees. The one attribute a caller can CHANGE — who may create a volume from the
//! snapshot — is recorded by [`super::snapshot_create_volume_permissions`], whose writers are
//! `ModifySnapshotAttribute` and `ResetSnapshotAttribute`.

use std::time::{Instant, SystemTime};

use super::*;

/// The `SnapshotAttributeName` member that names a snapshot's create volume permissions
/// (`smithy_query(query="shape",shape="SnapshotAttributeName")`, whose generated catalog is
/// [`crate::ops::types::SNAPSHOT_ATTRIBUTE_NAME_VALUES`]): "the users and groups that have
/// the permissions for creating volumes from the snapshot".
pub const SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION: &str = "createVolumePermission";

/// The other `SnapshotAttributeName` member: "the product codes" a snapshot carries.
pub const SNAPSHOT_ATTRIBUTE_PRODUCT_CODES: &str = "productCodes";

/// Is `snapshot` one of the PUBLISHED snapshots every account may create a volume from?
///
/// "`all` for public snapshots" and "public: … All Amazon Web Services accounts have create
/// volume permissions for these snapshots" (`DescribeSnapshots`, `RestorableByUserIds`). The
/// published public snapshots of this inventory are the ones the calling account does NOT own:
/// a snapshot of another account is visible to it only because every account may create a
/// volume from a public one, and the snapshots this emulator holds of another account are
/// exactly the ones the AMI catalogue is published with ([`super::published_snapshots`]).
///
/// It is therefore also the test for whether a snapshot came from that catalogue, which is why
/// [`ComAmazonawsEc2Amazonec2State::snapshot_product_codes`] asks it to decide which resource a
/// snapshot's Marketplace codes travel with.
///
/// A snapshot OF THE ACCOUNT becomes public the other way — by being granted create volume
/// permissions for the `all` group through `ModifySnapshotAttribute` — so the question "may
/// every account create a volume from this snapshot?" is
/// [`ComAmazonawsEc2Amazonec2State::snapshot_is_shared_with_all`], which asks this one and the
/// grants beside it. That is the ONE test the inventory's `RestorableByUserIds` narrowing and
/// the permissions an attribute read reports both ask, so they cannot disagree about which
/// snapshots are public.
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
/// Whichever source answers, the snapshot is reported in the tier its own tiering action has
/// reached ([`ComAmazonawsEc2Amazonec2State::snapshot_in_tier`]), so a snapshot resolved here
/// and the same snapshot in an inventory report cannot name two different storage tiers.
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
    let snapshot = if let Some(record) = store.snapshots.get(snapshot_id) {
        Some(record.view_at(now))
    } else if let Some(snapshot) = direct_api_snapshot(engine, snapshot_id, observed) {
        Some(snapshot)
    } else {
        store.image_backing_snapshots().remove(snapshot_id)
    };
    snapshot.map(|snapshot| store.snapshot_in_tier(snapshot, now))
}

/// The name EC2's parameter layer gives the `SnapshotId` member of a snapshot-attribute call
/// when it refuses the value one carried: the resource the member names, in the case that layer
/// spells it.
///
/// Neither the case nor the plurality is derivable from the member's own name — the same layer
/// calls `DescribeSnapshots`' id LIST `snapshotId` ([`super::SNAPSHOT_IDS_PARAMETER`]) — so it
/// is taken from the recorded refusal of a `DescribeSnapshotAttribute` call, and the operations
/// that MODIFY and RESET the same attributes answer the same layer's sentence.
pub const SNAPSHOT_PARAMETER: &str = "SNAPSHOT";

/// Whether `id` is a snapshot id EC2 could have ISSUED, as the operations that WRITE a
/// snapshot's attributes read one.
///
/// The stricter of the crate's two readings of a snapshot id: [`super::issued_id_is_well_formed`]
/// — one of the two published id forms, not the all-zero placeholder, and, where it carries the
/// LONG form, under a leading `0`, because that form carries 68 bits for a 64-bit identifier so
/// every id this service's mint has issued reads `snap-0…`. A value inside the generic id form
/// but outside the one EC2 issues — the `snap-1234567890abcdef0` shape documentation examples
/// and hand-written probes reach for — is therefore no snapshot id at all rather than a snapshot
/// that is not there, which is what the recorded `ResetSnapshotAttribute` call behind
/// `issues/.../20261105T021000Z-ec2-modifysnapshotattribute-stub.md` was answered
/// `InvalidSnapshotID.Malformed` for.
///
/// Which reading an operation takes is a fact of that operation, recorded by its caller: the
/// describes keep the wider [`super::snapshot_id_is_well_formed`] on their own recorded
/// evidence, where a short-form id the inventory holds nothing for is reported absent.
pub fn issued_snapshot_id_is_well_formed(id: &str) -> bool {
    issued_id_is_well_formed(SNAPSHOT_ID_PREFIX, id)
}

/// Refuse a value that is no snapshot id of the form EC2 ISSUES: HTTP 400
/// [`super::INVALID_SNAPSHOT_ID_MALFORMED`], in the sentence the PARAMETER layer of a
/// snapshot-attribute call words every value it rejects with.
///
/// The CODE is the snapshot's own malformed-id code, and the WORDING is that layer's: the fixed
/// text `Value ( `, ` ) for parameter `, the name it gives the member that carried the value
/// ([`SNAPSHOT_PARAMETER`]), and ` is invalid. ` around the value the request sent, which is the
/// only part that varies. The layer appends a reason after the period, and a value it could not
/// read as an id has none to add, so the message ends with the separating space and nothing
/// after it — the same form [`crate::ops::create_snapshot`] carries for the volume id of a
/// create, and the same spacing and member case as the recorded `DescribeSnapshotAttribute` call
/// behind `issues/.../20261023T102725Z-ec2-describesnapshotattribute-stub.md`.
///
/// Shared by the three operations that address one snapshot's attributes, so they cannot word
/// one refusal differently. It is NOT the crate's only wording for the code: EC2 answers an id
/// it cannot read in the wording of the LAYER that read it, so `DescribeSnapshots` refuses an
/// entry of its id LIST under `InvalidParameterValue` and a fast snapshot restore reports
/// [`super::unresolvable_snapshot_id`]'s own sentence.
///
/// The echoed value is cut to length by [`super::echoed_resource_id`], the ceiling this crate
/// holds every refusal that quotes a caller-chosen value to.
pub fn snapshot_id_is_not_an_issued_id<E>(snapshot_id: &str) -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::service_error(
        INVALID_SNAPSHOT_ID_MALFORMED,
        400,
        format!(
            "Value ( {} ) for parameter {SNAPSHOT_PARAMETER} is invalid. ",
            echoed_resource_id(snapshot_id)
        ),
    )
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
