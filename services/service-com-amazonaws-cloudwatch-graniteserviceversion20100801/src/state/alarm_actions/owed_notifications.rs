// SPDX-License-Identifier: CC0-1.0

//! The action notifications ONE settle has taken out of the store and NOT delivered yet.
//!
//! A transition's notification is queued once, by the evaluation that moved the alarm
//! ([`crate::state::alarm_evaluation`]), and nothing re-queues it afterwards: the alarm already
//! stands in the state it moved to, so every later evaluation computes that same state and moves
//! it nowhere. The queued record is therefore the ONLY account of an action the alarm owes — which
//! is why a settle must not be able to lose it.
//!
//! A settle that simply removed the records and then published them could: the delivery is a
//! chain of cross-service calls, and the pass making them can end before the last of them is made
//! — the task the alarm clock runs on is aborted when a new clock replaces it
//! ([`super::set_alarm_clock`]), a request's own future is dropped when the client it answers goes
//! away, and a panic in one action unwinds the rest. Each of those leaves the alarm moved, its
//! history silent about any execution, and nothing left in the store to deliver: no subscriber of
//! that topic is ever notified, and no later call can put it right. That is the failure this type
//! exists to make impossible.
//!
//! So the records are HELD here while the pass delivers them, and whatever is still undelivered
//! when the pass ends — however it ends — is handed back to the store as still owed. One record
//! leaves the hold only once every action it carries has been executed and recorded
//! ([`Self::delivered`]), so delivery is at-least-once rather than at-most-once: the same
//! guarantee an asynchronous fan-out can give, and the one that matches AWS, where an alarm action
//! is retried rather than dropped. The state-change EVENT a transition publishes is not retried
//! with it ([`Self::state_changes_published`]): the event is owed by the transition and has
//! already gone out, while what the retry is for is the action list.

use super::state_change_events::AlarmStateChange;
use super::AlarmNotification;
use crate::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State as State;

/// The notifications one settle holds while it delivers them.
pub(super) struct Owed<'store> {
    /// The store the undelivered remainder is handed back to.
    state: &'store ::engine::ServiceState<State>,
    /// What this settle has still to deliver, in the order it took them.
    held: ::std::collections::VecDeque<AlarmNotification>,
    /// Whether the state changes these records carry have already been published, so a record
    /// handed back does not announce its transition a second time.
    state_changes_published: bool,
}

impl<'store> Owed<'store> {
    /// Hold `taken` — the records this settle has just taken out of `state` — while it delivers
    /// them.
    pub(super) fn new(
        state: &'store ::engine::ServiceState<State>,
        taken: ::std::collections::VecDeque<AlarmNotification>,
    ) -> Self {
        Self {
            state,
            held: taken,
            state_changes_published: false,
        }
    }

    /// Whether this settle has nothing to deliver.
    pub(super) fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// The state changes the held records announce: the transitions among them, each as the event
    /// it publishes onto the account's default event bus.
    pub(super) fn state_changes(&self) -> Vec<AlarmStateChange> {
        self.held
            .iter()
            .filter(|notification| notification.publishes_state_change)
            .map(|notification| notification.state_change.clone())
            .collect()
    }

    /// Record that the events [`Self::state_changes`] named have gone out, so a record handed back
    /// to the store carries only the actions it still owes.
    pub(super) fn state_changes_published(&mut self) {
        self.state_changes_published = true;
    }

    /// The record this settle delivers next, or `None` once it has delivered them all.
    ///
    /// The record stays HELD while its actions are executed — a copy is handed over, not the
    /// record itself — so a pass that ends part-way through one leaves that record owed rather
    /// than silently dropped.
    pub(super) fn next_to_deliver(&self) -> Option<AlarmNotification> {
        self.held.front().cloned()
    }

    /// Release the record [`Self::next_to_deliver`] just answered with: every action it carries has
    /// been executed and recorded in its alarm's history, so nothing of it is owed any more.
    pub(super) fn delivered(&mut self) {
        self.held.pop_front();
    }
}

impl ::std::fmt::Debug for Owed<'_> {
    /// The store handle is not a value with a debug form, so what this reports is what a reader
    /// of a log line wants of it: how much is still owed, and whether the events have gone out.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("Owed")
            .field("held", &self.held.len())
            .field("state_changes_published", &self.state_changes_published)
            .finish_non_exhaustive()
    }
}

impl Drop for Owed<'_> {
    fn drop(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let published = self.state_changes_published;
        let mut store = self.state.lock();
        let mut owed = 0_usize;
        for mut notification in self.held.drain(..) {
            // The transition itself has already been announced, so what is handed back is the
            // action list alone.
            notification.publishes_state_change &= !published;
            store.queue_notification(notification);
            owed += 1;
        }
        ::tracing::warn!(
            owed,
            "this alarm settle ended before it had delivered every action notification it took; \
             the rest are owed again, so the next settle delivers them"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Owed;
    use crate::ops::types::MetricAlarm;
    use crate::state::alarm_actions::AlarmNotification;
    use crate::state::{AlarmStateSnapshot, ALARM_STATE_ALARM};

    /// The topic the record below owes its publish to.
    const TOPIC: &str = "arn:aws:sns:us-east-1:000000000000:alarm-notifications";

    /// One transition's record: an alarm in `ALARM` carrying an SNS action, as the evaluation that
    /// moved it queues it.
    fn transition(name: &str) -> AlarmNotification {
        let alarm = MetricAlarm {
            alarm_name: Some(name.to_string()),
            alarm_arn: Some(format!(
                "arn:aws:cloudwatch:us-east-1:000000000000:alarm:{name}"
            )),
            state_value: Some(ALARM_STATE_ALARM.to_string()),
            alarm_actions: Some(vec![TOPIC.to_string()]),
            ..MetricAlarm::default()
        };
        let previous = AlarmStateSnapshot::of_metric_alarm(&MetricAlarm::default());
        AlarmNotification::of_transition(&alarm, &previous, 0.0)
            .expect("an alarm with a name and a state owes a record")
    }

    /// A hold that ends before it has delivered a record hands that record back to the store, so
    /// the next settle delivers it — and hands back only the ACTIONS where the transition has
    /// already been announced.
    #[test]
    fn a_hold_that_ends_before_delivering_hands_the_record_back() {
        let engine = ::engine::InMemoryEngine::new();
        let state = crate::state::service_state(&engine).expect("the store exists");
        {
            let mut held = Owed::new(&state, [transition("undelivered")].into());
            held.state_changes_published();
            // The pass ends here, the way an aborted clock task or a dropped request future ends
            // it: nothing was delivered.
        }
        let again = state.lock().take_pending_notifications(8);
        let names: Vec<&str> = again
            .iter()
            .map(|notification| notification.alarm_name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["undelivered"],
            "a record nothing delivered must be owed again, got {names:?}"
        );
        assert!(
            again
                .front()
                .is_some_and(|notification| !notification.publishes_state_change),
            "the transition was already announced, so only its actions are owed again, got \
             {again:?}"
        );
    }

    /// A hold whose records were all delivered hands nothing back: a delivery made once is not
    /// made again.
    #[test]
    fn a_hold_whose_records_were_delivered_hands_nothing_back() {
        let engine = ::engine::InMemoryEngine::new();
        let state = crate::state::service_state(&engine).expect("the store exists");
        {
            let mut held = Owed::new(&state, [transition("delivered")].into());
            let taken = held.next_to_deliver();
            assert!(
                taken.is_some(),
                "the record a settle holds is the one it delivers next, got {taken:?}"
            );
            held.delivered();
            assert!(
                held.is_empty(),
                "a delivered record leaves the hold, got {held:?}"
            );
        }
        let again = state.lock().take_pending_notifications(8);
        assert!(
            again.is_empty(),
            "a record already delivered must not be owed again, got {again:?}"
        );
    }
}
