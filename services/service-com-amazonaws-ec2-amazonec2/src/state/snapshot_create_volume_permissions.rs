// SPDX-License-Identifier: CC0-1.0

//! WHO MAY CREATE A VOLUME from one EBS snapshot of the account, beside the account that owns
//! it: the create volume permissions `ModifySnapshotAttribute` grants and revokes,
//! `DescribeSnapshotAttribute` reports and `ResetSnapshotAttribute` clears.
//!
//! "You may add or remove specified Amazon Web Services account IDs from a snapshot's list of
//! create volume permissions" (`ModifySnapshotAttribute`): a permission is a GRANT recorded
//! against a snapshot id, either to an account ("the account ID to modify for the snapshot",
//! `UserIds`) or to a group ("the group to modify for the snapshot", `GroupNames`, whose only
//! modelled member is [`CREATE_VOLUME_PERMISSION_GROUP_ALL`]) — so the store
//! ([`ComAmazonawsEc2Amazonec2State::snapshot_create_volume_permissions`]) keys one set of each
//! by that id, and the two sets are [`BTreeSet`]s: a grant is a membership, so granting the same
//! account twice leaves one permission and the order a read reports them in is stable.
//!
//! The OWNER's permission is never recorded here: "an Amazon Web Services account has implicit
//! create volume permissions for all snapshots it owns" (`DescribeSnapshots`), so it is a fact
//! of the snapshot's `OwnerId` and not of this store — which is why a snapshot nobody has shared
//! holds no entry at all, and why this store starts empty for a fresh account.
//!
//! Three readers ask it, and each asks exactly one of the questions below so they cannot
//! disagree about one grant: the attribute read reports the grants
//! ([`ComAmazonawsEc2Amazonec2State::snapshot_create_volume_permissions_of`]), the inventory's
//! `RestorableByUserIds` narrowing asks whether one account or every account may restore a
//! snapshot ([`ComAmazonawsEc2Amazonec2State::snapshot_is_restorable_by`] and
//! [`ComAmazonawsEc2Amazonec2State::snapshot_is_shared_with_all`]), and the reset — like a
//! snapshot LEAVING the inventory ([`ComAmazonawsEc2Amazonec2State::remove_snapshot`]) — drops
//! every grant at once
//! ([`ComAmazonawsEc2Amazonec2State::forget_snapshot_create_volume_permissions`]): a snapshot the
//! account no longer holds can share nothing, the same rule the fast snapshot restores of a
//! deleted snapshot are dropped under.
//!
//! A granted account id is caller-chosen, so every stored grant is charged against the byte
//! budget this crate's stores share ([`MAX_STORED_BYTES`], through
//! [`snapshot_create_volume_permission_bytes`]) and given back when the grant is revoked or
//! reset: without that charge a caller could grant one snapshot to new accounts without bound.

use std::collections::BTreeSet;

use crate::ops::types::CreateVolumePermission;

use super::{
    ComAmazonawsEc2Amazonec2State, EXECUTABLE_BY_ALL, STORED_RECORD_OVERHEAD_BYTES, SnapshotView,
    snapshot_is_public,
};

/// "The name of the group" a create volume permission names when it is granted to every
/// account rather than to one (`CreateVolumePermission$Group`, "the group to be added or
/// removed. The possible value is `all`"): the `PermissionGroup` enum's only member (the
/// generated catalog [`crate::ops::types::PERMISSION_GROUP_VALUES`]), which is the same term
/// `DescribeSnapshots` selects public snapshots with through its `RestorableByUserIds` member.
///
/// DEFINED as that one term ([`EXECUTABLE_BY_ALL`]), so the group a permission is granted to,
/// the group a read reports and the term that selects on it cannot drift apart.
pub const CREATE_VOLUME_PERMISSION_GROUP_ALL: &str = EXECUTABLE_BY_ALL;

/// "You can make up to 500 modifications to a snapshot in a single operation"
/// (`ModifySnapshotAttribute`): how many permissions one call may add or remove.
///
/// A published bound on the REQUEST rather than on what is stored, and the model declares no
/// `@length` on either list it arrives in, so a handler is the only place it can be enforced.
pub const MAX_SNAPSHOT_ATTRIBUTE_MODIFICATIONS: usize = 500;

/// The bytes one stored create volume permission is charged against
/// [`super::MAX_STORED_BYTES`]: the fixed per-entry overhead every stored record of this crate
/// costs ([`STORED_RECORD_OVERHEAD_BYTES`]) plus the caller-chosen grantee it names.
///
/// The ONE definition of that charge: a grant asks it before the write, the write charges
/// exactly it, and a revoke gives back exactly it, so the budget cannot drift from what is
/// stored.
pub fn snapshot_create_volume_permission_bytes(grantee: &str) -> usize {
    STORED_RECORD_OVERHEAD_BYTES.saturating_add(grantee.len())
}

/// The create volume permissions recorded against ONE snapshot: the accounts it has been shared
/// with and the groups it has been granted to.
#[derive(Debug, Clone, Default)]
pub struct SnapshotCreateVolumePermissions {
    /// "The account ID to modify for the snapshot" (`ModifySnapshotAttributeRequest$UserIds`),
    /// one entry per account that may create a volume from the snapshot.
    pub user_ids: BTreeSet<String>,
    /// "The group to modify for the snapshot"
    /// (`ModifySnapshotAttributeRequest$GroupNames`) — [`CREATE_VOLUME_PERMISSION_GROUP_ALL`]
    /// being the only member the model declares.
    pub groups: BTreeSet<String>,
}

impl SnapshotCreateVolumePermissions {
    /// Has every grant been revoked, so the snapshot is shared with nobody?
    fn is_empty(&self) -> bool {
        self.user_ids.is_empty() && self.groups.is_empty()
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// "The users and groups that have the permissions for creating volumes from the snapshot"
    /// (`DescribeSnapshotAttributeResult$CreateVolumePermissions`), for `snapshot` as the
    /// account `account` sees it.
    ///
    /// Two sources, one answer:
    ///
    /// * a snapshot of the PUBLISHED catalogue — one the calling account does not own — is one
    ///   every account may create a volume from ([`snapshot_is_public`]), and that permission is
    ///   the one AWS reports as a GROUP rather than as an account;
    /// * a snapshot of the account carries whatever `ModifySnapshotAttribute` has granted on it,
    ///   and nothing else: the owner's own permission is implicit ("an Amazon Web Services
    ///   account has implicit create volume permissions for all snapshots it owns") and so is
    ///   not one of the permissions this attribute lists.
    ///
    /// The group grants are reported before the account grants, each in the stable order its set
    /// holds, so two reads of one snapshot report one list.
    pub fn snapshot_create_volume_permissions_of(
        &self,
        snapshot: &SnapshotView,
        account: &str,
    ) -> Vec<CreateVolumePermission> {
        let recorded = snapshot
            .snapshot_id
            .as_deref()
            .and_then(|snapshot_id| self.snapshot_create_volume_permissions.get(snapshot_id));
        let mut groups: BTreeSet<&str> = recorded
            .map(|permissions| {
                permissions
                    .groups
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<&str>>()
            })
            .unwrap_or_default();
        if snapshot_is_public(snapshot, account) {
            groups.insert(CREATE_VOLUME_PERMISSION_GROUP_ALL);
        }
        let mut reported: Vec<CreateVolumePermission> = groups
            .into_iter()
            .map(|group| CreateVolumePermission {
                group: Some(group.to_string()),
                ..CreateVolumePermission::default()
            })
            .collect();
        if let Some(permissions) = recorded {
            reported.extend(permissions.user_ids.iter().map(|user_id| {
                CreateVolumePermission {
                    user_id: Some(user_id.clone()),
                    ..CreateVolumePermission::default()
                }
            }));
        }
        reported
    }

    /// Do ALL accounts hold create volume permissions for `snapshot` — is it PUBLIC?
    ///
    /// "`all` for public snapshots" (`DescribeSnapshots`, `RestorableByUserIds`): a snapshot of
    /// the published catalogue is public because it is published ([`snapshot_is_public`]), and a
    /// snapshot of the account is public once the `all` group has been granted create volume
    /// permissions on it. ONE test, so the inventory's narrowing and the permissions an
    /// attribute read reports cannot disagree about which snapshots are public.
    pub fn snapshot_is_shared_with_all(&self, snapshot: &SnapshotView, account: &str) -> bool {
        snapshot_is_public(snapshot, account)
            || self.snapshot_is_shared_with_group(
                snapshot.snapshot_id.as_deref().unwrap_or_default(),
                CREATE_VOLUME_PERMISSION_GROUP_ALL,
            )
    }

    /// Does `user_id` hold create volume permissions for `snapshot`?
    ///
    /// The owner's implicit permission ("an Amazon Web Services account has implicit create
    /// volume permissions for all snapshots it owns") or an explicit grant to that account —
    /// which is what `RestorableByUserIds` selects on for an account id: "only snapshots with
    /// create volume permissions for those users are returned".
    pub fn snapshot_is_restorable_by(&self, snapshot: &SnapshotView, user_id: &str) -> bool {
        if snapshot.owner_id.as_deref() == Some(user_id) {
            return true;
        }
        self.snapshot_create_volume_permissions
            .get(snapshot.snapshot_id.as_deref().unwrap_or_default())
            .is_some_and(|permissions| permissions.user_ids.contains(user_id))
    }

    /// Has `group` been granted create volume permissions on the snapshot `snapshot_id` names?
    pub fn snapshot_is_shared_with_group(&self, snapshot_id: &str, group: &str) -> bool {
        self.snapshot_create_volume_permissions
            .get(snapshot_id)
            .is_some_and(|permissions| permissions.groups.contains(group))
    }

    /// The bytes granting `user_ids` and `groups` on `snapshot_id` would ADD to the store: the
    /// charge of the grants that are not recorded yet, a grant that already stands costing
    /// nothing further.
    ///
    /// Asked by a handler BEFORE it writes, so a request the budget has no room for is refused
    /// rather than half-applied.
    pub fn snapshot_create_volume_permission_charge(
        &self,
        snapshot_id: &str,
        user_ids: &[String],
        groups: &[String],
    ) -> usize {
        let recorded = self.snapshot_create_volume_permissions.get(snapshot_id);
        let charge_of = |grantee: &String, held: Option<&BTreeSet<String>>| {
            if held.is_some_and(|held| held.contains(grantee)) {
                0
            } else {
                snapshot_create_volume_permission_bytes(grantee)
            }
        };
        let users = user_ids
            .iter()
            .map(|user_id| charge_of(user_id, recorded.map(|held| &held.user_ids)));
        let groups = groups
            .iter()
            .map(|group| charge_of(group, recorded.map(|held| &held.groups)));
        users
            .chain(groups)
            .fold(0_usize, |total, bytes| total.saturating_add(bytes))
    }

    /// Grant `user_ids` and `groups` create volume permissions on the snapshot `snapshot_id`
    /// names.
    ///
    /// The ONE writer of a grant, so every grant is charged against the byte budget exactly
    /// once: a grantee already recorded is left as it stands and charged nothing, which is what
    /// makes repeating a grant idempotent rather than a way to grow the store.
    pub fn grant_snapshot_create_volume_permissions(
        &mut self,
        snapshot_id: &str,
        user_ids: &[String],
        groups: &[String],
    ) {
        let charged = {
            let permissions = self
                .snapshot_create_volume_permissions
                .entry(snapshot_id.to_string())
                .or_default();
            let mut charged = 0_usize;
            for user_id in user_ids {
                if permissions.user_ids.insert(user_id.clone()) {
                    charged = charged
                        .saturating_add(snapshot_create_volume_permission_bytes(user_id));
                }
            }
            for group in groups {
                if permissions.groups.insert(group.clone()) {
                    charged =
                        charged.saturating_add(snapshot_create_volume_permission_bytes(group));
                }
            }
            charged
        };
        self.add_stored_bytes(charged);
    }

    /// Revoke the create volume permissions `user_ids` and `groups` name on the snapshot
    /// `snapshot_id` names.
    ///
    /// The mirror of [`Self::grant_snapshot_create_volume_permissions`]: the bytes each revoked
    /// grant was charged are given back, and a snapshot left with no grant at all drops its
    /// entry, so the store holds only the snapshots that are actually shared. A grant that was
    /// never made is nothing to revoke, which is what makes a repeated remove idempotent.
    pub fn revoke_snapshot_create_volume_permissions(
        &mut self,
        snapshot_id: &str,
        user_ids: &[String],
        groups: &[String],
    ) {
        let Some(permissions) = self.snapshot_create_volume_permissions.get_mut(snapshot_id) else {
            return;
        };
        let mut refund = 0_usize;
        for user_id in user_ids {
            if permissions.user_ids.remove(user_id) {
                refund = refund.saturating_add(snapshot_create_volume_permission_bytes(user_id));
            }
        }
        for group in groups {
            if permissions.groups.remove(group) {
                refund = refund.saturating_add(snapshot_create_volume_permission_bytes(group));
            }
        }
        if permissions.is_empty() {
            self.snapshot_create_volume_permissions.remove(snapshot_id);
        }
        self.release_stored_bytes(refund);
    }

    /// Clear every create volume permission recorded against the snapshot `snapshot_id` names,
    /// giving back the bytes they were charged, and answer whether there were any.
    ///
    /// Two callers, one rule — the snapshot is shared with nobody again:
    /// `ResetSnapshotAttribute` ("resets permission settings for the specified snapshot", which
    /// leaves the snapshot with the permissions it was created with, none at all, its owner's
    /// being implicit), and [`Self::remove_snapshot`], because a snapshot that has left the
    /// inventory can share nothing and must not pass its grants to a later snapshot minted under
    /// the same id.
    pub fn forget_snapshot_create_volume_permissions(&mut self, snapshot_id: &str) -> bool {
        let Some(permissions) = self.snapshot_create_volume_permissions.remove(snapshot_id) else {
            return false;
        };
        let refund = permissions
            .user_ids
            .iter()
            .chain(permissions.groups.iter())
            .map(|grantee| snapshot_create_volume_permission_bytes(grantee))
            .fold(0_usize, |total, bytes| total.saturating_add(bytes));
        self.release_stored_bytes(refund);
        true
    }
}
