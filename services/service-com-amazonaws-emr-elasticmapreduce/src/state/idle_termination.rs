// SPDX-License-Identifier: CC0-1.0

//! The IDLE TIMER an auto-termination policy is: when a cluster carrying one shuts itself
//! down, and what it reports for having done so.
//!
//! "An auto-termination policy for an Amazon EMR cluster. An auto-termination policy defines
//! the amount of idle time in seconds after which a cluster automatically terminates."
//! (`AutoTerminationPolicy` doc.) "Specifies the amount of idle time in seconds after which
//! the cluster automatically terminates. You can specify a minimum of 60 seconds and a maximum
//! of 604800 seconds (seven days)." (`AutoTerminationPolicy$IdleTimeout` doc.)
//!
//! So the attachment is not a stored structure a read reports back: it is a TIMER, and this
//! module is the half of it that fires. The policy itself is the one `RunJobFlow` or
//! `PutAutoTerminationPolicy` attached and `RemoveAutoTerminationPolicy` takes off
//! ([`ClusterRecord::auto_termination_policy`]), so a cluster whose policy was removed is owed
//! no shutdown and one whose `IdleTimeout` was replaced is owed the new one.
//!
//! The span is the DOCUMENTED one, in seconds, taken from the policy: this is a duration the
//! member above states, so it keeps its value rather than being shortened to something this
//! emulator finds convenient. Nothing sleeps for it — the due instant is derived from what is
//! persisted ([`ClusterRecord::idle_termination_due_at`]) and materialised by the next call
//! that reports the cluster, exactly as every other transition the clock owes is.

use super::*;

/// `ClusterStatus$StateChangeReason.Message` for the shutdown a cluster takes when its
/// auto-termination policy's idle timeout has elapsed.
///
/// CHOSEN, not recorded: the shutdown is the policy acting, not a failure of the cluster and
/// not a `TerminateJobFlows` call, so the message says which of the two it was. It is worded
/// the way this service's other orderly shutdowns are — a short clause naming what brought the
/// cluster down ([`CLUSTER_STEPS_COMPLETED_REASON`]) — and interpolates nothing of the cluster
/// or of the policy.
pub const CLUSTER_IDLE_TIMEOUT_REASON: &str = "Terminated by auto-termination policy";

/// The instant a cluster that has been idle since `idle_since` and carries `policy` is owed
/// its shutdown at, or `None` when it is owed none.
///
/// The whole of the timer's arithmetic: the span is the policy's own `IdleTimeout`, in
/// seconds, counted from the instant the cluster fell idle. A cluster carrying no policy, a
/// policy stating no `IdleTimeout` (which is an attached policy all the same) or a timeout
/// that states no span at all is owed nothing, and neither is one that has not begun to be
/// idle.
///
/// The addition is CHECKED: the member is a `Long`, so a span no instant can be that far past
/// is a span this service answers no shutdown for rather than one it faults on.
#[must_use]
pub fn idle_termination_due(
    policy: Option<&AutoTerminationPolicy>,
    idle_since: Option<SystemTime>,
) -> Option<SystemTime> {
    let idle_timeout = u64::try_from(policy?.idle_timeout?).ok()?;
    if idle_timeout == 0 {
        return None;
    }
    idle_since?.checked_add(Duration::from_secs(idle_timeout))
}

impl ClusterRecord {
    /// The instant this cluster has had nothing left to do since, or `None` when it has work
    /// in hand or is not up to take any.
    ///
    /// Idle means the cluster is running nothing of its own: a step still to run or still
    /// running is work it is busy with, so the instant is the end of the last step of its
    /// queue, or the instant it became ready to run steps when it has run none. A cluster that
    /// is not ready yet has not begun to be idle, and one that is already shutting down or
    /// gone is not idle but finished.
    pub(crate) fn idle_since(&self) -> Option<SystemTime> {
        if !cluster_state::ACTIVE.contains(&self.status.state.as_str()) {
            return None;
        }
        let mut idle_since = self.status.ready_date_time?;
        for step in &self.steps {
            // A step that has not ended is work in hand: the cluster is not idle at all.
            if !step.is_terminal() {
                return None;
            }
            if let Some(ended) = step.end_date_time {
                idle_since = idle_since.max(ended);
            }
        }
        Some(idle_since)
    }

    /// The instant the shutdown this cluster's auto-termination policy owes comes due, or
    /// `None` for a cluster that is owed none.
    ///
    /// The policy's own timer ([`idle_termination_due`]) over the instant this cluster fell
    /// idle ([`Self::idle_since`]). Both are functions of what is persisted, so the instant is
    /// the same however often the cluster is read.
    pub(crate) fn idle_termination_due_at(&self) -> Option<SystemTime> {
        idle_termination_due(self.auto_termination_policy.as_ref(), self.idle_since())
    }

    /// Materialise the shutdown [`Self::idle_termination_due_at`] says this cluster is owed,
    /// once that instant has passed.
    ///
    /// The shutdown is an orderly one — the policy the caller attached, acting as it says it
    /// will — so it carries the modelled code for a shutdown that was asked for
    /// (`ClusterStateChangeReasonCode`) and ends the cluster `TERMINATED` rather than
    /// `TERMINATED_WITH_ERRORS`. Termination protection "prevent[s] the Amazon EC2 instances
    /// from being terminated by an API call or user intervention"
    /// (`Cluster$TerminationProtected` doc), so a protected cluster keeps its capacity here as
    /// it does against `TerminateJobFlows`: the timer stays armed and fires once the lock is
    /// cleared.
    pub(crate) fn stop_once_idle_timeout_elapsed(&mut self, now: SystemTime) {
        let Some(due) = self.idle_termination_due_at() else {
            return;
        };
        // The shutdown itself takes a span, as every other shutdown of this service does, and
        // it is owed from the instant the timer fired rather than from when somebody looked.
        // Checked, for the reason [`idle_termination_due`] gives.
        let Some(terminated_at) = due.checked_add(TRANSITION_SPAN) else {
            return;
        };
        if now < due || self.termination_protection_holds() {
            return;
        }
        self.status.state = cluster_state::TERMINATING.to_string();
        self.status.state_change_reason = Some(StateChangeReasonRecord {
            code: cluster_state_change_reason_code::USER_REQUEST.to_string(),
            message: Some(CLUSTER_IDLE_TIMEOUT_REASON.to_string()),
        });
        self.status.bootstrapping_due_at = None;
        self.status.ready_due_at = None;
        self.status.terminated_due_at = Some(terminated_at);
        for step in &mut self.steps {
            step.stop_with_cluster(terminated_at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window `AutoTerminationPolicy$IdleTimeout` documents: "a minimum of 60 seconds and
    /// a maximum of 604800 seconds (seven days)".
    const MINIMUM_IDLE_TIMEOUT: i64 = 60;
    /// See [`MINIMUM_IDLE_TIMEOUT`].
    const MAXIMUM_IDLE_TIMEOUT: i64 = 604_800;

    /// A policy stating `idle_timeout`.
    fn policy(idle_timeout: Option<i64>) -> AutoTerminationPolicy {
        AutoTerminationPolicy { idle_timeout }
    }

    /// THE RULE: the shutdown an auto-termination policy owes comes due exactly its own
    /// `IdleTimeout` in seconds after the instant the cluster fell idle — the DOCUMENTED
    /// duration, not one of this emulator's choosing.
    ///
    /// The rows differ along what the rule generalises over: whether a policy is attached at
    /// all, whether an attached one states a timeout, and the timeout's value — each published
    /// bound, a value between them and one past each bound (a value the attachment itself
    /// refuses, which still must not arm a timer this service invented).
    #[test]
    fn the_shutdown_is_due_one_idle_timeout_after_the_cluster_fell_idle() {
        let idle_since = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let cases: [(&str, Option<AutoTerminationPolicy>, Option<u64>); 8] = [
            ("no policy at all", None, None),
            ("an attached policy stating no timeout", Some(policy(None)), None),
            (
                "the published minimum",
                Some(policy(Some(MINIMUM_IDLE_TIMEOUT))),
                Some(60),
            ),
            ("a timeout between the bounds", Some(policy(Some(3_600))), Some(3_600)),
            (
                "the published maximum",
                Some(policy(Some(MAXIMUM_IDLE_TIMEOUT))),
                Some(604_800),
            ),
            (
                "one second past the maximum",
                Some(policy(Some(MAXIMUM_IDLE_TIMEOUT + 1))),
                Some(604_801),
            ),
            (
                "one second short of the minimum",
                Some(policy(Some(MINIMUM_IDLE_TIMEOUT - 1))),
                Some(59),
            ),
            ("a timeout of no seconds at all", Some(policy(Some(0))), None),
        ];
        for (case, attached, expected) in &cases {
            let due = idle_termination_due(attached.as_ref(), Some(idle_since));
            let expected = expected.map(|seconds| idle_since + Duration::from_secs(seconds));
            assert_eq!(
                due, expected,
                "case {case:?}: expected {expected:?}, got {due:?}"
            );
        }

        // A span no instant can be that far past is a span this service answers no shutdown
        // for, and a negative one names no span at all: the member is a `Long`, so the
        // arithmetic above is checked rather than faulted.
        for idle_timeout in [i64::MAX, i64::MIN, -1] {
            let due = idle_termination_due(Some(&policy(Some(idle_timeout))), Some(idle_since));
            assert_eq!(
                due, None,
                "case {idle_timeout:?}: expected no shutdown, got {due:?}"
            );
        }
        // One that is absurd but still an instant this clock holds answers that instant,
        // which is past the one the cluster fell idle at.
        let far = i64::MAX / 4;
        let due = idle_termination_due(Some(&policy(Some(far))), Some(idle_since));
        assert!(
            due.is_none_or(|due| due > idle_since),
            "case {far:?}: expected an instant past {idle_since:?}, got {due:?}"
        );
    }

    /// A cluster that has not begun to be idle is owed no shutdown, whatever its policy says:
    /// there is no instant for the timer to run from.
    #[test]
    fn a_cluster_that_is_not_idle_is_owed_no_shutdown() {
        for idle_timeout in [MINIMUM_IDLE_TIMEOUT, 3_600, MAXIMUM_IDLE_TIMEOUT] {
            let due = idle_termination_due(Some(&policy(Some(idle_timeout))), None);
            assert_eq!(
                due, None,
                "case {idle_timeout:?}: expected no shutdown, got {due:?}"
            );
        }
    }

    /// The shutdown the timer takes is an ORDERLY one: the modelled code for a shutdown that
    /// was asked for, which ends the cluster `TERMINATED` and not `TERMINATED_WITH_ERRORS`.
    #[test]
    fn the_idle_shutdown_is_an_orderly_one() {
        let reason = StateChangeReasonRecord {
            code: cluster_state_change_reason_code::USER_REQUEST.to_string(),
            message: Some(CLUSTER_IDLE_TIMEOUT_REASON.to_string()),
        };
        let terminal = terminal_cluster_state(Some(&reason));
        assert_eq!(
            terminal,
            cluster_state::TERMINATED,
            "case {:?}: expected an orderly terminal state, got {terminal:?}",
            CLUSTER_IDLE_TIMEOUT_REASON
        );
    }
}
