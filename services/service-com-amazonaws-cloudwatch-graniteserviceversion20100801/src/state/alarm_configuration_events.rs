// SPDX-License-Identifier: CC0-1.0

//! The EVENT a change to an alarm's CONFIGURATION publishes, and the writes that publish none.
//!
//! # The rule this module implements
//!
//! CloudWatch is an event SOURCE for what a client does to an alarm as well as for what the
//! alarm does: a write that CREATES an alarm, one that REPLACES the configuration of an alarm
//! that already exists, and a call that DELETES one each publish an event onto the account's
//! default event bus under this service's `source` and the `detail-type` of an alarm
//! configuration change ([`CONFIGURATION_CHANGE_DETAIL_TYPE`]), with the alarm's own ARN in the
//! event's `resources` and a `detail` naming the alarm, the `operation` the write performed
//! ([`OPERATION_CREATE`], [`OPERATION_UPDATE`], [`OPERATION_DELETE`]), the state the alarm is in
//! and the configuration that write left — beside the configuration it REPLACED, for an update.
//! A rule on that bus whose `EventPattern` matches then delivers it to that rule's targets,
//! which is how alarm governance (an alarm being weakened, retargeted or removed) is noticed
//! without the client polling for it.
//!
//! What publishes nothing is a call that changed no configuration: a write that re-states the
//! configuration the alarm already carries replaces it with itself — "the update completely
//! overwrites the previous configuration of the alarm" (`PutMetricAlarm`), and overwriting a
//! configuration with an identical one leaves the alarm as it was — and a call that touches
//! something that is not the alarm's configuration at all (its tags, which
//! `PutMetricAlarmInput$Tags` keeps out of an update, or a state a caller forces, which is the
//! other event entirely) changes no configuration either. A write this service REFUSES stores
//! nothing, so it publishes nothing for the same reason.
//!
//! # How a write becomes an event
//!
//! Nothing here decides WHAT happened: the handler that stored or removed the alarm does, and
//! the record it queued is what the event reports. The queue is the store's
//! (`pending_configuration_changes`), drained with no lock held by the same settle that
//! publishes the state changes ([`super::alarm_actions::settle_alarms`]), through the ONE
//! publisher this service's alarm events take to the bus
//! ([`super::alarm_actions::state_change_events::publish_alarm_events`]) — so the envelope of a
//! configuration change and of a state change cannot drift apart, and this module names no
//! other service's crate.

use ::serde_json::{Map, Value};

use super::alarm_actions::state_change_events::{metric_queries, state_document, AlarmEvent};
use crate::ops::types::{CompositeAlarm, LogAlarm, MetricAlarm};

/// The `detail-type` of the event a change to an alarm's configuration publishes.
pub const CONFIGURATION_CHANGE_DETAIL_TYPE: &str = "CloudWatch Alarm Configuration Change";

/// `detail.operation` of a write that CREATED the alarm: the store held no alarm of that name.
pub const OPERATION_CREATE: &str = "create";

/// `detail.operation` of a write that REPLACED the configuration of an alarm that already
/// existed with a different one.
pub const OPERATION_UPDATE: &str = "update";

/// `detail.operation` of a call that DELETED the alarm.
pub const OPERATION_DELETE: &str = "delete";

/// How many undelivered configuration changes this store holds before the oldest is dropped.
///
/// The store's OWN bound, not a limit AWS publishes, and the same shape as the one on the
/// pending state-change notifications ([`super::alarm_evaluation::MAX_PENDING_NOTIFICATIONS`]):
/// every write queues at most one record, each carrying two bounded configuration documents, so
/// what bounds the memory one client can make this store hold is how many of them may wait for
/// a publication at once.
pub const MAX_PENDING_CONFIGURATION_CHANGES: usize = 256;

/// How many queued configuration changes ONE request publishes, leaving the rest for the next
/// call that settles — which is how the asynchronous publication of AWS looks to a caller.
pub const MAX_CONFIGURATION_CHANGES_PER_REQUEST: usize = 32;

/// One change a write made to one alarm's configuration, as the event it publishes reports it.
///
/// Built where the write was made, from the alarm the store holds after it (and, for an update,
/// the alarm it held before), so the event reports the configuration a read of the same instant
/// reports and never a second, invented copy of it.
#[derive(Debug, Clone)]
pub struct AlarmConfigurationChange {
    /// The alarm the write changed, by name.
    pub alarm_name: String,
    /// That alarm's own ARN: the resource the event names in its `resources`, and the Region the
    /// publication is addressed in.
    pub alarm_arn: Option<String>,
    /// WHEN the write was made, which is the event's own `time`.
    pub at_secs: f64,
    /// `detail.operation`: what the write did to the alarm.
    operation: &'static str,
    /// `detail.state`: the state the alarm is in — a configuration write leaves it alone ("its
    /// state is left unchanged"), so this is the state the alarm already reported.
    state: Value,
    /// `detail.configuration`: the configuration the write left on the alarm, or — for a
    /// deletion — the one it removed, as the store's own record of an alarm's configuration
    /// states it (every configuration member the alarm carries and no state member, which is
    /// the same record the alarm's configuration-update history item carries).
    configuration: Value,
    /// `detail.previousConfiguration`: the configuration an UPDATE replaced, and `None` for a
    /// write that replaced none.
    previous_configuration: Option<Value>,
}

impl AlarmConfigurationChange {
    /// The change the write that stored the METRIC alarm `stored` publishes, where `previous` is
    /// the alarm that write replaced — and `None` for a write that changed no configuration.
    #[must_use]
    pub fn of_metric_write(
        previous: Option<&MetricAlarm>,
        stored: &MetricAlarm,
        at_secs: f64,
    ) -> Option<Self> {
        // The create/update/no-op decision is made on the store's OWN configuration record,
        // the one the history item of the same write records, so a write the history reports as
        // an update cannot be one this path reports as a no-op. The metric selection is
        // normalised on the documents the event RENDERS, afterwards.
        let before = previous.map(super::alarm_configuration_document);
        let after = super::alarm_configuration_document(stored);
        let operation = write_operation(before.as_ref(), &after)?;
        Some(Self {
            alarm_name: stored.alarm_name.clone()?,
            alarm_arn: stored.alarm_arn.clone(),
            at_secs,
            operation,
            state: reported_state(&super::AlarmStateSnapshot::of_metric_alarm(stored))?,
            configuration: with_metric_selection(after, stored),
            previous_configuration: before
                .zip(previous)
                .map(|(document, alarm)| with_metric_selection(document, alarm)),
        })
    }

    /// The change the deletion of the METRIC alarm `deleted` publishes.
    #[must_use]
    pub fn of_metric_deletion(deleted: &MetricAlarm, at_secs: f64) -> Option<Self> {
        Some(Self {
            alarm_name: deleted.alarm_name.clone()?,
            alarm_arn: deleted.alarm_arn.clone(),
            at_secs,
            operation: OPERATION_DELETE,
            state: reported_state(&super::AlarmStateSnapshot::of_metric_alarm(deleted))?,
            configuration: with_metric_selection(
                super::alarm_configuration_document(deleted),
                deleted,
            ),
            previous_configuration: None,
        })
    }

    /// The change the write that stored the COMPOSITE alarm `stored` publishes. Same rule; what
    /// differs is the configuration document, which reports the rule the alarm applies rather
    /// than a metric.
    #[must_use]
    pub fn of_composite_write(
        previous: Option<&CompositeAlarm>,
        stored: &CompositeAlarm,
        at_secs: f64,
    ) -> Option<Self> {
        let before = previous.map(super::composite_alarm_configuration_document);
        let after = super::composite_alarm_configuration_document(stored);
        let operation = write_operation(before.as_ref(), &after)?;
        Some(Self {
            alarm_name: stored.alarm_name.clone()?,
            alarm_arn: stored.alarm_arn.clone(),
            at_secs,
            operation,
            state: reported_state(&super::AlarmStateSnapshot::of_composite_alarm(stored))?,
            configuration: after,
            previous_configuration: before,
        })
    }

    /// The change the deletion of the COMPOSITE alarm `deleted` publishes.
    #[must_use]
    pub fn of_composite_deletion(deleted: &CompositeAlarm, at_secs: f64) -> Option<Self> {
        Some(Self {
            alarm_name: deleted.alarm_name.clone()?,
            alarm_arn: deleted.alarm_arn.clone(),
            at_secs,
            operation: OPERATION_DELETE,
            state: reported_state(&super::AlarmStateSnapshot::of_composite_alarm(deleted))?,
            configuration: super::composite_alarm_configuration_document(deleted),
            previous_configuration: None,
        })
    }

    /// The change the write that stored the LOG alarm `stored` publishes. Same rule; the
    /// configuration document reports the scheduled query the alarm evaluates.
    #[must_use]
    pub fn of_log_write(
        previous: Option<&LogAlarm>,
        stored: &LogAlarm,
        at_secs: f64,
    ) -> Option<Self> {
        let before = previous.map(super::log_alarm_configuration_document);
        let after = super::log_alarm_configuration_document(stored);
        let operation = write_operation(before.as_ref(), &after)?;
        Some(Self {
            alarm_name: stored.alarm_name.clone()?,
            alarm_arn: stored.alarm_arn.clone(),
            at_secs,
            operation,
            state: reported_state(&super::AlarmStateSnapshot::of_log_alarm(stored))?,
            configuration: after,
            previous_configuration: before,
        })
    }

    /// The change the deletion of the LOG alarm `deleted` publishes.
    #[must_use]
    pub fn of_log_deletion(deleted: &LogAlarm, at_secs: f64) -> Option<Self> {
        Some(Self {
            alarm_name: deleted.alarm_name.clone()?,
            alarm_arn: deleted.alarm_arn.clone(),
            at_secs,
            operation: OPERATION_DELETE,
            state: reported_state(&super::AlarmStateSnapshot::of_log_alarm(deleted))?,
            configuration: super::log_alarm_configuration_document(deleted),
            previous_configuration: None,
        })
    }

    /// The event's `detail`: the alarm's name, what the write did to it, the state it is in and
    /// the configuration documents the change is made of.
    ///
    /// Public because it IS the contract — these are the members a rule's `EventPattern` filters
    /// a configuration change on.
    #[must_use]
    pub fn detail(&self) -> Value {
        let mut detail = Map::new();
        detail.insert(
            "alarmName".to_string(),
            Value::from(self.alarm_name.as_str()),
        );
        detail.insert("operation".to_string(), Value::from(self.operation));
        detail.insert("state".to_string(), self.state.clone());
        detail.insert("configuration".to_string(), self.configuration.clone());
        // A write that replaced nothing reports no previous configuration rather than a null
        // one: there was no configuration before it.
        if let Some(previous) = self.previous_configuration.as_ref() {
            detail.insert("previousConfiguration".to_string(), previous.clone());
        }
        Value::Object(detail)
    }
}

/// `document` — the store's record of the METRIC alarm `alarm`'s configuration — with the
/// METRIC SELECTION it reports stated as the list of queries that alarm evaluates, which is how
/// the event a change to it publishes reports the selection.
///
/// The selection is reported as `metrics` whichever way the alarm was configured — a query-array
/// alarm reports its own queries and an alarm configured with a single metric reports the one
/// query that metric IS — through the ONE builder of that list ([`metric_queries`]), the same one
/// the state-change event's configuration takes, so a consumer reads one shape of metric
/// selection from either event and from either kind of alarm. An alarm that evaluates no metric
/// at all (one configured with a query language) reports no `metrics` rather than an empty list,
/// exactly as it names no metric.
///
/// Applied to the document the event RENDERS and never to the one the no-op decision is made on:
/// that decision belongs to the store's own configuration record, which reports every member a
/// client wrote as it wrote it, so a write that changed only a member this normalisation would
/// default is still the update it is.
fn with_metric_selection(mut document: Value, alarm: &MetricAlarm) -> Value {
    let queries = metric_queries(alarm);
    if let Some(members) = document.as_object_mut() {
        if queries.is_empty() {
            members.remove("metrics");
        } else {
            members.insert("metrics".to_string(), Value::Array(queries));
        }
    }
    document
}

/// Which operation a write PERFORMED on an alarm's configuration, and `None` when it performed
/// none.
///
/// `before` is the whole configuration the store held for that alarm and `stored` the one it
/// holds after the write, each as the store's own configuration record states it — every
/// configuration member of the alarm and no state member — so two configurations are the same
/// configuration exactly when every member a client can write is the same.
fn write_operation(before: Option<&Value>, stored: &Value) -> Option<&'static str> {
    match before {
        // The store held no alarm of this name, so the write created one.
        None => Some(OPERATION_CREATE),
        // The write stored the configuration the alarm already had: it overwrote the
        // configuration with itself, which changes nothing and announces nothing.
        Some(before) if before == stored => None,
        Some(_) => Some(OPERATION_UPDATE),
    }
}

/// The `detail.state` a configuration change reports: the state the alarm is in and the instant
/// it reached it, and `None` for an alarm carrying no state at all.
///
/// A configuration write leaves the alarm's state alone, so the state is reported as the alarm
/// carries it — including the instant it was last updated at, which is NOT this write's instant.
/// The explanation of that state is not part of it: what this event is about is the
/// configuration, and the state change that produced the explanation has its own event
/// ([`super::alarm_actions::state_change_events`]).
fn reported_state(state: &super::AlarmStateSnapshot) -> Option<Value> {
    Some(state_document(
        state.state_value.as_deref()?,
        None,
        None,
        state.state_updated_secs,
    ))
}

impl super::ComAmazonawsCloudwatchGraniteserviceversion20100801State {
    /// Queue the configuration change `change`, for the next settle to publish.
    ///
    /// Bounded by [`MAX_PENDING_CONFIGURATION_CHANGES`]: a store already holding that many
    /// undelivered changes drops the OLDEST, because the newest is the one that reports the
    /// configuration the alarm now has.
    pub fn enqueue_configuration_change(&mut self, change: AlarmConfigurationChange) {
        while self.pending_configuration_changes.len() >= MAX_PENDING_CONFIGURATION_CHANGES {
            let Some(dropped) = self.pending_configuration_changes.pop_front() else {
                break;
            };
            ::tracing::warn!(
                alarm = %dropped.alarm_name,
                pending = MAX_PENDING_CONFIGURATION_CHANGES,
                "this store already holds the most unpublished alarm configuration changes it \
                 can; the oldest is dropped unpublished"
            );
        }
        self.pending_configuration_changes.push_back(change);
    }

    /// Take at most `limit` of the queued configuration changes, oldest first, leaving the rest
    /// for the next caller: the caller PUBLISHES what it takes, so a taken change must not be
    /// left for another caller to publish twice.
    pub fn take_pending_configuration_changes(
        &mut self,
        limit: usize,
    ) -> Vec<AlarmConfigurationChange> {
        let taken = limit.min(self.pending_configuration_changes.len());
        self.pending_configuration_changes.drain(..taken).collect()
    }
}

/// Publish the configuration changes `changes` reports onto the account's default EventBridge
/// bus.
///
/// Called with NO store lock held, after the writes have been made and queued: a publication
/// reaches EventBridge's own handler, which matches the event against the account's rules and
/// delivers it to their targets — and a target may call back into this service, so holding the
/// alarm store's lock across it would deadlock the very call the event set off.
pub async fn publish(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    changes: &[AlarmConfigurationChange],
) {
    let events: Vec<AlarmEvent> = changes
        .iter()
        .map(|change| AlarmEvent {
            alarm_name: change.alarm_name.clone(),
            alarm_arn: change.alarm_arn.clone(),
            at_secs: change.at_secs,
            detail_type: CONFIGURATION_CHANGE_DETAIL_TYPE,
            detail: change.detail(),
        })
        .collect();
    super::alarm_actions::state_change_events::publish_alarm_events(callback, &events).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The alarm the rows below are built from: one metric, one dimension, in its created state.
    fn alarm() -> MetricAlarm {
        MetricAlarm {
            alarm_name: Some("backlog-too-long".to_string()),
            alarm_arn: Some(
                "arn:aws:cloudwatch:us-west-2:000000000000:alarm:backlog-too-long".to_string(),
            ),
            alarm_description: Some("the backlog is too long".to_string()),
            namespace: Some("Custom/Queue".to_string()),
            metric_name: Some("Backlog".to_string()),
            period: Some(60),
            statistic: Some("Average".to_string()),
            threshold: Some(1.0),
            comparison_operator: Some("GreaterThanThreshold".to_string()),
            state_value: Some(crate::state::ALARM_STATE_INSUFFICIENT_DATA.to_string()),
            state_updated_timestamp: Some(crate::state::to_timestamp(1_800_000_000.0)),
            ..MetricAlarm::default()
        }
    }

    /// WHICH operation each shape of write reports, and that a write changing nothing reports
    /// none: the rows differ along what the write did to the stored configuration.
    #[test]
    fn a_write_reports_the_operation_it_performed() {
        let created = alarm();
        let mut weakened = alarm();
        weakened.threshold = Some(1_000.0);
        let mut retagged = alarm();
        // Neither the tags nor the state is part of a configuration, so a write that changes
        // only those changed no configuration.
        retagged.state_value = Some(crate::state::ALARM_STATE_ALARM.to_string());

        for (case, previous, stored, expected) in [
            ("a write of an alarm the store held none of", None, &created, Some(OPERATION_CREATE)),
            (
                "a write that weakened the threshold",
                Some(&created),
                &weakened,
                Some(OPERATION_UPDATE),
            ),
            (
                "a write that re-stated the same configuration",
                Some(&created),
                &created,
                None,
            ),
            (
                "a write that changed only the alarm's state",
                Some(&created),
                &retagged,
                None,
            ),
        ] {
            let change = AlarmConfigurationChange::of_metric_write(previous, stored, 1_800_000_060.0);
            let operation = change
                .as_ref()
                .map(|change| change.detail()["operation"].clone());
            let expected = expected.map(Value::from);
            assert_eq!(
                operation, expected,
                "case {case:?}: expected the operation {expected:?}, got {operation:?}"
            );
        }
    }

    /// An UPDATE reports both configurations, a CREATE only the one it stored, and a DELETE the
    /// configuration it removed.
    #[test]
    fn the_detail_reports_the_configurations_of_the_change() {
        let created = alarm();
        let mut weakened = alarm();
        weakened.threshold = Some(1_000.0);

        let create = AlarmConfigurationChange::of_metric_write(None, &created, 1.0)
            .expect("a write of a new alarm owes a configuration change")
            .detail();
        assert_eq!(create["operation"], Value::from(OPERATION_CREATE));
        assert_eq!(
            create.get("previousConfiguration"),
            None,
            "a create replaced no configuration, so it reports none: {create}"
        );

        let update = AlarmConfigurationChange::of_metric_write(Some(&created), &weakened, 2.0)
            .expect("a write that changed the threshold owes a configuration change")
            .detail();
        assert_eq!(update["operation"], Value::from(OPERATION_UPDATE));
        assert_eq!(
            update.pointer("/previousConfiguration/threshold"),
            Some(&Value::from(1.0)),
            "an update reports the configuration it replaced: {update}"
        );
        assert_eq!(
            update.pointer("/configuration/threshold"),
            Some(&Value::from(1_000.0)),
            "an update reports the configuration it stored: {update}"
        );

        let delete = AlarmConfigurationChange::of_metric_deletion(&weakened, 3.0)
            .expect("the deletion of an alarm owes a configuration change")
            .detail();
        assert_eq!(delete["operation"], Value::from(OPERATION_DELETE));
        assert_eq!(
            delete["alarmName"],
            Value::from("backlog-too-long"),
            "a deletion names the alarm it removed: {delete}"
        );
    }
}
