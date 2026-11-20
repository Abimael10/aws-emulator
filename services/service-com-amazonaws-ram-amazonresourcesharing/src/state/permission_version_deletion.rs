// SPDX-License-Identifier: CC0-1.0

//! How the delete of ONE version of a managed permission reaches the reads that report
//! it, and which versions a caller may delete at all.
//!
//! `DeletePermissionVersion` "deletes one version of a customer managed permission",
//! and "the version you specify must not be attached to any resource share and must not
//! be the default version for the permission" (the operation's own documentation). Like
//! the delete of a whole permission ([`super::permission_deletion`]) it "is performed
//! asynchronously" and reports a `PermissionStatus`, so the version leaves the
//! attachable statuses for [`super::PERMISSION_STATUS_DELETING`] when the call is
//! answered and reaches [`super::PERMISSION_STATUS_DELETED`] once the move the clock
//! owes falls due.
//!
//! The version's record SURVIVES in that terminal status, exactly as a deleted
//! permission's does, so a caller polling `GetPermission` at that version number sees
//! the delete complete. What the delete takes away is what a live version is for: it no
//! longer counts against the versions a permission may carry, so "if a customer managed
//! permission has the maximum of 5 versions, then you must delete at least one version
//! before you can create another" holds.

use super::{
    ComAmazonawsRamAmazonresourcesharingState, OwedMove, PermissionDeletion, PermissionRecord,
    PermissionVersionRecord, FIRST_PERMISSION_VERSION, PERMISSION_DELETED_AFTER_MILLIS,
    PERMISSION_STATUS_DELETED, PERMISSION_STATUS_DELETING,
};

impl PermissionVersionRecord {
    /// Whether `DeletePermissionVersion` has been called on this version — it is either
    /// being deleted or deleted, and in neither case is it one of the versions the
    /// permission still carries.
    pub fn is_deleting_or_deleted(&self) -> bool {
        self.deletion.is_some()
    }
}

impl PermissionRecord {
    /// Every version of this permission no delete has taken away, in version-number
    /// order.
    ///
    /// These are the versions the permission still carries: a version on the delete path
    /// is not one of them, which is what frees room under the version limit.
    pub fn live_versions(&self) -> impl Iterator<Item = &PermissionVersionRecord> {
        self.versions
            .values()
            .filter(|version| !version.is_deleting_or_deleted())
    }

    /// How many versions this permission still carries, which is what the limit on the
    /// versions one permission may hold counts
    /// ([`super::MAX_PERMISSION_VERSIONS`]).
    pub fn live_version_count(&self) -> usize {
        self.live_versions().count()
    }
}

impl ComAmazonawsRamAmazonresourcesharingState {
    /// Start the delete of `version` of the managed permission `arn` names and answer
    /// the `PermissionStatus` that version now reports, or `None` when this store holds
    /// no such permission or no such version of it.
    ///
    /// The version goes to [`PERMISSION_STATUS_DELETING`] at once and the clock owes
    /// [`PERMISSION_STATUS_DELETED`] [`PERMISSION_DELETED_AFTER_MILLIS`] later — the
    /// same span the delete of a whole permission takes, because it is the same owed
    /// move. A version already on the delete path is left exactly as it is: the call
    /// that started the delete is the one that decided when it completes, so a repeat
    /// neither restarts the move nor moves the stamps.
    pub fn delete_permission_version(
        &mut self,
        arn: &str,
        version: i32,
        now: i64,
    ) -> Option<String> {
        let permission = self.permissions.get_mut(arn)?;
        let record = permission.versions.get_mut(&version)?;
        if let Some(deletion) = &record.deletion {
            return Some(deletion.status.clone());
        }
        record.deletion = Some(PermissionDeletion {
            status: PERMISSION_STATUS_DELETING.to_string(),
            owed_move: Some(OwedMove {
                status: PERMISSION_STATUS_DELETED.to_string(),
                due_at: now + PERMISSION_DELETED_AFTER_MILLIS,
            }),
        });
        record.last_updated_time = now;
        // The permission itself was last changed by this call: one of its versions left
        // the versions it carries.
        permission.last_updated_time = now;
        Some(PERMISSION_STATUS_DELETING.to_string())
    }

    /// Make every version the move its own delete owed at or before `now`.
    ///
    /// Like [`Self::settle_due_associations`], this only materialises what was already
    /// due, and it is driven from [`Self::settle_due_permissions`] so every call that
    /// settles a permission settles its versions too.
    pub fn settle_due_permission_versions(&mut self, now: i64) {
        for permission in self.permissions.values_mut() {
            for version in permission.versions.values_mut() {
                let Some(deletion) = &mut version.deletion else {
                    continue;
                };
                let Some(owed) = deletion.owed_move.clone() else {
                    continue;
                };
                if owed.due_at > now {
                    continue;
                }
                deletion.status = owed.status;
                deletion.owed_move = None;
                // The move happened when it fell due, not when somebody noticed.
                version.last_updated_time = owed.due_at;
            }
        }
    }

    /// The ARNs of the live resource shares governed by `version` of the managed
    /// permission `permission_arn`, in share ARN order.
    ///
    /// "The version you specify must not be attached to any resource share"
    /// (`DeletePermissionVersion`'s own documentation): a share records the version of
    /// the permission it attached ([`super::SharePermissionRecord`]), so what stands in
    /// the way of this delete is a share holding THIS version — a share attached to
    /// another version of the same permission holds nothing back. A share
    /// `DeleteResourceShare` has removed is itself gone, so it holds nothing back
    /// either.
    pub fn shares_attaching_permission_version(
        &self,
        permission_arn: &str,
        version: i32,
    ) -> Vec<String> {
        self.resource_shares
            .values()
            .filter(|share| !share.is_deleted())
            .filter(|share| {
                share
                    .attached_permissions
                    .get(permission_arn)
                    .is_some_and(|attachment| attachment.version == version)
            })
            .map(|share| share.arn.clone())
            .collect()
    }
}

/// The message of the `InvalidParameterException` an operation renders when the version
/// number a request names is below the first version any permission carries.
///
/// Versions are numbered upward from [`FIRST_PERMISSION_VERSION`]
/// (`GetPermission$permissionVersion`, `ListPermissionVersions`), so a number below it
/// names no version any permission could carry — the parameter is not a version number
/// at all, which is a different refusal from naming a version that was never written.
pub fn permission_version_below_first_message(member: &str) -> String {
    format!("{member} must be {FIRST_PERMISSION_VERSION} or greater")
}

/// The message of the `OperationNotPermittedException` a version delete renders when
/// the version it names is the permission's DEFAULT version.
///
/// "The version you specify … must not be the default version for the permission", and
/// "if the version is the default, you must first use SetDefaultPermissionVersion to set
/// a different version as the default" (`DeletePermissionVersion` and its
/// `permissionVersion` member), so the message names the permission, the version that
/// was asked for and the operation that moves the default away from it.
pub fn default_permission_version_not_deletable_message(arn: &str, version: i32) -> String {
    format!(
        "Version {version} is the default version of permission {arn}. You can't delete the \
         default version for a customer managed permission. Use SetDefaultPermissionVersion to \
         set a different version as the default first."
    )
}

/// The message of the `OperationNotPermittedException` a version delete renders when
/// the version it names is still attached to a resource share.
///
/// "You can't delete a version if it is attached to any resource shares"
/// (`DeletePermissionVersion$permissionVersion`) is the fixed text of this refusal
/// class; the version the request named and one share governed by it are what the
/// message takes from the request and from this store.
pub fn permission_version_attached_to_share_message(
    arn: &str,
    version: i32,
    shares: &[String],
) -> String {
    let attached = shares.first().map(String::as_str).unwrap_or_default();
    format!(
        "Version {version} of permission {arn} is attached to resource share {attached}. You \
         can't delete a version if it is attached to any resource shares."
    )
}
