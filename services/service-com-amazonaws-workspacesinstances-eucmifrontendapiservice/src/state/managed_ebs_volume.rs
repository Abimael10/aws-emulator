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
//! This module is that seam. Nothing here invents a volume or an attachment: the record is
//! written by the owning service's own writers
//! ([`ec2_store::ComAmazonawsEc2Amazonec2State::insert_volume`], which mints the `VolumeId`
//! out of ITS id counter, builds the EBS `Volume` shape its own `CreateVolume` builds and
//! charges the bytes against its own store budget, and
//! [`ec2_store::ComAmazonawsEc2Amazonec2State::attach_volume_to_instance`], which brings an
//! attachment up exactly as that service's own `AttachVolume` does) — so the id this service
//! reports is the id that store holds, and every EBS read of it (a describe, a tag, an
//! attach) resolves the same record.
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
use std::time::SystemTime;

use ::service_com_amazonaws_ec2_amazonec2::ops::types::AttachVolumeOutput;
use ::service_com_amazonaws_ec2_amazonec2::state::{self as ec2_store, VolumeSpec};

use crate::ops::types::{CreateVolumeInput, ValidationException};
use crate::state::{OutOfWindow, WindowSide};

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
    backing_service_refusal(unresolvable_snapshot_message(snapshot_id))
}

/// The modelled `ValidationException` (HTTP 400) for a refusal the BACKING service made,
/// carrying what that service answered as `message`.
///
/// The ONE place such a refusal is shaped, so every one of them reads alike to a client:
/// `Reason` is [`crate::state::VALIDATION_REASON_DEPENDENCY_FAILURE`] — the call was
/// refused by the service this one depends on, not by a constraint of this service's own
/// request shape — and there is no `FieldList`, there being no member of THIS request the
/// dependency faulted.
fn backing_service_refusal(message: String) -> ValidationException {
    ValidationException {
        message,
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

/// The code the backing Amazon EBS/EC2 service classifies a parameter value it will not
/// take under, read from the crate that owns those APIs so one code is spelled once.
const INVALID_PARAMETER_VALUE_CODE: &str = ec2_store::INVALID_PARAMETER_VALUE;

/// What the backing Amazon EBS service answers for a provisioned-performance value outside
/// the window the requested volume type publishes: the fixed `Volume `, the member as that
/// service names it, ` of `, the value the request sent, and whether it is `too high` with
/// the window's maximum or `too low` with its minimum.
///
/// RECORDED: the sentence AWS answered for a create whose `Iops` was above the window its
/// volume type publishes. The member and the value are the request's, the bound is read off
/// this service's own published catalog ([`crate::state::VOLUME_TYPE_SPECS`]), and the
/// member is lower case here because that is how the backing service names a volume's
/// performance members in this sentence.
fn volume_performance_out_of_window(out: OutOfWindow<'_>) -> String {
    let member = out.member.to_ascii_lowercase();
    let value = out.value;
    let bound = out.bound();
    match out.side {
        WindowSide::Above => format!("Volume {member} of {value} is too high; maximum is {bound}."),
        WindowSide::Below => format!("Volume {member} of {value} is too low; minimum is {bound}."),
    }
}

/// The modelled `ValidationException` (HTTP 400) for a create the backing Amazon EBS
/// service refuses for a provisioned-performance value outside the window the volume type
/// publishes.
///
/// The window is enforced by that service's own create, so what this service answers is its
/// refusal — the wrapper naming the call that failed ([`create_volume_failed`]) around the
/// code and sentence EBS worded for it — rather than a constraint of this service's request
/// shape ([`backing_service_refusal`]). Passed to
/// [`crate::state::VolumeTypeSpec::require_accepted_configuration`] by the create path, so
/// the rule stays shared and only its wording is this call's.
pub fn performance_refused_by_backing_service(out: OutOfWindow<'_>) -> ValidationException {
    backing_service_refusal(create_volume_failed(
        INVALID_PARAMETER_VALUE_CODE,
        &volume_performance_out_of_window(out),
    ))
}

/// How this service reports an attach the backing Amazon EC2 service refused: the fixed
/// `Ec2 can't associate volume `, the volume the request named, ` to instance `, the managed
/// EC2 instance the WorkSpace Instance IS, then the code EC2 classified the refusal as and
/// the sentence it worded for it in parentheses.
///
/// RECORDED: the message AWS answered for an attach EC2 refused. The wrapper is this
/// service's own and the code and sentence inside it are the dependency's, exactly as
/// [`create_volume_failed`] wraps a refused create.
fn associate_volume_failed(
    volume_id: &str,
    ec2_instance_id: &str,
    code: &str,
    message: &str,
) -> String {
    format!("Ec2 can't associate volume {volume_id} to instance {ec2_instance_id} ({code}: {message})")
}

/// What the backing Amazon EC2 service answers for an attach at a device path a volume
/// already occupies on the instance: the fixed text it words for a `unixDevice` value it
/// will not take, carrying the device path the request named in both of its places.
fn attachment_point_in_use(device: &str) -> String {
    format!(
        "Invalid value '{device}' for unixDevice. Attachment point {device} is already in use"
    )
}

/// The modelled `ValidationException` (HTTP 400) for an attach at a device path the
/// instance already carries a volume at.
///
/// The attach is performed by the service that owns the instance, against the managed EC2
/// instance this WorkSpace Instance IS, so an attachment point already in use is THAT
/// service's refusal of the `Device` the request named — a value no later call makes
/// attachable, which is why it is the modelled `ValidationException` (HTTP 400) and not the
/// state conflict a wait could clear.
pub fn device_already_in_use(
    volume_id: &str,
    ec2_instance_id: &str,
    device: &str,
) -> ValidationException {
    backing_service_refusal(associate_volume_failed(
        volume_id,
        ec2_instance_id,
        INVALID_PARAMETER_VALUE_CODE,
        &attachment_point_in_use(device),
    ))
}

/// The `Message` this service answers a call the backing Amazon EBS service will not make
/// because the volume is not in the `available` state — a volume still attached to a
/// WorkSpace Instance ("The volume must be in the `available` state (not attached to an
/// instance)", the backing EBS `DeleteVolume`).
///
/// RECORDED: the sentence AWS answered for a delete of an attached volume. The fixed text is
/// `EBS Volume is not in available state for volume `, and the volume id the request named
/// is its only variable part — so a caller deleting several volumes at once reads which one
/// the refusal is about.
pub fn volume_not_available_message(volume_id: &str) -> String {
    format!("EBS Volume is not in available state for volume {volume_id}")
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

/// Attach the Amazon EBS volume `volume_id` to the managed Amazon EC2 instance
/// `ec2_instance_id` at `device`, as `AssociateVolume` does, and answer whether the owning
/// store held the volume to attach.
///
/// The attachment this service accepts IS that volume's EBS attachment: the volume is an EBS
/// volume of the account and the WorkSpace Instance is the managed EC2 instance, so the
/// relation is recorded in the store that OWNS the volume and every EBS read of it — a
/// describe, a snapshot, a detach made over the EC2 API — resolves the one attachment, the
/// same way [`create_ebs_volume`] records the volume itself there. It is an INSTANCE's own
/// attachment, naming the `InstanceId` the volume is exposed to and the `Device` it is
/// exposed at, so it carries neither marker of an attachment to an Amazon Web
/// Services-managed resource ("the ARN of the Amazon Web Services-managed resource to which
/// the volume is attached" / "the service principal of the Amazon Web Services service that
/// owns the underlying resource", each returned only for those) — which is also why its
/// `DeleteOnTermination` is the one the owning service answers for a volume attached to a
/// running instance ([`ec2_store::ATTACHMENT_DELETE_ON_TERMINATION`]) rather than a launch's.
///
/// The attachment is brought up the way that store brings one up: `attaching` now, with its
/// clock owing `attached`, so a caller that reads straight after this call sees the
/// transient state.
pub fn attach_ebs_volume<Eng: Send + Sync + 'static>(
    engine: &Eng,
    volume_id: &str,
    ec2_instance_id: &str,
    device: &str,
) -> bool {
    let Some(state) = ec2_store::service_state(engine) else {
        return false;
    };
    let attachment = AttachVolumeOutput {
        volume_id: Some(volume_id.to_string()),
        instance_id: Some(ec2_instance_id.to_string()),
        device: Some(device.to_string()),
        state: Some(ec2_store::VOLUME_ATTACHMENT_STATE_ATTACHING.to_string()),
        // "The time stamp when the attachment initiated" (`VolumeAttachment$AttachTime`):
        // this call's own clock reading.
        attach_time: Some(ec2_store::system_time_to_date_time(SystemTime::now())),
        delete_on_termination: Some(ec2_store::ATTACHMENT_DELETE_ON_TERMINATION),
        associated_resource: None,
        instance_owning_service: None,
        ebs_card_index: Some(ec2_store::DEFAULT_EBS_CARD_INDEX),
    };
    let mut store = state.lock();
    store.attach_volume_to_instance(volume_id, attachment)
}

/// Detach the Amazon EBS volume `volume_id` from the managed Amazon EC2 instance
/// `ec2_instance_id` at `device`, as `DisassociateVolume` does, and answer whether the
/// owning store held that attachment.
///
/// The mirror of [`attach_ebs_volume`], for the same reason: the attachment this service
/// removes is the volume's EBS attachment, so it is taken down in the store that owns the
/// volume — `detaching` now with its removal owed by that store's clock, exactly as a detach
/// made over the EC2 API — and no EBS read reports the volume attached afterwards.
pub fn detach_ebs_volume<Eng: Send + Sync + 'static>(
    engine: &Eng,
    volume_id: &str,
    ec2_instance_id: &str,
    device: &str,
) -> bool {
    ec2_store::service_state(engine).is_some_and(|state| {
        state
            .lock()
            .detach_volume_from_instance(volume_id, ec2_instance_id, device)
    })
}

/// Bring this service's volume records into line with what the service that OWNS EBS volumes
/// holds, for Region `region`: a volume it no longer holds goes from here too, and so does an
/// attachment it no longer holds.
///
/// The volume a successful `CreateVolume` reports IS that EBS volume, so there is ONE volume
/// and one answer to whether it exists: a volume removed through the owning service's own
/// API is gone, and an operation here that looks its id up answers as it does for an id that
/// was never minted, rather than acting on a record of a volume the account does not hold.
/// Whether the volume is gone is the owning store's own reading of it
/// ([`ec2_store::ComAmazonawsEc2Amazonec2State::volume_is_withdrawn`]), so the two services
/// cannot disagree about which ids resolve.
///
/// An ATTACHMENT answers to the same rule, in the same direction: the attachment
/// `AssociateVolume` makes is the volume's EBS attachment ([`attach_ebs_volume`]), so one the
/// owning store no longer holds — taken down over the EC2 API, or released when the instance
/// it was on went away — is one this service holds no more either, and the volume it was on is
/// free to be attached again and free to be deleted.
///
/// Both were owed at the instant the owning service made them, whether or not anybody called
/// here — so this materialises what has ALREADY happened, the way
/// [`crate::state::ComAmazonawsWorkspacesinstancesEucmifrontendapiserviceState::settle_all`]
/// materialises what the clock already owed, and a WRITER calls it. The removal goes through
/// this store's own `remove_volume`, so the creating `ClientToken` and the volume's
/// modification claims are retired with the record exactly as a `DeleteVolume` retires them.
pub fn settle_volumes_against_the_owning_service<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &mut crate::state::ComAmazonawsWorkspacesinstancesEucmifrontendapiserviceState,
    region: &str,
) {
    let Some(owner) = ec2_store::service_state(engine) else {
        return;
    };
    let now = std::time::Instant::now();
    let (withdrawn, detached) = {
        let owner = owner.lock();
        let held: Vec<&crate::state::VolumeRecord> = state
            .volumes
            .values()
            .filter(|record| record.region == region)
            .collect();
        let withdrawn: Vec<String> = held
            .iter()
            .filter(|record| owner.volume_is_withdrawn(&record.id, now))
            .map(|record| record.id.clone())
            .collect();
        // Which of the volumes the account still holds no longer carry the attachment this
        // service recorded for them. The instance half of the attachment is the managed EC2
        // instance the WorkSpace Instance IS, read off this service's own record of it — the
        // same id the attach was made against. A WorkSpace Instance that has none (one whose
        // launch EC2 refused) has no EBS attachment for the owning store to have taken down,
        // so there is nothing of it to follow here.
        let detached: Vec<String> = held
            .iter()
            .filter(|record| !owner.volume_is_withdrawn(&record.id, now))
            .filter(|record| {
                record.attachment.as_ref().is_some_and(|attachment| {
                    state
                        .instances
                        .get(&attachment.workspace_instance_id)
                        .and_then(|instance| instance.ec2_instance_id.as_deref())
                        .is_some_and(|ec2_instance_id| {
                            !owner.volume_is_attached_at(
                                &record.id,
                                ec2_instance_id,
                                &attachment.device,
                                now,
                            )
                        })
                })
            })
            .map(|record| record.id.clone())
            .collect();
        (withdrawn, detached)
    };
    for volume_id in withdrawn {
        state.remove_volume(region, &volume_id);
    }
    for volume_id in detached {
        if let Some(record) = state.volume_mut(region, &volume_id) {
            record.attachment = None;
        }
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
