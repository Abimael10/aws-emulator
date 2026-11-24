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
//! * AN INVOCATION CARRIES THE RECORDS OF THE WINDOW IT REPORTS. The window a record belongs
//!   to is the one containing the instant its source stamped it as created at, so the records
//!   at the head of what a pass read that fall in ONE window are what that invocation is
//!   handed ([`of_one_window`]) and the window it reports is theirs ([`window_of`]) — never
//!   the window this poller's own clock happened to be in when it formed the batch, which for
//!   a record read shortly after a boundary is the window AFTER the one the record belongs to.
//!   A handler aggregating per window therefore counts exactly the records of the window it
//!   is told it is in, and the records behind a boundary are read again for their own window.
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
//! * EACH WINDOW ENDS IN ONE FINAL INVOCATION. Once the clock has passed a window's end, and
//!   the records that window is still owed have been handed over ([`awaits`]), the function is
//!   invoked once more for that shard, with no records, the state the window reached and
//!   `isFinalInvokeForWindow` true, which is where a handler writes the window's result; the
//!   window is then over and the records that follow aggregate into the window they belong to.
//!   A shard that was never invoked for has no window and so owes no final invocation.
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

/// The instant the window one record belongs to is decided from: the instant its SOURCE
/// stamped the record as created at, which is the very instant the record carries to the
/// handler ([`super::ShardRecord::arrived_at`]).
///
/// `None` for a record its source published no instant for, and for one stamped at an instant
/// no `SystemTime` holds: there is then no creation instant to place the record by.
pub(super) fn created_at(record: &super::ShardRecord) -> Option<SystemTime> {
    SystemTime::try_from(record.arrived_at?).ok()
}

/// The instant the window the records of ONE invocation aggregate into is decided from: the
/// instant the FIRST of them was created at, and `read_at` for a hand-over whose first record
/// carries no creation instant at all ([`created_at`]) — a record with no instant has none to
/// be contained by, so it aggregates into the window the clock is in.
pub(super) fn aggregating_at(records: &[super::ShardRecord], read_at: SystemTime) -> SystemTime {
    records.first().and_then(created_at).unwrap_or(read_at)
}

/// The window of `duration` the records of ONE invocation belong to: the window holding the
/// instant that invocation aggregates into ([`aggregating_at`]).
pub(super) fn window_of(
    records: &[super::ShardRecord],
    duration: Duration,
    read_at: SystemTime,
) -> (SystemTime, SystemTime) {
    window_at(aggregating_at(records, read_at), duration)
}

/// The records at the HEAD of `records` that belong to ONE window of `duration`: the window
/// the first of them falls in, and every record behind it created inside that same window.
///
/// Every record an invocation carries belongs to the window that invocation reports, so a
/// record created in a LATER window is left for the invocation of the window it belongs to —
/// the records are read in the order the shard holds them, so what is left behind is a suffix
/// of what was read and nothing is read past it. A record its source published no instant for
/// ([`created_at`]) belongs to whichever window the records it was read with are in, since
/// there is no instant of it to place it by.
pub(super) fn of_one_window(
    records: &[super::ShardRecord],
    duration: Duration,
    read_at: SystemTime,
) -> &[super::ShardRecord] {
    let window = window_of(records, duration, read_at);
    let held = records
        .iter()
        .take_while(|record| {
            created_at(record).is_none_or(|at| window_at(at, duration) == window)
        })
        .count();
    records.get(..held).unwrap_or(records)
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

/// The window `shard` is aggregating into for records created at `at`, opening that window
/// when the shard is aggregating into none yet.
///
/// The window a hand-over reports must CONTAIN the records it carries, so the window the
/// shard already holds is answered only while `at` falls inside it. A shard whose records have
/// moved into another window is finalised out of the one it held before it aggregates into
/// theirs ([`over`]), so a held window is replaced here only where that did not happen —
/// which a source stamping its records out of order can produce — and the records are then
/// still reported under the window they belong to rather than under one they are outside of.
///
/// Answers a COPY, so no part of the poller's working state is borrowed while the
/// invocations this window's event is built for are in flight.
pub(super) fn open(
    windows: &mut BTreeMap<String, OpenWindow>,
    shard: &str,
    duration: Duration,
    at: SystemTime,
) -> OpenWindow {
    let (start, end) = window_at(at, duration);
    let held = windows
        .entry(shard.to_string())
        .or_insert_with(|| opened(start, end));
    if held.start != start || held.end != end {
        *held = opened(start, end);
    }
    held.clone()
}

/// The window from `start` to `end` as it stands before any invocation of it: the empty
/// state, inside its bound, with none of the mapping's attempts spent on its final
/// invocation.
fn opened(start: SystemTime, end: SystemTime) -> OpenWindow {
    OpenWindow {
        start,
        end,
        state: json!({}),
        terminated_early: false,
        final_attempts: 0,
    }
}

/// Whether the final invocation of `window` is still owed RECORDS: `waiting` is the window
/// the records this shard holds belong to, and while that is this window itself the records
/// of it have not all been handed over yet, so it is not finalised.
///
/// A window whose state outgrew its bound is over whatever is still waiting for it: it was
/// terminated early, which is exactly what its final invocation reports.
fn awaits(window: &OpenWindow, waiting: Option<(SystemTime, SystemTime)>) -> bool {
    !window.terminated_early
        && waiting.is_some_and(|(start, end)| start == window.start && end == window.end)
}

/// The window of `shard` that is OVER and so owes its final invocation, given `waiting` — the
/// window the records this shard holds belong to, and `None` while it holds none to hand over.
///
/// A window is over once nothing more aggregates into it: the clock has passed its end, its
/// state outgrew its bound, or the records this shard now holds belong to ANOTHER window, which
/// says the same thing about a window whose end the clock is yet to pass. A window whose own
/// records are still waiting ([`awaits`]) is over in none of those senses, and a shard
/// aggregating into no window owes nothing.
pub(super) fn over(
    windows: &BTreeMap<String, OpenWindow>,
    shard: &str,
    waiting: Option<(SystemTime, SystemTime)>,
    now: SystemTime,
) -> Option<OpenWindow> {
    let window = windows.get(shard)?;
    if awaits(window, waiting) {
        return None;
    }
    // Records of ANOTHER window waiting on the shard say the same about this window as the
    // clock passing its end does: nothing more aggregates into it.
    due_to_close(windows, shard, now).or_else(|| waiting.is_some().then(|| window.clone()))
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
        attach, carry_state, created_at, due_to_close, final_attempt_failed, instant,
        of_one_window, open, over, returned_state, tumbling_window, window_at, window_of,
        MAX_WINDOW_STATE_BYTES,
    };
    use crate::state::event_source_poller::ShardRecord;

    /// One record of a shard its source stamped as CREATED at `created`, in whole epoch
    /// seconds, named by its sequence number; a record its source published no instant for
    /// carries none.
    fn record(sequence_number: &str, created: Option<u64>) -> ShardRecord {
        let stamped = created.map(|seconds| {
            ::aws_smithy_types::DateTime::from_secs(i64::try_from(seconds).unwrap_or_default())
        });
        ShardRecord {
            sequence_number: sequence_number.to_string(),
            arrived_at: stamped,
            written_at: stamped,
            envelope: json!({}),
        }
    }

    /// `seconds` since the epoch as an instant.
    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    /// RULE: the window an invocation reports is the fixed window containing the instant its
    /// records were created at, so every record it carries lies inside it — never the window
    /// the poller's own clock is in when it forms the batch, which for a record created
    /// before a boundary is the window AFTER the one that record belongs to.
    ///
    /// The rows differ along what the rule generalises over: where inside its window the
    /// record was created (its first second, inside it, its last second, the next window's
    /// first second), how far before the forming of the batch that was, and a record carrying
    /// no creation instant at all.
    #[test]
    fn the_window_an_invocation_reports_contains_the_records_it_carries() {
        let duration = Duration::from_secs(15);
        // The instant the batch is formed at: past the boundary at 1_800_000_015, so a window
        // taken from this clock rather than from the record is the window AFTER the one a
        // record created before that boundary belongs to.
        let formed_at = at(1_800_000_017);
        // (case, the instant the record was created at, the window it belongs to)
        let rows: Vec<(&str, Option<u64>, (u64, u64))> = vec![
            (
                "created-at-a-boundary",
                Some(1_800_000_000),
                (1_800_000_000, 1_800_000_015),
            ),
            (
                "created-inside-a-window",
                Some(1_800_000_007),
                (1_800_000_000, 1_800_000_015),
            ),
            (
                "created-in-the-last-second-before-a-boundary",
                Some(1_800_000_014),
                (1_800_000_000, 1_800_000_015),
            ),
            (
                "created-just-past-a-boundary",
                Some(1_800_000_015),
                (1_800_000_015, 1_800_000_030),
            ),
            (
                "created-windows-before-the-batch",
                Some(1_799_999_990),
                (1_799_999_985, 1_800_000_000),
            ),
            (
                "no-creation-instant-aggregates-into-the-window-of-the-clock",
                None,
                (1_800_000_015, 1_800_000_030),
            ),
        ];
        for (case, created, (start, end)) in rows {
            let records = vec![record("1", created)];
            let reported = window_of(&records, duration, formed_at);
            assert_eq!(
                reported,
                (at(start), at(end)),
                "case {case:?}: expected the window {start}..{end}, got {}..{}",
                instant(reported.0),
                instant(reported.1)
            );
            if let Some(created) = created_at(&records[0]) {
                assert!(
                    reported.0 <= created && created < reported.1,
                    "case {case:?}: the record created at {} must lie inside the window \
                     {}..{} the invocation reports",
                    instant(created),
                    instant(reported.0),
                    instant(reported.1)
                );
            }
        }
    }

    /// One row of the table below: the case, the window's duration in seconds, the instants
    /// the records of one read were created at, and how many of them are handed over
    /// together.
    type GroupingRow = (&'static str, u64, Vec<Option<u64>>, usize);

    /// RULE: one invocation carries the records at the head of what was read that belong to
    /// ONE window; a record created in a later window is left for the invocation of the
    /// window it belongs to.
    ///
    /// The rows differ along what the rule generalises over: the window's duration, how many
    /// windows the records read span, where the boundary falls inside what was read, a record
    /// carrying no creation instant, and a read every record of which is in its own window.
    #[test]
    fn one_invocation_carries_the_records_of_one_window() {
        // (case, the duration in seconds, the records' creation instants, the records handed
        //  over together)
        let rows: Vec<GroupingRow> = vec![
            (
                "every-record-in-one-window",
                15,
                vec![Some(1_800_000_001), Some(1_800_000_007), Some(1_800_000_014)],
                3,
            ),
            (
                "a-boundary-inside-what-was-read",
                15,
                vec![Some(1_800_000_013), Some(1_800_000_014), Some(1_800_000_015)],
                2,
            ),
            (
                "only-the-first-record-of-its-window",
                15,
                vec![Some(1_800_000_014), Some(1_800_000_020), Some(1_800_000_031)],
                1,
            ),
            (
                "a-record-per-window",
                1,
                vec![Some(1_800_000_000), Some(1_800_000_001)],
                1,
            ),
            (
                "a-window-wide-enough-for-the-whole-read",
                900,
                vec![Some(1_800_000_000), Some(1_800_000_450), Some(1_800_000_899)],
                3,
            ),
            (
                "a-record-with-no-creation-instant-travels-with-the-records-around-it",
                15,
                vec![Some(1_800_000_001), None, Some(1_800_000_007), Some(1_800_000_016)],
                3,
            ),
            ("nothing-was-read", 15, vec![], 0),
        ];
        for (case, seconds, created, expected) in rows {
            let duration = Duration::from_secs(seconds);
            let records: Vec<ShardRecord> = created
                .iter()
                .enumerate()
                .map(|(index, created)| record(&index.to_string(), *created))
                .collect();
            let handing = of_one_window(&records, duration, at(1_800_000_100));
            assert_eq!(
                handing.len(),
                expected,
                "case {case:?}: expected {expected} of the {} record(s) read handed over \
                 together, got {}",
                records.len(),
                handing.len()
            );
            // Whatever the row, every record handed over lies inside the one window the
            // invocation reports.
            let (start, end) = window_of(handing, duration, at(1_800_000_100));
            for held in handing {
                if let Some(created) = created_at(held) {
                    assert!(
                        start <= created && created < end,
                        "case {case:?}: the record created at {} must lie inside the window \
                         {}..{} it is handed over under",
                        instant(created),
                        instant(start),
                        instant(end)
                    );
                }
            }
        }
    }

    /// One row of the table below: the case, the window the records waiting on the shard
    /// belong to, the instant the shard is read at, and whether the window it holds is over.
    type AwaitingRow = (
        &'static str,
        Option<(SystemTime, SystemTime)>,
        SystemTime,
        bool,
    );

    /// RULE: a window a shard holds is over once nothing more aggregates into it — the clock
    /// has passed its end, or the records waiting on the shard belong to another window — and
    /// never while the records of that window itself are still waiting to be handed over.
    ///
    /// The rows differ along what the rule generalises over: which window the waiting records
    /// belong to (this one, the next one, none waiting at all) and where the clock stands
    /// against the window's end (inside it, past it).
    #[test]
    fn a_window_still_owed_its_own_records_is_not_finalised_yet() {
        let duration = Duration::from_secs(15);
        let opened_at = at(1_800_000_007);
        let its_own = window_at(opened_at, duration);
        let the_next = window_at(at(1_800_000_020), duration);
        let inside_it = at(1_800_000_010);
        let past_its_end = at(1_800_000_100);
        // (case, the window the records waiting on the shard belong to, the instant the shard
        //  is read at, whether the window it holds is over)
        let rows: Vec<AwaitingRow> = vec![
            ("nothing-waiting-inside-the-window", None, inside_it, false),
            ("its-own-records-waiting", Some(its_own), inside_it, false),
            (
                "its-own-records-still-waiting-past-its-end",
                Some(its_own),
                past_its_end,
                false,
            ),
            (
                "records-of-the-next-window-waiting",
                Some(the_next),
                inside_it,
                true,
            ),
            ("nothing-waiting-past-its-end", None, past_its_end, true),
        ];
        for (case, waiting, now, expected) in rows {
            let mut windows = BTreeMap::new();
            open(&mut windows, "shardId-1", duration, opened_at);
            let finalised = over(&windows, "shardId-1", waiting, now).is_some();
            assert_eq!(
                finalised, expected,
                "case {case:?}: expected the window to be over {expected}, got {finalised}"
            );
        }
        // A shard aggregating into no window owes no final invocation whatever waits on it.
        assert!(
            over(&BTreeMap::new(), "shardId-1", Some(its_own), past_its_end).is_none(),
            "case \"no-window\": a shard aggregating into no window owes no final invocation"
        );
        // A state past its bound ends the window at once, so it is finalised even while
        // records of it are waiting.
        let mut windows = BTreeMap::new();
        open(&mut windows, "shardId-1", duration, opened_at);
        let oversized = format!(
            r#"{{"state": {{"blob": "{}"}}}}"#,
            "x".repeat(MAX_WINDOW_STATE_BYTES)
        );
        carry_state(&mut windows, "shardId-1", Some(oversized.as_bytes()));
        assert!(
            over(&windows, "shardId-1", Some(its_own), opened_at).is_some(),
            "case \"terminated-early\": a window terminated early is over whatever is still \
             waiting for it"
        );
    }

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
