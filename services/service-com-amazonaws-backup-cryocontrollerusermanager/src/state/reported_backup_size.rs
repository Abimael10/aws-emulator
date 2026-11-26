// SPDX-License-Identifier: CC0-1.0

//! The SIZE a backup reports — what `BackupSizeInBytes` carries on a job and on the recovery
//! point it wrote, and what a settled job reports having transferred — decided by the TYPE of
//! the protected resource the backup was taken of.
//!
//! # The rule this module implements
//!
//! `BackupJob$BackupSizeInBytes` is "the size, in bytes, of a backup (recovery point)", and
//! the member's own documentation states what decides it: the value "can render differently
//! depending on the resource type as Backup pulls in data information from other Amazon Web
//! Services services. For example, the value returned may show a value of `0`, which may
//! differ from the anticipated value". So the figure is one OTHER services report to Backup
//! for a resource of that type, selected by the Backup resource type of the protected
//! resource ([`super::resource_type_of`]) — never a measurement of what this emulator happens
//! to hold, and never a figure derived from the request.
//!
//! The enumeration that documentation then gives, type by type:
//!
//! * Amazon Aurora, Amazon DocumentDB and Amazon Neptune "do not have this value populate";
//! * "An Amazon RDS instance will show as `0`";
//! * "Amazon FSx does not populate this value";
//! * "For Amazon DynamoDB with advanced features, this value refers to the size of the
//!   recovery point (backup)" — with those features ONLY. They are full Backup management of
//!   DynamoDB for the Region, which a client turns on through
//!   `UpdateRegionSettings$ResourceTypeManagementPreference` ("to enable full Backup
//!   management for DynamoDB along with Backup's advanced DynamoDB backup features", that
//!   member's doc); this emulator serves no operation that turns them on, so every DynamoDB
//!   table it protects is a PLAIN one, which no size is reported for;
//! * Amazon EC2 and Amazon EBS "show volume size (provisioned storage)", Amazon EFS "the
//!   delta bytes transferred during a backup", Amazon EKS "the size of your nested EKS
//!   recovery point", and a VMware virtual machine's figure reaches Backup "through an
//!   asynchronous workflow" — each a quantity only the service that OWNS the resource holds,
//!   so for those types the figure is read from that service
//!   ([`super::metered_resource_size`]), and a service this emulator cannot ask reports
//!   nothing rather than a size nobody answered — the `0` the same documentation says a
//!   reader may be served.
//!
//! The figure is settled WHEN THE BACKUP IS TAKEN and stored on the recovery point and on the
//! job that wrote it, so every read of that backup — the job, the point, the vault's listing,
//! a copy of it, a restore from it — reports the one stored value and no read recomputes it.
//!
//! What this emulator counts over a capture is a different quantity with a different purpose:
//! it bounds this store ([`super::captured_contents`]) and is reported to no caller, since an
//! internal storage representation is not a number an AWS client could predict.

use smithy_server_runtime::EmbeddedEmulator;

/// The figure a backup carries when no service reports Backup a size for it.
///
/// Named because it is the rule's own answer — "the value returned may show a value of `0`"
/// (the module docs) — rather than a missing one: the member is a `Long` every one of these
/// responses carries.
const NO_SIZE_REPORTED: i64 = 0;

/// The Backup resource types a backup of which carries no size at all, as the enumeration in
/// the module docs reads for each.
///
/// SOURCE: the model's documentation for `BackupJob$BackupSizeInBytes`
/// (`smithy_query(query="doc", shape="BackupJob", member="BackupSizeInBytes",
/// model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`),
/// spelled as this service spells its resource types ([`super::resource_type_of`]).
///
/// A type this list does not name is one whose figure its own service holds — see the module
/// docs and [`reported_backup_size`].
const RESOURCE_TYPES_REPORTING_NO_SIZE: &[&str] = &[
    "Aurora",
    "DocumentDB",
    // Reported only "with advanced features", which nothing here turns on.
    "DynamoDB",
    "FSx",
    "Neptune",
    "RDS",
];

/// The size a backup of `resource_arn` reports, for the recovery point the backup is about to
/// store and for the job that writes it.
///
/// The ONE place this service decides that figure, so a job, the point it wrote and every
/// listing of either can never disagree about it.
///
/// Asked with NO store guard held: for a type whose figure its owning service holds it makes
/// at most one in-process call and opens no socket; for every other type it asks nothing,
/// since the rule fixes the figure without the resource.
pub async fn reported_backup_size(
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
) -> i64 {
    let resource_type = super::resource_type_of(resource_arn);
    // A resource whose ARN resolved to no Backup resource type is one no owning service was
    // identified for, so nothing reports a size for it either.
    let reported_by_owning_service = resource_type.as_deref().is_some_and(|resource_type| {
        !RESOURCE_TYPES_REPORTING_NO_SIZE.contains(&resource_type)
    });
    if !reported_by_owning_service {
        return NO_SIZE_REPORTED;
    }
    super::metered_resource_size(callback, region, resource_arn).await
}
