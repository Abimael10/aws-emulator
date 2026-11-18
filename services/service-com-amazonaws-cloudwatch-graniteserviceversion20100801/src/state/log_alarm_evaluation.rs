// SPDX-License-Identifier: CC0-1.0

//! A LOG alarm's `StateValue`, computed from the results its scheduled query produces.
//!
//! "A log alarm evaluates the results of a CloudWatch Logs scheduled query against the
//! configured threshold and comparison operator to determine its state. When you create a log
//! alarm, the operation creates a service-managed CloudWatch Logs scheduled query that runs the
//! query string you provide on the schedule you configure. Each scheduled query execution
//! returns one or more aggregated values determined by the AggregationExpression, and each
//! aggregated value is compared against the alarm Threshold to determine the alarm state. The
//! alarm uses M-out-of-N evaluation: if QueryResultsToAlarm out of the most recent
//! QueryResultsToEvaluate query results breach the threshold, the alarm transitions to ALARM."
//! (`PutLogAlarm`) — so a log alarm's state is nobody's to write: it is what the EXECUTIONS of
//! its managed query make it.
//!
//! This module is the M-out-of-N fold, and the records it folds:
//!
//! * [`LogQueryExecution`] — one run of the alarm's managed query, which is also what CloudWatch
//!   Logs reports as that query's trigger history.
//! * [`LogQueryResult`] — one aggregated value a run produced, which is one "query result" of the
//!   M-out-of-N evaluation and one entry of the alarm's `StateReasonData`.
//!
//! The RUN itself belongs to CloudWatch Logs, which holds the log events — and so does the
//! AGGREGATION, which is written in that service's own query language: the alarm's
//! `AggregationExpression` is appended to its query string as the `stats` command it is
//! ([`aggregation`]) and the one query is run over the in-process callback ([`scheduled_runs`])
//! with no store guard held, so the values this fold compares are the ones that engine computed.
//! What comes back is recorded here, under the guard, and the fold then decides the alarm exactly
//! as the metric-alarm fold beside it decides a metric alarm: one transition, one history item,
//! one queued notification.

use ::serde_json::{Map, Value};

use super::alarm_actions::AlarmNotification;
use super::{
    to_timestamp, AlarmStateSnapshot, ComAmazonawsCloudwatchGraniteserviceversion20100801State as State,
    ALARM_STATE_ALARM, ALARM_STATE_INSUFFICIENT_DATA, ALARM_STATE_OK, ALARM_TYPE_LOG,
    HISTORY_DATA_VERSION, MAX_STATE_REASON_DATA_LEN,
};
use crate::ops::types::{AlarmHistoryItem, LogAlarm};

/// Running one of a log alarm's scheduled query executions over CloudWatch Logs.
pub mod scheduled_runs;

/// The ONE query an execution runs — the alarm's query string and its aggregation together — and
/// the reading of the aggregated rows that engine answers. Re-exported below, so a handler and the
/// run path reach them as `crate::state::…` like every other part of this store.
pub mod aggregation;

pub use aggregation::*;

/// `Complete`: the `TriggerHistoryRecord$executionStatus` of a scheduled query run that
/// produced its results, which is the status CloudWatch Logs reports for a run of this alarm's
/// managed query that happened.
pub const EXECUTION_STATUS_COMPLETE: &str = "Complete";

/// `InvalidQuery`: the same member's value for a run whose query string could not be run at
/// all, which is what a query CloudWatch Logs refuses leaves behind.
pub const EXECUTION_STATUS_INVALID_QUERY: &str = "InvalidQuery";

/// `Failed`: the same member's value for a run that could not be made — nothing refused the
/// query, and no results came back either.
pub const EXECUTION_STATUS_FAILED: &str = "Failed";

/// The status one aggregated value carries in the alarm's `StateReasonData`: the result of a run
/// that completed, so the value it carries is one the evaluation really compared.
pub const RESULT_STATUS_COMPLETE: &str = "COMPLETE";

/// The status the same member carries for the result of a run that produced NO aggregated value:
/// the execution is part of the window the alarm evaluates, and it carries no value because
/// nothing was computed for it, so the evaluation treats it as the missing data it is.
pub const RESULT_STATUS_FAILED: &str = "FAILED";

/// `PARTIAL_DATA`, one of the modelled `EvaluationState` values: "it indicates that the query
/// returned the maximum 500 contributor groups but more matched. The alarm evaluates the
/// available contributors, but results might be incomplete." (`LogAlarm$EvaluationState`)
pub const EVALUATION_STATE_PARTIAL_DATA: &str = "PARTIAL_DATA";

/// `EVALUATION_ERROR`: "it indicates configuration errors in the alarm setup that require review
/// and correction. Refer to the StateReason field of the alarm for more details."
/// (`LogAlarm$EvaluationState`) — the state of an alarm whose own query or aggregation expression
/// is one nothing can run.
pub const EVALUATION_STATE_ERROR: &str = "EVALUATION_ERROR";

/// `EVALUATION_FAILURE`: "it indicates temporary CloudWatch issues" (`LogAlarm$EvaluationState`)
/// — the state of an alarm whose run could not be made at all, which nothing about its
/// configuration explains.
pub const EVALUATION_STATE_FAILURE: &str = "EVALUATION_FAILURE";

/// The most contributor groups one execution's aggregation yields: "the query returned the
/// maximum 500 contributor groups but more matched" (`LogAlarm$EvaluationState`), which is the
/// ceiling that makes an evaluation report [`EVALUATION_STATE_PARTIAL_DATA`].
pub const MAX_CONTRIBUTOR_GROUPS: usize = 500;

/// THE EMULATOR'S OWN CEILING on the executions one log alarm keeps a record of: the alarm's
/// managed query keeps running for as long as the process does, so the oldest records are
/// dropped once it holds this many rather than growing without bound. It is the same ceiling
/// CloudWatch Logs keeps a scheduled query's trigger history to, because that history IS this
/// list.
pub const MAX_QUERY_EXECUTIONS_KEPT: usize = 100;

/// THE EMULATOR'S OWN CEILING on the aggregated values one log alarm keeps.
///
/// Only the most recent `QueryResultsToEvaluate` of them decide the alarm, and that member is
/// bounded by the writer ([`crate::ops::put_log_alarm`]), so this is the capacity bound beneath
/// it: enough to evaluate any alarm this service accepts, times the groups one execution may
/// yield, and nothing is ever decided on a result older than the window.
pub const MAX_QUERY_RESULTS_KEPT: usize = 1_000;

/// How long after a log alarm's write — and after each execution of its managed query — the NEXT
/// execution falls due.
///
/// A log alarm's query "runs the query string you provide on the schedule you configure"
/// (`PutLogAlarm`), and the shortest schedule a log alarm can state names a wall-clock gap of a
/// minute (`rate(1 minute)`, `ScheduleConfiguration$ScheduleExpression`). This span is THE
/// EMULATOR'S OWN COMPRESSION of that gap, for the same reason
/// [`super::alarm_evaluation::EVALUATION_DELAY_SECS`] is the emulator's own choice: a client that
/// configures a log alarm looks for what its executions deliver without waiting for the wall
/// clock, and an M-out-of-N evaluation needs SEVERAL executions before it can rest in `OK` — so
/// the runs that chain needs fall due inside the time a client waits rather than over the minutes
/// the schedule names. It is never zero (a caller reading straight after the write still sees the
/// alarm before its first run), never shorter than the span CloudWatch Logs gives a scheduled
/// query's own first run, and long enough that a client reading the managed query's recent
/// trigger history still finds the run its alarm's current state rests on. The instant is stored
/// and never slept on: the run is made by the first call, or clock tick, at or after it.
///
/// The LOOKBACK each run reads is NOT this span: it is the alarm's own
/// `ScheduleConfiguration$StartTimeOffset` / `EndTimeOffset`, applied verbatim
/// ([`LogAlarmQueryRun`]).
pub const QUERY_EXECUTION_SPAN_SECS: f64 = 15.0;

/// The most log alarm runs ONE settle makes.
///
/// Each run is two in-process requests to CloudWatch Logs plus the fold over the rows they
/// answer, so a store holding many log alarms cannot turn one call into unbounded work. A run
/// this bound does not reach stays DUE and the next call, or the next clock tick, makes it.
const MAX_RUNS_PER_SETTLE: usize = 8;

/// One recorded execution of the scheduled query CloudWatch manages for a log alarm.
///
/// This is the alarm's side of a CloudWatch Logs `TriggerHistoryRecord` ("a record of a scheduled
/// query execution, including execution status, timestamp") — the alarm's own
/// `ScheduledQueryConfiguration` is the one home of that query's configuration, so the runs of it
/// are held here too and CloudWatch Logs reports them from here rather than keeping a second
/// copy.
#[derive(Debug, Clone)]
pub struct LogQueryExecution {
    /// The id of the query this run executed, which is the handle CloudWatch Logs answers this
    /// run's own rows under — and the `QueryExecutionId` the alarm's notification names.
    pub query_id: String,
    /// When the run was triggered, as epoch seconds.
    pub triggered_secs: f64,
    /// The run's execution status: [`EXECUTION_STATUS_COMPLETE`] for a run that produced its
    /// results, and one of the other two for a run that did not.
    pub status: String,
    /// Why the run produced nothing, for a run that produced nothing.
    pub error_message: Option<String>,
}

/// One aggregated value an execution produced: one "query result" of the M-out-of-N evaluation.
#[derive(Debug, Clone)]
pub struct LogQueryResult {
    /// What identifies this value among the ones the alarm has evaluated.
    ///
    /// UNVERIFIED FORM: neither the model nor the published documentation states how CloudWatch
    /// composes a `queryResultId`, so this emulator names the value by the execution that
    /// produced it, with the contributor group appended when the aggregation groups — which is
    /// what distinguishes the values of one execution from each other.
    pub query_result_id: String,
    /// The execution that produced it.
    pub query_execution_id: String,
    /// The status of the result: [`RESULT_STATUS_COMPLETE`] for a value a completed run computed,
    /// and [`RESULT_STATUS_FAILED`] for the result of a run that computed none.
    pub status: String,
    /// The value itself, as the aggregation expression computed it over the run's rows, and `None`
    /// for a run that produced no aggregation at all — there is no value to report for it, so the
    /// result carries none and the fold counts it as missing.
    pub value: Option<f64>,
    /// The contributor group it belongs to, for an aggregation that groups; `None` for one that
    /// yields a single value per execution.
    pub group: Option<String>,
    /// When the execution that produced it was triggered, as epoch seconds.
    pub at_secs: f64,
}

/// One run of a log alarm's managed query, as the settle that makes it needs it.
///
/// Read out of the store under its guard and carried out with the guard released, so the two
/// services' stores are never locked at once.
#[derive(Debug, Clone)]
pub struct LogAlarmQueryRun {
    /// The alarm the run belongs to.
    pub alarm_name: String,
    /// The Region the query runs in: the alarm's own, read off its ARN.
    pub region: String,
    /// "The CloudWatch Logs query to execute on each scheduled run."
    /// (`ScheduledQueryConfiguration$QueryString`)
    pub query_string: String,
    /// "The log groups to query. Each entry can be a log group name or ARN."
    /// (`ScheduledQueryConfiguration$LogGroupIdentifiers`)
    pub log_group_identifiers: Vec<String>,
    /// The start of the window this run reads, as epoch seconds: "The offset, in seconds, before
    /// the scheduled execution time at which the query time range begins."
    /// (`ScheduleConfiguration$StartTimeOffset`)
    pub start_secs: i64,
    /// The end of that window: "The offset, in seconds, before the scheduled execution time at
    /// which the query time range ends." (`ScheduleConfiguration$EndTimeOffset`)
    pub end_secs: i64,
    /// "The Amazon Resource Name (ARN) of the IAM role that CloudWatch assumes when executing the
    /// scheduled query against the configured log groups."
    /// (`ScheduledQueryConfiguration$ScheduledQueryRoleARN`) — the identity the run is made under,
    /// so a run is made only where that role can be taken on.
    pub scheduled_query_role_arn: String,
    /// The aggregation the rows this run answers are folded by.
    pub aggregation_expression: String,
    /// When this run was triggered, as epoch seconds.
    pub triggered_secs: f64,
}

/// What ONE run produced, as the settle hands it back to the store.
#[derive(Debug, Clone, Default)]
pub struct LogAlarmRunOutcome {
    /// The id of the query that ran, when a query was started at all.
    pub query_id: Option<String>,
    /// The aggregated values the run's rows yielded, in the order the aggregation orders them,
    /// each with the contributor group it belongs to.
    pub values: Vec<(Option<String>, f64)>,
    /// Whether the aggregation had more contributor groups than it reports
    /// ([`MAX_CONTRIBUTOR_GROUPS`]).
    pub partial: bool,
    /// Why the run produced no values, for a run that produced none.
    pub failure: Option<String>,
    /// Whether that failure is one the alarm's own configuration explains, which is what decides
    /// between the two failure values of `EvaluationState`.
    pub configuration_fault: bool,
    /// The execution status the run ended in, for a run whose end the query's own text does not
    /// explain — a run that never reached CloudWatch Logs at all because the identity it had to be
    /// made under could not be taken on ends `Failed`, however much the alarm's configuration
    /// explains it. `None` leaves the status to the fault the failure carries.
    pub execution_status: Option<&'static str>,
}

impl State {
    /// The log alarm query runs the clock has ALREADY made due, claimed in the same critical
    /// section that found them owed: each alarm's next run is armed before this returns, so a
    /// settle overlapping this one does not make the same run a second time.
    ///
    /// At most [`MAX_RUNS_PER_SETTLE`] of them, oldest due first; what this leaves stays due.
    pub fn due_log_alarm_runs(&mut self, now_secs: f64) -> Vec<LogAlarmQueryRun> {
        let mut due: Vec<(f64, String)> = self
            .log_alarms
            .iter()
            .filter(|(_, stored)| stored.query_execution_due_secs <= now_secs)
            .map(|(name, stored)| (stored.query_execution_due_secs, name.clone()))
            .collect();
        due.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        });
        let mut runs: Vec<LogAlarmQueryRun> = Vec::new();
        for (_, name) in due.into_iter().take(MAX_RUNS_PER_SETTLE) {
            let Some(stored) = self.log_alarms.get_mut(&name) else {
                continue;
            };
            // Armed before the run is made, so the run is charged to this settle only.
            stored.query_execution_due_secs = now_secs + QUERY_EXECUTION_SPAN_SECS;
            if let Some(run) = query_run(&stored.alarm, now_secs) {
                runs.push(run);
            }
        }
        runs
    }

    /// The instant the next log alarm query run falls due, or `None` when this store holds no
    /// log alarm at all — which is what keeps the alarm clock running for them.
    pub fn next_log_alarm_run_due_secs(&self) -> Option<f64> {
        self.log_alarms
            .values()
            .map(|stored| stored.query_execution_due_secs)
            .min_by(f64::total_cmp)
    }

    /// Record what one run produced on the alarm it belongs to, then evaluate that alarm.
    ///
    /// The execution is recorded whatever it produced — a run that happened is part of the
    /// query's history either way — and each aggregated value becomes one query result of the
    /// M-out-of-N window.
    pub fn record_log_alarm_run(
        &mut self,
        run: &LogAlarmQueryRun,
        outcome: &LogAlarmRunOutcome,
        now_secs: f64,
    ) {
        let Some(stored) = self.log_alarms.get_mut(&run.alarm_name) else {
            return;
        };
        let status = match (&outcome.failure, outcome.execution_status, outcome.configuration_fault)
        {
            (None, _, _) => EXECUTION_STATUS_COMPLETE,
            (Some(_), Some(status), _) => status,
            (Some(_), None, true) => EXECUTION_STATUS_INVALID_QUERY,
            (Some(_), None, false) => EXECUTION_STATUS_FAILED,
        };
        // A run nothing could start has no query of its own to name, so the execution is
        // identified by the instant it was triggered at instead of by a query id nobody holds.
        let query_id = outcome
            .query_id
            .clone()
            .unwrap_or_else(|| format!("{:.0}", run.triggered_secs * 1_000.0));
        stored.query_executions.push(LogQueryExecution {
            query_id: query_id.clone(),
            triggered_secs: run.triggered_secs,
            status: status.to_string(),
            error_message: outcome.failure.clone(),
        });
        while stored.query_executions.len() > MAX_QUERY_EXECUTIONS_KEPT {
            stored.query_executions.remove(0);
        }
        for (group, value) in &outcome.values {
            stored.query_results.push(LogQueryResult {
                query_result_id: match group {
                    Some(group) => format!("{query_id}:{group}"),
                    None => query_id.clone(),
                },
                query_execution_id: query_id.clone(),
                status: RESULT_STATUS_COMPLETE.to_string(),
                value: Some(*value),
                group: group.clone(),
                at_secs: run.triggered_secs,
            });
        }
        // An execution that produced NOTHING is still one of the query results the alarm
        // evaluates — it is a scheduled result of the alarm's window, with no aggregation to
        // report — so it enters the window as a failed result carrying no value, which is what the
        // fold counts as missing data for that slot. A run that merely matched no log line is not
        // one of these: its aggregation ran and yielded no row.
        if outcome.failure.is_some() {
            stored.query_results.push(LogQueryResult {
                query_result_id: query_id.clone(),
                query_execution_id: query_id.clone(),
                status: RESULT_STATUS_FAILED.to_string(),
                value: None,
                group: None,
                at_secs: run.triggered_secs,
            });
        }
        while stored.query_results.len() > MAX_QUERY_RESULTS_KEPT {
            stored.query_results.remove(0);
        }
        // "If the value of this field is PARTIAL_DATA, it indicates that the query returned the
        // maximum 500 contributor groups but more matched" — and an evaluation that really ran
        // over everything it asked for reports no evaluation state at all, so a run that
        // succeeds CLEARS whatever the run before it left.
        let evaluation_state = match (&outcome.failure, outcome.configuration_fault) {
            (Some(_), true) => Some(EVALUATION_STATE_ERROR.to_string()),
            (Some(_), false) => Some(EVALUATION_STATE_FAILURE.to_string()),
            (None, _) if outcome.partial => Some(EVALUATION_STATE_PARTIAL_DATA.to_string()),
            (None, _) => None,
        };
        self.evaluate_log_alarm(
            &run.alarm_name,
            &query_id,
            evaluation_state,
            outcome.failure.as_deref(),
            now_secs,
        );
    }

    /// Decide ONE log alarm from the query results it holds, writing its new state only when the
    /// state the results imply differs from the one it reports.
    ///
    /// The transition path is the one every alarm of this store takes: the two state stamps, a
    /// `StateUpdate` history item naming both states, and the queued notification whose actions
    /// are the ones configured for the state the alarm moved to.
    fn evaluate_log_alarm(
        &mut self,
        name: &str,
        execution_id: &str,
        evaluation_state: Option<String>,
        failure: Option<&str>,
        now_secs: f64,
    ) {
        let Some(stored) = self.log_alarms.get(name) else {
            return;
        };
        let alarm = stored.alarm.clone();
        let window = evaluated_window(&alarm, &stored.query_results);
        let verdict = Verdict::of(&alarm, window, failure, now_secs);
        let Some(verdict) = verdict else {
            // Nothing to decide the alarm with and nothing to explain: the state it reports
            // stands, and only the evaluation state this run observed is written.
            if let Some(stored) = self.log_alarms.get_mut(name) {
                stored.alarm.evaluation_state = evaluation_state;
            }
            return;
        };
        let moved = alarm.state_value.as_deref() != Some(verdict.state.as_str());
        let previous = AlarmStateSnapshot::of_log_alarm(&alarm);
        let summary = format!(
            "Alarm updated from {} to {}",
            previous
                .state_value
                .as_deref()
                .unwrap_or(ALARM_STATE_INSUFFICIENT_DATA),
            verdict.state
        );
        let Some(stored) = self.log_alarms.get_mut(name) else {
            return;
        };
        // "The time stamp of the last update to the value of either the StateValue or
        // EvaluationState parameters." (`LogAlarm$StateUpdatedTimestamp`)
        let state_changed = moved || stored.alarm.evaluation_state != evaluation_state;
        stored.alarm.evaluation_state = evaluation_state;
        stored.alarm.state_reason = Some(verdict.reason.clone());
        stored.alarm.state_reason_data = Some(verdict.reason_data.clone());
        if state_changed {
            stored.alarm.state_updated_timestamp = Some(to_timestamp(now_secs));
        }
        if !moved {
            return;
        }
        stored.alarm.state_value = Some(verdict.state.clone());
        // "The date and time that the alarm's StateValue most recently changed."
        // (`LogAlarm$StateTransitionedTimestamp`)
        stored.alarm.state_transitioned_timestamp = Some(to_timestamp(now_secs));
        let updated = AlarmStateSnapshot::of_log_alarm(&stored.alarm);
        let notification =
            AlarmNotification::of_log_transition(&stored.alarm, execution_id, &previous, now_secs);
        self.record_history(
            name,
            now_secs,
            AlarmHistoryItem {
                alarm_name: Some(name.to_string()),
                alarm_contributor_id: None,
                alarm_type: Some(ALARM_TYPE_LOG.to_string()),
                timestamp: Some(to_timestamp(now_secs)),
                history_item_type: Some("StateUpdate".to_string()),
                history_summary: Some(summary),
                history_data: Some(super::state_transition_history_data(&previous, &updated)),
                alarm_contributor_attributes: None,
            },
        );
        if let Some(notification) = notification {
            self.enqueue_notification(notification, now_secs);
        }
    }
}

/// The run `alarm` owes at `now_secs`, or `None` for an alarm carrying no scheduled query
/// configuration at all — which is an alarm with nothing to run.
///
/// The window is the alarm's own: "The offset, in seconds, before the scheduled execution time at
/// which the query time range begins" and the offset at which it ends
/// (`ScheduleConfiguration$StartTimeOffset` / `$EndTimeOffset`), applied to the instant the run
/// was triggered at. Both ends are stated in the epoch SECONDS a query's range is stated in, and
/// each is rounded OUTWARD — the start down, the end up — so the window covers the whole of every
/// second the offsets name rather than cutting an event published inside one of them.
fn query_run(alarm: &LogAlarm, now_secs: f64) -> Option<LogAlarmQueryRun> {
    let configuration = alarm.scheduled_query_configuration.as_ref()?;
    let schedule = &configuration.schedule_configuration;
    #[allow(clippy::cast_possible_truncation)]
    let start_secs = (now_secs - schedule.start_time_offset as f64).floor() as i64;
    #[allow(clippy::cast_possible_truncation)]
    let end_secs = (now_secs - schedule.end_time_offset.unwrap_or(0) as f64).ceil() as i64;
    Some(LogAlarmQueryRun {
        alarm_name: alarm.alarm_name.clone()?,
        region: alarm
            .alarm_arn
            .as_deref()
            .and_then(::service_common::arn::parse)
            .map(|arn| arn.region.to_string())
            .unwrap_or_default(),
        query_string: configuration.query_string.clone(),
        log_group_identifiers: configuration
            .log_group_identifiers
            .clone()
            .unwrap_or_default(),
        start_secs,
        end_secs,
        scheduled_query_role_arn: configuration.scheduled_query_role_arn.clone(),
        aggregation_expression: configuration.aggregation_expression.clone(),
        triggered_secs: now_secs,
    })
}

/// The query results ONE evaluation of `alarm` reads: "the number of most recent scheduled query
/// results that the alarm evaluates against the threshold (the N in M-of-N evaluation)"
/// (`LogAlarm$QueryResultsToEvaluate`), oldest first.
fn evaluated_window<'a>(alarm: &LogAlarm, results: &'a [LogQueryResult]) -> &'a [LogQueryResult] {
    let evaluate = alarm
        .query_results_to_evaluate
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(1)
        .max(1);
    let from = results.len().saturating_sub(evaluate);
    results.get(from..).unwrap_or_default()
}

/// What one evaluation decided, and how it explains itself.
#[derive(Debug)]
struct Verdict {
    /// The state the results imply.
    state: String,
    /// `StateReason`: the explanation in text.
    reason: String,
    /// `StateReasonData`: the same explanation as the document a consumer parses.
    reason_data: String,
}

impl Verdict {
    /// The verdict `window` reaches for `alarm`, or `None` when the results decide nothing and
    /// there is no failure to explain either — in which case the alarm keeps the state and the
    /// explanation it has.
    fn of(
        alarm: &LogAlarm,
        window: &[LogQueryResult],
        failure: Option<&str>,
        now_secs: f64,
    ) -> Option<Self> {
        let evaluate = alarm
            .query_results_to_evaluate
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(1)
            .max(1);
        let to_alarm = alarm
            .query_results_to_alarm
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(1)
            .max(1)
            .min(evaluate);
        let threshold = alarm.threshold?;
        let comparison = super::alarm_evaluation::comparison_of(
            alarm.comparison_operator.as_deref().unwrap_or_default(),
        )?;

        let breaching = window
            .iter()
            .filter_map(|result| result.value)
            .filter(|value| (comparison.breaches)(*value, threshold))
            .count();
        // Only a result that CARRIES a value was compared at all: an execution that produced no
        // aggregation fills its slot of the window with no value to grade, so the slot is missing
        // data exactly as a slot no execution has reached yet is.
        let graded = window
            .iter()
            .filter(|result| result.value.is_some())
            .count();
        let not_breaching = graded.saturating_sub(breaching);
        // A slot of the window no execution has produced a result for yet is missing data, and
        // "How this alarm handles missing data points. Valid values are breaching,
        // notBreaching, ignore, and missing." (`LogAlarm$TreatMissingData`) says what each one
        // of them counts as.
        let missing = evaluate.saturating_sub(graded);
        let arm = alarm.treat_missing_data.as_deref().unwrap_or(MISSING_MISSING);
        let (breaching, not_breaching) = match arm {
            MISSING_BREACHING => (breaching + missing, not_breaching),
            MISSING_NOT_BREACHING => (breaching, not_breaching + missing),
            _ => (breaching, not_breaching),
        };
        // "if QueryResultsToAlarm out of the most recent QueryResultsToEvaluate query results
        // breach the threshold, the alarm transitions to ALARM" (`PutLogAlarm`) — and the OK
        // direction is the complement of that: the alarm leaves ALARM only once no window of
        // `QueryResultsToAlarm` results can breach any more, which takes
        // `QueryResultsToEvaluate - QueryResultsToAlarm + 1` non-breaching results.
        let to_ok = evaluate.saturating_sub(to_alarm).saturating_add(1);
        let state = if breaching >= to_alarm {
            ALARM_STATE_ALARM
        } else if not_breaching >= to_ok {
            ALARM_STATE_OK
        } else if failure.is_some() {
            // Nothing was computed and the window cannot reach either direction: the alarm has
            // no data to be in a graded state on.
            ALARM_STATE_INSUFFICIENT_DATA
        } else {
            // "ignore — the current alarm state is maintained" and the `missing` default leave
            // the alarm where it is until the results reach one of the two directions.
            return None;
        };
        let minimum = if state == ALARM_STATE_ALARM {
            to_alarm
        } else {
            to_ok
        };
        let counted = if state == ALARM_STATE_ALARM {
            breaching
        } else {
            not_breaching
        };
        // The values the sentence NAMES: the ones on the side of the comparison the state stands
        // for, because the sentence states that comparison the way that direction reads it. The
        // whole window is in the decision record beside it.
        let deciding = state == ALARM_STATE_ALARM;
        let cited: Vec<f64> = window
            .iter()
            .filter_map(|result| result.value)
            .filter(|value| (comparison.breaches)(*value, threshold) == deciding)
            .collect();
        Some(Self {
            state: state.to_string(),
            reason: state_reason(
                state,
                counted,
                &cited,
                evaluate,
                minimum,
                comparison.phrase,
                threshold,
                failure,
            ),
            reason_data: state_reason_data(alarm, window, threshold, evaluate, to_alarm, now_secs),
        })
    }
}

/// The `TreatMissingData` value that counts a missing query result as breaching.
const MISSING_BREACHING: &str = "breaching";

/// The `TreatMissingData` value that counts a missing query result as not breaching.
const MISSING_NOT_BREACHING: &str = "notBreaching";

/// The `TreatMissingData` default: "If this parameter is omitted, the default behavior of
/// missing is used." (`PutLogAlarmInput$TreatMissingData`) — a missing result is neither
/// breaching nor not breaching.
const MISSING_MISSING: &str = "missing";

/// The most characters the value list a `StateReason` cites may take, inside the modelled
/// `@length 0..1023` of `StateReason`.
const MAX_CITED_VALUE_CHARS: usize = 700;

/// The `StateReason` an evaluated log alarm reports: how many of the query results it evaluated
/// read the way the state says against the threshold, those results themselves, the minimum the
/// transition needed and the direction it was decided in.
///
/// UNVERIFIED WORDING: CloudWatch publishes no sentence for a log alarm's state reason, so this
/// is the sentence this service's own metric-alarm explanations are written in, with the query
/// results it evaluated in place of the datapoints — the counts, the comparison, the threshold
/// and the transition the state was decided in are all recoverable from it. A run that produced
/// nothing says that instead, because there is no comparison to report for it.
#[allow(clippy::too_many_arguments)]
fn state_reason(
    state: &str,
    counted: usize,
    cited: &[f64],
    evaluate: usize,
    minimum: usize,
    comparison: &str,
    threshold: f64,
    failure: Option<&str>,
) -> String {
    if let Some(failure) = failure {
        return format!("Insufficient Data: the scheduled query produced no result: {failure}");
    }
    let negation = if state == ALARM_STATE_ALARM { "" } else { "not " };
    let transition = if state == ALARM_STATE_ALARM {
        format!("{ALARM_STATE_OK} -> {ALARM_STATE_ALARM}")
    } else {
        format!("{ALARM_STATE_ALARM} -> {ALARM_STATE_OK}")
    };
    // The suffix belongs to the WINDOW the sentence names ("out of the last N query results"),
    // and the verb to the count of them that read the way the state says — the same split the
    // metric-alarm sentence this mirrors makes.
    let (results_suffix, _) = plural(evaluate);
    let (minimum_suffix, _) = plural(minimum);
    let (_, verb) = plural(counted);
    format!(
        "Threshold Crossed: {counted} out of the last {evaluate} query result{results_suffix}{} \
         {verb} {negation}{comparison} the threshold ({}) (minimum {minimum} query \
         result{minimum_suffix} for {transition} transition).",
        cited_values(cited),
        decimal_text(threshold)
    )
}

/// The query results an explanation CITES, newest first: ` [7.0, 2.0]`.
///
/// Only the results on the side of the comparison the state stands for are named
/// ([`state_reason`]). Empty — and so droppable from the sentence without leaving a gap — when
/// the evaluation had no result of its own on that side, which is every state a missing-data arm
/// alone decided.
fn cited_values(cited: &[f64]) -> String {
    let mut text = String::new();
    for value in cited.iter().rev() {
        let entry = decimal_text(*value);
        if text.len() + entry.len() + 2 > MAX_CITED_VALUE_CHARS {
            break;
        }
        if !text.is_empty() {
            text.push_str(", ");
        }
        text.push_str(&entry);
    }
    if text.is_empty() {
        return text;
    }
    format!(" [{text}]")
}

/// The plural suffix and the verb a count takes.
fn plural(count: usize) -> (&'static str, &'static str) {
    if count == 1 {
        ("", "was")
    } else {
        ("s", "were")
    }
}

/// One measured quantity as an explanation names it, through the one rendering this crate's
/// alarm explanations use.
fn decimal_text(value: f64) -> String {
    super::alarm_evaluation::decimal_text(value)
}

/// The `StateReasonData` an evaluated log alarm reports: the threshold it compared against, the
/// two halves of its M-out-of-N configuration, and the query results the state was decided over
/// — each with the id of the result, the status of the execution that produced it and the value
/// it carries.
///
/// "An explanation for the alarm state, in JSON format." (`LogAlarm$StateReasonData`), bounded by
/// [`MAX_STATE_REASON_DATA_LEN`] — a document is JSON, so it can never be truncated to fit: the
/// oldest results are dropped from the list instead until it does.
fn state_reason_data(
    alarm: &LogAlarm,
    window: &[LogQueryResult],
    threshold: f64,
    evaluate: usize,
    to_alarm: usize,
    now_secs: f64,
) -> String {
    let mut document = Map::new();
    document.insert("version".to_string(), Value::from(HISTORY_DATA_VERSION));
    if let Some(at) = super::document_instant(now_secs) {
        document.insert("queryDate".to_string(), Value::from(at));
    }
    document.insert("threshold".to_string(), Value::from(threshold));
    document.insert(
        "queryResultsToEvaluate".to_string(),
        Value::from(evaluate as u64),
    );
    document.insert(
        "queryResultsToAlarm".to_string(),
        Value::from(to_alarm as u64),
    );
    if let Some(operator) = alarm.comparison_operator.as_deref() {
        document.insert("comparisonOperator".to_string(), Value::from(operator));
    }
    // The results the state was decided over, oldest first — the order they were produced in.
    let mut first = 0;
    loop {
        let reported: Vec<Value> = window
            .get(first..)
            .unwrap_or_default()
            .iter()
            .map(result_document)
            .collect();
        let mut full = document.clone();
        full.insert("results".to_string(), Value::Array(reported));
        let rendered = Value::Object(full).to_string();
        if rendered.len() <= MAX_STATE_REASON_DATA_LEN {
            return rendered;
        }
        if first >= window.len() {
            // Not even the empty list fits, so the document reports the configuration alone.
            return Value::Object(document).to_string();
        }
        first += 1;
    }
}

/// One query result as the explanation document reports it.
fn result_document(result: &LogQueryResult) -> Value {
    let mut entry = Map::new();
    entry.insert(
        "queryResultId".to_string(),
        Value::from(result.query_result_id.clone()),
    );
    entry.insert("status".to_string(), Value::from(result.status.clone()));
    // A result no aggregation was computed for carries no value member at all: there is nothing to
    // report for it, and a value stated for it would be one nothing measured.
    if let Some(value) = result.value {
        entry.insert("value".to_string(), Value::from(value));
    }
    if let Some(group) = result.group.as_deref() {
        entry.insert("contributorGroup".to_string(), Value::from(group));
    }
    if let Some(at) = super::document_instant(result.at_secs) {
        entry.insert("timestamp".to_string(), Value::from(at));
    }
    entry.insert(
        "queryExecutionId".to_string(),
        Value::from(result.query_execution_id.clone()),
    );
    Value::Object(entry)
}

#[cfg(test)]
mod tests;
