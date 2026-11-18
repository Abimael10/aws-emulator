// SPDX-License-Identifier: CC0-1.0

//! Whether an index is BACKFILLING the items its table already held when the index was
//! added, and when the clock owes that work its completion.
//!
//! "Backfilling is the process of reading items from the table and determining whether
//! they can be added to the index. ... After all items have been processed, the
//! backfilling operation is complete and `Backfilling` is false", and "For indexes that
//! were created during a `CreateTable` operation, the `Backfilling` attribute does not
//! appear in the `DescribeTable` output" (the `GlobalSecondaryIndexDescription$Backfilling`
//! doc). A vector index reports the same member for the same reason — "Specifies whether
//! the index is currently backfilling. During backfill, `SearchVectors` operations might
//! return incomplete results" (`VectorIndexDescription$Backfilling`) — so the three states
//! an index's `Backfilling` can be in are stated ONCE here and every index kind that
//! reports the member projects it from this.
//!
//! The three states are: an index defined WITH its table, which reports no member at all
//! because there was no table data to read when it was defined; an index ADDED to a table
//! that already held items, which is backfilling until those items have been processed;
//! and an index added to a table holding none, which has nothing to read and so is never
//! backfilling. `super` re-exports this module, so every caller still names it
//! `crate::state::…`.

use ::std::time::SystemTime;

/// The backfill one stored index has to do, as its description's `Backfilling` reports it.
///
/// Carried by every index kind whose description has that member, so the member's three
/// states ([`IndexBackfill::backfilling`]) are decided by one rule wherever an index is
/// described.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexBackfill {
    /// Defined WITH the table: there were no items to read, and the member "does not
    /// appear in the DescribeTable output" at all.
    #[default]
    WithTable,
    /// ADDED to a table that already existed, with the instant the clock owes the reading
    /// of the items it already held. An index added to a table holding none has that
    /// instant already behind it, because there was nothing to read.
    Added {
        /// When every item the table already held has been processed, after which
        /// "the backfilling operation is complete and `Backfilling` is false".
        complete_at: SystemTime,
    },
}

impl IndexBackfill {
    /// The backfill an index ADDED to a table at `now` has to do, where `complete_at` is
    /// when the clock owes the reading of the items the table held and `holds_items` says
    /// whether it held any.
    ///
    /// An index added to a table with items has them to process, so it backfills for that
    /// span; one added to an empty table has nothing to read and its backfill is complete
    /// the moment it is added. Either way the member is REPORTED, because the index was
    /// not created with its table.
    pub fn added(holds_items: bool, now: SystemTime, complete_at: SystemTime) -> Self {
        Self::Added {
            complete_at: if holds_items { complete_at } else { now },
        }
    }

    /// What an index's description reports as `Backfilling` at `now`: nothing for an index
    /// defined with its table, and otherwise whether the items the table already held are
    /// still being processed.
    pub fn backfilling(&self, now: SystemTime) -> Option<bool> {
        match self {
            Self::WithTable => None,
            Self::Added { complete_at } => Some(now < *complete_at),
        }
    }
}
