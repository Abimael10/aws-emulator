// SPDX-License-Identifier: CC0-1.0

//! What a GRAPH DB cluster this front door provisioned RUNS, recorded where that cluster's
//! own DATA PLANE can read it.
//!
//! An Amazon Neptune cluster's data plane is the graph API this emulator serves at its own
//! endpoint ([`super::api_plane_address`]), and that plane's engine-status endpoint reports
//! the engine version the cluster was created with and the status of every feature the
//! create settled. The data plane configures none of that — this control plane does — so a
//! cluster it provisions is recorded in the directory both services read
//! ([`::service_common::graph_db_clusters`]), and deleting the cluster withdraws the record.
//!
//! Only a cluster whose data plane this emulator serves as an API is recorded
//! ([`super::engine_data_plane_is_an_api`]): for every other engine there is no such plane
//! to answer for, and a record of one would claim a graph where none is served.

use super::DbClusterRecord;

/// The CloudWatch log a graph cluster publishes when `EnableCloudwatchLogsExports` names
/// `audit`, which is the audit-log configuration this control plane holds for a cluster.
pub const GRAPH_AUDIT_LOG_EXPORT: &str = "audit";

/// The CloudWatch log a graph cluster publishes when `EnableCloudwatchLogsExports` names
/// `slowquery`.
pub const GRAPH_SLOW_QUERY_LOG_EXPORT: &str = "slowquery";

/// What `record` was provisioned with, as the shared directory holds it, or `None` for a
/// cluster whose data plane this emulator serves no graph API for.
///
/// `provisioned_at` is the instant the record is handed over, which is the create's own: the
/// directory orders clusters by it, and the stored create time is a wire timestamp rather
/// than an instant.
#[must_use]
pub fn provisioned_graph_cluster(
    record: &DbClusterRecord,
) -> Option<::service_common::graph_db_clusters::GraphClusterConfiguration> {
    if !super::engine_data_plane_is_an_api(&record.engine) {
        return None;
    }
    let publishes = |log: &str| {
        record
            .enabled_cloudwatch_logs_exports
            .iter()
            .any(|export| export.eq_ignore_ascii_case(log))
    };
    Some(::service_common::graph_db_clusters::GraphClusterConfiguration {
        engine_version: record.engine_version.clone().unwrap_or_default(),
        iam_database_authentication_enabled: record.iam_database_authentication_enabled,
        audit_log_enabled: publishes(GRAPH_AUDIT_LOG_EXPORT),
        slow_query_log_enabled: publishes(GRAPH_SLOW_QUERY_LOG_EXPORT),
        provisioned_at: ::std::time::SystemTime::now(),
    })
}

/// The directory of what each graph DB cluster this emulator provisioned RUNS is reached
/// through the ONE handle the shared crate exposes
/// ([`::service_common::graph_db_clusters::GraphDbClusters`]), in the SAME shared engine as
/// this service's own store — so this service spells no engine downcast of its own.
///
/// The engine keys state by TYPE, so a copy of its own inside this service's store would be
/// invisible to the graph data plane that reads it.
///
/// LOCK ORDER: a writer takes [`super::service_state`] first and this directory second, and
/// nothing here holds this one while taking another store, so the order cannot invert.
///
/// Answer `None` by leaving the directory unwritten: the record is an aid to the data plane,
/// never part of this operation's own answer.
pub fn graph_db_clusters<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::service_common::graph_db_clusters::GraphDbClusters> {
    ::service_common::graph_db_clusters::GraphDbClusters::of(engine)
}
