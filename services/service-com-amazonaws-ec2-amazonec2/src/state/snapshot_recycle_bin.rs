// SPDX-License-Identifier: CC0-1.0

//! EBS snapshots in the Recycle Bin: the store a deleted snapshot waits in between its
//! deletion and its permanent deletion, and the rules that move one in and out of it.
//!
//! The Recycle Bin holds a snapshot that has been deleted while a retention rule covered it,
//! until "the date and time when the snapshot is to be permanently deleted from the Recycle
//! Bin" (`SnapshotRecycleBinInfo$RecycleBinExitTime`) — the instant it entered plus the
//! retention period of the rule that covered it. A snapshot the bin holds is NOT one of the
//! snapshots the account can read: `DescribeSnapshots` refuses its id, nothing can be
//! restored from it and nothing can take a copy of it, while it is still being kept. So
//! [`super::ComAmazonawsEc2Amazonec2State::snapshots_in_recycle_bin`] keeps it in a store of
//! its own, keyed by the same `SnapshotId`, exactly as
//! [`super::ComAmazonawsEc2Amazonec2State::volumes_in_recycle_bin`] keeps a deleted volume —
//! and for the same reason the record is kept WHOLE: a restore gives back the snapshot the
//! bin took in, with its id, its description, its size, its tags and the completion the
//! clock owed it, rather than one rebuilt from the projection a report of the bin answers
//! with.
//!
//! The permanent deletion at the end of the retention period is owed by the CLOCK, recorded
//! rather than slept for the way a `pending` snapshot's `completed` move is, and
//! [`super::ComAmazonawsEc2Amazonec2State::materialize_due_transitions`] applies it: an
//! entry whose retention period has run out is gone from the bin whether or not anybody
//! asked, so `RestoreSnapshotFromRecycleBin` can only restore an entry that is still inside
//! its retention period.
//!
//! `RestoreSnapshotFromRecycleBin` is the operation that takes a snapshot back out of this
//! store, and `DeleteSnapshot` the one that puts it in: a deleted resource is retained only
//! when a Recycle Bin retention rule covers it, retention rules are the `rbin` service's
//! resources rather than EC2's, and that delete asks them through
//! [`super::recycle_bin_retention_period`] before it removes anything — so a delete no rule
//! covers is still a permanent one.
//! [`ComAmazonawsEc2Amazonec2State::move_snapshot_to_recycle_bin`] is the one writer of this
//! store, and [`RecycledSnapshotRecord::entry`] the projection `ListSnapshotsInRecycleBin`
//! reports.

use std::time::{Duration, Instant, SystemTime};

use crate::ops::types::SnapshotRecycleBinInfo;
use crate::scaffolded::Scaffolded;

use super::{
    ComAmazonawsEc2Amazonec2State, INVALID_SNAPSHOT_ID_MALFORMED, MAX_STORED_RECORDS_PER_STORE,
    STORED_RECORD_OVERHEAD_BYTES, SnapshotRecord, snapshot_payload_bytes,
    system_time_to_date_time,
};

/// The page sizes a report of the Recycle Bin's snapshots accepts: the modelled
/// `@range(min: 5, max: 1000)` of `ListSnapshotsInRecycleBinMaxResults`
/// (`smithy_query(query="shape",shape="ListSnapshotsInRecycleBinMaxResults")`). A request that
/// names none gets the top of this range, so the `NextToken` path behaves the same whether or
/// not a page size was asked for.
pub const SNAPSHOT_RECYCLE_BIN_PAGE_SIZE_RANGE: (i32, i32) = (5, 1_000);

/// The bytes one entry of the snapshot Recycle Bin holds.
///
/// The ONE definition of that charge, the way [`snapshot_payload_bytes`] is for a stored
/// snapshot: the fixed [`STORED_RECORD_OVERHEAD_BYTES`] every stored record costs — so an
/// entry carrying no optional text is NOT free against [`super::MAX_STORED_BYTES`] — plus the
/// text the entry carries over from the snapshot it was derived from, which is text a request
/// chose.
pub fn snapshot_recycle_bin_entry_payload_bytes(entry: &SnapshotRecycleBinInfo) -> usize {
    STORED_RECORD_OVERHEAD_BYTES
        .saturating_add(entry.description.as_deref().map_or(0, str::len))
        .saturating_add(entry.volume_id.as_deref().map_or(0, str::len))
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another entry fits inside the snapshot Recycle Bin's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]).
    ///
    /// The same contract [`Self::has_volume_recycle_bin_capacity`] keeps for the volume bin,
    /// and needed for the same reason: nothing in this emulator reclaims an entry of the bin
    /// before its retention period runs out, so without a ceiling a sequence of
    /// create-then-retain requests would grow memory without bound.
    pub fn has_snapshot_recycle_bin_capacity(&self) -> bool {
        self.snapshots_in_recycle_bin.len() < MAX_STORED_RECORDS_PER_STORE
    }

    /// Move the stored snapshot `snapshot_id` into the Recycle Bin, where it stays for
    /// `retention`, and answer the entry a report of the bin now holds for it.
    ///
    /// The entry is DERIVED from the stored snapshot rather than taken from a request, so a
    /// snapshot's description and source volume cannot read differently inside the bin from
    /// outside it; `retention` is the retention period of the Recycle Bin rule that covered
    /// the snapshot, which is a fact of that rule and so is given by the caller.
    ///
    /// `None` when the store holds no such snapshot, when the bin is at
    /// [`MAX_STORED_RECORDS_PER_STORE`], or when `now + retention` is not a representable
    /// instant. In each case NOTHING was removed and nothing stored — every fallible step is
    /// taken before the first mutation — so a refused move leaves the snapshot where it was
    /// instead of losing it between the two stores.
    ///
    /// `now` is the instant of THIS call, the wall-clock reading of the moment the snapshot is
    /// being moved, so the two instants the entry reports are one reading of one moment; the
    /// monotonic deadline the permanent deletion is enforced against is the SAME moment read
    /// off the clock that cannot jump, the way [`Self::move_volume_to_recycle_bin`] takes
    /// both.
    ///
    /// The byte accounting is symmetric: the bytes the create charged for the snapshot are
    /// given back as it leaves the store ([`Self::remove_snapshot`]) and the entry that takes
    /// its place is charged for itself, so a repeated create-then-retain sequence advances
    /// the byte budget rather than trading a charged snapshot for a free entry.
    pub fn move_snapshot_to_recycle_bin(
        &mut self,
        snapshot_id: &str,
        now: SystemTime,
        retention: Duration,
    ) -> Option<SnapshotRecycleBinInfo> {
        if !self.has_snapshot_recycle_bin_capacity() {
            return None;
        }
        // "The date and time when the snapshot is to be permanently deleted from the Recycle
        // Bin" is `now` plus the retention period the rule carries, and `SystemTime +
        // Duration` PANICS when that instant is not representable. The retention period comes
        // from the caller, so the sum is asked for with `checked_add` and the move is refused
        // rather than unwinding — and it is asked BEFORE the snapshot is removed.
        let exit_at = now.checked_add(retention)?;
        // The same instant on the MONOTONIC clock, which is what the permanent deletion the
        // clock owes is compared against ([`Self::materialize_due_transitions`]), so a
        // wall-clock adjustment can neither keep an expired entry nor expire a fresh one.
        // Fallible, and asked BEFORE the snapshot is removed, for the same reason as above.
        let exit_due_at = Instant::now().checked_add(retention)?;
        if !self.snapshots.contains_key(snapshot_id) {
            return None;
        }
        // The removal is the one that gives the snapshot's caller-chosen bytes back to the
        // budget and takes its fast snapshot restores with it: a restore is a property of the
        // (snapshot, Availability Zone) pair, and the snapshot has been deleted — what the
        // bin keeps is a deleted snapshot, not one the account can still restore volumes
        // from.
        let record = self.remove_snapshot(snapshot_id)?;
        let entry = SnapshotRecycleBinInfo {
            snapshot_id: record.view.snapshot_id.clone(),
            // "The description for the snapshot" and "the ID of the volume from which the
            // snapshot was created" (`SnapshotRecycleBinInfo`): read off the snapshot itself,
            // so the bin reports what the snapshot carried.
            description: record.view.description.clone(),
            volume_id: record.view.volume_id.clone(),
            // "The date and time when the snapshot entered the Recycle Bin" — now — and "the
            // date and time when the snapshot is to be permanently deleted from the Recycle
            // Bin", which is when the retention period that covered it runs out.
            recycle_bin_enter_time: Some(system_time_to_date_time(now)),
            recycle_bin_exit_time: Some(system_time_to_date_time(exit_at)),
        };
        self.add_stored_bytes(snapshot_recycle_bin_entry_payload_bytes(&entry));
        self.snapshots_in_recycle_bin.insert(
            snapshot_id.to_string(),
            RecycledSnapshotRecord {
                entry: entry.clone(),
                snapshot: record,
                exit_due_at,
            },
        );
        Some(entry)
    }

    /// Take the snapshot the Recycle Bin holds for `snapshot_id` back into the account's
    /// snapshots, and answer whether the bin held one.
    ///
    /// The MOVE back: the record the bin kept is the record the snapshot store gets, so the
    /// snapshot comes back with the id, the owner, the description, the volume and size it was
    /// taken of, its tags and the instant its `pending` -> `completed` move falls due. The
    /// byte accounting is the mirror of [`Self::move_snapshot_to_recycle_bin`]: the entry
    /// gives its bytes back and the snapshot is charged what the create charged for it, so a
    /// snapshot that goes into the bin and comes back leaves the budget where it started.
    ///
    /// `false` when the bin holds no entry for `snapshot_id`, and then NOTHING was written —
    /// so a caller answers the request's own refusal rather than reporting a restore that did
    /// not happen.
    ///
    /// The snapshot store's own count ceiling is NOT asked again here, unlike
    /// [`Self::insert_snapshot`]'s writers: a restore gives back a snapshot that store already
    /// held and the bin has been holding since, so refusing it would strand the record in a
    /// bin whose retention period then permanently deletes something the account asked to keep.
    /// The two stores' ceilings bound the pair either way.
    pub fn restore_recycled_snapshot(&mut self, snapshot_id: &str) -> bool {
        let Some(record) = self.snapshots_in_recycle_bin.remove(snapshot_id) else {
            return false;
        };
        self.release_stored_bytes(snapshot_recycle_bin_entry_payload_bytes(&record.entry));
        let RecycledSnapshotRecord { snapshot, .. } = record;
        self.add_stored_bytes(snapshot_payload_bytes(
            snapshot.view.tags.as_deref().unwrap_or_default(),
            snapshot.view.description.as_deref(),
        ));
        self.snapshots.insert(snapshot_id.to_string(), snapshot);
        true
    }

    /// Apply the permanent deletions the clock owes the snapshot Recycle Bin: an entry whose
    /// retention period has run out is deleted for good — "the date and time when the snapshot
    /// is to be permanently deleted from the Recycle Bin"
    /// (`SnapshotRecycleBinInfo$RecycleBinExitTime`) — whether or not anybody called, so
    /// neither a report of the bin nor a restore can see it afterwards, and the bytes the
    /// entry was charged go back to the budget.
    ///
    /// Reached only through [`Self::materialize_due_transitions`], so a reporting operation
    /// materialises nothing but what the clock already made due.
    pub(super) fn delete_due_recycled_snapshots(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .snapshots_in_recycle_bin
            .iter()
            .filter(|(_, record)| now >= record.exit_due_at)
            .map(|(snapshot_id, _)| snapshot_id.clone())
            .collect();
        for snapshot_id in expired {
            if let Some(record) = self.snapshots_in_recycle_bin.remove(&snapshot_id) {
                self.release_stored_bytes(snapshot_recycle_bin_entry_payload_bytes(&record.entry));
            }
        }
    }
}

/// One snapshot the Recycle Bin holds: the modelled entry a report of the bin answers with,
/// beside the stored snapshot itself, so
/// [`ComAmazonawsEc2Amazonec2State::restore_recycled_snapshot`] gives back the SAME resource
/// rather than one rebuilt from the entry's members — the entry carries neither the
/// snapshot's tags nor its encryption, and a restored snapshot keeps both.
#[derive(Debug, Clone)]
pub struct RecycledSnapshotRecord {
    /// What `ListSnapshotsInRecycleBin` reports for this snapshot, derived from the snapshot
    /// when it entered the bin
    /// ([`ComAmazonawsEc2Amazonec2State::move_snapshot_to_recycle_bin`] is the one writer).
    pub entry: SnapshotRecycleBinInfo,
    /// The snapshot exactly as the snapshot store held it, so a restore gives back the same
    /// resource.
    pub snapshot: SnapshotRecord,
    /// The instant from which this entry's permanent deletion is due — the monotonic reading
    /// of its `RecycleBinExitTime`, never reported, so it is the clock's and not a member of
    /// the entry two writers could disagree about.
    pub exit_due_at: Instant,
}

impl RecycledSnapshotRecord {
    /// How much of this entry's retention period is left at `now`, which is zero once the
    /// permanent deletion is due.
    pub fn retention_left(&self, now: Instant) -> Duration {
        self.exit_due_at.saturating_duration_since(now)
    }
}

/// Refuse a `SnapshotId` the Recycle Bin holds no entry for: HTTP 400
/// `InvalidSnapshotID.Malformed`, the one code the service answers for every id
/// `RestoreSnapshotFromRecycleBin` cannot resolve in the bin — an id that is not shaped like
/// one, a well-formed id the bin never held, one whose retention period has run out, and one
/// the account still has a live snapshot for.
///
/// The CODE is [`INVALID_SNAPSHOT_ID_MALFORMED`], the same code
/// [`super::unresolvable_snapshot_id`] answers a misshapen id with, but the WORDING is this
/// operation's: a Recycle Bin restore names the resource ID it could not resolve and says it
/// is malformed, as the recorded traffic behind
/// `issues/things-that-now-work/20261022T235108Z-ec2-restorevolumefromrecyclebin-stub.md`
/// has the service answering its sibling restore ("The volume ID 'vol-…' is malformed"),
/// where a describe of the account's snapshots answers
/// [`super::unresolvable_snapshot_id_error`]'s own two sentences instead. The message is
/// built from the request — the id the caller sent is the only part of it that varies, held
/// to the crate's one bound on an echoed caller value ([`super::echoed_resource_id`]), the same bound
/// the sibling restore's wording keeps — through the crate's one builder of that sentence
/// ([`super::snapshot_id_malformed_resource_message`]), which `ResetSnapshotAttribute` answers
/// on its own recorded evidence.
pub fn snapshot_id_not_in_recycle_bin<E>(snapshot_id: &str) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_SNAPSHOT_ID_MALFORMED,
        400,
        super::snapshot_id_malformed_resource_message(snapshot_id),
    )
}
