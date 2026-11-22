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
//! The LISTING of the same records is a different reading of the same absence:
//! "lists the executions of a maintenance window"
//! (`smithy_query(query="doc",shape="DescribeMaintenanceWindowExecutions")`) reports the runs
//! of ONE window this endpoint holds
//! ([`super::ComAmazonawsSsmAmazonssmState::maintenance_window_executions_of`]), in the order
//! they started, narrowed by the two documented filter keys
//! ([`EXECUTION_FILTER_KEYS`]) and bounded by the modelled page size
//! ([`execution_page_size`]). It models no not-found error, so a window that has run
//! nothing — including a `WindowId` nothing created — is reported as holding no execution
//! (HTTP 200 with an empty list) rather than refused: a listing reports the EXISTENCE of
//! nothing, which an empty list states exactly, while a read of ONE record has nothing to
//! report, which is why only the read refuses.
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

use crate::ops::types::{GetMaintenanceWindowExecutionOutput, MaintenanceWindowExecution};

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

/// The filter key that selects the runs that started no later than an instant: "supported
/// keys include `ExecutedBefore` and `ExecutedAfter`"
/// (`DescribeMaintenanceWindowExecutionsRequest$Filters`).
pub const EXECUTION_FILTER_KEY_EXECUTED_BEFORE: &str = "ExecutedBefore";

/// The filter key that selects the runs that started no earlier than an instant (same doc).
pub const EXECUTION_FILTER_KEY_EXECUTED_AFTER: &str = "ExecutedAfter";

/// The filter keys a listing of a window's runs may narrow by (same doc), and the one place
/// that set is written — what a refusal of another key lists.
pub const EXECUTION_FILTER_KEYS: &[&str] = &[
    EXECUTION_FILTER_KEY_EXECUTED_BEFORE,
    EXECUTION_FILTER_KEY_EXECUTED_AFTER,
];

/// Whether `key` is one of the filter keys a listing of a window's runs takes
/// ([`EXECUTION_FILTER_KEYS`]).
#[must_use]
pub fn is_execution_filter_key(key: &str) -> bool {
    EXECUTION_FILTER_KEYS.contains(&key)
}

/// The page size a listing of a window's runs serves (see `super::page_size`).
///
/// The listing bounds a page by the same modelled `MaintenanceWindowMaxResults`
/// (`range 10..100`) the describes of a window's tasks and of a run's task executions do, so
/// the bounds are read from where that shape's range is already written
/// ([`super::maintenance_window_tasks::MIN_TASK_PAGE_SIZE`]) rather than copied.
pub fn execution_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        super::maintenance_window_tasks::MIN_TASK_PAGE_SIZE,
        super::maintenance_window_tasks::MAX_TASK_PAGE_SIZE,
    )
}

/// Why `value` is not a value a filter of a window's runs may carry, or `None` when it is
/// one. `position` is the filter's place in the request's `Filters` list and `index` the
/// value's place in that filter's `Values`, both counted from one.
///
/// Both documented keys select by an INSTANT, so a value that names none narrows nothing:
/// "supported values are date/time strings in a valid ISO 8601 date/time format, such as
/// 2024-11-04T05:00:00Z" (`DescribeMaintenanceWindowExecutionsRequest$Filters`), read with
/// the one reader of that spelling this crate has
/// ([`super::commands::parse_filter_instant`]). The sentence is worded the way this API's
/// request-validation refusals are — the value, the member path the emulator's own boundary
/// validator spells a nested list element with, and the constraint it failed — as the
/// sibling filter refusal of this service already states one
/// ([`super::commands::command_filter_value_rejection`]).
#[must_use]
pub fn execution_filter_value_rejection(
    position: usize,
    index: usize,
    value: &str,
) -> Option<String> {
    if super::commands::parse_filter_instant(value).is_some() {
        return None;
    }
    Some(format!(
        "Value '{value}' at 'filters.{position}.member.values.{index}.member' failed to \
         satisfy constraint: Member must be a date/time string in a valid ISO 8601 date/time \
         format, such as 2024-11-04T05:00:00Z"
    ))
}

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

    /// This execution as the LISTING of a window's runs reports it.
    ///
    /// The members `MaintenanceWindowExecution` declares are the same ones the single read
    /// reports, beside the `WindowId` of the window the run belongs to, and every one of them
    /// comes off the record: a listing and a read of the same run report one execution.
    #[must_use]
    pub fn to_execution(&self) -> MaintenanceWindowExecution {
        MaintenanceWindowExecution {
            window_id: Some(self.window_id.clone()),
            window_execution_id: Some(self.window_execution_id.clone()),
            status: Some(self.status.clone()),
            status_details: self.status_details.clone(),
            start_time: Some(super::commands::wire_instant(self.started_at)),
            end_time: self.ended_at.map(super::commands::wire_instant),
        }
    }

    /// Whether this run satisfies the filter `key` carrying `values`.
    ///
    /// Both documented keys select by WHEN the run started: `ExecutedBefore` selects a run
    /// that started no later than the instant a value names and `ExecutedAfter` one that
    /// started no earlier than it, the bound itself included — the reading the sibling instant
    /// filters of this service take (`InvokedBefore` and `InvokedAfter` in
    /// [`super::commands`]). The values of one filter UNION — a filter is satisfied by any one
    /// of them, the reading every filtered listing of this service takes — so filters narrow
    /// and values widen. A key outside the documented set is refused before anything is
    /// matched, so one reaching here selects nothing.
    #[must_use]
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        let matches_value = |value: &String| match super::commands::parse_filter_instant(value) {
            // A value that names no instant narrows nothing; the listing refuses it before
            // matching, so this is a backstop rather than the first line of defence.
            None => false,
            Some(at) => match key {
                EXECUTION_FILTER_KEY_EXECUTED_BEFORE => self.started_at <= at,
                EXECUTION_FILTER_KEY_EXECUTED_AFTER => self.started_at >= at,
                _ => false,
            },
        };
        values.iter().any(matches_value)
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

    /// Every run of the window `window_id` this store holds in `region`, in the order they
    /// started — the order a listing reports them in, with the execution id breaking a tie so
    /// the order is total and a page can resume at an id.
    ///
    /// A window lives in ONE Region and a run belongs to ONE window, so a run of another
    /// window, or of this window at another Region's endpoint, is no run of this one. A
    /// `WindowId` nothing created holds no run either, which is every window until something
    /// records one.
    #[must_use]
    pub fn maintenance_window_executions_of(
        &self,
        region: &str,
        window_id: &str,
    ) -> Vec<&MaintenanceWindowExecutionRecord> {
        let mut held: Vec<&MaintenanceWindowExecutionRecord> = self
            .maintenance_window_executions
            .values()
            .filter(|execution| execution.region == region && execution.window_id == window_id)
            .collect();
        held.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.window_execution_id.cmp(&right.window_execution_id))
        });
        held
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

    /// A run of a window, so a listing row can differ from it in the window it belongs to,
    /// the Region it lives in and the instant it started.
    fn run_of(
        region: &str,
        window_id: &str,
        window_execution_id: &str,
        started: u64,
    ) -> MaintenanceWindowExecutionRecord {
        let mut held = record(region, window_execution_id);
        held.window_id = window_id.to_string();
        held.started_at = epoch(started);
        held
    }

    /// A listing walks the runs of ONE window of ONE Region, in the order they started, and
    /// reaches no other window's or Region's: the rows recorded below differ along the window
    /// they belong to, the Region they live in and when they started.
    #[test]
    fn a_listing_walks_the_runs_of_one_window() {
        let region = "us-west-2";
        let window = "mw-0123456789abcdef0";
        let other_window = "mw-ffffffffffffffff0";
        let mut store = ComAmazonawsSsmAmazonssmState::default();

        // Recorded out of the order they started, so the listing's order is the records' own
        // and not the insertion's.
        store.insert_maintenance_window_execution(run_of(
            region,
            window,
            "22222222-2222-2222-2222-222222222222",
            1_893_456_060,
        ));
        store.insert_maintenance_window_execution(run_of(
            region,
            window,
            "11111111-1111-1111-1111-111111111111",
            1_893_456_000,
        ));
        store.insert_maintenance_window_execution(run_of(
            region,
            other_window,
            "33333333-3333-3333-3333-333333333333",
            1_893_456_000,
        ));
        store.insert_maintenance_window_execution(run_of(
            "eu-central-1",
            window,
            "44444444-4444-4444-4444-444444444444",
            1_893_456_000,
        ));

        let listed: Vec<&str> = store
            .maintenance_window_executions_of(region, window)
            .into_iter()
            .map(|run| run.window_execution_id.as_str())
            .collect();
        assert_eq!(
            listed,
            vec![
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222",
            ],
            "expected this window's runs in the order they started, got {listed:?}"
        );

        // (case, the Region asked through, the window asked for)
        for (case, region, window) in [
            ("a window nothing ran", region, "mw-aaaaaaaaaaaaaaaaa"),
            ("a Region holding none of this window's", "eu-west-1", window),
            ("the window's run id, which names no window", region, "11111111-1111-1111-1111-111111111111"),
        ] {
            let listed = store.maintenance_window_executions_of(region, window);
            assert!(
                listed.is_empty(),
                "case {case:?}: expected no run, got {listed:?}"
            );
        }
    }

    /// A page of a window's runs is bounded by the modelled `MaintenanceWindowMaxResults`
    /// range (10..100), and a request naming no bound is served up to its maximum. The rows
    /// differ along the value asked for, including each published bound and one past it.
    #[test]
    fn a_page_of_runs_is_bounded_by_the_modelled_range() {
        // (case, MaxResults, the page size served or that it is refused)
        let rows: [(&str, Option<i32>, Option<usize>); 6] = [
            ("no bound asked for", None, Some(100)),
            ("the published minimum", Some(10), Some(10)),
            ("the published maximum", Some(100), Some(100)),
            ("a value inside the range", Some(42), Some(42)),
            ("one below the minimum", Some(9), None),
            ("one above the maximum", Some(101), None),
        ];

        for (case, max_results, expected) in rows {
            let served = execution_page_size(max_results).ok();
            assert_eq!(
                served, expected,
                "case {case:?}: expected {expected:?}, got {served:?}"
            );
        }
    }

    /// The two documented filter keys select a run by the instant it started, the bound
    /// included, and a filter's values union. The rows differ along the key, how many values
    /// the filter carries and where the run's own instant sits relative to them.
    #[test]
    fn an_instant_filter_selects_by_when_a_run_started() {
        let held = run_of(
            "us-west-2",
            "mw-0123456789abcdef0",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            // 2030-01-01T00:00:00Z, the instant the row bounds are written around.
            1_893_456_000,
        );

        // (case, key, the values the filter carries, whether the run satisfies it)
        let rows: [(&str, &str, &[&str], bool); 10] = [
            (
                "started before a later instant",
                EXECUTION_FILTER_KEY_EXECUTED_BEFORE,
                &["2030-01-02T00:00:00Z"],
                true,
            ),
            (
                "started after a later instant",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2030-01-02T00:00:00Z"],
                false,
            ),
            (
                "started after an earlier instant",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2029-12-31T00:00:00Z"],
                true,
            ),
            (
                "started before an earlier instant",
                EXECUTION_FILTER_KEY_EXECUTED_BEFORE,
                &["2029-12-31T00:00:00Z"],
                false,
            ),
            (
                "the bound is the instant it started, before",
                EXECUTION_FILTER_KEY_EXECUTED_BEFORE,
                &["2030-01-01T00:00:00Z"],
                true,
            ),
            (
                "the bound is the instant it started, after",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2030-01-01T00:00:00Z"],
                true,
            ),
            (
                "an instant written to sub-second precision",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2030-01-01T00:00:00.000Z"],
                true,
            ),
            (
                "two values, one of which it satisfies",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2030-01-02T00:00:00Z", "2029-12-31T00:00:00Z"],
                true,
            ),
            (
                "two values, neither of which it satisfies",
                EXECUTION_FILTER_KEY_EXECUTED_AFTER,
                &["2030-01-02T00:00:00Z", "2030-01-03T00:00:00Z"],
                false,
            ),
            (
                "a filter carrying no value at all",
                EXECUTION_FILTER_KEY_EXECUTED_BEFORE,
                &[],
                false,
            ),
        ];

        for (case, key, values, expected) in rows {
            let values: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
            let matched = held.matches_filter(key, &values);
            assert_eq!(
                matched, expected,
                "case {case:?}: expected matches={expected} for {key}={values:?}, got {matched}"
            );
        }
    }

    /// The keys a listing of a window's runs narrows by are exactly the two documented ones,
    /// and a value of either has to name an instant. The rows differ along the key asked for
    /// and the form of the value.
    #[test]
    fn a_filter_of_runs_takes_the_documented_keys_and_instant_values() {
        // (case, key, whether a listing narrows by it)
        let keys: [(&str, &str, bool); 6] = [
            ("the earlier bound", EXECUTION_FILTER_KEY_EXECUTED_BEFORE, true),
            ("the later bound", EXECUTION_FILTER_KEY_EXECUTED_AFTER, true),
            ("the same key in another case", "executedbefore", false),
            ("a key of the sibling listing", "STATUS", false),
            ("the window's own member", "WindowId", false),
            ("no key at all", "", false),
        ];

        for (case, key, narrows) in keys {
            assert_eq!(
                is_execution_filter_key(key),
                narrows,
                "case {case:?}: expected narrows={narrows} for {key:?}"
            );
        }

        // (case, the value carried, whether it names an instant)
        let values: [(&str, &str, bool); 6] = [
            ("the documented example", "2024-11-04T05:00:00Z", true),
            ("sub-second precision", "2024-11-04T05:00:00.250Z", true),
            ("a date with no time of day", "2024-11-04", false),
            ("the instant as epoch seconds", "1730696400", false),
            ("text naming no instant at all", "yesterday", false),
            ("no value at all", "", false),
        ];

        for (case, value, names_instant) in values {
            let rejection = execution_filter_value_rejection(1, 1, value);
            assert_eq!(
                rejection.is_none(),
                names_instant,
                "case {case:?}: expected an instant={names_instant}, got {rejection:?}"
            );
            if let Some(rejection) = rejection {
                assert!(
                    rejection.contains(value) || value.is_empty(),
                    "case {case:?}: the refusal states the value it refused, got {rejection:?}"
                );
                assert!(
                    rejection.contains("filters.1.member.values.1.member"),
                    "case {case:?}: the refusal states the member it was carried in, got \
                     {rejection:?}"
                );
            }
        }
    }

    /// What the LISTING reports comes off the record too, member by member — including the
    /// `WindowId` of the window the run belongs to, which the single read does not report. The
    /// rows differ along whether the run has finished and whether its status explains itself.
    #[test]
    fn a_listing_reports_what_the_record_holds() {
        // (case, how the row differs from the base record)
        let rows: [RecordVariantRow; 3] = [
            ("a run nothing has picked up", |_| {}),
            ("a run that has finished", |held| {
                held.ended_at = Some(epoch(1_893_459_600));
                held.status = "SUCCESS".to_string();
            }),
            ("a status with a detail", |held| {
                held.status = "CANCELLED".to_string();
                held.status_details = Some("Cancelled by the caller".to_string());
            }),
        ];

        for (case, differ) in rows {
            let mut held = run_of(
                "us-west-2",
                "mw-0123456789abcdef0",
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                1_893_456_000,
            );
            differ(&mut held);
            let reported = held.to_execution();
            assert_eq!(
                reported.window_id.as_deref(),
                Some(held.window_id.as_str()),
                "case {case:?}: expected the window the record holds, got {reported:?}"
            );
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
