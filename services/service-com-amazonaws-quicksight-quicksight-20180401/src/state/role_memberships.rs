// SPDX-License-Identifier: CC0-1.0

//! WHICH groups hold one of the service's roles.
//!
//! A role membership joins one group of a namespace to one role, so it is keyed by that
//! role's own address with the group's name under it,
//! `(AwsAccountId, Namespace, Role, MemberName)` — the way a group membership is keyed by
//! the group it belongs to. There is nothing else to hold: the membership IS the fact
//! that the group holds the role, and a read of a role reports its members by name, so
//! the key is the whole record.
//!
//! The member a role membership names is a group, so the membership outlives neither the
//! group nor the namespace both live in: it goes when the groups of a namespace go.
//!
//! A role is held by a group the account manages itself, so an account whose
//! authentication method gives it no QuickSight identity store
//! ([`ComAmazonawsQuicksightQuicksight20180401State::manages_quicksight_users`]) holds no
//! role memberships at all, and a call addressing them is refused with the modelled
//! `AccessDeniedException` ([`super::authorization::access_denied`]) carrying the reason
//! [`super::refusals::role_membership_not_authorized_message`] words.

use super::*;

impl ComAmazonawsQuicksightQuicksight20180401State {
    /// Whether the role `(aws_account_id, namespace, role)` holds the group
    /// `member_name` — what a membership read and a membership delete answer from.
    pub fn has_role_member(
        &self,
        aws_account_id: &str,
        namespace: &str,
        role: &str,
        member_name: &str,
    ) -> bool {
        self.role_memberships
            .contains(&role_membership_key(aws_account_id, namespace, role, member_name))
    }

    /// The names of the groups the role `(aws_account_id, namespace, role)` holds,
    /// ordered by name — the order `ListRoleMemberships` pages through.
    pub fn role_members_in(
        &self,
        aws_account_id: &str,
        namespace: &str,
        role: &str,
    ) -> Vec<&String> {
        let mut members: Vec<&String> = self
            .role_memberships
            .iter()
            .filter(|(account, member_namespace, member_role, _)| {
                account == aws_account_id && member_namespace == namespace && member_role == role
            })
            .map(|(_, _, _, member_name)| member_name)
            .collect();
        members.sort();
        members
    }

    /// Removes the role memberships naming the group
    /// `(aws_account_id, namespace, member_name)`, across every role of that namespace,
    /// answering how many went.
    ///
    /// A role membership names a group, so it goes when that group does — whichever roles
    /// held it.
    pub fn remove_role_memberships_of(
        &mut self,
        aws_account_id: &str,
        namespace: &str,
        member_name: &str,
    ) -> usize {
        let before = self.role_memberships.len();
        self.role_memberships
            .retain(|(account, member_namespace, _, member)| {
                account != aws_account_id
                    || member_namespace != namespace
                    || member != member_name
            });
        before - self.role_memberships.len()
    }

    /// Removes the role memberships the groups of `(aws_account_id, namespace)` hold,
    /// answering how many went.
    ///
    /// A role membership names a group of the namespace, so it goes when that group does —
    /// and the groups of a namespace go with the namespace.
    pub fn remove_role_memberships_in(&mut self, aws_account_id: &str, namespace: &str) -> usize {
        let before = self.role_memberships.len();
        self.role_memberships
            .retain(|(account, member_namespace, _, _)| {
                account != aws_account_id || member_namespace != namespace
            });
        before - self.role_memberships.len()
    }
}

/// The key the role membership joining the group `member_name` to
/// `(aws_account_id, namespace, role)` is held under, so every handler addresses one
/// membership the same way.
pub fn role_membership_key(
    aws_account_id: &str,
    namespace: &str,
    role: &str,
    member_name: &str,
) -> (String, String, String, String) {
    (
        aws_account_id.to_string(),
        namespace.to_string(),
        role.to_string(),
        member_name.to_string(),
    )
}

/// The TOTAL number of role memberships this emulator holds, across every account,
/// namespace and role.
///
/// A role's own membership list needs no allowance of its own: its members are groups of
/// the namespace that the create has to find in the store, so one role can hold no more
/// members than its namespace holds groups ([`MAX_GROUPS_PER_NAMESPACE`]). What a request
/// can still multiply freely is the rest of the key — the account id, the namespace and
/// the role are all `@httpLabel`s the caller chooses — so this is the bound that holds
/// for the process, for the same reason [`MAX_GROUP_MEMBERS`] is.
pub const MAX_ROLE_MEMBERSHIPS: usize = 100_000;
