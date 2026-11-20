// SPDX-License-Identifier: CC0-1.0

//! What the domain's log publishing has of ONE DOCUMENT WRITE, stated once for every route
//! that performs one.
//!
//! A write arrives on this plane in three forms — one document addressed by its path
//! ([`super::document_write`]), the partial update of one ([`super::document_update`]), and
//! an item of a bulk stream ([`super::bulk`]) — and the cluster applies all three through the
//! one write path ([`crate::state::ingest::index_into`]). So all three produce the same
//! records, which is what this module states:
//!
//! * the INDEXING slow-log record of the write, which the index's own
//!   `index.indexing.slowlog.threshold.index.<level>` decides is slow or not
//!   ([`crate::state::log_publishing`]) — the duration compared against it is the one the
//!   write really took;
//! * the cluster's APPLICATION-log line for an index it CREATED for that write, where the
//!   write named an index the cluster did not hold: creating an index is a change to the
//!   cluster's own state whichever call caused it, so the line reads as it does for a create
//!   that was asked for, with the cause the write's own
//!   ([`crate::state::log_publishing::CAUSE_AUTO_WRITE`]).
//!
//! Without this, a client whose producer loads documents in bulk would see an empty
//! slow-log group however low it set the index's threshold, while the same documents written
//! one at a time filled it.
//!
//! What one request can publish stays bounded by what it carried: one record per document it
//! WROTE (a bulk stream carries a bounded number of actions, inside a body bounded by
//! [`super::MAX_REQUEST_BODY_BYTES`]), each stating at most the first characters of that
//! document's own source, and the destination stream trims itself to the ceiling the owning
//! service holds every stream to.

use ::std::time::Duration;

use crate::state::{log_publishing, Domain};

use super::{setting_number, Request, NUMBER_OF_REPLICAS, NUMBER_OF_SHARDS};

/// Note the write of one document into `index_name` under `id`, which took `took` and
/// carried `source`.
///
/// `auto_created` says whether the cluster had to CREATE that index for this write, which is
/// read before the write is applied — afterwards the index is there either way.
pub(super) fn record_written(
    domain: &Domain,
    request: &Request<'_>,
    written: Written<'_>,
) {
    let Written {
        index_name,
        id,
        took,
        source,
        auto_created,
    } = written;
    let mut served: Vec<log_publishing::Served<'_>> = Vec::new();
    if auto_created {
        let (shards, replicas) = shard_layout(domain, index_name);
        served.push(log_publishing::Served::Applied {
            index: index_name,
            change: log_publishing::IndexChange::Created {
                cause: log_publishing::CAUSE_AUTO_WRITE,
                shards,
                replicas,
            },
        });
    }
    served.push(log_publishing::Served::Indexed {
        index: index_name,
        id,
        took,
        source,
    });
    request.record_served(domain, &served);
}

/// One applied write, as the route that performed it saw it.
pub(super) struct Written<'a> {
    /// The index the document landed in — the one the write path resolved, never the name
    /// the request spelled, so a write through an alias is recorded against the index that
    /// holds the document.
    pub index_name: &'a str,
    /// The `_id` it landed under.
    pub id: &'a str,
    /// How long the write really took.
    pub took: Duration,
    /// The document as it was written.
    pub source: &'a str,
    /// Whether the cluster created the index for this write.
    pub auto_created: bool,
}

/// The shard layout the created index carries, as the engine's own line carries it: the two
/// settings the index holds, and the cluster's own default of one of each where it holds
/// neither.
fn shard_layout(domain: &Domain, index_name: &str) -> (i64, i64) {
    domain.indexes.get(index_name).map_or((1, 1), |index| {
        (
            setting_number(&index.schema, NUMBER_OF_SHARDS).unwrap_or(1),
            setting_number(&index.schema, NUMBER_OF_REPLICAS).unwrap_or(1),
        )
    })
}
