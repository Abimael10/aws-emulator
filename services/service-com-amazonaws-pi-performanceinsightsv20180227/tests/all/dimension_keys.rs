// SPDX-License-Identifier: CC0-1.0

//! `DescribeDimensionKeys`, end to end over the wire.
//!
//! The rules under test are the ones the model, the member documentation and the
//! service's recorded behaviour state: `StartTime` must be earlier than
//! `EndTime`, and a window that is not is refused with the modelled
//! `InvalidArgumentException` (HTTP 400); a window that is gets the same granular
//! alignment `GetResourceMetrics` reports (`AlignedStartTime` at or before the
//! requested start, `AlignedEndTime` the latest period boundary the service
//! holds — its own clock floored to the effective period — both on a boundary of
//! that period); and the key list is what the window HOLDS — a dimension key is
//! a group of the DB sessions sampled on the engine, so the keys of a window are
//! the activity the data source's engine recorded inside it, grouped by the
//! request's `GroupBy`, selected by its `Filter`, broken down by its
//! `PartitionBy` and ordered by the load each group accounted for, descending. A
//! window over a data source whose engine recorded nothing holds no keys, which
//! is reported as the empty list rather than as invented sessions.

use serde_json::{json, Value};
use service_common::db_activity_samples::{ActivitySample, WaitEvent};
use service_common::db_data_sources::MonitoringConfiguration;
use smithy_server_runtime::MultiServiceRouter;

use crate::pi_harness::{call, emulator, emulator_with_engine, record_activity, register_data_source};

/// One day in seconds — a multiple of every documented period.
const ONE_DAY: i64 = 86_400;

/// The granularities `PeriodInSeconds` documents: 1 (one second), 60 (one
/// minute), 300 (five minutes), 3600 (one hour), 86400 (twenty-four hours).
const DOCUMENTED_PERIODS: &[i64] = &[1, 60, 300, 3600, 86400];

/// Epoch seconds, as the service reads its own clock.
fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is at or after the Unix epoch")
            .as_secs(),
    )
    .expect("the current time fits in an i64")
}

/// `seconds` floored to a `period` boundary measured from the Unix epoch.
fn floor_to(seconds: i64, period: i64) -> i64 {
    seconds - seconds.rem_euclid(period)
}

/// A window start that leaves a `span`-second window entirely in the past, two
/// days clear of the service's clock, and on a boundary of every documented
/// period.
fn past_start(span: i64) -> i64 {
    floor_to(unix_now() - span - 2 * ONE_DAY, ONE_DAY)
}

/// A monitored data source: an immutable DB resource id of the form the model
/// documents on every `Identifier` member.
const IDENTIFIER: &str = "db-ABCDEFG1234567";

/// Dimension groups `GroupBy.Group` may name, from the model's own
/// `DimensionGroup$Group` documentation and the dimension groups Performance
/// Insights publishes for RDS (`db`, `db.application`, `db.host`, `db.sql`,
/// `db.sql_tokenized`, `db.user`, `db.wait_event`, `db.wait_event_type`). The
/// rows below span several of them, including ones no recording cites.
const GROUPS: &[&str] = &[
    "db.sql",
    "db.sql_tokenized",
    "db.wait_event",
    "db.user",
    "db.host",
];

fn dimension_keys_input(
    identifier: &str,
    group: &str,
    metric: &str,
    start: i64,
    end: i64,
    period: Option<i64>,
) -> Value {
    let mut input = json!({
        "ServiceType": "RDS",
        "Identifier": identifier,
        "StartTime": start,
        "EndTime": end,
        "Metric": metric,
        "GroupBy": {"Group": group},
    });
    if let Some(period) = period {
        input["PeriodInSeconds"] = json!(period);
    }
    input
}

/// RULE: documented on `StartTime` — "The value for StartTime must be earlier
/// than the value for EndTime."
#[tokio::test]
async fn a_window_whose_end_does_not_follow_its_start_is_refused() {
    let emulator = emulator();

    // The rows differ along the ordering: an end before the start by a second,
    // by half an hour and by a day, and an end exactly at the start (which
    // describes no window either).
    let t0 = past_start(86_400);
    for (start, end, label) in [
        (t0, t0 - 1, "an end one second before the start"),
        (t0 + 1_800, t0 - 1_800, "an end half an hour before the start"),
        (t0, t0 - 86_400, "an end a day before the start"),
        (t0, t0, "an end equal to the start"),
    ] {
        for group in GROUPS {
            let case = format!("{label} / {group}");
            let answer = call(
                &emulator,
                "DescribeDimensionKeys",
                dimension_keys_input(IDENTIFIER, group, "db.load.avg", start, end, None),
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgumentException",
                "case {case}: expected InvalidArgumentException, got {:?}",
                answer.body
            );
        }
    }
}

/// RULE: a window whose end follows its start is answered with the aligned
/// window — the requested start floored to the effective period, and the latest
/// boundary the service holds (its clock floored to that period) — and with the
/// dimension keys it holds.
#[tokio::test]
async fn a_well_ordered_window_is_answered_with_the_aligned_window_and_its_keys() {
    let emulator = emulator();

    // The rows differ along the period (every documented value, including both
    // published bounds), whether the requested ends already sit on a boundary,
    // and the dimension group asked for.
    let mut cases: Vec<(i64, i64, Option<i64>, &str)> = Vec::new();
    for &period in DOCUMENTED_PERIODS {
        let t0 = past_start(period * 2);
        for (start_offset, end_offset) in [(0, period), (1, period + 1), (period - 1, period * 2)] {
            for group in GROUPS {
                cases.push((t0 + start_offset, t0 + end_offset, Some(period), group));
            }
        }
    }
    // And one row with no `PeriodInSeconds` at all, where the service picks it.
    let t0 = past_start(1_800);
    cases.push((t0, t0 + 1_800, None, "db.sql"));

    for (start, end, period, group) in cases {
        let case = format!("start={start} end={end} period={period:?} group={group}");
        let before = unix_now();
        let answer = call(
            &emulator,
            "DescribeDimensionKeys",
            dimension_keys_input(IDENTIFIER, group, "db.load.avg", start, end, period),
        )
        .await;
        let after = unix_now();
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} {:?}",
            answer.status, answer.body
        );

        let aligned_start = answer
            .member("AlignedStartTime")
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: no AlignedStartTime in {:?}", answer.body));
        let aligned_end = answer
            .member("AlignedEndTime")
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: no AlignedEndTime in {:?}", answer.body));
        assert!(
            aligned_start < aligned_end,
            "case {case}: expected AlignedStartTime < AlignedEndTime, got \
             {aligned_start}..{aligned_end}"
        );
        if let Some(period) = period {
            assert_eq!(
                aligned_start,
                floor_to(start, period),
                "case {case}: expected the start floored to the {period}s boundary, got \
                 {aligned_start}"
            );
            // The end bound is the service's clock floored to the period — the
            // latest boundary it holds — so the requested end does not drive it.
            let candidates = [floor_to(before, period), floor_to(after, period)];
            assert!(
                candidates.contains(&aligned_end),
                "case {case}: expected AlignedEndTime at the current {period}s boundary \
                 ({candidates:?}), got {aligned_end}"
            );
        } else {
            assert!(
                aligned_start <= start && aligned_end <= after,
                "case {case}: expected a start at or before {start} and an end at or before \
                 the clock, got {aligned_start}..{aligned_end}"
            );
        }

        // A dimension key is a group of DB sessions; this emulator runs no
        // engine and holds none, so the window holds no keys — reported as the
        // empty list, not as a 501 and not as invented sessions.
        assert_eq!(
            answer.member("Keys").as_array().map(Vec::len),
            Some(0),
            "case {case}: expected an empty Keys list, got {:?}",
            answer.body
        );
        // Nothing to page, so no cursor is handed out.
        assert!(
            answer.member("NextToken").is_null(),
            "case {case}: expected no NextToken, got {:?}",
            answer.body
        );
        // `PartitionKeys` is the breakdown `PartitionBy` asks for, and this
        // request asked for none.
        assert!(
            answer.member("PartitionKeys").is_null(),
            "case {case}: expected no PartitionKeys without PartitionBy, got {:?}",
            answer.body
        );
    }
}

/// RULE: "If PartitionBy was present in the request, PartitionKeys contains the
/// breakdown of dimension keys by the specified partitions" — present when the
/// request asks for a breakdown, absent when it does not.
#[tokio::test]
async fn partition_keys_are_reported_exactly_when_partition_by_asks_for_them() {
    let emulator = emulator();

    let t0 = past_start(3_600);
    for partition_by in [None, Some(json!({"Group": "db.user"})), Some(json!({"Group": "db.host", "Limit": 5}))] {
        let case = format!("PartitionBy={partition_by:?}");
        let mut input =
            dimension_keys_input(IDENTIFIER, "db.sql", "db.load.avg", t0, t0 + 3_600, Some(60));
        if let Some(partition_by) = &partition_by {
            input["PartitionBy"] = partition_by.clone();
        }

        let answer = call(&emulator, "DescribeDimensionKeys", input).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        let reported = answer.member("PartitionKeys").as_array().map(Vec::len);
        let expected = partition_by.as_ref().map(|_| 0);
        assert_eq!(
            reported, expected,
            "case {case}: expected PartitionKeys {expected:?}, got {:?}",
            answer.body
        );
    }
}

/// RULE: `PeriodInSeconds` accepts the documented granularities and nothing
/// else — the same set `GetResourceMetrics` accepts, because it is the member's
/// own documented value list.
#[tokio::test]
async fn period_in_seconds_accepts_exactly_the_documented_granularities() {
    let emulator = emulator();

    for &period in DOCUMENTED_PERIODS {
        let t0 = past_start(period);
        let answer = call(
            &emulator,
            "DescribeDimensionKeys",
            // A window of one period, so even the one-second granularity stays
            // small.
            dimension_keys_input(IDENTIFIER, "db.sql", "db.load.avg", t0, t0 + period, Some(period)),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case period={period} (documented): expected 200, got {} {:?}",
            answer.status, answer.body
        );
    }

    // Each row is one period off the documented set, including values just
    // below and just above every published one.
    let t0 = past_start(3_600);
    for period in [0, 2, 30, 59, 61, 299, 301, 3_599, 3_601, 86_399, 86_401, 172_800] {
        let answer = call(
            &emulator,
            "DescribeDimensionKeys",
            dimension_keys_input(
                IDENTIFIER,
                "db.sql",
                "db.load.avg",
                t0,
                t0 + 3_600,
                Some(period),
            ),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case period={period} (not documented): expected 400, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgumentException",
            "case period={period} (not documented): expected InvalidArgumentException, got {:?}",
            answer.body
        );
    }
}

/// RULE: a reporting operation that materialises nothing per period is not
/// refused for the WIDTH of its window — the data-point budget belongs to the
/// operation that allocates a point per period, not to the shared window
/// arithmetic.
#[tokio::test]
async fn a_window_wider_than_the_data_point_budget_is_still_answered() {
    let emulator = emulator();

    // A week at the finest documented granularity is 604 800 periods, far past
    // the data-point budget `GetResourceMetrics` is held to; this operation
    // answers it, because it reports no series over it.
    let week: i64 = 7 * 86_400;
    let t0 = past_start(week);
    let before = unix_now();
    let answer = call(
        &emulator,
        "DescribeDimensionKeys",
        dimension_keys_input(IDENTIFIER, "db.sql", "db.load.avg", t0, t0 + week, Some(1)),
    )
    .await;
    let after = unix_now();
    assert_eq!(
        answer.status, 200,
        "case a week at one-second granularity: expected 200, got {} {:?}",
        answer.status, answer.body
    );
    let aligned_end = answer.member("AlignedEndTime").as_i64().unwrap_or(-1);
    assert_eq!(
        answer.member("AlignedStartTime").as_i64(),
        Some(t0),
        "case a week at one-second granularity: expected the requested start back, got {:?}",
        answer.body
    );
    assert!(
        (before..=after).contains(&aligned_end),
        "case a week at one-second granularity: expected the end bound at the current second \
         ({before}..={after}), got {aligned_end}"
    );
    assert_eq!(
        answer.member("Keys").as_array().map(Vec::len),
        Some(0),
        "case a week at one-second granularity: expected an empty Keys list, got {:?}",
        answer.body
    );

    // The same window IS refused by the operation that would have to
    // materialise a point per period, so the budget is where the allocation is.
    let metrics = call(
        &emulator,
        "GetResourceMetrics",
        json!({
            "ServiceType": "RDS",
            "Identifier": IDENTIFIER,
            "MetricQueries": [{"Metric": "db.load.avg"}],
            "StartTime": t0,
            "EndTime": t0 + week,
            "PeriodInSeconds": 1,
        }),
    )
    .await;
    assert_eq!(
        metrics.error_code(),
        "InvalidArgumentException",
        "case the same window on GetResourceMetrics: expected InvalidArgumentException, got {} \
         {:?}",
        metrics.status,
        metrics.body
    );
}

/// RULE: the data-source authorization gate this service applies to every
/// operation naming a `(ServiceType, Identifier)` pair applies here too — an
/// identifier that is no immutable DB resource id is the uniform
/// `NotAuthorizedException`, never a 501 and never a served empty answer.
#[tokio::test]
async fn an_unmonitored_data_source_is_refused_as_not_authorized() {
    let emulator = emulator();

    // The rows span the forms an identifier can take without being a resource
    // id: a DB instance name, a bare word, a lowercase token, a token with no
    // kind prefix, an empty token.
    let t0 = past_start(3_600);
    for identifier in [
        "my-production-database",
        "database",
        "db-abcdefghijkl",
        "ABCDEFGHIJKLMNOPQRSTU1VW2X",
        "db-",
        "cluster-",
    ] {
        for service_type in ["RDS", "DOCDB"] {
            let case = format!("{identifier:?} / {service_type}");
            let mut input =
                dimension_keys_input(identifier, "db.sql", "db.load.avg", t0, t0 + 3_600, Some(60));
            input["ServiceType"] = json!(service_type);
            let answer = call(&emulator, "DescribeDimensionKeys", input).await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "NotAuthorizedException",
                "case {case}: expected NotAuthorizedException, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.error_message(),
                service_com_amazonaws_pi_performanceinsightsv20180227::state::NOT_AUTHORIZED_MESSAGE,
                "case {case}: expected the service's uniform authorization message, got {:?}",
                answer.body
            );
        }
    }
}

/// RULE: this operation hands out no `NextToken` (there is never a partial
/// page), so a token presented to it was not issued for the result set it
/// claims to resume and is refused rather than silently ignored.
#[tokio::test]
async fn a_cursor_this_operation_never_issued_is_refused() {
    let emulator = emulator();

    // The rows differ in where the token came from: one this service issued for
    // a DIFFERENT operation's result set, and one that is merely inside the
    // modelled `NextToken` pattern.
    let t0 = past_start(3_600);
    let mut paged = json!({
        "ServiceType": "RDS",
        "Identifier": IDENTIFIER,
        "MetricQueries": [{"Metric": "db.load.avg"}, {"Metric": "db.sampledload.avg"}],
        "StartTime": t0,
        "EndTime": t0 + 3_600,
        "PeriodInSeconds": 3_600,
    });
    paged["MaxResults"] = json!(1);
    let issued_elsewhere = call(&emulator, "GetResourceMetrics", paged)
        .await
        .member("NextToken")
        .as_str()
        .expect("a truncated GetResourceMetrics page hands back a cursor")
        .to_string();

    for (label, token) in [
        ("a cursor issued for another operation", issued_elsewhere),
        ("a pattern-valid token nothing issued", "abc123_=-".to_string()),
    ] {
        let mut input =
            dimension_keys_input(IDENTIFIER, "db.sql", "db.load.avg", t0, t0 + 3_600, Some(60));
        input["NextToken"] = json!(token);
        let answer = call(&emulator, "DescribeDimensionKeys", input).await;
        assert_eq!(
            answer.error_code(),
            "InvalidArgumentException",
            "case {label}: expected InvalidArgumentException, got {} {:?}",
            answer.status,
            answer.body
        );
    }
}

/// RULE: the key list is computed from the request and nothing else, so
/// repeating a call answers the same keys and the same start bound, and reading
/// dimension keys changes nothing a later caller sees. The END bound tracks the
/// service's clock, so it is the one member a repeat may advance.
#[tokio::test]
async fn repeating_the_same_request_answers_the_same_keys() {
    let emulator = emulator();
    let t0 = past_start(3_600);
    let input = dimension_keys_input(IDENTIFIER, "db.sql", "db.load.avg", t0 + 17, t0 + 3_600, Some(60));

    let first = call(&emulator, "DescribeDimensionKeys", input.clone()).await;
    let second = call(&emulator, "DescribeDimensionKeys", input).await;
    assert_eq!(
        first.status, 200,
        "case the first call: expected 200, got {} {:?}",
        first.status, first.body
    );
    for member in ["AlignedStartTime", "Keys", "PartitionKeys", "NextToken"] {
        assert_eq!(
            first.member(member),
            second.member(member),
            "case repeated call: expected the same {member}, got {:?} then {:?}",
            first.member(member),
            second.member(member)
        );
    }
    assert!(
        second.member("AlignedEndTime").as_i64() >= first.member("AlignedEndTime").as_i64(),
        "case repeated call: expected the end bound to track the clock forward, got {:?} then \
         {:?}",
        first.member("AlignedEndTime"),
        second.member("AlignedEndTime")
    );

    // And the report store the sibling operations own is untouched.
    let listed = call(
        &emulator,
        "ListPerformanceAnalysisReports",
        json!({"ServiceType": "RDS", "Identifier": IDENTIFIER}),
    )
    .await;
    assert_eq!(
        listed.member("AnalysisReports").as_array().map(Vec::len),
        Some(0),
        "case after two reads: expected the report store untouched, got {:?}",
        listed.body
    );
}

// ---------------------------------------------------------------------------
// The keys of a window that HELD session activity
// ---------------------------------------------------------------------------

/// The window every test below reports on: one hour, entirely in the past so the
/// service's own clock cannot move either bound under the assertions.
const WINDOW_SPAN: i64 = 3_600;

/// One unit of recorded session activity, as a database data plane records it when its
/// engine finishes a statement: the instant it was observed, how long the session was
/// active for it, what it was waiting on, who ran it and what they ran.
fn activity(
    at: i64,
    active_seconds: f64,
    wait: (&str, &str),
    user: &str,
    statement: &str,
) -> ActivitySample {
    ActivitySample {
        at_epoch_seconds: at as f64,
        active_seconds,
        wait_event: WaitEvent {
            name: wait.0.to_string(),
            event_type: wait.1.to_string(),
        },
        session_id: 42,
        user: user.to_string(),
        host: "10.0.1.7".to_string(),
        database: "appdb".to_string(),
        application: String::new(),
        statement: statement.to_string(),
        statement_digest: statement
            .split_whitespace()
            .map(|word| match word.chars().all(|c| c.is_ascii_digit()) {
                true => "?",
                false => word,
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// An emulator holding `samples` as the activity of [`IDENTIFIER`]'s engine, with the
/// data source itself on record the way the RDS control plane records a DB instance it
/// minted — and with the monitoring configuration `monitoring`, which is what decides
/// whether Performance Insights collects activity for it at all.
fn emulator_with_activity(
    samples: &[ActivitySample],
    monitoring: Option<MonitoringConfiguration>,
) -> MultiServiceRouter {
    let (router, engine) = emulator_with_engine();
    register_data_source(&engine, IDENTIFIER, "RDS", "mysql", monitoring);
    for sample in samples {
        record_activity(&engine, IDENTIFIER, sample.clone());
    }
    router
}

/// The wait events the two engine families publish for a session waiting on a row lock
/// and for one that was running: the pairs the RDS data plane files its samples under.
const LOCK_WAIT: (&str, &str) = ("synch/cond/mysys/my_thread_var::suspend", "synch");
const CPU: (&str, &str) = ("CPU", "CPU");

/// The `Total` of every key of an answer, in the order answered.
fn totals(answer: &Value) -> Vec<f64> {
    answer
        .get("Keys")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .map(|key| key.get("Total").and_then(Value::as_f64).unwrap_or(f64::NAN))
                .collect()
        })
        .unwrap_or_default()
}

/// The `Dimensions` map of key `index`.
fn dimensions(answer: &Value, index: usize) -> serde_json::Map<String, Value> {
    answer
        .get("Keys")
        .and_then(Value::as_array)
        .and_then(|keys| keys.get(index))
        .and_then(|key| key.get("Dimensions"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// RULE: the keys of a window are the recorded activity of the window, each with its
/// `Dimensions` and a float `Total` — the average active sessions that group accounted
/// for — ordered by `Total` DESCENDING.
#[tokio::test]
async fn keys_report_the_recorded_activity_ordered_by_total_descending() {
    let t0 = past_start(WINDOW_SPAN);
    // Three samples: two sessions blocked on a row lock and one on CPU, so the window
    // holds two differently weighted wait events.
    let samples = [
        activity(t0 + 10, 60.0, LOCK_WAIT, "appuser", "UPDATE ledger SET amount = 1"),
        activity(t0 + 90, 30.0, LOCK_WAIT, "reporter", "UPDATE ledger SET amount = 2"),
        activity(t0 + 120, 45.0, CPU, "appuser", "SELECT BENCHMARK(1, MD5('x'))"),
    ];
    let emulator = emulator_with_activity(&samples, None);

    let answer = call(
        &emulator,
        "DescribeDimensionKeys",
        dimension_keys_input(
            IDENTIFIER,
            "db.wait_event",
            "db.load.avg",
            t0,
            t0 + WINDOW_SPAN,
            Some(60),
        ),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case a window that held activity: expected 200, got {} {:?}",
        answer.status, answer.body
    );

    let reported = totals(&answer.body);
    let span = WINDOW_SPAN as f64;
    let expected = vec![90.0 / span, 45.0 / span];
    assert_eq!(
        reported.len(),
        expected.len(),
        "case a window that held activity: expected {} keys, got {:?}",
        expected.len(),
        answer.body
    );
    for (index, (got, want)) in reported.iter().zip(&expected).enumerate() {
        assert!(
            (got - want).abs() < 1e-9,
            "case key {index}: expected the average active sessions {want}, got {got}"
        );
    }
    assert!(
        reported.windows(2).all(|pair| pair[0] >= pair[1]),
        "case a window that held activity: expected the totals ordered descending, got \
         {reported:?}"
    );
    assert_eq!(
        dimensions(&answer.body, 0)
            .get("db.wait_event.name")
            .and_then(Value::as_str),
        Some(LOCK_WAIT.0),
        "case the heaviest key: expected the lock wait on top, got {:?}",
        answer.body
    );
    assert_eq!(
        dimensions(&answer.body, 0)
            .get("db.wait_event.type")
            .and_then(Value::as_str),
        Some(LOCK_WAIT.1),
        "case the heaviest key: expected every dimension of the group, got {:?}",
        answer.body
    );

    // A window BEFORE the activity holds none of it: the keys follow the window.
    let earlier = call(
        &emulator,
        "DescribeDimensionKeys",
        dimension_keys_input(
            IDENTIFIER,
            "db.wait_event",
            "db.load.avg",
            t0 - WINDOW_SPAN,
            t0,
            Some(60),
        ),
    )
    .await;
    assert_eq!(
        earlier.member("Keys").as_array().map(Vec::len),
        Some(0),
        "case the window before the activity: expected no keys, got {:?}",
        earlier.body
    );
}

/// RULE: the keys are grouped by the dimension group the request names, so each group
/// reports its own dimensions and its own number of keys; a group whose dimensions the
/// recorded activity does not fill reports no key at all rather than one filled from
/// something else.
#[tokio::test]
async fn keys_are_grouped_by_the_dimension_group_the_request_names() {
    let t0 = past_start(WINDOW_SPAN);
    let samples = [
        activity(t0 + 10, 60.0, LOCK_WAIT, "appuser", "UPDATE ledger SET amount = 1"),
        activity(t0 + 90, 30.0, LOCK_WAIT, "reporter", "UPDATE ledger SET amount = 2"),
        activity(t0 + 120, 45.0, CPU, "appuser", "SELECT BENCHMARK(1, MD5('x'))"),
    ];
    let emulator = emulator_with_activity(&samples, None);

    // Each row is a dimension group, how many keys the samples above hold for it, and a
    // dimension every one of those keys must carry.
    let rows: &[(&str, usize, &str)] = &[
        ("db.wait_event", 2, "db.wait_event.name"),
        ("db.wait_event_type", 2, "db.wait_event_type.name"),
        ("db.user", 2, "db.user.name"),
        ("db.host", 1, "db.host.name"),
        // The two UPDATEs differ in the value they set, so they are two statements and
        // one digest.
        ("db.sql", 3, "db.sql.statement"),
        ("db.sql_tokenized", 2, "db.sql_tokenized.statement"),
        ("db", 1, "db.name"),
        // Groups whose dimensions name what a blocking session was doing, which this
        // emulator's engines record nothing of.
        ("db.blocking_session", 0, "db.blocking_session.id"),
        ("db.query", 0, "db.query.statement"),
    ];

    for (group, keys, dimension) in rows {
        let answer = call(
            &emulator,
            "DescribeDimensionKeys",
            dimension_keys_input(
                IDENTIFIER,
                group,
                "db.load.avg",
                t0,
                t0 + WINDOW_SPAN,
                Some(60),
            ),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case the group {group}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.member("Keys").as_array().map(Vec::len),
            Some(*keys),
            "case the group {group}: expected {keys} key(s), got {:?}",
            answer.body
        );
        for index in 0..*keys {
            assert!(
                dimensions(&answer.body, index).contains_key(*dimension),
                "case the group {group}, key {index}: expected the dimension {dimension}, \
                 got {:?}",
                answer.body
            );
        }
        // Every key weighs something: a group with no load is not a key.
        assert!(
            totals(&answer.body).iter().all(|total| *total > 0.0),
            "case the group {group}: expected every key to carry its load, got {:?}",
            answer.body
        );
    }

    // `GroupBy.Dimensions` restricts which of the group's dimensions come back:
    // "Performance Insights returns all dimensions within this group, unless you provide
    // the names of specific dimensions within this group".
    let mut input = dimension_keys_input(
        IDENTIFIER,
        "db.wait_event",
        "db.load.avg",
        t0,
        t0 + WINDOW_SPAN,
        Some(60),
    );
    input["GroupBy"] = json!({"Group": "db.wait_event", "Dimensions": ["db.wait_event.name"]});
    let restricted = call(&emulator, "DescribeDimensionKeys", input).await;
    assert_eq!(
        dimensions(&restricted.body, 0).keys().collect::<Vec<_>>(),
        vec!["db.wait_event.name"],
        "case a restricted dimension list: expected only the named dimension, got {:?}",
        restricted.body
    );
}

/// RULE: `GroupBy.Limit` asks for "a limited number of values for a dimension" — the top
/// N of the descending order — and `MaxResults`/`NextToken` page what is left, visiting
/// every key exactly once.
#[tokio::test]
async fn the_limit_takes_the_top_n_and_max_results_pages_the_rest() {
    let t0 = past_start(WINDOW_SPAN);
    // Four users, each with a different load, so the top-N cut is observable.
    let samples: Vec<ActivitySample> = [("u1", 80.0), ("u2", 40.0), ("u3", 20.0), ("u4", 10.0)]
        .iter()
        .enumerate()
        .map(|(index, (user, active))| {
            activity(t0 + 10 + index as i64, *active, CPU, user, "SELECT 1")
        })
        .collect();
    let emulator = emulator_with_activity(&samples, None);

    let whole = call(
        &emulator,
        "DescribeDimensionKeys",
        dimension_keys_input(IDENTIFIER, "db.user", "db.load.avg", t0, t0 + WINDOW_SPAN, Some(60)),
    )
    .await;
    let all_totals = totals(&whole.body);
    assert_eq!(
        all_totals.len(),
        4,
        "case the whole key list: expected one key per user, got {:?}",
        whole.body
    );

    // Each row is a `Limit`, including one past the number of keys the window holds.
    for limit in [1_usize, 2, 3, 4, 25] {
        let mut input = dimension_keys_input(
            IDENTIFIER,
            "db.user",
            "db.load.avg",
            t0,
            t0 + WINDOW_SPAN,
            Some(60),
        );
        input["GroupBy"] = json!({"Group": "db.user", "Limit": limit});
        let answer = call(&emulator, "DescribeDimensionKeys", input).await;
        let reported = totals(&answer.body);
        assert_eq!(
            reported,
            all_totals[..limit.min(all_totals.len())].to_vec(),
            "case Limit={limit}: expected the top {limit} of the descending order, got {:?}",
            answer.body
        );
    }

    // Paging a `MaxResults` of one visits each key once, in the same order, and the
    // cursor stops when the keys run out.
    for page_size in [1_i64, 2, 3] {
        let case = format!("MaxResults={page_size}");
        let mut seen: Vec<f64> = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..=all_totals.len() {
            let mut input = dimension_keys_input(
                IDENTIFIER,
                "db.user",
                "db.load.avg",
                t0,
                t0 + WINDOW_SPAN,
                Some(60),
            );
            input["MaxResults"] = json!(page_size);
            if let Some(token) = &token {
                input["NextToken"] = json!(token);
            }
            let answer = call(&emulator, "DescribeDimensionKeys", input).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: expected 200, got {} {:?}",
                answer.status, answer.body
            );
            let page = totals(&answer.body);
            assert!(
                page.len() <= page_size as usize,
                "case {case}: expected at most {page_size} keys in a page, got {page:?}"
            );
            seen.extend(page);
            match answer.member("NextToken").as_str() {
                Some(next) => token = Some(next.to_string()),
                None => {
                    token = None;
                    break;
                }
            }
        }
        assert!(
            token.is_none(),
            "case {case}: expected paging to finish, still holding {token:?}"
        );
        assert_eq!(
            seen, all_totals,
            "case {case}: expected paging to visit every key once, in order, got {seen:?}"
        );
    }
}

/// RULE: a `Filter` selects among the sampled sessions by the dimension values they had,
/// so the keys and their totals are the load of the sessions it names — and a filter
/// nothing matches reports no keys.
#[tokio::test]
async fn a_filter_selects_the_sessions_it_names() {
    let t0 = past_start(WINDOW_SPAN);
    let samples = [
        activity(t0 + 10, 60.0, LOCK_WAIT, "appuser", "UPDATE ledger SET amount = 1"),
        activity(t0 + 90, 30.0, CPU, "appuser", "SELECT 1"),
        activity(t0 + 120, 45.0, CPU, "reporter", "SELECT 2"),
    ];
    let emulator = emulator_with_activity(&samples, None);
    let span = WINDOW_SPAN as f64;

    // Each row is a filter and the total load the keys it selects must add up to.
    let rows: &[(&str, Value, f64)] = &[
        ("no filter at all", json!({}), 135.0 / span),
        (
            "one user",
            json!({"db.user.name": "appuser"}),
            90.0 / span,
        ),
        (
            "one wait event",
            json!({"db.wait_event.name": CPU.0}),
            75.0 / span,
        ),
        (
            "a user and a wait event together",
            json!({"db.user.name": "appuser", "db.wait_event.name": CPU.0}),
            30.0 / span,
        ),
        (
            "a value no session had",
            json!({"db.user.name": "nobody"}),
            0.0,
        ),
        (
            "a dimension nothing records",
            json!({"db.blocking_session.id": "7"}),
            0.0,
        ),
    ];

    for (case, filter, expected) in rows {
        let mut input = dimension_keys_input(
            IDENTIFIER,
            "db.wait_event",
            "db.load.avg",
            t0,
            t0 + WINDOW_SPAN,
            Some(60),
        );
        if !filter.as_object().is_none_or(serde_json::Map::is_empty) {
            input["Filter"] = filter.clone();
        }
        let answer = call(&emulator, "DescribeDimensionKeys", input).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        let summed: f64 = totals(&answer.body).iter().sum();
        assert!(
            (summed - expected).abs() < 1e-9,
            "case {case}: expected the selected load {expected}, got {summed} in {:?}",
            answer.body
        );
    }
}

/// RULE: "If PartitionBy was present in the request, PartitionKeys contains the breakdown
/// of dimension keys by the specified partitions" — the partitions are the values of the
/// secondary group inside the window, and each key carries one value per partition,
/// aligned to that order and adding up to its own total.
#[tokio::test]
async fn partitions_break_each_key_down_in_the_partition_keys_order() {
    let t0 = past_start(WINDOW_SPAN);
    let samples = [
        activity(t0 + 10, 60.0, LOCK_WAIT, "appuser", "UPDATE ledger SET amount = 1"),
        activity(t0 + 90, 30.0, CPU, "appuser", "SELECT 1"),
        activity(t0 + 120, 45.0, CPU, "reporter", "SELECT 2"),
    ];
    let emulator = emulator_with_activity(&samples, None);

    let mut input = dimension_keys_input(
        IDENTIFIER,
        "db.user",
        "db.load.avg",
        t0,
        t0 + WINDOW_SPAN,
        Some(60),
    );
    input["PartitionBy"] = json!({"Group": "db.wait_event_type"});
    let answer = call(&emulator, "DescribeDimensionKeys", input).await;
    assert_eq!(
        answer.status, 200,
        "case a partitioned request: expected 200, got {} {:?}",
        answer.status, answer.body
    );

    let partition_keys = answer
        .member("PartitionKeys")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        partition_keys.len(),
        2,
        "case a partitioned request: expected one partition per wait-event type, got {:?}",
        answer.body
    );
    for (index, partition) in partition_keys.iter().enumerate() {
        assert!(
            partition
                .get("Dimensions")
                .and_then(Value::as_object)
                .is_some_and(|map| map.contains_key("db.wait_event_type.name")),
            "case partition {index}: expected the secondary group's dimension, got {partition:?}"
        );
    }

    let keys = answer.member("Keys").as_array().cloned().unwrap_or_default();
    for (index, key) in keys.iter().enumerate() {
        let partitions = key
            .get("Partitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            partitions.len(),
            partition_keys.len(),
            "case key {index}: expected one value per partition key, got {key:?}"
        );
        let summed: f64 = partitions
            .iter()
            .map(|value| value.as_f64().unwrap_or(f64::NAN))
            .sum();
        let total = key.get("Total").and_then(Value::as_f64).unwrap_or(f64::NAN);
        assert!(
            (summed - total).abs() < 1e-9,
            "case key {index}: expected the breakdown to add up to the total {total}, got \
             {summed} from {partitions:?}"
        );
    }
}

/// RULE: the load the keys total is the load `GetResourceMetrics` reports for
/// `db.load.avg` over the same window — one measurement of the same recorded activity,
/// read two ways — and `db.sampledload.avg` reports the same figure, which the `Metric`
/// member itself allows when no sampling threshold is reached.
#[tokio::test]
async fn the_keys_total_the_load_get_resource_metrics_reports() {
    let t0 = past_start(WINDOW_SPAN);
    let samples = [
        activity(t0 + 10, 60.0, LOCK_WAIT, "appuser", "UPDATE ledger SET amount = 1"),
        activity(t0 + 90, 30.0, CPU, "appuser", "SELECT 1"),
        activity(t0 + 120, 45.0, CPU, "reporter", "SELECT 2"),
    ];
    let emulator = emulator_with_activity(&samples, None);

    let keys = call(
        &emulator,
        "DescribeDimensionKeys",
        dimension_keys_input(
            IDENTIFIER,
            "db.wait_event",
            "db.load.avg",
            t0,
            t0 + WINDOW_SPAN,
            Some(WINDOW_SPAN),
        ),
    )
    .await;
    let keyed: f64 = totals(&keys.body).iter().sum();

    for metric in ["db.load.avg", "db.sampledload.avg"] {
        let series = call(
            &emulator,
            "GetResourceMetrics",
            json!({
                "ServiceType": "RDS",
                "Identifier": IDENTIFIER,
                "MetricQueries": [{"Metric": metric}],
                "StartTime": t0,
                "EndTime": t0 + WINDOW_SPAN,
                "PeriodInSeconds": WINDOW_SPAN,
            }),
        )
        .await;
        assert_eq!(
            series.status, 200,
            "case {metric}: expected 200, got {} {:?}",
            series.status, series.body
        );
        let points: Vec<f64> = series
            .member("MetricList")
            .as_array()
            .and_then(|list| list.first().cloned())
            .and_then(|entry| entry.get("DataPoints").cloned())
            .and_then(|points| points.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|point| point.get("Value").and_then(Value::as_f64).unwrap_or(f64::NAN))
            .collect();
        assert_eq!(
            points.len(),
            1,
            "case {metric}: expected one point for a one-period window, got {:?}",
            series.body
        );
        assert!(
            (points[0] - keyed).abs() < 1e-9,
            "case {metric}: expected the same load the keys total ({keyed}), got {}",
            points[0]
        );
        assert!(
            points[0] > 0.0,
            "case {metric}: expected the recorded activity to be reported, got {}",
            points[0]
        );
    }

    // An engine COUNTER is not the load series: nothing here instruments one, so its
    // points stay at zero over the same window.
    let counter = call(
        &emulator,
        "GetResourceMetrics",
        json!({
            "ServiceType": "RDS",
            "Identifier": IDENTIFIER,
            "MetricQueries": [{"Metric": "db.SQL.Innodb_rows_read.avg"}],
            "StartTime": t0,
            "EndTime": t0 + WINDOW_SPAN,
            "PeriodInSeconds": WINDOW_SPAN,
        }),
    )
    .await;
    let counter_point = counter
        .member("MetricList")
        .as_array()
        .and_then(|list| list.first().cloned())
        .and_then(|entry| entry.get("DataPoints").cloned())
        .and_then(|points| points.as_array().cloned())
        .unwrap_or_default()
        .first()
        .and_then(|point| point.get("Value").and_then(Value::as_f64))
        .unwrap_or(f64::NAN);
    assert_eq!(
        counter_point, 0.0,
        "case an engine counter: expected no reading for a counter nothing measures, got \
         {counter_point}"
    );
}

/// RULE: Performance Insights is what collects the activity, so a data source its owning
/// service records the feature as turned OFF for reports no keys and no load, whatever
/// its engine did.
#[tokio::test]
async fn activity_is_not_reported_for_a_data_source_the_feature_is_off_for() {
    let t0 = past_start(WINDOW_SPAN);
    let samples = [activity(
        t0 + 10,
        60.0,
        LOCK_WAIT,
        "appuser",
        "UPDATE ledger SET amount = 1",
    )];

    // Each row is the monitoring configuration the owning service persisted, and whether
    // the activity is reported under it.
    let rows: &[(&str, Option<MonitoringConfiguration>, bool)] = &[
        ("nothing on record", None, true),
        (
            "performance insights on",
            Some(MonitoringConfiguration {
                performance_insights_enabled: true,
                database_insights_mode: Some("advanced".to_string()),
            }),
            true,
        ),
        (
            "performance insights off",
            Some(MonitoringConfiguration {
                performance_insights_enabled: false,
                database_insights_mode: Some("standard".to_string()),
            }),
            false,
        ),
    ];

    for (case, monitoring, reported) in rows {
        let emulator = emulator_with_activity(&samples, monitoring.clone());
        let answer = call(
            &emulator,
            "DescribeDimensionKeys",
            dimension_keys_input(
                IDENTIFIER,
                "db.wait_event",
                "db.load.avg",
                t0,
                t0 + WINDOW_SPAN,
                Some(60),
            ),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.member("Keys").as_array().map(Vec::len) == Some(0),
            !*reported,
            "case {case}: expected the activity reported = {reported}, got {:?}",
            answer.body
        );
    }
}
