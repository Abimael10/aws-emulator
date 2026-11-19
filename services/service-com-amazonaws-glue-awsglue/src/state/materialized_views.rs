// SPDX-License-Identifier: CC0-1.0

//! The refresh task runs of a materialized view: the record one run is, and how a read
//! addresses it.
//!
//! A run lives BESIDE the catalog tree rather than on the view it refreshes, exactly as a
//! column statistics task run lives beside the table it covers
//! ([`super::column_statistics`]): `GetMaterializedViewRefreshTaskRun` addresses one by the
//! `MaterializedViewRefreshTaskRunId` a start minted, so the id is the whole address and
//! the view a run refreshes is recorded ON the run.

use super::*;

/// One materialized view refresh task run, as a start of that view recorded it.
///
/// The run carries the view it refreshes — the catalog, database and table a request named
/// it by — and the members only a run has: its id, the status it has reached, the instants
/// it moved between statuses and what its work consumed.
#[derive(Debug, Clone)]
pub struct MaterializedViewRefreshTaskRunEntry {
    /// `MaterializedViewRefreshTaskRun$MaterializedViewRefreshTaskRunId`: the `UUIDv4` the
    /// start minted, which is the whole address of this run.
    pub id: String,
    /// The catalog the refreshed view lives in, as [`resolve_catalog_id`] resolved it.
    pub catalog_id: String,
    /// The refreshed view's database, folded the way the catalog stores a database name.
    pub database_name: String,
    /// The refreshed view's own name, folded the way the catalog stores a table name.
    pub table_name: String,
    /// `Role`: "The IAM role that the service assumes to run the materialized view refresh
    /// task."
    pub role: Option<String>,
    /// One of the modelled `MaterializedViewRefreshState` values
    /// ([`crate::ops::types::MATERIALIZED_VIEW_REFRESH_STATE_VALUES`]).
    pub status: String,
    /// One of the modelled `MaterializedViewRefreshType` values: "Either FULL or
    /// INCREMENTAL" ([`crate::ops::types::MATERIALIZED_VIEW_REFRESH_TYPE_VALUES`]).
    pub refresh_type: Option<String>,
    /// `CreationTime`: "The time that this task was created."
    pub creation_time: SystemTime,
    /// `StartTime`: "The start time of the task."
    pub start_time: SystemTime,
    /// `LastUpdated`: "The last point in time when this task was modified."
    pub last_updated: SystemTime,
    /// `EndTime`: "The end time of the task", absent until the run has finished.
    pub end_time: Option<SystemTime>,
    /// `ErrorMessage`: "The error message for the job", absent until one has failed.
    pub error_message: Option<String>,
    /// `DPUSeconds`: "The calculated DPU usage in seconds for all autoscaled workers."
    pub dpu_seconds: f64,
    /// `ProcessedBytes`: "The number of bytes the refresh task run has scanned to refresh
    /// the materialized view."
    pub processed_bytes: Option<i64>,
}

impl MaterializedViewRefreshTaskRunEntry {
    /// Rebuild the modelled `MaterializedViewRefreshTaskRun` output shape from what is
    /// stored.
    pub fn to_materialized_view_refresh_task_run(&self) -> MaterializedViewRefreshTaskRun {
        MaterializedViewRefreshTaskRun {
            // "The Amazon Web Services account ID" the run belongs to, which is the account
            // this endpoint serves — the only one whose runs this store can hold.
            customer_id: Some(::engine::emulator_account_id().to_string()),
            materialized_view_refresh_task_run_id: Some(self.id.clone()),
            database_name: Some(self.database_name.clone()),
            table_name: Some(self.table_name.clone()),
            catalog_id: Some(self.catalog_id.clone()),
            role: self.role.clone(),
            status: Some(self.status.clone()),
            creation_time: Some(to_date_time(self.creation_time)),
            last_updated: Some(to_date_time(self.last_updated)),
            start_time: Some(to_date_time(self.start_time)),
            end_time: self.end_time.map(to_date_time),
            error_message: self.error_message.clone(),
            dpu_seconds: self.dpu_seconds,
            refresh_type: self.refresh_type.clone(),
            processed_bytes: self.processed_bytes,
        }
    }
}

/// How a materialized view refresh task run is addressed.
impl ComAmazonawsGlueAwsglueState {
    /// The stored materialized view refresh task run with this id IN the Data Catalog
    /// `catalog_id` addresses, or `None` when that catalog holds no such run.
    ///
    /// A run is ADDRESSED by the id the start that asked for it minted and the catalog the
    /// refreshed view lives in — `GetMaterializedViewRefreshTaskRun` takes a `@required`
    /// `CatalogId` beside that id — so the catalog is part of the address and a run of
    /// ANOTHER catalog is not an answer to this one: the walk matches both.
    pub fn materialized_view_refresh_task_run(
        &self,
        catalog_id: &str,
        id: &str,
    ) -> Option<&MaterializedViewRefreshTaskRunEntry> {
        self.materialized_view_refresh_task_runs
            .iter()
            .find(|run| run.id == id && run.catalog_id == catalog_id)
    }
}
