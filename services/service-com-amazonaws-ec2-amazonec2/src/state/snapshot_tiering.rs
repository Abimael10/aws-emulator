// SPDX-License-Identifier: CC0-1.0

//! The STORAGE TIER of one EBS snapshot: the archive or restore action last asked for it, and
//! the tier that action leaves the snapshot in.
//!
//! "Archives an Amazon EBS snapshot […] moved from the standard tier to the archive tier"
//! (`ModifySnapshotTier`): a snapshot's tier is not a member a caller sets, it is the outcome
//! of the last tiering action asked for it, and that action takes TIME — "the state of the
//! last archive or restore action"
//! (`DescribeSnapshotTierStatusRequest$Filters`) is `archival-in-progress` while it runs and
//! `archival-completed` once it is done. So this module stores the ACTION
//! ([`SnapshotTieringRecord`]) and DERIVES from it every member that describes it: the status
//! word, the progress, the completion instant and the tier the snapshot is in. The move from
//! in-progress to completed is owed by the CLOCK, recorded as a [`ClockTransition`] rather
//! than slept for, exactly as a `pending` snapshot's `completed` move is — so a read observes
//! it without writing anything and two readers cannot disagree about one snapshot's tier.
//!
//! ONE record per snapshot, keyed by `SnapshotId` in
//! [`ComAmazonawsEc2Amazonec2State::snapshot_tiering`], so the tier a snapshot is reported in
//! has one source whichever operation reports it: the inventory read projects it through
//! [`ComAmazonawsEc2Amazonec2State::snapshot_in_tier`] and the tier status read reports the
//! action itself. A snapshot NOTHING has ever archived has no record here, which is why a
//! fresh account's `DescribeSnapshotTierStatus` reports nothing: there is no archive or
//! restore action to describe.
//!
//! BOUNDS: an entry is only ever keyed by a snapshot id the inventory RESOLVED, so the map
//! holds at most one entry per snapshot the account holds — a count both this store's own
//! ceiling ([`super::MAX_STORED_SNAPSHOTS`]) and the EBS direct APIs' bound. The record itself
//! is three instants and two catalog words, so it retains nothing a caller chose.
//!
//! `ModifySnapshotTier` is the writer ([`ComAmazonawsEc2Amazonec2State::begin_snapshot_archival`]),
//! and a snapshot that leaves the inventory takes its record with it
//! ([`ComAmazonawsEc2Amazonec2State::forget_snapshot_tiering`], reached by every removal of a
//! snapshot): a snapshot the account no longer holds has no tier to report, the same rule its
//! fast snapshot restores and its create volume permissions go by.

use std::time::{Instant, SystemTime};

use super::{
    ClockTransition, ComAmazonawsEc2Amazonec2State, SNAPSHOT_STORAGE_TIER_STANDARD, SnapshotView,
    TRANSITION_DELAY,
};

/// "`archive` indicates that the snapshot is currently archived and that it must be restored
/// before it can be used" (`Snapshot$StorageTier`): the tier an archival moves a snapshot
/// into, `standard` ([`SNAPSHOT_STORAGE_TIER_STANDARD`]) being the one every snapshot is
/// created in. A member of the modelled `StorageTier` catalog
/// (`smithy_query(query="shape",shape="StorageTier")`).
pub const SNAPSHOT_STORAGE_TIER_ARCHIVE: &str = "archive";

/// The `TieringOperationStatus` member an archival that is still running reports:
/// `archival-in-progress` (`smithy_query(query="shape",shape="TieringOperationStatus")`),
/// which is also one of the words the `last-tiering-operation` filter selects on.
pub const TIERING_OPERATION_ARCHIVAL_IN_PROGRESS: &str = "archival-in-progress";

/// The `TieringOperationStatus` member an archival that has finished reports:
/// `archival-completed` — what the clock owes every archival this store starts.
pub const TIERING_OPERATION_ARCHIVAL_COMPLETED: &str = "archival-completed";

/// "The progress of the last archive or restore process, as a percentage"
/// (`SnapshotTierStatus$LastTieringProgress`) for an action that has not transferred anything
/// yet: none of it is accounted for, which as a percentage is zero.
pub const TIERING_PROGRESS_IN_PROGRESS: i32 = 0;

/// `SnapshotTierStatus$LastTieringProgress` for an action that has finished: all of the
/// snapshot's data has been moved, which as a percentage is 100 — the same reading a
/// `completed` snapshot's own `Progress` takes
/// ([`super::SNAPSHOT_PROGRESS_COMPLETED`]).
pub const TIERING_PROGRESS_COMPLETED: i32 = 100;

/// The archive or restore action last asked for one snapshot, and the move the clock owes it.
///
/// Stored instead of the members that describe it, for the reason the snapshot store gives for
/// the members it derives: a status word, a percentage, a completion instant and a storage tier
/// are all the same fact — how far this action has got — so deriving them from one record keeps
/// two writers from recording one snapshot's tier differently.
#[derive(Debug, Clone)]
pub struct SnapshotTieringRecord {
    /// "The date and time that the last archive or restore process was started"
    /// (`SnapshotTierStatus$LastTieringStartTime`), which is also what the call that started
    /// it answered (`ModifySnapshotTierResult$TieringStartTime`) — so the caller's receipt and
    /// every later read name one instant.
    pub started_at: SystemTime,
    /// The `TieringOperationStatus` this action reports while it is still running.
    pub in_progress: &'static str,
    /// The status the clock owes this action, and the instants that move falls due on — the
    /// crate's one shape for a move owed to the clock ([`ClockTransition`]).
    pub completed: ClockTransition<&'static str>,
    /// The storage tier the snapshot is in once this action has completed. The tier it is in
    /// BEFORE then is the one it was in when the action started, which is
    /// [`SnapshotTieringRecord::tier_until_completed`].
    pub tier_when_completed: &'static str,
    /// The tier the snapshot is in while the action is still running.
    pub tier_until_completed: &'static str,
}

impl SnapshotTieringRecord {
    /// An ARCHIVAL started at `started_at`: `archival-in-progress` now, `archival-completed`
    /// once the clock-owed move falls due, from which the snapshot is in the `archive` tier.
    ///
    /// "You can't access the snapshot until the archival process completes": the snapshot is
    /// still in the standard tier while the archival runs — "`archive` indicates that the
    /// snapshot is CURRENTLY archived" — and reaches the archive tier when the action does.
    pub fn archival_started(started_at: SystemTime) -> Self {
        Self {
            started_at,
            in_progress: TIERING_OPERATION_ARCHIVAL_IN_PROGRESS,
            completed: ClockTransition::due_in(
                TIERING_OPERATION_ARCHIVAL_COMPLETED,
                TRANSITION_DELAY,
            ),
            tier_when_completed: SNAPSHOT_STORAGE_TIER_ARCHIVE,
            tier_until_completed: SNAPSHOT_STORAGE_TIER_STANDARD,
        }
    }

    /// Has the clock-owed move of this action fallen due at `now`?
    pub fn has_completed(&self, now: Instant) -> bool {
        now >= self.completed.due_at
    }

    /// This action's `TieringOperationStatus` at `now`.
    pub fn operation_status_at(&self, now: Instant) -> &'static str {
        if self.has_completed(now) {
            self.completed.to
        } else {
            self.in_progress
        }
    }

    /// "The progress of the last archive or restore process, as a percentage" at `now`, which
    /// is the same fact as the status: nothing has been moved while the action is still
    /// running, and all of it has once it has completed.
    pub fn progress_at(&self, now: Instant) -> i32 {
        if self.has_completed(now) {
            TIERING_PROGRESS_COMPLETED
        } else {
            TIERING_PROGRESS_IN_PROGRESS
        }
    }

    /// The `StorageTier` the snapshot this action was asked for is in at `now`.
    pub fn storage_tier_at(&self, now: Instant) -> &'static str {
        if self.has_completed(now) {
            self.tier_when_completed
        } else {
            self.tier_until_completed
        }
    }

    /// "The date and time when the last archive process was completed"
    /// (`SnapshotTierStatus$ArchivalCompleteTime`) at `now`: the wall-clock instant the move
    /// fell due, and only once it has — an action still running has no completion to report,
    /// rather than one in the future. Only an ARCHIVAL has one; a restore completes into the
    /// standard tier and reports its own expiry instead.
    pub fn archival_complete_time_at(&self, now: Instant) -> Option<SystemTime> {
        let archived = self.completed.to == TIERING_OPERATION_ARCHIVAL_COMPLETED;
        (archived && self.has_completed(now)).then_some(self.completed.at)
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// Start the ARCHIVAL of the snapshot `snapshot_id` names at `started_at`, and answer the
    /// record that archival is now described by.
    ///
    /// The ONE writer of [`Self::snapshot_tiering`]: `ModifySnapshotTier` is the operation that
    /// "archives an Amazon EBS snapshot", and the caller has already resolved the snapshot and
    /// checked that its current tier admits an archival — so this records the action and
    /// nothing else. The instants the record derives its answers from are its own, not the
    /// caller's, so no request can report an archival as completed before the clock owes it.
    pub fn begin_snapshot_archival(
        &mut self,
        snapshot_id: &str,
        started_at: SystemTime,
    ) -> SnapshotTieringRecord {
        let record = SnapshotTieringRecord::archival_started(started_at);
        self.snapshot_tiering
            .insert(snapshot_id.to_string(), record.clone());
        record
    }

    /// The archive or restore action last asked for the snapshot `snapshot_id` names, or `None`
    /// when none ever was.
    pub fn snapshot_tiering_of(&self, snapshot_id: &str) -> Option<&SnapshotTieringRecord> {
        self.snapshot_tiering.get(snapshot_id)
    }

    /// Forget the tiering action of the snapshot `snapshot_id` names.
    ///
    /// Reached by every removal of a snapshot ([`Self::remove_snapshot`] and the handler that
    /// deletes a snapshot of the EBS direct APIs): a storage tier is a property OF a snapshot,
    /// so a snapshot the account no longer holds has none — the same rule that takes its fast
    /// snapshot restores and its create volume permissions with it.
    pub fn forget_snapshot_tiering(&mut self, snapshot_id: &str) {
        self.snapshot_tiering.remove(snapshot_id);
    }

    /// `snapshot` with the `StorageTier` its tiering action has reached at `now`.
    ///
    /// The ONE projection of a snapshot's tier onto the `Snapshot` shape every inventory read
    /// answers with, so `DescribeSnapshots`, an attribute read and the tier status read cannot
    /// disagree about which tier one snapshot is in. A snapshot no archive or restore was ever
    /// asked for keeps the tier its writer stored, which is the standard one every snapshot is
    /// created in.
    pub fn snapshot_in_tier(&self, mut snapshot: SnapshotView, now: Instant) -> SnapshotView {
        let tiering = snapshot
            .snapshot_id
            .as_deref()
            .and_then(|snapshot_id| self.snapshot_tiering_of(snapshot_id));
        if let Some(tiering) = tiering {
            snapshot.storage_tier = Some(tiering.storage_tier_at(now).to_string());
        }
        snapshot
    }
}
