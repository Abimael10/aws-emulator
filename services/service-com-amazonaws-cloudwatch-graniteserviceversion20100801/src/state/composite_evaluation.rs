// SPDX-License-Identifier: CC0-1.0

//! How a COMPOSITE alarm's state is computed: the evaluation passes this store makes over the
//! alarms a rule references, and the one evaluation of one alarm they are made of.
//!
//! A composite alarm's state is not a value a caller writes but one DERIVED from the alarms its
//! `AlarmRule` references ([`super::alarm_rule`] holds the rule language and the parse;
//! [`super::composite_suppression`] decides what its actions are suppressed by), so it is
//! computed here from what the store holds — bounded, per call, by the same budget the
//! metric-alarm pass is bounded by.

use super::*;

impl ComAmazonawsCloudwatchGraniteserviceversion20100801State {
    /// Materialise every composite-alarm rule evaluation the clock has already made due.
    ///
    /// A composite alarm's state is not a value a caller writes: it is "the rule that this
    /// alarm uses to evaluate its alarm state" (`CompositeAlarm$AlarmRule`) applied to the
    /// states of the alarms the rule references, so it is COMPUTED here from the alarms the
    /// store holds. The write is one the clock owed: each evaluation is due from the call that
    /// configured the alarm ([`super::StoredCompositeAlarm::evaluation_due_secs`]) or from the
    /// instant a referenced alarm's state changed ([`Self::make_parents_due`]), and the outcome
    /// depends only on the stored alarm states — so whichever call materialises it, every
    /// later caller sees the same thing.
    pub fn evaluate_composite_alarms(
        &mut self,
        now_secs: f64,
        budget: &mut alarm_evaluation::EvaluationBudget,
    ) {
        // A composite alarm's rule can reference another composite alarm, so one alarm's new
        // state can change its parent's outcome too; one pass settles one level of that
        // chain. The number of passes is bounded by [`MAX_COMPOSITE_EVALUATION_PASSES`] as
        // well as by the number of composite alarms, and the NODES one call may evaluate are
        // bounded by [`MAX_COMPOSITE_EVALUATION_NODES`] — so the work ONE call can do is
        // bounded whatever the store holds, rather than growing with the alarms times the size
        // of their rules. An evaluation the budget stops is not answered wrongly, only
        // settled later: it stays due, and the next call carries it (the evaluation is
        // idempotent). The pass bound is also what stops a CYCLE of composite alarms: "the
        // evaluation of composite alarms stops if CloudWatch detects a cycle in the
        // evaluation path."
        // The composite alarms are a PASS of their own, owed its first evaluation whatever the
        // metric-alarm pass before it has already spent
        // ([`alarm_evaluation::EvaluationBudget::start_pass`]), so a store whose metric alarms
        // keep one request busy cannot leave its composite alarms in the state their last
        // write left behind.
        budget.start_pass();
        let passes = self
            .composite_alarms
            .len()
            .min(MAX_COMPOSITE_EVALUATION_PASSES);
        let mut nodes = MAX_COMPOSITE_EVALUATION_NODES;
        for _ in 0..passes {
            if budget.spent()
                || !self.evaluate_composite_alarms_once(now_secs, &mut nodes, budget)
                || nodes == 0
            {
                break;
            }
        }
    }

    /// One evaluation pass over every composite alarm, reporting whether any alarm's state
    /// value changed. Alarms are visited in the order their evaluations came due
    /// ([`alarm_evaluation::due_order`]), and the pass stops as soon as `nodes` or the
    /// request's own `budget` is spent.
    ///
    /// Only an alarm whose evaluation the clock has already made DUE is charged, so the one
    /// evaluation a pass is owed is spent on an alarm that has one to settle rather than on
    /// whichever name happens to come first.
    pub(super) fn evaluate_composite_alarms_once(
        &mut self,
        now_secs: f64,
        nodes: &mut usize,
        budget: &mut alarm_evaluation::EvaluationBudget,
    ) -> bool {
        let ordered = alarm_evaluation::due_order(
            self.composite_alarms
                .iter()
                .map(|(name, stored)| (name.as_str(), stored.evaluation_due_secs)),
        );
        let mut changed = false;
        for name in ordered {
            if *nodes == 0 || budget.spent() {
                break;
            }
            if !self.composite_alarm_evaluation_due(&name, now_secs) {
                continue;
            }
            budget.charge_alarm();
            changed |= self.evaluate_composite_alarm(&name, now_secs, nodes);
            budget.settled_one();
        }
        changed
    }

    /// Whether the composite alarm `name` has an evaluation the clock has already made due.
    pub(super) fn composite_alarm_evaluation_due(&self, name: &str, now_secs: f64) -> bool {
        self.composite_alarms
            .get(name)
            .is_some_and(|stored| now_secs >= stored.evaluation_due_secs)
    }

    /// Evaluate ONE composite alarm, reporting whether its state value changed.
    ///
    /// The alarm's rule nodes are charged to `budget`, and an alarm whose evaluation the
    /// remaining budget cannot pay for is left for a later call.
    pub(super) fn evaluate_composite_alarm(
        &mut self,
        name: &str,
        now_secs: f64,
        budget: &mut usize,
    ) -> bool {
        // Everything this evaluation reads is read before anything is written.
        let Some(stored) = self.composite_alarms.get(name) else {
            return false;
        };
        if now_secs < stored.evaluation_due_secs {
            return false;
        }
        // A rule carries at least one node, so every evaluation costs something and the
        // budget always advances.
        let cost = stored.rule_nodes.max(1);
        if cost > *budget {
            return false;
        }
        *budget -= cost;
        let rule = &stored.rule;
        let previous = AlarmStateSnapshot::of_composite_alarm(&stored.alarm);
        let forced = stored.forced_child_states.clone();
        let suppressor = stored.alarm.actions_suppressor.clone();
        let wait_period = stored.alarm.actions_suppressor_wait_period;
        let extension_period = stored.alarm.actions_suppressor_extension_period;
        let reported_suppression = stored.alarm.actions_suppressed_by.clone();
        let suppressed_until = stored.actions_suppressed_until_secs;
        let withheld = stored.actions_withheld;
        let child_states = self.child_alarm_states(&stored.child_names);
        // The pairs the forced-state snapshot is compared against: a state a caller forced
        // stands until a CHILD's state changes, which is what this comparison detects.
        let children: Vec<(String, Option<String>)> = child_states
            .iter()
            .map(|child| (child.name.clone(), child.state_value.clone()))
            .collect();
        // "It returns to its actual state only once any of its children alarms change state."
        // (`SetAlarmState`) — until then the state a caller forced stands.
        let forced_stands = forced.is_some_and(|snapshot| snapshot == children);
        let evaluated = rule.evaluate(&|reference| self.alarm_state_value(reference));
        let state = if evaluated {
            ALARM_STATE_ALARM
        } else {
            ALARM_STATE_OK
        };
        // The state this evaluation leaves the alarm in: the state it evaluated, unless a state
        // a caller forced still stands, in which case the alarm stays where that force put it.
        let settled = if forced_stands {
            previous.state_value.as_deref()
        } else {
            Some(state)
        };
        // "Actions will be suppressed if the suppressor alarm is in the ALARM state."
        // (`CompositeAlarm$ActionsSuppressor`), for the wait period that follows this alarm
        // going into `ALARM` before the suppressor does, and for the extension period that
        // follows the suppressor leaving that state ([`composite_suppression`]) — a reported
        // fact about the suppressor's current state, this alarm's own state and the clock, so
        // it is derived here rather than stored by the writer.
        let suppressor_state = suppressor
            .as_deref()
            .and_then(|suppressor| self.alarm_state_value(suppressor));
        let suppressed = composite_suppression::suppression(
            &composite_suppression::SuppressorStanding {
                suppressor: suppressor.as_deref(),
                suppressor_state: suppressor_state.as_deref(),
                wait_period_secs: wait_period,
                extension_period_secs: extension_period,
                reported: reported_suppression.as_deref(),
                until_secs: suppressed_until,
                in_alarm: settled == Some(ALARM_STATE_ALARM),
                entering_alarm: !forced_stands
                    && state == ALARM_STATE_ALARM
                    && previous.state_value.as_deref() != Some(ALARM_STATE_ALARM),
            },
            now_secs,
        );
        let summary = format!(
            "Alarm updated from {} to {state}",
            previous
                .state_value
                .as_deref()
                .unwrap_or(ALARM_STATE_INSUFFICIENT_DATA)
        );

        let Some(stored) = self.composite_alarms.get_mut(name) else {
            return false;
        };
        stored.alarm.actions_suppressed_by = suppressed
            .suppressed_by
            .map(ToString::to_string);
        stored.alarm.actions_suppressed_reason = suppressed.reason;
        stored.actions_suppressed_until_secs = suppressed.until_secs;
        // "After this time, the composite alarm performs its actions."
        // (`CompositeAlarm$ActionsSuppressorWaitPeriod` /
        // `CompositeAlarm$ActionsSuppressorExtensionPeriod`) — a suppression that has ENDED
        // releases the actions the transition it withheld never executed. The alarm does not
        // MOVE: it is in the state it was already in, so no history item records a transition
        // and no state-change event announces one; what is released is the execution.
        let released = if withheld && suppressed.suppressed_by.is_none() {
            stored.actions_withheld = false;
            alarm_actions::AlarmNotification::of_composite_transition(
                &stored.alarm,
                &child_states,
                &AlarmStateSnapshot::of_composite_alarm(&stored.alarm),
                now_secs,
            )
            .filter(|released| !released.actions.is_empty())
            .map(|released| alarm_actions::AlarmNotification {
                publishes_state_change: false,
                ..released
            })
        } else {
            None
        };
        // The transition this evaluation is about to write has its own actions withheld exactly
        // when a suppression is in force as it happens, and the record of THAT is what the
        // release above is owed by.
        let transitions = !forced_stands && stored.alarm.state_value.as_deref() != Some(state);
        let suppressed_actions = if transitions {
            stored.actions_withheld = suppressed.suppressed_by.is_some();
            suppressed
                .suppressed_by
                .map(|reason| {
                    (
                        reason,
                        alarm_actions::actions_of_state(
                            state,
                            stored.alarm.ok_actions.as_deref(),
                            stored.alarm.alarm_actions.as_deref(),
                            stored.alarm.insufficient_data_actions.as_deref(),
                        )
                        .filter(|_| stored.alarm.actions_enabled.unwrap_or(true))
                        .unwrap_or_default()
                        .to_vec(),
                    )
                })
                .filter(|(_, actions)| !actions.is_empty())
        } else {
            None
        };
        if !forced_stands {
            // The evaluated state has taken over from any state a caller forced.
            stored.forced_child_states = None;
        }
        if let Some(released) = released {
            self.queue_notification(released);
        }
        if !transitions {
            return false;
        }
        let Some(stored) = self.composite_alarms.get_mut(name) else {
            return false;
        };
        stored.alarm.state_value = Some(state.to_string());
        stored.alarm.state_reason = Some(composite_state_reason(
            latest_child_transition(&child_states),
            evaluated,
            state,
        ));
        stored.alarm.state_reason_data = Some(composite_state_reason_data(&child_states));
        stored.alarm.state_updated_timestamp = Some(to_timestamp(now_secs));
        // "The timestamp of the last change to the alarm's StateValue."
        // (`CompositeAlarm$StateTransitionedTimestamp`) — this call changed it.
        stored.alarm.state_transitioned_timestamp = Some(to_timestamp(now_secs));
        let updated = AlarmStateSnapshot::of_composite_alarm(&stored.alarm);
        // The actions this transition fires, queued for the call that materialised it to
        // deliver: "the actions to execute when this alarm transitions to the ALARM state"
        // holds for a composite alarm exactly as it does for a metric alarm.
        let notification = alarm_actions::AlarmNotification::of_composite_transition(
            &stored.alarm,
            &child_states,
            &previous,
            now_secs,
        );
        let item = AlarmHistoryItem {
            alarm_name: Some(name.to_string()),
            alarm_contributor_id: None,
            alarm_type: Some(ALARM_TYPE_COMPOSITE.to_string()),
            timestamp: Some(to_timestamp(now_secs)),
            history_item_type: Some("StateUpdate".to_string()),
            history_summary: Some(summary),
            history_data: Some(state_transition_history_data(&previous, &updated)),
            alarm_contributor_attributes: None,
        };
        self.record_history(name, now_secs, item);
        // This alarm's new state is an input to every composite alarm whose rule reads it, so
        // their evaluations are owed from this instant too.
        self.make_parents_due(name, now_secs);
        if let Some(notification) = notification {
            self.enqueue_notification(notification, now_secs);
        }
        // The account of the actions this transition did NOT execute, recorded beside the
        // transition itself rather than in place of it: the alarm moved, and what the
        // suppression decided is only whether the actions configured for the state it moved to
        // were performed ([`composite_suppression::suppressed_actions_history_item`]).
        if let Some((reason, actions)) = suppressed_actions {
            let item = composite_suppression::suppressed_actions_history_item(
                name, reason, state, &actions, now_secs,
            );
            self.record_history(name, now_secs, item);
        }
        true
    }
}
