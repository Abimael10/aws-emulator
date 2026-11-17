// SPDX-License-Identifier: CC0-1.0

//! The TUMBLING WINDOW a mapping aggregates one shard's records over: which window an
//! invocation belongs to, what that invocation is told about it, what the handler's answer
//! carries into the next invocation of the same window, and the one final invocation each
//! window ends with.
//!
//! "(Kinesis and DynamoDB Streams only) The duration in seconds of a processing window for
//! DynamoDB and Kinesis Streams event sources. A value of 0 seconds indicates no tumbling
//! window" (`TumblingWindowInSeconds`). A mapping that configures one reads, batches and
//! checkpoints its shards exactly as [`super`] states — a window changes none of that — and
//! adds what a handler needs to carry a figure of its own across the invocations of one
//! window:
//!
//! * A WINDOW IS A FIXED, NON-OVERLAPPING SPAN OF THE CLOCK. A window of `d` seconds runs
//!   from a whole multiple of `d` since the epoch to the next one ([`window_at`]), so the
//!   windows of one mapping tile the clock without overlapping and which window an instant
//!   falls in is a fact about that instant rather than about when this poller started.
//! * EVERY INVOCATION SAYS WHICH WINDOW IT IS IN. Beside the records, the event carries the
//!   window's `start` and `end`, the `shardId` the records were read off and the
//!   `eventSourceARN` they were read from, the `state` the window has reached,
//!   `isFinalInvokeForWindow` and `isWindowTerminatedEarly` ([`attach`]) — so a handler
//!   aggregating over a window reads the window it is aggregating into out of the event.
//! * THE STATE IS WHAT THE HANDLER RETURNED. The `state` member of the document one
//!   invocation answered is what the NEXT invocation of that window is handed
//!   ([`carry_state`]), and the empty state is what the first invocation of a window gets,
//!   so the aggregate is the handler's own and nothing here computes it. An answer carrying
//!   no state leaves the window with the empty one. Because the state is the SHARD's and is
//!   handed on in that order, a mapping with a window reads one shard serially
//!   ([`super::in_flight_batches_of`]) rather than having several of its invocations in
//!   flight beside each other.
//! * EACH WINDOW ENDS IN ONE FINAL INVOCATION. Once the clock has passed a window's end the
//!   function is invoked once more for that shard, with no records, the state the window
//!   reached and `isFinalInvokeForWindow` true, which is where a handler writes the window's
//!   result; the window is then over and the records that follow aggregate into the window
//!   the clock is in. A shard that was never invoked for has no window and so owes no final
//!   invocation.
//! * A STATE THAT OUTGROWS ITS BOUND ENDS ITS WINDOW EARLY. AWS publishes the aggregate
//!   state as at most 1 MB per shard and terminates a window whose state outgrows that
//!   early, which the final invocation of such a window reports
//!   (`isWindowTerminatedEarly`); a state past the bound is not carried, so what this poller
//!   holds for one shard is bounded by that bound.
//!
//! The windows belong to the poller's own working state rather than to the mapping record a
//! client reads: every part of one is a fact about the invocations THIS poller has made, as
//! the attempts it has spent on a batch are ([`super::PollerWork`]).

use super::*;

/// How large the state one window carries may be before the window is terminated early:
/// AWS publishes the tumbling-window state as a maximum of 1 MB per shard.
pub(super) const MAX_WINDOW_STATE_BYTES: usize = 1024 * 1024;

/// The window one shard of a mapping is aggregating into.
#[derive(Debug, Clone)]
pub(super) struct OpenWindow {
    /// When the window began, which is what its invocations report as `window.start`.
    start: SystemTime,
    /// When it ends: the instant from which nothing more aggregates into it and its final
    /// invocation is owed, reported as `window.end`.
    end: SystemTime,
    /// The state the window has reached: what its last invocation returned, and the empty
    /// state before any of them has returned one.
    state: Value,
    /// Whether the state outgrew [`MAX_WINDOW_STATE_BYTES`], which ends the window at once
    /// and is what its final invocation reports as `isWindowTerminatedEarly`.
    terminated_early: bool,
    /// How many attempts have been made at this window's FINAL invocation, so the mapping's
    /// own allowance is spent on it as it is on a batch ([`super::attempts_allowed`]).
    final_attempts: u32,
}

/// The duration of the tumbling window a mapping carrying `tumbling_window_in_seconds`
/// aggregates over, or `None` for a mapping with no window at all.
///
/// "A value of 0 seconds indicates no tumbling window", which is what a mapping carrying no
/// value and one carrying zero both ask for; a negative value is no window either, since a
/// span is not negative.
pub(super) fn tumbling_window(tumbling_window_in_seconds: Option<i32>) -> Option<Duration> {
    let seconds = u64::try_from(tumbling_window_in_seconds?).ok()?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// The window of `duration` that `now` falls in: from the whole multiple of `duration`
/// since the epoch at or before `now`, to the next one.
pub(super) fn window_at(now: SystemTime, duration: Duration) -> (SystemTime, SystemTime) {
    let seconds = duration.as_secs().max(1);
    let since_epoch = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(since_epoch - since_epoch % seconds);
    (start, start + Duration::from_secs(seconds))
}

/// The window `shard` is aggregating into at `now`, opening the window the clock is in when
/// the shard is aggregating into none yet.
///
/// Answers a COPY, so no part of the poller's working state is borrowed while the
/// invocations this window's event is built for are in flight.
pub(super) fn open(
    windows: &mut BTreeMap<String, OpenWindow>,
    shard: &str,
    duration: Duration,
    now: SystemTime,
) -> OpenWindow {
    let (start, end) = window_at(now, duration);
    windows
        .entry(shard.to_string())
        .or_insert_with(|| OpenWindow {
            start,
            end,
            state: json!({}),
            terminated_early: false,
            final_attempts: 0,
        })
        .clone()
}

/// The window of `shard` that is OVER and so owes its final invocation — one whose end the
/// clock has passed, or one whose state outgrew its bound — and `None` while the shard is
/// aggregating into none or into one that is still open.
pub(super) fn due_to_close(
    windows: &BTreeMap<String, OpenWindow>,
    shard: &str,
    now: SystemTime,
) -> Option<OpenWindow> {
    windows
        .get(shard)
        .filter(|window| window.terminated_early || now >= window.end)
        .cloned()
}

/// The window of `shard` is finalised: nothing more is owed for it, and the records that
/// follow aggregate into the window the clock is in.
pub(super) fn closed(windows: &mut BTreeMap<String, OpenWindow>, shard: &str) {
    windows.remove(shard);
}

/// Put on `event` the members an invocation inside a tumbling window carries beside its
/// records: which window it is, where its records came from, the state the window has
/// reached, and whether this is the window's final invocation.
pub(super) fn attach(
    event: &mut Value,
    stream_arn: &str,
    shard: &str,
    window: &OpenWindow,
    is_final: bool,
) {
    let Some(members) = event.as_object_mut() else {
        return;
    };
    members.insert(
        "window".to_string(),
        json!({ "start": instant(window.start), "end": instant(window.end) }),
    );
    members.insert("state".to_string(), window.state.clone());
    members.insert("shardId".to_string(), json!(shard));
    members.insert("eventSourceARN".to_string(), json!(stream_arn));
    members.insert("isFinalInvokeForWindow".to_string(), json!(is_final));
    members.insert(
        "isWindowTerminatedEarly".to_string(),
        json!(window.terminated_early),
    );
}

/// Carry what one invocation of `shard`'s window answered into the next invocation of that
/// window.
///
/// A state LARGER than one window may carry ends that window at once instead
/// ([`MAX_WINDOW_STATE_BYTES`]): it is not carried — the window keeps the last state that was
/// inside the bound, so what this poller holds per shard is bounded by that bound — and the
/// window's final invocation reports that it was terminated early.
pub(super) fn carry_state(
    windows: &mut BTreeMap<String, OpenWindow>,
    shard: &str,
    payload: Option<&[u8]>,
) {
    let Some(window) = windows.get_mut(shard) else {
        return;
    };
    let state = returned_state(payload);
    if outgrows_its_bound(&state) {
        window.terminated_early = true;
        return;
    }
    window.state = state;
}

/// Record one FAILED attempt at the final invocation of `shard`'s window, and answer whether
/// the mapping has now spent on it every attempt it allows — in which case the window is
/// given up on, so the shard aggregates into the window the clock is in rather than waiting
/// on an invocation that keeps failing.
///
/// A mapping carrying the infinite default spends nothing and keeps trying, exactly as it
/// does with a batch its function did not process ([`super::attempts_allowed`]).
pub(super) fn final_attempt_failed(
    windows: &mut BTreeMap<String, OpenWindow>,
    shard: &str,
    attempts_allowed: Option<u32>,
) -> bool {
    let Some(window) = windows.get_mut(shard) else {
        return false;
    };
    window.final_attempts = window.final_attempts.saturating_add(1);
    let made = window.final_attempts;
    let spent = attempts_allowed.is_some_and(|allowed| made >= allowed);
    if spent {
        windows.remove(shard);
    }
    spent
}

/// The state one invocation answered with: the `state` member of the JSON document it
/// returned, and the EMPTY state for an answer that carried none — a document without that
/// member, a payload that is no JSON document at all, and an invocation that answered
/// nothing.
fn returned_state(payload: Option<&[u8]>) -> Value {
    payload
        .and_then(|payload| ::serde_json::from_slice::<Value>(payload).ok())
        .as_ref()
        .and_then(|document| document.get("state"))
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// Whether `state` is larger than the state of one window may be
/// ([`MAX_WINDOW_STATE_BYTES`]), measured as the document it is carried to the handler as.
fn outgrows_its_bound(state: &Value) -> bool {
    state.to_string().len() > MAX_WINDOW_STATE_BYTES
}

/// `at` as the instant a window's bounds are reported at: a whole second, rendered as the
/// ISO-8601 instant in UTC that every window bound AWS publishes is rendered as. A window
/// bound is always a whole second, since a window spans a whole number of them.
fn instant(at: SystemTime) -> String {
    ::aws_smithy_types::DateTime::from_secs(::aws_smithy_types::DateTime::from(at).secs())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, SystemTime};

    use serde_json::{json, Value};

    use super::{
        attach, carry_state, due_to_close, final_attempt_failed, instant, open, returned_state,
        tumbling_window, window_at, MAX_WINDOW_STATE_BYTES,
    };

    /// RULE: a mapping carrying no window, zero or a negative duration aggregates over no
    /// window; every other value is that many seconds.
    ///
    /// The rows differ along what the rule generalises over: the member absent, the value
    /// the model documents as no window, both bounds of the range above it, and a value
    /// inside it.
    #[test]
    fn only_a_positive_duration_is_a_tumbling_window() {
        // (case, the member's value, the window it configures)
        let rows: Vec<(&str, Option<i32>, Option<Duration>)> = vec![
            ("the-member-absent", None, None),
            ("zero-is-no-window", Some(0), None),
            ("one-second", Some(1), Some(Duration::from_secs(1))),
            ("thirty-seconds", Some(30), Some(Duration::from_secs(30))),
            (
                "the-largest-the-range-admits",
                Some(900),
                Some(Duration::from_secs(900)),
            ),
        ];
        for (case, configured, expected) in rows {
            let window = tumbling_window(configured);
            assert_eq!(
                window, expected,
                "case {case:?}: expected {expected:?} for {configured:?}, got {window:?}"
            );
        }
    }

    /// RULE: the window of `d` seconds an instant falls in runs from the whole multiple of
    /// `d` since the epoch at or before it to the next one.
    ///
    /// The rows differ along what the rule generalises over: the window's duration, and
    /// where inside its window the instant falls (its first instant, inside it, its last
    /// second).
    #[test]
    fn the_windows_of_one_duration_tile_the_clock() {
        // (case, the duration in seconds, the instant in whole seconds since the epoch,
        //  the window it falls in, in those same seconds)
        let rows: Vec<(&str, u64, u64, (u64, u64))> = vec![
            ("a-five-second-window-at-its-start", 5, 1_800_000_000, (1_800_000_000, 1_800_000_005)),
            ("a-five-second-window-one-in", 5, 1_800_000_001, (1_800_000_000, 1_800_000_005)),
            ("a-five-second-window-at-its-last-second", 5, 1_800_000_004, (1_800_000_000, 1_800_000_005)),
            ("the-next-five-second-window", 5, 1_800_000_005, (1_800_000_005, 1_800_000_010)),
            ("a-one-second-window", 1, 1_800_000_003, (1_800_000_003, 1_800_000_004)),
            ("the-largest-window", 900, 1_800_000_450, (1_800_000_000, 1_800_000_900)),
        ];
        for (case, duration, at, (start, end)) in rows {
            let (opened, closed) = window_at(
                SystemTime::UNIX_EPOCH + Duration::from_secs(at),
                Duration::from_secs(duration),
            );
            let expected = (
                SystemTime::UNIX_EPOCH + Duration::from_secs(start),
                SystemTime::UNIX_EPOCH + Duration::from_secs(end),
            );
            assert_eq!(
                (opened, closed),
                expected,
                "case {case:?}: expected the window {start}..{end}, got {:?}..{:?}",
                instant(opened),
                instant(closed)
            );
        }
    }

    /// RULE: an invocation inside a window carries that window's bounds, the shard and
    /// stream its records came from, the state the window has reached and whether it is the
    /// window's final invocation.
    #[test]
    fn an_invocation_carries_the_window_it_aggregates_into() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_003);
        let mut windows = BTreeMap::new();
        let window = open(&mut windows, "shardId-1", Duration::from_secs(5), now);
        // (case, whether this is the final invocation)
        for (case, is_final) in [("an-open-window", false), ("the-final-invocation", true)] {
            let mut event = json!({ "Records": [] });
            attach(&mut event, "arn:aws:dynamodb:::table/t/stream/s", "shardId-1", &window, is_final);
            assert_eq!(
                event["window"],
                json!({ "start": "2027-01-15T08:00:00Z", "end": "2027-01-15T08:00:05Z" }),
                "case {case:?}: expected the window's own bounds, got {event}"
            );
            assert_eq!(
                event["state"],
                json!({}),
                "case {case:?}: expected the empty state of a window nothing has returned \
                 one for, got {event}"
            );
            assert_eq!(
                event["shardId"],
                json!("shardId-1"),
                "case {case:?}: expected the shard the records came off, got {event}"
            );
            assert_eq!(
                event["eventSourceARN"],
                json!("arn:aws:dynamodb:::table/t/stream/s"),
                "case {case:?}: expected the stream the records were read from, got {event}"
            );
            assert_eq!(
                event["isFinalInvokeForWindow"],
                json!(is_final),
                "case {case:?}: expected isFinalInvokeForWindow {is_final}, got {event}"
            );
            assert_eq!(
                event["isWindowTerminatedEarly"],
                json!(false),
                "case {case:?}: a window whose state is inside its bound is not terminated \
                 early, got {event}"
            );
        }
    }

    /// RULE: the `state` one invocation returned is what the next invocation of that window
    /// is handed, and an answer carrying none leaves the empty state.
    ///
    /// The rows differ along what the rule generalises over: the shape of the state
    /// returned, an answer with no state member, an answer that is no document, and no
    /// answer at all.
    #[test]
    fn the_state_the_handler_returned_is_handed_to_the_next_invocation() {
        // (case, the payload the invocation answered, the state the next one is handed)
        let rows: Vec<(&str, Option<&[u8]>, Value)> = vec![
            ("an-aggregate", Some(br#"{"state": {"n": 2}}"#), json!({ "n": 2 })),
            ("an-empty-state", Some(br#"{"state": {}}"#), json!({})),
            ("a-nested-state", Some(br#"{"state": {"by": {"k": [1, 2]}}}"#), json!({ "by": { "k": [1, 2] } })),
            ("no-state-member", Some(br#"{"written": 3}"#), json!({})),
            ("no-document-at-all", Some(b"not json"), json!({})),
            ("no-answer", None, json!({})),
        ];
        for (case, payload, expected) in rows {
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let mut windows = BTreeMap::new();
            open(&mut windows, "shardId-1", Duration::from_secs(5), now);
            carry_state(&mut windows, "shardId-1", payload);
            let mut event = json!({ "Records": [] });
            let window = open(&mut windows, "shardId-1", Duration::from_secs(5), now);
            attach(&mut event, "stream", "shardId-1", &window, false);
            assert_eq!(
                event["state"], expected,
                "case {case:?}: expected the state {expected} carried on, got {event}"
            );
        }
    }

    /// RULE: a window is finalised once the clock has passed its end, or at once when the
    /// state it carries outgrows its bound; a window still open and a shard with no window
    /// owe no final invocation.
    #[test]
    fn a_window_is_due_to_close_when_it_is_over() {
        let opened = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let five = Duration::from_secs(5);
        let mut windows = BTreeMap::new();
        assert!(
            due_to_close(&windows, "shardId-1", opened).is_none(),
            "case \"no-window\": a shard aggregating into no window owes no final invocation"
        );
        open(&mut windows, "shardId-1", five, opened);
        // (case, the instant the window is read at, whether it is due to close)
        let rows: Vec<(&str, SystemTime, bool)> = vec![
            ("at-its-start", opened, false),
            ("inside-it", opened + Duration::from_secs(4), false),
            ("at-its-end", opened + five, true),
            ("past-its-end", opened + Duration::from_secs(600), true),
        ];
        for (case, now, expected) in rows {
            let due = due_to_close(&windows, "shardId-1", now).is_some();
            assert_eq!(
                due, expected,
                "case {case:?}: expected due_to_close {expected}, got {due}"
            );
        }
        // A state past the bound ends the window at once, whatever the clock says, and the
        // final invocation of such a window reports it.
        let oversized = format!(r#"{{"state": {{"blob": "{}"}}}}"#, "x".repeat(MAX_WINDOW_STATE_BYTES));
        carry_state(&mut windows, "shardId-1", Some(oversized.as_bytes()));
        let closing = due_to_close(&windows, "shardId-1", opened).unwrap_or_else(|| {
            panic!("case \"a-state-past-its-bound\": the window must be due to close at once")
        });
        let mut event = json!({ "Records": [] });
        attach(&mut event, "stream", "shardId-1", &closing, true);
        assert_eq!(
            event["isWindowTerminatedEarly"],
            json!(true),
            "case \"a-state-past-its-bound\": the final invocation must report the window was \
             terminated early, got {event}"
        );
    }

    /// RULE: the attempts a mapping allows are spent on a window's final invocation as they
    /// are on a batch: a mapping allowing a finite count gives the window up once they are
    /// spent, and one carrying the infinite default keeps trying.
    #[test]
    fn the_final_invocation_is_attempted_as_often_as_the_mapping_allows() {
        // (case, the attempts the mapping allows, how many failures before the window is
        //  given up on, as a count of attempts)
        let rows: Vec<(&str, Option<u32>, Option<u32>)> =
            vec![("one-attempt", Some(1), Some(1)), ("three-attempts", Some(3), Some(3)), ("the-infinite-default", None, None)];
        for (case, allowed, given_up_at) in rows {
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
            let mut windows = BTreeMap::new();
            open(&mut windows, "shardId-1", Duration::from_secs(5), now);
            for attempt in 1..=5u32 {
                let spent = final_attempt_failed(&mut windows, "shardId-1", allowed);
                let expected = given_up_at == Some(attempt);
                assert_eq!(
                    spent, expected,
                    "case {case:?}: at attempt {attempt} expected the window given up \
                     {expected}, got {spent}"
                );
                if spent {
                    assert!(
                        due_to_close(&windows, "shardId-1", now + Duration::from_secs(600))
                            .is_none(),
                        "case {case:?}: a window given up on owes no further invocation"
                    );
                    break;
                }
            }
        }
    }

    /// RULE: a payload that is not a JSON object answers the empty state rather than a
    /// state the handler never returned.
    #[test]
    fn a_payload_that_is_no_document_answers_the_empty_state() {
        // (case, the payload)
        let rows: Vec<(&str, &[u8])> = vec![
            ("an-empty-payload", b""),
            ("a-json-list", b"[1, 2, 3]"),
            ("a-json-string", b"\"done\""),
            ("a-truncated-document", b"{\"state\": {"),
        ];
        for (case, payload) in rows {
            let state = returned_state(Some(payload));
            assert_eq!(
                state,
                json!({}),
                "case {case:?}: expected the empty state, got {state}"
            );
        }
    }
}
