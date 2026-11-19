// SPDX-License-Identifier: CC0-1.0

//! The EXECUTIONS of a maintenance window: one record per run of a window, reached by the
//! id that run was executed under.
//!
//! "Retrieves details about a specific a maintenance window execution"
//! (`smithy_query(query="doc",shape="GetMaintenanceWindowExecution")`): a read resolves a
//! `WindowExecutionId` against the executions THIS store holds for the Region it was asked
//! through ([`super::ComAmazonawsSsmAmazonssmState::maintenance_window_execution`]) and
//! reports what that record holds — the window's own id space is a different one, so a
//! window id names no execution and an execution id names no window. An id no record is
//! held under names no execution at all, which is what the modelled
//! `DoesNotExistException` reports ([`WINDOW_EXECUTION_DOES_NOT_EXIST`]), and the read
//! writes nothing, so two callers see the same executions whether or not either of them
//! called.
//!
//! What PUTS a record here is a window's tasks being picked up and run. Running them is the
//! Systems Manager service's own work on the nodes a task targets, and this emulator runs
//! no agent on any node — the same reading Run Command takes of a command's invocations,
//! which are minted `Pending` and stay there because nothing picks them up
//! ([`super::commands`]) — so no run of a window is recorded here and nothing is invented
//! for one: an account whose windows have never run holds no execution, and every id a
//! caller can name is refused, exactly as a Region that has never run a window answers. A
//! record admitted here is reported from what it holds, and from nothing derived about work
//! that did not happen.

use std::time::SystemTime;

use crate::ops::types::GetMaintenanceWindowExecutionOutput;

use super::ComAmazonawsSsmAmazonssmState;

/// How this service states a `WindowExecutionId` that names no execution reachable through
/// the endpoint the read was asked through.
///
/// `DoesNotExistException`'s own account of its class is "the ID specified for a resource,
/// such as a maintenance window or patch baseline, doesn't exist"
/// (`smithy_query(query="doc",shape="DoesNotExistException")`), and this refusal is worded
/// for the DATA STORE the id was looked for in rather than for the id itself: fixed text
/// with no variable part, so nothing of the request is echoed into it.
pub const WINDOW_EXECUTION_DOES_NOT_EXIST: &str = "Resource does not exist in the data store.";

/// The `MaintenanceWindowExecutionStatus` a run that has been scheduled but whose tasks
/// nothing has picked up yet carries: "PENDING", from the modelled enum
/// (`crate::ops::types::MAINTENANCE_WINDOW_EXECUTION_STATUS_VALUES`).
pub const EXECUTION_STATUS_PENDING: &str = "PENDING";

/// One execution of one maintenance window, as this store holds it.
///
/// The record is the whole of what a read of the execution reports: its id, the window it
/// is a run of, when it started and — once the run is over — when it ended, the status it
/// has reached and the task executions it carries. Nothing is derived from the clock here,
/// because a run's progress is reported by the work that runs it rather than by how long
/// ago it started.
#[derive(Debug, Clone)]
pub struct MaintenanceWindowExecutionRecord {
    /// "The ID of the maintenance window execution"
    /// (`GetMaintenanceWindowExecutionResult$WindowExecutionId`), which is the key this
    /// record is held under.
    pub window_execution_id: String,
    /// The `WindowId` of the maintenance window this is a run of.
    pub window_id: String,
    /// The Region the window — and so this run of it — lives in, which is the other half of
    /// the store key.
    pub region: String,
    /// "The time the maintenance window started running"
    /// (`GetMaintenanceWindowExecutionResult$StartTime`).
    pub started_at: SystemTime,
    /// "The time the maintenance window finished running", or `None` for a run that has not
    /// finished.
    pub ended_at: Option<SystemTime>,
    /// "The status of the maintenance window execution", one of the modelled
    /// `MaintenanceWindowExecutionStatus` values.
    pub status: String,
    /// "The details explaining the status. Not available for all status values", so `None`
    /// for a status that explains itself.
    pub status_details: Option<String>,
    /// "The ID of the task executions from the maintenance window execution"
    /// (`GetMaintenanceWindowExecutionResult$TaskIds`), in the order the run reached them.
    pub task_execution_ids: Vec<String>,
}

impl MaintenanceWindowExecutionRecord {
    /// This execution as the single read of one reports it.
    ///
    /// Every member comes off the record: the two instants are stated at the resolution
    /// this service states an instant at ([`super::commands::wire_instant`]), a run that
    /// has not finished reports no end and a status that explains itself reports no detail.
    #[must_use]
    pub fn to_get_output(&self) -> GetMaintenanceWindowExecutionOutput {
        GetMaintenanceWindowExecutionOutput {
            window_execution_id: Some(self.window_execution_id.clone()),
            task_ids: Some(self.task_execution_ids.clone()),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
        }
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The execution `window_execution_id` names in `region`, or `None` when that Region
    /// holds none under that id — which is every id until something records a run.
    ///
    /// A maintenance window is reached through the endpoint of ONE Region, so a run of it is
    /// reached through that same endpoint: an execution of another Region's window is no
    /// execution of this one.
    #[must_use]
    pub fn maintenance_window_execution(
        &self,
        region: &str,
        window_execution_id: &str,
    ) -> Option<&MaintenanceWindowExecutionRecord> {
        self.maintenance_window_executions
            .get(&(region.to_string(), window_execution_id.to_string()))
    }

    /// Record `record` as a run of its window, keyed by the id it was executed under.
    pub fn insert_maintenance_window_execution(
        &mut self,
        record: MaintenanceWindowExecutionRecord,
    ) {
        let key = (record.region.clone(), record.window_execution_id.clone());
        self.maintenance_window_executions.insert(key, record);
    }
}

#[cfg(test)]
mod tests {
    //! The rules this module owns, over a SET of inputs rather than one id. The refusal a
    //! read answers an id this store holds nothing under is driven through real requests in
    //! `tests/all/maintenance_window_executions.rs`.

    use std::time::Duration;

    use super::*;

    /// One row of [`a_read_reports_what_the_record_holds`]: what a failing assertion calls
    /// it, and how that row's record differs from the base one.
    type RecordVariantRow = (&'static str, fn(&mut MaintenanceWindowExecutionRecord));

    /// An instant as a clock reading, which the rows below measure from.
    fn epoch(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// A record of one run, so a row can differ from it in one member.
    fn record(region: &str, window_execution_id: &str) -> MaintenanceWindowExecutionRecord {
        MaintenanceWindowExecutionRecord {
            window_execution_id: window_execution_id.to_string(),
            window_id: "mw-0123456789abcdef0".to_string(),
            region: region.to_string(),
            started_at: epoch(1_893_456_000),
            ended_at: None,
            status: EXECUTION_STATUS_PENDING.to_string(),
            status_details: None,
            task_execution_ids: Vec::new(),
        }
    }

    /// An execution is resolved by `(region, window execution id)` and by nothing else: the
    /// rows differ along the Region asked through and the id asked for.
    #[test]
    fn an_execution_resolves_through_the_endpoint_of_its_own_region() {
        let held_region = "us-west-2";
        let held_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let mut store = ComAmazonawsSsmAmazonssmState::default();
        store.insert_maintenance_window_execution(record(held_region, held_id));

        // (case, region, id, resolves)
        let rows: [(&str, &str, &str, bool); 5] = [
            ("the id it was recorded under", held_region, held_id, true),
            (
                "another Region's endpoint",
                "eu-central-1",
                held_id,
                false,
            ),
            (
                "an id nothing recorded",
                held_region,
                "ffffffff-ffff-ffff-ffff-ffffffffffff",
                false,
            ),
            (
                "the id in another case",
                held_region,
                "AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE",
                false,
            ),
            (
                "the window's own id",
                held_region,
                "mw-0123456789abcdef0",
                false,
            ),
        ];

        for (case, region, id, resolves) in rows {
            let found = store.maintenance_window_execution(region, id);
            assert_eq!(
                found.is_some(),
                resolves,
                "case {case:?}: expected resolves={resolves}, got {found:?}"
            );
        }
    }

    /// What a read reports comes off the record, member by member: the rows differ along
    /// whether the run has finished, whether its status explains itself and how many task
    /// executions it carries.
    #[test]
    fn a_read_reports_what_the_record_holds() {
        // (case, how the row differs from the base record)
        let rows: [RecordVariantRow; 4] = [
            ("a run nothing has picked up", |_| {}),
            ("a run that has finished", |held| {
                held.ended_at = Some(epoch(1_893_459_600));
                held.status = "SUCCESS".to_string();
            }),
            ("a status with a detail", |held| {
                held.status = "FAILED".to_string();
                held.status_details = Some("One or more tasks failed".to_string());
            }),
            ("a run carrying task executions", |held| {
                held.task_execution_ids = vec![
                    "11111111-2222-3333-4444-555555555555".to_string(),
                    "66666666-7777-8888-9999-aaaaaaaaaaaa".to_string(),
                ];
            }),
        ];

        for (case, differ) in rows {
            let mut held = record("us-west-2", "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
            differ(&mut held);
            let reported = held.to_get_output();
            assert_eq!(
                reported.window_execution_id.as_deref(),
                Some(held.window_execution_id.as_str()),
                "case {case:?}: expected the id the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.status.as_deref(),
                Some(held.status.as_str()),
                "case {case:?}: expected the status the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.status_details, held.status_details,
                "case {case:?}: expected the detail the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.task_ids.as_deref(),
                Some(held.task_execution_ids.as_slice()),
                "case {case:?}: expected the task executions the record holds, got \
                 {reported:?}"
            );
            assert_eq!(
                reported.start_time,
                Some(super::super::commands::wire_instant(held.started_at)),
                "case {case:?}: expected the instant the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.end_time,
                held.ended_at.map(super::super::commands::wire_instant),
                "case {case:?}: expected the end the record holds, got {reported:?}"
            );
        }
    }
}
