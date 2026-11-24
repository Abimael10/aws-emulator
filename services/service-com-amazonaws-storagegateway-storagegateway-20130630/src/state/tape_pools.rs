// SPDX-License-Identifier: CC0-1.0

//! The custom-tape-pool part of this service's state: the record
//! `CreateTapePool` mints, the `PoolInfo` projection `ListTapePools` reports,
//! and the lookup `DeleteTapePool` removes.
//!
//! A custom tape pool is an account-level resource — no member of
//! `CreateTapePoolInput` names a gateway — so a pool is keyed by its own ARN
//! alone, in the one map (`tape_pools`) every tape-pool handler reads and
//! writes. Every member the create carried is persisted here, so the reporting
//! operation computes its answer from the stored record and no handler invents a
//! value.

use std::time::SystemTime;

use super::{
    resource_not_found, ComAmazonawsStoragegatewayStoragegateway20130630State, ARN_NAMESPACE,
};
use crate::ops::types::{InvalidGatewayRequestException, PoolInfo, Tag};

/// The resource type a custom tape pool's ARN names, as Storage Gateway spells
/// it in a `PoolARN`.
pub const TAPE_POOL_RESOURCE: &str = "tapepool";

/// The status a stored custom tape pool carries. `PoolInfo$PoolStatus`
/// documents "Pool can be `ACTIVE` or `DELETED`"; a pool is usable the instant
/// `CreateTapePool` answers — the operation reports no asynchronous work — and a
/// deleted pool is removed from the store rather than retained, so `ACTIVE` is
/// the status of every pool this service reports.
pub const POOL_STATUS_ACTIVE: &str = "ACTIVE";

/// The retention lock type a pool records when the request names none:
/// `RetentionLockType` admits `COMPLIANCE`, `GOVERNANCE` and `NONE`, and `NONE`
/// is the value that says the pool locks no tape.
pub const RETENTION_LOCK_TYPE_NONE: &str = "NONE";

/// The retention lock time a pool records when the request names none:
/// `RetentionLockTimeInDays` is modelled with a minimum of 0 days, which is the
/// only value consistent with [`RETENTION_LOCK_TYPE_NONE`].
pub const RETENTION_LOCK_TIME_NONE: i32 = 0;

/// The ceiling on how many custom tape pools this store holds.
///
/// Like [`super::MAX_GATEWAYS`], this is the emulator's OWN bound, not a
/// published AWS default: a generated emulator keeps its resources in memory for
/// the life of the process and `CreateTapePool` is the only thing that adds one,
/// so the store needs a maximum entry count. `CreateTapePool` refuses past it
/// with this service's one modelled client error.
pub const MAX_TAPE_POOLS: usize = 100;

/// One custom tape pool, as this emulator persists it.
#[derive(Debug, Clone)]
pub struct TapePool {
    /// `arn:aws:storagegateway:<region>:<account>:tapepool/<pool_id>`.
    pub arn: String,
    /// The minted pool id the ARN ends in.
    pub pool_id: String,
    /// `PoolName`, len 1..100, as the create carried it.
    pub name: String,
    /// One of `TapeStorageClass`'s values.
    pub storage_class: String,
    /// One of `RetentionLockType`'s values.
    pub retention_lock_type: String,
    /// `RetentionLockTimeInDays`, 0..36500.
    pub retention_lock_time_in_days: i32,
    /// The tags the create carried. Reported by the tagging operations, not by
    /// `PoolInfo`, which carries no tag member.
    pub tags: Vec<Tag>,
    /// One of `PoolStatus`'s values.
    pub status: String,
    pub created_at: SystemTime,
}

impl TapePool {
    /// The `PoolInfo` projection `ListTapePools` reports.
    pub fn to_pool_info(&self) -> PoolInfo {
        PoolInfo {
            pool_arn: Some(self.arn.clone()),
            pool_name: Some(self.name.clone()),
            storage_class: Some(self.storage_class.clone()),
            retention_lock_type: Some(self.retention_lock_type.clone()),
            retention_lock_time_in_days: Some(self.retention_lock_time_in_days),
            pool_status: Some(self.status.clone()),
        }
    }
}

impl ComAmazonawsStoragegatewayStoragegateway20130630State {
    /// Mint an unused custom tape pool id: `pool-` and eight hex digits, the
    /// shape Storage Gateway pool ids take.
    ///
    /// Eight hex digits hold 32 bits, so the counter is masked to that width;
    /// the loop skips an id already taken, which is what makes the wrap safe.
    pub fn mint_tape_pool_id(&mut self) -> String {
        loop {
            self.tape_pool_id_seq = self.tape_pool_id_seq.wrapping_add(1);
            let id = format!("pool-{:08X}", self.tape_pool_id_seq & 0xFFFF_FFFF);
            if !self.tape_pools.values().any(|pool| pool.pool_id == id) {
                return id;
            }
        }
    }

    /// Forget a custom tape pool.
    pub fn remove_tape_pool(&mut self, pool_arn: &str) -> Option<TapePool> {
        self.tape_pools.remove(pool_arn)
    }
}

/// The ARN of the custom tape pool `pool_id` names, in `region`.
pub fn tape_pool_arn(region: &str, pool_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{TAPE_POOL_RESOURCE}/{pool_id}"),
    )
}

/// The `ErrorCode` a refusal names when a pool ARN names no custom tape pool.
///
/// The `ErrorCode` enum this service ships lists a `<Resource>NotFound` member
/// per resource kind (`GatewayNotFound`, `VolumeNotFound`, `SnapshotNotFound`,
/// `TapeCartridgeNotFound`, …) but no member for a custom tape pool, so this is
/// the code Storage Gateway answers with rather than one the shipped enum
/// carries — what the enum does establish is that a missing resource is told
/// apart by its own kind's code and never by the generic
/// [`super::ERROR_CODE_INVALID_PARAMETERS`].
pub const ERROR_CODE_POOL_NOT_FOUND: &str = "PoolNotFound";

/// The refusal for a pool ARN that names no custom tape pool.
pub fn tape_pool_not_found() -> InvalidGatewayRequestException {
    resource_not_found(ERROR_CODE_POOL_NOT_FOUND, "pool")
}
