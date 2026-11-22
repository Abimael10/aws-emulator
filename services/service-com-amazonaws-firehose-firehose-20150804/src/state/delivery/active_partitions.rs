// SPDX-License-Identifier: CC0-1.0

//! The partitions a dynamically partitioned delivery may write at once.
//!
//! Dynamic partitioning "creates smaller data sets from the streaming data by partitioning it
//! based on partition keys" (`DynamicPartitioningConfiguration`), and each distinct set of
//! keys a buffer's records derive is one partition the flush writes an object for
//! ([`super::grouped`]). How many of those a stream may hold open at one time is a published
//! quota — a Firehose stream has at most [`MAX_ACTIVE_PARTITIONS`] active partitions — and
//! this module is where it is charged.
//!
//! The partitions ACTIVE at a flush are the ones its own buffer creates: a partition is open
//! while records are buffered for it and is closed by the delivery that writes them, so the
//! count a flush is charged is the distinct key sets of the records it holds. They are
//! admitted in the records' ARRIVAL order, because a partition is created by the first record
//! that falls in it, and a record whose keys would open one beyond the quota is not delivered
//! under the data prefix: it goes to the destination's error output carrying
//! [`ACTIVE_PARTITIONS_LIMIT_EXCEEDED`] and its own bytes, which is what lets a consumer of
//! that prefix see the rejected records and re-drive them.
//!
//! Nothing about ingestion changes: `PutRecord` / `PutRecordBatch` take such a record like any
//! other — the quota is a property of the delivery, not of the put — so it is the error output
//! and not the put's answer that reports it.

use std::collections::BTreeSet;

use super::{DeliveryFailure, GroupKeys};

/// The most partitions one delivery stream may have active at a time.
///
/// A published Firehose quota ("active partitions per delivery stream",
/// <https://docs.aws.amazon.com/general/latest/gr/fh.html>), and the one a client designing
/// its partition keys has to design around: the cardinality of those keys, the fan-out of the
/// objects they write, and the error-prefix consumer that re-drives what the quota rejected.
pub(super) const MAX_ACTIVE_PARTITIONS: usize = 500;

/// The failure of a record whose partition keys would open a partition beyond
/// [`MAX_ACTIVE_PARTITIONS`].
///
/// Its keys were derived — nothing about the record or the extraction failed — so the error
/// output names the QUOTA it did not fit in, which is the condition a consumer re-drives such
/// a record on.
pub(super) const ACTIVE_PARTITIONS_LIMIT_EXCEEDED: DeliveryFailure = DeliveryFailure {
    output_type: "active-partition-count-exceeded",
    error_code: "DynamicPartitioning.ActivePartitionsLimitExceeded",
    error_message: "active-partition-count-exceeded",
};

/// `derived`, the partition keys of one buffer's records in arrival order, with the records
/// beyond the quota moved to [`ACTIVE_PARTITIONS_LIMIT_EXCEEDED`].
///
/// The first [`MAX_ACTIVE_PARTITIONS`] distinct key sets are the partitions this delivery
/// writes; every later record either falls in one of them — a partition already open takes any
/// number of records — or is a partition beyond the quota and is diverted. A record whose keys
/// could not be derived at all keeps its own failure: the quota is charged for the partitions
/// that exist, and such a record creates none.
pub(super) fn within_quota(derived: Vec<GroupKeys>) -> Vec<GroupKeys> {
    let mut active: BTreeSet<GroupKeys> = BTreeSet::new();
    derived
        .into_iter()
        .map(|keys| {
            // A record that derived no keys creates no partition, and one of a partition
            // already open is not charged again: either way the record keeps what it had.
            if !matches!(keys, GroupKeys::Derived(_)) || active.contains(&keys) {
                return keys;
            }
            if active.len() < MAX_ACTIVE_PARTITIONS {
                active.insert(keys.clone());
                return keys;
            }
            GroupKeys::Failed(ACTIVE_PARTITIONS_LIMIT_EXCEEDED)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// The keys of a record falling in the partition `slot`.
    fn slot(slot: usize) -> GroupKeys {
        GroupKeys::Derived(BTreeMap::from([(
            "slot".to_string(),
            format!("slot-{slot:04}"),
        )]))
    }

    /// How many of `admitted` were diverted to the quota failure.
    fn over_quota(admitted: &[GroupKeys]) -> usize {
        admitted
            .iter()
            .filter(|keys| **keys == GroupKeys::Failed(ACTIVE_PARTITIONS_LIMIT_EXCEEDED))
            .count()
    }

    #[test]
    fn partitions_up_to_the_quota_are_all_admitted() {
        let derived: Vec<GroupKeys> = (0..MAX_ACTIVE_PARTITIONS).map(slot).collect();
        let admitted = within_quota(derived.clone());
        assert_eq!(
            over_quota(&admitted),
            0,
            "case \"exactly the quota\": nothing is diverted, got {} of {} records diverted",
            over_quota(&admitted),
            derived.len()
        );
        assert_eq!(
            admitted, derived,
            "case \"exactly the quota\": every partition inside it keeps its own keys"
        );
    }

    #[test]
    fn only_the_partitions_beyond_the_quota_are_diverted() {
        let over = 3usize;
        let derived: Vec<GroupKeys> = (0..MAX_ACTIVE_PARTITIONS + over).map(slot).collect();
        let admitted = within_quota(derived.clone());
        assert_eq!(
            over_quota(&admitted),
            over,
            "case \"{over} partitions past the quota\": expected that many diverted, got {}",
            over_quota(&admitted)
        );
        assert_eq!(
            admitted[..MAX_ACTIVE_PARTITIONS],
            derived[..MAX_ACTIVE_PARTITIONS],
            "case \"{over} partitions past the quota\": the partitions inside it keep their own \
             keys"
        );
    }

    #[test]
    fn a_partition_already_open_takes_any_number_of_records() {
        // One partition past the quota, and every in-quota partition carrying a second
        // record: a record of an OPEN partition is never charged again.
        let mut derived: Vec<GroupKeys> = (0..MAX_ACTIVE_PARTITIONS).map(slot).collect();
        derived.extend((0..MAX_ACTIVE_PARTITIONS).map(slot));
        derived.push(slot(MAX_ACTIVE_PARTITIONS));
        let admitted = within_quota(derived);
        assert_eq!(
            over_quota(&admitted),
            1,
            "case \"repeated records of open partitions\": only the one new partition is \
             diverted, got {} diverted",
            over_quota(&admitted)
        );
    }
}
