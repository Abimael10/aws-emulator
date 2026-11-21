// SPDX-License-Identifier: CC0-1.0

//! The RESOLUTION every instant this service reports is carried at on the wire.
//!
//! RULE: every `Timestamp` member of this service is a plain `smithy.api#Timestamp` with no
//! `@timestampFormat` override (`smithy_query shape=Blueprint,JobRun`, whose instants target
//! `TimestampValue`), so its `awsJson1_1` protocol carries it in the body as a JSON number of
//! seconds since the epoch whose fractional part is the sub-second remainder; the finest
//! sub-second remainder this API states an instant to is the MILLISECOND. So, whatever
//! sub-second offset the host clock happened to be read at when a record was written, every
//! instant a response carries is a plain decimal count of epoch seconds with at most three
//! fractional digits — the whole millisecond the instant falls in — and that holds on every
//! read path, whether the instant sits on the response itself, inside a nested resource, or
//! on each element of a list.
//!
//! The fractional digits are the subject, so they are read from the response bytes as the
//! server wrote them rather than from a parsed float, whose own rounding would hide them.
//!
//! The rows differ along what the rule generalises over: which operation answers, which
//! member carries the instant, where in the body it sits, how many instants one body
//! carries, and — round by round, and in the table of the shared projection below — the
//! sub-second offset the instant was taken at.

use std::time::{Duration, UNIX_EPOCH};

use aws_smithy_types::date_time::Format;
use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::to_date_time;
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{create_and_start, emulator_with_engine, job_body, run_at_rest, start_run};
use crate::wire::{call_text, given_usage_profile, ok};

/// The most fractional digits an instant of this API is carried with: the millisecond is the
/// finest remainder it states one to, and three digits is what a millisecond takes.
const MAX_FRACTIONAL_DIGITS: usize = 3;

/// One millisecond, and one second, in nanoseconds.
const NANOS_PER_MILLISECOND: u32 = 1_000_000;
const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// The epoch-second window an instant stamped by this test run falls in: generous on both
/// sides, so this is a check that the number is a count of epoch SECONDS of this era rather
/// than of milliseconds (a thousand times larger) and not a check on how fast the host runs.
const EARLIEST_PLAUSIBLE_SECS: f64 = 1_600_000_000.0;
const LATEST_PLAUSIBLE_SECS: f64 = 4_102_444_800.0;

/// One whole epoch second the projection table below is anchored at — any second of this era
/// does, because the rule is about the remainder, not about which second it is added to.
const ANCHOR_SECONDS: u64 = 1_700_000_000;

/// The text of every number the body carries under `member`, exactly as the response
/// serialised it, wherever in the body it appears: a member of the response itself, a member
/// of a nested resource, or one entry per element of a list.
fn wire_numbers(raw: &str, member: &str) -> Vec<String> {
    let needle = format!("\"{member}\":");
    raw.match_indices(needle.as_str())
        .map(|(at, _)| {
            raw[at + needle.len()..]
                .trim_start()
                .chars()
                .take_while(|character| {
                    character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | 'e' | 'E')
                })
                .collect::<String>()
        })
        .collect()
}

/// The digits after the decimal point of a serialised number.
fn fractional_digits(text: &str) -> usize {
    text.split_once('.')
        .map_or(0, |(_, fraction)| fraction.len())
}

/// Make one call and assert that it reports every one of `members`, at least once each, as a
/// count of epoch seconds carried at millisecond resolution.
async fn assert_millisecond_resolution(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    members: &[&str],
) {
    let (status, raw) = call_text(router, operation, request).await;
    assert_eq!(
        status, 200,
        "case {case}: {operation} must answer 200, got {status} with body {raw}"
    );
    for member in members {
        let reported = wire_numbers(&raw, member);
        assert!(
            !reported.is_empty(),
            "case {case}: {operation} must report {member}, got {raw}"
        );
        for text in reported {
            assert!(
                !text.contains('e') && !text.contains('E'),
                "case {case}: {operation} must carry {member} as a plain decimal count of \
                 epoch seconds, got {text:?}"
            );
            assert!(
                fractional_digits(&text) <= MAX_FRACTIONAL_DIGITS,
                "case {case}: {operation} must carry {member} at millisecond resolution, so \
                 with at most {MAX_FRACTIONAL_DIGITS} fractional digits, got {text:?}"
            );
            let seconds: f64 = text.parse().unwrap_or_else(|_| {
                panic!("case {case}: {operation} must carry {member} as a number, got {text:?}")
            });
            assert!(
                (EARLIEST_PLAUSIBLE_SECS..=LATEST_PLAUSIBLE_SECS).contains(&seconds),
                "case {case}: {operation} must carry {member} as a count of epoch SECONDS, \
                 got {seconds}"
            );
        }
    }
}

#[tokio::test]
async fn every_read_path_reports_its_instants_at_millisecond_resolution() {
    let (router, _engine) = emulator_with_engine();

    // One row per round, each round writing its own records back to back so they land at
    // different sub-second offsets of the host clock: whatever offset a record was written
    // at, every read of it carries the instant at millisecond resolution.
    for round in 0..8 {
        let case = format!("round {round}");
        let blueprint = format!("bp-{round}");
        let job = format!("job-{round}");
        let database = format!("db_{round}");
        let table = format!("tbl_{round}");
        let workflow = format!("wf-{round}");
        let profile = format!("profile-{round}");

        ok(
            &router,
            "CreateBlueprint",
            &json!({
                "Name": &blueprint,
                "BlueprintLocation": "s3://blueprint-bucket/etl.zip",
            }),
        )
        .await;
        ok(&router, "CreateJob", &job_body(&job, json!({}))).await;
        ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": &database } }),
        )
        .await;
        ok(
            &router,
            "CreateTable",
            &json!({ "DatabaseName": &database, "TableInput": { "Name": &table } }),
        )
        .await;
        ok(&router, "CreateWorkflow", &json!({ "Name": &workflow })).await;
        given_usage_profile(&router, &profile, None).await;

        // Each row is one read path: the member names differ, and so does where the instant
        // sits — on the response itself, inside a nested resource, or on every element of a
        // list.
        let rows: [(&str, Value, &[&str]); 8] = [
            (
                "GetBlueprint",
                json!({ "Name": &blueprint }),
                &["CreatedOn", "LastModifiedOn"],
            ),
            (
                "GetJob",
                json!({ "JobName": &job }),
                &["CreatedOn", "LastModifiedOn"],
            ),
            (
                "GetJobs",
                json!({}),
                &["CreatedOn", "LastModifiedOn"],
            ),
            ("GetDatabase", json!({ "Name": &database }), &["CreateTime"]),
            ("GetDatabases", json!({}), &["CreateTime"]),
            (
                "GetTable",
                json!({ "DatabaseName": &database, "Name": &table }),
                &["CreateTime", "UpdateTime"],
            ),
            (
                "GetWorkflow",
                json!({ "Name": &workflow }),
                &["CreatedOn", "LastModifiedOn"],
            ),
            (
                "GetUsageProfile",
                json!({ "Name": &profile }),
                &["CreatedOn", "LastModifiedOn"],
            ),
        ];
        for (operation, request, members) in rows {
            assert_millisecond_resolution(&router, &case, operation, &request, members).await;
        }
    }
}

#[tokio::test]
async fn the_job_run_read_paths_report_their_instants_at_millisecond_resolution() {
    let (router, engine) = emulator_with_engine();
    let job = "run-instants";

    // Two runs of one job, so the listing carries one set of instants per element and the
    // single read carries one set. Each run is brought to rest before the next is started
    // (the job definition admits one concurrent run), and a run at rest is the one that
    // carries the instant it completed at beside the ones it started and was last modified
    // at.
    let first = create_and_start(&router, job, json!({})).await;
    run_at_rest(&router, &engine, "the first run", job, &first).await;
    let second = start_run(&router, &json!({ "JobName": job })).await;
    run_at_rest(&router, &engine, "the second run", job, &second).await;
    let run_ids = [first, second];

    let members = &["StartedOn", "LastModifiedOn", "CompletedOn"];
    for (index, run_id) in run_ids.iter().enumerate() {
        assert_millisecond_resolution(
            &router,
            &format!("run {index}"),
            "GetJobRun",
            &json!({ "JobName": job, "RunId": run_id }),
            members,
        )
        .await;
    }
    assert_millisecond_resolution(
        &router,
        "the whole history",
        "GetJobRuns",
        &json!({ "JobName": job }),
        members,
    )
    .await;

    // The history carries one instant per run, so the list form is covered element for
    // element rather than only on its first entry.
    let (_, raw) = call_text(&router, "GetJobRuns", &json!({ "JobName": job })).await;
    assert_eq!(
        wire_numbers(&raw, "StartedOn").len(),
        run_ids.len(),
        "the history must report one StartedOn per run, got {raw}"
    );
}

#[test]
fn the_shared_projection_reduces_an_instant_to_whole_milliseconds() {
    // Each row is a sub-second offset a clock read can land at: the whole second, one
    // nanosecond past it, one microsecond, the last nanosecond before the first whole
    // millisecond, one whole millisecond, a nanosecond-resolution offset of the kind a clock
    // read actually returns, the half second (so reducing is told apart from rounding up),
    // and the last nanosecond of the second.
    let offsets: [(&str, u32); 8] = [
        ("the whole second", 0),
        ("one nanosecond", 1),
        ("one microsecond", 1_000),
        (
            "the last nanosecond before the first millisecond",
            NANOS_PER_MILLISECOND - 1,
        ),
        ("one millisecond", NANOS_PER_MILLISECOND),
        ("a nanosecond-resolution offset", 384_209_157),
        ("the half second", NANOS_PER_SECOND / 2),
        ("the last nanosecond of the second", NANOS_PER_SECOND - 1),
    ];

    for (case, nanos) in offsets {
        let at = UNIX_EPOCH + Duration::new(ANCHOR_SECONDS, nanos);
        let reported = to_date_time(at);
        assert_eq!(
            reported.subsec_nanos() % NANOS_PER_MILLISECOND,
            0,
            "case {case}: a reported instant carries whole milliseconds, got {} ns",
            reported.subsec_nanos()
        );
        assert_eq!(
            reported.subsec_nanos(),
            nanos / NANOS_PER_MILLISECOND * NANOS_PER_MILLISECOND,
            "case {case}: the remainder is the whole millisecond the instant falls in, so it \
             never runs ahead of the clock, got {} ns for {nanos} ns",
            reported.subsec_nanos()
        );
        assert_eq!(
            u64::try_from(reported.secs()).expect("an instant after the epoch"),
            ANCHOR_SECONDS,
            "case {case}: reducing the remainder must not move the second, got {}",
            reported.secs()
        );
        let text = reported
            .fmt(Format::EpochSeconds)
            .unwrap_or_else(|error| panic!("case {case}: epoch seconds must format: {error}"));
        assert!(
            fractional_digits(&text) <= MAX_FRACTIONAL_DIGITS,
            "case {case}: at most {MAX_FRACTIONAL_DIGITS} fractional digits reach the wire, \
             got {text:?}"
        );
    }

    // An instant before the epoch, which no stored instant is, reports the epoch itself
    // rather than a negative count — at the same resolution.
    let before_epoch = UNIX_EPOCH - Duration::from_nanos(1);
    let fallback = to_date_time(before_epoch);
    assert_eq!(
        (fallback.secs(), fallback.subsec_nanos()),
        (0, 0),
        "case before the epoch: the fallback instant is the epoch itself, got {fallback:?}"
    );
}

/// The instant a record reports does not change between reads: reducing an instant to the
/// millisecond it falls in is done on the STORED instant, so every read of one record
/// carries the same number.
#[tokio::test]
async fn a_reported_instant_is_the_same_on_every_read() {
    let (router, _engine) = emulator_with_engine();
    let name = "stable-bp";
    ok(
        &router,
        "CreateBlueprint",
        &json!({ "Name": name, "BlueprintLocation": "s3://blueprint-bucket/etl.zip" }),
    )
    .await;

    let (_, first_raw) = call_text(&router, "GetBlueprint", &json!({ "Name": name })).await;
    let first = wire_numbers(&first_raw, "CreatedOn");
    assert_eq!(
        first.len(),
        1,
        "a read reports exactly one CreatedOn, got {first:?}"
    );
    for read in 0..4 {
        let (_, raw) = call_text(&router, "GetBlueprint", &json!({ "Name": name })).await;
        let again = wire_numbers(&raw, "CreatedOn");
        assert_eq!(
            again, first,
            "case read {read}: a reported instant must not change between reads, got {again:?} \
             after {first:?}"
        );
    }
}
