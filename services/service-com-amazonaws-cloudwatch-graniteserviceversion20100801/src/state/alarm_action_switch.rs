// SPDX-License-Identifier: CC0-1.0

//! The one write that turns an alarm's ACTIONS on or off, and the configuration change it is.
//!
//! `ActionsEnabled` is a member of an alarm's configuration like any other, so a call that
//! changes it is a configuration update: it stamps the alarm's configuration instant, records
//! the same `ConfigurationUpdate` history item a `Put*Alarm` write records, hands the alarm's
//! state back to its own evaluation, and publishes the one configuration-change event every
//! write that leaves an alarm's configuration different publishes
//! ([`super::alarm_configuration_events`]) — whichever operation made the write. A call that
//! asks for the setting the alarm already has changes no configuration, so it records nothing
//! and publishes nothing.

use super::alarm_configuration_events::AlarmConfigurationChange;
use super::*;

impl ComAmazonawsCloudwatchGraniteserviceversion20100801State {
    /// Turn the ACTIONS of every alarm `names` names on or off, whatever kind of alarm each
    /// one is, reporting back the names this store holds no alarm for.
    ///
    /// "Indicates whether actions should be executed during any changes to the alarm state"
    /// (`MetricAlarm$ActionsEnabled`) — the one member `EnableAlarmActions` ("Enables the
    /// actions for the specified alarms.") and `DisableAlarmActions` ("Disables the actions
    /// for the specified alarms. When an alarm's actions are disabled, the alarm actions do
    /// not execute when the alarm state changes.") write, so both operations share this one
    /// writer and neither can drift from the other. Every kind of alarm carries the member,
    /// so a name of any kind is written.
    ///
    /// A write that CHANGES the member is a configuration update: it stamps
    /// `AlarmConfigurationUpdatedTimestamp`, records the `ConfigurationUpdate` history item
    /// every other configuration write records, queues the configuration-change event every
    /// other configuration write queues ([`super::alarm_configuration_events`]), and — like
    /// every other configuration write — hands the alarm's state back to its own evaluation,
    /// which becomes due a span later ("the alarm is then evaluated and its state is set
    /// appropriately", `PutMetricAlarm`; "It is also reevaluated if you update its
    /// configuration", `SetAlarmState` on a composite alarm). So a state a caller forced does
    /// not outlive a reconfiguration of the alarm it was forced on. Asking for the setting an
    /// alarm already has changes nothing, so it records nothing, publishes nothing and
    /// re-evaluates nothing either.
    pub fn set_actions_enabled(
        &mut self,
        names: &[String],
        enabled: bool,
        now_secs: f64,
    ) -> Vec<String> {
        // A name the request repeats names ONE alarm, so the deduplicated list is walked: the
        // second mention would otherwise find the member already written and report the alarm
        // as unchanged twice.
        let mut named: Vec<&str> = names.iter().map(String::as_str).collect();
        named.sort_unstable();
        named.dedup();

        let mut unknown: Vec<String> = Vec::new();
        // When a configuration write re-evaluates the alarm it touched: the same span after the
        // write as every other transition this store's clock owes, so a caller reading straight
        // back still sees the state the write found.
        let due = now_secs + alarm_evaluation::EVALUATION_DELAY_SECS;
        // The history items the writes owe, recorded once the store is no longer borrowed by
        // the alarm each one belongs to.
        let mut records: Vec<(String, &'static str, String)> = Vec::new();
        // The configuration changes the writes owe the account's event bus, built from the
        // alarm this write left and the one it replaced — beside the history items, out of the
        // same two readings of the alarm, so an event cannot report a change the history does
        // not and a write that changed nothing owes neither.
        let mut changes: Vec<AlarmConfigurationChange> = Vec::new();
        for name in named {
            if let Some(stored) = self.alarms.get_mut(name) {
                if stored.alarm.actions_enabled == Some(enabled) {
                    continue;
                }
                let previous = stored.alarm.clone();
                stored.alarm.actions_enabled = Some(enabled);
                stored.alarm.alarm_configuration_updated_timestamp = Some(to_timestamp(now_secs));
                stored.evaluation_due_secs = stored.evaluation_due_secs.min(due);
                let data = configuration_update_history_data(Some(&previous), &stored.alarm);
                changes.extend(AlarmConfigurationChange::of_metric_write(
                    Some(&previous),
                    &stored.alarm,
                    now_secs,
                ));
                records.push((name.to_string(), ALARM_TYPE_METRIC, data));
            } else if let Some(stored) = self.composite_alarms.get_mut(name) {
                if stored.alarm.actions_enabled == Some(enabled) {
                    continue;
                }
                let previous = stored.alarm.clone();
                stored.alarm.actions_enabled = Some(enabled);
                stored.alarm.alarm_configuration_updated_timestamp = Some(to_timestamp(now_secs));
                stored.evaluation_due_secs = stored.evaluation_due_secs.min(due);
                // "It is also reevaluated if you update its configuration" (`SetAlarmState`) —
                // so the rule, not the force, owns this alarm's state from that evaluation on.
                stored.forced_child_states = None;
                let data =
                    composite_configuration_update_history_data(Some(&previous), &stored.alarm);
                changes.extend(AlarmConfigurationChange::of_composite_write(
                    Some(&previous),
                    &stored.alarm,
                    now_secs,
                ));
                records.push((name.to_string(), ALARM_TYPE_COMPOSITE, data));
            } else if let Some(stored) = self.log_alarms.get_mut(name) {
                if stored.alarm.actions_enabled == Some(enabled) {
                    continue;
                }
                let previous = stored.alarm.clone();
                stored.alarm.actions_enabled = Some(enabled);
                stored.alarm.alarm_configuration_updated_timestamp = Some(to_timestamp(now_secs));
                let data = log_configuration_update_history_data(Some(&previous), &stored.alarm);
                changes.extend(AlarmConfigurationChange::of_log_write(
                    Some(&previous),
                    &stored.alarm,
                    now_secs,
                ));
                records.push((name.to_string(), ALARM_TYPE_LOG, data));
            } else {
                unknown.push(name.to_string());
            }
        }
        for (name, kind, data) in records {
            self.record_history(
                &name,
                now_secs,
                AlarmHistoryItem {
                    alarm_name: Some(name.clone()),
                    alarm_contributor_id: None,
                    alarm_type: Some(kind.to_string()),
                    timestamp: Some(to_timestamp(now_secs)),
                    history_item_type: Some("ConfigurationUpdate".to_string()),
                    history_summary: Some(configuration_update_history_summary(&name, true)),
                    history_data: Some(data),
                    alarm_contributor_attributes: None,
                },
            );
        }
        for change in changes {
            self.enqueue_configuration_change(change);
        }
        unknown
    }
}
