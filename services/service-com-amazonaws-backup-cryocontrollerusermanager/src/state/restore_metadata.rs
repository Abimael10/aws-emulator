// SPDX-License-Identifier: CC0-1.0

//! The restore METADATA a start has to carry for the type of resource it restores.
//!
//! # The rule this module implements
//!
//! `StartRestoreJob`'s `Metadata` is "a set of metadata key-value pairs ... values in
//! addition to those provided by `GetRecoveryPointRestoreMetadata` might be required to
//! restore a resource. For example, you might need to provide a new resource name if the
//! original already exists" (the model's own doc for the member). For a resource type whose
//! restore cannot be performed without such a value, metadata that carries no entry for it
//! is a value AWS Backup REFUSES synchronously, with the parameter-value class and the
//! sentence for that key: the restore is not started, so nothing is stored and no restore job
//! is listed for the recovery point.
//!
//! The refusal is taken BEFORE the job is stored, which is what keeps it a refusal rather
//! than a failed restore: a start the service accepts stores a job every read answers from,
//! while a start it refuses leaves no trace at all.
//!
//! One entry per resource type that requires a metadata key; the first is Amazon DynamoDB,
//! whose restore creates a table under the name the metadata gives
//! ([`super::METADATA_TARGET_TABLE_NAME`]). A type with no entry here requires no particular
//! key, so its metadata is whatever the restore of that type reads.
//!
//! The same catalog answers part of the READ side ([`restore_metadata_of`]): the pairs a
//! stored recovery point reports include the keys the restore of its resource type reads,
//! filled from the resource the backup captured — so the metadata a client is HANDED for a
//! recovery point is metadata a start from it requires, and the two sides of one contract
//! cannot drift apart. The rest of what a point reports is the configuration the backup
//! captured from the resource's own service ([`super::captured_restore_metadata`]).

use std::collections::BTreeMap;

use crate::ops::types::InvalidParameterValueException;

/// The lead every restore-metadata refusal opens with.
const MESSAGE_INVALID_RESTORE_METADATA: &str = "Invalid restore metadata.";

/// What a restore of one resource type cannot be started without: the metadata key, and the
/// sentence AWS words its absence with.
///
/// SOURCE: the per-resource metadata AWS publishes for a restore (AWS Backup, *Metadata for
/// Amazon DynamoDB*, which the model's doc for `StartRestoreJob$Metadata` links) — the key a
/// restore of that type has no target without.
const REQUIRED_RESTORE_METADATA: &[(&str, &str, &str)] = &[(
    super::RESOURCE_TYPE_DYNAMODB,
    super::METADATA_TARGET_TABLE_NAME,
    "Target table name must be provided with restore metadata.",
)];

/// The key every recovery point's restore metadata names the backup request that captured it
/// under.
///
/// SOURCE: the restore metadata AWS Backup reports for a recovery point, where this is the
/// one key in the service's own `aws:backup:` namespace rather than the owning service's — so
/// it is answered for every resource type, from the job that wrote the point.
const METADATA_BACKUP_REQUEST_ID: &str = "aws:backup:request-id";

/// The restore metadata a stored recovery point CARRIES: what
/// `GetRecoveryPointRestoreMetadata` reports for `point`.
///
/// That read reports "the set of metadata key-value pairs that describe the original
/// configuration of the backed-up resource" (the member's model doc), and a restore is
/// started with those pairs — "values in addition to those provided by
/// `GetRecoveryPointRestoreMetadata` might be required ... for example, you might need to
/// provide a new resource name if the original already exists" (the model's doc for
/// `StartRestoreJob$Metadata`). Three sources answer it, each from state the backup already
/// stored, so two reads of one point can never disagree and the read performs no write:
///
/// * the keys the restore of that resource type READS
///   ([`REQUIRED_RESTORE_METADATA`], the catalog the synchronous refusal below measures a
///   start against), each filled with the backed-up resource's own name as the point's stored
///   ARN gives it ([`super::resource_name_of`]) — so a client that hands the metadata back
///   unchanged carries every key that start requires, whatever else the point knows;
/// * the CONFIGURATION the backup captured from the resource's own service
///   ([`super::captured_restore_metadata`]), which is the authority on how the resource was
///   configured and therefore overrides the pairs above where it names the same key;
/// * the backup REQUEST that captured the point, which is the job stored on it.
#[must_use]
pub fn restore_metadata_of(point: &super::RecoveryPoint) -> BTreeMap<String, String> {
    let mut metadata = required_restore_metadata_of(
        point.resource_type.as_deref(),
        super::resource_name_of(&point.resource_arn),
    );
    metadata.extend(
        point
            .captured_restore_metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    metadata.insert(
        METADATA_BACKUP_REQUEST_ID.to_string(),
        point.created_by_job_id.clone(),
    );
    metadata
}

/// The keys the restore of a `resource_type` resource reads, filled with that resource's own
/// name — nothing for a type the catalog names no key for, and nothing for a resource whose
/// ARN names no resource at all.
fn required_restore_metadata_of(
    resource_type: Option<&str>,
    resource_name: Option<String>,
) -> BTreeMap<String, String> {
    let (Some(resource_type), Some(resource_name)) = (resource_type, resource_name) else {
        return BTreeMap::new();
    };
    REQUIRED_RESTORE_METADATA
        .iter()
        .filter(|(named, _, _)| *named == resource_type)
        .map(|(_, key, _)| ((*key).to_string(), resource_name.clone()))
        .collect()
}

/// Why the `Metadata` of a restore of `resource_type` from `recovery_point_arn` cannot start
/// one, or `None` when it carries everything that type requires.
///
/// The keys are read through the same case-insensitive step the restore itself reads them
/// with ([`super::metadata_value`]), so a request this accepts is one the restore can perform
/// and a request it refuses is one the restore could not.
#[must_use]
pub fn rejected_restore_metadata(
    resource_type: Option<&str>,
    recovery_point_arn: &str,
    metadata: &BTreeMap<String, String>,
) -> Option<InvalidParameterValueException> {
    let resource_type = resource_type?;
    let (_, key, sentence) = REQUIRED_RESTORE_METADATA
        .iter()
        .find(|(named, _, _)| *named == resource_type)?;
    if super::metadata_value(metadata, key).is_some() {
        return None;
    }
    Some(InvalidParameterValueException {
        message: Some(format!(
            "{MESSAGE_INVALID_RESTORE_METADATA} {sentence} Restore metadata validation failed \
             for {resource_type} with recoveryPoint {recovery_point_arn}."
        )),
        ..Default::default()
    })
}
