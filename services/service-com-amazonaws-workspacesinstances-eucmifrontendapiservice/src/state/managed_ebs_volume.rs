// SPDX-License-Identifier: CC0-1.0

//! The Amazon EBS volume a WorkSpaces Instances volume IS.
//!
//! A volume of this service is not a record of this service alone: "Creates a new volume for
//! WorkSpace Instances" (`smithy_query(query="operation",operation="CreateVolume")`) carries
//! Amazon EBS's own configuration surface — the `AvailabilityZone` the volume is created in,
//! `SizeInGB`, the `VolumeTypeEnum` AWS publishes for EBS, `Iops`, `Throughput`, the
//! `SnapshotId` it is restored from, `Encrypted`/`KmsKeyId`, and `TagSpecifications` whose
//! `ResourceType` is `volume` — the id it answers is an EBS `VolumeId`
//! (`^vol-[0-9a-zA-Z]{1,63}$`), and `AssociateVolume`/`DisassociateVolume` attach it to an
//! instance at a `Device`, which is an EBS attachment. So the volume a successful
//! `CreateVolume` reports is a REAL EBS volume of the caller's own account: the service that
//! owns EBS volumes holds it, its reads resolve it, and `DeleteVolume` makes it disappear
//! from there.
//!
//! This module is that seam. Nothing here invents a volume: the record is written by the
//! owning service's own writer
//! ([`ec2_store::ComAmazonawsEc2Amazonec2State::insert_volume`]), which mints the `VolumeId`
//! out of ITS id counter, builds the EBS `Volume` shape its own `CreateVolume` builds and
//! charges the bytes against its own store budget — so the id this service reports is the id
//! that store holds, and every EBS read of it (a describe, a tag, an attach) resolves the
//! same record.
//!
//! The store is reached through the SHARED ENGINE, the same seam
//! `service-com-amazonaws-ec2-amazonec2` reaches the EBS direct APIs' snapshot store
//! through: this crate names the owning crate and no socket is opened, nothing leaves the
//! process, and an engine that service is not mounted on still carries its store, so there
//! is no "the other service was unreachable" outcome to report.
//!
//! LOCK ORDER: an operation of this service takes its OWN store first and the EC2 store
//! second (every function here is called with this service's store already locked, or with
//! none locked at all), and no EC2 handler reads this service's store — so the two orders
//! cannot invert.

use std::collections::BTreeMap;

use ::service_com_amazonaws_ec2_amazonec2::state::{self as ec2_store, VolumeSpec};

use crate::ops::types::{CreateVolumeInput, ValidationException};

/// Amazon EBS's error code for a `SnapshotId` its `CreateVolume` cannot read as a snapshot
/// of the account.
///
/// RECORDED: the refusal AWS answered a WorkSpaces Instances `CreateVolume` that named a
/// snapshot no snapshot of the account has, reported through
/// [`unresolvable_snapshot_message`].
const INVALID_SNAPSHOT_ID_CODE: &str = "InvalidSnapshotID.Malformed";

/// What the backing Amazon EBS service answers for a `SnapshotId` it cannot resolve, as
/// EBS words a bad value for that parameter: the fixed text is `Value ( `, ` ) for parameter
/// SNAPSHOT is invalid. `, and the only variable part is the snapshot id the REQUEST named.
fn invalid_snapshot_parameter(snapshot_id: &str) -> String {
    format!("Value ( {snapshot_id} ) for parameter SNAPSHOT is invalid. ")
}

/// How this service reports a create the backing Amazon EBS service refused: the fixed
/// `Create volume failed (`, the code EBS classified the refusal as, `: `, the sentence EBS
/// worded for it, and `)`.
///
/// RECORDED: the message AWS answered for a create EBS refused. The wrapper is this
/// service's own and the code and sentence inside it are the dependency's, so a client reads
/// which call failed and why.
fn create_volume_failed(code: &str, message: &str) -> String {
    format!("Create volume failed ({code}: {message})")
}

/// The modelled `ValidationException` (HTTP 400) for a create whose `SnapshotId` names no
/// snapshot the account holds.
///
/// `Reason` is [`crate::state::VALIDATION_REASON_DEPENDENCY_FAILURE`] and there is no
/// `FieldList`: the refusal is the BACKING service's answer to this create rather than a
/// modelled constraint of the request, so the message carries that service's own code and
/// sentence — which quote the snapshot id the request named, because that is how EBS words
/// a bad value for the parameter.
pub fn unresolvable_snapshot(snapshot_id: &str) -> ValidationException {
    ValidationException {
        message: unresolvable_snapshot_message(snapshot_id),
        reason: crate::state::VALIDATION_REASON_DEPENDENCY_FAILURE.to_string(),
        field_list: None,
    }
}

/// The sentence [`unresolvable_snapshot`] carries, named so a test can assert the wording
/// without restating it.
pub fn unresolvable_snapshot_message(snapshot_id: &str) -> String {
    create_volume_failed(
        INVALID_SNAPSHOT_ID_CODE,
        &invalid_snapshot_parameter(snapshot_id),
    )
}

/// Whether the account holds an EBS snapshot under `snapshot_id`.
///
/// "Source snapshot for volume creation" (`CreateVolumeRequest$SnapshotId`): a volume
/// restored from a snapshot needs that snapshot, so the id is resolved against the service
/// that OWNS the account's snapshots rather than against anything this crate keeps — a
/// snapshot created or deleted there is one this answer follows.
pub fn account_holds_snapshot<Eng: Send + Sync + 'static>(engine: &Eng, snapshot_id: &str) -> bool {
    ec2_store::service_state(engine)
        .is_some_and(|state| state.lock().holds_snapshot(snapshot_id))
}

/// Create the EBS volume `input` asks for, carrying `tags`, and answer the `VolumeId` the
/// service that owns EBS volumes minted for it.
///
/// EVERY member of the configuration is carried into that create — the zone, the size, the
/// type, the IOPS, the throughput, the encryption state and key, and the snapshot the volume
/// is restored from — so the volume the account holds is the one the caller asked for, and
/// `tags` is the tag set this service stores for the volume (read out of the
/// `volume`-typed `TagSpecifications`), so the volume carries them from the moment it is
/// created.
///
/// `None` when the owning store cannot hold another volume: no volume was created, so no id
/// is invented for one.
pub fn create_ebs_volume<Eng: Send + Sync + 'static>(
    engine: &Eng,
    input: &CreateVolumeInput,
    tags: &BTreeMap<String, String>,
) -> Option<String> {
    let state = ec2_store::service_state(engine)?;
    let mut store = state.lock();
    store.insert_volume(VolumeSpec {
        availability_zone: input.availability_zone.clone(),
        size: input.size_in_gb,
        volume_type: input.volume_type.clone(),
        iops: input.iops,
        throughput: input.throughput,
        encrypted: input.encrypted,
        kms_key_id: input.kms_key_id.clone(),
        snapshot_id: input.snapshot_id.clone(),
        tags: ec2_volume_tags(tags),
        // Multi-Attach is asked for at creation and this service's model has no member for
        // it, so the volume it provisions does not carry it.
        multi_attach_enabled: None,
        // The `ClientToken` that admitted the call is THIS service's idempotency guard, not
        // the EBS create's, so the volume is stored without one.
        client_token: None,
    })
}

/// Drop this service's record of every volume of Region `region` the service that OWNS EBS
/// volumes no longer holds.
///
/// The volume a successful `CreateVolume` reports IS that EBS volume, so there is ONE volume
/// and one answer to whether it exists: a volume removed through the owning service's own
/// API is gone, and an operation here that looks its id up answers as it does for an id that
/// was never minted, rather than acting on a record of a volume the account does not hold.
/// Whether the volume is gone is the owning store's own reading of it
/// ([`ec2_store::ComAmazonawsEc2Amazonec2State::volume_is_withdrawn`]), so the two services
/// cannot disagree about which ids resolve.
///
/// The removal was owed at the instant the owning service removed the volume, whether or not
/// anybody called here — so this materialises what has ALREADY happened, the way
/// [`crate::state::ComAmazonawsWorkspacesinstancesEucmifrontendapiserviceState::settle_all`]
/// materialises what the clock already owed, and a WRITER calls it. It goes through this
/// store's own `remove_volume`, so the creating `ClientToken` and the volume's modification
/// claims are retired with the record exactly as a `DeleteVolume` retires them.
pub fn drop_volumes_the_account_no_longer_holds<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &mut crate::state::ComAmazonawsWorkspacesinstancesEucmifrontendapiserviceState,
    region: &str,
) {
    let Some(owner) = ec2_store::service_state(engine) else {
        return;
    };
    let now = std::time::Instant::now();
    let withdrawn: Vec<String> = {
        let owner = owner.lock();
        state
            .volumes
            .values()
            .filter(|record| {
                record.region == region && owner.volume_is_withdrawn(&record.id, now)
            })
            .map(|record| record.id.clone())
            .collect()
    };
    for volume_id in withdrawn {
        state.remove_volume(region, &volume_id);
    }
}

/// Delete the EBS volume `volume_id`, as `DeleteVolume` does: the volume this service
/// reported IS that EBS volume, so deleting it removes it from the account and no EBS read
/// resolves it afterwards.
///
/// Answers whether the owning store held a volume to remove. Removal goes through that
/// store's own `remove_volume`, so the bytes it was charged and anything kept only with it
/// go back exactly as they would for a delete made over the EC2 API.
pub fn delete_ebs_volume<Eng: Send + Sync + 'static>(engine: &Eng, volume_id: &str) -> bool {
    ec2_store::service_state(engine).is_some_and(|state| {
        let mut store = state.lock();
        store.remove_volume(volume_id).is_some()
    })
}

/// This service's stored tag set as the owning service's own `Tag` list.
///
/// One tag is one `(Key, Value)` pair on either side — the two services shape the same EBS
/// tag — so the translation is this seam's whole content and neither side holds the other's
/// type.
fn ec2_volume_tags(
    tags: &BTreeMap<String, String>,
) -> Vec<::service_com_amazonaws_ec2_amazonec2::ops::types::Tag> {
    tags.iter()
        .map(
            |(key, value)| ::service_com_amazonaws_ec2_amazonec2::ops::types::Tag {
                key: Some(key.clone()),
                value: Some(value.clone()),
            },
        )
        .collect()
}
