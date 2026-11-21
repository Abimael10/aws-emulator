// SPDX-License-Identifier: CC0-1.0

//! The event notifications a backup vault is configured to publish.
//!
//! # The rule this module implements
//!
//! A vault's notification configuration is a STORED member of the vault, as its
//! resource policy is ([`super::BackupVault::access_policy`]): the call that
//! turns notifications on stores the topic and the events on the named vault,
//! the read reports them from that same stored vault, and the delete clears the
//! member — so the configuration a client attaches is the configuration the
//! vault carries, and nothing else can disagree with it.
//!
//! A vault carries at most ONE such configuration, so a later write replaces
//! the one before it rather than adding to it, and a vault carrying none has
//! none to report: the read refuses that vault, because there is no
//! configuration for it to answer with.

use super::ResourceNotFoundException;

/// The notification configuration a vault carries: the topic a vault's events
/// are published to, and which of its events are published at all.
///
/// Both members are kept exactly as the write stated them — the topic it named
/// and the events it listed, in that order — because neither has a part this
/// service fills in, so the read reports what was stored.
#[derive(Debug, Clone)]
pub struct VaultNotifications {
    /// "The Amazon Resource Name (ARN) that specifies the topic for a backup
    /// vault's events" (model doc for `PutBackupVaultNotificationsInput$SNSTopicArn`).
    pub sns_topic_arn: String,
    /// "An array of events that indicate the status of jobs to back up
    /// resources to the backup vault" (model doc for
    /// `PutBackupVaultNotificationsInput$BackupVaultEvents`), each one a token of
    /// the model's `BackupVaultEvent` enum — which the framework's boundary
    /// validator has already held every entry to by the time one is stored.
    pub events: Vec<String>,
}

/// The sentence AWS states for "the vault named carries no notification
/// configuration to report" — FIXED text: it names no part of the request, so
/// the vault asked about does not appear in it, and it states the class of the
/// refusal rather than anything about the account's vaults. Worded as the other
/// not-found conditions of this service are
/// ([`super::MESSAGE_BACKUP_JOB_NOT_FOUND`]). No `Code`: this condition has no
/// observed token.
pub const MESSAGE_VAULT_NOTIFICATIONS_NOT_FOUND: &str =
    "Failed reading notifications from database for Backup vault";

/// The modelled refusal for a notifications read of a vault that carries no
/// configuration — see [`MESSAGE_VAULT_NOTIFICATIONS_NOT_FOUND`].
pub fn vault_notifications_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(MESSAGE_VAULT_NOTIFICATIONS_NOT_FOUND.to_string()),
        ..Default::default()
    }
}
