// SPDX-License-Identifier: CC0-1.0

//! The KMS keys the EBS volumes of a LAUNCH are to be encrypted under, and what becomes of
//! the instance when one of them will not serve.
//!
//! An encrypted volume is not encrypted by Amazon EBS alone: Amazon EBS asks KMS for the
//! volume's data key ([`super::volume_key_refusal`], the one ask this crate makes of a
//! key), so a key in any key state KMS will not use — and equally a reference that names no
//! key of the account at all — produces no data key and the volume cannot come into
//! existence. A launch's root volume is the volume its instance boots from, so an instance
//! whose volumes cannot be created never runs: Amazon EC2 accepts the launch, answers the
//! `pending` instance it minted, and then takes that instance back — `shutting-down` and then
//! `terminated`, the pair every instance leaving makes — under the state reason that says why
//! ([`INVALID_KMS_KEY_STATE_CODE`]) — which is what a client watching the
//! instance repairs, on the key or on the launch parameters.
//!
//! The judgement is KMS's and is made at the instant of the LAUNCH, so a launch made after
//! the key was repaired is made; a launch that names no customer managed key asks for the
//! Amazon Web Services managed key of Amazon EBS, which is no key a request can misname and
//! so is nothing to resolve.

use std::time::Instant;

use crate::ops::types::{BlockDeviceMapping, EbsBlockDevice};

use super::{ComAmazonawsEc2Amazonec2State, InstanceStateName, PendingTransition, VolumeKeyRefusal};

/// The `StateReason$Code` Amazon EC2 records on an instance it took back because the KMS key
/// its volumes were to be encrypted under will not produce their data key.
///
/// The same code the Auto Scaling side of this emulator reports for a launch it could not
/// make for that reason, so one cause is named one way wherever it surfaces.
pub const INVALID_KMS_KEY_STATE_CODE: &str = "Client.InvalidKMSKey.InvalidState";

/// What that code SAYS about the key: fixed text for this class of refusal, carrying no part
/// of the request and not restating the key's own state — every state outside the one that
/// serves is reported alike.
const INVALID_KMS_KEY_STATE_DESCRIPTION: &str = "The KMS key provided is in an incorrect state";

/// The `StateReason` of such an instance, as `(code, message)`: "the message for the state
/// change" (`StateReason$Message`) is the code followed by its description, the shape every
/// instance state reason of this crate takes.
#[must_use]
pub fn invalid_kms_key_state_reason() -> (String, String) {
    (
        INVALID_KMS_KEY_STATE_CODE.to_string(),
        format!("{INVALID_KMS_KEY_STATE_CODE}: {INVALID_KMS_KEY_STATE_DESCRIPTION}"),
    )
}

/// How many distinct keys ONE launch is resolved against.
///
/// CHOSEN, not an AWS-published figure: each key is one call into the service that owns it, and
/// a request names its block device mappings itself, so without a ceiling here one launch could
/// turn a list of mappings into as many calls as its body has room for. It sits far above the
/// Amazon EBS volumes an instance can hold at all — the EC2 User Guide's instance volume limits
/// put those in the tens — so no launch an instance type could serve reaches it.
const MAX_LAUNCH_VOLUME_KEYS: usize = 64;

/// The customer managed keys the volumes of one launch would be encrypted under, each once and
/// in the order the launch names them, at most [`MAX_LAUNCH_VOLUME_KEYS`] of them.
///
/// `root_block_device` is the root device the launch RESOLVED — the AMI's own mapping with
/// the launch's entry for that device overlaid and the Region's EBS encryption default
/// applied ([`ComAmazonawsEc2Amazonec2State::launch_root_block_device`]) — so a key that
/// reaches the launch from the image rather than from the request is asked about too, and
/// only for a device that is really encrypted. `mappings` is the launch's own
/// `BlockDeviceMappings` as resolved (a launch template's entries included): a mapping that
/// names a key names a volume the launch creates under it, whichever device it is for.
///
/// The ONE reading of "which keys does this launch need", so the ask made of KMS and the
/// instance's fate cannot be about different keys.
#[must_use]
pub fn launch_volume_encryption_keys(
    root_block_device: Option<&EbsBlockDevice>,
    mappings: &[BlockDeviceMapping],
) -> Vec<String> {
    let named = mappings.iter().filter_map(|mapping| mapping.ebs.as_ref());
    let mut keys: Vec<String> = Vec::new();
    for key_id in root_block_device
        .filter(|ebs| ebs.encrypted == Some(true))
        .into_iter()
        .chain(named)
        .filter_map(|ebs| ebs.kms_key_id.clone())
        .filter(|key_id| !key_id.is_empty())
    {
        if keys.len() >= MAX_LAUNCH_VOLUME_KEYS {
            break;
        }
        if !keys.contains(&key_id) {
            keys.push(key_id);
        }
    }
    keys
}

impl ComAmazonawsEc2Amazonec2State {
    /// Take the launch of the instance `instance_id` names back, because the KMS key its
    /// volumes were to be encrypted under refused their data key (`refusal`).
    ///
    /// The instance stays in the `pending` state the launch answered with — the launch itself
    /// was accepted — and the clock owes it the SHUTDOWN instead of `running`: `shutting-down`,
    /// carrying the state reason that says why as that move is applied, and then `terminated`
    /// one span later, the pair every instance that leaves on its way out makes
    /// ([`Self::materialize_due_transitions`], which completes it). So a caller that reads
    /// straight after the launch still sees the instance coming up, one that reads while it is
    /// going sees `shutting-down` with the reason, and one that waits sees it `terminated` —
    /// whether or not anybody called in between.
    ///
    /// The volumes that launch created go with it, withdrawn at the same instant they would
    /// have reached `available` — exactly as the volume of a `CreateVolume` under such a key
    /// is ([`super::VolumeRecord::withdrawn_at`]): the key refused them, so they never came
    /// into being rather than existing attached to an instance that is gone.
    ///
    /// IDEMPOTENT: an instance already on its way to, or at, `terminated` keeps the
    /// transition it has.
    pub fn withdraw_launch_under_unusable_volume_key(
        &mut self,
        instance_id: &str,
        refusal: &VolumeKeyRefusal,
    ) {
        let now = Instant::now();
        let Some(instance) = self.instances.get_mut(instance_id) else {
            return;
        };
        if instance.state.is_terminating() {
            return;
        }
        ::tracing::debug!(
            instance_id,
            refusal = %refusal,
            "a volume of this launch could not be encrypted, so the instance is taken back \
             rather than run"
        );
        let (code, message) = invalid_kms_key_state_reason();
        instance.pending_transition = Some(
            PendingTransition::starting_now(InstanceStateName::ShuttingDown)
                .reporting(code, message),
        );
        // The guest never comes up: the instance it would have booted on is on its way out.
        instance.boot_due = None;
        for (volume_id, _) in self.instance_attachments_at(instance_id, now) {
            if let Some(record) = self.volumes.get_mut(&volume_id) {
                record.withdrawn_at = Some(record.available_at);
            }
        }
    }
}
