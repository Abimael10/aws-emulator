// SPDX-License-Identifier: CC0-1.0

//! WHERE a user pool answers: the role the Region a request reached holds for
//! the pool the request names, and the sentence an operation that Region does
//! not serve is refused with.
//!
//! A user pool is a REGIONAL resource. It lives in the Region it was created in
//! — which holds its PRIMARY replica ([`UserPool::to_primary_replica`]) — and
//! reaches another Region only as a replica of itself, added there by
//! `CreateUserPoolReplica`. The replica's role decides what that Region serves:
//! "The primary replica supports all end user and administrator operations",
//! while "the secondary replica supports a limited set of end user and
//! administrator operations. Generally, only administrator operations that set
//! configurations specific to the replica, and only end-user operations that do
//! not create or change attributes of a user are supported"
//! (`UserPoolReplicaType.Role` in this service's model). An operation outside
//! that limited set is refused where it is not served:
//! `OperationNotEnabledException` "is thrown when an operation is not available
//! in the current region or for the current user pool configuration. This can
//! occur when attempting to perform operations that are not supported in
//! secondary replica regions."
//!
//! A Region the pool holds NO replica in holds no pool of that id at all, so a
//! request addressed there resolves no user pool and is refused as an absent one
//! ([`user_pool_not_found_message`]) — the same answer every other
//! Region-selected view of this service gives for a pool it does not hold there
//! ([`ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::pool_tags`]).
//!
//! A child module of [`super`] rather than more of `state.rs`: which Region
//! answers for a pool is one concern, read by every operation that is served in
//! some Regions and not in others.

use super::*;

/// The sentence an operation the Region a request reached does not serve is
/// refused with: the whole message is fixed text for this error class, naming
/// neither the operation, the Region nor the pool.
pub const OPERATION_NOT_ENABLED_IN_REGION_MESSAGE: &str =
    "This operation is not enabled in this region for this User Pool";

/// What the Region a request was addressed to is to the user pool the request
/// names — the question every operation that is served in some of a pool's
/// Regions and not in others asks of the pool it resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolRegionRole {
    /// The pool's own Region, which holds its PRIMARY replica and "supports all
    /// end user and administrator operations".
    Primary,
    /// A Region holding a SECONDARY replica of the pool, which "supports a
    /// limited set of end user and administrator operations".
    SecondaryReplica,
    /// A Region the pool holds no replica in, so that Region holds no pool of
    /// this id at all.
    NoReplica,
}

impl UserPool {
    /// What `region` is to this pool as of `now`: the Region it lives in, a
    /// Region holding one of its secondary replicas, or a Region it has not been
    /// replicated to.
    ///
    /// A pure read: a secondary replica whose removal the clock has already made
    /// due is gone, which is computed here ([`UserPool::live_replica`]) rather
    /// than written, so asking this question changes nothing a later caller
    /// sees.
    #[must_use]
    pub fn region_role(&self, region: &str, now: SystemTime) -> PoolRegionRole {
        if region == self.region {
            PoolRegionRole::Primary
        } else if self.live_replica(region, now).is_some() {
            PoolRegionRole::SecondaryReplica
        } else {
            PoolRegionRole::NoReplica
        }
    }
}
