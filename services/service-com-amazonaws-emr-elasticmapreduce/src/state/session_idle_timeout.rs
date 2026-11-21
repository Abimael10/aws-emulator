// SPDX-License-Identifier: CC0-1.0

//! The IDLE TIMER a session's own timeout is: when a session that has been left alone ends
//! itself, and what it reports for having done so. One concern of this service's state.
//!
//! "The idle timeout, in minutes. If the session is idle for this duration, Amazon EMR
//! automatically terminates it." (`StartSessionInput$SessionIdleTimeoutInMinutes` doc,
//! mirrored on `Session$SessionIdleTimeoutInMinutes`.) So the member is not a value a read
//! merely echoes back: it is a TIMER, and this module is the half of it that fires. The
//! instant it counts from is the one `Session$IdleSince` reports — "the date and time that
//! the session last entered the `IDLE` state" — which the session's own walk records
//! ([`SESSION_START_PROGRESSION`]).
//!
//! The span is the DOCUMENTED one, in minutes, taken from the session: this is a duration
//! the member above states, so it keeps its value rather than being shortened to something
//! this emulator finds convenient. Nothing sleeps for it — the due instant is derived from
//! what is persisted ([`SessionRecord::idle_termination_due_at`]) and materialised by the
//! next call that reports the session, exactly as every other transition the clock owes is.
//!
//! A session started WITHOUT the member asked for no such timer and is owed no end: it stays
//! up until the caller terminates it or its cluster goes, which is the cluster's own idle
//! timer ([`super::idle_termination`]) read one level down.

use super::*;

/// How many seconds one minute of the timeout is, since the member states its span in
/// minutes and this clock counts in seconds.
const SECONDS_PER_MINUTE: u64 = 60;

/// `Session$StateChangeReason` — "a human-readable message describing the most recent state
/// change" — for the end a session takes because its idle timeout elapsed.
///
/// CHOSEN, not recorded: the end is the session's own timeout acting, not a failure of the
/// session and not a `TerminateSession` call, so the message says which of the three it was.
/// It is worded the way this service's other orderly self-shutdowns are — a short clause
/// naming what brought the resource down ([`CLUSTER_IDLE_TIMEOUT_REASON`]) — and
/// interpolates nothing of the session or of the timeout.
pub const SESSION_IDLE_TIMEOUT_REASON: &str = "Terminated for exceeding the session idle timeout";

/// The instant a session that has been idle since `idle_since` and was started with
/// `session_idle_timeout_in_minutes` is owed its end at, or `None` when it is owed none.
///
/// The whole of the timer's arithmetic: the span is the session's own idle timeout, in
/// minutes, counted from the instant it fell idle. A session that named no timeout is owed
/// nothing, and neither is one that named no span at all or that has not begun to be idle.
///
/// The arithmetic is CHECKED: the member is a `Long`, so a span no instant can be that far
/// past is a span this service answers no end for rather than one it faults on.
#[must_use]
pub fn session_idle_termination_due(
    session_idle_timeout_in_minutes: Option<i64>,
    idle_since: Option<SystemTime>,
) -> Option<SystemTime> {
    let minutes = u64::try_from(session_idle_timeout_in_minutes?).ok()?;
    let seconds = minutes.checked_mul(SECONDS_PER_MINUTE)?;
    if seconds == 0 {
        return None;
    }
    idle_since?.checked_add(Duration::from_secs(seconds))
}

impl SessionRecord {
    /// The instant the end this session's idle timeout owes comes due, or `None` for a
    /// session that is owed none.
    ///
    /// The session's own timer ([`session_idle_termination_due`]) over the instant it fell
    /// idle. Both are functions of what is persisted, so the instant is the same however
    /// often the session is read. A session that is not idle — one still coming up, one
    /// already on its way out and one that has ended — is owed nothing: only a session the
    /// clock has walked to `IDLE` has been idle since an instant at all.
    pub(crate) fn idle_termination_due_at(&self) -> Option<SystemTime> {
        if self.session.state != session_state::IDLE {
            return None;
        }
        session_idle_termination_due(
            self.session.session_idle_timeout_in_minutes,
            self.idle_since,
        )
    }

    /// Materialise the end [`Self::idle_termination_due_at`] says this session is owed, once
    /// that instant has passed, and answer whether it was armed.
    ///
    /// The end is an orderly one, so it is the same end a caller asking for it takes — "the
    /// session enters the TERMINATING state and then transitions to TERMINATED"
    /// (`TerminateSession` doc, Smithy model), from the instant the timer fired rather than
    /// from when somebody looked ([`SessionRecord::terminate_at`]) — and it records why the
    /// session went down ([`SESSION_IDLE_TIMEOUT_REASON`]), which is what tells this end
    /// apart from one a caller asked for.
    pub(crate) fn stop_once_idle_timeout_elapsed(&mut self, now: SystemTime) -> bool {
        let Some(due) = self.idle_termination_due_at() else {
            return false;
        };
        if now < due {
            return false;
        }
        self.terminate_at(due);
        self.session.state_change_reason = Some(SESSION_IDLE_TIMEOUT_REASON.to_string());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session's idle timeout is a `Long` with no range of its own, so the rows below span
    /// what a request can state rather than a published window: the smallest span that is a
    /// span at all, one a client would really ask for, and the spellings that state none.
    const ONE_MINUTE: i64 = 1;

    /// THE RULE: the end a session's idle timeout owes comes due exactly that many minutes
    /// after the instant the session fell idle — the DOCUMENTED duration, in the unit the
    /// member states it in, not one of this emulator's choosing.
    ///
    /// The rows differ along what the rule generalises over: whether a timeout was asked for
    /// at all, and its value — the smallest span, spans a client asks for, a span of no
    /// minutes and a negative one.
    #[test]
    fn the_end_is_due_one_idle_timeout_after_the_session_fell_idle() {
        let idle_since = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let cases: [(&str, Option<i64>, Option<u64>); 7] = [
            ("no timeout at all", None, None),
            ("the smallest span there is", Some(ONE_MINUTE), Some(60)),
            ("a quarter of an hour", Some(15), Some(900)),
            ("half an hour", Some(30), Some(1_800)),
            ("a day", Some(24 * 60), Some(86_400)),
            ("a timeout of no minutes at all", Some(0), None),
            ("a negative timeout", Some(-1), None),
        ];
        for (case, timeout, expected) in &cases {
            let due = session_idle_termination_due(*timeout, Some(idle_since));
            let expected = expected.map(|seconds| idle_since + Duration::from_secs(seconds));
            assert_eq!(
                due, expected,
                "case {case:?}: expected {expected:?}, got {due:?}"
            );
        }

        // A span no instant can be that far past is a span this service answers no end for:
        // the member is a `Long`, so the arithmetic above is checked rather than faulted.
        for timeout in [i64::MAX, i64::MIN] {
            let due = session_idle_termination_due(Some(timeout), Some(idle_since));
            assert_eq!(due, None, "case {timeout:?}: expected no end, got {due:?}");
        }
    }

    /// A session that has not begun to be idle is owed no end, whatever its timeout says:
    /// there is no instant for the timer to run from.
    #[test]
    fn a_session_that_is_not_idle_is_owed_no_end() {
        for timeout in [ONE_MINUTE, 15, 24 * 60] {
            let due = session_idle_termination_due(Some(timeout), None);
            assert_eq!(due, None, "case {timeout:?}: expected no end, got {due:?}");
        }
    }
}
