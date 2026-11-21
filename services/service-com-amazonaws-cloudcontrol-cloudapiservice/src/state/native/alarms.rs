// SPDX-License-Identifier: CC0-1.0

//! `AWS::CloudWatch::Alarm`, as it lives in the Amazon CloudWatch service's own store.
//!
//! A metric alarm is a resource of the CloudWatch service: `cloudwatch:PutMetricAlarm`
//! creates it, `cloudwatch:DescribeAlarms` reports it, `cloudwatch:TagResource` tags it and
//! `cloudwatch:DeleteAlarms` removes it, and the record of it lives in that service's state
//! in the same shared engine. So this provider holds a handle to THAT store and nothing of
//! its own: an alarm this emulator holds is one Cloud Control API resolves whichever API
//! created it, an alarm CloudWatch no longer holds is one Cloud Control API reports as not
//! found, and an alarm a `CreateResource` provisioned is one that service's own readers
//! report and its own evaluation then grades.
//!
//! An alarm's name "must be unique within the Region" (`PutMetricAlarmInput$AlarmName`), and
//! the owning service holds its alarms in ONE namespace keyed by that name — so a reference
//! resolves to the same alarm whichever Region the call naming it was signed for, and the
//! Region a create or an update was signed for is the Region the alarm's own ARN is built in.
//!
//! The resource model is the type's registry schema. Catalog source: the
//! `AWS::CloudWatch::Alarm` resource provider schema's `properties`, whose
//! `primaryIdentifier` is `/properties/AlarmName`. Every writable property of that schema is
//! applied here onto the owning record, and the read-only `Arn`
//! (`readOnlyProperties: ["/properties/Arn"]`) is reported beside them.
//!
//! The one property this path does not apply is `Metrics` — the alarm's metric math or
//! multi-metric configuration — because each entry of it is a metric query the owning
//! service parses and resolves on its own write path (`cloudwatch::state::metric_query`),
//! which this path, applying the work the clock owes under the store's guard, does not run.
//! A model that names it is refused rather than accepted and dropped, whatever the alarm
//! holds: no read reports that property, so nothing a client round-trips carries it, and a
//! value accepted here would be one no later read reports. The queries an alarm already
//! holds stay exactly as the owning service wrote them.

use ::std::time::{SystemTime, UNIX_EPOCH};

use ::serde_json::{Map, Value};
use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ops::types::{
    AlarmHistoryItem, Dimension, MetricAlarm, Tag, COMPARISON_OPERATOR_VALUES, STATISTIC_VALUES,
};
use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cw;

use super::{require_value, text, NativeRefusal, NativeResourceProvider};

/// The `TypeName` this provider stands behind.
pub const TYPE_NAME: &str = "AWS::CloudWatch::Alarm";

/// The property that carries the type's primary identifier, which the schema's
/// `primaryIdentifier` points at (`/properties/AlarmName`).
const IDENTIFIER_PROPERTY: &str = "AlarmName";

/// The type's one read-only property: the alarm's ARN
/// (`readOnlyProperties: ["/properties/Arn"]`), built by the owning service's own ARN
/// builder.
const ARN: &str = "Arn";

/// The alarm's description (`AlarmDescription`).
const ALARM_DESCRIPTION: &str = "AlarmDescription";

/// Whether the alarm's actions run (`ActionsEnabled`).
const ACTIONS_ENABLED: &str = "ActionsEnabled";

/// The actions the alarm takes on moving to `ALARM` (`AlarmActions`).
const ALARM_ACTIONS: &str = "AlarmActions";

/// The actions the alarm takes on moving to `OK` (`OKActions` — the schema spells the state
/// in capitals).
const OK_ACTIONS: &str = "OKActions";

/// The actions the alarm takes on moving to `INSUFFICIENT_DATA`
/// (`InsufficientDataActions`).
const INSUFFICIENT_DATA_ACTIONS: &str = "InsufficientDataActions";

/// How the alarm compares the metric to the threshold (`ComparisonOperator`).
const COMPARISON_OPERATOR: &str = "ComparisonOperator";

/// How many of the evaluation periods must breach (`DatapointsToAlarm`).
const DATAPOINTS_TO_ALARM: &str = "DatapointsToAlarm";

/// The dimensions of the metric the alarm watches (`Dimensions`).
const DIMENSIONS: &str = "Dimensions";

/// The `Name` member of one entry of [`DIMENSIONS`].
const DIMENSION_NAME: &str = "Name";

/// The `Value` member of one entry of [`DIMENSIONS`].
const DIMENSION_VALUE: &str = "Value";

/// How a percentile alarm treats a period of low sample count
/// (`EvaluateLowSampleCountPercentile`).
const EVALUATE_LOW_SAMPLE_COUNT_PERCENTILE: &str = "EvaluateLowSampleCountPercentile";

/// How many periods the alarm evaluates (`EvaluationPeriods`).
const EVALUATION_PERIODS: &str = "EvaluationPeriods";

/// The percentile statistic the alarm folds its metric with (`ExtendedStatistic`).
const EXTENDED_STATISTIC: &str = "ExtendedStatistic";

/// The metric the alarm watches (`MetricName`).
const METRIC_NAME: &str = "MetricName";

/// The metric queries an alarm over metric math watches (`Metrics`) — reported by a read,
/// and applied by nothing here (see this module's header).
const METRICS: &str = "Metrics";

/// The namespace of the metric the alarm watches (`Namespace`).
const NAMESPACE: &str = "Namespace";

/// The period, in seconds, the alarm evaluates the metric over (`Period`).
const PERIOD: &str = "Period";

/// The statistic the alarm folds its metric with (`Statistic`).
const STATISTIC: &str = "Statistic";

/// The value the alarm compares the metric against (`Threshold`).
const THRESHOLD: &str = "Threshold";

/// The id of the anomaly-detection query that supplies the threshold
/// (`ThresholdMetricId`).
const THRESHOLD_METRIC_ID: &str = "ThresholdMetricId";

/// How the alarm treats periods with no data (`TreatMissingData`).
const TREAT_MISSING_DATA: &str = "TreatMissingData";

/// The unit of the metric the alarm watches (`Unit`).
const UNIT: &str = "Unit";

/// The alarm's tag set (`Tags`), a list of `{ Key, Value }` pairs.
const TAGS: &str = "Tags";

/// The CloudWatch store the resources of the `AWS::CloudWatch::Alarm` type live in — that
/// service's OWN state struct, reached through the shared engine, so an alarm this emulator
/// holds is the alarm Cloud Control API acts on whichever API created it.
pub type CloudWatchState =
    ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::ComAmazonawsCloudwatchGraniteserviceversion20100801State;

/// The CloudWatch metric alarms of one engine, as Cloud Control API resources.
pub struct AlarmProvider {
    /// The owning service's store. A handle, so every answer is read from the live store.
    state: ::engine::ServiceState<CloudWatchState>,
}

impl AlarmProvider {
    /// The provider for the CloudWatch store THIS engine carries, or `None` when it carries
    /// none.
    pub fn for_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        cw::service_state(engine).map(|state| Self { state })
    }
}

impl NativeResourceProvider for AlarmProvider {
    fn type_name(&self) -> &'static str {
        TYPE_NAME
    }

    fn identifier_property(&self) -> &'static str {
        IDENTIFIER_PROPERTY
    }

    fn read(&self, region: &str, identifier: &str) -> Option<Value> {
        // One namespace per account, so an alarm is the same alarm whichever Region a call
        // that names it was signed for: see this module's header.
        let _ = region;
        self.state
            .lock()
            .alarms
            .get(identifier)
            .map(|stored| model(identifier, stored))
    }

    fn list(&self, region: &str, after: Option<&str>, limit: usize) -> Vec<(String, Value)> {
        // One namespace, whichever Region the call was signed for.
        let _ = region;
        let store = self.state.lock();
        // The owning store keys its metric alarms by name; the map is a hash map there, so
        // the window a listing pages over is selected here rather than read as a range —
        // at most `limit` names, the smallest that sort after `after`, so the memory one
        // listing costs is the page it answers.
        let mut window: ::std::collections::BTreeSet<&String> = ::std::collections::BTreeSet::new();
        for name in store.alarms.keys() {
            if after.is_some_and(|after| name.as_str() <= after) {
                continue;
            }
            window.insert(name);
            if window.len() > limit {
                window.pop_last();
            }
        }
        window
            .into_iter()
            .filter_map(|name| {
                store
                    .alarms
                    .get(name)
                    .map(|stored| (name.clone(), model(name, stored)))
            })
            .collect()
    }

    fn create(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let now = epoch_secs(at);
        let desired = DesiredModel::read(identifier, properties, &cw::alarm_arn(region, identifier))?;
        let mut store = self.state.lock();
        // The alarm is created in the owning service's store under its own rules: a name it
        // already holds — of this kind or of any other, since "this name must be unique
        // within the Region" — is that service's own already-exists verdict, and its quota
        // bounds what this create may add.
        if store.alarm_kind(identifier).is_some() {
            return Err(NativeRefusal::AlreadyExists);
        }
        if store.alarm_count() >= cw::MAX_ALARMS {
            return Err(NativeRefusal::AtCapacity(format!(
                "the quota of {} alarms has already been reached",
                cw::MAX_ALARMS
            )));
        }
        write(&mut store, identifier, region, desired, now, false);
        let stored = store.alarms.get(identifier).ok_or(NativeRefusal::NotFound)?;
        Ok(model(identifier, stored))
    }

    fn update(
        &self,
        region: &str,
        identifier: &str,
        properties: &Value,
        at: SystemTime,
    ) -> Result<Value, NativeRefusal> {
        let now = epoch_secs(at);
        let mut store = self.state.lock();
        if !store.alarms.contains_key(identifier) {
            return Err(NativeRefusal::NotFound);
        }
        let desired = DesiredModel::read(identifier, properties, &cw::alarm_arn(region, identifier))?;
        write(&mut store, identifier, region, desired, now, true);
        let stored = store.alarms.get(identifier).ok_or(NativeRefusal::NotFound)?;
        Ok(model(identifier, stored))
    }

    fn delete(&self, region: &str, identifier: &str, at: SystemTime) -> Result<(), NativeRefusal> {
        // One namespace, whichever Region the call was signed for.
        let _ = region;
        let now = epoch_secs(at);
        let mut store = self.state.lock();
        if !store.alarms.contains_key(identifier) {
            return Err(NativeRefusal::NotFound);
        }
        // An alarm a composite alarm references cannot be deleted while that reference
        // stands — the owning service raises its own error about it, which the handler maps
        // to no verdict of its own, and the alarm stays.
        if !store.composite_alarm_referrers(identifier).is_empty() {
            return Err(NativeRefusal::Rejected(format!(
                "Cannot delete {identifier} as there are composite alarm(s) depending on it."
            )));
        }
        store.alarms.remove(identifier);
        // A deleted alarm is an input every composite alarm whose rule references it loses,
        // so those alarms are owed their own evaluation from this instant.
        store.make_parents_due(identifier, now);
        Ok(())
    }
}

/// `at` in seconds since the Unix epoch, which is the clock the owning service's records are
/// kept against. An instant before the epoch, which no clock reports, reads as the epoch.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// What a resource model asks a metric alarm to be: the properties this path APPLIES, having
/// been checked against the owning service's own rules.
struct DesiredModel {
    /// The alarm's description.
    alarm_description: Option<String>,
    /// Whether the alarm's actions run — "the default is TRUE"
    /// (`PutMetricAlarmInput$ActionsEnabled`).
    actions_enabled: Option<bool>,
    /// The actions the alarm takes on moving to `ALARM`.
    alarm_actions: Vec<String>,
    /// The actions the alarm takes on moving to `OK`.
    ok_actions: Vec<String>,
    /// The actions the alarm takes on moving to `INSUFFICIENT_DATA`.
    insufficient_data_actions: Vec<String>,
    /// How the metric is compared to the threshold.
    comparison_operator: Option<String>,
    /// How many of the evaluation periods must breach.
    datapoints_to_alarm: Option<i32>,
    /// The dimensions of the metric the alarm watches.
    dimensions: Vec<Dimension>,
    /// How a percentile alarm treats a period of low sample count.
    evaluate_low_sample_count_percentile: Option<String>,
    /// How many periods the alarm evaluates.
    evaluation_periods: Option<i32>,
    /// The percentile statistic the alarm folds its metric with.
    extended_statistic: Option<String>,
    /// The metric the alarm watches.
    metric_name: Option<String>,
    /// The namespace of that metric.
    namespace: Option<String>,
    /// The period, in seconds, the alarm evaluates the metric over.
    period: Option<i32>,
    /// The statistic the alarm folds its metric with.
    statistic: Option<String>,
    /// The value the metric is compared against.
    threshold: Option<f64>,
    /// The id of the anomaly-detection query that supplies the threshold.
    threshold_metric_id: Option<String>,
    /// How the alarm treats periods with no data.
    treat_missing_data: Option<String>,
    /// The unit of the metric the alarm watches.
    unit: Option<String>,
    /// The tag set the alarm is to carry.
    tags: Vec<Tag>,
}

impl DesiredModel {
    /// Read `properties` as the model of the alarm `identifier`, whose own ARN is `arn` — or
    /// state why the owning service would not take it.
    ///
    /// The identity and the read-only ARN may be CARRIED (a read reports them, so a model a
    /// client round-trips carries them) but only with the value the alarm already answers
    /// to; a property this path does not apply is refused, since no read reports one.
    fn read(identifier: &str, properties: &Value, arn: &str) -> Result<Self, NativeRefusal> {
        let Some(model) = properties.as_object() else {
            return Err(NativeRefusal::Invalid(format!(
                "{TYPE_NAME} requires a resource model of its properties"
            )));
        };
        let mut desired = Self {
            alarm_description: None,
            actions_enabled: None,
            alarm_actions: Vec::new(),
            ok_actions: Vec::new(),
            insufficient_data_actions: Vec::new(),
            comparison_operator: None,
            datapoints_to_alarm: None,
            dimensions: Vec::new(),
            evaluate_low_sample_count_percentile: None,
            evaluation_periods: None,
            extended_statistic: None,
            metric_name: None,
            namespace: None,
            period: None,
            statistic: None,
            threshold: None,
            threshold_metric_id: None,
            treat_missing_data: None,
            unit: None,
            tags: Vec::new(),
        };
        for (property, value) in model {
            match property.as_str() {
                IDENTIFIER_PROPERTY => require_value(property, value, identifier)?,
                ARN => require_value(property, value, arn)?,
                ALARM_DESCRIPTION => desired.alarm_description = Some(text(property, value)?),
                ACTIONS_ENABLED => {
                    desired.actions_enabled = Some(value.as_bool().ok_or_else(|| {
                        NativeRefusal::Invalid(format!("{ACTIONS_ENABLED} must be a boolean"))
                    })?);
                }
                ALARM_ACTIONS => desired.alarm_actions = actions(property, value)?,
                OK_ACTIONS => desired.ok_actions = actions(property, value)?,
                INSUFFICIENT_DATA_ACTIONS => {
                    desired.insufficient_data_actions = actions(property, value)?;
                }
                COMPARISON_OPERATOR => {
                    let operator = text(property, value)?;
                    if !COMPARISON_OPERATOR_VALUES.contains(&operator.as_str()) {
                        return Err(one_of(COMPARISON_OPERATOR, COMPARISON_OPERATOR_VALUES));
                    }
                    desired.comparison_operator = Some(operator);
                }
                DATAPOINTS_TO_ALARM => {
                    desired.datapoints_to_alarm = Some(count(property, value)?);
                }
                DIMENSIONS => desired.dimensions = dimensions(value)?,
                EVALUATE_LOW_SAMPLE_COUNT_PERCENTILE => {
                    desired.evaluate_low_sample_count_percentile = Some(text(property, value)?);
                }
                EVALUATION_PERIODS => desired.evaluation_periods = Some(count(property, value)?),
                EXTENDED_STATISTIC => desired.extended_statistic = Some(text(property, value)?),
                METRIC_NAME => desired.metric_name = Some(text(property, value)?),
                NAMESPACE => desired.namespace = Some(text(property, value)?),
                PERIOD => desired.period = Some(count(property, value)?),
                STATISTIC => {
                    let statistic = text(property, value)?;
                    if !STATISTIC_VALUES.contains(&statistic.as_str()) {
                        return Err(one_of(STATISTIC, STATISTIC_VALUES));
                    }
                    desired.statistic = Some(statistic);
                }
                THRESHOLD => {
                    desired.threshold = Some(value.as_f64().ok_or_else(|| {
                        NativeRefusal::Invalid(format!("{THRESHOLD} must be a number"))
                    })?);
                }
                THRESHOLD_METRIC_ID => {
                    desired.threshold_metric_id = Some(text(property, value)?);
                }
                TREAT_MISSING_DATA => {
                    let policy = text(property, value)?;
                    if !cw::TREAT_MISSING_DATA_VALUES.contains(&policy.as_str()) {
                        return Err(one_of(
                            TREAT_MISSING_DATA,
                            &cw::TREAT_MISSING_DATA_VALUES,
                        ));
                    }
                    desired.treat_missing_data = Some(policy);
                }
                UNIT => desired.unit = Some(text(property, value)?),
                TAGS => desired.tags = tag_set(value)?,
                // A property of the type the OWNING service resolves on its own write path,
                // which this one does not run; see this module's header. No read reports it,
                // so a model naming it asks for work nothing here performs.
                METRICS => return Err(super::unsupported(TYPE_NAME, METRICS)),
                other => {
                    return Err(NativeRefusal::Invalid(format!(
                        "{other} is not a property of {TYPE_NAME}"
                    )));
                }
            }
        }
        // "The arithmetic operation to use when comparing the specified statistic and
        // threshold" and "the number of periods over which data is compared to the specified
        // threshold" are both required of a metric alarm
        // (`PutMetricAlarmInput$ComparisonOperator`, `$EvaluationPeriods`), so a model naming
        // neither describes no alarm that service would store.
        for (property, present) in [
            (COMPARISON_OPERATOR, desired.comparison_operator.is_some()),
            (EVALUATION_PERIODS, desired.evaluation_periods.is_some()),
        ] {
            if !present {
                return Err(NativeRefusal::Invalid(format!(
                    "{property} is required of {TYPE_NAME}"
                )));
            }
        }
        Ok(desired)
    }
}

/// Write the alarm `identifier` into the owning store from `desired`, as of `now`.
///
/// `replacing` is whether an alarm of this name is already there: "when you update an
/// existing alarm, its state is left unchanged, but the update completely overwrites the
/// previous configuration of the alarm" (`PutMetricAlarm`), and the `CreationId` names the
/// write that CREATED the alarm, so both carry over.
fn write(
    store: &mut CloudWatchState,
    identifier: &str,
    region: &str,
    desired: DesiredModel,
    now: f64,
    replacing: bool,
) {
    let previous = store.alarms.get(identifier).map(|stored| stored.alarm.clone());
    // The WARM-UP window the alarm is standing in, which this write leaves exactly where it is:
    // the warm-up configuration itself is one this path carries over rather than applies, so the
    // window it opened is the same one — and an alarm still inside it goes on reporting that
    // (`cw::alarm_evaluation::warm_up`).
    let warm_up_ends_secs = store
        .alarms
        .get(identifier)
        .and_then(|stored| stored.warm_up_ends_secs);
    let state = previous.as_ref().map(|alarm| {
        (
            alarm.state_value.clone(),
            alarm.state_reason.clone(),
            alarm.state_reason_data.clone(),
            alarm.state_updated_timestamp,
            alarm.state_transitioned_timestamp,
        )
    });
    let (state_value, state_reason, state_reason_data, state_updated, state_transitioned) = state
        .unwrap_or_else(|| {
            // "When this operation creates an alarm, the alarm state is immediately set to
            // INSUFFICIENT_DATA. The alarm is then evaluated and its state is set
            // appropriately." — and the reason reports why: nothing has checked it yet.
            (
                Some(cw::ALARM_STATE_INSUFFICIENT_DATA.to_string()),
                Some(cw::INITIAL_STATE_REASON.to_string()),
                None,
                Some(cw::to_timestamp(now)),
                Some(cw::to_timestamp(now)),
            )
        });
    let alarm = MetricAlarm {
        alarm_name: Some(identifier.to_string()),
        alarm_arn: Some(cw::alarm_arn(region, identifier)),
        alarm_description: desired.alarm_description,
        alarm_configuration_updated_timestamp: Some(cw::to_timestamp(now)),
        actions_enabled: Some(desired.actions_enabled.unwrap_or(true)),
        ok_actions: cw::configured_actions(Some(&desired.ok_actions)),
        alarm_actions: cw::configured_actions(Some(&desired.alarm_actions)),
        insufficient_data_actions: cw::configured_actions(Some(
            &desired.insufficient_data_actions,
        )),
        state_value,
        state_reason,
        state_reason_data,
        state_updated_timestamp: state_updated,
        metric_name: desired.metric_name,
        namespace: desired.namespace,
        statistic: desired.statistic,
        extended_statistic: desired.extended_statistic,
        dimensions: cw::configured_list(Some(&desired.dimensions)),
        period: desired.period,
        unit: desired.unit,
        evaluation_periods: desired.evaluation_periods,
        datapoints_to_alarm: desired.datapoints_to_alarm,
        threshold: desired.threshold,
        comparison_operator: desired.comparison_operator,
        treat_missing_data: desired.treat_missing_data,
        evaluate_low_sample_count_percentile: desired.evaluate_low_sample_count_percentile,
        // The metric queries of an alarm this path does not apply stay exactly as the owning
        // service wrote them: a model naming them is refused rather than applied
        // ([`DesiredModel::read`]), so nothing here can replace or drop them.
        metrics: previous.as_ref().and_then(|alarm| alarm.metrics.clone()),
        threshold_metric_id: desired.threshold_metric_id,
        // No evaluation has run against this configuration yet: the alarm's state is the one
        // this write left it in until the first evaluation becomes due — and an alarm inside a
        // warm-up window says so, that evaluation being withheld until the window passes.
        evaluation_state: cw::alarm_evaluation::warm_up::evaluation_state(
            warm_up_ends_secs,
            now,
        ),
        state_transitioned_timestamp: state_transitioned,
        evaluation_window: previous
            .as_ref()
            .and_then(|alarm| alarm.evaluation_window.clone()),
        warm_up_configuration: previous
            .as_ref()
            .and_then(|alarm| alarm.warm_up_configuration.clone()),
        evaluation_criteria: previous
            .as_ref()
            .and_then(|alarm| alarm.evaluation_criteria.clone()),
        evaluation_interval: previous.as_ref().and_then(|alarm| alarm.evaluation_interval),
        action_configuration: previous
            .as_ref()
            .and_then(|alarm| alarm.action_configuration.clone()),
    };
    // "Data about the alarm, in JSON format." (`AlarmHistoryItem$HistoryData`) — the
    // configuration this write stored, and the one it replaced where it replaced an alarm,
    // so the owning service's own history read reports this write like any other.
    let history_data = cw::configuration_update_history_data(previous.as_ref(), &alarm);
    let creation_id = store
        .alarms
        .get(identifier)
        .map(|stored| stored.creation_id.clone())
        .unwrap_or_else(|| cw::alarm_creation_id(now));
    let tags = desired.tags;
    store.alarms.insert(
        identifier.to_string(),
        cw::StoredAlarm {
            alarm,
            // A Cloud Control update applies the WHOLE model, so the tag set the model
            // carries is the one the alarm is left with — which is what makes a tag a
            // desired state named observable through the owning service's own tag reads.
            tags,
            creation_id,
            // The write starts the span during which the alarm still reports the state it
            // left behind; its first evaluation against the stored metric data becomes due
            // at the end of it.
            evaluation_due_secs: now + cw::alarm_evaluation::EVALUATION_DELAY_SECS,
            // Nothing a caller FORCED is standing: a configuration write hands the alarm
            // back to its own evaluation.
            forced_state_until_secs: 0.0,
            warm_up_ends_secs,
            // Nothing is recorded per contributor by a write: the entries of a query that
            // returns a vector of series are what the alarm's own evaluation grades
            // (`cw::StoredAlarmContributor`).
            contributors: Vec::new(),
        },
    );
    store.record_history(
        identifier,
        now,
        AlarmHistoryItem {
            alarm_name: Some(identifier.to_string()),
            alarm_contributor_id: None,
            alarm_type: Some(cw::ALARM_TYPE_METRIC.to_string()),
            timestamp: Some(cw::to_timestamp(now)),
            history_item_type: Some(CONFIGURATION_UPDATE.to_string()),
            history_summary: Some(cw::configuration_update_history_summary(
                identifier, replacing,
            )),
            history_data: Some(history_data),
            alarm_contributor_attributes: None,
        },
    );
}

/// The `HistoryItemType` of a write that stored an alarm's configuration, which is what this
/// path performs.
const CONFIGURATION_UPDATE: &str = "ConfigurationUpdate";

/// The resource model one metric alarm renders as — the schema's properties the owning
/// record carries, and only those it HAS: an alarm under no description and no tags carries
/// neither property, the same way the owning service's own reader reports neither.
fn model(name: &str, stored: &cw::StoredAlarm) -> Value {
    let alarm = &stored.alarm;
    let mut model = Map::new();
    model.insert(IDENTIFIER_PROPERTY.to_string(), Value::from(name));
    if let Some(arn) = &alarm.alarm_arn {
        model.insert(ARN.to_string(), Value::from(arn.clone()));
    }
    for (property, value) in [
        (ALARM_DESCRIPTION, alarm.alarm_description.clone()),
        (COMPARISON_OPERATOR, alarm.comparison_operator.clone()),
        (
            EVALUATE_LOW_SAMPLE_COUNT_PERCENTILE,
            alarm.evaluate_low_sample_count_percentile.clone(),
        ),
        (EXTENDED_STATISTIC, alarm.extended_statistic.clone()),
        (METRIC_NAME, alarm.metric_name.clone()),
        (NAMESPACE, alarm.namespace.clone()),
        (STATISTIC, alarm.statistic.clone()),
        (THRESHOLD_METRIC_ID, alarm.threshold_metric_id.clone()),
        (TREAT_MISSING_DATA, alarm.treat_missing_data.clone()),
        (UNIT, alarm.unit.clone()),
    ] {
        if let Some(value) = value {
            model.insert(property.to_string(), Value::from(value));
        }
    }
    if let Some(enabled) = alarm.actions_enabled {
        model.insert(ACTIONS_ENABLED.to_string(), Value::from(enabled));
    }
    for (property, actions) in [
        (ALARM_ACTIONS, alarm.alarm_actions.as_ref()),
        (OK_ACTIONS, alarm.ok_actions.as_ref()),
        (
            INSUFFICIENT_DATA_ACTIONS,
            alarm.insufficient_data_actions.as_ref(),
        ),
    ] {
        if let Some(actions) = actions.filter(|actions| !actions.is_empty()) {
            model.insert(
                property.to_string(),
                Value::Array(actions.iter().map(|action| Value::from(action.clone())).collect()),
            );
        }
    }
    for (property, count) in [
        (DATAPOINTS_TO_ALARM, alarm.datapoints_to_alarm),
        (EVALUATION_PERIODS, alarm.evaluation_periods),
        (PERIOD, alarm.period),
    ] {
        if let Some(count) = count {
            model.insert(property.to_string(), Value::from(count));
        }
    }
    if let Some(threshold) = alarm.threshold {
        model.insert(THRESHOLD.to_string(), Value::from(threshold));
    }
    if let Some(dimensions) = alarm.dimensions.as_ref().filter(|held| !held.is_empty()) {
        model.insert(
            DIMENSIONS.to_string(),
            Value::Array(
                dimensions
                    .iter()
                    .map(|dimension| {
                        Value::Object(Map::from_iter([
                            (
                                DIMENSION_NAME.to_string(),
                                Value::from(dimension.name.clone()),
                            ),
                            (
                                DIMENSION_VALUE.to_string(),
                                Value::from(dimension.value.clone()),
                            ),
                        ]))
                    })
                    .collect(),
            ),
        );
    }
    if !stored.tags.is_empty() {
        model.insert(
            TAGS.to_string(),
            super::tag_list(stored.tags.iter().map(|tag| (&tag.key, &tag.value))),
        );
    }
    Value::Object(model)
}

/// The refusal for a property whose value is outside the domain the owning service's model
/// gives it, naming that whole domain.
///
/// This type's registry schema declares each of those properties as plain text and publishes
/// no set for it (`crate::state::registry_schema`), so the set is the OWNING service's own
/// rule about the value and its break is that service's error rather than a fault in the
/// model against the schema.
fn one_of(property: &str, values: &[&str]) -> NativeRefusal {
    NativeRefusal::Rejected(format!("{property} must be one of {values:?}"))
}

/// The action ARNs `value` carries, or why the property is not a list of them.
fn actions(property: &str, value: &Value) -> Result<Vec<String>, NativeRefusal> {
    let Some(entries) = value.as_array() else {
        return Err(NativeRefusal::Invalid(format!(
            "{property} must be a list of action ARNs"
        )));
    };
    entries
        .iter()
        .map(|entry| text(property, entry))
        .collect::<Result<Vec<String>, NativeRefusal>>()
}

/// The non-negative whole number `value` carries, or why the property is not one.
///
/// Every count a metric alarm carries is an `Integer` in the owning service's model, so a
/// value outside that type — or a fractional one — describes no alarm it would store.
fn count(property: &str, value: &Value) -> Result<i32, NativeRefusal> {
    value
        .as_i64()
        .and_then(|count| i32::try_from(count).ok())
        .filter(|count| *count > 0)
        .ok_or_else(|| NativeRefusal::Invalid(format!("{property} must be a positive integer")))
}

/// The dimensions `value` carries: a list of `{ Name, Value }` pairs.
fn dimensions(value: &Value) -> Result<Vec<Dimension>, NativeRefusal> {
    let Some(entries) = value.as_array() else {
        return Err(NativeRefusal::Invalid(format!(
            "{DIMENSIONS} must be a list of {{ {DIMENSION_NAME}, {DIMENSION_VALUE} }} pairs"
        )));
    };
    let mut dimensions = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry.get(DIMENSION_NAME).and_then(Value::as_str);
        let dimension_value = entry.get(DIMENSION_VALUE).and_then(Value::as_str);
        let (Some(name), Some(dimension_value)) = (name, dimension_value) else {
            return Err(NativeRefusal::Invalid(format!(
                "every {DIMENSIONS} entry must carry {DIMENSION_NAME} and {DIMENSION_VALUE}"
            )));
        };
        dimensions.push(Dimension {
            name: name.to_string(),
            value: dimension_value.to_string(),
        });
    }
    Ok(dimensions)
}

/// The tag set `value` carries, in the owning service's own tag shape: the shared reading of
/// a tag set, so an alarm's tags are judged the way every other type's are.
fn tag_set(value: &Value) -> Result<Vec<Tag>, NativeRefusal> {
    Ok(super::tag_entries(value)?
        .into_iter()
        .map(|(key, value)| Tag { key, value })
        .collect())
}
