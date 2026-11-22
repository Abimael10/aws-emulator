// SPDX-License-Identifier: CC0-1.0

//! The TASK EXECUTIONS of one run of a maintenance window: one record per task the run
//! reached, reached by the pair `(WindowExecutionId, TaskExecutionId)`.
//!
//! "Retrieves the details about a specific task run as part of a maintenance window
//! execution" (`smithy_query(query="doc",shape="GetMaintenanceWindowExecutionTask")`): a
//! read resolves that pair against the task executions THIS store holds for the Region it
//! was asked through ([`super::ComAmazonawsSsmAmazonssmState::maintenance_window_execution_task`])
//! and reports what that record holds. A registered task's `WindowTaskId` and the
//! `TaskExecutionId` a run of it is reached by are different id spaces, so neither names
//! the other, and an id no record is held under names no task execution at all — which is
//! what the modelled `DoesNotExistException` reports, in the one sentence this service
//! refuses an id the data store holds nothing under with
//! ([`super::maintenance_window_executions::WINDOW_EXECUTION_DOES_NOT_EXIST`]).
//!
//! The LISTING of the same records is a different reading of the same absence:
//! `DescribeMaintenanceWindowExecutionTasks` reports the task executions of one run, so a
//! run that reached none — including an execution id nothing was ever recorded under — is
//! reported as holding none (HTTP 200 with an empty identity list) rather than refused. A
//! listing reports the EXISTENCE of nothing, which an empty list states exactly; a read of
//! ONE record has nothing to report, which is why only the read refuses.
//!
//! What PUTS a record here is a window's tasks being picked up and run, which is the
//! Systems Manager service's own work on the nodes a task targets and which this emulator
//! runs no agent for — the same reading its sibling store takes of a window's runs
//! ([`super::maintenance_window_executions`]) and Run Command takes of a command's
//! invocations ([`super::commands`]). So no task execution is invented for work that did
//! not happen: a record admitted here is reported from what it holds and from nothing else,
//! and every read is free of side effects, so two callers see the same task executions
//! whether or not either of them called.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{
    AlarmConfiguration, AlarmStateInformation, GetMaintenanceWindowExecutionTaskOutput,
    MaintenanceWindowExecutionTaskIdentity, MaintenanceWindowTaskParameterValueExpression,
};

use super::ComAmazonawsSsmAmazonssmState;

/// The filter keys a describe of a run's task executions — or of one task execution's
/// invocations — may narrow by: "the supported filter key is `STATUS` with the
/// corresponding values `PENDING`, `IN_PROGRESS`, `SUCCESS`, `FAILED`, `TIMED_OUT`,
/// `CANCELLING`, and `CANCELLED`"
/// (`DescribeMaintenanceWindowExecutionTasksRequest$Filters`, worded identically on
/// `DescribeMaintenanceWindowExecutionTaskInvocationsRequest$Filters`). What a refusal of
/// another key lists, and the one place that set is written — both listings narrow by it,
/// so [`super::maintenance_window_execution_task_invocations`] reads it from here.
pub const EXECUTION_STATUS_FILTER_KEYS: &[&str] = &["STATUS"];

/// Whether `key` is one of the filter keys those listings take
/// ([`EXECUTION_STATUS_FILTER_KEYS`]).
#[must_use]
pub fn is_execution_status_filter_key(key: &str) -> bool {
    EXECUTION_STATUS_FILTER_KEYS.contains(&key)
}

/// The page size a describe of a run's task executions, or of one task execution's
/// invocations, serves (see `super::page_size`).
///
/// Both listings bound a page by the same modelled `MaintenanceWindowMaxResults`
/// (`range 10..100`) the describes of a window's own tasks do, so the bounds are read from
/// where that shape's range is already written
/// ([`super::maintenance_window_tasks::MIN_TASK_PAGE_SIZE`]) rather than copied.
pub fn execution_task_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        super::maintenance_window_tasks::MIN_TASK_PAGE_SIZE,
        super::maintenance_window_tasks::MAX_TASK_PAGE_SIZE,
    )
}

/// One task execution of one run of a maintenance window, as this store holds it.
///
/// The record is the whole of what a read of the task execution reports: which run it
/// belongs to, which task ran, with what bounds and parameters, the status it reached and
/// when it started and — once it is over — ended. Nothing is derived from the clock, because
/// a task execution's progress is reported by the work that runs it rather than by how long
/// ago it started.
#[derive(Debug, Clone)]
pub struct MaintenanceWindowExecutionTaskRecord {
    /// The Region the window — and so this run of it — lives in, which is part of the store
    /// key.
    pub region: String,
    /// "The ID of the maintenance window execution that includes the task"
    /// (`GetMaintenanceWindowExecutionTaskResult$WindowExecutionId`), the run this task
    /// execution belongs to and the second part of the store key.
    pub window_execution_id: String,
    /// "The ID of the specific task execution in the maintenance window task that was
    /// retrieved" (`GetMaintenanceWindowExecutionTaskResult$TaskExecutionId`), the last part
    /// of the store key.
    pub task_execution_id: String,
    /// The `WindowTaskId` of the REGISTERED task this is a run of
    /// ([`super::maintenance_window_tasks`]), or `None` for a record held for a task that is
    /// no longer registered. A different id space from [`Self::task_execution_id`], so it is
    /// held rather than derived.
    pub window_task_id: Option<String>,
    /// "The ARN of the task that ran" (`GetMaintenanceWindowExecutionTaskResult$TaskArn`).
    pub task_arn: String,
    /// "The type of task that was run", one of the modelled `MaintenanceWindowTaskType`
    /// values (`crate::ops::types::MAINTENANCE_WINDOW_TASK_TYPE_VALUES`).
    pub task_type: String,
    /// "The role that was assumed when running the task", or `None` for a record that holds
    /// none.
    pub service_role: Option<String>,
    /// "The parameters passed to the task when it was run", in the order the run passed them.
    /// Each entry is one invocation's parameter map, ordered so two reads of one task
    /// execution report it the same way.
    pub task_parameters: Vec<BTreeMap<String, MaintenanceWindowTaskParameterValueExpression>>,
    /// "The priority of the task", from the modelled default of 0 upwards.
    pub priority: i32,
    /// "The defined maximum number of task executions that could be run in parallel", or
    /// `None` for a record that holds none.
    pub max_concurrency: Option<String>,
    /// "The defined maximum number of task execution errors allowed before scheduling of the
    /// task execution would have been stopped", or `None`.
    pub max_errors: Option<String>,
    /// "The status of the task", one of the modelled `MaintenanceWindowExecutionStatus`
    /// values (`crate::ops::types::MAINTENANCE_WINDOW_EXECUTION_STATUS_VALUES`).
    pub status: String,
    /// "The details explaining the status. Not available for all status values", so `None`
    /// for a status that explains itself.
    pub status_details: Option<String>,
    /// "The time the task execution started".
    pub started_at: SystemTime,
    /// "The time the task execution completed", or `None` for one that has not completed.
    pub ended_at: Option<SystemTime>,
    /// "The details for the CloudWatch alarm you applied to your maintenance window task",
    /// or `None`.
    pub alarm_configuration: Option<AlarmConfiguration>,
    /// "The CloudWatch alarms that were invoked by the maintenance window task", empty for a
    /// task execution that invoked none — the modelled list carries at least one entry, so an
    /// empty one is reported as no list at all.
    pub triggered_alarms: Vec<AlarmStateInformation>,
}

impl MaintenanceWindowExecutionTaskRecord {
    /// This task execution as the single read of one reports it.
    ///
    /// Every member comes off the record: the two instants are stated at the resolution this
    /// service states an instant at ([`super::commands::wire_instant`]), a task execution
    /// that has not finished reports no end, and a status that explains itself reports no
    /// detail.
    #[must_use]
    pub fn to_get_output(&self) -> GetMaintenanceWindowExecutionTaskOutput {
        GetMaintenanceWindowExecutionTaskOutput {
            window_execution_id: Some(self.window_execution_id.clone()),
            task_execution_id: Some(self.task_execution_id.clone()),
            task_arn: Some(self.task_arn.clone()),
            service_role: self.service_role.clone(),
            // `GetMaintenanceWindowExecutionTaskResult` names the task's type `Type`.
            r#type: Some(self.task_type.clone()),
            task_parameters: self.reported_task_parameters(),
            priority: self.priority,
            max_concurrency: self.max_concurrency.clone(),
            max_errors: self.max_errors.clone(),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
            alarm_configuration: self.alarm_configuration.clone(),
            triggered_alarms: self.reported_triggered_alarms(),
        }
    }

    /// This task execution as a LISTING of one run's task executions reports it: the
    /// identity members of the same record, which is the subset
    /// `MaintenanceWindowExecutionTaskIdentity` declares.
    #[must_use]
    pub fn to_identity(&self) -> MaintenanceWindowExecutionTaskIdentity {
        MaintenanceWindowExecutionTaskIdentity {
            window_execution_id: Some(self.window_execution_id.clone()),
            task_execution_id: Some(self.task_execution_id.clone()),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
            task_arn: Some(self.task_arn.clone()),
            task_type: Some(self.task_type.clone()),
            alarm_configuration: self.alarm_configuration.clone(),
            triggered_alarms: self.reported_triggered_alarms(),
        }
    }

    /// The task parameters a read reports: the maps the run passed, or nothing at all for a
    /// run that passed none — so a read of a task execution that carried no parameters
    /// reports no empty list nothing produced.
    #[must_use]
    pub fn reported_task_parameters(
        &self,
    ) -> Option<Vec<std::collections::HashMap<String, MaintenanceWindowTaskParameterValueExpression>>>
    {
        if self.task_parameters.is_empty() {
            return None;
        }
        Some(
            self.task_parameters
                .iter()
                .map(|parameters| {
                    parameters
                        .iter()
                        .map(|(name, expression)| (name.clone(), expression.clone()))
                        .collect()
                })
                .collect(),
        )
    }

    /// The alarms a read reports as invoked, or nothing at all when none were: the modelled
    /// `AlarmStateInformationList` carries between one and one entry, so an empty list is no
    /// value of it.
    #[must_use]
    pub fn reported_triggered_alarms(&self) -> Option<Vec<AlarmStateInformation>> {
        if self.triggered_alarms.is_empty() {
            return None;
        }
        Some(self.triggered_alarms.clone())
    }

    /// Whether the filter `key`=`values` selects this task execution.
    ///
    /// The one documented key narrows by the status the task execution reached, and a
    /// filter's values UNION: it is selected when one of that filter's values is its status.
    /// A key outside [`EXECUTION_STATUS_FILTER_KEYS`] selects nothing — its caller refuses
    /// such a key before anything is asked about the records.
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        match key {
            "STATUS" => values.iter().any(|value| value == &self.status),
            _ => false,
        }
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// The task execution `task_execution_id` names within the run `window_execution_id` in
    /// `region`, or `None` when that Region holds none under that pair — which is every pair
    /// until something records a run.
    ///
    /// Both ids are part of the question: a task execution belongs to ONE run, so the same
    /// task execution id under another run names no task execution of this one.
    #[must_use]
    pub fn maintenance_window_execution_task(
        &self,
        region: &str,
        window_execution_id: &str,
        task_execution_id: &str,
    ) -> Option<&MaintenanceWindowExecutionTaskRecord> {
        self.maintenance_window_execution_tasks.get(&(
            region.to_string(),
            window_execution_id.to_string(),
            task_execution_id.to_string(),
        ))
    }

    /// Every task execution the run `window_execution_id` holds in `region`, in the order
    /// they started — the order a listing reports them in, with the task execution id
    /// breaking a tie so the order is total and a page can resume at an id.
    #[must_use]
    pub fn maintenance_window_execution_tasks_of(
        &self,
        region: &str,
        window_execution_id: &str,
    ) -> Vec<&MaintenanceWindowExecutionTaskRecord> {
        let mut held: Vec<&MaintenanceWindowExecutionTaskRecord> = self
            .maintenance_window_execution_tasks
            .iter()
            .filter(|((region_of, execution_of, _), _)| {
                region_of == region && execution_of == window_execution_id
            })
            .map(|(_, task)| task)
            .collect();
        held.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.task_execution_id.cmp(&right.task_execution_id))
        });
        held
    }

    /// Record `record` as a task execution of its own run, keyed by the Region, the run and
    /// the id the task execution ran under.
    pub fn insert_maintenance_window_execution_task(
        &mut self,
        record: MaintenanceWindowExecutionTaskRecord,
    ) {
        let key = (
            record.region.clone(),
            record.window_execution_id.clone(),
            record.task_execution_id.clone(),
        );
        self.maintenance_window_execution_tasks.insert(key, record);
    }
}

#[cfg(test)]
mod tests {
    //! The rules this module owns, over a SET of inputs rather than one record. What the two
    //! reads of this surface answer over the wire is driven through real requests in
    //! `tests/all/maintenance_window_execution_tasks.rs`.

    use std::time::Duration;

    use super::*;

    /// One row of [`a_read_reports_what_the_record_holds`]: what a failing assertion calls
    /// it, and how that row's record differs from the base one.
    type RecordVariantRow = (
        &'static str,
        fn(&mut MaintenanceWindowExecutionTaskRecord),
    );

    /// An instant as a clock reading, which the rows below measure from.
    fn epoch(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// A record of one task execution, so a row can differ from it in one member.
    fn record(
        region: &str,
        window_execution_id: &str,
        task_execution_id: &str,
    ) -> MaintenanceWindowExecutionTaskRecord {
        MaintenanceWindowExecutionTaskRecord {
            region: region.to_string(),
            window_execution_id: window_execution_id.to_string(),
            task_execution_id: task_execution_id.to_string(),
            window_task_id: None,
            task_arn: "AWS-RunShellScript".to_string(),
            task_type: "RUN_COMMAND".to_string(),
            service_role: None,
            task_parameters: Vec::new(),
            priority: 0,
            max_concurrency: None,
            max_errors: None,
            status: super::super::maintenance_window_executions::EXECUTION_STATUS_PENDING
                .to_string(),
            status_details: None,
            started_at: epoch(1_893_456_000),
            ended_at: None,
            alarm_configuration: None,
            triggered_alarms: Vec::new(),
        }
    }

    /// A task execution is resolved by `(region, run, task execution id)` and by nothing
    /// else: the rows differ along the Region asked through, the run asked within and the id
    /// asked for.
    #[test]
    fn a_task_execution_resolves_within_its_own_run_and_region() {
        let held_region = "us-west-2";
        let held_execution = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let held_task = "11111111-2222-3333-4444-555555555555";
        let mut store = ComAmazonawsSsmAmazonssmState::default();
        store.insert_maintenance_window_execution_task(record(
            held_region,
            held_execution,
            held_task,
        ));

        // (case, region, run, task execution id, resolves)
        let rows: [(&str, &str, &str, &str, bool); 6] = [
            (
                "the pair it was recorded under",
                held_region,
                held_execution,
                held_task,
                true,
            ),
            (
                "another Region's endpoint",
                "eu-central-1",
                held_execution,
                held_task,
                false,
            ),
            (
                "another run of the same Region",
                held_region,
                "ffffffff-ffff-ffff-ffff-ffffffffffff",
                held_task,
                false,
            ),
            (
                "a task execution id nothing recorded",
                held_region,
                held_execution,
                "99999999-9999-9999-9999-999999999999",
                false,
            ),
            (
                "the ids the other way round",
                held_region,
                held_task,
                held_execution,
                false,
            ),
            (
                "a neighbouring id, one digit apart",
                held_region,
                held_execution,
                "11111111-2222-3333-4444-555555555556",
                false,
            ),
        ];

        for (case, region, execution, task, resolves) in rows {
            let found = store.maintenance_window_execution_task(region, execution, task);
            assert_eq!(
                found.is_some(),
                resolves,
                "case {case:?}: expected resolves={resolves}, got {found:?}"
            );
        }
    }

    /// What a read reports comes off the record, member by member — and the listing's
    /// identity reports the same values for the members it declares. The rows differ along
    /// whether the task execution has finished, whether its status explains itself, what
    /// bounds it ran under and whether it carried parameters.
    #[test]
    fn a_read_reports_what_the_record_holds() {
        // (case, how the row differs from the base record)
        let rows: [RecordVariantRow; 5] = [
            ("a task execution nothing has picked up", |_| {}),
            ("one that has finished", |held| {
                held.ended_at = Some(epoch(1_893_459_600));
                held.status = "SUCCESS".to_string();
            }),
            ("a status with a detail", |held| {
                held.status = "FAILED".to_string();
                held.status_details = Some("One or more invocations failed".to_string());
            }),
            ("one that ran under bounds and a role", |held| {
                held.max_concurrency = Some("10%".to_string());
                held.max_errors = Some("1".to_string());
                held.service_role = Some("arn:aws:iam::123456789012:role/mw".to_string());
                held.priority = 3;
            }),
            ("one that carried parameters", |held| {
                let mut parameters = BTreeMap::new();
                parameters.insert(
                    "commands".to_string(),
                    MaintenanceWindowTaskParameterValueExpression {
                        values: Some(vec!["echo hello".to_string()]),
                    },
                );
                held.task_parameters = vec![parameters];
            }),
        ];

        for (case, differ) in rows {
            let mut held = record(
                "us-west-2",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                "11111111-2222-3333-4444-555555555555",
            );
            differ(&mut held);
            let reported = held.to_get_output();
            assert_eq!(
                reported.task_execution_id.as_deref(),
                Some(held.task_execution_id.as_str()),
                "case {case:?}: expected the id the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.window_execution_id.as_deref(),
                Some(held.window_execution_id.as_str()),
                "case {case:?}: expected the run the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.r#type.as_deref(),
                Some(held.task_type.as_str()),
                "case {case:?}: expected the task type the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.priority, held.priority,
                "case {case:?}: expected the priority the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.max_concurrency, held.max_concurrency,
                "case {case:?}: expected the concurrency the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.max_errors, held.max_errors,
                "case {case:?}: expected the error bound the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.service_role, held.service_role,
                "case {case:?}: expected the role the record holds, got {reported:?}"
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
                reported.start_time,
                Some(super::super::commands::wire_instant(held.started_at)),
                "case {case:?}: expected the instant the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.end_time,
                held.ended_at.map(super::super::commands::wire_instant),
                "case {case:?}: expected the end the record holds, got {reported:?}"
            );
            assert_eq!(
                reported.task_parameters.map(|held| held.len()),
                if held.task_parameters.is_empty() {
                    None
                } else {
                    Some(held.task_parameters.len())
                },
                "case {case:?}: expected the parameters the record holds"
            );

            // The identity a listing reports carries the same values for the members it
            // declares, so a caller that lists and then reads sees one task execution.
            let identity = held.to_identity();
            assert_eq!(
                identity.task_execution_id.as_deref(),
                Some(held.task_execution_id.as_str()),
                "case {case:?}: expected the id the record holds, got {identity:?}"
            );
            assert_eq!(
                identity.status.as_deref(),
                Some(held.status.as_str()),
                "case {case:?}: expected the status the record holds, got {identity:?}"
            );
            assert_eq!(
                identity.task_arn.as_deref(),
                Some(held.task_arn.as_str()),
                "case {case:?}: expected the ARN the record holds, got {identity:?}"
            );
            assert_eq!(
                identity.end_time,
                held.ended_at.map(super::super::commands::wire_instant),
                "case {case:?}: expected the end the record holds, got {identity:?}"
            );
        }
    }

    /// The one documented filter key narrows by the status the record holds, and its values
    /// UNION; a key outside the documented set selects nothing. The rows differ along the
    /// status filtered for and how many values the filter carries.
    #[test]
    fn the_status_filter_narrows_by_the_status_the_record_holds() {
        let mut held = record(
            "us-west-2",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "11111111-2222-3333-4444-555555555555",
        );
        held.status = "TIMED_OUT".to_string();

        // (case, key, values, selects)
        let rows: [(&str, &str, &[&str], bool); 5] = [
            ("the status it holds", "STATUS", &["TIMED_OUT"], true),
            (
                "the status it holds among others",
                "STATUS",
                &["SUCCESS", "TIMED_OUT"],
                true,
            ),
            ("another status", "STATUS", &["SUCCESS"], false),
            ("no value at all", "STATUS", &[], false),
            (
                "a key outside the documented set",
                "TaskExecutionId",
                &["11111111-2222-3333-4444-555555555555"],
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

    /// A listing of one run's task executions walks the records of THAT run, in the order
    /// they started, and reaches no other run's or Region's.
    #[test]
    fn a_listing_walks_the_task_executions_of_one_run() {
        let region = "us-west-2";
        let run = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let mut store = ComAmazonawsSsmAmazonssmState::default();

        // Recorded out of order, so the listing's order is the records' and not the
        // insertion's.
        let mut later = record(region, run, "22222222-2222-2222-2222-222222222222");
        later.started_at = epoch(1_893_456_060);
        store.insert_maintenance_window_execution_task(later);
        store.insert_maintenance_window_execution_task(record(
            region,
            run,
            "11111111-1111-1111-1111-111111111111",
        ));
        store.insert_maintenance_window_execution_task(record(
            region,
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
            "33333333-3333-3333-3333-333333333333",
        ));
        store.insert_maintenance_window_execution_task(record(
            "eu-central-1",
            run,
            "44444444-4444-4444-4444-444444444444",
        ));

        let listed: Vec<&str> = store
            .maintenance_window_execution_tasks_of(region, run)
            .into_iter()
            .map(|task| task.task_execution_id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222",
            ],
            "expected this run's task executions in the order they started, got {listed:?}"
        );

        for (case, region, run) in [
            ("a run nothing recorded", region, "99999999-9999-9999-9999-999999999999"),
            ("a Region holding none of this run's", "eu-west-1", run),
        ] {
            let listed = store.maintenance_window_execution_tasks_of(region, run);
            assert!(
                listed.is_empty(),
                "case {case:?}: expected no task execution, got {listed:?}"
            );
        }
    }

    /// The page size both listings serve: a request naming no `MaxResults` is served up to
    /// the modelled maximum, and one naming a value inside the modelled range is served
    /// exactly it. The rows differ along the value asked for, including each published bound
    /// and one past it.
    #[test]
    fn a_page_is_bounded_by_the_modelled_range() {
        // (case, MaxResults, the page size served or that it is refused)
        let rows: [(&str, Option<i32>, Option<usize>); 6] = [
            ("no bound asked for", None, Some(100)),
            ("the published minimum", Some(10), Some(10)),
            ("the published maximum", Some(100), Some(100)),
            ("a value inside the range", Some(37), Some(37)),
            ("one below the minimum", Some(9), None),
            ("one above the maximum", Some(101), None),
        ];

        for (case, max_results, expected) in rows {
            let served = execution_task_page_size(max_results).ok();
            assert_eq!(
                served, expected,
                "case {case:?}: expected {expected:?}, got {served:?}"
            );
        }
    }
}
