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
//!
//! The same instants are what an index's BUILD is made of, so they live here too: an added
//! index has the storage its entries need allocated first
//! ([`IndexBackfill::allocating`]) and then reads the items, and it is `ACTIVE` when that
//! reading is done and the table's own update has settled
//! ([`IndexBackfill::settled_at`]) — which is why an index added to a table that already
//! holds items is still being built once the table reports itself settled.

use ::std::time::{Duration, SystemTime};

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
        /// When the clock owes the END of the resource allocation an index's build starts
        /// with: the storage the entries of the items it has to read will be held in.
        ///
        /// It is the phase a `Delete` of the index cannot be served in — that storage is
        /// not there to release yet, which is what the refusal tells the caller to retry
        /// past — and it is over well before the table's own update settles, so a caller
        /// that waits for the table is always past it.
        allocated_at: SystemTime,
        /// When every item the table already held has been processed, after which
        /// "the backfilling operation is complete and `Backfilling` is false".
        complete_at: SystemTime,
    },
}

impl IndexBackfill {
    /// The build an index ADDED to a table at `now` has ahead of it, where `span` is the
    /// span the table's own update takes and `holds_items` says whether the table already
    /// held items for the new index to read.
    ///
    /// The storage the index's entries need is laid down FIRST, which is done well before
    /// the table's own update settles, and only then are the items read — which takes one
    /// span more, so an index added to a table that already holds items is still being
    /// built when the table reports itself settled, exactly as a client that waits for the
    /// TABLE and then looks at the index sees. An index added to a table holding none has
    /// nothing to read, so its reading is over the moment it is added and its build ends
    /// with the table's own update ([`Self::settled_at`]).
    ///
    /// WHY THESE SPANS. AWS publishes no duration for either phase — an index build takes
    /// as long as the data takes — so what is modelled is the ORDER a client observes, in
    /// the service's own transient span, and nothing is waited for:
    ///
    /// * the reading is a span of its OWN, after the table's, because the table returns to
    ///   `ACTIVE` while the index it was given keeps building: a client that waits for the
    ///   table and then reads the index has to be able to see that;
    /// * the laying down of the storage is over inside the table's own span, because it is
    ///   what the table's update is doing: a caller that waits for the table is past that
    ///   phase, while one that acts straight after the call is inside it — which is what
    ///   makes the phase observable at all without being a race at the boundary.
    pub fn added(holds_items: bool, now: SystemTime, span: Duration) -> Self {
        Self::Added {
            allocated_at: now + span / 2,
            complete_at: if holds_items { now + span + span } else { now },
        }
    }

    /// What an index's description reports as `Backfilling` at `now`: nothing for an index
    /// defined with its table, nothing once the items the table already held have been
    /// read, and `true` while they are still being processed.
    ///
    /// "After all items have been processed, the backfilling operation is complete and
    /// `Backfilling` is false" (the `Backfilling` doc), which the service reports by
    /// dropping the member: a settled index describes no backfill, exactly as an index
    /// defined with its table does.
    pub fn backfilling(&self, now: SystemTime) -> Option<bool> {
        match self {
            Self::WithTable => None,
            Self::Added { complete_at, .. } => (now < *complete_at).then_some(true),
        }
    }

    /// When the clock owes this index its BUILD, which is when it becomes `ACTIVE`: the
    /// later of the instant the items the table already held have been read and
    /// `with_table`, the instant the table's own update settles.
    ///
    /// An index is built when it has read what it was given to read AND the table's own
    /// update is done, so one added to a table that already holds items keeps building
    /// after that update has settled — exactly as a client that waits for the TABLE and
    /// then looks at the index sees — while one with nothing to read is built with the
    /// table's update itself. An index defined WITH its table settles with the table.
    pub fn settled_at(&self, with_table: SystemTime) -> SystemTime {
        match self {
            Self::WithTable => with_table,
            Self::Added { complete_at, .. } => with_table.max(*complete_at),
        }
    }

    /// Whether the index this backfill belongs to is still having the storage its entries
    /// need ALLOCATED at `now`, the phase the build of an added index begins with
    /// ([`Self::added`]).
    ///
    /// An index defined with its table is being built for as long as the table itself is
    /// being created, which the table's own status answers, so this speaks only about an
    /// index that was ADDED.
    pub fn allocating(&self, now: SystemTime) -> bool {
        match self {
            Self::WithTable => false,
            Self::Added { allocated_at, .. } => now < *allocated_at,
        }
    }
}
