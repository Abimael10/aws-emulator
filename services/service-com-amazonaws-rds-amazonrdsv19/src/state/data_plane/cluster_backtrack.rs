// SPDX-License-Identifier: CC0-1.0

//! The CONSISTENT POINTS a DB CLUSTER keeps: the data it held at each instant a session
//! committed a change, which is what `BacktrackDBCluster` rewinds it to.
//!
//! "Backtracking rewinds the DB cluster to the time you specify" and "if the specified time
//! isn't a consistent time for the DB cluster, Aurora automatically chooses the nearest
//! possible consistent time": everything committed at or before that instant survives the
//! rewind and only what was committed after it is gone. So the cluster has to KEEP the data
//! of each instant its contents changed — exactly as a DB instance's continuous backup keeps
//! a recovery point per commit ([`super::continuous_backup`]) — or a rewind to any instant
//! after the cluster came up could only put back the data it came up with, which is a reset
//! rather than a rewind.
//!
//! What a point IS here: a COPY of the cluster volume, taken by the engine's own online
//! backup ([`super::sql_engine::InstanceDatabase::capture`]) at the instant a session on one
//! of the cluster's endpoints committed. Between two commits the cluster's data does not
//! change, so the copy taken at the last commit at or before an instant is the data the
//! cluster held at that instant. The points live on the cluster's own record
//! ([`crate::state::DbClusterRecord::backtrack_points`]), so they go with the cluster, and
//! they are bounded there two ways: by the cluster's backtrack window and by how many copies
//! one cluster keeps.
//!
//! A cluster with backtracking off keeps none — "to disable backtracking, set this value to
//! 0" leaves no time it can be rewound to — which the record's own writer enforces.

use std::sync::Arc;

use super::sql_engine::InstanceDatabase;
use crate::state::{ComAmazonawsRdsAmazonrdsv19State, DbClusterRecord};

/// Record what a session on an endpoint of the DB cluster `region`/`resource_id` has just
/// COMMITTED as a consistent point of that cluster. Does nothing for a cluster that keeps no
/// backtrack window.
///
/// The store's guard is taken only to read whether the cluster keeps points and, at the end,
/// to record one; the COPY itself is engine work, so it runs on a blocking task under the
/// engine's own copy deadline and page ceiling, exactly as the capture a DB cluster snapshot
/// takes does. Nothing of the client's statement depends on its outcome: a copy the engine
/// could not take leaves the cluster with the points it already had, and is recorded in this
/// service's own tracing.
pub async fn record_committed_state(
    state: &::engine::ServiceState<ComAmazonawsRdsAmazonrdsv19State>,
    region: &str,
    resource_id: &str,
    storage: &Arc<InstanceDatabase>,
) {
    let keeps_points = state
        .lock()
        .cluster_by_resource_id(region, resource_id)
        .is_some_and(DbClusterRecord::backtracking_enabled);
    if !keeps_points {
        return;
    }
    // The instant the data reached this state is read HERE, before the copy is awaited: a
    // client that notes the time right after its commit and then backtracks to it must find
    // this point at or before that time.
    let at = crate::state::now_as_wire_timestamp();
    let source = Arc::clone(storage);
    let captured = ::tokio::task::spawn_blocking(move || source.capture()).await;
    let copy = match captured {
        Ok(Ok(copy)) => Arc::new(copy),
        Ok(Err(error)) => {
            ::tracing::info!(
                operation = "DbClusterBacktrackPoint",
                outcome = "not-recorded",
                reason = %error.message,
                "a committed change could not be recorded as a consistent point"
            );
            return;
        }
        Err(error) => {
            ::tracing::info!(
                operation = "DbClusterBacktrackPoint",
                outcome = "not-recorded",
                reason = %error,
                "a committed change could not be recorded as a consistent point"
            );
            return;
        }
    };
    let mut store = state.lock();
    if let Some(record) = store.cluster_by_resource_id_mut(region, resource_id) {
        record.record_backtrack_point_at(at, copy);
    }
}
