// SPDX-License-Identifier: CC0-1.0

//! The SIZE of a protected resource, as the service that owns it meters it — the figure a
//! backup of that resource reports as the size of the backup.
//!
//! # The rule this module implements
//!
//! `BackupJob$BackupSizeInBytes` is "the size, in bytes, of a backup" and
//! `RecoveryPointByBackupVault$BackupSizeInBytes` "the size, in bytes, of a backup", which is
//! the size of the PROTECTED RESOURCE the backup was taken of. Only the service that owns the
//! resource meters that size, so the figure is read FROM that service when the backup is taken
//! — over the emulator's own in-process transport
//! ([`smithy_server_runtime::EmbeddedEmulator`]), the hop the backup already asks whether the
//! resource exists through ([`super::protected_resource_answer`]) — and stored on the recovery
//! point, never recomputed: a point describes the resource as it WAS.
//!
//! It is NOT a count of the bytes the capture copied ([`super::captured_contents`], which
//! measures what this emulator holds so it can bound its own store). The size a backup reports
//! is the owning service's own metered figure, which that service refreshes on its own
//! schedule: for an Amazon DynamoDB table it is `TableDescription$TableSizeBytes`, and
//! "DynamoDB updates this value approximately every six hours. Recent changes might not be
//! reflected in this value" (that member's doc) — so a table written to an instant before the
//! backup is metered at whatever its own service still reports for it, which for a table
//! written in the same minute is nothing.
//!
//! One arm per resource type whose owning service this emulator can ask; a type with no arm
//! here, and a hop that answers nothing, meter NOTHING rather than a size nobody reported.

use smithy_server_runtime::EmbeddedEmulator;

/// The size the service that owns `resource_arn` meters for it, for the recovery point a
/// backup of it is about to store.
///
/// Asked with NO store guard held: it makes at most one in-process call and opens no socket.
/// `0` is "nothing was metered" — see the module docs.
pub async fn metered_resource_size(
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
) -> i64 {
    match super::resource_type_of(resource_arn).as_deref() {
        Some(super::RESOURCE_TYPE_DYNAMODB) => {
            metered_dynamodb_table_size(callback, region, resource_arn).await
        }
        // A type whose owning service this emulator cannot ask meters nothing.
        _ => 0,
    }
}

/// The Amazon DynamoDB arm: the size that service reports for the table the ARN names.
///
/// The client and the table-name reading are the ones every other direction of a question
/// about one table uses ([`super::dynamodb_client`], [`super::table_name_of`]). A description
/// this emulator could not get, and a table whose own service reports no size, meter nothing.
async fn metered_dynamodb_table_size(
    callback: &EmbeddedEmulator,
    region: &str,
    resource_arn: &str,
) -> i64 {
    let Some(table_name) = super::table_name_of(resource_arn) else {
        return 0;
    };
    super::dynamodb_client(callback, region)
        .describe_table()
        .table_name(table_name)
        .send()
        .await
        .ok()
        .and_then(|described| described.table)
        .and_then(|table| table.table_size_bytes)
        .unwrap_or_default()
        .max(0)
}
