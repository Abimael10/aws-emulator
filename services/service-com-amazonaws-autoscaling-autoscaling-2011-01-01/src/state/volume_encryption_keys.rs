// SPDX-License-Identifier: CC0-1.0

//! The KMS key the volumes of a launch an Auto Scaling group makes are encrypted under,
//! resolved against the account's own KMS store.
//!
//! A launch template's `BlockDeviceMappings` may name a customer managed key for a volume
//! the launch creates, and that `KmsKeyId` is a REFERENCE to another service's resource:
//! Amazon EC2 stores whatever well-formed value `CreateLaunchTemplate` was given, and the
//! reference is resolved when the template is CONSUMED. Amazon EC2 Auto Scaling is one of
//! the consumers — a capacity change launches instances from the template — so this module
//! resolves it there, exactly as [`super::iam_instance_profiles`] resolves the template's
//! instance profile against IAM.
//!
//! An encrypted volume is not encrypted by Amazon EBS alone: Amazon EBS asks KMS for the
//! volume's data key, so a key that is in any key state but the one KMS serves, or that
//! names no key of this account and Region at all, produces no data key — the volume cannot
//! be created, the instance never comes into service, and Amazon EC2 takes the attempt back
//! under its own `Client.InvalidKMSKey.InvalidState` reason. The capacity change therefore
//! places nothing and ends as a terminal CANCELLED activity carrying that reason
//! ([`unusable_key_status_message`]), instead of reporting capacity the group never got.
//!
//! The key is resolved at the instant of the LAUNCH, the instant the launch-template version
//! itself is resolved: a group floating on `$Latest` or `$Default` launches from whatever
//! that word then names, so the key judged is the key of the version this launch reads — and
//! a launch made after the key was repaired, or after the word moved to a version that names
//! no customer key, is made.
//!
//! Nothing is refused on evidence that is not there: an engine carrying no KMS store judges
//! no key, and a mapping naming no key of its own asks for the Amazon Web Services managed
//! key of Amazon EBS ([`EBS_MANAGED_KEY_ALIAS`]), which is not a key a caller's request can
//! misname.
//!
//! LOCKING. The launch-template versions are read under Amazon EC2's store lock
//! ([`super::ec2_instances::launch_template_data`]), which is released before KMS's is taken
//! for the one pass that resolves the keys; nothing awaits while either is held. Neither
//! Amazon EC2 nor KMS reaches this service, so the locks are never taken in the other order
//! and no cycle exists.

use ::service_com_amazonaws_ec2_amazonec2::ops::types::RequestLaunchTemplateData;
use ::service_com_amazonaws_kms_trentservice::state as kms;

use crate::ops::types::{LaunchTemplateSpecification, MixedInstancesPolicy};

/// The alias of the Amazon Web Services managed key Amazon EBS encrypts a volume with when
/// the volume names no key of its own.
///
/// "You can recognize Amazon Web Services aliases because their names have the format
/// `aws/<service-name>`" (`smithy_query(query="doc",shape="ListAliases")` in the KMS model),
/// under the name Amazon EBS carries — the key `CreateVolume`'s own documentation calls "your
/// KMS key for Amazon EBS". KMS associates a managed key with such a predefined alias when it
/// is first asked for, so the alias always names an enabled key and its key state is not one a
/// caller moves: a volume under it is served, and nothing about it is resolved here.
pub const EBS_MANAGED_KEY_ALIAS: &str = "alias/aws/ebs";

/// The Amazon EC2 state-reason code for an instance whose volume could not be created because
/// the KMS key it was to be encrypted under will not serve it.
pub const INVALID_KEY_STATE_CODE: &str = "Client.InvalidKMSKey.InvalidState";

/// What that code says about the key: FIXED text for this class of refusal — the key's own
/// state is not restated, so every state outside the one that serves is reported alike.
const INVALID_KEY_STATE_REASON: &str = "The KMS key provided is in an incorrect state";

/// What the CANCELLED activity of a launch whose volume key will not serve records as its
/// `StatusMessage`: the instance never reached `InService` and was taken back, under the
/// Amazon EC2 reason that says why.
///
/// Fixed text for this class, carrying no part of the request: what the client acts on is the
/// reason the launch was cancelled, which it repairs on the key or on the template.
#[must_use]
pub fn unusable_key_status_message() -> String {
    format!(
        "Instance became unhealthy while waiting for instance to be in InService state. \
         Termination Reason: {INVALID_KEY_STATE_CODE}: {INVALID_KEY_STATE_REASON}"
    )
}

/// Why a launch from these launch sources creates no volume, or `None` when every key its
/// launch templates name will serve one.
///
/// `region` is the Region the group lives in, which is the Region its instances and their
/// volumes are created in and so the Region its keys answer at: a key ARN naming another
/// Region names no key the volume could be created under, which is the ordinary outcome of
/// asking KMS at the group's own Region rather than a case of its own.
pub fn unusable_volume_key_launch_failure<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    launch_template: Option<&LaunchTemplateSpecification>,
    mixed_instances_policy: Option<&MixedInstancesPolicy>,
) -> Option<String> {
    let keys: Vec<String> =
        super::ec2_instances::specifications(launch_template, mixed_instances_policy)
            .into_iter()
            .filter_map(|specification| {
                super::ec2_instances::launch_template_data(engine, specification)
            })
            .flat_map(named_volume_keys)
            .collect();
    if keys.is_empty() {
        return None;
    }
    let state = kms::service_state(engine)?;
    let store = state.lock();
    keys.iter()
        .any(|key| !serves_a_volume(&store, region, key))
        .then(unusable_key_status_message)
}

/// The keys the block device mappings of ONE launch-template version name, in the order the
/// version carries them.
///
/// "The block device mapping" (`RequestLaunchTemplateData$BlockDeviceMappings`) a version
/// carries applies to every launch made from it, so every volume that mapping describes is a
/// volume this launch creates — the root device and any other. A mapping naming no key, or
/// naming an empty one, names no key to resolve.
fn named_volume_keys(data: RequestLaunchTemplateData) -> Vec<String> {
    data.block_device_mappings
        .into_iter()
        .flatten()
        .filter_map(|mapping| mapping.ebs)
        .filter_map(|ebs| ebs.kms_key_id)
        .filter(|key_id| !key_id.is_empty())
        .collect()
}

/// Whether the key `key_id` names at `region` will produce the data key a volume created
/// there is encrypted with.
///
/// The key is resolved the way `kms:DescribeKey` resolves one — by key id, key ARN, alias or
/// alias ARN — and only a key in the one key state KMS serves produces a data key. A
/// reference KMS answers for no key of this account and Region at all (a key ARN of another
/// Region or account among them, and a value that is no key identifier) serves no volume
/// either: there is no key to encrypt it with.
fn serves_a_volume(
    store: &kms::ComAmazonawsKmsTrentserviceState,
    region: &str,
    key_id: &str,
) -> bool {
    if kms::alias_named_by(region, key_id).as_deref() == Some(EBS_MANAGED_KEY_ALIAS) {
        return true;
    }
    matches!(
        store.resolve(region, key_id),
        kms::KeyRef::Found(key) if key.reported_key_state() == kms::KEY_STATE_ENABLED
    )
}
