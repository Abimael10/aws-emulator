// SPDX-License-Identifier: CC0-1.0

//! The group memberships QuickSight's identity path holds: which user is a member of
//! which group.
//!
//! A membership is a child of one group and names one user of the same namespace — the
//! model states that a group's members must be users and that groups can't be members of
//! another group — so it is keyed by the group's own key with the member's name under it,
//! `(AwsAccountId, Namespace, GroupName, MemberName)`, the way a refresh schedule is
//! keyed by the data set it belongs to.
//!
//! A membership outlives neither of the two resources it joins: it goes when the group
//! holding it goes, and when the user it names is deleted.

use super::*;

impl ComAmazonawsQuicksightQuicksight20180401State {
    /// The membership `(aws_account_id, namespace, group_name)` holds under
    /// `member_name`, or `None` when that group holds no such member — the record every
    /// membership read answers from.
    pub fn group_member(
        &self,
        aws_account_id: &str,
        namespace: &str,
        group_name: &str,
        member_name: &str,
    ) -> Option<&GroupMemberRecord> {
        self.group_members.get(&(
            aws_account_id.to_string(),
            namespace.to_string(),
            group_name.to_string(),
            member_name.to_string(),
        ))
    }

    /// How many members `(aws_account_id, namespace, group_name)` holds — what
    /// [`MAX_GROUP_MEMBERS_PER_GROUP`] is checked against.
    pub fn group_member_count(
        &self,
        aws_account_id: &str,
        namespace: &str,
        group_name: &str,
    ) -> usize {
        self.group_members
            .keys()
            .filter(|(account, member_namespace, member_group, _)| {
                account == aws_account_id
                    && member_namespace == namespace
                    && member_group == group_name
            })
            .count()
    }

    /// Removes the memberships the groups of `(aws_account_id, namespace)` hold,
    /// answering how many went.
    ///
    /// A membership is held inside one group, so it goes when that group does — and the
    /// groups of a namespace go with the namespace.
    pub fn remove_group_members_in(&mut self, aws_account_id: &str, namespace: &str) -> usize {
        let before = self.group_members.len();
        self.group_members
            .retain(|(account, member_namespace, _, _), _| {
                account != aws_account_id || member_namespace != namespace
            });
        before - self.group_members.len()
    }

    /// Removes the memberships the group `(aws_account_id, namespace, group_name)`
    /// holds, answering how many went: a membership is held inside one group, so it goes
    /// when that group does.
    pub fn remove_group_members_of(
        &mut self,
        aws_account_id: &str,
        namespace: &str,
        group_name: &str,
    ) -> usize {
        let before = self.group_members.len();
        self.group_members
            .retain(|(account, member_namespace, member_group, _), _| {
                account != aws_account_id
                    || member_namespace != namespace
                    || member_group != group_name
            });
        before - self.group_members.len()
    }

    /// Removes the memberships naming the user `(aws_account_id, namespace, user_name)`,
    /// answering how many went: a group's members are users, so a user that no longer
    /// exists is a member of nothing.
    pub fn remove_memberships_of(
        &mut self,
        aws_account_id: &str,
        namespace: &str,
        user_name: &str,
    ) -> usize {
        let before = self.group_members.len();
        self.group_members
            .retain(|(account, member_namespace, _, member_name), _| {
                account != aws_account_id
                    || member_namespace != namespace
                    || member_name != user_name
            });
        before - self.group_members.len()
    }
}

/// A stored group membership: what `CreateGroupMembership` persisted, which is what every
/// later membership read reports.
#[derive(Debug, Clone)]
pub struct GroupMemberRecord {
    /// The name of the user this membership names, which identifies the membership
    /// inside its group.
    pub member_name: String,
    /// That user's own ARN: a group member IS a user, so the ARN a membership reports is
    /// the one the stored user carries rather than an address of the membership itself.
    pub arn: String,
}

impl GroupMemberRecord {
    /// This membership as the modelled `GroupMember` structure every membership read
    /// reports.
    pub fn to_group_member(&self) -> GroupMember {
        GroupMember {
            arn: Some(self.arn.clone()),
            member_name: Some(self.member_name.clone()),
        }
    }
}

/// How many members one group may hold.
///
/// A group's members are users of the group's own namespace, so a group can hold no more
/// members than its namespace holds users: this is that allowance
/// ([`MAX_USERS_PER_NAMESPACE`]), stated here as the ceiling the membership create checks
/// rather than left to be inferred from another store's bound.
pub const MAX_GROUP_MEMBERS_PER_GROUP: usize = MAX_USERS_PER_NAMESPACE;

/// The TOTAL number of memberships this emulator holds, across every account, namespace
/// and group.
///
/// This is the bound that holds for the process, for the same reason [`MAX_GROUPS`] and
/// [`MAX_USERS`] are: every part of a membership's key comes from the request — all four
/// parts are `@httpLabel`s the caller chooses — and one group's allowance alone would be
/// reset by naming another group, so the count of groups times the count of members is
/// what a process bound has to cap.
pub const MAX_GROUP_MEMBERS: usize = 100_000;
