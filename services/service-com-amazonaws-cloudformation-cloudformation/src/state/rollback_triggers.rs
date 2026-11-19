// SPDX-License-Identifier: CC0-1.0

//! The rollback TRIGGERS of one stack operation: the CloudWatch alarms a
//! `RollbackConfiguration` names, and the rollback an alarm in `ALARM` forces.
//!
//! ## The rule this module implements
//!
//! A `RollbackConfiguration` is not a record a stack merely reports. "If any of the alarms
//! you specify goes to ALARM state during the stack operation or this monitoring period,
//! CloudFormation rolls back the entire stack operation"
//! (`RollbackConfiguration$RollbackTriggers`) — so an operation monitored by a trigger whose
//! alarm is in that state does NOT reach its completed status: it takes its failed status
//! and is then rolled back exactly as an operation a resource failed is, through the one
//! rollback path this service has ([`super::StoredStack::begin_rollback`] for a create,
//! [`super::StoredStack::begin_owed_update_rollback`] for an update). A caller that asked
//! for the failed state to be kept (`DisableRollback`, `OnFailure=DO_NOTHING`) gets that
//! state instead, as it does for every other failure.
//!
//! What a trigger names is read, never assumed: the alarm's `StateValue` comes from Amazon
//! CloudWatch's own `DescribeAlarms`, called in-process over the signed transport every other
//! cross-service call of this service rides ([`super::provisioning`]). An alarm no CloudWatch
//! record answers for, and a read CloudWatch does not answer at all, are no evidence that
//! anything is in `ALARM`, so the operation is left alone — the one thing a trigger can do is
//! roll an operation back, and only an alarm really in that state does it.
//!
//! ## When the triggers are read
//!
//! They are read TWICE over the operation they monitor, because the alarm can already be in
//! that state when the operation begins and can enter it while the operation runs.
//!
//! The first read is the operation's own, before it deploys anything
//! ([`firing_trigger_refusal`], called by `CreateStack`, `UpdateStack` and
//! `ExecuteChangeSet` off the store lock): monitoring covers "the stack operation" itself, so
//! an operation whose alarm is ALREADY in that state is rolled back with NO resource change
//! carried out at all — nothing is asked of any service that owns one of the stack's
//! resources, which is the refusal channel every rollback CloudFormation itself reaches
//! before asking anybody goes through ([`super::provisioning::reconcile`]'s `refused`).
//!
//! The second read happens while the operation is IN FLIGHT, from the operations that report
//! a stack's progress ([`super::provisioning::advance_stabilisations`] drives it, before those
//! handlers take the store lock) — the same place the waits on the owning services are read,
//! and for the same reason: a call to another service cannot be made from under this store's
//! lock, and the failure it uncovers was owed at the instant the alarm entered that state
//! whether or not anybody called.
//!
//! `MonitoringTimeInMinutes` is the period CloudFormation goes on
//! watching the triggers for "after the stack creation or update operation deploys all
//! necessary resources" — so an operation that asked for one is still IN FLIGHT once its
//! resources have settled, and is held open, and monitored, until that period is through
//! ([`StoredStack::trigger_monitoring_until`], read by [`super::StoredStack::settle`]): an
//! alarm that enters `ALARM` after the change has landed rolls the operation back, which is
//! what that period exists for. The period keeps the value the request stated — it is a wait
//! the caller asked for — and is bounded only by the ceiling this emulator holds one operation
//! open for ([`super::MAX_TRIGGER_MONITORING`]), so no request can hold one open for the hours
//! the member admits.
//!
//! What a triggered failure then DOES is what every failure the clock makes due on this
//! service does: the rollback, or the failed state a caller asked to keep. A create that asked
//! for `OnFailure=DELETE` is the one request that is not read here — the stack records the
//! rollback setting that `DisableRollback` and `OnFailure=DO_NOTHING` both spell
//! ([`super::StoredStack::disable_rollback`]) but not the failure ACTION a create named, so a
//! triggered create settles in [`super::status::ROLLBACK_COMPLETE`] where a create whose
//! RESOURCE failed would have taken the stack away ([`crate::ops::create_stack`] reads the
//! member it still holds). That is the same divergence every clock-driven failure of this
//! service has — an expired wait condition and a resource an owning service never got ready
//! both roll back too — and closing it means recording that action on the stack, which is a
//! change of its own rather than part of this one.

use std::time::SystemTime;

use serde_json::{json, Value};

use super::provisioner::{CLOUDWATCH_SIGNING_NAME, CLOUDWATCH_TARGET_PREFIX};
use super::provisioning::{call_json_rpc, unwind, OnRefusal, Teardown, AWS_JSON_1_0};
use super::StoredRollbackTrigger;

/// What the failure a trigger forces OWES the services that own the stack's resources.
///
/// It is the same work ANY failure of the same operation owes — a failed create removes what
/// it had made, a failed update puts the previous configuration back — so this is the type
/// the other clock-driven failure of this service answers with
/// ([`super::wait_conditions::ExpiredWaitOwes`]) rather than a second reading of it.
use super::wait_conditions::ExpiredWaitOwes as Owes;

/// The alarm state a trigger rolls its operation back for: `StateValue` `ALARM`, one of the
/// three the model's `StateValue` enum carries (`ALARM | INSUFFICIENT_DATA | OK`). The other
/// two are an alarm that is not firing.
pub const ALARM_STATE: &str = "ALARM";

/// The alarm TYPES the read asks CloudWatch about.
///
/// `DescribeAlarms$AlarmTypes`: "If you omit this parameter, only metric alarms are returned,
/// even if composite alarms or log alarms exist in the account" — and a rollback trigger may
/// name either kind of alarm ([`super::ROLLBACK_TRIGGER_TYPES`]), so both are asked for or a
/// composite trigger could never fire.
const ALARM_TYPES: [&str; 2] = ["MetricAlarm", "CompositeAlarm"];

/// The lists a `DescribeAlarms` answer reports alarms in, one per alarm type asked for
/// (`DescribeAlarmsOutput$MetricAlarms` / `$CompositeAlarms`).
const ALARM_LISTS: [&str; 2] = ["MetricAlarms", "CompositeAlarms"];

/// The ARN element that identifies an alarm, which an alarm ARN's resource part starts with
/// (`arn:aws:cloudwatch:<region>:<account>:alarm:<alarm name>`).
const ALARM_RESOURCE_PREFIX: &str = "alarm:";

/// How many trigger alarms ONE request reads, across every monitored operation of the Region.
///
/// Reading a trigger is a call into CloudWatch, and the number of them a pass would make is
/// the PRODUCT of two counts a caller chooses: how many monitored operations are in flight,
/// and how many triggers each of them names. The second is bounded by the model
/// ([`super::MAX_ROLLBACK_TRIGGERS`]); this bounds the product, so one reporting call cannot
/// be amplified into an unbounded number of in-process calls however many stacks are
/// deploying at once. It is that ceiling times ten operations — more monitored operations in
/// flight at one instant than a client driving one deployment has.
///
/// A pass that runs out of budget leaves the operations it did not reach to the next one, and
/// nothing is lost by that: the rollback a trigger forces is owed from the instant its alarm
/// entered that state, and this read is what materialises it, so it is materialised by the
/// next call instead. An operation is read WHOLE or not at all, so no alarm is passed over as
/// not firing.
const ALARM_READS_PER_PASS: usize = super::MAX_ROLLBACK_TRIGGERS * 10;

/// How many colon-separated fields an ARN's head has before its resource part
/// (`arn:<partition>:<service>:<region>:<account-id>:<resource>`).
const ARN_HEAD_FIELDS: usize = 5;

/// Which ALARM one rollback trigger's `Arn` names: the Region it lives in and its name.
///
/// `RollbackTrigger$Arn` is "the Amazon Resource Name (ARN) of the rollback trigger", and the
/// trigger's `Type` is one of the two CloudWatch alarm types, so the ARN is an alarm's: its
/// Region field says where the alarm is and its resource part is `alarm:` plus the alarm
/// name. An ARN that names anything else identifies no alarm, and nothing is read for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlarmReference<'a> {
    /// The ARN's Region field, empty for an ARN that carries none.
    pub region: &'a str,
    /// The alarm name: the resource part after `alarm:`, which is the name
    /// `DescribeAlarms$AlarmNames` takes.
    pub name: &'a str,
}

impl<'a> AlarmReference<'a> {
    /// The alarm `arn` names, or `None` where it is not an alarm ARN at all.
    pub fn of(arn: &'a str) -> Option<Self> {
        let mut fields = arn.splitn(ARN_HEAD_FIELDS + 1, ':');
        if fields.next()? != "arn" {
            return None;
        }
        let _partition = fields.next()?;
        if fields.next()? != "cloudwatch" {
            return None;
        }
        let region = fields.next()?;
        let _account = fields.next()?;
        // The alarm NAME is whatever follows `alarm:`, taken whole: a name may carry
        // characters an ARN's earlier fields never do, so the split stops here.
        let name = fields.next()?.strip_prefix(ALARM_RESOURCE_PREFIX)?;
        if name.is_empty() {
            return None;
        }
        Some(Self { region, name })
    }
}

/// The fixed text of the reason a rollback the triggers forced reports, which the alarms that
/// were in `ALARM` follow.
const TRIGGER_FAILURE_PREFIX: &str = "The following CloudWatch Alarms were in ALARM state: ";

/// The `StackStatusReason` an operation reports when its ROLLBACK TRIGGERS are what rolled it
/// back.
///
/// One SUMMARY sentence, in the shape CloudFormation words this failure in:
/// [`TRIGGER_FAILURE_PREFIX`] and then the alarms that were in that state, comma-separated,
/// with nothing around the list and nothing after it. What the sentence NAMES is each alarm's
/// own name — `PutMetricAlarm$AlarmName`, which is also the name the trigger's ARN carries
/// ([`AlarmReference`]) — and not the ARN the configuration declared the trigger with: a
/// client reading this field to learn which alarm aborted the operation takes the name out of
/// it and asks CloudWatch about that alarm (`DescribeAlarms$AlarmNames`), so the name is what
/// it has to find there.
///
/// The sentence carries no trailing separator, unlike the resource summary
/// ([`super::resource_failure_summary`]): a follow-on clause this service adds to a reason is
/// worded onto the sentence it is given ([`super::rollback_requested`]), whether or not that
/// sentence left room for one.
pub fn trigger_failure_summary<'a>(alarm_names: impl IntoIterator<Item = &'a str>) -> String {
    let named: Vec<&str> = alarm_names.into_iter().collect();
    format!("{TRIGGER_FAILURE_PREFIX}{}", named.join(", "))
}

impl super::StoredStack {
    /// The rollback triggers MONITORING the operation in flight, or an empty list where there
    /// is no such operation or it has none.
    ///
    /// Only a CREATE or an UPDATE is monitored: those are the two operations
    /// `RollbackConfiguration` is supplied on ("after the stack creation or update operation
    /// deploys all necessary resources"), and a rollback already under way is what a trigger
    /// would ask for anyway. An update that has applied every change and is removing the
    /// resources it replaced ([`super::status::UPDATE_COMPLETE_CLEANUP_IN_PROGRESS`]) is still
    /// that update, so it is still monitored.
    pub fn monitored_rollback_triggers(&self) -> Vec<StoredRollbackTrigger> {
        if self.pending.is_none() {
            return Vec::new();
        }
        if !monitored_status(&self.status) {
            return Vec::new();
        }
        self.rollback_configuration
            .iter()
            .flat_map(|configuration| configuration.triggers.iter().cloned())
            .collect()
    }

    /// Until when the rollback triggers of the operation in flight are watched, where that
    /// period outlasts the instant `due_at` the operation's own transition is due at — so the
    /// operation is held open until then rather than concluding with its monitoring period
    /// unspent.
    ///
    /// `MonitoringTimeInMinutes` is the period CloudFormation watches the triggers for "after
    /// the stack creation or update operation deploys all necessary resources", and an alarm
    /// that enters `ALARM` inside it rolls the whole operation back — so an operation that
    /// asked for one is still IN FLIGHT while it runs, and the status a client polls says so.
    /// The period is read from the configuration the operation supplied, as stated, bounded by
    /// the ceiling this emulator holds an operation open for
    /// ([`super::MAX_TRIGGER_MONITORING`]).
    ///
    /// `None` is an operation with nothing to watch: one that named no trigger, one that asked
    /// for no monitoring period (`MonitoringTimeInMinutes` absent, zero or negative — "minimum
    /// value is 0", `RollbackConfiguration$MonitoringTimeInMinutes`), and one whose triggers
    /// are not being monitored at all ([`Self::monitored_rollback_triggers`]).
    pub fn trigger_monitoring_until(&self, due_at: SystemTime) -> Option<SystemTime> {
        if self.pending.is_none() || !monitored_status(&self.status) {
            return None;
        }
        let configuration = self.rollback_configuration.as_ref()?;
        if configuration.triggers.is_empty() {
            return None;
        }
        let minutes = u64::try_from(configuration.monitoring_time_in_minutes.unwrap_or(0)).ok()?;
        if minutes == 0 {
            return None;
        }
        let asked = ::std::time::Duration::from_secs(minutes.saturating_mul(60));
        Some(due_at + asked.min(super::MAX_TRIGGER_MONITORING))
    }

    /// Roll the operation in flight back because the alarms `fired` names — the alarms this
    /// operation's triggers name, by alarm name — are in `ALARM`, and answer the work that
    /// rollback leaves for the services that own the stack's resources.
    ///
    /// "CloudFormation rolls back the entire stack operation": the operation takes its failed
    /// status carrying [`trigger_failure_summary`] — nothing about the resources failed, so no
    /// resource's own event is rewritten — and the rollback that follows is the one every
    /// failure of that operation owes. A caller that asked for the failed state to be kept
    /// gets it, exactly as it does for a failure an owning service answered.
    ///
    /// Called with the store locked and the result acted on with it RELEASED. `None` is an
    /// operation this failure owes those services nothing for.
    pub fn fail_on_rollback_triggers(
        &mut self,
        now: SystemTime,
        fired: &[String],
    ) -> Option<Owes> {
        if fired.is_empty() || self.pending.is_none() {
            return None;
        }
        let creating = self.status == super::status::CREATE_IN_PROGRESS;
        let updating = self.status == super::status::UPDATE_IN_PROGRESS
            || self.status == super::status::UPDATE_COMPLETE_CLEANUP_IN_PROGRESS;
        if !creating && !updating {
            return None;
        }
        let reason = trigger_failure_summary(fired.iter().map(String::as_str));
        let failed_status = super::failed_status_of(&self.status);
        // Only a stack LEFT in the failed status rests in it, and only that one carries a
        // stack-level event for it: where the rollback begins at this very instant, its own
        // event is what reports the failure
        // ([`super::StoredStack::fail_transition_superseded`]).
        if self.disable_rollback {
            self.fail_transition(now, failed_status, reason.clone());
            return None;
        }
        self.fail_transition_superseded(now, failed_status, reason.clone());
        if updating {
            self.begin_owed_update_rollback(now, reason);
            return Some(Owes::Revert);
        }
        self.begin_rollback(now, reason);
        Some(Owes::Removal {
            resources: self.current_resources(),
            held: self.held_resource_ids(),
            needs: super::provisioning::dependencies_in_flight(self),
            policies: self.rolled_back_resource_policies(),
        })
    }
}

/// Whether a stack in `status` has a MONITORED operation in flight: one of the two operations
/// a `RollbackConfiguration` is supplied on, in a status that operation is still running in.
///
/// An update that has applied every change and is removing the resources it replaced is still
/// that update ([`super::status::UPDATE_COMPLETE_CLEANUP_IN_PROGRESS`]), so its triggers are
/// watched for as long as any other update's; a rollback is not monitored, since a rollback is
/// what a firing trigger would ask for anyway.
fn monitored_status(status: &str) -> bool {
    status == super::status::CREATE_IN_PROGRESS
        || status == super::status::UPDATE_IN_PROGRESS
        || status == super::status::UPDATE_COMPLETE_CLEANUP_IN_PROGRESS
}

/// The `StateValue` CloudWatch reports for the alarm `name` in `region`, or `None` where it
/// holds no such alarm or did not answer.
async fn alarm_state(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Option<String> {
    let answer = call_json_rpc(
        ctx,
        CLOUDWATCH_SIGNING_NAME,
        region,
        AWS_JSON_1_0,
        &format!("{CLOUDWATCH_TARGET_PREFIX}.DescribeAlarms"),
        &json!({"AlarmNames": [name], "AlarmTypes": ALARM_TYPES}),
    )
    .await
    .ok()?;
    ALARM_LISTS
        .iter()
        .filter_map(|list| answer.get(list).and_then(Value::as_array))
        .flatten()
        .find(|alarm| alarm.get("AlarmName").and_then(Value::as_str) == Some(name))
        .and_then(|alarm| alarm.get("StateValue").and_then(Value::as_str))
        .map(str::to_string)
}

/// Which alarms of `triggers` CloudWatch reports in [`ALARM_STATE`] right now, by alarm NAME
/// and in the order the configuration declared the triggers naming them.
///
/// The NAME, not the trigger's ARN, because the name is what the failure this read uncovers
/// reports ([`trigger_failure_summary`]) and what CloudWatch itself is asked about.
///
/// `stack_region` is the Region the stack's own calls are made in, which an alarm ARN
/// carrying no Region field is read in.
async fn alarms_in_alarm(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    stack_region: &str,
    triggers: &[StoredRollbackTrigger],
) -> Vec<String> {
    let mut fired = Vec::new();
    for trigger in triggers {
        let Some(alarm) = AlarmReference::of(&trigger.arn) else {
            continue;
        };
        let region = if alarm.region.is_empty() {
            stack_region
        } else {
            alarm.region
        };
        if alarm_state(ctx, region, alarm.name).await.as_deref() == Some(ALARM_STATE) {
            fired.push(alarm.name.to_string());
        }
    }
    fired
}

/// The refusal the rollback triggers of `configuration` ALREADY force on the operation about
/// to be carried out, or `None` where none of their alarms is in [`ALARM_STATE`].
///
/// "If any of the alarms you specify goes to ALARM state DURING THE STACK OPERATION or this
/// monitoring period, CloudFormation rolls back the entire stack operation": the operation
/// itself is monitored, so an alarm that is in that state when the operation begins rolls the
/// operation back before it changes anything — the stack's resources are not touched, which
/// is exactly what the deployment-refusal channel of
/// [`super::provisioning::reconcile`] does with the reason this answers
/// ([`trigger_failure_summary`]). The rollback, and the failed state a caller that asked for
/// it is left in instead, are then the ones that refusal already performs: no second reading
/// of what a failed operation of this service does.
///
/// Called by the operations a `RollbackConfiguration` is supplied on, with no store lock held
/// — each trigger is a read of CloudWatch's own `DescribeAlarms`, and the number of them one
/// call makes is bounded by the model ([`super::MAX_ROLLBACK_TRIGGERS`]). A configuration
/// naming no trigger reads nothing at all.
///
/// `region` is the Region the operation's own calls are made in, which a trigger ARN carrying
/// no Region field is read in.
pub async fn firing_trigger_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    configuration: Option<&super::StoredRollbackConfiguration>,
) -> Option<String> {
    let triggers = configuration.map(|configuration| configuration.triggers.as_slice())?;
    if triggers.is_empty() {
        return None;
    }
    let fired = alarms_in_alarm(ctx, region, triggers).await;
    if fired.is_empty() {
        return None;
    }
    Some(trigger_failure_summary(fired.iter().map(String::as_str)))
}

/// Read the rollback triggers of this Region's in-flight stack operations, and roll back
/// every operation one of its alarms is firing on.
///
/// The alarm reads reach CloudWatch's own handler and the rollback of a failed CREATE reaches
/// the services that own the resources it had made, so both are done with the store UNLOCKED
/// — which is why this runs from the operations that report a stack's progress rather than
/// from the store's own settle ([`super::provisioning::advance_stabilisations`] is what calls
/// it, before its handler takes the lock).
///
/// Only the stacks of the Region the request was signed for are read: that is the set the
/// operation calling this reports on, and the Region is what the credential scope of every
/// call made here names. How many of their triggers one pass reads is bounded
/// ([`ALARM_READS_PER_PASS`]).
pub(super) async fn settle_triggered_rollbacks(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    state: &::engine::ServiceState<super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
) {
    // Which operations are being monitored, and by what — read under a lock of its own,
    // because the reads below reach another service's handler and a client reading THIS
    // service must not be blocked behind them.
    let monitored: Vec<(String, Vec<StoredRollbackTrigger>)> = {
        let store = state.lock();
        store
            .stacks
            .iter()
            .filter(|stack| stack.region == region)
            .filter_map(|stack| {
                let triggers = stack.monitored_rollback_triggers();
                if triggers.is_empty() {
                    None
                } else {
                    Some((stack.stack_id.clone(), triggers))
                }
            })
            .collect()
    };
    let mut budget = ALARM_READS_PER_PASS;
    for (stack_id, triggers) in monitored {
        // The alarm reads one pass makes are bounded ([`ALARM_READS_PER_PASS`]). An
        // operation whose triggers do not all fit in what is left is SKIPPED rather than
        // read in part: reading some of a configuration's triggers would report an alarm
        // past the boundary as not firing, while skipping the operation leaves its failure
        // owed — the boundary moves as the operations ahead of it settle, and this pass
        // goes on to the ones that do fit.
        if triggers.len() > budget {
            continue;
        }
        budget -= triggers.len();
        let fired = alarms_in_alarm(ctx, region, &triggers).await;
        if fired.is_empty() {
            continue;
        }
        let owed = {
            let mut store = state.lock();
            let Some(stack) = store.resolve_mut(region, &stack_id) else {
                continue;
            };
            stack.fail_on_rollback_triggers(SystemTime::now(), &fired)
        };
        // A failed UPDATE's rollback is the one path every rollback this service owes goes
        // through, which the operation calling this drives for itself
        // ([`super::settle_due_rollbacks`]); a failed CREATE's removes what the create had
        // already made, here, exactly as every other failure of a create does.
        let Some(Owes::Removal {
            resources,
            held,
            needs,
            policies,
        }) = owed
        else {
            continue;
        };
        let deleted = unwind(
            ctx,
            region,
            &resources,
            &Teardown {
                stack_id: &stack_id,
                held: &held,
                needs: &needs,
                on_refusal: OnRefusal::Stop,
                policies,
                // The first walk of this rollback: nothing has been refused yet.
                left_alone: super::provisioning::NOTHING_LEFT_ALONE,
            },
        )
        .await;
        let mut store = state.lock();
        let Some(stack) = store.resolve_mut(region, &stack_id) else {
            continue;
        };
        stack.schedule_resource_events(&deleted.transitions);
        stack.record_materialised_resources(deleted.resources);
        // A resource whose owning service accepted the delete and still holds it holds this
        // rollback open, exactly as it holds the operation being rolled back open.
        stack.record_stabilisations(&deleted.removing, SystemTime::now());
        if let Some(failure) = deleted.failure {
            // Reported once this teardown has asked about every resource it still can
            // ([`super::StoredStack::hold_teardown_failure`]).
            if !stack.hold_teardown_failure(
                &deleted.transitions,
                &deleted.removing,
                &deleted.refused,
                super::status::ROLLBACK_FAILED,
                &failure,
            ) {
                stack.fail_transition(SystemTime::now(), super::status::ROLLBACK_FAILED, failure);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// One row of the ARN-reading table: the ARN, and the alarm it names (or none).
    struct ArnRow {
        case: &'static str,
        arn: &'static str,
        expected: Option<(&'static str, &'static str)>,
    }

    /// An alarm ARN is read as the Region and the alarm name it names, and an ARN that names
    /// anything else is no alarm at all.
    #[test]
    fn an_alarm_arn_is_read_as_its_region_and_its_name() {
        let rows: &[ArnRow] = &[
            ArnRow {
                case: "a plain alarm ARN",
                arn: "arn:aws:cloudwatch:us-east-1:000000000000:alarm:probe",
                expected: Some(("us-east-1", "probe")),
            },
            ArnRow {
                case: "an alarm name carrying a colon",
                arn: "arn:aws:cloudwatch:eu-west-2:000000000000:alarm:tier:probe",
                expected: Some(("eu-west-2", "tier:probe")),
            },
            ArnRow {
                case: "another partition",
                arn: "arn:aws-us-gov:cloudwatch:us-gov-west-1:000000000000:alarm:probe",
                expected: Some(("us-gov-west-1", "probe")),
            },
            ArnRow {
                case: "a composite alarm's ARN, which spells the same resource",
                arn: "arn:aws:cloudwatch:us-east-1:000000000000:alarm:roll-up",
                expected: Some(("us-east-1", "roll-up")),
            },
            ArnRow {
                case: "an ARN of another service",
                arn: "arn:aws:sns:us-east-1:000000000000:topic",
                expected: None,
            },
            ArnRow {
                case: "a CloudWatch ARN that is not an alarm",
                arn: "arn:aws:cloudwatch:us-east-1:000000000000:dashboard/board",
                expected: None,
            },
            ArnRow {
                case: "not an ARN at all",
                arn: "probe",
                expected: None,
            },
            ArnRow {
                case: "an alarm ARN with no name",
                arn: "arn:aws:cloudwatch:us-east-1:000000000000:alarm:",
                expected: None,
            },
        ];
        for row in rows {
            let read = AlarmReference::of(row.arn);
            let actual = read.as_ref().map(|alarm| (alarm.region, alarm.name));
            assert_eq!(
                actual, row.expected,
                "case {:?}: expected {:?} for {:?}, got {actual:?}",
                row.case, row.expected, row.arn
            );
        }
    }

    /// The summary names every alarm that was in `ALARM`, by NAME, in CloudFormation's own
    /// wording for this failure.
    #[test]
    fn the_summary_names_every_alarm_that_was_in_alarm() {
        for (case, names, expected) in [
            (
                "one alarm",
                vec!["deploy-errors"],
                "The following CloudWatch Alarms were in ALARM state: deploy-errors",
            ),
            (
                "two alarms",
                vec!["deploy-errors", "deploy-latency"],
                "The following CloudWatch Alarms were in ALARM state: deploy-errors, \
                 deploy-latency",
            ),
            (
                "an alarm name carrying a space and a colon",
                vec!["tier:probe one"],
                "The following CloudWatch Alarms were in ALARM state: tier:probe one",
            ),
        ] {
            let actual = trigger_failure_summary(names.iter().copied());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                !actual.contains('[') && !actual.ends_with('.') && !actual.ends_with(' '),
                "case {case:?}: the sentence brackets nothing and ends with the last alarm \
                 name, got {actual:?}"
            );
        }
    }

    /// One row of the monitoring-window table: the `MonitoringTimeInMinutes` the configuration
    /// states, how many triggers it names, and how long past the operation's own transition
    /// the triggers are watched for — `None` for an operation nothing watches after it.
    struct WindowRow {
        case: &'static str,
        minutes: Option<i32>,
        triggers: usize,
        watched_for: Option<Duration>,
    }

    /// The window the triggers of an update in flight are watched over is the period the
    /// configuration ASKED FOR, kept as the request stated it and bounded only by the ceiling
    /// this emulator holds one operation open for — and an operation with nothing to watch is
    /// watched for no time at all.
    ///
    /// The rows walk the member's published range (`MonitoringTimeInMinutes` is
    /// `@range(min: 0, max: 180)`): its own minimum, periods a client states, the ceiling
    /// itself, one minute past it and the member's maximum, beside a configuration that states
    /// no period and one that names no trigger.
    #[test]
    fn the_monitoring_window_is_the_period_the_configuration_asked_for() {
        let rows: &[WindowRow] = &[
            WindowRow {
                case: "no period stated",
                minutes: None,
                triggers: 1,
                watched_for: None,
            },
            WindowRow {
                case: "the member's own minimum",
                minutes: Some(0),
                triggers: 1,
                watched_for: None,
            },
            WindowRow {
                case: "one minute, kept as the request stated it",
                minutes: Some(1),
                triggers: 1,
                watched_for: Some(Duration::from_secs(60)),
            },
            WindowRow {
                case: "five minutes, kept as the request stated it",
                minutes: Some(5),
                triggers: 2,
                watched_for: Some(Duration::from_secs(5 * 60)),
            },
            WindowRow {
                case: "the ceiling itself",
                minutes: Some(10),
                triggers: 1,
                watched_for: Some(super::super::MAX_TRIGGER_MONITORING),
            },
            WindowRow {
                case: "one minute past the ceiling",
                minutes: Some(11),
                triggers: 1,
                watched_for: Some(super::super::MAX_TRIGGER_MONITORING),
            },
            WindowRow {
                case: "the member's own maximum",
                minutes: Some(180),
                triggers: 2,
                watched_for: Some(super::super::MAX_TRIGGER_MONITORING),
            },
            WindowRow {
                case: "a period asked for by a configuration naming no trigger",
                minutes: Some(5),
                triggers: 0,
                watched_for: None,
            },
        ];

        let now = SystemTime::now();
        let due_at = now + super::super::STATUS_TRANSITION_DELAY;
        for row in rows {
            let mut stack = super::super::StoredStack::new(
                "stack/monitored".to_string(),
                "monitored".to_string(),
                None,
                now,
            );
            stack.rollback_configuration = Some(super::super::StoredRollbackConfiguration {
                triggers: (0..row.triggers)
                    .map(|index| StoredRollbackTrigger {
                        arn: format!("arn:aws:cloudwatch:us-east-1:000000000000:alarm:probe-{index}"),
                        trigger_type: "AWS::CloudWatch::Alarm".to_string(),
                    })
                    .collect(),
                monitoring_time_in_minutes: row.minutes,
            });
            stack.begin_transition(
                now,
                super::super::status::UPDATE_IN_PROGRESS,
                super::super::status::UPDATE_COMPLETE,
                None,
                None,
            );
            let until = stack.trigger_monitoring_until(due_at);
            assert_eq!(
                until,
                row.watched_for.map(|window| due_at + window),
                "case {:?}: expected the triggers to be watched for {:?} past the transition, \
                 got {:?}",
                row.case,
                row.watched_for,
                until.map(|until| until.duration_since(due_at)),
            );
        }
    }

    /// ...and an operation nothing is in flight for, or one in a status no `RollbackConfiguration`
    /// is supplied on, is watched for no time at all however long a period it asked for.
    #[test]
    fn a_stack_with_no_monitored_operation_watches_its_triggers_for_nothing() {
        let now = SystemTime::now();
        let due_at = now + super::super::STATUS_TRANSITION_DELAY;
        let configuration = super::super::StoredRollbackConfiguration {
            triggers: vec![StoredRollbackTrigger {
                arn: "arn:aws:cloudwatch:us-east-1:000000000000:alarm:probe".to_string(),
                trigger_type: "AWS::CloudWatch::Alarm".to_string(),
            }],
            monitoring_time_in_minutes: Some(5),
        };

        // Nothing in flight: the stack settled in its terminal status.
        let mut settled = super::super::StoredStack::new(
            "stack/settled".to_string(),
            "settled".to_string(),
            None,
            now,
        );
        settled.rollback_configuration = Some(configuration.clone());
        settled.status = super::super::status::UPDATE_COMPLETE.to_string();
        assert_eq!(
            settled.trigger_monitoring_until(due_at),
            None,
            "a concluded operation's triggers are watched by nothing"
        );

        // In flight, but ROLLING BACK: a rollback is what a firing trigger would ask for.
        let mut rolling_back = super::super::StoredStack::new(
            "stack/rolling-back".to_string(),
            "rolling-back".to_string(),
            None,
            now,
        );
        rolling_back.rollback_configuration = Some(configuration);
        rolling_back.begin_transition(
            now,
            super::super::status::UPDATE_ROLLBACK_IN_PROGRESS,
            super::super::status::UPDATE_ROLLBACK_COMPLETE,
            None,
            None,
        );
        assert_eq!(
            rolling_back.trigger_monitoring_until(due_at),
            None,
            "a rollback in flight is not monitored by the triggers it was forced by"
        );
    }
}
