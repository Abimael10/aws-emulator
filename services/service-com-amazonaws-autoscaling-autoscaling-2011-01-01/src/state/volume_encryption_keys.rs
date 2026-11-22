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
//! volume's data key, so a volume is created only where that ask would be served. TWO
//! things decide it, and either one refuses the launch:
//!
//! * the KEY ITSELF — a key that is in any key state but the one KMS serves, or that names
//!   no key of this account and Region at all, produces no data key;
//! * the AUTHORITIES in force over the principal that asks — the key's own key policy, the
//!   identity-based policies of that principal and the grants it holds on the key. The
//!   principal is the group's own service-linked role ("the service-linked role that the
//!   Auto Scaling group uses to call other Amazon Web Services service on your behalf",
//!   `CreateAutoScalingGroupType$ServiceLinkedRoleARN`), so a key whose policy authorizes
//!   nobody but the account root serves this launch no data key either. The decision is
//!   KMS's OWN ([`kms::authorization::authorize_use`], taken for the role as the
//!   principal), the way every service in this tree that uses a key AS a role decides it.
//!
//! Either way the volume cannot be created, so Amazon EC2 takes the attempt back under its
//! own `Client.InvalidKMSKey.InvalidState` reason ([`ec2::invalid_kms_key_state_reason`]) and
//! the instance never comes into service. The capacity change therefore places nothing and
//! ends as a terminal CANCELLED activity carrying that reason
//! ([`unusable_key_status_message`]), instead of reporting capacity the group never got.
//!
//! The key is resolved at the instant of the LAUNCH, the instant the launch-template version
//! itself is resolved: a group floating on `$Latest` or `$Default` launches from whatever
//! that word then names, so the key judged is the key of the version this launch reads — and
//! a launch made after the key was repaired, after the role was authorized, or after the word
//! moved to a version that names no customer key, is made.
//!
//! Nothing is refused on evidence that is not there: an engine carrying no KMS store judges
//! no key, a mapping naming no key of its own asks for the Amazon Web Services managed key of
//! Amazon EBS ([`EBS_MANAGED_KEY_ALIAS`]), which is not a key a caller's request can misname,
//! and an Amazon Web Services managed key's policy is managed by AWS for the service that
//! owns it, so no role needs an authority of its own on one.
//!
//! LOCKING. The launch-template versions are read under Amazon EC2's store lock
//! ([`super::ec2_instances::launch_template_data`]), which is released before KMS's is taken
//! for the one pass that resolves the keys; the role's own identity documents are read out of
//! IAM's store before that pass, so no two of those locks are ever held at once and nothing
//! awaits while any of them is. Neither Amazon EC2, KMS nor IAM reaches this service, so the
//! locks are never taken in the other order and no cycle exists.

use ::service_com_amazonaws_ec2_amazonec2::ops::types::RequestLaunchTemplateData;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
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

/// The KMS operation Amazon EBS performs on the key to create an encrypted volume: it
/// "requests a data key from KMS" for the volume, a 256-bit symmetric data key it never
/// receives in plaintext — the same ask Amazon EC2's own volume path makes of a key
/// ([`ec2::volume_key_refusal`]), so the two cannot judge one key by different asks.
///
/// That CALL, and not a read of the key's metadata, is what a key's state and the authorities
/// over the asking principal refuse, which is the whole point of pointing a volume at a
/// customer managed key.
const VOLUME_DATA_KEY_OPERATION: &str = "GenerateDataKeyWithoutPlaintext";

/// The KMS error codes a refused ask for a volume's data key carries, as the modelled error
/// shapes of that service spell them: the key is disabled, the key is in some other state no
/// cryptographic operation can use, the reference names no key, the reference is not a
/// well-formed ARN, and the authorities in force do not let the asking principal use the key.
///
/// They are the record Amazon EC2 keeps on the attempt it took back
/// ([`ec2::VolumeKeyRefusal`]); the instance's own state reason is the one Amazon EC2 words
/// for every one of them alike.
const DISABLED_KEY_CODE: &str = "DisabledException";
const INVALID_KEY_STATE_CODE: &str = "KMSInvalidStateException";
const ABSENT_KEY_CODE: &str = "NotFoundException";
const MALFORMED_KEY_ARN_CODE: &str = "InvalidArnException";
const UNAUTHORIZED_KEY_CODE: &str = "AccessDeniedException";

/// What the CANCELLED activity of a launch whose volume key will not serve records as its
/// `StatusMessage`: the instance never reached `InService` and was taken back, under the
/// Amazon EC2 reason that says why.
///
/// Fixed text for this class, carrying no part of the request: what the client acts on is the
/// reason the launch was cancelled, which it repairs on the key, on the key's policy or on the
/// template. The reason itself is read from the service that owns the instance
/// ([`ec2::invalid_kms_key_state_reason`]), so the activity and the instance it is about name
/// one cause one way.
#[must_use]
pub fn unusable_key_status_message() -> String {
    let (_, reason) = ec2::invalid_kms_key_state_reason();
    format!(
        "Instance became unhealthy while waiting for instance to be in InService state. \
         Termination Reason: {reason}"
    )
}

/// Why a launch creates no volume: what the capacity change's terminal activity records, and
/// what the service that owns the key SAID about it.
///
/// The two have different readers: the message is the `Activity$StatusMessage` a client reads
/// the outcome of the capacity change off, and the refusal is the record Amazon EC2 keeps on
/// the attempt it takes back ([`ec2::VolumeKeyRefusal`]) — the launch IS made, since Amazon
/// EC2 accepts it and withdraws the instance once the volume cannot be created.
#[derive(Debug, Clone)]
pub struct UnusableVolumeKey {
    /// The cause the cancelled activity reports.
    pub status_message: String,
    /// KMS's own refusal of the volume's data key, as that service words it.
    pub refusal: ec2::VolumeKeyRefusal,
}

/// Why a launch from these launch sources creates no volume, or `None` when every key its
/// launch templates name will serve one for this group.
///
/// `region` is the Region the group lives in, which is the Region its instances and their
/// volumes are created in and so the Region its keys answer at: a key ARN naming another
/// Region names no key the volume could be created under, which is the ordinary outcome of
/// asking KMS at the group's own Region rather than a case of its own.
///
/// `service_linked_role_arn` is the role the group makes the launch under, which is the
/// principal the authorities over the key are weighed for.
pub fn unusable_volume_key_launch_failure<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    service_linked_role_arn: &str,
    launch_template: Option<&LaunchTemplateSpecification>,
    mixed_instances_policy: Option<&MixedInstancesPolicy>,
) -> Option<UnusableVolumeKey> {
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
    // The role's own identity-based documents are read out of IAM's store HERE, before KMS's
    // store is locked below: no two of those locks are held at once.
    let caller = kms::authorization::role_caller(engine, service_linked_role_arn);
    let store = state.lock();
    keys.iter()
        .find_map(|key| volume_key_refusal(&store, region, key, &caller))
        .map(|refusal| UnusableVolumeKey {
            status_message: unusable_key_status_message(),
            refusal,
        })
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

/// Why the key `key_id` names at `region` will not produce the data key a volume created
/// there for `caller` is encrypted with, or `None` when it will.
///
/// The key is resolved the way `kms:DescribeKey` resolves one — by key id, key ARN, alias or
/// alias ARN — and then held to the two things that decide the ask Amazon EBS makes of it: the
/// key's own state, and the authorities in force over the principal asking. The second is
/// KMS's own decision ([`kms::authorization::authorize_use`]) over the key record KMS's own
/// handlers wrote, taken for the use as it ARRIVES — through the service that creates the
/// volume, which is what a key policy scoped by `kms:ViaService` is written against.
fn volume_key_refusal(
    store: &kms::ComAmazonawsKmsTrentserviceState,
    region: &str,
    key_id: &str,
    caller: &kms::authorization::Caller,
) -> Option<ec2::VolumeKeyRefusal> {
    if kms::alias_named_by(region, key_id).as_deref() == Some(EBS_MANAGED_KEY_ALIAS) {
        return None;
    }
    let key = match store.resolve(region, key_id) {
        kms::KeyRef::Found(key) => key,
        kms::KeyRef::MalformedArn => {
            return Some(refusal(MALFORMED_KEY_ARN_CODE, kms::invalid_arn_message(key_id)))
        }
        kms::KeyRef::UnauthorizedAccount => {
            return Some(refusal(
                UNAUTHORIZED_KEY_CODE,
                kms::unauthorized_resource_message(&caller.arn, VOLUME_DATA_KEY_OPERATION),
            ))
        }
        // A reference KMS answers for no key of this account and Region at all — a key ARN of
        // another Region among them — names no key the volume could be encrypted with.
        kms::KeyRef::NotFound => {
            return Some(refusal(
                ABSENT_KEY_CODE,
                kms::key_not_found_message(region, key_id),
            ))
        }
    };
    // Only a key in the one key state KMS serves produces a data key, whoever asks.
    let key_state = key.reported_key_state();
    if key_state != kms::KEY_STATE_ENABLED {
        let code = if key_state == kms::KEY_STATE_DISABLED {
            DISABLED_KEY_CODE
        } else {
            INVALID_KEY_STATE_CODE
        };
        return Some(refusal(code, kms::invalid_state_message(key)));
    }
    // An Amazon Web Services managed key's policy is managed by AWS for the service that owns
    // the alias, so no principal needs an authority of its own on one.
    if key.key_manager == kms::KEY_MANAGER_AWS {
        return None;
    }
    kms::authorization::authorize_use(
        caller,
        store,
        region,
        key,
        kms::authorization::RequestedUse::through_service(
            VOLUME_DATA_KEY_OPERATION,
            &via_service(region),
        ),
        // The volume does not exist yet, so the ask carries no encryption context of its own
        // for a conditioned statement to be decided against.
        None,
        // And it presents no grant token: the authority a grant gives this role is the role's
        // already ([`kms::authorization::authorize_use`] weighs the grants it holds).
        None,
    )
    .err()
    .map(|why| refusal(UNAUTHORIZED_KEY_CODE, why))
}

/// One refusal of a volume's data key, in the shape the service that owns the instance keeps
/// it in: the error code of the service that owns the key, and what that service said.
fn refusal(code: &str, message: String) -> ec2::VolumeKeyRefusal {
    ec2::VolumeKeyRefusal {
        code: code.to_string(),
        message: Some(message),
    }
}

/// The value of the `kms:ViaService` condition key for the ask that creates the volume: the
/// regional endpoint name of the service that creates it, which is the service the data-key
/// request arrives through.
///
/// Read from that service's OWN published endpoint identity
/// ([`ec2::region_endpoint`]), so the value a conditioned key policy is judged against carries
/// the Region's partition rather than a DNS suffix spelled out here.
fn via_service(region: &str) -> String {
    ec2::region_endpoint(region)
}
