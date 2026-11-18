// SPDX-License-Identifier: CC0-1.0

//! The users this service deleted, and what a later read of one reports.
//!
//! QuickSight's identity path tells a user it once held apart from a name that never
//! named a user: a read of a deleted user states that the user does not exist, where a
//! read of a name with no record at all reports only the address the call asked about.
//! Making that distinction needs one thing the stored users cannot carry — that a user
//! was once there — so the deletions are remembered here, and every user handler asks
//! this module which of the two templates in [`super::refusals`] its miss renders.
//!
//! What is remembered is the ADDRESS of the deleted user, never the record: nothing a
//! registration stored outlives the deletion, so no later read can report a value of a
//! user that is gone.

use std::collections::VecDeque;

use super::*;

/// How a user is addressed: `(AwsAccountId, Namespace, UserName)`, the key the stored
/// users are held under.
type UserKey = (String, String, String);

/// How many deletions are remembered at once.
///
/// The memory is bounded for the same reason the store itself is ([`MAX_USERS`]): every
/// part of a user's address comes from the request, so a caller that registers and
/// deletes in a loop would otherwise grow it without end. It holds the same number of
/// addresses as there are users the store would hold, and the oldest deletion is the one
/// forgotten first — a read of a user forgotten this way reports the miss of a name with
/// no record, which is all the service still knows about it.
const MAX_REMEMBERED_USER_DELETIONS: usize = MAX_USERS;

/// The deletions QuickSight's identity path remembers: which addresses held a user that
/// this service has since removed.
///
/// A name that holds a user again is found by the read before this memory is consulted,
/// so a remembered address is only ever read for a name that holds no user now.
#[derive(Debug, Default)]
pub struct RememberedUserDeletions {
    /// The remembered addresses, for the membership test a miss makes.
    deleted: HashSet<UserKey>,
    /// The same addresses in the order they were remembered, so the oldest is the one
    /// dropped when the memory is full.
    order: VecDeque<UserKey>,
}

impl RememberedUserDeletions {
    /// Remembers that the user at `key` was deleted, forgetting the oldest deletion once
    /// the memory holds [`MAX_REMEMBERED_USER_DELETIONS`] of them.
    pub fn remember(&mut self, key: UserKey) {
        if !self.deleted.insert(key.clone()) {
            // Already remembered: a second deletion of one address is one deletion.
            return;
        }
        self.order.push_back(key);
        while self.order.len() > MAX_REMEMBERED_USER_DELETIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.deleted.remove(&oldest);
            }
        }
    }

    /// Whether a user this service deleted was held at `(aws_account_id, namespace,
    /// user_name)`.
    pub fn contains(&self, aws_account_id: &str, namespace: &str, user_name: &str) -> bool {
        self.deleted.contains(&(
            aws_account_id.to_string(),
            namespace.to_string(),
            user_name.to_string(),
        ))
    }
}

impl ComAmazonawsQuicksightQuicksight20180401State {
    /// Removes the user `(aws_account_id, namespace)` holds under `user_name`, answering
    /// the record that went, or `None` when the namespace held no user under that name.
    ///
    /// A removal here is what the identity path's deletions go through, so the deletion
    /// is remembered — and the groups this user was a member of lose that member — by
    /// the one code path that performs it.
    pub fn remove_user(
        &mut self,
        aws_account_id: &str,
        namespace: &str,
        user_name: &str,
    ) -> Option<UserRecord> {
        let key = (
            aws_account_id.to_string(),
            namespace.to_string(),
            user_name.to_string(),
        );
        let record = self.users.remove(&key)?;
        self.deleted_users.remember(key);
        // A group's members are users, so a user that is gone is a member of nothing.
        self.remove_memberships_of(aws_account_id, namespace, user_name);
        Some(record)
    }

    /// The message a user operation reports a `UserName` it holds no user under with:
    /// the deleted-user text for an address this service removed a user from, and the
    /// address alone for a name it holds no record of.
    pub fn user_miss_message(
        &self,
        aws_account_id: &str,
        namespace: &str,
        user_name: &str,
    ) -> String {
        if self
            .deleted_users
            .contains(aws_account_id, namespace, user_name)
        {
            return deleted_user_not_found_message(aws_account_id, namespace, user_name);
        }
        user_not_found_message(aws_account_id, namespace, user_name)
    }
}
