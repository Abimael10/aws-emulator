// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch monitors an instance refresh is started under.
//!
//! "The names of one or more CloudWatch alarms to monitor for the instance refresh"
//! (`smithy_query(query="doc",shape="AlarmSpecification",member="Alarms")`): the alarms a
//! refresh's `Preferences.AlarmSpecification` names are the monitors Amazon EC2 Auto
//! Scaling watches that refresh against, so a refresh is not allowed to BEGIN while one of
//! them is already sounding — the replacement it would roll out is the very thing the
//! monitor is objecting to. Amazon EC2 Auto Scaling refuses such a request with
//! `ValidationError` and the sentence [`MONITOR_IN_ALARM_MESSAGE`], and creates nothing.
//!
//! The alarms belong to CloudWatch, so their state is read out of the account's OWN
//! CloudWatch store through the shared engine — the same store `PutMetricAlarm` writes,
//! `SetAlarmState` forces and `DescribeAlarms` reports. Nothing about an alarm is copied
//! into this service's store, so a refresh is never gated on a state the alarm no longer
//! holds. A name that store resolves to no alarm at all reads as no monitor, so it gates
//! nothing: only an alarm that IS sounding stops a refresh.
//!
//! LOCKING. CloudWatch's store is taken here for the one read and released when it ends,
//! and this module is called with this service's own store UNLOCKED, so no request holds
//! two stores at once ([`crate::state::scaling_policies`] takes the same discipline).

use std::collections::BTreeSet;

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use crate::ops::types::AlarmSpecification;

/// What Amazon EC2 Auto Scaling answers a request that asks to start a refresh while one of
/// the monitors it names is sounding.
///
/// Fixed text: it names neither the request's alarms nor its group, so the same sentence is
/// answered whichever monitor is in alarm.
pub const MONITOR_IN_ALARM_MESSAGE: &str =
    "A CloudWatch monitor is in alarm. The instance refresh cannot start.";

/// The alarms a specification MONITORS, in the order it names them, with blank names and
/// repeats dropped.
///
/// The one reading of that member, so the gate below and anything else that asks which
/// monitors a refresh runs under are about the same set.
#[must_use]
pub fn monitored_alarms(specification: Option<&AlarmSpecification>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    specification
        .and_then(|specification| specification.alarms.as_ref())
        .into_iter()
        .flatten()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .filter(|name| seen.insert((*name).to_string()))
        .map(str::to_string)
        .collect()
}

/// The monitors of `specification` that are in the `ALARM` state right now, in the order
/// the specification names them.
///
/// Call it with this service's own store guard RELEASED. A specification that names no
/// monitor, and an engine carrying no CloudWatch store, read as nothing sounding: there is
/// no monitor for either to have gated on.
#[must_use]
pub fn sounding_monitors<Eng: Send + Sync + 'static>(
    engine: &Eng,
    specification: Option<&AlarmSpecification>,
) -> Vec<String> {
    let monitored = monitored_alarms(specification);
    if monitored.is_empty() {
        return Vec::new();
    }
    let Some(store) = cloudwatch::state::service_state(engine) else {
        return Vec::new();
    };
    let store = store.lock();
    monitored
        .into_iter()
        .filter(|name| {
            store.alarm_state_value(name).as_deref()
                == Some(cloudwatch::state::ALARM_STATE_ALARM)
        })
        .collect()
}

/// Whether any monitor `specification` names is sounding, which is what refuses the start of
/// a refresh.
#[must_use]
pub fn a_monitor_is_in_alarm<Eng: Send + Sync + 'static>(
    engine: &Eng,
    specification: Option<&AlarmSpecification>,
) -> bool {
    !sounding_monitors(engine, specification).is_empty()
}
