// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch alarms an environment's `Monitors` name, and what they have already made
//! due for a deployment in flight into that environment.
//!
//! `Monitors` is "Amazon CloudWatch alarms to monitor during the deployment process"
//! (`smithy_query(query="shape",shape="Monitor")`), and `FinalBakeTimeInMinutes` is "the
//! amount of time that AppConfig monitors for Amazon CloudWatch alarms before considering
//! the deployment to be complete and no longer eligible for automatic rollback" — so while
//! a deployment is in flight the SERVICE watches the alarms its environment names and rolls
//! the deployment back itself when one of them is alarming, with no `StopDeployment` from
//! the caller. The window ends when the deployment completes: a completed deployment is "no
//! longer eligible for automatic rollback".
//!
//! Two things can end a monitored deployment that way, and both are read rather than
//! guessed at:
//!
//! * the alarm is in its alarming state — the state belongs to the account's CloudWatch
//!   store in the SAME shared engine, the records `PutMetricAlarm`, `PutCompositeAlarm`,
//!   `PutLogAlarm` and `SetAlarmState` wrote, so it is read from there
//!   ([`MonitorHealth::read`]);
//! * the alarm cannot be read at all, because `AlarmRoleArn` — "ARN of an Identity and
//!   Access Management (IAM) role for AppConfig to monitor `AlarmArn`" — names a role of
//!   this account whose trust policy does not let [`SERVICE_PRINCIPAL`] assume it, so the
//!   monitoring the deployment was started under cannot happen and the deployment is not
//!   left running unmonitored.
//!
//! Either way the rollback is a transition the CLOCK already made due: the alarm entered
//! its state at an instant of its own, the trust policy withheld the grant from the start,
//! and AppConfig's watch of the alarm was owed from the first monitoring instant of the
//! deployment — so [`settle`] may be called by a reporting operation. It cannot make a
//! later caller see anything the clock had not already made true.
//!
//! An engine carrying no CloudWatch store, an alarm that store holds none of, or a role the
//! account's IAM holds none of shows NOTHING: the alarm or the role may belong to another
//! account or to a client that never told this emulator about it, so the deployment goes
//! ahead rather than being rolled back for a state nothing could be read from — the same
//! reading `service-com-amazonaws-codedeploy-codedeploy-20141006`'s
//! `state::service_role::unassumable_role_message` takes of a service role.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;

use crate::ops::types::Monitor;

/// The CloudWatch store an environment's monitors are read from: that service's OWN state
/// struct, reached through the SAME shared engine this service's store lives in.
type CloudWatchState = cloudwatch::ComAmazonawsCloudwatchGraniteserviceversion20100801State;

/// The IAM store a monitor's `AlarmRoleArn` is resolved against: that service's OWN state
/// struct, in the same shared engine, so the records `CreateRole` wrote are what is read.
type IamState = iam::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The alarm state a monitored deployment is rolled back by — one of CloudWatch's three
/// `StateValue`s, read through THAT service's own constant so the two services cannot
/// disagree about how the alarming state is spelled.
pub const ALARM_STATE_ALARMING: &str = cloudwatch::ALARM_STATE_ALARM;

/// The service principal AppConfig monitors an alarm as.
///
/// A role AppConfig is to monitor an alarm under has to trust this principal for
/// `sts:AssumeRole`; the AppConfig User Guide's monitoring step ("Configuring Amazon
/// CloudWatch alarms for AppConfig") gives exactly that trust relationship.
pub const SERVICE_PRINCIPAL: &str = "appconfig.amazonaws.com";

/// The most environments one [`settle`] call watches, and the most alarms and roles one read
/// of the CloudWatch and IAM stores keeps.
///
/// Well above AppConfig's own published bound of five monitors per environment times the
/// environments one account holds deployments in flight into, so no realistic store is cut
/// short by it; a store holding more contributes the first ones walked, which is the "what
/// cannot be finished being read shows nothing" reading every cross-store read in this tree
/// takes.
pub const MAX_WATCHED: usize = 512;

/// Why AppConfig rolls a monitored deployment back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Breach {
    /// The monitored alarm is in [`ALARM_STATE_ALARMING`], since the instant it entered
    /// that state.
    Alarming {
        alarm_arn: String,
        since: SystemTime,
    },
    /// The monitored alarm cannot be read, because the monitor role does not let
    /// [`SERVICE_PRINCIPAL`] assume it.
    Unmonitorable { alarm_arn: String, role_arn: String },
}

impl Breach {
    /// The `TriggeredBy` the rollback's event carries.
    ///
    /// The enum's members are `USER`, `APPCONFIG`, `CLOUDWATCH_ALARM` and `INTERNAL_ERROR`,
    /// and "events can be triggered by a user, AppConfig, an Amazon CloudWatch alarm, or an
    /// internal error" (`smithy_query(query="shape",shape="DeploymentEvent")`): a rollback an
    /// alarm initiated is the alarm's, and one AppConfig started because the monitoring the
    /// deployment needs could not be performed at all is attributed to the error that
    /// stopped it rather than to the alarm, which said nothing.
    pub fn triggered_by(&self) -> &'static str {
        match self {
            Self::Alarming { .. } => super::TRIGGERED_BY_CLOUDWATCH_ALARM,
            Self::Unmonitorable { .. } => super::TRIGGERED_BY_INTERNAL_ERROR,
        }
    }

    /// The rollback event's `Description`.
    ///
    /// "Descriptions include … the Amazon Web Services account or the Amazon CloudWatch
    /// alarm ARN that initiated a rollback"
    /// (`smithy_query(query="doc",shape="DeploymentEvent",member="Description")`), so a
    /// rollback an alarm initiated is reported as that alarm's ARN, bare. A rollback
    /// AppConfig started because it could not read the alarm at all is not the alarm's, and
    /// is reported as the reading that failed, naming the role
    /// ([`unable_to_assume_monitor_role`]).
    pub fn description(&self) -> String {
        match self {
            Self::Alarming { alarm_arn, .. } => alarm_arn.clone(),
            Self::Unmonitorable { role_arn, .. } => unable_to_assume_monitor_role(role_arn),
        }
    }

    /// The instant this breach made the rollback due for a deployment started at
    /// `started_at`.
    ///
    /// AppConfig watches the alarms of a deployment it is working on, so a breach is
    /// observed at a monitoring instant of THAT deployment and never before it began: an
    /// alarm that went alarming while the deployment was in flight is observed when it did,
    /// and one that was already alarming when the deployment started — like a trust policy
    /// that withheld the grant all along — is observed at the deployment's FIRST monitoring
    /// instant, one stage span in ([`super::STAGE_DURATION`]), so a caller reading the
    /// deployment straight after starting it still sees it in flight.
    pub fn due_from(&self, started_at: SystemTime) -> SystemTime {
        let first_watch = started_at
            .checked_add(super::STAGE_DURATION)
            .unwrap_or(started_at);
        match self {
            Self::Alarming { since, .. } => (*since).max(first_watch),
            Self::Unmonitorable { .. } => first_watch,
        }
    }
}

/// One deployment in flight whose environment declares monitors: everything needed to read
/// those monitors and to decide what they already made due for it.
#[derive(Debug, Clone)]
struct Watched {
    application_id: String,
    environment_id: String,
    deployment_number: i32,
    started_at: SystemTime,
    /// The instant the deployment stops being eligible for automatic rollback: its own
    /// completion ([`super::Deployment::monitored_until`]).
    monitored_until: SystemTime,
    /// The environment's monitors, in the order it declares them.
    monitors: Vec<Monitor>,
}

/// What the account's CloudWatch and IAM stores show about the alarms a set of monitors
/// name, as ONE snapshot.
///
/// Built with those stores' guards held and holding none afterwards, and read from ONE
/// snapshot, so two deployments settled by the same call cannot disagree about an alarm.
#[derive(Debug, Clone, Default)]
pub struct MonitorHealth {
    /// For each monitored alarm the CloudWatch store reports in [`ALARM_STATE_ALARMING`],
    /// the instant it entered that state.
    alarming: BTreeMap<String, SystemTime>,
    /// The monitor role ARNs the account's IAM shows [`SERVICE_PRINCIPAL`] cannot assume.
    unassumable: BTreeSet<String>,
}

impl MonitorHealth {
    /// The snapshot in which nothing is breached: what an engine carrying neither store
    /// shows.
    pub fn none() -> Self {
        Self::default()
    }

    /// What the two stores show about the alarms and roles `monitors` name.
    ///
    /// Only the ARNs the monitors actually name are looked up, so a store holding alarms and
    /// roles nothing monitors is not walked for them.
    pub fn read<'monitors, Eng: Send + Sync + 'static>(
        engine: &Eng,
        monitors: impl IntoIterator<Item = &'monitors Monitor>,
    ) -> Self {
        let mut alarm_arns: BTreeSet<&str> = BTreeSet::new();
        let mut role_arns: BTreeSet<&str> = BTreeSet::new();
        for monitor in monitors {
            if alarm_arns.len() < MAX_WATCHED && !monitor.alarm_arn.is_empty() {
                alarm_arns.insert(monitor.alarm_arn.as_str());
            }
            if let Some(role_arn) = monitor
                .alarm_role_arn
                .as_deref()
                .filter(|role_arn| !role_arn.is_empty())
            {
                if role_arns.len() < MAX_WATCHED {
                    role_arns.insert(role_arn);
                }
            }
        }
        Self {
            alarming: alarming_at(engine, &alarm_arns),
            unassumable: unassumable_at(engine, &role_arns),
        }
    }

    /// The first of `monitors` this snapshot shows a breach of, in the order the environment
    /// declares them, or `None` when it shows none.
    ///
    /// A monitor whose role cannot be assumed is breached whatever the alarm says: AppConfig
    /// cannot read that alarm at all, so no state of it is evidence of anything.
    pub fn breach(&self, monitors: &[Monitor]) -> Option<Breach> {
        monitors.iter().find_map(|monitor| {
            let role_arn = monitor.alarm_role_arn.as_deref().unwrap_or_default();
            if self.unassumable.contains(role_arn) {
                return Some(Breach::Unmonitorable {
                    alarm_arn: monitor.alarm_arn.clone(),
                    role_arn: role_arn.to_string(),
                });
            }
            self.alarming
                .get(&monitor.alarm_arn)
                .map(|since| Breach::Alarming {
                    alarm_arn: monitor.alarm_arn.clone(),
                    since: *since,
                })
        })
    }
}

/// Which of `alarm_arns` the account's CloudWatch store reports in
/// [`ALARM_STATE_ALARMING`], each with the instant it entered that state.
///
/// All three kinds of alarm CloudWatch holds are read — a monitor carries an alarm ARN and
/// not the kind of alarm behind it — and each reports its own `StateValue` and ARN.
fn alarming_at<Eng: Send + Sync + 'static>(
    engine: &Eng,
    alarm_arns: &BTreeSet<&str>,
) -> BTreeMap<String, SystemTime> {
    let mut alarming = BTreeMap::new();
    if alarm_arns.is_empty() {
        return alarming;
    }
    let Some(state) = cloudwatch::service_state(engine) else {
        return alarming;
    };
    let guard = state.lock();
    let store: &CloudWatchState = &guard;
    let metric = store.alarms.values().map(|stored| {
        (
            &stored.alarm.state_value,
            &stored.alarm.state_updated_timestamp,
            &stored.alarm.alarm_arn,
        )
    });
    let composite = store.composite_alarms.values().map(|stored| {
        (
            &stored.alarm.state_value,
            &stored.alarm.state_updated_timestamp,
            &stored.alarm.alarm_arn,
        )
    });
    let logs = store.log_alarms.values().map(|stored| {
        (
            &stored.alarm.state_value,
            &stored.alarm.state_updated_timestamp,
            &stored.alarm.alarm_arn,
        )
    });
    for (state_value, state_updated, arn) in metric.chain(composite).chain(logs) {
        if alarming.len() >= MAX_WATCHED {
            break;
        }
        if state_value.as_deref() != Some(ALARM_STATE_ALARMING) {
            continue;
        }
        let Some(arn) = arn.as_deref().filter(|arn| alarm_arns.contains(*arn)) else {
            continue;
        };
        // An alarm reporting no `StateUpdatedTimestamp` reports no instant it entered the
        // state at, which is read as "already alarming": the earliest instant there is, so
        // the breach is observed at the deployment's first monitoring instant.
        let since = state_updated
            .as_ref()
            .map_or(SystemTime::UNIX_EPOCH, |stamp| {
                at_epoch_secs(cloudwatch::from_timestamp(stamp))
            });
        alarming.insert(arn.to_string(), since);
    }
    alarming
}

/// Which of `role_arns` the account's IAM shows [`SERVICE_PRINCIPAL`] cannot assume.
///
/// An ARN the IAM store holds no role for is NOT evidence of a missing trust — the role may
/// belong to another account, or to a client that never told this emulator about it — so it
/// is left out, and so is a role whose document this emulator cannot read as a policy at
/// all. Only a role this emulator holds and whose trust policy withholds the grant is
/// reported.
fn unassumable_at<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arns: &BTreeSet<&str>,
) -> BTreeSet<String> {
    let mut unassumable = BTreeSet::new();
    if role_arns.is_empty() {
        return unassumable;
    }
    let Some(state) = iam::service_state(engine) else {
        return unassumable;
    };
    let guard = state.lock();
    let store: &IamState = &guard;
    for role_arn in role_arns {
        if unassumable.len() >= MAX_WATCHED {
            break;
        }
        let Some(role) = store.roles.values().find(|role| role.arn == *role_arn) else {
            continue;
        };
        let Some(document) = role.assume_role_policy_document.as_deref() else {
            continue;
        };
        // The rule itself is IAM's, not this service's, so it is read through the ONE
        // evaluator every service that resolves a caller-supplied service role shares: the
        // statement must name the principal and `sts:AssumeRole`, an explicit `Deny` wins
        // over any `Allow`, and the document is percent-decoded before it is parsed.
        if ::service_common::iam_trust_policy::admits_service_principal(document, SERVICE_PRINCIPAL)
            == Some(false)
        {
            unassumable.insert((*role_arn).to_string());
        }
    }
    unassumable
}

/// Epoch seconds as an instant.
///
/// An instant the duration type cannot represent — a value that is not a finite, positive
/// number of seconds, or one too large for it — is read as the earliest instant there is,
/// which is the same reading an absent one gets ([`alarming_at`]): nothing here panics on a
/// stored value, however it got there.
fn at_epoch_secs(secs: f64) -> SystemTime {
    if !secs.is_finite() || secs <= 0.0 {
        return SystemTime::UNIX_EPOCH;
    }
    std::time::Duration::try_from_secs_f64(secs)
        .ok()
        .and_then(|since_epoch| SystemTime::UNIX_EPOCH.checked_add(since_epoch))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The ARN of the first role of `monitors` the account's IAM shows [`SERVICE_PRINCIPAL`]
/// cannot assume, or `None` when it shows that of none of them.
///
/// `pub` because it is the one question a caller may want answered about an environment's
/// monitors without settling anything: a deployment into such an environment is NOT refused
/// up front — AppConfig accepts it and rolls it back once its own monitoring could not be
/// performed ([`settle`]) — so this is a reading, not a gate.
///
/// The non-answers are the ones every other reading in this module gives: a role the store
/// holds none of, or a document that cannot be read as a policy, shows nothing.
///
/// Call it with no store lock held, from the handler's `&*self.engine`.
pub fn unassumable_monitor_role<Eng: Send + Sync + 'static>(
    engine: &Eng,
    monitors: &[Monitor],
) -> Option<String> {
    if monitors.is_empty() {
        return None;
    }
    match MonitorHealth::read(engine, monitors.iter()).breach(monitors) {
        Some(Breach::Unmonitorable { role_arn, .. }) => Some(role_arn),
        _ => None,
    }
}

/// `Description` for the rollback AppConfig performs when it cannot assume the role an
/// environment's monitor named, which is what left the monitored alarm unreadable.
///
/// Fixed text plus the one variable part: the `AlarmRoleArn` taken from the environment's
/// `Monitors`. Nothing of any one request is stored here.
pub fn unable_to_assume_monitor_role(role_arn: &str) -> String {
    format!("Unable to assume environment monitor role: {role_arn}")
}

/// Roll back every deployment in flight whose environment's monitors already made a
/// rollback due at `now`, and bring the environments holding them up to the state that
/// leaves them in.
///
/// Call it with no store lock held, from the handler's `&*self.engine`, BEFORE the
/// operation materialises the environment it reads: a breach that fell due while the
/// deployment was still in flight is what the clock owed, so it is applied before the
/// completion a later instant would otherwise have owed.
///
/// Reads this service's store first and touches CloudWatch and IAM only when an environment
/// with monitors is actually holding a deployment in flight, so an account using no monitors
/// pays one lock for it.
pub fn settle<Eng: Send + Sync + 'static>(engine: &Eng, now: SystemTime) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let watched: Vec<Watched> = {
        let guard = state.lock();
        watched_deployments(&guard)
    };
    if watched.is_empty() {
        return;
    }
    let health = MonitorHealth::read(
        engine,
        watched.iter().flat_map(|watch| watch.monitors.iter()),
    );
    let mut store = state.lock();
    for watch in &watched {
        let Some(breach) = health.breach(&watch.monitors) else {
            continue;
        };
        let due = breach.due_from(watch.started_at);
        // Only a breach the clock has already reached, and one that fell due while this
        // deployment was still eligible for automatic rollback, rolls it back.
        if due > now || due >= watch.monitored_until {
            continue;
        }
        let Some(environment) = store
            .application_mut(&watch.application_id)
            .and_then(|application| application.environments.get_mut(&watch.environment_id))
        else {
            continue;
        };
        if let Some(deployment) = environment.deployments.get_mut(&watch.deployment_number) {
            roll_back(deployment, &breach, due);
        }
        // The rollback itself falls due at an instant of its own, which the clock may have
        // reached too: the environment is brought up to whatever this left it owing.
        environment.materialize(now);
    }
}

/// The deployments in flight whose environment declares monitors.
fn watched_deployments(store: &super::ComAmazonawsAppconfigAmazonappconfigState) -> Vec<Watched> {
    let mut watched = Vec::new();
    for application in store.all_applications() {
        for environment in application.environments.values() {
            if environment.monitors.is_empty() {
                continue;
            }
            for deployment in environment.deployments.values() {
                if watched.len() >= MAX_WATCHED {
                    return watched;
                }
                if !deployment.is_in_flight() {
                    continue;
                }
                watched.push(Watched {
                    application_id: environment.application_id.clone(),
                    environment_id: environment.id.clone(),
                    deployment_number: deployment.deployment_number,
                    started_at: deployment.started_at,
                    monitored_until: deployment.monitored_until(),
                    monitors: environment.monitors.clone(),
                });
            }
        }
    }
    watched
}

/// Start the rollback `breach` made due at `due` on `deployment`, and attribute the events
/// it logs to what asked for it.
///
/// The rollback is AppConfig's own, so it is the one [`super::Deployment::begin_rollback`]
/// performs — the deployment reports it in progress and the clock owes the `ROLLED_BACK` it
/// ends in — and only the attribution and the description are the breach's, the same way an
/// extension's abort directive records itself
/// (`super::extension_actions::apply_roll_back`). The description is also recorded ON the
/// deployment ([`super::Deployment::rollback_description`]), so the event the clock logs when
/// the rollback finishes names the same cause as the one that said it started.
fn roll_back(deployment: &mut super::Deployment, breach: &Breach, due: SystemTime) {
    if deployment.begin_rollback(due).is_err() {
        return;
    }
    let description = breach.description();
    if let Some(event) = deployment.event_log.last_mut() {
        event.triggered_by = Some(breach.triggered_by().to_string());
        event.description = Some(description.clone());
    }
    deployment.rollback_description = Some(description);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant a breach is observed at never precedes the deployment's first monitoring
    /// instant, and is the alarm's own instant once the deployment is under way.
    #[test]
    fn a_breach_is_observed_at_a_monitoring_instant_of_the_deployment() {
        let started_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let first_watch = started_at + super::super::STAGE_DURATION;
        let rows: [(&str, Breach, SystemTime); 3] = [
            (
                "an alarm that was already alarming",
                Breach::Alarming {
                    alarm_arn: "arn:aws:cloudwatch:us-east-1:111122223333:alarm:a".to_string(),
                    since: SystemTime::UNIX_EPOCH,
                },
                first_watch,
            ),
            (
                "an alarm that went alarming while the deployment was in flight",
                Breach::Alarming {
                    alarm_arn: "arn:aws:cloudwatch:us-east-1:111122223333:alarm:b".to_string(),
                    since: started_at + std::time::Duration::from_secs(30),
                },
                started_at + std::time::Duration::from_secs(30),
            ),
            (
                "a monitor role AppConfig cannot assume",
                Breach::Unmonitorable {
                    alarm_arn: "arn:aws:cloudwatch:us-east-1:111122223333:alarm:c".to_string(),
                    role_arn: "arn:aws:iam::111122223333:role/monitor".to_string(),
                },
                first_watch,
            ),
        ];
        for (case, breach, expected) in rows {
            let actual = breach.due_from(started_at);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the breach due at {expected:?}, got {actual:?}"
            );
        }
    }

    /// A monitor whose role cannot be assumed is breached whatever its alarm says, and the
    /// monitors are examined in the order the environment declares them.
    #[test]
    fn the_role_a_monitor_names_decides_before_the_state_of_its_alarm() {
        let alarm = "arn:aws:cloudwatch:us-east-1:111122223333:alarm:watched".to_string();
        let role = "arn:aws:iam::111122223333:role/monitor".to_string();
        let health = MonitorHealth {
            alarming: BTreeMap::from([(alarm.clone(), SystemTime::UNIX_EPOCH)]),
            unassumable: BTreeSet::from([role.clone()]),
        };
        let with_role = vec![Monitor {
            alarm_arn: alarm.clone(),
            alarm_role_arn: Some(role.clone()),
        }];
        assert_eq!(
            health.breach(&with_role),
            Some(Breach::Unmonitorable {
                alarm_arn: alarm.clone(),
                role_arn: role.clone(),
            }),
            "case \"an unassumable role\": expected the unreadable monitor to decide, got {:?}",
            health.breach(&with_role)
        );
        let without_role = vec![Monitor {
            alarm_arn: alarm.clone(),
            alarm_role_arn: None,
        }];
        assert_eq!(
            health.breach(&without_role),
            Some(Breach::Alarming {
                alarm_arn: alarm.clone(),
                since: SystemTime::UNIX_EPOCH,
            }),
            "case \"no role at all\": expected the alarm's own state to decide, got {:?}",
            health.breach(&without_role)
        );
        assert_eq!(
            MonitorHealth::none().breach(&with_role),
            None,
            "case \"nothing read\": a snapshot that shows nothing breaches nothing"
        );
    }
}
