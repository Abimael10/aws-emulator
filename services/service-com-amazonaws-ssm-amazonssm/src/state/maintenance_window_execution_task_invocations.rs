// SPDX-License-Identifier: CC0-1.0

//! The INVOCATIONS of one task execution of a maintenance window run: one record per
//! target the task execution reached, reached by the triple
//! `(WindowExecutionId, TaskExecutionId, InvocationId)`.
//!
//! "Retrieves information about a specific task running on a specific target"
//! (`smithy_query(query="doc",shape="GetMaintenanceWindowExecutionTaskInvocation")`): a
//! read resolves that triple against the invocations THIS store holds for the Region it was
//! asked through
//! ([`super::ComAmazonawsSsmAmazonssmState::maintenance_window_execution_task_invocation`])
//! and reports what that record holds. An id no record is held under names no invocation at
//! all, which the modelled `DoesNotExistException` reports in the one sentence this service
//! refuses an id the data store holds nothing under with
//! ([`super::maintenance_window_executions::WINDOW_EXECUTION_DOES_NOT_EXIST`]) — while the
//! LISTING of the same records reports a task execution that reached no target as holding
//! none, the reading its sibling listing takes
//! ([`super::maintenance_window_execution_tasks`]).
//!
//! What PUTS a record here is a maintenance window task running against a target, which is
//! the Systems Manager service's own work on the nodes that task targets and which this
//! emulator runs no agent for — so nothing is invented for work that did not happen, and
//! every read of this store is free of side effects.

use std::time::SystemTime;

use crate::ops::types::{
    GetMaintenanceWindowExecutionTaskInvocationOutput,
    MaintenanceWindowExecutionTaskInvocationIdentity,
};

use super::ComAmazonawsSsmAmazonssmState;

/// One invocation of one task execution against one target, as this store holds it.
///
/// The record is the whole of what a read of the invocation reports: which run and task
/// execution it belongs to, what ran, against which target, the status it reached and when
/// it started and — once it is over — ended.
#[derive(Debug, Clone)]
pub struct MaintenanceWindowExecutionTaskInvocationRecord {
    /// The Region the window — and so this run of it — lives in, the first part of the store
    /// key.
    pub region: String,
    /// "The maintenance window execution ID"
    /// (`GetMaintenanceWindowExecutionTaskInvocationResult$WindowExecutionId`), the run this
    /// invocation belongs to.
    pub window_execution_id: String,
    /// "The task execution ID" of the task execution this invocation belongs to
    /// ([`super::maintenance_window_execution_tasks`]).
    pub task_execution_id: String,
    /// "The invocation ID", the last part of the store key.
    pub invocation_id: String,
    /// "The execution ID", which is "the ID of the action performed in the service that
    /// actually handled the task invocation" — the Run Command command id, the Automation
    /// execution id or whatever the task type raised — or `None` for a record that holds
    /// none.
    pub execution_id: Option<String>,
    /// "Retrieves the task type for a maintenance window", one of the modelled
    /// `MaintenanceWindowTaskType` values
    /// (`crate::ops::types::MAINTENANCE_WINDOW_TASK_TYPE_VALUES`).
    pub task_type: String,
    /// "The parameters used at the time that the task ran", held exactly as the run stated
    /// them, or `None` for a record that holds none.
    pub parameters: Option<String>,
    /// "The task status for an invocation", one of the modelled
    /// `MaintenanceWindowExecutionStatus` values.
    pub status: String,
    /// "The details explaining the status. Details are only available for certain status
    /// values", so `None` for a status that explains itself.
    pub status_details: Option<String>,
    /// "The time that the task started running on the target".
    pub started_at: SystemTime,
    /// "The time that the task finished running on the target", or `None` for one that has
    /// not finished.
    pub ended_at: Option<SystemTime>,
    /// "User-provided value to be included in any Amazon CloudWatch Events or Amazon EventBridge
    /// events raised while running tasks for these targets in this maintenance window", or
    /// `None`.
    pub owner_information: Option<String>,
    /// "The maintenance window target ID" this invocation ran against
    /// ([`super::maintenance_window_targets`]), or `None` for a task that targeted nodes
    /// directly.
    pub window_target_id: Option<String>,
}

impl MaintenanceWindowExecutionTaskInvocationRecord {
    /// This invocation as the single read of one reports it.
    ///
    /// Every member comes off the record: the two instants are stated at the resolution this
    /// service states an instant at ([`super::commands::wire_instant`]) and an invocation
    /// that has not finished reports no end.
    #[must_use]
    pub fn to_get_output(&self) -> GetMaintenanceWindowExecutionTaskInvocationOutput {
        GetMaintenanceWindowExecutionTaskInvocationOutput {
            window_execution_id: Some(self.window_execution_id.clone()),
            task_execution_id: Some(self.task_execution_id.clone()),
            invocation_id: Some(self.invocation_id.clone()),
            execution_id: self.execution_id.clone(),
            task_type: Some(self.task_type.clone()),
            parameters: self.parameters.clone(),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
            owner_information: self.owner_information.clone(),
            window_target_id: self.window_target_id.clone(),
        }
    }

    /// This invocation as a LISTING of one task execution's invocations reports it: the
    /// identity members of the same record, which is the subset
    /// `MaintenanceWindowExecutionTaskInvocationIdentity` declares — the same members this
    /// read reports.
    #[must_use]
    pub fn to_identity(&self) -> MaintenanceWindowExecutionTaskInvocationIdentity {
        MaintenanceWindowExecutionTaskInvocationIdentity {
            window_execution_id: Some(self.window_execution_id.clone()),
            task_execution_id: Some(self.task_execution_id.clone()),
            invocation_id: Some(self.invocation_id.clone()),
            execution_id: self.execution_id.clone(),
            task_type: Some(self.task_type.clone()),
            parameters: self.parameters.clone(),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
            owner_information: self.owner_information.clone(),
            window_target_id: self.window_target_id.clone(),
        }
    }

    /// Whether the filter `key`=`values` selects this invocation.
    ///
    /// The one documented key narrows by the status the invocation reached, and a filter's
    /// values UNION — the same rule the sibling listing of a run's task executions follows,
    /// since the two listings document the one filter key
    /// ([`super::maintenance_window_execution_tasks::EXECUTION_STATUS_FILTER_KEYS`]).
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        match key {
            "STATUS" => values.iter().any(|value| value == &self.status),
            _ => false,
        }
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The invocation `invocation_id` names within the task execution `task_execution_id` of
    /// the run `window_execution_id` in `region`, or `None` when that Region holds none
    /// under that triple.
    ///
    /// All three ids are part of the question: an invocation belongs to ONE task execution of
    /// ONE run, so the same invocation id under another task execution names no invocation of
    /// this one.
    #[must_use]
    pub fn maintenance_window_execution_task_invocation(
        &self,
        region: &str,
        window_execution_id: &str,
        task_execution_id: &str,
        invocation_id: &str,
    ) -> Option<&MaintenanceWindowExecutionTaskInvocationRecord> {
        self.maintenance_window_execution_task_invocations.get(&(
            region.to_string(),
            window_execution_id.to_string(),
            task_execution_id.to_string(),
            invocation_id.to_string(),
        ))
    }

    /// Every invocation the task execution `task_execution_id` of the run
    /// `window_execution_id` holds in `region`, in the order they started — the order a
    /// listing reports them in, with the invocation id breaking a tie so the order is total
    /// and a page can resume at an id.
    #[must_use]
    pub fn maintenance_window_execution_task_invocations_of(
        &self,
        region: &str,
        window_execution_id: &str,
        task_execution_id: &str,
    ) -> Vec<&MaintenanceWindowExecutionTaskInvocationRecord> {
        let mut held: Vec<&MaintenanceWindowExecutionTaskInvocationRecord> = self
            .maintenance_window_execution_task_invocations
            .iter()
            .filter(|((region_of, execution_of, task_of, _), _)| {
                region_of == region
                    && execution_of == window_execution_id
                    && task_of == task_execution_id
            })
            .map(|(_, invocation)| invocation)
            .collect();
        held.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.invocation_id.cmp(&right.invocation_id))
        });
        held
    }

    /// Record `record` as an invocation of its own task execution, keyed by the Region, the
    /// run, the task execution and the id the invocation ran under.
    pub fn insert_maintenance_window_execution_task_invocation(
        &mut self,
        record: MaintenanceWindowExecutionTaskInvocationRecord,
    ) {
        let key = (
            record.region.clone(),
            record.window_execution_id.clone(),
            record.task_execution_id.clone(),
            record.invocation_id.clone(),
        );
        self.maintenance_window_execution_task_invocations
            .insert(key, record);
    }
}

#[cfg(test)]
mod tests {
    //! The rules this module owns, over a SET of inputs rather than one record. What the two
    //! reads of this surface answer over the wire is driven through real requests in
    //! `tests/all/maintenance_window_execution_tasks.rs`.

    use std::time::Duration;

    use super::*;

    /// One row of [`a_read_reports_what_the_record_holds`]: what a failing assertion calls it,
    /// and how that row's record differs from the base one.
    type RecordVariantRow = (
        &'static str,
        fn(&mut MaintenanceWindowExecutionTaskInvocationRecord),
    );

    /// An instant as a clock reading, which the rows below measure from.
    fn epoch(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// A record of one invocation, so a row can differ from it in one member.
    fn record(
        region: &str,
        window_execution_id: &str,
        task_execution_id: &str,
        invocation_id: &str,
    ) -> MaintenanceWindowExecutionTaskInvocationRecord {
        MaintenanceWindowExecutionTaskInvocationRecord {
            region: region.to_string(),
            window_execution_id: window_execution_id.to_string(),
            task_execution_id: task_execution_id.to_string(),
            invocation_id: invocation_id.to_string(),
            execution_id: None,
            task_type: "RUN_COMMAND".to_string(),
            parameters: None,
            status: super::super::maintenance_window_executions::EXECUTION_STATUS_PENDING
                .to_string(),
            status_details: None,
            started_at: epoch(1_893_456_000),
            ended_at: None,
            owner_information: None,
            window_target_id: None,
        }
    }

    /// An invocation is resolved by `(region, run, task execution, invocation id)` and by
    /// nothing else: the rows differ along each part of that key.
    #[test]
    fn an_invocation_resolves_within_its_own_task_execution() {
        let region = "us-west-2";
        let run = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let task = "11111111-2222-3333-4444-555555555555";
        let invocation = "66666666-7777-8888-9999-aaaaaaaaaaaa";
        let mut store = ComAmazonawsSsmAmazonssmState::default();
        store.insert_maintenance_window_execution_task_invocation(record(
            region, run, task, invocation,
        ));

        // (case, region, run, task execution, invocation, resolves)
        let rows: [(&str, &str, &str, &str, &str, bool); 5] = [
            (
                "the triple it was recorded under",
                region,
                run,
                task,
                invocation,
                true,
            ),
            (
                "another Region's endpoint",
                "eu-central-1",
                run,
                task,
                invocation,
                false,
            ),
            (
                "another run of the same Region",
                region,
                "ffffffff-ffff-ffff-ffff-ffffffffffff",
                task,
                invocation,
                false,
            ),
            (
                "another task execution of the same run",
                region,
                run,
                "99999999-9999-9999-9999-999999999999",
                invocation,
                false,
            ),
            (
                "an invocation id nothing recorded",
                region,
                run,
                task,
                "00000000-0000-0000-0000-000000000000",
                false,
            ),
        ];

        for (case, region, run, task, invocation, resolves) in rows {
            let found = store
                .maintenance_window_execution_task_invocation(region, run, task, invocation);
            assert_eq!(
                found.is_some(),
                resolves,
                "case {case:?}: expected resolves={resolves}, got {found:?}"
            );
        }
    }

    /// What a read reports comes off the record, and the listing's identity reports the same
    /// values. The rows differ along whether the invocation has finished, what the service
    /// that handled it raised, and what it ran against.
    #[test]
    fn a_read_reports_what_the_record_holds() {
        // (case, how the row differs from the base record)
        let rows: [RecordVariantRow; 4] = [
            ("an invocation nothing has picked up", |_| {}),
            ("one that has finished", |held| {
                held.ended_at = Some(epoch(1_893_456_120));
                held.status = "SUCCESS".to_string();
            }),
            ("one the handling service raised an id for", |held| {
                held.execution_id = Some("a1b2c3d4-5e6f-7a8b-9c0d-1e2f3a4b5c6d".to_string());
                held.parameters = Some("{\"commands\":[\"echo hello\"]}".to_string());
            }),
            ("one that ran against a registered target", |held| {
                held.window_target_id = Some("22222222-3333-4444-5555-666666666666".to_string());
                held.owner_information = Some("patch wave 1".to_string());
                held.status = "FAILED".to_string();
                held.status_details = Some("The step timed out".to_string());
            }),
        ];

        for (case, differ) in rows {
            let mut held = record(
                "us-west-2",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "11111111-2222-3333-4444-555555555555",
                "66666666-7777-8888-9999-aaaaaaaaaaaa",
            );
            differ(&mut held);
            let reported = held.to_get_output();
            assert_eq!(
                reported.invocation_id.as_deref(),
                Some(held.invocation_id.as_str()),
                "case {case:?}: expected the id the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.task_execution_id.as_deref(),
                Some(held.task_execution_id.as_str()),
                "case {case:?}: expected the task execution the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.execution_id, held.execution_id,
                "case {case:?}: expected the handling id the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.parameters, held.parameters,
                "case {case:?}: expected the parameters the record holds, got {reported:?}"
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
                reported.owner_information, held.owner_information,
                "case {case:?}: expected the owner information the record holds, got \
                 {reported:?}"
            );
            assert_eq!(
                reported.window_target_id, held.window_target_id,
                "case {case:?}: expected the target the record holds, got {reported:?}"
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

            let identity = held.to_identity();
            assert_eq!(
                identity.invocation_id.as_deref(),
                Some(held.invocation_id.as_str()),
                "case {case:?}: expected the id the record holds, got {identity:?}"
            );
            assert_eq!(
                identity.status.as_deref(),
                Some(held.status.as_str()),
                "case {case:?}: expected the status the record holds, got {identity:?}"
            );
            assert_eq!(
                identity.window_target_id, held.window_target_id,
                "case {case:?}: expected the target the record holds, got {identity:?}"
            );
        }
    }

    /// A listing of one task execution's invocations walks the records of THAT task
    /// execution, in the order they started, and reaches no other's.
    #[test]
    fn a_listing_walks_the_invocations_of_one_task_execution() {
        let region = "us-west-2";
        let run = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let task = "11111111-2222-3333-4444-555555555555";
        let mut store = ComAmazonawsSsmAmazonssmState::default();

        let mut later = record(region, run, task, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        later.started_at = epoch(1_893_456_060);
        store.insert_maintenance_window_execution_task_invocation(later);
        store.insert_maintenance_window_execution_task_invocation(record(
            region,
            run,
            task,
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
        ));
        store.insert_maintenance_window_execution_task_invocation(record(
            region,
            run,
            "99999999-9999-9999-9999-999999999999",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
        ));

        let listed: Vec<&str> = store
            .maintenance_window_execution_task_invocations_of(region, run, task)
            .into_iter()
            .map(|invocation| invocation.invocation_id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            ],
            "expected this task execution's invocations in the order they started, got \
             {listed:?}"
        );

        let none = store.maintenance_window_execution_task_invocations_of(
            region,
            run,
            "00000000-0000-0000-0000-000000000000",
        );
        assert!(
            none.is_empty(),
            "expected no invocation for a task execution nothing recorded, got {none:?}"
        );
    }

    /// The one documented filter key narrows by the status the record holds, and its values
    /// UNION; a key outside the documented set selects nothing.
    #[test]
    fn the_status_filter_narrows_by_the_status_the_record_holds() {
        let mut held = record(
            "us-west-2",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "11111111-2222-3333-4444-555555555555",
            "66666666-7777-8888-9999-aaaaaaaaaaaa",
        );
        held.status = "CANCELLED".to_string();

        // (case, key, values, selects)
        let rows: [(&str, &str, &[&str], bool); 4] = [
            ("the status it holds", "STATUS", &["CANCELLED"], true),
            (
                "the status it holds among others",
                "STATUS",
                &["CANCELLING", "CANCELLED"],
                true,
            ),
            ("another status", "STATUS", &["IN_PROGRESS"], false),
            (
                "a key outside the documented set",
                "InvocationId",
                &["66666666-7777-8888-9999-aaaaaaaaaaaa"],
                false,
            ),
        ];

        for (case, key, values, selects) in rows {
            let values: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
            assert_eq!(
                held.matches_filter(key, &values),
                selects,
                "case {case:?}: expected selects={selects} for status {:?}",
                held.status
            );
        }
    }
}
