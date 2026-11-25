// SPDX-License-Identifier: CC0-1.0

//! The alarm history documents this store builds (`AlarmHistoryItem$HistoryData`).
//!
//! Every history item an alarm writer records carries `HistoryData`: "Data about the alarm, in
//! JSON format." (`smithy_query(query="shape",shape="AlarmHistoryItem")`), a `HistoryData`
//! string with `@length 1..4095`. It is the JSON record of the change the item reports, so it
//! is built HERE, from the alarm as the store holds it before and after the write, and shared
//! by every writer — one definition, so two writers cannot record two different shapes of
//! document.

use super::*;

/// The modelled maximum length of one history document: `AlarmHistoryItem$HistoryData` targets
/// `HistoryData`, a string with `@length 1..4095`.
pub const MAX_HISTORY_DATA_LEN: usize = 4095;

/// The schema version every history document this service records declares, so a consumer can
/// tell which layout it is reading.
pub(super) const HISTORY_DATA_VERSION: &str = "1.0";

/// A history document's `type` for an alarm the call CREATED.
pub(super) const CONFIGURATION_CREATE: &str = "Create";

/// A history document's `type` for an alarm the call OVERWROTE.
pub(super) const CONFIGURATION_UPDATE: &str = "Update";

/// Which of the two serialisations of an alarm a document states it in.
///
/// An alarm is written out in two places, and the two do not state it the same way, so the
/// rendering is told which one it is writing rather than one of them being derived from the
/// other:
///
/// * [`AlarmDocument::Configuration`] is the store's record of a CONFIGURATION — the member set
///   the alarm's configuration consists of, under the model's own member names in lower camel
///   case, and no state. It is what the configuration-change event carries and what decides
///   whether a write changed an alarm at all ([`alarm_configuration_events`]).
/// * [`AlarmDocument::History`] is the SNAPSHOT an alarm-history document records: the whole
///   alarm the write left behind — its configuration, the instant it was configured at, and the
///   state it is standing in — in the alarm-history serialisation's own spelling, which ends an
///   alarm with its action lists, writes the OK list in lower case and states a dimension's
///   value before its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmDocument {
    /// The configuration record.
    Configuration,
    /// The snapshot an alarm-history document records.
    History,
}

/// One alarm's state, as a history document records it.
///
/// The three state members EVERY kind of alarm carries, so one definition of a transition
/// document serves the metric alarms `SetAlarmState` writes and the composite alarms whose
/// state the rule evaluation writes, and the two cannot record two different shapes of
/// document.
#[derive(Debug, Clone, Default)]
pub struct AlarmStateSnapshot {
    /// The alarm's `StateValue`.
    pub state_value: Option<String>,
    /// The alarm's `StateReason`.
    pub state_reason: Option<String>,
    /// The alarm's `StateReasonData`.
    pub state_reason_data: Option<String>,
    /// WHEN the alarm last reported this state, as its `StateUpdatedTimestamp` stood — which is
    /// the instant a state-change event names for the state a transition moved the alarm OUT of
    /// ([`alarm_actions::state_change_events`]). Not part of the history document: a transition
    /// record names the two states, and the item carries the instant of the transition itself.
    pub state_updated_secs: Option<f64>,
}

impl AlarmStateSnapshot {
    /// One metric alarm's state.
    pub fn of_metric_alarm(alarm: &MetricAlarm) -> Self {
        Self {
            state_value: alarm.state_value.clone(),
            state_reason: alarm.state_reason.clone(),
            state_reason_data: alarm.state_reason_data.clone(),
            state_updated_secs: alarm.state_updated_timestamp.as_ref().map(from_timestamp),
        }
    }

    /// One composite alarm's state.
    pub fn of_composite_alarm(alarm: &CompositeAlarm) -> Self {
        Self {
            state_value: alarm.state_value.clone(),
            state_reason: alarm.state_reason.clone(),
            state_reason_data: alarm.state_reason_data.clone(),
            state_updated_secs: alarm.state_updated_timestamp.as_ref().map(from_timestamp),
        }
    }

    /// One log alarm's state.
    pub fn of_log_alarm(alarm: &LogAlarm) -> Self {
        Self {
            state_value: alarm.state_value.clone(),
            state_reason: alarm.state_reason.clone(),
            state_reason_data: alarm.state_reason_data.clone(),
            state_updated_secs: alarm.state_updated_timestamp.as_ref().map(from_timestamp),
        }
    }
}

/// The JSON record of a state transition: the alarm's state before and after the write.
///
/// A consumer of alarm history reads the state a transition moved the alarm to out of this
/// document (`HistoryData` → `newState` → `stateValue`), which is why both states are named
/// rather than only the reason text the summary already carries.
pub fn state_transition_history_data(
    previous: &AlarmStateSnapshot,
    updated: &AlarmStateSnapshot,
) -> String {
    // `StateReasonData` is itself up to 4000 characters, so two of them need not fit the
    // modelled bound: the second candidate is the same document without them, which always
    // does (a state value plus two reasons of at most 1023 each).
    bounded_history_data(&[
        transition_document(previous, updated, true),
        transition_document(previous, updated, false),
    ])
}

/// The JSON record of a metric alarm's state transition.
pub fn state_update_history_data(previous: &MetricAlarm, updated: &MetricAlarm) -> String {
    state_transition_history_data(
        &AlarmStateSnapshot::of_metric_alarm(previous),
        &AlarmStateSnapshot::of_metric_alarm(updated),
    )
}

/// The JSON record of a configuration write: the alarm the call stored, and — for a write that
/// overwrote an alarm that already existed — the ORIGINAL values of the members it changed.
///
/// Takes the snapshot documents rather than an alarm, so the metric-alarm, composite-alarm and
/// log-alarm writers share one record layout and one length bound.
pub(super) fn configuration_history_data(
    previous: Option<Value>,
    updated: Value,
    alarm_name: Option<&str>,
) -> String {
    let (kind, stored_member) = match previous {
        Some(_) => (CONFIGURATION_UPDATE, "updatedAlarm"),
        None => (CONFIGURATION_CREATE, "createdAlarm"),
    };
    let heading = |document: Value| -> Value {
        Value::Object(
            [
                ("version".to_string(), Value::from(HISTORY_DATA_VERSION)),
                ("type".to_string(), Value::from(kind)),
                (stored_member.to_string(), document),
            ]
            .into_iter()
            .collect(),
        )
    };
    // An alarm's configuration is itself unbounded in the modelled sense (up to 20 metric
    // queries, each with an expression of up to 2048 characters, or a rule of up to 10240),
    // so the candidates drop fidelity in order: the stored alarm with the delta an update
    // made, then the stored alarm alone, then the name of the alarm the item already belongs
    // to — which always fits.
    let mut full = heading(updated.clone());
    if let (Some(previous), Value::Object(members)) = (previous.as_ref(), &mut full) {
        members.insert(
            "originalUpdatedFields".to_string(),
            original_updated_fields(previous, &updated),
        );
    }
    bounded_history_data(&[
        full,
        heading(updated),
        heading(Value::Object(
            [(
                "alarmName".to_string(),
                alarm_name.map_or(Value::Null, Value::from),
            )]
            .into_iter()
            .collect(),
        )),
    ])
}

/// The ORIGINAL values of the members an update CHANGED: one entry per member whose value
/// differs between the alarm the write replaced and the alarm it stored, carrying the value the
/// REPLACED alarm had.
///
/// What an update's history item is read for is the DELTA — which members the write changed and
/// what they were before it — so the record names those members alone rather than a second full
/// snapshot a consumer would have to diff itself. A member the stored alarm carries and the
/// replaced one did not had no original value at all, which is what its entry reports.
fn original_updated_fields(previous: &Value, updated: &Value) -> Value {
    let (Value::Object(previous), Value::Object(updated)) = (previous, updated) else {
        return Value::Object(Map::new());
    };
    let mut changed = Map::new();
    for (member, original) in previous {
        if updated.get(member) != Some(original) {
            changed.insert(member.clone(), original.clone());
        }
    }
    for member in updated.keys() {
        if !previous.contains_key(member) {
            changed.insert(member.clone(), Value::Null);
        }
    }
    Value::Object(changed)
}

/// The `HistorySummary` of a `ConfigurationUpdate` item: "A summary of the alarm history, in
/// text format." (`AlarmHistoryItem$HistorySummary`).
///
/// A summary NAMES the alarm it is about, in double quotes, and says what the write did to it:
/// a write that stored an alarm of a name the store held none of CREATED it, and one that
/// replaced an alarm that already existed UPDATED it. The name is the variable part, so the one
/// reading of that wording is shared by every writer that records such an item, whatever kind of
/// alarm it writes — a page of history that mixes several alarms (`DescribeAlarmHistory` can be
/// called with no `AlarmName`) attributes each item to its own alarm from the summary alone.
///
/// Bounded like every other summary this store writes: an alarm name is `@length 1..255`
/// (`AlarmName`) while the summary holding it is `@length 1..255` too, so a name long enough to
/// overrun the bound is cut at it rather than dropping the whole summary.
pub fn configuration_update_history_summary(alarm_name: &str, is_update: bool) -> String {
    let what_the_write_did = if is_update { "updated" } else { "created" };
    alarm_actions::bounded(format!("Alarm \"{alarm_name}\" {what_the_write_did}"))
}

/// The JSON record of a METRIC alarm's configuration write.
pub fn configuration_update_history_data(
    previous: Option<&MetricAlarm>,
    updated: &MetricAlarm,
) -> String {
    configuration_history_data(
        previous.map(metric_alarm_history_snapshot),
        metric_alarm_history_snapshot(updated),
        updated.alarm_name.as_deref(),
    )
}

/// The JSON record of a COMPOSITE alarm's configuration write.
pub fn composite_configuration_update_history_data(
    previous: Option<&CompositeAlarm>,
    updated: &CompositeAlarm,
) -> String {
    configuration_history_data(
        previous.map(composite_alarm_history_snapshot),
        composite_alarm_history_snapshot(updated),
        updated.alarm_name.as_deref(),
    )
}

/// The JSON record of a LOG alarm's configuration write.
pub fn log_configuration_update_history_data(
    previous: Option<&LogAlarm>,
    updated: &LogAlarm,
) -> String {
    configuration_history_data(
        previous.map(log_alarm_history_snapshot),
        log_alarm_history_snapshot(updated),
        updated.alarm_name.as_deref(),
    )
}

/// One METRIC alarm as an alarm-history document records it
/// ([`AlarmDocument::History`]).
pub(crate) fn metric_alarm_history_snapshot(alarm: &MetricAlarm) -> Value {
    metric_alarm_document(alarm, AlarmDocument::History)
}

/// One COMPOSITE alarm as an alarm-history document records it.
pub(crate) fn composite_alarm_history_snapshot(alarm: &CompositeAlarm) -> Value {
    composite_alarm_document(alarm, AlarmDocument::History)
}

/// One LOG alarm as an alarm-history document records it.
pub(crate) fn log_alarm_history_snapshot(alarm: &LogAlarm) -> Value {
    log_alarm_document(alarm, AlarmDocument::History)
}

/// The instant a document states a modelled timestamp as ([`document_instant`]), for a
/// timestamp the alarm carries.
///
/// Every instant INSIDE one of this service's JSON documents is written in that one form, so a
/// snapshot's timestamps read like every other instant a document reports rather than like the
/// modelled member beside them.
fn document_instant_member(stamp: Option<&::aws_smithy_types::DateTime>) -> Option<Value> {
    stamp
        .map(from_timestamp)
        .and_then(document_instant)
        .map(Value::from)
}

/// The ACTION members of an alarm document, in the order and under the names the serialisation
/// `style` states them.
///
/// The configuration record states them where the model lists them, under the model's own
/// member names; an alarm-history document states them LAST, and spells the OK list all in
/// lower case (`okactions`).
fn insert_action_members(
    document: &mut Map<String, Value>,
    actions_enabled: Option<bool>,
    ok_actions: &Option<Vec<String>>,
    alarm_actions: &Option<Vec<String>>,
    insufficient_data_actions: &Option<Vec<String>>,
    style: AlarmDocument,
) {
    insert_member(document, "actionsEnabled", actions_enabled.map(Value::from));
    if style == AlarmDocument::Configuration {
        insert_member(document, "okActions", json_strings(ok_actions));
    }
    insert_member(document, "alarmActions", json_strings(alarm_actions));
    insert_member(
        document,
        "insufficientDataActions",
        json_strings(insufficient_data_actions),
    );
    if style == AlarmDocument::History {
        insert_member(document, "okactions", json_strings(ok_actions));
    }
}

/// The STATE members an alarm-history snapshot states after an alarm's configuration: the state
/// the alarm is standing in and the instant it last reported it.
///
/// A snapshot records the alarm the write LEFT BEHIND, and an alarm is in a state whatever a
/// configuration write does to it ("When you update an existing alarm, its state is left
/// unchanged", `PutMetricAlarm`), so the state belongs to the snapshot even though it is not
/// part of the configuration the write stored. WHY the alarm is in that state is not: the
/// explanation of a state belongs to the state-transition item that moved the alarm there,
/// which names the reason on both sides of the move ([`alarm_state_document`]).
fn insert_state_members(
    document: &mut Map<String, Value>,
    state: &AlarmStateSnapshot,
    state_updated: Option<&::aws_smithy_types::DateTime>,
) {
    insert_member(document, "stateValue", json_text(&state.state_value));
    insert_member(
        document,
        "stateUpdatedTimestamp",
        document_instant_member(state_updated),
    );
}

/// A log alarm's configuration as a history document records it: every configuration member
/// the alarm carries, under the model's member name in lower camel case, and only those it
/// carries. The state members are NOT part of it, for the same reason they are not part of a
/// metric alarm's: "If you call this operation with the name of an existing log alarm, the
/// operation replaces the previous configuration of that alarm." (`PutLogAlarm`), so a
/// configuration write records configuration only.
///
/// `pub(crate)` for the reason [`alarm_configuration_document`] gives.
pub(crate) fn log_alarm_configuration_document(alarm: &LogAlarm) -> Value {
    log_alarm_document(alarm, AlarmDocument::Configuration)
}

/// One log alarm as the serialisation `style` states it.
fn log_alarm_document(alarm: &LogAlarm, style: AlarmDocument) -> Value {
    let mut config = Map::new();
    insert_member(&mut config, "alarmName", json_text(&alarm.alarm_name));
    insert_member(&mut config, "alarmArn", json_text(&alarm.alarm_arn));
    insert_member(
        &mut config,
        "alarmDescription",
        json_text(&alarm.alarm_description),
    );
    if style == AlarmDocument::History {
        insert_member(
            &mut config,
            "alarmConfigurationUpdatedTimestamp",
            document_instant_member(alarm.alarm_configuration_updated_timestamp.as_ref()),
        );
    }
    if style == AlarmDocument::Configuration {
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    insert_member(
        &mut config,
        "scheduledQueryConfiguration",
        alarm
            .scheduled_query_configuration
            .as_ref()
            .map(scheduled_query_configuration_document),
    );
    insert_member(
        &mut config,
        "queryResultsToEvaluate",
        alarm.query_results_to_evaluate.map(Value::from),
    );
    insert_member(
        &mut config,
        "queryResultsToAlarm",
        alarm.query_results_to_alarm.map(Value::from),
    );
    insert_member(&mut config, "threshold", alarm.threshold.map(Value::from));
    insert_member(
        &mut config,
        "comparisonOperator",
        json_text(&alarm.comparison_operator),
    );
    insert_member(
        &mut config,
        "treatMissingData",
        json_text(&alarm.treat_missing_data),
    );
    insert_member(
        &mut config,
        "actionLogLineCount",
        alarm.action_log_line_count.map(Value::from),
    );
    insert_member(
        &mut config,
        "actionLogLineRoleArn",
        json_text(&alarm.action_log_line_role_arn),
    );
    insert_member(
        &mut config,
        "warmUpConfiguration",
        alarm
            .warm_up_configuration
            .as_ref()
            .map(warm_up_configuration_document),
    );
    insert_member(
        &mut config,
        "actionConfiguration",
        alarm
            .action_configuration
            .as_ref()
            .map(action_configuration_document),
    );
    if style == AlarmDocument::History {
        insert_state_members(
            &mut config,
            &AlarmStateSnapshot::of_log_alarm(alarm),
            alarm.state_updated_timestamp.as_ref(),
        );
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    Value::Object(config)
}

/// A log alarm's `ScheduledQueryConfiguration` as a history document records it.
pub(super) fn scheduled_query_configuration_document(configuration: &ScheduledQueryConfiguration) -> Value {
    let mut document = Map::new();
    document.insert(
        "queryString".to_string(),
        Value::from(configuration.query_string.clone()),
    );
    insert_member(
        &mut document,
        "logGroupIdentifiers",
        json_strings(&configuration.log_group_identifiers),
    );
    insert_member(&mut document, "queryARN", json_text(&configuration.query_arn));
    document.insert(
        "scheduledQueryRoleARN".to_string(),
        Value::from(configuration.scheduled_query_role_arn.clone()),
    );
    let schedule = &configuration.schedule_configuration;
    let mut schedule_document = Map::new();
    schedule_document.insert(
        "scheduleExpression".to_string(),
        Value::from(schedule.schedule_expression.clone()),
    );
    schedule_document.insert(
        "startTimeOffset".to_string(),
        Value::from(schedule.start_time_offset),
    );
    insert_member(
        &mut schedule_document,
        "endTimeOffset",
        schedule.end_time_offset.map(Value::from),
    );
    document.insert(
        "scheduleConfiguration".to_string(),
        Value::Object(schedule_document),
    );
    document.insert(
        "aggregationExpression".to_string(),
        Value::from(configuration.aggregation_expression.clone()),
    );
    Value::Object(document)
}

/// A `WarmUpConfiguration` as a history document records it.
pub(super) fn warm_up_configuration_document(warm_up: &WarmUpConfiguration) -> Value {
    let mut document = Map::new();
    document.insert(
        "warmUpPeriodDurationInMinutes".to_string(),
        Value::from(warm_up.warm_up_period_duration_in_minutes),
    );
    insert_member(
        &mut document,
        "onlyStartEvaluatingAfterWarmUpPeriodEnds",
        warm_up
            .only_start_evaluating_after_warm_up_period_ends
            .map(Value::from),
    );
    Value::Object(document)
}

/// A composite alarm's configuration as a history document records it: every configuration
/// member the alarm carries, under the model's member name in lower camel case, and only
/// those it carries. The state members are NOT part of it, for the same reason they are not
/// part of a metric alarm's: "When you update an existing alarm, its state is left
/// unchanged" (`PutCompositeAlarm`), so a configuration write records no state.
///
/// `pub(crate)` for the reason [`alarm_configuration_document`] gives.
pub(crate) fn composite_alarm_configuration_document(alarm: &CompositeAlarm) -> Value {
    composite_alarm_document(alarm, AlarmDocument::Configuration)
}

/// One composite alarm as the serialisation `style` states it.
fn composite_alarm_document(alarm: &CompositeAlarm, style: AlarmDocument) -> Value {
    let mut config = Map::new();
    insert_member(&mut config, "alarmName", json_text(&alarm.alarm_name));
    insert_member(&mut config, "alarmArn", json_text(&alarm.alarm_arn));
    insert_member(
        &mut config,
        "alarmDescription",
        json_text(&alarm.alarm_description),
    );
    if style == AlarmDocument::History {
        insert_member(
            &mut config,
            "alarmConfigurationUpdatedTimestamp",
            document_instant_member(alarm.alarm_configuration_updated_timestamp.as_ref()),
        );
    }
    insert_member(&mut config, "alarmRule", json_text(&alarm.alarm_rule));
    if style == AlarmDocument::Configuration {
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    insert_member(
        &mut config,
        "actionsSuppressor",
        json_text(&alarm.actions_suppressor),
    );
    insert_member(
        &mut config,
        "actionsSuppressorWaitPeriod",
        alarm.actions_suppressor_wait_period.map(Value::from),
    );
    insert_member(
        &mut config,
        "actionsSuppressorExtensionPeriod",
        alarm.actions_suppressor_extension_period.map(Value::from),
    );
    insert_member(
        &mut config,
        "actionConfiguration",
        alarm
            .action_configuration
            .as_ref()
            .map(action_configuration_document),
    );
    if style == AlarmDocument::History {
        insert_state_members(
            &mut config,
            &AlarmStateSnapshot::of_composite_alarm(alarm),
            alarm.state_updated_timestamp.as_ref(),
        );
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    Value::Object(config)
}

/// One state-transition document, with or without the (potentially long) reason data.
pub(super) fn transition_document(
    previous: &AlarmStateSnapshot,
    updated: &AlarmStateSnapshot,
    reason_data: bool,
) -> Value {
    Value::Object(
        [
            ("version".to_string(), Value::from(HISTORY_DATA_VERSION)),
            (
                "oldState".to_string(),
                alarm_state_document(previous, reason_data),
            ),
            (
                "newState".to_string(),
                alarm_state_document(updated, reason_data),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

/// One alarm state as a transition document records it.
///
/// `stateReasonData` is itself JSON ("The reason that this alarm is set to this specific state,
/// in JSON format." — `MetricAlarm$StateReasonData`), so it is embedded as JSON rather than as
/// a quoted string whenever it parses as JSON.
pub(super) fn alarm_state_document(alarm: &AlarmStateSnapshot, reason_data: bool) -> Value {
    let mut state = Map::new();
    insert_member(&mut state, "stateValue", json_text(&alarm.state_value));
    insert_member(&mut state, "stateReason", json_text(&alarm.state_reason));
    if reason_data {
        insert_member(
            &mut state,
            "stateReasonData",
            alarm.state_reason_data.as_deref().map(embedded_json),
        );
    }
    Value::Object(state)
}

/// An alarm's configuration as a history document records it: every configuration member the
/// alarm carries, under the model's member name in lower camel case, and only those it carries
/// — a member the alarm does not have is absent from the document, exactly as it is absent
/// from the alarm. The state members are NOT part of it: "When you update an existing alarm,
/// its state is left unchanged" (`PutMetricAlarm`), so a configuration write records no state.
///
/// `pub(crate)` because it is also what decides whether a write CHANGED an alarm's
/// configuration at all ([`alarm_configuration_events`]): one record of a configuration serves
/// both, so a write the history reports as an update cannot be one the event path reports as a
/// no-op.
pub(crate) fn alarm_configuration_document(alarm: &MetricAlarm) -> Value {
    metric_alarm_document(alarm, AlarmDocument::Configuration)
}

/// One metric alarm as the serialisation `style` states it.
fn metric_alarm_document(alarm: &MetricAlarm, style: AlarmDocument) -> Value {
    let mut config = Map::new();
    insert_member(&mut config, "alarmName", json_text(&alarm.alarm_name));
    insert_member(&mut config, "alarmArn", json_text(&alarm.alarm_arn));
    insert_member(
        &mut config,
        "alarmDescription",
        json_text(&alarm.alarm_description),
    );
    if style == AlarmDocument::History {
        insert_member(
            &mut config,
            "alarmConfigurationUpdatedTimestamp",
            document_instant_member(alarm.alarm_configuration_updated_timestamp.as_ref()),
        );
    }
    if style == AlarmDocument::Configuration {
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    insert_member(&mut config, "namespace", json_text(&alarm.namespace));
    insert_member(&mut config, "metricName", json_text(&alarm.metric_name));
    insert_member(&mut config, "statistic", json_text(&alarm.statistic));
    insert_member(
        &mut config,
        "extendedStatistic",
        json_text(&alarm.extended_statistic),
    );
    insert_member(
        &mut config,
        "dimensions",
        alarm
            .dimensions
            .as_deref()
            .map(|dimensions| dimensions_document(dimensions, style)),
    );
    insert_member(&mut config, "period", alarm.period.map(Value::from));
    insert_member(&mut config, "unit", json_text(&alarm.unit));
    insert_member(
        &mut config,
        "evaluationPeriods",
        alarm.evaluation_periods.map(Value::from),
    );
    insert_member(
        &mut config,
        "datapointsToAlarm",
        alarm.datapoints_to_alarm.map(Value::from),
    );
    insert_member(&mut config, "threshold", alarm.threshold.map(Value::from));
    insert_member(
        &mut config,
        "comparisonOperator",
        json_text(&alarm.comparison_operator),
    );
    insert_member(
        &mut config,
        "treatMissingData",
        json_text(&alarm.treat_missing_data),
    );
    insert_member(
        &mut config,
        "evaluateLowSampleCountPercentile",
        json_text(&alarm.evaluate_low_sample_count_percentile),
    );
    insert_member(
        &mut config,
        "thresholdMetricId",
        json_text(&alarm.threshold_metric_id),
    );
    insert_member(
        &mut config,
        "metrics",
        alarm.metrics.as_deref().map(|queries| {
            Value::Array(
                queries
                    .iter()
                    .map(|query| metric_query_document(query, style))
                    .collect(),
            )
        }),
    );
    insert_member(
        &mut config,
        "evaluationInterval",
        alarm.evaluation_interval.map(Value::from),
    );
    insert_member(
        &mut config,
        "evaluationWindow",
        alarm.evaluation_window.as_ref().map(evaluation_window_document),
    );
    insert_member(
        &mut config,
        "warmUpConfiguration",
        alarm
            .warm_up_configuration
            .as_ref()
            .map(warm_up_configuration_document),
    );
    insert_member(
        &mut config,
        "evaluationCriteria",
        alarm
            .evaluation_criteria
            .as_ref()
            .map(evaluation_criteria_document),
    );
    insert_member(
        &mut config,
        "actionConfiguration",
        alarm
            .action_configuration
            .as_ref()
            .map(action_configuration_document),
    );
    if style == AlarmDocument::History {
        insert_state_members(
            &mut config,
            &AlarmStateSnapshot::of_metric_alarm(alarm),
            alarm.state_updated_timestamp.as_ref(),
        );
        insert_action_members(
            &mut config,
            alarm.actions_enabled,
            &alarm.ok_actions,
            &alarm.alarm_actions,
            &alarm.insufficient_data_actions,
            style,
        );
    }
    Value::Object(config)
}

/// A dimension list as the serialisation `style` states it.
///
/// A configuration record states a dimension the way the model lists it, its name before its
/// value; an alarm-history document states the VALUE first.
pub(super) fn dimensions_document(dimensions: &[Dimension], style: AlarmDocument) -> Value {
    Value::Array(
        dimensions
            .iter()
            .map(|dimension| {
                let name = ("name".to_string(), Value::from(dimension.name.clone()));
                let value = ("value".to_string(), Value::from(dimension.value.clone()));
                let members = match style {
                    AlarmDocument::Configuration => [name, value],
                    AlarmDocument::History => [value, name],
                };
                Value::Object(members.into_iter().collect())
            })
            .collect(),
    )
}

/// One metric-math query of a `Metrics` alarm as the serialisation `style` states it.
pub(super) fn metric_query_document(query: &MetricDataQuery, style: AlarmDocument) -> Value {
    let mut document = Map::new();
    document.insert("id".to_string(), Value::from(query.id.clone()));
    insert_member(&mut document, "expression", json_text(&query.expression));
    insert_member(&mut document, "label", json_text(&query.label));
    insert_member(
        &mut document,
        "returnData",
        query.return_data.map(Value::from),
    );
    insert_member(&mut document, "period", query.period.map(Value::from));
    insert_member(&mut document, "accountId", json_text(&query.account_id));
    insert_member(
        &mut document,
        "metricStat",
        query.metric_stat.as_ref().map(|stat| {
            let mut metric = Map::new();
            insert_member(&mut metric, "namespace", json_text(&stat.metric.namespace));
            insert_member(
                &mut metric,
                "metricName",
                json_text(&stat.metric.metric_name),
            );
            insert_member(
                &mut metric,
                "dimensions",
                stat.metric
                    .dimensions
                    .as_deref()
                    .map(|dimensions| dimensions_document(dimensions, style)),
            );
            let mut document = Map::new();
            document.insert("metric".to_string(), Value::Object(metric));
            document.insert("period".to_string(), Value::from(stat.period));
            document.insert("stat".to_string(), Value::from(stat.stat.clone()));
            insert_member(&mut document, "unit", json_text(&stat.unit));
            Value::Object(document)
        }),
    );
    Value::Object(document)
}

/// An `EvaluationWindow` union as a history document records it: the one member it carries.
pub(super) fn evaluation_window_document(window: &EvaluationWindow) -> Value {
    let (member, document) = match window {
        EvaluationWindow::WallClockWindow(wall_clock) => {
            let mut inner = Map::new();
            insert_member(&mut inner, "timezone", json_text(&wall_clock.timezone));
            ("wallClockWindow", Value::Object(inner))
        }
        EvaluationWindow::SlidingWindow(_) => ("slidingWindow", Value::Object(Map::new())),
    };
    Value::Object([(member.to_string(), document)].into_iter().collect())
}

/// An `EvaluationCriteria` union as a history document records it.
pub(super) fn evaluation_criteria_document(criteria: &EvaluationCriteria) -> Value {
    let EvaluationCriteria::PromQLCriteria(promql) = criteria;
    let mut inner = Map::new();
    inner.insert("query".to_string(), Value::from(promql.query.clone()));
    insert_member(
        &mut inner,
        "pendingPeriod",
        promql.pending_period.map(Value::from),
    );
    insert_member(
        &mut inner,
        "recoveryPeriod",
        promql.recovery_period.map(Value::from),
    );
    Value::Object(
        [("promQLCriteria".to_string(), Value::Object(inner))]
            .into_iter()
            .collect(),
    )
}

/// An `ActionConfiguration` as a history document records it.
pub(super) fn action_configuration_document(configuration: &ActionConfiguration) -> Value {
    let mut document = Map::new();
    insert_member(
        &mut document,
        "alarmTagsPropagation",
        configuration
            .alarm_tags_propagation
            .as_ref()
            .map(|propagation| {
                let mut inner = Map::new();
                inner.insert("enabled".to_string(), Value::from(propagation.enabled));
                insert_member(
                    &mut inner,
                    "includedTagKeys",
                    json_strings(&propagation.included_tag_keys),
                );
                insert_member(
                    &mut inner,
                    "excludedTagKeys",
                    json_strings(&propagation.excluded_tag_keys),
                );
                Value::Object(inner)
            }),
    );
    Value::Object(document)
}

/// The first candidate whose rendering fits the modelled `HistoryData` length.
///
/// A document is JSON, so it can never be truncated to fit: a reader parses it. Dropping the
/// longest part and re-rendering is what keeps it both parseable and inside
/// [`MAX_HISTORY_DATA_LEN`]; the last candidate a caller passes is one that always fits, and
/// the bare version object is the floor if even that one does not.
pub(super) fn bounded_history_data(candidates: &[Value]) -> String {
    for candidate in candidates {
        let rendered = candidate.to_string();
        if (1..=MAX_HISTORY_DATA_LEN).contains(&rendered.len()) {
            return rendered;
        }
    }
    Value::Object(
        [("version".to_string(), Value::from(HISTORY_DATA_VERSION))]
            .into_iter()
            .collect(),
    )
    .to_string()
}

/// Add `value` to a document under `member`, and nothing at all when there is no value.
pub(super) fn insert_member(document: &mut Map<String, Value>, member: &str, value: Option<Value>) {
    if let Some(value) = value {
        document.insert(member.to_string(), value);
    }
}

/// An optional string member as a JSON string.
pub(super) fn json_text(value: &Option<String>) -> Option<Value> {
    value.as_deref().map(Value::from)
}

/// An optional string list as a JSON array of strings.
pub(super) fn json_strings(values: &Option<Vec<String>>) -> Option<Value> {
    values
        .as_deref()
        .map(|values| Value::Array(values.iter().map(|value| Value::from(value.as_str())).collect()))
}

/// A member whose stored value is itself JSON, embedded as JSON when it parses as JSON and as
/// the string the writer stored when it does not — a caller may store anything in
/// `StateReasonData`, and a history document has to stay parseable either way.
pub(super) fn embedded_json(raw: &str) -> Value {
    ::serde_json::from_str(raw).unwrap_or_else(|_| Value::from(raw))
}
