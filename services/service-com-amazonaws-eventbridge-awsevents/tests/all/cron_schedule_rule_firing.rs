// SPDX-License-Identifier: CC0-1.0

//! WHEN a rule that carries a `cron(...)` `ScheduleExpression` fires, end to end over the wire
//! through the real `MultiServiceRouter` with Step Functions mounted beside EventBridge — so
//! each firing is read back where a client reads it, in the executions the target state machine
//! holds.
//!
//! RULE: a rule whose `ScheduleExpression` is a `cron(...)` expression is triggered at the
//! instants that expression NAMES and at no other instant — the expression states them, so a
//! rule carrying one fires at its first named instant ahead of the clock however long after the
//! rule was written that falls, and never at an instant of the service's own choosing. A
//! `rate(...)` expression states an interval and no instant at all, which is why its rule's
//! first firing falls inside the first period instead.
//!
//! The rows differ along what the expression names NEXT: an instant inside the next minute, an
//! instant decades ahead, and an expression whose year is behind the clock and which therefore
//! names none at all. The `rate` rule beside them is the CONTROL: it shares the emulator, the
//! clock and the watch window with the cron rules, so "the cron rules fired nothing" is an
//! observation about their expressions rather than about a clock that never ran.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::target_dispatch::{
    emulator, events, execution_inputs, fired_within, given_state_machine, target_role, Emulator,
};

/// A cron expression naming an instant in every minute: minute, hour, day-of-month and month
/// unrestricted, "no specific value" (`?`) in the day-of-week field — the six-field,
/// minute-resolution form `ScheduleExpression` documents ("Minutes Hours Day-of-month Month
/// Day-of-week Year"), whose instants therefore fall at the top of each minute.
const EVERY_MINUTE: &str = "cron(* * * * ? *)";

/// How many seconds apart the instants [`EVERY_MINUTE`] names are, which is also the minute its
/// instants fall at the top of.
const MINUTE: u64 = 60;

/// A cron expression whose only instants are decades ahead: midnight on the first of January
/// of the last year the format's Year field admits.
const DECADES_AHEAD: &str = "cron(0 0 1 1 ? 2099)";

/// A cron expression whose year is BEHIND the clock, so it names no instant at all any more.
const YEAR_BEHIND: &str = "cron(0 20 * * ? 2020)";

/// The control rule's expression: the shortest cadence a `rate(...)` can state.
const RATE: &str = "rate(1 minute)";

/// How long a firing this suite EXPECTS is waited for: past the next instant
/// [`EVERY_MINUTE`] names (at most one minute on) with generous room for the clock's own tick,
/// the delivery and however loaded the host running every other suite beside this one is. A
/// ceiling on patience, never a span anything here counts on.
const PATIENCE: Duration = Duration::from_secs(180);

/// How long the rules that must fire NOTHING yet are watched for a firing: several times over
/// the span this emulator holds a new `rate` rule before its first firing
/// ([`service_com_amazonaws_eventbridge_awsevents::state::schedule::FIRST_FIRING_DELAY`]), so
/// "nothing fired" is an observation rather than a race.
const QUIET_WATCH: Duration = Duration::from_secs(20);

/// How far past the instant it names a firing may be observed and still be a firing AT that
/// instant: generous room for the clock's own tick and the delivery, and far short of the
/// [`MINUTE`] between two instants [`EVERY_MINUTE`] names.
const ALIGNMENT_SLACK: u64 = 20;

/// How long one poll of a target's executions waits before reading again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Write an `ENABLED` rule firing on `expression`, with a state machine of its own as its only
/// target, and answer that state machine's ARN.
async fn given_schedule_rule(
    emulator: &Emulator,
    case: &str,
    name: &str,
    expression: &str,
) -> String {
    let machine = given_state_machine(emulator, name).await;
    let (status, written) = events(
        emulator,
        "PutRule",
        &json!({
            "Name": name,
            "ScheduleExpression": expression,
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRule answered {status} {written}"
    );
    let (status, targeted) = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{ "Id": "t1", "Arn": machine, "RoleArn": target_role() }],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutTargets answered {status} {targeted}"
    );
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targeted}"
    );
    machine
}

/// A clock reading as the seconds since the epoch it names.
fn epoch_seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The instant an envelope's `time` names, in seconds since the epoch.
///
/// The field is "an instant at WHOLE-SECOND precision closed by `Z`", so it is read as the
/// RFC 3339 instant it is written as.
fn fired_at(case: &str, input: &str) -> u64 {
    let envelope: Value = serde_json::from_str(input)
        .unwrap_or_else(|error| panic!("case {case:?}: the input is not JSON ({error}): {input:?}"));
    let time = envelope["time"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no time in {envelope}"))
        .to_string();
    let parsed = ::chrono::DateTime::parse_from_rfc3339(&time)
        .unwrap_or_else(|error| panic!("case {case:?}: {time:?} is not an RFC 3339 instant ({error})"));
    epoch_seconds(SystemTime::from(parsed))
}

/// A cron rule fires AT an instant its expression names: the firing a client sees for an
/// expression naming an instant in every minute falls at the top of a minute, and never before
/// the first such instant ahead of the moment the rule could reach its target — rather than a
/// span of the service's own choosing after that moment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_rule_fires_at_an_instant_its_expression_names() {
    let case = "an instant in every minute";
    let emulator = emulator();
    let machine = given_schedule_rule(&emulator, case, "cron-every-minute", EVERY_MINUTE).await;
    // The first instant the expression names once the rule can reach its target: the top of the
    // minute after it, because this expression names the top of every minute.
    let armed = epoch_seconds(SystemTime::now());
    let names_next = armed - armed % MINUTE + MINUTE;

    let inputs = fired_within(&emulator, &machine, PATIENCE).await;
    let first = inputs.first().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected a firing at the next instant {EVERY_MINUTE} names, got none"
        )
    });
    let at = fired_at(case, first);
    assert!(
        at >= names_next,
        "case {case:?}: expected no firing before {names_next}, the first instant \
         {EVERY_MINUTE} names once the rule could reach its target, got one at {at}"
    );
    let into_the_minute = at % MINUTE;
    assert!(
        into_the_minute <= ALIGNMENT_SLACK,
        "case {case:?}: expected a firing at the top of a minute, which is what {EVERY_MINUTE} \
         names, got one {into_the_minute} seconds into it ({first})"
    );
}

/// A cron rule whose expression names NO instant the clock has reached fires nothing: one whose
/// instants are decades ahead and one whose year is behind the clock both leave their targets
/// untouched, while the `rate` rule beside them — on the same emulator, the same clock and the
/// same window — fires.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cron_rule_fires_nothing_before_an_instant_it_names() {
    let emulator = emulator();
    // (case, the expression, the rule's name)
    let quiet: [(&str, &str, &str); 2] = [
        ("instants decades ahead", DECADES_AHEAD, "cron-decades-ahead"),
        ("a year behind the clock", YEAR_BEHIND, "cron-year-behind"),
    ];
    let mut watched = Vec::with_capacity(quiet.len());
    for (case, expression, name) in quiet {
        let machine = given_schedule_rule(&emulator, case, name, expression).await;
        watched.push((case, expression, machine));
    }
    let control = given_schedule_rule(&emulator, "the rate control", "rate-control", RATE).await;

    // Watch the whole window: a firing of one of these rules is a failure the moment it
    // appears, whichever instant of the window it appears in.
    let began = Instant::now();
    while began.elapsed() < QUIET_WATCH {
        for (case, expression, machine) in &watched {
            let inputs = execution_inputs(&emulator, machine).await;
            assert!(
                inputs.is_empty(),
                "case {case:?}: expected no firing before an instant {expression} names, got \
                 {inputs:?}"
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // And the clock DID run through that window: the control rule, whose expression names an
    // interval rather than instants, fired inside its first period.
    let fired = fired_within(&emulator, &control, PATIENCE).await;
    assert!(
        !fired.is_empty(),
        "case {:?}: expected the control rule to have fired inside its first period, got none",
        "the rate control"
    );
}
