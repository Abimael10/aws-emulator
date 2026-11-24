// SPDX-License-Identifier: CC0-1.0

//! The allocation of a user pool identity while it is still IN PROGRESS: the window in
//! which a create has claimed an `(AuthenticationType, UserName)` identity and the pool
//! does not hold the user yet.
//!
//! Claiming an identity is not instantaneous. The create that claims one holds the claim
//! while the record is written, and only once it is written does the pool report that user
//! and does a further create of the same username claim a name that is TAKEN. A create of
//! the SAME user arriving while that allocation is still running has nothing to conflict
//! with — the identity it asks for is the one being allocated, and the user it asks for is
//! the one being written — so it JOINS that allocation and is answered the same empty
//! result, leaving the identity allocated exactly once. A create asking for a DIFFERENT
//! user under that identity does conflict, whenever it arrives.
//!
//! The window stays open for [`USER_ALLOCATION_SPAN`] after the last create that joined it,
//! so a burst of identical creates is answered as the one allocation however the host
//! schedules them, and never longer than [`USER_ALLOCATION_LIMIT`], so no caller waits on a
//! burst that does not end. Nothing polls for the span: each create sleeps to the instant
//! the allocation it opened or joined is due to settle, and whichever of them gets there
//! first writes the record — the others find it already written.

use ::std::time::{Duration, Instant};

use super::{ComAmazonawsAppstreamPhotonadminproxyserviceState, UserRecord, MAX_RECORDS_PER_STORE};

/// The identity a user pool user is allocated under — the key
/// [`ComAmazonawsAppstreamPhotonadminproxyserviceState::users`] stores a record by:
/// `(AuthenticationType, UserName)`.
pub type UserIdentity = (String, String);

/// The identity the given authentication type and user name name.
///
/// Email addresses are case-sensitive, so the name is taken exactly as the request spells
/// it: two spellings differing only in capitalisation are two identities.
pub fn user_identity(authentication_type: &str, user_name: &str) -> UserIdentity {
    (authentication_type.to_string(), user_name.to_string())
}

/// How long an allocation of a user pool identity stays in progress once no further
/// identical create has joined it.
///
/// AWS publishes no duration for writing a user pool user, so the span is the emulator's
/// own: long enough that creates of one user issued together are all answered by the one
/// allocation whatever order the host runs them in, and short enough that a caller creating
/// a user nobody else is creating is not kept waiting.
pub const USER_ALLOCATION_SPAN: Duration = Duration::from_millis(150);

/// The longest an allocation may stay in progress, however many identical creates keep
/// joining it.
///
/// The ceiling on [`USER_ALLOCATION_SPAN`]'s extension: without it a caller could hold an
/// identity — and its own create's answer — open indefinitely by issuing identical creates
/// back to back.
pub const USER_ALLOCATION_LIMIT: Duration = Duration::from_secs(3);

/// An allocation of a user pool identity that is still in progress: the record it writes
/// when it settles, and when that is due.
#[derive(Debug, Clone)]
pub struct PendingUserAllocation {
    /// The user this allocation writes into the pool, as the create that opened it asked
    /// for. What a later create is compared against: a create asking for this same user
    /// joins the allocation, one asking for another user conflicts with it.
    pub record: UserRecord,
    /// When the pool is due to hold the record — [`USER_ALLOCATION_SPAN`] after the last
    /// create that opened or joined this allocation, and never past
    /// [`Self::settles_by`].
    pub settles_at: Instant,
    /// The instant this allocation settles whatever else joins it
    /// ([`USER_ALLOCATION_LIMIT`] after it was opened).
    pub settles_by: Instant,
}

impl PendingUserAllocation {
    /// Whether `record` asks for the user this allocation is already writing.
    ///
    /// The identity is the key both are stored under, so what is compared is what a create
    /// asks for BESIDE it: the user's names. Everything else on the record is the service's
    /// own (the ARN it mints, the instant it stamps, the sign-in flag and status a created
    /// user starts with), so it is the same for both by construction.
    fn allocates_the_same_user(&self, record: &UserRecord) -> bool {
        self.record.first_name == record.first_name && self.record.last_name == record.last_name
    }
}

/// What a create asking to allocate an identity is answered by the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAllocation {
    /// The identity is being allocated for exactly the user this create asked for, by this
    /// create or by an identical one it joined: the create is answered the empty success
    /// once that allocation settles.
    InProgress,
    /// The username is taken: the pool holds a user under this identity, or an allocation
    /// of it for a different user is in progress.
    Taken,
    /// The pool stands at the emulator's record ceiling, so there is no room for a new
    /// identity.
    PoolFull,
}

impl ComAmazonawsAppstreamPhotonadminproxyserviceState {
    /// Claim the identity `record` names for that record, reporting what the pool answers.
    ///
    /// The lookup and the claim are ONE step on the locked state, so however many creates
    /// arrive together the identity is claimed at most once: the first of them opens the
    /// allocation, an identical one joins it (and holds its window open for another
    /// [`USER_ALLOCATION_SPAN`]), and one naming a different user is told the username is
    /// taken.
    pub fn start_user_allocation(&mut self, record: UserRecord, now: Instant) -> UserAllocation {
        let identity = user_identity(&record.authentication_type, &record.user_name);

        // An allocation whose window has run out is one the clock already owes the pool:
        // settle it here so the identity is answered from the pool's contents rather than
        // from a claim nothing is still holding.
        if self
            .pending_users
            .get(&identity)
            .is_some_and(|pending| now >= pending.settles_by)
        {
            self.settle_user_allocation(&identity);
        }

        if self.users.contains_key(&identity) {
            return UserAllocation::Taken;
        }
        if let Some(pending) = self.pending_users.get_mut(&identity) {
            if !pending.allocates_the_same_user(&record) {
                return UserAllocation::Taken;
            }
            pending.settles_at = (now + USER_ALLOCATION_SPAN).min(pending.settles_by);
            return UserAllocation::InProgress;
        }

        // The store must not grow per request without a ceiling, and an identity being
        // allocated is a record the pool is about to hold, so it counts against it.
        if self.users.len() + self.pending_users.len() >= MAX_RECORDS_PER_STORE {
            return UserAllocation::PoolFull;
        }
        self.pending_users.insert(
            identity,
            PendingUserAllocation {
                record,
                settles_at: now + USER_ALLOCATION_SPAN,
                settles_by: now + USER_ALLOCATION_LIMIT,
            },
        );
        UserAllocation::InProgress
    }

    /// When the allocation of `identity` is due to settle; `None` once it has, which is
    /// when the pool holds the user.
    pub fn user_allocation_due(&self, identity: &UserIdentity) -> Option<Instant> {
        self.pending_users
            .get(identity)
            .map(|pending| pending.settles_at)
    }

    /// Write the allocation of `identity` into the pool, if it is still in progress.
    ///
    /// Idempotent, because every create that opened or joined the allocation settles it:
    /// the first of them writes the record and the rest find nothing left to write. The
    /// record already in the pool is never overwritten, so settling cannot undo a user's
    /// later history.
    pub fn settle_user_allocation(&mut self, identity: &UserIdentity) {
        if let Some(pending) = self.pending_users.remove(identity) {
            self.users.entry(identity.clone()).or_insert(pending.record);
        }
    }
}
